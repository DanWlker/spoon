// The menu a right click opens beside the pointer: the actions of the focused panel on the row
// clicked, as the status bar lists them, which run as their keys would.

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
    widget::style::{key_span, mark_selected_row, render_panel, KeyLabel, ListCursor},
};

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
}

#[derive(Debug)]
pub struct ActionMenu {
    entries: Vec<MenuEntry>,
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
    pub fn new(entries: Vec<MenuEntry>, at: Cell) -> Option<Self> {
        (!entries.is_empty()).then(|| ActionMenu {
            entries,
            at,
            cursor: ListCursor::default(),
        })
    }

    pub fn selected(&self) -> usize {
        self.cursor.selected()
    }

    pub fn select(&mut self, row: usize) {
        self.cursor.select(row, self.entries.len());
    }

    // The event of the entry at `row`.
    pub fn event(&self, row: usize) -> Option<UserEvent> {
        self.entries.get(row).map(|entry| entry.event)
    }

    pub fn handle_key(&mut self, event: UserEvent) -> MenuOutcome {
        match event {
            UserEvent::Confirm => MenuOutcome::Run(self.entries[self.selected()].event),
            UserEvent::Cancel => MenuOutcome::Close,
            event => {
                self.cursor.navigate(event, self.entries.len());
                MenuOutcome::Stay
            }
        }
    }

    pub fn keys() -> KeyList {
        use UserEvent::*;
        let mut k = Keys::of(&());
        k.add(&[Confirm], "run", "Run the selected action");
        k.add(&[Cancel], "close", "Close the menu");
        k.quiet(
            &[NavigateDown, SelectDown, NavigateUp, SelectUp],
            "Move down/up",
        );
        k.quiet(&[GoToTop, GoToBottom], "Go to the top/bottom");
        k.list()
    }

    // The width of the keys, the column of the short texts after them.
    fn keys_width(&self) -> usize {
        let keys = self.entries.iter().map(|e| e.label.keys.width());
        keys.max().unwrap_or(0)
    }

    // Beside the pointer on the `screen`: below and right of it, else above or left where it
    // would leave the screen; at most as large as the screen.
    pub fn area(&self, screen: Rect) -> Rect {
        let text = self.entries.iter().map(|e| e.label.action.width()).max();
        // the borders and the padding around the keys, a space and the text
        let width = (self.keys_width() + 1 + text.unwrap_or(0) + 4).min(screen.width as usize);
        let height = (self.entries.len() + 2).min(screen.height as usize);
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
        let offset = self.cursor.fit(height, self.entries.len(), 0);
        let rows = RowMap::Rows {
            first: offset,
            count: self.entries.len(),
        };
        ctx.layout.borrow_mut().record(Target::Menu, inner, rows);
        let keys_width = self.keys_width();
        for (y, entry) in (inner.top()..inner.bottom()).zip(self.entries.iter().skip(offset)) {
            let keys = format!("{:<keys_width$}", entry.label.keys);
            let line = Line::from(vec![
                key_span(keys, theme),
                Span::raw(format!(" {}", entry.label.action)).fg(theme.fg),
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
