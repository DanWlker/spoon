// The commands of the refs sidebar: creating, deleting and editing branches and tags, applying
// and dropping stashes. Each opens a popup confirming it.

use crate::{
    event::UserEvent,
    git::{split_remote_branch, Ref, Repository},
    keybind::Scope,
    widget::{
        git_dialog::GitDialog,
        ref_list::{RefPicks, RefSection, RefSelection},
    },
};

// Whether `event` does something on the selected row of the sidebar, with the `picked` refs: its
// key is handled there only then, and shown in the hints (see `App::sidebar_hints`). The commands
// of the graph work on a branch, remote branch or tag; diffing and deleting on the picked refs too.
// Other events, like moving about, are not commands of the sidebar and always apply.
pub fn ref_command_applies(
    repository: &Repository,
    event: UserEvent,
    selection: &RefSelection,
    picked: &RefPicks,
) -> bool {
    use RefSection::*;
    let on_ref = matches!(selection, RefSelection::Ref(Branches | Remotes | Tags, _));
    let graph = event.home_scope() == Scope::Graph;
    // a detached HEAD is no ref to delete, rename, pick, copy or diff, only a commit to work on
    let detached = *selection == RefSelection::DetachedHead;
    match event {
        // on the whole graph, wherever
        UserEvent::BranchOnly => true,
        UserEvent::ShortCopy | UserEvent::FullCopy => on_ref,
        UserEvent::ToggleSelect => matches!(selection, RefSelection::Ref(..)) || !picked.is_empty(),
        UserEvent::DeleteRef => match selection {
            RefSelection::Ref(..) => true,
            // a remote, removed
            RefSelection::Folder(Remotes, name) => repository.remotes().contains(name),
            _ => !picked.is_empty(),
        },
        UserEvent::Diff => diff_refs(selection, picked).is_some(),
        // the others work on the selected row alone
        _ if !picked.is_empty() => {
            !(graph
                || matches!(
                    event,
                    UserEvent::CreateRef | UserEvent::EditRef | UserEvent::ApplyStash
                ))
        }
        UserEvent::CreateRef => adds_remote(repository, selection),
        UserEvent::EditRef => match selection {
            RefSelection::Ref(Branches, _) => true,
            RefSelection::Folder(Remotes, name) => repository.remotes().contains(name),
            _ => false,
        },
        UserEvent::ApplyStash => matches!(selection, RefSelection::Ref(Stashes, _)),
        UserEvent::OpenInBrowser | UserEvent::CopyLink => match selection {
            RefSelection::Ref(section, _) => *section != Stashes,
            RefSelection::Folder(Remotes, name) => repository.remotes().contains(name),
            _ => false,
        },
        _ if graph => on_ref || (detached && event != UserEvent::Diff),
        _ => true,
    }
}

// The refs `D` diffs in the sidebar: the picked ones, else the selected branch, remote branch or
// tag; None for stashes and the rows that are no ref.
pub fn diff_refs(selection: &RefSelection, picked: &RefPicks) -> Option<Vec<String>> {
    let (section, names) = match selection {
        _ if !picked.is_empty() => (picked.section, picked.names.clone()),
        RefSelection::Ref(section, name) => (*section, vec![name.clone()]),
        _ => return None,
    };
    (section != RefSection::Stashes).then_some(names)
}

// The popup of `event` on the selected row, where `ref_command_applies`; None for other events.
// `picked` are the refs picked in the sidebar, which deleting works on.
pub fn ref_command(
    repository: &Repository,
    event: UserEvent,
    selection: &RefSelection,
    picked: &RefPicks,
) -> Option<Result<GitDialog, String>> {
    use RefSection::*;
    let dialog = match (event, selection) {
        // remotes only: branches and tags are created with the commands of the graph
        (UserEvent::CreateRef, _) if adds_remote(repository, selection) => {
            Ok(GitDialog::add_remote())
        }
        (UserEvent::DeleteRef, _) if !picked.is_empty() => {
            delete_refs(repository, picked.section, &picked.names)
        }
        (UserEvent::DeleteRef, RefSelection::Folder(Remotes, name))
            if repository.remotes().contains(name) =>
        {
            Ok(GitDialog::remove_remote(name))
        }
        (UserEvent::DeleteRef, RefSelection::Ref(section, name)) => {
            delete_refs(repository, *section, std::slice::from_ref(name))
        }
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
        // a remote, see `ref_command_applies`
        (UserEvent::EditRef, RefSelection::Folder(Remotes, name)) => {
            let url = repository.remote_url(name).unwrap_or_default();
            Ok(GitDialog::edit_remote(name, &url))
        }
        (UserEvent::ApplyStash, RefSelection::Ref(Stashes, name)) => Ok(GitDialog::apply_stash(
            name,
            &stash_message(repository, name),
        )),
        _ => return None,
    };
    Some(dialog)
}

// Whether a remote is added on the selected row: a remote, like removing one, or the remotes
// header.
fn adds_remote(repository: &Repository, selection: &RefSelection) -> bool {
    match selection {
        RefSelection::Folder(RefSection::Remotes, name) => repository.remotes().contains(name),
        RefSelection::Section(RefSection::Remotes) => true,
        _ => false,
    }
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
            let unmerged = repository.unmerged_branches(&names);
            // the upstreams still on their remotes
            let info = repository.remote_info();
            let mut upstreams: Vec<(String, String)> = names
                .iter()
                .filter_map(|name| info.upstreams.get(name))
                .filter(|(remote, branch)| {
                    info.remote_branches
                        .get(remote)
                        .is_some_and(|branches| branches.contains(branch))
                })
                .cloned()
                .collect();
            upstreams.sort();
            upstreams.dedup();
            Ok(GitDialog::delete_branches(names, unmerged, upstreams))
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
    use crate::test_support::git::git;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::git::SortCommit;

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
        crate::git::run_task(path, &dialog.command().unwrap().unwrap()).unwrap();
    }

    fn typed(text: &str) -> Vec<Result<char, UserEvent>> {
        text.chars().map(Ok).collect()
    }

    #[test]
    fn test_first_remote_from_the_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        git(path, &["init", "-q"]);
        git(
            path,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "a",
            ],
        );
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let header = RefSelection::Section(RefSection::Remotes);
        let none = RefPicks::default();
        assert!(ref_command_applies(
            &repository,
            UserEvent::CreateRef,
            &header,
            &none
        ));
    }

    #[test]
    fn test_edit_remote() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        git(path, &["init", "-q", "-b", "main"]);
        git(path, &["config", "user.name", "t"]);
        git(path, &["config", "user.email", "t@t"]);
        git(path, &["commit", "-q", "--allow-empty", "-m", "c1"]);
        git(
            path,
            &["remote", "add", "origin", "https://a.example/r.git"],
        );

        // only a remote's folder edits it
        use RefSection::*;
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let folder = RefSelection::Folder(Remotes, "other".into());
        let picks = RefPicks::default();
        assert!(!ref_command_applies(
            &repository,
            UserEvent::EditRef,
            &folder,
            &picks
        ));
        let folder = RefSelection::Folder(Remotes, "origin".into());
        assert!(ref_command_applies(
            &repository,
            UserEvent::EditRef,
            &folder,
            &picks
        ));

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
            let selection = RefSelection::Section(RefSection::Branches);
            let dialog = ref_command(&repository, UserEvent::DeleteRef, &selection, &picked)
                .unwrap()
                .unwrap();
            crate::git::run_task(path, &dialog.command().unwrap().unwrap()).unwrap();
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
            &RefSelection::Section(Branches),
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

        use RefSection::*;
        git(path, &["checkout", "-q", "-b", "topic"]);

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
        // it opens typing the name; the upstream back to none
        dialog.handle_key(None, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        for event in [UserEvent::NavigateDown, UserEvent::NavigateLeft] {
            dialog.handle_key(
                Some(event),
                KeyEvent::new(KeyCode::Null, KeyModifiers::NONE),
            );
        }
        crate::git::run_task(path, &dialog.command().unwrap().unwrap()).unwrap();
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

        // a branch deleted with its upstream on the remote, when checked
        git(path, &["branch", "-q", "up"]);
        git(path, &["push", "-q", "-u", "origin", "up"]);
        let up = RefSelection::Ref(Branches, "up".into());
        run(
            path,
            UserEvent::DeleteRef,
            up,
            &[Err(UserEvent::DialogToggle)],
        );
        assert_eq!(branches(), "main \n");
        assert_eq!(
            git(remote_dir.path(), &["branch", "--format=%(refname:short)"]),
            "main\n"
        );

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

        // a remote, added on the remotes and fetched; enter goes on to the empty URL
        let mut keys = typed("other");
        keys.push(Err(UserEvent::Confirm));
        keys.extend(typed(remote_dir.path().to_str().unwrap()));
        run(
            path,
            UserEvent::CreateRef,
            RefSelection::Folder(Remotes, "origin".into()),
            &keys,
        );
        assert_eq!(git(path, &["remote"]), "origin\nother\n");
        assert_eq!(
            git(path, &["rev-parse", "other/main"]),
            git(path, &["rev-parse", "origin/main"])
        );

        // a tag pushed, then deleted here and on the remote
        git(path, &["tag", "v1"]);
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
        // what works where: the commands of the graph on a ref, a new remote on the remotes
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let applies = |event, selection: RefSelection, picked: &RefPicks| {
            ref_command_applies(&repository, event, &selection, picked)
        };
        let none = RefPicks::default();
        let main = || RefSelection::Ref(Branches, "main".into());
        for event in [
            UserEvent::CreateBranch,
            UserEvent::CreateTag,
            UserEvent::Checkout,
            UserEvent::ShortCopy,
            UserEvent::DeleteRef,
            UserEvent::EditRef,
        ] {
            assert!(applies(event, main(), &none), "{event:?}");
            assert!(
                !applies(event, RefSelection::Section(Branches), &none),
                "{event:?}"
            );
        }
        // a remote is added on a remote, like it is removed, or on the header
        assert!(!applies(UserEvent::CreateRef, main(), &none));
        assert!(applies(
            UserEvent::CreateRef,
            RefSelection::Folder(Remotes, "other".into()),
            &none
        ));
        assert!(applies(
            UserEvent::CreateRef,
            RefSelection::Section(Remotes),
            &none
        ));
        assert!(!applies(
            UserEvent::CreateRef,
            RefSelection::Ref(Remotes, "origin/main".into()),
            &none
        ));
        assert!(applies(
            UserEvent::EditRef,
            RefSelection::Folder(Remotes, "other".into()),
            &none
        ));
        assert!(applies(
            UserEvent::DeleteRef,
            RefSelection::Folder(Remotes, "other".into()),
            &none
        ));
        assert!(!applies(UserEvent::ApplyStash, main(), &none));
        // the detached HEAD is a commit to work on, no ref to copy, delete or diff
        let head = || RefSelection::DetachedHead;
        assert!(applies(UserEvent::Checkout, head(), &none));
        assert!(applies(UserEvent::CreateBranch, head(), &none));
        for event in [UserEvent::Diff, UserEvent::ShortCopy, UserEvent::DeleteRef] {
            assert!(!applies(event, head(), &none), "{event:?}");
        }
        assert!(applies(
            UserEvent::NavigateDown,
            RefSelection::Section(Tags),
            &none
        ));
        // with picks, deleting and diffing them, not the commands on the selected ref
        let picks = RefPicks {
            section: Branches,
            names: vec!["main".into(), "topic".into()],
        };
        assert!(applies(UserEvent::DeleteRef, main(), &picks));
        assert!(applies(UserEvent::Diff, main(), &picks));
        assert!(!applies(UserEvent::Checkout, main(), &picks));
        assert!(!applies(UserEvent::EditRef, main(), &picks));

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

        // a remote removed, with its remote branches
        run(
            path,
            UserEvent::DeleteRef,
            RefSelection::Folder(Remotes, "other".into()),
            &[],
        );
        assert_eq!(git(path, &["remote"]), "origin\n");
        assert_eq!(git(path, &["branch", "-r", "--list", "other/*"]), "");
    }
}
