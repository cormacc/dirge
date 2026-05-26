## AgentEvent variant addition checklist

Adding a new variant to `AgentEvent` in `src/event.rs` requires updating ALL exhaustive match arms. The compiler will find most but these are the files:

1. `src/agent/agent_loop/bridge.rs` — `translate()` method (~line 97), `agent_event_kind` helper in tests (~line 981)
2. `src/agent/agent_loop/h7_smoke.rs` — `print_event()` function (~line 134)
3. `src/agent/agent_loop/integration.rs` — `agent_event_kind()` helper (~line 873)
4. `src/extras/acp/mod.rs` — ACP event loop match (~line 207)
5. `src/ui/mod.rs` — main UI event handler (~line 2048 with many arms); also the `#[cfg(feature = "loop")]` path around line 1576
6. `src/agent/review.rs` — uses wildcard `_ => {}` so won't break, but review anyway
7. `src/provider/mod.rs` — `run_print` path uses wildcard `_ => {}` (~line 554)

Run `cargo test --bin dirge` after adding — 1264+ tests should pass.
§
Adding a new variant to `AgentEvent` in `src/event.rs` requires updating ALL exhaustive match arms across the codebase. Key locations: `src/ui/mod.rs` (event handler), `src/agent/agent_loop/bridge.rs` (tests and translate), `src/agent/agent_loop/h7_smoke.rs`, `src/agent/agent_loop/integration.rs`, `src/extras/acp/mod.rs`, `src/provider/mod.rs` (run_print path), and `src/agent/review.rs`. Use `cargo check` to find all non-exhaustive patterns — the compiler output lists every location.
§
Use targeted `#[cfg_attr(not(feature = "X")), allow(dead_code))]` on struct fields and enum variants that are only consumed by feature-gated code. This is preferred over module-level `#![allow(dead_code)]`. For fields used only by `acp` feature: `#[cfg_attr(not(feature = "acp"), allow(dead_code))]`. For fields used only by `plugin` feature: `#[cfg_attr(not(feature = "plugin"), allow(dead_code))]`. Test-only constants and functions should use `#[cfg(test)]` instead of `#[allow(dead_code)]`.
