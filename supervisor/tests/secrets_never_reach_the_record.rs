//! The promise D3 makes, checked end to end: a run told to print a secret
//! prints the redaction marker, and the value appears nowhere in the database.
//!
//! Unit tests cover the two halves separately — the store keeps values out of
//! SQLite, the scrubber replaces them in a string. Neither can show that the
//! halves meet. This drives a real supervisor against a real child process that
//! really reads the variable out of its environment and really prints it, then
//! goes looking for the value in every byte Jod wrote.
//!
//! The harness is a shell script rather than `claude`, for the same reason as
//! in `survives_its_parent.rs`: what is under test is the transport, not any
//! particular agent. The secret is a token this file generates at run time — it
//! authenticates nothing anywhere, and it exists so that finding it in a file
//! is unambiguous evidence of a leak.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use jod_core::event::AgentEnvelope;
use jod_core::harness::HarnessKind;
use jod_core::runner::SpawnPlan;
use jod_core::secrets::Scope;
use jod_core::store::Store;

/// Tests here set `JOD_HOME`, which is process-wide. Two running at once would
/// send one test's secret file to the other test's directory.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Points `JOD_HOME` at a temporary directory for as long as it lives, holding
/// [`ENV_LOCK`] throughout. Unsets on drop even when the test panics, so a
/// failure cannot leave a later test writing into a directory that is gone.
struct HomeEnv(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

impl HomeEnv {
    fn pointing_at(dir: &std::path::Path) -> HomeEnv {
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("JOD_HOME", dir);
        HomeEnv(guard)
    }
}

impl Drop for HomeEnv {
    fn drop(&mut self) {
        std::env::remove_var("JOD_HOME");
    }
}

/// Reads the injected variable, refuses to run without it, and then prints it
/// on both streams — the worst thing a real agent could do with a credential,
/// which is exactly the case redaction exists for.
const LEAKY_HARNESS: &str = r#"
if [ -z "$TEST_SECRET" ]; then
  echo '{"type":"result","result":"the secret never reached the child","is_error":true}'
  exit 9
fi
echo '{"type":"system","subtype":"init","session_id":"sess-secret","model":"test-model"}'
echo "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"the key is $TEST_SECRET\"}]}}"
echo "and on stderr as prose: $TEST_SECRET" >&2
echo "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"plain env says $JOD_TEST_PLAIN\"}]}}"
echo '{"type":"result","result":"done","is_error":false}'
"#;

#[test]
fn a_run_that_prints_its_secret_records_the_marker_and_leaks_nothing() {
    let home = TempHome::new("redaction");
    let _env = HomeEnv::pointing_at(&home.dir);

    // Generated here, and a credential for nothing. Anything unique would do;
    // what matters is that a byte-for-byte match somewhere on disk can only
    // have come from this run.
    let token = format!(
        "jod-fake-token-{}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    );

    let store = Store::open(&home.db()).unwrap();
    store
        .put_secret(
            "TEST_SECRET",
            Scope::Global,
            "",
            &token,
            "a token this test made up",
        )
        .unwrap();

    let plan = home.plan(
        "run-redaction",
        LEAKY_HARNESS,
        vec![("JOD_TEST_PLAIN".into(), "plain-value-not-secret".into())],
        vec!["TEST_SECRET".into()],
    );
    home.save_run(&store, &plan);
    let events = home.run_to_completion(&plan, &store);

    let transcript = serde_json::to_string(&events).unwrap();
    println!("transcript: {transcript}");

    // The child had the value: it refuses to produce this at all without it.
    assert_eq!(
        store.run(&plan.run_id).unwrap().unwrap().status,
        "completed",
        "the harness exits 9 when the variable is missing, so this is the \
         injection half of the check: {transcript}"
    );
    assert!(
        transcript.contains("plain-value-not-secret"),
        "the non-secret environment pair did not arrive, so the run above \
         proves less than it looks: {transcript}"
    );

    // ...and printing it got the marker, on both streams.
    assert!(
        transcript.matches(jod_core::redact::MARKER).count() >= 2,
        "stdout and stderr must both be scrubbed: {transcript}"
    );
    assert!(
        !transcript.contains(&token),
        "the value reached the event stream"
    );

    // The check the spec actually names: not one byte of it anywhere under
    // `JOD_HOME` except the owner-only file it was stored in.
    let leaked = home.files_containing(&token);
    let is_the_secret_file = |p: &PathBuf| {
        p.parent() == Some(jod_core::paths::secrets_dir().as_path())
            && p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("TEST_SECRET."))
    };
    assert!(
        leaked.len() == 1 && is_the_secret_file(&leaked[0]),
        "the value is in files it must never be in: {leaked:?}"
    );
}

#[test]
fn a_secret_that_cannot_be_resolved_is_a_notice_rather_than_the_end_of_the_run() {
    // A missing key blocks one test, not a session. The run has to reach the
    // agent so the agent can end *blocked* — killing it here would throw away
    // everything else it was asked to do, and the reason would be visible only
    // in a log nobody reads.
    let home = TempHome::new("missing");
    let store = Store::open(&home.db()).unwrap();

    let plan = home.plan(
        "run-missing",
        LEAKY_HARNESS,
        vec![("TEST_SECRET".into(), "not-actually-a-secret".into())],
        vec!["NEVER_STORED".into()],
    );
    home.save_run(&store, &plan);
    let events = home.run_to_completion(&plan, &store);
    let transcript = serde_json::to_string(&events).unwrap();

    assert!(
        transcript.contains("NEVER_STORED") && transcript.contains("continues without it"),
        "the run must say plainly which secret was not injected: {transcript}"
    );
    assert_eq!(
        store.run(&plan.run_id).unwrap().unwrap().status,
        "completed",
        "an unresolvable name must not kill the run: {transcript}"
    );
}

// ---- the fixture ------------------------------------------------------

struct TempHome {
    dir: PathBuf,
}

impl TempHome {
    fn new(tag: &str) -> TempHome {
        let dir = std::env::temp_dir().join(format!(
            "jod-secret-e2e-{tag}-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        TempHome { dir }
    }

    fn db(&self) -> PathBuf {
        self.dir.join("jod.db")
    }

    fn plan(
        &self,
        run_id: &str,
        harness_body: &str,
        env: Vec<(String, String)>,
        secrets: Vec<String>,
    ) -> SpawnPlan {
        let program = self.dir.join(format!("{run_id}-harness.sh"));
        std::fs::write(&program, format!("#!/usr/bin/env bash\n{harness_body}")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();

        SpawnPlan {
            run_id: run_id.to_string(),
            harness: HarnessKind::ClaudeCode,
            db_path: self.db(),
            program,
            args: vec![],
            cwd: self.dir.clone(),
            env,
            secrets,
        }
    }

    /// The row the supervisor updates has to exist first, exactly as
    /// `spawn_agent` arranges on the real path.
    fn save_run(&self, store: &Store, plan: &SpawnPlan) {
        store
            .save_run(&jod_core::store::StoredRun {
                id: plan.run_id.clone(),
                name: plan.run_id.clone(),
                harness: HarnessKind::ClaudeCode.id().into(),
                status: "running".into(),
                cwd: self.dir.to_string_lossy().into(),
                session_id: None,
                pid: None,
                pgid: None,
                created_at_ms: 0,
                summary: serde_json::json!({}),
            })
            .unwrap();
    }

    fn run_to_completion(&self, plan: &SpawnPlan, store: &Store) -> Vec<AgentEnvelope> {
        let plan_path = self.dir.join(format!("{}-spawn.json", plan.run_id));
        std::fs::write(&plan_path, serde_json::to_vec_pretty(plan).unwrap()).unwrap();

        let status = std::process::Command::new(PathBuf::from(env!("CARGO_BIN_EXE_jod-run")))
            .arg(&plan_path)
            .current_dir(&self.dir)
            .status()
            .expect("the supervisor must start");
        assert!(status.success(), "the supervisor failed: {status}");

        // The supervisor writes the final status after its last event, and it
        // has exited by now, so this settles immediately or never.
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let settled = store
                .run(&plan.run_id)
                .ok()
                .flatten()
                .is_some_and(|r| r.status != "running");
            if settled {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        store.events(&plan.run_id).unwrap()
    }

    /// Every file under this home whose bytes contain `needle`.
    ///
    /// Bytes rather than lines, and every file rather than the database alone:
    /// SQLite keeps recently written pages in `jod.db-wal`, and a value sitting
    /// in a write-ahead log is as leaked as one in a table.
    fn files_containing(&self, needle: &str) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut stack = vec![self.dir.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let bytes = std::fs::read(&path).unwrap_or_default();
                if bytes.windows(needle.len()).any(|w| w == needle.as_bytes()) {
                    found.push(path);
                }
            }
        }
        found.sort();
        found
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}
