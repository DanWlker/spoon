use std::rc::Rc;

use ratatui::{
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    Frame,
};

use crate::{
    app::AppContext,
    event::{AppEvent, Sender, UserEvent, UserEventWithCount},
    git::Ref,
    view::{ListRefreshViewContext, RefreshViewContext, RefsRefreshViewContext},
    widget::{
        commit_list::{CommitList, CommitListState},
        local_changes::{LocalChangesPanel, LocalChangesState},
        ref_list::{RefList, RefListState},
    },
};

#[derive(Debug)]
pub struct RefsView<'a> {
    commit_list_state: Option<CommitListState<'a>>,
    ref_list_state: RefListState,
    local_changes_state: Option<LocalChangesState<'a>>,
    showing_local_changes: bool,

    ctx: Rc<AppContext>,
    tx: Sender,
}

impl<'a> RefsView<'a> {
    pub fn new(
        commit_list_state: CommitListState<'a>,
        refs: &[&Ref],
        mut local_changes_state: LocalChangesState<'a>,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> RefsView<'a> {
        local_changes_state.set_focused(false);
        let mut view = RefsView {
            commit_list_state: Some(commit_list_state),
            ref_list_state: RefListState::new(refs, local_changes_state.count()),
            local_changes_state: Some(local_changes_state),
            showing_local_changes: false,
            ctx,
            tx,
        };
        // local changes is selected initially, replacing the graph
        view.update_local_changes_visibility();
        view
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, _: KeyEvent) {
        let event = event_with_count.event;
        let count = event_with_count.count;

        match event {
            UserEvent::Quit => {
                self.tx.send(AppEvent::Quit);
            }
            UserEvent::Cancel | UserEvent::Close | UserEvent::RefList => {
                // close to whatever is shown: the local changes page or the commit list
                self.tx.send(AppEvent::CloseRefs {
                    to_local_changes: self.showing_local_changes,
                });
            }
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                for _ in 0..count {
                    self.ref_list_state.select_next();
                }
                self.update_commit_list_selected();
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                for _ in 0..count {
                    self.ref_list_state.select_prev();
                }
                self.update_commit_list_selected();
            }
            UserEvent::GoToTop => {
                self.ref_list_state.select_first();
                self.update_commit_list_selected();
            }
            UserEvent::GoToBottom => {
                self.ref_list_state.select_last();
                self.update_commit_list_selected();
            }
            UserEvent::NavigateRight => {
                self.ref_list_state.open_node();
                self.update_commit_list_selected();
            }
            UserEvent::NavigateLeft => {
                self.ref_list_state.close_node();
                self.update_commit_list_selected();
            }
            UserEvent::ShortCopy | UserEvent::FullCopy => {
                self.copy_ref_name();
            }
            UserEvent::HelpToggle => {
                self.tx.send(AppEvent::OpenHelp);
            }
            UserEvent::Refresh => {
                self.refresh();
            }
            _ => {}
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let [list_area, refs_area] = self.split_areas(area);

        if self.showing_local_changes {
            let panel = LocalChangesPanel::new(self.ctx.clone());
            f.render_stateful_widget(panel, list_area, self.as_mut_local_changes_state());
        } else {
            let commit_list = CommitList::new(self.ctx.clone());
            f.render_stateful_widget(commit_list, list_area, self.as_mut_list_state());
        }

        let ref_list = RefList::new(self.ctx.clone());
        f.render_stateful_widget(ref_list, refs_area, &mut self.ref_list_state);
    }

    pub fn update_layout(&mut self, area: Rect) {
        let [list_area, _] = self.split_areas(area);
        self.as_mut_list_state()
            .update_height(list_area.height as usize);
    }

    pub fn prepare_graph_uploads(&mut self) {
        self.as_mut_list_state().ensure_visible_graph_uploaded();
    }
}

impl<'a> RefsView<'a> {
    pub fn take_list_state(&mut self) -> CommitListState<'a> {
        self.commit_list_state.take().unwrap()
    }

    pub fn take_local_changes_state(&mut self) -> LocalChangesState<'a> {
        self.local_changes_state.take().unwrap()
    }

    fn as_mut_local_changes_state(&mut self) -> &mut LocalChangesState<'a> {
        self.local_changes_state.as_mut().unwrap()
    }

    pub fn is_showing_local_changes(&self) -> bool {
        self.showing_local_changes
    }

    // Used when coming from the local changes page, to show the graph with the branches.
    pub fn select_branches(&mut self) {
        self.ref_list_state.select_branches();
        self.update_local_changes_visibility();
    }

    fn as_mut_list_state(&mut self) -> &mut CommitListState<'a> {
        self.commit_list_state.as_mut().unwrap()
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

    pub fn image_clear_area(&self, area: Rect) -> Option<Rect> {
        if self.showing_local_changes {
            let [list_area, _] = self.split_areas(area);
            Some(list_area)
        } else {
            None
        }
    }

    fn split_areas(&self, area: Rect) -> [Rect; 2] {
        let graph_width = self.as_list_state().graph_area_cell_width() + 1; // graph area + marker
        let refs_width =
            (area.width.saturating_sub(graph_width)).min(self.ctx.ui_config.refs.width);
        Layout::horizontal([Constraint::Min(0), Constraint::Length(refs_width)]).areas(area)
    }

    fn update_commit_list_selected(&mut self) {
        if let Some(selected) = self.ref_list_state.selected_ref_name() {
            self.as_mut_list_state().select_ref(&selected)
        }
        self.update_local_changes_visibility();
    }

    // The local changes panel replaces the commit list while its node is selected.
    // Graph images are drawn outside of the text buffer, so they need explicit clearing when switching.
    fn update_local_changes_visibility(&mut self) {
        let show = self.ref_list_state.is_local_changes_selected();
        if show == self.showing_local_changes {
            return;
        }
        self.showing_local_changes = show;
        if show {
            self.tx.send(AppEvent::ClearViewImages);
        } else {
            self.tx.send(AppEvent::ClearScreen);
        }
    }

    fn copy_ref_name(&self) {
        if let Some(selected) = self.ref_list_state.selected_branch() {
            self.copy_to_clipboard("Branch Name".into(), selected);
        } else if let Some(selected) = self.ref_list_state.selected_tag() {
            self.copy_to_clipboard("Tag Name".into(), selected);
        }
    }

    fn copy_to_clipboard(&self, name: String, value: String) {
        self.tx.send(AppEvent::CopyToClipboard { name, value });
    }

    pub fn refresh(&self) {
        let list_state = self.as_list_state();
        let list_context = ListRefreshViewContext::from(list_state);
        let (tree_selected, tree_opened) = self.ref_list_state.current_tree_status();
        let refs_context = RefsRefreshViewContext {
            selected: tree_selected,
            opened: tree_opened,
        };
        let context = RefreshViewContext::Refs {
            list_context,
            refs_context,
        };
        self.tx.send(AppEvent::Refresh(context));
    }

    pub fn reset_refs_with(&mut self, refs_context: RefsRefreshViewContext) {
        self.ref_list_state
            .reset_tree_status(refs_context.selected, refs_context.opened);
        self.update_local_changes_visibility();
    }
}
