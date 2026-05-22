//! Central soft-wrap helper for any long text the UI prints.
//!
//! Single chokepoint so every render path (question prompts, chamber
//! rows, free-form messages) shares the same wrap policy: word-aware
//! when whitespace is available, character-fallback for unbreakable
//! runs (URLs, paths, code), display-width-aware (CJK / emoji), and
//! continuation-indent support so wrapped option text lines up under
//! its first character.
//!
//! Policy:
//!   - Width is measured with `UnicodeWidthStr` so wide glyphs count
//!     correctly; the result is suitable for fixed-column layouts
//!     like chamber rows or aligned bullet lists.
//!   - Hard newlines in the input are preserved as line breaks.
//!   - Continuation lines (every wrapped line after the first) are
//!     prefixed with `continuation_indent` so a wrapped option's
//!     extra lines visually align under the option's body.
//!   - When a single word is longer than the width budget, break it
//!     at the width boundary (display-width aware) rather than
//!     overflowing.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Soft-wrap `text` to `max_width` columns. Returns one entry per
/// visual line. Hard newlines in the input become hard line breaks
/// in the output.
///
/// `continuation_indent` is prepended to every line after the first
/// of each logical line; pass `""` for no indent. The indent's own
/// width counts against `max_width`, so the wrapping accounts for
/// it. Passing an indent wider than `max_width` is treated as no
/// indent (degenerate config).
pub fn soft_wrap(text: &str, max_width: usize, continuation_indent: &str) -> Vec<String> {
    if max_width == 0 {
        return text.lines().map(|l| l.to_string()).collect();
    }
    let cont_w = UnicodeWidthStr::width(continuation_indent);
    let effective_indent = if cont_w >= max_width {
        ""
    } else {
        continuation_indent
    };
    let cont_w = UnicodeWidthStr::width(effective_indent);

    let mut out: Vec<String> = Vec::new();
    for logical in text.split('\n') {
        if logical.is_empty() {
            out.push(String::new());
            continue;
        }
        wrap_logical_line(logical, max_width, effective_indent, cont_w, &mut out);
    }
    out
}

fn wrap_logical_line(
    line: &str,
    max_width: usize,
    cont_indent: &str,
    cont_w: usize,
    out: &mut Vec<String>,
) {
    // Width budget on each output row depends on whether it's the
    // first row of this logical line (no indent) or a continuation
    // (indent counted).
    let mut current = String::new();
    let mut current_w = 0usize;
    let mut is_first_row = true;

    let push_row = |out: &mut Vec<String>, current: &mut String, is_first: &mut bool| {
        if *is_first {
            out.push(std::mem::take(current));
            *is_first = false;
        } else {
            let mut s = String::with_capacity(cont_indent.len() + current.len());
            s.push_str(cont_indent);
            s.push_str(current);
            out.push(s);
            current.clear();
        }
    };

    // Tokenize on whitespace runs. Each token carries its leading
    // whitespace (if any) so we can decide whether to break on the
    // space when the token is the first of a new row.
    let tokens = tokenize(line);
    for token in tokens {
        let budget = if is_first_row {
            max_width
        } else {
            max_width.saturating_sub(cont_w)
        };

        let tok_w = UnicodeWidthStr::width(token.text);
        let ws_w = UnicodeWidthStr::width(token.leading_ws);

        // If current is empty (start of a row), drop the leading
        // whitespace — no point indenting just because we wrapped.
        if current.is_empty() {
            if tok_w <= budget {
                current.push_str(token.text);
                current_w = tok_w;
            } else {
                // Token by itself overflows the budget. Hard-break
                // it across however many rows are needed.
                break_long_token(
                    token.text,
                    budget,
                    max_width.saturating_sub(cont_w).max(1),
                    &mut current,
                    &mut current_w,
                    out,
                    cont_indent,
                    &mut is_first_row,
                );
            }
            continue;
        }

        // Does the token (with its leading whitespace) fit on the
        // current row?
        if current_w + ws_w + tok_w <= budget {
            current.push_str(token.leading_ws);
            current.push_str(token.text);
            current_w += ws_w + tok_w;
        } else {
            // Flush the row, start a new continuation row.
            push_row(out, &mut current, &mut is_first_row);
            current_w = 0;
            let new_budget = max_width.saturating_sub(cont_w).max(1);
            if tok_w <= new_budget {
                current.push_str(token.text);
                current_w = tok_w;
            } else {
                break_long_token(
                    token.text,
                    new_budget,
                    new_budget,
                    &mut current,
                    &mut current_w,
                    out,
                    cont_indent,
                    &mut is_first_row,
                );
            }
        }
    }

    if !current.is_empty() || out.is_empty() {
        push_row(out, &mut current, &mut is_first_row);
    }
}

struct Token<'a> {
    leading_ws: &'a str,
    text: &'a str,
}

fn tokenize(line: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let ws_start = i;
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }
        let ws_end = i;
        let word_start = i;
        while i < bytes.len() && bytes[i] != b' ' && bytes[i] != b'\t' {
            // Advance by char boundary; bytes might be multi-byte.
            let ch_len = utf8_char_len(bytes[i]);
            i += ch_len;
            if i > bytes.len() {
                i = bytes.len();
            }
        }
        if word_start < bytes.len() {
            tokens.push(Token {
                leading_ws: &line[ws_start..ws_end],
                text: &line[word_start..i],
            });
        } else if ws_start < ws_end {
            // Trailing whitespace (no word follows). Treat as an
            // empty-text token so the indent isn't lost on the row.
            tokens.push(Token {
                leading_ws: &line[ws_start..ws_end],
                text: "",
            });
        }
    }
    tokens
}

fn utf8_char_len(first_byte: u8) -> usize {
    if first_byte < 0x80 {
        1
    } else if first_byte < 0xC0 {
        1 // continuation byte alone (shouldn't happen on a valid str)
    } else if first_byte < 0xE0 {
        2
    } else if first_byte < 0xF0 {
        3
    } else {
        4
    }
}

/// Break a single token wider than the row budget across rows.
/// Walks chars summing display widths so a multi-cell glyph never
/// overflows. The first row uses `first_budget`, every subsequent
/// row uses `continuation_budget` (which already excludes the
/// continuation indent width).
#[allow(clippy::too_many_arguments)]
fn break_long_token(
    token: &str,
    first_budget: usize,
    continuation_budget: usize,
    current: &mut String,
    current_w: &mut usize,
    out: &mut Vec<String>,
    cont_indent: &str,
    is_first_row: &mut bool,
) {
    let mut remaining_budget = first_budget;
    for ch in token.chars() {
        let cw = ch.width().unwrap_or(0);
        if cw > remaining_budget {
            // Row full. Flush + start a new continuation row.
            if *is_first_row {
                out.push(std::mem::take(current));
                *is_first_row = false;
            } else {
                let mut s = String::with_capacity(cont_indent.len() + current.len());
                s.push_str(cont_indent);
                s.push_str(current);
                out.push(s);
                current.clear();
            }
            *current_w = 0;
            remaining_budget = continuation_budget;
        }
        current.push(ch);
        *current_w += cw;
        remaining_budget = remaining_budget.saturating_sub(cw);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_line_returns_unchanged() {
        let out = soft_wrap("hello world", 80, "");
        assert_eq!(out, vec!["hello world"]);
    }

    #[test]
    fn wraps_on_word_boundary_not_midword() {
        let out = soft_wrap("the quick brown fox jumps", 12, "");
        // Each line must NOT split mid-word.
        for line in &out {
            for word in line.split_whitespace() {
                assert!(word.len() <= 12, "word {word:?} fits its row");
            }
        }
        // First row should be "the quick" (9 chars), not "the quick br".
        assert_eq!(out[0], "the quick");
    }

    #[test]
    fn preserves_hard_newlines() {
        let out = soft_wrap("line one\nline two", 80, "");
        assert_eq!(out, vec!["line one", "line two"]);
    }

    #[test]
    fn applies_continuation_indent() {
        let out = soft_wrap("aaa bbb ccc ddd", 7, "  ");
        // First row no indent; subsequent rows get "  " prefix.
        assert_eq!(out[0], "aaa bbb");
        for line in &out[1..] {
            assert!(line.starts_with("  "));
        }
    }

    #[test]
    fn hard_breaks_unbreakable_long_token() {
        let out = soft_wrap("aaaaaaaaaaaaaaaaaa", 5, "");
        // 18-char token across 5-wide rows = 4 rows.
        assert_eq!(out.len(), 4);
        assert!(out.iter().all(|l| l.len() <= 5));
    }

    #[test]
    fn empty_input_returns_one_empty_row() {
        let out = soft_wrap("", 80, "");
        assert_eq!(out, vec![""]);
    }

    #[test]
    fn zero_width_returns_unwrapped_lines() {
        let out = soft_wrap("anything goes", 0, "");
        assert_eq!(out, vec!["anything goes"]);
    }

    /// CJK glyphs are width-2. A 6-cell budget fits 3 CJK chars, not 6.
    #[test]
    fn respects_display_width_for_cjk() {
        let out = soft_wrap("中文测试abc", 6, "");
        // First row: at most 3 CJK chars (6 cells), or 2 CJK + " " + "ab".
        // What we care about: no row's display width exceeds 6.
        for line in &out {
            assert!(
                UnicodeWidthStr::width(line.as_str()) <= 6,
                "row {line:?} width = {} <= 6",
                UnicodeWidthStr::width(line.as_str()),
            );
        }
    }

    /// Pathological indent wider than budget should fall back to no
    /// indent rather than spinning or panicking.
    #[test]
    fn indent_wider_than_width_degrades_gracefully() {
        let out = soft_wrap("aaa bbb ccc", 4, "            ");
        // Should still produce output and not panic.
        assert!(!out.is_empty());
    }
}
