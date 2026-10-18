// The looks shared by the panels, after Charm's tools (lipgloss, huh, bubbles): the title of the
// focused panel as a badge, key hints, and a mark beside the selected row.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Padding, Paragraph, Widget},
};

use crate::{color::ColorTheme, event::UserEvent};

// How often the spinner of a running task turns, see `spinner`.
pub const SPINNER_RATE: Duration = Duration::from_millis(180);

// lazygit's spinner, by the clock, so each frame drawn while a task runs shows where it is.
pub fn spinner() -> &'static str {
    const FRAMES: [&str; 4] = ["●∙∙", "∙●∙", "∙∙●", "∙●∙"];
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    FRAMES[(now.as_millis() / SPINNER_RATE.as_millis()) as usize % FRAMES.len()]
}

// The title of a panel, like "[1] Unstaged (3)": a badge while it has the focus.
// `extra`, like "· 2 picked", follows it.
pub fn panel_title(label: &str, extra: &str, focused: bool, theme: &ColorTheme) -> Line<'static> {
    let mut spans = vec![if focused {
        focused_title(format!(" {label} "), theme)
    } else {
        Span::raw(format!(" {label} ")).fg(theme.fg)
    }];
    if !extra.is_empty() {
        spans.push(Span::raw(format!(" {extra} ")).fg(theme.highlight_fg));
    }
    Line::from(spans)
}

// A panel after lazygit's: a rounded border all around, in the accent color while it has the
// focus.
pub fn panel_block<'a>(focused: bool, theme: &ColorTheme) -> Block<'a> {
    let fg = if focused {
        theme.accent_fg
    } else {
        theme.divider_fg
    };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(fg))
}

// The arrows around a value cycled with left and right, like a popup's option or the commit of
// the blame: each lit while its key would change the value.
pub fn choice_arrows(left: bool, right: bool, theme: &ColorTheme) -> [Span<'static>; 2] {
    let fg = |lit| {
        if lit {
            theme.highlight_fg
        } else {
            theme.muted_fg
        }
    };
    [Span::raw("‹ ").fg(fg(left)), Span::raw(" ›").fg(fg(right))]
}

// In place of `choice_arrows` for a value with nothing else to choose, lined up with the choices.
pub fn no_choice_arrows() -> [Span<'static>; 2] {
    [Span::raw("  "), Span::raw("")]
}

// The marker of a folder or section that opens: nvim-tree's chevrons (Nerd Font, Octicons).
pub fn fold_marker(open: bool) -> &'static str {
    if open {
        "\u{f47c} "
    } else {
        "\u{f460} "
    }
}

// Draws a panel in `area`, like every list: its border, `title` and a padding of one column each
// side; returns where its content goes.
pub fn render_panel(
    buf: &mut Buffer,
    area: Rect,
    title: Line<'_>,
    focused: bool,
    theme: &ColorTheme,
) -> Rect {
    let block = panel_block(focused, theme)
        .title(title)
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    block.render(area, buf);
    inner
}

// The title of the focused panel: a badge, or colored text alone when its background is "reset".
pub fn focused_title<'a>(
    text: impl Into<std::borrow::Cow<'a, str>>,
    theme: &ColorTheme,
) -> Span<'a> {
    let span = Span::raw(text).fg(theme.panel_title_focused_fg).bold();
    match theme.panel_title_focused_bg {
        Color::Reset => span,
        bg => span.bg(bg),
    }
}

// Text on a colored background, like an error.
pub fn badge<'a>(
    text: impl Into<std::borrow::Cow<'a, str>>,
    bg: Color,
    theme: &ColorTheme,
) -> Span<'a> {
    Span::raw(text).fg(theme.accent_text_fg).bg(bg).bold()
}

// A popup in the middle of `area`, as wide as its `lines` within `widths` (min, max), its border in
// `color`; returns where its text goes.
pub fn render_popup(
    title: Span,
    color: Color,
    lines: Vec<Line>,
    widths: (usize, usize),
    area: Rect,
    buf: &mut Buffer,
    theme: &ColorTheme,
) -> Rect {
    // text plus padding and borders
    let text_width = lines.iter().map(Line::width).max().unwrap_or(0);
    let width = (text_width.clamp(widths.0, widths.1) as u16 + 4).min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let popup = area.centered(Constraint::Length(width), Constraint::Length(height));
    Clear.render(popup, buf);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(color))
        .style(Style::default().fg(theme.fg).bg(theme.bg))
        .title(title)
        .padding(Padding::horizontal(1));
    let inner = block.inner(popup);
    Paragraph::new(lines).block(block).render(popup, buf);
    inner
}

// The color of a file's change, by its letter like git's: "A" added, "?" untracked, "D" deleted,
// "U" conflicted, "R" renamed, "C" copied, else modified.
pub fn change_color(symbol: &str, theme: &ColorTheme) -> Color {
    match symbol {
        "A" | "?" => theme.detail_file_change_add_fg,
        "D" | "U" => theme.detail_file_change_delete_fg,
        "R" | "C" => theme.detail_file_change_move_fg,
        _ => theme.detail_file_change_modify_fg,
    }
}

// Muted text standing in for what isn't there, like "No changes".
pub fn placeholder(text: impl Into<String>, theme: &ColorTheme) -> Line<'static> {
    Line::raw(text.into()).fg(theme.muted_fg)
}

// A divider, `width` columns of "─".
pub fn divider(width: usize, theme: &ColorTheme) -> Span<'static> {
    Span::raw("─".repeat(width)).fg(theme.divider_fg)
}

// A file's change letter in its color, the same in every list of files.
pub fn change_span(symbol: &str, theme: &ColorTheme) -> Span<'static> {
    Span::raw(symbol.to_string()).fg(change_color(symbol, theme))
}

// The width of the file tree beside a diff or a preview, in `width`.
pub fn tree_panel_width(width: u16) -> u16 {
    (width / 3).clamp(20, 50).min(width)
}

// Keys and what they do, like "j/k move • enter push • esc cancel".
// A key name, like "j/k", the same in the status bar, the keys popup, the help and the notices.
pub fn key_span(key: impl Into<String>, theme: &ColorTheme) -> Span<'static> {
    Span::raw(key.into()).fg(theme.accent_fg).bold()
}

// A key as shown, like "j/k", and what it does, like "move".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyLabel {
    pub keys: String,
    pub action: String,
}

impl KeyLabel {
    pub fn new(keys: impl Into<String>, action: impl Into<String>) -> Self {
        KeyLabel {
            keys: keys.into(),
            action: action.into(),
        }
    }
}

pub fn key_hints(hints: &[KeyLabel], theme: &ColorTheme) -> Line<'static> {
    let mut spans = vec![];
    for (i, hint) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" • ").fg(theme.divider_fg));
        }
        spans.push(key_span(hint.keys.clone(), theme));
        spans.push(Span::raw(format!(" {}", hint.action)).fg(theme.muted_fg));
    }
    Line::from(spans)
}

// A picked row: its text recolored to stay readable on the picked background, in every list.
pub fn picked_style(theme: &ColorTheme) -> Style {
    Style::default()
        .fg(theme.list_picked_fg)
        .bg(theme.list_picked_bg)
}

// Highlights a picked `row` of a list, across it; see `picked_style`.
pub fn mark_picked_row(buf: &mut Buffer, row: Rect, theme: &ColorTheme) {
    buf.set_style(row, picked_style(theme));
}

// Highlights the selected row of a list in a panel with a horizontal padding of one: its
// background across the row, the text keeping its colors, like the commit list; and a bar in
// that padding, quieter while the list doesn't have the focus.
pub fn mark_selected_row(buf: &mut Buffer, inner: Rect, y: u16, focused: bool, theme: &ColorTheme) {
    if y < inner.top() || y >= inner.bottom() {
        return;
    }
    let row = Rect::new(inner.x, y, inner.width, 1);
    buf.set_style(row, Style::default().bg(theme.list_selected_bg));
    if inner.x > 0 {
        mark_selected_bar(buf, inner.x - 1, y, focused, theme);
    }
}

// A scroll thumb, a full block, on `column` (one cell wide: a panel's border, or a divider), when `total` rows
// don't fit its height; the border stays as the track.
// In the color of the border it is on, the accent's when its panel has the focus.
pub fn render_scrollbar(
    buf: &mut Buffer,
    column: Rect,
    total: usize,
    offset: usize,
    focused: bool,
    theme: &ColorTheme,
) {
    let height = column.height as usize;
    if height == 0 || total <= height {
        return;
    }
    let thumb = (height * height / total).max(1);
    let max_offset = total - height;
    let top = offset.min(max_offset) * (height - thumb) / max_offset;
    let fg = if focused {
        theme.accent_fg
    } else {
        theme.divider_fg
    };
    let style = Style::default().fg(fg);
    for y in top..top + thumb {
        buf.set_string(column.x, column.y + y as u16, "█", style);
    }
}

// The scroll of rows shown `height` at a time from `offset`: text, a diff or a file's preview.
#[derive(Debug, Default, Clone)]
pub struct ScrollState {
    pub offset: usize,
    // of the last render, 0 before it
    pub height: usize,
    // the rows kept around a selection, of the last render, see `scroll_to`
    pub scrolloff: usize,
}

impl ScrollState {
    // Keeps the offset within the `count` rows shown in `height`, the last one at the bottom; the
    // rows to render. `scrolloff` is kept for the next selection, see `DiffCursor::scroll_to`.
    pub fn fit(&mut self, count: usize, height: usize, scrolloff: usize) -> std::ops::Range<usize> {
        self.height = height;
        self.scrolloff = scrolloff;
        self.offset = self.offset.min(count.saturating_sub(height));
        self.offset..self.offset + height
    }

    pub fn scroll(&mut self, rows: isize) {
        self.offset = self.offset.saturating_add_signed(rows);
    }

    // Scrolls by the scroll keys: a line, a page of the rows shown, or half of one; returns
    // whether `event` was one of them.
    pub fn handle_scroll(&mut self, event: UserEvent) -> bool {
        let page = self.height.max(1) as isize;
        let rows = match event {
            UserEvent::ScrollDown => 1,
            UserEvent::ScrollUp => -1,
            UserEvent::PageDown => page,
            UserEvent::PageUp => -page,
            UserEvent::HalfPageDown => page / 2,
            UserEvent::HalfPageUp => -page / 2,
            _ => return false,
        };
        self.scroll(rows);
        true
    }

    // Scrolls text with nothing to select by the movement keys too, to the top and the bottom.
    pub fn handle_event(&mut self, event: UserEvent) -> bool {
        match event {
            UserEvent::NavigateDown => self.scroll(1),
            UserEvent::NavigateUp => self.scroll(-1),
            UserEvent::GoToTop => self.offset = 0,
            UserEvent::GoToBottom => self.offset = usize::MAX,
            _ => return self.handle_scroll(event),
        }
        true
    }
}

// The row ctrl-d or ctrl-u selects in a list of `total` rows shown `height` at a time: half a
// page from `selected`, the list then scrolling only as moving there one row at a time would
// (see `scroll_to`); None for other events.
pub fn half_page_jump(
    event: UserEvent,
    selected: usize,
    height: usize,
    total: usize,
) -> Option<usize> {
    let half = (height / 2).max(1);
    match event {
        UserEvent::HalfPageDown => Some((selected + half).min(total.saturating_sub(1))),
        UserEvent::HalfPageUp => Some(selected.saturating_sub(half)),
        _ => None,
    }
}

// The first row shown of a list of `total` rows, `height` high, moved as little as can be from
// `offset` to show the `selected` row with `scrolloff` rows above and below it where there are,
// like vim's; at most half the height, and never past the last row.
pub fn scroll_to(
    offset: usize,
    selected: usize,
    height: usize,
    total: usize,
    scrolloff: usize,
) -> usize {
    let height = height.max(1);
    let margin = scrolloff.min((height - 1) / 2);
    let bottom = height - 1 - margin;
    offset
        .clamp(
            selected.saturating_sub(bottom),
            selected.saturating_sub(margin),
        )
        .min(total.saturating_sub(height))
}

// Where ctrl-d and ctrl-u go in a list: the list, moving its selection half a page, where the list
// is what is read (the commit graph, the sidebar, the commits of the timeline); or the pane beside
// it, scrolled like with the other scroll keys, where that is (the output of the command log, the
// diff or preview beside the file trees).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HalfPage {
    List,
    Pane,
}

// The selected row of a list and the rows shown from `offset`, `height` at a time: the commit list,
// the file trees, the command log and the timeline. The selection moves keep it in view with
// `scrolloff` rows around it, scrolling as little as can be, see `scroll_to`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ListCursor {
    selected: usize,
    offset: usize,
    // as last drawn, 0 before
    height: usize,
    scrolloff: usize,
}

impl ListCursor {
    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn offset(&self) -> usize {
        self.offset
    }

    pub fn height(&self) -> usize {
        self.height
    }

    // The row of the selection among those shown.
    pub fn row(&self) -> usize {
        self.selected - self.offset
    }

    // Selects the row at `index` of `total`, or the last one.
    pub fn select(&mut self, index: usize, total: usize) {
        self.selected = index.min(total.saturating_sub(1));
        // before the first drawing the rows shown are unknown, which `fit` scrolls to then
        if self.height > 0 {
            let (offset, height) = (self.offset, self.height);
            self.offset = scroll_to(offset, self.selected, height, total, self.scrolloff);
        }
    }

    // Selects the row at `index`, shown at `row` of the rows where it can be: a list shown again
    // as it was, or a match jumped to.
    pub fn select_at(&mut self, index: usize, row: usize, total: usize) {
        self.selected = index.min(total.saturating_sub(1));
        self.offset = self
            .selected
            .saturating_sub(row.min(self.height.saturating_sub(1)))
            .min(total.saturating_sub(self.height));
    }

    // The row the movement keys select in `total` rows: j and k (and J and K), g and G, and ctrl-d
    // and ctrl-u where `half_page` gives them to the list, see `half_page_jump`; None for other
    // events.
    pub fn moved_to(&self, event: UserEvent, total: usize, half_page: HalfPage) -> Option<usize> {
        let last = total.checked_sub(1)?;
        match event {
            UserEvent::NavigateDown | UserEvent::SelectDown => Some((self.selected + 1).min(last)),
            UserEvent::NavigateUp | UserEvent::SelectUp => Some(self.selected.saturating_sub(1)),
            UserEvent::GoToTop => Some(0),
            UserEvent::GoToBottom => Some(last),
            _ if half_page == HalfPage::List => {
                half_page_jump(event, self.selected, self.height, total)
            }
            _ => None,
        }
    }

    // Scrolls a row at a time with ctrl-e and ctrl-y, the selection staying put until it would
    // leave the `scrolloff` rows at the edge, then moving along.
    pub fn scroll_lines(&mut self, rows: isize, total: usize) {
        let margin = self.margin();
        let last_offset = total.saturating_sub(self.height);
        self.offset = self.offset.saturating_add_signed(rows).min(last_offset);
        let top = self.offset + margin;
        let bottom = (self.offset + self.height).saturating_sub(1 + margin);
        self.selected = self
            .selected
            .clamp(top, bottom.max(top))
            .min(total.saturating_sub(1));
    }

    // Scrolls a page at a time with ctrl-f and ctrl-b, the selection moving as much; at either end
    // of the list the rows stay and the selection goes on to the first or last row.
    pub fn scroll_pages(&mut self, pages: isize, total: usize) {
        if self.height == 0 || total == 0 {
            return;
        }
        let rows = pages * self.height as isize;
        let last_offset = total.saturating_sub(self.height);
        let offset = self.offset.saturating_add_signed(rows);
        self.selected = self.selected.saturating_add_signed(rows).min(total - 1);
        self.offset = offset.min(last_offset);
        if self.offset != 0 && self.offset != last_offset {
            self.offset = scroll_to(
                self.offset,
                self.selected,
                self.height,
                total,
                self.scrolloff,
            );
        }
    }

    // Shows `total` rows in `height`, keeping the selection in view: the first row shown.
    pub fn fit(&mut self, height: usize, total: usize, scrolloff: usize) -> usize {
        self.height = height;
        self.scrolloff = scrolloff;
        self.selected = self.selected.min(total.saturating_sub(1));
        self.offset = scroll_to(self.offset, self.selected, height, total, scrolloff);
        self.offset
    }

    fn margin(&self) -> usize {
        self.scrolloff.min(self.height.saturating_sub(1) / 2)
    }
}

// The scroll thumb on the right border of the panel `area` (borders all around).
pub fn render_panel_scrollbar(
    buf: &mut Buffer,
    area: Rect,
    total: usize,
    offset: usize,
    focused: bool,
    theme: &ColorTheme,
) {
    if area.width > 0 && area.height > 2 {
        let column = Rect::new(area.right() - 1, area.y + 1, 1, area.height - 2);
        render_scrollbar(buf, column, total, offset, focused, theme);
    }
}

// The bar beside a selected row, at `x`: in the padding of a panel, or on its border.
pub fn mark_selected_bar(buf: &mut Buffer, x: u16, y: u16, focused: bool, theme: &ColorTheme) {
    let fg = if focused {
        theme.highlight_fg
    } else {
        theme.muted_fg
    };
    buf.set_string(x, y, "▌", Style::default().fg(fg));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ui::{assert_key_style, theme_with_bg};

    #[test]
    fn test_key_hints_name_keys_like_everywhere() {
        let theme = theme_with_bg();
        let hints = [KeyLabel::new("j/k", "move"), KeyLabel::new("q", "quit")];
        let line = key_hints(&hints, &theme);
        let area = Rect::new(0, 0, 30, 1);
        let mut buf = Buffer::empty(area);
        line.render(area, &mut buf);
        assert_key_style(&buf, 0, 0, &theme);
        // "j/k move • q quit"
        assert_key_style(&buf, 11, 0, &theme);
        assert_ne!(
            buf[(4, 0)].fg,
            theme.accent_fg,
            "the description isn't a key"
        );
    }

    #[test]
    fn test_scroll_to_small_heights() {
        // every small case: no panic, the selected row shown, never past the last row
        for height in 0..6 {
            for total in 0..9 {
                for selected in 0..total.max(1) {
                    for offset in 0..10 {
                        for scrolloff in 0..8 {
                            let o = scroll_to(offset, selected, height, total, scrolloff);
                            let shown = height.max(1);
                            assert!(o <= total.saturating_sub(shown));
                            if total > 0 {
                                assert!((o..o + shown).contains(&selected), "{o} {selected}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn test_half_page_goes_to_the_list_or_the_pane() {
        let mut cursor = ListCursor::default();
        cursor.fit(10, 30, 0);
        let moved = |half_page| cursor.moved_to(UserEvent::HalfPageDown, 30, half_page);
        assert_eq!(moved(HalfPage::List), Some(5));
        // left to the pane, which scrolls
        assert_eq!(moved(HalfPage::Pane), None);
        // the other movement keys move the list either way
        let down = cursor.moved_to(UserEvent::NavigateDown, 30, HalfPage::Pane);
        assert_eq!(down, Some(1));
    }

    #[test]
    fn test_scroll_to() {
        assert_eq!(scroll_to(5, 2, 10, 20, 0), 2); // above: scrolls up to it
        assert_eq!(scroll_to(0, 12, 10, 20, 0), 3); // below: the last row shown
        assert_eq!(scroll_to(3, 7, 10, 20, 0), 3); // shown: stays
        assert_eq!(scroll_to(3, 7, 0, 20, 0), 7);
        // with rows kept around it: 2 above, 2 below
        assert_eq!(scroll_to(5, 6, 10, 20, 2), 4);
        assert_eq!(scroll_to(0, 8, 10, 20, 2), 1);
        assert_eq!(scroll_to(3, 7, 10, 20, 2), 3);
        // never past the last row, never more than half the height
        assert_eq!(scroll_to(0, 19, 10, 20, 2), 10);
        assert_eq!(scroll_to(0, 5, 4, 20, 9), 3); // 1 row kept in 4: rows 3 to 6
    }

    #[test]
    fn test_focused_title_without_background() {
        // like lazygit: only the text is colored
        let theme = ColorTheme::default();
        let title = focused_title("t", &theme);
        assert_eq!(title.style.bg, None);
        assert_eq!(title.style.fg, Some(theme.accent_fg));
        // a badge, with a background
        let config: crate::color::OptionalColorTheme =
            toml::from_str("panel_title_focused_bg = \"#89b4fa\"").unwrap();
        let theme: ColorTheme = config.into();
        let blue = Some(Color::Rgb(0x89, 0xb4, 0xfa));
        assert_eq!(focused_title("t", &theme).style.bg, blue);
    }
}
