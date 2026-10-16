// The git commands the app ran that change the repository, the newest first, and the output of
// the selected one under them, like lazygit's command log. See `git::command_log`.

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::Stylize,
    text::{Line, Span},
    widgets::{Clear, Padding, Paragraph, Widget},
};

use crate::{
    color::ColorTheme,
    git::{command_log, LoggedCommand},
    widget::style::{mark_selected_row, panel_block, panel_title, render_panel_scrollbar},
};

#[derive(Debug, Default)]
pub struct CommandLogState {
    entries: Vec<LoggedCommand>,
    selected: usize,
    offset: usize,
    // the scroll of the output of the selected command
    output_offset: usize,
    output_height: usize,
}

impl CommandLogState {
    pub fn new() -> Self {
        CommandLogState {
            entries: command_log(),
            ..Default::default()
        }
    }

    pub fn select_next(&mut self) {
        self.select(self.selected + 1);
    }

    pub fn select_prev(&mut self) {
        self.select(self.selected.saturating_sub(1));
    }

    pub fn select_first(&mut self) {
        self.select(0);
    }

    pub fn select_last(&mut self) {
        self.select(self.entries.len().saturating_sub(1));
    }

    fn select(&mut self, index: usize) {
        self.selected = index.min(self.entries.len().saturating_sub(1));
        self.output_offset = 0;
    }

    pub fn scroll_output_down(&mut self, n: usize) {
        let lines = self.selected_output().lines().count();
        self.output_offset = (self.output_offset + n).min(lines.saturating_sub(self.output_height));
    }

    pub fn scroll_output_up(&mut self, n: usize) {
        self.output_offset = self.output_offset.saturating_sub(n);
    }

    pub fn output_page(&self) -> usize {
        self.output_height.max(1)
    }

    // The command and its output, to copy.
    pub fn selected_text(&self) -> Option<String> {
        let entry = self.entries.get(self.selected)?;
        Some(format!("{}\n{}", entry.command, entry.output))
    }

    fn selected_output(&self) -> &str {
        self.entries
            .get(self.selected)
            .map_or("", |entry| entry.output.as_str())
    }
}

pub struct CommandLog<'a> {
    theme: &'a ColorTheme,
}

impl<'a> CommandLog<'a> {
    pub fn new(theme: &'a ColorTheme) -> Self {
        CommandLog { theme }
    }

    pub fn render(self, area: Rect, buf: &mut Buffer, state: &mut CommandLogState) {
        let theme = self.theme;
        Clear.render(area, buf);
        // the commands over two fifths, the output of the selected one under them
        let [list_area, output_area] =
            Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area);

        let count = state.entries.len().to_string();
        let block = panel_block(true, theme)
            .title(panel_title("Command log", &count, true, theme))
            .padding(Padding::horizontal(1));
        let inner = block.inner(list_area);
        block.render(list_area, buf);
        if state.entries.is_empty() {
            let text = "No git command has changed the repository yet";
            Line::raw(text)
                .fg(theme.status_input_transient_fg)
                .render(inner, buf);
        }
        let height = inner.height as usize;
        if state.selected < state.offset {
            state.offset = state.selected;
        } else if state.selected >= state.offset + height {
            state.offset = state.selected + 1 - height;
        }
        render_panel_scrollbar(buf, list_area, state.entries.len(), state.offset, theme);
        for (row, entry) in state
            .entries
            .iter()
            .enumerate()
            .skip(state.offset)
            .take(height)
        {
            let y = inner.y + (row - state.offset) as u16;
            let (mark, color) = if entry.ok {
                ("✓", theme.status_success_fg)
            } else {
                ("✗", theme.status_error_fg)
            };
            let line = Line::from(vec![
                Span::raw(format!("{mark} ")).fg(color),
                Span::raw(entry.at.format("%H:%M:%S ").to_string()).fg(theme.detail_label_fg),
                Span::raw(format!("{:>6} ", format_duration(entry.duration)))
                    .fg(theme.detail_label_fg),
                Span::raw(entry.command.clone()).fg(if entry.ok { theme.fg } else { color }),
            ]);
            line.render(Rect::new(inner.x, y, inner.width, 1), buf);
            if row == state.selected {
                mark_selected_row(buf, inner, y, true, theme);
            }
        }

        let block = panel_block(false, theme)
            .title(panel_title("Output", "", false, theme))
            .padding(Padding::horizontal(1));
        let inner = block.inner(output_area);
        block.render(output_area, buf);
        state.output_height = inner.height as usize;
        let output = state.selected_output();
        let lines: Vec<Line> = if output.is_empty() && !state.entries.is_empty() {
            vec![Line::raw("No output").fg(theme.status_input_transient_fg)]
        } else {
            output
                .lines()
                .map(|l| Line::raw(l.to_string()).fg(theme.fg))
                .collect()
        };
        Paragraph::new(lines)
            .scroll((state.output_offset as u16, 0))
            .render(inner, buf);
    }
}

fn format_duration(duration: std::time::Duration) -> String {
    match duration.as_millis() {
        ms if ms < 1000 => format!("{ms}ms"),
        ms => format!("{:.1}s", ms as f64 / 1000.0),
    }
}
