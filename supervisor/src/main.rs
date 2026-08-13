//! `jod-run` — one supervisor per agent run.
//!
//! This is the process that replaced tmux. Jod starts it detached, in its own
//! session, with a single argument: the path to the run's `spawn.json`. From
//! then on it is the only thing holding the harness's output, and it puts that
//! output where every client can reach it — the run's rows in `jod.db`.
//!
//! Two properties are the whole reason it exists as a separate program:
//!
//! - **It outlives whoever started it.** A thread inside `jod` cannot; the
//!   harness's stdout pipe would break the moment that process exited, and the
//!   agent would die with the terminal that launched it.
//! - **It is the single writer of a run's events.** Sequence numbers come from
//!   one counter in one process, so `(run_id, seq)` is dense and ordered
//!   without any coordination.
//!
//! It reports failure loudly. A run that could not start, could not be parsed,
//! or was killed all end with a terminal event and a recorded status, because a
//! run that simply stops being mentioned looks exactly like one that succeeded.
//!
//! It is also the only process that ever holds a secret's value. It reads the
//! value out of its owner-only file, puts it in the child's environment, and
//! scrubs it back out of everything the child prints — see [`inject`]. That
//! works only because this one process sits on both sides of the harness at
//! once; nothing upstream of it, and nothing downstream, sees the value at all.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use jod_core::cards::{CardKind, Importance, NewCard, Source};
use jod_core::event::{AgentEnvelope, AgentEvent};
use jod_core::redact::Scrubber;
use jod_core::runner::SpawnPlan;
use jod_core::secrets::read_secret_value;
use jod_core::service::AgentStatus;
use jod_core::store::Store;
use jod_core::workdir;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

/// How long a harness gets to exit after being asked, before it is made to.
const KILL_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// `0.1.0 (f4e4c72 2026-08-13)` — release, commit, commit date.
///
/// The same string, built the same way, that `jod --version` answers with:
/// `CARGO_PKG_VERSION` from the workspace, then the two variables
/// `build.rs` stamps on. The two binaries ship in one tarball and are looked
/// up as siblings, so they have to name a build identically or the answer is
/// worse than useless. → `cli/src/version.rs`
const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("JOD_BUILD_ID"),
    " ",
    env!("JOD_BUILD_DATE"),
    ")"
);

/// What `--help` prints.
///
/// Short on purpose. This is not a program anybody drives by hand — Jod writes
/// the plan and starts the process — so the useful thing to say is what the one
/// argument is and where it comes from, not a flag table.
const USAGE: &str = "\
jod-run — supervises one agent run and writes its events into SQLite.

Usage:
  jod-run <spawn.json>   supervise the run that plan describes
  jod-run --version      print the version, with the commit it was built from
  jod-run --help         print this

Jod starts one of these per run, detached and in its own session, with the
path to that run's `spawn.json`. The plan names the harness to launch, the
database to write into, and the directory to work in. Nothing else is read
from the command line, and there is no reason to start one by hand.
";

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let plan_path = match Invocation::of(std::env::args().nth(1)) {
        Invocation::Version => {
            println!("jod-run {LONG_VERSION}");
            return std::process::ExitCode::SUCCESS;
        }
        Invocation::Help => {
            print!("{USAGE}");
            return std::process::ExitCode::SUCCESS;
        }
        Invocation::Usage => {
            eprintln!("usage: jod-run <spawn.json>");
            return std::process::ExitCode::from(64);
        }
        Invocation::Plan(path) => path,
    };

    match run(&plan_path).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            // Nothing above can see this except `supervisor.log`, which is
            // exactly what that file is for.
            eprintln!("jod-run: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// What the first argument means.
///
/// The bug this exists for: there was no such question. The first argument was
/// turned straight into a `PathBuf` and handed to `std::fs::read`, so
/// `jod-run --version` answered `reading "--version": No such file or
/// directory` and exited 1 — the binary tried to *open* the flag.
///
/// Deliberately not a parser. `jod-run` is spawned by
/// `core::runner::launch`, which hands it exactly one argument: an absolute
/// path to the run's `spawn.json`. An absolute path cannot begin with `-`, so
/// looking at the first argument and recognising four exact spellings changes
/// nothing about any invocation that exists, while a real argument parser
/// would have opinions about unknown flags, `--`, and repeated arguments that
/// this program has never had. Everything that is not one of the four is still
/// a path, including how it fails.
///
/// The four spellings are the ones `jod` accepts, because someone who learned
/// them from the sibling binary will type them here.
enum Invocation {
    Plan(PathBuf),
    Version,
    Help,
    /// Nothing to supervise.
    Usage,
}

impl Invocation {
    fn of(first: Option<String>) -> Invocation {
        match first.as_deref() {
            Some("--version" | "-V") => Invocation::Version,
            Some("--help" | "-h") => Invocation::Help,
            Some(path) => Invocation::Plan(PathBuf::from(path)),
            None => Invocation::Usage,
        }
    }
}

async fn run(plan_path: &PathBuf) -> Result<(), String> {
    let raw = std::fs::read(plan_path).map_err(|e| format!("reading {plan_path:?}: {e}"))?;
    let plan: SpawnPlan =
        serde_json::from_slice(&raw).map_err(|e| format!("parsing {plan_path:?}: {e}"))?;

    let store = Arc::new(
        Store::open(&plan.db_path).map_err(|e| format!("opening {:?}: {e}", plan.db_path))?,
    );

    let mut writer = EventWriter::new(plan.run_id.clone(), store.clone());
    let mut harness = plan.harness.build();

    // Resolved before the child exists, because both halves of the promise are
    // built from the same list: what goes into the environment is exactly what
    // comes back out of the output.
    let injected = inject(&store, &plan, &mut writer);
    let scrubber = Scrubber::new(injected.iter().map(|(_, value)| value.clone()));

    let child = Command::new(&plan.program)
        .args(&plan.args)
        .current_dir(&plan.cwd)
        // Stamp *this* run's id over whatever was inherited, before anything
        // else in the environment is applied.
        //
        // The supervisor is the only process that knows, without being told,
        // which run it is supervising — that is its whole job. Everything below
        // it inherits this: the harness, and the Jod MCP server the harness
        // starts. So an agent's tools resolve the right identity even on a
        // harness with no per-run config document to carry one.
        //
        // Without it the variable simply flowed down the spawn chain. On the
        // path that opens a work the supervisor's own parent is the
        // orchestrator's MCP server, so its run id reached every descendant,
        // and `identify` — correctly — refused to act for a server whose
        // environment named one run while its process group named another.
        // Clearing it in the config writers was necessary and not sufficient:
        // it stops Jod *writing* a stale id, and this stops one *arriving*.
        .env(jod_core::mcp_config::RUN_ID_ENV, &plan.run_id)
        .envs(plan.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .envs(injected.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        // Nothing may read a terminal: this process has none, and a harness
        // that stops to ask a question would hang for an answer that can never
        // come. `agy --conversation` does exactly that on a resumed run.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();

    // The child has them now, and the scrubber has its own copies. Nothing else
    // in this process needs the values, so it stops holding them.
    drop(injected);

    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            // A spawn failure is a finished run, not a missing one. Recording
            // it is the difference between "the agent failed" and "the agent is
            // still thinking", which is the failure mode the charter names.
            let message = format!("could not start {:?}: {e}", plan.program);
            writer.emit(AgentEvent::Error {
                message: message.clone(),
            });
            writer.emit(harness.finalize(Some(127)));
            writer.set_status(AgentStatus::Failed);
            return Err(message);
        }
    };

    // stdout and stderr are merged into one line stream, which is what
    // `2>&1 | tee` used to do: harnesses print human-readable prose on stderr
    // alongside their JSON, and `Raw` is how that prose reaches the transcript
    // instead of vanishing.
    let (lines_tx, mut lines) = mpsc::unbounded_channel::<String>();
    if let Some(out) = child.stdout.take() {
        tokio::spawn(pump(out, lines_tx.clone()));
    }
    if let Some(err) = child.stderr.take() {
        tokio::spawn(pump(err, lines_tx.clone()));
    }
    drop(lines_tx); // so the channel closes once both pipes reach EOF

    let mut terminating = false;
    let mut outcome: Option<Exit> = None;
    // Every file this run put bytes into, as it goes. Accumulated here rather
    // than read back out of the events afterwards because this process already
    // has them in hand, and it is the one process that cannot miss any.
    let mut wrote: Vec<PathBuf> = Vec::new();

    let mut sigterm = signal_stream(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = signal_stream(tokio::signal::unix::SignalKind::interrupt())?;

    loop {
        tokio::select! {
            // Bias towards draining output: when a child exits with buffered
            // lines still in flight, the last thing it said matters most.
            biased;

            line = lines.recv() => match line {
                Some(line) => {
                    // Before the parser, not after. A value scrubbed after the
                    // JSON is decoded has already passed through code that can
                    // log, and `Raw` would carry the undecoded line verbatim
                    // into the transcript. `is_empty` is the common case — no
                    // secrets in play — and costs one branch.
                    let line = if scrubber.is_empty() {
                        line
                    } else {
                        scrubber.scrub(&line)
                    };
                    for event in harness.parse_line(&line) {
                        if let AgentEvent::Started { session_id: Some(id), .. } = &event {
                            writer.set_session(id);
                        }
                        if let AgentEvent::ToolCall { name, input } = &event {
                            if let Some(path) = workdir::written_path(name, input.as_ref()) {
                                // Relative to the harness, which is running in
                                // `plan.cwd` — not to this process, which is
                                // deliberately somewhere else entirely.
                                wrote.push(if path.is_absolute() {
                                    path
                                } else {
                                    plan.cwd.join(path)
                                });
                            }
                        }
                        writer.emit(event);
                    }
                }
                None => break, // both pipes closed
            },

            _ = sigterm.recv(), if !terminating => {
                terminating = true;
                stop(&mut child).await;
            },

            _ = sigint.recv(), if !terminating => {
                terminating = true;
                stop(&mut child).await;
            },

            status = child.wait(), if outcome.is_none() => {
                outcome = Some(Exit::of(status.ok()));
            },
        }
    }

    let outcome = match outcome {
        Some(o) => o,
        None => Exit::of(child.wait().await.ok()),
    };

    // The runner owns "the run is over", so the adapter is asked for its
    // accumulated answer and cost even when the harness printed no final
    // record of its own.
    let finished = harness.finalize(outcome.code);
    let errored = matches!(finished, AgentEvent::Finished { is_error: true, .. });
    writer.emit(finished);

    let status = match (terminating || outcome.signalled, errored) {
        (true, _) => AgentStatus::Killed,
        (false, true) => AgentStatus::Failed,
        (false, false) => AgentStatus::Completed,
    };
    writer.set_status(status);

    // Only for a run that says it succeeded. A failure already has something to
    // say for itself, and the state this is about is the one that says nothing:
    // `✓ done`, real money spent, and the directory you pointed at untouched.
    if status == AgentStatus::Completed {
        note_writes_outside_the_workspace(&store, &plan, &wrote);
    }

    Ok(())
}

/// Say so when a run finished having written nothing where it was pointed.
///
/// The failure this exists for is silent in both directions: the agent believed
/// it had succeeded, the run's row agreed, the fleet showed a green check — and
/// every file it produced was in the user's home directory, while the directory
/// they had added stayed empty. Nothing anywhere would ever have told them.
///
/// The workspace is the run's own working directory *plus* whatever its
/// conversation declares now, read at the end rather than taken from the plan
/// so that a worktree the agent claimed mid-run counts as somewhere it was
/// meant to write.
///
/// A card rather than a status change, and the distinction is the honest one:
/// the run really did complete. What it did not do is land anywhere anybody
/// asked for, and that is a thing to tell a person, not a way to relabel an
/// exit code.
///
/// Every failure here is a line on stderr. A run whose work is already durably
/// recorded must not be reported as broken because a warning about it could not
/// be filed.
fn note_writes_outside_the_workspace(store: &Store, plan: &SpawnPlan, wrote: &[PathBuf]) {
    let conversation = match store.conversation_for_run(&plan.run_id) {
        Ok(Some(id)) => id,
        // A run with no conversation — a probe, a summariser — has nowhere to
        // put a card and nobody reading for one.
        Ok(None) => return,
        Err(e) => {
            eprintln!("jod-run: could not find the run's conversation: {e}");
            return;
        }
    };

    let mut workspace = vec![plan.cwd.clone()];
    match store.roots(&conversation) {
        Ok(roots) => workspace.extend(roots.into_iter().map(|r| r.path)),
        Err(e) => eprintln!("jod-run: could not read the conversation's roots: {e}"),
    }

    let Some(strays) = workdir::strayed(wrote, &workspace) else {
        return;
    };

    let listed = |paths: &[PathBuf]| -> String {
        paths
            .iter()
            .take(SHOWN_PATHS)
            .map(|p| format!("  {}\n", p.display()))
            .collect::<String>()
    };
    let more = strays.len().saturating_sub(SHOWN_PATHS);
    let body = format!(
        "This run reported success, and every file it wrote landed outside the \
         directories it was given.\n\n\
         It was working in:\n{}\n\
         It wrote to:\n{}{}\n\
         Nothing it produced is in a directory this session declared. If you \
         meant it to work in one of them, the work is not there — it is at the \
         paths above.",
        listed(&workspace),
        listed(&strays),
        if more > 0 {
            format!("  …and {more} more\n")
        } else {
            String::new()
        },
    );

    let card = NewCard {
        conversation_id: conversation,
        run_id: Some(plan.run_id.clone()),
        // It chose, and it is being reported so the choice can be overruled.
        // Nothing is waiting on an answer — the run is over — so this is not
        // blocking however much it matters.
        kind: Some(CardKind::Decision),
        importance: Some(Importance::High),
        blocking: false,
        title: "this run wrote outside every directory it was given".into(),
        body,
        options: workspace
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect(),
        chosen: strays
            .first()
            .and_then(|p| p.parent())
            .map(|p| p.to_string_lossy().to_string()),
        source: Some(Source::Jod),
        dedupe_key: Some(format!("stray-writes:{}", plan.run_id)),
        ..NewCard::default()
    };
    if let Err(e) = store.raise_card(card) {
        eprintln!("jod-run: could not raise the stray-writes card: {e}");
    }
}

/// How many paths a card names before it summarises. A card is a thing you read
/// in a rail, and a run that wrote sixty files would otherwise paste all sixty.
const SHOWN_PATHS: usize = 8;

/// Turn the plan's secret *names* into environment pairs.
///
/// This is the only place in Jod that reads a secret's value, and it is the
/// only place that can be: the plan on disk names secrets, the database records
/// what exists, and the value itself lives in a `0600` file that
/// [`read_secret_value`] refuses to open if its mode has widened. The pairs
/// returned here go straight into the child's environment and into the
/// scrubber, and nowhere else.
///
/// Applied *after* `plan.env`, so a secret always beats a plain variable of the
/// same name. The alternative — last writer wins by list order — would make
/// whether a credential arrived depend on the order two unrelated pieces of
/// code appended to a vector.
///
/// **A name that will not resolve is not fatal.** A missing key blocks one
/// test, not a session: the run proceeds without the variable, the reason is
/// recorded as an event and in `supervisor.log`, and the agent is expected to
/// end *blocked* rather than invent a credential. Killing the run here would
/// instead lose everything it had already been asked to do.
fn inject(store: &Store, plan: &SpawnPlan, writer: &mut EventWriter) -> Vec<(String, String)> {
    let mut resolved = Vec::new();
    for name in &plan.secrets {
        let outcome = store
            .secret_by_name(name)
            .map_err(|e| format!("looking it up: {e}"))
            .and_then(|meta| meta.ok_or_else(|| "no secret of that name is stored".to_string()))
            .and_then(|meta| read_secret_value(&meta).map_err(|e| e.to_string()));

        match outcome {
            Ok(value) => resolved.push((name.clone(), value)),
            Err(why) => {
                // `why` is built from the store's and the reader's errors, both
                // of which name paths and modes but never contents — a refusal
                // that quoted the value would be the leak it exists to stop.
                let message = format!(
                    "secret `{name}` was not available and was not injected ({why}); \
                     the run continues without it"
                );
                eprintln!("jod-run: {message}");
                writer.emit(AgentEvent::Error { message });
            }
        }
    }
    resolved
}

/// How the harness ended.
///
/// `signalled` is read from the exit status rather than inferred from having
/// handled a signal, because a `SIGTERM` aimed at the run's process group
/// reaches the harness *and* the supervisor at once. The harness usually dies
/// first, its pipes close, and the supervisor can finish the whole run before
/// its own handler ever gets a turn — at which point a killed run would be
/// recorded as a clean completion. Asking the status is not a race.
struct Exit {
    code: Option<i32>,
    signalled: bool,
}

impl Exit {
    fn of(status: Option<std::process::ExitStatus>) -> Exit {
        use std::os::unix::process::ExitStatusExt;
        Exit {
            code: status.and_then(|s| s.code()),
            signalled: status.is_some_and(|s| s.signal().is_some()),
        }
    }
}

/// Ask the harness to stop, and insist if it will not.
///
/// A `SIGTERM` aimed at the run's process group reaches the harness directly as
/// well, so this is usually redundant — but a supervisor signalled on its own
/// must still take the harness with it rather than leave it orphaned.
async fn stop(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        let _ = jod_core::proc::signal_group(pid, jod_core::proc::SIGTERM);
    }
    if tokio::time::timeout(KILL_GRACE, child.wait()).await.is_err() {
        let _ = child.start_kill();
    }
}

fn signal_stream(
    kind: tokio::signal::unix::SignalKind,
) -> Result<tokio::signal::unix::Signal, String> {
    tokio::signal::unix::signal(kind).map_err(|e| format!("installing a signal handler: {e}"))
}

/// Forward one pipe's lines into the shared stream.
///
/// Lossy UTF-8 rather than a hard error: a harness that emits one malformed
/// byte should degrade to a mangled character in the transcript, not silence
/// the rest of the run.
async fn pump<R>(reader: R, tx: mpsc::UnboundedSender<String>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = BufReader::new(reader).split(b'\n');
    while let Ok(Some(chunk)) = lines.next_segment().await {
        let line = String::from_utf8_lossy(&chunk)
            .trim_end_matches('\r')
            .to_string();
        if tx.send(line).is_err() {
            return;
        }
    }
}

/// Appends a run's events, in order, from the one process allowed to.
struct EventWriter {
    run_id: String,
    store: Arc<Store>,
    seq: u64,
}

impl EventWriter {
    fn new(run_id: String, store: Arc<Store>) -> EventWriter {
        EventWriter {
            run_id,
            store,
            seq: 0,
        }
    }

    fn emit(&mut self, event: AgentEvent) {
        let envelope = AgentEnvelope {
            agent_id: self.run_id.clone(),
            at_ms: chrono::Utc::now().timestamp_millis(),
            seq: self.seq,
            event,
        };
        self.seq += 1;
        // A failed write is reported and the run continues. Losing one event is
        // bad; killing a working agent because a row would not insert is worse.
        if let Err(e) = self.store.append_event(&envelope) {
            eprintln!("jod-run: could not persist event {}: {e}", envelope.seq);
        }
    }

    /// Record the harness's session id on the run **and on its conversation**.
    ///
    /// The conversation half used to be somewhere else entirely, and that is
    /// why it was missing. `set_conversation_session` had exactly one caller:
    /// the drain task inside whatever process launched the run. So the session
    /// id only ever landed if the launcher outlived the turn — and for a
    /// session opened through `open_work` the launcher is Jod's own MCP
    /// server, which exits when the harness closes stdin, which is roughly
    /// when the turn ends.
    ///
    /// The consequence was total rather than cosmetic: `resume_for` found
    /// nothing, so the session could not be resumed, so a work's session could
    /// not be spoken to again — no mail, no card answer, no second turn. On
    /// every harness; OpenCode was merely where it was noticed, because
    /// Claude Code's per-run config masked the related identity problem.
    ///
    /// It belongs here because the supervisor is the process that cannot miss
    /// it. It already owns "what actually happened", it already writes every
    /// event durably, and it outlives the launcher by construction — that is
    /// the whole reason a run is a detached process group.
    fn set_session(&self, session_id: &str) {
        if let Err(e) = self.store.set_run_session(&self.run_id, session_id) {
            eprintln!("jod-run: could not record the session id: {e}");
        }
        // The spawn writes the prompt row before the harness starts, so the
        // conversation exists by the time a `Started` event arrives. A run with
        // no conversation is an ordinary case — a detached summariser, a probe
        // — and not an error.
        match self.store.conversation_for_run(&self.run_id) {
            Ok(Some(conversation)) => {
                if let Err(e) = self
                    .store
                    .set_conversation_session(&conversation, Some(session_id))
                {
                    eprintln!("jod-run: could not record the session on its conversation: {e}");
                }
            }
            Ok(None) => {}
            Err(e) => eprintln!("jod-run: could not find the run's conversation: {e}"),
        }
    }

    fn set_status(&self, status: AgentStatus) {
        if let Err(e) = self.store.set_run_status(&self.run_id, status.as_str()) {
            eprintln!("jod-run: could not record the final status: {e}");
        }
    }
}
