use std::rc::Rc;

use ratatui::{
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    Frame,
};

use crate::{
    app::AppContext,
    event::{hint, AppEvent, KeyHint, Sender, UserEvent, UserEventWithCount},
    git::{Commit, CommitHash, FileChange, Ref, Repository},
    view::{copy_path, graph_hints, typing_hints, ListRefreshViewContext, RefreshViewContext},
    widget::{
        commit_detail::{CommitDetail, CommitDetailState},
        commit_list::{commit_list_inner_height, render_commit_list_panel, CommitListState},
        commit_tabs::{
            render_compare_title, render_tab_titles, CommitChanges, CommitChangesState,
            CommitFiles, CommitFilesState, DetailTab,
        },
        file_tree::{FileTreeState, SearchDirection},
        tree_query::{QueryChange, QueryKind, TreeQuery},
    },
};

#[derive(Debug)]
pub struct DetailView<'a> {
    commit_list_state: Option<CommitListState<'a>>,
    commit_detail_state: CommitDetailState,
    tab: DetailTab,
    changes_state: CommitChangesState<'a>,
    files_state: CommitFilesState<'a>,
    // kept while moving between commits
    changes_query: TreeQuery,
    files_query: TreeQuery,

    commit: Commit,
    changes: Vec<FileChange>,
    refs: Vec<Ref>,
    // comparing two commits: only their changes are shown, under this title
    compare: Option<String>,
    // the commit list above has the focus (1), moving the selection shows the details of each commit
    list_focused: bool,
    repository: &'a Repository,

    ctx: Rc<AppContext>,
    tx: Sender,
}

impl<'a> DetailView<'a> {
    pub fn new(
        commit_list_state: CommitListState<'a>,
        commit: Commit,
        changes: Vec<FileChange>,
        refs: Vec<Ref>,
        repository: &'a Repository,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> DetailView<'a> {
        let hash = commit.commit_hash.clone();
        DetailView {
            commit_list_state: Some(commit_list_state),
            commit_detail_state: CommitDetailState::default(),
            tab: DetailTab::default(),
            changes_state: CommitChangesState::new(repository, hash.clone(), &changes),
            files_state: CommitFilesState::new(repository, &commit, &changes),
            changes_query: TreeQuery::default(),
            files_query: TreeQuery::default(),
            commit,
            changes,
            refs,
            compare: None,
            list_focused: false,
            repository,
            ctx,
            tx,
        }
    }

    // Shows the changes from `base` to `target` instead of the selected commit.
    pub fn start_compare(&mut self, base: CommitHash, target: CommitHash, title: String) {
        let repository = self.repository;
        let changes = repository.diff_summary_between(&base, &target);
        self.changes_state = CommitChangesState::between(repository, Some(base), target, &changes);
        self.compare = Some(title);
        self.show_tab(DetailTab::Changes);
    }

    pub fn clear_picks(&mut self) {
        self.as_mut_list_state().clear_picks();
    }

    pub fn is_list_focused(&self) -> bool {
        self.list_focused
    }

    pub fn key_hints(&self) -> Vec<KeyHint> {
        if self.is_typing_query() {
            return typing_hints(false);
        }
        let picked = self.as_list_state().picked_commit_hashes().len();
        if self.list_focused {
            let mut hints = graph_hints(picked);
            if picked == 0 {
                hints.insert(0, hint(UserEvent::Cancel, "close"));
            }
            return hints;
        }
        let mut hints = vec![];
        if picked > 0 {
            hints.push(hint(UserEvent::Cancel, "clear picks"));
        } else {
            hints.push(hint(UserEvent::Cancel, "close"));
        }
        if self.compare.is_none() {
            hints.push((
                vec![UserEvent::NextTab, UserEvent::PreviousTab],
                "tab".into(),
            ));
        }
        let copy = vec![UserEvent::ShortCopy, UserEvent::FullCopy];
        match self.tab {
            DetailTab::Info => hints.push((copy, "copy hash".into())),
            DetailTab::Changes | DetailTab::Files => hints.extend([
                hint(UserEvent::Search, "search"),
                hint(UserEvent::Filter, "filter"),
                (copy, "copy path".into()),
            ]),
        }
        hints
    }

    pub fn is_comparing(&self) -> bool {
        self.compare.is_some()
    }

    pub fn tab(&self) -> DetailTab {
        self.tab
    }

    // Shows the commit of the ref, selected in the refs sidebar.
    pub fn select_ref(&mut self, name: &str) {
        let before = self.as_list_state().selected_commit_hash().clone();
        self.as_mut_list_state().select_ref(name);
        if *self.as_list_state().selected_commit_hash() != before {
            self.update_selected_commit(|_| {});
        }
    }

    pub fn show_tab(&mut self, tab: DetailTab) {
        self.list_focused = false;
        self.tab = tab;
        self.load_tab();
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, key: KeyEvent) {
        let event = event_with_count.event;
        let count = event_with_count.count;

        // typing a search or filter, keys arrive unmapped, see App::run
        if self.is_typing_query() {
            if let Some(query) = self.query_mut() {
                let change = query.handle_key(key);
                self.apply_query_change(change);
            }
            self.update_query_status();
            return;
        }
        if let UserEvent::NextTab | UserEvent::PreviousTab = event {
            // the comparison only has changes; the focus stays where it is
            if !self.is_comparing() {
                self.tab = self.tab.cycle(event == UserEvent::NextTab);
                self.load_tab();
            }
            return;
        }
        if let UserEvent::FocusPanel(n) = event {
            if n == 1 {
                self.list_focused = true;
            } else if self.is_comparing() {
                // the comparison only has changes
                self.list_focused = false;
            } else if let Some(tab) = DetailTab::from_number(n) {
                self.show_tab(tab);
            }
            return;
        }
        // the picked commits are cleared before the search, the filter and the details
        if let UserEvent::Cancel | UserEvent::Close = event {
            if !self.as_list_state().picked_commit_hashes().is_empty() {
                self.clear_picks();
                return;
            }
        }
        if self.list_focused {
            if self.handle_list_event(event, count) {
                return;
            }
        } else if self.handle_query_event(event) {
            return;
        }

        // in the file trees, y and Y copy the path of the selected file
        if !self.list_focused {
            if let UserEvent::ShortCopy | UserEvent::FullCopy = event {
                let path = match self.tab {
                    DetailTab::Info => None,
                    DetailTab::Changes => self.changes_state.selected_path(),
                    DetailTab::Files => self.files_state.selected_path(),
                };
                if let Some(path) = path.map(String::from) {
                    let full = event == UserEvent::FullCopy;
                    self.tx.send(copy_path(self.repository, &path, full));
                    return;
                }
            }
        }

        let handled = match self.tab {
            _ if self.list_focused => false,
            DetailTab::Info => false,
            DetailTab::Changes => self.handle_changes_event(event, count),
            DetailTab::Files => self.handle_files_event(event, count),
        };
        if handled {
            return;
        }

        match event {
            UserEvent::NavigateDown => {
                for _ in 0..count {
                    self.commit_detail_state.scroll_down();
                }
            }
            UserEvent::NavigateUp => {
                for _ in 0..count {
                    self.commit_detail_state.scroll_up();
                }
            }
            UserEvent::PageDown => {
                for _ in 0..count {
                    self.commit_detail_state.scroll_page_down();
                }
            }
            UserEvent::PageUp => {
                for _ in 0..count {
                    self.commit_detail_state.scroll_page_up();
                }
            }
            UserEvent::HalfPageDown => {
                for _ in 0..count {
                    self.commit_detail_state.scroll_half_page_down();
                }
            }
            UserEvent::HalfPageUp => {
                for _ in 0..count {
                    self.commit_detail_state.scroll_half_page_up();
                }
            }
            UserEvent::GoToTop => {
                self.commit_detail_state.select_first();
            }
            UserEvent::GoToBottom => {
                self.commit_detail_state.select_last();
            }
            UserEvent::SelectDown => {
                self.tx.send(AppEvent::SelectOlderCommit);
            }
            UserEvent::SelectUp => {
                self.tx.send(AppEvent::SelectNewerCommit);
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
            UserEvent::Confirm | UserEvent::Cancel | UserEvent::Close => {
                self.tx.send(AppEvent::CloseDetail);
            }
            UserEvent::Refresh => {
                self.refresh();
            }
            _ => {}
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, page_focused: bool) {
        let [list_area, detail_area] = self.split_areas(area);

        let ctx = self.ctx.clone();
        let list_focused = page_focused && self.list_focused;
        render_commit_list_panel(f, list_area, self.as_mut_list_state(), ctx, list_focused);

        match self.tab {
            DetailTab::Info => {
                let commit_detail =
                    CommitDetail::new(&self.commit, &self.changes, &self.refs, self.ctx.clone());
                f.render_stateful_widget(commit_detail, detail_area, &mut self.commit_detail_state);
            }
            DetailTab::Changes => {
                let search = self.changes_query.search();
                let commit_changes = CommitChanges::new(self.ctx.clone(), search);
                f.render_stateful_widget(commit_changes, detail_area, &mut self.changes_state);
            }
            DetailTab::Files => {
                let search = self.files_query.search();
                let commit_files = CommitFiles::new(self.ctx.clone(), search);
                f.render_stateful_widget(commit_files, detail_area, &mut self.files_state);
            }
        }
        // each tab draws a top border, the titles go over it
        let indicator = self.query().and_then(TreeQuery::indicator);
        let focused = page_focused && !self.list_focused;
        match &self.compare {
            Some(title) => render_compare_title(
                title,
                focused,
                indicator,
                &self.ctx.color_theme,
                detail_area,
                f.buffer_mut(),
            ),
            None => render_tab_titles(
                self.tab,
                focused,
                indicator,
                &self.ctx.color_theme,
                detail_area,
                f.buffer_mut(),
            ),
        }
    }

    pub fn update_layout(&mut self, area: Rect) {
        let [list_area, _] = self.split_areas(area);
        self.as_mut_list_state()
            .update_height(commit_list_inner_height(list_area.height));
    }

    pub fn prepare_graph_uploads(&mut self) {
        self.as_mut_list_state().ensure_visible_graph_uploaded();
    }
}

impl<'a> DetailView<'a> {
    // Returns whether the event was handled by the changes tab.
    fn handle_changes_event(&mut self, event: UserEvent, count: usize) -> bool {
        let state = &mut self.changes_state;
        let page = state.diff_page_height();
        match event {
            UserEvent::ScrollDown => state.scroll_diff_down(count),
            UserEvent::ScrollUp => state.scroll_diff_up(count),
            UserEvent::PageDown => state.scroll_diff_down(page * count),
            UserEvent::PageUp => state.scroll_diff_up(page * count),
            UserEvent::HalfPageDown => state.scroll_diff_down(page / 2 * count),
            UserEvent::HalfPageUp => state.scroll_diff_up(page / 2 * count),
            _ => return handle_tree_event(event, count, |f| state.update_tree(f)),
        }
        true
    }

    // Moves in the commit list while it has the focus, showing the details of the selected commit;
    // returns whether the event was handled.
    fn handle_list_event(&mut self, event: UserEvent, count: usize) -> bool {
        let state = self.as_mut_list_state();
        let before = state.selected_commit_hash().clone();
        let repeat = |state: &mut CommitListState<'a>, f: fn(&mut CommitListState<'a>)| {
            (0..count).for_each(|_| f(state))
        };
        match event {
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                repeat(state, CommitListState::select_next)
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                repeat(state, CommitListState::select_prev)
            }
            UserEvent::GoToTop => state.select_first(),
            UserEvent::GoToBottom => state.select_last(),
            UserEvent::PageDown => repeat(state, CommitListState::scroll_down_page),
            UserEvent::PageUp => repeat(state, CommitListState::scroll_up_page),
            UserEvent::HalfPageDown => repeat(state, CommitListState::scroll_down_half),
            UserEvent::HalfPageUp => repeat(state, CommitListState::scroll_up_half),
            UserEvent::ScrollDown => repeat(state, CommitListState::scroll_down),
            UserEvent::ScrollUp => repeat(state, CommitListState::scroll_up),
            UserEvent::ToggleSelect => state.toggle_pick(),
            UserEvent::NextPicked => state.select_next_pick(true),
            UserEvent::PreviousPicked => state.select_next_pick(false),
            UserEvent::Confirm => {
                // into the details
                self.list_focused = false;
                return true;
            }
            _ => return false,
        }
        if *self.as_list_state().selected_commit_hash() != before {
            self.update_selected_commit(|_| {});
        }
        true
    }

    // Returns whether the event was handled by the files tab.
    fn handle_files_event(&mut self, event: UserEvent, count: usize) -> bool {
        let state = &mut self.files_state;
        let page = state.preview_page_height();
        match event {
            UserEvent::ScrollDown => state.scroll_preview_down(count),
            UserEvent::ScrollUp => state.scroll_preview_up(count),
            UserEvent::PageDown => state.scroll_preview_down(page * count),
            UserEvent::PageUp => state.scroll_preview_up(page * count),
            UserEvent::HalfPageDown => state.scroll_preview_down(page / 2 * count),
            UserEvent::HalfPageUp => state.scroll_preview_up(page / 2 * count),
            _ => return handle_tree_event(event, count, |f| state.update_tree(f)),
        }
        true
    }

    // The changes and files of a commit are only loaded once their tab is shown.
    fn load_tab(&mut self) {
        match self.tab {
            DetailTab::Info => {}
            DetailTab::Changes => self.changes_state.ensure_loaded(),
            DetailTab::Files => self.files_state.ensure_loaded(),
        }
        // the states of another commit start without the filter
        self.apply_query_change(QueryChange::Filter);
    }

    pub fn is_typing_query(&self) -> bool {
        self.query().is_some_and(TreeQuery::is_editing)
    }

    // The search and filter of the current tab, which has a file tree.
    fn query(&self) -> Option<&TreeQuery> {
        match self.tab {
            DetailTab::Info => None,
            DetailTab::Changes => Some(&self.changes_query),
            DetailTab::Files => Some(&self.files_query),
        }
    }

    fn query_mut(&mut self) -> Option<&mut TreeQuery> {
        match self.tab {
            DetailTab::Info => None,
            DetailTab::Changes => Some(&mut self.changes_query),
            DetailTab::Files => Some(&mut self.files_query),
        }
    }

    // Starts, steps through and clears the search and filter; returns whether the event was handled.
    fn handle_query_event(&mut self, event: UserEvent) -> bool {
        let Some(query) = self.query_mut() else {
            return false;
        };
        match event {
            UserEvent::Search => query.start(QueryKind::Search),
            UserEvent::Filter => query.start(QueryKind::Filter),
            UserEvent::GoToNext | UserEvent::GoToPrevious => {
                let direction = if event == UserEvent::GoToNext {
                    SearchDirection::Next
                } else {
                    SearchDirection::Previous
                };
                self.search(direction);
                return true;
            }
            UserEvent::Cancel | UserEvent::Close => {
                // clear the search, then the filter, before closing
                let change = query.clear();
                if change == QueryChange::None {
                    return false;
                }
                self.apply_query_change(change);
                return true;
            }
            _ => return false,
        }
        self.update_query_status();
        true
    }

    fn apply_query_change(&mut self, change: QueryChange) {
        match change {
            QueryChange::None => {}
            QueryChange::Search => self.search(SearchDirection::Current),
            QueryChange::Filter => {
                let filter = self.query().and_then(TreeQuery::filter).map(String::from);
                let filter = filter.as_deref();
                match self.tab {
                    DetailTab::Info => {}
                    DetailTab::Changes => self.changes_state.update_tree(|t| t.set_filter(filter)),
                    DetailTab::Files => self.files_state.update_tree(|t| t.set_filter(filter)),
                }
            }
        }
    }

    fn search(&mut self, direction: SearchDirection) {
        let Some(search) = self.query().and_then(TreeQuery::search).map(String::from) else {
            return;
        };
        match self.tab {
            DetailTab::Info => {}
            DetailTab::Changes => self
                .changes_state
                .update_tree(|t| t.search(&search, direction)),
            DetailTab::Files => self
                .files_state
                .update_tree(|t| t.search(&search, direction)),
        }
    }

    // Shows the search or filter being typed in the status line.
    fn update_query_status(&self) {
        match self.query().and_then(TreeQuery::status_input) {
            Some((message, cursor_position)) => self.tx.send(AppEvent::UpdateStatusInput {
                message,
                cursor_position,
                metadata: String::new(),
            }),
            None => self.tx.send(AppEvent::ClearStatusLine),
        }
    }

    pub fn take_list_state(&mut self) -> CommitListState<'a> {
        self.commit_list_state.take().unwrap()
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

    pub fn image_clear_area(&self, area: Rect) -> Rect {
        let [_, detail_area] = self.split_areas(area);
        detail_area
    }

    fn split_areas(&self, area: Rect) -> [Rect; 2] {
        let detail_height = (area.height - 1).min(self.ctx.ui_config.detail.height);
        Layout::vertical([Constraint::Min(0), Constraint::Length(detail_height)]).areas(area)
    }

    pub fn select_older_commit(&mut self) {
        self.update_selected_commit(|state| state.select_next());
    }

    pub fn select_newer_commit(&mut self) {
        self.update_selected_commit(|state| state.select_prev());
    }

    fn update_selected_commit<F>(&mut self, update_commit_list_state: F)
    where
        F: FnOnce(&mut CommitListState<'a>),
    {
        let commit_list_state = self.as_mut_list_state();
        update_commit_list_state(commit_list_state);
        if self.is_comparing() {
            // the comparison stays, the selection only moves in the list
            return;
        }
        let repository = self.repository;
        let selected = self.as_list_state().selected_commit_hash().clone();
        let (commit, changes) = repository.commit_detail(&selected);
        let refs = repository.refs(&selected).into_iter().cloned().collect();
        self.changes_state = CommitChangesState::new(repository, selected.clone(), &changes);
        self.files_state.set_commit(&commit, &changes);
        self.commit = commit;
        self.changes = changes;
        self.refs = refs;

        self.commit_detail_state.select_first();
        self.load_tab();
    }

    fn copy_commit_short_hash(&self) {
        let selected = &self.commit.commit_hash;
        self.copy_to_clipboard("Commit SHA (short)".into(), selected.as_short_hash().into());
    }

    fn copy_commit_hash(&self) {
        let selected = &self.commit.commit_hash;
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
        // the comparison is not kept, it is opened again with the picks
        if self.is_comparing() {
            RefreshViewContext::List { list_context }
        } else {
            RefreshViewContext::Detail { list_context }
        }
    }
}

// Moves in or expands and collapses a file tree; returns whether the event was handled.
fn handle_tree_event(
    event: UserEvent,
    count: usize,
    mut update: impl FnMut(&dyn Fn(&mut FileTreeState)),
) -> bool {
    match event {
        UserEvent::NavigateDown => (0..count).for_each(|_| update(&FileTreeState::select_next)),
        UserEvent::NavigateUp => (0..count).for_each(|_| update(&FileTreeState::select_prev)),
        UserEvent::GoToTop => update(&FileTreeState::select_first),
        UserEvent::GoToBottom => update(&FileTreeState::select_last),
        UserEvent::NavigateRight => update(&FileTreeState::expand),
        UserEvent::NavigateLeft => update(&FileTreeState::collapse),
        // enter expands and collapses directories instead of closing the details
        UserEvent::Confirm => update(&FileTreeState::toggle),
        _ => return false,
    }
    true
}
