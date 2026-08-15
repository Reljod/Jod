//! The claim this whole change rests on: a run outlives the process that
//! started it, and keeps reporting.
//!
//! tmux used to provide that. Now the supervisor does, and "it works" is not
//! something a unit test can assert — it needs a real spawner process that
//! really exits while a real agent is still producing output, and a *different*
//! process reading the result.
//!
//! So this test forks itself. The parent asks a short-lived child to launch the
//! run and then watches the child die; everything it checks afterwards is
//! checked against a database handle opened after that death.
//!
//! The harness is a shell script rather than `claude`, because what is under
//! test is the transport. Nothing here substitutes for a real harness anywhere
//! but in this file.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use jod_core::event::AgentEvent;
use jod_core::harness::HarnessKind;
use jod_core::runner::SpawnPlan;
use jod_core::store::Store;

/// Env var carrying the plan a re-invoked copy of this binary should launch.
const PLAN_ENV: &str = "JOD_TEST_PLAN";
/// Where that copy writes the pid it started, for its parent to read.
const PID_ENV: &str = "JOD_TEST_PID_FILE";

/// The spawner. Re-invoked as a separate process by the tests below, it starts
/// the supervisor, records its pid, and **exits immediately** — which is the
/// event the tests are actually about.
#[test]
#[ignore = "re-invoked as a subprocess by the tests in this file"]
fn spawner_child() {
    let Ok(plan_path) = std::env::var(PLAN_ENV) else {
        return;
    };
    let pid_file = std::env::var(PID_ENV).expect("the parent must say where to report");
    let plan: SpawnPlan =
        serde_json::from_slice(&std::fs::read(&plan_path).expect("plan")).expect("plan parses");
    let dir = Path::new(&plan_path).parent().unwrap().to_path_buf();

    let pid = jod_core::proc::spawn_detached(
        &supervisor_bin(),
        std::slice::from_ref(&plan_path),
        &dir,
        &dir.join("supervisor.log"),
    )
    .expect("the supervisor must start");

    Store::open(&plan.db_path)
        .expect("store")
        .set_run_process(&plan.run_id, pid, pid)
        .expect("recording the process");

    std::fs::write(pid_file, pid.to_string()).expect("reporting the pid");
    // and now this process ends, taking nothing with it.
}

#[test]
fn a_run_outlives_its_spawner_and_keeps_writing_events() {
    let fixture = Fixture::new("outlives", CHATTY_HARNESS, 0);
    let pid = fixture.launch_from_a_process_that_exits();

    // The spawner is gone; the run is not.
    assert!(
        jod_core::proc::group_alive(pid),
        "the run died with the process that started it"
    );

    // A brand-new handle on the file, in this process, which never held a pipe
    // to anything. This is what a `jod` started tomorrow would see.
    let store = Store::open(&fixture.db).expect("a second process can open the database");

    let events = fixture.wait_for_finish(&store);
    let kinds: Vec<&str> = events.iter().map(kind_of).collect();
    println!("events after the spawner died: {kinds:?}");

    assert!(
        kinds.contains(&"started"),
        "the session id never reached the store: {kinds:?}"
    );
    assert!(
        kinds.iter().filter(|k| **k == "message").count() >= 3,
        "output written after the spawner died was lost: {kinds:?}"
    );
    assert_eq!(kinds.last(), Some(&"finished"));

    let run = store.run(&fixture.run_id).unwrap().expect("the run row");
    assert_eq!(run.status, "completed", "a clean exit must read as completed");
    assert_eq!(run.pid, Some(pid));
    assert_eq!(run.pgid, Some(pid));
    assert_eq!(
        run.session_id.as_deref(),
        Some("sess-abc"),
        "the harness's conversation id must be recorded for `--resume`"
    );

    // Sequence numbers come from one writer, so they are dense and ordered.
    let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, (0..seqs.len() as u64).collect::<Vec<_>>());
}

/// The session id has to be on the *conversation*, written by the supervisor,
/// because there may be nobody else left to write it.
///
/// This is the bug that made a work's session unspeakable-to: no mail, no card
/// answer, no second turn, because `resume_for` had nothing to resume. It went
/// unnoticed because the only caller of `set_conversation_session` was a drain
/// task inside whatever process launched the run — and in every test and every
/// interactive `jod run`, that process is still there. On the path that opens a
/// work it is Jod's own MCP server, which exits when the harness closes stdin.
///
/// So the load-bearing clause is not the assertion, it is the fixture: the
/// launcher is **already gone** before anything below is read. A version of
/// this test that kept it alive would have passed against the broken build.
#[test]
fn the_session_id_reaches_the_conversation_even_though_the_launcher_is_gone() {
    let fixture = Fixture::new("session-id", CHATTY_HARNESS, 0);

    // What `spawn_agent` arranges before a harness starts: a conversation, and
    // the prompt recorded against this run. `conversation_for_run` finds the
    // conversation through that message, so without it the supervisor has
    // nothing to attach the session to.
    let store = Store::open(&fixture.db).unwrap();
    let conversation = store
        .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
        .unwrap()
        .id;
    store
        .append_message(
            &conversation,
            jod_core::conversation::NewMessage {
                run_id: Some(fixture.run_id.clone()),
                ..jod_core::conversation::NewMessage::new(
                    jod_core::conversation::Role::User,
                    "say three things",
                )
            },
        )
        .unwrap();
    assert_eq!(
        store.conversation(&conversation).unwrap().unwrap().session_id,
        None,
        "the fixture must start with nothing recorded, or this proves nothing"
    );
    drop(store);

    fixture.launch_from_a_process_that_exits();

    // A fresh handle, in a process that never held a pipe to anything — the
    // same view a `jod` started tomorrow would get.
    let store = Store::open(&fixture.db).unwrap();
    fixture.wait_for_finish(&store);

    assert_eq!(
        store.run(&fixture.run_id).unwrap().unwrap().session_id.as_deref(),
        Some("sess-abc"),
        "the run row lost the harness's session id"
    );
    assert_eq!(
        store
            .conversation(&conversation)
            .unwrap()
            .unwrap()
            .session_id
            .as_deref(),
        Some("sess-abc"),
        "the conversation cannot be resumed: nothing recorded the session id, \
         because the process that used to do it had already exited"
    );
    // The consequence, stated as the thing a caller actually asks for.
    assert!(
        matches!(
            store
                .resume_for(&conversation, jod_core::harness::HarnessKind::ClaudeCode)
                .unwrap(),
            jod_core::harness::Resume::Session(id) if id == "sess-abc"
        ),
        "resume_for still cannot resume this conversation"
    );
}

/// What the run said has to reach the transcript, written by the supervisor,
/// for exactly the same reason the session id does: there may be nobody left.
///
/// The bug: `messages` is the table `jod main` prints, and the only thing that
/// ever wrote a run's turns into it was `record_in_conversation`, a task inside
/// whatever process launched the run. `jod main` without `--wait` returns as
/// soon as the instruction is handed over, and a run opened through `open_work`
/// is launched by Jod's own MCP server, which exits with its harness. Both
/// leave the run talking to nobody. The `events` table stays complete, because
/// the supervisor writes that one — so `jod watch` replays the whole turn while
/// `jod main` shows it stopping in the middle, and the answer that was said and
/// paid for is simply not there.
///
/// The load-bearing clause is the fixture again: the launcher is **already
/// gone**, and the harness below sleeps before it says anything, so every word
/// it speaks is spoken after the only previous writer has exited.
///
/// It is a whole turn rather than a final reply on purpose. The loss was never
/// specific to how a turn ends — a tool call in the middle of one went missing
/// just as readily, and a transcript that skips the tool call but keeps the
/// answer is its own kind of wrong.
#[test]
fn what_the_run_said_reaches_the_transcript_even_though_the_launcher_is_gone() {
    let fixture = Fixture::new("transcript", A_WHOLE_TURN_HARNESS, 0);

    // What `spawn_agent_in` arranges before a harness starts: a conversation,
    // and the question this run was launched to answer recorded against it.
    // `conversation_for_run` finds the conversation through that row.
    let store = Store::open(&fixture.db).unwrap();
    let conversation = store
        .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
        .unwrap()
        .id;
    store
        .append_prompt(
            &conversation,
            &fixture.run_id,
            "every day at 9am, remind me to check the open issues",
        )
        .unwrap()
        .expect("the prompt must be recorded, or this proves nothing");
    drop(store);

    fixture.launch_from_a_process_that_exits();

    // A fresh handle, in a process that never held a pipe to anything — the
    // same view `jod main` gets when it prints the chat.
    let store = Store::open(&fixture.db).unwrap();
    let events = fixture.wait_for_finish(&store);
    let thread = store.thread(&conversation).unwrap();

    let roles: Vec<jod_core::conversation::Role> = thread.iter().map(|m| m.role).collect();
    use jod_core::conversation::Role;
    assert_eq!(
        roles,
        vec![
            Role::User,
            Role::ToolCall,
            Role::ToolResult,
            Role::Assistant
        ],
        "the transcript lost part of the turn. Every event is in the database, \
         so `jod watch` replays the whole thing, but `jod main` reads \
         `messages` and shows the turn stopping partway through: {thread:?}"
    );
    assert_eq!(
        thread.last().unwrap().text,
        "Armed as scratch-issues-daily, 9am daily.",
        "the reply the run was paid for is not in the transcript"
    );

    // Said once, not twice. The supervisor and any still-live launcher both
    // append, and `(run_id, run_seq)` is what keeps that from doubling a reply.
    //
    // Counted against the events rather than against a literal, so this stays
    // true if the fixture harness ever grows another line: every event
    // `NewMessage::from_event` is defined to project must appear exactly once.
    let projected = events
        .iter()
        .filter(|e| jod_core::conversation::NewMessage::from_event(&e.event).is_some())
        .count();
    assert_eq!(
        thread
            .iter()
            .filter(|m| m.run_id.as_deref() == Some(fixture.run_id.as_str()))
            .count(),
        projected + 1, // the run's own turns, plus the prompt nothing emits
        "every projected event must be recorded exactly once: {thread:?}"
    );
}

/// A session the harness has lost must stop being resumed — durably, and by
/// the supervisor, because there may be nobody else left to notice.
///
/// The bug, observed on the developer's own machine: a conversation held a
/// session id Claude Code no longer had. Every turn was launched
/// `--resume <that id>`, and every turn was refused in under a second, having
/// done nothing and spent nothing. Three in a row in the run table, identical.
/// Nothing anywhere cleared the pointer, so the thread could not take another
/// turn — not that day, not ever. A conversation is not supposed to be
/// reachable only until its harness forgets it.
///
/// *Why* the harness forgot is deliberately not claimed here: the session file
/// was gone from `~/.claude/projects` along with a whole sibling project
/// directory, and nothing in Jod deletes either. The repair does not depend on
/// knowing — a pointer the harness has disowned is dead however it died.
///
/// The load-bearing clause is the fixture, as in the test above: the launcher
/// is **already gone**. Recovery that needs the console to still be open is
/// not recovery, because the console is exactly what a person closes and
/// reopens before typing the turn that fails.
#[test]
fn a_session_the_harness_has_lost_stops_being_resumed() {
    let fixture = Fixture::new("lost-session", LOST_SESSION_HARNESS, 1);

    let store = Store::open(&fixture.db).unwrap();
    let conversation = store
        .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
        .unwrap()
        .id;
    store
        .append_message(
            &conversation,
            jod_core::conversation::NewMessage {
                run_id: Some(fixture.run_id.clone()),
                ..jod_core::conversation::NewMessage::new(
                    jod_core::conversation::Role::User,
                    "carry on",
                )
            },
        )
        .unwrap();
    // The state the bug leaves behind: a pointer to a session that is gone.
    store
        .set_conversation_session(&conversation, Some("sess-gone"))
        .unwrap();
    drop(store);

    fixture.launch_from_a_process_that_exits();

    let store = Store::open(&fixture.db).unwrap();
    let events = fixture.wait_for_finish(&store);
    let kinds: Vec<&str> = events.iter().map(kind_of).collect();
    assert!(
        kinds.contains(&"session_lost"),
        "the refusal was not recognised, so nothing could act on it: {kinds:?}"
    );

    assert_eq!(
        store
            .conversation(&conversation)
            .unwrap()
            .unwrap()
            .session_id,
        None,
        "the dead session id is still on the conversation, so the next turn \
         resumes it and fails in exactly the same way"
    );
    // Stated as the thing a caller actually asks, which is what decides how
    // the next turn is launched.
    assert_eq!(
        store
            .resume_for(&conversation, HarnessKind::ClaudeCode)
            .unwrap(),
        jod_core::harness::Resume::Fresh,
        "resume_for still hands out the dead session"
    );
}

/// Repair is not licence to clear whatever is there. A refusal naming a
/// session the conversation has already moved off — a turn that raced a
/// handoff, a straggling supervisor reporting late — must leave the live
/// pointer alone.
///
/// Without the guard the failure is worse than the one being fixed: the thread
/// is on a perfectly good session, and a stale message drops it.
#[test]
fn a_refusal_naming_some_other_session_leaves_a_live_one_alone() {
    let fixture = Fixture::new("lost-other", LOST_SESSION_HARNESS, 1);

    let store = Store::open(&fixture.db).unwrap();
    let conversation = store
        .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
        .unwrap()
        .id;
    store
        .append_message(
            &conversation,
            jod_core::conversation::NewMessage {
                run_id: Some(fixture.run_id.clone()),
                ..jod_core::conversation::NewMessage::new(
                    jod_core::conversation::Role::User,
                    "carry on",
                )
            },
        )
        .unwrap();
    // The harness above refuses `sess-gone`; this thread has since moved on.
    store
        .set_conversation_session(&conversation, Some("sess-current"))
        .unwrap();
    drop(store);

    fixture.launch_from_a_process_that_exits();

    let store = Store::open(&fixture.db).unwrap();
    fixture.wait_for_finish(&store);

    assert_eq!(
        store
            .conversation(&conversation)
            .unwrap()
            .unwrap()
            .session_id
            .as_deref(),
        Some("sess-current"),
        "a refusal about a different session dropped this thread's live one"
    );
}

#[test]
fn a_failing_harness_is_recorded_as_failed_not_quietly_finished() {
    let fixture = Fixture::new("failing", CHATTY_HARNESS, 3);
    fixture.launch_from_a_process_that_exits();
    let store = Store::open(&fixture.db).unwrap();

    let events = fixture.wait_for_finish(&store);
    let last = events.last().expect("a terminal event");
    match &last.event {
        AgentEvent::Finished {
            exit_code,
            is_error,
            ..
        } => {
            assert_eq!(*exit_code, Some(3), "the real exit code must be reported");
            assert!(is_error, "a non-zero exit is a failure, not a quiet finish");
        }
        other => panic!("expected Finished, got {other:?}"),
    }
    assert_eq!(store.run(&fixture.run_id).unwrap().unwrap().status, "failed");
}

#[test]
fn a_third_party_process_can_stop_a_run_it_never_started() {
    // The kill switch tmux used to provide, now provided by an integer in a
    // column: this process holds no handle on the run at all.
    let fixture = Fixture::new("killed", SLOW_HARNESS, 0);
    let pid = fixture.launch_from_a_process_that_exits();
    let store = Store::open(&fixture.db).unwrap();

    // Wait until it is genuinely under way, so this is a kill and not a race
    // against startup.
    fixture.wait_until(|| {
        store
            .events(&fixture.run_id)
            .map(|e| !e.is_empty())
            .unwrap_or(false)
    });

    jod_core::proc::signal_group(pid, jod_core::proc::SIGTERM).expect("signalling the group");

    let events = fixture.wait_for_finish(&store);
    assert_eq!(kind_of(events.last().unwrap()), "finished");
    assert_eq!(
        store.run(&fixture.run_id).unwrap().unwrap().status,
        "killed",
        "a killed run must not be reported as a failure or a success"
    );
    fixture.wait_until(|| !jod_core::proc::group_alive(pid));
}

#[test]
fn a_harness_that_cannot_start_ends_the_run_instead_of_hanging() {
    let mut fixture = Fixture::new("missing", CHATTY_HARNESS, 0);
    fixture.set_program(PathBuf::from("/definitely/not/a/binary"));
    fixture.launch_from_a_process_that_exits();
    let store = Store::open(&fixture.db).unwrap();

    let events = fixture.wait_for_finish(&store);
    let kinds: Vec<&str> = events.iter().map(kind_of).collect();
    assert_eq!(
        kinds,
        vec!["error", "finished"],
        "a spawn failure must be a finished run, not a missing one: {kinds:?}"
    );
    assert_eq!(store.run(&fixture.run_id).unwrap().unwrap().status, "failed");
}

// ---- the fixture ------------------------------------------------------

/// Emits Claude Code's stream-json, slowly enough that most of it is written
/// long after the spawner has exited.
const CHATTY_HARNESS: &str = r#"
echo '{"type":"system","subtype":"init","session_id":"sess-abc","model":"test-model"}'
sleep 0.4
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"one"}]}}'
sleep 0.2
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"two"}]}}'
sleep 0.2
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"three"}]}}'
echo '{"type":"result","result":"all done","is_error":false}'
exit $JOD_TEST_EXIT
"#;

/// A whole ordinary turn: a tool call, its result, and the reply that follows.
///
/// The sleep in front is the point. Everything below it is said after the
/// process that launched the run has exited, which is the state every `jod
/// main` without `--wait` and every `open_work` session is in.
const A_WHOLE_TURN_HARNESS: &str = r#"
echo '{"type":"system","subtype":"init","session_id":"sess-abc","model":"test-model"}'
sleep 0.4
echo '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tu_1","name":"ToolSearch","input":{"query":"select:schedule_create"}}]}}'
echo '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu_1","content":"found it","is_error":false}]}}'
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"Armed as scratch-issues-daily, 9am daily."}]}}'
echo '{"type":"result","result":"Armed as scratch-issues-daily, 9am daily.","is_error":false}'
exit $JOD_TEST_EXIT
"#;

/// Starts talking, then keeps running until something stops it.
const SLOW_HARNESS: &str = r#"
echo '{"type":"system","subtype":"init","session_id":"sess-abc","model":"test-model"}'
sleep 120
"#;

/// Claude Code refusing `--resume` for a session it no longer holds: one line
/// on stderr, exit 1, and nothing else on the wire — no `init`, so no
/// `Started`, so nothing that would otherwise record a session id. Copied from
/// a real failure rather than invented.
const LOST_SESSION_HARNESS: &str = r#"
echo 'No conversation found with session ID: sess-gone' >&2
exit $JOD_TEST_EXIT
"#;

struct Fixture {
    dir: PathBuf,
    db: PathBuf,
    run_id: String,
    plan: SpawnPlan,
}

impl Fixture {
    fn new(tag: &str, harness_body: &str, exit_code: i32) -> Fixture {
        let dir = std::env::temp_dir().join(format!(
            "jod-supervise-{tag}-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let harness = dir.join("fake-harness.sh");
        std::fs::write(
            &harness,
            format!("#!/usr/bin/env bash\nJOD_TEST_EXIT={exit_code}\n{harness_body}"),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&harness, std::fs::Permissions::from_mode(0o755)).unwrap();

        let db = dir.join("jod.db");
        let run_id = format!("run-{tag}");
        let plan = SpawnPlan {
            run_id: run_id.clone(),
            harness: HarnessKind::ClaudeCode,
            db_path: db.clone(),
            program: harness,
            args: vec![],
            cwd: dir.clone(),
            env: Vec::new(),
            secrets: Vec::new(),
        };

        // The row has to exist before the supervisor updates it, exactly as
        // `spawn_agent` arranges in the real path.
        let store = Store::open(&db).unwrap();
        store
            .save_run(&jod_core::store::StoredRun {
                id: run_id.clone(),
                name: tag.into(),
                harness: HarnessKind::ClaudeCode.id().into(),
                status: "running".into(),
                cwd: dir.to_string_lossy().into(),
                session_id: None,
                pid: None,
                pgid: None,
                created_at_ms: 0,
                summary: serde_json::json!({}),
            })
            .unwrap();
        drop(store);

        Fixture {
            dir,
            db,
            run_id,
            plan,
        }
    }

    fn set_program(&mut self, program: PathBuf) {
        self.plan.program = program;
    }

    /// Start the run from a process that then exits, and return its pgid.
    fn launch_from_a_process_that_exits(&self) -> u32 {
        let plan_path = self.dir.join("spawn.json");
        std::fs::write(&plan_path, serde_json::to_vec_pretty(&self.plan).unwrap()).unwrap();
        let pid_file = self.dir.join("pid");

        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "spawner_child", "--ignored", "--nocapture"])
            .env(PLAN_ENV, &plan_path)
            .env(PID_ENV, &pid_file)
            .status()
            .expect("re-invoking this test binary as the spawner");
        assert!(status.success(), "the spawner failed: {status}");
        // `status()` reaped it: the process that started the run is now gone.

        std::fs::read_to_string(&pid_file)
            .expect("the spawner must report the pid")
            .trim()
            .parse()
            .expect("a numeric pid")
    }

    fn wait_until(&self, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if done() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "timed out; supervisor.log:\n{}",
            std::fs::read_to_string(self.dir.join("supervisor.log")).unwrap_or_default()
        );
    }

    fn wait_for_finish(&self, store: &Store) -> Vec<jod_core::event::AgentEnvelope> {
        self.wait_until(|| {
            store
                .events(&self.run_id)
                .map(|events| events.iter().any(|e| kind_of(e) == "finished"))
                .unwrap_or(false)
        });
        // The status is written after the final event, so give it its moment
        // rather than racing the supervisor's last statement.
        self.wait_until(|| {
            store
                .run(&self.run_id)
                .ok()
                .flatten()
                .is_some_and(|r| r.status != "running")
        });
        store.events(&self.run_id).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn kind_of(e: &jod_core::event::AgentEnvelope) -> &'static str {
    match e.event {
        AgentEvent::Started { .. } => "started",
        AgentEvent::Thinking { .. } => "thinking",
        AgentEvent::Progress { .. } => "progress",
        AgentEvent::Delta { .. } => "delta",
        AgentEvent::Message { .. } => "message",
        AgentEvent::ToolCall { .. } => "tool_call",
        AgentEvent::ToolResult { .. } => "tool_result",
        AgentEvent::Finished { .. } => "finished",
        AgentEvent::Raw { .. } => "raw",
        AgentEvent::SessionLost { .. } => "session_lost",
        AgentEvent::Error { .. } => "error",
    }
}

fn supervisor_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_jod-run"))
}
