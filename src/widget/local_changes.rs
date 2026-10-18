use std::{collections::BTreeSet, path::PathBuf, rc::Rc};

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Flex, Layout, Rect},
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Paragraph, StatefulWidget, Widget},
};

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::widget::tree_query::Toward;
use crate::{
    app::AppContext,
    color::ColorTheme,
    event::UserEvent,
    git::{
        AfterTask, CommitHash, ConflictSide, DiffOf, FilesSnapshot, GitCommand, GitTask, Head,
        LocalChangeFile, LocalChangeStatus, LocalChanges, Operation, PatchDirection, Repository,
    },
    keys::{Availability, KeyList, Keys},
    layout::{fit_size, Direction, Edge, Panel},
    search::{SearchKind, SearchOptions},
    view::register_diff_keys,
    widget::{
        conflict::ConflictView,
        diff::{parse_diff, DiffPart, Step, CONVERTED_DIFF},
        diff_pane::DiffPane,
        file_tree::{
            find_next, path_contains, render_tree, Entry, FileTree, FileTreeRow, FileTreeState,
            TreeCursor, TreeShape,
        },
        git_dialog::{CommitKind, CommitPreview, GitDialog},
        style::{
            badge, divider, mark_selected_bar, panel_title, placeholder, render_panel,
            render_panel_scrollbar, tree_panel_width,
        },
        text_area::{Caret, Lines, TextArea},
        tree_query::{QueryKind, SearchDirection, TreeQuery},
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

// Shown beside the files instead of the diff of a conflicted file: its two sides, and the choice
// selected while the panel has the focus.
#[derive(Debug)]
struct ConflictPanel {
    view: ConflictView,
    item: ConflictItem,
}

// What the state asks the app for, once the event is handled: a git command run in the
// background (like continuing a rebase), a popup confirming one (like a commit), or an editor
// or merge tool run in place of the app.
#[derive(Debug)]
pub enum Request {
    Task(GitTask),
    Dialog(GitDialog),
    External(ExternalRequest),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalRequest {
    // an absolute path
    Editor(PathBuf),
    // a path relative to the top of the working tree
    MergeTool(String),
}

// The commit section's contents, kept across refreshes.
#[derive(Debug, Default, Clone)]
pub struct CommitDraft {
    // the first line of the message, and the rest after a blank line
    summary: TextArea,
    description: TextArea,
    amend: Option<Amend>,
    // git's prepared message put in the message box for the operation in progress, see
    // `prepare_message`
    prepared_message: Option<String>,
}

// Amending the commit after `base`, the parent of HEAD: the staged section is compared to it
// instead of HEAD, so it shows the changes of the commit being amended together with what is staged.
#[derive(Debug, Clone)]
struct Amend {
    base: String,
    // HEAD when amending started: once it moves, the amend was of another commit
    head: CommitHash,
    // the message typed before amending, restored when amending is turned off
    message_before: (TextArea, TextArea),
    of: AmendOf,
}

// What is amended: the last commit, or the one a rebase stopped at to edit, amending turned off
// when the rebase goes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AmendOf {
    LastCommit,
    EditStop,
}

impl CommitDraft {
    // Amends the commit after `base`, its `message` in place of the typed one.
    fn start_amend(&mut self, base: String, head: CommitHash, message: &str, of: AmendOf) {
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
    fn stop_amend(&mut self) {
        if let Some(amend) = self.amend.take() {
            (self.summary, self.description) = amend.message_before;
        }
    }

    fn amend_base(&self) -> Option<&str> {
        self.amend.as_ref().map(|amend| amend.base.as_str())
    }

    // The summary, then the description after a blank line.
    fn message(&self) -> String {
        let summary = self.summary.text().trim().to_string();
        let description = self.description.text().trim_end().to_string();
        if description.trim().is_empty() {
            summary
        } else {
            format!("{summary}\n\n{description}")
        }
    }

    // Splits `message` into its first line and the rest, without the blank lines between.
    fn set_message(&mut self, message: &str) {
        let (summary, description) = message
            .trim()
            .split_once('\n')
            .unwrap_or((message.trim(), ""));
        self.summary = TextArea::new(summary);
        self.description = TextArea::new(description.trim_start_matches('\n'));
    }

    fn is_empty(&self) -> bool {
        self.summary.is_empty() && self.description.is_empty()
    }

    fn field_mut(&mut self, field: CommitField) -> &mut TextArea {
        match field {
            CommitField::Summary => &mut self.summary,
            CommitField::Description => &mut self.description,
        }
    }
}

// The box of the commit section the keys go to.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CommitField {
    #[default]
    Summary,
    Description,
}

// Where the keys go on the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    // the files of the current section
    Files,
    // the panel beside the files: the hunks or lines of the diff (see `DiffPane`), or the
    // choices of the conflict panel shown instead for a conflicted file
    Side,
    // the commit section, on one of its boxes; back in it, the summary is selected
    Commit(CommitField),
    // typing the message into a box of the commit section
    Typing(CommitField),
}

// What the diff beside the files shows: the row under the cursor, or all the picked files, to
// stage hunks and lines from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffSource {
    Row,
    Picked,
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
            tree: FileTreeState::new(file_tree(files), TreeShape::Rooted),
            query: TreeQuery::default(),
            picked: BTreeSet::new(),
        }
    }
}

fn file_tree(files: &[LocalChangeFile]) -> FileTree {
    FileTree::new(files.iter().map(|f| f.path.as_str()))
}

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
        self.commit_draft.message()
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
    // where the keys were sent; the diff keeps whether it has them itself, see `focus`
    chosen: Focus,
    // the diff beside the files, its selected hunk or line and scroll (or that of the conflict
    // sides), and what it is of
    pane: DiffPane,
    diff_source: DiffSource,
    // searching the diff while it has the focus
    diff_query: TreeQuery,

    // the message, and amending
    draft: CommitDraft,
    // what the view is to ask the app for once the event is handled, see `take_request`
    request: Option<Request>,
    // the cursor moved to another row, whose diff loads once used, see `move_cursor`
    diff_stale: bool,
    // the fingerprint of the files as loaded, see `reload_if_changed`
    files_stamp: u64,
    // the conflicted file under the cursor, shown instead of its diff
    conflict: Option<ConflictPanel>,
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
            chosen: Focus::Files,
            pane: DiffPane::default(),
            diff_source: DiffSource::Row,
            diff_query: TreeQuery::default(),
            draft: CommitDraft::default(),
            request: None,
            diff_stale: false,
            files_stamp,
            conflict: None,
            operation: repository.operation_in_progress(),
        };
        if let Some(snapshot) = snapshot {
            state.draft = *snapshot.commit_draft;
            if state.draft.amend.is_some() {
                // the staged section shows the last commit too
                state.reload_staged();
            }
            if let Some(path) = snapshot.selected_path {
                state.tree_mut().select_path(&path);
            }
        }
        // after the draft, filling the message only when it is empty, like MERGE_MSG on conflicts
        state.prepare_message();
        state.sync_amend();
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

    // The path of the selected file, not a directory.
    pub fn selected_file(&self) -> Option<&str> {
        match self.selected_row()?.entry()? {
            Entry::File(path) => Some(path),
            Entry::Dir(_) => None,
        }
    }

    pub fn selected_path(&self) -> Option<String> {
        self.selected_row().map(|row| row.path.clone())
    }

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
    fn editing_commit(&self) -> bool {
        self.draft
            .amend
            .as_ref()
            .is_some_and(|amend| amend.of == AmendOf::EditStop)
    }

    pub fn is_editing(&self) -> bool {
        matches!(self.focus(), Focus::Typing(_))
    }

    // The box selected in the commit section, while it has the focus.
    fn commit_field(&self) -> Option<CommitField> {
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

    // Where the keys go: beside the files, the diff has them while it selects a hunk or line, like
    // in the details, and the conflict panel once entered.
    fn focus(&self) -> Focus {
        match self.chosen {
            Focus::Files | Focus::Side if self.conflict.is_none() => {
                if self.pane.is_focused() {
                    Focus::Side
                } else {
                    Focus::Files
                }
            }
            focus => focus,
        }
    }

    // Selecting hunks or lines in the diff.
    pub fn is_diff_focused(&self) -> bool {
        self.focus() == Focus::Side && self.conflict.is_none()
    }

    #[cfg(test)]
    fn is_selecting_lines(&self) -> bool {
        self.is_diff_focused() && self.pane.is_selecting_lines()
    }

    // What `selected_diff_text` is of.
    pub fn selected_diff_part(&self) -> DiffPart {
        self.pane.selected_part()
    }

    // Continues the rebase when one is in progress; otherwise asks to confirm the commit (which also
    // finishes a merge, cherry-pick or revert), or starts typing the message when there is none yet.
    pub fn commit_or_continue(&mut self) -> Result<(), String> {
        if let Some(operation) = self.continuing_operation() {
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
            return Ok(());
        }
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
    fn request_commit(&mut self) -> Result<(), String> {
        self.check_commit()?;
        let branch = match self.repository.head() {
            Head::Branch { name } => Some(name.clone()),
            _ => None,
        };
        let kind = match (&self.draft.amend, self.operation) {
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

    // Whether there is something to commit; the message isn't empty, see `commit_or_continue`.
    fn check_commit(&self) -> Result<(), String> {
        let amend = self.draft.amend.is_some();
        // a merge can be committed without changes, when its resolution keeps one side
        if !amend && self.changes.staged.is_empty() && self.operation.is_none() {
            return Err("Nothing staged to commit".into());
        }
        Ok(())
    }

    // Asks for confirmation to abort the operation in progress; its key works only then.
    pub fn request_abort(&mut self) {
        if let Some(operation) = self.operation {
            self.request = Some(Request::Dialog(GitDialog::abort(operation)));
        }
    }

    // Fills the empty message box with git's message for the merge, cherry-pick or revert in progress,
    // once, so it can still be cleared.
    fn prepare_message(&mut self) {
        let Some(operation) = self.operation else {
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

    // The rebase that committing continues, rather than making a commit: the commit it stopped at
    // to edit is amended first.
    pub fn continuing_operation(&self) -> Option<Operation> {
        self.operation
            .filter(|o| o.continues_without_commit() && !self.editing_commit())
    }

    // Commits the staged changes with the typed message, amending HEAD while amend is checked, then
    // goes on with the rebase, cherry-pick or revert it concludes a stop of; the app runs it and
    // clears the message once the commit is made.
    fn commit_task(&self) -> Result<GitTask, String> {
        self.check_commit()?;
        let amend = self.draft.amend.is_some();
        // the message is read from stdin so no editor is opened
        let mut commit = GitCommand::Commit.args(["--quiet", "--file=-"]);
        if amend {
            commit.args.push("--amend".into());
        }
        let continued = self
            .operation
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

    // What the view is to ask the app for, once the event is handled.
    pub fn take_request(&mut self) -> Option<Request> {
        self.request.take()
    }

    #[cfg(test)]
    fn take_task(&mut self) -> Option<GitTask> {
        match self.request.take() {
            Some(Request::Task(task)) => Some(task),
            other => std::mem::replace(&mut self.request, other).and(None),
        }
    }

    #[cfg(test)]
    fn dialog(&self) -> Option<&GitDialog> {
        match &self.request {
            Some(Request::Dialog(dialog)) => Some(dialog),
            _ => None,
        }
    }

    #[cfg(test)]
    fn dialog_mut(&mut self) -> Option<&mut GitDialog> {
        match &mut self.request {
            Some(Request::Dialog(dialog)) => Some(dialog),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn take_dialog(&mut self) -> Option<GitDialog> {
        match self.request.take() {
            Some(Request::Dialog(dialog)) => Some(dialog),
            other => std::mem::replace(&mut self.request, other).and(None),
        }
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
            .filter(|f| self.tree().keeps(&f.path))
            .collect()
    }

    // Asks for a message to stash the picked files, or else all changes, with.
    pub fn start_stash(&mut self) -> Result<(), String> {
        if self.count() == 0 {
            return Err("No local changes to stash".into());
        }
        let (target, paths) = if self.picked().is_empty() {
            ("all changes".to_string(), vec![])
        } else {
            let targets = self.target_files();
            if targets.is_empty() {
                return Err("No local changes in the picked files".into());
            }
            let mut paths: Vec<String> = targets.iter().map(|f| f.path.clone()).collect();
            paths.extend(targets.iter().filter_map(|f| f.orig_path.clone()));
            (picked_files_label(targets.len()), paths)
        };
        self.request = Some(Request::Dialog(GitDialog::stash(target, paths)));
        Ok(())
    }

    // Asks for confirmation to discard the selected unstaged line, hunk, file or directory.
    pub fn request_discard(&mut self) -> Result<(), String> {
        if matches!(self.focus(), Focus::Commit(_) | Focus::Typing(_))
            || self.is_conflict_focused()
            || self.section != LocalChangesSection::Unstaged
        {
            return Ok(());
        }
        let Some(row) = self.selected_row() else {
            return Ok(());
        };
        let path = if !self.picked().is_empty() && !self.is_diff_focused() {
            picked_files_label(self.target_files().len())
        } else if row.is_root() {
            "all files".to_string()
        } else {
            row.path.clone()
        };
        let target = if self.is_diff_focused() {
            format!("this {} of", self.pane.selected_part().name())
        } else {
            "all changes to".into()
        };
        let Some(task) = self.discard_task()? else {
            return Ok(());
        };
        let dialog = GitDialog::discard(format!("Discard {target} {path}"), task);
        self.request = Some(Request::Dialog(dialog));
        Ok(())
    }

    // Unstaged changes are between the index and the working tree, so discarding reverts the
    // working tree; None when there is nothing to discard.
    fn discard_task(&self) -> Result<Option<GitTask>, String> {
        let task = |commands, input| GitTask {
            commands,
            input,
            running: "Discarding changes".into(),
            done: "Discarded changes".into(),
            // in the diff, the page stays on it
            after: if self.is_diff_focused() {
                AfterTask::KeepView
            } else {
                AfterTask::Reload
            },
            ..Default::default()
        };
        if self.is_diff_focused() {
            // the other changed lines stay in the working tree, like when unstaging from the index
            let patch = self.pane.selected_patch(PatchDirection::Reverse);
            let apply = GitCommand::Apply.args(["--reverse", "-"]);
            return Ok(patch.map(|patch| task(vec![apply], Some(patch))));
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
        let commands = self.repository.discard_commands(
            &with_status(None),
            &with_status(Some(LocalChangeStatus::Untracked)),
            &with_status(Some(LocalChangeStatus::Conflicted)),
        )?;
        Ok((!commands.is_empty()).then(|| task(commands, None)))
    }

    // Turning amend on shows the changes of HEAD in the staged section, combined with what is staged,
    // and the HEAD message in place of the typed one. Turning it off restores both.
    pub fn toggle_amend(&mut self) -> Result<(), String> {
        if self.editing_commit() {
            return Err("The commit being edited is amended; C aborts the rebase".into());
        }
        if let Some(operation) = self.operation.filter(|_| self.draft.amend.is_none()) {
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
    fn reload_staged(&mut self) {
        self.changes = self.with_amend(self.repository.local_changes());
        let tree = file_tree(&self.changes.staged);
        self.sections[1].tree.set_tree(tree);
    }

    // A rebase stopped to edit a commit turns amending on, with the message of that commit, and
    // amending goes off again once the rebase goes on or is aborted, or once HEAD moves, by a
    // checkout or a commit made outside the app.
    fn sync_amend(&mut self) {
        let head = self.repository.head_commit();
        let moved = self
            .draft
            .amend
            .as_ref()
            .is_some_and(|amend| Some(&amend.head) != head.as_ref());
        if moved {
            self.draft.stop_amend();
        }
        let stopped =
            self.operation == Some(Operation::Rebase) && self.repository.rebase_stopped_to_edit();
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

    // Asks the view to open the file under the cursor in the editor.
    pub fn request_edit(&mut self) -> Result<(), String> {
        if self.is_commit_focused() {
            self.edit_message();
            return Ok(());
        }
        let Some(row) = self.selected_row() else {
            return Ok(());
        };
        let Some(file) = row.file_index().and_then(|i| self.files().get(i)) else {
            return Err("Select a file to edit".into());
        };
        let path = self.repository.path().join(&file.path);
        if !path.exists() {
            return Err(format!("{} does not exist in the working tree", file.path));
        }
        self.request = Some(Request::External(ExternalRequest::Editor(path)));
        Ok(())
    }

    // The keys of the local changes page, see `Keys`.
    pub fn register_keys(k: &mut Keys<Self>) {
        use UserEvent::*;
        let files = |v: &Self| v.focus() == Focus::Files;
        let diff = |v: &Self| v.is_diff_focused();
        let commit = |v: &Self| matches!(v.focus(), Focus::Commit(_));
        let conflict = |v: &Self| v.is_conflict_focused();
        let unstaged = |v: &Self| v.section == LocalChangesSection::Unstaged;
        let empty = |v: &Self| v.files().is_empty();
        let picks = |v: &Self| !v.picked().is_empty();
        // a row of the files, or a file, selected
        let row = |v: &Self| v.focus() == Focus::Files && v.selected_row().is_some();
        let file = |v: &Self| v.focus() == Focus::Files && v.selected_file().is_some();
        // a file or directory, not the root row standing for all of them
        let path = |v: &Self| row(v) && !v.selected_row().is_some_and(FileTreeRow::is_root);
        // something to commit: staged changes, amending, or an operation to conclude
        let committable = |v: &Self| {
            !v.changes.staged.is_empty() || v.draft.amend.is_some() || v.operation.is_some()
        };
        let stage = |v: &Self| if unstaged(v) { "stage" } else { "unstage" };
        let what = |v: &Self| v.pane.selected_part().name();
        // committing, or amending, or continuing an operation
        let commit_word = |v: &Self| {
            if v.editing_commit() || v.draft.amend.is_some() {
                "amend"
            } else if v.operation.is_some_and(Operation::continues_without_commit) {
                "continue"
            } else {
                "commit"
            }
            .to_string()
        };

        k.add(
            &[Cancel],
            "clear picks",
            "Back, then clear the picked files, the search, the filter",
        )
        .when(|v| files(v) && picks(v));
        k.add(
            &[ToggleStage],
            "stage",
            "Stage/unstage the file, directory, hunk or line, or the picked files",
        )
        .when(row)
        .short(|v| stage(v).into());
        k.add(
            &[ToggleStage],
            "stage",
            "Stage/unstage the file, directory, hunk or line, or the picked files",
        )
        .when(diff)
        .short(|v| format!("{} {}", stage(v), what(v)));
        k.add(
            &[Confirm],
            "select hunks",
            "Select the hunks of the diff, then their lines; resolve a conflict",
        )
        .when(|v| files(v) && !empty(v))
        .short(|v| {
            let resolve = v.conflict.is_some() && !picks(v);
            if resolve { "resolve" } else { "select hunks" }.into()
        });
        k.add(
            &[Confirm],
            "unstaged",
            "On the empty staged files, go to the unstaged ones",
        )
        .when(|v| files(v) && empty(v) && !unstaged(v));
        register_diff_keys(k, |v| v.is_diff_focused().then_some(&v.pane));
        k.add(
            &[Confirm],
            "choose",
            "Choose the side or the tool of the conflict",
        )
        .when(conflict);
        k.add(&[ToggleSelect], "pick", "Pick/unpick the file or directory")
            .when(row);
        k.add(
            &[Discard],
            "discard",
            "Discard the unstaged file, directory, hunk or line",
        )
        .when(|v| (row(v) || diff(v)) && unstaged(v));
        // on the empty unstaged files enter commits too, see `enter`; with nothing to commit they
        // say so, unlisted
        let commits = move |v: &Self| commit(v) || (files(v) && empty(v) && unstaged(v));
        const COMMIT: &str =
            "Commit after confirming (finishing a merge, cherry-pick or revert), or continue a rebase";
        let availability = move |applies: bool, v: &Self| match (applies, committable(v)) {
            (false, _) => Availability::Off,
            (true, false) => Availability::Warns,
            (true, true) => Availability::Works,
        };
        k.add_where(&[Confirm], "commit", COMMIT, move |v| {
            availability(commits(v), v)
        })
        .short(commit_word);
        let not_typing = |v: &Self| !v.is_editing();
        k.add_where(&[Commit], "commit", COMMIT, move |v| {
            availability(not_typing(v), v)
        })
        .short(commit_word);
        k.add(
            &[Abort],
            "abort",
            "Abort the rebase, merge, cherry-pick or revert in progress",
        )
        .when(|v| v.operation.is_some());
        k.add(
            &[ToggleAmend],
            "amend",
            "Amend the last commit, or stop amending",
        )
        // during an operation only stopping, see `toggle_amend`; a commit being edited stays amended
        .when(|v| !v.editing_commit() && (v.operation.is_none() || v.draft.amend.is_some()))
        .short(|v| {
            if v.draft.amend.is_some() {
                "stop amending"
            } else {
                "amend"
            }
            .into()
        });
        k.add(
            &[ToggleStage, Edit],
            "type",
            "Type the message, in the commit section",
        )
        .when(commit);
        k.add(&[Stash], "stash", "Stash all changes, or the picked files")
            .when(|v| files(v) && v.count() > 0);
        k.add(&[Edit], "edit", "Open the file in $VISUAL or $EDITOR")
            .when(file);
        k.add(
            &[Blame],
            "blame",
            "Blame the selected file as of the last commit",
        )
        .when(file);
        k.add(
            &[Timeline],
            "timeline",
            "Show the commits changing the selected file",
        )
        .when(file);
        k.add(
            &[Search],
            "search",
            "Search the files, or the diff while it has the focus",
        )
        .when(|v| (files(v) && !empty(v)) || diff(v));
        k.add(&[Filter], "filter", "Filter the files")
            .when(|v| files(v) && !empty(v));
        k.add(
            &[ShortCopy, FullCopy],
            "copy path",
            "Copy the relative/absolute path of the file",
        )
        .when(row);
        k.add(
            &[Ignore],
            "ignore",
            "Add the file or directory to .gitignore",
        )
        .when(path);
        k.add(
            &[Cancel],
            "back",
            "Back to the files, from the diff, the conflict or the commit section",
        )
        .when(|v| diff(v) || conflict(v) || commit(v));
        k.quiet(
            &[Cancel],
            "Back, then clear the picked files, the search, the filter",
        )
        .when(|v| files(v) && !picks(v));
        k.quiet(&[GoToNext, GoToPrevious], "Go to the next/previous match")
            .when(|v| files(v) || diff(v));
        k.quiet(
            &[NextFile, PreviousFile],
            "Go to the next/previous file, staying in the diff",
        )
        .when(|v| files(v) || diff(v));
        k.quiet(
            &[NextPicked, PreviousPicked],
            "Go to the next/previous picked file",
        )
        .when(files);
        k.quiet(
            &[NavigateDown, SelectDown, NavigateUp, SelectUp],
            "Select the next/previous file, hunk, line, conflict choice or message box",
        );
        k.quiet(
            &[GoToTop, GoToBottom],
            "Select the first/last file, hunk or line",
        )
        .when(|v| files(v) || diff(v));
        k.quiet(
            &[NavigateRight, NavigateLeft],
            "Expand/collapse the directory; choose a side of the conflict",
        )
        .when(|v| files(v) || conflict(v));
        k.quiet(
            &[
                PageDown,
                PageUp,
                HalfPageDown,
                HalfPageUp,
                ScrollDown,
                ScrollUp,
            ],
            "Scroll the diff; jump half a page while selecting lines",
        );
        k.quiet(
            &[FocusPanel(1), FocusPanel(2), FocusPanel(3)],
            "Focus the unstaged files, the staged files, the commit message",
        );
        if k.view().is_none() {
            Self::register_typing_message(k);
        }
    }

    // The keys of what has the focus, for the status bar; not the ones moving around.
    pub fn keys(&self) -> KeyList {
        if self.is_typing_query() {
            return crate::view::typing_keys(self.query_kind());
        }
        let mut keys = Keys::of(self);
        if self.is_editing() {
            // every other key is typed
            Self::register_typing_message(&mut keys);
        } else {
            Self::register_keys(&mut keys);
        }
        keys.list()
    }

    fn register_typing_message(k: &mut Keys<Self>) {
        k.add(
            &[UserEvent::Cancel],
            "stop typing",
            "Stop typing the message",
        );
    }

    pub fn repository(&self) -> &'a Repository {
        self.repository
    }

    // Adds the selected file or directory to .gitignore; returns what to tell.
    pub fn ignore_selected(&mut self) -> Result<String, String> {
        let Some(entry) = self.selected_row().and_then(FileTreeRow::entry) else {
            return Ok(String::new());
        };
        let message = self.repository.ignore_path(entry)?;
        self.reload();
        Ok(message)
    }

    pub fn toplevel(&self) -> &std::path::Path {
        self.repository.path()
    }

    #[cfg(test)]
    fn take_external_request(&mut self) -> Option<ExternalRequest> {
        match self.request.take() {
            Some(Request::External(request)) => Some(request),
            other => std::mem::replace(&mut self.request, other).and(None),
        }
    }

    pub fn is_conflict_focused(&self) -> bool {
        self.focus() == Focus::Side && self.conflict.is_some()
    }

    // Moves between the sides (side by side) and the tools below them.
    pub fn move_conflict_item(&mut self, direction: Direction) {
        use ConflictItem::*;
        let Some(ConflictPanel { item, .. }) = self.conflict.as_mut() else {
            return;
        };
        *item = match (*item, direction) {
            (Existing, Direction::Right) => Incoming,
            (Incoming, Direction::Left) => Existing,
            (Existing | Incoming, Direction::Down) => MergeTool,
            (MergeTool, Direction::Down) => Editor,
            (Editor, Direction::Up) => MergeTool,
            (MergeTool, Direction::Up) => Existing,
            (item, _) => item,
        };
    }

    fn activate_conflict_item(&mut self, item: ConflictItem) -> Result<(), String> {
        let Some(path) = self.selected_path() else {
            return Ok(());
        };
        let side = match item {
            ConflictItem::Existing => ConflictSide::Ours,
            ConflictItem::Incoming => ConflictSide::Theirs,
            ConflictItem::MergeTool => {
                self.request = Some(Request::External(ExternalRequest::MergeTool(path)));
                return Ok(());
            }
            ConflictItem::Editor => {
                let path = self.repository.path().join(path);
                self.request = Some(Request::External(ExternalRequest::Editor(path)));
                return Ok(());
            }
        };
        let result = self.repository.resolve_conflict(&path, side);
        self.chosen = Focus::Files;
        self.reload();
        result
    }

    // Goes one level deeper: files -> hunks -> lines of the selected hunk.
    // Does nothing when the diff has no hunk to select.
    // In the commit section, starts editing the message; in the conflict panel, runs the choice.
    pub fn enter(&mut self) -> Result<(), String> {
        match self.focus() {
            Focus::Commit(_) | Focus::Typing(_) => self.commit_or_continue()?,
            Focus::Side => match &self.conflict {
                Some(conflict) => self.activate_conflict_item(conflict.item)?,
                None => {
                    self.pane.enter();
                }
            },
            Focus::Files if self.picked().is_empty() && self.conflict.is_some() => {
                // a conflicted file is resolved from the conflict panel instead of its diff
                if let Some(conflict) = self.conflict.as_mut() {
                    conflict.item = ConflictItem::Existing;
                }
                self.chosen = Focus::Side;
            }
            // nothing here to open: on from the unstaged files to committing, from the staged
            // ones back to the unstaged, as nothing staged commits nothing
            Focus::Files if self.files().is_empty() => match self.section {
                LocalChangesSection::Unstaged => {
                    self.focus_commit();
                    self.commit_or_continue()?;
                }
                LocalChangesSection::Staged => self.focus_section(LocalChangesSection::Unstaged),
            },
            Focus::Files => {
                // with picked rows, the diff shows all their files to stage hunks and lines from
                if !self.picked().is_empty() {
                    self.diff_source = DiffSource::Picked;
                    self.load_diff();
                }
                if !self.pane.enter() && self.pane.diff().is_some_and(|diff| diff.converted) {
                    return Err(CONVERTED_DIFF.into());
                }
            }
        }
        Ok(())
    }

    // Goes one level back: lines -> hunks -> files, or from the commit section or the conflict
    // panel to the files. Returns false when already at the files.
    pub fn back(&mut self) -> bool {
        match self.focus() {
            Focus::Typing(field) => self.chosen = Focus::Commit(field),
            Focus::Commit(_) => self.chosen = Focus::Files,
            Focus::Side if self.conflict.is_some() => self.chosen = Focus::Files,
            Focus::Side if self.pane.is_selecting_lines() => {
                self.pane.back();
            }
            Focus::Side => self.focus_files(),
            Focus::Files => return false,
        }
        true
    }

    // Moves the focus to the panel that way: the files are on the left, unstaged over staged, the
    // diff (or conflict panel) on the right over the commit message. Returns false when there is
    // none, or nothing to focus in it, like a diff without hunks.
    pub fn move_focus(&mut self, direction: Direction) -> Result<bool, String> {
        use LocalChangesSection::*;
        match (self.focus(), direction) {
            (Focus::Files, Direction::Down) if self.section == Unstaged => {
                self.focus_section(Staged)
            }
            (Focus::Files, Direction::Up) if self.section == Staged => self.focus_section(Unstaged),
            // a focus key only moves the focus: with no files, on to the message without committing
            (Focus::Files, Direction::Right) if self.files().is_empty() => self.focus_commit(),
            (Focus::Files, Direction::Right) => {
                self.enter()?;
                return Ok(self.focus() != Focus::Files);
            }
            (Focus::Side, Direction::Down) => self.focus_commit(),
            (Focus::Side | Focus::Commit(_) | Focus::Typing(_), Direction::Left) => {
                self.focus_files()
            }
            // into the diff of the selected file, else its files
            (Focus::Commit(_) | Focus::Typing(_), Direction::Up) => {
                self.focus_files();
                if !self.files().is_empty() {
                    self.enter()?;
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    // Moves the focus to the files, whose diff shows the row under the cursor again.
    pub fn focus_files(&mut self) {
        self.chosen = Focus::Files;
        self.pane.leave();
        if std::mem::replace(&mut self.diff_source, DiffSource::Row) == DiffSource::Picked {
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
        match self.focus() {
            Focus::Side if self.conflict.is_none() => self.pane.step(step),
            // the summary, or the description under it
            Focus::Commit(_) | Focus::Typing(_) => {
                let field = match step {
                    Step::Next | Step::Last => CommitField::Description,
                    _ => CommitField::Summary,
                };
                self.chosen = match self.focus() {
                    Focus::Typing(_) => Focus::Typing(field),
                    _ => Focus::Commit(field),
                };
            }
            _ => self.move_cursor(row(self)),
        }
    }

    // Scrolls the diff by the scroll keys, or moves the selected line half a page; returns
    // whether `event` was one.
    pub fn scroll_diff(&mut self, event: UserEvent) -> bool {
        self.pane.scroll(event)
    }

    // Staging the unstaged changes, unstaging the staged ones.
    fn patch_direction(&self) -> PatchDirection {
        match self.section {
            LocalChangesSection::Unstaged => PatchDirection::Forward,
            LocalChangesSection::Staged => PatchDirection::Reverse,
        }
    }

    // Stages the selected line, hunk, file or directory, or unstages it in the staged section.
    pub fn toggle_stage(&mut self) -> Result<(), String> {
        let result = match self.focus() {
            Focus::Commit(_) | Focus::Typing(_) => {
                self.edit_message();
                return Ok(());
            }
            Focus::Side if self.conflict.is_some() => return Ok(()),
            Focus::Side => {
                // the patch is made and applied the same way
                let direction = self.patch_direction();
                match self.pane.selected_patch(direction) {
                    Some(patch) => self.repository.apply_patch(&patch, direction),
                    None => Ok(()),
                }
            }
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
                .unstage_paths(&paths, self.draft.amend_base()),
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
        self.sync_amend();
        let files = [&self.changes.unstaged, &self.changes.staged];
        for (section, files) in self.sections.iter_mut().zip(files) {
            section.tree.set_tree(file_tree(files));
            // the picks with no changes left, discarded or stashed, go
            section
                .picked
                .retain(|p| files.iter().any(|f| path_contains(p, &f.path)));
        }

        let in_diff = self.is_diff_focused();
        let selection = self.pane.selection();
        self.load_diff();
        if self.diff_source == DiffSource::Picked
            && self.pane.diff().is_none_or(|d| d.hunks.is_empty())
        {
            // nothing left to stage from the picked files
            self.diff_source = DiffSource::Row;
            self.load_diff();
        }
        // stay in the diff while the same path (or the picked files) still has hunks left
        let same_source =
            self.diff_source == DiffSource::Picked || self.selected_path() == selected_path;
        if in_diff && same_source && self.conflict.is_none() {
            self.pane.restore(selection);
        }
    }

    // While amending, the staged section shows the changes of HEAD too, see `Amend`.
    fn with_amend(&self, mut changes: LocalChanges) -> LocalChanges {
        if let Some(base) = self.draft.amend_base() {
            changes.staged = self.repository.staged_changes_against(base);
        }
        changes
    }

    // The files of the current section in `row`, which pass the filter.
    fn row_files(&self, row: &FileTreeRow) -> Vec<&LocalChangeFile> {
        self.files()
            .iter()
            .filter(|f| row.contains(&f.path) && self.tree().keeps(&f.path))
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

    // The search of the diff while it has the focus, else that of the focused section.
    pub fn active_query(&self) -> &TreeQuery {
        if self.is_diff_focused() {
            &self.diff_query
        } else {
            self.query()
        }
    }

    // The diff is searched as text, the files by name.
    pub fn query_kind(&self) -> SearchKind {
        if self.is_diff_focused() {
            SearchKind::Text
        } else {
            SearchKind::Names
        }
    }

    // Applies the query being typed again, with other options.
    pub fn reapply_query(&mut self, options: SearchOptions) {
        if self.diff_query.is_editing() {
            self.search_diff(SearchDirection::Current, options);
        } else {
            let change = self.query().editing_change();
            self.apply_query_change(change, options);
        }
    }

    pub fn handle_query_key(&mut self, key: KeyEvent, options: SearchOptions) {
        if self.diff_query.is_editing() {
            if self.diff_query.handle_key(key) == Some(QueryKind::Search) {
                self.search_diff(SearchDirection::Current, options);
            }
            return;
        }
        let change = self.query_mut().handle_key(key);
        self.apply_query_change(change, options);
    }

    // Clears the search, or else the filter. Returns false when there was neither.
    pub fn clear_query(&mut self, options: SearchOptions) -> bool {
        let change = self.query_mut().clear();
        self.apply_query_change(change, options);
        change.is_some()
    }

    // Clears the search of the diff while it has the focus. Returns false when there was none.
    pub fn clear_diff_query(&mut self) -> bool {
        self.is_diff_focused() && self.diff_query.clear().is_some()
    }

    // Returns false when the search matched nothing, see `active_query`.
    pub fn search_next(&mut self, toward: Toward, options: SearchOptions) -> bool {
        let direction = SearchDirection::from(toward);
        if self.is_diff_focused() {
            self.search_diff(direction, options)
        } else {
            self.search(direction, options)
        }
    }

    // Selects the match of the diff search: the changed line it is on, else its hunk. Returns
    // false when there is none.
    fn search_diff(&mut self, direction: SearchDirection, options: SearchOptions) -> bool {
        let Some(query) = self.diff_query.search().map(String::from) else {
            return true;
        };
        self.pane.search(&query, direction, options)
    }

    // The selected line as in the file, or the selected hunk as a diff, to copy.
    pub fn selected_diff_text(&self) -> Option<String> {
        self.pane.selected_text().filter(|_| self.is_diff_focused())
    }

    fn apply_query_change(&mut self, change: Option<QueryKind>, options: SearchOptions) {
        match change {
            None => {}
            Some(QueryKind::Search) => {
                self.search(SearchDirection::Current, options);
            }
            Some(QueryKind::Filter) => {
                // only the focused section is filtered
                let filter = self.query().filter().map(String::from);
                self.update_tree(|tree| tree.set_filter(filter.as_deref(), options));
            }
        }
    }

    // Selects the match of the search in the current section, expanding the directories hiding it.
    // Returns false when there is none.
    fn search(&mut self, direction: SearchDirection, options: SearchOptions) -> bool {
        let Some(query) = self.query().search().map(String::from) else {
            return true;
        };
        self.update_tree(|tree| tree.search(&query, direction, options))
    }

    // Moves the cursor to the next (or previous) picked row, wrapping around.
    pub fn select_next_pick(&mut self, toward: Toward) {
        if self.focus() != Focus::Files {
            return;
        }
        let direction = SearchDirection::from(toward);
        let tree = self.tree();
        let current = self.selected_path();
        let picked = self.picked();
        let path = find_next(&tree.all_rows(), current.as_deref(), direction, |row| {
            picked.contains(&row.path)
        });
        self.update_tree(|tree| tree.reveal(path.as_deref()));
    }

    // Goes to the next file of the section, skipping directories; from its diff, on in the diff of
    // the next file. The diff of the picked files has no next one.
    pub fn select_next_file(&mut self, toward: Toward) -> Result<(), String> {
        let in_diff = self.is_diff_focused();
        if !(in_diff || self.focus() == Focus::Files) || self.diff_source == DiffSource::Picked {
            return Ok(());
        }
        let direction = SearchDirection::from(toward);
        let current = self.selected_path();
        let path = find_next(
            &self.tree().all_rows(),
            current.as_deref(),
            direction,
            |row| !row.is_dir(),
        );
        self.chosen = Focus::Files;
        self.update_tree(|tree| tree.reveal(path.as_deref()));
        if in_diff && self.picked().is_empty() {
            self.enter()?;
        }
        Ok(())
    }

    // Applies a tree operation, reloading the diff when the selection changed.
    fn update_tree<R>(&mut self, f: impl FnOnce(&mut FileTreeState) -> R) -> R {
        let before = self.selected_path();
        let result = f(self.tree_mut());
        if self.selected_path() != before || self.pane.diff().is_none_or(|d| d.raw.is_empty()) {
            self.load_diff();
        }
        result
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
        self.focus() == Focus::Files
    }

    // Whether the keys moving in the files apply to them: the conflict panel is beside them.
    fn focus_on_files(&self) -> bool {
        self.focus() == Focus::Files || self.is_conflict_focused()
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
    pub fn rows(&self) -> &[FileTreeRow] {
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

    pub fn selected_row(&self) -> Option<&FileTreeRow> {
        self.tree().selected()
    }

    fn load_diff(&mut self) {
        self.diff_stale = false;
        let diff = self.diff_text();
        self.set_diff(&diff);
    }

    // The diff of the picked files, else of the selected row.
    fn diff_text(&self) -> String {
        let tracked_of = match self.section {
            LocalChangesSection::Staged => DiffOf::Staged {
                base: self.draft.amend_base(),
            },
            LocalChangesSection::Unstaged => DiffOf::Unstaged,
        };
        if self.diff_source == DiffSource::Picked {
            // untracked files are only shown by diffing each against /dev/null
            let targets = self.target_files();
            let (untracked, tracked): (Vec<&LocalChangeFile>, Vec<&LocalChangeFile>) = targets
                .into_iter()
                .partition(|f| f.status == LocalChangeStatus::Untracked);
            let diff = self
                .repository
                .local_change_diff(&file_paths(&tracked), tracked_of);
            return diff + &self.repository.untracked_diff(&file_paths(&untracked));
        }
        self.selected_row()
            .map(|row| {
                let untracked = row
                    .file_index()
                    .is_some_and(|i| self.files()[i].status == LocalChangeStatus::Untracked);
                // a directory shows only the files passing the filter
                let paths = if row.is_dir() && self.query().filter().is_some() {
                    file_paths(&self.row_files(row))
                } else {
                    vec![row.path.as_str()]
                };
                let of = if untracked {
                    DiffOf::Untracked
                } else {
                    tracked_of
                };
                self.repository.local_change_diff(&paths, of)
            })
            .unwrap_or_default()
    }

    // The two sides of the selected file, when it is conflicted.
    fn load_conflict(&self) -> Option<ConflictPanel> {
        self.selected_row()
            .and_then(FileTreeRow::file_index)
            .and_then(|i| self.files().get(i))
            .filter(|f| {
                f.status == LocalChangeStatus::Conflicted && self.diff_source == DiffSource::Row
            })
            .map(|f| ConflictPanel {
                view: ConflictView::load(self.repository, &f.path),
                item: ConflictItem::Existing,
            })
    }

    fn set_diff(&mut self, diff: &str) {
        self.conflict = self.load_conflict();
        // the diff or conflict selected before is gone, the commit section stays
        if !self.is_commit_focused() {
            self.chosen = Focus::Files;
        }
        let mut diff = parse_diff(diff);
        diff.drop_converted(self.repository);
        self.pane.set_diff(Some(diff));
    }
}

// A state shown after a title in the warning color, like amending or rebasing.
fn warn_note(text: &str, theme: &ColorTheme) -> Span<'static> {
    Span::raw(format!(" · {text} ")).fg(theme.status_warn_fg)
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
        layout.place(Panel::Files, files_area, Edge::Right);
        layout.place(Panel::Unstaged, unstaged_area, Edge::Bottom);
        layout.place(Panel::Commit, commit_area, Edge::Top);
        drop(layout);

        self.render_file_section(state, LocalChangesSection::Unstaged, unstaged_area, buf);
        self.render_file_section(state, LocalChangesSection::Staged, staged_area, buf);
        if state.conflict.is_some() {
            self.render_conflict(state, diff_area, buf);
        } else {
            self.render_diff(state, diff_area, buf);
        }
        self.render_commit(state, commit_area, buf);
    }
}

// the summary, a line, the description with 3 lines, and the borders
const COMMIT_AREA_HEIGHT: u16 = 1 + 1 + 3 + 2;

impl LocalChangesPanel<'_> {
    fn render_file_section(
        &self,
        state: &LocalChangesState,
        section: LocalChangesSection,
        area: Rect,
        buf: &mut Buffer,
    ) {
        let Section { tree, picked, .. } = state.section_state(section);
        let (number, name, files) = match section {
            LocalChangesSection::Unstaged => (1, "Unstaged", &state.changes.unstaged),
            LocalChangesSection::Staged => (2, "Staged", &state.changes.staged),
        };
        let active = state.section == section;
        let focused = active && self.focused && state.focus_on_files();
        let mut extras = vec![];
        if !picked.is_empty() {
            extras.push(format!("{} picked", picked.len()));
        }
        extras.extend(state.query_of(section).indicator());
        let label = format!("[{number}] {name} ({})", files.len());
        let theme = &self.ctx.color_theme;
        let mut title = panel_title(&label, extras, focused, theme);
        if section == LocalChangesSection::Staged && state.draft.amend.is_some() {
            title.push_span(warn_note("amending", theme));
        }
        let inner = render_panel(buf, area, title, focused, theme);

        let empty = if files.is_empty() {
            "No changes"
        } else {
            "No matches"
        };
        // only the section whose diff is displayed shows its cursor, quieter while another panel,
        // like the refs sidebar, has the focus
        let offset = render_tree(
            buf,
            inner,
            tree,
            |i| Some(files[i].status.symbol()),
            // the search is highlighted in the section it applies to
            state.query_of(section).search(),
            |path| picked.contains(path),
            match (active, self.focused) {
                (false, _) => TreeCursor::Hidden,
                (true, false) => TreeCursor::Dim,
                (true, true) => TreeCursor::Bright,
            },
            empty,
            &self.ctx,
        );
        let total = tree.rows().len();
        render_panel_scrollbar(buf, area, total, offset, focused, &self.ctx.color_theme);
    }

    fn render_diff(&self, state: &mut LocalChangesState, area: Rect, buf: &mut Buffer) {
        let title = match state.selected_row() {
            _ if state.diff_source == DiffSource::Picked => {
                picked_files_label(state.target_files().len())
            }
            Some(row) if row.is_root() => "/".to_string(),
            Some(row) => row.path.clone(),
            None => String::new(),
        };
        let diff_focused = self.focused && state.is_diff_focused();
        let indicator = state.diff_query.indicator();
        let title = panel_title(&title, indicator, diff_focused, &self.ctx.color_theme);
        let search = state.diff_query.search();
        let inner = state
            .pane
            .render_panel(area, title, buf, &self.ctx, diff_focused, search);
        // with no row, the diff is empty
        if state.selected_row().is_none() {
            let message = if state.count() == 0 {
                "No local changes"
            } else {
                "No file selected"
            };
            Paragraph::new(placeholder(message, &self.ctx.color_theme)).render(inner, buf);
        }
    }

    fn render_commit(&self, state: &LocalChangesState, area: Rect, buf: &mut Buffer) {
        let theme = &self.ctx.color_theme;
        let focused = self.focused && state.is_commit_focused();
        // amending and rebasing have no controls of their own, the title shows them
        let mut title = panel_title("[3] Commit", [], focused, theme).spans;
        if state.draft.amend.is_some() {
            title.push(warn_note("amending", theme));
        }
        if let Some(operation) = state.operation {
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
    }

    fn render_conflict(&self, state: &mut LocalChangesState, area: Rect, buf: &mut Buffer) {
        let Some(conflict) = &state.conflict else {
            return;
        };
        let theme = &self.ctx.color_theme;
        let path = state.selected_path().unwrap_or_default();
        let inner = render_panel(
            buf,
            area,
            Line::from(vec![
                Span::raw(format!(" {path} ")).fg(theme.fg),
                badge(" conflicted ", theme.status_error_fg, theme),
            ]),
            self.focused && state.is_conflict_focused(),
            theme,
        );

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
        let selected_item = (self.focused && state.is_conflict_focused()).then_some(conflict.item);
        let conflict = &conflict.view;
        let focused = |item| selected_item == Some(item);

        let sides = [
            (
                ours_area,
                ConflictItem::Existing,
                "Existing",
                &conflict.ours_label,
                &conflict.ours,
            ),
            (
                theirs_area,
                ConflictItem::Incoming,
                "Incoming",
                &conflict.theirs_label,
                &conflict.theirs,
            ),
        ];
        let mut height = 0;
        for (side_area, item, name, label, lines) in sides {
            // the name git put on the markers, like "HEAD"
            let note = (!label.is_empty()).then(|| label.clone());
            let title = panel_title(name, note, focused(item), theme);
            let content_area = render_panel(buf, side_area, title, focused(item), theme);
            height = content_area.height as usize;
            let longest = conflict.ours.len().max(conflict.theirs.len());
            let scrolloff = self.ctx.ui_config.common.scrolloff as usize;
            let range = state.pane.scroll_state().fit(longest, height, scrolloff);
            render_panel_scrollbar(buf, side_area, longest, range.start, focused(item), theme);
            let lines: Vec<Line> = lines
                .iter()
                .skip(range.start)
                .take(height)
                .map(|line| {
                    if line.starts_with("── conflict") {
                        Line::raw(line.clone()).fg(theme.muted_fg)
                    } else {
                        Line::raw(line.clone()).fg(theme.fg)
                    }
                })
                .collect();
            Paragraph::new(lines).render(content_area, buf);
        }
        state.pane.scroll_state().height = height;

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

    // The summary, a line, then the description, each with its placeholder while empty; the bar
    // beside the box selected.
    fn render_message(&self, state: &LocalChangesState, area: Rect, buf: &mut Buffer) {
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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    // The diff beside the files, always loaded on the page.
    fn diff<'s>(state: &'s LocalChangesState) -> &'s crate::widget::diff::ParsedDiff {
        state.pane.diff().unwrap()
    }
    use crate::{
        git::SortCommit,
        test_support::git::GitRepository,
        widget::{file_tree::ROOT_PATH, tree_query::QueryKind},
    };

    fn numbered_lines(n: usize) -> String {
        (1..=n).map(|i| format!("line {i}\n")).collect()
    }

    // The git command of the popup asked for, to run as confirming it does, see `run_task`.
    fn confirm(state: &mut LocalChangesState) -> Result<(), String> {
        let dialog = state.take_dialog().expect("a popup asking to confirm");
        state.request = Some(Request::Task(dialog.command().unwrap()?));
        Ok(())
    }

    // Confirms the popup asked for and runs its git command.
    fn confirm_and_run(state: &mut LocalChangesState) -> Result<(), String> {
        confirm(state)?;
        state.run_task()
    }

    fn commit(state: &mut LocalChangesState) -> Result<(), String> {
        state.request = Some(Request::Task(state.commit_task()?));
        Ok(())
    }

    // The question of the popup asked for.
    fn prompt(state: &LocalChangesState) -> Option<String> {
        state.dialog()?.intro().first().cloned()
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
        let shown = diff(&state).raw.clone();

        // like a held key, moving over the rows loads no diff
        for path in ["a.txt", "b.txt", "c.txt"] {
            let pos = state.rows().iter().position(|r| r.path == path).unwrap();
            state.move_cursor(pos);
            assert_eq!(diff(&state).raw, shown);
        }
        // the row it stops on is loaded once, before the next key or the next frame
        state.load_stale_diff();
        assert!(diff(&state).raw.contains("+c.txt"));
        Ok(())
    }

    #[test]
    fn test_rows_look_like_the_other_lists() -> Result<(), Box<dyn std::error::Error>> {
        use crate::color::ColorTheme;
        use crate::test_support::ui::{
            assert_guides_keep_color, assert_only_bg_changed, assert_row_bg, column_of, find_row,
            panel_row, test_ctx,
        };
        use ratatui::buffer::Buffer;

        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        fs::create_dir(repo_path.join("dir"))?;
        for file in ["dir/a.txt", "dir/b.txt", "top.txt"] {
            fs::write(repo_path.join(file), "1\n")?;
        }
        git.add(&["dir/a.txt", "dir/b.txt", "top.txt"]);
        git.commit("init", "2024-01-01");
        for file in ["dir/a.txt", "dir/b.txt", "top.txt"] {
            fs::write(repo_path.join(file), "2\n")?;
        }
        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        let theme = ColorTheme::default();
        let draw = |state: &mut LocalChangesState, focused: bool| {
            let area = Rect::new(0, 0, 90, 16);
            let mut buf = Buffer::empty(area);
            LocalChangesPanel::new(test_ctx(), focused).render(area, &mut buf, state);
            buf
        };

        for focused in [true, false] {
            select_row(&mut state, "top.txt");
            let plain = draw(&mut state, focused);
            select_row(&mut state, "dir/a.txt");
            let selected = draw(&mut state, focused);
            let y = find_row(&selected, "M a.txt");
            let row = panel_row(&selected, "Unstaged", y, 1);
            assert_row_bg(&selected, row, theme.list_selected_bg);
            assert_only_bg_changed(&plain, &selected, row);
            assert_guides_keep_color(&selected, row, &theme);
        }

        // a picked file: across the row, its tree lines as they are
        select_row(&mut state, "dir/b.txt");
        state.toggle_pick();
        select_row(&mut state, "top.txt");
        let buf = draw(&mut state, true);
        let y = find_row(&buf, "M b.txt");
        let row = panel_row(&buf, "Unstaged", y, 1);
        assert_row_bg(&buf, row, theme.list_picked_bg);
        assert_guides_keep_color(&buf, row, &theme);

        // directories in their color
        let y = find_row(&buf, " dir ");
        assert_eq!(
            buf[(column_of(&buf, y, " dir ") + 1, y)].fg,
            theme.file_tree_dir_fg
        );
        Ok(())
    }

    #[test]
    fn test_next_file() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        fs::write(repo_path.join("a.txt"), "a\n")?;
        git.add(&["a.txt"]);
        git.commit("init", "2024-01-01");
        fs::write(repo_path.join("a.txt"), "a2\n")?;
        fs::create_dir(repo_path.join("dir"))?;
        fs::write(repo_path.join("dir/x.txt"), "x\n")?;
        fs::write(repo_path.join("dir/y.txt"), "y\n")?;

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        let files: Vec<String> = state
            .tree()
            .all_rows()
            .iter()
            .filter(|r| !r.is_dir())
            .map(|r| r.path.clone())
            .collect();
        assert_eq!(files.len(), 3);

        // from the files, skipping the directories, back to the first after the last
        select_row(&mut state, &files[0]);
        state.select_next_file(Toward::Next)?;
        assert_eq!(state.selected_path().as_ref(), Some(&files[1]));
        state.select_next_file(Toward::Previous)?;
        state.select_next_file(Toward::Previous)?;
        assert_eq!(state.selected_path().as_ref(), Some(&files[2]));
        assert!(!state.is_diff_focused());

        // from the diff, on in the diff of the next file
        state.enter()?;
        assert!(state.is_diff_focused());
        state.select_next_file(Toward::Next)?;
        assert_eq!(state.selected_path().as_ref(), Some(&files[0]));
        assert!(state.is_diff_focused());

        // the diff alone says whether it has the keys: once it selects nothing, the files do
        state.pane.leave();
        assert!(!state.is_diff_focused() && state.is_files_focused());
        Ok(())
    }

    #[test]
    fn test_commit_summary_and_description() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        fs::write(repo_path.join("a.txt"), "a\n")?;
        git.add(&["a.txt"]);
        git.commit("init", "2024-01-01");
        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        let type_keys = |state: &mut LocalChangesState, codes: &[KeyCode]| {
            for code in codes {
                state.handle_text_key(key(*code));
            }
        };

        // with no message, focusing it types the summary right away
        state.focus_commit();
        assert!(state.is_editing());
        // enter goes on to the description, tab back to the summary
        type_keys(
            &mut state,
            &[
                KeyCode::Char('s'),
                KeyCode::Enter,
                KeyCode::Char('d'),
                KeyCode::Enter,
            ],
        );
        type_keys(
            &mut state,
            &[KeyCode::Char('e'), KeyCode::Tab, KeyCode::Char('!')],
        );
        assert_eq!(state.draft.message(), "s!\n\nd\ne");
        type_keys(&mut state, &[KeyCode::Esc]);
        assert!(!state.is_editing());

        // with a message, focusing it doesn't type; j and k pick the box
        state.focus_files();
        state.focus_commit();
        assert!(!state.is_editing());
        state.select_next();
        assert_eq!(state.commit_field(), Some(CommitField::Description));
        state.select_prev();
        assert_eq!(state.commit_field(), Some(CommitField::Summary));
        // leaving the section forgets the box: back in it, the summary is selected
        state.select_next();
        state.focus_files();
        state.focus_commit();
        assert_eq!(state.commit_field(), Some(CommitField::Summary));

        // a message splits at its first line, without the blank lines between
        state.draft.set_message("subject\n\n\nbody");
        assert_eq!(state.draft.summary.text(), "subject");
        assert_eq!(state.draft.description.text(), "body");
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
        assert_eq!(diff(&state).hunks.len(), 2);
        state.toggle_stage()?;
        assert_eq!(paths(&state.changes.staged), vec!["a.txt"]);
        assert_eq!(
            paths(&state.changes.unstaged),
            vec!["a.txt", "dir/x.txt", "dir/y.txt"]
        );
        assert!(state.is_diff_focused());
        assert_eq!(diff(&state).hunks.len(), 1);

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
        let staged_diff = repository.local_change_diff(&["a.txt"], DiffOf::Staged { base: None });
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
            state.handle_query_key(
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
                SearchOptions::default(),
            );
        }
        state.handle_query_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            SearchOptions::default(),
        );
        assert!(state.is_selecting_lines());
        assert_eq!(state.selected_diff_text().as_deref(), Some("new A"));
        assert_eq!(state.query().search(), None);
        state.search_next(Toward::Next, SearchOptions::default());
        assert_eq!(state.selected_diff_text().as_deref(), Some("new B"));
        state.search_next(Toward::Next, SearchOptions::default());
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
        assert!(diff(&state).raw.contains("+Y"));
        assert_eq!(state.enter().unwrap_err(), CONVERTED_DIFF);
        assert_eq!(state.focus(), Focus::Files);
        // the others as usual
        select_row(&mut state, "a.txt");
        state.enter()?;
        assert!(state.is_diff_focused() && !state.is_selecting_lines());
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
        let staged_diff =
            |path| repository.local_change_diff(&[path], DiffOf::Staged { base: None });
        let unstaged_diff = |path| repository.local_change_diff(&[path], DiffOf::Unstaged);

        // files -> hunks -> lines, starting on the first changed line
        select_row(&mut state, "a.txt");
        state.enter()?;
        assert_eq!(diff(&state).hunks.len(), 1);
        state.enter()?;
        assert!(state.is_selecting_lines());
        assert_eq!(state.selected_diff_text().as_deref(), Some("new A"));

        // stage a single line, staying in line mode on the next changed line
        state.toggle_stage()?;
        assert!(staged_diff("a.txt").contains("+new A"));
        assert!(!staged_diff("a.txt").contains("+new B"));
        assert!(unstaged_diff("a.txt").contains("+new B"));
        assert!(state.is_selecting_lines());
        assert_eq!(state.selected_diff_text().as_deref(), Some("new B"));

        // staging the last line returns to the files
        state.toggle_stage()?;
        assert!(staged_diff("a.txt").contains("+new B"));
        assert!(!state.is_diff_focused());
        assert!(!state.is_selecting_lines());

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
        assert!(state.is_diff_focused() && !state.is_selecting_lines());
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
        state.draft.set_message("draft");
        assert_eq!(paths(&state.changes.staged), vec!["c.txt"]);

        // 3, down to the checkbox, enter: the last commit is combined with the staged changes
        state.toggle_amend()?;
        assert_eq!(
            paths(&state.changes.staged),
            vec!["a.txt", "b.txt", "c.txt"]
        );
        assert_eq!(state.draft.message(), "second\n\nbody");

        // the staged diff includes the change made by the last commit
        state.focus_section(LocalChangesSection::Staged);
        select_row(&mut state, "a.txt");
        assert!(diff(&state).raw.contains("+more"));

        // unstaging while amending removes the file from the amended commit
        select_row(&mut state, "b.txt");
        state.toggle_stage()?;
        assert_eq!(paths(&state.changes.staged), vec!["a.txt", "c.txt"]);
        assert_eq!(paths(&state.changes.unstaged), vec!["b.txt"]);

        // the staged and the commit titles say so alike
        let area = Rect::new(0, 0, 120, 20);
        let mut buf = Buffer::empty(area);
        LocalChangesPanel::new(crate::test_support::ui::test_ctx(), true)
            .render(area, &mut buf, &mut state);
        let mut notes = vec![];
        for y in 0..area.height {
            let row: String = (0..area.width).map(|x| buf[(x, y)].symbol()).collect();
            for (i, _) in row.match_indices("· amending") {
                let x = row[..i].chars().count() as u16;
                notes.push(buf[(x, y)].style());
            }
        }
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert_eq!(notes[0], notes[1]);

        // an operation starting meanwhile leaves A to stop amending, not to start it
        state.operation = Some(Operation::Merge);
        assert!(state.keys().takes(UserEvent::ToggleAmend));
        state.operation = None;

        // the draft survives a refresh
        let reloaded = LocalChangesState::restore(&repository, Some(state.snapshot()));
        assert_eq!(paths(&reloaded.changes.staged), vec!["a.txt", "c.txt"]);

        // turning amend off restores the staged section and the typed message
        state.toggle_amend()?;
        assert_eq!(state.draft.message(), "draft");
        state.operation = Some(Operation::Merge);
        assert!(!state.keys().takes(UserEvent::ToggleAmend));
        state.operation = None;
        assert_eq!(paths(&state.changes.staged), vec!["b.txt", "c.txt"]);
        assert_eq!(state.changes.staged[0].status, LocalChangeStatus::Deleted);

        Ok(())
    }

    #[test]
    fn test_amend_stops_when_head_moves() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        fs::write(repo_path.join("a.txt"), "a\n")?;
        git.add(&["a.txt"]);
        git.commit("first", "2024-01-01");
        git_stdout(repo_path, &["branch", "other"]);
        fs::write(repo_path.join("a.txt"), "b\n")?;
        git.add(&["a.txt"]);
        git.commit("second", "2024-01-02");

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        state.draft.set_message("draft");
        state.toggle_amend()?;
        assert_eq!(paths(&state.changes.staged), vec!["a.txt"]);

        // another branch checked out: its HEAD isn't what was amended, the typed message is back
        git.checkout("other");
        let state = LocalChangesState::restore(&repository, Some(state.snapshot()));
        assert!(state.draft.amend.is_none());
        assert!(state.changes.staged.is_empty());
        assert_eq!(state.draft.message(), "draft");
        Ok(())
    }

    #[test]
    fn test_edit_stop_amends_the_commit() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        for (key, value) in [
            ("user.name", "Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
        ] {
            git_stdout(repo_path, &["config", key, value]);
        }
        git.commit("first", "2024-01-01");
        fs::write(repo_path.join("a.txt"), "a\n")?;
        git.add(&["a.txt"]);
        git.commit("second", "2024-01-02");
        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        // amending the last commit, then a rebase stops to edit it: the stop's amend takes over
        state.toggle_amend()?;
        let status = crate::git::git_command()
            .args(["rebase", "-q", "-i", "HEAD~1"])
            .env("GIT_SEQUENCE_EDITOR", "sed -i 1s/^pick/edit/")
            .current_dir(repo_path)
            .status()?;
        assert!(status.success());
        state.reload();
        assert_eq!(state.operation, Some(Operation::Rebase));
        assert!(state.editing_commit());
        assert_eq!(state.draft.message(), "second");
        // going on, amending stops
        git_stdout(repo_path, &["rebase", "--continue"]);
        state.reload();
        assert!(!state.editing_commit());
        assert!(state.draft.amend.is_none());
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
        let output = crate::git::git_command()
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
    fn test_keys_where_they_work() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        git.commit("init", "2024-01-01");
        fs::write(repo_path.join("a.txt"), "a\n")?;
        git.add(&["a.txt"]);
        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        let shown = |state: &LocalChangesState| -> Vec<String> {
            state.keys().hints().into_iter().map(|(_, s)| s).collect()
        };

        // nothing unstaged: no row to stage, pick, edit or filter; stashing and committing work
        state.focus_section(LocalChangesSection::Unstaged);
        let keys = state.keys();
        for event in [
            UserEvent::ToggleStage,
            UserEvent::ToggleSelect,
            UserEvent::Edit,
            UserEvent::Filter,
            UserEvent::Ignore,
        ] {
            assert!(!keys.takes(event), "{event:?}");
        }
        assert!(keys.takes(UserEvent::Stash));
        assert!(shown(&state).contains(&"commit".to_string()));

        // on the staged root, staging works but not editing or ignoring; on the file, all
        state.focus_section(LocalChangesSection::Staged);
        assert!(state.keys().takes(UserEvent::ToggleStage));
        assert!(!state.keys().takes(UserEvent::Edit));
        assert!(!state.keys().takes(UserEvent::Ignore));
        select_row(&mut state, "a.txt");
        assert!(state.keys().takes(UserEvent::Edit));
        assert!(state.keys().takes(UserEvent::Ignore));

        // nothing staged: c says so, unlisted
        git_stdout(repo_path, &["reset", "-q"]);
        state.reload();
        state.focus_section(LocalChangesSection::Staged);
        assert!(state.keys().takes(UserEvent::Commit));
        assert!(!shown(&state).contains(&"commit".to_string()));
        Ok(())
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
        state.draft.set_message("first change");
        assert!(commit(&mut state).is_err());
        fs::write(repo_path.join("a.txt"), "a\n")?;
        git.add(&["a.txt"]);
        state.reload();

        // c with an empty message starts typing it instead
        state.draft.set_message("");
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
        state.draft.set_message("typed");
        state.enter()?;
        assert!(state.dialog().and_then(GitDialog::commit_preview).is_some());
        state.take_dialog();
        assert!(state.is_commit_focused());
        state.back();

        // enter on the empty unstaged files goes on to committing too
        state.focus_section(LocalChangesSection::Unstaged);
        state.enter()?;
        assert!(state.dialog().and_then(GitDialog::commit_preview).is_some());
        state.take_dialog();
        state.back();
        // and on the empty staged ones back to the unstaged
        git_stdout(repo_path, &["reset", "-q"]);
        state.reload();
        state.focus_section(LocalChangesSection::Staged);
        state.enter()?;
        assert_eq!(state.section, LocalChangesSection::Unstaged);
        // which, empty, still commits
        git.add(&["a.txt"]);
        state.reload();
        state.enter()?;
        assert!(state.dialog().and_then(GitDialog::commit_preview).is_some());
        state.take_dialog();
        assert!(state.is_commit_focused());
        state.back();

        // c with a message asks to confirm the commit, showing it
        state.draft.set_message("first change\n\nbody");
        state.commit_or_continue()?;
        let Some(preview) = state.dialog().and_then(GitDialog::commit_preview) else {
            panic!("no commit to confirm");
        };
        assert_eq!(
            preview.kind,
            CommitKind::New {
                branch: Some("master".into()),
                finishing: None
            }
        );
        assert_eq!(
            preview.kind.sentence(),
            ("Commit to".into(), "master".into())
        );
        assert_eq!(preview.subject, "first change");
        assert_eq!(preview.body, vec!["body"]);
        let files: Vec<_> = preview
            .files
            .iter()
            .map(|f| (f.status, f.path.as_str()))
            .collect();
        assert_eq!(files, vec![(LocalChangeStatus::Added, "a.txt")]);
        confirm(&mut state)?;
        state.run_task()?;
        assert_eq!(
            git_stdout(repo_path, &["log", "-1", "--format=%B"]),
            "first change\n\nbody"
        );
        assert!(state.changes.staged.is_empty());
        assert!(state.draft.is_empty());

        // amend with another staged file, keeping the message of HEAD
        fs::write(repo_path.join("b.txt"), "b\n")?;
        git.add(&["b.txt"]);
        state.reload();
        state.toggle_amend()?;
        commit(&mut state)?;
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
        assert!(state.draft.amend.is_none());

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
        state.request_discard()?;
        assert!(state.take_dialog().is_some());
        assert_eq!(fs::read_to_string(repo_path.join("a.txt"))?, modified);

        // discard the first hunk
        state.request_discard()?;
        confirm_and_run(&mut state)?;
        let content = fs::read_to_string(repo_path.join("a.txt"))?;
        assert!(content.contains("line 2\n") && content.contains("line twenty-eight\n"));
        assert_eq!(diff(&state).hunks.len(), 1);

        // discard only the removal of "line 28", which brings it back next to the added line
        state.enter()?;
        state.enter()?;
        state.request_discard()?;
        confirm_and_run(&mut state)?;
        let content = fs::read_to_string(repo_path.join("a.txt"))?;
        assert!(content.contains("line 28\nline twenty-eight\n"));

        // discard a directory of untracked files
        state.back();
        state.back();
        select_row(&mut state, "dir");
        state.request_discard()?;
        confirm_and_run(&mut state)?;
        assert!(!repo_path.join("dir/u.txt").exists());

        // nothing is discarded from the staged section
        git.add(&["a.txt"]);
        state.reload();
        state.focus_section(LocalChangesSection::Staged);
        state.request_discard()?;
        assert!(state.dialog().is_none());

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
        assert!(diff(&state).raw.contains("+b"));

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
        state.request_discard()?;
        assert_eq!(
            prompt(&state),
            Some("Discard all changes to all files".into())
        );
        confirm_and_run(&mut state)?;
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
            state.handle_query_key(key(KeyCode::Char(c)), SearchOptions::default());
        }
        state.handle_query_key(key(KeyCode::Enter), SearchOptions::default());
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
        assert!(diff(&state).raw.contains("+a"));
        assert_eq!(
            state
                .query_of(LocalChangesSection::Unstaged)
                .indicator()
                .as_deref(),
            Some("/app")
        );
        assert!(state.clear_query(SearchOptions::default()));

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
        assert_eq!(
            state.query_of(LocalChangesSection::Staged).indicator(),
            None
        );

        // n and N step through matches, wrapping around
        type_query(&mut state, QueryKind::Search, "list");
        assert_eq!(state.selected_path().as_deref(), Some("docs/list.md"));
        state.search_next(Toward::Next, SearchOptions::default());
        assert_eq!(state.selected_path().as_deref(), Some("src/view/list.rs"));
        state.search_next(Toward::Next, SearchOptions::default());
        assert_eq!(state.selected_path().as_deref(), Some("docs/list.md"));

        // esc clears the search of the staged section, the filter is the unstaged one's
        assert!(state.clear_query(SearchOptions::default()));
        assert!(!state.clear_query(SearchOptions::default()));
        state.focus_section(LocalChangesSection::Unstaged);
        assert!(visible(&state).is_empty());
        assert!(state.clear_query(SearchOptions::default()));
        assert!(!state.clear_query(SearchOptions::default()));
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
        assert!(state.is_diff_focused() && state.diff_source == DiffSource::Picked);
        assert_eq!(diff(&state).hunks.len(), 2);
        state.toggle_stage()?;
        assert_eq!(paths(&state.changes.unstaged), vec!["c.txt"]);
        assert!(state.is_diff_focused() && state.diff_source == DiffSource::Picked);
        state.toggle_stage()?;
        assert!(state.changes.unstaged.is_empty());
        assert!(!state.is_diff_focused() && state.diff_source == DiffSource::Row);

        // discard the picked files only
        fs::write(repo_path.join("b.txt"), "x\nchanged\nagain\n")?;
        fs::write(repo_path.join("c.txt"), "x\nchanged\nagain\n")?;
        state.reload();
        state.clear_picks();
        select_row(&mut state, "b.txt");
        state.toggle_pick();
        state.request_discard()?;
        assert_eq!(
            prompt(&state),
            Some("Discard all changes to 1 picked file".into())
        );
        confirm_and_run(&mut state)?;
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
        assert!(state.clear_query(SearchOptions::default()));
        assert!(!state.clear_query(SearchOptions::default()));

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
        assert_eq!(prompt(&state), Some("Stash 1 picked file".into()));
        let dialog = state.dialog_mut().unwrap();
        for c in "only a".chars() {
            dialog.handle_key(None, key(KeyCode::Char(c)));
        }
        confirm_and_run(&mut state)?;
        assert_eq!(paths(&state.changes.unstaged), vec!["b.txt", "u.txt"]);
        assert!(git_stdout(repo_path, &["stash", "list"]).contains("only a"));

        // cancelling stashes nothing
        state.start_stash()?;
        state.take_dialog();
        assert_eq!(paths(&state.changes.unstaged), vec!["b.txt", "u.txt"]);

        // without picks, everything is stashed, including untracked files
        state.start_stash()?;
        confirm_and_run(&mut state)?;
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

        // abort only works while rebasing
        assert!(!state.keys().takes(UserEvent::Abort));
        Ok(())
    }

    #[test]
    fn test_focus_keys_never_continue() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let _git = conflicting_rebase(repo_path);
        fs::write(repo_path.join("a.txt"), "resolved\n")?;
        git_stdout(repo_path, &["add", "a.txt"]);

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        assert!(state.changes.unstaged.is_empty());

        // right from the empty unstaged files, and up from the message, only move the focus
        assert!(state.move_focus(Direction::Right)?);
        assert!(state.is_commit_focused());
        assert!(state.move_focus(Direction::Up)?);
        assert!(state.take_request().is_none());
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

        state.request_abort();
        assert_eq!(
            prompt(&state),
            Some("Abort the rebase and go back to where it started".into())
        );
        // cancelling keeps rebasing
        state.take_dialog();
        assert_eq!(state.operation, Some(Operation::Rebase));

        state.request_abort();
        confirm_and_run(&mut state)?;
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
        state.pane.scroll_state().scroll(3);
        // as loaded in the background
        let files = || FilesSnapshot::load(repo_path, repository.git_dir());

        // nothing changed: the diff stays scrolled
        state.reload_if_changed(files());
        assert_eq!(state.pane.scroll_state().offset, 3);

        // the file changed, still modified: the diff is loaded again
        fs::write(
            repo_path.join("a.txt"),
            numbered_lines(30).replace("line 3\n", "three\n"),
        )?;
        state.reload_if_changed(files());
        assert_eq!(state.pane.scroll_state().offset, 0);
        assert!(diff(&state).raw.contains("+three"));
        Ok(())
    }

    #[test]
    fn test_nested_repository() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        git.commit("init", "2024-01-01");
        let nested = repo_path.join("nested");
        fs::create_dir(&nested)?;
        GitRepository::new(&nested).init();
        fs::write(nested.join("n.txt"), "n\n")?;

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        // listed by git as `nested/`, a row named after it, not an empty name under it
        let rows: Vec<String> = state.rows().iter().map(|r| r.name.clone()).collect();
        assert_eq!(rows, vec!["/", "nested"]);
        // `clean -f` would skip it, saying it discarded it
        select_row(&mut state, "nested");
        let err = state.request_discard().unwrap_err();
        assert!(err.contains("repository of its own"), "{err}");
        assert!(nested.join("n.txt").exists());
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
        state.request_discard()?;
        assert_eq!(
            prompt(&state),
            Some("Discard all changes to all files".into())
        );
        confirm_and_run(&mut state)?;
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
        // the conflicted files in the description
        assert_eq!(state.draft.summary.text(), "Merge branch 'feature'");
        assert_eq!(state.draft.description.text(), "Conflicts:\n  a.txt");
        assert!(state.toggle_amend().is_err());
        // also when the page opens from a snapshot; once cleared, it stays so
        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let restored = LocalChangesState::restore(&repository, Some(Default::default()));
        assert_eq!(restored.draft.summary.text(), "Merge branch 'feature'");
        state.draft.set_message("");
        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let restored = LocalChangesState::restore(&repository, Some(state.snapshot()));
        assert_eq!(restored.draft.message(), "");
        state.draft.set_message("Merge branch 'feature'");

        resolve_and_stage(&mut state, repo_path);
        state.commit_or_continue()?;
        confirm_and_run(&mut state)?;
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
        state.request_abort();
        assert_eq!(
            prompt(&state),
            Some("Abort the merge and go back to where it started".into())
        );
        confirm_and_run(&mut state)?;
        assert_eq!(state.operation, None);
        assert_eq!(git_stdout(repo_path, &["rev-parse", "HEAD"]), before);
        // its message goes with it, it would be committed for the next commit
        assert_eq!(state.draft.message(), "");

        // but not once edited
        git_stdout(repo_path, &["merge", "feature"]);
        state.reload();
        state.draft.set_message("my own");
        git_stdout(repo_path, &["merge", "--abort"]);
        state.reload();
        assert_eq!(state.draft.message(), "my own");
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
        assert_eq!(
            state.draft.message(),
            "feature change\n\nConflicts:\n  a.txt"
        );

        resolve_and_stage(&mut state, repo_path);
        state.commit_or_continue()?;
        confirm_and_run(&mut state)?;
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
            .message()
            .starts_with("Revert \"feature change\""));

        state.request_abort();
        confirm_and_run(&mut state)?;
        assert_eq!(state.operation, None);
        assert!(!state.keys().takes(UserEvent::Abort));
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
        let conflict = &state.conflict.as_ref().unwrap().view;
        assert_eq!(
            (conflict.ours_label.as_str(), conflict.theirs_label.as_str()),
            ("HEAD", "feature")
        );
        assert_eq!(conflict.ours, ["── conflict 1 ──", "master"]);
        assert_eq!(conflict.theirs, ["── conflict 1 ──", "feature"]);
        // drawn as panels, their text a column off the border
        let area = Rect::new(0, 0, 120, 30);
        let mut buf = Buffer::empty(area);
        LocalChangesPanel::new(crate::test_support::ui::test_ctx(), true)
            .render(area, &mut buf, &mut state);
        let rows: Vec<String> = (0..area.height)
            .map(|y| (0..area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        assert!(
            rows.iter().any(|r| r.contains("╭ Existing  HEAD ")),
            "{rows:#?}"
        );
        assert!(rows.iter().any(|r| r.contains("│ master ")), "{rows:#?}");

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
        let conflict = &state.conflict.as_ref().unwrap().view;
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
        state.select_next_pick(Toward::Next);
        assert_eq!(state.selected_path().as_deref(), Some("dir/c.txt"));
        state.select_next_pick(Toward::Next);
        assert_eq!(state.selected_path().as_deref(), Some("b.txt"));
        state.select_next_pick(Toward::Next);
        assert_eq!(state.selected_path().as_deref(), Some("dir/c.txt"));
        state.select_next_pick(Toward::Previous);
        assert_eq!(state.selected_path().as_deref(), Some("b.txt"));
        Ok(())
    }
}
