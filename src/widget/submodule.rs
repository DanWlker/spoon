use std::{rc::Rc, time::Instant};

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::Stylize,
    text::Line,
    widgets::{Paragraph, StatefulWidget, Widget},
};

use crate::{
    app::AppContext,
    background::{shows_loading, Job},
    event::{Sender, UserEvent},
    git::Repository,
    graph,
    widget::{
        commit_list::{mark_commit_list_bar, submodule_commit_list, CommitList, CommitListState},
        diff::Step,
        style::{placeholder, render_panel, render_panel_scrollbar, render_scrollbar, tabs_shown},
    },
};

// A submodule shown in place of its gitlink diff: the commits the recorded gitlink doesn't have,
// drawn with the graph, and how many files are uncommitted inside it. Read-only: the pages scroll
// it, nothing else. The submodule's own repository is owned by the parent, so the commits and
// graph borrow it like the pages borrow the parent.
#[derive(Debug)]
pub struct SubmodulePanel<'a> {
    // what the panel shows, to reuse it when the same submodule is selected again: the path and
    // the two gitlinks
    key: String,
    // the files uncommitted inside the submodule; None in a commit's historical changes, where the
    // working tree says nothing
    uncommitted: Option<usize>,
    shows: Shows<'a>,
}

// What the panel shows under the note of the uncommitted files.
#[derive(Debug)]
enum Shows<'a> {
    // its commits loading on another thread, see `poll`
    Loading(Loading<'a>),
    // the commits the second gitlink has that the first doesn't, or, when `rewind`, those it lost
    // moving back, as `git diff --submodule=log` says
    Commits {
        list: Box<CommitListState<'a>>,
        rewind: bool,
    },
    // why there is nothing to list: not checked out, no commits between the gitlinks, or an error
    Message(String),
}

// The commits of a submodule between two gitlinks loading on another thread: the range that has
// them (from..to, else to..from) with its repository and graph, None when neither has commits.
type Loaded = Result<Option<((String, String), Repository, graph::GraphLayout)>, String>;

#[derive(Debug)]
struct Loading<'a> {
    // the parent, which keeps the submodule's repository once loaded
    repository: &'a Repository,
    path: String,
    // the two gitlinks compared, in order, see `shows_loaded`
    forward: (String, String),
    job: Job<Loaded>,
}

impl<'a> SubmodulePanel<'a> {
    // The same, between the two gitlinks a page compares, like the index and the submodule's HEAD
    // on the local changes page, or two commits of its history. Its commits load on another thread
    // with the app's `tx`, see `poll`; without it, as in the tests, right away.
    pub fn between(
        repository: &'a Repository,
        ctx: &AppContext,
        path: &str,
        from: &str,
        to: Option<&str>,
        uncommitted: Option<usize>,
        tx: Option<&Sender>,
    ) -> Self {
        let key = submodule_key(path, from, to);
        let panel = |shows| Self {
            key,
            uncommitted,
            shows,
        };
        let Some(to) = to else {
            return panel(Shows::Message("submodule not checked out".into()));
        };
        // nothing to load when the gitlink is unchanged: only the uncommitted files are new
        if from == to {
            return panel(Shows::Message(NO_COMMITS.into()));
        }
        // the commits between the two gitlinks, in the direction they differ: the checked-out
        // commit may be behind the recorded one, then its missing commits are listed
        let loaded =
            [(from, to), (to, from)]
                .into_iter()
                .enumerate()
                .find_map(|(i, (from, to))| {
                    Some((
                        repository.loaded_submodule_repository(path, from, to)?,
                        i == 1,
                    ))
                });
        if let Some((submodule, rewind)) = loaded {
            let graph = graph::calc_graph(submodule, None, &Default::default());
            return panel(commits(ctx, submodule, graph, rewind));
        }
        let ranges = [(from, to), (to, from)].map(|(from, to)| {
            let work = repository.submodule_repository_work(path, from, to);
            ((from.to_string(), to.to_string()), work)
        });
        let work = move || -> Loaded {
            for (range, work) in ranges {
                if let Some(submodule) = work()? {
                    let layout = graph::GraphLayout::calc(&submodule, None, &Default::default());
                    return Ok(Some((range, submodule, layout)));
                }
            }
            Ok(None)
        };
        let path = path.to_string();
        let forward = (from.to_string(), to.to_string());
        panel(match tx {
            Some(tx) => Shows::Loading(Loading {
                repository,
                path,
                forward,
                job: Job::spawn(tx.clone(), work),
            }),
            None => shows_loaded(ctx, repository, &path, &forward, work()),
        })
    }

    // Before drawing: the commits loaded on another thread, once they are.
    pub fn poll(&mut self, ctx: &AppContext) {
        let Shows::Loading(loading) = &self.shows else {
            return;
        };
        if let Some(done) = loading.job.try_take() {
            let (repository, path, forward) = (loading.repository, &loading.path, &loading.forward);
            self.shows = shows_loaded(ctx, repository, path, forward, done);
        }
    }

    // The commits loading on another thread, waiting for them, before a key acts on them.
    pub fn wait(&mut self, ctx: &AppContext) {
        let shows = std::mem::replace(&mut self.shows, Shows::Message(String::new()));
        self.shows = match shows {
            Shows::Loading(Loading {
                repository,
                path,
                forward,
                job,
            }) => shows_loaded(ctx, repository, &path, &forward, job.wait()),
            shows => shows,
        };
    }

    pub fn loading_since(&self) -> Option<Instant> {
        match &self.shows {
            Shows::Loading(loading) => Some(loading.job.started()),
            _ => None,
        }
    }

    // The commits listed, None when there are none to show.
    fn list_mut(&mut self) -> Option<&mut CommitListState<'a>> {
        match &mut self.shows {
            Shows::Commits { list, .. } => Some(list),
            _ => None,
        }
    }

    // What the panel shows, to reuse it for the same submodule.
    pub fn key(&self) -> &str {
        &self.key
    }

    // The files uncommitted inside the submodule, re-read when the panel is reused.
    pub fn set_uncommitted(&mut self, uncommitted: Option<usize>) {
        self.uncommitted = uncommitted;
    }

    // Moves by every key scrolling the commits: the movement keys, ctrl-d and ctrl-u, and the
    // page and line keys; false for other events or nothing to show.
    pub fn navigate(&mut self, event: UserEvent) -> bool {
        self.list_mut()
            .is_some_and(|list| list.handle_navigation(event))
    }

    // Moves the selected commit by `step`, for the movement keys of the page.
    pub fn step(&mut self, step: Step) {
        let Some(list) = self.list_mut() else {
            return;
        };
        match step {
            Step::Next => list.select_next(),
            Step::Prev => list.select_prev(),
            Step::First => list.select_first(),
            Step::Last => list.select_last(),
        }
    }

    pub fn ensure_visible_graph_uploaded(&mut self) {
        if let Some(list) = self.list_mut() {
            // the rows shown aren't known before the first draw, so upload all of them then
            if list.height() == 0 {
                list.ensure_all_graph_uploaded();
            } else {
                list.ensure_visible_graph_uploaded();
            }
        }
    }

    // The height the list is drawn in, set by the page before the graph images are prepared.
    pub fn set_list_height(&mut self, height: usize) {
        if let Some(list) = self.list_mut() {
            list.update_height(height);
        }
    }

    pub fn drain_pending_graph_uploads(&mut self) -> Vec<String> {
        self.list_mut()
            .map_or_else(Vec::new, |list| list.drain_pending_graph_uploads())
    }

    #[cfg(test)]
    pub fn subjects(&self) -> Vec<&str> {
        match &self.shows {
            Shows::Commits { list, .. } => list.subjects(),
            _ => vec![],
        }
    }

    #[cfg(test)]
    pub fn uncommitted(&self) -> Option<usize> {
        self.uncommitted
    }

    // Draws the panel with a border and `title` over `area`, like the panels beside the files: the
    // scrollbar on the border, the note and the commits inside.
    pub fn render_framed(
        &mut self,
        ctx: &Rc<AppContext>,
        area: Rect,
        buf: &mut Buffer,
        title: Line,
        focused: bool,
    ) {
        let theme = &ctx.color_theme;
        let inner = render_panel(buf, area, title, focused, theme);
        if let Some((total, offset)) = self.render_content(ctx, inner, buf, focused) {
            render_panel_scrollbar(buf, area, total, offset, focused, theme);
        }
    }

    // Draws the commits without a border, in the side of a commit's changes; the scrollbar in
    // `scrollbar`, one column beside `area`.
    pub fn render_bare(
        &mut self,
        ctx: &Rc<AppContext>,
        area: Rect,
        scrollbar: Rect,
        buf: &mut Buffer,
        focused: bool,
    ) {
        if let Some((total, offset)) = self.render_content(ctx, area, buf, focused) {
            render_scrollbar(buf, scrollbar, total, offset, focused, &ctx.color_theme);
        }
    }

    // Draws the note and the commits in `inner`, the list fitted to its height, its selection
    // marked like the commit list's; the rows shown and the first one, for the caller's
    // scrollbar. None when there is nothing to scroll.
    fn render_content(
        &mut self,
        ctx: &Rc<AppContext>,
        inner: Rect,
        buf: &mut Buffer,
        focused: bool,
    ) -> Option<(usize, usize)> {
        let theme = &ctx.color_theme;
        // the commits the gitlink moved by, then the files uncommitted inside it
        let count = |n: usize, what: &str| match n {
            1 => format!("1 {what}"),
            n => format!("{n} {what}s"),
        };
        let moved = match &self.shows {
            Shows::Commits { list, rewind } => Some(match rewind {
                false => format!("{} added", count(list.len(), "commit")),
                true => format!("{} removed, a rewind", count(list.len(), "commit")),
            }),
            _ => None,
        };
        let uncommitted = self.uncommitted.map(|n| match n {
            0 => "no uncommitted files".to_string(),
            n => count(n, "uncommitted file"),
        });
        let note: Vec<String> = moved.into_iter().chain(uncommitted).collect();
        let note_height = u16::from(!note.is_empty());
        let [note_area, list_area] =
            Layout::vertical([Constraint::Length(note_height), Constraint::Min(0)]).areas(inner);
        buf.set_line(
            note_area.x,
            note_area.y,
            &Line::raw(note.join(" · ")).fg(theme.muted_fg),
            note_area.width,
        );
        let list = match &mut self.shows {
            Shows::Commits { list, .. } => list,
            Shows::Loading(loading) => {
                // blank at first, so quick loading doesn't flash
                if shows_loading(loading.job.started()) {
                    Paragraph::new(placeholder("Loading…", theme)).render(list_area, buf);
                }
                return None;
            }
            Shows::Message(message) => {
                // git's errors take several lines
                let lines: Vec<Line> = message
                    .lines()
                    .map(|l| placeholder(tabs_shown(l), theme))
                    .collect();
                Paragraph::new(lines).render(list_area, buf);
                return None;
            }
        };
        list.update_height(list_area.height as usize);
        let (total, offset) = (list.len(), list.offset());
        StatefulWidget::render(CommitList::new(ctx.clone()), list_area, buf, &mut **list);
        mark_commit_list_bar(buf, list_area, list, focused, theme);
        Some((total, offset))
    }
}

// Said when the gitlinks are the same, or have no commits between them.
const NO_COMMITS: &str = "no commits ahead of the recorded one";

// The panel's commits from the work loading them, or why there are none; a rewind when they
// were found the other way round than `forward`, the two gitlinks compared.
fn shows_loaded<'a>(
    ctx: &AppContext,
    repository: &'a Repository,
    path: &str,
    forward: &(String, String),
    loaded: Loaded,
) -> Shows<'a> {
    match loaded {
        Ok(Some(((from, to), submodule, layout))) => {
            let rewind = (&from, &to) != (&forward.0, &forward.1);
            let submodule = repository.keep_submodule_repository(path, &from, &to, submodule);
            commits(ctx, submodule, graph::Graph::of(submodule, layout), rewind)
        }
        Ok(None) => Shows::Message(NO_COMMITS.into()),
        Err(err) => Shows::Message(err),
    }
}

fn commits<'a>(
    ctx: &AppContext,
    submodule: &'a Repository,
    graph: graph::Graph<'a>,
    rewind: bool,
) -> Shows<'a> {
    let manager = ctx.graph_image_manager(graph);
    let list = Box::new(submodule_commit_list(
        submodule,
        manager,
        ctx.search_options.clone(),
        ctx.ui_config.common.scrolloff as usize,
    ));
    Shows::Commits { list, rewind }
}

// What a submodule panel shows: its path and the two gitlinks, so a page can reuse the panel when
// the same submodule is selected again.
pub fn submodule_key(path: &str, from: &str, to: Option<&str>) -> String {
    format!("{path}\0{from}\0{}", to.unwrap_or_default())
}
