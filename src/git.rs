use std::ffi::OsStr;
use std::{
    collections::VecDeque,
    hash::Hash,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::Mutex,
    time::{Duration, Instant},
};

use chrono::{DateTime, FixedOffset, Local};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::Result;

#[derive(Debug, Default, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommitHash(String);

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
        Self(s.to_string())
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

// Which way git applies a patch: forward, making its changes (staging them), or in reverse, taking
// them back (unstaging or discarding them). A partial patch is made for its direction, see
// `DiffHunk::partial_patch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchDirection {
    Forward,
    Reverse,
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

// A branch on a remote: "origin" and "main" for origin/main.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RemoteBranch {
    pub remote: String,
    pub branch: String,
}

impl std::fmt::Display for RemoteBranch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.remote, self.branch)
    }
}

// A file or directory of the repository, by its path from the top.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry<'a> {
    Dir(&'a str),
    File(&'a str),
}

impl<'a> Entry<'a> {
    pub fn path(self) -> &'a str {
        match self {
            Entry::Dir(path) | Entry::File(path) => path,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Head {
    Branch { name: String },
    Detached { target: CommitHash },
    None,
}

#[derive(Debug, Clone, Copy)]
pub enum SortCommit {
    Chronological,
    Topological,
}

type CommitMap = FxHashMap<CommitHash, Commit>;
type CommitsMap = FxHashMap<CommitHash, Vec<CommitHash>>;

type RefMap = FxHashMap<CommitHash, Vec<Ref>>;

#[derive(Debug)]
pub struct Repository {
    // the top directory of the working tree (the repository itself when bare), which git
    // commands run in, since the paths they take and give are relative to it
    path: PathBuf,
    // the git directory, to find the state files of operations in without running git
    git_dir: Option<PathBuf>,
    // the local changes when loaded, for the sidebar count and the first local changes page
    // to share one `git status`; taken by the page, which shows them as they are after that
    loaded_local_changes: std::cell::RefCell<Option<LocalChanges>>,
    // what runs in the background, the repository refusing to be changed meanwhile, see `run`
    busy: std::cell::RefCell<Option<String>>,
    // the fingerprint of the refs when loaded, see `refs_snapshot`
    refs_snapshot: Option<String>,
    // the upstream of each local branch having one when loaded, see `branch_tracking`
    tracking: FxHashMap<String, Tracking>,
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
    #[cfg(test)]
    pub fn load(
        path: &Path,
        sort: SortCommit,
        max_count: Option<usize>,
        mailmap: bool,
    ) -> Result<Self> {
        Self::load_scoped(path, sort, max_count, mailmap, None)
    }

    // `revisions` loads only their history, like ["HEAD"] for the current branch; else that of
    // every branch, tag and stash.
    pub fn load_scoped(
        path: &Path,
        sort: SortCommit,
        max_count: Option<usize>,
        mailmap: bool,
        revisions: Option<&[String]>,
    ) -> Result<Self> {
        let (toplevel, git_dir) = locate_repository(path)?;
        let path = toplevel.as_deref().unwrap_or(path);
        // what doesn't depend on the commits loads alongside them
        std::thread::scope(|scope| {
            let local_changes = scope.spawn(|| load_local_changes(path));
            let remotes = scope.spawn(|| load_remotes(path));
            let textconv_drivers = scope.spawn(|| load_textconv_drivers(path));
            let refs_snapshot = scope.spawn(|| refs_snapshot(path).ok());
            let tracking = scope.spawn(|| load_branch_tracking(path));
            let mut repository = Self::load_commits(path, sort, max_count, mailmap, revisions)?;
            repository.git_dir = git_dir;
            repository.remotes = remotes.join().unwrap_or_default();
            repository.textconv_drivers = textconv_drivers.join().unwrap_or_default();
            repository.loaded_local_changes = local_changes.join().ok().into();
            repository.refs_snapshot = refs_snapshot.join().unwrap_or_default();
            repository.tracking = tracking.join().unwrap_or_default();
            Ok(repository)
        })
    }

    // The commits and refs, see `load_scoped`.
    fn load_commits(
        path: &Path,
        sort: SortCommit,
        max_count: Option<usize>,
        mailmap: bool,
        revisions: Option<&[String]>,
    ) -> Result<Self> {
        let (mut ref_map, head) = load_refs(path);

        let (stashes, stash_ref_map) = load_stashes(path, mailmap);
        // a graph of some refs leaves the stashes out, they stay in the sidebar
        let stashes = if revisions.is_some() { vec![] } else { stashes };
        let commits = load_all_commits(path, sort, &head, &stashes, max_count, mailmap, revisions)?;
        if commits.is_empty() {
            return Err("no commits in the repository".into());
        }

        let commits = merge_stashes_to_commits(commits, stashes);
        let commit_hashes = commits.iter().map(|c| c.commit_hash.clone()).collect();

        let (parents_map, children_map) = build_commits_maps(&commits);
        let commit_map = to_commit_map(commits);

        merge_ref_maps(&mut ref_map, stash_ref_map);

        Ok(Self::new(
            path.to_path_buf(),
            commit_map,
            parents_map,
            children_map,
            ref_map,
            head,
            commit_hashes,
        ))
    }

    pub fn new(
        path: PathBuf,
        commit_map: CommitMap,
        parents_map: CommitsMap,
        children_map: CommitsMap,
        ref_map: RefMap,
        head: Head,
        commit_hashes: Vec<CommitHash>,
    ) -> Self {
        Self {
            path,
            git_dir: None,
            loaded_local_changes: Default::default(),
            busy: Default::default(),
            refs_snapshot: None,
            tracking: Default::default(),
            commit_map,
            parents_map,
            children_map,
            ref_map,
            head,
            remotes: Vec::new(),
            textconv_drivers: Vec::new(),
            commit_hashes,
        }
    }

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
            Head::Branch { name } => self.ref_map.values().flatten().find_map(|r| match r {
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
            Head::Branch { name } => Some(name.clone()),
            _ => None,
        }
    }

    pub fn commit_detail(&self, commit_hash: &CommitHash) -> (Commit, Vec<FileChange>) {
        let commit = self.commit(commit_hash).unwrap().clone();
        let changes = self
            .commit_base(commit_hash)
            .map(|base| diff_summary(&self.path, &base, commit_hash.as_str()))
            .unwrap_or_default();
        (commit, changes)
    }

    // What a commit changed is compared to: its first parent, or the empty tree for a root commit.
    fn commit_base(&self, commit_hash: &CommitHash) -> Option<String> {
        match self.parents_hash(commit_hash).first() {
            Some(parent) => Some(parent.as_str().to_string()),
            None => empty_tree(&self.path).ok(),
        }
    }

    pub fn local_changes(&self) -> LocalChanges {
        load_local_changes(&self.path)
    }

    // The number of local changes, as they were when the repository was loaded.
    pub fn loaded_local_change_count(&self) -> usize {
        let mut loaded = self.loaded_local_changes.borrow_mut();
        loaded.get_or_insert_with(|| self.local_changes()).count()
    }

    // The local changes as they were when the repository was loaded the first time it is called,
    // so the sidebar and the page share one `git status`, and as they are now after that.
    // The local changes loaded again after the working tree changed, for the local changes page to
    // show them; returns how many there are.
    pub fn set_local_changes(&self, changes: LocalChanges) -> usize {
        let count = changes.count();
        *self.loaded_local_changes.borrow_mut() = Some(changes);
        count
    }

    // The git directory, None when git can't tell.
    pub fn git_dir(&self) -> Option<&Path> {
        self.git_dir.as_deref()
    }

    // See `files_stamp`.
    pub fn files_stamp(&self, changes: &LocalChanges) -> u64 {
        files_stamp(&self.path, self.git_dir(), changes)
    }

    pub fn take_loaded_local_changes(&self) -> LocalChanges {
        self.loaded_local_changes
            .take()
            .unwrap_or_else(|| self.local_changes())
    }

    pub fn local_change_diff(&self, paths: &[&str], of: DiffOf) -> String {
        if paths.is_empty() {
            return String::new();
        }
        load_local_change_diff(&self.path, paths, of)
    }

    // The diffs of untracked files, which git diffs only one at a time (against /dev/null), so a few
    // run side by side.
    pub fn untracked_diff(&self, paths: &[&str]) -> String {
        // each worker is a git process; more than 8 at once only crowd the disk and the cores
        // the app draws with, 4 when the core count is unknown
        let workers = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
        let chunk = paths.len().div_ceil(workers).max(1);
        let work_tree = self.path.as_path();
        std::thread::scope(|scope| {
            let diffs: Vec<_> = paths
                .chunks(chunk)
                .map(|chunk| {
                    scope.spawn(move || {
                        chunk
                            .iter()
                            .map(|path| {
                                load_local_change_diff(work_tree, &[path], DiffOf::Untracked)
                            })
                            .collect::<String>()
                    })
                })
                .collect();
            diffs
                .into_iter()
                .map(|diff| diff.join().unwrap_or_default())
                .collect()
        })
    }

    // The tree to compare the index to while amending: the parent of HEAD, or the empty tree for a root commit.
    pub fn amend_base(&self) -> std::result::Result<String, String> {
        git_output(&self.path, &["rev-parse", "--verify", "--quiet", "HEAD^"])
            .map(|s| s.trim().to_string())
            .or_else(|_| empty_tree(&self.path))
    }

    // The diff of `paths` (all files when empty) made by a commit, against its first parent.
    pub fn commit_diff(&self, commit_hash: &CommitHash, paths: &[&str]) -> String {
        self.commit_base(commit_hash)
            .map(|base| self.diff_between(&base, commit_hash.as_str(), paths))
            .unwrap_or_default()
    }

    // The diff of `paths` (all files when empty) from one commit to another.
    pub fn diff_between(&self, base: &str, target: &str, paths: &[&str]) -> String {
        let mut args = PATCH_DIFF_ARGS.to_vec();
        args.extend_from_slice(&["-M", base, target, "--"]);
        let paths = pathspecs(paths);
        args.extend(paths.iter().map(String::as_str));
        git_output(&self.path, &args).unwrap_or_default()
    }

    // The files changed from one commit to another.
    pub fn diff_summary_between(&self, base: &CommitHash, target: &CommitHash) -> Vec<FileChange> {
        diff_summary(&self.path, base.as_str(), target.as_str())
    }

    // The commit checked out, None in a repository without commits.
    pub fn head_commit(&self) -> Option<CommitHash> {
        git_output(&self.path, &["rev-parse", "--verify", "--quiet", "HEAD"])
            .ok()
            .map(|hash| CommitHash::from(hash.trim()))
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

    // How many commits `head` is after `commit`, on the history of `head`.
    pub fn count_after(
        &self,
        commit: &CommitHash,
        head: &str,
    ) -> std::result::Result<usize, String> {
        let range = format!("{}..{head}", commit.as_str());
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
            "--merged=HEAD",
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

    // The commits after `base` (all of them when None) up to `head`, with their parents.
    pub fn commits_after(
        &self,
        base: Option<&CommitHash>,
        head: &str,
    ) -> std::result::Result<FxHashMap<CommitHash, Vec<CommitHash>>, String> {
        let exclude = base.map(|base| format!("^{}", base.as_str()));
        let mut args = vec!["rev-list", "--parents", head];
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

    // The first of `steps` that conflicts, applied one after another on `start` in memory with
    // `git merge-tree`, like a rebase or a cherry-pick does: each is (base, commit), the changes
    // from base to commit, or a merge of commit when base is None. None when all apply, or git
    // can't tell (git before 2.38).
    pub fn first_conflict(
        &self,
        start: &str,
        steps: &[(Option<String>, String)],
    ) -> Option<Conflict> {
        let mut tree = start.to_string();
        // shortcut: checks the first 100 steps only, so a popup opens quickly; move it to a
        // background thread if long rebases are common
        for (base, commit) in steps.iter().take(MAX_CONFLICT_STEPS) {
            let base = base.as_ref().map(|b| format!("--merge-base={b}"));
            let mut args = vec!["merge-tree", "--write-tree", "--name-only", "--no-messages"];
            args.extend(base.as_deref());
            args.extend([tree.as_str(), commit.as_str()]);
            let (status, out) = git_output_with_status(&self.path, &args)?;
            let mut lines = out.lines();
            let result = lines.next()?.to_string();
            match status {
                Some(0) => tree = result,
                Some(1) => {
                    let files = lines.take_while(|l| !l.is_empty()).map(String::from);
                    return Some(Conflict::At(commit.clone(), files.collect()));
                }
                _ => return None,
            }
        }
        let unchecked = steps.len().saturating_sub(MAX_CONFLICT_STEPS);
        (unchecked > 0).then_some(Conflict::Unchecked(unchecked))
    }

    // The first conflict replaying the commits of `range` (rev-list arguments) on `onto`, like a
    // rebase, without the merges and the commits in `skip`, see `first_conflict`.
    pub fn replay_conflict(
        &self,
        onto: &str,
        range: &[&str],
        skip: &[CommitHash],
    ) -> Option<Conflict> {
        let mut args = vec!["rev-list", "--reverse", "--no-merges"];
        args.extend(range);
        let out = git_output(&self.path, &args).ok()?;
        let steps: Vec<_> = out
            .lines()
            .filter(|c| !skip.iter().any(|s| s.as_str() == *c))
            .map(|c| (Some(format!("{c}^")), c.to_string()))
            .collect();
        self.first_conflict(onto, &steps)
    }

    // The contents of a file at a commit.
    pub fn commit_file_content(
        &self,
        commit_hash: &CommitHash,
        path: &str,
    ) -> std::result::Result<Vec<u8>, String> {
        let object = format!("{}:{path}", commit_hash.as_str());
        git_output_bytes(&self.path, &["cat-file", "blob", &object])
    }

    // All file paths in the tree of a commit.
    pub fn commit_tree_paths(&self, commit_hash: &CommitHash) -> Vec<String> {
        let args = ["ls-tree", "-r", "-z", "--name-only", commit_hash.as_str()];
        git_output(&self.path, &args)
            .map(|out| {
                out.split('\0')
                    .filter(|p| !p.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default()
    }

    // Who last changed each line of the file at `path` as of the commit, see `Blame`.
    pub fn blame(
        &self,
        commit_hash: &CommitHash,
        path: &str,
    ) -> std::result::Result<Blame, String> {
        let args = ["blame", "--porcelain", commit_hash.as_str(), "--", path];
        git_output(&self.path, &args).map(|out| parse_blame(&out))
    }

    // The commits changing the file at `path` in the history of HEAD and of `start`, renames
    // followed, the newest first; with the index of the last one changing it as of `start`.
    pub fn file_history(
        &self,
        start: &CommitHash,
        path: &str,
    ) -> std::result::Result<(Vec<FileRevision>, usize), String> {
        let format = "--format=%x01%H%x00%aI%x00%s";
        let args = [
            "log",
            "--follow",
            "--name-only",
            "-z",
            format,
            "HEAD",
            start.as_str(),
        ];
        let out = git_output(&self.path, &[&args[..], &["--", path]].concat())?;
        let history: Vec<FileRevision> = out
            .split('\x01')
            .filter_map(|record| {
                let mut fields = record.split('\0');
                let (commit, date) = (fields.next()?, fields.next()?);
                let (subject, path) = (fields.next()?, fields.next()?);
                Some(FileRevision {
                    commit: CommitHash::from(commit),
                    date: parse_iso_date(date),
                    subject: subject.into(),
                    path: path.trim_start_matches('\n').into(),
                })
            })
            .collect();
        // walked as the history is, which leaves out merges
        let args = [
            "log",
            "-1",
            "--follow",
            "--format=%H",
            start.as_str(),
            "--",
            path,
        ];
        let last = git_output(&self.path, &args)?;
        let index = history
            .iter()
            .position(|r| r.commit.as_str() == last.trim())
            .ok_or_else(|| format!("{path} has no history"))?;
        Ok((history, index))
    }

    // Changes between `base` and the index, i.e. what would be committed when amending.
    pub fn staged_changes_against(&self, base: &str) -> Vec<LocalChangeFile> {
        let args = ["diff", "--cached", "--name-status", "-z", "-M", base];
        git_output(&self.path, &args)
            .map(|out| parse_name_status(out.as_bytes()))
            .unwrap_or_default()
    }

    pub fn head_commit_message(&self) -> std::result::Result<String, String> {
        git_output(&self.path, &["log", "-1", "--format=%B", "HEAD"])
            .map(|s| s.trim_end().to_string())
    }

    pub fn stage_paths(&self, paths: &[&str]) -> std::result::Result<(), String> {
        let mut args = vec!["add", "--all", "--"];
        let paths = pathspecs(paths);
        args.extend(paths.iter().map(String::as_str));
        self.run(&args, None)
    }

    // Resets the index entries of `paths` to `staged_base`, HEAD when None.
    pub fn unstage_paths(
        &self,
        paths: &[&str],
        staged_base: Option<&str>,
    ) -> std::result::Result<(), String> {
        let mut args = vec!["reset", "--quiet"];
        args.extend(staged_base);
        args.push("--");
        let paths = pathspecs(paths);
        args.extend(paths.iter().map(String::as_str));
        self.run(&args, None)
    }

    // The commands discarding the unstaged changes of `tracked` files (restoring them from the
    // index) and deleting `untracked` files. `conflicted` files, which git cannot restore from the
    // index, go back to HEAD, or are deleted when HEAD does not have them, like lazygit does.
    pub fn discard_commands(
        &self,
        tracked: &[&str],
        untracked: &[&str],
        conflicted: &[&str],
    ) -> std::result::Result<Vec<GitArgs>, String> {
        // git status lists an untracked repository inside as `nested/`, which `clean -f` skips
        if let Some(path) = untracked.iter().find(|path| path.ends_with('/')) {
            return Err(format!(
                "{path} is a repository of its own, delete it yourself"
            ));
        }
        let mut tracked = tracked.to_vec();
        let mut untracked = untracked.to_vec();
        // restoring and cleaning files moves no HEAD, which an operation stopped allows
        let command = |command: GitCommand, args: &[&str], paths: &[&str]| {
            let mut command = command.args(args.iter().copied());
            command.args.push("--".into());
            command.args.extend(pathspecs(paths));
            command
        };
        let mut commands = vec![];
        if !conflicted.is_empty() {
            commands.push(command(
                GitCommand::Restore,
                &["--staged", "--quiet"],
                conflicted,
            ));
            let mut args = vec!["ls-tree", "-z", "--name-only", "HEAD", "--"];
            args.extend_from_slice(conflicted);
            let in_head = git_output(&self.path, &args)?;
            let in_head: Vec<&str> = in_head.split('\0').collect();
            for path in conflicted {
                if in_head.contains(path) {
                    tracked.push(path);
                } else {
                    untracked.push(path);
                }
            }
        }
        if !tracked.is_empty() {
            commands.push(command(GitCommand::Restore, &["--quiet"], &tracked));
        }
        if !untracked.is_empty() {
            commands.push(command(
                GitCommand::Clean,
                &["--force", "--quiet"],
                &untracked,
            ));
        }
        Ok(commands)
    }

    // Adds the file or directory to the .gitignore at the top of the working tree, anchored
    // there. Returns what to tell: a tracked path stays tracked, ignoring only keeps untracked
    // files out.
    pub fn ignore_path(&self, entry: Entry) -> std::result::Result<String, String> {
        let pattern = gitignore_pattern(entry);
        let file = self.path.join(".gitignore");
        let content = std::fs::read_to_string(&file).unwrap_or_default();
        if content.lines().any(|line| line == pattern) {
            return Ok(format!("{pattern} is already in .gitignore"));
        }
        let separator = if content.is_empty() || content.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        std::fs::write(&file, format!("{content}{separator}{pattern}\n"))
            .map_err(|e| format!("failed to write .gitignore: {e}"))?;
        let specs = pathspecs(&[entry.path()]);
        let tracked =
            git_output(&self.path, &["ls-files", "--", &specs[0]]).is_ok_and(|out| !out.is_empty());
        Ok(if tracked {
            format!(
                "Added {pattern} to .gitignore; it is tracked, so git still follows its changes"
            )
        } else {
            format!("Added {pattern} to .gitignore")
        })
    }

    // The operation stopped (on a conflict, or to edit a commit) and waiting to be finished, if any.
    pub fn operation_in_progress(&self) -> Option<Operation> {
        let exists = |name: &str| self.git_path(name).is_some_and(|p| p.exists());
        // rebase-apply is also used by `git am`, which does not leave the "rebasing" file
        if exists("rebase-merge") || exists("rebase-apply/rebasing") {
            Some(Operation::Rebase)
        } else if exists("MERGE_HEAD") {
            Some(Operation::Merge)
        } else if exists("CHERRY_PICK_HEAD") {
            Some(Operation::CherryPick)
        } else if exists("REVERT_HEAD") {
            Some(Operation::Revert)
        } else {
            None
        }
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

    // Takes the refs as they are now as loaded, after reloading failed: else the change that made
    // it fail would reload again and again.
    pub fn take_refs_snapshot(&mut self) {
        self.refs_snapshot = refs_snapshot(&self.path).ok();
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
                if let Some((owner, branch)) = split_remote_branch(&remotes, name) {
                    if branch != "HEAD" {
                        remote_branches
                            .entry(owner.to_string())
                            .or_default()
                            .push(branch.to_string());
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
            rebase_auto_stash: setting("rebase.autostash").and_then(parse_git_bool) == Some(true),
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

    // Refuses changes to the repository while `running` (a git command in the background), which
    // could fail on its locked index or end up in the middle of its stash and pop, like lazygit.
    pub fn set_busy(&self, running: Option<String>) {
        *self.busy.borrow_mut() = running;
    }

    // Runs a git command changing the repository, unless one runs in the background, see `set_busy`.
    fn run(&self, args: &[&str], stdin: Option<&str>) -> std::result::Result<(), String> {
        self.check_idle()?;
        run_logged(
            git_command().args(args).current_dir(&self.path),
            args,
            stdin,
            None,
        )
    }

    // An error while a git command runs in the background, see `set_busy`.
    fn check_idle(&self) -> std::result::Result<(), String> {
        match self.busy.borrow().as_ref() {
            Some(running) => Err(format!("{running}, wait for it to finish")),
            None => Ok(()),
        }
    }

    // The top directory of the working tree, which file paths of local changes are relative to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    // The content of a conflicted file on one side, None when that side deleted it.
    pub fn conflict_side_content(&self, path: &str, side: ConflictSide) -> Option<String> {
        self.stage_blob(path, side.stage())
            .map(|blob| String::from_utf8_lossy(&blob).into_owned())
    }

    // The content of a conflicted file in an index stage: 1 the base, 2 ours, 3 theirs.
    fn stage_blob(&self, path: &str, stage: u8) -> Option<Vec<u8>> {
        let object = format!(":{stage}:{path}");
        git_output_bytes(&self.path, &["cat-file", "blob", &object]).ok()
    }

    // Resolves a conflicted file by taking one side where the two conflict, keeping the changes
    // of the other side that merged cleanly, and removing the file when that side deleted it.
    pub fn resolve_conflict(
        &self,
        path: &str,
        side: ConflictSide,
    ) -> std::result::Result<(), String> {
        // before the file is written, which `git add` would then be refused
        self.check_idle()?;
        let ours = self.stage_blob(path, ConflictSide::Ours.stage());
        let theirs = self.stage_blob(path, ConflictSide::Theirs.stage());
        // literal, so resolving `x*.txt` leaves `xa.txt` alone
        let spec = pathspecs(&[path]);
        let spec = spec[0].as_str();
        let (Some(ours), Some(theirs)) = (ours, theirs) else {
            // one side deleted the file: it is kept as it is on the side taken
            let kept = self.stage_blob(path, side.stage()).is_some();
            return if kept {
                self.run(&["checkout", side.option(), "--", spec], None)
                    .and_then(|_| self.run(&["add", "--", spec], None))
            } else {
                self.run(&["rm", "--quiet", "--", spec], None)
            };
        };
        match self.merge_file_favoring(path, side, ours, theirs) {
            Some(merged) => std::fs::write(self.path.join(path), merged)
                .map_err(|e| format!("failed to write {path}: {e}"))?,
            // a binary file can't be merged by lines, so it is taken whole
            None => self.run(&["checkout", side.option(), "--", spec], None)?,
        }
        self.run(&["add", "--", spec], None)
    }

    // Merges the two sides of a conflicted file again, resolving the conflicts in favor of `side`.
    fn merge_file_favoring(
        &self,
        path: &str,
        side: ConflictSide,
        ours: Vec<u8>,
        theirs: Vec<u8>,
    ) -> Option<Vec<u8>> {
        // both sides added the file when there is no base
        let base = self.stage_blob(path, 1).unwrap_or_default();
        let files = [("ours", ours), ("base", base), ("theirs", theirs)]
            .map(|(name, blob)| (self.git_path(&format!("spoon-merge-{name}")), blob));
        let mut paths = vec![];
        for (file, blob) in &files {
            let file = file.as_ref()?;
            std::fs::write(file, blob).ok()?;
            paths.push(file.to_string_lossy().into_owned());
        }
        let args = [
            "merge-file",
            "-p",
            side.option(),
            &paths[0],
            &paths[1],
            &paths[2],
        ];
        // fails (with a negative code) on binary files
        let merged = git_output_bytes(&self.path, &args).ok();
        for file in files.iter().filter_map(|(file, _)| file.as_ref()) {
            let _ = std::fs::remove_file(file);
        }
        merged
    }

    // Whether a rebase stopped to edit a commit (rather than on conflicts), which is amended then.
    pub fn rebase_stopped_to_edit(&self) -> bool {
        self.git_path("rebase-merge/amend")
            .is_some_and(|p| p.exists())
    }

    // The commit message git prepared for the operation in progress, without its comment lines.
    // With the files git lists as conflicted, which it comments out, under "Conflicts:".
    pub fn prepared_message(&self) -> Option<String> {
        let content = self.git_file("MERGE_MSG")?;
        let message = without_comments(&content)?;
        let conflicts: Vec<String> = content
            .lines()
            .skip_while(|l| l.trim() != "# Conflicts:")
            .skip(1)
            .map_while(|l| l.strip_prefix('#').map(str::trim))
            .filter(|path| !path.is_empty())
            .map(|path| format!("  {path}"))
            .collect();
        if conflicts.is_empty() {
            return Some(message);
        }
        Some(format!("{message}\n\nConflicts:\n{}", conflicts.join("\n")))
    }

    // The message `git merge --squash` prepared, until the squash is committed.
    pub fn squash_message(&self) -> Option<String> {
        without_comments(&self.git_file("SQUASH_MSG")?)
    }

    // A file of the repository's git directory, like MERGE_MSG.
    fn git_file(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.git_path(name)?).ok()
    }

    // The command continuing a rebase, or the remaining commits of a cherry-pick or revert of
    // several commits, after the commit concluding the stop; None when that commit concludes it.
    pub fn continue_args(&self, operation: Operation) -> Option<GitArgs> {
        // a merge, or a single cherry-pick or revert, is done once committed
        let goes_on = operation == Operation::Rebase
            || (operation != Operation::Merge
                && self.git_path("sequencer").is_some_and(|p| p.exists()));
        goes_on.then(|| GitCommand::Resume(operation).args(["--continue"]))
    }

    fn git_path(&self, name: &str) -> Option<PathBuf> {
        if let Some(git_dir) = &self.git_dir {
            return Some(git_dir.join(name));
        }
        git_output(&self.path, &["rev-parse", "--git-path", name])
            .map(|p| self.path.join(p.trim()))
            .ok()
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

// A message git prepared, without its comment lines; None when nothing is left.
fn without_comments(content: &str) -> Option<String> {
    let message: Vec<&str> = content.lines().filter(|l| !l.starts_with('#')).collect();
    Some(message.join("\n").trim().to_string()).filter(|m| !m.is_empty())
}

// The .gitignore line matching only `path` from the top: the characters git reads as a pattern
// escaped, and a directory ending with "/".
fn gitignore_pattern(entry: Entry) -> String {
    let mut pattern = String::from("/");
    for c in entry.path().chars() {
        if matches!(c, '*' | '?' | '[' | '\\' | '!' | '#') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    // trailing spaces are dropped unless escaped
    let spaces = pattern.len() - pattern.trim_end_matches(' ').len();
    pattern.truncate(pattern.len() - spaces);
    pattern.push_str(&"\\ ".repeat(spaces));
    if let Entry::Dir(_) = entry {
        pattern.push('/');
    }
    pattern
}

// A boolean as git reads it in its config, None when it isn't one.
pub fn parse_git_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" | "" => Some(false),
        _ => None,
    }
}

// The top directory of the working tree (None when bare) and the git directory of the repository
// at `path`, which must be in a working tree or a bare repository.
fn locate_repository(path: &Path) -> Result<(Option<PathBuf>, Option<PathBuf>)> {
    let not_a_repository = || "not a git repository (or any of the parent directories)".into();
    let args = [
        "rev-parse",
        "--is-inside-work-tree",
        "--is-bare-repository",
        "--absolute-git-dir",
        "--show-toplevel",
    ];
    // out of a working tree, git fails on the last one but still answers the others
    let output = git_command()
        .args(args)
        .current_dir(path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| not_a_repository())?;
    let output = String::from_utf8_lossy(&output.stdout);
    let mut lines = output.lines();
    let (inside_work_tree, bare) = (lines.next() == Some("true"), lines.next() == Some("true"));
    let git_dir = lines.next().map(PathBuf::from);
    if !inside_work_tree && !bare {
        return Err(not_a_repository());
    }
    let toplevel = lines.next().filter(|_| inside_work_tree).map(PathBuf::from);
    Ok((toplevel, git_dir))
}

fn load_all_commits(
    path: &Path,
    sort: SortCommit,
    head: &Head,
    stashes: &[Commit],
    max_count: Option<usize>,
    mailmap: bool,
    revisions: Option<&[String]>,
) -> std::result::Result<Vec<Commit>, String> {
    let mut cmd = git_command();
    cmd.arg("log");

    cmd.arg(match sort {
        SortCommit::Chronological => "--date-order",
        SortCommit::Topological => "--topo-order",
    })
    .arg(format!("--pretty={}", load_commits_format(mailmap)))
    .arg("-z"); // use NUL as a delimiter

    if let Some(n) = max_count {
        cmd.arg("--max-count").arg(n.to_string());
    }

    match revisions {
        Some(revisions) => {
            cmd.args(revisions);
        }
        None => {
            // exclude stashes and other refs
            cmd.arg("--branches").arg("--remotes").arg("--tags");

            // commits that are reachable from the stashes
            stashes.iter().for_each(|stash| {
                cmd.arg(stash.parent_commit_hashes[0].as_str());
            });

            if !matches!(head, Head::None) {
                cmd.arg("HEAD");
            }
        }
    }
    // the revisions are not paths
    cmd.arg("--");

    // a broken ref or a missing object fails it, with git's error
    cmd.current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut process = cmd
        .spawn()
        .map_err(|e| format!("failed to run git log: {e}"))?;

    let stdout = process.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let mut commits = Vec::new();

    for bytes in reader.split(b'\0') {
        let bytes = bytes.map_err(|e| format!("failed to read git log: {e}"))?;
        let record = String::from_utf8_lossy(&bytes);
        let fields: Vec<&str> = record.split('\x1f').collect();
        commits.push(parse_commit(&fields));
    }

    // the errors are read once the commits are, they are only a few lines
    let output = process
        .wait_with_output()
        .map_err(|e| format!("failed to run git log: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let error = stderr.lines().rfind(|l| !l.is_empty()).unwrap_or_default();
        return Err(format!("git log failed: {error}"));
    }
    Ok(commits)
}

// The stashes, as commits for the graph and as refs, from one `git stash list`.
fn load_stashes(path: &Path, mailmap: bool) -> (Vec<Commit>, RefMap) {
    // the fields of a commit, then the stash's name like "stash@{0}"
    let format = format!("{}%x1f%gd", load_commits_format(mailmap));
    // the dates are strict ISO in the format: a --date option would name the stashes by date
    let args = ["stash", "list", &format!("--pretty={format}"), "-z"];
    let out = git_output(path, &args).unwrap_or_default();
    let mut commits = Vec::new();
    let mut ref_map = RefMap::default();
    for record in out.split('\0').filter(|r| !r.is_empty()) {
        let fields: Vec<&str> = record.split('\x1f').collect();
        let commit = parse_commit(&fields[..COMMIT_FIELDS]);
        let r = Ref::Stash {
            name: fields[COMMIT_FIELDS].into(),
            message: commit.subject.clone(),
            target: commit.commit_hash.clone(),
        };
        ref_map
            .entry(commit.commit_hash.clone())
            .or_default()
            .push(r);
        commits.push(commit);
    }
    (commits, ref_map)
}

// the fields of `load_commits_format`
const COMMIT_FIELDS: usize = 10;

// A commit from the fields of `load_commits_format`.
fn parse_commit(fields: &[&str]) -> Commit {
    assert_eq!(fields.len(), COMMIT_FIELDS, "unexpected fields: {fields:?}");
    Commit {
        commit_hash: fields[0].into(),
        author_name: fields[1].into(),
        author_email: fields[2].into(),
        author_date: parse_iso_date(fields[3]),
        committer_name: fields[4].into(),
        committer_email: fields[5].into(),
        committer_date: parse_iso_date(fields[6]),
        subject: fields[7].into(),
        body: fields[8].into(),
        parent_commit_hashes: parse_parent_commit_hashes(fields[9]),
    }
}

fn load_commits_format(mailmap: bool) -> String {
    // The uppercase name/email placeholders (`%aN`, `%aE`, `%cN`, `%cE`) resolve
    // identities through the repository's .mailmap, while the lowercase variants
    // use the raw values recorded in each commit.
    let format = if mailmap {
        [
            "%H", "%aN", "%aE", "%aI", "%cN", "%cE", "%cI", "%s", "%b", "%P",
        ]
    } else {
        [
            "%H", "%an", "%ae", "%aI", "%cn", "%ce", "%cI", "%s", "%b", "%P",
        ]
    };
    format.join("%x1f") // use Unit Separator as a delimiter
}

fn parse_iso_date(s: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(s).unwrap()
}

// The lines of a file at a commit, each with the commit that last changed it.
#[derive(Debug, Default, Clone)]
pub struct Blame {
    pub lines: Vec<(CommitHash, String)>,
    pub commits: FxHashMap<CommitHash, BlameCommit>,
}

#[derive(Debug, Clone)]
pub struct BlameCommit {
    pub author: String,
    pub date: DateTime<FixedOffset>,
}

// A commit changing a file, and the file's path in it.
#[derive(Debug, Clone)]
pub struct FileRevision {
    pub commit: CommitHash,
    pub date: DateTime<FixedOffset>,
    pub subject: String,
    pub path: String,
}

// `git blame --porcelain`: each line is a header "<hash> <line> <line> [<count>]", the details of
// its commit the first time it comes ("author A", "author-time 1700000000", "author-tz +0100"),
// then the text after a tab.
fn parse_blame(out: &str) -> Blame {
    let mut blame = Blame::default();
    let mut commit = CommitHash::default();
    let (mut author, mut time, mut tz) = (String::new(), 0, 0);
    for line in out.lines() {
        if let Some(text) = line.strip_prefix('\t') {
            blame.lines.push((commit.clone(), text.into()));
            continue;
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "author" => author = value.into(),
            "author-time" => time = value.parse().unwrap_or(0),
            // +hhmm
            "author-tz" => {
                let n: i32 = value.parse().unwrap_or(0);
                tz = n / 100 * 3600 + n % 100 * 60;
            }
            // the details end with the file's name
            "filename" => {
                let offset = FixedOffset::east_opt(tz).unwrap_or(FixedOffset::east_opt(0).unwrap());
                let date = DateTime::from_timestamp(time, 0).unwrap_or_default();
                blame.commits.insert(
                    commit.clone(),
                    BlameCommit {
                        author: std::mem::take(&mut author),
                        date: date.with_timezone(&offset),
                    },
                );
            }
            _ if key.len() >= 40 && key.bytes().all(|b| b.is_ascii_hexdigit()) => {
                commit = CommitHash::from(key);
            }
            _ => {}
        }
    }
    blame
}

fn parse_parent_commit_hashes(s: &str) -> Vec<CommitHash> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split(' ').map(|s| s.into()).collect()
}

fn build_commits_maps(commits: &Vec<Commit>) -> (CommitsMap, CommitsMap) {
    let mut parents_map: CommitsMap = FxHashMap::default();
    let mut children_map: CommitsMap = FxHashMap::default();
    for commit in commits {
        let hash = &commit.commit_hash;
        for parent_hash in &commit.parent_commit_hashes {
            parents_map
                .entry(hash.clone())
                .or_default()
                .push(parent_hash.clone());
            children_map
                .entry(parent_hash.clone())
                .or_default()
                .push(hash.clone());
        }
    }

    (parents_map, children_map)
}

fn to_commit_map(commits: Vec<Commit>) -> CommitMap {
    commits
        .into_iter()
        .map(|commit| (commit.commit_hash.clone(), commit))
        .collect()
}

fn merge_stashes_to_commits(commits: Vec<Commit>, stashes: Vec<Commit>) -> Vec<Commit> {
    // Stash commit has multiple parent commits, but the first parent commit is the commit that the stash was created from.
    // If the first parent commit is not found, the stash commit is ignored.
    let mut ret = Vec::new();
    let mut statsh_map: FxHashMap<CommitHash, Vec<Commit>> =
        stashes
            .into_iter()
            .fold(FxHashMap::default(), |mut acc, commit| {
                let parent = commit.parent_commit_hashes[0].clone();
                acc.entry(parent).or_default().push(commit);
                acc
            });
    for commit in commits {
        if let Some(stashes) = statsh_map.remove(&commit.commit_hash) {
            for stash in stashes {
                ret.push(stash);
            }
        }
        ret.push(commit);
    }
    ret
}

fn load_refs(path: &Path) -> (RefMap, Head) {
    // none in a repository without commits
    let out = git_output(path, &["show-ref", "--head", "--dereference"]).unwrap_or_default();

    let mut ref_map = RefMap::default();
    let mut tag_map: FxHashMap<String, Ref> = FxHashMap::default();
    let mut head: Head = Head::None;

    for line in out.lines() {
        let parts: Vec<&str> = line.split(' ').collect();
        if parts.len() != 2 {
            panic!("unexpected number of parts: {} [{}]", parts.len(), line);
        }

        let hash = parts[0];
        let refs = parts[1];

        if refs == "HEAD" {
            head = if let Some(branch) = get_current_branch(path) {
                Head::Branch { name: branch }
            } else {
                Head::Detached {
                    target: hash.into(),
                }
            };
        } else if let Some(r) = parse_branch_refs(hash, refs) {
            ref_map.entry(hash.into()).or_default().push(r);
        } else if let Some(r) = parse_tag_refs(hash, refs) {
            // if annotated tag exists, it will be overwritten by the following line of the same tag
            // this will make the tag point to the commit that the annotated tag points to
            tag_map.insert(r.name().into(), r);
        }
    }

    for tag in tag_map.into_values() {
        ref_map.entry(tag.target().clone()).or_default().push(tag);
    }

    ref_map.values_mut().for_each(|refs| refs.sort());

    (ref_map, head)
}

fn merge_ref_maps(m1: &mut RefMap, m2: RefMap) {
    for (k, v) in m2 {
        m1.entry(k).or_default().extend(v);
    }
}

fn parse_branch_refs(hash: &str, refs: &str) -> Option<Ref> {
    if refs.starts_with("refs/heads/") {
        let name = refs.trim_start_matches("refs/heads/");
        Some(Ref::Branch {
            name: name.into(),
            target: hash.into(),
        })
    } else if refs.starts_with("refs/remotes/") {
        let name = refs.trim_start_matches("refs/remotes/");
        // "origin/HEAD" only points at the default branch of the remote, which is shown already
        if name.ends_with("/HEAD") {
            return None;
        }
        Some(Ref::RemoteBranch {
            name: name.into(),
            target: hash.into(),
        })
    } else {
        None
    }
}

fn parse_tag_refs(hash: &str, refs: &str) -> Option<Ref> {
    if refs.starts_with("refs/tags/") {
        let name = refs.trim_start_matches("refs/tags/");
        let name = name.trim_end_matches("^{}");
        Some(Ref::Tag {
            name: name.into(),
            target: hash.into(),
        })
    } else {
        None
    }
}

// The diff drivers with a `textconv`, like "docx" for `diff.docx.textconv`.
fn load_textconv_drivers(path: &Path) -> Vec<String> {
    let pattern = r"^diff\..*\.textconv$";
    git_output(path, &["config", "--name-only", "--get-regexp", pattern])
        .unwrap_or_default()
        .lines()
        .filter_map(|key| key.strip_prefix("diff.")?.strip_suffix(".textconv"))
        .map(String::from)
        .collect()
}

// The names of the remotes, sorted.
fn load_remotes(path: &Path) -> Vec<String> {
    let mut remotes: Vec<String> = git_output(path, &["remote"])
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect();
    remotes.sort();
    remotes
}

// None on a detached HEAD.
fn get_current_branch(path: &Path) -> Option<String> {
    let out = git_output(path, &["branch", "--show-current"]).ok()?;
    Some(out.trim().to_string()).filter(|branch| !branch.is_empty())
}

#[derive(Debug, Clone)]
pub enum FileChange {
    Add { path: String },
    Modify { path: String },
    Delete { path: String },
    Move { from: String, to: String },
}

impl FileChange {
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

// The remote and the branch on it of a remote branch like "origin/main"; remote names may contain
// slashes, the longest matching one owns the branch.
pub fn split_remote_branch<'a>(remotes: &'a [String], name: &'a str) -> Option<(&'a str, &'a str)> {
    remotes
        .iter()
        .filter(|remote| name.starts_with(&format!("{remote}/")))
        .max_by_key(|remote| remote.len())
        .map(|remote| (remote.as_str(), &name[remote.len() + 1..]))
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

// The two versions of a conflicted file: what was there (ours) and what is coming in (theirs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictSide {
    Ours,
    Theirs,
}

impl ConflictSide {
    // The index stage of the side in a conflict.
    fn stage(self) -> u8 {
        match self {
            ConflictSide::Ours => 2,
            ConflictSide::Theirs => 3,
        }
    }

    // The option of git checkout and git merge-file that takes the side.
    fn option(self) -> &'static str {
        match self {
            ConflictSide::Ours => "--ours",
            ConflictSide::Theirs => "--theirs",
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LocalChangeStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
    Conflicted,
}

impl LocalChangeStatus {
    // Its letter, like `git status --short`.
    pub fn symbol(self) -> &'static str {
        match self {
            LocalChangeStatus::Added => "A",
            LocalChangeStatus::Modified => "M",
            LocalChangeStatus::Deleted => "D",
            LocalChangeStatus::Renamed => "R",
            LocalChangeStatus::Copied => "C",
            LocalChangeStatus::TypeChanged => "T",
            LocalChangeStatus::Untracked => "?",
            LocalChangeStatus::Conflicted => "U",
        }
    }

    fn from_status_char(c: u8) -> Option<Self> {
        match c {
            b'A' => Some(LocalChangeStatus::Added),
            b'M' => Some(LocalChangeStatus::Modified),
            b'D' => Some(LocalChangeStatus::Deleted),
            b'R' => Some(LocalChangeStatus::Renamed),
            b'C' => Some(LocalChangeStatus::Copied),
            b'T' => Some(LocalChangeStatus::TypeChanged),
            b'?' => Some(LocalChangeStatus::Untracked),
            b'U' => Some(LocalChangeStatus::Conflicted),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LocalChangeFile {
    pub path: String,
    // the source path of a rename or copy
    pub orig_path: Option<String>,
    pub status: LocalChangeStatus,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LocalChanges {
    pub unstaged: Vec<LocalChangeFile>,
    pub staged: Vec<LocalChangeFile>,
}

impl LocalChanges {
    // Number of distinct paths with uncommitted changes, counting a path that is both staged and unstaged once.
    pub fn count(&self) -> usize {
        let mut paths: Vec<&str> = self
            .unstaged
            .iter()
            .chain(self.staged.iter())
            .map(|f| f.path.as_str())
            .collect();
        paths.sort_unstable();
        paths.dedup();
        paths.len()
    }
}

fn load_local_changes(path: &Path) -> LocalChanges {
    // without refreshing the index, whose lock would make a git command running meanwhile fail
    let args = [
        "--no-optional-locks",
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
        "-z",
    ];
    // fails in bare repositories, which have no working tree
    git_output_bytes(path, &args)
        .map(|out| parse_status_porcelain(&out))
        .unwrap_or_default()
}

fn parse_status_porcelain(bytes: &[u8]) -> LocalChanges {
    let mut changes = LocalChanges::default();
    let mut entries = bytes.split(|b| *b == b'\0').filter(|e| !e.is_empty());

    while let Some(entry) = entries.next() {
        if entry.len() < 4 {
            continue;
        }
        let (x, y) = (entry[0], entry[1]);
        let path = String::from_utf8_lossy(&entry[3..]).into_owned();
        // renames and copies, staged or in the working tree (of files added with
        // `git add --intent-to-add`), are followed by the original path
        let is_rename = |c: u8| c == b'R' || c == b'C';
        let orig_path = if is_rename(x) || is_rename(y) {
            entries
                .next()
                .map(|p| String::from_utf8_lossy(p).into_owned())
        } else {
            None
        };

        if x == b'?' {
            changes.unstaged.push(LocalChangeFile {
                path,
                orig_path: None,
                status: LocalChangeStatus::Untracked,
            });
            continue;
        }
        if x == b'U' || y == b'U' || (x == b'A' && y == b'A') || (x == b'D' && y == b'D') {
            changes.unstaged.push(LocalChangeFile {
                path,
                orig_path: None,
                status: LocalChangeStatus::Conflicted,
            });
            continue;
        }
        if let Some(status) = LocalChangeStatus::from_status_char(x) {
            changes.staged.push(LocalChangeFile {
                path: path.clone(),
                orig_path: orig_path.clone().filter(|_| is_rename(x)),
                status,
            });
        }
        if let Some(status) = LocalChangeStatus::from_status_char(y) {
            changes.unstaged.push(LocalChangeFile {
                path,
                orig_path: orig_path.filter(|_| !is_rename(x)),
                status,
            });
        }
    }

    changes
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

// Which changes of the working tree a diff shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffOf<'a> {
    Unstaged,
    // a file git doesn't track, diffed against nothing, one path at a time
    Untracked,
    // compared to `base`, HEAD when None, see `amend_base`
    Staged { base: Option<&'a str> },
}

fn load_local_change_diff(path: &Path, targets: &[&str], of: DiffOf) -> String {
    let mut args: Vec<String> = PATCH_DIFF_ARGS.iter().map(|a| a.to_string()).collect();
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
    // `--no-index` exits with 1 when the files differ
    git_output_with_status(path, &args).map_or_else(String::new, |(_, out)| out)
}

// `git diff --name-status -z` output: a status, then one path or two for renames and copies.
fn parse_name_status(bytes: &[u8]) -> Vec<LocalChangeFile> {
    let mut files = Vec::new();
    let mut tokens = bytes
        .split(|b| *b == b'\0')
        .filter(|t| !t.is_empty())
        .map(|t| String::from_utf8_lossy(t).into_owned());

    while let Some(status) = tokens.next() {
        let Some(status) = LocalChangeStatus::from_status_char(status.as_bytes()[0]) else {
            tokens.next();
            continue;
        };
        let first = tokens.next().unwrap_or_default();
        let (path, orig_path) = match status {
            LocalChangeStatus::Renamed | LocalChangeStatus::Copied => {
                (tokens.next().unwrap_or_default(), Some(first))
            }
            _ => (first, None),
        };
        files.push(LocalChangeFile {
            path,
            orig_path,
            status,
        });
    }
    files
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

// The local changes and a fingerprint of their files, loaded in the background every few seconds
// for the local changes page to reload when something changed, see `files_stamp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesSnapshot {
    pub changes: LocalChanges,
    pub stamp: u64,
}

impl FilesSnapshot {
    pub fn load(work_tree: &Path, git_dir: Option<&Path>) -> Self {
        let changes = load_local_changes(work_tree);
        let stamp = files_stamp(work_tree, git_dir, &changes);
        FilesSnapshot { changes, stamp }
    }
}

// A fingerprint of the local changes and what they are made of: the index, and each changed file's
// size and time of change, which an edit to an already changed file changes without changing its
// status.
pub fn files_stamp(work_tree: &Path, git_dir: Option<&Path>, changes: &LocalChanges) -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = rustc_hash::FxBuildHasher.build_hasher();
    let mut stat = |path: &Path| match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            meta.len().hash(&mut hasher);
            meta.modified().ok().hash(&mut hasher);
        }
        Err(_) => hasher.write_u8(0),
    };
    if let Some(git_dir) = git_dir {
        stat(&git_dir.join("index"));
    }
    for file in changes.unstaged.iter().chain(&changes.staged) {
        stat(&work_tree.join(&file.path));
    }
    changes.unstaged.hash(&mut hasher);
    changes.staged.hash(&mut hasher);
    hasher.finish()
}

// How far each local branch is ahead and behind its upstream, those in sync left out.
fn load_branch_tracking(path: &Path) -> FxHashMap<String, Tracking> {
    // the full name: the short one is "heads/v2" when a tag is named v2 too
    let format = "--format=%(refname)%00%(upstream:short)%00%(upstream:track,nobracket)";
    let refs = git_output(path, &["for-each-ref", format, "refs/heads"]).unwrap_or_default();
    refs.lines()
        .filter_map(|line| {
            let mut parts = line.split('\0');
            let name = parts.next()?.strip_prefix("refs/heads/")?;
            let upstream = parts.next().filter(|u| !u.is_empty())?;
            let track = parts.next().unwrap_or_default();
            // like "ahead 2, behind 1", empty when in sync or without an upstream
            let count = |what: &str| {
                track
                    .split(", ")
                    .find_map(|part| part.strip_prefix(what)?.trim().parse().ok())
                    .unwrap_or(0)
            };
            let tracking = Tracking {
                upstream: upstream.to_string(),
                ahead: count("ahead "),
                behind: count("behind "),
            };
            Some((name.to_string(), tracking))
        })
        .collect()
}

// A local branch's upstream, like "origin/main", and how many commits the branch is ahead of it
// and behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tracking {
    pub upstream: String,
    pub ahead: usize,
    pub behind: usize,
}

// A fingerprint of the branches, remote branches, tags, HEAD and the stashes, which changes when
// they are changed from outside the app, like lazygit's `RefsSnapshot`: where each ref points
// (a fetch in the background moves the remote ones), the branch checked out or the commit when
// detached, and every stash, as the sidebar drops them by position.
pub fn refs_snapshot(path: &Path) -> std::result::Result<String, String> {
    let refs = git_output(
        path,
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ],
    )?;
    let head = git_output(path, &["symbolic-ref", "-q", "HEAD"])
        .or_else(|_| git_output(path, &["rev-parse", "-q", "--verify", "HEAD"]))
        .unwrap_or_default();
    // older stashes are only in the reflog of refs/stash, which for-each-ref doesn't see
    let stashes = git_output(path, &["stash", "list", "--format=%H"]).unwrap_or_default();
    Ok(refs + &head + &stashes)
}

// The git commands the app runs in the background. What each does to HEAD, to signing and to the
// remotes is told by its variant, so a new command can't be left out of those checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitCommand {
    Apply,
    Branch,
    Checkout,
    CherryPick,
    Clean,
    Commit,
    Fetch,
    Merge,
    Pull,
    Push,
    Rebase,
    Remote,
    Reset,
    Restore,
    Revert,
    Rm,
    Stash,
    Tag,
    // going on with the operation stopped, or aborting it: `rebase --continue`, `merge --abort`
    Resume(Operation),
}

impl GitCommand {
    pub fn name(self) -> &'static str {
        match self {
            GitCommand::Apply => "apply",
            GitCommand::Branch => "branch",
            GitCommand::Checkout => "checkout",
            GitCommand::CherryPick => "cherry-pick",
            GitCommand::Clean => "clean",
            GitCommand::Commit => "commit",
            GitCommand::Fetch => "fetch",
            GitCommand::Merge => "merge",
            GitCommand::Pull => "pull",
            GitCommand::Push => "push",
            GitCommand::Rebase => "rebase",
            GitCommand::Remote => "remote",
            GitCommand::Reset => "reset",
            GitCommand::Restore => "restore",
            GitCommand::Revert => "revert",
            GitCommand::Rm => "rm",
            GitCommand::Stash => "stash",
            GitCommand::Tag => "tag",
            GitCommand::Resume(operation) => operation.name(),
        }
    }

    // Whether it moves or rewrites HEAD, which git refuses or tangles up while a rebase, merge,
    // cherry-pick or revert waits to be continued or aborted; going on with it or aborting it doesn't.
    pub fn moves_head(self) -> bool {
        use GitCommand::*;
        match self {
            Checkout | Rebase | Merge | CherryPick | Revert | Reset | Pull => true,
            Apply | Branch | Clean | Commit | Fetch | Push | Remote | Restore | Rm | Stash
            | Tag | Resume(_) => false,
        }
    }

    // Whether it may make a commit or a tag, which git signs when configured to.
    pub fn may_sign(self) -> bool {
        use GitCommand::*;
        match self {
            Commit | Rebase | CherryPick | Merge | Revert | Tag | Pull | Resume(_) => true,
            Apply | Branch | Checkout | Clean | Fetch | Push | Remote | Reset | Restore | Rm
            | Stash => false,
        }
    }

    // Whether it talks to a remote, one at a time, see `run_task`.
    pub fn is_remote(self) -> bool {
        use GitCommand::*;
        match self {
            Fetch | Pull | Push => true,
            Apply | Branch | Checkout | CherryPick | Clean | Commit | Merge | Rebase | Remote
            | Reset | Restore | Revert | Rm | Stash | Tag | Resume(_) => false,
        }
    }

    // The command with its arguments.
    pub fn args<S: Into<String>>(self, args: impl IntoIterator<Item = S>) -> GitArgs {
        GitArgs {
            command: self,
            args: args.into_iter().map(Into::into).collect(),
        }
    }
}

// A git command and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitArgs {
    pub command: GitCommand,
    pub args: Vec<String>,
}

impl GitArgs {
    // What git is run with, the command first.
    pub fn argv(&self) -> Vec<String> {
        std::iter::once(self.command.name().to_string())
            .chain(self.args.iter().cloned())
            .collect()
    }
}

// As in the popups and the command log: "commit --quiet --file=-".
impl std::fmt::Display for GitArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.argv().join(" "))
    }
}

// Git commands run in the background, like fetch, merge or checkout, and what to say about them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GitTask {
    // the git commands, each run when the one before succeeded
    pub commands: Vec<GitArgs>,
    // stash the local changes before the command, and reapply them after
    pub stash: bool,
    pub env: Vec<(String, String)>,
    // "Pushing main to origin/main"
    pub running: String,
    // "Pushed main to origin/main"
    pub done: String,
    // made of the picked commits, which are cleared once it is done
    pub uses_picks: bool,
    pub after: AfterTask,
    // the local branch it pushes or pulls, shown beside it in the sidebar while it runs, like lazygit
    pub branch_status: Option<BranchStatus>,
    // what the first command reads, like the message of a commit
    pub input: Option<String>,
    // said when it fails instead of git's error, which may tell little
    pub failed: Option<String>,
    // stopped once it runs longer, like a fetch from a remote that never answers
    pub timeout: Option<Duration>,
}

// What the app shows once a git command is done.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum AfterTask {
    // the repository reloaded, the page as it was
    #[default]
    Reload,
    // and this commit selected, like "HEAD~2", as rewriting the history changes its hash
    Select(String),
    // the local changes page, it left changes to commit
    LocalChanges,
    // only the working tree changed, the page stays as it is instead of being reloaded
    KeepView,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchStatus {
    pub branch: String,
    // "Pushing", and "↑" where that doesn't fit
    pub label: String,
    pub short: &'static str,
}

impl GitTask {
    // Whether it may make a commit or a tag, see `GitCommand::may_sign`.
    pub fn may_sign(&self) -> bool {
        self.commands.iter().any(|c| c.command.may_sign())
    }

    // Whether it moves or rewrites HEAD, see `GitCommand::moves_head`.
    pub fn moves_head(&self) -> bool {
        self.commands.iter().any(|c| c.command.moves_head())
    }

    // Whether it talks to a remote, see `GitCommand::is_remote`.
    pub fn is_remote(&self) -> bool {
        self.commands.iter().any(|c| c.command.is_remote())
    }

    // Whether it makes a commit with a typed message, which the message box is then cleared of.
    pub fn commits(&self) -> bool {
        self.commands
            .first()
            .is_some_and(|c| c.command == GitCommand::Commit)
            && self.input.is_some()
    }

    pub fn new(command: GitCommand, args: &[&str], running: String, done: String) -> Self {
        GitTask {
            commands: vec![command.args(args.iter().copied())],
            running,
            done,
            ..Default::default()
        }
    }

    // An interactive rebase of the commits after `base` (all of them when None), its todo list
    // changed by the sed `expressions`, see `todo_action`.
    pub fn rebase_todo(base: Option<&str>, auto_stash: bool, expressions: &[String]) -> Self {
        let stash = if auto_stash {
            "--autostash"
        } else {
            "--no-autostash"
        };
        // each expression changes its line, else the rebase stops before starting, as git's
        // config could have changed the todo list
        let sed = std::iter::once("sed -i.bak -E".to_string())
            .chain(expressions.iter().map(|e| format!("-e '{e}'")))
            .collect::<Vec<_>>()
            .join(" ");
        let editor = format!(
            "todo() {{ {sed} \"$1\" && test \"$(diff \"$1.bak\" \"$1\" | grep -c '^>')\" = {} \
             || {{ echo 'spoon: a commit is missing from the todo list of the rebase' >&2; exit 1; }}; }}; todo",
            expressions.len()
        );
        let env = [
            ("GIT_SEQUENCE_EDITOR", editor.as_str()),
            // the todo list names the commits with full hashes, of SHA-1 and SHA-256 alike
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_CONFIG_KEY_0", "core.abbrev"),
            ("GIT_CONFIG_VALUE_0", "no"),
        ];
        GitTask {
            // autosquash would move and squash `fixup!` commits nobody picked
            commands: vec![GitCommand::Rebase.args([
                "-i",
                "--no-autosquash",
                stash,
                base.unwrap_or("--root"),
            ])],
            env: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
        }
    }
}

// A sed expression turning the pick of the commit (a full hash) in a rebase todo list into
// `action`, like "squash" or "drop".
pub fn todo_action(hash: &str, action: &str) -> String {
    format!("s/^(pick|p) {hash} /{action} {hash} /")
}

// Runs the command of a task, between stashing and reapplying the local changes if it says so.
pub fn run_task(path: &Path, task: &GitTask) -> std::result::Result<(), String> {
    // one remote command at a time, like lazygit's sync mutex: a pull waits for the fetch on
    // launch instead of both locking the same remote refs
    static REMOTE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _remote = task.is_remote().then(|| {
        REMOTE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    });
    // stashed when the stash changed: git stashes nothing for some changes it lists, like those
    // in a submodule, and popping then would take an older stash
    let stash_top = || git_output(path, &["rev-parse", "--verify", "--quiet", "refs/stash"]).ok();
    let stashed = task.stash && {
        let before = stash_top();
        let message = format!("spoon: {}", task.running);
        let args = GitCommand::Stash.args(["push", "--include-untracked", "-m", &message]);
        run_task_step(path, &args, None, task)?;
        stash_top() != before
    };
    let pop = || run_task_step(path, &GitCommand::Stash.args(["pop"]), None, task);

    let run = || {
        task.commands.iter().enumerate().try_for_each(|(i, args)| {
            let input = task.input.as_deref().filter(|_| i == 0);
            run_task_step(path, args, input, task)
        })
    };
    match run() {
        Ok(()) => {}
        Err(err) if stashed => {
            // nothing changed, the local changes go back
            return Err(match pop() {
                Ok(()) => err,
                Err(_) => format!("{err} (the local changes are kept in the stash)"),
            });
        }
        Err(err) => return Err(err),
    }
    if stashed {
        pop().map_err(|err| {
            format!(
                "{}, but reapplying the local changes failed (they are kept in the stash): {err}",
                task.done
            )
        })?;
    }
    Ok(())
}

// It runs while the app owns the terminal, so git must not prompt: no editor (the messages git
// prepares are used as they are), and credentials come from a helper or an agent.
fn run_task_step(
    path: &Path,
    args: &GitArgs,
    input: Option<&str>,
    task: &GitTask,
) -> std::result::Result<(), String> {
    let args = args.argv();
    let mut cmd = git_command();
    cmd.args(&args)
        .current_dir(path)
        .env("GIT_EDITOR", "true")
        .env("GIT_TERMINAL_PROMPT", "0")
        .envs(task.env.iter().map(|(k, v)| (k, v)));
    if task.is_remote() {
        let has_ssh_command = std::env::var_os("GIT_SSH_COMMAND").is_some()
            || std::env::var_os("GIT_SSH").is_some()
            || git_output(path, &["config", "--get", "core.sshCommand"]).is_ok();
        if !has_ssh_command {
            cmd.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
        }
    }
    run_logged(&mut cmd, &args, input, task.timeout)
}

// Runs `cmd`, git with `args`, writing `input` to it, and logs it in the command log; the error
// says what went wrong in git's words when it fails.
fn run_logged(
    cmd: &mut Command,
    args: &[impl AsRef<str>],
    input: Option<&str>,
    timeout: Option<Duration>,
) -> std::result::Result<(), String> {
    let name = args.first().map_or("", |a| a.as_ref());
    let command_error = |e: std::io::Error| {
        let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
        format!("failed to run git {}: {e}", args.join(" "))
    };
    let started = Instant::now();
    let mut child = cmd
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(command_error)?;
    if let Some(input) = input {
        use std::io::Write;
        // closed once written, so git can finish
        let mut stdin = child.stdin.take().expect("stdin is piped");
        match stdin.write_all(input.as_bytes()) {
            // git stopped before reading it, like on a failing hook: its output says why
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
            result => result.map_err(command_error)?,
        }
    }
    let output = match timeout {
        None => child.wait_with_output().map_err(command_error)?,
        Some(timeout) => match wait_with_timeout(child, timeout).map_err(command_error)? {
            Some(output) => output,
            None => {
                let error = format!("git {name} timed out after {}s", timeout.as_secs());
                log_command(args, false, &error, started);
                return Err(error);
            }
        },
    };
    // git reports progress and results of many commands on stderr
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    log_command(args, output.status.success(), &text, started);
    if output.status.success() {
        return Ok(());
    }
    // a rejected ref or a conflict says what went wrong best, then the error lines, and the
    // hints after them how to fix it
    let lines = || text.lines().map(str::trim);
    let error = lines()
        .find(|line| line.starts_with("! ") || line.starts_with("CONFLICT"))
        .or_else(|| lines().find(|line| line.starts_with("error:") || line.starts_with("fatal:")))
        .or_else(|| lines().rfind(|line| !line.is_empty()))
        .unwrap_or_default();
    Err(format!("git {name} failed: {error}"))
}

// Like `Child::wait_with_output`, but stops the command once `timeout` passed, None then.
fn wait_with_timeout(mut child: Child, timeout: Duration) -> std::io::Result<Option<Output>> {
    // read meanwhile, a command filling a pipe would wait for it to be read
    fn read(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut buf = vec![];
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            buf
        })
    }
    let stdout = read(child.stdout.take());
    let stderr = read(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            // SIGTERM, not `Child::kill`'s SIGKILL: git then removes its lock files and the pack it
            // was receiving, which would otherwise make the next fetch fail
            let _ = Command::new("kill").arg(child.id().to_string()).status();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    Ok(Some(Output {
        status,
        stdout,
        stderr,
    }))
}

// The command stashing the changes of `paths` (all changes when empty), including untracked files.
pub fn stash_args(paths: &[String], message: &str) -> GitArgs {
    let mut args: Vec<String> = ["push", "--include-untracked", "--quiet"]
        .map(String::from)
        .to_vec();
    if !message.is_empty() {
        args.extend(["--message".into(), message.into()]);
    }
    if !paths.is_empty() {
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        args.push("--".into());
        args.extend(pathspecs(&paths));
    }
    GitCommand::Stash.args(args)
}

pub const MAX_CONFLICT_STEPS: usize = 100;

// What `Repository::first_conflict` found.
#[derive(Debug, PartialEq, Eq)]
pub enum Conflict {
    // the commit stopped at, and the files conflicting
    At(String, Vec<String>),
    // none in the steps checked, with this many after them not checked
    Unchecked(usize),
}

// The empty tree, as the base of a root commit.
fn empty_tree(path: &Path) -> std::result::Result<String, String> {
    git_output(path, &["hash-object", "-t", "tree", "/dev/null"]).map(|s| s.trim().to_string())
}

// A git command of the app, taking no lock it can do without, like `git status` refreshing the
// index, which would make the user's own git commands fail meanwhile. In tests, the git config of
// the user's home and system is left out, so the tests behave alike everywhere.
pub fn git_command() -> Command {
    let mut cmd = Command::new("git");
    cmd.env("GIT_OPTIONAL_LOCKS", "0");
    #[cfg(test)]
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    cmd
}

// Paths of files or directories for git, taken as they are, not as patterns: a file named
// `*.log` is only that file. The root of the repository stays ":/".
pub fn pathspecs(paths: &[&str]) -> Vec<String> {
    paths
        .iter()
        .map(|path| match *path {
            ":/" => path.to_string(),
            path => format!(":(literal){path}"),
        })
        .collect()
}

// The output of git whatever its exit status, with the status: some commands tell with it, like
// `merge-tree` a conflict or `diff --no-index` files that differ. None when git didn't run.
fn git_output_with_status(
    path: &Path,
    args: &[impl AsRef<OsStr>],
) -> Option<(Option<i32>, String)> {
    let output = git_command()
        .args(args)
        .current_dir(path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let out = String::from_utf8_lossy(&output.stdout).into_owned();
    Some((output.status.code(), out))
}

fn git_output(path: &Path, args: &[&str]) -> std::result::Result<String, String> {
    git_output_bytes(path, args).map(|out| String::from_utf8_lossy(&out).into_owned())
}

fn git_output_bytes(path: &Path, args: &[&str]) -> std::result::Result<Vec<u8>, String> {
    let output = git_command()
        .args(args)
        .current_dir(path)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to run git {}: {e}", args.join(" ")))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("git {} failed: {}", args[0], stderr.trim()))
    }
}

// A git command the app ran that changes the repository, for the command log. Reads, like
// those of the refresh every few seconds, are left out.
#[derive(Debug, Clone)]
pub struct LoggedCommand {
    pub at: DateTime<Local>,
    // "git push origin main", with passwords in URLs hidden
    pub command: String,
    pub ok: bool,
    // stdout then stderr, its last lines
    pub output: String,
    pub duration: Duration,
}

// shortcut: kept in memory, the last ones only; written to a file if they're needed after quitting
const COMMAND_LOG_SIZE: usize = 500;
const COMMAND_LOG_OUTPUT_LINES: usize = 200;
static COMMAND_LOG: Mutex<VecDeque<LoggedCommand>> = Mutex::new(VecDeque::new());

fn log_command(args: &[impl AsRef<str>], ok: bool, output: &str, started: Instant) {
    let args: Vec<String> = args.iter().map(|a| hide_password(a.as_ref())).collect();
    let lines: Vec<&str> = output.lines().collect();
    let output = lines[lines.len().saturating_sub(COMMAND_LOG_OUTPUT_LINES)..].join("\n");
    let entry = LoggedCommand {
        at: Local::now(),
        command: format!("git {}", args.join(" ")),
        ok,
        output,
        duration: started.elapsed(),
    };
    // a command run while another thread panicked holding it still gets logged
    let mut log = COMMAND_LOG.lock().unwrap_or_else(|e| e.into_inner());
    log.push_front(entry);
    log.truncate(COMMAND_LOG_SIZE);
}

// The commands logged, the newest first.
pub fn command_log() -> Vec<LoggedCommand> {
    let log = COMMAND_LOG.lock().unwrap_or_else(|e| e.into_inner());
    log.iter().cloned().collect()
}

// "https://user:token@host/r.git" -> "https://***@host/r.git"
fn hide_password(arg: &str) -> String {
    let Some(scheme_end) = arg.find("://").map(|i| i + 3) else {
        return arg.to_string();
    };
    let host_end = arg[scheme_end..]
        .find('/')
        .map_or(arg.len(), |i| scheme_end + i);
    match arg[scheme_end..host_end].rfind('@') {
        Some(at) => format!("{}***{}", &arg[..scheme_end], &arg[scheme_end + at..]),
        None => arg.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_root_commit_and_stash() {
        use crate::test_support::git::GitRepository;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        std::fs::write(path.join("a.txt"), "a\n").unwrap();
        repo.add(&["a.txt"]);
        repo.commit("first", "2024-01-01");
        repo.stash("2024-01-02");
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();

        // the root commit's files, against the empty tree like its diff
        let root = repository.head_commit().unwrap();
        let (_, changes) = repository.commit_detail(&root);
        assert!(matches!(&changes[..], [FileChange::Add { path }] if path == "a.txt"));

        // the stash from one `git stash list`: a commit of the graph and a ref
        let stash = repository
            .all_refs()
            .into_iter()
            .find(|r| matches!(r, Ref::Stash { .. }))
            .expect("the stash ref")
            .clone();
        let Ref::Stash { name, target, .. } = &stash else {
            unreachable!()
        };
        assert_eq!(name, "stash@{0}");
        assert!(repository.commit(target).is_some());
    }

    #[test]
    fn test_file_history_from_a_resolving_merge() {
        use crate::test_support::git::{git, GitRepository};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        std::fs::write(path.join("f"), "base\n").unwrap();
        repo.add(&["f"]);
        repo.commit("base", "2024-01-01");
        repo.checkout_b("side");
        std::fs::write(path.join("f"), "side\n").unwrap();
        repo.add(&["f"]);
        repo.commit("side", "2024-01-02");
        repo.checkout("master");
        std::fs::write(path.join("f"), "master\n").unwrap();
        repo.add(&["f"]);
        repo.commit("master", "2024-01-03");
        // the merge conflicts, its resolution the last change of f
        let _ = git_command()
            .args(["merge", "side"])
            .current_dir(path)
            .output();
        std::fs::write(path.join("f"), "resolved\n").unwrap();
        git(path, &["commit", "-qam", "merge"]);

        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let head = repository.head_commit().unwrap();
        let (history, index) = repository.file_history(&head, "f").unwrap();
        assert!(!history.is_empty());
        assert!(index < history.len());
    }

    #[test]
    fn test_parse_blame() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let out = format!(
            "{a} 1 1 1\nauthor Ann\nauthor-time 0\nauthor-tz -0130\nfilename f\n\tone\n\
             {b} 2 2 1\nauthor Bo\nauthor-time 60\nauthor-tz +0100\nfilename f\n\t\ttwo\n\
             {a} 3 3\n\tthree\n"
        );
        let blame = parse_blame(&out);
        let lines: Vec<(&str, &str)> = blame
            .lines
            .iter()
            .map(|(commit, text)| (commit.as_str(), text.as_str()))
            .collect();
        assert_eq!(lines, [(&*a, "one"), (&*b, "\ttwo"), (&*a, "three")]);
        let ann = &blame.commits[&CommitHash::from(a.as_str())];
        assert_eq!(ann.author, "Ann");
        assert_eq!(ann.date.to_rfc3339(), "1969-12-31T22:30:00-01:30");
        let bo = &blame.commits[&CommitHash::from(b.as_str())];
        assert_eq!(bo.date.to_rfc3339(), "1970-01-01T01:01:00+01:00");
    }

    #[test]
    fn test_parse_name_status() {
        let input = b"M\0a.rs\0R087\0old.rs\0new.rs\0A\0b.rs\0";
        let actual = parse_name_status(input);
        let expected = vec![
            LocalChangeFile {
                path: "a.rs".into(),
                orig_path: None,
                status: LocalChangeStatus::Modified,
            },
            LocalChangeFile {
                path: "new.rs".into(),
                orig_path: Some("old.rs".into()),
                status: LocalChangeStatus::Renamed,
            },
            LocalChangeFile {
                path: "b.rs".into(),
                orig_path: None,
                status: LocalChangeStatus::Added,
            },
        ];
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_parse_status_porcelain() {
        let input = b"M  staged.rs\0 M unstaged.rs\0MM both.rs\0?? new.txt\0R  new_name.rs\0old_name.rs\0UU conflict.rs\0";
        let actual = parse_status_porcelain(input);

        let file = |path: &str, status| LocalChangeFile {
            path: path.into(),
            orig_path: None,
            status,
        };
        let expected = LocalChanges {
            unstaged: vec![
                file("unstaged.rs", LocalChangeStatus::Modified),
                file("both.rs", LocalChangeStatus::Modified),
                file("new.txt", LocalChangeStatus::Untracked),
                file("conflict.rs", LocalChangeStatus::Conflicted),
            ],
            staged: vec![
                file("staged.rs", LocalChangeStatus::Modified),
                file("both.rs", LocalChangeStatus::Modified),
                LocalChangeFile {
                    path: "new_name.rs".into(),
                    orig_path: Some("old_name.rs".into()),
                    status: LocalChangeStatus::Renamed,
                },
            ],
        };
        assert_eq!(actual, expected);
        assert_eq!(actual.count(), 6);
    }

    #[test]
    fn test_parse_status_porcelain_worktree_rename() {
        // `mv README.md docs.md; git add -N docs.md`
        let actual = parse_status_porcelain(b" R docs.md\0README.md\0 M Makefile\0");

        let expected = LocalChanges {
            unstaged: vec![
                LocalChangeFile {
                    path: "docs.md".into(),
                    orig_path: Some("README.md".into()),
                    status: LocalChangeStatus::Renamed,
                },
                LocalChangeFile {
                    path: "Makefile".into(),
                    orig_path: None,
                    status: LocalChangeStatus::Modified,
                },
            ],
            staged: vec![],
        };
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_load_alongside_commits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let git = crate::test_support::git::GitRepository::new(path);
        git.init();
        git.commit("init", "2024-01-01");
        std::fs::write(path.join("a"), "a\n").unwrap();
        git_output(
            path,
            &["remote", "add", "origin", "https://example.com/r.git"],
        )
        .unwrap();

        // the status, remotes and refs are loaded in the background while the commits are
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        assert_eq!(repository.loaded_local_change_count(), 1);
        assert_eq!(repository.remotes(), ["origin"]);
        assert_eq!(
            repository.refs_snapshot(),
            Some(refs_snapshot(path).unwrap().as_str())
        );
        assert!(repository.branch_tracking().is_empty());
    }

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
    fn test_locate_repository() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap();
        git_output(&path, &["init", "-q", "wt"]).unwrap();
        git_output(&path, &["init", "-q", "--bare", "bare.git"]).unwrap();
        std::fs::create_dir(path.join("wt/sub")).unwrap();

        // from a subdirectory: the top of the working tree, and the git directory
        let (toplevel, git_dir) = locate_repository(&path.join("wt/sub")).unwrap();
        assert_eq!(toplevel, Some(path.join("wt")));
        assert_eq!(git_dir, Some(path.join("wt/.git")));
        // a bare repository has no working tree
        let (toplevel, git_dir) = locate_repository(&path.join("bare.git")).unwrap();
        assert_eq!(toplevel, None);
        assert_eq!(git_dir, Some(path.join("bare.git")));
        assert!(locate_repository(&path).is_err());
    }

    #[test]
    fn test_untracked_diff() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let git = crate::test_support::git::GitRepository::new(path);
        git.init();
        git.commit("init", "2024-01-01");
        let names: Vec<String> = (0..20).map(|i| format!("f{i:02}")).collect();
        for name in &names {
            std::fs::write(path.join(name), format!("{name}\n")).unwrap();
        }
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let paths: Vec<&str> = names.iter().map(String::as_str).collect();

        // side by side, the same as one after another, in order
        let one_by_one: String = paths
            .iter()
            .map(|p| repository.local_change_diff(&[p], DiffOf::Untracked))
            .collect();
        assert_eq!(repository.untracked_diff(&paths), one_by_one);
        assert!(one_by_one.contains("+f00") && one_by_one.contains("+f19"));
        assert_eq!(repository.untracked_diff(&[]), "");
    }

    #[test]
    fn test_files_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let git = crate::test_support::git::GitRepository::new(path);
        git.init();
        std::fs::write(path.join("a"), "a\n").unwrap();
        git.add(&["a"]);
        git.commit("init", "2024-01-01");
        std::fs::write(path.join("a"), "b\n").unwrap();
        let git_dir = path.join(".git");
        let load = || FilesSnapshot::load(path, Some(&git_dir));

        let before = load();
        assert_eq!(load(), before);
        // edited again, still modified: the status is the same, the fingerprint isn't
        std::fs::write(path.join("a"), "bc\n").unwrap();
        let edited = load();
        assert_eq!(edited.changes, before.changes);
        assert_ne!(edited.stamp, before.stamp);
        // staged, from outside the app
        git.add(&["a"]);
        assert_ne!(load().stamp, edited.stamp);
    }

    #[test]
    fn test_busy_repository_refuses_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let git = crate::test_support::git::GitRepository::new(path);
        git.init();
        git.commit("init", "2024-01-01");
        std::fs::write(path.join("a"), "a\n").unwrap();
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();

        // while a git command runs in the background
        repository.set_busy(Some("Pushing main".into()));
        assert_eq!(
            repository.stage_paths(&["a"]),
            Err("Pushing main, wait for it to finish".into())
        );
        repository.set_busy(None);
        repository.stage_paths(&["a"]).unwrap();
    }

    #[test]
    fn test_rebase_todo() {
        use crate::test_support::git::git;
        // SHA-256 hashes are longer than the 40 characters of SHA-1
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        git(
            path,
            &["init", "-q", "-b", "master", "--object-format=sha256"],
        );
        for (key, value) in [
            ("user.name", "T"),
            ("user.email", "t@e"),
            ("commit.gpgsign", "false"),
            ("rebase.autoSquash", "true"),
        ] {
            git(path, &["config", key, value]);
        }
        for subject in ["c1", "c2", "fixup! c1", "c3"] {
            git(path, &["commit", "-q", "--allow-empty", "-m", subject]);
        }
        let log = || git(path, &["log", "--format=%s"]);
        let hash = |rev: &str| git(path, &["rev-parse", rev]).trim().to_string();
        let base = hash("HEAD~3");

        // dropping the fixup drops it, nothing is squashed though autoSquash is set
        let todo = [todo_action(&hash("HEAD~1"), "drop")];
        run_task(path, &GitTask::rebase_todo(Some(&base), false, &todo)).unwrap();
        assert_eq!(log(), "c3\nc2\nc1\n");

        // a commit not in the todo list stops the rebase before it changes anything
        let todo = [todo_action(&"0".repeat(64), "drop")];
        let err = run_task(path, &GitTask::rebase_todo(Some(&base), false, &todo)).unwrap_err();
        assert!(err.contains("missing from the todo list"), "{err}");
        assert_eq!(log(), "c3\nc2\nc1\n");
        assert!(!path.join(".git/rebase-merge").exists());
    }

    #[test]
    fn test_task_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let git = crate::test_support::git::GitRepository::new(path);
        git.init();
        git.commit("init", "2024-01-01");
        // the first command reads it, like the message of a commit
        let task = GitTask {
            input: Some("from stdin".into()),
            ..GitTask::new(
                GitCommand::Commit,
                &["--allow-empty", "--no-verify", "-F", "-"],
                String::new(),
                String::new(),
            )
        };
        run_task(path, &task).unwrap();
        let message = git_output(path, &["log", "-1", "--format=%s"]).unwrap();
        assert_eq!(message.trim(), "from stdin");
    }

    #[test]
    fn test_task_moves_head() {
        use GitCommand::*;
        let task =
            |command, args: &[&str]| GitTask::new(command, args, String::new(), String::new());
        assert!(task(Checkout, &["-b", "topic", "HEAD"]).moves_head());
        assert!(task(Pull, &["origin", "main"]).moves_head());
        assert!(!task(Branch, &["topic", "HEAD"]).moves_head());
        // going on with the operation, or aborting it
        assert!(!task(Resume(Operation::Rebase), &["--continue"]).moves_head());
        assert!(!task(Resume(Operation::Merge), &["--abort"]).moves_head());
        // discarding files restores them
        assert!(!task(Restore, &["--", "f"]).moves_head());
        let mut tag_then_reset = task(Tag, &["v1"]);
        tag_then_reset.commands.push(Reset.args(["--hard"]));
        assert!(tag_then_reset.moves_head());
    }

    #[test]
    fn test_task_may_sign() {
        use GitCommand::*;
        let task =
            |command, args: &[&str]| GitTask::new(command, args, String::new(), String::new());
        assert!(task(Rebase, &["-i", "main"]).may_sign());
        assert!(!task(Fetch, &["origin"]).may_sign());
        let mut delete_tag = task(Branch, &["-D", "x"]);
        delete_tag.commands.push(Tag.args(["v1"]));
        assert!(delete_tag.may_sign());
    }

    #[test]
    fn test_paths_are_literal() {
        use crate::test_support::git::GitRepository;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        let write = |name: &str, text: &str| std::fs::write(path.join(name), text).unwrap();
        write("foo[1].txt", "a\n");
        write("foo1.txt", "a\n");
        repo.add(&["foo[1].txt", "foo1.txt"]);
        repo.commit("first", "2024-01-01");
        write("foo[1].txt", "b\n");
        write("foo1.txt", "b\n");
        write("*.log", "x\n");
        write("important.log", "keep\n");

        // only the files named, not those their names match as patterns
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let commands = repository
            .discard_commands(&["foo[1].txt"], &["*.log"], &[])
            .unwrap();
        let task = GitTask {
            commands,
            ..Default::default()
        };
        run_task(path, &task).unwrap();
        let read = |name: &str| std::fs::read_to_string(path.join(name)).ok();
        assert_eq!(read("foo[1].txt").as_deref(), Some("a\n"));
        assert_eq!(read("foo1.txt").as_deref(), Some("b\n"));
        assert_eq!(read("*.log"), None);
        assert_eq!(read("important.log").as_deref(), Some("keep\n"));

        // the root, all of them
        repository.stage_paths(&[":/"]).unwrap();
        let staged = git_output(path, &["diff", "--cached", "--name-only"]).unwrap();
        assert_eq!(staged, "foo1.txt\nimportant.log\n");
    }

    #[test]
    fn test_load_says_why_it_failed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = crate::test_support::git::GitRepository::new(path);
        repo.init();
        repo.commit("first", "2024-01-01");
        // a branch to an object that doesn't exist, like after a broken copy of the repository
        let missing = "1234567890123456789012345678901234567890\n";
        std::fs::write(path.join(".git/refs/heads/broken"), missing).unwrap();
        let err = Repository::load(path, SortCommit::Chronological, None, false).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("git log failed: fatal: bad object"),
            "{err}"
        );
    }

    #[test]
    fn test_stash_kept_when_not_reapplied() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let git = |args: &[&str]| git_output(path, args).unwrap();
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(path.join("f"), "1\n").unwrap();
        git(&["add", "f"]);
        git(&["commit", "-q", "-m", "c1"]);
        git(&["checkout", "-q", "-b", "other"]);
        std::fs::write(path.join("f"), "other\n").unwrap();
        git(&["commit", "-q", "-am", "c2"]);
        git(&["checkout", "-q", "main"]);
        let stashed = |args: &[&str]| GitTask {
            stash: true,
            ..GitTask::new(
                GitCommand::Checkout,
                args,
                "Checking out".into(),
                "Checked out".into(),
            )
        };

        // the command failing, the changes come back as they were
        std::fs::write(path.join("f"), "local\n").unwrap();
        let err = run_task(path, &stashed(&["-q", "nowhere"])).unwrap_err();
        assert!(err.starts_with("git checkout failed:"), "{err}");
        assert_eq!(std::fs::read_to_string(path.join("f")).unwrap(), "local\n");
        assert_eq!(git(&["stash", "list"]), "");

        // reapplying them conflicting, they are kept in the stash, and the user told so
        let err = run_task(path, &stashed(&["-q", "other"])).unwrap_err();
        assert!(err.starts_with("Checked out, but reapplying the local changes failed (they are kept in the stash):"), "{err}");
        assert_eq!(
            git(&["stash", "list", "--format=%s"]),
            "On main: spoon: Checking out\n"
        );
    }

    #[test]
    fn test_hook_failing_before_reading_the_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let git = |args: &[&str]| git_output(path, args).unwrap();
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(path.join("f"), "1\n").unwrap();
        git(&["add", "f"]);
        let hook = path.join(".git/hooks/pre-commit");
        std::fs::write(&hook, "#!/bin/sh\necho 'hook says no' >&2\nexit 1\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();

        // longer than a pipe holds, so writing it fails once git is gone
        let task = GitTask {
            input: Some("m".repeat(1 << 20)),
            ..GitTask::new(
                GitCommand::Commit,
                &["--quiet", "--file=-"],
                String::new(),
                String::new(),
            )
        };
        let err = run_task(path, &task).unwrap_err();
        assert_eq!(err, "git commit failed: hook says no");
    }

    #[test]
    fn test_stash_only_what_git_stashes() {
        let dir = tempfile::tempdir().unwrap();
        let (sub, path) = (dir.path().join("sub"), dir.path().join("work"));
        let git = |dir: &Path, args: &[&str]| git_output(dir, args).unwrap();
        for dir in [&sub, &path] {
            std::fs::create_dir(dir).unwrap();
            git(dir, &["init", "-q", "-b", "main"]);
            git(dir, &["config", "user.name", "t"]);
            git(dir, &["config", "user.email", "t@t"]);
            git(dir, &["config", "commit.gpgsign", "false"]);
            std::fs::write(dir.join("f"), "1\n").unwrap();
            git(dir, &["add", "f"]);
            git(dir, &["commit", "-q", "-m", "c1"]);
        }
        let sub_url = sub.to_str().unwrap();
        let add = [
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            sub_url,
            "sub",
        ];
        git(&path, &add);
        git(&path, &["commit", "-q", "-m", "sub"]);
        // an older stash, and changes only in the submodule, which git doesn't stash
        std::fs::write(path.join("f"), "2\n").unwrap();
        git(&path, &["stash", "-q", "-m", "older"]);
        std::fs::write(path.join("sub/f"), "2\n").unwrap();
        assert!(!git(&path, &["status", "--porcelain"]).is_empty());

        let task = GitTask {
            stash: true,
            ..GitTask::new(GitCommand::Branch, &["other"], String::new(), String::new())
        };
        run_task(&path, &task).unwrap();
        // the older stash is left alone
        let stashes = git(&path, &["stash", "list", "--format=%s"]);
        assert_eq!(stashes, "On main: older\n");
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

    #[test]
    fn test_head_only() {
        use crate::test_support::git::GitRepository;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        repo.commit("base", "2024-01-01");
        repo.checkout_b("other");
        repo.commit("other", "2024-01-02");
        repo.checkout("master");
        repo.commit("main", "2024-01-03");

        let subjects = |repository: &Repository| -> Vec<String> {
            let history = repository.head_history();
            repository
                .all_commits()
                .iter()
                .map(|c| {
                    let mark = if history.contains(&c.commit_hash) {
                        ""
                    } else {
                        "-"
                    };
                    format!("{mark}{}", c.subject)
                })
                .collect()
        };
        // all commits, the other branch's off the history of HEAD
        let all = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        assert_eq!(subjects(&all), ["main", "-other", "base"]);
        let head = ["HEAD".to_string()];
        let head_only =
            Repository::load_scoped(path, SortCommit::Chronological, None, false, Some(&head))
                .unwrap();
        assert_eq!(subjects(&head_only), ["main", "base"]);
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
    fn test_resolve_conflict_keeps_clean_changes() {
        use crate::test_support::git::GitRepository;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        let lines = |first: &str, last: &str| format!("{first}\n2\n3\n4\n5\n6\n7\n8\n{last}\n");
        std::fs::write(path.join("f"), lines("1", "9")).unwrap();
        repo.add(&["f"]);
        repo.commit("base", "2024-01-01");
        repo.checkout_b("incoming");
        std::fs::write(path.join("f"), lines("ONE", "NINE-theirs")).unwrap();
        repo.add(&["f"]);
        repo.commit("incoming", "2024-01-02");
        repo.checkout("master");
        std::fs::write(path.join("f"), lines("1", "NINE-ours")).unwrap();
        repo.add(&["f"]);
        repo.commit("existing", "2024-01-03");
        assert!(git_output(path, &["merge", "incoming"]).is_err());

        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        // refused while a git command runs, the conflict markers kept
        let conflicted = std::fs::read_to_string(path.join("f")).unwrap();
        repository.set_busy(Some("Pushing master".into()));
        assert!(repository
            .resolve_conflict("f", ConflictSide::Ours)
            .is_err());
        assert_eq!(std::fs::read_to_string(path.join("f")).unwrap(), conflicted);
        repository.set_busy(None);
        repository
            .resolve_conflict("f", ConflictSide::Ours)
            .unwrap();
        // the conflict goes our way, the clean change of the incoming side stays
        let content = std::fs::read_to_string(path.join("f")).unwrap();
        assert_eq!(content, lines("ONE", "NINE-ours"));
        assert!(repository.local_changes().unstaged.is_empty());
    }

    #[test]
    fn test_resolve_conflict_only_touches_its_file() {
        use crate::test_support::git::GitRepository;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        std::fs::write(path.join("x*.txt"), "base\n").unwrap();
        std::fs::write(path.join("xa.txt"), "keep\n").unwrap();
        repo.add(&["."]);
        repo.commit("base", "2024-01-01");
        repo.checkout_b("incoming");
        std::fs::write(path.join("x*.txt"), "theirs\n").unwrap();
        repo.add(&["."]);
        repo.commit("incoming", "2024-01-02");
        repo.checkout("master");
        std::fs::write(path.join("x*.txt"), "ours\n").unwrap();
        repo.add(&["."]);
        repo.commit("existing", "2024-01-03");
        assert!(git_output(path, &["merge", "incoming"]).is_err());
        std::fs::write(path.join("xa.txt"), "unsaved\n").unwrap();

        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        repository
            .resolve_conflict("x*.txt", ConflictSide::Ours)
            .unwrap();
        // a file the name matches as a pattern keeps its changes
        let content = std::fs::read_to_string(path.join("xa.txt")).unwrap();
        assert_eq!(content, "unsaved\n");
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
        let diff =
            parse_diff(&repository.local_change_diff(&["é.txt"], DiffOf::Staged { base: None }));
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
        let staged =
            parse_diff(&repository.local_change_diff(&[":/"], DiffOf::Staged { base: None }));
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
        let head = repository.head_commit().unwrap();
        let base = format!("{}~1", head.as_str());
        let diff = parse_diff(&repository.diff_between(&base, head.as_str(), &[]));
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

    #[test]
    fn test_ignore_path() {
        use crate::test_support::git::GitRepository;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        std::fs::write(path.join(".gitignore"), "*.log").unwrap();
        std::fs::create_dir(path.join("build")).unwrap();
        for file in ["a[1].txt", "build/out", "keep.txt", "t.txt"] {
            std::fs::write(path.join(file), "x").unwrap();
        }
        repo.add(&["t.txt"]);
        repo.commit("t", "2024-01-01");
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();

        repository.ignore_path(Entry::File("a[1].txt")).unwrap();
        repository.ignore_path(Entry::Dir("build")).unwrap();
        let tracked = repository.ignore_path(Entry::File("t.txt")).unwrap();
        assert!(tracked.contains("tracked"), "{tracked}");
        let again = repository.ignore_path(Entry::Dir("build")).unwrap();
        assert!(again.contains("already"), "{again}");
        let content = std::fs::read_to_string(path.join(".gitignore")).unwrap();
        assert_eq!(content, "*.log\n/a\\[1].txt\n/build/\n/t.txt\n");
        // git ignores exactly them: keep.txt shows, a[1].txt and build/ don't
        let status = git_output(path, &["status", "--porcelain", "--untracked-files=all"]).unwrap();
        assert_eq!(status, "?? .gitignore\n?? keep.txt\n");
        assert_eq!(gitignore_pattern(Entry::File("#x ")), "/\\#x\\ ");
    }

    #[test]
    fn test_command_log() {
        use crate::test_support::git::GitRepository;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        repo.commit("a", "2024-01-01");
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        // the log is shared by the tests running at the same time, so this one is found by name
        assert!(repository
            .stage_paths(&["no-such-file-for-the-log"])
            .is_err());
        let log = command_log();
        let entry = log
            .iter()
            .find(|e| e.command.contains("no-such-file-for-the-log"))
            .unwrap();
        assert!(!entry.ok);
        assert!(entry.command.starts_with("git add --all -- "));
        assert!(
            entry.output.contains("did not match any files"),
            "{}",
            entry.output
        );

        assert_eq!(
            hide_password("https://me:token@github.com/a/b.git"),
            "https://***@github.com/a/b.git"
        );
        assert_eq!(
            hide_password("git@github.com:a/b.git"),
            "git@github.com:a/b.git"
        );
        assert_eq!(hide_password("https://host/a@b"), "https://host/a@b");
    }

    #[test]
    fn test_launch_fetch_moves_snapshot_and_times_out() {
        use crate::test_support::git::GitRepository;

        let dir = tempfile::tempdir().unwrap();
        let remote = dir.path().join("remote.git");
        let work = dir.path().join("work");
        std::fs::create_dir(&work).unwrap();
        git_output(dir.path(), &["init", "-q", "--bare", "remote.git"]).unwrap();
        let repo = GitRepository::new(&work);
        repo.init();
        repo.commit("first", "2024-01-01");
        let url = remote.to_str().unwrap();
        git_output(&work, &["remote", "add", "origin", url]).unwrap();
        git_output(&work, &["push", "-q", "origin", "HEAD:refs/heads/main"]).unwrap();
        let fetch = |timeout| GitTask {
            timeout,
            ..GitTask::new(GitCommand::Fetch, &["--all"], String::new(), String::new())
        };

        // a branch made on the remote shows in the fingerprint once fetched, which reloads the app
        let head = git_output(&work, &["rev-parse", "HEAD"]).unwrap();
        let other = [
            "--git-dir",
            url,
            "update-ref",
            "refs/heads/other",
            head.trim(),
        ];
        git_output(dir.path(), &other).unwrap();
        let before = refs_snapshot(&work).unwrap();
        run_task(&work, &fetch(None)).unwrap();
        assert_ne!(refs_snapshot(&work).unwrap(), before);

        // a remote that doesn't answer is given up on, and leaves nothing in the way of the next fetch
        let slow = [
            "config",
            "remote.origin.uploadpack",
            "sleep 5; git-upload-pack",
        ];
        git_output(&work, &slow).unwrap();
        let started = Instant::now();
        let err = run_task(&work, &fetch(Some(Duration::from_millis(300)))).unwrap_err();
        assert_eq!(err, "git fetch timed out after 0s");
        assert!(started.elapsed() < Duration::from_secs(3));
        git_output(&work, &["config", "--unset", "remote.origin.uploadpack"]).unwrap();
        run_task(&work, &fetch(None)).unwrap();
    }

    #[test]
    fn test_refs_snapshot_sees_stashes() {
        use crate::test_support::git::GitRepository;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        std::fs::write(path.join("f"), "a\n").unwrap();
        repo.add(&["f"]);
        repo.commit("base", "2024-01-01");
        let before = refs_snapshot(path).unwrap();
        std::fs::write(path.join("f"), "b\n").unwrap();
        git_output(path, &["stash", "push", "-q"]).unwrap();
        // a stash made outside the app reloads the sidebar, which drops stashes by position
        let one = refs_snapshot(path).unwrap();
        assert_ne!(one, before);
        std::fs::write(path.join("f"), "c\n").unwrap();
        git_output(path, &["stash", "push", "-q"]).unwrap();
        let two = refs_snapshot(path).unwrap();
        // so does dropping an older one, which leaves refs/stash as it is
        git_output(path, &["stash", "drop", "-q", "stash@{1}"]).unwrap();
        assert_ne!(refs_snapshot(path).unwrap(), two);
    }

    #[test]
    fn test_local_changes_from_subdirectory() {
        use crate::test_support::git::GitRepository;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        std::fs::create_dir(path.join("sub")).unwrap();
        std::fs::write(path.join("sub/f"), "a").unwrap();
        repo.add(&["sub/f"]);
        repo.commit("first", "2024-01-01");
        std::fs::write(path.join("sub/f"), "b").unwrap();

        let repository =
            Repository::load(&path.join("sub"), SortCommit::Chronological, None, false).unwrap();
        let changes = repository.local_changes();
        assert_eq!(changes.unstaged[0].path, "sub/f");
        repository.stage_paths(&["sub/f"]).unwrap();
        assert_eq!(repository.local_changes().staged[0].path, "sub/f");
        assert_eq!(repository.operation_in_progress(), None);
    }
}
