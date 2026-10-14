use ratatui::{
    crossterm::event::{Event, KeyCode, KeyEvent},
    style::Stylize,
    text::Span,
};
use tui_input::{backend::crossterm::EventHandler, Input};

use crate::color::ColorTheme;

// Searching (`/`) and filtering (`f`) a file tree by path.
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

// Substring match, ignoring case unless the query has an uppercase letter (smart case).
pub fn path_matches(path: &str, query: &str) -> bool {
    if query.chars().any(char::is_uppercase) {
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

// The spans of a file or directory name, with the match of `search` highlighted.
pub fn highlighted_name(
    name: &str,
    search: Option<&str>,
    theme: &ColorTheme,
) -> Vec<Span<'static>> {
    let Some(range) = search.and_then(|query| match_range(name, query)) else {
        return vec![Span::raw(name.to_string())];
    };
    vec![
        Span::raw(name[..range.start].to_string()),
        Span::raw(name[range.clone()].to_string())
            .fg(theme.list_match_fg)
            .bg(theme.list_match_bg),
        Span::raw(name[range.end..].to_string()),
    ]
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
}
