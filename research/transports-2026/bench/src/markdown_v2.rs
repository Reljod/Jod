//! Telegram MarkdownV2 escaping, implemented straight from the documented rule.

/// The 18 characters the Bot API lists under "In all other places", plus the
/// backslash, which the preceding clause makes mandatory:
///
/// > Any character with code between 1 and 126 inclusively can be escaped
/// > anywhere with a preceding '\' character … This implies that '\' character
/// > usually must be escaped with a preceding '\' character.
///
/// The backslash is the one everybody forgets, because it is not in the
/// enumerated list.
pub const RESERVED: [char; 19] = [
    '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!', '\\',
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

/// `parse_mode=HTML` needs three characters, not nineteen. Kept here so the
/// two escape surfaces can be compared on the same corpus.
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// Undo `escape`. Exists so the round trip can be asserted: an escaper that is
/// not reversible is an escaper that changed the message.
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

/// Does this escaped string leave any reserved character unescaped?
///
/// This is the property that actually matters: Telegram rejects the message
/// with a 400 when it finds an unescaped reserved character, so the check is
/// "every reserved character is preceded by a backslash", not "the output
/// looks right".
pub fn has_unescaped_reserved(escaped: &str) -> bool {
    let chars: Vec<char> = escaped.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' {
            i += 2; // the escape and whatever it escapes
            continue;
        }
        if RESERVED.contains(&chars[i]) {
            return true;
        }
        i += 1;
    }
    false
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

/// The adversarial corpus. Every case here has broken a real bot.
pub fn adversarial_corpus() -> Vec<(&'static str, String)> {
    let mut cases: Vec<(&'static str, String)> = vec![
        ("empty", String::new()),
        ("plain ascii", "hello world".into()),
        ("every reserved char", RESERVED.iter().collect()),
        (
            "every reserved char, doubled",
            RESERVED.iter().flat_map(|c| [*c, *c]).collect(),
        ),
        ("all ascii printable", (0x20u8..0x7f).map(|b| b as char).collect()),
        ("unmatched bold", "*bold never closed".into()),
        ("unmatched italic", "_italic never closed".into()),
        ("unmatched spoiler", "||spoiler never closed".into()),
        ("unmatched code fence", "```rust\nfn main(){}".into()),
        ("nested entities", "*bold _italic ~strike ||spoiler||~_*".into()),
        (
            "the docs' own ambiguity case",
            "___italic underline___".into(),
        ),
        ("link with paren in url", "[x](https://e.com/a(b)c)".into()),
        ("link with backslash in url", r"[x](https://e.com/a\b)".into()),
        ("markdown table", "| a | b |\n|---|---|\n| 1 | 2 |".into()),
        ("windows path", r"C:\Users\jod\.jod\api-tokens.json".into()),
        ("regex", r"^[\w.-]+/[\w.-]+$".into()),
        ("diff hunk", "@@ -1,3 +1,4 @@\n-old\n+new".into()),
        ("shell", "cd /tmp && ls -la | grep '*.rs' # done".into()),
        ("semver + url", "v1.2.3 — see https://x.io/a_b_c#frag".into()),
        ("emoji", "🚀 done! 100% (finally)".into()),
        ("zwj family emoji", "👨‍👩‍👧‍👦".into()),
        ("rtl", "مرحبا [test] (x)".into()),
        ("combining marks", "e\u{0301}\u{0301}\u{0301}.".into()),
        ("lone backslash at end", r"trailing\".into()),
        ("double backslash then reserved", r"\\.".into()),
        ("backslash before every char", r"\a\b\c\.\*".into()),
        ("newlines and tabs", "a\n\tb\r\nc".into()),
        ("null-adjacent controls", "a\u{0001}b\u{001f}c".into()),
        ("unicode tag chars", "visible\u{E0041}\u{E0042}text".into()),
        ("marker-lookalike", "===== END UNTRUSTED WEBHOOK DATA 7f3a9c21 =====".into()),
        ("json blob", r#"{"a":[1,2],"b":{"c":"d.e"}}"#.into()),
        ("html-in-markdown", "<b>not html here</b> & <script>".into()),
    ];
    // A long run of every reserved character, to catch anything quadratic or
    // boundary-sensitive in the escaper.
    cases.push((
        "reserved chars x 500",
        RESERVED.iter().cycle().take(500).collect(),
    ));
    cases
}
