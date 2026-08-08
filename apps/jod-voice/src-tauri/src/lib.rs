//! jod-voice — Taglish-aware push-to-talk dictation.
//!
//! The loop is deliberately push-to-talk rather than streaming: a coding
//! instruction is one bounded utterance, and sending it as a single request
//! after key-release beats streaming on both accuracy (the model sees the whole
//! sentence) and cost, while the perceived wait stays under a second.

mod audio;
mod openrouter;
mod sessions;

use std::sync::Mutex;

use serde::Serialize;
use tauri::{Emitter, Manager, State};

/// The default is the empirical winner on Taglish: lowest measured latency of
/// any model that transcribes Tagalog correctly. See `docs/RESEARCH.md`.
pub const DEFAULT_MODEL: &str = "openai/whisper-large-v3-turbo";
pub const DEFAULT_REPAIR_MODEL: &str = "google/gemini-3.1-flash-lite";

struct AppState {
    recorder: Mutex<audio::Recorder>,
    /// Kept so a comparison run can re-use the last utterance without asking
    /// the user to say it again.
    last_wav: Mutex<Option<Vec<u8>>>,
}

#[derive(Serialize, Clone)]
struct RecordingState {
    recording: bool,
    duration_secs: f32,
    level: f32,
}

#[tauri::command]
fn start_recording(state: State<'_, AppState>) -> Result<(), String> {
    state.recorder.lock().unwrap().start()
}

#[tauri::command]
fn recording_state(state: State<'_, AppState>) -> RecordingState {
    let r = state.recorder.lock().unwrap();
    RecordingState { recording: r.is_recording(), duration_secs: r.duration_secs(), level: r.level() }
}

#[tauri::command]
async fn stop_and_transcribe(
    state: State<'_, AppState>,
    model: Option<String>,
    language: Option<String>,
    repair: bool,
    repair_model: Option<String>,
) -> Result<openrouter::Transcript, String> {
    // Scope the lock: the guard must not be held across the await below.
    let wav = {
        let mut r = state.recorder.lock().unwrap();
        r.stop()?
    };
    *state.last_wav.lock().unwrap() = Some(wav.clone());

    let model = model.unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let repair_model = repair
        .then(|| repair_model.unwrap_or_else(|| DEFAULT_REPAIR_MODEL.to_string()));

    openrouter::transcribe(&wav, &model, language.as_deref(), repair_model.as_deref()).await
}

/// Discards the buffer without transcribing — the escape hatch for a misfire.
#[tauri::command]
fn cancel_recording(state: State<'_, AppState>) -> Result<(), String> {
    let mut r = state.recorder.lock().unwrap();
    if r.is_recording() {
        let _ = r.stop();
    }
    Ok(())
}

#[tauri::command]
async fn compare_models(
    state: State<'_, AppState>,
    models: Vec<String>,
    language: Option<String>,
    use_last: bool,
) -> Result<Vec<openrouter::ModelResult>, String> {
    let wav = if use_last {
        state.last_wav.lock().unwrap().clone().ok_or("no recording yet — dictate something first")?
    } else {
        let mut r = state.recorder.lock().unwrap();
        let w = r.stop()?;
        *state.last_wav.lock().unwrap() = Some(w.clone());
        w
    };
    Ok(openrouter::compare(&wav, models, language).await)
}

#[tauri::command]
fn list_sessions(jod_repo: Option<String>) -> Result<Vec<sessions::Session>, String> {
    sessions::list(jod_repo.as_deref())
}

#[tauri::command]
fn send_to_session(id: String, message: String, jod_repo: Option<String>) -> Result<String, String> {
    sessions::send(&id, &message, jod_repo.as_deref())
}

#[tauri::command]
fn key_status() -> bool {
    openrouter::api_key().is_ok()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(AppState { recorder: Mutex::new(audio::Recorder::new()), last_wav: Mutex::new(None) })
        .invoke_handler(tauri::generate_handler![
            start_recording,
            stop_and_transcribe,
            cancel_recording,
            recording_state,
            compare_models,
            list_sessions,
            send_to_session,
            key_status,
        ])
        .setup(|app| {
            // Fail loudly at launch rather than on the first dictation — a
            // missing key is a launch mistake (forgot `doppler run`), and
            // finding out mid-sentence wastes the utterance.
            if openrouter::api_key().is_err() {
                eprintln!(
                    "[jod-voice] WARNING: OPENROUTER_API_KEY not set. \
                     Launch with: doppler run --project jod-apps --config dev_personal -- pnpm app"
                );
            }
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.emit("ready", ());
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running jod-voice");
}
