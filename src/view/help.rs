use std::rc::Rc;

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Stylize},
    text::Line,
    widgets::{Block, Padding, Paragraph},
    Frame,
};

use crate::{
    app::{App, AppContext},
    color::ColorTheme,
    event::{AppEvent, Sender, UserEvent},
    keybind::KeyBind,
    keys::Keys,
    view::{detail::DetailView, list::ListView, register_typing_keys, BlameView, TimelineView},
    widget::{
        command_log::CommandLogState,
        git_dialog::GitDialog,
        local_changes::LocalChangesState,
        style::{key_span, ScrollState},
    },
};

// The keys of the whole app, over the page, see `AppShell::help`.
#[derive(Debug)]
pub struct HelpView {
    help_key_lines: Vec<Line<'static>>,
    help_value_lines: Vec<Line<'static>>,
    help_key_line_max_width: u16,

    scroll: ScrollState,

    tx: Sender,
}

impl HelpView {
    pub fn new(ctx: Rc<AppContext>, tx: Sender) -> HelpView {
        let (help_key_lines, help_value_lines) = build_lines(&ctx.color_theme, &ctx.keybind);
        let help_key_line_max_width = help_key_lines
            .iter()
            .map(|line| line.width())
            .max()
            .unwrap_or_default() as u16;
        HelpView {
            help_key_lines,
            help_value_lines,
            help_key_line_max_width,
            scroll: ScrollState::default(),
            tx,
        }
    }

    // The keys of the help, see `Keys`.
    pub fn register_keys(k: &mut Keys<Self>) {
        use UserEvent::*;
        k.add(&[HelpToggle, Cancel], "close", "Close help");
        k.quiet(
            &[NavigateDown, SelectDown, NavigateUp, SelectUp],
            "Scroll down/up",
        );
        k.quiet(&[PageDown, PageUp], "Scroll a page down/up");
        k.quiet(&[HalfPageDown, HalfPageUp], "Scroll half a page down/up");
        k.quiet(&[ScrollDown, ScrollUp], "Scroll down/up");
        k.quiet(&[GoToTop, GoToBottom], "Go to the top/bottom");
    }

    pub fn handle_event(&mut self, event: UserEvent) {
        match event {
            UserEvent::HelpToggle | UserEvent::Cancel => {
                self.tx.send(AppEvent::CloseOverlay);
            }
            // the help has nothing to select, so these scroll too
            UserEvent::SelectDown => {
                self.scroll.scroll(1);
            }
            UserEvent::SelectUp => {
                self.scroll.scroll(-1);
            }
            _ => {
                self.scroll.handle_event(event);
            }
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        self.update_state(area);

        let [mut key_area, mut value_area] =
            Layout::horizontal([Constraint::Percentage(30), Constraint::Percentage(70)])
                .areas(area);

        if key_area.width - 4 /* padding */ < self.help_key_line_max_width {
            [key_area, value_area] = Layout::horizontal([
                Constraint::Length(self.help_key_line_max_width + 4),
                Constraint::Min(0),
            ])
            .areas(area);
        }

        let key_lines: Vec<Line> = self
            .help_key_lines
            .iter()
            .skip(self.scroll.offset)
            .take(area.height as usize)
            .cloned()
            .collect();
        let value_lines: Vec<Line> = self
            .help_value_lines
            .iter()
            .skip(self.scroll.offset)
            .take(area.height as usize)
            .cloned()
            .collect();

        let key_paragraph = Paragraph::new(key_lines)
            .block(Block::default().padding(Padding::new(3, 1, 0, 0)))
            .right_aligned();
        let value_paragraph = Paragraph::new(value_lines)
            .block(Block::default().padding(Padding::new(1, 3, 0, 0)))
            .left_aligned();

        f.render_widget(key_paragraph, key_area);
        f.render_widget(value_paragraph, value_area);
    }
}

impl HelpView {
    fn update_state(&mut self, area: Rect) {
        self.scroll
            .fit(self.help_key_lines.len(), area.height as usize, 0);
    }
}

// The keys of every part of the app, as each registers them (see `Keys`), with their sentences.
fn build_lines(
    color_theme: &ColorTheme,
    keybind: &KeyBind,
) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
    let (keys, values): (Vec<_>, Vec<_>) = sections()
        .into_iter()
        .map(|(title, helps)| build_block_lines(title, helps, color_theme, keybind))
        .unzip();
    (
        join_line_groups_with_empty(keys),
        join_line_groups_with_empty(values),
    )
}

type Section = (&'static str, Vec<(Vec<UserEvent>, &'static str)>);

// Each part of the app with all the keys it registers, whatever its state.
fn sections() -> Vec<Section> {
    fn section<T: ?Sized>(
        register: impl FnOnce(&mut Keys<T>),
    ) -> Vec<(Vec<UserEvent>, &'static str)> {
        let mut keys = Keys::all();
        register(&mut keys);
        keys.list().help()
    }
    vec![
        ("Common:", section(App::register_app_keys)),
        ("Search and filter:", section(register_typing_keys)),
        ("Help:", section(HelpView::register_keys)),
        ("Commit List:", section(ListView::register_keys)),
        ("Commit Detail:", section(DetailView::register_keys)),
        ("Refs Sidebar:", section(App::register_sidebar_keys)),
        ("Local Changes:", section(LocalChangesState::register_keys)),
        ("Blame:", section(BlameView::register_keys)),
        ("Timeline:", section(TimelineView::register_keys)),
        ("Command Log:", section(CommandLogState::register_keys)),
        ("Git Command Popups:", section(GitDialog::register_keys)),
    ]
}

fn build_block_lines(
    title: &'static str,
    helps: Vec<(Vec<UserEvent>, &'static str)>,
    color_theme: &ColorTheme,
    keybind: &KeyBind,
) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
    // the keys bound to nothing aren't listed
    let helps: Vec<_> = helps
        .into_iter()
        .filter(|(events, _)| !keybind.all_keys(events).is_empty())
        .collect();
    let mut key_lines = Vec::new();
    let mut value_lines = Vec::new();

    let key_title_lines = vec![Line::from(title)
        .fg(color_theme.help_block_title_fg)
        .add_modifier(Modifier::BOLD)];
    let value_title_lines = vec![Line::from("")];
    let key_binding_lines: Vec<Line> = helps
        .iter()
        .map(|(events, _)| Line::from(key_span(keybind.all_keys(events), color_theme)))
        .collect();
    let value_binding_lines: Vec<Line> = helps
        .into_iter()
        .map(|(_, value)| Line::raw(value))
        .collect();

    key_lines.extend(key_title_lines);
    key_lines.extend(key_binding_lines);
    value_lines.extend(value_title_lines);
    value_lines.extend(value_binding_lines);

    (key_lines, value_lines)
}

fn join_line_groups_with_empty(line_groups: Vec<Vec<Line<'static>>>) -> Vec<Line<'static>> {
    let mut result = Vec::new();
    let n = line_groups.len();
    for (i, lines) in line_groups.into_iter().enumerate() {
        result.extend(lines);
        if i < n - 1 {
            result.push(Line::raw(""));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use ratatui::style::Modifier;

    use super::*;

    #[test]
    fn test_every_bound_key_is_registered() {
        // a key no part of the app registers would never work, see `App::dispatch`, and the help
        // wouldn't list it
        let registered: Vec<UserEvent> = sections()
            .into_iter()
            .flat_map(|(_, helps)| helps.into_iter().flat_map(|(events, _)| events))
            .collect();
        for event in KeyBind::new(None).bound_events() {
            assert!(
                registered.contains(&event),
                "{event:?} is registered nowhere"
            );
        }
    }

    #[test]
    fn test_keys_look_like_everywhere() {
        // like the status bar's hints and the keys popup: "k/up", in the key style
        let theme = ColorTheme::default();
        let keybind = KeyBind::new(None);
        let (keys, values) = build_lines(&theme, &keybind);
        let mut checked = 0;
        for (key, value) in keys.iter().zip(&values) {
            // the rows of keys, not the titles of the blocks
            if value.width() == 0 || key.width() == 0 {
                continue;
            }
            for span in &key.spans {
                assert_eq!(span.style.fg, Some(theme.accent_fg), "{key}");
                assert!(span.style.add_modifier.contains(Modifier::BOLD), "{key}");
                assert!(!span.content.contains('<'), "{key}");
            }
            checked += 1;
        }
        assert!(checked > 20);
    }
}
