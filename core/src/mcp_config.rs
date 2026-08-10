//! Pointing a harness at Jod's own MCP server.
//!
//! [`crate::mcp`] answers the protocol and [`crate::harness::ToolAccess`] says
//! how much of Jod an agent may reach. This is the piece that connects them to
//! an actual command line, and without it both are decoration: `tools` was set,
//! capped, and tested for two hours while reaching no harness at all.
//!
//! That failure is worth naming because it is the one this branch keeps
//! producing — a component that is complete, tested, and wired to nothing. A
//! green suite over a disconnected module reads exactly like a working system.
//!
//! ## Why the config is a file on disk
//!
//! Both harnesses take MCP servers as *files*: Claude Code has
//! `--mcp-config <path>`, OpenCode reads its own config. Neither accepts a
//! server definition inline on the command line, so Jod writes one.
//!
//! One file per access level, not one per run. A run's config is a function of
//! its level and nothing else, so a per-run temp file would be N copies of three
//! documents plus a cleanup problem — and a file left behind by a killed run
//! would be a stale grant sitting on disk.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::harness::ToolAccess;

/// The MCP config for one access level, written under `~/.jod/mcp/`.
///
/// Returns the path to hand a harness. Rewritten every call rather than cached:
/// it is a few hundred bytes, and the alternative is a stale file pointing at a
/// `jod` binary that has since moved — which fails as "the agent has no tools"
/// long after anyone would connect it to an upgrade.
pub fn config_for(access: ToolAccess, jod_home: &Path) -> Result<PathBuf> {
    let dir = jod_home.join("mcp");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", access.as_str()));

    // The running executable, not a name looked up on PATH. A daemon started
    // from a build directory must point agents at *that* binary, or they get
    // whichever `jod` a shell would have found — possibly an older install,
    // possibly none.
    let exe = std::env::current_exe()?;

    let doc = serde_json::json!({
        "mcpServers": {
            "jod": {
                "command": exe.to_string_lossy(),
                "args": ["mcp", "--access", access.as_str()],
                // The child must open the same database this process is using.
                // Inheriting the environment would work today and break the
                // moment a daemon runs with a JOD_HOME its children do not.
                "env": { "JOD_HOME": jod_home.to_string_lossy() },
            }
        }
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&doc)?)?;
    Ok(path)
}

/// The name a harness will show these tools under.
///
/// Claude Code namespaces MCP tools as `mcp__<server>__<tool>`, so the server
/// name is part of every tool name an agent sees. Fixed here so a prompt can
/// refer to them and be right.
pub const SERVER_NAME: &str = "jod";

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jod-mcp-cfg-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_config_names_the_running_binary_rather_than_trusting_the_path() {
        let home = scratch("exe");
        let path = config_for(ToolAccess::ReadOnly, &home).unwrap();
        let doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();

        let command = doc["mcpServers"]["jod"]["command"].as_str().unwrap();
        assert_eq!(
            command,
            std::env::current_exe().unwrap().to_string_lossy(),
            "a daemon run from a build directory must point at its own binary"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The child has to open the same database, or an agent's tools answer
    /// about a store nobody is looking at.
    #[test]
    fn a_config_pins_the_database_rather_than_inheriting_it() {
        let home = scratch("home");
        let path = config_for(ToolAccess::Delegate, &home).unwrap();
        let doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            doc["mcpServers"]["jod"]["env"]["JOD_HOME"].as_str().unwrap(),
            home.to_string_lossy()
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The level is carried in the args, because that is the only thing the
    /// server has to go on — there is no handshake in which an agent could
    /// claim one.
    #[test]
    fn each_level_gets_its_own_file_naming_that_level() {
        let home = scratch("levels");
        for access in [
            ToolAccess::ReadOnly,
            ToolAccess::Delegate,
            ToolAccess::Orchestrate,
        ] {
            let path = config_for(access, &home).unwrap();
            assert!(
                path.to_string_lossy().contains(access.as_str()),
                "{path:?} does not name {access:?}"
            );
            let doc: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            let args: Vec<&str> = doc["mcpServers"]["jod"]["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a.as_str().unwrap())
                .collect();
            assert_eq!(args, vec!["mcp", "--access", access.as_str()]);
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Rewritten every call, so an upgraded binary does not leave agents
    /// pointing at a path that no longer exists.
    #[test]
    fn writing_twice_leaves_one_current_file() {
        let home = scratch("rewrite");
        let first = config_for(ToolAccess::ReadOnly, &home).unwrap();
        std::fs::write(&first, b"{\"stale\": true}").unwrap();
        let second = config_for(ToolAccess::ReadOnly, &home).unwrap();
        assert_eq!(first, second);
        let doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&second).unwrap()).unwrap();
        assert!(doc["mcpServers"]["jod"].is_object(), "stale content survived");
        let _ = std::fs::remove_dir_all(&home);
    }
}
