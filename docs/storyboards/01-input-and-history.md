# Input, history, and draft preservation

The user is mid-draft, recalls earlier prompts via `Up`, then returns
to the original draft with `Down`. The cursor lands where it started.

## Flow

1. User types text into the input box. Each keystroke appends to the
   buffer and advances the cursor.
2. User presses `Up`. The current buffer + cursor are stashed as the
   draft; the most-recent history entry is loaded; cursor goes to end.
3. User presses `Up` again. History index decrements; the stashed
   draft is left untouched.
4. User presses `Down`. History index advances back toward newer
   entries.
5. User presses `Down` past the newest entry. The stashed draft is
   restored, including the original cursor position (clamped to
   buffer length).

## Implementation

- `src/ui/input.rs::InputEditor::handle_event` — keystroke routing.
- `src/ui/input.rs::InputEditor::history_up` — stash-on-first-up,
  decrement on subsequent ups.
- `src/ui/input.rs::InputEditor::history_down` — advance, then
  restore stash when going past newest.
- `src/ui/input.rs::InputEditor::set_text` — `/fork` and similar
  clear the stash because the inserted text becomes the new draft.

## Edge cases

- Cursor past end of restored buffer: clamped via
  `cursor.min(self.buffer.len())`.
- Multi-line drafts: buffer is a flat `\n`-joined string; stash/restore
  is a straight clone.
- Submit clears the stash alongside `history_pos`. A subsequent `Up`
  stashes the empty buffer and restores cleanly on `Down` past end.
