use serde::Deserialize;

// How a search went, shown in the status line on every page; `index` is 0 while the selected row
// isn't a match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchOutcome {
    Found {
        index: usize,
        total: usize,
        query: String,
    },
    NotFound {
        query: String,
    },
}

impl SearchOutcome {
    // How the search for `query` went over the rows `matched` says match, in order, the row
    // `selected` among them.
    pub fn of(query: &str, selected: usize, matched: impl Iterator<Item = bool>) -> Self {
        let mut total = 0;
        let mut index = 0;
        for (row, matched) in matched.enumerate() {
            if matched {
                total += 1;
                if row == selected {
                    index = total;
                }
            }
        }
        if total == 0 {
            return SearchOutcome::NotFound {
                query: query.to_string(),
            };
        }
        SearchOutcome::Found {
            index,
            total,
            query: query.to_string(),
        }
    }
}

impl std::fmt::Display for SearchOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SearchOutcome::Found {
                index,
                total,
                query,
            } => write!(f, "Match {index} of {total} (query: \"{query}\")"),
            SearchOutcome::NotFound { query } => write!(f, "No matches found (query: \"{query}\")"),
        }
    }
}

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
    // Whether searching this looks in `part`.
    pub fn includes(self, part: SearchTarget) -> bool {
        self == SearchTarget::All || self == part
    }

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

impl SearchKind {
    // Whether the fields of the commits searched can be picked.
    pub fn has_target(self) -> bool {
        self == SearchKind::Commits
    }

    pub fn has_fuzzy(self) -> bool {
        self != SearchKind::Text
    }
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
        if kind.has_target() {
            parts.push(self.target.as_str());
        }
        parts.push(case);
        if kind.has_fuzzy() {
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
    fn test_search_outcome_counts_the_matches_and_the_selected_one() {
        let matched = [true, false, true, true, false];
        let outcome = |selected: usize| SearchOutcome::of("a", selected, matched.into_iter());

        // the third row is the second match
        assert_eq!(
            outcome(2),
            SearchOutcome::Found {
                index: 2,
                total: 3,
                query: "a".into(),
            }
        );
        // a row that doesn't match is no match of the search
        assert_eq!(
            outcome(1),
            SearchOutcome::Found {
                index: 0,
                total: 3,
                query: "a".into(),
            }
        );
        assert_eq!(
            SearchOutcome::of("zzz", 0, [false, false].into_iter()),
            SearchOutcome::NotFound {
                query: "zzz".into()
            }
        );
    }

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
