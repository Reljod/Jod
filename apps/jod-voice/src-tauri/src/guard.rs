//! Guards against transcribing things that are not Taglish speech.
//!
//! Two independent problems, two layers:
//!
//! 1. **Whisper hallucinates on non-speech.** Fed silence or room tone it emits
//!    canned subtitle-corpus phrases — "Thank you.", "Okay.", "Outro" in
//!    English, and their Korean or Japanese equivalents ("시청해주셔서
//!    감사합니다") when the language detector free-runs. The fix is to never
//!    send non-speech in the first place.
//! 2. **Auto-detect can land outside Tagalog/English.** Once it picks Korean,
//!    the decoder emits Hangul and the transcript is unusable.
//!
//! Layer 1 is the real cure — measurements showed every spurious detection came
//! from a buffer with no speech in it. Layer 2 is the backstop for when real
//! speech is still misread.

/// Shortest buffer that could plausibly hold a word. Below this, a key-tap
/// misfire is far likelier than an utterance.
const MIN_DURATION_SECS: f32 = 0.35;

/// Absolute floor. Digital silence sits at 0.0 and a muted mic near it; real
/// speech clears this even when recorded quietly and far from the mic.
const MIN_PEAK: f32 = 0.003;

/// Peak-to-RMS ratio. This is the discriminator that matters, because it is
/// **gain-invariant** — it separates speech from noise without caring how loud
/// the mic is. Measured: Taglish speech lands at 6.0–6.6, steady white/pink
/// noise and mains hum at ~1.7. The threshold sits in the empty middle.
const MIN_CREST_FACTOR: f32 = 2.5;

#[derive(Debug, PartialEq)]
pub enum Speech {
    Present,
    TooShort,
    TooQuiet,
    /// Energy without the impulsive structure of speech — hum, fan, hiss.
    Noise,
}

impl Speech {
    /// Phrased for a user who just spoke and got nothing back, so it says what
    /// to do differently rather than naming a threshold.
    pub fn message(&self) -> &'static str {
        match self {
            Speech::Present => "",
            Speech::TooShort => "Too short — hold the key while you speak.",
            Speech::TooQuiet => "Didn't hear anything — check your microphone.",
            Speech::Noise => "Only background noise — nothing transcribed.",
        }
    }
}

/// Peak amplitude and RMS of a normalized f32 buffer.
pub fn levels(samples: &[f32]) -> (f32, f32) {
    if samples.is_empty() {
        return (0.0, 0.0);
    }
    let peak = samples.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    (peak, rms)
}

pub fn assess(samples: &[f32], rate: u32) -> Speech {
    if rate == 0 || (samples.len() as f32 / rate as f32) < MIN_DURATION_SECS {
        return Speech::TooShort;
    }
    let (peak, rms) = levels(samples);
    if peak < MIN_PEAK {
        return Speech::TooQuiet;
    }
    // rms is only zero when every sample is zero, which MIN_PEAK already caught.
    if rms > 0.0 && peak / rms < MIN_CREST_FACTOR {
        return Speech::Noise;
    }
    Speech::Present
}

// --- language and script ---------------------------------------------------

/// Languages this app accepts. Tagalog and English only, for now — anything
/// else is a detector error rather than a real utterance.
///
/// Providers report a display name (`"Tagalog"`) rather than a code, and
/// Filipino is the same language under its national name, so both are listed.
const ALLOWED_LANGUAGES: [&str; 5] = ["tagalog", "filipino", "english", "tl", "en"];

pub fn is_allowed_language(reported: &str) -> bool {
    let l = reported.trim().to_lowercase();
    ALLOWED_LANGUAGES.contains(&l.as_str())
}

/// Tagalog and English are both Latin-script, so any character from a
/// non-Latin writing system proves the transcript is wrong — whatever the
/// provider claims the language was. Catches the Hangul case directly, and
/// still works when a provider omits the language field entirely.
///
/// Returns the first offending character, for the log line.
pub fn disallowed_script_char(text: &str) -> Option<char> {
    text.chars().find(|c| {
        matches!(*c as u32,
            0x0370..=0x03FF   // Greek
            | 0x0400..=0x04FF // Cyrillic
            | 0x0590..=0x05FF // Hebrew
            | 0x0600..=0x06FF // Arabic
            | 0x0900..=0x097F // Devanagari
            | 0x0E00..=0x0E7F // Thai
            | 0x1100..=0x11FF // Hangul Jamo
            | 0x3040..=0x30FF // Hiragana + Katakana
            | 0x3130..=0x318F // Hangul Compatibility Jamo
            | 0x4E00..=0x9FFF // CJK Unified Ideographs
            | 0xAC00..=0xD7AF // Hangul Syllables
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthesizes a buffer with a chosen crest factor: impulsive like speech,
    /// or flat like noise.
    fn buffer(len: usize, peak: f32, flat: bool) -> Vec<f32> {
        let mut v = vec![if flat { peak } else { peak * 0.08 }; len];
        if !flat {
            v[0] = peak; // one loud transient against a quiet floor
        }
        v
    }

    #[test]
    fn accepts_speech_shaped_audio() {
        let v = buffer(16_000, 0.5, false);
        assert_eq!(assess(&v, 16_000), Speech::Present);
    }

    #[test]
    fn rejects_buffers_shorter_than_a_word() {
        let v = buffer(3_200, 0.5, false); // 0.2s
        assert_eq!(assess(&v, 16_000), Speech::TooShort);
    }

    #[test]
    fn rejects_digital_silence() {
        assert_eq!(assess(&vec![0.0; 16_000], 16_000), Speech::TooQuiet);
    }

    #[test]
    fn rejects_steady_noise_however_loud() {
        // Loud but flat — a fan or mains hum. Level must not rescue it.
        let v = buffer(32_000, 0.9, true);
        assert_eq!(assess(&v, 16_000), Speech::Noise);
    }

    #[test]
    fn accepts_quiet_speech_because_the_test_is_gain_invariant() {
        // Same shape as the accepted case, 100x quieter.
        let v = buffer(16_000, 0.005, false);
        assert_eq!(assess(&v, 16_000), Speech::Present);
    }

    #[test]
    fn empty_buffer_is_too_short() {
        assert_eq!(assess(&[], 16_000), Speech::TooShort);
    }

    #[test]
    fn levels_reports_peak_and_rms() {
        let (peak, rms) = levels(&[0.0, 1.0, -1.0, 0.0]);
        assert_eq!(peak, 1.0);
        assert!((rms - 0.70710677).abs() < 1e-6);
    }

    #[test]
    fn allows_tagalog_english_and_filipino_in_any_casing() {
        for l in ["Tagalog", "tagalog", "English", "Filipino", "tl", "EN"] {
            assert!(is_allowed_language(l), "{l} should be allowed");
        }
    }

    #[test]
    fn rejects_korean_and_other_languages() {
        for l in ["Korean", "ko", "Japanese", "Chinese", "Spanish"] {
            assert!(!is_allowed_language(l), "{l} should be rejected");
        }
    }

    #[test]
    fn flags_hangul_in_the_transcript() {
        // Whisper's Korean "thanks for watching" hallucination.
        assert!(disallowed_script_char("시청해주셔서 감사합니다").is_some());
    }

    #[test]
    fn flags_other_non_latin_scripts() {
        for s in ["ありがとう", "谢谢观看", "Спасибо", "شكرا", "ขอบคุณ"] {
            assert!(disallowed_script_char(s).is_some(), "{s} should be flagged");
        }
    }

    #[test]
    fn passes_taglish_including_accents_and_punctuation() {
        let ok = "Pwede mo bang i-refactor 'yung authentication module? Oo naman — ayos!";
        assert_eq!(disallowed_script_char(ok), None);
    }

    #[test]
    fn passes_code_identifiers_and_symbols() {
        assert_eq!(disallowed_script_char("i-update mo yung useEffect() sa React 18.2 — 100% done"), None);
    }
}
