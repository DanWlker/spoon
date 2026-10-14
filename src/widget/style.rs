// The looks shared by the panels, after Charm's tools (lipgloss, huh, bubbles): the title of the
// focused panel as a badge, key hints, and a mark beside the selected row.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
};

use crate::color::ColorTheme;

// The title of a panel, like "[1] Unstaged (3)": a badge while it has the focus.
// `extra`, like "· 2 picked", follows it.
pub fn panel_title(label: &str, extra: &str, focused: bool, theme: &ColorTheme) -> Line<'static> {
    let mut spans = vec![if focused {
        Span::raw(format!(" {label} "))
            .fg(theme.accent_text_fg)
            .bg(theme.accent_fg)
            .bold()
    } else {
        Span::raw(format!(" {label} ")).fg(theme.fg)
    }];
    if !extra.is_empty() {
        spans.push(Span::raw(format!(" {extra} ")).fg(theme.highlight_fg));
    }
    Line::from(spans)
}

// Keys and what they do, like "j/k move • enter push • esc cancel".
pub fn key_hints(hints: &[(&str, &str)], theme: &ColorTheme) -> Line<'static> {
    let mut spans = vec![];
    for (i, (key, description)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" • ").fg(theme.divider_fg));
        }
        spans.push(Span::raw(key.to_string()).fg(theme.fg));
        spans.push(Span::raw(format!(" {description}")).fg(theme.status_input_transient_fg));
    }
    Line::from(spans)
}

// Highlights the selected row of a list in a panel with a horizontal padding of one,
// putting a bar in that padding. A list without the focus keeps a quieter mark.
pub fn mark_selected_row(buf: &mut Buffer, inner: Rect, y: u16, focused: bool, theme: &ColorTheme) {
    if y < inner.top() || y >= inner.bottom() {
        return;
    }
    let row = Rect::new(inner.x, y, inner.width, 1);
    if focused {
        let style = Style::default()
            .fg(theme.ref_selected_fg)
            .bg(theme.ref_selected_bg);
        buf.set_style(row, style);
    } else {
        let style = Style::default()
            .fg(theme.highlight_fg)
            .add_modifier(Modifier::BOLD);
        buf.set_style(row, style);
    }
    if inner.x > 0 {
        let fg = if focused {
            theme.highlight_fg
        } else {
            theme.status_input_transient_fg
        };
        buf.set_string(inner.x - 1, y, "▌", Style::default().fg(fg));
    }
}
