use std::{ops::RangeInclusive, sync::Arc, time::Instant};

use ratatui::{buffer::Buffer, layout::Rect, text::Line};

use crate::{
    background::Job,
    color::ColorTheme,
    context::AppContext,
    event::Sender,
    git::{ConvertedPaths, PatchDirection, Repository},
    keybind::UserEvent,
    keys::Keys,
    layout::{RowMap, Target},
    pointer::OnPicks,
    search::{SearchOptions, SearchOutcome},
    widget::{
        diff::{
            diff_lines, matching_rows, parse_diff, render_diff_lines, render_split_rows, rewrap,
            split_widths, DiffCursor, DiffHunk, DiffLayout, DiffPart, DiffRowKind, Measuring,
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

// What the pane has of its diff.
#[derive(Debug, Default)]
enum DiffState {
    // loaded lazily, nothing until then
    #[default]
    Nothing,
    // the one selected, which the keys act on
    Shown(Arc<ParsedDiff>),
    // no longer the one selected: drawn until the next one is shown, where it was scrolled; no
    // key acts on it
    Outdated(Arc<ParsedDiff>),
    // loading in the background, shown once loaded, see `load_in_background`; the diff before
    // drawn meanwhile, and what to select once it is shown, see `restore` and `enter`
    Loading {
        job: Job<ParsedDiff>,
        previous: Option<Arc<ParsedDiff>>,
        pending: Option<DiffSelection>,
    },
}

// A diff beside what picks it: moving into it selects hunks, then lines, to copy, link to, apply or
// stage; the changes tab, the timeline and the local changes.
#[derive(Debug, Default)]
pub struct DiffPane {
    state: DiffState,
    // the hunks or the lines while the diff has the focus, None while what is beside it does (the
    // files of the changes tab, the commits of the timeline)
    focus: Option<DiffPart>,
    cursor: DiffCursor,
    // the app's, from the last diff loaded in the background, see `load_in_background`, to measure
    // a big diff's wrapped rows on another thread too; without it, as in the tests, right away
    tx: Option<Sender>,
    // the wrapped rows of a big diff measuring on another thread, see `rewrap`
    measuring: Option<Measuring>,
    // the rows of the diff shown matching the last search, with its query and options, see
    // `search`: searched once for the letter typed and how many match, then for n and N
    matches: Option<(String, SearchOptions, Vec<usize>)>,
}

impl DiffPane {
    // Shows another diff, from the top, the focus beside it.
    pub fn set_diff(&mut self, diff: Option<ParsedDiff>) {
        self.state = diff.map_or(DiffState::Nothing, |diff| DiffState::Shown(Arc::new(diff)));
        self.measuring = None;
        self.matches = None;
        self.focus = None;
        self.cursor.reset();
    }

    // The diff is no longer the one selected: drawn until the next one is shown, where it was
    // scrolled, the focus beside it so no key acts on it. The one loading is of what was selected
    // too, so it is dropped.
    pub fn outdate(&mut self) {
        self.state = match std::mem::take(&mut self.state) {
            DiffState::Shown(diff) | DiffState::Outdated(diff) => DiffState::Outdated(diff),
            DiffState::Loading { previous, .. } => {
                previous.map_or(DiffState::Nothing, DiffState::Outdated)
            }
            DiffState::Nothing => DiffState::Nothing,
        };
        self.focus = None;
    }

    pub fn diff(&self) -> Option<&ParsedDiff> {
        shown(&self.state).map(Arc::as_ref)
    }

    // What to select in the diff loading, once it is shown.
    fn pending(&self) -> Option<DiffSelection> {
        match &self.state {
            DiffState::Loading { pending, .. } => *pending,
            _ => None,
        }
    }

    // Loads another diff with `work`, the git command giving it, on another thread, the focus
    // beside it, parsed there too, which pairing the lines of a big change can take a while for;
    // the one before is only drawn meanwhile, where it was scrolled, so no key acts on it. See
    // `poll_loading`.
    pub fn load_in_background(
        &mut self,
        tx: Sender,
        converted: ConvertedPaths,
        work: impl FnOnce() -> String + Send + 'static,
    ) {
        self.outdate();
        let previous = match std::mem::take(&mut self.state) {
            DiffState::Outdated(diff) => Some(diff),
            _ => None,
        };
        self.tx = Some(tx.clone());
        self.state = DiffState::Loading {
            job: Job::spawn(tx, move || {
                let mut diff = parse_diff(&work());
                diff.drop_converted(&converted);
                diff
            }),
            previous,
            pending: None,
        };
    }

    // Shows the diff loading in the background once it is loaded; returns whether it was.
    pub fn poll_loading(&mut self) -> bool {
        let DiffState::Loading { job, .. } = &self.state else {
            return false;
        };
        let Some(diff) = job.try_take() else {
            return false;
        };
        self.show(diff, self.pending());
        true
    }

    // Shows the diff loading in the background, waiting for it, before a key acts on it.
    pub fn wait_loading(&mut self) {
        if !matches!(self.state, DiffState::Loading { .. }) {
            return;
        }
        if let DiffState::Loading { job, pending, .. } = std::mem::take(&mut self.state) {
            self.show(job.wait(), pending);
        }
    }

    // Shows the diff `raw`, the output of the git command giving it, in place of the one loading.
    pub fn show_loaded(&mut self, raw: &str, repository: &Repository) {
        let mut diff = parse_diff(raw);
        diff.drop_converted(&repository.converted_paths());
        self.show(diff, self.pending());
    }

    // Shows `diff`, selecting `pending` in it, see `restore`.
    fn show(&mut self, diff: ParsedDiff, pending: Option<DiffSelection>) {
        self.set_diff(Some(diff));
        if let Some(selection) = pending {
            self.restore(selection);
        }
    }

    // When its diff started loading in the background, None when it isn't.
    pub fn loading_since(&self) -> Option<Instant> {
        match &self.state {
            DiffState::Loading { job, .. } => Some(job.started()),
            _ => None,
        }
    }

    // Whether the diff is of one file, which shows more or less context around its changes.
    // Of the diff drawn while another loads too, the keys being those of the diff reloading.
    pub fn is_single_file(&self) -> bool {
        drawn(&self.state).is_some_and(|diff| diff.is_single_file())
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
        if let DiffState::Loading { pending, .. } = &mut self.state {
            *pending = Some(selection);
            return;
        }
        let Some(diff) = shown(&self.state) else {
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
    // page, or along when it would leave the window; returns whether `event` was one.
    pub fn scroll(&mut self, event: UserEvent) -> bool {
        let focused = shown(&self.state).zip(self.focus);
        if let Some((diff, part)) = focused {
            if self.cursor.jump_page(diff, event, part) {
                return true;
            }
        }
        if !self.cursor.scroll.handle_scroll(event) {
            return false;
        }
        // ctrl-e and ctrl-y, see `keep_in_view`; the wheel leaves the selection, even off the rows
        let wheel = matches!(event, UserEvent::Wheel(_));
        if let Some((diff, part)) = focused.filter(|_| !wheel) {
            self.cursor.keep_in_view(diff, part);
        }
        true
    }

    // The part with the focus, or that will have it once the diff loading is shown (see
    // `restore`): the keys of the diff are taken meanwhile, each waiting for it before it acts.
    fn focused_part(&self) -> Option<DiffPart> {
        self.focus.or(self.pending().and_then(|p| p.focus))
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
        let diff = self.diff().filter(|_| self.is_focused())?;
        let rows = self.cursor.highlighted(diff, self.selected_part());
        let mut lines = diff
            .rows
            .get(rows)?
            .iter()
            .filter_map(|row| Some(row.new?.get() as usize));
        let first = lines.next()?;
        Some(first..=lines.next_back().unwrap_or(first))
    }

    // Goes one level deeper, like on the local changes page: into the hunks, then the lines of the
    // selected hunk. Returns false when the diff has no hunk.
    // While the diff loads in the background, its first hunk once it is shown.
    pub fn enter(&mut self) -> bool {
        if let DiffState::Loading { pending, .. } = &mut self.state {
            let first = DiffSelection {
                focus: Some(DiffPart::Hunk),
                hunk: 0,
                line: 0,
            };
            *pending = Some(first);
            return true;
        }
        let Some(diff) = shown(&self.state) else {
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

    // Selects what the mouse points at, at `row` of the diff: its hunk, the cursor on the changed
    // line there, or that line while selecting lines (in the hunk of the lines picked); a drag
    // starting there goes into the lines, see `OnPicks`. Off the rows, on empty space, a border or
    // the thumb, it goes into the diff, no deeper when already in it, at the first hunk shown, the
    // diff not scrolling.
    pub fn point(&mut self, row: Option<usize>, picks: OnPicks) {
        // off the lines picked, a header or empty space too
        if self.focus == Some(DiffPart::Line) {
            self.cursor.prepare_point(row, picks);
        }
        let Some(row) = row else {
            if self.is_focused() {
                return;
            }
            // while it loads, its first hunk once shown
            let Some(diff) = shown(&self.state) else {
                self.enter();
                return;
            };
            let offset = self.cursor.scroll.offset;
            let top = self.cursor.wrap().logical_at(offset);
            let hunk = diff.hunks.iter().position(|h| h.rows.end > top);
            if self.cursor.select_hunk(diff, hunk.unwrap_or(usize::MAX)) {
                self.focus = Some(DiffPart::Hunk);
                self.cursor.scroll.offset = offset;
            }
            return;
        };
        let Some(diff) = shown(&self.state) else {
            return;
        };
        let Some(hunk) = diff.hunks.iter().position(|h| h.rows.contains(&row)) else {
            return;
        };
        let change = diff.rows[row].is_change();
        if self.focus == Some(DiffPart::Line) {
            if change && (hunk == self.cursor.hunk || !self.cursor.has_range()) {
                self.cursor.select_line(diff, row);
            }
            return;
        }
        self.cursor.select_hunk(diff, hunk);
        if change {
            self.cursor.line = row;
        }
        self.focus = Some(DiffPart::Hunk);
        if picks == OnPicks::StartRange && self.cursor.enter_lines(diff) {
            self.focus = Some(DiffPart::Line);
        }
    }

    // Clears the lines picked, keeping the selected one.
    pub fn clear_picks(&mut self) {
        self.cursor.cancel_range();
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
                if let Some(diff) = shown(&self.state) {
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

    // The line h (the old side) or l (the new side) selects while selecting lines side by side,
    // see `DiffCursor::other_side`.
    fn other_side(&self, event: UserEvent) -> Option<usize> {
        let kind = match event {
            UserEvent::NavigateLeft => DiffRowKind::Removed,
            UserEvent::NavigateRight => DiffRowKind::Added,
            _ => return None,
        };
        let diff = shown(&self.state).filter(|_| self.focus == Some(DiffPart::Line))?;
        self.cursor.other_side(diff, kind)
    }

    pub fn can_switch_side(&self, event: UserEvent) -> bool {
        self.other_side(event).is_some()
    }

    // Selects the line on the other side for h or l, see `other_side`; false when there is none.
    pub fn switch_side(&mut self, event: UserEvent) -> bool {
        let (Some(row), Some(diff)) = (self.other_side(event), shown(&self.state)) else {
            return false;
        };
        self.cursor.select_line(diff, row);
        true
    }

    // Moves the selected hunk or line.
    pub fn step(&mut self, step: Step) {
        let Some(diff) = shown(&self.state) else {
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
        let diff = self.diff()?;
        match self.focus {
            None => None,
            Some(DiffPart::Hunk) => self.cursor.selected_hunk(diff).map(DiffHunk::patch),
            Some(DiffPart::Line) => self.cursor.line_patch(diff, direction),
        }
    }

    // The selected line as in the file, or the selected hunk as a diff, to copy.
    pub fn selected_text(&self) -> Option<String> {
        let diff = self.diff().filter(|_| self.is_focused())?;
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
        let Some(diff) = shown(&self.state) else {
            return false;
        };
        let searched = matches!(&self.matches, Some((q, o, _)) if q == query && *o == options);
        if !searched {
            let rows = matching_rows(diff, query, options);
            self.matches = Some((query.to_string(), options, rows));
        }
        let matches = self.matches.as_ref().map_or(&[][..], |(_, _, rows)| rows);
        let found = self.cursor.search(diff, matches, from, direction);
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
        let selected = self.selected_row();
        Some(match &self.matches {
            Some((q, o, rows)) if q == query && *o == options => {
                SearchOutcome::of_matches(query, selected, rows)
            }
            _ => SearchOutcome::of_matches(
                query,
                selected,
                &matching_rows(self.diff()?, query, options),
            ),
        })
    }

    // Draws the diff in a panel titled `title`, like the timeline and the local changes do: the
    // selection bar in the padding, the scrollbar on the right border. Returns the area inside.
    #[expect(clippy::too_many_arguments)]
    pub fn render_panel(
        &mut self,
        area: Rect,
        target: Target,
        title: Line,
        buf: &mut Buffer,
        ctx: &AppContext,
        focused: bool,
        search: Option<&str>,
    ) -> Rect {
        let inner = render_panel(buf, area, title, focused, &ctx.color_theme);
        ctx.layout.borrow_mut().frame(target, area);
        let scrollbar = Rect::new(area.right().saturating_sub(1), inner.y, 1, inner.height);
        self.render(inner, target, scrollbar, buf, ctx, focused, search);
        inner
    }

    // Draws the diff, its selected hunk or line highlighted while it has the focus, and the
    // scrollbar in the `scrollbar` column, on the right border.
    #[expect(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        area: Rect,
        target: Target,
        scrollbar: Rect,
        buf: &mut Buffer,
        ctx: &AppContext,
        focused: bool,
        search: Option<&str>,
    ) {
        let theme = &ctx.color_theme;
        let wrap = ctx.wrap.get();
        let layout = ctx.diff_layout.get();
        let scrolloff = ctx.ui_config.common.scrolloff as usize;
        let Some(diff) = drawn(&self.state).cloned() else {
            ctx.layout.borrow_mut().record(target, area, RowMap::None);
            return;
        };
        self.measure(&diff, theme, area.width, wrap, layout);
        let diff = diff.as_ref();
        let wrap = self.cursor.wrap().wraps();
        let total = self.cursor.wrap().total();
        let range = self
            .cursor
            .scroll
            .fit(total, area.height as usize, scrolloff);
        let thumb = render_scrollbar(buf, scrollbar, total, range.start, focused, theme);
        ctx.layout.borrow_mut().record_thumb(target, thumb);
        // a hunk selects only its changed lines, the context around them staying as it is
        let selected = |row: usize| {
            self.focus
                .is_some_and(|part| self.cursor.is_highlighted(diff, part, row))
        };
        let restyle = |line: &mut Line| {
            if let Some(query) = search {
                highlight_match(line, query, ctx.search_options.get(), theme);
            }
        };
        if self.cursor.wrap().split_rows().is_some() {
            let map = self.cursor.wrap();
            let lines =
                render_split_rows(diff, map, range, selected, restyle, wrap, theme, area, buf);
            let split = split_widths(area.width).0 as usize;
            ctx.layout
                .borrow_mut()
                .record(target, area, RowMap::Sides { lines, split });
            return;
        }
        let lines = (range.start..range.start + area.height as usize)
            .map(|screen| (screen < total).then(|| self.cursor.wrap().logical_at(screen)))
            .collect();
        ctx.layout
            .borrow_mut()
            .record(target, area, RowMap::Lines(lines));
        // only the rows the screen range overlaps are built
        let rows = self.cursor.wrap().logical_rows(range.clone());
        let skip = range.start - self.cursor.wrap().visual_start(rows.start);
        let mut lines = diff_lines(diff, theme, rows.clone(), area.width);
        lines.iter_mut().for_each(restyle);
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

impl DiffPane {
    // The scroll counts screen rows while wrapping or split, see `WrapMap`: at another width, wrap
    // or layout the rows are measured again, showing the selection again with them. A big diff's
    // take a while to wrap: they are measured on another thread, drawn unwrapped meanwhile.
    fn measure(
        &mut self,
        diff: &Arc<ParsedDiff>,
        theme: &ColorTheme,
        width: u16,
        wrap: bool,
        layout: DiffLayout,
    ) {
        let work = || {
            let (shared, colors) = (diff.clone(), theme.clone());
            move || WrapMap::of(&shared, &colors, width, wrap, layout)
        };
        let Some(map) = rewrap(
            self.cursor.wrap(),
            &mut self.measuring,
            self.tx.as_ref(),
            diff.rows.len(),
            (width, wrap, layout),
            |wrap| WrapMap::of(diff, theme, width, wrap, layout),
            work,
        ) else {
            return;
        };
        self.cursor.set_wrap_map(map);
        if self.focus.is_some() {
            let rows = self.cursor.highlighted(diff, self.selected_part());
            self.cursor.scroll_to(rows);
        }
    }
}

// The diff shown, which the keys act on.
fn shown(state: &DiffState) -> Option<&Arc<ParsedDiff>> {
    match state {
        DiffState::Shown(diff) => Some(diff),
        _ => None,
    }
}

// The diff drawn: the one shown, else the one before while the next loads.
fn drawn(state: &DiffState) -> Option<&Arc<ParsedDiff>> {
    match state {
        DiffState::Shown(diff) | DiffState::Outdated(diff) => Some(diff),
        DiffState::Loading { previous, .. } => previous.as_ref(),
        DiffState::Nothing => None,
    }
}

// The keys of a diff, alike on every page, where `pane` gives it while it has the focus: into the
// lines of the selected hunk, and copying the hunk or line.
pub fn register_diff_keys<T: ?Sized>(
    k: &mut Keys<T>,
    pane: fn(&T) -> Option<&DiffPane>,
    context: fn(&T) -> usize,
) {
    use UserEvent::*;
    k.add(
        &[Confirm],
        "select lines",
        "Select the hunks of the diff, then their lines",
    )
    .when(|v| pane(v).is_some_and(|p| !p.is_selecting_lines()))
    .moves(|_| true);
    k.add(&[ShortCopy], "copy hunk", "Copy the selected hunk or line")
        .when(|v| pane(v).is_some())
        .short(|v| {
            let name = pane(v).map_or(DiffPart::Hunk.name(), DiffPane::selected_name);
            format!("copy {name}")
        });
    // like the lines of a file preview
    k.add(
        &[ToggleSelect],
        "select lines",
        "Select the lines from the selected one to where it moves, in its hunk; another selection adds to them, Esc clears them",
    )
    .when(|v| pane(v).is_some_and(DiffPane::is_selecting_lines))
    .short(|v| {
        match pane(v).is_some_and(DiffPane::is_visual) {
            true => "end select",
            false => "select lines",
        }
        .into()
    });
    // side by side, j and k move along a side, h and l across, each only where it switches
    k.add(
        &[NavigateLeft],
        "old side",
        "In a diff side by side, select the nearest line on the old side, in the hunk",
    )
    .when(|v| pane(v).is_some_and(|p| p.can_switch_side(NavigateLeft)));
    k.add(
        &[NavigateRight],
        "new side",
        "In a diff side by side, select the nearest line on the new side, in the hunk",
    )
    .when(|v| pane(v).is_some_and(|p| p.can_switch_side(NavigateRight)));
    // like the files in their folders or as a list, how the files are shown
    k.add(
        &[ToggleTreeView],
        "split/unified",
        "Show the diffs side by side, or in one column",
    )
    .when(|v| pane(v).is_some());
    // a diff of several files has no one file to show more or less context of
    let single_file = |v: &T| pane(v).is_some_and(DiffPane::is_single_file);
    k.add(
        &[IncreaseDiffContext],
        "more context",
        "Show more lines around the changes",
    )
    .when(single_file);
    k.add(
        &[DecreaseDiffContext],
        "less context",
        "Show fewer lines around the changes",
    )
    .when(move |v| single_file(v) && context(v) > 1);
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
        pane.render(
            area,
            Target::DetailPane,
            scrollbar,
            &mut buf,
            ctx,
            false,
            None,
        );
        buf
    }

    // What to select once the diff loading is shown goes with it: a diff set meanwhile used to
    // leave the pane focused, with no diff to act on.
    #[test]
    fn test_diff_set_while_loading_drops_what_was_to_be_selected() {
        let ec = crate::event::EventController::without_terminal();
        let mut pane = DiffPane::default();
        pane.load_in_background(ec.sender(), Default::default(), String::new);
        assert!(pane.enter());
        assert!(pane.is_focused());
        pane.set_diff(None);
        assert!(!pane.is_focused());
    }

    // ctrl-e and ctrl-y scroll the diff like vim: the selected line comes along once it would
    // leave the window, the selected hunk once it left it whole. They used to scroll it alone.
    #[test]
    fn test_scroll_keys_pull_the_selection_along() {
        let ctx = test_ctx();
        let area = Rect::new(0, 0, 30, 4);
        // three hunks of a header and two changed lines, rows 0-2, 3-5 and 6-8
        let hunk = |n: usize| format!("@@ -{n},2 +{n},2 @@\n-old{n}\n+new{n}\n");
        let raw = format!(
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n{}{}{}",
            hunk(1),
            hunk(10),
            hunk(20)
        );
        let mut pane = DiffPane::default();
        pane.set_diff(Some(parse_diff(&raw)));
        draw(&mut pane, &ctx, area);
        assert!(pane.enter());
        let hunk_of = |pane: &DiffPane| pane.cursor.hunk;

        // the first hunk stays while any of it is in view
        pane.scroll(UserEvent::ScrollDown);
        pane.scroll(UserEvent::ScrollDown);
        assert_eq!(hunk_of(&pane), 0);
        // gone whole: the nearest hunk in view
        pane.scroll(UserEvent::ScrollDown);
        assert_eq!(hunk_of(&pane), 1);
        assert_eq!(pane.cursor.scroll.offset, 3);

        // the lines: the selected one comes along to the nearest changed line in view
        assert!(pane.enter());
        let line = pane.cursor.line;
        pane.scroll(UserEvent::ScrollDown);
        pane.scroll(UserEvent::ScrollDown);
        assert!(pane.cursor.line > line);
        assert!(pane.cursor.line >= pane.cursor.scroll.offset);
    }

    // The wheel scrolls the diff alone: the selected hunk stays, even off the rows shown.
    #[test]
    fn test_wheel_leaves_the_selection() {
        let ctx = test_ctx();
        let area = Rect::new(0, 0, 30, 4);
        let hunk = |n: usize| format!("@@ -{n},2 +{n},2 @@\n-old{n}\n+new{n}\n");
        let raw = format!(
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n{}{}{}",
            hunk(1),
            hunk(10),
            hunk(20)
        );
        let mut pane = DiffPane::default();
        pane.set_diff(Some(parse_diff(&raw)));
        draw(&mut pane, &ctx, area);
        assert!(pane.enter());
        for _ in 0..2 {
            assert!(pane.scroll(UserEvent::Wheel(crate::widget::style::WHEEL_ROWS as isize)));
        }
        // the first hunk, rows 0-2, is off the rows shown from 4
        assert_eq!(pane.cursor.hunk, 0);
        draw(&mut pane, &ctx, area);
        assert_eq!(
            pane.cursor.scroll.offset,
            2 * crate::widget::style::WHEEL_ROWS
        );
    }

    // Split, the old side is left of the divider and the new side right of it, a removed line
    // beside the added one replacing it; the selected line marked on its row, and a click takes
    // the line on the side clicked.
    #[test]
    fn test_split_draws_the_sides_and_clicks_take_them() {
        let ctx = test_ctx();
        ctx.diff_layout.set(DiffLayout::Split);
        // one column of the panel's padding, for the selection bar
        let area = Rect::new(1, 0, 29, 4);
        let mut pane = DiffPane::default();
        pane.set_diff(Some(parse_diff(
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n a\n-old one\n+new one\n",
        )));
        assert!(pane.enter() && pane.enter());
        pane.step(Step::Next);
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 4));
        let scrollbar = Rect::new(area.right(), 0, 0, 4);
        pane.render(
            area,
            Target::DetailPane,
            scrollbar,
            &mut buf,
            &ctx,
            true,
            None,
        );
        let rows = dump(&buf);
        let rows: Vec<_> = rows.lines().map(str::trim_end).collect();
        assert_eq!(
            rows,
            [
                "         @@ -1,2 +1,2 @@",
                "   1 a         │  1 a",
                "▌  2 old one   │  2 new one",
                "",
            ]
        );
        // the added line is selected: its number marked, the removed one's not
        let selected = |x| buf[(x, 2)].style().bg == Some(ctx.color_theme.list_selected_bg);
        assert!(selected(17) && !selected(3));
        let layout = ctx.layout.borrow();
        let row_at = |x, y| layout.hit(x, y).and_then(|hit| hit.row);
        assert_eq!((row_at(2, 1), row_at(20, 1)), (Some(1), Some(1)));
        assert_eq!((row_at(2, 2), row_at(20, 2)), (Some(2), Some(3)));
        assert_eq!(row_at(2, 3), None);
    }

    // Side by side, j and k move along the side of the selected line, h and l to the nearest line
    // on the other side, in the hunk; lines picked stay on their side. In one column h and l do
    // nothing.
    #[test]
    fn test_split_moves_along_a_side_and_across() {
        let ctx = test_ctx();
        let area = Rect::new(0, 0, 60, 10);
        let mut pane = DiffPane::default();
        // the rows side by side: (-, totally new), (call(a), call(b)), (zzz, -), (foo baz, foo qux)
        pane.set_diff(Some(parse_diff(concat!(
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@\n",
            "-call(a, 1);\n-zzz qqq\n-foo bar baz\n",
            "+totally new\n+call(b, 1);\n+foo bar qux\n",
        ))));
        assert!(pane.enter() && pane.enter());
        draw(&mut pane, &ctx, area);
        assert_eq!(pane.selected_row(), 1);
        assert!(!pane.can_switch_side(UserEvent::NavigateRight));

        ctx.diff_layout.set(DiffLayout::Split);
        draw(&mut pane, &ctx, area);
        let mut moves = vec![];
        for event in ["j", "j", "j", "l", "h", "l", "k", "k", "k", "h"] {
            match event {
                "j" => pane.step(Step::Next),
                "k" => pane.step(Step::Prev),
                "h" => assert!(pane.switch_side(UserEvent::NavigateLeft)),
                _ => assert!(pane.switch_side(UserEvent::NavigateRight)),
            }
            moves.push(pane.selected_row());
        }
        assert_eq!(moves, [2, 3, 3, 6, 3, 6, 5, 4, 4, 1]);
        // on the old side already
        assert!(!pane.switch_side(UserEvent::NavigateLeft));

        pane.toggle_range();
        pane.step(Step::Next);
        pane.step(Step::Next);
        assert_eq!(pane.selected_row(), 3);
        assert_eq!(
            pane.selected_text().as_deref(),
            Some("call(a, 1);\nzzz qqq\nfoo bar baz")
        );
        assert!(!pane.can_switch_side(UserEvent::NavigateRight));
    }

    // Side by side, on the new side, the page keys move the selected line a page along it, and
    // ctrl-e pulls it to the first line shown: the added lines of a change come after its removed
    // ones in the diff, beside them on screen. They used to jump to the end, and pull to the last.
    #[test]
    fn test_split_pages_along_the_new_side() {
        let ctx = test_ctx();
        ctx.diff_layout.set(DiffLayout::Split);
        let area = Rect::new(0, 0, 60, 5);
        let mut raw = String::from("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,40 +1,40 @@\n");
        for i in 0..40 {
            raw += &format!("-let a{i} = f(x, {i});\n");
        }
        for i in 0..40 {
            raw += &format!("+let a{i} = f(y, {i});\n");
        }
        let start = |ctx: &AppContext| {
            let mut pane = DiffPane::default();
            pane.set_diff(Some(parse_diff(&raw)));
            assert!(pane.enter() && pane.enter());
            draw(&mut pane, ctx, area);
            assert!(pane.switch_side(UserEvent::NavigateRight));
            draw(&mut pane, ctx, area);
            pane
        };
        let mut pane = start(&ctx);
        let mut rows = vec![pane.selected_row()];
        for event in [UserEvent::PageDown, UserEvent::PageDown, UserEvent::PageUp] {
            pane.scroll(event);
            draw(&mut pane, &ctx, area);
            rows.push(pane.selected_row());
        }
        let mut pane = start(&ctx);
        for _ in 0..3 {
            pane.scroll(UserEvent::ScrollDown);
            draw(&mut pane, &ctx, area);
            rows.push(pane.selected_row());
        }
        // a page down, another, back up; then ctrl-e three times
        assert_eq!(rows, [41, 46, 51, 46, 42, 43, 44]);
    }

    // A big diff's rows are wrapped on another thread, as measuring them takes a while: drawn
    // unwrapped meanwhile, wrapped once measured.
    #[test]
    fn test_big_diff_wraps_in_the_background() {
        let ctx = test_ctx();
        ctx.wrap.set(true);
        let area = Rect::new(0, 0, 20, 5);
        let long = "x".repeat(40);
        let mut raw = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -0,0 +1,30000 @@\n".to_string();
        for _ in 0..30_000 {
            raw += &format!("+{long}\n");
        }
        let ec = crate::event::EventController::without_terminal();
        let mut pane = DiffPane::default();
        pane.load_in_background(ec.sender(), Default::default(), move || raw);
        pane.wait_loading();

        draw(&mut pane, &ctx, area);
        assert!(pane.measuring.is_some());
        assert!(!pane.cursor.wrap().wraps());
        let measured = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !pane.cursor.wrap().wraps() {
            assert!(std::time::Instant::now() < measured, "never wrapped");
            std::thread::sleep(std::time::Duration::from_millis(5));
            draw(&mut pane, &ctx, area);
        }
        assert!(pane.measuring.is_none());
        assert!(pane.cursor.wrap().total() > 30_000);
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
        pane.load_in_background(ec.sender(), Default::default(), move || next.to_string());
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
        let ec = crate::event::EventController::without_terminal();
        let raw =
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n@@ -5 +5 @@\n-c\n+d\n";

        // the hunk selected before a reload, while the reloaded diff still loads
        let mut pane = DiffPane::default();
        pane.load_in_background(ec.sender(), Default::default(), move || raw.to_string());
        let selection = DiffSelection {
            focus: Some(DiffPart::Hunk),
            hunk: 1,
            line: 0,
        };
        pane.restore(selection);
        pane.wait_loading();
        assert!(pane.is_focused());
        assert_eq!(pane.cursor.hunk, 1);

        // entering, its first hunk
        pane.load_in_background(ec.sender(), Default::default(), move || raw.to_string());
        assert!(pane.enter());
        // its keys taken meanwhile, see `focused_part`
        assert!(pane.is_focused());
        pane.wait_loading();
        assert!(pane.is_focused());
        assert_eq!(pane.cursor.hunk, 0);

        // another diff loading instead drops what was to be selected
        pane.load_in_background(ec.sender(), Default::default(), move || raw.to_string());
        pane.restore(selection);
        pane.load_in_background(ec.sender(), Default::default(), move || raw.to_string());
        pane.wait_loading();
        assert!(!pane.is_focused());
    }
}
