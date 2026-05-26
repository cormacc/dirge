---
triggers:
  - "add variant to AgentEvent"
  - "new AgentEvent"
  - "add event type"
  - "AgentEvent variant"
---

# Adding an AgentEvent Variant

Every new variant in `src/event.rs`'s `AgentEvent` enum breaks exhaustive match arms across the codebase.

## Checklist of files to update

1. `src/event.rs` — add the variant with doc comment
2. `src/agent/agent_loop/bridge.rs` — 
   - `translate()` method (the main event translation table)
   - `agent_event_kind()` helper in `#[cfg(test)]` block (~line 990)
   - Translation table comment at top of file (~line 8)
   - Any test that asserts event sequences containing the new variant
3. `src/agent/agent_loop/h7_smoke.rs` — `print_event()` function (~line 134)
4. `src/agent/agent_loop/integration.rs` — `agent_event_kind()` helper (~line 873)
5. `src/extras/acp/mod.rs` — ACP event loop match (~line 207). Non-interactive path — usually drop the variant with a comment.
6. `src/ui/mod.rs` — main UI event handler. Two locations:
   - The main match block (~line 2048) — handle the variant properly (render to screen, persist to session)
   - There may be a `#[cfg(feature = "loop")]`-gated path for loop mode interjections (~line 1576)
7. `src/agent/review.rs` — uses wildcard `_ => {}` so usually won't break, but verify
8. `src/provider/mod.rs` — `run_print` path uses wildcard `_ => {}` (~line 554), verify

## Discovery

```bash
# Let the compiler find all non-exhaustive matches
cargo check --bin dirge 2>&1 | grep "not covered"
```

This lists every file + line that needs updating.

## After all match arms compile

```bash
cargo test --bin dirge
```

Target: 1259+ tests passing, 0 warnings.

## Example: Adding `UserMessage` variant

When adding `AgentEvent::UserMessage { content: CompactString }` to surface steering-injected user messages in the UI log:

- **bridge.rs**: Emit it from `LoopEvent::MessageStart` when `message` is `LoopMessage::User`
- **UI (first location, ~line 2048)**: Call `write_user_lines()` then `session.add_message(MessageRole::User, &content)`
- **UI (loop path, ~line 1576)**: Show the actual message text with `»` prefix (not just "loop active — message queued")
- **ACP**: Drop with a comment ("ACP doesn't support mid-stream interjection")
- **All other locations**: Add arm that either handles or explicitly ignores
