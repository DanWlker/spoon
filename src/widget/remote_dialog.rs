use ratatui::{
    buffer::Buffer,
    crossterm::event::{Event, KeyCode, KeyEvent},
    layout::Rect,
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Widget},
};
use tui_input::{backend::crossterm::EventHandler, Input};

use crate::{color::ColorTheme, event::UserEvent, git::RemoteInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteAction {
    Fetch,
    Pull,
    Push,
}

// A git command to run against a remote, and what to say while and after it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCommand {
    pub args: Vec<String>,
    // "Pushing main to origin/main"
    pub running: String,
    // "Pushed main to origin/main"
    pub done: String,
}

impl RemoteCommand {
    pub fn fetch_all() -> Self {
        RemoteCommand {
            args: vec!["fetch".into(), "--all".into()],
            running: "Fetching all remotes".into(),
            done: "Fetched all remotes".into(),
        }
    }
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
    Run(RemoteCommand),
    Error(String),
}

// The popup to fetch from a remote, pull a branch, or push one.
#[derive(Debug)]
pub struct RemoteDialog {
    action: RemoteAction,
    info: RemoteInfo,
    fields: Vec<(FieldId, &'static str, Field)>,
    selected: usize,
    // typing into the selected text field
    editing: bool,
}

impl RemoteDialog {
    pub fn new(action: RemoteAction, info: RemoteInfo) -> Result<Self, String> {
        if info.remotes.is_empty() {
            return Err("No remotes configured".into());
        }
        let mut dialog = RemoteDialog {
            action,
            info,
            fields: vec![],
            selected: 0,
            editing: false,
        };
        match action {
            RemoteAction::Fetch => {
                let remote = dialog.default_remote(dialog.info.current_branch.as_deref());
                dialog.push_choice(
                    FieldId::Remote,
                    "Remote",
                    dialog.info.remotes.clone(),
                    &remote,
                );
            }
            RemoteAction::Pull => {
                let current = dialog.info.current_branch.clone();
                let remote = dialog.default_remote(current.as_deref());
                dialog.push_choice(
                    FieldId::Remote,
                    "Remote",
                    dialog.info.remotes.clone(),
                    &remote,
                );
                dialog
                    .fields
                    .push((FieldId::Branch, "Branch", Field::text("", vec![])));
                dialog.update_pull_branch(None);
                let (rebase, auto_stash) = (dialog.info.pull_rebase, dialog.info.auto_stash);
                dialog.push_check(FieldId::Rebase, "Rebase instead of merge", rebase);
                dialog.push_check(
                    FieldId::AutoStash,
                    "Stash and reapply local changes",
                    auto_stash,
                );
            }
            RemoteAction::Push => {
                if dialog.info.branches.is_empty() {
                    return Err("No local branches to push".into());
                }
                let current = dialog.info.current_branch.clone().unwrap_or_default();
                let branches = dialog.info.branches.clone();
                dialog.push_choice(FieldId::LocalBranch, "Branch", branches, &current);
                let remotes = dialog.info.remotes.clone();
                dialog.push_choice(FieldId::Remote, "To remote", remotes, "");
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
        branch
            .and_then(|branch| self.info.upstreams.get(branch))
            .map(|(remote, _)| remote.clone())
            .filter(|remote| self.info.remotes.contains(remote))
            .or_else(|| self.info.remotes.iter().find(|r| *r == "origin").cloned())
            .unwrap_or_else(|| self.info.remotes[0].clone())
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

    fn checked(&self, id: FieldId) -> bool {
        matches!(self.field(id), Some(Field::Check(true)))
    }

    fn remote_branches(&self, remote: &str) -> Vec<String> {
        self.info
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
        let current = self.info.current_branch.clone();
        let upstream = current
            .as_ref()
            .and_then(|branch| self.info.upstreams.get(branch))
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
            .info
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
        match event {
            Some(UserEvent::Cancel | UserEvent::Close | UserEvent::Quit) => DialogOutcome::Close,
            Some(UserEvent::NavigateDown | UserEvent::SelectDown) => {
                self.selected = (self.selected + 1) % self.fields.len();
                DialogOutcome::None
            }
            Some(UserEvent::NavigateUp | UserEvent::SelectUp) => {
                self.selected = (self.selected + self.fields.len() - 1) % self.fields.len();
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
            Some(UserEvent::ToggleStage | UserEvent::ToggleSelect) => {
                if let Field::Check(checked) = &mut self.fields[self.selected].2 {
                    *checked = !*checked;
                }
                DialogOutcome::None
            }
            Some(UserEvent::Edit) => {
                self.editing = matches!(self.fields[self.selected].2, Field::Text { .. });
                DialogOutcome::None
            }
            Some(UserEvent::Confirm) => match self.command() {
                Ok(command) => DialogOutcome::Run(command),
                Err(err) => DialogOutcome::Error(err),
            },
            _ => DialogOutcome::None,
        }
    }

    // Changes the selected field to its next or previous option, or toggles it.
    fn change(&mut self, forward: bool) {
        let id = self.fields[self.selected].0;
        let previous_branch = self.value(FieldId::Branch);
        match &mut self.fields[self.selected].2 {
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
        match (self.action, id) {
            (RemoteAction::Pull, FieldId::Remote) => self.update_pull_branch(Some(previous_branch)),
            (RemoteAction::Push, FieldId::LocalBranch) => self.update_push_remote(),
            (RemoteAction::Push, FieldId::Remote) => self.update_push_remote_branch(),
            _ => {}
        }
    }

    pub fn command(&self) -> Result<RemoteCommand, String> {
        let remote = self.value(FieldId::Remote);
        match self.action {
            RemoteAction::Fetch => Ok(RemoteCommand {
                args: vec!["fetch".into(), remote.clone()],
                running: format!("Fetching {remote}"),
                done: format!("Fetched {remote}"),
            }),
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
                if self.checked(FieldId::AutoStash) {
                    args.push("--autostash".into());
                }
                args.extend([remote.clone(), branch.clone()]);
                Ok(RemoteCommand {
                    args,
                    running: format!("Pulling {remote}/{branch}"),
                    done: format!("Pulled {remote}/{branch}"),
                })
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
                if !self.info.upstreams.contains_key(&branch) {
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
                Ok(RemoteCommand {
                    args,
                    running: format!("{force} {branch} to {remote}/{remote_branch}"),
                    done: format!("Pushed {branch} to {remote}/{remote_branch}"),
                })
            }
        }
    }

    fn title(&self) -> &'static str {
        match self.action {
            RemoteAction::Fetch => " Fetch ",
            RemoteAction::Pull => " Pull ",
            RemoteAction::Push => " Push ",
        }
    }

    // What the command will do, under the fields.
    fn summary(&self) -> String {
        match self.command() {
            Ok(command) => format!("git {}", command.args.join(" ")),
            Err(err) => err,
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

pub struct RemoteDialogWidget<'a> {
    dialog: &'a RemoteDialog,
    theme: &'a ColorTheme,
}

impl<'a> RemoteDialogWidget<'a> {
    pub fn new(dialog: &'a RemoteDialog, theme: &'a ColorTheme) -> Self {
        RemoteDialogWidget { dialog, theme }
    }
}

const LABEL_WIDTH: usize = 12;

impl Widget for RemoteDialogWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (dialog, theme) = (self.dialog, self.theme);
        let dim = Style::default().fg(theme.status_input_transient_fg);
        let verb = match dialog.action {
            RemoteAction::Fetch => "fetch",
            RemoteAction::Pull => "pull",
            RemoteAction::Push => "push",
        };
        let keys_hint =
            format!("j/k move  h/l change  space toggle  e type  enter {verb}  esc cancel");
        // the width stays the same while typing
        let min_width = keys_hint.chars().count();
        let hint = if dialog.editing {
            "type a branch name    enter / esc: done".to_string()
        } else {
            keys_hint
        };

        let mut lines = vec![Line::raw("")];
        let mut cursor = None;
        let mut checks_started = false;
        for (i, (_, label, field)) in dialog.fields.iter().enumerate() {
            let selected = i == dialog.selected;
            let row_style = if selected {
                Style::default()
                    .fg(theme.list_selected_fg)
                    .bg(theme.list_selected_bg)
            } else {
                Style::default().fg(theme.fg)
            };
            let marker = if selected { "▌" } else { " " };
            let line = match field {
                Field::Choice { options, index } => {
                    let more = if options.len() > 1 {
                        format!("  {}/{}", index + 1, options.len())
                    } else {
                        String::new()
                    };
                    Line::from(vec![
                        Span::raw(format!("{marker} {label:<LABEL_WIDTH$}  ")),
                        Span::raw("‹ ").patch_style(dim),
                        Span::raw(options[*index].clone()).bold(),
                        Span::raw(" ›").patch_style(dim),
                        Span::raw(more).patch_style(dim),
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
                    let prefix = format!("{marker} {label:<LABEL_WIDTH$}  ");
                    if selected && dialog.editing {
                        let x = prefix.chars().count() + 2 + input.visual_cursor();
                        cursor = Some((x as u16, lines.len() as u16));
                    }
                    Line::from(vec![
                        Span::raw(prefix),
                        Span::raw("‹ ").patch_style(dim),
                        Span::raw(value.to_string()).bold(),
                        Span::raw(" ›").patch_style(dim),
                        Span::raw(note).fg(theme.status_info_fg),
                    ])
                }
                Field::Check(checked) => {
                    if !checks_started {
                        checks_started = true;
                        lines.push(Line::raw(""));
                    }
                    let mark = if *checked { "[x]" } else { "[ ]" };
                    Line::raw(format!("{marker} {mark} {label}"))
                }
            };
            lines.push(line.patch_style(row_style));
        }
        lines.push(Line::raw(""));
        lines.push(Line::raw(dialog.summary()).patch_style(dim).italic());
        lines.push(Line::raw(""));
        lines.push(Line::raw(hint.clone()).patch_style(dim));

        let content_width = lines
            .iter()
            .map(Line::width)
            .max()
            .unwrap_or_default()
            .max(min_width);
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
            .border_style(Style::default().fg(theme.help_block_title_fg))
            .style(Style::default().fg(theme.fg).bg(theme.bg))
            .title(Span::raw(dialog.title()).bold())
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
            auto_stash: false,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn press(dialog: &mut RemoteDialog, event: UserEvent) -> DialogOutcome {
        dialog.handle_key(Some(event), key(KeyCode::Null))
    }

    fn args(dialog: &RemoteDialog) -> String {
        dialog.command().unwrap().args.join(" ")
    }

    #[test]
    fn test_fetch_defaults_to_upstream_remote() {
        let mut dialog = RemoteDialog::new(RemoteAction::Fetch, info()).unwrap();
        assert_eq!(args(&dialog), "fetch origin");
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
        assert!(RemoteDialog::new(RemoteAction::Pull, info).is_err());
    }

    #[test]
    fn test_pull_options() {
        let mut dialog = RemoteDialog::new(RemoteAction::Pull, info()).unwrap();
        // pull.rebase is set
        assert_eq!(args(&dialog), "pull --rebase origin main");

        // the branch follows the remote
        press(&mut dialog, UserEvent::NavigateLeft);
        assert_eq!(args(&dialog), "pull --rebase fork topic");
        press(&mut dialog, UserEvent::NavigateRight);
        press(&mut dialog, UserEvent::NavigateDown);
        press(&mut dialog, UserEvent::NavigateRight);
        assert_eq!(args(&dialog), "pull --rebase origin dev");

        press(&mut dialog, UserEvent::NavigateDown);
        press(&mut dialog, UserEvent::ToggleStage);
        press(&mut dialog, UserEvent::NavigateDown);
        press(&mut dialog, UserEvent::ToggleStage);
        assert_eq!(args(&dialog), "pull --no-rebase --autostash origin dev");

        let outcome = press(&mut dialog, UserEvent::Confirm);
        assert!(matches!(outcome, DialogOutcome::Run(c) if c.running == "Pulling origin/dev"));
    }

    #[test]
    fn test_push_options() {
        let mut dialog = RemoteDialog::new(RemoteAction::Push, info()).unwrap();
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
        press(&mut dialog, UserEvent::Edit);
        assert!(dialog.editing);
        dialog.handle_key(None, key(KeyCode::Char('2')));
        dialog.handle_key(None, key(KeyCode::Enter));
        assert!(!dialog.editing);

        press(&mut dialog, UserEvent::NavigateDown);
        press(&mut dialog, UserEvent::ToggleStage);
        press(&mut dialog, UserEvent::NavigateDown);
        press(&mut dialog, UserEvent::ToggleStage);
        assert_eq!(
            args(&dialog),
            "push --force-with-lease --tags --set-upstream origin refs/heads/topic:refs/heads/topic2"
        );
    }
}
