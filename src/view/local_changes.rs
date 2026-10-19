use std::rc::Rc;

use ratatui::{crossterm::event::KeyEvent, layout::Rect, Frame};

use crate::widget::tree_query::Toward;
use crate::{
    app::AppContext,
    event::{AppEvent, Sender, UserEvent},
    external::{editor_command, merge_tool_command},
    git::FilesSnapshot,
    keybind::Scope,
    keys::KeyList,
    layout::{Direction, Panel},
    view::{
        copy_path, copy_selection, not_found, query_status, toggle_search_option, CopyForm,
        ListRefreshViewContext, RefreshViewContext,
    },
    widget::{
        commit_list::CommitListState,
        local_changes::{
            ExternalRequest, LocalChangesPanel, LocalChangesSection, LocalChangesSnapshot,
            LocalChangesState, Request,
        },
        tree_query::QueryKind,
    },
};

#[derive(Debug)]
pub struct LocalChangesView<'a> {
    commit_list_state: CommitListState<'a>,
    local_changes_state: LocalChangesState<'a>,
    // the last key's event, the repeats of a key moving the cursor loading no diff
    last_event: Option<UserEvent>,

    ctx: Rc<AppContext>,
    tx: Sender,
}

impl<'a> LocalChangesView<'a> {
    pub fn new(
        commit_list_state: CommitListState<'a>,
        local_changes_state: LocalChangesState<'a>,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> LocalChangesView<'a> {
        LocalChangesView {
            commit_list_state,
            local_changes_state,
            last_event: None,
            ctx,
            tx,
        }
    }

    // Before drawing: the diff of the row the cursor moved to.
    pub fn update_layout(&mut self, area: Rect) {
        self.local_changes_state.load_stale_diff();
        self.local_changes_state.update_submodule_height(area);
    }

    pub fn handle_event(&mut self, event: UserEvent, key: KeyEvent) {
        // held, a key moving the cursor loads the diff once it is let go (or drawn)
        if self.last_event != Some(event) {
            self.local_changes_state.load_stale_diff();
        }
        self.last_event = Some(event);
        self.handle_event_inner(event, key);
        self.send_request();
    }

    // Asks the app for what the state queued, so no request waits for an unrelated later key.
    fn send_request(&mut self) {
        match self.local_changes_state.take_request() {
            // continuing a rebase runs in the background, the app refreshing once it is done
            Some(Request::Task(task)) => self.tx.send(AppEvent::StartTask(task)),
            // a commit, discard, stash or abort asks first
            Some(Request::Dialog(dialog)) => self.tx.send(AppEvent::OpenDialog(Box::new(dialog))),
            Some(Request::External(request)) => self.run_external(request),
            None => {}
        }
    }

    fn handle_event_inner(&mut self, event: UserEvent, key: KeyEvent) {
        // while typing, keys arrive unmapped, see App::run
        if self.local_changes_state.is_editing() {
            self.local_changes_state.handle_text_key(key);
            return;
        }
        if self.local_changes_state.is_typing_query() {
            let kind = self.local_changes_state.query_kind();
            let toggled = toggle_search_option(&self.ctx, &key, kind);
            let options = self.ctx.search_options.get();
            if toggled {
                self.local_changes_state.reapply_query(options);
            } else {
                self.local_changes_state.handle_query_key(key, options);
            }
            self.update_query_status();
            return;
        }

        // the diff copies its hunk or line; it doesn't get the keys of the files beside it, see
        // `Scope::LocalChangesDiff`
        if self.local_changes_state.is_diff_focused() && event == UserEvent::ShortCopy {
            if let Some(text) = self.local_changes_state.selected_diff_text() {
                let part = self.local_changes_state.selected_diff_part();
                self.tx.send(copy_selection(text, part));
            }
            return;
        }

        if self.local_changes_state.is_conflict_focused() {
            // the sides are side by side, the tools below them
            let direction = match event {
                UserEvent::NavigateLeft => Some(Direction::Left),
                UserEvent::NavigateRight => Some(Direction::Right),
                UserEvent::NavigateUp | UserEvent::SelectUp => Some(Direction::Up),
                UserEvent::NavigateDown | UserEvent::SelectDown => Some(Direction::Down),
                _ => None,
            };
            if let Some(direction) = direction {
                self.local_changes_state.move_conflict_item(direction);
                return;
            }
        }
        // the keys of the file list do nothing in the commit section, the focus stays there
        if self.local_changes_state.is_commit_focused()
            && matches!(
                event,
                UserEvent::Search
                    | UserEvent::Filter
                    | UserEvent::GoToNext
                    | UserEvent::GoToPrevious
                    | UserEvent::ToggleSelect
                    | UserEvent::NextPicked
                    | UserEvent::PreviousPicked
                    | UserEvent::NextFile
                    | UserEvent::PreviousFile
                    | UserEvent::Discard
                    | UserEvent::NavigateLeft
                    | UserEvent::NavigateRight
            )
        {
            return;
        }

        let options = self.ctx.search_options.get();
        match event {
            UserEvent::Cancel => {
                // lines -> hunks -> files, or commit section -> files,
                // then clear the picked files, then the search, then the filter;
                // the page itself is left with tab, not closed
                let state = &mut self.local_changes_state;
                let _ = state.clear_diff_query()
                    || state.back()
                    || state.clear_picks()
                    || state.clear_query(options);
            }
            UserEvent::ToggleSelect => {
                self.local_changes_state.toggle_pick();
            }
            UserEvent::NextPicked => {
                self.local_changes_state.select_next_pick(Toward::Next);
            }
            UserEvent::PreviousPicked => {
                self.local_changes_state.select_next_pick(Toward::Previous);
            }
            UserEvent::NextFile | UserEvent::PreviousFile => {
                let toward = Toward::of(event, UserEvent::NextFile);
                let result = self.local_changes_state.select_next_file(toward);
                self.after_action(result);
            }
            // as of the last commit, the local changes aren't blamed
            UserEvent::Blame | UserEvent::Timeline => {
                let state = &self.local_changes_state;
                let head = state.repository().head_commit();
                if let (Some(path), Some(commit)) = (state.selected_file(), head) {
                    let path = path.into();
                    self.tx.send(match event {
                        UserEvent::Blame => AppEvent::OpenBlame {
                            commit,
                            path,
                            line: None,
                        },
                        _ => AppEvent::OpenTimeline { commit, path },
                    });
                }
            }
            UserEvent::Edit => {
                if let Err(err) = self.local_changes_state.request_edit() {
                    self.tx.send(AppEvent::NotifyError(err));
                }
            }
            UserEvent::Commit => {
                let result = self.local_changes_state.commit_or_continue();
                self.after_action(result);
            }
            UserEvent::Abort => {
                self.local_changes_state.request_abort();
            }
            UserEvent::ToggleAmend => {
                if let Err(err) = self.local_changes_state.toggle_amend() {
                    self.tx.send(AppEvent::NotifyError(err));
                }
            }
            UserEvent::Stash => {
                if let Err(err) = self.local_changes_state.start_stash() {
                    self.tx.send(AppEvent::NotifyError(err));
                }
            }
            UserEvent::Search => {
                self.local_changes_state.start_query(QueryKind::Search);
                self.update_query_status();
            }
            UserEvent::Filter => {
                self.local_changes_state.start_query(QueryKind::Filter);
                self.update_query_status();
            }
            UserEvent::GoToNext | UserEvent::GoToPrevious => {
                let state = &mut self.local_changes_state;
                if !state.search_next(Toward::of(event, UserEvent::GoToNext), options) {
                    let search = state
                        .active_query()
                        .search()
                        .unwrap_or_default()
                        .to_string();
                    self.tx
                        .send(not_found(&self.ctx, search, state.query_kind()));
                }
            }
            UserEvent::Confirm => {
                let result = self.local_changes_state.enter();
                self.after_action(result);
            }
            UserEvent::ToggleStage => {
                let result = self.local_changes_state.toggle_stage();
                self.after_action(result);
            }
            UserEvent::Discard => {
                if let Err(err) = self.local_changes_state.request_discard() {
                    self.tx.send(AppEvent::NotifyError(err));
                }
            }
            UserEvent::FocusPanel(1) => {
                self.local_changes_state
                    .focus_section(LocalChangesSection::Unstaged);
            }
            UserEvent::FocusPanel(2) => {
                self.local_changes_state
                    .focus_section(LocalChangesSection::Staged);
            }
            UserEvent::FocusPanel(3) => {
                self.local_changes_state.focus_commit();
            }
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                self.local_changes_state.select_next();
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                self.local_changes_state.select_prev();
            }
            UserEvent::GoToTop => {
                self.local_changes_state.select_first();
            }
            UserEvent::GoToBottom => {
                self.local_changes_state.select_last();
            }
            UserEvent::NavigateRight => {
                self.local_changes_state.expand();
            }
            UserEvent::NavigateLeft => {
                self.local_changes_state.collapse();
            }
            // every scroll key scrolls what has the focus: the files or the submodule move, the
            // commit message, the diff or the conflict sides scroll
            UserEvent::HalfPageDown
            | UserEvent::HalfPageUp
            | UserEvent::PageDown
            | UserEvent::PageUp
            | UserEvent::ScrollDown
            | UserEvent::ScrollUp => {
                self.local_changes_state.scroll(event);
            }
            // y and Y copy the path of the selected file
            UserEvent::ShortCopy | UserEvent::FullCopy
                if !self.local_changes_state.is_commit_focused() =>
            {
                if let Some(row) = self.local_changes_state.selected_row() {
                    let repository = self.local_changes_state.repository();
                    self.tx
                        .send(copy_path(repository, row, CopyForm::of(event)));
                }
            }
            UserEvent::Ignore if self.local_changes_state.is_files_focused() => {
                match self.local_changes_state.ignore_selected() {
                    Ok(message) if message.is_empty() => {}
                    Ok(message) => self.tx.send(AppEvent::NotifySuccess(message)),
                    Err(err) => self.tx.send(AppEvent::NotifyError(err)),
                }
            }
            _ => {}
        }
    }

    pub fn keys(&self) -> KeyList {
        self.local_changes_state.keys()
    }

    pub fn move_focus(&mut self, direction: Direction) -> bool {
        let result = self.local_changes_state.move_focus(direction);
        let moved = *result.as_ref().unwrap_or(&true);
        self.after_action(result.map(|_| ()));
        self.send_request();
        moved
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, focused: bool) {
        let panel = LocalChangesPanel::new(self.ctx.clone(), focused);
        f.render_stateful_widget(panel, area, &mut self.local_changes_state);
    }
}

impl<'a> LocalChangesView<'a> {
    pub fn into_list_state(self) -> CommitListState<'a> {
        self.commit_list_state
    }

    fn as_list_state(&self) -> &CommitListState<'a> {
        &self.commit_list_state
    }

    // Turns amending on, with the message of the last commit in the commit section.
    pub fn start_amend(&mut self) -> Result<(), String> {
        let result = self.local_changes_state.start_amend();
        self.send_request();
        result
    }

    // Another panel takes the focus, leaving the diff and the message.
    pub fn blur(&mut self) {
        self.local_changes_state.focus_files();
    }

    pub fn local_change_count(&self) -> usize {
        self.local_changes_state.count()
    }

    pub fn graph_image_ids_sorted(&self) -> Vec<u32> {
        let mut ids = self.as_list_state().graph_image_ids_sorted();
        ids.extend(self.local_changes_state.submodule_graph_image_ids());
        ids
    }

    // The submodule panel's graph images, drawn beside the files, see `CommitListState`.
    pub fn prepare_graph_uploads(&mut self) {
        self.local_changes_state.prepare_graph_uploads();
    }

    pub fn drain_pending_graph_uploads(&mut self) -> Vec<String> {
        self.local_changes_state.drain_pending_graph_uploads()
    }

    // Runs the editor or merge tool the state asked for, in place of the app.
    fn run_external(&mut self, request: ExternalRequest) {
        let command = match request {
            ExternalRequest::Editor(path) => editor_command(&path),
            ExternalRequest::MergeTool(path) => {
                let tool = self.ctx.core_config.git.merge_tool.as_deref();
                merge_tool_command(self.local_changes_state.toplevel(), &path, tool)
            }
        };
        self.tx.send(AppEvent::RunExternal(command));
    }

    pub fn is_capturing_keys(&self) -> bool {
        let state = &self.local_changes_state;
        state.is_editing() || state.is_typing_query()
    }

    // Shows the search or filter being typed in the status line.
    fn update_query_status(&self) {
        let state = &self.local_changes_state;
        let event = query_status(&self.ctx, state.active_query(), state.query_kind());
        self.tx.send(event);
    }

    fn after_action(&mut self, result: Result<(), String>) {
        if let Err(err) = result {
            self.tx.send(AppEvent::NotifyError(err));
        }
    }

    // The panel whose height changes with ctrl-up and ctrl-down: the unstaged files over the
    // staged ones from them, else the commit message under the diff.
    pub fn height_panel(&self) -> Panel {
        if self.local_changes_state.is_files_focused() {
            Panel::Unstaged
        } else {
            Panel::Commit
        }
    }

    // The scope of the keys: the diff, or the rest of the page.
    pub fn scope(&self) -> Scope {
        if self.local_changes_state.is_diff_focused() {
            Scope::LocalChangesDiff
        } else {
            Scope::LocalChanges
        }
    }

    // Reloads the changes, keeping the selection, for those made outside the app.
    pub fn reload_files(&mut self, files: FilesSnapshot) {
        self.local_changes_state.reload_if_changed(files);
        self.send_request();
    }

    // Empties the message box once its commit is made.
    pub fn clear_draft(&mut self) {
        self.local_changes_state.clear_draft();
    }

    // What to restore of the page, after a refresh or coming back to it.
    pub fn context(&self) -> LocalChangesSnapshot {
        self.local_changes_state.snapshot()
    }

    pub fn refresh(&self) {
        self.tx.send(AppEvent::Refresh(self.refresh_context()));
    }

    // What to show again once the repository is reloaded.
    pub fn refresh_context(&self) -> RefreshViewContext {
        RefreshViewContext::LocalChanges {
            list_context: ListRefreshViewContext::from(self.as_list_state()),
            local_changes_context: self.context(),
        }
    }
}
