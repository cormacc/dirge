//! Chat region widget.
//!
//! Paints chat scrollback into the `Layout::chat` rect plus the two
//! vertical ║ borders at `chat_v_left_col` / `chat_v_right_col`. The
//! widget owns the verticals because they extend the full chat
//! height — making the top frame paint the corners and this widget
//! the body keeps one source of truth for each row's content.
//!
//! Selection rendering, mouse mapping, and ANSI escape parsing in
//! `LineEntry.text` are deferred to a follow-up — this phase covers
//! plain-text rendering + color + scroll offset, which is enough to
//! port the streaming chat path.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color as RColor, Modifier, Style};
use ratatui::widgets::Widget;

use super::layout::Layout;
use crate::ui::renderer::LineEntry;

/// Render the chat region from a slice of `LineEntry` lines.
///
/// `scroll_offset` is the number of lines from the END of the
/// buffer to skip (0 = show newest). Matches the legacy renderer's
/// `Renderer::scroll_offset` semantics so the migration can swap
/// paint paths without changing state.
#[derive(Clone, Copy)]
pub struct ChatPane<'a> {
    layout: &'a Layout,
    lines: &'a [LineEntry],
    scroll_offset: usize,
    /// Style for the chat ║ verticals.
    border_style: Style,
}

impl<'a> ChatPane<'a> {
    pub fn new(layout: &'a Layout, lines: &'a [LineEntry], scroll_offset: usize) -> Self {
        Self {
            layout,
            lines,
            scroll_offset,
            border_style: Style::default().fg(RColor::Green),
        }
    }

    /// Override the ║ border style. Default is `Color::Green`.
    pub fn border_style(mut self, style: Style) -> Self {
        self.border_style = style;
        self
    }
}

impl<'a> Widget for ChatPane<'a> {
    fn render(self, _area: Rect, buf: &mut Buffer) {
        let l = self.layout;
        let visible = l.chat.height as usize;

        // ── chat ║ verticals on every row of the chat band ──
        for dy in 0..l.chat.height {
            let y = l.chat.y + dy;
            if l.chat_v_left_col < buf.area.width {
                buf[(l.chat_v_left_col, y)]
                    .set_char('║')
                    .set_style(self.border_style);
            }
            if l.chat_v_right_col < buf.area.width {
                buf[(l.chat_v_right_col, y)]
                    .set_char('║')
                    .set_style(self.border_style);
            }
        }

        // ── chat body ──
        if visible == 0 || l.chat.width == 0 || self.lines.is_empty() {
            return;
        }
        // Window the buffer slice: take `visible` lines ending at
        // (total - scroll_offset). When scroll_offset > 0 the user
        // has scrolled up so we show an older window.
        let total = self.lines.len();
        let end = total.saturating_sub(self.scroll_offset);
        let start = end.saturating_sub(visible);
        let slice = &self.lines[start..end];
        for (i, entry) in slice.iter().enumerate() {
            let y = l.chat.y + i as u16;
            paint_line(buf, l.chat.x, y, l.chat.width, entry);
        }
    }
}

/// Write `entry.text` into the chat row at `(x, y)`, clipped to
/// `width` cells, styled with the entry's color.
fn paint_line(buf: &mut Buffer, x: u16, y: u16, width: u16, entry: &LineEntry) {
    if width == 0 {
        return;
    }
    let style = Style::default().fg(crossterm_to_ratatui(entry.color));
    // ratatui's `set_stringn` clips at the requested width and
    // returns the actual end position — exactly the semantics we
    // need to keep the chat content from spilling into the ║
    // border.
    buf.set_stringn(x, y, entry.text.as_str(), width as usize, style);
}

/// Translate a crossterm color into ratatui's equivalent. The two
/// enums are isomorphic for the variants we use; falls back to
/// Reset/Reset for anything exotic.
pub fn crossterm_to_ratatui(c: crossterm::style::Color) -> RColor {
    use crossterm::style::Color as CC;
    match c {
        CC::Reset => RColor::Reset,
        CC::Black => RColor::Black,
        CC::DarkGrey => RColor::DarkGray,
        CC::Red => RColor::Red,
        CC::DarkRed => RColor::LightRed,
        CC::Green => RColor::Green,
        CC::DarkGreen => RColor::LightGreen,
        CC::Yellow => RColor::Yellow,
        CC::DarkYellow => RColor::LightYellow,
        CC::Blue => RColor::Blue,
        CC::DarkBlue => RColor::LightBlue,
        CC::Magenta => RColor::Magenta,
        CC::DarkMagenta => RColor::LightMagenta,
        CC::Cyan => RColor::Cyan,
        CC::DarkCyan => RColor::LightCyan,
        CC::White => RColor::White,
        CC::Grey => RColor::Gray,
        CC::Rgb { r, g, b } => RColor::Rgb(r, g, b),
        CC::AnsiValue(v) => RColor::Indexed(v),
    }
}

// Suppress the not-yet-used modifier import — selection rendering
// in a follow-up will use it (reverse video for selected runs).
const _: Modifier = Modifier::REVERSED;

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::style::Color as CC;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn line(text: &str, color: CC) -> LineEntry {
        LineEntry {
            text: text.into(),
            color,
        }
    }

    /// ║ borders appear on every chat row even when the buffer
    /// is empty.
    #[test]
    fn renders_borders_on_empty_buffer() {
        let layout = Layout::new(160, 30, 1);
        let mut backend = TestBackend::new(160, 30);
        let mut terminal = Terminal::new(backend.clone()).unwrap();
        let lines: Vec<LineEntry> = vec![];
        terminal
            .draw(|f| {
                let area = f.area();
                f.render_widget(ChatPane::new(&layout, &lines, 0), area);
            })
            .unwrap();
        backend = terminal.backend().clone();

        for dy in 0..layout.chat.height {
            let y = layout.chat.y + dy;
            assert_eq!(
                backend
                    .buffer()
                    .cell((layout.chat_v_left_col, y))
                    .unwrap()
                    .symbol(),
                "║",
                "missing left ║ at row {y}"
            );
            assert_eq!(
                backend
                    .buffer()
                    .cell((layout.chat_v_right_col, y))
                    .unwrap()
                    .symbol(),
                "║",
                "missing right ║ at row {y}"
            );
        }
    }

    /// Lines paint into the chat rect, starting at chat.y. Text is
    /// clipped to chat.width so it cannot overwrite the right ║.
    #[test]
    fn paints_buffer_lines_into_chat_rect() {
        let layout = Layout::new(160, 30, 1);
        let mut backend = TestBackend::new(160, 30);
        let mut terminal = Terminal::new(backend.clone()).unwrap();
        let lines = vec![
            line("hello", CC::Green),
            line("world", CC::Cyan),
        ];
        terminal
            .draw(|f| {
                let area = f.area();
                f.render_widget(ChatPane::new(&layout, &lines, 0), area);
            })
            .unwrap();
        backend = terminal.backend().clone();

        // Lines paint TOP-anchored at chat.y, chat.y + 1, ...
        // (matches the legacy renderer's render_viewport loop —
        // when total_lines < visible, content fills the top rows
        // and the bottom rows stay blank).
        let row_hello = layout.chat.y;
        let row_world = row_hello + 1;
        // Read the first 5 cells of each row.
        let read = |y: u16, w: u16| -> String {
            (0..w)
                .map(|i| {
                    backend
                        .buffer()
                        .cell((layout.chat.x + i, y))
                        .unwrap()
                        .symbol()
                        .to_string()
                })
                .collect()
        };
        assert_eq!(read(row_hello, 5), "hello");
        assert_eq!(read(row_world, 5), "world");
    }

    /// Long text is clipped at chat.width and never touches the
    /// right ║ column.
    #[test]
    fn long_line_clips_at_chat_width() {
        let layout = Layout::new(40, 10, 1);
        // chat.width = 38 (narrow terminal, full chat band).
        let mut backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend.clone()).unwrap();
        let long = "x".repeat(200);
        let lines = vec![line(&long, CC::Green)];
        terminal
            .draw(|f| {
                let area = f.area();
                f.render_widget(ChatPane::new(&layout, &lines, 0), area);
            })
            .unwrap();
        backend = terminal.backend().clone();

        // Single line lands on the top row of the chat band.
        let row = layout.chat.y;
        // chat.x .. chat.x + chat.width should be 'x'.
        for i in 0..layout.chat.width {
            assert_eq!(
                backend
                    .buffer()
                    .cell((layout.chat.x + i, row))
                    .unwrap()
                    .symbol(),
                "x",
                "expected 'x' at col {} (chat content)",
                layout.chat.x + i
            );
        }
        // Right ║ must NOT be overwritten.
        assert_eq!(
            backend
                .buffer()
                .cell((layout.chat_v_right_col, row))
                .unwrap()
                .symbol(),
            "║"
        );
    }

    /// scroll_offset shifts which lines are visible.
    #[test]
    fn scroll_offset_windows_older_lines() {
        let layout = Layout::new(160, 30, 1); // chat.height = 24
        let mut backend = TestBackend::new(160, 30);
        let mut terminal = Terminal::new(backend.clone()).unwrap();
        // 30 lines named "L0".."L29"; with scroll_offset = 5 the
        // window is lines[30-5-24 .. 30-5] = lines[1..25]. Painted
        // top-anchored: row chat.y → L1, row chat.y + 23 → L24.
        let lines: Vec<LineEntry> = (0..30)
            .map(|i| line(&format!("L{i}"), CC::Green))
            .collect();
        terminal
            .draw(|f| {
                let area = f.area();
                f.render_widget(ChatPane::new(&layout, &lines, 5), area);
            })
            .unwrap();
        backend = terminal.backend().clone();

        let row_top = layout.chat.y;
        let row_bot = layout.chat.y + layout.chat.height - 1;
        let read = |y: u16, w: u16| -> String {
            (0..w)
                .map(|i| {
                    backend
                        .buffer()
                        .cell((layout.chat.x + i, y))
                        .unwrap()
                        .symbol()
                        .to_string()
                })
                .collect()
        };
        assert_eq!(read(row_top, 3), "L1 ", "top visible row should be L1");
        assert_eq!(read(row_bot, 3), "L24", "bottom visible row should be L24");
    }

    /// crossterm → ratatui color translation covers the common
    /// theme colors the renderer uses.
    #[test]
    fn color_translation_covers_theme_palette() {
        assert_eq!(crossterm_to_ratatui(CC::Green), RColor::Green);
        assert_eq!(crossterm_to_ratatui(CC::Cyan), RColor::Cyan);
        assert_eq!(crossterm_to_ratatui(CC::DarkMagenta), RColor::LightMagenta);
        assert_eq!(crossterm_to_ratatui(CC::Yellow), RColor::Yellow);
        assert_eq!(crossterm_to_ratatui(CC::Rgb { r: 1, g: 2, b: 3 }), RColor::Rgb(1, 2, 3));
        assert_eq!(crossterm_to_ratatui(CC::AnsiValue(42)), RColor::Indexed(42));
    }
}
