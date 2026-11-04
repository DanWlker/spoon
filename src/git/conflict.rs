// Conflicts: the sides of a conflicted file, resolving and staging them, and where a rebase
// would stop on one.

use std::path::Path;

use super::{
    git_command, git_output, git_output_bytes, git_output_with_status, pathspecs, run_logged,
    CommitHash, LocalChangeStatus, LocalChanges, Repository,
};

impl Repository {
    // The first of `steps` that conflicts, applied one after another on `start` in memory with
    // `git merge-tree`, like a rebase or a cherry-pick does: each is (base, commit), the changes
    // from base to commit, or a merge of commit when base is None. None when all apply, or git
    // can't tell (git before 2.38). As work to run on another thread, the popup warning of it
    // opening meanwhile.
    pub fn first_conflict_work(
        &self,
        start: &str,
        steps: Vec<(Option<String>, String)>,
    ) -> ConflictWork {
        let (path, start) = (self.path.clone(), start.to_string());
        Box::new(move || first_conflict_at(&path, &start, &steps))
    }

    // The first conflict replaying the commits of `range` (rev-list arguments) on `onto`, like a
    // rebase, without the merges and the commits in `skip`, see `first_conflict_work`.
    pub fn replay_conflict_work(
        &self,
        onto: &str,
        range: &[&str],
        skip: &[CommitHash],
    ) -> ConflictWork {
        let (path, onto) = (self.path.clone(), onto.to_string());
        let range: Vec<String> = range.iter().map(|r| r.to_string()).collect();
        let skip = skip.to_vec();
        Box::new(move || {
            let mut args = vec!["rev-list", "--reverse", "--no-merges"];
            args.extend(range.iter().map(String::as_str));
            let out = git_output(&path, &args).ok()?;
            let steps: Vec<_> = out
                .lines()
                .filter(|c| !skip.iter().any(|s| s.as_str() == *c))
                .map(|c| (Some(format!("{c}^")), c.to_string()))
                .collect();
            first_conflict_at(&path, &onto, &steps)
        })
    }

    // The content of a conflicted file on one side, None when that side deleted it.
    // What a side of a conflicted file is in the index: a file, a symlink, a submodule or nothing.
    pub fn conflict_side(&self, path: &str, side: ConflictSide) -> SideContent {
        let stage = self.unmerged(path).ok().and_then(|stages| {
            stages
                .into_iter()
                .find_map(|(stage, mode, object)| (stage == side.stage()).then_some((mode, object)))
        });
        let Some((mode, object)) = stage else {
            return SideContent::Deleted;
        };
        let blob = || {
            git_output_bytes(&self.path, &["cat-file", "blob", &object])
                .map(|blob| String::from_utf8_lossy(&blob).into_owned())
                .unwrap_or_default()
        };
        match mode.as_str() {
            // its short hash and subject, from the submodule when it is checked out
            "160000" => {
                let submodule = self.path.join(path);
                let summary = submodule
                    .join(".git")
                    .exists()
                    .then(|| git_output(&submodule, &["log", "-1", "--format=%h %s", &object]).ok())
                    .flatten()
                    .map(|summary| summary.trim().to_string());
                SideContent::Submodule(
                    summary.unwrap_or_else(|| object.get(..7).unwrap_or(&object).into()),
                )
            }
            "120000" => SideContent::Symlink(blob()),
            _ => SideContent::File(blob()),
        }
    }

    // `changes`, with the conflicts resolved by hand staged like lazygit does (see
    // `resolved_conflicts`), loaded again then; not while a git command runs, which `stage_paths`
    // refuses.
    pub fn stage_resolved_conflicts(&self, changes: LocalChanges) -> LocalChanges {
        let conflicted: Vec<&str> = changes
            .unstaged
            .iter()
            .filter(|f| f.status == LocalChangeStatus::Conflicted)
            .map(|f| f.path.as_str())
            .collect();
        let resolved = self.resolved_conflicts(&conflicted);
        let paths: Vec<&str> = resolved.iter().map(String::as_str).collect();
        if paths.is_empty() || self.stage_paths(&paths).is_err() {
            return changes;
        }
        self.local_changes()
    }

    // The conflicted files of `paths` resolved by hand, to stage like lazygit does: both sides
    // changed the text of the file, and the file in the working tree has no conflict marker left.
    // A binary file, which git leaves without markers, a side deleted, a symlink or a submodule
    // is resolved by picking a side instead.
    fn resolved_conflicts(&self, paths: &[&str]) -> Vec<String> {
        let text_sides = |path: &str| {
            self.unmerged(path).is_ok_and(|stages| {
                [2, 3].iter().all(|side| {
                    stages.iter().any(|(stage, mode, _)| {
                        stage == side && matches!(mode.as_str(), "100644" | "100755")
                    })
                })
            })
        };
        let no_markers = |path: &str| {
            let file = self.path.join(path);
            let Ok(content) = std::fs::read(&file) else {
                return false;
            };
            // binary as git tells it, by a NUL in its first 8000 bytes
            let binary = content.iter().take(8000).any(|&b| b == 0);
            let regular = std::fs::symlink_metadata(&file).is_ok_and(|meta| meta.is_file());
            regular
                && !binary
                && !String::from_utf8_lossy(&content)
                    .lines()
                    .any(|line| is_conflict_marker(line, "<<<<<<<"))
        };
        paths
            .iter()
            .filter(|path| text_sides(path) && no_markers(path))
            .map(|path| path.to_string())
            .collect()
    }

    // The stage (1 the base, 2 ours, 3 theirs), mode and object of each side git has of the
    // conflicted `path`; none when it isn't conflicted.
    fn unmerged(&self, path: &str) -> std::result::Result<Vec<(u8, String, String)>, String> {
        // literal, so `x*.txt` isn't `xa.txt`
        let spec = pathspecs(&[path]);
        // "<mode> <object> <stage>\t<path>"
        let out = git_output(&self.path, &["ls-files", "-u", "-z", "--", &spec[0]])?;
        Ok(out
            .split('\0')
            .filter_map(|entry| {
                let mut fields = entry.split_once('\t')?.0.split(' ');
                let (mode, object) = (fields.next()?, fields.next()?);
                let stage = fields.next()?.parse().ok()?;
                Some((stage, mode.to_string(), object.to_string()))
            })
            .collect())
    }

    // Resolves a conflicted file by taking one side whole, removing the file when that side
    // deleted it; choosing conflict by conflict is left to the merge tool.
    pub fn resolve_conflict(
        &self,
        path: &str,
        side: ConflictSide,
    ) -> std::result::Result<(), String> {
        // before the file is written, which `git add` would then be refused
        self.check_idle()?;
        // literal, so resolving `x*.txt` leaves `xa.txt` alone
        let spec = pathspecs(&[path]);
        let spec = spec[0].as_str();
        let stages = self.unmerged(path)?;
        // a file with no stage of a conflict would be removed below, by `git rm`
        if stages.is_empty() {
            return Err(format!("{path} has no conflict to resolve"));
        }
        let taken = stages
            .iter()
            .find(|(stage, ..)| *stage == side.stage())
            .map(|(_, mode, object)| (mode.as_str(), object.as_str()));
        match taken {
            // the side taken deleted it
            None => return self.run(&["rm", "--quiet", "--", spec], None),
            // `checkout --ours` leaves a submodule as it is, so it is checked out at the commit
            // taken, then staged, like lazygit
            Some(("160000", commit)) => {
                let args = ["checkout", commit];
                let submodule = self.path.join(path);
                run_logged(
                    git_command().args(args).current_dir(submodule),
                    &args,
                    None,
                    None,
                )?;
            }
            Some(_) => self.run(&["checkout", side.option(), "--", spec], None)?,
        }
        self.run(&["add", "--", spec], None)
    }
}

// A side of a conflicted file, see `Repository::conflict_side`: a file's content, where a symlink
// points, a submodule's commit (its short hash and subject), or nothing, the side deleting it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SideContent {
    File(String),
    Symlink(String),
    Submodule(String),
    Deleted,
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

    // The option of git checkout that takes the side.
    fn option(self) -> &'static str {
        match self {
            ConflictSide::Ours => "--ours",
            ConflictSide::Theirs => "--theirs",
        }
    }
}

// Whether `line` is the conflict marker `marker`, like "<<<<<<<", alone or followed by a label.
pub fn is_conflict_marker(line: &str, marker: &str) -> bool {
    line.strip_prefix(marker)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
}

pub const MAX_CONFLICT_STEPS: usize = 100;

// The work finding the first conflict of a command, see `Repository::first_conflict_work`.
pub type ConflictWork = Box<dyn FnOnce() -> Option<Conflict> + Send>;

fn first_conflict_at(
    path: &Path,
    start: &str,
    steps: &[(Option<String>, String)],
) -> Option<Conflict> {
    let mut tree = start.to_string();
    // the first steps only, which bounds the work of a long rebase
    for (base, commit) in steps.iter().take(MAX_CONFLICT_STEPS) {
        let base = base.as_ref().map(|b| format!("--merge-base={b}"));
        let mut args = vec!["merge-tree", "--write-tree", "--name-only", "--no-messages"];
        args.extend(base.as_deref());
        args.extend([tree.as_str(), commit.as_str()]);
        let (status, out) = git_output_with_status(path, &args)?;
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

// What `Repository::first_conflict_work` found.
#[derive(Debug, PartialEq, Eq)]
pub enum Conflict {
    // the commit stopped at, and the files conflicting
    At(String, Vec<String>),
    // none in the steps checked, with this many after them not checked
    Unchecked(usize),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::GitTask;
    use crate::git::RunningTask;
    use crate::git::SortCommit;

    // A git command running in the background, saying `running`.
    fn running(running: &str) -> RunningTask {
        let task = GitTask {
            running: running.into(),
            ..Default::default()
        };
        RunningTask {
            task,
            head_before_commit: None,
            quit_after: false,
        }
    }

    #[test]
    fn test_resolve_conflict_takes_side_whole() {
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
        *repository.running.lock() = Some(running("Pushing master"));
        assert!(repository
            .resolve_conflict("f", ConflictSide::Ours)
            .is_err());
        assert_eq!(std::fs::read_to_string(path.join("f")).unwrap(), conflicted);
        *repository.running.lock() = None;
        repository
            .resolve_conflict("f", ConflictSide::Ours)
            .unwrap();
        // the file is ours, the change of the incoming side that merged cleanly left out too
        let content = std::fs::read_to_string(path.join("f")).unwrap();
        assert_eq!(content, lines("1", "NINE-ours"));
        assert!(repository.local_changes().unstaged.is_empty());
        // once resolved and committed, taking a side again is refused instead of removing the file
        git_output(path, &["commit", "-q", "--no-edit"]).unwrap();
        assert!(repository
            .resolve_conflict("f", ConflictSide::Theirs)
            .is_err());
        assert_eq!(std::fs::read_to_string(path.join("f")).unwrap(), content);
    }

    #[test]
    fn test_resolved_conflicts() {
        use crate::test_support::git::GitRepository;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        let write = |name: &str, content: &[u8]| std::fs::write(path.join(name), content).unwrap();
        for (name, content) in [
            ("text", &b"base\n"[..]),
            ("binary", b"\0base"),
            ("gone", b"base\n"),
        ] {
            write(name, content);
        }
        repo.add(&["text", "binary", "gone"]);
        repo.commit("base", "2024-01-01");
        repo.checkout_b("incoming");
        write("text", b"theirs\n");
        write("binary", b"\0theirs");
        write("gone", b"theirs\n");
        repo.add(&["text", "binary", "gone"]);
        repo.commit("incoming", "2024-01-02");
        repo.checkout("master");
        write("text", b"ours\n");
        write("binary", b"\0ours");
        repo.add(&["text", "binary"]);
        git_output(path, &["rm", "-q", "gone"]).unwrap();
        repo.commit("existing", "2024-01-03");
        assert!(git_output(path, &["merge", "incoming"]).is_err());

        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let conflicted = ["text", "binary", "gone"];
        // the markers still there
        assert!(repository.resolved_conflicts(&conflicted).is_empty());
        // the text edited: resolved. A binary file has no markers to remove, and a side deleted
        // nothing to edit: both are resolved by picking a side
        write("text", b"both\n");
        assert_eq!(repository.resolved_conflicts(&conflicted), ["text"]);
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
    fn test_resolve_conflict_of_symlink() {
        use crate::test_support::git::{git, GitRepository};

        let dir = tempfile::tempdir().unwrap();
        let path = &dir.path().join("repo");
        std::fs::create_dir(path).unwrap();
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, "outside\n").unwrap();
        let repo = GitRepository::new(path);
        repo.init();
        let link = |target: &str| {
            let _ = std::fs::remove_file(path.join("link"));
            std::os::unix::fs::symlink(target, path.join("link")).unwrap();
            repo.add(&["link"]);
        };
        link("../base.txt");
        repo.commit("base", "2024-01-01");
        repo.checkout_b("incoming");
        link("../theirs.txt");
        repo.commit("incoming", "2024-01-02");
        repo.checkout("master");
        link("../outside.txt");
        repo.commit("existing", "2024-01-03");
        assert!(git_output(path, &["merge", "incoming"]).is_err());

        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        repository
            .resolve_conflict("link", ConflictSide::Theirs)
            .unwrap();
        // the link is taken whole, the file it pointed at left alone
        let target = std::fs::read_link(path.join("link")).unwrap();
        assert_eq!(target, Path::new("../theirs.txt"));
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "outside\n");
        assert_eq!(
            git(path, &["rev-parse", ":link"]),
            git(path, &["rev-parse", "incoming:link"])
        );
    }

    #[test]
    fn test_resolve_conflict_of_submodule() {
        use crate::test_support::git::{git, GitRepository};

        let dir = tempfile::tempdir().unwrap();
        let inner = &dir.path().join("inner");
        let path = &dir.path().join("repo");
        std::fs::create_dir_all(inner).unwrap();
        std::fs::create_dir_all(path).unwrap();
        let inner_repo = GitRepository::new(inner);
        inner_repo.init();
        // commits on two branches, which git can't fast-forward one to the other
        inner_repo.commit("c1", "2024-01-01");
        inner_repo.checkout_b("side");
        inner_repo.commit("c2", "2024-01-02");
        inner_repo.checkout("master");
        inner_repo.commit("c3", "2024-01-03");
        let hash = |rev: &str| git(inner, &["rev-parse", rev]).trim().to_string();
        let gitlink = |rev: &str| {
            let info = format!("160000,{},sub", hash(rev));
            git(path, &["update-index", "--add", "--cacheinfo", &info]);
        };
        let repo = GitRepository::new(path);
        repo.init();
        git(path, &["clone", "-q", "../inner", "sub"]);
        gitlink("master~1");
        repo.commit("base", "2024-01-01");
        repo.checkout_b("incoming");
        gitlink("side");
        repo.commit("incoming", "2024-01-02");
        repo.checkout("master");
        gitlink("master");
        repo.commit("existing", "2024-01-03");
        assert!(git_output(path, &["merge", "incoming"]).is_err());

        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        repository
            .resolve_conflict("sub", ConflictSide::Theirs)
            .unwrap();
        // the submodule is checked out at the commit taken, which is staged
        assert_eq!(
            git(&path.join("sub"), &["rev-parse", "HEAD"]).trim(),
            hash("side")
        );
        let staged = git(path, &["ls-files", "--stage", "sub"]);
        assert_eq!(staged, format!("160000 {} 0\tsub\n", hash("side")));
    }
}
