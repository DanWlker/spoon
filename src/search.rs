use std::sync::atomic::{AtomicBool, Ordering};

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

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SearchOptions {
    pub target: SearchTarget,
    pub smart_case: bool,
    pub fuzzy: bool,
}

impl SearchOptions {
    pub fn status_string(&self) -> String {
        let case = if self.smart_case {
            "smart-case"
        } else {
            "case-sensitive"
        };
        let matcher = if self.fuzzy { "fuzzy" } else { "substring" };
        format!("[{}] [{case}] [{matcher}]", self.target.as_str())
    }
}

// Whether a search for `query` is case-insensitive: smart case is, unless the query has an
// uppercase letter, case-sensitive never is.
pub fn case_insensitive(smart_case: bool, query: &str) -> bool {
    smart_case && !query.chars().any(char::is_uppercase)
}

// The commit list's options, which the file trees, diffs and commit details search with too.
// shortcut: global, keep them in `TreeQuery` if the trees get toggles of their own.
static SHARED_SMART_CASE: AtomicBool = AtomicBool::new(true);
static SHARED_FUZZY: AtomicBool = AtomicBool::new(false);

pub fn share(options: SearchOptions) {
    SHARED_SMART_CASE.store(options.smart_case, Ordering::Relaxed);
    SHARED_FUZZY.store(options.fuzzy, Ordering::Relaxed);
}

pub fn shared_smart_case() -> bool {
    SHARED_SMART_CASE.load(Ordering::Relaxed)
}

pub fn shared_fuzzy() -> bool {
    SHARED_FUZZY.load(Ordering::Relaxed)
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

        assert_eq!(options.status_string(), "[author] [smart-case] [fuzzy]");
    }
}
