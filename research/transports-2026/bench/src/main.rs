//! Two experiments, run against real code rather than asserted in prose.
//!
//! 1. **GitHub webhook signatures.** Compute `X-Hub-Signature-256` the way
//!    GitHub documents it, verify it in constant time, and demonstrate the trap
//!    that makes this fail in production: verifying anything other than the
//!    exact bytes on the wire.
//! 2. **Telegram MarkdownV2 escaping.** Implement the documented rule verbatim
//!    and show what breaks when each clause of it is dropped.
//!
//! `cargo run` prints the findings; `cargo test` asserts them.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

// ---------------------------------------------------------------------------
// Experiment 1 — GitHub webhook signatures
// ---------------------------------------------------------------------------

pub mod github_sig {
    use super::*;

    type HmacSha256 = Hmac<Sha256>;

    /// Produce the header value GitHub would send. Only a test needs this;
    /// production only ever verifies.
    pub fn sign(secret: &[u8], raw_body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC takes a key of any length");
        mac.update(raw_body);
        let mut out = String::with_capacity(7 + 64);
        out.push_str("sha256=");
        for b in mac.finalize().into_bytes() {
            out.push_str(&format!("{b:02x}"));
        }
        out
    }

    /// The whole verification, in the shape the receiver should use.
    ///
    /// `raw_body` must be the bytes as they arrived. Not a re-serialised
    /// `serde_json::Value`, not a trimmed string, not a lossy UTF-8 conversion.
    pub fn verify(secret: &[u8], raw_body: &[u8], header: Option<&str>) -> bool {
        let Some(header) = header else { return false };
        let Some(hex) = header.strip_prefix("sha256=") else {
            return false;
        };
        let Some(presented) = decode_hex(hex) else {
            return false;
        };
        let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC takes a key of any length");
        mac.update(raw_body);
        // `verify_slice` is already constant time — it compares through
        // `CtOutput`. The explicit `ct_eq` below is the same guarantee written
        // out, for the version that needs the digest for other reasons.
        let expected = mac.finalize().into_bytes();
        expected.ct_eq(&presented).into()
    }

    /// The same check via the `Mac` trait's own comparison, which is the
    /// idiomatic form and needs no `subtle` import.
    pub fn verify_idiomatic(secret: &[u8], raw_body: &[u8], header: Option<&str>) -> bool {
        let Some(hex) = header.and_then(|h| h.strip_prefix("sha256=")) else {
            return false;
        };
        let Some(presented) = decode_hex(hex) else {
            return false;
        };
        let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC takes a key of any length");
        mac.update(raw_body);
        mac.verify_slice(&presented).is_ok()
    }

    /// The wrong way, kept here only so the experiment can measure it.
    pub fn verify_naive_string_eq(secret: &[u8], raw_body: &[u8], header: Option<&str>) -> bool {
        header.map(|h| h == sign(secret, raw_body)).unwrap_or(false)
    }

    fn decode_hex(s: &str) -> Option<Vec<u8>> {
        if s.len() % 2 != 0 {
            return None;
        }
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Experiment 2 — Telegram MarkdownV2
// ---------------------------------------------------------------------------

pub mod markdown_v2 {
    /// The 18 characters the Bot API lists under "In all other places", plus
    /// the backslash, which the preceding clause makes mandatory:
    ///
    /// > Any character with code between 1 and 126 inclusively can be escaped
    /// > anywhere with a preceding '\' character … This implies that '\'
    /// > character usually must be escaped with a preceding '\' character.
    ///
    /// The backslash is the one everybody forgets, because it is not in the
    /// enumerated list.
    pub const RESERVED: [char; 19] = [
        '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!',
        '\\',
    ];

    /// Escape a plain-text run for `parse_mode=MarkdownV2`.
    pub fn escape(text: &str) -> String {
        let mut out = String::with_capacity(text.len() * 2);
        for c in text.chars() {
            if RESERVED.contains(&c) {
                out.push('\\');
            }
            out.push(c);
        }
        out
    }

    /// Inside a `pre`/`code` entity only these two are special.
    pub fn escape_code(text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 8);
        for c in text.chars() {
            if c == '`' || c == '\\' {
                out.push('\\');
            }
            out.push(c);
        }
        out
    }

    /// Inside the `(...)` of an inline link only these two are special.
    pub fn escape_link_url(url: &str) -> String {
        let mut out = String::with_capacity(url.len() + 8);
        for c in url.chars() {
            if c == ')' || c == '\\' {
                out.push('\\');
            }
            out.push(c);
        }
        out
    }

    /// Undo `escape`. Exists so the round-trip can be asserted: an escaper that
    /// is not reversible is an escaper that changed the message.
    pub fn unescape(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// `sendMessage.text` is "1-4096 characters after entities parsing", and
    /// entity offsets are counted in UTF-16 code units — so an emoji costs 2.
    pub fn len_utf16(text: &str) -> usize {
        text.chars().map(char::len_utf16).sum()
    }

    pub const LIMIT: usize = 4096;

    /// Split **plain** text into pieces that each fit, before escaping.
    ///
    /// Order matters. Escaping first and splitting after can cut between a
    /// backslash and the character it escapes, which produces a stray escape at
    /// the end of one chunk and an unescaped reserved character at the start of
    /// the next — a 400 from Telegram, or worse, silently mangled text.
    pub fn chunk_plain(text: &str, limit: usize) -> Vec<String> {
        let mut chunks = Vec::new();
        let mut current = String::new();
        let mut current_len = 0usize;
        for line in text.split_inclusive('\n') {
            let line_len = len_utf16(line);
            if line_len > limit {
                // A single line longer than the whole limit still has to go
                // somewhere; fall back to a hard cut on character boundaries.
                if !current.is_empty() {
                    chunks.push(std::mem::take(&mut current));
                    current_len = 0;
                }
                for c in line.chars() {
                    if current_len + c.len_utf16() > limit {
                        chunks.push(std::mem::take(&mut current));
                        current_len = 0;
                    }
                    current.push(c);
                    current_len += c.len_utf16();
                }
                continue;
            }
            if current_len + line_len > limit {
                chunks.push(std::mem::take(&mut current));
                current_len = 0;
            }
            current.push_str(line);
            current_len += line_len;
        }
        if !current.is_empty() {
            chunks.push(current);
        }
        chunks
    }
}

// ---------------------------------------------------------------------------

const SECRET: &[u8] = b"jod-webhook-secret-not-a-real-one";

fn fixture() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fixtures/issues.labeled.json"
    ))
    .expect("fixture payload must exist")
}

fn main() {
    let raw = fixture();
    println!("# Experiment 1 — GitHub `X-Hub-Signature-256`\n");
    println!("fixture: fixtures/issues.labeled.json ({} bytes)", raw.len());
    let header = github_sig::sign(SECRET, &raw);
    println!("signature: {header}");
    println!(
        "verify(raw)                    = {}",
        github_sig::verify(SECRET, &raw, Some(&header))
    );
    println!(
        "verify_idiomatic(raw)          = {}",
        github_sig::verify_idiomatic(SECRET, &raw, Some(&header))
    );

    // The trap. Everything below is a payload a careless handler might feed to
    // the verifier instead of the bytes that arrived.
    let value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    let reserialised = serde_json::to_vec(&value).unwrap();
    println!(
        "\nre-serialised body is {} bytes vs {} on the wire",
        reserialised.len(),
        raw.len()
    );
    println!(
        "verify(serde_json round-trip)  = {}   <-- the trap",
        github_sig::verify(SECRET, &reserialised, Some(&header))
    );
    let trimmed = raw
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect::<Vec<u8>>();
    println!(
        "verify(whitespace stripped)    = {}",
        github_sig::verify(SECRET, &trimmed, Some(&header))
    );
    let mut flipped = raw.clone();
    let last = flipped.len() - 2;
    flipped[last] ^= 0x01;
    println!(
        "verify(one bit flipped)        = {}",
        github_sig::verify(SECRET, &flipped, Some(&header))
    );
    println!(
        "verify(wrong secret)           = {}",
        github_sig::verify(b"wrong", &raw, Some(&header))
    );
    println!(
        "verify(no header)              = {}",
        github_sig::verify(SECRET, &raw, None)
    );
    println!(
        "verify(sha1= prefix)           = {}",
        github_sig::verify(SECRET, &raw, Some(&header.replace("sha256=", "sha1=")))
    );

    println!("\n# Experiment 2 — Telegram MarkdownV2\n");
    let samples = [
        "run #42 failed - see docs/jod-api.md (line 3.14!)",
        "cost: $1.20 [+2 files] {ok}",
        "a literal backslash: C:\\Users\\jod",
        "emoji cost 2 UTF-16 units each: 🚀🚀",
    ];
    for s in samples {
        let e = markdown_v2::escape(s);
        println!("plain:   {s}");
        println!("escaped: {e}");
        println!(
            "round-trips: {}  len_utf16 plain={} escaped={}\n",
            markdown_v2::unescape(&e) == s,
            markdown_v2::len_utf16(s),
            markdown_v2::len_utf16(&e)
        );
    }

    let long: String = (0..900)
        .map(|i| format!("line {i}: agent said something about run-42.\n"))
        .collect();
    let chunks = markdown_v2::chunk_plain(&long, markdown_v2::LIMIT);
    println!(
        "chunking {} UTF-16 units -> {} chunks, max chunk {} units, all <= {}: {}",
        markdown_v2::len_utf16(&long),
        chunks.len(),
        chunks
            .iter()
            .map(|c| markdown_v2::len_utf16(c))
            .max()
            .unwrap(),
        markdown_v2::LIMIT,
        chunks
            .iter()
            .all(|c| markdown_v2::len_utf16(c) <= markdown_v2::LIMIT)
    );
    let rejoined: String = chunks.concat();
    println!("chunks rejoin to the original: {}", rejoined == long);

    // Escape-then-split, the wrong order, on a boundary chosen to land inside
    // an escape pair.
    let text = format!("{}.", "x".repeat(4095));
    let escaped = markdown_v2::escape(&text);
    let bad_split = &escaped[..4096];
    println!(
        "escape-then-split leaves a dangling backslash at the cut: {}",
        bad_split.ends_with('\\')
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signature_over_the_raw_body_verifies() {
        let raw = fixture();
        let header = github_sig::sign(SECRET, &raw);
        assert!(header.starts_with("sha256="));
        assert_eq!(header.len(), 7 + 64);
        assert!(github_sig::verify(SECRET, &raw, Some(&header)));
        assert!(github_sig::verify_idiomatic(SECRET, &raw, Some(&header)));
    }

    /// The whole reason the receiver must take `Bytes`, not `Json<Value>`.
    #[test]
    fn a_reserialised_body_does_not_verify() {
        let raw = fixture();
        let header = github_sig::sign(SECRET, &raw);
        let value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        let reserialised = serde_json::to_vec(&value).unwrap();
        assert_ne!(raw, reserialised, "fixture must not be canonically encoded");
        assert!(
            !github_sig::verify(SECRET, &reserialised, Some(&header)),
            "parsing before verifying would have silently passed"
        );
    }

    #[test]
    fn one_flipped_bit_fails() {
        let raw = fixture();
        let header = github_sig::sign(SECRET, &raw);
        let mut flipped = raw.clone();
        let last = flipped.len() - 2;
        flipped[last] ^= 0x01;
        assert!(!github_sig::verify(SECRET, &flipped, Some(&header)));
    }

    #[test]
    fn a_wrong_secret_fails() {
        let raw = fixture();
        let header = github_sig::sign(SECRET, &raw);
        assert!(!github_sig::verify(b"not-the-secret", &raw, Some(&header)));
    }

    #[test]
    fn a_malformed_header_is_a_refusal_not_a_panic() {
        let raw = fixture();
        for header in [
            None,
            Some(""),
            Some("sha256="),
            Some("sha256=zz"),
            Some("sha1=abcdef"),
            Some("abcdef"),
            // Odd length, and a prefix of a valid signature.
            Some("sha256=abc"),
            // Even length but too short: `subtle`'s slice comparison must
            // refuse on the length rather than compare a prefix.
            Some("sha256=abcd"),
            Some("sha256=00"),
        ] {
            assert!(
                !github_sig::verify(SECRET, &raw, header),
                "{header:?} was accepted"
            );
        }
    }

    /// GitHub's own worked example, from the docs. Anchors this implementation
    /// to the published algorithm rather than to itself.
    #[test]
    fn it_matches_githubs_documented_example() {
        // docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries
        let secret = b"It's a Secret to Everybody";
        let body = b"Hello, World!";
        assert_eq!(
            github_sig::sign(secret, body),
            "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17"
        );
    }

    #[test]
    fn every_documented_reserved_character_is_escaped() {
        for c in markdown_v2::RESERVED {
            let s = format!("a{c}b");
            let e = markdown_v2::escape(&s);
            assert!(e.contains(&format!("\\{c}")), "{c:?} was not escaped: {e}");
            assert_eq!(markdown_v2::unescape(&e), s);
        }
    }

    /// The clause outside the enumerated list. A escaper built by copying the
    /// 18-character list misses this one and mangles every Windows path.
    #[test]
    fn the_backslash_is_escaped_even_though_the_list_omits_it() {
        assert_eq!(markdown_v2::escape(r"C:\jod"), r"C:\\jod");
        assert_eq!(markdown_v2::unescape(r"C:\\jod"), r"C:\jod");
    }

    #[test]
    fn code_and_link_contexts_escape_less_not_more() {
        assert_eq!(markdown_v2::escape_code("a.b-c `x` \\y"), r"a.b-c \`x\` \\y");
        assert_eq!(
            markdown_v2::escape_link_url("https://x/a(b)c"),
            r"https://x/a(b\)c"
        );
    }

    #[test]
    fn escaping_is_lossless_for_arbitrary_text() {
        let cases = [
            "",
            "plain",
            "*.*",
            "!!!",
            "a\nb",
            "🚀 launch (now) — 100% done!",
            r"\\\\",
            "```rust\nfn main(){}\n```",
        ];
        for c in cases {
            assert_eq!(markdown_v2::unescape(&markdown_v2::escape(c)), c, "{c:?}");
        }
    }

    #[test]
    fn an_emoji_costs_two_utf16_units() {
        assert_eq!(markdown_v2::len_utf16("🚀"), 2);
        assert_eq!(markdown_v2::len_utf16("a"), 1);
        // The naive `.len()` (bytes) and `.chars().count()` both disagree with
        // what Telegram counts.
        assert_eq!("🚀".len(), 4);
        assert_eq!("🚀".chars().count(), 1);
    }

    #[test]
    fn chunks_fit_and_reassemble() {
        let long: String = (0..900)
            .map(|i| format!("line {i}: something happened.\n"))
            .collect();
        let chunks = markdown_v2::chunk_plain(&long, markdown_v2::LIMIT);
        assert!(chunks.len() > 1);
        for c in &chunks {
            assert!(markdown_v2::len_utf16(c) <= markdown_v2::LIMIT);
        }
        assert_eq!(chunks.concat(), long);
    }

    #[test]
    fn a_single_overlong_line_is_still_split() {
        let long = "x".repeat(10_000);
        let chunks = markdown_v2::chunk_plain(&long, markdown_v2::LIMIT);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks.concat(), long);
    }

    #[test]
    fn chunking_never_splits_a_multi_unit_character() {
        let long = "🚀".repeat(4000);
        let chunks = markdown_v2::chunk_plain(&long, markdown_v2::LIMIT);
        for c in &chunks {
            assert!(markdown_v2::len_utf16(c) <= markdown_v2::LIMIT);
            assert!(c.chars().all(|ch| ch == '🚀'));
        }
        assert_eq!(chunks.concat(), long);
    }

    /// Split first, escape second. The other order cuts escape pairs in half.
    #[test]
    fn splitting_after_escaping_would_dangle_a_backslash() {
        let text = format!("{}.", "x".repeat(4095));
        let escaped = markdown_v2::escape(&text);
        assert!(escaped[..markdown_v2::LIMIT].ends_with('\\'));
        // The correct order produces chunks that each survive escaping.
        for chunk in markdown_v2::chunk_plain(&text, markdown_v2::LIMIT) {
            let e = markdown_v2::escape(&chunk);
            assert_eq!(markdown_v2::unescape(&e), chunk);
        }
    }
}
