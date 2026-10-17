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
    event::{hint, AppEvent, KeyHint, Sender, UserEvent},
    git::{CommitHash, FileRevision, Repository},
    search::SearchKind,
    view::{
        commit_event, commit_hints, copy_selection, file_link, moving_hints, query_status,
        revision_spans, type_query, typing_hints,
    },
    widget::{
        commit_list::{format_date, ELLIPSIS},
        diff::Step,
        file_text::FileText,
        style::{choice_arrows, mark_selected_bar, panel_block, panel_title},
        tree_query::{
            find_around, highlighted_text, name_matches, QueryChange, QueryKind, SearchDirection,
            TreeQuery,
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
    labels: FxHashMap<CommitHash, (String, String)>,
    author_width: usize,
    date_width: usize,
    text: FileText,
    focus: BlameFocus,
    query: TreeQuery,
    ctx: Rc<AppContext>,
    tx: Sender,
}

impl BlameView {
    // The blame of the file as of the last commit changing it up to `commit`, on `line` (from 0).
    pub fn new(
        repository: &Repository,
        commit: &CommitHash,
        path: &str,
        line: Option<usize>,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Result<Self, String> {
        let (history, index) = repository.file_history(commit, path)?;
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
            ctx,
            tx,
        };
        view.load(repository)?;
        if let Some(line) = line {
            view.text.select_line(line);
            view.focus = BlameFocus::Text;
        }
        Ok(view)
    }

    // Blames the file as of the commit shown, keeping the selected line.
    fn load(&mut self, repository: &Repository) -> Result<(), String> {
        let revision = &self.history[self.index];
        let blame = repository.blame(&revision.commit, &revision.path)?;
        let line = self.text.selected_line();
        let (commits, lines) = blame.lines.into_iter().unzip();
        self.commits = commits;
        self.labels = blame
            .commits
            .into_iter()
            .map(|(commit, c)| (commit, (c.author, format_date(&self.ctx, &c.date))))
            .collect();
        let widest = |f: fn(&(String, String)) -> &String| {
            self.labels
                .values()
                .map(|l| f(l).width())
                .max()
                .unwrap_or(0)
        };
        self.author_width = widest(|l| &l.0).min(AUTHOR_WIDTH);
        self.date_width = widest(|l| &l.1);
        self.text.set_lines(lines);
        self.text.select_line(line);
        Ok(())
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
        if self.query.is_editing() {
            let kind = self.query_kind();
            if type_query(&self.ctx, &mut self.query, key, kind) == QueryChange::Search {
                self.search(SearchDirection::Current);
            }
            self.update_query_status();
            return;
        }
        let body = self.focus != BlameFocus::Revision;
        match event {
            UserEvent::ForceQuit | UserEvent::Quit => self.tx.send(AppEvent::Quit),
            UserEvent::Cancel | UserEvent::Close => match self.query.clear() {
                QueryChange::None => self.tx.send(AppEvent::CloseOverlay),
                _ => self.update_query_status(),
            },
            UserEvent::FocusPanel(0) => self.focus = BlameFocus::Revision,
            UserEvent::FocusPanel(1) if !body => self.focus = BlameFocus::Commit,
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
                self.query.start(QueryKind::Search);
                self.update_query_status();
            }
            UserEvent::GoToNext if body => self.search(SearchDirection::Next),
            UserEvent::GoToPrevious if body => self.search(SearchDirection::Previous),
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
                        self.tx.send(copy_selection(text, true));
                    }
                }
                // the file at the commit shown, on the line
                UserEvent::OpenInBrowser | UserEvent::CopyLink => {
                    let revision = &self.history[self.index];
                    let lines = Some((line + 1, line + 1));
                    let link = file_link(event, &revision.commit, &revision.path, lines);
                    self.tx.send(link);
                }
                _ => {}
            },
            BlameFocus::Revision => {}
        }
    }

    // Shows the blame as of another commit changing the file, as the timeline selects one.
    fn select(&mut self, index: usize, repository: &Repository) {
        let index = index.min(self.history.len() - 1);
        if index == self.index {
            return;
        }
        let shown = std::mem::replace(&mut self.index, index);
        if let Err(err) = self.load(repository) {
            self.index = shown;
            self.tx.send(AppEvent::NotifyError(err));
        }
    }

    // Searches what has the focus: the commits of the lines, by hash, author or date, from one run
    // of lines to the next, or the text.
    fn search(&mut self, direction: SearchDirection) {
        let Some(search) = self.query.search().map(String::from) else {
            return;
        };
        let options = self.ctx.search_options.get();
        let found = match self.focus {
            // the commit shown isn't searched
            BlameFocus::Revision => return,
            BlameFocus::Commit => {
                let commits = &self.commits;
                let found = find_around(commits.len(), self.text.selected_line(), direction, |i| {
                    let commit = &commits[i];
                    let starts = i == 0 || commits[i - 1] != *commit;
                    let (author, date) = &self.labels[commit];
                    starts
                        && [commit.as_str(), author, date]
                            .iter()
                            .any(|text| name_matches(text, &search, options))
                });
                if let Some(line) = found {
                    self.text.select_line(line);
                }
                found.is_some()
            }
            BlameFocus::Text => self.text.search(&search, direction, options),
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
        match self.focus {
            BlameFocus::Text => SearchKind::Text,
            _ => SearchKind::Names,
        }
    }

    // The keys working where the focus is, the most useful first: the status bar shows as many as
    // fit, the keys popup all.
    pub fn key_hints(&self) -> Vec<KeyHint> {
        use UserEvent::*;
        if self.query.is_editing() {
            return typing_hints(self.query_kind());
        }
        // on the commit shown, left and right step too
        let mut steps = vec![OlderCommit, NewerCommit];
        if self.focus == BlameFocus::Revision {
            steps.extend([NavigateLeft, NavigateRight]);
        }
        let mut hints = vec![(steps, "older/newer commit".into())];
        let commit = commit_hints();
        match self.focus {
            BlameFocus::Revision => {
                hints.extend(commit);
                hints.push(hint(NavigateDown, "to the lines"));
            }
            BlameFocus::Commit => {
                hints.extend(commit);
                hints.push(hint(NavigateRight, "to the text"));
            }
            BlameFocus::Text => hints.extend([
                hint(ShortCopy, "copy line"),
                hint(OpenInBrowser, "open in browser"),
                hint(CopyLink, "copy link"),
                hint(NavigateLeft, "to the commits"),
            ]),
        }
        if self.focus != BlameFocus::Revision {
            hints.extend(moving_hints());
            hints.extend([
                hint(Search, "search"),
                (vec![GoToNext, GoToPrevious], "next/previous match".into()),
            ]);
        }
        hints.extend([
            (vec![FocusPanel(0), FocusPanel(1)], "focus panel".into()),
            hint(ToggleWrap, "wrap"),
            hint(Cancel, "close"),
        ]);
        hints
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
        let block =
            panel_block(focused, theme).title(panel_title("[0] Commit", &position, focused, theme));
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
        let block = panel_block(focused, theme).title(panel_title(
            "[1] Blame",
            &revision.path,
            focused,
            theme,
        ));
        let inner = block.inner(body);
        block.render(body, buf);
        let author_width = self.author_width;
        let gutter_width = (7 + 1 + author_width + 1 + self.date_width) as u16;
        let [gutter, divider, text_area] = Layout::horizontal([
            Constraint::Length(gutter_width.min(inner.width / 2)),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .areas(inner);
        for y in divider.top()..divider.bottom() {
            buf.set_string(divider.x, y, "│", Style::default().fg(theme.divider_fg));
        }
        // the search highlighted where it applies, in the text or the commits
        let search_in = |focus| self.query.search().filter(|_| self.focus == focus);
        let text_search = search_in(BlameFocus::Text);
        let search = search_in(BlameFocus::Commit);
        let rows = self
            .text
            .render(text_area, buf, &ctx, focused, true, text_search);

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
            let (author, date) = &self.labels[commit];
            // cut and padded to the width, as the commit list cuts names
            let author = console::truncate_str(author, author_width, ELLIPSIS);
            let author = console::pad_str(&author, author_width, console::Alignment::Left, None);
            let options = ctx.search_options.get();
            let text = |text: &str, shown: usize, fg| {
                highlighted_text(text, shown, Style::default().fg(fg), search, options, theme)
            };
            let mut spans = text(commit.as_str(), 7, theme.list_hash_fg);
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
        // older, before the rename, on the same line
        press(&mut view, UserEvent::OlderCommit);
        assert_eq!(
            shown(&view),
            (c2.clone(), "a".into(), vec![c1.clone(), c2.clone()])
        );
        assert_eq!(view.text.selected_line(), 1);
        press(&mut view, UserEvent::OlderCommit);
        press(&mut view, UserEvent::OlderCommit);
        assert_eq!(
            shown(&view),
            (c1.clone(), "a".into(), vec![c1.clone(), c1.clone()])
        );
        press(&mut view, UserEvent::NewerCommit);
        press(&mut view, UserEvent::NewerCommit);
        press(&mut view, UserEvent::NewerCommit);
        assert_eq!(shown(&view).0, c3);

        // up from the first line to the commit shown, down back to the lines
        press(&mut view, UserEvent::GoToTop);
        press(&mut view, UserEvent::NavigateUp);
        assert_eq!(view.focus, BlameFocus::Revision);
        press(&mut view, UserEvent::NavigateDown);
        assert_eq!(view.focus, BlameFocus::Commit);
        press(&mut view, UserEvent::FocusPanel(0));
        press(&mut view, UserEvent::FocusPanel(1));
        assert_eq!(view.focus, BlameFocus::Commit);

        // left and right on the commit shown step through the history too
        press(&mut view, UserEvent::FocusPanel(0));
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
        press(&mut view, UserEvent::Cancel);

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

        // the first field of the popup, selected, its options going around
        let popup_arrows = |labels: &[&str]| {
            let targets = labels
                .iter()
                .map(|label| CheckoutTarget {
                    label: label.to_string(),
                    args: vec![label.to_string()],
                })
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
