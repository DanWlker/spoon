// What the mouse does, from the raw events of the terminal: clicks, double clicks (which
// terminals don't report, so they are timed here), ctrl+clicks, right clicks, drags, the wheel and
// the pointer moving. The app turns each into the events the keys send, see `App::handle_gesture`.

use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::widget::tree_query::Toward;

// The most between the presses of a double click, on the same cell.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

// What selecting a row with the mouse does to the picks of its list: they stay, as moving with
// the keys, or are cleared unless the row is picked, so the action works on the row pointed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnPicks {
    Keep,
    ClearUnlessPicked,
    // a drag starts there, picking from it: as `Keep`, a diff going into its lines to pick them
    StartRange,
}

// A cell of the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub x: u16,
    pub y: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    Click(Cell),
    DoubleClick(Cell),
    CtrlClick(Cell),
    RightClick(Cell),
    // the button held down moved off the cell pressed
    DragStart { from: Cell, to: Cell },
    DragTo(Cell),
    DragEnd(Cell),
    Wheel { at: Cell, toward: Toward },
    Hover(Cell),
}

// The left button held down.
#[derive(Debug, Clone, Copy)]
struct Press {
    at: Cell,
    when: Instant,
    ctrl: bool,
    dragging: bool,
}

#[derive(Debug, Default)]
pub struct Pointer {
    press: Option<Press>,
    // the press of the last click, which a second one on its cell soon after makes a double click
    last_click: Option<(Cell, Instant)>,
}

impl Pointer {
    // What `mouse` does, at `now`; None while it is only part of a gesture, like a press.
    pub fn gesture(&mut self, mouse: MouseEvent, now: Instant) -> Option<Gesture> {
        let cell = Cell {
            x: mouse.column,
            y: mouse.row,
        };
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.press = Some(Press {
                    at: cell,
                    when: now,
                    ctrl: mouse.modifiers.contains(KeyModifiers::CONTROL),
                    dragging: false,
                });
                None
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let press = self.press.as_mut()?;
                if press.dragging {
                    Some(Gesture::DragTo(cell))
                } else if cell != press.at {
                    press.dragging = true;
                    Some(Gesture::DragStart {
                        from: press.at,
                        to: cell,
                    })
                } else {
                    None
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let press = self.press.take()?;
                if press.dragging || press.ctrl {
                    self.last_click = None;
                    return Some(match press.dragging {
                        true => Gesture::DragEnd(cell),
                        false => Gesture::CtrlClick(press.at),
                    });
                }
                let double = self.last_click.is_some_and(|(at, when)| {
                    at == press.at && press.when.duration_since(when) < DOUBLE_CLICK
                });
                if double {
                    self.last_click = None;
                    Some(Gesture::DoubleClick(press.at))
                } else {
                    self.last_click = Some((press.at, press.when));
                    Some(Gesture::Click(press.at))
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                self.last_click = None;
                Some(Gesture::RightClick(cell))
            }
            MouseEventKind::ScrollDown => Some(Gesture::Wheel {
                at: cell,
                toward: Toward::Next,
            }),
            MouseEventKind::ScrollUp => Some(Gesture::Wheel {
                at: cell,
                toward: Toward::Previous,
            }),
            MouseEventKind::Moved => Some(Gesture::Hover(cell)),
            _ => None,
        }
    }

    // Forgets the press and the click before it: a click that only ended typing, which the next
    // one doesn't make a double click.
    pub fn cancel(&mut self) {
        self.press = None;
        self.last_click = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: MouseEventKind, x: u16, y: u16, modifiers: KeyModifiers) -> MouseEvent {
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers,
        }
    }

    // A press and a release of the left button at (x, y), `at` ms after `start`.
    fn click(pointer: &mut Pointer, start: Instant, at: u64, x: u16, y: u16) -> Option<Gesture> {
        let now = start + Duration::from_millis(at);
        let none = KeyModifiers::NONE;
        let press = event(MouseEventKind::Down(MouseButton::Left), x, y, none);
        assert_eq!(pointer.gesture(press, now), None);
        pointer.gesture(
            event(MouseEventKind::Up(MouseButton::Left), x, y, none),
            now,
        )
    }

    #[test]
    fn test_click_and_double_click() {
        let (mut pointer, start) = (Pointer::default(), Instant::now());
        let cell = Cell { x: 3, y: 4 };
        assert_eq!(
            click(&mut pointer, start, 0, 3, 4),
            Some(Gesture::Click(cell))
        );
        assert_eq!(
            click(&mut pointer, start, 300, 3, 4),
            Some(Gesture::DoubleClick(cell))
        );
        // a third is a click again, as is one too late or elsewhere
        assert_eq!(
            click(&mut pointer, start, 500, 3, 4),
            Some(Gesture::Click(cell))
        );
        assert_eq!(
            click(&mut pointer, start, 1000, 3, 4),
            Some(Gesture::Click(cell))
        );
        let other = Cell { x: 5, y: 4 };
        assert_eq!(
            click(&mut pointer, start, 1100, 5, 4),
            Some(Gesture::Click(other))
        );
    }

    #[test]
    fn test_drag_never_clicks() {
        let (mut pointer, now) = (Pointer::default(), Instant::now());
        let none = KeyModifiers::NONE;
        let at = |kind, x, y| event(kind, x, y, none);
        let drag = MouseEventKind::Drag(MouseButton::Left);
        assert_eq!(
            pointer.gesture(at(MouseEventKind::Down(MouseButton::Left), 1, 1), now),
            None
        );
        // dragging within the pressed cell is no drag yet
        assert_eq!(pointer.gesture(at(drag, 1, 1), now), None);
        assert_eq!(
            pointer.gesture(at(drag, 1, 3), now),
            Some(Gesture::DragStart {
                from: Cell { x: 1, y: 1 },
                to: Cell { x: 1, y: 3 }
            })
        );
        assert_eq!(
            pointer.gesture(at(drag, 1, 5), now),
            Some(Gesture::DragTo(Cell { x: 1, y: 5 }))
        );
        let up = at(MouseEventKind::Up(MouseButton::Left), 1, 5);
        assert_eq!(
            pointer.gesture(up, now),
            Some(Gesture::DragEnd(Cell { x: 1, y: 5 }))
        );
        // a click right after on the first cell is not a double click
        assert_eq!(
            click(&mut pointer, now, 100, 1, 1),
            Some(Gesture::Click(Cell { x: 1, y: 1 }))
        );
    }

    #[test]
    fn test_ctrl_and_right_click() {
        let (mut pointer, now) = (Pointer::default(), Instant::now());
        let cell = Cell { x: 2, y: 2 };
        let ctrl = KeyModifiers::CONTROL;
        let press = event(MouseEventKind::Down(MouseButton::Left), 2, 2, ctrl);
        assert_eq!(pointer.gesture(press, now), None);
        let up = event(MouseEventKind::Up(MouseButton::Left), 2, 2, ctrl);
        assert_eq!(pointer.gesture(up, now), Some(Gesture::CtrlClick(cell)));
        // a ctrl+click doesn't count toward a double click
        assert_eq!(
            click(&mut pointer, now, 100, 2, 2),
            Some(Gesture::Click(cell))
        );
        let right = event(
            MouseEventKind::Down(MouseButton::Right),
            2,
            2,
            KeyModifiers::NONE,
        );
        assert_eq!(pointer.gesture(right, now), Some(Gesture::RightClick(cell)));
    }

    #[test]
    fn test_wheel_hover_and_cancel() {
        let (mut pointer, now) = (Pointer::default(), Instant::now());
        let none = KeyModifiers::NONE;
        let cell = Cell { x: 7, y: 8 };
        let down = event(MouseEventKind::ScrollDown, 7, 8, none);
        assert_eq!(
            pointer.gesture(down, now),
            Some(Gesture::Wheel {
                at: cell,
                toward: Toward::Next
            })
        );
        let up = event(MouseEventKind::ScrollUp, 7, 8, none);
        assert_eq!(
            pointer.gesture(up, now),
            Some(Gesture::Wheel {
                at: cell,
                toward: Toward::Previous
            })
        );
        let moved = event(MouseEventKind::Moved, 7, 8, none);
        assert_eq!(pointer.gesture(moved, now), Some(Gesture::Hover(cell)));
        // a press taken by a border drag is forgotten: its release is nothing
        let press = event(MouseEventKind::Down(MouseButton::Left), 7, 8, none);
        assert_eq!(pointer.gesture(press, now), None);
        pointer.cancel();
        let release = event(MouseEventKind::Up(MouseButton::Left), 7, 8, none);
        assert_eq!(pointer.gesture(release, now), None);
    }
}
