use std::{
    collections::{BTreeSet, HashSet},
    ops::Range,
    path::PathBuf,
    rc::Rc,
};

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
    git::{ConflictSide, LocalChangeFile, LocalChangeStatus, LocalChanges, Operation, Repository},
    widget::{
        conflict::ConflictView,
        diff::{diff_lines, parse_diff, ParsedDiff},
        file_tree::{
            ancestors, build_rooted_file_tree_rows, find_match, find_next, FileTreeRow,
            SearchDirection, ROOT_PATH,
        },
        style::{key_hints, mark_selected_row, panel_title},
        text_area::TextArea,
        tree_query::{highlighted_name, path_matches, QueryChange, QueryKind, TreeQuery},
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
}

// The commit section's contents, kept across refreshes.
#[derive(Debug, Default, Clone)]
pub struct CommitDraft {
    message: TextArea,
    amend_base: Option<String>,
    message_before_amend: Option<TextArea>,
}

#[derive(Debug)]
pub struct LocalChangesState<'a> {
    repository: &'a Repository,
    // the paths of local changes are relative to it
    toplevel: PathBuf,
    changes: LocalChanges,
    unstaged_rows: Vec<FileTreeRow>,
    staged_rows: Vec<FileTreeRow>,
    section: LocalChangesSection,
    unstaged_cursor: usize,
    staged_cursor: usize,
    // directories collapsed by the user, all are expanded otherwise
    unstaged_collapsed: HashSet<String>,
    staged_collapsed: HashSet<String>,
    // the search and filter of each section, applying to it only
    unstaged_query: TreeQuery,
    staged_query: TreeQuery,
    // paths of the rows picked with `m` in each section, which actions apply to instead of the cursor
    unstaged_picked: BTreeSet<String>,
    staged_picked: BTreeSet<String>,
    // whether the diff shows the picked files rather than the row under the cursor
    diff_of_picked: bool,
    // false while the page is only previewed from the refs list
    focused: bool,
    // true while selecting hunks (or lines) in the diff instead of files
    diff_focused: bool,
    // true while selecting single lines in the diff instead of hunks
    line_mode: bool,
    diff: ParsedDiff,
    selected_hunk: usize,
    // index into `diff.rows`, always an added or removed line while in line mode
    selected_line: usize,
    diff_offset: usize,
    diff_height: usize,

    commit_focused: bool,
    // true while typing the commit message
    editing: bool,
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
    conflict_focused: bool,
    conflict_item: ConflictItem,
    // an editor or merge tool for the view to run, see `take_external_request`
    external_request: Option<ExternalRequest>,
    // a rebase, merge, cherry-pick or revert stopped and waits to be finished (or aborted)
    operation: Option<Operation>,
    // whether git's prepared message was put in the message box for the operation in progress
    message_prepared: bool,
}

impl<'a> LocalChangesState<'a> {
    pub fn load(repository: &'a Repository) -> Self {
        let changes = repository.local_changes();
        let unstaged_rows = build_rows(&changes.unstaged, &HashSet::new(), None);
        let staged_rows = build_rows(&changes.staged, &HashSet::new(), None);
        // while only previewed, show whichever section has something to look at
        let section = if unstaged_rows.is_empty() && !staged_rows.is_empty() {
            LocalChangesSection::Staged
        } else {
            LocalChangesSection::Unstaged
        };
        let mut state = Self {
            repository,
            toplevel: repository.toplevel(),
            changes,
            unstaged_rows,
            staged_rows,
            section,
            unstaged_cursor: 0,
            unstaged_collapsed: HashSet::new(),
            staged_collapsed: HashSet::new(),
            unstaged_query: TreeQuery::default(),
            staged_query: TreeQuery::default(),
            unstaged_picked: BTreeSet::new(),
            staged_picked: BTreeSet::new(),
            diff_of_picked: false,
            staged_cursor: 0,
            focused: false,
            diff_focused: false,
            line_mode: false,
            diff: ParsedDiff::default(),
            selected_hunk: 0,
            selected_line: 0,
            diff_offset: 0,
            diff_height: 0,
            commit_focused: false,
            editing: false,
            message: TextArea::default(),
            amend_base: None,
            message_before_amend: None,
            needs_refresh: false,
            stash_input: None,
            confirm: None,
            conflict: None,
            conflict_focused: false,
            conflict_item: ConflictItem::Existing,
            external_request: None,
            operation: repository.operation_in_progress(),
            message_prepared: false,
        };
        state.load_diff();
        state.prepare_message();
        state
    }

    pub fn count(&self) -> usize {
        self.changes.count()
    }

    pub fn section(&self) -> LocalChangesSection {
        self.section
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        if !focused {
            self.leave_diff();
            self.commit_focused = false;
            self.editing = false;
        }
    }

    pub fn focus_section(&mut self, section: LocalChangesSection) {
        self.leave_diff();
        self.commit_focused = false;
        self.editing = false;
        if self.section != section {
            self.section = section;
            self.load_diff();
        }
    }

    pub fn selected_path(&self) -> Option<String> {
        self.selected_row().map(|row| row.path.clone())
    }

    // Restores the section and row selected before a refresh, if the path still has changes.
    pub fn reset_selection(&mut self, section: LocalChangesSection, path: Option<String>) {
        self.section = section;
        if let Some(path) = path {
            if let Some(pos) = self.rows().iter().position(|row| row.path == path) {
                *self.cursor_mut() = pos;
            }
        }
        self.load_diff();
    }

    pub fn focus_commit(&mut self) {
        self.commit_focused = true;
        self.leave_diff();
    }

    pub fn is_editing(&self) -> bool {
        self.editing
    }

    // Handles a key typed into the commit message; Esc stops editing.
    pub fn handle_text_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.editing = false;
        } else {
            self.message.handle_key(key);
        }
    }

    pub fn is_commit_focused(&self) -> bool {
        self.commit_focused
    }

    // Continues the rebase when one is in progress; otherwise commits (which also finishes a merge,
    // cherry-pick or revert), or starts typing the message when there is none yet.
    pub fn commit_or_continue(&mut self) -> Result<(), String> {
        if let Some(operation) = self.operation.filter(|o| o.continues_without_commit()) {
            let result = self.repository.continue_operation(operation);
            if result.is_ok() {
                self.needs_refresh = true;
            }
            self.reload();
            return result;
        }
        if self.message.text().trim().is_empty() {
            self.focus_commit();
            self.editing = true;
            return Ok(());
        }
        self.commit()
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
        let message = self.message.text();
        if message.trim().is_empty() {
            return Err("Commit message is empty".into());
        }
        let amend = self.amend_base.is_some();
        // a merge can be committed without changes, when its resolution keeps one side
        if !amend && self.changes.staged.is_empty() && self.operation.is_none() {
            return Err("Nothing staged to commit".into());
        }
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

    // Returns true once after a commit or stash was made.
    pub fn take_needs_refresh(&mut self) -> bool {
        std::mem::take(&mut self.needs_refresh)
    }

    // Picks or unpicks the row under the cursor, for actions on several files.
    pub fn toggle_pick(&mut self) {
        if self.diff_focused || self.commit_focused {
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
        match self.section {
            LocalChangesSection::Unstaged => &self.unstaged_picked,
            LocalChangesSection::Staged => &self.staged_picked,
        }
    }

    fn picked_mut(&mut self) -> &mut BTreeSet<String> {
        match self.section {
            LocalChangesSection::Unstaged => &mut self.unstaged_picked,
            LocalChangesSection::Staged => &mut self.staged_picked,
        }
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
            .filter(|f| self.picked().iter().any(|p| covers(p, &f.path)))
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
        if self.commit_focused
            || self.conflict_focused
            || self.section != LocalChangesSection::Unstaged
        {
            return;
        }
        let Some(row) = self.selected_row() else {
            return;
        };
        let path = if !self.picked().is_empty() && !self.diff_focused {
            picked_files_label(self.target_files().len())
        } else if row.is_root() {
            "all files".to_string()
        } else {
            row.path.clone()
        };
        let target = if self.line_mode {
            "this line of"
        } else if self.diff_focused {
            "this hunk of"
        } else {
            "all changes to"
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
            KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                let Some((action, _)) = self.confirm.take() else {
                    return Ok(());
                };
                let result = match action {
                    ConfirmAction::Discard => {
                        let picked_files = !self.diff_focused;
                        let result = self.discard();
                        if picked_files && result.is_ok() {
                            self.clear_picks();
                        }
                        result
                    }
                    ConfirmAction::Abort(operation) => {
                        let result = self.repository.abort_operation(operation);
                        if result.is_ok() {
                            self.needs_refresh = true;
                        }
                        result
                    }
                };
                self.reload();
                result
            }
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                self.confirm = None;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    // Unstaged changes are between the index and the working tree, so discarding reverts the working tree.
    fn discard(&self) -> Result<(), String> {
        if self.line_mode || self.diff_focused {
            let Some(hunk) = self.diff.hunks.get(self.selected_hunk) else {
                return Ok(());
            };
            let patch = if self.line_mode {
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

    pub fn restore_commit_draft(&mut self, draft: CommitDraft) {
        self.message = draft.message;
        self.amend_base = draft.amend_base;
        self.message_before_amend = draft.message_before_amend;
        if self.amend_base.is_some() {
            self.reload();
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
        if self.commit_focused {
            return Ok(());
        }
        let Some(row) = self.selected_row() else {
            return Ok(());
        };
        let Some(file) = row.file_index.and_then(|i| self.files().get(i)) else {
            return Err("Select a file to edit".into());
        };
        let path = self.toplevel.join(&file.path);
        if !path.exists() {
            return Err(format!("{} does not exist in the working tree", file.path));
        }
        self.external_request = Some(ExternalRequest::Editor(path));
        Ok(())
    }

    pub fn toplevel(&self) -> &std::path::Path {
        &self.toplevel
    }

    pub fn take_external_request(&mut self) -> Option<ExternalRequest> {
        self.external_request.take()
    }

    pub fn is_conflict_focused(&self) -> bool {
        self.conflict_focused
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
                self.external_request = Some(ExternalRequest::Editor(self.toplevel.join(path)));
                return Ok(());
            }
        };
        let result = self.repository.resolve_conflict(&path, side);
        self.conflict_focused = false;
        self.reload();
        result
    }

    // Goes one level deeper: files -> hunks -> lines of the selected hunk.
    // Does nothing when the diff has no hunk to select.
    // In the commit section, starts editing the message or toggles amend.
    pub fn enter(&mut self) -> Result<(), String> {
        if self.commit_focused {
            self.editing = true;
        } else if self.conflict_focused {
            self.activate_conflict_item()?;
        } else if self.conflict.is_some() && !self.diff_focused && self.picked().is_empty() {
            // a conflicted file is resolved from the conflict panel instead of its diff
            self.conflict_focused = true;
            self.conflict_item = ConflictItem::Existing;
        } else if !self.diff_focused {
            // with picked rows, the diff shows all their files to stage hunks and lines from
            if !self.picked().is_empty() {
                self.diff_of_picked = true;
                self.load_diff();
            }
            if !self.diff.hunks.is_empty() {
                self.diff_focused = true;
                self.select_hunk(0);
            }
        } else if !self.line_mode {
            let hunk_rows = self.diff.hunks[self.selected_hunk].rows.clone();
            if let Some(row) = hunk_rows
                .into_iter()
                .find(|&r| self.diff.rows[r].is_change())
            {
                self.line_mode = true;
                self.select_line(row);
            }
        }
        Ok(())
    }

    // Goes one level back: lines -> hunks -> files, or from the commit section to the files.
    // Returns false when already at the files.
    pub fn back(&mut self) -> bool {
        if self.editing {
            self.editing = false;
            true
        } else if self.commit_focused {
            self.commit_focused = false;
            true
        } else if self.conflict_focused {
            self.conflict_focused = false;
            true
        } else if self.line_mode {
            self.line_mode = false;
            // the cursor may have moved into another hunk
            self.select_hunk(self.selected_hunk);
            true
        } else if self.diff_focused {
            self.leave_diff();
            true
        } else {
            false
        }
    }

    // Moves the focus out of the diff, which shows the row under the cursor again.
    fn leave_diff(&mut self) {
        self.diff_focused = false;
        self.line_mode = false;
        self.conflict_focused = false;
        if std::mem::take(&mut self.diff_of_picked) {
            self.load_diff();
        }
    }

    pub fn select_next(&mut self) {
        if self.line_mode {
            let row = self.changed_rows().find(|&r| r > self.selected_line);
            if let Some(row) = row {
                self.select_line(row);
            }
        } else if self.diff_focused {
            self.select_hunk(self.selected_hunk + 1);
        } else {
            self.move_cursor(self.cursor() + 1);
        }
    }

    pub fn select_prev(&mut self) {
        if self.line_mode {
            let row = self.changed_rows().rfind(|&r| r < self.selected_line);
            if let Some(row) = row {
                self.select_line(row);
            }
        } else if self.diff_focused {
            self.select_hunk(self.selected_hunk.saturating_sub(1));
        } else {
            self.move_cursor(self.cursor().saturating_sub(1));
        }
    }

    pub fn select_first(&mut self) {
        if self.line_mode {
            let row = self.changed_rows().next();
            if let Some(row) = row {
                self.select_line(row);
            }
        } else if self.diff_focused {
            self.select_hunk(0);
        } else {
            self.move_cursor(0);
        }
    }

    pub fn select_last(&mut self) {
        if self.line_mode {
            let row = self.changed_rows().last();
            if let Some(row) = row {
                self.select_line(row);
            }
        } else if self.diff_focused {
            self.select_hunk(usize::MAX);
        } else {
            self.move_cursor(usize::MAX);
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
        if self.commit_focused || self.conflict_focused {
            return Ok(());
        }
        let result = if self.line_mode {
            self.toggle_stage_line()
        } else if self.diff_focused {
            self.toggle_stage_hunk()
        } else {
            let result = self.toggle_stage_row();
            if result.is_ok() {
                self.clear_picks();
            }
            result
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
        match self.section {
            LocalChangesSection::Unstaged => self.repository.stage_paths(&paths),
            LocalChangesSection::Staged => {
                // a staged rename is only fully unstaged together with its source path
                paths.extend(targets.iter().filter_map(|f| f.orig_path.as_deref()));
                self.repository
                    .unstage_paths(&paths, self.amend_base.as_deref())
            }
        }
    }

    // Reloads the changes, keeping each section's selection on the same path if it still exists,
    // otherwise on the same position, like lazygit does after staging.
    fn reload(&mut self) {
        let unstaged_path = self
            .unstaged_rows
            .get(self.unstaged_cursor)
            .map(|r| r.path.clone());
        let staged_path = self
            .staged_rows
            .get(self.staged_cursor)
            .map(|r| r.path.clone());
        let selected_path = self.selected_path();

        self.changes = self.load_changes();
        self.operation = self.repository.operation_in_progress();
        self.prepare_message();
        self.unstaged_rows = build_rows(
            &self.changes.unstaged,
            &self.unstaged_collapsed,
            self.unstaged_query.filter(),
        );
        self.staged_rows = build_rows(
            &self.changes.staged,
            &self.staged_collapsed,
            self.staged_query.filter(),
        );
        self.unstaged_cursor =
            restore_cursor(&self.unstaged_rows, unstaged_path, self.unstaged_cursor);
        self.staged_cursor = restore_cursor(&self.staged_rows, staged_path, self.staged_cursor);

        let diff_focused = self.diff_focused;
        let line_mode = self.line_mode;
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
        if diff_focused && same_source && !self.diff.hunks.is_empty() {
            self.diff_focused = true;
            self.select_hunk(selected_hunk);
            if line_mode {
                // continue from the same position, the staged line itself is gone or became context
                let row = self
                    .changed_rows()
                    .find(|&r| r >= selected_line)
                    .or_else(|| self.changed_rows().last());
                if let Some(row) = row {
                    self.line_mode = true;
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
                let path = self.selected_path();
                let rows = build_rows(self.files(), self.collapsed(), self.query().filter());
                let cursor = restore_cursor(&rows, None, self.cursor());
                match self.section {
                    LocalChangesSection::Unstaged => self.unstaged_rows = rows,
                    LocalChangesSection::Staged => self.staged_rows = rows,
                }
                *self.cursor_mut() = cursor;
                self.select_path_or_keep(path);
            }
        }
    }

    // Selects the match of the search in the current section, expanding the directories hiding it.
    fn search(&mut self, direction: SearchDirection) {
        let Some(query) = self.query().search().map(String::from) else {
            return;
        };
        let all_rows = build_rows(self.files(), &HashSet::new(), self.query().filter());
        let current = self.selected_path();
        let path = find_match(&all_rows, current.as_deref(), &query, direction);
        self.reveal(path);
    }

    // Moves the cursor to the next (or previous) picked row, wrapping around.
    pub fn select_next_pick(&mut self, forward: bool) {
        if self.diff_focused || self.commit_focused || self.conflict_focused {
            return;
        }
        let direction = if forward {
            SearchDirection::Next
        } else {
            SearchDirection::Previous
        };
        let all_rows = build_rows(self.files(), &HashSet::new(), self.query().filter());
        let current = self.selected_path();
        let picked = self.picked();
        let path = find_next(&all_rows, current.as_deref(), direction, |row| {
            picked.contains(&row.path)
        });
        self.reveal(path);
    }

    // Selects `path`, expanding the directories hiding it.
    fn reveal(&mut self, path: Option<String>) {
        let Some(path) = path else {
            return;
        };
        for dir in ancestors(&path) {
            self.collapsed_mut().remove(&dir);
        }
        self.rebuild_rows();
        self.select_path_or_keep(Some(path));
    }

    // Moves the cursor to `path` if it is shown, reloading the diff when the selection changed.
    fn select_path_or_keep(&mut self, path: Option<String>) {
        let before = self.selected_path();
        if let Some(pos) = path.and_then(|p| self.rows().iter().position(|r| r.path == p)) {
            *self.cursor_mut() = pos;
        }
        if self.selected_path() != before || self.diff.raw.is_empty() {
            self.load_diff();
        }
    }

    // The search and filter, for the title of `section`.
    fn query_indicator(&self, section: LocalChangesSection) -> Option<String> {
        let query = self.query_of(section);
        let parts: Vec<String> = [
            query.search().map(|s| format!("/{s}")),
            query.filter().map(|f| format!("filter: {f}")),
        ]
        .into_iter()
        .flatten()
        .collect();
        (!parts.is_empty()).then(|| parts.join("  "))
    }

    pub fn expand(&mut self) {
        if let Some(row) = self
            .files_focused_dir()
            .filter(|r| self.collapsed().contains(&r.path))
        {
            let path = row.path.clone();
            self.collapsed_mut().remove(&path);
            self.rebuild_rows();
        }
    }

    // Collapses the selected directory, or moves to the parent directory when it is not open.
    pub fn collapse(&mut self) {
        if self.diff_focused || self.commit_focused {
            return;
        }
        let Some(row) = self.selected_row() else {
            return;
        };
        if row.is_dir() && !self.collapsed().contains(&row.path) {
            let path = row.path.clone();
            self.collapsed_mut().insert(path);
            self.rebuild_rows();
        } else if !row.is_root() {
            let parent = match row.path.rsplit_once('/') {
                Some((parent, _)) => parent.to_string(),
                None => ROOT_PATH.to_string(),
            };
            if let Some(pos) = self.rows().iter().position(|r| r.path == parent) {
                self.move_cursor(pos);
            }
        }
    }

    fn files_focused_dir(&self) -> Option<&FileTreeRow> {
        if self.diff_focused || self.commit_focused {
            return None;
        }
        self.selected_row().filter(|r| r.is_dir())
    }

    fn collapsed(&self) -> &HashSet<String> {
        match self.section {
            LocalChangesSection::Unstaged => &self.unstaged_collapsed,
            LocalChangesSection::Staged => &self.staged_collapsed,
        }
    }

    // The search and filter of the focused section.
    fn query(&self) -> &TreeQuery {
        self.query_of(self.section)
    }

    fn query_mut(&mut self) -> &mut TreeQuery {
        match self.section {
            LocalChangesSection::Unstaged => &mut self.unstaged_query,
            LocalChangesSection::Staged => &mut self.staged_query,
        }
    }

    fn query_of(&self, section: LocalChangesSection) -> &TreeQuery {
        match section {
            LocalChangesSection::Unstaged => &self.unstaged_query,
            LocalChangesSection::Staged => &self.staged_query,
        }
    }

    fn collapsed_mut(&mut self) -> &mut HashSet<String> {
        match self.section {
            LocalChangesSection::Unstaged => &mut self.unstaged_collapsed,
            LocalChangesSection::Staged => &mut self.staged_collapsed,
        }
    }

    // Rebuilds the rows of the current section after expanding or collapsing, keeping the selection.
    fn rebuild_rows(&mut self) {
        let path = self.selected_path();
        let filter = self.query().filter();
        let rows = match self.section {
            LocalChangesSection::Unstaged => {
                build_rows(&self.changes.unstaged, &self.unstaged_collapsed, filter)
            }
            LocalChangesSection::Staged => {
                build_rows(&self.changes.staged, &self.staged_collapsed, filter)
            }
        };
        let cursor = restore_cursor(&rows, path, self.cursor());
        match self.section {
            LocalChangesSection::Unstaged => self.unstaged_rows = rows,
            LocalChangesSection::Staged => self.staged_rows = rows,
        }
        *self.cursor_mut() = cursor;
    }

    fn rows(&self) -> &[FileTreeRow] {
        match self.section {
            LocalChangesSection::Unstaged => &self.unstaged_rows,
            LocalChangesSection::Staged => &self.staged_rows,
        }
    }

    fn files(&self) -> &[LocalChangeFile] {
        match self.section {
            LocalChangesSection::Unstaged => &self.changes.unstaged,
            LocalChangesSection::Staged => &self.changes.staged,
        }
    }

    fn cursor(&self) -> usize {
        match self.section {
            LocalChangesSection::Unstaged => self.unstaged_cursor,
            LocalChangesSection::Staged => self.staged_cursor,
        }
    }

    fn cursor_mut(&mut self) -> &mut usize {
        match self.section {
            LocalChangesSection::Unstaged => &mut self.unstaged_cursor,
            LocalChangesSection::Staged => &mut self.staged_cursor,
        }
    }

    fn move_cursor(&mut self, cursor: usize) {
        let cursor = cursor.min(self.rows().len().saturating_sub(1));
        if self.cursor() != cursor {
            *self.cursor_mut() = cursor;
            self.load_diff();
        }
    }

    fn selected_row(&self) -> Option<&FileTreeRow> {
        self.rows().get(self.cursor())
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
        self.conflict =
            conflicted.map(|path| ConflictView::load(self.repository, &self.toplevel, &path));
        self.conflict_focused = false;
        self.diff = parse_diff(diff);
        self.diff_focused = false;
        self.line_mode = false;
        self.selected_hunk = 0;
        self.selected_line = 0;
        self.diff_offset = 0;
    }
}

// While filtering, only matching files are shown, with all directories expanded.
fn build_rows(
    files: &[LocalChangeFile],
    collapsed: &HashSet<String>,
    filter: Option<&str>,
) -> Vec<FileTreeRow> {
    build_rooted_file_tree_rows(
        files.iter().map(|f| f.path.as_str()),
        |path| filter.is_some() || !collapsed.contains(path),
        |path| filter.is_none_or(|f| path_matches(path, f)),
    )
}

fn picked_files_label(count: usize) -> String {
    if count == 1 {
        "1 picked file".into()
    } else {
        format!("{count} picked files")
    }
}

// Whether the picked row at `picked` contains the file at `path`.
fn covers(picked: &str, path: &str) -> bool {
    picked == ROOT_PATH
        || picked == path
        || path
            .strip_prefix(picked)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn file_paths<'f>(files: &[&'f LocalChangeFile]) -> Vec<&'f str> {
    files.iter().map(|f| f.path.as_str()).collect()
}

fn restore_cursor(rows: &[FileTreeRow], path: Option<String>, cursor: usize) -> usize {
    path.and_then(|path| rows.iter().position(|row| row.path == path))
        .unwrap_or(cursor)
        .min(rows.len().saturating_sub(1))
}

pub struct LocalChangesPanel<'a> {
    ctx: Rc<AppContext>,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl LocalChangesPanel<'_> {
    pub fn new(ctx: Rc<AppContext>) -> Self {
        Self {
            ctx,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<'a> StatefulWidget for LocalChangesPanel<'a> {
    type State = LocalChangesState<'a>;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let files_width = (area.width / 3).clamp(20, 50).min(area.width);
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
        let picked = match section {
            LocalChangesSection::Unstaged => &state.unstaged_picked,
            LocalChangesSection::Staged => &state.staged_picked,
        };
        let (number, name, files, rows, cursor) = match section {
            LocalChangesSection::Unstaged => (
                1,
                "Unstaged",
                &state.changes.unstaged,
                &state.unstaged_rows,
                state.unstaged_cursor,
            ),
            LocalChangesSection::Staged => (
                2,
                "Staged",
                &state.changes.staged,
                &state.staged_rows,
                state.staged_cursor,
            ),
        };
        let active = state.section == section;
        let focused = active && state.focused && !state.diff_focused && !state.commit_focused;
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

        // the search is highlighted in the section it applies to
        let search = state.query_of(section).search();
        let lines: Vec<Line> = rows
            .iter()
            .map(|row| {
                let indent = "  ".repeat(row.depth);
                match row.file_index {
                    None => {
                        let collapsed = match section {
                            LocalChangesSection::Unstaged => &state.unstaged_collapsed,
                            LocalChangesSection::Staged => &state.staged_collapsed,
                        };
                        // everything is expanded while filtering
                        let symbol = if state.query_of(section).filter().is_none()
                            && collapsed.contains(&row.path)
                        {
                            "▸ "
                        } else {
                            "▾ "
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

        // keep the cursor visible when the tree is taller than the section
        let height = inner.height as usize;
        let offset = (cursor + 1).saturating_sub(height);
        let lines: Vec<Line> = lines.into_iter().skip(offset).collect();
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
            mark_selected_row(buf, inner, y, state.focused, &self.ctx.color_theme);
        }
    }

    fn render_diff(&self, state: &mut LocalChangesState, area: Rect, buf: &mut Buffer) {
        let title = match state.selected_row() {
            _ if state.diff_of_picked => picked_files_label(state.target_files().len()),
            Some(row) if row.is_root() => "/".to_string(),
            Some(row) => row.path.clone(),
            None => String::new(),
        };
        let mode = if state.line_mode {
            "select lines"
        } else if state.diff_focused {
            "select hunks"
        } else {
            ""
        };
        let block = Block::default()
            .borders(Borders::TOP)
            .style(Style::default().fg(self.ctx.color_theme.divider_fg))
            .title(panel_title(
                &title,
                mode,
                state.focused && state.diff_focused,
                &self.ctx.color_theme,
            ))
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        block.render(area, buf);

        let mut lines = if state.selected_row().is_none() {
            let message = if state.count() == 0 {
                "No local changes"
            } else {
                "No file selected"
            };
            vec![Line::raw(message).fg(self.ctx.color_theme.status_input_transient_fg)]
        } else {
            diff_lines(&state.diff, &self.ctx.color_theme)
        };

        let highlight_style = Style::default().bg(self.ctx.color_theme.list_selected_bg);
        if state.line_mode {
            if let Some(line) = lines.get_mut(state.selected_line) {
                line.style = line.style.patch(highlight_style);
            }
        } else if state.diff_focused {
            if let Some(hunk) = state.diff.hunks.get(state.selected_hunk) {
                for line in &mut lines[hunk.rows.clone()] {
                    line.style = line.style.patch(highlight_style);
                }
            }
        }

        let height = inner.height as usize;
        state.diff_height = height;
        state.diff_offset = state.diff_offset.min(lines.len().saturating_sub(height));
        let lines: Vec<Line> = lines.into_iter().skip(state.diff_offset).collect();
        Paragraph::new(lines)
            .style(Style::default().fg(self.ctx.color_theme.fg))
            .render(inner, buf);
    }

    fn render_commit(&self, state: &LocalChangesState, area: Rect, buf: &mut Buffer) {
        let theme = &self.ctx.color_theme;
        let focused = state.focused && state.commit_focused;
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
        if state.editing {
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
                Span::raw(" conflicted ")
                    .fg(theme.accent_text_fg)
                    .bg(theme.status_error_fg)
                    .bold(),
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
        let focused = |item| state.focused && state.conflict_focused && state.conflict_item == item;

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
        let (title, verb) = match action {
            ConfirmAction::Discard => ("Discard changes", "discard"),
            ConfirmAction::Abort(_) => ("Abort", "abort"),
        };
        let hint = key_hints(&[("y/enter", verb), ("n/esc", "cancel")], theme);
        // text plus padding and borders
        let width = (prompt.chars().count().max(hint.width()) as u16 + 4).min(area.width);
        let height = 8.min(area.height);
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
            .style(Style::default().fg(theme.status_error_fg).bg(theme.bg))
            .title(
                Span::raw(format!(" {title} "))
                    .fg(theme.accent_text_fg)
                    .bg(theme.status_error_fg)
                    .bold(),
            )
            .padding(Padding::new(1, 1, 1, 0));
        let lines = vec![
            Line::raw(prompt.to_string()).fg(theme.fg).bold(),
            Line::raw("This cannot be undone.").fg(theme.status_error_fg),
            Line::raw(""),
            hint,
        ];
        Paragraph::new(lines).block(block).render(popup, buf);
    }

    fn render_message(&self, state: &LocalChangesState, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let theme = &self.ctx.color_theme;
        if state.message.is_empty() && !state.editing {
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

        if state.editing {
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
    use crate::{git::SortCommit, test_support::git::GitRepository, widget::tree_query::QueryKind};

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
        state.set_focused(true);

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
        assert!(state.diff_focused);
        assert_eq!(state.diff.hunks.len(), 1);

        // staging the last hunk returns to the files
        state.toggle_stage()?;
        assert_eq!(
            paths(&state.changes.unstaged),
            vec!["dir/x.txt", "dir/y.txt"]
        );
        assert!(!state.diff_focused);

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
        state.set_focused(true);
        let staged_diff = |path| repository.local_change_diff(&[path], false, true, None);
        let unstaged_diff = |path| repository.local_change_diff(&[path], false, false, None);

        // files -> hunks -> lines, starting on the first changed line
        select_row(&mut state, "a.txt");
        state.enter()?;
        assert_eq!(state.diff.hunks.len(), 1);
        state.enter()?;
        assert!(state.line_mode);
        assert_eq!(state.diff.rows[state.selected_line].text, "new A");

        // stage a single line, staying in line mode on the next changed line
        state.toggle_stage()?;
        assert!(staged_diff("a.txt").contains("+new A"));
        assert!(!staged_diff("a.txt").contains("+new B"));
        assert!(unstaged_diff("a.txt").contains("+new B"));
        assert!(state.line_mode);
        assert_eq!(state.diff.rows[state.selected_line].text, "new B");

        // staging the last line returns to the files
        state.toggle_stage()?;
        assert!(staged_diff("a.txt").contains("+new B"));
        assert!(!state.diff_focused);
        assert!(!state.line_mode);

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
        assert!(state.diff_focused && !state.line_mode);
        assert!(state.back());
        assert!(!state.diff_focused);
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
        state.set_focused(true);
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
        let draft = state.commit_draft();
        let mut reloaded = LocalChangesState::load(&repository);
        reloaded.restore_commit_draft(draft);
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
        state.set_focused(true);

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

        // c with a message commits
        state.message = TextArea::new("first change\n\nbody");
        state.commit_or_continue()?;
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
        state.set_focused(true);

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
        state.set_focused(true);

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
        state.set_focused(true);
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
        state.set_focused(true);
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
        state.set_focused(true);

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
        assert!(state.diff_focused && state.diff_of_picked);
        assert_eq!(state.diff.hunks.len(), 2);
        state.toggle_stage()?;
        assert_eq!(paths(&state.changes.unstaged), vec!["c.txt"]);
        assert!(state.diff_focused && state.diff_of_picked);
        state.toggle_stage()?;
        assert!(state.changes.unstaged.is_empty());
        assert!(!state.diff_focused && !state.diff_of_picked);

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
        state.set_focused(true);
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
        state.set_focused(true);
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
        state.set_focused(true);
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
        state.set_focused(true);
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
