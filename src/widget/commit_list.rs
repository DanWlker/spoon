use std::{cell::Cell, rc::Rc};

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{List, ListItem, StatefulWidget, Widget},
    Frame,
};
use rustc_hash::{FxHashMap, FxHashSet};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::widget::tree_query::Toward;
use crate::{
    app::AppContext,
    color::ColorTheme,
    config::UserListColumnType,
    event::{AppEvent, UserEvent},
    git::{Commit, CommitHash, Head, Ref, Repository, Tracking},
    graph::{Fold, GraphImageManager},
    protocol::PreparedImage,
    search::{SearchOptions, SearchOutcome, SearchTarget},
    widget::{
        style::{
            mark_selected_bar, panel_block, panel_title, picked_style, render_panel_scrollbar,
            ListCursor,
        },
        tree_query::{
            find_around, highlighted_text, name_matches, QueryKind, SearchDirection, TreeQuery,
        },
    },
};

pub const ELLIPSIS: &str = "...";

// before the labels of refs (Nerd Font): a branch whose upstream is on the same commit, a remote
// branch, a tag and a stash; a plain branch has none
const UPSTREAM_ICON: &str = "\u{ec63}";
const REMOTE_BRANCH_ICON: &str = "\u{f015f}";
const TAG_ICON: &str = "\u{f04f9}";
const STASH_ICON: &str = "\u{f01c}";

#[derive(Debug)]
pub struct CommitInfo<'a> {
    commit: &'a Commit,
    refs: Vec<&'a Ref>,
    // the local branches whose upstream is at the commit too, shown in their label, see `new`
    with_upstream: Vec<&'a str>,
    // on the history of HEAD; the other commits are dimmed
    on_head: bool,
}

impl<'a> CommitInfo<'a> {
    // A branch and its upstream at the commit show as one label, the branch's, marked by
    // `UPSTREAM_ICON`; other remote branches keep their own.
    pub fn new(
        commit: &'a Commit,
        mut refs: Vec<&'a Ref>,
        tracking: &'a FxHashMap<String, Tracking>,
        on_head: bool,
    ) -> Self {
        let upstream_of = |r: &Ref| match r {
            Ref::Branch { name, .. } => tracking.get(name).map(|t| t.upstream.as_str()),
            _ => None,
        };
        let upstreams: Vec<&str> = refs
            .iter()
            .filter_map(|r| upstream_of(r))
            .filter(|u| {
                refs.iter()
                    .any(|r| matches!(r, Ref::RemoteBranch { name, .. } if name == u))
            })
            .collect();
        let with_upstream = refs
            .iter()
            .filter(|r| upstream_of(r).is_some_and(|u| upstreams.contains(&u)))
            .map(|r| r.name())
            .collect();
        refs.retain(
            |r| !matches!(r, Ref::RemoteBranch { name, .. } if upstreams.contains(&name.as_str())),
        );
        Self {
            commit,
            refs,
            with_upstream,
            on_head,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRefreshContext {
    query: String,
}

#[derive(Debug)]
pub struct CommitListState<'a> {
    commits: Vec<CommitInfo<'a>>,
    commit_hash_set: FxHashSet<&'a CommitHash>,
    graph_image_manager: GraphImageManager<'a>,
    graph_cell_width: u16,

    // the commit of each ref by its full name ("refs/heads/main", "stash@{0}", "HEAD")
    ref_name_to_commit_index_map: FxHashMap<String, usize>,

    // the search, typed like the others, see `TreeQuery`
    query: TreeQuery,
    // the app's, see `AppContext::search_options`
    search_options: Rc<Cell<SearchOptions>>,
    // the commits the search matches
    search_matches: Vec<bool>,

    // commits picked with space for commands on several commits, in the order they were picked
    picked_commit_hashes: Vec<CommitHash>,
    // what part of the history is loaded, like "main only", when not all of it
    filter_note: Option<String>,
    // the checked-out branch, its label in its own color; a detached HEAD gets a label
    head: Head,
    cursor: ListCursor,
    scrolloff: usize,
}

impl<'a> CommitListState<'a> {
    pub fn new(
        commits: Vec<CommitInfo<'a>>,
        graph_image_manager: GraphImageManager<'a>,
        graph_cell_width: u16,
        ref_name_to_commit_index_map: FxHashMap<String, usize>,
        search_options: Rc<Cell<SearchOptions>>,
        scrolloff: usize,
    ) -> CommitListState<'a> {
        let commit_hash_set = commits.iter().map(|c| &c.commit.commit_hash).collect();
        let search_matches = vec![false; commits.len()];
        CommitListState {
            commits,
            commit_hash_set,
            graph_image_manager,
            graph_cell_width,
            ref_name_to_commit_index_map,
            query: TreeQuery::default(),
            search_options,
            search_matches,
            picked_commit_hashes: Vec::new(),
            filter_note: None,
            head: Head::None,
            cursor: ListCursor::default(),
            scrolloff,
        }
    }

    pub fn graph_area_cell_width(&self) -> u16 {
        self.graph_cell_width + 1 // right pad
    }

    pub fn picked_commit_hashes(&self) -> &[CommitHash] {
        &self.picked_commit_hashes
    }

    pub fn is_picked(&self, commit_hash: &CommitHash) -> bool {
        self.picked_commit_hashes.contains(commit_hash)
    }

    // Whether the commits are being selected from where the visual selection started.
    pub fn is_visual(&self) -> bool {
        self.cursor.is_visual()
    }

    // Starts or ends a visual selection, picking the commits from where it started to the selected
    // one.
    pub fn toggle_visual(&mut self) {
        if self.commits.is_empty() {
            return;
        }
        self.cursor.toggle_visual();
        self.sync_visual_picks();
    }

    fn sync_visual_picks(&mut self) {
        let Some(range) = self.cursor.visual_range(self.commits.len()) else {
            return;
        };
        let selected: Vec<CommitHash> = self.commits[range]
            .iter()
            .map(|commit_info| commit_info.commit.commit_hash.clone())
            .collect();
        for hash in selected {
            if !self.picked_commit_hashes.contains(&hash) {
                self.picked_commit_hashes.push(hash);
            }
        }
    }

    pub fn clear_picks(&mut self) {
        self.cursor.cancel_visual();
        self.picked_commit_hashes.clear();
    }

    pub fn restore_picked_commit_hashes(&mut self, picked_commit_hashes: Vec<CommitHash>) {
        self.cursor.cancel_visual();
        self.picked_commit_hashes = picked_commit_hashes
            .into_iter()
            .filter(|hash| self.commit_hash_set.contains(hash))
            .collect();
    }

    pub fn len(&self) -> usize {
        self.commits.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.commits.is_empty()
    }

    // The first row shown, for the scrollbar of a read-only panel.
    pub fn offset(&self) -> usize {
        self.cursor.offset()
    }

    #[cfg(test)]
    pub fn subjects(&self) -> Vec<&str> {
        self.commits
            .iter()
            .map(|c| c.commit.subject.as_str())
            .collect()
    }

    pub fn update_height(&mut self, height: usize) {
        self.cursor.fit(height, self.commits.len(), self.scrolloff);
    }

    pub fn ensure_visible_graph_uploaded(&mut self) {
        self.commits
            .iter()
            .skip(self.cursor.offset())
            .take(self.cursor.height())
            .for_each(|commit_info| {
                self.graph_image_manager
                    .ensure_uploaded(&commit_info.commit.commit_hash);
            });
    }

    // The rows the list was last drawn with, 0 before the first draw.
    pub fn height(&self) -> usize {
        self.cursor.height()
    }

    // Uploads every commit's graph image, for a list whose height isn't known yet (the first
    // draw of the submodule panel).
    pub fn ensure_all_graph_uploaded(&mut self) {
        let Self {
            commits,
            graph_image_manager,
            ..
        } = self;
        commits.iter().for_each(|commit_info| {
            graph_image_manager.ensure_uploaded(&commit_info.commit.commit_hash);
        });
    }

    pub fn drain_pending_graph_uploads(&mut self) -> Vec<String> {
        self.graph_image_manager.drain_pending_uploads()
    }

    pub fn select_next(&mut self) {
        self.select_in_view(self.cursor.selected() + 1);
        self.sync_visual_picks();
    }

    pub fn select_prev(&mut self) {
        self.select_in_view(self.cursor.selected().saturating_sub(1));
        self.sync_visual_picks();
    }

    pub fn select_first(&mut self) {
        self.select_in_view(0);
        self.sync_visual_picks();
    }

    pub fn select_last(&mut self) {
        self.select_in_view(usize::MAX);
        self.sync_visual_picks();
    }

    pub fn scroll_down(&mut self) {
        self.cursor.scroll_lines(1, self.commits.len());
        self.sync_visual_picks();
    }

    pub fn scroll_up(&mut self) {
        self.cursor.scroll_lines(-1, self.commits.len());
        self.sync_visual_picks();
    }

    pub fn scroll_down_page(&mut self) {
        self.cursor.scroll_pages(1, self.commits.len());
        self.sync_visual_picks();
    }

    pub fn scroll_up_page(&mut self) {
        self.cursor.scroll_pages(-1, self.commits.len());
        self.sync_visual_picks();
    }

    // Moves the selection half a page by `event`, see `half_page_jump`.
    fn jump_half_page(&mut self, event: UserEvent) {
        if let Some(index) = self.cursor.moved_to(event, self.commits.len()) {
            self.select_in_view(index);
            self.sync_visual_picks();
        }
    }

    // Selects the commit at `index`, scrolling only when it isn't in view: the movement keys, the
    // search, and the refs and hashes picked elsewhere.
    fn select_in_view(&mut self, index: usize) {
        self.cursor.select(index, self.commits.len());
    }

    // Selects the next (or previous) commit the search matches, wrapping around.
    pub fn select_next_match(&mut self, toward: Toward) {
        self.select_match_from(self.cursor.selected(), SearchDirection::from(toward));
    }

    // Selects the commit at `from` when the search matches it (`SearchDirection::Current`), else
    // the next or previous one it matches.
    fn select_match_from(&mut self, from: usize, direction: SearchDirection) {
        if let Some(index) = find_around(self.commits.len(), from, direction, |i| {
            self.search_matches[i]
        }) {
            self.select_in_view(index);
        }
    }

    // Folds the selected merge commit with left, unfolds it with right; None for other events
    // and commits.
    pub fn fold_event(&self, event: UserEvent) -> Option<AppEvent> {
        let commit = self.selected_commit_hash();
        let merge = self.graph_image_manager.graph().merges.get(commit)?;
        let fold = match (event, merge) {
            (UserEvent::NavigateLeft, Fold::Unfolded) => Fold::Folded,
            (UserEvent::NavigateRight, Fold::Folded) => Fold::Unfolded,
            _ => return None,
        };
        let commit = commit.clone();
        Some(AppEvent::FoldMerge { commit, fold })
    }

    // The subject of each commit shown, and whether it is on the current branch; for the tests.
    #[cfg(test)]
    pub fn on_head(&self) -> Vec<(&str, bool)> {
        self.commits
            .iter()
            .map(|info| (info.commit.subject.as_str(), info.on_head))
            .collect()
    }

    pub fn selected_commit_hash(&self) -> &CommitHash {
        &self.commits[self.cursor.selected()].commit.commit_hash
    }

    // The selected commit, counting from 1, and how many there are.
    pub fn position(&self) -> (usize, usize) {
        (self.cursor.selected() + 1, self.commits.len())
    }

    // The selected commit and the commits shown.
    pub fn cursor(&self) -> ListCursor {
        self.cursor
    }

    pub fn reset_height(&mut self, height: usize) {
        self.update_height(height);
    }

    // Shows the selected commit at `row` of those shown, where it can be.
    pub fn restore_selected_row(&mut self, row: usize) {
        let index = self.cursor.selected();
        self.cursor.select_at(index, row, self.commits.len());
    }

    pub fn set_filter_note(&mut self, note: Option<String>) {
        self.filter_note = note;
    }

    pub fn set_head(&mut self, head: Head) {
        self.head = head;
    }

    pub fn select_ref(&mut self, ref_name: &str) {
        if let Some(&index) = self.ref_name_to_commit_index_map.get(ref_name) {
            self.select_in_view(index);
        }
    }

    // Selects the commit; false when the graph doesn't show it.
    pub fn select_commit_hash(&mut self, commit_hash: &CommitHash) -> bool {
        if !self.commit_hash_set.contains(commit_hash) {
            return false;
        }
        let index = self
            .commits
            .iter()
            .position(|commit_info| commit_info.commit.commit_hash == *commit_hash);
        if let Some(index) = index {
            self.select_in_view(index);
        }
        index.is_some()
    }

    // Typing the search, which takes every key.
    pub fn is_typing_search(&self) -> bool {
        self.query.is_editing()
    }

    // A search typed and kept, which n and N go through.
    pub fn search_applied(&self) -> bool {
        !self.query.is_editing() && self.query.search().is_some()
    }

    pub fn search_options(&self) -> SearchOptions {
        self.search_options.get()
    }

    pub fn restore_search_options(&mut self, options: SearchOptions) {
        self.search_options.set(options);
    }

    // Starts typing the search, from the applied one, looking for matches from the selection.
    pub fn start_search(&mut self) {
        if !self.query.is_editing() {
            self.cursor.cancel_visual();
            let from = self.cursor.selected();
            self.query.start(QueryKind::Search, from);
        }
    }

    // The search, where it looks in `part`, to highlight there.
    fn search_in(&self, part: SearchTarget) -> Option<&str> {
        self.query
            .search()
            .filter(|_| self.search_options.get().target.includes(part))
    }

    // The search to type into, see `search_changed`.
    pub fn query_mut(&mut self) -> &mut TreeQuery {
        &mut self.query
    }

    pub fn query(&self) -> &TreeQuery {
        &self.query
    }

    // Finds the matches again once the search or its options changed: while typing, the first
    // from where it started; else the selected commit stays, or the next match.
    pub fn search_changed(&mut self) {
        let from = self.query.step_from(self.cursor.selected());
        self.update_search_matches();
        self.select_match_from(from, SearchDirection::Current);
    }

    pub fn search_refresh_context(&self) -> Option<SearchRefreshContext> {
        self.search_applied().then(|| SearchRefreshContext {
            query: self.query.search().unwrap_or_default().into(),
        })
    }

    pub fn restore_search(&mut self, context: &SearchRefreshContext) {
        self.query = TreeQuery::with_search(context.query.clone());
        self.update_search_matches();
    }

    pub fn cancel_search(&mut self) {
        self.query = TreeQuery::default();
        self.update_search_matches();
    }

    // Moves by the keys moving in a list: the selection, by pages, or only the view; returns
    // whether `event` was one of them.
    pub fn handle_navigation(&mut self, event: UserEvent) -> bool {
        match event {
            UserEvent::NavigateDown | UserEvent::SelectDown => self.select_next(),
            UserEvent::NavigateUp | UserEvent::SelectUp => self.select_prev(),
            UserEvent::GoToTop => self.select_first(),
            UserEvent::GoToBottom => self.select_last(),
            UserEvent::ScrollDown => self.scroll_down(),
            UserEvent::ScrollUp => self.scroll_up(),
            UserEvent::PageDown => self.scroll_down_page(),
            UserEvent::PageUp => self.scroll_up_page(),
            UserEvent::HalfPageDown | UserEvent::HalfPageUp => self.jump_half_page(event),
            _ => return false,
        }
        true
    }

    pub fn toggle_smart_case(&mut self) {
        let mut options = self.search_options.get();
        options.smart_case = !options.smart_case;
        self.search_options.set(options);
        self.search_changed();
    }

    pub fn toggle_fuzzy(&mut self) {
        let mut options = self.search_options.get();
        options.fuzzy = !options.fuzzy;
        self.search_options.set(options);
        self.search_changed();
    }

    pub fn toggle_search_target(&mut self) {
        let mut options = self.search_options.get();
        options.target = options.target.next();
        self.search_options.set(options);
        self.search_changed();
    }

    // How the applied search went, None without one.
    pub fn search_outcome(&self) -> Option<SearchOutcome> {
        let query = self.query.search().filter(|_| self.search_applied())?;
        let selected = self.cursor.selected();
        Some(SearchOutcome::of(
            query,
            selected,
            self.search_matches.iter().copied(),
        ))
    }

    fn update_search_matches(&mut self) {
        let options = self.search_options.get();
        let query = self.query.search();
        for (matched, commit_info) in self.search_matches.iter_mut().zip(&self.commits) {
            *matched = query.is_some_and(|query| commit_matches(commit_info, query, options));
        }
    }

    fn prepared_image(&self, commit_info: &'a CommitInfo) -> &PreparedImage {
        self.graph_image_manager
            .prepared_image(&commit_info.commit.commit_hash)
    }
}

pub struct CommitList<'a> {
    ctx: Rc<AppContext>,
    _marker: std::marker::PhantomData<&'a ()>,
}

// The rows of the commit list under the border of its panel.
pub fn commit_list_inner_height(height: u16) -> usize {
    height.saturating_sub(2) as usize
}

// A commit's date as the commit list shows it, see `[ui.list]` in the config.
pub fn format_date(ctx: &AppContext, date: &chrono::DateTime<chrono::FixedOffset>) -> String {
    let format = &ctx.ui_config.list.date_format;
    if ctx.ui_config.list.date_local {
        date.with_timezone(&chrono::Local)
            .format(format)
            .to_string()
    } else {
        date.format(format).to_string()
    }
}

// The commit list as the first panel of the graph: under a top border titled with its number,
// which stands out while it has the focus, and the count of picked commits.
pub fn render_commit_list_panel<'a>(
    f: &mut Frame,
    area: Rect,
    state: &mut CommitListState<'a>,
    ctx: Rc<AppContext>,
    focused: bool,
) {
    let theme = &ctx.color_theme;
    let picked = match state.picked_commit_hashes().len() {
        0 => None,
        n => Some(format!("{n} picked")),
    };
    let notes = state.filter_note.iter().cloned().chain(picked);
    let block =
        panel_block(focused, theme).title(panel_title("[2] Commits", notes, focused, theme));
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_stateful_widget(CommitList::new(ctx.clone()), inner, state);
    // the bar on the border, the graph starts right after it
    let y = inner.y + state.cursor.row() as u16;
    if inner.x > 0 && y < inner.bottom() && !state.commits.is_empty() {
        mark_selected_bar(f.buffer_mut(), inner.x - 1, y, focused, &ctx.color_theme);
    }
    let theme = &ctx.color_theme;
    render_panel_scrollbar(
        f.buffer_mut(),
        area,
        state.commits.len(),
        state.cursor.offset(),
        focused,
        theme,
    );
}

// The commit list of a submodule's commits, read-only: built once per selection, every commit on
// the submodule's HEAD and its refs shown, with the graph. The submodule's repository is owned by
// the parent, so the borrows live as long as this list.
pub fn submodule_commit_list<'a>(
    repository: &'a Repository,
    graph_image_manager: GraphImageManager<'a>,
    search_options: Rc<Cell<SearchOptions>>,
    scrolloff: usize,
) -> CommitListState<'a> {
    let graph = graph_image_manager.graph();
    let mut ref_name_to_commit_index_map = FxHashMap::default();
    let commits = graph
        .commits
        .iter()
        .enumerate()
        .map(|(i, commit)| {
            let refs = repository.refs(&commit.commit_hash);
            for r in &refs {
                let name = r
                    .named()
                    .map_or_else(|| r.name().to_string(), |n| n.full_name());
                ref_name_to_commit_index_map.insert(name, i);
            }
            CommitInfo::new(commit, refs, repository.branch_tracking(), true)
        })
        .collect();
    let graph_cell_width = graph_image_manager.graph_cell_width();
    CommitListState::new(
        commits,
        graph_image_manager,
        graph_cell_width,
        ref_name_to_commit_index_map,
        search_options,
        scrolloff,
    )
}

impl<'a> CommitList<'a> {
    pub fn new(ctx: Rc<AppContext>) -> Self {
        Self {
            ctx,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<'a> StatefulWidget for CommitList<'a> {
    type State = CommitListState<'a>;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        self.update_state(area, state);

        let constraints = calc_cell_widths(
            area.width,
            self.ctx.ui_config.list.subject_min_width,
            state.graph_area_cell_width(),
            self.ctx.ui_config.list.name_width,
            self.ctx.ui_config.list.date_width,
            &self.ctx.ui_config.list.columns,
        );
        let chunks = Layout::horizontal(constraints).split(area);

        for (i, col) in self.ctx.ui_config.list.columns.iter().enumerate() {
            match col {
                UserListColumnType::Graph => {
                    self.render_graph(buf, chunks[i], state);
                }
                UserListColumnType::Marker => {
                    self.render_marker(buf, chunks[i], state);
                }
                UserListColumnType::Subject => {
                    self.render_subject(buf, chunks[i], state);
                }
                UserListColumnType::Name => {
                    self.render_name(buf, chunks[i], state);
                }
                UserListColumnType::Hash => {
                    self.render_hash(buf, chunks[i], state);
                }
                UserListColumnType::Date => {
                    self.render_date(buf, chunks[i], state);
                }
            }
        }
    }
}

impl CommitList<'_> {
    // The color of a commit's text: `fg`, or dimmed when the commit is not on the current branch.
    fn text_fg(&self, commit_info: &CommitInfo, fg: Color) -> Color {
        if commit_info.on_head {
            fg
        } else {
            self.ctx.color_theme.list_dimmed_fg
        }
    }

    fn update_state(&self, area: Rect, state: &mut CommitListState) {
        state.update_height(area.height as usize);
    }

    fn render_graph(&self, buf: &mut Buffer, area: Rect, state: &CommitListState) {
        if area.is_empty() {
            return;
        }
        self.rendering_commit_info_iter(state)
            .for_each(|(i, commit_info)| {
                let prepared_image = state.prepared_image(commit_info);
                let max_graph_width = area.width.saturating_sub(1) as usize;
                let y = area.top() + i as u16;
                for (x, image_cell) in prepared_image
                    .cells()
                    .iter()
                    .take(max_graph_width)
                    .enumerate()
                {
                    let cell = &mut buf[(area.left() + x as u16, y)];
                    cell.set_symbol(image_cell.symbol());
                    cell.set_style(image_cell.style());
                    cell.set_diff_option(image_cell.diff_option());
                }
            });
    }

    fn render_marker(&self, buf: &mut Buffer, area: Rect, state: &CommitListState) {
        if area.is_empty() {
            return;
        }
        let items: Vec<ListItem> = self
            .rendering_commit_info_iter(state)
            // a plain divider: the colors of the branches are in the graph
            .map(|_| ListItem::new("│".fg(self.ctx.color_theme.divider_fg)))
            .collect();
        Widget::render(List::new(items), area, buf)
    }

    fn render_subject(&self, buf: &mut Buffer, area: Rect, state: &CommitListState) {
        let max_width = (area.width as usize).saturating_sub(2);
        if area.is_empty() || max_width == 0 {
            return;
        }
        let items: Vec<ListItem> = self
            .rendering_commit_info_iter(state)
            .map(|(i, commit_info)| {
                let mut spans = refs_spans(
                    commit_info,
                    state.search_in(SearchTarget::Ref),
                    state.search_options(),
                    &state.head,
                    &self.ctx.color_theme,
                );
                let ref_spans_width: usize = spans.iter().map(|s| s.width()).sum();
                let max_width = max_width.saturating_sub(ref_spans_width);
                let commit = commit_info.commit;
                let subject_fg = self.text_fg(commit_info, self.ctx.color_theme.list_subject_fg);
                if max_width > ELLIPSIS.len() {
                    spans.extend(highlighted_cut(
                        &commit.subject,
                        max_width,
                        Style::default().fg(subject_fg),
                        state.search_in(SearchTarget::Subject),
                        state.search_options(),
                        &self.ctx.color_theme,
                    ));
                }
                self.to_commit_list_item(i, spans, state)
            })
            .collect();
        Widget::render(List::new(items), area, buf);
    }

    fn render_name(&self, buf: &mut Buffer, area: Rect, state: &CommitListState) {
        let max_width = (area.width as usize).saturating_sub(2);
        if area.is_empty() || max_width == 0 {
            return;
        }
        let items: Vec<ListItem> = self
            .rendering_commit_info_iter(state)
            .map(|(i, commit_info)| {
                let commit = commit_info.commit;
                let name_fg = self.text_fg(commit_info, self.ctx.color_theme.list_name_fg);
                let spans = highlighted_cut(
                    &commit.author_name,
                    max_width,
                    Style::default().fg(name_fg),
                    state.search_in(SearchTarget::Author),
                    state.search_options(),
                    &self.ctx.color_theme,
                );
                self.to_commit_list_item(i, spans, state)
            })
            .collect();
        Widget::render(List::new(items), area, buf);
    }

    fn render_hash(&self, buf: &mut Buffer, area: Rect, state: &CommitListState) {
        if area.is_empty() {
            return;
        }
        let items: Vec<ListItem> = self
            .rendering_commit_info_iter(state)
            .map(|(i, commit_info)| {
                let hash_fg = self.text_fg(commit_info, self.ctx.color_theme.list_hash_fg);
                let hash = commit_info.commit.commit_hash.as_short_hash();
                let spans = highlighted_text(
                    hash,
                    hash.len(),
                    Style::default().fg(hash_fg),
                    state.search_in(SearchTarget::Hash),
                    state.search_options(),
                    &self.ctx.color_theme,
                );
                self.to_commit_list_item(i, spans, state)
            })
            .collect();
        Widget::render(List::new(items), area, buf);
    }

    fn render_date(&self, buf: &mut Buffer, area: Rect, state: &CommitListState) {
        if area.is_empty() {
            return;
        }
        let items: Vec<ListItem> = self
            .rendering_commit_info_iter(state)
            .map(|(i, commit_info)| {
                let date_str = format_date(&self.ctx, &commit_info.commit.author_date);
                self.to_commit_list_item(
                    i,
                    vec![date_str.fg(self.text_fg(commit_info, self.ctx.color_theme.list_date_fg))],
                    state,
                )
            })
            .collect();
        Widget::render(List::new(items), area, buf);
    }

    fn rendering_commit_info_iter<'a>(
        &'a self,
        state: &'a CommitListState,
    ) -> impl Iterator<Item = (usize, &'a CommitInfo<'a>)> {
        state
            .commits
            .iter()
            .skip(state.cursor.offset())
            .take(state.cursor.height())
            .enumerate()
    }

    fn to_commit_list_item<'a, 'b>(
        &'b self,
        i: usize,
        spans: Vec<Span<'a>>,
        state: &'b CommitListState,
    ) -> ListItem<'a> {
        let mut spans = spans;
        spans.insert(0, Span::raw(" "));
        spans.push(Span::raw(" "));
        let mut line = Line::from(spans);
        // the selection only colors the background, the text keeps its colors
        let index = state.cursor.offset() + i;
        let selected = index == state.cursor.selected();
        let picked = state.is_picked(&state.commits[index].commit.commit_hash);
        // while selecting, the cursor is picked like the rows it extends; after v stops it goes
        // back to the selected color
        if picked && (state.is_visual() || !selected) {
            // spans keep their own colors over the line's, so each one takes the picked style
            let picked = picked_style(&self.ctx.color_theme);
            line.spans = line
                .spans
                .into_iter()
                .map(|span| span.patch_style(picked))
                .collect();
            line = line.patch_style(picked);
        } else if selected {
            line = line.bg(self.ctx.color_theme.list_selected_bg);
        }
        ListItem::new(line)
    }
}

// Whether the search matches the commit, in what it looks in: its refs (not stashes), subject,
// author or short hash.
fn commit_matches(info: &CommitInfo, query: &str, options: SearchOptions) -> bool {
    let commit = info.commit;
    let matches = |text: &str| name_matches(text, query, options);
    options.target.includes(SearchTarget::Ref)
        && info
            .refs
            .iter()
            .any(|r| !matches!(r, Ref::Stash { .. }) && matches(r.name()))
        || options.target.includes(SearchTarget::Subject) && matches(&commit.subject)
        || options.target.includes(SearchTarget::Author) && matches(&commit.author_name)
        || options.target.includes(SearchTarget::Hash)
            && matches(commit.commit_hash.as_short_hash())
}

// `text` cut to `width` with an ellipsis, what `search` matches in it highlighted, as everywhere.
fn highlighted_cut(
    text: &str,
    width: usize,
    style: Style,
    search: Option<&str>,
    options: SearchOptions,
    theme: &ColorTheme,
) -> Vec<Span<'static>> {
    if text.width() <= width {
        return highlighted_text(text, text.len(), style, search, options, theme);
    }
    // the bytes of the first chars that fit before the ellipsis, measured as ratatui draws them
    let limit = width.saturating_sub(ELLIPSIS.len());
    let mut used = 0;
    let shown = text
        .char_indices()
        .find(|(_, c)| {
            used += c.width().unwrap_or(0);
            used > limit
        })
        .map_or(text.len(), |(i, _)| i);
    let mut spans = highlighted_text(text, shown, style, search, options, theme);
    spans.push(Span::styled(ELLIPSIS, style));
    spans
}

// The refs at the commit as labels tinted with their colors, each with its icon: branches, remote
// branches, tags and stashes, what `search` matches in their names highlighted. The commit list
// and the details show them alike.
pub fn refs_spans(
    commit_info: &CommitInfo,
    search: Option<&str>,
    options: SearchOptions,
    head: &Head,
    color_theme: &ColorTheme,
) -> Vec<Span<'static>> {
    let mut spans = vec![];
    if matches!(head, Head::Detached { target } if *target == commit_info.commit.commit_hash) {
        spans.push(Span::styled(
            " HEAD ",
            ref_label_style(color_theme.list_ref_head_fg),
        ));
        spans.push(Span::raw(" "));
    }
    let head_branch = match head {
        Head::Branch { name, .. } => Some(name.as_str()),
        _ => None,
    };
    for r in &commit_info.refs {
        let name = r.name();
        let style = ref_label_style(ref_color(r, head_branch, color_theme));
        let icon = match r {
            Ref::Branch { .. } if commit_info.with_upstream.contains(&name) => Some(UPSTREAM_ICON),
            Ref::Branch { .. } => None,
            Ref::RemoteBranch { .. } => Some(REMOTE_BRANCH_ICON),
            Ref::Tag { .. } => Some(TAG_ICON),
            Ref::Stash { .. } => Some(STASH_ICON),
        };
        let prefix = icon.map_or(" ".into(), |icon| format!(" {icon} "));
        // stashes aren't searched
        let search = search.filter(|_| !matches!(r, Ref::Stash { .. }));
        spans.push(Span::styled(prefix, style));
        spans.extend(highlighted_text(
            name,
            name.len(),
            style,
            search,
            options,
            color_theme,
        ));
        spans.push(Span::styled(" ", style));
        spans.push(Span::raw(" "));
    }
    spans
}

// The color of the label of `r`, the checked-out branch's its own.
pub fn ref_color(r: &Ref, head_branch: Option<&str>, theme: &ColorTheme) -> Color {
    match r {
        Ref::Branch { name, .. } if head_branch == Some(name.as_str()) => theme.list_ref_head_fg,
        Ref::Branch { .. } => theme.list_ref_branch_fg,
        Ref::RemoteBranch { .. } => theme.list_ref_remote_branch_fg,
        Ref::Tag { .. } => theme.list_ref_tag_fg,
        Ref::Stash { .. } => theme.list_ref_stash_fg,
    }
}

// A ref label in `color`, on a dark tint of it.
pub fn ref_label_style(color: Color) -> Style {
    let style = Style::default().add_modifier(Modifier::BOLD);
    match color {
        Color::Rgb(r, g, b) => {
            let [ir, ig, ib] = [0x1E_u16, 0x1E, 0x24];
            let tint = |c: u8, base: u16| ((c as u16 * 3 + base * 7) / 10) as u8;
            style
                .fg(color)
                .bg(Color::Rgb(tint(r, ir), tint(g, ig), tint(b, ib)))
        }
        // named colors cannot be tinted
        _ => style.fg(LABEL_INK).bg(color),
    }
}

const LABEL_INK: Color = Color::Rgb(0x1E, 0x1E, 0x24);

fn calc_cell_widths(
    area_width: u16,
    subject_min_width: u16,
    graph_width: u16,
    name_width: u16,
    date_width: u16,
    columns: &[UserListColumnType],
) -> Vec<Constraint> {
    let pad = 2;
    let (
        mut graph_cell_width,
        mut marker_cell_width,
        mut name_cell_width,
        mut hash_cell_width,
        mut date_cell_width,
    ) = (0, 0, 0, 0, 0);

    for col in columns {
        match col {
            UserListColumnType::Graph => {
                graph_cell_width = graph_width;
            }
            UserListColumnType::Marker => {
                marker_cell_width = 1;
            }
            UserListColumnType::Name => {
                name_cell_width = name_width + pad;
            }
            UserListColumnType::Hash => {
                hash_cell_width = 7 + pad;
            }
            UserListColumnType::Date => {
                date_cell_width = date_width + pad;
            }
            UserListColumnType::Subject => {}
        }
    }

    let mut total_width = graph_cell_width
        + marker_cell_width
        + hash_cell_width
        + name_cell_width
        + date_cell_width
        + subject_min_width;

    if total_width > area_width {
        total_width = total_width.saturating_sub(name_cell_width);
        name_cell_width = 0;
    }
    if total_width > area_width {
        total_width = total_width.saturating_sub(date_cell_width);
        date_cell_width = 0;
    }
    if total_width > area_width {
        hash_cell_width = 0;
    }

    let mut constraints = Vec::new();
    for col in columns {
        match col {
            UserListColumnType::Graph => {
                constraints.push(Constraint::Length(graph_cell_width));
            }
            UserListColumnType::Marker => {
                constraints.push(Constraint::Length(marker_cell_width));
            }
            UserListColumnType::Subject => {
                constraints.push(Constraint::Min(0));
            }
            UserListColumnType::Name => {
                constraints.push(Constraint::Length(name_cell_width));
            }
            UserListColumnType::Hash => {
                constraints.push(Constraint::Length(hash_cell_width));
            }
            UserListColumnType::Date => {
                constraints.push(Constraint::Length(date_cell_width));
            }
        }
    }
    constraints
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::{
        color::GraphColorSet,
        config::{CoreConfig, GraphColorConfig, UiConfig},
        git::{Head, Repository},
        graph::{calc_graph, CellWidthType, GraphImageWidthMode, GraphStyle},
        keybind::KeyBind,
        protocol::ImageProtocol,
        view::ListRefreshViewContext,
    };

    use super::*;

    // The row of the selected commit among those shown, the first shown, and how many are.
    fn status(state: &CommitListState) -> (usize, usize, usize) {
        let cursor = state.cursor();
        (cursor.row(), cursor.offset(), cursor.height())
    }

    // The search's message, and whether it found something.
    fn matched(state: &CommitListState) -> Option<(String, bool)> {
        let outcome = state.search_outcome()?;
        let found = matches!(outcome, SearchOutcome::Found { .. });
        Some((outcome.to_string(), found))
    }

    fn with_commit_list_state<R>(
        subjects: &[&str],
        f: impl FnOnce(&mut CommitListState<'_>) -> R,
    ) -> R {
        with_commit_list_state_and_protocol(subjects, ImageProtocol::Iterm2, f)
    }

    fn with_commit_list_state_and_protocol<R>(
        subjects: &[&str],
        image_protocol: ImageProtocol,
        f: impl FnOnce(&mut CommitListState<'_>) -> R,
    ) -> R {
        let commits: Vec<Commit> = subjects
            .iter()
            .enumerate()
            .map(|(i, subject)| Commit {
                commit_hash: CommitHash::from(format!("{:040x}", i + 1).as_str()),
                subject: (*subject).into(),
                ..Commit::default()
            })
            .collect();
        let repository = Repository::new(
            PathBuf::new(),
            PathBuf::new(),
            commits,
            FxHashMap::default(),
            Head::None,
        );
        let graph = calc_graph(&repository, None, &Default::default());
        let graph_color_set = GraphColorSet::new(&GraphColorConfig::default());
        let image_params =
            crate::graph::ImageParams::new(&graph_color_set, CellWidthType::Double, false);
        let commit_infos = graph
            .commits
            .iter()
            .map(|commit| {
                let refs = repository.refs(&commit.commit_hash);
                CommitInfo::new(commit, refs, repository.branch_tracking(), true)
            })
            .collect();
        let graph_image_manager = GraphImageManager::new(
            graph,
            image_params,
            CellWidthType::Double,
            GraphStyle::Rounded,
            GraphImageWidthMode::Compact,
            image_protocol,
        );
        let mut state = CommitListState::new(
            commit_infos,
            graph_image_manager,
            0,
            FxHashMap::default(),
            Default::default(),
            0,
        );
        state.reset_height(subjects.len());
        f(&mut state)
    }

    // A key typed into the search, as the commit list's view does.
    fn type_key(state: &mut CommitListState<'_>, key: KeyEvent) {
        if state.query_mut().handle_key(key).is_some() {
            state.search_changed();
        }
    }

    fn input_search_query(state: &mut CommitListState<'_>, query: &str) {
        state.start_search();
        for c in query.chars() {
            type_key(state, KeyEvent::from(KeyCode::Char(c)));
        }
    }

    fn apply_search(state: &mut CommitListState<'_>) {
        type_key(state, KeyEvent::from(KeyCode::Enter));
    }

    fn typed(state: &CommitListState<'_>) -> Option<String> {
        state.query().status_input().map(|(text, _)| text)
    }

    #[test]
    fn test_branch_and_its_upstream_show_as_one_label() {
        let commit = Commit::default();
        let target = commit.commit_hash.clone();
        let branch = |name: &str| Ref::Branch {
            name: name.into(),
            target: target.clone(),
        };
        let remote = |name: &str| Ref::RemoteBranch {
            name: name.into(),
            target: target.clone(),
        };
        let refs = [
            branch("master"),
            remote("origin/master"),
            remote("fork/master"),
            branch("feat"),
        ];
        let tracking = |upstream: &str| Tracking {
            upstream: upstream.into(),
            ahead: 0,
            behind: 0,
        };
        let tracking: FxHashMap<String, Tracking> = [
            ("master".to_string(), tracking("origin/master")),
            // elsewhere
            ("feat".to_string(), tracking("origin/feat")),
        ]
        .into_iter()
        .collect();
        let info = CommitInfo::new(&commit, refs.iter().collect(), &tracking, true);
        let theme = ColorTheme::default();
        let label: String = refs_spans(&info, None, SearchOptions::default(), &Head::None, &theme)
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        // another remote's branch isn't its upstream
        assert_eq!(
            label,
            format!(" {UPSTREAM_ICON} master   {REMOTE_BRANCH_ICON} fork/master   feat  ")
        );
    }

    #[test]
    fn test_rows_look_like_the_other_lists() {
        use ratatui::{backend::TestBackend, Terminal};

        use crate::test_support::ui::{
            assert_only_bg_changed, assert_row_bg, column_of, find_row, panel_row, test_ctx,
        };

        let theme = ColorTheme::default();
        with_commit_list_state(&["first", "second"], |state| {
            for focused in [true, false] {
                let draw = |state: &mut CommitListState| {
                    // as the app does before drawing
                    state.ensure_visible_graph_uploaded();
                    let mut terminal = Terminal::new(TestBackend::new(60, 6)).unwrap();
                    terminal
                        .draw(|f| render_commit_list_panel(f, f.area(), state, test_ctx(), focused))
                        .unwrap();
                    terminal.backend().buffer().clone()
                };
                state.select_next();
                let plain = draw(state);
                state.select_prev();
                let selected = draw(state);
                let y = find_row(&selected, "first");
                let row = panel_row(&selected, "Commits", y, 0);
                // the graph is images, drawn over the cells: from the space before the subject
                let x = column_of(&selected, y, "first") - 1;
                let text = Rect::new(x, y, row.right() - x, 1);
                assert_row_bg(&selected, text, theme.list_selected_bg);
                assert_only_bg_changed(&plain, &selected, text);

                // a picked commit: across the row too
                state.toggle_visual();
                state.toggle_visual();
                state.select_next();
                let buf = draw(state);
                let text = Rect::new(x, y, row.right() - x, 1);
                assert_row_bg(&buf, text, theme.list_picked_bg);
                state.select_prev();
                state.clear_picks();
            }
        });
    }

    #[test]
    fn test_graph_diff_preserves_neighboring_cells_and_skips_unchanged_images() {
        for image_protocol in [
            ImageProtocol::Iterm2,
            ImageProtocol::Kitty,
            ImageProtocol::KittyUnicode { tmux: false },
            ImageProtocol::KittyUnicode { tmux: true },
        ] {
            with_commit_list_state_and_protocol(&["first", "second"], image_protocol, |state| {
                state.ensure_visible_graph_uploaded();
                let ctx = Rc::new(AppContext {
                    keybind: KeyBind::new(None),
                    core_config: CoreConfig::default(),
                    ui_config: UiConfig::default(),
                    color_theme: ColorTheme::default(),
                    image_protocol,
                    graph: Default::default(),
                    running_task: Default::default(),
                    key_names: Default::default(),
                    layout: Default::default(),
                    wrap: Default::default(),
                    diff_context: std::cell::Cell::new(crate::git::DEFAULT_DIFF_CONTEXT),
                    search_options: Default::default(),
                });
                let list = CommitList::new(ctx);
                // Width 2 clips the image cells. Its escape sequence must not skip the next row.
                for width in [2, 10] {
                    let area = Rect::new(0, 0, width, 2);
                    let mut before = Buffer::empty(area);
                    list.render_graph(&mut before, area, state);
                    let mut after = Buffer::empty(area);
                    list.render_graph(&mut after, area, state);
                    assert!(before.diff(&after).is_empty());

                    after[(width - 1, 0)].set_symbol("x");
                    after[(0, 1)].set_fg(Color::Red);
                    let positions: Vec<_> = before
                        .diff(&after)
                        .into_iter()
                        .map(|(x, y, _)| (x, y))
                        .collect();
                    assert_eq!(positions, vec![(width - 1, 0), (0, 1)]);
                }
            });
        }
    }

    #[test]
    fn test_visual_selects_the_commits_between_the_ends() {
        with_commit_list_state(&["first", "second", "third"], |state| {
            state.toggle_visual();
            state.select_last();
            assert_eq!(state.picked_commit_hashes().len(), 3);
            // ending the visual selection keeps the commits picked
            state.toggle_visual();
            assert_eq!(state.picked_commit_hashes().len(), 3);

            // a new visual selection keeps the commits picked before it
            state.select_first();
            state.toggle_visual();
            assert_eq!(state.picked_commit_hashes().len(), 3);
            state.toggle_visual();

            state.clear_picks();
            state.select_first();
            state.toggle_visual();
            state.toggle_visual();
            assert_eq!(state.picked_commit_hashes().len(), 1);
            state.select_next();
            state.toggle_visual();
            state.toggle_visual();
            assert_eq!(state.picked_commit_hashes().len(), 2);

            state.clear_picks();
            assert!(state.picked_commit_hashes().is_empty());
        });
    }

    #[test]
    fn test_pick_refresh_preserves_loaded_hashes_and_drops_missing_hashes() {
        let context = with_commit_list_state(&["first", "second"], |state| {
            state.toggle_visual();
            state.select_next();
            state.toggle_visual();
            ListRefreshViewContext::from(&*state)
        });
        assert_eq!(context.picked_commit_hashes.len(), 2);

        with_commit_list_state(&["first", "second"], |state| {
            state.restore_picked_commit_hashes(context.picked_commit_hashes.clone());
            assert_eq!(state.picked_commit_hashes(), context.picked_commit_hashes);
        });
        with_commit_list_state(&["first"], |state| {
            state.restore_picked_commit_hashes(context.picked_commit_hashes);
            assert_eq!(state.picked_commit_hashes().len(), 1);
        });
    }

    #[test]
    fn test_pick_only_changes_row_colors() {
        with_commit_list_state(&["first", "second", "third"], |state| {
            let mut ui_config = UiConfig::default();
            ui_config.list.columns = vec![
                UserListColumnType::Marker,
                UserListColumnType::Subject,
                UserListColumnType::Name,
                UserListColumnType::Hash,
                UserListColumnType::Date,
            ];
            let ctx = Rc::new(AppContext {
                keybind: KeyBind::new(None),
                core_config: CoreConfig::default(),
                ui_config,
                color_theme: ColorTheme::default(),
                image_protocol: ImageProtocol::Iterm2,
                graph: Default::default(),
                running_task: Default::default(),
                key_names: Default::default(),
                layout: Default::default(),
                wrap: Default::default(),
                diff_context: std::cell::Cell::new(crate::git::DEFAULT_DIFF_CONTEXT),
                search_options: Default::default(),
            });
            let area = Rect::new(0, 0, 80, 3);
            let mut before = Buffer::empty(area);
            CommitList::new(ctx.clone()).render(area, &mut before, state);

            // pick the first commit, then move the cursor away from it
            state.toggle_visual();
            state.toggle_visual();
            state.select_next();
            let mut after = Buffer::empty(area);
            CommitList::new(ctx.clone()).render(area, &mut after, state);

            // the marker column is unchanged, the text of the picked row is recolored
            assert_eq!(after[(0, 0)].symbol(), "│");
            assert_eq!(after[(0, 0)].fg, ctx.color_theme.divider_fg);
            assert_eq!(after[(2, 0)].bg, ctx.color_theme.list_picked_bg);
            assert_eq!(after[(2, 0)].fg, ctx.color_theme.list_picked_fg);
            for x in 0..area.width {
                assert_eq!(after[(x, 0)].symbol(), before[(x, 0)].symbol());
            }

            // while selecting, the cursor row is picked too
            state.toggle_visual();
            let mut visual = Buffer::empty(area);
            CommitList::new(ctx.clone()).render(area, &mut visual, state);
            assert_eq!(visual[(2, 1)].bg, ctx.color_theme.list_picked_bg);
        });
    }

    #[test]
    fn test_scrolloff_keeps_context_for_selection_and_single_row_scrolling() {
        with_commit_list_state(&["commit"; 12], |state| {
            state.scrolloff = 2;
            state.reset_height(6);

            for _ in 0..4 {
                state.select_next();
            }
            assert_eq!(status(state), (3, 1, 6));

            state.select_prev();
            state.select_prev();
            assert_eq!(status(state), (2, 0, 6));

            state.select_in_view(7);
            assert_eq!(status(state), (3, 4, 6));
            state.scroll_down();
            assert_eq!(status(state), (2, 5, 6));
            state.scroll_up();
            assert_eq!(status(state), (3, 4, 6));

            state.select_last();
            assert_eq!(status(state), (5, 6, 6));
            state.select_prev();
            assert_eq!(status(state), (4, 6, 6));
        });
    }

    #[test]
    fn test_scrolloff_is_limited_by_list_height() {
        with_commit_list_state(&["commit"; 8], |state| {
            state.scrolloff = 10;
            state.reset_height(2);

            state.select_next();
            state.select_next();
            assert_eq!(status(state), (1, 1, 2));

            state.select_in_view(6);
            state.restore_selected_row(1);
            assert_eq!(status(state), (1, 5, 2));
        });
    }

    #[test]
    fn test_page_scrolloff_keeps_context_until_final_page() {
        with_commit_list_state(&["commit"; 30], |state| {
            state.scrolloff = 2;
            state.reset_height(10);

            state.scroll_down_page();
            assert_eq!(status(state), (2, 8, 10));
            state.scroll_down_page();
            assert_eq!(status(state), (2, 18, 10));
            state.scroll_down_page();
            assert_eq!(status(state), (9, 20, 10));
            state.scroll_down_page();
            assert_eq!(status(state), (9, 20, 10));

            state.scroll_up_page();
            assert_eq!(status(state), (7, 12, 10));
            state.scroll_up_page();
            assert_eq!(status(state), (7, 2, 10));
            state.scroll_up_page();
            assert_eq!(status(state), (0, 0, 10));
        });
    }

    #[test]
    fn test_half_page_jumps_then_scrolls() {
        with_commit_list_state(&["commit"; 26], |state| {
            state.scrolloff = 2;
            state.reset_height(10);
            state.select_in_view(12);
            assert_eq!(status(state), (7, 5, 10));

            // the selection moves down the page, the list stays
            state.jump_half_page(UserEvent::HalfPageDown);
            assert_eq!(status(state), (7, 10, 10));
            // then scrolls, keeping 2 rows under it
            state.jump_half_page(UserEvent::HalfPageDown);
            assert_eq!(status(state), (7, 15, 10));
            // to the last commit
            state.jump_half_page(UserEvent::HalfPageDown);
            assert_eq!(status(state), (9, 16, 10));
            state.jump_half_page(UserEvent::HalfPageUp);
            assert_eq!(status(state), (4, 16, 10));
        });
    }

    #[test]
    fn test_full_page_scroll_reaches_last_commit_after_final_page() {
        with_commit_list_state(&["commit"; 25], |state| {
            state.scrolloff = 2;
            state.reset_height(10);
            state.select_in_view(12);

            state.scroll_down_page();
            assert_eq!(status(state), (7, 15, 10));
            state.scroll_down_page();
            assert_eq!(status(state), (9, 15, 10));
        });
    }

    #[test]
    fn test_refresh_restores_selected_row_with_scrolloff() {
        with_commit_list_state(&["commit"; 30], |state| {
            state.scrolloff = 2;
            state.reset_height(10);
            state.select_in_view(15);
            // at the row a reload restores it to
            state.restore_selected_row(2);
            assert_eq!(status(state), (2, 13, 10));

            state.restore_selected_row(7);
            assert_eq!(status(state), (7, 8, 10));
            assert_eq!(state.cursor.selected(), 15);
        });
    }

    #[test]
    fn test_zero_scrolloff_preserves_page_scroll_behavior() {
        with_commit_list_state(&["commit"; 25], |state| {
            state.reset_height(10);
            state.select_in_view(12);
            // at the row a reload restores it to
            state.restore_selected_row(0);
            assert_eq!(status(state), (0, 12, 10));

            state.scroll_down_page();
            assert_eq!(status(state), (7, 15, 10));
            state.scroll_down_page();
            assert_eq!(status(state), (9, 15, 10));
        });
    }

    #[test]
    fn test_search_moves_the_selection_without_scrolling_to_a_match_in_view() {
        let mut subjects: Vec<String> = (0..30).map(|i| format!("commit {i}")).collect();
        subjects[3] = "target above".into();
        subjects[20] = "target shown".into();
        subjects[25] = "target below".into();
        let subjects: Vec<&str> = subjects.iter().map(String::as_str).collect();
        with_commit_list_state(&subjects, |state| {
            state.scrolloff = 2;
            state.reset_height(10);
            state.select_in_view(15);
            // at the row a reload restores it to
            state.restore_selected_row(2);
            assert_eq!(status(state), (2, 13, 10));

            // a match already shown: the list stays where it is
            input_search_query(state, "target");
            apply_search(state);
            assert_eq!(state.cursor.selected(), 20);
            assert_eq!(status(state), (7, 13, 10));

            // past the rows shown: as little scrolling as shows it, with the scrolloff under it
            state.select_next_match(Toward::Next);
            assert_eq!(state.cursor.selected(), 25);
            assert_eq!(status(state), (7, 18, 10));

            // and above them, with the scrolloff over it
            state.select_next_match(Toward::Next);
            assert_eq!(state.cursor.selected(), 3);
            assert_eq!(status(state), (2, 1, 10));
        });
    }

    #[test]
    fn test_restore_search_recalculates_matches_with_applied_options() {
        let (context, options) = with_commit_list_state(&["Fix parser", "other"], |state| {
            input_search_query(state, "fx");
            state.toggle_smart_case();
            state.toggle_fuzzy();
            apply_search(state);

            (
                state.search_refresh_context().unwrap(),
                state.search_options(),
            )
        });

        assert_eq!(context, SearchRefreshContext { query: "fx".into() });
        assert_eq!(
            options,
            SearchOptions {
                target: SearchTarget::All,
                smart_case: true,
                fuzzy: true,
            }
        );

        with_commit_list_state(&["unrelated", "FIX new", "fix parser"], |state| {
            state.restore_search_options(options);
            state.restore_search(&context);

            assert_eq!(state.search_refresh_context(), Some(context.clone()));
            assert_eq!(
                state.commits[state.cursor.selected()].commit.subject,
                "unrelated"
            );

            state.select_next_match(Toward::Next);
            assert_eq!(
                matched(state),
                Some(("Match 1 of 2 (query: \"fx\")".into(), true))
            );
            assert_eq!(
                state.commits[state.cursor.selected()].commit.subject,
                "FIX new"
            );

            state.select_next_match(Toward::Next);
            assert_eq!(
                matched(state),
                Some(("Match 2 of 2 (query: \"fx\")".into(), true))
            );
            assert_eq!(
                state.commits[state.cursor.selected()].commit.subject,
                "fix parser"
            );
        });
    }

    #[test]
    fn test_restore_search_options_without_applied_search() {
        let options = with_commit_list_state(&["FIX"], |state| {
            state.toggle_smart_case();
            state.search_options()
        });

        with_commit_list_state(&["FIX"], |state| {
            state.restore_search_options(options);
            input_search_query(state, "fix");
            apply_search(state);

            assert_eq!(
                matched(state),
                Some(("Match 1 of 1 (query: \"fix\")".into(), true))
            );
        });
    }

    #[test]
    fn test_restore_search_keeps_selected_match_position() {
        let context = with_commit_list_state(&["fix"], |state| {
            input_search_query(state, "fix");
            apply_search(state);
            state.search_refresh_context().unwrap()
        });

        with_commit_list_state(&["first", "second", "fix", "last"], |state| {
            state.reset_height(2);
            state.select_in_view(2);
            assert_eq!(status(state), (1, 1, 2));

            state.restore_search(&context);

            assert_eq!(status(state), (1, 1, 2));
            assert_eq!(
                matched(state),
                Some(("Match 1 of 1 (query: \"fix\")".into(), true))
            );
        });
    }

    #[test]
    fn test_search_options_are_reused_for_next_search() {
        with_commit_list_state(&["FIX"], |state| {
            input_search_query(state, "fix");
            state.toggle_smart_case();
            apply_search(state);
            state.cancel_search();

            input_search_query(state, "fix");
            apply_search(state);

            assert_eq!(
                matched(state),
                Some(("Match 1 of 1 (query: \"fix\")".into(), true))
            );
        });
    }

    #[test]
    fn test_resume_search_preserves_query_options_matches_and_view_position() {
        with_commit_list_state(&["other", "Fix parser", "FIX graph", "last"], |state| {
            state.reset_height(2);
            state.toggle_smart_case();
            state.toggle_fuzzy();
            state.toggle_search_target();
            input_search_query(state, "fx");
            type_key(state, KeyEvent::from(KeyCode::Left));
            apply_search(state);
            state.select_next_match(Toward::Next);

            let options = state.search_options();
            let position = status(state);
            let result = matched(state);
            let matches = state.search_matches.clone();
            assert_eq!(result, Some(("Match 2 of 2 (query: \"fx\")".into(), true)));

            state.start_search();

            assert_eq!(typed(state), Some("/fx".into()));
            assert_eq!(state.query().status_input().unwrap().1, 3);
            assert_eq!(state.search_options(), options);
            assert_eq!(status(state), position);
            assert_eq!(state.search_matches, matches);

            apply_search(state);
            assert_eq!(matched(state), result);
            assert_eq!(status(state), position);
        });
    }

    #[test]
    fn test_resume_search_edits_from_current_selection() {
        with_commit_list_state(
            &["fix parser older", "fix graph", "fix parser newer"],
            |state| {
                input_search_query(state, "fix");
                apply_search(state);
                state.select_next_match(Toward::Next);
                assert_eq!(state.cursor.selected(), 1);

                state.start_search();
                for c in " parser".chars() {
                    type_key(state, KeyEvent::from(KeyCode::Char(c)));
                }
                assert_eq!(state.cursor.selected(), 2);
                assert!(!state.search_matches[1]);
                assert!(state.search_matches[2]);

                type_key(state, KeyEvent::from(KeyCode::Backspace));
                assert_eq!(typed(state), Some("/fix parse".into()));
                apply_search(state);
                assert_eq!(
                    matched(state),
                    Some(("Match 2 of 2 (query: \"fix parse\")".into(), true))
                );
            },
        );
    }

    #[test]
    fn test_resume_search_does_not_move_selection_off_an_unmatched_commit() {
        with_commit_list_state(&["fix", "other", "fix again"], |state| {
            input_search_query(state, "fix");
            apply_search(state);
            state.select_next();
            let position = status(state);
            let result = matched(state);

            state.start_search();
            apply_search(state);

            assert_eq!(status(state), position);
            assert_eq!(state.cursor.selected(), 1);
            assert_eq!(matched(state), result);
        });
    }

    #[test]
    fn test_resume_search_without_matches_can_be_corrected() {
        with_commit_list_state(&["fix", "other"], |state| {
            input_search_query(state, "fixx");
            apply_search(state);
            let result = matched(state);

            state.start_search();
            assert_eq!(typed(state), Some("/fixx".into()));
            apply_search(state);
            assert_eq!(matched(state), result);

            state.start_search();
            type_key(state, KeyEvent::from(KeyCode::Backspace));
            apply_search(state);
            assert_eq!(
                matched(state),
                Some(("Match 1 of 1 (query: \"fix\")".into(), true))
            );
        });
    }

    #[test]
    fn test_cancel_resumed_search_starts_next_search_empty() {
        with_commit_list_state(&["fix", "other"], |state| {
            input_search_query(state, "fix");
            apply_search(state);
            state.start_search();
            state.cancel_search();

            assert!(!state.search_applied() && !state.is_typing_search());
            assert!(state.search_matches.iter().all(|m| !m));
            state.start_search();
            assert_eq!(typed(state), Some("/".into()));
        });
    }

    #[test]
    fn test_clear_resumed_query_clears_matches_and_applies_inactive_search() {
        for fuzzy in [false, true] {
            with_commit_list_state(&["fix", "other"], |state| {
                if fuzzy {
                    state.toggle_fuzzy();
                }
                input_search_query(state, "fix");
                apply_search(state);
                state.start_search();
                type_key(
                    state,
                    KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
                );

                assert_eq!(typed(state), Some("/".into()));
                assert!(state.search_matches.iter().all(|m| !m));
                apply_search(state);
                assert!(!state.search_applied() && !state.is_typing_search());
                assert_eq!(matched(state), None);

                state.start_search();
                assert_eq!(typed(state), Some("/".into()));
                type_key(state, KeyEvent::from(KeyCode::Char('o')));
                apply_search(state);
                assert_eq!(
                    matched(state),
                    Some(("Match 1 of 1 (query: \"o\")".into(), true))
                );
            });
        }
    }

    #[test]
    fn test_resume_search_moves_cursor_to_end_of_long_unicode_query() {
        let query = "検索".repeat(40);
        with_commit_list_state(&[query.as_str()], |state| {
            input_search_query(state, &query);
            type_key(state, KeyEvent::from(KeyCode::Home));
            apply_search(state);

            state.start_search();
            assert_eq!(typed(state), Some(format!("/{query}")));
            assert_eq!(state.query().status_input().unwrap().1, 161);
            type_key(state, KeyEvent::from(KeyCode::Char('追')));
            assert_eq!(typed(state), Some(format!("/{query}追")));
        });
    }

    #[test]
    fn test_search_options_can_be_changed_before_search() {
        with_commit_list_state(&["FIX"], |state| {
            state.toggle_smart_case();
            input_search_query(state, "fix");
            apply_search(state);

            assert_eq!(
                matched(state),
                Some(("Match 1 of 1 (query: \"fix\")".into(), true))
            );
        });
    }

    #[test]
    fn test_search_option_string_after_toggles() {
        with_commit_list_state(&["fix"], |state| {
            state.toggle_smart_case();
            assert_eq!(
                state
                    .search_options()
                    .status_string(crate::search::SearchKind::Commits),
                "[all] [smart-case] [substring]"
            );
            state.toggle_smart_case();
            assert_eq!(
                state
                    .search_options()
                    .status_string(crate::search::SearchKind::Commits),
                "[all] [case-sensitive] [substring]"
            );
            state.toggle_fuzzy();
            assert_eq!(
                state
                    .search_options()
                    .status_string(crate::search::SearchKind::Commits),
                "[all] [case-sensitive] [fuzzy]"
            );
            state.toggle_fuzzy();
            assert_eq!(
                state
                    .search_options()
                    .status_string(crate::search::SearchKind::Commits),
                "[all] [case-sensitive] [substring]"
            );
            state.toggle_search_target();
            assert_eq!(
                state
                    .search_options()
                    .status_string(crate::search::SearchKind::Commits),
                "[subject] [case-sensitive] [substring]"
            );
        });
    }

    #[test]
    fn test_search_target_matches_only_the_selected_field() {
        let commit = Commit {
            commit_hash: CommitHash::from("abcdef0123456789abcdef0123456789abcdef01"),
            subject: "subject-match".into(),
            author_name: "author-match".into(),
            ..Commit::default()
        };
        let reference = Ref::Branch {
            name: "ref-match".into(),
            target: commit.commit_hash.clone(),
        };
        let stash = Ref::Stash {
            name: "stash@{0}".into(),
            message: "stash-match".into(),
            target: commit.commit_hash.clone(),
        };
        let tracking = FxHashMap::default();
        let info = CommitInfo::new(&commit, vec![&reference, &stash], &tracking, true);
        let cases = [
            (SearchTarget::All, "subject-match", true),
            (SearchTarget::All, "author-match", true),
            (SearchTarget::All, "ref-match", true),
            (SearchTarget::All, "abcdef0", true),
            (SearchTarget::All, "stash@", false),
            (SearchTarget::Subject, "subject-match", true),
            (SearchTarget::Subject, "author-match", false),
            (SearchTarget::Author, "author-match", true),
            (SearchTarget::Author, "ref-match", false),
            (SearchTarget::Ref, "ref-match", true),
            (SearchTarget::Ref, "abcdef0", false),
            (SearchTarget::Hash, "abcdef0", true),
            (SearchTarget::Hash, "subject-match", false),
        ];
        for (target, query, expected) in cases {
            let options = SearchOptions {
                target,
                ..Default::default()
            };
            assert_eq!(
                commit_matches(&info, query, options),
                expected,
                "{target:?}: {query}"
            );
        }
    }

    #[test]
    fn test_matches_highlight_whole_chars() {
        let theme = ColorTheme::default();
        let highlight = |text: &str, width: usize, query: &str, fuzzy: bool| {
            let options = SearchOptions {
                fuzzy,
                ..Default::default()
            };
            highlighted_cut(text, width, Style::default(), Some(query), options, &theme)
                .iter()
                .filter(|span| span.style.bg == Some(theme.list_match_bg))
                .map(|span| span.content.to_string())
                .collect::<String>()
        };
        // fuzzy, by chars, not bytes
        assert_eq!(highlight("Müller", 10, "ül", true), "ül");
        // cut with an ellipsis, the match within what is shown
        assert_eq!(highlight("Müller Straße", 6, "ller", false), "l");
        // an escape code after the cut doesn't move it off a char boundary
        assert_eq!(highlight("abcdé\x1b[mxyz", 5, "abc", false), "ab");
    }

    #[test]
    fn test_applied_search_target_toggle_recalculates_matches() {
        with_commit_list_state(&["fix", "other"], |state| {
            input_search_query(state, "fix");
            apply_search(state);

            state.toggle_search_target();
            assert_eq!(state.search_options().target, SearchTarget::Subject);
            assert_eq!(
                matched(state),
                Some(("Match 1 of 1 (query: \"fix\")".into(), true))
            );

            state.toggle_search_target();
            assert_eq!(state.search_options().target, SearchTarget::Author);
            assert_eq!(
                matched(state),
                Some(("No matches found (query: \"fix\")".into(), false))
            );
        });
    }

    #[test]
    fn test_applied_search_options_keep_selected_match() {
        with_commit_list_state(&["FIX", "fix"], |state| {
            input_search_query(state, "fix");
            apply_search(state);
            state.toggle_smart_case();

            assert_eq!(state.commits[state.cursor.selected()].commit.subject, "fix");
            assert_eq!(
                matched(state),
                Some(("Match 2 of 2 (query: \"fix\")".into(), true))
            );
        });
    }

    #[test]
    fn test_applied_search_options_select_next_match() {
        with_commit_list_state(&["FIX", "fix"], |state| {
            state.toggle_smart_case();
            input_search_query(state, "fix");
            apply_search(state);
            state.toggle_smart_case();

            assert_eq!(state.commits[state.cursor.selected()].commit.subject, "fix");
            assert_eq!(
                matched(state),
                Some(("Match 1 of 1 (query: \"fix\")".into(), true))
            );
        });
    }

    #[test]
    fn test_applied_search_options_keep_selection_when_no_matches() {
        with_commit_list_state(&["fix", "other"], |state| {
            state.toggle_fuzzy();
            input_search_query(state, "fx");
            apply_search(state);
            let selected = state.cursor.selected();

            state.toggle_fuzzy();

            assert_eq!(state.cursor.selected(), selected);
            assert_eq!(
                matched(state),
                Some(("No matches found (query: \"fx\")".into(), false))
            );
        });
    }

    #[test]
    fn test_calc_cell_widths_all_columns() {
        let area_width = 80;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Graph,
            UserListColumnType::Marker,
            UserListColumnType::Subject,
            UserListColumnType::Name,
            UserListColumnType::Hash,
            UserListColumnType::Date,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        let expected = vec![
            Constraint::Length(6),  // Graph
            Constraint::Length(1),  // Marker
            Constraint::Min(0),     // Subject
            Constraint::Length(12), // Name (10 + 2 pad)
            Constraint::Length(9),  // Hash (7 + 2 pad)
            Constraint::Length(17), // Date (15 + 2 pad)
        ];
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_calc_cell_width_all_columns_small_area_remove_name_date_hash() {
        let area_width = 30;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Graph,
            UserListColumnType::Marker,
            UserListColumnType::Subject,
            UserListColumnType::Name,
            UserListColumnType::Hash,
            UserListColumnType::Date,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        // Graph + Marker + Subject + Hash = 6 + 1 + 20 + 9 = 36 > 30
        // => Name, Date, and Hash are removed
        let expected = vec![
            Constraint::Length(6), // Graph
            Constraint::Length(1), // Marker
            Constraint::Min(0),    // Subject
            Constraint::Length(0), // Name removed
            Constraint::Length(0), // Hash removed
            Constraint::Length(0), // Date removed
        ];
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_calc_cell_width_all_columns_small_area_remove_name_date() {
        let area_width = 40;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Graph,
            UserListColumnType::Marker,
            UserListColumnType::Subject,
            UserListColumnType::Name,
            UserListColumnType::Hash,
            UserListColumnType::Date,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        // Graph + Marker + Subject + Hash = 6 + 1 + 20 + 9 = 36
        // Graph + Marker + Subject + Date + Hash = 6 + 1 + 20 + 17 + 9 = 53 > 40
        // => Name and Date are removed
        let expected = vec![
            Constraint::Length(6), // Graph
            Constraint::Length(1), // Marker
            Constraint::Min(0),    // Subject
            Constraint::Length(0), // Name removed
            Constraint::Length(9), // Hash (7 + 2 pad)
            Constraint::Length(0), // Date removed
        ];
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_calc_cell_width_all_columns_small_area_remove_name() {
        let area_width = 60;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Graph,
            UserListColumnType::Marker,
            UserListColumnType::Subject,
            UserListColumnType::Name,
            UserListColumnType::Hash,
            UserListColumnType::Date,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        // Graph + Marker + Subject + Date + Hash = 6 + 1 + 20 + 17 + 9 = 53 <= 60
        // Graph + Marker + Subject + Name + Date + Hash = 6 + 1 + 20 + 12 + 17 + 9 = 65 > 60
        // => Name is removed
        let expected = vec![
            Constraint::Length(6),  // Graph
            Constraint::Length(1),  // Marker
            Constraint::Min(0),     // Subject
            Constraint::Length(0),  // Name removed
            Constraint::Length(9),  // Hash (7 + 2 pad)
            Constraint::Length(17), // Date (15 + 2 pad)
        ];
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_calc_cell_width_columns_order() {
        let area_width = 80;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Date,
            UserListColumnType::Subject,
            UserListColumnType::Hash,
            UserListColumnType::Graph,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        let expected = vec![
            Constraint::Length(17), // Date (15 + 2 pad)
            Constraint::Min(0),     // Subject
            Constraint::Length(9),  // Hash (7 + 2 pad)
            Constraint::Length(6),  // Graph
        ];
        assert_eq!(actual, expected);
    }
}
