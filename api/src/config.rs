//! Daemon configuration — the knobs that bound what a valid credential can do.
//!
//! Every security control here **fails closed**. An unset allowlist denies every
//! spawn rather than allowing every spawn, because the alternative turns a
//! forgotten config line into an open shell on the box.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use jod_core::PermissionPolicy;
use serde::{Deserialize, Serialize};

/// Where the daemon listens by default: loopback only.
///
/// Not a placeholder — the API is reached over a Tailscale tailnet, and binding
/// a public interface would put an endpoint that spawns shells on the internet.
/// → `docs/jod-api.md`
pub const DEFAULT_BIND: &str = "127.0.0.1:8787";

pub const DEFAULT_MAX_AGENTS: usize = 8;
pub const DEFAULT_MAX_BODY_BYTES: usize = 256 * 1024;
pub const DEFAULT_SESSION_TTL_HOURS: u64 = 24 * 7;

/// How often the daemon rescans the store for runs another process started.
///
/// This is the delay a person sees between starting a run in `jod tui` and it
/// appearing in the web HUD, so it is set by patience rather than by load: two
/// seconds reads as "immediately" and costs one indexed query, because a run
/// already known is skipped before its events are read.
pub const DEFAULT_DISCOVER_SECS: u64 = 2;

/// Below this, the scan is a busy loop against SQLite rather than a poll.
const MIN_DISCOVER_SECS: u64 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub bind: String,
    /// The most permissive policy a *remote* caller may request.
    ///
    /// `Bypass` auto-approves every tool call, which over an API is a remote
    /// shell. Raising this is a deliberate local act, never something a request
    /// can do to itself.
    pub max_permission: PermissionPolicy,
    pub max_concurrent_agents: usize,
    /// Roots an agent may be spawned under. Empty denies every spawn.
    pub allowed_cwd: Vec<PathBuf>,
    pub max_body_bytes: usize,
    /// How long a browser cookie session lives. Shorter than a typical web
    /// app's, because this credential spawns processes on a server.
    pub session_ttl_hours: u64,
    /// Seconds between rescans for runs started by another process.
    ///
    /// `0` turns discovery off, which leaves the daemon seeing only the runs it
    /// launched itself and those that existed at boot.
    pub discover_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind: DEFAULT_BIND.to_string(),
            max_permission: PermissionPolicy::AcceptEdits,
            max_concurrent_agents: DEFAULT_MAX_AGENTS,
            allowed_cwd: Vec::new(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            session_ttl_hours: DEFAULT_SESSION_TTL_HOURS,
            discover_secs: DEFAULT_DISCOVER_SECS,
        }
    }
}

impl Config {
    /// Read `~/.jod/api.toml` if it exists, then let the environment win.
    ///
    /// A missing file is not an error: the defaults are safe, and a systemd unit
    /// that sets everything through `Environment=` should not need a file.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let mut config = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => return Err(e.into()),
        };
        config.apply_env();
        Ok(config)
    }

    /// Environment overrides, so a unit file can configure the daemon without
    /// shipping a config file next to it.
    pub fn apply_env(&mut self) {
        if let Ok(v) = std::env::var("JOD_API_BIND") {
            self.bind = v;
        }
        if let Ok(v) = std::env::var("JOD_API_MAX_PERMISSION") {
            if let Some(p) = parse_permission(&v) {
                self.max_permission = p;
            }
        }
        if let Ok(v) = std::env::var("JOD_API_MAX_AGENTS") {
            if let Ok(n) = v.parse() {
                self.max_concurrent_agents = n;
            }
        }
        if let Ok(v) = std::env::var("JOD_API_MAX_BODY") {
            if let Ok(n) = v.parse() {
                self.max_body_bytes = n;
            }
        }
        if let Ok(v) = std::env::var("JOD_API_SESSION_TTL_HOURS") {
            if let Ok(n) = v.parse() {
                self.session_ttl_hours = n;
            }
        }
        if let Ok(v) = std::env::var("JOD_API_DISCOVER_SECS") {
            if let Ok(n) = v.parse() {
                self.discover_secs = n;
            }
        }
        if let Ok(v) = std::env::var("JOD_API_ALLOWED_CWD") {
            self.allowed_cwd = v
                .split(':')
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect();
        }
    }

    pub fn socket_addr(&self) -> anyhow::Result<SocketAddr> {
        Ok(self.bind.parse()?)
    }

    pub fn session_ttl_ms(&self) -> i64 {
        (self.session_ttl_hours as i64).saturating_mul(60 * 60 * 1000)
    }

    /// How often to rescan for runs another process started, or `None` to not.
    ///
    /// Returning an `Option` rather than a `Duration` is what keeps `0` from
    /// meaning "scan as fast as the CPU allows" — the one value where the
    /// obvious reading of the number and the safe behaviour disagree.
    pub fn discover_interval(&self) -> Option<std::time::Duration> {
        match self.discover_secs {
            0 => None,
            n => Some(std::time::Duration::from_secs(n.max(MIN_DISCOVER_SECS))),
        }
    }

    /// Is `requested` within the configured ceiling?
    ///
    /// Ordered `Ask < AcceptEdits < Bypass` by how much a caller can do without
    /// being asked.
    pub fn permits(&self, requested: PermissionPolicy) -> bool {
        rank(requested) <= rank(self.max_permission)
    }

    /// Resolve a requested working directory against the allowlist.
    ///
    /// Canonicalises **before** comparing, so `/allowed/../../etc` is rejected
    /// rather than passing a string prefix test. Requires the directory to
    /// exist, which is not a hardship — an agent cannot run in one that doesn't.
    pub fn resolve_cwd(&self, requested: &Path) -> Result<PathBuf, CwdRejection> {
        if self.allowed_cwd.is_empty() {
            return Err(CwdRejection::NoAllowlist);
        }
        let canonical = requested
            .canonicalize()
            .map_err(|_| CwdRejection::Unreadable(requested.to_path_buf()))?;
        if !canonical.is_dir() {
            return Err(CwdRejection::NotADirectory(canonical));
        }
        for root in &self.allowed_cwd {
            // A root that cannot be canonicalised is a misconfiguration, not a
            // reason to admit the request: skip it and keep failing closed.
            let Ok(root) = root.canonicalize() else {
                continue;
            };
            if canonical.starts_with(&root) {
                return Ok(canonical);
            }
        }
        Err(CwdRejection::OutsideAllowlist(canonical))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CwdRejection {
    #[error("no working directory is allowed: set allowed_cwd (or JOD_API_ALLOWED_CWD)")]
    NoAllowlist,
    #[error("working directory does not exist or is unreadable: {0}")]
    Unreadable(PathBuf),
    #[error("not a directory: {0}")]
    NotADirectory(PathBuf),
    #[error("working directory is outside the allowlist: {0}")]
    OutsideAllowlist(PathBuf),
}

/// How much can happen without anyone being asked, as a number to compare.
///
/// Taken from [`PermissionPolicy::ALL`]'s own order rather than restated. This
/// used to be a hand-written copy of `jod-core`'s ranking, kept in step by
/// nothing but attention — and a ceiling that disagrees with the thing it is
/// meant to cap is the one bug in this file that would not look like a bug.
fn rank(p: PermissionPolicy) -> usize {
    PermissionPolicy::ALL
        .iter()
        .position(|m| *m == p)
        .expect("PermissionPolicy::ALL is missing a variant")
}

pub fn parse_permission(s: &str) -> Option<PermissionPolicy> {
    jod_core::mcp::parse_permission(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_bind_is_loopback_only() {
        // If this ever becomes 0.0.0.0, an endpoint that spawns shells is on the
        // public internet. That is the whole threat model.
        let addr = Config::default().socket_addr().unwrap();
        assert!(
            addr.ip().is_loopback(),
            "default bind {addr} is not loopback"
        );
    }

    #[test]
    fn bypass_is_refused_under_the_default_ceiling() {
        let c = Config::default();
        assert!(c.permits(PermissionPolicy::Ask));
        assert!(c.permits(PermissionPolicy::AcceptEdits));
        assert!(!c.permits(PermissionPolicy::Bypass));
    }

    #[test]
    fn an_ask_only_ceiling_refuses_edits() {
        let c = Config {
            max_permission: PermissionPolicy::Ask,
            ..Default::default()
        };
        assert!(c.permits(PermissionPolicy::Ask));
        assert!(!c.permits(PermissionPolicy::AcceptEdits));
        assert!(!c.permits(PermissionPolicy::Bypass));
    }

    #[test]
    fn an_empty_allowlist_denies_every_spawn() {
        let c = Config::default();
        assert!(c.allowed_cwd.is_empty());
        assert!(matches!(
            c.resolve_cwd(Path::new("/tmp")),
            Err(CwdRejection::NoAllowlist)
        ));
    }

    #[test]
    fn a_directory_under_an_allowed_root_is_accepted() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        let nested = root.join("jod-api-cwd-test");
        std::fs::create_dir_all(&nested).unwrap();
        let c = Config {
            allowed_cwd: vec![root],
            ..Default::default()
        };
        assert_eq!(
            c.resolve_cwd(&nested).unwrap(),
            nested.canonicalize().unwrap()
        );
    }

    #[test]
    fn traversal_out_of_an_allowed_root_is_rejected() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        let c = Config {
            allowed_cwd: vec![root.clone()],
            ..Default::default()
        };
        // Canonicalisation must collapse this to /etc before the prefix test.
        let escape = root.join("../../etc");
        match c.resolve_cwd(&escape) {
            Err(CwdRejection::OutsideAllowlist(p)) => {
                assert!(!p.starts_with(&root), "{p:?} should have escaped {root:?}")
            }
            // On a host without /etc the path simply doesn't resolve — also a refusal.
            Err(CwdRejection::Unreadable(_)) => {}
            other => panic!("traversal was not rejected: {other:?}"),
        }
    }

    #[test]
    fn a_nonexistent_directory_is_rejected_rather_than_assumed() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        let c = Config {
            allowed_cwd: vec![root.clone()],
            ..Default::default()
        };
        assert!(matches!(
            c.resolve_cwd(&root.join("definitely-not-here-9f2c")),
            Err(CwdRejection::Unreadable(_))
        ));
    }

    #[test]
    fn permission_parsing_accepts_the_documented_spellings_only() {
        assert_eq!(parse_permission("ask"), Some(PermissionPolicy::Ask));
        assert_eq!(
            parse_permission("ACCEPT-EDITS"),
            Some(PermissionPolicy::AcceptEdits)
        );
        assert_eq!(parse_permission("bypass"), Some(PermissionPolicy::Bypass));
        assert_eq!(parse_permission("yolo"), None);
    }

    /// Discovery is what makes a run started in `jod tui` appear in the web
    /// HUD. Defaulting it off would mean the daemon only ever shows its own
    /// work, which is the bug this knob exists to fix.
    #[test]
    fn discovery_is_on_by_default() {
        assert_eq!(
            Config::default().discover_interval(),
            Some(std::time::Duration::from_secs(DEFAULT_DISCOVER_SECS))
        );
    }

    #[test]
    fn a_zero_interval_turns_discovery_off_rather_than_spinning() {
        // The one value where the obvious reading of the number — "no delay" —
        // and the safe behaviour disagree.
        let c = Config {
            discover_secs: 0,
            ..Default::default()
        };
        assert_eq!(c.discover_interval(), None);
    }

    #[test]
    fn a_sub_second_interval_is_raised_to_the_floor() {
        let c = Config {
            discover_secs: 1,
            ..Default::default()
        };
        assert!(c.discover_interval().unwrap() >= std::time::Duration::from_secs(1));
    }

    #[test]
    fn a_missing_config_file_yields_safe_defaults() {
        let c = Config::load(Path::new("/definitely/not/a/config.toml")).unwrap();
        assert!(c.allowed_cwd.is_empty());
        assert!(!c.permits(PermissionPolicy::Bypass));
    }
}
