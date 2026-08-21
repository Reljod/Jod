//! A run that died because its harness was never signed in.
//!
//! The failure this pins was found by hand. A prompt went out to Claude Code,
//! the harness printed one sentence — `Failed to authenticate: OAuth session
//! expired and could not be refreshed` — and exited within a second, and the
//! whole of what the console showed was that sentence and `✗ failed · $0.0000
//! · 1s`. Everything worked. The binary was found, the process started, the
//! output was recorded, the run was marked failed. Nothing said what to do,
//! and `jod harnesses` went on calling the harness usable because a file was
//! on disk.
//!
//! So this drives the real `jod-run` binary against a real database with a
//! harness that fails the way that one did, and asserts that the transcript
//! carries an instruction rather than only a symptom.
//!
//! The negative half matters as much: a run that failed for any other reason
//! must not be told to sign in. It is asserted through AGY, which has no
//! sign-in command and no way of being asked about one — so the answer is the
//! same on a machine with credentials and on a machine without, and this test
//! cannot start passing or failing because of how the box it runs on is set up.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use jod_core::harness::HarnessKind;
use jod_core::runner::SpawnPlan;
use jod_core::store::Store;
use jod_core::AgentEvent;

/// The line the harness really printed, on 2026-08-21.
const OBSERVED: &str = "Failed to authenticate: OAuth session expired and could not be refreshed";

#[test]
fn a_run_that_could_not_authenticate_says_how_to_fix_it() {
    let world = World::new("signed-out");
    let run = world.launch(
        HarnessKind::ClaudeCode,
        &format!("echo '{OBSERVED}' >&2\nexit 1\n"),
    );

    let store = Store::open(&world.db).unwrap();
    run.wait_for_status(&store);

    assert_eq!(
        store.run(&run.id).unwrap().unwrap().status,
        "failed",
        "the run really did fail; nothing here relabels that"
    );

    let advice = run.errors(&store).join("\n");
    assert!(
        advice.contains("jod login"),
        "a run that died unauthenticated has to name the one command that \
         fixes it. The transcript said only:\n{}",
        run.transcript(&store)
    );
    assert!(
        advice.contains("Claude Code"),
        "…and which harness it is about: {advice}"
    );
}

/// The other half, and the one that decides whether anybody keeps reading
/// these messages: an ordinary failure gets no sign-in suggestion stapled to
/// it.
#[test]
fn a_failure_that_is_not_about_credentials_suggests_nothing() {
    let world = World::new("ordinary");
    let run = world.launch(
        HarnessKind::Agy,
        "echo 'error: no such file or directory' >&2\nexit 2\n",
    );

    let store = Store::open(&world.db).unwrap();
    run.wait_for_status(&store);

    let errors = run.errors(&store).join("\n");
    assert!(
        !errors.contains("jod login"),
        "an unrelated failure must not send anybody to sign in: {errors}"
    );
}

// ---- the world --------------------------------------------------------

struct World {
    dir: PathBuf,
    db: PathBuf,
}

struct Run {
    id: String,
    dir: PathBuf,
}

impl World {
    fn new(tag: &str) -> World {
        let dir = std::env::temp_dir().join(format!(
            "jod-auth-{tag}-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("jod.db");
        World { dir, db }
    }

    /// A run row and a real supervisor over a harness that fails on purpose.
    fn launch(&self, harness: HarnessKind, harness_body: &str) -> Run {
        let script = self.dir.join("fake-harness.sh");
        std::fs::write(&script, format!("#!/usr/bin/env bash\n{harness_body}")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let cwd = self.dir.join("console");
        std::fs::create_dir_all(&cwd).unwrap();

        let run_id = "run-under-test".to_string();
        let store = Store::open(&self.db).unwrap();
        store
            .save_run(&jod_core::store::StoredRun {
                id: run_id.clone(),
                name: "hello".into(),
                harness: harness.id().into(),
                status: "running".into(),
                cwd: cwd.to_string_lossy().into(),
                session_id: None,
                pid: None,
                pgid: None,
                created_at_ms: 0,
                summary: serde_json::json!({}),
            })
            .unwrap();
        drop(store);

        let run_dir = self.dir.join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let plan = SpawnPlan {
            run_id: run_id.clone(),
            harness,
            db_path: self.db.clone(),
            program: script,
            args: vec![],
            cwd,
            env: Vec::new(),
            secrets: Vec::new(),
        };
        let plan_path = run_dir.join("spawn.json");
        std::fs::write(&plan_path, serde_json::to_vec_pretty(&plan).unwrap()).unwrap();

        jod_core::proc::spawn_detached(
            &PathBuf::from(env!("CARGO_BIN_EXE_jod-run")),
            &[plan_path.to_string_lossy().to_string()],
            &run_dir,
            &run_dir.join("supervisor.log"),
        )
        .expect("the supervisor must start");

        Run {
            id: run_id,
            dir: run_dir,
        }
    }
}

impl Run {
    fn wait_for_status(&self, store: &Store) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if store
                .run(&self.id)
                .ok()
                .flatten()
                .is_some_and(|r| r.status != "running")
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "timed out; supervisor.log:\n{}",
            std::fs::read_to_string(self.dir.join("supervisor.log")).unwrap_or_default()
        );
    }

    /// Everything the run reported as an error, in order.
    fn errors(&self, store: &Store) -> Vec<String> {
        store
            .events_since(&self.id, None, 10_000)
            .unwrap()
            .into_iter()
            .filter_map(|envelope| match envelope.event {
                AgentEvent::Error { message } => Some(message),
                _ => None,
            })
            .collect()
    }

    /// What a person would have had to read instead, for a failure message.
    fn transcript(&self, store: &Store) -> String {
        store
            .events_since(&self.id, None, 10_000)
            .unwrap()
            .into_iter()
            .map(|e| format!("{:?}", e.event))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Drop for World {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}
