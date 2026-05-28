# Storyboards

Step-by-step walkthroughs of user-facing flows in dirge. Each file
describes what the user does, what they see, and which code paths
run. Useful for onboarding new contributors and as a regression
checklist when refactoring.

## Current storyboards

| File | Flow |
|---|---|
| [01-input-and-history.md](01-input-and-history.md) | Typing a message, history navigation, draft preservation |
| [02-permission-ask.md](02-permission-ask.md) | Permission prompt on a `bash` write — Allow once / Allow always / Deny |
| [03-background-task-notification.md](03-background-task-notification.md) | Background task completes; next turn sees a notification without leaking session state into the visible echo |
| [04-compress-with-focus.md](04-compress-with-focus.md) | `/compress <focus>` — focus-topic compaction |
| [05-session-resume-staleness.md](05-session-resume-staleness.md) | `dirge -c` resume with stale `working_dir` / `updated_at` warning |
| [06-webfetch-ssrf-defense.md](06-webfetch-ssrf-defense.md) | Agent tries to fetch a private URL — multi-layer SSRF defense fires |
