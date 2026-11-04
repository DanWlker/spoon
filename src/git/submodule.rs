// Submodules: what a gitlink points to, and the repository of a submodule, loaded once.

use rustc_hash::FxHashSet;
use std::path::Path;

use super::{git_output, load_local_changes, pathspecs, CommitHash, Repository};

impl Repository {
    // The paths among `paths` that are gitlinks (submodules) in the tree of a commit, from their
    // modes: a submodule path is otherwise listed as a file. Only those
    // paths are listed, not the whole tree, which is big in a big repository.
    pub fn commit_gitlink_paths(
        &self,
        commit_hash: &CommitHash,
        paths: &[&str],
    ) -> FxHashSet<String> {
        if paths.is_empty() {
            return FxHashSet::default();
        }
        let mut args = vec!["ls-tree", "-r", "-z", commit_hash.as_str()];
        // past this many, the whole tree is listed, the command line staying short enough
        let pathspecs = pathspecs(paths);
        if paths.len() <= 1000 {
            args.push("--");
            args.extend(pathspecs.iter().map(String::as_str));
        }
        let out = git_output(&self.path, &args).unwrap_or_default();
        out.split('\0')
            .filter_map(|entry| {
                let (meta, path) = entry.split_once('\t')?;
                meta.starts_with("160000 ").then(|| path.to_string())
            })
            .collect()
    }

    // Whether `path` is a submodule tracked in the index, see `submodule_info`.
    pub fn is_submodule(&self, path: &str) -> bool {
        self.submodules.contains(path)
    }

    // What a submodule records and is at: the commit the index records, its current HEAD and how
    // many files are uncommitted inside it. None when `path` isn't a submodule. The parent repo
    // can only stage the gitlink, so the pages beside the files show the submodule's own commits
    // in place of the gitlink diff.
    pub fn submodule_info(&self, path: &str) -> Option<SubmoduleInfo> {
        if !self.is_submodule(path) {
            return None;
        }
        let index = self.index_submodule_commit(path)?;
        let submodule = self.path.join(path);
        // a checked-out submodule has a `.git` file (or directory) of its own; without it git
        // falls back to the superproject above, which would read the wrong HEAD
        let head = submodule
            .join(".git")
            .exists()
            .then(|| git_output(&submodule, &["rev-parse", "HEAD"]).ok())
            .flatten()
            .map(|out| out.trim().to_string());
        let Some(head) = head else {
            return Some(SubmoduleInfo {
                index,
                head: None,
                // nothing can be read in a working tree that isn't checked out
                uncommitted: None,
            });
        };
        Some(SubmoduleInfo {
            uncommitted: Some(load_local_changes(&submodule).count()),
            index,
            head: Some(head),
        })
    }

    // A submodule's own repository, for the pages that list its commits. Loaded once per path and
    // pair of recorded commits (`from..to`), then kept: the parent owns it for the rest of this
    // repository's life, so the reference a page holds stays valid, and it all goes on reload.
    // None when `from..to` has no commits (equal, or `to` behind `from`).
    #[cfg(test)]
    pub fn submodule_repository(
        &self,
        path: &str,
        from: &str,
        to: &str,
    ) -> std::result::Result<Option<&Repository>, String> {
        if let Some(repository) = self.loaded_submodule_repository(path, from, to) {
            return Ok(Some(repository));
        }
        let loaded = self.submodule_repository_work(path, from, to)()?;
        Ok(loaded.map(|repository| self.keep_submodule_repository(path, from, to, repository)))
    }

    // The submodule's repository of `from..to` when loaded already, see `submodule_repository`.
    pub fn loaded_submodule_repository(
        &self,
        path: &str,
        from: &str,
        to: &str,
    ) -> Option<&Repository> {
        self.submodule_repositories
            .get(&format!("{path}\0{from}..{to}"))
    }

    // Loads the submodule's repository of `from..to` on another thread, to keep with
    // `keep_submodule_repository`; None when it has no commits.
    pub fn submodule_repository_work(
        &self,
        path: &str,
        from: &str,
        to: &str,
    ) -> impl FnOnce() -> std::result::Result<Option<Repository>, String> + Send + 'static {
        let submodule = self.path.join(path);
        let range = format!("{from}..{to}");
        move || Repository::load_graph(&submodule, &[range]).map_err(|err| err.to_string())
    }

    // Keeps the submodule's repository of `from..to` loaded on another thread, see
    // `submodule_repository`.
    pub fn keep_submodule_repository(
        &self,
        path: &str,
        from: &str,
        to: &str,
        repository: Repository,
    ) -> &Repository {
        let key = format!("{path}\0{from}..{to}");
        self.submodule_repositories.insert(key, repository)
    }

    // The gitlink (commit) the index records for the submodule at `path`.
    fn index_submodule_commit(&self, path: &str) -> Option<String> {
        let spec = pathspecs(&[path]);
        let out = git_output(&self.path, &["ls-files", "--stage", "-z", "--", &spec[0]]).ok()?;
        let entry = out.split('\0').find(|entry| !entry.is_empty())?;
        let (meta, _) = entry.split_once('\t')?;
        let mut fields = meta.split(' ');
        let mode = fields.next()?;
        let commit = fields.next()?;
        (mode == "160000").then(|| commit.to_string())
    }

    // The commit `revision` records for the submodule at `path`, or None when it has no gitlink
    // there (a plain file or directory, or the submodule wasn't in that commit). The tree is read
    // rather than the current index, so an old commit that moved a submodule still finds both
    // gitlinks after the path stops being a submodule at HEAD.
    pub fn submodule_gitlink_at(&self, revision: &str, path: &str) -> Option<String> {
        let spec = pathspecs(&[path]);
        let out = git_output(&self.path, &["ls-tree", "-z", revision, "--", &spec[0]]).ok()?;
        out.split('\0').find_map(|entry| {
            let (meta, file) = entry.split_once('\t')?;
            let mut fields = meta.split(' ');
            let mode = fields.next()?;
            let _kind = fields.next()?;
            let commit = fields.next()?;
            (mode == "160000" && file == path).then(|| commit.to_string())
        })
    }
}

// The submodule repositories loaded beside this one, kept append-only so the references handed
// out by `submodule_repository` stay valid while more are loaded. The map boxes its values, so
// their addresses don't change.
#[derive(Default)]
pub(super) struct SubmoduleRepositories(elsa::sync::FrozenMap<String, Box<Repository>>);

impl std::fmt::Debug for SubmoduleRepositories {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SubmoduleRepositories({} loaded)", self.0.len())
    }
}

impl SubmoduleRepositories {
    fn get(&self, key: &str) -> Option<&Repository> {
        self.0.get(key)
    }

    fn insert(&self, key: String, repository: Repository) -> &Repository {
        self.0.insert(key, Box::new(repository))
    }
}

// A submodule seen from the superproject: the commit the index records, the one the submodule is
// at, and how many files are uncommitted inside it. The pages beside the files list the
// submodule's commits from `submodule_repository` instead of the gitlink diff, which the parent
// can't turn into the content changes.
#[derive(Debug, Default, Clone)]
pub struct SubmoduleInfo {
    // the commit the index records for the submodule
    pub index: String,
    // None when the submodule isn't checked out, so its working tree can't be read
    pub head: Option<String>,
    // the files with uncommitted changes inside the submodule, counted once each; None without a
    // working tree
    pub uncommitted: Option<usize>,
}

// The paths of the submodules the index tracks (mode 160000), which its gitlink entries name.
pub(super) fn load_submodules(path: &Path) -> FxHashSet<String> {
    let out = git_output(path, &["ls-files", "--stage", "-z"]).unwrap_or_default();
    out.split('\0')
        .filter_map(|entry| {
            let (meta, file) = entry.split_once('\t')?;
            meta.starts_with("160000 ").then(|| file.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::SortCommit;
    use std::path::PathBuf;

    // A superproject with `sub` added from a repository of its own, returned with the submodule's
    // path.
    fn repository_with_submodule(path: &Path) -> PathBuf {
        use crate::test_support::git::{git, GitRepository};

        let base = path.join("base");
        std::fs::create_dir(&base).unwrap();
        let base_repo = GitRepository::new(&base);
        base_repo.init();
        std::fs::write(base.join("a.txt"), "one\n").unwrap();
        base_repo.add(&["a.txt"]);
        base_repo.commit("init", "2024-01-01");

        let superproject = path.join("super");
        std::fs::create_dir(&superproject).unwrap();
        let repo = GitRepository::new(&superproject);
        repo.init();
        git(
            &superproject,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                base.to_str().unwrap(),
                "sub",
            ],
        );
        repo.commit("add sub", "2024-01-02");
        superproject
    }

    #[test]
    fn test_submodule_info_and_repository_of_its_commits() {
        use crate::test_support::git::GitRepository;

        let dir = tempfile::tempdir().unwrap();
        let superproject = repository_with_submodule(dir.path());
        let sub = superproject.join("sub");
        let sub_repo = GitRepository::new(&sub);
        std::fs::write(sub.join("a.txt"), "two\n").unwrap();
        sub_repo.add(&["a.txt"]);
        sub_repo.commit("second", "2024-01-03");
        std::fs::write(sub.join("a.txt"), "three\n").unwrap();
        std::fs::write(sub.join("new.txt"), "new\n").unwrap();

        let repository =
            Repository::load(&superproject, SortCommit::Chronological, None, false).unwrap();
        assert!(repository.is_submodule("sub"));
        let info = repository.submodule_info("sub").unwrap();
        let head = info.head.clone().unwrap();
        assert_eq!(head, sub_repo.rev_parse_head());
        assert_eq!(info.uncommitted, Some(2));
        assert_eq!(
            repository.submodule_gitlink_at("HEAD", "sub"),
            Some(info.index.clone())
        );
        let gitlinks = |paths: &[&str]| {
            repository.commit_gitlink_paths(&repository.head_commit().unwrap(), paths)
        };
        assert!(gitlinks(&["a.txt", "sub"]).contains("sub"));
        assert!(gitlinks(&["a.txt"]).is_empty());
        // the commits the recorded gitlink doesn't have
        let sub_repository = repository
            .submodule_repository("sub", &info.index, &head)
            .unwrap()
            .unwrap();
        let subjects: Vec<&str> = sub_repository
            .all_commits()
            .iter()
            .map(|c| c.subject.as_str())
            .collect();
        assert_eq!(subjects, ["second"]);
        // loaded once, the same repository comes back
        assert!(std::ptr::eq(
            sub_repository,
            repository
                .submodule_repository("sub", &info.index, &head)
                .unwrap()
                .unwrap()
        ));
        // the other way round has no commits, the recorded one being the older:
        assert!(repository
            .submodule_repository("sub", &head, &info.index)
            .unwrap()
            .is_none());
        // a plain path is not a submodule
        assert!(repository.submodule_info("base").is_none());
    }

    #[test]
    fn test_submodule_info_of_an_unchecked_out_submodule() {
        let dir = tempfile::tempdir().unwrap();
        let superproject = repository_with_submodule(dir.path());
        // without its `.git` file the submodule isn't checked out; git would otherwise fall back
        // to the superproject above and read its HEAD
        std::fs::remove_file(superproject.join("sub/.git")).unwrap();

        let repository =
            Repository::load(&superproject, SortCommit::Chronological, None, false).unwrap();
        let info = repository.submodule_info("sub").unwrap();
        assert_eq!(info.head, None);
        assert_eq!(info.uncommitted, None);
    }
}
