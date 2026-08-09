//! Browser sessions: a cookie a `EventSource` can carry.
//!
//! This exists for exactly one reason. `EventSource` cannot set an
//! `Authorization` header — the one real constraint SSE imposes. The way out is
//! either to re-implement SSE over `fetch` (giving back the automatic reconnect
//! and `Last-Event-ID` resume that SSE was chosen for) or to put the credential
//! somewhere the browser sends on its own. A cookie is that somewhere.
//!
//! `POST /v1/session` trades a bearer token for an `HttpOnly` cookie. Bearer
//! auth keeps working everywhere, so curl, the CLI and native mobile clients
//! never need a cookie.
//!
//! Sessions live in memory only. A daemon restart signs every browser out,
//! which for a credential that can execute code is a feature.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::auth::{hash_token, Scope};

pub const COOKIE_NAME: &str = "jod_session";

/// Default session lifetime. Shorter than a typical web app's because this
/// credential spawns processes on a server.
pub const DEFAULT_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone)]
pub struct Session {
    pub label: String,
    pub scope: Scope,
    pub expires_at_ms: i64,
}

#[derive(Default)]
pub struct SessionStore {
    /// Keyed by the SHA-256 of the session id, so the store holds no usable
    /// credential — the same reason the token file holds digests.
    sessions: Mutex<HashMap<String, Session>>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint a session id. Returned once, in a `Set-Cookie`, and never stored.
    pub fn create(&self, label: &str, scope: Scope, now_ms: i64, ttl_ms: i64) -> String {
        let id = crate::auth::generate_token();
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        prune(&mut sessions, now_ms);
        sessions.insert(
            hash_token(&id),
            Session {
                label: label.to_string(),
                scope,
                expires_at_ms: now_ms.saturating_add(ttl_ms),
            },
        );
        id
    }

    /// Resolve a cookie value, if it is live.
    pub fn get(&self, presented: &str, now_ms: i64) -> Option<Session> {
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        prune(&mut sessions, now_ms);
        sessions.get(&hash_token(presented)).cloned()
    }

    pub fn revoke(&self, presented: &str) -> bool {
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.remove(&hash_token(presented)).is_some()
    }

    pub fn len(&self) -> usize {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn prune(sessions: &mut HashMap<String, Session>, now_ms: i64) {
    sessions.retain(|_, s| s.expires_at_ms > now_ms);
}

/// Pull our cookie out of a `Cookie` header.
///
/// Hand-rolled rather than pulling a cookie crate in for one lookup. Values are
/// hex session ids, so no quoting or percent-decoding is in play.
pub fn session_from_cookie_header(header: Option<&str>) -> Option<&str> {
    let header = header?;
    header.split(';').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name.trim() == COOKIE_NAME).then(|| value.trim())
    })
}

/// Build the `Set-Cookie` value.
///
/// `HttpOnly` keeps it away from any script (including an injected one),
/// `SameSite=Strict` is the CSRF defence, and `Secure` is safe to set
/// unconditionally: production is HTTPS via Tailscale, and browsers treat
/// `http://localhost` as a secure context for dev.
pub fn set_cookie_value(id: &str, ttl_ms: i64) -> String {
    format!(
        "{COOKIE_NAME}={id}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age={}",
        ttl_ms / 1000
    )
}

/// The `Set-Cookie` that clears it.
pub fn clear_cookie_value() -> String {
    format!("{COOKIE_NAME}=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_created_session_resolves() {
        let store = SessionStore::new();
        let id = store.create("web", Scope::Read, 0, DEFAULT_TTL_MS);
        let s = store.get(&id, 1000).expect("session should resolve");
        assert_eq!(s.label, "web");
        assert_eq!(s.scope, Scope::Read);
    }

    #[test]
    fn an_unknown_cookie_does_not_resolve() {
        let store = SessionStore::new();
        store.create("web", Scope::Read, 0, DEFAULT_TTL_MS);
        assert!(store.get("jod_nonsense", 0).is_none());
    }

    #[test]
    fn a_session_expires() {
        let store = SessionStore::new();
        let id = store.create("web", Scope::Read, 0, 1000);
        assert!(store.get(&id, 999).is_some());
        assert!(store.get(&id, 1001).is_none());
    }

    #[test]
    fn expiry_evicts_so_the_map_cannot_grow_forever() {
        let store = SessionStore::new();
        store.create("web", Scope::Read, 0, 1000);
        let _ = store.get("anything", 2000);
        assert!(store.is_empty(), "expired session was not evicted");
    }

    #[test]
    fn revoking_signs_that_browser_out() {
        let store = SessionStore::new();
        let id = store.create("web", Scope::Write, 0, DEFAULT_TTL_MS);
        assert!(store.revoke(&id));
        assert!(store.get(&id, 0).is_none());
        assert!(
            !store.revoke(&id),
            "revoking twice should report nothing to do"
        );
    }

    #[test]
    fn the_session_id_is_never_stored_in_the_clear() {
        let store = SessionStore::new();
        let id = store.create("web", Scope::Read, 0, DEFAULT_TTL_MS);
        let sessions = store.sessions.lock().unwrap();
        assert!(
            !sessions.contains_key(&id),
            "the session id is its own key — it should be hashed"
        );
        assert!(sessions.contains_key(&hash_token(&id)));
    }

    #[test]
    fn the_cookie_is_found_among_others() {
        assert_eq!(
            session_from_cookie_header(Some("theme=dark; jod_session=abc123; other=1")),
            Some("abc123")
        );
        assert_eq!(
            session_from_cookie_header(Some("jod_session=abc")),
            Some("abc")
        );
        assert_eq!(session_from_cookie_header(Some("theme=dark")), None);
        assert_eq!(session_from_cookie_header(None), None);
    }

    #[test]
    fn a_similarly_named_cookie_is_not_mistaken_for_ours() {
        assert_eq!(
            session_from_cookie_header(Some("not_jod_session=abc")),
            None,
            "a suffix match would let another cookie impersonate the session"
        );
    }

    #[test]
    fn the_cookie_carries_every_hardening_flag() {
        let v = set_cookie_value("abc", DEFAULT_TTL_MS);
        for flag in ["HttpOnly", "Secure", "SameSite=Strict", "Path=/"] {
            assert!(v.contains(flag), "cookie is missing {flag}: {v}");
        }
        assert!(v.contains("Max-Age=604800"));
    }

    #[test]
    fn clearing_expires_the_cookie_immediately() {
        assert!(clear_cookie_value().contains("Max-Age=0"));
    }
}
