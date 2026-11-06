// The local changes: their status from git, staging, unstaging, discarding and ignoring them.

use std::{
    hash::Hash,
    path::{Path, PathBuf},
    time::Instant,
};

use super::{
    git_bytes, git_output, git_output_bytes, lock, pathspecs, text_from_git, work_path, GitArgs,
    GitCommand, Repository,
};

impl Repository {
    pub fn local_changes(&self) -> LocalChanges {
        self.local_changes_work()()
    }

    // `local_changes` as work to run on another thread.
    pub fn local_changes_work(&self) -> impl FnOnce() -> LocalChanges + Send + 'static {
        let path = self.path.clone();
        move || load_local_changes(&path)
    }

    // The number of local changes, as they were when the repository was loaded.
    pub fn loaded_local_change_count(&self) -> usize {
        let mut loaded = lock(&self.loaded_local_changes);
        let taken = *lock(&self.taken_count);
        match (loaded.as_ref(), taken) {
            (Some(changes), _) => changes.count(),
            (None, Some(count)) => count,
            (None, None) => {
                *lock(&self.local_changes_started) = Instant::now();
                loaded.insert(self.local_changes()).count()
            }
        }
    }

    // The local changes as they were when the repository was loaded the first time it is called,
    // so the sidebar and the page share one `git status`, and as they are now after that.
    // The local changes loaded again after the working tree changed, for the local changes page to
    // show them; dropped when they started loading before those it has, which a reload after a
    // command of the app can load meanwhile.
    pub fn set_local_changes(&self, files: FilesSnapshot) {
        {
            let mut started = lock(&self.local_changes_started);
            if files.started < *started {
                return;
            }
            *started = files.started;
        }
        *lock(&self.loaded_local_changes) = Some(files.changes);
    }

    // See `files_stamp`.
    pub fn files_stamp(&self, changes: &LocalChanges) -> u64 {
        files_stamp(&self.path, &self.git_dir, changes)
    }

    pub fn take_loaded_local_changes(&self) -> LocalChanges {
        let loaded = lock(&self.loaded_local_changes).take();
        let changes = loaded.unwrap_or_else(|| {
            *lock(&self.local_changes_started) = Instant::now();
            self.local_changes()
        });
        *lock(&self.taken_count) = Some(changes.count());
        changes
    }

    // Changes between `base` and the index, i.e. what would be committed when amending.
    pub fn staged_changes_against(&self, base: &str) -> Vec<LocalChangeFile> {
        let args = ["diff", "--cached", "--name-status", "-z", "-M", base];
        git_output(&self.path, &args)
            .map(|out| parse_name_status(out.as_bytes()))
            .unwrap_or_default()
    }

    // Stages the paths: `git add -- <paths>`, the command a user would type. It records the
    // deletions and finds the new files under a directory already, `--all` would add nothing.
    pub fn stage_paths(&self, paths: &[&str]) -> std::result::Result<(), String> {
        let mut args = vec!["add", "--"];
        let paths = pathspecs(paths);
        args.extend(paths.iter().map(String::as_str));
        self.run(&args, None)
    }

    // Resets the index entries of `paths` to `staged_base`, HEAD when None: `git reset --quiet
    // [HEAD^] -- <paths>`, the command a user would type, leaving out the name where git means it.
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

    // Adds the files and directories to the .gitignore, see `ignore_path`, stopping at the first
    // that fails; what to tell of them.
    pub fn ignore_entries(&self, entries: &[Entry]) -> std::result::Result<String, String> {
        let messages = entries
            .iter()
            .map(|&entry| self.ignore_path(entry))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(messages.join("; "))
    }

    // Adds the file or directory to the .gitignore at the top of the working tree, anchored
    // there. Returns what to tell: a tracked path stays tracked, ignoring only keeps untracked
    // files out.
    pub fn ignore_path(&self, entry: Entry) -> std::result::Result<String, String> {
        let pattern = gitignore_pattern(entry);
        let file = self.path.join(".gitignore");
        // a repository may come with a .gitignore linking to a file outside of it
        if file.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
            return Err(".gitignore is a symbolic link, which spoon doesn't write through".into());
        }
        // as bytes, which any of its lines may be; it used to be emptied when they weren't UTF-8
        let content = match std::fs::read(&file) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(format!("failed to read .gitignore: {e}")),
        };
        let line = git_bytes(&pattern);
        if content.split(|&b| b == b'\n').any(|l| l == &line[..]) {
            return Ok(format!("{pattern} is already in .gitignore"));
        }
        let mut added = Vec::new();
        if !content.is_empty() && !content.ends_with(b"\n") {
            added.push(b'\n');
        }
        added.extend_from_slice(&line);
        added.push(b'\n');
        // added at its end, the rest left as it is
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&file)
            .and_then(|mut f| std::io::Write::write_all(&mut f, &added))
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

    // The paths of `files` in the working tree, to open in the editor; an error names one gone
    // from it.
    pub fn working_tree_paths(&self, files: &[&str]) -> std::result::Result<Vec<PathBuf>, String> {
        files
            .iter()
            .map(|file| {
                let path = work_path(&self.path, file);
                match path.exists() {
                    true => Ok(path),
                    false => Err(format!("{file} does not exist in the working tree")),
                }
            })
            .collect()
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

// A message git prepared, without its comment lines, those starting with one of `starts`; None
// when nothing is left. See `Repository::comment_starts`.
pub(super) fn without_comments(content: &str, starts: &[String]) -> Option<String> {
    let message: Vec<&str> = content
        .lines()
        .filter(|l| !starts.iter().any(|s| l.starts_with(s.as_str())))
        .collect();
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

pub(super) fn load_local_changes(path: &Path) -> LocalChanges {
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
        let path = text_from_git(&entry[3..]);
        // renames and copies, staged or in the working tree (of files added with
        // `git add --intent-to-add`), are followed by the original path
        let is_rename = |c: u8| c == b'R' || c == b'C';
        let orig_path = if is_rename(x) || is_rename(y) {
            entries.next().map(text_from_git)
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

// `git diff --name-status -z` output: a status, then one path or two for renames and copies.
pub(super) fn parse_name_status(bytes: &[u8]) -> Vec<LocalChangeFile> {
    let mut files = Vec::new();
    let mut tokens = bytes
        .split(|b| *b == b'\0')
        .filter(|t| !t.is_empty())
        .map(text_from_git);

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

// The local changes and a fingerprint of their files, loaded in the background every few seconds
// for the local changes page to reload when something changed, see `files_stamp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesSnapshot {
    pub changes: LocalChanges,
    pub stamp: u64,
    // when it started loading: one started before the page reloaded after a change of its own
    // shows the files as they were before that change
    pub started: Instant,
}

impl FilesSnapshot {
    pub fn load(work_tree: &Path, git_dir: &Path) -> Self {
        let started = Instant::now();
        let changes = load_local_changes(work_tree);
        let stamp = files_stamp(work_tree, git_dir, &changes);
        FilesSnapshot {
            changes,
            stamp,
            started,
        }
    }
}

// A fingerprint of the local changes and what they are made of: the index, and each changed file's
// size and time of change, which an edit to an already changed file changes without changing its
// status.
pub fn files_stamp(work_tree: &Path, git_dir: &Path, changes: &LocalChanges) -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = rustc_hash::FxBuildHasher.build_hasher();
    let mut stat = |path: &Path| match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            meta.len().hash(&mut hasher);
            meta.modified().ok().hash(&mut hasher);
        }
        Err(_) => hasher.write_u8(0),
    };
    stat(&git_dir.join("index"));
    for file in changes.unstaged.iter().chain(&changes.staged) {
        stat(&work_path(work_tree, &file.path));
    }
    changes.unstaged.hash(&mut hasher);
    changes.staged.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::run_task;
    use crate::git::GitTask;
    use crate::git::SortCommit;

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
        let load = || FilesSnapshot::load(path, &git_dir);

        let before = load();
        assert_eq!(load().stamp, before.stamp);
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

    // A .gitignore linking outside of the repository used to have the pattern written there.
    #[test]
    fn test_ignore_through_a_linked_gitignore_is_refused() {
        use crate::test_support::git::GitRepository;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("repo");
        std::fs::create_dir(&path).unwrap();
        let outside = dir.path().join("outside");
        std::fs::write(&outside, "kept\n").unwrap();
        std::os::unix::fs::symlink(&outside, path.join(".gitignore")).unwrap();
        let repo = GitRepository::new(&path);
        repo.init();
        repo.commit("first", "2024-01-01");
        std::fs::write(path.join("a.txt"), "x").unwrap();
        let repository = Repository::load(&path, SortCommit::Chronological, None, false).unwrap();

        assert!(repository.ignore_path(Entry::File("a.txt")).is_err());
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "kept\n");
    }

    #[test]
    fn test_ignore_names_with_pattern_characters() {
        use crate::test_support::git::GitRepository;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        repo.commit("init", "2024-01-01");
        let names = ["x*.log", "!bang", "#hash", "trail ", "q?.txt"];
        for file in names.iter().chain(&["xa.log", "qa.txt"]) {
            std::fs::write(path.join(file), "x").unwrap();
        }
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        for name in names {
            repository.ignore_path(Entry::File(name)).unwrap();
        }
        // only those files, not the ones their characters would match as a pattern
        let status = git_output(path, &["status", "--porcelain", "--untracked-files=all"]).unwrap();
        assert_eq!(status, "?? .gitignore\n?? qa.txt\n?? xa.log\n");
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
