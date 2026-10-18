// The git commands the app ran that change the repository, the newest first, and the output of
// the selected one under them, like lazygit's command log. See `git::command_log`.

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::Stylize,
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Widget},
};

use crate::{
    color::ColorTheme,
    event::UserEvent,
    git::{command_log, LoggedCommand},
    keys::Keys,
    widget::style::{
        mark_selected_row, panel_title, placeholder, render_panel, render_panel_scrollbar,
        HalfPage, ListCursor, ScrollState,
    },
};

#[derive(Debug, Default)]
pub struct CommandLogState {
    entries: Vec<LoggedCommand>,
    cursor: ListCursor,
    // the scroll of the output of the selected command
    output: ScrollState,
}

impl CommandLogState {
    pub fn new() -> Self {
        CommandLogState {
            entries: command_log(),
            ..Default::default()
        }
    }

    // Moves the selection by the movement keys, the output of the command selected shown from
    // the top; returns whether `event` was one.
    pub fn move_selection(&mut self, event: UserEvent) -> bool {
        let total = self.entries.len();
        let Some(index) = self.cursor.moved_to(event, total, HalfPage::Pane) else {
            return false;
        };
        self.cursor.select(index, self.entries.len());
        self.output.offset = 0;
        true
    }

    // The keys of the command log, see `Keys`.
    pub fn register_keys(k: &mut Keys<Self>) {
        use UserEvent::*;
        k.add(
            &[NavigateDown, NavigateUp, SelectDown, SelectUp],
            "move",
            "Select the next/previous command",
        );
        k.add(
            &[HalfPageDown, HalfPageUp],
            "scroll output",
            "Scroll the output of the selected command",
        );
        k.quiet(
            &[PageDown, PageUp, ScrollDown, ScrollUp],
            "Scroll the output of the selected command",
        );
        k.add(
            &[GoToTop, GoToBottom],
            "top/bottom",
            "Select the newest/oldest command",
        );
        k.add(
            &[ShortCopy, FullCopy],
            "copy",
            "Copy the command and its output",
        );
        k.add(&[Cancel, CommandLog], "close", "Close the command log");
    }

    // Scrolls the output by the scroll keys; returns whether `event` was one.
    pub fn scroll_output(&mut self, event: UserEvent) -> bool {
        self.output.handle_scroll(event)
    }

    // The command and its output, to copy.
    pub fn selected_text(&self) -> Option<String> {
        let entry = self.entries.get(self.cursor.selected())?;
        Some(format!("{}\n{}", entry.command, entry.output))
    }
}

pub struct CommandLog<'a> {
    theme: &'a ColorTheme,
    // see `scroll_to`
    scrolloff: usize,
}

impl<'a> CommandLog<'a> {
    pub fn new(theme: &'a ColorTheme, scrolloff: usize) -> Self {
        CommandLog { theme, scrolloff }
    }

    pub fn render(self, area: Rect, buf: &mut Buffer, state: &mut CommandLogState) {
        let theme = self.theme;
        Clear.render(area, buf);
        Block::default().bg(theme.bg).render(area, buf);
        // the commands over two fifths, the output of the selected one under them
        let [list_area, output_area] =
            Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area);

        let count = state.entries.len().to_string();
        let inner = render_panel(
            buf,
            list_area,
            panel_title("Command log", &count, true, theme),
            true,
            theme,
        );
        if state.entries.is_empty() {
            placeholder("No git command has changed the repository yet", theme).render(inner, buf);
        }
        let height = inner.height as usize;
        let total = state.entries.len();
        let offset = state.cursor.fit(height, total, self.scrolloff);
        render_panel_scrollbar(buf, list_area, total, offset, true, theme);
        for (row, entry) in state.entries.iter().enumerate().skip(offset).take(height) {
            let y = inner.y + (row - offset) as u16;
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
            if row == state.cursor.selected() {
                mark_selected_row(buf, inner, y, true, theme);
            }
        }

        let inner = render_panel(
            buf,
            output_area,
            panel_title("Output", "", false, theme),
            false,
            theme,
        );
        let output = state
            .entries
            .get(state.cursor.selected())
            .map_or("", |entry| entry.output.as_str());
        let lines: Vec<Line> = if output.is_empty() && !state.entries.is_empty() {
            vec![placeholder("No output", theme)]
        } else {
            output
                .lines()
                .map(|l| Line::raw(l.to_string()).fg(theme.fg))
                .collect()
        };
        let rows = state.output.fit(lines.len(), inner.height as usize, 0);
        Paragraph::new(lines)
            .scroll((rows.start as u16, 0))
            .render(inner, buf);
    }
}

fn format_duration(duration: std::time::Duration) -> String {
    match duration.as_millis() {
        ms if ms < 1000 => format!("{ms}ms"),
        ms => format!("{:.1}s", ms as f64 / 1000.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ui::{
        assert_fills_bg, assert_only_bg_changed, assert_row_bg, find_row, panel_row, theme_with_bg,
    };

    #[test]
    fn test_looks_like_the_other_popups() {
        let theme = theme_with_bg();
        let entry = |command: &str| LoggedCommand {
            at: chrono::Local::now(),
            command: command.into(),
            ok: true,
            output: "done".into(),
            duration: std::time::Duration::from_millis(5),
        };
        let mut state = CommandLogState {
            entries: vec![entry("git push"), entry("git pull")],
            ..Default::default()
        };
        let area = Rect::new(0, 0, 60, 12);
        let draw = |state: &mut CommandLogState| {
            let mut buf = Buffer::empty(area);
            CommandLog::new(&theme, 0).render(area, &mut buf, state);
            buf
        };
        state.move_selection(UserEvent::NavigateDown);
        let plain = draw(&mut state);
        state.move_selection(UserEvent::NavigateUp);
        let buf = draw(&mut state);
        // on the theme's background, but the selected row, a row like the other lists'
        assert_fills_bg(&buf, area, &theme, &[theme.list_selected_bg]);
        let y = find_row(&buf, "git push");
        let row = panel_row(&buf, "Command log", y, 1);
        assert_row_bg(&buf, row, theme.list_selected_bg);
        assert_only_bg_changed(&plain, &buf, row);
    }
}
