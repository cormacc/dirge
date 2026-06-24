//! Non-blocking compaction (dirge-tv3p / dirge-dtyn).
//!
//! Compaction's slow part is the summarizer LLM call. Running it inline in the
//! UI event loop froze rendering, input, and Ctrl+C for 10–60s. This module is
//! the off-thread half, mirroring the `/plan` phase machinery
//! ([`crate::agent::plan::runtime`]): the loop builds the prompt + resolves the
//! model on-thread ([`crate::ui::slash::prepare_compaction`]), [`spawn`]s the
//! summarizer as a task that streams a terminal event back, and a dedicated
//! `select!` arm installs the result on-thread
//! ([`crate::ui::slash::install_compaction`]) and runs the continuation.
//!
//! The session is loop-owned and is NOT touched while the task runs — the loop
//! gates new prompts/commands until the phase resolves — so the `cut_idx` /
//! `tokens_before` captured at prepare time are still valid at install.

use crate::ui::slash::CompactionRequest;

/// What to do once compaction installs — the four trigger sites differ only
/// here.
pub(crate) enum CompactionThen {
    /// Explicit `/compress` or post-turn auto-compact: nothing follows.
    Nothing,
    /// Preemptive (pre-prompt) and reactive (overflow-recovery) compaction:
    /// run this prompt as a normal streamed agent turn afterward. `reactive`
    /// marks the overflow-recovery case, where a compaction FAILURE means the
    /// prompt still won't fit, so we must not blindly resend it.
    SendPrompt { prompt: String, reactive: bool },
}

/// Terminal event from the spawned summarizer task. (There's no `Progress` —
/// the loop already printed "compressing…" and the spinner animates on-loop.)
pub(crate) enum CompactionPhaseEvent {
    /// The summarizer returned; install this summary on the UI thread.
    Done { summary: String },
    /// The summarizer errored (or the injection guard tripped on the prompt
    /// build — though that's caught earlier, on-thread).
    Failed { error: String },
}

/// Handle to the spawned compaction task: the terminal-event channel the loop
/// drains, the task (so Ctrl+C can `abort()` it), the install inputs captured
/// on-thread, and the continuation.
pub(crate) struct CompactionPhaseHandle {
    pub rx: tokio::sync::mpsc::Receiver<CompactionPhaseEvent>,
    pub task: tokio::task::JoinHandle<()>,
    pub cut_idx: usize,
    pub tokens_before: u64,
    pub then: CompactionThen,
}

/// Spawn the summarizer LLM off-thread and return the handle the UI loop drives
/// from its `select!`. `req` carries the model + prebuilt prompt produced by
/// `prepare_compaction` on the UI thread.
pub(crate) fn spawn(req: CompactionRequest, then: CompactionThen) -> CompactionPhaseHandle {
    let CompactionRequest {
        model,
        prompt,
        cut_idx,
        tokens_before,
    } = req;
    // Capacity 1: the task sends exactly one terminal event.
    let (tx, rx) = tokio::sync::mpsc::channel::<CompactionPhaseEvent>(1);
    let task = tokio::spawn(async move {
        let event = match crate::provider::run_compaction(model, prompt).await {
            Ok(summary) => CompactionPhaseEvent::Done { summary },
            Err(e) => CompactionPhaseEvent::Failed {
                error: e.to_string(),
            },
        };
        let _ = tx.send(event).await;
    });
    CompactionPhaseHandle {
        rx,
        task,
        cut_idx,
        tokens_before,
        then,
    }
}
