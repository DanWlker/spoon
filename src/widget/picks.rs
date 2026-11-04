// What is picked in a list for commands on several items: commits, files and directories, refs.
// Every list picks alike: one at a time, or a visual selection from where it started to the
// cursor, which adds to those picked before it and, moving back, unpicks what it went over.

use std::hash::Hash;

use rustc_hash::FxHashSet;

#[derive(Debug, Clone)]
pub struct Picks<K> {
    // in the order they were picked, which copying commits' hashes keeps
    order: Vec<K>,
    // the same, to tell one picked in no time whatever their number
    set: FxHashSet<K>,
    // those picked before the visual selection running, see `sync_visual`
    before_visual: Option<(Vec<K>, FxHashSet<K>)>,
}

impl<K> Default for Picks<K> {
    fn default() -> Self {
        Picks {
            order: Vec::new(),
            set: FxHashSet::default(),
            before_visual: None,
        }
    }
}

impl<K: Clone + Eq + Hash> Picks<K> {
    pub fn insert(&mut self, key: &K) {
        if self.set.insert(key.clone()) {
            self.order.push(key.clone());
        }
    }

    pub fn contains(&self, key: &K) -> bool {
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
    }

    // A visual selection starts: what it picks adds to those picked now.
    pub fn start_visual(&mut self) {
        self.before_visual = Some((self.order.clone(), self.set.clone()));
    }

    // The visual selection ended, or was cancelled: what it picked stays.
    pub fn end_visual(&mut self) {
        self.before_visual = None;
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
        for key in keys {
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
}
