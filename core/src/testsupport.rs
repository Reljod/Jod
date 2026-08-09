//! Test-only scaffolding shared across the crate's unit tests.
//!
//! Two things are hard to test in this crate: code that shells out to `tmux`,
//! and code that reads process-wide environment variables. Both are covered
//! here rather than in each module, so a test reads as the behaviour it is
//! asserting instead of as setup.
//!
//! The fake tmux is a real executable script that keeps its session list in a
//! file, so it *behaves* like tmux (creating, listing and killing sessions)
//! rather than merely recording calls. That is what makes an end-to-end
//! `spawn_agent` test possible without a tmux server — and without any risk of
//! touching the developer's own sessions.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::event::{AgentEvent, Usage};
use crate::harness::{ArgPart, Harness, HarnessKind, SpawnRequest};

/// Unique-enough suffix for temp paths. Deliberately not random: the crate's
/// tests must stay reproducible, so this counts within the process instead.
static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A directory under the system temp dir, removed when the value is dropped.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("jod-test-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Write `body` to `path` and make it executable.
pub fn write_executable(path: &Path, body: &str) {
    std::fs::write(path, body).expect("write script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("make script executable");
    }
}

/// Sets environment variables for the duration of a test and restores them
/// afterwards, holding [`crate::ENV_LOCK`] so two such tests cannot interleave.
///
/// Rust runs tests as threads of one process, so an unguarded `set_var` in one
/// test is visible to every other — this is the only safe way to test
/// env-driven discovery.
pub struct EnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    saved: Vec<(String, Option<String>)>,
}

impl EnvGuard {
    pub fn new() -> Self {
        Self {
            _lock: crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner()),
            saved: Vec::new(),
        }
    }

    pub fn set(&mut self, key: &str, value: impl AsRef<std::ffi::OsStr>) -> &mut Self {
        self.saved.push((key.to_string(), std::env::var(key).ok()));
        std::env::set_var(key, value);
        self
    }

    pub fn remove(&mut self, key: &str) -> &mut Self {
        self.saved.push((key.to_string(), std::env::var(key).ok()));
        std::env::remove_var(key);
        self
    }

    /// Point `PATH` and `HOME` at nothing, so binary discovery can only succeed
    /// via an explicit `JOD_*_BIN` override. Without this a developer machine
    /// with a real `claude` or `tmux` installed would take a different code
    /// path than CI.
    pub fn isolate_discovery(&mut self) -> &mut Self {
        self.set("PATH", "/definitely/not/a/dir");
        self.set("HOME", "/definitely/not/a/home");
        self
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // Restore in reverse, so a key set twice ends on its original value.
        for (key, value) in self.saved.iter().rev() {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

/// A stand-in `tmux` that keeps real session state in a file.
pub struct FakeTmux {
    dir: TempDir,
}

impl FakeTmux {
    /// A fake that behaves like a working tmux.
    pub fn new() -> Self {
        Self::with_kill_failure(None)
    }

    /// As [`FakeTmux::new`], but `kill-session` fails with `message` on stderr.
    /// Used to prove the caller distinguishes "already gone" from a real fault.
    pub fn with_kill_failure(message: Option<&str>) -> Self {
        let dir = TempDir::new("tmux");
        let log = dir.join("calls.log");
        let sessions = dir.join("sessions");
        std::fs::write(&sessions, b"").expect("seed session list");

        let kill_branch = match message {
            Some(msg) => format!("\n    printf '%s\\n' {msg:?} >&2; exit 1"),
            None => r#"
    if grep -qxF "$name" "$SESSIONS"; then
      grep -vxF "$name" "$SESSIONS" > "$SESSIONS.tmp" || true
      mv "$SESSIONS.tmp" "$SESSIONS"
    else
      echo "can't find session $name" >&2
      exit 1
    fi"#
            .to_string(),
        };

        // An absolute shebang and an explicit PATH, because these fakes run
        // under `EnvGuard::isolate_discovery`, which points PATH at nothing —
        // `/usr/bin/env bash` would not even resolve, let alone `grep`.
        let body = format!(
            r#"#!/bin/bash
PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH
LOG={log:?}
SESSIONS={sessions:?}
printf '%s\n' "$*" >> "$LOG"

cmd="${{1:-}}"
shift || true

case "$cmd" in
  has-session)
    grep -qxF "$2" "$SESSIONS"
    ;;
  new-session)
    name=""
    while [ $# -gt 0 ]; do
      case "$1" in
        -s) name="$2"; shift 2 ;;
        *) shift ;;
      esac
    done
    printf '%s\n' "$name" >> "$SESSIONS"
    ;;
  kill-session)
    name="$2"{kill_branch}
    ;;
  list-sessions)
    cat "$SESSIONS"
    ;;
  set-option)
    :
    ;;
  *)
    echo "fake tmux: unknown command $cmd" >&2
    exit 1
    ;;
esac
"#,
            log = log.to_string_lossy(),
            sessions = sessions.to_string_lossy(),
            kill_branch = kill_branch,
        );

        let bin = dir.join("tmux");
        write_executable(&bin, &body);
        Self { dir }
    }

    /// A tmux that is present but fails every command — the "tmux server is
    /// wedged" case, distinct from tmux being absent.
    pub fn broken() -> Self {
        let dir = TempDir::new("tmux-broken");
        let bin = dir.join("tmux");
        write_executable(
            &bin,
            "#!/bin/bash\nprintf '%s\\n' 'fake tmux: refusing' >&2\nexit 1\n",
        );
        Self { dir }
    }

    pub fn bin(&self) -> PathBuf {
        self.dir.join("tmux")
    }

    /// Pretend these sessions already exist.
    pub fn seed_sessions(&self, names: &[&str]) {
        let mut body = String::new();
        for n in names {
            body.push_str(n);
            body.push('\n');
        }
        std::fs::write(self.dir.join("sessions"), body).expect("seed sessions");
    }

    pub fn live_sessions(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("sessions"))
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_string)
            .collect()
    }

    /// Every argv the fake was invoked with, one per line.
    pub fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

/// A harness that turns each non-blank line into a `Message`, so runner tests
/// can assert on plumbing without depending on Claude's or OpenCode's wire
/// format.
#[derive(Default)]
pub struct EchoHarness;

impl Harness for EchoHarness {
    fn kind(&self) -> HarnessKind {
        HarnessKind::ClaudeCode
    }

    fn args(&self, _req: &SpawnRequest) -> Vec<ArgPart> {
        vec![ArgPart::lit("--echo"), ArgPart::Prompt]
    }

    fn parse_line(&mut self, line: &str) -> Vec<AgentEvent> {
        if line.trim().is_empty() {
            return vec![];
        }
        vec![AgentEvent::Message { text: line.to_string() }]
    }

    fn finalize(&mut self, exit_code: Option<i32>) -> AgentEvent {
        AgentEvent::Finished {
            text: None,
            exit_code,
            is_error: exit_code.is_some_and(|c| c != 0),
            usage: Usage::default(),
        }
    }
}
