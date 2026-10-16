use std::{
    ops::Range,
    rc::Rc,
    time::{Duration, Instant},
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
    git::Repository,
    widget::{
        file_tree::SearchDirection,
        style::{divider, mark_selected_bar, placeholder, ScrollState},
        tree_query::{find_around, match_range, patch_range},
    },
};

// Why the hunks of a file aren't selectable, see `ParsedDiff::drop_converted`.
pub const CONVERTED_DIFF: &str =
    "This diff is text converted from the file (textconv): its hunks and lines can't be staged or applied, only the whole file";

#[derive(Debug, Default)]
pub struct ParsedDiff {
    // shared with the hunks, which are parts of it
    pub raw: Rc<str>,
    pub rows: Vec<DiffRow>,
    pub hunks: Vec<DiffHunk>,
    // hunks were left out, of files diffed as text converted from them
    pub converted: bool,
    // the width of the line numbers, of the biggest one
    number_width: usize,
}

impl ParsedDiff {
    // Leaves out the hunks of files git diffs as text converted from them, like a .docx with a
    // textconv driver: the text isn't in the file, so they can't be staged or applied.
    pub fn drop_converted(&mut self, repository: &Repository) {
        let mut paths: Vec<&str> = self.hunks.iter().map(DiffHunk::path).collect();
        paths.dedup();
        let converted = repository.converted_paths(&paths);
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
    diff: Rc<str>,
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
    // The patch is applied to the index: forward to stage, or `reverse` to unstage.
    // Unselected changes must leave the index as is, so they become context on the side that is
    // in the index (removed lines when staging, added lines when unstaging) and are dropped from the other.
    pub fn partial_patch(&self, selected: &[usize], reverse: bool) -> String {
        let mut body = String::new();
        let (mut old_count, mut new_count) = (0, 0);
        // whether the previous line was kept, so its "\ No newline at end of file" marker is too
        let mut kept = true;

        for (i, line) in self.lines().enumerate() {
            let as_context = || format!(" {}", &line[1..]);
            match line.as_bytes().first() {
                Some(b'+') if selected.contains(&i) => {
                    body.push_str(line);
                    new_count += 1;
                    kept = true;
                }
                Some(b'-') if selected.contains(&i) => {
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

// The selected hunk, or changed line, of a diff and its scroll offset; the local changes and the
// commit changes select the same way.
#[derive(Debug, Default, Clone)]
pub struct DiffCursor {
    pub hunk: usize,
    // a row of `ParsedDiff::rows`, a change of the hunk
    pub line: usize,
    pub scroll: ScrollState,
    // the row the search last matched, where the next one starts from
    search_row: Option<usize>,
}

impl DiffCursor {
    // Back to the top of another diff.
    pub fn reset(&mut self) {
        let height = self.scroll.height;
        *self = DiffCursor::default();
        self.scroll.height = height;
    }

    // Selects a hunk, the last one past the end, showing it; false when there is none.
    pub fn select_hunk(&mut self, diff: &ParsedDiff, index: usize) -> bool {
        let Some(last) = diff.hunks.len().checked_sub(1) else {
            return false;
        };
        self.hunk = index.min(last);
        // show the whole hunk, or at least its beginning
        self.scroll_to(diff.hunks[self.hunk].rows.clone());
        true
    }

    // Selects a row, in the hunk containing it.
    pub fn select_line(&mut self, diff: &ParsedDiff, row: usize) {
        self.line = row;
        if let Some(index) = diff.hunks.iter().position(|h| h.rows.contains(&row)) {
            self.hunk = index;
        }
        self.scroll_to(row..row + 1);
    }

    // Selects the first change of the selected hunk; false when it has none.
    pub fn enter_lines(&mut self, diff: &ParsedDiff) -> bool {
        let Some(hunk) = diff.hunks.get(self.hunk) else {
            return false;
        };
        match hunk.rows.clone().find(|&r| diff.rows[r].is_change()) {
            Some(row) => {
                self.select_line(diff, row);
                true
            }
            None => false,
        }
    }

    // Selects the row matching `query` in `direction` from the last match, else from the selection:
    // the changed line it is on, else its hunk. Returns whether that is a changed line, None when
    // nothing matches.
    pub fn search(
        &mut self,
        diff: &ParsedDiff,
        query: &str,
        direction: SearchDirection,
        lines: bool,
    ) -> Option<bool> {
        let selected = if lines {
            self.line
        } else {
            diff.hunks.get(self.hunk).map_or(0, |h| h.rows.start)
        };
        let start = self.search_row.unwrap_or(selected);
        let row = find_around(diff.rows.len(), start, direction, |r| {
            match_range(&diff.rows[r].text, query).is_some()
        })?;
        let changed = diff.rows[row].is_change();
        if changed {
            self.select_line(diff, row);
        } else {
            if let Some(index) = diff.hunks.iter().position(|h| h.rows.contains(&row)) {
                self.hunk = index;
            }
            self.scroll_to(row..row + 1);
        }
        self.search_row = Some(row);
        Some(changed)
    }

    pub fn move_hunk(&mut self, diff: &ParsedDiff, step: Step) {
        self.search_row = None;
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
        self.search_row = None;
        let mut changed = changed_rows(diff);
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

    // Selects the changed line at or after `row`, else the last one; for a diff reloaded after
    // the line was staged.
    pub fn select_line_near(&mut self, diff: &ParsedDiff, row: usize) -> bool {
        let row = changed_rows(diff)
            .find(|&r| r >= row)
            .or_else(|| changed_rows(diff).last());
        if let Some(row) = row {
            self.select_line(diff, row);
        }
        row.is_some()
    }

    pub fn scroll_to(&mut self, rows: Range<usize>) {
        let ScrollState { offset, height } = &mut self.scroll;
        // before the first render the height is unknown
        if *height == 0 || rows.start < *offset || rows.len() > *height {
            *offset = rows.start;
        } else if rows.end > *offset + *height {
            *offset = rows.end - *height;
        }
    }

    // The rows highlighted: the selected line, else the selected hunk.
    pub fn highlighted(&self, diff: &ParsedDiff, lines: bool) -> Range<usize> {
        if lines {
            self.line..self.line + 1
        } else {
            diff.hunks.get(self.hunk).map_or(0..0, |h| h.rows.clone())
        }
    }

    pub fn selected_hunk<'d>(&self, diff: &'d ParsedDiff) -> Option<&'d DiffHunk> {
        diff.hunks.get(self.hunk)
    }

    // A patch of the selected line alone, see `DiffHunk::partial_patch`.
    pub fn line_patch(&self, diff: &ParsedDiff, reverse: bool) -> Option<String> {
        let hunk = self.selected_hunk(diff)?;
        // the hunk header occupies the first row, its lines follow
        let line = self.line.checked_sub(hunk.rows.start + 1)?;
        Some(hunk.partial_patch(&[line], reverse))
    }

    // The text of the selected line as in the file, without its +/- and line ending.
    pub fn line_text(&self, diff: &ParsedDiff) -> Option<String> {
        let hunk = self.selected_hunk(diff)?;
        let line = hunk
            .lines()
            .nth(self.line.checked_sub(hunk.rows.start + 1)?)?;
        Some(line.get(1..)?.trim_end_matches(['\n', '\r']).to_string())
    }

    // The selected line as in the file while selecting `lines`, else the selected hunk as a diff,
    // to copy.
    pub fn selected_text(&self, diff: &ParsedDiff, lines: bool) -> Option<String> {
        if lines {
            self.line_text(diff)
        } else {
            self.selected_hunk(diff).map(DiffHunk::text)
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

#[derive(Debug, PartialEq, Eq)]
pub enum DiffRowKind {
    File,
    Hunk,
    Context,
    Added,
    Removed,
    Meta,
}

#[derive(Debug, PartialEq, Eq)]
pub struct DiffRow {
    pub kind: DiffRowKind,
    pub old: Option<usize>,
    pub new: Option<usize>,
    pub text: String,
    // the bytes of `text` that differ from the line it replaces, or that replaces it
    pub emphasis: Vec<Range<usize>>,
}

impl DiffRow {
    pub fn is_change(&self) -> bool {
        matches!(self.kind, DiffRowKind::Added | DiffRowKind::Removed)
    }

    fn without_numbers(kind: DiffRowKind, text: &str) -> Self {
        Self {
            kind,
            old: None,
            new: None,
            text: text.to_string(),
            emphasis: Vec::new(),
        }
    }
}

pub fn parse_diff(diff: &str) -> ParsedDiff {
    let shared: Rc<str> = Rc::from(diff);
    // file names are only shown when the diff spans multiple files (a directory)
    let multiple_files = diff
        .lines()
        .filter(|l| l.starts_with("diff --git "))
        .count()
        > 1;

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

        if line.starts_with("diff --git ") {
            finish_hunk(&mut current, &mut hunks, rows.len());
            file_start = start;
            if multiple_files {
                let path = line.rsplit(" b/").next().unwrap_or(line);
                rows.push(DiffRow::without_numbers(DiffRowKind::File, path));
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
                rows.push(DiffRow::without_numbers(DiffRowKind::Hunk, line));
                continue;
            }
        }
        let Some(hunk) = current.as_mut() else {
            // part of the file header
            continue;
        };
        hunk.body.end = end;

        let text = line.get(1..).unwrap_or_default().replace('\t', "    ");
        let row = match line.as_bytes().first() {
            Some(b'+') => {
                new += 1;
                DiffRow {
                    kind: DiffRowKind::Added,
                    old: None,
                    new: Some(new - 1),
                    text,
                    emphasis: Vec::new(),
                }
            }
            Some(b'-') => {
                old += 1;
                DiffRow {
                    kind: DiffRowKind::Removed,
                    old: Some(old - 1),
                    new: None,
                    text,
                    emphasis: Vec::new(),
                }
            }
            Some(b'\\') => DiffRow::without_numbers(DiffRowKind::Meta, line),
            _ => {
                old += 1;
                new += 1;
                DiffRow {
                    kind: DiffRowKind::Context,
                    old: Some(old - 1),
                    new: Some(new - 1),
                    text,
                    emphasis: Vec::new(),
                }
            }
        };
        rows.push(row);
    }
    finish_hunk(&mut current, &mut hunks, rows.len());
    emphasize_changes(&mut rows);
    let max_line = rows
        .iter()
        .flat_map(|r| [r.old, r.new])
        .flatten()
        .max()
        .unwrap_or(0);

    ParsedDiff {
        raw: shared,
        rows,
        hunks,
        converted: false,
        number_width: max_line.to_string().len().max(3),
    }
}

// Pairs the removed lines of a change with the added lines after them, in order, and marks in
// each pair the words that differ between them.
fn emphasize_changes(rows: &mut [DiffRow]) {
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
        for (&r, &a) in removed.iter().zip(&added) {
            (rows[r].emphasis, rows[a].emphasis) = changed_ranges(&rows[r].text, &rows[a].text);
        }
        i = end;
    }
}

// The bytes of the words of `old` and `new` that differ, by a diff of their words; none when the
// lines are mostly different, or alike only in blanks.
fn changed_ranges(old: &str, new: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let (old_words, new_words) = (words(old), words(new));
    // a line too long to diff in time is marked coarser, not waited for
    let deadline = Instant::now() + Duration::from_millis(10);
    let ops = similar::capture_diff_slices_deadline(
        similar::Algorithm::Myers,
        &old_words,
        &new_words,
        Some(deadline),
    );
    // how alike the lines are, by their text past blanks: unmarked under two fifths
    let text = |words: &[&str]| -> usize { words.iter().map(|w| w.trim().len()).sum() };
    let kept: usize = ops
        .iter()
        .map(|op| op.as_tag_tuple())
        .filter(|(tag, ..)| *tag == DiffTag::Equal)
        .map(|(_, words, _)| text(&old_words[words]))
        .sum();
    if 5 * kept < text(&old_words) + text(&new_words) {
        return (Vec::new(), Vec::new());
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
    let (old_starts, new_starts) = (starts(&old_words), starts(&new_words));
    let mut marks = (Vec::new(), Vec::new());
    for op in &ops {
        let (tag, o, n) = op.as_tag_tuple();
        if tag != DiffTag::Equal {
            push_range(&mut marks.0, old_starts[o.start]..old_starts[o.end]);
            push_range(&mut marks.1, new_starts[n.start]..new_starts[n.end]);
        }
    }
    marks
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
    let dim = theme.muted_fg;
    if diff.raw.is_empty() {
        // e.g. a directory of untracked files, which `git diff` does not show
        return vec![placeholder("No diff to show", theme)];
    }
    if diff.rows.is_empty() {
        return hunkless_lines(diff)
            .skip(range.start)
            .take(range.len())
            .map(|l| Line::raw(l).fg(dim))
            .collect();
    }

    let width = diff.number_width;
    let number = |n: Option<usize>| n.map_or(" ".repeat(width), |n| format!("{n:>width$}"));

    diff.rows
        .iter()
        .skip(range.start)
        .take(range.len())
        .map(|row| {
            if row.kind == DiffRowKind::File {
                let path = format!("{} ", row.text);
                let fill =
                    (panel_width as usize).saturating_sub(3 + Span::raw(path.as_str()).width());
                return Line::from(vec![
                    Span::raw("── ").fg(theme.divider_fg),
                    Span::raw(path).fg(theme.detail_file_change_modify_fg),
                    divider(fill, theme),
                ]);
            }
            let gutter = format!("{} {} ", number(row.old), number(row.new));
            let text_color = match row.kind {
                DiffRowKind::Hunk => theme.diff_hunk_fg,
                DiffRowKind::Added | DiffRowKind::Removed | DiffRowKind::Context => theme.fg,
                DiffRowKind::Meta | DiffRowKind::File => dim,
            };
            let mut line = Line::from(vec![
                Span::raw(gutter).fg(dim),
                Span::raw(row.text.as_str()).fg(text_color),
            ]);
            // added and removed lines on a background across the panel, what changed in them
            // on a stronger one
            let (bg, word_bg) = match row.kind {
                DiffRowKind::Added => (theme.diff_add_bg, theme.diff_add_word_bg),
                DiffRowKind::Removed => (theme.diff_delete_bg, theme.diff_delete_word_bg),
                _ => return line,
            };
            for range in &row.emphasis {
                patch_range(&mut line, range.clone(), Style::default().bg(word_bg));
            }
            line.bg(bg)
        })
        .collect()
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

// Renders the lines of `diff_lines` for `rows`, a line's background (added, removed) across
// `area`, the `selected` rows marked on their line numbers and by the bar left of `area`;
// long lines wrapped, when `wrap`.
pub fn render_diff_lines(
    lines: Vec<Line>,
    rows: Range<usize>,
    selected: Range<usize>,
    wrap: bool,
    theme: &ColorTheme,
    area: Rect,
    buf: &mut Buffer,
) {
    let lines = rows.zip(lines).map(|(row, mut line)| {
        if selected.contains(&row) {
            select_diff_line(&mut line, theme);
        }
        (row, line)
    });
    let visual = visual_rows(lines, wrap, &selected, area);
    for (y, (_, line)) in (area.y..).zip(&visual) {
        if let Some(bg) = line.style.bg {
            buf.set_style(Rect::new(area.x, y, area.width, 1), Style::default().bg(bg));
        }
    }
    render_visual_rows(visual, &selected, theme, area, buf);
}

// The screen rows showing `lines` (a row of the text each) in `area`: wrapped at its width when
// `wrap`, and starting further down when wrapping pushed the `selected` rows out of it.
pub fn visual_rows<'a>(
    lines: impl Iterator<Item = (usize, Line<'a>)>,
    wrap: bool,
    selected: &Range<usize>,
    area: Rect,
) -> Vec<(usize, Line<'a>)> {
    let width = area.width as usize;
    let mut visual: Vec<(usize, Line<'a>)> = Vec::new();
    for (row, line) in lines {
        if wrap {
            visual.extend(wrap_line(line, width).into_iter().map(|l| (row, l)));
        } else {
            visual.push((row, line));
        }
    }
    let height = area.height as usize;
    let first = visual.iter().position(|(row, _)| selected.contains(row));
    let last = visual.iter().rposition(|(row, _)| selected.contains(row));
    if let (Some(first), Some(last)) = (first, last) {
        let skip = (last + 1).saturating_sub(height).min(first);
        visual.drain(..skip);
    }
    visual.truncate(height);
    visual
}

// Draws the rows of `visual_rows`, with the bar left of `area` beside the `selected` ones.
pub fn render_visual_rows(
    visual: Vec<(usize, Line)>,
    selected: &Range<usize>,
    theme: &ColorTheme,
    area: Rect,
    buf: &mut Buffer,
) {
    let selected_ys: Vec<u16> = (area.y..)
        .zip(&visual)
        .filter(|(_, (row, _))| selected.contains(row))
        .map(|(y, _)| y)
        .collect();
    let lines: Vec<Line> = visual.into_iter().map(|(_, line)| line).collect();
    Paragraph::new(lines).fg(theme.fg).render(area, buf);
    if area.x > 0 {
        for y in selected_ys {
            mark_selected_bar(buf, area.x - 1, y, true, theme);
        }
    }
}

// `line` in rows of `width` columns, the rows after the first indented past its first span, the
// line numbers, in their style, and keeping the line's style (the background of a change).
fn wrap_line(line: Line<'_>, width: usize) -> Vec<Line<'static>> {
    let indent = if line.spans.len() > 1 {
        line.spans[0].width()
    } else {
        0
    };
    if width <= indent + 1 {
        return vec![owned(line)];
    }
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
            if used + w > width && used > indent {
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

    #[test]
    fn test_parse_diff() {
        let header = "diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n";
        let hunk1 = "@@ -3,2 +3,2 @@ ctx\n keep\n-old\n+new\n";
        let hunk2 = "@@ -20 +20 @@\n-a\r\n+b\r\n";
        let actual = parse_diff(&format!("{header}{hunk1}{hunk2}"));

        let row = |kind, old, new, text: &str| DiffRow {
            kind,
            old,
            new,
            text: text.into(),
            emphasis: Vec::new(),
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
        assert_eq!(actual.rows, expected_rows);

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

        // the selected row kept in view below the rows wrapping above it
        let lines = (0..3).map(|i| (i, line("abcdefghijkl")));
        let visual = visual_rows(lines, true, &(2..3), Rect::new(0, 0, 7, 4));
        let rows: Vec<usize> = visual.iter().map(|(row, _)| *row).collect();
        assert_eq!(rows, [1, 2, 2, 2]);
        let lines = (0..3).map(|i| (i, line("abcdefghijkl")));
        let visual = visual_rows(lines, false, &(2..3), Rect::new(0, 0, 7, 4));
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
        let emphasis: Vec<_> = diff
            .rows
            .iter()
            .map(|r| (r.text.as_str(), r.emphasis.clone()))
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

    #[test]
    fn test_partial_patch() {
        let file_header = "diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n";
        let hunk = "@@ -1,3 +1,3 @@\n a\n-b\n-c\n+B\n+C\n";
        let diff = parse_diff(&format!("{file_header}{hunk}"));
        let hunk = &diff.hunks[0];

        // staging "-b" and "+B": "-c" stays in the index as context, "+C" is left out
        let actual = hunk.partial_patch(&[1, 3], false);
        let expected = format!("{file_header}@@ -1,3 +1,3 @@\n a\n-b\n c\n+B\n");
        assert_eq!(actual, expected);

        // unstaging "+C": "+B" stays in the index as context, "-b" and "-c" are left out
        let actual = hunk.partial_patch(&[4], true);
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
        let actual = hunk.partial_patch(&[0], false);
        let expected = format!("{file_header}@@ -0,0 +1,1 @@\n+a\n");
        assert_eq!(actual, expected);

        // unstaging part of it keeps the file, so it becomes a modification
        let actual = hunk.partial_patch(&[1], true);
        let expected = "diff --git a/f b/f\nindex 0000000..1234567\n--- a/f\n+++ b/f\n@@ -1,1 +1,2 @@\n a\n+b\n";
        assert_eq!(actual, expected);
    }
}
