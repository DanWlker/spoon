use std::collections::{BTreeMap, HashSet};

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
        if self.is_root() {
            return true;
        }
        match self.file_index {
            Some(_) => self.path == path,
            None => path
                .strip_prefix(self.path.as_str())
                .is_some_and(|rest| rest.starts_with('/')),
        }
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

    // The rows of the tree, descending only into the directories for which `is_open` is true.
    pub fn rows(&self, is_open: impl Fn(&str) -> bool) -> Vec<FileTreeRow> {
        let mut rows = Vec::new();
        collect_rows(&self.root, "", 0, &is_open, &mut rows);
        rows
    }
}

// A tree whose directories can be expanded and collapsed, with a cursor on one of its rows.
#[derive(Debug)]
pub struct FileTreeState {
    tree: FileTree,
    open_by_default: bool,
    // directories whose state differs from `open_by_default`
    toggled: HashSet<String>,
    rows: Vec<FileTreeRow>,
    cursor: usize,
}

impl FileTreeState {
    pub fn new(tree: FileTree, open_by_default: bool) -> Self {
        let mut state = Self {
            tree,
            open_by_default,
            toggled: HashSet::new(),
            rows: Vec::new(),
            cursor: 0,
        };
        state.rebuild_rows();
        state
    }

    // The same expanded directories and selected path over another tree, where they exist.
    pub fn with_tree(&self, tree: FileTree) -> Self {
        let mut state = Self {
            tree,
            open_by_default: self.open_by_default,
            toggled: self.toggled.clone(),
            rows: Vec::new(),
            cursor: 0,
        };
        state.rebuild_rows();
        if let Some(path) = self.selected().map(|r| r.path.as_str()) {
            state.select_path(path);
        }
        state
    }

    pub fn rows(&self) -> &[FileTreeRow] {
        &self.rows
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn selected(&self) -> Option<&FileTreeRow> {
        self.rows.get(self.cursor)
    }

    pub fn is_open(&self, path: &str) -> bool {
        self.open_by_default != self.toggled.contains(path)
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
        } else if let Some((parent, _)) = self.selected().and_then(|r| r.path.rsplit_once('/')) {
            let parent = parent.to_string();
            self.select_path(&parent);
        }
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
        if !self.toggled.remove(&path) {
            self.toggled.insert(path);
        }
        self.rebuild_rows();
    }

    fn rebuild_rows(&mut self) {
        let selected = self.selected().map(|r| r.path.clone());
        self.rows = self.tree.rows(|path| self.is_open(path));
        self.cursor = 0;
        if let Some(path) = selected {
            self.select_path(&path);
        }
    }
}

// The rows of a fully expanded tree of `paths`, under a root row when there are any.
pub fn build_rooted_file_tree_rows<'p>(
    paths: impl IntoIterator<Item = &'p str>,
) -> Vec<FileTreeRow> {
    let rows = build_file_tree_rows(paths);
    if rows.is_empty() {
        return rows;
    }
    std::iter::once(FileTreeRow::root())
        .chain(rows.into_iter().map(|row| FileTreeRow {
            depth: row.depth + 1,
            ..row
        }))
        .collect()
}

// The rows of a fully expanded tree of `paths`.
pub fn build_file_tree_rows<'p>(paths: impl IntoIterator<Item = &'p str>) -> Vec<FileTreeRow> {
    FileTree::new(paths).rows(|_| true)
}

fn sort_files(node: &mut DirNode) {
    node.files.sort();
    for child in node.dirs.values_mut() {
        sort_files(child);
    }
}

fn collect_rows(
    node: &DirNode,
    parent: &str,
    depth: usize,
    is_open: &impl Fn(&str) -> bool,
    rows: &mut Vec<FileTreeRow>,
) {
    let join = |name: &str| {
        if parent.is_empty() {
            name.to_string()
        } else {
            format!("{parent}/{name}")
        }
    };
    for (name, child) in &node.dirs {
        let path = join(name);
        rows.push(FileTreeRow {
            depth,
            name: name.clone(),
            path: path.clone(),
            file_index: None,
        });
        if is_open(&path) {
            collect_rows(child, &path, depth + 1, is_open, rows);
        }
    }
    for (name, index) in &node.files {
        rows.push(FileTreeRow {
            depth,
            name: name.clone(),
            path: join(name),
            file_index: Some(*index),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(depth: usize, name: &str, path: &str, file_index: Option<usize>) -> FileTreeRow {
        FileTreeRow {
            depth,
            name: name.into(),
            path: path.into(),
            file_index,
        }
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
    fn test_build_rooted_file_tree_rows() {
        let actual = build_rooted_file_tree_rows(["src/main.rs", "README.md"]);
        let expected = vec![
            row(0, "/", ":/", None),
            row(1, "src", "src", None),
            row(2, "main.rs", "src/main.rs", Some(0)),
            row(1, "README.md", "README.md", Some(1)),
        ];
        assert_eq!(actual, expected);
        assert!(actual[0].contains("src/main.rs"));
        assert!(actual[0].contains("README.md"));

        assert!(build_rooted_file_tree_rows([]).is_empty());
    }

    #[test]
    fn test_collapsed_rows() {
        let tree = FileTree::new(["tests/cases/a.ts", "src/main.rs", "README.md"]);

        let actual = tree.rows(|_| false);
        let expected = vec![
            row(0, "src", "src", None),
            row(0, "tests", "tests", None),
            row(0, "README.md", "README.md", Some(2)),
        ];
        assert_eq!(actual, expected);

        // an open directory inside a closed one stays hidden
        let actual = tree.rows(|path| path == "tests/cases");
        assert_eq!(actual, expected);

        let actual = tree.rows(|path| path == "tests");
        let expected = vec![
            row(0, "src", "src", None),
            row(0, "tests", "tests", None),
            row(1, "cases", "tests/cases", None),
            row(0, "README.md", "README.md", Some(2)),
        ];
        assert_eq!(actual, expected);
    }
}
