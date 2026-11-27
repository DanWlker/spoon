// Loading a repository: its commits, refs and stashes from git, parsed, with what loads
// alongside them.

use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Mutex,
    time::Instant,
};

use jiff::{fmt::temporal::Pieces, tz::TimeZone, ZonedDateTime};
use rustc_hash::FxHashMap;

use crate::{
    background::{joined, Cancel},
    error::Result,
};

use super::{
    git_command, git_output, load_submodules, Commit, CommitHash, CommitsMap, Head, Operation, Ref,
    RefMap, Repository, SortCommit, TrackState, Tracking, WorkTree,
};
use crate::git::{diff::empty_tree, text_from_git, FilesSnapshot, IndexRefresh, LoadedFiles, Seen};

// Run once the commits are listed while a repository loads on this thread, for a test to change
// the repository then.
#[cfg(test)]
thread_local! {
    static AFTER_LISTED: std::cell::Cell<Option<Box<dyn FnOnce()>>> = Default::default();
}

impl Repository {
    #[cfg(test)]
    pub fn load(
        path: &Path,
        sort: SortCommit,
        max_count: Option<usize>,
        mailmap: bool,
    ) -> Result<Self> {
        let cancel = Cancel::default();
        Self::load_scoped(path, sort, max_count, mailmap, None, &cancel)
    }

    // `revisions` loads only their history, like ["HEAD"] for the current branch; else that of
    // every branch, tag and stash. The commits stop loading once `cancel` says so, see
    // `load_all_commits`.
    pub fn load_scoped(
        path: &Path,
        sort: SortCommit,
        max_count: Option<usize>,
        mailmap: bool,
        revisions: Option<&[String]>,
        cancel: &Cancel,
    ) -> Result<Self> {
        let (toplevel, git_dir) = locate_repository(path)?;
        let path = toplevel.as_deref().unwrap_or(path);
        // the refs as of before they load, so the check after sees one changed meanwhile, see
        // `App::check_refs`
        let refs_snapshot = refs_snapshot(path, &git_dir).ok();
        // what doesn't depend on the commits loads alongside them
        let local_changes_started = Instant::now();
        std::thread::scope(|scope| {
            let work_tree = WorkTree {
                path: path.to_path_buf(),
                git_dir: git_dir.clone(),
            };
            let local_changes = scope.spawn(move || work_tree.load_files(IndexRefresh::Skipped));
            let remotes = scope.spawn(|| load_remotes(path));
            let textconv_drivers = scope.spawn(|| load_textconv_drivers(path));
            let operation = scope.spawn(|| read_operation(&git_dir));
            let tracking = scope.spawn(|| load_branch_tracking(path));
            let submodules = scope.spawn(|| load_submodules(path));
            let empty_tree = scope.spawn(|| empty_tree(path).ok());
            let mut repository = Self::load_commits(
                path,
                git_dir.clone(),
                sort,
                max_count,
                mailmap,
                revisions,
                cancel,
            )?;
            #[cfg(test)]
            AFTER_LISTED.with(|hook| hook.take().map(|hook| hook()));
            repository.remotes = joined(remotes);
            repository.textconv_drivers = joined(textconv_drivers);
            repository.loaded_local_changes = Mutex::new(LoadedFiles {
                files: joined(local_changes),
                seen: Seen::First,
            });
            repository.local_changes_started = Mutex::new(local_changes_started);
            repository.refs_snapshot = refs_snapshot;
            repository.operation = joined(operation);
            repository.tracking = joined(tracking);
            repository.submodules = joined(submodules);
            repository.empty_tree = joined(empty_tree);
            // walked here, off the thread drawing, which needs it for the first frame's keys
            repository.head_history();
            Ok(repository)
        })
    }

    // A repository loaded for its commit graph only: a submodule's, shown beside the parent's.
    // Unlike `load_scoped` it skips the status, remotes, tracking, textconv drivers, submodules
    // and stashes, none of which the panel needs, cutting the git commands run when it is picked.
    // None when the revisions have no commits.
    pub fn load_graph(path: &Path, revisions: &[String], cancel: &Cancel) -> Result<Option<Self>> {
        let (toplevel, git_dir) = locate_repository(path)?;
        let path = toplevel.as_deref().unwrap_or(path);
        let (ref_map, head) = load_refs(path);
        let commits = load_all_commits(
            path,
            SortCommit::Chronological,
            Listed::Revisions(revisions),
            None,
            false,
            cancel,
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
        cancel: &Cancel,
    ) -> Result<Self> {
        let (mut ref_map, head) = load_refs(path);

        let (stashes, stash_ref_map) = load_stashes(path, mailmap);
        // a graph of some refs leaves the stashes out, they stay in the sidebar
        let stashes = if revisions.is_some() { vec![] } else { stashes };
        let listed = match revisions {
            Some(revisions) => Listed::Revisions(revisions),
            None => Listed::All {
                head: &head,
                stashes: &stashes,
            },
        };
        let commits = load_all_commits(path, sort, listed, max_count, mailmap, cancel)?;
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
        let (parents_map, children_map) = build_commits_maps(&commits);
        let commit_indices = commits
            .iter()
            .enumerate()
            .map(|(index, commit)| (commit.commit_hash.clone(), index))
            .collect();
        Self {
            path,
            git_dir,
            // none for a repository loaded for its graph only, see `load_graph`
            loaded_local_changes: Mutex::new(LoadedFiles {
                files: FilesSnapshot::of(Default::default(), 0, Instant::now()),
                seen: Seen::First,
            }),
            local_changes_started: Mutex::new(Instant::now()),
            refs_snapshot: None,
            operation: None,
            tracking: Default::default(),
            submodules: Default::default(),
            submodule_repositories: Default::default(),
            commits,
            commit_indices,
            parents_map,
            children_map,
            ref_map,
            head,
            remotes: Vec::new(),
            textconv_drivers: Vec::new(),
            head_history: Default::default(),
            empty_tree: None,
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
    // the branch the graph keeps in the first column, laid out with it, see `graph::GraphLayout::calc`
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
        cancel: &Cancel,
    ) -> std::result::Result<Loaded, String> {
        let load = |revisions| {
            Repository::load_scoped(
                &self.path,
                self.sort,
                self.max_count,
                self.mailmap,
                revisions,
                cancel,
            )
            .map_err(|e| e.to_string())
        };
        match load(revisions) {
            Err(_) if revisions.is_some() && !cancel.is_cancelled() => {
                load(None).map(Loaded::RefsGone)
            }
            loaded => loaded.map(Loaded::Repository),
        }
    }
}

// The top directory of the working tree (None when bare) and the git directory of the repository
// at `path`, which must be in a working tree or a bare repository.
fn locate_repository(path: &Path) -> Result<(Option<PathBuf>, PathBuf)> {
    let not_a_repository = || {
        format!(
            "not a git repository (or any of the parent directories): {}",
            path.display()
        )
        .into()
    };
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
        // git itself missing, not the repository
        .map_err(|e| format!("failed to run git: {e}"))?;
    // the paths as bytes, which may not be UTF-8
    use std::os::unix::ffi::OsStrExt;
    let path_of = |line: &[u8]| PathBuf::from(std::ffi::OsStr::from_bytes(line));
    let mut lines = output.stdout.split(|&b| b == b'\n');
    let mut flag = || lines.next() == Some(b"true".as_slice());
    let (inside_work_tree, bare) = (flag(), flag());
    let git_dir = match (inside_work_tree || bare, lines.next()) {
        (true, Some(git_dir)) if !git_dir.is_empty() => path_of(git_dir),
        _ => return Err(not_a_repository()),
    };
    let toplevel = lines
        .next()
        .filter(|line| inside_work_tree && !line.is_empty())
        .map(path_of);
    Ok((toplevel, git_dir))
}

// Which commits `load_all_commits` lists: those of every ref, with the stashes' and a detached
// HEAD's, or those of some revisions only.
enum Listed<'a> {
    All {
        head: &'a Head,
        stashes: &'a [Commit],
    },
    Revisions(&'a [String]),
}

fn load_all_commits(
    path: &Path,
    sort: SortCommit,
    listed: Listed,
    max_count: Option<usize>,
    mailmap: bool,
    cancel: &Cancel,
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

    match listed {
        Listed::Revisions(revisions) => {
            cmd.args(revisions);
        }
        Listed::All { head, stashes } => {
            // exclude stashes and other refs
            cmd.arg("--branches").arg("--remotes").arg("--tags");

            // commits that are reachable from the stashes
            for parent in stashes
                .iter()
                .filter_map(|s| s.parent_commit_hashes.first())
            {
                cmd.arg(parent.as_str());
            }

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
    // read meanwhile: git waits once the pipe is full, like with a trace of every ref
    // (GIT_TRACE_REFS) of a big repository, while the commits wait to be read
    let stderr = super::task::read_pipe(process.stderr.take());

    let reader = BufReader::new(stdout);

    let mut commits = Vec::new();
    let mut fields = Vec::with_capacity(COMMIT_FIELDS);

    // each field ends with a NUL, which git keeps out of them, see `load_commits_format`
    for bytes in reader.split(b'\0') {
        // stopped once the job loading them is dropped, see `Job::read`; it only reads
        if cancel.is_cancelled() {
            let _ = process.kill();
            let _ = process.wait();
            return Err("git log stopped".into());
        }
        let bytes = bytes.map_err(|e| format!("failed to read git log: {e}"))?;
        fields.push(text_from_git(&bytes));
        if fields.len() == COMMIT_FIELDS {
            commits.push(parse_commit(&fields));
            fields.clear();
        }
    }

    let status = process
        .wait()
        .map_err(|e| format!("failed to run git log: {e}"))?;
    if !status.success() {
        let stderr = stderr.join().unwrap_or_default();
        let stderr = String::from_utf8_lossy(&stderr);
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
pub(super) fn parse_iso_date(s: &str) -> ZonedDateTime {
    let parse = || {
        let pieces = Pieces::parse(s).ok()?;
        let offset = pieces.to_numeric_offset()?;
        pieces
            .date()
            .to_datetime(pieces.time()?)
            .to_zoned_datetime(TimeZone::fixed(offset))
            .ok()
    };
    parse().unwrap_or_default()
}

fn parse_parent_commit_hashes(s: &str) -> Vec<CommitHash> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split(' ').map(|s| s.into()).collect()
}

fn build_commits_maps(commits: &[Commit]) -> (CommitsMap, CommitsMap) {
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

fn merge_stashes_to_commits(commits: Vec<Commit>, stashes: Vec<Commit>) -> Vec<Commit> {
    // Stash commit has multiple parent commits, but the first parent commit is the commit that the stash was created from.
    // If the first parent commit is not found, or there is none, the stash commit is ignored.
    let mut ret = Vec::new();
    let mut stash_map: FxHashMap<CommitHash, Vec<Commit>> = FxHashMap::default();
    for stash in stashes {
        if let Some(parent) = stash.parent_commit_hashes.first() {
            stash_map.entry(parent.clone()).or_default().push(stash);
        }
    }
    for commit in commits {
        if let Some(stashes) = stash_map.remove(&commit.commit_hash) {
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
        // "<hash> <ref>": git refuses to list a ref named with a space
        let Some((hash, refs)) = line.split_once(' ') else {
            continue;
        };

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

    if let (Head::None, Some(name)) = (&head, get_current_branch(path)) {
        head = Head::Unborn { name };
    }
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

// How far each local branch with an upstream is ahead and behind it; one in sync at (0, 0).
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
            let state = match track {
                "gone" => TrackState::Gone,
                _ => TrackState::Counts {
                    ahead: count("ahead "),
                    behind: count("behind "),
                },
            };
            let tracking = Tracking {
                upstream: upstream.to_string(),
                state,
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
    use rstest::rstest;

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

    // A graph loaded for a job dropped meanwhile, like a submodule's commits once the cursor
    // moved on, stops reading git log; one kept loads.
    #[test]
    fn test_load_graph_stops_once_cancelled() {
        use crate::test_support::git::GitRepository;

        let dir = tempfile::tempdir().unwrap();
        let repo = GitRepository::new(dir.path());
        repo.init();
        repo.commit("one", "2024-01-01");
        let revisions = ["HEAD".to_string()];
        let loaded = Repository::load_graph(dir.path(), &revisions, &Cancel::default());
        assert_eq!(loaded.unwrap().unwrap().all_commits().len(), 1);
        let stopped = Repository::load_graph(dir.path(), &revisions, &Cancel::cancelled());
        assert!(stopped.is_err());
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

    // A repository in a folder whose name isn't UTF-8 couldn't be opened: its paths were read
    // with the bytes replaced.
    #[test]
    fn test_load_a_repository_in_a_folder_not_in_utf8() {
        use crate::test_support::git::GitRepository;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(std::ffi::OsStr::from_bytes(b"caf\xe9"));
        std::fs::create_dir(&path).unwrap();
        let repo = GitRepository::new(&path);
        repo.init();
        std::fs::write(path.join("a.txt"), "a\n").unwrap();
        repo.add(&["a.txt"]);
        repo.commit("first", "2024-01-01");
        let repository = Repository::load(&path, SortCommit::Chronological, None, false).unwrap();
        assert!(repository.head_commit().is_some());
    }

    // A branch whose upstream is gone isn't in sync with it: it was taken for one, at 0 and 0.
    #[test]
    fn test_branch_with_its_upstream_gone() {
        use crate::test_support::git::{git, GitRepository};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        repo.commit("first", "2024-01-01");
        git(path, &["update-ref", "refs/remotes/origin/x", "HEAD"]);
        git(path, &["config", "remote.origin.url", "/nowhere"]);
        let fetch = "+refs/heads/*:refs/remotes/origin/*";
        git(path, &["config", "remote.origin.fetch", fetch]);
        git(path, &["config", "branch.master.remote", "origin"]);
        git(path, &["config", "branch.master.merge", "refs/heads/x"]);
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let state = |r: &Repository| r.branch_tracking()["master"].state;
        assert_eq!(
            state(&repository),
            TrackState::Counts {
                ahead: 0,
                behind: 0
            }
        );
        git(path, &["update-ref", "-d", "refs/remotes/origin/x"]);
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        assert_eq!(state(&repository), TrackState::Gone);
    }

    // A stash with no parent, which another tool can leave in refs/stash, stopped the app loading.
    #[test]
    fn test_load_a_stash_without_parent() {
        use crate::test_support::git::{git, GitRepository};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        std::fs::write(path.join("a.txt"), "a\n").unwrap();
        repo.add(&["a.txt"]);
        repo.commit("first", "2024-01-01");
        let root = git(path, &["commit-tree", "-m", "x", "HEAD^{tree}"]);
        git(
            path,
            &["update-ref", "--create-reflog", "refs/stash", root.trim()],
        );
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        assert!(repository.head_commit().is_some());
    }

    #[test]
    fn test_parse_iso_date_of_a_broken_date() {
        // what git prints for a commit whose date or time zone is broken
        for broken in ["%aI", "2023-11-19T02:52:20+99:99"] {
            assert_eq!(parse_iso_date(broken), ZonedDateTime::default());
        }
        let date = parse_iso_date("2023-11-19T02:52:20+08:00");
        assert_eq!(
            date.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string(),
            "2023-11-19T02:52:20+08:00"
        );
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
        let taken = |repository: &Repository| {
            let (files, seen) = repository.take_loaded_local_changes();
            (files.changes.count(), seen)
        };
        assert_eq!(taken(&repository), (1, Seen::First));
        assert_eq!(repository.loaded_local_change_count(), 1);
        // taken again, the page loads them again, see `LocalChangesState::restore`
        assert_eq!(taken(&repository), (1, Seen::Again));
        repository.set_local_changes(crate::git::FilesSnapshot::load(
            path,
            &path.join(".git"),
            IndexRefresh::Skipped,
        ));
        assert_eq!(taken(&repository), (2, Seen::First));
        // a `git status` started before is older than them: dropped
        let older = crate::git::FilesSnapshot::of(Default::default(), 0, before);
        repository.set_local_changes(older);
        assert_eq!(repository.loaded_local_change_count(), 2);
        // one with a conflict is kept, its conflicts resolved by hand staged once the page takes it
        let changes = crate::git::LocalChanges {
            unstaged: vec![crate::git::LocalChangeFile {
                path: "c".into(),
                orig_path: None,
                status: crate::git::LocalChangeStatus::Conflicted,
            }],
            staged: vec![],
        };
        let conflicted = crate::git::FilesSnapshot::of(changes, 0, Instant::now());
        repository.set_local_changes(conflicted);
        assert_eq!(repository.loaded_local_change_count(), 1);
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
            Repository::load_scoped(path, sort, None, false, Some(&head), &Cancel::default())
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

    #[rstest]
    #[case("2024-01-01T01:02:03Z", "2024-01-01T01:02:03Z", "+0000")]
    #[case("2024-01-01T01:02:03+00:00", "2024-01-01T01:02:03Z", "+0000")]
    #[case("2024-01-01T01:02:03+09:30", "2023-12-31T15:32:03Z", "+0930")]
    #[case("2024-01-01T01:02:03-03:30", "2024-01-01T04:32:03Z", "-0330")]
    fn test_parse_iso_date_preserves_offset(
        #[case] input: &str,
        #[case] timestamp: &str,
        #[case] offset: &str,
    ) {
        let date = parse_iso_date(input);

        assert_eq!(date.timestamp().to_string(), timestamp);
        assert_eq!(date.strftime("%Y-%m-%d").to_string(), "2024-01-01");
        assert_eq!(
            date.strftime("%Y-%m-%d %H:%M:%S %z").to_string(),
            format!("2024-01-01 01:02:03 {offset}")
        );
    }

    #[rstest]
    #[case("2024-03-10T06:59:59+00:00", "2024-03-10 01:59:59 -0500")]
    #[case("2024-03-10T07:00:00+00:00", "2024-03-10 03:00:00 -0400")]
    fn test_commit_date_conversion_respects_daylight_saving_time(
        #[case] input: &str,
        #[case] expected: &str,
    ) {
        let time_zone = TimeZone::posix("EST5EDT,M3.2.0,M11.1.0").unwrap();
        let date = parse_iso_date(input).with_time_zone(time_zone);

        assert_eq!(date.strftime("%Y-%m-%d %H:%M:%S %z").to_string(), expected);
    }
}

// A commit made while the repository loads, after its commits are listed, is seen by the next
// check of the refs: the snapshot loaded with it is of before, not of after.
#[cfg(test)]
#[test]
fn test_refs_changed_while_loading_are_seen_by_the_next_check() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let git = crate::test_support::git::GitRepository::new(&path);
    git.init();
    git.commit("one", "2024-01-01");
    let outside = path.clone();
    let commit =
        move || crate::test_support::git::GitRepository::new(&outside).commit("two", "2024-01-02");
    AFTER_LISTED.with(|hook| hook.set(Some(Box::new(commit))));
    let repository = Repository::load(&path, SortCommit::Chronological, None, false).unwrap();
    assert_eq!(repository.all_commits().len(), 1);
    assert_ne!(
        repository.refs_snapshot.as_deref(),
        refs_snapshot(&path, &path.join(".git")).ok().as_deref()
    );
}
