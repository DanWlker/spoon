use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
use once_cell::sync::Lazy;
use ratatui::{
    crossterm::event::{Event, KeyCode, KeyEvent},
    style::{Style, Stylize},
    text::{Line, Span},
};
use std::sync::atomic::{AtomicBool, Ordering};
use tui_input::{backend::crossterm::EventHandler, Input};

use crate::{color::ColorTheme, widget::file_tree::SearchDirection};

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
        self.filter.as_deref().is_none_or(|f| path_matches(path, f))
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

// Whether paths are searched and filtered fuzzily, like the commit list with core.search.fuzzy.
// shortcut: set once from the config at startup, the commit list's fuzzy toggle doesn't change it;
// keep it in `TreeQuery` if the trees get a toggle of their own.
static FUZZY_PATHS: AtomicBool = AtomicBool::new(false);

// smart case: ignoring it unless the query has an uppercase letter
static PATH_MATCHER: Lazy<SkimMatcherV2> = Lazy::new(SkimMatcherV2::default);

pub fn set_fuzzy_paths(fuzzy: bool) {
    FUZZY_PATHS.store(fuzzy, Ordering::Relaxed);
}

// Whether `path` matches `query`: fuzzily, or as a substring; smart case either way.
pub fn path_matches(path: &str, query: &str) -> bool {
    path_matches_with(path, query, FUZZY_PATHS.load(Ordering::Relaxed))
}

fn path_matches_with(path: &str, query: &str, fuzzy: bool) -> bool {
    if fuzzy {
        PATH_MATCHER.fuzzy_match(path, query).is_some()
    } else if query.chars().any(char::is_uppercase) {
        path.contains(query)
    } else {
        path.to_lowercase().contains(&query.to_lowercase())
    }
}

// The byte range of the first match of `query` in `text`, to highlight it.
pub fn match_range(text: &str, query: &str) -> Option<std::ops::Range<usize>> {
    let start = if query.chars().any(char::is_uppercase) {
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

// The spans of a file or directory name, with the match of `search` highlighted.
pub fn highlighted_name(
    name: &str,
    search: Option<&str>,
    theme: &ColorTheme,
) -> Vec<Span<'static>> {
    let fuzzy = FUZZY_PATHS.load(Ordering::Relaxed);
    let ranges = search.map_or(vec![], |query| name_match_ranges(name, query, fuzzy));
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
    let Some((_, indices)) = PATH_MATCHER.fuzzy_indices(name, query) else {
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
        assert!(path_matches_with("src/widget/diff.rs", "swd", true));
        assert!(!path_matches_with("src/widget/diff.rs", "swd", false));
        assert!(!path_matches_with("src/main.rs", "Main", true));
        assert!(!path_matches_with("src/main.rs", "xyz", true));
        // the picked letters of the name, joined where they follow each other; by bytes
        assert_eq!(name_match_ranges("dïff.rs", "dïrs", true), [0..3, 6..8]);
        // only the rest of the path matched
        assert!(name_match_ranges("diff.rs", "src", true).is_empty());
    }
}
