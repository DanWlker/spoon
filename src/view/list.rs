use std::rc::Rc;

use ratatui::{crossterm::event::KeyEvent, layout::Rect, Frame};

use crate::{
    app::AppContext,
    event::{hint, AppEvent, KeyHint, Sender, UserEvent},
    git::CommitHash,
    view::{graph_hints, typing_hints, ListRefreshViewContext, RefreshViewContext},
    widget::{
        commit_list::{
            commit_list_inner_height, render_commit_list_panel, CommitListState, SearchState,
        },
        commit_tabs::DetailTab,
    },
};

#[derive(Debug)]
pub struct ListView<'a> {
    commit_list_state: Option<CommitListState<'a>>,

    ctx: Rc<AppContext>,
    tx: Sender,
}

impl<'a> ListView<'a> {
    pub fn new(
        commit_list_state: CommitListState<'a>,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> ListView<'a> {
        ListView {
            commit_list_state: Some(commit_list_state),
            ctx,
            tx,
        }
    }

    pub fn handle_event(&mut self, event: UserEvent, key: KeyEvent) {
        let tx = self.tx.clone();
        if handle_search_event(self.as_mut_list_state(), &tx, event, key) {
            return;
        }
        match event {
            UserEvent::Quit => {
                self.tx.send(AppEvent::Quit);
            }
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                self.as_mut_list_state().select_next();
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                self.as_mut_list_state().select_prev();
            }
            UserEvent::GoToTop => {
                self.as_mut_list_state().select_first();
            }
            UserEvent::GoToBottom => {
                self.as_mut_list_state().select_last();
            }
            UserEvent::ScrollDown => {
                self.as_mut_list_state().scroll_down();
            }
            UserEvent::ScrollUp => {
                self.as_mut_list_state().scroll_up();
            }
            UserEvent::PageDown => {
                self.as_mut_list_state().scroll_down_page();
            }
            UserEvent::PageUp => {
                self.as_mut_list_state().scroll_up_page();
            }
            UserEvent::HalfPageDown => {
                self.as_mut_list_state().scroll_down_half();
            }
            UserEvent::HalfPageUp => {
                self.as_mut_list_state().scroll_up_half();
            }
            UserEvent::ShortCopy => {
                self.copy_commit_short_hash();
            }
            UserEvent::FullCopy => {
                self.copy_commit_hash();
            }
            UserEvent::UserCommand(n) => {
                self.tx.send(AppEvent::OpenUserCommand(n));
            }
            UserEvent::HelpToggle => {
                self.tx.send(AppEvent::OpenHelp);
            }
            // an applied search is cleared first, see `handle_search_event`
            UserEvent::Cancel => {
                self.as_mut_list_state().clear_picks();
            }
            UserEvent::ToggleSelect => {
                self.as_mut_list_state().toggle_pick();
            }
            UserEvent::NextPicked => {
                self.as_mut_list_state().select_next_pick(true);
            }
            UserEvent::PreviousPicked => {
                self.as_mut_list_state().select_next_pick(false);
            }
            UserEvent::Confirm => {
                self.tx.send(AppEvent::OpenDetail(DetailTab::Info));
            }
            UserEvent::FocusPanel(n) => {
                // the panels of the details, opened right away
                if let Some(tab) = DetailTab::from_number(n) {
                    self.tx.send(AppEvent::OpenDetail(tab));
                }
            }
            UserEvent::Refresh => {
                self.refresh();
            }
            _ => {}
        }
    }

    pub fn key_hints(&self) -> Vec<KeyHint> {
        let state = self.as_list_state();
        if let SearchState::Searching { .. } = state.search_state() {
            return typing_hints(true);
        }
        let mut hints = vec![hint(UserEvent::Confirm, "details")];
        hints.extend(graph_hints(state.picked_commit_hashes().len()));
        hints
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, focused: bool) {
        let ctx = self.ctx.clone();
        render_commit_list_panel(f, area, self.as_mut_list_state(), ctx, focused);
    }

    pub fn update_layout(&mut self, area: Rect) {
        self.as_mut_list_state()
            .update_height(commit_list_inner_height(area.height));
    }

    pub fn prepare_graph_uploads(&mut self) {
        self.as_mut_list_state().ensure_visible_graph_uploaded();
    }
}

impl<'a> ListView<'a> {
    pub fn take_list_state(&mut self) -> CommitListState<'a> {
        self.commit_list_state.take().unwrap()
    }

    fn as_mut_list_state(&mut self) -> &mut CommitListState<'a> {
        self.commit_list_state.as_mut().unwrap()
    }

    pub fn clear_picks(&mut self) {
        self.as_mut_list_state().clear_picks();
    }

    pub fn select_ref(&mut self, name: &str) {
        self.as_mut_list_state().select_ref(name);
    }

    pub fn as_list_state(&self) -> &CommitListState<'a> {
        self.commit_list_state.as_ref().unwrap()
    }

    pub fn drain_pending_graph_uploads(&mut self) -> Vec<String> {
        self.as_mut_list_state().drain_pending_graph_uploads()
    }

    pub fn graph_image_ids_sorted(&self) -> Vec<u32> {
        self.as_list_state().graph_image_ids_sorted()
    }

    fn copy_commit_short_hash(&self) {
        let selected = self.as_list_state().selected_commit_hash();
        self.copy_to_clipboard("Commit SHA (short)".into(), selected.as_short_hash().into());
    }

    fn copy_commit_hash(&self) {
        let selected = self.as_list_state().selected_commit_hash();
        self.copy_to_clipboard("Commit SHA".into(), selected.as_str().into());
    }

    fn copy_to_clipboard(&self, name: String, value: String) {
        self.tx.send(AppEvent::CopyToClipboard { name, value });
    }

    pub fn refresh(&self) {
        self.tx.send(AppEvent::Refresh(self.refresh_context()));
    }

    // What to show again once the repository is reloaded.
    pub fn refresh_context(&self) -> RefreshViewContext {
        let list_context = ListRefreshViewContext::from(self.as_list_state());
        RefreshViewContext::List { list_context }
    }

    pub fn reset_commit_list_with(&mut self, list_context: &ListRefreshViewContext) {
        let ListRefreshViewContext {
            commit_hash,
            picked_commit_hashes,
            selected,
            height,
            scroll_to_top,
            search_options,
            search_context,
        } = list_context;
        let list_state = self.as_mut_list_state();
        list_state.restore_picked_commit_hashes(picked_commit_hashes.clone());
        list_state.restore_search_options(*search_options);
        list_state.reset_height(*height);
        if *scroll_to_top {
            list_state.select_first();
        } else {
            list_state.select_commit_hash(&CommitHash::from(commit_hash.as_str()));
            list_state.restore_selected_row(*selected);
        }
        if let Some(search_context) = search_context {
            list_state.restore_search(search_context);
        }
    }
}

// Searching the commit list, from the list or from the details beside it: typing the query, its
// options, going through the matches and clearing it. Returns whether the event was one of these.
pub fn handle_search_event(
    state: &mut CommitListState,
    tx: &Sender,
    event: UserEvent,
    key: KeyEvent,
) -> bool {
    match (state.search_state(), event) {
        (SearchState::Searching { .. }, UserEvent::Confirm) => {
            state.apply_search();
            update_matched_message(state, tx);
        }
        (SearchState::Searching { .. }, UserEvent::Cancel) => {
            state.cancel_search();
            tx.send(AppEvent::ClearStatusLine);
        }
        (SearchState::Searching { .. }, UserEvent::SearchTargetToggle) => {
            state.toggle_search_target();
            update_search_status(state, tx);
        }
        (SearchState::Searching { .. }, UserEvent::IgnoreCaseToggle) => {
            state.toggle_ignore_case();
            update_search_status(state, tx);
        }
        (SearchState::Searching { .. }, UserEvent::FuzzyToggle) => {
            state.toggle_fuzzy();
            update_search_status(state, tx);
        }
        (SearchState::Searching { .. }, _) => {
            state.handle_search_input(key);
            update_search_status(state, tx);
        }
        (_, UserEvent::Search) => {
            state.start_search();
            update_search_status(state, tx);
        }
        (_, UserEvent::SearchTargetToggle) => {
            state.toggle_search_target();
            update_search_options_message(state, tx);
        }
        (_, UserEvent::IgnoreCaseToggle) => {
            state.toggle_ignore_case();
            update_search_options_message(state, tx);
        }
        (_, UserEvent::FuzzyToggle) => {
            state.toggle_fuzzy();
            update_search_options_message(state, tx);
        }
        (SearchState::Applied { .. }, UserEvent::GoToNext) => {
            state.select_next_match();
            update_matched_message(state, tx);
        }
        (SearchState::Applied { .. }, UserEvent::GoToPrevious) => {
            state.select_prev_match();
            update_matched_message(state, tx);
        }
        // an applied search is cleared before the picked commits
        (SearchState::Applied { .. }, UserEvent::Cancel) => {
            state.cancel_search();
            tx.send(AppEvent::ClearStatusLine);
        }
        _ => return false,
    }
    true
}

fn update_search_status(state: &CommitListState, tx: &Sender) {
    if let SearchState::Searching { .. } = state.search_state() {
        if let Some(query) = state.search_query_string() {
            tx.send(AppEvent::UpdateStatusInput {
                message: query,
                cursor_position: state.search_query_cursor_position(),
                metadata: state.search_options().status_string(),
            });
        }
    }
}

fn update_search_options_message(state: &CommitListState, tx: &Sender) {
    if let SearchState::Applied { .. } = state.search_state() {
        update_matched_message(state, tx);
    } else {
        let options = state.search_options().status_string();
        tx.send(AppEvent::UpdateStatusTransient(format!(
            "Search: {options}"
        )));
    }
}

fn update_matched_message(state: &CommitListState, tx: &Sender) {
    if let Some((msg, matched)) = state.matched_query_string() {
        tx.send(AppEvent::UpdateSearchResult {
            message: msg,
            options: state.search_options().status_string(),
            matched,
        });
    } else {
        tx.send(AppEvent::ClearStatusLine);
    }
}
