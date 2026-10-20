use std::{collections::BTreeSet, ops::Range};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Style, Stylize},
    text::{Line, Span},
};

use crate::{
    app::AppContext,
    event::UserEvent,
    search::{SearchOptions, SearchOutcome},
    widget::{
        diff::{render_visual_rows, wrapped_rows, Step, WrapMap},
        style::{picked_style, render_scrollbar, scroll_to, ScrollState, VisualSelection},
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
    // where a visual selection started, the selected line having moved from there
    visual: VisualSelection,
    // the lines selected, kept once the visual selection ends until it is cleared
    selection: BTreeSet<usize>,
    // the screen rows the lines wrap to, and the scroll among them
    wrap: WrapMap,
    scroll_state: ScrollState,
}

impl FileText {
    // Other lines, from the top.
    pub fn set_lines(&mut self, lines: Vec<String>) {
        self.lines = lines;
        self.selected = 0;
        self.visual.cancel();
        self.selection.clear();
        self.wrap = WrapMap::default();
        self.scroll_state = ScrollState::default();
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    // Whether the lines are being selected from where the visual selection started.
    pub fn is_visual(&self) -> bool {
        self.visual.is_visual()
    }

    // Starts or ends a visual selection at the selected line. Ending it keeps the lines selected,
    // which `cancel_visual` clears.
    pub fn toggle_visual(&mut self) {
        if self.visual.toggle(self.selected) {
            self.sync_selection();
        }
    }

    // Clears the selected lines; the selected line alone shows afterwards.
    pub fn cancel_visual(&mut self) {
        self.visual.cancel();
        self.selection.clear();
    }

    // Whether lines are selected, while the visual selection or after it ended.
    pub fn has_selection(&self) -> bool {
        !self.selection.is_empty()
    }

    // The visual selection, from where it started to the selected line, keeping the lines selected
    // before it started.
    fn sync_selection(&mut self) {
        if let Some(range) = self.visual.range(self.selected, self.lines.len()) {
            self.selection.extend(range);
        }
    }

    // Selects the first line shown, to move from there.
    pub fn select_shown(&mut self) {
        self.selected = self.logical_at(self.scroll_state.offset);
        self.sync_selection();
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

    // Scrolls by the scroll keys, like vim: `Ctrl-e/y` scroll the window alone, the selected
    // line staying (it may leave the window); `Ctrl-d/u` and `Ctrl-f/b` move it with the window by
    // the same screen rows, so it keeps its place on screen. Returns whether `event` was one.
    pub fn scroll(&mut self, event: UserEvent) -> bool {
        let height = self.scroll_state.height as isize;
        let rows = match event {
            UserEvent::ScrollDown => 1,
            UserEvent::ScrollUp => -1,
            UserEvent::HalfPageDown => height / 2,
            UserEvent::HalfPageUp => -height / 2,
            UserEvent::PageDown => height,
            UserEvent::PageUp => -height,
            _ => return false,
        };
        // ctrl-e and ctrl-y scroll the window alone
        if matches!(event, UserEvent::ScrollDown | UserEvent::ScrollUp) {
            self.scroll_view(rows);
        } else {
            self.scroll_cursor(rows);
        }
        true
    }

    // Scrolls the window `rows` screen rows, the selected line staying put.
    fn scroll_view(&mut self, rows: isize) {
        let last = self.wrap.total().saturating_sub(self.scroll_state.height);
        self.scroll_state.offset = self
            .scroll_state
            .offset
            .saturating_add_signed(rows)
            .min(last);
    }

    // Scrolls the window `rows` screen rows and moves the selected line as many, like vim's
    // ctrl-d/u: the line keeps its screen row while the window scrolls; at the end of the file the
    // window stops and the line keeps going to the last one.
    fn scroll_cursor(&mut self, rows: isize) {
        let last = self.wrap.total().saturating_sub(self.scroll_state.height);
        self.scroll_state.offset = self
            .scroll_state
            .offset
            .saturating_add_signed(rows)
            .min(last);
        let screen = self
            .wrap
            .visual_start(self.selected)
            .saturating_add_signed(rows);
        self.selected = self.logical_at(screen);
        self.sync_selection();
    }

    // Selects a line, the last one past the end.
    pub fn select_line(&mut self, line: usize) {
        self.selected = line.min(self.lines.len().saturating_sub(1));
        self.sync_selection();
        self.scroll_to(self.selected..self.selected + 1);
    }

    // The selected line, from 0.
    pub fn selected_line(&self) -> usize {
        self.selected
    }

    // How the search for `query` went over the lines, the selected one among them.
    pub fn search_outcome(&self, query: &str, options: SearchOptions) -> SearchOutcome {
        let matched = self
            .lines
            .iter()
            .map(|line| match_range(line, query, options).is_some())
            .collect::<Vec<_>>();
        SearchOutcome::of(query, self.selected, matched.into_iter())
    }

    // The selected line as in the file, or the selected lines, to copy.
    pub fn selected_text(&self) -> Option<String> {
        if self.selection.is_empty() {
            return self.lines.get(self.selected).cloned();
        }
        let lines: Vec<&str> = self
            .selection
            .iter()
            .filter_map(|&line| self.lines.get(line))
            .map(String::as_str)
            .collect();
        Some(lines.join("\n"))
    }

    // Selects the line matching `query` from the line `from`; false when nothing matches.
    pub fn search(
        &mut self,
        query: &str,
        from: usize,
        direction: SearchDirection,
        options: SearchOptions,
    ) -> bool {
        self.visual.cancel();
        let lines = &self.lines;
        let found = find_around(lines.len(), from, direction, |i| {
            match_range(&lines[i], query, options).is_some()
        });
        let Some(line) = found else {
            return false;
        };
        self.select_line(line);
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
        let cursor = match cursor {
            LineCursor::Shown => Some(self.selected),
            LineCursor::Hidden => None,
        };
        // the selected lines are picked, like the other lists; the cursor is picked too while
        // selecting, and goes back to its own color once v stops
        let selected_style = |row: usize| {
            let selected = self.selection.contains(&row);
            if selected && (self.visual.is_visual() || cursor != Some(row)) {
                Some(picked_style(theme))
            } else if cursor == Some(row) {
                Some(Style::default().bg(theme.list_selected_bg))
            } else {
                None
            }
        };
        let width = self.lines.len().to_string().len();
        let lines = rows.clone().map(|row| {
            let mut line = Line::from(vec![
                Span::raw(format!("{:>width$} ", row + 1)).fg(theme.muted_fg),
                Span::raw(self.lines[row].replace('\t', "    ")),
            ]);
            if let Some(query) = search {
                highlight_match(&mut line, query, ctx.search_options.get(), theme);
            }
            if let Some(style) = selected_style(row) {
                if style.fg.is_some() {
                    // a search match keeps its own colors, so a selected line still shows where
                    // the match is
                    line.spans = line
                        .spans
                        .into_iter()
                        .map(|span| {
                            if span.style.bg == Some(theme.list_match_bg) {
                                span
                            } else {
                                span.patch_style(style)
                            }
                        })
                        .collect();
                }
                line.style = line.style.patch(style);
            }
            (row, line)
        });
        let visual = wrapped_rows(lines, skip..skip + screen.len(), wrap, area);
        let rows = visual.iter().map(|(row, _)| *row).collect();
        // the background and text color across the row, like the other lists
        for (y, (row, _)) in (area.y..).zip(&visual) {
            if let Some(style) = selected_style(*row) {
                buf.set_style(Rect::new(area.x, y, area.width, 1), style);
            }
        }
        // the bar follows the selected line, which may be outside the selection after it ended
        render_visual_rows(visual, |row| cursor == Some(row), theme, area, buf);
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
            .logical_at(screen)
            .min(self.lines.len().saturating_sub(1))
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

    // Like vim: ctrl-e and ctrl-y scroll the window alone, the selected line staying; ctrl-d and
    // ctrl-u, like ctrl-f and ctrl-b, move it with the window by the same rows.
    #[test]
    fn test_scroll_alone_and_with_the_selected_line() {
        let ctx = test_ctx();
        let area = Rect::new(0, 0, 40, 10);
        let lines: Vec<String> = (1..=40).map(|i| format!("line {i}")).collect();
        let mut text = FileText::default();
        text.set_lines(lines);
        text.render(
            area,
            &mut Buffer::empty(area),
            &ctx,
            true,
            LineCursor::Shown,
            None,
        );
        text.select_line(20);
        assert_eq!(text.selected_line(), 20);
        let offset = text.scroll_state.offset;

        // ctrl-e: the window moves, the line stays
        text.scroll(UserEvent::ScrollDown);
        assert_eq!(text.scroll_state.offset, offset + 1);
        assert_eq!(text.selected_line(), 20);

        // ctrl-d: both move half a page
        text.scroll(UserEvent::HalfPageDown);
        assert_eq!(text.scroll_state.offset, offset + 1 + 5);
        assert_eq!(text.selected_line(), 25);
    }

    // v selects from the line it starts on to the selected one, which the movement keys move.
    #[test]
    fn test_visual_selection_copies_the_lines_between() {
        let mut text = FileText::default();
        text.set_lines(["one", "two", "three", "four"].map(String::from).to_vec());
        assert_eq!(text.selected_text().as_deref(), Some("one"));

        text.toggle_visual();
        text.step(Step::Next);
        text.step(Step::Next);
        assert!(text.is_visual());
        assert_eq!(text.selected_text().as_deref(), Some("one\ntwo\nthree"));

        // ending the visual selection keeps the lines selected
        text.toggle_visual();
        assert!(!text.is_visual());
        assert_eq!(text.selected_text().as_deref(), Some("one\ntwo\nthree"));

        // a new visual selection keeps the lines selected before it
        text.select_line(2);
        text.toggle_visual();
        text.step(Step::Next);
        text.toggle_visual();
        assert_eq!(
            text.selected_text().as_deref(),
            Some("one\ntwo\nthree\nfour")
        );

        // only esc clears the selection
        text.cancel_visual();
        assert!(!text.has_selection());
        assert_eq!(text.selected_text().as_deref(), Some("four"));
    }

    // A file shorter than the panel still moves the selected line, like vim: only the window stays.
    #[test]
    fn test_scroll_moves_the_line_when_the_file_fits() {
        let ctx = test_ctx();
        let area = Rect::new(0, 0, 40, 10);
        let lines: Vec<String> = (1..=4).map(|i| format!("line {i}")).collect();
        let mut text = FileText::default();
        text.set_lines(lines);
        text.render(
            area,
            &mut Buffer::empty(area),
            &ctx,
            true,
            LineCursor::Shown,
            None,
        );
        assert_eq!(text.scroll_state.offset, 0);

        text.scroll(UserEvent::HalfPageDown);
        assert_eq!(text.selected_line(), 3, "the line went to the last one");
        assert_eq!(text.scroll_state.offset, 0, "the window couldn't scroll");
    }

    // Selected lines fill the row with the picked color, the cursor line keeps the selected color,
    // and the bar follows the cursor even after it moved out of the selection.
    #[test]
    fn test_selection_fills_the_row_and_the_bar_follows_the_cursor() {
        let ctx = test_ctx();
        let area = Rect::new(2, 0, 20, 6);
        let mut text = FileText::default();
        text.set_lines(["one", "two", "three", "four"].map(String::from).to_vec());
        text.toggle_visual();
        text.step(Step::Next);
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 6));
        text.render(area, &mut buf, &ctx, true, LineCursor::Shown, None);

        // while selecting, the cursor is picked like the line it extends, its text too
        for x in area.left()..area.right() {
            assert_eq!(buf[(x, 0)].bg, ctx.color_theme.list_picked_bg);
            assert_eq!(buf[(x, 1)].bg, ctx.color_theme.list_picked_bg);
        }
        assert_eq!(buf[(area.x + 4, 0)].fg, ctx.color_theme.list_picked_fg);
        assert_eq!(buf[(area.x - 1, 1)].symbol(), "▌");

        // ending the selection and moving the cursor below it
        text.toggle_visual();
        text.step(Step::Next);
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 6));
        text.render(area, &mut buf, &ctx, true, LineCursor::Shown, None);
        for x in area.left()..area.right() {
            assert_eq!(buf[(x, 0)].bg, ctx.color_theme.list_picked_bg);
            assert_eq!(buf[(x, 1)].bg, ctx.color_theme.list_picked_bg);
            assert_eq!(buf[(x, 2)].bg, ctx.color_theme.list_selected_bg);
        }
        assert_eq!(buf[(area.x - 1, 2)].symbol(), "▌");
    }

    #[test]
    fn test_a_search_match_stays_visible_on_a_picked_line() {
        let ctx = test_ctx();
        let area = Rect::new(0, 0, 20, 3);
        let mut text = FileText::default();
        text.set_lines(vec!["abc".into()]);
        text.toggle_visual();
        let mut buf = Buffer::empty(area);
        text.render(area, &mut buf, &ctx, true, LineCursor::Shown, Some("a"));
        let (x, y) = (area.left()..area.right())
            .zip(std::iter::repeat(area.top()))
            .find(|(x, y)| buf[(*x, *y)].symbol() == "a")
            .expect("the match shown");
        assert_eq!(
            buf[(x, y)].bg,
            ctx.color_theme.list_match_bg,
            "the picked style hid the match"
        );
    }
}
