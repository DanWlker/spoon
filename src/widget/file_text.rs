use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Style, Stylize},
    text::{Line, Span},
};

use crate::{
    app::AppContext,
    event::UserEvent,
    search::SearchOptions,
    widget::{
        diff::{render_visual_rows, visual_rows, Step},
        style::{render_scrollbar, HalfPage, ListCursor},
        tree_query::{find_around, highlight_match, match_range, SearchDirection},
    },
};

// Whether the selected line of a text shows, while the selection moves in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineCursor {
    Shown,
    Hidden,
}

// The lines of a file at a commit, with a selected line moving in them and a search: the preview of
// the files tab, and the text of the blame.
#[derive(Debug, Default)]
pub struct FileText {
    lines: Vec<String>,
    cursor: ListCursor,
    // the line the search last matched, where the next one starts from
    search_row: Option<usize>,
}

impl FileText {
    // Other lines, from the top.
    pub fn set_lines(&mut self, lines: Vec<String>) {
        self.lines = lines;
        self.cursor = ListCursor::default();
        self.search_row = None;
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    // Selects the first line shown, to move from there.
    pub fn select_shown(&mut self) {
        let offset = self.cursor.offset();
        self.cursor.select_at(offset, 0, self.lines.len());
    }

    pub fn step(&mut self, step: Step) {
        let Some(last) = self.lines.len().checked_sub(1) else {
            return;
        };
        let line = match step {
            Step::Next => self.cursor.selected() + 1,
            Step::Prev => self.cursor.selected().saturating_sub(1),
            Step::First => 0,
            Step::Last => last,
        };
        self.select_line(line);
    }

    // Scrolls by the scroll keys, or moves the selected line half a page; returns whether `event`
    // was one.
    pub fn scroll(&mut self, event: UserEvent) -> bool {
        let total = self.lines.len();
        if let Some(line) = self.cursor.moved_to(event, total, HalfPage::List) {
            self.cursor.select(line, total);
            return true;
        }
        match event {
            UserEvent::ScrollDown => self.cursor.scroll_lines(1, total),
            UserEvent::ScrollUp => self.cursor.scroll_lines(-1, total),
            UserEvent::PageDown => self.cursor.scroll_pages(1, total),
            UserEvent::PageUp => self.cursor.scroll_pages(-1, total),
            _ => return false,
        }
        true
    }

    // Selects a line, the last one past the end.
    pub fn select_line(&mut self, line: usize) {
        self.cursor.select(line, self.lines.len());
        self.search_row = None;
    }

    // The selected line, from 0.
    pub fn selected_line(&self) -> usize {
        self.cursor.selected()
    }

    // The selected line as in the file, to copy.
    pub fn selected_text(&self) -> Option<String> {
        self.lines.get(self.cursor.selected()).cloned()
    }

    // Selects the line matching `query`, around from the last match; false when nothing matches.
    pub fn search(
        &mut self,
        query: &str,
        direction: SearchDirection,
        options: SearchOptions,
    ) -> bool {
        let lines = &self.lines;
        let start = self.search_row.unwrap_or(self.cursor.selected());
        let found = find_around(lines.len(), start, direction, |i| {
            match_range(&lines[i], query, options).is_some()
        });
        let Some(line) = found else {
            return false;
        };
        self.select_line(line);
        self.search_row = Some(line);
        true
    }

    // Draws the lines with their numbers, the selected one highlighted with `cursor`, and the
    // scrollbar on the column right of `area`; returns the line shown on each row of `area`, which
    // differ from the rows when long lines are wrapped.
    pub fn render(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        ctx: &AppContext,
        focused: bool,
        cursor: LineCursor,
        search: Option<&str>,
    ) -> Vec<usize> {
        let theme = &ctx.color_theme;
        let lines = &self.lines;
        // only the visible lines are built, files can be long
        let width = lines.len().to_string().len();
        let scrolloff = ctx.ui_config.common.scrolloff as usize;
        let offset = self
            .cursor
            .fit(area.height as usize, lines.len(), scrolloff);
        let border = Rect::new(area.right(), area.y, 1, area.height);
        render_scrollbar(buf, border, lines.len(), offset, focused, theme);
        let highlight_style = Style::default().bg(theme.list_selected_bg);
        let selected = match cursor {
            LineCursor::Shown => self.cursor.selected()..self.cursor.selected() + 1,
            LineCursor::Hidden => 0..0,
        };
        let visible = lines[offset.min(lines.len())..]
            .iter()
            .zip(offset..)
            .map(|(line, i)| {
                let mut line = Line::from(vec![
                    Span::raw(format!("{:>width$} ", i + 1)).fg(theme.muted_fg),
                    Span::raw(line.replace('\t', "    ")),
                ]);
                if let Some(query) = search {
                    highlight_match(&mut line, query, ctx.search_options.get(), theme);
                }
                if selected.contains(&i) {
                    line.style = line.style.patch(highlight_style);
                }
                (i, line)
            });
        let visual = visual_rows(visible, ctx.wrap.get(), &selected, area);
        let rows = visual.iter().map(|(row, _)| *row).collect();
        render_visual_rows(visual, &selected, theme, area, buf);
        rows
    }
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;

    use super::*;
    use crate::test_support::ui::test_ctx;

    // The scroll keys move the selected line of a file's text like the other lists: the page keys
    // by a page, ctrl-d and ctrl-u by half of one, g and G to the ends.
    #[test]
    fn test_scroll_moves_the_selection_like_the_other_lists() {
        let ctx = test_ctx();
        let area = Rect::new(0, 0, 40, 10);
        let lines: Vec<String> = (1..=40).map(|i| format!("line {i}")).collect();
        let mut text = FileText::default();
        text.set_lines(lines);
        // draw once, to give the scroll the height it moves by
        text.render(
            area,
            &mut Buffer::empty(area),
            &ctx,
            true,
            LineCursor::Shown,
            None,
        );

        assert_eq!(text.selected_line(), 0);
        assert!(text.scroll(UserEvent::PageDown));
        assert_eq!(text.selected_line(), 10);
        assert!(text.scroll(UserEvent::HalfPageDown));
        assert_eq!(text.selected_line(), 15);
        assert!(text.scroll(UserEvent::GoToBottom));
        assert_eq!(text.selected_line(), 39);
        assert!(text.scroll(UserEvent::PageUp));
        assert_eq!(text.selected_line(), 29);
    }
}
