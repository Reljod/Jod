//! The local engine against a real whisper.cpp and real speech.
//!
//! `#[ignore]` by default, and deliberately not skipped-when-missing: a test
//! that quietly passes because its dependency is absent is a test that reports
//! green for a pipeline nobody ran. Ignored is honest — it says "this did not
//! run" rather than "this succeeded".
//!
//! To run it:
//!
//! ```sh
//! # whisper.cpp, built or installed
//! export WHISPER_CLI=/path/to/whisper.cpp/build/bin/whisper-cli
//! # any multilingual ggml model
//! export WHISPER_TEST_MODEL=/path/to/ggml-tiny.bin
//! # a 16 kHz 16-bit WAV of speech — whisper.cpp ships samples/jfk.wav
//! export WHISPER_TEST_WAV=/path/to/whisper.cpp/samples/jfk.wav
//!
//! cargo test -p jod-voice-core --test whisper_cpp -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use jod_voice_core::local::Whisper;

fn from_env(key: &str) -> PathBuf {
    PathBuf::from(
        std::env::var(key).unwrap_or_else(|_| panic!("{key} is not set — see this file's header")),
    )
}

/// End to end: the argv this crate builds, run against real speech, producing
/// text clean enough to drop straight into the composer.
///
/// Asserts on the words rather than the whole string because whisper's
/// punctuation varies between models, and pinning it would make the test about
/// the model rather than about the integration.
#[test]
#[ignore = "needs whisper.cpp, a model, and a WAV — see the header"]
fn real_speech_comes_back_as_a_clean_sentence() {
    let whisper = Whisper {
        program: from_env("WHISPER_CLI"),
    };
    let model = from_env("WHISPER_TEST_MODEL");
    let wav = from_env("WHISPER_TEST_WAV");

    let said = whisper
        .transcribe(&model, &wav, "auto", Some(4))
        .expect("whisper.cpp did not transcribe");

    println!("transcript: {said:?}");

    assert!(!said.is_empty(), "nothing came back");
    // The sample is JFK's inaugural; these words survive any model size.
    let lower = said.to_lowercase();
    assert!(
        lower.contains("country"),
        "the transcript does not look like the sample: {said:?}"
    );

    // The three things `clean` and the flags exist to guarantee.
    assert!(
        !said.contains('['),
        "a caption or timestamp reached the composer: {said:?}"
    );
    assert!(
        !said.contains("-->"),
        "timestamps were not suppressed: {said:?}"
    );
    assert!(
        !said.contains('\n'),
        "segments were not joined into one line: {said:?}"
    );
}

/// A missing model must fail loudly with whisper's own message, not silently
/// produce an empty transcript that reads as "you said nothing".
#[test]
#[ignore = "needs whisper.cpp — see the header"]
fn a_missing_model_is_an_error_rather_than_an_empty_transcript() {
    let whisper = Whisper {
        program: from_env("WHISPER_CLI"),
    };
    let wav = from_env("WHISPER_TEST_WAV");

    let err = whisper
        .transcribe(&PathBuf::from("/nonexistent/ggml.bin"), &wav, "auto", None)
        .expect_err("a missing model was not reported");
    println!("error: {err}");
    assert!(err.to_lowercase().contains("whisper"));
}
