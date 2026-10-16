// Errors and warnings in boxes at the top right, the newest first, like snacks.nvim's notifier;
// they stay until dismissed. Other messages go to the status bar.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Stylize,
    text::Line,
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Widget},
};
use unicode_width::UnicodeWidthChar;

use crate::color::ColorTheme;

// shown at most, the oldest drop off
const MAX_NOTICES: usize = 5;
// the lines of a message shown, git's errors can be long
const MAX_LINES: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub error: bool,
    pub message: String,
}

// Adds a notice, the newest last; the same message again replaces the older one.
pub fn push_notice(notices: &mut Vec<Notice>, notice: Notice) {
    notices.retain(|n| *n != notice);
    notices.push(notice);
    let excess = notices.len().saturating_sub(MAX_NOTICES);
    notices.drain(..excess);
}

// Draws the notices in the top right of `area`, the newest on top; `dismiss` is the key hiding them.
pub fn render_notices(
    notices: &[Notice],
    dismiss: &str,
    area: Rect,
    buf: &mut Buffer,
    theme: &ColorTheme,
) {
    let width = (area.width / 3).clamp(30, 60).min(area.width);
    let text_width = width.saturating_sub(4) as usize;
    let mut y = area.y;
    for (i, notice) in notices.iter().rev().enumerate() {
        let (color, title) = if notice.error {
            (theme.status_error_fg, " \u{f057} Error ")
        } else {
            (theme.status_warn_fg, " \u{f071} Warning ")
        };
        let mut lines = wrap(&notice.message, text_width);
        if lines.len() > MAX_LINES {
            lines.truncate(MAX_LINES);
            lines[MAX_LINES - 1].push('…');
        }
        let height = lines.len() as u16 + 2;
        if y + height > area.bottom() {
            break;
        }
        let rect = Rect::new(area.right() - width, y, width, height);
        let mut block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(color)
            .title(Line::raw(title).fg(color).bold());
        // the key on the newest one
        if i == 0 {
            block = block.title_bottom(
                Line::raw(format!(" {dismiss} dismiss "))
                    .fg(theme.status_input_transient_fg)
                    .right_aligned(),
            );
        }
        Clear.render(rect, buf);
        let text: Vec<Line> = lines
            .into_iter()
            .map(|l| Line::raw(l).fg(theme.fg))
            .collect();
        Paragraph::new(text)
            .block(block.padding(ratatui::widgets::Padding::horizontal(1)))
            .bg(theme.bg)
            .render(rect, buf);
        y += height;
    }
}

// The lines of `text` in `width` columns, broken at spaces where it can, else anywhere.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for paragraph in text.lines() {
        let mut line = String::new();
        let mut line_width = 0;
        for word in paragraph.split(' ') {
            let word_width: usize = word.chars().filter_map(|c| c.width()).sum();
            let space = usize::from(!line.is_empty());
            if line_width + space + word_width > width && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                line_width = 0;
            }
            if !line.is_empty() {
                line.push(' ');
                line_width += 1;
            }
            // a word longer than the line goes on over the next ones
            for c in word.chars() {
                let w = c.width().unwrap_or(0);
                if line_width + w > width {
                    lines.push(std::mem::take(&mut line));
                    line_width = 0;
                }
                line.push(c);
                line_width += w;
            }
        }
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrap() {
        assert_eq!(wrap("a bb ccc", 4), ["a bb", "ccc"]);
        assert_eq!(wrap("abcdefg", 3), ["abc", "def", "g"]);
        assert_eq!(wrap("one\ntwo", 10), ["one", "two"]);
    }

    #[test]
    fn test_push_notice() {
        let notice = |m: &str| Notice {
            error: true,
            message: m.into(),
        };
        let mut notices = vec![];
        for m in ["a", "b", "a", "c", "d", "e", "f"] {
            push_notice(&mut notices, notice(m));
        }
        // "a" again moved after "b", then the oldest dropped off
        let messages: Vec<&str> = notices.iter().map(|n| n.message.as_str()).collect();
        assert_eq!(messages, ["a", "c", "d", "e", "f"]);
    }
}
