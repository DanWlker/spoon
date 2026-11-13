use std::rc::Rc;

use ratatui::{crossterm::event::KeyEvent, layout::Rect, Frame};

use crate::widget::tree_query::Toward;
use crate::{
    context::AppContext,
    event::{AppEvent, Sender},
    git::{CommitHash, Head, Repository},
    graph_commands::{branches_at, commit_command_works},
    keybind::{Scope, UserEvent},
    keys::{Availability, KeyList, Keys},
    layout::Target,
    pointer::OnPicks,
    ref_commands::lone_stash,
    search::SearchKind,
    view::{
        commit_event, copy_hashes, query_status, register_commit_link_keys,
        register_graph_commands, search_status, CopyForm, ListRefreshViewContext,
        RefreshViewContext, Reselect,
    },
    widget::tree_query::{type_query, typing_keys},
    widget::{
        commit_list::{commit_list_inner_height, render_commit_list_panel, CommitListState},
        commit_tabs::DetailTab,
    },
};

#[derive(Debug)]
pub struct ListView<'a> {
    commit_list_state: CommitListState<'a>,

    repository: &'a Repository,
    ctx: Rc<AppContext>,
    tx: Sender,
}

impl<'a> ListView<'a> {
    pub fn new(
        commit_list_state: CommitListState<'a>,
        repository: &'a Repository,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> ListView<'a> {
        ListView {
            commit_list_state,
            repository,
            ctx,
            tx,
        }
    }

    pub fn handle_event(&mut self, event: UserEvent, key: KeyEvent) {
        let (ctx, tx) = (self.ctx.clone(), self.tx.clone());
        // while typing the search, keys arrive unmapped, see `App::run`
        if self.as_list_state().is_typing_search() {
            type_commit_search(self.as_mut_list_state(), &ctx, &tx, key);
            return;
        }
        // the picked commits are cleared before the search, like on every page
        let picked = !self.as_list_state().picked_commit_hashes().is_empty();
        if event == UserEvent::Cancel && picked {
            self.as_mut_list_state().clear_picks();
            return;
        }
        if handle_search_event(self.as_mut_list_state(), &ctx, &tx, event) {
            return;
        }
        match event {
            _ if self.as_mut_list_state().handle_navigation(event) => {}
            // after the picks and the search
            UserEvent::Cancel => self.tx.send(AppEvent::ShowAllCommits),
            UserEvent::NavigateLeft | UserEvent::NavigateRight => {
                if let Some(fold) = self.as_list_state().fold_event(event) {
                    self.tx.send(fold);
                }
            }
            UserEvent::ShortCopy | UserEvent::FullCopy => {
                let picked = self.as_list_state().picked_commit_hashes();
                if !picked.is_empty() {
                    self.tx.send(copy_hashes(picked, CopyForm::of(event)));
                } else {
                    let commit = self.as_list_state().selected_commit_hash();
                    if let Some(event) = commit_event(event, commit) {
                        self.tx.send(event);
                    }
                }
            }
            UserEvent::OpenInBrowser | UserEvent::CopyLink => {
                let commit = self.as_list_state().selected_commit_hash();
                if let Some(event) = commit_event(event, commit) {
                    self.tx.send(event);
                }
            }
            UserEvent::ToggleSelect => {
                self.as_mut_list_state().toggle_visual();
            }
            // on a stash, see `lone_stash`
            UserEvent::Select => {
                let commit = self.as_list_state().selected_commit_hash();
                if let Some(dialog) = lone_stash(self.repository, commit) {
                    self.tx.send(AppEvent::OpenDialog(Box::new(dialog)));
                }
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
            _ => {}
        }
    }

    // The keys of the graph page, see `Keys`.
    pub fn register_keys(k: &mut Keys<Self>) {
        register_commit_list_keys(
            k,
            |v| ListFacts::of(v.as_list_state(), v.repository),
            // a new repository has no commits yet, nothing to select
            |v, event| match v.as_list_state().is_empty() {
                true => Availability::Off,
                false => graph_availability(v.as_list_state(), v.repository, event),
            },
        );
    }

    pub fn is_visual(&self) -> bool {
        self.as_list_state().is_visual()
    }

    // Selects the commit at `row` the mouse points at, see `View::point`.
    pub fn point(&mut self, target: Target, row: Option<usize>, picks: OnPicks) -> bool {
        if target != Target::Commits {
            return false;
        }
        if let Some(row) = row {
            self.as_mut_list_state().point(row, picks);
        }
        true
    }

    pub fn keys(&self) -> KeyList {
        if self.as_list_state().is_typing_search() {
            return typing_keys(SearchKind::Commits);
        }
        let mut keys = Keys::of(self);
        Self::register_keys(&mut keys);
        keys.list()
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
    pub fn into_list_state(self) -> CommitListState<'a> {
        self.commit_list_state
    }

    fn as_mut_list_state(&mut self) -> &mut CommitListState<'a> {
        &mut self.commit_list_state
    }

    pub fn clear_picks(&mut self) {
        self.as_mut_list_state().clear_picks();
    }

    pub fn select_commit(&mut self, commit: &CommitHash) -> bool {
        self.as_mut_list_state().select_commit_hash(commit)
    }

    pub fn select_ref(&mut self, name: &str) {
        self.as_mut_list_state().select_ref(name);
    }

    pub fn as_list_state(&self) -> &CommitListState<'a> {
        &self.commit_list_state
    }

    pub fn drain_pending_graph_uploads(&mut self) -> Vec<String> {
        self.as_mut_list_state().drain_pending_graph_uploads()
    }

    pub fn refresh(&self) {
        self.tx.send(AppEvent::Reload);
    }

    // What to show again once the repository is reloaded.
    pub fn refresh_context(&self) -> RefreshViewContext {
        let list_context = ListRefreshViewContext::from(self.as_list_state());
        RefreshViewContext::List { list_context }
    }

    pub fn reset_commit_list_with(&mut self, list_context: &ListRefreshViewContext) {
        let ListRefreshViewContext {
            select,
            picked_commit_hashes,
            cursor,
            search_options,
            search_context,
        } = list_context;
        let list_state = self.as_mut_list_state();
        list_state.restore_picked_commit_hashes(picked_commit_hashes.clone());
        list_state.restore_search_options(*search_options);
        list_state.reset_height(cursor.height());
        match select {
            Reselect::Newest => list_state.select_first(),
            Reselect::Commit(hash) => {
                list_state.select_commit_hash(hash);
                // the rows the wheel scrolled to stay, else the selection keeps its row
                match cursor.row() {
                    Some(row) => list_state.restore_selected_row(row),
                    None => list_state.restore_scroll(cursor.offset()),
                }
            }
        }
        if let Some(search_context) = search_context {
            list_state.restore_search(search_context);
        }
    }
}

// What the keys of a commit list depend on.
pub struct ListFacts {
    pub picked: usize,
    pub search_applied: bool,
    // the graph shows a branch's commits only, see `UserEvent::BranchOnly`
    pub branch_only: bool,
    // the commits are being selected from where the visual selection started
    pub visual: bool,
    // a rebase, merge, cherry-pick or revert waits to be finished: the commands moving HEAD are
    // refused meanwhile, see `App::blocked_by_operation`
    pub operation_waits: bool,
    // HEAD has no commits and the graph shows all of them: showing the current branch only would
    // show nothing, git failing on the branch without commits
    pub head_history_empty: bool,
    // the selected commit is a stash, which space applies, see `lone_stash`
    pub stash: bool,
    // the branches at the selected commit, see `branches_at`
    pub commit_branches: usize,
}

// Whether the command of the graph `event` works on the picked commits, else the selected one,
// see `commit_command_works`.
pub fn list_command_works(
    state: &CommitListState,
    repository: &Repository,
    event: UserEvent,
) -> bool {
    if state.is_empty() {
        return false;
    }
    let picked = state.picked_commit_hashes();
    let targets = match picked.is_empty() {
        true => std::slice::from_ref(state.selected_commit_hash()),
        false => picked,
    };
    commit_command_works(repository, event, targets)
}

impl ListFacts {
    pub fn of(state: &CommitListState, repository: &Repository) -> Self {
        ListFacts {
            operation_waits: repository.operation_in_progress().is_some(),
            picked: state.picked_commit_hashes().len(),
            search_applied: state.search_applied(),
            branch_only: state.is_branch_only(),
            visual: state.is_visual(),
            head_history_empty: matches!(repository.head(), Head::Unborn { .. })
                && !state.is_filtered(),
            stash: !state.is_empty()
                && lone_stash(repository, state.selected_commit_hash()).is_some(),
            commit_branches: match state.is_empty() {
                true => 0,
                false => branches_at(repository, state.selected_commit_hash()).len(),
            },
        }
    }
}

// A command of the graph that would be refused is unlisted, its key saying so when pressed; the
// other keys work.
pub fn graph_availability(
    state: &CommitListState,
    repository: &Repository,
    event: UserEvent,
) -> Availability {
    let graph = event.home_scope() == Scope::Graph;
    match graph && !list_command_works(state, repository, event) {
        true => Availability::Warns,
        false => Availability::Works,
    }
}

// The keys of a commit list, on the graph page and over the details, each as `availability` says
// of its event: the commands on the selected or picked commits, picking, searching, and moving.
pub fn register_commit_list_keys<T: ?Sized>(
    k: &mut Keys<T>,
    facts: impl Fn(&T) -> ListFacts + Copy,
    availability: impl Fn(&T, UserEvent) -> Availability + Copy,
) {
    use UserEvent::*;
    let availability = move |v: &T, event| match event {
        BranchOnly if facts(v).head_history_empty => Availability::Off,
        _ => availability(v, event),
    };
    let works = move |event| move |v: &T| availability(v, event) == Availability::Works;
    k.add(&[Confirm], "details", "Show the details of the commit")
        .when(works(Confirm));
    k.add(
        &[Cancel],
        "clear picks",
        "Clear the picked commits, then the search",
    )
    .when(move |v| works(Cancel)(v) && facts(v).picked > 0);
    k.add(
        &[Cancel],
        "clear search",
        "Clear the picked commits, then the search",
    )
    .when(move |v| works(Search)(v) && facts(v).search_applied && facts(v).picked == 0);
    k.add(
        &[Cancel],
        "show all",
        "Show all the commits again after showing a branch's only",
    )
    .when(move |v| {
        let facts = facts(v);
        works(Cancel)(v) && facts.branch_only && facts.picked == 0 && !facts.search_applied
    });
    k.add_where(
        &[ToggleSelect],
        "select",
        "Select the commits from the first to the selected one",
        move |v| availability(v, ToggleSelect),
    )
    .short(move |v| {
        if facts(v).visual {
            "end select"
        } else {
            "select"
        }
        .into()
    });
    // before checking out, which space does on the other commits, see `Keys::meaning`
    k.add(&[Select], "apply", "Apply the stash")
        .when(move |v| works(Select)(v) && facts(v).stash);
    register_graph_commands(
        k,
        availability,
        move |v| facts(v).picked > 0,
        move |v| facts(v).operation_waits,
    );
    // unbound by default, in the menu of a commit; several branches there say to choose one
    k.add_where(
        &[BranchOnlyAtCommit],
        "this branch only",
        "Show only the commits of the branch at the selected commit, or all again",
        move |v| match (
            availability(v, BranchOnlyAtCommit),
            facts(v).commit_branches,
        ) {
            (Availability::Works, 0) => Availability::Off,
            (Availability::Works, 1) => Availability::Works,
            (Availability::Works, _) => Availability::Warns,
            (availability, _) => availability,
        },
    );
    // a stash isn't on the web host
    register_commit_link_keys(k, works(ShortCopy), move |v| !facts(v).stash);
    k.add(&[Search], "search", "Search the commits")
        .when(works(Search));
    k.quiet(&[GoToNext, GoToPrevious], "Go to the next/previous match")
        .when(move |v| works(GoToNext)(v) && facts(v).search_applied);
    let quiet = [
        (
            vec![SearchTargetToggle],
            "Toggle what the search of the commit list looks in",
        ),
        (
            vec![SmartCaseToggle],
            "Toggle smart case or case-sensitive, for every search",
        ),
        (
            vec![FuzzyToggle],
            "Toggle fuzzy match, for every search but of text",
        ),
        (
            vec![NavigateDown, SelectDown, NavigateUp, SelectUp],
            "Select the older/newer commit",
        ),
        (vec![GoToTop, GoToBottom], "Go to the top/bottom"),
        (
            vec![HalfPageDown, HalfPageUp],
            "Jump half a page down/up, scrolling as needed",
        ),
        (vec![PageDown, PageUp], "Scroll a page down/up"),
        (vec![ScrollDown, ScrollUp], "Scroll down/up"),
        (
            vec![NavigateLeft, NavigateRight],
            "Fold/unfold the selected merge commit",
        ),
        (
            vec![FocusPanel(3), FocusPanel(4), FocusPanel(5)],
            "Show the info/changes/files of the commit",
        ),
    ];
    for (events, long) in quiet {
        k.quiet(&events, long).when(works(events[0]));
    }
}

// A key typed into the search of the commit list, unmapped like any search (see `type_query`);
// the matches follow, and the status line shows the search, or once kept how it went.
pub fn type_commit_search(
    state: &mut CommitListState,
    ctx: &AppContext,
    tx: &Sender,
    key: KeyEvent,
) {
    // enter only keeps it
    if type_query(ctx, state.query_mut(), key, SearchKind::Commits).is_some() {
        state.search_changed();
    }
    if state.is_typing_search() {
        tx.send(query_status(ctx, state.query(), SearchKind::Commits));
    } else {
        update_matched_message(state, tx);
    }
}

// The keys of the search of the commit list while not typing it: starting it, its options, and
// going through its matches. Returns whether `event` was one.
pub fn handle_search_event(
    state: &mut CommitListState,
    ctx: &AppContext,
    tx: &Sender,
    event: UserEvent,
) -> bool {
    let applied = state.search_applied();
    match event {
        UserEvent::Search => {
            state.start_search();
            tx.send(query_status(ctx, state.query(), SearchKind::Commits));
        }
        UserEvent::SearchTargetToggle => {
            state.toggle_search_target();
            update_search_options_message(state, tx);
        }
        UserEvent::SmartCaseToggle => {
            state.toggle_smart_case();
            update_search_options_message(state, tx);
        }
        UserEvent::FuzzyToggle => {
            state.toggle_fuzzy();
            update_search_options_message(state, tx);
        }
        UserEvent::GoToNext | UserEvent::GoToPrevious if applied => {
            state.select_next_match(Toward::of(event, UserEvent::GoToNext));
            update_matched_message(state, tx);
        }
        // after the picked commits, which the callers clear first
        UserEvent::Cancel if applied => {
            state.cancel_search();
            tx.send(AppEvent::ClearStatusLine);
        }
        _ => return false,
    }
    true
}

fn update_search_options_message(state: &CommitListState, tx: &Sender) {
    if state.search_applied() {
        update_matched_message(state, tx);
    } else {
        let options = state.search_options().status_string(SearchKind::Commits);
        tx.send(AppEvent::UpdateStatusTransient(format!(
            "Search: {options}"
        )));
    }
}

fn update_matched_message(state: &CommitListState, tx: &Sender) {
    tx.send(search_status(
        SearchKind::Commits,
        state.search_options(),
        state.search_outcome(),
    ));
}
