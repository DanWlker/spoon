// The looks shared by the panels, after Charm's tools (lipgloss, huh, bubbles): the title of the
// focused panel as a badge, key hints, and a mark beside the selected row.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType},
};

use crate::color::ColorTheme;

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

// The width of the file tree beside a diff or a preview, in `width`.
pub fn tree_panel_width(width: u16) -> u16 {
    (width / 3).clamp(20, 50).min(width)
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(focused_title("t", &theme).style.bg, Some(theme.accent_fg));
    }
}
