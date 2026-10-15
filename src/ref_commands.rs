// The commands of the refs sidebar: creating, deleting and editing branches and tags, applying
// and dropping stashes. Each opens a popup confirming it.

use crate::{
    event::UserEvent,
    git::{Head, Ref, Repository},
    widget::{
        git_dialog::GitDialog,
        ref_list::{RefSection, RefSelection},
    },
};

// The popup of `event` on the selected row; None for other events, and for applying on a row
// that is not a stash.
pub fn ref_command(
    repository: &Repository,
    event: UserEvent,
    selection: &RefSelection,
) -> Option<Result<GitDialog, String>> {
    use RefSection::*;
    let current = match repository.head() {
        Head::Branch { name } => Some(name.clone()),
        _ => None,
    };
    let dialog = match (event, selection) {
        (UserEvent::CreateRef, RefSelection::Section(Tags) | RefSelection::Ref(Tags, _)) => {
            Ok(GitDialog::create_tag("HEAD"))
        }
        (UserEvent::CreateRef, RefSelection::Section(Branches | Remotes)) => {
            let from = current.unwrap_or_else(|| "HEAD".into());
            Ok(create_branch(repository, &from))
        }
        (UserEvent::CreateRef, RefSelection::Ref(Branches | Remotes, name)) => {
            Ok(create_branch(repository, name))
        }
        (UserEvent::CreateRef, _) => Err("Select the branches or the tags to create one".into()),
        (UserEvent::DeleteRef, RefSelection::Ref(Branches, name)) => {
            if Some(name) == current.as_ref() {
                Err(format!(
                    "{name} is checked out, check out another branch to delete it"
                ))
            } else {
                Ok(GitDialog::delete_branch(name, repository.is_merged(name)))
            }
        }
        (UserEvent::DeleteRef, RefSelection::Ref(Remotes, name)) => {
            let info = repository.remote_info();
            // remote names may contain slashes, the longest matching one owns the branch
            info.remotes
                .iter()
                .filter(|remote| name.starts_with(&format!("{remote}/")))
                .max_by_key(|remote| remote.len())
                .map(|remote| GitDialog::delete_remote_branch(remote, &name[remote.len() + 1..]))
                .ok_or_else(|| format!("No remote has {name}"))
        }
        (UserEvent::DeleteRef, RefSelection::Ref(Tags, name)) => {
            let info = repository.remote_info();
            let remote = info
                .remotes
                .iter()
                .find(|r| *r == "origin")
                .or(info.remotes.first())
                .cloned();
            Ok(GitDialog::delete_tag(name, remote))
        }
        (UserEvent::DeleteRef, RefSelection::Ref(Stashes, name)) => Ok(GitDialog::drop_stash(
            name,
            &stash_message(repository, name),
        )),
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
        (UserEvent::EditRef, _) => {
            Err("Select a local branch to rename it or change its upstream".into())
        }
        (UserEvent::ApplyStash, RefSelection::Ref(Stashes, name)) => Ok(GitDialog::apply_stash(
            name,
            &stash_message(repository, name),
        )),
        _ => return None,
    };
    Some(dialog)
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
        let mut dialog = ref_command(&repository, event, &selection)
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

        // a branch from the current one, checked out
        use RefSection::*;
        let mut keys = typed("topic");
        keys.push(Err(UserEvent::Confirm));
        run(
            path,
            UserEvent::CreateRef,
            RefSelection::Section(Branches),
            &keys,
        );
        assert_eq!(git(path, &["branch", "--show-current"]), "topic\n");

        // the current branch can't be deleted
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let selection = RefSelection::Ref(Branches, "topic".into());
        let deleted = ref_command(&repository, UserEvent::DeleteRef, &selection).unwrap();
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
        let mut dialog = ref_command(&repository, UserEvent::EditRef, &selection)
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
        let dialog = ref_command(&repository, UserEvent::DeleteRef, &ahead)
            .unwrap()
            .unwrap();
        assert!(format!("{dialog:?}").contains("merged: false"));
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

        // an annotated tag, pushed, then deleted here and on the remote
        let mut keys = typed("v1");
        keys.extend([
            Err(UserEvent::Confirm),
            Err(UserEvent::NavigateDown),
            Err(UserEvent::DialogEdit),
        ]);
        keys.extend(typed("first"));
        run(
            path,
            UserEvent::CreateRef,
            RefSelection::Section(Tags),
            &keys,
        );
        assert_eq!(
            git(path, &["tag", "-n"])
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
        assert!(ref_command(&repository, UserEvent::ApplyStash, &section).is_none());
    }
}
