use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
use ratatui::{
    crossterm::event::{Event, KeyCode, KeyEvent},
    style::Style,
    text::{Line, Span},
};
use std::sync::LazyLock;
use tui_input::{backend::crossterm::EventHandler, Input};

use crate::{
    color::ColorTheme,
    event::UserEvent,
    search::{case_insensitive, SearchOptions},
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

    // What applying the query being typed again changes, once its options are.
    pub fn editing_change(&self) -> QueryChange {
        match self.editing {
            Some((QueryKind::Search, _)) => QueryChange::Search,
            Some((QueryKind::Filter, _)) => QueryChange::Filter,
            None => QueryChange::None,
        }
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

// Whether a name, like a path or a field of a commit, matches `query`, see `match_ranges`.
pub fn name_matches(name: &str, query: &str, options: SearchOptions) -> bool {
    !match_ranges(name, query, options).is_empty()
}

// The byte range of the first occurrence of `query` in `text`, smart case or case-sensitive; the
// search of text, like diffs and files, which isn't fuzzy.
pub fn match_range(
    text: &str,
    query: &str,
    options: SearchOptions,
) -> Option<std::ops::Range<usize>> {
    if !case_insensitive(options.smart_case, query) {
        let start = text.find(query)?;
        return Some(start..start + query.len());
    }
    // the usual case, without allocating: searched for every field of every commit as typed
    if text.is_ascii() && query.is_ascii() {
        let (text, query) = (text.as_bytes(), query.as_bytes());
        let start = text
            .windows(query.len().max(1))
            .position(|w| w.eq_ignore_ascii_case(query))
            .or_else(|| query.is_empty().then_some(0))?;
        return Some(start..start + query.len());
    }
    // lowercasing can change byte lengths: each char lowercased, with where it is in `text`
    let mut lower = String::new();
    let mut origins = Vec::new();
    for (at, c) in text.char_indices() {
        for l in c.to_lowercase() {
            origins.push((lower.len(), at));
            lower.push(l);
        }
    }
    let start = lower.find(&query.to_lowercase())?;
    let end = start + query.to_lowercase().len();
    let origin = |i: usize| {
        let found = origins.iter().find(|(from, _)| *from >= i);
        found.map_or(text.len(), |(_, at)| *at)
    };
    Some(origin(start)..origin(end))
}

// Which way to go from the selection, to the next or the previous one; round, see `around`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toward {
    Next,
    Previous,
}

impl Toward {
    // `next` one way, the other event the other way.
    pub fn of(event: UserEvent, next: UserEvent) -> Self {
        if event == next {
            Toward::Next
        } else {
            Toward::Previous
        }
    }

    // The index of `len` after or before `index`, round; from none, the first or the last.
    pub fn around(self, index: Option<usize>, len: usize) -> usize {
        match (index, self) {
            (Some(i), Toward::Next) => (i + 1) % len,
            (Some(i), Toward::Previous) => (i + len - 1) % len,
            (None, Toward::Next) => 0,
            (None, Toward::Previous) => len - 1,
        }
    }
}

impl From<Toward> for SearchDirection {
    fn from(toward: Toward) -> Self {
        match toward {
            Toward::Next => SearchDirection::Next,
            Toward::Previous => SearchDirection::Previous,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDirection {
    // the selected row if it matches, else the next match
    Current,
    Next,
    Previous,
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
pub fn highlight_match(
    line: &mut Line<'_>,
    query: &str,
    options: SearchOptions,
    theme: &ColorTheme,
) {
    let text: String = line
        .spans
        .iter()
        .skip(1)
        .map(|s| s.content.as_ref())
        .collect();
    if let Some(matched) = match_range(&text, query, options) {
        patch_range(line, matched, match_style(theme));
    }
}

// The style of what a search matches, the same everywhere.
pub fn match_style(theme: &ColorTheme) -> Style {
    Style::default()
        .fg(theme.list_match_fg)
        .bg(theme.list_match_bg)
}

// `shown`, the start of `text` (all of it, or a short hash), in `style`, with what `search` matches
// in `text`, see `match_ranges`, highlighted where it is shown.
pub fn highlighted_text(
    text: &str,
    shown: usize,
    style: Style,
    search: Option<&str>,
    options: SearchOptions,
    theme: &ColorTheme,
) -> Vec<Span<'static>> {
    let shown = &text[..shown];
    let ranges = search.map_or(vec![], |query| match_ranges(text, query, options));
    let mut spans = Vec::new();
    let mut at = 0;
    for range in ranges.into_iter().filter(|r| r.start < shown.len()) {
        let end = range.end.min(shown.len());
        spans.push(Span::styled(shown[at..range.start].to_string(), style));
        let matched = style.patch(match_style(theme));
        spans.push(Span::styled(shown[range.start..end].to_string(), matched));
        at = end;
    }
    spans.push(Span::styled(shown[at..].to_string(), style));
    spans
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

// The byte ranges of `name` matching `query`, as every search of names matches: the commit list,
// the file trees and the commits of the blame and the timeline. The letters a fuzzy match picks,
// joined where they follow each other, or the substring; smart case or case-sensitive.
pub fn match_ranges(
    name: &str,
    query: &str,
    options: SearchOptions,
) -> Vec<std::ops::Range<usize>> {
    // nothing typed matches everything, as a substring does
    if !options.fuzzy || query.is_empty() {
        return match_range(name, query, options).into_iter().collect();
    }
    let matcher = path_matcher(options.smart_case);
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

    fn type_str(query: &mut TreeQuery, text: &str) {
        for c in text.chars() {
            query.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
    }

    #[test]
    fn test_match_range_where_lowercase_changes_lengths() {
        let smart = SearchOptions {
            smart_case: true,
            ..Default::default()
        };
        // İ lowercases to two chars, three bytes
        assert_eq!(match_range("İx Foo", "foo", smart), Some(4..7));
        assert_eq!(match_range("İx Foo", "i̇x", smart), Some(0..3));
        assert_eq!(match_range("İx Foo", "Foo", smart), Some(4..7));
        assert_eq!(match_range("İx Foo", "FOO", smart), None);
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
        assert_eq!(query.filter(), Some("src"));
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
    fn test_name_matches() {
        let smart_case = SearchOptions {
            smart_case: true,
            ..Default::default()
        };
        assert!(name_matches("src/Main.rs", "main", smart_case));
        assert!(!name_matches("src/main.rs", "Main", smart_case));
        assert!(name_matches("src/Main.rs", "Main", smart_case));
        assert_eq!(match_range("src/Main.rs", "main", smart_case), Some(4..8));
        assert_eq!(match_range("README", "x", smart_case), None);
    }

    #[test]
    fn test_fuzzy_paths() {
        let name_matches_with = |path, query, fuzzy, smart_case| {
            let options = SearchOptions {
                fuzzy,
                smart_case,
                ..Default::default()
            };
            name_matches(path, query, options)
        };
        assert!(name_matches_with("src/widget/diff.rs", "swd", true, true));
        assert!(!name_matches_with("src/widget/diff.rs", "swd", false, true));
        assert!(!name_matches_with("src/main.rs", "Main", true, true));
        assert!(!name_matches_with("src/main.rs", "xyz", true, true));
        // case-sensitive: a lowercase query no longer matches an uppercase path
        assert!(name_matches_with("src/Main.rs", "main", false, true));
        assert!(!name_matches_with("src/Main.rs", "main", false, false));
        assert!(!name_matches_with("src/Main.rs", "mn", true, false));
        assert!(name_matches_with("src/Main.rs", "Mn", true, false));
        // the picked letters of the name, joined where they follow each other; by bytes
        let fuzzy = SearchOptions {
            fuzzy: true,
            ..Default::default()
        };
        assert_eq!(match_ranges("dïff.rs", "dïrs", fuzzy), [0..3, 6..8]);
        // only the rest of the path matched
        assert!(match_ranges("diff.rs", "src", fuzzy).is_empty());
    }
}
