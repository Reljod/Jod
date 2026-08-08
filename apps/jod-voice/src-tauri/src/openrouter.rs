//! OpenRouter client — transcription plus an optional Taglish repair pass.
//!
//! Two endpoints, deliberately:
//!
//! * `/audio/transcriptions` is the fast path. It is the cheapest, lowest
//!   latency route, but it **ignores the `prompt` field on every provider**, so
//!   there is no way to bias it toward Taglish or toward your codebase's jargon.
//!   Model choice is the only lever.
//! * `/chat/completions` accepts a system prompt. That is the only place a
//!   Taglish instruction can actually land, so the optional repair pass runs
//!   there over the *text* — cheap, since no audio is re-sent.

use base64::Engine;
use serde::{Deserialize, Serialize};
use std::time::Instant;

const TRANSCRIBE_URL: &str = "https://openrouter.ai/api/v1/audio/transcriptions";
const CHAT_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

/// Guards against a hung request wedging the push-to-talk loop. OpenRouter's own
/// ceiling is 60s; we cut earlier because a dictation that slow is already
/// useless.
const TIMEOUT_SECS: u64 = 30;

/// Instruction for the repair pass. The three prohibitions map to the documented
/// failure modes of code-switched ASR: translating instead of transcribing,
/// collapsing to a single language, and hallucinating filler.
const REPAIR_SYSTEM: &str = "\
You repair raw speech-to-text output for a Filipino software engineer who dictates in Taglish \
(Tagalog-English code-switching) to a coding assistant.

Rules, in order of importance:
1. NEVER translate. Tagalog words stay Tagalog; English words stay English. Preserve the mix exactly as spoken.
2. NEVER drop a language. If the speaker mixed both, the output mixes both.
3. NEVER add content, commentary, or filler that was not spoken.
4. Fix obvious mis-hearings of technical terms (e.g. 'react hook' spellings, 'git' commands, \
library and API names, camelCase or kebab-case identifiers).
5. Apply Filipino verb-affix conventions on English stems with a hyphen: i-refactor, na-deploy, mag-commit, i-check.
6. Add sentence punctuation and capitalization. Remove pure disfluencies (uh, ah, eh) only.

Return ONLY the corrected transcript. No preamble, no quotes, no explanation.";

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Transcript {
    pub text: String,
    /// Round-trip wall time, which is what the user actually feels.
    pub latency_ms: u64,
    pub cost_usd: f64,
    pub model: String,
    /// Present only when the repair pass ran.
    pub raw_text: Option<String>,
    pub repair_ms: Option<u64>,
}

#[derive(Serialize)]
struct InputAudio<'a> {
    data: &'a str,
    format: &'a str,
}

#[derive(Serialize)]
struct TranscribeReq<'a> {
    model: &'a str,
    input_audio: InputAudio<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<&'a str>,
}

#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    cost: f64,
}

#[derive(Deserialize)]
struct TranscribeRes {
    text: String,
    #[serde(default)]
    usage: Option<Usage>,
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("could not build HTTP client: {e}"))
}

/// Reads the key from the environment only. The app is launched through
/// `doppler run`, so the secret never lands in a config file or in this repo.
pub fn api_key() -> Result<String, String> {
    std::env::var("OPENROUTER_API_KEY").map_err(|_| {
        "OPENROUTER_API_KEY is not set. Launch with: \
         doppler run --project jod-apps --config dev_personal -- pnpm app"
            .to_string()
    })
}

/// Surfaces the provider's own error text instead of a bare status code —
/// OpenRouter's messages are specific (bad model id, unsupported format) and
/// guessing from a 400 wastes a debugging cycle.
async fn read_error(res: reqwest::Response) -> String {
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    match serde_json::from_str::<serde_json::Value>(&body) {
        Ok(v) => {
            let msg = v
                .pointer("/error/message")
                .and_then(|m| m.as_str())
                .unwrap_or(&body)
                .to_string();
            format!("OpenRouter {status}: {msg}")
        }
        Err(_) => format!("OpenRouter {status}: {body}"),
    }
}

pub async fn transcribe(
    wav: &[u8],
    model: &str,
    language: Option<&str>,
    repair_with: Option<&str>,
) -> Result<Transcript, String> {
    let key = api_key()?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(wav);

    let started = Instant::now();
    let res = client()?
        .post(TRANSCRIBE_URL)
        .bearer_auth(&key)
        .json(&TranscribeReq {
            model,
            input_audio: InputAudio { data: &b64, format: "wav" },
            language,
        })
        .send()
        .await
        .map_err(|e| format!("transcription request failed: {e}"))?;

    if !res.status().is_success() {
        return Err(read_error(res).await);
    }

    let parsed: TranscribeRes = res
        .json()
        .await
        .map_err(|e| format!("could not parse transcription response: {e}"))?;
    let latency_ms = started.elapsed().as_millis() as u64;
    let mut cost = parsed.usage.map(|u| u.cost).unwrap_or(0.0);
    let text = parsed.text.trim().to_string();

    // Repair is best-effort: a failure there must not lose a good transcript.
    if let Some(repair_model) = repair_with {
        if !text.is_empty() {
            let t = Instant::now();
            match repair(&text, repair_model, &key).await {
                Ok((fixed, repair_cost)) => {
                    cost += repair_cost;
                    return Ok(Transcript {
                        text: fixed,
                        latency_ms,
                        cost_usd: cost,
                        model: model.to_string(),
                        raw_text: Some(text),
                        repair_ms: Some(t.elapsed().as_millis() as u64),
                    });
                }
                Err(e) => eprintln!("[jod-voice] repair pass failed, using raw text: {e}"),
            }
        }
    }

    Ok(Transcript { text, latency_ms, cost_usd: cost, model: model.to_string(), raw_text: None, repair_ms: None })
}

#[derive(Deserialize)]
struct ChatRes {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct ChatMessage {
    #[serde(default)]
    content: String,
}

async fn repair(text: &str, model: &str, key: &str) -> Result<(String, f64), String> {
    let body = serde_json::json!({
        "model": model,
        "temperature": 0,
        "messages": [
            { "role": "system", "content": REPAIR_SYSTEM },
            { "role": "user", "content": text }
        ]
    });

    let res = client()?
        .post(CHAT_URL)
        .bearer_auth(key)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("repair request failed: {e}"))?;

    if !res.status().is_success() {
        return Err(read_error(res).await);
    }

    let parsed: ChatRes = res.json().await.map_err(|e| format!("could not parse repair response: {e}"))?;
    let out = parsed
        .choices
        .first()
        .map(|c| c.message.content.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "repair returned no text".to_string())?;
    Ok((out, parsed.usage.map(|u| u.cost).unwrap_or(0.0)))
}

/// One utterance against many models at once — the only honest way to pick a
/// model, since published WER is measured on read speech, not on your voice.
pub async fn compare(wav: &[u8], models: Vec<String>, language: Option<String>) -> Vec<ModelResult> {
    let tasks: Vec<_> = models
        .into_iter()
        .map(|m| {
            let wav = wav.to_vec();
            let lang = language.clone();
            tokio::spawn(async move {
                match transcribe(&wav, &m, lang.as_deref(), None).await {
                    Ok(t) => ModelResult {
                        model: m,
                        text: t.text,
                        latency_ms: t.latency_ms,
                        cost_usd: t.cost_usd,
                        error: None,
                    },
                    Err(e) => ModelResult {
                        model: m,
                        text: String::new(),
                        latency_ms: 0,
                        cost_usd: 0.0,
                        error: Some(e),
                    },
                }
            })
        })
        .collect();

    let mut out = Vec::new();
    for t in tasks {
        if let Ok(r) = t.await {
            out.push(r);
        }
    }
    out.sort_by_key(|r| if r.error.is_some() { u64::MAX } else { r.latency_ms });
    out
}

#[derive(Debug, Serialize, Clone)]
pub struct ModelResult {
    pub model: String,
    pub text: String,
    pub latency_ms: u64,
    pub cost_usd: f64,
    pub error: Option<String>,
}
