use ratatui::style::Color as RatatuiColor;
use serde::Deserialize;
use smart_default::SmartDefault;
use umbra::optional;

use crate::config::GraphColorConfig;

#[optional(derives = [Deserialize], visibility = pub)]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault)]
pub struct ColorTheme {
    #[default(RatatuiColor::Reset)]
    pub fg: RatatuiColor,
    #[default(RatatuiColor::Reset)]
    pub bg: RatatuiColor,

    #[default(CREAM)]
    pub list_selected_fg: RatatuiColor,
    #[default(SELECTION_BG)]
    pub list_selected_bg: RatatuiColor,
    #[default(INK)]
    pub list_picked_fg: RatatuiColor,
    #[default(FUCHSIA)]
    pub list_picked_bg: RatatuiColor,
    #[default(MUTED)]
    pub list_ref_paren_fg: RatatuiColor,
    #[default(GREEN)]
    pub list_ref_branch_fg: RatatuiColor,
    #[default(RED)]
    pub list_ref_remote_branch_fg: RatatuiColor,
    #[default(YELLOW)]
    pub list_ref_tag_fg: RatatuiColor,
    #[default(FUCHSIA)]
    pub list_ref_stash_fg: RatatuiColor,
    #[default(CYAN)]
    pub list_head_fg: RatatuiColor,
    #[default(RatatuiColor::Reset)]
    pub list_subject_fg: RatatuiColor,
    #[default(LAVENDER)]
    pub list_name_fg: RatatuiColor,
    #[default(INDIGO)]
    pub list_hash_fg: RatatuiColor,
    #[default(MUTED)]
    pub list_date_fg: RatatuiColor,
    #[default(INK)]
    pub list_match_fg: RatatuiColor,
    #[default(YELLOW)]
    pub list_match_bg: RatatuiColor,

    #[default(MUTED)]
    pub detail_label_fg: RatatuiColor,
    #[default(RatatuiColor::Reset)]
    pub detail_name_fg: RatatuiColor,
    #[default(RatatuiColor::Reset)]
    pub detail_date_fg: RatatuiColor,
    #[default(INDIGO)]
    pub detail_email_fg: RatatuiColor,
    #[default(RatatuiColor::Reset)]
    pub detail_hash_fg: RatatuiColor,
    #[default(GREEN)]
    pub detail_ref_branch_fg: RatatuiColor,
    #[default(RED)]
    pub detail_ref_remote_branch_fg: RatatuiColor,
    #[default(YELLOW)]
    pub detail_ref_tag_fg: RatatuiColor,
    #[default(GREEN)]
    pub detail_file_change_add_fg: RatatuiColor,
    #[default(YELLOW)]
    pub detail_file_change_modify_fg: RatatuiColor,
    #[default(RED)]
    pub detail_file_change_delete_fg: RatatuiColor,
    #[default(FUCHSIA)]
    pub detail_file_change_move_fg: RatatuiColor,

    #[default(CREAM)]
    pub ref_selected_fg: RatatuiColor,
    #[default(SELECTION_BG)]
    pub ref_selected_bg: RatatuiColor,

    #[default(FUCHSIA)]
    pub help_block_title_fg: RatatuiColor,
    #[default(INDIGO)]
    pub help_key_fg: RatatuiColor,

    #[default(RatatuiColor::Reset)]
    pub virtual_cursor_fg: RatatuiColor,
    #[default(RatatuiColor::Reset)]
    pub status_input_fg: RatatuiColor,
    #[default(MUTED)]
    pub status_input_transient_fg: RatatuiColor,
    #[default(INDIGO)]
    pub status_info_fg: RatatuiColor,
    #[default(GREEN)]
    pub status_success_fg: RatatuiColor,
    #[default(YELLOW)]
    pub status_warn_fg: RatatuiColor,
    #[default(RED)]
    pub status_error_fg: RatatuiColor,

    #[default(SUBTLE)]
    pub divider_fg: RatatuiColor,

    // the focused panel: its title (as a badge) and its border
    #[default(INDIGO)]
    pub accent_fg: RatatuiColor,
    // text on the accent color
    #[default(CREAM)]
    pub accent_text_fg: RatatuiColor,
    // the mark beside the selected row
    #[default(FUCHSIA)]
    pub highlight_fg: RatatuiColor,
    #[default(STATUS_BAR_FG)]
    pub status_bar_fg: RatatuiColor,
    #[default(STATUS_BAR_BG)]
    pub status_bar_bg: RatatuiColor,
}

// The default colors, after the palette of Charm's tools (lipgloss, huh, bubbles).
const INDIGO: RatatuiColor = RatatuiColor::Rgb(0x75, 0x71, 0xF9);
const FUCHSIA: RatatuiColor = RatatuiColor::Rgb(0xF7, 0x80, 0xE2);
const CREAM: RatatuiColor = RatatuiColor::Rgb(0xFF, 0xFD, 0xF5);
const GREEN: RatatuiColor = RatatuiColor::Rgb(0x02, 0xBF, 0x87);
const RED: RatatuiColor = RatatuiColor::Rgb(0xED, 0x56, 0x7A);
const YELLOW: RatatuiColor = RatatuiColor::Rgb(0xEC, 0xCB, 0x6E);
const CYAN: RatatuiColor = RatatuiColor::Rgb(0x6E, 0xC8, 0xF0);
const LAVENDER: RatatuiColor = RatatuiColor::Rgb(0xA4, 0x9F, 0xC5);
const MUTED: RatatuiColor = RatatuiColor::Rgb(0x62, 0x62, 0x62);
const SUBTLE: RatatuiColor = RatatuiColor::Rgb(0x3A, 0x3A, 0x3A);
const INK: RatatuiColor = RatatuiColor::Rgb(0x1E, 0x1E, 0x24);
const SELECTION_BG: RatatuiColor = RatatuiColor::Rgb(0x35, 0x31, 0x4C);
const STATUS_BAR_FG: RatatuiColor = RatatuiColor::Rgb(0xC1, 0xC6, 0xB2);
const STATUS_BAR_BG: RatatuiColor = RatatuiColor::Rgb(0x2A, 0x2A, 0x2E);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphColor {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

impl GraphColor {
    pub fn from_rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Self::from_rgba(r, g, b, 255)
    }

    pub fn to_image_color(self) -> image::Rgba<u8> {
        image::Rgba([self.r, self.g, self.b, self.a])
    }

    pub fn to_ratatui_color(self) -> RatatuiColor {
        RatatuiColor::Rgb(self.r, self.g, self.b)
    }

    fn transparent() -> Self {
        Self::from_rgba(0, 0, 0, 0)
    }
}

#[derive(Debug, Clone)]
pub struct GraphColorSet {
    pub colors: Vec<GraphColor>,
    pub edge_color: GraphColor,
    pub background_color: GraphColor,
}

impl GraphColorSet {
    pub fn new(config: &GraphColorConfig) -> Self {
        let colors = config
            .branches
            .iter()
            .filter_map(|s| parse_rgba_color(s))
            .collect();
        let edge_color = parse_rgba_color(&config.edge).unwrap_or(GraphColor::transparent());
        let background_color =
            parse_rgba_color(&config.background).unwrap_or(GraphColor::transparent());

        Self {
            colors,
            edge_color,
            background_color,
        }
    }

    pub fn get(&self, index: usize) -> GraphColor {
        self.colors[index % self.colors.len()]
    }
}

fn parse_rgba_color(s: &str) -> Option<GraphColor> {
    if !s.starts_with('#') {
        return None;
    }

    let s = &s[1..];
    let l = s.len();
    if l != 6 && l != 8 {
        return None;
    }

    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    if l == 6 {
        Some(GraphColor::from_rgb(r, g, b))
    } else {
        let a = u8::from_str_radix(&s[6..8], 16).ok()?;
        Some(GraphColor::from_rgba(r, g, b, a))
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("#ff0000", Some(GraphColor { r: 255, g: 0, b: 0, a: 255}))]
    #[case("#AABBCCDD", Some(GraphColor { r: 170, g: 187, b: 204, a: 221}))]
    #[case("#ff000", None)]
    #[case("#fff", None)]
    #[case("000000", None)]
    #[case("##123456", None)]
    fn test_parse_rgba_color(#[case] input: &str, #[case] expected: Option<GraphColor>) {
        assert_eq!(parse_rgba_color(input), expected);
    }
}
