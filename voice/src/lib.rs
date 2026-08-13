//! Dictation for the `jod` console.
//!
//! Talking to the orchestrator instead of typing at it, from inside the TUI.
//! Three steps, and the seams between them are where the decisions live:
//!
//! 1. [`record`] captures an utterance — by running a recorder *program*,
//!    never by linking an audio backend. See that module for why.
//! 2. [`guard`] decides whether what came back is speech at all, before a
//!    single byte leaves the machine.
//! 3. Transcription, by whichever of two engines is configured:
//!    - [`local`] runs whisper.cpp here. No key, no network, no per-utterance
//!      cost, and nothing said at a desk arriving at somebody's API. **This is
//!      the default** once a model is downloaded.
//!    - [`transcribe`] calls OpenRouter, for a machine with no model on it.
//!
//! The two are the same family of model on purpose — the cloud research picked
//! `whisper-large-v3-turbo` by measurement, and [`local::RECOMMENDED`] is its
//! GGML build — so switching engines changes the latency and the bill, not
//! whether Taglish survives.
//!
//! ## Why this is a crate and not a module in the CLI
//!
//! `jod` runs on a headless VPS. A dictation path that made the CLI
//! unbuildable without a sound card would trade the feature for the product,
//! so everything here is either pure or subprocess-shaped, and the crate
//! carries no system library of its own.
//!
//! ## Its relationship to `apps/jod-voice`
//!
//! The desktop app solves the same problem on hardware it can rely on: it
//! links `cpal` and captures in-process, which is the better answer when you
//! know there is a microphone. The two are deliberately not merged yet —
//! unifying them means building `apps/jod-voice`, which needs ALSA headers
//! this crate specifically avoids depending on. The Taglish repair prompt and
//! the speech thresholds are the parts that must not drift; they are marked in
//! [`guard`] and [`transcribe`].

pub mod guard;
pub mod local;
pub mod record;
pub mod spoken;
pub mod stream;
pub mod transcribe;

pub use guard::Speech;
pub use local::{Model, Whisper};
pub use record::{Recorder, Recording};
pub use spoken::Spoken;
pub use stream::{Heard, Session};
pub use transcribe::Transcript;

/// The model dictation uses unless told otherwise.
///
/// Measured, not assumed: 0% WER on the Taglish fixtures at 0.76s p50. Several
/// faster models silently delete the Tagalog half of a code-switched sentence,
/// which is worse than being slow because it looks like success.
/// → `apps/jod-voice/docs/RESEARCH.md`
pub const DEFAULT_MODEL: &str = "openai/whisper-large-v3-turbo";
