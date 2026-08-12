//! Scrubbing secret values out of a harness's output.
//!
//! Lives in core rather than in the supervisor because it has to be testable
//! on its own — this is the code standing between a credential and a permanent
//! record of it, and "we ran it and it looked fine" is not evidence.
//!
//! The supervisor is the only process holding both the values it injected and
//! the lines the child printed, so it is the only place this can run. It runs
//! **before parsing**: a value scrubbed after the JSON is decoded has already
//! been through a parser, and a parser is a thing that can log.

/// What replaces a secret in the output.
///
/// Visible on purpose. A silently removed value looks like a harness bug; a
/// marker tells whoever is reading the transcript that Jod did this and why.
pub const MARKER: &str = "[redacted]";

/// Replaces known secret values wherever they appear in a line.
///
/// Built once per run from the values the supervisor actually injected, so
/// injection and redaction can never disagree about what is secret.
#[derive(Debug, Clone, Default)]
pub struct Scrubber {
    /// Longest first. Order matters: if one secret contains another, scrubbing
    /// the short one first would leave a recognisable fragment of the long one
    /// behind, spliced around a marker.
    values: Vec<String>,
}

impl Scrubber {
    /// Build from raw values. Anything too short to redact safely is dropped
    /// here rather than at the call site, so a caller cannot accidentally
    /// include one.
    pub fn new(values: impl IntoIterator<Item = String>) -> Scrubber {
        let mut values: Vec<String> = values
            .into_iter()
            .filter(|v| v.len() >= crate::secrets::MIN_REDACTABLE_LEN)
            .collect();
        values.sort_by_key(|v| std::cmp::Reverse(v.len()));
        values.dedup();
        Scrubber { values }
    }

    /// Whether this scrubber would change anything, so the hot path can skip
    /// the work when no secrets are in play — which is almost every run.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Replace every occurrence of every known value.
    pub fn scrub(&self, line: &str) -> String {
        if self.values.is_empty() {
            return line.to_string();
        }
        let mut out = line.to_string();
        for v in &self.values {
            if out.contains(v.as_str()) {
                out = out.replace(v.as_str(), MARKER);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_is_replaced_wherever_it_appears() {
        let s = Scrubber::new(["sk-abcdef123456".to_string()]);
        let line = r#"{"text":"the key is sk-abcdef123456, twice: sk-abcdef123456"}"#;
        let out = s.scrub(line);
        assert!(!out.contains("sk-abcdef123456"));
        assert_eq!(out.matches(MARKER).count(), 2);
    }

    #[test]
    fn a_short_value_is_not_redacted_because_it_would_mangle_ordinary_output() {
        // "test" would match half of every transcript. It is injected and left
        // alone, and the rail is what tells the user that happened.
        let s = Scrubber::new(["test".to_string()]);
        assert!(s.is_empty());
        assert_eq!(s.scrub("this is a test"), "this is a test");
    }

    #[test]
    fn the_longer_secret_is_scrubbed_first_so_no_fragment_survives() {
        // If "abcdefgh" were replaced first, the longer value would be left as
        // "[redacted]ijkl" — a recognisable tail of a live credential.
        let s = Scrubber::new(["abcdefgh".to_string(), "abcdefghijkl".to_string()]);
        let out = s.scrub("token=abcdefghijkl");
        assert_eq!(out, format!("token={MARKER}"));
    }

    #[test]
    fn an_empty_scrubber_leaves_the_line_exactly_alone() {
        let s = Scrubber::default();
        assert!(s.is_empty());
        assert_eq!(s.scrub("nothing to do here"), "nothing to do here");
    }
}
