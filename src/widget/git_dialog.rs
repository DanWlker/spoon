use ratatui::{
    buffer::Buffer,
    crossterm::event::{Event, KeyCode, KeyEvent},
    layout::Rect,
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::Widget,
};
use tui_input::{backend::crossterm::EventHandler, Input};

use crate::widget::tree_query::Toward;
use crate::{
    app::AppContext,
    browse::{LinkAction, PageLink},
    color::ColorTheme,
    event::{AppEvent, UserEvent},
    git::{
        parse_git_bool, stash_args, todo_action, AfterTask, BranchStatus, CommitHash, Conflict,
        FileChange, GitTask, NamedRef, RemoteInfo, MAX_CONFLICT_STEPS,
    },
    keys::{KeyList, Keys},
    widget::style::{change_span, choice_arrows, focused_title, no_choice_arrows, render_popup},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteAction {
    Fetch,
    Pull,
    Push,
}

// A branch or commit to check out, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutTarget {
    pub label: String,
    pub args: Vec<String>,
}

// Where a new branch starts or a tag goes: a commit, or a branch, remote branch or tag (a branch
// made from a remote branch tracks it). Never typed, so always one that exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartPoint {
    Commit(CommitHash),
    Ref(NamedRef),
}

impl StartPoint {
    // As the popup shows it: the short hash, or the name.
    fn label(&self) -> String {
        match self {
            StartPoint::Commit(hash) => hash.as_short_hash().to_string(),
            StartPoint::Ref(named) => named.name().to_string(),
        }
    }

    // As git takes it, in full so a branch and a tag of the same name can't be mixed up.
    fn revision(&self) -> String {
        match self {
            StartPoint::Commit(hash) => hash.as_str().to_string(),
            StartPoint::Ref(named) => named.full_name(),
        }
    }
}

// A commit listed in a popup: its full hash, and the line describing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogCommit {
    pub hash: CommitHash,
    pub line: String,
}

#[derive(Debug)]
enum DialogKind {
    Remote(RemoteAction, Box<RemoteInfo>),
    Rebase {
        onto: String,
        description: String,
    },
    Merge {
        source: String,
        description: String,
    },
    Checkout {
        targets: Vec<CheckoutTarget>,
    },
    CherryPick {
        commits: Vec<DialogCommit>,
    },
    Squash {
        commits: Vec<DialogCommit>,
        base: Option<String>,
    },
    Revert {
        commits: Vec<DialogCommit>,
    },
    Reset {
        commit: DialogCommit,
        branch: String,
    },
    // a file as `change` changed it in `commit`
    ResetFile {
        commit: DialogCommit,
        change: Box<FileChange>,
    },
    // `base` is where the rebase starts, None from the root
    Edit {
        commit: DialogCommit,
        base: Option<String>,
    },
    Drop {
        commits: Vec<DialogCommit>,
        base: Option<String>,
    },
    // where it may start, the first chosen
    CreateBranch {
        starts: Vec<StartPoint>,
    },
    // local branches, with those whose commits are on no other branch, and their upstreams
    // still on their remotes, as (remote, branch), which may be deleted too
    DeleteBranches {
        names: Vec<String>,
        unmerged: Vec<String>,
        upstreams: Vec<(String, String)>,
    },
    // branches on their remotes, as (remote, branch)
    DeleteRemoteBranches {
        branches: Vec<(String, String)>,
    },
    // `upstream` is like "origin/main", empty when there is none
    // `upstreams` are the choices of the upstream field after None, the current one among them
    EditBranch {
        name: String,
        upstream: Option<String>,
        upstreams: Vec<String>,
    },
    EditRemote {
        name: String,
        url: String,
    },
    AddRemote,
    RemoveRemote {
        name: String,
    },
    CreateTag {
        at: StartPoint,
    },
    // `remote` is where they may be deleted too
    DeleteTags {
        names: Vec<String>,
        remote: Option<String>,
    },
    // a stash like "stash@{0}"
    ApplyStash {
        name: String,
    },
    // stashes like "stash@{0}"
    DropStashes {
        names: Vec<String>,
    },
    // links to open in the browser, or to copy: the pages of each remote
    OpenLink {
        links: Vec<Vec<PageLink>>,
        action: LinkAction,
    },
    // a command made beforehand, like discarding changes, and what it loses
    Prepared {
        task: Box<GitTask>,
        warning: &'static str,
    },
    Commit {
        preview: Box<CommitPreview>,
        task: Box<GitTask>,
    },
    // the changes of `paths` (all of them when empty) on the local changes page
    Stash {
        paths: Vec<String>,
        target: String,
    },
}

// The commit to make, shown in the popup confirming it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitPreview {
    // "Commit to", "Amend the last commit on", "Commit the merge on"
    pub action: String,
    pub branch: String,
    pub amend: bool,
    pub subject: String,
    pub body: Vec<String>,
    // the staged files, as their status letter like "M" and their path
    pub files: Vec<(String, String)>,
}

// The lines of the message, and the files, shown in the popup confirming a commit.
const COMMIT_PREVIEW_LINES: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldId {
    Remote,
    Branch,
    LocalBranch,
    RemoteBranch,
    Rebase,
    AutoStash,
    Tags,
    Force,
    MergeType,
    Target,
    LocalChanges,
    Commit,
    AppendOrigin,
    ResetMode,
    FileState,
    Messages,
    Name,
    From,
    Message,
    Upstream,
    At,
    Index,
    Checkout,
    Pop,
    Url,
    Link,
    NoVerify,
}

#[derive(Debug)]
enum Field {
    // one of the options, changed with left/right
    Choice { options: Vec<String>, index: usize },
    // any text, or one of the options with left/right
    Text { input: Input, options: Vec<String> },
    Check(bool),
    // a value that can't change, like the commit a branch starts from
    Fixed(String),
}

#[derive(Debug)]
pub enum DialogOutcome {
    None,
    Close,
    Quit,
    // what the popup leads to, for the app once it closes: its git command, or the link chosen
    Send(Box<AppEvent>),
    Error(String),
}

const MERGE_TYPES: [&str; 5] = [
    "Fast-forward if possible",
    "Always create a merge commit",
    "Fast-forward only",
    "Squash (commit in local changes)",
    "Don't commit (edit in local changes)",
];

const LOCAL_CHANGES_OPTIONS: [&str; 3] = ["Keep", "Stash and reapply", "Discard"];

const SQUASH_MESSAGES: [&str; 2] = ["Combine the messages", "Keep the parent's message (fixup)"];

const FILE_STATES: [&str; 2] = ["At the commit", "Before the commit"];

const RESET_MODES: [&str; 3] = [
    "Mixed: keep the changes, unstaged",
    "Soft: keep the changes, staged",
    "Hard: discard the changes",
];

// The popup confirming a git command, with its options: fetch, pull and push, and the commands
// on the commit graph like merge or checkout.
#[derive(Debug)]
pub struct GitDialog {
    title: &'static str,
    // the word for the command in the key hints, like "push"
    verb: &'static str,
    // what the command does, above the options
    intro: Vec<String>,
    kind: DialogKind,
    // the event that opened the popup, which runs the command too, so a double press runs it
    trigger: Option<UserEvent>,
    fields: Vec<(FieldId, String, Field)>,
    selected: usize,
    // typing into the selected text field
    editing: bool,
    // the stash checkbox was changed, so it no longer follows the config of how to pull
    auto_stash_changed: bool,
    // the commit to select once the command is done, see `GitTask::select`
    select: Option<String>,
    // the conflicts git would stop at, shown in the warning color
    conflicts: Option<String>,
    // the remote branch already having the commits it rewrites, see `GitDialog::pushed`
    pushed: Option<String>,
}

impl GitDialog {
    fn with(title: &'static str, verb: &'static str, intro: Vec<String>, kind: DialogKind) -> Self {
        GitDialog {
            title,
            verb,
            intro,
            kind,
            fields: vec![],
            selected: 0,
            editing: false,
            auto_stash_changed: false,
            trigger: None,
            select: None,
            conflicts: None,
            pushed: None,
        }
    }

    // Warns that rewriting the commits, already on `remote_branch`, means force pushing them.
    pub fn pushed(mut self, remote_branch: Option<String>) -> Self {
        self.pushed = remote_branch
            .map(|b| format!("Already on {b}, pushing the rewrite needs a force push."));
        self
    }

    // Selects the revision once the command rewrote the history, see `GitTask::select`.
    pub fn selecting(mut self, revision: Option<String>) -> Self {
        self.select = revision;
        self
    }

    // Discards changes on the local changes page, with `task` made from what is selected.
    pub fn discard(prompt: String, task: GitTask) -> Self {
        let kind = DialogKind::Prepared {
            task: Box::new(task),
            warning: "The changes are lost.",
        };
        Self::with(" Discard changes ", "discard", vec![prompt], kind)
    }

    // Aborts the rebase, merge, cherry-pick or revert in progress, named like "rebase".
    pub fn abort(operation: &str) -> Self {
        let task = GitTask {
            commands: vec![vec![operation.to_string(), "--abort".into()]],
            running: format!("Aborting the {operation}"),
            done: format!("Aborted the {operation}"),
            ..Default::default()
        };
        let intro = vec![format!(
            "Abort the {operation} and go back to where it started"
        )];
        let kind = DialogKind::Prepared {
            task: Box::new(task),
            warning: "What was done since it started is lost.",
        };
        Self::with(" Abort ", "abort", intro, kind)
    }

    // Commits as `preview` shows; the commit key confirms it too.
    pub fn commit(preview: CommitPreview, task: GitTask) -> Self {
        let (title, verb) = if preview.amend {
            (" Amend ", "amend")
        } else {
            (" Commit ", "commit")
        };
        let kind = DialogKind::Commit {
            preview: Box::new(preview),
            task: Box::new(task),
        };
        let mut dialog = Self::with(title, verb, vec![], kind);
        dialog.trigger = Some(UserEvent::Commit);
        dialog
    }

    // Stashes the changes of `paths`, all of them when empty, described as `target`.
    pub fn stash(target: String, paths: Vec<String>) -> Self {
        let intro = vec![format!("Stash {target}")];
        let mut dialog = Self::with(
            " Stash ",
            "stash",
            intro,
            DialogKind::Stash { paths, target },
        );
        dialog.push_text(FieldId::Message, "Message", "", vec![]);
        dialog.editing = true;
        dialog
    }

    fn stash_command(&self, paths: &[String], target: &str) -> Result<GitTask, String> {
        Ok(GitTask {
            commands: vec![stash_args(paths, self.value(FieldId::Message).trim())],
            running: format!("Stashing {target}"),
            done: format!("Stashed {target}"),
            ..Default::default()
        })
    }

    #[cfg(test)]
    pub fn intro(&self) -> &[String] {
        &self.intro
    }

    #[cfg(test)]
    pub fn commit_preview(&self) -> Option<&CommitPreview> {
        match &self.kind {
            DialogKind::Commit { preview, .. } => Some(preview),
            _ => None,
        }
    }

    // The event its key confirms it with besides Enter, like the key that opened it.
    // Typing into a text field, which takes every key.
    pub fn is_editing(&self) -> bool {
        self.editing
    }

    pub fn trigger(&self) -> Option<UserEvent> {
        self.trigger
    }

    // Warns of the conflicts the command runs into, see `Repository::first_conflict`, with the
    // commit it stops at for those applying several commits, all but merging.
    pub fn conflicting(mut self, conflict: Option<Conflict>) -> Self {
        let at = !matches!(self.kind, DialogKind::Merge { .. });
        self.conflicts = conflict.map(|conflict| {
            let (commit, files) = match conflict {
                Conflict::At(commit, files) => (commit, files),
                Conflict::Unchecked(n) => {
                    return format!(
                    "No conflicts in the first {MAX_CONFLICT_STEPS} commits, {n} more not checked."
                )
                }
            };
            let names = capped(files, 3).join(", ");
            let at = if at {
                format!(" at {}", shorten_hash(&commit))
            } else {
                String::new()
            };
            format!("Conflicts{at} in {names}.")
        });
        self
    }

    pub fn remote(action: RemoteAction, info: RemoteInfo) -> Result<Self, String> {
        if info.remotes.is_empty() {
            return Err("No remotes configured".into());
        }
        // git would merge into the detached HEAD, a commit no branch keeps
        if action == RemoteAction::Pull && info.current_branch.is_none() {
            return Err("Check out a branch to pull into it".into());
        }
        let (title, verb, trigger) = match action {
            RemoteAction::Fetch => (" Fetch ", "fetch", UserEvent::Fetch),
            RemoteAction::Pull => (" Pull ", "pull", UserEvent::Pull),
            RemoteAction::Push => (" Push ", "push", UserEvent::Push),
        };
        let mut dialog = Self::with(
            title,
            verb,
            vec![],
            DialogKind::Remote(action, Box::new(info.clone())),
        );
        dialog.trigger = Some(trigger);
        let current = info.current_branch.clone();
        match action {
            RemoteAction::Fetch => {
                let remote = dialog.default_remote(current.as_deref());
                dialog.push_choice(FieldId::Remote, "Remote", info.remotes.clone(), &remote);
            }
            RemoteAction::Pull => {
                // only the remotes with a branch to pull, as fetched
                let remotes: Vec<String> = info
                    .remotes
                    .iter()
                    .filter(|r| info.remote_branches.get(*r).is_some_and(|b| !b.is_empty()))
                    .cloned()
                    .collect();
                if remotes.is_empty() {
                    return Err("No remote has a branch to pull, fetch first".into());
                }
                let remote = dialog.default_remote(current.as_deref());
                dialog.push_choice(FieldId::Remote, "Remote", remotes, &remote);
                dialog.push_choice(FieldId::Branch, "Branch", vec![], "");
                dialog.update_pull_branch(None);
                dialog.push_check(FieldId::Rebase, "Rebase instead of merge", info.pull_rebase);
                let auto_stash = info.auto_stash(info.pull_rebase);
                dialog.push_check(
                    FieldId::AutoStash,
                    "Stash and reapply local changes",
                    auto_stash,
                );
            }
            RemoteAction::Push => {
                if info.branches.is_empty() {
                    return Err("No local branches to push".into());
                }
                let current = current.unwrap_or_default();
                dialog.push_choice(
                    FieldId::LocalBranch,
                    "Branch",
                    info.branches.clone(),
                    &current,
                );
                dialog.push_choice(FieldId::Remote, "To remote", info.remotes.clone(), "");
                dialog.push_text(FieldId::RemoteBranch, "As branch", "", vec![]);
                dialog.push_check(FieldId::Tags, "Push all tags", false);
                dialog.push_check(FieldId::Force, "Force push (with lease)", false);
                dialog.push_check(
                    FieldId::NoVerify,
                    "Skip the pre-push hook (--no-verify)",
                    false,
                );
                dialog.update_push_remote();
            }
        }
        Ok(dialog)
    }

    fn remote_command(&self, action: RemoteAction) -> Result<GitTask, String> {
        let remote = self.value(FieldId::Remote);
        let mut branch_status = None;
        let (args, running, done) = match action {
            RemoteAction::Fetch => (
                vec!["fetch".to_string(), remote.clone()],
                format!("Fetching {remote}"),
                format!("Fetched {remote}"),
            ),
            RemoteAction::Pull => {
                let branch = self.value(FieldId::Branch);
                let mut args = vec!["pull".to_string()];
                // explicit, so the checkbox wins over pull.rebase
                args.push(if self.checked(FieldId::Rebase) {
                    "--rebase".into()
                } else {
                    "--no-rebase".into()
                });
                // explicit too, so the checkbox wins over rebase.autoStash and merge.autoStash
                args.push(if self.checked(FieldId::AutoStash) {
                    "--autostash".into()
                } else {
                    "--no-autostash".into()
                });
                args.extend([remote.clone(), branch.clone()]);
                // pulling merges into the checked-out branch
                branch_status = self
                    .info()
                    .current_branch
                    .clone()
                    .map(|branch| BranchStatus {
                        branch,
                        label: "Pulling".into(),
                        short: "↓",
                    });
                (
                    args,
                    format!("Pulling {remote}/{branch}"),
                    format!("Pulled {remote}/{branch}"),
                )
            }
            RemoteAction::Push => {
                let branch = self.value(FieldId::LocalBranch);
                let remote_branch = self.value(FieldId::RemoteBranch);
                if remote_branch.is_empty() {
                    return Err("Choose a branch to push to".into());
                }
                let mut args = vec!["push".to_string()];
                if self.checked(FieldId::Force) {
                    args.push("--force-with-lease".into());
                }
                if self.checked(FieldId::Tags) {
                    args.push("--tags".into());
                }
                if self.checked(FieldId::NoVerify) {
                    args.push("--no-verify".into());
                }
                // a branch without an upstream tracks the branch it is pushed to
                if !self.info().upstreams.contains_key(&branch) {
                    args.push("--set-upstream".into());
                }
                args.extend([
                    remote.clone(),
                    format!("refs/heads/{branch}:refs/heads/{remote_branch}"),
                ]);
                let force = if self.checked(FieldId::Force) {
                    "Force pushing"
                } else {
                    "Pushing"
                };
                branch_status = Some(BranchStatus {
                    branch: branch.clone(),
                    label: force.into(),
                    short: "↑",
                });
                (
                    args,
                    format!("{force} {branch} to {remote}/{remote_branch}"),
                    format!("Pushed {branch} to {remote}/{remote_branch}"),
                )
            }
        };
        Ok(GitTask {
            commands: vec![args],
            remote: true,
            running,
            done,
            branch_status,
            ..Default::default()
        })
    }

    // `onto` is what git is given, `onto_label` how it is shown.
    // `auto_stash` is the default of the checkbox, from rebase.autoStash.
    pub fn rebase(branch: &str, onto: String, onto_label: &str, auto_stash: bool) -> Self {
        let description = format!("Rebased {branch} onto {onto_label}");
        let intro = vec![format!("Rebase {branch} onto {onto_label}")];
        let kind = DialogKind::Rebase { onto, description };
        let mut dialog = Self::with(" Rebase ", "rebase", intro, kind);
        dialog.push_check(
            FieldId::AutoStash,
            "Stash and reapply local changes",
            auto_stash,
        );
        dialog
    }

    fn rebase_command(&self, onto: &str, description: &str) -> Result<GitTask, String> {
        let auto_stash = self.checked(FieldId::AutoStash);
        let stash = if auto_stash {
            "--autostash"
        } else {
            "--no-autostash"
        };
        Ok(GitTask::new(
            &["rebase", stash, onto],
            description.replace("Rebased", "Rebasing"),
            description.to_string(),
        ))
    }

    // `merge_ff` is the merge.ff config, which picks the default type.
    pub fn merge(branch: &str, source: String, source_label: &str, merge_ff: Option<&str>) -> Self {
        let description = format!("Merged {source_label} into {branch}");
        let intro = vec![format!("Merge {source_label} into {branch}")];
        let kind = DialogKind::Merge {
            source,
            description,
        };
        let mut dialog = Self::with(" Merge ", "merge", intro, kind);
        let types = MERGE_TYPES.map(String::from).to_vec();
        let default = match merge_ff {
            Some("only") => MERGE_TYPES[2],
            Some(value) if parse_git_bool(value) == Some(false) => MERGE_TYPES[1],
            _ => MERGE_TYPES[0],
        };
        dialog.push_choice(FieldId::MergeType, "Type", types, default);
        dialog
    }

    fn merge_command(&self, source: &str, description: &str) -> Result<GitTask, String> {
        let mut args = vec!["merge"];
        args.extend_from_slice(match self.choice(FieldId::MergeType) {
            0 => &["--ff", "--no-edit"][..],
            1 => &["--no-ff", "--no-edit"],
            2 => &["--ff-only"],
            3 => &["--squash"],
            _ => &["--no-ff", "--no-commit"],
        });
        args.push(source);
        let done = match self.choice(FieldId::MergeType) {
            3 | 4 => format!("{description}, commit it in local changes"),
            _ => description.to_string(),
        };
        Ok(GitTask {
            after: if self.choice(FieldId::MergeType) >= 3 {
                AfterTask::LocalChanges
            } else {
                AfterTask::Reload
            },
            ..GitTask::new(&args, description.replace("Merged", "Merging"), done)
        })
    }

    pub fn checkout(targets: Vec<CheckoutTarget>) -> Self {
        let options = targets.iter().map(|t| t.label.clone()).collect();
        let mut dialog = Self::with(
            " Checkout ",
            "check out",
            vec![],
            DialogKind::Checkout { targets },
        );
        dialog.push_choice(FieldId::Target, "Check out", options, "");
        let options = LOCAL_CHANGES_OPTIONS.map(String::from).to_vec();
        dialog.push_choice(FieldId::LocalChanges, "Local changes", options, "");
        dialog
    }

    fn checkout_command(&self, targets: &[CheckoutTarget]) -> Result<GitTask, String> {
        let target = &targets[self.choice(FieldId::Target)];
        let local_changes = self.choice(FieldId::LocalChanges);
        let mut args = vec!["checkout".to_string()];
        if local_changes == 2 {
            args.push("--force".into());
        }
        args.extend(target.args.iter().cloned());
        Ok(GitTask {
            commands: vec![args],
            stash: local_changes == 1,
            running: format!("Checking out {}", target.label),
            done: format!("Checked out {}", target.label),
            ..Default::default()
        })
    }

    // `commits` are the oldest first, the order they are applied in.
    pub fn cherry_pick(branch: &str, commits: Vec<DialogCommit>) -> Self {
        let intro = commits_intro(&format!("Cherry-pick onto {branch}"), &commits);
        let kind = DialogKind::CherryPick { commits };
        let mut dialog = Self::with(" Cherry-pick ", "cherry-pick", intro, kind);
        dialog.push_check(FieldId::Commit, "Commit (else edit in local changes)", true);
        dialog.push_check(
            FieldId::AppendOrigin,
            "Append \"(cherry picked from commit …)\" to the messages",
            false,
        );
        dialog
    }

    fn cherry_pick_command(&self, commits: &[DialogCommit]) -> Result<GitTask, String> {
        let mut args = vec!["cherry-pick".to_string()];
        if !self.checked(FieldId::Commit) {
            args.push("--no-commit".into());
        }
        if self.checked(FieldId::AppendOrigin) {
            args.push("-x".into());
        }
        args.extend(commits.iter().map(|c| c.hash.as_str().to_string()));
        let n = count_commits(commits.len());
        Ok(GitTask {
            commands: vec![args],
            running: format!("Cherry-picking {n}"),
            done: format!("Cherry-picked {n}"),
            uses_picks: true,
            after: if self.checked(FieldId::Commit) {
                AfterTask::Reload
            } else {
                AfterTask::LocalChanges
            },
            ..Default::default()
        })
    }

    // `base` is the commit the rebase starts after, None to rebase from the root.
    // `auto_stash` is the default of the checkbox, from rebase.autoStash.
    pub fn squash(
        branch: &str,
        commits: Vec<DialogCommit>,
        base: Option<String>,
        auto_stash: bool,
    ) -> Self {
        let intro = commits_intro(&format!("Squash into their parents, on {branch}"), &commits);
        let kind = DialogKind::Squash { commits, base };
        let mut dialog = Self::with(" Squash ", "squash", intro, kind);
        let messages = SQUASH_MESSAGES.map(String::from).to_vec();
        dialog.push_choice(FieldId::Messages, "Messages", messages, "");
        dialog.push_check(
            FieldId::AutoStash,
            "Stash and reapply local changes",
            auto_stash,
        );
        dialog
    }

    fn squash_command(
        &self,
        commits: &[DialogCommit],
        base: Option<&str>,
    ) -> Result<GitTask, String> {
        let auto_stash = self.checked(FieldId::AutoStash);
        let fixup = self.choice(FieldId::Messages) == 1;
        let action = if fixup { "fixup" } else { "squash" };
        let todo: Vec<String> = commits
            .iter()
            .map(|c| todo_action(c.hash.as_str(), action))
            .collect();
        let n = count_commits(commits.len());
        Ok(GitTask {
            running: format!("Squashing {n}"),
            done: format!("Squashed {n} into their parents"),
            uses_picks: true,
            ..GitTask::rebase_todo(base, auto_stash, &todo)
        })
    }

    // `commits` are the newest first, the order they are reverted in.
    pub fn revert(branch: &str, commits: Vec<DialogCommit>) -> Self {
        let intro = commits_intro(&format!("Revert on {branch}"), &commits);
        let kind = DialogKind::Revert { commits };
        let mut dialog = Self::with(" Revert ", "revert", intro, kind);
        dialog.push_check(FieldId::Commit, "Commit (else edit in local changes)", true);
        dialog
    }

    fn revert_command(&self, commits: &[DialogCommit]) -> Result<GitTask, String> {
        let mut args = vec!["revert".to_string(), "--no-edit".into()];
        if !self.checked(FieldId::Commit) {
            args.push("--no-commit".into());
        }
        args.extend(commits.iter().map(|c| c.hash.as_str().to_string()));
        let n = count_commits(commits.len());
        Ok(GitTask {
            commands: vec![args],
            running: format!("Reverting {n}"),
            done: format!("Reverted {n}"),
            uses_picks: true,
            after: if self.checked(FieldId::Commit) {
                AfterTask::Reload
            } else {
                AfterTask::LocalChanges
            },
            ..Default::default()
        })
    }

    // Edits `commit`: a rebase stopping at it, to amend it on the local changes page.
    pub fn edit(
        branch: &str,
        commit: DialogCommit,
        base: Option<String>,
        auto_stash: bool,
    ) -> Self {
        let intro = vec![
            format!("Edit on {branch}, amending it on the local changes page"),
            format!("  {}", commit.line),
        ];
        let kind = DialogKind::Edit { commit, base };
        let mut dialog = Self::with(" Edit commit ", "edit", intro, kind);
        dialog.push_check(
            FieldId::AutoStash,
            "Stash and reapply local changes",
            auto_stash,
        );
        dialog
    }

    fn edit_command(&self, commit: &DialogCommit, base: Option<&str>) -> Result<GitTask, String> {
        let auto_stash = self.checked(FieldId::AutoStash);
        let todo = [todo_action(commit.hash.as_str(), "edit")];
        let short = commit.hash.as_short_hash();
        Ok(GitTask {
            running: format!("Stopping at {short}"),
            done: format!("Editing {short}, commit to amend it and go on"),
            after: AfterTask::LocalChanges,
            ..GitTask::rebase_todo(base, auto_stash, &todo)
        })
    }

    // `base` is the commit the rebase starts after, None to rebase from the root.
    pub fn drop(
        branch: &str,
        commits: Vec<DialogCommit>,
        base: Option<String>,
        auto_stash: bool,
    ) -> Self {
        let intro = commits_intro(&format!("Drop from {branch}"), &commits);
        let kind = DialogKind::Drop { commits, base };
        let mut dialog = Self::with(" Drop ", "drop", intro, kind);
        dialog.push_check(
            FieldId::AutoStash,
            "Stash and reapply local changes",
            auto_stash,
        );
        dialog
    }

    fn drop_command(
        &self,
        commits: &[DialogCommit],
        base: Option<&str>,
    ) -> Result<GitTask, String> {
        let auto_stash = self.checked(FieldId::AutoStash);
        let todo: Vec<String> = commits
            .iter()
            .map(|c| todo_action(c.hash.as_str(), "drop"))
            .collect();
        let n = count_commits(commits.len());
        Ok(GitTask {
            running: format!("Dropping {n}"),
            done: format!("Dropped {n}"),
            uses_picks: true,
            ..GitTask::rebase_todo(base, auto_stash, &todo)
        })
    }

    // A branch at a commit of the graph, which nothing else can be chosen instead of.
    pub fn create_branch_at(commit: CommitHash) -> Self {
        let from = StartPoint::Commit(commit);
        let field = Field::Fixed(from.label());
        Self::create_branch(vec![from], field)
    }

    // A branch from a ref of the sidebar, or one of the local `branches` chosen instead.
    pub fn create_branch_from(from: NamedRef, branches: Vec<String>) -> Self {
        let others: Vec<NamedRef> = branches
            .into_iter()
            .map(NamedRef::Branch)
            .filter(|branch| *branch != from)
            .collect();
        let starts: Vec<StartPoint> = std::iter::once(from)
            .chain(others)
            .map(StartPoint::Ref)
            .collect();
        let options = starts.iter().map(StartPoint::label).collect();
        Self::create_branch(starts, Field::Choice { options, index: 0 })
    }

    // `from` is the field choosing among `starts`.
    fn create_branch(starts: Vec<StartPoint>, from: Field) -> Self {
        let kind = DialogKind::CreateBranch { starts };
        let mut dialog = Self::with(" New branch ", "create", vec![], kind);
        dialog.push_text(FieldId::Name, "Name", "", vec![]);
        dialog.fields.push((FieldId::From, "From".into(), from));
        dialog.push_check(FieldId::Checkout, "Check it out", true);
        dialog.editing = true;
        dialog
    }

    fn create_branch_command(&self, starts: &[StartPoint]) -> Result<GitTask, String> {
        let name = self.value(FieldId::Name);
        let start = &starts[self.choice(FieldId::From)];
        let from = start.revision();
        if name.is_empty() {
            return Err("Type a name for the branch".into());
        }
        // git would read it as an option, `-D` deleting the branch it starts from
        if name.starts_with('-') {
            return Err("A branch name can't start with -".into());
        }
        // tracking `from` when it is a remote branch, either way
        let args: &[&str] = if self.checked(FieldId::Checkout) {
            &["checkout", "-b", &name, &from]
        } else {
            &["branch", &name, &from]
        };
        Ok(GitTask::new(
            args,
            format!("Creating {name}"),
            format!("Created {name} from {}", start.label()),
        ))
    }

    // Local branches; `unmerged` are those whose commits are on no other branch, which it warns
    // about, as their commits may be lost. `upstreams` (remote, branch) are their upstreams still on
    // their remotes, which a checkbox deletes too.
    pub fn delete_branches(
        names: Vec<String>,
        unmerged: Vec<String>,
        upstreams: Vec<(String, String)>,
    ) -> Self {
        let intro = names_intro("Delete the branch", "Delete the branches", &names);
        let label = match upstreams.len() {
            0 => None,
            1 if names.len() == 1 => Some("Delete its upstream on the remote too"),
            _ => Some("Delete their upstreams on the remotes too"),
        };
        let kind = DialogKind::DeleteBranches {
            names,
            unmerged,
            upstreams,
        };
        let mut dialog = Self::with(" Delete branch ", "delete", intro, kind);
        if let Some(label) = label {
            dialog.push_check(FieldId::Force, label, false);
        }
        dialog
    }

    // confirmed with the warning when it isn't merged
    fn delete_branches_command(
        &self,
        names: &[String],
        upstreams: &[(String, String)],
    ) -> Result<GitTask, String> {
        let mut args = vec!["branch".to_string(), "-D".into()];
        args.extend(names.iter().cloned());
        let what = names_label("branch", "branches", names);
        let mut task = GitTask {
            commands: vec![args],
            running: format!("Deleting {what}"),
            done: format!("Deleted {what}"),
            ..Default::default()
        };
        if self.checked(FieldId::Force) {
            // the remote first: should it fail, offline say, nothing is deleted yet to retry it
            task.commands.splice(0..0, delete_on_remotes(upstreams));
            task.remote = true;
            task.done = format!("Deleted {what}, also on the remote");
        }
        Ok(task)
    }

    // Branches on their remotes, as (remote, branch).
    pub fn delete_remote_branches(branches: Vec<(String, String)>) -> Self {
        let names: Vec<String> = branches.iter().map(|(r, b)| format!("{r}/{b}")).collect();
        let intro = names_intro(
            "Delete from its remote the branch",
            "Delete from their remotes the branches",
            &names,
        );
        let kind = DialogKind::DeleteRemoteBranches { branches };
        Self::with(" Delete branch ", "delete", intro, kind)
    }

    fn delete_remote_branches_command(
        &self,
        branches: &[(String, String)],
    ) -> Result<GitTask, String> {
        let commands = delete_on_remotes(branches);
        let names: Vec<String> = branches.iter().map(|(r, b)| format!("{r}/{b}")).collect();
        let what = names_label("branch", "branches", &names);
        Ok(GitTask {
            commands,
            running: format!("Deleting {what}"),
            done: format!("Deleted {what}"),
            remote: true,
            ..Default::default()
        })
    }

    // Renames a local branch or changes its upstream, one of `remote_branches` (like
    // "origin/main") or none.
    pub fn edit_branch(name: &str, upstream: Option<String>, remote_branches: Vec<String>) -> Self {
        // the current upstream stays a choice, even when its remote branch is gone
        let mut upstreams = remote_branches;
        if let Some(upstream) = upstream.as_ref().filter(|u| !upstreams.contains(u)) {
            upstreams.insert(0, upstream.clone());
        }
        let options = std::iter::once("None".to_string())
            .chain(upstreams.iter().cloned())
            .collect();
        let index = upstream
            .as_ref()
            .and_then(|u| upstreams.iter().position(|o| o == u))
            .map_or(0, |i| i + 1);
        let kind = DialogKind::EditBranch {
            name: name.to_string(),
            upstream,
            upstreams,
        };
        let mut dialog = Self::with(" Edit branch ", "save", vec![], kind);
        dialog.push_text(FieldId::Name, "Name", name, vec![]);
        dialog.fields.push((
            FieldId::Upstream,
            "Upstream".into(),
            Field::Choice { options, index },
        ));
        dialog.editing = true;
        dialog
    }

    fn edit_branch_command(
        &self,
        name: &str,
        upstream: &Option<String>,
        upstreams: &[String],
    ) -> Result<GitTask, String> {
        // the first choice is None
        let new_upstream = self
            .choice(FieldId::Upstream)
            .checked_sub(1)
            .map(|i| upstreams[i].clone());
        let new_name = self.value(FieldId::Name);
        if new_name.is_empty() {
            return Err("Type a name for the branch".into());
        }
        // git would read it as an option, `-f` overwriting the branch with the current one
        if new_name.starts_with('-') {
            return Err("A branch name can't start with -".into());
        }
        let mut commands: Vec<Vec<String>> = vec![];
        if new_name != *name {
            commands.push(vec![
                "branch".into(),
                "-m".into(),
                name.to_string(),
                new_name.clone(),
            ]);
        }
        if new_upstream != *upstream {
            let upstream_arg = match new_upstream {
                None => "--unset-upstream".to_string(),
                Some(new_upstream) => format!("--set-upstream-to={new_upstream}"),
            };
            commands.push(vec!["branch".into(), upstream_arg, new_name.clone()]);
        }
        if commands.is_empty() {
            return Err("Change the name or the upstream".into());
        }
        Ok(GitTask {
            commands,
            running: format!("Saving {new_name}"),
            done: format!("Saved {new_name}"),
            ..Default::default()
        })
    }

    // Renames a remote or changes its URL.
    pub fn edit_remote(name: &str, url: &str) -> Self {
        let kind = DialogKind::EditRemote {
            name: name.to_string(),
            url: url.to_string(),
        };
        let mut dialog = Self::remote_form(" Edit remote ", "save", kind, name, url);
        dialog.editing = true;
        dialog
    }

    fn edit_remote_command(&self, name: &str, url: &str) -> Result<GitTask, String> {
        let (new_name, new_url) = self.remote_values()?;
        let mut commands: Vec<Vec<String>> = vec![];
        if new_name != *name {
            commands.push(vec![
                "remote".into(),
                "rename".into(),
                name.to_string(),
                new_name.clone(),
            ]);
        }
        if new_url != *url {
            commands.push(vec![
                "remote".into(),
                "set-url".into(),
                new_name.clone(),
                new_url,
            ]);
        }
        if commands.is_empty() {
            return Err("Change the name or the URL".into());
        }
        Ok(GitTask {
            commands,
            running: format!("Saving {new_name}"),
            done: format!("Saved {new_name}"),
            ..Default::default()
        })
    }

    // A new remote, fetched once added, like lazygit.
    pub fn add_remote() -> Self {
        let mut dialog = Self::remote_form(" New remote ", "add", DialogKind::AddRemote, "", "");
        dialog.editing = true;
        dialog
    }

    fn add_remote_command(&self) -> Result<GitTask, String> {
        let (name, url) = self.remote_values()?;
        Ok(GitTask {
            commands: vec![
                vec!["remote".into(), "add".into(), name.clone(), url],
                vec!["fetch".into(), name.clone()],
            ],
            running: format!("Adding {name}"),
            done: format!("Added and fetched {name}"),
            remote: true,
            ..Default::default()
        })
    }

    // The name and URL of a remote.
    fn remote_form(
        title: &'static str,
        verb: &'static str,
        kind: DialogKind,
        name: &str,
        url: &str,
    ) -> Self {
        let mut dialog = Self::with(title, verb, vec![], kind);
        dialog.push_text(FieldId::Name, "Name", name, vec![]);
        dialog.push_text(FieldId::Url, "URL", url, vec![]);
        dialog
    }

    // The name and URL typed for a remote.
    fn remote_values(&self) -> Result<(String, String), String> {
        let name = self.value(FieldId::Name);
        let url = self.value(FieldId::Url);
        if name.is_empty() || url.is_empty() {
            return Err("Type a name and a URL for the remote".into());
        }
        Ok((name, url))
    }

    // A tag at `at`, annotated when it has a message.
    pub fn create_tag(at: StartPoint) -> Self {
        let label = at.label();
        let mut dialog = Self::with(" New tag ", "create", vec![], DialogKind::CreateTag { at });
        dialog.push_text(FieldId::Name, "Name", "", vec![]);
        dialog.push_text(FieldId::Message, "Message", "", vec![]);
        // where the cursor is, nothing else to choose
        dialog
            .fields
            .push((FieldId::At, "At".into(), Field::Fixed(label)));
        dialog.editing = true;
        dialog
    }

    fn create_tag_command(&self, at: &StartPoint) -> Result<GitTask, String> {
        let name = self.value(FieldId::Name);
        let message = self.value(FieldId::Message);
        let (label, at) = (at.label(), at.revision());
        if name.is_empty() {
            return Err("Type a name for the tag".into());
        }
        // git would read it as an option, `-d` deleting the tag it is made at
        if name.starts_with('-') {
            return Err("A tag name can't start with -".into());
        }
        let mut args = vec!["tag"];
        if !message.is_empty() {
            args.extend(["-a", "-m", &message]);
        }
        args.extend([name.as_str(), &at]);
        Ok(GitTask::new(
            &args,
            format!("Tagging {label} {name}"),
            format!("Tagged {label} {name}"),
        ))
    }

    // `remote` is where they may be deleted too, when there are remotes.
    // Removes a remote, and its remote branches from this repository.
    pub fn remove_remote(name: &str) -> Self {
        let intro = vec![format!("Remove the remote {name}")];
        let kind = DialogKind::RemoveRemote {
            name: name.to_string(),
        };
        Self::with(" Remove remote ", "remove", intro, kind)
    }

    pub fn delete_tags(names: Vec<String>, remote: Option<String>) -> Self {
        let intro = names_intro("Delete the tag", "Delete the tags", &names);
        // named, as it may not be the remote meant: origin, else the first one
        let label = remote
            .as_ref()
            .map(|remote| format!("Delete them from {remote} too"));
        let kind = DialogKind::DeleteTags { names, remote };
        let mut dialog = Self::with(" Delete tag ", "delete", intro, kind);
        if let Some(label) = label {
            dialog.push_check(FieldId::Force, &label, false);
        }
        dialog
    }

    fn delete_tags_command(
        &self,
        names: &[String],
        remote: Option<&str>,
    ) -> Result<GitTask, String> {
        let mut args = vec!["tag".to_string(), "-d".into()];
        args.extend(names.iter().cloned());
        let what = names_label("tag", "tags", names);
        let mut task = GitTask {
            commands: vec![args],
            running: format!("Deleting {what}"),
            done: format!("Deleted {what}"),
            ..Default::default()
        };
        if let Some(remote) = remote.filter(|_| self.checked(FieldId::Force)) {
            let mut push = vec!["push".to_string(), remote.to_string(), "--delete".into()];
            push.extend(names.iter().map(|name| format!("refs/tags/{name}")));
            // the remote first: should it fail, offline say, nothing is deleted yet to retry it
            task.commands.insert(0, push);
            task.remote = true;
            task.done = format!("Deleted {what}, also from {remote}");
        }
        Ok(task)
    }

    // `name` is like "stash@{0}", `message` what it says.
    pub fn apply_stash(name: &str, message: &str) -> Self {
        let intro = vec![format!("Apply {name}"), format!("  {message}")];
        let kind = DialogKind::ApplyStash {
            name: name.to_string(),
        };
        let mut dialog = Self::with(" Apply stash ", "apply", intro, kind);
        dialog.push_check(FieldId::Index, "Restore what was staged as staged", false);
        dialog.push_check(FieldId::Pop, "Drop it once applied (pop)", false);
        dialog
    }

    fn apply_stash_command(&self, name: &str) -> Result<GitTask, String> {
        // popping drops it once applied, git keeps it when it stops on conflicts
        let pop = self.checked(FieldId::Pop);
        let mut args = vec!["stash", if pop { "pop" } else { "apply" }];
        if self.checked(FieldId::Index) {
            args.push("--index");
        }
        args.push(name);
        let (running, done) = if pop {
            (
                format!("Popping {name}"),
                format!("Applied and dropped {name}"),
            )
        } else {
            (format!("Applying {name}"), format!("Applied {name}"))
        };
        Ok(GitTask {
            after: AfterTask::LocalChanges,
            ..GitTask::new(&args, running, done)
        })
    }

    // `stashes` are like ("stash@{0}", its message).
    pub fn drop_stashes(stashes: Vec<(String, String)>) -> Self {
        let lines: Vec<String> = stashes.iter().map(|(n, m)| format!("{n} {m}")).collect();
        let intro = names_intro("Drop", "Drop the stashes", &lines);
        let names = stashes.into_iter().map(|(name, _)| name).collect();
        Self::with(
            " Drop stash ",
            "drop",
            intro,
            DialogKind::DropStashes { names },
        )
    }

    fn drop_stashes_command(&self, names: &[String]) -> Result<GitTask, String> {
        // the newest first would renumber the older ones, so the oldest go first
        let mut names = names.to_vec();
        names.sort_by_key(|name| std::cmp::Reverse(stash_index(name)));
        let what = names_label("stash", "stashes", &names);
        let commands = names
            .iter()
            .map(|name| vec!["stash".to_string(), "drop".into(), name.clone()]);
        Ok(GitTask {
            commands: commands.collect(),
            running: format!("Dropping {what}"),
            done: format!("Dropped {what}"),
            ..Default::default()
        })
    }

    // Confirms opening a link in the browser, choosing it when there are several; or which to copy.
    pub fn open_link(links: Vec<PageLink>, action: LinkAction) -> Self {
        let (title, verb, trigger) = match action {
            LinkAction::Copy => (" Copy link ", "copy", UserEvent::CopyLink),
            LinkAction::Open => (" Open in browser ", "open", UserEvent::OpenInBrowser),
        };
        // the links of each remote, which has the same pages as the others, see `browse::links`
        let links: Vec<Vec<PageLink>> = links
            .chunk_by(|a, b| a.remote == b.remote)
            .map(<[PageLink]>::to_vec)
            .collect();
        let remotes: Vec<String> = links.iter().map(|pages| pages[0].remote.clone()).collect();
        let pages: Vec<String> = links[0].iter().map(|link| link.page.clone()).collect();
        let mut dialog = Self::with(title, verb, vec![], DialogKind::OpenLink { links, action });
        if remotes.len() > 1 {
            dialog.push_choice(FieldId::Remote, "Remote", remotes, "");
        }
        if pages.len() > 1 {
            dialog.push_choice(FieldId::Link, "Page", pages, "");
        }
        dialog.trigger = Some(trigger);
        dialog
    }

    pub fn reset(branch: &str, commit: DialogCommit) -> Self {
        let intro = vec![format!("Reset {branch} to"), format!("  {}", commit.line)];
        let kind = DialogKind::Reset {
            commit,
            branch: branch.to_string(),
        };
        let mut dialog = Self::with(" Reset ", "reset", intro, kind);
        let modes = RESET_MODES.map(String::from).to_vec();
        dialog.push_choice(FieldId::ResetMode, "Mode", modes, "");
        dialog
    }

    fn reset_command(&self, commit: &DialogCommit, branch: &str) -> Result<GitTask, String> {
        let mode = ["--mixed", "--soft", "--hard"][self.choice(FieldId::ResetMode)];
        let short = commit.hash.as_short_hash();
        Ok(GitTask::new(
            &["reset", mode, commit.hash.as_str()],
            format!("Resetting {branch} to {short}"),
            format!("Reset {branch} to {short}"),
        ))
    }

    // Resets a file `commit` changed, in the index and the working tree, to how the commit left it
    // or how it was before it.
    pub fn reset_file(commit: DialogCommit, change: FileChange) -> Self {
        let path = match &change {
            FileChange::Add { path }
            | FileChange::Modify { path }
            | FileChange::Delete { path } => path,
            FileChange::Move { to, .. } => to,
        };
        let intro = vec![format!("Reset {path} to"), format!("  {}", commit.line)];
        let kind = DialogKind::ResetFile {
            commit,
            change: Box::new(change),
        };
        let mut dialog = Self::with(" Reset file ", "reset", intro, kind);
        let states = FILE_STATES.map(String::from).to_vec();
        dialog.push_choice(FieldId::FileState, "State", states, "");
        dialog
    }

    fn reset_file_command(
        &self,
        commit: &DialogCommit,
        change: &FileChange,
    ) -> Result<GitTask, String> {
        use FileChange::*;
        let before = self.choice(FieldId::FileState) == 1;
        // the path the file has then, and the one it doesn't, removed: before the commit added
        // it, after it deleted it, and either name of a renamed file
        let (present, absent) = match (change, before) {
            (Modify { path }, _) | (Add { path }, false) | (Delete { path }, true) => {
                (Some(path), None)
            }
            (Add { path }, true) | (Delete { path }, false) => (None, Some(path)),
            (Move { from, to }, false) => (Some(to), Some(from)),
            (Move { from, to }, true) => (Some(from), Some(to)),
        };
        let source = if before {
            format!("{}^", commit.hash.as_str())
        } else {
            commit.hash.as_str().to_string()
        };
        let mut commands: Vec<Vec<String>> = vec![];
        if let Some(path) = absent {
            let args = ["rm", "-q", "-f", "--ignore-unmatch", "--", path];
            commands.push(args.map(String::from).to_vec());
        }
        if let Some(path) = present {
            let args = [
                "restore",
                "--source",
                &source,
                "--staged",
                "--worktree",
                "--",
                path,
            ];
            commands.push(args.map(String::from).to_vec());
        }
        let name = present.or(absent).cloned().unwrap_or_default();
        let at = if before { "before" } else { "at" };
        let short = commit.hash.as_short_hash();
        Ok(GitTask {
            commands,
            running: format!("Resetting {name}"),
            done: format!("Reset {name} to {at} {short}"),
            after: AfterTask::KeepView,
            ..Default::default()
        })
    }

    fn info(&self) -> &RemoteInfo {
        match &self.kind {
            DialogKind::Remote(_, info) => info,
            _ => unreachable!("only the remote popups have remote info"),
        }
    }

    fn push_choice(&mut self, id: FieldId, label: &str, options: Vec<String>, value: &str) {
        let index = options.iter().position(|o| o == value).unwrap_or(0);
        self.fields
            .push((id, label.into(), Field::Choice { options, index }));
    }

    // A text field, `options` completing it.
    fn push_text(&mut self, id: FieldId, label: &str, value: &str, options: Vec<String>) {
        self.fields
            .push((id, label.into(), Field::text(value, options)));
    }

    fn push_check(&mut self, id: FieldId, label: &str, checked: bool) {
        self.fields.push((id, label.into(), Field::Check(checked)));
    }

    // The upstream remote of the branch, else origin, else the first one.
    fn default_remote(&self, branch: Option<&str>) -> String {
        let info = self.info();
        branch
            .and_then(|branch| info.upstreams.get(branch))
            .map(|(remote, _)| remote.clone())
            .filter(|remote| info.remotes.contains(remote))
            .or_else(|| info.remotes.iter().find(|r| *r == "origin").cloned())
            .unwrap_or_else(|| info.remotes[0].clone())
    }

    fn field(&self, id: FieldId) -> Option<&Field> {
        self.fields.iter().find(|(i, ..)| *i == id).map(|(.., f)| f)
    }

    fn field_mut(&mut self, id: FieldId) -> Option<&mut Field> {
        self.fields
            .iter_mut()
            .find(|(i, ..)| *i == id)
            .map(|(.., f)| f)
    }

    // The text of a text or choice field the popup has; its command reads only those.
    fn value(&self, id: FieldId) -> String {
        match self.field(id) {
            Some(Field::Choice { options, index }) => options[*index].clone(),
            Some(Field::Text { input, .. }) => input.value().trim().to_string(),
            Some(Field::Fixed(value)) => value.clone(),
            _ => panic!("the popup has no text or choice field {id:?}"),
        }
    }

    // The option chosen in a choice field the popup has.
    fn choice(&self, id: FieldId) -> usize {
        match self.field(id) {
            Some(Field::Choice { index, .. }) => *index,
            // the only value
            Some(Field::Fixed(_)) => 0,
            _ => panic!("the popup has no choice field {id:?}"),
        }
    }

    // A checkbox not offered, like deleting on a remote when there is none, isn't checked.
    fn checked(&self, id: FieldId) -> bool {
        matches!(self.field(id), Some(Field::Check(true)))
    }

    fn remote_branches(&self, remote: &str) -> Vec<String> {
        self.info()
            .remote_branches
            .get(remote)
            .cloned()
            .unwrap_or_default()
    }

    // The branch to pull from the chosen remote: the one chosen before when the remote has it too,
    // else the upstream of the current branch, else the current branch, else the first one.
    fn update_pull_branch(&mut self, previous: Option<String>) {
        let remote = self.value(FieldId::Remote);
        let options = self.remote_branches(&remote);
        let current = self.info().current_branch.clone();
        let upstream = current
            .as_ref()
            .and_then(|branch| self.info().upstreams.get(branch))
            .filter(|(r, _)| *r == remote)
            .map(|(_, branch)| branch.clone());
        let value = previous
            .filter(|branch| options.contains(branch))
            .or(upstream)
            .or_else(|| current.filter(|branch| options.contains(branch)))
            .or_else(|| options.first().cloned())
            .unwrap_or_default();
        if let Some(field) = self.field_mut(FieldId::Branch) {
            let index = options.iter().position(|o| *o == value).unwrap_or(0);
            *field = Field::Choice { options, index };
        }
    }

    // Stashing follows rebase.autoStash when rebasing and merge.autoStash when merging,
    // until it is changed.
    fn update_pull_auto_stash(&mut self) {
        if self.auto_stash_changed {
            return;
        }
        let default = self.info().auto_stash(self.checked(FieldId::Rebase));
        if let Some(Field::Check(checked)) = self.field_mut(FieldId::AutoStash) {
            *checked = default;
        }
    }

    // The remote to push the chosen branch to, then the branch on it.
    fn update_push_remote(&mut self) {
        let branch = self.value(FieldId::LocalBranch);
        let remote = self.default_remote(Some(&branch));
        if let Some(Field::Choice { options, index }) = self.field_mut(FieldId::Remote) {
            *index = options.iter().position(|o| *o == remote).unwrap_or(0);
        }
        self.update_push_remote_branch();
    }

    // The upstream branch when pushing to the upstream remote, else the same name.
    fn update_push_remote_branch(&mut self) {
        let branch = self.value(FieldId::LocalBranch);
        let remote = self.value(FieldId::Remote);
        let value = self
            .info()
            .upstreams
            .get(&branch)
            .filter(|(r, _)| *r == remote)
            .map(|(_, b)| b.clone())
            .unwrap_or_else(|| branch.clone());
        let options = self.remote_branches(&remote);
        if let Some(field) = self.field_mut(FieldId::RemoteBranch) {
            *field = Field::text(&value, options);
        }
    }

    // `event` is what the key is bound to, keys are typed as they are while editing a text field.
    pub fn handle_key(&mut self, event: Option<UserEvent>, key: KeyEvent) -> DialogOutcome {
        if self.editing {
            match key.code {
                // on to the next required field still empty, if any
                KeyCode::Enter => match self.missing_field() {
                    Some(index) => self.selected = index,
                    None => self.editing = false,
                },
                KeyCode::Esc => self.editing = false,
                _ => {
                    if let Field::Text { input, .. } = &mut self.fields[self.selected].2 {
                        input.handle_event(&Event::Key(key));
                    }
                }
            }
            return DialogOutcome::None;
        }
        let len = self.fields.len().max(1);
        // the key that opened the popup confirms it, like enter
        let event = if event.is_some() && event == self.trigger {
            Some(UserEvent::Confirm)
        } else {
            event
        };
        match event {
            Some(UserEvent::Cancel) => DialogOutcome::Close,
            Some(UserEvent::Quit) => DialogOutcome::Quit,
            Some(UserEvent::NavigateDown | UserEvent::SelectDown) => {
                self.selected = (self.selected + 1) % len;
                DialogOutcome::None
            }
            Some(UserEvent::NavigateUp | UserEvent::SelectUp) => {
                self.selected = (self.selected + len - 1) % len;
                DialogOutcome::None
            }
            Some(UserEvent::NavigateRight) => {
                self.change(Toward::Next);
                DialogOutcome::None
            }
            Some(UserEvent::NavigateLeft) => {
                self.change(Toward::Previous);
                DialogOutcome::None
            }
            Some(UserEvent::DialogToggle) => {
                if self.can_toggle() {
                    self.change(Toward::Next);
                }
                DialogOutcome::None
            }
            Some(UserEvent::DialogEdit) => {
                self.editing = self.can_edit();
                DialogOutcome::None
            }
            // toggles when the field can be, else types into it
            Some(UserEvent::DialogActivate) => {
                if self.can_toggle() {
                    self.change(Toward::Next);
                } else {
                    self.editing = self.can_edit();
                }
                DialogOutcome::None
            }
            // a required field still empty is typed into first
            Some(UserEvent::Confirm) => match self.missing_field() {
                Some(index) => {
                    self.selected = index;
                    self.editing = true;
                    DialogOutcome::None
                }
                None => self.run(),
            },
            _ => DialogOutcome::None,
        }
    }

    // The next required text field still empty, after the selected one: a name, a URL, the branch
    // to pull or to push to.
    fn missing_field(&self) -> Option<usize> {
        let len = self.fields.len();
        (1..=len)
            .map(|step| (self.selected + step) % len)
            .find(|&index| match &self.fields[index] {
                (
                    FieldId::Name | FieldId::Url | FieldId::RemoteBranch,
                    _,
                    Field::Text { input, .. },
                ) => input.value().trim().is_empty(),
                _ => false,
            })
    }

    // The keys of the popup and its selected field, see `Keys`.
    pub fn register_keys(k: &mut Keys<Self>) {
        use UserEvent::*;
        const RUN: &str = "Run the command (the key that opened the popup too)";
        let typing = |d: &Self| d.editing;
        let idle = |d: &Self| !d.editing;
        let next_missing = |d: &Self| d.missing_field().is_some_and(|i| i != d.selected);
        k.add(
            &[Confirm],
            "next",
            "While typing, go on to the next empty required field, else stop typing",
        )
        .when(move |d| typing(d) && next_missing(d));
        // a required field still empty keeps enter typing into it
        k.add(
            &[Confirm],
            "done typing",
            "While typing, go on to the next empty required field, else stop typing",
        )
        .when(|d| d.editing && d.missing_field().is_none());
        k.add(&[Cancel], "done typing", "While typing, stop typing")
            .when(typing);
        // space toggles the field when it can, else types into it
        k.add(
            &[DialogActivate, DialogToggle],
            "toggle",
            "Toggle the checkbox, or go to the next option",
        )
        .when(|d| idle(d) && d.can_toggle());
        k.add(&[DialogEdit], "type", "Type into the text field")
            .when(|d| idle(d) && d.can_toggle() && d.can_edit());
        k.add(
            &[DialogActivate, DialogEdit],
            "type",
            "Type into the text field",
        )
        .when(|d| idle(d) && !d.can_toggle() && d.can_edit());
        k.add(&[Confirm], "run", RUN)
            .when(idle)
            .short(|d| d.verb.to_string());
        if let Some(trigger) = k.view().and_then(|d| d.trigger) {
            k.add(&[trigger], "run", RUN)
                .when(idle)
                .short(|d| d.verb.to_string());
        }
        k.add(&[Cancel], "cancel", "Cancel").when(idle);
        k.quiet(
            &[NavigateDown, SelectDown, NavigateUp, SelectUp],
            "Select the next/previous field",
        )
        .when(idle);
        k.quiet(&[NavigateRight, NavigateLeft], "Change the selected field")
            .when(idle);
        k.quiet(&[Quit], "Quit app").when(idle);
    }

    pub fn keys(&self) -> KeyList {
        let mut keys = Keys::of(self);
        Self::register_keys(&mut keys);
        keys.list()
    }

    // Whether the selected field is a checkbox, or has options to go through.
    fn can_toggle(&self) -> bool {
        match self.fields.get(self.selected) {
            Some((_, _, Field::Check(_))) => true,
            // a single option is fixed, like where a branch starts from a commit
            Some((_, _, Field::Choice { options, .. })) => options.len() > 1,
            Some((_, _, Field::Text { options, .. })) => !options.is_empty(),
            Some((_, _, Field::Fixed(_))) | None => false,
        }
    }

    fn can_edit(&self) -> bool {
        matches!(
            self.fields.get(self.selected),
            Some((_, _, Field::Text { .. }))
        )
    }

    fn run(&self) -> DialogOutcome {
        let event = match &self.kind {
            DialogKind::OpenLink { links, action } => AppEvent::OpenUrl {
                url: self.chosen_link(links).url.clone(),
                action: *action,
            },
            _ => match self.command() {
                Some(Ok(task)) => AppEvent::StartTask(task),
                Some(Err(err)) => return DialogOutcome::Error(err),
                None => return DialogOutcome::None,
            },
        };
        DialogOutcome::Send(Box::new(event))
    }

    // The link on the remote and the page chosen, either the only one when it has no field.
    fn chosen_link<'l>(&self, links: &'l [Vec<PageLink>]) -> &'l PageLink {
        // without a field, the only remote or page
        let index = |id| match self.field(id) {
            Some(Field::Choice { index, .. }) => *index,
            _ => 0,
        };
        &links[index(FieldId::Remote)][index(FieldId::Link)]
    }

    // Changes the selected field to its next or previous option, or toggles it.
    fn change(&mut self, toward: Toward) {
        // the branch to pull, kept on another remote having it too
        let previous_branch = matches!(self.kind, DialogKind::Remote(RemoteAction::Pull, _))
            .then(|| self.value(FieldId::Branch));
        let Some((id, _, field)) = self.fields.get_mut(self.selected) else {
            return;
        };
        let id = *id;
        match field {
            Field::Choice { options, index } => {
                *index = toward.around(Some(*index), options.len());
            }
            Field::Text { input, options } => {
                if options.is_empty() {
                    return;
                }
                let current = options.iter().position(|o| o == input.value().trim());
                let next = toward.around(current, options.len());
                *input = Input::new(options[next].clone());
            }
            Field::Check(checked) => *checked = !*checked,
            Field::Fixed(_) => {}
        }
        // the fields that follow the one changed
        let action = match &self.kind {
            DialogKind::Remote(action, _) => Some(*action),
            _ => None,
        };
        match (action, id) {
            (Some(RemoteAction::Pull), FieldId::Remote) => {
                self.update_pull_branch(previous_branch);
            }
            (Some(RemoteAction::Push), FieldId::LocalBranch) => self.update_push_remote(),
            (Some(RemoteAction::Push), FieldId::Remote) => self.update_push_remote_branch(),
            (Some(RemoteAction::Pull), FieldId::Rebase) => self.update_pull_auto_stash(),
            (_, FieldId::AutoStash) => self.auto_stash_changed = true,
            _ => {}
        }
    }

    // The git command of the popup; None for a link, which is opened instead, see `run`.
    pub fn command(&self) -> Option<Result<GitTask, String>> {
        let task = self.kind_command()?;
        Some(task.map(|mut task| {
            if let Some(revision) = &self.select {
                task.after = AfterTask::Select(revision.clone());
            }
            task
        }))
    }

    // The git commands of the popup, built by the command of each kind next to its constructor.
    fn kind_command(&self) -> Option<Result<GitTask, String>> {
        Some(match &self.kind {
            DialogKind::OpenLink { .. } => return None,
            DialogKind::Remote(action, _) => self.remote_command(*action),
            DialogKind::Rebase { onto, description } => self.rebase_command(onto, description),
            DialogKind::Merge {
                source,
                description,
            } => self.merge_command(source, description),
            DialogKind::Checkout { targets } => self.checkout_command(targets),
            DialogKind::CherryPick { commits } => self.cherry_pick_command(commits),
            DialogKind::Squash { commits, base } => self.squash_command(commits, base.as_deref()),
            DialogKind::Edit { commit, base } => self.edit_command(commit, base.as_deref()),
            DialogKind::Drop { commits, base } => self.drop_command(commits, base.as_deref()),
            DialogKind::CreateBranch { starts } => self.create_branch_command(starts),
            DialogKind::DeleteBranches {
                names, upstreams, ..
            } => self.delete_branches_command(names, upstreams),
            DialogKind::DeleteRemoteBranches { branches } => {
                self.delete_remote_branches_command(branches)
            }
            DialogKind::EditBranch {
                name,
                upstream,
                upstreams,
            } => self.edit_branch_command(name, upstream, upstreams),
            DialogKind::EditRemote { name, url } => self.edit_remote_command(name, url),
            DialogKind::AddRemote => self.add_remote_command(),
            DialogKind::RemoveRemote { name } => Ok(GitTask::new(
                &["remote", "remove", name],
                format!("Removing {name}"),
                format!("Removed {name}"),
            )),
            DialogKind::CreateTag { at } => self.create_tag_command(at),
            DialogKind::DeleteTags { names, remote } => {
                self.delete_tags_command(names, remote.as_deref())
            }
            DialogKind::Prepared { task, .. } | DialogKind::Commit { task, .. } => {
                Ok(*task.clone())
            }
            DialogKind::Stash { paths, target } => self.stash_command(paths, target),
            DialogKind::ApplyStash { name } => self.apply_stash_command(name),
            DialogKind::DropStashes { names } => self.drop_stashes_command(names),
            DialogKind::Revert { commits } => self.revert_command(commits),
            DialogKind::Reset { commit, branch } => self.reset_command(commit, branch),
            DialogKind::ResetFile { commit, change } => self.reset_file_command(commit, change),
        })
    }

    // What the chosen options lose, shown in the warning color.
    fn warning(&self) -> Option<String> {
        let warning = match &self.kind {
            DialogKind::Reset { .. } if self.choice(FieldId::ResetMode) == 2 => {
                Some("The local changes are lost.")
            }
            DialogKind::Checkout { .. } if self.choice(FieldId::LocalChanges) == 2 => {
                Some("Changes to tracked files are lost, untracked files are kept.")
            }
            DialogKind::Remote(RemoteAction::Push, _) if self.checked(FieldId::Force) => {
                Some("Commits only on the remote branch are lost.")
            }
            DialogKind::ResetFile { .. } => Some("Its local changes are lost."),
            DialogKind::Squash { .. } => Some("The commits after them are rewritten too."),
            DialogKind::Drop { .. } => {
                Some("Their changes are lost, and the commits after them are rewritten.")
            }
            DialogKind::DeleteRemoteBranches { .. } => {
                Some("They are deleted on the remote, for everyone.")
            }
            DialogKind::RemoveRemote { .. } => {
                Some("Its branches here and the upstreams set to them go; the remote is untouched.")
            }
            DialogKind::DeleteBranches {
                names,
                unmerged,
                upstreams,
            } => {
                let unmerged = match (names.len(), unmerged.as_slice()) {
                    (_, []) => None,
                    (1, _) => Some("Its commits on no other branch are lost.".into()),
                    (_, [one]) => {
                        Some(format!("The commits of {one} on no other branch are lost."))
                    }
                    (_, more) => Some(format!(
                        "The commits of {} on no other branch are lost.",
                        more.join(", ")
                    )),
                };
                // the upstream may be another branch, like main
                let remote = self.checked(FieldId::Force).then(|| {
                    let names: Vec<String> =
                        upstreams.iter().map(|(r, b)| format!("{r}/{b}")).collect();
                    format!("Deletes {} for everyone.", names.join(", "))
                });
                // the remote first, which can't be undone here
                let warnings: Vec<String> = remote.into_iter().chain(unmerged).collect();
                return (!warnings.is_empty()).then(|| warnings.join("\n"));
            }
            DialogKind::DropStashes { .. } => Some("The stashed changes are lost."),
            DialogKind::Prepared { warning, .. } => Some(*warning),
            DialogKind::Commit { preview, .. } if preview.amend => {
                Some("The last commit is rewritten.")
            }
            _ => None,
        };
        warning.map(String::from)
    }

    // The git commands it will run, under the fields.
    fn summary(&self) -> String {
        if let DialogKind::OpenLink { links, .. } = &self.kind {
            return self.chosen_link(links).url.clone();
        }
        let task = match self.command() {
            Some(Ok(task)) => task,
            Some(Err(err)) => return err,
            None => return String::new(),
        };
        let git = |args: &[String]| {
            let args: Vec<_> = args.iter().map(|a| shorten_hash(a)).collect();
            format!("git {}", args.join(" "))
        };
        // with those run after it
        let command = task
            .commands
            .iter()
            .map(|args| git(args))
            .collect::<Vec<_>>()
            .join("; ");
        if task.stash {
            format!("git stash; {command}; git stash pop")
        } else {
            command
        }
    }
}

impl Field {
    fn text(value: &str, options: Vec<String>) -> Field {
        Field::Text {
            input: Input::new(value.to_string()),
            options,
        }
    }
}

fn count_commits(n: usize) -> String {
    if n == 1 {
        "1 commit".into()
    } else {
        format!("{n} commits")
    }
}

// The heading and the commits under it, at most a screenful.
// "Delete the branch main", or the heading over a line for each name.
fn names_intro(one: &str, many: &str, names: &[String]) -> Vec<String> {
    match names {
        [name] => vec![format!("{one} {name}")],
        _ => {
            let mut lines = vec![format!("{many}:")];
            lines.extend(
                capped(names.to_vec(), MAX_LISTED)
                    .iter()
                    .map(|n| format!("  {n}")),
            );
            lines
        }
    }
}

// the most items a popup lists, so it fits on the screen
const MAX_LISTED: usize = 10;

// The first `max` items, then a line saying how many more were left out.
fn capped(mut items: Vec<String>, max: usize) -> Vec<String> {
    if items.len() > max {
        let rest = items.len() - max;
        items.truncate(max);
        items.push(format!("… and {rest} more"));
    }
    items
}

// "main", or "3 branches".
fn names_label(one: &str, many: &str, names: &[String]) -> String {
    match names {
        [name] => name.clone(),
        _ => format!(
            "{} {}",
            names.len(),
            if names.len() == 1 { one } else { many }
        ),
    }
}

// Deletes the branches on their remotes, as (remote, branch): one push for those of each remote.
fn delete_on_remotes(branches: &[(String, String)]) -> Vec<Vec<String>> {
    let mut remotes: Vec<&String> = branches.iter().map(|(r, _)| r).collect();
    remotes.sort();
    remotes.dedup();
    remotes
        .into_iter()
        .map(|remote| {
            let mut args = vec!["push".to_string(), remote.clone(), "--delete".into()];
            args.extend(
                branches
                    .iter()
                    .filter(|(r, _)| r == remote)
                    // in full, as a tag of the same name makes the short one ambiguous
                    .map(|(_, b)| format!("refs/heads/{b}")),
            );
            args
        })
        .collect()
}

// The number of a stash like "stash@{2}".
fn stash_index(name: &str) -> usize {
    name.trim_start_matches("stash@{")
        .trim_end_matches('}')
        .parse()
        .unwrap_or(0)
}

fn commits_intro(heading: &str, commits: &[DialogCommit]) -> Vec<String> {
    let commits = commits.iter().map(|c| c.line.clone()).collect();
    let mut lines = vec![format!("{heading}:")];
    lines.extend(capped(commits, MAX_LISTED).iter().map(|c| format!("  {c}")));
    lines
}

// The commit to make: on which branch, the message beside a bar, and the staged files.
fn commit_preview_lines(preview: &CommitPreview, theme: &ColorTheme) -> Vec<Line<'static>> {
    let dim = Style::default().fg(theme.muted_fg);
    let color = if preview.amend {
        theme.status_warn_fg
    } else {
        theme.accent_fg
    };
    let bar = || Span::raw("┃ ").fg(color);
    let mut lines = vec![
        Line::from(vec![
            Span::raw(format!("{} ", preview.action))
                .fg(theme.fg)
                .bold(),
            Span::raw(preview.branch.clone())
                .fg(theme.highlight_fg)
                .bold(),
        ]),
        Line::raw(""),
        Line::from(vec![
            bar(),
            Span::raw(preview.subject.clone()).fg(theme.fg).bold(),
        ]),
    ];
    if !preview.body.is_empty() {
        lines.push(Line::from(bar()));
    }
    for line in capped(preview.body.clone(), COMMIT_PREVIEW_LINES) {
        lines.push(Line::from(vec![bar(), Span::styled(line, dim)]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        match preview.files.len() {
            0 => "No staged changes".to_string(),
            1 => "1 staged file".to_string(),
            n => format!("{n} staged files"),
        },
        dim,
    ));
    for (status, path) in preview.files.iter().take(COMMIT_PREVIEW_LINES) {
        lines.push(Line::from(vec![
            Span::raw("  "),
            change_span(status, theme),
            Span::raw(format!(" {path}")).fg(theme.fg),
        ]));
    }
    if let Some(more) = preview
        .files
        .len()
        .checked_sub(COMMIT_PREVIEW_LINES)
        .filter(|n| *n > 0)
    {
        lines.push(Line::styled(format!("  … and {more} more"), dim));
    }
    lines
}

// A full commit hash is shown short.
// A full hash shortened, also before a suffix like "^"; other arguments as they are.
fn shorten_hash(arg: &str) -> std::borrow::Cow<'_, str> {
    let is_hash = |hash: &str| hash.len() == 40 && hash.chars().all(|c| c.is_ascii_hexdigit());
    match arg.get(..40) {
        Some(hash) if is_hash(hash) && !arg[40..].starts_with(|c: char| c.is_ascii_hexdigit()) => {
            format!("{}{}", &arg[..7], &arg[40..]).into()
        }
        _ => arg.into(),
    }
}

pub struct GitDialogWidget<'a> {
    dialog: &'a GitDialog,
    theme: &'a ColorTheme,
}

impl<'a> GitDialogWidget<'a> {
    pub fn new(dialog: &'a GitDialog, ctx: &'a AppContext) -> Self {
        GitDialogWidget {
            dialog,
            theme: &ctx.color_theme,
        }
    }
}

const LABEL_WIDTH: usize = 13;
// the popups are at least this wide, so they don't change width much between fields
const MIN_CONTENT_WIDTH: usize = 50;
// long commit subjects and commands are cut at this width
const MAX_CONTENT_WIDTH: usize = 90;

impl Widget for GitDialogWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (dialog, theme) = (self.dialog, self.theme);
        let dim = Style::default().fg(theme.muted_fg);
        // the keys are in the status bar

        // the intro, the fields and the summary, a blank line around each one there is
        let mut lines = vec![Line::raw("")];
        if let DialogKind::Commit { preview, .. } = &dialog.kind {
            lines.extend(commit_preview_lines(preview, theme));
            lines.push(Line::raw(""));
        } else if !dialog.intro.is_empty() {
            for (i, line) in dialog.intro.iter().enumerate() {
                // the heading stands out from the commits under it
                lines.push(if i == 0 {
                    Line::raw(line.clone()).fg(theme.accent_fg).bold()
                } else {
                    Line::raw(line.clone()).fg(theme.fg)
                });
            }
            lines.push(Line::raw(""));
        }
        let mut cursor = None;
        let mut checks_started = false;
        for (i, (_, label, field)) in dialog.fields.iter().enumerate() {
            let selected = i == dialog.selected;
            // the field being changed has a bar beside it, like huh's forms
            let bar = if selected {
                Span::raw("▌ ").fg(theme.highlight_fg)
            } else {
                Span::raw("  ")
            };
            let label_style = if selected {
                Style::default().fg(theme.accent_fg).bold()
            } else {
                Style::default().fg(theme.fg)
            };
            let line = match field {
                Field::Choice { options, index } => {
                    // the options go around
                    let cycles = selected && options.len() > 1;
                    let [left, right] = choice_arrows(cycles, cycles, theme);
                    let more = if options.len() > 1 {
                        format!("  {}/{}", index + 1, options.len())
                    } else {
                        String::new()
                    };
                    Line::from(vec![
                        bar,
                        Span::styled(format!("{label:<LABEL_WIDTH$}  "), label_style),
                        left,
                        Span::raw(options[*index].clone()).fg(theme.fg).bold(),
                        right,
                        Span::styled(more, dim),
                    ])
                }
                Field::Text { input, options } => {
                    let value = input.value();
                    let known = options.iter().any(|o| o == value.trim());
                    // a value not among the options, like a new remote branch
                    let note = if value.trim().is_empty() || known || options.is_empty() {
                        ""
                    } else {
                        "  new"
                    };
                    let cycles = selected && !options.is_empty();
                    let [left, right] = choice_arrows(cycles, cycles, theme);
                    let prefix_width = 2 + LABEL_WIDTH + 2;
                    if selected && dialog.editing {
                        let x = prefix_width + 2 + input.visual_cursor();
                        cursor = Some((x as u16, lines.len() as u16));
                    }
                    Line::from(vec![
                        bar,
                        Span::styled(format!("{label:<LABEL_WIDTH$}  "), label_style),
                        left,
                        Span::raw(value.to_string()).fg(theme.fg).bold(),
                        right,
                        Span::raw(note).fg(theme.highlight_fg),
                    ])
                }
                // no arrows, there is nothing to choose
                Field::Fixed(value) => {
                    let [left, right] = no_choice_arrows();
                    Line::from(vec![
                        bar,
                        Span::styled(format!("{label:<LABEL_WIDTH$}  "), label_style),
                        left,
                        Span::raw(value.clone()).fg(theme.fg).bold(),
                        right,
                    ])
                }
                Field::Check(checked) => {
                    if !checks_started && i > 0 {
                        lines.push(Line::raw(""));
                    }
                    checks_started = true;
                    let mark = if *checked {
                        Span::raw("[•] ").fg(theme.status_success_fg).bold()
                    } else {
                        Span::styled("[ ] ", dim)
                    };
                    Line::from(vec![
                        bar,
                        mark,
                        Span::styled(label.to_string(), label_style),
                    ])
                }
            };
            lines.push(line);
        }
        if !dialog.fields.is_empty() {
            lines.push(Line::raw(""));
        }
        // a line for each
        let warning = dialog.warning();
        for warning in dialog
            .conflicts
            .iter()
            .chain(&dialog.pushed)
            .chain(&warning)
            .flat_map(|w| w.lines())
        {
            lines.push(
                Line::raw(format!("! {warning}"))
                    .fg(theme.status_warn_fg)
                    .bold(),
            );
        }
        // wrapped at spaces, a link or a command can be longer than the popup is wide
        for part in textwrap::wrap(&dialog.summary(), MAX_CONTENT_WIDTH)
            .into_iter()
            .filter(|part| !part.is_empty())
        {
            lines.push(Line::raw(part.into_owned()).patch_style(dim).italic());
        }
        // after the warnings and the summary, when there are any
        if lines.last().is_some_and(|line| line.width() > 0) {
            lines.push(Line::raw(""));
        }

        let title = focused_title(dialog.title, theme);
        let widths = (MIN_CONTENT_WIDTH, MAX_CONTENT_WIDTH);
        let inner = render_popup(title, theme.accent_fg, lines, widths, area, buf, theme);

        if let Some((x, y)) = cursor {
            let (x, y) = (inner.x + x, inner.y + y);
            if x < inner.right() && y < inner.bottom() {
                buf[(x, y)].set_style(Style::default().reversed());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::git::git;
    use ratatui::crossterm::event::KeyModifiers;
    use rustc_hash::FxHashMap;

    use super::*;

    #[test]
    fn test_popups_look_alike() {
        use crate::{
            test_support::ui::{
                assert_fills_bg, assert_popup_frame, popup_rect, test_ctx_with, theme_with_bg,
            },
            widget::style::mark_selected_bar,
        };

        let theme = theme_with_bg();
        let ctx = test_ctx_with(theme.clone());
        // the bar beside the selected row of a list, which the selected field matches
        let mut list_bar = Buffer::empty(Rect::new(0, 0, 1, 1));
        mark_selected_bar(&mut list_bar, 0, 0, true, &theme);

        let preview = CommitPreview {
            action: "Commit to".into(),
            branch: "main".into(),
            amend: false,
            subject: "subject".into(),
            body: vec!["body".into()],
            files: vec![("M".into(), "a.rs".into())],
        };
        let dialogs = [
            GitDialog::abort("rebase"),
            GitDialog::stash("all changes".into(), vec![]),
            GitDialog::merge("main", "feature".into(), "feature", None),
            GitDialog::delete_branches(vec!["a".into(), "b".into()], vec!["a".into()], vec![]),
            GitDialog::commit(preview, GitTask::default()),
            GitDialog::remote(RemoteAction::Push, info()).unwrap(),
            GitDialog::create_tag(StartPoint::Ref(NamedRef::Branch("main".into()))),
            GitDialog::open_link(
                vec![PageLink {
                    remote: "origin".into(),
                    page: "Commit".into(),
                    url: "https://h/c".into(),
                }],
                LinkAction::Open,
            ),
        ];
        for dialog in &dialogs {
            let area = Rect::new(0, 0, 100, 30);
            let mut buf = Buffer::empty(area);
            GitDialogWidget::new(dialog, &ctx).render(area, &mut buf);
            let rect = popup_rect(&buf);
            assert_popup_frame(&buf, rect, theme.accent_fg);
            assert_fills_bg(&buf, rect, &theme, &[]);
            // one blank line between the parts, and around them
            let inner = rect.inner(ratatui::layout::Margin::new(1, 1));
            let blank = |y| (inner.left()..inner.right()).all(|x| buf[(x, y)].symbol() == " ");
            assert!(
                blank(inner.top()) && blank(inner.bottom() - 1),
                "{}",
                dialog.title
            );
            for y in inner.top()..inner.bottom() - 1 {
                assert!(!(blank(y) && blank(y + 1)), "{}", dialog.title);
            }
            for y in rect.top()..rect.bottom() {
                for x in rect.left()..rect.right() {
                    if buf[(x, y)].symbol() == "▌" {
                        assert_eq!(buf[(x, y)].fg, list_bar[(0, 0)].fg, "{}", dialog.title);
                    }
                }
            }
        }
    }

    fn info() -> RemoteInfo {
        let mut remote_branches = FxHashMap::default();
        remote_branches.insert("origin".into(), vec!["main".into(), "dev".into()]);
        remote_branches.insert("fork".into(), vec!["topic".into()]);
        let mut upstreams = FxHashMap::default();
        upstreams.insert("main".into(), ("origin".into(), "main".into()));
        RemoteInfo {
            current_branch: Some("main".into()),
            remotes: vec!["fork".into(), "origin".into()],
            remote_branches,
            branches: vec!["main".into(), "topic".into()],
            upstreams,
            pull_rebase: true,
            rebase_auto_stash: false,
            merge_auto_stash: false,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    // The git command the popup runs.
    fn run(outcome: DialogOutcome) -> GitTask {
        match outcome {
            DialogOutcome::Send(event) => match *event {
                AppEvent::StartTask(task) => task,
                event => panic!("no git command but {event:?}"),
            },
            outcome => panic!("nothing runs but {outcome:?}"),
        }
    }

    // The link the popup opens or copies.
    fn opened(outcome: DialogOutcome) -> (String, LinkAction) {
        match outcome {
            DialogOutcome::Send(event) => match *event {
                AppEvent::OpenUrl { url, action } => (url, action),
                event => panic!("no link but {event:?}"),
            },
            outcome => panic!("nothing opens but {outcome:?}"),
        }
    }

    fn press(dialog: &mut GitDialog, event: UserEvent) -> DialogOutcome {
        dialog.handle_key(Some(event), key(KeyCode::Null))
    }

    fn args(dialog: &GitDialog) -> String {
        let task = dialog.command().unwrap().unwrap();
        task.commands[0].join(" ")
    }

    fn commit(n: char) -> DialogCommit {
        DialogCommit {
            hash: n.to_string().repeat(40).as_str().into(),
            line: format!("{n} subject"),
        }
    }

    #[test]
    fn test_reset_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        git(path, &["init", "-q", "-b", "main"]);
        git(path, &["config", "user.name", "t"]);
        git(path, &["config", "user.email", "t@t"]);
        git(path, &["config", "commit.gpgsign", "false"]);
        let write = |name: &str, text: &str| std::fs::write(path.join(name), text).unwrap();
        write("a", "1\n");
        git(path, &["add", "."]);
        git(path, &["commit", "-q", "-m", "root"]);
        write("a", "2\n");
        write("n", "x\n");
        git(path, &["add", "."]);
        git(path, &["commit", "-q", "-m", "c1"]);
        git(path, &["mv", "n", "m"]);
        git(path, &["commit", "-q", "-m", "c2"]);
        git(path, &["rm", "-q", "a"]);
        git(path, &["commit", "-q", "-m", "c3"]);
        write("m", "y\n");
        git(path, &["commit", "-q", "-am", "c4"]);

        // the files there are, with their content, after resetting `change` of the commit `back`
        // from HEAD, at it or before it
        let reset = |back: usize, change: FileChange, before: bool| -> String {
            let hash = git(path, &["rev-parse", &format!("HEAD~{back}")]);
            let commit = DialogCommit {
                hash: hash.trim().into(),
                line: String::new(),
            };
            let mut dialog = GitDialog::reset_file(commit, change);
            if before {
                press(&mut dialog, UserEvent::NavigateRight);
            }
            crate::git::run_task(path, &dialog.command().unwrap().unwrap()).unwrap();
            let status = git(path, &["status", "--porcelain"]);
            git(path, &["reset", "-q", "--hard"]);
            status
        };
        let path_of = |p: &str| p.to_string();
        use FileChange::*;
        assert_eq!(reset(3, Modify { path: path_of("a") }, true), "A  a\n");
        // added by c1, before it: already gone
        assert_eq!(reset(3, Add { path: path_of("n") }, true), "");
        let renamed = || Move {
            from: path_of("n"),
            to: path_of("m"),
        };
        assert_eq!(reset(2, renamed(), false), "M  m\n");
        assert_eq!(reset(2, renamed(), true), "D  m\nA  n\n");
        // deleted by c3, at it: already gone; before it, back
        assert_eq!(reset(1, Delete { path: path_of("a") }, false), "");
        assert_eq!(reset(1, Delete { path: path_of("a") }, true), "A  a\n");
        // the root has no parent: what it added goes
        assert_eq!(reset(4, Add { path: path_of("m") }, true), "D  m\n");
    }

    #[test]
    fn test_fixed_values_have_no_arrows() {
        use crate::test_support::ui::{find_row, row_text, test_ctx};
        let ctx = test_ctx();
        let draw = |dialog: &GitDialog| {
            let area = Rect::new(0, 0, 100, 20);
            let mut buf = Buffer::empty(area);
            GitDialogWidget::new(dialog, &ctx).render(area, &mut buf);
            buf
        };
        // from a commit of the graph, or a tag where the cursor is: nothing else to choose
        let commit = || "a".repeat(40).as_str().into();
        for (dialog, label) in [
            (GitDialog::create_branch_at(commit()), "From"),
            (GitDialog::create_tag(StartPoint::Commit(commit())), "At"),
        ] {
            let buf = draw(&dialog);
            let row = row_text(&buf, find_row(&buf, label));
            assert!(!row.contains('‹') && !row.contains('›'), "{row}");
            assert!(row.contains("aaaaaaa"), "{row}");
        }
        // from a branch of the sidebar the start can be chosen, even when it's the only branch
        let main = || NamedRef::Branch("main".into());
        let buf = draw(&GitDialog::create_branch_from(main(), vec!["dev".into()]));
        assert!(row_text(&buf, find_row(&buf, "From")).contains("‹ main ›"));
        let buf = draw(&GitDialog::create_branch_from(main(), vec!["main".into()]));
        assert!(row_text(&buf, find_row(&buf, "From")).contains("‹ main ›"));
    }

    #[test]
    fn test_enter_goes_to_empty_required_fields() {
        let mut dialog = GitDialog::create_tag(StartPoint::Ref(NamedRef::Branch("main".into())));
        let enter = key(KeyCode::Enter);
        // the name, still empty, stays typed into
        dialog.handle_key(Some(UserEvent::Confirm), enter);
        assert_eq!((dialog.selected, dialog.editing), (0, true));
        dialog.handle_key(None, key(KeyCode::Char('v')));
        // the message is optional, so typing is done
        dialog.handle_key(Some(UserEvent::Confirm), enter);
        assert_eq!((dialog.selected, dialog.editing), (0, false));
        assert_eq!(args(&dialog), "tag v refs/heads/main");

        // enter on the popup goes to the empty URL instead of failing
        let mut dialog = GitDialog::add_remote();
        dialog.handle_key(None, key(KeyCode::Char('o')));
        dialog.handle_key(Some(UserEvent::Confirm), key(KeyCode::Esc));
        assert!(matches!(
            press(&mut dialog, UserEvent::Confirm),
            DialogOutcome::None
        ));
        assert_eq!((dialog.selected, dialog.editing), (1, true));
    }

    #[test]
    fn test_no_pull_while_detached() {
        let detached = RemoteInfo {
            current_branch: None,
            ..info()
        };
        assert!(GitDialog::remote(RemoteAction::Pull, detached.clone()).is_err());
        assert!(GitDialog::remote(RemoteAction::Fetch, detached).is_ok());
        // nothing fetched, nothing to choose to pull
        let unfetched = RemoteInfo {
            remote_branches: FxHashMap::default(),
            ..info()
        };
        let refused = GitDialog::remote(RemoteAction::Pull, unfetched).unwrap_err();
        assert_eq!(refused, "No remote has a branch to pull, fetch first");
    }

    #[test]
    fn test_delete_tags_names_the_remote() {
        let dialog = GitDialog::delete_tags(vec!["v1".into()], Some("fork".into()));
        assert_eq!(dialog.fields[0].1, "Delete them from fork too");
        let dialog = GitDialog::delete_tags(vec!["v1".into()], None);
        assert!(dialog.fields.is_empty());

        // on the remote first: failing there, offline say, nothing is deleted yet to retry it
        let commands = |dialog: &GitDialog| -> Vec<String> {
            let task = dialog.command().unwrap().unwrap();
            task.commands.iter().map(|c| c.join(" ")).collect()
        };
        let mut dialog = GitDialog::delete_tags(vec!["v1".into()], Some("fork".into()));
        press(&mut dialog, UserEvent::DialogToggle);
        assert_eq!(
            commands(&dialog),
            ["push fork --delete refs/tags/v1", "tag -d v1"]
        );
        let upstreams = vec![("origin".into(), "a".into())];
        let mut dialog = GitDialog::delete_branches(vec!["a".into()], vec![], upstreams);
        press(&mut dialog, UserEvent::DialogToggle);
        assert_eq!(
            commands(&dialog),
            ["push origin --delete refs/heads/a", "branch -D a"]
        );
    }

    #[test]
    fn test_bad_names_and_upstreams_are_refused() {
        // `git branch -D feature` would delete feature
        let mut dialog = GitDialog::create_branch_from(NamedRef::Branch("feature".into()), vec![]);
        dialog.handle_key(None, key(KeyCode::Char('-')));
        dialog.handle_key(None, key(KeyCode::Char('D')));
        assert!(dialog.command().unwrap().is_err());
        // `git branch -m topic -f` would overwrite topic with the current branch
        let mut dialog = GitDialog::edit_branch("topic", None, vec![]);
        dialog.handle_key(None, key(KeyCode::End));
        for _ in 0..5 {
            dialog.handle_key(None, key(KeyCode::Backspace));
        }
        dialog.handle_key(None, key(KeyCode::Char('-')));
        dialog.handle_key(None, key(KeyCode::Char('f')));
        assert!(dialog.command().unwrap().is_err());
        // the upstream only chosen among the remote branches, or none
        let remote_branches = vec!["origin/main".to_string()];
        let mut dialog = GitDialog::edit_branch("topic", None, remote_branches);
        dialog.handle_key(None, key(KeyCode::Esc));
        press(&mut dialog, UserEvent::NavigateDown);
        assert!(!dialog.can_edit());
        press(&mut dialog, UserEvent::NavigateRight);
        assert_eq!(args(&dialog), "branch --set-upstream-to=origin/main topic");
        // renamed, an upstream gone from the remote stays, not unset
        let remote_branches = vec!["origin/main".to_string()];
        let gone = Some("origin/gone".to_string());
        let mut dialog = GitDialog::edit_branch("topic", gone, remote_branches);
        dialog.handle_key(None, key(KeyCode::Char('2')));
        let task = dialog.command().unwrap().unwrap();
        assert_eq!(task.commands, [["branch", "-m", "topic", "topic2"]]);
        // from a commit, or a tag at it, where the cursor is
        let dialog = GitDialog::create_branch_at("a".repeat(40).as_str().into());
        assert!(matches!(&dialog.fields[1].2, Field::Fixed(_)));
        let dialog = GitDialog::create_tag(StartPoint::Commit("a".repeat(40).as_str().into()));
        assert!(matches!(&dialog.fields[2].2, Field::Fixed(_)));
        let mut dialog = GitDialog::create_tag(StartPoint::Ref(NamedRef::Tag("v1".into())));
        dialog.handle_key(None, key(KeyCode::Char('-')));
        dialog.handle_key(None, key(KeyCode::Char('d')));
        assert!(dialog.command().unwrap().is_err());
        dialog.handle_key(None, key(KeyCode::Home));
        dialog.handle_key(None, key(KeyCode::Delete));
        assert_eq!(args(&dialog), "tag d refs/tags/v1");
    }

    #[test]
    fn test_fetch_defaults_to_upstream_remote() {
        let mut dialog = GitDialog::remote(RemoteAction::Fetch, info()).unwrap();
        assert_eq!(args(&dialog), "fetch origin");
        assert!(dialog.command().unwrap().unwrap().remote);
        press(&mut dialog, UserEvent::NavigateRight);
        assert_eq!(args(&dialog), "fetch fork");
        assert!(matches!(
            press(&mut dialog, UserEvent::Cancel),
            DialogOutcome::Close
        ));
    }

    #[test]
    fn test_no_remotes() {
        let info = RemoteInfo {
            remotes: vec![],
            ..info()
        };
        assert!(GitDialog::remote(RemoteAction::Pull, info).is_err());
    }

    #[test]
    fn test_pull_auto_stash_follows_config() {
        let info = RemoteInfo {
            rebase_auto_stash: true,
            ..info()
        };
        let mut dialog = GitDialog::remote(RemoteAction::Pull, info).unwrap();
        assert_eq!(args(&dialog), "pull --rebase --autostash origin main");

        // merging follows merge.autoStash, which is not set
        press(&mut dialog, UserEvent::NavigateDown);
        press(&mut dialog, UserEvent::NavigateDown);
        press(&mut dialog, UserEvent::DialogToggle);
        assert_eq!(args(&dialog), "pull --no-rebase --no-autostash origin main");

        // once changed, the choice stays
        press(&mut dialog, UserEvent::NavigateDown);
        press(&mut dialog, UserEvent::DialogToggle);
        press(&mut dialog, UserEvent::NavigateUp);
        press(&mut dialog, UserEvent::DialogToggle);
        assert_eq!(args(&dialog), "pull --rebase --autostash origin main");
        press(&mut dialog, UserEvent::DialogToggle);
        assert_eq!(args(&dialog), "pull --no-rebase --autostash origin main");
    }

    #[test]
    fn test_merge_type_follows_config() {
        let dialog = GitDialog::merge("main", "feature".into(), "feature", Some("False"));
        assert_eq!(args(&dialog), "merge --no-ff --no-edit feature");
        let dialog = GitDialog::merge("main", "feature".into(), "feature", Some("only"));
        assert_eq!(args(&dialog), "merge --ff-only feature");
        let dialog = GitDialog::rebase("main", "feature".into(), "feature", false);
        assert_eq!(args(&dialog), "rebase --no-autostash feature");
    }

    #[test]
    fn test_open_link() {
        let link = |remote: &str, page: &str, url: &str| PageLink {
            remote: remote.into(),
            page: page.into(),
            url: url.into(),
        };
        let links = vec![
            link("origin", "Branch", "https://h/o/r/tree/b"),
            link("origin", "Pull request", "https://h/o/r/compare/b"),
        ];
        // the branch first, the pull request chosen with the toggle; o again opens it
        let mut dialog = GitDialog::open_link(links, LinkAction::Open);
        assert_eq!(dialog.fields.len(), 1);
        assert_eq!(dialog.summary(), "https://h/o/r/tree/b");
        press(&mut dialog, UserEvent::DialogToggle);
        assert_eq!(
            opened(press(&mut dialog, UserEvent::OpenInBrowser)),
            ("https://h/o/r/compare/b".into(), LinkAction::Open)
        );
        // the remote chosen first, then the page on it
        let links = vec![
            link("origin", "Branch", "https://h/o/r/tree/b"),
            link("origin", "Pull request", "https://h/o/r/compare/b"),
            link("up", "Branch", "https://h/u/r/tree/b"),
            link("up", "Pull request", "https://h/u/r/compare/b"),
        ];
        let mut dialog = GitDialog::open_link(links, LinkAction::Copy);
        press(&mut dialog, UserEvent::DialogToggle);
        press(&mut dialog, UserEvent::NavigateDown);
        press(&mut dialog, UserEvent::DialogToggle);
        assert_eq!(
            opened(press(&mut dialog, UserEvent::Confirm)),
            ("https://h/u/r/compare/b".into(), LinkAction::Copy)
        );
        // one link, to confirm
        let mut dialog = GitDialog::open_link(
            vec![link("origin", "Commit", "https://h/x")],
            LinkAction::Open,
        );
        assert!(dialog.fields.is_empty());
        assert_eq!(
            opened(press(&mut dialog, UserEvent::Confirm)),
            ("https://h/x".into(), LinkAction::Open)
        );
    }

    #[test]
    fn test_trigger_key_runs() {
        // pressing the key opening the popup again runs it
        let mut dialog = GitDialog::remote(RemoteAction::Pull, info()).unwrap();
        let task = run(press(&mut dialog, UserEvent::Pull));
        assert_eq!(task.running, "Pulling origin/main");
        // the sidebar shows it beside the checked-out branch
        let status = task.branch_status.unwrap();
        assert_eq!((status.branch.as_str(), status.short), ("main", "↓"));
        let mut dialog = GitDialog::remote(RemoteAction::Fetch, info()).unwrap();
        assert!(matches!(
            press(&mut dialog, UserEvent::Push),
            DialogOutcome::None
        ));
        // like enter, it goes to a required field left empty instead of failing
        let mut dialog = GitDialog::remote(RemoteAction::Push, info()).unwrap();
        dialog.selected = 2;
        dialog.fields[2].2 = Field::text("", vec![]);
        assert!(matches!(
            press(&mut dialog, UserEvent::Push),
            DialogOutcome::None
        ));
        assert!(dialog.editing);

        // space toggles a field that can be: the next choice, the next option of a text field,
        // a checkbox; else it types, like e
        let mut dialog = GitDialog::remote(RemoteAction::Pull, info()).unwrap();
        press(&mut dialog, UserEvent::DialogActivate);
        assert_eq!(args(&dialog), "pull --rebase --no-autostash fork topic");
        press(&mut dialog, UserEvent::NavigateDown);
        press(&mut dialog, UserEvent::NavigateDown);
        press(&mut dialog, UserEvent::DialogActivate);
        assert_eq!(args(&dialog), "pull --no-rebase --no-autostash fork topic");
        press(&mut dialog, UserEvent::NavigateUp);
        press(&mut dialog, UserEvent::NavigateUp);
        press(&mut dialog, UserEvent::NavigateLeft);
        press(&mut dialog, UserEvent::NavigateDown);
        // origin has two branches to go through
        press(&mut dialog, UserEvent::DialogActivate);
        assert_eq!(args(&dialog), "pull --no-rebase --no-autostash origin dev");
        assert!(!dialog.editing);
        press(&mut dialog, UserEvent::DialogToggle);
        assert_eq!(args(&dialog), "pull --no-rebase --no-autostash origin main");
        // a branch to pull is chosen, not typed
        press(&mut dialog, UserEvent::DialogEdit);
        assert!(!dialog.editing);
        // while typing, the key opening the popup is typed
        let mut dialog = GitDialog::remote(RemoteAction::Push, info()).unwrap();
        press(&mut dialog, UserEvent::NavigateDown);
        press(&mut dialog, UserEvent::NavigateDown);
        press(&mut dialog, UserEvent::DialogEdit);
        assert!(dialog.editing);
        let typed = KeyEvent::new(KeyCode::Char('P'), KeyModifiers::NONE);
        dialog.handle_key(Some(UserEvent::Push), typed);
        assert!(dialog.editing);
        assert!(args(&dialog).ends_with("mainP"), "{}", args(&dialog));
    }

    #[test]
    fn test_pull_options() {
        let mut dialog = GitDialog::remote(RemoteAction::Pull, info()).unwrap();
        // pull.rebase is set
        assert_eq!(args(&dialog), "pull --rebase --no-autostash origin main");

        // the branch follows the remote
        press(&mut dialog, UserEvent::NavigateLeft);
        assert_eq!(args(&dialog), "pull --rebase --no-autostash fork topic");
        press(&mut dialog, UserEvent::NavigateRight);
        press(&mut dialog, UserEvent::NavigateDown);
        press(&mut dialog, UserEvent::NavigateRight);
        assert_eq!(args(&dialog), "pull --rebase --no-autostash origin dev");

        press(&mut dialog, UserEvent::NavigateDown);
        press(&mut dialog, UserEvent::DialogToggle);
        press(&mut dialog, UserEvent::NavigateDown);
        press(&mut dialog, UserEvent::DialogToggle);
        assert_eq!(args(&dialog), "pull --no-rebase --autostash origin dev");

        let outcome = press(&mut dialog, UserEvent::Confirm);
        assert_eq!(run(outcome).running, "Pulling origin/dev");
    }

    #[test]
    fn test_push_options() {
        let mut dialog = GitDialog::remote(RemoteAction::Push, info()).unwrap();
        assert_eq!(args(&dialog), "push origin refs/heads/main:refs/heads/main");

        // a branch without an upstream is pushed under its own name, and tracks it
        press(&mut dialog, UserEvent::NavigateRight);
        assert_eq!(
            args(&dialog),
            "push --set-upstream origin refs/heads/topic:refs/heads/topic"
        );

        // type another name for the remote branch
        press(&mut dialog, UserEvent::NavigateDown);
        press(&mut dialog, UserEvent::NavigateDown);
        press(&mut dialog, UserEvent::DialogEdit);
        assert!(dialog.editing);
        dialog.handle_key(None, key(KeyCode::Char('2')));
        dialog.handle_key(None, key(KeyCode::Enter));
        assert!(!dialog.editing);

        press(&mut dialog, UserEvent::NavigateDown);
        press(&mut dialog, UserEvent::DialogToggle);
        press(&mut dialog, UserEvent::NavigateDown);
        press(&mut dialog, UserEvent::DialogToggle);
        assert_eq!(
            args(&dialog),
            "push --force-with-lease --tags --set-upstream origin refs/heads/topic:refs/heads/topic2"
        );
        press(&mut dialog, UserEvent::NavigateDown);
        press(&mut dialog, UserEvent::DialogToggle);
        assert!(args(&dialog).starts_with("push --force-with-lease --tags --no-verify "));
    }

    #[test]
    fn test_graph_commands() {
        let mut dialog = GitDialog::rebase("main", "feature".into(), "feature", true);
        assert_eq!(args(&dialog), "rebase --autostash feature");
        press(&mut dialog, UserEvent::DialogToggle);
        assert_eq!(args(&dialog), "rebase --no-autostash feature");

        let mut dialog = GitDialog::merge("main", "feature".into(), "feature", None);
        assert_eq!(args(&dialog), "merge --ff --no-edit feature");
        for _ in 0..4 {
            press(&mut dialog, UserEvent::NavigateRight);
        }
        assert_eq!(args(&dialog), "merge --no-ff --no-commit feature");

        let targets = vec![
            CheckoutTarget {
                label: "feature".into(),
                args: vec!["feature".into()],
            },
            CheckoutTarget {
                label: "aaaaaaa (detached)".into(),
                args: vec!["--detach".into(), "a".repeat(40)],
            },
        ];
        let mut dialog = GitDialog::checkout(targets);
        assert_eq!(args(&dialog), "checkout feature");
        press(&mut dialog, UserEvent::NavigateDown);
        press(&mut dialog, UserEvent::NavigateRight);
        assert!(dialog.command().unwrap().unwrap().stash);
        press(&mut dialog, UserEvent::NavigateRight);
        assert_eq!(args(&dialog), "checkout --force feature");
        assert!(dialog.warning().is_some());

        let mut dialog = GitDialog::cherry_pick("main", vec![commit('a'), commit('b')]);
        press(&mut dialog, UserEvent::DialogToggle);
        press(&mut dialog, UserEvent::NavigateDown);
        press(&mut dialog, UserEvent::DialogToggle);
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        assert_eq!(args(&dialog), format!("cherry-pick --no-commit -x {a} {b}"));
        assert_eq!(
            dialog.summary(),
            "git cherry-pick --no-commit -x aaaaaaa bbbbbbb"
        );

        let dialog = GitDialog::revert("main", vec![commit('b')]);
        assert_eq!(args(&dialog), format!("revert --no-edit {b}"));

        let mut dialog = GitDialog::reset("main", commit('a'));
        assert_eq!(args(&dialog), format!("reset --mixed {a}"));
        press(&mut dialog, UserEvent::NavigateLeft);
        assert_eq!(args(&dialog), format!("reset --hard {a}"));

        let dialog = GitDialog::squash("main", vec![commit('b')], None, true);
        let task = dialog.command().unwrap().unwrap();
        assert_eq!(
            args(&dialog),
            "rebase -i --no-autosquash --autostash --root"
        );
        assert!(task.env.iter().any(|(k, v)| k == "GIT_SEQUENCE_EDITOR"
            && v.contains(&format!("sed -i.bak -E -e 's/^(pick|p) {b} /squash {b} /'"))));
    }

    // Runs git in the repository, returning what it printed.
    fn test_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        git(path, &["init", "-q", "-b", "main"]);
        git(path, &["config", "user.name", "t"]);
        git(path, &["config", "user.email", "t@t"]);
        git(path, &["config", "commit.gpgsign", "false"]);
        for n in 1..=4 {
            std::fs::write(path.join("f"), format!("{n}\n")).unwrap();
            git(path, &["add", "f"]);
            git(path, &["commit", "-q", "-m", &format!("c{n}")]);
        }
        dir
    }

    #[test]
    fn test_squash_runs() {
        let dir = test_repo();
        let path = dir.path();
        let rev = |r: &str| git(path, &["rev-parse", r]).trim().to_string();
        let commits = vec![DialogCommit {
            hash: rev("HEAD~1").as_str().into(),
            line: String::new(),
        }];
        // c3 into c2, rebasing after c1
        let dialog = GitDialog::squash("main", commits, Some(rev("HEAD~3")), true);
        crate::git::run_task(path, &dialog.command().unwrap().unwrap()).unwrap();

        let log = git(path, &["log", "--format=%s"]);
        assert_eq!(log, "c4\nc2\nc1\n");
        let message = git(path, &["log", "-1", "--format=%B", "HEAD~1"]);
        assert!(
            message.contains("c2") && message.contains("c3"),
            "{message}"
        );
    }

    #[test]
    fn test_checkout_stashes_and_reapplies() {
        let dir = test_repo();
        let path = dir.path();
        git(path, &["branch", "old", "HEAD~1"]);
        std::fs::write(path.join("untracked"), "x").unwrap();

        let targets = vec![CheckoutTarget {
            label: "old".into(),
            args: vec!["old".into()],
        }];
        let mut dialog = GitDialog::checkout(targets);
        dialog.handle_key(Some(UserEvent::NavigateDown), key(KeyCode::Null));
        dialog.handle_key(Some(UserEvent::NavigateRight), key(KeyCode::Null));
        crate::git::run_task(path, &dialog.command().unwrap().unwrap()).unwrap();

        assert_eq!(git(path, &["branch", "--show-current"]).trim(), "old");
        assert!(path.join("untracked").exists());
        assert_eq!(git(path, &["stash", "list"]), "");
    }
}
