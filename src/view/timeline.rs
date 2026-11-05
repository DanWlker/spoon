use std::rc::Rc;

use ratatui::{
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::Widget,
    Frame,
};

use crate::{
    app::AppContext,
    event::{AppEvent, Sender, UserEvent},
    git::{FileRevision, Repository},
    keys::{KeyList, Keys},
    layout::{fit_size, Direction, Edge, Panel},
    search::{SearchKind, SearchOutcome},
    view::{
        apply_selection, commit_event, copy_selection, edit_files, file_link, query_status,
        register_apply_key, register_commit_keys, register_diff_keys, register_moving_keys,
        revision_matches, revision_spans, search_status, type_query, typing_keys,
    },
    widget::{
        diff::Step,
        diff_pane::DiffPane,
        style::{
            loading_note, mark_selected_row, panel_block, panel_title, render_panel_scrollbar,
            ListCursor,
        },
        tree_query::{find_around, QueryKind, SearchDirection, TreeQuery},
    },
};

// The commits changing a file, over everything, with the file's changes in the selected one beside
// them, see `UserEvent::Timeline`.
#[derive(Debug)]
pub struct TimelineView {
    // the newest first
    history: Vec<FileRevision>,
    cursor: ListCursor,
    // the focus is on the commits while it is outside the diff
    pane: DiffPane,
    // the search of the diff
    query: TreeQuery,
    ctx: Rc<AppContext>,
    tx: Sender,
}

impl TimelineView {
    // The commits changing the file, on the last one changing it up to `commit`.
    #[cfg(test)]
    pub fn new(
        repository: &Repository,
        commit: &crate::git::CommitHash,
        path: &str,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Result<Self, String> {
        let (history, selected) = repository.file_history(commit, path)?;
        let mut view = Self::of(history, selected, ctx, tx);
        view.load(repository);
        Ok(view)
    }

    // The view of the file's `history` on the revision at `selected`, loaded on another thread,
    // see `Repository::file_history_work`; its diff loads with `show_selected`.
    pub fn of(
        history: Vec<FileRevision>,
        selected: usize,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        let mut cursor = ListCursor::default();
        cursor.select(selected, history.len());
        TimelineView {
            history,
            cursor,
            pane: DiffPane::default(),
            query: TreeQuery::default(),
            ctx,
            tx,
        }
    }

    // The file's changes in the selected commit, from the path it had before when it moved.
    fn load(&mut self, repository: &Repository) {
        let raw = self.diff_work(repository)();
        self.pane.show_loaded(&raw, repository);
    }

    fn diff_work(&self, repository: &Repository) -> Box<dyn FnOnce() -> String + Send> {
        let revision = &self.history[self.cursor.selected()];
        let mut paths = vec![revision.path.as_str()];
        if let Some(older) = self.history.get(self.cursor.selected() + 1) {
            if older.path != revision.path {
                paths.push(&older.path);
            }
        }
        let context = self.ctx.diff_context.get();
        repository.commit_diff_work(&revision.commit, &paths, context)
    }

    // The diff of the commit selected now: from the diff, it stays in the diff of that one, loaded
    // right away for that; from the commits, it loads on another thread, the screen keeping up with
    // the keys meanwhile, see `poll_loading`.
    pub fn show_selected(&mut self, repository: &Repository) {
        if self.pane.is_focused() {
            self.load(repository);
            self.pane.enter();
        } else {
            let work = self.diff_work(repository);
            self.pane.load_in_background(self.tx.clone(), work);
        }
    }

    // Before drawing: the diff loaded in the background, once it is.
    pub fn poll_loading(&mut self, repository: &Repository) {
        self.pane.poll_loading(repository);
    }

    #[cfg(test)]
    pub fn is_loading(&self) -> bool {
        self.pane.loading_since().is_some()
    }

    // Selects another commit; in the diff, it stays there, on the first hunk.
    fn select(&mut self, index: usize, repository: &Repository) {
        let index = index.min(self.history.len() - 1);
        if index == self.cursor.selected() {
            return;
        }
        self.cursor.select(index, self.history.len());
        self.show_selected(repository);
    }

    // The path of the file in the selected commit, for the page bar.
    pub fn path(&self) -> &str {
        &self.history[self.cursor.selected()].path
    }

    // While typing a search, every key goes to it as is.
    pub fn is_diff_focused(&self) -> bool {
        self.pane.is_focused()
    }

    pub fn is_typing(&self) -> bool {
        self.query.is_editing()
    }

    pub fn handle_event(&mut self, event: UserEvent, key: KeyEvent, repository: &Repository) {
        if self.query.is_editing() {
            let kind = self.query_kind();
            if type_query(&self.ctx, &mut self.query, key, kind) == Some(QueryKind::Search) {
                self.search(SearchDirection::Current, repository);
            }
            self.update_query_status();
            return;
        }
        // the keys moving the selection leave its diff loading, the others may act on it
        if !event.moves_selection() {
            self.pane.wait_loading(repository);
        }
        let in_diff = self.pane.is_focused();
        match event {
            // the search, then out of the lines and hunks, then closes
            UserEvent::Cancel => {
                if self.query.clear().is_some() {
                    self.update_query_status();
                } else if !self.pane.back() {
                    self.tx.send(AppEvent::CloseOverlay);
                }
            }
            UserEvent::FocusPanel(1) => while self.pane.back() {},
            UserEvent::FocusPanel(2) | UserEvent::NavigateRight if !in_diff => {
                self.pane.enter();
            }
            UserEvent::NavigateLeft if in_diff => {
                self.pane.back();
            }
            UserEvent::OlderCommit => self.select(self.cursor.selected() + 1, repository),
            UserEvent::NewerCommit => {
                self.select(self.cursor.selected().saturating_sub(1), repository)
            }
            UserEvent::Search => {
                // from the commits, or from the diff while it has the focus
                let from = match in_diff {
                    true => self.pane.selected_row(),
                    false => self.cursor.selected(),
                };
                self.query.start(QueryKind::Search, from);
                self.update_query_status();
            }
            UserEvent::GoToNext => self.search(SearchDirection::Next, repository),
            UserEvent::GoToPrevious => self.search(SearchDirection::Previous, repository),
            // the file under its newest name, as it is in the working tree
            UserEvent::Edit => self
                .tx
                .send(edit_files(repository, &[&self.history[0].path])),
            _ if in_diff => self.handle_diff_event(event, repository),
            _ => self.handle_commits_event(event, repository),
        }
    }

    // The keys on the commits: moving, and acting on the selected one; every scroll key moves
    // the commits.
    fn handle_commits_event(&mut self, event: UserEvent, repository: &Repository) {
        let total = self.history.len();
        let before = self.cursor.selected();
        if self.cursor.navigate(event, total) {
            // the offset moves even when the selection stays, like ctrl-e/y; the diff only reloads
            // when another commit is selected
            if self.cursor.selected() != before {
                self.show_selected(repository);
            }
            return;
        }
        let commit = &self.history[self.cursor.selected()].commit;
        if let Some(event) = commit_event(event, commit) {
            self.tx.send(event);
        }
    }

    // The keys in the diff: moving the selected hunk or line, copying it and linking to it.
    fn handle_diff_event(&mut self, event: UserEvent, repository: &Repository) {
        let step = Step::of(event);
        if let Some(step) = step {
            self.pane.step(step);
            return;
        }
        match event {
            UserEvent::Confirm => {
                self.pane.enter();
            }
            UserEvent::ToggleSelect => self.pane.toggle_range(),
            UserEvent::Select => {
                if let Some(event) = apply_selection(&self.pane) {
                    self.tx.send(event);
                }
            }
            UserEvent::IncreaseDiffContext | UserEvent::DecreaseDiffContext => {
                self.adjust_diff_context(event == UserEvent::IncreaseDiffContext, repository)
            }
            UserEvent::ShortCopy => {
                if let Some(text) = self.pane.selected_text() {
                    self.tx
                        .send(copy_selection(text, self.pane.selected_name()));
                }
            }
            // the file at the commit, at the selected lines
            UserEvent::OpenInBrowser | UserEvent::CopyLink => {
                let revision = &self.history[self.cursor.selected()];
                let lines = self.pane.selected_lines();
                let link = file_link(event, &revision.commit, &revision.path, lines);
                self.tx.send(link);
            }
            _ => {
                self.pane.scroll(event);
            }
        }
    }

    // Shows more or less of the file around the changes in the selected commit.
    fn adjust_diff_context(&mut self, increase: bool, repository: &Repository) {
        let context = self.ctx.diff_context.get();
        let context = if increase {
            context.saturating_add(1)
        } else {
            context.saturating_sub(1)
        };
        if context == self.ctx.diff_context.get() {
            return;
        }
        self.ctx.diff_context.set(context);
        let selection = self.pane.selection();
        self.load(repository);
        self.pane.restore(selection);
    }

    // Searches the diff while it has the focus, else the commits, by hash, date or subject.
    fn search(&mut self, direction: SearchDirection, repository: &Repository) {
        let Some(search) = self.query.search().map(String::from) else {
            return;
        };
        let options = self.ctx.search_options.get();
        let from = self.query.step_from(if self.pane.is_focused() {
            self.pane.selected_row()
        } else {
            self.cursor.selected()
        });
        if self.pane.is_focused() {
            self.pane.search(&search, from, direction, options);
        } else {
            let history = &self.history;
            let found = find_around(history.len(), from, direction, |i| {
                revision_matches(&self.ctx, &history[i], &search, options)
            });
            if let Some(index) = found {
                self.select(index, repository);
            }
        }
        self.update_query_status();
    }

    // How the search of what has the focus went: the commits, or the rows of the diff.
    fn search_outcome(&self) -> Option<SearchOutcome> {
        let query = self.query.search()?;
        let options = self.ctx.search_options.get();
        let selected = match self.pane.is_focused() {
            true => return self.pane.search_outcome(query, options),
            false => self.cursor.selected(),
        };
        let matched = self
            .history
            .iter()
            .map(|revision| revision_matches(&self.ctx, revision, query, options))
            .collect::<Vec<_>>();
        Some(SearchOutcome::of(query, selected, matched.into_iter()))
    }

    fn update_query_status(&self) {
        let kind = self.query_kind();
        let event = match self.query.is_editing() {
            true => query_status(&self.ctx, &self.query, kind),
            false => search_status(kind, self.ctx.search_options.get(), self.search_outcome()),
        };
        self.tx.send(event);
    }

    // The text or the diff is searched as text, the commits by name.
    fn query_kind(&self) -> SearchKind {
        if self.pane.is_focused() {
            SearchKind::Text
        } else {
            SearchKind::Names
        }
    }

    // Alt and the arrow keys: right into the diff, left back to the commits.
    pub fn move_focus(&mut self, direction: Direction) {
        match direction {
            Direction::Right if !self.pane.is_focused() => {
                self.pane.enter();
            }
            Direction::Left => while self.pane.back() {},
            _ => {}
        }
    }

    // The keys of the timeline working where the focus is, the most useful first: the status bar
    // shows as many as fit, the keys popup all; see `Keys`.
    pub fn register_keys(k: &mut Keys<Self>) {
        use UserEvent::*;
        let diff = |v: &Self| v.pane.is_focused();
        let commits = |v: &Self| !v.pane.is_focused();
        // a diff without hunks, like a rename's, has nothing to go into; one loading shows the key,
        // which waits for it, so it doesn't flicker off while moving through the commits
        let enterable = move |v: &Self| {
            commits(v)
                && (v.pane.loading_since().is_some()
                    || v.pane.diff().is_some_and(|d| !d.hunks.is_empty()))
        };
        // the main action in the diff
        register_apply_key(k, |v| v.pane.is_focused().then_some(&v.pane));
        k.add(
            &[OlderCommit, NewerCommit],
            "older/newer commit",
            "Select the older/newer commit, from the diff too",
        );
        register_diff_keys(
            k,
            |v| v.pane.is_focused().then_some(&v.pane),
            |v| v.ctx.diff_context.get(),
        );
        k.add(
            &[OpenInBrowser],
            "open in browser",
            "Open the file at the selected lines on the web host, in the diff",
        )
        .when(diff);
        k.add(
            &[CopyLink],
            "copy link",
            "Copy the link of the file at the selected lines, in the diff",
        )
        .when(diff);
        k.add(&[NavigateLeft], "back", "Into the diff/back out of it")
            .when(diff);
        k.add(
            &[NavigateRight],
            "into the diff",
            "Into the diff/back out of it",
        )
        .when(enterable);
        register_commit_keys(k, commits);
        register_moving_keys(k, |_| true);
        k.quiet(
            &[SelectDown, SelectUp],
            "Select the older/newer commit, on the commits",
        )
        .when(commits);
        k.add(
            &[Search],
            "search",
            "Search the commits (hash, date, subject), or the diff",
        );
        k.add(
            &[GoToNext, GoToPrevious],
            "next/previous match",
            "Go to the next/previous match",
        );
        k.add(
            &[Edit],
            "edit",
            "Open the file, under its newest name, as it is in the working tree in $VISUAL or $EDITOR",
        );
        k.quiet(&[FocusPanel(1)], "Focus the commits").when(diff);
        k.quiet(&[FocusPanel(2)], "Focus the diff").when(enterable);
        k.add(
            &[Cancel],
            "back",
            "Clear the search, back out of the diff, then close",
        )
        .when(diff);
        k.add(
            &[Cancel],
            "close",
            "Clear the search, back out of the diff, then close",
        )
        .when(commits);
    }

    pub fn keys(&self) -> KeyList {
        if self.query.is_editing() {
            return typing_keys(self.query_kind());
        }
        let mut keys = Keys::of(self);
        Self::register_keys(&mut keys);
        keys.list()
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let ctx = self.ctx.clone();
        let theme = &ctx.color_theme;
        let buf = f.buffer_mut();
        // the commits beside the diff, resizable; both keep some room
        let width = ctx.layout.borrow().size(
            Panel::TimelineCommits,
            (u32::from(area.width) * 35 / 100) as u16,
        );
        let width = fit_size(width, area.width, 20, 20);
        let [list_area, diff_area] =
            Layout::horizontal([Constraint::Length(width), Constraint::Min(0)]).areas(area);
        ctx.layout
            .borrow_mut()
            .place(Panel::TimelineCommits, list_area, Edge::Right);

        // the commits, the selected one with the bar
        let focused = !self.pane.is_focused();
        let count = self.history.len();
        // the search in the title of the panel it applies to, like its highlights
        let indicator = |shown: bool| self.query.indicator().filter(|_| shown);
        let position = format!("{}/{count}", count - self.cursor.selected());
        let notes = [position].into_iter().chain(indicator(focused));
        let block =
            panel_block(focused, theme).title(panel_title("[1] Commits", notes, focused, theme));
        let inner = block.inner(list_area);
        block.render(list_area, buf);
        let scrolloff = ctx.ui_config.common.scrolloff as usize;
        let offset = self.cursor.fit(inner.height as usize, count, scrolloff);
        let rows = self.history.iter().enumerate().skip(offset);
        for ((i, revision), y) in rows.zip(inner.top()..inner.bottom()) {
            if i == self.cursor.selected() {
                mark_selected_row(buf, inner, y, focused, theme);
            }
            // the search of the commits highlighted, not the diff's
            let search = self.query.search().filter(|_| focused);
            let line = Line::from(revision_spans(&ctx, revision, search));
            buf.set_line(inner.x, y, &line, inner.width);
        }
        render_panel_scrollbar(buf, list_area, count, offset, focused, theme);

        // the file's changes in it
        let path = self.history[self.cursor.selected()].path.clone();
        let notes = [path].into_iter().chain(indicator(!focused));
        let notes = notes.chain(loading_note(self.pane.loading_since()));
        let title = panel_title("[2] Diff", notes, !focused, theme);
        let search = self.query.search().filter(|_| !focused);
        self.pane
            .render_panel(diff_area, title, buf, &ctx, !focused, search);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ratatui::{
        backend::TestBackend,
        crossterm::event::{KeyCode, KeyModifiers},
        Terminal,
    };

    use super::*;
    use crate::{
        event::EventController,
        git::CommitHash,
        git::SortCommit,
        test_support::{
            git::{git, GitRepository},
            ui::test_ctx,
        },
    };

    #[test]
    fn test_timeline_shows_the_changes_of_each_commit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        fs::write(path.join("a"), "one\ntwo\n").unwrap();
        repo.add(&["a"]);
        repo.commit("c1", "2024-01-01");
        fs::write(path.join("other"), "x\n").unwrap();
        repo.add(&["other"]);
        repo.commit("not a", "2024-01-02");
        git(path, &["mv", "a", "b"]);
        repo.commit("c2", "2024-01-03");
        fs::write(path.join("b"), "one\nTWO\n").unwrap();
        repo.add(&["b"]);
        repo.commit("c3", "2024-01-04");
        let head = CommitHash::from(repo.rev_parse_head().as_str());
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let ec = EventController::without_terminal();
        let mut view = TimelineView::new(&repository, &head, "b", test_ctx(), ec.sender()).unwrap();
        let key = KeyEvent::new(KeyCode::Null, KeyModifiers::NONE);
        let press = |view: &mut TimelineView, event| view.handle_event(event, key, &repository);
        let subjects: Vec<&str> = view.history.iter().map(|r| r.subject.as_str()).collect();
        // the commit not changing the file left out
        assert_eq!(subjects, ["c3", "c2", "c1"]);
        let raw = |view: &TimelineView| view.pane.diff().unwrap().raw.clone();
        assert!(raw(&view).contains("+TWO"));

        // into the diff, the hunks then the lines; older keeps it in the diff
        press(&mut view, UserEvent::NavigateRight);
        press(&mut view, UserEvent::Confirm);
        assert!(view.pane.is_selecting_lines());
        press(&mut view, UserEvent::OlderCommit);
        // the rename, from the path before it; with no hunk, back on the commits
        assert!(raw(&view).contains("rename from a"));
        assert!(!view.pane.is_focused());
        // nothing to go into there, and the commits already have the focus
        assert!(!view.keys().takes(UserEvent::NavigateRight));
        assert!(!view.keys().takes(UserEvent::FocusPanel(2)));
        assert!(!view.keys().takes(UserEvent::FocusPanel(1)));
        press(&mut view, UserEvent::NewerCommit);
        press(&mut view, UserEvent::NavigateRight);
        press(&mut view, UserEvent::OlderCommit);
        press(&mut view, UserEvent::OlderCommit);
        // from the commits, the diff loads in the background, the key into it shown meanwhile
        assert!(view.pane.loading_since().is_some());
        assert!(view.keys().takes(UserEvent::NavigateRight));
        view.pane.wait_loading(&repository);
        assert!(raw(&view).contains("+two"));
        assert!(view.keys().takes(UserEvent::FocusPanel(2)));

        // j and k move between the commits
        press(&mut view, UserEvent::NavigateUp);
        press(&mut view, UserEvent::NavigateUp);
        press(&mut view, UserEvent::NavigateUp);
        assert_eq!(view.cursor.selected(), 0);
        press(&mut view, UserEvent::GoToBottom);
        assert_eq!(view.cursor.selected(), 2);

        // the commits searched by subject
        press(&mut view, UserEvent::Search);
        for c in "c2".chars() {
            let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
            view.handle_event(UserEvent::Unknown, key, &repository);
        }
        assert_eq!(view.history[view.cursor.selected()].subject, "c2");
    }

    // Selecting another commit, its diff loading, the diff before stays shown, the title saying so.
    #[test]
    fn test_diff_stays_while_the_next_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        for (text, date) in [("first", "2024-01-01"), ("second", "2024-01-02")] {
            fs::write(path.join("a"), format!("{text}\n")).unwrap();
            repo.add(&["a"]);
            repo.commit(text, date);
        }
        let head = CommitHash::from(repo.rev_parse_head().as_str());
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let ec = EventController::without_terminal();
        let mut view = TimelineView::new(&repository, &head, "a", test_ctx(), ec.sender()).unwrap();
        // the diff panel alone, the subjects of the commits beside it naming them too
        let diff = |view: &mut TimelineView| {
            let mut terminal = Terminal::new(TestBackend::new(100, 10)).unwrap();
            let frame = terminal.draw(|f| view.render(f, f.area())).unwrap();
            let rows = frame.buffer.content.chunks(100);
            rows.map(|r| r[40..].iter().map(|c| c.symbol()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(diff(&mut view).contains("second"));

        let key = KeyEvent::new(KeyCode::Null, KeyModifiers::NONE);
        view.handle_event(UserEvent::OlderCommit, key, &repository);
        assert!(view.is_loading());
        // the diff before stays, without a note while it is quick, see `shows_loading`
        let loading = diff(&mut view);
        assert!(
            loading.contains("second") && !loading.contains("loading…"),
            "{loading}"
        );
        view.pane.wait_loading(&repository);
        let loaded = diff(&mut view);
        assert!(
            loaded.contains("first") && !loaded.contains("second") && !loaded.contains("loading…"),
            "{loaded}"
        );
    }

    #[test]
    fn test_scroll_keys_scroll_the_commits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        for i in 0..20 {
            fs::write(path.join("a"), format!("{i}\n")).unwrap();
            repo.add(&["a"]);
            repo.commit(&format!("c{i}"), "2024-01-01");
        }
        let head = CommitHash::from(repo.rev_parse_head().as_str());
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let ec = EventController::without_terminal();
        let mut view = TimelineView::new(&repository, &head, "a", test_ctx(), ec.sender()).unwrap();
        view.cursor.fit(5, view.history.len(), 0);
        view.cursor.select(5, view.history.len());
        let key = KeyEvent::new(KeyCode::Null, KeyModifiers::NONE);

        // ctrl-e and ctrl-y scroll the window alone, the selected commit staying
        let before = (view.cursor.selected(), view.cursor.offset());
        view.handle_event(UserEvent::ScrollDown, key, &repository);
        assert_eq!(view.cursor.selected(), before.0);
        assert_eq!(view.cursor.offset(), before.1 + 1);
        view.handle_event(UserEvent::ScrollUp, key, &repository);
        assert_eq!(view.cursor.offset(), before.1);

        // a page moves the selection and keeps the rows it scrolled
        view.handle_event(UserEvent::PageDown, key, &repository);
        assert!(view.cursor.selected() > before.0);
        assert!(view.cursor.offset() >= before.1);
    }
}
