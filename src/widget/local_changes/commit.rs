// The commit section of the local changes page: the message box, committing and amending, and
// going on with an operation in progress, like a rebase or a merge.

use super::*;

// The commit section's contents, kept across refreshes.
#[derive(Debug, Default, Clone)]
pub struct CommitDraft {
    // the first line of the message, and the rest after a blank line
    pub(super) summary: TextArea,
    pub(super) description: TextArea,
    pub(super) amend: Option<Amend>,
    // git's prepared message put in the message box for the operation in progress, see
    // `prepare_message`
    pub(super) prepared_message: Option<String>,
}

// Amending the commit after `base`, the parent of HEAD: the staged section is compared to it
// instead of HEAD, so it shows the changes of the commit being amended together with what is staged.
#[derive(Debug, Clone)]
pub(super) struct Amend {
    pub(super) base: String,
    // HEAD when amending started: once it moves, the amend was of another commit
    pub(super) head: CommitHash,
    // the message typed before amending, restored when amending is turned off
    pub(super) message_before: (TextArea, TextArea),
    pub(super) of: AmendOf,
}

// What is amended: the last commit, or the one a rebase stopped at to edit, amending turned off
// when the rebase goes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AmendOf {
    LastCommit,
    EditStop,
}

impl CommitDraft {
    // Amends the commit after `base`, its `message` in place of the typed one.
    pub(super) fn start_amend(
        &mut self,
        base: String,
        head: CommitHash,
        message: &str,
        of: AmendOf,
    ) {
        let message_before = (self.summary.clone(), self.description.clone());
        self.amend = Some(Amend {
            base,
            head,
            message_before,
            of,
        });
        self.set_message(message);
    }

    // Stops amending, the typed message back.
    pub(super) fn stop_amend(&mut self) {
        if let Some(amend) = self.amend.take() {
            (self.summary, self.description) = amend.message_before;
        }
    }

    pub(super) fn amend_base(&self) -> Option<&str> {
        self.amend.as_ref().map(|amend| amend.base.as_str())
    }

    // The summary, then the description after a blank line.
    pub(super) fn message(&self) -> String {
        let summary = self.summary.text().trim().to_string();
        let description = self.description.text().trim_end().to_string();
        if description.trim().is_empty() {
            summary
        } else {
            format!("{summary}\n\n{description}")
        }
    }

    // Splits `message` into its first line and the rest, without the blank lines between.
    pub(super) fn set_message(&mut self, message: &str) {
        let (summary, description) = message
            .trim()
            .split_once('\n')
            .unwrap_or((message.trim(), ""));
        self.summary = TextArea::new(summary);
        self.description = TextArea::new(description.trim_start_matches('\n'));
    }

    pub(super) fn is_empty(&self) -> bool {
        self.summary.is_empty() && self.description.is_empty()
    }

    pub(super) fn field_mut(&mut self, field: CommitField) -> &mut TextArea {
        match field {
            CommitField::Summary => &mut self.summary,
            CommitField::Description => &mut self.description,
        }
    }
}

impl LocalChangesState<'_> {
    // Focuses the commit section, typing the message right away when there is none yet.
    pub fn focus_commit(&mut self) {
        self.focus_files();
        self.chosen = if self.draft.is_empty() {
            Focus::Typing(CommitField::Summary)
        } else {
            Focus::Commit(CommitField::Summary)
        };
    }

    // A rebase stopped to edit a commit, which committing amends before going on, see
    // `sync_amend`.
    pub(super) fn editing_commit(&self) -> bool {
        self.draft
            .amend
            .as_ref()
            .is_some_and(|amend| amend.of == AmendOf::EditStop)
    }

    // The box selected in the commit section, while it has the focus.
    pub(super) fn commit_field(&self) -> Option<CommitField> {
        match self.focus() {
            Focus::Commit(field) | Focus::Typing(field) => Some(field),
            _ => None,
        }
    }

    // Handles a key typed into the commit message: Esc stops editing, Tab and Shift-Tab switch
    // between the summary and the description, Enter in the one-line summary goes on below.
    pub fn handle_text_key(&mut self, key: KeyEvent) {
        let Focus::Typing(field) = self.focus() else {
            return;
        };
        match (key.code, field) {
            (KeyCode::Esc, _) => self.chosen = Focus::Commit(field),
            (KeyCode::Tab | KeyCode::BackTab, CommitField::Summary)
            | (KeyCode::Enter, CommitField::Summary) => {
                self.chosen = Focus::Typing(CommitField::Description);
            }
            (KeyCode::Tab | KeyCode::BackTab, CommitField::Description) => {
                self.chosen = Focus::Typing(CommitField::Summary);
            }
            (_, field) => self.draft.field_mut(field).handle_key(key),
        }
    }

    pub fn is_commit_focused(&self) -> bool {
        matches!(self.focus(), Focus::Commit(_) | Focus::Typing(_))
    }

    // Asks to confirm the commit (which also finishes a merge, cherry-pick or revert, or goes on with
    // the rebase it stopped in), or starts typing the message when there is none yet.
    pub fn commit(&mut self) -> Result<(), String> {
        if self.draft.summary.text().trim().is_empty() {
            self.focus_files();
            self.chosen = Focus::Typing(CommitField::Summary);
            return Ok(());
        }
        self.request_commit()
    }

    // Focuses the commit section and starts typing in the selected box, the summary when it
    // didn't have the focus.
    pub fn edit_message(&mut self) {
        let field = self.commit_field().unwrap_or_default();
        self.focus_files();
        self.chosen = Focus::Typing(field);
    }

    // Asks to confirm the commit, showing what it will be: the branch, the message and the files.
    pub(super) fn request_commit(&mut self) -> Result<(), String> {
        self.check_commit()?;
        let branch = self.repository.current_branch();
        let kind = match (&self.draft.amend, self.repository.operation_in_progress()) {
            // HEAD is the commit being edited, on no branch while rebasing
            (Some(amend), _) if amend.of == AmendOf::EditStop => CommitKind::AmendEditStop {
                commit: self
                    .repository
                    .head_commit()
                    .unwrap_or_else(|| "HEAD".into()),
            },
            (Some(_), _) => CommitKind::AmendLast { branch },
            (None, finishing) => CommitKind::New { branch, finishing },
        };
        let subject = self.draft.summary.text().trim().to_string();
        let description = self.draft.description.text();
        let body: Vec<String> = description.trim().lines().map(String::from).collect();
        let files = self.changes.staged.clone();
        // amending a commit a rebase stopped at was warned about when it started
        let pushed = matches!(kind, CommitKind::AmendLast { .. })
            .then(|| self.repository.upstream_containing("HEAD"))
            .flatten();
        let preview = CommitPreview {
            kind,
            subject,
            body,
            files,
        };
        let dialog = GitDialog::commit(preview, self.commit_task()?).pushed(pushed);
        self.request = Some(Request::Dialog(dialog));
        Ok(())
    }

    // Whether there is something to commit; the message isn't empty, see `commit`.
    pub(super) fn check_commit(&self) -> Result<(), String> {
        let amend = self.draft.amend.is_some();
        // a merge can be committed without changes, when its resolution keeps one side
        if !amend
            && self.changes.staged.is_empty()
            && self.repository.operation_in_progress().is_none()
        {
            return Err("Nothing staged to commit".into());
        }
        Ok(())
    }

    // Asks for confirmation to abort the operation in progress; its key works only then.
    pub fn request_abort(&mut self) {
        if let Some(operation) = self.repository.operation_in_progress() {
            self.request = Some(Request::Dialog(GitDialog::abort(operation)));
        }
    }

    // Goes on with the rebase in progress, the other operations being finished by committing; its
    // key works only then.
    pub fn continue_rebase(&mut self) {
        let Some(operation) = self.continuing_operation() else {
            return;
        };
        let name = operation.name();
        self.request = Some(Request::Task(GitTask {
            commands: self
                .repository
                .continue_args(operation)
                .into_iter()
                .collect(),
            running: format!("Continuing the {name}"),
            done: format!("Continued the {name}"),
            ..Default::default()
        }));
    }

    // Asks for confirmation to skip the commit the rebase, cherry-pick or revert stopped at; its
    // key works only then, a merge having no commit to skip.
    pub fn request_skip(&mut self) {
        if let Some(operation) = self.skippable_operation() {
            self.request = Some(Request::Dialog(GitDialog::skip(operation)));
        }
    }

    pub(super) fn skippable_operation(&self) -> Option<Operation> {
        self.repository
            .operation_in_progress()
            .filter(|operation| *operation != Operation::Merge)
    }

    // Fills the empty message box with git's message for the merge, cherry-pick or revert in progress,
    // once, so it can still be cleared.
    pub(super) fn prepare_message(&mut self) {
        let Some(operation) = self.repository.operation_in_progress() else {
            // a squash merge leaves its message without a merge in progress
            match self.repository.squash_message() {
                Some(message) if self.draft.prepared_message.is_none() && self.draft.is_empty() => {
                    self.draft.set_message(&message);
                    self.draft.prepared_message = Some(self.draft.message());
                }
                Some(_) => {}
                // the operation is over, aborted say: its message goes too, unless it was edited
                None => {
                    if self.draft.prepared_message.take() == Some(self.draft.message()) {
                        self.draft.set_message("");
                    }
                }
            }
            return;
        };
        if operation.continues_without_commit()
            || self.draft.prepared_message.is_some()
            || !self.draft.is_empty()
        {
            return;
        }
        if let Some(message) = self.repository.prepared_message() {
            self.draft.set_message(&message);
            self.draft.prepared_message = Some(self.draft.message());
        }
    }

    // The rebase in progress, which goes on without committing, see `continue_rebase`.
    pub(super) fn continuing_operation(&self) -> Option<Operation> {
        self.repository
            .operation_in_progress()
            .filter(|o| o.continues_without_commit())
    }

    // Commits the staged changes with the typed message, amending HEAD while amend is checked, then
    // goes on with the rebase, cherry-pick or revert it concludes a stop of; the app runs it and
    // clears the message once the commit is made.
    pub(super) fn commit_task(&self) -> Result<GitTask, String> {
        self.check_commit()?;
        let amend = self.draft.amend.is_some();
        // the message is read from stdin so no editor is opened
        let mut commit = GitCommand::Commit.args(["--quiet", "--file=-"]);
        if amend {
            commit.args.push("--amend".into());
        }
        let continued = self
            .repository
            .operation_in_progress()
            .and_then(|operation| self.repository.continue_args(operation));
        let (running, done) = if amend {
            ("Amending the last commit", "Amended the last commit")
        } else {
            ("Committing", "Committed")
        };
        Ok(GitTask {
            commands: std::iter::once(commit).chain(continued).collect(),
            input: Some(self.draft.message()),
            running: running.into(),
            done: done.into(),
            ..Default::default()
        })
    }

    // Empties the message box and stops amending, once the commit is made.
    pub fn clear_draft(&mut self) {
        self.draft = CommitDraft::default();
    }

    // Turning amend on shows the changes of HEAD in the staged section, combined with what is staged,
    // and the HEAD message in place of the typed one. Turning it off restores both.
    pub fn toggle_amend(&mut self) -> Result<(), String> {
        if self.editing_commit() {
            return Err("The commit being edited is amended; C aborts the rebase".into());
        }
        if let Some(operation) = self
            .repository
            .operation_in_progress()
            .filter(|_| self.draft.amend.is_none())
        {
            // HEAD is the commit before the operation, amending it would mix the two up
            return Err(format!("Cannot amend during a {}", operation.name()));
        }
        if self.draft.amend.is_some() {
            self.draft.stop_amend();
        } else {
            let base = self.repository.amend_base()?;
            let message = self.repository.head_commit_message()?;
            let head = self.repository.head_commit().ok_or("No commit to amend")?;
            self.draft
                .start_amend(base, head, &message, AmendOf::LastCommit);
        }
        self.reload();
        Ok(())
    }

    // Turns amending on, if it isn't, focusing the commit section with the message of the commit.
    pub fn start_amend(&mut self) -> Result<(), String> {
        if self.draft.amend.is_none() {
            self.toggle_amend()?;
        }
        self.focus_commit();
        Ok(())
    }

    // Loads the changes again, after amending turned on or off changed the staged section.
    pub(super) fn reload_staged(&mut self) {
        self.changes = self.with_amend(self.repository.local_changes());
        let tree = file_tree(&self.changes.staged);
        self.sections[1].tree.set_tree(tree);
    }

    // A rebase stopped to edit a commit turns amending on, with the message of that commit, and
    // amending goes off again once the rebase goes on or is aborted, or once HEAD moves, by a
    // checkout or a commit made outside the app.
    pub(super) fn sync_amend(&mut self) {
        let head = self.repository.head_commit();
        let moved = self
            .draft
            .amend
            .as_ref()
            .is_some_and(|amend| Some(&amend.head) != head.as_ref());
        if moved {
            self.draft.stop_amend();
        }
        let stopped = self.repository.operation_in_progress() == Some(Operation::Rebase)
            && self.repository.rebase_stopped_to_edit();
        let changed = moved
            | if stopped && !self.editing_commit() {
                match (
                    self.repository.amend_base(),
                    self.repository.head_commit_message(),
                    head,
                ) {
                    // amending the last commit before, it's the commit stopped at now
                    (Ok(base), Ok(message), Some(head)) => {
                        self.draft.stop_amend();
                        self.draft
                            .start_amend(base, head, &message, AmendOf::EditStop);
                        true
                    }
                    _ => false,
                }
            } else if !stopped && self.editing_commit() {
                self.draft.stop_amend();
                true
            } else {
                false
            };
        if changed {
            self.reload_staged();
        }
    }

    // While amending, the staged section shows the changes of HEAD too, see `Amend`.
    pub(super) fn with_amend(&self, mut changes: LocalChanges) -> LocalChanges {
        if let Some(base) = self.draft.amend_base() {
            changes.staged = self.repository.staged_changes_against(base);
        }
        changes
    }
}

impl LocalChangesPanel<'_> {
    pub(super) fn render_commit(&self, state: &LocalChangesState, area: Rect, buf: &mut Buffer) {
        let theme = &self.ctx.color_theme;
        let focused = self.focused && state.is_commit_focused();
        // amending and rebasing have no controls of their own, the title shows them
        let mut title = panel_title("[3] Commit", [], focused, theme).spans;
        if state.draft.amend.is_some() {
            title.push(warn_note("amending", theme));
        }
        if let Some(operation) = state.repository.operation_in_progress() {
            let doing = match operation {
                Operation::Rebase => "rebasing",
                Operation::Merge => "merging",
                Operation::CherryPick => "cherry-picking",
                Operation::Revert => "reverting",
            };
            title.push(warn_note(doing, theme));
        }
        let text_area = render_panel(buf, area, Line::from(title), focused, theme);
        self.render_message(state, text_area, buf);
        // for the mouse, as the commit key; dimmed with nothing to commit
        let style = if state.can_commit() {
            Style::default().fg(theme.accent_fg).bold()
        } else {
            Style::default().fg(theme.muted_fg)
        };
        let button = render_border_buttons(buf, area, &[("[Commit]", style)], theme);
        let mut layout = self.ctx.layout.borrow_mut();
        for at in &button {
            layout.record(Target::CommitButton, *at, RowMap::One);
        }
        // at the bottom left, as the amend key; dimmed where it can't toggle
        const AMEND: &str = "Amend";
        let width = 4 + AMEND.len() as u16;
        let fits = button
            .first()
            .is_some_and(|at| area.x.saturating_add(3 + width) < at.x);
        if fits {
            let label = match state.keys().takes(UserEvent::ToggleAmend) {
                true => Style::default().fg(theme.fg),
                false => Style::default().fg(theme.muted_fg),
            };
            let mark = check_mark(state.draft.amend.is_some(), label, theme);
            let line = Line::from(vec![
                Span::raw(" "),
                mark,
                Span::styled(AMEND, label),
                Span::raw(" "),
            ]);
            let y = area.bottom() - 1;
            buf.set_line(area.x + 1, y, &line, width + 2);
            layout.record(
                Target::AmendCheck,
                Rect::new(area.x + 2, y, width, 1),
                RowMap::One,
            );
        }
    }

    // The summary, a line, then the description, each with its placeholder while empty; the bar
    // beside the box selected.
    pub(super) fn render_message(&self, state: &LocalChangesState, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let theme = &self.ctx.color_theme;
        let [summary_area, divider_area, description_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .areas(area);
        let selected = state.commit_field();
        let editing = state.is_editing();
        let boxes = [
            (
                CommitField::Summary,
                &state.draft.summary,
                "Summary",
                summary_area,
            ),
            (
                CommitField::Description,
                &state.draft.description,
                "Description",
                description_area,
            ),
        ];
        for (field, text, empty, box_area) in boxes {
            let mut layout = self.ctx.layout.borrow_mut();
            layout.record(Target::Commit(field), box_area, RowMap::None);
            drop(layout);
            let typing = editing && selected == Some(field);
            if text.is_empty() && !typing {
                placeholder(empty, theme).render(box_area, buf);
            } else {
                let caret = if typing { Caret::Shown } else { Caret::Hidden };
                // the one-line summary scrolls sideways, the description wraps
                let lines = match field {
                    CommitField::Summary => Lines::Scrolled,
                    CommitField::Description => Lines::Wrapped,
                };
                let style = Style::default().fg(theme.fg);
                text.render(box_area, buf, style, caret, lines);
            }
            let focused = self.focused && state.is_commit_focused();
            if focused && selected == Some(field) && box_area.height > 0 && area.x > 0 {
                mark_selected_bar(buf, area.x - 1, box_area.y, true, theme);
            }
        }
        divider(divider_area.width as usize, theme).render(divider_area, buf);
    }
}
