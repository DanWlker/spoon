use serde::Deserialize;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchTarget {
    #[default]
    All,
    Subject,
    Author,
    Ref,
    Hash,
}

impl SearchTarget {
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Subject,
            Self::Subject => Self::Author,
            Self::Author => Self::Ref,
            Self::Ref => Self::Hash,
            Self::Hash => Self::All,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Subject => "subject",
            Self::Author => "author",
            Self::Ref => "ref",
            Self::Hash => "hash",
        }
    }
}

// What a search goes through, which decides the options that apply to it: the commit list's, all
// of them; names, like paths and the commits of the blame and the timeline, smart case and fuzzy;
// or text, like diffs and files, smart case only, as it is matched exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    Commits,
    Names,
    Text,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SearchOptions {
    pub target: SearchTarget,
    pub smart_case: bool,
    pub fuzzy: bool,
}

impl SearchOptions {
    // The options that apply to a search of `kind`, for the status line.
    pub fn status_string(&self, kind: SearchKind) -> String {
        let case = if self.smart_case {
            "smart-case"
        } else {
            "case-sensitive"
        };
        let matcher = if self.fuzzy { "fuzzy" } else { "substring" };
        let mut parts = vec![];
        if kind == SearchKind::Commits {
            parts.push(self.target.as_str());
        }
        parts.push(case);
        if kind != SearchKind::Text {
            parts.push(matcher);
        }
        parts
            .iter()
            .map(|part| format!("[{part}]"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

// Whether a search for `query` is case-insensitive: smart case is, unless the query has an
// uppercase letter, case-sensitive never is.
pub fn case_insensitive(smart_case: bool, query: &str) -> bool {
    smart_case && !query.chars().any(char::is_uppercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_target_next_cycles_through_all_targets() {
        let mut target = SearchTarget::All;
        let expected = [
            SearchTarget::Subject,
            SearchTarget::Author,
            SearchTarget::Ref,
            SearchTarget::Hash,
            SearchTarget::All,
        ];

        for expected_target in expected {
            target = target.next();
            assert_eq!(target, expected_target);
        }
    }

    #[test]
    fn test_search_options_status_string() {
        let options = SearchOptions {
            target: SearchTarget::Author,
            smart_case: true,
            fuzzy: true,
        };

        let status = |kind| options.status_string(kind);
        assert_eq!(status(SearchKind::Commits), "[author] [smart-case] [fuzzy]");
        assert_eq!(status(SearchKind::Names), "[smart-case] [fuzzy]");
        assert_eq!(status(SearchKind::Text), "[smart-case]");
    }
}
