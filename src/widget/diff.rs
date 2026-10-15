use std::ops::Range;

use ratatui::{
    style::Stylize,
    text::{Line, Span},
};

use crate::color::ColorTheme;

#[derive(Debug, Default)]
pub struct ParsedDiff {
    pub raw: String,
    pub rows: Vec<DiffRow>,
    pub hunks: Vec<DiffHunk>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct DiffHunk {
    // rows of `ParsedDiff::rows` displaying this hunk, the header first and then one row per line
    pub rows: Range<usize>,
    // the "diff --git" line up to the "+++" line of the file this hunk belongs to
    file_header: String,
    // the "@@ ... @@" line
    header: String,
    old_start: usize,
    old_count: usize,
    // with their original line endings, the patch has to match the file byte for byte
    lines: Vec<String>,
}

impl DiffHunk {
    // The file header and this hunk alone, ready for `git apply`.
    pub fn patch(&self) -> String {
        format!("{}{}{}", self.file_header, self.header, self.lines.concat())
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

        for (i, line) in self.lines.iter().enumerate() {
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
        let file_header = partial_file_header(&self.file_header, old_count, new_count);
        format!("{file_header}{header}{body}")
    }
}

// A partially applied patch of an added (or deleted) file keeps lines on the other side too,
// so the file is no longer created (or deleted) by it: turn the header into a modification.
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
    file_header
        .split_inclusive('\n')
        .filter_map(|line| {
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
        }
    }
}

pub fn parse_diff(diff: &str) -> ParsedDiff {
    // file names are only shown when the diff spans multiple files (a directory)
    let multiple_files = diff
        .lines()
        .filter(|l| l.starts_with("diff --git "))
        .count()
        > 1;

    let mut rows = Vec::new();
    let mut hunks = Vec::new();
    let mut file_header = String::new();
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
    for raw in diff.split_inclusive('\n') {
        let line = raw.trim_end_matches(['\n', '\r']);

        if line.starts_with("diff --git ") {
            finish_hunk(&mut current, &mut hunks, rows.len());
            file_header = raw.to_string();
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
                current = Some(DiffHunk {
                    rows: rows.len()..rows.len(),
                    file_header: file_header.clone(),
                    header: raw.to_string(),
                    old_start: o,
                    old_count,
                    lines: Vec::new(),
                });
                rows.push(DiffRow::without_numbers(DiffRowKind::Hunk, line));
                continue;
            }
        }
        let Some(hunk) = current.as_mut() else {
            file_header.push_str(raw);
            continue;
        };
        hunk.lines.push(raw.to_string());

        let text = line.get(1..).unwrap_or_default().replace('\t', "    ");
        let row = match line.as_bytes().first() {
            Some(b'+') => {
                new += 1;
                DiffRow {
                    kind: DiffRowKind::Added,
                    old: None,
                    new: Some(new - 1),
                    text,
                }
            }
            Some(b'-') => {
                old += 1;
                DiffRow {
                    kind: DiffRowKind::Removed,
                    old: Some(old - 1),
                    new: None,
                    text,
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
                }
            }
        };
        rows.push(row);
    }
    finish_hunk(&mut current, &mut hunks, rows.len());

    ParsedDiff {
        raw: diff.to_string(),
        rows,
        hunks,
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
// line numbers of both sides, then the colored text.
pub fn diff_lines<'a>(
    diff: &'a ParsedDiff,
    theme: &ColorTheme,
    range: Range<usize>,
) -> Vec<Line<'a>> {
    let dim = theme.status_input_transient_fg;
    if diff.raw.is_empty() {
        // e.g. a directory of untracked files, which `git diff` does not show
        return vec![Line::raw("No diff to show").fg(dim)];
    }
    if diff.rows.is_empty() {
        return hunkless_lines(diff)
            .skip(range.start)
            .take(range.len())
            .map(|l| Line::raw(l).fg(dim))
            .collect();
    }

    let max_line = diff
        .rows
        .iter()
        .flat_map(|r| [r.old, r.new])
        .flatten()
        .max()
        .unwrap_or(0);
    let width = max_line.to_string().len().max(3);
    let number = |n: Option<usize>| n.map_or(" ".repeat(width), |n| format!("{n:>width$}"));

    diff.rows
        .iter()
        .skip(range.start)
        .take(range.len())
        .map(|row| {
            let gutter = format!("{} {} ", number(row.old), number(row.new));
            let text_color = match row.kind {
                DiffRowKind::Hunk => theme.status_info_fg,
                DiffRowKind::Added => theme.detail_file_change_add_fg,
                DiffRowKind::Removed => theme.detail_file_change_delete_fg,
                DiffRowKind::Context => theme.fg,
                DiffRowKind::Meta => dim,
                DiffRowKind::File => theme.detail_file_change_modify_fg,
            };
            Line::from(vec![
                Span::raw(gutter).fg(dim),
                Span::raw(row.text.as_str()).fg(text_color),
            ])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
