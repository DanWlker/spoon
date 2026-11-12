// What is picked in a list for commands on several items: commits, files and directories, refs.
// Every list picks alike: one at a time, or a visual selection from where it started to the
// cursor, which adds to those picked before it and, moving back, unpicks what it went over.

use std::{borrow::Borrow, hash::Hash};

use rustc_hash::FxHashSet;

#[derive(Debug, Clone)]
pub struct Picks<K> {
    // in the order they were picked, which copying commits' hashes keeps
    order: Vec<K>,
    // the same, to tell one picked in no time whatever their number
    set: FxHashSet<K>,
    // those picked before the visual selection running, see `sync_visual`
    before_visual: Option<(Vec<K>, FxHashSet<K>)>,
    // what the visual selection running covered, see `finish_visual`
    span: Span<K>,
}

// The rows a visual selection covered: one only since it started, which ending it on toggles,
// else a range, which picks even once back on one row.
#[derive(Debug, Clone)]
enum Span<K> {
    None,
    One(K),
    Range,
}

impl<K> Default for Picks<K> {
    fn default() -> Self {
        Picks {
            order: Vec::new(),
            set: FxHashSet::default(),
            before_visual: None,
            span: Span::None,
        }
    }
}

impl<K: Clone + Eq + Hash> Picks<K> {
    pub fn insert(&mut self, key: &K) {
        if self.set.insert(key.clone()) {
            self.order.push(key.clone());
        }
    }

    pub fn contains<Q: Hash + Eq + ?Sized>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
    {
        self.set.contains(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = &K> {
        self.order.iter()
    }

    pub fn as_slice(&self) -> &[K] {
        &self.order
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    // Unpicks all, ending the visual selection.
    pub fn clear(&mut self) {
        *self = Picks::default();
    }

    // Keeps those `keep` is true for, like after a reload, those still there.
    pub fn retain(&mut self, mut keep: impl FnMut(&K) -> bool) {
        self.order.retain(|key| keep(key));
        self.set.retain(|key| keep(key));
        self.before_visual = None;
        self.span = Span::None;
    }

    // A visual selection starts: what it picks adds to those picked now.
    pub fn start_visual(&mut self) {
        self.before_visual = Some((self.order.clone(), self.set.clone()));
        self.span = Span::None;
    }

    // The visual selection was cancelled or stopped: what it picked stays.
    pub fn end_visual(&mut self) {
        self.before_visual = None;
        self.span = Span::None;
    }

    // The visual selection ended with `v`: one row picked before it, `v v` on it, is unpicked;
    // a range picks all it covers.
    pub fn finish_visual(&mut self) {
        if let (Span::One(key), Some((_, before))) = (&self.span, &self.before_visual) {
            if before.contains(key) {
                self.set.remove(key);
                self.order.retain(|k| k != key);
            }
        }
        self.end_visual();
    }

    // Those picked before the visual selection, and `keys`, those of the rows from its start to
    // the cursor; nothing without one.
    pub fn sync_visual<'k>(&mut self, keys: impl IntoIterator<Item = &'k K>)
    where
        K: 'k,
    {
        let Some((order, set)) = &self.before_visual else {
            return;
        };
        (self.order, self.set) = (order.clone(), set.clone());
        let mut keys = keys.into_iter().peekable();
        let first = keys.next();
        self.span = match (&self.span, first) {
            (Span::None | Span::One(_), Some(key)) if keys.peek().is_none() => {
                Span::One(key.clone())
            }
            _ => Span::Range,
        };
        for key in first.into_iter().chain(keys) {
            self.insert(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_visual_selection_follows_the_cursor_back() {
        let mut picks = Picks::default();
        picks.insert(&"e");
        // from a to the cursor, adding to e
        picks.start_visual();
        picks.sync_visual(&["a", "b", "c", "d"]);
        assert_eq!(picks.as_slice(), ["e", "a", "b", "c", "d"]);
        // back: c and d unpicked
        picks.sync_visual(&["a", "b"]);
        assert_eq!(picks.as_slice(), ["e", "a", "b"]);
        assert!(picks.contains(&"b") && !picks.contains(&"c"));
        // ended, it keeps what it picked, and a move picks nothing more
        picks.end_visual();
        picks.sync_visual(&["a", "b", "c"]);
        assert_eq!(picks.as_slice(), ["e", "a", "b"]);
        // each picked once
        picks.insert(&"a");
        assert_eq!(picks.len(), 3);
    }

    #[test]
    fn test_v_v_on_a_picked_key_unpicks_it() {
        let mut picks = Picks::default();
        picks.insert(&"a");
        // v v on the picked a: unpicked
        picks.start_visual();
        picks.sync_visual(&["a"]);
        picks.finish_visual();
        assert!(!picks.contains("a"));
        // v v on the unpicked b: picked
        picks.start_visual();
        picks.sync_visual(&["b"]);
        picks.finish_visual();
        assert!(picks.contains("b"));
        // a range always picks, even over the picked b
        picks.start_visual();
        picks.sync_visual(&["a", "b", "c"]);
        picks.finish_visual();
        assert_eq!(picks.as_slice(), ["b", "a", "c"]);
        // moved away and back, it is a range, which picks
        picks.start_visual();
        picks.sync_visual(&["b", "c"]);
        picks.sync_visual(&["b"]);
        picks.finish_visual();
        assert!(picks.contains("b"));
        // cancelled, a single row picked before stays
        picks.start_visual();
        picks.sync_visual(&["a"]);
        picks.end_visual();
        assert!(picks.contains("a"));
    }
}
