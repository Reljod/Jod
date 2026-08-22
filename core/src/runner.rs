//! Launching an agent, and watching one.
//!
//! There is no shell here, and no file in the middle. Jod writes a plan, starts
//! a detached `jod-run` supervisor on it, and the supervisor appends the run's
//! events straight into SQLite. Watching a run is then a query, which is why any
//! process — the CLI that started it, a daemon that restarted since, an HTTP
//! client on a phone — can follow the same run without sharing anything but the
//! database.
//!
//! What used to be here: a generated bash script, `tee`, a JSONL file, and a
//! tailer that had to belong to the process that spawned the agent. All four
//! existed to get bytes from a tmux pane into the store. → `docs/decisions.md`

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

use crate::error::{JodError, Result};
use crate::event::AgentEnvelope;
use crate::harness::{ArgPart, Harness, HarnessKind, SpawnRequest};
use crate::store::Store;
use crate::{discovery, paths, proc};

/// Everything the supervisor needs, and the human-readable record of exactly
/// what was launched. Written to `~/.jod/runs/<id>/spawn.json`.
///
/// Passed as a file rather than as arguments so that the supervisor's own
/// command line stays one short path — a plan is a thing you can read after the
/// fact, and `ps` output is not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpawnPlan {
    pub run_id: String,
    pub harness: HarnessKind,
    /// Absolute path to the database the supervisor writes into. Absolute
    /// because the supervisor runs with the *agent's* working directory.
    pub db_path: PathBuf,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Non-secret environment for the child process.
    ///
    /// This file is written so a person can read afterwards exactly what was
    /// launched, which is precisely why no credential may appear in it.
    #[serde(default)]
    pub env: Vec<(String, String)>,
    /// Names of secrets the supervisor should resolve and inject.
    ///
    /// Names, never values: the value is read at exec time from the owner-only
    /// secret file and exists only in the supervisor's memory and the child's
    /// environment. A plan on disk that named a value would be a second copy
    /// of the credential at ordinary permissions.
    ///
    /// The supervisor also builds its output scrubber from whatever these
    /// resolve to, so injection and redaction can never disagree about what is
    /// secret.
    #[serde(default)]
    pub secrets: Vec<String>,
}

/// Resolve `ArgPart::Prompt` into the real prompt.
///
/// It used to become a shell variable, because the launcher was a script and an
/// inlined prompt containing `$(...)` would have been re-parsed. There is no
/// shell now: argv goes to `execve` as it stands, so a prompt is just a string.
fn resolve_args(parts: &[ArgPart], prompt: &str) -> Vec<String> {
    parts
        .iter()
        .map(|p| match p {
            ArgPart::Literal(s) => s.clone(),
            ArgPart::Prompt => prompt.to_string(),
        })
        .collect()
}

/// Where the `jod-run` supervisor lives.
///
/// Next to the running executable first: `jod`, `jod-api` and a bundled desktop
/// app all ship it as a sibling, and that copy is the one built from the same
/// source as the caller. `PATH` is the fallback for a development shell.
pub fn locate_supervisor() -> Option<PathBuf> {
    discovery::find_binary("JOD_SUPERVISOR_BIN", &["jod-run"], &[])
}

/// A launched run, as the caller needs to talk about it.
#[derive(Debug, Clone)]
pub struct LaunchedRun {
    pub agent_id: String,
    /// The supervisor's pid, which is also the id of the process group holding
    /// both it and the harness.
    pub pid: u32,
    pub pgid: u32,
}

/// Write the run's files, start its detached supervisor, and begin forwarding
/// events. Returns as soon as the supervisor is up.
///
/// The run does not depend on this process surviving: it has its own session
/// and its output goes to the database, not down a pipe held here.
pub async fn launch(
    agent_id: &str,
    req: &SpawnRequest,
    program: &Path,
    harness: Box<dyn Harness>,
    store: Arc<Store>,
    tx: UnboundedSender<AgentEnvelope>,
) -> Result<LaunchedRun> {
    let supervisor = locate_supervisor().ok_or(JodError::SupervisorNotFound)?;

    let dir = paths::run_dir(agent_id);
    tokio::fs::create_dir_all(&dir).await?;
    // A harness that has no system-prompt flag still has to receive the
    // framing, so it goes in front of the prompt for those. Done here rather
    // than in each adapter because the prompt reaches argv as a placeholder —
    // an adapter cannot rewrite what it never holds.
    //
    // The framing also carries how this run reaches the web, when Jod has a
    // browser to offer. Done here, once, rather than at each of the twenty-odd
    // places that build a `SpawnRequest`: an instruction that has to be
    // remembered is one that will be missing from whichever call site is added
    // next, and "the agent browsed straight out of the VPS's own IP" is a
    // failure nobody sees until a site starts refusing.
    //
    // Rebound onto the request rather than used locally, because the harness
    // that *does* take a system prompt reads it off `req` to build the flag. A
    // local would have framed only the harnesses that cannot take one, which is
    // exactly backwards.
    let req = &SpawnRequest {
        system: crate::mcp_config::framing(req.system.as_deref()),
        ..req.clone()
    };
    let prompt = match (&req.system, harness.takes_system_prompt()) {
        (Some(system), false) => format!("{system}\n\n---\n\n{}", req.prompt),
        _ => req.prompt.clone(),
    };
    tokio::fs::write(paths::prompt_path(agent_id), &prompt).await?;

    // The run's own id, stamped over whatever the caller supplied.
    //
    // `args` is handed nothing but the request, and it needs the id to write a
    // per-run MCP config — that config is how Jod's own tools know which member
    // is calling them. Overwritten rather than trusted, because a caller that
    // set this would be naming a run it does not own, and sender identity is
    // the one thing on this path that must not be an argument.
    let req = &SpawnRequest {
        run_id: Some(agent_id.to_string()),
        ..req.clone()
    };

    let plan = SpawnPlan {
        run_id: agent_id.to_string(),
        harness: harness.kind(),
        db_path: store.path().ok_or(JodError::StoreRequired)?,
        program: program.to_path_buf(),
        // The run's own store, handed to the adapter rather than left for it to
        // find. Claude Code reads the standing grants out of it; the others
        // ignore it. See `Harness::args`.
        args: resolve_args(&harness.args(req, Some(store.as_ref())), &prompt),
        cwd: req.cwd.clone(),
        env: req.env.clone(),
        secrets: req.secrets.clone(),
    };
    let plan_file = paths::spawn_path(agent_id);
    tokio::fs::write(&plan_file, serde_json::to_vec_pretty(&plan)?).await?;

    // The supervisor runs from the run's own directory, not the agent's: it
    // must keep working if the agent's working directory is removed underneath
    // it, and it addresses everything else by absolute path anyway.
    let pid = proc::spawn_detached(
        &supervisor,
        &[plan_file.to_string_lossy().to_string()],
        &dir,
        &paths::supervisor_log_path(agent_id),
    )
    .map_err(|e| JodError::Spawn(format!("could not start `{}`: {e}", supervisor.display())))?;

    // `setsid` made the supervisor a session and group leader, so its pid is
    // its pgid. Recorded rather than recomputed, because by the time anyone
    // asks, the process may be gone and `getpgid` would fail.
    store.set_run_process(agent_id, pid, pid)?;

    tokio::spawn(follow(agent_id.to_string(), pid, store, tx, None));

    Ok(LaunchedRun {
        agent_id: agent_id.to_string(),
        pid,
        pgid: pid,
    })
}

/// How long to keep reading after the supervisor's group disappears.
///
/// A supervisor that dies writes its last events just before it goes, so a
/// reader that stopped the instant the process vanished would race it.
const GRACE_POLLS_AFTER_GROUP_GONE: u32 = 8;
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(120);
const BATCH: usize = 512;

/// Forward a run's events from the store onto `tx`, oldest first, until it ends.
///
/// This is deliberately a poll rather than a subscription. SQLite in WAL mode
/// lets a reader run while the supervisor writes, and 120 ms is the same
/// latency the file tailer had — while costing nothing but a query, and working
/// for a process that did not launch the run and holds no handle to it.
///
/// Public because "watch this run" is a capability every client wants and the
/// core is the only place that should know how it is done.
pub async fn follow(
    agent_id: String,
    pgid: u32,
    store: Arc<Store>,
    tx: UnboundedSender<AgentEnvelope>,
    after: Option<u64>,
) {
    let mut cursor: Option<u64> = after;
    let mut idle_after_group_gone = 0u32;

    loop {
        let batch = match store.events_since(&agent_id, cursor, BATCH) {
            Ok(b) => b,
            Err(e) => {
                // The store is how a run reports anything at all, so a read
                // failure is reported into the stream rather than swallowed.
                let _ = tx.send(AgentEnvelope {
                    agent_id: agent_id.clone(),
                    at_ms: chrono::Utc::now().timestamp_millis(),
                    seq: cursor.map_or(0, |c| c + 1),
                    event: crate::event::AgentEvent::Error {
                        message: format!("could not read the run's events: {e}"),
                    },
                });
                return;
            }
        };

        let mut finished = false;
        for envelope in batch {
            cursor = Some(envelope.seq);
            finished |= matches!(envelope.event, crate::event::AgentEvent::Finished { .. });
            if tx.send(envelope).is_err() {
                return; // nobody is listening any more
            }
        }

        if finished {
            return;
        }

        if proc::group_alive(pgid) {
            idle_after_group_gone = 0;
        } else {
            idle_after_group_gone += 1;
            if idle_after_group_gone >= GRACE_POLLS_AFTER_GROUP_GONE {
                return;
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::AgentEvent;

    #[test]
    fn the_prompt_is_substituted_verbatim_because_no_shell_sees_it() {
        // The old launcher had to hide this behind a variable; `execve` does not
        // interpret its arguments, so the value passes through untouched.
        let hostile = "'; rm -rf /tmp/x; echo $(id) `whoami`";
        let args = resolve_args(&[ArgPart::lit("-p"), ArgPart::Prompt], hostile);
        assert_eq!(args, vec!["-p".to_string(), hostile.to_string()]);
    }

    #[test]
    fn literals_pass_through_unquoted() {
        let args = resolve_args(&[ArgPart::lit("--model"), ArgPart::lit("opus")], "p");
        assert_eq!(args, vec!["--model".to_string(), "opus".to_string()]);
    }

    #[test]
    fn a_plan_round_trips_through_json() {
        let plan = SpawnPlan {
            run_id: "r1".into(),
            harness: HarnessKind::ClaudeCode,
            db_path: "/home/x/.jod/jod.db".into(),
            program: "/bin/claude".into(),
            args: vec!["-p".into(), "hello".into()],
            cwd: "/work".into(),
            env: Vec::new(),
            secrets: Vec::new(),
        };
        let json = serde_json::to_string(&plan).unwrap();
        assert_eq!(serde_json::from_str::<SpawnPlan>(&json).unwrap(), plan);
    }

    fn envelope(id: &str, seq: u64, event: AgentEvent) -> AgentEnvelope {
        AgentEnvelope {
            agent_id: id.into(),
            at_ms: 0,
            seq,
            event,
        }
    }

    #[tokio::test]
    async fn following_a_finished_run_replays_it_and_stops() {
        let store = Arc::new(Store::in_memory().unwrap());
        for (seq, event) in [
            AgentEvent::Started {
                session_id: Some("s".into()),
                model: None,
            },
            AgentEvent::Message { text: "hi".into() },
            AgentEvent::Finished {
                text: Some("done".into()),
                exit_code: Some(0),
                is_error: false,
                usage: Default::default(),
            },
        ]
        .into_iter()
        .enumerate()
        {
            store.append_event(&envelope("r", seq as u64, event)).unwrap();
        }

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        // pgid 4_000_000 is not alive; the run must still be replayed in full,
        // because the events are already there and the reader is late, not lost.
        follow("r".into(), 4_000_000, store, tx, None).await;

        let mut seen = Vec::new();
        while let Ok(e) = rx.try_recv() {
            seen.push(e.seq);
        }
        assert_eq!(seen, vec![0, 1, 2], "a late follower still gets the whole run");
    }

    #[tokio::test]
    async fn a_follower_gives_up_on_a_run_whose_supervisor_vanished() {
        // No Finished event and no live process: the follower must return rather
        // than poll a dead run for ever.
        let store = Arc::new(Store::in_memory().unwrap());
        store
            .append_event(&envelope("r", 0, AgentEvent::Message { text: "x".into() }))
            .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            follow("r".into(), 4_000_000, store, tx, None),
        )
        .await
        .expect("the follower must not hang on a dead run");

        assert_eq!(rx.try_recv().unwrap().seq, 0);
    }
}
