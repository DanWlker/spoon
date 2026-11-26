use std::{borrow::Cow, cell::OnceCell, num::NonZeroU32, ops::Range, sync::Arc};

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
    background::{Cancel, Job},
    color::ColorTheme,
    event::Sender,
    git::{git_bytes, text_from_git, ConvertedPaths, PatchDirection},
    keybind::UserEvent,
    pointer::OnPicks,
    search::SearchOptions,
    widget::{
        picks::Picks,
        style::{
            divider, mark_selected_bar, page_rows, picked_style, placeholder, selection_rows,
            tabs_shown, ScrollState, VisualSelection,
        },
        tree_query::{match_range, patch_range, SearchDirection},
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
        let mut paths: Vec<Cow<str>> = self.hunks.iter().map(DiffHunk::path).collect();
        paths.dedup();
        let converted = converted.of(&paths.iter().map(AsRef::as_ref).collect::<Vec<_>>());
        if converted.is_empty() {
            return;
        }
        self.hunks
            .retain(|h| !converted.contains(h.path().as_ref()));
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
    pub fn path(&self) -> Cow<'_, str> {
        let file_header = self.file_header();
        header_path(file_header, "+++ ", "b/")
            .or_else(|| header_path(file_header, "--- ", "a/"))
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
    let path_of = |marker, prefix| header_path(file_header, marker, prefix).unwrap_or_default();
    let new_path = path_of("+++ ", "b/");
    let old_path = path_of("--- ", "a/");
    let (old_new, new_new) = (quote_path("a/", &new_path), quote_path("b/", &new_path));
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
                    return Some(format!("diff --git {old_new} {new_new}\n"));
                }
                if line.starts_with("--- ") {
                    return Some(format!("--- {old_new}\n"));
                }
            }
            if (old_count > 0 && line.starts_with("new file mode"))
                || (new_count > 0 && line.starts_with("deleted file mode"))
            {
                None
            } else if old_count > 0 && line.starts_with("--- /dev/null") {
                Some(format!("--- {old_new}\n"))
            } else if new_count > 0 && line.starts_with("+++ /dev/null") {
                Some(format!("+++ {}\n", quote_path("b/", &old_path)))
            } else {
                Some(line.to_string())
            }
        })
        .collect()
}

// The path on the `marker` line of a file header ("+++ " or "--- "), without its `prefix`. Git
// quotes a name holding a quote, a backslash or a control character, C style ("b/say \"hi\""),
// and ends a name holding a space with a tab.
fn header_path<'a>(file_header: &'a str, marker: &str, prefix: &str) -> Option<Cow<'a, str>> {
    let rest = file_header.lines().find_map(|l| l.strip_prefix(marker))?;
    let Some(quoted) = rest.strip_prefix('"') else {
        let rest = rest.strip_suffix('\t').unwrap_or(rest);
        return rest.strip_prefix(prefix).map(Cow::Borrowed);
    };
    let name = unquote(quoted)?;
    name.strip_prefix(prefix)
        .map(|path| path.to_string().into())
}

// A name git quoted C style, from after its opening quote: up to the closing one, if any, with
// its escapes read. None when an escape is cut short.
fn unquote(quoted: &str) -> Option<String> {
    let bytes = git_bytes(quoted);
    let mut name = Vec::with_capacity(bytes.len());
    let mut it = bytes.iter().copied();
    while let Some(b) = it.next() {
        match b {
            b'"' => break,
            b'\\' => name.push(match it.next()? {
                b'a' => 7,
                b'b' => 8,
                b't' => b'\t',
                b'n' => b'\n',
                b'v' => 11,
                b'f' => 12,
                b'r' => b'\r',
                d @ b'0'..=b'3' => {
                    let digit = |b: u8| (b'0'..=b'7').contains(&b).then(|| b - b'0');
                    let (d2, d3) = (digit(it.next()?)?, digit(it.next()?)?);
                    (d - b'0') << 6 | d2 << 3 | d3
                }
                other => other,
            }),
            b => name.push(b),
        }
    }
    Some(text_from_git(&name))
}

// `prefix` and `path` as git names them in a file header, quoted as `header_path` reads them.
fn quote_path(prefix: &str, path: &str) -> String {
    let special = |c: char| c == '"' || c == '\\' || c.is_ascii_control();
    if !path.contains(special) {
        return format!("{prefix}{path}");
    }
    let mut quoted = format!("\"{prefix}");
    for c in path.chars() {
        match c {
            '"' | '\\' => quoted.extend(['\\', c]),
            '\t' => quoted.push_str("\\t"),
            '\n' => quoted.push_str("\\n"),
            c if c.is_ascii_control() => quoted.push_str(&format!("\\{:03o}", u32::from(c))),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
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
    // the rows shown wrapped while wrapping; the others take a screen row each, drawn as they are,
    // until the whole diff is measured, see `rewrap`
    measured: Range<usize>,
}

// Every row of a diff or a file, for `WrapMap` to measure, see `rewrap`.
pub const ALL_ROWS: Range<usize> = 0..usize::MAX;

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
    #[cfg(test)]
    pub fn of(
        diff: &ParsedDiff,
        theme: &ColorTheme,
        width: u16,
        wrap: bool,
        layout: DiffLayout,
    ) -> Self {
        Self::of_until(
            diff,
            theme,
            width,
            wrap,
            layout,
            ALL_ROWS,
            &Cancel::default(),
        )
    }

    // `of`, wrapping only the shown rows of the rows of the diff `near` (see `ALL_ROWS`), and
    // stopping once `cancel` says nobody waits for it any more, as measuring a big diff's rows
    // wrapped on another thread does, see `rewrap`; its rows measured so far then.
    pub fn of_until(
        diff: &ParsedDiff,
        theme: &ColorTheme,
        width: u16,
        wrap: bool,
        layout: DiffLayout,
        near: Range<usize>,
        cancel: &Cancel,
    ) -> Self {
        // asked now and then, a few times a millisecond
        const ASKED_EVERY: usize = 1024;
        let split_rows = match layout {
            DiffLayout::Split => diff.split_rows(),
            DiffLayout::Unified => Vec::new(),
        };
        if split_rows.is_empty() {
            let rows = diff_line_count(diff);
            // made a few at a time, the whole diff of lines would take as much memory again
            let lines = |range: Range<usize>| {
                let end = range.end;
                range
                    .step_by(ASKED_EVERY)
                    .take_while(|_| !cancel.is_cancelled())
                    .flat_map(move |start| {
                        diff_lines(diff, theme, start..end.min(start + ASKED_EVERY), width)
                    })
            };
            return WrapMap {
                layout,
                ..Self::from_lines(rows, near, lines, width, wrap)
            };
        }
        let mut placed = vec![0; diff.rows.len()];
        for (at, split_row) in split_rows.iter().enumerate() {
            for row in split_row.rows() {
                placed[row] = row_u32(at);
            }
        }
        let mut map = WrapMap {
            width,
            wrap,
            layout,
            rows: split_rows.len(),
            split: Some(SplitMap {
                rows: split_rows,
                placed,
            }),
            ..WrapMap::default()
        };
        if wrap {
            let (old_width, new_width) = split_widths(width);
            let height = |row: u32, gutter, width: u16| {
                let line = diff_line(diff, &diff.rows[row as usize], gutter, theme, width);
                wrapped_height(&line, width as usize)
            };
            map.measured =
                map.place(near.start.min(diff.rows.len())..near.end.min(diff.rows.len()));
            let split_rows = map.split_rows().unwrap_or_default();
            let asked =
                |(i, _): &(usize, &SplitRow)| i % ASKED_EVERY != 0 || !cancel.is_cancelled();
            let shown = split_rows[map.measured.clone()]
                .iter()
                .enumerate()
                .take_while(asked);
            let heights = shown.map(|(_, split_row)| match *split_row {
                SplitRow::Whole(row) => height(row, Gutter::Both, width),
                SplitRow::Sides(old, new) => {
                    let old = old.map_or(1, |row| height(row, Gutter::Old, old_width));
                    let new = new.map_or(1, |row| height(row, Gutter::New, new_width));
                    old.max(new)
                }
            });
            map.prefix = prefix_of(map.measured_rows(heights));
        }
        map
    }

    // The heights of every row shown, those of the `measured` rows given, one screen row each
    // past them.
    fn measured_rows(&self, measured: impl Iterator<Item = usize>) -> impl Iterator<Item = usize> {
        let ones = std::iter::repeat_n(1, self.measured.start);
        let after = std::iter::repeat_n(1, self.rows.saturating_sub(self.measured.end));
        ones.chain(measured).chain(after)
    }

    // The screen rows the `rows` of `lines` wrap to at `width`, wrapped when `wrap`; the diff and
    // the file preview measure the same way. Only the rows `near` (see `ALL_ROWS`) are measured,
    // `lines` making those: a big diff would take a while, and as much memory again.
    pub fn from_lines<'a, L: IntoIterator<Item = Line<'a>>>(
        rows: usize,
        near: Range<usize>,
        lines: impl FnOnce(Range<usize>) -> L,
        width: u16,
        wrap: bool,
    ) -> Self {
        let mut map = WrapMap {
            width,
            wrap,
            rows,
            ..WrapMap::default()
        };
        if wrap {
            map.measured = near.start.min(rows)..near.end.min(rows);
            let lines = lines(map.measured.clone()).into_iter();
            let heights = lines.map(|line| wrapped_height(&line, width as usize));
            map.prefix = prefix_of(map.measured_rows(heights));
        }
        map
    }

    // Whether row shown `shown` is drawn wrapped: while wrapping, once measured, see `rewrap`.
    pub fn wraps_row(&self, shown: usize) -> bool {
        self.wrap && self.measured.contains(&shown)
    }

    // Whether the shown rows of the rows of the diff `rows` are measured, wrapped or not.
    fn measures(&self, rows: Range<usize>) -> bool {
        let logical = self
            .split
            .as_ref()
            .map_or(self.rows, |split| split.placed.len());
        let shown = self.place(rows.start.min(logical)..rows.end.min(logical));
        !self.wrap || (self.measured.start <= shown.start && shown.end <= self.measured.end)
    }

    // Whether every row is measured, wrapped or not, see `rewrap`.
    pub fn is_measured(&self) -> bool {
        !self.wrap || self.measured == (0..self.rows)
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

// The screen rows of many rows wrapped at `width`, side by side or not, measuring on another
// thread, see `rewrap`.
#[derive(Debug)]
pub struct Measuring {
    width: u16,
    layout: DiffLayout,
    job: Job<WrapMap>,
}

// The map at `width`, `wrap` and `layout`, when `current` isn't it, see `WrapMap`: with `tx`, `map`
// measures only the rows of the diff `near` the screen right away, so the screen shows wrapped at
// once, and when there are more, `work` measures all of them on another thread (see `measuring`);
// meanwhile, the screen moved past those rows, `map` measures those near it instead, so the rows
// shown are always wrapped. Without `tx`, `map` measures all of them (`ALL_ROWS`). A diff and the
// lines of a file are measured so.
pub fn rewrap<W: FnOnce(&Cancel) -> WrapMap + Send + 'static>(
    current: &WrapMap,
    measuring: &mut Option<Measuring>,
    tx: Option<&Sender>,
    (width, wrap, layout): (u16, bool, DiffLayout),
    near: NearScreen,
    map: impl FnOnce(Range<usize>) -> WrapMap,
    work: impl FnOnce() -> W,
) -> Option<WrapMap> {
    let measured = match measuring.take() {
        Some(m) if wrap && (m.width, m.layout) == (width, layout) => match m.job.try_take() {
            Some(map) => Some(map),
            None => {
                *measuring = Some(m);
                None
            }
        },
        // measuring for another width or layout
        _ => None,
    };
    Some(match measured {
        Some(map) => map,
        None if current.matches(width, wrap, layout) => {
            if measuring.is_none() || current.measures(near.shown()) {
                return None;
            }
            map(near.measured())
        }
        None => {
            let map = map(if tx.is_some() {
                near.measured()
            } else {
                ALL_ROWS
            });
            if let Some(tx) = tx.filter(|_| !map.is_measured()) {
                // stopped once measuring for another width or diff, see `Job::read`
                let job = Job::read(tx.clone(), work());
                *measuring = Some(Measuring { width, layout, job });
            }
            map
        }
    })
}

// The rows of a diff or a file near the screen, `height` rows high from the row `top`, which
// `rewrap` measures right away.
#[derive(Debug, Clone, Copy)]
pub struct NearScreen {
    pub top: usize,
    pub height: u16,
}

impl NearScreen {
    // Those the screen can show: as many as it is high, twice, each of a pair side by side being
    // a row of the diff.
    fn shown(self) -> Range<usize> {
        self.top..self.top.saturating_add(2 * usize::from(self.height))
    }

    // Those measured: the ones shown, and a screen of them above to scroll to.
    fn measured(self) -> Range<usize> {
        let shown = self.shown();
        self.top.saturating_sub(usize::from(self.height))..shown.end
    }
}

// The widths of the old and the new side of a diff `width` wide, a divider between them.
pub fn split_widths(width: u16) -> (u16, u16) {
    let sides = width.saturating_sub(1);
    (sides / 2, sides - sides / 2)
}

// The rows of `diff` a search matches, see `row_matches`, in order: a big diff's searched on all
// the cores, a part at a time, as each letter typed searches it again.
pub fn matching_rows(diff: &ParsedDiff, query: &str, options: SearchOptions) -> Vec<usize> {
    // a millisecond or two on one core, each core taking the next part when done with one, so the
    // slower cores take fewer
    const PART: usize = 16 * 1024;
    let rows = diff.rows.len();
    let matching = |part: usize| -> Vec<usize> {
        let rows = part * PART..rows.min((part + 1) * PART);
        rows.filter(|&row| row_matches(diff, row, query, options))
            .collect()
    };
    let parts = rows.div_ceil(PART);
    if parts <= 1 {
        return matching(0);
    }
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut found: Vec<(usize, Vec<usize>)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..cores.min(parts))
            .map(|_| {
                scope.spawn(|| {
                    let mut found = Vec::new();
                    loop {
                        let part = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if part >= parts {
                            return found;
                        }
                        found.push((part, matching(part)));
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| {
                worker
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    });
    found.sort_unstable_by_key(|(part, _)| *part);
    found.into_iter().flat_map(|(_, rows)| rows).collect()
}

// Whether the text of the row of `diff` at `row` matches a search, as it is in the file, like a
// file preview's lines.
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
    // the rows picked, kept once the visual selection ends until cleared, all in the selected hunk;
    // like the rows of a list, see `Picks`
    picked: Picks<usize>,
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
            let rows: Vec<usize> = rows
                .filter(|&r| side.is_none_or(|kind| diff.rows[r].kind == kind))
                .collect();
            self.picked.sync_visual(&rows);
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
        matches: &[usize],
        from: usize,
        direction: SearchDirection,
    ) -> Option<DiffPart> {
        // the first match from `from` in `direction`, round
        let after = |row: usize| matches.partition_point(|&m| m < row);
        let row = match direction {
            SearchDirection::Current => matches.get(after(from)),
            SearchDirection::Next => matches.get(after(from + 1)),
            SearchDirection::Previous => after(from).checked_sub(1).and_then(|i| matches.get(i)),
        };
        let round = match direction {
            SearchDirection::Previous => matches.last(),
            _ => matches.first(),
        };
        let row = *row.or(round)?;
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

    // The page keys on the selected hunk or line, like vim's and a list's: the window moves half a
    // page or a page, and the selection moves as much, to the changed line or the hunk at the
    // screen row it lands on, else the next one on (the window going on to show it), else the
    // last. A hunk taller than the page is read a page at a time. At either end the window stops
    // and the selection goes on. False for other events.
    pub fn jump_page(&mut self, diff: &ParsedDiff, event: UserEvent, part: DiffPart) -> bool {
        let Some(rows) = page_rows(event, self.scroll.height) else {
            return false;
        };
        let down = rows > 0;
        let rows = rows.unsigned_abs();
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
        // past its end: the window moves, then the unit at the screen row the page reached, else
        // the next or previous one, or the edge of the diff when there is none further and the
        // selection is already on it
        let last_offset = self.wrap.total().saturating_sub(self.scroll.height);
        let moved = if down {
            self.scroll.offset.saturating_add(rows)
        } else {
            self.scroll.offset.saturating_sub(rows)
        };
        self.scroll.offset = moved.min(last_offset);
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
                    // the picked lines stay in their hunk, the window with them
                    _ if self.has_range() => self.scroll_to(self.line..self.line + 1),
                    _ => self.scroll_to_edge(down),
                }
            }
            DiffPart::Hunk => {
                // the hunk the row is in, else the next or previous one
                let found = if down {
                    diff.hunks.iter().position(|h| h.rows.end > row)
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
                        // the window stays where it moved while the hunk shows in it
                        let offset = self.scroll.offset;
                        self.select_hunk(diff, index);
                        let screen = self.wrap.visual_range(diff.hunks[index].rows.clone());
                        let height = self.scroll.height;
                        if screen.start < offset + height && offset < screen.end {
                            self.scroll.offset = offset;
                        }
                    }
                    _ => self.scroll_to_edge(down),
                }
            }
        }
        true
    }

    // After ctrl-e or ctrl-y scrolled the window: the selected line pulled along to the nearest
    // changed line out of the `scrolloff` rows at its edges, like vim's and like a list's; the
    // selected hunk once it left the window whole, to the nearest hunk in it. The window stays
    // where it scrolled to.
    pub fn keep_in_view(&mut self, diff: &ParsedDiff, part: DiffPart) {
        let height = self.scroll.height;
        let total = self.wrap.total();
        self.scroll.offset = self.scroll.offset.min(total.saturating_sub(height.max(1)));
        let offset = self.scroll.offset;
        let window = self.wrap.logical_rows(offset..offset + height);
        if window.is_empty() {
            return;
        }
        let crosses = |rows: &Range<usize>| rows.start < window.end && window.start < rows.end;
        let allowed = selection_rows(offset, height, total, self.scroll.scrolloff);
        // while split, the window's rows can hold rows of a change scrolled out beside them
        let shows = |row: usize| {
            let rows = self.wrap.visual_range(row..row + 1);
            rows.start < allowed.end && allowed.start < rows.end
        };
        match part {
            DiffPart::Line => {
                if shows(self.line) {
                    return;
                }
                let mut rows = self.selectable_rows(diff).filter(|r| window.contains(r));
                let above = self.wrap.visual_range(self.line..self.line + 1).start < allowed.start;
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
        match self.visual.toggle(self.line) {
            true => {
                self.picked.start_visual();
                self.picked.sync_visual(&[self.line]);
            }
            false => self.picked.finish_visual(),
        }
    }

    // Clears the lines picked; false when there were none.
    pub fn cancel_range(&mut self) -> bool {
        self.visual.cancel();
        let picked = !self.picked.is_empty();
        self.picked.clear();
        picked
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
        let mut rows: Vec<usize> = self.picked.iter().copied().collect();
        if rows.is_empty() {
            rows.push(self.line);
        }
        rows.sort_unstable();
        rows
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
        self.scroll.show_rows(self.wrap.visual_range(rows));
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

    // How `row` is marked on its line numbers: the lines picked in the picked color, the cursor
    // line selected, like a file's text (picked too while selecting), or the changed lines of the
    // selected hunk selected.
    pub fn row_mark(&self, diff: &ParsedDiff, part: DiffPart, row: usize) -> Option<RowMark> {
        match part {
            DiffPart::Line => {
                let picked = self.picked.contains(&row) && diff.rows[row].is_change();
                if picked && (self.visual.is_visual() || row != self.line) {
                    Some(RowMark::Picked)
                } else {
                    (row == self.line).then_some(RowMark::Selected)
                }
            }
            DiffPart::Hunk => self.is_cursor(diff, part, row).then_some(RowMark::Selected),
        }
    }

    // Whether the bar beside `row` marks it: the cursor line, or the changed lines of the hunk.
    pub fn is_cursor(&self, diff: &ParsedDiff, part: DiffPart, row: usize) -> bool {
        match part {
            DiffPart::Line => row == self.line,
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
        let text = &self.raw[row.text.clone()];
        // a file's name git quoted, see `new_path_start`
        if row.kind == DiffRowKind::File && self.raw[..row.text.start].ends_with("\"b/") {
            if let Some(name) = unquote(text) {
                return shown(&name).into_owned().into();
            }
        }
        shown(text)
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
    parse_diff_until(diff, &Cancel::default())
}

// `parse_diff`, stopping once `cancel` says nobody waits for it any more, with what it parsed so
// far: a big diff takes a while, see `Job::read`.
pub fn parse_diff_until(diff: &str, cancel: &Cancel) -> ParsedDiff {
    let shared: Arc<str> = Arc::from(diff);
    // file names are only shown when the diff spans multiple files (a directory); a conflicted one
    // is a combined diff, named, its hunks of three sides not staged nor shown
    const COMBINED: &str = "diff --cc ";
    let multiple_files_count = diff
        .lines()
        .filter(|l| l.starts_with("diff --git ") || l.starts_with(COMBINED))
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
    for (i, raw) in diff.split_inclusive('\n').enumerate() {
        // asked now and then, a few times a millisecond
        if i % 4096 == 0 && cancel.is_cancelled() {
            break;
        }
        let start = end;
        end += raw.len();
        let line = raw.trim_end_matches(['\n', '\r']);
        // the bytes of the diff of `line` from `from`
        let bytes = |from: usize| start + from..start + line.len();

        if line.starts_with(COMBINED) {
            finish_hunk(&mut current, &mut hunks, rows.len());
            file_start = start;
            if multiple_files {
                let path = start + COMBINED.len()..start + line.len();
                rows.push(DiffRow::without_numbers(DiffRowKind::File, path));
            }
            continue;
        }
        if line.starts_with("diff --git ") {
            finish_hunk(&mut current, &mut hunks, rows.len());
            file_start = start;
            if multiple_files {
                let path = new_path_start(line);
                // a quoted name without its closing quote
                let end = start + line.len() - usize::from(line.ends_with('"'));
                rows.push(DiffRow::without_numbers(
                    DiffRowKind::File,
                    start + path..end,
                ));
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
    let (emphasis, pairs) = pair_changes(&mut rows, &shared, cancel);
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
fn pair_changes(
    rows: &mut [DiffRow],
    diff: &str,
    cancel: &Cancel,
) -> (Vec<Range<usize>>, Vec<(usize, usize)>) {
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
            if cancel.is_cancelled() {
                return (emphasis, pairs);
            }
            // a line diffed against a few others can take a while
            let tried = (next..added.len()).take(LOOKAHEAD);
            let found = tried
                .take_while(|_| !cancel.is_cancelled())
                .find_map(|new| {
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

// The bytes of the words of `old` and `new` that differ, by a diff of their words: the diff is
// parsed on another thread, see `DiffPane::load_in_background`. None when the lines aren't
// `alike`, or alike only in blanks, or too long to diff quickly.
fn changed_ranges(old: &Words, new: &Words) -> Option<[Vec<Range<usize>>; 2]> {
    // shortcut: longer lines, like minified code, aren't marked, as diffing two of 100k words
    // takes seconds; raise it if long lines of prose should be
    const MAX_WORDS: usize = 1024;
    if old.words.len() > MAX_WORDS || new.words.len() > MAX_WORDS {
        return None;
    }
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
    // quoted by git, for a quote, a backslash or a control character: after the last ` "b/`,
    // which can't be inside a quoted name, its quotes escaped, nor in one not quoted
    if line.ends_with('"') {
        if let Some(i) = line.rfind(" \"b/") {
            return i + 4;
        }
    }
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
            .map(|l| Line::raw(tabs_shown(l)).fg(theme.muted_fg))
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

// How a row of a diff is marked on its line numbers, like the rows of a list: picked, or
// selected (the cursor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowMark {
    Picked,
    Selected,
}

// Marks a line of `diff_lines` on its line numbers, keeping the colors of the change.
fn mark_diff_line(line: &mut Line, mark: RowMark, theme: &ColorTheme) {
    if let Some(gutter) = line.spans.first_mut() {
        gutter.style = match mark {
            RowMark::Picked => gutter.style.patch(picked_style(theme)),
            RowMark::Selected => gutter
                .style
                .fg(theme.list_selected_fg)
                .bg(theme.list_selected_bg)
                .add_modifier(Modifier::BOLD),
        };
    }
}

// Renders `lines` with their logical row, the screen rows `screen` of them, a line's background
// (added, removed) across `area`, the rows marked on their line numbers by `mark` and the cursor
// rows by the bar left of `area`; long lines wrapped at the width of `area`, those `wrap` says.
#[expect(clippy::too_many_arguments)]
pub fn render_diff_lines<'a>(
    lines: impl Iterator<Item = (usize, Line<'a>)>,
    screen: Range<usize>,
    mark: impl Fn(usize) -> Option<RowMark>,
    cursor: impl Fn(usize) -> bool,
    wrap: impl Fn(usize) -> bool,
    theme: &ColorTheme,
    area: Rect,
    buf: &mut Buffer,
) {
    let visual = wrapped_rows(
        lines.map(|(row, mut line)| {
            if let Some(mark) = mark(row) {
                mark_diff_line(&mut line, mark, theme);
            }
            (row, line)
        }),
        screen,
        wrap,
        area,
    );
    render_visual_rows(visual, cursor, theme, area, buf);
}

// The stripes where a side has no line beside one on the other side, in the split diff and the
// conflict view alike.
pub fn filler(width: u16, theme: &ColorTheme) -> Span<'static> {
    Span::styled(
        "╱".repeat(width as usize),
        Style::default()
            .fg(theme.diff_filler_fg)
            .bg(theme.diff_filler_bg),
    )
}

// Draws the screen rows `screen` of `diff` side by side, see `ParsedDiff::split_rows`: the old
// side left of a divider, the new side right of it, a file or hunk header across both; otherwise
// like `render_diff_lines`, each line made changed by `restyle` (the matches of a search), wrapped
// as `map` says. Returns the rows of the diff on each screen row drawn, of the old side and of the
// new side.
#[expect(clippy::too_many_arguments)]
pub fn render_split_rows(
    diff: &ParsedDiff,
    map: &WrapMap,
    screen: Range<usize>,
    (mark, cursor): (impl Fn(usize) -> Option<RowMark>, impl Fn(usize) -> bool),
    restyle: impl Fn(&mut Line),
    theme: &ColorTheme,
    area: Rect,
    buf: &mut Buffer,
) -> Vec<[Option<usize>; 2]> {
    let (old_width, new_width) = split_widths(area.width);
    let make = |row: usize, gutter, width: u16, wrap: bool| -> Vec<Line<'static>> {
        let mut line = diff_line(diff, &diff.rows[row], gutter, theme, width);
        if let Some(mark) = mark(row) {
            mark_diff_line(&mut line, mark, theme);
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
    let split_rows = &map.split_rows().unwrap_or_default()[shown.clone()];
    for (split_row, at) in split_rows.iter().zip(shown) {
        let wrap = map.wraps_row(at);
        match *split_row {
            SplitRow::Whole(row) => {
                let row = row as usize;
                let lines = make(row, Gutter::Both, area.width, wrap).into_iter();
                drawn.extend(lines.map(|line| Drawn::Whole(row, line)));
            }
            SplitRow::Sides(old, new) => {
                let (old, new) = (old.map(|row| row as usize), new.map(|row| row as usize));
                let side = |row: Option<usize>, gutter, width| {
                    row.map_or_else(Vec::new, |row| make(row, gutter, width, wrap))
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
            Drawn::Sides(rows, lines) => {
                let new_x = area.x.saturating_add(old_width).saturating_add(1);
                let sides = [
                    Rect::new(area.x, y, old_width, 1),
                    Rect::new(new_x, y, new_width, 1),
                ];
                for (i, ((row, line), side)) in rows.iter().zip(lines).zip(sides).enumerate() {
                    // not beside a note like "\ No newline at end of file", which isn't a line
                    let beside_change = rows[1 - i].is_some_and(|r| diff.rows[r].is_change());
                    match (row, line) {
                        (_, Some(line)) => draw_row(line, side, theme, buf),
                        // no line on this side beside one added or removed: filler stripes
                        (None, None) if beside_change => {
                            buf.set_span(side.x, y, &filler(side.width, theme), side.width);
                        }
                        (None, None) => {}
                        // the rows past a line shorter than the one beside it, wrapped, stay blank
                        (Some(_), None) => {}
                    }
                }
                if old_width < area.width {
                    buf.set_string(
                        area.x.saturating_add(old_width),
                        y,
                        "│",
                        Style::default().fg(theme.divider_fg),
                    );
                }
                rows
            }
        };
        if rows.iter().flatten().any(|&row| cursor(row)) {
            mark_row_selected(area, y, theme, buf);
        }
        hits.push(rows);
    }
    hits
}

// The screen rows `screen` of `lines` with their logical row, wrapped at the width of `area` those
// `wrap` says; only the rows shown are built.
pub fn wrapped_rows<'a>(
    lines: impl Iterator<Item = (usize, Line<'a>)>,
    screen: Range<usize>,
    wrap: impl Fn(usize) -> bool,
    area: Rect,
) -> Vec<(usize, Line<'static>)> {
    let width = area.width as usize;
    lines
        .flat_map(|(row, line)| {
            let wrapped = if wrap(row) {
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

    // A file git names quoted, for a quote, a backslash or a tab in its name, is shown by its
    // name as it is: its separator used to show the whole "diff --git" line.
    #[test]
    fn test_file_separator_of_a_file_git_quotes() {
        let file = |header: &str| format!("{header}\n@@ -1 +1 @@\n-a\n+b\n");
        let raw = [
            file("diff --git \"a/say \\\"hi\\\".txt\" \"b/say \\\"hi\\\".txt\""),
            file("diff --git a/plain \"b/tab\\there\""),
            file("diff --git a/x.rs b/x.rs"),
        ]
        .concat();
        let diff = parse_diff(&raw);
        let names: Vec<String> = diff
            .rows
            .iter()
            .filter(|r| r.kind == DiffRowKind::File)
            .map(|r| diff.row_text(r).into_owned())
            .collect();
        assert_eq!(names, ["say \"hi\".txt", "tab    here", "x.rs"]);
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
            .filter(|&row| cursor.row_mark(&diff, DiffPart::Hunk, row).is_some())
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
                .filter(|&row| cursor.row_mark(&diff, DiffPart::Line, row).is_some())
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
        let matches = matching_rows(&diff, "a", SearchOptions::default());
        cursor.search(&diff, &matches, 0, SearchDirection::Next);
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
        let visual = wrapped_rows(lines, 5..9, |_| true, Rect::new(0, 0, 7, 4));
        let rows: Vec<usize> = visual.iter().map(|(row, _)| *row).collect();
        assert_eq!(rows, [1, 2, 2, 2]);
        let lines = (0..3).map(|i| (i, line("abcdefghijkl")));
        let visual = wrapped_rows(lines, 0..4, |_| false, Rect::new(0, 0, 7, 4));
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

    // Lines too long to diff quickly by their words, like minified code, aren't marked.
    #[test]
    fn test_long_lines_unmarked() {
        let line = |end: &str| format!("{}{end}", "w ".repeat(600));
        let diff = parse_diff(&format!(
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-{}\n+{}\n",
            line("a"),
            line("b"),
        ));
        assert!(diff.rows.iter().all(|r| diff.row_emphasis(r).is_empty()));
    }

    // A diff that fits the panel still moves the line to a change: only the window stays put.
    // ctrl-e keeps the selected line out of the `scrolloff` rows at the top, as in a list or a
    // file's text: it used to ride on the top row.
    #[test]
    fn test_scroll_line_keeps_the_scrolloff() {
        let mut raw = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -0,0 +1,40 @@\n".to_string();
        for i in 0..40 {
            raw.push_str(&format!("+{i}\n"));
        }
        let diff = parse_diff(&raw);
        let mut cursor = DiffCursor::default();
        cursor.scroll.height = 10;
        cursor.scroll.scrolloff = 3;
        let theme = ColorTheme::default();
        cursor.set_wrap_map(WrapMap::of(&diff, &theme, 80, false, DiffLayout::Unified));
        cursor.select_line(&diff, 1);
        cursor.scroll.offset = 0;
        for _ in 0..5 {
            cursor.scroll.handle_scroll(UserEvent::ScrollDown);
            cursor.keep_in_view(&diff, DiffPart::Line);
        }
        assert_eq!(cursor.scroll.offset, 5);
        assert_eq!(cursor.line, 5 + 3);
        // and at the bottom, scrolled back up
        cursor.select_line(&diff, 5 + 9);
        cursor.scroll.offset = 5;
        cursor.scroll.handle_scroll(UserEvent::ScrollUp);
        cursor.keep_in_view(&diff, DiffPart::Line);
        assert_eq!(cursor.line, 4 + 9 - 3);
    }

    // ctrl-e and ctrl-y moved the selected line out of the scrolloff rows at an edge the diff
    // can't scroll past, though nothing scrolled, like vim doesn't.
    #[test]
    fn test_scroll_line_at_an_edge_leaves_the_selection() {
        for lines in [40, 4] {
            let mut raw = format!("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -0,0 +1,{lines} @@\n");
            for i in 0..lines {
                raw.push_str(&format!("+{i}\n"));
            }
            let diff = parse_diff(&raw);
            let mut cursor = DiffCursor::default();
            cursor.scroll.height = 10;
            cursor.scroll.scrolloff = 3;
            let theme = ColorTheme::default();
            cursor.set_wrap_map(WrapMap::of(&diff, &theme, 80, false, DiffLayout::Unified));
            let first = cursor.selectable_rows(&diff).next().unwrap();
            cursor.select_line(&diff, first);
            cursor.scroll.offset = 0;
            for event in [UserEvent::ScrollUp, UserEvent::ScrollDown] {
                cursor.scroll.handle_scroll(event);
                cursor.keep_in_view(&diff, DiffPart::Line);
                if lines == 4 || event == UserEvent::ScrollUp {
                    assert_eq!(cursor.line, first, "{lines} lines, {event:?}");
                }
            }
        }
    }

    // The lines of a diff without hunks, like a binary file's, draw a name as every row does: a
    // byte that isn't UTF-8 as U+FFFD, not the character keeping it, and a tab as spaces.
    #[test]
    fn test_hunkless_lines_drawn_like_the_rows() {
        let raw = crate::git::text_from_git(
            b"diff --git a/caf\xe9.bin b/caf\xe9.bin\nBinary files a/caf\xe9.bin and b/caf\xe9.bin differ\n",
        );
        let diff = parse_diff(&raw);
        let theme = ColorTheme::default();
        let text: String = diff_lines(&diff, &theme, 0..10, 80)
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.to_string()))
            .collect();
        assert!(
            text.contains("caf\u{FFFD}.bin") && !text.contains('\u{F7E9}'),
            "{text:?}"
        );
    }

    // A folder's diff shows a conflicted file, which git gives as a combined diff (`diff --cc`)
    // first, by its name, and counts it: it was left out, the next file shown as the only one,
    // without its name.
    #[test]
    fn test_combined_diff_of_a_conflicted_file_is_named() {
        let raw = "diff --cc a.txt\nindex 1,2..0\n--- a/a.txt\n+++ b/a.txt\n@@@ -1,1 -1,1 +1,5 @@@\n++<<<<<<< HEAD\n\
diff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-x\n+y\n";
        let diff = parse_diff(raw);
        assert!(!diff.is_single_file());
        let files: Vec<String> = diff
            .rows
            .iter()
            .filter(|row| row.kind == DiffRowKind::File)
            .map(|row| diff.row_text(row).into_owned())
            .collect();
        assert_eq!(files, ["a.txt", "b.txt"]);
        assert_eq!(diff.hunks.len(), 1);
    }

    // Lines are picked like the rows of a list (`Picks`): moving back unpicks what the visual
    // selection went over, and v v on a picked line unpicks it. Lines only used to add up.
    #[test]
    fn test_lines_picked_like_the_rows_of_a_list() {
        let diff =
            parse_diff("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -0,0 +1,4 @@\n+a\n+b\n+c\n+d\n");
        let mut cursor = DiffCursor::default();
        cursor.select_line(&diff, 1);
        cursor.toggle_range();
        cursor.move_line(&diff, Step::Next);
        cursor.move_line(&diff, Step::Next);
        cursor.move_line(&diff, Step::Prev);
        assert_eq!(cursor.line_rows(), [1, 2]);
        // ended, then v v on the picked b
        cursor.toggle_range();
        cursor.toggle_range();
        cursor.toggle_range();
        assert_eq!(cursor.line_rows(), [1]);
    }

    // ctrl-d in a diff pages like vim and like a list: the view moves half a page, and the
    // selection takes the changed line, or the hunk, at the row it lands on (else the next one).
    // The diff used to pick the next one first, the view following only as far as it had to.
    // While lines are picked, which stay in one hunk, ctrl-d past its last line moved the window
    // to the end of the diff, the selection left off screen.
    #[test]
    fn test_page_with_lines_picked_keeps_them_shown() {
        let mut raw = "diff --git a/f b/f\n--- a/f\n+++ b/f\n".to_string();
        for at in [1, 21, 41, 61, 81] {
            raw.push_str(&format!("@@ -{at},0 +{at},10 @@\n"));
            for i in 0..10 {
                raw.push_str(&format!("+{i}\n"));
            }
        }
        let diff = parse_diff(&raw);
        let mut cursor = DiffCursor::default();
        cursor.scroll.height = 10;
        let theme = ColorTheme::default();
        cursor.set_wrap_map(WrapMap::of(&diff, &theme, 80, false, DiffLayout::Unified));
        let first = cursor.selectable_rows(&diff).next().unwrap();
        cursor.select_line(&diff, first);
        cursor.toggle_range();
        for _ in 0..4 {
            cursor.jump_page(&diff, UserEvent::HalfPageDown, DiffPart::Line);
            let line = cursor.wrap.visual_range(cursor.line..cursor.line + 1);
            let offset = cursor.scroll.offset;
            assert!(
                offset <= line.start && line.end <= offset + 10,
                "{offset} {line:?}"
            );
        }
    }

    #[test]
    fn test_half_page_in_a_diff_like_vim() {
        let theme = ColorTheme::default();
        let cursor_of = |diff: &ParsedDiff, height: usize| {
            let mut cursor = DiffCursor::default();
            cursor.scroll.height = height;
            cursor.set_wrap_map(WrapMap::of(diff, &theme, 80, false, DiffLayout::Unified));
            cursor
        };
        // every line changed: the line keeps its row on screen, as in a list
        let mut raw = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -0,0 +1,40 @@\n".to_string();
        for i in 0..40 {
            raw.push_str(&format!("+{i}\n"));
        }
        let diff = parse_diff(&raw);
        let mut cursor = cursor_of(&diff, 10);
        cursor.select_line(&diff, 3);
        cursor.scroll.offset = 0;
        let mut pages = vec![];
        for _ in 0..3 {
            cursor.jump_page(&diff, UserEvent::HalfPageDown, DiffPart::Line);
            pages.push((cursor.scroll.offset, cursor.line));
        }
        assert_eq!(pages, [(5, 8), (10, 13), (15, 18)]);

        // small hunks: the one at the row landed on, the view moving half a page, then stopping
        // at the end while the selection goes on
        let mut raw = "diff --git a/f b/f\n--- a/f\n+++ b/f\n".to_string();
        for at in [1, 20, 40, 60, 80, 100] {
            raw.push_str(&format!("@@ -{at},5 +{at},5 @@\n a\n-b\n+B\n c\n d\n"));
        }
        let diff = parse_diff(&raw);
        let mut cursor = cursor_of(&diff, 20);
        cursor.select_hunk(&diff, 0);
        let mut pages = vec![];
        for _ in 0..4 {
            cursor.jump_page(&diff, UserEvent::HalfPageDown, DiffPart::Hunk);
            pages.push((cursor.scroll.offset, cursor.hunk));
        }
        assert_eq!(pages, [(10, 1), (16, 3), (16, 4), (16, 5)]);
    }

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

    // A file git names quoted, for a quote, a backslash or a tab in its name, used to make a
    // patch without its path: "git diff header lacks filename information".
    #[test]
    fn test_partial_patch_of_a_file_git_quotes() {
        use crate::git::git_command;
        use crate::test_support::git::{git, GitRepository};
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        repo.commit("init", "2024-01-01");
        let apply = |patch: &str| {
            let mut child = git_command()
                .args(["apply", "--cached", "--reverse", "-"])
                .current_dir(path)
                .stdin(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(patch.as_bytes())
                .unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success(), "{patch}\n{output:?}");
        };
        let staged = |names: &[&str]| {
            let mut args = vec!["-c", "core.quotePath=false", "diff", "--cached", "-M", "--"];
            args.extend(names);
            parse_diff(&git(path, &args))
        };
        for name in ["say \"hi\".txt", "back\\slash.txt", "tab\there.txt"] {
            // a new file: unstaging one of its lines keeps the other staged
            std::fs::write(path.join(name), "a\nb\n").unwrap();
            git(path, &["add", "--", name]);
            let diff = staged(&[name]);
            assert_eq!(diff.hunks[0].path(), name);
            apply(&diff.hunks[0].partial_patch(&[1], PatchDirection::Reverse));
            assert_eq!(git(path, &["show", &format!(":{name}")]), "a\n");
            git(path, &["commit", "-qm", "add", "--", name]);

            // a renamed file: a line of it unstaged under its new name
            let moved = format!("moved {name}");
            git(path, &["mv", "--", name, &moved]);
            std::fs::write(path.join(&moved), "a\nc\n").unwrap();
            git(path, &["add", "--", &moved]);
            let diff = staged(&[name, &moved]);
            assert!(diff.hunks[0].file_header().contains("rename from"));
            assert_eq!(diff.hunks[0].path(), moved);
            apply(&diff.hunks[0].partial_patch(&[2], PatchDirection::Reverse));
            assert_eq!(git(path, &["show", &format!(":{moved}")]), "a\n");
            git(path, &["reset", "-q"]);
        }
    }

    // A big diff is searched on several threads, a part each, its matches in order as on one.
    #[test]
    fn test_matching_rows_of_a_big_diff_in_order() {
        let mut raw = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -0,0 +1,50000 @@\n".to_string();
        for i in 0..50_000 {
            raw += &format!("+{}\n", if i % 3 == 0 { "hit" } else { "miss" });
        }
        let diff = parse_diff(&raw);
        let options = SearchOptions::default();
        let one_by_one: Vec<usize> = (0..diff.rows.len())
            .filter(|&row| row_matches(&diff, row, "hit", options))
            .collect();
        assert_eq!(one_by_one.len(), 16_667);
        assert_eq!(matching_rows(&diff, "hit", options), one_by_one);
    }

    // Parsing stops once nobody waits for the diff any more, see `Job::read`.
    #[test]
    fn test_parse_stops_once_cancelled() {
        let mut raw = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n".to_string();
        raw += "-old line\n+new line\n";
        assert_eq!(parse_diff(&raw).rows.len(), 3);
        assert!(parse_diff_until(&raw, &Cancel::cancelled()).rows.is_empty());
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
        // wrapped near row 6 only, see `rewrap`: its pair as tall as when all are, the rest a
        // screen row each
        let theme = ColorTheme::default();
        let near = WrapMap::of_until(
            &diff,
            &theme,
            16,
            true,
            DiffLayout::Split,
            6..7,
            &Cancel::default(),
        );
        let all = WrapMap::of(&diff, &theme, 16, true, DiffLayout::Split);
        assert!(near.wraps_row(3) && !near.wraps_row(2) && !near.is_measured());
        assert!(all.is_measured());
        let height = |map: &WrapMap| map.visual_range(6..7).len();
        assert!(height(&all) > 1);
        assert_eq!(height(&near), height(&all));
        assert_eq!(near.total(), 6 + height(&all));
    }

    // A "\ No newline at end of file" note isn't a line: nothing beside it is striped, on either
    // side, while an added line beside nothing still is.
    #[test]
    fn test_split_no_stripes_beside_a_no_newline_note() {
        let theme = ColorTheme::default();
        for (body, note_on_old) in [
            (
                "@@ -1 +1,2 @@\n-last\n\\ No newline at end of file\n+last\n+new\n",
                true,
            ),
            (
                "@@ -1 +1 @@\n-last\n+last\n\\ No newline at end of file\n",
                false,
            ),
        ] {
            let diff = parse_diff(&format!("diff --git a/f b/f\n--- a/f\n+++ b/f\n{body}"));
            let map = WrapMap::of(&diff, &theme, 80, true, DiffLayout::Split);
            let area = Rect::new(0, 0, 80, map.total() as u16);
            let mut buf = Buffer::empty(area);
            render_split_rows(
                &diff,
                &map,
                0..map.total(),
                (|_| None, |_| false),
                |_| {},
                &theme,
                area,
                &mut buf,
            );
            let (old_width, _) = split_widths(80);
            let side = |y: u16, x: std::ops::Range<u16>| -> String {
                x.map(|x| buf[(x, y)].symbol().to_string()).collect()
            };
            let rows: Vec<_> = (0..area.height)
                .map(|y| (side(y, 0..old_width), side(y, old_width + 1..80)))
                .collect();
            let (_, beside) = rows
                .iter()
                .map(|(old, new)| if note_on_old { (old, new) } else { (new, old) })
                .find(|(note, _)| note.contains("No newline"))
                .unwrap();
            assert_eq!(beside.trim(), "", "{rows:#?}");
            if note_on_old {
                // "+new" beside nothing
                let new = rows.iter().find(|(_, new)| new.contains("new")).unwrap();
                assert!(new.0.contains('╱'), "{rows:#?}");
            }
        }
    }

    // The side with no line beside one added or removed is striped; the rows past a line shorter
    // than its wrapped pair stay blank.
    #[test]
    fn test_split_filler_marks_only_missing_lines() {
        let diff = parse_diff(concat!(
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,3 +1,2 @@\n ctx\n-gone\n",
            "-foo bar baz\n+foo bar baz qux quux\n",
        ));
        let theme = ColorTheme::default();
        let map = WrapMap::of(&diff, &theme, 40, true, DiffLayout::Split);
        let area = Rect::new(0, 0, 40, map.total() as u16);
        let mut buf = Buffer::empty(area);
        render_split_rows(
            &diff,
            &map,
            0..map.total(),
            (|_| None, |_| false),
            |_| {},
            &theme,
            area,
            &mut buf,
        );
        let (old_width, _) = split_widths(40);
        let side = |y: u16, x: std::ops::Range<u16>| -> String {
            x.map(|x| buf[(x, y)].symbol().to_string()).collect()
        };
        let old = |y| side(y, 0..old_width);
        let new = |y| side(y, old_width + 1..40);
        let rows: Vec<_> = (0..area.height).map(|y| (old(y), new(y))).collect();
        let stripes = "╱".repeat((40 - old_width - 1) as usize);
        // header, ctx, "gone" beside stripes, "foo bar baz qux quux" over 2+ rows
        assert!(
            rows[2].0.contains("gone") && rows[2].1 == stripes,
            "{rows:#?}"
        );
        assert_eq!(buf[(old_width + 1, 2)].bg, theme.diff_filler_bg);
        assert!(rows.len() > 4, "{rows:#?}");
        assert!(
            rows[4..].iter().all(|(old, _)| old.trim().is_empty()),
            "{rows:#?}"
        );
        assert!(!rows.iter().any(|(old, _)| old.contains('╱')), "{rows:#?}");
    }

    #[test]
    fn test_wrap_map_makes_no_lines_unwrapped() {
        // each row its own screen row, the lines never made
        let map = WrapMap::from_lines(3, ALL_ROWS, |_| -> Vec<Line> { panic!("made") }, 80, false);
        assert_eq!(map.total(), 3);
        assert_eq!(map.visual_range(1..2), 1..2);
        assert_eq!(map.logical_rows(1..5), 1..3);
        assert_eq!(map.logical_at(2), 2);
        // while wrapping they are measured
        let lines = || vec![Line::raw("a".repeat(25)), Line::raw("b")];
        let map = WrapMap::from_lines(2, ALL_ROWS, |_| lines(), 10, true);
        assert_eq!(map.total(), 4);
        assert_eq!(map.visual_range(1..2), 3..4);
        assert!(map.is_measured());
        // only those asked, see `rewrap`: the others a screen row each, drawn as they are
        let map = WrapMap::from_lines(2, 1..2, |range| lines()[range].to_vec(), 10, true);
        assert_eq!(map.total(), 2);
        assert!(map.wraps_row(1) && !map.wraps_row(0) && !map.is_measured());
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
        cursor.search(
            &diff,
            &matching_rows(&diff, "dos", options),
            0,
            SearchDirection::Next,
        );
        assert!(cursor.has_range());
        cursor.search(
            &diff,
            &matching_rows(&diff, "diez", options),
            0,
            SearchDirection::Next,
        );
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
