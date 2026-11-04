// The diffs of commits and of the local changes, the files they change, and applying a patch.

use rustc_hash::FxHashSet;
use std::path::Path;

use super::{
    git_output, git_output_bytes, git_output_with_status, parse_name_status, pathspecs, Commit,
    CommitHash, LocalChangeStatus, Repository,
};

impl Repository {
    // The paths among `paths` whose diff shows text converted from them (a diff driver with a
    // `textconv`, set in .gitattributes), which can't be applied back as a patch.
    pub fn converted_paths(&self, paths: &[&str]) -> FxHashSet<String> {
        if self.textconv_drivers.is_empty() || paths.is_empty() {
            return FxHashSet::default();
        }
        let mut args = vec!["check-attr", "-z", "diff", "--"];
        args.extend_from_slice(paths);
        let out = git_output(&self.path, &args).unwrap_or_default();
        // path, attribute, value, each ended by a NUL
        let fields: Vec<&str> = out.split('\0').collect();
        fields
            .as_chunks::<3>()
            .0
            .iter()
            .filter(|[_, _, driver]| self.textconv_drivers.iter().any(|d| d == driver))
            .map(|[path, _, _]| path.to_string())
            .collect()
    }

    pub fn commit_detail(&self, commit_hash: &CommitHash) -> (Commit, Vec<FileChange>) {
        let commit = self.commit(commit_hash).unwrap().clone();
        (commit, self.commit_changes_work(commit_hash)())
    }

    // The files a commit changed, see `commit_detail`, as work to run on another thread.
    pub fn commit_changes_work(
        &self,
        commit_hash: &CommitHash,
    ) -> impl FnOnce() -> Vec<FileChange> + Send + 'static {
        let base = self.commit_base(commit_hash);
        let (path, target) = (self.path.clone(), commit_hash.as_str().to_string());
        move || {
            base.map(|base| diff_summary(&path, &base, &target))
                .unwrap_or_default()
        }
    }

    // What a commit changed is compared to: its first parent, or the empty tree for a root commit.
    fn commit_base(&self, commit_hash: &CommitHash) -> Option<String> {
        match self.parents_hash(commit_hash).first() {
            Some(parent) => Some(parent.as_str().to_string()),
            None => empty_tree(&self.path).ok(),
        }
    }

    #[cfg(test)]
    pub fn local_change_diff(&self, paths: &[&str], of: DiffOf, context: usize) -> String {
        self.local_change_diff_work(paths, of, context)()
    }

    // `local_change_diff` as work to run on another thread.
    pub fn local_change_diff_work(
        &self,
        paths: &[&str],
        of: DiffOf,
        context: usize,
    ) -> Box<dyn FnOnce() -> String + Send> {
        if paths.is_empty() {
            return Box::new(String::new);
        }
        Box::new(local_change_diff_work(&self.path, paths, of, context))
    }

    // The tree to compare the index to while amending: `HEAD^`, or the empty tree for a root
    // commit. The name, not the resolved tree: the commands naming it read like the ones a user
    // would type, see `GitArgs`.
    pub fn amend_base(&self) -> std::result::Result<String, String> {
        git_output(&self.path, &["rev-parse", "--verify", "--quiet", "HEAD^"])
            .map(|_| "HEAD^".to_string())
            .or_else(|_| empty_tree(&self.path))
    }

    // The diff of `paths` (all files when empty) made by a commit, against its first parent,
    // showing `context` lines around each change, as work to run on another thread.
    pub fn commit_diff_work(
        &self,
        commit_hash: &CommitHash,
        paths: &[&str],
        context: usize,
    ) -> Box<dyn FnOnce() -> String + Send> {
        match self.commit_base(commit_hash) {
            Some(base) => self.diff_between_work(&base, commit_hash.as_str(), paths, context),
            None => Box::new(String::new),
        }
    }

    // The diff of `paths` (all files when empty) from one commit to another, showing `context`
    // lines around each change.
    #[cfg(test)]
    pub fn diff_between(&self, base: &str, target: &str, paths: &[&str], context: usize) -> String {
        self.diff_between_work(base, target, paths, context)()
    }

    // `diff_between` as work to run on another thread.
    pub fn diff_between_work(
        &self,
        base: &str,
        target: &str,
        paths: &[&str],
        context: usize,
    ) -> Box<dyn FnOnce() -> String + Send> {
        let mut args: Vec<String> = PATCH_DIFF_ARGS.iter().map(|a| a.to_string()).collect();
        args.extend(["-M", base, target].map(String::from));
        args.push(format!("--unified={context}"));
        args.push("--".into());
        args.extend(pathspecs(paths));
        let path = self.path.clone();
        Box::new(move || {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            git_output(&path, &args).unwrap_or_default()
        })
    }

    // The files changed from one commit to another.
    pub fn diff_summary_between(&self, base: &CommitHash, target: &CommitHash) -> Vec<FileChange> {
        diff_summary(&self.path, base.as_str(), target.as_str())
    }

    // Applies `patch`, a diff produced by `local_change_diff`, to the index: forward stages it,
    // in reverse unstages it.
    pub fn apply_patch(
        &self,
        patch: &str,
        direction: PatchDirection,
    ) -> std::result::Result<(), String> {
        let args: &[&str] = match direction {
            PatchDirection::Forward => &["apply", "--cached", "-"],
            PatchDirection::Reverse => &["apply", "--cached", "--reverse", "-"],
        };
        self.run(args, Some(patch))
    }
}

// Which way git applies a patch: forward, making its changes (staging them), or in reverse, taking
// them back (unstaging or discarding them). A partial patch is made for its direction, see
// `DiffHunk::partial_patch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchDirection {
    Forward,
    Reverse,
}

#[derive(Debug, Clone)]
pub enum FileChange {
    Add { path: String },
    Modify { path: String },
    Delete { path: String },
    Move { from: String, to: String },
}

impl FileChange {
    // The path it leaves the file at, the one the diff names: `to` for a rename.
    pub fn path(&self) -> &str {
        match self {
            FileChange::Add { path }
            | FileChange::Modify { path }
            | FileChange::Delete { path } => path,
            FileChange::Move { to, .. } => to,
        }
    }

    // Its letter, like git's: "A" added, "M" modified, "D" deleted, "R" renamed.
    pub fn symbol(&self) -> &'static str {
        match self {
            FileChange::Add { .. } => "A",
            FileChange::Modify { .. } => "M",
            FileChange::Delete { .. } => "D",
            FileChange::Move { .. } => "R",
        }
    }
}

fn diff_summary(path: &Path, base: &str, target: &str) -> Vec<FileChange> {
    // -z leaves paths as they are, git would quote those with special characters
    git_output_bytes(path, &["diff", "--name-status", "-z", "-M", base, target])
        .map(|out| parse_name_status(&out))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|file| match file.status {
            LocalChangeStatus::Added => Some(FileChange::Add { path: file.path }),
            LocalChangeStatus::Modified | LocalChangeStatus::TypeChanged => {
                Some(FileChange::Modify { path: file.path })
            }
            LocalChangeStatus::Deleted => Some(FileChange::Delete { path: file.path }),
            LocalChangeStatus::Renamed | LocalChangeStatus::Copied => Some(FileChange::Move {
                from: file.orig_path.unwrap_or_default(),
                to: file.path,
            }),
            LocalChangeStatus::Untracked | LocalChangeStatus::Conflicted => None,
        })
        .collect()
}

// A diff in the format it is parsed in, and applied back as a patch: ignoring user config that
// changes it, like colors, external diff tools or other prefixes. Files converted to text to diff
// them are shown so, without hunks to apply, see `ParsedDiff::drop_converted`. Paths are not
// quoted, so `+++ b/é.txt` names the file as it is (tabs and quotes in names still are).
const PATCH_DIFF_ARGS: [&str; 7] = [
    "-c",
    "core.quotePath=false",
    "diff",
    "--no-color",
    "--no-ext-diff",
    "--src-prefix=a/",
    "--dst-prefix=b/",
];

// The lines git shows around a change by default.
pub const DEFAULT_DIFF_CONTEXT: usize = 3;

// Which changes of the working tree a diff shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffOf<'a> {
    Unstaged,
    // a file git doesn't track, diffed against nothing, one path at a time
    Untracked,
    // compared to `base`, HEAD when None, see `amend_base`
    Staged { base: Option<&'a str> },
}

// The diff of the local changes of `targets`, see `Repository::local_change_diff_work`.
fn local_change_diff_work(
    path: &Path,
    targets: &[&str],
    of: DiffOf,
    context: usize,
) -> impl FnOnce() -> String + Send + 'static {
    let mut args: Vec<String> = PATCH_DIFF_ARGS.iter().map(|a| a.to_string()).collect();
    args.push(format!("--unified={context}"));
    match of {
        DiffOf::Staged { base } => {
            args.push("--cached".into());
            args.extend(base.map(String::from));
            args.push("--".into());
            args.extend(pathspecs(targets));
        }
        DiffOf::Untracked => {
            args.extend(["--no-index", "--", "/dev/null"].map(String::from));
            args.extend(targets.iter().map(|t| t.to_string()));
        }
        DiffOf::Unstaged => {
            args.push("--".into());
            args.extend(pathspecs(targets));
        }
    }
    let path = path.to_path_buf();
    // `--no-index` exits with 1 when the files differ
    move || git_output_with_status(&path, &args).map_or_else(String::new, |(_, out)| out)
}

// The empty tree, as the base of a root commit.
fn empty_tree(path: &Path) -> std::result::Result<String, String> {
    git_output(path, &["hash-object", "-t", "tree", "/dev/null"]).map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::read_head_commit;
    use crate::git::run_task;
    use crate::git::GitCommand;
    use crate::git::GitTask;
    use crate::git::SortCommit;

    #[test]
    fn test_diff_summary_special_paths() {
        use crate::test_support::git::GitRepository;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        std::fs::write(path.join("café.txt"), "a\n").unwrap();
        repo.add(&["café.txt"]);
        repo.commit("first", "2024-01-01");
        std::fs::write(path.join("café.txt"), "b\n").unwrap();
        std::fs::write(path.join("a\tb.txt"), "b\n").unwrap();
        repo.add(&["café.txt", "a\tb.txt"]);
        repo.commit("second", "2024-01-02");

        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let hashes: Vec<CommitHash> = repository
            .all_commits()
            .iter()
            .map(|c| c.commit_hash.clone())
            .collect();
        let paths = |changes: Vec<FileChange>| -> Vec<String> {
            changes
                .into_iter()
                .map(|c| match c {
                    FileChange::Add { path } | FileChange::Modify { path } => path,
                    other => panic!("{other:?}"),
                })
                .collect()
        };
        // as they are, not quoted
        let mut changed = paths(repository.commit_detail(&hashes[0]).1);
        changed.sort();
        assert_eq!(changed, ["a\tb.txt", "café.txt"]);
        assert_eq!(paths(repository.commit_detail(&hashes[1]).1), ["café.txt"]);
    }

    #[test]
    fn test_unstage_lines_of_added_unicode_file() {
        use crate::test_support::git::GitRepository;
        use crate::widget::diff::parse_diff;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        repo.commit("base", "2024-01-01");
        std::fs::write(path.join("é.txt"), "a\nb\n").unwrap();
        repo.add(&["é.txt"]);

        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let diff = parse_diff(&repository.local_change_diff(
            &["é.txt"],
            DiffOf::Staged { base: None },
            DEFAULT_DIFF_CONTEXT,
        ));
        // unstaging the second line keeps the file staged with the first
        let patch = diff.hunks[0].partial_patch(&[1], PatchDirection::Reverse);
        repository
            .apply_patch(&patch, PatchDirection::Reverse)
            .unwrap();
        let staged = git_output(path, &["show", ":é.txt"]).unwrap();
        assert_eq!(staged, "a\n");
    }

    #[test]
    fn test_hunks_of_renamed_file() {
        use crate::test_support::git::GitRepository;
        use crate::widget::diff::parse_diff;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        let numbers: String = (1..=20).map(|n| format!("{n}\n")).collect();
        std::fs::write(path.join("old.txt"), &numbers).unwrap();
        repo.add(&["old.txt"]);
        repo.commit("base", "2024-01-01");
        git_output(path, &["mv", "old.txt", "new.txt"]).unwrap();
        let edited = numbers
            .replace("\n5\n", "\nfive\n")
            .replace("\n15\n", "\nfifteen\n");
        std::fs::write(path.join("new.txt"), &edited).unwrap();
        repo.add(&["new.txt"]);
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();

        // unstaging a hunk of a staged rename keeps the rename and the other hunk staged
        let staged = parse_diff(&repository.local_change_diff(
            &[":/"],
            DiffOf::Staged { base: None },
            DEFAULT_DIFF_CONTEXT,
        ));
        assert_eq!(staged.hunks.len(), 2);
        repository
            .apply_patch(&staged.hunks[0].patch(), PatchDirection::Reverse)
            .unwrap();
        let index = git_output(path, &["show", ":new.txt"]).unwrap();
        assert_eq!(index, numbers.replace("\n15\n", "\nfifteen\n"));
        assert!(git_output(path, &["show", ":old.txt"]).is_err());

        // a hunk of a commit renaming the file applies to the file under its new name
        git_output(path, &["add", "new.txt"]).unwrap();
        git_output(path, &["commit", "-q", "-m", "rename"]).unwrap();
        std::fs::write(path.join("new.txt"), &numbers).unwrap();
        let head = read_head_commit(path).unwrap();
        let base = format!("{}~1", head.as_str());
        let diff =
            parse_diff(&repository.diff_between(&base, head.as_str(), &[], DEFAULT_DIFF_CONTEXT));
        // to the working tree, as `git apply` does from the changes of a commit
        let task = GitTask {
            commands: vec![GitCommand::Apply.args(["-"])],
            input: Some(diff.hunks[0].partial_patch(&[3, 4], PatchDirection::Forward)),
            ..Default::default()
        };
        run_task(path, &task).unwrap();
        let content = std::fs::read_to_string(path.join("new.txt")).unwrap();
        assert_eq!(content, numbers.replace("\n5\n", "\nfive\n"));
    }
}
