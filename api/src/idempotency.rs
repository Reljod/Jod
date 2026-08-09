//! Remembering that a spawn already happened.
//!
//! A phone on a flaky connection retries a POST it never saw a response to.
//! Without a key, that retry spawns a *second* agent: double the spend, double
//! the work, and two agents editing one worktree. With one, the retry returns
//! the agent the first attempt created.
//!
//! Keys are scoped to the token that used them, so two devices cannot collide
//! by both sending `Idempotency-Key: 1`.

use std::collections::HashMap;
use std::sync::Mutex;

/// How long a key is honoured. Long enough to cover a phone that reconnects
/// after a night in a tunnel; short enough that the map cannot grow forever.
pub const TTL_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone)]
struct Entry {
    agent_id: String,
    at_ms: i64,
}

#[derive(Default)]
pub struct IdempotencyCache {
    entries: Mutex<HashMap<(String, String), Entry>>,
}

impl IdempotencyCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The agent a previous identical request created, if it is still remembered.
    pub fn get(&self, token_label: &str, key: &str, now_ms: i64) -> Option<String> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        prune(&mut entries, now_ms);
        entries
            .get(&(token_label.to_string(), key.to_string()))
            .map(|e| e.agent_id.clone())
    }

    pub fn put(&self, token_label: &str, key: &str, agent_id: &str, now_ms: i64) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        prune(&mut entries, now_ms);
        entries.insert(
            (token_label.to_string(), key.to_string()),
            Entry {
                agent_id: agent_id.to_string(),
                at_ms: now_ms,
            },
        );
    }
}

fn prune(entries: &mut HashMap<(String, String), Entry>, now_ms: i64) {
    entries.retain(|_, e| now_ms.saturating_sub(e.at_ms) < TTL_MS);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repeated_key_returns_the_original_agent() {
        let c = IdempotencyCache::new();
        c.put("phone", "k1", "agent-1", 0);
        assert_eq!(c.get("phone", "k1", 1000).as_deref(), Some("agent-1"));
    }

    #[test]
    fn an_unseen_key_is_a_miss() {
        let c = IdempotencyCache::new();
        c.put("phone", "k1", "agent-1", 0);
        assert!(c.get("phone", "k2", 0).is_none());
    }

    #[test]
    fn keys_do_not_collide_across_tokens() {
        let c = IdempotencyCache::new();
        c.put("phone", "1", "agent-phone", 0);
        c.put("laptop", "1", "agent-laptop", 0);
        assert_eq!(c.get("phone", "1", 0).as_deref(), Some("agent-phone"));
        assert_eq!(c.get("laptop", "1", 0).as_deref(), Some("agent-laptop"));
    }

    #[test]
    fn a_key_expires_so_the_map_cannot_grow_forever() {
        let c = IdempotencyCache::new();
        c.put("phone", "k1", "agent-1", 0);
        assert!(c.get("phone", "k1", TTL_MS - 1).is_some());
        assert!(c.get("phone", "k1", TTL_MS + 1).is_none());
    }

    #[test]
    fn expiry_evicts_rather_than_merely_hiding() {
        let c = IdempotencyCache::new();
        c.put("phone", "k1", "agent-1", 0);
        // A later unrelated read past the TTL must drop the stale entry.
        let _ = c.get("phone", "other", TTL_MS + 1);
        let entries = c.entries.lock().unwrap();
        assert!(entries.is_empty(), "expired entry was not evicted");
    }
}
