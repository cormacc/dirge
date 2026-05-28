//! Memory entry usage telemetry sidecar at `.dirge/memory/.usage.json`.
//!
//! dirge-mo0w (audit finding B): parallel to `extras/skills/usage.rs`, but
//! adapted for the memory entry shape — entries don't have names, so they're
//! keyed by a deterministic FNV-1a hash of their content. Per-entry record
//! tracks first/last seen timestamps so the curator can derive entry age
//! and stability across runs.
//!
//! Owned entirely by the curator: the memory tool's write path stays
//! untouched. The curator re-scans MEMORY.md / PITFALLS.md on each run
//! and reconciles the sidecar against current entries. Hashes that
//! disappeared are dropped; new hashes get `first_seen_at = now`;
//! survivors get `last_seen_at = now`.
//!
//! Why FNV-1a 64-bit: needs to be deterministic across processes (rules
//! out `std::collections::hash_map::DefaultHasher` which is randomized);
//! doesn't need crypto strength (entry IDs are local, not auth tokens);
//! pulls no new dependencies.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::extras::dirge_paths::ProjectPaths;

/// Per-entry telemetry record. Smaller than the skills' equivalent
/// because memory entries don't have per-entry events (no
/// `use_count`/`view_count`/`patch_count`): the only signal
/// available is "the entry was present in MEMORY.md at curator
/// run time `T`".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MemoryUsage {
    /// ISO-8601 UTC timestamp when the curator first observed
    /// this entry's content hash.
    pub first_seen_at: String,
    /// ISO-8601 UTC timestamp of the most recent curator run
    /// that still saw this entry.
    pub last_seen_at: String,
    /// Which memory target this entry belongs to. Stored so a
    /// single shared sidecar can carry both `memory` and
    /// `pitfalls` entries without collisions.
    pub target: String,
}

/// Sidecar store at `.dirge/memory/.usage.json`. Keys are FNV-1a
/// 64-bit hashes of entry content rendered as zero-padded hex
/// (16 chars). HashMap stays compact even with thousands of
/// entries.
#[derive(Debug, Clone)]
pub struct MemoryUsageStore {
    path: PathBuf,
    data: HashMap<String, MemoryUsage>,
}

impl MemoryUsageStore {
    /// Load the sidecar from disk, returning an empty store if
    /// the file doesn't exist or is corrupt. Mirrors
    /// `skills::usage::UsageStore::load` semantics: never
    /// blocks curator runs on a malformed sidecar.
    pub fn load(paths: &ProjectPaths) -> Self {
        let path = paths.memory_dir().join(".usage.json");
        let data = if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
                Err(_) => HashMap::new(),
            }
        } else {
            HashMap::new()
        };
        Self { path, data }
    }

    /// Persist the sidecar atomically. Returns the atomic-write
    /// error verbatim — callers can decide whether to log or
    /// surface.
    pub fn save(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create memory usage dir: {e}"))?;
        }
        let content = serde_json::to_string_pretty(&self.data)
            .map_err(|e| format!("serialize memory usage: {e}"))?;
        crate::fs_atomic::atomic_write_sync(&self.path, content.as_bytes())
            .map_err(|e| format!("write memory usage: {e}"))
    }

    /// Reconcile the sidecar against a fresh scan of memory
    /// entries. `entries` is the current `(target, content)`
    /// pairs from MEMORY.md / PITFALLS.md.
    ///
    /// Returns `(added, retained, dropped)` counts for
    /// telemetry / report generation.
    pub fn reconcile(&mut self, entries: &[(&str, &str)], now_iso: &str) -> ReconcileReport {
        let mut current_keys = std::collections::HashSet::new();
        let mut added = 0usize;
        let mut retained = 0usize;
        for (target, content) in entries {
            let key = entry_id(content);
            current_keys.insert(key.clone());
            self.data
                .entry(key)
                .and_modify(|u| {
                    u.last_seen_at = now_iso.to_string();
                    retained += 1;
                })
                .or_insert_with(|| {
                    added += 1;
                    MemoryUsage {
                        first_seen_at: now_iso.to_string(),
                        last_seen_at: now_iso.to_string(),
                        target: (*target).to_string(),
                    }
                });
        }
        let before = self.data.len();
        self.data.retain(|k, _| current_keys.contains(k));
        let dropped = before - self.data.len();
        ReconcileReport {
            added,
            retained,
            dropped,
        }
    }

    /// Get the record for an entry by content (computes the
    /// hash internally so callers don't see the keying scheme).
    pub fn get(&self, content: &str) -> Option<&MemoryUsage> {
        self.data.get(&entry_id(content))
    }

    /// Number of tracked entries. Used by tests + audit
    /// reporting; not on a production hot path.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Path the store reads from / writes to. Exposed for the
    /// curator's report writer.
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Counts returned by `reconcile`. Curator surfaces these in
/// audit reports so each run shows what changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReconcileReport {
    /// New entries observed for the first time this run.
    pub added: usize,
    /// Entries that survived from the previous run.
    pub retained: usize,
    /// Entries that disappeared from MEMORY.md / PITFALLS.md
    /// since the last run (deleted or replaced).
    pub dropped: usize,
}

/// FNV-1a 64-bit hash of `content`, rendered as 16-char zero-
/// padded lowercase hex. Deterministic across processes (unlike
/// std's `DefaultHasher`). Sufficient for entry identification —
/// not for security claims.
pub fn entry_id(content: &str) -> String {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = FNV_OFFSET;
    for byte in content.as_bytes() {
        h ^= *byte as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fresh project root under the OS tempdir. Same
    /// pattern as `skills::usage::tests::temp_project` so the
    /// tests have no extra-crate dependency.
    fn temp_project() -> (ProjectPaths, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "dirge-memory-usage-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let paths = ProjectPaths::new(&dir);
        (paths, dir)
    }

    #[test]
    fn entry_id_is_deterministic_across_calls() {
        assert_eq!(entry_id("hello"), entry_id("hello"));
        assert_eq!(entry_id(""), entry_id(""));
        assert_eq!(entry_id("\nedge\ncase\n"), entry_id("\nedge\ncase\n"));
    }

    #[test]
    fn entry_id_differs_for_different_content() {
        assert_ne!(entry_id("foo"), entry_id("bar"));
        assert_ne!(entry_id("foo"), entry_id("foo "));
        assert_ne!(entry_id("foo"), entry_id("Foo"));
    }

    #[test]
    fn entry_id_is_16_hex_chars() {
        let id = entry_id("anything");
        assert_eq!(id.len(), 16, "FNV-1a 64-bit → 16 hex chars: {id:?}");
        assert!(
            id.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "lowercase hex only: {id:?}",
        );
    }

    #[test]
    fn load_returns_empty_store_when_no_sidecar_exists() {
        let (paths, _tmp) = temp_project();
        let store = MemoryUsageStore::load(&paths);
        assert!(store.is_empty());
    }

    #[test]
    fn reconcile_records_new_entries_with_current_timestamp() {
        let (paths, _tmp) = temp_project();
        let mut store = MemoryUsageStore::load(&paths);
        let now = "2026-05-28T12:00:00Z";
        let entries = [("memory", "first fact"), ("pitfalls", "avoid this")];
        let r = store.reconcile(&entries, now);
        assert_eq!(r.added, 2);
        assert_eq!(r.retained, 0);
        assert_eq!(r.dropped, 0);
        let rec = store.get("first fact").expect("entry recorded");
        assert_eq!(rec.first_seen_at, now);
        assert_eq!(rec.last_seen_at, now);
        assert_eq!(rec.target, "memory");
    }

    #[test]
    fn reconcile_updates_last_seen_for_surviving_entries() {
        let (paths, _tmp) = temp_project();
        let mut store = MemoryUsageStore::load(&paths);
        store.reconcile(&[("memory", "fact")], "2026-05-28T12:00:00Z");
        let r = store.reconcile(&[("memory", "fact")], "2026-06-04T12:00:00Z");
        assert_eq!(r.retained, 1);
        assert_eq!(r.added, 0);
        let rec = store.get("fact").unwrap();
        assert_eq!(rec.first_seen_at, "2026-05-28T12:00:00Z");
        assert_eq!(rec.last_seen_at, "2026-06-04T12:00:00Z");
    }

    #[test]
    fn reconcile_drops_entries_that_disappeared() {
        let (paths, _tmp) = temp_project();
        let mut store = MemoryUsageStore::load(&paths);
        store.reconcile(
            &[("memory", "old fact"), ("memory", "another old fact")],
            "2026-05-28T12:00:00Z",
        );
        let r = store.reconcile(&[("memory", "another old fact")], "2026-06-04T12:00:00Z");
        assert_eq!(r.dropped, 1, "missing entry must be dropped");
        assert_eq!(r.retained, 1);
        assert!(store.get("old fact").is_none());
        assert!(store.get("another old fact").is_some());
    }

    #[test]
    fn save_then_load_round_trips_records() {
        let (paths, _tmp) = temp_project();
        let mut store = MemoryUsageStore::load(&paths);
        store.reconcile(
            &[("memory", "round-trip me"), ("pitfalls", "and me")],
            "2026-05-28T12:00:00Z",
        );
        store.save().expect("save");
        let reloaded = MemoryUsageStore::load(&paths);
        assert_eq!(reloaded.len(), 2);
        let rec = reloaded.get("round-trip me").unwrap();
        assert_eq!(rec.target, "memory");
        assert_eq!(rec.first_seen_at, "2026-05-28T12:00:00Z");
    }

    /// Corrupt JSON on disk must not block curator runs —
    /// fall through to an empty store (parallel to
    /// `skills::usage::UsageStore::load` semantics).
    #[test]
    fn load_recovers_from_corrupt_json() {
        let (paths, _tmp) = temp_project();
        std::fs::create_dir_all(paths.memory_dir()).unwrap();
        std::fs::write(paths.memory_dir().join(".usage.json"), "not json {{").unwrap();
        let store = MemoryUsageStore::load(&paths);
        assert!(store.is_empty(), "corrupt JSON → empty store, no panic");
    }

    /// Idempotent reconcile: running the same scan twice with
    /// the same timestamp yields the same store. Important for
    /// the curator's retry behavior — a partial save followed
    /// by another run shouldn't corrupt state.
    #[test]
    fn reconcile_is_idempotent_on_identical_input() {
        let (paths, _tmp) = temp_project();
        let mut store = MemoryUsageStore::load(&paths);
        let entries = [("memory", "a"), ("memory", "b")];
        store.reconcile(&entries, "2026-05-28T12:00:00Z");
        let snapshot_a = store.clone();
        store.reconcile(&entries, "2026-05-28T12:00:00Z");
        assert_eq!(store.data, snapshot_a.data);
    }
}
