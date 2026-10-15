use std::rc::Rc;

use ratatui::{crossterm::event::KeyEvent, layout::Rect, Frame};

use crate::{
    app::AppContext,
    event::{hint, AppEvent, KeyHint, Sender, UserEvent, UserEventWithCount},
    git::{Commit, CommitHash, FileChange, Ref, Repository},
    search::SearchOptions,
    view::{
        detail::DetailView, help::HelpView, list::ListView, local_changes::LocalChangesView,
        user_command::UserCommandView,
    },
    widget::{
        commit_list::{CommitListState, SearchRefreshContext},
        file_tree::ROOT_PATH,
        local_changes::LocalChangesState,
    },
};

// The keys of the commit graph, with the commands on the selected or picked commits.
pub fn graph_hints(picked: usize) -> Vec<KeyHint> {
    let mut hints = vec![];
    if picked > 0 {
        hints.push(hint(UserEvent::Cancel, "clear picks"));
    }
    hints.extend([
        hint(UserEvent::ToggleSelect, "pick"),
        hint(UserEvent::Diff, "diff"),
        hint(UserEvent::Checkout, "checkout"),
        hint(UserEvent::Merge, "merge"),
        hint(UserEvent::Rebase, "rebase"),
        hint(UserEvent::CherryPick, "cherry-pick"),
        hint(UserEvent::Squash, "squash"),
        hint(UserEvent::AmendCommit, "amend"),
        hint(UserEvent::DropCommit, "drop"),
        hint(UserEvent::CreateTag, "tag"),
        hint(UserEvent::BranchOnly, "branch only"),
        hint(UserEvent::Revert, "revert"),
        hint(UserEvent::Reset, "reset"),
        hint(UserEvent::Search, "search"),
        (
            vec![UserEvent::ShortCopy, UserEvent::FullCopy],
            "copy hash".into(),
        ),
    ]);
    hints
}

// The keys while typing a search or filter.
pub fn typing_hints(searching_commits: bool) -> Vec<KeyHint> {
    let mut hints = vec![];
    if searching_commits {
        hints.extend([
            hint(UserEvent::SearchTargetToggle, "target"),
            hint(UserEvent::IgnoreCaseToggle, "ignore case"),
            hint(UserEvent::FuzzyToggle, "fuzzy"),
        ]);
    }
    hints.extend([
        hint(UserEvent::Confirm, "apply"),
        hint(UserEvent::Cancel, "cancel"),
    ]);
    hints
}

// Copies the path of a file in the repository: relative to its top, or absolute with `full`.
pub fn copy_path(repository: &Repository, path: &str, full: bool) -> AppEvent {
    // the root row of the local changes stands for the whole repository
    let relative = if path == ROOT_PATH { "." } else { path };
    if full {
        let absolute = repository.path().join(relative);
        let absolute = absolute.to_string_lossy();
        AppEvent::CopyToClipboard {
            name: "Absolute Path".into(),
            value: absolute.trim_end_matches("/.").to_string(),
        }
    } else {
        AppEvent::CopyToClipboard {
            name: "Path".into(),
            value: relative.to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewKind {
    Default,
    List,
    Detail,
    // the details comparing two commits
    Compare,
    UserCommand,
    LocalChanges,
    Help,
}

#[derive(Debug, Default)]
pub enum View<'a> {
    #[default]
    Default, // dummy variant to make #[default] work
    List(Box<ListView<'a>>),
    Detail(Box<DetailView<'a>>),
    UserCommand(Box<UserCommandView<'a>>),
    LocalChanges(Box<LocalChangesView<'a>>),
    Help(Box<HelpView<'a>>),
}

impl<'a> View<'a> {
    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, key_event: KeyEvent) {
        match self {
            View::Default => {}
            View::List(view) => view.handle_event(event_with_count, key_event),
            View::Detail(view) => view.handle_event(event_with_count, key_event),
            View::UserCommand(view) => view.handle_event(event_with_count, key_event),
            View::LocalChanges(view) => view.handle_event(event_with_count, key_event),
            View::Help(view) => view.handle_event(event_with_count, key_event),
        }
    }

    // `focused` is false while another panel, like the refs sidebar, has the focus;
    // the page shows its selection dimmed then.
    pub fn render(&mut self, f: &mut Frame, area: Rect, focused: bool) {
        match self {
            View::Default => {}
            View::List(view) => view.render(f, area, focused),
            View::Detail(view) => view.render(f, area, focused),
            View::UserCommand(view) => view.render(f, area),
            View::LocalChanges(view) => view.render(f, area, focused),
            View::Help(view) => view.render(f, area),
        }
    }

    pub fn update_layout(&mut self, area: Rect) {
        match self {
            View::Default => {}
            View::List(view) => view.update_layout(area),
            View::Detail(view) => view.update_layout(area),
            View::UserCommand(view) => view.update_layout(area),
            View::LocalChanges(_) | View::Help(_) => {}
        }
    }

    pub fn prepare_graph_uploads(&mut self) {
        match self {
            View::Default => {}
            View::List(view) => view.prepare_graph_uploads(),
            View::Detail(view) => view.prepare_graph_uploads(),
            View::UserCommand(view) => view.prepare_graph_uploads(),
            View::LocalChanges(_) | View::Help(_) => {}
        }
    }

    pub fn drain_pending_graph_uploads(&mut self) -> Vec<String> {
        match self {
            View::Default => Vec::new(),
            View::List(view) => view.drain_pending_graph_uploads(),
            View::Detail(view) => view.drain_pending_graph_uploads(),
            View::UserCommand(view) => view.drain_pending_graph_uploads(),
            View::LocalChanges(_) | View::Help(_) => Vec::new(),
        }
    }

    pub fn graph_image_ids_sorted(&self) -> Vec<u32> {
        match self {
            View::Default => Vec::new(),
            View::List(view) => view.graph_image_ids_sorted(),
            View::Detail(view) => view.graph_image_ids_sorted(),
            View::UserCommand(view) => view.graph_image_ids_sorted(),
            View::LocalChanges(view) => view.graph_image_ids_sorted(),
            View::Help(view) => view.graph_image_ids_sorted(),
        }
    }

    // The keys of what has the focus in the view, for the status bar; not the ones moving around.
    pub fn key_hints(&self) -> Vec<KeyHint> {
        match self {
            View::List(view) => view.key_hints(),
            View::Detail(view) => view.key_hints(),
            View::LocalChanges(view) => view.key_hints(),
            View::UserCommand(_) | View::Help(_) => vec![hint(UserEvent::Cancel, "close")],
            View::Default => vec![],
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
            View::Help(_) => ViewKind::Help,
        }
    }

    pub fn image_clear_area(&self, area: Rect) -> Option<Rect> {
        match self {
            View::Default | View::List(_) => None,
            View::Detail(view) => Some(view.image_clear_area(area)),
            View::UserCommand(view) => Some(view.image_clear_area(area)),
            View::LocalChanges(_) | View::Help(_) => Some(area),
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
        match self {
            View::List(view) => Some(view.take_list_state()),
            View::Detail(view) => Some(view.take_list_state()),
            View::UserCommand(view) => Some(view.take_list_state()),
            View::LocalChanges(view) => Some(view.take_list_state()),
            View::Default | View::Help(_) => None,
        }
    }

    // The commit list of a view showing the commit graph.
    pub fn as_list_state(&self) -> Option<&CommitListState<'a>> {
        match self {
            View::List(view) => Some(view.as_list_state()),
            View::Detail(view) => Some(view.as_list_state()),
            View::UserCommand(view) => Some(view.as_list_state()),
            View::Default | View::LocalChanges(_) | View::Help(_) => None,
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

    pub fn of_help(before: View<'a>, ctx: Rc<AppContext>, tx: Sender) -> Self {
        View::Help(Box::new(HelpView::new(before, ctx, tx)))
    }

    pub fn refresh(&mut self) {
        match self {
            View::Default => {}
            View::List(view) => view.refresh(),
            View::Detail(view) => view.refresh(),
            View::UserCommand(view) => view.refresh(),
            View::LocalChanges(view) => view.refresh(),
            View::Help(_) => {}
        }
    }

    // What to show again once the repository is reloaded, None for views without the commit list.
    pub fn refresh_context(&self) -> Option<RefreshViewContext> {
        match self {
            View::List(view) => Some(view.refresh_context()),
            View::Detail(view) => Some(view.refresh_context()),
            View::UserCommand(view) => Some(view.refresh_context()),
            View::LocalChanges(view) => Some(view.refresh_context()),
            View::Default | View::Help(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum RefreshViewContext {
    List {
        list_context: ListRefreshViewContext,
    },
    Detail {
        list_context: ListRefreshViewContext,
    },
    UserCommand {
        list_context: ListRefreshViewContext,
        user_command_context: UserCommandRefreshViewContext,
    },
    LocalChanges {
        list_context: ListRefreshViewContext,
        local_changes_context: LocalChangesRefreshViewContext,
    },
}

impl RefreshViewContext {
    pub fn list_context_mut(&mut self) -> &mut ListRefreshViewContext {
        match self {
            RefreshViewContext::List { list_context }
            | RefreshViewContext::Detail { list_context }
            | RefreshViewContext::UserCommand { list_context, .. }
            | RefreshViewContext::LocalChanges { list_context, .. } => list_context,
        }
    }

    pub fn list_context(&self) -> &ListRefreshViewContext {
        match self {
            RefreshViewContext::List { list_context }
            | RefreshViewContext::Detail { list_context }
            | RefreshViewContext::UserCommand { list_context, .. }
            | RefreshViewContext::LocalChanges { list_context, .. } => list_context,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ListRefreshViewContext {
    pub commit_hash: String,
    pub picked_commit_hashes: Vec<CommitHash>,
    pub selected: usize,
    pub height: usize,
    pub scroll_to_top: bool,
    pub search_options: SearchOptions,
    pub search_context: Option<SearchRefreshContext>,
}

impl From<&CommitListState<'_>> for ListRefreshViewContext {
    fn from(list_state: &CommitListState) -> Self {
        let commit_hash = list_state.selected_commit_hash().as_str().into();
        let (selected, offset, height) = list_state.current_list_status();
        // If the selected commit is the top one and there is no offset, it means the list is already scrolled to the top.
        // In this case, we set scroll_to_top to true to indicate that the view should be scrolled to the top after refresh.
        let scroll_to_top = selected == 0 && offset == 0;
        let search_options = list_state.search_options();
        let search_context = list_state.search_refresh_context();
        ListRefreshViewContext {
            commit_hash,
            picked_commit_hashes: list_state.picked_commit_hashes().to_vec(),
            selected,
            height,
            scroll_to_top,
            search_options,
            search_context,
        }
    }
}

#[derive(Debug, Clone)]
pub struct UserCommandRefreshViewContext {
    pub n: usize,
}

pub use crate::widget::local_changes::LocalChangesSnapshot as LocalChangesRefreshViewContext;
