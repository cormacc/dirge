//! Memory entry lifecycle curator. Periodic background pass that
//! tracks MEMORY.md / PITFALLS.md entries via the usage sidecar,
//! identifies stale candidates, and writes an audit report.
//!
//! dirge-mo0w (audit finding B), PR-1 of 2: mechanical pass only.
//! No LLM call yet — that lands in the follow-up PR with the
//! `MEMORY_CURATOR_PROMPT` analog of `dirge-odv3`. This PR builds
//! the infrastructure (state, telemetry reconciliation, audit
//! report, trigger gate) so the LLM pass has somewhere to plug
//! into.
//!
//! Parallel structure to `extras::skills::curator`:
//! - `.dirge/memory/.curator_state` — scheduler state
//! - `.dirge/memory/.curator_reports/{YYYYMMDD-HHMMSS}/REPORT.md`
//! - 7-day interval gate, first-run defer
//! - 30-day stale, 90-day archive-candidate thresholds
//!
//! Differences from the skills curator:
//! - Entries aren't named — they're keyed by FNV-1a hash of
//!   content via `memory_usage::MemoryUsageStore`.
//! - PR-1 only IDENTIFIES archive candidates; it doesn't move
//!   them. Actual archival waits for the LLM consolidation pass
//!   in PR-2, where the model can judge whether stale entries
//!   are also obsolete (some old facts are still load-bearing).

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::extras::dirge_paths::ProjectPaths;
use crate::extras::memory_usage::{MemoryUsageStore, ReconcileReport, entry_id};

/// Days since `first_seen_at` before an entry counts as stale.
const STALE_AFTER_DAYS: u64 = 30;

/// Days of staleness before an entry becomes an archive candidate
/// for the LLM pass (PR-2). PR-1 just identifies them.
#[allow(dead_code)]
const ARCHIVE_AFTER_STALE_DAYS: u64 = 90;

/// Minimum hours between curator runs.
const INTERVAL_HOURS: u64 = 168; // 7 days

const ENTRY_DELIMITER: &str = "\n§\n";

// ── State ─────────────────────────────────────────────

/// Persistent scheduler state at `.dirge/memory/.curator_state`.
/// Mirrors the skills curator's state shape so future code that
/// wants to coordinate the two runners has the same field
/// vocabulary to work with.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MemoryCuratorState {
    /// Unix timestamp (seconds) of the last curator run. `None`
    /// = never run; different from epoch-0 which is a valid
    /// timestamp on some systems.
    pub last_run: Option<u64>,
    /// Timestamp when the state was first seeded.
    pub first_check: u64,
}

impl MemoryCuratorState {
    fn new(now: u64) -> Self {
        Self {
            last_run: None,
            first_check: now,
        }
    }

    fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::new(now_secs()));
        }
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("read curator state: {e}"))?;
        serde_json::from_str(&content).map_err(|e| format!("parse curator state: {e}"))
    }

    fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create state dir: {e}"))?;
        }
        let content =
            serde_json::to_string_pretty(self).map_err(|e| format!("serialize state: {e}"))?;
        crate::fs_atomic::atomic_write_sync(path, content.as_bytes())
            .map_err(|e| format!("write state: {e}"))
    }
}

// ── Curator ───────────────────────────────────────────

/// Memory lifecycle manager. Constructed once per run.
pub struct MemoryCurator {
    paths: ProjectPaths,
    state: MemoryCuratorState,
    state_path: PathBuf,
}

impl MemoryCurator {
    pub fn new(paths: &ProjectPaths) -> Result<Self, String> {
        let state_path = paths.memory_dir().join(".curator_state");
        let state = MemoryCuratorState::load(&state_path)?;
        Ok(Self {
            paths: paths.clone(),
            state,
            state_path,
        })
    }

    /// Should the curator run now? `false` on first-ever check
    /// (seeds the state without running) and during the 7-day
    /// interval gate.
    pub fn should_run_now(&self) -> bool {
        let now = now_secs();
        let Some(last) = self.state.last_run else {
            return false; // first-run defer; seed via run_mechanical_pass
        };
        let elapsed = Duration::from_secs(now.saturating_sub(last));
        elapsed >= Duration::from_secs(INTERVAL_HOURS * 3600)
    }

    /// Run the mechanical pass: reconcile telemetry sidecar
    /// against current entries, identify stale candidates, write
    /// audit report. No LLM call, no archival. Returns the
    /// per-run report so callers (tests, follow-on LLM pass) can
    /// inspect what happened.
    pub fn run_mechanical_pass(&mut self) -> Result<MechanicalReport, String> {
        let started_at = chrono::Utc::now();
        let started_at_iso = started_at.to_rfc3339();
        let started_at_filename = started_at.format("%Y%m%d-%H%M%S").to_string();
        let now = now_secs();

        // 1. Scan MEMORY.md and PITFALLS.md into (target, entry)
        //    pairs.
        let entries = self.scan_entries()?;
        let total_entries = entries.len();

        // 2. Reconcile usage sidecar.
        let mut usage = MemoryUsageStore::load(&self.paths);
        let entries_slice: Vec<(&str, &str)> = entries
            .iter()
            .map(|(t, c)| (t.as_str(), c.as_str()))
            .collect();
        let reconcile = usage.reconcile(&entries_slice, &started_at_iso);
        if let Err(e) = usage.save() {
            // Don't abort the pass — the report still has value.
            tracing::warn!(
                target: "dirge::memory_curator",
                error = %e,
                "Failed to save memory usage sidecar",
            );
        }

        // 3. Identify stale candidates.
        let mut stale_candidates: Vec<StaleCandidate> = Vec::new();
        for (target, content) in &entries {
            let Some(rec) = usage.get(content) else {
                continue;
            };
            let Ok(first_seen) = chrono::DateTime::parse_from_rfc3339(&rec.first_seen_at) else {
                continue;
            };
            let age_secs = started_at.timestamp() - first_seen.timestamp();
            let age_days = (age_secs.max(0) as u64) / 86400;
            if age_days >= STALE_AFTER_DAYS {
                stale_candidates.push(StaleCandidate {
                    target: target.clone(),
                    entry_id: entry_id(content),
                    preview: preview(content),
                    age_days,
                });
            }
        }
        stale_candidates.sort_by(|a, b| b.age_days.cmp(&a.age_days));

        // 4. Update state.
        self.state.last_run = Some(now);
        self.state.save(&self.state_path)?;

        let report = MechanicalReport {
            started_at_iso: started_at_iso.clone(),
            total_entries,
            reconcile,
            stale_candidates,
        };

        // 5. Write audit report.
        let reports_dir = self
            .paths
            .memory_dir()
            .join(".curator_reports")
            .join(&started_at_filename);
        std::fs::create_dir_all(&reports_dir).map_err(|e| format!("create reports dir: {e}"))?;
        let report_path = reports_dir.join("REPORT.md");
        std::fs::write(&report_path, report.to_markdown())
            .map_err(|e| format!("write report: {e}"))?;

        Ok(report)
    }

    /// Read MEMORY.md and PITFALLS.md from `.dirge/memory/`,
    /// split on `\n§\n`, return `(target, entry_content)` pairs.
    /// Empty / missing files contribute nothing — caller treats
    /// an empty Vec as "no entries to curate".
    fn scan_entries(&self) -> Result<Vec<(String, String)>, String> {
        let mut entries: Vec<(String, String)> = Vec::new();
        for target in ["memory", "pitfalls"] {
            let file_name = match target {
                "memory" => "MEMORY.md",
                "pitfalls" => "PITFALLS.md",
                _ => continue,
            };
            let path = self.paths.memory_dir().join(file_name);
            if !path.is_file() {
                continue;
            }
            let _ = File::open(&path); // sanity check accessibility
            let content =
                std::fs::read_to_string(&path).map_err(|e| format!("read {file_name}: {e}"))?;
            for raw in content.split(ENTRY_DELIMITER) {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    continue;
                }
                entries.push((target.to_string(), trimmed.to_string()));
            }
        }
        Ok(entries)
    }
}

// ── Report ────────────────────────────────────────────

/// Per-run report. Curator returns this so callers (tests
/// today; LLM pass in PR-2) can introspect what the mechanical
/// pass observed. Also rendered as Markdown to disk for human
/// review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MechanicalReport {
    pub started_at_iso: String,
    pub total_entries: usize,
    pub reconcile: ReconcileReport,
    pub stale_candidates: Vec<StaleCandidate>,
}

/// One entry the curator would propose for archive consideration.
/// PR-1 only identifies these; PR-2's LLM pass decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleCandidate {
    pub target: String,
    pub entry_id: String,
    pub preview: String,
    pub age_days: u64,
}

impl MechanicalReport {
    /// Render as Markdown for `REPORT.md`. Keep it scan-friendly
    /// — the audit report's job is "show me at a glance what
    /// changed this run."
    pub fn to_markdown(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(out, "# Memory curator — mechanical pass\n");
        let _ = writeln!(out, "- Started: {}", self.started_at_iso);
        let _ = writeln!(out, "- Total entries: {}", self.total_entries);
        let _ = writeln!(
            out,
            "- Reconcile: +{} new / {} retained / -{} dropped",
            self.reconcile.added, self.reconcile.retained, self.reconcile.dropped,
        );
        let _ = writeln!(out, "- Stale candidates: {}", self.stale_candidates.len());

        if !self.stale_candidates.is_empty() {
            let _ = writeln!(out, "\n## Stale candidates (≥ {STALE_AFTER_DAYS} days)\n",);
            let _ = writeln!(out, "| Target | Age (days) | Entry ID | Preview |");
            let _ = writeln!(out, "|---|---|---|---|");
            for c in &self.stale_candidates {
                let _ = writeln!(
                    out,
                    "| `{}` | {} | `{}` | {} |",
                    c.target,
                    c.age_days,
                    c.entry_id,
                    c.preview.replace('|', "\\|"),
                );
            }
        }

        let _ = writeln!(
            out,
            "\n_Mechanical pass only \u{2014} no entries archived. LLM consolidation pass (dirge-mo0w PR-2) decides actual fate._"
        );

        out
    }
}

// ── Helpers ───────────────────────────────────────────

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// First-line snippet of an entry, capped at 80 chars. Used in
/// the audit report so the operator can identify which entry is
/// stale without rendering the full content.
fn preview(content: &str) -> String {
    let first = content.lines().next().unwrap_or("");
    let trimmed = first.trim();
    if trimmed.chars().count() <= 80 {
        trimmed.to_string()
    } else {
        let cut: String = trimmed.chars().take(77).collect();
        format!("{cut}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_project() -> (ProjectPaths, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "dirge-memory-curator-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let paths = ProjectPaths::new(&dir);
        std::fs::create_dir_all(paths.memory_dir()).unwrap();
        (paths, dir)
    }

    fn write_memory(paths: &ProjectPaths, name: &str, entries: &[&str]) {
        let path = paths.memory_dir().join(name);
        let content = entries.join(ENTRY_DELIMITER);
        std::fs::write(path, content).unwrap();
    }

    /// First-ever check seeds state but does NOT run.
    #[test]
    fn should_run_now_returns_false_on_first_check() {
        let (paths, _tmp) = temp_project();
        let curator = MemoryCurator::new(&paths).unwrap();
        assert!(
            !curator.should_run_now(),
            "first check must seed state without running (mirrors skills curator)",
        );
    }

    /// After a run, the 7-day interval gate keeps subsequent
    /// checks from re-running immediately.
    #[test]
    fn should_run_now_respects_interval_gate() {
        let (paths, _tmp) = temp_project();
        let state_path = paths.memory_dir().join(".curator_state");
        let just_ran = MemoryCuratorState {
            last_run: Some(now_secs()),
            first_check: now_secs() - 86400,
        };
        std::fs::create_dir_all(paths.memory_dir()).unwrap();
        just_ran.save(&state_path).unwrap();
        let curator = MemoryCurator::new(&paths).unwrap();
        assert!(
            !curator.should_run_now(),
            "must respect 7-day interval gate",
        );
    }

    /// After 8 days, the gate opens and the curator should run.
    #[test]
    fn should_run_now_returns_true_after_interval_elapsed() {
        let (paths, _tmp) = temp_project();
        let state_path = paths.memory_dir().join(".curator_state");
        let eight_days_ago = now_secs().saturating_sub(8 * 24 * 3600);
        let stale = MemoryCuratorState {
            last_run: Some(eight_days_ago),
            first_check: eight_days_ago,
        };
        std::fs::create_dir_all(paths.memory_dir()).unwrap();
        stale.save(&state_path).unwrap();
        let curator = MemoryCurator::new(&paths).unwrap();
        assert!(curator.should_run_now(), "after 8 days the gate must open");
    }

    /// Empty memory directory: pass runs cleanly, report shows
    /// zero entries, state advances.
    #[test]
    fn run_mechanical_pass_handles_empty_memory_dir() {
        let (paths, _tmp) = temp_project();
        let mut curator = MemoryCurator::new(&paths).unwrap();
        let report = curator.run_mechanical_pass().unwrap();
        assert_eq!(report.total_entries, 0);
        assert_eq!(report.reconcile.added, 0);
        assert_eq!(report.stale_candidates.len(), 0);
        // State advanced.
        assert!(curator.state.last_run.is_some());
    }

    /// Fresh entries get recorded in the usage sidecar and
    /// surface as "added" in the report, but DON'T appear as
    /// stale candidates (they're new).
    #[test]
    fn run_mechanical_pass_records_fresh_entries_without_marking_stale() {
        let (paths, _tmp) = temp_project();
        write_memory(&paths, "MEMORY.md", &["fact 1", "fact 2"]);
        write_memory(&paths, "PITFALLS.md", &["pitfall 1"]);
        let mut curator = MemoryCurator::new(&paths).unwrap();
        let report = curator.run_mechanical_pass().unwrap();
        assert_eq!(report.total_entries, 3);
        assert_eq!(report.reconcile.added, 3);
        assert_eq!(report.reconcile.dropped, 0);
        assert_eq!(
            report.stale_candidates.len(),
            0,
            "freshly-observed entries can't be stale yet",
        );
    }

    /// Entries first observed > 30 days ago surface as stale
    /// candidates. Simulated by pre-seeding the usage sidecar
    /// with a backdated `first_seen_at`.
    #[test]
    fn run_mechanical_pass_identifies_entries_first_seen_long_ago_as_stale() {
        let (paths, _tmp) = temp_project();
        write_memory(&paths, "MEMORY.md", &["old fact", "new fact"]);
        // Pre-seed the sidecar with backdated "old fact".
        let mut usage = MemoryUsageStore::load(&paths);
        let thirty_one_days_ago = chrono::Utc::now() - chrono::Duration::days(31);
        let now = chrono::Utc::now().to_rfc3339();
        usage.reconcile(&[("memory", "old fact")], &thirty_one_days_ago.to_rfc3339());
        usage.reconcile(&[("memory", "old fact"), ("memory", "new fact")], &now);
        usage.save().unwrap();

        let mut curator = MemoryCurator::new(&paths).unwrap();
        let report = curator.run_mechanical_pass().unwrap();

        let stale_targets: Vec<&str> = report
            .stale_candidates
            .iter()
            .map(|c| c.preview.as_str())
            .collect();
        assert!(
            stale_targets.contains(&"old fact"),
            "old entry must be marked stale: {stale_targets:?}",
        );
        assert!(
            !stale_targets.contains(&"new fact"),
            "fresh entry must NOT be stale: {stale_targets:?}",
        );
    }

    /// REPORT.md is written under `.dirge/memory/.curator_reports/{ts}/`.
    #[test]
    fn run_mechanical_pass_writes_audit_report_to_disk() {
        let (paths, _tmp) = temp_project();
        write_memory(&paths, "MEMORY.md", &["one fact"]);
        let mut curator = MemoryCurator::new(&paths).unwrap();
        curator.run_mechanical_pass().unwrap();
        let reports_root = paths.memory_dir().join(".curator_reports");
        assert!(reports_root.is_dir(), "reports root must exist");
        let entries: Vec<_> = std::fs::read_dir(&reports_root)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1, "exactly one run directory per run");
        let report_md = entries[0].path().join("REPORT.md");
        assert!(report_md.is_file(), "REPORT.md must be written");
        let body = std::fs::read_to_string(&report_md).unwrap();
        assert!(body.contains("# Memory curator"));
        assert!(body.contains("Total entries: 1"));
    }

    /// Removed entries: an entry present last run but absent now
    /// surfaces as `dropped` in the reconcile report and is
    /// purged from the sidecar.
    #[test]
    fn run_mechanical_pass_drops_entries_that_disappeared() {
        let (paths, _tmp) = temp_project();
        write_memory(&paths, "MEMORY.md", &["doomed fact"]);
        let mut curator = MemoryCurator::new(&paths).unwrap();
        curator.run_mechanical_pass().unwrap();
        // Now remove the entry from disk and re-run.
        write_memory(&paths, "MEMORY.md", &[]);
        let mut curator2 = MemoryCurator::new(&paths).unwrap();
        let report = curator2.run_mechanical_pass().unwrap();
        assert_eq!(report.reconcile.dropped, 1, "removed entry must drop");
        let usage = MemoryUsageStore::load(&paths);
        assert!(usage.is_empty(), "sidecar must be purged");
    }

    /// State persistence: a fresh curator instance loads the
    /// last_run timestamp the previous instance wrote.
    #[test]
    fn run_mechanical_pass_persists_last_run_timestamp() {
        let (paths, _tmp) = temp_project();
        write_memory(&paths, "MEMORY.md", &["whatever"]);
        let mut curator = MemoryCurator::new(&paths).unwrap();
        curator.run_mechanical_pass().unwrap();
        let last_run = curator.state.last_run;
        let curator2 = MemoryCurator::new(&paths).unwrap();
        assert_eq!(
            curator2.state.last_run, last_run,
            "state must round-trip through disk",
        );
    }

    /// Report markdown contains a "no entries archived" disclaimer
    /// so the operator knows PR-1 is mechanical-only.
    #[test]
    fn report_markdown_disclaims_actual_archival() {
        let report = MechanicalReport {
            started_at_iso: "2026-05-28T12:00:00Z".to_string(),
            total_entries: 5,
            reconcile: ReconcileReport {
                added: 1,
                retained: 4,
                dropped: 0,
            },
            stale_candidates: vec![],
        };
        let md = report.to_markdown();
        assert!(
            md.contains("no entries archived"),
            "PR-1 must disclaim mechanical-only scope: {md}",
        );
    }

    /// Preview helper: short entries pass through verbatim;
    /// long entries get truncated with an ellipsis marker.
    #[test]
    fn preview_truncates_long_lines_with_ellipsis() {
        let short = preview("short and sweet");
        assert_eq!(short, "short and sweet");
        let long = preview(&"x".repeat(120));
        assert!(
            long.ends_with("..."),
            "long preview must end with '...': {long:?}",
        );
        assert!(long.len() <= 80, "preview must cap length: {}", long.len());
    }
}
