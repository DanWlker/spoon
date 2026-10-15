use ratatui::{
    buffer::Buffer,
    crossterm::event::{Event, KeyCode, KeyEvent},
    layout::Rect,
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Widget},
};
use tui_input::{backend::crossterm::EventHandler, Input};

use crate::{
    app::AppContext,
    color::ColorTheme,
    event::UserEvent,
    git::{GitTask, RemoteInfo},
    keybind::KeyBind,
    widget::style::{badge, key_hints},
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

// A commit listed in a popup: its full hash, and the line describing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogCommit {
    pub hash: String,
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
}

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
}

#[derive(Debug)]
enum Field {
    // one of the options, changed with left/right
    Choice { options: Vec<String>, index: usize },
    // any text, or one of the options with left/right
    Text { input: Input, options: Vec<String> },
    Check(bool),
}

#[derive(Debug, PartialEq, Eq)]
pub enum DialogOutcome {
    None,
    Close,
    Run(GitTask),
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
    fields: Vec<(FieldId, &'static str, Field)>,
    selected: usize,
    // typing into the selected text field
    editing: bool,
    // the stash checkbox was changed, so it no longer follows the config of how to pull
    auto_stash_changed: bool,
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
        }
    }

    pub fn remote(action: RemoteAction, info: RemoteInfo) -> Result<Self, String> {
        if info.remotes.is_empty() {
            return Err("No remotes configured".into());
        }
        let (title, verb) = match action {
            RemoteAction::Fetch => (" Fetch ", "fetch"),
            RemoteAction::Pull => (" Pull ", "pull"),
            RemoteAction::Push => (" Push ", "push"),
        };
        let mut dialog = Self::with(
            title,
            verb,
            vec![],
            DialogKind::Remote(action, Box::new(info.clone())),
        );
        let current = info.current_branch.clone();
        match action {
            RemoteAction::Fetch => {
                let remote = dialog.default_remote(current.as_deref());
                dialog.push_choice(FieldId::Remote, "Remote", info.remotes.clone(), &remote);
            }
            RemoteAction::Pull => {
                let remote = dialog.default_remote(current.as_deref());
                dialog.push_choice(FieldId::Remote, "Remote", info.remotes.clone(), &remote);
                dialog
                    .fields
                    .push((FieldId::Branch, "Branch", Field::text("", vec![])));
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
                dialog
                    .fields
                    .push((FieldId::RemoteBranch, "As branch", Field::text("", vec![])));
                dialog.push_check(FieldId::Tags, "Push all tags", false);
                dialog.push_check(FieldId::Force, "Force push (with lease)", false);
                dialog.update_push_remote();
            }
        }
        Ok(dialog)
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
            Some("false" | "no" | "off" | "0") => MERGE_TYPES[1],
            Some("only") => MERGE_TYPES[2],
            _ => MERGE_TYPES[0],
        };
        dialog.push_choice(FieldId::MergeType, "Type", types, default);
        dialog
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
        dialog.push_check(
            FieldId::AutoStash,
            "Stash and reapply local changes",
            auto_stash,
        );
        dialog
    }

    // `commits` are the newest first, the order they are reverted in.
    pub fn revert(branch: &str, commits: Vec<DialogCommit>) -> Self {
        let intro = commits_intro(&format!("Revert on {branch}"), &commits);
        let kind = DialogKind::Revert { commits };
        let mut dialog = Self::with(" Revert ", "revert", intro, kind);
        dialog.push_check(FieldId::Commit, "Commit (else edit in local changes)", true);
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

    fn info(&self) -> &RemoteInfo {
        match &self.kind {
            DialogKind::Remote(_, info) => info,
            _ => unreachable!("only the remote popups have remote info"),
        }
    }

    fn push_choice(&mut self, id: FieldId, label: &'static str, options: Vec<String>, value: &str) {
        let index = options.iter().position(|o| o == value).unwrap_or(0);
        self.fields
            .push((id, label, Field::Choice { options, index }));
    }

    fn push_check(&mut self, id: FieldId, label: &'static str, checked: bool) {
        self.fields.push((id, label, Field::Check(checked)));
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

    fn value(&self, id: FieldId) -> String {
        match self.field(id) {
            Some(Field::Choice { options, index }) => options[*index].clone(),
            Some(Field::Text { input, .. }) => input.value().trim().to_string(),
            _ => String::new(),
        }
    }

    fn choice(&self, id: FieldId) -> usize {
        match self.field(id) {
            Some(Field::Choice { index, .. }) => *index,
            _ => 0,
        }
    }

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

    // The branch to pull from the chosen remote: the one typed before when the remote has it too,
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
            *field = Field::text(&value, options);
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
                KeyCode::Enter | KeyCode::Esc => self.editing = false,
                _ => {
                    if let Field::Text { input, .. } = &mut self.fields[self.selected].2 {
                        input.handle_event(&Event::Key(key));
                    }
                }
            }
            return DialogOutcome::None;
        }
        let len = self.fields.len().max(1);
        match event {
            Some(UserEvent::Cancel | UserEvent::Close | UserEvent::Quit) => DialogOutcome::Close,
            Some(UserEvent::NavigateDown | UserEvent::SelectDown) => {
                self.selected = (self.selected + 1) % len;
                DialogOutcome::None
            }
            Some(UserEvent::NavigateUp | UserEvent::SelectUp) => {
                self.selected = (self.selected + len - 1) % len;
                DialogOutcome::None
            }
            Some(UserEvent::NavigateRight) => {
                self.change(true);
                DialogOutcome::None
            }
            Some(UserEvent::NavigateLeft) => {
                self.change(false);
                DialogOutcome::None
            }
            Some(UserEvent::DialogToggle) => {
                if let Some((_, _, Field::Check(_))) = self.fields.get(self.selected) {
                    self.change(true);
                }
                DialogOutcome::None
            }
            Some(UserEvent::DialogEdit) => {
                self.editing = matches!(
                    self.fields.get(self.selected),
                    Some((_, _, Field::Text { .. }))
                );
                DialogOutcome::None
            }
            Some(UserEvent::Confirm) => match self.command() {
                Ok(task) => DialogOutcome::Run(task),
                Err(err) => DialogOutcome::Error(err),
            },
            _ => DialogOutcome::None,
        }
    }

    // Changes the selected field to its next or previous option, or toggles it.
    fn change(&mut self, forward: bool) {
        let previous_branch = self.value(FieldId::Branch);
        let Some((id, _, field)) = self.fields.get_mut(self.selected) else {
            return;
        };
        let id = *id;
        match field {
            Field::Choice { options, index } => {
                let len = options.len();
                *index = if forward {
                    (*index + 1) % len
                } else {
                    (*index + len - 1) % len
                };
            }
            Field::Text { input, options } => {
                if options.is_empty() {
                    return;
                }
                let len = options.len();
                let next = match options.iter().position(|o| o == input.value().trim()) {
                    Some(i) if forward => (i + 1) % len,
                    Some(i) => (i + len - 1) % len,
                    None if forward => 0,
                    None => len - 1,
                };
                *input = Input::new(options[next].clone());
            }
            Field::Check(checked) => *checked = !*checked,
        }
        // the fields that follow the one changed
        let action = match &self.kind {
            DialogKind::Remote(action, _) => Some(*action),
            _ => None,
        };
        match (action, id) {
            (Some(RemoteAction::Pull), FieldId::Remote) => {
                self.update_pull_branch(Some(previous_branch));
            }
            (Some(RemoteAction::Push), FieldId::LocalBranch) => self.update_push_remote(),
            (Some(RemoteAction::Push), FieldId::Remote) => self.update_push_remote_branch(),
            (Some(RemoteAction::Pull), FieldId::Rebase) => self.update_pull_auto_stash(),
            (_, FieldId::AutoStash) => self.auto_stash_changed = true,
            _ => {}
        }
    }

    pub fn command(&self) -> Result<GitTask, String> {
        match &self.kind {
            DialogKind::Remote(action, _) => self.remote_command(*action),
            DialogKind::Rebase { onto, description } => {
                let stash = if self.checked(FieldId::AutoStash) {
                    "--autostash"
                } else {
                    "--no-autostash"
                };
                Ok(GitTask::new(
                    &["rebase", stash, onto],
                    description.replace("Rebased", "Rebasing"),
                    description.clone(),
                ))
            }
            DialogKind::Merge {
                source,
                description,
            } => {
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
                    _ => description.clone(),
                };
                Ok(GitTask {
                    show_local_changes: self.choice(FieldId::MergeType) >= 3,
                    ..GitTask::new(&args, description.replace("Merged", "Merging"), done)
                })
            }
            DialogKind::Checkout { targets } => {
                let target = &targets[self.choice(FieldId::Target)];
                let local_changes = self.choice(FieldId::LocalChanges);
                let mut args = vec!["checkout".to_string()];
                if local_changes == 2 {
                    args.push("--force".into());
                }
                args.extend(target.args.iter().cloned());
                Ok(GitTask {
                    args,
                    stash: local_changes == 1,
                    running: format!("Checking out {}", target.label),
                    done: format!("Checked out {}", target.label),
                    ..Default::default()
                })
            }
            DialogKind::CherryPick { commits } => {
                let mut args = vec!["cherry-pick".to_string()];
                if !self.checked(FieldId::Commit) {
                    args.push("--no-commit".into());
                }
                if self.checked(FieldId::AppendOrigin) {
                    args.push("-x".into());
                }
                args.extend(commits.iter().map(|c| c.hash.clone()));
                let n = count_commits(commits.len());
                Ok(GitTask {
                    args,
                    running: format!("Cherry-picking {n}"),
                    done: format!("Cherry-picked {n}"),
                    uses_picks: true,
                    show_local_changes: !self.checked(FieldId::Commit),
                    ..Default::default()
                })
            }
            DialogKind::Squash { commits, base } => {
                let stash = if self.checked(FieldId::AutoStash) {
                    "--autostash"
                } else {
                    "--no-autostash"
                };
                let mut args = vec!["rebase".to_string(), "-i".into(), stash.into()];
                args.push(base.clone().unwrap_or_else(|| "--root".into()));
                // the todo list names the commits with full hashes, the picked ones become squashes
                let editor = std::iter::once("sed -i.bak -E".to_string())
                    .chain(
                        commits
                            .iter()
                            .map(|c| format!("-e 's/^(pick|p) {0} /squash {0} /'", c.hash)),
                    )
                    .collect::<Vec<_>>()
                    .join(" ");
                let env = [
                    ("GIT_SEQUENCE_EDITOR", editor.as_str()),
                    ("GIT_CONFIG_COUNT", "1"),
                    ("GIT_CONFIG_KEY_0", "core.abbrev"),
                    ("GIT_CONFIG_VALUE_0", "40"),
                ];
                let n = count_commits(commits.len());
                Ok(GitTask {
                    args,
                    env: env
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect(),
                    running: format!("Squashing {n}"),
                    done: format!("Squashed {n} into their parents"),
                    uses_picks: true,
                    ..Default::default()
                })
            }
            DialogKind::Revert { commits } => {
                let mut args = vec!["revert".to_string(), "--no-edit".into()];
                if !self.checked(FieldId::Commit) {
                    args.push("--no-commit".into());
                }
                args.extend(commits.iter().map(|c| c.hash.clone()));
                let n = count_commits(commits.len());
                Ok(GitTask {
                    args,
                    running: format!("Reverting {n}"),
                    done: format!("Reverted {n}"),
                    uses_picks: true,
                    show_local_changes: !self.checked(FieldId::Commit),
                    ..Default::default()
                })
            }
            DialogKind::Reset { commit, branch } => {
                let mode = ["--mixed", "--soft", "--hard"][self.choice(FieldId::ResetMode)];
                let short = &commit.hash[..7.min(commit.hash.len())];
                Ok(GitTask::new(
                    &["reset", mode, &commit.hash],
                    format!("Resetting {branch} to {short}"),
                    format!("Reset {branch} to {short}"),
                ))
            }
        }
    }

    fn remote_command(&self, action: RemoteAction) -> Result<GitTask, String> {
        let remote = self.value(FieldId::Remote);
        let (args, running, done) = match action {
            RemoteAction::Fetch => (
                vec!["fetch".to_string(), remote.clone()],
                format!("Fetching {remote}"),
                format!("Fetched {remote}"),
            ),
            RemoteAction::Pull => {
                let branch = self.value(FieldId::Branch);
                if branch.is_empty() {
                    return Err("Choose a branch to pull".into());
                }
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
                (
                    args,
                    format!("{force} {branch} to {remote}/{remote_branch}"),
                    format!("Pushed {branch} to {remote}/{remote_branch}"),
                )
            }
        };
        Ok(GitTask {
            args,
            remote: true,
            running,
            done,
            ..Default::default()
        })
    }

    // What the chosen options lose, shown in the warning color.
    fn warning(&self) -> Option<&'static str> {
        match &self.kind {
            DialogKind::Reset { .. } if self.choice(FieldId::ResetMode) == 2 => {
                Some("The local changes are lost.")
            }
            DialogKind::Checkout { .. } if self.choice(FieldId::LocalChanges) == 2 => {
                Some("Changes to tracked files are lost, untracked files are kept.")
            }
            DialogKind::Remote(RemoteAction::Push, _) if self.checked(FieldId::Force) => {
                Some("Commits only on the remote branch are lost.")
            }
            DialogKind::Squash { .. } => Some("The commits after them are rewritten too."),
            _ => None,
        }
    }

    // The git commands it will run, under the fields.
    fn summary(&self) -> String {
        let task = match self.command() {
            Ok(task) => task,
            Err(err) => return err,
        };
        let args: Vec<&str> = task.args.iter().map(|a| shorten_hash(a)).collect();
        let command = format!("git {}", args.join(" "));
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
fn commits_intro(heading: &str, commits: &[DialogCommit]) -> Vec<String> {
    const MAX: usize = 10;
    let mut lines = vec![format!("{heading}:")];
    lines.extend(commits.iter().take(MAX).map(|c| format!("  {}", c.line)));
    if commits.len() > MAX {
        lines.push(format!("  … and {} more", commits.len() - MAX));
    }
    lines
}

// A full commit hash is shown short.
fn shorten_hash(arg: &str) -> &str {
    if arg.len() == 40 && arg.chars().all(|c| c.is_ascii_hexdigit()) {
        &arg[..7]
    } else {
        arg
    }
}

pub struct GitDialogWidget<'a> {
    dialog: &'a GitDialog,
    theme: &'a ColorTheme,
    keybind: &'a KeyBind,
}

impl<'a> GitDialogWidget<'a> {
    pub fn new(dialog: &'a GitDialog, ctx: &'a AppContext) -> Self {
        GitDialogWidget {
            dialog,
            theme: &ctx.color_theme,
            keybind: &ctx.keybind,
        }
    }

    // The keys of the events, like "j/k", the first of each.
    fn keys(&self, events: &[UserEvent]) -> String {
        let keys: Vec<String> = events
            .iter()
            .filter_map(|event| self.keybind.keys_for_event(*event).into_iter().next())
            .map(|key| key.to_lowercase())
            .collect();
        keys.join("/")
    }
}

const LABEL_WIDTH: usize = 13;
// long commit subjects and commands are cut at this width
const MAX_CONTENT_WIDTH: usize = 90;

impl Widget for GitDialogWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (dialog, theme) = (self.dialog, self.theme);
        let (move_keys, change_keys, toggle_key, edit_key, run_key, cancel_key) = (
            self.keys(&[UserEvent::NavigateDown, UserEvent::NavigateUp]),
            self.keys(&[UserEvent::NavigateLeft, UserEvent::NavigateRight]),
            self.keys(&[UserEvent::DialogToggle]),
            self.keys(&[UserEvent::DialogEdit]),
            self.keys(&[UserEvent::Confirm]),
            self.keys(&[UserEvent::Cancel]),
        );
        let dim = Style::default().fg(theme.status_input_transient_fg);
        let has_text = dialog
            .fields
            .iter()
            .any(|(.., f)| matches!(f, Field::Text { .. }));
        let has_check = dialog
            .fields
            .iter()
            .any(|(.., f)| matches!(f, Field::Check(_)));
        let mut keys = vec![
            (move_keys.as_str(), "move"),
            (change_keys.as_str(), "change"),
        ];
        if has_check {
            keys.push((toggle_key.as_str(), "toggle"));
        }
        if has_text {
            keys.push((edit_key.as_str(), "type"));
        }
        keys.extend([
            (run_key.as_str(), dialog.verb),
            (cancel_key.as_str(), "cancel"),
        ]);
        keys.retain(|(key, _)| !key.is_empty());
        let keys_hint = key_hints(&keys, theme);
        // the width stays the same while typing
        let min_width = keys_hint.width();
        let hint = if dialog.editing {
            key_hints(&[("type", "a branch name"), ("enter/esc", "done")], theme)
        } else {
            keys_hint
        };

        let mut lines = vec![];
        if !dialog.intro.is_empty() {
            lines.push(Line::raw(""));
            for (i, line) in dialog.intro.iter().enumerate() {
                // the heading stands out from the commits under it
                lines.push(if i == 0 {
                    Line::raw(line.clone()).fg(theme.accent_fg).bold()
                } else {
                    Line::raw(line.clone()).fg(theme.fg)
                });
            }
        }
        lines.push(Line::raw(""));
        let mut cursor = None;
        let mut checks_started = false;
        for (i, (_, label, field)) in dialog.fields.iter().enumerate() {
            let selected = i == dialog.selected;
            // the field being changed has a bar beside it, like huh's forms
            let bar = if selected {
                Span::raw("┃ ").fg(theme.accent_fg)
            } else {
                Span::raw("  ")
            };
            let label_style = if selected {
                Style::default().fg(theme.accent_fg).bold()
            } else {
                Style::default().fg(theme.fg)
            };
            let arrow_style = if selected {
                Style::default().fg(theme.highlight_fg)
            } else {
                dim
            };
            let line = match field {
                Field::Choice { options, index } => {
                    let more = if options.len() > 1 {
                        format!("  {}/{}", index + 1, options.len())
                    } else {
                        String::new()
                    };
                    Line::from(vec![
                        bar,
                        Span::styled(format!("{label:<LABEL_WIDTH$}  "), label_style),
                        Span::styled("‹ ", arrow_style),
                        Span::raw(options[*index].clone()).fg(theme.fg).bold(),
                        Span::styled(" ›", arrow_style),
                        Span::styled(more, dim),
                    ])
                }
                Field::Text { input, options } => {
                    let value = input.value();
                    let known = options.iter().any(|o| o == value.trim());
                    let note = if value.trim().is_empty() || known {
                        ""
                    } else {
                        "  new"
                    };
                    let prefix_width = 2 + LABEL_WIDTH + 2;
                    if selected && dialog.editing {
                        let x = prefix_width + 2 + input.visual_cursor();
                        cursor = Some((x as u16, lines.len() as u16));
                    }
                    Line::from(vec![
                        bar,
                        Span::styled(format!("{label:<LABEL_WIDTH$}  "), label_style),
                        Span::styled("‹ ", arrow_style),
                        Span::raw(value.to_string()).fg(theme.fg).bold(),
                        Span::styled(" ›", arrow_style),
                        Span::raw(note).fg(theme.highlight_fg),
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
        lines.push(Line::raw(""));
        if let Some(warning) = dialog.warning() {
            lines.push(
                Line::raw(format!("! {warning}"))
                    .fg(theme.status_warn_fg)
                    .bold(),
            );
        }
        lines.push(Line::raw(dialog.summary()).patch_style(dim).italic());
        lines.push(Line::raw(""));
        lines.push(hint);

        let content_width = lines
            .iter()
            .map(Line::width)
            .max()
            .unwrap_or_default()
            .clamp(min_width, MAX_CONTENT_WIDTH.max(min_width));
        let width = (content_width as u16 + 4).min(area.width);
        let height = (lines.len() as u16 + 2).min(area.height);
        let popup = Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        );
        Clear.render(popup, buf);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.accent_fg))
            .style(Style::default().fg(theme.fg).bg(theme.bg))
            .title(badge(dialog.title, theme.accent_fg, theme))
            .padding(Padding::horizontal(1));
        let inner = block.inner(popup);
        Paragraph::new(lines).block(block).render(popup, buf);

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
    use ratatui::crossterm::event::KeyModifiers;
    use rustc_hash::FxHashMap;

    use super::*;

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

    fn press(dialog: &mut GitDialog, event: UserEvent) -> DialogOutcome {
        dialog.handle_key(Some(event), key(KeyCode::Null))
    }

    fn args(dialog: &GitDialog) -> String {
        let task = dialog.command().unwrap();
        task.args.join(" ")
    }

    fn commit(n: char) -> DialogCommit {
        DialogCommit {
            hash: n.to_string().repeat(40),
            line: format!("{n} subject"),
        }
    }

    #[test]
    fn test_fetch_defaults_to_upstream_remote() {
        let mut dialog = GitDialog::remote(RemoteAction::Fetch, info()).unwrap();
        assert_eq!(args(&dialog), "fetch origin");
        assert!(dialog.command().unwrap().remote);
        press(&mut dialog, UserEvent::NavigateRight);
        assert_eq!(args(&dialog), "fetch fork");
        assert_eq!(press(&mut dialog, UserEvent::Cancel), DialogOutcome::Close);
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
        let dialog = GitDialog::merge("main", "feature".into(), "feature", Some("false"));
        assert_eq!(args(&dialog), "merge --no-ff --no-edit feature");
        let dialog = GitDialog::merge("main", "feature".into(), "feature", Some("only"));
        assert_eq!(args(&dialog), "merge --ff-only feature");
        let dialog = GitDialog::rebase("main", "feature".into(), "feature", false);
        assert_eq!(args(&dialog), "rebase --no-autostash feature");
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
        assert!(matches!(outcome, DialogOutcome::Run(t) if t.running == "Pulling origin/dev"));
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
        assert!(dialog.command().unwrap().stash);
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
        let task = dialog.command().unwrap();
        assert_eq!(args(&dialog), "rebase -i --autostash --root");
        assert!(task.env.iter().any(|(k, v)| k == "GIT_SEQUENCE_EDITOR"
            && v == &format!("sed -i.bak -E -e 's/^(pick|p) {b} /squash {b} /'")));
    }

    // Runs git in the repository, returning what it printed.
    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

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
            hash: rev("HEAD~1"),
            line: String::new(),
        }];
        // c3 into c2, rebasing after c1
        let dialog = GitDialog::squash("main", commits, Some(rev("HEAD~3")), true);
        crate::git::run_task(path, &dialog.command().unwrap()).unwrap();

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
        crate::git::run_task(path, &dialog.command().unwrap()).unwrap();

        assert_eq!(git(path, &["branch", "--show-current"]).trim(), "old");
        assert!(path.join("untracked").exists());
        assert_eq!(git(path, &["stash", "list"]), "");
    }
}
