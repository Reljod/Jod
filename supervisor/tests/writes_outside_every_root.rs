//! A run that succeeds and lands nowhere anybody asked for.
//!
//! The failure this pins was found by hand, not by a test, and the reason no
//! test had it is that every part of it worked. The harness exited 0, the
//! supervisor recorded `completed`, the fleet drew a green check against real
//! money spent — and the whole project was written to a directory nobody had
//! named, while the one the user added stayed empty. Nothing in the system was
//! wrong except the one thing nothing looked at.
//!
//! So this drives the real `jod-run` binary against a real database and asserts
//! what a person would see afterwards: the run is still `completed`, because it
//! is, **and** there is a card in the rail saying where the work actually went.
//! A unit test on the comparison function would have passed against the broken
//! build, because the comparison was never called.
//!
//! The harness is a shell script printing Claude Code's stream-json — and one
//! that really creates the files it reports, so the paths being compared are
//! paths that exist and can differ in their spelling, which is where a symlink
//! under `/var` would otherwise sink the whole check.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use jod_core::cards::Query;
use jod_core::harness::HarnessKind;
use jod_core::runner::SpawnPlan;
use jod_core::store::Store;

/// BUG-14: the run reports success, and every file it wrote is outside every
/// directory the session declared.
#[test]
fn a_run_that_wrote_outside_every_directory_it_was_given_does_not_pass_silently() {
    let world = World::new("strayed");
    // What the user pointed at, and what they got. The declared directory is
    // real, empty and untouched, exactly as it was found.
    let declared = world.dir.join("dogfood").join("tetris");
    let elsewhere = world.dir.join("home").join("tetris");
    std::fs::create_dir_all(&declared).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();

    let run = world.launch(&declared, &writes_to(&[&elsewhere.join("index.html")]));

    let store = Store::open(&world.db).unwrap();
    run.wait_for_status(&store);

    let row = store.run(&run.id).unwrap().expect("the run row");
    assert_eq!(
        row.status, "completed",
        "the run really did complete; relabelling its exit code would be a lie"
    );
    assert!(
        std::fs::read_dir(&declared).unwrap().next().is_none(),
        "the directory the user pointed at is untouched — that is the bug"
    );

    let cards = store
        .cards(&Query {
            conversation_id: Some(run.conversation.clone()),
            ..Default::default()
        })
        .unwrap();
    let card = cards.first().unwrap_or_else(|| {
        panic!(
            "a run that finished having written nothing where it was pointed \
             must say so; the rail is empty and the fleet shows a green check"
        )
    });
    assert!(
        card.body.contains(&elsewhere.display().to_string()),
        "the card has to name where the work actually went: {}",
        card.body
    );
    assert!(
        card.body.contains(&declared.display().to_string()),
        "…and where it was supposed to go: {}",
        card.body
    );
}

/// The other half, and the one that decides whether anybody keeps reading these
/// cards: a run that worked where it was told writes no card, however many
/// scratch files it left elsewhere.
#[test]
fn a_run_that_worked_where_it_was_pointed_raises_nothing() {
    let world = World::new("landed");
    let declared = world.dir.join("dogfood").join("tetris");
    std::fs::create_dir_all(&declared).unwrap();

    let run = world.launch(
        &declared,
        &writes_to(&[
            &declared.join("index.html"),
            &world.dir.join("tmp-notes.txt"),
        ]),
    );

    let store = Store::open(&world.db).unwrap();
    run.wait_for_status(&store);

    assert_eq!(store.run(&run.id).unwrap().unwrap().status, "completed");
    let cards = store
        .cards(&Query {
            conversation_id: Some(run.conversation.clone()),
            ..Default::default()
        })
        .unwrap();
    assert!(
        cards.is_empty(),
        "a run that landed where it was pointed is not worth a card about its \
         scratch files: {cards:?}"
    );
}

/// A harness that really writes each file and reports it the way Claude Code
/// does, then finishes cleanly.
fn writes_to(paths: &[&Path]) -> String {
    let mut script =
        String::from("echo '{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"sess-w\",\"model\":\"test-model\"}'\n");
    for (n, path) in paths.iter().enumerate() {
        script.push_str(&format!(
            "mkdir -p \"$(dirname '{p}')\"\nprintf 'hello' > '{p}'\n\
             echo '{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"tool_use\",\
             \"id\":\"t{n}\",\"name\":\"Write\",\"input\":{{\"file_path\":\"{p}\",\
             \"content\":\"hello\"}}}}]}}}}'\n",
            p = path.display(),
        ));
    }
    script.push_str("echo '{\"type\":\"result\",\"result\":\"done\",\"is_error\":false}'\nexit 0\n");
    script
}

// ---- the world --------------------------------------------------------

struct World {
    dir: PathBuf,
    db: PathBuf,
}

struct Run {
    id: String,
    conversation: String,
    dir: PathBuf,
}

impl World {
    fn new(tag: &str) -> World {
        let dir = std::env::temp_dir().join(format!(
            "jod-strays-{tag}-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("jod.db");
        World { dir, db }
    }

    /// Set the run up exactly as `Jod::spawn_agent_in` does — a run row, a
    /// conversation, the prompt that binds them, and the directory the user
    /// added as a root — then start the real supervisor on it.
    fn launch(&self, declared_root: &Path, harness_body: &str) -> Run {
        let harness = self.dir.join("fake-harness.sh");
        std::fs::write(&harness, format!("#!/usr/bin/env bash\n{harness_body}")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&harness, std::fs::Permissions::from_mode(0o755)).unwrap();

        // The working directory the run is launched with: where the console was
        // opened, not the directory the user named in the prompt.
        let cwd = self.dir.join("console");
        std::fs::create_dir_all(&cwd).unwrap();

        // One run per world, and a world is one database in one fresh
        // directory, so a fixed id cannot collide with anything.
        let run_id = "run-under-test".to_string();
        let store = Store::open(&self.db).unwrap();
        store
            .save_run(&jod_core::store::StoredRun {
                id: run_id.clone(),
                name: "build a tetris game in the tetris directory".into(),
                harness: HarnessKind::ClaudeCode.id().into(),
                status: "running".into(),
                cwd: cwd.to_string_lossy().into(),
                session_id: None,
                pid: None,
                pgid: None,
                created_at_ms: 0,
                summary: serde_json::json!({}),
            })
            .unwrap();
        let conversation = store
            .new_conversation(HarnessKind::ClaudeCode, &cwd.to_string_lossy(), None)
            .unwrap();
        store
            .append_prompt(
                &conversation.id,
                &run_id,
                "build a tetris game in the tetris directory",
            )
            .unwrap();
        // `/add-dir`, through the same call the picker makes.
        store
            .add_root(
                &conversation.id,
                jod_core::roots::NewRoot::reading(declared_root),
            )
            .unwrap();
        drop(store);

        let run_dir = self.dir.join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let plan = SpawnPlan {
            run_id: run_id.clone(),
            harness: HarnessKind::ClaudeCode,
            db_path: self.db.clone(),
            program: harness,
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
            conversation: conversation.id,
            dir: run_dir,
        }
    }
}

impl Run {
    /// Wait for the terminal status, which the supervisor writes after its last
    /// event — and after the check this file is about.
    fn wait_for_status(&self, store: &Store) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if store
                .run(&self.id)
                .ok()
                .flatten()
                .is_some_and(|r| r.status != "running")
            {
                // The card is raised after the status, so give that its moment
                // rather than racing the supervisor's last statement.
                std::thread::sleep(Duration::from_millis(200));
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "timed out; supervisor.log:\n{}",
            std::fs::read_to_string(self.dir.join("supervisor.log")).unwrap_or_default()
        );
    }
}

impl Drop for World {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}
