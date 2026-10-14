use std::path::Path;

use crate::git::{ConflictSide, Repository};

// The two sides of a conflicted file, to pick one of them.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ConflictView {
    // the names git put on the conflict markers, like "HEAD" and the merged branch
    pub ours_label: String,
    pub theirs_label: String,
    pub ours: Vec<String>,
    pub theirs: Vec<String>,
}

impl ConflictView {
    // The conflicting blocks of the file in the working tree, or the whole content of each side when
    // the file has no conflict markers (when one side deleted it, or it is binary).
    pub fn load(repository: &Repository, toplevel: &Path, path: &str) -> Self {
        let content = std::fs::read_to_string(toplevel.join(path)).unwrap_or_default();
        if let Some(view) = parse_conflict_markers(&content) {
            return view;
        }
        let side = |side| match repository.conflict_side_content(path, side) {
            Some(content) => content.lines().map(String::from).collect(),
            None => vec!["(deleted)".to_string()],
        };
        ConflictView {
            ours_label: String::new(),
            theirs_label: String::new(),
            ours: side(ConflictSide::Ours),
            theirs: side(ConflictSide::Theirs),
        }
    }
}

// Splits the conflict blocks of `content` into both sides, each block under a numbered header.
// The common ancestor of the diff3 style (after |||||||) is left out.
pub fn parse_conflict_markers(content: &str) -> Option<ConflictView> {
    #[derive(PartialEq)]
    enum Part {
        Outside,
        Ours,
        Base,
        Theirs,
    }
    let mut view = ConflictView::default();
    let mut part = Part::Outside;
    let mut count = 0;

    for line in content.lines() {
        let marker = |m: &str| line == m || line.starts_with(&format!("{m} "));
        if part == Part::Outside && marker("<<<<<<<") {
            count += 1;
            if view.ours_label.is_empty() {
                view.ours_label = line.get(8..).unwrap_or_default().to_string();
            }
            let header = format!("── conflict {count} ──");
            view.ours.push(header.clone());
            view.theirs.push(header);
            part = Part::Ours;
        } else if part == Part::Ours && marker("|||||||") {
            part = Part::Base;
        } else if (part == Part::Ours || part == Part::Base) && line == "=======" {
            part = Part::Theirs;
        } else if part == Part::Theirs && marker(">>>>>>>") {
            if view.theirs_label.is_empty() {
                view.theirs_label = line.get(8..).unwrap_or_default().to_string();
            }
            part = Part::Outside;
        } else {
            let line = line.replace('\t', "    ");
            match part {
                Part::Ours => view.ours.push(line),
                Part::Theirs => view.theirs.push(line),
                Part::Outside | Part::Base => {}
            }
        }
    }
    (count > 0).then_some(view)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_conflict_markers() {
        let content = "\
start
<<<<<<< HEAD
ours 1
=======
theirs 1
>>>>>>> feature
middle
<<<<<<< HEAD
ours 2
||||||| base
base 2
=======
theirs 2a
theirs 2b
>>>>>>> feature
end
";
        let actual = parse_conflict_markers(content).unwrap();
        let expected = ConflictView {
            ours_label: "HEAD".into(),
            theirs_label: "feature".into(),
            ours: vec![
                "── conflict 1 ──".into(),
                "ours 1".into(),
                "── conflict 2 ──".into(),
                "ours 2".into(),
            ],
            theirs: vec![
                "── conflict 1 ──".into(),
                "theirs 1".into(),
                "── conflict 2 ──".into(),
                "theirs 2a".into(),
                "theirs 2b".into(),
            ],
        };
        assert_eq!(actual, expected);

        assert_eq!(parse_conflict_markers("no conflict\n"), None);
    }
}
