// The sizes of the panels that can be resized, with alt and the arrow keys or by dragging their
// borders with the mouse, the borders drawn in the last frame to drag, and where the rows of each
// panel were drawn, for the clicks to find what is under the pointer.

use ratatui::layout::Rect;
use rustc_hash::FxHashMap;

use crate::widget::{
    commit_tabs::DetailTab,
    git_dialog::DialogHit,
    local_changes::{CommitField, ConflictItem},
};

// What a click can focus, as drawn in the last frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Sidebar,
    Commits,
    // a tab title of the details
    DetailTab(DetailTab),
    Info,
    // the files of the changes or files tab, and the diff or preview beside them
    DetailFiles,
    DetailPane,
    // the file sections of the local changes, and the diff or conflict beside them
    Unstaged,
    Staged,
    ChangesSide,
    // a choice of the conflict panel in place of the diff, a box of the commit message, the
    // commits of a submodule in place of its diff
    Conflict(ConflictItem),
    Commit(CommitField),
    Submodule,
    // the commit shown at the top of the blame, the commits beside its lines, and the lines
    BlameRevision,
    BlameCommits,
    BlameText,
    TimelineCommits,
    TimelineDiff,
    LogCommands,
    LogOutput,
    KeysList,
    // the pages in the bar at the top
    LocalChangesPage,
    GraphPage,
    Menu,
    // a part of the popup, see `DialogHit`
    Dialog(DialogHit),
    // the [x] of what it closes, and the button committing below the commit message
    Close(Closes),
    CommitButton,
    // drawn over the panels, a notice or the keys popup: a click on it does nothing
    Covered,
}

impl Target {
    // Rows whose Enter leaves the window they are in: the commits of the blame and the timeline
    // (shown on the graph) and the actions of the keys list (run). A click there only selects, so
    // it doesn't leave by accident, and a double click does Enter.
    pub fn enter_leaves(self) -> bool {
        matches!(
            self,
            Target::BlameRevision
                | Target::BlameCommits
                | Target::TimelineCommits
                | Target::KeysList
        )
    }
}

// What an [x] closes, see `render_close_button`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closes {
    Details,
    // the window over the pages: the blame, the timeline, the command log or the keys list
    Window,
    KeysPopup,
    // showing one branch's commits only, back to all of them
    BranchOnly,
}

// The rows of a panel on the lines of its area: none, one (a button), from the row shown first
// (`count` in all), or line by line for text whose rows wrap over several lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowMap {
    None,
    One,
    Rows { first: usize, count: usize },
    Lines(Vec<Option<usize>>),
}

// What is under the pointer: the target, and its row there, None past its rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub target: Target,
    pub row: Option<usize>,
    // the cells from the left of what is hit, like into the text of a field
    pub column: usize,
}

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
    // the width of the commits beside the lines in the blame, with the line between them
    BlameCommits,
    // the width of the commits beside the diff in the timeline
    TimelineCommits,
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

// What the resize keys change of a panel: its width with alt-left and alt-right, its height
// with alt-up and alt-down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    Width,
    Height,
}

// The edge of a panel its border is on, which dragging resizes it by: the right one is dragged
// left and right, the bottom and top ones up and down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Right,
    Bottom,
    Top,
}

// The border of a panel, where it was drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Border {
    panel: Panel,
    edge: Edge,
    // the x of a vertical line, the y of a horizontal one
    at: u16,
    // where the line goes along, [from, to)
    from: u16,
    to: u16,
    // the size is measured from the edge across the panel: the left one for the right edge, the
    // top one for the bottom edge, the bottom one for the top edge
    anchor: u16,
}

impl Border {
    fn vertical(&self) -> bool {
        self.edge == Edge::Right
    }

    // Moving the border right or down grows the panel.
    fn forward(&self) -> bool {
        self.edge != Edge::Top
    }

    // The size putting the border at `pos`, along the axis it moves on.
    fn size_at(&self, pos: u16) -> u16 {
        if self.forward() {
            (pos + 1).saturating_sub(self.anchor)
        } else {
            self.anchor.saturating_sub(pos)
        }
    }

    // On the line, or the line of the panel next to it, which touches it.
    fn is_under(&self, x: u16, y: u16) -> bool {
        let (pos, along) = if self.vertical() { (x, y) } else { (y, x) };
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
    // where each target was drawn in the last frame, see `hit`
    hits: Vec<(Target, Rect, RowMap)>,
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

    // `target` drawn in `area`, its rows on its lines as `rows` says.
    pub fn record(&mut self, target: Target, area: Rect, rows: RowMap) {
        if !area.is_empty() {
            self.hits.push((target, area, rows));
        }
    }

    // What was drawn last at the cell, the innermost of what is drawn over each other.
    pub fn hit(&self, x: u16, y: u16) -> Option<Hit> {
        let (target, area, rows) = self
            .hits
            .iter()
            .rev()
            .find(|(_, area, _)| area.contains((x, y).into()))?;
        let line = (y - area.y) as usize;
        let row = match rows {
            RowMap::None => None,
            RowMap::One => Some(0),
            RowMap::Rows { first, count } => first.checked_add(line).filter(|row| row < count),
            RowMap::Lines(lines) => lines.get(line).copied().flatten(),
        };
        Some(Hit {
            target: *target,
            row,
            column: (x - area.x) as usize,
        })
    }

    // Forgets what was drawn, once drawn again or once what it showed changed.
    pub fn clear_hits(&mut self) {
        self.hits.clear();
    }

    // The border of `panel` being drawn in `area`: its right side when `vertical`, else its bottom
    // side going `forward`, or its top side.
    pub fn place(&mut self, panel: Panel, area: Rect, edge: Edge) {
        if area.is_empty() {
            return;
        }
        let (at, from, to, anchor, size) = match edge {
            Edge::Right => (
                area.right() - 1,
                area.top(),
                area.bottom(),
                area.left(),
                area.width,
            ),
            Edge::Bottom => (
                area.bottom() - 1,
                area.left(),
                area.right(),
                area.top(),
                area.height,
            ),
            Edge::Top => (
                area.top(),
                area.left(),
                area.right(),
                area.bottom(),
                area.height,
            ),
        };
        let border = Border {
            panel,
            edge,
            at,
            from,
            to,
            anchor,
        };
        self.borders.push((border, size));
    }

    // Moves the border of the panel drawn in the last frame by `step` cells, to the right or
    // down when positive; returns whether there was such a border.
    pub fn move_border(&mut self, panel: Panel, step: i32) -> bool {
        let Some((border, size)) = self.borders.iter_mut().find(|(b, _)| b.panel == panel) else {
            return false;
        };
        let step = if border.forward() { step } else { -step };
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
        let pos = if border.vertical() { x } else { y };
        self.set_size(border.panel, border.size_at(pos).max(1));
        true
    }

    // Stops dragging a border; returns whether one was.
    pub fn release(&mut self) -> bool {
        self.dragging.take().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // the sidebar 30 wide, the details under the commit list from y 20 to the bottom at 40
    fn place(layout: &mut PanelLayout) {
        layout.place(Panel::Sidebar, Rect::new(0, 0, 30, 40), Edge::Right);
        layout.place(Panel::Details, Rect::new(30, 20, 130, 20), Edge::Top);
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

    #[test]
    fn test_hit_maps_screen_lines_to_rows() {
        let mut layout = PanelLayout::default();
        let rows = RowMap::Rows {
            first: 10,
            count: 12,
        };
        layout.record(Target::Commits, Rect::new(2, 5, 20, 4), rows);
        let hit = |x, y| layout.hit(x, y);
        assert_eq!(
            hit(2, 5),
            Some(Hit {
                target: Target::Commits,
                row: Some(10),
                column: 0
            })
        );
        assert_eq!(
            hit(21, 6),
            Some(Hit {
                target: Target::Commits,
                row: Some(11),
                column: 19
            })
        );
        // past the rows, in the panel: no row
        assert_eq!(
            hit(3, 7),
            Some(Hit {
                target: Target::Commits,
                row: None,
                column: 1
            })
        );
        assert_eq!(hit(22, 5), None);
        assert_eq!(hit(2, 9), None);
        assert_eq!(hit(1, 5), None);
    }

    #[test]
    fn test_hit_takes_the_last_drawn() {
        let mut layout = PanelLayout::default();
        let no_rows = RowMap::Rows { first: 0, count: 0 };
        layout.record(Target::Commits, Rect::new(0, 0, 20, 10), no_rows);
        layout.record(
            Target::Sidebar,
            Rect::new(5, 5, 5, 2),
            RowMap::Rows { first: 0, count: 2 },
        );
        assert_eq!(
            layout.hit(6, 6),
            Some(Hit {
                target: Target::Sidebar,
                row: Some(1),
                column: 1
            })
        );
        assert_eq!(
            layout.hit(1, 1),
            Some(Hit {
                target: Target::Commits,
                row: None,
                column: 1
            })
        );
        layout.clear_hits();
        assert_eq!(layout.hit(6, 6), None);
    }

    #[test]
    fn test_hit_lines_for_wrapped_text() {
        let mut layout = PanelLayout::default();
        // row 4 wraps over two lines
        let lines = RowMap::Lines(vec![Some(3), Some(4), Some(4), None]);
        layout.record(Target::DetailPane, Rect::new(0, 10, 30, 4), lines);
        assert_eq!(layout.hit(0, 12).and_then(|h| h.row), Some(4));
        assert_eq!(layout.hit(0, 13).and_then(|h| h.row), None);
        layout.record(Target::Info, Rect::new(0, 0, 5, 5), RowMap::None);
        assert_eq!(
            layout.hit(1, 1),
            Some(Hit {
                target: Target::Info,
                row: None,
                column: 1
            })
        );
    }

    #[test]
    fn test_hits_at_zero_size() {
        let mut layout = PanelLayout::default();
        layout.record(
            Target::Commits,
            Rect::new(3, 3, 0, 5),
            RowMap::Rows { first: 0, count: 9 },
        );
        let no_rows = RowMap::Rows { first: 0, count: 0 };
        layout.record(Target::Sidebar, Rect::new(3, 3, 5, 0), no_rows);
        assert_eq!(layout.hit(3, 3), None);
        // near the largest coordinates
        let edge = Rect::new(u16::MAX - 1, u16::MAX - 1, 1, 1);
        layout.record(
            Target::Sidebar,
            edge,
            RowMap::Rows {
                first: usize::MAX,
                count: usize::MAX,
            },
        );
        assert_eq!(
            layout.hit(u16::MAX - 1, u16::MAX - 1).map(|h| h.target),
            Some(Target::Sidebar)
        );
    }
}
