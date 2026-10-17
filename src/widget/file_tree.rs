use std::{
    cell::Cell,
    collections::{BTreeMap, HashSet},
};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::{
    app::AppContext,
    color::ColorTheme,
    search::SearchOptions,
    widget::{
        style::{
            change_span, fold_marker, mark_picked_row, mark_selected_row, placeholder, scroll_to,
        },
        tree_query::{find_around, highlighted_text, name_matches, SearchDirection},
    },
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

// How a tree starts: its directories open or closed, or open under a root row standing for
// everything, see `FileTreeRow::root`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeShape {
    Open,
    Closed,
    Rooted,
}

// A tree whose directories can be expanded and collapsed, with a cursor on one of its rows.
#[derive(Debug)]
pub struct FileTreeState {
    tree: FileTree,
    shape: TreeShape,
    // directories whose state differs from how they start, see `TreeShape`
    toggled: HashSet<String>,
    // only files whose path matches are shown, with all directories expanded; matched with the
    // search options of when it was set
    filter: Option<(String, SearchOptions)>,
    rows: Vec<FileTreeRow>,
    // the indent markers of the rows, see `tree_guides`
    guides: Vec<String>,
    cursor: usize,
    // the first row shown, moved as little as can be to show the cursor as the tree is drawn
    offset: Cell<usize>,
}

impl FileTreeState {
    pub fn new(tree: FileTree, shape: TreeShape) -> Self {
        let mut state = Self {
            tree,
            shape,
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
            shape: self.shape,
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
        self.tree.rows(|_| true, |path| self.keeps(path))
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn selected(&self) -> Option<&FileTreeRow> {
        self.rows.get(self.cursor)
    }

    pub fn is_open(&self, path: &str) -> bool {
        let open_by_default = self.shape != TreeShape::Closed;
        self.filter.is_some() || open_by_default != self.toggled.contains(path)
    }

    pub fn filter(&self) -> Option<&str> {
        self.filter.as_ref().map(|(filter, _)| filter.as_str())
    }

    pub fn set_filter(&mut self, filter: Option<&str>, options: SearchOptions) {
        self.filter = filter.map(|f| (f.to_string(), options));
        self.rebuild_rows();
    }

    // Whether `path` passes the filter.
    pub fn keeps(&self, path: &str) -> bool {
        self.filter
            .as_ref()
            .is_none_or(|(filter, options)| name_matches(path, filter, *options))
    }

    // Selects the next match of `query`, expanding the directories that hide it.
    pub fn search(&mut self, query: &str, direction: SearchDirection, options: SearchOptions) {
        let all_rows = self.all_rows();
        let current = self.selected().map(|r| r.path.as_str());
        let path = find_match(&all_rows, current, query, direction, options);
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
            None if self.shape == TreeShape::Rooted => ROOT_PATH.to_string(),
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
        let rows = self
            .tree
            .rows(|path| self.is_open(path), |path| self.keeps(path));
        self.rows = if self.shape == TreeShape::Rooted {
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

// The path of the match of `query` in `rows` from `current` in `direction`, wrapping around.
pub fn find_match(
    rows: &[FileTreeRow],
    current: Option<&str>,
    query: &str,
    direction: SearchDirection,
    options: SearchOptions,
) -> Option<String> {
    find_next(rows, current, direction, |row| {
        name_matches(&row.path, query, options)
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

// The row of a directory in a file tree: after its `guide` (see `tree_guides`), open or closed,
// its name in the directory color with the match of `search` highlighted.
pub fn dir_line(
    row: &FileTreeRow,
    guide: &str,
    open: bool,
    search: Option<&str>,
    options: SearchOptions,
    theme: &ColorTheme,
) -> Line<'static> {
    let mut spans = vec![guide_span(guide, theme), fold_marker(open).into()];
    spans.extend(
        highlighted_text(
            &row.name,
            row.name.len(),
            Style::default(),
            search,
            options,
            theme,
        )
        .into_iter()
        .map(|span| match span.style.fg {
            Some(_) => span,
            None => span.fg(theme.file_tree_dir_fg),
        }),
    );
    Line::from(spans)
}

// The indent markers of a row, in the color of the dividers.
pub fn guide_span(guide: &str, theme: &ColorTheme) -> Span<'static> {
    Span::raw(guide.to_string()).fg(theme.divider_fg)
}

// Draws the rows of `tree` in `area`, like every file tree: the directories open or closed, the
// files with their change letter from `symbol` (by their index) when they have one, the match of
// `search` highlighted; the `picked` rows and, with `cursor` (whether the tree has the focus), the
// selected one highlighted across the row, the lines of the tree keeping their color; `empty`
// when there is no row. Returns the first row shown, for the scrollbar.
#[allow(clippy::too_many_arguments)]
pub fn render_tree(
    buf: &mut Buffer,
    area: Rect,
    tree: &FileTreeState,
    symbol: impl Fn(usize) -> Option<&'static str>,
    search: Option<&str>,
    picked: impl Fn(&str) -> bool,
    cursor: Option<bool>,
    empty: &str,
    ctx: &AppContext,
) -> usize {
    let theme = &ctx.color_theme;
    let options = ctx.search_options.get();
    let rows = tree.rows();
    if rows.is_empty() {
        Paragraph::new(placeholder(empty, theme)).render(area, buf);
        return 0;
    }
    let height = area.height as usize;
    // no room for a row, like in a very short terminal
    if height == 0 {
        return 0;
    }
    let offset = tree.scroll(height, ctx.ui_config.common.scrolloff as usize);
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(index, row)| {
            let guide = tree.guide(index);
            let Some(file) = row.file_index else {
                return dir_line(row, guide, tree.is_open(&row.path), search, options, theme);
            };
            let mut spans = vec![guide_span(guide, theme)];
            if let Some(symbol) = symbol(file) {
                spans.extend([change_span(symbol, theme), Span::raw(" ")]);
            }
            spans.extend(highlighted_text(
                &row.name,
                row.name.len(),
                Style::default(),
                search,
                options,
                theme,
            ));
            Line::from(spans)
        })
        .collect();
    Paragraph::new(lines).fg(theme.fg).render(area, buf);

    for (y, row) in (area.top()..area.bottom()).zip(rows.iter().skip(offset)) {
        if picked(&row.path) {
            mark_picked_row(buf, Rect::new(area.x, y, area.width, 1), theme);
        }
    }
    if let Some(focused) = cursor {
        let y = area.y + (tree.cursor() - offset) as u16;
        mark_selected_row(buf, area, y, focused, theme);
    }
    unhighlight_guides(buf, area, tree, offset, theme);
    offset
}

// Gives the indent markers at the start of the rows `area` shows from `offset` their own color
// back, over the highlight of the selected and picked rows.
pub fn unhighlight_guides(
    buf: &mut Buffer,
    area: Rect,
    tree: &FileTreeState,
    offset: usize,
    theme: &ColorTheme,
) {
    for (y, index) in (area.top()..area.bottom()).zip(offset..tree.rows().len()) {
        keep_guide_color(buf, area, y, tree.guide(index), theme);
    }
}

// Gives the indent markers of the row at `y` of `area` their color back, see `unhighlight_guides`.
pub fn keep_guide_color(buf: &mut Buffer, area: Rect, y: u16, guide: &str, theme: &ColorTheme) {
    // the background of a highlight stays, the whole row shows selected
    let style = Style::new()
        .fg(theme.divider_fg)
        .remove_modifier(Modifier::BOLD);
    // the markers are one column each
    let width = (guide.chars().count() as u16).min(area.width);
    buf.set_style(Rect::new(area.x, y, width, 1), style);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ui::{
        assert_guides_keep_color, assert_only_bg_changed, assert_row_bg, row_text,
    };

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
        let state = FileTreeState::new(tree, TreeShape::Open);
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
        let mut state = FileTreeState::new(FileTree::new(paths), TreeShape::Open);
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
        let state = FileTreeState::new(FileTree::new(paths), TreeShape::Closed);
        assert_eq!(visible(&state), vec!["src", "README.md"]);
    }

    #[test]
    fn test_rooted_file_tree_state() {
        let paths = ["src/main.rs", "README.md"];
        let mut state = FileTreeState::new(FileTree::new(paths), TreeShape::Rooted);
        let expected = vec![
            row(0, "/", ":/", None),
            row(1, "src", "src", None),
            row(2, "main.rs", "src/main.rs", Some(0)),
            row(1, "README.md", "README.md", Some(1)),
        ];
        assert_eq!(state.rows(), expected);
        assert!(state.rows()[0].contains("src/main.rs"));
        assert!(state.rows()[0].contains("README.md"));

        assert!(FileTreeState::new(FileTree::new([]), TreeShape::Rooted)
            .rows()
            .is_empty());

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
        let mut state = FileTreeState::new(FileTree::new(paths), TreeShape::Closed);
        let visible = |state: &FileTreeState| -> Vec<String> {
            state.rows().iter().map(|r| r.path.clone()).collect()
        };
        assert_eq!(visible(&state), vec!["docs", "src", "README.md"]);

        // searching expands the directories hiding the match, and wraps around
        state.search("list", SearchDirection::Current, SearchOptions::default());
        assert_eq!(state.selected().unwrap().path, "docs/list.md");
        state.search("list", SearchDirection::Next, SearchOptions::default());
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
        state.search("list", SearchDirection::Next, SearchOptions::default());
        assert_eq!(state.selected().unwrap().path, "docs/list.md");
        state.search("list", SearchDirection::Previous, SearchOptions::default());
        assert_eq!(state.selected().unwrap().path, "src/view/list.rs");

        // filtering shows only matching files, expanded, keeping the selection
        state.set_filter(Some("list"), SearchOptions::default());
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

        state.set_filter(Some("nothing"), SearchOptions::default());
        assert!(visible(&state).is_empty());
        state.set_filter(None, SearchOptions::default());
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

    // Draws `tree` in 20 columns and `height` rows, from column 1, the change letter "M" on each
    // file and `picked` highlighted.
    fn draw_tree(tree: &FileTreeState, height: u16, focused: bool, picked: &str) -> Buffer {
        let ctx = crate::test_support::ui::test_ctx();
        let mut buf = Buffer::empty(Rect::new(0, 0, 23, height));
        let area = Rect::new(1, 0, 20, height);
        let symbol = |_| Some("M");
        let is_picked = |path: &str| path == picked;
        render_tree(
            &mut buf,
            area,
            tree,
            symbol,
            None,
            is_picked,
            Some(focused),
            "",
            &ctx,
        );
        buf
    }

    #[test]
    fn test_tree_rows_look_like_the_other_lists() {
        let theme = ColorTheme::default();
        let mut tree =
            FileTreeState::new(FileTree::new(["a/x.rs", "a/y.rs", "z.rs"]), TreeShape::Open);
        for focused in [true, false] {
            tree.select_path("z.rs");
            let plain = draw_tree(&tree, 5, focused, "");
            tree.select_path("a/x.rs");
            let selected = draw_tree(&tree, 5, focused, "");
            // the change letter and the lines keep their colors
            let row = Rect::new(1, 1, 20, 1);
            assert_row_bg(&selected, row, theme.list_selected_bg);
            assert_only_bg_changed(&plain, &selected, row);
            assert_guides_keep_color(&selected, row, &theme);
        }
        // a picked row: across it too
        let buf = draw_tree(&tree, 5, true, "a/y.rs");
        let row = Rect::new(1, 2, 20, 1);
        assert_row_bg(&buf, row, theme.list_picked_bg);
        assert_guides_keep_color(&buf, row, &theme);
    }

    #[test]
    fn test_tiny_trees_with_scrolloff() {
        // a scrolloff larger than the tree is tall: drawn without a crash, the cursor shown
        let mut ui_config = crate::config::UiConfig::default();
        ui_config.common.scrolloff = 5;
        let ctx = crate::test_support::ui::test_ctx_of(ColorTheme::default(), ui_config);
        let paths: Vec<String> = (0..10).map(|i| format!("f{i}.rs")).collect();
        let mut tree = FileTreeState::new(
            FileTree::new(paths.iter().map(String::as_str)),
            TreeShape::Open,
        );
        for height in 0..4 {
            for index in 0..10 {
                tree.select_index(index);
                let mut buf = Buffer::empty(Rect::new(0, 0, 23, height.max(1)));
                let area = Rect::new(1, 0, 20, height);
                let offset = render_tree(
                    &mut buf,
                    area,
                    &tree,
                    |_| None,
                    None,
                    |_| false,
                    Some(true),
                    "",
                    &ctx,
                );
                if height > 0 {
                    assert!((offset..offset + height as usize).contains(&index));
                }
            }
        }
    }

    #[test]
    fn test_tree_scrolls_like_the_other_lists() {
        let paths: Vec<String> = (0..10).map(|i| format!("f{i}.rs")).collect();
        let mut tree = FileTreeState::new(
            FileTree::new(paths.iter().map(String::as_str)),
            TreeShape::Open,
        );
        let top = |tree: &FileTreeState| row_text(&draw_tree(tree, 4, true, ""), 0);
        // down past the bottom: the view follows the cursor
        tree.select_index(5);
        assert!(top(&tree).contains("f2.rs"), "{}", top(&tree));
        // up within the view: the view stays
        tree.select_index(3);
        assert!(top(&tree).contains("f2.rs"), "{}", top(&tree));
        // up past the top: it follows again
        tree.select_index(0);
        assert!(top(&tree).contains("f0.rs"), "{}", top(&tree));
    }
}
