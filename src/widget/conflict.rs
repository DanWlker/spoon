use std::{collections::VecDeque, path::Path};

use rustc_hash::FxHashSet;

use crate::{
    background::Cancel,
    git::{conflict_side, is_conflict_marker, ConflictSide, Operation, Repository, SideContent},
    widget::style::tabs_shown,
};

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
    // no line, keeping the sides' conflicts level
    Padding,
}

// A row of a side: its line in the file in the working tree (none for headers, padding, and the
// sides of a file without markers).
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
    // the file has no conflict markers (when one side deleted it, or it is binary); as work to run on
    // another thread, as a big file takes a while, stopping early once the job it runs for is
    // dropped, see `Job::read`.
    pub fn load_work(
        repository: &Repository,
        path: &str,
    ) -> impl FnOnce(&Cancel) -> Self + Send + 'static {
        let (repository, path) = (repository.path().to_path_buf(), path.to_string());
        move |cancel| ConflictView::load(&repository, &path, cancel)
    }

    // See `load_work`, in the repository at `repository`.
    pub fn load(repository: &Path, path: &str, cancel: &Cancel) -> Self {
        let file = crate::git::work_path(repository, path);
        // the markers of a file only, not of what a symlink points at
        let editable = std::fs::symlink_metadata(&file).is_ok_and(|meta| meta.is_file());
        // a binary file has no markers, and would take long to convert
        let content = (editable.then(|| std::fs::read(&file).ok()).flatten())
            .filter(|bytes| !crate::git::is_binary(bytes));
        if let Some(bytes) = content {
            // its bytes that aren't UTF-8 kept, like those of the sides from git
            if let Some(view) = parse_conflict_markers(&crate::git::text_from_git(&bytes)) {
                return ConflictView { editable, ..view };
            }
        }
        let side = |side| {
            let lines = match conflict_side(repository, path, side, cancel) {
                Ok(SideContent::File(content)) => content
                    .lines()
                    .map(|l| tabs_shown(l).into_owned())
                    .collect(),
                Ok(SideContent::Symlink(target)) => vec![format!("Symlink to {target}")],
                Ok(SideContent::Submodule(commit)) => vec![format!("Submodule at {commit}")],
                Ok(SideContent::Deleted) => vec!["(deleted)".to_string()],
                // stopped, nobody waits for it
                Err(_) if cancel.is_cancelled() => vec![],
                Err(err) => {
                    let err = err.lines().last().unwrap_or_default();
                    vec![format!("Cannot show this side: {}", tabs_shown(err))]
                }
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
        let marker = |m: &str| is_conflict_marker(line, m);
        let text = tabs_shown(line).into_owned();
        let context = |number: usize, text: &str| ConflictRow {
            number: Some(number),
            text: text.to_string(),
            kind: RowKind::Context,
        };
        if part == Part::Outside && marker("<<<<<<<") {
            view.conflicts += 1;
            if view.ours_label.is_empty() {
                view.ours_label = tabs_shown(line.get(8..).unwrap_or_default()).into_owned();
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
                view.theirs_label = tabs_shown(line.get(8..).unwrap_or_default()).into_owned();
            }
            has_base |= block_has_base;
            let height = ours.len().max(ancestor.len()).max(theirs.len());
            let ancestor = std::mem::take(&mut ancestor);
            let ancestor_lines: FxHashSet<&str> = ancestor
                .iter()
                .map(|(_, a): &(usize, String)| a.as_str())
                .collect();
            // a side's rows, compared to the ancestor's lines when the block has them, padded
            let side = |lines: Vec<(usize, String)>, compare: bool| {
                let rows = lines.into_iter().map(|(number, text)| {
                    let changed = compare && !ancestor_lines.contains(text.as_str());
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
                    kind: RowKind::Padding,
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
                    RowKind::Padding => "p",
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
                "||p",
                "11|e|c",
                "12|f|c",
                "|── conflict 2 of 2 ──|h",
                "14|ours 2|+",
                "||p",
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
        assert_eq!(rows(&base[9..11]), ["16|base 2|k", "||p"]);

        assert_eq!(parse_conflict_markers("no conflict\n"), None);
    }

    // A file that isn't UTF-8, like Latin-1, still shows its conflict.
    #[test]
    fn test_conflict_of_a_file_not_utf8() {
        use crate::{
            git::SortCommit,
            test_support::git::{git, GitRepository},
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        std::fs::write(path.join("f"), b"caf\xe9\n").unwrap();
        repo.add(&["f"]);
        repo.commit("base", "2024-01-01");
        repo.checkout_b("incoming");
        std::fs::write(path.join("f"), b"th\xe9\n").unwrap();
        repo.add(&["f"]);
        repo.commit("incoming", "2024-01-02");
        repo.checkout("master");
        std::fs::write(path.join("f"), b"cr\xe8me\n").unwrap();
        repo.add(&["f"]);
        repo.commit("existing", "2024-01-03");
        let merged = crate::git::git_command()
            .args(["merge", "-q", "incoming"])
            .current_dir(path)
            .output()
            .unwrap();
        assert!(!merged.status.success());
        assert!(git(path, &["diff", "--name-only", "--diff-filter=U"]).contains('f'));

        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let view = ConflictView::load(repository.path(), "f", &Cancel::default());
        assert_eq!(view.conflicts, 1);
    }

    // A binary file in the work tree isn't read for markers, even holding lines like them.
    #[test]
    fn test_binary_file_shows_the_sides() {
        use crate::{git::SortCommit, test_support::git::GitRepository};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        std::fs::write(path.join("f"), "base\n").unwrap();
        repo.add(&["f"]);
        repo.commit("base", "2024-01-01");
        repo.checkout_b("incoming");
        std::fs::write(path.join("f"), "theirs\n").unwrap();
        repo.add(&["f"]);
        repo.commit("incoming", "2024-01-02");
        repo.checkout("master");
        std::fs::write(path.join("f"), "ours\n").unwrap();
        repo.add(&["f"]);
        repo.commit("existing", "2024-01-03");
        let merged = crate::git::git_command()
            .args(["merge", "-q", "incoming"])
            .current_dir(path)
            .output()
            .unwrap();
        assert!(!merged.status.success());
        std::fs::write(path.join("f"), "\0\n<<<<<<< a\nx\n=======\ny\n>>>>>>> b\n").unwrap();

        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let view = ConflictView::load(repository.path(), "f", &Cancel::default());
        assert_eq!(view.conflicts, 0);
        assert_eq!(rows(&view.ours), ["|ours|k"]);
        // stopped once nobody waits for it: the sides aren't read
        let stopped = ConflictView::load(repository.path(), "f", &Cancel::cancelled());
        assert!(stopped.ours.is_empty() && stopped.theirs.is_empty());
    }

    // git failing to list the sides, like with a broken index, showed both as deleted.
    #[test]
    fn test_a_side_git_cannot_read_is_not_shown_deleted() {
        use crate::test_support::git::GitRepository;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        std::fs::write(path.join("f"), "base\n").unwrap();
        repo.add(&["f"]);
        repo.commit("base", "2024-01-01");
        repo.checkout_b("incoming");
        std::fs::write(path.join("f"), "theirs\n").unwrap();
        repo.add(&["f"]);
        repo.commit("incoming", "2024-01-02");
        repo.checkout("master");
        std::fs::write(path.join("f"), "ours\n").unwrap();
        repo.add(&["f"]);
        repo.commit("existing", "2024-01-03");
        let merged = crate::git::git_command()
            .args(["merge", "-q", "incoming"])
            .current_dir(path)
            .output()
            .unwrap();
        assert!(!merged.status.success());
        // no markers to show, so the sides are read from git
        std::fs::remove_file(path.join("f")).unwrap();
        std::fs::write(path.join(".git/index"), "broken").unwrap();

        let view = ConflictView::load(path, "f", &Cancel::default());
        let ours = rows(&view.ours);
        assert_eq!(ours.len(), 1);
        assert!(ours[0].starts_with("|Cannot show this side: "), "{ours:?}");
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
