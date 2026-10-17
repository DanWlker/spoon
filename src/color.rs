use ratatui::style::Color as RatatuiColor;
use serde::Deserialize;
use smart_default::SmartDefault;
use umbra::optional;

use crate::config::GraphColorConfig;

#[optional(derives = [Deserialize], visibility = pub)]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault)]
pub struct ColorTheme {
    #[default(TEXT)]
    pub fg: RatatuiColor,
    #[default(RatatuiColor::Reset)]
    pub bg: RatatuiColor,

    #[default(TEXT)]
    pub list_selected_fg: RatatuiColor,
    // Overlay 2 at 25% over Base, the style guide's selection
    #[default(RatatuiColor::Rgb(0x3B, 0x3D, 0x4F))]
    pub list_selected_bg: RatatuiColor,
    // text on an accent color is Base, in the style guide
    #[default(BASE)]
    pub list_picked_fg: RatatuiColor,
    #[default(MAUVE)]
    pub list_picked_bg: RatatuiColor,
    #[default(BLUE)]
    pub list_ref_branch_fg: RatatuiColor,
    // the checked-out branch
    #[default(GREEN)]
    pub list_ref_head_fg: RatatuiColor,
    #[default(BLUE)]
    pub list_ref_remote_branch_fg: RatatuiColor,
    #[default(YELLOW)]
    pub list_ref_tag_fg: RatatuiColor,
    #[default(MAUVE)]
    pub list_ref_stash_fg: RatatuiColor,
    #[default(TEXT)]
    pub list_subject_fg: RatatuiColor,
    #[default(TEXT)]
    pub list_name_fg: RatatuiColor,
    #[default(TEXT)]
    pub list_hash_fg: RatatuiColor,
    #[default(TEXT)]
    pub list_date_fg: RatatuiColor,
    // the text of commits that are not on the current branch
    #[default(OVERLAY0)]
    pub list_dimmed_fg: RatatuiColor,
    // a search match: Text on Teal, here Teal at 30% over Base to keep Text readable
    #[default(TEXT)]
    pub list_match_fg: RatatuiColor,
    #[default(RatatuiColor::Rgb(0x41, 0x59, 0x60))]
    pub list_match_bg: RatatuiColor,

    #[default(SUBTEXT0)]
    pub detail_label_fg: RatatuiColor,
    #[default(TEXT)]
    pub detail_name_fg: RatatuiColor,
    #[default(TEXT)]
    pub detail_date_fg: RatatuiColor,
    #[default(BLUE)]
    pub detail_email_fg: RatatuiColor,
    #[default(TEXT)]
    pub detail_hash_fg: RatatuiColor,
    #[default(GREEN)]
    pub detail_file_change_add_fg: RatatuiColor,
    // yellow like git's, not the style guide's Blue for changed lines
    #[default(YELLOW)]
    pub detail_file_change_modify_fg: RatatuiColor,
    #[default(RED)]
    pub detail_file_change_delete_fg: RatatuiColor,
    #[default(MAUVE)]
    pub detail_file_change_move_fg: RatatuiColor,
    // the names of the directories in the file trees
    #[default(BLUE)]
    pub file_tree_dir_fg: RatatuiColor,
    // the "@@ -1 +1 @@" headers of the hunks of a diff
    #[default(BLUE)]
    pub diff_hunk_fg: RatatuiColor,
    // the lines added and removed in a diff, Mocha's green and red over its base
    #[default(RatatuiColor::Rgb(0x39, 0x45, 0x45))]
    pub diff_add_bg: RatatuiColor,
    #[default(RatatuiColor::Rgb(0x49, 0x34, 0x46))]
    pub diff_delete_bg: RatatuiColor,
    // the part of a changed line that differs from the line it replaces, stronger
    #[default(RatatuiColor::Rgb(0x4E, 0x63, 0x56))]
    pub diff_add_word_bg: RatatuiColor,
    #[default(RatatuiColor::Rgb(0x69, 0x44, 0x59))]
    pub diff_delete_word_bg: RatatuiColor,

    #[default(MAUVE)]
    pub help_block_title_fg: RatatuiColor,

    #[default(ROSEWATER)]
    pub virtual_cursor_fg: RatatuiColor,
    // quieter text: hints, placeholders, line numbers, summaries
    #[default(OVERLAY1)]
    pub muted_fg: RatatuiColor,
    #[default(BLUE)]
    pub status_info_fg: RatatuiColor,
    #[default(GREEN)]
    pub status_success_fg: RatatuiColor,
    #[default(YELLOW)]
    pub status_warn_fg: RatatuiColor,
    #[default(RED)]
    pub status_error_fg: RatatuiColor,

    // the borders of the panels without the focus, and the lines of the trees
    #[default(OVERLAY0)]
    pub divider_fg: RatatuiColor,

    // the focused panel: its border, and its title unless panel_title_focused_* say otherwise
    #[default(LAVENDER)]
    pub accent_fg: RatatuiColor,
    // text on the accent color
    #[default(BASE)]
    pub accent_text_fg: RatatuiColor,
    // the title of the focused panel: colored text, like lazygit; with a background, a badge
    #[default(LAVENDER)]
    pub panel_title_focused_fg: RatatuiColor,
    #[default(RatatuiColor::Reset)]
    pub panel_title_focused_bg: RatatuiColor,
    // the mark beside the selected row
    #[default(MAUVE)]
    pub highlight_fg: RatatuiColor,
    #[default(SUBTEXT0)]
    pub status_bar_fg: RatatuiColor,
    #[default(RatatuiColor::Reset)]
    pub status_bar_bg: RatatuiColor,
}

// The default colors: Catppuccin Mocha, https://catppuccin.com/palette, used as its style guide
// says, https://github.com/catppuccin/catppuccin/blob/main/docs/style-guide.md
const TEXT: RatatuiColor = RatatuiColor::Rgb(0xCD, 0xD6, 0xF4);
const SUBTEXT0: RatatuiColor = RatatuiColor::Rgb(0xA6, 0xAD, 0xC8);
const OVERLAY1: RatatuiColor = RatatuiColor::Rgb(0x7F, 0x84, 0x9C);
const OVERLAY0: RatatuiColor = RatatuiColor::Rgb(0x6C, 0x70, 0x86);
const BASE: RatatuiColor = RatatuiColor::Rgb(0x1E, 0x1E, 0x2E);
const ROSEWATER: RatatuiColor = RatatuiColor::Rgb(0xF5, 0xE0, 0xDC);
const LAVENDER: RatatuiColor = RatatuiColor::Rgb(0xB4, 0xBE, 0xFE);
const BLUE: RatatuiColor = RatatuiColor::Rgb(0x89, 0xB4, 0xFA);
const MAUVE: RatatuiColor = RatatuiColor::Rgb(0xCB, 0xA6, 0xF7);
const GREEN: RatatuiColor = RatatuiColor::Rgb(0xA6, 0xE3, 0xA1);
const YELLOW: RatatuiColor = RatatuiColor::Rgb(0xF9, 0xE2, 0xAF);
const RED: RatatuiColor = RatatuiColor::Rgb(0xF3, 0x8B, 0xA8);

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
