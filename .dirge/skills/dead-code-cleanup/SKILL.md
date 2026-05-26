---
triggers:
  - "remove dead code"
  - "clean up allow(dead_code)"
  - "fix warnings"
  - "eliminate all warnings"
  - "no warnings"
---

# Dead Code Cleanup

Systematic removal of `#[allow(dead_code)]` and unused-code warnings from a Rust project.

## Philosophy

- **Remove legacy code entirely** — don't keep it around with `#[allow(dead_code)]`. If there's a new way, delete the old way.
- **Feature-gate targeted suppressions** — `#[cfg_attr(not(feature = "X"), allow(dead_code))]` is preferred over module-level `#![allow(dead_code)]`.
- **Test-only items use `#[cfg(test)]`** — not `#[allow(dead_code)]`.
- **API-surface items get plain `#[allow(dead_code)]` with doc comments** — for protocol-complete variants, hook context fields, etc.

## Workflow

### Step 1: Audit current state

```bash
cargo check --bin dirge 2>&1 | grep "^warning:" | wc -l
cargo check --bin dirge 2>&1 | grep "^warning:"
```

Categorize warnings into:
- **"never used"** — truly dead. Remove or gate with `#[cfg(test)]`.
- **"never read"** — fields only consumed by feature-gated paths. Use `#[cfg_attr(not(feature = "..."), allow(dead_code))]`.
- **"never constructed"** — protocol-complete variants. Add targeted `#[allow(dead_code)]` with doc comment.

### Step 2: Remove module-level suppressions

Remove `#![allow(dead_code)]` and `#![allow(unused_imports)]` from module files. The compiler will surface all real issues.

### Step 3: Handle "never used" items

For each "never used" warning, search all references:
```bash
grep -r "ITEM_NAME" src/
```

Decision tree:
- Only used in `#[cfg(test)]` blocks → gate ITEM with `#[cfg(test)]`
- Only used by itself (dead) → delete it
- Used in tests that test removed functionality → delete both
- Used nowhere but has tests that test it → delete tests first, then delete

### Step 4: Handle "never read" fields

These are almost always feature-gated consumption. Check which feature flag gate them:
- `acp` feature: `#[cfg_attr(not(feature = "acp"), allow(dead_code))]`
- `plugin` feature: `#[cfg_attr(not(feature = "plugin"), allow(dead_code))]`
- General API surface: plain `#[allow(dead_code)]` with doc comment

### Step 5: Handle "never constructed" variants

Protocol-complete variants (like `DeltaPhase::TextEnd`) that exist for defensive matching get:
```rust
/// Matched defensively in the bridge; never constructed by
/// current providers but kept for protocol completeness.
#[allow(dead_code)]
TextEnd,
```

### Step 6: Fix re-exports

When removing `#![allow(unused_imports)]` from `mod.rs`, the re-export block may need broad re-exports since many items are consumed only by tests or external crates. If the re-export list is large and many items are test-only, add `#![allow(unused_imports)]` back but ONLY on the re-export block, not the whole module.

### Step 7: Verify

```bash
cargo check --bin dirge 2>&1 | grep "^warning:" | wc -l
# Target: 0
cargo test --bin dirge
# Must pass
```

## Pitfalls

- **`cargo fix` can't fix all issues** — it only removes unused imports. Dead struct fields and variants need manual intervention.
- **Don't mark actively-constructed variants as dead** — `AgentEvent::Interjected` is constructed by the bridge (rig stream cancellation path), not legacy.
- **Test compilation errors cascade** — removing a function breaks its tests. Fix tests first or delete them together.
- **Re-export blocks matter** — items gated with `#[cfg(test)]` need a corresponding `#[cfg(test)]` re-export in `mod.rs`.

## Files commonly affected

- `src/event.rs` — `AgentEvent` variants and their fields
- `src/agent/agent_loop/mod.rs` — module-level suppressions and re-exports
- `src/agent/agent_loop/hooks.rs` — hook context struct fields (plugin-gated)
- `src/agent/agent_loop/message.rs` — `LoopEvent`, `DeltaPhase`, `LoopMessage`
- `src/agent/agent_loop/stream.rs` — `StreamOptions` fields
- `src/agent/agent_loop/context_manager.rs` — decision struct fields
- `src/agent/agent_loop/inflight.rs` — `InflightSet` methods
- `src/agent/agent_loop/tool.rs` — `LoopTool` trait methods
- `src/lsp/` — LSP module (progressive feature rollout)
- `src/plugin/loader.rs` — `LoadedPlugin` struct (plugin-gated)
