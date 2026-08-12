//! Credentials an agent can use and cannot read.
//!
//! The model this converged on is the one GitHub Actions, Doppler, Infisical
//! and `op run` all landed on independently: **inject at exec, mask on output,
//! reference by name.** The agent is told a variable exists; the value reaches
//! its tools through the process environment and never through its context.
//!
//! Three rules, and every one of them is load-bearing:
//!
//! 1. **The value lives outside every repository**, in a file at owner-only
//!    permissions, verified on read. Not in the database — a value in SQLite
//!    is a value in every backup, every `jod conv show`, and every screen
//!    share.
//! 2. **The value is injected by the supervisor at spawn**, which is the only
//!    process that sees both the child's environment and its output. It is not
//!    in the prompt, the transcript, or `spawn.json`.
//! 3. **The value is scrubbed back out of the output** before anything is
//!    parsed or stored. Redaction is the belt to injection's braces: an agent
//!    that echoes the variable still cannot get the value into the record.
//!
//! ## What this is not
//!
//! Not a keychain, and not a permission system. A missing key blocks one test,
//! not a session — which is the point. The agent is told to treat an absent
//! credential as a *blocked* ending rather than a reason to invent one.

use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{JodError, Result};
use crate::store::Store;

/// Who a secret is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Every session on the box.
    Global,
    /// One work. The default, so a key given for one project is not handed to
    /// every agent Reljod runs.
    Work,
    /// One conversation.
    Conversation,
}

impl Default for Scope {
    fn default() -> Self {
        Scope::Work
    }
}

impl Scope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Work => "work",
            Scope::Conversation => "conversation",
        }
    }

    pub fn parse(s: &str) -> Scope {
        match s {
            "global" => Scope::Global,
            "conversation" => Scope::Conversation,
            _ => Scope::Work,
        }
    }
}

/// Everything about a secret that is safe to show anyone.
///
/// Note what is absent, permanently: the value. This type is what the rail,
/// the CLI, the MCP tools and the agent's preamble are allowed to see. If a
/// field is ever added here that could reconstruct a value, the design has
/// been broken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretMeta {
    pub id: i64,
    /// The environment variable's name. Validated to be a legal one, because
    /// a name that cannot be exported is a secret that silently never arrives.
    pub name: String,
    pub scope: Scope,
    /// The work or conversation id; empty for global.
    pub scope_id: String,
    /// What it is for, in the owner's words. Shown to the agent so it knows
    /// which variable to reach for.
    pub hint: String,
    /// Length only — never content. The scrubber needs it to decide what is
    /// too short to redact safely, and asking for the length must not require
    /// reading the value.
    pub length: usize,
    /// Whether this value is long enough to redact without mangling ordinary
    /// output. A four-character secret would match half of everything, so it
    /// is injected and *not* redacted — and the rail says so when it is
    /// stored, because a silent exception here is a leak nobody was told about.
    pub redactable: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Below this, a value is injected but not redacted.
///
/// Short strings appear in ordinary output constantly; redacting them would
/// replace legitimate text with the marker and make transcripts unreadable,
/// which is its own kind of failure. The threshold is named here so the rail
/// and the scrubber cannot disagree about it.
pub const MIN_REDACTABLE_LEN: usize = 8;

/// Whether `name` is a legal environment variable name.
///
/// Deliberately strict — leading letter or underscore, then letters, digits
/// and underscores. A name outside this set may be silently dropped by a shell
/// or a harness somewhere down the line, and the failure would look like "the
/// secret did not work" rather than "the name was invalid".
pub fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// ---- where a value lives ----------------------------------------------

/// The file holding one secret's value.
///
/// The name is kept readable so a person can see what is on the box without a
/// tool, and the hash — over the scope *and* the scope id as well as the name —
/// is what keeps two secrets called `OPENAI_API_KEY` in two different works from
/// being the same file. Hashing the scope id rather than spelling it out also
/// keeps a conversation id, which may be anything, out of a filename.
fn value_path(scope: Scope, scope_id: &str, name: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    // NUL separators, so `("work", "ab")` and `("worka", "b")` cannot hash
    // alike and hand one work's credential to another.
    hasher.update(scope.as_str().as_bytes());
    hasher.update([0u8]);
    hasher.update(scope_id.as_bytes());
    hasher.update([0u8]);
    hasher.update(name.as_bytes());
    let digest = hasher.finalize();
    let mut tag = String::with_capacity(32);
    for byte in &digest[..16] {
        use std::fmt::Write as _;
        let _ = write!(tag, "{byte:02x}");
    }
    // `name` has been through `is_valid_name`, so it cannot contain a slash, a
    // dot-dot, or anything else that would escape this directory.
    crate::paths::secrets_dir().join(format!("{name}.{tag}"))
}

/// Create the secrets directory at `0700`, and repair it if it is wider.
///
/// Reset on every write rather than only at creation: a directory that another
/// process, an unpacked backup or a careless `chmod -R` has widened is the case
/// this exists to catch, and that cannot be caught by only looking once.
fn ensure_secrets_dir() -> Result<PathBuf> {
    let dir = crate::paths::secrets_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

/// Read one secret's value. **The spawn path is the only caller.**
///
/// Not a rule the compiler can hold, so it is stated here and kept true by
/// review: the supervisor calls this to fill the child's environment, and
/// nothing else in Jod calls it at all. A second caller is a second place a
/// credential can be printed from.
///
/// The mode is checked on the *open file handle* and the value is read from
/// that same handle, so there is no window in which the file that was checked
/// and the file that was read could be different ones. Any group or world bit
/// is a refusal: a `0644` secret has already been readable by every process on
/// the box, and reading it anyway would let Jod keep promising a privacy it no
/// longer has.
///
/// The directory is checked too, for a different attack. File permissions stop
/// somebody *reading* the value; they do nothing about somebody replacing it,
/// which only needs write permission on the directory holding it. A swapped
/// file is a credential of an attacker's choosing injected into an agent that
/// trusts it, so a group- or world-writable secrets directory is refused as
/// firmly as a readable file.
///
/// The file is the value byte for byte — no trailing newline is added or
/// stripped. A value written by hand must therefore use `printf`, not `echo`,
/// or the newline becomes part of the credential.
pub fn read_secret_value(meta: &SecretMeta) -> Result<String> {
    let path = value_path(meta.scope, &meta.scope_id, &meta.name);
    let dir = crate::paths::secrets_dir();
    let dir_mode = std::fs::metadata(&dir)?.permissions().mode() & 0o777;
    if dir_mode & 0o022 != 0 {
        return Err(JodError::Invalid(format!(
            "refusing to read secret `{}`: {} is mode {dir_mode:04o}, so anyone on this \
             machine can replace the file and choose what the agent is handed. Run \
             `chmod 700` on it.",
            meta.name,
            dir.display()
        )));
    }
    let mut file = std::fs::File::open(&path).map_err(|e| {
        JodError::Invalid(format!(
            "secret `{}` is recorded but its value file is unreadable ({}): {e}",
            meta.name,
            path.display()
        ))
    })?;
    let mode = file.metadata()?.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(JodError::Invalid(format!(
            "refusing to read secret `{}`: {} is mode {mode:04o}, which lets someone \
             other than its owner read it. Run `chmod 600` on it, and treat the value \
             as exposed.",
            meta.name,
            path.display()
        )));
    }
    let mut value = String::new();
    file.read_to_string(&mut value)?;
    Ok(value)
}

// ---- the store ---------------------------------------------------------

const COLUMNS: &str =
    "id, name, scope, scope_id, hint, length, redactable, created_at_ms, updated_at_ms";

fn meta_from_row(r: &rusqlite::Row) -> rusqlite::Result<SecretMeta> {
    Ok(SecretMeta {
        id: r.get(0)?,
        name: r.get(1)?,
        scope: Scope::parse(&r.get::<_, String>(2)?),
        scope_id: r.get(3)?,
        hint: r.get(4)?,
        length: r.get::<_, i64>(5)?.max(0) as usize,
        redactable: r.get::<_, i64>(6)? != 0,
        created_at_ms: r.get(7)?,
        updated_at_ms: r.get(8)?,
    })
}

impl Store {
    /// Store a value on disk and its description in the database.
    ///
    /// The two halves are deliberately unequal: everything the rail, the CLI,
    /// the MCP tools and the agent ever see is the row, and the row cannot
    /// reconstruct the value. Nothing here logs, formats or returns `value` —
    /// it is written once, to one file, and then dropped.
    pub fn put_secret(
        &self,
        name: &str,
        scope: Scope,
        scope_id: &str,
        value: &str,
        hint: &str,
    ) -> Result<SecretMeta> {
        if !is_valid_name(name) {
            return Err(JodError::Invalid(format!(
                "`{name}` is not a legal environment variable name: a letter or underscore, \
                 then letters, digits and underscores. A name a shell would drop makes a \
                 present secret behave like an absent one."
            )));
        }
        if value.is_empty() {
            return Err(JodError::Invalid(format!(
                "secret `{name}` has no value; an empty variable is exported successfully \
                 and then fails wherever it is used, which is the hardest kind of missing \
                 credential to diagnose"
            )));
        }
        if value.contains('\0') {
            // `execve` takes NUL-terminated strings, so the child would receive
            // a silently truncated credential and report an authentication
            // failure that looks like a wrong key rather than a mangled one.
            return Err(JodError::Invalid(format!(
                "secret `{name}` contains a NUL byte, which cannot survive being passed \
                 to a process"
            )));
        }
        // Global is the one scope with nothing to be scoped to. Anything else
        // without an id would quietly become a second global bucket — a key
        // meant for one work handed to every session on the box, which is the
        // exact failure the default scope exists to prevent.
        let scope_id = if scope == Scope::Global { "" } else { scope_id };
        if scope != Scope::Global && scope_id.is_empty() {
            return Err(JodError::Invalid(format!(
                "a {} secret needs the id of the {} it belongs to",
                scope.as_str(),
                scope.as_str()
            )));
        }

        // The value goes down before the row, so the failure mode is an
        // orphaned `0600` file rather than a recorded secret with no value —
        // one is a file nothing references, the other is a run that resolves
        // the name, finds nothing, and reports a missing credential for a
        // secret the owner watched themselves store.
        ensure_secrets_dir()?;
        let path = value_path(scope, scope_id, name);
        write_value(&path, value)?;

        let at = now_ms();
        let length = value.len();
        let redactable = length >= MIN_REDACTABLE_LEN;
        self.write(|tx| {
            tx.execute(
                "INSERT INTO secrets
                   (name, scope, scope_id, hint, length, redactable, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
                 ON CONFLICT(scope, scope_id, name) DO UPDATE SET
                   hint = ?4, length = ?5, redactable = ?6, updated_at_ms = ?7",
                params![
                    name,
                    scope.as_str(),
                    scope_id,
                    hint,
                    length as i64,
                    redactable as i64,
                    at
                ],
            )?;
            Ok(())
        })?;

        self.secret(name, scope, scope_id)?.ok_or_else(|| {
            JodError::Invalid(format!(
                "secret `{name}` was written but could not be read back"
            ))
        })
    }

    /// One secret's description, by its exact scope.
    pub fn secret(&self, name: &str, scope: Scope, scope_id: &str) -> Result<Option<SecretMeta>> {
        let scope_id = if scope == Scope::Global { "" } else { scope_id };
        let conn = self.conn.lock().expect("store lock poisoned");
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {COLUMNS} FROM secrets WHERE scope = ?1 AND scope_id = ?2 AND name = ?3"
                ),
                params![scope.as_str(), scope_id, name],
                meta_from_row,
            )
            .optional()?)
    }

    /// Every secret stored at exactly this scope. Names and hints — never
    /// values, which is why this is the query the CLI and the rail use.
    pub fn secret_names(&self, scope: Scope, scope_id: &str) -> Result<Vec<SecretMeta>> {
        let scope_id = if scope == Scope::Global { "" } else { scope_id };
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM secrets
             WHERE scope = ?1 AND scope_id = ?2
             ORDER BY name"
        ))?;
        let rows = stmt.query_map(params![scope.as_str(), scope_id], meta_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// What a run in this conversation, in this work, should be given.
    ///
    /// Precedence is conversation > work > global, resolved by *name*: a
    /// conversation that defines `OPENAI_API_KEY` gets its own, not two
    /// variables of the same name racing to be exported last. Narrower wins
    /// because the narrower one was set later and more deliberately.
    pub fn secrets_for(
        &self,
        conversation_id: Option<&str>,
        work_id: Option<&str>,
    ) -> Result<Vec<SecretMeta>> {
        // Widest first, so each pass overwrites the one before it.
        let mut resolved: std::collections::BTreeMap<String, SecretMeta> =
            std::collections::BTreeMap::new();
        for meta in self.secret_names(Scope::Global, "")? {
            resolved.insert(meta.name.clone(), meta);
        }
        if let Some(work) = work_id.filter(|w| !w.is_empty()) {
            for meta in self.secret_names(Scope::Work, work)? {
                resolved.insert(meta.name.clone(), meta);
            }
        }
        if let Some(conversation) = conversation_id.filter(|c| !c.is_empty()) {
            for meta in self.secret_names(Scope::Conversation, conversation)? {
                resolved.insert(meta.name.clone(), meta);
            }
        }
        Ok(resolved.into_values().collect())
    }

    /// Resolve a bare name, the way the supervisor has to.
    ///
    /// `spawn.json` carries names alone, and by then the choice of *which*
    /// `OPENAI_API_KEY` has already been made by [`Store::secrets_for`] at
    /// launch. This repeats that ordering — conversation, then work, then
    /// global, most recently updated first within a tier — so the supervisor
    /// resolves to the same row rather than an arbitrary one. It cannot be
    /// exact: a name alone genuinely does not say whose it is.
    pub fn secret_by_name(&self, name: &str) -> Result<Option<SecretMeta>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {COLUMNS} FROM secrets WHERE name = ?1
                     ORDER BY CASE scope
                                WHEN 'conversation' THEN 0
                                WHEN 'work' THEN 1
                                ELSE 2
                              END,
                              updated_at_ms DESC
                     LIMIT 1"
                ),
                params![name],
                meta_from_row,
            )
            .optional()?)
    }

    /// Forget a secret: the value first, then the record of it.
    ///
    /// That order on purpose. If removing the file fails, the row stays and the
    /// caller is told; the reverse would leave a live credential on disk that
    /// nothing in Jod any longer knows about, and so nothing would ever clean
    /// up. Returns whether there was anything to remove.
    pub fn remove_secret(&self, name: &str, scope: Scope, scope_id: &str) -> Result<bool> {
        let scope_id = if scope == Scope::Global { "" } else { scope_id };
        let path = value_path(scope, scope_id, name);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            // Already gone is the outcome asked for, not a failure.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let removed = self.write(|tx| {
            Ok(tx.execute(
                "DELETE FROM secrets WHERE scope = ?1 AND scope_id = ?2 AND name = ?3",
                params![scope.as_str(), scope_id, name],
            )?)
        })?;
        Ok(removed > 0)
    }
}

/// Write a value at `0600`, atomically.
///
/// Through a temporary file in the same directory and a rename, so a crash or a
/// full disk mid-write leaves the previous value intact rather than half of the
/// new one — a truncated credential fails authentication in a way that looks
/// like a wrong key. The temporary is created `0600` by `mode()`, so the value
/// never exists at a wider mode even for an instant, and `set_permissions`
/// afterwards repairs a file whose mode was widened before this write.
fn write_value(path: &std::path::Path, value: &str) -> Result<()> {
    // Appended to the whole filename rather than swapped for its extension:
    // the extension here is the scope hash, and replacing it would give two
    // scopes' copies of the same name one shared temporary file.
    let tmp = path.with_file_name(format!(
        "{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(value.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_that_a_shell_would_reject_is_refused_here_first() {
        assert!(is_valid_name("OPENAI_API_KEY"));
        assert!(is_valid_name("_private"));
        assert!(!is_valid_name("2FA_TOKEN"), "cannot start with a digit");
        assert!(!is_valid_name("MY-KEY"), "hyphens are not exportable");
        assert!(!is_valid_name(""), "the empty name is not a name");
        assert!(!is_valid_name("HAS SPACE"));
    }

    #[test]
    fn the_redaction_floor_is_shared_so_the_rail_and_the_scrubber_agree() {
        // Not a behaviour test so much as a tripwire: if this constant moves,
        // both the "stored, but too short to redact" warning and the scrubber
        // move with it, and neither can drift alone.
        assert!(MIN_REDACTABLE_LEN >= 8);
    }

    // ---- the store, against a real `JOD_HOME` --------------------------

    /// A temporary `JOD_HOME` and a store inside it.
    ///
    /// Holds [`crate::ENV_LOCK`] for its whole life. Every path here is derived
    /// from a process-wide environment variable, so two of these running at
    /// once would write each other's secrets into each other's directories —
    /// and the test that greps the database for a leaked value would be
    /// grepping somebody else's database.
    struct Home {
        dir: std::path::PathBuf,
        store: Store,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl Home {
        fn new(tag: &str) -> Home {
            let guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let dir = std::env::temp_dir().join(format!(
                "jod-secrets-{tag}-{}-{}",
                std::process::id(),
                now_ms()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            std::env::set_var("JOD_HOME", &dir);
            let store = Store::open(&dir.join("jod.db")).unwrap();
            Home {
                dir,
                store,
                _guard: guard,
            }
        }

        fn mode_of(&self, path: &std::path::Path) -> u32 {
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777
        }

        /// Every byte Jod wrote anywhere under `JOD_HOME` except the secret
        /// files themselves — database, WAL, journal, run directories.
        fn everything_but_the_secret_files(&self) -> Vec<u8> {
            let mut bytes = Vec::new();
            let secrets = crate::paths::secrets_dir();
            let mut stack = vec![self.dir.clone()];
            while let Some(dir) = stack.pop() {
                if dir == secrets {
                    continue;
                }
                for entry in std::fs::read_dir(&dir).unwrap() {
                    let path = entry.unwrap().path();
                    if path.is_dir() {
                        stack.push(path);
                    } else {
                        bytes.extend(std::fs::read(&path).unwrap_or_default());
                    }
                }
            }
            bytes
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            std::env::remove_var("JOD_HOME");
            std::fs::remove_dir_all(&self.dir).ok();
        }
    }

    #[test]
    fn a_stored_value_is_on_disk_at_owner_only_permissions_and_nowhere_in_the_database() {
        let home = Home::new("owner-only");
        let value = "value-that-must-never-be-in-a-row-8f21c4";
        let meta = home
            .store
            .put_secret(
                "OPENAI_API_KEY",
                Scope::Work,
                "work-1",
                value,
                "the API key",
            )
            .unwrap();

        assert_eq!(meta.length, value.len());
        assert!(meta.redactable);
        assert_eq!(meta.hint, "the API key");

        let path = value_path(Scope::Work, "work-1", "OPENAI_API_KEY");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), value);
        assert_eq!(home.mode_of(&path), 0o600, "the value file must be 0600");
        assert_eq!(
            home.mode_of(&crate::paths::secrets_dir()),
            0o700,
            "the directory must be 0700"
        );

        // The promise, checked rather than asserted: the bytes are not in the
        // database, its write-ahead log, or anything else Jod left on disk.
        let elsewhere = home.everything_but_the_secret_files();
        assert!(
            !elsewhere
                .windows(value.len())
                .any(|w| w == value.as_bytes()),
            "the value reached a file that is not the secret file"
        );
        // ...and the name, which is not secret, is there — so the search above
        // is looking in the right place rather than at nothing.
        assert!(
            elsewhere
                .windows("OPENAI_API_KEY".len())
                .any(|w| w == b"OPENAI_API_KEY"),
            "the metadata row was never written, so the check above proves nothing"
        );
    }

    #[test]
    fn a_world_readable_value_file_is_refused_on_read() {
        let home = Home::new("world-readable");
        let meta = home
            .store
            .put_secret("TOKEN", Scope::Global, "", "a-long-enough-value", "")
            .unwrap();
        assert_eq!(read_secret_value(&meta).unwrap(), "a-long-enough-value");

        // Someone — a backup restore, a `chmod -R`, a helpful script — widens
        // it. The value is already exposed at that point; continuing to hand it
        // out would be Jod promising a privacy that is gone.
        let path = value_path(Scope::Global, "", "TOKEN");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = read_secret_value(&meta).expect_err("a 0644 secret must be refused");
        let message = err.to_string();
        assert!(
            message.contains("0644"),
            "the mode must be named: {message}"
        );
        assert!(
            !message.contains("a-long-enough-value"),
            "the refusal leaked the value it was refusing to hand over: {message}"
        );

        // Group-readable alone is refused too: a shared group is other people.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(read_secret_value(&meta).is_err());
    }

    #[test]
    fn a_writable_secrets_directory_is_refused_even_when_the_file_itself_is_0600() {
        let home = Home::new("writable-dir");
        let meta = home
            .store
            .put_secret("SWAPPABLE", Scope::Global, "", "long-enough-value", "")
            .unwrap();

        // The attack this stops is substitution, not reading: with write
        // permission on the directory anyone can unlink the file and leave one
        // of their own at the same path, still `0600`, still owned by them —
        // and the agent would be handed a credential somebody else chose.
        let dir = crate::paths::secrets_dir();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        let message = read_secret_value(&meta)
            .expect_err("a world-writable secrets directory must be refused")
            .to_string();
        assert!(message.contains("0777"), "{message}");
        assert!(!message.contains("long-enough-value"), "{message}");

        // Storing anything again puts the directory back, because a mode is
        // repaired on every write rather than only at creation.
        home.store
            .put_secret("SWAPPABLE", Scope::Global, "", "long-enough-value", "")
            .unwrap();
        assert_eq!(home.mode_of(&dir), 0o700);
        assert_eq!(read_secret_value(&meta).unwrap(), "long-enough-value");
    }

    #[test]
    fn a_recorded_secret_whose_file_has_vanished_says_so_plainly() {
        let home = Home::new("vanished");
        let meta = home
            .store
            .put_secret("GONE_KEY", Scope::Global, "", "long-enough-value", "")
            .unwrap();
        std::fs::remove_file(value_path(Scope::Global, "", "GONE_KEY")).unwrap();

        // The supervisor treats this as a name it could not resolve and carries
        // on; what matters here is that the reason is legible rather than an
        // empty string standing in for a credential.
        let message = read_secret_value(&meta).unwrap_err().to_string();
        assert!(message.contains("GONE_KEY"), "{message}");
        assert!(message.contains("value file"), "{message}");
    }

    #[test]
    fn a_short_value_is_stored_but_marked_unredactable() {
        let home = Home::new("short");
        let meta = home
            .store
            .put_secret("PIN", Scope::Global, "", "1234", "")
            .unwrap();
        assert!(
            !meta.redactable,
            "a four-character value would match half of ordinary output"
        );
        assert_eq!(meta.length, 4);
        // It is still injectable — the exception is about scrubbing, not about
        // refusing to store it.
        assert_eq!(read_secret_value(&meta).unwrap(), "1234");
    }

    #[test]
    fn a_name_no_shell_could_export_is_refused_before_anything_is_written() {
        let home = Home::new("bad-name");
        let err = home
            .store
            .put_secret("MY-KEY", Scope::Global, "", "long-enough-value", "")
            .unwrap_err();
        assert!(matches!(err, JodError::Invalid(_)), "{err}");
        assert!(
            !crate::paths::secrets_dir().exists(),
            "a refused secret must leave nothing behind"
        );
        assert!(home
            .store
            .secret_names(Scope::Global, "")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn an_empty_value_and_a_scope_without_an_id_are_both_refused() {
        let home = Home::new("refusals");
        assert!(matches!(
            home.store.put_secret("EMPTY", Scope::Global, "", "", ""),
            Err(JodError::Invalid(_))
        ));
        // A work secret with no work would be a global secret wearing a
        // disguise, which is the accident the work default exists to prevent.
        assert!(matches!(
            home.store
                .put_secret("K", Scope::Work, "", "long-enough-value", ""),
            Err(JodError::Invalid(_))
        ));
        assert!(matches!(
            home.store
                .put_secret("K", Scope::Global, "", "has\0nul-in-it", ""),
            Err(JodError::Invalid(_))
        ));
    }

    #[test]
    fn storing_the_same_name_twice_replaces_the_value_rather_than_stacking_rows() {
        let home = Home::new("upsert");
        let first = home
            .store
            .put_secret("ROTATED", Scope::Work, "w", "first-value-here", "old")
            .unwrap();
        let second = home
            .store
            .put_secret("ROTATED", Scope::Work, "w", "second-value", "new")
            .unwrap();

        assert_eq!(first.id, second.id, "rotation must not mint a second row");
        assert_eq!(second.hint, "new");
        assert_eq!(second.length, "second-value".len());
        assert_eq!(read_secret_value(&second).unwrap(), "second-value");
        assert_eq!(home.store.secret_names(Scope::Work, "w").unwrap().len(), 1);
    }

    #[test]
    fn two_works_with_the_same_name_keep_separate_values() {
        let home = Home::new("two-works");
        home.store
            .put_secret("API_KEY", Scope::Work, "alpha", "alpha-value-here", "")
            .unwrap();
        home.store
            .put_secret("API_KEY", Scope::Work, "beta", "beta-value-here", "")
            .unwrap();

        let alpha = home
            .store
            .secret("API_KEY", Scope::Work, "alpha")
            .unwrap()
            .unwrap();
        let beta = home
            .store
            .secret("API_KEY", Scope::Work, "beta")
            .unwrap()
            .unwrap();
        assert_eq!(read_secret_value(&alpha).unwrap(), "alpha-value-here");
        assert_eq!(
            read_secret_value(&beta).unwrap(),
            "beta-value-here",
            "one work's key was handed to another"
        );
    }

    #[test]
    fn the_narrower_scope_wins_when_a_name_is_defined_twice() {
        let home = Home::new("precedence");
        home.store
            .put_secret("API_KEY", Scope::Global, "", "global-value-x", "")
            .unwrap();
        home.store
            .put_secret("API_KEY", Scope::Work, "w", "work-value-xxx", "")
            .unwrap();
        home.store
            .put_secret("ONLY_GLOBAL", Scope::Global, "", "global-only-value", "")
            .unwrap();

        let for_work = home.store.secrets_for(None, Some("w")).unwrap();
        let names: Vec<&str> = for_work.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["API_KEY", "ONLY_GLOBAL"]);
        let api = &for_work[0];
        assert_eq!(api.scope, Scope::Work);
        assert_eq!(read_secret_value(api).unwrap(), "work-value-xxx");

        home.store
            .put_secret("API_KEY", Scope::Conversation, "c", "conv-value-xxx", "")
            .unwrap();
        let for_conversation = home.store.secrets_for(Some("c"), Some("w")).unwrap();
        assert_eq!(
            for_conversation.len(),
            2,
            "one variable per name, not three"
        );
        assert_eq!(for_conversation[0].scope, Scope::Conversation);
        assert_eq!(
            read_secret_value(&for_conversation[0]).unwrap(),
            "conv-value-xxx"
        );

        // A run in no work and no conversation still gets the global one, and
        // only the global one.
        let bare = home.store.secrets_for(None, None).unwrap();
        assert_eq!(bare.len(), 2);
        assert!(bare.iter().all(|m| m.scope == Scope::Global));

        // And the supervisor, holding only a name, resolves the same way.
        let resolved = home.store.secret_by_name("API_KEY").unwrap().unwrap();
        assert_eq!(resolved.scope, Scope::Conversation);
        assert!(home.store.secret_by_name("NOT_A_SECRET").unwrap().is_none());
    }

    #[test]
    fn removing_a_secret_takes_the_value_with_the_record() {
        let home = Home::new("remove");
        home.store
            .put_secret("DOOMED", Scope::Work, "w", "value-to-delete", "")
            .unwrap();
        let path = value_path(Scope::Work, "w", "DOOMED");
        assert!(path.exists());

        assert!(home
            .store
            .remove_secret("DOOMED", Scope::Work, "w")
            .unwrap());
        assert!(
            !path.exists(),
            "the row went but the credential stayed on disk, where nothing now tracks it"
        );
        assert!(home
            .store
            .secret_names(Scope::Work, "w")
            .unwrap()
            .is_empty());
        assert!(
            !home
                .store
                .remove_secret("DOOMED", Scope::Work, "w")
                .unwrap(),
            "removing what is not there is not an error, but it is not a removal"
        );
    }
}
