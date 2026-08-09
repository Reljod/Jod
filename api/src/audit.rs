//! An append-only record of every mutating request.
//!
//! When something goes wrong the question is always "what ran, and who asked
//! for it". This answers it in a file that stays greppable when the daemon is
//! not running — the same principle as the rest of Jod's state.
//!
//! The log names the token's **label**, never the token. A security log that
//! contains credentials is a credential store with worse permissions.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub at_ms: i64,
    pub action: String,
    /// Which credential acted — the label, never the secret.
    pub token_label: String,
    /// Tailnet identity when `tailscale serve` supplied one. Audit only: it is
    /// a client-settable header and is never an authorisation input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tailnet_user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

pub struct AuditLog {
    path: PathBuf,
}

impl AuditLog {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn default_path() -> PathBuf {
        jod_core::paths::jod_home().join("audit.jsonl")
    }

    /// Append one line. A failure to audit must never fail the request it is
    /// auditing, so this reports rather than propagates.
    pub fn append(&self, entry: &AuditEntry) {
        if let Err(e) = self.try_append(entry) {
            eprintln!("jod-api: could not write audit entry: {e}");
        }
    }

    fn try_append(&self, entry: &AuditEntry) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut line = serde_json::to_string(entry)?;
        line.push('\n');
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(line.as_bytes())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Build an entry with the current wall clock.
pub fn entry(action: &str, token_label: &str, outcome: &str) -> AuditEntry {
    AuditEntry {
        at_ms: chrono::Utc::now().timestamp_millis(),
        action: action.to_string(),
        token_label: token_label.to_string(),
        tailnet_user: None,
        agent_id: None,
        outcome: outcome.to_string(),
        detail: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_log(name: &str) -> AuditLog {
        let path = std::env::temp_dir().join(format!("jod-api-audit-{name}.jsonl"));
        let _ = std::fs::remove_file(&path);
        AuditLog::new(path)
    }

    #[test]
    fn entries_append_one_json_line_each() {
        let log = temp_log("append");
        log.append(&entry("spawn", "phone", "ok"));
        log.append(&entry("kill", "phone", "ok"));
        let text = std::fs::read_to_string(log.path()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in lines {
            serde_json::from_str::<AuditEntry>(line).expect("each line is standalone JSON");
        }
        let _ = std::fs::remove_file(log.path());
    }

    #[test]
    fn an_entry_records_the_label_and_never_a_token() {
        let log = temp_log("nosecret");
        let mut e = entry("spawn", "phone", "ok");
        e.agent_id = Some("agent-1".into());
        log.append(&e);
        let text = std::fs::read_to_string(log.path()).unwrap();
        assert!(text.contains("phone"));
        assert!(text.contains("agent-1"));
        assert!(
            !text.contains("jod_"),
            "an audit line looks like it holds a token"
        );
        let _ = std::fs::remove_file(log.path());
    }

    #[test]
    fn an_unwritable_path_does_not_panic_the_request() {
        // Auditing is best-effort: it must never take down the API.
        let log = AuditLog::new(PathBuf::from("/proc/definitely/not/writable.jsonl"));
        log.append(&entry("spawn", "phone", "ok"));
    }
}
