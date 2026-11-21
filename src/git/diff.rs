// The diffs of commits and of the local changes, the files they change, and applying a patch.

use rustc_hash::{FxHashMap, FxHashSet};
use std::path::{Path, PathBuf};

use crate::background::Cancel;

use super::{
    git_output, git_output_bytes_until, git_run_until, parse_name_status, pathspecs,
    submodule::gitlinks_at, text_from_git, CommitHash, LocalChangeStatus, Repository, WorkTree,
};

// Tells the paths whose diff shows text converted from them (a diff driver with a `textconv`, set in
// .gitattributes), which can't be applied back as a patch; on the thread loading the diff.
#[derive(Debug, Default, Clone)]
pub struct ConvertedPaths {
    repository: PathBuf,
    textconv_drivers: Vec<String>,
}

impl ConvertedPaths {
    // Those of `paths`.
    pub fn of(&self, paths: &[&str]) -> FxHashSet<String> {
        if self.textconv_drivers.is_empty() || paths.is_empty() {
            return FxHashSet::default();
        }
        let mut args = vec!["check-attr", "-z", "diff", "--"];
        args.extend_from_slice(paths);
        let out = git_output(&self.repository, &args).unwrap_or_default();
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
}

impl Repository {
    pub fn converted_paths(&self) -> ConvertedPaths {
        ConvertedPaths {
            repository: self.path.clone(),
            textconv_drivers: self.textconv_drivers.clone(),
        }
    }

    #[cfg(test)]
    pub fn commit_detail(&self, commit_hash: &CommitHash) -> (super::Commit, Vec<FileChange>) {
        let commit = self.commit(commit_hash).unwrap().clone();
        let changes = self.changes_work(None, commit_hash)(&Cancel::default());
        (commit, changes.changes)
    }

    // What changed from `base` to the commit, from what it is compared to when None (see
    // `commit_base`): its files, and the submodules among them. As work to run on another thread,
    // stopping early once the job it runs for is dropped, see `Job::read`.
    pub fn changes_work(
        &self,
        base: Option<&CommitHash>,
        commit_hash: &CommitHash,
    ) -> impl FnOnce(&Cancel) -> CommitChanges + Send + 'static {
        let base = match base {
            Some(base) => Some(base.as_str().to_string()),
            None => self.commit_base(commit_hash),
        };
        let (path, target) = (self.path.clone(), commit_hash.as_str().to_string());
        move |cancel| {
            let Some(base) = base else {
                return CommitChanges::default();
            };
            let changes = diff_summary(&path, &base, &target, cancel);
            // under its name before a rename too
            let paths: Vec<&str> = changes
                .iter()
                .flat_map(|change| match change {
                    FileChange::Move { from, to } => vec![from.as_str(), to.as_str()],
                    change => vec![change.path()],
                })
                .collect();
            let gitlinks = ChangedGitlinks {
                commit: gitlinks_at(&path, &target, &paths, cancel),
                base: gitlinks_at(&path, &base, &paths, cancel),
            };
            CommitChanges { changes, gitlinks }
        }
    }

    // What a commit changed is compared to: its first parent, or the empty tree for a root commit.
    fn commit_base(&self, commit_hash: &CommitHash) -> Option<String> {
        match self.parents_hash(commit_hash).first() {
            Some(parent) => Some(parent.as_str().to_string()),
            None => self.empty_tree.clone(),
        }
    }

    #[cfg(test)]
    pub fn local_change_diff(&self, paths: &[&str], of: DiffOf, context: usize) -> String {
        self.local_change_diff_work(paths, of, context)(&Cancel::default())
    }

    // `local_change_diff` as work to run on another thread.
    pub fn local_change_diff_work(&self, paths: &[&str], of: DiffOf, context: usize) -> DiffWork {
        if paths.is_empty() {
            return Box::new(|_| String::new());
        }
        Box::new(local_change_diff_work(&self.path, paths, of, context))
    }

    // The tree to compare the index to while amending: `HEAD^`, or the empty tree for a root
    // commit. The name, not the resolved tree: the commands naming it read like the ones a user
    // would type, see `GitArgs`.
    pub fn amend_base(&self) -> std::result::Result<String, String> {
        git_output(&self.path, &["rev-parse", "--verify", "--quiet", "HEAD^"])
            .map(|_| "HEAD^".to_string())
            .or_else(|_| {
                self.empty_tree
                    .clone()
                    .ok_or_else(|| "no empty tree".to_string())
            })
    }

    // The diff of `paths` (all files when empty) made by a commit, against its first parent,
    // showing `context` lines around each change, as work to run on another thread.
    pub fn commit_diff_work(
        &self,
        commit_hash: &CommitHash,
        paths: &[&str],
        context: usize,
    ) -> DiffWork {
        match self.commit_base(commit_hash) {
            Some(base) => self.diff_between_work(&base, commit_hash.as_str(), paths, context),
            None => Box::new(|_| String::new()),
        }
    }

    // The diff of `paths` (all files when empty) from one commit to another, showing `context`
    // lines around each change.
    #[cfg(test)]
    pub fn diff_between(&self, base: &str, target: &str, paths: &[&str], context: usize) -> String {
        self.diff_between_work(base, target, paths, context)(&Cancel::default())
    }

    // `diff_between` as work to run on another thread.
    pub fn diff_between_work(
        &self,
        base: &str,
        target: &str,
        paths: &[&str],
        context: usize,
    ) -> DiffWork {
        let mut args: Vec<String> = PATCH_DIFF_ARGS.iter().map(|a| a.to_string()).collect();
        args.extend(["-M", base, target].map(String::from));
        args.push(format!("--unified={context}"));
        args.push("--".into());
        args.extend(pathspecs(paths));
        let path = self.path.clone();
        Box::new(move |cancel| {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            git_output_bytes_until(&path, &args, cancel)
                .map(|out| text_from_git(&out))
                .unwrap_or_default()
        })
    }
}

impl WorkTree {
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

// The submodules (gitlinks) among the paths a commit changed, with the commit each records: in
// it, and in what it is compared to, a submodule deleted being a gitlink in the base only.
#[derive(Debug, Default, Clone)]
pub struct ChangedGitlinks {
    pub commit: FxHashMap<String, String>,
    pub base: FxHashMap<String, String>,
}

impl ChangedGitlinks {
    pub fn contains(&self, path: &str) -> bool {
        self.commit.contains_key(path) || self.base.contains_key(path)
    }
}

// What changed from one commit to another, see `Repository::changes_work`.
#[derive(Debug, Default)]
pub struct CommitChanges {
    pub changes: Vec<FileChange>,
    pub gitlinks: ChangedGitlinks,
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

fn diff_summary(path: &Path, base: &str, target: &str, cancel: &Cancel) -> Vec<FileChange> {
    // -z leaves paths as they are, git would quote those with special characters
    let args = ["diff", "--name-status", "-z", "-M", base, target];
    git_output_bytes_until(path, &args, cancel)
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

// The work making a diff, stopping early once the job it runs for is dropped, see
// `Job::read`.
pub type DiffWork = Box<dyn FnOnce(&Cancel) -> String + Send>;

// The diff of the local changes of `targets`, see `Repository::local_change_diff_work`.
fn local_change_diff_work(
    path: &Path,
    targets: &[&str],
    of: DiffOf,
    context: usize,
) -> impl FnOnce(&Cancel) -> String + Send + 'static {
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
    move |cancel| {
        git_run_until(&path, &args, cancel)
            .map_or_else(String::new, |out| text_from_git(&out.stdout))
    }
}

// The empty tree, as the base of a root commit.
pub(super) fn empty_tree(path: &Path) -> std::result::Result<String, String> {
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

    // The changes and the files of a commit stop loading once nobody waits for them, like once
    // the details showing them close.
    #[test]
    fn test_commit_changes_and_files_stopped_once_cancelled() {
        use crate::test_support::git::GitRepository;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        std::fs::write(path.join("a"), "a\n").unwrap();
        repo.add(&["a"]);
        repo.commit("c1", "2024-01-01");
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let head = repository.head_commit().unwrap();
        let changes = |cancel| repository.changes_work(None, &head)(&cancel).changes;
        assert_eq!(changes(Cancel::default()).len(), 1);
        assert!(changes(Cancel::cancelled()).is_empty());
        let files = |cancel| repository.commit_tree_work(&head)(&cancel).0;
        assert_eq!(files(Cancel::default()), ["a"]);
        assert!(files(Cancel::cancelled()).is_empty());
    }

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
            .work_tree()
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
            .work_tree()
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
