// The keys of the focused panel in a box at the bottom right, after helix's infobox and the
// helix preset of which-key: a key and what it does on each row.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Stylize,
    text::{Line, Span},
    widgets::{Clear, Paragraph, Widget},
};

use unicode_width::UnicodeWidthStr;

use crate::{
    color::ColorTheme,
    widget::style::{key_span, panel_block, panel_title},
};

pub struct KeysPopup<'a> {
    // the keys, like "j/k", and what they do
    keys: &'a [(String, String)],
    theme: &'a ColorTheme,
}

impl<'a> KeysPopup<'a> {
    pub fn new(keys: &'a [(String, String)], theme: &'a ColorTheme) -> Self {
        KeysPopup { keys, theme }
    }

    // Its width and height, as big as the keys (with some room for short ones); see `area`.
    pub fn size(&self) -> (u16, u16) {
        let key_width = self.key_width();
        let row_width = self
            .keys
            .iter()
            .map(|(_, description)| key_width + SEPARATOR.width() + description.width())
            .max()
            .unwrap_or(0)
            .max(TITLE.width() + 2);
        // the borders and a space on each side
        let width = (row_width as u16 + 4).max(MIN_WIDTH);
        let height = (self.keys.len() as u16 + 2).min(MAX_HEIGHT);
        (width, height)
    }

    // Where it goes in `area`: in the bottom right corner, the rows that don't fit left out.
    pub fn area(&self, area: Rect) -> Rect {
        let (width, height) = self.size();
        let (width, height) = (width.min(area.width), height.min(area.height));
        Rect::new(area.right() - width, area.bottom() - height, width, height)
    }

    fn key_width(&self) -> usize {
        self.keys
            .iter()
            .map(|(key, _)| key.width())
            .max()
            .unwrap_or(0)
    }
}

const TITLE: &str = "Keys";
const MIN_WIDTH: u16 = 30;
const MAX_HEIGHT: u16 = 25;
const SEPARATOR: &str = " ➜ ";

impl Widget for KeysPopup<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = self.area(area);
        if area.width < 3 || area.height < 3 {
            return;
        }
        let theme = self.theme;
        let key_width = self.key_width();
        let lines: Vec<Line> = self
            .keys
            .iter()
            .map(|(key, description)| {
                Line::from(vec![
                    key_span(format!(" {key:>key_width$}"), theme),
                    Span::raw(SEPARATOR).fg(theme.divider_fg),
                    Span::raw(description.clone()).fg(theme.fg),
                ])
            })
            .collect();
        let block = panel_block(true, theme).title(panel_title(TITLE, "", true, theme));
        Clear.render(area, buf);
        Paragraph::new(lines)
            .block(block)
            .bg(theme.bg)
            .render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ui::{
        assert_fills_bg, assert_key_style, assert_popup_frame, column_of, find_row, popup_rect,
        theme_with_bg,
    };

    #[test]
    fn test_looks_like_the_other_popups() {
        let theme = theme_with_bg();
        let keys = vec![("j/k".to_string(), "move".to_string())];
        let area = Rect::new(0, 0, 50, 10);
        let mut buf = Buffer::empty(area);
        KeysPopup::new(&keys, &theme).render(area, &mut buf);
        let rect = popup_rect(&buf);
        assert_popup_frame(&buf, rect, theme.accent_fg);
        assert_fills_bg(&buf, rect, &theme, &[]);
        let y = find_row(&buf, "j/k");
        assert_key_style(&buf, column_of(&buf, y, "j/k"), y, &theme);
    }

    #[test]
    fn test_keys_popup_area() {
        let theme = ColorTheme::default();
        let keys = vec![
            ("d".to_string(), "diff".to_string()),
            (
                "ctrl-x".to_string(),
                "a long description of the key".to_string(),
            ),
        ];
        let popup = KeysPopup::new(&keys, &theme);
        // in the bottom right corner, the rows with the borders
        let area = popup.area(Rect::new(0, 0, 100, 40));
        assert_eq!(area, Rect::new(100 - 42, 40 - 4, 42, 4));
        // never out of a small area
        let area = popup.area(Rect::new(5, 5, 20, 3));
        assert_eq!(area, Rect::new(5, 5, 20, 3));
    }
}
