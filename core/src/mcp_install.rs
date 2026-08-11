//! Registering Jod's MCP server with the harnesses a *person* launches.
//!
//! [`crate::mcp_config`] solves the other half of this problem: a run that
//! **Jod** spawns is handed `--mcp-config <path>` on its command line, so it
//! comes up already holding `schedule_create`, `delegate`, `remember` and the
//! rest. That config is per-access-level, regenerated every launch, and no
//! human ever sees it.
//!
//! None of which reaches the session Reljod opens by typing `claude` in a repo.
//! That process gets whatever is in the *user's* own config and nothing else,
//! so Jod's tools are absent — and the failure reads exactly like the feature
//! not existing. Asking such a session to schedule something gets an honest
//! "there is no tool for that", which is how a fully-built scheduler with four
//! MCP tools and a passing suite can look, from the chair, like nothing at all.
//!
//! So this module writes the *durable* registration: one entry in each
//! harness's own user-level config, pointing at this binary.
//!
//! ## Why it re-writes rather than checks
//!
//! The entry names [`std::env::current_exe`] for the same reason
//! [`crate::mcp_config`] does — a config naming `jod` on the `PATH` silently
//! becomes a config naming *some other* `jod` after an install moves it, and
//! that failure surfaces as "the agent has no tools" long after anyone would
//! connect it to an upgrade. Re-running an install is therefore the fix for a
//! stale path, and is safe to do on every daemon start.
//!
//! ## What it will not do
//!
//! Clobber. These are files a person edits by hand: `~/.claude.json` also holds
//! session state, and an OpenCode config holds their model choices. A file that
//! does not parse is left exactly as it is and reported, because the failure
//! mode of guessing is destroying configuration Jod does not own. Every write
//! goes through a temporary file and a rename, so an interrupted install leaves
//! either the old config or the new one and never half of either.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::error::{JodError, Result};
use crate::harness::{HarnessKind, ToolAccess};

/// The name the entry is written under, and so the prefix an agent sees on
/// every tool: Claude Code namespaces MCP tools as `mcp__<server>__<tool>`.
/// Shared with [`crate::mcp_config`] so there is one spelling of it.
pub use crate::mcp_config::SERVER_NAME;

/// What an install actually did to one harness's config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// No `jod` entry was there before.
    Added,
    /// An entry was there and said something different — a stale binary path,
    /// a different access level.
    Updated,
    /// The config already said exactly this. Nothing was written.
    AlreadyCurrent,
    /// `--dry-run`: the entry is absent or stale, and a real run would write.
    WouldWrite,
}

impl Outcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::Added => "added",
            Outcome::Updated => "updated",
            Outcome::AlreadyCurrent => "already current",
            Outcome::WouldWrite => "would write",
        }
    }

    /// Whether this outcome changed a file on disk.
    pub fn wrote(&self) -> bool {
        matches!(self, Outcome::Added | Outcome::Updated)
    }
}

/// One harness's result, for a caller that wants to print a line per harness.
#[derive(Debug, Clone)]
pub struct Registration {
    pub harness: HarnessKind,
    pub path: PathBuf,
    pub outcome: Outcome,
}

/// Where a harness keeps the config it reads on an ordinary interactive start.
///
/// Not the same file Jod hands a spawned run — that one is Jod's, under
/// `~/.jod/mcp/`. This is the harness's own, which is why each of the three is
/// a different path *and* a different shape.
pub fn config_path(harness: HarnessKind) -> Result<PathBuf> {
    let home = home_dir()?;
    Ok(match harness {
        // Claude Code keeps user-scope MCP servers in `~/.claude.json`, the
        // same file `claude mcp add --scope user` writes. Note this is not
        // `~/.claude/settings.json`: settings has no `mcpServers` key, and an
        // entry written there is read by nothing.
        HarnessKind::ClaudeCode => home.join(".claude.json"),
        // OpenCode honours XDG, and accepts either extension. Prefer whichever
        // file already exists so an install edits the config the user is
        // actually using rather than creating a second one beside it.
        HarnessKind::OpenCode => {
            let dir = match std::env::var("XDG_CONFIG_HOME") {
                Ok(x) if !x.is_empty() => PathBuf::from(x),
                _ => home.join(".config"),
            }
            .join("opencode");
            let jsonc = dir.join("opencode.jsonc");
            if jsonc.exists() {
                jsonc
            } else {
                dir.join("opencode.json")
            }
        }
        // AGY is Antigravity, and ships as the Gemini CLI's sibling: its config
        // dir is `~/.gemini/config`, with MCP servers in their own file rather
        // than in `config.json`.
        HarnessKind::Agy => home.join(".gemini").join("config").join("mcp_config.json"),
    })
}

/// The entry to write, in the shape this harness reads it.
///
/// Two shapes, not one. Claude Code and AGY both take the familiar
/// `{"mcpServers": {name: {command, args, env}}}`; OpenCode takes `{"mcp":
/// {name: {type, command: [argv...], enabled, environment}}}` — one array
/// instead of a command plus args, and different spellings of `env` and of
/// "this is a subprocess". Writing the wrong one produces a config that parses,
/// loads, and provides no tools.
fn entry(harness: HarnessKind, exe: &Path, access: ToolAccess, jod_home: &Path) -> (String, Value) {
    let exe = exe.to_string_lossy().to_string();
    let args = vec![
        "mcp".to_string(),
        "--access".to_string(),
        access.as_str().to_string(),
    ];
    // The child must open the same database this process is using. Inheriting
    // the environment would work today and break the moment a daemon runs with
    // a JOD_HOME its children do not.
    let env = json!({ "JOD_HOME": jod_home.to_string_lossy() });

    match harness {
        HarnessKind::ClaudeCode | HarnessKind::Agy => (
            "mcpServers".to_string(),
            json!({ "command": exe, "args": args, "env": env }),
        ),
        HarnessKind::OpenCode => {
            let mut argv = vec![exe];
            argv.extend(args);
            (
                "mcp".to_string(),
                json!({
                    "type": "local",
                    "command": argv,
                    "enabled": true,
                    "environment": env,
                }),
            )
        }
    }
}

/// Register Jod with one harness, leaving everything else in the file alone.
///
/// `dry_run` reports what would happen and writes nothing, so the slash command
/// can show a person the diff-shaped summary before touching their config.
pub fn install(
    harness: HarnessKind,
    access: ToolAccess,
    jod_home: &Path,
    dry_run: bool,
) -> Result<Registration> {
    let path = config_path(harness)?;
    let exe = std::env::current_exe()?;
    install_at(&path, harness, access, jod_home, &exe, dry_run)
}

/// The whole of the logic, with every ambient input passed in — which is what
/// makes it testable against a scratch directory rather than the real `$HOME`.
pub fn install_at(
    path: &Path,
    harness: HarnessKind,
    access: ToolAccess,
    jod_home: &Path,
    exe: &Path,
    dry_run: bool,
) -> Result<Registration> {
    // Refused before anything is read, because the damage is done by the write
    // and the write is always wrong. `cargo test` runs binaries out of
    // `target/debug/deps/`, each one named for a content hash and deleted by
    // the next rebuild; a config pointing at one is a config that breaks
    // silently, on a machine whose owner never asked for it. This has happened:
    // registration hung off the daemon's run loop, the daemon had tests, and a
    // green suite left three real harness configs naming `jod_core-4ff9c547`.
    // The call site moved, and this stays as the check that does not depend on
    // remembering why.
    if is_a_test_binary(exe) {
        return Err(JodError::Invalid(format!(
            "refusing to register {} as the MCP server: that is a test binary, \
             and it will not exist after the next build",
            exe.display()
        )));
    }

    let before = read_config(path)?;
    let mut after = before.clone();

    let (section, server) = entry(harness, exe, access, jod_home);
    let servers = after
        .entry(&section)
        .or_insert_with(|| Value::Object(Map::new()));
    // A `mcpServers` that is not an object is a config Jod does not understand.
    // Refuse rather than replace it: the alternative is silently discarding
    // every other server the person had registered.
    let Some(servers) = servers.as_object_mut() else {
        return Err(JodError::Invalid(format!(
            "{}: `{section}` is not an object, so Jod will not edit it — fix it \
             or add the `{SERVER_NAME}` server by hand",
            path.display()
        )));
    };

    let existing = servers.get(SERVER_NAME).cloned();
    if existing.as_ref() == Some(&server) {
        return Ok(Registration {
            harness,
            path: path.to_path_buf(),
            outcome: Outcome::AlreadyCurrent,
        });
    }
    servers.insert(SERVER_NAME.to_string(), server);

    let outcome = if dry_run {
        Outcome::WouldWrite
    } else if existing.is_some() {
        Outcome::Updated
    } else {
        Outcome::Added
    };

    if !dry_run {
        write_atomically(path, &Value::Object(after))?;
    }

    Ok(Registration {
        harness,
        path: path.to_path_buf(),
        outcome,
    })
}

/// Every harness, skipping the ones that are not installed.
///
/// Skipping is the point: writing `~/.gemini/config/mcp_config.json` on a
/// machine with no AGY creates a config directory for a program that is not
/// there, and the next person to look at it has to work out who made it. A
/// harness that is later installed picks up its entry on the next install —
/// which the daemon runs on every start.
pub fn install_all(
    access: ToolAccess,
    jod_home: &Path,
    dry_run: bool,
) -> Vec<Result<Registration>> {
    HarnessKind::ALL
        .into_iter()
        .filter(|h| h.locate().is_some())
        .map(|h| install(h, access, jod_home, dry_run))
        .collect()
}

/// Register with every installed harness on daemon start, and never fail.
///
/// This is the "upon build" half. A deploy is `cargo build` then `install -m
/// 0755 target/release/jod /usr/local/bin/jod` then a service restart, and it
/// is precisely the moment the binary path can change — so the restart that
/// follows every build is also the cheapest place to refresh the registration.
/// Nobody has to remember a step, and a machine that has just been upgraded is
/// registered before its first schedule fires.
///
/// Best-effort by construction. A harness config Jod cannot parse is a reason
/// to say so on stderr; it is not a reason for the daemon that runs every
/// schedule on this box to refuse to start. `JOD_NO_MCP_INSTALL=1` opts out
/// entirely, for a machine whose configs are managed by something else.
pub fn ensure_registered() {
    if std::env::var("JOD_NO_MCP_INSTALL").is_ok_and(|v| !v.is_empty() && v != "0") {
        return;
    }
    let home = crate::paths::jod_home();
    for result in install_all(ToolAccess::Orchestrate, &home, false) {
        match result {
            // Silent when there was nothing to do: a daemon that restarts often
            // should not narrate an unchanged config every time.
            Ok(r) if r.outcome == Outcome::AlreadyCurrent => {}
            Ok(r) => eprintln!(
                "[jod/mcp] {} {} in {}",
                r.harness.label(),
                r.outcome.as_str(),
                r.path.display()
            ),
            Err(e) => eprintln!("[jod/mcp] not registered, continuing: {e}"),
        }
    }
}

/// Read a config that may not exist, may be empty, and must not be guessed at.
///
/// An absent or empty file is an empty object — both are ordinary on a fresh
/// machine, and AGY in particular ships a zero-byte `mcp_config.json`. A file
/// with bytes that are not JSON is an error, never an overwrite.
fn read_config(path: &Path) -> Result<Map<String, Value>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(e) => return Err(e.into()),
    };
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(JodError::Invalid(format!(
            "{}: the config is not a JSON object, so Jod will not edit it",
            path.display()
        ))),
        // Comments are legal in an `opencode.jsonc` and this parser rejects
        // them. Saying so beats "expected value at line 3", and leaving the
        // file untouched beats reformatting away a person's comments.
        Err(e) => Err(JodError::Invalid(format!(
            "{}: could not parse this config ({e}), so Jod left it alone — add \
             the `{SERVER_NAME}` server by hand, or run with --dry-run to see \
             the entry to paste",
            path.display()
        ))),
    }
}

/// Write via a temporary file in the same directory, then rename.
///
/// The same directory matters: `rename` is only atomic within a filesystem, and
/// `/tmp` is frequently a different one. Existing permissions are carried over
/// so a config the user made private does not come back world-readable.
fn write_atomically(path: &Path, value: &Value) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!(
        "{}.jod-{}",
        path.extension().and_then(|e| e.to_str()).unwrap_or("json"),
        std::process::id()
    ));

    let mut body = serde_json::to_vec_pretty(value)?;
    body.push(b'\n');
    std::fs::write(&tmp, &body)?;

    #[cfg(unix)]
    if let Ok(meta) = std::fs::metadata(path) {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode();
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode));
    }

    // Rename over the original. On failure the temp file is removed rather than
    // left beside a config as debris someone else has to identify.
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

/// Whether this executable is one Cargo built to run a test.
///
/// Narrow on purpose. `target/debug/jod` is a perfectly reasonable thing for a
/// developer to register while working on Jod, and this must not block it; only
/// `target/.../deps/` — where the test harnesses live — is refused.
fn is_a_test_binary(exe: &Path) -> bool {
    let parts: Vec<_> = exe.components().map(|c| c.as_os_str()).collect();
    let target = parts.iter().position(|p| *p == "target");
    let deps = parts.iter().rposition(|p| *p == "deps");
    matches!((target, deps), (Some(t), Some(d)) if d > t)
}

fn home_dir() -> Result<PathBuf> {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() => Ok(PathBuf::from(h)),
        _ => Err(JodError::Invalid(
            "no $HOME, so Jod cannot tell where a harness keeps its config".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("jod-mcp-install-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn install_into(path: &Path, harness: HarnessKind, dry_run: bool) -> Result<Registration> {
        install_at(
            path,
            harness,
            ToolAccess::Orchestrate,
            Path::new("/home/x/.jod"),
            Path::new("/usr/local/bin/jod"),
            dry_run,
        )
    }

    fn read(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn a_missing_config_becomes_one_naming_this_binary() {
        let path = scratch("missing").join("claude.json");
        let r = install_into(&path, HarnessKind::ClaudeCode, false).unwrap();
        assert_eq!(r.outcome, Outcome::Added);

        let doc = read(&path);
        let jod = &doc["mcpServers"]["jod"];
        assert_eq!(jod["command"], "/usr/local/bin/jod");
        assert_eq!(jod["args"], json!(["mcp", "--access", "orchestrate"]));
        assert_eq!(jod["env"]["JOD_HOME"], "/home/x/.jod");
    }

    /// The failure this whole module exists to prevent is a *silent* one, and
    /// the loudest version of it would be Jod eating the user's own config.
    #[test]
    fn installing_keeps_every_other_key_and_every_other_server() {
        let path = scratch("preserve").join("claude.json");
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&json!({
                "numStartups": 42,
                "mcpServers": { "linear": { "command": "linear-mcp" } },
            }))
            .unwrap(),
        )
        .unwrap();

        install_into(&path, HarnessKind::ClaudeCode, false).unwrap();

        let doc = read(&path);
        assert_eq!(doc["numStartups"], 42, "unrelated key was dropped");
        assert_eq!(doc["mcpServers"]["linear"]["command"], "linear-mcp");
        assert_eq!(doc["mcpServers"]["jod"]["command"], "/usr/local/bin/jod");
    }

    #[test]
    fn a_second_install_changes_nothing_and_says_so() {
        let path = scratch("idempotent").join("claude.json");
        assert_eq!(
            install_into(&path, HarnessKind::ClaudeCode, false)
                .unwrap()
                .outcome,
            Outcome::Added
        );
        assert_eq!(
            install_into(&path, HarnessKind::ClaudeCode, false)
                .unwrap()
                .outcome,
            Outcome::AlreadyCurrent
        );
    }

    /// The stale-binary case from the module docs: same server name, different
    /// path, and the install must replace it rather than call it current.
    #[test]
    fn an_entry_naming_a_moved_binary_is_rewritten() {
        let path = scratch("stale").join("claude.json");
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&json!({
                "mcpServers": { "jod": { "command": "/old/build/dir/jod", "args": [] } },
            }))
            .unwrap(),
        )
        .unwrap();

        let r = install_into(&path, HarnessKind::ClaudeCode, false).unwrap();
        assert_eq!(r.outcome, Outcome::Updated);
        assert_eq!(
            read(&path)["mcpServers"]["jod"]["command"],
            "/usr/local/bin/jod"
        );
    }

    #[test]
    fn opencode_gets_an_argv_array_rather_than_a_command_and_args() {
        let path = scratch("opencode").join("opencode.json");
        install_into(&path, HarnessKind::OpenCode, false).unwrap();

        let jod = &read(&path)["mcp"]["jod"];
        assert_eq!(jod["type"], "local");
        assert_eq!(jod["enabled"], true);
        assert_eq!(
            jod["command"],
            json!(["/usr/local/bin/jod", "mcp", "--access", "orchestrate"])
        );
        assert_eq!(jod["environment"]["JOD_HOME"], "/home/x/.jod");
    }

    /// AGY ships this file as zero bytes, which is not valid JSON and must not
    /// be treated as a corrupt config.
    #[test]
    fn an_empty_file_is_an_empty_config_rather_than_a_parse_error() {
        let path = scratch("empty").join("mcp_config.json");
        std::fs::write(&path, "").unwrap();
        assert_eq!(
            install_into(&path, HarnessKind::Agy, false)
                .unwrap()
                .outcome,
            Outcome::Added
        );
        assert_eq!(
            read(&path)["mcpServers"]["jod"]["command"],
            "/usr/local/bin/jod"
        );
    }

    #[test]
    fn a_config_that_does_not_parse_is_reported_and_left_exactly_as_it_was() {
        let path = scratch("corrupt").join("opencode.jsonc");
        let original = "{\n  // a comment this parser cannot read\n  \"model\": \"x\"\n}";
        std::fs::write(&path, original).unwrap();

        let err = install_into(&path, HarnessKind::OpenCode, false).unwrap_err();
        assert!(matches!(err, JodError::Invalid(_)), "{err:?}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "a config Jod could not parse was modified anyway"
        );
    }

    #[test]
    fn a_dry_run_reports_the_write_without_making_it() {
        let path = scratch("dry").join("claude.json");
        let r = install_into(&path, HarnessKind::ClaudeCode, true).unwrap();
        assert_eq!(r.outcome, Outcome::WouldWrite);
        assert!(!path.exists(), "a dry run created the config");
    }

    /// `mcpServers: []` is a real thing people write, and replacing it wholesale
    /// would discard whatever they meant by it.
    #[test]
    fn a_server_section_that_is_not_an_object_is_refused_rather_than_replaced() {
        let path = scratch("wrong-shape").join("claude.json");
        std::fs::write(&path, r#"{"mcpServers": []}"#).unwrap();

        let err = install_into(&path, HarnessKind::ClaudeCode, false).unwrap_err();
        assert!(matches!(err, JodError::Invalid(_)), "{err:?}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"mcpServers": []}"#
        );
    }

    /// The access level is the security boundary, so it has to reach the
    /// command line the harness will actually run.
    #[test]
    fn the_access_level_is_written_into_the_servers_arguments() {
        let dir = scratch("access");
        for (access, want) in [
            (ToolAccess::ReadOnly, "read_only"),
            (ToolAccess::Delegate, "delegate"),
            (ToolAccess::Orchestrate, "orchestrate"),
        ] {
            let path = dir.join(format!("{want}.json"));
            install_at(
                &path,
                HarnessKind::ClaudeCode,
                access,
                Path::new("/home/x/.jod"),
                Path::new("/usr/local/bin/jod"),
                false,
            )
            .unwrap();
            assert_eq!(read(&path)["mcpServers"]["jod"]["args"][2], want);
        }
    }

    /// The regression that cost three real config files. Registration used to
    /// hang off `Daemon::run`, the daemon had tests, and `cargo test` wrote
    /// `~/.claude.json`, `~/.config/opencode/opencode.jsonc` and
    /// `~/.gemini/config/mcp_config.json` on the developer's own machine — each
    /// naming a `target/debug/deps/jod_core-<hash>` that the next build deleted.
    #[test]
    fn a_cargo_test_binary_is_never_registered_as_the_server() {
        let path = scratch("test-binary").join("claude.json");
        let err = install_at(
            &path,
            HarnessKind::ClaudeCode,
            ToolAccess::Orchestrate,
            Path::new("/home/x/.jod"),
            Path::new("/repo/target/debug/deps/jod_core-4ff9c5476a666f1a"),
            false,
        )
        .unwrap_err();

        assert!(matches!(err, JodError::Invalid(_)), "{err:?}");
        assert!(
            !path.exists(),
            "a test binary was written into a harness config"
        );
    }

    /// The guard must not become "no developer may register a local build" —
    /// `target/debug/jod` is a reasonable thing to point a harness at while
    /// working on Jod.
    #[test]
    fn an_ordinary_build_of_jod_is_still_registrable() {
        assert!(is_a_test_binary(Path::new(
            "/repo/target/debug/deps/jod_core-4ff9c547"
        )));
        assert!(is_a_test_binary(Path::new(
            "/repo/target/release/deps/jod-abc"
        )));
        assert!(!is_a_test_binary(Path::new("/repo/target/debug/jod")));
        assert!(!is_a_test_binary(Path::new("/usr/local/bin/jod")));
        assert!(!is_a_test_binary(Path::new("/home/deps/bin/jod")));
    }

    /// Every path must be a file inside a directory the harness reads, and the
    /// three must not collide.
    #[test]
    fn each_harness_has_its_own_config_path() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("HOME", "/home/tester");
        std::env::remove_var("XDG_CONFIG_HOME");

        let paths: Vec<PathBuf> = HarnessKind::ALL
            .into_iter()
            .map(|h| config_path(h).unwrap())
            .collect();

        assert_eq!(paths[0], PathBuf::from("/home/tester/.claude.json"));
        assert_eq!(
            paths[1],
            PathBuf::from("/home/tester/.config/opencode/opencode.json")
        );
        assert_eq!(
            paths[2],
            PathBuf::from("/home/tester/.gemini/config/mcp_config.json")
        );

        let mut unique = paths.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            paths.len(),
            "two harnesses share a config path"
        );
    }

    #[test]
    fn opencode_edits_the_jsonc_config_when_that_is_the_one_that_exists() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("xdg");
        std::env::set_var("HOME", "/home/tester");
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        std::fs::create_dir_all(dir.join("opencode")).unwrap();
        std::fs::write(dir.join("opencode").join("opencode.jsonc"), "{}").unwrap();

        let chosen = config_path(HarnessKind::OpenCode).unwrap();
        std::env::remove_var("XDG_CONFIG_HOME");

        assert_eq!(chosen, dir.join("opencode").join("opencode.jsonc"));
    }
}
