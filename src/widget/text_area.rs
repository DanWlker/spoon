use ratatui::{
    buffer::Buffer, crossterm::event::KeyEvent, layout::Rect, style::Style, widgets::Widget,
};
use ratatui_textarea::{CursorMove, WrapMode};

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
pub struct TextArea(ratatui_textarea::TextArea<'static>);

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
        Self(inner)
    }

    pub fn text(&self) -> String {
        self.0.lines().join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    // (row, column in chars)
    #[cfg(test)]
    pub fn cursor(&self) -> (usize, usize) {
        let cursor = self.0.cursor();
        (cursor.0, cursor.1)
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        self.0.input(key);
    }

    // Draws the text in `style`: while typing with the caret, scrolled to it, else from the start.
    pub fn render(&self, area: Rect, buf: &mut Buffer, style: Style, caret: Caret, lines: Lines) {
        // a copy to style, the text is short
        let mut inner = self.0.clone();
        inner.set_style(style);
        if lines == Lines::Scrolled {
            inner.set_wrap_mode(WrapMode::None);
        }
        if caret == Caret::Hidden {
            inner.set_cursor_style(style);
            inner.move_cursor(CursorMove::Top);
            inner.move_cursor(CursorMove::Head);
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
}
