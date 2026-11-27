// The looks shared by the panels, after Charm's tools (lipgloss, huh, bubbles): the title of the
// focused panel as a badge, key hints, and a mark beside the selected row.

use std::borrow::Cow;
use std::ops::Range;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Padding, Paragraph, Widget},
};

use crate::{
    background::shows_loading,
    color::ColorTheme,
    git::is_kept_byte,
    keybind::UserEvent,
    layout::{RowMap, Thumb},
};

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

// The note in the title of a panel loading on another thread since then, what it showed before
// staying meanwhile; only once it takes long, see `shows_loading`.
pub fn loading_note(since: Option<Instant>) -> Option<String> {
    since
        .filter(|&since| shows_loading(since))
        .map(|_| "loading…".to_string())
}

// The title of a panel, like "[1] Unstaged (3)": a badge while it has the focus.
// `notes` follow it, in this order on every panel: what it shows (a count, a path, "main only"),
// what is picked or checked, the search or filter, then whether it loads.
pub fn panel_title(
    label: &str,
    notes: impl IntoIterator<Item = String>,
    focused: bool,
    theme: &ColorTheme,
) -> Line<'static> {
    warned_panel_title(label, &[], notes, focused, theme)
}

// A title with `badge`, the state of what it shows like "conflicted", after the label and before
// the notes, see `panel_title`.
pub fn badged_panel_title(
    label: &str,
    badge: Span<'static>,
    notes: impl IntoIterator<Item = String>,
    focused: bool,
    theme: &ColorTheme,
) -> Line<'static> {
    let mut title = panel_title(label, notes, focused, theme);
    // the label is the first span
    title.spans.insert(1, badge);
    title
}

// A title with `warnings`, states to mind like "amending" or "rebasing", in the warning color: the
// first of its notes, being about what the panel shows, see `panel_title`.
pub fn warned_panel_title(
    label: &str,
    warnings: &[&str],
    notes: impl IntoIterator<Item = String>,
    focused: bool,
    theme: &ColorTheme,
) -> Line<'static> {
    // a title can name a file, or a branch git put on conflict markers
    let label = tabs_shown(label);
    let mut spans = vec![if focused {
        focused_title(format!(" {label} "), theme)
    } else {
        Span::raw(format!(" {label} ")).fg(theme.fg)
    }];
    spans.extend(title_notes(warnings, notes, theme));
    Line::from(spans)
}

// The note of a panel picking rows, how many are picked; None with none.
pub fn picked_note(count: usize) -> Option<String> {
    (count > 0).then(|| format!("{count} picked"))
}

// The notes after a panel's title, see `warned_panel_title`: shared with the titles of the tabs of
// the details, which have no single label.
pub fn title_notes(
    warnings: &[&str],
    notes: impl IntoIterator<Item = String>,
    theme: &ColorTheme,
) -> Vec<Span<'static>> {
    let warn = |text: &str| Span::raw(text.to_string()).fg(theme.status_warn_fg);
    let note = |text: String| Span::raw(text).fg(theme.highlight_fg);
    let notes = (warnings.iter().map(|w| warn(w)))
        .chain(notes.into_iter().map(|n| note(tabs_shown(&n).into_owned())));
    let mut spans = vec![];
    for span in notes {
        spans.push(note(if spans.is_empty() { " " } else { " · " }.into()));
        spans.push(span);
    }
    if !spans.is_empty() {
        spans.push(note(" ".into()));
    }
    spans
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
) -> (Rect, Rect) {
    // text plus padding and borders
    let text_width = lines.iter().map(Line::width).max().unwrap_or(0);
    let width = (text_width.clamp(widths.0, widths.1) as u16 + 4).min(area.width);
    let height = u16::try_from(lines.len()).map_or(u16::MAX, |n| n.saturating_add(2));
    let height = height.min(area.height);
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
    (popup, inner)
}

// Buttons for the mouse on the bottom border of `area`, at the right before its last "─╯", a
// space around each: their areas, none when the border is too narrow for them.
pub fn render_border_buttons(
    buf: &mut Buffer,
    area: Rect,
    buttons: &[(&str, Style)],
    theme: &ColorTheme,
) -> Vec<Rect> {
    let width = buttons
        .iter()
        .map(|(text, _)| text.len() + 1)
        .sum::<usize>()
        + 1;
    if area.height < 2 || (area.width as usize) < width + 3 {
        return vec![];
    }
    let y = area.bottom() - 1;
    let mut x = area.right() - 2 - width as u16;
    let space = Style::default().fg(theme.fg);
    let mut areas = vec![];
    for (text, style) in buttons {
        buf.set_string(x, y, " ", space);
        x += 1;
        buf.set_string(x, y, text, *style);
        areas.push(Rect::new(x, y, text.len() as u16, 1));
        x += text.len() as u16;
    }
    buf.set_string(x, y, " ", space);
    areas
}

// A checkbox's mark, the same in the popups and the commit message; `unchecked` styles the empty one.
pub fn check_mark(checked: bool, unchecked: Style, theme: &ColorTheme) -> Span<'static> {
    if checked {
        Span::raw("[•] ").fg(theme.status_success_fg).bold()
    } else {
        Span::styled("[ ] ", unchecked)
    }
}

// The [x] closing a panel or popup, on the top border of `area` right before its corner: its area,
// none when the border is too narrow for it.
pub fn render_close_button(buf: &mut Buffer, area: Rect, theme: &ColorTheme) -> Option<Rect> {
    const CLOSE: &str = "[x]";
    if area.height < 2 || area.width < 12 {
        return None;
    }
    let at = Rect::new(
        area.right() - 1 - CLOSE.len() as u16,
        area.y,
        CLOSE.len() as u16,
        1,
    );
    buf.set_string(at.x, at.y, CLOSE, Style::default().fg(theme.fg));
    Some(at)
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
) -> Option<(Rect, RowMap)> {
    let height = column.height as usize;
    if height == 0 || total <= height {
        return None;
    }
    let track = Thumb {
        track: column,
        total,
        offset,
    };
    let (thumb, top) = (track.length(), track.top());
    let fg = if focused {
        theme.accent_fg
    } else {
        theme.divider_fg
    };
    let style = Style::default().fg(fg);
    for y in top..top + thumb {
        buf.set_string(column.x, column.y + y as u16, "█", style);
    }
    // for the mouse to drag, recorded by the caller under its panel, see `Thumb`
    let area = Rect::new(column.x, column.y + top as u16, 1, thumb as u16);
    Some((area, RowMap::Thumb(track)))
}

// The rows a notch of the mouse wheel scrolls, as lazygit's.
pub const WHEEL_ROWS: usize = 2;

// The rows a list shows: around its selection, or where the wheel scrolled them, see
// `ListCursor::wheel`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Shown {
    #[default]
    Selection,
    Scrolled,
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

    // Scrolls as little as can be to show the screen `rows` with `scrolloff` rows around them, see
    // `scroll_to`; not counted here, rendering keeps the offset within them, see `fit`.
    pub fn show_rows(&mut self, rows: Range<usize>) {
        self.offset = scroll_to(self.offset, rows, self.height, usize::MAX, self.scrolloff);
    }

    // Scrolls by the scroll keys: a line, a page of the rows shown, or half of one; returns
    // whether `event` was one of them.
    pub fn handle_scroll(&mut self, event: UserEvent) -> bool {
        let rows = match event {
            UserEvent::ScrollDown => 1,
            UserEvent::ScrollUp => -1,
            UserEvent::Wheel(rows) => rows,
            // as everywhere, see `page_rows`
            _ => match page_rows(event, self.height) {
                Some(rows) => rows,
                None => return false,
            },
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

// The rows the page keys scroll a window `height` rows high: a page with ctrl-f and ctrl-b, half
// of one with ctrl-d and ctrl-u, down or up; None for other events.
pub fn page_rows(event: UserEvent, height: usize) -> Option<isize> {
    let page = height.max(1) as isize;
    match event {
        UserEvent::PageDown => Some(page),
        UserEvent::PageUp => Some(-page),
        UserEvent::HalfPageDown => Some((page / 2).max(1)),
        UserEvent::HalfPageUp => Some(-(page / 2).max(1)),
        _ => None,
    }
}

// Vim's ctrl-d/u and ctrl-f/b, in lists and in text alike: the window of `height` rows from
// `offset` scrolls `rows`, and the selection at row `selected` moves as many, keeping its row on
// screen, out of the `scrolloff` rows at an edge the window can still scroll past. At either end
// the window stops and the selection goes on, to the first or last of the `total` rows. Returns
// the first row shown and the row selected.
pub fn scroll_with_selection(
    offset: usize,
    selected: usize,
    rows: isize,
    height: usize,
    total: usize,
    scrolloff: usize,
) -> (usize, usize) {
    let offset = offset
        .saturating_add_signed(rows)
        .min(total.saturating_sub(height.max(1)));
    let selected = selected.saturating_add_signed(rows);
    let rows = selection_rows(offset, height, total, scrolloff);
    (offset, selected.max(rows.start).min(rows.end - 1))
}

// The rows of a window of `height` rows from `offset`, of `total`, the selection may be on, as
// vim's: out of the `scrolloff` rows at an edge the window can still scroll past, and in the
// list. Never empty.
pub fn selection_rows(
    offset: usize,
    height: usize,
    total: usize,
    scrolloff: usize,
) -> Range<usize> {
    let height = height.max(1);
    let last_offset = total.saturating_sub(height);
    let margin = scrolloff.min((height - 1) / 2);
    let start = match offset > 0 {
        true => offset + margin,
        false => offset,
    };
    let end = match offset < last_offset {
        true => offset + height - margin,
        false => offset + height,
    };
    start.min(total.saturating_sub(1))..end.min(total).max(start + 1)
}

// The first row shown of a list of `total` rows, `height` high, moved as little as can be from
// `offset` to show `rows` (the selected row, or a hunk) with `scrolloff` rows above and below
// them where there are, like vim's: at most half the height, their top first when they don't fit,
// and never past the last row.
pub fn scroll_to(
    offset: usize,
    rows: Range<usize>,
    height: usize,
    total: usize,
    scrolloff: usize,
) -> usize {
    let height = height.max(1);
    let margin = scrolloff.min((height - 1) / 2);
    offset
        .max((rows.end + margin).saturating_sub(height))
        .min(rows.start.saturating_sub(margin))
        .min(total.saturating_sub(height))
}

// Where a visual selection started, the selection itself having moved from there. Lists and file
// text keep it here, so the same keys start, extend and end a selection everywhere.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct VisualSelection {
    anchor: Option<usize>,
}

impl VisualSelection {
    pub fn is_visual(&self) -> bool {
        self.anchor.is_some()
    }

    // Starts a visual selection at `selected`, or ends it; true when it started.
    pub fn toggle(&mut self, selected: usize) -> bool {
        if self.anchor.take().is_some() {
            return false;
        }
        self.anchor = Some(selected);
        true
    }

    pub fn cancel(&mut self) {
        self.anchor = None;
    }

    // The rows from where the selection started to `selected`, none when it isn't visual or there
    // are no rows; the ends are kept within `total`, so a stale selection cannot index past them.
    pub fn range(&self, selected: usize, total: usize) -> Option<Range<usize>> {
        let anchor = self.anchor?.min(total.checked_sub(1)?);
        let selected = selected.min(total - 1);
        let (start, end) = (anchor.min(selected), anchor.max(selected));
        Some(start..end + 1)
    }
}

// The selected row of a list and the rows shown from `offset`, `height` at a time: the commit list,
// the file trees, the command log and the timeline. The selection moves keep it in view with
// `scrolloff` rows around it, scrolling as little as can be, see `scroll_to`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ListCursor {
    selected: usize,
    offset: usize,
    shown: Shown,
    // as last drawn, 0 before
    height: usize,
    scrolloff: usize,
    visual: VisualSelection,
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

    // The row of the selection among those shown; None when the wheel scrolled it off them.
    pub fn row(&self) -> Option<usize> {
        let row = self.selected.checked_sub(self.offset)?;
        (self.height == 0 || row < self.height).then_some(row)
    }

    // Whether the selection is a visual one, which the movement keys extend.
    pub fn is_visual(&self) -> bool {
        self.visual.is_visual()
    }

    // Starts a visual selection at the selected row, or ends it where it is.
    pub fn toggle_visual(&mut self) {
        self.visual.toggle(self.selected);
    }

    pub fn cancel_visual(&mut self) {
        self.visual.cancel();
    }

    // The row the visual selection started on; None when it isn't visual.
    pub fn visual_start(&self) -> Option<usize> {
        self.visual.anchor
    }

    // Moves the start of a visual selection to `start`, the row its item is on once the rows
    // changed, or starts one there; None ends it, like when its item is no longer shown.
    pub fn set_visual_start(&mut self, start: Option<usize>) {
        self.visual.anchor = start;
    }

    // The rows a visual selection covers, from where it started to the selection; None when it
    // isn't visual. `total` keeps the ends within the rows now shown.
    pub fn visual_range(&self, total: usize) -> Option<Range<usize>> {
        self.visual.range(self.selected, total)
    }

    // Selects the row at `index` of `total`, or the last one.
    pub fn select(&mut self, index: usize, total: usize) {
        self.selected = index.min(total.saturating_sub(1));
        self.shown = Shown::Selection;
        // before the first drawing the rows shown are unknown, which `fit` scrolls to then
        if self.height > 0 {
            let (offset, height) = (self.offset, self.height);
            self.offset = scroll_to(offset, self.row_range(), height, total, self.scrolloff);
        }
    }

    // Selects the row at `index`, shown at `row` of the rows where it can be: a list shown again
    // as it was, or a match jumped to.
    pub fn select_at(&mut self, index: usize, row: usize, total: usize) {
        self.selected = index.min(total.saturating_sub(1));
        self.shown = Shown::Selection;
        self.offset = self
            .selected
            .saturating_sub(row.min(self.height.saturating_sub(1)))
            .min(total.saturating_sub(self.height));
    }

    // The row the movement keys select in `total` rows: j and k (and J and K), g and G; None for
    // other events.
    pub fn moved_to(&self, event: UserEvent, total: usize) -> Option<usize> {
        let last = total.checked_sub(1)?;
        match event {
            UserEvent::NavigateDown | UserEvent::SelectDown => Some((self.selected + 1).min(last)),
            UserEvent::NavigateUp | UserEvent::SelectUp => Some(self.selected.saturating_sub(1)),
            UserEvent::GoToTop => Some(0),
            UserEvent::GoToBottom => Some(last),
            _ => None,
        }
    }

    // Moves by every key scrolling a list: the movement keys, ctrl-d and ctrl-u, and the page and
    // line keys; returns whether `event` was one of them.
    pub fn navigate(&mut self, event: UserEvent, total: usize) -> bool {
        match event {
            UserEvent::ScrollDown => self.scroll_lines(1, total),
            UserEvent::ScrollUp => self.scroll_lines(-1, total),
            UserEvent::Wheel(rows) => self.wheel(rows, total),
            UserEvent::PageDown
            | UserEvent::PageUp
            | UserEvent::HalfPageDown
            | UserEvent::HalfPageUp => self.scroll_pages(event, total),
            _ => {
                let Some(index) = self.moved_to(event, total) else {
                    return false;
                };
                self.select(index, total);
            }
        }
        true
    }

    // Scrolls a row at a time with ctrl-e and ctrl-y, the selection staying put until it would
    // leave the `scrolloff` rows at the edge, then moving along.
    pub fn scroll_lines(&mut self, rows: isize, total: usize) {
        self.shown = Shown::Selection;
        // before the first draw too, the selected row stays at or below the first shown
        let last_offset = total.saturating_sub(self.height.max(1));
        self.offset = self.offset.saturating_add_signed(rows).min(last_offset);
        let rows = selection_rows(self.offset, self.height, total, self.scrolloff);
        self.selected = self.selected.max(rows.start).min(rows.end - 1);
    }

    // Scrolls a page at a time with ctrl-f and ctrl-b, half of one with ctrl-d and ctrl-u, the
    // selection moving as much, see `scroll_with_selection`.
    pub fn scroll_pages(&mut self, event: UserEvent, total: usize) {
        // before the first draw the rows shown are unknown
        let Some(rows) = page_rows(event, self.height).filter(|_| self.height > 0) else {
            return;
        };
        self.shown = Shown::Selection;
        (self.offset, self.selected) = scroll_with_selection(
            self.offset,
            self.selected,
            rows,
            self.height,
            total,
            self.scrolloff,
        );
    }

    // Scrolls the rows shown alone, by the wheel: the selection stays, even off them, until it
    // moves.
    fn wheel(&mut self, rows: isize, total: usize) {
        self.show_from(self.offset.saturating_add_signed(rows), total);
    }

    // Shows the rows from `offset` whatever is selected, as the wheel left them: a list shown
    // again so after a reload.
    pub fn show_from(&mut self, offset: usize, total: usize) {
        self.shown = Shown::Scrolled;
        self.offset = offset.min(total.saturating_sub(self.height.max(1)));
    }

    // Shows `total` rows in `height`, keeping the selection in view unless the wheel scrolled
    // away from it: the first row shown.
    pub fn fit(&mut self, height: usize, total: usize, scrolloff: usize) -> usize {
        self.height = height;
        self.scrolloff = scrolloff;
        self.selected = self.selected.min(total.saturating_sub(1));
        self.offset = match self.shown {
            Shown::Selection => scroll_to(self.offset, self.row_range(), height, total, scrolloff),
            Shown::Scrolled => self.offset.min(total.saturating_sub(height)),
        };
        self.offset
    }

    fn row_range(&self) -> Range<usize> {
        self.selected..self.selected + 1
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
) -> Option<(Rect, RowMap)> {
    if area.width > 0 && area.height > 2 {
        let column = Rect::new(area.right() - 1, area.y + 1, 1, area.height - 2);
        return render_scrollbar(buf, column, total, offset, focused, theme);
    }
    None
}

// Text as it is drawn: ratatui drops control characters, so a tab, in a commit message, a file
// name or the output of git, shows as four spaces; a byte of a name that isn't UTF-8 as U+FFFD, see
// `text_from_git`. What is copied or given to git stays as it is.
pub fn tabs_shown(text: &str) -> Cow<'_, str> {
    if !text.chars().any(|c| c == '\t' || is_kept_byte(c)) {
        return text.into();
    }
    text.chars()
        .fold(String::with_capacity(text.len()), |mut shown, c| {
            match c {
                '\t' => shown.push_str("    "),
                c if is_kept_byte(c) => shown.push('\u{FFFD}'),
                c => shown.push(c),
            }
            shown
        })
        .into()
}

// The columns `c` takes once drawn, see `tabs_shown`.
pub fn shown_width(c: char) -> usize {
    match c {
        '\t' => 4,
        c => unicode_width::UnicodeWidthChar::width(c).unwrap_or(0),
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

    // A title naming a file draws it like the rows do: its tabs as spaces, a byte that isn't
    // UTF-8 as U+FFFD, which a terminal used to leave out.
    #[test]
    fn test_panel_title_draws_names_like_the_rows() {
        let theme = ColorTheme::default();
        let name = crate::git::text_from_git(b"caf\xe9\t.txt");
        let title = panel_title(&name, [name.clone()], false, &theme);
        let text: String = title.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, " caf\u{FFFD}    .txt  caf\u{FFFD}    .txt ");
    }

    #[test]
    fn test_loading_note_only_once_it_takes_long() {
        let now = Instant::now();
        let before = now
            .checked_sub(crate::background::LOADING_SHOWN_AFTER)
            .unwrap();
        assert_eq!(loading_note(None), None);
        assert_eq!(loading_note(Some(now)), None);
        assert_eq!(loading_note(Some(before)), Some("loading…".into()));
    }

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
                            let o =
                                scroll_to(offset, selected..selected + 1, height, total, scrolloff);
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

    // ctrl-d and ctrl-u scroll a list with its selection, like vim's and like a file's text: the
    // selection keeps its row on screen, out of the `scrolloff` rows at an edge the list can still
    // scroll past; at either end the list stops and the selection goes on. Lists used to move the
    // selection alone.
    #[test]
    fn test_half_page_scrolls_with_the_selection_like_vim() {
        let mut cursor = ListCursor::default();
        cursor.fit(10, 30, 0);
        cursor.select(3, 30);
        assert!(cursor.navigate(UserEvent::HalfPageDown, 30));
        assert_eq!((cursor.offset(), cursor.selected()), (5, 8));
        assert!(cursor.navigate(UserEvent::HalfPageUp, 30));
        assert_eq!((cursor.offset(), cursor.selected()), (0, 3));
        // at the end the list stops, the selection going on to the last row
        cursor.select(29, 30);
        assert!(cursor.navigate(UserEvent::HalfPageDown, 30));
        assert_eq!((cursor.offset(), cursor.selected()), (20, 29));

        // from the top row, out of the scrolloff rows once the list scrolled
        let mut cursor = ListCursor::default();
        cursor.fit(10, 30, 2);
        assert!(cursor.navigate(UserEvent::HalfPageDown, 30));
        assert_eq!((cursor.offset(), cursor.selected()), (5, 7));
    }

    #[test]
    fn test_half_page_moves_the_list() {
        let mut cursor = ListCursor::default();
        cursor.fit(10, 30, 0);
        // ctrl-d scrolls the list instead, see `scroll_pages`
        assert_eq!(cursor.moved_to(UserEvent::HalfPageDown, 30), None);
        // the other movement keys move it a row
        assert_eq!(cursor.moved_to(UserEvent::NavigateDown, 30), Some(1));
    }

    // Scrolled before the list is drawn, the first row shown used to pass the selected one,
    // which `row` underflowed on.
    #[test]
    fn test_scroll_lines_before_the_first_draw() {
        let mut cursor = ListCursor::default();
        for _ in 0..3 {
            cursor.scroll_lines(1, 3);
        }
        assert_eq!(cursor.row(), Some(0));
    }

    // Half a page is a row at least, as the lists' and the text's: at a height of 1 it was none.
    #[test]
    fn test_half_page_of_a_scroll_moves_a_row_at_least() {
        let mut scroll = ScrollState {
            height: 1,
            ..Default::default()
        };
        assert!(scroll.handle_scroll(UserEvent::HalfPageDown));
        assert_eq!(scroll.offset, 1);
    }

    // ctrl-e and ctrl-y moved the selection out of the scrolloff rows at an edge the list can't
    // scroll past, though nothing scrolled. vim leaves it.
    #[test]
    fn test_scroll_lines_at_an_edge_leaves_the_selection() {
        let mut cursor = ListCursor::default();
        cursor.fit(10, 30, 3);
        cursor.scroll_lines(-1, 30);
        assert_eq!((cursor.offset(), cursor.selected()), (0, 0));
        // a list shorter than its window
        let mut cursor = ListCursor::default();
        cursor.fit(10, 5, 3);
        cursor.scroll_lines(1, 5);
        assert_eq!((cursor.offset(), cursor.selected()), (0, 0));
        // at the bottom
        cursor.select(4, 5);
        cursor.scroll_lines(-1, 5);
        assert_eq!(cursor.selected(), 4);
    }

    // The wheel scrolls the rows shown alone: the selection stays, even off them, until it moves,
    // which shows it again.
    #[test]
    fn test_wheel_leaves_the_selection() {
        let mut cursor = ListCursor::default();
        cursor.fit(10, 30, 2);
        for _ in 0..5 {
            assert!(cursor.navigate(UserEvent::Wheel(WHEEL_ROWS as isize), 30));
        }
        assert_eq!((cursor.selected(), cursor.offset()), (0, 5 * WHEEL_ROWS));
        assert_eq!(cursor.row(), None);
        // drawing again keeps the rows scrolled to
        assert_eq!(cursor.fit(10, 30, 2), 5 * WHEEL_ROWS);
        // no further than the last row, nor the first
        for _ in 0..30 {
            cursor.navigate(UserEvent::Wheel(WHEEL_ROWS as isize), 30);
        }
        assert_eq!(cursor.offset(), 20);
        for _ in 0..30 {
            cursor.navigate(UserEvent::Wheel(-(WHEEL_ROWS as isize)), 30);
        }
        assert_eq!(cursor.offset(), 0);
        cursor.navigate(UserEvent::Wheel(WHEEL_ROWS as isize), 30);
        cursor.navigate(UserEvent::Wheel(WHEEL_ROWS as isize), 30);
        // moving the selection shows it again
        assert!(cursor.navigate(UserEvent::NavigateDown, 30));
        assert_eq!((cursor.selected(), cursor.offset()), (1, 0));
        assert_eq!(cursor.row(), Some(1));
    }

    #[test]
    fn test_navigate_moves_by_every_scroll_key() {
        let mut cursor = ListCursor::default();
        cursor.fit(10, 30, 0);
        assert!(cursor.navigate(UserEvent::ScrollDown, 30));
        assert_eq!(cursor.selected(), 1);
        assert!(cursor.navigate(UserEvent::PageDown, 30));
        assert_eq!(cursor.selected(), 11);
        assert!(cursor.navigate(UserEvent::HalfPageDown, 30));
        assert_eq!(cursor.selected(), 16);
        assert!(cursor.navigate(UserEvent::GoToBottom, 30));
        assert_eq!(cursor.selected(), 29);
        assert!(cursor.navigate(UserEvent::GoToTop, 30));
        assert_eq!(cursor.selected(), 0);
        assert!(!cursor.navigate(UserEvent::Cancel, 30));
    }

    #[test]
    fn test_scroll_to() {
        assert_eq!(scroll_to(5, 2..2 + 1, 10, 20, 0), 2); // above: scrolls up to it
        assert_eq!(scroll_to(0, 12..12 + 1, 10, 20, 0), 3); // below: the last row shown
        assert_eq!(scroll_to(3, 7..7 + 1, 10, 20, 0), 3); // shown: stays
        assert_eq!(scroll_to(3, 7..7 + 1, 0, 20, 0), 7);
        // with rows kept around it: 2 above, 2 below
        assert_eq!(scroll_to(5, 6..6 + 1, 10, 20, 2), 4);
        assert_eq!(scroll_to(0, 8..8 + 1, 10, 20, 2), 1);
        assert_eq!(scroll_to(3, 7..7 + 1, 10, 20, 2), 3);
        // never past the last row, never more than half the height
        assert_eq!(scroll_to(0, 19..19 + 1, 10, 20, 2), 10);
        assert_eq!(scroll_to(0, 5..5 + 1, 4, 20, 9), 3); // 1 row kept in 4: rows 3 to 6
                                                         // a hunk: whole with the rows around it, else from its top with the margin above it
        assert_eq!(scroll_to(0, 12..15, 10, 50, 2), 7);
        assert_eq!(scroll_to(30, 12..15, 10, 50, 2), 10);
        assert_eq!(scroll_to(0, 40..60, 10, 100, 3), 37);
        assert_eq!(scroll_to(0, 40..60, 0, 100, 3), 40);
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
