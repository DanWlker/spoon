// Loading a repository: its commits, refs and stashes from git, parsed, with what loads
// alongside them.

use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Mutex,
    time::Instant,
};

use chrono::{DateTime, FixedOffset};
use rustc_hash::FxHashMap;

use crate::{background::joined, Result};

use super::{
    git_command, git_output, load_local_changes, load_submodules, Commit, CommitHash, CommitMap,
    CommitsMap, Head, Operation, Ref, RefMap, Repository, SharedTask, SortCommit, Tracking,
};
use crate::git::text_from_git;

impl Repository {
    #[cfg(test)]
    pub fn load(
        path: &Path,
        sort: SortCommit,
        max_count: Option<usize>,
        mailmap: bool,
    ) -> Result<Self> {
        Self::load_scoped(path, sort, max_count, mailmap, None, Default::default())
    }

    // `revisions` loads only their history, like ["HEAD"] for the current branch; else that of
    // every branch, tag and stash. `running` is the app's, see `RunningTask`.
    pub fn load_scoped(
        path: &Path,
        sort: SortCommit,
        max_count: Option<usize>,
        mailmap: bool,
        revisions: Option<&[String]>,
        running: SharedTask,
    ) -> Result<Self> {
        let (toplevel, git_dir) = locate_repository(path)?;
        let path = toplevel.as_deref().unwrap_or(path);
        // what doesn't depend on the commits loads alongside them
        let local_changes_started = Instant::now();
        std::thread::scope(|scope| {
            let local_changes = scope.spawn(|| load_local_changes(path));
            let remotes = scope.spawn(|| load_remotes(path));
            let textconv_drivers = scope.spawn(|| load_textconv_drivers(path));
            let refs_snapshot = scope.spawn(|| refs_snapshot(path, &git_dir).ok());
            let operation = scope.spawn(|| read_operation(&git_dir));
            let tracking = scope.spawn(|| load_branch_tracking(path));
            let submodules = scope.spawn(|| load_submodules(path));
            let mut repository =
                Self::load_commits(path, git_dir.clone(), sort, max_count, mailmap, revisions)?;
            repository.running = running;
            repository.remotes = joined(remotes);
            repository.textconv_drivers = joined(textconv_drivers);
            repository.loaded_local_changes = Mutex::new(Some(joined(local_changes)));
            repository.local_changes_started = Mutex::new(local_changes_started);
            repository.refs_snapshot = joined(refs_snapshot);
            repository.operation = joined(operation);
            repository.tracking = joined(tracking);
            repository.submodules = joined(submodules);
            Ok(repository)
        })
    }

    // A repository loaded for its commit graph only: a submodule's, shown beside the parent's.
    // Unlike `load_scoped` it skips the status, remotes, tracking, textconv drivers, submodules
    // and stashes, none of which the panel needs, cutting the git commands run when it is picked.
    // None when the revisions have no commits.
    pub fn load_graph(path: &Path, revisions: &[String]) -> Result<Option<Self>> {
        let (toplevel, git_dir) = locate_repository(path)?;
        let path = toplevel.as_deref().unwrap_or(path);
        let (ref_map, head) = load_refs(path);
        let commits = load_all_commits(
            path,
            SortCommit::Chronological,
            &head,
            &[],
            None,
            false,
            Some(revisions),
        )?;
        if commits.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self::new(
            path.to_path_buf(),
            git_dir,
            commits,
            ref_map,
            head,
        )))
    }

    // The commits and refs, see `load_scoped`.
    fn load_commits(
        path: &Path,
        git_dir: PathBuf,
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
        merge_ref_maps(&mut ref_map, stash_ref_map);
        Ok(Self::new(
            path.to_path_buf(),
            git_dir,
            commits,
            ref_map,
            head,
        ))
    }

    // `commits` in the order `git log` gave them.
    pub fn new(
        path: PathBuf,
        git_dir: PathBuf,
        commits: Vec<Commit>,
        ref_map: RefMap,
        head: Head,
    ) -> Self {
        let commit_hashes = commits.iter().map(|c| c.commit_hash.clone()).collect();
        let (parents_map, children_map) = build_commits_maps(&commits);
        let commit_map = to_commit_map(commits);
        Self {
            path,
            git_dir,
            loaded_local_changes: Default::default(),
            local_changes_started: Mutex::new(Instant::now()),
            taken_count: Default::default(),
            running: Default::default(),
            refs_snapshot: None,
            operation: None,
            tracking: Default::default(),
            submodules: Default::default(),
            submodule_repositories: Default::default(),
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
}

// How the app loads the repository, the same on each reload.
#[derive(Debug, Clone)]
pub struct LoadOptions {
    pub path: PathBuf,
    pub sort: SortCommit,
    pub max_count: Option<usize>,
    pub mailmap: bool,
    // the branch the graph keeps in the first column, laid out with it, see `graph::calc_graph`
    pub primary_branch: Option<String>,
}

// A repository loaded, see `LoadOptions::load`.
#[derive(Debug)]
pub enum Loaded {
    // with the history of the revisions asked for
    Repository(Repository),
    // with that of every ref, one of the revisions asked for being gone
    RefsGone(Repository),
}

impl LoadOptions {
    // Loads the history of `revisions`, see `Repository::load_scoped`, else that of every ref when
    // one of them is gone; an error is a message, to send from the thread loading it.
    pub fn load(
        &self,
        revisions: Option<&[String]>,
        running: SharedTask,
    ) -> std::result::Result<Loaded, String> {
        let load = |revisions| {
            Repository::load_scoped(
                &self.path,
                self.sort,
                self.max_count,
                self.mailmap,
                revisions,
                running.clone(),
            )
            .map_err(|e| e.to_string())
        };
        match load(revisions) {
            Err(_) if revisions.is_some() => load(None).map(Loaded::RefsGone),
            loaded => loaded.map(Loaded::Repository),
        }
    }
}

// The top directory of the working tree (None when bare) and the git directory of the repository
// at `path`, which must be in a working tree or a bare repository.
fn locate_repository(path: &Path) -> Result<(Option<PathBuf>, PathBuf)> {
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
    let git_dir = match (inside_work_tree || bare, lines.next()) {
        (true, Some(git_dir)) => PathBuf::from(git_dir),
        _ => return Err(not_a_repository()),
    };
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

            // only a detached HEAD is left out of --branches; on a branch, HEAD would add nothing
            if matches!(head, Head::Detached { .. }) {
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
    let mut fields = Vec::with_capacity(COMMIT_FIELDS);

    // each field ends with a NUL, which git keeps out of them, see `load_commits_format`
    for bytes in reader.split(b'\0') {
        let bytes = bytes.map_err(|e| format!("failed to read git log: {e}"))?;
        fields.push(text_from_git(&bytes));
        if fields.len() == COMMIT_FIELDS {
            commits.push(parse_commit(&fields));
            fields.clear();
        }
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
    let format = format!("{}%x00%gd", load_commits_format(mailmap));
    // the dates are strict ISO in the format: a --date option would name the stashes by date
    let args = ["stash", "list", &format!("--pretty={format}"), "-z"];
    let out = git_output(path, &args).unwrap_or_default();
    let mut commits = Vec::new();
    let mut ref_map = RefMap::default();
    for fields in out
        .split('\0')
        .collect::<Vec<_>>()
        .as_chunks::<{ COMMIT_FIELDS + 1 }>()
        .0
    {
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
fn parse_commit(fields: &[impl AsRef<str>]) -> Commit {
    let field = |i: usize| fields[i].as_ref();
    Commit {
        commit_hash: field(0).into(),
        author_name: field(1).into(),
        author_email: field(2).into(),
        author_date: parse_iso_date(field(3)),
        committer_name: field(4).into(),
        committer_email: field(5).into(),
        committer_date: parse_iso_date(field(6)),
        subject: field(7).into(),
        body: field(8).into(),
        parent_commit_hashes: parse_parent_commit_hashes(field(9)),
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
    // like lazygit: git refuses a NUL in a commit message, so none is in a field, which other
    // control bytes could be
    format.join("%x00")
}

// A broken date of a commit, which git prints as it is or with a time zone like +99:99, is the
// start of 1970, as git shows a date too big.
pub(super) fn parse_iso_date(s: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(s).unwrap_or_default()
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
                Head::Branch {
                    name: branch,
                    target: hash.into(),
                }
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
    if let Some(name) = refs.strip_prefix("refs/heads/") {
        Some(Ref::Branch {
            name: name.into(),
            target: hash.into(),
        })
    } else if let Some(name) = refs.strip_prefix("refs/remotes/") {
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
    if let Some(name) = refs.strip_prefix("refs/tags/") {
        // the commit of an annotated tag, see `load_refs`
        let name = name.strip_suffix("^{}").unwrap_or(name);
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

// The commit checked out now, see `Repository::head_commit` for the one loaded.
pub fn read_head_commit(path: &Path) -> Option<CommitHash> {
    git_output(path, &["rev-parse", "--verify", "--quiet", "HEAD"])
        .ok()
        .map(|hash| CommitHash::from(hash.trim()))
}

// None on a detached HEAD.
fn get_current_branch(path: &Path) -> Option<String> {
    let out = git_output(path, &["branch", "--show-current"]).ok()?;
    Some(out.trim().to_string()).filter(|branch| !branch.is_empty())
}

// How far each local branch is ahead and behind its upstream, those in sync left out.
pub(super) fn load_branch_tracking(path: &Path) -> FxHashMap<String, Tracking> {
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

// The operation stopped in the repository of `git_dir` now, see `Repository::operation_in_progress`
// for the one loaded.
pub fn read_operation(git_dir: &Path) -> Option<Operation> {
    let exists = |name: &str| git_dir.join(name).exists();
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

// A fingerprint of the branches, remote branches, tags, HEAD, the stashes and the operation in
// progress, which changes when they are changed from outside the app, like lazygit's
// `RefsSnapshot`: where each ref points (a fetch in the background moves the remote ones), the
// branch checked out or the commit when detached, every stash, as the sidebar drops them by
// position, and a merge, cherry-pick or revert stopped on conflicts, which moves no ref.
pub fn refs_snapshot(path: &Path, git_dir: &Path) -> std::result::Result<String, String> {
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
    let operation = format!("{:?}", read_operation(git_dir));
    Ok(refs + &head + &stashes + &operation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::FileChange;

    // `log.showSignature` makes git print the check of a signature before the fields of a signed
    // commit, which used to be read as its hash.
    #[test]
    fn test_load_signed_commits_with_show_signature() {
        use crate::test_support::git::{git, GitRepository};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let key = path.join("key");
        let keygen = std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&key)
            .status()
            .unwrap();
        assert!(keygen.success());
        let repo = GitRepository::new(path);
        repo.init();
        git(path, &["config", "gpg.format", "ssh"]);
        git(path, &["config", "user.signingkey", key.to_str().unwrap()]);
        git(path, &["config", "commit.gpgsign", "true"]);
        git(path, &["config", "log.showSignature", "true"]);
        repo.commit("signed", "2024-01-01");
        repo.stash("2024-01-02");

        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let head = repo.rev_parse_head();
        assert_eq!(repository.head_commit().unwrap().as_str(), head.trim());
        for commit in repository.all_commits() {
            let hash = commit.commit_hash.as_str();
            assert!(
                hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
                "{hash:?}"
            );
        }
    }

    // `git branch refs/heads/foo` makes `refs/heads/refs/heads/foo`, which used to be read as the
    // branch `foo`, another one.
    #[test]
    fn test_load_branch_named_like_a_ref() {
        use crate::test_support::git::{git, GitRepository};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        repo.commit("first", "2024-01-01");
        git(path, &["branch", "refs/heads/foo"]);
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let mut names: Vec<String> = repository
            .all_refs()
            .into_iter()
            .filter_map(|r| match r {
                Ref::Branch { name, .. } | Ref::Tag { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();
        names.sort();
        assert_eq!(names, ["master", "refs/heads/foo"]);
    }

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
    fn test_parse_iso_date_of_a_broken_date() {
        // what git prints for a commit whose date or time zone is broken
        for broken in ["%aI", "2023-11-19T02:52:20+99:99"] {
            assert_eq!(parse_iso_date(broken), DateTime::<FixedOffset>::default());
        }
        let date = parse_iso_date("2023-11-19T02:52:20+08:00");
        assert_eq!(date.to_rfc3339(), "2023-11-19T02:52:20+08:00");
    }

    #[test]
    fn test_load_alongside_commits() {
        let before = std::time::Instant::now();
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
        // taken by the page, the count stays as loaded, without running git status again
        std::fs::write(path.join("b"), "b\n").unwrap();
        assert_eq!(repository.take_loaded_local_changes().count(), 1);
        assert_eq!(repository.loaded_local_change_count(), 1);
        // taken again, they are as they are now
        assert_eq!(repository.take_loaded_local_changes().count(), 2);
        // a `git status` started before is older than them: dropped
        let older = crate::git::FilesSnapshot {
            changes: Default::default(),
            stamp: 0,
            started: before,
        };
        repository.set_local_changes(older);
        assert_eq!(repository.loaded_local_change_count(), 2);
        assert_eq!(repository.remotes(), ["origin"]);
        assert_eq!(
            repository.refs_snapshot(),
            Some(refs_snapshot(path, &path.join(".git")).unwrap().as_str())
        );
        assert!(repository.branch_tracking().is_empty());
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
        assert_eq!(git_dir, path.join("wt/.git"));
        // a bare repository has no working tree
        let (toplevel, git_dir) = locate_repository(&path.join("bare.git")).unwrap();
        assert_eq!(toplevel, None);
        assert_eq!(git_dir, path.join("bare.git"));
        assert!(locate_repository(&path).is_err());
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
        let sort = SortCommit::Chronological;
        let head_only =
            Repository::load_scoped(path, sort, None, false, Some(&head), Default::default())
                .unwrap();
        assert_eq!(subjects(&head_only), ["main", "base"]);
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
        let before = refs_snapshot(path, &path.join(".git")).unwrap();
        std::fs::write(path.join("f"), "b\n").unwrap();
        git_output(path, &["stash", "push", "-q"]).unwrap();
        // a stash made outside the app reloads the sidebar, which drops stashes by position
        let one = refs_snapshot(path, &path.join(".git")).unwrap();
        assert_ne!(one, before);
        std::fs::write(path.join("f"), "c\n").unwrap();
        git_output(path, &["stash", "push", "-q"]).unwrap();
        let two = refs_snapshot(path, &path.join(".git")).unwrap();
        // so does dropping an older one, which leaves refs/stash as it is
        git_output(path, &["stash", "drop", "-q", "stash@{1}"]).unwrap();
        assert_ne!(refs_snapshot(path, &path.join(".git")).unwrap(), two);
    }

    #[test]
    fn test_load_commits_with_separators_in_messages() {
        use crate::test_support::git::{git, GitRepository};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        repo.commit("first", "2024-01-01");
        // the bytes git log's fields and records were once split on
        repo.commit("sub\x1fject\x01\n\nbo\x1fdy", "2024-01-02");
        std::fs::write(path.join("f"), "x").unwrap();
        repo.add(&["f"]);
        git(path, &["stash", "push", "-q", "-m", "st\x1fash"]);

        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let head = repository.head_commit().unwrap();
        let commit = repository.commit(&head).unwrap();
        assert_eq!(commit.subject, "sub\x1fject\x01");
        assert_eq!(commit.body, "bo\x1fdy\n");
        assert_eq!(repository.parents_hash(&head).len(), 1);
        let stash = repository
            .all_refs()
            .into_iter()
            .find_map(|r| match r {
                Ref::Stash { message, .. } => Some(message.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(stash, "On master: st\x1fash");
    }
}
