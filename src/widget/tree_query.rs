use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
use ratatui::{
    buffer::Buffer,
    crossterm::event::{Event, KeyCode, KeyEvent},
    layout::Rect,
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use std::sync::LazyLock;
use tui_input::{backend::crossterm::EventHandler, Input};

use crate::{
    color::ColorTheme,
    search::{case_insensitive, shared_fuzzy, shared_smart_case},
    widget::{
        file_tree::{keeps, FileTreeRow, FileTreeState, SearchDirection},
        style::{change_span, fold_marker, mark_picked_row, mark_selected_row, placeholder},
    },
};

// Searching (`/`) and filtering (`?`) a file tree by path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryKind {
    Search,
    Filter,
}

#[derive(Debug, Default)]
pub struct TreeQuery {
    // the query being typed, applied live
    editing: Option<(QueryKind, Input)>,
    search: Option<String>,
    filter: Option<String>,
}

// What a key typed into the query changed, for the tree to update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryChange {
    None,
    Search,
    Filter,
}

impl TreeQuery {
    // Starts typing a query, from its current value.
    pub fn start(&mut self, kind: QueryKind) {
        let value = match kind {
            QueryKind::Search => self.search.clone(),
            QueryKind::Filter => self.filter.clone(),
        };
        self.editing = Some((kind, Input::new(value.unwrap_or_default())));
    }

    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    // Enter keeps the query, Esc clears it; other keys edit it, which applies it right away.
    pub fn handle_key(&mut self, key: KeyEvent) -> QueryChange {
        let Some((kind, input)) = self.editing.as_mut() else {
            return QueryChange::None;
        };
        let kind = *kind;
        match key.code {
            KeyCode::Enter => {
                self.editing = None;
                return QueryChange::None;
            }
            KeyCode::Esc => {
                self.editing = None;
                self.set(kind, None);
            }
            _ => {
                input.handle_event(&Event::Key(key));
                let value = Some(input.value().to_string()).filter(|v| !v.is_empty());
                self.set(kind, value);
            }
        }
        match kind {
            QueryKind::Search => QueryChange::Search,
            QueryKind::Filter => QueryChange::Filter,
        }
    }

    // Clears the search, or the filter when there is no search. Returns what was cleared.
    pub fn clear(&mut self) -> QueryChange {
        if self.search.take().is_some() {
            QueryChange::Search
        } else if self.filter.take().is_some() {
            QueryChange::Filter
        } else {
            QueryChange::None
        }
    }

    pub fn search(&self) -> Option<&str> {
        self.search.as_deref()
    }

    pub fn filter(&self) -> Option<&str> {
        self.filter.as_deref()
    }

    // Whether `path` passes the filter.
    pub fn keeps(&self, path: &str) -> bool {
        keeps(self.filter(), path)
    }

    // The query being typed, as shown in the status line: (text, cursor position).
    pub fn status_input(&self) -> Option<(String, u16)> {
        let (kind, input) = self.editing.as_ref()?;
        let prefix = prefix(*kind);
        let cursor = (prefix.chars().count() + input.visual_cursor()) as u16;
        Some((format!("{prefix}{}", input.value()), cursor))
    }

    // A short description of the active search and filter, for titles.
    pub fn indicator(&self) -> Option<String> {
        let parts: Vec<String> = [
            self.search
                .as_ref()
                .map(|s| format!("{}{s}", prefix(QueryKind::Search))),
            self.filter
                .as_ref()
                .map(|f| format!("{}{f}", prefix(QueryKind::Filter))),
        ]
        .into_iter()
        .flatten()
        .collect();
        (!parts.is_empty()).then(|| parts.join("  "))
    }

    fn set(&mut self, kind: QueryKind, value: Option<String>) {
        match kind {
            QueryKind::Search => self.search = value,
            QueryKind::Filter => self.filter = value,
        }
    }
}

fn prefix(kind: QueryKind) -> &'static str {
    match kind {
        QueryKind::Search => "/",
        QueryKind::Filter => "filter: ",
    }
}

static SMART_CASE_MATCHER: LazyLock<SkimMatcherV2> = LazyLock::new(SkimMatcherV2::default);
static CASE_SENSITIVE_MATCHER: LazyLock<SkimMatcherV2> =
    LazyLock::new(|| SkimMatcherV2::default().respect_case());

fn path_matcher(smart_case: bool) -> &'static SkimMatcherV2 {
    if smart_case {
        &SMART_CASE_MATCHER
    } else {
        &CASE_SENSITIVE_MATCHER
    }
}

// Whether `path` matches `query`, searching as the commit list does: fuzzily or as a substring,
// smart case or case-sensitive.
pub fn path_matches(path: &str, query: &str) -> bool {
    path_matches_with(path, query, shared_fuzzy(), shared_smart_case())
}

fn path_matches_with(path: &str, query: &str, fuzzy: bool, smart_case: bool) -> bool {
    if fuzzy {
        path_matcher(smart_case).fuzzy_match(path, query).is_some()
    } else if case_insensitive(smart_case, query) {
        path.to_lowercase().contains(&query.to_lowercase())
    } else {
        path.contains(query)
    }
}

// The byte range of the first match of `query` in `text`, to highlight it.
pub fn match_range(text: &str, query: &str) -> Option<std::ops::Range<usize>> {
    let start = if !case_insensitive(shared_smart_case(), query) {
        text.find(query)?
    } else {
        // lowercasing can change byte lengths, so only use the result when it does not
        let lower = text.to_lowercase();
        if lower.len() != text.len() {
            return None;
        }
        lower.find(&query.to_lowercase())?
    };
    Some(start..start + query.len())
}

// The row of the `count` matching, in `direction` around from `start` (the row of the last match,
// or the selection); every row is tried once.
pub fn find_around(
    count: usize,
    start: usize,
    direction: SearchDirection,
    matches: impl Fn(usize) -> bool,
) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let start = start.min(count - 1);
    let row = |i: usize| match direction {
        SearchDirection::Current => (start + i) % count,
        SearchDirection::Next => (start + 1 + i) % count,
        SearchDirection::Previous => (start + count * 2 - 1 - i) % count,
    };
    (0..count).map(row).find(|&r| matches(r))
}

// Highlights the match of `query` in the last span of a line, its text after any line numbers.
pub fn highlight_match(line: &mut Line<'_>, query: &str, theme: &ColorTheme) {
    let text: String = line
        .spans
        .iter()
        .skip(1)
        .map(|s| s.content.as_ref())
        .collect();
    if let Some(matched) = match_range(&text, query) {
        let style = Style::default()
            .fg(theme.list_match_fg)
            .bg(theme.list_match_bg);
        patch_range(line, matched, style);
    }
}

// Patches `style` onto the bytes `range` of the text after the first span, a line number.
pub fn patch_range(line: &mut Line<'_>, range: std::ops::Range<usize>, style: Style) {
    if line.spans.is_empty() {
        return;
    }
    let mut spans = Vec::new();
    let mut offset = 0;
    for span in line.spans.drain(1..) {
        let len = span.content.len();
        let start = range.start.clamp(offset, offset + len) - offset;
        let end = range.end.clamp(offset, offset + len) - offset;
        for (part, patched) in [(0..start, false), (start..end, true), (end..len, false)] {
            if part.is_empty() {
                continue;
            }
            let part = Span::styled(span.content[part].to_string(), span.style);
            spans.push(if patched {
                part.patch_style(style)
            } else {
                part
            });
        }
        offset += len;
    }
    line.spans.extend(spans);
}

// The row of a directory in a file tree: after its `guide` (see `tree_guides`), open or closed,
// its name in the directory color with the match of `search` highlighted.
pub fn dir_line(
    row: &FileTreeRow,
    guide: &str,
    open: bool,
    search: Option<&str>,
    theme: &ColorTheme,
) -> Line<'static> {
    let mut spans = vec![guide_span(guide, theme), fold_marker(open).into()];
    spans.extend(
        highlighted_name(&row.name, search, theme)
            .into_iter()
            .map(|span| match span.style.fg {
                Some(_) => span,
                None => span.fg(theme.file_tree_dir_fg),
            }),
    );
    Line::from(spans)
}

// The indent markers of a row, in the color of the dividers.
pub fn guide_span(guide: &str, theme: &ColorTheme) -> Span<'static> {
    Span::raw(guide.to_string()).fg(theme.divider_fg)
}

// Draws the rows of `tree` in `area`, like every file tree: the directories open or closed, the
// files with their change letter from `symbol` (by their index) when they have one, the match of
// `search` highlighted; the `picked` rows and, with `cursor` (whether the tree has the focus), the
// selected one highlighted across the row, the lines of the tree keeping their color; `empty`
// when there is no row. Returns the first row shown, for the scrollbar.
#[allow(clippy::too_many_arguments)]
pub fn render_tree(
    buf: &mut Buffer,
    area: Rect,
    tree: &FileTreeState,
    symbol: impl Fn(usize) -> Option<&'static str>,
    search: Option<&str>,
    picked: impl Fn(&str) -> bool,
    cursor: Option<bool>,
    empty: &str,
    theme: &ColorTheme,
) -> usize {
    let rows = tree.rows();
    if rows.is_empty() {
        Paragraph::new(placeholder(empty, theme)).render(area, buf);
        return 0;
    }
    let height = area.height as usize;
    // no room for a row, like in a very short terminal
    if height == 0 {
        return 0;
    }
    let offset = tree.scroll(height);
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(index, row)| {
            let guide = tree.guide(index);
            let Some(file) = row.file_index else {
                return dir_line(row, guide, tree.is_open(&row.path), search, theme);
            };
            let mut spans = vec![guide_span(guide, theme)];
            if let Some(symbol) = symbol(file) {
                spans.extend([change_span(symbol, theme), Span::raw(" ")]);
            }
            spans.extend(highlighted_name(&row.name, search, theme));
            Line::from(spans)
        })
        .collect();
    Paragraph::new(lines).fg(theme.fg).render(area, buf);

    for (y, row) in (area.top()..area.bottom()).zip(rows.iter().skip(offset)) {
        if picked(&row.path) {
            mark_picked_row(buf, Rect::new(area.x, y, area.width, 1), theme);
        }
    }
    if let Some(focused) = cursor {
        let y = area.y + (tree.cursor() - offset) as u16;
        mark_selected_row(buf, area, y, focused, theme);
    }
    unhighlight_guides(buf, area, tree, offset, theme);
    offset
}

// Gives the indent markers at the start of the rows `area` shows from `offset` their own color
// back, over the highlight of the selected and picked rows.
pub fn unhighlight_guides(
    buf: &mut Buffer,
    area: Rect,
    tree: &FileTreeState,
    offset: usize,
    theme: &ColorTheme,
) {
    for (y, index) in (area.top()..area.bottom()).zip(offset..tree.rows().len()) {
        keep_guide_color(buf, area, y, tree.guide(index), theme);
    }
}

// Gives the indent markers of the row at `y` of `area` their color back, see `unhighlight_guides`.
pub fn keep_guide_color(buf: &mut Buffer, area: Rect, y: u16, guide: &str, theme: &ColorTheme) {
    // the background of a highlight stays, the whole row shows selected
    let style = Style::new()
        .fg(theme.divider_fg)
        .remove_modifier(Modifier::BOLD);
    // the markers are one column each
    let width = (guide.chars().count() as u16).min(area.width);
    buf.set_style(Rect::new(area.x, y, width, 1), style);
}

// The spans of a file or directory name, with the match of `search` highlighted.
pub fn highlighted_name(
    name: &str,
    search: Option<&str>,
    theme: &ColorTheme,
) -> Vec<Span<'static>> {
    let ranges = search.map_or(vec![], |query| {
        name_match_ranges(name, query, shared_fuzzy())
    });
    let mut spans = Vec::new();
    let mut at = 0;
    for range in ranges {
        spans.push(Span::raw(name[at..range.start].to_string()));
        spans.push(
            Span::raw(name[range.clone()].to_string())
                .fg(theme.list_match_fg)
                .bg(theme.list_match_bg),
        );
        at = range.end;
    }
    spans.push(Span::raw(name[at..].to_string()));
    spans
}

// The byte ranges of `name` matching `query`: the letters a fuzzy match picks, joined where they
// follow each other, or the substring. None when only the rest of the path matches.
fn name_match_ranges(name: &str, query: &str, fuzzy: bool) -> Vec<std::ops::Range<usize>> {
    if !fuzzy {
        return match_range(name, query).into_iter().collect();
    }
    let matcher = path_matcher(shared_smart_case());
    let Some((_, indices)) = matcher.fuzzy_indices(name, query) else {
        return vec![];
    };
    let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
    for (i, (at, c)) in name.char_indices().enumerate() {
        if !indices.contains(&i) {
            continue;
        }
        match ranges.last_mut() {
            Some(last) if last.end == at => last.end = at + c.len_utf8(),
            _ => ranges.push(at..at + c.len_utf8()),
        }
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        test_support::ui::{
            assert_guides_keep_color, assert_only_bg_changed, assert_row_bg, row_text,
        },
        widget::file_tree::FileTree,
    };

    // Draws `tree` in 20 columns and `height` rows, from column 1, the change letter "M" on each
    // file and `picked` highlighted.
    fn draw_tree(tree: &FileTreeState, height: u16, focused: bool, picked: &str) -> Buffer {
        let theme = ColorTheme::default();
        let mut buf = Buffer::empty(Rect::new(0, 0, 23, height));
        let area = Rect::new(1, 0, 20, height);
        let symbol = |_| Some("M");
        let is_picked = |path: &str| path == picked;
        render_tree(
            &mut buf,
            area,
            tree,
            symbol,
            None,
            is_picked,
            Some(focused),
            "",
            &theme,
        );
        buf
    }

    #[test]
    fn test_tree_rows_look_like_the_other_lists() {
        let theme = ColorTheme::default();
        let mut tree = FileTreeState::new(FileTree::new(["a/x.rs", "a/y.rs", "z.rs"]), true);
        for focused in [true, false] {
            tree.select_path("z.rs");
            let plain = draw_tree(&tree, 5, focused, "");
            tree.select_path("a/x.rs");
            let selected = draw_tree(&tree, 5, focused, "");
            // the change letter and the lines keep their colors
            let row = Rect::new(1, 1, 20, 1);
            assert_row_bg(&selected, row, theme.list_selected_bg);
            assert_only_bg_changed(&plain, &selected, row);
            assert_guides_keep_color(&selected, row, &theme);
        }
        // a picked row: across it too
        let buf = draw_tree(&tree, 5, true, "a/y.rs");
        let row = Rect::new(1, 2, 20, 1);
        assert_row_bg(&buf, row, theme.list_picked_bg);
        assert_guides_keep_color(&buf, row, &theme);
    }

    #[test]
    fn test_tree_scrolls_like_the_other_lists() {
        let paths: Vec<String> = (0..10).map(|i| format!("f{i}.rs")).collect();
        let mut tree = FileTreeState::new(FileTree::new(paths.iter().map(String::as_str)), true);
        let top = |tree: &FileTreeState| row_text(&draw_tree(tree, 4, true, ""), 0);
        // down past the bottom: the view follows the cursor
        tree.select_index(5);
        assert!(top(&tree).contains("f2.rs"), "{}", top(&tree));
        // up within the view: the view stays
        tree.select_index(3);
        assert!(top(&tree).contains("f2.rs"), "{}", top(&tree));
        // up past the top: it follows again
        tree.select_index(0);
        assert!(top(&tree).contains("f0.rs"), "{}", top(&tree));
    }

    fn type_str(query: &mut TreeQuery, text: &str) {
        for c in text.chars() {
            query.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
    }

    #[test]
    fn test_tree_query() {
        let mut query = TreeQuery::default();
        assert_eq!(query.indicator(), None);

        // typing applies the search live, enter keeps it
        query.start(QueryKind::Search);
        type_str(&mut query, "ma");
        assert_eq!(query.search(), Some("ma"));
        assert_eq!(query.status_input(), Some(("/ma".into(), 3)));
        query.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(!query.is_editing());

        // esc while typing clears that query only
        query.start(QueryKind::Filter);
        type_str(&mut query, "src");
        assert!(query.keeps("src/main.rs"));
        assert!(!query.keeps("README.md"));
        assert_eq!(query.indicator(), Some("/ma  filter: src".into()));
        query.handle_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(query.filter(), None);
        assert_eq!(query.search(), Some("ma"));

        // clearing removes the search first, then the filter
        query.start(QueryKind::Filter);
        type_str(&mut query, "x");
        query.handle_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(query.clear(), QueryChange::Search);
        assert_eq!(query.clear(), QueryChange::Filter);
        assert_eq!(query.clear(), QueryChange::None);
    }

    #[test]
    fn test_path_matches() {
        assert!(path_matches("src/Main.rs", "main"));
        assert!(!path_matches("src/main.rs", "Main"));
        assert!(path_matches("src/Main.rs", "Main"));
        assert_eq!(match_range("src/Main.rs", "main"), Some(4..8));
        assert_eq!(match_range("README", "x"), None);
    }

    #[test]
    fn test_fuzzy_paths() {
        assert!(path_matches_with("src/widget/diff.rs", "swd", true, true));
        assert!(!path_matches_with("src/widget/diff.rs", "swd", false, true));
        assert!(!path_matches_with("src/main.rs", "Main", true, true));
        assert!(!path_matches_with("src/main.rs", "xyz", true, true));
        // case-sensitive: a lowercase query no longer matches an uppercase path
        assert!(path_matches_with("src/Main.rs", "main", false, true));
        assert!(!path_matches_with("src/Main.rs", "main", false, false));
        assert!(!path_matches_with("src/Main.rs", "mn", true, false));
        assert!(path_matches_with("src/Main.rs", "Mn", true, false));
        // the picked letters of the name, joined where they follow each other; by bytes
        assert_eq!(name_match_ranges("dïff.rs", "dïrs", true), [0..3, 6..8]);
        // only the rest of the path matched
        assert!(name_match_ranges("diff.rs", "src", true).is_empty());
    }
}
