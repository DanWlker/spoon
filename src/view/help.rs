use std::rc::Rc;

use ratatui::{
    crossterm::event::KeyEvent,
    layout::Rect,
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::Widget,
    Frame,
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::App,
    context::AppContext,
    event::{AppEvent, Sender},
    keybind::{KeyBind, Scope, UserEvent},
    keys::{KeyList, Keys},
    layout::{RowMap, Target},
    search::SearchKind,
    view::{detail::DetailView, list::ListView, query_status, BlameView, TimelineView},
    widget::tree_query::{register_typing_keys, type_query, typing_keys},
    widget::{
        command_log::CommandLogState,
        git_dialog::GitDialog,
        local_changes::LocalChangesState,
        style::{
            key_span, mark_selected_row, panel_title, placeholder, render_panel,
            render_panel_scrollbar, ListCursor,
        },
        tree_query::{highlighted_text, name_matches, QueryKind, TreeQuery},
    },
};

// The keys list, over the page, like lazygit's: the actions of the focused panel first, which
// enter runs, then those of the other panels, to read; / filters them by what they do.
#[derive(Debug)]
pub struct HelpView {
    rows: Vec<HelpRow>,
    // the indexes of the rows the filter keeps
    shown: Vec<usize>,
    cursor: ListCursor,
    query: TreeQuery,

    ctx: Rc<AppContext>,
    tx: Sender,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HelpRow {
    keys: String,
    text: &'static str,
    place: Place,
}

// Where an action of the list works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    // in the focused panel, run by this event
    Here(UserEvent),
    // in another part of the app, named so
    Elsewhere(&'static str),
}

impl HelpView {
    // The list for the focused panel, whose keys are `here`, named as they are bound in `scope`.
    pub fn new(ctx: Rc<AppContext>, tx: Sender, here: &KeyList, scope: Scope) -> HelpView {
        let rows = build_rows(&ctx.keybind, here, scope);
        let shown = (0..rows.len()).collect();
        HelpView {
            rows,
            shown,
            cursor: ListCursor::default(),
            query: TreeQuery::default(),
            ctx,
            tx,
        }
    }

    // The keys of the list, see `Keys`.
    pub fn register_keys(k: &mut Keys<Self>) {
        use UserEvent::*;
        k.add(&[Confirm], "run", "Run the selected action")
            .when(|v| v.selected_event().is_some());
        k.add(&[Search], "filter", "Filter the actions by what they do");
        k.add(&[Cancel], "clear filter", "Clear the filter, then close")
            .when(|v| v.query.filter().is_some());
        k.add(&[HelpToggle, Cancel], "close", "Close the keys list")
            .when(|v| v.query.filter().is_none());
        k.quiet(&[HelpToggle], "Close the keys list")
            .when(|v| v.query.filter().is_some());
        k.quiet(
            &[NavigateDown, SelectDown, NavigateUp, SelectUp],
            "Select the next/previous action",
        );
        k.quiet(&[GoToTop, GoToBottom], "Go to the top/bottom");
        k.quiet(&[HalfPageDown, HalfPageUp], "Jump half a page down/up");
        k.quiet(&[PageDown, PageUp], "Scroll a page down/up");
        k.quiet(&[ScrollDown, ScrollUp], "Scroll down/up");
    }

    pub fn keys(&self) -> KeyList {
        if self.query.is_editing() {
            return typing_keys(SearchKind::Names);
        }
        let mut keys = Keys::of(self);
        Self::register_keys(&mut keys);
        keys.list()
    }

    pub fn is_typing(&self) -> bool {
        self.query.is_editing()
    }

    // A key typed into the filter, which applies right away.
    pub fn type_key(&mut self, key: KeyEvent) {
        if type_query(&self.ctx, &mut self.query, key, SearchKind::Names).is_some() {
            self.apply_filter();
        }
        self.tx
            .send(query_status(&self.ctx, &self.query, SearchKind::Names));
    }

    pub fn handle_event(&mut self, event: UserEvent) {
        match event {
            UserEvent::Confirm => {
                if let Some(event) = self.selected_event() {
                    self.tx.send(AppEvent::RunAction(event));
                }
            }
            UserEvent::Search => {
                self.query.start(QueryKind::Filter, 0);
                self.tx
                    .send(query_status(&self.ctx, &self.query, SearchKind::Names));
            }
            // the filter first
            UserEvent::Cancel if self.query.filter().is_some() => {
                self.query.clear();
                self.apply_filter();
            }
            UserEvent::HelpToggle | UserEvent::Cancel => self.tx.send(AppEvent::CloseOverlay),
            event => {
                self.cursor.navigate(event, self.shown.len());
            }
        }
    }

    // Selects the action the mouse points at, see `View::point`.
    pub fn point(&mut self, target: Target, row: Option<usize>) -> bool {
        if target != Target::KeysList {
            return false;
        }
        if let Some(row) = row {
            self.cursor.select(row, self.shown.len());
        }
        true
    }

    // The event the selected row runs, None on an action of another panel.
    fn selected_event(&self) -> Option<UserEvent> {
        let row = &self.rows[*self.shown.get(self.cursor.selected())?];
        match row.place {
            Place::Here(event) => Some(event),
            Place::Elsewhere(_) => None,
        }
    }

    fn apply_filter(&mut self) {
        let options = self.ctx.search_options.get();
        let filter = self.query.filter();
        self.shown = (0..self.rows.len())
            .filter(|&i| filter.is_none_or(|f| name_matches(self.rows[i].text, f, options)))
            .collect();
        self.cursor.select(0, self.shown.len());
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        // one panel, like the command log's
        let ctx = self.ctx.clone();
        let theme = &ctx.color_theme;
        let notes = [self.shown.len().to_string()]
            .into_iter()
            .chain(self.query.indicator());
        let title = panel_title("Keys", notes, true, theme);
        let panel = area;
        let inner = render_panel(f.buffer_mut(), panel, title, true, theme);
        if self.shown.is_empty() {
            placeholder("No action matches the filter", theme).render(inner, f.buffer_mut());
        }
        let height = inner.height as usize;
        let total = self.shown.len();
        let scrolloff = ctx.ui_config.common.scrolloff as usize;
        let offset = self.cursor.fit(height, total, scrolloff);
        render_panel_scrollbar(f.buffer_mut(), panel, total, offset, true, theme);
        let shown = RowMap::Rows {
            first: offset,
            count: total,
        };
        ctx.layout
            .borrow_mut()
            .record(Target::KeysList, inner, shown);

        // at most a third of the width, for the text
        let key_width = self
            .shown
            .iter()
            .map(|&i| self.rows[i].keys.width())
            .max()
            .unwrap_or(0)
            .min(inner.width as usize / 3);
        let filter = self.query.filter();
        let options = ctx.search_options.get();
        for (n, &i) in self.shown.iter().enumerate().skip(offset).take(height) {
            let row = &self.rows[i];
            let y = inner.y + (n - offset) as u16;
            let (style, place) = match row.place {
                Place::Here(_) => (Style::default().fg(theme.fg), None),
                Place::Elsewhere(place) => (Style::default().fg(theme.muted_fg), Some(place)),
            };
            // keys are ASCII names, cut to the column
            let keys: String = row.keys.chars().take(key_width).collect();
            let mut spans = vec![key_span(format!(" {keys:>key_width$}  "), theme)];
            spans.extend(highlighted_text(
                row.text,
                row.text.len(),
                style,
                filter,
                options,
                theme,
            ));
            if let Some(place) = place {
                spans.push(Span::raw(format!("  {place}")).fg(theme.muted_fg).italic());
            }
            Line::from(spans).render(Rect::new(inner.x, y, inner.width, 1), f.buffer_mut());
            if n == self.cursor.selected() {
                mark_selected_row(f.buffer_mut(), inner, y, true, theme);
            }
        }
    }
}

// The actions of the focused panel (its keys `here`, with those of the whole app), then those of
// the other parts of the app, each once.
fn build_rows(keybind: &KeyBind, here: &KeyList, scope: Scope) -> Vec<HelpRow> {
    let mut rows: Vec<HelpRow> = here
        .working()
        .help()
        .into_iter()
        // the list itself is open
        .filter(|(events, _)| !events.contains(&UserEvent::HelpToggle))
        .map(|(events, text)| HelpRow {
            keys: keybind.short_keys(scope, &events),
            text,
            place: Place::Here(events[0]),
        })
        .collect();
    for (title, scopes, helps) in sections() {
        for (events, text) in helps {
            if rows.iter().any(|row| row.text == text) {
                continue;
            }
            let mut keys: Vec<String> = scopes
                .iter()
                .map(|scope| keybind.short_keys(*scope, &events))
                .filter(|keys| !keys.is_empty())
                .collect();
            keys.dedup();
            rows.push(HelpRow {
                keys: keys.join("/"),
                text,
                place: Place::Elsewhere(title),
            });
        }
    }
    rows
}

type Section = (
    &'static str,
    &'static [Scope],
    Vec<(Vec<UserEvent>, &'static str)>,
);

// Each part of the app with all the keys it registers, whatever its state, and the scopes its keys
// are bound in.
fn sections() -> Vec<Section> {
    fn section<T: ?Sized>(
        register: impl FnOnce(&mut Keys<T>),
    ) -> Vec<(Vec<UserEvent>, &'static str)> {
        let mut keys = Keys::all();
        register(&mut keys);
        keys.list().help()
    }
    use Scope::*;
    vec![
        ("everywhere", &[Global], section(App::register_app_keys)),
        ("typing a search", &[Global], section(register_typing_keys)),
        ("keys list", &[Global], section(HelpView::register_keys)),
        (
            "commit list",
            &[CommitList],
            section(ListView::register_keys),
        ),
        (
            "commit details",
            &[Details, DetailsPane],
            section(DetailView::register_keys),
        ),
        ("refs", &[Sidebar], section(App::register_sidebar_keys)),
        (
            "local changes",
            &[LocalChanges, LocalChangesDiff],
            section(LocalChangesState::register_keys),
        ),
        (
            "blame",
            &[FileHistory, FileHistoryPane],
            section(BlameView::register_keys),
        ),
        (
            "timeline",
            &[FileHistory, FileHistoryPane],
            section(TimelineView::register_keys),
        ),
        (
            "command log",
            &[Global],
            section(CommandLogState::register_keys),
        ),
        ("git popups", &[Dialog], section(GitDialog::register_keys)),
    ]
}

#[cfg(test)]
mod tests {
    use ratatui::{
        crossterm::event::{KeyCode, KeyModifiers},
        style::Modifier,
    };

    use super::*;
    use crate::event::EventController;

    // The keys list over a panel whose actions are rebasing and checking out, which has no key.
    fn list(ec: &EventController) -> HelpView {
        let mut keys = Keys::<()>::all();
        keys.add(&[UserEvent::Rebase], "rebase", "Rebase onto the commit");
        keys.add(&[UserEvent::Checkout], "checkout", "Check out the commit");
        let ctx = crate::test_support::ui::test_ctx();
        HelpView::new(ctx, ec.sender(), &keys.list(), Scope::CommitList)
    }

    fn type_text(help: &mut HelpView, text: &str) {
        for c in text.chars() {
            help.type_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        help.type_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    }

    fn runs(ec: &EventController) -> Vec<UserEvent> {
        std::iter::from_fn(|| ec.try_recv())
            .filter_map(|event| match event {
                AppEvent::RunAction(event) => Some(event),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn test_runs_the_actions_of_the_panel_filtered() {
        let ec = EventController::without_terminal();
        let mut help = list(&ec);
        // the panel's first, with their keys as bound there
        assert_eq!(help.rows[0].keys, "r");
        assert_eq!(help.rows[0].place, Place::Here(UserEvent::Rebase));
        help.handle_event(UserEvent::Confirm);
        assert_eq!(runs(&ec), [UserEvent::Rebase]);

        // an action with no key is listed and runs too
        assert_eq!(help.rows[1].keys, "");
        help.handle_event(UserEvent::Search);
        assert!(help.is_typing());
        type_text(&mut help, "Check out the commit");
        assert!(!help.is_typing());
        // once: the other panels checking out a commit say the same
        assert_eq!(help.shown.len(), 1);
        help.handle_event(UserEvent::Confirm);
        assert_eq!(runs(&ec), [UserEvent::Checkout]);

        // the other panels' actions are listed, not run
        help.handle_event(UserEvent::Cancel);
        assert_eq!(help.shown.len(), help.rows.len());
        help.handle_event(UserEvent::Search);
        type_text(&mut help, "Squash the picked");
        assert_eq!(help.shown.len(), 1);
        let row = &help.rows[help.shown[0]];
        assert_eq!(row.place, Place::Elsewhere("commit list"));
        assert_eq!(row.keys, "s");
        assert!(!help.keys().takes(UserEvent::Confirm));
        help.handle_event(UserEvent::Confirm);
        assert_eq!(runs(&ec), []);

        // esc clears the filter, then closes
        help.handle_event(UserEvent::Cancel);
        assert!(help.query.filter().is_none());
        help.handle_event(UserEvent::Cancel);
        assert!(matches!(ec.try_recv(), Some(AppEvent::CloseOverlay)));
    }

    #[test]
    fn test_draws_in_a_narrow_terminal() {
        let ec = EventController::without_terminal();
        let mut help = list(&ec);
        for (width, height) in [(1, 1), (12, 10), (3, 40)] {
            let backend = ratatui::backend::TestBackend::new(width, height);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal.draw(|f| help.render(f, f.area())).unwrap();
        }
    }

    #[test]
    fn test_every_bound_key_is_registered() {
        // a key no part of the app registers would never work, see `App::dispatch`, and the keys
        // list wouldn't list it
        let registered: Vec<UserEvent> = sections()
            .into_iter()
            .flat_map(|(_, _, helps)| helps.into_iter().flat_map(|(events, _)| events))
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
        // like the status bar's hints and the keys popup: in the key style
        let ec = EventController::without_terminal();
        let mut help = list(&ec);
        let theme = help.ctx.color_theme.clone();
        let backend = ratatui::backend::TestBackend::new(60, 10);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let frame = terminal.draw(|f| help.render(f, f.area())).unwrap();
        let buf = frame.buffer;
        let key = (0..buf.area.width)
            .map(|x| &buf[(x, 1)])
            .find(|cell| cell.symbol() == "r")
            .unwrap();
        assert_eq!(key.fg, theme.accent_fg);
        assert!(key.modifier.contains(Modifier::BOLD));
        // no key is written like a config key, "<ctrl-x>"
        assert!(help.rows.iter().all(|row| !row.keys.contains('<')));
    }
}
