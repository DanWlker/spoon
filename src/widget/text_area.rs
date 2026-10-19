use std::cell::Cell;

use ratatui::{
    buffer::Buffer, crossterm::event::KeyEvent, layout::Rect, style::Style, widgets::Widget,
};
use ratatui_textarea::{CursorMove, WrapMode};

use crate::event::UserEvent;

// Whether the text shows the caret, while it is typed into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caret {
    Shown,
    Hidden,
}

// How long lines show: wrapped, or scrolled sideways like a one-line input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lines {
    Wrapped,
    Scrolled,
}

// The commit message editor: ratatui-textarea, with its Emacs-like keys, undo (ctrl-u), redo
// (ctrl-r) and moving by word; long lines wrap at words.
#[derive(Debug, Clone)]
pub struct TextArea {
    inner: ratatui_textarea::TextArea<'static>,
    // rows scrolled from the top while the text is shown without the caret
    scroll: i16,
    // the rows last drawn, for a page or half a page
    height: Cell<u16>,
}

impl Default for TextArea {
    fn default() -> Self {
        Self::new("")
    }
}

impl TextArea {
    // The cursor is placed at the end of the text.
    pub fn new(text: &str) -> Self {
        let mut inner =
            ratatui_textarea::TextArea::new(text.split('\n').map(String::from).collect());
        inner.set_wrap_mode(WrapMode::WordOrGlyph);
        inner.set_cursor_line_style(Style::default());
        inner.move_cursor(CursorMove::Bottom);
        inner.move_cursor(CursorMove::End);
        Self {
            inner,
            scroll: 0,
            height: Cell::new(0),
        }
    }

    pub fn text(&self) -> String {
        self.inner.lines().join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    // (row, column in chars)
    #[cfg(test)]
    pub fn cursor(&self) -> (usize, usize) {
        let cursor = self.inner.cursor();
        (cursor.0, cursor.1)
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        // typing shows the caret, which the viewport follows again
        self.scroll = 0;
        self.inner.input(key);
    }

    // Scrolls the text by the scroll keys, shown without the caret; returns whether `event` was
    // one of them.
    pub fn scroll(&mut self, event: UserEvent) -> bool {
        let page = self.height.get().max(1) as i16;
        // the last row the text can be scrolled to, from the lines it has
        let last = self.inner.lines().len().max(1) as i16;
        let max = (last - page).max(0);
        let rows = match event {
            UserEvent::ScrollDown => 1,
            UserEvent::ScrollUp => -1,
            UserEvent::HalfPageDown => page / 2,
            UserEvent::HalfPageUp => -page / 2,
            UserEvent::PageDown => page,
            UserEvent::PageUp => -page,
            UserEvent::GoToTop => {
                self.scroll = 0;
                return true;
            }
            UserEvent::GoToBottom => {
                self.scroll = max;
                return true;
            }
            _ => return false,
        };
        self.scroll = self.scroll.saturating_add(rows).clamp(0, max);
        true
    }

    // Draws the text in `style`: while typing with the caret, scrolled to it, else from the last
    // rows scrolled to by the scroll keys.
    pub fn render(&self, area: Rect, buf: &mut Buffer, style: Style, caret: Caret, lines: Lines) {
        self.height.set(area.height);
        // a copy to style and scroll, the text is short
        let mut inner = self.inner.clone();
        inner.set_style(style);
        if lines == Lines::Scrolled {
            inner.set_wrap_mode(WrapMode::None);
        }
        if caret == Caret::Hidden {
            inner.set_cursor_style(style);
            inner.move_cursor(CursorMove::Top);
            inner.move_cursor(CursorMove::Head);
            if self.scroll != 0 {
                inner.scroll((self.scroll, 0));
            }
        }
        inner.render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    fn type_keys(text_area: &mut TextArea, codes: &[KeyCode]) {
        for code in codes {
            text_area.handle_key(KeyEvent::from(*code));
        }
    }

    #[test]
    fn test_editing() {
        let mut text_area = TextArea::default();
        assert!(text_area.is_empty());

        type_keys(
            &mut text_area,
            &[
                KeyCode::Char('f'),
                KeyCode::Char('ö'),
                KeyCode::Char('x'),
                KeyCode::Enter,
                KeyCode::Char('b'),
            ],
        );
        assert_eq!(text_area.text(), "föx\nb");
        assert_eq!(text_area.cursor(), (1, 1));

        // up keeps the column, then edit in the middle of a multi-byte line
        type_keys(
            &mut text_area,
            &[KeyCode::Up, KeyCode::Right, KeyCode::Backspace],
        );
        assert_eq!(text_area.text(), "fx\nb");

        // backspace at the start of a line joins it with the previous one
        type_keys(
            &mut text_area,
            &[KeyCode::Down, KeyCode::Home, KeyCode::Backspace],
        );
        assert_eq!(text_area.text(), "fxb");
        assert_eq!(text_area.cursor(), (0, 2));

        // ctrl-u undoes
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        text_area.handle_key(ctrl('u'));
        assert_eq!(text_area.text(), "fx\nb");

        assert_eq!(TextArea::new("a\nbc").cursor(), (1, 2));
    }

    #[test]
    fn test_scrolls_the_shown_rows() {
        let mut text = TextArea::new("a\nb\nc\nd\ne");
        let area = Rect::new(0, 0, 10, 3);
        let style = Style::default();
        let first = |text: &TextArea| {
            let mut buf = Buffer::empty(area);
            text.render(area, &mut buf, style, Caret::Hidden, Lines::Wrapped);
            let row: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
            row.trim().to_string()
        };
        assert_eq!(first(&text), "a");
        // a line, a page (clamped to the last rows: c, d, e), and back to the top
        assert!(text.scroll(UserEvent::ScrollDown));
        assert_eq!(first(&text), "b");
        assert!(text.scroll(UserEvent::PageDown));
        assert_eq!(first(&text), "c");
        assert!(text.scroll(UserEvent::GoToTop));
        assert_eq!(first(&text), "a");
        assert!(text.scroll(UserEvent::GoToBottom));
        assert_eq!(first(&text), "c");
        // typing shows the caret again, from the top
        text.handle_key(KeyEvent::from(KeyCode::Char('x')));
        assert_eq!(first(&text), "a");
    }
}
