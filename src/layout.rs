// The sizes of the panels that can be resized, with ctrl and the arrow keys or by dragging their
// borders with the mouse, and the borders drawn in the last frame to drag.

use ratatui::layout::Rect;
use rustc_hash::FxHashMap;

// A size that can be changed: a width for the panels side by side, a height for those on top of
// each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Panel {
    // the width of the refs sidebar
    Sidebar,
    // the height of the commit details under the commit list
    Details,
    // the width of the file tree beside the diff or preview in the details
    DetailTree,
    // the width of the files on the local changes page
    Files,
    // the height of the unstaged files over the staged ones
    Unstaged,
    // the height of the commit message on the local changes page
    Commit,
}

// A panel's `size` within `total`, at least `min`, leaving `keep` for the panel beside it when
// there is room for both.
pub fn fit_size(size: u16, total: u16, min: u16, keep: u16) -> u16 {
    size.clamp(min, total.saturating_sub(keep).max(min))
        .min(total)
}

// Where the focus moves to, from the focused panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

// The border of a panel, where it was drawn: a vertical line is dragged left and right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Border {
    panel: Panel,
    vertical: bool,
    // the x of a vertical line, the y of a horizontal one
    at: u16,
    // where the line goes along, [from, to)
    from: u16,
    to: u16,
    // the size is measured from this edge: the left or top one going forward, else the right or
    // bottom one
    anchor: u16,
    forward: bool,
}

impl Border {
    // The size putting the border at `pos`, along the axis it moves on.
    fn size_at(&self, pos: u16) -> u16 {
        if self.forward {
            (pos + 1).saturating_sub(self.anchor)
        } else {
            self.anchor.saturating_sub(pos)
        }
    }

    // On the line, or the line of the panel next to it, which touches it.
    fn is_under(&self, x: u16, y: u16) -> bool {
        let (pos, along) = if self.vertical { (x, y) } else { (y, x) };
        pos.abs_diff(self.at) <= 1 && (self.from..self.to).contains(&along)
    }
}

#[derive(Debug, Default)]
pub struct PanelLayout {
    // the sizes set by the user, else the defaults are used
    sizes: FxHashMap<Panel, u16>,
    // the borders drawn in the last frame, with the size of their panel then
    borders: Vec<(Border, u16)>,
    dragging: Option<Border>,
    // counts the changes of the sizes, for what was drawn at the old ones to be cleared
    version: u64,
}

impl PanelLayout {
    // The size of the panel set by the user, else `default`.
    pub fn size(&self, panel: Panel, default: u16) -> u16 {
        self.sizes.get(&panel).copied().unwrap_or(default)
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    fn set_size(&mut self, panel: Panel, size: u16) {
        if self.sizes.insert(panel, size) != Some(size) {
            self.version += 1;
        }
    }

    // Forgets the borders of the last frame, before drawing the next one.
    pub fn clear_borders(&mut self) {
        self.borders.clear();
    }

    // The border of `panel` being drawn in `area`: its right side when `vertical`, else its bottom
    // side going `forward`, or its top side.
    pub fn place(&mut self, panel: Panel, area: Rect, vertical: bool, forward: bool) {
        if area.is_empty() {
            return;
        }
        let (at, from, to, anchor, size) = match (vertical, forward) {
            (true, _) => (
                area.right() - 1,
                area.top(),
                area.bottom(),
                area.left(),
                area.width,
            ),
            (false, true) => (
                area.bottom() - 1,
                area.left(),
                area.right(),
                area.top(),
                area.height,
            ),
            (false, false) => (
                area.top(),
                area.left(),
                area.right(),
                area.bottom(),
                area.height,
            ),
        };
        let border = Border {
            panel,
            vertical,
            at,
            from,
            to,
            anchor,
            forward: forward || vertical,
        };
        self.borders.push((border, size));
    }

    // Moves the border of the panel drawn in the last frame by `step` cells, to the right or
    // down when positive; returns whether there was such a border.
    pub fn move_border(&mut self, panel: Panel, step: i32) -> bool {
        let Some((border, size)) = self.borders.iter_mut().find(|(b, _)| b.panel == panel) else {
            return false;
        };
        let step = if border.forward { step } else { -step };
        // kept for the keys pressed before the next frame
        *size = (*size as i32 + step).max(1) as u16;
        let size = *size;
        self.set_size(panel, size);
        true
    }

    // Starts dragging the border under the mouse, if any; returns whether there was one.
    pub fn press(&mut self, x: u16, y: u16) -> bool {
        // the innermost border, drawn last, over those around it
        self.dragging = self
            .borders
            .iter()
            .rev()
            .find(|(b, _)| b.is_under(x, y))
            .map(|(b, _)| *b);
        self.dragging.is_some()
    }

    // Moves the border being dragged to the mouse; returns whether one is.
    pub fn drag_to(&mut self, x: u16, y: u16) -> bool {
        let Some(border) = self.dragging else {
            return false;
        };
        let pos = if border.vertical { x } else { y };
        self.set_size(border.panel, border.size_at(pos).max(1));
        true
    }

    pub fn release(&mut self) {
        self.dragging = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // the sidebar 30 wide, the details under the commit list from y 20 to the bottom at 40
    fn place(layout: &mut PanelLayout) {
        layout.place(Panel::Sidebar, Rect::new(0, 0, 30, 40), true, true);
        layout.place(Panel::Details, Rect::new(30, 20, 130, 20), false, false);
    }

    #[test]
    fn test_drag_borders() {
        let mut layout = PanelLayout::default();
        place(&mut layout);

        // the line of the panel beside the border grabs it too
        assert!(layout.press(30, 5));
        assert!(layout.drag_to(40, 7));
        assert_eq!(layout.size(Panel::Sidebar, 30), 41);
        layout.release();
        assert!(!layout.drag_to(10, 7));

        assert!(!layout.press(50, 30));
        assert!(layout.press(50, 19));
        layout.drag_to(52, 10);
        assert_eq!(layout.size(Panel::Details, 20), 30);
    }

    #[test]
    fn test_move_borders() {
        let mut layout = PanelLayout::default();
        place(&mut layout);

        // right widens the sidebar; down lowers the top of the details, which get shorter
        assert!(layout.move_border(Panel::Sidebar, 2));
        assert_eq!(layout.size(Panel::Sidebar, 0), 32);
        assert!(layout.move_border(Panel::Details, 2));
        assert_eq!(layout.size(Panel::Details, 0), 18);
        assert!(!layout.move_border(Panel::Files, 2));
    }
}
