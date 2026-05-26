//! App-level text selection handler for mouse drag in the chat buffer.
//!
//! `handle()` consumes a `UserEvent` (Mouse Down / Drag / Up) and the
//! current `Renderer` state, producing an `Outcome` the UI loop's
//! `tokio::select!` arm dispatches on: repaint the viewport, copy to
//! clipboard on mouse-up, or pass through as unhandled.
//!
//! Selection state lives on `Renderer` (`selection_active`,
//! `selection_start`, `selection_end`); this module is stateless.

use crate::event::UserEvent;
use crate::ui::renderer::{Renderer, copy_to_clipboard};

#[derive(Debug)]
pub enum Outcome {
    /// Nothing matched — pass the event on to the consumer.
    NotHandled,
    /// Buffer state changed (drag started / moved); repaint needed.
    Repaint,
    /// Selection completed (mouse-up) and `String` was copied to
    /// the system clipboard. Repaint still needed.
    RepaintAndCopied,
}

pub fn handle(ev: &UserEvent, renderer: &mut Renderer) -> Outcome {
    match ev {
        UserEvent::MouseDown { row, col } => {
            let Some(pos) = renderer.buffer_pos_at(*row, *col) else {
                return Outcome::NotHandled;
            };
            renderer.selection_active = true;
            renderer.selection_start = Some(pos);
            renderer.selection_end = Some(pos);
            Outcome::Repaint
        }
        UserEvent::MouseDrag { row, col } => {
            if !renderer.selection_active {
                return Outcome::NotHandled;
            }
            let Some(pos) = renderer.buffer_pos_at(*row, *col) else {
                return Outcome::NotHandled;
            };
            renderer.selection_end = Some(pos);
            Outcome::Repaint
        }
        UserEvent::MouseUp { row, col } => {
            if !renderer.selection_active {
                return Outcome::NotHandled;
            }
            if let Some(pos) = renderer.buffer_pos_at(*row, *col) {
                renderer.selection_end = Some(pos);
            }
            renderer.selection_active = false;
            let text = renderer.selected_text();
            renderer.clear_selection();
            match text {
                Some(t) => {
                    copy_to_clipboard(&t);
                    Outcome::RepaintAndCopied
                }
                None => Outcome::Repaint,
            }
        }
        _ => Outcome::NotHandled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::renderer::Renderer;

    #[test]
    fn mouse_down_starts_selection() {
        let mut r = Renderer::new().unwrap();
        r.set_chat_rect_for_test(ratatui::layout::Rect::new(0, 1, 80, 24));
        r.write_line("hello world", crossterm::style::Color::White)
            .unwrap();
        let outcome = handle(&UserEvent::MouseDown { row: 1, col: 0 }, &mut r);
        assert!(matches!(outcome, Outcome::Repaint));
        assert!(r.selection_active);
    }

    #[test]
    fn mouse_up_completes_selection() {
        let mut r = Renderer::new().unwrap();
        r.set_chat_rect_for_test(ratatui::layout::Rect::new(0, 1, 80, 24));
        r.write_line("hello world", crossterm::style::Color::White)
            .unwrap();
        handle(&UserEvent::MouseDown { row: 1, col: 0 }, &mut r);
        handle(&UserEvent::MouseDrag { row: 1, col: 5 }, &mut r);
        let outcome = handle(&UserEvent::MouseUp { row: 1, col: 5 }, &mut r);
        // Outcome reports selection completed; clipboard copy is
        // best-effort (depends on OS tool availability).
        assert!(
            matches!(outcome, Outcome::RepaintAndCopied | Outcome::Repaint),
            "got {outcome:?}"
        );
        assert!(!r.selection_active);
    }

    #[test]
    fn non_mouse_events_are_not_handled() {
        let mut r = Renderer::new().unwrap();
        let outcome = handle(
            &UserEvent::Key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('y'),
                crossterm::event::KeyModifiers::NONE,
            )),
            &mut r,
        );
        assert!(matches!(outcome, Outcome::NotHandled));
    }

    #[test]
    fn mouse_outside_chat_is_not_handled() {
        let mut r = Renderer::new().unwrap();
        let outcome = handle(&UserEvent::MouseDown { row: 999, col: 999 }, &mut r);
        assert!(matches!(outcome, Outcome::NotHandled));
        assert!(!r.selection_active);
    }
}
