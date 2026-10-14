use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

// A minimal multi-line text editor; the cursor column counts chars, not bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextArea {
    lines: Vec<String>,
    row: usize,
    col: usize,
}

impl Default for TextArea {
    fn default() -> Self {
        Self::new("")
    }
}

impl TextArea {
    // The cursor is placed at the end of the text.
    pub fn new(text: &str) -> Self {
        let lines: Vec<String> = text.split('\n').map(String::from).collect();
        let row = lines.len() - 1;
        let col = lines[row].chars().count();
        Self { lines, row, col }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    // (row, column in chars)
    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        let ctrl_or_alt = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Char(c) if !ctrl_or_alt => self.insert_char(c),
            KeyCode::Enter => self.insert_newline(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left => self.move_left(),
            KeyCode::Right => self.move_right(),
            KeyCode::Up => self.move_vertically(-1),
            KeyCode::Down => self.move_vertically(1),
            KeyCode::Home => self.col = 0,
            KeyCode::End => self.col = self.line_len(),
            _ => {}
        }
    }

    fn line_len(&self) -> usize {
        self.lines[self.row].chars().count()
    }

    fn byte_index(&self) -> usize {
        let line = &self.lines[self.row];
        line.char_indices()
            .nth(self.col)
            .map_or(line.len(), |(i, _)| i)
    }

    fn insert_char(&mut self, c: char) {
        let i = self.byte_index();
        self.lines[self.row].insert(i, c);
        self.col += 1;
    }

    fn insert_newline(&mut self) {
        let i = self.byte_index();
        let rest = self.lines[self.row].split_off(i);
        self.lines.insert(self.row + 1, rest);
        self.row += 1;
        self.col = 0;
    }

    fn backspace(&mut self) {
        if self.col > 0 {
            self.col -= 1;
            let i = self.byte_index();
            self.lines[self.row].remove(i);
        } else if self.row > 0 {
            let line = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.line_len();
            self.lines[self.row].push_str(&line);
        }
    }

    fn delete(&mut self) {
        if self.col < self.line_len() {
            let i = self.byte_index();
            self.lines[self.row].remove(i);
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
        }
    }

    fn move_left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.line_len();
        }
    }

    fn move_right(&mut self) {
        if self.col < self.line_len() {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    fn move_vertically(&mut self, delta: isize) {
        let row = self.row as isize + delta;
        if row >= 0 && (row as usize) < self.lines.len() {
            self.row = row as usize;
            self.col = self.col.min(self.line_len());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

        // ctrl combinations are not typed
        text_area.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(text_area.text(), "fxb");

        assert_eq!(TextArea::new("a\nbc").cursor(), (1, 2));
    }
}
