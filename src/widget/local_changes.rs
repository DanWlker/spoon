use std::{collections::BTreeSet, path::PathBuf, rc::Rc};

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Padding, Paragraph, StatefulWidget, Widget},
};

use ratatui::crossterm::event::{Event, KeyCode, KeyEvent};
use tui_input::{backend::crossterm::EventHandler, Input};

use crate::{
    app::AppContext,
    event::{KeyHint, UserEvent},
    git::{
        ConflictSide, FilesSnapshot, GitTask, Head, LocalChangeFile, LocalChangeStatus,
        LocalChanges, Operation, Repository,
    },
    layout::{fit_size, Direction, Panel},
    widget::{
        conflict::ConflictView,
        diff::{
            diff_line_count, diff_lines, parse_diff, render_diff_lines, DiffCursor, DiffHunk,
            ParsedDiff, Step, CONVERTED_DIFF,
        },
        file_tree::{
            find_next, path_contains, FileTree, FileTreeRow, FileTreeState, SearchDirection,
        },
        style::{
            badge, mark_selected_row, panel_block, panel_title, popup_title,
            render_panel_scrollbar, render_popup, tree_panel_width,
        },
        text_area::TextArea,
        tree_query::{
            dir_line, highlight_match, highlighted_name, QueryChange, QueryKind, TreeQuery,
        },
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalChangesSection {
    Unstaged,
    Staged,
}

// The choices of the conflict panel: a side to keep, or a tool to resolve the conflicts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConflictItem {
    Existing,
    Incoming,
    MergeTool,
    Editor,
}

// An interactive program for the view to run in place of the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalRequest {
    // an absolute path
    Editor(PathBuf),
    // a path relative to the top of the working tree
    MergeTool(String),
}

// An action waiting for the user to confirm it in a popup.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ConfirmAction {
    Discard,
    Abort(Operation),
    Commit(Box<CommitPreview>),
}

// The commit to make, shown in the popup confirming it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CommitPreview {
    // "Commit to", "Amend the last commit on", "Commit the merge on"
    action: String,
    branch: String,
    amend: bool,
    subject: String,
    body: Vec<String>,
    files: Vec<(LocalChangeStatus, String)>,
}

// The commit section's contents, kept across refreshes.
#[derive(Debug, Default, Clone)]
pub struct CommitDraft {
    message: TextArea,
    // set while amending: the parent of HEAD, which the staged section is compared to instead of HEAD,
    // so it shows the changes of the commit being amended together with what is staged
    amend_base: Option<String>,
    // the message typed before amending, restored when amending is turned off
    message_before_amend: Option<TextArea>,
    // amending the commit a rebase stopped at to edit, turned off when the rebase goes on
    amend_for_edit: bool,
    // whether git's prepared message was put in the message box for the operation in progress,
    // see `prepare_message`
    message_prepared: bool,
}

impl CommitDraft {
    // Amends the commit after `base`, its `message` in place of the typed one.
    fn start_amend(&mut self, base: String, message: &str) {
        self.amend_base = Some(base);
        self.message_before_amend =
            Some(std::mem::replace(&mut self.message, TextArea::new(message)));
    }

    // Stops amending, the typed message back.
    fn stop_amend(&mut self) {
        self.amend_base = None;
        self.amend_for_edit = false;
        self.message = self.message_before_amend.take().unwrap_or_default();
    }
}

// Where the keys go on the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    // the files of the current section
    Files,
    // the hunks of the diff
    Hunks,
    // the changed lines of the diff
    Lines,
    // the choices of the conflict panel, shown instead of the diff of a conflicted file
    Conflict,
    // the commit section, `editing` while typing the message
    Commit { editing: bool },
}

// A file section: its tree of files, search and filter, and picked rows.
#[derive(Debug)]
struct Section {
    // the root row, then the directories (expanded by default) and files
    tree: FileTreeState,
    // the search and filter, applying to this section only
    query: TreeQuery,
    // paths of the rows picked with `v`, which actions apply to instead of the cursor
    picked: BTreeSet<String>,
}

impl Section {
    fn new(files: &[LocalChangeFile]) -> Self {
        Self {
            tree: FileTreeState::rooted(file_tree(files)),
            query: TreeQuery::default(),
            picked: BTreeSet::new(),
        }
    }
}

fn file_tree(files: &[LocalChangeFile]) -> FileTree {
    FileTree::new(files.iter().map(|f| f.path.as_str()))
}

// The lines of the message, and the files, shown in the popup confirming a commit.
const COMMIT_PREVIEW_LINES: usize = 6;

// What is kept of the page while it isn't shown, or across a refresh.
#[derive(Debug, Clone)]
pub struct LocalChangesSnapshot {
    pub section: LocalChangesSection,
    pub selected_path: Option<String>,
    // boxed to keep the refresh context small
    pub commit_draft: Box<CommitDraft>,
}

impl LocalChangesSnapshot {
    #[cfg(test)]
    pub fn commit_draft_message(&self) -> String {
        self.commit_draft.message.text()
    }
}

impl Default for LocalChangesSnapshot {
    // the page as it is opened the first time
    fn default() -> Self {
        Self {
            section: LocalChangesSection::Unstaged,
            selected_path: None,
            commit_draft: Box::default(),
        }
    }
}

#[derive(Debug)]
pub struct LocalChangesState<'a> {
    repository: &'a Repository,
    // the paths of local changes are relative to it
    changes: LocalChanges,
    // the unstaged and the staged section, see `LocalChangesSection`
    sections: [Section; 2],
    section: LocalChangesSection,
    // whether the diff shows the picked files rather than the row under the cursor
    diff_of_picked: bool,
    focus: Focus,
    diff: ParsedDiff,
    // the selected hunk or line, and the scroll of the diff (or of the conflict sides)
    cursor: DiffCursor,
    // searching the diff while it has the focus
    diff_query: TreeQuery,

    // the message, and amending
    draft: CommitDraft,
    // a rebase stopped to edit a commit, which committing amends before going on
    editing_commit: bool,
    // set once a stash is made, until the view picks it up to refresh the repository
    needs_refresh: bool,
    // a git command for the app to run in the background, see `take_task`
    task: Option<GitTask>,
    // the cursor moved to another row, whose diff loads once used, see `move_cursor`
    diff_stale: bool,
    // the fingerprint of the files as loaded, see `reload_if_changed`
    files_stamp: u64,
    // the message typed before stashing, see `start_stash`
    stash_input: Option<Input>,

    // the action and question shown in a popup while waiting for the user to confirm
    confirm: Option<(ConfirmAction, String)>,
    // the two sides of the conflicted file under the cursor, shown instead of its diff
    conflict: Option<ConflictView>,
    conflict_item: ConflictItem,
    // an editor or merge tool for the view to run, see `take_external_request`
    external_request: Option<ExternalRequest>,
    // a rebase, merge, cherry-pick or revert stopped and waits to be finished (or aborted)
    operation: Option<Operation>,
}

impl<'a> LocalChangesState<'a> {
    #[cfg(test)]
    pub fn load(repository: &'a Repository) -> Self {
        Self::restore(repository, None)
    }

    // Loads the page as it was in `snapshot`, loading the diff once, for the selected file.
    pub fn restore(repository: &'a Repository, snapshot: Option<LocalChangesSnapshot>) -> Self {
        let changes = repository.take_loaded_local_changes();
        let files_stamp = repository.files_stamp(&changes);
        let sections = [
            Section::new(&changes.unstaged),
            Section::new(&changes.staged),
        ];
        let section = snapshot
            .as_ref()
            .map_or(LocalChangesSection::Unstaged, |s| s.section);
        let mut state = Self {
            repository,
            changes,
            sections,
            section,
            diff_of_picked: false,
            focus: Focus::Files,
            diff: ParsedDiff::default(),
            cursor: DiffCursor::default(),
            diff_query: TreeQuery::default(),
            draft: CommitDraft::default(),
            editing_commit: false,
            needs_refresh: false,
            task: None,
            diff_stale: false,
            files_stamp,
            stash_input: None,
            confirm: None,
            conflict: None,
            conflict_item: ConflictItem::Existing,
            external_request: None,
            operation: repository.operation_in_progress(),
        };
        if let Some(snapshot) = snapshot {
            state.draft = *snapshot.commit_draft;
            if state.draft.amend_base.is_some() {
                // the staged section shows the last commit too
                state.reload_staged();
            }
            if let Some(path) = snapshot.selected_path {
                state.tree_mut().select_path(&path);
            }
        }
        // after the draft, filling the message only when it is empty, like MERGE_MSG on conflicts
        state.prepare_message();
        state.sync_edit_stop();
        state.load_diff();
        state
    }

    // What to restore of the page, after a refresh or coming back to it.
    pub fn snapshot(&self) -> LocalChangesSnapshot {
        LocalChangesSnapshot {
            section: self.section,
            selected_path: self.selected_path(),
            commit_draft: Box::new(self.draft.clone()),
        }
    }

    pub fn count(&self) -> usize {
        self.changes.count()
    }

    pub fn focus_section(&mut self, section: LocalChangesSection) {
        self.focus_files();
        if self.section != section {
            self.section = section;
            self.load_diff();
        }
    }

    pub fn selected_path(&self) -> Option<String> {
        self.selected_row().map(|row| row.path.clone())
    }

    pub fn focus_commit(&mut self) {
        self.focus_files();
        self.focus = Focus::Commit { editing: false };
    }

    pub fn is_editing(&self) -> bool {
        self.focus == Focus::Commit { editing: true }
    }

    // Handles a key typed into the commit message; Esc stops editing.
    pub fn handle_text_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.focus = Focus::Commit { editing: false };
        } else {
            self.draft.message.handle_key(key);
        }
    }

    pub fn is_commit_focused(&self) -> bool {
        matches!(self.focus, Focus::Commit { .. })
    }

    // Selecting hunks or lines in the diff.
    pub fn is_diff_focused(&self) -> bool {
        matches!(self.focus, Focus::Hunks | Focus::Lines)
    }

    pub fn is_selecting_lines(&self) -> bool {
        self.focus == Focus::Lines
    }

    // Continues the rebase when one is in progress; otherwise asks to confirm the commit (which also
    // finishes a merge, cherry-pick or revert), or starts typing the message when there is none yet.
    pub fn commit_or_continue(&mut self) -> Result<(), String> {
        if let Some(operation) = self.continuing_operation() {
            let name = operation.name();
            self.task = Some(GitTask {
                commands: self
                    .repository
                    .continue_args(operation)
                    .into_iter()
                    .collect(),
                running: format!("Continuing the {name}"),
                done: format!("Continued the {name}"),
                ..Default::default()
            });
            return Ok(());
        }
        if self.draft.message.text().trim().is_empty() {
            self.edit_message();
            return Ok(());
        }
        self.request_commit()
    }

    // Focuses the commit section and starts typing the message.
    pub fn edit_message(&mut self) {
        self.focus_commit();
        self.focus = Focus::Commit { editing: true };
    }

    // Asks to confirm the commit, showing what it will be: the branch, the message and the files.
    fn request_commit(&mut self) -> Result<(), String> {
        self.check_commit()?;
        let amend = self.draft.amend_base.is_some();
        let (action, branch) = if self.editing_commit {
            // HEAD is the commit being edited, on no branch while rebasing
            let head = self
                .repository
                .head_commit()
                .unwrap_or_else(|| "HEAD".into());
            let target = format!("{}, then go on with the rebase", head.as_short_hash());
            ("Amend".to_string(), target)
        } else {
            let branch = match self.repository.head() {
                Head::Branch { name } => name.clone(),
                _ => "detached HEAD".to_string(),
            };
            let action = match (amend, self.operation) {
                (true, _) => "Amend the last commit on".to_string(),
                (false, Some(operation)) => format!("Commit the {} on", operation.name()),
                (false, None) => "Commit to".to_string(),
            };
            (action, branch)
        };
        let message = self.draft.message.text();
        let mut lines = message.trim().lines().map(String::from);
        let subject = lines.next().unwrap_or_default();
        // the blank line after the subject is shown as a gap
        let body: Vec<String> = lines.skip_while(|l| l.trim().is_empty()).collect();
        let files = self
            .changes
            .staged
            .iter()
            .map(|f| (f.status, f.path.clone()))
            .collect();
        let preview = CommitPreview {
            action: action.clone(),
            branch: branch.clone(),
            amend,
            subject,
            body,
            files,
        };
        let prompt = format!("{action} {branch}");
        self.confirm = Some((ConfirmAction::Commit(Box::new(preview)), prompt));
        Ok(())
    }

    // Whether there is something to commit, with a message.
    fn check_commit(&self) -> Result<(), String> {
        if self.draft.message.text().trim().is_empty() {
            return Err("Commit message is empty".into());
        }
        let amend = self.draft.amend_base.is_some();
        // a merge can be committed without changes, when its resolution keeps one side
        if !amend && self.changes.staged.is_empty() && self.operation.is_none() {
            return Err("Nothing staged to commit".into());
        }
        Ok(())
    }

    // Asks for confirmation to abort the operation in progress.
    pub fn request_abort(&mut self) -> Result<(), String> {
        let Some(operation) = self.operation else {
            return Err("No rebase, merge, cherry-pick or revert in progress".into());
        };
        let prompt = format!(
            "Abort the {} and go back to where it started?",
            operation.name()
        );
        self.confirm = Some((ConfirmAction::Abort(operation), prompt));
        Ok(())
    }

    // Fills the empty message box with git's message for the merge, cherry-pick or revert in progress,
    // once, so it can still be cleared.
    fn prepare_message(&mut self) {
        let Some(operation) = self.operation else {
            // a squash merge leaves its message without a merge in progress
            match self.repository.squash_message() {
                Some(message) if !self.draft.message_prepared && self.draft.message.is_empty() => {
                    self.draft.message = TextArea::new(&message);
                    self.draft.message_prepared = true;
                }
                Some(_) => {}
                None => self.draft.message_prepared = false,
            }
            return;
        };
        if operation.continues_without_commit()
            || self.draft.message_prepared
            || !self.draft.message.is_empty()
        {
            return;
        }
        if let Some(message) = self.repository.prepared_message() {
            self.draft.message = TextArea::new(&message);
            self.draft.message_prepared = true;
        }
    }

    // The rebase that committing continues, rather than making a commit: the commit it stopped at
    // to edit is amended first.
    pub fn continuing_operation(&self) -> Option<Operation> {
        self.operation
            .filter(|o| o.continues_without_commit() && !self.editing_commit)
    }

    // Commits the staged changes with the typed message, amending HEAD while amend is checked, then
    // goes on with the rebase, cherry-pick or revert it concludes a stop of; the app runs it, see
    // `take_task`, and clears the message once the commit is made.
    pub fn commit(&mut self) -> Result<(), String> {
        self.check_commit()?;
        let amend = self.draft.amend_base.is_some();
        // the message is read from stdin so no editor is opened
        let mut commit = vec!["commit".to_string(), "--quiet".into(), "--file=-".into()];
        if amend {
            commit.push("--amend".into());
        }
        let continued = self
            .operation
            .and_then(|operation| self.repository.continue_args(operation));
        let (running, done) = if amend {
            ("Amending the last commit", "Amended the last commit")
        } else {
            ("Committing", "Committed")
        };
        self.task = Some(GitTask {
            commands: std::iter::once(commit).chain(continued).collect(),
            input: Some(self.draft.message.text()),
            running: running.into(),
            done: done.into(),
            ..Default::default()
        });
        Ok(())
    }

    // The git command for the app to run in the background, like a commit.
    pub fn take_task(&mut self) -> Option<GitTask> {
        self.task.take()
    }

    // Runs the git command asked for as the app does, see `App::finish_task`, then reloads.
    #[cfg(test)]
    fn run_task(&mut self) -> Result<(), String> {
        let task = self.take_task().expect("a git command to run");
        let head = self.repository.head_commit();
        let result = crate::git::run_task(self.repository.path(), &task);
        if task.input.is_some() && self.repository.head_commit() != head {
            self.clear_draft();
        }
        self.reload();
        result
    }

    // Empties the message box and stops amending, once the commit is made.
    pub fn clear_draft(&mut self) {
        self.draft = CommitDraft::default();
    }

    // Returns true once after the repository may have changed, by a commit, stash, continue or abort.
    pub fn take_needs_refresh(&mut self) -> bool {
        std::mem::take(&mut self.needs_refresh)
    }

    // Picks or unpicks the row under the cursor, for actions on several files.
    pub fn toggle_pick(&mut self) {
        if self.is_diff_focused() || self.is_commit_focused() {
            return;
        }
        let Some(path) = self.selected_path() else {
            return;
        };
        let picked = self.picked_mut();
        if !picked.remove(&path) {
            picked.insert(path);
        }
    }

    // Clears the picked rows of the current section. Returns false when there were none.
    pub fn clear_picks(&mut self) -> bool {
        let picked = self.picked_mut();
        let had_picks = !picked.is_empty();
        picked.clear();
        had_picks
    }

    fn picked(&self) -> &BTreeSet<String> {
        &self.current().picked
    }

    fn picked_mut(&mut self) -> &mut BTreeSet<String> {
        &mut self.current_mut().picked
    }

    // The files actions apply to: those in the picked rows, or else in the row under the cursor.
    // Only files passing the filter are included.
    fn target_files(&self) -> Vec<&LocalChangeFile> {
        if self.picked().is_empty() {
            return match self.selected_row() {
                Some(row) => self.row_files(row),
                None => Vec::new(),
            };
        }
        self.files()
            .iter()
            .filter(|f| self.picked().iter().any(|p| path_contains(p, &f.path)))
            .filter(|f| self.query().keeps(&f.path))
            .collect()
    }

    // Starts typing a stash message; Enter stashes the picked files, or else all changes.
    pub fn start_stash(&mut self) -> Result<(), String> {
        if self.count() == 0 {
            return Err("No local changes to stash".into());
        }
        self.stash_input = Some(Input::default());
        Ok(())
    }

    pub fn is_typing_stash(&self) -> bool {
        self.stash_input.is_some()
    }

    // The stash prompt for the status line: (text, cursor position, hint).
    pub fn stash_status_input(&self) -> Option<(String, u16, String)> {
        let input = self.stash_input.as_ref()?;
        let prefix = "Stash message: ";
        let cursor = (prefix.len() + input.visual_cursor()) as u16;
        let target = if self.picked().is_empty() {
            "all changes".to_string()
        } else {
            picked_files_label(self.target_files().len())
        };
        let hint = format!("Enter: stash {target}  Esc: cancel");
        Some((format!("{prefix}{}", input.value()), cursor, hint))
    }

    pub fn handle_stash_key(&mut self, key: KeyEvent) -> Result<(), String> {
        let Some(input) = self.stash_input.as_mut() else {
            return Ok(());
        };
        match key.code {
            KeyCode::Esc => {
                self.stash_input = None;
                Ok(())
            }
            KeyCode::Enter => {
                let message = input.value().trim().to_string();
                self.stash_input = None;
                let result = self.stash(&message);
                self.reload();
                result
            }
            _ => {
                input.handle_event(&Event::Key(key));
                Ok(())
            }
        }
    }

    fn stash(&mut self, message: &str) -> Result<(), String> {
        let paths: Vec<&str> = if self.picked().is_empty() {
            Vec::new()
        } else {
            let targets = self.target_files();
            if targets.is_empty() {
                return Ok(());
            }
            let mut paths = file_paths(&targets);
            paths.extend(targets.iter().filter_map(|f| f.orig_path.as_deref()));
            paths
        };
        self.repository.stash_push(&paths, message)?;
        self.clear_picks();
        self.needs_refresh = true;
        Ok(())
    }

    // Asks for confirmation to discard the selected unstaged line, hunk, file or directory.
    pub fn request_discard(&mut self) {
        if matches!(self.focus, Focus::Commit { .. } | Focus::Conflict)
            || self.section != LocalChangesSection::Unstaged
        {
            return;
        }
        let Some(row) = self.selected_row() else {
            return;
        };
        let path = if !self.picked().is_empty() && !self.is_diff_focused() {
            picked_files_label(self.target_files().len())
        } else if row.is_root() {
            "all files".to_string()
        } else {
            row.path.clone()
        };
        let target = match self.focus {
            Focus::Lines => "this line of",
            Focus::Hunks => "this hunk of",
            _ => "all changes to",
        };
        self.confirm = Some((ConfirmAction::Discard, format!("Discard {target} {path}?")));
    }

    pub fn is_confirming(&self) -> bool {
        self.confirm.is_some()
    }

    #[cfg(test)]
    fn confirm_prompt(&self) -> Option<&str> {
        self.confirm.as_ref().map(|(_, prompt)| prompt.as_str())
    }

    // Enter confirms, Esc cancels; other keys are ignored.
    pub fn handle_confirm_key(&mut self, key: KeyEvent) -> Result<(), String> {
        match key.code {
            KeyCode::Enter => self.confirm(),
            KeyCode::Esc => {
                self.confirm = None;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    // Whether the popup asks to confirm a commit, which the commit key confirms too.
    pub fn is_confirming_commit(&self) -> bool {
        matches!(self.confirm, Some((ConfirmAction::Commit(_), _)))
    }

    // Runs the action waiting for confirmation.
    pub fn confirm(&mut self) -> Result<(), String> {
        let Some((action, _)) = self.confirm.take() else {
            return Ok(());
        };
        let result = match action {
            ConfirmAction::Discard => {
                let picked_files = !self.is_diff_focused();
                let result = self.discard();
                if picked_files && result.is_ok() {
                    self.clear_picks();
                }
                result
            }
            ConfirmAction::Abort(operation) => {
                let name = operation.name();
                self.task = Some(GitTask {
                    commands: vec![vec![name.to_string(), "--abort".into()]],
                    running: format!("Aborting the {name}"),
                    done: format!("Aborted the {name}"),
                    ..Default::default()
                });
                return Ok(());
            }
            // reloads itself
            ConfirmAction::Commit(_) => return self.commit(),
        };
        self.reload();
        result
    }

    // Unstaged changes are between the index and the working tree, so discarding reverts the working tree.
    fn discard(&self) -> Result<(), String> {
        if self.is_diff_focused() {
            // the other changed lines stay in the working tree, like when unstaging from the index
            let patch = if self.focus == Focus::Lines {
                self.cursor.line_patch(&self.diff, true)
            } else {
                self.cursor.selected_hunk(&self.diff).map(DiffHunk::patch)
            };
            return match patch {
                Some(patch) => self.repository.apply_patch_to_worktree(&patch, true),
                None => Ok(()),
            };
        }

        let files = self.target_files();
        let with_status = |status: Option<LocalChangeStatus>| -> Vec<&str> {
            files
                .iter()
                .filter(|f| match status {
                    Some(status) => f.status == status,
                    None => !matches!(
                        f.status,
                        LocalChangeStatus::Untracked | LocalChangeStatus::Conflicted
                    ),
                })
                .map(|f| f.path.as_str())
                .collect()
        };
        self.repository.discard_paths(
            &with_status(None),
            &with_status(Some(LocalChangeStatus::Untracked)),
            &with_status(Some(LocalChangeStatus::Conflicted)),
        )
    }

    // Turning amend on shows the changes of HEAD in the staged section, combined with what is staged,
    // and the HEAD message in place of the typed one. Turning it off restores both.
    pub fn toggle_amend(&mut self) -> Result<(), String> {
        if self.editing_commit {
            return Err("The commit being edited is amended; C aborts the rebase".into());
        }
        if let Some(operation) = self.operation.filter(|_| self.draft.amend_base.is_none()) {
            // HEAD is the commit before the operation, amending it would mix the two up
            return Err(format!("Cannot amend during a {}", operation.name()));
        }
        if self.draft.amend_base.is_some() {
            self.draft.stop_amend();
        } else {
            let base = self.repository.amend_base()?;
            let message = self.repository.head_commit_message()?;
            self.draft.start_amend(base, &message);
        }
        self.reload();
        Ok(())
    }

    // Turns amending on, if it isn't, focusing the commit section with the message of the commit.
    pub fn start_amend(&mut self) -> Result<(), String> {
        if self.draft.amend_base.is_none() {
            self.toggle_amend()?;
        }
        self.focus_commit();
        Ok(())
    }

    // Loads the changes again, after amending turned on or off changed the staged section.
    fn reload_staged(&mut self) {
        self.changes = self.load_changes();
        let tree = file_tree(&self.changes.staged);
        self.sections[1].tree.set_tree(tree);
    }

    // A rebase stopped to edit a commit turns amending on, with the message of that commit, and
    // amending goes off again once the rebase goes on or is aborted.
    fn sync_edit_stop(&mut self) {
        self.editing_commit =
            self.operation == Some(Operation::Rebase) && self.repository.rebase_stopped_to_edit();
        let changed = if self.editing_commit && self.draft.amend_base.is_none() {
            match (
                self.repository.amend_base(),
                self.repository.head_commit_message(),
            ) {
                (Ok(base), Ok(message)) => {
                    self.draft.start_amend(base, &message);
                    self.draft.amend_for_edit = true;
                    true
                }
                _ => false,
            }
        } else if !self.editing_commit && self.draft.amend_for_edit {
            self.draft.stop_amend();
            true
        } else {
            false
        };
        if changed {
            self.reload_staged();
        }
    }

    // Asks the view to open the file under the cursor in the editor.
    pub fn request_edit(&mut self) -> Result<(), String> {
        if self.is_commit_focused() {
            self.edit_message();
            return Ok(());
        }
        let Some(row) = self.selected_row() else {
            return Ok(());
        };
        let Some(file) = row.file_index.and_then(|i| self.files().get(i)) else {
            return Err("Select a file to edit".into());
        };
        let path = self.repository.path().join(&file.path);
        if !path.exists() {
            return Err(format!("{} does not exist in the working tree", file.path));
        }
        self.external_request = Some(ExternalRequest::Editor(path));
        Ok(())
    }

    // The keys of what has the focus, for the status bar; not the ones moving around.
    pub fn key_hints(&self) -> Vec<KeyHint> {
        use UserEvent::*;
        let h =
            |events: &[UserEvent], description: &str| (events.to_vec(), description.to_string());
        if let Some((action, _)) = &self.confirm {
            return match action {
                ConfirmAction::Commit(preview) => {
                    let verb = if preview.amend { "amend" } else { "commit" };
                    vec![h(&[Confirm, Commit], verb), h(&[Cancel], "cancel")]
                }
                ConfirmAction::Abort(_) => vec![h(&[Confirm], "abort"), h(&[Cancel], "cancel")],
                ConfirmAction::Discard => {
                    vec![h(&[Confirm], "discard"), h(&[Cancel], "cancel")]
                }
            };
        }
        if self.is_typing_stash() {
            return vec![h(&[Confirm], "stash"), h(&[Cancel], "cancel")];
        }
        if self.is_typing_query() {
            return crate::view::typing_hints(false);
        }
        let unstaged = self.section == LocalChangesSection::Unstaged;
        let stage = if unstaged { "stage" } else { "unstage" };
        let commit = if self.editing_commit {
            "amend"
        } else if self
            .operation
            .is_some_and(Operation::continues_without_commit)
        {
            "continue"
        } else if self.draft.amend_base.is_some() {
            "amend"
        } else {
            "commit"
        };
        // amending is not available while an operation is in progress, aborting is
        let amend_or_abort = match (self.operation, self.draft.amend_base.is_some()) {
            (Some(_), _) => h(&[Abort], "abort"),
            (None, true) => h(&[ToggleAmend], "stop amending"),
            (None, false) => h(&[ToggleAmend], "amend"),
        };
        let mut hints = match self.focus {
            Focus::Commit { editing: true } => return vec![h(&[Cancel], "stop typing")],
            Focus::Commit { editing: false } => vec![
                h(&[ToggleStage, Edit], "type"),
                h(&[Confirm, Commit], commit),
                amend_or_abort,
                h(&[Cancel], "back"),
            ],
            Focus::Conflict => vec![h(&[Confirm], "choose"), h(&[Cancel], "back")],
            Focus::Hunks | Focus::Lines => {
                let (deeper, what) = if self.focus == Focus::Hunks {
                    (vec![h(&[Confirm], "lines")], "hunk")
                } else {
                    (vec![], "line")
                };
                let mut hints = vec![h(&[ToggleStage], &format!("{stage} {what}"))];
                if unstaged {
                    hints.push(h(&[Discard], "discard"));
                }
                hints.extend(deeper);
                hints.extend([
                    h(&[ShortCopy], &format!("copy {what}")),
                    h(&[Search], "search"),
                    h(&[Cancel], "back"),
                ]);
                hints
            }
            Focus::Files => {
                let mut hints = vec![h(&[ToggleStage], stage)];
                if self.conflict.is_some() && self.picked().is_empty() {
                    hints.push(h(&[Confirm], "resolve"));
                } else {
                    hints.push(h(&[Confirm], "hunks"));
                }
                hints.push(h(&[ToggleSelect], "pick"));
                if unstaged {
                    hints.push(h(&[Discard], "discard"));
                }
                hints.extend([
                    h(&[Commit], commit),
                    amend_or_abort,
                    h(&[Stash], "stash"),
                    h(&[Edit], "edit"),
                    h(&[Search], "search"),
                    h(&[Filter], "filter"),
                    h(&[ShortCopy, FullCopy], "copy path"),
                    h(&[Ignore], "ignore"),
                ]);
                if !self.picked().is_empty() {
                    hints.insert(0, h(&[Cancel], "clear picks"));
                }
                hints
            }
        };
        hints.retain(|(events, _)| !events.is_empty());
        hints
    }

    pub fn repository(&self) -> &'a Repository {
        self.repository
    }

    // Adds the selected file or directory to .gitignore; returns what to tell.
    pub fn ignore_selected(&mut self) -> Result<String, String> {
        let Some(row) = self.selected_row() else {
            return Ok(String::new());
        };
        let message = self
            .repository
            .ignore_path(&row.path, row.file_index.is_none())?;
        self.reload();
        Ok(message)
    }

    pub fn toplevel(&self) -> &std::path::Path {
        self.repository.path()
    }

    pub fn take_external_request(&mut self) -> Option<ExternalRequest> {
        self.external_request.take()
    }

    pub fn is_conflict_focused(&self) -> bool {
        self.focus == Focus::Conflict
    }

    // Moves between the sides (side by side) and the tools below them.
    pub fn move_conflict_item(&mut self, direction: Direction) {
        use ConflictItem::*;
        self.conflict_item = match (self.conflict_item, direction) {
            (Existing, Direction::Right) => Incoming,
            (Incoming, Direction::Left) => Existing,
            (Existing | Incoming, Direction::Down) => MergeTool,
            (MergeTool, Direction::Down) => Editor,
            (Editor, Direction::Up) => MergeTool,
            (MergeTool, Direction::Up) => Existing,
            (item, _) => item,
        };
    }

    fn activate_conflict_item(&mut self) -> Result<(), String> {
        let Some(path) = self.selected_path() else {
            return Ok(());
        };
        let side = match self.conflict_item {
            ConflictItem::Existing => ConflictSide::Ours,
            ConflictItem::Incoming => ConflictSide::Theirs,
            ConflictItem::MergeTool => {
                self.external_request = Some(ExternalRequest::MergeTool(path));
                return Ok(());
            }
            ConflictItem::Editor => {
                self.external_request =
                    Some(ExternalRequest::Editor(self.repository.path().join(path)));
                return Ok(());
            }
        };
        let result = self.repository.resolve_conflict(&path, side);
        self.focus = Focus::Files;
        self.reload();
        result
    }

    // Goes one level deeper: files -> hunks -> lines of the selected hunk.
    // Does nothing when the diff has no hunk to select.
    // In the commit section, starts editing the message; in the conflict panel, runs the choice.
    pub fn enter(&mut self) -> Result<(), String> {
        match self.focus {
            Focus::Commit { .. } => self.commit_or_continue()?,
            Focus::Conflict => self.activate_conflict_item()?,
            Focus::Files if self.conflict.is_some() && self.picked().is_empty() => {
                // a conflicted file is resolved from the conflict panel instead of its diff
                self.focus = Focus::Conflict;
                self.conflict_item = ConflictItem::Existing;
            }
            Focus::Files => {
                // with picked rows, the diff shows all their files to stage hunks and lines from
                if !self.picked().is_empty() {
                    self.diff_of_picked = true;
                    self.load_diff();
                }
                if self.cursor.select_hunk(&self.diff, 0) {
                    self.focus = Focus::Hunks;
                } else if self.diff.converted {
                    return Err(CONVERTED_DIFF.into());
                }
            }
            Focus::Hunks => {
                if self.cursor.enter_lines(&self.diff) {
                    self.focus = Focus::Lines;
                }
            }
            Focus::Lines => {}
        }
        Ok(())
    }

    // Goes one level back: lines -> hunks -> files, or from the commit section or the conflict
    // panel to the files. Returns false when already at the files.
    pub fn back(&mut self) -> bool {
        match self.focus {
            Focus::Commit { editing: true } => self.focus = Focus::Commit { editing: false },
            Focus::Commit { editing: false } | Focus::Conflict => self.focus = Focus::Files,
            Focus::Lines => {
                self.focus = Focus::Hunks;
                // the cursor may have moved into another hunk
                self.cursor.select_hunk(&self.diff, self.cursor.hunk);
            }
            Focus::Hunks => self.focus_files(),
            Focus::Files => return false,
        }
        true
    }

    // Moves the focus to the panel that way: the files are on the left, unstaged over staged, the
    // diff (or conflict panel) on the right over the commit message. Returns false when there is
    // none, or nothing to focus in it, like a diff without hunks.
    pub fn move_focus(&mut self, direction: Direction) -> Result<bool, String> {
        use LocalChangesSection::*;
        match (self.focus, direction) {
            (Focus::Files, Direction::Down) if self.section == Unstaged => {
                self.focus_section(Staged)
            }
            (Focus::Files, Direction::Up) if self.section == Staged => self.focus_section(Unstaged),
            (Focus::Files, Direction::Right) => {
                self.enter()?;
                return Ok(self.focus != Focus::Files);
            }
            (Focus::Hunks | Focus::Lines | Focus::Conflict, Direction::Down) => self.focus_commit(),
            (
                Focus::Hunks | Focus::Lines | Focus::Conflict | Focus::Commit { .. },
                Direction::Left,
            ) => self.focus_files(),
            // into the diff of the selected file, else its files
            (Focus::Commit { .. }, Direction::Up) => {
                self.focus_files();
                self.enter()?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    // Moves the focus to the files, whose diff shows the row under the cursor again.
    pub fn focus_files(&mut self) {
        self.focus = Focus::Files;
        if std::mem::take(&mut self.diff_of_picked) {
            self.load_diff();
        }
    }

    pub fn select_next(&mut self) {
        self.step(Step::Next, |s| s.cursor() + 1);
    }

    pub fn select_prev(&mut self) {
        self.step(Step::Prev, |s| s.cursor().saturating_sub(1));
    }

    pub fn select_first(&mut self) {
        self.step(Step::First, |_| 0);
    }

    pub fn select_last(&mut self) {
        self.step(Step::Last, |_| usize::MAX);
    }

    // Moves the selected line or hunk, else the cursor of the files to `row`.
    fn step(&mut self, step: Step, row: impl Fn(&Self) -> usize) {
        match self.focus {
            Focus::Lines => self.cursor.move_line(&self.diff, step),
            Focus::Hunks => self.cursor.move_hunk(&self.diff, step),
            _ => self.move_cursor(row(self)),
        }
    }

    pub fn scroll_diff_down(&mut self, n: usize) {
        self.cursor.scroll_down(n);
    }

    pub fn scroll_diff_up(&mut self, n: usize) {
        self.cursor.scroll_up(n);
    }

    pub fn diff_page_height(&self) -> usize {
        self.cursor.page_height()
    }

    // Stages the selected line, hunk, file or directory, or unstages it in the staged section.
    pub fn toggle_stage(&mut self) -> Result<(), String> {
        let result = match self.focus {
            Focus::Commit { .. } => {
                self.edit_message();
                return Ok(());
            }
            Focus::Conflict => return Ok(()),
            Focus::Lines => self.toggle_stage_line(),
            Focus::Hunks => self.toggle_stage_hunk(),
            Focus::Files => {
                let result = self.toggle_stage_row();
                if result.is_ok() {
                    self.clear_picks();
                }
                result
            }
        };
        // reload even on failure, since git may have changed part of the index
        self.reload();
        result
    }

    fn toggle_stage_hunk(&self) -> Result<(), String> {
        let Some(hunk) = self.cursor.selected_hunk(&self.diff) else {
            return Ok(());
        };
        let reverse = self.section == LocalChangesSection::Staged;
        self.repository.apply_patch_to_index(&hunk.patch(), reverse)
    }

    fn toggle_stage_line(&self) -> Result<(), String> {
        let reverse = self.section == LocalChangesSection::Staged;
        let Some(patch) = self.cursor.line_patch(&self.diff, reverse) else {
            return Ok(());
        };
        self.repository.apply_patch_to_index(&patch, reverse)
    }

    fn toggle_stage_row(&self) -> Result<(), String> {
        let Some(row) = self.selected_row() else {
            return Ok(());
        };
        // with picked rows or a filter, the files are given rather than the whole directory
        let targets = self.target_files();
        let mut paths = if self.picked().is_empty() && self.query().filter().is_none() {
            vec![row.path.as_str()]
        } else {
            file_paths(&targets)
        };
        if paths.is_empty() {
            return Ok(());
        }
        // a rename is only fully (un)staged together with its source path
        paths.extend(targets.iter().filter_map(|f| f.orig_path.as_deref()));
        match self.section {
            LocalChangesSection::Unstaged => self.repository.stage_paths(&paths),
            LocalChangesSection::Staged => self
                .repository
                .unstage_paths(&paths, self.draft.amend_base.as_deref()),
        }
    }

    // Reloads with `files`, loaded in the background, for changes made outside the app, leaving the
    // page as it is (scroll, focus) when nothing it shows changed.
    pub fn reload_if_changed(&mut self, files: FilesSnapshot) {
        if files.stamp != self.files_stamp
            || self.repository.operation_in_progress() != self.operation
        {
            self.reload_with(files.changes);
        }
    }

    // Reloads the changes, keeping each section's selection on the same path if it still exists,
    // otherwise on the same position, like lazygit does after staging.
    pub fn reload(&mut self) {
        self.reload_with(self.repository.local_changes());
    }

    fn reload_with(&mut self, changes: LocalChanges) {
        let selected_path = self.selected_path();

        self.files_stamp = self.repository.files_stamp(&changes);
        self.changes = self.with_amend(changes);
        self.operation = self.repository.operation_in_progress();
        self.prepare_message();
        self.sync_edit_stop();
        self.sections[0]
            .tree
            .set_tree(file_tree(&self.changes.unstaged));
        self.sections[1]
            .tree
            .set_tree(file_tree(&self.changes.staged));

        let focus = self.focus;
        let selected = self.cursor.clone();
        self.load_diff();
        if self.diff_of_picked && self.diff.hunks.is_empty() {
            // nothing left to stage from the picked files
            self.diff_of_picked = false;
            self.load_diff();
        }
        // stay in the diff while the same path (or the picked files) still has hunks left
        let same_source = self.diff_of_picked || self.selected_path() == selected_path;
        if matches!(focus, Focus::Hunks | Focus::Lines)
            && same_source
            && !self.diff.hunks.is_empty()
        {
            self.focus = Focus::Hunks;
            self.cursor.select_hunk(&self.diff, selected.hunk);
            // continue from the same position, the staged line itself is gone or became context
            if focus == Focus::Lines && self.cursor.select_line_near(&self.diff, selected.line) {
                self.focus = Focus::Lines;
            }
        }
    }

    fn load_changes(&self) -> LocalChanges {
        self.with_amend(self.repository.local_changes())
    }

    // While amending, the staged section shows the changes of HEAD too, see `amend_base`.
    fn with_amend(&self, mut changes: LocalChanges) -> LocalChanges {
        if let Some(base) = &self.draft.amend_base {
            changes.staged = self.repository.staged_changes_against(base);
        }
        changes
    }

    // The files of the current section in `row`, which pass the filter.
    fn row_files(&self, row: &FileTreeRow) -> Vec<&LocalChangeFile> {
        self.files()
            .iter()
            .filter(|f| row.contains(&f.path) && self.query().keeps(&f.path))
            .collect()
    }

    // Starts typing a search of the diff while it has the focus, else a search or filter of the
    // files, moving the focus back to them.
    pub fn start_query(&mut self, kind: QueryKind) {
        if self.is_diff_focused() {
            if kind == QueryKind::Search {
                self.diff_query.start(kind);
            }
            return;
        }
        self.focus_section(self.section);
        self.query_mut().start(kind);
    }

    pub fn is_typing_query(&self) -> bool {
        self.query().is_editing() || self.diff_query.is_editing()
    }

    pub fn query_status_input(&self) -> Option<(String, u16)> {
        self.diff_query
            .status_input()
            .or_else(|| self.query().status_input())
    }

    pub fn handle_query_key(&mut self, key: KeyEvent) {
        if self.diff_query.is_editing() {
            if self.diff_query.handle_key(key) == QueryChange::Search {
                self.search_diff(SearchDirection::Current);
            }
            return;
        }
        let change = self.query_mut().handle_key(key);
        self.apply_query_change(change);
    }

    // Clears the search, or else the filter. Returns false when there was neither.
    pub fn clear_query(&mut self) -> bool {
        let change = self.query_mut().clear();
        self.apply_query_change(change);
        change != QueryChange::None
    }

    // Clears the search of the diff while it has the focus. Returns false when there was none.
    pub fn clear_diff_query(&mut self) -> bool {
        self.is_diff_focused() && self.diff_query.clear() != QueryChange::None
    }

    pub fn search_next(&mut self, forward: bool) {
        let direction = if forward {
            SearchDirection::Next
        } else {
            SearchDirection::Previous
        };
        if self.is_diff_focused() {
            self.search_diff(direction);
        } else {
            self.search(direction);
        }
    }

    // Selects the match of the diff search: the changed line it is on, else its hunk.
    fn search_diff(&mut self, direction: SearchDirection) {
        let Some(query) = self.diff_query.search() else {
            return;
        };
        let lines = self.focus == Focus::Lines;
        if let Some(changed) = self.cursor.search(&self.diff, query, direction, lines) {
            self.focus = if changed { Focus::Lines } else { Focus::Hunks };
        }
    }

    // The selected line as in the file, or the selected hunk as a diff, to copy.
    pub fn selected_diff_text(&self) -> Option<String> {
        match self.focus {
            Focus::Lines => self.cursor.line_text(&self.diff),
            Focus::Hunks => self.cursor.selected_hunk(&self.diff).map(DiffHunk::text),
            _ => None,
        }
    }

    fn apply_query_change(&mut self, change: QueryChange) {
        match change {
            QueryChange::None => {}
            QueryChange::Search => self.search(SearchDirection::Current),
            QueryChange::Filter => {
                // only the focused section is filtered
                let filter = self.query().filter().map(String::from);
                self.update_tree(|tree| tree.set_filter(filter.as_deref()));
            }
        }
    }

    // Selects the match of the search in the current section, expanding the directories hiding it.
    fn search(&mut self, direction: SearchDirection) {
        let Some(query) = self.query().search().map(String::from) else {
            return;
        };
        self.update_tree(|tree| tree.search(&query, direction));
    }

    // Moves the cursor to the next (or previous) picked row, wrapping around.
    pub fn select_next_pick(&mut self, forward: bool) {
        if self.focus != Focus::Files {
            return;
        }
        let direction = if forward {
            SearchDirection::Next
        } else {
            SearchDirection::Previous
        };
        let tree = self.tree();
        let current = self.selected_path();
        let picked = self.picked();
        let path = find_next(&tree.all_rows(), current.as_deref(), direction, |row| {
            picked.contains(&row.path)
        });
        self.update_tree(|tree| tree.reveal(path.as_deref()));
    }

    // Applies a tree operation, reloading the diff when the selection changed.
    fn update_tree(&mut self, f: impl FnOnce(&mut FileTreeState)) {
        let before = self.selected_path();
        f(self.tree_mut());
        if self.selected_path() != before || self.diff.raw.is_empty() {
            self.load_diff();
        }
    }

    // The search and filter, for the title of `section`.
    fn query_indicator(&self, section: LocalChangesSection) -> Option<String> {
        self.query_of(section).indicator()
    }

    pub fn expand(&mut self) {
        if self.focus_on_files() {
            self.update_tree(FileTreeState::expand);
        }
    }

    // Collapses the selected directory, or moves to the parent directory when it is not open.
    pub fn collapse(&mut self) {
        if self.focus_on_files() {
            self.update_tree(FileTreeState::collapse);
        }
    }

    pub fn is_files_focused(&self) -> bool {
        self.focus == Focus::Files
    }

    // Whether the keys moving in the files apply to them: the conflict panel is beside them.
    fn focus_on_files(&self) -> bool {
        matches!(self.focus, Focus::Files | Focus::Conflict)
    }

    fn current(&self) -> &Section {
        self.section_state(self.section)
    }

    fn current_mut(&mut self) -> &mut Section {
        &mut self.sections[self.section as usize]
    }

    fn section_state(&self, section: LocalChangesSection) -> &Section {
        &self.sections[section as usize]
    }

    fn tree(&self) -> &FileTreeState {
        &self.current().tree
    }

    fn tree_mut(&mut self) -> &mut FileTreeState {
        &mut self.current_mut().tree
    }

    // The search and filter of the focused section.
    fn query(&self) -> &TreeQuery {
        &self.current().query
    }

    fn query_mut(&mut self) -> &mut TreeQuery {
        &mut self.current_mut().query
    }

    fn query_of(&self, section: LocalChangesSection) -> &TreeQuery {
        &self.section_state(section).query
    }

    #[cfg(test)]
    fn rows(&self) -> &[FileTreeRow] {
        self.tree().rows()
    }

    fn files(&self) -> &[LocalChangeFile] {
        match self.section {
            LocalChangesSection::Unstaged => &self.changes.unstaged,
            LocalChangesSection::Staged => &self.changes.staged,
        }
    }

    fn cursor(&self) -> usize {
        self.tree().cursor()
    }

    // Moves the cursor in the files; the diff of the row loads once used, see `load_stale_diff`.
    fn move_cursor(&mut self, cursor: usize) {
        let before = self.cursor();
        self.tree_mut().select_index(cursor);
        if self.cursor() != before {
            self.diff_stale = true;
        }
    }

    // Loads the diff of the selected row, when the cursor moved since it was.
    pub fn load_stale_diff(&mut self) {
        if std::mem::take(&mut self.diff_stale) {
            self.load_diff();
        }
    }

    fn selected_row(&self) -> Option<&FileTreeRow> {
        self.tree().selected()
    }

    fn load_diff(&mut self) {
        self.diff_stale = false;
        let diff = self.diff_text();
        self.set_diff(&diff);
    }

    // The diff of the picked files, else of the selected row.
    fn diff_text(&self) -> String {
        let staged = self.section == LocalChangesSection::Staged;
        let base = self.draft.amend_base.as_deref();
        if self.diff_of_picked {
            // untracked files are only shown by diffing each against /dev/null
            let targets = self.target_files();
            let (untracked, tracked): (Vec<&LocalChangeFile>, Vec<&LocalChangeFile>) = targets
                .into_iter()
                .partition(|f| f.status == LocalChangeStatus::Untracked);
            let diff =
                self.repository
                    .local_change_diff(&file_paths(&tracked), false, staged, base);
            return diff + &self.repository.untracked_diff(&file_paths(&untracked));
        }
        self.selected_row()
            .map(|row| {
                let untracked = row
                    .file_index
                    .is_some_and(|i| self.files()[i].status == LocalChangeStatus::Untracked);
                // a directory shows only the files passing the filter
                let paths = if row.is_dir() && self.query().filter().is_some() {
                    file_paths(&self.row_files(row))
                } else {
                    vec![row.path.as_str()]
                };
                self.repository
                    .local_change_diff(&paths, untracked, staged, base)
            })
            .unwrap_or_default()
    }

    // The two sides of the selected file, when it is conflicted.
    fn load_conflict(&self) -> Option<ConflictView> {
        self.selected_row()
            .and_then(|row| row.file_index)
            .and_then(|i| self.files().get(i))
            .filter(|f| f.status == LocalChangeStatus::Conflicted && !self.diff_of_picked)
            .map(|f| ConflictView::load(self.repository, &f.path))
    }

    fn set_diff(&mut self, diff: &str) {
        self.conflict = self.load_conflict();
        // the diff or conflict selected before is gone, the commit section stays
        if !self.is_commit_focused() {
            self.focus = Focus::Files;
        }
        self.diff = parse_diff(diff);
        self.diff.drop_converted(self.repository);
        self.cursor.reset();
    }
}

fn picked_files_label(count: usize) -> String {
    if count == 1 {
        "1 picked file".into()
    } else {
        format!("{count} picked files")
    }
}

fn file_paths<'f>(files: &[&'f LocalChangeFile]) -> Vec<&'f str> {
    files.iter().map(|f| f.path.as_str()).collect()
}

pub struct LocalChangesPanel<'a> {
    ctx: Rc<AppContext>,
    // false while another panel, like the refs sidebar, has the focus
    focused: bool,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl LocalChangesPanel<'_> {
    pub fn new(ctx: Rc<AppContext>, focused: bool) -> Self {
        Self {
            ctx,
            focused,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<'a> StatefulWidget for LocalChangesPanel<'a> {
    type State = LocalChangesState<'a>;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let mut layout = self.ctx.layout.borrow_mut();
        // each keeps some room
        let files_width = fit_size(
            layout.size(Panel::Files, tree_panel_width(area.width)),
            area.width,
            10,
            20,
        );
        let [files_area, right_area] =
            Layout::horizontal([Constraint::Length(files_width), Constraint::Min(0)]).areas(area);
        let commit_height = fit_size(
            layout.size(Panel::Commit, COMMIT_AREA_HEIGHT),
            right_area.height,
            3,
            3,
        );
        let [diff_area, commit_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(commit_height)])
                .areas(right_area);
        let unstaged_height = fit_size(
            layout.size(Panel::Unstaged, files_area.height / 2),
            files_area.height,
            3,
            3,
        );
        let [unstaged_area, staged_area] =
            Layout::vertical([Constraint::Length(unstaged_height), Constraint::Min(0)])
                .areas(files_area);
        layout.place(Panel::Files, files_area, true, true);
        layout.place(Panel::Unstaged, unstaged_area, false, true);
        layout.place(Panel::Commit, commit_area, false, false);
        drop(layout);

        self.render_file_section(state, LocalChangesSection::Unstaged, unstaged_area, buf);
        self.render_file_section(state, LocalChangesSection::Staged, staged_area, buf);
        if state.conflict.is_some() {
            self.render_conflict(state, diff_area, buf);
        } else {
            self.render_diff(state, diff_area, buf);
        }
        self.render_commit(state, commit_area, buf);
        if let Some((action, prompt)) = &state.confirm {
            match action {
                ConfirmAction::Commit(preview) => self.render_commit_popup(preview, area, buf),
                _ => self.render_confirm_popup(action, prompt, area, buf),
            }
        }
    }
}

// the message with 3 lines and the borders
const COMMIT_AREA_HEIGHT: u16 = 3 + 2;

impl LocalChangesPanel<'_> {
    fn render_file_section(
        &self,
        state: &LocalChangesState,
        section: LocalChangesSection,
        area: Rect,
        buf: &mut Buffer,
    ) {
        let Section { tree, picked, .. } = state.section_state(section);
        let (rows, cursor) = (tree.rows(), tree.cursor());
        let (number, name, files) = match section {
            LocalChangesSection::Unstaged => (1, "Unstaged", &state.changes.unstaged),
            LocalChangesSection::Staged => (2, "Staged", &state.changes.staged),
        };
        let active = state.section == section;
        let focused = active && self.focused && state.focus_on_files();
        let mut extras = vec![];
        if section == LocalChangesSection::Staged && state.draft.amend_base.is_some() {
            extras.push("amending".to_string());
        }
        if !picked.is_empty() {
            extras.push(format!("{} picked", picked.len()));
        }
        extras.extend(state.query_indicator(section));
        let label = format!("[{number}] {name} ({})", files.len());
        let block = panel_block(focused, &self.ctx.color_theme)
            .title(panel_title(
                &label,
                &extras.join(" · "),
                focused,
                &self.ctx.color_theme,
            ))
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        block.render(area, buf);

        if rows.is_empty() {
            let message = if files.is_empty() {
                "No changes"
            } else {
                "No matches"
            };
            Paragraph::new(Line::raw(message).fg(self.ctx.color_theme.status_input_transient_fg))
                .render(inner, buf);
            return;
        }

        // no room for a row, like in a very short terminal
        if inner.height == 0 {
            return;
        }
        // keep the cursor visible when the tree is taller than the section
        let height = inner.height as usize;
        let offset = (cursor + 1).saturating_sub(height);
        render_panel_scrollbar(
            buf,
            area,
            rows.len(),
            offset,
            focused,
            &self.ctx.color_theme,
        );

        // the search is highlighted in the section it applies to
        let search = state.query_of(section).search();
        let lines: Vec<Line> = rows
            .iter()
            .skip(offset)
            .take(height)
            .map(|row| {
                let indent = "  ".repeat(row.depth);
                match row.file_index {
                    // everything is expanded while filtering
                    None => dir_line(row, tree.is_open(&row.path), search, &self.ctx.color_theme),
                    Some(i) => {
                        let status = files[i].status;
                        let mut spans = vec![
                            Span::raw(indent),
                            Span::raw(status_symbol(status)).fg(self.status_color(status)),
                            Span::raw(" "),
                        ];
                        spans.extend(highlighted_name(&row.name, search, &self.ctx.color_theme));
                        Line::from(spans)
                    }
                }
            })
            .collect();
        Paragraph::new(lines)
            .style(Style::default().fg(self.ctx.color_theme.fg))
            .render(inner, buf);

        // picked rows, like picked commits in the commit list
        let picked_style = Style::default()
            .bg(self.ctx.color_theme.list_picked_bg)
            .fg(self.ctx.color_theme.list_picked_fg);
        for (i, row) in rows.iter().enumerate().skip(offset).take(height) {
            if picked.contains(&row.path) {
                let y = inner.y + (i - offset) as u16;
                buf.set_style(Rect::new(inner.x, y, inner.width, 1), picked_style);
            }
        }

        // only the section whose diff is displayed shows its cursor
        if active {
            let y = inner.y + (cursor - offset) as u16;
            // quieter while another panel, like the refs sidebar, has the focus
            mark_selected_row(buf, inner, y, self.focused, &self.ctx.color_theme);
        }
    }

    fn render_diff(&self, state: &mut LocalChangesState, area: Rect, buf: &mut Buffer) {
        let title = match state.selected_row() {
            _ if state.diff_of_picked => picked_files_label(state.target_files().len()),
            Some(row) if row.is_root() => "/".to_string(),
            Some(row) => row.path.clone(),
            None => String::new(),
        };
        let mode = match state.focus {
            Focus::Lines => "select lines",
            Focus::Hunks => "select hunks",
            _ => "",
        };
        let diff_focused = self.focused && state.is_diff_focused();
        let block = panel_block(diff_focused, &self.ctx.color_theme)
            .title(panel_title(
                &title,
                mode,
                diff_focused,
                &self.ctx.color_theme,
            ))
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        block.render(area, buf);

        let selected = state.selected_row().is_some();
        let count = if selected {
            diff_line_count(&state.diff)
        } else {
            1
        };
        let range = state.cursor.fit(count, inner.height as usize);
        let offset = range.start;
        let theme = &self.ctx.color_theme;
        render_panel_scrollbar(buf, area, count, offset, diff_focused, theme);

        let mut lines = if selected {
            diff_lines(&state.diff, &self.ctx.color_theme, range)
        } else {
            let message = if state.count() == 0 {
                "No local changes"
            } else {
                "No file selected"
            };
            vec![Line::raw(message).fg(self.ctx.color_theme.status_input_transient_fg)]
        };

        // the selected line or hunk, by its row in the diff
        let highlighted = match state.focus {
            Focus::Lines | Focus::Hunks => state
                .cursor
                .highlighted(&state.diff, state.focus == Focus::Lines),
            _ => 0..0,
        };
        let search = state.diff_query.search().filter(|_| selected);
        if let Some(query) = search {
            for line in lines.iter_mut() {
                highlight_match(line, query, &self.ctx.color_theme);
            }
        }
        let rows = offset..offset + lines.len();
        let (wrap, theme) = (self.ctx.wrap.get(), &self.ctx.color_theme);
        render_diff_lines(lines, rows, highlighted, wrap, theme, inner, buf);
    }

    fn render_commit(&self, state: &LocalChangesState, area: Rect, buf: &mut Buffer) {
        let theme = &self.ctx.color_theme;
        let focused = self.focused && state.is_commit_focused();
        // amending and rebasing have no controls of their own, the title shows them
        let mut title = panel_title("[3] Commit", "", focused, theme).spans;
        if state.draft.amend_base.is_some() {
            title.push(Span::raw(" · amending ").fg(theme.status_warn_fg));
        }
        if let Some(operation) = state.operation {
            let doing = match operation {
                Operation::Rebase => "rebasing",
                Operation::Merge => "merging",
                Operation::CherryPick => "cherry-picking",
                Operation::Revert => "reverting",
            };
            title.push(Span::raw(format!(" · {doing} ")).fg(theme.status_warn_fg));
        }
        let block = panel_block(focused, theme)
            .title(Line::from(title))
            .padding(Padding::horizontal(1));
        let text_area = block.inner(area);
        block.render(area, buf);
        self.render_message(state, text_area, buf);
    }

    fn render_conflict(&self, state: &mut LocalChangesState, area: Rect, buf: &mut Buffer) {
        let Some(conflict) = &state.conflict else {
            return;
        };
        let theme = &self.ctx.color_theme;
        let path = state.selected_path().unwrap_or_default();
        let block = panel_block(self.focused && state.is_conflict_focused(), theme)
            .title(Line::from(vec![
                Span::raw(format!(" {path} ")).fg(theme.fg),
                badge(" conflicted ", theme.status_error_fg, theme),
            ]))
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        block.render(area, buf);

        // the sides to pick from, side by side over half the height, then the tools with a line
        // between each, all centered vertically
        let [sides_area, _, tools_area] = Layout::vertical([
            Constraint::Percentage(50),
            Constraint::Length(1),
            Constraint::Length(3),
        ])
        .flex(Flex::Center)
        .areas(inner);
        let [ours_area, theirs_area] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .areas(sides_area);
        let (panel_focused, selected_item) = (
            self.focused && state.is_conflict_focused(),
            state.conflict_item,
        );
        let focused = |item| panel_focused && selected_item == item;

        let label = |name: &str, label: &str| {
            if label.is_empty() {
                format!(" {name} ")
            } else {
                format!(" {name} · {label} ")
            }
        };
        let sides = [
            (
                ours_area,
                ConflictItem::Existing,
                label("Existing", &conflict.ours_label),
                &conflict.ours,
            ),
            (
                theirs_area,
                ConflictItem::Incoming,
                label("Incoming", &conflict.theirs_label),
                &conflict.theirs,
            ),
        ];
        let mut height = 0;
        for (side_area, item, title, lines) in sides {
            let block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .style(Style::default().fg(if focused(item) {
                    theme.accent_fg
                } else {
                    theme.divider_fg
                }))
                .title(panel_title(title.trim(), "", focused(item), theme));
            let content_area = block.inner(side_area);
            block.render(side_area, buf);
            height = content_area.height as usize;
            let longest = conflict.ours.len().max(conflict.theirs.len());
            let range = state.cursor.fit(longest, height);
            let lines: Vec<Line> = lines
                .iter()
                .skip(range.start)
                .take(height)
                .map(|line| {
                    if line.starts_with("── conflict") {
                        Line::raw(line.clone()).fg(theme.status_input_transient_fg)
                    } else {
                        Line::raw(line.clone()).fg(theme.fg)
                    }
                })
                .collect();
            Paragraph::new(lines).render(content_area, buf);
        }
        state.cursor.height = height;

        let tool = match &self.ctx.core_config.git.merge_tool {
            Some(tool) => format!("  Edit in merge tool ({tool})  "),
            None => "  Edit in merge tool  ".to_string(),
        };
        let tools = [
            (ConflictItem::MergeTool, tool),
            (ConflictItem::Editor, "  Open in editor  ".to_string()),
        ];
        for (i, (item, text)) in tools.into_iter().enumerate() {
            let width = (text.chars().count() as u16).min(tools_area.width);
            // centered under the sides
            let x = tools_area.x + (tools_area.width - width) / 2;
            let tool_area = Rect::new(x, tools_area.y + 2 * i as u16, width, 1);
            if tool_area.y >= tools_area.bottom() {
                break;
            }
            // buttons, the focused one filled
            let style = if focused(item) {
                Style::default()
                    .bg(theme.accent_fg)
                    .fg(theme.accent_text_fg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg).bg(theme.divider_fg)
            };
            Paragraph::new(Line::styled(text, style)).render(tool_area, buf);
        }
    }

    fn render_confirm_popup(
        &self,
        action: &ConfirmAction,
        prompt: &str,
        area: Rect,
        buf: &mut Buffer,
    ) {
        let theme = &self.ctx.color_theme;
        let title = match action {
            ConfirmAction::Abort(_) => "Abort",
            _ => "Discard changes",
        };
        let lines = vec![
            Line::raw(""),
            Line::raw(prompt.to_string()).fg(theme.fg).bold(),
            Line::raw("This cannot be undone.").fg(theme.status_error_fg),
            Line::raw(""),
        ];
        let title = popup_title(format!(" {title} "), theme.status_error_fg, theme);
        render_popup(
            title,
            theme.status_error_fg,
            lines,
            POPUP_WIDTHS,
            area,
            buf,
            theme,
        );
    }

    // The commit to make: on which branch, the message, the files; like the popups of git commands.
    fn render_commit_popup(&self, preview: &CommitPreview, area: Rect, buf: &mut Buffer) {
        let theme = &self.ctx.color_theme;
        let dim = Style::default().fg(theme.status_input_transient_fg);
        let (title, color) = if preview.amend {
            ("Amend", theme.status_warn_fg)
        } else {
            ("Commit", theme.accent_fg)
        };

        let mut lines = vec![
            Line::raw(""),
            Line::from(vec![
                Span::raw(format!("{} ", preview.action))
                    .fg(theme.fg)
                    .bold(),
                Span::raw(preview.branch.clone())
                    .fg(theme.highlight_fg)
                    .bold(),
            ]),
            Line::raw(""),
        ];
        // the message beside a bar, like the selected field of a popup
        let bar = || Span::raw("┃ ").fg(color);
        lines.push(Line::from(vec![
            bar(),
            Span::raw(preview.subject.clone()).fg(theme.fg).bold(),
        ]));
        if !preview.body.is_empty() {
            lines.push(Line::from(bar()));
        }
        for line in preview.body.iter().take(COMMIT_PREVIEW_LINES) {
            lines.push(Line::from(vec![bar(), Span::styled(line.clone(), dim)]));
        }
        if preview.body.len() > COMMIT_PREVIEW_LINES {
            lines.push(Line::from(vec![bar(), Span::styled("…", dim)]));
        }
        lines.push(Line::raw(""));

        let count = preview.files.len();
        lines.push(Line::styled(
            match count {
                0 => "No staged changes".to_string(),
                1 => "1 staged file".to_string(),
                n => format!("{n} staged files"),
            },
            dim,
        ));
        for (status, path) in preview.files.iter().take(COMMIT_PREVIEW_LINES) {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::raw(status_symbol(*status))
                    .fg(self.status_color(*status))
                    .bold(),
                Span::raw(format!(" {path}")).fg(theme.fg),
            ]));
        }
        if count > COMMIT_PREVIEW_LINES {
            let more = format!("  … {} more", count - COMMIT_PREVIEW_LINES);
            lines.push(Line::styled(more, dim));
        }
        lines.push(Line::raw(""));
        let title = popup_title(format!(" {title} "), color, theme);
        render_popup(title, color, lines, POPUP_WIDTHS, area, buf, theme);
    }

    fn render_message(&self, state: &LocalChangesState, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let theme = &self.ctx.color_theme;
        if state.draft.message.is_empty() && !state.is_editing() {
            Paragraph::new(Line::raw("Commit message").fg(theme.status_input_transient_fg))
                .render(area, buf);
            return;
        }

        state
            .draft
            .message
            .render(area, buf, Style::default().fg(theme.fg), state.is_editing());
    }

    fn status_color(&self, status: LocalChangeStatus) -> Color {
        let theme = &self.ctx.color_theme;
        match status {
            LocalChangeStatus::Added | LocalChangeStatus::Untracked => {
                theme.detail_file_change_add_fg
            }
            LocalChangeStatus::Modified | LocalChangeStatus::TypeChanged => {
                theme.detail_file_change_modify_fg
            }
            LocalChangeStatus::Deleted | LocalChangeStatus::Conflicted => {
                theme.detail_file_change_delete_fg
            }
            LocalChangeStatus::Renamed | LocalChangeStatus::Copied => {
                theme.detail_file_change_move_fg
            }
        }
    }
}

// The width of the text of a popup; longer lines are cut.
// the text width of its popups
const POPUP_WIDTHS: (usize, usize) = (44, 80);

// A popup in the middle of `area`, with a rounded border of `color` and its title in it.
fn status_symbol(status: LocalChangeStatus) -> &'static str {
    match status {
        LocalChangeStatus::Added => "A",
        LocalChangeStatus::Modified => "M",
        LocalChangeStatus::Deleted => "D",
        LocalChangeStatus::Renamed => "R",
        LocalChangeStatus::Copied => "C",
        LocalChangeStatus::TypeChanged => "T",
        LocalChangeStatus::Untracked => "?",
        LocalChangeStatus::Conflicted => "U",
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{
        git::SortCommit,
        test_support::git::GitRepository,
        widget::{file_tree::ROOT_PATH, tree_query::QueryKind},
    };

    fn numbered_lines(n: usize) -> String {
        (1..=n).map(|i| format!("line {i}\n")).collect()
    }

    fn select_row(state: &mut LocalChangesState, path: &str) {
        let pos = state.rows().iter().position(|r| r.path == path).unwrap();
        state.move_cursor(pos);
        // as the next key, or drawing, does
        state.load_stale_diff();
    }

    fn paths(files: &[LocalChangeFile]) -> Vec<&str> {
        let mut paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        paths.sort_unstable();
        paths
    }

    #[test]
    fn test_diff_loads_once_the_cursor_settles() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        git.commit("init", "2024-01-01");
        for name in ["a.txt", "b.txt", "c.txt"] {
            fs::write(repo_path.join(name), format!("{name}\n"))?;
        }
        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        let shown = state.diff.raw.clone();

        // like a held key, moving over the rows loads no diff
        for path in ["a.txt", "b.txt", "c.txt"] {
            let pos = state.rows().iter().position(|r| r.path == path).unwrap();
            state.move_cursor(pos);
            assert_eq!(state.diff.raw, shown);
        }
        // the row it stops on is loaded once, before the next key or the next frame
        state.load_stale_diff();
        assert!(state.diff.raw.contains("+c.txt"));
        Ok(())
    }

    #[test]
    fn test_toggle_stage() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        fs::write(repo_path.join("a.txt"), numbered_lines(30))?;
        git.add(&["a.txt"]);
        git.commit("init", "2024-01-01");

        // two hunks far enough apart to stay separate
        let modified = numbered_lines(30)
            .replace("line 2\n", "line two\n")
            .replace("line 28\n", "line twenty-eight\n");
        fs::write(repo_path.join("a.txt"), modified)?;
        fs::create_dir(repo_path.join("dir"))?;
        fs::write(repo_path.join("dir/x.txt"), "x\n")?;
        fs::write(repo_path.join("dir/y.txt"), "y\n")?;

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);

        // stage the first hunk of a file, staying in the diff while hunks remain
        select_row(&mut state, "a.txt");
        state.enter()?;
        assert_eq!(state.diff.hunks.len(), 2);
        state.toggle_stage()?;
        assert_eq!(paths(&state.changes.staged), vec!["a.txt"]);
        assert_eq!(
            paths(&state.changes.unstaged),
            vec!["a.txt", "dir/x.txt", "dir/y.txt"]
        );
        assert!(state.is_diff_focused());
        assert_eq!(state.diff.hunks.len(), 1);

        // staging the last hunk returns to the files
        state.toggle_stage()?;
        assert_eq!(
            paths(&state.changes.unstaged),
            vec!["dir/x.txt", "dir/y.txt"]
        );
        assert!(!state.is_diff_focused());

        // stage a whole directory of untracked files
        select_row(&mut state, "dir");
        state.toggle_stage()?;
        assert!(state.changes.unstaged.is_empty());
        assert_eq!(
            paths(&state.changes.staged),
            vec!["a.txt", "dir/x.txt", "dir/y.txt"]
        );

        // unstage the second hunk of a staged file
        state.focus_section(LocalChangesSection::Staged);
        select_row(&mut state, "a.txt");
        state.enter()?;
        state.select_next();
        state.toggle_stage()?;
        assert_eq!(paths(&state.changes.unstaged), vec!["a.txt"]);
        let staged_diff = repository.local_change_diff(&["a.txt"], false, true, None);
        assert!(staged_diff.contains("+line two"));
        assert!(!staged_diff.contains("+line twenty-eight"));

        // unstage a single file from the staged directory
        state.back();
        select_row(&mut state, "dir/x.txt");
        state.toggle_stage()?;
        assert_eq!(paths(&state.changes.staged), vec!["a.txt", "dir/y.txt"]);

        // stage the hunk of an untracked file
        state.focus_section(LocalChangesSection::Unstaged);
        select_row(&mut state, "dir/x.txt");
        state.enter()?;
        state.toggle_stage()?;
        assert_eq!(
            paths(&state.changes.staged),
            vec!["a.txt", "dir/x.txt", "dir/y.txt"]
        );

        Ok(())
    }

    #[test]
    fn test_diff_search_and_copy() -> Result<(), Box<dyn std::error::Error>> {
        use ratatui::crossterm::event::KeyModifiers;
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        fs::write(repo_path.join("a.txt"), numbered_lines(30))?;
        git.add(&["a.txt"]);
        git.commit("init", "2024-01-01");
        let modified = numbered_lines(30)
            .replace("line 2\n", "line 2\nnew A\n")
            .replace("line 25\n", "line 25\nnew B\n");
        fs::write(repo_path.join("a.txt"), modified)?;

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        select_row(&mut state, "a.txt");
        state.enter()?;
        assert!(state.selected_diff_text().unwrap().contains("+new A"));

        // the search of the diff selects the line matched; the files' query is left alone
        state.start_query(QueryKind::Search);
        for c in "new".chars() {
            state.handle_query_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        state.handle_query_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(state.is_selecting_lines());
        assert_eq!(state.selected_diff_text().as_deref(), Some("new A"));
        assert_eq!(state.query().search(), None);
        state.search_next(true);
        assert_eq!(state.selected_diff_text().as_deref(), Some("new B"));
        state.search_next(true);
        assert_eq!(state.selected_diff_text().as_deref(), Some("new A"));

        // a filter can't start from the diff, esc clears the search before going back
        state.start_query(QueryKind::Filter);
        assert!(!state.is_typing_query());
        assert!(state.clear_diff_query() && !state.clear_diff_query());
        assert!(state.is_diff_focused());
        Ok(())
    }

    #[test]
    fn test_converted_diff() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        // .bin files are diffed as text converted from them
        fs::write(repo_path.join(".gitattributes"), "*.bin diff=upper\n")?;
        git_stdout(
            repo_path,
            &["config", "diff.upper.textconv", "tr a-z A-Z <"],
        );
        fs::write(repo_path.join("a.bin"), "x\n")?;
        fs::write(repo_path.join("a.txt"), "x\n")?;
        git.add(&[".gitattributes", "a.bin", "a.txt"]);
        git.commit("init", "2024-01-01");
        fs::write(repo_path.join("a.bin"), "x\ny\n")?;
        fs::write(repo_path.join("a.txt"), "x\ny\n")?;

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        // shown converted, without hunks to stage
        select_row(&mut state, "a.bin");
        assert!(state.diff.raw.contains("+Y"));
        assert_eq!(state.enter().unwrap_err(), CONVERTED_DIFF);
        assert_eq!(state.focus, Focus::Files);
        // the others as usual
        select_row(&mut state, "a.txt");
        state.enter()?;
        assert_eq!(state.focus, Focus::Hunks);
        Ok(())
    }

    #[test]
    fn test_toggle_stage_lines() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        fs::write(repo_path.join("a.txt"), numbered_lines(10))?;
        git.add(&["a.txt"]);
        git.commit("init", "2024-01-01");

        // one hunk with two added lines
        let modified = numbered_lines(10)
            .replace("line 5\n", "line 5\nnew A\n")
            .replace("line 6\n", "line 6\nnew B\n");
        fs::write(repo_path.join("a.txt"), modified)?;
        fs::write(repo_path.join("u.txt"), "u1\nu2\nu3\n")?;

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        let staged_diff = |path| repository.local_change_diff(&[path], false, true, None);
        let unstaged_diff = |path| repository.local_change_diff(&[path], false, false, None);

        // files -> hunks -> lines, starting on the first changed line
        select_row(&mut state, "a.txt");
        state.enter()?;
        assert_eq!(state.diff.hunks.len(), 1);
        state.enter()?;
        assert!((state.focus == Focus::Lines));
        assert_eq!(state.diff.rows[state.cursor.line].text, "new A");

        // stage a single line, staying in line mode on the next changed line
        state.toggle_stage()?;
        assert!(staged_diff("a.txt").contains("+new A"));
        assert!(!staged_diff("a.txt").contains("+new B"));
        assert!(unstaged_diff("a.txt").contains("+new B"));
        assert!((state.focus == Focus::Lines));
        assert_eq!(state.diff.rows[state.cursor.line].text, "new B");

        // staging the last line returns to the files
        state.toggle_stage()?;
        assert!(staged_diff("a.txt").contains("+new B"));
        assert!(!state.is_diff_focused());
        assert!(!(state.focus == Focus::Lines));

        // unstage a single line
        state.focus_section(LocalChangesSection::Staged);
        select_row(&mut state, "a.txt");
        state.enter()?;
        state.enter()?;
        state.toggle_stage()?;
        assert!(!staged_diff("a.txt").contains("+new A"));
        assert!(staged_diff("a.txt").contains("+new B"));

        // back from lines goes to hunks, then to files
        assert!(state.back());
        assert!(state.is_diff_focused() && !(state.focus == Focus::Lines));
        assert!(state.back());
        assert!(!state.is_diff_focused());
        assert!(!state.back());

        // stage one line of an untracked file
        state.focus_section(LocalChangesSection::Unstaged);
        select_row(&mut state, "u.txt");
        state.enter()?;
        state.enter()?;
        state.toggle_stage()?;
        let staged = staged_diff("u.txt");
        assert!(staged.contains("new file mode") && staged.contains("+u1"));
        assert!(!staged.contains("+u2"));

        // stage the rest of it, then unstage only its last line
        state.back();
        state.back();
        state.toggle_stage()?;
        state.focus_section(LocalChangesSection::Staged);
        select_row(&mut state, "u.txt");
        state.enter()?;
        state.enter()?;
        state.select_last();
        state.toggle_stage()?;
        let staged = staged_diff("u.txt");
        assert!(staged.contains("+u1") && staged.contains("+u2"));
        assert!(!staged.contains("+u3"));

        Ok(())
    }

    #[test]
    fn test_toggle_amend() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        fs::write(repo_path.join("a.txt"), "a\n")?;
        git.add(&["a.txt"]);
        git.commit("first", "2024-01-01");
        fs::write(repo_path.join("a.txt"), "a\nmore\n")?;
        fs::write(repo_path.join("b.txt"), "b\n")?;
        git.add(&["a.txt", "b.txt"]);
        git.commit("second\n\nbody", "2024-01-02");

        // already staged before amending
        fs::write(repo_path.join("c.txt"), "c\n")?;
        git.add(&["c.txt"]);

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        state.draft.message = TextArea::new("draft");
        assert_eq!(paths(&state.changes.staged), vec!["c.txt"]);

        // 3, down to the checkbox, enter: the last commit is combined with the staged changes
        state.toggle_amend()?;
        assert_eq!(
            paths(&state.changes.staged),
            vec!["a.txt", "b.txt", "c.txt"]
        );
        assert_eq!(state.draft.message.text(), "second\n\nbody");

        // the staged diff includes the change made by the last commit
        state.focus_section(LocalChangesSection::Staged);
        select_row(&mut state, "a.txt");
        assert!(state.diff.raw.contains("+more"));

        // unstaging while amending removes the file from the amended commit
        select_row(&mut state, "b.txt");
        state.toggle_stage()?;
        assert_eq!(paths(&state.changes.staged), vec!["a.txt", "c.txt"]);
        assert_eq!(paths(&state.changes.unstaged), vec!["b.txt"]);

        // the draft survives a refresh
        let reloaded = LocalChangesState::restore(&repository, Some(state.snapshot()));
        assert_eq!(paths(&reloaded.changes.staged), vec!["a.txt", "c.txt"]);

        // turning amend off restores the staged section and the typed message
        state.toggle_amend()?;
        assert_eq!(state.draft.message.text(), "draft");
        assert_eq!(paths(&state.changes.staged), vec!["b.txt", "c.txt"]);
        assert_eq!(state.changes.staged[0].status, LocalChangeStatus::Deleted);

        Ok(())
    }

    #[test]
    fn test_amend_root_commit() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        fs::write(repo_path.join("a.txt"), "a\n")?;
        git.add(&["a.txt"]);
        git.commit("root", "2024-01-01");

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        state.toggle_amend()?;
        assert_eq!(paths(&state.changes.staged), vec!["a.txt"]);
        assert_eq!(state.changes.staged[0].status, LocalChangeStatus::Added);
        Ok(())
    }

    fn git_stdout(repo_path: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(repo_path)
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    #[test]
    fn test_commit() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        git.commit("init", "2024-01-01");
        // committing goes through the user's git config, so do not depend on it
        for (key, value) in [
            ("user.name", "Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", "/dev/null"),
        ] {
            git_stdout(repo_path, &["config", key, value]);
        }

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);

        // nothing staged, then no message
        state.draft.message = TextArea::new("first change");
        assert!(state.commit().is_err());
        fs::write(repo_path.join("a.txt"), "a\n")?;
        git.add(&["a.txt"]);
        state.reload();
        state.draft.message = TextArea::new("  \n");
        assert!(state.commit().is_err());

        // c with an empty message starts typing it instead
        state.draft.message = TextArea::default();
        state.commit_or_continue()?;
        assert!(state.is_editing());
        state.back();
        state.back();

        // in the commit section, space and e type the message, enter asks to commit
        state.focus_commit();
        state.toggle_stage()?;
        assert!(state.is_editing());
        state.back();
        state.request_edit()?;
        assert!(state.is_editing());
        state.back();
        state.draft.message = TextArea::new("typed");
        state.enter()?;
        assert!(state.is_confirming_commit());
        state.handle_confirm_key(KeyEvent::from(KeyCode::Esc))?;
        assert!(!state.is_confirming());
        assert!(state.is_commit_focused());
        state.back();

        // c with a message asks to confirm the commit, showing it
        state.draft.message = TextArea::new("first change\n\nbody");
        state.commit_or_continue()?;
        assert!(state.is_confirming_commit());
        assert_eq!(state.confirm_prompt(), Some("Commit to master"));
        let Some((ConfirmAction::Commit(preview), _)) = &state.confirm else {
            panic!("no commit to confirm");
        };
        assert_eq!(preview.subject, "first change");
        assert_eq!(preview.body, vec!["body"]);
        assert_eq!(
            preview.files,
            vec![(LocalChangeStatus::Added, "a.txt".to_string())]
        );
        state.confirm()?;
        state.run_task()?;
        assert_eq!(
            git_stdout(repo_path, &["log", "-1", "--format=%B"]),
            "first change\n\nbody"
        );
        assert!(state.changes.staged.is_empty());
        assert!(state.draft.message.is_empty());

        // amend with another staged file, keeping the message of HEAD
        fs::write(repo_path.join("b.txt"), "b\n")?;
        git.add(&["b.txt"]);
        state.reload();
        state.toggle_amend()?;
        state.commit()?;
        state.run_task()?;
        assert_eq!(git_stdout(repo_path, &["rev-list", "--count", "HEAD"]), "2");
        assert_eq!(
            git_stdout(repo_path, &["log", "-1", "--format=%s"]),
            "first change"
        );
        assert_eq!(
            git_stdout(repo_path, &["show", "--name-only", "--format=", "HEAD"]),
            "a.txt\nb.txt"
        );
        assert!(state.draft.amend_base.is_none());

        Ok(())
    }

    #[test]
    fn test_discard() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        fs::write(repo_path.join("a.txt"), numbered_lines(30))?;
        git.add(&["a.txt"]);
        git.commit("init", "2024-01-01");

        let modified = numbered_lines(30)
            .replace("line 2\n", "line two\n")
            .replace("line 28\n", "line twenty-eight\n");
        fs::write(repo_path.join("a.txt"), &modified)?;
        fs::create_dir(repo_path.join("dir"))?;
        fs::write(repo_path.join("dir/u.txt"), "u\n")?;

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);

        // cancelling keeps the changes
        select_row(&mut state, "a.txt");
        state.enter()?;
        state.request_discard();
        assert!(state.is_confirming());
        state.handle_confirm_key(key(KeyCode::Esc))?;
        assert!(!state.is_confirming());
        assert_eq!(fs::read_to_string(repo_path.join("a.txt"))?, modified);

        // discard the first hunk
        state.request_discard();
        state.handle_confirm_key(key(KeyCode::Enter))?;
        let content = fs::read_to_string(repo_path.join("a.txt"))?;
        assert!(content.contains("line 2\n") && content.contains("line twenty-eight\n"));
        assert_eq!(state.diff.hunks.len(), 1);

        // discard only the removal of "line 28", which brings it back next to the added line
        state.enter()?;
        state.enter()?;
        state.request_discard();
        state.handle_confirm_key(key(KeyCode::Enter))?;
        let content = fs::read_to_string(repo_path.join("a.txt"))?;
        assert!(content.contains("line 28\nline twenty-eight\n"));

        // discard a directory of untracked files
        state.back();
        state.back();
        select_row(&mut state, "dir");
        state.request_discard();
        state.handle_confirm_key(key(KeyCode::Enter))?;
        assert!(!repo_path.join("dir/u.txt").exists());

        // nothing is discarded from the staged section
        git.add(&["a.txt"]);
        state.reload();
        state.focus_section(LocalChangesSection::Staged);
        state.request_discard();
        assert!(!state.is_confirming());

        Ok(())
    }

    #[test]
    fn test_root_row() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        fs::write(repo_path.join("a.txt"), "a\n")?;
        git.add(&["a.txt"]);
        git.commit("init", "2024-01-01");
        fs::write(repo_path.join("a.txt"), "a\nb\n")?;
        fs::write(repo_path.join("u.txt"), "u\n")?;
        fs::create_dir(repo_path.join("dir"))?;
        fs::write(repo_path.join("dir/x.txt"), "x\n")?;

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);

        // the root row is selected first, and shows the diff of everything
        assert!(state.selected_row().unwrap().is_root());
        assert!(state.diff.raw.contains("+b"));

        // stage all, including untracked files
        state.toggle_stage()?;
        assert!(state.changes.unstaged.is_empty());
        assert_eq!(
            paths(&state.changes.staged),
            vec!["a.txt", "dir/x.txt", "u.txt"]
        );

        // unstage all
        state.focus_section(LocalChangesSection::Staged);
        select_row(&mut state, ROOT_PATH);
        state.toggle_stage()?;
        assert!(state.changes.staged.is_empty());

        // discard all
        state.focus_section(LocalChangesSection::Unstaged);
        select_row(&mut state, ROOT_PATH);
        state.request_discard();
        assert_eq!(
            state.confirm_prompt(),
            Some("Discard all changes to all files?")
        );
        state.handle_confirm_key(key(KeyCode::Enter))?;
        assert!(state.changes.unstaged.is_empty());
        assert_eq!(fs::read_to_string(repo_path.join("a.txt"))?, "a\n");
        assert!(!repo_path.join("u.txt").exists());

        Ok(())
    }

    #[test]
    fn test_collapse() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        git.commit("init", "2024-01-01");
        fs::create_dir_all(repo_path.join("src/sub"))?;
        fs::write(repo_path.join("src/sub/a.rs"), "a\n")?;
        fs::write(repo_path.join("src/b.rs"), "b\n")?;
        fs::write(repo_path.join("README"), "r\n")?;

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        let visible = |state: &LocalChangesState| -> Vec<String> {
            state.rows().iter().map(|r| r.path.clone()).collect()
        };
        let selected = |state: &LocalChangesState| state.selected_path().unwrap();
        assert_eq!(
            visible(&state),
            vec![":/", "src", "src/sub", "src/sub/a.rs", "src/b.rs", "README"]
        );

        // left from a file goes to its directory, then collapses it
        select_row(&mut state, "src/sub/a.rs");
        state.collapse();
        assert_eq!(selected(&state), "src/sub");
        state.collapse();
        assert_eq!(
            visible(&state),
            vec![":/", "src", "src/sub", "src/b.rs", "README"]
        );

        // a top level file goes up to the root
        select_row(&mut state, "README");
        state.collapse();
        assert_eq!(selected(&state), ":/");

        // right expands again
        select_row(&mut state, "src/sub");
        state.expand();
        assert_eq!(visible(&state).len(), 6);

        // a collapsed directory stays collapsed after staging, and stages everything inside
        state.collapse();
        state.toggle_stage()?;
        assert_eq!(paths(&state.changes.staged), vec!["src/sub/a.rs"]);
        state.focus_section(LocalChangesSection::Staged);
        assert_eq!(
            visible(&state),
            vec![":/", "src", "src/sub", "src/sub/a.rs"]
        );
        state.focus_section(LocalChangesSection::Unstaged);
        assert_eq!(visible(&state), vec![":/", "src", "src/b.rs", "README"]);

        // collapsing the root hides everything
        select_row(&mut state, ":/");
        state.collapse();
        assert_eq!(visible(&state), vec![":/"]);

        Ok(())
    }

    fn type_query(state: &mut LocalChangesState, kind: QueryKind, text: &str) {
        state.start_query(kind);
        for c in text.chars() {
            state.handle_query_key(key(KeyCode::Char(c)));
        }
        state.handle_query_key(key(KeyCode::Enter));
    }

    #[test]
    fn test_search_and_filter() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        git.commit("init", "2024-01-01");
        fs::create_dir_all(repo_path.join("src/view"))?;
        fs::create_dir(repo_path.join("docs"))?;
        fs::write(repo_path.join("src/view/list.rs"), "l\n")?;
        fs::write(repo_path.join("src/app.rs"), "a\n")?;
        fs::write(repo_path.join("docs/list.md"), "d\n")?;

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        let visible = |state: &LocalChangesState| -> Vec<String> {
            state.rows().iter().map(|r| r.path.clone()).collect()
        };

        // search reveals a match in a collapsed directory
        select_row(&mut state, "src");
        state.collapse();
        type_query(&mut state, QueryKind::Search, "app");
        assert_eq!(state.selected_path().as_deref(), Some("src/app.rs"));
        assert!(state.diff.raw.contains("+a"));
        assert_eq!(
            state
                .query_indicator(LocalChangesSection::Unstaged)
                .as_deref(),
            Some("/app")
        );
        assert!(state.clear_query());

        // filter, then stage everything shown: only the matching files
        type_query(&mut state, QueryKind::Filter, "list");
        assert_eq!(
            visible(&state),
            vec![
                ":/",
                "docs",
                "docs/list.md",
                "src",
                "src/view",
                "src/view/list.rs"
            ]
        );
        select_row(&mut state, ROOT_PATH);
        state.toggle_stage()?;
        assert_eq!(
            paths(&state.changes.staged),
            vec!["docs/list.md", "src/view/list.rs"]
        );
        assert_eq!(paths(&state.changes.unstaged), vec!["src/app.rs"]);

        // nothing left matches in the unstaged section; the staged one has no filter of its own
        assert!(visible(&state).is_empty());
        state.focus_section(LocalChangesSection::Staged);
        assert_eq!(visible(&state).len(), 6);
        assert_eq!(state.query_indicator(LocalChangesSection::Staged), None);

        // n and N step through matches, wrapping around
        type_query(&mut state, QueryKind::Search, "list");
        assert_eq!(state.selected_path().as_deref(), Some("docs/list.md"));
        state.search_next(true);
        assert_eq!(state.selected_path().as_deref(), Some("src/view/list.rs"));
        state.search_next(true);
        assert_eq!(state.selected_path().as_deref(), Some("docs/list.md"));

        // esc clears the search of the staged section, the filter is the unstaged one's
        assert!(state.clear_query());
        assert!(!state.clear_query());
        state.focus_section(LocalChangesSection::Unstaged);
        assert!(visible(&state).is_empty());
        assert!(state.clear_query());
        assert!(!state.clear_query());
        assert_eq!(visible(&state), vec![":/", "src", "src/app.rs"]);

        Ok(())
    }

    #[test]
    fn test_picked_files() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        for name in ["a.txt", "b.txt", "c.txt"] {
            fs::write(repo_path.join(name), "x\n")?;
        }
        git.add(&["a.txt", "b.txt", "c.txt"]);
        git.commit("init", "2024-01-01");
        for name in ["a.txt", "b.txt", "c.txt"] {
            fs::write(repo_path.join(name), "x\nchanged\n")?;
        }
        fs::create_dir(repo_path.join("dir"))?;
        fs::write(repo_path.join("dir/u.txt"), "u\n")?;

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);

        // pick a file and a directory, then stage both at once
        select_row(&mut state, "a.txt");
        state.toggle_pick();
        select_row(&mut state, "dir");
        state.toggle_pick();
        state.toggle_stage()?;
        assert_eq!(paths(&state.changes.staged), vec!["a.txt", "dir/u.txt"]);
        assert_eq!(paths(&state.changes.unstaged), vec!["b.txt", "c.txt"]);
        assert!(state.picked().is_empty());

        // enter shows the diff of the picked files, to stage their hunks one by one
        select_row(&mut state, "b.txt");
        state.toggle_pick();
        select_row(&mut state, "c.txt");
        state.toggle_pick();
        state.enter()?;
        assert!(state.is_diff_focused() && state.diff_of_picked);
        assert_eq!(state.diff.hunks.len(), 2);
        state.toggle_stage()?;
        assert_eq!(paths(&state.changes.unstaged), vec!["c.txt"]);
        assert!(state.is_diff_focused() && state.diff_of_picked);
        state.toggle_stage()?;
        assert!(state.changes.unstaged.is_empty());
        assert!(!state.is_diff_focused() && !state.diff_of_picked);

        // discard the picked files only
        fs::write(repo_path.join("b.txt"), "x\nchanged\nagain\n")?;
        fs::write(repo_path.join("c.txt"), "x\nchanged\nagain\n")?;
        state.reload();
        state.clear_picks();
        select_row(&mut state, "b.txt");
        state.toggle_pick();
        state.request_discard();
        assert_eq!(
            state.confirm_prompt(),
            Some("Discard all changes to 1 picked file?")
        );
        state.handle_confirm_key(key(KeyCode::Enter))?;
        assert_eq!(paths(&state.changes.unstaged), vec!["c.txt"]);

        // unstage picked files
        state.focus_section(LocalChangesSection::Staged);
        select_row(&mut state, "a.txt");
        state.toggle_pick();
        state.toggle_stage()?;
        assert!(!paths(&state.changes.staged).contains(&"a.txt"));

        // esc: back from the diff, then the picked files, then the search and filter
        state.focus_section(LocalChangesSection::Unstaged);
        select_row(&mut state, "c.txt");
        state.toggle_pick();
        type_query(&mut state, QueryKind::Filter, "txt");
        state.enter()?;
        assert!(state.back());
        assert!(!state.back());
        assert!(state.clear_picks());
        assert!(!state.clear_picks());
        assert!(state.clear_query());
        assert!(!state.clear_query());

        Ok(())
    }

    #[test]
    fn test_stash() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        for name in ["a.txt", "b.txt"] {
            fs::write(repo_path.join(name), "x\n")?;
        }
        git.add(&["a.txt", "b.txt"]);
        git.commit("init", "2024-01-01");
        for (key, value) in [("user.name", "Test"), ("user.email", "test@example.com")] {
            git_stdout(repo_path, &["config", key, value]);
        }

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        assert!(state.start_stash().is_err());

        // stash only the picked file
        fs::write(repo_path.join("a.txt"), "a\n")?;
        fs::write(repo_path.join("b.txt"), "b\n")?;
        fs::write(repo_path.join("u.txt"), "u\n")?;
        state.reload();
        select_row(&mut state, "a.txt");
        state.toggle_pick();
        state.start_stash()?;
        let (_, _, hint) = state.stash_status_input().unwrap();
        assert_eq!(hint, "Enter: stash 1 picked file  Esc: cancel");
        for c in "only a".chars() {
            state.handle_stash_key(key(KeyCode::Char(c)))?;
        }
        state.handle_stash_key(key(KeyCode::Enter))?;
        assert!(state.take_needs_refresh());
        assert_eq!(paths(&state.changes.unstaged), vec!["b.txt", "u.txt"]);
        assert!(git_stdout(repo_path, &["stash", "list"]).contains("only a"));

        // esc cancels
        state.start_stash()?;
        state.handle_stash_key(key(KeyCode::Esc))?;
        assert!(!state.is_typing_stash());
        assert_eq!(paths(&state.changes.unstaged), vec!["b.txt", "u.txt"]);

        // without picks, everything is stashed, including untracked files
        state.start_stash()?;
        state.handle_stash_key(key(KeyCode::Enter))?;
        assert!(state.changes.unstaged.is_empty());
        assert_eq!(git_stdout(repo_path, &["stash", "list"]).lines().count(), 2);

        Ok(())
    }

    // A repository stopped on a conflict while rebasing "feature" onto "master".
    fn conflicting_rebase(repo_path: &std::path::Path) -> GitRepository<'_> {
        let git = GitRepository::new(repo_path);
        git.init();
        for (key, value) in [
            ("user.name", "Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", "/dev/null"),
        ] {
            git_stdout(repo_path, &["config", key, value]);
        }
        fs::write(repo_path.join("a.txt"), "base\n").unwrap();
        git.add(&["a.txt"]);
        git.commit("base", "2024-01-01");
        git.checkout_b("feature");
        fs::write(repo_path.join("a.txt"), "feature\n").unwrap();
        git.add(&["a.txt"]);
        git.commit("feature change", "2024-01-02");
        git.checkout("master");
        fs::write(repo_path.join("a.txt"), "master\n").unwrap();
        git.add(&["a.txt"]);
        git.commit("master change", "2024-01-03");
        git.checkout("feature");
        git_stdout(repo_path, &["rebase", "master"]);
        git
    }

    #[test]
    fn test_rebase_continue() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let _git = conflicting_rebase(repo_path);

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        assert_eq!(state.operation, Some(Operation::Rebase));
        assert_eq!(
            state.changes.unstaged[0].status,
            LocalChangeStatus::Conflicted
        );

        // continuing with the conflict unresolved fails and keeps rebasing
        state.commit_or_continue()?;
        assert!(state.run_task().is_err());
        assert_eq!(state.operation, Some(Operation::Rebase));

        // resolve, stage, then c continues instead of committing
        fs::write(repo_path.join("a.txt"), "resolved\n")?;
        state.reload();
        select_row(&mut state, "a.txt");
        state.toggle_stage()?;
        state.commit_or_continue()?;
        state.run_task()?;
        assert_eq!(state.operation, None);
        assert_eq!(
            git_stdout(repo_path, &["log", "--format=%s", "-2"]),
            "feature change\nmaster change"
        );

        // abort only exists while rebasing
        assert!(state.request_abort().is_err());
        Ok(())
    }

    #[test]
    fn test_rebase_abort() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let _git = conflicting_rebase(repo_path);
        let feature_before = git_stdout(repo_path, &["rev-parse", "feature"]);

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);

        state.request_abort()?;
        assert_eq!(
            state.confirm_prompt(),
            Some("Abort the rebase and go back to where it started?")
        );
        // cancelling keeps rebasing
        state.handle_confirm_key(key(KeyCode::Esc))?;
        assert_eq!(state.operation, Some(Operation::Rebase));

        state.request_abort()?;
        state.handle_confirm_key(key(KeyCode::Enter))?;
        state.run_task()?;
        assert_eq!(state.operation, None);
        assert!(state.changes.unstaged.is_empty());
        assert_eq!(
            git_stdout(repo_path, &["rev-parse", "HEAD"]),
            feature_before
        );
        Ok(())
    }

    // A repository with "feature" and "master" changing a.txt differently, on master.
    fn diverged_branches(repo_path: &std::path::Path) -> GitRepository<'_> {
        let git = GitRepository::new(repo_path);
        git.init();
        for (key, value) in [
            ("user.name", "Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", "/dev/null"),
        ] {
            git_stdout(repo_path, &["config", key, value]);
        }
        fs::write(repo_path.join("a.txt"), "base\n").unwrap();
        git.add(&["a.txt"]);
        git.commit("base", "2024-01-01");
        git.checkout_b("feature");
        fs::write(repo_path.join("a.txt"), "feature\n").unwrap();
        git.add(&["a.txt"]);
        git.commit("feature change", "2024-01-02");
        fs::write(repo_path.join("b.txt"), "b\n").unwrap();
        git.add(&["b.txt"]);
        git.commit("feature second", "2024-01-03");
        git.checkout("master");
        fs::write(repo_path.join("a.txt"), "master\n").unwrap();
        git.add(&["a.txt"]);
        git.commit("master change", "2024-01-04");
        git
    }

    fn resolve_and_stage(state: &mut LocalChangesState, repo_path: &std::path::Path) {
        fs::write(repo_path.join("a.txt"), "resolved\n").unwrap();
        state.reload();
        select_row(state, "a.txt");
        state.toggle_stage().unwrap();
    }

    #[test]
    fn test_reload_if_changed() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        fs::write(repo_path.join("a.txt"), numbered_lines(30))?;
        git.add(&["a.txt"]);
        git.commit("init", "2024-01-01");
        fs::write(
            repo_path.join("a.txt"),
            numbered_lines(30).replace("line 2\n", "two\n"),
        )?;

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        select_row(&mut state, "a.txt");
        state.scroll_diff_down(3);
        // as loaded in the background
        let files = || FilesSnapshot::load(repo_path, repository.git_dir());

        // nothing changed: the diff stays scrolled
        state.reload_if_changed(files());
        assert_eq!(state.cursor.offset, 3);

        // the file changed, still modified: the diff is loaded again
        fs::write(
            repo_path.join("a.txt"),
            numbered_lines(30).replace("line 3\n", "three\n"),
        )?;
        state.reload_if_changed(files());
        assert_eq!(state.cursor.offset, 0);
        assert!(state.diff.raw.contains("+three"));
        Ok(())
    }

    #[test]
    fn test_discard_conflicted() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let _git = diverged_branches(repo_path);
        fs::write(repo_path.join("c.txt"), "c\n")?;
        git_stdout(repo_path, &["add", "c.txt"]);
        git_stdout(repo_path, &["commit", "-m", "c"]);
        git_stdout(repo_path, &["merge", "feature"]);
        fs::write(repo_path.join("c.txt"), "changed\n")?;

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        // all files: the conflicted a.txt goes back to HEAD, the others are discarded too
        state.request_discard();
        assert_eq!(
            state.confirm_prompt(),
            Some("Discard all changes to all files?")
        );
        state.handle_confirm_key(key(KeyCode::Enter))?;
        assert_eq!(fs::read_to_string(repo_path.join("a.txt"))?, "master\n");
        assert_eq!(fs::read_to_string(repo_path.join("c.txt"))?, "c\n");
        assert!(state.changes.unstaged.is_empty());
        Ok(())
    }

    #[test]
    fn test_merge_conflict() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let _git = diverged_branches(repo_path);
        git_stdout(repo_path, &["merge", "feature"]);

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        assert_eq!(state.operation, Some(Operation::Merge));
        // git's message, without its comment lines
        assert_eq!(state.draft.message.text(), "Merge branch 'feature'");
        assert!(state.toggle_amend().is_err());
        // also when the page opens from a snapshot; once cleared, it stays so
        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let restored = LocalChangesState::restore(&repository, Some(Default::default()));
        assert_eq!(restored.draft.message.text(), "Merge branch 'feature'");
        state.draft.message = TextArea::default();
        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let restored = LocalChangesState::restore(&repository, Some(state.snapshot()));
        assert_eq!(restored.draft.message.text(), "");
        state.draft.message = TextArea::new("Merge branch 'feature'");

        resolve_and_stage(&mut state, repo_path);
        state.commit_or_continue()?;
        state.confirm()?;
        state.run_task()?;
        assert_eq!(state.operation, None);
        // a merge commit, with both parents
        let parents = git_stdout(repo_path, &["log", "-1", "--format=%p"]);
        assert_eq!(parents.split(' ').count(), 2);
        assert!(git_stdout(repo_path, &["log", "-1", "--format=%s"]).starts_with("Merge branch"));

        // a new merge, aborted
        git_stdout(repo_path, &["reset", "--hard", "HEAD^"]);
        git_stdout(repo_path, &["merge", "feature"]);
        let before = git_stdout(repo_path, &["rev-parse", "HEAD"]);
        state.reload();
        state.request_abort()?;
        assert_eq!(
            state.confirm_prompt(),
            Some("Abort the merge and go back to where it started?")
        );
        state.handle_confirm_key(key(KeyCode::Enter))?;
        state.run_task()?;
        assert_eq!(state.operation, None);
        assert_eq!(git_stdout(repo_path, &["rev-parse", "HEAD"]), before);
        Ok(())
    }

    #[test]
    fn test_cherry_pick_sequence() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let _git = diverged_branches(repo_path);
        // the first commit conflicts, the second follows once it is committed
        git_stdout(repo_path, &["cherry-pick", "feature~1", "feature"]);

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        assert_eq!(state.operation, Some(Operation::CherryPick));
        assert_eq!(state.draft.message.text(), "feature change");

        resolve_and_stage(&mut state, repo_path);
        state.commit_or_continue()?;
        state.confirm()?;
        state.run_task()?;
        assert_eq!(state.operation, None);
        assert_eq!(
            git_stdout(repo_path, &["log", "--format=%s", "-3"]),
            "feature second\nfeature change\nmaster change"
        );
        Ok(())
    }

    #[test]
    fn test_revert_abort() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = diverged_branches(repo_path);
        // reverting the base change of a.txt conflicts with master's change
        git.checkout("feature");
        fs::write(repo_path.join("a.txt"), "feature again\n")?;
        git.add(&["a.txt"]);
        git.commit("feature again", "2024-01-05");
        git_stdout(repo_path, &["revert", "--no-edit", "HEAD~2"]);

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        assert_eq!(state.operation, Some(Operation::Revert));
        assert!(state
            .draft
            .message
            .text()
            .starts_with("Revert \"feature change\""));

        state.request_abort()?;
        state.handle_confirm_key(key(KeyCode::Enter))?;
        state.run_task()?;
        assert_eq!(state.operation, None);
        assert!(state.request_abort().is_err());
        Ok(())
    }

    #[test]
    fn test_conflict_panel() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let _git = diverged_branches(repo_path);
        git_stdout(repo_path, &["merge", "feature"]);

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        select_row(&mut state, "a.txt");

        // the conflicted file shows both sides instead of its diff
        let conflict = state.conflict.as_ref().unwrap();
        assert_eq!(
            (conflict.ours_label.as_str(), conflict.theirs_label.as_str()),
            ("HEAD", "feature")
        );
        assert_eq!(conflict.ours, ["── conflict 1 ──", "master"]);
        assert_eq!(conflict.theirs, ["── conflict 1 ──", "feature"]);

        // the tools ask the view to run them
        state.enter()?;
        assert!(state.is_conflict_focused());
        state.move_conflict_item(Direction::Down);
        state.enter()?;
        assert_eq!(
            state.take_external_request(),
            Some(ExternalRequest::MergeTool("a.txt".into()))
        );
        state.move_conflict_item(Direction::Down);
        state.enter()?;
        let toplevel = state.toplevel().to_path_buf();
        assert_eq!(
            state.take_external_request(),
            Some(ExternalRequest::Editor(toplevel.join("a.txt")))
        );

        // esc goes back to the files
        assert!(state.back());
        assert!(!state.is_conflict_focused());

        // e opens the file in the editor from the files too
        state.request_edit()?;
        assert_eq!(
            state.take_external_request(),
            Some(ExternalRequest::Editor(toplevel.join("a.txt")))
        );
        select_row(&mut state, ROOT_PATH);
        assert!(state.request_edit().is_err());
        select_row(&mut state, "a.txt");

        // picking a side resolves the file with it
        state.enter()?;
        state.move_conflict_item(Direction::Right);
        state.enter()?;
        assert_eq!(fs::read_to_string(repo_path.join("a.txt"))?, "feature\n");
        assert!(state.changes.unstaged.is_empty());
        assert!(state.conflict.is_none());
        assert_eq!(state.operation, Some(Operation::Merge));
        Ok(())
    }

    #[test]
    fn test_conflict_with_deleted_side() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = diverged_branches(repo_path);
        // master deletes the file that feature changes
        git_stdout(repo_path, &["rm", "--quiet", "a.txt"]);
        git.commit("delete", "2024-01-05");
        git_stdout(repo_path, &["merge", "feature"]);

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        select_row(&mut state, "a.txt");
        let conflict = state.conflict.as_ref().unwrap();
        assert_eq!(conflict.ours, ["(deleted)"]);
        assert_eq!(conflict.theirs, ["feature"]);

        // keeping the deleting side removes the file
        state.enter()?;
        state.enter()?;
        assert!(!repo_path.join("a.txt").exists());
        assert!(state.changes.unstaged.is_empty());
        Ok(())
    }

    #[test]
    fn test_select_next_pick() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        git.commit("init", "2024-01-01");
        fs::create_dir(repo_path.join("dir"))?;
        for name in ["a.txt", "b.txt", "dir/c.txt"] {
            fs::write(repo_path.join(name), "x\n")?;
        }

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        for path in ["dir/c.txt", "b.txt"] {
            select_row(&mut state, path);
            state.toggle_pick();
        }

        // jumping to a pick hidden in a collapsed directory expands it
        select_row(&mut state, "dir");
        state.collapse();
        select_row(&mut state, ROOT_PATH);
        state.select_next_pick(true);
        assert_eq!(state.selected_path().as_deref(), Some("dir/c.txt"));
        state.select_next_pick(true);
        assert_eq!(state.selected_path().as_deref(), Some("b.txt"));
        state.select_next_pick(true);
        assert_eq!(state.selected_path().as_deref(), Some("dir/c.txt"));
        state.select_next_pick(false);
        assert_eq!(state.selected_path().as_deref(), Some("b.txt"));
        Ok(())
    }
}
