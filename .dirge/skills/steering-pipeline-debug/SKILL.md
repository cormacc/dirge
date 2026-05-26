---
name: steering-pipeline-debug
description: Debugging why user messages typed during an agent run don't appear in the UI log or reach the model.
---

# Steering Pipeline Debugging

Use when investigating why user messages typed during an agent run don't appear in the UI log, don't reach the model, or the agent doesn't respond to mid-run interjections.

## The pipeline

```
UI input (Enter)
  → interjection_queue (VecDeque<String>, Mutex-protected)
    → steering_from_queue() polls at turn boundaries → LoopMessage::User
      → run.rs inner loop injects → stream.rs/tools.rs emits LoopEvent::MessageStart { message: LoopMessage::User }
        → bridge.rs translate() → AgentEvent::UserMessage { content }
          → ui/mod.rs event handler → write_user_lines() + session.add_message()
```

## Key files and lines

1. **`src/ui/mod.rs:1576-1593`** — `#[cfg(feature = "loop")]` path: user types during active loop → queue + display `»` prefix
2. **`src/ui/mod.rs:1870-1885`** — non-loop path: user types while `is_running` → queue + display `»` prefix
3. **`src/ui/mod.rs:674`** — `interjection_queue` definition (Arc<Mutex<VecDeque<String>>>)
4. **`src/agent/agent_loop/steering.rs:1-484`** — `steering_from_queue()` builds `GetSteeringMessagesFn`, wraps with `MID_TURN_STEER_WRAPPER`
5. **`src/agent/agent_loop/run.rs:237-241`** — initial steering poll before outer loop
6. **`src/agent/agent_loop/run.rs:587-591`** — steering refresh after each inner iteration
7. **`src/agent/agent_loop/bridge.rs:221-236`** — `MessageStart` translation: User → UserMessage, Custom → CustomMessage, others → no-op
8. **`src/event.rs:155-159`** — `AgentEvent::UserMessage { content: CompactString }`
9. **`src/ui/mod.rs:3488-3492`** — UI handler for `AgentEvent::UserMessage`

## Common failure modes

### Message queued but never reaches model
- Check `steering_from_queue` is wired into `LoopConfig.get_steering_messages` (integration.rs:388-390)
- Check `QueueMode` — `All` drains entire queue per poll, `OneAtATime` drains oldest only
- Check that the inner loop's `while has_more_tool_calls || !pending_messages.is_empty()` condition is reached

### Message queued but not displayed in UI
- Check bridge's `MessageStart` handler — originally returned `Vec::new()` for User messages
- Check UI has arm for `AgentEvent::UserMessage` — added in event handler at ~line 3488
- Check both UI paths display the `»` lines: loop path (~1576) and non-loop path (~1870)

### AgentEvent variant added but compile fails
- See "AgentEvent variant addition checklist" in memory — must update ~7 match arms across the codebase

## Verification

```bash
# Bridge tests — verifies translation correctness
cargo test --bin dirge agent::agent_loop::bridge

# Steering tests — verifies queue polling, sanitization, modes
cargo test --bin dirge agent::agent_loop::steering

# Integration tests — verifies end-to-end steering injection
cargo test --bin dirge agent::agent_loop::integration

# Full suite
cargo test --bin dirge
```