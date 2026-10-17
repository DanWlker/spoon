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
// the blame: lit while it is selected.
pub fn choice_arrows(selected: bool, theme: &ColorTheme) -> [Span<'static>; 2] {
    let fg = if selected {
        theme.highlight_fg
    } else {
        theme.muted_fg
    };
    [Span::raw("‹ ").fg(fg), Span::raw(" ›").fg(fg)]
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

pub fn key_hints(hints: &[(&str, &str)], theme: &ColorTheme) -> Line<'static> {
    let mut spans = vec![];
    for (i, (key, description)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" • ").fg(theme.divider_fg));
        }
        spans.push(key_span(*key, theme));
        spans.push(Span::raw(format!(" {description}")).fg(theme.muted_fg));
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
}

impl ScrollState {
    // Keeps the offset within the `count` rows shown in `height`, the last one at the bottom; the
    // rows to render.
    pub fn fit(&mut self, count: usize, height: usize) -> std::ops::Range<usize> {
        self.height = height;
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
        let line = key_hints(&[("j/k", "move"), ("q", "quit")], &theme);
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
