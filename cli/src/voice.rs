//! Which engine transcribes you, and whether everything it needs is here.
//!
//! Dictation has three moving parts on three different machines' worth of
//! assumptions — a recorder program, a transcription engine, and a model — and
//! any of them can be missing. This module is the one place that answers "can
//! I press Ctrl-V", so the CLI's `jod voice check` and the console's own
//! failure message cannot disagree about it.
//!
//! ## Local is the default once a model exists
//!
//! Not because it is faster — it is not, on a laptop — but because it is the
//! only version of this that costs nothing per sentence and keeps what is said
//! at a desk on the desk. The network engine stays for a machine with no model
//! on it.

use std::path::PathBuf;
use std::sync::Arc;

use jod_core::store::Store;
use jod_voice_core::local::{self, Whisper};

/// Where the chosen model is remembered.
///
/// In `settings` rather than a config file, because the console, the CLI and
/// anything else reaching this database must agree, and a file is a second
/// place for the answer to live.
const MODEL_KEY: &str = "voice.model";
/// `local` or `cloud`. Absent means: local if a model is downloaded.
const ENGINE_KEY: &str = "voice.engine";

/// The language passed to whisper.
///
/// `auto`, always, and it is not configurable by accident: whisper.cpp
/// defaults to `en`, and Taglish decoded as English loses the Tagalog. See
/// [`jod_voice_core::local`].
pub const LANGUAGE: &str = "auto";

/// How an utterance will be transcribed.
#[derive(Debug, Clone)]
pub enum Engine {
    /// whisper.cpp, on this machine.
    Local {
        whisper: Whisper,
        model_path: PathBuf,
        model_name: String,
    },
    /// OpenRouter, over the network.
    Cloud { model: String },
}

impl Engine {
    /// A short phrase for the status bar and for `jod voice check`.
    pub fn label(&self) -> String {
        match self {
            Engine::Local { model_name, .. } => format!("{model_name} · on this machine"),
            Engine::Cloud { model } => format!("{model} · over the network"),
        }
    }

    /// Transcribe one utterance.
    ///
    /// Blocking for the local engine and async for the cloud one, so this is
    /// `async` and the local branch does its work inline. That is honest about
    /// what happens: whisper.cpp on a laptop occupies a core for a second, and
    /// pretending otherwise by spawning would only move where it blocks. The
    /// caller runs this off the UI loop either way.
    pub async fn transcribe(&self, wav: &[u8]) -> Result<String, String> {
        match self {
            Engine::Local {
                whisper,
                model_path,
                ..
            } => whisper.transcribe_wav(model_path, wav, LANGUAGE, None),
            Engine::Cloud { model } => {
                jod_voice_core::transcribe::transcribe(wav, model, None, None)
                    .await
                    .map(|t| t.text)
            }
        }
    }
}

/// What is set up, and what is missing.
pub struct Status {
    pub recorder: Option<String>,
    pub engine: Result<Engine, String>,
    pub installed: Vec<local::Model>,
}

/// Read the configured engine, or say precisely what is missing.
///
/// Every error here is written to be actionable: the failure this replaces is
/// pressing a key and getting nothing, which tells you only that something is
/// wrong somewhere.
pub fn resolve(store: &Arc<Store>, jod_home: &std::path::Path) -> Result<Engine, String> {
    let chosen = store.setting(MODEL_KEY).ok().flatten();
    let forced = store.setting(ENGINE_KEY).ok().flatten();

    if forced.as_deref() == Some("cloud") {
        return cloud();
    }

    // A named model that is not downloaded is a misconfiguration worth naming,
    // rather than silently falling back to the network and a bill.
    if let Some(name) = chosen.as_deref() {
        let Some(m) = local::model(name) else {
            return Err(format!(
                "`{name}` is not a model this build knows — `jod voice models` lists them"
            ));
        };
        if !m.is_installed(jod_home) {
            return Err(format!(
                "{name} is chosen but not downloaded — `jod voice download {name}` fetches it \
                 ({} MB)",
                m.mb
            ));
        }
        let whisper = Whisper::detect().ok_or_else(missing_whisper)?;
        return Ok(Engine::Local {
            whisper,
            model_path: m.path(jod_home),
            model_name: m.name.to_string(),
        });
    }

    // Nothing chosen. A downloaded model is taken as the intent — downloading
    // one is not something anybody does by accident.
    if let Some(m) = local::installed(jod_home).into_iter().next_back() {
        if let Some(whisper) = Whisper::detect() {
            return Ok(Engine::Local {
                whisper,
                model_path: m.path(jod_home),
                model_name: m.name.to_string(),
            });
        }
    }
    cloud()
}

fn cloud() -> Result<Engine, String> {
    if !jod_voice_core::transcribe::is_configured() {
        return Err(format!(
            "dictation is not set up. Either download a model to transcribe here — \
             `jod voice download {}` — or set OPENROUTER_API_KEY to transcribe over \
             the network.",
            local::RECOMMENDED
        ));
    }
    Ok(Engine::Cloud {
        model: jod_voice_core::DEFAULT_MODEL.to_string(),
    })
}

fn missing_whisper() -> String {
    "a model is downloaded but whisper.cpp is not installed. Get it with \
     `brew install whisper-cpp`, your package manager, or a source build — then \
     put `whisper-cli` on PATH or point WHISPER_CLI at it."
        .to_string()
}

/// Everything `jod voice check` reports.
pub fn status(store: &Arc<Store>, jod_home: &std::path::Path) -> Status {
    Status {
        recorder: jod_voice_core::record::Backend::detect().map(|b| b.program().to_string()),
        engine: resolve(store, jod_home),
        installed: local::installed(jod_home),
    }
}

pub fn set_model(store: &Arc<Store>, name: &str) -> Result<(), String> {
    let m = local::model(name)
        .ok_or_else(|| format!("`{name}` is not a model this build knows"))?;
    store
        .set_setting(MODEL_KEY, m.name)
        .map_err(|e| format!("could not save the choice: {e}"))?;
    store
        .set_setting(ENGINE_KEY, "local")
        .map_err(|e| format!("could not save the choice: {e}"))
}

pub fn set_cloud(store: &Arc<Store>) -> Result<(), String> {
    store
        .set_setting(ENGINE_KEY, "cloud")
        .map_err(|e| format!("could not save the choice: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Arc<Store> {
        Arc::new(Store::in_memory().unwrap())
    }

    /// An empty machine has to be told the two ways forward, not merely that
    /// something is wrong.
    #[test]
    fn an_unconfigured_machine_is_told_both_ways_to_fix_it() {
        // Only meaningful without an ambient key; with one, cloud is genuinely
        // available and the message would be wrong to show.
        if jod_voice_core::transcribe::is_configured() {
            return;
        }
        let err = resolve(&store(), std::path::Path::new("/nonexistent")).unwrap_err();
        assert!(err.contains("jod voice download"), "{err}");
        assert!(err.contains("OPENROUTER_API_KEY"), "{err}");
    }

    /// Silently falling back to the network would turn a typo into a bill.
    #[test]
    fn a_chosen_model_that_is_not_downloaded_says_so_rather_than_going_to_the_network() {
        let s = store();
        set_model(&s, "small").unwrap();
        let err = resolve(&s, std::path::Path::new("/nonexistent")).unwrap_err();
        assert!(err.contains("not downloaded"), "{err}");
        assert!(err.contains("jod voice download small"), "{err}");
    }

    #[test]
    fn a_model_this_build_does_not_know_is_refused_at_the_point_of_choosing() {
        assert!(set_model(&store(), "enormous").is_err());
    }

    #[test]
    fn choosing_a_model_is_remembered() {
        let s = store();
        set_model(&s, "large-v3-turbo").unwrap();
        assert_eq!(s.setting(MODEL_KEY).unwrap().as_deref(), Some("large-v3-turbo"));
        assert_eq!(s.setting(ENGINE_KEY).unwrap().as_deref(), Some("local"));
    }

    /// The console announces the engine before he speaks, so the label has to
    /// distinguish the two without needing the rest of the sentence.
    #[test]
    fn the_label_says_where_the_transcription_happens() {
        let local = Engine::Local {
            whisper: Whisper {
                program: "whisper-cli".into(),
            },
            model_path: "/m/ggml.bin".into(),
            model_name: "small".into(),
        };
        assert!(local.label().contains("this machine"));
        let cloud = Engine::Cloud {
            model: "whisper-large-v3-turbo".into(),
        };
        assert!(cloud.label().contains("network"));
    }

    /// whisper.cpp defaults `-l` to English, and Taglish decoded as English
    /// loses the Tagalog. Nothing may make this configurable by accident.
    #[test]
    fn the_language_handed_to_whisper_is_always_auto() {
        assert_eq!(LANGUAGE, "auto");
    }
}
