use std::{collections::BTreeSet, ops::Range, path::PathBuf, rc::Rc};

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, StatefulWidget, Widget},
};

use ratatui::crossterm::event::{Event, KeyCode, KeyEvent};
use tui_input::{backend::crossterm::EventHandler, Input};

use crate::{
    app::AppContext,
    event::UserEvent,
    git::{
        ConflictSide, Head, LocalChangeFile, LocalChangeStatus, LocalChanges, Operation,
        Repository,
    },
    widget::{
        conflict::ConflictView,
        diff::{diff_line_count, diff_lines, parse_diff, ParsedDiff},
        file_tree::{
            find_next, path_contains, FileTree, FileTreeRow, FileTreeState, SearchDirection,
        },
        style::{badge, key_hints, mark_selected_row, panel_title, tree_panel_width},
        text_area::TextArea,
        tree_query::{highlighted_name, QueryChange, QueryKind, TreeQuery},
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

// A direction to move between the choices of the conflict panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictMove {
    Left,
    Right,
    Up,
    Down,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfirmAction {
    Discard,
    Abort(Operation),
    Commit,
}

// The commit section's contents, kept across refreshes.
#[derive(Debug, Default, Clone)]
pub struct CommitDraft {
    message: TextArea,
    amend_base: Option<String>,
    message_before_amend: Option<TextArea>,
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
    selected_hunk: usize,
    // index into `diff.rows`, always an added or removed line while selecting lines
    selected_line: usize,
    diff_offset: usize,
    diff_height: usize,

    message: TextArea,
    // set while amending: the parent of HEAD, which the staged section is compared to instead of HEAD,
    // so it shows the changes of the commit being amended together with what is staged
    amend_base: Option<String>,
    // the message typed before amending, restored when amending is turned off
    message_before_amend: Option<TextArea>,
    // set once a commit or stash is made, until the view picks it up to refresh the repository
    needs_refresh: bool,
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
    // whether git's prepared message was put in the message box for the operation in progress
    message_prepared: bool,
}

impl<'a> LocalChangesState<'a> {
    #[cfg(test)]
    pub fn load(repository: &'a Repository) -> Self {
        Self::restore(repository, None)
    }

    // Loads the page as it was in `snapshot`, loading the diff once, for the selected file.
    pub fn restore(repository: &'a Repository, snapshot: Option<LocalChangesSnapshot>) -> Self {
        let changes = repository.take_loaded_local_changes();
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
            selected_hunk: 0,
            selected_line: 0,
            diff_offset: 0,
            diff_height: 0,
            message: TextArea::default(),
            amend_base: None,
            message_before_amend: None,
            needs_refresh: false,
            stash_input: None,
            confirm: None,
            conflict: None,
            conflict_item: ConflictItem::Existing,
            external_request: None,
            operation: repository.operation_in_progress(),
            message_prepared: false,
        };
        state.prepare_message();
        if let Some(snapshot) = snapshot {
            let draft = *snapshot.commit_draft;
            state.message = draft.message;
            state.amend_base = draft.amend_base;
            state.message_before_amend = draft.message_before_amend;
            if state.amend_base.is_some() {
                // the staged section shows the last commit too
                state.changes = state.load_changes();
                let tree = file_tree(&state.changes.staged);
                state.sections[1].tree.set_tree(tree);
            }
            if let Some(path) = snapshot.selected_path {
                state.tree_mut().select_path(&path);
            }
        }
        state.load_diff();
        state
    }

    // What to restore of the page, after a refresh or coming back to it.
    pub fn snapshot(&self) -> LocalChangesSnapshot {
        LocalChangesSnapshot {
            section: self.section,
            selected_path: self.selected_path(),
            commit_draft: Box::new(self.commit_draft()),
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
            self.message.handle_key(key);
        }
    }

    pub fn is_commit_focused(&self) -> bool {
        matches!(self.focus, Focus::Commit { .. })
    }

    // Selecting hunks or lines in the diff.
    fn is_diff_focused(&self) -> bool {
        matches!(self.focus, Focus::Hunks | Focus::Lines)
    }

    // Continues the rebase when one is in progress; otherwise asks to confirm the commit (which also
    // finishes a merge, cherry-pick or revert), or starts typing the message when there is none yet.
    pub fn commit_or_continue(&mut self) -> Result<(), String> {
        if let Some(operation) = self.operation.filter(|o| o.continues_without_commit()) {
            let result = self.repository.continue_operation(operation);
            // a continue that stops on the next conflict has still applied commits
            self.needs_refresh = true;
            self.reload();
            return result;
        }
        if self.message.text().trim().is_empty() {
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
        let branch = match self.repository.head() {
            Head::Branch { name } => name.clone(),
            _ => "detached HEAD".to_string(),
        };
        let heading = match (self.amend_base.is_some(), self.operation) {
            (true, _) => format!("Amend the last commit on {branch}"),
            (false, Some(operation)) => format!("Commit the {} on {branch}", operation.name()),
            (false, None) => format!("Commit to {branch}"),
        };
        let mut lines = vec![heading, String::new()];
        let message = self.message.text();
        let message: Vec<&str> = message.trim_end().lines().collect();
        lines.extend(message.iter().take(COMMIT_PREVIEW_LINES).map(|l| format!("  {l}")));
        if message.len() > COMMIT_PREVIEW_LINES {
            lines.push("  …".into());
        }
        lines.push(String::new());
        let files = &self.changes.staged;
        lines.push(match files.len() {
            0 => "No staged changes".to_string(),
            1 => "1 staged file:".to_string(),
            n => format!("{n} staged files:"),
        });
        for file in files.iter().take(COMMIT_PREVIEW_LINES) {
            lines.push(format!("  {} {}", status_symbol(file.status), file.path));
        }
        if files.len() > COMMIT_PREVIEW_LINES {
            lines.push(format!("  … and {} more", files.len() - COMMIT_PREVIEW_LINES));
        }
        self.confirm = Some((ConfirmAction::Commit, lines.join("\n")));
        Ok(())
    }

    // Whether there is something to commit, with a message.
    fn check_commit(&self) -> Result<(), String> {
        if self.message.text().trim().is_empty() {
            return Err("Commit message is empty".into());
        }
        let amend = self.amend_base.is_some();
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
                Some(message) if !self.message_prepared && self.message.is_empty() => {
                    self.message = TextArea::new(&message);
                    self.message_prepared = true;
                }
                Some(_) => {}
                None => self.message_prepared = false,
            }
            return;
        };
        if operation.continues_without_commit() || self.message_prepared || !self.message.is_empty()
        {
            return;
        }
        if let Some(message) = self.repository.prepared_message() {
            self.message = TextArea::new(&message);
            self.message_prepared = true;
        }
    }

    // Commits the staged changes with the typed message, amending HEAD while amend is checked.
    pub fn commit(&mut self) -> Result<(), String> {
        self.check_commit()?;
        let message = self.message.text();
        let amend = self.amend_base.is_some();
        self.repository.commit_staged(&message, amend)?;
        // the next commits of a cherry-pick or revert of several commits
        let continued = match self.operation {
            Some(operation) => self.repository.continue_operation(operation),
            None => Ok(()),
        };

        self.message = TextArea::default();
        self.amend_base = None;
        self.message_before_amend = None;
        self.message_prepared = false;
        self.needs_refresh = true;
        self.reload();
        continued
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

    // y or Enter confirms, n or Esc cancels; other keys are ignored.
    pub fn handle_confirm_key(&mut self, key: KeyEvent) -> Result<(), String> {
        match key.code {
            KeyCode::Char('y' | 'Y') | KeyCode::Enter => self.confirm(),
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                self.confirm = None;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    // Whether the popup asks to confirm a commit, which the commit key confirms too.
    pub fn is_confirming_commit(&self) -> bool {
        matches!(self.confirm, Some((ConfirmAction::Commit, _)))
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
                        self.needs_refresh = true;
                        self.repository.abort_operation(operation)
                    }
                    // reloads itself
                    ConfirmAction::Commit => return self.commit(),
                };
                self.reload();
                result
    }

    // Unstaged changes are between the index and the working tree, so discarding reverts the working tree.
    fn discard(&self) -> Result<(), String> {
        if self.is_diff_focused() {
            let Some(hunk) = self.diff.hunks.get(self.selected_hunk) else {
                return Ok(());
            };
            let patch = if self.focus == Focus::Lines {
                let line = self.selected_line - hunk.rows.start - 1;
                // the other changed lines stay in the working tree, like when unstaging from the index
                hunk.partial_patch(&[line], true)
            } else {
                hunk.patch()
            };
            return self.repository.apply_patch_to_worktree(&patch, true);
        }

        let (untracked, tracked): (Vec<&LocalChangeFile>, Vec<&LocalChangeFile>) = self
            .target_files()
            .into_iter()
            .partition(|f| f.status == LocalChangeStatus::Untracked);
        self.repository
            .discard_paths(&file_paths(&tracked), &file_paths(&untracked))
    }

    pub fn commit_draft(&self) -> CommitDraft {
        CommitDraft {
            message: self.message.clone(),
            amend_base: self.amend_base.clone(),
            message_before_amend: self.message_before_amend.clone(),
        }
    }

    // Turning amend on shows the changes of HEAD in the staged section, combined with what is staged,
    // and the HEAD message in place of the typed one. Turning it off restores both.
    pub fn toggle_amend(&mut self) -> Result<(), String> {
        if let Some(operation) = self.operation.filter(|_| self.amend_base.is_none()) {
            // HEAD is the commit before the operation, amending it would mix the two up
            return Err(format!("Cannot amend during a {}", operation.name()));
        }
        if self.amend_base.take().is_some() {
            self.message = self.message_before_amend.take().unwrap_or_default();
        } else {
            let base = self.repository.amend_base()?;
            let message = self.repository.head_commit_message()?;
            self.amend_base = Some(base);
            self.message_before_amend = Some(std::mem::replace(
                &mut self.message,
                TextArea::new(&message),
            ));
        }
        self.reload();
        Ok(())
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
    pub fn move_conflict_item(&mut self, direction: ConflictMove) {
        use ConflictItem::*;
        self.conflict_item = match (self.conflict_item, direction) {
            (Existing, ConflictMove::Right) => Incoming,
            (Incoming, ConflictMove::Left) => Existing,
            (Existing | Incoming, ConflictMove::Down) => MergeTool,
            (MergeTool, ConflictMove::Down) => Editor,
            (Editor, ConflictMove::Up) => MergeTool,
            (MergeTool, ConflictMove::Up) => Existing,
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
                if !self.diff.hunks.is_empty() {
                    self.focus = Focus::Hunks;
                    self.select_hunk(0);
                }
            }
            Focus::Hunks => {
                let hunk_rows = self.diff.hunks[self.selected_hunk].rows.clone();
                if let Some(row) = hunk_rows
                    .into_iter()
                    .find(|&r| self.diff.rows[r].is_change())
                {
                    self.focus = Focus::Lines;
                    self.select_line(row);
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
                self.select_hunk(self.selected_hunk);
            }
            Focus::Hunks => self.focus_files(),
            Focus::Files => return false,
        }
        true
    }

    // Moves the focus to the files, whose diff shows the row under the cursor again.
    pub fn focus_files(&mut self) {
        self.focus = Focus::Files;
        if std::mem::take(&mut self.diff_of_picked) {
            self.load_diff();
        }
    }

    pub fn select_next(&mut self) {
        match self.focus {
            Focus::Lines => {
                let row = self.changed_rows().find(|&r| r > self.selected_line);
                self.select_line_if(row);
            }
            Focus::Hunks => self.select_hunk(self.selected_hunk + 1),
            _ => self.move_cursor(self.cursor() + 1),
        }
    }

    pub fn select_prev(&mut self) {
        match self.focus {
            Focus::Lines => {
                let row = self.changed_rows().rfind(|&r| r < self.selected_line);
                self.select_line_if(row);
            }
            Focus::Hunks => self.select_hunk(self.selected_hunk.saturating_sub(1)),
            _ => self.move_cursor(self.cursor().saturating_sub(1)),
        }
    }

    pub fn select_first(&mut self) {
        match self.focus {
            Focus::Lines => {
                let row = self.changed_rows().next();
                self.select_line_if(row);
            }
            Focus::Hunks => self.select_hunk(0),
            _ => self.move_cursor(0),
        }
    }

    pub fn select_last(&mut self) {
        match self.focus {
            Focus::Lines => {
                let row = self.changed_rows().last();
                self.select_line_if(row);
            }
            Focus::Hunks => self.select_hunk(usize::MAX),
            _ => self.move_cursor(usize::MAX),
        }
    }

    fn select_line_if(&mut self, row: Option<usize>) {
        if let Some(row) = row {
            self.select_line(row);
        }
    }

    pub fn scroll_diff_down(&mut self, n: usize) {
        self.diff_offset = self.diff_offset.saturating_add(n);
    }

    pub fn scroll_diff_up(&mut self, n: usize) {
        self.diff_offset = self.diff_offset.saturating_sub(n);
    }

    pub fn diff_page_height(&self) -> usize {
        self.diff_height.max(1)
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
        let Some(hunk) = self.diff.hunks.get(self.selected_hunk) else {
            return Ok(());
        };
        let reverse = self.section == LocalChangesSection::Staged;
        self.repository.apply_patch_to_index(&hunk.patch(), reverse)
    }

    fn toggle_stage_line(&self) -> Result<(), String> {
        let Some(hunk) = self.diff.hunks.get(self.selected_hunk) else {
            return Ok(());
        };
        // the hunk header occupies the first row, its lines follow
        let line = self.selected_line - hunk.rows.start - 1;
        let reverse = self.section == LocalChangesSection::Staged;
        let patch = hunk.partial_patch(&[line], reverse);
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
                .unstage_paths(&paths, self.amend_base.as_deref()),
        }
    }

    // Reloads the changes, keeping each section's selection on the same path if it still exists,
    // otherwise on the same position, like lazygit does after staging.
    fn reload(&mut self) {
        let selected_path = self.selected_path();

        self.changes = self.load_changes();
        self.operation = self.repository.operation_in_progress();
        self.prepare_message();
        self.sections[0]
            .tree
            .set_tree(file_tree(&self.changes.unstaged));
        self.sections[1]
            .tree
            .set_tree(file_tree(&self.changes.staged));

        let focus = self.focus;
        let selected_hunk = self.selected_hunk;
        let selected_line = self.selected_line;
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
            self.select_hunk(selected_hunk);
            if focus == Focus::Lines {
                // continue from the same position, the staged line itself is gone or became context
                let row = self
                    .changed_rows()
                    .find(|&r| r >= selected_line)
                    .or_else(|| self.changed_rows().last());
                if let Some(row) = row {
                    self.focus = Focus::Lines;
                    self.select_line(row);
                }
            }
        }
    }

    fn load_changes(&self) -> LocalChanges {
        let mut changes = self.repository.local_changes();
        if let Some(base) = &self.amend_base {
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

    // Starts typing a search or filter, moving the focus back to the files.
    pub fn start_query(&mut self, kind: QueryKind) {
        self.focus_section(self.section);
        self.query_mut().start(kind);
    }

    pub fn is_typing_query(&self) -> bool {
        self.query().is_editing()
    }

    pub fn query_status_input(&self) -> Option<(String, u16)> {
        self.query().status_input()
    }

    pub fn handle_query_key(&mut self, key: KeyEvent) {
        let change = self.query_mut().handle_key(key);
        self.apply_query_change(change);
    }

    // Clears the search, or else the filter. Returns false when there was neither.
    pub fn clear_query(&mut self) -> bool {
        let change = self.query_mut().clear();
        self.apply_query_change(change);
        change != QueryChange::None
    }

    pub fn search_next(&mut self, forward: bool) {
        let direction = if forward {
            SearchDirection::Next
        } else {
            SearchDirection::Previous
        };
        self.search(direction);
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

    fn move_cursor(&mut self, cursor: usize) {
        let before = self.cursor();
        self.tree_mut().select_index(cursor);
        if self.cursor() != before {
            self.load_diff();
        }
    }

    fn selected_row(&self) -> Option<&FileTreeRow> {
        self.tree().selected()
    }

    fn select_hunk(&mut self, index: usize) {
        let Some(last) = self.diff.hunks.len().checked_sub(1) else {
            return;
        };
        self.selected_hunk = index.min(last);
        // show the whole hunk, or at least its beginning
        let rows = self.diff.hunks[self.selected_hunk].rows.clone();
        self.scroll_diff_to(rows);
    }

    fn select_line(&mut self, row: usize) {
        self.selected_line = row;
        if let Some(index) = self.diff.hunks.iter().position(|h| h.rows.contains(&row)) {
            self.selected_hunk = index;
        }
        self.scroll_diff_to(row..row + 1);
    }

    fn scroll_diff_to(&mut self, rows: Range<usize>) {
        let height = self.diff_height;
        // before the first render the height is unknown
        if height == 0 || rows.start < self.diff_offset || rows.len() > height {
            self.diff_offset = rows.start;
        } else if rows.end > self.diff_offset + height {
            self.diff_offset = rows.end - height;
        }
    }

    fn changed_rows(&self) -> impl DoubleEndedIterator<Item = usize> + '_ {
        self.diff
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.is_change())
            .map(|(i, _)| i)
    }

    fn load_diff(&mut self) {
        let staged = self.section == LocalChangesSection::Staged;
        let base = self.amend_base.as_deref();
        if self.diff_of_picked {
            // untracked files are only shown by diffing each against /dev/null
            let targets = self.target_files();
            let (untracked, tracked): (Vec<&LocalChangeFile>, Vec<&LocalChangeFile>) = targets
                .into_iter()
                .partition(|f| f.status == LocalChangeStatus::Untracked);
            let mut diff =
                self.repository
                    .local_change_diff(&file_paths(&tracked), false, staged, base);
            for file in untracked {
                diff.push_str(&self.repository.local_change_diff(
                    &[&file.path],
                    true,
                    staged,
                    base,
                ));
            }
            self.set_diff(&diff);
            return;
        }
        let diff = self.selected_row().map(|row| {
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
        });
        self.set_diff(&diff.unwrap_or_default());
    }

    fn set_diff(&mut self, diff: &str) {
        let conflicted = self
            .selected_row()
            .and_then(|row| row.file_index)
            .and_then(|i| self.files().get(i))
            .filter(|f| f.status == LocalChangeStatus::Conflicted && !self.diff_of_picked)
            .map(|f| f.path.clone());
        self.conflict = conflicted.map(|path| ConflictView::load(self.repository, &path));
        // the diff or conflict selected before is gone, the commit section stays
        if !self.is_commit_focused() {
            self.focus = Focus::Files;
        }
        self.diff = parse_diff(diff);
        self.selected_hunk = 0;
        self.selected_line = 0;
        self.diff_offset = 0;
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
        let files_width = tree_panel_width(area.width);
        let [files_area, right_area] =
            Layout::horizontal([Constraint::Length(files_width), Constraint::Min(0)]).areas(area);
        let [diff_area, commit_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(COMMIT_AREA_HEIGHT)])
                .areas(right_area);
        let [unstaged_area, staged_area] =
            Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)])
                .areas(files_area);

        self.render_file_section(state, LocalChangesSection::Unstaged, unstaged_area, buf);
        self.render_file_section(state, LocalChangesSection::Staged, staged_area, buf);
        if state.conflict.is_some() {
            self.render_conflict(state, diff_area, buf);
        } else {
            self.render_diff(state, diff_area, buf);
        }
        self.render_commit(state, commit_area, buf);
        if let Some((action, prompt)) = &state.confirm {
            self.render_confirm_popup(*action, prompt, area, buf);
        }
    }
}

// top border, message box with 3 lines and its borders
const COMMIT_AREA_HEIGHT: u16 = 1 + 5;

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
        if section == LocalChangesSection::Staged && state.amend_base.is_some() {
            extras.push("amending".to_string());
        }
        if !picked.is_empty() {
            extras.push(format!("{} picked", picked.len()));
        }
        extras.extend(state.query_indicator(section));
        let label = format!("[{number}] {name} ({})", files.len());
        let block = Block::default()
            .borders(Borders::TOP | Borders::RIGHT)
            .style(Style::default().fg(self.ctx.color_theme.divider_fg))
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

        // keep the cursor visible when the tree is taller than the section
        let height = inner.height as usize;
        let offset = (cursor + 1).saturating_sub(height);

        // the search is highlighted in the section it applies to
        let search = state.query_of(section).search();
        let lines: Vec<Line> = rows
            .iter()
            .skip(offset)
            .take(height)
            .map(|row| {
                let indent = "  ".repeat(row.depth);
                match row.file_index {
                    None => {
                        // everything is expanded while filtering
                        let symbol = if tree.is_open(&row.path) {
                            "▾ "
                        } else {
                            "▸ "
                        };
                        let mut spans = vec![Span::raw(indent), Span::raw(symbol)];
                        spans.extend(highlighted_name(&row.name, search, &self.ctx.color_theme));
                        Line::from(spans)
                    }
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
        let block = Block::default()
            .borders(Borders::TOP)
            .style(Style::default().fg(self.ctx.color_theme.divider_fg))
            .title(panel_title(
                &title,
                mode,
                self.focused && state.is_diff_focused(),
                &self.ctx.color_theme,
            ))
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        block.render(area, buf);

        let height = inner.height as usize;
        state.diff_height = height;
        let selected = state.selected_row().is_some();
        let count = if selected {
            diff_line_count(&state.diff)
        } else {
            1
        };
        state.diff_offset = state.diff_offset.min(count.saturating_sub(height));
        let offset = state.diff_offset;

        let mut lines = if selected {
            diff_lines(&state.diff, &self.ctx.color_theme, offset..offset + height)
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
            Focus::Lines => state.selected_line..state.selected_line + 1,
            Focus::Hunks => state
                .diff
                .hunks
                .get(state.selected_hunk)
                .map_or(0..0, |hunk| hunk.rows.clone()),
            _ => 0..0,
        };
        let highlight_style = Style::default().bg(self.ctx.color_theme.list_selected_bg);
        for (row, line) in (offset..).zip(lines.iter_mut()) {
            if highlighted.contains(&row) {
                line.style = line.style.patch(highlight_style);
            }
        }

        Paragraph::new(lines)
            .style(Style::default().fg(self.ctx.color_theme.fg))
            .render(inner, buf);
    }

    fn render_commit(&self, state: &LocalChangesState, area: Rect, buf: &mut Buffer) {
        let theme = &self.ctx.color_theme;
        let focused = self.focused && state.is_commit_focused();
        // amending and rebasing have no controls of their own, the title shows them
        let mut title = panel_title("[3] Commit", "", focused, theme).spans;
        if state.amend_base.is_some() {
            title.push(Span::raw(" · amending ").fg(theme.status_warn_fg));
        }
        if let Some(operation) = state.operation {
            let (doing, c) = match operation {
                Operation::Rebase => ("rebasing", "continue"),
                Operation::Merge => ("merging", "commit"),
                Operation::CherryPick => ("cherry-picking", "commit"),
                Operation::Revert => ("reverting", "commit"),
            };
            let text = format!(" · {doing} (c {c}, C abort) ");
            title.push(Span::raw(text).fg(theme.status_warn_fg));
        }
        let block = Block::default()
            .borders(Borders::TOP)
            .style(Style::default().fg(theme.divider_fg))
            .title(Line::from(title))
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        block.render(area, buf);

        let border_fg = if focused {
            theme.accent_fg
        } else {
            theme.divider_fg
        };
        let mut message_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::default().fg(border_fg));
        if state.is_editing() {
            message_block = message_block.title(Span::raw(" editing · Esc to stop ").fg(border_fg));
        }
        let text_area = message_block.inner(inner);
        message_block.render(inner, buf);
        self.render_message(state, text_area, buf);
    }

    fn render_conflict(&self, state: &mut LocalChangesState, area: Rect, buf: &mut Buffer) {
        let Some(conflict) = &state.conflict else {
            return;
        };
        let theme = &self.ctx.color_theme;
        let path = state.selected_path().unwrap_or_default();
        let block = Block::default()
            .borders(Borders::TOP)
            .style(Style::default().fg(theme.divider_fg))
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
            state.diff_offset = state.diff_offset.min(longest.saturating_sub(height));
            let lines: Vec<Line> = lines
                .iter()
                .skip(state.diff_offset)
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
        state.diff_height = height;

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
                Style::default().fg(theme.fg).bg(theme.status_bar_bg)
            };
            Paragraph::new(Line::styled(text, style)).render(tool_area, buf);
        }
    }

    fn render_confirm_popup(
        &self,
        action: ConfirmAction,
        prompt: &str,
        area: Rect,
        buf: &mut Buffer,
    ) {
        let theme = &self.ctx.color_theme;
        // the commit key confirms a commit too
        let commit_key = self
            .ctx
            .keybind
            .keys_for_event(UserEvent::Commit)
            .into_iter()
            .min_by_key(|key| key.chars().count())
            .map(|key| format!("/{}", key.to_lowercase()))
            .unwrap_or_default();
        let (title, confirm_keys, verb, color, warning) = match action {
            ConfirmAction::Discard => (
                "Discard changes",
                "y/enter".to_string(),
                "discard",
                theme.status_error_fg,
                Some("This cannot be undone."),
            ),
            ConfirmAction::Abort(_) => (
                "Abort",
                "y/enter".to_string(),
                "abort",
                theme.status_error_fg,
                Some("This cannot be undone."),
            ),
            ConfirmAction::Commit => (
                "Commit",
                format!("y/enter{commit_key}"),
                "commit",
                theme.accent_fg,
                None,
            ),
        };
        let hint = key_hints(&[(&confirm_keys, verb), ("n/esc", "cancel")], theme);

        // the first line asks, the others tell more
        let mut lines: Vec<Line> = prompt
            .lines()
            .enumerate()
            .map(|(i, line)| {
                let line = Line::raw(line.to_string());
                if i == 0 {
                    line.fg(theme.fg).bold()
                } else {
                    line.fg(theme.fg)
                }
            })
            .collect();
        if let Some(warning) = warning {
            lines.push(Line::raw(warning).fg(theme.status_error_fg));
        }
        lines.push(Line::raw(""));
        lines.push(hint);

        // text plus padding and borders
        let text_width = lines.iter().map(Line::width).max().unwrap_or(0);
        let width = (text_width as u16 + 4).min(area.width);
        let height = (lines.len() as u16 + 3).min(area.height);
        let popup = Rect::new(
            area.x + (area.width.saturating_sub(width)) / 2,
            area.y + (area.height.saturating_sub(height)) / 2,
            width,
            height,
        );
        Clear.render(popup, buf);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::default().fg(color).bg(theme.bg))
            .title(badge(format!(" {title} "), color, theme))
            .padding(Padding::new(1, 1, 1, 0));
        Paragraph::new(lines).block(block).render(popup, buf);
    }

    fn render_message(&self, state: &LocalChangesState, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let theme = &self.ctx.color_theme;
        if state.message.is_empty() && !state.is_editing() {
            Paragraph::new(Line::raw("Commit message").fg(theme.status_input_transient_fg))
                .render(area, buf);
            return;
        }

        // scroll to keep the cursor visible
        let (row, col) = state.message.cursor();
        let (width, height) = (area.width as usize, area.height as usize);
        let row_offset = (row + 1).saturating_sub(height);
        let col_offset = (col + 1).saturating_sub(width);
        let lines: Vec<Line> = state
            .message
            .lines()
            .iter()
            .skip(row_offset)
            .map(|line| Line::raw(line.chars().skip(col_offset).collect::<String>()))
            .collect();
        Paragraph::new(lines).fg(theme.fg).render(area, buf);

        if state.is_editing() {
            let x = area.x + (col - col_offset) as u16;
            let y = area.y + (row - row_offset) as u16;
            buf.set_style(
                Rect::new(x, y, 1, 1),
                Style::default().add_modifier(ratatui::style::Modifier::REVERSED),
            );
        }
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
    }

    fn paths(files: &[LocalChangeFile]) -> Vec<&str> {
        let mut paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        paths.sort_unstable();
        paths
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
        assert_eq!(state.diff.rows[state.selected_line].text, "new A");

        // stage a single line, staying in line mode on the next changed line
        state.toggle_stage()?;
        assert!(staged_diff("a.txt").contains("+new A"));
        assert!(!staged_diff("a.txt").contains("+new B"));
        assert!(unstaged_diff("a.txt").contains("+new B"));
        assert!((state.focus == Focus::Lines));
        assert_eq!(state.diff.rows[state.selected_line].text, "new B");

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
        state.message = TextArea::new("draft");
        assert_eq!(paths(&state.changes.staged), vec!["c.txt"]);

        // 3, down to the checkbox, enter: the last commit is combined with the staged changes
        state.toggle_amend()?;
        assert_eq!(
            paths(&state.changes.staged),
            vec!["a.txt", "b.txt", "c.txt"]
        );
        assert_eq!(state.message.text(), "second\n\nbody");

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
        assert_eq!(state.message.text(), "draft");
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
        state.message = TextArea::new("first change");
        assert!(state.commit().is_err());
        fs::write(repo_path.join("a.txt"), "a\n")?;
        git.add(&["a.txt"]);
        state.reload();
        state.message = TextArea::new("  \n");
        assert!(state.commit().is_err());

        // c with an empty message starts typing it instead
        state.message = TextArea::default();
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
        state.message = TextArea::new("typed");
        state.enter()?;
        assert!(state.is_confirming_commit());
        state.handle_confirm_key(KeyEvent::from(KeyCode::Esc))?;
        assert!(!state.is_confirming());
        assert!(state.is_commit_focused());
        state.back();

        // c with a message asks to confirm the commit, showing it
        state.message = TextArea::new("first change\n\nbody");
        state.commit_or_continue()?;
        assert!(state.is_confirming_commit());
        let prompt = state.confirm_prompt().unwrap();
        assert!(prompt.starts_with("Commit to master"), "{prompt}");
        assert!(prompt.contains("  first change\n  \n  body"), "{prompt}");
        assert!(prompt.contains("1 staged file:\n  A a.txt"), "{prompt}");
        assert!(!state.take_needs_refresh());
        state.confirm()?;
        assert!(state.take_needs_refresh());
        assert!(!state.take_needs_refresh());
        assert_eq!(
            git_stdout(repo_path, &["log", "-1", "--format=%B"]),
            "first change\n\nbody"
        );
        assert!(state.changes.staged.is_empty());
        assert!(state.message.is_empty());

        // amend with another staged file, keeping the message of HEAD
        fs::write(repo_path.join("b.txt"), "b\n")?;
        git.add(&["b.txt"]);
        state.reload();
        state.toggle_amend()?;
        state.commit()?;
        assert_eq!(git_stdout(repo_path, &["rev-list", "--count", "HEAD"]), "2");
        assert_eq!(
            git_stdout(repo_path, &["log", "-1", "--format=%s"]),
            "first change"
        );
        assert_eq!(
            git_stdout(repo_path, &["show", "--name-only", "--format=", "HEAD"]),
            "a.txt\nb.txt"
        );
        assert!(state.amend_base.is_none());

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
        state.handle_confirm_key(key(KeyCode::Char('n')))?;
        assert!(!state.is_confirming());
        assert_eq!(fs::read_to_string(repo_path.join("a.txt"))?, modified);

        // discard the first hunk
        state.request_discard();
        state.handle_confirm_key(key(KeyCode::Char('y')))?;
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
        state.handle_confirm_key(key(KeyCode::Char('y')))?;
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
        state.handle_confirm_key(key(KeyCode::Char('y')))?;
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
        state.handle_confirm_key(key(KeyCode::Char('y')))?;
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
        assert!(state.commit_or_continue().is_err());
        assert_eq!(state.operation, Some(Operation::Rebase));

        // resolve, stage, then c continues instead of committing
        fs::write(repo_path.join("a.txt"), "resolved\n")?;
        state.reload();
        select_row(&mut state, "a.txt");
        state.toggle_stage()?;
        state.commit_or_continue()?;
        assert!(state.take_needs_refresh());
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
        state.handle_confirm_key(key(KeyCode::Char('n')))?;
        assert_eq!(state.operation, Some(Operation::Rebase));

        state.request_abort()?;
        state.handle_confirm_key(key(KeyCode::Char('y')))?;
        assert!(state.take_needs_refresh());
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
    fn test_merge_conflict() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let _git = diverged_branches(repo_path);
        git_stdout(repo_path, &["merge", "feature"]);

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        assert_eq!(state.operation, Some(Operation::Merge));
        // git's message, without its comment lines
        assert_eq!(state.message.text(), "Merge branch 'feature'");
        assert!(state.toggle_amend().is_err());

        resolve_and_stage(&mut state, repo_path);
        state.commit_or_continue()?;
        state.confirm()?;
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
        assert_eq!(state.message.text(), "feature change");

        resolve_and_stage(&mut state, repo_path);
        state.commit_or_continue()?;
        state.confirm()?;
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
            .message
            .text()
            .starts_with("Revert \"feature change\""));

        state.request_abort()?;
        state.handle_confirm_key(key(KeyCode::Char('y')))?;
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
        state.move_conflict_item(ConflictMove::Down);
        state.enter()?;
        assert_eq!(
            state.take_external_request(),
            Some(ExternalRequest::MergeTool("a.txt".into()))
        );
        state.move_conflict_item(ConflictMove::Down);
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
        state.move_conflict_item(ConflictMove::Right);
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
