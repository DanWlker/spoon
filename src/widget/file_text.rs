use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Style, Stylize},
    text::{Line, Span},
};

use crate::{
    app::AppContext,
    event::UserEvent,
    search::SearchOptions,
    widget::{
        diff::{render_visual_rows, visual_rows, DiffCursor, Step},
        style::{half_page_jump, render_scrollbar},
        tree_query::{find_around, highlight_match, match_range, SearchDirection},
    },
};

// The lines of a file at a commit, with a selected line moving in them and a search: the preview of
// the files tab, and the text of the blame.
#[derive(Debug, Default)]
pub struct FileText {
    lines: Vec<String>,
    cursor: DiffCursor,
    // the line the search last matched, where the next one starts from
    search_row: Option<usize>,
}

impl FileText {
    // Other lines, from the top; the height they were last shown in is kept.
    pub fn set_lines(&mut self, lines: Vec<String>) {
        self.lines = lines;
        self.cursor.reset();
        self.search_row = None;
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    // Selects the first line shown, to move from there.
    pub fn select_shown(&mut self) {
        self.cursor.line = self.cursor.scroll.offset;
    }

    pub fn step(&mut self, step: Step) {
        let Some(last) = self.lines.len().checked_sub(1) else {
            return;
        };
        let line = match step {
            Step::Next => self.cursor.line + 1,
            Step::Prev => self.cursor.line.saturating_sub(1),
            Step::First => 0,
            Step::Last => last,
        };
        self.select_line(line.min(last));
    }

    // Scrolls by the scroll keys, or moves the selected line half a page; returns whether `event`
    // was one.
    pub fn scroll(&mut self, event: UserEvent) -> bool {
        let height = self.cursor.scroll.height;
        match half_page_jump(event, self.cursor.line, height, self.lines.len()) {
            Some(line) => {
                self.select_line(line);
                true
            }
            None => self.cursor.scroll.handle_scroll(event),
        }
    }

    pub fn select_line(&mut self, line: usize) {
        self.cursor.line = line;
        self.cursor.scroll_to(line..line + 1);
        self.search_row = None;
    }

    // The selected line, from 0.
    pub fn selected_line(&self) -> usize {
        self.cursor.line
    }

    // The selected line as in the file, to copy.
    pub fn selected_text(&self) -> Option<String> {
        self.lines.get(self.cursor.line).cloned()
    }

    // Selects the line matching `query`, around from the last match; false when nothing matches.
    pub fn search(
        &mut self,
        query: &str,
        direction: SearchDirection,
        options: SearchOptions,
    ) -> bool {
        let lines = &self.lines;
        let start = self.search_row.unwrap_or(self.cursor.line);
        let found = find_around(lines.len(), start, direction, |i| {
            match_range(&lines[i], query, options).is_some()
        });
        let Some(line) = found else {
            return false;
        };
        self.select_line(line);
        self.search_row = Some(line);
        true
    }

    // Draws the lines with their numbers, the selected one highlighted when `selected`, and the
    // scrollbar on the column right of `area`; returns the line shown on each row of `area`, which
    // differ from the rows when long lines are wrapped.
    pub fn render(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        ctx: &AppContext,
        focused: bool,
        selected: bool,
        search: Option<&str>,
    ) -> Vec<usize> {
        let theme = &ctx.color_theme;
        let lines = &self.lines;
        // only the visible lines are built, files can be long
        let width = lines.len().to_string().len();
        let range = self.cursor.scroll.fit(lines.len(), area.height as usize);
        let border = Rect::new(area.right(), area.y, 1, area.height);
        render_scrollbar(buf, border, lines.len(), range.start, focused, theme);
        let highlight_style = Style::default().bg(theme.list_selected_bg);
        let selected = if selected {
            self.cursor.line..self.cursor.line + 1
        } else {
            0..0
        };
        let visible = lines[range.start.min(lines.len())..]
            .iter()
            .zip(range)
            .map(|(line, i)| {
                let mut line = Line::from(vec![
                    Span::raw(format!("{:>width$} ", i + 1)).fg(theme.muted_fg),
                    Span::raw(line.replace('\t', "    ")),
                ]);
                if let Some(query) = search {
                    highlight_match(&mut line, query, ctx.search_options.get(), theme);
                }
                if selected.contains(&i) {
                    line.style = line.style.patch(highlight_style);
                }
                (i, line)
            });
        let visual = visual_rows(visible, ctx.wrap.get(), &selected, area);
        let rows = visual.iter().map(|(row, _)| *row).collect();
        render_visual_rows(visual, &selected, theme, area, buf);
        rows
    }
}
