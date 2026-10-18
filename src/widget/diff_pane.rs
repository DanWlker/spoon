use ratatui::{buffer::Buffer, layout::Rect};

use crate::{
    app::AppContext,
    event::UserEvent,
    search::SearchOptions,
    widget::{
        diff::{
            diff_line_count, diff_lines, render_diff_lines, DiffCursor, DiffHunk, ParsedDiff, Step,
        },
        style::{render_scrollbar, ScrollState},
        tree_query::{highlight_match, SearchDirection},
    },
};

// What has the focus around a diff: what is beside it (the files of the changes tab, the commits of
// the timeline), or the hunks or lines of the diff.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum DiffFocus {
    #[default]
    Outside,
    Hunks,
    Lines,
}

// A diff beside what picks it: moving into it selects hunks, then lines, to copy, link to, apply or
// stage; the changes tab, the timeline and the local changes.
// The selection of a diff, kept across its reload, see `DiffPane::restore`.
#[derive(Debug, Clone, Copy)]
pub struct DiffSelection {
    focus: DiffFocus,
    hunk: usize,
    line: usize,
}

#[derive(Debug, Default)]
pub struct DiffPane {
    // loaded lazily, None until then
    diff: Option<ParsedDiff>,
    focus: DiffFocus,
    cursor: DiffCursor,
}

impl DiffPane {
    // Shows another diff, from the top, the focus beside it.
    pub fn set_diff(&mut self, diff: Option<ParsedDiff>) {
        self.diff = diff;
        self.focus = DiffFocus::Outside;
        self.cursor.reset();
    }

    pub fn diff(&self) -> Option<&ParsedDiff> {
        self.diff.as_ref()
    }

    // What is selected, to select it again in the diff reloaded, see `restore`.
    pub fn selection(&self) -> DiffSelection {
        DiffSelection {
            focus: self.focus,
            hunk: self.cursor.hunk,
            line: self.cursor.line,
        }
    }

    // Selects again in the diff reloaded after staging part of it what `selection` was: the same
    // hunk, then the changed line at or after the same row, as the staged line itself is gone or
    // became context. Stays out of the diff when it has no hunk left.
    pub fn restore(&mut self, selection: DiffSelection) {
        let Some(diff) = &self.diff else {
            return;
        };
        if selection.focus == DiffFocus::Outside || !self.cursor.select_hunk(diff, selection.hunk) {
            return;
        }
        self.focus = DiffFocus::Hunks;
        if selection.focus == DiffFocus::Lines && self.cursor.select_line_near(diff, selection.line)
        {
            self.focus = DiffFocus::Lines;
        }
    }

    // Moves the focus out of the diff, to what is beside it.
    pub fn leave(&mut self) {
        self.focus = DiffFocus::Outside;
    }

    // The scroll of what is shown in place of the diff, like the sides of a conflict.
    pub fn scroll_state(&mut self) -> &mut ScrollState {
        &mut self.cursor.scroll
    }

    // Scrolls the diff by the scroll keys, or moves the selected line half a page; returns
    // whether `event` was one.
    pub fn scroll(&mut self, event: UserEvent) -> bool {
        let jumped = match &self.diff {
            Some(diff) if self.focus == DiffFocus::Lines => self.cursor.jump_half_page(diff, event),
            _ => false,
        };
        jumped || self.cursor.scroll.handle_scroll(event)
    }

    pub fn is_focused(&self) -> bool {
        self.focus != DiffFocus::Outside
    }

    pub fn is_selecting_lines(&self) -> bool {
        self.focus == DiffFocus::Lines
    }

    // The lines of the selected hunk or line in the file at the commit, (first, last); None
    // outside the diff, or for lines only removed.
    pub fn selected_lines(&self) -> Option<(usize, usize)> {
        let diff = self.diff.as_ref().filter(|_| self.is_focused())?;
        let rows = self.cursor.highlighted(diff, self.is_selecting_lines());
        let mut lines = diff.rows.get(rows)?.iter().filter_map(|row| row.new);
        let first = lines.next()?;
        Some((first, lines.next_back().unwrap_or(first)))
    }

    // Goes one level deeper, like on the local changes page: into the hunks, then the lines of the
    // selected hunk. Returns false when the diff has no hunk.
    pub fn enter(&mut self) -> bool {
        let Some(diff) = &self.diff else {
            return false;
        };
        match self.focus {
            DiffFocus::Outside if self.cursor.select_hunk(diff, 0) => self.focus = DiffFocus::Hunks,
            DiffFocus::Hunks if self.cursor.enter_lines(diff) => self.focus = DiffFocus::Lines,
            DiffFocus::Outside => return false,
            _ => {}
        }
        true
    }

    // Goes one level back: lines -> hunks -> out of the diff. Returns false when already out.
    pub fn back(&mut self) -> bool {
        match self.focus {
            DiffFocus::Outside => return false,
            DiffFocus::Hunks => self.focus = DiffFocus::Outside,
            DiffFocus::Lines => {
                self.focus = DiffFocus::Hunks;
                if let Some(diff) = &self.diff {
                    self.cursor.leave_lines(diff);
                }
            }
        }
        true
    }

    // Moves the selected hunk or line.
    pub fn step(&mut self, step: Step) {
        let Some(diff) = &self.diff else {
            return;
        };
        match self.focus {
            DiffFocus::Outside => {}
            DiffFocus::Hunks => self.cursor.move_hunk(diff, step),
            DiffFocus::Lines => self.cursor.move_line(diff, step),
        }
    }

    // The patch of the selected hunk or line, see `DiffHunk::partial_patch` for `reverse`.
    pub fn selected_patch(&self, reverse: bool) -> Option<String> {
        let diff = self.diff.as_ref()?;
        match self.focus {
            DiffFocus::Outside => None,
            DiffFocus::Hunks => self.cursor.selected_hunk(diff).map(DiffHunk::patch),
            DiffFocus::Lines => self.cursor.line_patch(diff, reverse),
        }
    }

    // The selected line as in the file, or the selected hunk as a diff, to copy.
    pub fn selected_text(&self) -> Option<String> {
        let diff = self.diff.as_ref().filter(|_| self.is_focused())?;
        self.cursor.selected_text(diff, self.is_selecting_lines())
    }

    // Selects a row matching `query`, see `DiffCursor::search`. Returns false when nothing matches.
    pub fn search(
        &mut self,
        query: &str,
        direction: SearchDirection,
        options: SearchOptions,
    ) -> bool {
        let Some(diff) = &self.diff else {
            return false;
        };
        let lines = self.is_selecting_lines();
        match self.cursor.search(diff, query, direction, lines, options) {
            Some(changed) => {
                self.focus = if changed {
                    DiffFocus::Lines
                } else {
                    DiffFocus::Hunks
                };
                true
            }
            None => false,
        }
    }

    // Draws the diff, its selected hunk or line highlighted while it has the focus, and the
    // scrollbar in the `scrollbar` column, on the right border.
    pub fn render(
        &mut self,
        area: Rect,
        scrollbar: Rect,
        buf: &mut Buffer,
        ctx: &AppContext,
        focused: bool,
        search: Option<&str>,
    ) {
        let theme = &ctx.color_theme;
        let count = self.diff.as_ref().map_or(0, diff_line_count);
        let range = self.cursor.scroll.fit(count, area.height as usize);
        let Some(diff) = &self.diff else {
            return;
        };
        render_scrollbar(buf, scrollbar, count, range.start, focused, theme);
        let mut lines = diff_lines(diff, theme, range.clone(), area.width);
        let highlighted = match self.focus {
            DiffFocus::Outside => 0..0,
            focus => self.cursor.highlighted(diff, focus == DiffFocus::Lines),
        };
        if let Some(query) = search {
            for line in lines.iter_mut() {
                highlight_match(line, query, ctx.search_options.get(), theme);
            }
        }
        render_diff_lines(lines, range, highlighted, ctx.wrap.get(), theme, area, buf);
    }
}
