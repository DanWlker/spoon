// Checks of the looks the panels share, run on what a widget drew, so a panel drawing a row its
// own way shows up in a test: a highlighted row has its background across the panel, selecting
// a row changes only its background, and the lines of a tree keep their color.

use std::rc::Rc;

use ratatui::{buffer::Buffer, layout::Rect, style::Color};

use crate::{
    app::AppContext,
    color::ColorTheme,
    config::{CoreConfig, UiConfig},
    keybind::KeyBind,
    protocol::ImageProtocol,
};

pub fn test_ctx() -> Rc<AppContext> {
    Rc::new(AppContext {
        keybind: KeyBind::new(None),
        core_config: CoreConfig::default(),
        ui_config: UiConfig::default(),
        color_theme: ColorTheme::default(),
        image_protocol: ImageProtocol::Kitty,
        running_task: Default::default(),
        key_names: Default::default(),
        layout: Default::default(),
        wrap: Default::default(),
    })
}

// The text of the row at `y`, to find a row by what it shows.
pub fn row_text(buf: &Buffer, y: u16) -> String {
    let area = buf.area;
    (area.left()..area.right())
        .map(|x| buf[(x, y)].symbol())
        .collect()
}

// The row at `y` showing `text`, first from the top.
pub fn find_row(buf: &Buffer, text: &str) -> u16 {
    let area = buf.area;
    (area.top()..area.bottom())
        .find(|&y| row_text(buf, y).contains(text))
        .unwrap_or_else(|| panic!("no row shows {text:?}:\n{}", dump(buf)))
}

// The row at `y` inside the panel titled `title`: between its side borders and their
// `padding`, where a list draws its rows. The panel is found by the corners around its title,
// as the lines of a tree look like its borders.
pub fn panel_row(buf: &Buffer, title: &str, y: u16, padding: u16) -> Rect {
    let top = find_row(buf, title);
    let x = column_of(buf, top, title);
    let corner = |x: u16, symbol: &str| buf[(x, top)].symbol() == symbol;
    let left = (buf.area.left()..x).rev().find(|&x| corner(x, "╭"));
    let right = (x..buf.area.right()).find(|&x| corner(x, "╮"));
    let (left, right) = left.zip(right).expect("the corners of the panel");
    let x = left + 1 + padding;
    Rect::new(x, y, right.saturating_sub(x + padding), 1)
}

// The column showing `text` in the row at `y`.
pub fn column_of(buf: &Buffer, y: u16, text: &str) -> u16 {
    let row: Vec<&str> = (buf.area.left()..buf.area.right())
        .map(|x| buf[(x, y)].symbol())
        .collect();
    let first: Vec<&str> = text.split("").filter(|s| !s.is_empty()).collect();
    (0..row.len())
        .find(|&i| row[i..].starts_with(&first))
        .map(|i| buf.area.left() + i as u16)
        .unwrap_or_else(|| panic!("row {y} doesn't show {text:?}: {}", row_text(buf, y)))
}

// A highlighted row: every cell of `row` on `bg`.
pub fn assert_row_bg(buf: &Buffer, row: Rect, bg: Color) {
    for x in row.left()..row.right() {
        assert_eq!(
            buf[(x, row.y)].bg,
            bg,
            "cell {x} of {:?} is not highlighted: {}",
            row,
            row_text(buf, row.y)
        );
    }
}

// Selecting a row changes only its background: each cell of `row` keeps its text, color and
// modifiers from `plain`, where the row isn't selected.
pub fn assert_only_bg_changed(plain: &Buffer, selected: &Buffer, row: Rect) {
    for x in row.left()..row.right() {
        let (a, b) = (&plain[(x, row.y)], &selected[(x, row.y)]);
        assert_eq!(
            (a.symbol(), a.fg, a.modifier),
            (b.symbol(), b.fg, b.modifier),
            "selecting changed cell {x} of {}",
            row_text(selected, row.y)
        );
    }
}

// The lines of a tree in `row` keep the divider color, highlighted or not.
pub fn assert_guides_keep_color(buf: &Buffer, row: Rect, theme: &ColorTheme) {
    for x in row.left()..row.right() {
        let cell = &buf[(x, row.y)];
        if matches!(cell.symbol(), "│" | "└") {
            assert_eq!(
                cell.fg,
                theme.divider_fg,
                "the line at {x} of {} changed color",
                row_text(buf, row.y)
            );
        }
    }
}

pub fn dump(buf: &Buffer) -> String {
    let area = buf.area;
    (area.top()..area.bottom())
        .map(|y| row_text(buf, y))
        .collect::<Vec<_>>()
        .join("\n")
}
