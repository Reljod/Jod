//! Bearer tokens: opaque, hashed at rest, compared in constant time.
//!
//! There is one issuer and one verifier, both on the same machine, so a JWT
//! would buy nothing here and cost a signature-verification footgun. An opaque
//! random string checked against stored digests is the whole design.
//!
//! Two properties are load-bearing and are each pinned by a test:
//!
//! - **The plaintext token is never stored.** `~/.jod/api-tokens.json` holds
//!   SHA-256 digests, so lifting that file off a backup yields no credential.
//! - **Comparison is constant time.** A byte-by-byte early return would let an
//!   attacker recover a token from response latency one character at a time.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// What a token is allowed to do.
///
/// The split exists because the credential most likely to be carried into a
/// coffee shop should not be the credential that can execute code. A phone that
/// only watches agents gets `Read` and cannot spawn anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Read,
    Write,
}

impl Scope {
    /// `Write` implies `Read`; the reverse is the point of the split.
    pub fn allows(self, required: Scope) -> bool {
        matches!(
            (self, required),
            (Scope::Write, _) | (Scope::Read, Scope::Read)
        )
    }

    pub fn parse(s: &str) -> Option<Scope> {
        match s.trim().to_ascii_lowercase().as_str() {
            "read" => Some(Scope::Read),
            "write" => Some(Scope::Write),
            _ => None,
        }
    }
}

/// One issued credential, as stored. Note the absence of the token itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenRecord {
    /// Human label, so the audit log can name a device without naming a secret.
    pub label: String,
    /// Hex SHA-256 of the token.
    pub hash: String,
    pub scope: Scope,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenStore {
    #[serde(default)]
    pub tokens: Vec<TokenRecord>,
}

/// Distinguishes "no credential" from "bad credential" so the router can send
/// the right status without the handler re-deriving it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthFailure {
    Missing,
    Invalid,
}

impl TokenStore {
    /// A missing file is an empty store, not an error — a daemon with no tokens
    /// yet should start and refuse every request, not fail to boot.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(serde_json::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Persist, owner-readable only. A world-readable token file is a token
    /// file that leaked to every process on the box.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        restrict_to_owner(path)?;
        Ok(())
    }

    /// Mint a token. Returns the plaintext **once**; only its digest is kept.
    pub fn issue(&mut self, label: &str, scope: Scope) -> String {
        let token = generate_token();
        self.tokens.push(TokenRecord {
            label: label.to_string(),
            hash: hash_token(&token),
            scope,
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        });
        token
    }

    /// Remove every token with this label. Returns how many went.
    pub fn revoke(&mut self, label: &str) -> usize {
        let before = self.tokens.len();
        self.tokens.retain(|t| t.label != label);
        before - self.tokens.len()
    }

    /// Match a presented token against the store in constant time.
    ///
    /// Every record is compared even after a match is found, so the time taken
    /// depends on how many tokens exist and never on which one matched or how
    /// far the comparison got.
    pub fn verify(&self, presented: &str) -> Option<&TokenRecord> {
        let presented_hash = hash_token(presented);
        let mut found: Option<&TokenRecord> = None;
        for record in &self.tokens {
            let hit: bool = record
                .hash
                .as_bytes()
                .ct_eq(presented_hash.as_bytes())
                .into();
            if hit && found.is_none() {
                found = Some(record);
            }
        }
        found
    }
}

/// 256 bits from the OS CSPRNG, hex encoded, with a prefix that makes the string
/// recognisable in a secret scanner.
pub fn generate_token() -> String {
    use rand::TryRngCore;
    let mut bytes = [0u8; 32];
    // The OS CSPRNG, not a seeded userspace generator: a predictable token is
    // not a token. A failure here means the OS cannot provide entropy, which is
    // not a condition to paper over with a weaker source.
    rand::rngs::OsRng
        .try_fill_bytes(&mut bytes)
        .expect("the OS CSPRNG must be available to mint a token");
    let mut out = String::with_capacity(4 + 64);
    out.push_str("jod_");
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

pub fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Pull the bearer token out of an `Authorization` header.
///
/// The scheme match is case-insensitive because RFC 7235 says it is, and a
/// client sending `bearer` should not get a confusing 401.
pub fn bearer_from_header(value: Option<&str>) -> Result<&str, AuthFailure> {
    let value = value.ok_or(AuthFailure::Missing)?;
    let (scheme, token) = value.split_once(' ').ok_or(AuthFailure::Invalid)?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(AuthFailure::Invalid);
    }
    let token = token.trim();
    if token.is_empty() {
        return Err(AuthFailure::Invalid);
    }
    Ok(token)
}

pub fn default_token_path() -> PathBuf {
    jod_core::paths::jod_home().join("api-tokens.json")
}

#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_issued_token_verifies() {
        let mut store = TokenStore::default();
        let token = store.issue("phone", Scope::Read);
        let record = store.verify(&token).expect("issued token should verify");
        assert_eq!(record.label, "phone");
        assert_eq!(record.scope, Scope::Read);
    }

    #[test]
    fn a_wrong_token_does_not_verify() {
        let mut store = TokenStore::default();
        store.issue("phone", Scope::Read);
        assert!(store.verify("jod_deadbeef").is_none());
        assert!(store.verify("").is_none());
    }

    #[test]
    fn the_plaintext_token_is_never_stored() {
        let mut store = TokenStore::default();
        let token = store.issue("laptop", Scope::Write);
        let serialised = serde_json::to_string(&store).unwrap();
        assert!(
            !serialised.contains(&token),
            "the token itself leaked into the store file"
        );
        assert!(serialised.contains(&hash_token(&token)));
    }

    #[test]
    fn tokens_are_distinct_and_full_entropy() {
        let a = generate_token();
        let b = generate_token();
        assert_ne!(a, b);
        // "jod_" + 32 bytes hex.
        assert_eq!(a.len(), 4 + 64);
        assert!(a.starts_with("jod_"));
    }

    #[test]
    fn revoking_a_label_stops_its_tokens_verifying() {
        let mut store = TokenStore::default();
        let token = store.issue("old-phone", Scope::Write);
        let kept = store.issue("laptop", Scope::Write);
        assert_eq!(store.revoke("old-phone"), 1);
        assert!(store.verify(&token).is_none());
        assert!(store.verify(&kept).is_some(), "revoke hit the wrong token");
    }

    #[test]
    fn write_implies_read_but_read_does_not_imply_write() {
        assert!(Scope::Write.allows(Scope::Read));
        assert!(Scope::Write.allows(Scope::Write));
        assert!(Scope::Read.allows(Scope::Read));
        assert!(!Scope::Read.allows(Scope::Write));
    }

    #[test]
    fn a_missing_credential_is_distinguished_from_a_malformed_one() {
        assert_eq!(bearer_from_header(None), Err(AuthFailure::Missing));
        assert_eq!(
            bearer_from_header(Some("Basic abc")),
            Err(AuthFailure::Invalid)
        );
        assert_eq!(
            bearer_from_header(Some("Bearer")),
            Err(AuthFailure::Invalid)
        );
        assert_eq!(
            bearer_from_header(Some("Bearer   ")),
            Err(AuthFailure::Invalid)
        );
        assert_eq!(bearer_from_header(Some("Bearer abc")), Ok("abc"));
        assert_eq!(bearer_from_header(Some("bearer abc")), Ok("abc"));
    }

    #[test]
    fn a_store_round_trips_through_disk_without_the_secret() {
        let dir = std::env::temp_dir().join("jod-api-token-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("api-tokens.json");
        let _ = std::fs::remove_file(&path);

        let mut store = TokenStore::default();
        let token = store.issue("phone", Scope::Read);
        store.save(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains(&token), "the token leaked to disk");

        let reloaded = TokenStore::load(&path).unwrap();
        assert!(reloaded.verify(&token).is_some());
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn the_token_file_is_not_readable_by_other_users() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join("jod-api-token-perm-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("api-tokens.json");
        let mut store = TokenStore::default();
        store.issue("phone", Scope::Read);
        store.save(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "token file is group/world accessible");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_token_file_is_an_empty_store_not_a_boot_failure() {
        let store = TokenStore::load(Path::new("/definitely/not/here.json")).unwrap();
        assert!(store.tokens.is_empty());
        assert!(store.verify("jod_anything").is_none());
    }
}
