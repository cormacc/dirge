## Bridge drops LoopMessage::User at MessageStart

`src/agent/agent_loop/bridge.rs` line 221-236 originally returned `Vec::new()` for all non-Custom `LoopMessage::User` variants at `MessageStart`. The comment explicitly said "user messages aren't AgentEvents at all." This was the root cause of steering-injected user messages being swallowed — the UI never received an event for them.

Fix: emit `AgentEvent::UserMessage { content }` for `LoopMessage::User` so the UI can display the interjected text in the log and persist it to the session. The bridge's translation table comment and the `message_start_end_are_no_ops` test both encoded the wrong assumption; both needed updating.
§
Legacy code path: the old `run_agent_loop_continue` and `LoopError` enum were removed. The new way is mid-run steering via `get_steering_messages` + `steering_from_queue`. The `Interjected` variant in `AgentEvent` is actively constructed by the bridge (not legacy) — it's emitted when the rig stream is cancelled via interject signal. Don't mark it dead_code.
