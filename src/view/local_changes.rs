use std::rc::Rc;

use ratatui::{
    crossterm::event::{KeyCode, KeyEvent},
    layout::Rect,
    Frame,
};

use crate::{
    app::AppContext,
    event::{AppEvent, KeyHint, Page, Sender, UserEvent, UserEventWithCount},
    external::{editor_command, merge_tool_command},
    view::{copy_path, ListRefreshViewContext, RefreshViewContext},
    widget::{
        commit_list::CommitListState,
        local_changes::{
            ConflictMove, ExternalRequest, LocalChangesPanel, LocalChangesSection,
            LocalChangesSnapshot, LocalChangesState,
        },
        tree_query::QueryKind,
    },
};

#[derive(Debug)]
pub struct LocalChangesView<'a> {
    commit_list_state: Option<CommitListState<'a>>,
    local_changes_state: LocalChangesState<'a>,

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
            commit_list_state: Some(commit_list_state),
            local_changes_state,
            ctx,
            tx,
        }
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, key: KeyEvent) {
        self.handle_event_inner(event_with_count, key);
        self.run_external_request();
    }

    fn handle_event_inner(&mut self, event_with_count: UserEventWithCount, key: KeyEvent) {
        // like lazygit, the files are not changed while a git command runs in the background,
        // which could fail on the locked index or end up in the middle of its stash and pop
        if self.changes_repository(event_with_count.event, key) {
            if let Some(message) = self.ctx.busy_message() {
                self.tx.send(AppEvent::NotifyWarn(message));
                return;
            }
        }
        // while confirming or typing, keys arrive unmapped, see App::run
        if self.as_state().is_confirming() {
            let result = if self.confirms_commit(key) {
                self.as_mut_state().confirm()
            } else {
                self.as_mut_state().handle_confirm_key(key)
            };
            self.after_action(result);
            return;
        }
        if self.as_state().is_editing() {
            self.as_mut_state().handle_text_key(key);
            return;
        }
        if self.as_state().is_typing_query() {
            self.as_mut_state().handle_query_key(key);
            self.update_query_status();
            return;
        }
        if self.as_state().is_typing_stash() {
            let result = self.as_mut_state().handle_stash_key(key);
            self.update_stash_status();
            self.after_action(result);
            return;
        }

        let event = event_with_count.event;
        let count = event_with_count.count;

        // the diff only takes its own keys, not those of the files beside it
        if self.as_state().is_diff_focused() {
            match event {
                UserEvent::ToggleSelect
                | UserEvent::NextPicked
                | UserEvent::PreviousPicked
                | UserEvent::Filter
                | UserEvent::FullCopy
                | UserEvent::NavigateLeft
                | UserEvent::NavigateRight => return,
                UserEvent::ShortCopy => {
                    if let Some(text) = self.as_state().selected_diff_text() {
                        let name = if self.as_state().is_selecting_lines() {
                            "Line"
                        } else {
                            "Hunk"
                        };
                        self.tx.send(AppEvent::CopyToClipboard {
                            name: name.into(),
                            value: text,
                        });
                    }
                    return;
                }
                _ => {}
            }
        }

        if self.as_state().is_conflict_focused() {
            // the sides are side by side, the tools below them
            let direction = match event {
                UserEvent::NavigateLeft => Some(ConflictMove::Left),
                UserEvent::NavigateRight => Some(ConflictMove::Right),
                UserEvent::NavigateUp | UserEvent::SelectUp => Some(ConflictMove::Up),
                UserEvent::NavigateDown | UserEvent::SelectDown => Some(ConflictMove::Down),
                _ => None,
            };
            if let Some(direction) = direction {
                self.as_mut_state().move_conflict_item(direction);
                return;
            }
        }
        if self.as_state().is_commit_focused() {
            // the commit section only has the message, there is nothing to move to
            if let UserEvent::NavigateDown
            | UserEvent::NavigateUp
            | UserEvent::NavigateLeft
            | UserEvent::NavigateRight
            | UserEvent::SelectDown
            | UserEvent::SelectUp = event
            {
                return;
            }
        }

        match event {
            UserEvent::Quit => {
                self.tx.send(AppEvent::Quit);
            }
            UserEvent::Cancel | UserEvent::Close => {
                // lines -> hunks -> files, or commit section -> files,
                // then clear the picked files, then the search, then the filter;
                // the page itself is left with tab, not closed
                let state = self.as_mut_state();
                let _ = state.clear_diff_query()
                    || state.back()
                    || state.clear_picks()
                    || state.clear_query();
            }
            UserEvent::ToggleSelect => {
                self.as_mut_state().toggle_pick();
            }
            UserEvent::NextPicked => {
                self.as_mut_state().select_next_pick(true);
            }
            UserEvent::PreviousPicked => {
                self.as_mut_state().select_next_pick(false);
            }
            UserEvent::Edit => {
                if let Err(err) = self.as_mut_state().request_edit() {
                    self.tx.send(AppEvent::NotifyError(err));
                }
            }
            UserEvent::Commit => {
                let result = self.as_mut_state().commit_or_continue();
                self.after_action(result);
            }
            UserEvent::Abort => {
                if let Err(err) = self.as_mut_state().request_abort() {
                    self.tx.send(AppEvent::NotifyError(err));
                }
            }
            UserEvent::ToggleAmend => {
                if let Err(err) = self.as_mut_state().toggle_amend() {
                    self.tx.send(AppEvent::NotifyError(err));
                }
            }
            UserEvent::Stash => match self.as_mut_state().start_stash() {
                Ok(()) => self.update_stash_status(),
                Err(err) => self.tx.send(AppEvent::NotifyError(err)),
            },
            UserEvent::Search => {
                self.as_mut_state().start_query(QueryKind::Search);
                self.update_query_status();
            }
            UserEvent::Filter => {
                self.as_mut_state().start_query(QueryKind::Filter);
                self.update_query_status();
            }
            UserEvent::GoToNext => {
                self.as_mut_state().search_next(true);
            }
            UserEvent::GoToPrevious => {
                self.as_mut_state().search_next(false);
            }
            UserEvent::Confirm => {
                let result = self.as_mut_state().enter();
                self.after_action(result);
            }
            UserEvent::ToggleStage => {
                let result = self.as_mut_state().toggle_stage();
                self.after_action(result);
            }
            UserEvent::Discard => {
                self.as_mut_state().request_discard();
            }
            UserEvent::FocusPanel(1) => {
                self.as_mut_state()
                    .focus_section(LocalChangesSection::Unstaged);
            }
            UserEvent::FocusPanel(2) => {
                self.as_mut_state()
                    .focus_section(LocalChangesSection::Staged);
            }
            UserEvent::FocusPanel(3) => {
                self.as_mut_state().edit_message();
            }
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                for _ in 0..count {
                    self.as_mut_state().select_next();
                }
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                for _ in 0..count {
                    self.as_mut_state().select_prev();
                }
            }
            UserEvent::GoToTop => {
                self.as_mut_state().select_first();
            }
            UserEvent::GoToBottom => {
                self.as_mut_state().select_last();
            }
            UserEvent::NavigateRight => {
                self.as_mut_state().expand();
            }
            UserEvent::NavigateLeft => {
                self.as_mut_state().collapse();
            }
            UserEvent::ScrollDown => {
                self.as_mut_state().scroll_diff_down(count);
            }
            UserEvent::ScrollUp => {
                self.as_mut_state().scroll_diff_up(count);
            }
            UserEvent::PageDown => {
                let n = self.as_state().diff_page_height() * count;
                self.as_mut_state().scroll_diff_down(n);
            }
            UserEvent::PageUp => {
                let n = self.as_state().diff_page_height() * count;
                self.as_mut_state().scroll_diff_up(n);
            }
            UserEvent::HalfPageDown => {
                let n = self.as_state().diff_page_height() / 2 * count;
                self.as_mut_state().scroll_diff_down(n);
            }
            UserEvent::HalfPageUp => {
                let n = self.as_state().diff_page_height() / 2 * count;
                self.as_mut_state().scroll_diff_up(n);
            }
            UserEvent::HelpToggle => {
                self.tx.send(AppEvent::OpenHelp);
            }
            // y and Y copy the path of the selected file
            UserEvent::ShortCopy | UserEvent::FullCopy if !self.as_state().is_commit_focused() => {
                if let Some(path) = self.as_state().selected_path() {
                    let full = event == UserEvent::FullCopy;
                    let repository = self.as_state().repository();
                    self.tx.send(copy_path(repository, &path, full));
                }
            }
            UserEvent::Refresh => {
                self.refresh();
            }
            _ => {}
        }
    }

    pub fn key_hints(&self) -> Vec<KeyHint> {
        self.as_state().key_hints()
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, focused: bool) {
        let panel = LocalChangesPanel::new(self.ctx.clone(), focused);
        f.render_stateful_widget(panel, area, self.as_mut_state());
    }
}

impl<'a> LocalChangesView<'a> {
    pub fn take_list_state(&mut self) -> CommitListState<'a> {
        self.commit_list_state.take().unwrap()
    }

    fn as_list_state(&self) -> &CommitListState<'a> {
        self.commit_list_state.as_ref().unwrap()
    }

    fn as_state(&self) -> &LocalChangesState<'a> {
        &self.local_changes_state
    }

    fn as_mut_state(&mut self) -> &mut LocalChangesState<'a> {
        &mut self.local_changes_state
    }

    // Turns amending on, with the message of the last commit in the commit section.
    pub fn start_amend(&mut self) -> Result<(), String> {
        self.as_mut_state().start_amend()
    }

    // Another panel takes the focus, leaving the diff and the message.
    pub fn blur(&mut self) {
        self.as_mut_state().focus_files();
    }

    pub fn local_change_count(&self) -> usize {
        self.as_state().count()
    }

    pub fn graph_image_ids_sorted(&self) -> Vec<u32> {
        self.as_list_state().graph_image_ids_sorted()
    }

    // Runs the editor or merge tool the state asked for, in place of the app.
    fn run_external_request(&mut self) {
        let Some(request) = self.as_mut_state().take_external_request() else {
            return;
        };
        let command = match request {
            ExternalRequest::Editor(path) => editor_command(&path),
            ExternalRequest::MergeTool(path) => {
                let tool = self.ctx.core_config.git.merge_tool.as_deref();
                merge_tool_command(self.as_state().toplevel(), &path, tool)
            }
        };
        self.tx.send(AppEvent::RunExternal(command));
    }

    pub fn is_capturing_keys(&self) -> bool {
        let state = self.as_state();
        state.is_editing()
            || state.is_confirming()
            || state.is_typing_query()
            || state.is_typing_stash()
    }

    // Shows the stash message being typed in the status line.
    fn update_stash_status(&self) {
        match self.as_state().stash_status_input() {
            Some((message, cursor_position, metadata)) => {
                self.tx.send(AppEvent::UpdateStatusInput {
                    message,
                    cursor_position,
                    metadata,
                })
            }
            None => self.tx.send(AppEvent::ClearStatusLine),
        }
    }

    // Shows the search or filter being typed in the status line.
    fn update_query_status(&self) {
        match self.as_state().query_status_input() {
            Some((message, cursor_position)) => self.tx.send(AppEvent::UpdateStatusInput {
                message,
                cursor_position,
                metadata: String::new(),
            }),
            None => self.tx.send(AppEvent::ClearStatusLine),
        }
    }

    // Whether the key is the commit key, while the popup asks to confirm a commit.
    fn confirms_commit(&self, key: KeyEvent) -> bool {
        self.as_state().is_confirming_commit()
            && self.ctx.keybind.get_on(&[Page::LocalChanges], &key) == Some(UserEvent::Commit)
    }

    // Whether the key may change the repository: staging, discarding, stashing, committing,
    // aborting or resolving a conflict, or confirming one of these.
    fn changes_repository(&self, event: UserEvent, key: KeyEvent) -> bool {
        let state = self.as_state();
        if state.is_confirming() {
            return key.code == KeyCode::Enter || self.confirms_commit(key);
        }
        if state.is_typing_stash() {
            return key.code == KeyCode::Enter;
        }
        if state.is_editing() || state.is_typing_query() {
            return false;
        }
        match event {
            UserEvent::Commit | UserEvent::Abort | UserEvent::Stash | UserEvent::Discard => true,
            // in the commit section, space types the message and enter commits (or continues)
            UserEvent::ToggleStage => !state.is_commit_focused(),
            UserEvent::Confirm => state.is_conflict_focused() || state.is_commit_focused(),
            _ => false,
        }
    }

    // Like lazygit, reloads the repository after anything that may have changed it, whether it
    // succeeded or not: a failed continue may still have applied some commits before it stopped.
    fn after_action(&mut self, result: Result<(), String>) {
        if self.as_mut_state().take_needs_refresh() {
            self.refresh();
        }
        if let Err(err) = result {
            self.tx.send(AppEvent::NotifyError(err));
        }
    }

    // Reloads the changes, keeping the selection, for those made outside the app.
    pub fn reload_files(&mut self) {
        self.as_mut_state().reload_if_changed();
    }

    // What to restore of the page, after a refresh or coming back to it.
    pub fn context(&self) -> LocalChangesSnapshot {
        self.as_state().snapshot()
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
