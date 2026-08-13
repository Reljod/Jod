//! Guards against transcribing things that are not Taglish speech.
//!
//! **Kept in step with `apps/jod-voice/src-tauri/src/guard.rs` by hand.** The
//! thresholds below are measurements, not preferences, and the two copies
//! exist only because unifying them requires building the desktop app — which
//! needs the ALSA headers this crate exists to avoid. Change one, change both.
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
            // Not "hold the key": in the console this is a toggle, because a
            // terminal cannot see a key being released. See `cli/src/tui`.
            Speech::TooShort => "Too short — nothing was said.",
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

// --- translation detection --------------------------------------------------

/// Tagalog function words and particles with no English homographs. Presence of
/// several is strong evidence the speaker was using Tagalog; their sudden
/// absence is strong evidence something translated it away.
///
/// Deliberately excludes words that are also English ("may", "sa", "din") so
/// that English-only dictation never scores as Tagalog.
const TAGALOG_MARKERS: [&str; 34] = [
    "ang", "ng", "mga", "yung", "iyong", "kasi", "tapos", "pwede", "puwede", "hindi", "naman",
    "ako", "akong", "ikaw", "siya", "niya", "nila", "natin", "namin", "ninyo", "kong", "mong",
    "yun", "iyon", "ito", "dito", "diyan", "ganito", "talaga", "grabe", "muntik", "buti",
    "kaya", "bang",
];

/// How many distinct Tagalog markers appear in `text`.
pub fn tagalog_markers(text: &str) -> usize {
    let words: Vec<String> = text
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .map(|w| w.trim_matches('\'').to_string())
        .filter(|w| !w.is_empty())
        .collect();
    TAGALOG_MARKERS.iter().filter(|m| words.iter().any(|w| w == *m)).count()
}

/// True when `after` looks like a translation of `before` rather than a cleanup
/// of it — the Tagalog went in and did not come out.
///
/// Requires two markers before, so a single incidental word cannot trip it, and
/// fires only on a total wipe. A repair pass that keeps any Tagalog at all is
/// doing its job.
pub fn looks_translated(before: &str, after: &str) -> bool {
    tagalog_markers(before) >= 2 && tagalog_markers(after) == 0
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
    fn counts_tagalog_markers_in_taglish() {
        let t = "Pwede mo bang i-refactor yung authentication module kasi may bug";
        assert!(tagalog_markers(t) >= 3, "got {}", tagalog_markers(t));
    }

    #[test]
    fn english_only_text_scores_zero_markers() {
        assert_eq!(tagalog_markers("Please refactor the authentication module now"), 0);
    }

    #[test]
    fn detects_the_real_translation_observed_from_the_api() {
        // Measured: pinning language=en turned this Taglish utterance into English.
        let before = "Pwede mo bang i-refactor yung authentication module? \
                      Yung login flow kasi may bug pa rin sa session handling.";
        let after = "Can you refactor your authentication module? \
                     The login flow is still a bug in session handling.";
        assert!(looks_translated(before, after));
    }

    #[test]
    fn a_cleanup_that_keeps_tagalog_is_not_a_translation() {
        let before = "pwede mo bang i refactor yung authentication module";
        let after = "Pwede mo bang i-refactor yung authentication module?";
        assert!(!looks_translated(before, after));
    }

    #[test]
    fn english_dictation_is_never_flagged_as_translated() {
        // No Tagalog went in, so nothing can have been translated away.
        let before = "refactor the auth module and add unit tests";
        let after = "Refactor the auth module and add unit tests.";
        assert!(!looks_translated(before, after));
    }

    #[test]
    fn a_single_incidental_marker_does_not_trip_the_check() {
        assert!(!looks_translated("update ang module", "Update the module"));
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
