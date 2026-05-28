# `/compress <focus>` focus-topic compaction

The user runs `/compress permission layer refactor`. The text after
the command becomes the focus topic. The compactor produces a summary
that allocates ~60-70% of its budget to the focus topic and one-line
each to the other threads. The summary replaces older turns; the
recent tail is preserved.

## Flow

1. User submits `/compress permission layer refactor`.
2. Slash parser joins `parts[1..]` into the focus string and returns
   a `DEFER_COMPRESS:<focus>` sentinel.
3. `handle_compress` selects `messages_to_summarize` (everything below
   the tail that fits in `keep_recent_tokens`) and calls
   `compress_messages(model, msgs, prev_summary, Some(focus))`.
4. `compress_messages` builds an `instructions_block` with the focus
   framing and substitutes it into `COMPACTION_PROMPT`.
5. Summarizer runs against the configured auxiliary model. The
   prompt is head-tail truncated to 128 KiB if oversized.
6. `handle_compress` pushes the summary into `session.compactions`
   and rewrites `session.messages` to keep head + summary + tail.
7. Next `convert_history` emits `Message::system("[Previous
   conversation summary]\n…")` plus the preserved tail.

## Implementation

- `src/ui/slash/mod.rs` — `/compress` / `/compact` parsing; emits
  `DEFER_COMPRESS:<focus>` sentinel.
- `src/ui/slash/mod.rs::handle_compress` — selects messages, calls
  the provider, writes the summary into the session.
- `src/provider/mod.rs::compress_messages` — focus framing in
  `instructions_block`; substitutes into `COMPACTION_PROMPT`.
- `src/agent/compression::build_summary_prompt` — direct Rust-built
  prompt for the auto-compact path; accepts `focus_topic`.
- `src/agent/compression::run_compaction_pass_with_focus` —
  loop-driven auto-compact at >75% context usage.
- `src/session/mod.rs::Session::compacted_context` — reads compaction
  for prompt assembly.

## Edge cases

- Empty or whitespace-only focus: `text.trim()` is empty →
  `instructions_block` is `"(none)"`; default summary structure.
- Focus containing quotes: interpolated raw; the LLM sees the literal
  quotes.
- Focus + previous compaction: previous summary is fed via
  `previous_summary`; the update-summary branch fires; focus framing
  applies to both new-summary and update-summary branches.
