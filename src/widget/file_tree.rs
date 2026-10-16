use std::{
    cell::Cell,
    collections::{BTreeMap, HashSet},
};

use crate::widget::{
    style::scroll_to,
    tree_query::{find_around, path_matches},
};

// A row of a file tree as displayed: directories first, then files, each sorted by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTreeRow {
    pub depth: usize,
    pub name: String,
    // full path of the file or directory
    pub path: String,
    // index of the file in the paths the tree was built from, None for directories
    pub file_index: Option<usize>,
}

// The path of the row standing for the whole repository: git's pathspec for the top directory.
pub const ROOT_PATH: &str = ":/";

impl FileTreeRow {
    // A "/" row containing everything, above the rows of the top directory.
    pub fn root() -> Self {
        Self {
            depth: 0,
            name: "/".into(),
            path: ROOT_PATH.into(),
            file_index: None,
        }
    }

    pub fn is_root(&self) -> bool {
        self.path == ROOT_PATH
    }

    pub fn is_dir(&self) -> bool {
        self.file_index.is_none()
    }

    // Whether `path` is this file, or is inside this directory.
    pub fn contains(&self, path: &str) -> bool {
        path_contains(&self.path, path)
    }
}

#[derive(Debug, Default)]
pub struct FileTree {
    root: DirNode,
}

#[derive(Debug, Default)]
struct DirNode {
    dirs: BTreeMap<String, DirNode>,
    files: Vec<(String, usize)>,
}

impl FileTree {
    pub fn new<'p>(paths: impl IntoIterator<Item = &'p str>) -> Self {
        let mut root = DirNode::default();
        for (index, path) in paths.into_iter().enumerate() {
            let mut parts: Vec<&str> = path.split('/').collect();
            let name = parts.pop().unwrap_or_default();
            let mut node = &mut root;
            for part in parts {
                node = node.dirs.entry(part.to_string()).or_default();
            }
            node.files.push((name.to_string(), index));
        }
        sort_files(&mut root);
        Self { root }
    }

    // The rows of the files for which `keep` is true and their directories, descending only into the
    // directories for which `is_open` is true.
    pub fn rows(
        &self,
        is_open: impl Fn(&str) -> bool,
        keep: impl Fn(&str) -> bool,
    ) -> Vec<FileTreeRow> {
        let mut rows = Vec::new();
        collect_rows(&self.root, "", 0, &is_open, &keep, Some(&mut rows));
        rows
    }
}

// A tree whose directories can be expanded and collapsed, with a cursor on one of its rows.
#[derive(Debug)]
pub struct FileTreeState {
    tree: FileTree,
    open_by_default: bool,
    // under a root row standing for everything, see `FileTreeRow::root`
    rooted: bool,
    // directories whose state differs from `open_by_default`
    toggled: HashSet<String>,
    // only files whose path matches are shown, with all directories expanded
    filter: Option<String>,
    rows: Vec<FileTreeRow>,
    // the indent markers of the rows, see `tree_guides`
    guides: Vec<String>,
    cursor: usize,
    // the first row shown, moved as little as can be to show the cursor as the tree is drawn
    offset: Cell<usize>,
}

impl FileTreeState {
    pub fn new(tree: FileTree, open_by_default: bool) -> Self {
        Self::with_options(tree, open_by_default, false)
    }

    // A tree under a root row, open by default, like the files of the local changes.
    pub fn rooted(tree: FileTree) -> Self {
        Self::with_options(tree, true, true)
    }

    fn with_options(tree: FileTree, open_by_default: bool, rooted: bool) -> Self {
        let mut state = Self {
            tree,
            open_by_default,
            rooted,
            toggled: HashSet::new(),
            filter: None,
            rows: Vec::new(),
            guides: Vec::new(),
            cursor: 0,
            offset: Cell::new(0),
        };
        state.rebuild_rows();
        state
    }

    // The same expanded directories and selected path over another tree, where they exist.
    pub fn with_tree(&self, tree: FileTree) -> Self {
        let mut state = Self {
            tree,
            open_by_default: self.open_by_default,
            rooted: self.rooted,
            toggled: self.toggled.clone(),
            filter: self.filter.clone(),
            rows: Vec::new(),
            guides: Vec::new(),
            cursor: 0,
            offset: self.offset.clone(),
        };
        state.rebuild_rows();
        if let Some(path) = self.selected().map(|r| r.path.as_str()) {
            state.select_path(path);
        }
        state
    }

    // Replaces the tree, like after the files changed, keeping the expanded directories and the
    // selected path, or else the position of the cursor.
    pub fn set_tree(&mut self, tree: FileTree) {
        self.tree = tree;
        self.rebuild_rows();
    }

    pub fn rows(&self) -> &[FileTreeRow] {
        &self.rows
    }

    // The first row shown in `height` rows, showing the cursor with `scrolloff` rows around it,
    // moved as little as can be since the last time, like the other lists; see `scroll_to`.
    pub fn scroll(&self, height: usize, scrolloff: usize) -> usize {
        let offset = scroll_to(
            self.offset.get(),
            self.cursor,
            height,
            self.rows.len(),
            scrolloff,
        );
        self.offset.set(offset);
        offset
    }

    // The indent markers before the row at `index`, see `tree_guides`.
    pub fn guide(&self, index: usize) -> &str {
        self.guides.get(index).map_or("", String::as_str)
    }

    // All rows with every directory expanded, passing the filter, to search in.
    pub fn all_rows(&self) -> Vec<FileTreeRow> {
        let keep = |path: &str| keeps(self.filter.as_deref(), path);
        self.tree.rows(|_| true, keep)
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn selected(&self) -> Option<&FileTreeRow> {
        self.rows.get(self.cursor)
    }

    pub fn is_open(&self, path: &str) -> bool {
        self.filter.is_some() || self.open_by_default != self.toggled.contains(path)
    }

    pub fn filter(&self) -> Option<&str> {
        self.filter.as_deref()
    }

    pub fn set_filter(&mut self, filter: Option<&str>) {
        self.filter = filter.map(String::from);
        self.rebuild_rows();
    }

    // Selects the next match of `query`, expanding the directories that hide it.
    pub fn search(&mut self, query: &str, direction: SearchDirection) {
        let all_rows = self.all_rows();
        let current = self.selected().map(|r| r.path.as_str());
        let path = find_match(&all_rows, current, query, direction);
        self.reveal(path.as_deref());
    }

    // Selects `path`, expanding the directories that hide it.
    pub fn reveal(&mut self, path: Option<&str>) {
        let Some(path) = path else {
            return;
        };
        for dir in ancestors(path) {
            if !self.is_open(&dir) {
                self.toggled_mut(dir);
            }
        }
        self.rebuild_rows();
        self.select_path(path);
    }

    // Moves the cursor to the row at `index`, or the last one.
    pub fn select_index(&mut self, index: usize) {
        self.cursor = index.min(self.rows.len().saturating_sub(1));
    }

    pub fn select_path(&mut self, path: &str) {
        if let Some(pos) = self.rows.iter().position(|r| r.path == path) {
            self.cursor = pos;
        }
    }

    pub fn select_next(&mut self) {
        self.cursor = (self.cursor + 1).min(self.rows.len().saturating_sub(1));
    }

    pub fn select_prev(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn select_first(&mut self) {
        self.cursor = 0;
    }

    pub fn select_last(&mut self) {
        self.cursor = self.rows.len().saturating_sub(1);
    }

    pub fn expand(&mut self) {
        if let Some(path) = self.selected_dir().filter(|p| !self.is_open(p)) {
            self.toggle_dir(path);
        }
    }

    // Collapses the selected directory, or moves to the parent directory when it is not open.
    pub fn collapse(&mut self) {
        if let Some(path) = self.selected_dir().filter(|p| self.is_open(p)) {
            self.toggle_dir(path);
            return;
        }
        let Some(row) = self.selected().filter(|r| !r.is_root()) else {
            return;
        };
        let parent = match row.path.rsplit_once('/') {
            Some((parent, _)) => parent.to_string(),
            // the top directory is in the root row
            None if self.rooted => ROOT_PATH.to_string(),
            None => return,
        };
        self.select_path(&parent);
    }

    pub fn toggle(&mut self) {
        if let Some(path) = self.selected_dir() {
            self.toggle_dir(path);
        }
    }

    fn selected_dir(&self) -> Option<String> {
        self.selected()
            .filter(|r| r.is_dir())
            .map(|r| r.path.clone())
    }

    fn toggle_dir(&mut self, path: String) {
        self.toggled_mut(path);
        self.rebuild_rows();
    }

    fn toggled_mut(&mut self, path: String) {
        if !self.toggled.remove(&path) {
            self.toggled.insert(path);
        }
    }

    // Keeps the selected path, or else the position of the cursor.
    fn rebuild_rows(&mut self) {
        let selected = self.selected().map(|r| r.path.clone());
        let keep = |path: &str| keeps(self.filter.as_deref(), path);
        let rows = self.tree.rows(|path| self.is_open(path), keep);
        self.rows = if self.rooted {
            under_root(rows, self.is_open(ROOT_PATH))
        } else {
            rows
        };
        self.guides = tree_guides(self.rows.iter().map(|r| (r.depth, r.is_dir())));
        self.select_index(self.cursor);
        if let Some(path) = selected {
            self.select_path(&path);
        }
    }
}

// The indent markers before each row of a tree, given as its depth and whether it shows a marker
// of its own, like nvim-tree's: a line down the entries of each directory, "└" at its last one,
// two columns a level. A row's own marker, like a directory's fold marker, goes after its guide;
// the guide of the other rows, like files, ends with their line.
pub fn tree_guides(rows: impl IntoIterator<Item = (usize, bool)>) -> Vec<String> {
    let rows: Vec<(usize, bool)> = rows.into_iter().collect();
    // whether each row is the last entry of its directory, from the end
    let mut last = vec![false; rows.len()];
    let mut seen: Vec<bool> = vec![];
    for (i, &(depth, _)) in rows.iter().enumerate().rev() {
        seen.resize(depth + 1, false);
        last[i] = !seen[depth];
        seen[depth] = true;
    }
    // whether the directory at each depth has entries after it, from the start
    let mut continues: Vec<bool> = vec![];
    rows.iter()
        .zip(last)
        .map(|(&(depth, own_marker), last)| {
            // the top level has no line
            let mut guide: String = (0..depth)
                .map(|level| match continues.get(level) {
                    Some(true) if level > 0 => "│ ",
                    _ => "  ",
                })
                .collect();
            if !own_marker {
                guide.push_str(match (depth, last) {
                    (0, _) => "  ",
                    (_, true) => "└ ",
                    _ => "│ ",
                });
            }
            continues.resize(depth + 1, false);
            continues[depth] = !last;
            guide
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDirection {
    // the selected row if it matches, else the next match
    Current,
    Next,
    Previous,
}

// The path of the match of `query` in `rows` from `current` in `direction`, wrapping around.
pub fn find_match(
    rows: &[FileTreeRow],
    current: Option<&str>,
    query: &str,
    direction: SearchDirection,
) -> Option<String> {
    find_next(rows, current, direction, |row| {
        path_matches(&row.path, query)
    })
}

// The path of the next row in `rows` for which `wanted` is true, from `current` in `direction`,
// wrapping around. The root row is never one.
pub fn find_next(
    rows: &[FileTreeRow],
    current: Option<&str>,
    direction: SearchDirection,
    wanted: impl Fn(&FileTreeRow) -> bool,
) -> Option<String> {
    let cur = current
        .and_then(|path| rows.iter().position(|r| r.path == path))
        .unwrap_or(0);
    find_around(rows.len(), cur, direction, |i| {
        !rows[i].is_root() && wanted(&rows[i])
    })
    .map(|i| rows[i].path.clone())
}

// The directories containing `path`, outermost first, starting with the root.
pub fn ancestors(path: &str) -> Vec<String> {
    let mut dirs = vec![ROOT_PATH.to_string()];
    let mut end = 0;
    while let Some(i) = path[end..].find('/') {
        end += i;
        dirs.push(path[..end].to_string());
        end += 1;
    }
    dirs
}

// Whether the row at `row_path` (a file, a directory or the root) is or contains the file at `path`.
pub fn path_contains(row_path: &str, path: &str) -> bool {
    row_path == ROOT_PATH
        || row_path == path
        || path
            .strip_prefix(row_path)
            .is_some_and(|rest| rest.starts_with('/'))
}

pub fn keeps(filter: Option<&str>, path: &str) -> bool {
    filter.is_none_or(|f| path_matches(path, f))
}

// `rows` under a root row, when there are any; only the root row when it is closed.
fn under_root(rows: Vec<FileTreeRow>, open: bool) -> Vec<FileTreeRow> {
    if rows.is_empty() {
        return Vec::new();
    }
    if !open {
        return vec![FileTreeRow::root()];
    }
    std::iter::once(FileTreeRow::root())
        .chain(rows.into_iter().map(|row| FileTreeRow {
            depth: row.depth + 1,
            ..row
        }))
        .collect()
}

fn sort_files(node: &mut DirNode) {
    node.files.sort();
    for child in node.dirs.values_mut() {
        sort_files(child);
    }
}

// Returns whether any file under `node` is kept; directories without kept files are left out.
// Without `rows` (a closed directory) it only looks for a kept file, building no rows.
fn collect_rows(
    node: &DirNode,
    parent: &str,
    depth: usize,
    is_open: &impl Fn(&str) -> bool,
    keep: &impl Fn(&str) -> bool,
    mut rows: Option<&mut Vec<FileTreeRow>>,
) -> bool {
    let join = |name: &str| {
        if parent.is_empty() {
            name.to_string()
        } else {
            format!("{parent}/{name}")
        }
    };
    let mut any_kept = false;
    for (name, child) in &node.dirs {
        let path = join(name);
        let Some(rows) = rows.as_deref_mut() else {
            if collect_rows(child, &path, depth + 1, is_open, keep, None) {
                return true;
            }
            continue;
        };
        let mut child_rows = Vec::new();
        let open = is_open(&path);
        let sink = open.then_some(&mut child_rows);
        if !collect_rows(child, &path, depth + 1, is_open, keep, sink) {
            continue;
        }
        any_kept = true;
        rows.push(FileTreeRow {
            depth,
            name: name.clone(),
            path,
            file_index: None,
        });
        rows.extend(child_rows);
    }
    for (name, index) in &node.files {
        let path = join(name);
        if keep(&path) {
            any_kept = true;
            let Some(rows) = rows.as_deref_mut() else {
                return true;
            };
            rows.push(FileTreeRow {
                depth,
                name: name.clone(),
                path,
                file_index: Some(*index),
            });
        }
    }
    any_kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_file_tree_rows<'p>(paths: impl IntoIterator<Item = &'p str>) -> Vec<FileTreeRow> {
        FileTree::new(paths).rows(|_| true, |_| true)
    }

    fn row(depth: usize, name: &str, path: &str, file_index: Option<usize>) -> FileTreeRow {
        FileTreeRow {
            depth,
            name: name.into(),
            path: path.into(),
            file_index,
        }
    }

    #[test]
    fn test_tree_guides() {
        // like nvim-tree's indent markers: a line down each directory, "└" at its last entry
        let tree = FileTree::new(["a/b/x.rs", "a/b/w.rs", "a/y.rs", "z.rs"]);
        let state = FileTreeState::new(tree, true);
        let lines: Vec<String> = (0..state.rows().len())
            .map(|i| {
                let row = &state.rows()[i];
                let marker = if row.is_dir() { "> " } else { "" };
                format!("{}{marker}{}", state.guide(i), row.name)
            })
            .collect();
        assert_eq!(
            lines,
            [
                "> a",
                "  > b",
                "  │ │ w.rs",
                "  │ └ x.rs",
                "  └ y.rs",
                "  z.rs",
            ]
        );
    }

    #[test]
    fn test_build_file_tree_rows() {
        let paths = [
            "tests/cases/b.ts",
            "README.md",
            "tests/cases/a.ts",
            "src/main.rs",
        ];
        let actual = build_file_tree_rows(paths);

        let expected = vec![
            row(0, "src", "src", None),
            row(1, "main.rs", "src/main.rs", Some(3)),
            row(0, "tests", "tests", None),
            row(1, "cases", "tests/cases", None),
            row(2, "a.ts", "tests/cases/a.ts", Some(2)),
            row(2, "b.ts", "tests/cases/b.ts", Some(0)),
            row(0, "README.md", "README.md", Some(1)),
        ];
        assert_eq!(actual, expected);

        assert!(actual[2].contains("tests/cases/a.ts"));
        assert!(!actual[2].contains("testsuite/a.ts"));
        assert!(actual[1].contains("src/main.rs"));
    }

    #[test]
    fn test_file_tree_state() {
        let paths = ["src/a.rs", "src/b.rs", "README.md"];
        let mut state = FileTreeState::new(FileTree::new(paths), true);
        let visible = |state: &FileTreeState| -> Vec<String> {
            state.rows().iter().map(|r| r.path.clone()).collect()
        };
        assert_eq!(
            visible(&state),
            vec!["src", "src/a.rs", "src/b.rs", "README.md"]
        );

        // collapsing from a file goes to its directory first, then collapses it
        state.select_next();
        state.collapse();
        assert_eq!(state.selected().unwrap().path, "src");
        state.collapse();
        assert_eq!(visible(&state), vec!["src", "README.md"]);

        // the expanded state and selection carry over to another tree
        let other = state.with_tree(FileTree::new(["src/c.rs", "README.md"]));
        assert_eq!(visible(&other), vec!["src", "README.md"]);
        assert_eq!(other.selected().unwrap().path, "src");

        state.toggle();
        assert_eq!(
            visible(&state),
            vec!["src", "src/a.rs", "src/b.rs", "README.md"]
        );

        // closed by default
        let state = FileTreeState::new(FileTree::new(paths), false);
        assert_eq!(visible(&state), vec!["src", "README.md"]);
    }

    #[test]
    fn test_rooted_file_tree_state() {
        let paths = ["src/main.rs", "README.md"];
        let mut state = FileTreeState::rooted(FileTree::new(paths));
        let expected = vec![
            row(0, "/", ":/", None),
            row(1, "src", "src", None),
            row(2, "main.rs", "src/main.rs", Some(0)),
            row(1, "README.md", "README.md", Some(1)),
        ];
        assert_eq!(state.rows(), expected);
        assert!(state.rows()[0].contains("src/main.rs"));
        assert!(state.rows()[0].contains("README.md"));

        assert!(FileTreeState::rooted(FileTree::new([])).rows().is_empty());

        // a file at the top goes to the root row, then the root row collapses everything
        state.select_last();
        state.collapse();
        assert_eq!(state.selected().unwrap().path, ROOT_PATH);
        state.collapse();
        assert_eq!(state.rows(), vec![row(0, "/", ":/", None)]);

        // the selected path stays when the files change, else the position
        state.toggle();
        state.select_path("README.md");
        state.set_tree(FileTree::new(["src/main.rs", "README.md", "a.rs"]));
        assert_eq!(state.selected().unwrap().path, "README.md");
        state.set_tree(FileTree::new(["src/main.rs", "a.rs"]));
        assert_eq!(state.cursor(), 3);
        assert_eq!(state.selected().unwrap().path, "a.rs");
    }

    #[test]
    fn test_filter_and_search() {
        let paths = [
            "src/view/list.rs",
            "src/app.rs",
            "docs/list.md",
            "README.md",
        ];
        let mut state = FileTreeState::new(FileTree::new(paths), false);
        let visible = |state: &FileTreeState| -> Vec<String> {
            state.rows().iter().map(|r| r.path.clone()).collect()
        };
        assert_eq!(visible(&state), vec!["docs", "src", "README.md"]);

        // searching expands the directories hiding the match, and wraps around
        state.search("list", SearchDirection::Current);
        assert_eq!(state.selected().unwrap().path, "docs/list.md");
        state.search("list", SearchDirection::Next);
        assert_eq!(state.selected().unwrap().path, "src/view/list.rs");
        assert_eq!(
            visible(&state),
            vec![
                "docs",
                "docs/list.md",
                "src",
                "src/view",
                "src/view/list.rs",
                "src/app.rs",
                "README.md"
            ]
        );
        state.search("list", SearchDirection::Next);
        assert_eq!(state.selected().unwrap().path, "docs/list.md");
        state.search("list", SearchDirection::Previous);
        assert_eq!(state.selected().unwrap().path, "src/view/list.rs");

        // filtering shows only matching files, expanded, keeping the selection
        state.set_filter(Some("list"));
        assert_eq!(
            visible(&state),
            vec![
                "docs",
                "docs/list.md",
                "src",
                "src/view",
                "src/view/list.rs"
            ]
        );
        assert_eq!(state.selected().unwrap().path, "src/view/list.rs");

        state.set_filter(Some("nothing"));
        assert!(visible(&state).is_empty());
        state.set_filter(None);
        assert_eq!(visible(&state).len(), 7);
    }

    #[test]
    fn test_ancestors() {
        assert_eq!(ancestors("a/b/c.rs"), vec![":/", "a", "a/b"]);
        assert_eq!(ancestors("c.rs"), vec![":/"]);
    }

    #[test]
    fn test_collapsed_rows() {
        let tree = FileTree::new(["tests/cases/a.ts", "src/main.rs", "README.md"]);

        let actual = tree.rows(|_| false, |_| true);
        let expected = vec![
            row(0, "src", "src", None),
            row(0, "tests", "tests", None),
            row(0, "README.md", "README.md", Some(2)),
        ];
        assert_eq!(actual, expected);

        // an open directory inside a closed one stays hidden
        let actual = tree.rows(|path| path == "tests/cases", |_| true);
        assert_eq!(actual, expected);

        let actual = tree.rows(|path| path == "tests", |_| true);
        let expected = vec![
            row(0, "src", "src", None),
            row(0, "tests", "tests", None),
            row(1, "cases", "tests/cases", None),
            row(0, "README.md", "README.md", Some(2)),
        ];
        assert_eq!(actual, expected);
    }
}
