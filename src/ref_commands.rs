// The commands of the refs sidebar: creating, deleting and editing branches and tags, applying
// and dropping stashes. Each opens a popup confirming it.

use crate::{
    event::UserEvent,
    git::{split_remote_branch, Ref, Repository},
    widget::{
        git_dialog::GitDialog,
        ref_list::{RefPicks, RefSection, RefSelection},
    },
};

// The popup of `event` on the selected row; None for other events, and where it does nothing: a
// remote created off the remotes, a branch or tag off a branch, remote branch or tag, applying off
// a stash.
// `picked` are the refs picked in the sidebar, which deleting works on.
pub fn ref_command(
    repository: &Repository,
    event: UserEvent,
    selection: &RefSelection,
    picked: &RefPicks,
) -> Option<Result<GitDialog, String>> {
    use RefSection::*;
    let dialog = match (event, selection) {
        // remotes only: branches are created with `CreateBranch`, tags from the graph
        (
            UserEvent::CreateRef,
            RefSelection::Section(Remotes)
            | RefSelection::Folder(Remotes, _)
            | RefSelection::Ref(Remotes, _),
        ) => Ok(GitDialog::add_remote()),
        // from the selected branch, remote branch or tag; nothing on other rows
        (UserEvent::CreateBranch, RefSelection::Ref(Branches | Remotes | Tags, name)) => {
            Ok(create_branch(repository, name))
        }
        (UserEvent::CreateTag, RefSelection::Ref(Branches | Remotes | Tags, name)) => {
            Ok(GitDialog::create_tag(name))
        }
        (UserEvent::DeleteRef, _) if !picked.is_empty() => {
            delete_refs(repository, picked.section, &picked.names)
        }
        (UserEvent::DeleteRef, RefSelection::Ref(section, name)) => {
            delete_refs(repository, *section, std::slice::from_ref(name))
        }
        (UserEvent::DeleteRef, _) => Err("Select a branch, a tag or a stash to delete it".into()),
        (UserEvent::EditRef, RefSelection::Ref(Branches, name)) => {
            let info = repository.remote_info();
            let upstream = info
                .upstreams
                .get(name)
                .map(|(remote, branch)| format!("{remote}/{branch}"));
            let mut remote_branches: Vec<String> = info
                .remote_branches
                .iter()
                .flat_map(|(remote, branches)| {
                    branches.iter().map(move |b| format!("{remote}/{b}"))
                })
                .collect();
            remote_branches.sort();
            Ok(GitDialog::edit_branch(name, upstream, remote_branches))
        }
        (UserEvent::EditRef, RefSelection::Folder(Remotes, name))
            if repository.remote_info().remotes.contains(name) =>
        {
            let url = repository.remote_url(name).unwrap_or_default();
            Ok(GitDialog::edit_remote(name, &url))
        }
        (UserEvent::EditRef, _) => Err(
            "Select a local branch to rename it or change its upstream, or a remote to edit it"
                .into(),
        ),
        (UserEvent::ApplyStash, RefSelection::Ref(Stashes, name)) => Ok(GitDialog::apply_stash(
            name,
            &stash_message(repository, name),
        )),
        _ => return None,
    };
    Some(dialog)
}

// Deletes the refs of a section: local branches (not the checked-out one), branches on their
// remotes, tags, or drops stashes.
fn delete_refs(
    repository: &Repository,
    section: RefSection,
    names: &[String],
) -> Result<GitDialog, String> {
    let names = names.to_vec();
    match section {
        RefSection::Branches => {
            if let Some(current) = repository
                .current_branch()
                .filter(|current| names.contains(current))
            {
                return Err(format!(
                    "{current} is checked out, check out another branch to delete it"
                ));
            }
            let unmerged = names
                .iter()
                .filter(|name| !repository.is_merged(name))
                .cloned()
                .collect();
            Ok(GitDialog::delete_branches(names, unmerged))
        }
        RefSection::Remotes => {
            let info = repository.remote_info();
            let branches = names
                .iter()
                .map(|name| {
                    split_remote_branch(&info.remotes, name)
                        .map(|(remote, branch)| (remote.to_string(), branch.to_string()))
                        .ok_or_else(|| format!("No remote has {name}"))
                })
                .collect::<Result<_, String>>()?;
            Ok(GitDialog::delete_remote_branches(branches))
        }
        RefSection::Tags => {
            let info = repository.remote_info();
            let remote = info
                .remotes
                .iter()
                .find(|r| *r == "origin")
                .or(info.remotes.first())
                .cloned();
            Ok(GitDialog::delete_tags(names, remote))
        }
        RefSection::Stashes => {
            let stashes = names
                .into_iter()
                .map(|name| {
                    let message = stash_message(repository, &name);
                    (name, message)
                })
                .collect();
            Ok(GitDialog::drop_stashes(stashes))
        }
    }
}

// A new branch from `from`, or another local branch.
pub fn create_branch(repository: &Repository, from: &str) -> GitDialog {
    let mut branches: Vec<String> = repository
        .all_refs()
        .into_iter()
        .filter(|r| matches!(r, Ref::Branch { .. }))
        .map(|r| r.name().to_string())
        .collect();
    branches.sort();
    GitDialog::create_branch(from, branches)
}

fn stash_message(repository: &Repository, name: &str) -> String {
    repository
        .all_refs()
        .into_iter()
        .find_map(|r| match r {
            Ref::Stash {
                name: n, message, ..
            } if n == name => Some(message.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::git::SortCommit;

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    // Runs the popup of the event on the selection, after the keys: characters are typed, the
    // others are events.
    fn run(
        path: &std::path::Path,
        event: UserEvent,
        selection: RefSelection,
        keys: &[Result<char, UserEvent>],
    ) {
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let mut dialog = ref_command(&repository, event, &selection, &RefPicks::default())
            .unwrap()
            .unwrap();
        for key in keys {
            match key {
                Ok(c) => {
                    dialog.handle_key(None, KeyEvent::new(KeyCode::Char(*c), KeyModifiers::NONE))
                }
                // enter ends typing
                Err(UserEvent::Confirm) => dialog.handle_key(
                    Some(UserEvent::Confirm),
                    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                ),
                Err(event) => dialog.handle_key(
                    Some(*event),
                    KeyEvent::new(KeyCode::Null, KeyModifiers::NONE),
                ),
            };
        }
        crate::git::run_task(path, &dialog.command().unwrap()).unwrap();
    }

    fn typed(text: &str) -> Vec<Result<char, UserEvent>> {
        text.chars().map(Ok).collect()
    }

    #[test]
    fn test_edit_remote() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        git(path, &["init", "-q", "-b", "main"]);
        git(path, &["commit", "-q", "--allow-empty", "-m", "c1"]);
        git(
            path,
            &["remote", "add", "origin", "https://a.example/r.git"],
        );

        // only a remote's folder edits it
        use RefSection::*;
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let folder = RefSelection::Folder(Remotes, "other".into());
        let refused = ref_command(
            &repository,
            UserEvent::EditRef,
            &folder,
            &RefPicks::default(),
        )
        .unwrap();
        assert!(refused.is_err());

        // renamed, with another URL
        let mut keys = vec![Err(UserEvent::DialogEdit)];
        keys.extend(typed("2"));
        keys.extend([
            Err(UserEvent::Confirm),
            Err(UserEvent::NavigateDown),
            Err(UserEvent::DialogEdit),
        ]);
        keys.extend(typed("x"));
        keys.push(Err(UserEvent::Confirm));
        let folder = RefSelection::Folder(Remotes, "origin".into());
        run(path, UserEvent::EditRef, folder, &keys);
        assert_eq!(
            git(path, &["remote", "-v"]).lines().next().unwrap(),
            "origin2\thttps://a.example/r.gitx (fetch)"
        );
    }

    #[test]
    fn test_delete_picked() {
        let dir = tempfile::tempdir().unwrap();
        let remote_dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        git(remote_dir.path(), &["init", "-q", "--bare", "-b", "main"]);
        git(path, &["init", "-q", "-b", "main"]);
        git(path, &["config", "user.name", "t"]);
        git(path, &["config", "user.email", "t@t"]);
        git(path, &["commit", "-q", "--allow-empty", "-m", "c1"]);
        let remote = remote_dir.path().to_str().unwrap();
        git(path, &["remote", "add", "origin", remote]);
        for name in ["a", "b", "c"] {
            git(path, &["branch", name]);
            git(path, &["tag", &format!("v{name}")]);
            git(path, &["push", "-q", "origin", &format!("main:r{name}")]);
        }
        git(path, &["fetch", "-q", "origin"]);
        for i in 0..4 {
            std::fs::write(path.join("f"), format!("{i}\n")).unwrap();
            git(
                path,
                &[
                    "stash",
                    "push",
                    "-q",
                    "--include-untracked",
                    "-m",
                    &format!("s{i}"),
                ],
            );
        }
        use RefSection::*;
        let picks = |section, names: &[&str]| RefPicks {
            section,
            names: names.iter().map(|n| n.to_string()).collect(),
        };
        let delete = |picked: RefPicks| {
            let repository =
                Repository::load(path, SortCommit::Chronological, None, false).unwrap();
            // the picked refs, whatever is selected
            let selection = RefSelection::LocalChanges;
            let dialog = ref_command(&repository, UserEvent::DeleteRef, &selection, &picked)
                .unwrap()
                .unwrap();
            crate::git::run_task(path, &dialog.command().unwrap()).unwrap();
        };

        delete(picks(Branches, &["a", "c"]));
        assert_eq!(
            git(path, &["branch", "--format=%(refname:short)"]),
            "b\nmain\n"
        );
        delete(picks(Tags, &["va", "vb"]));
        assert_eq!(git(path, &["tag"]), "vc\n");
        delete(picks(Remotes, &["origin/ra", "origin/rc"]));
        let on_remote = git(remote_dir.path(), &["branch", "--format=%(refname:short)"]);
        assert_eq!(on_remote, "rb\n");
        // stash@{1} and stash@{3} go, in an order that doesn't renumber the other one
        delete(picks(Stashes, &["stash@{1}", "stash@{3}"]));
        assert_eq!(
            git(path, &["stash", "list", "--format=%s"]),
            "On main: s3\nOn main: s1\n"
        );

        // the checked-out branch can't be among them
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let picked = picks(Branches, &["b", "main"]);
        let refused = ref_command(
            &repository,
            UserEvent::DeleteRef,
            &RefSelection::LocalChanges,
            &picked,
        );
        assert!(refused.unwrap().unwrap_err().contains("checked out"));
    }

    #[test]
    fn test_ref_commands() {
        let dir = tempfile::tempdir().unwrap();
        let remote_dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        git(remote_dir.path(), &["init", "-q", "--bare", "-b", "main"]);
        git(path, &["init", "-q", "-b", "main"]);
        git(path, &["config", "user.name", "t"]);
        git(path, &["config", "user.email", "t@t"]);
        git(path, &["config", "commit.gpgsign", "false"]);
        git(path, &["config", "tag.gpgsign", "false"]);
        std::fs::write(path.join("f"), "1\n").unwrap();
        git(path, &["add", "f"]);
        git(path, &["commit", "-q", "-m", "c1"]);
        git(
            path,
            &[
                "remote",
                "add",
                "origin",
                remote_dir.path().to_str().unwrap(),
            ],
        );
        git(path, &["push", "-q", "origin", "main"]);
        git(path, &["fetch", "-q", "origin"]);
        let branches = || {
            git(
                path,
                &["branch", "--format=%(refname:short) %(upstream:short)"],
            )
        };

        // a branch from the selected one, checked out
        use RefSection::*;
        let mut keys = typed("topic");
        keys.push(Err(UserEvent::Confirm));
        run(
            path,
            UserEvent::CreateBranch,
            RefSelection::Ref(Branches, "main".into()),
            &keys,
        );
        assert_eq!(git(path, &["branch", "--show-current"]), "topic\n");

        // the current branch can't be deleted
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let selection = RefSelection::Ref(Branches, "topic".into());
        let deleted = ref_command(
            &repository,
            UserEvent::DeleteRef,
            &selection,
            &RefPicks::default(),
        )
        .unwrap();
        assert!(deleted.unwrap_err().contains("checked out"));
        git(path, &["checkout", "-q", "main"]);

        // renamed, tracking origin/main
        let mut keys = vec![Err(UserEvent::DialogEdit)];
        keys.extend(typed("2"));
        keys.extend([
            Err(UserEvent::Confirm),
            Err(UserEvent::NavigateDown),
            Err(UserEvent::DialogToggle),
        ]);
        run(path, UserEvent::EditRef, selection.clone(), &keys);
        assert_eq!(branches(), "main \ntopic2 origin/main\n");

        // no longer tracking it
        let selection = RefSelection::Ref(Branches, "topic2".into());
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let mut dialog = ref_command(
            &repository,
            UserEvent::EditRef,
            &selection,
            &RefPicks::default(),
        )
        .unwrap()
        .unwrap();
        dialog.handle_key(
            Some(UserEvent::NavigateDown),
            KeyEvent::new(KeyCode::Null, KeyModifiers::NONE),
        );
        dialog.handle_key(
            Some(UserEvent::DialogEdit),
            KeyEvent::new(KeyCode::Null, KeyModifiers::NONE),
        );
        for _ in 0.."origin/main".len() {
            dialog.handle_key(None, KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        }
        crate::git::run_task(path, &dialog.command().unwrap()).unwrap();
        assert_eq!(branches(), "main \ntopic2 \n");

        run(path, UserEvent::DeleteRef, selection, &[]);
        assert_eq!(branches(), "main \n");

        // a branch not merged is deleted too, with a warning
        git(path, &["checkout", "-q", "-b", "ahead"]);
        git(path, &["commit", "-q", "--allow-empty", "-m", "c2"]);
        git(path, &["checkout", "-q", "main"]);
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let ahead = RefSelection::Ref(Branches, "ahead".into());
        let dialog = ref_command(
            &repository,
            UserEvent::DeleteRef,
            &ahead,
            &RefPicks::default(),
        )
        .unwrap()
        .unwrap();
        assert!(format!("{dialog:?}").contains(r#"unmerged: ["ahead"]"#));
        run(path, UserEvent::DeleteRef, ahead, &[]);
        assert_eq!(branches(), "main \n");

        // a remote branch, deleted on the remote
        git(path, &["push", "-q", "origin", "main:gone"]);
        run(
            path,
            UserEvent::DeleteRef,
            RefSelection::Ref(Remotes, "origin/gone".into()),
            &[],
        );
        assert_eq!(
            git(remote_dir.path(), &["branch", "--format=%(refname:short)"]),
            "main\n"
        );

        // a remote, added on the remotes and fetched
        let mut keys = typed("other");
        keys.extend([Err(UserEvent::Confirm), Err(UserEvent::NavigateDown)]);
        keys.push(Err(UserEvent::DialogEdit));
        keys.extend(typed(remote_dir.path().to_str().unwrap()));
        run(
            path,
            UserEvent::CreateRef,
            RefSelection::Section(Remotes),
            &keys,
        );
        assert_eq!(git(path, &["remote"]), "origin\nother\n");
        assert_eq!(
            git(path, &["rev-parse", "other/main"]),
            git(path, &["rev-parse", "origin/main"])
        );

        // an annotated tag at the selected branch, pushed, then deleted here and on the remote
        let mut keys = typed("v1");
        keys.extend([
            Err(UserEvent::Confirm),
            Err(UserEvent::NavigateDown),
            Err(UserEvent::DialogEdit),
        ]);
        keys.extend(typed("first"));
        run(
            path,
            UserEvent::CreateTag,
            RefSelection::Ref(Branches, "main".into()),
            &keys,
        );
        assert_eq!(
            git(path, &["tag", "-n", "--points-at", "main"])
                .split_whitespace()
                .collect::<Vec<_>>(),
            ["v1", "first"]
        );
        git(path, &["push", "-q", "origin", "v1"]);
        let keys = [Err(UserEvent::DialogToggle)];
        run(
            path,
            UserEvent::DeleteRef,
            RefSelection::Ref(Tags, "v1".into()),
            &keys,
        );
        assert_eq!(git(path, &["tag"]), "");
        assert_eq!(git(remote_dir.path(), &["tag"]), "");
        // `a` makes remotes only
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        for selection in [RefSelection::Section(Tags), RefSelection::Section(Branches)] {
            let none = ref_command(
                &repository,
                UserEvent::CreateRef,
                &selection,
                &RefPicks::default(),
            );
            assert!(none.is_none());
        }
        // b and t make a branch or tag at a ref only, not on the headers
        let headers = [RefSelection::Section(Branches), RefSelection::Section(Tags)];
        for (event, selection) in [UserEvent::CreateBranch, UserEvent::CreateTag]
            .into_iter()
            .zip(headers)
        {
            let none = ref_command(&repository, event, &selection, &RefPicks::default());
            assert!(none.is_none());
        }

        // a stash applied with what was staged, kept, then dropped
        std::fs::write(path.join("f"), "2\n").unwrap();
        git(path, &["add", "f"]);
        git(path, &["stash", "-q"]);
        let stash = RefSelection::Ref(Stashes, "stash@{0}".into());
        run(
            path,
            UserEvent::ApplyStash,
            stash.clone(),
            &[Err(UserEvent::DialogToggle)],
        );
        assert_eq!(git(path, &["status", "--porcelain"]), "M  f\n");
        run(path, UserEvent::DeleteRef, stash, &[]);
        assert_eq!(git(path, &["stash", "list"]), "");

        // popped: applied (unstaged, without --index), then dropped
        git(path, &["stash", "-q"]);
        let keys = [Err(UserEvent::NavigateDown), Err(UserEvent::DialogToggle)];
        run(
            path,
            UserEvent::ApplyStash,
            RefSelection::Ref(Stashes, "stash@{0}".into()),
            &keys,
        );
        assert_eq!(git(path, &["status", "--porcelain"]), " M f\n");
        assert_eq!(git(path, &["stash", "list"]), "");

        // space on another row opens or closes it
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let section = RefSelection::Section(Branches);
        assert!(ref_command(
            &repository,
            UserEvent::ApplyStash,
            &section,
            &RefPicks::default()
        )
        .is_none());
    }
}
