use std::{path::PathBuf, rc::Rc, time::Instant};

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Flex, Layout, Rect},
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Paragraph, StatefulWidget, Widget},
};

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use rustc_hash::FxHashSet;

use crate::widget::tree_query::Toward;
use crate::{
    background::Job,
    color::ColorTheme,
    context::AppContext,
    event::Sender,
    git::{
        AfterTask, CommitHash, ConflictSide, DiffOf, FilesSnapshot, GitCommand, GitTask, Head,
        LocalChangeFile, LocalChangeStatus, LocalChanges, Operation, PatchDirection, Repository,
    },
    keybind::UserEvent,
    keys::{Availability, KeyList, Keys},
    layout::{fit_size, Direction, Edge, Panel},
    search::{SearchKind, SearchOptions, SearchOutcome},
    widget::diff_pane::register_diff_keys,
    widget::tree_query::type_query,
    widget::{
        conflict::{side_names, ConflictView, RowKind},
        diff::{Step, CONVERTED_DIFF},
        diff_pane::DiffPane,
        file_tree::{
            find_next, path_contains, render_tree, Entry, FileTree, FileTreeRow, FileTreeState,
            TreeCursor, TreeSelection, TreeShape, ROOT_PATH,
        },
        git_dialog::{CommitKind, CommitPreview, GitDialog},
        style::{
            badge, divider, loading_note, mark_selected_bar, panel_title, placeholder,
            render_panel, render_panel_scrollbar, tree_panel_width,
        },
        submodule::{submodule_key, SubmodulePanel},
        text_area::{Caret, Lines, TextArea},
        tree_query::{QueryKind, SearchDirection, TreeQuery},
    },
};

mod commit;
mod conflict;

pub use commit::CommitDraft;
use conflict::{ConflictItem, ConflictPanel};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalChangesSection {
    Unstaged,
    Staged,
}

// What shows beside the files: the diff of the selected row, the sides of a conflicted file (a
// conflicted submodule too, resolving it coming first), or a submodule's own commits in place of
// its gitlink diff.
#[derive(Debug)]
enum Beside<'a> {
    Diff,
    Conflict(ConflictPanel),
    Submodule(Box<SubmodulePanel<'a>>),
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
    // absolute paths
    Editor(Vec<PathBuf>),
    // a path relative to the top of the working tree
    MergeTool(String),
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
    // the submodule panel beside the files, shown in place of the gitlink diff
    Submodule,
    // the panel beside the files: the hunks or lines of the diff (see `DiffPane`), or the
    // choices of the conflict panel shown instead for a conflicted file
    Side,
    // the commit section, on one of its boxes; back in it, the summary is selected
    Commit(CommitField),
    // typing the message into a box of the commit section
    Typing(CommitField),
}

// A file section: its tree of files, search and filter, and picked rows.
#[derive(Debug)]
struct Section {
    // the root row, then the directories (expanded by default) and files
    tree: FileTreeState,
    // the search and filter, applying to this section only
    query: TreeQuery,
    // the rows picked with `v`, which actions apply to instead of the cursor
    picked: TreeSelection,
}

impl Section {
    fn new(files: &[LocalChangeFile]) -> Self {
        Self {
            tree: FileTreeState::new(file_tree(files), TreeShape::Rooted),
            query: TreeQuery::default(),
            picked: TreeSelection::default(),
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
    // what had the keys, put back once the page is loaded again
    focus: Focus,
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
            focus: Focus::Files,
        }
    }
}

#[derive(Debug)]
pub struct LocalChangesState<'a> {
    repository: &'a Repository,
    // what the submodule panel draws its graph with, and the config everywhere
    ctx: Rc<AppContext>,
    // the paths of local changes are relative to it
    changes: LocalChanges,
    // the unstaged and the staged section, see `LocalChangesSection`
    sections: [Section; 2],
    section: LocalChangesSection,
    // where the keys were sent; the diff keeps whether it has them itself, see `focus`
    chosen: Focus,
    // the diff beside the files, its selected hunk or line and scroll (or that of the conflict
    // sides)
    pane: DiffPane,
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
    // the app's, set by the view, to load the files on another thread after a change, the screen
    // keeping up meanwhile; without it, as in the tests, they load right away. See `reload`.
    background: Option<Sender>,
    // the files loading after a change, shown once loaded
    files_loading: Option<Job<LocalChanges>>,
    // when the files last started loading after a change, see `reload_if_changed`
    reloaded: Instant,
    // what shows beside the files, see `prepare_diff`
    beside: Beside<'a>,
    // the submodule panel shown before, keyed by what it shows, to reuse when selected again
    submodule_cache: Option<(String, SubmodulePanel<'a>)>,
}

impl<'a> LocalChangesState<'a> {
    #[cfg(test)]
    pub fn load(repository: &'a Repository) -> Self {
        Self::restore(repository, crate::test_support::ui::test_ctx(), None, None)
    }

    // Loads the page as it was in `snapshot`, loading the diff once, for the selected file: on
    // another thread through `background`, which wakes the app up to show it (the files and the
    // diffs after it load there too), else right away.
    pub fn restore(
        repository: &'a Repository,
        ctx: Rc<AppContext>,
        snapshot: Option<LocalChangesSnapshot>,
        background: Option<Sender>,
    ) -> Self {
        let changes = repository.stage_resolved_conflicts(repository.take_loaded_local_changes());
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
            ctx,
            changes,
            sections,
            section,
            chosen: Focus::Files,
            pane: DiffPane::default(),
            diff_query: TreeQuery::default(),
            draft: CommitDraft::default(),
            request: None,
            diff_stale: false,
            files_stamp,
            background,
            files_loading: None,
            reloaded: Instant::now(),
            beside: Beside::Diff,
            submodule_cache: None,
        };
        let focus = snapshot.as_ref().map(|snapshot| snapshot.focus);
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
        // the loaded diff and the submodule panel show what the focus may be on
        if let Some(focus) = focus {
            state.restore_focus(focus);
        }
        state
    }

    // Puts the keys back where they were: in the diff, the submodule panel or the commit section,
    // a box being typed into staying selected rather than typed into again.
    fn restore_focus(&mut self, focus: Focus) {
        match focus {
            Focus::Files => self.focus_files(),
            Focus::Side => {
                self.focus_files();
                self.pane.enter();
            }
            Focus::Submodule => self.chosen = Focus::Submodule,
            Focus::Commit(field) => self.chosen = Focus::Commit(field),
            Focus::Typing(field) => self.chosen = Focus::Commit(field),
        }
    }

    // What to restore of the page, after a refresh or coming back to it.
    pub fn snapshot(&self) -> LocalChangesSnapshot {
        LocalChangesSnapshot {
            section: self.section,
            selected_path: self.selected_path(),
            commit_draft: Box::new(self.draft.clone()),
            focus: self.focus(),
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

    // The one file an action on a single file works on: the selected file, or the one picked file.
    // A picked directory is not expanded, and several picks leave no one file.
    pub fn single_target_file(&self) -> Option<&str> {
        if self.picked().is_empty() {
            return self.selected_file();
        }
        let path = self.picked().single_path()?;
        self.files()
            .iter()
            .find(|file| file.path == path)
            .map(|file| file.path.as_str())
    }

    pub fn selected_path(&self) -> Option<String> {
        self.selected_row().map(|row| row.path.clone())
    }

    pub fn is_editing(&self) -> bool {
        matches!(self.focus(), Focus::Typing(_))
    }

    // Where the keys go: beside the files, the diff has them while it selects a hunk or line, like
    // in the details, and the conflict panel once entered.
    fn focus(&self) -> Focus {
        match self.chosen {
            Focus::Files | Focus::Side
                if self.conflict().is_none() && self.submodule().is_none() =>
            {
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
        self.focus() == Focus::Side && self.conflict().is_none()
    }

    // Scrolling the submodule panel shown in place of a gitlink diff.
    pub fn is_submodule_focused(&self) -> bool {
        self.focus() == Focus::Submodule
    }

    #[cfg(test)]
    fn is_selecting_lines(&self) -> bool {
        self.is_diff_focused() && self.pane.is_selecting_lines()
    }

    // What `selected_diff_text` is of, see `DiffPane::selected_name`.
    pub fn selected_diff_name(&self) -> &'static str {
        self.pane.selected_name()
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
        let path = self.repository.path().to_path_buf();
        let task = self.take_task().expect("a git command to run");
        let head = crate::git::read_head_commit(&path);
        let result = crate::git::run_task(&path, &task);
        if task.input.is_some() && crate::git::read_head_commit(&path) != head {
            self.clear_draft();
        }
        self.reload_repository();
        result
    }

    // Loads the repository and the page again, as the app does after a git command or a change
    // made outside; the repository is leaked, as tests keep the page.
    #[cfg(test)]
    fn reload_repository(&mut self) {
        let sort = crate::git::SortCommit::Chronological;
        let repository = Repository::load(self.repository.path(), sort, None, false).unwrap();
        *self = Self::restore(
            Box::leak(Box::new(repository)),
            self.ctx.clone(),
            Some(self.snapshot()),
            self.background.clone(),
        );
    }

    // Whether the files are being picked from where the visual selection started.
    pub fn is_visual(&self) -> bool {
        self.tree().is_visual()
    }

    // Starts or ends a visual selection, picking the rows from where it started to the selected
    // one.
    pub fn toggle_visual(&mut self) {
        if self.is_diff_focused() {
            self.pane.toggle_range();
            return;
        }
        if self.is_commit_focused() {
            return;
        }
        let section = self.current_mut();
        section.picked.toggle_visual(&mut section.tree);
    }

    fn sync_visual_picks(&mut self) {
        let section = self.current_mut();
        section.picked.sync_visual(&section.tree);
    }

    // Clears the picked rows of the current section. Returns false when there were none.
    pub fn clear_picks(&mut self) -> bool {
        let section = self.current_mut();
        let had_picks = !section.picked.is_empty();
        section.picked.clear(&mut section.tree);
        had_picks
    }

    fn picked(&self) -> &TreeSelection {
        &self.current().picked
    }

    pub fn picked_paths(&self) -> Vec<String> {
        self.picked().paths()
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
            .filter(|f| self.picked().contains_under(&f.path))
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
        } else if row.is_root() && self.query().filter().is_none() {
            "all files".to_string()
        } else if row.is_root() {
            // the files the filter leaves, see `row_files`
            format!("the {} files filtered", self.target_files().len())
        } else {
            row.path.clone()
        };
        let target = if self.is_diff_focused() {
            format!("this {} of", self.pane.selected_name())
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

    // Asks the view to open the picked files, or the file under the cursor, in the editor.
    pub fn request_edit(&mut self) -> Result<(), String> {
        if self.is_commit_focused() {
            self.edit_message();
            return Ok(());
        }
        let picked = self.picked().clone();
        let files: Vec<&LocalChangeFile> = if picked.is_empty() {
            self.selected_row()
                .and_then(FileTreeRow::file_index)
                .and_then(|i| self.files().get(i))
                .into_iter()
                .collect()
        } else {
            self.files()
                .iter()
                .filter(|file| picked.contains(&file.path))
                .collect()
        };
        // a submodule is a directory, not a file to open in the editor
        let files: Vec<&LocalChangeFile> = files
            .into_iter()
            .filter(|file| !self.repository.is_submodule(&file.path))
            .collect();
        if files.is_empty() {
            return Err("Select a file to edit".into());
        }
        let files: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
        let paths = self.repository.working_tree_paths(&files)?;
        self.request = Some(Request::External(ExternalRequest::Editor(paths)));
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
            !v.changes.staged.is_empty()
                || v.draft.amend.is_some()
                || v.repository.operation_in_progress().is_some()
        };
        let stage = |v: &Self| if unstaged(v) { "stage" } else { "unstage" };
        let what = |v: &Self| v.pane.selected_name();
        // committing, or amending
        let commit_word = |v: &Self| {
            if v.editing_commit() || v.draft.amend.is_some() {
                "amend"
            } else {
                "commit"
            }
            .to_string()
        };

        k.add(
            &[Cancel],
            "clear picks",
            "Back, then clear the picked files, then the filter",
        )
        .when(|v| files(v) && picks(v));
        k.add(
            &[Select],
            "stage",
            "Stage/unstage the file, directory, hunk or line, or the picked files; resolve a conflict",
        )
        .when(row)
        .short(|v| match v.conflict().is_some() && !picks(v) {
            true => "resolve".into(),
            false => stage(v).into(),
        });
        k.add(&[Select], "stage", "Stage/unstage the hunk or line")
            .when(diff)
            .short(|v| format!("{} {}", stage(v), what(v)));
        k.add(
            &[Confirm],
            "select hunks",
            "Select the hunks of the diff, then their lines; resolve a conflict",
        )
        .when(|v| files(v) && !empty(v))
        .short(|v| {
            let resolve = v.conflict().is_some() && !picks(v);
            match (resolve, v.submodule().is_some()) {
                (true, _) => "resolve",
                (false, true) => "submodule",
                (false, false) => "select hunks",
            }
            .into()
        });
        k.add(
            &[Confirm],
            "unstaged",
            "On the empty staged files, go to the unstaged ones",
        )
        .when(|v| files(v) && empty(v) && !unstaged(v));
        register_diff_keys(
            k,
            |v| v.is_diff_focused().then_some(&v.pane),
            |v| v.ctx.diff_context.get(),
        );
        k.add(
            &[Select],
            "choose",
            "Choose the side or the tool of the conflict",
        )
        .when(conflict);
        k.add(
            &[ToggleSelect],
            "select",
            "Select the files from the first to the selected one",
        )
        .when(row)
        .short(|v| {
            if v.is_visual() {
                "end select"
            } else {
                "select"
            }
            .into()
        });
        k.add(
            &[Discard],
            "discard",
            "Discard the unstaged file, directory, hunk or line",
        )
        // `git restore` leaves a submodule's checkout as it is
        .when(|v| (row(v) || diff(v)) && unstaged(v) && v.submodule().is_none());
        // on the empty unstaged files enter commits too, see `enter`; with nothing to commit they
        // say so, unlisted
        let commits = move |v: &Self| commit(v) || (files(v) && empty(v) && unstaged(v));
        const COMMIT: &str =
            "Commit after confirming (finishing a merge, cherry-pick or revert, or going on with a rebase)";
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
        k.add(&[Continue], "continue", "Continue the rebase in progress")
            .when(|v| v.continuing_operation().is_some());
        k.add(
            &[Abort],
            "abort",
            "Abort the rebase, merge, cherry-pick or revert in progress, after confirming",
        )
        .when(|v| v.repository.operation_in_progress().is_some());
        k.add(
            &[Skip],
            "skip",
            "Skip the commit the rebase, cherry-pick or revert stopped at, after confirming",
        )
        .when(|v| v.skippable_operation().is_some());
        k.add(
            &[ToggleAmend],
            "amend",
            "Amend the last commit, or stop amending",
        )
        // during an operation only stopping, see `toggle_amend`; a commit being edited stays amended
        .when(|v| {
            !v.editing_commit()
                && (v.repository.operation_in_progress().is_none() || v.draft.amend.is_some())
        })
        .short(|v| {
            if v.draft.amend.is_some() {
                "stop amending"
            } else {
                "amend"
            }
            .into()
        });
        k.add(
            &[Select, Edit],
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
        .when(move |v| (files(v) || diff(v)) && v.single_target_file().is_some());
        k.add(
            &[Timeline],
            "timeline",
            "Show the commits changing the selected file",
        )
        .when(move |v| (files(v) || diff(v)) && v.single_target_file().is_some());
        k.add(&[Search], "filter", "Filter the files")
            .when(|v| files(v) && !empty(v));
        k.add(&[Search], "search", "Search the diff").when(diff);
        k.add(
            &[ShortCopy],
            "copy path",
            "Copy the path of the file, from the top of the repository",
        )
        .when(row);
        k.add(
            &[FullCopy],
            "copy full path",
            "Copy the absolute path of the file",
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
            "Back, then clear the picked files, then the filter",
        )
        .when(|v| files(v) && !picks(v));
        k.quiet(&[GoToNext, GoToPrevious], "Go to the next/previous match")
            .when(diff);
        k.quiet(&[NextFile], "Next file, staying in the diff")
            .when(|v| files(v) || diff(v));
        k.quiet(&[PreviousFile], "Previous file, staying in the diff")
            .when(|v| files(v) || diff(v));
        k.quiet(
            &[NextFolder],
            "Next folder at the same depth (or a level up), staying in the diff",
        )
        .when(|v| files(v) || diff(v));
        k.quiet(
            &[PreviousFolder],
            "Previous folder at the same depth (or a level up), staying in the diff",
        )
        .when(|v| files(v) || diff(v));
        k.quiet(
            &[NavigateDown, SelectDown, NavigateUp, SelectUp],
            "Select the next/previous file, hunk, line, conflict choice or message box",
        );
        k.quiet(
            &[GoToTop, GoToBottom],
            "Select the first/last file, hunk or line, or the summary/description",
        )
        .when(|v| files(v) || diff(v) || commit(v));
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
            "Scroll the focused panel; from the files, move a page or a line at a time",
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
            return crate::widget::tree_query::typing_keys(self.query_kind());
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

    // Adds the picked files and directories, or the selected one, to .gitignore; returns what to
    // tell.
    pub fn ignore_selected(&mut self) -> Result<String, String> {
        let entries = self.picked().entries(self.tree());
        if entries.is_empty() {
            return Ok(String::new());
        }
        let result = self.repository.ignore_entries(&entries);
        self.reload();
        result
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

    // Goes one level deeper: files -> hunks -> lines of the selected hunk.
    // Does nothing when the diff has no hunk to select.
    // In the commit section, starts editing the message; on a conflicted file, goes into its
    // conflict panel, whose choice space runs, see `select`.
    pub fn enter(&mut self) -> Result<(), String> {
        match self.focus() {
            Focus::Commit(_) | Focus::Typing(_) => self.commit()?,
            Focus::Side => {
                self.pane.enter();
            }
            Focus::Submodule => {}
            // the submodule panel is read on its own, like the diff
            Focus::Files if self.submodule().is_some() => self.chosen = Focus::Submodule,
            Focus::Files if self.picked().is_empty() && self.conflict().is_some() => {
                // a conflicted file is resolved from the conflict panel instead of its diff
                if let Some(conflict) = self.conflict_mut() {
                    conflict.item = ConflictItem::Existing;
                }
                self.chosen = Focus::Side;
            }
            // nothing here to open: on from the unstaged files to committing, from the staged
            // ones back to the unstaged, as nothing staged commits nothing
            Focus::Files if self.files().is_empty() => match self.section {
                LocalChangesSection::Unstaged => {
                    self.focus_commit();
                    self.commit()?;
                }
                LocalChangesSection::Staged => self.focus_section(LocalChangesSection::Unstaged),
            },
            Focus::Files => {
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
            Focus::Side if self.conflict().is_some() => self.chosen = Focus::Files,
            Focus::Submodule => self.chosen = Focus::Files,
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
            (Focus::Submodule, Direction::Down) => self.focus_commit(),
            (
                Focus::Submodule | Focus::Side | Focus::Commit(_) | Focus::Typing(_),
                Direction::Left,
            ) => self.focus_files(),
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

    // Moves the cursor of the files by every key scrolling the list; returns whether the cursor
    // moved.
    pub fn navigate_files(&mut self, event: UserEvent) -> bool {
        let before = self.cursor();
        self.tree_mut().navigate(event);
        let moved = self.cursor() != before;
        if moved {
            self.diff_stale = true;
            self.sync_visual_picks();
        }
        moved
    }

    // Moves the selected line or hunk, else the cursor of the files to `row`.
    fn step(&mut self, step: Step, row: impl Fn(&Self) -> usize) {
        match self.focus() {
            Focus::Side if self.conflict().is_none() => self.pane.step(step),
            Focus::Submodule => {
                if let Some(submodule) = self.submodule_mut() {
                    submodule.step(step);
                }
            }
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

    // Scrolls what has the focus by every key scrolling it: the files or the submodule move, the
    // commit message scrolls, and the diff or the conflict sides scroll. Returns whether `event`
    // was one of them.
    pub fn scroll(&mut self, event: UserEvent) -> bool {
        match self.focus() {
            Focus::Files => self.navigate_files(event),
            Focus::Submodule => self
                .submodule_mut()
                .is_some_and(|submodule| submodule.navigate(event)),
            Focus::Commit(field) => self.draft.field_mut(field).scroll(event),
            Focus::Side => self.pane.scroll(event),
            Focus::Typing(_) => false,
        }
    }

    // Prepares the graph images of the submodule panel, if one is shown, see `CommitListState`.
    pub fn prepare_graph_uploads(&mut self) {
        if let Some(submodule) = self.submodule_mut() {
            submodule.ensure_visible_graph_uploaded();
        }
    }

    // Tells the submodule panel the height its list is drawn in, before its graph images are
    // prepared: only the rows shown are drawn, not every commit in the range.
    pub fn update_submodule_height(&mut self, area: Rect) {
        if self.submodule().is_none() {
            return;
        }
        let diff = local_changes_areas(&self.ctx, area).diff;
        if let Some(submodule) = self.submodule_mut() {
            // the panel's borders and the note line
            submodule.set_list_height(diff.height.saturating_sub(3) as usize);
        }
    }

    // The graph images the submodule panel has queued, to write to the terminal.
    pub fn drain_pending_graph_uploads(&mut self) -> Vec<String> {
        self.submodule_mut()
            .map_or_else(Vec::new, SubmodulePanel::drain_pending_graph_uploads)
    }

    // Staging the unstaged changes, unstaging the staged ones.
    fn patch_direction(&self) -> PatchDirection {
        match self.section {
            LocalChangesSection::Unstaged => PatchDirection::Forward,
            LocalChangesSection::Staged => PatchDirection::Reverse,
        }
    }

    // Space: stages the selected line, hunk, file or directory, or unstages it in the staged
    // section; runs the choice of the conflict panel; types in the commit section.
    pub fn select(&mut self) -> Result<(), String> {
        let result = match self.focus() {
            Focus::Commit(_) | Focus::Typing(_) => {
                self.edit_message();
                return Ok(());
            }
            Focus::Side if let Some(conflict) = self.conflict() => {
                return self.activate_conflict_item(conflict.item);
            }
            // the submodule panel is read-only
            Focus::Submodule => Ok(()),
            Focus::Side => {
                // the patch is made and applied the same way
                let direction = self.patch_direction();
                match self.pane.selected_patch(direction) {
                    Some(patch) => self.repository.apply_patch(&patch, direction),
                    None => Ok(()),
                }
            }
            // like lazygit, a conflicted file is never staged with its markers: it is resolved in
            // the conflict panel, and staged once resolved, see `reload_with`
            Focus::Files if self.picked().is_empty() && self.conflict().is_some() => {
                return self.enter();
            }
            Focus::Files
                if self
                    .target_files()
                    .iter()
                    .any(|f| f.status == LocalChangeStatus::Conflicted) =>
            {
                return Err("Cannot stage files with merge conflicts, resolve them first".into());
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
    // Those from before its own last change are dropped, and wait for one loading after a change.
    pub fn reload_if_changed(&mut self, files: FilesSnapshot) {
        let stale = files.started < self.reloaded || self.files_loading.is_some();
        if !stale && files.stamp != self.files_stamp {
            self.reload_with(files.changes);
        }
    }

    // Loads the files on another thread with the app's `tx`, after a change, see `reload`.
    // Reloads the changes, keeping each section's selection on the same path if it still exists,
    // otherwise on the same position, like lazygit does after staging: on another thread when the
    // view set it up, see `poll_files` and `load_stale_diff`.
    pub fn reload(&mut self) {
        self.reloaded = Instant::now();
        match &self.background {
            Some(tx) => {
                let work = self.repository.local_changes_work();
                self.files_loading = Some(Job::spawn(tx.clone(), work));
            }
            None => self.reload_with(self.repository.local_changes()),
        }
    }

    fn reload_with(&mut self, changes: LocalChanges) {
        let selected_path = self.selected_path();
        let changes = self.repository.stage_resolved_conflicts(changes);

        self.files_stamp = self.repository.files_stamp(&changes);
        self.changes = self.with_amend(changes);
        self.prepare_message();
        self.sync_amend();
        let files = [&self.changes.unstaged, &self.changes.staged];
        for (section, files) in self.sections.iter_mut().zip(files) {
            section.tree.set_tree(file_tree(files));
            section.tree.cancel_visual();
            // the picks with no changes left, discarded or stashed, go: those that are no file nor
            // directory of one, see `path_contains`
            let kept: FxHashSet<&str> = files
                .iter()
                .flat_map(|f| {
                    let path = f.path.as_str();
                    let dirs = path.match_indices('/').map(move |(i, _)| &path[..i]);
                    std::iter::once(path).chain(dirs)
                })
                .collect();
            section
                .picked
                .retain(|p| kept.contains(p) || (p == ROOT_PATH && !files.is_empty()));
        }

        let in_diff = self.is_diff_focused();
        let selection = self.pane.selection();
        self.load_diff();
        // stay in the diff while the same path still has hunks left
        if in_diff && self.selected_path() == selected_path && self.conflict().is_none() {
            self.pane.restore(selection);
        }
    }

    // The files of the current section in `row`, which pass the filter.
    fn row_files(&self, row: &FileTreeRow) -> Vec<&LocalChangeFile> {
        self.files()
            .iter()
            .filter(|f| row.contains(&f.path) && self.tree().keeps(&f.path))
            .collect()
    }

    // Starts typing a search of the diff while it has the focus, going from its selected row, else
    // a filter of the files, moving the focus back to them.
    pub fn start_query(&mut self) {
        if self.is_diff_focused() {
            let from = self.pane.selected_row();
            self.diff_query.start(QueryKind::Search, from);
            return;
        }
        self.focus_section(self.section);
        self.tree_mut().cancel_visual();
        // a filter goes from no row, see `TreeQuery::step_from`
        self.query_mut().start(QueryKind::Filter, 0);
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

    // How the search of the diff went, while it has the focus; the files are only filtered.
    pub fn search_outcome(&self) -> Option<SearchOutcome> {
        let query = self
            .diff_query
            .search()
            .filter(|_| self.is_diff_focused())?;
        self.pane
            .search_outcome(query, self.ctx.search_options.get())
    }

    // The diff is searched as text, the files by name.
    pub fn query_kind(&self) -> SearchKind {
        if self.is_diff_focused() {
            SearchKind::Text
        } else {
            SearchKind::Names
        }
    }

    // Types `key` in the query being typed, see `type_query`: the search of the diff, else the
    // search or the filter of the files.
    pub fn type_query_key(&mut self, key: KeyEvent) {
        let ctx = self.ctx.clone();
        let kind = self.query_kind();
        if self.diff_query.is_editing() {
            if type_query(&ctx, &mut self.diff_query, key, kind) == Some(QueryKind::Search) {
                self.search_diff(SearchDirection::Current, ctx.search_options.get());
            }
            return;
        }
        let change = type_query(&ctx, self.query_mut(), key, kind);
        self.apply_query_change(change, ctx.search_options.get());
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
        !self.is_diff_focused() || self.search_diff(SearchDirection::from(toward), options)
    }

    // Selects the match of the diff search: the changed line it is on, else its hunk. Returns
    // false when there is none.
    fn search_diff(&mut self, direction: SearchDirection, options: SearchOptions) -> bool {
        let Some(query) = self.diff_query.search().map(String::from) else {
            return true;
        };
        let from = self.diff_query.step_from(self.pane.selected_row());
        self.pane.search(&query, from, direction, options)
    }

    // The selected line as in the file, or the selected hunk as a diff, to copy.
    pub fn selected_diff_text(&self) -> Option<String> {
        self.pane.selected_text().filter(|_| self.is_diff_focused())
    }

    // The files are only filtered, see `start_query`.
    fn apply_query_change(&mut self, change: Option<QueryKind>, options: SearchOptions) {
        if change == Some(QueryKind::Filter) {
            // only the focused section is filtered
            let filter = self.query().filter().map(String::from);
            self.update_tree(|tree| tree.set_filter(filter.as_deref(), options));
        }
    }

    // Goes to the next file of the section, skipping directories; from its diff, on in the diff of
    // the next file.
    pub fn select_next_file(&mut self, toward: Toward) -> Result<(), String> {
        let in_diff = self.is_diff_focused();
        if !(in_diff || self.focus() == Focus::Files) {
            return Ok(());
        }
        let direction = SearchDirection::from(toward);
        let rows = self.tree().all_rows();
        let current = self
            .selected_path()
            .and_then(|path| rows.iter().position(|row| row.path == path))
            .unwrap_or(0);
        let path = find_next(&rows, current, direction, |row| !row.is_dir());
        self.chosen = Focus::Files;
        self.update_tree(|tree| tree.reveal(path.as_deref()));
        if in_diff {
            self.enter()?;
        }
        Ok(())
    }

    // Goes to the next folder at the depth of the selected one (of the selected file's), or a level
    // up when none is left at it, round; closed folders stay closed. From the diff, on in the diff
    // of the first file of that folder.
    pub fn select_next_folder(&mut self, toward: Toward) -> Result<(), String> {
        let in_diff = self.is_diff_focused();
        if !(in_diff || self.focus() == Focus::Files) {
            return Ok(());
        }
        let rows = self.tree().all_rows();
        let selected = self
            .selected_path()
            .and_then(|path| rows.iter().position(|row| row.path == path));
        let folder = match selected {
            // the root row, above everything: the first or the last folder at the top
            None => {
                let mut top = rows.iter().filter(|row| row.is_dir() && row.depth == 0);
                match toward {
                    Toward::Next => top.next(),
                    Toward::Previous => top.next_back(),
                }
                .map(|row| row.path.clone())
            }
            Some(selected) => {
                // a file goes from its folder; a file at the top from itself
                let from = (0..=selected)
                    .rev()
                    .find(|&i| rows[i].is_dir() && rows[i].contains(&rows[selected].path))
                    .unwrap_or(selected);
                let anchor = &rows[from];
                find_next(&rows, from, SearchDirection::from(toward), |row| {
                    row.is_dir() && row.depth <= anchor.depth && !anchor.contains(&row.path)
                })
            }
        };
        let Some(folder) = folder else {
            return Ok(());
        };
        let target = match in_diff {
            true => rows
                .iter()
                .find(|row| !row.is_dir() && path_contains(&folder, &row.path))
                .map_or(folder, |file| file.path.clone()),
            false => folder,
        };
        self.chosen = Focus::Files;
        self.update_tree(|tree| tree.reveal(Some(&target)));
        if in_diff {
            self.enter()?;
        }
        Ok(())
    }

    // Applies a tree operation, reloading the diff when the selection changed.
    fn update_tree<R>(&mut self, f: impl FnOnce(&mut FileTreeState) -> R) -> R {
        let before = self.selected_path();
        let result = f(self.tree_mut());
        self.sync_visual_picks();
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
            self.sync_visual_picks();
        }
    }

    // Loads the diff of the selected row, when the cursor moved since it was, or waits for the
    // one loading in the background: before a key acts on it.
    pub fn load_stale_diff(&mut self) {
        if let Some(job) = self.files_loading.take() {
            self.reload_with(job.wait());
        }
        if std::mem::take(&mut self.diff_stale) {
            self.load_diff();
        }
        self.pane.wait_loading(self.repository);
        if let Beside::Submodule(submodule) = &mut self.beside {
            submodule.wait(&self.ctx);
        }
    }

    #[cfg(test)]
    pub fn is_diff_loading(&self) -> bool {
        let submodule = self
            .submodule()
            .and_then(SubmodulePanel::loading_since)
            .is_some();
        self.pane.loading_since().is_some() || self.files_loading.is_some() || submodule
    }

    // Before drawing: starts loading the diff of the row the cursor moved to on another thread,
    // the screen keeping up with the keys meanwhile, or shows it once loaded.
    pub fn load_stale_diff_in_background(&mut self, tx: &Sender) {
        if let Some(changes) = self.files_loading.as_ref().and_then(Job::try_take) {
            self.files_loading = None;
            self.reload_with(changes);
        }
        if std::mem::take(&mut self.diff_stale) && self.prepare_diff() {
            let work = self.diff_work();
            self.pane.load_in_background(tx.clone(), work);
        }
        self.pane.poll_loading(self.repository);
        if let Beside::Submodule(submodule) = &mut self.beside {
            submodule.poll(&self.ctx);
        }
    }

    pub fn selected_row(&self) -> Option<&FileTreeRow> {
        self.tree().selected()
    }

    // On another thread when the view set it up, see `restore`: a big diff would hold up the
    // screen; the one shown before is drawn meanwhile, and a key waits for it, see `load_stale_diff`.
    fn load_diff(&mut self) {
        self.diff_stale = false;
        if self.prepare_diff() {
            let work = self.diff_work();
            match &self.background {
                Some(tx) => self.pane.load_in_background(tx.clone(), work),
                None => self.pane.show_loaded(&work(), self.repository),
            }
        }
    }

    // Shows more or fewer lines around the changes of the selected file, loading its diff again.
    pub fn adjust_diff_context(&mut self, event: UserEvent) {
        if !self.ctx.adjust_diff_context(event) {
            return;
        }
        let selection = self.pane.selection();
        self.load_diff();
        if self.conflict().is_none() {
            self.pane.restore(selection);
        }
    }

    // The diff of the selected row, as work to run.
    fn diff_work(&self) -> Box<dyn FnOnce() -> String + Send> {
        let context = self.ctx.diff_context.get();
        let tracked_of = match self.section {
            LocalChangesSection::Staged => DiffOf::Staged {
                base: self.draft.amend_base(),
            },
            LocalChangesSection::Unstaged => DiffOf::Unstaged,
        };
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
                self.repository.local_change_diff_work(&paths, of, context)
            })
            .unwrap_or_else(|| Box::new(String::new))
    }

    // Shows the conflict or the submodule of the selected row beside the files, else its diff:
    // returns whether the diff is to be loaded, as it is for a conflict too.
    fn prepare_diff(&mut self) -> bool {
        // park the submodule panel shown before, to reuse it when it is selected again
        if let Beside::Submodule(panel) = std::mem::replace(&mut self.beside, Beside::Diff) {
            self.submodule_cache = Some((panel.key().to_string(), *panel));
        }
        self.beside = match self.load_conflict() {
            Some(conflict) => Beside::Conflict(conflict),
            None => self
                .load_submodule()
                .map_or(Beside::Diff, |panel| Beside::Submodule(Box::new(panel))),
        };
        // the diff or conflict selected before is gone, the commit section stays
        if !self.is_commit_focused() {
            self.chosen = Focus::Files;
        }
        // a submodule has no hunks to select, the panel beside the files shows it instead; else
        // the diff loaded replaces the one shown, see `DiffPane::load_in_background`
        let submodule = self.submodule().is_some();
        if submodule {
            self.pane.set_diff(None);
        }
        !submodule
    }

    // The submodule shown beside the files, see `Beside`.
    fn submodule(&self) -> Option<&SubmodulePanel<'a>> {
        match &self.beside {
            Beside::Submodule(submodule) => Some(submodule),
            _ => None,
        }
    }

    fn submodule_mut(&mut self) -> Option<&mut SubmodulePanel<'a>> {
        match &mut self.beside {
            Beside::Submodule(submodule) => Some(submodule),
            _ => None,
        }
    }

    // The submodule of the selected file, shown in place of its gitlink diff. The last panel is
    // reused when it shows the same submodule, so moving away and back is instant; only its
    // uncommitted count is read again. None for a directory or a plain file.
    fn load_submodule(&mut self) -> Option<SubmodulePanel<'a>> {
        let row = self.selected_row()?;
        let is_dir = row.is_dir();
        let path = row.path.clone();
        if is_dir {
            return None;
        }
        let info = self.repository.submodule_info(&path)?;
        // the two gitlinks the section compares: the index and the working tree when unstaged,
        // the base commit and the index when staged
        let (from, to) = match self.section {
            LocalChangesSection::Unstaged => (info.index.clone(), info.head.clone()),
            LocalChangesSection::Staged => {
                let base = self.draft.amend_base().unwrap_or("HEAD");
                let from = self.repository.submodule_gitlink_at(base, &path)?;
                (from, Some(info.index.clone()))
            }
        };
        let key = submodule_key(&path, &from, to.as_deref());
        if let Some((cached_key, mut panel)) = self.submodule_cache.take() {
            if cached_key == key {
                panel.set_uncommitted(info.uncommitted);
                return Some(panel);
            }
        }
        Some(SubmodulePanel::between(
            self.repository,
            &self.ctx,
            &path,
            &from,
            to.as_deref(),
            info.uncommitted,
            self.background.as_ref(),
        ))
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
        let areas = local_changes_areas(&self.ctx, area);
        self.render_file_section(state, LocalChangesSection::Unstaged, areas.unstaged, buf);
        self.render_file_section(state, LocalChangesSection::Staged, areas.staged, buf);
        if state.conflict().is_some() {
            self.render_conflict(state, areas.diff, buf);
        } else if state.submodule().is_some() {
            self.render_submodule(state, areas.diff, buf);
        } else {
            self.render_diff(state, areas.diff, buf);
        }
        self.render_commit(state, areas.commit, buf);
    }
}

// the summary, a line, the description with 3 lines, and the borders
const COMMIT_AREA_HEIGHT: u16 = 1 + 1 + 3 + 2;

// Where the local changes page draws its panels.
struct LocalChangesAreas {
    unstaged: Rect,
    staged: Rect,
    diff: Rect,
    commit: Rect,
}

fn local_changes_areas(ctx: &AppContext, area: Rect) -> LocalChangesAreas {
    let mut layout = ctx.layout.borrow_mut();
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
        Layout::vertical([Constraint::Min(0), Constraint::Length(commit_height)]).areas(right_area);
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
    LocalChangesAreas {
        unstaged: unstaged_area,
        staged: staged_area,
        diff: diff_area,
        commit: commit_area,
    }
}

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
            // the filter is highlighted in the section it applies to
            state.query_of(section).filter(),
            |path| picked.contains(path),
            match (active, tree.is_visual(), self.focused) {
                (false, _, _) => TreeCursor::Hidden,
                (true, false, false) => TreeCursor::Dim,
                (true, false, true) => TreeCursor::Bright,
                (true, true, false) => TreeCursor::PickedDim,
                (true, true, true) => TreeCursor::PickedBright,
            },
            empty,
            &self.ctx,
        );
        let total = tree.rows().len();
        render_panel_scrollbar(buf, area, total, offset, focused, &self.ctx.color_theme);
    }

    fn render_diff(&self, state: &mut LocalChangesState, area: Rect, buf: &mut Buffer) {
        let title = match state.selected_row() {
            Some(row) if row.is_root() => "/".to_string(),
            Some(row) => row.path.clone(),
            None => String::new(),
        };
        let diff_focused = self.focused && state.is_diff_focused();
        let notes = state.diff_query.indicator().into_iter();
        let notes = notes.chain(loading_note(state.pane.loading_since()));
        let title = panel_title(&title, notes, diff_focused, &self.ctx.color_theme);
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

    fn render_submodule(&self, state: &mut LocalChangesState, area: Rect, buf: &mut Buffer) {
        let path = state.selected_path().unwrap_or_default();
        let theme = &self.ctx.color_theme;
        let title = Line::from(vec![
            Span::raw(format!(" {path} ")).fg(theme.fg),
            badge(" submodule changed ", theme.status_warn_fg, theme),
        ]);
        let focused = self.focused && state.is_submodule_focused();
        let Some(submodule) = state.submodule_mut() else {
            return;
        };
        submodule.render_framed(&self.ctx, area, buf, title, focused);
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
        test_support::{
            git::{git, GitRepository},
            ui::test_ctx,
        },
        widget::file_tree::ROOT_PATH,
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
    fn test_submodule_shows_its_commits_instead_of_the_gitlink_diff(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let base = dir.path().join("base");
        fs::create_dir(&base)?;
        let base_repo = GitRepository::new(&base);
        base_repo.init();
        fs::write(base.join("a.txt"), "one\n")?;
        base_repo.add(&["a.txt"]);
        base_repo.commit("init", "2024-01-01");

        let superproject = dir.path().join("super");
        fs::create_dir(&superproject)?;
        let repo = GitRepository::new(&superproject);
        repo.init();
        git(
            &superproject,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                base.to_str().unwrap(),
                "sub",
            ],
        );
        repo.commit("add sub", "2024-01-02");
        let sub = superproject.join("sub");
        let sub_repo = GitRepository::new(&sub);
        fs::write(sub.join("a.txt"), "two\n")?;
        sub_repo.add(&["a.txt"]);
        sub_repo.commit("second", "2024-01-03");

        let repository = Repository::load(&superproject, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        select_row(&mut state, "sub");
        let submodule = state.submodule().expect("the submodule panel");
        assert_eq!(submodule.subjects(), ["second"]);
        // the gitlink diff isn't shown, there is nothing to enter in it
        assert!(state.pane.diff().is_none());

        // the files have the focus; Enter focuses the submodule panel, Esc goes back; discard,
        // which leaves a submodule's checkout as it is, isn't offered
        assert!(state.is_files_focused());
        assert!(!state.keys().takes(UserEvent::Discard));
        state.enter()?;
        assert!(state.is_submodule_focused());
        assert!(!state.is_files_focused());
        state.back();
        assert!(state.is_files_focused());
        assert!(state.request_edit().is_err(), "a submodule is a directory");
        Ok(())
    }

    #[test]
    fn test_submodule_panel_marks_its_selected_commit() -> Result<(), Box<dyn std::error::Error>> {
        use crate::test_support::ui::{column_of, find_row, row_text};
        let dir = tempfile::tempdir()?;
        let base = dir.path().join("base");
        fs::create_dir(&base)?;
        let base_repo = GitRepository::new(&base);
        base_repo.init();
        base_repo.commit("init", "2024-01-01");
        let superproject = dir.path().join("super");
        fs::create_dir(&superproject)?;
        let repo = GitRepository::new(&superproject);
        repo.init();
        let add = ["-c", "protocol.file.allow=always", "submodule", "add"];
        git(
            &superproject,
            &[&add[..], &[base.to_str().unwrap(), "sub"]].concat(),
        );
        repo.commit("add sub", "2024-01-02");
        let sub = superproject.join("sub");
        let sub_repo = GitRepository::new(&sub);
        sub_repo.commit("second", "2024-01-03");
        sub_repo.commit("third", "2024-01-04");

        let repository = Repository::load(&superproject, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        select_row(&mut state, "sub");
        state.enter()?;
        let area = Rect::new(0, 0, 120, 30);
        // as the app does before drawing
        state.update_submodule_height(area);
        state.prepare_graph_uploads();
        let mut buf = Buffer::empty(area);
        LocalChangesPanel::new(test_ctx(), true).render(area, &mut buf, &mut state);
        // the bar at the selected commit, the first, like the commit list's
        let title = find_row(&buf, "submodule changed");
        let border = (0..area.width)
            .rev()
            .find(|&x| {
                x < column_of(&buf, title, "submodule changed") && buf[(x, title)].symbol() == "╭"
            })
            .unwrap();
        // in the panel's padding, right before the graph, like the files' bar beside it
        let bar = |row: &str| buf[(border + 1, find_row(&buf, row))].symbol().to_string();
        assert_eq!(bar("third"), "▌");
        assert_eq!(bar("second"), " ");
        // the commits the checked-out one has, the gitlink moving forward
        assert!(row_text(&buf, title + 1).contains("2 commits added · no uncommitted files"));
        Ok(())
    }

    #[test]
    fn test_submodule_checked_out_before_the_recorded_commit_still_lists_its_commits(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let base = dir.path().join("base");
        fs::create_dir(&base)?;
        let base_repo = GitRepository::new(&base);
        base_repo.init();
        fs::write(base.join("a.txt"), "one\n")?;
        base_repo.add(&["a.txt"]);
        base_repo.commit("init", "2024-01-01");
        let first = base_repo.rev_parse_head();
        fs::write(base.join("a.txt"), "two\n")?;
        base_repo.add(&["a.txt"]);
        base_repo.commit("second", "2024-01-02");

        let superproject = dir.path().join("super");
        fs::create_dir(&superproject)?;
        let repo = GitRepository::new(&superproject);
        repo.init();
        git(
            &superproject,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                base.to_str().unwrap(),
                "sub",
            ],
        );
        repo.commit("add sub", "2024-01-03");
        // the submodule is checked out before the commit the superproject records
        git(&superproject.join("sub"), &["checkout", "-q", &first]);

        let repository = Repository::load(&superproject, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        select_row(&mut state, "sub");
        let submodule = state.submodule().expect("the submodule panel");
        // the commits the recorded gitlink has that the checked-out one doesn't
        assert_eq!(submodule.subjects(), ["second"]);
        assert!(state.pane.diff().is_none());
        // said so, the gitlink moving back
        let area = Rect::new(0, 0, 120, 30);
        state.update_submodule_height(area);
        state.prepare_graph_uploads();
        let mut buf = Buffer::empty(area);
        LocalChangesPanel::new(test_ctx(), true).render(area, &mut buf, &mut state);
        let note = crate::test_support::ui::find_row(&buf, "submodule changed") + 1;
        let note = crate::test_support::ui::row_text(&buf, note);
        assert!(note.contains("1 commit removed, a rewind"), "{note}");
        Ok(())
    }

    #[test]
    fn test_submodule_panel_reads_its_uncommitted_count_again_when_reused(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let base = dir.path().join("base");
        fs::create_dir(&base)?;
        let base_repo = GitRepository::new(&base);
        base_repo.init();
        fs::write(base.join("a.txt"), "one\n")?;
        base_repo.add(&["a.txt"]);
        base_repo.commit("init", "2024-01-01");

        let superproject = dir.path().join("super");
        fs::create_dir(&superproject)?;
        let repo = GitRepository::new(&superproject);
        repo.init();
        git(
            &superproject,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                base.to_str().unwrap(),
                "sub",
            ],
        );
        repo.commit("add sub", "2024-01-02");
        let sub = superproject.join("sub");
        let sub_repo = GitRepository::new(&sub);
        fs::write(sub.join("a.txt"), "two\n")?;
        sub_repo.add(&["a.txt"]);
        sub_repo.commit("second", "2024-01-03");
        fs::write(sub.join("a.txt"), "three\n")?;
        fs::write(sub.join("one.txt"), "one\n")?;
        fs::write(superproject.join("newfile.txt"), "new\n")?;

        let repository = Repository::load(&superproject, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        select_row(&mut state, "sub");
        let submodule = state.submodule().expect("the submodule panel");
        assert_eq!(submodule.subjects(), ["second"]);
        assert_eq!(submodule.uncommitted(), Some(2));

        // a third uncommitted file, then away and back: the panel is reused, its count read again
        fs::write(sub.join("two.txt"), "two\n")?;
        select_row(&mut state, "newfile.txt");
        assert!(state.submodule().is_none());
        select_row(&mut state, "sub");
        let submodule = state.submodule().expect("the submodule panel");
        assert_eq!(submodule.subjects(), ["second"]);
        assert_eq!(submodule.uncommitted(), Some(3));
        Ok(())
    }

    #[test]
    fn test_staged_submodule_shows_the_staged_commits() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let base = dir.path().join("base");
        fs::create_dir(&base)?;
        let base_repo = GitRepository::new(&base);
        base_repo.init();
        fs::write(base.join("a.txt"), "one\n")?;
        base_repo.add(&["a.txt"]);
        base_repo.commit("init", "2024-01-01");

        let superproject = dir.path().join("super");
        fs::create_dir(&superproject)?;
        let repo = GitRepository::new(&superproject);
        repo.init();
        git(
            &superproject,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                base.to_str().unwrap(),
                "sub",
            ],
        );
        repo.commit("add sub", "2024-01-02");
        let sub = superproject.join("sub");
        let sub_repo = GitRepository::new(&sub);
        fs::write(sub.join("a.txt"), "two\n")?;
        sub_repo.add(&["a.txt"]);
        sub_repo.commit("second", "2024-01-03");

        let repository = Repository::load(&superproject, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        select_row(&mut state, "sub");
        assert_eq!(
            state.submodule().unwrap().subjects(),
            ["second"],
            "unstaged: index to the working tree"
        );

        // the new gitlink is staged; the staged section compares HEAD to the index
        state.select()?;
        state.focus_section(LocalChangesSection::Staged);
        select_row(&mut state, "sub");
        assert_eq!(
            state.submodule().unwrap().subjects(),
            ["second"],
            "staged: HEAD to the index"
        );
        Ok(())
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
    fn test_diff_context_shows_more_lines_around_the_change(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        fs::write(repo_path.join("a.txt"), numbered_lines(20))?;
        git.add(&["a.txt"]);
        git.commit("init", "2024-01-01");
        // a change in the middle, so there are lines to show on both sides
        fs::write(
            repo_path.join("a.txt"),
            numbered_lines(20).replace("line 10\n", "line 10\nnew\n"),
        )?;
        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        state.enter()?;
        assert!(state.is_diff_focused());
        assert!(state.keys().takes(UserEvent::IncreaseDiffContext));

        let before = diff(&state).rows.len();
        state.adjust_diff_context(UserEvent::IncreaseDiffContext);
        assert!(diff(&state).rows.len() > before);
        assert!(state.is_diff_focused(), "the selection stays in the diff");
        state.adjust_diff_context(UserEvent::DecreaseDiffContext);
        assert_eq!(diff(&state).rows.len(), before);
        Ok(())
    }

    #[test]
    fn test_diff_keys_work_while_the_diff_reloads() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        fs::write(repo_path.join("a.txt"), numbered_lines(20))?;
        git.add(&["a.txt"]);
        git.commit("init", "2024-01-01");
        let changed = numbered_lines(20).replace("line 10\n", "line 10\nnew\n");
        fs::write(repo_path.join("a.txt"), changed)?;
        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        // loading the diffs on another thread, as the app does
        let ec = crate::event::EventController::without_terminal();
        let mut state =
            LocalChangesState::restore(&repository, test_ctx(), None, Some(ec.sender()));
        state.load_stale_diff();
        state.enter()?;
        state.adjust_diff_context(UserEvent::IncreaseDiffContext);
        // still in the diff while it reloads: its keys are listed, and the next one is taken
        assert!(state.is_diff_focused());
        assert!(state.keys().takes(UserEvent::IncreaseDiffContext));
        state.load_stale_diff();
        assert!(state.is_diff_focused());
        Ok(())
    }

    #[test]
    fn test_half_page_moves_the_files() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        git.commit("init", "2024-01-01");
        for i in 0..30 {
            fs::write(repo_path.join(format!("u{i:02}.txt")), "x\n")?;
        }
        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        // drawn once: the half page counts the rows the files are shown in
        let area = Rect::new(0, 0, 90, 20);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        LocalChangesPanel::new(crate::test_support::ui::test_ctx(), true)
            .render(area, &mut buf, &mut state);

        let first = state.cursor();
        state.scroll(UserEvent::HalfPageDown);
        let jumped = state.cursor();
        assert!(jumped > first, "the files didn't move");
        // down again, then back up: the same rows as coming down
        state.scroll(UserEvent::HalfPageDown);
        assert!(state.cursor() > jumped);
        state.scroll(UserEvent::HalfPageUp);
        assert_eq!(state.cursor(), jumped);
        state.scroll(UserEvent::HalfPageUp);
        assert_eq!(state.cursor(), first);
        Ok(())
    }

    #[test]
    fn test_scroll_keys_scroll_the_commit_message() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        git.commit("init", "2024-01-01");
        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        let body: String = (1..=20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        state.draft.set_message(&format!("subject\n\n{body}"));
        // the message is on the description box, its 3 lines shown
        state.focus_commit();
        state.select_next();
        let area = Rect::new(0, 0, 90, 20);
        let shown = |state: &mut LocalChangesState| {
            let mut buf = ratatui::buffer::Buffer::empty(area);
            LocalChangesPanel::new(crate::test_support::ui::test_ctx(), true)
                .render(area, &mut buf, state);
            crate::test_support::ui::dump(&buf)
        };
        let top = shown(&mut state);
        assert!(state.scroll(UserEvent::HalfPageDown));
        assert_ne!(shown(&mut state), top, "the message didn't scroll");
        // g and G pick the summary and the description, as j and k do
        assert!(state.keys().takes(UserEvent::GoToTop));
        state.select_first();
        assert_eq!(state.commit_field(), Some(CommitField::Summary));
        assert!(state.keys().takes(UserEvent::GoToBottom));
        state.select_last();
        assert_eq!(state.commit_field(), Some(CommitField::Description));
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
        state.toggle_visual();
        state.toggle_visual();
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
    fn test_next_folder() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        git.commit("init", "2024-01-01");
        for file in [
            "src/view/list.rs",
            "src/widget/w.rs",
            "src/app.rs",
            "docs/d.md",
            "tests/t.rs",
            "README",
        ] {
            let path = repo_path.join(file);
            fs::create_dir_all(path.parent().unwrap())?;
            fs::write(path, "x\n")?;
        }
        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        let go = |state: &mut LocalChangesState, from: &str, toward| {
            select_row(state, from);
            state.select_next_folder(toward).unwrap();
            state.selected_path().unwrap()
        };
        // the next folder at the same depth, over the folders inside
        assert_eq!(go(&mut state, "src", Toward::Next), "tests");
        assert_eq!(go(&mut state, "src/view", Toward::Next), "src/widget");
        // none left at that depth: a level up
        assert_eq!(go(&mut state, "src/widget", Toward::Next), "tests");
        // a file goes from its folder, a file at the top from itself, round; the root to the top
        assert_eq!(go(&mut state, "src/widget/w.rs", Toward::Next), "tests");
        assert_eq!(go(&mut state, ROOT_PATH, Toward::Next), "docs");
        assert_eq!(go(&mut state, "README", Toward::Next), "docs");
        assert_eq!(go(&mut state, "src/widget", Toward::Previous), "src/view");
        assert_eq!(go(&mut state, "docs", Toward::Previous), "tests");

        // a closed folder stays closed
        select_row(&mut state, "docs");
        state.collapse();
        assert_eq!(go(&mut state, "src", Toward::Previous), "docs");
        assert!(!state.tree().is_open("docs"));

        // from the diff, on in the diff of the folder's first file
        select_row(&mut state, "src/view/list.rs");
        state.enter()?;
        assert!(state.is_diff_focused());
        state.select_next_folder(Toward::Next)?;
        assert_eq!(state.selected_path().as_deref(), Some("src/widget/w.rs"));
        assert!(state.is_diff_focused());
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
        // files picked change nothing: the focus used to go back to the files
        state.back();
        state.toggle_visual();
        state.toggle_visual();
        assert!(!state.picked().is_empty());
        state.enter()?;
        state.select_next_file(Toward::Next)?;
        assert_eq!(state.selected_path().as_ref(), Some(&files[1]));
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
        state.select()?;
        assert_eq!(paths(&state.changes.staged), vec!["a.txt"]);
        assert_eq!(
            paths(&state.changes.unstaged),
            vec!["a.txt", "dir/x.txt", "dir/y.txt"]
        );
        assert!(state.is_diff_focused());
        assert_eq!(diff(&state).hunks.len(), 1);

        // staging the last hunk returns to the files
        state.select()?;
        assert_eq!(
            paths(&state.changes.unstaged),
            vec!["dir/x.txt", "dir/y.txt"]
        );
        assert!(!state.is_diff_focused());

        // stage a whole directory of untracked files
        select_row(&mut state, "dir");
        state.select()?;
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
        state.select()?;
        assert_eq!(paths(&state.changes.unstaged), vec!["a.txt"]);
        let staged_diff = repository.local_change_diff(
            &["a.txt"],
            DiffOf::Staged { base: None },
            crate::git::DEFAULT_DIFF_CONTEXT,
        );
        assert!(staged_diff.contains("+line two"));
        assert!(!staged_diff.contains("+line twenty-eight"));

        // unstage a single file from the staged directory
        state.back();
        select_row(&mut state, "dir/x.txt");
        state.select()?;
        assert_eq!(paths(&state.changes.staged), vec!["a.txt", "dir/y.txt"]);

        // stage the hunk of an untracked file
        state.focus_section(LocalChangesSection::Unstaged);
        select_row(&mut state, "dir/x.txt");
        state.enter()?;
        state.select()?;
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
        state.start_query();
        for c in "new".chars() {
            state.type_query_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        state.type_query_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(state.is_selecting_lines());
        assert_eq!(state.selected_diff_text().as_deref(), Some("new A"));
        assert_eq!(state.query().search(), None);
        state.search_next(Toward::Next, SearchOptions::default());
        assert_eq!(state.selected_diff_text().as_deref(), Some("new B"));
        state.search_next(Toward::Next, SearchOptions::default());
        assert_eq!(state.selected_diff_text().as_deref(), Some("new A"));

        // esc clears the search before going back
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

    // A hunk of a file that isn't UTF-8 is staged byte for byte: it used to be read with U+FFFD in
    // place of its bytes, which went into the index, see `text_from_git`.
    #[test]
    fn test_hunks_of_a_file_not_in_utf8_staged_as_they_are(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        fs::write(repo_path.join("latin1.txt"), b"x\n")?;
        git.add(&["latin1.txt"]);
        git.commit("init", "2024-01-01");
        fs::write(repo_path.join("latin1.txt"), b"x\ncaf\xe9\n")?;

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        select_row(&mut state, "latin1.txt");
        state.enter()?;
        state.select()?;
        let staged = crate::git::git_command()
            .args(["show", ":latin1.txt"])
            .current_dir(repo_path)
            .output()?
            .stdout;
        assert_eq!(staged, b"x\ncaf\xe9\n");
        // the same file, under a name that isn't UTF-8 either, line by line
        use std::os::unix::ffi::OsStrExt;
        let name = repo_path.join(std::ffi::OsStr::from_bytes(b"\xe9t\xe9.txt"));
        fs::write(&name, b"x\n")?;
        git_stdout(repo_path, &["add", "-A"]);
        git_stdout(repo_path, &["commit", "-qm", "two"]);
        fs::write(&name, b"x\n\xe0\n\xe8\n")?;
        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        select_row(&mut state, &crate::git::text_from_git(b"\xe9t\xe9.txt"));
        state.enter()?;
        state.enter()?;
        state.select()?;
        let staged = crate::git::git_command()
            .args(["show"])
            .arg(std::ffi::OsStr::from_bytes(b":\xe9t\xe9.txt"))
            .current_dir(repo_path)
            .output()?
            .stdout;
        assert_eq!(staged, b"x\n\xe0\n");
        Ok(())
    }

    // Fewer lines around the changes stop at one: git can't apply a hunk without any, which made
    // staging fail at 0.
    #[test]
    fn test_hunks_stage_with_the_least_context() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        fs::write(repo_path.join("a.txt"), numbered_lines(20))?;
        git.add(&["a.txt"]);
        git.commit("init", "2024-01-01");
        let modified = numbered_lines(20).replace("line 10\n", "line 10\nnew\n");
        fs::write(repo_path.join("a.txt"), modified)?;

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        select_row(&mut state, "a.txt");
        state.enter()?;
        for _ in 0..5 {
            state.adjust_diff_context(UserEvent::DecreaseDiffContext);
        }
        assert_eq!(state.ctx.diff_context.get(), 1);
        assert!(!state.keys().takes(UserEvent::DecreaseDiffContext));
        state.select()?;
        let staged = repository.local_change_diff(
            &["a.txt"],
            DiffOf::Staged { base: None },
            crate::git::DEFAULT_DIFF_CONTEXT,
        );
        assert!(staged.contains("+new"));
        Ok(())
    }

    // The keys of the selected file act from the files and its diff alone: blame and timeline
    // used to act on it from the commit message too.
    #[test]
    fn test_file_keys_wait_for_the_files_or_the_diff() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        fs::write(repo_path.join("a.txt"), "a\n")?;
        git.add(&["a.txt"]);
        git.commit("init", "2024-01-01");
        fs::write(repo_path.join("a.txt"), "b\n")?;

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        select_row(&mut state, "a.txt");
        assert!(state.keys().takes(UserEvent::Blame));
        state.enter()?;
        assert!(state.keys().takes(UserEvent::Timeline));
        state.focus_commit();
        state.handle_text_key(KeyEvent::from(KeyCode::Esc));
        assert!(matches!(state.focus(), Focus::Commit(_)));
        assert!(!state.keys().takes(UserEvent::Blame));
        assert!(!state.keys().takes(UserEvent::Timeline));
        Ok(())
    }

    // The keys of the files do nothing in the commit section, the focus staying there: its keys
    // leave them out, which is all that keeps them from acting.
    #[test]
    fn test_file_keys_left_out_in_the_commit_section() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        fs::write(repo_path.join("a.txt"), "a\n")?;
        git.add(&["a.txt"]);
        git.commit("init", "2024-01-01");
        fs::write(repo_path.join("a.txt"), "b\n")?;
        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        state.focus_commit();
        state.handle_text_key(KeyEvent::from(KeyCode::Esc));
        assert!(matches!(state.focus(), Focus::Commit(_)));
        use UserEvent::*;
        for event in [
            Search,
            GoToNext,
            GoToPrevious,
            ToggleSelect,
            NextFile,
            PreviousFile,
            NextFolder,
            PreviousFolder,
            Discard,
            NavigateLeft,
            NavigateRight,
            ShortCopy,
            FullCopy,
            Ignore,
        ] {
            assert!(!state.keys().takes(event), "{event:?}");
        }
        Ok(())
    }

    #[test]
    fn test_toggle_stage_a_range_of_lines() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        fs::write(repo_path.join("a.txt"), numbered_lines(10))?;
        git.add(&["a.txt"]);
        git.commit("init", "2024-01-01");
        // one hunk with three added lines
        let modified = numbered_lines(10).replace("line 5\n", "line 5\nnew A\nnew B\nnew C\n");
        fs::write(repo_path.join("a.txt"), modified)?;

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        select_row(&mut state, "a.txt");
        state.enter()?;
        state.enter()?;
        // v and down select "new A" and "new B", which y copies and space stages
        state.toggle_visual();
        state.select_next();
        assert_eq!(state.selected_diff_name(), "lines");
        assert_eq!(state.selected_diff_text().as_deref(), Some("new A\nnew B"));
        state.select()?;
        let staged = repository.local_change_diff(
            &["a.txt"],
            DiffOf::Staged { base: None },
            crate::git::DEFAULT_DIFF_CONTEXT,
        );
        assert!(staged.contains("+new A") && staged.contains("+new B"));
        assert!(!staged.contains("+new C"));
        // on the line left, the range ended
        assert!(state.is_selecting_lines());
        assert_eq!(state.selected_diff_name(), "line");
        assert_eq!(state.selected_diff_text().as_deref(), Some("new C"));
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
        let staged_diff = |path| {
            repository.local_change_diff(
                &[path],
                DiffOf::Staged { base: None },
                crate::git::DEFAULT_DIFF_CONTEXT,
            )
        };
        let unstaged_diff = |path| {
            repository.local_change_diff(
                &[path],
                DiffOf::Unstaged,
                crate::git::DEFAULT_DIFF_CONTEXT,
            )
        };

        // files -> hunks -> lines, starting on the first changed line
        select_row(&mut state, "a.txt");
        state.enter()?;
        assert_eq!(diff(&state).hunks.len(), 1);
        state.enter()?;
        assert!(state.is_selecting_lines());
        assert_eq!(state.selected_diff_text().as_deref(), Some("new A"));

        // stage a single line, staying in line mode on the next changed line
        state.select()?;
        assert!(staged_diff("a.txt").contains("+new A"));
        assert!(!staged_diff("a.txt").contains("+new B"));
        assert!(unstaged_diff("a.txt").contains("+new B"));
        assert!(state.is_selecting_lines());
        assert_eq!(state.selected_diff_text().as_deref(), Some("new B"));

        // staging the last line returns to the files
        state.select()?;
        assert!(staged_diff("a.txt").contains("+new B"));
        assert!(!state.is_diff_focused());
        assert!(!state.is_selecting_lines());

        // unstage a single line
        state.focus_section(LocalChangesSection::Staged);
        select_row(&mut state, "a.txt");
        state.enter()?;
        state.enter()?;
        state.select()?;
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
        state.select()?;
        let staged = staged_diff("u.txt");
        assert!(staged.contains("new file mode") && staged.contains("+u1"));
        assert!(!staged.contains("+u2"));

        // stage the rest of it, then unstage only its last line
        state.back();
        state.back();
        state.select()?;
        state.focus_section(LocalChangesSection::Staged);
        select_row(&mut state, "u.txt");
        state.enter()?;
        state.enter()?;
        state.select_last();
        state.select()?;
        let staged = staged_diff("u.txt");
        assert!(staged.contains("+u1") && staged.contains("+u2"));
        assert!(!staged.contains("+u3"));

        Ok(())
    }

    #[test]
    fn test_a_reload_keeps_where_the_keys_go() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        fs::write(repo_path.join("a.txt"), numbered_lines(5))?;
        git.add(&["a.txt"]);
        git.commit("first", "2024-01-01");
        fs::write(repo_path.join("a.txt"), "changed\n")?;
        let repository = reload(repo_path);

        // in the diff beside the files
        let mut state = LocalChangesState::load(&repository);
        assert!(state.is_files_focused());
        state.enter()?;
        assert!(state.is_diff_focused());
        let reloaded =
            LocalChangesState::restore(&repository, test_ctx(), Some(state.snapshot()), None);
        assert!(reloaded.is_diff_focused());

        // in the commit section, a box being typed into only staying selected
        let mut state = LocalChangesState::load(&repository);
        state.focus_commit();
        assert!(state.is_editing());
        let reloaded =
            LocalChangesState::restore(&repository, test_ctx(), Some(state.snapshot()), None);
        assert!(reloaded.is_commit_focused());
        assert!(!reloaded.is_editing());

        // and on the files
        let mut state = LocalChangesState::load(&repository);
        state.focus_commit();
        state.focus_files();
        let reloaded =
            LocalChangesState::restore(&repository, test_ctx(), Some(state.snapshot()), None);
        assert!(reloaded.is_files_focused());
        Ok(())
    }

    // The repository loaded again, as the app does after a git command or a change made outside.
    fn reload(path: &std::path::Path) -> Repository {
        Repository::load(path, SortCommit::Chronological, None, false).unwrap()
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
        state.select()?;
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
        let merging = || -> Result<Repository, Box<dyn std::error::Error>> {
            let head = git_stdout(repo_path, &["rev-parse", "HEAD"]);
            fs::write(repo_path.join(".git/MERGE_HEAD"), head)?;
            let repository = reload(repo_path);
            fs::remove_file(repo_path.join(".git/MERGE_HEAD"))?;
            Ok(repository)
        };
        let merging_repository = merging()?;
        let during = LocalChangesState::restore(
            &merging_repository,
            crate::test_support::ui::test_ctx(),
            Some(state.snapshot()),
            None,
        );
        assert!(during.keys().takes(UserEvent::ToggleAmend));

        // the draft survives a refresh
        let reloaded = LocalChangesState::restore(
            &repository,
            crate::test_support::ui::test_ctx(),
            Some(state.snapshot()),
            None,
        );
        assert_eq!(paths(&reloaded.changes.staged), vec!["a.txt", "c.txt"]);

        // turning amend off restores the staged section and the typed message
        state.toggle_amend()?;
        assert_eq!(state.draft.message(), "draft");
        let merging_repository = merging()?;
        let during = LocalChangesState::restore(
            &merging_repository,
            crate::test_support::ui::test_ctx(),
            Some(state.snapshot()),
            None,
        );
        assert!(!during.keys().takes(UserEvent::ToggleAmend));
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
        let repository = reload(repo_path);
        let state = LocalChangesState::restore(
            &repository,
            crate::test_support::ui::test_ctx(),
            Some(state.snapshot()),
            None,
        );
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
        state.reload_repository();
        assert_eq!(
            state.repository.operation_in_progress(),
            Some(Operation::Rebase)
        );
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
            UserEvent::Select,
            UserEvent::ToggleSelect,
            UserEvent::Edit,
            UserEvent::Search,
            UserEvent::Ignore,
        ] {
            assert!(!keys.takes(event), "{event:?}");
        }
        assert!(keys.takes(UserEvent::Stash));
        assert!(shown(&state).contains(&"commit".to_string()));

        // on the staged root, staging works but not editing or ignoring; on the file, all
        state.focus_section(LocalChangesSection::Staged);
        assert!(state.keys().takes(UserEvent::Select));
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
        state.commit()?;
        assert!(state.is_editing());
        state.back();
        state.back();

        // in the commit section, space and e type the message, enter asks to commit
        state.focus_commit();
        state.select()?;
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
        state.commit()?;
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

    // File names that aren't UTF-8 are staged, unstaged, discarded and ignored as they are: two
    // used to be shown as one, U+FFFD in place of their bytes, which matched no file in git.
    #[test]
    fn test_names_not_in_utf8() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        git.commit("init", "2024-01-01");
        let name = |b: &[u8]| repo_path.join(std::ffi::OsStr::from_bytes(b));
        fs::write(name(b"caf\xe9.txt"), "e\n")?;
        fs::write(name(b"caf\xe8.txt"), "e\n")?;
        // an ignore file in Latin-1 already
        fs::write(repo_path.join(".gitignore"), b"/d\xe9j\xe0\n")?;
        let (e9, e8) = (
            crate::git::text_from_git(b"caf\xe9.txt"),
            crate::git::text_from_git(b"caf\xe8.txt"),
        );
        assert_ne!(e9, e8);

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        let staged = || {
            crate::git::git_command()
                .args(["diff", "--cached", "--name-only", "-z"])
                .current_dir(repo_path)
                .output()
                .unwrap()
                .stdout
        };

        // staged as itself
        select_row(&mut state, &e9);
        state.select()?;
        assert_eq!(staged(), b"caf\xe9.txt\0");
        // and unstaged
        state.focus_section(LocalChangesSection::Staged);
        select_row(&mut state, &e9);
        state.select()?;
        assert!(staged().is_empty());
        state.focus_section(LocalChangesSection::Unstaged);

        // ignored as itself, the lines before kept as they were
        select_row(&mut state, &e8);
        state.ignore_selected()?;
        assert_eq!(
            fs::read(repo_path.join(".gitignore"))?,
            b"/d\xe9j\xe0\n/caf\xe8.txt\n"
        );

        // discarded, an untracked file gone
        select_row(&mut state, &e9);
        state.request_discard()?;
        confirm_and_run(&mut state)?;
        assert!(!name(b"caf\xe9.txt").exists());
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
        state.select()?;
        assert!(state.changes.unstaged.is_empty());
        assert_eq!(
            paths(&state.changes.staged),
            vec!["a.txt", "dir/x.txt", "u.txt"]
        );

        // unstage all
        state.focus_section(LocalChangesSection::Staged);
        select_row(&mut state, ROOT_PATH);
        state.select()?;
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
        state.select()?;
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

    fn type_query(state: &mut LocalChangesState, text: &str) {
        state.start_query();
        for c in text.chars() {
            state.type_query_key(key(KeyCode::Char(c)));
        }
        state.type_query_key(key(KeyCode::Enter));
    }

    #[test]
    fn test_filter() -> Result<(), Box<dyn std::error::Error>> {
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

        // / filters the files, then staging everything shown stages only the matching files
        type_query(&mut state, "list");
        assert_eq!(
            state
                .query_of(LocalChangesSection::Unstaged)
                .indicator()
                .as_deref(),
            Some("filter: list")
        );
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
        state.select()?;
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

        // esc clears the filter of the section it is in only
        assert!(!state.clear_query(SearchOptions::default()));
        state.focus_section(LocalChangesSection::Unstaged);
        assert!(visible(&state).is_empty());
        assert!(state.clear_query(SearchOptions::default()));
        assert!(!state.clear_query(SearchOptions::default()));
        assert_eq!(visible(&state), vec![":/", "src", "src/app.rs"]);

        Ok(())
    }

    #[test]
    fn test_ignore_picked_files() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        for name in ["a.txt", "b.txt"] {
            fs::write(repo_path.join(name), "x\n")?;
        }
        git.add(&["a.txt", "b.txt"]);
        git.commit("init", "2024-01-01");
        for name in ["a.txt", "b.txt"] {
            fs::write(repo_path.join(name), "changed\n")?;
        }
        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);

        select_row(&mut state, "a.txt");
        state.toggle_visual();
        select_row(&mut state, "b.txt");
        state.toggle_visual();
        let message = state.ignore_selected()?;
        assert!(message.contains("a.txt") && message.contains("b.txt"));
        assert_eq!(
            fs::read_to_string(repo_path.join(".gitignore"))?,
            "/a.txt\n/b.txt\n"
        );
        Ok(())
    }

    // Ignore adds what is picked, shown or not: a pick in a closed directory used to be left
    // out, as only the rows shown were looked at.
    #[test]
    fn test_ignore_picks_in_a_closed_directory() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        git.commit("init", "2024-01-01");
        fs::create_dir(repo_path.join("dir"))?;
        fs::write(repo_path.join("dir/u.txt"), "u\n")?;
        fs::write(repo_path.join("dir/v.txt"), "v\n")?;
        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);

        select_row(&mut state, "dir/u.txt");
        state.toggle_visual();
        state.toggle_visual();
        select_row(&mut state, "dir");
        state.collapse();
        assert!(!state.tree().rows().iter().any(|r| r.path == "dir/u.txt"));
        state.ignore_selected()?;
        assert_eq!(
            fs::read_to_string(repo_path.join(".gitignore"))?,
            "/dir/u.txt\n"
        );
        Ok(())
    }

    #[test]
    fn test_single_target_file_does_not_expand_a_picked_directory(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        git.commit("init", "2024-01-01");
        fs::create_dir(repo_path.join("dir"))?;
        fs::write(repo_path.join("dir/u.txt"), "u\n")?;
        fs::write(repo_path.join("b.txt"), "b\n")?;

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        select_row(&mut state, "dir");
        state.toggle_visual();
        assert_eq!(
            state.single_target_file(),
            None,
            "a directory is not a file"
        );

        // one picked file, exact
        state.clear_picks();
        select_row(&mut state, "dir/u.txt");
        state.toggle_visual();
        state.toggle_visual();
        assert_eq!(state.single_target_file(), Some("dir/u.txt"));

        // another pick leaves no one file
        select_row(&mut state, "b.txt");
        state.toggle_visual();
        state.toggle_visual();
        assert_eq!(state.single_target_file(), None);
        Ok(())
    }

    #[test]
    fn test_edit_opens_only_the_exact_picked_files() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        git.commit("init", "2024-01-01");
        fs::create_dir(repo_path.join("dir"))?;
        fs::write(repo_path.join("dir/u.txt"), "u\n")?;
        fs::write(repo_path.join("b.txt"), "b\n")?;

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        // the directory is closed, so the range picks it and the file, not the file under it
        select_row(&mut state, "dir");
        state.collapse();
        state.toggle_visual();
        select_row(&mut state, "b.txt");
        state.toggle_visual();
        state.request_edit()?;
        assert_eq!(
            state.take_external_request(),
            Some(ExternalRequest::Editor(vec![repo_path.join("b.txt")])),
            "the directory was expanded"
        );
        Ok(())
    }

    #[test]
    fn test_visual_selection_of_files_follows_the_cursor_back(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        for name in ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"] {
            fs::write(repo_path.join(name), "x\n")?;
        }
        let git = GitRepository::new(repo_path);
        git.init();
        git.commit("init", "2024-01-01");
        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        let picked = |state: &LocalChangesState| {
            state.picked().names().map(String::from).collect::<Vec<_>>()
        };
        // e picked on its own first, kept by the selection after it
        select_row(&mut state, "e.txt");
        state.toggle_visual();
        state.toggle_visual();
        // moving back unpicks what the selection went over, like the commit list
        select_row(&mut state, "a.txt");
        state.toggle_visual();
        (0..3).for_each(|_| _ = state.navigate_files(UserEvent::NavigateDown));
        assert_eq!(
            picked(&state),
            ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"]
        );
        (0..2).for_each(|_| _ = state.navigate_files(UserEvent::NavigateUp));
        assert_eq!(picked(&state), ["a.txt", "b.txt", "e.txt"]);
        state.toggle_visual();
        assert_eq!(picked(&state), ["a.txt", "b.txt", "e.txt"]);
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
        state.toggle_visual();
        select_row(&mut state, "dir");
        state.toggle_visual();
        state.select()?;
        assert_eq!(paths(&state.changes.staged), vec!["a.txt", "dir/u.txt"]);
        assert_eq!(paths(&state.changes.unstaged), vec!["b.txt", "c.txt"]);
        assert!(state.picked().is_empty());

        // a new visual selection keeps the files picked before it
        select_row(&mut state, "b.txt");
        state.toggle_visual();
        state.toggle_visual();
        select_row(&mut state, "c.txt");
        state.toggle_visual();
        state.toggle_visual();
        assert_eq!(
            state.picked().names().collect::<Vec<_>>(),
            ["b.txt", "c.txt"]
        );
        state.request_edit()?;
        assert_eq!(
            state.take_external_request(),
            Some(ExternalRequest::Editor(vec![
                repo_path.join("b.txt"),
                repo_path.join("c.txt"),
            ]))
        );
        state.clear_picks();

        // enter opens the cursor row's diff only, even with files picked
        select_row(&mut state, "b.txt");
        state.toggle_visual();
        select_row(&mut state, "c.txt");
        state.toggle_visual();
        state.enter()?;
        assert!(state.is_diff_focused());
        assert_eq!(diff(&state).hunks.len(), 1);
        state.back();
        state.clear_picks();

        // discard the picked files only
        fs::write(repo_path.join("b.txt"), "x\nchanged\nagain\n")?;
        fs::write(repo_path.join("c.txt"), "x\nchanged\nagain\n")?;
        state.reload();
        state.clear_picks();
        select_row(&mut state, "b.txt");
        state.toggle_visual();
        state.toggle_visual();
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
        state.toggle_visual();
        state.toggle_visual();
        state.select()?;
        assert!(!paths(&state.changes.staged).contains(&"a.txt"));

        // esc: back from the diff, then the picked files, then the filter
        state.focus_section(LocalChangesSection::Unstaged);
        select_row(&mut state, "c.txt");
        state.toggle_visual();
        state.toggle_visual();
        type_query(&mut state, "txt");
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
        state.toggle_visual();
        state.toggle_visual();
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
    fn test_rebase_skip() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let _git = conflicting_rebase(repo_path);
        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);

        // skipping drops the commit it stopped at, after confirming, and the rebase goes on
        state.request_skip();
        assert!(prompt(&state).is_some_and(|p| p.starts_with("Skip the commit the rebase")));
        confirm_and_run(&mut state)?;
        assert_eq!(state.repository.operation_in_progress(), None);
        assert_eq!(
            git_stdout(repo_path, &["log", "--format=%s", "-1"]),
            "master change"
        );
        Ok(())
    }

    #[test]
    fn test_rebase_continue() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let _git = conflicting_rebase(repo_path);

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        assert_eq!(
            state.repository.operation_in_progress(),
            Some(Operation::Rebase)
        );
        assert_eq!(
            state.changes.unstaged[0].status,
            LocalChangeStatus::Conflicted
        );

        // m c continues, m s skips, while rebasing; continuing with the conflict unresolved fails
        // and keeps rebasing
        assert!(state.keys().takes(UserEvent::Continue) && state.keys().takes(UserEvent::Skip));
        state.continue_rebase();
        assert!(state.run_task().is_err());
        assert_eq!(
            state.repository.operation_in_progress(),
            Some(Operation::Rebase)
        );

        // resolved, staged as the files load again, then continued
        fs::write(repo_path.join("a.txt"), "resolved\n")?;
        state.reload();
        state.continue_rebase();
        state.run_task()?;
        assert_eq!(state.repository.operation_in_progress(), None);
        assert_eq!(
            git_stdout(repo_path, &["log", "--format=%s", "-2"]),
            "feature change\nmaster change"
        );

        // abort, continue and skip only work while rebasing
        for event in [UserEvent::Abort, UserEvent::Continue, UserEvent::Skip] {
            assert!(!state.keys().takes(event), "{event:?}");
        }
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
        assert_eq!(
            state.repository.operation_in_progress(),
            Some(Operation::Rebase)
        );

        state.request_abort();
        confirm_and_run(&mut state)?;
        assert_eq!(state.repository.operation_in_progress(), None);
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

    // edited without markers, staged as the files load again
    fn resolve_and_stage(state: &mut LocalChangesState, repo_path: &std::path::Path) {
        fs::write(repo_path.join("a.txt"), "resolved\n").unwrap();
        state.reload();
        assert!(state.changes.staged.iter().any(|f| f.path == "a.txt"));
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

        // loaded before the page staged the file, it would show it unstaged again: dropped
        let before = files();
        state.select()?;
        assert!(state.changes.unstaged.is_empty());
        state.reload_if_changed(before);
        assert!(state.changes.unstaged.is_empty());
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
        assert_eq!(
            state.repository.operation_in_progress(),
            Some(Operation::Merge)
        );
        // git's message, without its comment lines
        // the conflicted files in the description
        assert_eq!(state.draft.summary.text(), "Merge branch 'feature'");
        assert_eq!(state.draft.description.text(), "Conflicts:\n  a.txt");
        assert!(state.toggle_amend().is_err());
        // also when the page opens from a snapshot; once cleared, it stays so
        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let restored = LocalChangesState::restore(
            &repository,
            crate::test_support::ui::test_ctx(),
            Some(Default::default()),
            None,
        );
        assert_eq!(restored.draft.summary.text(), "Merge branch 'feature'");
        state.draft.set_message("");
        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let restored = LocalChangesState::restore(
            &repository,
            crate::test_support::ui::test_ctx(),
            Some(state.snapshot()),
            None,
        );
        assert_eq!(restored.draft.message(), "");
        state.draft.set_message("Merge branch 'feature'");
        // a merge is finished by committing, with no commit to skip
        assert!(state.keys().takes(UserEvent::Abort));
        assert!(!state.keys().takes(UserEvent::Continue) && !state.keys().takes(UserEvent::Skip));

        resolve_and_stage(&mut state, repo_path);
        state.commit()?;
        confirm_and_run(&mut state)?;
        assert_eq!(state.repository.operation_in_progress(), None);
        // a merge commit, with both parents
        let parents = git_stdout(repo_path, &["log", "-1", "--format=%p"]);
        assert_eq!(parents.split(' ').count(), 2);
        assert!(git_stdout(repo_path, &["log", "-1", "--format=%s"]).starts_with("Merge branch"));

        // a new merge, aborted
        git_stdout(repo_path, &["reset", "--hard", "HEAD^"]);
        git_stdout(repo_path, &["merge", "feature"]);
        let before = git_stdout(repo_path, &["rev-parse", "HEAD"]);
        state.reload_repository();
        state.request_abort();
        assert_eq!(
            prompt(&state),
            Some("Abort the merge and go back to where it started".into())
        );
        confirm_and_run(&mut state)?;
        assert_eq!(state.repository.operation_in_progress(), None);
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
        assert_eq!(
            state.repository.operation_in_progress(),
            Some(Operation::CherryPick)
        );
        assert_eq!(
            state.draft.message(),
            "feature change\n\nConflicts:\n  a.txt"
        );

        resolve_and_stage(&mut state, repo_path);
        state.commit()?;
        confirm_and_run(&mut state)?;
        assert_eq!(state.repository.operation_in_progress(), None);
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
        assert_eq!(
            state.repository.operation_in_progress(),
            Some(Operation::Revert)
        );
        assert!(state
            .draft
            .message()
            .starts_with("Revert \"feature change\""));

        state.request_abort();
        confirm_and_run(&mut state)?;
        assert_eq!(state.repository.operation_in_progress(), None);
        assert!(!state.keys().takes(UserEvent::Abort));
        Ok(())
    }

    #[test]
    fn test_conflict_panel_of_a_rebase_with_the_ancestor() -> Result<(), Box<dyn std::error::Error>>
    {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let _git = diverged_branches(repo_path);
        git_stdout(repo_path, &["config", "merge.conflictStyle", "diff3"]);
        git_stdout(repo_path, &["checkout", "--quiet", "feature"]);
        git_stdout(repo_path, &["rebase", "master"]);

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        select_row(&mut state, "a.txt");
        let area = Rect::new(0, 0, 120, 30);
        let mut buf = Buffer::empty(area);
        LocalChangesPanel::new(crate::test_support::ui::test_ctx(), true)
            .render(area, &mut buf, &mut state);
        let rows: Vec<String> = (0..area.height)
            .map(|y| (0..area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        // the rebase's sides named for what they are, the ancestor between them
        for title in ["╭ Rebasing onto", "╭ Base", "╭ Your commit"] {
            assert!(rows.iter().any(|r| r.contains(title)), "{title}: {rows:#?}");
        }
        assert!(rows.iter().any(|r| r.contains(" base ")), "{rows:#?}");
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
        let conflict = &state.conflict().unwrap().view;
        assert_eq!(
            (conflict.ours_label.as_str(), conflict.theirs_label.as_str()),
            ("HEAD", "feature")
        );
        let texts = |rows: &[crate::widget::conflict::ConflictRow]| {
            rows.iter().map(|row| row.text.clone()).collect::<Vec<_>>()
        };
        assert_eq!(texts(&conflict.ours), ["── conflict 1 of 1 ──", "master"]);
        assert_eq!(
            texts(&conflict.theirs),
            ["── conflict 1 of 1 ──", "feature"]
        );
        // drawn as panels, their text a column off the border
        let area = Rect::new(0, 0, 120, 30);
        let mut buf = Buffer::empty(area);
        LocalChangesPanel::new(crate::test_support::ui::test_ctx(), true)
            .render(area, &mut buf, &mut state);
        let rows: Vec<String> = (0..area.height)
            .map(|y| (0..area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        assert!(
            rows.iter().any(|r| r.contains("╭ Current branch  HEAD ")),
            "{rows:#?}"
        );
        assert!(
            rows.iter().any(|r| r.contains("╭ Merging in  feature ")),
            "{rows:#?}"
        );
        // the line number of the file in the working tree, then the line
        assert!(rows.iter().any(|r| r.contains("│ 2 master ")), "{rows:#?}");
        // and what Enter does
        assert!(
            rows.iter()
                .any(|r| r.contains("Enter to keep a side for the whole file")),
            "{rows:#?}"
        );

        // the tools ask the view to run them, with space; enter only went into the panel
        state.enter()?;
        assert!(state.is_conflict_focused());
        assert!(state.keys().takes(UserEvent::Select));
        assert!(!state.keys().takes(UserEvent::Confirm));
        state.move_conflict_item(Direction::Down);
        state.select()?;
        assert_eq!(
            state.take_external_request(),
            Some(ExternalRequest::MergeTool("a.txt".into()))
        );
        state.move_conflict_item(Direction::Down);
        state.select()?;
        let toplevel = state.toplevel().to_path_buf();
        assert_eq!(
            state.take_external_request(),
            Some(ExternalRequest::Editor(vec![toplevel.join("a.txt")]))
        );

        // esc goes back to the files
        assert!(state.back());
        assert!(!state.is_conflict_focused());

        // e opens the file in the editor from the files too
        state.request_edit()?;
        assert_eq!(
            state.take_external_request(),
            Some(ExternalRequest::Editor(vec![toplevel.join("a.txt")]))
        );
        select_row(&mut state, ROOT_PATH);
        assert!(state.request_edit().is_err());
        select_row(&mut state, "a.txt");

        // picking a side resolves the file with it
        state.enter()?;
        state.move_conflict_item(Direction::Right);
        state.select()?;
        assert_eq!(fs::read_to_string(repo_path.join("a.txt"))?, "feature\n");
        assert!(state.changes.unstaged.is_empty());
        assert!(state.conflict().is_none());
        assert_eq!(
            state.repository.operation_in_progress(),
            Some(Operation::Merge)
        );
        Ok(())
    }

    #[test]
    fn test_space_on_conflicted_files() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let _git = diverged_branches(repo_path);
        git_stdout(repo_path, &["merge", "feature"]);
        fs::write(repo_path.join("new.txt"), "new\n")?;

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        let staged =
            |state: &LocalChangesState| state.changes.staged.iter().any(|f| f.path == "a.txt");
        // like lazygit, never staged with its markers: space on it opens the conflict panel
        select_row(&mut state, "a.txt");
        state.select()?;
        assert!(state.is_conflict_focused());
        assert!(!staged(&state));
        assert!(state.back());
        // nor with the other files of a directory
        select_row(&mut state, ROOT_PATH);
        assert!(state.select().is_err());
        assert!(!staged(&state));
        assert!(!state.changes.staged.iter().any(|f| f.path == "new.txt"));

        // resolved in the editor, no markers left: staged once the files load again, the page
        // made again too, as after the editor
        fs::write(repo_path.join("a.txt"), "resolved\n")?;
        let state = LocalChangesState::load(&repository);
        assert!(staged(&state));
        assert!(state.changes.unstaged.iter().all(|f| f.path != "a.txt"));
        git_stdout(repo_path, &["reset", "-q", "--", "a.txt"]);
        git_stdout(repo_path, &["checkout", "-q", "-m", "--", "a.txt"]);
        let mut state = LocalChangesState::load(&repository);
        assert!(!staged(&state));
        fs::write(repo_path.join("a.txt"), "resolved\n")?;
        state.reload();
        assert!(staged(&state));
        Ok(())
    }

    #[test]
    fn test_conflict_of_a_submodule() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let (inner, repo_path) = (dir.path().join("inner"), dir.path().join("repo"));
        fs::create_dir_all(&inner)?;
        fs::create_dir_all(&repo_path)?;
        let inner_repo = GitRepository::new(&inner);
        inner_repo.init();
        // commits on two branches, which git can't fast-forward one to the other
        inner_repo.commit("c1", "2024-01-01");
        inner_repo.checkout_b("side");
        inner_repo.commit("c2 side", "2024-01-02");
        inner_repo.checkout("master");
        inner_repo.commit("c3 master", "2024-01-03");
        let hash = |rev: &str| git_stdout(&inner, &["rev-parse", rev]).trim().to_string();
        let gitlink = |rev: &str| {
            let info = format!("160000,{},sub", hash(rev));
            git_stdout(&repo_path, &["update-index", "--add", "--cacheinfo", &info]);
        };
        let git = GitRepository::new(&repo_path);
        git.init();
        git_stdout(&repo_path, &["clone", "-q", "../inner", "sub"]);
        gitlink("master~1");
        git.commit("base", "2024-01-01");
        git.checkout_b("incoming");
        gitlink("side");
        git.commit("incoming", "2024-01-02");
        git.checkout("master");
        gitlink("master");
        git.commit("existing", "2024-01-03");
        git_stdout(&repo_path, &["merge", "incoming"]);

        let repository = Repository::load(&repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        select_row(&mut state, "sub");
        // the conflict is shown, not the submodule's commits; enter goes into its sides
        let texts = |rows: &[crate::widget::conflict::ConflictRow]| {
            rows.iter().map(|row| row.text.clone()).collect::<Vec<_>>()
        };
        let view = &state.conflict().unwrap().view;
        let short = |rev: &str| hash(rev)[..7].to_string();
        let at = |rev: &str, subject: &str| vec![format!("Submodule at {} {subject}", short(rev))];
        assert_eq!(texts(&view.ours), at("master", "c3 master"));
        assert_eq!(texts(&view.theirs), at("side", "c2 side"));
        state.enter()?;
        assert!(state.is_conflict_focused());
        assert!(state.keys().takes(UserEvent::Select));
        // a directory, no file to edit: the tools end at the merge tool
        state.move_conflict_item(Direction::Down);
        state.move_conflict_item(Direction::Down);
        assert_eq!(state.conflict().unwrap().item, ConflictItem::MergeTool);
        Ok(())
    }

    #[test]
    fn test_conflict_of_a_symlink() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = &dir.path().join("repo");
        fs::create_dir(repo_path)?;
        // what the link points at has conflict markers of its own, which aren't the link's
        let markers = "<<<<<<< a\nx\n=======\ny\n>>>>>>> b\n";
        fs::write(dir.path().join("ours.txt"), markers)?;
        let git = GitRepository::new(repo_path);
        git.init();
        let link = |target: &str| -> std::io::Result<()> {
            let _ = fs::remove_file(repo_path.join("link"));
            std::os::unix::fs::symlink(target, repo_path.join("link"))
        };
        link("../base.txt")?;
        git.add(&["link"]);
        git.commit("base", "2024-01-01");
        git.checkout_b("incoming");
        link("../theirs.txt")?;
        git.add(&["link"]);
        git.commit("incoming", "2024-01-02");
        git.checkout("master");
        link("../ours.txt")?;
        git.add(&["link"]);
        git.commit("existing", "2024-01-03");
        git_stdout(repo_path, &["merge", "incoming"]);

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let mut state = LocalChangesState::load(&repository);
        select_row(&mut state, "link");
        let texts = |rows: &[crate::widget::conflict::ConflictRow]| {
            rows.iter().map(|row| row.text.clone()).collect::<Vec<_>>()
        };
        let view = &state.conflict().unwrap().view;
        assert_eq!(texts(&view.ours), ["Symlink to ../ours.txt"]);
        assert_eq!(texts(&view.theirs), ["Symlink to ../theirs.txt"]);
        // no file to edit: the editor would open what it points at
        state.enter()?;
        state.move_conflict_item(Direction::Down);
        state.move_conflict_item(Direction::Down);
        assert_eq!(state.conflict().unwrap().item, ConflictItem::MergeTool);
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
        let conflict = &state.conflict().unwrap().view;
        let texts = |rows: &[crate::widget::conflict::ConflictRow]| {
            rows.iter().map(|row| row.text.clone()).collect::<Vec<_>>()
        };
        assert_eq!(texts(&conflict.ours), ["(deleted)"]);
        assert_eq!(texts(&conflict.theirs), ["feature"]);

        // keeping the deleting side removes the file
        state.enter()?;
        state.select()?;
        assert!(!repo_path.join("a.txt").exists());
        assert!(state.changes.unstaged.is_empty());
        Ok(())
    }
}
