use std::rc::Rc;

use ratatui::{
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    Frame,
};

use crate::widget::tree_query::Toward;
use crate::{
    app::AppContext,
    event::{AppEvent, Sender, UserEvent},
    git::{
        AfterTask, Commit, CommitHash, FileChange, GitCommand, GitTask, PatchDirection, Ref,
        Repository,
    },
    graph_commands::dialog_commit,
    keybind::Scope,
    keys::{Availability, KeyList, Keys},
    layout::{fit_size, Direction, Edge, Panel},
    search::SearchKind,
    view::{
        commit_event, copy_path, copy_selection, file_link, handle_search_event, not_found,
        query_status, register_commit_link_keys, register_commit_list_keys, register_diff_keys,
        type_commit_search, type_query, typing_keys, Compare, CopyForm, ListFacts,
        ListRefreshViewContext, RefreshViewContext,
    },
    widget::{
        commit_detail::CommitDetail,
        commit_list::{commit_list_inner_height, render_commit_list_panel, CommitListState},
        commit_tabs::{
            render_compare_title, render_tab_titles, CommitChangesState, CommitFilesState,
            CommitTab, DetailTab,
        },
        diff::DiffPart,
        diff::{Step, CONVERTED_DIFF},
        file_tree::{Entry, FileTreeRow, FileTreeState},
        git_dialog::GitDialog,
        style::{panel_block, ScrollState},
        tree_query::{QueryChange, QueryKind, SearchDirection, TreeQuery},
    },
};

// What has the focus in the view: the scope of the keys, where alt and the arrow keys go and the
// panels highlighted follow from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailFocus {
    // the commit list above the details, moving the selection shows the details of each commit
    List,
    // the info tab
    Info,
    // the files of the changes or files tab
    Files,
    // the diff or preview beside the files
    Pane,
}

// Two commits compared: only their changes are shown, under the comparison's title; or set aside,
// with them, while the details of a commit in it are shown.
#[derive(Debug, Default)]
enum Comparing<'a> {
    #[default]
    No,
    Shown(Compare),
    Parked(Compare, Box<CommitChangesState<'a>>),
}

#[derive(Debug)]
pub struct DetailView<'a> {
    commit_list_state: CommitListState<'a>,
    commit_detail_state: ScrollState,
    tab: DetailTab,
    changes_state: CommitChangesState<'a>,
    files_state: CommitFilesState<'a>,
    // kept while moving between commits
    changes_query: TreeQuery,
    files_query: TreeQuery,
    // searching the diff of the changes tab, or the preview of the files tab, while it has the focus
    pane_query: TreeQuery,

    commit: Commit,
    changes: Vec<FileChange>,
    refs: Vec<Ref>,
    compare: Comparing<'a>,
    // the commit list above has the focus (1), moving the selection shows the details of each commit
    list_focused: bool,
    // the selection moved to another commit, whose details load once used, see `load_selected`
    stale: bool,
    // the last key's event, the repeats of a key moving the selection loading nothing
    last_event: Option<UserEvent>,
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
            commit_list_state,
            commit_detail_state: ScrollState::default(),
            tab: DetailTab::default(),
            changes_state: CommitChangesState::new(repository, hash.clone(), &changes),
            files_state: CommitFilesState::new(repository, &commit, &changes),
            changes_query: TreeQuery::default(),
            files_query: TreeQuery::default(),
            pane_query: TreeQuery::default(),
            commit,
            changes,
            refs,
            compare: Comparing::No,
            list_focused: false,
            stale: false,
            last_event: None,
            repository,
            ctx,
            tx,
        }
    }

    // Shows the changes from `base` to `target` instead of the selected commit.
    pub fn start_compare(&mut self, base: CommitHash, target: CommitHash, compare: Compare) {
        let repository = self.repository;
        let changes = repository.diff_summary_between(&base, &target);
        self.changes_state = CommitChangesState::between(repository, Some(base), target, &changes);
        self.compare = Comparing::Shown(compare);
        self.show_tab(DetailTab::Changes);
    }

    pub fn clear_picks(&mut self) {
        self.as_mut_list_state().clear_picks();
    }

    // The keys of the details and the commit list over them, see `Keys`.
    pub fn register_keys(k: &mut Keys<Self>) {
        use DetailFocus::*;
        use UserEvent::*;
        let on = |focus: DetailFocus| move |v: &Self| v.focus() == focus;
        let files = on(Files);
        let pane = on(Pane);
        let tab = |tab: DetailTab| move |v: &Self| v.tab == tab;
        let file_selected = |v: &Self| !v.list_focused && v.selected_file().is_some();
        // a row of the files, which a commit without changes has none of
        let entry = move |v: &Self| files(v) && v.selected_row().is_some();
        // a file or directory, not the root row standing for all of them
        let path = |v: &Self| v.selected_row().and_then(FileTreeRow::entry).is_some();
        // or a filter hiding all of them, to change
        let queried = move |v: &Self| {
            files(v)
                && v.query()
                    .is_some_and(|q| q.search().is_some() || q.filter().is_some())
        };

        // the commit list: back to the diff of two commits, closing it, or the list's own keys
        k.add(
            &[Cancel],
            "back to diff",
            "Back to the diff of the two commits",
        )
        .when(|v| v.compare_parked() && v.focus() != Pane);
        k.add(&[Cancel], "close diff", "Close the diff of the two commits")
            .when(|v| v.is_comparing() && !v.compare_parked() && v.focus() != Pane);
        // the help lists them under the commit list
        if k.view().is_some() {
            register_commit_list_keys(
                k,
                |v| ListFacts::of(v.as_list_state(), v.repository),
                |v, event| match event {
                    _ if !v.list_focused => Availability::Off,
                    // no other diff until this one is closed, which clears the picks too
                    ToggleSelect | Diff | BranchOnly if v.is_comparing() => Availability::Warns,
                    Cancel if v.is_comparing() => Availability::Off,
                    _ => Availability::Works,
                },
            );
        }

        // the tabs
        k.add(
            &[Confirm],
            "select hunks",
            "Select the hunks of the diff, then their lines",
        )
        .when(|v| entry(v) && tab(DetailTab::Changes)(v));
        k.add(
            &[Confirm],
            "select lines",
            "Select the lines of the preview",
        )
        .when(|v| entry(v) && tab(DetailTab::Files)(v));
        register_diff_keys(k, |v| {
            let diff = v.focus() == Pane && v.tab == DetailTab::Changes;
            diff.then_some(&v.changes_state.pane)
        });
        k.quiet(&[Confirm], "Close the details").when(on(Info));
        k.add(
            &[ApplyChange],
            "apply to working tree",
            "Apply the selected hunk or line to the working tree",
        )
        .when(|v| pane(v) && tab(DetailTab::Changes)(v));
        k.add(
            &[ShortCopy],
            "copy line",
            "Copy the selected line of the preview",
        )
        .when(|v| pane(v) && tab(DetailTab::Files)(v));
        k.add(
            &[Cancel],
            "clear picks",
            "Clear the picked commits, then close the details",
        )
        .when(|v| {
            !v.list_focused
                && v.focus() != Pane
                && !v.is_comparing()
                && !v.as_list_state().picked_commit_hashes().is_empty()
        });
        k.add(
            &[Cancel],
            "close",
            "Clear the picked commits, then close the details",
        )
        .when(|v| {
            !v.list_focused
                && v.focus() != Pane
                && !v.is_comparing()
                && v.as_list_state().picked_commit_hashes().is_empty()
        });
        k.add(
            &[Cancel],
            "back",
            "Back from the diff or preview to the files",
        )
        .when(pane);
        k.add(&[NextTab, PreviousTab], "tab", "Show the next/previous tab")
            .when(|v| !v.list_focused && !v.shows_compare());
        register_commit_link_keys(k, on(Info));
        k.add(
            &[ResetFile],
            "reset file",
            "Reset the selected file to the commit or before it",
        )
        .when(|v| v.reset_file_change().is_some());
        k.add(
            &[Search],
            "search",
            "Search the files; the diff or preview while it has the focus",
        )
        .when(|v| entry(v) || queried(v) || pane(v));
        k.add(&[Filter], "filter", "Filter the files")
            .when(|v| entry(v) || queried(v));
        k.add(
            &[ShortCopy, FullCopy],
            "copy path",
            "Copy the relative/absolute path of the file",
        )
        .when(entry);
        k.add(
            &[Ignore],
            "ignore",
            "Add the file or directory to .gitignore",
        )
        .when(move |v| files(v) && path(v));
        k.add(
            &[OpenInBrowser],
            "open in browser",
            "Open the selected file at the commit on the web host, at the selected lines",
        )
        .when(move |v| (files(v) || pane(v)) && path(v));
        k.add(
            &[CopyLink],
            "copy link",
            "Copy the link of the selected file, at the selected lines",
        )
        .when(move |v| (files(v) || pane(v)) && path(v));
        k.add(
            &[Blame],
            "blame",
            "Blame the selected file, from the selected line",
        )
        .when(file_selected);
        k.add(
            &[Timeline],
            "timeline",
            "Show the commits changing the selected file",
        )
        .when(file_selected);
        k.quiet(&[GoToNext, GoToPrevious], "Go to the next/previous match")
            .when(|v| entry(v) || queried(v) || pane(v));
        k.quiet(
            &[NavigateDown, NavigateUp, GoToTop, GoToBottom],
            "Move or scroll down/up, to the top/bottom",
        )
        .when(|v| !v.list_focused);
        k.quiet(
            &[NavigateRight, NavigateLeft],
            "Expand/collapse the directory",
        )
        .when(entry);
        k.quiet(
            &[
                PageDown,
                PageUp,
                HalfPageDown,
                HalfPageUp,
                ScrollDown,
                ScrollUp,
            ],
            "Scroll the info, diff or preview",
        )
        .when(|v| !v.list_focused);
        k.quiet(&[SelectDown, SelectUp], "Select the older/newer commit")
            .when(|v| on(Info)(v) || files(v));
        k.quiet(
            &[FocusPanel(1), FocusPanel(2), FocusPanel(3), FocusPanel(4)],
            "Focus the commit list, the info, changes or files",
        );
    }

    pub fn keys(&self) -> KeyList {
        if self.is_typing_query() {
            return typing_keys(self.query_kind());
        }
        if self.list_focused && self.as_list_state().is_typing_search() {
            return typing_keys(SearchKind::Commits);
        }
        let mut keys = Keys::of(self);
        Self::register_keys(&mut keys);
        keys.list()
    }

    // Moves the focus to the panel that way: the commit list above the details, the diff or
    // preview beside the files. Returns false when there is none.
    pub fn move_focus(&mut self, direction: Direction) -> bool {
        use DetailFocus::*;
        match (direction, self.focus(), self.tab) {
            (Direction::Up, Info | Files | Pane, _) => self.focus_list(),
            (Direction::Down, List, _) => self.show_tab(self.tab),
            // into the diff or preview beside the files, and back
            (Direction::Right, Files, DetailTab::Changes) => {
                return self.changes_state.pane.enter()
            }
            (Direction::Right, Files, DetailTab::Files) => return self.files_state.enter(),
            (Direction::Left, Pane, DetailTab::Changes) => return self.changes_state.pane.back(),
            (Direction::Left, Pane, DetailTab::Files) => return self.files_state.back(),
            _ => return false,
        }
        true
    }

    pub fn focus(&self) -> DetailFocus {
        let pane = match self.tab {
            _ if self.list_focused => return DetailFocus::List,
            DetailTab::Info => return DetailFocus::Info,
            DetailTab::Changes => self.changes_state.pane.is_focused(),
            DetailTab::Files => self.files_state.is_preview_focused(),
        };
        if pane {
            DetailFocus::Pane
        } else {
            DetailFocus::Files
        }
    }

    pub fn focus_list(&mut self) {
        self.list_focused = true;
    }

    // The scope of the keys: the commit list, the diff or preview beside the files, or the rest
    // of the details.
    pub fn scope(&self) -> Scope {
        match self.focus() {
            DetailFocus::List => Scope::CommitList,
            DetailFocus::Pane => Scope::DetailsPane,
            DetailFocus::Info | DetailFocus::Files => Scope::Details,
        }
    }

    // Whether the files of the changes or files tab have the focus, with the diff or preview
    // beside them.
    pub fn shows_tree(&self) -> bool {
        matches!(self.focus(), DetailFocus::Files | DetailFocus::Pane)
    }

    // Whether two commits are compared, also while the details of a commit in the comparison
    // are shown over it.
    pub fn is_comparing(&self) -> bool {
        !matches!(self.compare, Comparing::No)
    }

    // The files changed in the changes tab, or in the comparison.
    #[cfg(test)]
    pub fn changed_paths(&self) -> Vec<&str> {
        self.changes_state.paths()
    }

    // Whether the comparison is shown.
    fn shows_compare(&self) -> bool {
        matches!(self.compare, Comparing::Shown(_))
    }

    // Whether the comparison is set aside for the details of a commit in it.
    fn compare_parked(&self) -> bool {
        matches!(self.compare, Comparing::Parked(..))
    }

    // Shows the details of the selected commit over the comparison, which comes back with Esc.
    fn park_compare(&mut self) {
        let compare = match std::mem::take(&mut self.compare) {
            Comparing::Shown(compare) => compare,
            other => return self.compare = other,
        };
        let changes = std::mem::replace(
            &mut self.changes_state,
            CommitChangesState::new(self.repository, self.commit.commit_hash.clone(), &[]),
        );
        self.compare = Comparing::Parked(compare, Box::new(changes));
        self.update_selected_commit(|_| {});
        self.show_tab(DetailTab::Info);
    }

    fn unpark_compare(&mut self) {
        let (compare, changes) = match std::mem::take(&mut self.compare) {
            Comparing::Parked(compare, changes) => (compare, changes),
            other => return self.compare = other,
        };
        self.compare = Comparing::Shown(compare);
        self.changes_state = *changes;
        self.tab = DetailTab::Changes;
        self.list_focused = true;
        self.load_tab();
    }

    pub fn tab(&self) -> DetailTab {
        self.tab
    }

    // Shows the commit of the ref, selected in the refs sidebar.
    pub fn select_commit(&mut self, commit: &CommitHash) -> bool {
        self.list_focused = true;
        if !self.as_mut_list_state().select_commit_hash(commit) {
            return false;
        }
        self.update_selected_commit(|_| {});
        true
    }

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

    pub fn handle_event(&mut self, event: UserEvent, key: KeyEvent) {
        // held, a key moving the selection loads the details once it is let go (or drawn)
        if self.last_event != Some(event) {
            self.load_selected();
        }
        self.last_event = Some(event);
        // typing a search or filter, keys arrive unmapped, see App::run
        if self.is_typing_query() {
            let (ctx, kind) = (self.ctx.clone(), self.query_kind());
            if let Some(query) = self.query_mut() {
                let change = type_query(&ctx, query, key, kind);
                self.apply_query_change(change);
            }
            self.update_query_status();
            return;
        }
        // searching the commit list works as without the details, which follow the selection
        if self.list_focused {
            let before = self.as_list_state().selected_commit_hash().clone();
            let (ctx, tx) = (self.ctx.clone(), self.tx.clone());
            let state = self.as_mut_list_state();
            let handled = if state.is_typing_search() {
                type_commit_search(state, &ctx, &tx, key);
                true
            } else {
                handle_search_event(state, &ctx, &tx, event)
            };
            if handled {
                if *self.as_list_state().selected_commit_hash() != before {
                    self.update_selected_commit(|_| {});
                }
                return;
            }
        }
        if let UserEvent::NextTab | UserEvent::PreviousTab = event {
            // the comparison only has changes; the focus stays where it is
            if !self.shows_compare() {
                self.tab = self.tab.cycle(Toward::of(event, UserEvent::NextTab));
                self.load_tab();
            }
            return;
        }
        if let UserEvent::FocusPanel(n) = event {
            if n == 1 {
                self.list_focused = true;
            } else if self.shows_compare() {
                // the comparison only has changes, in [2]
                if n == 2 {
                    self.list_focused = false;
                }
            } else if let Some(tab) = DetailTab::from_number(n) {
                self.show_tab(tab);
            }
            return;
        }
        // the picked commits are cleared before the search, the filter and the details; in the
        // diff or preview, Esc goes back to the files first
        if let UserEvent::Cancel = event {
            // back from the details of a commit to the comparison
            if self.list_focused && self.compare_parked() {
                self.unpark_compare();
                return;
            }
            let picked = !self.as_list_state().picked_commit_hashes().is_empty();
            if !self.is_pane_focused() && picked && !self.compare_parked() {
                self.clear_picks();
                // the diff of the picked commits goes with them
                if self.is_comparing() {
                    self.tx.send(AppEvent::CloseDetail);
                }
                return;
            }
            // a comparison ends from the commit list too, showing the whole graph again
            if self.list_focused && self.is_comparing() {
                self.tx.send(AppEvent::CloseDetail);
                return;
            }
        }
        if self.list_focused {
            if self.handle_list_event(event) {
                return;
            }
        } else if self.handle_query_event(event) {
            return;
        }

        if let (UserEvent::ResetFile, Some(change)) = (event, self.reset_file_change()) {
            let commit = dialog_commit(self.repository, &self.commit.commit_hash);
            let dialog = GitDialog::reset_file(commit, change.clone());
            self.tx.send(AppEvent::OpenDialog(Box::new(dialog)));
            return;
        }

        // the file selected in the tree: its blame, on the line selected in its diff or preview, or
        // its timeline
        if let (UserEvent::Blame | UserEvent::Timeline, false) = (event, self.list_focused) {
            let line = match self.tab {
                DetailTab::Changes => self
                    .changes_state
                    .pane
                    .selected_lines()
                    .map(|lines| *lines.start()),
                DetailTab::Files => self.files_state.selected_line(),
                DetailTab::Info => None,
            };
            if let Some(path) = self.selected_file() {
                let (commit, path) = (self.commit.commit_hash.clone(), path.into());
                self.tx.send(match event {
                    UserEvent::Blame => AppEvent::OpenBlame {
                        commit,
                        path,
                        line: line.map(|line| line.saturating_sub(1)),
                    },
                    _ => AppEvent::OpenTimeline { commit, path },
                });
            }
            return;
        }
        // the diff or preview beside the files gets only its own keys and those of the whole app
        // (see `Scope::DetailsPane`), which go on below
        if self.is_pane_focused() {
            self.handle_pane_event(event);
            return;
        }
        // in the file trees, y and Y copy the path of the selected file
        if !self.list_focused {
            if let UserEvent::ShortCopy | UserEvent::FullCopy = event {
                if let Some(row) = self.selected_row() {
                    self.tx
                        .send(copy_path(self.repository, row, CopyForm::of(event)));
                    return;
                }
            }
            // o opens the selected file at this commit on the web host, O copies its link
            if let UserEvent::OpenInBrowser | UserEvent::CopyLink = event {
                if let Some(entry) = self.selected_row().and_then(FileTreeRow::entry) {
                    let link = file_link(event, &self.commit.commit_hash, entry.path(), None);
                    self.tx.send(link);
                    return;
                }
            }
            // i adds the selected file or directory to .gitignore
            if event == UserEvent::Ignore {
                if let Some(entry) = self.selected_row().and_then(FileTreeRow::entry) {
                    let event = match self.repository.ignore_path(entry) {
                        Ok(message) => {
                            self.tx.send(AppEvent::LocalChangesChanged);
                            AppEvent::NotifySuccess(message)
                        }
                        Err(err) => AppEvent::NotifyError(err),
                    };
                    self.tx.send(event);
                    return;
                }
            }
        }

        let handled = match self.tab {
            _ if self.list_focused => false,
            DetailTab::Info => false,
            DetailTab::Changes => self.handle_changes_event(event),
            DetailTab::Files => self.handle_files_event(event),
        };
        if handled || self.commit_detail_state.handle_event(event) {
            return;
        }

        match event {
            UserEvent::SelectDown => {
                self.tx.send(AppEvent::SelectOlderCommit);
            }
            UserEvent::SelectUp => {
                self.tx.send(AppEvent::SelectNewerCommit);
            }
            UserEvent::ShortCopy
            | UserEvent::FullCopy
            | UserEvent::OpenInBrowser
            | UserEvent::CopyLink => {
                if let Some(event) = commit_event(event, &self.commit.commit_hash) {
                    self.tx.send(event);
                }
            }
            UserEvent::Confirm | UserEvent::Cancel if self.compare_parked() => {
                self.unpark_compare();
            }
            UserEvent::Confirm | UserEvent::Cancel => {
                self.tx.send(AppEvent::CloseDetail);
            }
            _ => {}
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, page_focused: bool) {
        let [list_area, detail_area] = self.split_areas(area);

        let ctx = self.ctx.clone();
        let list_focused = page_focused && self.list_focused;
        render_commit_list_panel(f, list_area, self.as_mut_list_state(), ctx, list_focused);

        // one panel for the tabs, the titles go over its top border
        let focused = page_focused && !self.list_focused;
        let block = panel_block(focused, &self.ctx.color_theme);
        let panel_area = detail_area;
        let detail_area = block.inner(panel_area);
        f.render_widget(block, panel_area);
        self.ctx
            .layout
            .borrow_mut()
            .place(Panel::Details, panel_area, Edge::Top);

        match self.tab {
            DetailTab::Info => {
                let commit_detail = CommitDetail::new(
                    &self.commit,
                    &self.changes,
                    &self.refs,
                    self.repository.head(),
                    self.repository.branch_tracking(),
                    focused,
                    self.ctx.clone(),
                );
                f.render_stateful_widget(commit_detail, detail_area, &mut self.commit_detail_state);
            }
            DetailTab::Changes => {
                let search = self.changes_query.search();
                let diff_search = self.pane_query.search();
                CommitTab::new(self.ctx.clone(), search, diff_search, focused).render_changes(
                    detail_area,
                    f.buffer_mut(),
                    &mut self.changes_state,
                );
            }
            DetailTab::Files => {
                let search = self.files_query.search();
                let preview_search = self.pane_query.search();
                CommitTab::new(self.ctx.clone(), search, preview_search, focused).render_files(
                    detail_area,
                    f.buffer_mut(),
                    &mut self.files_state,
                );
            }
        }
        let indicator = self.query().and_then(TreeQuery::indicator);
        let detail_area = panel_area;
        match &self.compare {
            // the panel 2, like the tabs
            Comparing::Shown(compare) => render_compare_title(
                &format!("[2] {}", compare.title()),
                focused,
                indicator,
                &self.ctx.color_theme,
                detail_area,
                f.buffer_mut(),
            ),
            _ => render_tab_titles(
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
        self.load_selected();
        let [list_area, _] = self.split_areas(area);
        self.as_mut_list_state()
            .update_height(commit_list_inner_height(list_area.height));
    }

    pub fn prepare_graph_uploads(&mut self) {
        self.as_mut_list_state().ensure_visible_graph_uploaded();
    }
}

impl<'a> DetailView<'a> {
    // Whether the diff of the changes tab, or the preview of the files tab, has the focus.
    fn is_pane_focused(&self) -> bool {
        self.focus() == DetailFocus::Pane
    }

    // The change of the file selected in the files of the changes tab of a commit, which `u` resets; none in a
    // comparison, having no commit to reset it before or at.
    fn reset_file_change(&self) -> Option<&FileChange> {
        // on the files, not their diff
        if self.focus() != DetailFocus::Files
            || self.tab != DetailTab::Changes
            || self.shows_compare()
        {
            return None;
        }
        let path = self.selected_file()?;
        self.changes.iter().find(|change| match change {
            FileChange::Move { to, .. } => to == path,
            FileChange::Add { path: p }
            | FileChange::Modify { path: p }
            | FileChange::Delete { path: p } => p == path,
        })
    }

    // The row selected in the file tree of the tab.
    fn selected_row(&self) -> Option<&FileTreeRow> {
        match self.tab {
            DetailTab::Info => None,
            DetailTab::Changes => self.changes_state.selected_row(),
            DetailTab::Files => self.files_state.selected_row(),
        }
    }

    // The path of the selected file, to blame, show the timeline of or reset.
    fn selected_file(&self) -> Option<&str> {
        match self.selected_row()?.entry()? {
            Entry::File(path) => Some(path),
            Entry::Dir(_) => None,
        }
    }

    // The keys of the diff or preview having the focus: moving the selection in it, scrolling,
    // copying and applying (the search is handled before); others are ignored.
    fn handle_pane_event(&mut self, event: UserEvent) {
        // o opens the file at this commit on the web host, at the selected lines; O copies the link
        if let UserEvent::OpenInBrowser | UserEvent::CopyLink = event {
            let lines = match self.tab {
                DetailTab::Changes => self.changes_state.pane.selected_lines(),
                DetailTab::Files => self.files_state.selected_line().map(|line| line..=line),
                DetailTab::Info => None,
            };
            if let Some(entry) = self.selected_row().and_then(FileTreeRow::entry) {
                let link = file_link(event, &self.commit.commit_hash, entry.path(), lines);
                self.tx.send(link);
            }
            return;
        }
        let step = Step::of(event);
        match self.tab {
            DetailTab::Changes => {
                let state = &mut self.changes_state.pane;
                if let Some(step) = step {
                    state.step(step);
                    return;
                }
                match event {
                    UserEvent::Confirm => {
                        state.enter();
                    }
                    UserEvent::Cancel => {
                        state.back();
                    }
                    UserEvent::ApplyChange => self.apply_change(),
                    UserEvent::ShortCopy => {
                        if let Some(text) = state.selected_text() {
                            self.tx.send(copy_selection(text, state.selected_part()));
                        }
                    }
                    _ => {
                        self.changes_state.pane.scroll(event);
                    }
                }
            }
            DetailTab::Files => {
                let state = &mut self.files_state;
                if let Some(step) = step {
                    state.step(step);
                    return;
                }
                match event {
                    UserEvent::Cancel => {
                        state.back();
                    }
                    UserEvent::ShortCopy => {
                        if let Some(text) = state.selected_text() {
                            self.tx.send(copy_selection(text, DiffPart::Line));
                        }
                    }
                    _ => {
                        self.handle_files_event(event);
                    }
                }
            }
            DetailTab::Info => {}
        }
    }

    // Returns whether the event was handled by the files of the changes tab.
    fn handle_changes_event(&mut self, event: UserEvent) -> bool {
        // into the diff; a directory without one opens or closes
        if event == UserEvent::Confirm && self.changes_state.pane.enter() {
            return true;
        }
        if event == UserEvent::Confirm && self.changes_state.is_converted() {
            self.tx.send(AppEvent::NotifyWarn(CONVERTED_DIFF.into()));
            return true;
        }
        if event == UserEvent::ApplyChange || self.changes_state.pane.scroll(event) {
            return true;
        }
        let state = &mut self.changes_state;
        handle_tree_event(event, |f| state.update_tree(f))
    }

    // Applies the selected hunk or line of the diff to the working tree.
    fn apply_change(&self) {
        let Some(patch) = self
            .changes_state
            .pane
            .selected_patch(PatchDirection::Forward)
        else {
            return;
        };
        let what = self.changes_state.pane.selected_part().name();
        let task = GitTask {
            commands: vec![GitCommand::Apply.args(["-"])],
            input: Some(patch),
            running: format!("Applying the {what} to the working tree"),
            done: format!("Applied the {what} to the working tree"),
            // git names the lines that differ, which tells little more
            failed: Some(format!(
                "The {what} doesn't apply to the working tree, the file differs there"
            )),
            after: AfterTask::KeepView,
            ..Default::default()
        };
        self.tx.send(AppEvent::StartTask(task));
    }

    // Moves in the commit list while it has the focus, showing the details of the selected commit;
    // returns whether the event was handled.
    fn handle_list_event(&mut self, event: UserEvent) -> bool {
        let (comparing, shows_compare) = (self.is_comparing(), self.shows_compare());
        let tx = self.tx.clone();
        let state = self.as_mut_list_state();
        let before = state.selected_commit_hash().clone();
        match event {
            _ if state.handle_navigation(event) => {}
            UserEvent::NavigateLeft | UserEvent::NavigateRight => {
                if let Some(fold) = state.fold_event(event) {
                    tx.send(fold);
                }
                return true;
            }
            UserEvent::ToggleSelect if comparing => {
                let msg = "Close the diff first (esc) to pick other commits".to_string();
                tx.send(AppEvent::NotifyWarn(msg));
                return true;
            }
            UserEvent::ToggleSelect => state.toggle_pick(),
            UserEvent::NextPicked => state.select_next_pick(Toward::Next),
            UserEvent::PreviousPicked => state.select_next_pick(Toward::Previous),
            // the details of the selected commit, over the comparison
            UserEvent::Confirm if shows_compare => {
                self.park_compare();
                return true;
            }
            UserEvent::Confirm => {
                // into the details
                self.list_focused = false;
                return true;
            }
            // the details stay open; they are closed from them
            UserEvent::Cancel => return true,
            _ => return false,
        }
        if *self.as_list_state().selected_commit_hash() != before {
            self.update_selected_commit(|_| {});
        }
        true
    }

    // Returns whether the event was handled by the files tab: scrolling the preview from the files
    // or the preview, and the files.
    fn handle_files_event(&mut self, event: UserEvent) -> bool {
        let state = &mut self.files_state;
        let preview_focused = state.is_preview_focused();
        match event {
            // into the preview of a file; a directory opens or closes
            UserEvent::Confirm if !preview_focused && state.enter() => {}
            // ctrl-d and ctrl-u too, the preview being what is read, see `HalfPage::Pane`
            _ if state.scroll_preview(event) => {}
            _ if preview_focused => return false,
            _ => {
                return handle_tree_event(event, |f| {
                    state.update_tree(f);
                })
            }
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

    // Typing a search or filter, of a tab or the commit list, which takes every key.
    pub fn is_capturing_keys(&self) -> bool {
        self.is_typing_query() || (self.list_focused && self.as_list_state().is_typing_search())
    }

    // The search and filter of the current tab, which has a file tree.
    fn query(&self) -> Option<&TreeQuery> {
        if self.is_pane_focused() {
            return Some(&self.pane_query);
        }
        match self.tab {
            DetailTab::Info => None,
            DetailTab::Changes => Some(&self.changes_query),
            DetailTab::Files => Some(&self.files_query),
        }
    }

    fn query_mut(&mut self) -> Option<&mut TreeQuery> {
        if self.is_pane_focused() {
            return Some(&mut self.pane_query);
        }
        match self.tab {
            DetailTab::Info => None,
            DetailTab::Changes => Some(&mut self.changes_query),
            DetailTab::Files => Some(&mut self.files_query),
        }
    }

    // Starts, steps through and clears the search and filter; returns whether the event was handled.
    fn handle_query_event(&mut self, event: UserEvent) -> bool {
        // the diff is searched, not filtered
        let in_diff = self.is_pane_focused();
        let Some(query) = self.query_mut() else {
            return false;
        };
        match event {
            UserEvent::Search => query.start(QueryKind::Search),
            UserEvent::Filter if in_diff => return true,
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
            UserEvent::Cancel => {
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
        let options = self.ctx.search_options.get();
        match change {
            QueryChange::None => {}
            QueryChange::Search => self.search(SearchDirection::Current),
            QueryChange::Filter => {
                let filter = self.query().and_then(TreeQuery::filter).map(String::from);
                let filter = filter.as_deref();
                match self.tab {
                    DetailTab::Info => {}
                    DetailTab::Changes => self
                        .changes_state
                        .update_tree(|t| t.set_filter(filter, options)),
                    DetailTab::Files => {
                        self.files_state
                            .update_tree(|t| t.set_filter(filter, options));
                    }
                }
            }
        }
    }

    fn search(&mut self, direction: SearchDirection) {
        let Some(search) = self.query().and_then(TreeQuery::search).map(String::from) else {
            return;
        };
        let options = self.ctx.search_options.get();
        let found = match (self.tab, self.is_pane_focused()) {
            (DetailTab::Info, _) => true,
            (DetailTab::Changes, true) => {
                self.changes_state.pane.search(&search, direction, options)
            }
            (DetailTab::Files, true) => {
                self.files_state.search_preview(&search, direction, options)
            }
            (DetailTab::Changes, false) => self
                .changes_state
                .update_tree(|t| t.search(&search, direction, options)),
            (DetailTab::Files, false) => self
                .files_state
                .update_tree(|t| t.search(&search, direction, options))
                .unwrap_or(true),
        };
        // while typing, the status line shows the query instead
        if !found && !self.is_typing_query() {
            self.tx
                .send(not_found(&self.ctx, search, self.query_kind()));
        }
    }

    // Shows the search or filter being typed in the status line.
    fn update_query_status(&self) {
        // the info tab has no search
        let event = match self.query() {
            Some(query) => query_status(&self.ctx, query, self.query_kind()),
            None => AppEvent::ClearStatusLine,
        };
        self.tx.send(event);
    }

    // The diff or preview is searched as text, the file trees by name.
    fn query_kind(&self) -> SearchKind {
        if self.is_pane_focused() {
            SearchKind::Text
        } else {
            SearchKind::Names
        }
    }

    pub fn into_list_state(self) -> CommitListState<'a> {
        self.commit_list_state
    }

    fn as_mut_list_state(&mut self) -> &mut CommitListState<'a> {
        &mut self.commit_list_state
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

    pub fn image_clear_area(&self, area: Rect) -> Rect {
        let [_, detail_area] = self.split_areas(area);
        detail_area
    }

    fn split_areas(&self, area: Rect) -> [Rect; 2] {
        let height = self
            .ctx
            .layout
            .borrow()
            .size(Panel::Details, self.ctx.ui_config.detail.height);
        // the commit list keeps a few rows
        let detail_height = fit_size(height, area.height, 3, 4);
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
        if self.shows_compare() {
            // the comparison stays, the selection only moves in the list
            return;
        }
        self.stale = true;
    }

    // Loads the details of the selected commit, when the selection moved since they were.
    pub fn load_selected(&mut self) {
        if !std::mem::take(&mut self.stale) {
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

        self.commit_detail_state.offset = 0;
        self.load_tab();
    }

    pub fn refresh(&self) {
        self.tx.send(AppEvent::Refresh(self.refresh_context()));
    }

    // What to show again once the repository is reloaded.
    pub fn refresh_context(&self) -> RefreshViewContext {
        let list_context = ListRefreshViewContext::from(self.as_list_state());
        // also while the details of a commit in it are shown over it
        let compare = match &self.compare {
            Comparing::No => None,
            Comparing::Shown(compare) | Comparing::Parked(compare, _) => Some(compare.clone()),
        };
        RefreshViewContext::Detail {
            list_context,
            tab: self.tab,
            compare,
        }
    }
}

// Moves in or expands and collapses a file tree; returns whether the event was handled.
fn handle_tree_event(
    event: UserEvent,
    mut update: impl FnMut(&dyn Fn(&mut FileTreeState)),
) -> bool {
    match event {
        UserEvent::NavigateDown => update(&FileTreeState::select_next),
        UserEvent::NavigateUp => update(&FileTreeState::select_prev),
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
