// The commands of the commit graph: diff, rebase, merge, checkout, cherry-pick, squash, revert,
// reset, and rewriting commits (amend, drop). Each works on the selected commit or
// the picked ones, and is checked against the repository before the popup confirming it opens.

use rustc_hash::FxHashMap;

use crate::{
    event::UserEvent,
    git::{split_remote_branch, CommitHash, Ref, Repository},
    widget::git_dialog::{CheckoutTarget, DialogCommit, GitDialog},
};

pub enum GraphCommand {
    // a popup confirming a git command
    Dialog(GitDialog),
    // the last commit amended on the local changes page
    AmendHead,
    // the changes from `base` to `target`, shown in the details
    Compare {
        base: CommitHash,
        target: CommitHash,
        title: String,
    },
}

// The command of `event` on the `selected` commit, or the `picked` ones; None for other events.
pub fn graph_command(
    repository: &Repository,
    event: UserEvent,
    selected: CommitHash,
    picked: Vec<CommitHash>,
) -> Option<Result<GraphCommand, String>> {
    let commands = GraphCommands { repository };
    let branch = repository.current_branch();
    let branch_label = branch.clone().unwrap_or_else(|| "HEAD".into());
    let single = |name: &str| -> Result<(), String> {
        if picked.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "{name} works on the selected commit, clear the picked ones first"
            ))
        }
    };
    // the picked commits, else the selected one
    let targets = if picked.is_empty() {
        vec![selected.clone()]
    } else {
        picked.clone()
    };
    let dialog = match event {
        UserEvent::Diff => return Some(commands.compare(selected, picked)),
        UserEvent::Rebase => single("Rebase").and_then(|_| {
            let branch = branch.ok_or("Check out a branch to rebase it")?;
            commands.not_head(&selected, "rebase onto")?;
            let (target, label) = commands.name_of(&selected);
            Ok(GitDialog::rebase(
                &branch,
                target,
                &label,
                commands.rebase_auto_stash(),
            ))
        }),
        UserEvent::Merge => single("Merge").and_then(|_| {
            commands.not_head(&selected, "merge")?;
            let merge_ff = repository.config("merge.ff");
            let (target, label) = commands.name_of(&selected);
            Ok(GitDialog::merge(
                &branch_label,
                target,
                &label,
                merge_ff.as_deref(),
            ))
        }),
        UserEvent::Checkout => {
            single("Checkout").map(|_| GitDialog::checkout(commands.checkout_targets(&selected)))
        }
        UserEvent::CherryPick => {
            let mut commits = commands.sort_newest_first(targets);
            commits.reverse();
            let commits = commits.iter().map(|h| commands.dialog_commit(h)).collect();
            Ok(GitDialog::cherry_pick(&branch_label, commits))
        }
        UserEvent::Squash => commands.squash(&branch_label, targets),
        UserEvent::Revert => {
            let commits = commands.sort_newest_first(targets);
            let commits = commits.iter().map(|h| commands.dialog_commit(h)).collect();
            Ok(GitDialog::revert(&branch_label, commits))
        }
        UserEvent::AmendCommit => {
            return Some(single("Amend").and_then(|_| commands.amend(&branch_label, &selected)))
        }
        UserEvent::Reset => single("Reset")
            .map(|_| GitDialog::reset(&branch_label, commands.dialog_commit(&selected))),
        UserEvent::DropCommit => commands.drop(&branch_label, targets),
        UserEvent::CreateTag => {
            single("Tag").map(|_| GitDialog::create_tag(selected.as_short_hash()))
        }
        UserEvent::CreateBranch => {
            single("Branch").map(|_| create_branch(repository, selected.as_short_hash()))
        }
        _ => return None,
    };
    Some(dialog.map(GraphCommand::Dialog))
}

// The command of `event` on a branch, remote branch or tag selected in the refs sidebar: rebase
// and merge name the ref, checkout offers it first; the others work on the commit it points at.
pub fn ref_graph_command(
    repository: &Repository,
    event: UserEvent,
    name: &str,
    target: CommitHash,
) -> Option<Result<GraphCommand, String>> {
    let commands = GraphCommands { repository };
    let branch = repository.current_branch();
    let checked_out = || -> Result<(), String> {
        if branch.as_deref() == Some(name) {
            Err(format!("{name} is checked out, choose another branch"))
        } else {
            Ok(())
        }
    };
    let dialog = match event {
        UserEvent::Rebase => checked_out().and_then(|_| {
            let branch = branch.clone().ok_or("Check out a branch to rebase it")?;
            Ok(GitDialog::rebase(
                &branch,
                name.to_string(),
                name,
                commands.rebase_auto_stash(),
            ))
        }),
        UserEvent::Merge => checked_out().map(|_| {
            let branch_label = branch.clone().unwrap_or_else(|| "HEAD".into());
            let merge_ff = repository.config("merge.ff");
            GitDialog::merge(&branch_label, name.to_string(), name, merge_ff.as_deref())
        }),
        UserEvent::Checkout => checked_out().map(|_| {
            let mut targets = commands.checkout_targets(&target);
            match targets
                .iter()
                .position(|t| t.args.last().map(String::as_str) == Some(name))
            {
                Some(i) => {
                    let ours = targets.remove(i);
                    targets.insert(0, ours);
                }
                // a tag, checked out detached in place of its commit
                None => {
                    targets.retain(|t| t.args.first().map(String::as_str) != Some("--detach"));
                    targets.insert(
                        0,
                        CheckoutTarget {
                            label: format!("{name} (detached HEAD)"),
                            args: vec!["--detach".into(), name.to_string()],
                        },
                    );
                }
            }
            GitDialog::checkout(targets)
        }),
        // from the name, so a branch made from a remote branch tracks it
        UserEvent::CreateBranch => Ok(create_branch(repository, name)),
        UserEvent::CreateTag => Ok(GitDialog::create_tag(name)),
        _ => return graph_command(repository, event, target, vec![]),
    };
    Some(dialog.map(GraphCommand::Dialog))
}

// A new branch from `from`, or another local branch.
fn create_branch(repository: &Repository, from: &str) -> GitDialog {
    let mut branches: Vec<String> = repository
        .all_refs()
        .into_iter()
        .filter(|r| matches!(r, Ref::Branch { .. }))
        .map(|r| r.name().to_string())
        .collect();
    branches.sort();
    GitDialog::create_branch(from, branches)
}

struct GraphCommands<'a> {
    repository: &'a Repository,
}

impl GraphCommands<'_> {
    // The commit as a line in a popup: its short hash and subject.
    fn dialog_commit(&self, hash: &CommitHash) -> DialogCommit {
        let subject = self
            .repository
            .commit(hash)
            .map(|c| c.subject.clone())
            .unwrap_or_default();
        DialogCommit {
            hash: hash.as_str().to_string(),
            line: format!("{} {subject}", hash.as_short_hash()),
        }
    }

    // The commit to merge or rebase onto, and its label: by the name of the one local branch at
    // it, for "Merge branch 'x'" as the message, else by its hash; by the hash too when a tag has
    // that name, which git would take instead.
    fn name_of(&self, hash: &CommitHash) -> (String, String) {
        let mut branches = self
            .repository
            .refs(hash)
            .into_iter()
            .filter(|r| matches!(r, Ref::Branch { .. }));
        let tagged = |name: &str| {
            self.repository
                .all_refs()
                .iter()
                .any(|r| matches!(r, Ref::Tag { .. }) && r.name() == name)
        };
        match (branches.next(), branches.next()) {
            (Some(branch), None) if !tagged(branch.name()) => {
                (branch.name().to_string(), branch.name().to_string())
            }
            _ => (hash.as_str().to_string(), hash.as_short_hash().to_string()),
        }
    }

    // An error when `hash` is the checked-out commit, which there is nothing to `what`.
    fn not_head(&self, hash: &CommitHash, what: &str) -> Result<(), String> {
        if self.repository.head_commit().as_ref() == Some(hash) {
            return Err(format!(
                "The selected commit is checked out, choose another one to {what}"
            ));
        }
        Ok(())
    }

    // Commits ordered as in the graph, newest first.
    fn sort_newest_first(&self, mut hashes: Vec<CommitHash>) -> Vec<CommitHash> {
        self.repository.sort_newest_first(&mut hashes);
        hashes
    }

    // Whether rebasing stashes local changes by default: rebase.autoStash, on when it is not set.
    fn rebase_auto_stash(&self) -> bool {
        self.repository
            .config_bool("rebase.autoStash")
            .unwrap_or(true)
    }

    // The branches at the commit to check out, and the commit itself.
    fn checkout_targets(&self, hash: &CommitHash) -> Vec<CheckoutTarget> {
        let current = self.repository.current_branch();
        let all_refs = self.repository.all_refs();
        let local_exists = |name: &str| {
            all_refs
                .iter()
                .any(|r| matches!(r, Ref::Branch { .. }) && r.name() == name)
        };
        let mut targets = vec![];
        for r in self.repository.refs(hash) {
            match r {
                Ref::Branch { name, .. } if Some(name) != current.as_ref() => {
                    targets.push(CheckoutTarget {
                        label: name.clone(),
                        args: vec![name.clone()],
                    });
                }
                Ref::RemoteBranch { name, .. } => {
                    // a new local branch tracking it, unless there is one by that name
                    let Some((_, local)) = split_remote_branch(self.repository.remotes(), name)
                    else {
                        continue;
                    };
                    if local != "HEAD" && !local_exists(local) {
                        targets.push(CheckoutTarget {
                            label: format!("{local} (new, tracking {name})"),
                            args: vec!["--track".into(), name.clone()],
                        });
                    }
                }
                _ => {}
            }
        }
        targets.push(CheckoutTarget {
            label: format!("{} (detached HEAD)", hash.as_short_hash()),
            args: vec!["--detach".into(), hash.as_str().into()],
        });
        targets
    }

    // The parents of a commit, asked of git when it is not loaded (with `--max-count`).
    fn parents(&self, hash: &CommitHash) -> Result<Vec<CommitHash>, String> {
        match self.repository.commit(hash) {
            Some(_) => Ok(self
                .repository
                .parents_hash(hash)
                .into_iter()
                .cloned()
                .collect()),
            None => self.repository.parents_of(hash.as_str()),
        }
    }

    // The parent of a commit to rewrite, None for the first commit; `what` it can't be when it is
    // a merge commit, like "squashed".
    fn single_parent(&self, hash: &CommitHash, what: &str) -> Result<Option<CommitHash>, String> {
        match &self.parents(hash)?[..] {
            [] => Ok(None),
            [parent] => Ok(Some(parent.clone())),
            _ => Err(format!(
                "{} is a merge commit, it can't be {what}",
                hash.as_short_hash()
            )),
        }
    }

    // The commits a rebase from after `base` (from the root when None) goes through, with their
    // parents, once checked that `commits` are on the current branch and none of them are merges,
    // which the rebase would flatten.
    fn rebased(
        &self,
        branch: &str,
        base: Option<&CommitHash>,
        commits: &[CommitHash],
    ) -> Result<FxHashMap<CommitHash, Vec<CommitHash>>, String> {
        let rebased = self.repository.commits_after(base, "HEAD")?;
        if let Some(missing) = commits.iter().find(|h| !rebased.contains_key(*h)) {
            let short = missing.as_short_hash();
            return Err(format!("{short} is not on {branch}, check it out first"));
        }
        if rebased.values().any(|parents| parents.len() > 1) {
            return Err("There are merge commits after it, which the rebase would flatten".into());
        }
        Ok(rebased)
    }

    // The commit `n` commits before HEAD, to select once the history is rewritten.
    fn select_back(n: usize) -> Option<String> {
        Some(format!("HEAD~{n}"))
    }

    // Squashing rebases from the parent of the oldest commit's parent, so the history from there
    // must be a line on the current branch.
    fn squash(&self, branch: &str, commits: Vec<CommitHash>) -> Result<GitDialog, String> {
        let commits = self.sort_newest_first(commits);
        let oldest = commits.last().ok_or("No commit to squash")?;
        let short = oldest.as_short_hash();
        let parent = self
            .single_parent(oldest, "squashed")?
            .ok_or_else(|| format!("{short} is the first commit, it has no parent"))?;
        // None when the parent is the first commit, to rebase from the root
        let base = self.parents(&parent)?.into_iter().next();
        self.rebased(branch, base.as_ref(), &commits)?;
        let commits = commits.iter().map(|h| self.dialog_commit(h)).collect();
        let base = base.map(|h| h.as_str().to_string());
        Ok(GitDialog::squash(
            branch,
            commits,
            base,
            self.rebase_auto_stash(),
        ))
    }

    // Drops the commits, rebasing from the parent of the oldest one.
    fn drop(&self, branch: &str, commits: Vec<CommitHash>) -> Result<GitDialog, String> {
        let commits = self.sort_newest_first(commits);
        let (Some(newest), Some(oldest)) = (commits.first(), commits.last()) else {
            return Err("No commit to drop".into());
        };
        let base = self.single_parent(oldest, "dropped")?;
        self.rebased(branch, base.as_ref(), &commits)?;
        for hash in &commits {
            self.single_parent(hash, "dropped")?;
        }
        // the commit after the newest one takes its place
        let after = self.repository.count_after(newest, "HEAD")?;
        let select = Self::select_back(after.saturating_sub(1));
        let commits = commits.iter().map(|h| self.dialog_commit(h)).collect();
        let base = base.map(|h| h.as_str().to_string());
        let dialog = GitDialog::drop(branch, commits, base, self.rebase_auto_stash());
        Ok(dialog.selecting(select))
    }

    // Amends a commit on the local changes page, with its changes and message there: the last one
    // right away, an older one once a rebase stopped at it, so the commits after it must be a line
    // on the current branch.
    fn amend(&self, branch: &str, commit: &CommitHash) -> Result<GraphCommand, String> {
        let head = self
            .repository
            .head_commit()
            .ok_or("Nothing is checked out")?;
        if *commit == head {
            return Ok(GraphCommand::AmendHead);
        }
        let base = self.single_parent(commit, "amended")?;
        self.rebased(branch, base.as_ref(), std::slice::from_ref(commit))?;
        let base = base.map(|h| h.as_str().to_string());
        let dialog = GitDialog::edit(
            branch,
            self.dialog_commit(commit),
            base,
            self.rebase_auto_stash(),
        );
        Ok(GraphCommand::Dialog(dialog))
    }

    // The changes between two commits: the checked out commit and the selected (or picked) one,
    // or the two picked ones; the older one is the base.
    fn compare(
        &self,
        selected: CommitHash,
        picked: Vec<CommitHash>,
    ) -> Result<GraphCommand, String> {
        let head = || {
            self.repository
                .head_commit()
                .ok_or("Nothing is checked out")
        };
        let (a, b) = match picked.as_slice() {
            [] => (head()?, selected),
            [one] => (head()?, one.clone()),
            [one, two] => (one.clone(), two.clone()),
            more => {
                return Err(format!(
                    "Diff compares two commits, {} are picked",
                    more.len()
                ))
            }
        };
        if a == b {
            return Err("That commit is checked out, there is nothing to compare".into());
        }
        let [target, base]: [CommitHash; 2] = self
            .sort_newest_first(vec![a, b])
            .try_into()
            .expect("two commits");
        let title = format!("Diff {} → {}", base.as_short_hash(), target.as_short_hash());
        Ok(GraphCommand::Compare {
            base,
            target,
            title,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        git::{GitTask, SortCommit},
        test_support::git::GitRepository,
    };

    fn command(
        repository: &Repository,
        event: UserEvent,
        selected: &CommitHash,
        picked: &[CommitHash],
    ) -> Result<GraphCommand, String> {
        graph_command(repository, event, selected.clone(), picked.to_vec()).unwrap()
    }

    fn args(command: Result<GraphCommand, String>) -> String {
        match command {
            Ok(GraphCommand::Dialog(dialog)) => dialog.command().unwrap().commands[0].join(" "),
            Ok(GraphCommand::Compare { title, .. }) => title,
            Ok(GraphCommand::AmendHead) => "amend head".into(),
            Err(err) => err,
        }
    }

    #[test]
    fn test_graph_commands() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let git = GitRepository::new(path);
        git.init();
        for (i, date) in ["2024-01-01", "2024-01-02", "2024-01-03"]
            .iter()
            .enumerate()
        {
            std::fs::write(path.join("f"), format!("{i}\n")).unwrap();
            git.add(&["f"]);
            git.commit(&format!("c{}", i + 1), date);
        }
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let hashes: Vec<CommitHash> = repository
            .all_commits()
            .iter()
            .map(|c| c.commit_hash.clone())
            .collect();
        let [c3, c2, c1] = [&hashes[0], &hashes[1], &hashes[2]];

        // squashing c3 into c2 rebases after c1; c2 into c1 rebases from the root
        let squash = args(command(&repository, UserEvent::Squash, c3, &[]));
        assert_eq!(squash, format!("rebase -i --autostash {}", c1.as_str()));
        let squash = args(command(&repository, UserEvent::Squash, c2, &[]));
        assert_eq!(squash, "rebase -i --autostash --root");
        let squash = args(command(&repository, UserEvent::Squash, c1, &[]));
        assert!(squash.contains("first commit"), "{squash}");

        // the older commit is the base, picks first
        let diff = args(command(&repository, UserEvent::Diff, c1, &[]));
        assert_eq!(
            diff,
            format!("Diff {} → {}", c1.as_short_hash(), c3.as_short_hash())
        );
        let diff = args(command(
            &repository,
            UserEvent::Diff,
            c3,
            &[c2.clone(), c1.clone()],
        ));
        assert_eq!(
            diff,
            format!("Diff {} → {}", c1.as_short_hash(), c2.as_short_hash())
        );
        let diff = args(command(&repository, UserEvent::Diff, c3, &hashes));
        assert_eq!(diff, "Diff compares two commits, 3 are picked");

        // rebase and merge name the commit by its hash, without one branch at it
        git.checkout_b("other");
        let rebase = args(command(&repository, UserEvent::Rebase, c1, &[]));
        assert_eq!(rebase, format!("rebase --autostash {}", c1.as_str()));
        let merge = args(command(&repository, UserEvent::Merge, c1, &[]));
        assert!(merge.ends_with(c1.as_str()), "{merge}");
        let merge = args(command(&repository, UserEvent::Merge, c3, &[]));
        assert!(merge.contains("is checked out"), "{merge}");
        // with one, by its name, for "Merge branch 'topic'" as the message
        let git_in = |git_args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(git_args)
                .current_dir(path)
                .status()
                .unwrap();
            assert!(status.success());
            Repository::load(path, SortCommit::Chronological, None, false).unwrap()
        };
        let reloaded = git_in(&["branch", "topic", c1.as_str()]);
        let merge = args(command(&reloaded, UserEvent::Merge, c1, &[]));
        assert!(merge.ends_with(" topic"), "{merge}");
        let rebase = args(command(&reloaded, UserEvent::Rebase, c1, &[]));
        assert_eq!(rebase, "rebase --autostash topic");
        // a tag of the same name would be merged instead, so the hash is
        let reloaded = git_in(&["tag", "topic", c2.as_str()]);
        let merge = args(command(&reloaded, UserEvent::Merge, c1, &[]));
        assert!(merge.ends_with(c1.as_str()), "{merge}");
        let rebase = args(command(&reloaded, UserEvent::Rebase, c1, &[]));
        assert_eq!(rebase, format!("rebase --autostash {}", c1.as_str()));

        // commands on the selected commit refuse picked ones
        let reset = args(command(
            &repository,
            UserEvent::Reset,
            c3,
            std::slice::from_ref(c2),
        ));
        assert!(reset.contains("clear the picked ones"), "{reset}");
        assert!(graph_command(&repository, UserEvent::Refresh, c3.clone(), vec![]).is_none());
    }

    #[test]
    fn test_amend_commit() {
        use crate::widget::local_changes::LocalChangesState;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let git = GitRepository::new(path);
        git.init();
        for (i, date) in ["2024-01-01", "2024-01-02", "2024-01-03"]
            .iter()
            .enumerate()
        {
            std::fs::write(path.join(format!("f{i}")), "x\n").unwrap();
            git.add(&[&format!("f{i}")]);
            git.commit(&format!("c{}", i + 1), date);
        }
        let output = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(path)
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap()
        };
        let load = || Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let repository = load();
        let hashes: Vec<CommitHash> = repository
            .all_commits()
            .iter()
            .map(|c| c.commit_hash.clone())
            .collect();

        // the last commit is amended on the local changes page right away
        let head = command(&repository, UserEvent::AmendCommit, &hashes[0], &[]);
        assert!(matches!(head, Ok(GraphCommand::AmendHead)));

        // an older one, with nothing staged and a change kept aside: the rebase stops at it
        std::fs::write(path.join("f0"), "changed\n").unwrap();
        let Ok(GraphCommand::Dialog(dialog)) =
            command(&repository, UserEvent::AmendCommit, &hashes[1], &[])
        else {
            panic!("no dialog");
        };
        crate::git::run_task(path, &dialog.command().unwrap()).unwrap();
        assert_eq!(output(&["log", "-1", "--format=%s"]), "c2\n");

        // the page amends it: its message and changes, with a new file and message
        let repository = load();
        let mut state = LocalChangesState::load(&repository);
        assert_eq!(state.snapshot().commit_draft_message(), "c2");
        std::fs::write(path.join("new"), "n\n").unwrap();
        git.add(&["new"]);
        state.commit_or_continue().unwrap();
        state.confirm().unwrap();
        crate::git::run_task(path, &state.take_task().unwrap()).unwrap();
        let repository = load();
        let _ = LocalChangesState::load(&repository);
        assert_eq!(output(&["log", "--format=%s"]), "c3\nc2\nc1\n");
        assert_eq!(
            output(&["show", "--format=", "--name-only", "HEAD~1"]),
            "f1\nnew\n"
        );
        assert_eq!(output(&["status", "--porcelain"]), " M f0\n");
    }

    // Three commits c1..c3 on main, each adding a file f0..f2, and a way to run git.
    fn rewrite_repo() -> (tempfile::TempDir, impl Fn(&[&str]) -> String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let git = GitRepository::new(&path);
        git.init();
        for (i, date) in ["2024-01-01", "2024-01-02", "2024-01-03"]
            .iter()
            .enumerate()
        {
            std::fs::write(path.join(format!("f{i}")), "x\n").unwrap();
            git.add(&[&format!("f{i}")]);
            git.commit(&format!("c{}", i + 1), date);
        }
        let output = move |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&path)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
            String::from_utf8(out.stdout).unwrap()
        };
        (dir, output)
    }

    fn load(path: &std::path::Path) -> (Repository, Vec<CommitHash>) {
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let hashes = repository
            .all_commits()
            .iter()
            .map(|c| c.commit_hash.clone())
            .collect();
        (repository, hashes)
    }

    // Runs the command of the popup or the one run right away, with the text field typed.
    fn run(path: &std::path::Path, command: Result<GraphCommand, String>, typed: &str) -> GitTask {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let task = match command {
            Ok(GraphCommand::Dialog(mut dialog)) => {
                for c in typed.chars() {
                    let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
                    dialog.handle_key(None, key);
                }
                dialog.command().unwrap()
            }
            _ => panic!("no command"),
        };
        crate::git::run_task(path, &task).unwrap();
        task
    }

    #[test]
    fn test_drop() {
        let (dir, git) = rewrite_repo();
        let path = dir.path();
        let (repository, hashes) = load(path);
        std::fs::write(path.join("f0"), "changed\n").unwrap();

        // c2 and its file go, the commits after it are rebased onto c1, the one after is selected
        let task = run(
            path,
            command(&repository, UserEvent::DropCommit, &hashes[1], &[]),
            "",
        );
        assert_eq!(task.select.as_deref(), Some("HEAD~0"));
        assert_eq!(git(&["log", "--format=%s"]), "c3\nc1\n");
        assert_eq!(git(&["ls-files"]), "f0\nf2\n");
        assert_eq!(git(&["status", "--porcelain"]), " M f0\n");
    }

    #[test]
    fn test_ref_graph_command() {
        let (dir, git) = rewrite_repo();
        let path = dir.path();
        git(&["branch", "topic", "HEAD~1"]);
        git(&["tag", "v1", "HEAD~2"]);
        let (repository, hashes) = load(path);
        let on = |event, name: &str, target: &CommitHash| {
            args(ref_graph_command(&repository, event, name, target.clone()).unwrap())
        };

        // rebase and merge name the branch
        assert_eq!(
            on(UserEvent::Rebase, "topic", &hashes[1]),
            "rebase --autostash topic"
        );
        assert!(on(UserEvent::Merge, "topic", &hashes[1]).ends_with(" topic"));
        assert!(on(UserEvent::Merge, "master", &hashes[0]).contains("is checked out"));
        // checkout offers the branch first, a tag detached
        assert_eq!(
            on(UserEvent::Checkout, "topic", &hashes[1]),
            "checkout topic"
        );
        assert_eq!(
            on(UserEvent::Checkout, "v1", &hashes[2]),
            "checkout --detach v1"
        );
        // a tag at a commit of the graph, and at a ref of the sidebar
        let tag = command(&repository, UserEvent::CreateTag, &hashes[1], &[]);
        run(path, tag, "t1");
        assert_eq!(git(&["tag", "--points-at", "topic"]), "t1\n");
        let tag = ref_graph_command(
            &repository,
            UserEvent::CreateTag,
            "topic",
            hashes[1].clone(),
        );
        assert_eq!(
            run(path, tag.unwrap(), "t2").commands,
            [["tag", "t2", "topic"]]
        );

        // a branch at a commit of the graph, checked out
        let branch = command(&repository, UserEvent::CreateBranch, &hashes[2], &[]);
        run(path, branch, "b1");
        assert_eq!(git(&["branch", "--show-current"]), "b1\n");
        assert_eq!(git(&["rev-parse", "HEAD"]).trim(), hashes[2].as_str());
    }

    #[test]
    fn test_drop_picked() {
        let (dir, git) = rewrite_repo();
        let path = dir.path();
        let (repository, hashes) = load(path);

        // the picked c3 and c1 go, c2 is rebased onto the root and selected
        let picked = [hashes[0].clone(), hashes[2].clone()];
        let task = run(
            path,
            command(&repository, UserEvent::DropCommit, &hashes[1], &picked),
            "",
        );
        assert_eq!(task.select.as_deref(), Some("HEAD~0"));
        assert_eq!(git(&["log", "--format=%s"]), "c2\n");
        assert_eq!(git(&["ls-files"]), "f1\n");
    }

    #[test]
    fn test_fixup() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (dir, git) = rewrite_repo();
        let path = dir.path();
        let (repository, hashes) = load(path);
        let Ok(GraphCommand::Dialog(mut dialog)) =
            command(&repository, UserEvent::Squash, &hashes[0], &[])
        else {
            panic!("no dialog");
        };
        // the messages: combined, or the parent's
        dialog.handle_key(
            Some(UserEvent::DialogToggle),
            KeyEvent::new(KeyCode::Null, KeyModifiers::NONE),
        );
        crate::git::run_task(path, &dialog.command().unwrap()).unwrap();
        assert_eq!(git(&["log", "--format=%B"]), "c2\n\nc1\n\n");
        assert_eq!(git(&["ls-files"]), "f0\nf1\nf2\n");
    }
}
