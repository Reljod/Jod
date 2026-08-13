//! Telling the console what to do, out loud.
//!
//! Hands-free means the last thing you do with a sentence — send it — has to
//! be sayable too. This module reads a finished utterance and decides whether
//! it ends in an instruction to the console rather than words for the composer.
//!
//! ## Why a phrase list and not a model
//!
//! A classifier would understand more phrasings. It would also cost a second
//! model pass per utterance, add latency to every sentence, and — the part
//! that matters — occasionally decide that "let's go ahead and refactor the
//! parser" was a command to send. A misfire here dispatches agents at a
//! repository while your hands are full.
//!
//! So the rule is narrow and predictable, which is what makes it trustworthy:
//!
//! * a command is only recognised at the **end** of an utterance, because that
//!   is where you say it,
//! * it must be **all that is left** in its clause — "go ahead" after a comma
//!   or a pause, not "go ahead and…",
//! * and the phrase is **stripped** before the text is used, so the word "send"
//!   never lands in the prompt.
//!
//! ## Taglish
//!
//! The phrases below are the ones actually said, in both languages and mixed.
//! "sige" and "sige na" are how somebody says go ahead here, and leaving them
//! out would mean the feature only works when you remember to speak English.

/// What an utterance turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spoken {
    /// Words for the composer.
    Text(String),
    /// Send what is in the composer. Carries any text said before the phrase,
    /// which belongs in the prompt first.
    Send(String),
    /// Throw away what is in the composer.
    Clear,
    /// Stop listening. The one command that must work when nothing else does.
    Stop,
    /// Undo the last utterance that was appended.
    Undo,
    /// Nothing usable — an empty transcript, or a caption.
    Nothing,
}

/// Phrases that mean *send it*.
///
/// Longest first, so "go ahead and send it" cannot match the short form and
/// leave "and send it" behind in the prompt.
const SEND: [&str; 22] = [
    "go ahead and send it",
    "okay go ahead na",
    "sige send mo na",
    "ipasok mo na",
    "ipasok mo",
    "isend mo na",
    "isend mo",
    "send it na",
    "send mo na",
    "go ahead na",
    "put it in",
    "send it in",
    "send that",
    "send it",
    "submit it",
    "go ahead",
    "sige na",
    "send now",
    "do it na",
    "sige",
    "submit",
    "send",
];

/// Phrases that mean *forget what I just said*.
const CLEAR: [&str; 10] = [
    "scratch all of that",
    "scratch that",
    "clear that",
    "delete that",
    "forget that",
    "never mind",
    "nevermind",
    "start over",
    "burahin mo",
    "clear it",
];

/// Phrases that mean *stop listening*.
///
/// These matter most: this is how the microphone gets switched off when your
/// hands are not free, so it is worth being generous.
const STOP: [&str; 9] = [
    "stop listening",
    "stop the mic",
    "stop dictation",
    "mic off",
    "microphone off",
    "tama na",
    "tigil muna",
    "stop listening na",
    "hinto muna",
];

/// Phrases that mean *take back the last thing I said*.
const UNDO: [&str; 6] = [
    "undo that",
    "take that back",
    "remove that",
    "delete the last one",
    "bawiin mo",
    "undo",
];

/// Read a finished transcript.
pub fn interpret(transcript: &str) -> Spoken {
    let text = transcript.trim();
    if text.is_empty() {
        return Spoken::Nothing;
    }

    // The three that take something back are checked first, and as a clause
    // *anywhere* rather than only at the end. "scratch that, go ahead" has to
    // clear rather than send: one reading loses a sentence, the other sends a
    // sentence that was just cancelled, and only the second one starts agents.
    //
    // Stop leads, because if the microphone is mishearing badly enough that
    // nothing else works, switching it off is the command that must survive.
    if has_clause(text, &STOP) {
        return Spoken::Stop;
    }
    if has_clause(text, &CLEAR) {
        return Spoken::Clear;
    }
    if has_clause(text, &UNDO) {
        return Spoken::Undo;
    }
    // Send only at the end, where it is actually said.
    if let Some(rest) = ends_with_phrase(text, &SEND) {
        return Spoken::Send(rest);
    }
    Spoken::Text(text.to_string())
}

/// Whether one of `phrases` appears as a whole clause of `text`.
///
/// A clause is the whole utterance, or a run bounded by commas and full stops.
/// Splitting rather than substring-matching is what keeps "never mind the
/// tests, fix the parser" from being read as a cancellation.
fn has_clause(text: &str, phrases: &[&str]) -> bool {
    normalise(text)
        .split(['.', ','])
        .map(str::trim)
        .any(|clause| phrases.contains(&clause))
}

/// Whether `text` ends with one of `phrases`, returning what came before it.
///
/// The clause rule lives here: the phrase must either be the whole utterance
/// or be preceded by a comma or a full stop. That is what separates "refactor
/// the parser, go ahead" from "go ahead and refactor the parser" — the second
/// is not a command, and treating it as one would send half a sentence.
fn ends_with_phrase(text: &str, phrases: &[&str]) -> Option<String> {
    let normalised = normalise(text);
    for phrase in phrases {
        let Some(head) = normalised.strip_suffix(phrase) else {
            continue;
        };
        let head = head.trim_end();
        // The whole utterance was the command.
        if head.is_empty() {
            return Some(String::new());
        }
        // Otherwise it has to be its own clause.
        if !head.ends_with(',') && !head.ends_with('.') && !head.ends_with(" and") {
            continue;
        }
        if head.ends_with(" and") {
            // "go ahead and send it" already matched as a whole phrase; an
            // "and" here means the phrase is part of what was said.
            continue;
        }
        let kept = head.trim_end_matches([',', '.']).trim();
        return Some(recase(text, kept));
    }
    None
}

/// Lowercase, with trailing sentence punctuation removed.
///
/// Transcripts arrive capitalised and punctuated — the repair pass adds both —
/// so a phrase list matched against raw text would miss "Go ahead." every
/// time.
fn normalise(text: &str) -> String {
    text.trim()
        .trim_end_matches(['.', '!', '?', '…'])
        .trim()
        .to_lowercase()
}

/// Recover the original casing of the kept prefix.
///
/// Matching happens on a lowercased copy, so the offsets are only valid
/// because `to_lowercase` here maps ASCII one-to-one; a prefix length is used
/// rather than the lowercased text itself so "Refactor the Parser" is not
/// handed back flattened.
fn recase(original: &str, lowered_prefix: &str) -> String {
    let trimmed = original.trim();
    if lowered_prefix.is_empty() {
        return String::new();
    }
    // Walk the original by characters until as many have been seen as the
    // prefix has, which keeps this correct for multi-byte text.
    let want = lowered_prefix.chars().count();
    let kept: String = trimmed.chars().take(want).collect();
    kept.trim_end_matches([',', '.', ' ']).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn send(text: &str) -> String {
        match interpret(text) {
            Spoken::Send(rest) => rest,
            other => panic!("{text:?} was not heard as send: {other:?}"),
        }
    }

    fn text(t: &str) -> String {
        match interpret(t) {
            Spoken::Text(s) => s,
            other => panic!("{t:?} was not heard as text: {other:?}"),
        }
    }

    // ---- sending ----

    /// The phrase from the request.
    #[test]
    fn go_ahead_sends() {
        assert_eq!(send("go ahead"), "");
    }

    #[test]
    fn put_it_in_sends() {
        assert_eq!(send("put it in"), "");
    }

    /// Transcripts arrive capitalised and punctuated by the repair pass.
    #[test]
    fn punctuation_and_capitals_do_not_hide_the_command() {
        assert_eq!(send("Go ahead."), "");
        assert_eq!(send("Send it!"), "");
    }

    /// Saying the instruction and the command in one breath is the natural
    /// way to do this hands-free.
    #[test]
    fn an_instruction_and_the_command_arrive_together() {
        assert_eq!(send("fix the parser, go ahead"), "fix the parser");
    }

    #[test]
    fn the_command_is_stripped_so_it_never_reaches_the_prompt() {
        let kept = send("run the tests. send it");
        assert!(!kept.to_lowercase().contains("send"), "{kept:?}");
        assert_eq!(kept, "run the tests");
    }

    /// The misfire that matters: this is a sentence about work, not a command.
    #[test]
    fn go_ahead_inside_a_sentence_is_not_a_command() {
        assert_eq!(
            text("let's go ahead and refactor the parser"),
            "let's go ahead and refactor the parser"
        );
    }

    #[test]
    fn a_sentence_merely_containing_send_is_not_a_command() {
        assert_eq!(
            text("send the report to the API and log it"),
            "send the report to the API and log it"
        );
    }

    /// Taglish is the input this is built for.
    #[test]
    fn taglish_send_phrases_work() {
        assert_eq!(send("sige"), "");
        assert_eq!(send("ipasok mo na"), "");
        assert_eq!(send("i-refactor natin yung parser, sige na"), "i-refactor natin yung parser");
    }

    /// Longest-first matching: the short form must not win and leave a
    /// fragment behind in the prompt.
    #[test]
    fn a_longer_phrase_wins_over_the_short_one_inside_it() {
        assert_eq!(send("go ahead and send it"), "");
    }

    // ---- the other commands ----

    #[test]
    fn the_microphone_can_be_switched_off_by_voice() {
        assert_eq!(interpret("stop listening"), Spoken::Stop);
        assert_eq!(interpret("tama na"), Spoken::Stop);
    }

    #[test]
    fn a_mistake_can_be_thrown_away_by_voice() {
        assert_eq!(interpret("scratch that"), Spoken::Clear);
        assert_eq!(interpret("never mind"), Spoken::Clear);
    }

    #[test]
    fn the_last_utterance_can_be_taken_back() {
        assert_eq!(interpret("undo that"), Spoken::Undo);
    }

    /// Clear must beat send, or "scratch that, go ahead" would send the thing
    /// that was just cancelled — and sending is the one that starts agents.
    #[test]
    fn clearing_beats_sending_when_both_are_said() {
        assert_eq!(interpret("scratch that, go ahead"), Spoken::Clear);
    }

    /// The other half of the clause rule: a cancel phrase used as ordinary
    /// words must not cancel anything.
    #[test]
    fn a_cancel_phrase_inside_a_sentence_does_not_cancel() {
        assert_eq!(
            text("never mind the tests for now"),
            "never mind the tests for now"
        );
    }

    #[test]
    fn stop_is_recognised_after_dictation_in_the_same_breath() {
        assert_eq!(interpret("that's everything, stop listening"), Spoken::Stop);
    }

    /// Switching the microphone off is the command that has to work when the
    /// transcript is otherwise a mess, so it is matched first.
    #[test]
    fn stop_wins_over_every_other_command() {
        assert_eq!(interpret("scratch that, stop listening"), Spoken::Stop);
    }

    // ---- ordinary dictation ----

    #[test]
    fn plain_speech_is_just_text() {
        assert_eq!(
            text("i-refactor natin yung parser ngayon"),
            "i-refactor natin yung parser ngayon"
        );
    }

    #[test]
    fn an_empty_transcript_is_nothing() {
        assert_eq!(interpret("   "), Spoken::Nothing);
    }

    /// Casing is preserved: the prompt should read as it was said.
    #[test]
    fn the_kept_text_is_not_flattened_to_lowercase() {
        assert_eq!(send("Refactor the Parser, go ahead"), "Refactor the Parser");
    }

    /// A word that merely starts with a command is not that command.
    #[test]
    fn a_longer_word_containing_a_phrase_is_not_a_command() {
        assert_eq!(text("sigena"), "sigena");
    }
}
