use std::path::{Path, PathBuf};

use crate::agent::tools::background::{BackgroundStore, TaskKind};
use crate::session::Session;

pub struct StatusLine;

fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}k", n / 1000)
    } else {
        n.to_string()
    }
}

/// Find the current git branch for `start`, walking up parent
/// directories until we hit a `.git` entry (file for worktrees,
/// directory for the main checkout) or the filesystem root. Returns
/// `None` when the directory isn't inside a git working tree, or
/// when `.git/HEAD` is unreadable / detached / malformed (the status
/// line is informational, not a git porcelain — we just omit the
/// segment in those cases).
fn git_branch(start: &Path) -> Option<String> {
    let head_path = find_git_head(start)?;
    let head = std::fs::read_to_string(head_path).ok()?;
    let head = head.trim();
    head.strip_prefix("ref: refs/heads/").map(|b| b.to_string())
}

fn find_git_head(start: &Path) -> Option<PathBuf> {
    let mut cur: PathBuf = start.to_path_buf();
    loop {
        let dot_git = cur.join(".git");
        if dot_git.is_dir() {
            return Some(dot_git.join("HEAD"));
        }
        if dot_git.is_file() {
            // Worktree pointer: `gitdir: <path>` → HEAD lives there.
            let txt = std::fs::read_to_string(&dot_git).ok()?;
            let gitdir = txt.trim().strip_prefix("gitdir: ")?;
            return Some(PathBuf::from(gitdir).join("HEAD"));
        }
        if !cur.pop() {
            return None;
        }
    }
}

impl StatusLine {
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        session: &Session,
        is_running: bool,
        _spinner_tick: u64,
        loop_label: Option<&str>,
        prompt_name: Option<&str>,
        perm_mode: Option<&str>,
        bg_store: Option<&BackgroundStore>,
    ) -> String {
        let state = if is_running { "running" } else { "ready" };
        let wd_path = Path::new(&session.working_dir);
        let dir = wd_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&session.working_dir);
        // Append `:branch` when the working dir is inside a git
        // working tree. Detached HEAD / non-git dirs show just the
        // project name.
        let project_label = match git_branch(wd_path) {
            Some(b) => format!("{}:{}", dir, b),
            None => dir.to_string(),
        };

        let ctx = session.context_window;
        let used = session.total_estimated_tokens;
        let pct = (used * 100).checked_div(ctx).unwrap_or(0);

        // TODO(cost-tracking): `session.total_cost` is always 0.0
        // because dirge doesn't yet have a per-provider pricing
        // table — `AgentEvent::Done` emits `cost: 0.0` unconditionally
        // (see `src/agent/runner.rs::run_stream`). Until that's wired,
        // the cost segment is suppressed entirely to avoid showing a
        // misleading "$0.0000". When pricing lands, restore the
        // conditional formatter that was here previously.
        let cost_str = String::new();

        let compact_badge = if session.compactions.is_empty() {
            String::new()
        } else {
            format!(" cmp:{}", session.compactions.len())
        };

        let loop_badge = match loop_label {
            Some(label) => format!(" [{}]", label),
            None => String::new(),
        };

        let prompt_badge = match prompt_name {
            Some(name) => format!(" [{}]", name),
            None => String::new(),
        };

        let perm_badge = match perm_mode {
            Some(m) if m != "standard" => format!(" | mode:{}", m),
            _ => String::new(),
        };

        // Active background work, counted per kind. Each badge is shown
        // only when non-zero, like the other conditional badges, so the
        // bar stays quiet during normal single-agent work.
        let (active_agents, active_shells) = match bg_store {
            Some(s) => (
                s.running_count_kind(TaskKind::Subagent),
                s.running_count_kind(TaskKind::Shell),
            ),
            None => (0, 0),
        };
        let agents_badge = if active_agents > 0 {
            format!(" | agents:{}", active_agents)
        } else {
            String::new()
        };
        let shells_badge = if active_shells > 0 {
            format!(" | shells:{}", active_shells)
        } else {
            String::new()
        };

        format!(
            "{}{} | {}{} | {}/{} ({}%) | {}msgs | {}{}{}{}{}{}",
            project_label,
            cost_str,
            session.model,
            loop_badge,
            fmt_tokens(used),
            fmt_tokens(ctx),
            pct,
            session.messages.len(),
            state,
            compact_badge,
            prompt_badge,
            perm_badge,
            agents_badge,
            shells_badge,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::StatusLine;
    use crate::agent::tools::background::{BackgroundStore, TaskKind};
    use crate::session::Session;

    /// Build a store with `agents` running subagents and `shells` running
    /// shells (each needs a live handle to count, so attach a never-ending
    /// spawned task per entry).
    fn store_with(agents: usize, shells: usize) -> BackgroundStore {
        let store = BackgroundStore::new();
        let mut n = 0;
        for (kind, count) in [(TaskKind::Subagent, agents), (TaskKind::Shell, shells)] {
            for _ in 0..count {
                let id = format!("id{n}");
                n += 1;
                store.insert(id.clone(), kind);
                let h = tokio::runtime::Handle::try_current()
                    .ok()
                    .map(|_| tokio::spawn(async { std::future::pending::<()>().await }));
                if let Some(h) = h {
                    store.attach_handle(&id, h);
                }
            }
        }
        store
    }

    fn render(store: &BackgroundStore) -> String {
        let session = Session::new("openrouter", "test-model", 100_000);
        StatusLine::render(&session, false, 0, None, None, None, Some(store))
    }

    #[tokio::test]
    async fn badges_hidden_when_nothing_active() {
        let line = render(&store_with(0, 0));
        assert!(
            !line.contains("agents:"),
            "no agents badge expected: {line}"
        );
        assert!(
            !line.contains("shells:"),
            "no shells badge expected: {line}"
        );
    }

    #[tokio::test]
    async fn agents_and_shells_counted_separately() {
        let line = render(&store_with(2, 3));
        assert!(line.contains("agents:2"), "expected agents:2 in: {line}");
        assert!(line.contains("shells:3"), "expected shells:3 in: {line}");
    }
}
