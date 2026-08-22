//! `jod approve-hook` refuses to let a run that delegates sit and wait.
//!
//! This runs the real binary, because the thing under test is a *protocol*
//! between two programs. Claude Code writes a payload to this process's stdin
//! and reads a decision off its stdout, and a matcher that is right inside a
//! unit test but never reached — a flag that did not parse, a decision printed
//! to stderr — is a rule that does not exist. The unit tests in
//! `cli/src/approve.rs` cover which commands the matcher catches; this covers
//! that a caller invoking the command line Jod writes into the settings
//! document gets an answer back.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

/// Run the hook over one `Bash` command and return what it printed.
///
/// The flags are the ones `harness/claude.rs` writes for an orchestrating run
/// in `auto`: refuse waiting commands, and raise no approval cards, because
/// there is nobody to ask.
fn decide(command: &str) -> String {
    // Its own home, per call. Not the real `~/.jod`, which this must never
    // touch, and not one home shared by the whole test binary either — these
    // tests run in parallel threads, and two `jod` processes opening the same
    // fresh database at once is a locked database rather than a decision.
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let home = std::env::temp_dir().join(format!(
        "jod-approve-hook-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command, "description": "under test" },
    })
    .to_string();

    let mut child = Command::new(env!("CARGO_BIN_EXE_jod"))
        .args(["approve-hook", "--run", "run-under-test", "--wait", "1"])
        .args(["--refuse-waiting", "--never-ask"])
        .env("JOD_HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the built jod binary runs");
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(payload.as_bytes())
        .expect("the hook reads its payload from stdin");
    let out = child.wait_with_output().expect("the hook exits");
    let _ = std::fs::remove_dir_all(&home);
    // The exit code is never the answer — stdout is. A hook that crashed and a
    // hook that had nothing to say are both "let the harness decide". Stderr is
    // kept only so a broken test reads as broken rather than as an allow.
    assert!(
        out.status.success(),
        "the hook exited {}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("the decision is utf-8")
}

/// What the decision says, or `None` when the hook said nothing at all.
fn verdict(printed: &str) -> Option<(String, String)> {
    let line = printed.lines().find(|l| l.starts_with('{'))?;
    let doc: serde_json::Value = serde_json::from_str(line).expect("a decision is one JSON object");
    let out = &doc["hookSpecificOutput"];
    assert_eq!(out["hookEventName"], "PreToolUse");
    Some((
        out["permissionDecision"].as_str()?.to_string(),
        out["permissionDecisionReason"].as_str()?.to_string(),
    ))
}

/// The two commands actually recorded from one turn of main's, which spent
/// forty-two seconds and thirty-nine cents busy-waiting on a run it had just
/// delegated and produced no answer.
#[test]
fn the_commands_that_blocked_main_are_refused_and_told_why() {
    for command in [
        "sleep 45; echo waited",
        r#"until [ "$(jod agents --json | grep -c a2e4f620)" = "0" ]; do sleep 5; done"#,
        "while true; do jod agents; sleep 5; done",
    ] {
        let (decision, reason) =
            verdict(&decide(command)).unwrap_or_else(|| panic!("no decision for `{command}`"));
        assert_eq!(decision, "deny", "`{command}` was left able to poll");
        // Named, not merely refused. A model told only "denied" tries the same
        // idea through a different tool; a model told the answer is coming to
        // it returns and reads the answer.
        assert!(
            reason.contains("card") && reason.contains("return now"),
            "the refusal does not say why or what to do instead: {reason}"
        );
    }
}

/// An ordinary command goes through, including one that merely contains the
/// word. This is the direction that costs more to get wrong: an agent refused
/// `grep sleep` cannot do its job, and a rule that fires on that does not read
/// as a rule — it reads as Jod being broken.
///
/// Nothing printed is the allow. Saying nothing hands the call back to Claude
/// Code's own permission flow, which is where every call went before this hook
/// existed; `--never-ask` means this process has no second opinion to offer
/// about anything that is not a wait.
#[test]
fn an_ordinary_command_is_left_alone() {
    for command in ["ls", "grep -rn sleep core/src", "python sleep_test.py"] {
        let printed = decide(command);
        assert!(
            verdict(&printed).is_none(),
            "`{command}` was answered when it should have been passed through: {printed}"
        );
    }
}
