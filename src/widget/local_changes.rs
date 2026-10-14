use std::{ops::Range, rc::Rc};

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph, StatefulWidget, Widget},
};

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::{
    app::AppContext,
    git::{LocalChangeFile, LocalChangeStatus, LocalChanges, Repository},
    widget::{
        diff::{diff_lines, parse_diff, ParsedDiff},
        file_tree::{build_rooted_file_tree_rows, FileTreeRow},
        text_area::TextArea,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalChangesSection {
    Unstaged,
    Staged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommitItem {
    Message,
    Amend,
    Commit,
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
    changes: LocalChanges,
    unstaged_rows: Vec<FileTreeRow>,
    staged_rows: Vec<FileTreeRow>,
    section: LocalChangesSection,
    unstaged_cursor: usize,
    staged_cursor: usize,
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
    commit_item: CommitItem,
    // true while typing the commit message
    editing: bool,
    message: TextArea,
    // set while amending: the parent of HEAD, which the staged section is compared to instead of HEAD,
    // so it shows the changes of the commit being amended together with what is staged
    amend_base: Option<String>,
    // the message typed before amending, restored when amending is turned off
    message_before_amend: Option<TextArea>,
    // set once a commit is made, until the view picks it up to refresh the repository
    committed: bool,

    // the question shown in a popup while waiting for the user to confirm discarding
    discard_prompt: Option<String>,
}

impl<'a> LocalChangesState<'a> {
    pub fn load(repository: &'a Repository) -> Self {
        let changes = repository.local_changes();
        let unstaged_rows =
            build_rooted_file_tree_rows(changes.unstaged.iter().map(|f| f.path.as_str()));
        let staged_rows =
            build_rooted_file_tree_rows(changes.staged.iter().map(|f| f.path.as_str()));
        // while only previewed, show whichever section has something to look at
        let section = if unstaged_rows.is_empty() && !staged_rows.is_empty() {
            LocalChangesSection::Staged
        } else {
            LocalChangesSection::Unstaged
        };
        let mut state = Self {
            repository,
            changes,
            unstaged_rows,
            staged_rows,
            section,
            unstaged_cursor: 0,
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
            commit_item: CommitItem::Message,
            editing: false,
            message: TextArea::default(),
            amend_base: None,
            message_before_amend: None,
            committed: false,
            discard_prompt: None,
        };
        state.load_diff();
        state
    }

    pub fn count(&self) -> usize {
        self.changes.count()
    }

    pub fn section(&self) -> LocalChangesSection {
        self.section
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        if !focused {
            self.diff_focused = false;
            self.line_mode = false;
            self.commit_focused = false;
            self.editing = false;
        }
    }

    pub fn focus_section(&mut self, section: LocalChangesSection) {
        self.diff_focused = false;
        self.line_mode = false;
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
        self.diff_focused = false;
        self.line_mode = false;
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

    pub fn cycle_commit_item(&mut self, forward: bool) {
        self.commit_item = match (self.commit_item, forward) {
            (CommitItem::Message, true) | (CommitItem::Commit, false) => CommitItem::Amend,
            (CommitItem::Amend, true) | (CommitItem::Message, false) => CommitItem::Commit,
            (CommitItem::Commit, true) | (CommitItem::Amend, false) => CommitItem::Message,
        };
    }

    // Commits the staged changes with the typed message, amending HEAD while amend is checked.
    pub fn commit(&mut self) -> Result<(), String> {
        let message = self.message.text();
        if message.trim().is_empty() {
            return Err("Commit message is empty".into());
        }
        let amend = self.amend_base.is_some();
        if !amend && self.changes.staged.is_empty() {
            return Err("Nothing staged to commit".into());
        }
        self.repository.commit_staged(&message, amend)?;

        self.message = TextArea::default();
        self.amend_base = None;
        self.message_before_amend = None;
        self.commit_item = CommitItem::Message;
        self.committed = true;
        self.reload();
        Ok(())
    }

    // Returns true once after a commit was made.
    pub fn take_committed(&mut self) -> bool {
        std::mem::take(&mut self.committed)
    }

    // Asks for confirmation to discard the selected unstaged line, hunk, file or directory.
    pub fn request_discard(&mut self) {
        if self.commit_focused || self.section != LocalChangesSection::Unstaged {
            return;
        }
        let Some(row) = self.selected_row() else {
            return;
        };
        let path = if row.is_root() {
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
        self.discard_prompt = Some(format!("Discard {target} {path}?"));
    }

    pub fn is_confirming(&self) -> bool {
        self.discard_prompt.is_some()
    }

    // y or Enter discards, n or Esc cancels; other keys are ignored.
    pub fn handle_confirm_key(&mut self, key: KeyEvent) -> Result<(), String> {
        match key.code {
            KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                self.discard_prompt = None;
                let result = self.discard();
                self.reload();
                result
            }
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                self.discard_prompt = None;
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

        let Some(row) = self.selected_row() else {
            return Ok(());
        };
        let (untracked, tracked): (Vec<&LocalChangeFile>, Vec<&LocalChangeFile>) = self
            .changes
            .unstaged
            .iter()
            .filter(|f| row.contains(&f.path))
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

    // Goes one level deeper: files -> hunks -> lines of the selected hunk.
    // Does nothing when the diff has no hunk to select.
    // In the commit section, starts editing the message or toggles amend.
    pub fn enter(&mut self) -> Result<(), String> {
        if self.commit_focused {
            match self.commit_item {
                CommitItem::Message => self.editing = true,
                CommitItem::Amend => self.toggle_amend()?,
                CommitItem::Commit => self.commit()?,
            }
        } else if !self.diff_focused {
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
        } else if self.line_mode {
            self.line_mode = false;
            // the cursor may have moved into another hunk
            self.select_hunk(self.selected_hunk);
            true
        } else if self.diff_focused {
            self.diff_focused = false;
            true
        } else {
            false
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
        if self.commit_focused {
            // space on the checkbox toggles it like enter, and does nothing on the message
            return match self.commit_item {
                CommitItem::Amend => self.toggle_amend(),
                CommitItem::Commit => self.commit(),
                CommitItem::Message => Ok(()),
            };
        }
        let result = if self.line_mode {
            self.toggle_stage_line()
        } else if self.diff_focused {
            self.toggle_stage_hunk()
        } else {
            self.toggle_stage_row()
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
        match self.section {
            LocalChangesSection::Unstaged => self.repository.stage_paths(&[&row.path]),
            LocalChangesSection::Staged => {
                // a staged rename is only fully unstaged together with its source path
                let mut paths = vec![row.path.as_str()];
                paths.extend(
                    self.changes
                        .staged
                        .iter()
                        .filter(|f| row.contains(&f.path))
                        .filter_map(|f| f.orig_path.as_deref()),
                );
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
        self.unstaged_rows =
            build_rooted_file_tree_rows(self.changes.unstaged.iter().map(|f| f.path.as_str()));
        self.staged_rows =
            build_rooted_file_tree_rows(self.changes.staged.iter().map(|f| f.path.as_str()));
        self.unstaged_cursor =
            restore_cursor(&self.unstaged_rows, unstaged_path, self.unstaged_cursor);
        self.staged_cursor = restore_cursor(&self.staged_rows, staged_path, self.staged_cursor);

        let diff_focused = self.diff_focused;
        let line_mode = self.line_mode;
        let selected_hunk = self.selected_hunk;
        let selected_line = self.selected_line;
        self.load_diff();
        // stay in the diff while the same path still has hunks left
        if diff_focused && self.selected_path() == selected_path && !self.diff.hunks.is_empty() {
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
        let diff = self.selected_row().map(|row| {
            let untracked = row
                .file_index
                .is_some_and(|i| self.files()[i].status == LocalChangeStatus::Untracked);
            self.repository.local_change_diff(
                &row.path,
                untracked,
                staged,
                self.amend_base.as_deref(),
            )
        });
        self.diff = parse_diff(&diff.unwrap_or_default());
        self.diff_focused = false;
        self.line_mode = false;
        self.selected_hunk = 0;
        self.selected_line = 0;
        self.diff_offset = 0;
    }
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
        self.render_diff(state, diff_area, buf);
        self.render_commit(state, commit_area, buf);
        if let Some(prompt) = &state.discard_prompt {
            self.render_discard_popup(prompt, area, buf);
        }
    }
}

// top border, message box with 3 lines and its borders, amend checkbox
const COMMIT_AREA_HEIGHT: u16 = 1 + 5 + 1;

impl LocalChangesPanel<'_> {
    fn render_file_section(
        &self,
        state: &LocalChangesState,
        section: LocalChangesSection,
        area: Rect,
        buf: &mut Buffer,
    ) {
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
        let title_fg = if active && state.focused && !state.diff_focused && !state.commit_focused {
            self.ctx.color_theme.status_info_fg
        } else {
            self.ctx.color_theme.fg
        };
        let block = Block::default()
            .borders(Borders::TOP | Borders::RIGHT)
            .style(Style::default().fg(self.ctx.color_theme.divider_fg))
            .title(
                Span::raw(format!(
                    " [{number}] {name} ({}){} ",
                    files.len(),
                    if section == LocalChangesSection::Staged && state.amend_base.is_some() {
                        " · amending"
                    } else {
                        ""
                    }
                ))
                .fg(title_fg)
                .bold(),
            )
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        block.render(area, buf);

        if rows.is_empty() {
            Paragraph::new(
                Line::raw("No changes").fg(self.ctx.color_theme.status_input_transient_fg),
            )
            .render(inner, buf);
            return;
        }

        let lines: Vec<Line> = rows
            .iter()
            .map(|row| {
                let indent = "  ".repeat(row.depth);
                match row.file_index {
                    None => Line::from(vec![
                        Span::raw(indent),
                        Span::raw("▾ "),
                        Span::raw(row.name.clone()),
                    ]),
                    Some(i) => {
                        let status = files[i].status;
                        Line::from(vec![
                            Span::raw(indent),
                            Span::raw(status_symbol(status)).fg(self.status_color(status)),
                            Span::raw(" "),
                            Span::raw(row.name.clone()),
                        ])
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

        // only the section whose diff is displayed shows its cursor
        if active {
            let y = inner.y + (cursor - offset) as u16;
            if y < inner.bottom() {
                let highlight_style = Style::default()
                    .bg(self.ctx.color_theme.ref_selected_bg)
                    .fg(self.ctx.color_theme.ref_selected_fg);
                buf.set_style(Rect::new(inner.x, y, inner.width, 1), highlight_style);
            }
        }
    }

    fn render_diff(&self, state: &mut LocalChangesState, area: Rect, buf: &mut Buffer) {
        let mut title = match state.selected_row() {
            Some(row) if row.is_root() => "/".to_string(),
            Some(row) => row.path.clone(),
            None => String::new(),
        };
        if state.line_mode {
            title.push_str(" · select lines");
        } else if state.diff_focused {
            title.push_str(" · select hunks");
        }
        let title_fg = if state.diff_focused {
            self.ctx.color_theme.status_info_fg
        } else {
            self.ctx.color_theme.fg
        };
        let block = Block::default()
            .borders(Borders::TOP)
            .style(Style::default().fg(self.ctx.color_theme.divider_fg))
            .title(Span::raw(format!(" {title} ")).fg(title_fg))
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
        let title_fg = if focused {
            theme.status_info_fg
        } else {
            theme.fg
        };
        let block = Block::default()
            .borders(Borders::TOP)
            .style(Style::default().fg(theme.divider_fg))
            .title(Span::raw(" [3] Commit ").fg(title_fg).bold())
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        block.render(area, buf);

        let [message_area, amend_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);

        let message_selected = focused && state.commit_item == CommitItem::Message;
        let border_fg = if message_selected {
            theme.status_info_fg
        } else {
            theme.divider_fg
        };
        let mut message_block = Block::default()
            .borders(Borders::ALL)
            .style(Style::default().fg(border_fg));
        if state.editing {
            message_block = message_block.title(Span::raw(" editing · Esc to stop ").fg(border_fg));
        }
        let text_area = message_block.inner(message_area);
        message_block.render(message_area, buf);
        self.render_message(state, text_area, buf);

        // the checkbox on the left and the button on the right of the same line
        let checkbox = if state.amend_base.is_some() {
            "[x] Amend"
        } else {
            "[ ] Amend"
        };
        let button = if state.amend_base.is_some() {
            "[ Amend Commit ]"
        } else {
            "[ Commit ]"
        };
        let [checkbox_area, _, button_area] = Layout::horizontal([
            Constraint::Length(checkbox.chars().count() as u16),
            Constraint::Min(0),
            Constraint::Length(button.chars().count() as u16),
        ])
        .areas(amend_area);
        Paragraph::new(Line::raw(checkbox).fg(theme.fg)).render(checkbox_area, buf);
        Paragraph::new(Line::raw(button).fg(theme.fg).bold()).render(button_area, buf);

        let highlighted = match state.commit_item {
            CommitItem::Amend => Some(checkbox_area),
            CommitItem::Commit => Some(button_area),
            CommitItem::Message => None,
        };
        if let Some(highlighted) = highlighted.filter(|_| focused) {
            let highlight_style = Style::default()
                .bg(theme.ref_selected_bg)
                .fg(theme.ref_selected_fg);
            buf.set_style(highlighted, highlight_style);
        }
    }

    fn render_discard_popup(&self, prompt: &str, area: Rect, buf: &mut Buffer) {
        let theme = &self.ctx.color_theme;
        let hint = "y / Enter: discard    n / Esc: cancel";
        // text plus padding and borders
        let width = (prompt.chars().count().max(hint.len()) as u16 + 4).min(area.width);
        let height = 6.min(area.height);
        let popup = Rect::new(
            area.x + (area.width.saturating_sub(width)) / 2,
            area.y + (area.height.saturating_sub(height)) / 2,
            width,
            height,
        );
        Clear.render(popup, buf);
        let block = Block::default()
            .borders(Borders::ALL)
            .style(Style::default().fg(theme.status_warn_fg).bg(theme.bg))
            .title(Span::raw(" Discard changes ").bold())
            .padding(Padding::horizontal(1));
        let lines = vec![
            Line::raw(prompt.to_string()).fg(theme.fg),
            Line::raw("This cannot be undone.").fg(theme.status_warn_fg),
            Line::raw(""),
            Line::raw(hint).fg(theme.status_input_transient_fg),
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
    use crate::{git::SortCommit, test_support::git::GitRepository, widget::file_tree::ROOT_PATH};

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
        let staged_diff = repository.local_change_diff("a.txt", false, true, None);
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
        let staged_diff = |path| repository.local_change_diff(path, false, true, None);
        let unstaged_diff = |path| repository.local_change_diff(path, false, false, None);

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
        state.focus_commit();
        state.cycle_commit_item(true);
        state.enter()?;
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
        state.focus_commit();
        state.toggle_stage()?;
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

        // message, amend checkbox, commit button
        state.message = TextArea::new("first change\n\nbody");
        state.focus_commit();
        state.cycle_commit_item(false);
        state.enter()?;
        assert!(state.take_committed());
        assert!(!state.take_committed());
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
            state.discard_prompt.as_deref(),
            Some("Discard all changes to all files?")
        );
        state.handle_confirm_key(key(KeyCode::Char('y')))?;
        assert!(state.changes.unstaged.is_empty());
        assert_eq!(fs::read_to_string(repo_path.join("a.txt"))?, "a\n");
        assert!(!repo_path.join("u.txt").exists());

        Ok(())
    }
}
