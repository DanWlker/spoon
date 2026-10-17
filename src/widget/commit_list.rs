use std::rc::Rc;

use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
use laurier::highlight::highlight_matched_text;
use ratatui::{
    buffer::Buffer,
    crossterm::event::{Event, KeyEvent},
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{List, ListItem, StatefulWidget, Widget},
    Frame,
};
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::LazyLock;
use tui_input::{backend::crossterm::EventHandler, Input, InputRequest};

use crate::{
    app::AppContext,
    color::ColorTheme,
    config::UserListColumnType,
    event::UserEvent,
    git::{Commit, CommitHash, Head, Ref},
    graph::GraphImageManager,
    protocol::PreparedImage,
    search::{case_insensitive, SearchOptions, SearchTarget},
    widget::{
        style::{
            mark_selected_bar, panel_block, panel_title, picked_style, render_panel_scrollbar,
            scroll_to,
        },
        tree_query::{find_around, SearchDirection},
    },
};

static FUZZY_MATCHER: LazyLock<SkimMatcherV2> =
    LazyLock::new(|| SkimMatcherV2::default().respect_case());

const ELLIPSIS: &str = "...";

#[derive(Debug)]
pub struct CommitInfo<'a> {
    commit: &'a Commit,
    refs: Vec<&'a Ref>,
    // on the history of HEAD; the other commits are dimmed
    on_head: bool,
}

impl<'a> CommitInfo<'a> {
    pub fn new(commit: &'a Commit, refs: Vec<&'a Ref>, on_head: bool) -> Self {
        Self {
            commit,
            refs,
            on_head,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchState {
    Inactive,
    Searching {
        start_index: usize,
        match_index: usize,
    },
    Applied {
        match_index: usize,
        total_match: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRefreshContext {
    query: String,
}

impl SearchState {
    fn update_match_index(&mut self, index: usize) {
        match self {
            SearchState::Searching { match_index, .. } => *match_index = index,
            SearchState::Applied { match_index, .. } => *match_index = index,
            _ => {}
        }
    }
}

#[derive(Debug, Default, Clone)]
struct SearchMatch {
    refs: FxHashMap<String, SearchMatchPosition>,
    subject: Option<SearchMatchPosition>,
    author_name: Option<SearchMatchPosition>,
    commit_hash: Option<SearchMatchPosition>,
    match_index: usize, // 1-based
}

impl SearchMatch {
    fn set(&mut self, c: &Commit, refs: &[&Ref], matcher: &SearchMatcher, target: SearchTarget) {
        self.refs = if matches!(target, SearchTarget::All | SearchTarget::Ref) {
            refs.iter()
                .filter(|r| !matches!(*r, Ref::Stash { .. }))
                .filter_map(|r| {
                    matcher
                        .matched_position(r.name())
                        .map(|pos| (r.name().into(), pos))
                })
                .collect()
        } else {
            FxHashMap::default()
        };
        self.subject = if matches!(target, SearchTarget::All | SearchTarget::Subject) {
            matcher.matched_position(&c.subject)
        } else {
            None
        };
        self.author_name = if matches!(target, SearchTarget::All | SearchTarget::Author) {
            matcher.matched_position(&c.author_name)
        } else {
            None
        };
        self.commit_hash = if matches!(target, SearchTarget::All | SearchTarget::Hash) {
            matcher.matched_position(c.commit_hash.as_short_hash())
        } else {
            None
        };
        self.match_index = 0;
    }

    fn matched(&self) -> bool {
        !self.refs.is_empty()
            || self.subject.is_some()
            || self.author_name.is_some()
            || self.commit_hash.is_some()
    }

    fn clear(&mut self) {
        self.refs.clear();
        self.subject = None;
        self.author_name = None;
        self.commit_hash = None;
    }
}

#[derive(Debug, Default, Clone)]
struct SearchMatchPosition {
    matched_indices: Vec<usize>,
}

impl SearchMatchPosition {
    fn new(matched_indices: Vec<usize>) -> Self {
        Self { matched_indices }
    }
}

struct SearchMatcher {
    query: String,
    // smart case with a lowercase query: both sides lowercased
    lowercase: bool,
    fuzzy: bool,
}

impl SearchMatcher {
    fn new(query: &str, smart_case: bool, fuzzy: bool) -> Self {
        let lowercase = case_insensitive(smart_case, query);
        let query = if lowercase {
            query.to_lowercase()
        } else {
            query.into()
        };
        Self {
            query,
            lowercase,
            fuzzy,
        }
    }

    fn matched_position(&self, s: &str) -> Option<SearchMatchPosition> {
        if self.fuzzy {
            let result = if self.lowercase {
                FUZZY_MATCHER.fuzzy_indices(&s.to_lowercase(), &self.query)
            } else {
                FUZZY_MATCHER.fuzzy_indices(s, &self.query)
            };
            // the matcher counts chars, the highlight bytes
            result
                .map(|(_, indices)| {
                    s.char_indices()
                        .enumerate()
                        .filter(|(i, _)| indices.binary_search(i).is_ok())
                        .flat_map(|(_, (b, c))| b..b + c.len_utf8())
                        .collect()
                })
                .map(SearchMatchPosition::new)
        } else {
            let result = if self.lowercase {
                s.to_lowercase().find(&self.query)
            } else {
                s.find(&self.query)
            };
            result
                .map(|p| (p..(p + self.query.len())).collect())
                .map(SearchMatchPosition::new)
        }
    }
}

#[derive(Debug)]
pub struct CommitListState<'a> {
    commits: Vec<CommitInfo<'a>>,
    commit_hash_set: FxHashSet<&'a CommitHash>,
    graph_image_manager: GraphImageManager<'a>,
    graph_cell_width: u16,

    ref_name_to_commit_index_map: FxHashMap<&'a str, usize>,

    search_state: SearchState,
    search_options: SearchOptions,
    search_input: Input,
    search_matches: Vec<SearchMatch>,

    // commits picked with space for commands on several commits, in the order they were picked
    picked_commit_hashes: Vec<CommitHash>,
    // what part of the history is loaded, like "main only", when not all of it
    filter_note: Option<String>,
    // the checked-out branch, its label in its own color; a detached HEAD gets a label
    head: Head,
    selected: usize,
    offset: usize,
    total: usize,
    height: usize,
    scrolloff: usize,
}

impl<'a> CommitListState<'a> {
    pub fn new(
        commits: Vec<CommitInfo<'a>>,
        graph_image_manager: GraphImageManager<'a>,
        graph_cell_width: u16,
        ref_name_to_commit_index_map: FxHashMap<&'a str, usize>,
        search_options: SearchOptions,
        scrolloff: usize,
    ) -> CommitListState<'a> {
        let total = commits.len();
        let commit_hash_set = commits.iter().map(|c| &c.commit.commit_hash).collect();
        CommitListState {
            commits,
            commit_hash_set,
            graph_image_manager,
            graph_cell_width,
            ref_name_to_commit_index_map,
            search_state: SearchState::Inactive,
            search_options,
            search_input: Input::default(),
            search_matches: vec![SearchMatch::default(); total],
            picked_commit_hashes: Vec::new(),
            filter_note: None,
            head: Head::None,
            selected: 0,
            offset: 0,
            total,
            height: 0,
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

    pub fn toggle_pick(&mut self) {
        if self.total == 0 {
            return;
        }
        let selected = self.selected_commit_hash().clone();
        if let Some(pos) = self
            .picked_commit_hashes
            .iter()
            .position(|h| *h == selected)
        {
            self.picked_commit_hashes.remove(pos);
        } else {
            self.picked_commit_hashes.push(selected);
        }
    }

    pub fn clear_picks(&mut self) {
        self.picked_commit_hashes.clear();
    }

    pub fn restore_picked_commit_hashes(&mut self, picked_commit_hashes: Vec<CommitHash>) {
        self.picked_commit_hashes = picked_commit_hashes
            .into_iter()
            .filter(|hash| self.commit_hash_set.contains(hash))
            .collect();
    }

    pub fn update_height(&mut self, height: usize) {
        self.height = height;

        if self.total > self.height && self.total - self.height < self.offset {
            let diff = self.offset - (self.total - self.height);
            self.selected += diff;
            self.offset -= diff;
        }
        if self.selected >= self.height {
            let diff = self.selected - self.height + 1;
            self.selected -= diff;
            self.offset += diff;
        }
    }

    pub fn ensure_visible_graph_uploaded(&mut self) {
        self.commits
            .iter()
            .skip(self.offset)
            .take(self.height)
            .for_each(|commit_info| {
                self.graph_image_manager
                    .ensure_uploaded(&commit_info.commit.commit_hash);
            });
    }

    pub fn drain_pending_graph_uploads(&mut self) -> Vec<String> {
        self.graph_image_manager.drain_pending_uploads()
    }

    pub fn graph_image_ids_sorted(&self) -> Vec<u32> {
        let mut image_ids: Vec<u32> = self
            .graph_image_manager
            .image_ids()
            .iter()
            .copied()
            .collect();
        image_ids.sort_unstable();
        image_ids
    }

    pub fn select_next(&mut self) {
        if self.total > 0 {
            self.select_visible_index((self.current_selected_index() + 1).min(self.total - 1));
        }
    }

    pub fn select_prev(&mut self) {
        self.select_visible_index(self.current_selected_index().saturating_sub(1));
    }

    pub fn select_first(&mut self) {
        self.selected = 0;
        self.offset = 0;
    }

    pub fn select_last(&mut self) {
        // the last rows fill the list, from the top when they all fit
        let height = self.height.max(1);
        self.selected = (height - 1).min(self.total - 1);
        self.offset = self.total.saturating_sub(height);
    }

    pub fn scroll_down(&mut self) {
        if self.offset + self.height < self.total {
            self.offset += 1;
            if self.selected > self.effective_scrolloff() {
                self.selected -= 1;
            }
        }
    }

    pub fn scroll_up(&mut self) {
        if self.offset > 0 {
            self.offset -= 1;
            if self.selected < self.height.saturating_sub(1 + self.effective_scrolloff()) {
                self.selected += 1;
            }
        }
    }

    pub fn scroll_down_page(&mut self) {
        self.scroll_down_height(self.height);
    }

    pub fn scroll_up_page(&mut self) {
        self.scroll_up_height(self.height);
    }

    pub fn scroll_down_half(&mut self) {
        self.scroll_down_height(self.height / 2);
    }

    pub fn scroll_up_half(&mut self) {
        self.scroll_up_height(self.height / 2);
    }

    fn scroll_down_height(&mut self, scroll_height: usize) {
        if self.height == 0 || self.total == 0 {
            return;
        }
        if self.offset + self.height + scroll_height < self.total {
            self.offset += scroll_height;
        } else {
            let old_offset = self.offset;
            let size = self.height.min(self.total);
            self.offset = self.total - size;
            self.selected += scroll_height - (self.offset - old_offset);
            if self.selected >= size {
                self.selected = size - 1;
            }
        }
        // At the final page, keep the viewport fixed so further page scrolls
        // can move the selection to the last commit as before.
        if self.offset < self.total.saturating_sub(self.height) {
            self.select_visible_index(self.current_selected_index());
        }
    }

    fn scroll_up_height(&mut self, scroll_height: usize) {
        if self.height == 0 || self.total == 0 {
            return;
        }
        if self.offset > scroll_height {
            self.offset -= scroll_height;
        } else {
            let old_offset = self.offset;
            self.offset = 0;
            self.selected = self
                .selected
                .saturating_sub(scroll_height - (old_offset - self.offset));
        }
        if self.offset > 0 {
            self.select_visible_index(self.current_selected_index());
        }
    }

    fn select_index(&mut self, index: usize) {
        if index < self.total {
            self.offset = index
                .saturating_sub(self.effective_scrolloff())
                .min(self.total.saturating_sub(self.height));
            self.selected = index - self.offset;
        }
    }

    fn effective_scrolloff(&self) -> usize {
        self.scrolloff.min(self.height.saturating_sub(1) / 2)
    }

    fn select_visible_index(&mut self, index: usize) {
        if self.height == 0 {
            self.select_index(index);
            return;
        }

        self.offset = scroll_to(self.offset, index, self.height, self.total, self.scrolloff);
        self.selected = index - self.offset;
    }

    // Selects the next (or previous) picked commit, wrapping around.
    pub fn select_next_pick(&mut self, forward: bool) {
        let direction = if forward {
            SearchDirection::Next
        } else {
            SearchDirection::Previous
        };
        let current = self.current_selected_index();
        let target = find_around(self.commits.len(), current, direction, |i| {
            self.is_picked(&self.commits[i].commit.commit_hash)
        });
        if let Some(index) = target {
            self.select_visible_index(index);
        }
    }

    pub fn select_next_match(&mut self) {
        self.select_next_match_index(self.current_selected_index());
    }

    pub fn select_prev_match(&mut self) {
        self.select_prev_match_index(self.current_selected_index());
    }

    pub fn selected_commit_hash(&self) -> &CommitHash {
        &self.commits[self.current_selected_index()]
            .commit
            .commit_hash
    }

    fn current_selected_index(&self) -> usize {
        self.offset + self.selected
    }

    // The selected commit, counting from 1, and how many there are.
    pub fn position(&self) -> (usize, usize) {
        (self.current_selected_index() + 1, self.total)
    }

    pub fn current_list_status(&self) -> (usize, usize, usize) {
        (self.selected, self.offset, self.height)
    }

    pub fn reset_height(&mut self, height: usize) {
        self.height = height;
    }

    pub fn restore_selected_row(&mut self, row: usize) {
        let index = self.current_selected_index();
        self.offset = index
            .saturating_sub(row.min(self.height.saturating_sub(1)))
            .min(self.total.saturating_sub(self.height));
        self.selected = index - self.offset;
    }

    pub fn set_filter_note(&mut self, note: Option<String>) {
        self.filter_note = note;
    }

    pub fn set_head(&mut self, head: Head) {
        self.head = head;
    }

    pub fn select_ref(&mut self, ref_name: &str) {
        if let Some(&index) = self.ref_name_to_commit_index_map.get(ref_name) {
            self.select_index(index);
        }
    }

    pub fn select_commit_hash(&mut self, commit_hash: &CommitHash) {
        if !self.commit_hash_set.contains(commit_hash) {
            return;
        }
        if let Some(index) = self
            .commits
            .iter()
            .position(|commit_info| commit_info.commit.commit_hash == *commit_hash)
        {
            self.select_index(index);
        }
    }

    pub fn search_state(&self) -> SearchState {
        self.search_state
    }

    pub fn search_options(&self) -> SearchOptions {
        self.search_options
    }

    pub fn restore_search_options(&mut self, options: SearchOptions) {
        self.search_options = options;
    }

    pub fn start_search(&mut self) {
        let match_index = match self.search_state {
            SearchState::Inactive => {
                self.search_input.reset();
                self.clear_search_matches();
                0
            }
            SearchState::Applied { match_index, .. } => {
                self.search_input.handle(InputRequest::GoToEnd);
                match_index
            }
            SearchState::Searching { .. } => return,
        };
        self.search_state = SearchState::Searching {
            start_index: self.current_selected_index(),
            match_index,
        };
    }

    pub fn handle_search_input(&mut self, key: KeyEvent) {
        if let SearchState::Searching { start_index, .. } = self.search_state {
            self.search_input.handle_event(&Event::Key(key));
            self.update_search_matches();
            self.select_current_or_next_match_index(start_index);
        }
    }

    pub fn apply_search(&mut self) {
        if let SearchState::Searching { match_index, .. } = self.search_state {
            if self.search_input.value().is_empty() {
                self.cancel_search();
            } else {
                let total_match = self.search_matches.iter().filter(|m| m.matched()).count();
                self.search_state = SearchState::Applied {
                    match_index,
                    total_match,
                };
            }
        }
    }

    pub fn search_refresh_context(&self) -> Option<SearchRefreshContext> {
        if let SearchState::Applied { .. } = self.search_state {
            Some(SearchRefreshContext {
                query: self.search_input.value().into(),
            })
        } else {
            None
        }
    }

    pub fn restore_search(&mut self, context: &SearchRefreshContext) {
        self.search_input = Input::new(context.query.clone());
        self.update_search_matches();

        let total_match = self.search_matches.iter().filter(|m| m.matched()).count();
        self.search_state = SearchState::Applied {
            // The selected commit may not match after refresh; next/previous updates this value.
            match_index: 0,
            total_match,
        };

        if total_match > 0 {
            let current_index = self.current_selected_index();
            if self.search_matches[current_index].matched() {
                self.search_state
                    .update_match_index(self.search_matches[current_index].match_index);
            }
        }
    }

    pub fn cancel_search(&mut self) {
        if let SearchState::Searching { .. } | SearchState::Applied { .. } = self.search_state {
            self.search_state = SearchState::Inactive;
            self.search_input.reset();
            self.clear_search_matches();
        }
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
            UserEvent::HalfPageDown => self.scroll_down_half(),
            UserEvent::HalfPageUp => self.scroll_up_half(),
            _ => return false,
        }
        true
    }

    pub fn toggle_smart_case(&mut self) {
        self.search_options.smart_case = !self.search_options.smart_case;
        self.update_search_after_options_change();
    }

    pub fn toggle_fuzzy(&mut self) {
        self.search_options.fuzzy = !self.search_options.fuzzy;
        self.update_search_after_options_change();
    }

    pub fn toggle_search_target(&mut self) {
        self.search_options.target = self.search_options.target.next();
        self.update_search_after_options_change();
    }

    pub fn search_query_string(&self) -> Option<String> {
        if let SearchState::Searching { .. } = self.search_state {
            let query = self.search_input.value();
            Some(format!("/{query}"))
        } else {
            None
        }
    }

    pub fn matched_query_string(&self) -> Option<(String, bool)> {
        if let SearchState::Applied {
            match_index,
            total_match,
            ..
        } = self.search_state
        {
            let query = self.search_input.value();
            if total_match == 0 {
                let msg = format!("No matches found (query: \"{query}\")");
                Some((msg, false))
            } else {
                let msg = format!("Match {match_index} of {total_match} (query: \"{query}\")");
                Some((msg, true))
            }
        } else {
            None
        }
    }

    pub fn search_query_cursor_position(&self) -> u16 {
        self.search_input.visual_cursor() as u16 + 1 // add 1 for "/"
    }

    fn update_search_matches(&mut self) {
        if self.search_input.value().is_empty() {
            self.clear_search_matches();
            self.search_state.update_match_index(0);
            return;
        }
        let matcher = SearchMatcher::new(
            self.search_input.value(),
            self.search_options.smart_case,
            self.search_options.fuzzy,
        );
        let mut match_index = 1;
        for (i, commit_info) in self.commits.iter().enumerate() {
            let m = &mut self.search_matches[i];
            m.set(
                commit_info.commit,
                commit_info.refs.as_slice(),
                &matcher,
                self.search_options.target,
            );
            if m.matched() {
                m.match_index = match_index;
                match_index += 1;
            }
        }
    }

    fn update_search_after_options_change(&mut self) {
        match self.search_state {
            SearchState::Inactive => {}
            SearchState::Searching { start_index, .. } => {
                self.update_search_matches();
                self.select_current_or_next_match_index(start_index);
            }
            SearchState::Applied { .. } => {
                let current_index = self.current_selected_index();
                self.update_search_matches();
                let total_match = self.search_matches.iter().filter(|m| m.matched()).count();
                self.search_state = SearchState::Applied {
                    match_index: 0,
                    total_match,
                };
                if total_match > 0 {
                    self.select_current_or_next_match_index(current_index);
                }
            }
        }
    }

    fn clear_search_matches(&mut self) {
        self.search_matches.iter_mut().for_each(|m| m.clear());
    }

    fn select_current_or_next_match_index(&mut self, current_index: usize) {
        if self.search_matches[current_index].matched() {
            self.select_index(current_index);
            self.search_state
                .update_match_index(self.search_matches[current_index].match_index);
        } else {
            self.select_next_match_index(current_index)
        }
    }

    fn select_next_match_index(&mut self, current_index: usize) {
        let mut i = (current_index + 1) % self.total;
        while i != current_index {
            if self.search_matches[i].matched() {
                self.select_index(i);
                self.search_state
                    .update_match_index(self.search_matches[i].match_index);
                return;
            }
            if i == self.total - 1 {
                i = 0;
            } else {
                i += 1;
            }
        }
    }

    fn select_prev_match_index(&mut self, current_index: usize) {
        let mut i = (current_index + self.total - 1) % self.total;
        while i != current_index {
            if self.search_matches[i].matched() {
                self.select_index(i);
                self.search_state
                    .update_match_index(self.search_matches[i].match_index);
                return;
            }
            if i == 0 {
                i = self.total - 1;
            } else {
                i -= 1;
            }
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
    let mut notes: Vec<String> = state.filter_note.iter().cloned().collect();
    match state.picked_commit_hashes().len() {
        0 => {}
        n => notes.push(format!("{n} picked")),
    }
    let block = panel_block(focused, theme).title(panel_title(
        "[1] Commits",
        &notes.join(" · "),
        focused,
        theme,
    ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_stateful_widget(CommitList::new(ctx.clone()), inner, state);
    // the bar on the border, the graph starts right after it
    let y = inner.y + state.selected as u16;
    if inner.x > 0 && y < inner.bottom() && !state.commits.is_empty() {
        mark_selected_bar(f.buffer_mut(), inner.x - 1, y, focused, &ctx.color_theme);
    }
    let theme = &ctx.color_theme;
    render_panel_scrollbar(
        f.buffer_mut(),
        area,
        state.commits.len(),
        state.offset,
        focused,
        theme,
    );
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
                    &state.search_matches[state.offset + i].refs,
                    &state.head,
                    &self.ctx.color_theme,
                );
                let ref_spans_width: usize = spans.iter().map(|s| s.width()).sum();
                let max_width = max_width.saturating_sub(ref_spans_width);
                let commit = commit_info.commit;
                let subject_fg = self.text_fg(commit_info, self.ctx.color_theme.list_subject_fg);
                if max_width > ELLIPSIS.len() {
                    let truncate = console::measure_text_width(&commit.subject) > max_width;
                    let subject = if truncate {
                        console::truncate_str(&commit.subject, max_width, ELLIPSIS).to_string()
                    } else {
                        commit.subject.to_string()
                    };

                    let sub_spans =
                        if let Some(pos) = state.search_matches[state.offset + i].subject.clone() {
                            highlighted_spans(
                                subject.into(),
                                pos,
                                subject_fg,
                                Modifier::empty(),
                                &self.ctx.color_theme,
                                truncate,
                            )
                        } else {
                            vec![subject.fg(subject_fg)]
                        };

                    spans.extend(sub_spans)
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
                let truncate = console::measure_text_width(&commit.author_name) > max_width;
                let name = if truncate {
                    console::truncate_str(&commit.author_name, max_width, ELLIPSIS).to_string()
                } else {
                    commit.author_name.to_string()
                };
                let spans =
                    if let Some(pos) = state.search_matches[state.offset + i].author_name.clone() {
                        highlighted_spans(
                            name.into(),
                            pos,
                            name_fg,
                            Modifier::empty(),
                            &self.ctx.color_theme,
                            truncate,
                        )
                    } else {
                        vec![name.fg(name_fg)]
                    };
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
                let spans =
                    if let Some(pos) = state.search_matches[state.offset + i].commit_hash.clone() {
                        highlighted_spans(
                            hash.into(),
                            pos,
                            hash_fg,
                            Modifier::empty(),
                            &self.ctx.color_theme,
                            false,
                        )
                    } else {
                        vec![hash.fg(hash_fg)]
                    };
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
                let date = &commit_info.commit.author_date;
                let date_str = if self.ctx.ui_config.list.date_local {
                    let local = date.with_timezone(&chrono::Local);
                    local
                        .format(&self.ctx.ui_config.list.date_format)
                        .to_string()
                } else {
                    date.format(&self.ctx.ui_config.list.date_format)
                        .to_string()
                };
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
            .skip(state.offset)
            .take(state.height)
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
        if i == state.selected {
            line = line.bg(self.ctx.color_theme.list_selected_bg);
        } else if state.is_picked(&state.commits[state.offset + i].commit.commit_hash) {
            // spans keep their own colors over the line's, so each one takes the picked style
            let picked = picked_style(&self.ctx.color_theme);
            line.spans = line
                .spans
                .into_iter()
                .map(|span| span.patch_style(picked))
                .collect();
            line = line.patch_style(picked);
        }
        ListItem::new(line)
    }
}

// The refs at the commit as labels tinted with their colors: branches, remote branches, tags and
// stashes.
fn refs_spans<'a>(
    commit_info: &'a CommitInfo,
    refs_matches: &'a FxHashMap<String, SearchMatchPosition>,
    head: &Head,
    color_theme: &'a ColorTheme,
) -> Vec<Span<'a>> {
    let mut spans = vec![];
    if matches!(head, Head::Detached { target } if *target == commit_info.commit.commit_hash) {
        spans.push(Span::styled(
            " HEAD ",
            ref_label_style(color_theme.list_ref_head_fg),
        ));
        spans.push(Span::raw(" "));
    }
    let head_branch = match head {
        Head::Branch { name } => Some(name.as_str()),
        _ => None,
    };
    for r in &commit_info.refs {
        let name = r.name();
        let style = ref_label_style(ref_color(r, head_branch, color_theme));
        let fg = style.fg.unwrap_or_default();
        let bg = style.bg.unwrap_or_default();
        let label = format!(" {name} ");
        let label_spans = match refs_matches.get(name) {
            // the match positions are of the name, the label has a space before it
            Some(pos) => {
                let mut pos = pos.clone();
                pos.matched_indices = pos.matched_indices.iter().map(|i| i + 1).collect();
                highlighted_spans(label.into(), pos, fg, Modifier::BOLD, color_theme, false)
                    .into_iter()
                    .map(|span| {
                        if span.style.bg.is_none() {
                            span.bg(bg)
                        } else {
                            span
                        }
                    })
                    .collect()
            }
            None => vec![Span::styled(label, style)],
        };
        spans.extend(label_spans);
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

fn highlighted_spans(
    s: Span<'_>,
    pos: SearchMatchPosition,
    base_fg: Color,
    base_modifier: Modifier,
    color_theme: &ColorTheme,
    truncate: bool,
) -> Vec<Span<'static>> {
    // whole chars only: lowercasing for the search, or truncating, can move the bytes of a match
    let text = &s.content;
    let indices = pos
        .matched_indices
        .into_iter()
        .filter(|&i| i < text.len())
        .flat_map(|i| {
            let start = (0..=i)
                .rev()
                .find(|&b| text.is_char_boundary(b))
                .unwrap_or(0);
            let len = text[start..].chars().next().map_or(1, char::len_utf8);
            start..start + len
        })
        .collect();
    let mut hm = highlight_matched_text(vec![s])
        .matched_indices(indices)
        .not_matched_style(Style::default().fg(base_fg).add_modifier(base_modifier))
        .matched_style(
            Style::default()
                .fg(color_theme.list_match_fg)
                .bg(color_theme.list_match_bg)
                .add_modifier(base_modifier),
        );
    if truncate {
        hm = hm.ellipsis(ELLIPSIS);
    }
    hm.into_spans()
}

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

    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

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
        let commit_hashes = commits.iter().map(|c| c.commit_hash.clone()).collect();
        let commit_map = commits
            .into_iter()
            .map(|c| (c.commit_hash.clone(), c))
            .collect();
        let repository = Repository::new(
            PathBuf::new(),
            commit_map,
            FxHashMap::default(),
            FxHashMap::default(),
            FxHashMap::default(),
            Head::None,
            commit_hashes,
        );
        let graph = calc_graph(&repository, None);
        let graph_color_set = GraphColorSet::new(&GraphColorConfig::default());
        let image_params =
            crate::graph::ImageParams::new(&graph_color_set, CellWidthType::Double, false);
        let graph_image_manager = GraphImageManager::new(
            &graph,
            image_params,
            CellWidthType::Double,
            GraphStyle::Rounded,
            GraphImageWidthMode::Compact,
            image_protocol,
        );
        let commit_infos = graph
            .commits
            .iter()
            .map(|commit| CommitInfo::new(commit, repository.refs(&commit.commit_hash), true))
            .collect();
        let mut state = CommitListState::new(
            commit_infos,
            graph_image_manager,
            0,
            FxHashMap::default(),
            SearchOptions::default(),
            0,
        );
        state.reset_height(subjects.len());
        f(&mut state)
    }

    fn input_search_query(state: &mut CommitListState<'_>, query: &str) {
        state.start_search();
        for c in query.chars() {
            state.handle_search_input(KeyEvent::from(KeyCode::Char(c)));
        }
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
                state.toggle_pick();
                state.select_next();
                let buf = draw(state);
                let text = Rect::new(x, y, row.right() - x, 1);
                assert_row_bg(&buf, text, theme.list_picked_bg);
                state.select_prev();
                state.toggle_pick();
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
                    running_task: Default::default(),
                    key_names: Default::default(),
                    layout: Default::default(),
                    wrap: Default::default(),
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
    fn test_pick_toggles_commits_in_picking_order() {
        with_commit_list_state(&["first", "second", "third"], |state| {
            let first = state.selected_commit_hash().clone();
            state.toggle_pick();
            state.select_last();
            let third = state.selected_commit_hash().clone();
            let status = state.current_list_status();
            state.toggle_pick();
            assert_eq!(state.picked_commit_hashes(), [first.clone(), third.clone()]);
            assert_eq!(state.current_list_status(), status);

            state.select_first();
            state.toggle_pick();
            assert_eq!(state.picked_commit_hashes(), [third]);

            state.clear_picks();
            assert!(state.picked_commit_hashes().is_empty());
        });
    }

    #[test]
    fn test_select_next_pick_wraps_around() {
        with_commit_list_state(&["c0", "c1", "c2", "c3", "c4"], |state| {
            state.reset_height(5);
            for index in [1, 3] {
                state.select_index(index);
                state.toggle_pick();
            }
            state.select_index(0);
            state.select_next_pick(true);
            assert_eq!(state.current_selected_index(), 1);
            state.select_next_pick(true);
            assert_eq!(state.current_selected_index(), 3);
            state.select_next_pick(true);
            assert_eq!(state.current_selected_index(), 1);
            state.select_next_pick(false);
            assert_eq!(state.current_selected_index(), 3);
        });
    }

    #[test]
    fn test_pick_refresh_preserves_loaded_hashes_and_drops_missing_hashes() {
        let context = with_commit_list_state(&["first", "second"], |state| {
            state.toggle_pick();
            state.select_next();
            state.toggle_pick();
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
                running_task: Default::default(),
                key_names: Default::default(),
                layout: Default::default(),
                wrap: Default::default(),
                search_options: Default::default(),
            });
            let area = Rect::new(0, 0, 80, 3);
            let mut before = Buffer::empty(area);
            CommitList::new(ctx.clone()).render(area, &mut before, state);

            // pick the first commit, then move the cursor away from it
            state.toggle_pick();
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
        });
    }

    #[test]
    fn test_scrolloff_keeps_context_for_selection_and_single_row_scrolling() {
        with_commit_list_state(&["commit"; 12], |state| {
            state.reset_height(6);
            state.scrolloff = 2;

            for _ in 0..4 {
                state.select_next();
            }
            assert_eq!(state.current_list_status(), (3, 1, 6));

            state.select_prev();
            state.select_prev();
            assert_eq!(state.current_list_status(), (2, 0, 6));

            state.select_index(7);
            assert_eq!(state.current_list_status(), (2, 5, 6));
            state.scroll_down();
            assert_eq!(state.current_list_status(), (2, 6, 6));
            state.scroll_up();
            assert_eq!(state.current_list_status(), (3, 5, 6));

            state.select_last();
            assert_eq!(state.current_list_status(), (5, 6, 6));
            state.select_prev();
            assert_eq!(state.current_list_status(), (4, 6, 6));
        });
    }

    #[test]
    fn test_scrolloff_is_limited_by_list_height() {
        with_commit_list_state(&["commit"; 8], |state| {
            state.reset_height(2);
            state.scrolloff = 10;

            state.select_next();
            state.select_next();
            assert_eq!(state.current_list_status(), (1, 1, 2));

            state.select_index(6);
            state.restore_selected_row(1);
            assert_eq!(state.current_list_status(), (1, 5, 2));
        });
    }

    #[test]
    fn test_page_scrolloff_keeps_context_until_final_page() {
        with_commit_list_state(&["commit"; 30], |state| {
            state.reset_height(10);
            state.scrolloff = 2;

            state.scroll_down_page();
            assert_eq!(state.current_list_status(), (2, 8, 10));
            state.scroll_down_page();
            assert_eq!(state.current_list_status(), (2, 18, 10));
            state.scroll_down_page();
            assert_eq!(state.current_list_status(), (9, 20, 10));
            state.scroll_down_page();
            assert_eq!(state.current_list_status(), (9, 20, 10));

            state.scroll_up_page();
            assert_eq!(state.current_list_status(), (7, 12, 10));
            state.scroll_up_page();
            assert_eq!(state.current_list_status(), (7, 2, 10));
            state.scroll_up_page();
            assert_eq!(state.current_list_status(), (0, 0, 10));
        });
    }

    #[test]
    fn test_half_page_scroll_reaches_last_commit_after_final_page() {
        with_commit_list_state(&["commit"; 26], |state| {
            state.reset_height(10);
            state.scrolloff = 2;
            state.select_index(12);

            state.scroll_down_half();
            assert_eq!(state.current_list_status(), (2, 15, 10));
            state.scroll_down_half();
            assert_eq!(state.current_list_status(), (6, 16, 10));
            state.scroll_down_half();
            assert_eq!(state.current_list_status(), (9, 16, 10));
        });
    }

    #[test]
    fn test_full_page_scroll_reaches_last_commit_after_final_page() {
        with_commit_list_state(&["commit"; 25], |state| {
            state.reset_height(10);
            state.scrolloff = 2;
            state.select_index(12);

            state.scroll_down_page();
            assert_eq!(state.current_list_status(), (7, 15, 10));
            state.scroll_down_page();
            assert_eq!(state.current_list_status(), (9, 15, 10));
        });
    }

    #[test]
    fn test_refresh_restores_selected_row_with_scrolloff() {
        with_commit_list_state(&["commit"; 30], |state| {
            state.reset_height(10);
            state.scrolloff = 2;
            state.select_index(15);
            assert_eq!(state.current_list_status(), (2, 13, 10));

            state.restore_selected_row(7);
            assert_eq!(state.current_list_status(), (7, 8, 10));
            assert_eq!(state.current_selected_index(), 15);
        });
    }

    #[test]
    fn test_zero_scrolloff_preserves_page_scroll_behavior() {
        with_commit_list_state(&["commit"; 25], |state| {
            state.reset_height(10);
            state.select_index(12);
            assert_eq!(state.current_list_status(), (0, 12, 10));

            state.scroll_down_page();
            assert_eq!(state.current_list_status(), (7, 15, 10));
            state.scroll_down_page();
            assert_eq!(state.current_list_status(), (9, 15, 10));
        });
    }

    #[test]
    fn test_restore_search_recalculates_matches_with_applied_options() {
        let (context, options) = with_commit_list_state(&["Fix parser", "other"], |state| {
            input_search_query(state, "fx");
            state.toggle_smart_case();
            state.toggle_fuzzy();
            state.apply_search();

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
                state.commits[state.current_selected_index()].commit.subject,
                "unrelated"
            );

            state.select_next_match();
            assert_eq!(
                state.matched_query_string(),
                Some(("Match 1 of 2 (query: \"fx\")".into(), true))
            );
            assert_eq!(
                state.commits[state.current_selected_index()].commit.subject,
                "FIX new"
            );

            state.select_next_match();
            assert_eq!(
                state.matched_query_string(),
                Some(("Match 2 of 2 (query: \"fx\")".into(), true))
            );
            assert_eq!(
                state.commits[state.current_selected_index()].commit.subject,
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
            state.apply_search();

            assert_eq!(
                state.matched_query_string(),
                Some(("Match 1 of 1 (query: \"fix\")".into(), true))
            );
        });
    }

    #[test]
    fn test_restore_search_keeps_selected_match_position() {
        let context = with_commit_list_state(&["fix"], |state| {
            input_search_query(state, "fix");
            state.apply_search();
            state.search_refresh_context().unwrap()
        });

        with_commit_list_state(&["first", "second", "fix", "last"], |state| {
            state.reset_height(2);
            state.select_index(2);
            state.scroll_up();
            assert_eq!(state.current_list_status(), (1, 1, 2));

            state.restore_search(&context);

            assert_eq!(state.current_list_status(), (1, 1, 2));
            assert_eq!(
                state.matched_query_string(),
                Some(("Match 1 of 1 (query: \"fix\")".into(), true))
            );
        });
    }

    #[test]
    fn test_search_options_are_reused_for_next_search() {
        with_commit_list_state(&["FIX"], |state| {
            input_search_query(state, "fix");
            state.toggle_smart_case();
            state.apply_search();
            state.cancel_search();

            input_search_query(state, "fix");
            state.apply_search();

            assert_eq!(
                state.matched_query_string(),
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
            state.handle_search_input(KeyEvent::from(KeyCode::Left));
            state.apply_search();
            state.select_next_match();
            state.scroll_up();

            let options = state.search_options();
            let position = state.current_list_status();
            let result = state.matched_query_string();
            let matches: Vec<_> = state
                .search_matches
                .iter()
                .map(|m| m.subject.as_ref().map(|p| p.matched_indices.clone()))
                .collect();
            assert_eq!(result, Some(("Match 2 of 2 (query: \"fx\")".into(), true)));

            state.start_search();

            assert_eq!(state.search_query_string(), Some("/fx".into()));
            assert_eq!(state.search_query_cursor_position(), 3);
            assert_eq!(state.search_options(), options);
            assert_eq!(state.current_list_status(), position);
            assert_eq!(
                state
                    .search_matches
                    .iter()
                    .map(|m| m.subject.as_ref().map(|p| p.matched_indices.clone()))
                    .collect::<Vec<_>>(),
                matches
            );

            state.apply_search();
            assert_eq!(state.matched_query_string(), result);
            assert_eq!(state.current_list_status(), position);
        });
    }

    #[test]
    fn test_resume_search_edits_from_current_selection() {
        with_commit_list_state(
            &["fix parser older", "fix graph", "fix parser newer"],
            |state| {
                input_search_query(state, "fix");
                state.apply_search();
                state.select_next_match();
                assert_eq!(state.current_selected_index(), 1);

                state.start_search();
                for c in " parser".chars() {
                    state.handle_search_input(KeyEvent::from(KeyCode::Char(c)));
                }
                assert_eq!(state.current_selected_index(), 2);
                assert!(!state.search_matches[1].matched());
                assert!(state.search_matches[2].matched());

                state.handle_search_input(KeyEvent::from(KeyCode::Backspace));
                assert_eq!(state.search_query_string(), Some("/fix parse".into()));
                state.apply_search();
                assert_eq!(
                    state.matched_query_string(),
                    Some(("Match 2 of 2 (query: \"fix parse\")".into(), true))
                );
            },
        );
    }

    #[test]
    fn test_resume_search_does_not_move_selection_off_an_unmatched_commit() {
        with_commit_list_state(&["fix", "other", "fix again"], |state| {
            input_search_query(state, "fix");
            state.apply_search();
            state.select_next();
            let position = state.current_list_status();
            let result = state.matched_query_string();

            state.start_search();
            state.apply_search();

            assert_eq!(state.current_list_status(), position);
            assert_eq!(state.current_selected_index(), 1);
            assert_eq!(state.matched_query_string(), result);
        });
    }

    #[test]
    fn test_resume_search_without_matches_can_be_corrected() {
        with_commit_list_state(&["fix", "other"], |state| {
            input_search_query(state, "fixx");
            state.apply_search();
            let result = state.matched_query_string();

            state.start_search();
            assert_eq!(state.search_query_string(), Some("/fixx".into()));
            state.apply_search();
            assert_eq!(state.matched_query_string(), result);

            state.start_search();
            state.handle_search_input(KeyEvent::from(KeyCode::Backspace));
            state.apply_search();
            assert_eq!(
                state.matched_query_string(),
                Some(("Match 1 of 1 (query: \"fix\")".into(), true))
            );
        });
    }

    #[test]
    fn test_cancel_resumed_search_starts_next_search_empty() {
        with_commit_list_state(&["fix", "other"], |state| {
            input_search_query(state, "fix");
            state.apply_search();
            state.start_search();
            state.cancel_search();

            assert_eq!(state.search_state(), SearchState::Inactive);
            assert!(state.search_matches.iter().all(|m| !m.matched()));
            state.start_search();
            assert_eq!(state.search_query_string(), Some("/".into()));
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
                state.apply_search();
                state.start_search();
                state.handle_search_input(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));

                assert_eq!(state.search_query_string(), Some("/".into()));
                assert!(state.search_matches.iter().all(|m| !m.matched()));
                state.apply_search();
                assert_eq!(state.search_state(), SearchState::Inactive);
                assert_eq!(state.matched_query_string(), None);

                state.start_search();
                assert_eq!(state.search_query_string(), Some("/".into()));
                state.handle_search_input(KeyEvent::from(KeyCode::Char('o')));
                state.apply_search();
                assert_eq!(
                    state.matched_query_string(),
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
            state.handle_search_input(KeyEvent::from(KeyCode::Home));
            state.apply_search();

            state.start_search();
            assert_eq!(state.search_query_string(), Some(format!("/{query}")));
            assert_eq!(state.search_query_cursor_position(), 161);
            state.handle_search_input(KeyEvent::from(KeyCode::Char('追')));
            assert_eq!(state.search_query_string(), Some(format!("/{query}追")));
        });
    }

    #[test]
    fn test_search_options_can_be_changed_before_search() {
        with_commit_list_state(&["FIX"], |state| {
            state.toggle_smart_case();
            input_search_query(state, "fix");
            state.apply_search();

            assert_eq!(
                state.matched_query_string(),
                Some(("Match 1 of 1 (query: \"fix\")".into(), true))
            );
        });
    }

    #[test]
    fn test_search_option_string_after_toggles() {
        with_commit_list_state(&["fix"], |state| {
            state.toggle_smart_case();
            assert_eq!(
                state.search_options().status_string(),
                "[all] [smart-case] [substring]"
            );
            state.toggle_smart_case();
            assert_eq!(
                state.search_options().status_string(),
                "[all] [case-sensitive] [substring]"
            );
            state.toggle_fuzzy();
            assert_eq!(
                state.search_options().status_string(),
                "[all] [case-sensitive] [fuzzy]"
            );
            state.toggle_fuzzy();
            assert_eq!(
                state.search_options().status_string(),
                "[all] [case-sensitive] [substring]"
            );
            state.toggle_search_target();
            assert_eq!(
                state.search_options().status_string(),
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
        let refs = [&reference];
        let cases = [
            (SearchTarget::All, "subject-match", true),
            (SearchTarget::All, "author-match", true),
            (SearchTarget::All, "ref-match", true),
            (SearchTarget::All, "abcdef0", true),
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
            let matcher = SearchMatcher::new(query, false, false);
            let mut search_match = SearchMatch::default();
            search_match.set(&commit, &refs, &matcher, target);
            assert_eq!(search_match.matched(), expected, "{target:?}: {query}");
        }
    }

    #[test]
    fn test_matches_highlight_whole_chars() {
        let theme = ColorTheme::default();
        let highlight = |text: &str, indices: Vec<usize>, truncate: bool| {
            let pos = SearchMatchPosition::new(indices);
            highlighted_spans(
                text.into(),
                pos,
                Color::Reset,
                Modifier::empty(),
                &theme,
                truncate,
            )
            .iter()
            .filter(|span| span.style.bg == Some(theme.list_match_bg))
            .map(|span| span.content.to_string())
            .collect::<String>()
        };

        // the fuzzy matcher counts chars, the highlight takes bytes
        let fuzzy = SearchMatcher::new("ül", false, true);
        let pos = fuzzy.matched_position("Müller").unwrap();
        assert_eq!(pos.matched_indices, vec![1, 2, 3]);
        assert_eq!(highlight("Müller", pos.matched_indices, false), "ül");

        // a byte inside a char takes the whole char
        assert_eq!(highlight("Müller", vec![2], false), "ü");
        // past the end of a truncated text
        assert_eq!(highlight("Mü…", vec![1, 2, 40], true), "ü");
    }

    #[test]
    fn test_smart_case() {
        // ignoring case unless the query has an uppercase letter; case-sensitive never
        let found = |query, smart_case, text| {
            SearchMatcher::new(query, smart_case, false)
                .matched_position(text)
                .is_some()
        };
        assert!(found("fix", true, "Fix bug"));
        assert!(!found("Fix", true, "fix bug"));
        assert!(found("Fix", true, "Fix bug"));
        assert!(!found("fix", false, "Fix bug"));
    }

    #[test]
    fn test_search_target_change_clears_previous_field_matches() {
        let commit = Commit {
            commit_hash: CommitHash::from("abcdef0123456789abcdef0123456789abcdef01"),
            subject: "match".into(),
            author_name: "match".into(),
            ..Commit::default()
        };
        let reference = Ref::Branch {
            name: "match".into(),
            target: commit.commit_hash.clone(),
        };
        let refs = [&reference];
        let matcher = SearchMatcher::new("match", false, false);
        let mut search_match = SearchMatch::default();

        search_match.set(&commit, &refs, &matcher, SearchTarget::All);
        assert!(!search_match.refs.is_empty());
        assert!(search_match.subject.is_some());
        assert!(search_match.author_name.is_some());

        search_match.set(&commit, &refs, &matcher, SearchTarget::Hash);
        assert!(search_match.refs.is_empty());
        assert!(search_match.subject.is_none());
        assert!(search_match.author_name.is_none());
    }

    #[test]
    fn test_applied_search_target_toggle_recalculates_matches() {
        with_commit_list_state(&["fix", "other"], |state| {
            input_search_query(state, "fix");
            state.apply_search();

            state.toggle_search_target();
            assert_eq!(state.search_options().target, SearchTarget::Subject);
            assert_eq!(
                state.matched_query_string(),
                Some(("Match 1 of 1 (query: \"fix\")".into(), true))
            );

            state.toggle_search_target();
            assert_eq!(state.search_options().target, SearchTarget::Author);
            assert_eq!(
                state.matched_query_string(),
                Some(("No matches found (query: \"fix\")".into(), false))
            );
        });
    }

    #[test]
    fn test_applied_search_options_keep_selected_match() {
        with_commit_list_state(&["FIX", "fix"], |state| {
            input_search_query(state, "fix");
            state.apply_search();
            state.toggle_smart_case();

            assert_eq!(
                state.commits[state.current_selected_index()].commit.subject,
                "fix"
            );
            assert_eq!(
                state.matched_query_string(),
                Some(("Match 2 of 2 (query: \"fix\")".into(), true))
            );
        });
    }

    #[test]
    fn test_applied_search_options_select_next_match() {
        with_commit_list_state(&["FIX", "fix"], |state| {
            state.toggle_smart_case();
            input_search_query(state, "fix");
            state.apply_search();
            state.toggle_smart_case();

            assert_eq!(
                state.commits[state.current_selected_index()].commit.subject,
                "fix"
            );
            assert_eq!(
                state.matched_query_string(),
                Some(("Match 1 of 1 (query: \"fix\")".into(), true))
            );
        });
    }

    #[test]
    fn test_applied_search_options_keep_selection_when_no_matches() {
        with_commit_list_state(&["fix", "other"], |state| {
            state.toggle_fuzzy();
            input_search_query(state, "fx");
            state.apply_search();
            let selected = state.current_selected_index();

            state.toggle_fuzzy();

            assert_eq!(state.current_selected_index(), selected);
            assert_eq!(
                state.matched_query_string(),
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
