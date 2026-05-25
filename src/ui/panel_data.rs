//! Info panel data types.
//!
//! `PanelData`, `SubagentStatusRow`, and `LeftPanelInfo` — the three
//! structs that carry the right-hand side panel's content. Extracted
//! from `renderer.rs` so the panel painter and the UI loop can share
//! them without pulling in the full Renderer.

/// Snapshot of the data the info panel displays. Built fresh by the UI loop
/// at each redraw because the underlying state (todos, modified files, etc.)
/// is mutated by the agent and we don't want stale reads.
#[derive(Default, Clone)]
pub struct PanelData {
    /// (server name, connected) — connected currently always true because the
    /// MCP manager drops failed connections at connect time; future health
    /// tracking can flip this to false.
    pub mcp: Vec<(String, bool)>,
    /// (server_id, short root path, ok) — ok=false for broken servers.
    pub lsp: Vec<(String, String, bool)>,
    /// (status glyph, todo text). Status is single-char shorthand
    /// like "[ ]", "[~]", "[x]" depending on the todo state.
    pub todos: Vec<(String, String)>,
    /// Recent modified file paths, shortened relative to cwd when possible.
    pub modified: Vec<String>,
    /// ui-redesign: latest system load snapshot for the
    /// [SYSTEM LOAD] sub-panel. `None` when the polling task hasn't
    /// produced a reading yet (very early startup) — painter skips
    /// the section in that case.
    pub sysload: Option<crate::ui::sysload::SysLoadSnapshot>,
}

/// dirge-gek: one row in the left-gutter subagent panel. Rendered as
/// `<status-glyph> <short-id> <truncated-prompt>` so a quick glance
/// shows what's running. The UI loop rebuilds these from
/// `bg_store.list()` on each lifecycle event and pushes via
/// `Renderer::set_subagent_status`.
#[derive(Debug, Clone, Default)]
pub struct SubagentStatusRow {
    pub id_short: String,
    pub state: String,
    pub prompt_short: String,
}

/// ui-redesign: idle-state info for the left panel. When no
/// subagents are active, the left gutter paints this card: ASCII
/// DIRGE logo + agent metadata. Updated at session-start (and on
/// `/model` switch / `/prompt` switch) by the UI loop.
#[derive(Debug, Clone, Default)]
pub struct LeftPanelInfo {
    pub agent_id: String,
    pub model: String,
    pub focus: String,
}
