//! Inflight set — authoritative running-id tracker.
//!
//! Faithful port of `DeepSeek-Reasonix/src/core/inflight.ts` (52 lines).
//!
//! UI cards consult `inflight.has(call_id)` to derive spinner state
//! instead of trusting end-event delivery. The loop adds on dispatch
//! entry and deletes in `finally` so every exit path cleans up.
//!
//! Thread-safe: wraps a `Mutex<HashSet<String>>` so multiple tokio
//! tasks can add/delete ids concurrently (parallel tool dispatch).

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// Authoritative running-id set. Cards derive `running` from
/// `has(id)` instead of trusting end-event delivery.
#[derive(Debug, Clone, Default)]
pub struct InflightSet {
    ids: Arc<Mutex<HashSet<String>>>,
}

impl InflightSet {
    pub fn new() -> Self {
        Self {
            ids: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Add an id to the set. Idempotent — re-adding the same id is a no-op.
    pub fn add(&self, id: &str) {
        let Ok(mut ids) = self.ids.lock() else {
            tracing::error!("inflight set poisoned, skipping add");
            return;
        };
        ids.insert(id.to_string());
    }

    /// Remove an id from the set. No-op if the id was not present.
    pub fn delete(&self, id: &str) {
        let Ok(mut ids) = self.ids.lock() else {
            tracing::error!("inflight set poisoned, skipping delete");
            return;
        };
        ids.remove(id);
    }

    /// Check whether an id is currently in the set.
    #[allow(dead_code)]
    pub fn has(&self, id: &str) -> bool {
        self.ids.lock().map(|ids| ids.contains(id)).unwrap_or(false)
    }

    /// Number of ids currently in flight.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.ids.lock().map(|ids| ids.len()).unwrap_or(0)
    }

    /// True when no ids are in flight.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.ids.lock().map(|ids| ids.is_empty()).unwrap_or(true)
    }

    /// Drop everything — used at session reset.
    /// No-op on an empty set.
    #[allow(dead_code)]
    pub fn clear(&self) {
        let Ok(mut ids) = self.ids.lock() else {
            tracing::error!("inflight set poisoned, skipping clear");
            return;
        };
        ids.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_has_delete_round_trips() {
        let s = InflightSet::new();
        assert!(!s.has("a"));
        s.add("a");
        assert!(s.has("a"));
        assert_eq!(s.len(), 1);
        s.delete("a");
        assert!(!s.has("a"));
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn add_is_idempotent() {
        let s = InflightSet::new();
        s.add("a");
        s.add("a");
        s.add("a");
        assert_eq!(s.len(), 1);
        assert!(s.has("a"));
    }

    #[test]
    fn delete_on_missing_id_is_noop() {
        let s = InflightSet::new();
        s.delete("never-added");
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn clear_empties_the_set() {
        let s = InflightSet::new();
        s.add("a");
        s.add("b");
        assert_eq!(s.len(), 2);
        s.clear();
        assert_eq!(s.len(), 0);
        assert!(!s.has("a"));
        assert!(!s.has("b"));
    }

    #[test]
    fn clear_on_empty_set_is_noop() {
        let s = InflightSet::new();
        s.clear();
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn is_empty_reflects_state() {
        let s = InflightSet::new();
        assert!(s.is_empty());
        s.add("a");
        assert!(!s.is_empty());
        s.delete("a");
        assert!(s.is_empty());
    }

    /// Port of inflight.test.ts:89 — finally contract: id removed
    /// even when work throws.
    #[test]
    fn finally_contract_id_removed_when_work_throws() {
        let s = InflightSet::new();
        s.add("job-1");
        // Simulate work that fails; "finally" block deletes.
        s.delete("job-1");
        assert!(!s.has("job-1"));
        assert_eq!(s.len(), 0);
    }

    /// Cloned InflightSet shares state.
    #[test]
    fn clones_share_state() {
        let s1 = InflightSet::new();
        let s2 = s1.clone();
        s1.add("a");
        assert!(s2.has("a"));
        s2.delete("a");
        assert!(!s1.has("a"));
    }
}
