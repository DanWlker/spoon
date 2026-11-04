use std::{ops::RangeInclusive, time::Instant};

use ratatui::{buffer::Buffer, layout::Rect, text::Line};

use crate::{
    app::AppContext,
    background::Job,
    event::{Sender, UserEvent},
    git::{PatchDirection, Repository},
    search::{SearchOptions, SearchOutcome},
    widget::{
        diff::{
            diff_lines, parse_diff, render_diff_lines, row_matches, DiffCursor, DiffHunk, DiffPart,
            ParsedDiff, Step, WrapMap,
        },
        style::{render_panel, render_scrollbar, ScrollState},
        tree_query::{highlight_match, SearchDirection},
    },
};

// The selection of a diff, kept across its reload, see `DiffPane::restore`.
#[derive(Debug, Clone, Copy)]
pub struct DiffSelection {
    focus: Option<DiffPart>,
    hunk: usize,
    line: usize,
}

// A diff beside what picks it: moving into it selects hunks, then lines, to copy, link to, apply or
// stage; the changes tab, the timeline and the local changes.
#[derive(Debug, Default)]
pub struct DiffPane {
    // loaded lazily, None until then
    diff: Option<ParsedDiff>,
    // the diff loading in the background, shown once loaded, see `load_in_background`
    loading: Option<Job<String>>,
    // what to select in the diff loading once it is shown, see `restore` and `enter`
    pending: Option<DiffSelection>,
    // the diff shown before the one loading, drawn meanwhile; no key acts on it
    previous: Option<ParsedDiff>,
    // the hunks or the lines while the diff has the focus, None while what is beside it does (the
    // files of the changes tab, the commits of the timeline)
    focus: Option<DiffPart>,
    cursor: DiffCursor,
}

impl DiffPane {
    // Shows another diff, from the top, the focus beside it.
    pub fn set_diff(&mut self, diff: Option<ParsedDiff>) {
        self.loading = None;
        self.previous = None;
        self.diff = diff;
        self.focus = None;
        self.cursor.reset();
    }

    // The diff is no longer the one selected: drawn until the next one is shown, where it was
    // scrolled, the focus beside it so no key acts on it.
    pub fn outdate(&mut self) {
        self.previous = self.diff.take().or(self.previous.take());
        self.focus = None;
    }

    pub fn diff(&self) -> Option<&ParsedDiff> {
        self.diff.as_ref()
    }

    // Loads another diff with `work`, the git command giving it, on another thread, the focus
    // beside it; the one before is only drawn meanwhile, where it was scrolled, so no key acts on
    // it. See `poll_loading`.
    pub fn load_in_background(
        &mut self,
        tx: Sender,
        work: impl FnOnce() -> String + Send + 'static,
    ) {
        self.outdate();
        self.pending = None;
        self.loading = Some(Job::spawn(tx, work));
    }

    // Shows the diff loading in the background once it is loaded; returns whether it was.
    pub fn poll_loading(&mut self, repository: &Repository) -> bool {
        let Some(raw) = self.loading.as_ref().and_then(Job::try_take) else {
            return false;
        };
        self.show_loaded(&raw, repository);
        true
    }

    // Shows the diff loading in the background, waiting for it, before a key acts on it.
    pub fn wait_loading(&mut self, repository: &Repository) {
        if let Some(job) = self.loading.take() {
            self.show_loaded(&job.wait(), repository);
        }
    }

    // Shows the diff `raw`, the output of the git command giving it.
    pub fn show_loaded(&mut self, raw: &str, repository: &Repository) {
        let mut diff = parse_diff(raw);
        diff.drop_converted(repository);
        self.set_diff(Some(diff));
        if let Some(selection) = self.pending.take() {
            self.restore(selection);
        }
    }

    // When its diff started loading in the background, None when it isn't.
    pub fn loading_since(&self) -> Option<Instant> {
        self.loading.as_ref().map(Job::started)
    }

    // Whether the diff is of one file, which shows more or less context around its changes.
    // Of the diff drawn while another loads too, the keys being those of the diff reloading.
    pub fn is_single_file(&self) -> bool {
        self.diff
            .as_ref()
            .or(self.previous.as_ref())
            .is_some_and(ParsedDiff::is_single_file)
    }

    // What is selected, to select it again in the diff reloaded, see `restore`.
    pub fn selection(&self) -> DiffSelection {
        DiffSelection {
            focus: self.focus,
            hunk: self.cursor.hunk,
            line: self.cursor.line,
        }
    }

    // Selects again in the diff reloaded after staging part of it what `selection` was: the same
    // hunk, then the changed line at or after the same row, as the staged line itself is gone or
    // became context. Stays out of the diff when it has no hunk left.
    // While the diff loads in the background, once it is shown.
    pub fn restore(&mut self, selection: DiffSelection) {
        if self.loading.is_some() {
            self.pending = Some(selection);
            return;
        }
        let Some(diff) = &self.diff else {
            return;
        };
        if selection.focus.is_none() || !self.cursor.select_hunk(diff, selection.hunk) {
            return;
        }
        self.focus = Some(DiffPart::Hunk);
        if selection.focus == Some(DiffPart::Line)
            && self.cursor.select_line_near(diff, selection.line)
        {
            self.focus = Some(DiffPart::Line);
        }
    }

    // Moves the focus out of the diff, to what is beside it.
    pub fn leave(&mut self) {
        self.focus = None;
    }

    // The scroll of what is shown in place of the diff, like the sides of a conflict.
    pub fn scroll_state(&mut self) -> &mut ScrollState {
        &mut self.cursor.scroll
    }

    // Scrolls the diff by the scroll keys, or moves the selected hunk or line half a page or a
    // page; returns whether `event` was one.
    pub fn scroll(&mut self, event: UserEvent) -> bool {
        let jumped = match (&self.diff, self.focus) {
            (Some(diff), Some(part)) => self.cursor.jump_page(diff, event, part),
            _ => false,
        };
        jumped || self.cursor.scroll.handle_scroll(event)
    }

    // The part with the focus, or that will have it once the diff loading is shown (see
    // `restore`): the keys of the diff are taken meanwhile, each waiting for it before it acts.
    fn focused_part(&self) -> Option<DiffPart> {
        self.focus.or(self.pending.and_then(|p| p.focus))
    }

    pub fn is_focused(&self) -> bool {
        self.focused_part().is_some()
    }

    pub fn is_selecting_lines(&self) -> bool {
        self.focused_part() == Some(DiffPart::Line)
    }

    // What `selected_text` is of: the hunk, until lines are selected.
    pub fn selected_part(&self) -> DiffPart {
        self.focused_part().unwrap_or(DiffPart::Hunk)
    }

    // The lines of the selected hunk or line in the file at the commit; None outside the diff, or
    // for lines only removed.
    pub fn selected_lines(&self) -> Option<RangeInclusive<usize>> {
        let diff = self.diff.as_ref().filter(|_| self.is_focused())?;
        let rows = self.cursor.highlighted(diff, self.selected_part());
        let mut lines = diff
            .rows
            .get(rows)?
            .iter()
            .filter_map(|row| Some(row.new? as usize));
        let first = lines.next()?;
        Some(first..=lines.next_back().unwrap_or(first))
    }

    // Goes one level deeper, like on the local changes page: into the hunks, then the lines of the
    // selected hunk. Returns false when the diff has no hunk.
    // While the diff loads in the background, its first hunk once it is shown.
    pub fn enter(&mut self) -> bool {
        if self.loading.is_some() {
            let first = DiffSelection {
                focus: Some(DiffPart::Hunk),
                hunk: 0,
                line: 0,
            };
            self.pending = Some(first);
            return true;
        }
        let Some(diff) = &self.diff else {
            return false;
        };
        match self.focus {
            None if self.cursor.select_hunk(diff, 0) => self.focus = Some(DiffPart::Hunk),
            Some(DiffPart::Hunk) if self.cursor.enter_lines(diff) => {
                self.focus = Some(DiffPart::Line)
            }
            None => return false,
            _ => {}
        }
        true
    }

    // Goes one level back: the lines picked cleared -> hunks -> out of the diff. Returns false
    // when already out.
    pub fn back(&mut self) -> bool {
        match self.focus {
            None => return false,
            Some(DiffPart::Line) if self.cursor.cancel_range() => {}
            Some(DiffPart::Hunk) => self.focus = None,
            Some(DiffPart::Line) => {
                self.focus = Some(DiffPart::Hunk);
                if let Some(diff) = &self.diff {
                    self.cursor.leave_lines(diff);
                }
            }
        }
        true
    }

    // Starts picking lines from the selected one, or ends it keeping them picked, while selecting
    // lines, see `DiffCursor::toggle_range`.
    pub fn toggle_range(&mut self) {
        if self.is_selecting_lines() {
            self.cursor.toggle_range();
        }
    }

    pub fn has_range(&self) -> bool {
        self.is_selecting_lines() && self.cursor.has_range()
    }

    // Whether the lines picked are extended by moving, see `toggle_range`.
    pub fn is_visual(&self) -> bool {
        self.is_selecting_lines() && self.cursor.is_visual()
    }

    // What is selected, for the keys and messages: "hunk", "line" or "lines".
    pub fn selected_name(&self) -> &'static str {
        match self.selected_part() {
            _ if self.has_range() => "lines",
            part => part.name(),
        }
    }

    // Moves the selected hunk or line.
    pub fn step(&mut self, step: Step) {
        let Some(diff) = &self.diff else {
            return;
        };
        match self.focus {
            None => {}
            Some(DiffPart::Hunk) => self.cursor.move_hunk(diff, step),
            Some(DiffPart::Line) => self.cursor.move_line(diff, step),
        }
    }

    // The patch of the selected hunk or line, to apply in `direction`.
    pub fn selected_patch(&self, direction: PatchDirection) -> Option<String> {
        let diff = self.diff.as_ref()?;
        match self.focus {
            None => None,
            Some(DiffPart::Hunk) => self.cursor.selected_hunk(diff).map(DiffHunk::patch),
            Some(DiffPart::Line) => self.cursor.line_patch(diff, direction),
        }
    }

    // The selected line as in the file, or the selected hunk as a diff, to copy.
    pub fn selected_text(&self) -> Option<String> {
        let diff = self.diff.as_ref().filter(|_| self.is_focused())?;
        self.cursor.selected_text(diff, self.selected_part())
    }

    // Selects a row matching `query` from the row `from`, see `DiffCursor::search`. Returns false
    // when nothing matches.
    pub fn search(
        &mut self,
        query: &str,
        from: usize,
        direction: SearchDirection,
        options: SearchOptions,
    ) -> bool {
        let Some(diff) = &self.diff else {
            return false;
        };
        let found = self.cursor.search(diff, query, from, direction, options);
        if found.is_some() {
            self.focus = found;
        }
        found.is_some()
    }

    // The row a search goes from: the one the keys are on.
    pub fn selected_row(&self) -> usize {
        self.cursor.selected_row()
    }

    // How the search for `query` went over the rows of the diff, None without one.
    pub fn search_outcome(&self, query: &str, options: SearchOptions) -> Option<SearchOutcome> {
        let diff = self.diff.as_ref()?;
        let matched = (0..diff.rows.len())
            .map(|row| row_matches(diff, row, query, options))
            .collect::<Vec<_>>();
        Some(SearchOutcome::of(
            query,
            self.selected_row(),
            matched.into_iter(),
        ))
    }

    // Draws the diff in a panel titled `title`, like the timeline and the local changes do: the
    // selection bar in the padding, the scrollbar on the right border. Returns the area inside.
    pub fn render_panel(
        &mut self,
        area: Rect,
        title: Line,
        buf: &mut Buffer,
        ctx: &AppContext,
        focused: bool,
        search: Option<&str>,
    ) -> Rect {
        let inner = render_panel(buf, area, title, focused, &ctx.color_theme);
        let scrollbar = Rect::new(area.right().saturating_sub(1), inner.y, 1, inner.height);
        self.render(inner, scrollbar, buf, ctx, focused, search);
        inner
    }

    // Draws the diff, its selected hunk or line highlighted while it has the focus, and the
    // scrollbar in the `scrollbar` column, on the right border.
    pub fn render(
        &mut self,
        area: Rect,
        scrollbar: Rect,
        buf: &mut Buffer,
        ctx: &AppContext,
        focused: bool,
        search: Option<&str>,
    ) {
        let theme = &ctx.color_theme;
        let wrap = ctx.wrap.get();
        let scrolloff = ctx.ui_config.common.scrolloff as usize;
        let Some(diff) = self.diff.as_ref().or(self.previous.as_ref()) else {
            return;
        };
        // the scroll counts screen rows while wrapping, see `WrapMap`; at another width or wrap
        // the rows are measured again, showing the selection again with them
        if !self.cursor.wrap().matches(area.width, wrap) {
            let map = WrapMap::of(diff, theme, area.width, wrap);
            self.cursor.set_wrap_map(map);
            if self.focus.is_some() {
                let rows = self.cursor.highlighted(diff, self.selected_part());
                self.cursor.scroll_to(rows);
            }
        }
        let total = self.cursor.wrap().total();
        let range = self
            .cursor
            .scroll
            .fit(total, area.height as usize, scrolloff);
        render_scrollbar(buf, scrollbar, total, range.start, focused, theme);
        // only the rows the screen range overlaps are built
        let rows = self.cursor.wrap().logical_rows(range.clone());
        let skip = range.start - self.cursor.wrap().visual_start(rows.start);
        let mut lines = diff_lines(diff, theme, rows.clone(), area.width);
        // a hunk selects only its changed lines, the context around them staying as it is
        let selected = |row: usize| {
            self.focus
                .is_some_and(|part| self.cursor.is_highlighted(diff, part, row))
        };
        if let Some(query) = search {
            for line in lines.iter_mut() {
                highlight_match(line, query, ctx.search_options.get(), theme);
            }
        }
        render_diff_lines(
            rows.zip(lines),
            skip..skip + range.len(),
            selected,
            wrap,
            theme,
            area,
            buf,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        test_support::ui::{dump, test_ctx},
        widget::diff::parse_diff,
    };

    // A diff whose rows fit the panel unwrapped, so only their wrapping makes it scroll.
    fn long_line_diff() -> ParsedDiff {
        parse_diff(&format!(
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+{}\n",
            "x".repeat(80)
        ))
    }

    fn draw(pane: &mut DiffPane, ctx: &AppContext, area: Rect) -> Buffer {
        let mut buf = Buffer::empty(area);
        let scrollbar = Rect::new(area.right().saturating_sub(1), area.y, 1, area.height);
        pane.render(area, scrollbar, &mut buf, ctx, false, None);
        buf
    }

    #[test]
    fn test_wrap_scrolls_the_wrapped_rows() {
        let ctx = test_ctx();
        ctx.wrap.set(true);
        let area = Rect::new(0, 0, 20, 5);
        let mut pane = DiffPane::default();
        pane.set_diff(Some(long_line_diff()));

        let top = draw(&mut pane, &ctx, area);
        assert_eq!(pane.cursor.scroll.offset, 0);
        // the wrapped rows are more than the panel, so a page down moves the view
        assert!(pane.cursor.wrap().total() > area.height as usize);
        pane.scroll(UserEvent::PageDown);
        let scrolled = draw(&mut pane, &ctx, area);
        assert!(
            pane.cursor.scroll.offset > 0,
            "the wrapped diff didn't scroll"
        );
        assert_ne!(dump(&top), dump(&scrolled));

        // and back to the top
        pane.scroll(UserEvent::PageUp);
        draw(&mut pane, &ctx, area);
        assert_eq!(pane.cursor.scroll.offset, 0);
    }

    #[test]
    fn test_unwrapped_diff_does_not_scroll() {
        let ctx = test_ctx();
        let area = Rect::new(0, 0, 20, 5);
        let mut pane = DiffPane::default();
        pane.set_diff(Some(long_line_diff()));

        draw(&mut pane, &ctx, area);
        // the logical rows fit the panel, so there is nothing to scroll
        assert_eq!(pane.cursor.wrap().total(), 3);
        pane.scroll(UserEvent::PageDown);
        draw(&mut pane, &ctx, area);
        assert_eq!(pane.cursor.scroll.offset, 0);
    }

    // The diff before stays drawn while the next one loads, without a key acting on it.
    #[test]
    fn test_diff_before_drawn_while_loading() {
        let ctx = test_ctx();
        let area = Rect::new(0, 0, 20, 5);
        let ec = crate::event::EventController::without_terminal();
        let mut pane = DiffPane::default();
        pane.set_diff(Some(long_line_diff()));
        pane.enter();
        assert!(pane.is_focused());

        let next = "diff --git a/g b/g\n--- a/g\n+++ b/g\n@@ -1 +1 @@\n-gone\n+new\n";
        pane.load_in_background(ec.sender(), move || next.to_string());
        let shown = dump(&draw(&mut pane, &ctx, area));
        assert!(shown.contains("old"), "{shown}");
        assert!(!pane.is_focused());
        assert!(pane.diff().is_none());
        assert!(pane.selected_text().is_none());

        // loaded, as `poll_loading` shows it
        pane.set_diff(Some(parse_diff(next)));
        let shown = dump(&draw(&mut pane, &ctx, area));
        assert!(shown.contains("new") && !shown.contains("old"), "{shown}");
    }

    #[test]
    fn test_selects_once_the_diff_loading_is_shown() {
        use crate::{
            event::EventController,
            git::{Repository, SortCommit},
            test_support::git::GitRepository,
        };
        let dir = tempfile::tempdir().unwrap();
        let git = GitRepository::new(dir.path());
        git.init();
        git.commit("init", "2024-01-01");
        let repository =
            Repository::load(dir.path(), SortCommit::Chronological, None, false).unwrap();
        let ec = EventController::without_terminal();
        let raw =
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n@@ -5 +5 @@\n-c\n+d\n";

        // the hunk selected before a reload, while the reloaded diff still loads
        let mut pane = DiffPane::default();
        pane.load_in_background(ec.sender(), move || raw.to_string());
        let selection = DiffSelection {
            focus: Some(DiffPart::Hunk),
            hunk: 1,
            line: 0,
        };
        pane.restore(selection);
        pane.wait_loading(&repository);
        assert!(pane.is_focused());
        assert_eq!(pane.cursor.hunk, 1);

        // entering, its first hunk
        pane.load_in_background(ec.sender(), move || raw.to_string());
        assert!(pane.enter());
        // its keys taken meanwhile, see `focused_part`
        assert!(pane.is_focused());
        pane.wait_loading(&repository);
        assert!(pane.is_focused());
        assert_eq!(pane.cursor.hunk, 0);

        // another diff loading instead drops what was to be selected
        pane.load_in_background(ec.sender(), move || raw.to_string());
        pane.restore(selection);
        pane.load_in_background(ec.sender(), move || raw.to_string());
        pane.wait_loading(&repository);
        assert!(!pane.is_focused());
    }
}
