use std::{collections::HashSet, rc::Rc};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    widgets::StatefulWidget,
};
use rustc_hash::FxHashMap;
use semver::Version;
use unicode_width::UnicodeWidthStr;

use crate::{
    app::AppContext,
    git::{split_remote_branch, CommitHash, Head, Ref},
    widget::{
        file_tree::{keep_guide_color, tree_guides},
        style::{
            divider, fold_marker, mark_picked_row, mark_selected_row, panel_title, render_panel,
            render_panel_scrollbar, scroll_to, spinner,
        },
    },
};

const TREE_LOCAL_CHANGES_ROOT_IDENT: &str = "__local_changes__";
const TREE_BRANCH_ROOT_IDENT: &str = "__branches__";
const TREE_REMOTE_ROOT_IDENT: &str = "__remotes__";
// the row of a detached HEAD in the branches, see `RefListState::set_head`
pub const DETACHED_HEAD: &str = "HEAD";
const TREE_TAG_ROOT_IDENT: &str = "__tags__";
const TREE_STASH_ROOT_IDENT: &str = "__stashes__";

const TREE_BRANCH_ROOT_TEXT: &str = "Branches";
const TREE_REMOTE_ROOT_TEXT: &str = "Remotes";
const TREE_TAG_ROOT_TEXT: &str = "Tags";
const TREE_STASH_ROOT_TEXT: &str = "Stashes";

// The sections of the sidebar under the local changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefSection {
    Branches,
    Remotes,
    Tags,
    Stashes,
}

impl RefSection {
    fn of_root(identifier: &str) -> Option<RefSection> {
        match identifier {
            TREE_BRANCH_ROOT_IDENT => Some(RefSection::Branches),
            TREE_REMOTE_ROOT_IDENT => Some(RefSection::Remotes),
            TREE_TAG_ROOT_IDENT => Some(RefSection::Tags),
            TREE_STASH_ROOT_IDENT => Some(RefSection::Stashes),
            _ => None,
        }
    }

    // What the refs of the section are, like "branches".
    pub fn plural(self) -> &'static str {
        match self {
            RefSection::Branches => "branches",
            RefSection::Remotes => "remote branches",
            RefSection::Tags => "tags",
            RefSection::Stashes => "stashes",
        }
    }
}

// The refs picked in the sidebar, all of one section so the commands on them make sense, in the
// order they were picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefPicks {
    pub section: RefSection,
    pub names: Vec<String>,
}

impl Default for RefPicks {
    fn default() -> Self {
        Self {
            section: RefSection::Branches,
            names: vec![],
        }
    }
}

impl RefPicks {
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    fn contains(&self, section: RefSection, name: &str) -> bool {
        self.section == section && self.names.iter().any(|n| n == name)
    }
}

// What is selected in the sidebar, for its commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefSelection {
    LocalChanges,
    // a section
    Section(RefSection),
    // a folder in a section, by its path: a remote ("origin"), or a prefix of names ("feature")
    Folder(RefSection, String),
    // a ref, by its full name: "main", "origin/main", "v1.0", "stash@{0}"
    Ref(RefSection, String),
}

#[derive(Debug)]
pub struct RefListState {
    roots: Vec<RefTreeNode>,
    visible_rows: Vec<VisibleRefRow>,
    selected: Vec<String>,
    opened: HashSet<Vec<String>>,
    offset: usize,
    scroll_to_selection: bool,
    picked: RefPicks,
    // the commits to push and to pull of the local branches with an upstream, by name
    tracking: FxHashMap<String, (usize, usize)>,
    // the checked-out branch, or "HEAD" when detached, marked
    head_branch: Option<String>,
    // the commits of the stashes by name, their names change as stashes come and go
    stash_targets: FxHashMap<String, CommitHash>,
}

impl RefListState {
    // `remotes` are the names of the remotes, the folders of their branches.
    pub fn new(refs: &[&Ref], remotes: &[String], local_change_count: usize) -> Self {
        let selected = vec![TREE_LOCAL_CHANGES_ROOT_IDENT.into()];
        let opened = HashSet::from([vec![TREE_BRANCH_ROOT_IDENT.into()]]);
        let mut state = Self {
            roots: build_ref_tree_nodes(refs, remotes, local_change_count),
            visible_rows: Vec::new(),
            selected,
            opened,
            offset: 0,
            scroll_to_selection: true,
            picked: RefPicks::default(),
            tracking: FxHashMap::default(),
            head_branch: None,
            stash_targets: refs
                .iter()
                .filter(|r| matches!(r, Ref::Stash { .. }))
                .map(|r| (r.name().to_string(), r.target().clone()))
                .collect(),
        };
        state.rebuild_visible_rows();
        state
    }

    // The commits ahead of and behind the upstream of each local branch, shown beside them.
    pub fn set_tracking(&mut self, tracking: FxHashMap<String, (usize, usize)>) {
        self.tracking = tracking;
    }

    // Marks the checked-out branch; a detached HEAD gets a row of its own, first in the branches.
    pub fn set_head(&mut self, head: &Head) {
        self.head_branch = match head {
            Head::Branch { name } => Some(name.clone()),
            Head::Detached { target } => {
                let name = format!("HEAD (detached at {})", target.as_short_hash());
                let node = RefTreeNode::new(DETACHED_HEAD.into(), name, Vec::new());
                if let Some(branches) = self
                    .roots
                    .iter_mut()
                    .find(|r| r.identifier == TREE_BRANCH_ROOT_IDENT)
                {
                    branches.children.insert(0, node);
                }
                self.rebuild_visible_rows();
                Some(DETACHED_HEAD.into())
            }
            Head::None => None,
        };
    }

    pub fn picked(&self) -> &RefPicks {
        &self.picked
    }

    // Picks or unpicks the selected ref; refs of another section than the picked ones can't be
    // picked with them.
    pub fn toggle_pick(&mut self) -> Result<(), String> {
        let RefSelection::Ref(section, name) = self.selection() else {
            return Err("Select a branch, a tag or a stash to pick it".into());
        };
        if self.picked.is_empty() {
            self.picked.section = section;
        } else if self.picked.section != section {
            return Err(format!(
                "Only {} can be picked with the picked {}, esc clears them",
                self.picked.section.plural(),
                self.picked.section.plural()
            ));
        }
        match self.picked.names.iter().position(|n| *n == name) {
            Some(i) => {
                self.picked.names.remove(i);
            }
            None => self.picked.names.push(name),
        }
        Ok(())
    }

    pub fn clear_picks(&mut self) {
        self.picked.names.clear();
    }

    pub fn select_next(&mut self) {
        let last = self.visible_rows.len().saturating_sub(1);
        let mut next = self
            .selected_index()
            .map_or(0, |index| index.saturating_add(1))
            .min(last);
        if self.visible_rows[next].separator {
            next = (next + 1).min(last);
        }
        self.select_visible_index(next);
    }

    pub fn select_prev(&mut self) {
        let mut prev = self
            .selected_index()
            .map_or(self.visible_rows.len().saturating_sub(1), |index| {
                index.saturating_sub(1)
            });
        if self.visible_rows[prev].separator {
            prev = prev.saturating_sub(1);
        }
        self.select_visible_index(prev);
    }

    pub fn select_first(&mut self) {
        self.select_visible_index(0);
    }

    pub fn select_last(&mut self) {
        self.select_visible_index(self.visible_rows.len().saturating_sub(1));
    }

    pub fn open_node(&mut self) {
        if self
            .visible_rows
            .iter()
            .any(|row| row.identifier == self.selected && row.has_children)
            && self.opened.insert(self.selected.clone())
        {
            self.rebuild_visible_rows();
            self.scroll_to_selection = true;
        }
    }

    // Opens or closes the selected node; returns false when it has no children.
    pub fn toggle_node(&mut self) -> bool {
        let has_children = self
            .visible_rows
            .iter()
            .any(|row| row.identifier == self.selected && row.has_children);
        if !has_children {
            return false;
        }
        if !self.opened.remove(&self.selected) {
            self.opened.insert(self.selected.clone());
        }
        self.rebuild_visible_rows();
        self.scroll_to_selection = true;
        true
    }

    pub fn close_node(&mut self) {
        if self.opened.remove(&self.selected) {
            self.rebuild_visible_rows();
        } else if self.selected.len() > 1 {
            self.selected.pop();
        }
        // The category roots always remain selectable.
        self.scroll_to_selection = true;
    }

    pub fn selected_ref_name(&self) -> Option<String> {
        if self.is_local_changes_selected() {
            None
        } else {
            self.selected.last().cloned()
        }
    }

    pub fn select_branches(&mut self) {
        self.open_section(TREE_BRANCH_ROOT_IDENT.into());
    }

    // Selects and opens the section after (or before) the selected one, round; from the local
    // changes the first (or last).
    pub fn select_section(&mut self, forward: bool) {
        let sections: Vec<&String> = self
            .roots
            .iter()
            .map(|root| &root.identifier)
            .filter(|id| RefSection::of_root(id).is_some())
            .collect();
        let n = sections.len();
        if n == 0 {
            return;
        }
        let next = match (
            sections.iter().position(|s| **s == self.selected[0]),
            forward,
        ) {
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
            (None, true) => 0,
            (None, false) => n - 1,
        };
        self.open_section(sections[next].clone());
    }

    fn open_section(&mut self, identifier: String) {
        self.selected = vec![identifier];
        self.opened.insert(self.selected.clone());
        self.rebuild_visible_rows();
        self.scroll_to_selection = true;
    }

    pub fn select_local_changes(&mut self) {
        self.selected = vec![TREE_LOCAL_CHANGES_ROOT_IDENT.into()];
        self.scroll_to_selection = true;
    }

    // Keeps the count in the name of the local changes row up to date.
    pub fn set_local_change_count(&mut self, count: usize) {
        let name = local_changes_name(count);
        if let Some(root) = self.roots.first_mut() {
            if root.name != name {
                root.name.clone_from(&name);
                if let Some(row) = self.visible_rows.first_mut() {
                    row.name = name;
                }
            }
        }
    }

    pub fn selection(&self) -> RefSelection {
        let Some(section) = RefSection::of_root(&self.selected[0]) else {
            return RefSelection::LocalChanges;
        };
        let is_ref = self.selected.len() > 1
            && self
                .visible_rows
                .iter()
                .any(|row| row.identifier == self.selected && !row.folder);
        match self.selected.last() {
            Some(name) if is_ref => RefSelection::Ref(section, name.clone()),
            Some(path) if self.selected.len() > 1 => RefSelection::Folder(section, path.clone()),
            _ => RefSelection::Section(section),
        }
    }

    pub fn is_local_changes_selected(&self) -> bool {
        self.selected.len() == 1 && self.selected[0] == TREE_LOCAL_CHANGES_ROOT_IDENT
    }

    // Shows the refs of the reloaded repository, keeping the selection, the opened nodes and the
    // picked refs still there.
    pub fn set_refs(
        &mut self,
        refs: &[&Ref],
        remotes: &[String],
        local_change_count: usize,
        head: &Head,
    ) {
        let (selected, opened) = self.current_tree_status();
        let offset = self.offset;
        let mut picked = std::mem::take(&mut self.picked);
        // a picked stash by its commit, as stash@{0} names another once one is stashed
        if picked.section == RefSection::Stashes {
            let targets: Vec<_> = picked
                .names
                .iter()
                .filter_map(|name| self.stash_targets.get(name).cloned())
                .collect();
            picked.names = refs
                .iter()
                .filter(|r| matches!(r, Ref::Stash { .. }) && targets.contains(r.target()))
                .map(|r| r.name().to_string())
                .collect();
        }
        *self = Self::new(refs, remotes, local_change_count);
        // the row of a detached HEAD, before the selection looks for it
        self.set_head(head);
        self.reset_tree_status(selected, opened);
        self.offset = offset;
        picked.names.retain(|name| {
            refs.iter().any(|r| {
                r.name() == name
                    && matches!(
                        (r, picked.section),
                        (Ref::Branch { .. }, RefSection::Branches)
                            | (Ref::RemoteBranch { .. }, RefSection::Remotes)
                            | (Ref::Tag { .. }, RefSection::Tags)
                            | (Ref::Stash { .. }, RefSection::Stashes)
                    )
            })
        });
        self.picked = picked;
    }

    pub fn current_tree_status(&self) -> (Vec<String>, Vec<Vec<String>>) {
        (self.selected.clone(), self.opened.iter().cloned().collect())
    }

    pub fn reset_tree_status(&mut self, selected: Vec<String>, opened: Vec<Vec<String>>) {
        self.opened = opened.into_iter().collect();
        self.rebuild_visible_rows();
        self.selected = selected;
        while !self.selected.is_empty() && self.selected_index().is_none() {
            self.selected.pop();
        }
        if self.selected.is_empty() {
            self.selected = vec![TREE_LOCAL_CHANGES_ROOT_IDENT.into()];
        }
        self.scroll_to_selection = true;
    }

    fn selected_index(&self) -> Option<usize> {
        self.visible_rows
            .iter()
            .position(|row| row.identifier == self.selected)
    }

    fn select_visible_index(&mut self, index: usize) {
        if let Some(row) = self.visible_rows.get(index).filter(|row| !row.separator) {
            self.selected.clone_from(&row.identifier);
            self.scroll_to_selection = true;
        }
    }

    fn rebuild_visible_rows(&mut self) {
        self.visible_rows.clear();
        collect_visible_rows(&self.roots, &self.opened, &[], &mut self.visible_rows);
    }
}

#[derive(Debug)]
struct VisibleRefRow {
    identifier: Vec<String>,
    name: String,
    depth: usize,
    has_children: bool,
    // a section, a folder or a remote, even without children
    folder: bool,
    separator: bool,
}

fn collect_visible_rows(
    nodes: &[RefTreeNode],
    opened: &HashSet<Vec<String>>,
    parent: &[String],
    rows: &mut Vec<VisibleRefRow>,
) {
    for node in nodes {
        let mut identifier = parent.to_vec();
        identifier.push(node.identifier.clone());
        rows.push(VisibleRefRow {
            identifier: identifier.clone(),
            name: node.name.clone(),
            depth: parent.len(),
            has_children: !node.children.is_empty(),
            folder: node.folder || !node.children.is_empty(),
            separator: false,
        });
        if parent.is_empty() && node.identifier == TREE_LOCAL_CHANGES_ROOT_IDENT {
            // divides the working tree from the refs, and is skipped when moving the selection
            rows.push(VisibleRefRow {
                identifier: Vec::new(),
                name: String::new(),
                depth: 0,
                has_children: false,
                folder: false,
                separator: true,
            });
        }
        if opened.contains(&identifier) {
            collect_visible_rows(&node.children, opened, &identifier, rows);
        }
    }
}

pub struct RefList {
    ctx: Rc<AppContext>,
    focused: bool,
    // only the title is shown
    collapsed: bool,
}

impl RefList {
    pub fn new(ctx: Rc<AppContext>, focused: bool, collapsed: bool) -> RefList {
        RefList {
            ctx,
            focused,
            collapsed,
        }
    }
}

impl StatefulWidget for RefList {
    type State = RefListState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let theme = &self.ctx.color_theme;
        let label = if self.collapsed { "[0]" } else { "[0] Refs" };
        let picked = match state.picked.len() {
            0 => String::new(),
            n if self.collapsed => n.to_string(),
            n => format!("{n} picked"),
        };
        let inner = render_panel(
            buf,
            area,
            panel_title(label, &picked, self.focused, theme),
            self.focused,
            theme,
        );
        if self.collapsed || inner.width == 0 || inner.height == 0 {
            return;
        }

        let height = inner.height as usize;
        state.offset = state.offset.min(state.visible_rows.len().saturating_sub(1));
        if state.scroll_to_selection {
            if let Some(index) = state.selected_index() {
                let (total, scrolloff) = (
                    state.visible_rows.len(),
                    self.ctx.ui_config.common.scrolloff,
                );
                state.offset = scroll_to(state.offset, index, height, total, scrolloff as usize);
            }
            state.scroll_to_selection = false;
        }
        let total = state.visible_rows.len();
        render_panel_scrollbar(buf, area, total, state.offset, self.focused, theme);

        let item_style = Style::default().fg(theme.fg);
        // the sections stand out from the refs in them
        let root_style = item_style.add_modifier(Modifier::BOLD);

        // the lines of the tree, like the file trees'; a folder's fold marker and the checked-out
        // branch's dot go in their own column
        let guides = tree_guides(state.visible_rows.iter().map(|row| {
            let head = row.depth > 0
                && row.identifier[0] == TREE_BRANCH_ROOT_IDENT
                && state.head_branch.as_deref() == row.identifier.last().map(String::as_str);
            (row.depth, row.separator || row.has_children || head)
        }));
        for ((line, row), guide) in state
            .visible_rows
            .iter()
            .skip(state.offset)
            .take(height)
            .enumerate()
            .zip(guides.iter().skip(state.offset))
        {
            let y = inner.y + line as u16;
            if row.separator {
                let width = inner.width as usize;
                buf.set_span(inner.x, y, &divider(width, theme), inner.width);
                continue;
            }
            // the checked-out branch, like `git branch`
            let head = row.depth > 0
                && row.identifier[0] == TREE_BRANCH_ROOT_IDENT
                && state.head_branch.as_deref() == row.identifier.last().map(String::as_str);
            let symbol = if head {
                "● "
            } else if !row.has_children {
                ""
            } else {
                fold_marker(state.opened.contains(&row.identifier))
            };
            let (guide_x, _) = buf.set_stringn(
                inner.x,
                y,
                guide,
                inner.width as usize,
                Style::default().fg(theme.divider_fg),
            );
            let symbol_style = if head {
                Style::default().fg(theme.list_ref_head_fg)
            } else {
                Style::default().fg(theme.muted_fg)
            };
            let room = inner.right().saturating_sub(guide_x) as usize;
            let (text_x, _) = buf.set_stringn(guide_x, y, symbol, room, symbol_style);
            let remaining = inner.width.saturating_sub(text_x - inner.x);
            let picked = !row.folder
                && row.identifier.len() > 1
                && RefSection::of_root(&row.identifier[0]).is_some_and(|section| {
                    state
                        .picked
                        .contains(section, &row.identifier[row.identifier.len() - 1])
                });
            let style = if row.depth == 0 {
                root_style
            } else if head {
                root_style.fg(theme.list_ref_head_fg)
            } else {
                item_style
            };
            // a local branch: the commits to push and to pull, after its name
            let branch = (row.depth > 0 && row.identifier[0] == TREE_BRANCH_ROOT_IDENT)
                .then(|| row.identifier[row.identifier.len() - 1].as_str());
            // what runs on it, like lazygit's "Pushing |", in place of its counts
            let task = self.ctx.running_task.borrow();
            let running = task
                .as_ref()
                .and_then(|task| task.branch_status.as_ref())
                .filter(|status| branch == Some(status.branch.as_str()));
            let tracking = branch
                .filter(|_| running.is_none())
                .and_then(|branch| state.tracking.get(branch));
            let mut counts = vec![];
            if let Some(&(ahead, behind)) = tracking {
                if ahead > 0 {
                    counts.push((format!("↑{ahead}"), theme.status_success_fg));
                }
                if behind > 0 {
                    counts.push((format!("↓{behind}"), theme.status_warn_fg));
                }
            }
            if let Some(status) = running {
                // shortened to "↑ ∙●∙" when the name leaves no room for "Pushing ∙●∙"
                let room = (remaining as usize).saturating_sub(row.name.width() + 1);
                let label = format!("{} {}", status.label, spinner());
                let label = if label.width() <= room {
                    label
                } else {
                    format!("{} {}", status.short, spinner())
                };
                counts.push((label, theme.highlight_fg));
            }
            let counts_width: usize = counts.iter().map(|(c, _)| c.width() + 1).sum();
            let name_width = (remaining as usize).saturating_sub(counts_width);
            let (mut x, _) = buf.set_stringn(text_x, y, &row.name, name_width, style);
            for (count, fg) in counts {
                let width = inner.right().saturating_sub(x + 1) as usize;
                (x, _) = buf.set_stringn(x + 1, y, count, width, Style::default().fg(fg));
            }
            if picked {
                mark_picked_row(buf, Rect::new(inner.x, y, inner.width, 1), theme);
            }
            if row.identifier == state.selected {
                mark_selected_row(buf, inner, y, self.focused, theme);
            }
            keep_guide_color(buf, inner, y, guide, theme);
        }
    }
}

fn build_ref_tree_nodes(
    refs: &[&Ref],
    remotes: &[String],
    local_change_count: usize,
) -> Vec<RefTreeNode> {
    let mut branch_refs = Vec::new();
    let mut remote_refs = Vec::new();
    let mut tag_refs = Vec::new();
    let mut stash_refs = Vec::new();

    for &r in refs {
        match r {
            Ref::Tag { name, .. } => tag_refs.push(name.into()),
            Ref::Branch { name, .. } => branch_refs.push(name.into()),
            Ref::RemoteBranch { name, .. } => remote_refs.push(name.into()),
            Ref::Stash { name, message, .. } => stash_refs.push((name.into(), message.into())),
        }
    }

    let mut branch_nodes = refs_to_ref_tree_nodes(branch_refs);
    let remote_nodes = remotes_to_ref_tree_nodes(remote_refs, remotes);
    let mut tag_nodes = refs_to_ref_tree_nodes(tag_refs);
    let mut stash_nodes = refs_to_stash_ref_tree_nodes(stash_refs);

    sort_branch_tree_nodes(&mut branch_nodes);
    sort_tag_tree_nodes(&mut tag_nodes);
    sort_stash_tree_nodes(&mut stash_nodes);

    vec![
        RefTreeNode::new(
            TREE_LOCAL_CHANGES_ROOT_IDENT.into(),
            local_changes_name(local_change_count),
            Vec::new(),
        ),
        RefTreeNode::new(
            TREE_BRANCH_ROOT_IDENT.into(),
            TREE_BRANCH_ROOT_TEXT.into(),
            branch_nodes,
        ),
        RefTreeNode::new(
            TREE_REMOTE_ROOT_IDENT.into(),
            TREE_REMOTE_ROOT_TEXT.into(),
            remote_nodes,
        ),
        RefTreeNode::new(
            TREE_TAG_ROOT_IDENT.into(),
            TREE_TAG_ROOT_TEXT.into(),
            tag_nodes,
        ),
        RefTreeNode::new(
            TREE_STASH_ROOT_IDENT.into(),
            TREE_STASH_ROOT_TEXT.into(),
            stash_nodes,
        ),
    ]
}

fn local_changes_name(count: usize) -> String {
    format!("Local Changes ({count})")
}

#[derive(Debug)]
struct RefTreeNode {
    identifier: String,
    name: String,
    children: Vec<RefTreeNode>,
    // a remote, a folder even without branches; others are folders when they have children
    folder: bool,
}

impl RefTreeNode {
    fn new(identifier: String, name: String, children: Vec<Self>) -> Self {
        Self {
            identifier,
            name,
            children,
            folder: false,
        }
    }
}

fn refs_to_stash_ref_tree_nodes(ref_name_messages: Vec<(String, String)>) -> Vec<RefTreeNode> {
    let mut nodes: Vec<RefTreeNode> = Vec::new();
    for (name, message) in ref_name_messages {
        let node = RefTreeNode {
            identifier: name.clone(),
            name: message.to_string(),
            children: Vec::new(),
            folder: false,
        };
        nodes.push(node);
    }
    nodes
}

// A folder for each remote, named in full even with a '/' in its name, with its branches in
// folders by the '/' in their names; then the branches of no remote known, like those of a
// removed one, in folders by every '/'.
fn remotes_to_ref_tree_nodes(ref_names: Vec<String>, remotes: &[String]) -> Vec<RefTreeNode> {
    let mut by_remote: Vec<(String, Vec<String>)> =
        remotes.iter().map(|r| (r.clone(), vec![])).collect();
    let mut unknown = vec![];
    for name in ref_names {
        match split_remote_branch(remotes, &name) {
            Some((remote, branch)) => {
                let i = remotes.iter().position(|r| r == remote).unwrap();
                by_remote[i].1.push(branch.to_string());
            }
            None => unknown.push(name),
        }
    }
    // every remote, so one not fetched yet can still be edited or removed
    let mut nodes: Vec<RefTreeNode> = by_remote
        .into_iter()
        .map(|(remote, branches)| {
            let mut children = refs_to_ref_tree_nodes(branches);
            sort_branch_tree_nodes(&mut children);
            prefix_identifiers(&mut children, &remote);
            RefTreeNode {
                folder: true,
                ..RefTreeNode::new(remote.clone(), remote, children)
            }
        })
        .collect();
    let mut unknown = refs_to_ref_tree_nodes(unknown);
    sort_branch_tree_nodes(&mut unknown);
    nodes.extend(unknown);
    nodes
}

// The identifiers of the nodes under the folder `prefix`, which are their full names.
fn prefix_identifiers(nodes: &mut [RefTreeNode], prefix: &str) {
    for node in nodes {
        node.identifier = format!("{prefix}/{}", node.identifier);
        prefix_identifiers(&mut node.children, prefix);
    }
}

fn refs_to_ref_tree_nodes(ref_names: Vec<String>) -> Vec<RefTreeNode> {
    let mut nodes: Vec<RefTreeNode> = Vec::new();

    for ref_name in ref_names {
        let mut current_nodes = &mut nodes;
        let mut parent_identifier = String::new();

        for part in ref_name.split('/') {
            if let Some(index) = current_nodes.iter().position(|n| n.name == part) {
                let node = &mut current_nodes[index];
                current_nodes = &mut node.children;
                parent_identifier.clone_from(&node.identifier);
            } else {
                let identifier = if parent_identifier.is_empty() {
                    part.to_string()
                } else {
                    format!("{parent_identifier}/{part}")
                };
                let node = RefTreeNode {
                    identifier: identifier.clone(),
                    name: part.to_string(),
                    children: Vec::new(),
                    folder: false,
                };
                current_nodes.push(node);
                current_nodes = current_nodes.last_mut().unwrap().children.as_mut();
                parent_identifier = identifier;
            }
        }
    }

    nodes
}

fn sort_branch_tree_nodes(nodes: &mut [RefTreeNode]) {
    nodes.sort_by(|a, b| {
        b.children
            .len()
            .cmp(&a.children.len())
            .then(a.name.cmp(&b.name))
    });
    for node in nodes {
        sort_branch_tree_nodes(&mut node.children);
    }
}

fn sort_tag_tree_nodes(nodes: &mut [RefTreeNode]) {
    nodes.sort_by(|a, b| {
        let a_version = parse_semantic_version_tag(&a.name);
        let b_version = parse_semantic_version_tag(&b.name);
        if a_version.is_none() && b_version.is_none() {
            // if both are not semantic versions, sort by name asc
            a.name.cmp(&b.name)
        } else {
            // if both are semantic versions, sort by version desc
            // if only one is a semantic version, it will be sorted first
            b_version.cmp(&a_version)
        }
    });
}

// By their index, stash@{2} before stash@{10}.
fn sort_stash_tree_nodes(nodes: &mut [RefTreeNode]) {
    let index = |node: &RefTreeNode| {
        node.identifier
            .trim_start_matches("stash@{")
            .trim_end_matches('}')
            .parse::<usize>()
            .unwrap_or(usize::MAX)
    };
    nodes.sort_by_key(index);
}

fn parse_semantic_version_tag(tag: &str) -> Option<Version> {
    let tag = tag.trim_start_matches('v');
    Version::parse(tag).ok()
}

#[cfg(test)]
mod tests {
    use ratatui::{buffer::Buffer, layout::Rect, widgets::StatefulWidget};

    use super::*;
    use crate::{
        color::ColorTheme,
        test_support::ui::{
            assert_guides_keep_color, assert_only_bg_changed, assert_row_bg, find_row, panel_row,
            test_ctx,
        },
    };

    // Renders the sidebar, `focused` or not, into a buffer.
    fn draw(state: &mut RefListState, focused: bool) -> Buffer {
        let area = Rect::new(0, 0, 30, 12);
        let mut buf = Buffer::empty(area);
        RefList::new(test_ctx(), focused, false).render(area, &mut buf, state);
        buf
    }

    // Moves the selection to the row named `name`, from the top.
    fn select(state: &mut RefListState, name: &str) {
        state.select_first();
        for _ in 0..50 {
            if state.selected.last().map(String::as_str) == Some(name) {
                return;
            }
            state.select_next();
        }
        panic!("no row {name}");
    }

    #[test]
    fn test_rows_look_like_the_other_lists() {
        let branch = |name: &str| Ref::Branch {
            name: name.into(),
            target: "a".repeat(40).as_str().into(),
        };
        let (one, two, main) = (branch("feature/one"), branch("feature/two"), branch("main"));
        let mut state = RefListState::new(&[&one, &two, &main], &[], 0);
        state.select_branches();
        select(&mut state, "feature");
        state.open_node();
        let theme = ColorTheme::default();

        for focused in [true, false] {
            select(&mut state, "feature/two");
            let plain = draw(&mut state, focused);
            select(&mut state, "feature/one");
            let selected = draw(&mut state, focused);
            let y = find_row(&selected, "one");
            let row = panel_row(&selected, "Refs", y, 1);
            // the background across the row, focused or not; the text as it was
            assert_row_bg(&selected, row, theme.list_selected_bg);
            assert_only_bg_changed(&plain, &selected, row);
            assert_guides_keep_color(&selected, row, &theme);
        }

        // a picked branch: across the row too
        state.toggle_pick().unwrap();
        select(&mut state, "main");
        let buf = draw(&mut state, true);
        let y = find_row(&buf, "one");
        let row = panel_row(&buf, "Refs", y, 1);
        assert_row_bg(&buf, row, theme.list_picked_bg);
        assert_guides_keep_color(&buf, row, &theme);
    }

    #[test]
    fn test_picks_of_one_section() {
        let branch = |name: &str| Ref::Branch {
            name: name.into(),
            target: "a".repeat(40).as_str().into(),
        };
        let tag = Ref::Tag {
            name: "v1".into(),
            target: "a".repeat(40).as_str().into(),
        };
        let (a, b) = (branch("a"), branch("b"));
        let refs = [&a, &b, &tag];
        let mut state = RefListState::new(&refs, &[], 0);
        state.select_branches();
        state.select_next();
        state.toggle_pick().unwrap();
        state.select_next();
        state.toggle_pick().unwrap();
        assert_eq!(state.picked().names, ["a", "b"]);
        // a tag can't be picked with branches
        state.reset_tree_status(
            vec![TREE_TAG_ROOT_IDENT.into(), "v1".into()],
            vec![vec![TREE_TAG_ROOT_IDENT.into()]],
        );
        assert!(state.toggle_pick().unwrap_err().contains("Only branches"));
        // the picks stay through a reload, without those gone
        state.set_refs(&[&a, &tag], &[], 0, &Head::None);
        assert_eq!(state.picked().names, ["a"]);
        state.clear_picks();
        state.toggle_pick().unwrap();
        assert_eq!(state.picked().section, RefSection::Tags);
    }

    #[test]
    fn test_detached_head_row() {
        let target: CommitHash = "a".repeat(40).as_str().into();
        let branch = Ref::Branch {
            name: "main".into(),
            target: target.clone(),
        };
        let mut state = RefListState::new(&[&branch], &[], 0);
        state.set_head(&Head::Branch {
            name: "main".into(),
        });
        let names = |state: &RefListState| -> Vec<String> {
            state.visible_rows.iter().map(|r| r.name.clone()).collect()
        };
        assert!(!names(&state).iter().any(|n| n.starts_with("HEAD")));
        // only while detached, first in the branches, selecting the commit as a ref
        let head = Head::Detached { target };
        state.set_head(&head);
        assert!(names(&state).contains(&"HEAD (detached at aaaaaaa)".to_string()));
        state.select_branches();
        state.select_next();
        let selection = RefSelection::Ref(RefSection::Branches, DETACHED_HEAD.into());
        assert_eq!(state.selection(), selection);
        // and stays selected through a reload
        state.set_refs(&[&branch], &[], 0, &head);
        assert_eq!(state.selection(), selection);
    }

    #[test]
    fn test_stash_picks_follow_their_stash() {
        let stash = |n: usize, c: char| Ref::Stash {
            name: format!("stash@{{{n}}}"),
            message: String::new(),
            target: c.to_string().repeat(40).as_str().into(),
        };
        let mut state = RefListState::new(&[&stash(0, 'a')], &[], 0);
        state.reset_tree_status(
            vec![TREE_STASH_ROOT_IDENT.into(), "stash@{0}".into()],
            vec![vec![TREE_STASH_ROOT_IDENT.into()]],
        );
        state.toggle_pick().unwrap();
        // a new stash takes stash@{0}, the picked one is stash@{1} now
        state.set_refs(&[&stash(0, 'b'), &stash(1, 'a')], &[], 0, &Head::None);
        assert_eq!(state.picked().names, ["stash@{1}"]);
    }

    #[test]
    fn test_remote_folders() {
        let remote = |name: &str| Ref::RemoteBranch {
            name: name.into(),
            target: "a".repeat(40).as_str().into(),
        };
        let refs = [
            remote("team/origin/main"),
            remote("team/origin/feature/x"),
            remote("origin/main"),
            remote("gone/main"),
        ];
        let refs: Vec<&Ref> = refs.iter().collect();
        let remotes = ["origin".to_string(), "team/origin".to_string()];
        let nodes = remotes_to_ref_tree_nodes(
            refs.iter().map(|r| r.name().to_string()).collect(),
            &remotes,
        );
        let tree = |nodes: &[RefTreeNode]| -> Vec<(String, String)> {
            nodes
                .iter()
                .map(|n| (n.identifier.clone(), n.name.clone()))
                .collect()
        };
        // a folder for each remote, named in full, then the branches of no remote known
        assert_eq!(
            tree(&nodes),
            [
                ("origin".into(), "origin".into()),
                ("team/origin".into(), "team/origin".into()),
                ("gone".into(), "gone".into()),
            ]
        );
        // the branches of a remote by their full names, in folders by the rest
        assert_eq!(
            tree(&nodes[1].children),
            [
                ("team/origin/feature".into(), "feature".into()),
                ("team/origin/main".into(), "main".into()),
            ]
        );
        assert_eq!(
            tree(&nodes[1].children[0].children),
            [("team/origin/feature/x".into(), "x".into())]
        );

        // a remote not fetched yet is still a folder, to edit or remove it
        let mut state = RefListState::new(&[], &["new".to_string()], 0);
        while !matches!(
            state.selection(),
            RefSelection::Section(RefSection::Remotes)
        ) {
            state.select_next();
        }
        state.open_node();
        state.select_next();
        assert!(matches!(
            state.selection(),
            RefSelection::Folder(RefSection::Remotes, name) if name == "new"
        ));
    }

    #[test]
    fn test_select_section() {
        let mut state = RefListState::new(&[], &["origin".to_string()], 0);
        let section = |state: &RefListState| match state.selection() {
            RefSelection::Section(section) => Some(section),
            _ => None,
        };
        // from the local changes, round both ways
        state.select_section(false);
        assert_eq!(section(&state), Some(RefSection::Stashes));
        state.select_section(true);
        assert_eq!(section(&state), Some(RefSection::Branches));
        state.select_section(true);
        assert_eq!(section(&state), Some(RefSection::Remotes));
        // opened: the remote under it is the next row
        state.select_next();
        assert!(matches!(
            state.selection(),
            RefSelection::Folder(RefSection::Remotes, name) if name == "origin"
        ));
        // from a row in the section
        state.select_section(false);
        assert_eq!(section(&state), Some(RefSection::Branches));
    }
}
