use std::collections::VecDeque;

use crate::git::{ConflictSide, Operation, Repository, SideContent};

// The lines shown around each conflict, from the file.
const CONTEXT: usize = 3;

// What a row of a side shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    // "── conflict 1 of 2 ──"
    Header,
    // a line around the conflicts, the same on every side
    Context,
    // a line of the side as the common ancestor has it, or with no ancestor to compare to
    Kept,
    // a line of the side the common ancestor doesn't have: what that side changed
    Changed,
}

// A row of a side: its line in the file in the working tree (none for headers, the padding that
// keeps the sides' conflicts level, and the sides of a file without markers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictRow {
    pub number: Option<usize>,
    pub text: String,
    pub kind: RowKind,
}

// The sides of a conflicted file, to pick one of them, with the lines around each conflict.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ConflictView {
    // the names git put on the conflict markers, like "HEAD" and the merged branch
    pub ours_label: String,
    pub theirs_label: String,
    pub ours: Vec<ConflictRow>,
    pub theirs: Vec<ConflictRow>,
    // the common ancestor's side, only in the diff3 and zdiff3 conflict styles
    pub base: Option<Vec<ConflictRow>>,
    // 0 for a file without markers, deleted on a side or binary
    pub conflicts: usize,
    // whether the working tree has a file there to open in the editor: not a symlink, which the
    // editor would follow, nor a submodule
    pub editable: bool,
}

impl ConflictView {
    // The conflicting blocks of the file in the working tree, or the whole content of each side when
    // the file has no conflict markers (when one side deleted it, or it is binary).
    pub fn load(repository: &Repository, path: &str) -> Self {
        let file = repository.path().join(path);
        // the markers of a file only, not of what a symlink points at
        let editable = std::fs::symlink_metadata(&file).is_ok_and(|meta| meta.is_file());
        if editable {
            let content = std::fs::read_to_string(&file).unwrap_or_default();
            if let Some(view) = parse_conflict_markers(&content) {
                return ConflictView { editable, ..view };
            }
        }
        let side = |side| {
            let lines = match repository.conflict_side(path, side) {
                SideContent::File(content) => content.lines().map(String::from).collect(),
                SideContent::Symlink(target) => vec![format!("Symlink to {target}")],
                SideContent::Submodule(commit) => vec![format!("Submodule at {commit}")],
                SideContent::Deleted => vec!["(deleted)".to_string()],
            };
            lines
                .into_iter()
                .map(|text| ConflictRow {
                    number: None,
                    text,
                    kind: RowKind::Kept,
                })
                .collect()
        };
        ConflictView {
            ours: side(ConflictSide::Ours),
            theirs: side(ConflictSide::Theirs),
            editable,
            ..Default::default()
        }
    }

    // The widest line number, for the width of the numbers.
    pub fn last_number(&self) -> Option<usize> {
        let rows = self.ours.iter().chain(&self.theirs);
        rows.chain(self.base.iter().flatten())
            .filter_map(|row| row.number)
            .max()
    }
}

// What the sides are to the operation stopped on the conflict, ours then theirs: in a rebase, git's
// "ours" is the branch rebased onto and "theirs" the commit replayed on it.
pub fn side_names(operation: Option<Operation>) -> (&'static str, &'static str) {
    match operation {
        Some(Operation::Merge) => ("Current branch", "Merging in"),
        Some(Operation::Rebase) => ("Rebasing onto", "Your commit"),
        Some(Operation::CherryPick) => ("Current", "Picking"),
        Some(Operation::Revert) => ("Current", "Reverting"),
        None => ("Current", "Incoming"),
    }
}

// Splits the conflict blocks of `content` into the sides, each block under a numbered header with
// `CONTEXT` lines of the file around it; the sides are padded to keep each block level. The common
// ancestor of the diff3 style (after |||||||) is a side of its own, and the lines of the other
// sides it doesn't have are marked changed.
pub fn parse_conflict_markers(content: &str) -> Option<ConflictView> {
    #[derive(PartialEq)]
    enum Part {
        Outside,
        Ours,
        Base,
        Theirs,
    }
    let mut view = ConflictView::default();
    let mut base = Vec::new();
    let mut has_base = false;
    let mut part = Part::Outside;
    // the block being read: each side's numbered lines, whether it has an ancestor
    let (mut ours, mut ancestor, mut theirs) = (Vec::new(), Vec::new(), Vec::new());
    let mut block_has_base = false;
    // the lines before the next block, and how many lines after the last are still shown
    let mut before: VecDeque<(usize, String)> = VecDeque::new();
    let mut after = 0;

    for (index, line) in content.lines().enumerate() {
        let number = index + 1;
        let marker = |m: &str| line == m || line.starts_with(&format!("{m} "));
        let text = line.replace('\t', "    ");
        let context = |number: usize, text: &str| ConflictRow {
            number: Some(number),
            text: text.to_string(),
            kind: RowKind::Context,
        };
        if part == Part::Outside && marker("<<<<<<<") {
            view.conflicts += 1;
            if view.ours_label.is_empty() {
                view.ours_label = line.get(8..).unwrap_or_default().to_string();
            }
            let header = ConflictRow {
                number: None,
                text: String::new(),
                kind: RowKind::Header,
            };
            for column in [&mut view.ours, &mut base, &mut view.theirs] {
                column.push(header.clone());
                column.extend(before.iter().map(|(n, text)| context(*n, text)));
            }
            before.clear();
            after = 0;
            part = Part::Ours;
        } else if part == Part::Ours && marker("|||||||") {
            block_has_base = true;
            part = Part::Base;
        } else if (part == Part::Ours || part == Part::Base) && line == "=======" {
            part = Part::Theirs;
        } else if part == Part::Theirs && marker(">>>>>>>") {
            if view.theirs_label.is_empty() {
                view.theirs_label = line.get(8..).unwrap_or_default().to_string();
            }
            has_base |= block_has_base;
            let height = ours.len().max(ancestor.len()).max(theirs.len());
            let ancestor = std::mem::take(&mut ancestor);
            // a side's rows, compared to the ancestor's lines when the block has them, padded
            let side = |lines: Vec<(usize, String)>, compare: bool| {
                let rows = lines.into_iter().map(|(number, text)| {
                    let changed = compare && !ancestor.iter().any(|(_, a)| *a == text);
                    let kind = match changed {
                        true => RowKind::Changed,
                        false => RowKind::Kept,
                    };
                    ConflictRow {
                        number: Some(number),
                        text,
                        kind,
                    }
                });
                let padding = ConflictRow {
                    number: None,
                    text: String::new(),
                    kind: RowKind::Kept,
                };
                let mut rows: Vec<ConflictRow> = rows.collect();
                rows.resize(height, padding);
                rows
            };
            view.ours
                .extend(side(std::mem::take(&mut ours), block_has_base));
            view.theirs
                .extend(side(std::mem::take(&mut theirs), block_has_base));
            base.extend(side(ancestor.clone(), false));
            block_has_base = false;
            after = CONTEXT;
            part = Part::Outside;
        } else {
            match part {
                Part::Ours => ours.push((number, text)),
                Part::Base => ancestor.push((number, text)),
                Part::Theirs => theirs.push((number, text)),
                // shown after the last block, else kept for before the next one
                Part::Outside if after > 0 => {
                    after -= 1;
                    for column in [&mut view.ours, &mut base, &mut view.theirs] {
                        column.push(context(number, &text));
                    }
                }
                Part::Outside => {
                    before.push_back((number, text));
                    if before.len() > CONTEXT {
                        before.pop_front();
                    }
                }
            }
        }
    }
    if view.conflicts == 0 {
        return None;
    }
    // the headers say how many there are, known once all are read
    let total = view.conflicts;
    for column in [&mut view.ours, &mut view.theirs, &mut base] {
        let headers = column.iter_mut().filter(|row| row.kind == RowKind::Header);
        for (count, row) in (1..).zip(headers) {
            row.text = format!("── conflict {count} of {total} ──");
        }
    }
    view.base = has_base.then_some(base);
    Some(view)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Each row as "number|text|kind", for short expectations.
    fn rows(rows: &[ConflictRow]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                let number = row.number.map_or(String::new(), |n| n.to_string());
                let kind = match row.kind {
                    RowKind::Header => "h",
                    RowKind::Context => "c",
                    RowKind::Kept => "k",
                    RowKind::Changed => "+",
                };
                format!("{number}|{}|{kind}", row.text)
            })
            .collect()
    }

    #[test]
    fn test_parse_conflict_markers() {
        let content = "\
a
b
c
d
<<<<<<< HEAD
ours 1
=======
theirs 1a
theirs 1b
>>>>>>> feature
e
f
<<<<<<< HEAD
ours 2
||||||| base
base 2
=======
base 2
theirs 2
>>>>>>> feature
end
";
        let view = parse_conflict_markers(content).unwrap();
        assert_eq!(
            (view.ours_label.as_str(), view.theirs_label.as_str()),
            ("HEAD", "feature")
        );
        assert_eq!(view.conflicts, 2);
        // 3 lines before the first conflict; the lines between two conflicts once, after the
        // first; the sides padded level; with an ancestor, what a side changed is marked
        assert_eq!(
            rows(&view.ours),
            [
                "|── conflict 1 of 2 ──|h",
                "2|b|c",
                "3|c|c",
                "4|d|c",
                "6|ours 1|k",
                "||k",
                "11|e|c",
                "12|f|c",
                "|── conflict 2 of 2 ──|h",
                "14|ours 2|+",
                "||k",
                "21|end|c",
            ]
        );
        assert_eq!(
            rows(&view.theirs),
            [
                "|── conflict 1 of 2 ──|h",
                "2|b|c",
                "3|c|c",
                "4|d|c",
                "8|theirs 1a|k",
                "9|theirs 1b|k",
                "11|e|c",
                "12|f|c",
                "|── conflict 2 of 2 ──|h",
                "18|base 2|k",
                "19|theirs 2|+",
                "21|end|c",
            ]
        );
        let base = view.base.unwrap();
        assert_eq!(base.len(), view.ours.len());
        assert_eq!(rows(&base[9..11]), ["16|base 2|k", "||k"]);

        assert_eq!(parse_conflict_markers("no conflict\n"), None);
    }

    #[test]
    fn test_no_base_without_diff3_markers() {
        let view = parse_conflict_markers("<<<<<<< a\nx\n=======\ny\n>>>>>>> b\n").unwrap();
        assert_eq!(view.base, None);
        // nothing to compare to: no line is marked changed
        assert_eq!(rows(&view.ours), ["|── conflict 1 of 1 ──|h", "2|x|k"]);
    }

    #[test]
    fn test_side_names_follow_the_operation() {
        // in a rebase, ours is the branch rebased onto, theirs the commit replayed
        assert_eq!(
            side_names(Some(Operation::Rebase)),
            ("Rebasing onto", "Your commit")
        );
        assert_eq!(
            side_names(Some(Operation::Merge)),
            ("Current branch", "Merging in")
        );
    }
}
