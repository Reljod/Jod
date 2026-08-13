//! OpenRouter transcription, plus the optional Taglish repair pass.
//!
//! **The repair prompt below is kept in step with
//! `apps/jod-voice/src-tauri/src/openrouter.rs` by hand** — see the note in
//! [`crate`]. It is the part most expensive to get wrong and the easiest to
//! let drift.
//!
//! Two endpoints, deliberately:
//!
//! * `/audio/transcriptions` is the fast path. It is the cheapest and lowest
//!   latency route, but **every provider ignores its `prompt` field**, so
//!   there is no way to bias it toward Taglish or toward this codebase's
//!   jargon. Model choice is the only lever there is.
//! * `/chat/completions` accepts a system prompt, which is the only place a
//!   Taglish instruction can actually land. So the optional repair pass runs
//!   there, over the *text* — cheap, because no audio is re-sent.

use base64::Engine;
use serde::{Deserialize, Serialize};
use std::time::Instant;

use crate::guard;

const TRANSCRIBE_URL: &str = "https://openrouter.ai/api/v1/audio/transcriptions";
const CHAT_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

/// Guards against a hung request wedging dictation. OpenRouter's own ceiling
/// is 60s; this cuts earlier because a transcript that slow has already lost
/// its argument for existing over typing.
const TIMEOUT_SECS: u64 = 30;

/// Instruction for the repair pass.
///
/// The three prohibitions map to the documented failure modes of
/// code-switched ASR: translating instead of transcribing, collapsing to a
/// single language, and hallucinating filler.
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

/// One dictated utterance, transcribed.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Transcript {
    pub text: String,
    /// Round-trip wall time, which is what is actually felt.
    pub latency_ms: u64,
    pub cost_usd: f64,
    pub model: String,
    /// Present only when the repair pass ran.
    pub raw_text: Option<String>,
    /// Language the provider reported. Carried so a wrong detection is visible
    /// rather than silent.
    pub language: Option<String>,
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
    /// `verbose_json` is what makes the language guard possible — plain `json`
    /// returns only `text`, so a misdetection would be invisible until the
    /// wrong script showed up on screen.
    response_format: &'a str,
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
    #[serde(default)]
    language: Option<String>,
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("could not build HTTP client: {e}"))
}

/// Reads the key from the environment only, so the secret never lands in a
/// config file or in this repo.
pub fn api_key() -> Result<String, String> {
    std::env::var("OPENROUTER_API_KEY").map_err(|_| {
        "OPENROUTER_API_KEY is not set, so dictation cannot transcribe. \
         Export it, or launch the console through Doppler."
            .to_string()
    })
}

/// Whether dictation is configured at all.
///
/// Checked before a key is pressed rather than after an utterance, so the
/// answer to "why did nothing happen" arrives before he has spoken a sentence
/// into a console that was never going to transcribe it.
pub fn is_configured() -> bool {
    std::env::var("OPENROUTER_API_KEY").is_ok_and(|k| !k.trim().is_empty())
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

async fn transcribe_once(
    key: &str,
    b64: &str,
    model: &str,
    language: Option<&str>,
) -> Result<(String, Option<String>, f64), String> {
    let res = client()?
        .post(TRANSCRIBE_URL)
        .bearer_auth(key)
        .json(&TranscribeReq {
            model,
            input_audio: InputAudio {
                data: b64,
                format: "wav",
            },
            language,
            response_format: "verbose_json",
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
    Ok((
        parsed.text.trim().to_string(),
        parsed.language,
        parsed.usage.map(|u| u.cost).unwrap_or(0.0),
    ))
}

/// Rejects a transcript whose script proves it is neither Tagalog nor English.
///
/// Deliberately the *only* check applied to the returned text. Anything that
/// re-requests or rewrites risks turning transcription into translation, which
/// is a worse failure than the one it would fix.
fn wrong_script(text: &str) -> Option<String> {
    guard::disallowed_script_char(text)
        .map(|c| format!("non-Latin script in transcript (U+{:04X})", c as u32))
}

/// Transcribe one utterance.
///
/// `repair_with` names a chat model for the Taglish repair pass, or `None` to
/// return what the transcriber said verbatim.
pub async fn transcribe(
    wav: &[u8],
    model: &str,
    language: Option<&str>,
    repair_with: Option<&str>,
) -> Result<Transcript, String> {
    let key = api_key()?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(wav);

    let started = Instant::now();
    let (text, reported, mut cost) = transcribe_once(&key, &b64, model, language).await?;

    // There is no retry and no second opinion. The speech gate already removed
    // the case that caused stray detections, and re-requesting with a language
    // hint is precisely what makes Whisper translate rather than transcribe.
    if let Some(reason) = wrong_script(&text) {
        return Err(format!(
            "that came back in another language ({reason}) — please try again"
        ));
    }

    let mut out = Transcript {
        text: text.clone(),
        latency_ms: started.elapsed().as_millis() as u64,
        cost_usd: cost,
        model: model.to_string(),
        raw_text: None,
        language: reported,
    };

    let Some(repair_model) = repair_with else {
        return Ok(out);
    };
    if out.text.is_empty() {
        return Ok(out);
    }

    // A failed repair costs the polish, never the utterance — hence `if let`
    // and no error branch.
    if let Ok((repaired, spent)) = repair(&key, &out.text, repair_model).await {
        cost += spent;
        // The repair pass is an improvement, never a gate: if it dropped the
        // Tagalog it has done the one thing it was told not to, and the raw
        // transcript is strictly better than its output.
        if guard::looks_translated(&out.text, &repaired) || wrong_script(&repaired).is_some() {
            out.cost_usd = cost;
            return Ok(out);
        }
        out.raw_text = Some(out.text.clone());
        out.text = repaired;
        out.cost_usd = cost;
    }
    out.latency_ms = started.elapsed().as_millis() as u64;
    Ok(out)
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct ChatReq<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    temperature: f32,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Deserialize)]
struct ChatChoiceMessage {
    #[serde(default)]
    content: String,
}

#[derive(Deserialize)]
struct ChatRes {
    #[serde(default)]
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<Usage>,
}

/// The text-only repair pass.
async fn repair(key: &str, text: &str, model: &str) -> Result<(String, f64), String> {
    let res = client()?
        .post(CHAT_URL)
        .bearer_auth(key)
        .json(&ChatReq {
            model,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: REPAIR_SYSTEM,
                },
                ChatMessage {
                    role: "user",
                    content: text,
                },
            ],
            // Zero: this is a correction, not a rewrite, and every degree of
            // freedom here is a chance to paraphrase what was actually said.
            temperature: 0.0,
        })
        .send()
        .await
        .map_err(|e| format!("repair request failed: {e}"))?;

    if !res.status().is_success() {
        return Err(read_error(res).await);
    }
    let parsed: ChatRes = res
        .json()
        .await
        .map_err(|e| format!("could not parse repair response: {e}"))?;
    let out = parsed
        .choices
        .first()
        .map(|c| c.message.content.trim().to_string())
        .unwrap_or_default();
    if out.is_empty() {
        return Err("the repair pass returned nothing".into());
    }
    Ok((out, parsed.usage.map(|u| u.cost).unwrap_or(0.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The prompt is the feature. A repair pass that lost these rules would
    /// translate rather than repair, and nothing else in the pipeline checks.
    #[test]
    fn the_repair_prompt_still_forbids_the_three_failure_modes() {
        assert!(REPAIR_SYSTEM.contains("NEVER translate"));
        assert!(REPAIR_SYSTEM.contains("NEVER drop a language"));
        assert!(REPAIR_SYSTEM.contains("NEVER add content"));
    }

    #[test]
    fn a_transcript_in_another_script_is_refused() {
        assert!(wrong_script("시청해주셔서 감사합니다").is_some());
    }

    /// Taglish is Latin-script throughout, so the guard must never fire on it.
    #[test]
    fn ordinary_taglish_is_not_refused() {
        assert!(wrong_script("pwede ba nating i-refactor yung parser?").is_none());
    }

    #[test]
    fn dictation_is_not_configured_without_a_key() {
        // Reads the ambient environment; asserted only in the direction that
        // cannot be wrong either way it is set.
        if std::env::var("OPENROUTER_API_KEY").is_err() {
            assert!(!is_configured());
        }
    }
}
