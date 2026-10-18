use std::ops::RangeInclusive;

use ratatui::{buffer::Buffer, layout::Rect, text::Line};

use crate::{
    app::AppContext,
    event::UserEvent,
    git::PatchDirection,
    search::SearchOptions,
    widget::{
        diff::{
            diff_lines, render_diff_lines, DiffCursor, DiffHunk, DiffPart, ParsedDiff, Step,
            WrapMap,
        },
        style::{render_panel, render_scrollbar, ScrollState},
        tree_query::{highlight_match, SearchDirection},
    },
};

// The selection of a diff, kept across its reload, see `DiffPane::restore`.
#[derive(Debug, Clone, Copy)]
pub struct DiffSelection {
    focus: Option<DiffPart>,
    hunk: usize,
    line: usize,
}

// A diff beside what picks it: moving into it selects hunks, then lines, to copy, link to, apply or
// stage; the changes tab, the timeline and the local changes.
#[derive(Debug, Default)]
pub struct DiffPane {
    // loaded lazily, None until then
    diff: Option<ParsedDiff>,
    // the hunks or the lines while the diff has the focus, None while what is beside it does (the
    // files of the changes tab, the commits of the timeline)
    focus: Option<DiffPart>,
    cursor: DiffCursor,
}

impl DiffPane {
    // Shows another diff, from the top, the focus beside it.
    pub fn set_diff(&mut self, diff: Option<ParsedDiff>) {
        self.diff = diff;
        self.focus = None;
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
        if selection.focus.is_none() || !self.cursor.select_hunk(diff, selection.hunk) {
            return;
        }
        self.focus = Some(DiffPart::Hunk);
        if selection.focus == Some(DiffPart::Line)
            && self.cursor.select_line_near(diff, selection.line)
        {
            self.focus = Some(DiffPart::Line);
        }
    }

    // Moves the focus out of the diff, to what is beside it.
    pub fn leave(&mut self) {
        self.focus = None;
    }

    // The scroll of what is shown in place of the diff, like the sides of a conflict.
    pub fn scroll_state(&mut self) -> &mut ScrollState {
        &mut self.cursor.scroll
    }

    // Scrolls the diff by the scroll keys, or moves the selected line half a page or a page; returns
    // whether `event` was one.
    pub fn scroll(&mut self, event: UserEvent) -> bool {
        let jumped = match &self.diff {
            Some(diff) if self.focus == Some(DiffPart::Line) => self.cursor.jump_page(diff, event),
            _ => false,
        };
        jumped || self.cursor.scroll.handle_scroll(event)
    }

    pub fn is_focused(&self) -> bool {
        self.focus.is_some()
    }

    pub fn is_selecting_lines(&self) -> bool {
        self.focus == Some(DiffPart::Line)
    }

    // What `selected_text` is of: the hunk, until lines are selected.
    pub fn selected_part(&self) -> DiffPart {
        self.focus.unwrap_or(DiffPart::Hunk)
    }

    // The lines of the selected hunk or line in the file at the commit; None outside the diff, or
    // for lines only removed.
    pub fn selected_lines(&self) -> Option<RangeInclusive<usize>> {
        let diff = self.diff.as_ref().filter(|_| self.is_focused())?;
        let rows = self.cursor.highlighted(diff, self.selected_part());
        let mut lines = diff.rows.get(rows)?.iter().filter_map(|row| row.new);
        let first = lines.next()?;
        Some(first..=lines.next_back().unwrap_or(first))
    }

    // Goes one level deeper, like on the local changes page: into the hunks, then the lines of the
    // selected hunk. Returns false when the diff has no hunk.
    pub fn enter(&mut self) -> bool {
        let Some(diff) = &self.diff else {
            return false;
        };
        match self.focus {
            None if self.cursor.select_hunk(diff, 0) => self.focus = Some(DiffPart::Hunk),
            Some(DiffPart::Hunk) if self.cursor.enter_lines(diff) => {
                self.focus = Some(DiffPart::Line)
            }
            None => return false,
            _ => {}
        }
        true
    }

    // Goes one level back: lines -> hunks -> out of the diff. Returns false when already out.
    pub fn back(&mut self) -> bool {
        match self.focus {
            None => return false,
            Some(DiffPart::Hunk) => self.focus = None,
            Some(DiffPart::Line) => {
                self.focus = Some(DiffPart::Hunk);
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
            None => {}
            Some(DiffPart::Hunk) => self.cursor.move_hunk(diff, step),
            Some(DiffPart::Line) => self.cursor.move_line(diff, step),
        }
    }

    // The patch of the selected hunk or line, to apply in `direction`.
    pub fn selected_patch(&self, direction: PatchDirection) -> Option<String> {
        let diff = self.diff.as_ref()?;
        match self.focus {
            None => None,
            Some(DiffPart::Hunk) => self.cursor.selected_hunk(diff).map(DiffHunk::patch),
            Some(DiffPart::Line) => self.cursor.line_patch(diff, direction),
        }
    }

    // The selected line as in the file, or the selected hunk as a diff, to copy.
    pub fn selected_text(&self) -> Option<String> {
        let diff = self.diff.as_ref().filter(|_| self.is_focused())?;
        self.cursor.selected_text(diff, self.selected_part())
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
        let part = self.selected_part();
        let found = self.cursor.search(diff, query, direction, part, options);
        if found.is_some() {
            self.focus = found;
        }
        found.is_some()
    }

    // Draws the diff in a panel titled `title`, like the timeline and the local changes do: the
    // selection bar in the padding, the scrollbar on the right border. Returns the area inside.
    pub fn render_panel(
        &mut self,
        area: Rect,
        title: Line,
        buf: &mut Buffer,
        ctx: &AppContext,
        focused: bool,
        search: Option<&str>,
    ) -> Rect {
        let inner = render_panel(buf, area, title, focused, &ctx.color_theme);
        let scrollbar = Rect::new(area.right().saturating_sub(1), inner.y, 1, inner.height);
        self.render(inner, scrollbar, buf, ctx, focused, search);
        inner
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
        let wrap = ctx.wrap.get();
        let scrolloff = ctx.ui_config.common.scrolloff as usize;
        let Some(diff) = &self.diff else {
            return;
        };
        // the scroll counts screen rows while wrapping, see `WrapMap`; at another width or wrap
        // the rows are measured again, showing the selection again with them
        if !self.cursor.wrap().matches(area.width, wrap) {
            let map = WrapMap::of(diff, theme, area.width, wrap);
            self.cursor.set_wrap_map(map);
            if self.focus.is_some() {
                let rows = self.cursor.highlighted(diff, self.selected_part());
                self.cursor.scroll_to(rows);
            }
        }
        let total = self.cursor.wrap().total();
        let range = self
            .cursor
            .scroll
            .fit(total, area.height as usize, scrolloff);
        render_scrollbar(buf, scrollbar, total, range.start, focused, theme);
        // only the rows the screen range overlaps are built
        let rows = self.cursor.wrap().logical_rows(range.clone());
        let skip = range.start - self.cursor.wrap().visual_start(rows.start);
        let mut lines = diff_lines(diff, theme, rows.clone(), area.width);
        let highlighted = match self.focus {
            None => 0..0,
            Some(part) => self.cursor.highlighted(diff, part),
        };
        if let Some(query) = search {
            for line in lines.iter_mut() {
                highlight_match(line, query, ctx.search_options.get(), theme);
            }
        }
        render_diff_lines(
            rows.zip(lines),
            skip..skip + range.len(),
            highlighted,
            wrap,
            theme,
            area,
            buf,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        test_support::ui::{dump, test_ctx},
        widget::diff::parse_diff,
    };

    // A diff whose rows fit the panel unwrapped, so only their wrapping makes it scroll.
    fn long_line_diff() -> ParsedDiff {
        parse_diff(&format!(
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+{}\n",
            "x".repeat(80)
        ))
    }

    fn draw(pane: &mut DiffPane, ctx: &AppContext, area: Rect) -> Buffer {
        let mut buf = Buffer::empty(area);
        let scrollbar = Rect::new(area.right().saturating_sub(1), area.y, 1, area.height);
        pane.render(area, scrollbar, &mut buf, ctx, false, None);
        buf
    }

    #[test]
    fn test_wrap_scrolls_the_wrapped_rows() {
        let ctx = test_ctx();
        ctx.wrap.set(true);
        let area = Rect::new(0, 0, 20, 5);
        let mut pane = DiffPane::default();
        pane.set_diff(Some(long_line_diff()));

        let top = draw(&mut pane, &ctx, area);
        assert_eq!(pane.cursor.scroll.offset, 0);
        // the wrapped rows are more than the panel, so a page down moves the view
        assert!(pane.cursor.wrap().total() > area.height as usize);
        pane.scroll(UserEvent::PageDown);
        let scrolled = draw(&mut pane, &ctx, area);
        assert!(
            pane.cursor.scroll.offset > 0,
            "the wrapped diff didn't scroll"
        );
        assert_ne!(dump(&top), dump(&scrolled));

        // and back to the top
        pane.scroll(UserEvent::PageUp);
        draw(&mut pane, &ctx, area);
        assert_eq!(pane.cursor.scroll.offset, 0);
    }

    #[test]
    fn test_unwrapped_diff_does_not_scroll() {
        let ctx = test_ctx();
        let area = Rect::new(0, 0, 20, 5);
        let mut pane = DiffPane::default();
        pane.set_diff(Some(long_line_diff()));

        draw(&mut pane, &ctx, area);
        // the logical rows fit the panel, so there is nothing to scroll
        assert_eq!(pane.cursor.wrap().total(), 3);
        pane.scroll(UserEvent::PageDown);
        draw(&mut pane, &ctx, area);
        assert_eq!(pane.cursor.scroll.offset, 0);
    }
}
