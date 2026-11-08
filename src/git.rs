// The repository: its commits and refs, and how they relate; what git does with them is in the
// modules below.

use std::{
    path::{Path, PathBuf},
    sync::Mutex,
    time::Instant,
};

use chrono::{DateTime, FixedOffset};
use rustc_hash::{FxHashMap, FxHashSet};

mod conflict;
mod diff;
mod history;
mod load;
mod local_changes;
mod submodule;
mod task;

pub use conflict::{
    is_conflict_marker, Conflict, ConflictSide, ConflictWork, SideContent, MAX_CONFLICT_STEPS,
};
pub use diff::{DiffOf, FileChange, PatchDirection, DEFAULT_DIFF_CONTEXT};
pub use history::{Blame, CommitTree, FileBlameResult, FileHistoryResult, FileRevision};
use load::parse_iso_date;
pub use load::{read_head_commit, read_operation, refs_snapshot, LoadOptions, Loaded};
use local_changes::{load_local_changes, parse_name_status};
pub use local_changes::{Entry, FilesSnapshot, LocalChangeFile, LocalChangeStatus, LocalChanges};
use submodule::{load_submodules, SubmoduleRepositories};
pub use task::{
    command_log, git_arg, git_bytes, git_command, is_kept_byte, pathspecs, run_task, stash_args,
    text_from_git, todo_action, work_path, AfterTask, BranchStatus, GitArgs, GitCommand, GitTask,
    LoggedCommand, RunningTask, SharedTask,
};
use task::{git_output, git_output_bytes, git_output_with_status, lock, run_logged};

// One allocation for all the copies of a hash, which the maps of parents, children and the graph's
// layout hold for each commit: a `String` each took about 80 bytes a copy.
#[derive(Debug, Default, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommitHash(std::sync::Arc<str>);

impl CommitHash {
    pub fn as_short_hash(&self) -> &str {
        &self.0[0..7]
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for CommitHash {
    fn from(s: &str) -> Self {
        Self(s.into())
    }
}

#[derive(Debug, Default, Clone)]
pub struct Commit {
    pub commit_hash: CommitHash,
    pub author_name: String,
    pub author_email: String,
    pub author_date: DateTime<FixedOffset>,
    pub committer_name: String,
    pub committer_email: String,
    pub committer_date: DateTime<FixedOffset>,
    pub subject: String,
    pub body: String,
    pub parent_commit_hashes: Vec<CommitHash>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ref {
    Tag {
        name: String,
        target: CommitHash,
    },
    Branch {
        name: String,
        target: CommitHash,
    },
    RemoteBranch {
        name: String,
        target: CommitHash,
    },
    Stash {
        name: String,
        message: String,
        target: CommitHash,
    },
}

impl Ref {
    pub fn name(&self) -> &str {
        match self {
            Ref::Tag { name, .. } => name,
            Ref::Branch { name, .. } => name,
            Ref::RemoteBranch { name, .. } => name,
            Ref::Stash { name, .. } => name,
        }
    }

    pub fn target(&self) -> &CommitHash {
        match self {
            Ref::Tag { target, .. } => target,
            Ref::Branch { target, .. } => target,
            Ref::RemoteBranch { target, .. } => target,
            Ref::Stash { target, .. } => target,
        }
    }

    // The name git takes for it alone, as a tag may have a branch's name: refs/heads/x,
    // refs/tags/x, or stash@{0}.
    pub fn git_name(&self) -> String {
        self.named()
            .map_or_else(|| self.name().to_string(), |n| n.full_name())
    }

    // A branch, remote branch or tag; None for a stash.
    pub fn named(&self) -> Option<NamedRef> {
        match self {
            Ref::Branch { name, .. } => Some(NamedRef::Branch(name.clone())),
            Ref::RemoteBranch { name, .. } => Some(NamedRef::RemoteBranch(name.clone())),
            Ref::Tag { name, .. } => Some(NamedRef::Tag(name.clone())),
            Ref::Stash { .. } => None,
        }
    }
}

// A branch, remote branch or tag by its name: a ref to work on, unlike a stash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamedRef {
    Branch(String),
    RemoteBranch(String),
    Tag(String),
}

impl NamedRef {
    pub fn name(&self) -> &str {
        match self {
            NamedRef::Branch(name) | NamedRef::RemoteBranch(name) | NamedRef::Tag(name) => name,
        }
    }

    // The name git can't take for another ref, as a branch and a tag may share a name.
    pub fn full_name(&self) -> String {
        match self {
            NamedRef::Branch(name) => format!("refs/heads/{name}"),
            NamedRef::RemoteBranch(name) => format!("refs/remotes/{name}"),
            NamedRef::Tag(name) => format!("refs/tags/{name}"),
        }
    }
}

// A revision to compare: HEAD, a commit, or a branch, remote branch or tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rev {
    Head,
    Commit(CommitHash),
    Ref(NamedRef),
}

impl Rev {
    // The name git takes for it alone, a ref in full, see `NamedRef::full_name`.
    pub fn git_name(&self) -> String {
        match self {
            Rev::Head => "HEAD".into(),
            Rev::Commit(hash) => hash.as_str().into(),
            Rev::Ref(named) => named.full_name(),
        }
    }

    // As the graph and the sidebar show it.
    pub fn shown(&self) -> String {
        match self {
            Rev::Head => "HEAD".into(),
            Rev::Commit(hash) => hash.as_short_hash().into(),
            Rev::Ref(named) => named.name().into(),
        }
    }
}

// A stash, like "stash@{0}", what it says, and its commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stash {
    pub name: String,
    pub message: String,
    pub target: CommitHash,
}

impl Stash {
    // The number in its name, 2 for "stash@{2}"; None for a name that isn't one.
    pub fn index(&self) -> Option<usize> {
        let index = self.name.strip_prefix("stash@{")?.strip_suffix('}')?;
        index.parse().ok()
    }
}

// A branch on a remote: "origin" and "main" for origin/main.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RemoteBranch {
    pub remote: String,
    pub branch: String,
}

impl RemoteBranch {
    // The remote and the branch on it of a remote branch like "origin/main", one of `remotes`;
    // remote names may contain slashes, the longest matching one owns the branch.
    pub fn parse(remotes: &[String], name: &str) -> Option<RemoteBranch> {
        let remote = remotes
            .iter()
            .filter(|remote| name.starts_with(&format!("{remote}/")))
            .max_by_key(|remote| remote.len())?;
        Some(RemoteBranch {
            remote: remote.clone(),
            branch: name[remote.len() + 1..].to_string(),
        })
    }
}

impl std::fmt::Display for RemoteBranch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.remote, self.branch)
    }
}

#[derive(Debug, Clone)]
pub enum Head {
    // `target` is the commit checked out
    Branch { name: String, target: CommitHash },
    Detached { target: CommitHash },
    None,
}

#[derive(Debug, Clone, Copy)]
pub enum SortCommit {
    Chronological,
    Topological,
}

pub(super) type CommitMap = FxHashMap<CommitHash, Commit>;

pub(super) type CommitsMap = FxHashMap<CommitHash, Vec<CommitHash>>;

pub(super) type RefMap = FxHashMap<CommitHash, Vec<Ref>>;

#[derive(Debug)]
pub struct Repository {
    // the top directory of the working tree (the repository itself when bare), which git
    // commands run in, since the paths they take and give are relative to it
    path: PathBuf,
    // the git directory, to find the state files of operations in without running git
    git_dir: PathBuf,
    // the local changes when loaded, for the sidebar count and the first local changes page
    // to share one `git status`; taken by the page, which shows them as they are after that
    loaded_local_changes: Mutex<Option<LocalChanges>>,
    // when the last ones it holds or handed over started loading, see `set_local_changes`
    local_changes_started: Mutex<Instant>,
    // their count once the page took them, for the page bar not to run `git status` again
    taken_count: Mutex<Option<usize>>,
    // the git command the app runs in the background, shared with it: the repository refuses to
    // be changed meanwhile, see `run`
    running: SharedTask,
    // the fingerprint of the refs when loaded, see `refs_snapshot`
    refs_snapshot: Option<String>,
    // the operation stopped and waiting when loaded, see `operation_in_progress`
    operation: Option<Operation>,
    // the upstream of each local branch having one when loaded, see `branch_tracking`
    tracking: FxHashMap<String, Tracking>,
    // the paths of the submodules tracked in the index when loaded (mode 160000)
    submodules: FxHashSet<String>,
    // the submodules' own repositories, loaded for the pages that show their commits
    submodule_repositories: SubmoduleRepositories,
    commit_map: CommitMap,

    parents_map: CommitsMap,
    children_map: CommitsMap,

    ref_map: RefMap,
    head: Head,
    // the names of the remotes, sorted, as loaded
    remotes: Vec<String>,
    // the diff drivers converting files to text to diff them (`diff.<driver>.textconv`)
    textconv_drivers: Vec<String>,
    // to preserve order of the original commits from `git log`, we store the commit hashes
    commit_hashes: Vec<CommitHash>,
}

impl Repository {
    // The names of the remotes when the repository was loaded, without running git, see
    // `remote_info` for those of now.
    pub fn remotes(&self) -> &[String] {
        &self.remotes
    }

    pub fn commit(&self, commit_hash: &CommitHash) -> Option<&Commit> {
        self.commit_map.get(commit_hash)
    }

    pub fn all_commits(&self) -> Vec<&Commit> {
        self.commit_hashes
            .iter()
            .filter_map(|hash| self.commit(hash))
            .collect()
    }

    pub fn parents_hash(&self, commit_hash: &CommitHash) -> Vec<&CommitHash> {
        self.parents_map
            .get(commit_hash)
            .map(|hs| hs.iter().collect::<Vec<&CommitHash>>())
            .unwrap_or_default()
    }

    pub fn children_hash(&self, commit_hash: &CommitHash) -> Vec<&CommitHash> {
        self.children_map
            .get(commit_hash)
            .map(|hs| hs.iter().collect::<Vec<&CommitHash>>())
            .unwrap_or_default()
    }

    pub fn refs(&self, commit_hash: &CommitHash) -> Vec<&Ref> {
        self.ref_map
            .get(commit_hash)
            .map(|refs| refs.iter().collect::<Vec<&Ref>>())
            .unwrap_or_default()
    }

    pub fn all_refs(&self) -> Vec<&Ref> {
        self.ref_map.values().flatten().collect()
    }

    pub fn head(&self) -> &Head {
        &self.head
    }

    // The loaded commits on the history of HEAD.
    pub fn head_history(&self) -> rustc_hash::FxHashSet<&CommitHash> {
        let tip = match &self.head {
            Head::Branch { name, .. } => self.ref_map.values().flatten().find_map(|r| match r {
                Ref::Branch { name: n, target } if n == name => Some(target),
                _ => None,
            }),
            Head::Detached { target } => Some(target),
            Head::None => None,
        };
        let mut history = rustc_hash::FxHashSet::default();
        let mut pending: Vec<&CommitHash> = tip.into_iter().collect();
        while let Some(hash) = pending.pop() {
            if self.commit_map.contains_key(hash) && history.insert(hash) {
                pending.extend(self.parents_map.get(hash).into_iter().flatten());
            }
        }
        history
    }

    pub fn remote_url(&self, remote: &str) -> Option<String> {
        git_output(&self.path, &["remote", "get-url", remote])
            .ok()
            .map(|url| url.trim().to_string())
    }

    // The branch checked out, None when HEAD is detached.
    pub fn current_branch(&self) -> Option<String> {
        match &self.head {
            Head::Branch { name, .. } => Some(name.clone()),
            _ => None,
        }
    }

    // The git directory, like `.git`.
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    // The commit checked out, as loaded; None in a repository without commits.
    pub fn head_commit(&self) -> Option<CommitHash> {
        match &self.head {
            Head::Branch { target, .. } | Head::Detached { target } => Some(target.clone()),
            Head::None => None,
        }
    }

    // Orders commits as in the commit list, newest first, in one pass over the list;
    // commits not in it go last.
    pub fn sort_newest_first(&self, hashes: &mut [CommitHash]) {
        let wanted: rustc_hash::FxHashSet<&CommitHash> = hashes.iter().collect();
        let positions: FxHashMap<&CommitHash, usize> = self
            .commit_hashes
            .iter()
            .enumerate()
            .filter(|(_, h)| wanted.contains(h))
            .map(|(i, h)| (h, i))
            .collect();
        let position = |h: &CommitHash| positions.get(h).copied().unwrap_or(usize::MAX);
        hashes.sort_by_cached_key(position);
    }

    // The parents of a commit, asked of git as they may not be loaded (with `--max-count`).
    pub fn parents_of(&self, commit: &str) -> std::result::Result<Vec<CommitHash>, String> {
        let out = git_output(
            &self.path,
            &["rev-list", "--parents", "-n", "1", commit, "--"],
        )?;
        Ok(out
            .split_whitespace()
            .skip(1)
            .map(CommitHash::from)
            .collect())
    }

    // How many commits HEAD is after `commit`, on the history of HEAD.
    pub fn count_after(&self, commit: &CommitHash) -> std::result::Result<usize, String> {
        // up to HEAD, which git takes when the end is left out
        let range = format!("{}..", commit.as_str());
        let out = git_output(&self.path, &["rev-list", "--count", &range, "--"])?;
        out.trim().parse().map_err(|e| format!("{e}"))
    }

    // The upstream of the current branch, like "origin/main", when it has the commit: rewriting
    // the commit then needs a force push.
    pub fn upstream_containing(&self, commit: &str) -> Option<String> {
        let upstream = git_output(
            &self.path,
            &["rev-parse", "--symbolic-full-name", "@{upstream}"],
        )
        .ok()?;
        let contains = format!("--contains={commit}");
        let args = [
            "for-each-ref",
            &contains,
            "--format=%(refname:short)",
            upstream.trim(),
        ];
        let found = git_output(&self.path, &args).ok()?;
        Some(found.trim().to_string()).filter(|b| !b.is_empty())
    }

    // The local branches of `names` whose last commit is neither in HEAD nor on another branch,
    // local or remote, but those of `names`, which are deleted with them: their commits are lost.
    pub fn unmerged_branches(&self, names: &[String]) -> Vec<String> {
        // those merged into HEAD, in one call; the full names, a tag may have the same short one
        let args = [
            "for-each-ref",
            "--merged",
            "--format=%(refname)",
            "refs/heads",
        ];
        let merged = git_output(&self.path, &args).unwrap_or_default();
        let merged: FxHashSet<&str> = merged
            .lines()
            .filter_map(|r| r.strip_prefix("refs/heads/"))
            .collect();
        let deleted: FxHashSet<String> = names.iter().map(|n| format!("refs/heads/{n}")).collect();
        let on_other_branch = |branch: &str| {
            let contains = format!("--contains=refs/heads/{branch}");
            let args = [
                "for-each-ref",
                &contains,
                "--format=%(refname)",
                "refs/heads",
                "refs/remotes",
            ];
            git_output(&self.path, &args)
                .unwrap_or_default()
                .lines()
                .any(|r| !deleted.contains(r))
        };
        names
            .iter()
            .filter(|name| !merged.contains(name.as_str()) && !on_other_branch(name))
            .cloned()
            .collect()
    }

    // The commit a revision like "HEAD~2" names.
    pub fn resolve(&self, revision: &str) -> Option<CommitHash> {
        let revision = format!("{revision}^{{commit}}");
        git_output(&self.path, &["rev-parse", "--verify", "--quiet", &revision])
            .ok()
            .map(|hash| CommitHash::from(hash.trim()))
    }

    // The commits after `base` (all of them when None) up to HEAD, with their parents.
    pub fn commits_after(
        &self,
        base: Option<&CommitHash>,
    ) -> std::result::Result<FxHashMap<CommitHash, Vec<CommitHash>>, String> {
        let exclude = base.map(|base| format!("^{}", base.as_str()));
        let mut args = vec!["rev-list", "--parents", "HEAD"];
        args.extend(exclude.as_deref());
        args.push("--");
        let out = git_output(&self.path, &args)?;
        Ok(out
            .lines()
            .filter_map(|line| {
                let mut hashes = line.split_whitespace().map(CommitHash::from);
                Some((hashes.next()?, hashes.collect()))
            })
            .collect())
    }

    // The operation stopped (on a conflict, or to edit a commit) and waiting to be finished, if
    // any, as loaded.
    pub fn operation_in_progress(&self) -> Option<Operation> {
        self.operation
    }

    // The commits each local branch with an upstream has that it lacks (ahead, to push) and the
    // other way round (behind, to pull), by branch name.
    pub fn branch_tracking(&self) -> &FxHashMap<String, Tracking> {
        &self.tracking
    }

    // The fingerprint of the refs as loaded, see `refs_snapshot`.
    pub fn refs_snapshot(&self) -> Option<&str> {
        self.refs_snapshot.as_deref()
    }

    // The remotes, local branches and their upstreams, to fetch, pull and push with.
    pub fn remote_info(&self) -> RemoteInfo {
        // the remotes and the settings of pulling, in one call
        let pattern = r"^(remote\..*\.url|pull\.rebase|rebase\.autostash|merge\.autostash)$";
        let config =
            git_output(&self.path, &["config", "-z", "--get-regexp", pattern]).unwrap_or_default();
        let mut remotes = vec![];
        let mut settings = FxHashMap::default();
        for entry in config.split('\0').filter(|e| !e.is_empty()) {
            // a key, then its value on the next line (none for a boolean set by its name alone)
            let (key, value) = entry.split_once('\n').unwrap_or((entry, "true"));
            match key
                .strip_prefix("remote.")
                .and_then(|k| k.strip_suffix(".url"))
            {
                Some(remote) if !remotes.iter().any(|r| r == remote) => {
                    remotes.push(remote.to_string())
                }
                Some(_) => {}
                // the last value wins, like `git config --get`
                None => {
                    settings.insert(key.to_string(), value.to_string());
                }
            }
        }
        remotes.sort();
        let setting = |key: &str| settings.get(key).map(String::as_str);

        // the branches of the remotes and the local ones with their upstreams, in one call
        let format = "--format=%(refname)%00%(upstream:remotename)%00%(upstream:remoteref)";
        let refs = git_output(
            &self.path,
            &["for-each-ref", format, "refs/heads", "refs/remotes"],
        )
        .unwrap_or_default();
        let mut remote_branches: FxHashMap<String, Vec<String>> = FxHashMap::default();
        let mut branches = vec![];
        let mut upstreams = FxHashMap::default();
        for line in refs.lines() {
            let parts: Vec<&str> = line.split('\0').collect();
            let [refname, remote, remote_ref] = parts[..] else {
                continue;
            };
            if let Some(name) = refname.strip_prefix("refs/heads/") {
                branches.push(name.to_string());
                if let Some(branch) = remote_ref.strip_prefix("refs/heads/") {
                    if !remote.is_empty() {
                        let upstream = RemoteBranch {
                            remote: remote.to_string(),
                            branch: branch.to_string(),
                        };
                        upstreams.insert(name.to_string(), upstream);
                    }
                }
            } else if let Some(name) = refname.strip_prefix("refs/remotes/") {
                if let Some(RemoteBranch { remote, branch }) = RemoteBranch::parse(&remotes, name) {
                    if branch != "HEAD" {
                        remote_branches.entry(remote).or_default().push(branch);
                    }
                }
            }
        }

        RemoteInfo {
            current_branch: self.current_branch(),
            remotes,
            remote_branches,
            branches,
            upstreams,
            // pull.rebase may also be "merges" or "interactive", which rebase too
            pull_rebase: setting("pull.rebase").is_some_and(|v| parse_git_bool(v) != Some(false)),
            // rebasing stashes unless told not to, like lazygit, which always does
            rebase_auto_stash: setting("rebase.autostash").and_then(parse_git_bool) != Some(false),
            merge_auto_stash: setting("merge.autostash").and_then(parse_git_bool) == Some(true),
        }
    }

    // A git config value of the repository, None when it is not set.
    pub fn config(&self, key: &str) -> Option<String> {
        git_output(&self.path, &["config", "--get", key])
            .ok()
            .map(|v| v.trim().to_string())
    }

    // Whether git signs the commits or tags it makes, which may ask for a passphrase in the
    // terminal (a pinentry like pinentry-curses).
    pub fn signs(&self) -> bool {
        self.config_bool("commit.gpgSign") == Some(true)
            || self.config_bool("tag.gpgSign") == Some(true)
    }

    // A boolean git config value, None when it is not set (or not a boolean).
    pub fn config_bool(&self, key: &str) -> Option<bool> {
        self.config(key).as_deref().and_then(parse_git_bool)
    }

    // The top directory of the working tree, which file paths of local changes are relative to.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

// A boolean as git reads it in its config, None when it isn't one.
pub fn parse_git_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" | "" => Some(false),
        _ => None,
    }
}

// An operation that can stop on conflicts, leaving the repository in the middle of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Rebase,
    Merge,
    CherryPick,
    Revert,
}

impl Operation {
    pub fn name(self) -> &'static str {
        match self {
            Operation::Rebase => "rebase",
            Operation::Merge => "merge",
            Operation::CherryPick => "cherry-pick",
            Operation::Revert => "revert",
        }
    }

    // A rebase goes on with its own commits, the others are finished by committing.
    pub fn continues_without_commit(self) -> bool {
        self == Operation::Rebase
    }
}

#[derive(Debug, Default, Clone)]
pub struct RemoteInfo {
    pub current_branch: Option<String>,
    pub remotes: Vec<String>,
    // the branches of each remote, without the remote name
    pub remote_branches: FxHashMap<String, Vec<String>>,
    pub branches: Vec<String>,
    // local branch -> (remote, branch on the remote)
    pub upstreams: FxHashMap<String, RemoteBranch>,
    pub pull_rebase: bool,
    // whether pulling stashes local changes, when rebasing and when merging
    pub rebase_auto_stash: bool,
    pub merge_auto_stash: bool,
}

impl RemoteInfo {
    // The upstream remote of `branch`, else origin, else the first one; None without remotes.
    pub fn default_remote(&self, branch: Option<&str>) -> Option<&String> {
        branch
            .and_then(|branch| self.upstreams.get(branch))
            .map(|upstream| &upstream.remote)
            .filter(|remote| self.remotes.contains(remote))
            .or_else(|| self.remotes.iter().find(|r| *r == "origin"))
            .or_else(|| self.remotes.first())
    }

    // Whether pulling stashes local changes by default, by the config of how it pulls.
    pub fn auto_stash(&self, rebase: bool) -> bool {
        if rebase {
            self.rebase_auto_stash
        } else {
            self.merge_auto_stash
        }
    }
}

// A local branch's upstream, like "origin/main", and how many commits the branch is ahead of it
// and behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tracking {
    pub upstream: String,
    pub ahead: usize,
    pub behind: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::load::load_branch_tracking;

    #[test]
    fn test_unmerged_branches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let git = crate::test_support::git::GitRepository::new(path);
        git.init();
        git.commit("init", "2024-01-01");
        // behind HEAD, ahead of it, and ahead of it but also on another branch
        git_output(path, &["branch", "behind"]).unwrap();
        git.commit("second", "2024-01-02");
        git.checkout_b("ahead");
        git.commit("third", "2024-01-03");
        git_output(path, &["branch", "copy"]).unwrap();
        // and ahead of it, only pushed, with no upstream set
        git.checkout_b("pushed");
        git.commit("fourth", "2024-01-04");
        git_output(path, &["update-ref", "refs/remotes/origin/pushed", "HEAD"]).unwrap();
        git.checkout("master");
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();

        let names = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        let unmerged = |n: &[&str]| repository.unmerged_branches(&names(n));
        assert_eq!(
            unmerged(&["behind", "ahead", "pushed"]),
            Vec::<String>::new()
        );
        git_output(path, &["update-ref", "-d", "refs/remotes/origin/pushed"]).unwrap();
        assert_eq!(unmerged(&["pushed"]), ["pushed"]);
        // deleted together, the other branches having them go too
        let all = ["ahead", "copy", "pushed"];
        assert_eq!(unmerged(&all), all);
    }

    #[test]
    fn test_push_fetch_and_remote_info() {
        use crate::test_support::git::GitRepository;

        let dir = tempfile::tempdir().unwrap();
        let remote = dir.path().join("remote.git");
        let work = dir.path().join("work");
        std::fs::create_dir(&work).unwrap();
        git_output(dir.path(), &["init", "-q", "--bare", "remote.git"]).unwrap();
        let repo = GitRepository::new(&work);
        repo.init();
        repo.commit("first", "2024-01-01");
        git_output(
            &work,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        )
        .unwrap();

        let run = |command, args: &[&str]| {
            run_task(
                &work,
                &GitTask::new(command, args, String::new(), String::new()),
            )
        };
        let push = [
            "--set-upstream",
            "origin",
            "refs/heads/master:refs/heads/main",
        ];
        run(GitCommand::Push, &push).unwrap();
        run(GitCommand::Fetch, &["--all"]).unwrap();

        let repository = Repository::load(&work, SortCommit::Chronological, None, false).unwrap();
        let info = repository.remote_info();
        assert_eq!(info.current_branch.as_deref(), Some("master"));
        assert_eq!(info.remotes, vec!["origin"]);
        assert_eq!(info.branches, vec!["master"]);
        assert_eq!(info.remote_branches["origin"], vec!["main"]);
        assert_eq!(info.upstreams["master"].to_string(), "origin/main");
        // unset, rebasing stashes and merging doesn't; set, they follow it
        assert!(info.rebase_auto_stash && !info.merge_auto_stash);
        git_output(&work, &["config", "rebase.autoStash", "false"]).unwrap();
        assert!(!repository.remote_info().rebase_auto_stash);

        // the reason is reported, not the hints after it
        let err = run(GitCommand::Fetch, &["nowhere"]).unwrap_err();
        assert!(err.starts_with("git fetch failed: fatal:"), "{err}");

        // in sync, then two commits to push and, once they are pushed and one undone, one to pull
        let tracking = || {
            let tracking = &load_branch_tracking(&work)["master"];
            assert_eq!(tracking.upstream, "origin/main");
            (tracking.ahead, tracking.behind)
        };
        assert_eq!(tracking(), (0, 0));
        repo.commit("second", "2024-01-02");
        repo.commit("third", "2024-01-03");
        assert_eq!(tracking(), (2, 0));
        run(GitCommand::Push, &["origin", "master:main"]).unwrap();
        git_output(&work, &["reset", "-q", "--hard", "HEAD~1"]).unwrap();
        assert_eq!(tracking(), (0, 1));
        // also with a tag of the same name
        git_output(&work, &["tag", "master"]).unwrap();
        assert_eq!(tracking(), (0, 1));
    }
}
