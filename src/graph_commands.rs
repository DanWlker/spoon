// The commands of the commit graph: diff, rebase, merge, checkout, cherry-pick, squash, revert and
// reset. Each works on the selected commit or the picked ones, and is checked against the
// repository before the popup confirming it opens.

use crate::{
    event::UserEvent,
    git::{CommitHash, Head, Ref, Repository},
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
    let branch = commands.current_branch();
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
            let (onto, label) = commands.ref_or_commit(&selected);
            Ok(GitDialog::rebase(
                &branch,
                onto,
                &label,
                commands.rebase_auto_stash(),
            ))
        }),
        UserEvent::Merge => single("Merge").and_then(|_| {
            let (source, label) = commands.ref_or_commit(&selected);
            if Some(&source) == branch.as_ref() {
                return Err(format!("{source} is checked out, choose another branch"));
            }
            let merge_ff = repository.config("merge.ff");
            Ok(GitDialog::merge(
                &branch_label,
                source,
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
        _ => return None,
    };
    Some(dialog.map(GraphCommand::Dialog))
}

struct GraphCommands<'a> {
    repository: &'a Repository,
}

impl GraphCommands<'_> {
    fn current_branch(&self) -> Option<String> {
        match self.repository.head() {
            Head::Branch { name } => Some(name.clone()),
            _ => None,
        }
    }

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

    // What to give git for a commit, and how to show it: a local branch at it, else a remote
    // branch, else a tag, else the commit itself.
    fn ref_or_commit(&self, hash: &CommitHash) -> (String, String) {
        let refs = self.repository.refs(hash);
        let find = |f: fn(&Ref) -> bool| refs.iter().find(|r| f(r)).map(|r| r.name().to_string());
        find(|r| matches!(r, Ref::Branch { .. }))
            .or_else(|| find(|r| matches!(r, Ref::RemoteBranch { .. })))
            .or_else(|| find(|r| matches!(r, Ref::Tag { .. })))
            .map(|name| (name.clone(), name))
            .unwrap_or_else(|| {
                let line = self.dialog_commit(hash).line;
                (hash.as_str().to_string(), line)
            })
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
        let current = self.current_branch();
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
                    let Some((_, local)) = name.split_once('/') else {
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

    // Squashing rebases from the parent of the oldest commit's parent, so the history from there
    // must be a line on the current branch.
    fn squash(&self, branch: &str, commits: Vec<CommitHash>) -> Result<GitDialog, String> {
        let repository = self.repository;
        let commits = self.sort_newest_first(commits);
        let oldest = commits.last().ok_or("No commit to squash")?;
        let short = oldest.as_short_hash();
        let parent = match repository.parents_hash(oldest)[..] {
            [] => return Err(format!("{short} is the first commit, it has no parent")),
            [parent] => parent.clone(),
            _ => return Err(format!("{short} is a merge commit, it can't be squashed")),
        };
        // the parents of a commit at the end of the loaded ones (with `--max-count`) are asked of git;
        // None when the parent is the first commit, to rebase from the root
        let base = match repository.commit(&parent) {
            Some(_) => repository
                .parents_hash(&parent)
                .first()
                .map(|h| (*h).clone()),
            None => repository.parents_of(parent.as_str())?.into_iter().next(),
        };

        // what the rebase goes through, in one call
        let rebased = repository.commits_after(base.as_ref(), "HEAD")?;
        for hash in &commits {
            let short = hash.as_short_hash();
            match rebased.get(hash).map(Vec::len) {
                None => return Err(format!("{short} is not on {branch}, check it out first")),
                Some(1) => {}
                Some(_) => return Err(format!("{short} is a merge commit, it can't be squashed")),
            }
        }
        if rebased.values().any(|parents| parents.len() > 1) {
            return Err("There are merge commits after them, which squashing would flatten".into());
        }
        let commits = commits.iter().map(|h| self.dialog_commit(h)).collect();
        let base = base.map(|h| h.as_str().to_string());
        Ok(GitDialog::squash(
            branch,
            commits,
            base,
            self.rebase_auto_stash(),
        ))
    }

    // Amends a commit on the local changes page, with its changes and message there: the last one
    // right away, an older one once a rebase stopped at it, so the commits after it must be a line
    // on the current branch.
    fn amend(&self, branch: &str, commit: &CommitHash) -> Result<GraphCommand, String> {
        let repository = self.repository;
        let short = commit.as_short_hash();
        let head = repository.head_commit().ok_or("Nothing is checked out")?;
        if *commit == head {
            return Ok(GraphCommand::AmendHead);
        }
        let base = match repository.parents_hash(commit)[..] {
            [] => None,
            [parent] => Some(parent.clone()),
            _ => return Err(format!("{short} is a merge commit, it can't be amended")),
        };
        let rebased = repository.commits_after(base.as_ref(), "HEAD")?;
        if !rebased.contains_key(commit) {
            return Err(format!("{short} is not on {branch}, check it out first"));
        }
        if rebased.values().any(|parents| parents.len() > 1) {
            return Err("There are merge commits after it, which the rebase would flatten".into());
        }
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
    use crate::{git::SortCommit, test_support::git::GitRepository};

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
            Ok(GraphCommand::Dialog(dialog)) => dialog.command().unwrap().args.join(" "),
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
        let repository = load();
        let _ = LocalChangesState::load(&repository);
        assert_eq!(output(&["log", "--format=%s"]), "c3\nc2\nc1\n");
        assert_eq!(
            output(&["show", "--format=", "--name-only", "HEAD~1"]),
            "f1\nnew\n"
        );
        assert_eq!(output(&["status", "--porcelain"]), " M f0\n");
    }
}
