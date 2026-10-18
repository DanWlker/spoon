// The commands of the commit graph: diff, rebase, merge, checkout, cherry-pick, squash, revert,
// reset, and rewriting commits (amend, drop). Each works on the selected commit or
// the picked ones, and is checked against the repository before the popup confirming it opens.

use rustc_hash::FxHashMap;

use crate::{
    event::UserEvent,
    git::{split_remote_branch, CommitHash, Conflict, NamedRef, Ref, Repository, Rev},
    view::Compare,
    widget::git_dialog::{CheckoutTarget, DialogCommit, GitDialog, StartPoint},
};

pub enum GraphCommand {
    // a popup confirming a git command
    Dialog(GitDialog),
    // the last commit amended on the local changes page
    AmendHead,
    // the changes from one commit to another, shown in the details
    Compare(Compare),
}

// The command of `event` on the `selected` commit, or the `picked` ones; None for other events.
pub fn graph_command(
    repository: &Repository,
    event: UserEvent,
    selected: CommitHash,
    picked: Vec<CommitHash>,
) -> Option<Result<GraphCommand, String>> {
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
    // git needs to be told which parent the changes of a merge are from
    let applied = match event {
        UserEvent::CherryPick => Some("cherry-picked"),
        UserEvent::Revert => Some("reverted"),
        _ => None,
    };
    if let Some(what) = applied {
        if let Err(err) = targets
            .iter()
            .try_for_each(|hash| single_parent(repository, hash, what).map(drop))
        {
            return Some(Err(err));
        }
    }
    let dialog = match event {
        UserEvent::Diff => return Some(compare(repository, selected, picked)),
        UserEvent::Rebase => single("Rebase").and_then(|_| {
            let branch = branch.ok_or("Check out a branch to rebase it")?;
            not_head(repository, &selected, "rebase onto")?;
            let (target, label) = name_of(repository, &selected);
            let conflict = rebase_conflict(repository, &target);
            Ok(
                GitDialog::rebase(&branch, target, &label, rebase_auto_stash(repository))
                    .conflicting(conflict),
            )
        }),
        UserEvent::Merge => single("Merge").and_then(|_| {
            not_head(repository, &selected, "merge")?;
            let merge_ff = repository.config("merge.ff");
            let (target, label) = name_of(repository, &selected);
            let conflict = merge_conflict(repository, &target);
            Ok(
                GitDialog::merge(&branch_label, target, &label, merge_ff.as_deref())
                    .conflicting(conflict),
            )
        }),
        UserEvent::Checkout => {
            single("Checkout").map(|_| GitDialog::checkout(checkout_targets(repository, &selected)))
        }
        UserEvent::CherryPick => {
            let mut commits = sort_newest_first(repository, targets);
            commits.reverse();
            let steps: Vec<_> = commits
                .iter()
                .map(|h| (Some(format!("{}^", h.as_str())), h.as_str().to_string()))
                .collect();
            let conflict = repository.first_conflict("HEAD", &steps);
            let commits = commits
                .iter()
                .map(|h| dialog_commit(repository, h))
                .collect();
            Ok(GitDialog::cherry_pick(&branch_label, commits).conflicting(conflict))
        }
        UserEvent::Squash => squash(repository, &branch_label, targets),
        UserEvent::Revert => {
            let commits = sort_newest_first(repository, targets);
            // undoing a commit is applying the changes from it back to its parent
            let steps: Vec<_> = commits
                .iter()
                .map(|h| (Some(h.as_str().to_string()), format!("{}^", h.as_str())))
                .collect();
            let conflict = repository.first_conflict("HEAD", &steps);
            let commits = commits
                .iter()
                .map(|h| dialog_commit(repository, h))
                .collect();
            Ok(GitDialog::revert(&branch_label, commits).conflicting(conflict))
        }
        UserEvent::AmendCommit => {
            return Some(single("Amend").and_then(|_| amend(repository, &branch_label, &selected)))
        }
        UserEvent::Reset => single("Reset")
            .map(|_| GitDialog::reset(&branch_label, dialog_commit(repository, &selected))),
        UserEvent::DropCommit => drop_commits(repository, &branch_label, targets),
        UserEvent::CreateTag => {
            single("Tag").map(|_| GitDialog::create_tag(StartPoint::Commit(selected.clone())))
        }
        UserEvent::CreateBranch => {
            single("Branch").map(|_| GitDialog::create_branch_at(selected.clone()))
        }
        _ => return None,
    };
    Some(dialog.map(GraphCommand::Dialog))
}

// The command of `event` on a branch, remote branch or tag selected in the refs sidebar: rebase
// and merge name the ref (in full when another has its name, which git could take instead),
// checkout offers it first; the others work on the commit it points at.
pub fn ref_graph_command(
    repository: &Repository,
    event: UserEvent,
    selected: &NamedRef,
    target: CommitHash,
) -> Option<Result<GraphCommand, String>> {
    let name = selected.name();
    let shared = repository
        .all_refs()
        .iter()
        .filter(|r| r.name() == name)
        .count()
        > 1;
    let onto = if shared {
        selected.full_name()
    } else {
        name.to_string()
    };
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
            let conflict = rebase_conflict(repository, &onto);
            Ok(
                GitDialog::rebase(&branch, onto, name, rebase_auto_stash(repository))
                    .conflicting(conflict),
            )
        }),
        UserEvent::Merge => checked_out().map(|_| {
            let branch_label = branch.clone().unwrap_or_else(|| "HEAD".into());
            let merge_ff = repository.config("merge.ff");
            let conflict = merge_conflict(repository, &onto);
            GitDialog::merge(&branch_label, onto, name, merge_ff.as_deref()).conflicting(conflict)
        }),
        UserEvent::Checkout => checked_out().map(|_| {
            let mut targets = checkout_targets(repository, &target);
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
        UserEvent::CreateBranch => Ok(create_branch(repository, selected.clone())),
        UserEvent::CreateTag => Ok(GitDialog::create_tag(StartPoint::Ref(selected.clone()))),
        _ => return graph_command(repository, event, target, vec![]),
    };
    Some(dialog.map(GraphCommand::Dialog))
}

// A new branch from `from`, or another local branch chosen instead.
fn create_branch(repository: &Repository, from: NamedRef) -> GitDialog {
    let mut branches: Vec<String> = repository
        .all_refs()
        .into_iter()
        .filter(|r| matches!(r, Ref::Branch { .. }))
        .map(|r| r.name().to_string())
        .collect();
    branches.sort();
    GitDialog::create_branch_from(from, branches)
}

// The commit as a line in a popup: its short hash and subject.
pub fn dialog_commit(repository: &Repository, hash: &CommitHash) -> DialogCommit {
    let subject = repository
        .commit(hash)
        .map(|c| c.subject.clone())
        .unwrap_or_default();
    DialogCommit {
        hash: hash.clone(),
        line: format!("{} {subject}", hash.as_short_hash()),
    }
}

// The commit to merge or rebase onto, and its label: by the name of the one local branch at
// it, for "Merge branch 'x'" as the message, else by its hash; by the hash too when a tag has
// that name, which git would take instead.
fn name_of(repository: &Repository, hash: &CommitHash) -> (String, String) {
    let mut branches = repository
        .refs(hash)
        .into_iter()
        .filter(|r| matches!(r, Ref::Branch { .. }));
    let tagged = |name: &str| {
        repository
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
fn not_head(repository: &Repository, hash: &CommitHash, what: &str) -> Result<(), String> {
    if repository.head_commit().as_ref() == Some(hash) {
        return Err(format!(
            "The selected commit is checked out, choose another one to {what}"
        ));
    }
    Ok(())
}

// Commits ordered as in the graph, newest first.
fn sort_newest_first(repository: &Repository, mut hashes: Vec<CommitHash>) -> Vec<CommitHash> {
    repository.sort_newest_first(&mut hashes);
    hashes
}

// The conflicts rebasing the current branch onto `onto` stops at: the commits on it and not on
// `onto`, without those already there, like git rebase.
fn rebase_conflict(repository: &Repository, onto: &str) -> Option<Conflict> {
    let range = format!("{onto}...HEAD");
    repository.replay_conflict(onto, &["--right-only", "--cherry-pick", &range], &[])
}

// The conflicts merging `source` into HEAD runs into.
fn merge_conflict(repository: &Repository, source: &str) -> Option<Conflict> {
    repository.first_conflict("HEAD", &[(None, source.to_string())])
}

// Whether rebasing stashes local changes by default: rebase.autoStash, on when it is not set.
fn rebase_auto_stash(repository: &Repository) -> bool {
    repository.config_bool("rebase.autoStash").unwrap_or(true)
}

// The branches at the commit to check out, and the commit itself.
fn checkout_targets(repository: &Repository, hash: &CommitHash) -> Vec<CheckoutTarget> {
    let current = repository.current_branch();
    let all_refs = repository.all_refs();
    let local_exists = |name: &str| {
        all_refs
            .iter()
            .any(|r| matches!(r, Ref::Branch { .. }) && r.name() == name)
    };
    let mut targets = vec![];
    for r in repository.refs(hash) {
        match r {
            Ref::Branch { name, .. } if Some(name) != current.as_ref() => {
                targets.push(CheckoutTarget {
                    label: name.clone(),
                    args: vec![name.clone()],
                });
            }
            Ref::RemoteBranch { name, .. } => {
                // a new local branch tracking it, unless there is one by that name
                let Some((_, local)) = split_remote_branch(repository.remotes(), name) else {
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
fn parents(repository: &Repository, hash: &CommitHash) -> Result<Vec<CommitHash>, String> {
    match repository.commit(hash) {
        Some(_) => Ok(repository.parents_hash(hash).into_iter().cloned().collect()),
        None => repository.parents_of(hash.as_str()),
    }
}

// The parent of a commit to rewrite, None for the first commit; `what` it can't be when it is
// a merge commit, like "squashed".
fn single_parent(
    repository: &Repository,
    hash: &CommitHash,
    what: &str,
) -> Result<Option<CommitHash>, String> {
    match &parents(repository, hash)?[..] {
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
    repository: &Repository,
    branch: &str,
    base: Option<&CommitHash>,
    commits: &[CommitHash],
) -> Result<FxHashMap<CommitHash, Vec<CommitHash>>, String> {
    let rebased = repository.commits_after(base, "HEAD")?;
    if let Some(missing) = commits.iter().find(|h| !rebased.contains_key(*h)) {
        let short = missing.as_short_hash();
        return Err(format!("{short} is not on {branch}, check it out first"));
    }
    if rebased.values().any(|parents| parents.len() > 1) {
        return Err("There are merge commits after it, which the rebase would flatten".into());
    }
    Ok(rebased)
}

// Squashing rebases from the parent of the oldest commit's parent, so the history from there
// must be a line on the current branch.
fn squash(
    repository: &Repository,
    branch: &str,
    commits: Vec<CommitHash>,
) -> Result<GitDialog, String> {
    let commits = sort_newest_first(repository, commits);
    let oldest = commits.last().ok_or("No commit to squash")?;
    let short = oldest.as_short_hash();
    let parent = single_parent(repository, oldest, "squashed")?
        .ok_or_else(|| format!("{short} is the first commit, it has no parent"))?;
    // None when the parent is the first commit, to rebase from the root
    let base = parents(repository, &parent)?.into_iter().next();
    rebased(repository, branch, base.as_ref(), &commits)?;
    let commits = commits
        .iter()
        .map(|h| dialog_commit(repository, h))
        .collect();
    let base = base.map(|h| h.as_str().to_string());
    // the parent is squashed into too, so it is rewritten as well
    let pushed = repository.upstream_containing(parent.as_str());
    Ok(GitDialog::squash(branch, commits, base, rebase_auto_stash(repository)).pushed(pushed))
}

// Drops the commits, rebasing from the parent of the oldest one.
fn drop_commits(
    repository: &Repository,
    branch: &str,
    commits: Vec<CommitHash>,
) -> Result<GitDialog, String> {
    let commits = sort_newest_first(repository, commits);
    // never empty: the picked commits, else the selected one
    let (newest, oldest) = (&commits[0], &commits[commits.len() - 1]);
    let base = single_parent(repository, oldest, "dropped")?;
    rebased(repository, branch, base.as_ref(), &commits)?;
    for hash in &commits {
        single_parent(repository, hash, "dropped")?;
    }
    // the commit after the newest one takes its place, under a new hash
    let after = repository.count_after(newest, "HEAD")?;
    let select = Some(format!("HEAD~{}", after.saturating_sub(1)));
    let dropped = commits.clone();
    let commits = commits
        .iter()
        .map(|h| dialog_commit(repository, h))
        .collect();
    let base = base.map(|h| h.as_str().to_string());
    // the commits after them are replayed without them; not checked from the root
    let conflict = base.as_ref().and_then(|base| {
        let range = format!("{base}..HEAD");
        repository.replay_conflict(base, &[&range], &dropped)
    });
    let pushed = repository.upstream_containing(oldest.as_str());
    let dialog = GitDialog::drop(branch, commits, base, rebase_auto_stash(repository));
    Ok(dialog
        .selecting(select)
        .conflicting(conflict)
        .pushed(pushed))
}

// Amends a commit on the local changes page, with its changes and message there: the last one
// right away, an older one once a rebase stopped at it, so the commits after it must be a line
// on the current branch.
fn amend(
    repository: &Repository,
    branch: &str,
    commit: &CommitHash,
) -> Result<GraphCommand, String> {
    let head = repository.head_commit().ok_or("Nothing is checked out")?;
    if *commit == head {
        return Ok(GraphCommand::AmendHead);
    }
    let base = single_parent(repository, commit, "amended")?;
    rebased(
        repository,
        branch,
        base.as_ref(),
        std::slice::from_ref(commit),
    )?;
    let base = base.map(|h| h.as_str().to_string());
    let dialog = GitDialog::edit(
        branch,
        dialog_commit(repository, commit),
        base,
        rebase_auto_stash(repository),
    );
    let pushed = repository.upstream_containing(commit.as_str());
    Ok(GraphCommand::Dialog(dialog.pushed(pushed)))
}

// The changes between two commits: the checked out commit and the selected (or picked) one,
// or the two picked ones; the older one is the base.
fn compare(
    repository: &Repository,
    selected: CommitHash,
    picked: Vec<CommitHash>,
) -> Result<GraphCommand, String> {
    let head = || repository.head_commit().ok_or("Nothing is checked out");
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
    let [target, base]: [CommitHash; 2] = sort_newest_first(repository, vec![a, b])
        .try_into()
        .expect("two commits");
    Ok(GraphCommand::Compare(Compare {
        base: Rev::Commit(base),
        target: Rev::Commit(target),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        git::{AfterTask, GitTask, SortCommit},
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
            Ok(GraphCommand::Dialog(dialog)) => {
                dialog.command().unwrap().unwrap().commands[0].join(" ")
            }
            Ok(GraphCommand::Compare(compare)) => compare.title(),
            Ok(GraphCommand::AmendHead) => "amend head".into(),
            Err(err) => err,
        }
    }

    #[test]
    fn test_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let git = GitRepository::new(path);
        git.init();
        let commit = |file: &str, content: &str, message: &str| {
            std::fs::write(path.join(file), content).unwrap();
            git.add(&[file]);
            git.commit(message, "2024-01-01");
        };
        commit("f", "base\n", "base");
        git.checkout_b("feature");
        commit("f", "feature\n", "f1");
        commit("g", "g\n", "f2");
        git.checkout("master");
        commit("f", "master\n", "m1");
        commit("h", "h\n", "m2");
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let rev = |r: &str| {
            crate::test_support::git::git(path, &["rev-parse", r])
                .trim()
                .to_string()
        };

        // on master: merging feature, or rebasing onto it, conflicts in f, at f1 when rebasing
        assert_eq!(
            merge_conflict(&repository, "feature"),
            Some(Conflict::At("feature".into(), vec!["f".into()]))
        );
        assert_eq!(
            rebase_conflict(&repository, "feature"),
            Some(Conflict::At(rev("master~1"), vec!["f".into()]))
        );
        // reverting m1 or m2 is clean, no later commit changed their files
        let revert =
            |c: &str| repository.first_conflict("HEAD", &[(Some(rev(c)), rev(&format!("{c}^")))]);
        assert!(revert("master~1").is_none());
        assert!(revert("master").is_none());
        // dropping m1 replays m2 on base, which is clean; cherry-picking f1 is not
        let m1 = CommitHash::from(rev("master~1").as_str());
        assert!(repository
            .replay_conflict(&rev("master~2"), &["master~2..HEAD"], &[m1])
            .is_none());
        let f1 = rev("feature~1");
        let pick = repository.first_conflict("HEAD", &[(Some(format!("{f1}^")), f1.clone())]);
        assert_eq!(pick, Some(Conflict::At(f1, vec!["f".into()])));
        // past the steps checked, it says how many were not
        let clean = (Some(rev("master~1")), rev("master"));
        let steps = vec![clean; crate::git::MAX_CONFLICT_STEPS + 1];
        assert_eq!(
            repository.first_conflict("HEAD", &steps),
            Some(Conflict::Unchecked(1))
        );
    }

    #[test]
    fn test_merge_commits_are_not_applied() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let git = GitRepository::new(path);
        git.init();
        git.commit("base", "2024-01-01");
        git.checkout_b("side");
        git.commit("side", "2024-01-02");
        git.checkout("master");
        git.merge(&["side"], "2024-01-03");
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let merge = repository.head_commit().unwrap();
        for (event, what) in [
            (UserEvent::CherryPick, "cherry-picked"),
            (UserEvent::Revert, "reverted"),
        ] {
            let err = args(command(&repository, event, &merge, &[]));
            let expected = format!(
                "{} is a merge commit, it can't be {what}",
                merge.as_short_hash()
            );
            assert_eq!(err, expected);
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
        assert_eq!(
            squash,
            format!("rebase -i --no-autosquash --autostash {}", c1.as_str())
        );
        let squash = args(command(&repository, UserEvent::Squash, c2, &[]));
        assert_eq!(squash, "rebase -i --no-autosquash --autostash --root");
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
            let status = crate::git::git_command()
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
            let out = crate::git::git_command()
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
        crate::git::run_task(path, &dialog.command().unwrap().unwrap()).unwrap();
        assert_eq!(output(&["log", "-1", "--format=%s"]), "c2\n");

        // the page amends it: its message and changes, with a new file and message
        let repository = load();
        let mut state = LocalChangesState::load(&repository);
        assert_eq!(state.snapshot().commit_draft_message(), "c2");
        std::fs::write(path.join("new"), "n\n").unwrap();
        git.add(&["new"]);
        state.commit_or_continue().unwrap();
        let dialog = state.take_dialog().unwrap();
        crate::git::run_task(path, &dialog.command().unwrap().unwrap()).unwrap();
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
            let out = crate::git::git_command()
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
                dialog.command().unwrap().unwrap()
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
        assert_eq!(task.after, AfterTask::Select("HEAD~0".into()));
        assert_eq!(git(&["log", "--format=%s"]), "c3\nc1\n");
        assert_eq!(git(&["ls-files"]), "f0\nf2\n");
        assert_eq!(git(&["status", "--porcelain"]), " M f0\n");
    }

    #[test]
    fn test_rewriting_pushed_commits_warns() {
        let (dir, git) = rewrite_repo();
        let path = dir.path();
        // c1 and c2 pushed, c3 not
        git(&["update-ref", "refs/remotes/origin/master", "HEAD~1"]);
        // on another remote branch only, it isn't where the branch is pushed to
        git(&["update-ref", "refs/remotes/origin/other", "HEAD"]);
        let (repository, hashes) = load(path);
        let warning = |event, hash: &CommitHash| match command(&repository, event, hash, &[]) {
            Ok(GraphCommand::Dialog(dialog)) => {
                let dialog = format!("{dialog:?}");
                assert!(!dialog.contains("origin/other"), "{dialog}");
                dialog.contains(
                    "Some(\"Already on origin/master, pushing the rewrite needs a force push.\")",
                )
            }
            _ => panic!("no popup"),
        };
        // no upstream, nothing is warned of
        assert!(!warning(UserEvent::DropCommit, &hashes[1]));
        git(&["remote", "add", "origin", "https://example.com/repo.git"]);
        git(&["branch", "--set-upstream-to=origin/master"]);
        assert!(!warning(UserEvent::DropCommit, &hashes[0]));
        assert!(warning(UserEvent::DropCommit, &hashes[1]));
        assert!(warning(UserEvent::AmendCommit, &hashes[1]));
        // c3 squashed into c2, which is pushed
        assert!(warning(UserEvent::Squash, &hashes[0]));
    }

    #[test]
    fn test_ref_graph_command() {
        let (dir, git) = rewrite_repo();
        let path = dir.path();
        git(&["branch", "topic", "HEAD~1"]);
        git(&["tag", "v1", "HEAD~2"]);
        let (repository, hashes) = load(path);
        let branch = |name: &str, target: &CommitHash| Ref::Branch {
            name: name.into(),
            target: target.clone(),
        };
        let on = |event, selected: &Ref| {
            let named = selected.named().unwrap();
            let target = selected.target().clone();
            args(ref_graph_command(&repository, event, &named, target).unwrap())
        };

        // rebase and merge name the branch, in full when a tag has its name
        let topic = branch("topic", &hashes[1]);
        assert_eq!(on(UserEvent::Rebase, &topic), "rebase --autostash topic");
        assert!(on(UserEvent::Merge, &topic).ends_with(" topic"));
        git(&["tag", "topic", "HEAD~2"]);
        let (repository, _) = load(path);
        let on = |event, selected: &Ref| {
            let named = selected.named().unwrap();
            let target = selected.target().clone();
            args(ref_graph_command(&repository, event, &named, target).unwrap())
        };
        assert!(on(UserEvent::Merge, &topic).ends_with(" refs/heads/topic"));
        git(&["tag", "-d", "topic"]);
        let (repository, _) = load(path);
        let on = |event, selected: &Ref| {
            let named = selected.named().unwrap();
            let target = selected.target().clone();
            args(ref_graph_command(&repository, event, &named, target).unwrap())
        };
        let master = branch("master", &hashes[0]);
        assert!(on(UserEvent::Merge, &master).contains("is checked out"));
        // checkout offers the branch first, a tag detached
        assert_eq!(on(UserEvent::Checkout, &topic), "checkout topic");
        let v1 = Ref::Tag {
            name: "v1".into(),
            target: hashes[2].clone(),
        };
        assert_eq!(on(UserEvent::Checkout, &v1), "checkout --detach v1");
        // a tag at a commit of the graph, and at a ref of the sidebar
        let tag = command(&repository, UserEvent::CreateTag, &hashes[1], &[]);
        run(path, tag, "t1");
        assert_eq!(git(&["tag", "--points-at", "topic"]), "t1\n");
        let named = topic.named().unwrap();
        let tag = ref_graph_command(&repository, UserEvent::CreateTag, &named, hashes[1].clone());
        assert_eq!(
            run(path, tag.unwrap(), "t2").commands,
            [["tag", "t2", "refs/heads/topic"]]
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
        assert_eq!(task.after, AfterTask::Select("HEAD~0".into()));
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
        crate::git::run_task(path, &dialog.command().unwrap().unwrap()).unwrap();
        assert_eq!(git(&["log", "--format=%B"]), "c2\n\nc1\n\n");
        assert_eq!(git(&["ls-files"]), "f0\nf1\nf2\n");
    }
}
