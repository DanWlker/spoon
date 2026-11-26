use std::{cell::Cell, collections::HashSet, ops::Range, sync::Arc};

use rustc_hash::{FxHashMap, FxHashSet};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

pub use crate::git::Entry;
use crate::path_tree::{tree_path, FileTree, FileTreeRow, RowKind, ROOT_PATH};
use crate::{
    color::ColorTheme,
    context::AppContext,
    keybind::UserEvent,
    layout::{RowMap, Target},
    pointer::OnPicks,
    search::SearchOptions,
    widget::{
        picks::Picks,
        style::{
            change_span, check_mark, fold_marker, mark_picked_row, mark_selected_bar,
            mark_selected_row, placeholder, ListCursor,
        },
        tree_query::{find_around, highlighted_text, name_matches, SearchDirection},
    },
};

// How the directories of a tree start, before they are expanded or collapsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Folders {
    Open,
    Closed,
}

// Whether a tree has a root row standing for everything above its top directory, see
// `FileTreeRow::root`; it starts open, whatever `Folders` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Root {
    Shown,
    Hidden,
}

// How a tree shows its files: in their directories, or a list of their paths, like lazygit's
// flat view; each panel keeps its own, toggled with a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeView {
    Tree,
    List,
}

// A tree whose directories can be expanded and collapsed, with a cursor on one of its rows.
#[derive(Debug, Clone)]
pub struct FileTreeState {
    tree: FileTree,
    folders: Folders,
    root: Root,
    view: TreeView,
    // directories whose state differs from how they start, see `Folders`
    toggled: HashSet<String>,
    // only files whose path matches are shown, with all directories expanded; matched with the
    // search options of when it was set
    filter: Option<(String, SearchOptions)>,
    rows: Vec<FileTreeRow>,
    // the indent markers of the rows, see `tree_guides`
    guides: Vec<Guide>,
    // the selected row and the rows shown, which drawing the tree moves to show it
    cursor: Cell<ListCursor>,
    // how many times the rows changed under a visual selection, for it to keep what it picked
    // over them, see `TreeSelection::sync_visual`
    rebuilds: u64,
}

impl FileTreeState {
    pub fn new(tree: FileTree, folders: Folders, root: Root, view: TreeView) -> Self {
        let mut state = Self {
            tree,
            folders,
            root,
            view,
            toggled: HashSet::new(),
            filter: None,
            rows: Vec::new(),
            guides: Vec::new(),
            cursor: Cell::default(),
            rebuilds: 0,
        };
        state.rebuild_rows();
        state
    }

    // The same expanded directories and selected path over another tree, where they exist.
    pub fn with_tree(&self, tree: FileTree) -> Self {
        let mut state = Self {
            tree,
            folders: self.folders,
            root: self.root,
            view: self.view,
            toggled: self.toggled.clone(),
            filter: self.filter.clone(),
            rows: Vec::new(),
            guides: Vec::new(),
            cursor: self.cursor.clone(),
            rebuilds: self.rebuilds,
        };
        state.rebuild_rows_keeping(self.selected_path(), self.visual_start_path());
        state
    }

    // Replaces the tree, like after the files changed, keeping the expanded directories and the
    // selected path, or else the position of the cursor.
    // The tree before is freed on another thread, which takes long for many files.
    pub fn set_tree(&mut self, tree: FileTree) {
        crate::background::drop_elsewhere(std::mem::replace(&mut self.tree, tree));
        self.rebuild_rows();
    }

    // The index of the file at `path`, see `FileTree::file_index`.
    pub fn file_index(&self, path: &str) -> Option<usize> {
        self.tree.file_index(path)
    }

    pub fn rows(&self) -> &[FileTreeRow] {
        &self.rows
    }

    // The first row shown in `height` rows, showing the cursor with `scrolloff` rows around it,
    // moved as little as can be since the last time, like the other lists; see `scroll_to`.
    pub fn scroll(&self, height: usize, scrolloff: usize) -> usize {
        let mut cursor = self.cursor.get();
        let offset = cursor.fit(height, self.rows.len(), scrolloff);
        self.cursor.set(cursor);
        offset
    }

    // The indent markers before the row at `index`, see `tree_guides`, drawn only for the rows
    // shown.
    pub fn guide(&self, index: usize) -> String {
        self.guides.get(index).map(Guide::text).unwrap_or_default()
    }

    // All rows with every directory expanded, passing the filter, to search in.
    pub fn all_rows(&self) -> Vec<FileTreeRow> {
        match self.view {
            TreeView::Tree => self.tree.rows(|_| true, |path| self.keeps(path)),
            TreeView::List => self.list_rows(),
        }
    }

    pub fn view(&self) -> TreeView {
        self.view
    }

    // Shows the files in their directories, or as a list, keeping the selected path when it is
    // still shown.
    pub fn toggle_view(&mut self) {
        self.view = match self.view {
            TreeView::Tree => TreeView::List,
            TreeView::List => TreeView::Tree,
        };
        self.rebuild_rows();
    }

    // The files passing the filter, a row each named by its path, sorted by it.
    fn list_rows(&self) -> Vec<FileTreeRow> {
        self.tree.list_rows(|path| self.keeps(path))
    }

    pub fn cursor(&self) -> usize {
        self.cursor.get().selected()
    }

    // Whether the files are being selected from where the visual selection started.
    pub fn is_visual(&self) -> bool {
        self.cursor.get().is_visual()
    }

    // Starts or ends a visual selection at the cursor.
    pub fn toggle_visual(&mut self) {
        self.cursor.get_mut().toggle_visual();
    }

    pub fn cancel_visual(&mut self) {
        self.cursor.get_mut().cancel_visual();
    }

    // The rows a visual selection covers, from where it started to the cursor; None when it isn't
    // visual.
    pub fn visual_range(&self) -> Option<Range<usize>> {
        self.cursor.get().visual_range(self.rows.len())
    }

    pub fn selected(&self) -> Option<&FileTreeRow> {
        self.rows.get(self.cursor())
    }

    pub fn is_open(&self, path: &str) -> bool {
        let open_by_default = path == ROOT_PATH || self.folders == Folders::Open;
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

    // Selects `path`, expanding the directories that hide it.
    pub fn reveal(&mut self, path: Option<&str>) {
        let Some(path) = path else {
            return;
        };
        for dir in ancestors(path) {
            if !self.is_open(dir) {
                self.toggled_mut(dir.to_string());
            }
        }
        self.rebuild_rows();
        self.select_path(path);
    }

    // Moves the cursor to the row at `index`, or the last one.
    pub fn select_index(&mut self, index: usize) {
        let total = self.rows.len();
        self.cursor.get_mut().select(index, total);
    }

    pub fn select_path(&mut self, path: &str) {
        if let Some(pos) = self.rows.iter().position(|r| *r.path == *path) {
            self.select_index(pos);
        }
    }

    // Moves by every key scrolling the tree: the movement keys, ctrl-d and ctrl-u, and the page
    // and line keys; returns whether `event` was one of them.
    pub fn navigate(&self, event: UserEvent) -> bool {
        let total = self.rows.len();
        let mut cursor = self.cursor.get();
        let handled = cursor.navigate(event, total);
        self.cursor.set(cursor);
        handled
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
            None if self.root == Root::Shown => ROOT_PATH.to_string(),
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
            .map(|r| r.path.to_string())
    }

    fn toggle_dir(&mut self, path: String) {
        // every folder shows open under a filter, see `is_open`: one toggled unseen would be
        // closed once it is cleared
        if self.filter.is_some() {
            return;
        }
        self.toggled_mut(path);
        self.rebuild_rows();
    }

    fn toggled_mut(&mut self, path: String) {
        if !self.toggled.remove(&path) {
            self.toggled.insert(path);
        }
    }

    fn selected_path(&self) -> Option<Arc<str>> {
        self.selected().map(|r| r.path.clone())
    }

    fn visual_start_path(&self) -> Option<Arc<str>> {
        let start = self.cursor.get().visual_start()?;
        self.rows.get(start).map(|r| r.path.clone())
    }

    fn rebuild_rows(&mut self) {
        self.rebuild_rows_keeping(self.selected_path(), self.visual_start_path());
    }

    // Keeps the `selected` path, or else the position of the cursor, and a visual selection
    // starting from `visual_start` wherever its row is now, ending it when it is no longer shown.
    // The rows before are freed on another thread, which takes long for many.
    fn rebuild_rows_keeping(&mut self, selected: Option<Arc<str>>, visual_start: Option<Arc<str>>) {
        let rows = match self.view {
            TreeView::Tree => self
                .tree
                .rows(|path| self.is_open(path), |path| self.keeps(path)),
            TreeView::List => self.list_rows(),
        };
        let rows = if self.root == Root::Shown {
            under_root(rows, self.is_open(ROOT_PATH))
        } else {
            rows
        };
        let before = std::mem::replace(&mut self.rows, rows);
        let changed = before.len() != self.rows.len()
            || before.iter().zip(&self.rows).any(|(a, b)| a.path != b.path);
        if visual_start.is_some() && changed {
            self.rebuilds += 1;
        }
        crate::background::drop_elsewhere(before);
        self.guides = tree_guides(self.rows.iter().map(|r| (r.depth, r.is_dir())));
        let start = visual_start.and_then(|path| self.rows.iter().position(|r| r.path == path));
        self.cursor.get_mut().set_visual_start(start);
        self.select_index(self.cursor());
        if let Some(path) = selected {
            self.select_path(&path);
        }
    }
}

// The rows of a tree picked with `v`, kept after the visual selection ends until they are cleared:
// the local changes sections, the changes and files tabs of the commit details. The same type keeps
// the visual selection, so a filter, a collapse or a reload cannot leave it indexing rows that are
// gone.
#[derive(Debug, Default, Clone)]
pub struct TreeSelection {
    // shared with the rows, see `FileTreeRow::path`: a visual selection syncs them on each move
    picked: Picks<Arc<str>>,
    // the rows last synced, see `FileTreeState::rebuilds`
    synced: u64,
}

impl TreeSelection {
    pub fn is_empty(&self) -> bool {
        self.picked.is_empty()
    }

    pub fn len(&self) -> usize {
        self.picked.len()
    }

    // In the order they were picked.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.picked.iter().map(|path| &**path)
    }

    #[cfg(test)]
    pub fn names(&self) -> impl Iterator<Item = &str> {
        let mut names: Vec<&str> = self.iter().collect();
        names.sort_unstable();
        names.into_iter()
    }

    // Sorted, like the tree: the order the paths were picked in is the tree's too, but for a
    // visual selection added to those picked before.
    pub fn paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self.iter().map(String::from).collect();
        paths.sort_unstable();
        paths
    }

    // The one picked path, or None when none or more than one is picked.
    pub fn single_path(&self) -> Option<&str> {
        match self.picked.as_slice() {
            [path] => Some(path),
            _ => None,
        }
    }

    pub fn contains(&self, path: &str) -> bool {
        self.picked.contains(path)
    }

    // Whether any picked row is `path` or contains it, like a picked directory or the root.
    pub fn contains_under(&self, path: &str) -> bool {
        ancestors(path)
            .chain([path])
            .any(|row| self.picked.contains(row))
    }

    // Clears the picked rows, and the visual selection with them.
    pub fn clear(&mut self, tree: &mut FileTreeState) {
        tree.cancel_visual();
        self.picked.clear();
    }

    // Clears the picked rows when there is no tree to end a visual selection in.
    pub fn clear_picks(&mut self) {
        self.picked.clear();
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) {
        self.picked.retain(|path| keep(path));
    }

    // Before the mouse selects `row` of the tree, see `OnPicks`.
    pub fn prepare_point(&mut self, tree: &mut FileTreeState, row: Option<usize>, picks: OnPicks) {
        // no row, on empty space, is none picked
        let picked = row
            .and_then(|row| tree.rows().get(row))
            .is_some_and(|row| self.contains(&row.path));
        if picks == OnPicks::ClearUnlessPicked && !picked {
            self.clear(tree);
        }
    }

    // Starts or ends a visual selection, picking the rows from where it started to the selected
    // one and keeping those picked before, see `Picks`.
    pub fn toggle_visual(&mut self, tree: &mut FileTreeState) {
        tree.toggle_visual();
        self.synced = tree.rebuilds;
        match tree.visual_range().is_some() {
            true => self.picked.start_visual(),
            false => self.picked.finish_visual(),
        }
        self.sync_visual(tree);
    }

    // Picks the rows the visual selection now covers.
    pub fn sync_visual(&mut self, tree: &FileTreeState) {
        let Some(range) = tree.visual_range() else {
            return;
        };
        if std::mem::replace(&mut self.synced, tree.rebuilds) != tree.rebuilds {
            self.picked.keep_visual();
        }
        self.picked
            .sync_visual(tree.rows()[range].iter().map(|row| &row.path));
    }

    // The entries to act on: those picked, shown or not (in a closed directory, or left out by
    // the filter), else the selected row; in the order of the tree.
    pub fn entries<'a>(&'a self, tree: &'a FileTreeState) -> Vec<Entry<'a>> {
        if self.picked.is_empty() {
            return tree
                .selected()
                .and_then(FileTreeRow::entry)
                .into_iter()
                .collect();
        }
        let mut paths: Vec<&str> = self.iter().filter(|path| *path != ROOT_PATH).collect();
        paths.sort_unstable();
        paths
            .into_iter()
            .map(|path| match tree.tree.is_dir(path) {
                true => Entry::Dir(path),
                false => Entry::File(path),
            })
            .collect()
    }
}

// The files of a tree checked, like those to stash, by their index in the paths the tree was built
// from, see `RowKind::File`: a path git lists as `nested/` is the row `nested`. A directory, and the
// root row, is checked when every file under it the filter shows is, and toggles them all.
#[derive(Debug, Default)]
pub struct CheckedFiles {
    checked: Picks<usize>,
    // every file, and whether the filter shows it, see `show`
    files: Vec<(String, bool)>,
    // the files shown under each directory and the root, and how many of them are checked
    counts: FxHashMap<String, (usize, usize)>,
}

impl CheckedFiles {
    // `files`, each checked or not, all shown; in the order the tree was built from.
    pub fn new(files: impl IntoIterator<Item = (String, bool)>) -> Self {
        let mut this = Self::default();
        for (index, (path, checked)) in files.into_iter().enumerate() {
            if checked {
                this.checked.insert(&index);
            }
            this.files.push((path, true));
        }
        this.count();
        this
    }

    pub fn len(&self) -> usize {
        self.checked.len()
    }

    pub fn total(&self) -> usize {
        self.files.len()
    }

    pub fn contains(&self, index: usize) -> bool {
        self.checked.contains(&index)
    }

    // Only the files `keep` is true for are shown, like the filter of the tree.
    pub fn show(&mut self, keep: impl Fn(&str) -> bool) {
        for (path, shown) in &mut self.files {
            *shown = keep(path);
        }
        self.count();
    }

    // A file checked, or a directory with every file under it shown checked.
    pub fn row_checked(&self, row: &FileTreeRow) -> bool {
        match row.kind {
            RowKind::File(index) => self.contains(index),
            RowKind::Root | RowKind::Dir => self
                .counts
                .get(&*row.path)
                .is_some_and(|&(checked, total)| checked == total),
        }
    }

    // Checks the file of `row`, or every file under it shown; unchecks them when all are.
    pub fn toggle(&mut self, row: &FileTreeRow) {
        let check = !self.row_checked(row);
        let under: Vec<usize> = match row.kind {
            RowKind::File(index) => vec![index],
            RowKind::Root | RowKind::Dir => (0..self.files.len())
                .filter(|&i| self.files[i].1 && row.contains(tree_path(&self.files[i].0)))
                .collect(),
        };
        let changed: FxHashSet<usize> = under
            .into_iter()
            .filter(|&index| check != self.contains(index))
            .collect();
        if check {
            for index in &changed {
                self.checked.insert(index);
            }
        } else {
            // at once, as removing one at a time goes through all of them each time
            self.checked.retain(|index| !changed.contains(index));
        }
        for &index in &changed {
            for dir in ancestors(tree_path(&self.files[index].0)) {
                if let Some(count) = self.counts.get_mut(dir) {
                    count.0 = if check { count.0 + 1 } else { count.0 - 1 };
                }
            }
        }
    }

    // Counts the files shown, and those checked, under each directory.
    fn count(&mut self) {
        self.counts.clear();
        for (index, (path, shown)) in self.files.iter().enumerate() {
            if !shown {
                continue;
            }
            let checked = self.checked.contains(&index);
            for dir in ancestors(tree_path(path)) {
                // a directory's name allocated once, when first counted
                let count = match self.counts.get_mut(dir) {
                    Some(count) => count,
                    None => self.counts.entry(dir.to_string()).or_default(),
                };
                count.0 += usize::from(checked);
                count.1 += 1;
            }
        }
    }
}

// The indent markers before each row of a tree, given as its depth and whether it shows a marker
// of its own, like nvim-tree's: a line down the entries of each directory, "└" at its last one,
// two columns a level. A row's own marker, like a directory's fold marker, goes after its guide;
// the guide of the other rows, like files, ends with their line.
pub fn tree_guides(rows: impl IntoIterator<Item = (usize, bool)>) -> Vec<Guide> {
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
    let mut continues: u128 = 0;
    rows.iter()
        .zip(last)
        .map(|(&(depth, own_marker), last)| {
            let guide = Guide {
                depth,
                continues,
                own_marker,
                last,
            };
            if depth < 128 {
                // the levels below are no longer this one's
                continues &= (1u128 << depth) - 1;
                if !last {
                    continues |= 1 << depth;
                }
            }
            guide
        })
        .collect()
}

// The indent markers before a row, see `tree_guides`: kept small for the many rows of a tree,
// drawn only for those shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Guide {
    depth: usize,
    // the levels above whose directories have entries after it, one bit each; shortcut: a
    // directory 128 levels deep or more draws no line, which no real tree reaches
    continues: u128,
    own_marker: bool,
    last: bool,
}

impl Guide {
    pub fn text(&self) -> String {
        // the top level has no line
        let mut guide: String = (0..self.depth)
            .map(
                |level| match level > 0 && level < 128 && self.continues >> level & 1 == 1 {
                    true => "│ ",
                    false => "  ",
                },
            )
            .collect();
        if !self.own_marker {
            guide.push_str(match (self.depth, self.last) {
                (0, _) => "  ",
                (_, true) => "└ ",
                _ => "│ ",
            });
        }
        guide
    }
}

// The path of the next row in `rows` for which `wanted` is true, from `from` in `direction`,
// wrapping around. The root row is never one.
pub fn find_next(
    rows: &[FileTreeRow],
    from: usize,
    direction: SearchDirection,
    wanted: impl Fn(&FileTreeRow) -> bool,
) -> Option<String> {
    find_around(rows.len(), from, direction, |i| {
        !rows[i].is_root() && wanted(&rows[i])
    })
    .map(|i| rows[i].path.to_string())
}

// The directories containing `path`, outermost first, starting with the root; borrowed, as asked
// for every file of a tree.
pub fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    let dirs = path.match_indices('/').map(|(i, _)| &path[..i]);
    std::iter::once(ROOT_PATH).chain(dirs)
}

// `rows` under a root row, when there are any; only the root row when it is closed.
fn under_root(mut rows: Vec<FileTreeRow>, open: bool) -> Vec<FileTreeRow> {
    if rows.is_empty() {
        return rows;
    }
    if !open {
        return vec![FileTreeRow::root()];
    }
    for row in &mut rows {
        row.depth += 1;
    }
    rows.insert(0, FileTreeRow::root());
    rows
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

// How the selected row of a tree is drawn: not at all, quieter while the tree doesn't have the
// focus, or bright while it has; picked too while the rows are being selected, see `render_tree`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeCursor {
    Hidden,
    Dim,
    Bright,
}

// Draws the rows of `tree` in `area`, like every file tree: the directories open or closed, the
// files with their change letter from `symbol` (by their index) when they have one, what the
// filter matched highlighted; the `picked` rows and, unless the `cursor` is hidden, the selected one
// highlighted across the row, the lines of the tree keeping their color; `empty` when there is
// no row. Returns the first row shown, for the scrollbar.
#[expect(clippy::too_many_arguments)]
pub fn render_tree(
    buf: &mut Buffer,
    area: Rect,
    target: Target,
    tree: &FileTreeState,
    symbol: impl Fn(usize) -> Option<&'static str>,
    picked: impl Fn(&str) -> bool,
    cursor: TreeCursor,
    empty: &str,
    checks: Option<&CheckedFiles>,
    ctx: &AppContext,
) -> usize {
    let theme = &ctx.color_theme;
    // the filter highlights what it matched, with the options it was set with
    let (search, options) = match &tree.filter {
        Some((filter, options)) => (Some(filter.as_str()), *options),
        None => (None, ctx.search_options.get()),
    };
    let rows = tree.rows();
    if rows.is_empty() {
        Paragraph::new(placeholder(empty, theme)).render(area, buf);
        ctx.layout.borrow_mut().record(target, area, RowMap::None);
        return 0;
    }
    let height = area.height as usize;
    // no room for a row, like in a very short terminal
    if height == 0 {
        return 0;
    }
    let offset = tree.scroll(height, ctx.ui_config.common.scrolloff as usize);
    let shown = RowMap::Rows {
        first: offset,
        count: rows.len(),
    };
    ctx.layout.borrow_mut().record(target, area, shown);
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(index, row)| {
            let guide = tree.guide(index);
            // a checkbox after the lines of the tree, like a popup's
            let check = checks.map(|checks| {
                check_mark(
                    checks.row_checked(row),
                    Style::default().fg(theme.muted_fg),
                    theme,
                )
            });
            let Some(file) = row.file_index() else {
                let mut line =
                    dir_line(row, &guide, tree.is_open(&row.path), search, options, theme);
                if let Some(check) = check {
                    line.spans.insert(1, check);
                }
                return line;
            };
            let mut spans = vec![guide_span(&guide, theme)];
            spans.extend(check);
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
    // the selected row, unless the wheel scrolled it off the rows shown
    let shown = tree.cursor().checked_sub(offset);
    let shown = shown.filter(|row| *row < area.height as usize);
    if let Some(row) = shown.filter(|_| cursor != TreeCursor::Hidden) {
        let y = area.y + row as u16;
        let focused = cursor == TreeCursor::Bright;
        if tree.is_visual() {
            mark_picked_row(buf, Rect::new(area.x, y, area.width, 1), theme);
            if area.x > 0 {
                mark_selected_bar(buf, area.x - 1, y, focused, theme);
            }
        } else {
            mark_selected_row(buf, area, y, focused, theme);
        }
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
        keep_guide_color(buf, area, y, &tree.guide(index), theme);
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
        let kind = match file_index {
            _ if path == ROOT_PATH => RowKind::Root,
            Some(index) => RowKind::File(index),
            None => RowKind::Dir,
        };
        FileTreeRow {
            depth,
            name: name.into(),
            path: path.into(),
            kind,
        }
    }

    // Closed directories under a root row: the root starts open, showing the top directory.
    #[test]
    fn test_closed_folders_under_a_root() {
        let paths = ["src/x.rs", "a.rs"];
        let state = FileTreeState::new(
            FileTree::new(paths),
            Folders::Closed,
            Root::Shown,
            TreeView::Tree,
        );
        let rows: Vec<&str> = state.rows().iter().map(|r| r.path.as_ref()).collect();
        assert_eq!(rows, [ROOT_PATH, "src", "a.rs"]);
    }

    // The files as a list of their paths, by path, under the root row; the selection stays across
    // the toggle when its row is still shown.
    #[test]
    fn test_list_view() {
        let paths = ["b/x.rs", "a.rs", "b/c/y.rs"];
        let mut state = FileTreeState::new(
            FileTree::new(paths),
            Folders::Open,
            Root::Shown,
            TreeView::Tree,
        );
        state.select_path("b/c/y.rs");
        state.toggle_view();
        let rows: Vec<(&str, &str)> = state
            .rows()
            .iter()
            .map(|r| (r.name.as_ref(), r.path.as_ref()))
            .collect();
        assert_eq!(
            rows,
            [
                ("/", ROOT_PATH),
                ("a.rs", "a.rs"),
                ("b/c/y.rs", "b/c/y.rs"),
                ("b/x.rs", "b/x.rs")
            ]
        );
        assert_eq!(&*state.selected().unwrap().path, "b/c/y.rs");
        // the search looks in the rows shown, no directory among them
        assert!(state.all_rows().iter().all(|r| !r.is_dir()));
        state.toggle_view();
        assert_eq!(state.view(), TreeView::Tree);
        assert!(state.rows().iter().any(|r| *r.path == *"b/c"));
        assert_eq!(&*state.selected().unwrap().path, "b/c/y.rs");
    }

    // A directory and the root are checked when every file under them is, and toggle them all.
    #[test]
    fn test_checked_files() {
        let paths = ["a/x", "a/y", "z"];
        let state = FileTreeState::new(
            FileTree::new(paths),
            Folders::Open,
            Root::Shown,
            TreeView::Tree,
        );
        let row = |path: &str| {
            state
                .rows()
                .iter()
                .find(|r| *r.path == *path)
                .unwrap()
                .clone()
        };
        let mut checked = CheckedFiles::new(paths.map(|p| (p.to_string(), p == "a/x")));
        assert_eq!((checked.len(), checked.total()), (1, 3));
        assert!(!checked.row_checked(&row("a")));
        // some unchecked under it: checks them
        checked.toggle(&row("a"));
        assert!(checked.row_checked(&row("a")) && checked.contains(1));
        assert!(!checked.row_checked(&row(ROOT_PATH)));
        checked.toggle(&row("z"));
        assert!(checked.row_checked(&row(ROOT_PATH)));
        // all checked: unchecks them
        checked.toggle(&row("a"));
        assert_eq!(checked.len(), 1);
        assert!(!checked.contains(0) && !checked.row_checked(&row(ROOT_PATH)));
        checked.toggle(&row(ROOT_PATH));
        assert_eq!(checked.len(), 3);
        checked.toggle(&row(ROOT_PATH));
        assert_eq!(checked.len(), 0);
        // with a filter, of the files it shows
        checked.show(|path| path.starts_with("a/"));
        checked.toggle(&row("a"));
        assert!(checked.row_checked(&row(ROOT_PATH)) && !checked.contains(2));
        checked.show(|_| true);
        assert!(!checked.row_checked(&row(ROOT_PATH)));
        checked.toggle(&row(ROOT_PATH));
        assert_eq!(checked.len(), 3);
    }

    #[test]
    fn test_tree_guides() {
        // like nvim-tree's indent markers: a line down each directory, "└" at its last entry
        let tree = FileTree::new(["a/b/x.rs", "a/b/w.rs", "a/y.rs", "z.rs"]);
        let state = FileTreeState::new(tree, Folders::Open, Root::Shown, TreeView::Tree);
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
                "> /",
                "  > a",
                "  │ > b",
                "  │ │ │ w.rs",
                "  │ │ └ x.rs",
                "  │ └ y.rs",
                "  └ z.rs",
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
        let mut state = FileTreeState::new(
            FileTree::new(paths),
            Folders::Open,
            Root::Shown,
            TreeView::Tree,
        );
        let visible = |state: &FileTreeState| -> Vec<String> {
            state.rows().iter().map(|r| r.path.to_string()).collect()
        };
        assert_eq!(
            visible(&state),
            vec![ROOT_PATH, "src", "src/a.rs", "src/b.rs", "README.md"]
        );

        // collapsing from a file goes to its directory first, then collapses it
        state.navigate(UserEvent::NavigateDown);
        state.navigate(UserEvent::NavigateDown);
        state.collapse();
        assert_eq!(&*state.selected().unwrap().path, "src");
        state.collapse();
        assert_eq!(visible(&state), vec![ROOT_PATH, "src", "README.md"]);

        // the expanded state and selection carry over to another tree
        let other = state.with_tree(FileTree::new(["src/c.rs", "README.md"]));
        assert_eq!(visible(&other), vec![ROOT_PATH, "src", "README.md"]);
        assert_eq!(&*other.selected().unwrap().path, "src");

        state.toggle();
        assert_eq!(
            visible(&state),
            vec![ROOT_PATH, "src", "src/a.rs", "src/b.rs", "README.md"]
        );

        // closed by default
        let state = FileTreeState::new(
            FileTree::new(paths),
            Folders::Closed,
            Root::Hidden,
            TreeView::Tree,
        );
        assert_eq!(visible(&state), vec!["src", "README.md"]);
    }

    #[test]
    fn test_rooted_file_tree_state() {
        let paths = ["src/main.rs", "README.md"];
        let mut state = FileTreeState::new(
            FileTree::new(paths),
            Folders::Open,
            Root::Shown,
            TreeView::Tree,
        );
        let expected = vec![
            row(0, "/", ":/", None),
            row(1, "src", "src", None),
            row(2, "main.rs", "src/main.rs", Some(0)),
            row(1, "README.md", "README.md", Some(1)),
        ];
        assert_eq!(state.rows(), expected);
        assert!(state.rows()[0].contains("src/main.rs"));
        assert!(state.rows()[0].contains("README.md"));

        assert!(FileTreeState::new(
            FileTree::new([]),
            Folders::Open,
            Root::Shown,
            TreeView::Tree
        )
        .rows()
        .is_empty());

        // a file at the top goes to the root row, then the root row collapses everything
        state.navigate(UserEvent::GoToBottom);
        state.collapse();
        assert_eq!(&*state.selected().unwrap().path, ROOT_PATH);
        state.collapse();
        assert_eq!(state.rows(), vec![row(0, "/", ":/", None)]);

        // the selected path stays when the files change, else the position
        state.toggle();
        state.select_path("README.md");
        state.set_tree(FileTree::new(["src/main.rs", "README.md", "a.rs"]));
        assert_eq!(&*state.selected().unwrap().path, "README.md");
        state.set_tree(FileTree::new(["src/main.rs", "a.rs"]));
        assert_eq!(state.cursor(), 3);
        assert_eq!(&*state.selected().unwrap().path, "a.rs");
    }

    #[test]
    fn test_filter() {
        let paths = [
            "src/view/list.rs",
            "src/app.rs",
            "docs/list.md",
            "README.md",
        ];
        let mut state = FileTreeState::new(
            FileTree::new(paths),
            Folders::Closed,
            Root::Hidden,
            TreeView::Tree,
        );
        let visible = |state: &FileTreeState| -> Vec<String> {
            state.rows().iter().map(|r| r.path.to_string()).collect()
        };
        assert_eq!(visible(&state), vec!["docs", "src", "README.md"]);

        state.reveal(Some("docs/list.md"));
        state.reveal(Some("src/view/list.rs"));

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
        assert_eq!(&*state.selected().unwrap().path, "src/view/list.rs");

        state.set_filter(Some("nothing"), SearchOptions::default());
        assert!(visible(&state).is_empty());
        state.set_filter(None, SearchOptions::default());
        assert_eq!(visible(&state).len(), 7);
    }

    // A filter set fuzzy highlights what it matched once the search options change: it used to
    // highlight with them, matching nothing.
    #[test]
    fn test_filter_highlighted_with_its_options() {
        let fuzzy = SearchOptions {
            fuzzy: true,
            ..SearchOptions::default()
        };
        let mut tree = FileTreeState::new(
            FileTree::new(["axb.rs"]),
            Folders::Closed,
            Root::Hidden,
            TreeView::Tree,
        );
        tree.set_filter(Some("ab"), fuzzy);
        let ctx = crate::test_support::ui::test_ctx();
        ctx.search_options.set(SearchOptions::default());
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 1));
        render_tree(
            &mut buf,
            Rect::new(0, 0, 20, 1),
            Target::DetailFiles,
            &tree,
            |_| None,
            |_| false,
            TreeCursor::Hidden,
            "",
            None,
            &ctx,
        );
        let x = (0..20).find(|&x| buf[(x, 0)].symbol() == "a").unwrap();
        assert_eq!(buf[(x, 0)].bg, ctx.color_theme.list_match_bg);
    }

    #[test]
    fn test_ancestors() {
        assert_eq!(
            ancestors("a/b/c.rs").collect::<Vec<_>>(),
            [":/", "a", "a/b"]
        );
        assert_eq!(ancestors("c.rs").collect::<Vec<_>>(), [":/"]);
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
    fn draw_tree(tree: &FileTreeState, height: u16, cursor: TreeCursor, picked: &str) -> Buffer {
        let ctx = crate::test_support::ui::test_ctx();
        let mut buf = Buffer::empty(Rect::new(0, 0, 23, height));
        let area = Rect::new(1, 0, 20, height);
        let symbol = |_| Some("M");
        let is_picked = |path: &str| path == picked;
        render_tree(
            &mut buf,
            area,
            Target::DetailFiles,
            tree,
            symbol,
            is_picked,
            cursor,
            "",
            None,
            &ctx,
        );
        buf
    }

    #[test]
    fn test_tree_rows_look_like_the_other_lists() {
        let theme = ColorTheme::default();
        let mut tree = FileTreeState::new(
            FileTree::new(["a/x.rs", "a/y.rs", "z.rs"]),
            Folders::Open,
            Root::Shown,
            TreeView::Tree,
        );
        for cursor in [TreeCursor::Bright, TreeCursor::Dim] {
            tree.select_path("z.rs");
            let plain = draw_tree(&tree, 5, cursor, "");
            tree.select_path("a/x.rs");
            let selected = draw_tree(&tree, 5, cursor, "");
            // the change letter and the lines keep their colors, under the root row
            let row = Rect::new(1, 2, 20, 1);
            assert_row_bg(&selected, row, theme.list_selected_bg);
            assert_only_bg_changed(&plain, &selected, row);
            assert_guides_keep_color(&selected, row, &theme);
        }
        // a picked row: across it too
        let buf = draw_tree(&tree, 5, TreeCursor::Bright, "a/y.rs");
        let row = Rect::new(1, 3, 20, 1);
        assert_row_bg(&buf, row, theme.list_picked_bg);
        assert_guides_keep_color(&buf, row, &theme);

        // while selecting, the cursor is picked too, with its bar
        tree.select_path("a/x.rs");
        tree.toggle_visual();
        let buf = draw_tree(&tree, 5, TreeCursor::Bright, "");
        let row = Rect::new(1, 2, 20, 1);
        assert_row_bg(&buf, row, theme.list_picked_bg);
        assert_eq!(buf[(0, 2)].symbol(), "▌");
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
            Folders::Open,
            Root::Shown,
            TreeView::Tree,
        );
        for height in 0..4 {
            for index in 0..10 {
                tree.select_index(index);
                let mut buf = Buffer::empty(Rect::new(0, 0, 23, height.max(1)));
                let area = Rect::new(1, 0, 20, height);
                let offset = render_tree(
                    &mut buf,
                    area,
                    Target::DetailFiles,
                    &tree,
                    |_| None,
                    |_| false,
                    TreeCursor::Bright,
                    "",
                    None,
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
            Folders::Open,
            Root::Shown,
            TreeView::Tree,
        );
        let top = |tree: &FileTreeState| row_text(&draw_tree(tree, 4, TreeCursor::Bright, ""), 0);
        // down past the bottom: the view follows the cursor (the root row is 0)
        tree.select_index(6);
        assert!(top(&tree).contains("f2.rs"), "{}", top(&tree));
        // up within the view: the view stays
        tree.select_index(4);
        assert!(top(&tree).contains("f2.rs"), "{}", top(&tree));
        // up past the top: it follows again
        tree.select_index(1);
        assert!(top(&tree).contains("f0.rs"), "{}", top(&tree));
    }

    #[test]
    fn test_half_page_jumps() {
        let paths: Vec<String> = (0..10).map(|i| format!("f{i}.rs")).collect();
        let mut tree = FileTreeState::new(
            FileTree::new(paths.iter().map(String::as_str)),
            Folders::Open,
            Root::Shown,
            TreeView::Tree,
        );
        // drawn once: the half page counts the rows the tree is shown in
        let _ = draw_tree(&tree, 4, TreeCursor::Bright, "");
        tree.select_index(0);
        tree.navigate(UserEvent::HalfPageDown);
        assert_eq!(tree.cursor(), 2);
        tree.navigate(UserEvent::HalfPageDown);
        assert_eq!(tree.cursor(), 4);
        tree.navigate(UserEvent::HalfPageUp);
        assert_eq!(tree.cursor(), 2);
        // the last row, not past it
        for _ in 0..10 {
            tree.navigate(UserEvent::HalfPageDown);
        }
        assert_eq!(tree.cursor(), 10);
    }

    // v on y, up to src and src collapsed: what it covered stays picked, and nothing below y,
    // which the start counted as a row number moved onto.
    #[test]
    fn test_visual_selection_keeps_its_start_when_rows_change_above_it() {
        let paths = ["src/a", "src/b", "w", "x", "y", "z", "z1", "z2"];
        let mut tree = FileTreeState::new(
            FileTree::new(paths),
            Folders::Open,
            Root::Shown,
            TreeView::Tree,
        );
        let mut picked = TreeSelection::default();
        tree.select_path("y");
        picked.toggle_visual(&mut tree);
        while tree.selected().is_some_and(|r| &*r.path != "src") {
            tree.navigate(UserEvent::NavigateUp);
            picked.sync_visual(&tree);
        }
        tree.collapse();
        picked.sync_visual(&tree);
        assert_eq!(picked.paths(), ["src", "src/a", "src/b", "w", "x", "y"]);
        // it goes on from y: down past it picks z, as a move from y does
        for _ in 0..4 {
            tree.navigate(UserEvent::NavigateDown);
            picked.sync_visual(&tree);
        }
        assert!(tree.is_visual());
        assert_eq!(
            picked.paths(),
            ["src", "src/a", "src/b", "w", "x", "y", "z"]
        );
    }

    #[test]
    fn test_visual_selection_ends_when_a_filter_hides_its_start() {
        let paths: Vec<String> = (0..10).map(|i| format!("f{i}.rs")).collect();
        let mut tree = FileTreeState::new(
            FileTree::new(paths.iter().map(String::as_str)),
            Folders::Open,
            Root::Shown,
            TreeView::Tree,
        );
        tree.select_index(5);
        tree.toggle_visual();
        assert!(tree.is_visual());

        tree.set_filter(Some("f0"), SearchOptions::default());
        assert!(!tree.is_visual(), "the selection starts past the rows left");
        assert_eq!(tree.visual_range(), None);
    }

    // Under a filter every folder shows open, so h and l there change nothing: they changed which
    // were open once the filter was cleared, unseen.
    #[test]
    fn test_folders_stay_as_they_were_under_a_filter() {
        let mut tree = FileTreeState::new(
            FileTree::new(["src/a.rs", "src/b.rs", "z.rs"]),
            Folders::Open,
            Root::Shown,
            TreeView::Tree,
        );
        tree.set_filter(Some("rs"), SearchOptions::default());
        tree.select_path("src");
        tree.collapse();
        tree.toggle();
        tree.toggle();
        tree.set_filter(None, SearchOptions::default());
        assert!(tree.is_open("src"));
    }
}
