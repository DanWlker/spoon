use std::rc::Rc;

use ratatui::{crossterm::event::KeyEvent, layout::Rect, Frame};

use crate::{
    app::AppContext,
    event::{hint, KeyHint, Sender, UserEvent},
    git::{Commit, FileChange, Ref, Repository},
    layout::Direction,
    view::{
        detail::DetailView, list::ListView, local_changes::LocalChangesView,
        user_command::UserCommandView, RefreshViewContext,
    },
    widget::{commit_list::CommitListState, local_changes::LocalChangesState},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewKind {
    Default,
    List,
    Detail,
    // the details comparing two commits
    Compare,
    UserCommand,
    LocalChanges,
}

#[derive(Debug, Default)]
pub enum View<'a> {
    #[default]
    Default, // dummy variant to make #[default] work
    List(Box<ListView<'a>>),
    Detail(Box<DetailView<'a>>),
    UserCommand(Box<UserCommandView<'a>>),
    LocalChanges(Box<LocalChangesView<'a>>),
}

// Calls the same method on whichever page is shown, `default` without one.
macro_rules! on_page {
    ($view:expr, $page:ident => $call:expr, $default:expr) => {
        match $view {
            View::Default => $default,
            View::List($page) => $call,
            View::Detail($page) => $call,
            View::UserCommand($page) => $call,
            View::LocalChanges($page) => $call,
        }
    };
}

impl<'a> View<'a> {
    pub fn handle_event(&mut self, event: UserEvent, key_event: KeyEvent) {
        on_page!(self, view => view.handle_event(event, key_event), {})
    }

    // `focused` is false while another panel, like the refs sidebar, has the focus;
    // the page shows its selection dimmed then.
    pub fn render(&mut self, f: &mut Frame, area: Rect, focused: bool) {
        on_page!(self, view => view.render(f, area, focused), {})
    }

    pub fn update_layout(&mut self, area: Rect) {
        on_page!(self, view => view.update_layout(area), {})
    }

    pub fn prepare_graph_uploads(&mut self) {
        match self {
            View::Default => {}
            View::List(view) => view.prepare_graph_uploads(),
            View::Detail(view) => view.prepare_graph_uploads(),
            View::UserCommand(view) => view.prepare_graph_uploads(),
            View::LocalChanges(_) => {}
        }
    }

    pub fn drain_pending_graph_uploads(&mut self) -> Vec<String> {
        match self {
            View::Default => Vec::new(),
            View::List(view) => view.drain_pending_graph_uploads(),
            View::Detail(view) => view.drain_pending_graph_uploads(),
            View::UserCommand(view) => view.drain_pending_graph_uploads(),
            View::LocalChanges(_) => Vec::new(),
        }
    }

    pub fn graph_image_ids_sorted(&self) -> Vec<u32> {
        on_page!(self, view => view.graph_image_ids_sorted(), Vec::new())
    }

    // The keys of what has the focus in the view, for the status bar; not the ones moving around.
    pub fn key_hints(&self) -> Vec<KeyHint> {
        match self {
            View::List(view) => view.key_hints(),
            View::Detail(view) => view.key_hints(),
            View::LocalChanges(view) => view.key_hints(),
            View::UserCommand(_) => vec![hint(UserEvent::Cancel, "close")],
            View::Default => vec![],
        }
    }

    // Moves the focus to the panel of the page that way; false when there is none.
    pub fn move_focus(&mut self, direction: Direction) -> bool {
        match self {
            View::Detail(view) => view.move_focus(direction),
            View::LocalChanges(view) => view.move_focus(direction),
            _ => false,
        }
    }

    // What the view shows, to tell when the screen must be cleared of the graph images.
    pub fn kind(&self) -> ViewKind {
        match self {
            View::Default => ViewKind::Default,
            View::List(_) => ViewKind::List,
            View::Detail(view) if view.is_comparing() => ViewKind::Compare,
            View::Detail(_) => ViewKind::Detail,
            View::UserCommand(_) => ViewKind::UserCommand,
            View::LocalChanges(_) => ViewKind::LocalChanges,
        }
    }

    pub fn image_clear_area(&self, area: Rect) -> Option<Rect> {
        match self {
            View::Default | View::List(_) => None,
            View::Detail(view) => Some(view.image_clear_area(area)),
            View::UserCommand(view) => Some(view.image_clear_area(area)),
            View::LocalChanges(_) => Some(area),
        }
    }

    // Another panel, like the refs sidebar, takes the focus from the page.
    pub fn blur(&mut self) {
        if let View::LocalChanges(view) = self {
            view.blur();
        }
    }

    pub fn clear_picked_commits(&mut self) {
        match self {
            View::List(view) => view.clear_picks(),
            View::Detail(view) => view.clear_picks(),
            _ => {}
        }
    }

    // Takes the commit list out of a view showing it, to build the next view on it.
    pub fn take_list_state(&mut self) -> Option<CommitListState<'a>> {
        on_page!(self, view => Some(view.take_list_state()), None)
    }

    // The commit list of a view showing the commit graph.
    pub fn as_list_state(&self) -> Option<&CommitListState<'a>> {
        match self {
            View::List(view) => Some(view.as_list_state()),
            View::Detail(view) => Some(view.as_list_state()),
            View::UserCommand(view) => Some(view.as_list_state()),
            View::Default | View::LocalChanges(_) => None,
        }
    }

    // Views with numbered panels take digit keys; elsewhere they are a count prefix.
    // In the commit list, they open the details on that tab.
    pub fn has_panels(&self) -> bool {
        matches!(
            self,
            View::List(_) | View::LocalChanges(_) | View::Detail(_)
        )
    }

    // While typing text or answering a prompt, every key goes to the view as is instead of being mapped to an event.
    pub fn is_capturing_keys(&self) -> bool {
        match self {
            View::LocalChanges(view) => view.is_capturing_keys(),
            View::Detail(view) => view.is_typing_query(),
            _ => false,
        }
    }

    pub fn of_list(
        commit_list_state: CommitListState<'a>,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        View::List(Box::new(ListView::new(commit_list_state, ctx, tx)))
    }

    pub fn of_detail(
        commit_list_state: CommitListState<'a>,
        commit: Commit,
        changes: Vec<FileChange>,
        refs: Vec<Ref>,
        repository: &'a Repository,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        View::Detail(Box::new(DetailView::new(
            commit_list_state,
            commit,
            changes,
            refs,
            repository,
            ctx,
            tx,
        )))
    }

    pub fn of_user_command(
        commit_list_state: CommitListState<'a>,
        command_output: String,
        user_command_number: usize,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        View::UserCommand(Box::new(UserCommandView::new(
            commit_list_state,
            command_output,
            user_command_number,
            ctx,
            tx,
        )))
    }

    pub fn of_local_changes(
        commit_list_state: CommitListState<'a>,
        local_changes_state: LocalChangesState<'a>,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        View::LocalChanges(Box::new(LocalChangesView::new(
            commit_list_state,
            local_changes_state,
            ctx,
            tx,
        )))
    }

    pub fn refresh(&mut self) {
        on_page!(self, view => view.refresh(), {})
    }

    // What to show again once the repository is reloaded, None for views without the commit list.
    pub fn refresh_context(&self) -> Option<RefreshViewContext> {
        on_page!(self, view => Some(view.refresh_context()), None)
    }
}
