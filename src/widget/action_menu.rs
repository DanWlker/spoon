// The menu a right click opens beside the pointer: the actions of the focused panel on the row
// clicked, as the status bar lists them, which run as their keys would, in sections split by a
// rule.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Stylize,
    text::{Line, Span},
    widgets::{Clear, Widget},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    context::AppContext,
    keybind::UserEvent,
    keys::{KeyList, Keys},
    layout::{RowMap, Target},
    pointer::Cell,
    widget::style::{divider, key_span, mark_selected_row, render_panel, KeyLabel, ListCursor},
};

// Between an action and its keys.
const GAP: &str = "   ";

// An action of the menu: the event it runs, with its keys and short text.
#[derive(Debug, Clone)]
pub struct MenuEntry {
    pub event: UserEvent,
    pub label: KeyLabel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuOutcome {
    Stay,
    Close,
    Run(UserEvent),
    // q, which quits the app from anywhere
    Quit,
}

// The sections of the menu, in their order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Section {
    Show,
    Git,
    Filter,
    Copy,
}

fn section(event: UserEvent) -> Section {
    use UserEvent::*;
    match event {
        Select | Stash | Commit | Discard | Fetch | FetchAll | Pull | Push | Rebase | Merge
        | Checkout | CherryPick | Squash | Revert | Reset | AmendCommit | DropCommit
        | CreateTag | CreateBranch | CreateRef | DeleteRef | EditRef | ResetFile | Ignore => {
            Section::Git
        }
        BranchOnly | BranchOnlyAtCommit => Section::Filter,
        ShortCopy | FullCopy | CopyLink | OpenInBrowser => Section::Copy,
        _ => Section::Show,
    }
}

#[derive(Debug)]
pub struct ActionMenu {
    // the entries by section, None the rule between two sections
    rows: Vec<Option<MenuEntry>>,
    // the cell right clicked
    at: Cell,
    cursor: ListCursor,
}

// Whether the menu leaves out the hint running `event`, which is no action on the row: moving the
// selection, the focus, the tab, the commit shown or the match, closing, searching and picking
// (which dragging does), the commands of the whole page and the context of the diffs. The keys
// still have them.
pub fn left_out_of_menu(event: UserEvent) -> bool {
    use UserEvent::*;
    event.moves_selection()
        || matches!(
            event,
            NextTab
                | PreviousTab
                | OlderCommit
                | NewerCommit
                | GoToNext
                | GoToPrevious
                | NavigateLeft
                | NavigateRight
                | Cancel
                | HelpToggle
                | Search
                | ToggleSelect
                | Commit
                | ToggleAmend
                | Continue
                | Abort
                | Skip
                | IncreaseDiffContext
                | DecreaseDiffContext
        )
}

impl ActionMenu {
    // None without an entry.
    pub fn new(mut entries: Vec<MenuEntry>, at: Cell) -> Option<Self> {
        entries.sort_by_key(|entry| section(entry.event));
        let mut rows = vec![];
        for entry in entries {
            let last = rows.last().and_then(|row: &Option<MenuEntry>| row.as_ref());
            if last.is_some_and(|last| section(last.event) != section(entry.event)) {
                rows.push(None);
            }
            rows.push(Some(entry));
        }
        (!rows.is_empty()).then(|| ActionMenu {
            rows,
            at,
            cursor: ListCursor::default(),
        })
    }

    pub fn selected(&self) -> usize {
        self.cursor.selected()
    }

    // The entry at `row`; a rule keeps the selection.
    pub fn select(&mut self, row: usize) {
        if self.event(row).is_some() {
            self.cursor.select(row, self.rows.len());
        }
    }

    // The event of the entry at `row`, None on a rule.
    pub fn event(&self, row: usize) -> Option<UserEvent> {
        self.rows.get(row)?.as_ref().map(|entry| entry.event)
    }

    pub fn handle_key(&mut self, event: UserEvent) -> MenuOutcome {
        match event {
            UserEvent::Confirm => self
                .event(self.selected())
                .map_or(MenuOutcome::Stay, MenuOutcome::Run),
            UserEvent::Cancel => MenuOutcome::Close,
            UserEvent::Quit => MenuOutcome::Quit,
            event => {
                let from = self.selected();
                self.cursor.navigate(event, self.rows.len());
                // past a rule the way it moved; a rule is never first or last
                let to = self.selected();
                if self.event(to).is_none() {
                    let past = if to > from { to + 1 } else { to - 1 };
                    self.cursor.select(past, self.rows.len());
                }
                MenuOutcome::Stay
            }
        }
    }

    pub fn keys() -> KeyList {
        use UserEvent::*;
        let mut k = Keys::of(&());
        k.add(&[Confirm], "run", "Run the selected action");
        k.add(&[Cancel], "close", "Close the menu");
        k.quiet(&[Quit], "Quit app");
        k.quiet(
            &[NavigateDown, SelectDown, NavigateUp, SelectUp],
            "Move down/up",
        );
        k.quiet(&[GoToTop, GoToBottom], "Go to the top/bottom");
        k.list()
    }

    // The widths of the actions and of their keys, a column each: the action first, its keys
    // after it.
    fn widths(&self) -> (usize, usize) {
        let width = |of: fn(&KeyLabel) -> &str| {
            let widths = self.rows.iter().flatten().map(|e| of(&e.label).width());
            widths.max().unwrap_or(0)
        };
        (width(|l| &l.action), width(|l| &l.keys))
    }

    // Beside the pointer on the `screen`: below and right of it, else above or left where it
    // would leave the screen; at most as large as the screen.
    pub fn area(&self, screen: Rect) -> Rect {
        let (action, keys) = self.widths();
        // the borders and the padding around the action, the gap and the keys
        let width = (action + GAP.len() + keys + 4).min(screen.width as usize);
        let height = (self.rows.len() + 2).min(screen.height as usize);
        let (width, height) = (width as u16, height as u16);
        let place = |at: u16, size: u16, start: u16, end: u16| {
            let after = at.saturating_add(1);
            let pos = if after.saturating_add(size) <= end {
                after
            } else {
                at.saturating_sub(size)
            };
            pos.clamp(start, end.saturating_sub(size).max(start))
        };
        Rect::new(
            place(self.at.x, width, screen.left(), screen.right()),
            place(self.at.y, height, screen.top(), screen.bottom()),
            width,
            height,
        )
    }

    pub fn render(&mut self, buf: &mut Buffer, screen: Rect, ctx: &AppContext) {
        let theme = &ctx.color_theme;
        let area = self.area(screen).intersection(buf.area);
        Clear.render(area, buf);
        let inner = render_panel(buf, area, Line::default(), true, theme);
        let height = inner.height as usize;
        let offset = self.cursor.fit(height, self.rows.len(), 0);
        let rows = RowMap::Rows {
            first: offset,
            count: self.rows.len(),
        };
        ctx.layout.borrow_mut().record(Target::Menu, inner, rows);
        let (action_width, _) = self.widths();
        for (y, row) in (inner.top()..inner.bottom()).zip(self.rows.iter().skip(offset)) {
            let Some(entry) = row else {
                // between the borders, past the padding
                let width = area.width.saturating_sub(2);
                buf.set_span(
                    area.x.saturating_add(1),
                    y,
                    &divider(width.into(), theme),
                    width,
                );
                continue;
            };
            let action = format!("{:<action_width$}{GAP}", entry.label.action);
            let line = Line::from(vec![
                Span::raw(action).fg(theme.fg),
                key_span(entry.label.keys.clone(), theme),
            ]);
            buf.set_line(inner.x, y, &line, inner.width);
        }
        if let Some(row) = self
            .selected()
            .checked_sub(offset)
            .filter(|row| *row < height)
        {
            mark_selected_row(buf, inner, inner.y + row as u16, true, theme);
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{buffer::Buffer, layout::Rect};

    use super::*;
    use crate::{keybind::UserEvent, test_support::ui::test_ctx, widget::style::KeyLabel};

    fn entries(n: usize) -> Vec<MenuEntry> {
        (0..n)
            .map(|i| MenuEntry {
                event: UserEvent::Merge,
                label: KeyLabel::new("m", format!("merge {i}")),
            })
            .collect()
    }

    #[test]
    fn test_placement_flips_at_the_edges() {
        let screen = Rect::new(0, 0, 80, 24);
        let menu = ActionMenu::new(entries(5), Cell { x: 2, y: 2 }).unwrap();
        let area = menu.area(screen);
        assert!(area.x >= 2 && area.y >= 3, "{area:?}");
        // in the bottom right corner: left of and above the pointer
        let menu = ActionMenu::new(entries(5), Cell { x: 78, y: 22 }).unwrap();
        let area = menu.area(screen);
        assert!(area.right() <= 78 && area.bottom() <= 22, "{area:?}");
        // taller than the screen: as tall as it
        let menu = ActionMenu::new(entries(50), Cell { x: 10, y: 10 }).unwrap();
        assert_eq!(menu.area(screen).height, 24);
    }

    #[test]
    fn test_moving_hints_are_left_out() {
        use UserEvent::*;
        for event in [
            NavigateDown,
            NextTab,
            OlderCommit,
            GoToNext,
            NavigateLeft,
            Cancel,
        ] {
            assert!(left_out_of_menu(event), "{event:?}");
        }
        assert!(!left_out_of_menu(Checkout));
        assert!(!left_out_of_menu(Confirm));
        assert!(ActionMenu::new(vec![], Cell { x: 0, y: 0 }).is_none());
    }

    #[test]
    fn test_keys_move_run_and_close() {
        let mut menu = ActionMenu::new(entries(3), Cell { x: 0, y: 0 }).unwrap();
        assert_eq!(menu.handle_key(UserEvent::NavigateDown), MenuOutcome::Stay);
        assert_eq!(menu.handle_key(UserEvent::NavigateDown), MenuOutcome::Stay);
        assert_eq!(menu.selected(), 2);
        assert_eq!(
            menu.handle_key(UserEvent::Confirm),
            MenuOutcome::Run(UserEvent::Merge)
        );
        assert_eq!(menu.handle_key(UserEvent::Cancel), MenuOutcome::Close);
        menu.select(0);
        assert_eq!(menu.selected(), 0);
    }

    // A row names the action first, its keys after it, in a column at the right.
    #[test]
    fn test_action_before_its_keys() {
        let ctx = test_ctx();
        let screen = Rect::new(0, 0, 40, 10);
        let entries = vec![
            MenuEntry {
                event: UserEvent::Merge,
                label: KeyLabel::new("m", "merge"),
            },
            MenuEntry {
                event: UserEvent::Checkout,
                label: KeyLabel::new("space", "checkout"),
            },
        ];
        let mut menu = ActionMenu::new(entries, Cell { x: 0, y: 0 }).unwrap();
        let mut buf = Buffer::empty(screen);
        menu.render(&mut buf, screen, &ctx);
        let row = |y: u16| -> String { (0..40).map(|x| buf[(x, y)].symbol()).collect() };
        let (merge, checkout) = (row(2), row(3));
        assert!(merge.contains("merge      m "), "{merge:?}");
        assert!(checkout.contains("checkout   space "), "{checkout:?}");
    }

    // The entries by section, a rule between two; the keys, the pointer and a click pass the rule
    // by.
    #[test]
    fn test_sections_split_by_a_rule() {
        let ctx = test_ctx();
        let entry = |event, action| MenuEntry {
            event,
            label: KeyLabel::new("", action),
        };
        let entries = vec![
            entry(UserEvent::ShortCopy, "copy"),
            entry(UserEvent::Merge, "merge"),
            entry(UserEvent::Diff, "diff"),
            entry(UserEvent::Checkout, "checkout"),
        ];
        let mut menu = ActionMenu::new(entries, Cell { x: 0, y: 0 }).unwrap();
        let screen = Rect::new(0, 0, 20, 10);
        let mut buf = Buffer::empty(screen);
        menu.render(&mut buf, screen, &ctx);
        let row = |y: u16| -> String {
            let row: String = (0..20).map(|x| buf[(x, y)].symbol()).collect();
            row.trim().to_string()
        };
        let rows: Vec<String> = (2..8).map(row).collect();
        assert_eq!(
            rows,
            [
                "│▌diff        │",
                "│─────────────│",
                "│ merge       │",
                "│ checkout    │",
                "│─────────────│",
                "│ copy        │",
            ],
        );
        assert_eq!(menu.event(1), None);
        menu.handle_key(UserEvent::NavigateDown);
        assert_eq!(menu.selected(), 2);
        menu.handle_key(UserEvent::NavigateUp);
        assert_eq!(menu.selected(), 0);
        menu.select(1);
        assert_eq!(menu.selected(), 0);
    }

    #[test]
    fn test_draws_at_every_size() {
        let ctx = test_ctx();
        for width in 1..=40 {
            for height in 1..=16 {
                let screen = Rect::new(0, 0, width, height);
                let corners = [
                    (0, 0),
                    (width - 1, 0),
                    (0, height - 1),
                    (width - 1, height - 1),
                ];
                for (x, y) in corners {
                    let mut menu = ActionMenu::new(entries(20), Cell { x, y }).unwrap();
                    let mut buf = Buffer::empty(screen);
                    menu.render(&mut buf, screen, &ctx);
                }
            }
        }
    }
}
