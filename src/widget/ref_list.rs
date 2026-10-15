use std::{collections::HashSet, rc::Rc};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    widgets::{Padding, StatefulWidget},
};
use rustc_hash::FxHashMap;
use semver::Version;

use crate::{
    app::AppContext,
    git::Ref,
    widget::style::{mark_selected_row, panel_block, panel_title},
};

const TREE_LOCAL_CHANGES_ROOT_IDENT: &str = "__local_changes__";
const TREE_BRANCH_ROOT_IDENT: &str = "__branches__";
const TREE_REMOTE_ROOT_IDENT: &str = "__remotes__";
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
}

impl RefListState {
    pub fn new(refs: &[&Ref], local_change_count: usize) -> Self {
        let selected = vec![TREE_LOCAL_CHANGES_ROOT_IDENT.into()];
        let opened = HashSet::from([vec![TREE_BRANCH_ROOT_IDENT.into()]]);
        let mut state = Self {
            roots: build_ref_tree_nodes(refs, local_change_count),
            visible_rows: Vec::new(),
            selected,
            opened,
            offset: 0,
            scroll_to_selection: true,
            picked: RefPicks::default(),
            tracking: FxHashMap::default(),
        };
        state.rebuild_visible_rows();
        state
    }

    // The commits ahead of and behind the upstream of each local branch, shown beside them.
    pub fn set_tracking(&mut self, tracking: FxHashMap<String, (usize, usize)>) {
        self.tracking = tracking;
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
        self.selected = vec![TREE_BRANCH_ROOT_IDENT.into()];
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
                .any(|row| row.identifier == self.selected && !row.has_children);
        match self.selected.last() {
            Some(name) if is_ref => RefSelection::Ref(section, name.clone()),
            Some(path) if self.selected.len() > 1 => RefSelection::Folder(section, path.clone()),
            _ => RefSelection::Section(section),
        }
    }

    pub fn is_local_changes_selected(&self) -> bool {
        self.selected.len() == 1 && self.selected[0] == TREE_LOCAL_CHANGES_ROOT_IDENT
    }

    pub fn selected_branch(&self) -> Option<String> {
        if self.selected.len() > 1
            && (self.selected[0] == TREE_BRANCH_ROOT_IDENT
                || self.selected[0] == TREE_REMOTE_ROOT_IDENT)
        {
            self.selected.last().cloned()
        } else {
            None
        }
    }

    pub fn selected_tag(&self) -> Option<String> {
        if self.selected.len() > 1 && self.selected[0] == TREE_TAG_ROOT_IDENT {
            self.selected.last().cloned()
        } else {
            None
        }
    }

    // Shows the refs of the reloaded repository, keeping the selection, the opened nodes and the
    // picked refs still there.
    pub fn set_refs(&mut self, refs: &[&Ref], local_change_count: usize) {
        let (selected, opened) = self.current_tree_status();
        let offset = self.offset;
        let mut picked = std::mem::take(&mut self.picked);
        *self = Self::new(refs, local_change_count);
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
            separator: false,
        });
        if parent.is_empty() && node.identifier == TREE_LOCAL_CHANGES_ROOT_IDENT {
            // divides the working tree from the refs, and is skipped when moving the selection
            rows.push(VisibleRefRow {
                identifier: Vec::new(),
                name: String::new(),
                depth: 0,
                has_children: false,
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
        let block = panel_block(self.focused, theme)
            .title(panel_title(label, &picked, self.focused, theme))
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        ratatui::widgets::Widget::render(block, area, buf);
        if self.collapsed || inner.width == 0 || inner.height == 0 {
            return;
        }

        let height = inner.height as usize;
        state.offset = state.offset.min(state.visible_rows.len().saturating_sub(1));
        if state.scroll_to_selection {
            if let Some(index) = state.selected_index() {
                if index < state.offset {
                    state.offset = index;
                } else if index >= state.offset + height {
                    state.offset = index + 1 - height;
                }
            }
            state.scroll_to_selection = false;
        }

        let item_style = Style::default().fg(theme.fg);
        // the sections stand out from the refs in them
        let root_style = item_style.add_modifier(Modifier::BOLD);

        for (line, row) in state
            .visible_rows
            .iter()
            .skip(state.offset)
            .take(height)
            .enumerate()
        {
            let y = inner.y + line as u16;
            if row.separator {
                let divider_style = Style::default().fg(self.ctx.color_theme.divider_fg);
                let line = "─".repeat(inner.width as usize);
                buf.set_stringn(inner.x, y, line, inner.width as usize, divider_style);
                continue;
            }
            let symbol = if !row.has_children {
                "  "
            } else if state.opened.contains(&row.identifier) {
                "▾ "
            } else {
                "▸ "
            };
            let prefix = format!("{}{symbol}", "  ".repeat(row.depth));
            let symbol_style = Style::default().fg(theme.status_input_transient_fg);
            let (text_x, _) =
                buf.set_stringn(inner.x, y, prefix, inner.width as usize, symbol_style);
            let remaining = inner.width.saturating_sub(text_x - inner.x);
            let picked = !row.has_children
                && row.identifier.len() > 1
                && RefSection::of_root(&row.identifier[0]).is_some_and(|section| {
                    state
                        .picked
                        .contains(section, &row.identifier[row.identifier.len() - 1])
                });
            let style = if picked {
                Style::default()
                    .fg(theme.list_picked_fg)
                    .bg(theme.list_picked_bg)
            } else if row.depth == 0 {
                root_style
            } else {
                item_style
            };
            // a local branch: the commits to push and to pull, after its name
            let tracking = (row.depth > 0 && row.identifier[0] == TREE_BRANCH_ROOT_IDENT)
                .then(|| {
                    state
                        .tracking
                        .get(&row.identifier[row.identifier.len() - 1])
                })
                .flatten();
            let mut counts = vec![];
            if let Some(&(ahead, behind)) = tracking {
                if ahead > 0 {
                    counts.push((format!("↑{ahead}"), theme.status_success_fg));
                }
                if behind > 0 {
                    counts.push((format!("↓{behind}"), theme.status_warn_fg));
                }
            }
            let counts_width: usize = counts.iter().map(|(c, _)| c.chars().count() + 1).sum();
            let name_width = (remaining as usize).saturating_sub(counts_width);
            let (mut x, _) = buf.set_stringn(text_x, y, &row.name, name_width, style);
            for (count, fg) in counts {
                let width = inner.right().saturating_sub(x + 1) as usize;
                (x, _) = buf.set_stringn(x + 1, y, count, width, Style::default().fg(fg));
            }
            // the selection only shows while moving in the sidebar
            if self.focused && row.identifier == state.selected {
                mark_selected_row(buf, inner, y, true, theme);
            }
        }
    }
}

fn build_ref_tree_nodes(refs: &[&Ref], local_change_count: usize) -> Vec<RefTreeNode> {
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
    let mut remote_nodes = refs_to_ref_tree_nodes(remote_refs);
    let mut tag_nodes = refs_to_ref_tree_nodes(tag_refs);
    let mut stash_nodes = refs_to_stash_ref_tree_nodes(stash_refs);

    sort_branch_tree_nodes(&mut branch_nodes);
    sort_branch_tree_nodes(&mut remote_nodes);
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
}

impl RefTreeNode {
    fn new(identifier: String, name: String, children: Vec<Self>) -> Self {
        Self {
            identifier,
            name,
            children,
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
        };
        nodes.push(node);
    }
    nodes
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
    use super::*;

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
        let mut state = RefListState::new(&refs, 0);
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
        state.set_refs(&[&a, &tag], 0);
        assert_eq!(state.picked().names, ["a"]);
        state.clear_picks();
        state.toggle_pick().unwrap();
        assert_eq!(state.picked().section, RefSection::Tags);
    }
}
