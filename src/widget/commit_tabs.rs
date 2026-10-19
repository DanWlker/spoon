use std::{collections::BTreeSet, rc::Rc};

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph, Widget},
};

use crate::widget::tree_query::Toward;
use crate::{
    app::AppContext,
    color::ColorTheme,
    event::UserEvent,
    git::{Commit, CommitHash, FileChange, PatchDirection, Repository},
    layout::{fit_size, Edge, Panel},
    search::SearchOptions,
    widget::style::{focused_title, panel_title, placeholder, render_scrollbar, tree_panel_width},
    widget::{
        diff::{parse_diff, Step},
        diff_pane::DiffPane,
        file_text::{FileText, LineCursor},
        file_tree::{
            render_tree, Entry, FileTree, FileTreeRow, FileTreeState, TreeCursor, TreeSelection,
            TreeShape,
        },
        submodule::SubmodulePanel,
        tree_query::SearchDirection,
    },
};

// The tabs of the commit detail section, switched with the panel numbers 2 to 4 (1 is the commit list).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    #[default]
    Info,
    Changes,
    Files,
}

impl DetailTab {
    const ALL: [DetailTab; 3] = [DetailTab::Info, DetailTab::Changes, DetailTab::Files];

    // The tab after (or before) this one, around from the last to the first.
    pub fn cycle(self, toward: Toward) -> Self {
        let i = Self::ALL.iter().position(|t| *t == self);
        Self::ALL[toward.around(i, Self::ALL.len())]
    }

    pub fn from_number(n: usize) -> Option<Self> {
        Self::ALL.get(n.checked_sub(3)?).copied()
    }

    fn title(self) -> &'static str {
        match self {
            DetailTab::Info => "Info",
            DetailTab::Changes => "Changes",
            DetailTab::Files => "Files",
        }
    }
}

// Draws the tab titles over the top border of the detail section.
// The search and filter of the current tab are shown after the titles.
pub fn render_tab_titles(
    current: DetailTab,
    // false while the commit list has the focus
    focused: bool,
    indicator: Option<String>,
    theme: &ColorTheme,
    area: Rect,
    buf: &mut Buffer,
) {
    let mut spans: Vec<Span> = DetailTab::ALL
        .iter()
        .enumerate()
        .map(|(i, tab)| {
            let title = format!(" [{}] {} ", i + 3, tab.title());
            if *tab == current && focused {
                focused_title(title, theme)
            } else if *tab == current {
                Span::raw(title).fg(theme.fg)
            } else {
                Span::raw(title).fg(theme.muted_fg)
            }
        })
        .collect();
    if let Some(indicator) = indicator {
        spans.push(Span::raw(format!(" {indicator} ")).fg(theme.highlight_fg));
    }
    let area = Rect::new(area.x + 1, area.y, area.width.saturating_sub(1), 1);
    Line::from(spans).render(area, buf);
}

// Draws the title of a comparison of two commits over the top border of the detail section.
pub fn render_compare_title(
    title: &str,
    focused: bool,
    indicator: Option<String>,
    theme: &ColorTheme,
    area: Rect,
    buf: &mut Buffer,
) {
    let spans = panel_title(title, indicator, focused, theme).spans;
    let area = Rect::new(area.x + 1, area.y, area.width.saturating_sub(1), 1);
    Line::from(spans).render(area, buf);
}

// A file changed by a commit, as listed in the changes tab.
#[derive(Debug)]
struct ChangedFile {
    path: String,
    // the source of a rename, which the diff needs too
    from: Option<String>,
    symbol: &'static str,
}

#[derive(Debug)]
pub struct CommitChangesState<'a> {
    repository: &'a Repository,
    ctx: Rc<AppContext>,
    commit_hash: CommitHash,
    // compared to this commit instead of the parent
    base: Option<CommitHash>,
    files: Vec<ChangedFile>,
    // under the root row, expanded by default to show every changed file, like the local changes
    tree: FileTreeState,
    // the rows selected with v, which moving extends and esc clears
    picked: TreeSelection,
    // the diff of the selected file, loaded lazily, only while the tab is shown
    pub pane: DiffPane,
    // the selected submodule's commits, shown instead of its gitlink diff
    submodule: Option<SubmodulePanel<'a>>,
}

impl<'a> CommitChangesState<'a> {
    #[cfg(test)]
    pub fn paths(&self) -> Vec<&str> {
        self.files.iter().map(|f| f.path.as_str()).collect()
    }

    pub fn selected_row(&self) -> Option<&FileTreeRow> {
        self.tree.selected()
    }

    // Whether the files are being selected from where the visual selection started.
    pub fn is_visual(&self) -> bool {
        self.tree.is_visual()
    }

    // Whether files are selected, while the visual selection or after it ended.
    pub fn has_selection(&self) -> bool {
        !self.picked.is_empty()
    }

    pub fn picked_paths(&self) -> Vec<String> {
        self.picked.paths()
    }

    // The one file a single-file view works on: the selected file, or the only changed file under
    // the picked rows. None when several files are picked.
    pub fn single_picked_file(&self) -> Option<&str> {
        if self.picked.is_empty() {
            return match self.selected_row()?.entry()? {
                Entry::File(path) => Some(path),
                Entry::Dir(_) => None,
            };
        }
        let files: Vec<&str> = self
            .files
            .iter()
            .map(|file| file.path.as_str())
            .filter(|path| self.picked.contains_under(path))
            .collect();
        match files.as_slice() {
            [path] => Some(path),
            _ => None,
        }
    }

    // The entries to act on: the picked files and directories, or the selected row.
    pub fn picked_entries(&self) -> Vec<Entry<'_>> {
        self.picked.entries(&self.tree)
    }

    // Starts or ends a visual selection, selecting the files from where it started to the selected
    // one.
    pub fn toggle_visual(&mut self) {
        self.picked.toggle_visual(&mut self.tree);
    }

    pub fn cancel_visual(&mut self) {
        self.picked.clear(&mut self.tree);
    }

    pub fn new(
        repository: &'a Repository,
        ctx: Rc<AppContext>,
        commit_hash: CommitHash,
        changes: &[FileChange],
    ) -> Self {
        Self::between(repository, ctx, None, commit_hash, changes)
    }

    // The changes from `base` to the commit; from its parent when None.
    pub fn between(
        repository: &'a Repository,
        ctx: Rc<AppContext>,
        base: Option<CommitHash>,
        commit_hash: CommitHash,
        changes: &[FileChange],
    ) -> Self {
        let files: Vec<ChangedFile> = changes
            .iter()
            .map(|change| {
                let (path, from) = match change {
                    FileChange::Add { path }
                    | FileChange::Modify { path }
                    | FileChange::Delete { path } => (path, None),
                    FileChange::Move { from, to } => (to, Some(from.clone())),
                };
                ChangedFile {
                    path: path.clone(),
                    from,
                    symbol: change.symbol(),
                }
            })
            .collect();
        let mut tree = FileTreeState::new(
            FileTree::new(files.iter().map(|f| f.path.as_str())),
            TreeShape::Rooted,
        );
        // start on the first file rather than its directory
        if let Some(path) = tree
            .rows()
            .iter()
            .find(|r| !r.is_dir())
            .map(|r| r.path.clone())
        {
            tree.select_path(&path);
        }
        Self {
            repository,
            ctx,
            commit_hash,
            base,
            files,
            tree,
            picked: TreeSelection::default(),
            pane: DiffPane::default(),
            submodule: None,
        }
    }

    // Shows the diff again, the same file around the changes as the settings say.
    pub fn reload_diff(&mut self) {
        let selection = self.pane.selection();
        self.pane.set_diff(None);
        self.submodule = None;
        self.ensure_loaded();
        self.pane.restore(selection);
    }

    pub fn ensure_loaded(&mut self) {
        if self.pane.diff().is_some() || self.submodule.is_some() {
            return;
        }
        // a submodule shows its own commits instead of the gitlink diff
        if let Some(submodule) = self.load_submodule() {
            self.submodule = Some(submodule);
            self.pane.set_diff(None);
            return;
        }
        let diff = match self.tree.selected() {
            Some(row) => {
                // a directory shows the diff of all its changed files, those passing the filter
                let files: Vec<&ChangedFile> = self
                    .files
                    .iter()
                    .filter(|f| row.contains(&f.path))
                    .filter(|f| self.tree.keeps(&f.path))
                    .collect();
                let mut paths: Vec<&str> = match self.tree.filter() {
                    Some(_) if row.is_dir() => files.iter().map(|f| f.path.as_str()).collect(),
                    _ => vec![row.path.as_str()],
                };
                paths.extend(files.iter().filter_map(|f| f.from.as_deref()));
                let context = self.ctx.diff_context.get();
                match &self.base {
                    Some(base) => self.repository.diff_between(
                        base.as_str(),
                        self.commit_hash.as_str(),
                        &paths,
                        context,
                    ),
                    None => self
                        .repository
                        .commit_diff(&self.commit_hash, &paths, context),
                }
            }
            None => String::new(),
        };
        let mut diff = parse_diff(&diff);
        diff.drop_converted(self.repository);
        self.pane.set_diff(Some(diff));
    }

    // The submodule of the selected file, between the gitlink of the base (or the parent) and the
    // commit's, when both have one. None for a directory or a plain file.
    fn load_submodule(&self) -> Option<SubmodulePanel<'a>> {
        let row = self.tree.selected()?;
        if row.is_dir() {
            return None;
        }
        let path = &row.path;
        let to = self
            .repository
            .submodule_gitlink_at(self.commit_hash.as_str(), path)?;
        let from = match &self.base {
            Some(base) => self.repository.submodule_gitlink_at(base.as_str(), path)?,
            None => {
                let parent = self
                    .repository
                    .commit(&self.commit_hash)?
                    .parent_commit_hashes
                    .first()?;
                self.repository
                    .submodule_gitlink_at(parent.as_str(), path)?
            }
        };
        Some(SubmodulePanel::between(
            self.repository,
            &self.ctx,
            path,
            &from,
            Some(&to),
            None,
        ))
    }

    // Applies a tree operation, reloading the diff when the selection changed.
    pub fn update_tree<R>(&mut self, f: impl FnOnce(&mut FileTreeState) -> R) -> R {
        let before = self.tree.selected().map(|r| r.path.clone());
        let result = f(&mut self.tree);
        self.picked.sync_visual(&self.tree);
        if self.tree.selected().map(|r| &r.path) != before.as_ref() {
            self.pane.set_diff(None);
            self.submodule = None;
            self.ensure_loaded();
        }
        result
    }

    // Whether hunks were left out of the diff of the selected file, see
    // `ParsedDiff::drop_converted`; a directory opens and closes instead.
    pub fn is_converted(&self) -> bool {
        self.tree.selected().is_some_and(|r| !r.is_dir())
            && self.pane.diff().is_some_and(|d| d.converted)
    }

    // The submodule panel's graph images, drawn beside the file tree, see `CommitListState`.
    pub fn prepare_submodule_uploads(&mut self) {
        if let Some(submodule) = &mut self.submodule {
            submodule.ensure_visible_graph_uploaded();
        }
    }

    pub fn drain_submodule_uploads(&mut self) -> Vec<String> {
        self.submodule
            .as_mut()
            .map_or_else(Vec::new, SubmodulePanel::drain_pending_graph_uploads)
    }

    // The height the submodule's commits are drawn in, set before their graph images are prepared.
    pub fn set_submodule_height(&mut self, height: usize) {
        if let Some(submodule) = &mut self.submodule {
            submodule.set_list_height(height);
        }
    }
}

// The contents of the file selected in the files tab.
#[derive(Debug)]
enum FilePreview {
    // the lines of a file, in `CommitFilesState::text`, the files keeping the focus
    Text,
    // the same, the selection moving in its lines
    TextFocused,
    Binary,
    Directory,
    Error(String),
}

#[derive(Debug)]
pub struct CommitFilesState<'a> {
    repository: &'a Repository,
    commit_hash: CommitHash,
    // the first parent of the commit and the changes from it, to work out the files of the
    // commits next to it without listing them all again
    first_parent: Option<CommitHash>,
    changes: Vec<FileChange>,
    // the files of the commit once listed, or worked out
    paths: Option<BTreeSet<String>>,
    // loaded lazily, only once the tab is shown
    tree: Option<FileTreeState>,
    // the rows selected with v, which moving extends and esc clears
    picked: TreeSelection,
    // the tree of the previously shown commit, whose expanded directories and selection are kept
    previous_tree: Option<FileTreeState>,
    preview: Option<FilePreview>,
    // the lines of a file with text, see `FilePreview::Text`
    text: FileText,
}

impl<'a> CommitFilesState<'a> {
    pub fn selected_row(&self) -> Option<&FileTreeRow> {
        self.tree.as_ref()?.selected()
    }

    // Whether the files are being selected from where the visual selection started.
    pub fn is_tree_visual(&self) -> bool {
        self.tree.as_ref().is_some_and(FileTreeState::is_visual)
    }

    // Whether files are selected, while the visual selection or after it ended.
    pub fn has_tree_selection(&self) -> bool {
        !self.picked.is_empty()
    }

    pub fn picked_paths(&self) -> Vec<String> {
        self.picked.paths()
    }

    // The one file a single-file view works on: the selected file, or the only file under the
    // picked rows. None when several files are picked.
    pub fn single_picked_file(&self) -> Option<&str> {
        if self.picked.is_empty() {
            return match self.selected_row()?.entry()? {
                Entry::File(path) => Some(path),
                Entry::Dir(_) => None,
            };
        }
        let files: Vec<&str> = self
            .paths
            .as_ref()?
            .iter()
            .map(String::as_str)
            .filter(|path| self.picked.contains_under(path))
            .collect();
        match files.as_slice() {
            [path] => Some(path),
            _ => None,
        }
    }

    // The entries to act on: the picked files and directories, or the selected row.
    pub fn picked_entries(&self) -> Vec<Entry<'_>> {
        self.tree
            .as_ref()
            .map_or_else(Vec::new, |tree| self.picked.entries(tree))
    }

    // Starts or ends a visual selection, selecting the files from where it started to the selected
    // one.
    pub fn toggle_tree_visual(&mut self) {
        let Self { tree, picked, .. } = self;
        if let Some(tree) = tree {
            picked.toggle_visual(tree);
        }
    }

    pub fn cancel_tree_visual(&mut self) {
        let Self { tree, picked, .. } = self;
        match tree {
            Some(tree) => picked.clear(tree),
            None => picked.clear_picks(),
        }
    }

    pub fn new(repository: &'a Repository, commit: &Commit, changes: &[FileChange]) -> Self {
        Self {
            repository,
            commit_hash: commit.commit_hash.clone(),
            first_parent: commit.parent_commit_hashes.first().cloned(),
            changes: changes.to_vec(),
            paths: None,
            tree: None,
            picked: TreeSelection::default(),
            previous_tree: None,
            preview: None,
            text: FileText::default(),
        }
    }

    // Shows another commit's files, keeping the expanded directories and the selected path.
    // Stepping to the parent or a child of the commit works out its files from the changes
    // between them, keeping the tree when no file was added, deleted or moved.
    pub fn set_commit(&mut self, commit: &Commit, changes: &[FileChange]) {
        let hash = &commit.commit_hash;
        let first_parent = commit.parent_commit_hashes.first();
        let step = if first_parent == Some(&self.commit_hash) {
            Some((changes, PatchDirection::Forward))
        } else if self.first_parent.as_ref() == Some(hash) {
            Some((&self.changes[..], PatchDirection::Reverse))
        } else {
            None
        };
        let same_paths = match (self.paths.take(), step) {
            (Some(mut paths), Some((changes, direction))) => {
                let changed = apply_changes(&mut paths, changes, direction);
                self.paths = Some(paths);
                !changed
            }
            _ => false,
        };

        self.commit_hash = hash.clone();
        self.first_parent = first_parent.cloned();
        self.changes = changes.to_vec();
        if let Some(tree) = self.tree.as_mut() {
            tree.cancel_visual();
        }
        self.picked.clear_picks();
        if !same_paths {
            if let Some(tree) = self.tree.take() {
                self.previous_tree = Some(tree);
            }
        }
        self.preview = None;
    }

    pub fn ensure_loaded(&mut self) {
        if self.tree.is_none() {
            let repository = self.repository;
            let commit_hash = &self.commit_hash;
            let paths = self.paths.get_or_insert_with(|| {
                repository
                    .commit_tree_paths(commit_hash)
                    .into_iter()
                    .collect()
            });
            let tree = FileTree::new(paths.iter().map(String::as_str));
            self.tree = Some(match self.previous_tree.take() {
                Some(previous) => previous.with_tree(tree),
                // collapsed by default, a whole repository is too much to show at once
                None => FileTreeState::new(tree, TreeShape::Closed),
            });
            self.preview = None;
        }
        if self.preview.is_none() {
            self.load_preview();
        }
    }

    // Applies a tree operation, reloading the preview when the selection changed; None before the
    // tree is loaded.
    pub fn update_tree<R>(&mut self, f: impl FnOnce(&mut FileTreeState) -> R) -> Option<R> {
        let (result, selected_changed) = {
            let tree = self.tree.as_mut()?;
            let before = tree.selected().map(|r| r.path.clone());
            let result = f(tree);
            let changed = tree.selected().map(|r| &r.path) != before.as_ref();
            (result, changed)
        };
        if let Some(tree) = self.tree.as_ref() {
            self.picked.sync_visual(tree);
        }
        if selected_changed {
            self.load_preview();
        }
        Some(result)
    }

    // Scrolls the preview by the scroll keys, or moves the selected line half a page; returns
    // whether `event` was one.
    pub fn scroll_preview(&mut self, event: UserEvent) -> bool {
        self.is_preview_focused() && self.text.scroll(event)
    }

    pub fn is_preview_focused(&self) -> bool {
        matches!(self.preview, Some(FilePreview::TextFocused))
    }

    // Whether the lines of the preview are being selected from where the visual selection started.
    pub fn is_visual(&self) -> bool {
        self.text.is_visual()
    }

    // Whether lines of the preview are selected, while the visual selection or after it ended.
    pub fn has_selection(&self) -> bool {
        self.text.has_selection()
    }

    // Starts or ends a visual selection of the preview's lines, while the preview has the focus.
    pub fn toggle_visual(&mut self) {
        if self.is_preview_focused() {
            self.text.toggle_visual();
        }
    }

    pub fn cancel_visual(&mut self) {
        self.text.cancel_visual();
    }

    // Whether the preview can have the focus, which only a file with text can.
    fn has_text(&self) -> bool {
        matches!(
            self.preview,
            Some(FilePreview::Text | FilePreview::TextFocused)
        ) && !self.text.lines().is_empty()
    }

    // Moves the focus into the preview of a file, on the first line shown; false when it has no text.
    pub fn enter(&mut self) -> bool {
        if !self.has_text() {
            return false;
        }
        if !self.is_preview_focused() {
            self.preview = Some(FilePreview::TextFocused);
            self.text.select_shown();
        }
        true
    }

    // Moves the focus back to the files; false when it is there.
    pub fn back(&mut self) -> bool {
        let focused = self.is_preview_focused();
        if focused {
            self.preview = Some(FilePreview::Text);
        }
        focused
    }

    pub fn step(&mut self, step: Step) {
        if self.has_text() {
            self.text.step(step);
        }
    }

    // The selected line of the preview, to copy.
    pub fn selected_text(&self) -> Option<String> {
        self.text
            .selected_text()
            .filter(|_| self.is_preview_focused())
    }

    // The number of the selected line of the preview, from 1.
    pub fn selected_line(&self) -> Option<usize> {
        self.selected_text().map(|_| self.text.selected_line() + 1)
    }

    // Selects the line of the preview matching `query`, around from the last match; false when
    // nothing matches.
    pub fn search_preview(
        &mut self,
        query: &str,
        direction: SearchDirection,
        options: SearchOptions,
    ) -> bool {
        if !self.has_text() || !self.text.search(query, direction, options) {
            return false;
        }
        self.preview = Some(FilePreview::TextFocused);
        true
    }

    fn load_preview(&mut self) {
        self.text.set_lines(Vec::new());
        let Some(row) = self.tree.as_ref().and_then(|t| t.selected()) else {
            self.preview = Some(FilePreview::Directory);
            return;
        };
        if row.is_dir() {
            self.preview = Some(FilePreview::Directory);
            return;
        }
        let preview = match self
            .repository
            .commit_file_content(&self.commit_hash, &row.path)
        {
            // the same heuristic as git: a NUL byte near the start means binary
            Ok(bytes) if bytes.iter().take(8000).any(|b| *b == 0) => FilePreview::Binary,
            // as in the file, to copy; tabs are expanded when shown
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                self.text
                    .set_lines(text.lines().map(String::from).collect());
                FilePreview::Text
            }
            Err(err) => FilePreview::Error(err),
        };
        self.preview = Some(preview);
    }
}

// Applies the changes of a commit to the files of its parent (forward), or takes them back from
// the files of the commit (in reverse); returns whether a file was added, deleted or moved.
fn apply_changes(
    paths: &mut BTreeSet<String>,
    changes: &[FileChange],
    direction: PatchDirection,
) -> bool {
    let mut changed = false;
    for change in changes {
        let (added, removed) = match change {
            FileChange::Modify { .. } => continue,
            FileChange::Add { path } => (Some(path), None),
            FileChange::Delete { path } => (None, Some(path)),
            FileChange::Move { from, to } => (Some(to), Some(from)),
        };
        let (added, removed) = match direction {
            PatchDirection::Forward => (added, removed),
            PatchDirection::Reverse => (removed, added),
        };
        if let Some(path) = removed {
            paths.remove(path);
        }
        if let Some(path) = added {
            paths.insert(path.clone());
        }
        changed = true;
    }
    changed
}

// The changes or the files tab of the details: a file tree beside the diff or the preview.
pub struct CommitTab {
    ctx: Rc<AppContext>,
    // highlighted in the names
    search: Option<String>,
    // highlighted in the diff or the preview
    pane_search: Option<String>,
    // the panel has the focus, its border the accent's color
    focused: bool,
}

impl CommitTab {
    pub fn new(
        ctx: Rc<AppContext>,
        search: Option<&str>,
        pane_search: Option<&str>,
        focused: bool,
    ) -> Self {
        Self {
            ctx,
            search: search.map(String::from),
            pane_search: pane_search.map(String::from),
            focused,
        }
    }

    pub fn render_changes(self, area: Rect, buf: &mut Buffer, state: &mut CommitChangesState) {
        let theme = &self.ctx.color_theme;
        let [tree_area, diff_area] = split_tree_and_side(&self.ctx, area, buf);

        let empty = if state.files.is_empty() {
            "No changes"
        } else {
            "No matches"
        };
        let tree_focused = !state.pane.is_focused();
        let cursor = match (state.tree.is_visual(), tree_focused) {
            (true, true) => TreeCursor::PickedBright,
            (true, false) => TreeCursor::PickedDim,
            (false, true) => TreeCursor::Bright,
            (false, false) => TreeCursor::Dim,
        };
        let offset = render_tree(
            buf,
            tree_area,
            &state.tree,
            |i| Some(state.files[i].symbol),
            self.search.as_deref(),
            |path| state.picked.contains(path),
            cursor,
            empty,
            &self.ctx,
        );
        render_tree_scrollbar(buf, tree_area, &state.tree, offset, tree_focused, theme);

        // a submodule shows its own commits, with the graph, instead of the gitlink diff
        if let Some(submodule) = state.submodule.as_mut() {
            let scrollbar = Rect::new(diff_area.right(), diff_area.y, 1, diff_area.height);
            submodule.render_bare(&self.ctx, diff_area, scrollbar, buf, self.focused);
            return;
        }

        let search = self.pane_search.as_deref();
        let scrollbar = Rect::new(diff_area.right(), diff_area.y, 1, diff_area.height);
        state
            .pane
            .render(diff_area, scrollbar, buf, &self.ctx, self.focused, search);
    }

    pub fn render_files(self, area: Rect, buf: &mut Buffer, state: &mut CommitFilesState) {
        let theme = &self.ctx.color_theme;
        let [tree_area, preview_area] = split_tree_and_side(&self.ctx, area, buf);

        if let Some(tree) = &state.tree {
            // a commit always has files, they can only be filtered out
            let focused = !state.is_preview_focused();
            let cursor = match (tree.is_visual(), focused) {
                (true, true) => TreeCursor::PickedBright,
                (true, false) => TreeCursor::PickedDim,
                (false, true) => TreeCursor::Bright,
                (false, false) => TreeCursor::Dim,
            };
            let offset = render_tree(
                buf,
                tree_area,
                tree,
                |_| None,
                self.search.as_deref(),
                |path| state.picked.contains(path),
                cursor,
                "No matches",
                &self.ctx,
            );
            render_tree_scrollbar(buf, tree_area, tree, offset, focused, theme);
        }

        let dim = |text: String| vec![placeholder(text, theme)];
        let lines = match &state.preview {
            Some(preview @ (FilePreview::Text | FilePreview::TextFocused)) => {
                let search = self.pane_search.as_deref();
                let ctx = &self.ctx;
                let cursor = match preview {
                    FilePreview::TextFocused => LineCursor::Shown,
                    _ => LineCursor::Hidden,
                };
                state
                    .text
                    .render(preview_area, buf, ctx, self.focused, cursor, search);
                return;
            }
            Some(FilePreview::Binary) => dim("Binary file".into()),
            Some(FilePreview::Error(err)) => dim(err.clone()),
            Some(FilePreview::Directory) | None => Vec::new(),
        };
        Paragraph::new(lines).render(preview_area, buf);
    }
}

// Where the tree and the side beside it are drawn, before either has the room of its border.
struct TreeSideAreas {
    tree: Rect,
    tree_inner: Rect,
    side: Rect,
}

fn tree_and_side_areas(ctx: &AppContext, inner: Rect) -> TreeSideAreas {
    let default = tree_panel_width(inner.width);
    let tree_width = ctx.layout.borrow().size(Panel::DetailTree, default);
    // both keep some room
    let tree_width = fit_size(tree_width, inner.width, 10, 10);
    let [tree_area, side_area] =
        Layout::horizontal([Constraint::Length(tree_width), Constraint::Min(0)]).areas(inner);
    let tree_inner = Block::default()
        .borders(Borders::RIGHT)
        .padding(Padding::horizontal(1))
        .inner(tree_area);
    let side = Rect {
        x: side_area.x + 1,
        width: side_area.width.saturating_sub(1),
        ..side_area
    };
    TreeSideAreas {
        tree: tree_area,
        tree_inner,
        side,
    }
}

// The area beside the tree, for the page to size the submodule panel before its graph images are
// prepared.
pub fn changes_side_area(ctx: &AppContext, inner: Rect) -> Rect {
    tree_and_side_areas(ctx, inner).side
}

// Draws the divider, returning the tree area and the area beside it.
fn split_tree_and_side(ctx: &AppContext, inner: Rect, buf: &mut Buffer) -> [Rect; 2] {
    let theme = &ctx.color_theme;
    let areas = tree_and_side_areas(ctx, inner);
    ctx.layout
        .borrow_mut()
        .place(Panel::DetailTree, areas.tree, Edge::Right);
    let tree_block = Block::default()
        .borders(Borders::RIGHT)
        .style(Style::default().fg(theme.divider_fg))
        .padding(Padding::horizontal(1));
    tree_block.render(areas.tree, buf);
    [areas.tree_inner, areas.side]
}

// The scrollbar of a tree beside a diff or a preview, on the divider past its padding.
fn render_tree_scrollbar(
    buf: &mut Buffer,
    area: Rect,
    tree: &FileTreeState,
    offset: usize,
    focused: bool,
    theme: &ColorTheme,
) {
    let divider = Rect::new(area.right() + 1, area.y, 1, area.height);
    // the accent's while the tree has the focus, like the other panels' scrollbars
    render_scrollbar(buf, divider, tree.rows().len(), offset, focused, theme);
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{
        git::SortCommit,
        test_support::git::{git, GitRepository},
        test_support::ui::test_ctx,
        widget::file_tree::ROOT_PATH,
    };

    // A superproject with `sub` added, and a commit bumping its gitlink from the first commit to
    // the second inside it; the two gitlinks are returned.
    fn repository_with_a_bumped_submodule(
        path: &std::path::Path,
    ) -> (Repository, std::path::PathBuf, String, String) {
        let base = path.join("base");
        fs::create_dir(&base).unwrap();
        let base_repo = GitRepository::new(&base);
        base_repo.init();
        fs::write(base.join("a.txt"), "one\n").unwrap();
        base_repo.add(&["a.txt"]);
        base_repo.commit("first", "2024-01-01");
        let first = base_repo.rev_parse_head();

        let superproject = path.join("super");
        fs::create_dir(&superproject).unwrap();
        let repo = GitRepository::new(&superproject);
        repo.init();
        git(
            &superproject,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                base.to_str().unwrap(),
                "sub",
            ],
        );
        repo.commit("add sub", "2024-01-02");

        let sub = superproject.join("sub");
        let sub_repo = GitRepository::new(&sub);
        fs::write(sub.join("a.txt"), "two\n").unwrap();
        sub_repo.add(&["a.txt"]);
        sub_repo.commit("second", "2024-01-03");
        let second = sub_repo.rev_parse_head();

        repo.add(&["sub"]);
        repo.commit("bump the submodule", "2024-01-04");
        let repository =
            Repository::load(&superproject, SortCommit::Chronological, None, false).unwrap();
        (repository, superproject, first, second)
    }

    #[test]
    fn test_a_bumped_submodule_is_found_after_it_leaves_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let (_, superproject, _first, _second) = repository_with_a_bumped_submodule(dir.path());
        // the submodule is gone from HEAD and the index, but its checkout is still there
        git(&superproject, &["rm", "--cached", "-q", "sub"]);
        let repo = GitRepository::new(&superproject);
        repo.commit("remove the submodule", "2024-01-05");

        let repository =
            Repository::load(&superproject, SortCommit::Chronological, None, false).unwrap();
        let bump = repository
            .all_commits()
            .iter()
            .find(|commit| commit.subject == "bump the submodule")
            .unwrap()
            .commit_hash
            .clone();
        let (_, changes) = repository.commit_detail(&bump);
        let mut state = CommitChangesState::new(&repository, test_ctx(), bump, &changes);
        state.ensure_loaded();
        let submodule = state.submodule.as_ref().expect("the submodule panel");
        assert_eq!(submodule.subjects(), ["second"]);
    }

    #[test]
    fn test_changes_of_a_bumped_submodule_list_its_commits() {
        let dir = tempfile::tempdir().unwrap();
        let (repository, _superproject, _first, _second) =
            repository_with_a_bumped_submodule(dir.path());
        let head = repository.head_commit().unwrap();
        let (_, changes) = repository.commit_detail(&head);
        let mut state = CommitChangesState::new(&repository, test_ctx(), head.clone(), &changes);
        state.ensure_loaded();
        let submodule = state.submodule.as_ref().expect("the submodule panel");
        assert_eq!(submodule.subjects(), ["second"]);
        // the gitlink diff isn't shown, its commits are
        assert!(state.pane.diff().is_none());
    }

    fn visible(tree: &FileTreeState) -> Vec<&str> {
        tree.rows().iter().map(|r| r.path.as_str()).collect()
    }

    fn selected(tree: &FileTreeState) -> &str {
        &tree.selected().unwrap().path
    }

    #[test]
    fn test_current_tab_titled_like_the_other_panels() {
        let theme = ColorTheme::default();
        for focused in [true, false] {
            let area = Rect::new(0, 0, 60, 1);
            let mut buf = Buffer::empty(area);
            render_tab_titles(DetailTab::Changes, focused, None, &theme, area, &mut buf);
            let row: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
            let x = row.find("[4] Changes").unwrap() as u16;
            // the same title drawn like the other panels'
            let mut expected = Buffer::empty(area);
            panel_title("[4] Changes", [], focused, &theme).render(area, &mut expected);
            assert_eq!(buf[(x, 0)], expected[(1, 0)], "focused: {focused}");
        }
    }

    #[test]
    fn test_commit_tabs() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        fs::create_dir(repo_path.join("src"))?;
        fs::write(repo_path.join("src/a.rs"), "a\n")?;
        fs::write(
            repo_path.join("README"),
            "readme\nwith enough lines\nto detect\na rename\n",
        )?;
        fs::write(repo_path.join("image.bin"), b"\x00\x01binary")?;
        git.add(&["src/a.rs", "README", "image.bin"]);
        git.commit("first", "2024-01-01");
        fs::write(repo_path.join("src/a.rs"), "a\nchanged\n")?;
        crate::git::git_command()
            .args(["mv", "README", "DOC.md"])
            .current_dir(repo_path)
            .output()?;
        git.add(&["src/a.rs"]);
        git.commit("second", "2024-01-02");

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let commits = repository.all_commits();
        let (second, first) = (
            commits[0].commit_hash.clone(),
            commits[1].commit_hash.clone(),
        );

        // changes: under the root row like the local changes, expanded, starts on the first file,
        // a rename shows its source too
        let (_, changes) = repository.commit_detail(&second);
        let mut state = CommitChangesState::new(&repository, test_ctx(), second.clone(), &changes);
        state.ensure_loaded();
        assert_eq!(
            visible(&state.tree),
            vec![ROOT_PATH, "src", "src/a.rs", "DOC.md"]
        );
        assert_eq!(selected(&state.tree), "src/a.rs");
        assert!(state.pane.diff().unwrap().raw.contains("+changed"));
        state.update_tree(|tree| {
            tree.navigate(UserEvent::GoToBottom);
        });
        assert!(state
            .pane
            .diff()
            .unwrap()
            .raw
            .contains("rename from README"));

        // the root row shows the diff of the whole commit
        state.update_tree(|tree| {
            tree.navigate(UserEvent::GoToTop);
        });
        let raw = &state.pane.diff().unwrap().raw;
        assert!(raw.contains("+changed") && raw.contains("rename from README"));

        // changes: v selects the files between the ends, keeping them after it stops
        state.update_tree(|tree| tree.select_path("src/a.rs"));
        state.toggle_visual();
        state.update_tree(|tree| tree.navigate(UserEvent::NavigateDown));
        assert_eq!(
            selected(&state.tree),
            "DOC.md",
            "{:?}",
            visible(&state.tree)
        );
        assert!(
            state.picked.contains("src/a.rs") && state.picked.contains("DOC.md"),
            "{:?}",
            state.picked
        );
        state.toggle_visual();
        assert!(!state.is_visual() && state.has_selection());
        state.cancel_visual();
        assert!(!state.has_selection());

        // changes: folders collapse, and the diff follows the selection
        state.update_tree(|tree| tree.select_path("src"));
        state.update_tree(FileTreeState::toggle);
        assert_eq!(visible(&state.tree), vec![ROOT_PATH, "src", "DOC.md"]);
        let raw = &state.pane.diff().unwrap().raw;
        assert!(raw.contains("+changed") && !raw.contains("rename from README"));

        // the root commit is compared to the empty tree
        let (_, changes) = repository.commit_detail(&first);
        let mut state = CommitChangesState::new(&repository, test_ctx(), first.clone(), &changes);
        state.ensure_loaded();
        assert!(state.pane.diff().unwrap().raw.contains("+a"));

        // files: collapsed by default, with a preview of the selected file
        let (second_commit, second_changes) = repository.commit_detail(&second);
        let mut state = CommitFilesState::new(&repository, &second_commit, &second_changes);
        state.ensure_loaded();
        let tree = state.tree.as_ref().unwrap();
        assert_eq!(visible(tree), vec!["src", "DOC.md", "image.bin"]);
        assert!(matches!(state.preview, Some(FilePreview::Directory)));
        state.update_tree(FileTreeState::toggle);
        state.update_tree(|tree| {
            tree.navigate(UserEvent::NavigateDown);
        });
        assert_eq!(selected(state.tree.as_ref().unwrap()), "src/a.rs");
        assert!(
            matches!(
                &state.preview,
                Some(FilePreview::Text | FilePreview::TextFocused)
            ) && state.text.lines() == ["a", "changed"]
        );

        // files: v selects the rows too
        state.toggle_tree_visual();
        state.update_tree(|tree| tree.navigate(UserEvent::NavigateDown));
        assert!(state.has_tree_selection());
        state.toggle_tree_visual();
        assert!(!state.is_tree_visual() && state.has_tree_selection());
        state.cancel_tree_visual();
        assert!(!state.has_tree_selection());

        state.update_tree(|tree| {
            tree.navigate(UserEvent::GoToBottom);
        });
        assert!(matches!(state.preview, Some(FilePreview::Binary)));

        // another commit keeps the expanded directories and the selection
        state.update_tree(|tree| {
            tree.navigate(UserEvent::GoToTop);
        });
        state.update_tree(|tree| {
            tree.navigate(UserEvent::NavigateDown);
        });
        let (first_commit, first_changes) = repository.commit_detail(&first);
        state.set_commit(&first_commit, &first_changes);
        state.ensure_loaded();
        let tree = state.tree.as_ref().unwrap();
        assert_eq!(
            visible(tree),
            vec!["src", "src/a.rs", "README", "image.bin"]
        );
        assert_eq!(selected(tree), "src/a.rs");
        assert!(matches!(
            &state.preview,
            Some(FilePreview::Text | FilePreview::TextFocused)
        ));
        assert_eq!(state.text.lines(), ["a"]);

        Ok(())
    }

    #[test]
    fn test_files_follow_commits() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path();
        let git = GitRepository::new(path);
        git.init();
        let text = "some\nlines\nenough\nto rename\n";
        fs::write(path.join("a"), text)?;
        fs::write(path.join("b"), "b\n")?;
        git.add(&["a", "b"]);
        git.commit("add a and b", "2024-01-01");
        fs::write(path.join("a"), format!("{text}more\n"))?;
        git.add(&["a"]);
        git.commit("modify a", "2024-01-02");
        fs::create_dir(path.join("dir"))?;
        fs::write(path.join("dir/c"), "c\n")?;
        fs::remove_file(path.join("b"))?;
        git.add(&["dir/c", "b"]);
        git.commit("add dir/c, delete b", "2024-01-03");
        crate::git::git_command()
            .args(["mv", "a", "x"])
            .current_dir(path)
            .output()?;
        git.commit("rename a to x", "2024-01-04");

        let repository = Repository::load(path, SortCommit::Chronological, None, false)?;
        let hashes: Vec<CommitHash> = repository
            .all_commits()
            .iter()
            .map(|c| c.commit_hash.clone())
            .collect();
        let files = |state: &CommitFilesState| -> Vec<String> {
            state.paths.as_ref().unwrap().iter().cloned().collect()
        };
        let listed = |hash: &CommitHash| -> Vec<String> {
            let mut paths = repository.commit_tree_paths(hash);
            paths.sort();
            paths
        };

        let (commit, changes) = repository.commit_detail(&hashes[0]);
        let mut state = CommitFilesState::new(&repository, &commit, &changes);
        state.ensure_loaded();
        // down to the first commit, then back up, without listing the files again
        let order = (1..hashes.len()).chain((0..hashes.len() - 1).rev());
        for i in order {
            let (commit, changes) = repository.commit_detail(&hashes[i]);
            state.set_commit(&commit, &changes);
            state.ensure_loaded();
            assert_eq!(files(&state), listed(&hashes[i]), "at commit {i}");
            let tree = state.tree.as_ref().unwrap();
            assert!(tree
                .rows()
                .iter()
                .all(|r| r.is_dir() || listed(&hashes[i]).contains(&r.path)));
        }
        Ok(())
    }

    #[test]
    fn test_diff_focus() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        let lines: Vec<String> = (1..=20).map(|n| format!("line {n}\n")).collect();
        fs::write(repo_path.join("f"), lines.concat())?;
        git.add(&["f"]);
        git.commit("first", "2024-01-01");
        let mut changed = lines.clone();
        changed[1] = "two\n".into();
        changed[17] = "eighteen\n".into();
        fs::write(repo_path.join("f"), changed.concat())?;
        git.add(&["f"]);
        git.commit("second", "2024-01-02");

        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let second = repository.all_commits()[0].commit_hash.clone();
        let (_, changes) = repository.commit_detail(&second);
        let mut state = CommitChangesState::new(&repository, test_ctx(), second, &changes);
        state.ensure_loaded();

        // enter goes into the hunks, then the lines of the selected one; back goes out
        assert!(!state.pane.is_focused());
        assert!(state.pane.enter());
        assert_eq!(
            state.pane.selected_text().unwrap().lines().nth(2),
            Some("-line 2")
        );
        state.pane.step(Step::Next);
        assert!(state.pane.selected_text().unwrap().contains("+eighteen"));
        state.pane.enter();
        assert!(state.pane.is_selecting_lines());
        assert_eq!(state.pane.selected_text().as_deref(), Some("line 18"));
        state.pane.step(Step::Next);
        assert_eq!(state.pane.selected_text().as_deref(), Some("eighteen"));
        assert!(state.pane.back() && state.pane.back() && !state.pane.back());

        // searching selects the line matched, around from the last match
        assert!(!state
            .pane
            .search("TWO", SearchDirection::Current, SearchOptions::default()));
        assert!(state
            .pane
            .search("two", SearchDirection::Current, SearchOptions::default()));
        assert_eq!(state.pane.selected_text().as_deref(), Some("two"));
        assert!(state
            .pane
            .search("line 1", SearchDirection::Next, SearchOptions::default()));
        assert!(state.pane.search(
            "line 1",
            SearchDirection::Previous,
            SearchOptions::default()
        ));

        // applying the added line to a working tree going back to the first commit
        fs::write(repo_path.join("f"), lines.concat())?;
        state.pane.search(
            "eighteen",
            SearchDirection::Current,
            SearchOptions::default(),
        );
        let patch = state
            .pane
            .selected_patch(crate::git::PatchDirection::Forward)
            .unwrap();
        let task = crate::git::GitTask {
            commands: vec![crate::git::GitCommand::Apply.args(["-"])],
            input: Some(patch),
            ..Default::default()
        };
        crate::git::run_task(repo_path, &task)?;
        let mut expected = lines.clone();
        expected.insert(18, "eighteen\n".into());
        assert_eq!(fs::read_to_string(repo_path.join("f"))?, expected.concat());
        Ok(())
    }

    #[test]
    fn test_preview_focus() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo_path = dir.path();
        let git = GitRepository::new(repo_path);
        git.init();
        fs::write(repo_path.join("f"), "one\n\ttwo\nthree\n")?;
        git.add(&["f"]);
        git.commit("first", "2024-01-01");
        let repository = Repository::load(repo_path, SortCommit::Chronological, None, false)?;
        let commit = repository.all_commits()[0].clone();
        let (_, changes) = repository.commit_detail(&commit.commit_hash);
        let mut state = CommitFilesState::new(&repository, &commit, &changes);
        state.ensure_loaded();

        // enter focuses the preview on its first line, lines are copied as in the file
        assert!(state.enter() && state.is_preview_focused());
        assert_eq!(state.selected_text().as_deref(), Some("one"));
        state.step(Step::Next);
        assert_eq!(state.selected_text().as_deref(), Some("\ttwo"));
        state.step(Step::Last);
        state.step(Step::Next);
        assert_eq!(state.selected_text().as_deref(), Some("three"));

        // searching goes around from the last match
        assert!(state.search_preview("o", SearchDirection::Next, SearchOptions::default()));
        assert_eq!(state.selected_text().as_deref(), Some("one"));
        assert!(state.search_preview("o", SearchDirection::Next, SearchOptions::default()));
        assert_eq!(state.selected_text().as_deref(), Some("\ttwo"));
        assert!(!state.search_preview("zzz", SearchDirection::Next, SearchOptions::default()));
        assert!(state.back() && !state.back());
        assert_eq!(state.selected_text(), None);
        Ok(())
    }

    #[test]
    fn test_changes_tree_looks_like_the_local_changes() -> Result<(), Box<dyn std::error::Error>> {
        use crate::widget::local_changes::LocalChangesState;
        let dir = tempfile::tempdir()?;
        let path = dir.path();
        let git = GitRepository::new(path);
        git.init();
        git.commit("first", "2024-01-01");
        fs::create_dir(path.join("src"))?;
        fs::write(path.join("src/a.rs"), "a\n")?;
        fs::write(path.join("b"), "b\n")?;
        git.add(&["src/a.rs", "b"]);

        // the same files, staged then committed, give the same rows
        let names = |rows: &[crate::widget::file_tree::FileTreeRow]| -> Vec<String> {
            rows.iter()
                .map(|r| format!("{}{}", r.depth, r.name))
                .collect()
        };
        let repository = Repository::load(path, SortCommit::Chronological, None, false)?;
        let mut local = LocalChangesState::load(&repository);
        local.focus_section(crate::widget::local_changes::LocalChangesSection::Staged);
        let local_rows = names(local.rows());
        git.commit("second", "2024-01-02");
        let repository = Repository::load(path, SortCommit::Chronological, None, false)?;
        let head = repository.head_commit().unwrap();
        let (_, changes) = repository.commit_detail(&head);
        let state = CommitChangesState::new(&repository, test_ctx(), head, &changes);
        assert_eq!(names(state.tree.rows()), local_rows);
        Ok(())
    }
}
