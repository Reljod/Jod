//! Typing into a chat that is already working, against a real database file.
//!
//! The unit tests in `delivery` cover the decisions, and they cover them by
//! handing `plan_injection` a `busy` flag. That is the right shape for testing
//! a judgement and it leaves one thing unasserted: **where `busy` comes from.**
//! It is a join from `messages` to `runs`, and a queue that plans perfectly
//! against a bool it was handed is still a queue that never speaks if that join
//! is wrong.
//!
//! So everything here goes through real rows — a real run marked running, a
//! real message tying it to a conversation — and closes the store between the
//! steps, because "it works while the program is running" is not the guarantee
//! a queue makes. A message typed into the console has to survive the console.

use jod_core::delivery::{Kind, Plan, State};
use jod_core::harness::HarnessKind;
use jod_core::store::{Store, StoredRun};

/// A private directory for one test, removed on the way out.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("jod-queue-e2e-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// Open the database as a fresh process would.
    fn open(&self) -> Store {
        Store::open(&self.0.join("jod.db")).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A run row and the message that ties it to a conversation, which together are
/// the whole of what `conversation_is_busy` reads.
fn turn_in_flight(store: &Store, conversation: &str, run_id: &str, asked: &str) {
    store
        .save_run(&StoredRun {
            id: run_id.into(),
            name: format!("main {run_id}"),
            harness: "claude_code".into(),
            status: "running".into(),
            cwd: "/tmp".into(),
            session_id: None,
            pid: None,
            pgid: None,
            created_at_ms: 0,
            summary: serde_json::Value::Null,
        })
        .unwrap();
    store.append_prompt(conversation, run_id, asked).unwrap();
}

/// The turn ends. Nothing else about the conversation changes.
fn turn_ends(store: &Store, run_id: &str) {
    let mut run = store.run(run_id).unwrap().expect("the run is there");
    run.status = "done".into();
    store.save_run(&run).unwrap();
}

/// The whole of what happens when Reljod types while main is working, in the
/// order it happens, across four processes.
#[test]
fn a_line_typed_mid_turn_is_judged_once_and_delivered_when_the_turn_ends() {
    let scratch = Scratch::new("held");

    // 1. A turn is in flight, and something is typed behind it.
    let conversation = {
        let store = scratch.open();
        let conversation = store.main_conversation(HarnessKind::ClaudeCode, "/tmp").unwrap();
        turn_in_flight(&store, &conversation, "run-main", "port the parser");
        assert!(
            store.conversation_is_busy(&conversation).unwrap(),
            "a running run with a message in this conversation is what busy means"
        );
        store
            .enqueue_delivery(&conversation, Kind::Human, "typed", "also update the README")
            .unwrap();
        conversation
    };

    // 2. A new process reads the queue and finds something nobody has judged.
    let items = {
        let store = scratch.open();
        let busy = store.conversation_is_busy(&conversation).unwrap();
        let Plan::Judge { items, .. } = store.plan_injection(&conversation, busy).unwrap() else {
            panic!("a message typed behind a running turn is judged");
        };
        assert_eq!(items.len(), 1);
        // Claimed, so a second process starts no second assistant.
        assert_eq!(store.claim_for_review(&[items[0].id]).unwrap(), 1);
        items
    };
    {
        let store = scratch.open();
        let busy = store.conversation_is_busy(&conversation).unwrap();
        assert_eq!(
            store.plan_injection(&conversation, busy).unwrap(),
            Plan::Hold,
            "a queue somebody is already reading is not one to read again"
        );
        assert_eq!(store.under_review_for(&conversation).unwrap().len(), 1);
    }

    // 3. The assistant holds. The message goes back to waiting, stamped, so no
    //    later tick pays to have it read a second time.
    {
        let store = scratch.open();
        store.finish_review(&[items[0].id]).unwrap();
        let waiting = store.pending_for(&conversation).unwrap();
        assert_eq!(waiting.len(), 1);
        assert!(waiting[0].reviewed_at_ms.is_some());
        let busy = store.conversation_is_busy(&conversation).unwrap();
        assert_eq!(store.plan_injection(&conversation, busy).unwrap(), Plan::Hold);
    }

    // 4. The turn ends, and the line goes in — through the join, with nobody
    //    passing a flag.
    {
        let store = scratch.open();
        turn_ends(&store, "run-main");
        assert!(!store.conversation_is_busy(&conversation).unwrap());
        let busy = store.conversation_is_busy(&conversation).unwrap();
        let injection = store
            .plan_injection(&conversation, busy)
            .unwrap()
            .speak()
            .expect("the turn is over, so the queue speaks");
        assert!(injection.prompt.contains("also update the README"));

        store
            .mark_deliveries_delivered(&[items[0].id], Some("run-next"))
            .unwrap();
        assert!(store.pending_for(&conversation).unwrap().is_empty());
    }
}

/// The other verdict, which is the one that costs something if it is wrong.
///
/// Stopping the run is all `interrupt_main` does — it delivers nothing. What
/// makes that sufficient is asserted here: the conversation is no longer busy,
/// so the very next thing that asks the queue is told to speak.
#[test]
fn stopping_the_turn_is_the_whole_of_what_an_interrupt_has_to_do() {
    let scratch = Scratch::new("stopped");
    let store = scratch.open();
    let conversation = store.main_conversation(HarnessKind::ClaudeCode, "/tmp").unwrap();
    turn_in_flight(&store, &conversation, "run-main", "port the parser");
    let queued = store
        .enqueue_delivery(&conversation, Kind::Human, "typed", "no — the other repo")
        .unwrap();
    store.claim_for_review(&[queued.id]).unwrap();

    // What the doorman knows, and it is read from the same join.
    let flight = store
        .in_flight_turn(&conversation)
        .unwrap()
        .expect("there is a turn to stop");
    assert_eq!(flight.run_id, "run-main");
    assert_eq!(flight.asked.as_deref(), Some("port the parser"));

    // The verdict: the rows go back in the queue, and the run dies.
    store.finish_review(&[queued.id]).unwrap();
    turn_ends(&store, "run-main");

    let store = scratch.open();
    let busy = store.conversation_is_busy(&conversation).unwrap();
    let injection = store
        .plan_injection(&conversation, busy)
        .unwrap()
        .speak()
        .expect("nothing is in flight, so the message goes in as the next turn");
    assert!(injection.prompt.contains("no — the other repo"));
    assert_eq!(injection.items[0].state, State::Queued);
}

/// A message left `reviewing` by an assistant that died is not a lost message.
///
/// The failure this guards against is silent and permanent: `pending_for` does
/// not see a `reviewing` row, so a crashed doorman would take Reljod's sentence
/// out of the queue for ever with nothing reporting it missing.
#[test]
fn a_message_survives_the_assistant_reading_it_dying() {
    let scratch = Scratch::new("crashed");
    let conversation = {
        let store = scratch.open();
        let conversation = store.main_conversation(HarnessKind::ClaudeCode, "/tmp").unwrap();
        turn_in_flight(&store, &conversation, "run-main", "port the parser");
        let queued = store
            .enqueue_delivery(&conversation, Kind::Human, "typed", "still here")
            .unwrap();
        store.claim_for_review(&[queued.id]).unwrap();
        conversation
    };

    // The process holding it goes away without a verdict. What is left on disk
    // is a row nothing is reading and nothing is showing.
    let store = scratch.open();
    assert!(store.pending_for(&conversation).unwrap().is_empty());
    let stranded = store.under_review_for(&conversation).unwrap();
    assert_eq!(stranded.len(), 1, "and it is findable, which is the point");

    let ids: Vec<i64> = stranded.iter().map(|p| p.id).collect();
    store.finish_review(&ids).unwrap();
    assert_eq!(store.pending_for(&conversation).unwrap().len(), 1);
}
