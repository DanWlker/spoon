use std::rc::Rc;

use ratatui::{crossterm::event::KeyEvent, layout::Rect, Frame};

use crate::widget::tree_query::Toward;
use crate::{
    app::AppContext,
    event::{AppEvent, Sender, UserEvent},
    git::CommitHash,
    keys::{Availability, KeyList, Keys},
    search::SearchKind,
    view::{
        commit_event, query_status, register_graph_commands, type_query, typing_keys,
        ListRefreshViewContext, RefreshViewContext, Reselect,
    },
    widget::{
        commit_list::{commit_list_inner_height, render_commit_list_panel, CommitListState},
        commit_tabs::DetailTab,
        tree_query::QueryChange,
    },
};

#[derive(Debug)]
pub struct ListView<'a> {
    commit_list_state: CommitListState<'a>,

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
            commit_list_state,
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
            UserEvent::NavigateLeft | UserEvent::NavigateRight => {
                if let Some(fold) = self.as_list_state().fold_event(event) {
                    self.tx.send(fold);
                }
            }
            UserEvent::ShortCopy
            | UserEvent::FullCopy
            | UserEvent::OpenInBrowser
            | UserEvent::CopyLink => {
                let commit = self.as_list_state().selected_commit_hash();
                if let Some(event) = commit_event(event, commit) {
                    self.tx.send(event);
                }
            }
            UserEvent::ToggleSelect => {
                self.as_mut_list_state().toggle_pick();
            }
            UserEvent::NextPicked => {
                self.as_mut_list_state().select_next_pick(Toward::Next);
            }
            UserEvent::PreviousPicked => {
                self.as_mut_list_state().select_next_pick(Toward::Previous);
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
            |v| ListFacts::of(v.as_list_state(), &v.ctx),
            |_, _| Availability::Works,
        );
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

    pub fn graph_image_ids_sorted(&self) -> Vec<u32> {
        self.as_list_state().graph_image_ids_sorted()
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
                list_state.restore_selected_row(cursor.row());
            }
        }
        if let Some(search_context) = search_context {
            list_state.restore_search(search_context);
        }
    }
}

// Searching the commit list, from the list or from the details beside it: typing the query, its
// options, going through the matches and clearing it. Returns whether the event was one of these.
// What the keys of a commit list depend on.
pub struct ListFacts {
    pub picked: usize,
    pub search_applied: bool,
    // see `AppContext::operation`
    pub operation_waits: bool,
}

impl ListFacts {
    pub fn of(state: &CommitListState, ctx: &AppContext) -> Self {
        ListFacts {
            operation_waits: ctx.operation.get().is_some(),
            picked: state.picked_commit_hashes().len(),
            search_applied: state.search_applied(),
        }
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
    k.add_where(
        &[ToggleSelect],
        "pick",
        "Pick/unpick the commit",
        move |v| availability(v, ToggleSelect),
    );
    k.quiet(
        &[NextPicked, PreviousPicked],
        "Go to the next/previous picked commit",
    )
    .when(works(NextPicked));
    register_graph_commands(
        k,
        availability,
        move |v| facts(v).picked > 0,
        move |v| facts(v).operation_waits,
    );
    k.add(
        &[OpenInBrowser],
        "open in browser",
        "Open the commit on the web host",
    )
    .when(works(OpenInBrowser));
    k.add(&[CopyLink], "copy link", "Copy the link of the commit")
        .when(works(CopyLink));
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
            vec![FocusPanel(2), FocusPanel(3), FocusPanel(4)],
            "Show the info/changes/files of the commit",
        ),
    ];
    k.add(
        &[ShortCopy, FullCopy],
        "copy hash",
        "Copy the commit short/full hash",
    )
    .when(works(ShortCopy));
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
    if type_query(ctx, state.query_mut(), key, SearchKind::Commits) != QueryChange::None {
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
        // an applied search is cleared before the picked commits
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
    if let Some(outcome) = state.search_outcome() {
        tx.send(AppEvent::UpdateSearchResult {
            outcome,
            options: state.search_options().status_string(SearchKind::Commits),
        });
    } else {
        tx.send(AppEvent::ClearStatusLine);
    }
}
