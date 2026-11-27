use std::{
    ops::{Range, RangeInclusive},
    sync::Arc,
};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Style, Stylize},
    text::{Line, Span},
};

use crate::{
    background::Cancel,
    context::AppContext,
    event::Sender,
    keybind::UserEvent,
    layout::{RowMap, Target},
    pointer::OnPicks,
    search::{SearchOptions, SearchOutcome},
    widget::{
        diff::{
            render_visual_rows, rewrap, wrapped_rows, DiffLayout, Measuring, NearScreen, Step,
            WrapMap, ALL_ROWS,
        },
        picks::Picks,
        style::{
            page_rows, picked_style, render_scrollbar, scroll_with_selection, selection_rows,
            tabs_shown, ScrollState, VisualSelection,
        },
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
    // shared with the thread measuring them wrapped, see `rewrap`
    lines: Arc<[String]>,
    selected: usize,
    // where a visual selection started, the selected line having moved from there
    visual: VisualSelection,
    // the lines selected, kept once the visual selection ends until it is cleared; like the rows
    // of a list, see `Picks`
    selection: Picks<usize>,
    // the screen rows the lines wrap to, and the scroll among them
    wrap: WrapMap,
    scroll_state: ScrollState,
    // the app's, to measure many lines wrapped on another thread, see `measure_in_background`;
    // without it, as in the tests, right away
    tx: Option<Sender>,
    // the lines wrapped measuring on another thread, see `rewrap`
    measuring: Option<Measuring>,
}

impl FileText {
    // Measures many lines wrapped on another thread, waking the app up with `tx` once done, see
    // `rewrap`.
    pub fn measure_in_background(&mut self, tx: &Sender) {
        if self.tx.is_none() {
            self.tx = Some(tx.clone());
        }
    }

    // Other lines, from the top.
    pub fn set_lines(&mut self, lines: Vec<String>) {
        self.lines = lines.into();
        self.measuring = None;
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
        match self.visual.toggle(self.selected) {
            true => {
                self.selection.start_visual();
                self.sync_selection();
            }
            false => self.selection.finish_visual(),
        }
    }

    // Clears the selected lines; the selected line alone shows afterwards.
    pub fn cancel_visual(&mut self) {
        self.visual.cancel();
        self.selection.clear();
    }

    // What is selected, for the keys and messages: "line" or "lines", like `DiffPane`'s.
    pub fn selected_name(&self) -> &'static str {
        match self.has_selection() {
            true => "lines",
            false => "line",
        }
    }

    // Whether lines are selected, while the visual selection or after it ended.
    pub fn has_selection(&self) -> bool {
        !self.selection.is_empty()
    }

    // The visual selection, from where it started to the selected line, keeping the lines selected
    // before it started.
    fn sync_selection(&mut self) {
        if let Some(range) = self.visual.range(self.selected, self.lines.len()) {
            self.selection.sync_visual(&range.collect::<Vec<_>>());
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
        let rows = match event {
            UserEvent::ScrollDown => 1,
            UserEvent::ScrollUp => -1,
            // the wheel scrolls the window alone, the selected line staying even off it
            UserEvent::Wheel(rows) => {
                let last = self
                    .wrap
                    .total()
                    .saturating_sub(self.scroll_state.height.max(1));
                let offset = self.scroll_state.offset.saturating_add_signed(rows);
                self.scroll_state.offset = offset.min(last);
                return true;
            }
            _ => match page_rows(event, self.scroll_state.height) {
                Some(rows) => {
                    self.scroll_cursor(rows);
                    return true;
                }
                None => return false,
            },
        };
        // ctrl-e and ctrl-y scroll the window alone
        self.scroll_view(rows);
        true
    }

    // Scrolls the window `rows` screen rows, the selected line staying put until it would leave
    // the `scrolloff` rows at the edge, then moving along, like vim's ctrl-e/y.
    fn scroll_view(&mut self, rows: isize) {
        let ScrollState {
            offset,
            height,
            scrolloff,
        } = self.scroll_state;
        let total = self.wrap.total();
        let offset = offset
            .saturating_add_signed(rows)
            .min(total.saturating_sub(height.max(1)));
        self.scroll_state.offset = offset;
        // the screen rows the line may be on, as in a list, see `selection_rows`
        let allowed = selection_rows(offset, height, total, scrolloff);
        let (top, bottom) = (allowed.start, allowed.end - 1);
        let screen = self.wrap.visual_start(self.selected);
        let moved = if screen < top {
            // the first line starting at or below the top row
            let line = self.logical_at(top);
            match self.wrap.visual_start(line) < top {
                true => (line + 1).min(self.lines.len().saturating_sub(1)),
                false => line,
            }
        } else if screen > bottom {
            self.logical_at(bottom)
        } else {
            return;
        };
        self.selected = moved;
        self.sync_selection();
    }

    // Scrolls the window `rows` screen rows and moves the selected line as many, as a list does,
    // see `scroll_with_selection`.
    fn scroll_cursor(&mut self, rows: isize) {
        let ScrollState {
            offset,
            height,
            scrolloff,
        } = self.scroll_state;
        let selected = self.wrap.visual_start(self.selected);
        let total = self.wrap.total();
        let (offset, screen) =
            scroll_with_selection(offset, selected, rows, height, total, scrolloff);
        self.scroll_state.offset = offset;
        self.selected = self.logical_at(screen);
        self.sync_selection();
    }

    // Before the mouse selects `line`, see `OnPicks`.
    pub fn prepare_point(&mut self, line: Option<usize>, picks: OnPicks) {
        let picked = line.is_some_and(|line| self.selection.contains(&line));
        if picks == OnPicks::ClearUnlessPicked && !picked {
            self.cancel_visual();
        }
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

    // The lines selected, from the first to the last, else the selected line, from 0: what a link
    // to the file points at.
    pub fn selected_lines(&self) -> RangeInclusive<usize> {
        match (self.selection.iter().min(), self.selection.iter().max()) {
            (Some(&first), Some(&last)) => first..=last,
            _ => self.selected..=self.selected,
        }
    }

    // How the search for `query` went over the lines, the selected one among them.
    pub fn search_outcome(&self, query: &str, options: SearchOptions) -> SearchOutcome {
        let matched = self
            .lines
            .iter()
            .map(|line| match_range(line, query, options).is_some());
        SearchOutcome::of(query, self.selected, matched)
    }

    // The selected line as in the file, or the selected lines, to copy.
    pub fn selected_text(&self) -> Option<String> {
        if self.selection.is_empty() {
            return self.lines.get(self.selected).cloned();
        }
        let mut picked: Vec<usize> = self.selection.iter().copied().collect();
        picked.sort_unstable();
        let lines: Vec<&str> = picked
            .into_iter()
            .filter_map(|line| self.lines.get(line))
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
        // the lines picked stay
        self.visual.cancel();
        self.selection.end_visual();
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
    #[expect(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        area: Rect,
        target: Target,
        buf: &mut Buffer,
        ctx: &AppContext,
        focused: bool,
        cursor: LineCursor,
        search: Option<&str>,
    ) -> Vec<usize> {
        let theme = &ctx.color_theme;
        // the scroll counts screen rows while wrapping, see `WrapMap`; at another width or wrap
        // the lines are measured again, the line at the top staying there and the selection shown
        // again with them; those of a big file off the screen on another thread, drawn as they
        // are meanwhile, see `rewrap`
        let (width, rows) = (area.width, self.lines.len());
        let work = || {
            let lines = self.lines.clone();
            move |cancel: &Cancel| {
                // stopped once measuring for another width or file, see `rewrap`
                let measured =
                    |range| plain_lines(&lines, range).take_while(|_| !cancel.is_cancelled());
                WrapMap::from_lines(lines.len(), ALL_ROWS, measured, width, true)
            }
        };
        let top = self.wrap.logical_at(self.scroll_state.offset);
        let map = rewrap(
            &self.wrap,
            &mut self.measuring,
            self.tx.as_ref(),
            (width, ctx.wrap.get(), DiffLayout::Unified),
            NearScreen {
                top,
                height: area.height,
            },
            |near| {
                let lines = |range| plain_lines(&self.lines, range);
                WrapMap::from_lines(rows, near, lines, width, ctx.wrap.get())
            },
            work,
        );
        if let Some(map) = map {
            self.wrap = map;
            self.scroll_state.offset = self.wrap.visual_start(top);
            self.scroll_to(self.selected..self.selected + 1);
        }
        let scrolloff = ctx.ui_config.common.scrolloff as usize;
        let total = self.wrap.total();
        let screen = self
            .scroll_state
            .fit(total, area.height as usize, scrolloff);
        let border = Rect::new(area.right(), area.y, 1, area.height);
        let thumb = render_scrollbar(buf, border, total, screen.start, focused, theme);
        ctx.layout.borrow_mut().record_thumb(target, thumb);
        let lines = (screen.start..screen.start + area.height as usize)
            .map(|row| (row < total).then(|| self.wrap.logical_at(row)))
            .collect();
        ctx.layout
            .borrow_mut()
            .record(target, area, RowMap::Lines(lines));
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
        // as they were measured, the numbers muted
        let lines = plain_lines(&self.lines, rows.clone())
            .zip(rows)
            .map(|(mut line, row)| {
                let number = std::mem::take(&mut line.spans[0]);
                line.spans[0] = number.fg(theme.muted_fg);
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
        let visual = wrapped_rows(
            lines,
            skip..skip + screen.len(),
            |row| self.wrap.wraps_row(row),
            area,
        );
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

    // Scrolls as little as can be to show `rows` with `scrolloff` rows around them, see
    // `style::scroll_to`; `rows` are logical, counted as screen rows while wrapping.
    fn scroll_to(&mut self, rows: Range<usize>) {
        self.scroll_state.show_rows(self.wrap.visual_range(rows));
    }

    // The first logical line of screen row `screen`, the last line past the end.
    fn logical_at(&self, screen: usize) -> usize {
        self.wrap
            .logical_at(screen)
            .min(self.lines.len().saturating_sub(1))
    }
}

// The lines of a file, numbered, without the search or the selection: only their width matters
// for measuring how they wrap.
fn plain_lines(lines: &[String], range: Range<usize>) -> impl Iterator<Item = Line<'static>> + '_ {
    let width = lines.len().to_string().len();
    let start = range.start;
    lines[range].iter().zip(start..).map(move |(text, i)| {
        Line::from(vec![
            Span::raw(format!("{:>width$} ", i + 1)),
            Span::raw(tabs_shown(text).into_owned()),
        ])
    })
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;

    use super::*;
    use crate::{test_support::ui::test_ctx, widget::style::WHEEL_ROWS};

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
            Target::DetailPane,
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
            Target::DetailPane,
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
    fn test_wheel_leaves_the_selected_line() {
        let ctx = test_ctx();
        let area = Rect::new(0, 0, 40, 10);
        let lines: Vec<String> = (1..=40).map(|i| format!("line {i}")).collect();
        let mut text = FileText::default();
        text.set_lines(lines);
        let draw = |text: &mut FileText| {
            text.render(
                area,
                Target::DetailPane,
                &mut Buffer::empty(area),
                &ctx,
                true,
                LineCursor::Shown,
                None,
            );
        };
        draw(&mut text);
        for _ in 0..10 {
            assert!(text.scroll(UserEvent::Wheel(WHEEL_ROWS as isize)));
        }
        draw(&mut text);
        assert_eq!(text.selected_line(), 0);
        assert_eq!(text.scroll_state.offset, 10 * WHEEL_ROWS);
        text.scroll(UserEvent::Wheel(-(WHEEL_ROWS as isize)));
        assert_eq!(text.scroll_state.offset, 9 * WHEEL_ROWS);
    }

    #[test]
    fn test_scroll_alone_and_with_the_selected_line() {
        let ctx = test_ctx();
        let area = Rect::new(0, 0, 40, 10);
        let lines: Vec<String> = (1..=40).map(|i| format!("line {i}")).collect();
        let mut text = FileText::default();
        text.set_lines(lines);
        text.render(
            area,
            Target::DetailPane,
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
        // until it would leave the window, then it moves along, like vim's
        for _ in 0..20 {
            text.scroll(UserEvent::ScrollDown);
        }
        let top = text.scroll_state.offset;
        assert_eq!(text.selected_line(), top);
        // and the other way, at the bottom
        for _ in 0..20 {
            text.scroll(UserEvent::ScrollUp);
        }
        let bottom = text.scroll_state.offset + text.scroll_state.height - 1;
        assert_eq!(text.selected_line(), bottom);
        text.select_line(20);

        // ctrl-d: both move half a page
        let (offset, line) = (text.scroll_state.offset, text.selected_line());
        text.scroll(UserEvent::HalfPageDown);
        assert_eq!(text.scroll_state.offset, offset + 5);
        assert_eq!(text.selected_line(), line + 5);
    }

    // ctrl-d from the top keeps the selected line out of the `scrolloff` rows at the top, as a
    // list does: it used to land on the top row.
    #[test]
    fn test_half_page_keeps_the_scrolloff() {
        let mut ui_config = crate::config::UiConfig::default();
        ui_config.common.scrolloff = 2;
        let ctx =
            crate::test_support::ui::test_ctx_of(crate::color::ColorTheme::default(), ui_config);
        let area = Rect::new(0, 0, 40, 10);
        let mut text = FileText::default();
        text.set_lines((1..=40).map(|i| format!("line {i}")).collect());
        let mut buf = Buffer::empty(area);
        text.render(
            area,
            Target::DetailPane,
            &mut buf,
            &ctx,
            true,
            LineCursor::Shown,
            None,
        );
        text.scroll(UserEvent::HalfPageDown);
        assert_eq!((text.scroll_state.offset, text.selected_line()), (5, 7));
    }

    // Lines are picked like the rows of a list (`Picks`): moving back unpicks what the visual
    // selection went over, and v v on a picked line unpicks it. Lines only used to add up.
    #[test]
    fn test_lines_picked_like_the_rows_of_a_list() {
        let mut text = FileText::default();
        text.set_lines(["a", "b", "c", "d"].map(String::from).to_vec());
        text.toggle_visual();
        text.step(Step::Next);
        text.step(Step::Next);
        text.step(Step::Prev);
        assert_eq!(text.selected_text().as_deref(), Some("a\nb"));
        // ended, then v v on the picked b
        text.toggle_visual();
        text.toggle_visual();
        text.toggle_visual();
        assert_eq!(text.selected_text().as_deref(), Some("a"));
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

    // ctrl-e and ctrl-y leave the selected line where nothing scrolls, as a list and a diff do: at
    // the top it moved past the scrolloff, at the bottom back from it.
    #[test]
    fn test_scroll_line_at_an_edge_leaves_the_selection() {
        let ctx = test_ctx();
        let area = Rect::new(0, 0, 40, 10);
        let lines: Vec<String> = (1..=40).map(|i| format!("line {i}")).collect();
        let mut text = FileText::default();
        text.set_lines(lines);
        let draw = |text: &mut FileText| {
            text.render(
                area,
                Target::DetailPane,
                &mut Buffer::empty(area),
                &ctx,
                true,
                LineCursor::Shown,
                None,
            )
        };
        draw(&mut text);
        text.scroll_state.scrolloff = 3;
        assert!(text.scroll(UserEvent::ScrollUp));
        assert_eq!(text.selected_line(), 0);
        text.step(Step::Last);
        draw(&mut text);
        text.scroll_state.scrolloff = 3;
        assert!(text.scroll(UserEvent::ScrollDown));
        assert_eq!(text.selected_line(), 39);
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
            Target::DetailPane,
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
        text.render(
            area,
            Target::DetailPane,
            &mut buf,
            &ctx,
            true,
            LineCursor::Shown,
            None,
        );

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
        text.render(
            area,
            Target::DetailPane,
            &mut buf,
            &ctx,
            true,
            LineCursor::Shown,
            None,
        );
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
        text.render(
            area,
            Target::DetailPane,
            &mut buf,
            &ctx,
            true,
            LineCursor::Shown,
            Some("a"),
        );
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
    // Many lines are wrapped on another thread, as measuring them takes a while: those on the
    // screen right away, the others drawn unwrapped meanwhile, all wrapped once measured.
    #[test]
    fn test_many_lines_wrap_in_the_background() {
        let ctx = test_ctx();
        ctx.wrap.set(true);
        let area = Rect::new(0, 0, 20, 5);
        let draw = |text: &mut FileText| {
            let mut buf = Buffer::empty(area);
            text.render(
                area,
                Target::DetailPane,
                &mut buf,
                &ctx,
                true,
                LineCursor::Shown,
                None,
            );
        };
        let ec = crate::event::EventController::without_terminal();
        let mut text = FileText::default();
        text.measure_in_background(&ec.sender());
        text.set_lines(vec!["x".repeat(40); 30_000]);

        draw(&mut text);
        assert!(text.measuring.is_some());
        // those on the screen right away
        assert!(text.wrap.wraps_row(0) && !text.wrap.is_measured());
        let measured = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !text.wrap.is_measured() {
            assert!(std::time::Instant::now() < measured, "never wrapped");
            std::thread::sleep(std::time::Duration::from_millis(5));
            draw(&mut text);
        }
        assert!(text.wrap.total() > 30_000);
    }
}
