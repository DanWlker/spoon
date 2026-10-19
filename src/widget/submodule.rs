use std::rc::Rc;

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::Stylize,
    text::Line,
    widgets::{Paragraph, StatefulWidget, Widget},
};

use crate::{
    app::AppContext,
    event::UserEvent,
    git::Repository,
    graph,
    widget::{
        commit_list::{submodule_commit_list, CommitList, CommitListState},
        style::{placeholder, render_panel, render_panel_scrollbar, render_scrollbar},
    },
};

// A submodule shown in place of its gitlink diff: the commits the recorded gitlink doesn't have,
// drawn with the graph, and how many files are uncommitted inside it. Read-only: the pages scroll
// it, nothing else. The submodule's own repository is owned by the parent, so the commits and
// graph borrow it like the pages borrow the parent.
#[derive(Debug)]
pub struct SubmodulePanel<'a> {
    // the files uncommitted inside the submodule; None in a commit's historical changes, where the
    // working tree says nothing
    uncommitted: Option<usize>,
    // None when there is nothing to list: not checked out, or its commits can't be read
    list: Option<CommitListState<'a>>,
    // why there is nothing to list
    message: Option<String>,
}

impl<'a> SubmodulePanel<'a> {
    // The panel for the local changes: the commits between the gitlink the index records and the
    // submodule's HEAD. None when `path` isn't a submodule.
    pub fn changes(repository: &'a Repository, ctx: &AppContext, path: &str) -> Option<Self> {
        let info = repository.submodule_info(path)?;
        Some(Self::load(
            repository,
            ctx,
            path,
            &info.recorded,
            info.head.as_deref(),
            Some(info.uncommitted),
        ))
    }

    // The panel for a commit's change of `path`, between two recorded gitlinks. None when `path`
    // isn't a submodule.
    pub fn between(
        repository: &'a Repository,
        ctx: &AppContext,
        path: &str,
        from: &str,
        to: &str,
    ) -> Option<Self> {
        if !repository.is_submodule(path) {
            return None;
        }
        Some(Self::load(repository, ctx, path, from, Some(to), None))
    }

    fn load(
        repository: &'a Repository,
        ctx: &AppContext,
        path: &str,
        from: &str,
        to: Option<&str>,
        uncommitted: Option<usize>,
    ) -> Self {
        let Some(to) = to else {
            return Self {
                uncommitted,
                list: None,
                message: Some("submodule not checked out".into()),
            };
        };
        // nothing to load when the gitlink is unchanged: only the uncommitted files are new
        if from == to {
            return Self {
                uncommitted,
                list: None,
                message: Some("no commits ahead of the recorded one".into()),
            };
        }
        match repository.submodule_repository(path, from, to) {
            Ok(submodule) => {
                let graph = graph::calc_graph(submodule, None, &Default::default());
                let manager = ctx.graph_image_manager(graph);
                let list = submodule_commit_list(
                    submodule,
                    manager,
                    ctx.search_options.clone(),
                    ctx.ui_config.common.scrolloff as usize,
                );
                Self {
                    uncommitted,
                    list: Some(list),
                    message: None,
                }
            }
            Err(err) => Self {
                uncommitted,
                list: None,
                message: Some(err),
            },
        }
    }

    // Scrolls the commits by the scroll keys; false for other events or nothing to scroll.
    pub fn scroll(&mut self, event: UserEvent) -> bool {
        self.list.as_mut().is_some_and(|list| list.scroll(event))
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

    pub fn drain_pending_graph_uploads(&mut self) -> Vec<String> {
        self.list
            .as_mut()
            .map_or_else(Vec::new, |list| list.drain_pending_graph_uploads())
    }

    pub fn graph_image_ids_sorted(&self) -> Vec<u32> {
        self.list
            .as_ref()
            .map_or_else(Vec::new, |list| list.graph_image_ids_sorted())
    }

    #[cfg(test)]
    pub fn subjects(&self) -> Vec<&str> {
        self.list
            .as_ref()
            .map_or_else(Vec::new, |list| list.subjects())
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
            let message = self.message.as_deref().unwrap_or("no commits");
            Paragraph::new(placeholder(message, theme)).render(list_area, buf);
            return None;
        };
        list.update_height(list_area.height as usize);
        let (total, offset) = (list.len(), list.offset());
        StatefulWidget::render(CommitList::new(ctx.clone()), list_area, buf, list);
        Some((total, offset))
    }
}
