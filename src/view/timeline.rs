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
    git::{CommitHash, FileRevision, Repository},
    keys::{KeyList, Keys},
    layout::{fit_size, Direction, Edge, Panel},
    search::SearchKind,
    view::{
        commit_event, copy_selection, file_link, query_status, register_commit_keys,
        register_moving_keys, revision_matches, revision_spans, type_query, typing_keys,
    },
    widget::{
        diff::{parse_diff, Step},
        diff_pane::DiffPane,
        style::{
            half_page_jump, mark_selected_row, panel_block, panel_title, render_panel_scrollbar,
            scroll_to,
        },
        tree_query::{find_around, QueryChange, QueryKind, SearchDirection, TreeQuery},
    },
};

// The commits changing a file, over everything, with the file's changes in the selected one beside
// them, see `UserEvent::Timeline`.
#[derive(Debug)]
pub struct TimelineView {
    // the newest first
    history: Vec<FileRevision>,
    selected: usize,
    // the first commit shown, and how many fit, as last drawn
    offset: usize,
    height: usize,
    // the focus is on the commits while it is outside the diff
    pane: DiffPane,
    // the search of the diff
    query: TreeQuery,
    ctx: Rc<AppContext>,
    tx: Sender,
}

impl TimelineView {
    // The commits changing the file, on the last one changing it up to `commit`.
    pub fn new(
        repository: &Repository,
        commit: &CommitHash,
        path: &str,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Result<Self, String> {
        let (history, selected) = repository.file_history(commit, path)?;
        let mut view = TimelineView {
            history,
            selected,
            offset: 0,
            height: 0,
            pane: DiffPane::default(),
            query: TreeQuery::default(),
            ctx,
            tx,
        };
        view.load(repository);
        Ok(view)
    }

    // The file's changes in the selected commit, from the path it had before when it moved.
    fn load(&mut self, repository: &Repository) {
        let revision = &self.history[self.selected];
        let mut paths = vec![revision.path.as_str()];
        if let Some(older) = self.history.get(self.selected + 1) {
            if older.path != revision.path {
                paths.push(&older.path);
            }
        }
        let mut diff = parse_diff(&repository.commit_diff(&revision.commit, &paths));
        diff.drop_converted(repository);
        self.pane.set_diff(Some(diff));
    }

    // Selects another commit; in the diff, it stays there, on the first hunk.
    fn select(&mut self, index: usize, repository: &Repository) {
        let index = index.min(self.history.len() - 1);
        if index == self.selected {
            return;
        }
        let in_diff = self.pane.is_focused();
        self.selected = index;
        self.load(repository);
        if in_diff {
            self.pane.enter();
        }
    }

    // The path of the file in the selected commit, for the page bar.
    pub fn path(&self) -> &str {
        &self.history[self.selected].path
    }

    // While typing a search, every key goes to it as is.
    pub fn is_typing(&self) -> bool {
        self.query.is_editing()
    }

    pub fn handle_event(&mut self, event: UserEvent, key: KeyEvent, repository: &Repository) {
        if self.query.is_editing() {
            let kind = self.query_kind();
            if type_query(&self.ctx, &mut self.query, key, kind) == QueryChange::Search {
                self.search(SearchDirection::Current, repository);
            }
            self.update_query_status();
            return;
        }
        let in_diff = self.pane.is_focused();
        match event {
            // the search, then out of the lines and hunks, then closes
            UserEvent::Cancel | UserEvent::Close => {
                if self.query.clear() != QueryChange::None {
                    self.update_query_status();
                } else if !self.pane.back() {
                    self.tx.send(AppEvent::CloseOverlay);
                }
            }
            UserEvent::FocusPanel(0) => while self.pane.back() {},
            UserEvent::FocusPanel(1) | UserEvent::NavigateRight if !in_diff => {
                self.pane.enter();
            }
            UserEvent::NavigateLeft if in_diff => {
                self.pane.back();
            }
            UserEvent::OlderCommit => self.select(self.selected + 1, repository),
            UserEvent::NewerCommit => self.select(self.selected.saturating_sub(1), repository),
            UserEvent::Search => {
                self.query.start(QueryKind::Search);
                self.update_query_status();
            }
            UserEvent::GoToNext => self.search(SearchDirection::Next, repository),
            UserEvent::GoToPrevious => self.search(SearchDirection::Previous, repository),
            _ if in_diff => self.handle_diff_event(event),
            _ => self.handle_commits_event(event, repository),
        }
    }

    // The keys on the commits: moving, and acting on the selected one; the scroll keys scroll the
    // diff.
    fn handle_commits_event(&mut self, event: UserEvent, repository: &Repository) {
        let last = self.history.len() - 1;
        let index = match event {
            UserEvent::NavigateDown | UserEvent::SelectDown => Some(self.selected + 1),
            UserEvent::NavigateUp | UserEvent::SelectUp => Some(self.selected.saturating_sub(1)),
            UserEvent::GoToTop => Some(0),
            UserEvent::GoToBottom => Some(last),
            _ => half_page_jump(event, self.selected, self.height, self.history.len()),
        };
        if let Some(index) = index {
            self.select(index, repository);
        } else if !self.pane.scroll(event) {
            let commit = &self.history[self.selected].commit;
            if let Some(event) = commit_event(event, commit) {
                self.tx.send(event);
            }
        }
    }

    // The keys in the diff: moving the selected hunk or line, copying it and linking to it.
    fn handle_diff_event(&mut self, event: UserEvent) {
        let step = Step::of(event);
        if let Some(step) = step {
            self.pane.step(step);
            return;
        }
        match event {
            UserEvent::Confirm => {
                self.pane.enter();
            }
            UserEvent::ShortCopy => {
                if let Some(text) = self.pane.selected_text() {
                    let line = self.pane.is_selecting_lines();
                    self.tx.send(copy_selection(text, line));
                }
            }
            // the file at the commit, at the selected lines
            UserEvent::OpenInBrowser | UserEvent::CopyLink => {
                let revision = &self.history[self.selected];
                let lines = self.pane.selected_lines();
                let link = file_link(event, &revision.commit, &revision.path, lines);
                self.tx.send(link);
            }
            _ => {
                self.pane.scroll(event);
            }
        }
    }

    // Searches the diff while it has the focus, else the commits, by hash, date or subject.
    fn search(&mut self, direction: SearchDirection, repository: &Repository) {
        let Some(search) = self.query.search().map(String::from) else {
            return;
        };
        let options = self.ctx.search_options.get();
        let found = if self.pane.is_focused() {
            self.pane.search(&search, direction, options)
        } else {
            let history = &self.history;
            let found = find_around(history.len(), self.selected, direction, |i| {
                revision_matches(&self.ctx, &history[i], &search, options)
            });
            if let Some(index) = found {
                self.select(index, repository);
            }
            found.is_some()
        };
        // while typing, the status line shows the query instead
        if !found && !self.query.is_editing() {
            let message = format!("No match for {search}");
            self.tx.send(AppEvent::NotifyWarn(message));
        }
    }

    fn update_query_status(&self) {
        let typed = self.query.status_input();
        self.tx
            .send(query_status(&self.ctx, typed, self.query_kind()));
    }

    // The text or the diff is searched as text, the commits by name.
    fn query_kind(&self) -> SearchKind {
        if self.pane.is_focused() {
            SearchKind::Text
        } else {
            SearchKind::Names
        }
    }

    // The keys working where the focus is, the most useful first: the status bar shows as many as
    // fit, the keys popup all.
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

    // The keys of the timeline, see `Keys`.
    pub fn register_keys(k: &mut Keys<Self>) {
        use UserEvent::*;
        let diff = |v: &Self| v.pane.is_focused();
        let commits = |v: &Self| !v.pane.is_focused();
        k.add(
            &[OlderCommit, NewerCommit],
            "older/newer commit",
            "Select the older/newer commit, from the diff too",
        );
        k.add(
            &[Confirm],
            "select lines",
            "Select the commit on the graph, closing the window; in the diff, select lines",
        )
        .when(diff);
        k.add(&[ShortCopy], "copy", "Copy the hunk or line, in the diff")
            .when(diff);
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
        .when(commits);
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
            &[FocusPanel(0), FocusPanel(1)],
            "focus panel",
            "Focus the commits/the diff",
        );
        k.add(
            &[Cancel, Close],
            "back",
            "Clear the search, back out of the diff, then close",
        )
        .when(diff);
        k.add(
            &[Cancel, Close],
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
        let width = ctx
            .layout
            .borrow()
            .size(Panel::TimelineCommits, area.width * 35 / 100);
        let width = fit_size(width, area.width, 20, 20);
        let [list_area, diff_area] =
            Layout::horizontal([Constraint::Length(width), Constraint::Min(0)]).areas(area);
        ctx.layout
            .borrow_mut()
            .place(Panel::TimelineCommits, list_area, Edge::Right);

        // the commits, the selected one with the bar
        let focused = !self.pane.is_focused();
        let count = self.history.len();
        let position = format!("{}/{count}", count - self.selected);
        let block = panel_block(focused, theme).title(panel_title(
            "[0] Commits",
            &position,
            focused,
            theme,
        ));
        let inner = block.inner(list_area);
        block.render(list_area, buf);
        self.height = inner.height as usize;
        let scrolloff = ctx.ui_config.common.scrolloff as usize;
        self.offset = scroll_to(self.offset, self.selected, self.height, count, scrolloff);
        let rows = self.history.iter().enumerate().skip(self.offset);
        for ((i, revision), y) in rows.zip(inner.top()..inner.bottom()) {
            if i == self.selected {
                mark_selected_row(buf, inner, y, focused, theme);
            }
            // the search of the commits highlighted, not the diff's
            let search = self.query.search().filter(|_| focused);
            let line = Line::from(revision_spans(&ctx, revision, search));
            buf.set_line(inner.x, y, &line, inner.width);
        }
        render_panel_scrollbar(buf, list_area, count, self.offset, focused, theme);

        // the file's changes in it
        let path = &self.history[self.selected].path;
        let block =
            panel_block(!focused, theme).title(panel_title("[1] Diff", path, !focused, theme));
        let inner = block.inner(diff_area);
        block.render(diff_area, buf);
        // the scrollbar on the right border, the selection bar on the left one
        let search = self.query.search().filter(|_| !focused);
        self.pane.render(inner, buf, &ctx, !focused, search);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    use super::*;
    use crate::{
        event::EventController,
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
        press(&mut view, UserEvent::NewerCommit);
        press(&mut view, UserEvent::NavigateRight);
        press(&mut view, UserEvent::OlderCommit);
        press(&mut view, UserEvent::OlderCommit);
        assert!(raw(&view).contains("+two"));

        // j and k move between the commits
        press(&mut view, UserEvent::NavigateUp);
        press(&mut view, UserEvent::NavigateUp);
        press(&mut view, UserEvent::NavigateUp);
        assert_eq!(view.selected, 0);
        press(&mut view, UserEvent::GoToBottom);
        assert_eq!(view.selected, 2);

        // the commits searched by subject
        press(&mut view, UserEvent::Search);
        for c in "c2".chars() {
            let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
            view.handle_event(UserEvent::Unknown, key, &repository);
        }
        assert_eq!(view.history[view.selected].subject, "c2");
    }
}
