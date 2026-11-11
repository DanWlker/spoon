// The bars of the app: the pages at the top, and the status line at the bottom with the keys,
// what runs in the background, the message or the text typed.

use super::*;

impl App<'_> {
    // The pages at the top, the one shown as a pill; the local changes with their count.
    pub(super) fn render_page_bar(&self, f: &mut Frame, area: Rect) {
        let theme = &self.ctx.color_theme;
        let count = match &self.view {
            View::LocalChanges(view) => view.local_change_count(),
            _ => self.repository.loaded_local_change_count(),
        };
        let on_local_changes = !self.sidebar_shown();
        let pages = [
            (
                format!("Local Changes ({count})"),
                on_local_changes,
                theme.status_success_fg,
            ),
            (
                "Commit Graph".to_string(),
                !on_local_changes,
                theme.accent_fg,
            ),
        ];
        // a window over them, like the help, after them, with its file
        let window = self.shell.overlay.as_ref().map(Overlay::title);
        let pill = |name: String, color| {
            [
                Span::raw("\u{e0b6}").fg(color),
                badge(name, color, theme),
                Span::raw("\u{e0b4}").fg(color),
            ]
        };
        let mut spans = vec![Span::raw(" ")];
        for (name, shown, color) in pages {
            if shown && window.is_none() {
                spans.extend(pill(name, color));
            } else {
                spans.push(Span::raw(format!(" {name} ")).fg(theme.muted_fg));
            }
            spans.push(Span::raw("  "));
        }
        if let Some((name, path)) = window {
            spans.extend(pill(name.into(), theme.accent_fg));
            if let Some(path) = path {
                spans.push(Span::raw(format!(" {path}")).fg(theme.muted_fg));
            }
        }
        f.render_widget(Line::from(spans), area);
    }

    // What runs in the background, with a spinner in the status bar: the git command, else what
    // loads once it takes long (see `shows_loading`): the repository, the graph laid out again, or
    // the file history of a window.
    pub(super) fn background_activity(&self) -> Option<String> {
        if let Some(running) = self.ctx.running_task.lock().as_ref() {
            return Some(running.task.running.clone());
        }
        let shell = &self.shell;
        let reload = shell.reload.as_ref().map(|r| (r.started, "Reloading"));
        let regraph = shell.regraphs.first().map(|&s| (s, "Drawing the graph"));
        reload
            .into_iter()
            .chain(regraph)
            .find(|&(started, _)| shows_loading(started))
            .map(|(_, activity)| activity.into())
            .or_else(|| shell.opening.as_ref().and_then(Opening::activity))
    }

    // A bar after a neovim statusline: the message, or what runs in the background, or the
    // repository; on the right the keys of what has the focus and the position in the commit list.
    // The page is in the page bar.
    pub(super) fn render_status_line(&self, f: &mut Frame, area: Rect) {
        let theme = &self.ctx.color_theme;
        let bar = Style::default()
            .fg(theme.status_bar_fg)
            .bg(theme.status_bar_bg);
        let mut left = vec![Span::raw(" ")];
        let left_width: usize = left.iter().map(Span::width).sum();
        let mut right = vec![];

        match &self.shell.app_status.status_line {
            StatusLine::None => {
                // the keys of a key sequence typed so far
                let prefix = keys_to_string(&self.shell.app_status.pending_keys);
                if !prefix.is_empty() {
                    left.push(Span::raw(format!("{prefix} ")).fg(theme.accent_fg).bold());
                }
                // what runs in the background, else where the repository is
                match self.background_activity() {
                    Some(activity) => left.push(
                        Span::raw(format!("{activity} {}", spinner())).fg(theme.highlight_fg),
                    ),
                    None => {
                        // shortened like neovim's pathshorten when it would crowd the keys
                        let mut path = home_relative(self.repository.path());
                        if path.width() > area.width as usize / 4 {
                            path = shorten_path(&path);
                        }
                        left.push(Span::raw(format!("󰉋 {path}")).fg(theme.status_bar_fg));
                        // the checked-out branch, as the sidebar is on the graph only
                        let head = match self.repository.head() {
                            Head::Branch { name, .. } | Head::Unborn { name } => Some(name.clone()),
                            Head::Detached { target } => {
                                Some(format!("detached at {}", target.as_short_hash()))
                            }
                            Head::None => None,
                        };
                        if let Some(head) = head {
                            left.push(
                                Span::raw(format!("   \u{e725} {head}")).fg(theme.list_ref_head_fg),
                            );
                        }
                    }
                }
            }
            StatusLine::Input {
                message,
                cursor_byte_index,
                metadata,
            } => {
                let input = status_input_line(
                    message,
                    *cursor_byte_index,
                    &self.ctx.ui_config.common.cursor_type,
                    Style::default().fg(theme.status_bar_fg),
                    Style::default().fg(theme.virtual_cursor_fg),
                );
                left.extend(input.spans);
                right.push(Span::raw(format!("{metadata} ")).fg(theme.muted_fg));
            }
            StatusLine::Transient(msg) => {
                right.push(Span::raw(format!("{msg} ")).fg(theme.muted_fg));
            }
            StatusLine::SearchResult { outcome, options } => {
                let span = Span::raw(outcome.to_string());
                left.push(match outcome {
                    SearchOutcome::Found { .. } => span.fg(theme.status_info_fg),
                    SearchOutcome::NotFound { .. } => span.fg(theme.status_warn_fg).bold(),
                });
                right.push(Span::raw(format!("{options} ")).fg(theme.muted_fg));
            }
            StatusLine::NotificationSuccess(msg) => {
                left.push(
                    Span::raw(format!("✓ {msg}"))
                        .fg(theme.status_success_fg)
                        .bold(),
                );
            }
        }

        // with a message, what runs in the background goes to the right
        let message = !matches!(self.shell.app_status.status_line, StatusLine::None);
        if let Some(activity) = self.background_activity().filter(|_| message) {
            let text = format!("{activity} {} ", spinner());
            right.insert(0, Span::raw(text).fg(theme.highlight_fg));
        }
        // the selected commit and how many there are, like the line and column
        if let Some(state) = self.view.as_list_state() {
            let (selected, total) = state.position();
            right.push(
                Span::raw(format!("[{selected}/{total}] "))
                    .fg(theme.muted_fg)
                    .italic(),
            );
        }

        // the keys of what has the focus, as many as fit, before the rest of the right side
        let used: usize = left.iter().chain(&right).map(Span::width).sum();
        let available = (area.width as usize).saturating_sub(used + 2);
        let hints = self.fit_key_hints(available);
        if !hints.spans.is_empty() {
            let mut spans = hints.spans;
            spans.push(Span::raw(" "));
            right.splice(0..0, spans);
        }

        let line = status_bar_line(left, right, area.width);
        f.render_widget(Paragraph::new(line).style(bar), area);

        if let (
            StatusLine::Input {
                message,
                cursor_byte_index,
                ..
            },
            CursorType::Native,
        ) = (
            &self.shell.app_status.status_line,
            &self.ctx.ui_config.common.cursor_type,
        ) {
            let cursor_position = message[..*cursor_byte_index].width();
            // input wider than the bar is cut, the cursor kept on its last column
            let offset = u16::try_from(left_width + cursor_position).unwrap_or(u16::MAX);
            let x = area
                .x
                .saturating_add(offset)
                .min(area.right().saturating_sub(1));
            f.set_cursor_position((x, area.y));
        }
    }

    // The keys for the status bar (see `keys_are_local`), as many as fit in `width`, the first
    // ones being the most useful.
    pub(super) fn fit_key_hints(&self, width: usize) -> Line<'static> {
        let keys = self.shown_keys(if self.keys_are_local() {
            self.local_hints()
        } else {
            self.app_keys().hints()
        });
        let theme = &self.ctx.color_theme;
        // each hint and the " • " before it, see `key_hints`
        let mut used = 0;
        let fitting = keys
            .iter()
            .take_while(|hint| {
                let separator = if used == 0 { 0 } else { 3 };
                used += separator + key_hints(std::slice::from_ref(hint), theme).width();
                used <= width
            })
            .count();
        key_hints(&keys[..fitting], theme)
    }
}

// The path with the home directory as ~, like a shell prompt.
pub(super) fn home_relative(path: &std::path::Path) -> String {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    match home
        .as_deref()
        .and_then(|home| path.strip_prefix(home).ok())
    {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

// The directories of the path by their first letter, like "~/p/spoon".
pub(super) fn shorten_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    let last = parts.len().saturating_sub(1);
    parts
        .iter()
        .enumerate()
        .map(|(i, part)| match part.chars().next() {
            // hidden directories keep the letter after the dot
            Some('.') if i < last => part.chars().take(2).collect(),
            Some(c) if i < last => c.to_string(),
            _ => part.to_string(),
        })
        .collect::<Vec<_>>()
        .join("/")
}

// The typed `message`, with a virtual cursor inserted at `cursor_byte_index` rather than drawn over
// a character.
pub(super) fn status_input_line<'a>(
    message: &'a str,
    cursor_byte_index: usize,
    cursor_type: &'a CursorType,
    input_style: Style,
    cursor_style: Style,
) -> Line<'a> {
    match cursor_type {
        CursorType::Native => Line::from(Span::styled(message, input_style)),
        CursorType::Virtual(cursor) => {
            let (before, after) = message.split_at(cursor_byte_index);
            Line::from(vec![
                Span::styled(before, input_style),
                Span::styled(cursor.as_str(), cursor_style),
                Span::styled(after, input_style),
            ])
        }
    }
}

// The spans on the left, then those on the right, aligned to the right edge when they fit.
pub(super) fn status_bar_line<'a>(
    mut left: Vec<Span<'a>>,
    right: Vec<Span<'a>>,
    width: u16,
) -> Line<'a> {
    let left_width: usize = left.iter().map(Span::width).sum();
    let right_width: usize = right.iter().map(Span::width).sum();
    let width = width as usize;
    if left_width + right_width < width {
        left.push(Span::raw(" ".repeat(width - left_width - right_width)));
        left.extend(right);
    }
    Line::from(left)
}

#[cfg(test)]
mod tests {
    use ratatui::{backend::TestBackend, buffer::Buffer};
    use rstest::rstest;

    use super::*;

    #[test]
    fn test_shorten_path() {
        assert_eq!(shorten_path("~/projects/spoon"), "~/p/spoon");
        assert_eq!(shorten_path("/tmp/.cache/dr"), "/t/.c/dr");
        assert_eq!(shorten_path("~"), "~");
    }

    #[rstest]
    #[case("/", 1, "|", "/|")]
    #[case("/abc", 1, "|", "/|abc")]
    #[case("/abc", 2, "|", "/a|bc")]
    #[case("/abc", 4, "|", "/abc|")]
    #[case("/a界b", 2, "|", "/a|界b")]
    #[case("/a界b", 5, "|", "/a界|b")]
    #[case("/ae\u{301}b", 5, "|", "/ae\u{301}|b")]
    #[case("/a👩‍💻b", 13, "|", "/a👩‍💻|b")]
    #[case("/abc", 2, "||", "/a||bc")]
    #[case("/abc", 2, "界", "/a界bc")]
    #[case("/abc", 2, "", "/abc")]
    fn test_status_input_line_inserts_virtual_cursor_without_hiding_text(
        #[case] message: &str,
        #[case] cursor_byte_index: usize,
        #[case] cursor: &str,
        #[case] expected: &str,
    ) {
        let cursor_type = CursorType::Virtual(cursor.into());
        let mut terminal = Terminal::new(TestBackend::new(20, 1)).unwrap();
        terminal
            .draw(|frame| {
                let line = status_input_line(
                    message,
                    cursor_byte_index,
                    &cursor_type,
                    Style::default(),
                    Style::default(),
                );
                frame.render_widget(Paragraph::new(line), frame.area());
            })
            .unwrap();

        let mut expected_buffer = Buffer::with_lines([expected]);
        expected_buffer.resize(Rect::new(0, 0, 20, 1));
        assert_eq!(terminal.backend().buffer(), &expected_buffer);
    }

    #[test]
    fn test_status_bar_line_right_aligns_the_right_spans() {
        let line = status_bar_line(vec![Span::raw("left")], vec![Span::raw("[meta]")], 14);
        let texts: Vec<&str> = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(texts, ["left", "    ", "[meta]"]);

        // without room, the right spans are left out
        let line = status_bar_line(vec![Span::raw("left")], vec![Span::raw("[meta]")], 10);
        assert_eq!(line.spans.len(), 1);
    }
}
