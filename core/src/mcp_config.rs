//! Pointing a harness at Jod's own MCP server.
//!
//! [`crate::mcp`] answers the protocol and [`crate::harness::ToolAccess`] says
//! how much of Jod an agent may reach. This connects them to an actual command
//! line, and without it both are decoration — a green suite over a disconnected
//! module reads exactly like a working system.
//!
//! ## Why the config is a file on disk
//!
//! Neither harness accepts a server definition inline, so Jod writes one:
//! Claude Code takes `--mcp-config <path>`, OpenCode reads its own config.
//!
//! One file per access level, not one per run. A run's config is a function of
//! its level and nothing else, so a per-run temp file would be N copies of three
//! documents plus a cleanup problem — and a file left behind by a killed run
//! would be a stale grant sitting on disk.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::harness::{PermissionPolicy, ToolAccess};

/// Where the server reads the run it belongs to.
///
/// An environment variable rather than an argument, and pinned by the config
/// rather than inherited, for the same reason `JOD_HOME` is: a value a run
/// depends on should not be whatever the process tree happened to be carrying.
/// It is *identity*, so it is set by whoever launches the run and by nothing
/// the model can reach.
pub const RUN_ID_ENV: &str = "JOD_RUN_ID";

/// The conversation that run belongs to, when it is known at launch. Advisory:
/// the run id is the identity, and the conversation is resolved from it when
/// this is absent.
pub const CONVERSATION_ID_ENV: &str = "JOD_CONVERSATION_ID";

/// The MCP config for one access level, written under `~/.jod/mcp/`.
///
/// Returns the path to hand a harness, or `None` when there is nothing to
/// offer — no Jod access *and* no browser on this machine — so a caller can
/// leave `--mcp-config` off entirely rather than pointing a harness at a
/// document declaring no servers.
///
/// Rewritten every call rather than cached: it is a few hundred bytes, and the
/// alternative is a stale file pointing at a `jod` binary that has since
/// moved — which fails as "the agent has no tools" long after anyone would
/// connect it to an upgrade.
///
/// **`access` is `Option`, and `None` is not the same as "no config".** It
/// means the run was granted none of Jod's own verbs. It may still browse,
/// because browsing touches no run, schedule or memory — conflating the two
/// would make the web reachable only by runs that can also spawn agents.
pub fn config_for(access: Option<ToolAccess>, jod_home: &Path) -> Result<Option<PathBuf>> {
    config_with(
        access,
        jod_home,
        crate::paths::browser_mcp_script()
            .map(|script| (crate::paths::browser_python(), script)),
    )
}

/// [`config_for`], with the browser passed in rather than discovered.
///
/// The discovery reads process-wide environment and the real `~/.jod`, which a
/// test cannot vary without fighting every other test in the process. `jod_home`
/// was already a parameter for exactly this reason; the browser follows it.
pub fn config_with(
    access: Option<ToolAccess>,
    jod_home: &Path,
    browser: Option<(PathBuf, PathBuf)>,
) -> Result<Option<PathBuf>> {
    if access.is_none() && browser.is_none() {
        return Ok(None);
    }

    let dir = jod_home.join("mcp");
    std::fs::create_dir_all(&dir)?;
    // A file per *offer*, and "none" is one of them. Named rather than shared
    // so that two runs at different levels never race each other's rewrite.
    let level = access.map(|a| a.as_str()).unwrap_or("none");
    let path = dir.join(format!("{level}.json"));
    // No run identity at all — a session somebody started by hand, and
    // `jod mcp install`. Use [`config_for_run`] wherever the run is known.
    //
    // No permission either, and that is the honest answer rather than a
    // shortcut: there is no run whose policy this could be. The server keeps
    // its own conservative default, which is the right thing for a session Jod
    // did not launch and cannot vouch for.
    write_config(&path, access, jod_home, browser, None, None, None)
}

/// The MCP config for one *run*, written beside that run's own files.
///
/// **Not yet called from anywhere.** Its call site is `harness/claude.rs`,
/// where `args()` still reaches for [`config_for`]. Left unwired to avoid a
/// concurrent edit to argv ordering in that file; swapping the call is the
/// whole of the wiring.
///
/// Nothing is broken while it waits: [`crate::mcp::identify`] resolves the run
/// from the process group, which is authoritative and works on every harness.
/// This adds a second, agreeing source for the case where the store has no row
/// for that group.
///
/// It exists for sender identity. Jod's messaging tools must know which member
/// is calling, and the only honest answer is the run — an agent that could name
/// its own sender could send as anyone. So the run travels the way the access
/// level and the database already do: set by the launcher, unreachable by the
/// model.
///
/// **In the run's directory, not the shared `mcp/` one**, so a killed run does
/// not leave a grant on disk. It is created and removed with everything else
/// that run wrote.
///
/// `access` is `Option` for the reason [`config_for`] gives.
///
/// `permission` is the run's *own* policy and becomes the ceiling of the server
/// this document starts. Passed rather than defaulted, because the default is
/// `accept_edits` — which is how a session in `auto` opened background work in
/// `accept_edits`. See [`server_args`].
pub fn config_for_run(
    access: Option<ToolAccess>,
    jod_home: &Path,
    run_id: &str,
    conversation_id: Option<&str>,
    permission: PermissionPolicy,
) -> Result<Option<PathBuf>> {
    config_for_run_with(
        access,
        jod_home,
        run_id,
        conversation_id,
        crate::paths::browser_mcp_script()
            .map(|script| (crate::paths::browser_python(), script)),
        permission,
    )
}

/// [`config_for_run`], with the browser passed in rather than discovered — the
/// same injection, and for the same reason, as [`config_with`].
pub fn config_for_run_with(
    access: Option<ToolAccess>,
    jod_home: &Path,
    run_id: &str,
    conversation_id: Option<&str>,
    browser: Option<(PathBuf, PathBuf)>,
    permission: PermissionPolicy,
) -> Result<Option<PathBuf>> {
    if access.is_none() && browser.is_none() {
        return Ok(None);
    }
    let dir = crate::paths::run_dir(run_id);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("mcp.json");
    write_config(
        &path,
        access,
        jod_home,
        browser,
        Some(run_id),
        conversation_id,
        Some(permission),
    )
}

fn write_config(
    path: &Path,
    access: Option<ToolAccess>,
    jod_home: &Path,
    browser: Option<(PathBuf, PathBuf)>,
    run_id: Option<&str>,
    conversation_id: Option<&str>,
    permission: Option<PermissionPolicy>,
) -> Result<Option<PathBuf>> {
    // The running executable, not a name looked up on PATH. A daemon started
    // from a build directory must point agents at *that* binary, or they get
    // whichever `jod` a shell would have found — possibly an older install,
    // possibly none.
    let exe = std::env::current_exe()?;

    // The child must open the same database this process is using. Inheriting
    // the environment would work today and break the moment a daemon runs with
    // a JOD_HOME its children do not.
    let mut env = serde_json::Map::new();
    env.insert(
        "JOD_HOME".to_string(),
        jod_home.to_string_lossy().to_string().into(),
    );
    // **Written even with no run, as an empty string, because leaving a
    // variable out does not unset it — it inherits it.** On the path that opens
    // a work the spawn chain begins at the *orchestrator's* MCP server, so
    // omitting the key let its run id fall all the way through and a fresh
    // session's server claimed the run that started it.
    //
    // Observed: the parity suite's OpenCode leg refused every tool with "its
    // process group belongs to run 9aeabbb1…, but its environment claims run
    // 8a82f92b…" — the main chat. Claude Code was unaffected only because its
    // per-run config overwrites the key, masking the leak rather than stopping
    // it.
    //
    // `identify` reads an empty claim as no claim, so a cleared variable means
    // "ask the process group". Nothing about `identify` was relaxed to fix
    // this, and nothing should be.
    env.insert(
        RUN_ID_ENV.to_string(),
        run_id.unwrap_or_default().to_string().into(),
    );
    env.insert(
        CONVERSATION_ID_ENV.to_string(),
        conversation_id.unwrap_or_default().to_string().into(),
    );

    let mut servers = serde_json::json!({});
    if let Some(access) = access {
        servers["jod"] = serde_json::json!({
            "command": exe.to_string_lossy(),
            "args": server_args(access, permission),
            "env": env,
        });
    }

    // The browser, at every access level including read-only.
    //
    // Reading a web page *is* a read, and the level exists to bound what an
    // agent may do to Jod — delegate, orchestrate, spend money — not to decide
    // whether it may look something up. An unattended run gets
    // `ToolAccess::unattended()`, which is read-only, and that is exactly the
    // run most likely to need a page: it has nobody to ask.
    //
    // Registered only when the script is actually on this machine. Claude Code
    // is given `--strict-mcp-config`, so an agent's servers are exactly the
    // ones named here — which means naming one that cannot start hands the
    // agent tools that fail on first use, and it will report that it cannot
    // browse rather than falling back to anything.
    if let Some((python, script)) = browser {
        servers["browser"] = serde_json::json!({
            "command": python.to_string_lossy(),
            "args": [script.to_string_lossy()],
            // Same argument as above: the server reads `~/.jod/browser.env` for
            // its proxy credentials, and must read the one belonging to the
            // JOD_HOME this process is using.
            "env": { "JOD_HOME": jod_home.to_string_lossy() },
        });
    }

    let doc = serde_json::json!({ "mcpServers": servers });
    std::fs::write(path, serde_json::to_vec_pretty(&doc)?)?;
    Ok(Some(path.to_path_buf()))
}

/// The argv for the `jod mcp` server this document starts.
///
/// `--max-permission` is the half that used to be missing, and its absence was
/// not cosmetic. The flag's own default is `accept_edits`, so every per-run
/// server silently held that ceiling however the run itself was launched — and
/// [`crate::mcp`]'s `open_work` caps what it opens against exactly that value.
/// A main chat running in `auto` therefore opened its background work in
/// `accept_edits`, where headless Claude Code has nobody to ask and dead-ends on
/// `git init`. The mode on the status bar was right; the mode on the child was
/// not, and nothing in between said so.
///
/// Only emitted when the run's policy is actually known. A shared config serves
/// sessions Jod did not launch, and inventing a ceiling for one of those would
/// be a guess dressed as identity.
fn server_args(access: ToolAccess, permission: Option<PermissionPolicy>) -> Vec<String> {
    let mut args = vec![
        "mcp".to_string(),
        "--access".to_string(),
        access.as_str().to_string(),
    ];
    if let Some(permission) = permission {
        args.push("--max-permission".to_string());
        args.push(permission.as_str().to_string());
    }
    args
}

/// Whether this machine can offer the browser at all.
pub fn browser_available() -> bool {
    crate::paths::browser_mcp_script().is_some()
}

/// The name the browser's tools appear under, as `mcp__browser__browse`.
///
/// Fixed here for the same reason [`SERVER_NAME`] is: the prompt that tells an
/// agent to route its browsing through these tools has to name them, and a
/// prompt naming tools that do not exist is worse than no prompt at all.
pub const BROWSER_SERVER_NAME: &str = "browser";

/// What every run is told about how it reaches the web.
///
/// This is a *prompt*, not a permission, and it has to be, because the
/// alternative does not exist: no harness offers "deny WebFetch but allow this
/// MCP server" as a single switch, and Claude Code's built-in `WebFetch` and
/// `WebSearch` are granted or denied by name alongside everything else. So the
/// instruction is how the routing actually happens, and it is stated in terms
/// of what an agent gets out of complying — reaching pages that would otherwise
/// refuse it — rather than as a rule, because an agent that understands why a
/// tool is better uses it when it matters and a rule gets followed until it is
/// inconvenient.
///
/// Only ever added when the browser is actually registered.
pub const BROWSER_PROMPT: &str = "\
Web access: use the `browser` MCP tools (mcp__browser__browse and its \
siblings) for every page you read, in preference to any built-in fetch or \
search tool. They drive a real Firefox with patched fingerprints egressing \
through a residential proxy, so they reach pages that block a plain HTTP \
fetch — which is most pages worth reading. A direct fetch also exposes this \
machine's own IP. If a page needs a click or a login, use browser_open then \
browser_click / browser_type: the session keeps its cookies between calls.";

/// The framing a run is launched with, given what it was granted.
///
/// Returns `None` when there is nothing to say, so a caller can leave
/// `SpawnRequest::system` alone rather than setting it to an empty string —
/// which some harnesses take as a system prompt that says nothing, and others
/// as no system prompt at all.
pub fn framing(existing: Option<&str>) -> Option<String> {
    framing_with(existing, browser_available())
}

/// [`framing`], with the machine's answer passed in.
///
/// Injected for the same reason `config_with` exists: whether this box has the
/// browser is process-wide state, and a test that reads it can only assert the
/// branch that machine happens to be on. Guarding the other branch behind an
/// `if` looks like a test and is not one — it passes by not running.
pub fn framing_with(existing: Option<&str>, browser: bool) -> Option<String> {
    if !browser {
        return existing.map(str::to_string);
    }
    Some(match existing {
        Some(text) if !text.trim().is_empty() => format!("{text}\n\n{BROWSER_PROMPT}"),
        _ => BROWSER_PROMPT.to_string(),
    })
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

    /// A machine with the browser installed, as `config_with` wants it.
    fn with_browser() -> Option<(PathBuf, PathBuf)> {
        Some((
            PathBuf::from("/usr/bin/python3"),
            PathBuf::from("/home/x/.jod/browser/jod_browser_mcp.py"),
        ))
    }

    fn read(path: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn a_config_names_the_running_binary_rather_than_trusting_the_path() {
        let home = scratch("exe");
        let path = config_with(Some(ToolAccess::ReadOnly), &home, None)
            .unwrap()
            .unwrap();
        let doc = read(&path);

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
        let path = config_with(Some(ToolAccess::Delegate), &home, None)
            .unwrap()
            .unwrap();
        assert_eq!(
            read(&path)["mcpServers"]["jod"]["env"]["JOD_HOME"]
                .as_str()
                .unwrap(),
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
            let path = config_with(Some(access), &home, None).unwrap().unwrap();
            assert!(
                path.to_string_lossy().contains(access.as_str()),
                "{path:?} does not name {access:?}"
            );
            let doc = read(&path);
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

    /// The whole point of a per-run config: the server can say who is calling
    /// without being told by the caller.
    #[test]
    fn a_runs_config_pins_the_run_so_the_server_knows_who_is_calling() {
        let home = scratch("run");
        let path = config_for_run_with(
            Some(ToolAccess::Delegate),
            &home,
            "run-42",
            Some("conv-7"),
            None,
            PermissionPolicy::Bypass,
        )
        .unwrap()
        .unwrap();
        let doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let env = &doc["mcpServers"]["jod"]["env"];
        assert_eq!(env[RUN_ID_ENV].as_str().unwrap(), "run-42");
        assert_eq!(env[CONVERSATION_ID_ENV].as_str().unwrap(), "conv-7");
        // The level still travels in the argv, unchanged: identity says who you
        // are, never what you may do. The ceiling travels beside it, because a
        // server that cannot name the run's policy caps everything it opens at
        // the flag's own default instead.
        let args: Vec<&str> = doc["mcpServers"]["jod"]["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();
        assert_eq!(
            args,
            vec!["mcp", "--access", "delegate", "--max-permission", "bypass"]
        );
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(crate::paths::run_dir("run-42"));
    }

    /// **Regression: a run in `auto` opened background work in `accept_edits`.**
    ///
    /// The server's ceiling was never passed, so it took `--max-permission`'s
    /// own default — `accept_edits` — whatever the run holding it was launched
    /// with. `open_work` caps against that ceiling, so a main chat the operator
    /// had put in `auto` spawned children one level down, where headless Claude
    /// Code has nobody to ask and refuses `git init` outright. The status bar
    /// said `auto` and was telling the truth about the wrong process.
    ///
    /// Asserted across every policy rather than at `Bypass` alone: the failure
    /// was a default silently standing in for a real value, and a test pinning
    /// one value would keep passing if the others were dropped on the floor.
    #[test]
    fn a_runs_server_holds_that_runs_own_ceiling_and_never_the_flags_default() {
        let home = scratch("ceiling");
        for policy in PermissionPolicy::ALL {
            let run = format!("run-ceiling-{}", policy.as_str());
            let path = config_for_run_with(
                Some(ToolAccess::Orchestrate),
                &home,
                &run,
                None,
                None,
                policy,
            )
            .unwrap()
            .unwrap();
            let doc: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            let args: Vec<String> = doc["mcpServers"]["jod"]["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a.as_str().unwrap().to_string())
                .collect();
            let at = args
                .iter()
                .position(|a| a == "--max-permission")
                .unwrap_or_else(|| panic!("{policy:?} wrote no ceiling at all: {args:?}"));
            assert_eq!(
                args.get(at + 1).map(String::as_str),
                Some(policy.as_str()),
                "{policy:?} did not reach its own server: {args:?}"
            );
            // The spelling has to be one the CLI actually parses back, or the
            // server dies on startup and the run silently loses every Jod tool.
            assert_eq!(
                crate::mcp::parse_permission(policy.as_str()),
                Some(policy),
                "`{}` does not round-trip through the argument parser",
                policy.as_str()
            );
            let _ = std::fs::remove_dir_all(crate::paths::run_dir(&run));
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A grant left on disk by a killed run is exactly what the shared
    /// directory was kept free of, so a per-run config lives with the run.
    #[test]
    fn a_runs_config_lives_with_that_runs_own_files() {
        let home = scratch("run-dir");
        let path = config_for_run_with(
            Some(ToolAccess::ReadOnly),
            &home,
            "run-43",
            None,
            None,
            PermissionPolicy::AcceptEdits,
        )
        .unwrap()
        .unwrap();
        assert!(
            path.starts_with(crate::paths::run_dir("run-43")),
            "{path:?} is not in the run's own directory"
        );
        let doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            doc["mcpServers"]["jod"]["env"][CONVERSATION_ID_ENV].as_str(),
            Some(""),
            "a conversation nobody knew yet must be cleared, never invented and never inherited"
        );
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(crate::paths::run_dir("run-43"));
    }

    /// **A shared config must clear the run id, not omit it.**
    ///
    /// Omitting a variable does not unset it for the child — it inherits it.
    /// The chain that produced the bug: an MCP server is started by the
    /// harness, the harness by the supervisor, the supervisor by whatever asked
    /// for the run, and on the path that opens a work that is the
    /// *orchestrator's* own MCP server, whose environment names the
    /// orchestrator's run. So a fresh session's server claimed the run that
    /// started it, `identify` saw the process group and the environment
    /// disagree, and every tool needing a sender was refused.
    ///
    /// Asserting the key is present *and* empty rather than merely absent is
    /// the whole of the fix: revert `write_config` to skipping the key and this
    /// fails.
    #[test]
    fn a_shared_config_clears_the_run_id_rather_than_leaving_it_to_be_inherited() {
        let home = scratch("shared");
        let path = config_with(Some(ToolAccess::ReadOnly), &home, None)
            .unwrap()
            .unwrap();
        let doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let env = &doc["mcpServers"]["jod"]["env"];
        assert_eq!(
            env[RUN_ID_ENV].as_str(),
            Some(""),
            "an absent key inherits the launcher's run; an empty one overrides it"
        );
        assert_eq!(env[CONVERSATION_ID_ENV].as_str(), Some(""));
        let _ = std::fs::remove_dir_all(&home);
    }

    /// And the empty value has to mean what the fix assumes it means. Asserted
    /// against `identify` itself rather than trusted, because the two halves
    /// live in different files and only their agreement makes this work.
    #[test]
    fn an_empty_claim_is_no_claim_so_the_process_group_decides() {
        let store = crate::store::Store::in_memory().unwrap();
        assert_eq!(
            crate::mcp::identify(&store, Some("")),
            crate::mcp::Identity::Unknown,
            "a cleared run id must fall back to the process group, not dispute with it"
        );
    }

    /// Rewritten every call, so an upgraded binary does not leave agents
    /// pointing at a path that no longer exists.
    #[test]
    fn writing_twice_leaves_one_current_file() {
        let home = scratch("rewrite");
        let first = config_with(Some(ToolAccess::ReadOnly), &home, None)
            .unwrap()
            .unwrap();
        std::fs::write(&first, b"{\"stale\": true}").unwrap();
        let second = config_with(Some(ToolAccess::ReadOnly), &home, None)
            .unwrap()
            .unwrap();
        assert_eq!(first, second);
        assert!(
            read(&second)["mcpServers"]["jod"].is_object(),
            "stale content survived"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    // ---- the browser ----------------------------------------------------

    /// The change this file's `Option<ToolAccess>` exists for. A run granted
    /// none of Jod's verbs may still read a web page, because reading a page is
    /// not one of Jod's verbs — it touches no run, no schedule and no memory.
    #[test]
    fn a_run_granted_no_jod_tools_still_gets_the_browser() {
        let home = scratch("browser-only");
        let path = config_with(None, &home, with_browser()).unwrap().unwrap();
        let doc = read(&path);
        assert!(
            doc["mcpServers"]["jod"].is_null(),
            "a run granted nothing must not reach Jod's own verbs"
        );
        assert!(
            doc["mcpServers"]["browser"].is_object(),
            "but it must still be able to browse"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Every level, not just the privileged ones. An unattended run is
    /// read-only by design and is the one most likely to need a page, because
    /// it has nobody to ask.
    #[test]
    fn the_browser_is_offered_at_every_access_level() {
        let home = scratch("browser-levels");
        for access in [
            None,
            Some(ToolAccess::ReadOnly),
            Some(ToolAccess::Delegate),
            Some(ToolAccess::Orchestrate),
        ] {
            let path = config_with(access, &home, with_browser()).unwrap().unwrap();
            assert!(
                read(&path)["mcpServers"]["browser"].is_object(),
                "{access:?} was not offered the browser"
            );
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Claude Code gets `--strict-mcp-config`, so the servers named here are
    /// exactly the ones an agent has. Naming one whose command does not exist
    /// would hand it tools that fail on first use.
    #[test]
    fn a_machine_without_the_browser_does_not_advertise_it() {
        let home = scratch("no-browser");
        let path = config_with(Some(ToolAccess::ReadOnly), &home, None)
            .unwrap()
            .unwrap();
        assert!(read(&path)["mcpServers"]["browser"].is_null());
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Nothing to grant and nothing to offer is not an empty config file — it
    /// is no `--mcp-config` flag at all.
    #[test]
    fn nothing_to_offer_writes_nothing() {
        let home = scratch("nothing");
        assert!(config_with(None, &home, None).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Same argument as the database: the server reads its proxy credentials
    /// from `~/.jod/browser.env`, and must read the one this process is using.
    #[test]
    fn the_browser_is_pinned_to_the_same_jod_home() {
        let home = scratch("browser-home");
        let path = config_with(None, &home, with_browser()).unwrap().unwrap();
        let doc = read(&path);
        assert_eq!(
            doc["mcpServers"]["browser"]["env"]["JOD_HOME"]
                .as_str()
                .unwrap(),
            home.to_string_lossy()
        );
        assert_eq!(
            doc["mcpServers"]["browser"]["command"].as_str().unwrap(),
            "/usr/bin/python3"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    // ---- the framing ----------------------------------------------------

    /// The prompt has to name the tools it is telling the agent to use, or it
    /// is advice about something the agent cannot find.
    #[test]
    fn the_browser_prompt_names_the_tools_it_asks_for() {
        assert!(BROWSER_PROMPT.contains("mcp__browser__browse"));
        assert!(BROWSER_PROMPT.contains(BROWSER_SERVER_NAME));
    }

    /// A caller's own framing is a role; the browser note is a fact about the
    /// machine. Losing either one would be a run that is missing half of what
    /// it was told.
    #[test]
    fn framing_keeps_the_callers_own_system_prompt() {
        let framed = framing_with(Some("You are reviewing a PR."), true).unwrap();
        assert!(framed.contains("You are reviewing a PR."));
        assert!(framed.contains("mcp__browser__browse"));
    }

    /// An empty string is not a system prompt somebody wrote; treating it as
    /// one would leave a leading blank line in front of every framing.
    #[test]
    fn framing_treats_blank_text_as_absent() {
        assert_eq!(framing_with(Some("   "), true).unwrap(), BROWSER_PROMPT);
        assert_eq!(framing_with(None, true).unwrap(), BROWSER_PROMPT);
    }

    /// No browser, nothing to add — and in particular not an empty string,
    /// which some harnesses read as a system prompt that says nothing.
    #[test]
    fn framing_adds_nothing_when_there_is_no_browser_to_describe() {
        assert_eq!(framing_with(None, false), None);
        assert_eq!(framing_with(Some("role"), false), Some("role".to_string()));
    }

    /// The TUI and `jod run` both spawn with `tools: None`, and both go through
    /// `runner::launch`. If the framing dropped the browser note for a run
    /// granted no Jod verbs, every interactive turn would lose it — which is
    /// most of how Jod is actually driven.
    #[test]
    fn a_run_granted_no_jod_tools_is_still_told_how_to_browse() {
        assert!(framing_with(None, true).unwrap().contains("mcp__browser"));
    }
}
