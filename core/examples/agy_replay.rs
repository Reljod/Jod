//! Replay a captured AGY transcript through the adapter and print what Jod
//! would have shown for it.
//!
//! ```text
//! agy --print "…" --output-format stream-json --add-dir "$PWD" > run.jsonl
//! cargo run -p jod-core --example agy_replay -- run.jsonl
//! ```
//!
//! This exists because the AGY adapter can only be wrong in one way: by
//! disagreeing with what AGY actually prints. Two of the bugs it has had —
//! reading a tool's result out of a field AGY does not send, and reading a
//! result labelled `ERROR` as a failed run when the turn had answered in full —
//! were invisible to every unit test, because the tests and the adapter shared
//! the same wrong idea of the format. Both showed up the moment a real capture
//! went through it. Reach for this before changing anything in `harness::agy`,
//! and keep the capture: it is the evidence a fixture cannot be.

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: agy_replay <capture.jsonl>");
    let text = std::fs::read_to_string(&path).expect("reading the capture");
    let mut harness: Box<dyn jod_core::harness::Harness> =
        jod_core::harness::HarnessKind::Agy.build();
    for line in text.lines() {
        for event in harness.parse_line(line) {
            println!("{event:?}");
        }
    }
    // The exit code the real supervisor would have passed in. Zero, because a
    // capture taken from a shell that succeeded is the interesting case: it is
    // where the adapter's own verdict is the only thing deciding the run.
    println!("FINAL {:?}", harness.finalize(Some(0)));
}
