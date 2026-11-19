use std::{
    borrow::Cow, cell::OnceCell, collections::BTreeSet, num::NonZeroU32, ops::Range, sync::Arc,
};

use similar::DiffTag;
use unicode_width::UnicodeWidthChar;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::{
    color::ColorTheme,
    git::{ConvertedPaths, PatchDirection},
    keybind::UserEvent,
    pointer::OnPicks,
    search::SearchOptions,
    widget::{
        style::{
            divider, mark_selected_bar, placeholder, scroll_to, tabs_shown, ScrollState,
            VisualSelection,
        },
        tree_query::{find_around, match_range, patch_range, SearchDirection},
    },
};

// Why the hunks of a file aren't selectable, see `ParsedDiff::drop_converted`.
pub const CONVERTED_DIFF: &str =
    "This diff is text converted from the file (textconv): its hunks and lines can't be staged or applied, only the whole file";

#[derive(Debug, Default)]
pub struct ParsedDiff {
    // shared with the hunks, which are parts of it
    pub raw: Arc<str>,
    pub rows: Vec<DiffRow>,
    // the words changed in the rows, all in one list rather than one per row, see `DiffRow`
    emphasis: Vec<Range<usize>>,
    // the removed rows paired with the added row replacing them, by the removed row, see
    // `pair_changes`
    pairs: Vec<(usize, usize)>,
    pub hunks: Vec<DiffHunk>,
    // hunks were left out, of files diffed as text converted from them
    pub converted: bool,
    // how many files the diff spans
    files: usize,
    // the width of the line numbers, of the biggest one
    number_width: usize,
}

impl ParsedDiff {
    // Whether the diff is of one file, which shows more or less context around its changes.
    pub fn is_single_file(&self) -> bool {
        self.files == 1
    }

    // Leaves out the hunks of files git diffs as text converted from them, like a .docx with a
    // textconv driver: the text isn't in the file, so they can't be staged or applied. Bytes that
    // aren't UTF-8 are kept, see `text_from_git`.
    pub fn drop_converted(&mut self, converted: &ConvertedPaths) {
        let mut paths: Vec<&str> = self.hunks.iter().map(DiffHunk::path).collect();
        paths.dedup();
        let converted = converted.of(&paths);
        if converted.is_empty() {
            return;
        }
        self.hunks.retain(|h| !converted.contains(h.path()));
        self.converted = true;
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct DiffHunk {
    // rows of `ParsedDiff::rows` displaying this hunk, the header first and then one row per line
    pub rows: Range<usize>,
    // the whole diff, the parts below are byte ranges of
    diff: Arc<str>,
    // the "diff --git" line up to the "+++" line of the file this hunk belongs to
    file_header: Range<usize>,
    // the "@@ ... @@" line
    header: Range<usize>,
    old_start: usize,
    old_count: usize,
    // its lines, with their original line endings, the patch has to match the file byte for byte
    body: Range<usize>,
}

impl DiffHunk {
    fn file_header(&self) -> &str {
        &self.diff[self.file_header.clone()]
    }

    fn header(&self) -> &str {
        &self.diff[self.header.clone()]
    }

    fn body(&self) -> &str {
        &self.diff[self.body.clone()]
    }

    // Its lines, with their line endings.
    fn lines(&self) -> std::str::SplitInclusive<'_, char> {
        self.body().split_inclusive('\n')
    }

    // The path of its file, the old one of a deleted file.
    fn path(&self) -> &str {
        let path_of = |prefix: &str| {
            self.file_header()
                .lines()
                .find_map(|l| l.strip_prefix(prefix))
        };
        path_of("+++ b/")
            .or_else(|| path_of("--- a/"))
            .unwrap_or_default()
    }

    // The file header and this hunk alone, ready for `git apply`.
    pub fn patch(&self) -> String {
        // no counts of 0 on the other side: the hunk creates or deletes the file as it is
        let file_header = partial_file_header(self.file_header(), 0, 0);
        format!("{file_header}{}{}", self.header(), self.body())
    }

    // The hunk as shown in a diff: its header and lines.
    pub fn text(&self) -> String {
        format!("{}{}", self.header(), self.body())
    }

    // A patch applying only the `selected` lines (indices into `lines`) of this hunk.
    //
    // The patch is applied in `direction`: forward to stage, in reverse to unstage or discard.
    // Unselected changes must leave the target as is, so they become context on the side that is
    // in it (removed lines when staging, added lines when unstaging) and are dropped from the other.
    // `selected` is sorted.
    pub fn partial_patch(&self, selected: &[usize], direction: PatchDirection) -> String {
        let reverse = direction == PatchDirection::Reverse;
        let mut body = String::new();
        let (mut old_count, mut new_count) = (0, 0);
        // whether the previous line was kept, so its "\ No newline at end of file" marker is too
        let mut kept = true;

        for (i, line) in self.lines().enumerate() {
            let as_context = || format!(" {}", &line[1..]);
            match line.as_bytes().first() {
                Some(b'+') if selected.binary_search(&i).is_ok() => {
                    body.push_str(line);
                    new_count += 1;
                    kept = true;
                }
                Some(b'-') if selected.binary_search(&i).is_ok() => {
                    body.push_str(line);
                    old_count += 1;
                    kept = true;
                }
                Some(b'+') if reverse => {
                    body.push_str(&as_context());
                    old_count += 1;
                    new_count += 1;
                    kept = true;
                }
                Some(b'-') if !reverse => {
                    body.push_str(&as_context());
                    old_count += 1;
                    new_count += 1;
                    kept = true;
                }
                Some(b'+' | b'-') => kept = false,
                Some(b'\\') => {
                    if kept {
                        body.push_str(line);
                    }
                }
                _ => {
                    body.push_str(line);
                    old_count += 1;
                    new_count += 1;
                    kept = true;
                }
            }
        }

        // The first line of the hunk, the same on both sides since it is the only hunk of the patch.
        // An empty side is numbered by the line before it, as `git diff` does.
        let first_line = if self.old_count == 0 {
            self.old_start + 1
        } else {
            self.old_start
        };
        let start = |count: usize| {
            if count == 0 {
                first_line - 1
            } else {
                first_line
            }
        };
        let header = format!(
            "@@ -{},{old_count} +{},{new_count} @@\n",
            start(old_count),
            start(new_count)
        );
        let file_header = partial_file_header(self.file_header(), old_count, new_count);
        format!("{file_header}{header}{body}")
    }
}

// A partially applied patch of an added (or deleted) file keeps lines on the other side too,
// so the file is no longer created (or deleted) by it: turn the header into a modification.
// A renamed (or copied) file is patched under its new name alone: with the rename, git would
// look for the old file, or unstage the rename along with the hunk.
fn partial_file_header(file_header: &str, old_count: usize, new_count: usize) -> String {
    let path_of = |prefix: &str| {
        file_header
            .lines()
            .find_map(|l| l.strip_prefix(prefix))
            .unwrap_or_default()
            .to_string()
    };
    let new_path = path_of("+++ b/");
    let old_path = path_of("--- a/");
    let renamed = file_header
        .lines()
        .any(|l| l.starts_with("rename from ") || l.starts_with("copy from "));
    file_header
        .split_inclusive('\n')
        .filter_map(|line| {
            if renamed {
                const RENAME: [&str; 6] = [
                    "similarity index",
                    "dissimilarity index",
                    "rename from ",
                    "rename to ",
                    "copy from ",
                    "copy to ",
                ];
                if RENAME.iter().any(|p| line.starts_with(p)) {
                    return None;
                }
                if line.starts_with("diff --git ") {
                    return Some(format!("diff --git a/{new_path} b/{new_path}\n"));
                }
                if line.starts_with("--- a/") {
                    return Some(format!("--- a/{new_path}\n"));
                }
            }
            if (old_count > 0 && line.starts_with("new file mode"))
                || (new_count > 0 && line.starts_with("deleted file mode"))
            {
                None
            } else if old_count > 0 && line.starts_with("--- /dev/null") {
                Some(format!("--- a/{new_path}\n"))
            } else if new_count > 0 && line.starts_with("+++ /dev/null") {
                Some(format!("+++ b/{old_path}\n"))
            } else {
                Some(line.to_string())
            }
        })
        .collect()
}

// A move of the selection in a diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Next,
    Prev,
    First,
    Last,
}

impl Step {
    // The move of j and k, g and G.
    pub fn of(event: UserEvent) -> Option<Step> {
        match event {
            UserEvent::NavigateDown => Some(Step::Next),
            UserEvent::NavigateUp => Some(Step::Prev),
            UserEvent::GoToTop => Some(Step::First),
            UserEvent::GoToBottom => Some(Step::Last),
            _ => None,
        }
    }
}

// What is selected in a diff, to copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffPart {
    Hunk,
    Line,
}

impl DiffPart {
    // "copy hunk", "Applied the line"
    pub fn name(self) -> &'static str {
        match self {
            DiffPart::Hunk => "hunk",
            DiffPart::Line => "line",
        }
    }
}

// The screen rows the rows of a diff take, side by side (see `ParsedDiff::split_rows`) and wrapped,
// with a running total: the first screen row of each row shown, and one past the last. The scroll
// counts these, see `DiffCursor::scroll_to`.
#[derive(Debug, Default, Clone)]
pub struct WrapMap {
    width: u16,
    wrap: bool,
    layout: DiffLayout,
    // the rows side by side; None in one column, or when the diff has no rows to show so (a binary
    // file's), each row of the diff shown on its own
    split: Option<SplitMap>,
    // the rows shown, each its own screen row while not wrapping
    rows: usize,
    // prefix[i] = the screen row shown row i starts on, one longer than the rows, while wrapping;
    // empty else, as the rows are their own screen rows
    prefix: Vec<usize>,
}

// How the diffs are shown: in one column, or the old and the new side side by side.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum DiffLayout {
    #[default]
    Unified,
    Split,
}

impl DiffLayout {
    pub fn toggled(self) -> Self {
        match self {
            DiffLayout::Unified => DiffLayout::Split,
            DiffLayout::Split => DiffLayout::Unified,
        }
    }
}

// The rows of a diff side by side, see `ParsedDiff::split_rows`.
#[derive(Debug, Clone)]
struct SplitMap {
    rows: Vec<SplitRow>,
    // the row of `rows` each row of the diff is on
    placed: Vec<u32>,
}

// The running total of `heights`, from 0: where each row starts, and one past the last.
fn prefix_of(heights: impl Iterator<Item = usize>) -> Vec<usize> {
    let starts = heights.scan(0, |total, height| {
        *total += height;
        Some(*total)
    });
    std::iter::once(0).chain(starts).collect()
}

impl WrapMap {
    // The rows of `diff` at `width` as `layout` says, wrapped when `wrap`; built once per change of
    // them, so drawing still builds only the rows it shows.
    pub fn of(
        diff: &ParsedDiff,
        theme: &ColorTheme,
        width: u16,
        wrap: bool,
        layout: DiffLayout,
    ) -> Self {
        let split_rows = match layout {
            DiffLayout::Split => diff.split_rows(),
            DiffLayout::Unified => Vec::new(),
        };
        if split_rows.is_empty() {
            let rows = diff_line_count(diff);
            // made a few at a time, the whole diff of lines would take as much memory again
            const CHUNK: usize = 1024;
            let lines = || {
                (0..rows).step_by(CHUNK).flat_map(move |start| {
                    diff_lines(diff, theme, start..rows.min(start + CHUNK), width)
                })
            };
            return WrapMap {
                layout,
                ..Self::from_lines(rows, lines, width, wrap)
            };
        }
        let mut placed = vec![0; diff.rows.len()];
        for (at, split_row) in split_rows.iter().enumerate() {
            for row in split_row.rows() {
                placed[row] = row_u32(at);
            }
        }
        let mut prefix = vec![];
        if wrap {
            let (old_width, new_width) = split_widths(width);
            let height = |row: u32, gutter, width: u16| {
                let line = diff_line(diff, &diff.rows[row as usize], gutter, theme, width);
                wrapped_height(&line, width as usize)
            };
            prefix = prefix_of(split_rows.iter().map(|split_row| match *split_row {
                SplitRow::Whole(row) => height(row, Gutter::Both, width),
                SplitRow::Sides(old, new) => {
                    let old = old.map_or(1, |row| height(row, Gutter::Old, old_width));
                    let new = new.map_or(1, |row| height(row, Gutter::New, new_width));
                    old.max(new)
                }
            }));
        }
        WrapMap {
            width,
            wrap,
            layout,
            rows: split_rows.len(),
            split: Some(SplitMap {
                rows: split_rows,
                placed,
            }),
            prefix,
        }
    }

    // The screen rows the `rows` of `lines` wrap to at `width`, wrapped when `wrap`; the diff and
    // the file preview measure the same way. The lines are only made while wrapping: a big diff
    // would take a while, and as much memory again.
    pub fn from_lines<'a, L: IntoIterator<Item = Line<'a>>>(
        rows: usize,
        lines: impl FnOnce() -> L,
        width: u16,
        wrap: bool,
    ) -> Self {
        let mut prefix = vec![];
        if wrap {
            prefix = prefix_of(
                lines()
                    .into_iter()
                    .map(|line| wrapped_height(&line, width as usize)),
            );
        }
        WrapMap {
            width,
            wrap,
            rows,
            prefix,
            ..WrapMap::default()
        }
    }

    // Whether this is the map for `width`, `wrap` and `layout`.
    pub fn matches(&self, width: u16, wrap: bool, layout: DiffLayout) -> bool {
        self.width == width && self.wrap == wrap && self.layout == layout
    }

    // The rows side by side, None while they aren't.
    pub fn split_rows(&self) -> Option<&[SplitRow]> {
        self.split.as_ref().map(|split| split.rows.as_slice())
    }

    // The screen rows the diff takes.
    pub fn total(&self) -> usize {
        self.prefix.last().copied().unwrap_or(self.rows)
    }

    // The rows shown that rows of the diff are on, from the first to the last.
    fn place(&self, rows: Range<usize>) -> Range<usize> {
        let Some(split) = &self.split else {
            return rows;
        };
        let at = |row: usize| split.placed.get(row).map_or(self.rows, |&at| at as usize);
        if rows.is_empty() {
            return at(rows.start)..at(rows.start);
        }
        // a change shows its added rows beside its removed ones, so they are out of order
        let (first, last) = rows.map(at).fold((usize::MAX, 0), |(first, last), at| {
            (first.min(at), last.max(at))
        });
        first..last + 1
    }

    // The rows of the diff on rows shown `shown`, from the first to the last.
    fn unplace(&self, shown: Range<usize>) -> Range<usize> {
        let Some(split) = &self.split else {
            return shown;
        };
        let end = split.placed.len();
        let rows = || {
            split
                .rows
                .get(shown.clone())
                .unwrap_or_default()
                .iter()
                .flat_map(|split_row| split_row.rows())
        };
        match rows().min() {
            Some(first) => first..rows().max().map_or(first, |last| last + 1),
            None => {
                let at = split.rows.get(shown.start).and_then(|r| r.rows().next());
                let at = at.unwrap_or(end);
                at..at
            }
        }
    }

    // The first screen row of row shown `shown` (see `shown_rows`), the end of the diff past its
    // last.
    pub fn shown_start(&self, shown: usize) -> usize {
        if self.prefix.len() <= 1 {
            return shown;
        }
        self.prefix
            .get(shown)
            .copied()
            .unwrap_or_else(|| self.total())
    }

    // The screen rows logical `rows` take.
    pub fn visual_range(&self, rows: Range<usize>) -> Range<usize> {
        // no map yet: the rows are their own screen rows, so a selection made before the first
        // draw still shows
        let shown = self.place(rows);
        self.shown_start(shown.start)..self.shown_start(shown.end)
    }

    // The first screen row of logical row `row`, `row` itself when nothing has been measured
    // yet; the end of the diff past its last.
    pub fn visual_start(&self, row: usize) -> usize {
        self.visual_range(row..row).start
    }

    // The logical row of screen row `screen`, `screen` itself when nothing has been measured yet.
    pub fn logical_at(&self, screen: usize) -> usize {
        if self.prefix.len() <= 1 && self.split.is_none() {
            return screen;
        }
        self.logical_rows(screen..screen + 1).start
    }

    // The logical rows whose screen rows overlap `visual`; while split, from the first to the
    // last, which can hold rows of a change scrolled out beside them.
    pub fn logical_rows(&self, visual: Range<usize>) -> Range<usize> {
        self.unplace(self.shown_rows(visual))
    }

    // The rows shown (of the diff, or of `split_rows` while split) whose screen rows overlap
    // `visual`.
    pub fn shown_rows(&self, visual: Range<usize>) -> Range<usize> {
        if self.prefix.is_empty() {
            return visual.start.min(self.rows)..visual.end.min(self.rows);
        }
        let start = self
            .prefix
            .partition_point(|&p| p <= visual.start)
            .saturating_sub(1);
        let end = self
            .prefix
            .partition_point(|&p| p < visual.end)
            .min(self.prefix.len().saturating_sub(1));
        start..end
    }
}

// The widths of the old and the new side of a diff `width` wide, a divider between them.
pub fn split_widths(width: u16) -> (u16, u16) {
    let sides = width.saturating_sub(1);
    (sides / 2, sides - sides / 2)
}

// Whether the row of `diff` at `row` is one a search matches.
// Whether the text of `row` matches, as it is in the file, like a file preview's lines.
pub fn row_matches(diff: &ParsedDiff, row: usize, query: &str, options: SearchOptions) -> bool {
    match_range(&diff.raw[diff.rows[row].text.clone()], query, options).is_some()
}

// The selected hunk, or changed line, of a diff and its scroll offset; the local changes and the
// commit changes select the same way.
#[derive(Debug, Default, Clone)]
pub struct DiffCursor {
    pub hunk: usize,
    // the row of `ParsedDiff::rows` the keys are on: a change of the hunk, or the row a search
    // matched in it, which the next search goes on from
    pub line: usize,
    // where a visual selection of lines started, the selected line moving from there, like the
    // lines of a file preview
    visual: VisualSelection,
    // the rows picked, kept once the visual selection ends until cleared, all in the selected hunk
    picked: BTreeSet<usize>,
    pub scroll: ScrollState,
    // the screen rows the diff's rows wrap to, for the scroll to count
    wrap: WrapMap,
}

impl DiffCursor {
    // Back to the top of another diff.
    pub fn reset(&mut self) {
        let height = self.scroll.height;
        *self = DiffCursor::default();
        self.scroll.height = height;
    }

    // Measures the scroll against the rows `map` wraps to, see `WrapMap`.
    pub fn set_wrap_map(&mut self, map: WrapMap) {
        self.wrap = map;
    }

    // The wrapped rows the scroll counts against, see `WrapMap`.
    pub fn wrap(&self) -> &WrapMap {
        &self.wrap
    }

    // Selects a hunk, the last one past the end, showing it; false when there is none.
    pub fn select_hunk(&mut self, diff: &ParsedDiff, index: usize) -> bool {
        let Some(last) = diff.hunks.len().checked_sub(1) else {
            return false;
        };
        self.hunk = index.min(last);
        self.cancel_range();
        let rows = &diff.hunks[self.hunk].rows;
        // the row a search goes on from: the first change of the hunk
        self.line = rows
            .clone()
            .find(|&row| diff.rows[row].is_change())
            .unwrap_or(rows.start);
        // show the whole hunk, or at least its beginning
        self.scroll_to(rows.clone());
        true
    }

    // Selects a row, in the hunk containing it, extending the visual selection to it.
    pub fn select_line(&mut self, diff: &ParsedDiff, row: usize) {
        self.line = row;
        if let Some(rows) = self.visual.range(row, usize::MAX) {
            // side by side, the lines picked are those of the selected line's side
            let side = self.side(diff);
            self.picked
                .extend(rows.filter(|&r| side.is_none_or(|kind| diff.rows[r].kind == kind)));
        }
        if let Some(index) = diff.hunks.iter().position(|h| h.rows.contains(&row)) {
            self.hunk = index;
        }
        self.scroll_to(row..row + 1);
    }

    // Selects the change of the selected hunk the cursor is on (a click puts it there, see
    // `DiffPane::point`), else its first change; false when it has none.
    pub fn enter_lines(&mut self, diff: &ParsedDiff) -> bool {
        let Some(hunk) = diff.hunks.get(self.hunk) else {
            return false;
        };
        let on_change = hunk.rows.contains(&self.line) && diff.rows[self.line].is_change();
        if on_change {
            self.select_line(diff, self.line);
            return true;
        }
        match hunk.rows.clone().find(|&r| diff.rows[r].is_change()) {
            Some(row) => {
                self.select_line(diff, row);
                true
            }
            None => false,
        }
    }

    // Selects the row matching `query` in `direction` from the row `from`: the row itself when it
    // is a change, else its hunk. Returns which of them it selected, None when nothing matches.
    pub fn search(
        &mut self,
        diff: &ParsedDiff,
        query: &str,
        from: usize,
        direction: SearchDirection,
        options: SearchOptions,
    ) -> Option<DiffPart> {
        let row = find_around(diff.rows.len(), from, direction, |r| {
            row_matches(diff, r, query, options)
        })?;
        // the row the next search goes on from, in the hunk it is in; the lines picked stay, but
        // in their own hunk only, which staging them takes them from
        self.line = row;
        if let Some(index) = diff.hunks.iter().position(|h| h.rows.contains(&row)) {
            if index != self.hunk {
                self.cancel_range();
            }
            self.hunk = index;
        }
        self.scroll_to(row..row + 1);
        Some(if diff.rows[row].is_change() {
            DiffPart::Line
        } else {
            DiffPart::Hunk
        })
    }

    // The row a search goes from: the one the keys are on.
    pub fn selected_row(&self) -> usize {
        self.line
    }

    pub fn move_hunk(&mut self, diff: &ParsedDiff, step: Step) {
        let index = match step {
            Step::Next => self.hunk + 1,
            Step::Prev => self.hunk.saturating_sub(1),
            Step::First => 0,
            Step::Last => usize::MAX,
        };
        self.select_hunk(diff, index);
    }

    // Moves between the changed lines, across hunks.
    pub fn move_line(&mut self, diff: &ParsedDiff, step: Step) {
        let mut changed = self.selectable_rows(diff);
        let row = match step {
            Step::Next => changed.find(|&r| r > self.line),
            Step::Prev => changed.rfind(|&r| r < self.line),
            Step::First => changed.next(),
            Step::Last => changed.last(),
        };
        if let Some(row) = row {
            self.select_line(diff, row);
        }
    }

    // The page keys on the selected hunk or line: the window moves half a page or a page, and the
    // selection moves with it. A line only comes to rest on a changed line, the next one from the
    // screen row the page reached, else the last. A hunk taller than the page is read a page at a
    // time; else the next or previous hunk is selected. False for other events.
    pub fn jump_page(&mut self, diff: &ParsedDiff, event: UserEvent, part: DiffPart) -> bool {
        let down = matches!(event, UserEvent::HalfPageDown | UserEvent::PageDown);
        let rows = match event {
            UserEvent::HalfPageDown | UserEvent::HalfPageUp => self.scroll.height / 2,
            UserEvent::PageDown | UserEvent::PageUp => self.scroll.height,
            _ => return false,
        };
        let rows = rows.max(1);
        let unit = match part {
            DiffPart::Line => self.line..self.line + 1,
            DiffPart::Hunk => diff.hunks.get(self.hunk).map_or(0..0, |h| h.rows.clone()),
        };
        let screen = self.wrap.visual_range(unit);
        // where the unit shows in the view, the page moving from there
        let bottom = self.scroll.offset + self.scroll.height.saturating_sub(1);
        let anchor = screen.start.clamp(self.scroll.offset, bottom);
        let target = if down {
            anchor.saturating_add(rows)
        } else {
            anchor.saturating_sub(rows)
        };
        // the page is still inside the unit: scroll it, keeping the selection
        let inside = if down {
            target < screen.end
        } else {
            target >= screen.start
        };
        if inside {
            let moved = if down {
                rows as isize
            } else {
                -(rows as isize)
            };
            self.scroll.offset = self.scroll.offset.saturating_add_signed(moved);
            return true;
        }
        // past its end: the next or previous unit from the screen row the page reached, or the
        // edge of the diff when there is none further and the selection is already on it
        let row = self.wrap.logical_at(target);
        match part {
            DiffPart::Line => {
                // by screen rows: side by side, the added lines of a change are beside its
                // removed ones, after them in the diff
                let screen = |r: usize| self.wrap.visual_range(r..r + 1);
                let mut changed = self.selectable_rows(diff);
                let found = if down {
                    changed.find(|&r| screen(r).end > target)
                } else {
                    changed.rfind(|&r| screen(r).start <= target)
                };
                let found = found.or_else(|| {
                    if down {
                        self.selectable_rows(diff).last()
                    } else {
                        self.selectable_rows(diff).next()
                    }
                });
                match found {
                    Some(row) if row != self.line => {
                        self.select_line(diff, row);
                    }
                    _ => self.scroll_to_edge(down),
                }
            }
            DiffPart::Hunk => {
                let found = if down {
                    diff.hunks.iter().position(|h| h.rows.start >= row)
                } else {
                    diff.hunks.iter().rposition(|h| h.rows.start <= row)
                };
                let found = found.or_else(|| {
                    if down {
                        diff.hunks.len().checked_sub(1)
                    } else {
                        (!diff.hunks.is_empty()).then_some(0)
                    }
                });
                match found {
                    Some(index) if index != self.hunk => {
                        self.select_hunk(diff, index);
                    }
                    _ => self.scroll_to_edge(down),
                }
            }
        }
        true
    }

    // After ctrl-e or ctrl-y scrolled the window: the selected line pulled along to the nearest
    // changed line in it, like vim's; the selected hunk once it left the window whole, to the
    // nearest hunk in it. The window stays where it scrolled to.
    pub fn keep_in_view(&mut self, diff: &ParsedDiff, part: DiffPart) {
        let height = self.scroll.height;
        self.scroll.offset = self
            .scroll
            .offset
            .min(self.wrap.total().saturating_sub(height.max(1)));
        let offset = self.scroll.offset;
        let window = self.wrap.logical_rows(offset..offset + height);
        if window.is_empty() {
            return;
        }
        let crosses = |rows: &Range<usize>| rows.start < window.end && window.start < rows.end;
        // while split, the window's rows can hold rows of a change scrolled out beside them
        let shows = |row: usize| {
            let rows = self.wrap.visual_range(row..row + 1);
            rows.start < offset + height && offset < rows.end
        };
        match part {
            DiffPart::Line => {
                if shows(self.line) {
                    return;
                }
                let mut rows = self.selectable_rows(diff).filter(|r| window.contains(r));
                let above = self.wrap.visual_range(self.line..self.line + 1).start < offset;
                let row = match above {
                    true => rows.find(|&r| shows(r)),
                    false => rows.rfind(|&r| shows(r)),
                };
                if let Some(row) = row {
                    self.select_line(diff, row);
                }
            }
            DiffPart::Hunk => {
                let Some(hunk) = diff.hunks.get(self.hunk) else {
                    return;
                };
                if crosses(&hunk.rows) {
                    return;
                }
                let index = match hunk.rows.end <= window.start {
                    true => diff.hunks.iter().position(|h| crosses(&h.rows)),
                    false => diff.hunks.iter().rposition(|h| crosses(&h.rows)),
                };
                if let Some(index) = index {
                    self.select_hunk(diff, index);
                }
            }
        }
        self.scroll.offset = offset;
    }

    // The changed rows the selected line can move to: those of the selected hunk while lines are
    // picked, which stay in one hunk, else all; side by side, those on its side.
    fn selectable_rows<'d>(
        &self,
        diff: &'d ParsedDiff,
    ) -> impl DoubleEndedIterator<Item = usize> + 'd {
        let hunk = self
            .has_range()
            .then(|| diff.hunks.get(self.hunk))
            .flatten()
            .map(|hunk| hunk.rows.clone());
        let side = self.side(diff);
        changed_rows(diff).filter(move |&row| {
            hunk.as_ref().is_none_or(|rows| rows.contains(&row))
                && side.is_none_or(|kind| diff.rows[row].kind == kind)
        })
    }

    // The side of the selected changed line while the diff is side by side: removed on the old
    // side, added on the new one.
    fn side(&self, diff: &ParsedDiff) -> Option<DiffRowKind> {
        let row = diff.rows.get(self.line).filter(|row| row.is_change())?;
        self.wrap.split_rows().map(|_| row.kind)
    }

    // The changed line of the selected hunk on the side of `kind` (removed: the old side, added:
    // the new one) nearest on screen to the selected line, the one beside it first, then below
    // before above, while side by side and on the other side; none while lines are picked, which
    // stay on one side.
    pub fn other_side(&self, diff: &ParsedDiff, kind: DiffRowKind) -> Option<usize> {
        if self.has_range() || self.side(diff)? == kind {
            return None;
        }
        let split = self.wrap.split.as_ref()?;
        let hunk = diff.hunks.get(self.hunk)?;
        // quicker than walking the rows shown, in a hunk all on one side, like a new file's
        if !hunk.rows.clone().any(|row| diff.rows[row].kind == kind) {
            return None;
        }
        let at = *split.placed.get(self.line)? as usize;
        // the rows shown of the hunk, which are together, outwards from the selected line's
        let in_hunk = |shown: usize| {
            let split_row = *split.rows.get(shown)?;
            split_row
                .rows()
                .all(|row| hunk.rows.contains(&row))
                .then_some(split_row)
        };
        let on_side = |split_row: SplitRow| {
            let SplitRow::Sides(old, new) = split_row else {
                return None;
            };
            let row = match kind {
                DiffRowKind::Removed => old,
                _ => new,
            };
            row.map(|row| row as usize)
                .filter(|&row| diff.rows[row].kind == kind)
        };
        let mut below = (at..).map_while(in_hunk);
        let mut above = (0..at).rev().map_while(in_hunk);
        loop {
            let (b, a) = (below.next(), above.next());
            if b.is_none() && a.is_none() {
                return None;
            }
            if let Some(row) = b.into_iter().chain(a).find_map(on_side) {
                return Some(row);
            }
        }
    }

    // Starts a visual selection of lines at the selected line, or ends it, keeping the lines
    // picked; another one adds to them, like the lines of a file preview.
    pub fn toggle_range(&mut self) {
        if self.visual.toggle(self.line) {
            self.picked.insert(self.line);
        }
    }

    // Clears the lines picked; false when there were none.
    pub fn cancel_range(&mut self) -> bool {
        self.visual.cancel();
        !std::mem::take(&mut self.picked).is_empty()
    }

    // Before the mouse selects `row`, or no row on empty space, see `OnPicks`.
    pub fn prepare_point(&mut self, row: Option<usize>, picks: OnPicks) {
        let picked = row.is_some_and(|row| self.picked.contains(&row));
        if picks == OnPicks::ClearUnlessPicked && !picked {
            self.cancel_range();
        }
    }

    // Whether lines are picked, while the visual selection or after it ended.
    pub fn has_range(&self) -> bool {
        !self.picked.is_empty()
    }

    pub fn is_visual(&self) -> bool {
        self.visual.is_visual()
    }

    // The rows of the lines picked, else of the selected line, sorted.
    fn line_rows(&self) -> Vec<usize> {
        match self.picked.is_empty() {
            true => vec![self.line],
            false => self.picked.iter().copied().collect(),
        }
    }

    // Scrolls to the edge of the diff the page was going to, the selection kept where it is.
    fn scroll_to_edge(&mut self, down: bool) {
        self.scroll.offset = if down {
            self.wrap.total().saturating_sub(self.scroll.height)
        } else {
            0
        };
    }

    // Selects the changed line at or after `row`, else the last one; for a diff reloaded after
    // the line was staged.
    pub fn select_line_near(&mut self, diff: &ParsedDiff, row: usize) -> bool {
        self.cancel_range();
        let row = changed_rows(diff)
            .find(|&r| r >= row)
            .or_else(|| changed_rows(diff).last());
        if let Some(row) = row {
            self.select_line(diff, row);
        }
        row.is_some()
    }

    // Scrolls as little as can be to show `rows` with `scrolloff` rows around them, see
    // `style::scroll_to`; `rows` are logical, counted as screen rows while wrapping.
    pub fn scroll_to(&mut self, rows: Range<usize>) {
        let ScrollState {
            offset,
            height,
            scrolloff,
        } = self.scroll;
        let rows = self.wrap.visual_range(rows);
        // the rows aren't counted here: rendering keeps the offset within them, see `fit`
        self.scroll.offset = scroll_to(offset, rows, height, usize::MAX, scrolloff);
    }

    // The rows highlighted: from the first line picked to the last, else the selected line, or
    // the selected hunk.
    pub fn highlighted(&self, diff: &ParsedDiff, part: DiffPart) -> Range<usize> {
        match part {
            DiffPart::Line => {
                let rows = self.line_rows();
                rows[0]..rows[rows.len() - 1] + 1
            }
            DiffPart::Hunk => diff.hunks.get(self.hunk).map_or(0..0, |h| h.rows.clone()),
        }
    }

    // Whether `row` is drawn selected: the selected line, or the changed lines of the range of
    // lines or of the selected hunk, whose context stays as it is.
    pub fn is_highlighted(&self, diff: &ParsedDiff, part: DiffPart, row: usize) -> bool {
        match part {
            DiffPart::Line => {
                row == self.line || (self.picked.contains(&row) && diff.rows[row].is_change())
            }
            DiffPart::Hunk => diff
                .hunks
                .get(self.hunk)
                .is_some_and(|hunk| hunk.rows.contains(&row) && diff.rows[row].is_change()),
        }
    }

    pub fn selected_hunk<'d>(&self, diff: &'d ParsedDiff) -> Option<&'d DiffHunk> {
        diff.hunks.get(self.hunk)
    }

    // A patch of the selected line or range of lines alone, see `DiffHunk::partial_patch`.
    pub fn line_patch(&self, diff: &ParsedDiff, direction: PatchDirection) -> Option<String> {
        let hunk = self.selected_hunk(diff)?;
        // the hunk header occupies the first row, its lines follow
        let first = hunk.rows.start + 1;
        let lines: Option<Vec<usize>> = self
            .line_rows()
            .iter()
            .map(|row| row.checked_sub(first))
            .collect();
        Some(hunk.partial_patch(&lines?, direction))
    }

    // The text of the lines picked, else of the selected line, as in the file, without their +/-
    // and line endings, one per line.
    pub fn line_text(&self, diff: &ParsedDiff) -> Option<String> {
        let hunk = self.selected_hunk(diff)?;
        let first = hunk.rows.start + 1;
        let rows = self.line_rows();
        let lines: Vec<&str> = hunk
            .lines()
            .enumerate()
            .filter(|(i, _)| rows.binary_search(&(first + i)).is_ok())
            .map(|(_, line)| line)
            // "\ No newline at end of file" isn't a line of the file
            .filter(|line| !line.starts_with('\\'))
            .filter_map(|line| Some(line.get(1..)?.trim_end_matches(['\n', '\r'])))
            .collect();
        (!lines.is_empty()).then(|| lines.join("\n"))
    }

    // The selected line as in the file, or the selected hunk as a diff, to copy.
    pub fn selected_text(&self, diff: &ParsedDiff, part: DiffPart) -> Option<String> {
        match part {
            DiffPart::Line => self.line_text(diff),
            DiffPart::Hunk => self.selected_hunk(diff).map(DiffHunk::text),
        }
    }

    // Back from selecting lines to selecting the hunk of the selected line, which may not be the
    // one selected before.
    pub fn leave_lines(&mut self, diff: &ParsedDiff) {
        self.select_hunk(diff, self.hunk);
    }
}

fn changed_rows(diff: &ParsedDiff) -> impl DoubleEndedIterator<Item = usize> + '_ {
    diff.rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.is_change())
        .map(|(i, _)| i)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffRowKind {
    File,
    Hunk,
    Context,
    Added,
    Removed,
    Meta,
}

// A row of a diff, as small as it can be: a big diff has millions, see `ParsedDiff::row_text`.
#[derive(Debug, PartialEq, Eq)]
pub struct DiffRow {
    pub kind: DiffRowKind,
    // lines count from 1, so none takes no more room than a number
    pub old: Option<NonZeroU32>,
    pub new: Option<NonZeroU32>,
    // the bytes of the diff showing as its text, without the mark of the change
    text: Range<usize>,
    // where its ranges are in `ParsedDiff::emphasis`, see `ParsedDiff::row_emphasis`
    emphasis: Range<u32>,
}

impl DiffRow {
    pub fn is_change(&self) -> bool {
        matches!(self.kind, DiffRowKind::Added | DiffRowKind::Removed)
    }

    fn without_numbers(kind: DiffRowKind, text: Range<usize>) -> Self {
        Self {
            kind,
            old: None,
            new: None,
            text,
            emphasis: 0..0,
        }
    }
}

impl ParsedDiff {
    // The text of `row` as shown, its tabs as four spaces.
    pub fn row_text(&self, row: &DiffRow) -> std::borrow::Cow<'_, str> {
        shown(&self.raw[row.text.clone()])
    }

    // The bytes of the text of `row`, as shown, that differ from the line it replaces, or that
    // replaces it.
    pub fn row_emphasis(&self, row: &DiffRow) -> &[Range<usize>] {
        &self.emphasis[row.emphasis.start as usize..row.emphasis.end as usize]
    }
}

// A screen row of a diff shown side by side, see `ParsedDiff::split_rows`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitRow {
    // across both sides: a file, a hunk header or a note
    Whole(u32),
    // the old side and the new side: a line on both, or a removed line beside the added one
    // replacing it, either side empty
    Sides(Option<u32>, Option<u32>),
}

impl SplitRow {
    // The rows of the diff shown on it.
    fn rows(self) -> impl Iterator<Item = usize> {
        let (a, b) = match self {
            SplitRow::Whole(row) => (Some(row), None),
            SplitRow::Sides(old, new) => (old, new),
        };
        a.into_iter().chain(b).map(|row| row as usize)
    }
}

// A row of a diff, or of its rows side by side, as kept by `SplitRow` and `WrapMap`, half the size
// of a usize: the rows of a diff of over 4G of them wouldn't fit in memory anyway.
fn row_u32(row: usize) -> u32 {
    row as u32
}

impl ParsedDiff {
    // The rows side by side: the lines of a change on their side, a removed one beside the added
    // one it is paired with, the others on a screen row of their own where they are met, like
    // delta's; a "\ No newline" note on the side of the line it follows. Empty without rows.
    pub fn split_rows(&self) -> Vec<SplitRow> {
        use DiffRowKind::*;
        let mut split = Vec::new();
        let mut i = 0;
        while i < self.rows.len() {
            let at = row_u32(i);
            match self.rows[i].kind {
                Context => split.push(SplitRow::Sides(Some(at), Some(at))),
                Added | Removed => {}
                File | Hunk | Meta => split.push(SplitRow::Whole(at)),
            }
            if !self.rows[i].is_change() {
                i += 1;
                continue;
            }
            // the change from `i`, each side's rows in order
            let (mut old, mut new) = (Vec::new(), Vec::new());
            let mut on_new = false;
            while let Some(row) = self.rows.get(i) {
                match row.kind {
                    Removed => on_new = false,
                    Added => on_new = true,
                    Meta => {}
                    _ => break,
                }
                match on_new {
                    true => new.push(i),
                    false => old.push(i),
                }
                i += 1;
            }
            // an added row paired with none
            let alone = |&a: &usize| SplitRow::Sides(None, Some(row_u32(a)));
            // the first added row not shown yet
            let mut next = 0;
            for &row in &old {
                let pair = self.pairs.binary_search_by_key(&row, |p| p.0).ok();
                let skip =
                    pair.and_then(|k| new[next..].iter().position(|&a| a == self.pairs[k].1));
                let Some(skip) = skip else {
                    split.push(SplitRow::Sides(Some(row_u32(row)), None));
                    continue;
                };
                let added = &new[next..=next + skip];
                split.extend(added[..skip].iter().map(alone));
                split.push(SplitRow::Sides(
                    Some(row_u32(row)),
                    Some(row_u32(added[skip])),
                ));
                next += skip + 1;
            }
            split.extend(new[next..].iter().map(alone));
        }
        split
    }
}

// `text` as shown, its tabs as four spaces.
fn shown(text: &str) -> Cow<'_, str> {
    tabs_shown(text)
}

pub fn parse_diff(diff: &str) -> ParsedDiff {
    let shared: Arc<str> = Arc::from(diff);
    // file names are only shown when the diff spans multiple files (a directory)
    let multiple_files_count = diff
        .lines()
        .filter(|l| l.starts_with("diff --git "))
        .count();
    let multiple_files = multiple_files_count > 1;

    let mut rows = Vec::new();
    let mut hunks = Vec::new();
    // where the header of the current file starts, it goes on up to its first hunk
    let mut file_start = 0;
    // the hunk being read, its `rows` end is set once it is finished
    let mut current: Option<DiffHunk> = None;
    let (mut old, mut new) = (0, 0);

    let finish_hunk =
        |current: &mut Option<DiffHunk>, hunks: &mut Vec<DiffHunk>, rows_len: usize| {
            if let Some(mut hunk) = current.take() {
                hunk.rows.end = rows_len;
                hunks.push(hunk);
            }
        };

    // keep the original line endings, the patch has to match the file byte for byte
    let mut end = 0;
    for raw in diff.split_inclusive('\n') {
        let start = end;
        end += raw.len();
        let line = raw.trim_end_matches(['\n', '\r']);
        // the bytes of the diff of `line` from `from`
        let bytes = |from: usize| start + from..start + line.len();

        if line.starts_with("diff --git ") {
            finish_hunk(&mut current, &mut hunks, rows.len());
            file_start = start;
            if multiple_files {
                let path = new_path_start(line);
                rows.push(DiffRow::without_numbers(DiffRowKind::File, bytes(path)));
            }
            continue;
        }
        if line.starts_with("@@") {
            if let Some((o, old_count, n)) = parse_hunk_header(line) {
                finish_hunk(&mut current, &mut hunks, rows.len());
                (old, new) = (o, n);
                // a hunk after another ends its file's header where the first one starts
                let file_end = hunks
                    .last()
                    .filter(|h: &&DiffHunk| h.file_header.start == file_start)
                    .map_or(start, |h| h.file_header.end);
                current = Some(DiffHunk {
                    rows: rows.len()..rows.len(),
                    diff: shared.clone(),
                    file_header: file_start..file_end,
                    header: start..end,
                    old_start: o,
                    old_count,
                    body: end..end,
                });
                rows.push(DiffRow::without_numbers(DiffRowKind::Hunk, bytes(0)));
                continue;
            }
        }
        let Some(hunk) = current.as_mut() else {
            // part of the file header
            continue;
        };
        hunk.body.end = end;

        let text = bytes(line.len().min(1));
        // a line number beyond u32 shows none, a file of billions of lines being no text
        let number = |n: usize| u32::try_from(n).ok().and_then(NonZeroU32::new);
        let row = match line.as_bytes().first() {
            Some(b'+') => {
                new += 1;
                DiffRow {
                    kind: DiffRowKind::Added,
                    old: None,
                    new: number(new - 1),
                    text,
                    emphasis: 0..0,
                }
            }
            Some(b'-') => {
                old += 1;
                DiffRow {
                    kind: DiffRowKind::Removed,
                    old: number(old - 1),
                    new: None,
                    text,
                    emphasis: 0..0,
                }
            }
            Some(b'\\') => DiffRow::without_numbers(DiffRowKind::Meta, bytes(0)),
            _ => {
                old += 1;
                new += 1;
                DiffRow {
                    kind: DiffRowKind::Context,
                    old: number(old - 1),
                    new: number(new - 1),
                    text,
                    emphasis: 0..0,
                }
            }
        };
        rows.push(row);
    }
    finish_hunk(&mut current, &mut hunks, rows.len());
    let (emphasis, pairs) = pair_changes(&mut rows, &shared);
    let max_line = rows
        .iter()
        .flat_map(|r| [r.old, r.new])
        .flatten()
        .max()
        .map_or(0, NonZeroU32::get);

    ParsedDiff {
        raw: shared,
        rows,
        emphasis,
        pairs,
        hunks,
        converted: false,
        files: multiple_files_count,
        number_width: max_line.to_string().len().max(3),
    }
}

// Pairs each removed line of a change with the first added line after the last one paired that is
// alike enough (see `alike`), like delta does, and marks in each pair the words that differ between
// them, in the text as shown, the bytes of `diff`. Returns the marks and the pairs.
fn pair_changes(rows: &mut [DiffRow], diff: &str) -> (Vec<Range<usize>>, Vec<(usize, usize)>) {
    // shortcut: an added line more than this many past the last one paired isn't tried, as each
    // try diffs two lines; raise it when lines further apart should pair
    const LOOKAHEAD: usize = 16;
    let mut emphasis = Vec::new();
    let mut pairs = Vec::new();
    // the rows of `kind` from `from` on, past "\ No newline" notes, and where they end
    let run = |rows: &[DiffRow], from: usize, kind: DiffRowKind| {
        let mut found = Vec::new();
        let mut end = from;
        while let Some(row) = rows.get(end) {
            if row.kind == kind {
                found.push(end);
            } else if row.kind != DiffRowKind::Meta {
                break;
            }
            end += 1;
        }
        (found, end)
    };
    let mut i = 0;
    while i < rows.len() {
        let (removed, end) = run(rows, i, DiffRowKind::Removed);
        if removed.is_empty() {
            i += 1;
            continue;
        }
        let (added, end) = run(rows, end, DiffRowKind::Added);
        // the lines, removed then added, split into words when first compared: a big change
        // compares few of its lines
        let spans: Vec<_> = (removed.iter().chain(&added))
            .map(|&row| rows[row].text.clone())
            .collect();
        let texts: Vec<OnceCell<Cow<str>>> = spans.iter().map(|_| OnceCell::new()).collect();
        let words: Vec<OnceCell<Words>> = spans.iter().map(|_| OnceCell::new()).collect();
        let line = |k: usize| {
            words[k]
                .get_or_init(|| Words::of(texts[k].get_or_init(|| shown(&diff[spans[k].clone()]))))
        };
        // the first added line not passed yet
        let mut next = 0;
        for (old, &r) in removed.iter().enumerate() {
            let found = (next..added.len()).take(LOOKAHEAD).find_map(|new| {
                let (old, new_line) = (line(old), line(removed.len() + new));
                let ranges = changed_ranges(old, new_line)?;
                Some((new, ranges))
            });
            let Some((new, [old, new_ranges])) = found else {
                continue;
            };
            let a = added[new];
            next = new + 1;
            pairs.push((r, a));
            // a diff with more than u32 ranges, of over 4GB, shows the rest without them
            let mut add = |ranges: Vec<_>| {
                let start = u32::try_from(emphasis.len()).ok()?;
                emphasis.extend(ranges);
                let end = u32::try_from(emphasis.len()).ok()?;
                Some(start..end)
            };
            if let (Some(old), Some(new)) = (add(old), add(new_ranges)) {
                (rows[r].emphasis, rows[a].emphasis) = (old, new);
            }
        }
        i = end;
    }
    (emphasis, pairs)
}

// The words of a line as shown (see `words`), sorted past blanks too, and the length of its text
// past blanks.
struct Words<'a> {
    words: Vec<&'a str>,
    sorted: Vec<&'a str>,
    text: usize,
}

impl<'a> Words<'a> {
    fn of(line: &'a str) -> Self {
        let words = words(line);
        let mut sorted: Vec<&str> = (words.iter().map(|word| word.trim()))
            .filter(|word| !word.is_empty())
            .collect();
        sorted.sort_unstable();
        let text = sorted.iter().map(|word| word.len()).sum();
        Words {
            words,
            sorted,
            text,
        }
    }
}

// Whether two lines keeping `kept` of their text past blanks are alike enough to pair and mark:
// delta's test, of a distance (changed / (changed + 2 kept)) of at most 0.6.
fn alike(kept: usize, old: &Words, new: &Words) -> bool {
    5 * kept >= old.text + new.text
}

// The most text two lines can keep, past blanks: that of the words both have, each as often as
// both do, merged from their sorted words.
fn common_text(old: &Words, new: &Words) -> usize {
    let (old, new) = (&old.sorted, &new.sorted);
    let (mut i, mut j, mut kept) = (0, 0, 0);
    while let (Some(o), Some(n)) = (old.get(i), new.get(j)) {
        match o.cmp(n) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                kept += o.len();
                (i, j) = (i + 1, j + 1);
            }
        }
    }
    kept
}

// The bytes of the words of `old` and `new` that differ, by a diff of their words, as long as it
// takes: the diff is parsed on another thread, see `DiffPane::load_in_background`. None when the
// lines aren't `alike`, or alike only in blanks.
fn changed_ranges(old: &Words, new: &Words) -> Option<[Vec<Range<usize>>; 2]> {
    // what they have in common bounds what the diff keeps, which is quicker to tell
    if !alike(common_text(old, new), old, new) {
        return None;
    }
    let ops = similar::capture_diff_slices(similar::Algorithm::Myers, &old.words, &new.words);
    let kept: usize = ops
        .iter()
        .map(|op| op.as_tag_tuple())
        .filter(|(tag, ..)| *tag == DiffTag::Equal)
        .map(|(_, words, _)| {
            old.words[words]
                .iter()
                .map(|w| w.trim().len())
                .sum::<usize>()
        })
        .sum();
    if !alike(kept, old, new) {
        return None;
    }
    // the byte where each word starts, and the end of the line
    let starts = |words: &[&str]| -> Vec<usize> {
        std::iter::once(0)
            .chain(words.iter().scan(0, |at, w| {
                *at += w.len();
                Some(*at)
            }))
            .collect()
    };
    let (old_starts, new_starts) = (starts(&old.words), starts(&new.words));
    let mut marks = (Vec::new(), Vec::new());
    for op in &ops {
        let (tag, o, n) = op.as_tag_tuple();
        if tag != DiffTag::Equal {
            push_range(&mut marks.0, old_starts[o.start]..old_starts[o.end]);
            push_range(&mut marks.1, new_starts[n.start]..new_starts[n.end]);
        }
    }
    Some([marks.0, marks.1])
}

// Adds `range` to `ranges`, joined to the last when they touch.
fn push_range(ranges: &mut Vec<Range<usize>>, range: Range<usize>) {
    match ranges.last_mut() {
        _ if range.is_empty() => {}
        Some(last) if last.end == range.start => last.end = range.end,
        _ => ranges.push(range),
    }
}

// The words of `line`: runs of letters, digits and "_", runs of blanks, and each other character.
fn words(line: &str) -> Vec<&str> {
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    };
    let mut words = Vec::new();
    let mut start = 0;
    let mut prev = None;
    for (i, c) in line.char_indices() {
        let kind = class(c);
        if i > 0 && (prev != Some(kind) || kind == 2) {
            words.push(&line[start..i]);
            start = i;
        }
        prev = Some(kind);
    }
    if !line.is_empty() {
        words.push(&line[start..]);
    }
    words
}

// Where the new path starts in "diff --git a/<old> b/<new>": after the middle " b/" when both are
// alike, which a path holding " b/" itself needs, else after the last " b/", a rename's.
fn new_path_start(line: &str) -> usize {
    let prefix = "diff --git a/".len();
    let rest = line.get(prefix..).unwrap_or_default();
    let half = rest.len().saturating_sub(3) / 2;
    let same = rest.get(half..half + 3) == Some(" b/")
        && rest
            .get(..half)
            .is_some_and(|old| rest.get(half + 3..) == Some(old));
    match same {
        true => prefix + half + 3,
        false => line.rfind(" b/").map_or(0, |i| i + 3),
    }
}

// "@@ -12,3 +12,4 @@ fn foo()" -> (12, 3, 12): old start, old count and new start
fn parse_hunk_header(line: &str) -> Option<(usize, usize, usize)> {
    let mut parts = line.split(' ').skip(1);
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let start = |s: &str| s.split(',').next()?.parse::<usize>().ok();
    // the count is omitted when it is 1
    let old_count = match old.split_once(',') {
        Some((_, count)) => count.parse::<usize>().ok()?,
        None => 1,
    };
    Some((start(old)?, old_count, start(new)?))
}

// The lines of a diff without hunks: binary files and mode-only changes.
fn hunkless_lines(diff: &ParsedDiff) -> impl Iterator<Item = &str> {
    diff.raw
        .lines()
        .filter(|l| !l.starts_with("diff --git") && !l.starts_with("index "))
}

// The number of lines `diff_lines` displays for `diff`.
pub fn diff_line_count(diff: &ParsedDiff) -> usize {
    if diff.raw.is_empty() {
        1
    } else if diff.rows.is_empty() {
        hunkless_lines(diff).count()
    } else {
        diff.rows.len()
    }
}

// The lines displaying `diff` in `range` (of `diff_line_count`), only those on screen:
// line numbers of both sides, then the colored text; the path of each file of a diff of several
// as a separator line across the `panel_width`.
pub fn diff_lines<'a>(
    diff: &'a ParsedDiff,
    theme: &ColorTheme,
    range: Range<usize>,
    panel_width: u16,
) -> Vec<Line<'a>> {
    if diff.raw.is_empty() {
        // e.g. a directory of untracked files, which `git diff` does not show
        return vec![placeholder("No diff to show", theme)];
    }
    if diff.rows.is_empty() {
        return hunkless_lines(diff)
            .skip(range.start)
            .take(range.len())
            .map(|l| Line::raw(l).fg(theme.muted_fg))
            .collect();
    }

    diff.rows
        .iter()
        .skip(range.start)
        .take(range.len())
        .map(|row| diff_line(diff, row, Gutter::Both, theme, panel_width))
        .collect()
}

// The line numbers a line of a diff shows: of both sides, or of the side it is drawn on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gutter {
    Both,
    Old,
    New,
}

// The line displaying `row` of `diff`, see `diff_lines`, its numbers as `gutter` says.
fn diff_line<'a>(
    diff: &'a ParsedDiff,
    row: &DiffRow,
    gutter: Gutter,
    theme: &ColorTheme,
    panel_width: u16,
) -> Line<'a> {
    let dim = theme.muted_fg;
    if row.kind == DiffRowKind::File {
        let path = format!("{} ", diff.row_text(row));
        let fill = (panel_width as usize).saturating_sub(3 + Span::raw(path.as_str()).width());
        return Line::from(vec![
            Span::raw("── ").fg(theme.divider_fg),
            Span::raw(path).fg(theme.detail_file_change_modify_fg),
            divider(fill, theme),
        ]);
    }
    let width = diff.number_width;
    let number = |n: Option<NonZeroU32>| n.map_or(" ".repeat(width), |n| format!("{n:>width$}"));
    let gutter = match gutter {
        Gutter::Both => format!("{} {} ", number(row.old), number(row.new)),
        Gutter::Old => format!("{} ", number(row.old)),
        Gutter::New => format!("{} ", number(row.new)),
    };
    let text_color = match row.kind {
        DiffRowKind::Hunk => theme.diff_hunk_fg,
        DiffRowKind::Added | DiffRowKind::Removed | DiffRowKind::Context => theme.fg,
        DiffRowKind::Meta | DiffRowKind::File => dim,
    };
    let mut line = Line::from(vec![
        Span::raw(gutter).fg(dim),
        Span::raw(diff.row_text(row)).fg(text_color),
    ]);
    // added and removed lines on a background across the panel, what changed in them on a
    // stronger one
    let (bg, word_bg) = match row.kind {
        DiffRowKind::Added => (theme.diff_add_bg, theme.diff_add_word_bg),
        DiffRowKind::Removed => (theme.diff_delete_bg, theme.diff_delete_word_bg),
        _ => return line,
    };
    for range in diff.row_emphasis(row) {
        patch_range(&mut line, range.clone(), Style::default().bg(word_bg));
    }
    line.bg(bg)
}

// Marks a line of `diff_lines` selected on its line numbers, keeping the colors of the change.
fn select_diff_line(line: &mut Line, theme: &ColorTheme) {
    if let Some(gutter) = line.spans.first_mut() {
        gutter.style = gutter
            .style
            .fg(theme.list_selected_fg)
            .bg(theme.list_selected_bg)
            .add_modifier(Modifier::BOLD);
    }
}

// Renders `lines` with their logical row, the screen rows `screen` of them, a line's background
// (added, removed) across `area`, the `selected` rows marked on their line numbers and by the bar
// left of `area`; long lines wrapped at the width of `area`, when `wrap`.
pub fn render_diff_lines<'a>(
    lines: impl Iterator<Item = (usize, Line<'a>)>,
    screen: Range<usize>,
    selected: impl Fn(usize) -> bool,
    wrap: bool,
    theme: &ColorTheme,
    area: Rect,
    buf: &mut Buffer,
) {
    let visual = wrapped_rows(
        lines.map(|(row, mut line)| {
            if selected(row) {
                select_diff_line(&mut line, theme);
            }
            (row, line)
        }),
        screen,
        wrap,
        area,
    );
    render_visual_rows(visual, selected, theme, area, buf);
}

// Draws the screen rows `screen` of `diff` side by side, see `ParsedDiff::split_rows`: the old
// side left of a divider, the new side right of it, a file or hunk header across both; otherwise
// like `render_diff_lines`, each line made changed by `restyle` (the matches of a search). Returns
// the rows of the diff on each screen row drawn, of the old side and of the new side.
#[expect(clippy::too_many_arguments)]
pub fn render_split_rows(
    diff: &ParsedDiff,
    map: &WrapMap,
    screen: Range<usize>,
    selected: impl Fn(usize) -> bool,
    restyle: impl Fn(&mut Line),
    wrap: bool,
    theme: &ColorTheme,
    area: Rect,
    buf: &mut Buffer,
) -> Vec<[Option<usize>; 2]> {
    let (old_width, new_width) = split_widths(area.width);
    let make = |row: usize, gutter, width: u16| -> Vec<Line<'static>> {
        let mut line = diff_line(diff, &diff.rows[row], gutter, theme, width);
        if selected(row) {
            select_diff_line(&mut line, theme);
        }
        restyle(&mut line);
        match wrap {
            true => wrap_line(line, width as usize),
            false => vec![owned(line)],
        }
    };
    // each screen row: a line across both sides, or the line of each side, with the rows of the
    // diff they show
    enum Drawn {
        Whole(usize, Line<'static>),
        Sides([Option<usize>; 2], [Option<Line<'static>>; 2]),
    }
    let shown = map.shown_rows(screen.clone());
    let skip = screen.start - map.shown_start(shown.start);
    let mut drawn = Vec::new();
    for split_row in &map.split_rows().unwrap_or_default()[shown] {
        match *split_row {
            SplitRow::Whole(row) => {
                let row = row as usize;
                let lines = make(row, Gutter::Both, area.width).into_iter();
                drawn.extend(lines.map(|line| Drawn::Whole(row, line)));
            }
            SplitRow::Sides(old, new) => {
                let (old, new) = (old.map(|row| row as usize), new.map(|row| row as usize));
                let side = |row: Option<usize>, gutter, width| {
                    row.map_or_else(Vec::new, |row| make(row, gutter, width))
                };
                let mut old_lines = side(old, Gutter::Old, old_width).into_iter();
                let mut new_lines = side(new, Gutter::New, new_width).into_iter();
                let height = old_lines.len().max(new_lines.len());
                drawn.extend(
                    (0..height)
                        .map(|_| Drawn::Sides([old, new], [old_lines.next(), new_lines.next()])),
                );
            }
        }
    }
    let mut hits = Vec::new();
    for (y, drawn) in (area.y..)
        .zip(drawn.into_iter().skip(skip))
        .take(screen.len())
    {
        let rows = match drawn {
            Drawn::Whole(row, line) => {
                draw_row(line, Rect::new(area.x, y, area.width, 1), theme, buf);
                [Some(row); 2]
            }
            Drawn::Sides(rows, [old_line, new_line]) => {
                if let Some(line) = old_line {
                    draw_row(line, Rect::new(area.x, y, old_width, 1), theme, buf);
                }
                let new_x = area.x.saturating_add(old_width).saturating_add(1);
                if let Some(line) = new_line {
                    draw_row(line, Rect::new(new_x, y, new_width, 1), theme, buf);
                }
                if old_width < area.width {
                    buf.set_string(
                        area.x + old_width,
                        y,
                        "│",
                        Style::default().fg(theme.divider_fg),
                    );
                }
                rows
            }
        };
        if rows.iter().flatten().any(|&row| selected(row)) {
            mark_row_selected(area, y, theme, buf);
        }
        hits.push(rows);
    }
    hits
}

// The screen rows `screen` of `lines` with their logical row, wrapped at the width of `area` when
// `wrap`; only the rows shown are built.
pub fn wrapped_rows<'a>(
    lines: impl Iterator<Item = (usize, Line<'a>)>,
    screen: Range<usize>,
    wrap: bool,
    area: Rect,
) -> Vec<(usize, Line<'static>)> {
    let width = area.width as usize;
    lines
        .flat_map(|(row, line)| {
            let wrapped = if wrap {
                wrap_line(line, width)
            } else {
                vec![owned(line)]
            };
            wrapped.into_iter().map(move |line| (row, line))
        })
        .skip(screen.start)
        .take(screen.len())
        .collect()
}

// Draws screen rows, with the bar left of `area` beside the `selected` ones.
pub fn render_visual_rows(
    visual: Vec<(usize, Line)>,
    selected: impl Fn(usize) -> bool,
    theme: &ColorTheme,
    area: Rect,
    buf: &mut Buffer,
) {
    for (y, (row, line)) in (area.y..area.bottom()).zip(visual) {
        draw_row(line, Rect::new(area.x, y, area.width, 1), theme, buf);
        if selected(row) {
            mark_row_selected(area, y, theme, buf);
        }
    }
}

// Draws a screen row of a diff or a file in `area`, one row high, its background (of a change, or
// the selection) across it.
fn draw_row(line: Line, area: Rect, theme: &ColorTheme, buf: &mut Buffer) {
    if let Some(bg) = line.style.bg {
        buf.set_style(area, Style::default().bg(bg));
    }
    Paragraph::new(line).fg(theme.fg).render(area, buf);
}

// Marks screen row `y` of `area` selected, by the bar left of it.
fn mark_row_selected(area: Rect, y: u16, theme: &ColorTheme, buf: &mut Buffer) {
    if area.x > 0 {
        mark_selected_bar(buf, area.x - 1, y, true, theme);
    }
}

// `line` in rows of `width` columns, the rows after the first indented past its first span, the
// line numbers, in their style, and keeping the line's style (the background of a change).
fn wrap_line(line: Line<'_>, width: usize) -> Vec<Line<'static>> {
    let Some(indent) = wrap_indent(&line, width) else {
        return vec![owned(line)];
    };
    let style = line.style;
    // the line numbers' style under them too, like the selection's
    let gutter_style = line.spans.first().map(|s| s.style).unwrap_or_default();
    let mut rows = Vec::new();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for span in line.spans {
        let mut run = String::new();
        for c in span.content.chars() {
            let w = c.width().unwrap_or(0);
            if breaks(used, w, width, indent) {
                if !run.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut run), span.style));
                }
                rows.push(Line::from(std::mem::take(&mut spans)).style(style));
                spans.push(Span::styled(" ".repeat(indent), gutter_style));
                used = indent;
            }
            run.push(c);
            used += w;
        }
        if !run.is_empty() {
            spans.push(Span::styled(run, span.style));
        }
    }
    rows.push(Line::from(spans).style(style));
    rows
}

// The columns of the line numbers, the first span, that the rows of `line` after its first are
// indented by once wrapped at `width`; None when there is no room past them to wrap.
fn wrap_indent(line: &Line, width: usize) -> Option<usize> {
    let indent = if line.spans.len() > 1 {
        line.spans[0].width()
    } else {
        0
    };
    (width > indent + 1).then_some(indent)
}

// Whether a character `w` wide starts a new row, `used` columns into one past its `indent`.
fn breaks(used: usize, w: usize, width: usize, indent: usize) -> bool {
    used + w > width && used > indent
}

// The rows `wrap_line` wraps `line` to, counted without making them.
fn wrapped_height(line: &Line, width: usize) -> usize {
    let Some(indent) = wrap_indent(line, width) else {
        return 1;
    };
    let (mut rows, mut used) = (1, 0);
    for c in line.spans.iter().flat_map(|span| span.content.chars()) {
        let w = c.width().unwrap_or(0);
        if breaks(used, w, width, indent) {
            rows += 1;
            used = indent;
        }
        used += w;
    }
    rows
}

fn owned(line: Line<'_>) -> Line<'static> {
    let spans: Vec<Span<'static>> = line
        .spans
        .into_iter()
        .map(|s| Span::styled(s.content.into_owned(), s.style))
        .collect();
    Line::from(spans).style(line.style)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_separators() {
        // a diff of several files: each one's path on a line across the panel
        let file = |name: &str| {
            format!(
                "diff --git a/{name} b/{name}\n--- a/{name}\n+++ b/{name}\n@@ -1 +1 @@\n-a\n+b\n"
            )
        };
        let diff = parse_diff(&format!("{}{}", file("x.rs"), file("y.rs")));
        let theme = ColorTheme::default();
        let lines = diff_lines(&diff, &theme, 0..diff.rows.len(), 30);
        let separators: Vec<&Line> = lines
            .iter()
            .filter(|l| l.to_string().starts_with("── "))
            .collect();
        assert_eq!(separators.len(), 2);
        for line in separators {
            assert_eq!(line.width(), 30, "{line}");
            assert!(line.to_string().ends_with('─'));
            assert_eq!(line.spans[0].style.fg, Some(theme.divider_fg));
        }
    }

    // A path holding " b/" used to be shown from its last " b/" on.
    #[test]
    fn test_new_path_start() {
        let name = |line: &str| line[new_path_start(line)..].to_string();
        assert_eq!(name("diff --git a/x b/y b/x b/y"), "x b/y");
        assert_eq!(name("diff --git a/src/a.rs b/src/a.rs"), "src/a.rs");
        assert_eq!(name("diff --git a/old.rs b/new.rs"), "new.rs");
        assert_eq!(name("diff --git a/é b/é"), "é");
    }

    #[test]
    fn test_parse_diff() {
        let header = "diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n";
        let hunk1 = "@@ -3,2 +3,2 @@ ctx\n keep\n-old\n+new\n";
        let hunk2 = "@@ -20 +20 @@\n-a\r\n+b\r\n";
        let actual = parse_diff(&format!("{header}{hunk1}{hunk2}"));

        let row = |kind, old: Option<u32>, new: Option<u32>, text: &str| {
            (
                kind,
                old.and_then(NonZeroU32::new),
                new.and_then(NonZeroU32::new),
                text.to_string(),
            )
        };
        let expected_rows = vec![
            row(DiffRowKind::Hunk, None, None, "@@ -3,2 +3,2 @@ ctx"),
            row(DiffRowKind::Context, Some(3), Some(3), "keep"),
            row(DiffRowKind::Removed, Some(4), None, "old"),
            row(DiffRowKind::Added, None, Some(4), "new"),
            row(DiffRowKind::Hunk, None, None, "@@ -20 +20 @@"),
            row(DiffRowKind::Removed, Some(20), None, "a"),
            row(DiffRowKind::Added, None, Some(20), "b"),
        ];
        let rows: Vec<_> = actual
            .rows
            .iter()
            .map(|r| (r.kind, r.old, r.new, actual.row_text(r).into_owned()))
            .collect();
        assert_eq!(rows, expected_rows);
        assert!(actual
            .rows
            .iter()
            .all(|r| actual.row_emphasis(r).is_empty() || r.is_change()));

        let hunks: Vec<(Range<usize>, String)> = actual
            .hunks
            .iter()
            .map(|h| (h.rows.clone(), h.patch()))
            .collect();
        let expected_hunks = vec![
            (0..4, format!("{header}{hunk1}")),
            (4..7, format!("{header}{hunk2}")),
        ];
        assert_eq!(hunks, expected_hunks);
        // two files: each hunk with its own file's header
        let other = "diff --git a/g b/g\n--- a/g\n+++ b/g\n";
        let hunk3 = "@@ -1 +1 @@\n-x\n+y\n";
        let diff = parse_diff(&format!("{header}{hunk1}{hunk2}{other}{hunk3}"));
        let patches: Vec<String> = diff.hunks.iter().map(DiffHunk::patch).collect();
        assert_eq!(
            patches,
            [
                format!("{header}{hunk1}"),
                format!("{header}{hunk2}"),
                format!("{other}{hunk3}")
            ]
        );
        assert_eq!(diff.hunks[2].text(), hunk3);
    }

    #[test]
    fn test_a_selected_hunk_highlights_only_its_changed_lines() {
        let diff = parse_diff(
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@\n ctx\n-old\n+new\n ctx\n",
        );
        let mut cursor = DiffCursor::default();
        assert!(cursor.select_hunk(&diff, 0));
        let selected: Vec<usize> = (0..diff.rows.len())
            .filter(|&row| cursor.is_highlighted(&diff, DiffPart::Hunk, row))
            .collect();
        // the hunk header and the context around the change stay unselected
        assert_eq!(selected, [2, 3]);
    }

    #[test]
    fn test_lines_picked_like_a_file_preview_stay_in_their_hunk() {
        let file_header = "diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n";
        let diff = parse_diff(&format!(
            "{file_header}@@ -1,3 +1,3 @@\n a\n-b\n-c\n+B\n+C\n@@ -10,2 +10,2 @@\n x\n-y\n+Y\n"
        ));
        let mut cursor = DiffCursor::default();
        assert!(cursor.select_hunk(&diff, 0));
        assert!(cursor.enter_lines(&diff));
        let highlighted = |cursor: &DiffCursor| {
            (0..diff.rows.len())
                .filter(|&row| cursor.is_highlighted(&diff, DiffPart::Line, row))
                .count()
        };
        // from "-b" to "-c", then v again ends it, keeping them picked
        cursor.toggle_range();
        cursor.move_line(&diff, Step::Next);
        cursor.toggle_range();
        assert!(!cursor.is_visual() && cursor.has_range());
        assert_eq!(cursor.line_text(&diff).as_deref(), Some("b\nc"));
        // moving keeps them and stops at the last change of their hunk
        for _ in 0..10 {
            cursor.move_line(&diff, Step::Next);
        }
        assert_eq!(cursor.hunk, 0);
        assert_eq!(cursor.line_text(&diff).as_deref(), Some("b\nc"));
        // another selection adds "+C": staging them leaves "+B" out
        cursor.toggle_range();
        cursor.toggle_range();
        assert_eq!(cursor.line_text(&diff).as_deref(), Some("b\nc\nC"));
        assert_eq!(highlighted(&cursor), 3);
        assert_eq!(
            cursor.line_patch(&diff, PatchDirection::Forward),
            Some(format!("{file_header}@@ -1,3 +1,2 @@\n a\n-b\n-c\n+C\n"))
        );
        // cleared, the line moves on to the next hunk again
        assert!(cursor.cancel_range());
        assert!(!cursor.cancel_range());
        for _ in 0..10 {
            cursor.move_line(&diff, Step::Next);
        }
        assert_eq!(cursor.hunk, 1);
        assert_eq!(highlighted(&cursor), 1);
        // a search ends the selection
        cursor.toggle_range();
        cursor.search(
            &diff,
            "a",
            0,
            SearchDirection::Next,
            SearchOptions::default(),
        );
        assert!(!cursor.has_range());
    }

    #[test]
    fn test_wrap_rows() {
        let line = |text: &str| Line::from(vec![Span::raw("12 "), Span::raw(text.to_string())]);
        let text = |l: &Line| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        // past the line numbers, continued under them
        let rows = wrap_line(line("abcdefgh"), 7);
        let texts: Vec<String> = rows.iter().map(text).collect();
        assert_eq!(texts, ["12 abcd", "   efgh"]);
        // the indent styled like the line numbers, a selection's highlight going down
        let mut selected = line("abcdefgh");
        selected.spans[0] = selected.spans[0].clone().bg(ratatui::style::Color::Red);
        let rows = wrap_line(selected, 7);
        assert_eq!(rows[1].spans[0].style.bg, Some(ratatui::style::Color::Red));

        // the screen rows asked for, wrapped or one per line
        let lines = (0..3).map(|i| (i, line("abcdefghijkl")));
        let visual = wrapped_rows(lines, 5..9, true, Rect::new(0, 0, 7, 4));
        let rows: Vec<usize> = visual.iter().map(|(row, _)| *row).collect();
        assert_eq!(rows, [1, 2, 2, 2]);
        let lines = (0..3).map(|i| (i, line("abcdefghijkl")));
        let visual = wrapped_rows(lines, 0..4, false, Rect::new(0, 0, 7, 4));
        assert_eq!(visual.len(), 3);
    }

    #[test]
    #[expect(
        clippy::single_range_in_vec_init,
        reason = "the emphasis of each line is a list of ranges, here of one"
    )]
    fn test_emphasis_of_changed_lines() {
        let diff = parse_diff(concat!(
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,4 +1,4 @@\n",
            "-call(a, 1, x);\n-  foo\n+call(b, 1, yz);\n+  bar\n",
            " ctx\n-é = 1\n\\ No newline at end of file\n+é = 2\n",
            "-one two three four\n+uno dos three cuatro\n",
        ));
        let texts: Vec<_> = diff.rows.iter().map(|r| diff.row_text(r)).collect();
        let emphasis: Vec<_> = diff
            .rows
            .iter()
            .zip(&texts)
            .map(|(r, text)| (text.as_ref(), diff.row_emphasis(r).to_vec()))
            .collect();
        assert_eq!(
            emphasis[1..],
            [
                // each edit apart
                ("call(a, 1, x);", vec![5..6, 11..12]),
                // nothing but the indentation in common
                ("  foo", vec![]),
                ("call(b, 1, yz);", vec![5..6, 11..13]),
                ("  bar", vec![]),
                ("ctx", vec![]),
                // past the note, in bytes
                ("é = 1", vec![5..6]),
                ("\\ No newline at end of file", vec![]),
                ("é = 2", vec![5..6]),
                // mostly different
                ("one two three four", vec![]),
                ("uno dos three cuatro", vec![]),
            ]
        );
    }

    // A diff that fits the panel still moves the line to a change: only the window stays put.
    #[test]
    fn test_page_moves_the_line_when_the_diff_fits() {
        let diff = parse_diff(
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,5 +1,5 @@\n a\n-b\n+B\n c\n d\n",
        );
        let changed: Vec<usize> = changed_rows(&diff).collect();
        let mut cursor = DiffCursor::default();
        cursor.scroll.height = 20;
        cursor.set_wrap_map(WrapMap::of(
            &diff,
            &ColorTheme::default(),
            80,
            false,
            DiffLayout::Unified,
        ));
        cursor.select_line(&diff, changed[0]);

        assert!(cursor.jump_page(&diff, UserEvent::PageDown, DiffPart::Line));
        assert!(cursor.line > changed[0], "the line moved");
        assert!(diff.rows[cursor.line].is_change());
        assert_eq!(cursor.scroll.offset, 0, "the window couldn't scroll");
    }

    // The line keys always land on a changed line, never on the context between them.
    #[test]
    fn test_page_lands_on_a_changed_line() {
        let context = " x\n".repeat(10);
        let hunk = format!("@@ -1,23 +1,23 @@\n-a\n+A\n{context}-b\n{context}+c\n");
        let diff = parse_diff(&format!("diff --git a/f b/f\n--- a/f\n+++ b/f\n{hunk}"));
        let changed: Vec<usize> = changed_rows(&diff).collect();
        let [a, big_a, b, c] = changed[..] else {
            panic!("{changed:?}")
        };
        let mut cursor = DiffCursor::default();
        cursor.scroll.height = 8;
        cursor.set_wrap_map(WrapMap::of(
            &diff,
            &ColorTheme::default(),
            80,
            false,
            DiffLayout::Unified,
        ));
        cursor.select_line(&diff, a);
        let mut jump = |event| {
            assert!(cursor.jump_page(&diff, event, DiffPart::Line));
            assert!(diff.rows[cursor.line].is_change());
            cursor.line
        };
        // 4 rows down is context, the next change after it
        assert_eq!(jump(UserEvent::HalfPageDown), b);
        assert_eq!(jump(UserEvent::HalfPageDown), c);
        assert_eq!(jump(UserEvent::HalfPageUp), b);
        assert_eq!(jump(UserEvent::HalfPageUp), big_a);
        assert_eq!(jump(UserEvent::HalfPageUp), a);
        // a whole page goes further than half of one
        assert_eq!(jump(UserEvent::PageDown), b);
        assert_eq!(jump(UserEvent::PageUp), big_a);
    }

    // A changed line taller than the panel is read a page at a time: the page scrolls it instead
    // of jumping past it or doing nothing.
    #[test]
    fn test_page_inside_a_tall_line_scrolls_it() {
        // the added line is the last changed line, taller than the panel
        let diff = parse_diff(&format!(
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,1 +1,1 @@\n-a\n+{}\n",
            "x".repeat(200)
        ));
        let changed: Vec<usize> = changed_rows(&diff).collect();
        let [_, long] = changed[..] else {
            panic!("{changed:?}")
        };
        let mut cursor = DiffCursor::default();
        cursor.scroll.height = 4;
        cursor.set_wrap_map(WrapMap::of(
            &diff,
            &ColorTheme::default(),
            20,
            true,
            DiffLayout::Unified,
        ));
        cursor.select_line(&diff, long);
        let offset = cursor.scroll.offset;

        assert!(cursor.jump_page(&diff, UserEvent::PageDown, DiffPart::Line));
        assert_eq!(cursor.line, long, "the tall line stayed selected");
        assert_eq!(cursor.scroll.offset, offset + 4, "the tall line scrolled");

        // past its end, the page stops at the bottom instead of snapping back to its top
        while cursor.scroll.offset + 4 < cursor.wrap().total() {
            cursor.jump_page(&diff, UserEvent::PageDown, DiffPart::Line);
        }
        cursor.jump_page(&diff, UserEvent::PageDown, DiffPart::Line);
        assert_eq!(cursor.line, long);
        assert_eq!(
            cursor.scroll.offset,
            cursor.wrap().total().saturating_sub(4),
            "the page stopped at the bottom"
        );
    }

    // A short hunk hands the page over to the next one; a hunk taller than the panel scrolls.
    #[test]
    fn test_page_with_short_and_tall_hunks() {
        let context = " x\n".repeat(20);
        let diff = parse_diff(&format!(
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n\
             @@ -1,1 +1,1 @@\n-a\n+A\n\
             @@ -26,22 +26,22 @@\n{context}-b\n+B\n"
        ));
        assert_eq!(diff.hunks.len(), 2);

        // the first hunk is short: the page moves to the second
        let mut cursor = DiffCursor::default();
        cursor.scroll.height = 4;
        cursor.set_wrap_map(WrapMap::of(
            &diff,
            &ColorTheme::default(),
            80,
            false,
            DiffLayout::Unified,
        ));
        cursor.select_hunk(&diff, 0);
        assert!(cursor.jump_page(&diff, UserEvent::PageDown, DiffPart::Hunk));
        assert_eq!(cursor.hunk, 1);

        // the second is taller than the panel: the page scrolls it, keeping it selected
        cursor.select_hunk(&diff, 1);
        let offset = cursor.scroll.offset;
        assert!(cursor.jump_page(&diff, UserEvent::PageDown, DiffPart::Hunk));
        assert_eq!(cursor.hunk, 1);
        assert_eq!(cursor.scroll.offset, offset + 4);

        // past its end, the page stops at the bottom instead of snapping back to its top
        while cursor.scroll.offset + 4 < cursor.wrap().total() {
            cursor.jump_page(&diff, UserEvent::PageDown, DiffPart::Hunk);
        }
        cursor.jump_page(&diff, UserEvent::PageDown, DiffPart::Hunk);
        assert_eq!(cursor.hunk, 1);
        assert_eq!(
            cursor.scroll.offset,
            cursor.wrap().total().saturating_sub(4)
        );
    }

    #[test]
    fn test_partial_patch() {
        let file_header = "diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n";
        let hunk = "@@ -1,3 +1,3 @@\n a\n-b\n-c\n+B\n+C\n";
        let diff = parse_diff(&format!("{file_header}{hunk}"));
        let hunk = &diff.hunks[0];

        // staging "-b" and "+B": "-c" stays in the index as context, "+C" is left out
        let actual = hunk.partial_patch(&[1, 3], PatchDirection::Forward);
        let expected = format!("{file_header}@@ -1,3 +1,3 @@\n a\n-b\n c\n+B\n");
        assert_eq!(actual, expected);

        // unstaging "+C": "+B" stays in the index as context, "-b" and "-c" are left out
        let actual = hunk.partial_patch(&[4], PatchDirection::Reverse);
        let expected = format!("{file_header}@@ -1,2 +1,3 @@\n a\n B\n+C\n");
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_partial_patch_of_added_file() {
        let file_header =
            "diff --git a/f b/f\nnew file mode 100644\nindex 0000000..1234567\n--- /dev/null\n+++ b/f\n";
        let diff = parse_diff(&format!("{file_header}@@ -0,0 +1,2 @@\n+a\n+b\n"));
        let hunk = &diff.hunks[0];

        // staging part of a new file still creates it
        let actual = hunk.partial_patch(&[0], PatchDirection::Forward);
        let expected = format!("{file_header}@@ -0,0 +1,1 @@\n+a\n");
        assert_eq!(actual, expected);

        // unstaging part of it keeps the file, so it becomes a modification
        let actual = hunk.partial_patch(&[1], PatchDirection::Reverse);
        let expected = "diff --git a/f b/f\nindex 0000000..1234567\n--- a/f\n+++ b/f\n@@ -1,1 +1,2 @@\n a\n+b\n";
        assert_eq!(actual, expected);
    }

    // Side by side, a removed line sits beside the first added line after the last one paired
    // that is alike, like delta's: the added lines passed on their own rows before it, a removed
    // line alike to none on its own row, a note on the side of the line it follows.
    #[test]
    fn test_split_rows_pair_alike_lines() {
        let diff = parse_diff(concat!(
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,5 +1,5 @@\n ctx\n",
            "-call(a, 1);\n-zzz qqq\n-foo bar baz\n",
            "+totally new\n+call(b, 1);\n+foo bar qux\n\\ No newline at end of file\n",
        ));
        let texts: Vec<_> = diff.rows.iter().map(|r| diff.row_text(r)).collect();
        let text = |row: Option<u32>| row.map_or("", |row| texts[row as usize].as_ref());
        let rows: Vec<_> = diff
            .split_rows()
            .into_iter()
            .map(|row| match row {
                SplitRow::Whole(row) => (text(Some(row)), "*"),
                SplitRow::Sides(old, new) => (text(old), text(new)),
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("@@ -1,5 +1,5 @@", "*"),
                ("ctx", "ctx"),
                ("", "totally new"),
                ("call(a, 1);", "call(b, 1);"),
                ("zzz qqq", ""),
                ("foo bar baz", "foo bar qux"),
                ("", "\\ No newline at end of file"),
            ]
        );
        // the scroll counts them: the change's rows are on the screen rows of their pair
        let map = WrapMap::of(&diff, &ColorTheme::default(), 40, false, DiffLayout::Split);
        assert_eq!(map.total(), 7);
        // "call(b, 1);", row 6, beside "call(a, 1);"
        assert_eq!(map.visual_range(6..7), 3..4);
        assert_eq!(map.visual_range(2..3), 3..4);
        // the hunk, from its header to its last row
        assert_eq!(map.visual_range(0..9), 0..7);
        assert_eq!(map.logical_at(2), 5);
    }

    #[test]
    fn test_wrap_map_makes_no_lines_unwrapped() {
        // each row its own screen row, the lines never made
        let map = WrapMap::from_lines(3, || -> Vec<Line> { panic!("made") }, 80, false);
        assert_eq!(map.total(), 3);
        assert_eq!(map.visual_range(1..2), 1..2);
        assert_eq!(map.logical_rows(1..5), 1..3);
        assert_eq!(map.logical_at(2), 2);
        // while wrapping they are measured
        let lines = || vec![Line::raw("a".repeat(25)), Line::raw("b")];
        let map = WrapMap::from_lines(2, lines, 10, true);
        assert_eq!(map.total(), 4);
        assert_eq!(map.visual_range(1..2), 3..4);
    }

    // A search keeps the lines picked while it stays in their hunk; in another one it clears them,
    // as they are staged from the selected hunk. It used to clear them always.
    #[test]
    fn test_search_keeps_the_lines_picked_in_their_hunk() {
        let diff = parse_diff(concat!(
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n",
            "@@ -1,2 +1,2 @@\n-one\n+uno\n-two\n+dos\n",
            "@@ -10 +10 @@\n-ten\n+diez\n",
        ));
        let options = SearchOptions::default();
        let mut cursor = DiffCursor::default();
        assert!(cursor.select_hunk(&diff, 0));
        assert!(cursor.enter_lines(&diff));
        cursor.toggle_range();
        assert!(cursor.has_range());
        cursor.search(&diff, "dos", 0, SearchDirection::Next, options);
        assert!(cursor.has_range());
        cursor.search(&diff, "diez", 0, SearchDirection::Next, options);
        assert!(!cursor.has_range());
        assert_eq!(cursor.hunk, 1);
    }

    #[test]
    fn test_tabs_show_as_spaces() {
        let diff =
            parse_diff("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-\ta = 1\n+\ta = 2\n");
        // shown as spaces, the changed word marked in the text as shown
        let removed = &diff.rows[1];
        assert_eq!(diff.row_text(removed), "    a = 1");
        assert_eq!(diff.row_emphasis(removed), [Range { start: 8, end: 9 }]);
        // and searched as in the file, like a file preview
        let options = SearchOptions::default();
        assert!(row_matches(&diff, 2, "\ta = 2", options));
        assert!(!row_matches(&diff, 2, "    a = 2", options));
        // the patch keeps the tab, byte for byte
        assert!(diff.hunks[0].patch().contains("+\ta = 2"));
    }

    #[test]
    fn test_wrapped_height_counts_the_rows_of_wrap_line() {
        let lines = [
            Line::from(vec![Span::raw("  1   1 "), Span::raw("x".repeat(70))]),
            Line::from(vec![
                Span::raw("12 "),
                Span::raw("é漢字".repeat(9)),
                Span::raw("tail"),
            ]),
            Line::raw("a single span, no line numbers to indent the rows by"),
            Line::from(vec![Span::raw("wide gutter "), Span::raw("text")]),
            Line::raw(""),
        ];
        for line in lines {
            for width in [1, 2, 5, 12, 13, 20, 80] {
                let made = wrap_line(line.clone(), width).len();
                assert_eq!(wrapped_height(&line, width), made, "{line:?} at {width}");
            }
        }
    }
}
