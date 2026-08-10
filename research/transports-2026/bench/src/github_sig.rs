//! GitHub webhook signature verification, in the shape the receiver should use.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

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

/// The whole verification.
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
    // `CtOutput`. The explicit `ct_eq` is the same guarantee written out.
    let expected = mac.finalize().into_bytes();
    expected.ct_eq(&presented).into()
}

/// The same check via the `Mac` trait's own comparison, which is idiomatic and
/// needs no `subtle` import.
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

/// The wrong way, kept only so the timing experiment can measure it. A
/// byte-by-byte loop that returns on the first mismatch leaks how many leading
/// bytes were correct.
pub fn naive_early_return_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    for i in 0..a.len() {
        if a[i] != b[i] {
            return false;
        }
    }
    true
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.ct_eq(b).into()
}

pub fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}
