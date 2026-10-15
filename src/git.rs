use std::{
    hash::Hash,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use chrono::{DateTime, FixedOffset};
use rustc_hash::FxHashMap;

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
    commit_map: CommitMap,

    parents_map: CommitsMap,
    children_map: CommitsMap,

    ref_map: RefMap,
    head: Head,
    // to preserve order of the original commits from `git log`, we store the commit hashes
    commit_hashes: Vec<CommitHash>,
}

impl Repository {
    pub fn load(
        path: &Path,
        sort: SortCommit,
        max_count: Option<usize>,
        mailmap: bool,
    ) -> Result<Self> {
        check_git_repository(path)?;
        let (toplevel, git_dir) = locate_repository(path);
        let path = toplevel.as_deref().unwrap_or(path);

        let (mut ref_map, head) = load_refs(path);

        let stashes = load_all_stashes(path, mailmap);
        let commits = load_all_commits(path, sort, &head, &stashes, max_count, mailmap);
        if commits.is_empty() {
            return Err("no commits in the repository".into());
        }

        let commits = merge_stashes_to_commits(commits, stashes);
        let commit_hashes = commits.iter().map(|c| c.commit_hash.clone()).collect();

        let (parents_map, children_map) = build_commits_maps(&commits);
        let commit_map = to_commit_map(commits);

        let stash_ref_map = load_stashes_as_refs(path);
        merge_ref_maps(&mut ref_map, stash_ref_map);

        let mut repository = Self::new(
            path.to_path_buf(),
            commit_map,
            parents_map,
            children_map,
            ref_map,
            head,
            commit_hashes,
        );
        repository.git_dir = git_dir;
        repository.loaded_local_changes = Some(load_local_changes(path)).into();
        Ok(repository)
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
            commit_map,
            parents_map,
            children_map,
            ref_map,
            head,
            commit_hashes,
        }
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
        let mut args = vec![
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "-M",
            base,
            target,
            "--",
        ];
        args.extend_from_slice(paths);
        git_output(&self.path, &args).unwrap_or_default()
    }

    // The files changed from one commit to another.
    pub fn diff_summary_between(&self, base: &CommitHash, target: &CommitHash) -> Vec<FileChange> {
        let args = [
            "diff",
            "--name-status",
            "-M",
            base.as_str(),
            target.as_str(),
        ];
        git_output(&self.path, &args)
            .unwrap_or_default()
            .lines()
            .filter_map(parse_name_status_line)
            .collect()
    }

    // The commit checked out, None in a repository without commits.
    pub fn head_commit(&self) -> Option<CommitHash> {
        git_output(&self.path, &["rev-parse", "--verify", "--quiet", "HEAD"])
            .ok()
            .map(|hash| CommitHash::from(hash.trim()))
    }

    // Where the commit is in the commit list, newer commits first.
    pub fn commit_position(&self, commit_hash: &CommitHash) -> Option<usize> {
        self.commit_hashes.iter().position(|h| h == commit_hash)
    }

    pub fn is_ancestor(&self, ancestor: &CommitHash, descendant: &str) -> bool {
        let args = ["merge-base", "--is-ancestor", ancestor.as_str(), descendant];
        git_output(&self.path, &args).is_ok()
    }

    // Whether there are merge commits after `base` up to `head`.
    // The parents of a commit, asked of git as they may not be loaded, with `--max-count`.
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

    pub fn has_merges_between(&self, base: Option<&CommitHash>, head: &str) -> bool {
        let range = match base {
            Some(base) => format!("{}..{head}", base.as_str()),
            None => head.to_string(),
        };
        git_output(&self.path, &["rev-list", "--merges", "-n", "1", &range])
            .is_ok_and(|out| !out.trim().is_empty())
    }

    // The contents of a file at a commit.
    pub fn commit_file_content(
        &self,
        commit_hash: &CommitHash,
        path: &str,
    ) -> std::result::Result<Vec<u8>, String> {
        let object = format!("{}:{path}", commit_hash.as_str());
        let output = Command::new("git")
            .args(["cat-file", "blob", &object])
            .current_dir(&self.path)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("failed to run git cat-file: {e}"))?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!("git cat-file failed: {}", stderr.trim()))
        }
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
        args.extend_from_slice(paths);
        run_git(&self.path, &args, None)
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
        args.extend_from_slice(paths);
        run_git(&self.path, &args, None)
    }

    // Discards the unstaged changes of `tracked` files (restoring them from the index) and deletes `untracked` files.
    pub fn discard_paths(
        &self,
        tracked: &[&str],
        untracked: &[&str],
    ) -> std::result::Result<(), String> {
        if !tracked.is_empty() {
            let mut args = vec!["checkout", "--quiet", "--"];
            args.extend_from_slice(tracked);
            run_git(&self.path, &args, None)?;
        }
        if !untracked.is_empty() {
            let mut args = vec!["clean", "--force", "--quiet", "--"];
            args.extend_from_slice(untracked);
            run_git(&self.path, &args, None)?;
        }
        Ok(())
    }

    // Applies `patch` to the working tree, `reverse` to undo the changes in it.
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
        run_git(&self.path, &args, Some(patch))
    }

    // Stashes the changes of `paths` (all changes when empty), including untracked files.
    pub fn stash_push(&self, paths: &[&str], message: &str) -> std::result::Result<(), String> {
        let mut args = vec!["stash", "push", "--include-untracked", "--quiet"];
        if !message.is_empty() {
            args.extend(["--message", message]);
        }
        if !paths.is_empty() {
            args.push("--");
            args.extend_from_slice(paths);
        }
        run_git(&self.path, &args, None)
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

    // The top directory of the working tree, which file paths of local changes are relative to.
    pub fn toplevel(&self) -> PathBuf {
        self.path.clone()
    }

    // The remotes, local branches and their upstreams, to fetch, pull and push with.
    pub fn remote_info(&self) -> RemoteInfo {
        let remotes: Vec<String> = git_output(&self.path, &["remote"])
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect();

        let mut remote_branches: FxHashMap<String, Vec<String>> = FxHashMap::default();
        let refs = git_output(
            &self.path,
            &["for-each-ref", "--format=%(refname)", "refs/remotes"],
        )
        .unwrap_or_default();
        for name in refs.lines() {
            let name = name.trim_start_matches("refs/remotes/");
            // remote names may contain slashes, the longest matching one owns the branch
            let remote = remotes
                .iter()
                .filter(|remote| name.starts_with(&format!("{remote}/")))
                .max_by_key(|remote| remote.len());
            if let Some(remote) = remote {
                let branch = &name[remote.len() + 1..];
                if branch != "HEAD" {
                    remote_branches
                        .entry(remote.clone())
                        .or_default()
                        .push(branch.to_string());
                }
            }
        }

        let format = "--format=%(refname:short)%00%(upstream:remotename)%00%(upstream:remoteref)";
        let heads =
            git_output(&self.path, &["for-each-ref", format, "refs/heads"]).unwrap_or_default();
        let mut branches = vec![];
        let mut upstreams = FxHashMap::default();
        for line in heads.lines() {
            let parts: Vec<&str> = line.split('\0').collect();
            let [name, remote, remote_ref] = parts[..] else {
                continue;
            };
            branches.push(name.to_string());
            if let Some(branch) = remote_ref.strip_prefix("refs/heads/") {
                if !remote.is_empty() {
                    upstreams.insert(name.to_string(), (remote.to_string(), branch.to_string()));
                }
            }
        }

        // pull.rebase may also be "merges" or "interactive", which rebase too
        let pull_rebase = git_output(&self.path, &["config", "--get", "pull.rebase"])
            .is_ok_and(|v| !matches!(v.trim(), "false" | "no" | "off" | "0" | ""));

        RemoteInfo {
            current_branch: match &self.head {
                Head::Branch { name } => Some(name.clone()),
                _ => None,
            },
            remotes,
            remote_branches,
            branches,
            upstreams,
            pull_rebase,
            rebase_auto_stash: self.config_bool("rebase.autoStash").unwrap_or(false),
            merge_auto_stash: self.config_bool("merge.autoStash").unwrap_or(false),
        }
    }

    // A git config value of the repository, None when it is not set.
    pub fn config(&self, key: &str) -> Option<String> {
        git_output(&self.path, &["config", "--get", key])
            .ok()
            .map(|v| v.trim().to_string())
    }

    // A boolean git config value, None when it is not set (or not a boolean).
    pub fn config_bool(&self, key: &str) -> Option<bool> {
        match git_output(&self.path, &["config", "--type=bool", "--get", key]) {
            Ok(v) if v.trim() == "true" => Some(true),
            Ok(v) if v.trim() == "false" => Some(false),
            _ => None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    // The content of a conflicted file on one side, None when that side deleted it.
    pub fn conflict_side_content(&self, path: &str, side: ConflictSide) -> Option<String> {
        let stage = match side {
            ConflictSide::Ours => 2,
            ConflictSide::Theirs => 3,
        };
        self.stage_blob(path, stage)
            .map(|blob| String::from_utf8_lossy(&blob).into_owned())
    }

    // The content of a conflicted file in an index stage: 1 the base, 2 ours, 3 theirs.
    fn stage_blob(&self, path: &str, stage: u8) -> Option<Vec<u8>> {
        let object = format!(":{stage}:{path}");
        let output = Command::new("git")
            .args(["cat-file", "blob", &object])
            .current_dir(&self.path)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        output.status.success().then_some(output.stdout)
    }

    // Resolves a conflicted file by taking one side where the two conflict, keeping the changes
    // of the other side that merged cleanly, and removing the file when that side deleted it.
    pub fn resolve_conflict(
        &self,
        path: &str,
        side: ConflictSide,
    ) -> std::result::Result<(), String> {
        if self.conflict_side_content(path, side).is_none() {
            return run_git(&self.path, &["rm", "--quiet", "--", path], None);
        }
        match self.merge_file_favoring(path, side) {
            Some(merged) => std::fs::write(self.path.join(path), merged)
                .map_err(|e| format!("failed to write {path}: {e}"))?,
            // a binary file can't be merged by lines, so it is taken whole
            None => {
                let option = match side {
                    ConflictSide::Ours => "--ours",
                    ConflictSide::Theirs => "--theirs",
                };
                run_git(&self.path, &["checkout", option, "--", path], None)?;
            }
        }
        run_git(&self.path, &["add", "--", path], None)
    }

    // Merges the two sides of a conflicted file again, resolving the conflicts in favor of `side`.
    fn merge_file_favoring(&self, path: &str, side: ConflictSide) -> Option<Vec<u8>> {
        let ours = self.stage_blob(path, 2)?;
        let theirs = self.stage_blob(path, 3)?;
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
        let option = match side {
            ConflictSide::Ours => "--ours",
            ConflictSide::Theirs => "--theirs",
        };
        let output = Command::new("git")
            .args(["merge-file", "-p", option, &paths[0], &paths[1], &paths[2]])
            .current_dir(&self.path)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        for (file, _) in &files {
            if let Some(file) = file {
                let _ = std::fs::remove_file(file);
            }
        }
        // fails (with a negative code) on binary files
        let output = output.ok()?;
        output.status.success().then_some(output.stdout)
    }

    // The commit message git prepared for the operation in progress, without its comment lines.
    pub fn prepared_message(&self) -> Option<String> {
        self.message_file("MERGE_MSG")
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

    // Continues a rebase, or the remaining commits of a cherry-pick or revert of several commits.
    pub fn continue_operation(&self, operation: Operation) -> std::result::Result<(), String> {
        if operation == Operation::Merge {
            return Ok(());
        }
        let command = operation.name();
        // a single cherry-pick or revert is done once committed, only a sequence goes on
        if operation != Operation::Rebase && !self.git_path("sequencer").is_some_and(|p| p.exists())
        {
            return Ok(());
        }
        let output = Command::new("git")
            .args([command, "--continue"])
            // keep the messages of the commits instead of opening an editor
            .env("GIT_EDITOR", "true")
            .current_dir(&self.path)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("failed to run git {command} --continue: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            // git explains conflicts left to resolve on stdout
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let reason = [stderr.trim(), stdout.trim()]
                .into_iter()
                .find(|s| !s.is_empty())
                .unwrap_or_default()
                .to_string();
            Err(format!("git {command} --continue failed: {reason}"))
        }
    }

    pub fn abort_operation(&self, operation: Operation) -> std::result::Result<(), String> {
        run_git(&self.path, &[operation.name(), "--abort"], None)
    }

    fn git_path(&self, name: &str) -> Option<PathBuf> {
        if let Some(git_dir) = &self.git_dir {
            return Some(git_dir.join(name));
        }
        git_output(&self.path, &["rev-parse", "--git-path", name])
            .map(|p| self.path.join(p.trim()))
            .ok()
    }

    pub fn commit_staged(&self, message: &str, amend: bool) -> std::result::Result<(), String> {
        // the message is read from stdin so no editor is opened
        let mut args = vec!["commit", "--quiet", "--file=-"];
        if amend {
            args.push("--amend");
        }
        run_git(&self.path, &args, Some(message))
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
        run_git(&self.path, &args, Some(patch))
    }
}

fn check_git_repository(path: &Path) -> Result<()> {
    if !is_inside_work_tree(path) && !is_bare_repository(path) {
        let msg = "not a git repository (or any of the parent directories)";
        return Err(msg.into());
    }
    Ok(())
}

// The top directory of the working tree (None when bare) and the git directory of the repository at `path`.
fn locate_repository(path: &Path) -> (Option<PathBuf>, Option<PathBuf>) {
    let git_dir = git_output(path, &["rev-parse", "--absolute-git-dir"])
        .ok()
        .map(|p| PathBuf::from(p.trim()));
    let toplevel = is_inside_work_tree(path)
        .then(|| git_output(path, &["rev-parse", "--show-toplevel"]).ok())
        .flatten()
        .map(|p| PathBuf::from(p.trim()));
    (toplevel, git_dir)
}

fn is_inside_work_tree(path: &Path) -> bool {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .current_dir(path)
        .output()
        .unwrap();
    output.status.success() && output.stdout == b"true\n"
}

fn is_bare_repository(path: &Path) -> bool {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--is-bare-repository")
        .current_dir(path)
        .output()
        .unwrap();
    output.status.success() && output.stdout == b"true\n"
}

fn load_all_commits(
    path: &Path,
    sort: SortCommit,
    head: &Head,
    stashes: &[Commit],
    max_count: Option<usize>,
    mailmap: bool,
) -> Vec<Commit> {
    let mut cmd = Command::new("git");
    cmd.arg("log");

    cmd.arg(match sort {
        SortCommit::Chronological => "--date-order",
        SortCommit::Topological => "--topo-order",
    })
    .arg(format!("--pretty={}", load_commits_format(mailmap)))
    .arg("--date=iso-strict")
    .arg("-z"); // use NUL as a delimiter

    // exclude stashes and other refs
    cmd.arg("--branches").arg("--remotes").arg("--tags");

    // commits that are reachable from the stashes
    stashes.iter().for_each(|stash| {
        cmd.arg(stash.parent_commit_hashes[0].as_str());
    });

    if !matches!(head, Head::None) {
        cmd.arg("HEAD");
    }

    if let Some(n) = max_count {
        cmd.arg("--max-count").arg(n.to_string());
    }

    cmd.current_dir(path).stdout(Stdio::piped());

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
    let mut cmd = Command::new("git")
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
    let mut cmd = Command::new("git")
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
    let mut cmd = Command::new("git")
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

fn get_current_branch(path: &Path) -> Option<String> {
    let mut cmd = Command::new("git")
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

#[derive(Debug)]
pub enum FileChange {
    Add { path: String },
    Modify { path: String },
    Delete { path: String },
    Move { from: String, to: String },
}

pub fn get_diff_summary(path: &Path, commit_hash: &CommitHash) -> Vec<FileChange> {
    let mut cmd = Command::new("git")
        .arg("diff")
        .arg("--name-status")
        .arg(format!("{}^", commit_hash.0))
        .arg(&commit_hash.0)
        .current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = cmd.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let mut changes = Vec::new();

    for line in reader.lines() {
        changes.extend(parse_name_status_line(&line.unwrap()));
    }

    cmd.wait().unwrap();

    changes
}

// A line of `git diff --name-status`, like "M\tsrc/a.rs" or "R087\told.rs\tnew.rs".
fn parse_name_status_line(line: &str) -> Option<FileChange> {
    let parts: Vec<&str> = line.split('\t').collect();
    match (parts.first()?.get(..1)?, parts.len()) {
        ("A", 2) => Some(FileChange::Add {
            path: parts[1].into(),
        }),
        ("M" | "T", 2) => Some(FileChange::Modify {
            path: parts[1].into(),
        }),
        ("D", 2) => Some(FileChange::Delete {
            path: parts[1].into(),
        }),
        ("R" | "C", 3) => Some(FileChange::Move {
            from: parts[1].into(),
            to: parts[2].into(),
        }),
        _ => None,
    }
}

pub fn get_initial_commit_additions(path: &Path, commit_hash: &CommitHash) -> Vec<FileChange> {
    let mut cmd = Command::new("git")
        .arg("ls-tree")
        .arg("--name-status")
        .arg("-r") // the empty tree hash
        .arg(&commit_hash.0)
        .current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = cmd.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let mut changes = Vec::new();

    for line in reader.lines() {
        let line = line.unwrap();
        changes.push(FileChange::Add { path: line });
    }

    cmd.wait().unwrap();

    changes
}

// The two versions of a conflicted file: what was there (ours) and what is coming in (theirs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictSide {
    Ours,
    Theirs,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
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
    let output = Command::new("git")
        .arg("status")
        .arg("--porcelain=v1")
        .arg("--untracked-files=all")
        .arg("-z")
        .current_dir(path)
        .stderr(Stdio::null())
        .output();
    match output {
        // fails in bare repositories, which have no working tree
        Ok(output) if output.status.success() => parse_status_porcelain(&output.stdout),
        _ => LocalChanges::default(),
    }
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

fn load_local_change_diff(
    path: &Path,
    targets: &[&str],
    untracked: bool,
    staged: bool,
    staged_base: Option<&str>,
) -> String {
    let mut cmd = Command::new("git");
    // the output is applied back as a patch, so ignore user config that changes its format
    cmd.arg("diff")
        .arg("--no-color")
        .arg("--no-ext-diff")
        .arg("--src-prefix=a/")
        .arg("--dst-prefix=b/");
    if staged {
        cmd.arg("--cached")
            .args(staged_base)
            .arg("--")
            .args(targets);
    } else if untracked {
        cmd.arg("--no-index")
            .arg("--")
            .arg("/dev/null")
            .args(targets);
    } else {
        cmd.arg("--").args(targets);
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

// Git commands run in the background, like fetch, merge or checkout, and what to say about them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitTask {
    // run one after another, stopping at the first that fails
    pub steps: Vec<Vec<String>>,
    // stash the local changes before the steps, and reapply them after
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
}

impl GitTask {
    pub fn new(args: &[&str], running: String, done: String) -> Self {
        GitTask {
            steps: vec![args.iter().map(|a| a.to_string()).collect()],
            running,
            done,
            ..Default::default()
        }
    }
}

// Runs the steps of a task and returns the last line it printed.
pub fn run_task(path: &Path, task: &GitTask) -> std::result::Result<String, String> {
    let stashed = task.stash
        && !git_output(path, &["status", "--porcelain"])
            .unwrap_or_default()
            .trim()
            .is_empty();
    if stashed {
        let message = format!("serie: {}", task.running);
        let args = ["stash", "push", "--include-untracked", "-m", &message].map(String::from);
        run_task_step(path, &args, task)?;
    }
    let pop = || run_task_step(path, &["stash", "pop"].map(String::from), task);

    let mut last = String::new();
    for step in &task.steps {
        match run_task_step(path, step, task) {
            Ok(output) => last = output,
            Err(err) if stashed => {
                // nothing changed, the local changes go back
                return Err(match pop() {
                    Ok(_) => err,
                    Err(_) => format!("{err} (the local changes are kept in the stash)"),
                });
            }
            Err(err) => return Err(err),
        }
    }
    if stashed {
        pop().map_err(|err| {
            format!(
                "{}, but reapplying the local changes failed (they are kept in the stash): {err}",
                task.done
            )
        })?;
    }
    Ok(last)
}

// It runs while the app owns the terminal, so git must not prompt: no editor (the messages git
// prepares are used as they are), and credentials come from a helper or an agent.
fn run_task_step(
    path: &Path,
    args: &[String],
    task: &GitTask,
) -> std::result::Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(path)
        .stdin(Stdio::null())
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
    let output = cmd
        .output()
        .map_err(|e| format!("failed to run git {}: {e}", args.join(" ")))?;
    // git reports progress and results of many commands on stderr
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let last_line = |text: &str| {
        text.lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .unwrap_or_default()
            .to_string()
    };
    if output.status.success() {
        Ok(last_line(&text))
    } else {
        // a rejected ref or a conflict says what went wrong best, then the error lines,
        // and the hints after them how to fix it
        let lines = || text.lines().map(str::trim);
        let error = lines()
            .find(|line| line.starts_with("! ") || line.starts_with("CONFLICT"))
            .or_else(|| {
                lines().find(|line| line.starts_with("error:") || line.starts_with("fatal:"))
            })
            .map(String::from)
            .unwrap_or_else(|| last_line(&text));
        Err(format!("git {} failed: {error}", args[0]))
    }
}

// The empty tree, as the base of a root commit.
fn empty_tree(path: &Path) -> std::result::Result<String, String> {
    git_output(path, &["hash-object", "-t", "tree", "/dev/null"]).map(|s| s.trim().to_string())
}

fn git_output(path: &Path, args: &[&str]) -> std::result::Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to run git {}: {e}", args.join(" ")))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("git {} failed: {}", args[0], stderr.trim()))
    }
}

fn run_git(path: &Path, args: &[&str], stdin: Option<&str>) -> std::result::Result<(), String> {
    let command_error = |e: std::io::Error| format!("failed to run git {}: {e}", args.join(" "));
    let mut child = Command::new("git")
        .args(args)
        .current_dir(path)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::null())
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
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("git {} failed: {}", args[0], stderr.trim()))
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
        repository
            .resolve_conflict("f", ConflictSide::Ours)
            .unwrap();
        // the conflict goes our way, the clean change of the incoming side stays
        let content = std::fs::read_to_string(path.join("f")).unwrap();
        assert_eq!(content, lines("ONE", "NINE-ours"));
        assert!(repository.local_changes().unstaged.is_empty());
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
