//! Copying something out of the transcript, without the terminal's own
//! selection.
//!
//! Dragging across a pane stops working the moment it has scrollback and
//! wrapping: the selection takes the box-drawing characters, the gutter and the
//! wrap points with it, and a hundred-line reply comes out as a hundred lines
//! of ragged text with `│` down the left. So the copy has to come from the
//! *state*, which is unwrapped and ungarnished, rather than from the screen.
//!
//! ## OSC 52 rather than a clipboard crate
//!
//! The clipboard here is the **terminal's**, reached with an escape sequence
//! the terminal itself acts on. That is not a way of avoiding a dependency; it
//! is the only mechanism that works where Jod actually runs. A clipboard
//! library talks to the X server, the Wayland compositor or the macOS
//! pasteboard *on the machine the process is on* — which, over ssh to the VPS,
//! is the wrong machine, and usually no machine at all. OSC 52 travels back
//! down the same connection to the terminal in front of you.
//!
//! It is not universal — tmux needs `set -g set-clipboard on`, and a few
//! terminals refuse it outright — so the notice says what was copied and how
//! much, which is also the only feedback available: nothing can read the
//! clipboard back to check.

use super::app::Entry;

/// What a yank found, and what to call it.
pub struct Yanked {
    pub text: String,
    /// For the notice. A person who pressed the key needs to know *which* of
    /// the several plausible things it took.
    pub what: &'static str,
}

/// The most useful thing to copy right now.
///
/// The last agent reply, unless that reply is entirely one fenced code block —
/// in which case the block's contents, without the fences. That is not a guess
/// about intent: nobody has ever wanted to paste ```` ``` ```` into a shell,
/// and a reply that is *only* a block is one whose whole payload is the code.
///
/// A reply with prose *around* a block is left whole, because there the prose
/// is doing something and choosing for the reader would be the guess.
pub fn from_transcript(transcript: &[Entry]) -> Option<Yanked> {
    let reply = transcript.iter().rev().find_map(|entry| match entry {
        Entry::Agent(text) if !text.trim().is_empty() => Some(text),
        _ => None,
    })?;

    match sole_code_block(reply) {
        Some(code) => Some(Yanked {
            text: code,
            what: "the code block",
        }),
        None => Some(Yanked {
            text: reply.clone(),
            what: "the last reply",
        }),
    }
}

/// The contents of the one fenced block this text consists of, if that is all
/// it is.
fn sole_code_block(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if !trimmed.starts_with("```") {
        return None;
    }
    let mut lines: Vec<&str> = trimmed.lines().collect();
    // The closing fence has to be the last line, or there is prose after it.
    if lines.len() < 2 || !lines.last()?.trim().starts_with("```") {
        return None;
    }
    lines.pop();
    // Drop the opening fence and whatever language tag rides on it.
    lines.remove(0);
    // A fence with nothing between it is not a code block worth taking; fall
    // back to the whole reply so the key still does something.
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// The escape sequence that puts `text` on the terminal's clipboard.
///
/// `\x1b]52;c;<base64>\x07` — `c` is the clipboard selection, and the payload
/// is base64 because the sequence is terminated by a control character and a
/// newline in the middle of it would truncate the copy at the first line.
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

/// Base64, written out rather than pulled in.
///
/// Twenty lines against a dependency in a workspace manifest that three lanes
/// share — and a manifest edit is not a change one lane should make quietly.
/// The alphabet is the standard one with padding, which is what every terminal
/// implementing OSC 52 expects.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// What the transcript says after a yank.
///
/// The length is in it because nothing can read the clipboard back to confirm:
/// if the terminal silently refused the sequence — tmux without
/// `set-clipboard on`, or one of the terminals that decline it — the only thing
/// that separates "copied" from "did nothing" is that Jod claimed a number.
pub fn note(yanked: &Yanked) -> String {
    let lines = yanked.text.lines().count();
    format!(
        "copied {} — {} {}",
        yanked.what,
        lines,
        if lines == 1 { "line" } else { "lines" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(text: &str) -> Entry {
        Entry::Agent(text.to_string())
    }

    #[test]
    fn the_last_agent_reply_is_what_gets_copied() {
        let transcript = vec![
            agent("the first thing"),
            Entry::You("a question".into()),
            agent("the latest thing"),
            Entry::Notice("something Jod said".into()),
        ];
        let yanked = from_transcript(&transcript).expect("a reply");
        assert_eq!(yanked.text, "the latest thing");
        assert_eq!(yanked.what, "the last reply");
    }

    /// Nobody has ever wanted to paste a fence into a shell.
    #[test]
    fn a_reply_that_is_only_a_code_block_yields_the_code_without_its_fences() {
        let yanked = from_transcript(&[agent("```rust\nfn main() {}\n```")]).expect("a block");
        assert_eq!(yanked.text, "fn main() {}");
        assert_eq!(yanked.what, "the code block");
    }

    /// Prose around a block is doing something, so choosing for the reader
    /// would be the guess.
    #[test]
    fn a_reply_with_prose_around_a_block_is_copied_whole() {
        let text = "Here is the fix:\n```\nlet x = 1;\n```\nApply it and rerun.";
        let yanked = from_transcript(&[agent(text)]).expect("a reply");
        assert_eq!(yanked.text, text);
        assert_eq!(yanked.what, "the last reply");
    }

    #[test]
    fn an_empty_fence_falls_back_to_the_whole_reply() {
        let yanked = from_transcript(&[agent("```\n```")]).expect("a reply");
        assert_eq!(yanked.what, "the last reply");
    }

    #[test]
    fn a_transcript_with_no_agent_reply_yields_nothing() {
        assert!(from_transcript(&[]).is_none());
        assert!(from_transcript(&[Entry::You("only me".into())]).is_none());
        assert!(
            from_transcript(&[agent("   ")]).is_none(),
            "an empty reply is not worth copying"
        );
    }

    /// Checked against the published vectors in RFC 4648, because a
    /// hand-written encoder is exactly the thing that is subtly wrong at the
    /// padding boundaries and looks right everywhere else.
    #[test]
    fn base64_matches_the_rfc_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_handles_text_that_is_not_ascii() {
        assert_eq!(base64("é".as_bytes()), "w6k=");
    }

    /// A newline in the middle of the sequence would truncate the copy at the
    /// first line, which is why the payload is encoded at all.
    #[test]
    fn the_escape_sequence_is_well_formed_and_carries_no_raw_newline() {
        let seq = osc52("one\ntwo");
        assert!(seq.starts_with("\x1b]52;c;"));
        assert!(seq.ends_with('\x07'));
        assert!(!seq[7..seq.len() - 1].contains('\n'));
        assert!(seq.contains("b25lCnR3bw=="));
    }

    /// Nothing can read the clipboard back, so the number is the only thing
    /// separating "copied" from "silently did nothing".
    #[test]
    fn the_notice_says_what_went_and_how_much() {
        let one = note(&Yanked {
            text: "just this".into(),
            what: "the last reply",
        });
        assert_eq!(one, "copied the last reply — 1 line");

        let many = note(&Yanked {
            text: "a\nb\nc".into(),
            what: "the code block",
        });
        assert_eq!(many, "copied the code block — 3 lines");
    }
}
