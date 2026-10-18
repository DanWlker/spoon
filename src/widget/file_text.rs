use std::ops::Range;

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
        diff::{render_visual_rows, wrapped_rows, Step, WrapMap},
        style::{render_scrollbar, scroll_to, ScrollState},
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
// the files tab, and the text of the blame. The scroll counts the screen rows the lines wrap to, so
// long lines are read whole, see `WrapMap`.
#[derive(Debug, Default)]
pub struct FileText {
    lines: Vec<String>,
    selected: usize,
    // the screen rows the lines wrap to, and the scroll among them
    wrap: WrapMap,
    scroll_state: ScrollState,
    // the line the search last matched, where the next one starts from
    search_row: Option<usize>,
}

impl FileText {
    // Other lines, from the top.
    pub fn set_lines(&mut self, lines: Vec<String>) {
        self.lines = lines;
        self.selected = 0;
        self.wrap = WrapMap::default();
        self.scroll_state = ScrollState::default();
        self.search_row = None;
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    // Selects the first line shown, to move from there.
    pub fn select_shown(&mut self) {
        self.selected = self.logical_at(self.scroll_state.offset);
    }

    pub fn step(&mut self, step: Step) {
        let Some(last) = self.lines.len().checked_sub(1) else {
            return;
        };
        let line = match step {
            Step::Next => (self.selected + 1).min(last),
            Step::Prev => self.selected.saturating_sub(1),
            Step::First => 0,
            Step::Last => last,
        };
        self.select_line(line);
    }

    // Scrolls by the scroll keys, moving the selected line along when the edges would leave it
    // out; returns whether `event` was one.
    pub fn scroll(&mut self, event: UserEvent) -> bool {
        let height = self.scroll_state.height as isize;
        let rows = match event {
            UserEvent::ScrollDown => 1,
            UserEvent::ScrollUp => -1,
            UserEvent::PageDown => height,
            UserEvent::PageUp => -height,
            UserEvent::HalfPageDown => height / 2,
            UserEvent::HalfPageUp => -height / 2,
            _ => return false,
        };
        self.scroll_rows(rows);
        true
    }

    // Scrolls the view `rows` screen rows, keeping the selected line within the `scrolloff` rows at
    // the edges, moving it along when it would leave them.
    fn scroll_rows(&mut self, rows: isize) {
        let height = self.scroll_state.height;
        let total = self.wrap.total();
        let offset = self
            .scroll_state
            .offset
            .saturating_add_signed(rows)
            .min(total.saturating_sub(height));
        self.scroll_state.offset = offset;
        let margin = self.margin();
        let (top, bottom) = (
            offset + margin,
            (offset + height).saturating_sub(1 + margin),
        );
        let selected = self.wrap.visual_range(self.selected..self.selected + 1);
        if selected.start < top {
            self.selected = self.logical_at(top);
        } else if selected.end > bottom + 1 {
            self.selected = self.logical_at(bottom);
        }
    }

    // Selects a line, the last one past the end.
    pub fn select_line(&mut self, line: usize) {
        self.selected = line.min(self.lines.len().saturating_sub(1));
        self.search_row = None;
        self.scroll_to(self.selected..self.selected + 1);
    }

    // The selected line, from 0.
    pub fn selected_line(&self) -> usize {
        self.selected
    }

    // The selected line as in the file, to copy.
    pub fn selected_text(&self) -> Option<String> {
        self.lines.get(self.selected).cloned()
    }

    // Selects the line matching `query`, around from the last match; false when nothing matches.
    pub fn search(
        &mut self,
        query: &str,
        direction: SearchDirection,
        options: SearchOptions,
    ) -> bool {
        let lines = &self.lines;
        let start = self.search_row.unwrap_or(self.selected);
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
        let wrap = ctx.wrap.get();
        // the scroll counts screen rows while wrapping, see `WrapMap`; at another width or wrap
        // the lines are measured again, showing the selection again with them
        if !self.wrap.matches(area.width, wrap) {
            self.wrap = WrapMap::from_lines(self.plain_lines(), area.width, wrap);
            self.scroll_to(self.selected..self.selected + 1);
        }
        let scrolloff = ctx.ui_config.common.scrolloff as usize;
        let total = self.wrap.total();
        let screen = self
            .scroll_state
            .fit(total, area.height as usize, scrolloff);
        let border = Rect::new(area.right(), area.y, 1, area.height);
        render_scrollbar(buf, border, total, screen.start, focused, theme);
        // only the lines the screen range overlaps are built
        let rows = self.wrap.logical_rows(screen.clone());
        let skip = screen.start - self.wrap.visual_start(rows.start);
        let selected = match cursor {
            LineCursor::Shown => self.selected..self.selected + 1,
            LineCursor::Hidden => 0..0,
        };
        let highlight_style = Style::default().bg(theme.list_selected_bg);
        let width = self.lines.len().to_string().len();
        let lines = rows.clone().map(|row| {
            let mut line = Line::from(vec![
                Span::raw(format!("{:>width$} ", row + 1)).fg(theme.muted_fg),
                Span::raw(self.lines[row].replace('\t', "    ")),
            ]);
            if let Some(query) = search {
                highlight_match(&mut line, query, ctx.search_options.get(), theme);
            }
            if selected.contains(&row) {
                line.style = line.style.patch(highlight_style);
            }
            (row, line)
        });
        let visual = wrapped_rows(lines, skip..skip + screen.len(), wrap, area);
        let rows = visual.iter().map(|(row, _)| *row).collect();
        render_visual_rows(visual, &selected, theme, area, buf);
        rows
    }

    // The lines of the file, numbered, without the search or the selection: only their width
    // matters for measuring how they wrap.
    fn plain_lines(&self) -> impl Iterator<Item = Line<'static>> + '_ {
        let width = self.lines.len().to_string().len();
        self.lines.iter().enumerate().map(move |(i, text)| {
            Line::from(vec![
                Span::raw(format!("{:>width$} ", i + 1)),
                Span::raw(text.replace('\t', "    ")),
            ])
        })
    }

    // Scrolls as little as can be to show `rows` with `scrolloff` rows around them, see
    // `style::scroll_to`; `rows` are logical, counted as screen rows while wrapping.
    fn scroll_to(&mut self, rows: Range<usize>) {
        let ScrollState {
            offset,
            height,
            scrolloff,
        } = self.scroll_state;
        let rows = self.wrap.visual_range(rows);
        self.scroll_state.offset = scroll_to(offset, rows, height, usize::MAX, scrolloff);
    }

    // The first logical line of screen row `screen`, the last line past the end.
    fn logical_at(&self, screen: usize) -> usize {
        self.wrap
            .logical_rows(screen..screen + 1)
            .start
            .min(self.lines.len().saturating_sub(1))
    }

    fn margin(&self) -> usize {
        self.scroll_state
            .scrolloff
            .min(self.scroll_state.height.saturating_sub(1) / 2)
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
        text.step(Step::Last);
        assert_eq!(text.selected_line(), 39);
        assert!(text.scroll(UserEvent::PageUp));
        assert_eq!(text.selected_line(), 29);
    }

    // A line far wider than the panel wraps, and the scroll keys reach all of it: the file
    // preview used to stop at what fit the panel.
    #[test]
    fn test_scroll_reaches_the_wrapped_rows_of_a_long_line() {
        let ctx = test_ctx();
        ctx.wrap.set(true);
        let area = Rect::new(0, 0, 20, 5);
        let mut text = FileText::default();
        text.set_lines(vec!["x".repeat(200)]);
        let rows = text.render(
            area,
            &mut Buffer::empty(area),
            &ctx,
            true,
            LineCursor::Shown,
            None,
        );
        // the one line wraps past the panel
        assert!(text.wrap.total() > area.height as usize);
        assert_eq!(rows.iter().max(), Some(&0));

        // scrolling reaches the end of the wrapped line
        let last = text.wrap.total().saturating_sub(area.height as usize);
        while text.scroll_state.offset < last {
            text.scroll(UserEvent::PageDown);
        }
        assert_eq!(text.scroll_state.offset, last);
    }
}
