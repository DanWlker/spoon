use std::rc::Rc;

use ratatui::{
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::Widget,
    Frame,
};
use rustc_hash::FxHashMap;
use unicode_width::UnicodeWidthStr;

use crate::{
    app::AppContext,
    background::Job,
    event::{AppEvent, Sender, UserEvent},
    git::{Blame, CommitHash, FileRevision, Repository},
    keys::{KeyList, Keys},
    layout::{fit_size, Direction, Edge, Panel},
    search::{SearchKind, SearchOptions, SearchOutcome},
    view::{
        commit_event, edit_files, file_link, query_status, register_commit_keys,
        register_moving_keys, revision_spans, search_status, type_query, typing_keys,
        SHORT_HASH_WIDTH,
    },
    widget::{
        commit_list::{format_date, ELLIPSIS},
        diff::Step,
        file_text::{FileText, LineCursor},
        style::{choice_arrows, loading_note, mark_selected_bar, panel_block, panel_title},
        tree_query::{
            find_around, highlighted_text, name_matches, QueryKind, SearchDirection, TreeQuery,
        },
    },
};

// The width the authors' names are cut to.
const AUTHOR_WIDTH: usize = 16;

// What has the focus: the commit shown at the top, or a line, its commit or its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlameFocus {
    Revision,
    Commit,
    Text,
}

// The author and the date of a commit, as the blame shows them beside its lines.
#[derive(Debug)]
struct BlameLabel {
    author: String,
    date: String,
}

// Who last changed each line of a file, over everything: the commit it is shown as of at the top,
// stepped through the commits changing the file, and below the commit of each line beside its
// text, see `UserEvent::Blame`.
#[derive(Debug)]
pub struct BlameView {
    // the commits changing the file, the newest first
    history: Vec<FileRevision>,
    // the commit shown, of `history`
    index: usize,
    // the commit of each line
    commits: Vec<CommitHash>,
    // the author and date of each commit, as shown, and the width they are given
    labels: FxHashMap<CommitHash, BlameLabel>,
    author_width: usize,
    date_width: usize,
    text: FileText,
    focus: BlameFocus,
    query: TreeQuery,
    // the blame of another commit loading on another thread, with the commit shown before, which
    // stays when it fails
    loading: Option<(usize, Job<Result<Blame, String>>)>,
    ctx: Rc<AppContext>,
    tx: Sender,
}

impl BlameView {
    // The blame of the file as of the last commit changing it up to `commit`, on `line` (from 0).
    #[cfg(test)]
    pub fn new(
        repository: &Repository,
        commit: &CommitHash,
        path: &str,
        line: Option<usize>,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Result<Self, String> {
        let (history, index, blame) = repository.file_blame_work(commit, path)()?;
        Ok(Self::of(history, index, blame, line, ctx, tx))
    }

    // The view of the file's `history` with the `blame` of the revision at `index`, loaded on
    // another thread, see `Repository::file_blame_work`.
    pub fn of(
        history: Vec<FileRevision>,
        index: usize,
        blame: Blame,
        line: Option<usize>,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        let mut view = BlameView {
            history,
            index,
            commits: Vec::new(),
            labels: FxHashMap::default(),
            author_width: 0,
            date_width: 0,
            text: FileText::default(),
            focus: BlameFocus::Commit,
            query: TreeQuery::default(),
            loading: None,
            ctx,
            tx,
        };
        view.show(blame);
        if let Some(line) = line {
            view.text.select_line(line);
            view.focus = BlameFocus::Text;
        }
        view
    }

    fn show(&mut self, blame: Blame) {
        let line = self.text.selected_line();
        let (commits, lines) = blame.lines.into_iter().unzip();
        self.commits = commits;
        self.labels = blame
            .commits
            .into_iter()
            .map(|(commit, c)| {
                let date = format_date(&self.ctx, &c.date);
                (
                    commit,
                    BlameLabel {
                        author: c.author,
                        date,
                    },
                )
            })
            .collect();
        let widest = |f: fn(&BlameLabel) -> &String| {
            self.labels
                .values()
                .map(|l| f(l).width())
                .max()
                .unwrap_or(0)
        };
        self.author_width = widest(|l| &l.author).min(AUTHOR_WIDTH);
        self.date_width = widest(|l| &l.date);
        self.text.set_lines(lines);
        self.text.select_line(line);
    }

    // The path of the file as of the commit shown, for the page bar.
    pub fn path(&self) -> &str {
        &self.history[self.index].path
    }

    // While typing a search, every key goes to it as is.
    pub fn is_typing(&self) -> bool {
        self.query.is_editing()
    }

    pub fn handle_event(&mut self, event: UserEvent, key: KeyEvent, repository: &Repository) {
        // stepping to another commit replaces the blame loading, the others may act on it
        let steps = matches!(event, UserEvent::OlderCommit | UserEvent::NewerCommit)
            || (self.focus == BlameFocus::Revision
                && matches!(event, UserEvent::NavigateLeft | UserEvent::NavigateRight));
        if self.query.is_editing() || !steps {
            self.wait_loading();
        }
        if self.query.is_editing() {
            let kind = self.query_kind();
            if type_query(&self.ctx, &mut self.query, key, kind) == Some(QueryKind::Search) {
                self.search(SearchDirection::Current);
            }
            self.update_query_status();
            return;
        }
        let body = self.focus != BlameFocus::Revision;
        match event {
            // the search, then up from the lines to the commit shown, then closes, like the timeline
            UserEvent::Cancel => {
                if self.query.clear().is_some() {
                    self.update_query_status();
                } else if self.text.has_selection() {
                    self.text.cancel_visual();
                } else if body {
                    self.focus = BlameFocus::Revision;
                } else {
                    self.tx.send(AppEvent::CloseOverlay);
                }
            }
            UserEvent::FocusPanel(1) => self.focus = BlameFocus::Revision,
            UserEvent::FocusPanel(2) if !body => self.focus = BlameFocus::Commit,
            UserEvent::NavigateLeft if body => self.focus = BlameFocus::Commit,
            UserEvent::NavigateRight if body => self.focus = BlameFocus::Text,
            UserEvent::NavigateDown if !body => self.focus = BlameFocus::Commit,
            // up from the first line to the commit shown
            UserEvent::NavigateUp if body && self.text.selected_line() == 0 => {
                self.focus = BlameFocus::Revision
            }
            // on the commit shown, left and right step too, like a popup's option
            UserEvent::OlderCommit | UserEvent::NavigateLeft => {
                self.select(self.index + 1, repository)
            }
            UserEvent::NewerCommit | UserEvent::NavigateRight => {
                self.select(self.index.saturating_sub(1), repository)
            }
            UserEvent::Search if body => {
                // from the commits of the lines, or from the text, wherever the line is
                self.query
                    .start(QueryKind::Search, self.text.selected_line());
                self.update_query_status();
            }
            UserEvent::GoToNext if body => self.search(SearchDirection::Next),
            UserEvent::GoToPrevious if body => self.search(SearchDirection::Previous),
            // the file under its newest name, as it is in the working tree
            UserEvent::Edit => self
                .tx
                .send(edit_files(repository, &[&self.history[0].path])),
            _ if body => self.handle_line_event(event),
            _ => {
                let commit = &self.history[self.index].commit;
                if let Some(event) = commit_event(event, commit) {
                    self.tx.send(event);
                }
            }
        }
    }

    // The keys on a line: moving, and acting on its commit or its text.
    fn handle_line_event(&mut self, event: UserEvent) {
        let step = Step::of(event);
        if let Some(step) = step {
            self.text.step(step);
            return;
        }
        if event == UserEvent::ToggleSelect && self.focus == BlameFocus::Text {
            self.text.toggle_visual();
            return;
        }
        if self.text.scroll(event) {
            return;
        }
        let line = self.text.selected_line();
        match self.focus {
            BlameFocus::Commit => {
                let event = self.commits.get(line).and_then(|c| commit_event(event, c));
                if let Some(event) = event {
                    self.tx.send(event);
                }
            }
            BlameFocus::Text => match event {
                UserEvent::ShortCopy => {
                    if let Some(text) = self.text.selected_text() {
                        let name = if self.text.has_selection() {
                            "Lines"
                        } else {
                            "Line"
                        };
                        self.tx.send(AppEvent::copy(name, text));
                    }
                }
                // the file at the commit shown, at the lines selected, else the line
                UserEvent::OpenInBrowser | UserEvent::CopyLink => {
                    let revision = &self.history[self.index];
                    let lines = self.text.selected_lines();
                    let lines = Some(lines.start() + 1..=lines.end() + 1);
                    let link = file_link(event, &revision.commit, &revision.path, lines);
                    self.tx.send(link);
                }
                _ => {}
            },
            BlameFocus::Revision => {}
        }
    }

    // Shows the blame as of another commit changing the file, as the timeline selects one.
    // The blame loads on another thread, said in place of the text meanwhile, see `poll_loading`;
    // one still loading is replaced, the blame shown before it staying when this one fails.
    fn select(&mut self, index: usize, repository: &Repository) {
        let index = index.min(self.history.len() - 1);
        if index == self.index {
            return;
        }
        let shown = self.loading.take().map_or(self.index, |(shown, _)| shown);
        self.index = index;
        let revision = &self.history[index];
        let work = repository.blame_work(&revision.commit, &revision.path);
        self.loading = Some((shown, Job::spawn(self.tx.clone(), work)));
    }

    // Before drawing: the blame loaded on another thread, once it is.
    pub fn poll_loading(&mut self) {
        if let Some((shown, job)) = &self.loading {
            if let Some(blame) = job.try_take() {
                let shown = *shown;
                self.loading = None;
                self.show_loaded(shown, blame);
            }
        }
    }

    // The blame loading, waiting for it, before a key acts on it.
    fn wait_loading(&mut self) {
        if let Some((shown, job)) = self.loading.take() {
            self.show_loaded(shown, job.wait());
        }
    }

    // Failing, the commit shown before stays.
    fn show_loaded(&mut self, shown: usize, blame: Result<Blame, String>) {
        match blame {
            Ok(blame) => self.show(blame),
            Err(err) => {
                self.index = shown;
                self.tx.send(AppEvent::NotifyError(err));
            }
        }
    }

    #[cfg(test)]
    pub fn is_loading(&self) -> bool {
        self.loading.is_some()
    }

    // Searches what has the focus: the commits of the lines, by hash, author or date, from one run
    // of lines to the next, or the text.
    fn search(&mut self, direction: SearchDirection) {
        let Some(search) = self.query.search().map(String::from) else {
            return;
        };
        let options = self.ctx.search_options.get();
        match self.focus {
            // the commit shown isn't searched
            BlameFocus::Revision => return,
            BlameFocus::Commit => {
                let from = self.query.step_from(self.text.selected_line());
                let found = find_around(self.commits.len(), from, direction, |i| {
                    self.commit_matches(i, &search, options)
                });
                if let Some(line) = found {
                    self.text.select_line(line);
                }
            }
            BlameFocus::Text => {
                let from = self.query.step_from(self.text.selected_line());
                self.text.search(&search, from, direction, options);
            }
        }
        self.update_query_status();
    }

    // Whether the line at `line` starts the commit a search for `query` matches: the blame shows a
    // commit once, on its first line.
    fn commit_matches(&self, line: usize, query: &str, options: SearchOptions) -> bool {
        let commit = &self.commits[line];
        let starts = line == 0 || self.commits[line - 1] != *commit;
        let BlameLabel { author, date } = &self.labels[commit];
        starts
            && [commit.as_str(), author, date]
                .iter()
                .any(|text| name_matches(text, query, options))
    }

    // How the search of what has the focus went: the commit of the line, or the text.
    fn search_outcome(&self) -> Option<SearchOutcome> {
        let query = self.query.search()?;
        let options = self.ctx.search_options.get();
        let matched = match self.focus {
            // the commit shown isn't searched
            BlameFocus::Revision => return None,
            BlameFocus::Commit => (0..self.commits.len())
                .map(|line| self.commit_matches(line, query, options))
                .collect::<Vec<_>>(),
            BlameFocus::Text => return Some(self.text.search_outcome(query, options)),
        };
        Some(SearchOutcome::of(
            query,
            self.text.selected_line(),
            matched.into_iter(),
        ))
    }

    fn update_query_status(&self) {
        let kind = self.query_kind();
        let event = match self.query.is_editing() {
            true => query_status(&self.ctx, &self.query, kind),
            false => search_status(kind, self.ctx.search_options.get(), self.search_outcome()),
        };
        self.tx.send(event);
    }

    // The text is searched as text, the commits by name.
    fn query_kind(&self) -> SearchKind {
        match self.focus {
            BlameFocus::Text => SearchKind::Text,
            _ => SearchKind::Names,
        }
    }

    // Alt and the arrow keys: up to the commit shown, down to the lines, left and right between
    // the commits and the text of the lines.
    pub fn move_focus(&mut self, direction: Direction) {
        use BlameFocus::*;
        self.focus = match (self.focus, direction) {
            (Revision, Direction::Down) => Commit,
            (Commit | Text, Direction::Up) => Revision,
            (Text, Direction::Left) => Commit,
            (Commit, Direction::Right) => Text,
            (focus, _) => focus,
        };
    }

    // The keys of the blame working where the focus is, the most useful first: the status bar
    // shows as many as fit, the keys popup all; see `Keys`.
    pub fn register_keys(k: &mut Keys<Self>) {
        use BlameFocus::{Commit, Revision, Text};
        use UserEvent::*;
        let on = |focus: BlameFocus| move |v: &Self| v.focus == focus;
        let body = |v: &Self| v.focus != Revision;
        k.add(
            &[OlderCommit, NewerCommit],
            "older/newer commit",
            "Blame as of the older/newer commit changing the file",
        );
        // on the commit shown, left and right step too
        k.add(
            &[NavigateLeft, NavigateRight],
            "older/newer commit",
            "On the commit shown, blame as of the older/newer commit; else act on the commit or the text of the line",
        )
        .when(on(Revision));
        register_commit_keys(k, |v| v.focus != Text);
        k.add(
            &[NavigateDown],
            "to the lines",
            "Move down; from the commit shown to the lines",
        )
        .when(on(Revision));
        k.add(
            &[NavigateRight],
            "to the text",
            "On the commit shown, blame as of the older/newer commit; else act on the commit or the text of the line",
        )
        .when(on(Commit));
        k.add(&[ShortCopy], "copy line", "Copy the line, on its text")
            .when(on(Text))
            .short(|v| {
                if v.text.has_selection() {
                    "copy lines"
                } else {
                    "copy line"
                }
                .into()
            });
        k.add(
            &[ToggleSelect],
            "select lines",
            "Select the lines from the first to the selected one, on its text",
        )
        .when(on(Text))
        .short(|v| {
            if v.text.is_visual() {
                "end select"
            } else {
                "select lines"
            }
            .into()
        });
        k.add(
            &[OpenInBrowser],
            "open in browser",
            "Open the file at the line, or the lines selected, on the web host, on its text",
        )
        .when(on(Text));
        k.add(
            &[CopyLink],
            "copy link",
            "Copy the link of the file at the line, or the lines selected, on its text",
        )
        .when(on(Text));
        k.add(
            &[NavigateLeft],
            "to the commits",
            "On the commit shown, blame as of the older/newer commit; else act on the commit or the text of the line",
        )
        .when(on(Text));
        register_moving_keys(k, body);
        k.add(
            &[Search],
            "search",
            "Search the lines' commits (hash, author, date) or the text",
        )
        .when(body);
        k.add(
            &[GoToNext, GoToPrevious],
            "next/previous match",
            "Go to the next/previous match",
        )
        .when(body);
        k.add(
            &[Edit],
            "edit",
            "Open the file, under its newest name, as it is in the working tree in $VISUAL or $EDITOR",
        );
        k.quiet(
            &[FocusPanel(1)],
            "Focus the commit shown (also up from the first line)",
        )
        .when(body);
        k.quiet(&[FocusPanel(2)], "Focus the lines")
            .when(on(Revision));
        k.add(
            &[Cancel],
            "back",
            "Clear the search, the selected lines, then back to the commit shown, then close",
        )
        .when(body);
        k.add(
            &[Cancel],
            "close",
            "Clear the search, the selected lines, then back to the commit shown, then close",
        )
        .when(on(Revision));
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
        let [top, body] = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(area);

        // the commit shown, with its date and subject, stepped through with left and right
        let focused = self.focus == BlameFocus::Revision;
        let (count, revision) = (self.history.len(), &self.history[self.index]);
        let position = format!("{}/{count}", count - self.index);
        let block = panel_block(focused, theme).title(panel_title(
            "[1] Commit",
            [position],
            focused,
            theme,
        ));
        // [ and ] step from anywhere
        let [left, right] = choice_arrows(self.index + 1 < count, self.index > 0, theme);
        let mut spans = vec![left];
        spans.extend(revision_spans(&ctx, revision, None));
        spans.push(right);
        let line = Line::from(spans);
        let inner = block.inner(top);
        block.render(top, buf);
        line.render(inner, buf);

        // the lines, each run of lines of a commit headed by it
        let focused = !focused;
        // the search applies to the text or the commits, both in this panel
        let notes = [revision.path.clone()]
            .into_iter()
            .chain(self.query.indicator())
            .chain(loading_note(
                self.loading.as_ref().map(|(_, job)| job.started()),
            ));
        let block =
            panel_block(focused, theme).title(panel_title("[2] Blame", notes, focused, theme));
        let inner = block.inner(body);
        block.render(body, buf);
        // no room for the commits' divider
        if inner.is_empty() {
            return;
        }
        let author_width = self.author_width;
        // the commits with the line after them, resizable; both keep some room
        let default = (SHORT_HASH_WIDTH + 1 + author_width + 1 + self.date_width + 1) as u16;
        let width = ctx.layout.borrow().size(Panel::BlameCommits, default);
        let width = fit_size(width, inner.width, 8, 10);
        let [commits_area, text_area] =
            Layout::horizontal([Constraint::Length(width), Constraint::Min(0)]).areas(inner);
        ctx.layout
            .borrow_mut()
            .place(Panel::BlameCommits, commits_area, Edge::Right);
        let gutter = Rect {
            width: commits_area.width - 1,
            ..commits_area
        };
        for y in commits_area.top()..commits_area.bottom() {
            let x = commits_area.right() - 1;
            buf.set_string(x, y, "│", Style::default().fg(theme.divider_fg));
        }
        // the search highlighted where it applies, in the text or the commits
        let search_in = |focus| self.query.search().filter(|_| self.focus == focus);
        let text_search = search_in(BlameFocus::Text);
        let search = search_in(BlameFocus::Commit);
        let rows = self.text.render(
            text_area,
            buf,
            &ctx,
            focused,
            LineCursor::Shown,
            text_search,
        );

        let selected = self.text.selected_line();
        let selected_style = Style::default().bg(theme.list_selected_bg);
        let mut previous = None;
        for (y, &row) in (gutter.y..).zip(&rows) {
            if row == selected {
                buf.set_style(Rect::new(gutter.x, y, gutter.width, 1), selected_style);
                let commit_focused = self.focus == BlameFocus::Commit;
                mark_selected_bar(buf, gutter.x - 1, y, commit_focused, theme);
                let text_focused = self.focus == BlameFocus::Text;
                mark_selected_bar(buf, text_area.x - 1, y, text_focused, theme);
            }
            // the first row of a line starting a run, or the top one
            let commit = &self.commits[row];
            let heads =
                previous.is_none() || (previous != Some(row) && self.commits[row - 1] != *commit);
            previous = Some(row);
            if !heads {
                continue;
            }
            let BlameLabel { author, date } = &self.labels[commit];
            // cut and padded to the width, as the commit list cuts names
            let author = console::truncate_str(author, author_width, ELLIPSIS);
            let author = console::pad_str(&author, author_width, console::Alignment::Left, None);
            let options = ctx.search_options.get();
            let text = |text: &str, shown: usize, fg| {
                highlighted_text(text, shown, Style::default().fg(fg), search, options, theme)
            };
            let mut spans = text(commit.as_str(), SHORT_HASH_WIDTH, theme.list_hash_fg);
            spans.push(Span::raw(" "));
            spans.extend(text(&author, author.len(), theme.list_name_fg));
            spans.push(Span::raw(" "));
            spans.extend(text(date, date.len(), theme.list_date_fg));
            let line = Line::from(spans);
            buf.set_line(gutter.x, y, &line, gutter.width);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ratatui::{
        backend::TestBackend,
        buffer::Buffer,
        crossterm::event::{KeyCode, KeyModifiers},
        Terminal,
    };

    use super::*;
    use crate::{
        event::EventController,
        git::SortCommit,
        test_support::{
            git::{git, GitRepository},
            ui::test_ctx,
        },
        widget::git_dialog::{CheckoutTarget, GitDialog, GitDialogWidget},
    };

    #[test]
    fn test_blame_steps_through_the_history_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        fs::write(path.join("a"), "one\ntwo\n").unwrap();
        repo.add(&["a"]);
        repo.commit("c1", "2024-01-01");
        let c1 = CommitHash::from(repo.rev_parse_head().as_str());
        fs::write(path.join("a"), "one\nTWO\n").unwrap();
        repo.add(&["a"]);
        repo.commit("c2", "2024-01-02");
        let c2 = CommitHash::from(repo.rev_parse_head().as_str());
        git(path, &["mv", "a", "b"]);
        repo.commit("c3", "2024-01-03");
        let c3 = CommitHash::from(repo.rev_parse_head().as_str());
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let ec = EventController::without_terminal();
        let mut view =
            BlameView::new(&repository, &c3, "b", Some(1), test_ctx(), ec.sender()).unwrap();
        let key = KeyEvent::new(KeyCode::Null, KeyModifiers::NONE);
        let press = |view: &mut BlameView, event| view.handle_event(event, key, &repository);
        let shown = |view: &BlameView| {
            let revision = &view.history[view.index];
            (
                revision.commit.clone(),
                revision.path.clone(),
                view.commits.clone(),
            )
        };

        // the rename keeps the lines' commits
        assert_eq!(
            shown(&view),
            (c3.clone(), "b".into(), vec![c1.clone(), c2.clone()])
        );
        assert_eq!(view.focus, BlameFocus::Text);
        // older, before the rename, on the same line, loaded on another thread
        press(&mut view, UserEvent::OlderCommit);
        assert!(view.is_loading());
        view.wait_loading();
        assert_eq!(
            shown(&view),
            (c2.clone(), "a".into(), vec![c1.clone(), c2.clone()])
        );
        assert_eq!(view.text.selected_line(), 1);
        // e opens the file under its newest name, not the one before the rename
        while ec.try_recv().is_some() {}
        press(&mut view, UserEvent::Edit);
        match ec.try_recv() {
            Some(AppEvent::RunExternal(command)) => {
                assert_eq!(command.last(), Some(&path.join("b").display().to_string()))
            }
            event => panic!("no editor run: {event:?}"),
        }
        // O copies the link of the file at the lines selected
        press(&mut view, UserEvent::GoToTop);
        press(&mut view, UserEvent::ToggleSelect);
        press(&mut view, UserEvent::NavigateDown);
        while ec.try_recv().is_some() {}
        press(&mut view, UserEvent::CopyLink);
        match ec.try_recv() {
            Some(AppEvent::Link {
                target: crate::browse::LinkTarget::File { lines, .. },
                ..
            }) => assert_eq!(lines, Some(1..=2)),
            event => panic!("no link: {event:?}"),
        }
        press(&mut view, UserEvent::ToggleSelect);
        press(&mut view, UserEvent::Cancel);
        press(&mut view, UserEvent::OlderCommit);
        press(&mut view, UserEvent::OlderCommit);
        view.wait_loading();
        assert_eq!(
            shown(&view),
            (c1.clone(), "a".into(), vec![c1.clone(), c1.clone()])
        );
        press(&mut view, UserEvent::NewerCommit);
        press(&mut view, UserEvent::NewerCommit);
        press(&mut view, UserEvent::NewerCommit);
        assert_eq!(shown(&view).0, c3);
        view.wait_loading();
        // stepping again while one loads replaces it, the blame shown before both staying on failure
        press(&mut view, UserEvent::OlderCommit);
        press(&mut view, UserEvent::OlderCommit);
        assert_eq!(view.loading.as_ref().map(|(shown, _)| *shown), Some(0));
        view.wait_loading();
        assert_eq!(
            shown(&view),
            (c1.clone(), "a".into(), vec![c1.clone(), c1.clone()])
        );
        press(&mut view, UserEvent::NewerCommit);
        press(&mut view, UserEvent::NewerCommit);
        assert_eq!(shown(&view).0, c3);

        // up from the first line to the commit shown, down back to the lines
        press(&mut view, UserEvent::GoToTop);
        press(&mut view, UserEvent::NavigateUp);
        assert_eq!(view.focus, BlameFocus::Revision);
        press(&mut view, UserEvent::NavigateDown);
        assert_eq!(view.focus, BlameFocus::Commit);
        // alt and the arrows move about, left and right in the lines
        view.move_focus(Direction::Right);
        assert_eq!(view.focus, BlameFocus::Text);
        view.move_focus(Direction::Left);
        view.move_focus(Direction::Up);
        assert_eq!(view.focus, BlameFocus::Revision);
        view.move_focus(Direction::Down);
        assert_eq!(view.focus, BlameFocus::Commit);
        // 1 and 2 too, each shown only away from its panel
        assert!(!view.keys().takes(UserEvent::FocusPanel(2)));
        press(&mut view, UserEvent::FocusPanel(1));
        assert_eq!(view.focus, BlameFocus::Revision);
        assert!(!view.keys().takes(UserEvent::FocusPanel(1)));
        press(&mut view, UserEvent::FocusPanel(2));
        assert_eq!(view.focus, BlameFocus::Commit);

        // left and right on the commit shown step through the history too
        view.move_focus(Direction::Up);
        press(&mut view, UserEvent::NavigateLeft);
        assert_eq!(shown(&view).0, c2);
        press(&mut view, UserEvent::NavigateRight);
        assert_eq!(shown(&view).0, c3);
        assert_eq!(view.focus, BlameFocus::Revision);

        // searching the commits shown by hash, the lines' commits by hash, from run to run
        let search = |view: &mut BlameView, query: &str| {
            press(view, UserEvent::Search);
            for c in query.chars() {
                let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
                view.handle_event(UserEvent::Unknown, key, &repository);
            }
            let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
            view.handle_event(UserEvent::Unknown, enter, &repository);
        };
        // not on the commit shown
        search(&mut view, &c1.as_str()[..7]);
        assert_eq!(shown(&view).0, c3);
        press(&mut view, UserEvent::NavigateDown);
        search(&mut view, &c2.as_str()[..7]);
        assert_eq!(
            (view.focus, view.text.selected_line()),
            (BlameFocus::Commit, 1)
        );
        // esc: the search, then up to the commit shown, then closes
        press(&mut view, UserEvent::Cancel);
        assert_eq!(view.focus, BlameFocus::Commit);
        press(&mut view, UserEvent::Cancel);
        assert_eq!(view.focus, BlameFocus::Revision);

        // from an older commit not changing the file, as of the last one changing it
        let view = BlameView::new(&repository, &c2, "a", None, test_ctx(), ec.sender()).unwrap();
        assert_eq!(shown(&view).0, c2);
    }

    // The arrows of a value changed with left and right look the same in the blame and the popups:
    // each lit while its key changes the value.
    #[test]
    fn test_choice_arrows_alike_in_the_blame_and_the_popups() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        for (i, date) in ["2024-01-01", "2024-01-02", "2024-01-03"]
            .iter()
            .enumerate()
        {
            fs::write(path.join("a"), format!("{i}\n")).unwrap();
            repo.add(&["a"]);
            repo.commit("c", date);
        }
        let head = CommitHash::from(repo.rev_parse_head().as_str());
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let ec = EventController::without_terminal();
        let ctx = test_ctx();
        let (lit, muted) = (ctx.color_theme.highlight_fg, ctx.color_theme.muted_fg);
        let mut view =
            BlameView::new(&repository, &head, "a", None, ctx.clone(), ec.sender()).unwrap();
        let key = KeyEvent::new(KeyCode::Null, KeyModifiers::NONE);
        let arrows = |buf: &Buffer| {
            let fg = |symbol| {
                buf.content
                    .iter()
                    .find(|c| c.symbol() == symbol)
                    .unwrap()
                    .fg
            };
            (fg("‹"), fg("›"))
        };
        let blame_arrows = |view: &mut BlameView, event| {
            view.handle_event(event, key, &repository);
            let mut terminal = Terminal::new(TestBackend::new(60, 8)).unwrap();
            let frame = terminal.draw(|f| view.render(f, f.area())).unwrap();
            arrows(frame.buffer)
        };
        // the newest, the oldest, between them; whatever has the focus, as [ and ] work from anywhere
        assert_eq!(blame_arrows(&mut view, UserEvent::Unknown), (lit, muted));
        assert_eq!(blame_arrows(&mut view, UserEvent::OlderCommit), (lit, lit));
        assert_eq!(
            blame_arrows(&mut view, UserEvent::OlderCommit),
            (muted, lit)
        );
        // too narrow for the lines, drawn without them
        view.wait_loading();
        let mut terminal = Terminal::new(TestBackend::new(2, 8)).unwrap();
        terminal.draw(|f| view.render(f, f.area())).unwrap();

        // the first field of the popup, selected, its options going around
        let popup_arrows = |labels: &[&str]| {
            let targets = labels
                .iter()
                .map(|label| CheckoutTarget::Branch(label.to_string()))
                .collect();
            let dialog = GitDialog::checkout(targets);
            let area = Rect::new(0, 0, 80, 20);
            let mut buf = Buffer::empty(area);
            GitDialogWidget::new(&dialog, &ctx).render(area, &mut buf);
            arrows(&buf)
        };
        assert_eq!(popup_arrows(&["main", "feat"]), (lit, lit));
        assert_eq!(popup_arrows(&["main"]), (muted, muted));
    }

    // Stepping to another commit, the lines before stay shown until its blame loads, the title
    // saying so.
    #[test]
    fn test_lines_stay_while_the_next_blame_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        for (i, date) in ["2024-01-01", "2024-01-02"].iter().enumerate() {
            fs::write(path.join("a"), format!("text{i}\n")).unwrap();
            repo.add(&["a"]);
            repo.commit("c", date);
        }
        let head = CommitHash::from(repo.rev_parse_head().as_str());
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let ec = EventController::without_terminal();
        let mut view =
            BlameView::new(&repository, &head, "a", None, test_ctx(), ec.sender()).unwrap();
        let screen = |view: &mut BlameView| {
            let mut terminal = Terminal::new(TestBackend::new(60, 8)).unwrap();
            crate::test_support::ui::dump(
                terminal.draw(|f| view.render(f, f.area())).unwrap().buffer,
            )
        };
        let key = KeyEvent::new(KeyCode::Null, KeyModifiers::NONE);
        view.handle_event(UserEvent::OlderCommit, key, &repository);
        // the blame before stays, without a note while it is quick, see `shows_loading`
        let loading = screen(&mut view);
        assert!(
            loading.contains("text1") && !loading.contains("loading…"),
            "{loading}"
        );
        view.wait_loading();
        let loaded = screen(&mut view);
        assert!(
            loaded.contains("text0") && !loaded.contains("loading…"),
            "{loaded}"
        );
    }

    // A search's matches look the same in the blame's commits and the timeline's as everywhere.
    #[test]
    fn test_matches_highlighted_like_everywhere() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        fs::write(path.join("a"), "one\n").unwrap();
        repo.add(&["a"]);
        repo.commit("fix the thing", "2024-01-01");
        let head = CommitHash::from(repo.rev_parse_head().as_str());
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let ec = EventController::without_terminal();
        let ctx = test_ctx();
        let theme = &ctx.color_theme;
        let typed = |query: &str| {
            let mut keys: Vec<KeyEvent> = query
                .chars()
                .map(|c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
                .collect();
            keys.push(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            keys
        };
        // the text of the cells in the colors of a match
        let matched = |buf: &Buffer| -> String {
            buf.content
                .iter()
                .filter(|c| c.fg == theme.list_match_fg && c.bg == theme.list_match_bg)
                .map(|c| c.symbol())
                .collect()
        };
        let draw = |render: &mut dyn FnMut(&mut Frame)| {
            let mut terminal = Terminal::new(TestBackend::new(140, 8)).unwrap();
            matched(terminal.draw(|f| render(f)).unwrap().buffer)
        };
        let short = &head.as_str()[..7];

        let mut blame =
            BlameView::new(&repository, &head, "a", None, ctx.clone(), ec.sender()).unwrap();
        let key = KeyEvent::new(KeyCode::Null, KeyModifiers::NONE);
        blame.handle_event(UserEvent::Search, key, &repository);
        for key in typed(&short[..4]) {
            blame.handle_event(UserEvent::Unknown, key, &repository);
        }
        assert_eq!(draw(&mut |f| blame.render(f, f.area())), &short[..4]);

        let mut timeline =
            crate::view::TimelineView::new(&repository, &head, "a", ctx.clone(), ec.sender())
                .unwrap();
        timeline.handle_event(UserEvent::Search, key, &repository);
        for key in typed("thing") {
            timeline.handle_event(UserEvent::Unknown, key, &repository);
        }
        assert_eq!(draw(&mut |f| timeline.render(f, f.area())), "thing");

        // fuzzily, as the commit list and the file trees search
        let fuzzy = crate::search::SearchOptions {
            fuzzy: true,
            ..ctx.search_options.get()
        };
        ctx.search_options.set(fuzzy);
        timeline.handle_event(UserEvent::Cancel, key, &repository);
        timeline.handle_event(UserEvent::Search, key, &repository);
        for key in typed("ftg") {
            timeline.handle_event(UserEvent::Unknown, key, &repository);
        }
        assert_eq!(draw(&mut |f| timeline.render(f, f.area())), "ftg");
    }
}
