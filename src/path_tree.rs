// The files and directories of a list of paths as a tree, and its rows: the work tree's local
// changes, loaded in the background with their tree, and the files of a commit. What a panel shows
// of one, which directories are open and where the cursor is, is `FileTreeState`'s.

use std::{collections::BTreeMap, sync::Arc};

use crate::git::Entry;

// A row of a file tree as displayed: directories first, then files, each sorted by name. Its
// texts are those of the tree, shared rather than copied: a tree of 100k files is laid out in
// rows again on each change, see `FileTreeState::rebuild_rows`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTreeRow {
    pub depth: usize,
    pub name: Arc<str>,
    // the full path of the file or directory, or `ROOT_PATH` for the root row; git takes it as a
    // pathspec, and the tree keeps its selection and open directories by it
    pub path: Arc<str>,
    pub kind: RowKind,
}

// What a row of a file tree is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    // the "/" row standing for everything, see `FileTreeRow::root`
    Root,
    Dir,
    // a file, by its index in the paths the tree was built from
    File(usize),
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
            kind: RowKind::Root,
        }
    }

    pub fn is_root(&self) -> bool {
        self.kind == RowKind::Root
    }

    // A directory, or the root row containing them all.
    pub fn is_dir(&self) -> bool {
        !matches!(self.kind, RowKind::File(_))
    }

    pub fn file_index(&self) -> Option<usize> {
        match self.kind {
            RowKind::File(index) => Some(index),
            RowKind::Root | RowKind::Dir => None,
        }
    }

    // The file or directory, None for the root row.
    pub fn entry(&self) -> Option<Entry<'_>> {
        match self.kind {
            RowKind::Root => None,
            RowKind::Dir => Some(Entry::Dir(&self.path)),
            RowKind::File(_) => Some(Entry::File(&self.path)),
        }
    }

    // Whether `path` is this file, or is inside this directory.
    pub fn contains(&self, path: &str) -> bool {
        path_contains(&self.path, path)
    }
}

// Built once, then only read: shared rather than copied, like with the files it was loaded with
// and by the panels showing it, see `FilesSnapshot`.
#[derive(Debug, Default, Clone)]
pub struct FileTree {
    root: Arc<DirNode>,
}

// A directory of a tree, by its full path: "" for the top one.
#[derive(Debug, Default, Clone)]
struct DirNode {
    path: Arc<str>,
    dirs: BTreeMap<Arc<str>, DirNode>,
    files: Vec<FileNode>,
}

// A file of a tree, its name and full path, by its index in the paths the tree was built from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct FileNode {
    name: Arc<str>,
    path: Arc<str>,
    index: usize,
}

impl FileTree {
    pub fn new<'p>(paths: impl IntoIterator<Item = &'p str>) -> Self {
        let mut root = DirNode::default();
        for (index, path) in paths.into_iter().enumerate() {
            // an untracked repository inside is listed as `nested/`, a file named `nested`
            let path = tree_path(path);
            let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
            let mut node = &mut root;
            let mut end = 0;
            for part in dir.split('/').filter(|_| !dir.is_empty()) {
                end += part.len();
                let full = &dir[..end];
                end += 1;
                node = node.dirs.entry(part.into()).or_insert_with(|| DirNode {
                    path: full.into(),
                    ..Default::default()
                });
            }
            node.files.push(FileNode {
                name: name.into(),
                path: path.into(),
                index,
            });
        }
        sort_files(&mut root);
        Self {
            root: Arc::new(root),
        }
    }

    // The index of the file at `path` in the paths the tree was built from: down its directories,
    // then among their files, sorted by name; without going through every file.
    pub fn file_index(&self, path: &str) -> Option<usize> {
        let path = tree_path(path);
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
        let mut node: &DirNode = &self.root;
        for part in dir.split('/').filter(|_| !dir.is_empty()) {
            node = node.dirs.get(part)?;
        }
        let at = node.files.partition_point(|file| &*file.name < name);
        let file = node.files.get(at).filter(|file| &*file.name == name)?;
        Some(file.index)
    }

    // Whether `path` is a directory of the tree.
    pub fn is_dir(&self, path: &str) -> bool {
        let mut node: &DirNode = &self.root;
        for part in path.split('/') {
            let Some(dir) = node.dirs.get(part) else {
                return false;
            };
            node = dir;
        }
        true
    }

    // The rows of the files for which `keep` is true and their directories, descending only into the
    // directories for which `is_open` is true.
    pub fn rows(
        &self,
        is_open: impl Fn(&str) -> bool,
        keep: impl Fn(&str) -> bool,
    ) -> Vec<FileTreeRow> {
        let mut rows = Vec::new();
        collect_rows(&self.root, 0, &is_open, &keep, Some(&mut rows));
        rows
    }

    // The files for which `keep` is true as a list, a row each named by its path, sorted by it.
    pub fn list_rows(&self, keep: impl Fn(&str) -> bool) -> Vec<FileTreeRow> {
        fn collect(node: &DirNode, keep: &impl Fn(&str) -> bool, rows: &mut Vec<FileTreeRow>) {
            for child in node.dirs.values() {
                collect(child, keep, rows);
            }
            let kept = node.files.iter().filter(|file| keep(&file.path));
            rows.extend(kept.map(|file| FileTreeRow {
                depth: 0,
                name: file.path.clone(),
                path: file.path.clone(),
                kind: RowKind::File(file.index),
            }));
        }
        let mut rows = Vec::new();
        collect(&self.root, &keep, &mut rows);
        rows.sort_unstable_by(|a, b| a.path.cmp(&b.path));
        rows
    }
}

// The path of a file's row: an untracked repository inside, which git lists as `nested/`, is a
// file named `nested`, see `FileTree::new`.
pub fn tree_path(path: &str) -> &str {
    path.trim_end_matches('/')
}

// Whether the row at `row_path` (a file, a directory or the root) is or contains the file at `path`.
pub fn path_contains(row_path: &str, path: &str) -> bool {
    row_path == ROOT_PATH
        || row_path == path
        || path
            .strip_prefix(row_path)
            .is_some_and(|rest| rest.starts_with('/'))
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
    depth: usize,
    is_open: &impl Fn(&str) -> bool,
    keep: &impl Fn(&str) -> bool,
    mut rows: Option<&mut Vec<FileTreeRow>>,
) -> bool {
    let mut any_kept = false;
    for (name, child) in &node.dirs {
        let Some(rows) = rows.as_deref_mut() else {
            if collect_rows(child, depth + 1, is_open, keep, None) {
                return true;
            }
            continue;
        };
        // its own row first, taken back when it has nothing kept
        let at = rows.len();
        rows.push(FileTreeRow {
            depth,
            name: name.clone(),
            path: child.path.clone(),
            kind: RowKind::Dir,
        });
        let sink = is_open(&child.path).then_some(&mut *rows);
        if collect_rows(child, depth + 1, is_open, keep, sink) {
            any_kept = true;
        } else {
            rows.truncate(at);
        }
    }
    for file in &node.files {
        if keep(&file.path) {
            any_kept = true;
            let Some(rows) = rows.as_deref_mut() else {
                return true;
            };
            rows.push(FileTreeRow {
                depth,
                name: file.name.clone(),
                path: file.path.clone(),
                kind: RowKind::File(file.index),
            });
        }
    }
    any_kept
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_file_index() {
        let tree = super::FileTree::new(["b/z.rs", "a.rs", "b/c/y.rs", "b/x.rs"]);
        assert_eq!(tree.file_index("a.rs"), Some(1));
        assert_eq!(tree.file_index("b/x.rs"), Some(3));
        assert_eq!(tree.file_index("b/z.rs"), Some(0));
        assert_eq!(tree.file_index("b/c/y.rs"), Some(2));
        assert_eq!(tree.file_index("b"), None);
        assert_eq!(tree.file_index("b/w.rs"), None);
        assert_eq!(tree.file_index("c/x.rs"), None);
    }
}
