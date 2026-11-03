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
        commit_list::{submodule_commit_list, CommitList, CommitListState},
        diff::Step,
        style::{placeholder, render_panel, render_panel_scrollbar, render_scrollbar},
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
    // None when there is nothing to list: not checked out, or its commits can't be read
    list: Option<CommitListState<'a>>,
    // why there is nothing to list
    message: Option<String>,
    // its commits loading on another thread, see `poll`
    loading: Option<Loading<'a>>,
}

// The commits of a submodule between two gitlinks loading on another thread: the range that has
// them (from..to, else to..from) with its repository and graph, None when neither has commits.
type Loaded = Result<Option<((String, String), Repository, graph::GraphLayout)>, String>;

#[derive(Debug)]
struct Loading<'a> {
    // the parent, which keeps the submodule's repository once loaded
    repository: &'a Repository,
    path: String,
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
        let mut panel = Self {
            key,
            uncommitted,
            list: None,
            message: None,
            loading: None,
        };
        let Some(to) = to else {
            panel.message = Some("submodule not checked out".into());
            return panel;
        };
        // nothing to load when the gitlink is unchanged: only the uncommitted files are new
        if from == to {
            panel.message = Some("no commits ahead of the recorded one".into());
            return panel;
        }
        // the commits between the two gitlinks, in the direction they differ: the checked-out
        // commit may be behind the recorded one, then its missing commits are listed
        let loaded = [(from, to), (to, from)]
            .into_iter()
            .find_map(|(from, to)| repository.loaded_submodule_repository(path, from, to));
        if let Some(submodule) = loaded {
            let graph = graph::calc_graph(submodule, None, &Default::default());
            panel.show(ctx, submodule, graph);
            return panel;
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
        match tx {
            Some(tx) => {
                let job = Job::spawn(tx.clone(), work);
                panel.loading = Some(Loading {
                    repository,
                    path,
                    job,
                });
            }
            None => panel.show_loaded(ctx, repository, &path, work()),
        }
        panel
    }

    // Before drawing: the commits loaded on another thread, once they are.
    pub fn poll(&mut self, ctx: &AppContext) {
        let Some(loaded) = self.loading.as_ref().and_then(|l| l.job.try_take()) else {
            return;
        };
        let Loading {
            repository, path, ..
        } = self.loading.take().expect("loading");
        self.show_loaded(ctx, repository, &path, loaded);
    }

    // The commits loading on another thread, waiting for them, before a key acts on them.
    pub fn wait(&mut self, ctx: &AppContext) {
        if let Some(Loading {
            repository,
            path,
            job,
        }) = self.loading.take()
        {
            self.show_loaded(ctx, repository, &path, job.wait());
        }
    }

    pub fn loading_since(&self) -> Option<Instant> {
        self.loading.as_ref().map(|loading| loading.job.started())
    }

    fn show_loaded(
        &mut self,
        ctx: &AppContext,
        repository: &'a Repository,
        path: &str,
        loaded: Loaded,
    ) {
        self.message = None;
        match loaded {
            Ok(Some(((from, to), submodule, layout))) => {
                let submodule = repository.keep_submodule_repository(path, &from, &to, submodule);
                self.show(ctx, submodule, graph::Graph::of(submodule, layout));
            }
            Ok(None) => self.message = Some("no commits ahead of the recorded one".into()),
            Err(err) => self.message = Some(err),
        }
    }

    fn show(&mut self, ctx: &AppContext, submodule: &'a Repository, graph: graph::Graph<'a>) {
        let manager = ctx.graph_image_manager(graph);
        self.list = Some(submodule_commit_list(
            submodule,
            manager,
            ctx.search_options.clone(),
            ctx.ui_config.common.scrolloff as usize,
        ));
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
        self.list
            .as_mut()
            .is_some_and(|list| list.handle_navigation(event))
    }

    // Moves the selected commit by `step`, for the movement keys of the page.
    pub fn step(&mut self, step: Step) {
        let Some(list) = &mut self.list else {
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
        if let Some(list) = &mut self.list {
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
        if let Some(list) = &mut self.list {
            list.update_height(height);
        }
    }

    pub fn drain_pending_graph_uploads(&mut self) -> Vec<String> {
        self.list
            .as_mut()
            .map_or_else(Vec::new, |list| list.drain_pending_graph_uploads())
    }

    #[cfg(test)]
    pub fn subjects(&self) -> Vec<&str> {
        self.list
            .as_ref()
            .map_or_else(Vec::new, |list| list.subjects())
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
        if let Some((total, offset)) = self.render_content(ctx, inner, buf) {
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
        if let Some((total, offset)) = self.render_content(ctx, area, buf) {
            render_scrollbar(buf, scrollbar, total, offset, focused, &ctx.color_theme);
        }
    }

    // Draws the note and the commits in `inner`, the list fitted to its height; the rows shown and
    // the first one, for the caller's scrollbar. None when there is nothing to scroll.
    fn render_content(
        &mut self,
        ctx: &Rc<AppContext>,
        inner: Rect,
        buf: &mut Buffer,
    ) -> Option<(usize, usize)> {
        let theme = &ctx.color_theme;
        let note_height = u16::from(self.uncommitted.is_some());
        let [note_area, list_area] =
            Layout::vertical([Constraint::Length(note_height), Constraint::Min(0)]).areas(inner);
        if let Some(uncommitted) = self.uncommitted {
            let note = match uncommitted {
                0 => "no uncommitted files".to_string(),
                1 => "1 uncommitted file".to_string(),
                n => format!("{n} uncommitted files"),
            };
            buf.set_line(
                note_area.x,
                note_area.y,
                &Line::raw(note).fg(theme.muted_fg),
                note_area.width,
            );
        }
        let Some(list) = &mut self.list else {
            let message = match &self.loading {
                Some(loading) if shows_loading(loading.job.started()) => "Loading…",
                Some(_) => "",
                None => self.message.as_deref().unwrap_or("no commits"),
            };
            Paragraph::new(placeholder(message, theme)).render(list_area, buf);
            return None;
        };
        list.update_height(list_area.height as usize);
        let (total, offset) = (list.len(), list.offset());
        StatefulWidget::render(CommitList::new(ctx.clone()), list_area, buf, list);
        Some((total, offset))
    }
}

// What a submodule panel shows: its path and the two gitlinks, so a page can reuse the panel when
// the same submodule is selected again.
pub fn submodule_key(path: &str, from: &str, to: Option<&str>) -> String {
    format!("{path}\0{from}\0{}", to.unwrap_or_default())
}
