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
        ListCursor, ScrollState,
    },
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CommandLogFocus {
    #[default]
    Commands,
    Output,
}

#[derive(Debug, Default)]
pub struct CommandLogState {
    entries: Vec<LoggedCommand>,
    cursor: ListCursor,
    // the scroll of the output of the selected command
    output: ScrollState,
    focus: CommandLogFocus,
}

impl CommandLogState {
    pub fn new() -> Self {
        CommandLogState {
            entries: command_log(),
            ..Default::default()
        }
    }

    pub fn focus(&self) -> CommandLogFocus {
        self.focus
    }

    // Focuses the commands or the output, by the panel number.
    pub fn focus_panel(&mut self, panel: usize) {
        self.focus = match panel {
            1 => CommandLogFocus::Commands,
            2 => CommandLogFocus::Output,
            _ => return,
        };
    }

    // Moves by every key scrolling the commands while they have the focus, the output of the
    // command selected shown from the top; returns whether `event` was one.
    pub fn move_selection(&mut self, event: UserEvent) -> bool {
        if self.focus != CommandLogFocus::Commands {
            return false;
        }
        let total = self.entries.len();
        let mut cursor = self.cursor;
        if !cursor.navigate(event, total) {
            return false;
        }
        self.cursor = cursor;
        self.output.offset = 0;
        true
    }

    // The keys of the command log, see `Keys`.
    pub fn register_keys(k: &mut Keys<Self>) {
        use UserEvent::*;
        k.add(
            &[FocusPanel(1), FocusPanel(2)],
            "focus",
            "Focus the commands or the output",
        );
        k.add(
            &[NavigateDown, NavigateUp, SelectDown, SelectUp],
            "move",
            "Select the next/previous command",
        )
        .when(|s| s.focus() == CommandLogFocus::Commands);
        k.add(
            &[NavigateDown, NavigateUp, SelectDown, SelectUp],
            "move",
            "Move down/up",
        )
        .when(|s| s.focus() == CommandLogFocus::Output);
        k.add(&[GoToTop, GoToBottom], "top/bottom", "Go to the top/bottom");
        k.add(
            &[HalfPageDown, HalfPageUp],
            "half page",
            "Jump half a page down/up",
        );
        k.quiet(&[PageDown, PageUp], "Scroll a page down/up");
        k.quiet(&[ScrollDown, ScrollUp], "Scroll down/up");
        k.add(
            &[ShortCopy, FullCopy],
            "copy",
            "Copy the command and its output",
        );
        k.add(&[Cancel, CommandLog], "close", "Close the command log");
    }

    // Scrolls the output by the scroll keys while it has the focus; returns whether `event` was
    // one.
    pub fn scroll_output(&mut self, event: UserEvent) -> bool {
        self.focus == CommandLogFocus::Output && self.output.handle_event(event)
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
        let commands_focused = state.focus() == CommandLogFocus::Commands;
        let inner = render_panel(
            buf,
            list_area,
            panel_title("[1] Command log", [count], commands_focused, theme),
            commands_focused,
            theme,
        );
        if state.entries.is_empty() {
            placeholder("No git command has changed the repository yet", theme).render(inner, buf);
        }
        let height = inner.height as usize;
        let total = state.entries.len();
        let offset = state.cursor.fit(height, total, self.scrolloff);
        render_panel_scrollbar(buf, list_area, total, offset, commands_focused, theme);
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
            panel_title("[2] Output", [], !commands_focused, theme),
            !commands_focused,
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
        render_panel_scrollbar(
            buf,
            output_area,
            lines.len(),
            rows.start,
            !commands_focused,
            theme,
        );
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

    #[test]
    fn test_focused_panel_titled_like_the_others() {
        let theme = ColorTheme::default();
        let mut state = CommandLogState {
            entries: vec![LoggedCommand {
                at: chrono::Local::now(),
                command: "git push".into(),
                ok: true,
                output: "done".into(),
                duration: std::time::Duration::from_millis(5),
            }],
            ..Default::default()
        };
        let area = Rect::new(0, 0, 60, 12);
        let title = |state: &mut CommandLogState, label: &str| {
            let mut buf = Buffer::empty(area);
            CommandLog::new(&theme, 0).render(area, &mut buf, state);
            for y in 0..area.height {
                let row: String = (0..area.width).map(|x| buf[(x, y)].symbol()).collect();
                if let Some(x) = row.find(label) {
                    return buf[(x as u16, y)].style();
                }
            }
            panic!("{label} not drawn");
        };
        // the same title each draws focused, `panel_title`
        let focused = |label: &str, focused: bool| {
            let mut buf = Buffer::empty(Rect::new(0, 0, 30, 1));
            panel_title(label, [], focused, &theme).render(Rect::new(0, 0, 30, 1), &mut buf);
            buf[(1, 0)].style()
        };
        assert_eq!(
            title(&mut state, "[1] Command log"),
            focused("[1] Command log", true)
        );
        state.focus_panel(2);
        assert_eq!(
            title(&mut state, "[1] Command log"),
            focused("[1] Command log", false)
        );
        assert_eq!(title(&mut state, "[2] Output"), focused("[2] Output", true));
    }

    #[test]
    fn test_half_page_moves_the_commands_then_scrolls_the_output() {
        let entry = |command: &str| LoggedCommand {
            at: chrono::Local::now(),
            command: command.into(),
            ok: true,
            output: (0..40)
                .map(|i| format!("line {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
            duration: std::time::Duration::from_millis(5),
        };
        let entries: Vec<LoggedCommand> = (0..30).map(|i| entry(&format!("git cmd{i}"))).collect();
        let mut state = CommandLogState {
            entries,
            ..Default::default()
        };
        let area = Rect::new(0, 0, 60, 24);
        // drawn once: the half page counts the rows shown, of both panels
        let draw = |state: &mut CommandLogState| {
            let mut buf = Buffer::empty(area);
            CommandLog::new(&ColorTheme::default(), 0).render(area, &mut buf, state);
        };

        // the commands: ctrl-d jumps half a page in the list, the output shown from the top
        assert_eq!(state.focus(), CommandLogFocus::Commands);
        draw(&mut state);
        let first = state.cursor.selected();
        assert!(state.move_selection(UserEvent::HalfPageDown));
        assert!(state.cursor.selected() > first);
        assert_eq!(state.output.offset, 0);
        // the page and line keys move the commands too
        let jumped = state.cursor.selected();
        assert!(state.move_selection(UserEvent::PageDown));
        assert!(state.cursor.selected() > jumped);
        assert!(state.move_selection(UserEvent::ScrollDown));

        // the output: [2] scrolls it, the selection stays
        state.focus_panel(2);
        draw(&mut state);
        let selected = state.cursor.selected();
        assert!(!state.move_selection(UserEvent::HalfPageDown));
        assert!(state.scroll_output(UserEvent::HalfPageDown));
        assert!(state.output.offset > 0);
        assert_eq!(state.cursor.selected(), selected);
        // j and k scroll it a line at a time too
        let scrolled = state.output.offset;
        assert!(!state.move_selection(UserEvent::NavigateDown));
        assert!(state.scroll_output(UserEvent::NavigateDown));
        assert_eq!(state.output.offset, scrolled + 1);

        // the commands again: [1] moves the list
        state.focus_panel(1);
        assert!(state.move_selection(UserEvent::HalfPageDown));
    }
}
