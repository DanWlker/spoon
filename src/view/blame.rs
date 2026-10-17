use std::rc::Rc;

use ratatui::{
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::Widget,
    Frame,
};
use rustc_hash::FxHashMap;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
    app::AppContext,
    browse::LinkTarget,
    event::{hint, AppEvent, KeyHint, Sender, UserEvent},
    git::{BlameCommit, CommitHash, FileRevision, Repository},
    view::{commit_event, copy_selection, status_input},
    widget::{
        commit_list::format_date,
        diff::Step,
        file_text::FileText,
        style::{choice_arrows, mark_selected_bar, panel_block, panel_title},
        tree_query::{QueryChange, QueryKind, SearchDirection, TreeQuery},
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
    authors: FxHashMap<CommitHash, BlameCommit>,
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
            authors: FxHashMap::default(),
            text: FileText::default(),
            focus: BlameFocus::Commit,
            query: TreeQuery::default(),
            ctx,
            tx,
        };
        view.load(repository)?;
        if let Some(line) = line {
            view.select_line(line);
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
        self.authors = blame.commits;
        self.text.set_lines(lines);
        self.select_line(line);
        Ok(())
    }

    fn select_line(&mut self, line: usize) {
        if let Some(last) = self.commits.len().checked_sub(1) {
            self.text.select_line(line.min(last));
        }
    }

    // While typing a search, every key goes to it as is.
    pub fn is_typing(&self) -> bool {
        self.query.is_editing()
    }

    pub fn handle_event(&mut self, event: UserEvent, key: KeyEvent, repository: &Repository) {
        if self.query.is_editing() {
            if self.query.handle_key(key) == QueryChange::Search {
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
            UserEvent::OlderCommit
            | UserEvent::NewerCommit
            | UserEvent::NavigateLeft
            | UserEvent::NavigateRight => {
                let older = matches!(event, UserEvent::OlderCommit | UserEvent::NavigateLeft);
                self.step_history(older, repository);
            }
            UserEvent::ToggleWrap => self.ctx.wrap.set(!self.ctx.wrap.get()),
            UserEvent::Search => {
                self.query.start(QueryKind::Search);
                self.update_query_status();
            }
            UserEvent::GoToNext => self.search(SearchDirection::Next),
            UserEvent::GoToPrevious => self.search(SearchDirection::Previous),
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
        let step = match event {
            UserEvent::NavigateDown => Some(Step::Next),
            UserEvent::NavigateUp => Some(Step::Prev),
            UserEvent::GoToTop => Some(Step::First),
            UserEvent::GoToBottom => Some(Step::Last),
            _ => None,
        };
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
                    let target = LinkTarget::File {
                        commit: revision.commit.as_str().into(),
                        path: revision.path.clone(),
                        lines: Some((line + 1, line + 1)),
                    };
                    let copy = event == UserEvent::CopyLink;
                    self.tx.send(AppEvent::Link { target, copy });
                }
                _ => {}
            },
            BlameFocus::Revision => {}
        }
    }

    // Shows the blame as of the older or newer commit changing the file.
    fn step_history(&mut self, older: bool, repository: &Repository) {
        let index = if older {
            self.index + 1
        } else {
            self.index.wrapping_sub(1)
        };
        if index >= self.history.len() {
            return;
        }
        let shown = std::mem::replace(&mut self.index, index);
        if let Err(err) = self.load(repository) {
            self.index = shown;
            self.tx.send(AppEvent::NotifyError(err));
        }
    }

    fn search(&mut self, direction: SearchDirection) {
        let Some(search) = self.query.search() else {
            return;
        };
        let found = self
            .text
            .search(search, direction, self.ctx.search_options.get());
        // while typing, the status line shows the query instead
        if !found && !self.query.is_editing() {
            let message = format!("No match for {search}");
            self.tx.send(AppEvent::NotifyWarn(message));
        }
    }

    fn update_query_status(&self) {
        let typed = self.query.status_input();
        self.tx.send(status_input(typed, String::new()));
    }

    pub fn key_hints(&self) -> Vec<KeyHint> {
        // on the commit shown, left and right step too
        let mut steps = vec![UserEvent::OlderCommit, UserEvent::NewerCommit];
        if self.focus == BlameFocus::Revision {
            steps.extend([UserEvent::NavigateLeft, UserEvent::NavigateRight]);
        }
        let mut hints = vec![(steps, "older/newer commit".into())];
        let commit = [
            hint(UserEvent::Checkout, "checkout"),
            hint(UserEvent::OpenInBrowser, "open in browser"),
            (
                vec![UserEvent::ShortCopy, UserEvent::FullCopy],
                "copy hash".into(),
            ),
        ];
        match self.focus {
            BlameFocus::Revision => hints.extend(commit),
            BlameFocus::Commit => {
                hints.push(hint(UserEvent::Confirm, "show on graph"));
                hints.extend(commit);
            }
            BlameFocus::Text => hints.extend([
                hint(UserEvent::ShortCopy, "copy line"),
                hint(UserEvent::OpenInBrowser, "open in browser"),
            ]),
        }
        hints.extend([
            hint(UserEvent::Search, "search"),
            hint(UserEvent::ToggleWrap, "wrap"),
            hint(UserEvent::Cancel, "close"),
        ]);
        hints
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let ctx = self.ctx.clone();
        let theme = &ctx.color_theme;
        let buf = f.buffer_mut();
        let [top, body] = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(area);

        // the commit shown, with its date and subject, cycled with left and right
        let focused = self.focus == BlameFocus::Revision;
        let (count, revision) = (self.history.len(), &self.history[self.index]);
        let position = format!("{}/{count}", count - self.index);
        let block =
            panel_block(focused, theme).title(panel_title("[0] Commit", &position, focused, theme));
        let [left, right] = choice_arrows(focused, theme);
        let line = Line::from(vec![
            left,
            Span::raw(revision.commit.as_short_hash().to_string()).fg(theme.list_hash_fg),
            Span::raw(" "),
            Span::raw(format_date(&ctx, &revision.date)).fg(theme.list_date_fg),
            Span::raw(" "),
            Span::raw(revision.subject.clone()).fg(theme.list_subject_fg),
            right,
        ]);
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
        let dates: FxHashMap<&CommitHash, String> = self
            .authors
            .iter()
            .map(|(commit, c)| (commit, format_date(&ctx, &c.date)))
            .collect();
        let author_width = self.authors.values().map(|c| c.author.width()).max();
        let author_width = author_width.unwrap_or(0).min(AUTHOR_WIDTH);
        let date_width = dates.values().map(|d| d.width()).max().unwrap_or(0);
        let gutter_width = (7 + 1 + author_width + 1 + date_width) as u16;
        let [gutter, divider, text_area] = Layout::horizontal([
            Constraint::Length(gutter_width.min(inner.width / 2)),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .areas(inner);
        for y in divider.top()..divider.bottom() {
            buf.set_string(divider.x, y, "│", Style::default().fg(theme.divider_fg));
        }
        let search = self.query.search();
        let rows = self
            .text
            .render(text_area, buf, &ctx, focused, true, search);

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
            let (author, date) = match self.authors.get(commit) {
                Some(c) => (c.author.as_str(), dates[commit].as_str()),
                None => ("", ""),
            };
            // cut to its width, then padded to it
            let mut used = 0;
            let mut author: String = author
                .chars()
                .take_while(|c| {
                    used += c.width().unwrap_or(0);
                    used <= author_width
                })
                .collect();
            author += &" ".repeat(author_width - author.width());
            let line = Line::from(vec![
                Span::raw(commit.as_short_hash().to_string()).fg(theme.list_hash_fg),
                Span::raw(" "),
                Span::raw(author).fg(theme.list_name_fg),
                Span::raw(" "),
                Span::raw(date.to_string()).fg(theme.list_date_fg),
            ]);
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

        // from an older commit not changing the file, as of the last one changing it
        let view = BlameView::new(&repository, &c2, "a", None, test_ctx(), ec.sender()).unwrap();
        assert_eq!(shown(&view).0, c2);
    }

    // The arrows of a value cycled with left and right look the same in the blame and the popups.
    #[test]
    fn test_choice_arrows_alike_in_the_blame_and_the_popups() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        fs::write(path.join("a"), "one\n").unwrap();
        repo.add(&["a"]);
        repo.commit("c1", "2024-01-01");
        let c1 = CommitHash::from(repo.rev_parse_head().as_str());
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        let ec = EventController::without_terminal();
        let ctx = test_ctx();
        let theme = &ctx.color_theme;
        let mut view =
            BlameView::new(&repository, &c1, "a", None, ctx.clone(), ec.sender()).unwrap();
        let key = KeyEvent::new(KeyCode::Null, KeyModifiers::NONE);
        let arrow_fg = |buf: &Buffer| {
            let cell = buf.content.iter().find(|c| c.symbol() == "‹").unwrap();
            cell.fg
        };
        let blame_arrow = |view: &mut BlameView| {
            let mut terminal = Terminal::new(TestBackend::new(60, 8)).unwrap();
            let frame = terminal.draw(|f| view.render(f, f.area())).unwrap();
            arrow_fg(frame.buffer)
        };
        assert_eq!(blame_arrow(&mut view), theme.muted_fg);
        view.handle_event(UserEvent::FocusPanel(0), key, &repository);
        let selected = blame_arrow(&mut view);
        assert_eq!(selected, theme.highlight_fg);

        let target = CheckoutTarget {
            label: "main".into(),
            args: vec!["main".into()],
        };
        let dialog = GitDialog::checkout(vec![target]);
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        GitDialogWidget::new(&dialog, &ctx).render(area, &mut buf);
        assert_eq!(arrow_fg(&buf), selected);
    }
}
