use std::{
    collections::VecDeque,
    hash::Hash,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, Stdio},
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

    // The name git can't take for another ref, as a branch and a tag may share a name.
    pub fn full_name(&self) -> String {
        match self {
            Ref::Tag { name, .. } => format!("refs/tags/{name}"),
            Ref::Branch { name, .. } => format!("refs/heads/{name}"),
            Ref::RemoteBranch { name, .. } => format!("refs/remotes/{name}"),
            Ref::Stash { name, .. } => name.clone(),
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
    // how far each local branch is ahead and behind its upstream when loaded, see `branch_tracking`
    tracking: FxHashMap<String, (usize, usize)>,
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

        let stashes = if revisions.is_some() {
            vec![]
        } else {
            load_all_stashes(path, mailmap)
        };
        let commits = load_all_commits(path, sort, &head, &stashes, max_count, mailmap, revisions);
        if commits.is_empty() {
            return Err("no commits in the repository".into());
        }

        let commits = merge_stashes_to_commits(commits, stashes);
        let commit_hashes = commits.iter().map(|c| c.commit_hash.clone()).collect();

        let (parents_map, children_map) = build_commits_maps(&commits);
        let commit_map = to_commit_map(commits);

        let stash_ref_map = load_stashes_as_refs(path);
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
        let changes = if commit.parent_commit_hashes.is_empty() {
            get_initial_commit_additions(&self.path, commit_hash)
        } else {
            get_diff_summary(&self.path, commit_hash)
        };
        (commit, changes)
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

    // `staged_base` is the tree staged changes are compared to, HEAD when None (see `amend_base`)
    // `untracked` only applies to a single path
    pub fn local_change_diff(
        &self,
        paths: &[&str],
        untracked: bool,
        staged: bool,
        staged_base: Option<&str>,
    ) -> String {
        if paths.is_empty() {
            return String::new();
        }
        load_local_change_diff(&self.path, paths, untracked, staged, staged_base)
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
                                load_local_change_diff(work_tree, &[path], true, false, None)
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
        let base = match self.parents_hash(commit_hash).first() {
            Some(parent) => parent.as_str().to_string(),
            None => match empty_tree(&self.path) {
                Ok(tree) => tree,
                Err(_) => return String::new(),
            },
        };
        self.diff_between(&base, commit_hash.as_str(), paths)
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

    // The local branches of `names` merged neither into HEAD nor into their upstream.
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
        let merged_into_upstream = |branch: &str| {
            let (branch, upstream) = (
                format!("refs/heads/{branch}"),
                format!("{branch}@{{upstream}}"),
            );
            git_output(
                &self.path,
                &["merge-base", "--is-ancestor", &branch, &upstream],
            )
            .is_ok()
        };
        names
            .iter()
            .filter(|name| !merged.contains(name.as_str()) && !merged_into_upstream(name))
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
            let output = git_command()
                .args(&args)
                .current_dir(&self.path)
                .stdin(Stdio::null())
                .output()
                .ok()?;
            let out = String::from_utf8_lossy(&output.stdout);
            let mut lines = out.lines();
            let result = lines.next()?.to_string();
            match output.status.code() {
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
    ) -> std::result::Result<Vec<Vec<String>>, String> {
        let mut tracked = tracked.to_vec();
        let mut untracked = untracked.to_vec();
        let command = |args: &[&str], paths: &[&str]| -> Vec<String> {
            let mut command: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            command.push("--".into());
            command.extend(pathspecs(paths));
            command
        };
        let mut commands = vec![];
        if !conflicted.is_empty() {
            commands.push(command(&["reset", "--quiet"], conflicted));
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
            commands.push(command(&["checkout", "--quiet"], &tracked));
        }
        if !untracked.is_empty() {
            commands.push(command(&["clean", "--force", "--quiet"], &untracked));
        }
        Ok(commands)
    }

    // Adds the file or directory (`dir`) at `path` to the .gitignore at the top of the working
    // tree, anchored there. Returns what to tell: a tracked path stays tracked, ignoring only
    // keeps untracked files out.
    pub fn ignore_path(&self, path: &str, dir: bool) -> std::result::Result<String, String> {
        if path == ":/" {
            return Err("The whole repository can't be ignored, choose a file or directory".into());
        }
        let pattern = gitignore_pattern(path, dir);
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
        let specs = pathspecs(&[path]);
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

    // Applies `patch` to the working tree, `reverse` to undo the changes in it.
    #[cfg(test)]
    pub fn apply_patch_to_worktree(
        &self,
        patch: &str,
        reverse: bool,
    ) -> std::result::Result<(), String> {
        let mut args = vec!["apply"];
        if reverse {
            args.push("--reverse");
        }
        args.push("-");
        self.run(&args, Some(patch))
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
    pub fn branch_tracking(&self) -> &FxHashMap<String, (usize, usize)> {
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
                        upstreams
                            .insert(name.to_string(), (remote.to_string(), branch.to_string()));
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
        run_git(&self.path, args, stdin)
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
        let message = self.message_file("MERGE_MSG")?;
        let content = std::fs::read_to_string(self.git_path("MERGE_MSG")?).ok()?;
        let conflicts: Vec<&str> = content
            .lines()
            .skip_while(|l| l.trim() != "# Conflicts:")
            .skip(1)
            .map_while(|l| l.strip_prefix('#').map(str::trim))
            .filter(|path| !path.is_empty())
            .collect();
        if conflicts.is_empty() {
            return Some(message);
        }
        let files: Vec<String> = conflicts.iter().map(|path| format!("  {path}")).collect();
        Some(format!("{message}\n\nConflicts:\n{}", files.join("\n")))
    }

    // The message `git merge --squash` prepared, until the squash is committed.
    pub fn squash_message(&self) -> Option<String> {
        self.message_file("SQUASH_MSG")
    }

    fn message_file(&self, name: &str) -> Option<String> {
        let content = std::fs::read_to_string(self.git_path(name)?).ok()?;
        let message: Vec<&str> = content.lines().filter(|l| !l.starts_with('#')).collect();
        Some(message.join("\n").trim().to_string()).filter(|m| !m.is_empty())
    }

    // The command continuing a rebase, or the remaining commits of a cherry-pick or revert of
    // several commits, after the commit concluding the stop; None when that commit concludes it.
    pub fn continue_args(&self, operation: Operation) -> Option<Vec<String>> {
        // a merge, or a single cherry-pick or revert, is done once committed
        let goes_on = operation == Operation::Rebase
            || (operation != Operation::Merge
                && self.git_path("sequencer").is_some_and(|p| p.exists()));
        goes_on.then(|| vec![operation.name().to_string(), "--continue".into()])
    }

    fn git_path(&self, name: &str) -> Option<PathBuf> {
        if let Some(git_dir) = &self.git_dir {
            return Some(git_dir.join(name));
        }
        git_output(&self.path, &["rev-parse", "--git-path", name])
            .map(|p| self.path.join(p.trim()))
            .ok()
    }

    // Stages (or unstages, with `reverse`) the changes in `patch`, a diff produced by `local_change_diff`.
    pub fn apply_patch_to_index(
        &self,
        patch: &str,
        reverse: bool,
    ) -> std::result::Result<(), String> {
        let mut args = vec!["apply", "--cached"];
        if reverse {
            args.push("--reverse");
        }
        args.push("-");
        self.run(&args, Some(patch))
    }
}

// The .gitignore line matching only `path` from the top: the characters git reads as a pattern
// escaped, and a directory ending with "/".
fn gitignore_pattern(path: &str, dir: bool) -> String {
    let mut pattern = String::from("/");
    for c in path.chars() {
        if matches!(c, '*' | '?' | '[' | '\\' | '!' | '#') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    // trailing spaces are dropped unless escaped
    let spaces = pattern.len() - pattern.trim_end_matches(' ').len();
    pattern.truncate(pattern.len() - spaces);
    pattern.push_str(&"\\ ".repeat(spaces));
    if dir {
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
) -> Vec<Commit> {
    let mut cmd = git_command();
    cmd.arg("log");

    cmd.arg(match sort {
        SortCommit::Chronological => "--date-order",
        SortCommit::Topological => "--topo-order",
    })
    .arg(format!("--pretty={}", load_commits_format(mailmap)))
    .arg("--date=iso-strict")
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

    // an unknown revision only leaves the history empty
    cmd.current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut process = cmd.spawn().unwrap();

    let stdout = process.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let mut commits = Vec::new();

    for bytes in reader.split(b'\0') {
        let bytes = bytes.unwrap();
        let s = String::from_utf8_lossy(&bytes);

        let parts: Vec<&str> = s.split('\x1f').collect();
        if parts.len() != 10 {
            panic!("unexpected number of parts: {} [{}]", parts.len(), s);
        }

        let commit = Commit {
            commit_hash: parts[0].into(),
            author_name: parts[1].into(),
            author_email: parts[2].into(),
            author_date: parse_iso_date(parts[3]),
            committer_name: parts[4].into(),
            committer_email: parts[5].into(),
            committer_date: parse_iso_date(parts[6]),
            subject: parts[7].into(),
            body: parts[8].into(),
            parent_commit_hashes: parse_parent_commit_hashes(parts[9]),
        };

        commits.push(commit);
    }

    process.wait().unwrap();

    commits
}

fn load_all_stashes(path: &Path, mailmap: bool) -> Vec<Commit> {
    let mut cmd = git_command()
        .arg("stash")
        .arg("list")
        .arg(format!("--pretty={}", load_commits_format(mailmap)))
        .arg("--date=iso-strict")
        .arg("-z") // use NUL as a delimiter
        .current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = cmd.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let mut commits = Vec::new();

    for bytes in reader.split(b'\0') {
        let bytes = bytes.unwrap();
        let s = String::from_utf8_lossy(&bytes);

        let parts: Vec<&str> = s.split('\x1f').collect();
        if parts.len() != 10 {
            panic!("unexpected number of parts: {} [{}]", parts.len(), s);
        }

        let commit = Commit {
            commit_hash: parts[0].into(),
            author_name: parts[1].into(),
            author_email: parts[2].into(),
            author_date: parse_iso_date(parts[3]),
            committer_name: parts[4].into(),
            committer_email: parts[5].into(),
            committer_date: parse_iso_date(parts[6]),
            subject: parts[7].into(),
            body: parts[8].into(),
            parent_commit_hashes: parse_parent_commit_hashes(parts[9]),
        };

        commits.push(commit);
    }

    cmd.wait().unwrap();

    commits
}

fn load_commits_format(mailmap: bool) -> String {
    // The uppercase name/email placeholders (`%aN`, `%aE`, `%cN`, `%cE`) resolve
    // identities through the repository's .mailmap, while the lowercase variants
    // use the raw values recorded in each commit.
    let format = if mailmap {
        [
            "%H", "%aN", "%aE", "%ad", "%cN", "%cE", "%cd", "%s", "%b", "%P",
        ]
    } else {
        [
            "%H", "%an", "%ae", "%ad", "%cn", "%ce", "%cd", "%s", "%b", "%P",
        ]
    };
    format.join("%x1f") // use Unit Separator as a delimiter
}

fn parse_iso_date(s: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(s).unwrap()
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
    let mut cmd = git_command()
        .arg("show-ref")
        .arg("--head")
        .arg("--dereference")
        .current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = cmd.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let mut ref_map = RefMap::default();
    let mut tag_map: FxHashMap<String, Ref> = FxHashMap::default();
    let mut head: Head = Head::None;

    for line in reader.lines() {
        let line = line.unwrap();

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

    cmd.wait().unwrap();

    (ref_map, head)
}

fn load_stashes_as_refs(path: &Path) -> RefMap {
    let format = ["%gd", "%H", "%s"].join("%x1f"); // use Unit Separator as a delimiter
    let mut cmd = git_command()
        .arg("stash")
        .arg("list")
        .arg(format!("--format={format}"))
        .current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = cmd.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let mut ref_map = RefMap::default();

    for line in reader.lines() {
        let line = line.unwrap();

        let parts: Vec<&str> = line.split('\x1f').collect();
        if parts.len() != 3 {
            panic!("unexpected number of parts: {} [{}]", parts.len(), line);
        }

        let name = parts[0];
        let hash = parts[1];
        let subject = parts[2];

        let r = Ref::Stash {
            name: name.into(),
            message: subject.into(),
            target: hash.into(),
        };

        ref_map.entry(hash.into()).or_default().push(r);
    }

    cmd.wait().unwrap();

    ref_map
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

fn get_current_branch(path: &Path) -> Option<String> {
    let mut cmd = git_command()
        .arg("branch")
        .arg("--show-current")
        .current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = cmd.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let branch = if let Some(line) = reader.lines().next() {
        line.ok()
    } else {
        None
    };

    cmd.wait().unwrap();

    branch
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

pub fn get_diff_summary(path: &Path, commit_hash: &CommitHash) -> Vec<FileChange> {
    diff_summary(path, &format!("{}^", commit_hash.0), &commit_hash.0)
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

pub fn get_initial_commit_additions(path: &Path, commit_hash: &CommitHash) -> Vec<FileChange> {
    // -z leaves paths as they are, git would quote those with special characters
    git_output_bytes(
        path,
        &["ls-tree", "-r", "-z", "--name-only", &commit_hash.0],
    )
    .unwrap_or_default()
    .split(|b| *b == b'\0')
    .filter(|p| !p.is_empty())
    .map(|p| FileChange::Add {
        path: String::from_utf8_lossy(p).into_owned(),
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

fn load_local_change_diff(
    path: &Path,
    targets: &[&str],
    untracked: bool,
    staged: bool,
    staged_base: Option<&str>,
) -> String {
    let mut cmd = git_command();
    cmd.args(PATCH_DIFF_ARGS);
    if staged {
        cmd.arg("--cached")
            .args(staged_base)
            .arg("--")
            .args(pathspecs(targets));
    } else if untracked {
        cmd.arg("--no-index")
            .arg("--")
            .arg("/dev/null")
            .args(targets);
    } else {
        cmd.arg("--").args(pathspecs(targets));
    }
    // `--no-index` exits with 1 when the files differ, so the exit status is not checked
    cmd.current_dir(path)
        .stderr(Stdio::null())
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default()
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
    pub upstreams: FxHashMap<String, (String, String)>,
    pub pull_rebase: bool,
    // whether pulling stashes local changes, when rebasing and when merging
    pub rebase_auto_stash: bool,
    pub merge_auto_stash: bool,
}

impl RemoteInfo {
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
fn load_branch_tracking(path: &Path) -> FxHashMap<String, (usize, usize)> {
    // the full name: the short one is "heads/v2" when a tag is named v2 too
    let format = "--format=%(refname)%00%(upstream:track,nobracket)";
    let refs = git_output(path, &["for-each-ref", format, "refs/heads"]).unwrap_or_default();
    refs.lines()
        .filter_map(|line| {
            let (name, track) = line.split_once('\0')?;
            let name = name.strip_prefix("refs/heads/")?;
            // like "ahead 2, behind 1", empty when in sync or without an upstream
            let count = |what: &str| {
                track
                    .split(", ")
                    .find_map(|part| part.strip_prefix(what)?.trim().parse().ok())
                    .unwrap_or(0)
            };
            let counts = (count("ahead "), count("behind "));
            (counts != (0, 0)).then(|| (name.to_string(), counts))
        })
        .collect()
}

// A fingerprint of the local branches, HEAD and the stashes, which changes when they are changed
// from outside the app, like lazygit's `RefsSnapshot`: where each branch points, the branch
// checked out or the commit when detached, and every stash, as the sidebar drops them by position.
pub fn refs_snapshot(path: &Path) -> std::result::Result<String, String> {
    let refs = git_output(
        path,
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            "refs/heads",
        ],
    )?;
    let head = git_output(path, &["symbolic-ref", "-q", "HEAD"])
        .or_else(|_| git_output(path, &["rev-parse", "-q", "--verify", "HEAD"]))
        .unwrap_or_default();
    // older stashes are only in the reflog of refs/stash, which for-each-ref doesn't see
    let stashes = git_output(path, &["stash", "list", "--format=%H"]).unwrap_or_default();
    Ok(refs + &head + &stashes)
}

// Git commands run in the background, like fetch, merge or checkout, and what to say about them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GitTask {
    // the arguments of the git commands, each run when the one before succeeded
    pub commands: Vec<Vec<String>>,
    // stash the local changes before the command, and reapply them after
    pub stash: bool,
    // talks to a remote
    pub remote: bool,
    pub env: Vec<(String, String)>,
    // "Pushing main to origin/main"
    pub running: String,
    // "Pushed main to origin/main"
    pub done: String,
    // made of the picked commits, which are cleared once it is done
    pub uses_picks: bool,
    // leaves changes to commit, so the local changes page is shown once it is done
    pub show_local_changes: bool,
    // the commit to select once it is done, like "HEAD~2", as rewriting the history changes its hash
    pub select: Option<String>,
    // the local branch it pushes or pulls, shown beside it in the sidebar while it runs, like lazygit
    pub branch_status: Option<BranchStatus>,
    // what the first command reads, like the message of a commit
    pub input: Option<String>,
    // said when it fails instead of git's error, which may tell little
    pub failed: Option<String>,
    // changes only the working tree, so the page stays as it is instead of being reloaded
    pub keeps_view: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchStatus {
    pub branch: String,
    // "Pushing", and "↑" where that doesn't fit
    pub label: String,
    pub short: &'static str,
}

impl GitTask {
    // Whether it may make a commit or a tag, which git signs when configured to.
    pub fn may_sign(&self) -> bool {
        self.commands
            .iter()
            .filter_map(|args| args.first())
            .any(|command| {
                matches!(
                    command.as_str(),
                    "commit" | "rebase" | "cherry-pick" | "merge" | "revert" | "tag" | "pull"
                )
            })
    }

    // Whether it moves or rewrites HEAD, which git refuses or tangles up while a rebase, merge,
    // cherry-pick or revert waits to be continued or aborted; going on with it or aborting it doesn't.
    pub fn moves_head(&self) -> bool {
        self.commands.iter().any(|args| {
            let resumes = args
                .iter()
                .any(|a| matches!(a.as_str(), "--continue" | "--abort" | "--skip" | "--quit"));
            let command = args.first().map(String::as_str);
            !resumes
                && matches!(
                    command,
                    Some(
                        "checkout"
                            | "switch"
                            | "rebase"
                            | "merge"
                            | "cherry-pick"
                            | "revert"
                            | "reset"
                            | "pull"
                    )
                )
        })
    }

    pub fn new(args: &[&str], running: String, done: String) -> Self {
        GitTask {
            commands: vec![args.iter().map(|a| a.to_string()).collect()],
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
            commands: vec![[
                "rebase",
                "-i",
                "--no-autosquash",
                stash,
                base.unwrap_or("--root"),
            ]
            .map(String::from)
            .to_vec()],
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
    // stashed when the stash changed: git stashes nothing for some changes it lists, like those
    // in a submodule, and popping then would take an older stash
    let stash_top = || git_output(path, &["rev-parse", "--verify", "--quiet", "refs/stash"]).ok();
    let stashed = task.stash && {
        let before = stash_top();
        let message = format!("spoon: {}", task.running);
        let args = ["stash", "push", "--include-untracked", "-m", &message].map(String::from);
        run_task_step(path, &args, None, task)?;
        stash_top() != before
    };
    let pop = || run_task_step(path, &["stash", "pop"].map(String::from), None, task);

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
    args: &[String],
    input: Option<&str>,
    task: &GitTask,
) -> std::result::Result<(), String> {
    let mut cmd = git_command();
    cmd.args(args)
        .current_dir(path)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_EDITOR", "true")
        .env("GIT_TERMINAL_PROMPT", "0")
        .envs(task.env.iter().map(|(k, v)| (k, v)));
    if task.remote {
        let has_ssh_command = std::env::var_os("GIT_SSH_COMMAND").is_some()
            || std::env::var_os("GIT_SSH").is_some()
            || git_output(path, &["config", "--get", "core.sshCommand"]).is_ok();
        if !has_ssh_command {
            cmd.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
        }
    }
    let started = Instant::now();
    let command_error = |e: std::io::Error| format!("failed to run git {}: {e}", args.join(" "));
    let mut child = cmd.spawn().map_err(command_error)?;
    if let Some(input) = input {
        use std::io::Write;
        // closed once written, so git can finish
        let mut stdin = child.stdin.take().expect("stdin is piped");
        stdin.write_all(input.as_bytes()).map_err(command_error)?;
    }
    let output = child.wait_with_output().map_err(command_error)?;
    // git reports progress and results of many commands on stderr
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    log_command(args, output.status.success(), &text, started);
    if output.status.success() {
        Ok(())
    } else {
        // a rejected ref or a conflict says what went wrong best, then the error lines,
        // and the hints after them how to fix it
        let lines = || text.lines().map(str::trim);
        let error = lines()
            .find(|line| line.starts_with("! ") || line.starts_with("CONFLICT"))
            .or_else(|| {
                lines().find(|line| line.starts_with("error:") || line.starts_with("fatal:"))
            })
            .or_else(|| lines().rfind(|line| !line.is_empty()))
            .unwrap_or_default();
        Err(format!("git {} failed: {error}", args[0]))
    }
}

// The command stashing the changes of `paths` (all changes when empty), including untracked files.
pub fn stash_args(paths: &[String], message: &str) -> Vec<String> {
    let mut args: Vec<String> = ["stash", "push", "--include-untracked", "--quiet"]
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
    args
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
// index, which would make the user's own git commands fail meanwhile.
fn git_command() -> Command {
    let mut cmd = Command::new("git");
    cmd.env("GIT_OPTIONAL_LOCKS", "0");
    cmd
}

// Paths of files or directories for git, taken as they are, not as patterns: a file named
// `*.log` is only that file. The root of the repository stays ":/".
fn pathspecs(paths: &[&str]) -> Vec<String> {
    paths
        .iter()
        .map(|path| match *path {
            ":/" => path.to_string(),
            path => format!(":(literal){path}"),
        })
        .collect()
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

fn run_git(path: &Path, args: &[&str], stdin: Option<&str>) -> std::result::Result<(), String> {
    let command_error = |e: std::io::Error| format!("failed to run git {}: {e}", args.join(" "));
    let started = Instant::now();
    let mut child = git_command()
        .args(args)
        .current_dir(path)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(command_error)?;

    if let Some(input) = stdin {
        use std::io::Write;
        // dropped at the end of this block, closing stdin so git can finish
        let mut child_stdin = child.stdin.take().expect("failed to open stdin");
        child_stdin
            .write_all(input.as_bytes())
            .map_err(command_error)?;
    }

    let output = child.wait_with_output().map_err(command_error)?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    log_command(args, output.status.success(), &text, started);
    if output.status.success() {
        Ok(())
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
        // behind HEAD, ahead of it, and ahead of it but pushed to its upstream (a local branch here)
        git_output(path, &["branch", "behind"]).unwrap();
        git.commit("second", "2024-01-02");
        git.checkout_b("ahead");
        git.commit("third", "2024-01-03");
        git_output(path, &["branch", "pushed"]).unwrap();
        git_output(path, &["branch", "--set-upstream-to=ahead", "pushed"]).unwrap();
        git.checkout("master");
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();

        let names = ["behind", "ahead", "pushed"].map(String::from);
        assert_eq!(repository.unmerged_branches(&names), ["ahead"]);
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
            .map(|p| repository.local_change_diff(&[p], true, false, None))
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
                &[
                    "-c",
                    "user.name=T",
                    "-c",
                    "user.email=t@e",
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "--allow-empty",
                    "--no-verify",
                    "-F",
                    "-",
                ],
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
        let task = |args: &[&str]| GitTask::new(args, String::new(), String::new());
        assert!(task(&["checkout", "-b", "topic", "HEAD"]).moves_head());
        assert!(task(&["pull", "origin", "main"]).moves_head());
        assert!(!task(&["branch", "topic", "HEAD"]).moves_head());
        // going on with the operation, or aborting it
        assert!(!task(&["rebase", "--continue"]).moves_head());
        assert!(!task(&["merge", "--abort"]).moves_head());
        let mut tag_then_reset = task(&["tag", "v1"]);
        tag_then_reset
            .commands
            .push(vec!["reset".into(), "--hard".into()]);
        assert!(tag_then_reset.moves_head());
    }

    #[test]
    fn test_task_may_sign() {
        let task = |args: &[&str]| GitTask::new(args, String::new(), String::new());
        assert!(task(&["rebase", "-i", "main"]).may_sign());
        assert!(!task(&["fetch", "origin"]).may_sign());
        let mut delete_tag = task(&["branch", "-D", "x"]);
        delete_tag.commands.push(vec!["tag".into(), "v1".into()]);
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
            ..GitTask::new(&["branch", "other"], String::new(), String::new())
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

        let run = |s: &str| {
            let args: Vec<&str> = s.split(' ').collect();
            run_task(&work, &GitTask::new(&args, String::new(), String::new()))
        };
        run("push --set-upstream origin refs/heads/master:refs/heads/main").unwrap();
        run("fetch --all").unwrap();

        let repository = Repository::load(&work, SortCommit::Chronological, None, false).unwrap();
        let info = repository.remote_info();
        assert_eq!(info.current_branch.as_deref(), Some("master"));
        assert_eq!(info.remotes, vec!["origin"]);
        assert_eq!(info.branches, vec!["master"]);
        assert_eq!(info.remote_branches["origin"], vec!["main"]);
        assert_eq!(
            info.upstreams["master"],
            ("origin".to_string(), "main".to_string())
        );

        // the reason is reported, not the hints after it
        let err = run("fetch nowhere").unwrap_err();
        assert!(err.starts_with("git fetch failed: fatal:"), "{err}");

        // in sync, then two commits to push and, once they are pushed and one undone, one to pull
        let tracking = || load_branch_tracking(&work);
        assert!(tracking().is_empty());
        repo.commit("second", "2024-01-02");
        repo.commit("third", "2024-01-03");
        assert_eq!(tracking()["master"], (2, 0));
        run("push origin master:main").unwrap();
        git_output(&work, &["reset", "-q", "--hard", "HEAD~1"]).unwrap();
        assert_eq!(tracking()["master"], (0, 1));
        // also with a tag of the same name
        git_output(&work, &["tag", "master"]).unwrap();
        assert_eq!(tracking()["master"], (0, 1));
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
        let diff = parse_diff(&repository.local_change_diff(&["é.txt"], false, true, None));
        // unstaging the second line keeps the file staged with the first
        let patch = diff.hunks[0].partial_patch(&[1], true);
        repository.apply_patch_to_index(&patch, true).unwrap();
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
        let staged = parse_diff(&repository.local_change_diff(&[":/"], false, true, None));
        assert_eq!(staged.hunks.len(), 2);
        repository
            .apply_patch_to_index(&staged.hunks[0].patch(), true)
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
        repository
            .apply_patch_to_worktree(&diff.hunks[0].partial_patch(&[3, 4], false), false)
            .unwrap();
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

        repository.ignore_path("a[1].txt", false).unwrap();
        repository.ignore_path("build", true).unwrap();
        let tracked = repository.ignore_path("t.txt", false).unwrap();
        assert!(tracked.contains("tracked"), "{tracked}");
        let again = repository.ignore_path("build", true).unwrap();
        assert!(again.contains("already"), "{again}");
        let content = std::fs::read_to_string(path.join(".gitignore")).unwrap();
        assert_eq!(content, "*.log\n/a\\[1].txt\n/build/\n/t.txt\n");
        // git ignores exactly them: keep.txt shows, a[1].txt and build/ don't
        let status = git_output(path, &["status", "--porcelain", "--untracked-files=all"]).unwrap();
        assert_eq!(status, "?? .gitignore\n?? keep.txt\n");
        assert_eq!(gitignore_pattern("#x ", false), "/\\#x\\ ");
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
