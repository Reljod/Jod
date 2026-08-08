//! Bridge to running Jod sessions via the `orchestrate` skill's `orc` CLI.
//!
//! This is what makes the app a *coding-harness* input rather than a notepad:
//! dictate, then drop the text straight into a Claude session that is already
//! running with its context intact. `orc send` continues a session rather than
//! starting one, so the transcript lands mid-conversation.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Session {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub cwd: String,
}

/// How to reach `orc`. The installed `jod` shim can be older than the checkout —
/// a shim without the `orc` subcommand exits non-zero rather than being absent —
/// so we keep the bundled script as a fallback and try each in turn.
enum Entry {
    Shim(String),
    Script(PathBuf),
}

impl Entry {
    fn command(&self) -> Command {
        match self {
            Entry::Shim(path) => {
                let mut c = Command::new(path);
                c.arg("orc");
                c
            }
            Entry::Script(path) => {
                let mut c = Command::new("node");
                c.arg(path);
                c
            }
        }
    }

    fn describe(&self) -> String {
        match self {
            Entry::Shim(p) => format!("{p} orc"),
            Entry::Script(p) => format!("node {}", p.display()),
        }
    }
}

const SCRIPT_REL: &str = ".agents/skills/orchestrate/scripts/orc.mjs";

/// Candidate entry points, most specific first: an explicit `JOD_REPO`, then the
/// shim, then the conventional checkout locations.
fn entries(jod_repo: Option<&str>) -> Vec<Entry> {
    let mut out = Vec::new();

    if let Some(repo) = jod_repo.filter(|r| !r.is_empty()) {
        out.extend(script_in(PathBuf::from(repo)));
    }
    if let Ok(repo) = std::env::var("JOD_REPO") {
        out.extend(script_in(PathBuf::from(repo)));
    }
    if let Ok(shim) = which_jod() {
        out.push(Entry::Shim(shim));
    }
    if let Some(home) = dirs::home_dir() {
        out.extend(script_in(home.join("Developer/Repositories/Projects/Jod")));
        out.extend(script_in(home.join("Jod")));
    }
    out
}

/// The bundled `orc.mjs` under `base`, if it is actually there.
fn script_in(base: PathBuf) -> Option<Entry> {
    let p = base.join(SCRIPT_REL);
    p.is_file().then_some(Entry::Script(p))
}

fn which_jod() -> Result<String, ()> {
    let out = Command::new("sh").arg("-lc").arg("command -v jod").output().map_err(|_| ())?;
    if !out.status.success() {
        return Err(());
    }
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if p.is_empty() { Err(()) } else { Ok(p) }
}

/// Runs `orc` with `args`, trying each entry point until one succeeds.
/// Reports every attempt on failure — a silent fallback that ends in "not
/// found" is far harder to debug than a list of what was tried.
fn run(args: &[&str], jod_repo: Option<&str>) -> Result<String, String> {
    let candidates = entries(jod_repo);
    if candidates.is_empty() {
        return Err("could not find `orc`: no `jod` on PATH and no Jod checkout found. \
                    Set JOD_REPO to your Jod repository."
            .into());
    }

    let mut tried = Vec::new();
    for entry in &candidates {
        match entry.command().args(args).output() {
            Ok(out) if out.status.success() => {
                return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
            }
            Ok(out) => {
                let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
                tried.push(format!("{} → {}", entry.describe(), first_line(&msg)));
            }
            Err(e) => tried.push(format!("{} → {e}", entry.describe())),
        }
    }
    Err(format!("`orc {}` failed. Tried: {}", args.join(" "), tried.join("; ")))
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("no output").to_string()
}

pub fn list(jod_repo: Option<&str>) -> Result<Vec<Session>, String> {
    let stdout = run(&["ls", "--json"], jod_repo)?;
    if stdout.is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&stdout).map_err(|e| format!("could not parse session list: {e}"))
}

pub fn send(id: &str, message: &str, jod_repo: Option<&str>) -> Result<String, String> {
    if message.trim().is_empty() {
        return Err("refusing to send an empty message".into());
    }
    run(&["send", id, message], jod_repo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_orc_ls_shape() {
        let json = r#"[{"id":"bba4312f","name":"Orchestrator","state":"done","cwd":"/tmp"}]"#;
        let s: Vec<Session> = serde_json::from_str(json).unwrap();
        assert_eq!(s[0].id, "bba4312f");
        assert_eq!(s[0].state, "done");
    }

    #[test]
    fn tolerates_missing_optional_fields() {
        let s: Vec<Session> = serde_json::from_str(r#"[{"id":"abc"}]"#).unwrap();
        assert_eq!(s[0].id, "abc");
        assert!(s[0].name.is_empty());
    }

    #[test]
    fn refuses_to_send_blank_messages() {
        assert!(send("abc", "   ", None).is_err());
    }

    #[test]
    fn explicit_repo_is_preferred_over_other_entries() {
        // The repo this test builds in always has the script, so an explicit
        // path must land first in the candidate list.
        let repo = concat!(env!("CARGO_MANIFEST_DIR"), "/../../..");
        let found = entries(Some(repo));
        assert!(
            matches!(found.first(), Some(Entry::Script(_))),
            "explicit JOD_REPO should be tried first"
        );
    }

    #[test]
    fn failure_message_lists_what_was_tried() {
        let err = run(&["definitely-not-a-subcommand"], None).unwrap_err();
        assert!(err.contains("Tried:"), "expected attempt list, got: {err}");
    }
}
