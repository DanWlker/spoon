// The history of a file: its blame, the commits changing it, and its tree and contents at a commit.

use std::path::Path;

use chrono::{DateTime, FixedOffset};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::background::joined;

use super::{git_output, git_output_bytes, parse_iso_date, CommitHash, Repository};

impl Repository {
    // The contents of a file at a commit, as work to run on another thread.
    pub fn commit_file_content_work(
        &self,
        commit_hash: &CommitHash,
        path: &str,
    ) -> impl FnOnce() -> std::result::Result<Vec<u8>, String> + Send + 'static {
        let object = format!("{}:{path}", commit_hash.as_str());
        let repository = self.path.clone();
        move || git_output_bytes(&repository, &["cat-file", "blob", &object])
    }

    pub fn commit_tree(&self, commit_hash: &CommitHash) -> CommitTree {
        self.commit_tree_work(commit_hash)()
    }

    // All file paths in the tree of a commit, and those of them that are gitlinks (submodules),
    // from their modes, in one listing; as work to run on another thread.
    pub fn commit_tree_work(
        &self,
        commit_hash: &CommitHash,
    ) -> impl FnOnce() -> CommitTree + Send + 'static {
        let (repository, commit) = (self.path.clone(), commit_hash.as_str().to_string());
        move || commit_tree_at(&repository, &commit)
    }

    // Who last changed each line of the file at `path` as of the commit, see `Blame`, as work to
    // run on another thread.
    pub fn blame_work(
        &self,
        commit_hash: &CommitHash,
        path: &str,
    ) -> impl FnOnce() -> std::result::Result<Blame, String> + Send + 'static {
        let (repository, commit, path) = (self.path.clone(), commit_hash.clone(), path.to_string());
        move || blame_at(&repository, &commit, &path)
    }

    #[cfg(test)]
    pub fn file_history(&self, start: &CommitHash, path: &str) -> FileHistoryResult {
        self.file_history_work(start, path)()
    }

    // The commits changing the file at `path` in the history of HEAD and of `start`, renames
    // followed, the newest first; with the index of the last one changing it as of `start`. As
    // work to run on another thread.
    pub fn file_history_work(
        &self,
        start: &CommitHash,
        path: &str,
    ) -> impl FnOnce() -> FileHistoryResult + Send + 'static {
        let (repository, start, path) = (self.path.clone(), start.clone(), path.to_string());
        move || file_history_at(&repository, &start, &path)
    }

    // The file's history with its blame as of the last commit changing it up to `start`, as work
    // to run on another thread, see `file_history_work`.
    pub fn file_blame_work(
        &self,
        start: &CommitHash,
        path: &str,
    ) -> impl FnOnce() -> FileBlameResult + Send + 'static {
        let (repository, start, path) = (self.path.clone(), start.clone(), path.to_string());
        move || {
            let (history, index) = file_history_at(&repository, &start, &path)?;
            let revision = &history[index];
            let blame = blame_at(&repository, &revision.commit, &revision.path)?;
            Ok((history, index, blame))
        }
    }
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
            // the details end with the file's name; a commit blamed under two paths comes again
            // with the name alone, its details kept from the first time
            "filename" => {
                let offset = FixedOffset::east_opt(tz).unwrap_or(FixedOffset::east_opt(0).unwrap());
                let date = DateTime::from_timestamp(time, 0).unwrap_or_default();
                blame
                    .commits
                    .entry(commit.clone())
                    .or_insert_with(|| BlameCommit {
                        author: std::mem::take(&mut author),
                        date: date.with_timezone(&offset),
                    });
            }
            _ if key.len() >= 40 && key.bytes().all(|b| b.is_ascii_hexdigit()) => {
                commit = CommitHash::from(key);
            }
            _ => {}
        }
    }
    blame
}

// All file paths in the tree of a commit, and those of them that are gitlinks.
pub type CommitTree = (Vec<String>, FxHashSet<String>);

// See `Repository::commit_tree_work`.
fn commit_tree_at(repository: &Path, commit: &str) -> CommitTree {
    let out = git_output(repository, &["ls-tree", "-r", "-z", commit]).unwrap_or_default();
    let mut paths = Vec::new();
    let mut gitlinks = FxHashSet::default();
    for (meta, path) in out.split('\0').filter_map(|entry| entry.split_once('\t')) {
        if meta.starts_with("160000 ") {
            gitlinks.insert(path.to_string());
        }
        paths.push(path.to_string());
    }
    (paths, gitlinks)
}

// The commits changing a file and the one last changing it, see `Repository::file_history_work`.
pub type FileHistoryResult = std::result::Result<(Vec<FileRevision>, usize), String>;

// The commits changing a file with the blame of the last one, see `Repository::file_blame_work`.
pub type FileBlameResult = std::result::Result<(Vec<FileRevision>, usize, Blame), String>;

fn blame_at(
    repository: &Path,
    commit_hash: &CommitHash,
    path: &str,
) -> std::result::Result<Blame, String> {
    let args = ["blame", "--porcelain", commit_hash.as_str(), "--", path];
    git_output(repository, &args).map(|out| parse_blame(&out))
}

// See `Repository::file_history_work`: its two walks of the history run side by side.
// `--follow` only follows a name back in time, so a file renamed after `start` is walked under its
// name at HEAD, to list the commits from the rename on too; when git doesn't see the rename, or
// the last commit as of `start` isn't on that walk, it is walked under `path` alone.
fn file_history_at(repository: &Path, start: &CommitHash, path: &str) -> FileHistoryResult {
    let walk = |name: &str| {
        let format = "--format=%H%x00%aI%x00%s";
        let args = [
            "log",
            "--follow",
            "--name-only",
            "-z",
            format,
            "HEAD",
            start.as_str(),
            "--",
            name,
        ];
        // each field ends with a NUL, which git keeps out of them, then the file's name, on a
        // line of its own, which no field starts with
        let out = git_output(repository, &args)?;
        let mut fields = out.split('\0').peekable();
        let mut history = vec![];
        while let (Some(commit), Some(date), Some(subject)) =
            (fields.next(), fields.next(), fields.next())
        {
            let Some(path) = fields.next_if(|f| f.starts_with('\n')) else {
                continue;
            };
            history.push(FileRevision {
                commit: CommitHash::from(commit),
                date: parse_iso_date(date),
                subject: subject.into(),
                path: path[1..].into(),
            });
        }
        Ok::<_, String>(history)
    };
    // the file's name at HEAD, when it isn't there under `path`
    let renamed = || {
        let head_file = format!("HEAD:{path}");
        git_output(repository, &["cat-file", "-e", &head_file]).err()?;
        let args = [
            "diff",
            "-M",
            "--diff-filter=R",
            "--name-status",
            "-z",
            start.as_str(),
            "HEAD",
        ];
        let out = git_output(repository, &args).ok()?;
        // "R<score>", the old path and the new one, of each rename
        let fields: Vec<&str> = out.split('\0').collect();
        fields
            .as_chunks::<3>()
            .0
            .iter()
            .find(|[_, old, _]| *old == path)
            .map(|[_, _, new]| new.to_string())
    };
    // walked as the history is, which leaves out merges
    let last_args = [
        "log",
        "-1",
        "--follow",
        "--format=%H",
        start.as_str(),
        "--",
        path,
    ];
    let (renamed, history, last) = std::thread::scope(|scope| {
        let last = scope.spawn(|| git_output(repository, &last_args));
        let renamed = renamed();
        let history = walk(renamed.as_deref().unwrap_or(path));
        (renamed, history, joined(last))
    });
    let last = last?;
    let position = |history: &[FileRevision]| {
        history
            .iter()
            .position(|r| r.commit.as_str() == last.trim())
    };
    let mut history = history?;
    let mut index = position(&history);
    if index.is_none() && renamed.is_some() {
        history = walk(path)?;
        index = position(&history);
    }
    let index = index.ok_or_else(|| format!("{path} has no history"))?;
    Ok((history, index))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::git_command;
    use crate::git::SortCommit;

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
    fn test_file_history_from_before_a_rename() {
        use crate::test_support::git::{git, GitRepository};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        // a is renamed to b, x to y rewritten, which git can't tell from a new file
        std::fs::write(path.join("a"), "one\ntwo\nthree\nfour\n").unwrap();
        std::fs::write(path.join("x"), "x\n").unwrap();
        repo.add(&["a", "x"]);
        repo.commit("c1", "2024-01-01");
        std::fs::write(path.join("a"), "one\ntwo\nthree\nFOUR\n").unwrap();
        std::fs::write(path.join("x"), "X\n").unwrap();
        repo.add(&["a", "x"]);
        repo.commit("c2", "2024-01-02");
        let c2 = CommitHash::from(repo.rev_parse_head().as_str());
        git(path, &["mv", "a", "b"]);
        git(path, &["mv", "x", "y"]);
        std::fs::write(path.join("y"), "nothing\nalike\n").unwrap();
        repo.add(&["y"]);
        repo.commit("c3", "2024-01-03");
        std::fs::write(path.join("b"), "ONE\ntwo\nthree\nFOUR\n").unwrap();
        repo.add(&["b"]);
        repo.commit("c4", "2024-01-04");

        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let shown = |history: &[FileRevision]| -> Vec<String> {
            history
                .iter()
                .map(|r| format!("{} {}", r.subject, r.path))
                .collect()
        };
        // the commits after the rename too, each under the name it had then
        let (history, index) = repository.file_history(&c2, "a").unwrap();
        assert_eq!(shown(&history), ["c4 b", "c3 b", "c2 a", "c1 a"]);
        assert_eq!(index, 2);
        // the rename not seen: under the name as of c2 alone
        let (history, index) = repository.file_history(&c2, "x").unwrap();
        assert_eq!(shown(&history), ["c3 x", "c2 x", "c1 x"]);
        assert_eq!(index, 1);
    }

    #[test]
    fn test_parse_blame() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let out = format!(
            "{a} 1 1 1\nauthor Ann\nauthor-time 0\nauthor-tz -0130\nfilename f\n\tone\n\
             {b} 2 2 1\nauthor Bo\nauthor-time 60\nauthor-tz +0100\nfilename f\n\t\ttwo\n\
             {a} 3 3\n\tthree\n\
             {a} 1 4 1\nfilename g\n\tfour\n"
        );
        let blame = parse_blame(&out);
        let lines: Vec<(&str, &str)> = blame
            .lines
            .iter()
            .map(|(commit, text)| (commit.as_str(), text.as_str()))
            .collect();
        assert_eq!(
            lines,
            [(&*a, "one"), (&*b, "\ttwo"), (&*a, "three"), (&*a, "four")]
        );
        let ann = &blame.commits[&CommitHash::from(a.as_str())];
        assert_eq!(ann.author, "Ann");
        assert_eq!(ann.date.to_rfc3339(), "1969-12-31T22:30:00-01:30");
        let bo = &blame.commits[&CommitHash::from(b.as_str())];
        assert_eq!(bo.date.to_rfc3339(), "1970-01-01T01:01:00+01:00");
    }

    #[test]
    fn test_file_history_with_separators_in_subjects() {
        use crate::test_support::git::GitRepository;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        for (i, subject) in ["one\x01", "two\x1f"].iter().enumerate() {
            std::fs::write(path.join("f"), i.to_string()).unwrap();
            repo.add(&["f"]);
            repo.commit(subject, "2024-01-01");
        }

        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let head = repository.head_commit().unwrap();
        let (history, index) = repository.file_history(&head, "f").unwrap();
        let subjects: Vec<&str> = history.iter().map(|r| r.subject.as_str()).collect();
        assert_eq!(subjects, ["two\x1f", "one\x01"]);
        assert!(history.iter().all(|r| r.path == "f"));
        assert_eq!(index, 0);
    }
}
