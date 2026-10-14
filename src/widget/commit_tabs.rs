use std::rc::Rc;

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
    git::{CommitHash, FileChange, Repository},
    widget::style::{mark_selected_row, panel_title},
    widget::{
        diff::{diff_lines, parse_diff, ParsedDiff},
        file_tree::{FileTree, FileTreeRow, FileTreeState},
        tree_query::{highlighted_name, path_matches},
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
                Span::raw(title)
                    .fg(theme.accent_text_fg)
                    .bg(theme.accent_fg)
                    .bold()
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
    diff_offset: usize,
    diff_height: usize,
}

impl<'a> CommitChangesState<'a> {
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
            diff_offset: 0,
            diff_height: 0,
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
                    .filter(|f| self.tree.filter().is_none_or(|q| path_matches(&f.path, q)))
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
        self.diff = Some(parse_diff(&diff));
        self.diff_offset = 0;
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
        self.diff_offset = self.diff_offset.saturating_add(n);
    }

    pub fn scroll_diff_up(&mut self, n: usize) {
        self.diff_offset = self.diff_offset.saturating_sub(n);
    }

    pub fn diff_page_height(&self) -> usize {
        self.diff_height.max(1)
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
    // loaded lazily, only once the tab is shown
    tree: Option<FileTreeState>,
    // the tree of the previously shown commit, whose expanded directories and selection are kept
    previous_tree: Option<FileTreeState>,
    preview: Option<FilePreview>,
    preview_offset: usize,
    preview_height: usize,
}

impl<'a> CommitFilesState<'a> {
    pub fn new(repository: &'a Repository, commit_hash: CommitHash) -> Self {
        Self {
            repository,
            commit_hash,
            tree: None,
            previous_tree: None,
            preview: None,
            preview_offset: 0,
            preview_height: 0,
        }
    }

    // Shows another commit's files, keeping the expanded directories and the selected path.
    pub fn set_commit(&mut self, commit_hash: CommitHash) {
        self.commit_hash = commit_hash;
        if let Some(tree) = self.tree.take() {
            self.previous_tree = Some(tree);
        }
        self.preview = None;
    }

    pub fn ensure_loaded(&mut self) {
        if self.tree.is_none() {
            let paths = self.repository.commit_tree_paths(&self.commit_hash);
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
        self.preview_offset = self.preview_offset.saturating_add(n);
    }

    pub fn scroll_preview_up(&mut self, n: usize) {
        self.preview_offset = self.preview_offset.saturating_sub(n);
    }

    pub fn preview_page_height(&self) -> usize {
        self.preview_height.max(1)
    }

    fn load_preview(&mut self) {
        self.preview_offset = 0;
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
            Ok(bytes) => FilePreview::Text(
                String::from_utf8_lossy(&bytes)
                    .lines()
                    .map(|line| line.replace('\t', "    "))
                    .collect(),
            ),
            Err(err) => FilePreview::Error(err),
        };
        self.preview = Some(preview);
    }
}

pub struct CommitChanges<'a> {
    ctx: Rc<AppContext>,
    // highlighted in the names
    search: Option<String>,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl CommitChanges<'_> {
    pub fn new(ctx: Rc<AppContext>, search: Option<&str>) -> Self {
        Self {
            ctx,
            search: search.map(String::from),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<'a> StatefulWidget for CommitChanges<'a> {
    type State = CommitChangesState<'a>;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let theme = &self.ctx.color_theme;
        let [tree_area, diff_area] = split_tree_and_side(theme, area, buf);

        if state.tree.rows().is_empty() {
            let message = if state.files.is_empty() {
                "No changes"
            } else {
                "No matches"
            };
            Paragraph::new(Line::raw(message).fg(theme.status_input_transient_fg))
                .render(tree_area, buf);
        } else {
            let lines = state
                .tree
                .rows()
                .iter()
                .map(|row| match row.file_index {
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
                })
                .collect();
            render_rows_with_cursor(lines, state.tree.cursor(), theme, tree_area, buf);
        }

        let lines = match &state.diff {
            Some(diff) => diff_lines(diff, theme),
            None => Vec::new(),
        };
        state.diff_height = diff_area.height as usize;
        state.diff_offset = render_scrolled(lines, state.diff_offset, theme, diff_area, buf);
    }
}

pub struct CommitFiles<'a> {
    ctx: Rc<AppContext>,
    // highlighted in the names
    search: Option<String>,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl CommitFiles<'_> {
    pub fn new(ctx: Rc<AppContext>, search: Option<&str>) -> Self {
        Self {
            ctx,
            search: search.map(String::from),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<'a> StatefulWidget for CommitFiles<'a> {
    type State = CommitFilesState<'a>;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let theme = &self.ctx.color_theme;
        let [tree_area, preview_area] = split_tree_and_side(theme, area, buf);

        if let Some(tree) = &state.tree {
            if tree.rows().is_empty() && tree.filter().is_some() {
                Paragraph::new(Line::raw("No matches").fg(theme.status_input_transient_fg))
                    .render(tree_area, buf);
            }
            let search = self.search.as_deref();
            let lines = tree
                .rows()
                .iter()
                .map(|row| match row.file_index {
                    None => dir_line(row, tree.is_open(&row.path), search, theme),
                    Some(_) => {
                        let mut spans = vec!["  ".repeat(row.depth).into(), "  ".into()];
                        spans.extend(highlighted_name(&row.name, search, theme));
                        Line::from(spans)
                    }
                })
                .collect();
            render_rows_with_cursor(lines, tree.cursor(), theme, tree_area, buf);
        }

        state.preview_height = preview_area.height as usize;
        let dim = |text: String| vec![Line::raw(text).fg(theme.status_input_transient_fg)];
        let lines = match &state.preview {
            Some(FilePreview::Text(lines)) => {
                // only the visible lines are built, files can be long
                let width = lines.len().to_string().len();
                let max_offset = lines.len().saturating_sub(state.preview_height);
                state.preview_offset = state.preview_offset.min(max_offset);
                let visible: Vec<Line> = lines
                    .iter()
                    .enumerate()
                    .skip(state.preview_offset)
                    .take(state.preview_height)
                    .map(|(i, line)| {
                        Line::from(vec![
                            Span::raw(format!("{:>width$} ", i + 1))
                                .fg(theme.status_input_transient_fg),
                            Span::raw(line.clone()),
                        ])
                    })
                    .collect();
                Paragraph::new(visible)
                    .fg(theme.fg)
                    .render(preview_area, buf);
                return;
            }
            Some(FilePreview::Binary) => dim("Binary file".into()),
            Some(FilePreview::Error(err)) => dim(err.clone()),
            Some(FilePreview::Directory) | None => Vec::new(),
        };
        Paragraph::new(lines).render(preview_area, buf);
    }
}

// Draws the top border and the divider, returning the tree area and the area beside it.
fn split_tree_and_side(theme: &ColorTheme, area: Rect, buf: &mut Buffer) -> [Rect; 2] {
    let block = Block::default()
        .borders(Borders::TOP)
        .style(Style::default().fg(theme.divider_fg));
    let inner = block.inner(area);
    block.render(area, buf);

    let tree_width = (inner.width / 3).clamp(20, 50).min(inner.width);
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
    let symbol = if open { "▾ " } else { "▸ " };
    let mut spans = vec!["  ".repeat(row.depth).into(), symbol.into()];
    spans.extend(highlighted_name(&row.name, search, theme));
    Line::from(spans)
}

// Renders `lines` from `offset`, clamped so the end of the lines stays at the bottom; returns the clamped offset.
fn render_scrolled(
    lines: Vec<Line>,
    offset: usize,
    theme: &ColorTheme,
    area: Rect,
    buf: &mut Buffer,
) -> usize {
    let offset = offset.min(lines.len().saturating_sub(area.height as usize));
    let lines: Vec<Line> = lines.into_iter().skip(offset).collect();
    Paragraph::new(lines).fg(theme.fg).render(area, buf);
    offset
}

// Renders `lines` scrolled so the cursor is visible, highlighting the cursor line.
fn render_rows_with_cursor(
    lines: Vec<Line>,
    cursor: usize,
    theme: &ColorTheme,
    area: Rect,
    buf: &mut Buffer,
) {
    if lines.is_empty() {
        return;
    }
    let height = area.height as usize;
    let offset = (cursor + 1).saturating_sub(height);
    let lines: Vec<Line> = lines.into_iter().skip(offset).collect();
    Paragraph::new(lines).fg(theme.fg).render(area, buf);

    if let Ok(y) = u16::try_from(cursor - offset) {
        mark_selected_row(buf, area, area.y + y, true, theme);
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
        let mut state = CommitFilesState::new(&repository, second);
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
        state.set_commit(first);
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
}
