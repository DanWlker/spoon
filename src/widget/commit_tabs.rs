use std::{collections::BTreeSet, rc::Rc};

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph, StatefulWidget, Widget},
};

use crate::{
    app::AppContext,
    color::ColorTheme,
    git::{Commit, CommitHash, FileChange, Repository},
    layout::{Border, Panel},
    widget::style::{
        focused_title, fold_marker, mark_selected_row, panel_title, render_scrollbar,
        tree_panel_width,
    },
    widget::{
        diff::{
            diff_line_count, diff_lines, parse_diff, render_diff_lines, render_visual_rows,
            visual_rows, DiffCursor, DiffHunk, ParsedDiff, Step,
        },
        file_tree::{keeps, FileTree, FileTreeRow, FileTreeState, SearchDirection},
        tree_query::{find_around, highlight_match, highlighted_name, match_range},
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
    pub fn cycle(self, forward: bool) -> Self {
        let len = Self::ALL.len();
        let i = Self::ALL.iter().position(|t| *t == self).unwrap_or(0);
        let next = if forward {
            (i + 1) % len
        } else {
            (i + len - 1) % len
        };
        Self::ALL[next]
    }

    pub fn from_number(n: usize) -> Option<Self> {
        Self::ALL.get(n.checked_sub(2)?).copied()
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
            let title = format!(" [{}] {} ", i + 2, tab.title());
            if *tab == current && focused {
                focused_title(title, theme)
            } else if *tab == current {
                Span::raw(title).fg(theme.fg).bold()
            } else {
                Span::raw(title).fg(theme.status_input_transient_fg)
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
    let indicator = indicator.unwrap_or_default();
    let spans = panel_title(title, &indicator, focused, theme).spans;
    let area = Rect::new(area.x + 1, area.y, area.width.saturating_sub(1), 1);
    Line::from(spans).render(area, buf);
}

// What has the focus in the changes tab: the files, or the hunks or lines of the diff beside them.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum ChangesFocus {
    #[default]
    Files,
    Hunks,
    Lines,
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
    commit_hash: CommitHash,
    // compared to this commit instead of the parent
    base: Option<CommitHash>,
    files: Vec<ChangedFile>,
    // expanded by default, to show every changed file
    tree: FileTreeState,
    // loaded lazily, only while the tab is shown
    diff: Option<ParsedDiff>,
    focus: ChangesFocus,
    cursor: DiffCursor,
}

impl<'a> CommitChangesState<'a> {
    // The path of the selected file or directory, relative to the top of the repository.
    pub fn selected_path(&self) -> Option<&str> {
        self.tree.selected().map(|r| r.path.as_str())
    }

    // The selected path, and whether it is a directory.
    pub fn selected_entry(&self) -> Option<(&str, bool)> {
        let row = self.tree.selected()?;
        Some((row.path.as_str(), row.file_index.is_none()))
    }

    pub fn new(
        repository: &'a Repository,
        commit_hash: CommitHash,
        changes: &[FileChange],
    ) -> Self {
        Self::between(repository, None, commit_hash, changes)
    }

    // The changes from `base` to the commit; from its parent when None.
    pub fn between(
        repository: &'a Repository,
        base: Option<CommitHash>,
        commit_hash: CommitHash,
        changes: &[FileChange],
    ) -> Self {
        let files: Vec<ChangedFile> = changes
            .iter()
            .map(|change| {
                let (path, from, symbol) = match change {
                    FileChange::Add { path } => (path, None, "A"),
                    FileChange::Modify { path } => (path, None, "M"),
                    FileChange::Delete { path } => (path, None, "D"),
                    FileChange::Move { from, to } => (to, Some(from.clone()), "R"),
                };
                ChangedFile {
                    path: path.clone(),
                    from,
                    symbol,
                }
            })
            .collect();
        let mut tree =
            FileTreeState::new(FileTree::new(files.iter().map(|f| f.path.as_str())), true);
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
            commit_hash,
            base,
            files,
            tree,
            diff: None,
            focus: ChangesFocus::default(),
            cursor: DiffCursor::default(),
        }
    }

    pub fn ensure_loaded(&mut self) {
        if self.diff.is_some() {
            return;
        }
        let diff = match self.tree.selected() {
            Some(row) => {
                // a directory shows the diff of all its changed files, those passing the filter
                let files: Vec<&ChangedFile> = self
                    .files
                    .iter()
                    .filter(|f| row.contains(&f.path))
                    .filter(|f| keeps(self.tree.filter(), &f.path))
                    .collect();
                let mut paths: Vec<&str> = match self.tree.filter() {
                    Some(_) if row.is_dir() => files.iter().map(|f| f.path.as_str()).collect(),
                    _ => vec![row.path.as_str()],
                };
                paths.extend(files.iter().filter_map(|f| f.from.as_deref()));
                match &self.base {
                    Some(base) => self.repository.diff_between(
                        base.as_str(),
                        self.commit_hash.as_str(),
                        &paths,
                    ),
                    None => self.repository.commit_diff(&self.commit_hash, &paths),
                }
            }
            None => String::new(),
        };
        let mut diff = parse_diff(&diff);
        diff.drop_converted(self.repository);
        self.diff = Some(diff);
        self.focus = ChangesFocus::Files;
        self.cursor.reset();
    }

    // Applies a tree operation, reloading the diff when the selection changed.
    pub fn update_tree(&mut self, f: impl FnOnce(&mut FileTreeState)) {
        let before = self.tree.selected().map(|r| r.path.clone());
        f(&mut self.tree);
        if self.tree.selected().map(|r| &r.path) != before.as_ref() {
            self.diff = None;
            self.ensure_loaded();
        }
    }

    pub fn scroll_diff_down(&mut self, n: usize) {
        self.cursor.scroll_down(n);
    }

    pub fn scroll_diff_up(&mut self, n: usize) {
        self.cursor.scroll_up(n);
    }

    pub fn diff_page_height(&self) -> usize {
        self.cursor.page_height()
    }

    // Whether hunks were left out of the diff of the selected file, see
    // `ParsedDiff::drop_converted`; a directory opens and closes instead.
    pub fn is_converted(&self) -> bool {
        self.tree.selected().is_some_and(|r| !r.is_dir())
            && self.diff.as_ref().is_some_and(|d| d.converted)
    }

    pub fn is_diff_focused(&self) -> bool {
        self.focus != ChangesFocus::Files
    }

    pub fn is_selecting_lines(&self) -> bool {
        self.focus == ChangesFocus::Lines
    }

    // The lines of the selected hunk or line in the file at the commit, (first, last); None for
    // the files, or lines only removed.
    pub fn selected_lines(&self) -> Option<(usize, usize)> {
        let diff = self.diff.as_ref().filter(|_| self.is_diff_focused())?;
        let rows = self.cursor.highlighted(diff, self.is_selecting_lines());
        let mut lines = diff.rows.get(rows)?.iter().filter_map(|row| row.new);
        let first = lines.next()?;
        Some((first, lines.next_back().unwrap_or(first)))
    }

    // Goes one level deeper, like on the local changes page: files -> hunks -> lines of the
    // selected hunk. Returns false when the diff has no hunk.
    pub fn enter(&mut self) -> bool {
        let Some(diff) = &self.diff else {
            return false;
        };
        match self.focus {
            ChangesFocus::Files if self.cursor.select_hunk(diff, 0) => {
                self.focus = ChangesFocus::Hunks
            }
            ChangesFocus::Hunks if self.cursor.enter_lines(diff) => {
                self.focus = ChangesFocus::Lines
            }
            ChangesFocus::Files => return false,
            _ => {}
        }
        true
    }

    // Goes one level back: lines -> hunks -> files. Returns false when already at the files.
    pub fn back(&mut self) -> bool {
        match self.focus {
            ChangesFocus::Files => return false,
            ChangesFocus::Hunks => self.focus = ChangesFocus::Files,
            ChangesFocus::Lines => {
                self.focus = ChangesFocus::Hunks;
                // the cursor may have moved into another hunk
                if let Some(diff) = &self.diff {
                    self.cursor.select_hunk(diff, self.cursor.hunk);
                }
            }
        }
        true
    }

    // Moves the selected hunk or line in the diff.
    pub fn step(&mut self, step: Step) {
        let Some(diff) = &self.diff else {
            return;
        };
        match self.focus {
            ChangesFocus::Files => {}
            ChangesFocus::Hunks => self.cursor.move_hunk(diff, step),
            ChangesFocus::Lines => self.cursor.move_line(diff, step),
        }
    }

    // The patch of the selected hunk or line, to apply to the working tree.
    pub fn selected_patch(&self) -> Option<String> {
        let diff = self.diff.as_ref()?;
        match self.focus {
            ChangesFocus::Files => None,
            ChangesFocus::Hunks => self.cursor.selected_hunk(diff).map(DiffHunk::patch),
            ChangesFocus::Lines => self.cursor.line_patch(diff, false),
        }
    }

    // The selected line as in the file, or the selected hunk as a diff, to copy.
    pub fn selected_text(&self) -> Option<String> {
        let diff = self.diff.as_ref()?;
        match self.focus {
            ChangesFocus::Files => None,
            ChangesFocus::Hunks => self.cursor.selected_hunk(diff).map(DiffHunk::text),
            ChangesFocus::Lines => self.cursor.line_text(diff),
        }
    }

    // Selects a row of the diff matching `query`, see `DiffCursor::search`. Returns false when
    // nothing matches.
    pub fn search_diff(&mut self, query: &str, direction: SearchDirection) -> bool {
        let Some(diff) = &self.diff else {
            return false;
        };
        let lines = self.focus == ChangesFocus::Lines;
        match self.cursor.search(diff, query, direction, lines) {
            Some(changed) => {
                self.focus = if changed {
                    ChangesFocus::Lines
                } else {
                    ChangesFocus::Hunks
                };
                true
            }
            None => false,
        }
    }
}

// The contents of the file selected in the files tab.
#[derive(Debug)]
enum FilePreview {
    Text(Vec<String>),
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
    // the tree of the previously shown commit, whose expanded directories and selection are kept
    previous_tree: Option<FileTreeState>,
    preview: Option<FilePreview>,
    // the selected line of the preview while it has the focus, and its scroll
    preview_focused: bool,
    cursor: DiffCursor,
    // the line the search last matched, where the next one starts from
    search_row: Option<usize>,
}

impl<'a> CommitFilesState<'a> {
    // The path of the selected file or directory, relative to the top of the repository.
    pub fn selected_path(&self) -> Option<&str> {
        self.tree.as_ref()?.selected().map(|r| r.path.as_str())
    }

    // The selected path, and whether it is a directory.
    pub fn selected_entry(&self) -> Option<(&str, bool)> {
        let row = self.tree.as_ref()?.selected()?;
        Some((row.path.as_str(), row.file_index.is_none()))
    }

    pub fn new(repository: &'a Repository, commit: &Commit, changes: &[FileChange]) -> Self {
        Self {
            repository,
            commit_hash: commit.commit_hash.clone(),
            first_parent: commit.parent_commit_hashes.first().cloned(),
            changes: changes.to_vec(),
            paths: None,
            tree: None,
            previous_tree: None,
            preview: None,
            preview_focused: false,
            cursor: DiffCursor::default(),
            search_row: None,
        }
    }

    // Shows another commit's files, keeping the expanded directories and the selected path.
    // Stepping to the parent or a child of the commit works out its files from the changes
    // between them, keeping the tree when no file was added, deleted or moved.
    pub fn set_commit(&mut self, commit: &Commit, changes: &[FileChange]) {
        let hash = &commit.commit_hash;
        let first_parent = commit.parent_commit_hashes.first();
        let step = if first_parent == Some(&self.commit_hash) {
            Some((changes, true))
        } else if self.first_parent.as_ref() == Some(hash) {
            Some((&self.changes[..], false))
        } else {
            None
        };
        let same_paths = match (self.paths.take(), step) {
            (Some(mut paths), Some((changes, forward))) => {
                let changed = apply_changes(&mut paths, changes, forward);
                self.paths = Some(paths);
                !changed
            }
            _ => false,
        };

        self.commit_hash = hash.clone();
        self.first_parent = first_parent.cloned();
        self.changes = changes.to_vec();
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
                None => FileTreeState::new(tree, false),
            });
            self.preview = None;
        }
        if self.preview.is_none() {
            self.load_preview();
        }
    }

    // Applies a tree operation, reloading the preview when the selection changed.
    pub fn update_tree(&mut self, f: impl FnOnce(&mut FileTreeState)) {
        let Some(tree) = self.tree.as_mut() else {
            return;
        };
        let before = tree.selected().map(|r| r.path.clone());
        f(tree);
        if tree.selected().map(|r| &r.path) != before.as_ref() {
            self.load_preview();
        }
    }

    pub fn scroll_preview_down(&mut self, n: usize) {
        self.cursor.scroll_down(n);
    }

    pub fn scroll_preview_up(&mut self, n: usize) {
        self.cursor.scroll_up(n);
    }

    pub fn preview_page_height(&self) -> usize {
        self.cursor.page_height()
    }

    pub fn is_preview_focused(&self) -> bool {
        self.preview_focused
    }

    fn text_lines(&self) -> Option<&[String]> {
        match &self.preview {
            Some(FilePreview::Text(lines)) if !lines.is_empty() => Some(lines),
            _ => None,
        }
    }

    // Moves the focus into the preview of a file, on the first line shown; false when it has no text.
    pub fn enter(&mut self) -> bool {
        if self.preview_focused || self.text_lines().is_none() {
            return self.preview_focused;
        }
        self.preview_focused = true;
        self.cursor.line = self.cursor.offset;
        true
    }

    // Moves the focus back to the files; false when it is there.
    pub fn back(&mut self) -> bool {
        std::mem::take(&mut self.preview_focused)
    }

    pub fn step(&mut self, step: Step) {
        let Some(last) = self.text_lines().map(|l| l.len() - 1) else {
            return;
        };
        let line = match step {
            Step::Next => self.cursor.line + 1,
            Step::Prev => self.cursor.line.saturating_sub(1),
            Step::First => 0,
            Step::Last => last,
        };
        self.select_line(line.min(last));
        self.search_row = None;
    }

    fn select_line(&mut self, line: usize) {
        self.cursor.line = line;
        self.cursor.scroll_to(line..line + 1);
    }

    // The selected line of the preview, to copy.
    pub fn selected_text(&self) -> Option<String> {
        let lines = self.text_lines().filter(|_| self.preview_focused)?;
        lines.get(self.cursor.line).cloned()
    }

    // The number of the selected line of the preview, from 1.
    pub fn selected_line(&self) -> Option<usize> {
        self.selected_text().map(|_| self.cursor.line + 1)
    }

    // Selects the line of the preview matching `query`, around from the last match; false when
    // nothing matches.
    pub fn search_preview(&mut self, query: &str, direction: SearchDirection) -> bool {
        let Some(lines) = self.text_lines() else {
            return false;
        };
        let start = self.search_row.unwrap_or(self.cursor.line);
        let found = find_around(lines.len(), start, direction, |i| {
            match_range(&lines[i], query).is_some()
        });
        let Some(line) = found else {
            return false;
        };
        self.preview_focused = true;
        self.select_line(line);
        self.search_row = Some(line);
        true
    }

    fn load_preview(&mut self) {
        self.preview_focused = false;
        self.cursor.reset();
        self.search_row = None;
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
            Ok(bytes) => FilePreview::Text(
                String::from_utf8_lossy(&bytes)
                    .lines()
                    .map(String::from)
                    .collect(),
            ),
            Err(err) => FilePreview::Error(err),
        };
        self.preview = Some(preview);
    }
}

// Applies the changes of a commit to the files of its parent (`forward`), or takes them back from
// the files of the commit; returns whether a file was added, deleted or moved.
fn apply_changes(paths: &mut BTreeSet<String>, changes: &[FileChange], forward: bool) -> bool {
    let mut changed = false;
    for change in changes {
        let (added, removed) = match change {
            FileChange::Modify { .. } => continue,
            FileChange::Add { path } => (Some(path), None),
            FileChange::Delete { path } => (None, Some(path)),
            FileChange::Move { from, to } => (Some(to), Some(from)),
        };
        let (added, removed) = if forward {
            (added, removed)
        } else {
            (removed, added)
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

pub struct CommitChanges<'a> {
    ctx: Rc<AppContext>,
    // highlighted in the names
    search: Option<String>,
    // highlighted in the diff
    diff_search: Option<String>,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl CommitChanges<'_> {
    pub fn new(ctx: Rc<AppContext>, search: Option<&str>, diff_search: Option<&str>) -> Self {
        Self {
            ctx,
            search: search.map(String::from),
            diff_search: diff_search.map(String::from),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<'a> StatefulWidget for CommitChanges<'a> {
    type State = CommitChangesState<'a>;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let theme = &self.ctx.color_theme;
        let [tree_area, diff_area] = split_tree_and_side(&self.ctx, area, buf);

        if state.tree.rows().is_empty() {
            let message = if state.files.is_empty() {
                "No changes"
            } else {
                "No matches"
            };
            Paragraph::new(Line::raw(message).fg(theme.status_input_transient_fg))
                .render(tree_area, buf);
        } else {
            let line = |row: &FileTreeRow| match row.file_index {
                None => dir_line(
                    row,
                    state.tree.is_open(&row.path),
                    self.search.as_deref(),
                    theme,
                ),
                Some(i) => {
                    let symbol = state.files[i].symbol;
                    let mut spans = vec![
                        "  ".repeat(row.depth).into(),
                        Span::raw(symbol).fg(symbol_color(symbol, theme)),
                        " ".into(),
                    ];
                    spans.extend(highlighted_name(&row.name, self.search.as_deref(), theme));
                    Line::from(spans)
                }
            };
            let (rows, cursor) = (state.tree.rows(), state.tree.cursor());
            let tree_focused = !state.is_diff_focused();
            render_rows_with_cursor(rows, line, cursor, tree_focused, theme, tree_area, buf);
        }

        let count = state.diff.as_ref().map_or(0, diff_line_count);
        let range = state.cursor.fit(count, diff_area.height as usize);
        let Some(diff) = &state.diff else {
            return;
        };
        let border = Rect::new(diff_area.right(), diff_area.y, 1, diff_area.height);
        render_scrollbar(buf, border, count, range.start, theme);
        let mut lines = diff_lines(diff, theme, range.clone());
        let highlighted = match state.focus {
            ChangesFocus::Files => 0..0,
            focus => state.cursor.highlighted(diff, focus == ChangesFocus::Lines),
        };
        if let Some(query) = &self.diff_search {
            for line in lines.iter_mut() {
                highlight_match(line, query, theme);
            }
        }
        let wrap = self.ctx.wrap.get();
        render_diff_lines(lines, range, highlighted, wrap, theme, diff_area, buf);
    }
}

pub struct CommitFiles<'a> {
    ctx: Rc<AppContext>,
    // highlighted in the names
    search: Option<String>,
    // highlighted in the preview
    preview_search: Option<String>,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl CommitFiles<'_> {
    pub fn new(ctx: Rc<AppContext>, search: Option<&str>, preview_search: Option<&str>) -> Self {
        Self {
            ctx,
            search: search.map(String::from),
            preview_search: preview_search.map(String::from),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<'a> StatefulWidget for CommitFiles<'a> {
    type State = CommitFilesState<'a>;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let theme = &self.ctx.color_theme;
        let [tree_area, preview_area] = split_tree_and_side(&self.ctx, area, buf);

        if let Some(tree) = &state.tree {
            if tree.rows().is_empty() && tree.filter().is_some() {
                Paragraph::new(Line::raw("No matches").fg(theme.status_input_transient_fg))
                    .render(tree_area, buf);
            }
            let search = self.search.as_deref();
            let line = |row: &FileTreeRow| match row.file_index {
                None => dir_line(row, tree.is_open(&row.path), search, theme),
                Some(_) => {
                    let mut spans = vec!["  ".repeat(row.depth).into(), "  ".into()];
                    spans.extend(highlighted_name(&row.name, search, theme));
                    Line::from(spans)
                }
            };
            let focused = !state.preview_focused;
            render_rows_with_cursor(
                tree.rows(),
                line,
                tree.cursor(),
                focused,
                theme,
                tree_area,
                buf,
            );
        }

        let dim = |text: String| vec![Line::raw(text).fg(theme.status_input_transient_fg)];
        let lines = match &state.preview {
            Some(FilePreview::Text(lines)) => {
                // only the visible lines are built, files can be long
                let width = lines.len().to_string().len();
                let range = state.cursor.fit(lines.len(), preview_area.height as usize);
                let border =
                    Rect::new(preview_area.right(), preview_area.y, 1, preview_area.height);
                render_scrollbar(buf, border, lines.len(), range.start, theme);
                let highlight_style = Style::default().bg(theme.list_selected_bg);
                let selected = if state.preview_focused {
                    state.cursor.line..state.cursor.line + 1
                } else {
                    0..0
                };
                let visible =
                    lines[range.start.min(lines.len())..]
                        .iter()
                        .zip(range)
                        .map(|(line, i)| {
                            let mut line = Line::from(vec![
                                Span::raw(format!("{:>width$} ", i + 1))
                                    .fg(theme.status_input_transient_fg),
                                Span::raw(line.replace('\t', "    ")),
                            ]);
                            if let Some(query) = &self.preview_search {
                                highlight_match(&mut line, query, theme);
                            }
                            if selected.contains(&i) {
                                line.style = line.style.patch(highlight_style);
                            }
                            (i, line)
                        });
                let visual = visual_rows(visible, self.ctx.wrap.get(), &selected, preview_area);
                render_visual_rows(visual, &selected, theme, preview_area, buf);
                return;
            }
            Some(FilePreview::Binary) => dim("Binary file".into()),
            Some(FilePreview::Error(err)) => dim(err.clone()),
            Some(FilePreview::Directory) | None => Vec::new(),
        };
        Paragraph::new(lines).render(preview_area, buf);
    }
}

// Draws the divider, returning the tree area and the area beside it.
fn split_tree_and_side(ctx: &AppContext, inner: Rect, buf: &mut Buffer) -> [Rect; 2] {
    let theme = &ctx.color_theme;
    let default = tree_panel_width(inner.width);
    let tree_width = ctx.layout.borrow().size(Panel::DetailTree, default);
    // both keep some room
    let tree_width = tree_width
        .clamp(10, inner.width.saturating_sub(10).max(10))
        .min(inner.width);
    if tree_width > 0 {
        let border = Border {
            panel: Panel::DetailTree,
            vertical: true,
            at: inner.left() + tree_width - 1,
            from: inner.top(),
            to: inner.bottom(),
            anchor: inner.left(),
            forward: true,
        };
        ctx.layout.borrow_mut().place(border, tree_width);
    }
    let [tree_area, side_area] =
        Layout::horizontal([Constraint::Length(tree_width), Constraint::Min(0)]).areas(inner);
    let tree_block = Block::default()
        .borders(Borders::RIGHT)
        .style(Style::default().fg(theme.divider_fg))
        .padding(Padding::horizontal(1));
    let tree_inner = tree_block.inner(tree_area);
    tree_block.render(tree_area, buf);

    let side_area = Rect {
        x: side_area.x + 1,
        width: side_area.width.saturating_sub(1),
        ..side_area
    };
    [tree_inner, side_area]
}

fn dir_line(
    row: &FileTreeRow,
    open: bool,
    search: Option<&str>,
    theme: &ColorTheme,
) -> Line<'static> {
    let mut spans = vec!["  ".repeat(row.depth).into(), fold_marker(open).into()];
    spans.extend(highlighted_name(&row.name, search, theme));
    Line::from(spans)
}

// Renders the `rows` on screen as `line` makes them, scrolled so the cursor is visible,
// highlighting the cursor line.
fn render_rows_with_cursor<'a>(
    rows: &'a [FileTreeRow],
    line: impl Fn(&'a FileTreeRow) -> Line<'a>,
    cursor: usize,
    focused: bool,
    theme: &ColorTheme,
    area: Rect,
    buf: &mut Buffer,
) {
    if rows.is_empty() {
        return;
    }
    let height = area.height as usize;
    let offset = (cursor + 1).saturating_sub(height);
    // on the divider beside the tree, past its padding
    let divider = Rect::new(area.right() + 1, area.y, 1, area.height);
    render_scrollbar(buf, divider, rows.len(), offset, theme);
    let lines: Vec<Line> = rows.iter().skip(offset).take(height).map(line).collect();
    Paragraph::new(lines).fg(theme.fg).render(area, buf);

    if let Ok(y) = u16::try_from(cursor - offset) {
        mark_selected_row(buf, area, area.y + y, focused, theme);
    }
}

fn symbol_color(symbol: &str, theme: &ColorTheme) -> Color {
    match symbol {
        "A" => theme.detail_file_change_add_fg,
        "D" => theme.detail_file_change_delete_fg,
        "R" => theme.detail_file_change_move_fg,
        _ => theme.detail_file_change_modify_fg,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{git::SortCommit, test_support::git::GitRepository};

    fn visible(tree: &FileTreeState) -> Vec<&str> {
        tree.rows().iter().map(|r| r.path.as_str()).collect()
    }

    fn selected(tree: &FileTreeState) -> &str {
        &tree.selected().unwrap().path
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
        std::process::Command::new("git")
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

        // changes: expanded, starts on the first file, a rename shows its source too
        let (_, changes) = repository.commit_detail(&second);
        let mut state = CommitChangesState::new(&repository, second.clone(), &changes);
        state.ensure_loaded();
        assert_eq!(visible(&state.tree), vec!["src", "src/a.rs", "DOC.md"]);
        assert_eq!(selected(&state.tree), "src/a.rs");
        assert!(state.diff.as_ref().unwrap().raw.contains("+changed"));
        state.update_tree(FileTreeState::select_last);
        assert!(state
            .diff
            .as_ref()
            .unwrap()
            .raw
            .contains("rename from README"));

        // changes: folders collapse, and the diff follows the selection
        state.update_tree(FileTreeState::select_first);
        state.update_tree(FileTreeState::toggle);
        assert_eq!(visible(&state.tree), vec!["src", "DOC.md"]);
        assert!(state.diff.as_ref().unwrap().raw.contains("+changed"));

        // the root commit is compared to the empty tree
        let (_, changes) = repository.commit_detail(&first);
        let mut state = CommitChangesState::new(&repository, first.clone(), &changes);
        state.ensure_loaded();
        assert!(state.diff.as_ref().unwrap().raw.contains("+a"));

        // files: collapsed by default, with a preview of the selected file
        let (second_commit, second_changes) = repository.commit_detail(&second);
        let mut state = CommitFilesState::new(&repository, &second_commit, &second_changes);
        state.ensure_loaded();
        let tree = state.tree.as_ref().unwrap();
        assert_eq!(visible(tree), vec!["src", "DOC.md", "image.bin"]);
        assert!(matches!(state.preview, Some(FilePreview::Directory)));
        state.update_tree(FileTreeState::toggle);
        state.update_tree(FileTreeState::select_next);
        assert_eq!(selected(state.tree.as_ref().unwrap()), "src/a.rs");
        assert!(
            matches!(&state.preview, Some(FilePreview::Text(lines)) if lines == &["a", "changed"])
        );
        state.update_tree(FileTreeState::select_last);
        assert!(matches!(state.preview, Some(FilePreview::Binary)));

        // another commit keeps the expanded directories and the selection
        state.update_tree(FileTreeState::select_first);
        state.update_tree(FileTreeState::select_next);
        let (first_commit, first_changes) = repository.commit_detail(&first);
        state.set_commit(&first_commit, &first_changes);
        state.ensure_loaded();
        let tree = state.tree.as_ref().unwrap();
        assert_eq!(
            visible(tree),
            vec!["src", "src/a.rs", "README", "image.bin"]
        );
        assert_eq!(selected(tree), "src/a.rs");
        assert!(matches!(&state.preview, Some(FilePreview::Text(lines)) if lines == &["a"]));

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
        std::process::Command::new("git")
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
        let mut state = CommitChangesState::new(&repository, second, &changes);
        state.ensure_loaded();

        // enter goes into the hunks, then the lines of the selected one; back goes out
        assert!(!state.is_diff_focused());
        assert!(state.enter());
        assert_eq!(
            state.selected_text().unwrap().lines().nth(2),
            Some("-line 2")
        );
        state.step(Step::Next);
        assert!(state.selected_text().unwrap().contains("+eighteen"));
        state.enter();
        assert!(state.is_selecting_lines());
        assert_eq!(state.selected_text().as_deref(), Some("line 18"));
        state.step(Step::Next);
        assert_eq!(state.selected_text().as_deref(), Some("eighteen"));
        assert!(state.back() && state.back() && !state.back());

        // searching selects the line matched, around from the last match
        assert!(!state.search_diff("TWO", SearchDirection::Current));
        assert!(state.search_diff("two", SearchDirection::Current));
        assert_eq!(state.selected_text().as_deref(), Some("two"));
        assert!(state.search_diff("line 1", SearchDirection::Next));
        assert!(state.search_diff("line 1", SearchDirection::Previous));

        // applying the added line to a working tree going back to the first commit
        fs::write(repo_path.join("f"), lines.concat())?;
        state.search_diff("eighteen", SearchDirection::Current);
        let patch = state.selected_patch().unwrap();
        repository.apply_patch_to_worktree(&patch, false)?;
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
        assert!(state.search_preview("o", SearchDirection::Next));
        assert_eq!(state.selected_text().as_deref(), Some("one"));
        assert!(state.search_preview("o", SearchDirection::Next));
        assert_eq!(state.selected_text().as_deref(), Some("\ttwo"));
        assert!(!state.search_preview("zzz", SearchDirection::Next));
        assert!(state.back() && !state.back());
        assert_eq!(state.selected_text(), None);
        Ok(())
    }
}
