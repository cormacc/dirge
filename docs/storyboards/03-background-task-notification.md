# Background task completes between turns

A `task` tool dispatched with `background=true` finishes while the
user is typing the next prompt. On submit, the LLM receives a
`<system-reminder>` block listing finished tasks, prepended to the
user's text. The visible chat log shows only what the user typed —
the wrapper is stripped at render time.

## Flow

1. Background runner stores its result and emits
   `LifecycleEvent::Finished` to the UI sink.
2. UI's event loop drains the channel and prints a `[done] task-N
   completed …` notification line.
3. User submits the next prompt.
4. `prepend_pending_notifications` drains the notification store and
   constructs a combined string: `<system-reminder>…</system-reminder>
   \n\n<user text>`. This becomes the agent run's `initial_prompt`.
5. `session.add_message(MessageRole::User, &text)` persists the
   clean user text — not the wrapped version.
6. Agent loop emits `LoopEvent::MessageStart { LoopMessage::User(…) }`
   carrying the full wrapped string.
7. Bridge converts to `AgentEvent::UserMessage { content }` with the
   wrapper preserved (downstream request builder needs it).
8. UI consumer strips the leading `<system-reminder>` block before
   calling `write_user_lines`. Only the plain user text renders.

## Implementation

- `src/agent/tools/background.rs::BackgroundStore::with_ui_sink` —
  lifecycle event channel.
- `src/agent/tools/background.rs::prepend_pending_notifications` —
  builds the wrapped prompt.
- `src/agent/agent_loop/integration.rs::run_agent_loop_with_summarizer`
  — sets `initial_prompt` to the wrapped string.
- `src/agent/agent_loop/bridge.rs` — `LoopMessage::User` →
  `AgentEvent::UserMessage` conversion.
- `src/ui/mod.rs::strip_leading_system_reminder` — strips a leading
  `<system-reminder>…</system-reminder>` plus trailing whitespace.
  Returns input unchanged if no block, no close tag, or block is
  mid-message.
- `src/ui/mod.rs::write_user_lines` — single render point for
  `<you>` lines.
- `src/ui/run_handlers/done.rs::handle_done` and
  `src/ui/run_handlers/interjected.rs::handle_interjected` — drain
  the interjection queue and respawn the runner; rely on the bridge
  for rendering so the wrapper-strip applies uniformly.

## Edge cases

- No pending notifications: `prepend_pending_notifications` returns
  the prompt unchanged; the strip is a no-op.
- Multiple finished tasks: all rendered inside a single
  `<system-reminder>` block, stripped together.
- User literally pastes a `<system-reminder>` block at the start of
  their message: gets stripped. Mitigated by "leading position only"
  — a quoted reminder mid-message survives.
- Missing close tag: defensive — input passes through unstripped.
