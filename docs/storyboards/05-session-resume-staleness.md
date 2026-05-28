# Session resume with staleness warning

The user runs `dirge -c` to resume the most recent session. If the
session's `working_dir` differs from cwd, or `updated_at` is more
than 24 hours old, dirge prints an informational stderr warning
before rendering the session. The resume itself proceeds normally.

## Flow

1. User runs `dirge -c` from a directory that differs from where the
   session was created.
2. `find_recent_sessions(1)` returns the newest session id;
   `load_session` parses the JSON and populates `loaded_mtime`.
3. `warn_on_stale_resume` compares `session.working_dir` against
   `std::env::current_dir()`. On mismatch, prints a warning naming
   both paths.
4. Same function parses `session.updated_at` as RFC3339 and computes
   age. If `>= 24h`, prints an age warning.
5. `render_session` prints the banner, compaction count if any, and
   walks `session.messages` rendering each as `<you>` / `<dirge>` /
   `<sys>`. The user can type immediately.

## Implementation

- `src/main.rs` — `--continue` / `-c` resolution; calls
  `session::storage::find_recent_sessions` and `load_session`.
- `src/main.rs::warn_on_stale_resume` — emits both warnings on stderr.
- `src/session/storage.rs::load_session` — JSON parse, schema
  migrations, `loaded_mtime` population for concurrent-writer check.
- `src/ui/events.rs::render_session` — banner + message replay.

## Edge cases

- First-time use (`-c` with no prior session): `find_recent_sessions`
  returns empty; `warn_on_stale_resume` is never called.
- Same cwd: working-dir warning skipped; age warning still possible.
- Sub-24h age: age warning skipped; working-dir warning still possible.
- Corrupted `updated_at`: RFC3339 parse fails; age warning silently
  skipped — defensive.
- Empty `working_dir` (very old session pre-dating the field):
  comparison is skipped.
