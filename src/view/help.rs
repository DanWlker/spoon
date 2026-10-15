use std::rc::Rc;

use ratatui::{
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Stylize},
    text::{Line, Span},
    widgets::{Block, Padding, Paragraph},
    Frame,
};

use crate::{
    app::AppContext,
    color::ColorTheme,
    config::CoreConfig,
    event::{AppEvent, Sender, UserEvent, UserEventWithCount},
    keybind::KeyBind,
    view::View,
};

#[derive(Debug)]
pub struct HelpView<'a> {
    before: View<'a>,

    help_key_lines: Vec<Line<'static>>,
    help_value_lines: Vec<Line<'static>>,
    help_key_line_max_width: u16,

    offset: usize,
    height: usize,

    tx: Sender,
}

impl HelpView<'_> {
    pub fn new<'a>(before: View<'a>, ctx: Rc<AppContext>, tx: Sender) -> HelpView<'a> {
        let (help_key_lines, help_value_lines) =
            build_lines(&ctx.color_theme, &ctx.keybind, &ctx.core_config);
        let help_key_line_max_width = help_key_lines
            .iter()
            .map(|line| line.width())
            .max()
            .unwrap_or_default() as u16;
        HelpView {
            before,
            help_key_lines,
            help_value_lines,
            help_key_line_max_width,
            offset: 0,
            height: 0,
            tx,
        }
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, _: KeyEvent) {
        let event = event_with_count.event;
        let count = event_with_count.count;

        match event {
            UserEvent::Quit => {
                self.tx.send(AppEvent::Quit);
            }
            UserEvent::HelpToggle | UserEvent::Cancel | UserEvent::Close => {
                self.tx.send(AppEvent::CloseHelp);
            }
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                for _ in 0..count {
                    self.scroll_down();
                }
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                for _ in 0..count {
                    self.scroll_up();
                }
            }
            UserEvent::PageDown => {
                for _ in 0..count {
                    self.scroll_page_down();
                }
            }
            UserEvent::PageUp => {
                for _ in 0..count {
                    self.scroll_page_up();
                }
            }
            UserEvent::HalfPageDown => {
                for _ in 0..count {
                    self.scroll_half_page_down();
                }
            }
            UserEvent::HalfPageUp => {
                for _ in 0..count {
                    self.scroll_half_page_up();
                }
            }
            UserEvent::GoToTop => {
                self.select_first();
            }
            UserEvent::GoToBottom => {
                self.select_last();
            }
            _ => {}
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        self.update_state(area);

        let [mut key_area, mut value_area] =
            Layout::horizontal([Constraint::Percentage(30), Constraint::Percentage(70)])
                .areas(area);

        if key_area.width - 4 /* padding */ < self.help_key_line_max_width {
            [key_area, value_area] = Layout::horizontal([
                Constraint::Length(self.help_key_line_max_width + 4),
                Constraint::Min(0),
            ])
            .areas(area);
        }

        let key_lines: Vec<Line> = self
            .help_key_lines
            .iter()
            .skip(self.offset)
            .take(area.height as usize)
            .cloned()
            .collect();
        let value_lines: Vec<Line> = self
            .help_value_lines
            .iter()
            .skip(self.offset)
            .take(area.height as usize)
            .cloned()
            .collect();

        let key_paragraph = Paragraph::new(key_lines)
            .block(Block::default().padding(Padding::new(3, 1, 0, 0)))
            .right_aligned();
        let value_paragraph = Paragraph::new(value_lines)
            .block(Block::default().padding(Padding::new(1, 3, 0, 0)))
            .left_aligned();

        f.render_widget(key_paragraph, key_area);
        f.render_widget(value_paragraph, value_area);
    }
}

impl<'a> HelpView<'a> {
    pub fn take_before_view(&mut self) -> View<'a> {
        std::mem::take(&mut self.before)
    }

    pub fn graph_image_ids_sorted(&self) -> Vec<u32> {
        self.before.graph_image_ids_sorted()
    }

    fn scroll_down(&mut self) {
        self.offset = self.offset.saturating_add(1);
    }

    fn scroll_up(&mut self) {
        self.offset = self.offset.saturating_sub(1);
    }

    fn scroll_page_down(&mut self) {
        self.offset = self.offset.saturating_add(self.height);
    }

    fn scroll_page_up(&mut self) {
        self.offset = self.offset.saturating_sub(self.height);
    }

    fn scroll_half_page_down(&mut self) {
        self.offset = self.offset.saturating_add(self.height / 2);
    }

    fn scroll_half_page_up(&mut self) {
        self.offset = self.offset.saturating_sub(self.height / 2);
    }

    fn select_first(&mut self) {
        self.offset = 0;
    }

    fn select_last(&mut self) {
        self.offset = usize::MAX;
    }

    fn update_state(&mut self, area: Rect) {
        self.height = area.height as usize;
        self.offset = self.offset.min(self.help_key_lines.len() - 1)
    }
}

#[rustfmt::skip]
fn build_lines(
    color_theme: &ColorTheme,
    keybind: &KeyBind,
    core_config: &CoreConfig,
) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
    let user_command_help_items = keybind
        .user_command_event_numbers()
        .into_iter()
        .flat_map(|n| {
            core_config
                .user_command
                .commands
                .get(&n.to_string())
                .map(|c| format!("Execute user command {} - {}", n, c.name))
                .map(|desc| (vec![UserEvent::UserCommand(n)], desc))
        })
        .collect::<Vec<_>>();

    let common_helps = vec![
        (vec![UserEvent::ForceQuit, UserEvent::Quit], "Quit app".into()),
        (vec![UserEvent::HelpToggle], "Open help".into()),
        (vec![UserEvent::RefList], "Switch between local changes and the commit graph".into()),
        (vec![UserEvent::FocusPanel(0)], "Focus the refs sidebar, again to shrink/grow it".into()),
        (vec![UserEvent::ResizeLeft, UserEvent::ResizeRight], "Move the border beside the focused panel (or drag it)".into()),
        (vec![UserEvent::ResizeUp, UserEvent::ResizeDown], "Move the border above or under the focused panel (or drag it)".into()),
        (vec![UserEvent::FocusLeft, UserEvent::FocusRight, UserEvent::FocusUp, UserEvent::FocusDown], "Focus the panel beside, above or under the focused one".into()),
        (vec![UserEvent::KeysToggle], "Show/hide the keys of the focused panel".into()),
        (vec![UserEvent::Fetch], "Fetch from a remote".into()),
        (vec![UserEvent::FetchAll], "Fetch all remotes".into()),
        (vec![UserEvent::Pull], "Pull a branch of a remote".into()),
        (vec![UserEvent::Push], "Push a branch to a remote".into()),
    ];
    let (common_key_lines, common_value_lines) = build_block_lines("Common:", common_helps, color_theme, keybind);

    let help_helps = vec![
        (vec![UserEvent::HelpToggle, UserEvent::Cancel, UserEvent::Close], "Close help".into()),
        (vec![UserEvent::NavigateDown, UserEvent::SelectDown], "Scroll down".into()),
        (vec![UserEvent::NavigateUp, UserEvent::SelectUp], "Scroll up".into()),
        (vec![UserEvent::PageDown], "Scroll page down".into()),
        (vec![UserEvent::PageUp], "Scroll page up".into()),
        (vec![UserEvent::HalfPageDown], "Scroll half page down".into()),
        (vec![UserEvent::HalfPageUp], "Scroll half page up".into()),
        (vec![UserEvent::GoToTop], "Go to top".into()),
        (vec![UserEvent::GoToBottom], "Go to bottom".into()),
    ];
    let (help_key_lines, help_value_lines) = build_block_lines("Help:", help_helps, color_theme, keybind);

    let mut list_helps = vec![
        (vec![UserEvent::NavigateDown, UserEvent::SelectDown], "Move down".into()),
        (vec![UserEvent::NavigateUp, UserEvent::SelectUp], "Move up".into()),
        (vec![UserEvent::GoToTop], "Go to top".into()),
        (vec![UserEvent::GoToBottom], "Go to bottom".into()),
        (vec![UserEvent::PageDown], "Scroll page down".into()),
        (vec![UserEvent::PageUp], "Scroll page up".into()),
        (vec![UserEvent::HalfPageDown], "Scroll half page down".into()),
        (vec![UserEvent::HalfPageUp], "Scroll half page up".into()),
        (vec![UserEvent::ScrollDown], "Scroll down".into()),
        (vec![UserEvent::ScrollUp], "Scroll up".into()),
        (vec![UserEvent::Confirm], "Show commit details".into()),
        (vec![UserEvent::FocusPanel(2)], "Show commit info".into()),
        (vec![UserEvent::FocusPanel(3)], "Show changed files and their diff".into()),
        (vec![UserEvent::FocusPanel(4)], "Show all files of the commit".into()),
        (vec![UserEvent::Search], "Start search".into()),
        (vec![UserEvent::Cancel], "Cancel search / Clear picked commits".into()),
        (vec![UserEvent::ToggleSelect], "Pick/Unpick commit".into()),
        (vec![UserEvent::NextPicked, UserEvent::PreviousPicked], "Go to next/previous picked commit".into()),
        (vec![UserEvent::Diff], "Diff with the checked out commit, or the two picked commits".into()),
        (vec![UserEvent::Rebase], "Rebase the current branch onto the selected commit".into()),
        (vec![UserEvent::Merge], "Merge the selected commit into the current branch".into()),
        (vec![UserEvent::Checkout], "Check out the selected branch or commit".into()),
        (vec![UserEvent::CherryPick], "Cherry-pick the picked commits, or the selected one".into()),
        (vec![UserEvent::Squash], "Squash the picked commits, or the selected one, into their parents".into()),
        (vec![UserEvent::Revert], "Revert the picked commits, or the selected one".into()),
        (vec![UserEvent::Reset], "Reset the current branch to the selected commit".into()),
        (vec![UserEvent::AmendCommit], "Amend the selected commit on the local changes page".into()),
        (vec![UserEvent::DropCommit], "Drop the picked commits, or the selected one".into()),
        (vec![UserEvent::CreateBranch], "Create a branch at the selected commit".into()),
        (vec![UserEvent::CreateTag], "Create a tag at the selected commit".into()),
        (vec![UserEvent::BranchOnly], "Show only the commits of the current branch, or all".into()),
        (vec![UserEvent::GoToNext], "Go to next search match".into()),
        (vec![UserEvent::GoToPrevious], "Go to previous search match".into()),
        (vec![UserEvent::SearchTargetToggle], "Toggle search target".into()),
        (vec![UserEvent::IgnoreCaseToggle], "Toggle ignore case".into()),
        (vec![UserEvent::FuzzyToggle], "Toggle fuzzy match".into()),
        (vec![UserEvent::Refresh], "Refresh".into()),
        (vec![UserEvent::ShortCopy], "Copy commit short hash".into()),
        (vec![UserEvent::FullCopy], "Copy commit hash".into()),
    ];
    list_helps.extend(user_command_help_items.clone());
    let (list_key_lines, list_value_lines) = build_block_lines("Commit List:", list_helps, color_theme, keybind);
    
    let mut detail_helps = vec![
        (vec![UserEvent::Cancel, UserEvent::Close], "Clear picked commits, then close commit details".into()),
        (vec![UserEvent::Confirm], "Close commit details".into()),
        (vec![UserEvent::FocusPanel(1)], "Focus the commit list, the details follow the selection".into()),
        (vec![UserEvent::FocusPanel(2)], "Show commit info".into()),
        (vec![UserEvent::FocusPanel(3)], "Show changed files and their diff".into()),
        (vec![UserEvent::FocusPanel(4)], "Show all files of the commit with a preview".into()),
        (vec![UserEvent::NextTab, UserEvent::PreviousTab], "Show the next/previous tab (while a tab has the focus)".into()),
        (vec![UserEvent::NavigateRight, UserEvent::NavigateLeft], "Expand/Collapse directory (changes, files)".into()),
        (vec![UserEvent::Search], "Search files (changes, files); the diff while it has the focus".into()),
        (vec![UserEvent::Confirm], "Changes: focus the diff, select hunks, then lines".into()),
        (vec![UserEvent::ApplyChange], "Apply the selected hunk or line to the working tree".into()),
        (vec![UserEvent::Filter], "Filter files (changes, files)".into()),
        (vec![UserEvent::GoToNext, UserEvent::GoToPrevious], "Go to next/previous search match".into()),
        (vec![UserEvent::NavigateDown], "Scroll down".into()),
        (vec![UserEvent::NavigateUp], "Scroll up".into()),
        (vec![UserEvent::PageDown], "Scroll page down".into()),
        (vec![UserEvent::PageUp], "Scroll page up".into()),
        (vec![UserEvent::HalfPageDown], "Scroll half page down".into()),
        (vec![UserEvent::HalfPageUp], "Scroll half page up".into()),
        (vec![UserEvent::GoToTop], "Go to top".into()),
        (vec![UserEvent::GoToBottom], "Go to bottom".into()),
        (vec![UserEvent::SelectDown], "Select older commit".into()),
        (vec![UserEvent::SelectUp], "Select newer commit".into()),
        (vec![UserEvent::Refresh], "Refresh".into()),
        (vec![UserEvent::ShortCopy], "Copy commit short hash; the relative path in a file tree".into()),
        (vec![UserEvent::FullCopy], "Copy commit hash; the absolute path in a file tree".into()),
    ];
    detail_helps.extend(user_command_help_items.clone());
    let (detail_key_lines, detail_value_lines) = build_block_lines("Commit Detail:", detail_helps, color_theme, keybind);

    let refs_helps = vec![
        (vec![UserEvent::Cancel, UserEvent::Close], "Back to the page".into()),
        (vec![UserEvent::FocusPanel(1), UserEvent::FocusPanel(2), UserEvent::FocusPanel(3), UserEvent::FocusPanel(4)], "Focus a panel of the page".into()),
        (vec![UserEvent::Confirm], "Open/Close node, or back to the page".into()),
        (vec![UserEvent::NavigateDown, UserEvent::SelectDown], "Move down".into()),
        (vec![UserEvent::NavigateUp, UserEvent::SelectUp], "Move up".into()),
        (vec![UserEvent::GoToTop], "Go to top".into()),
        (vec![UserEvent::GoToBottom], "Go to bottom".into()),
        (vec![UserEvent::NavigateRight], "Open node".into()),
        (vec![UserEvent::NavigateLeft], "Close node".into()),
        (vec![UserEvent::Refresh], "Refresh".into()),
        (vec![UserEvent::ShortCopy], "Copy ref name".into()),
        (vec![UserEvent::CreateBranch], "New branch from the selected branch, remote branch or tag".into()),
        (vec![UserEvent::CreateTag], "New tag at the selected branch, remote branch or tag".into()),
        (vec![UserEvent::CreateRef], "New remote (on the remotes)".into()),
        (vec![UserEvent::DeleteRef], "Delete the branch or tag, drop the stash".into()),
        (vec![UserEvent::EditRef], "Rename the branch or change its upstream; edit the remote".into()),
        (vec![UserEvent::ApplyStash], "Apply the stash; open/close a node elsewhere".into()),
        (vec![UserEvent::Merge], "Merge the branch into the current branch".into()),
        (vec![UserEvent::Rebase], "Rebase the current branch onto the branch".into()),
        (vec![UserEvent::Checkout], "Check out the branch or tag".into()),
        (vec![UserEvent::ToggleSelect], "Pick/Unpick the ref, refs of one kind together".into()),
        (vec![UserEvent::Diff], "Diff the two picked refs, or the current branch with the ref".into()),
    ];
    let (refs_key_lines, refs_value_lines) = build_block_lines("Refs Sidebar:", refs_helps, color_theme, keybind);

    let local_changes_helps = vec![
        (vec![UserEvent::Cancel, UserEvent::Close], "Back, then clear picked files, search, filter".into()),
        (vec![UserEvent::FocusPanel(1)], "Focus unstaged files".into()),
        (vec![UserEvent::FocusPanel(2)], "Focus staged files".into()),
        (vec![UserEvent::FocusPanel(3)], "Type the commit message".into()),
        (vec![UserEvent::Confirm], "Select hunks / lines; Resolve a conflict; Commit".into()),
        (vec![UserEvent::Edit], "Open the file in $VISUAL / $EDITOR".into()),
        (vec![UserEvent::Commit], "Commit after confirming (finishing a merge, cherry-pick or revert), or continue a rebase".into()),
        (vec![UserEvent::ToggleAmend], "Toggle amending the last commit".into()),
        (vec![UserEvent::Abort], "Abort the rebase, merge, cherry-pick or revert in progress".into()),
        (vec![UserEvent::ToggleStage], "Stage/Unstage file, directory, hunk or line, or the picked files".into()),
        (vec![UserEvent::ToggleSelect], "Pick/Unpick file or directory".into()),
        (vec![UserEvent::NextPicked, UserEvent::PreviousPicked], "Go to next/previous picked file".into()),
        (vec![UserEvent::Stash], "Stash all changes, or the picked files".into()),
        (vec![UserEvent::Discard], "Discard unstaged file, directory, hunk or line".into()),
        (vec![UserEvent::NavigateRight, UserEvent::NavigateLeft], "Expand/Collapse directory".into()),
        (vec![UserEvent::Search], "Search files".into()),
        (vec![UserEvent::Filter], "Filter files".into()),
        (vec![UserEvent::GoToNext, UserEvent::GoToPrevious], "Go to next/previous search match".into()),
        (vec![UserEvent::NavigateDown, UserEvent::SelectDown], "Select next file/hunk/line".into()),
        (vec![UserEvent::NavigateUp, UserEvent::SelectUp], "Select previous file/hunk/line".into()),
        (vec![UserEvent::GoToTop], "Select first file/hunk/line".into()),
        (vec![UserEvent::GoToBottom], "Select last file/hunk/line".into()),
        (vec![UserEvent::PageDown], "Scroll diff page down".into()),
        (vec![UserEvent::PageUp], "Scroll diff page up".into()),
        (vec![UserEvent::HalfPageDown], "Scroll diff half page down".into()),
        (vec![UserEvent::HalfPageUp], "Scroll diff half page up".into()),
        (vec![UserEvent::ScrollDown], "Scroll diff down".into()),
        (vec![UserEvent::ScrollUp], "Scroll diff up".into()),
        (vec![UserEvent::Refresh], "Refresh".into()),
        (vec![UserEvent::ShortCopy, UserEvent::FullCopy], "Copy the relative/absolute path of the file".into()),
    ];
    let (local_changes_key_lines, local_changes_value_lines) = build_block_lines("Local Changes:", local_changes_helps, color_theme, keybind);
    
    let dialog_helps = vec![
        (vec![UserEvent::NavigateDown, UserEvent::NavigateUp], "Select next/previous field".into()),
        (vec![UserEvent::NavigateRight, UserEvent::NavigateLeft], "Change the selected field".into()),
        (vec![UserEvent::DialogActivate], "Toggle the field if it can be, else type into it".into()),
        (vec![UserEvent::DialogToggle], "Toggle the checkbox, or go to the next option".into()),
        (vec![UserEvent::DialogEdit], "Type into the text field".into()),
        (vec![UserEvent::Confirm], "Run the command".into()),
        (vec![UserEvent::Cancel, UserEvent::Close], "Cancel".into()),
    ];
    let (dialog_key_lines, dialog_value_lines) = build_block_lines("Git Command Popups:", dialog_helps, color_theme, keybind);

    let mut user_command_helps = vec![
        (vec![UserEvent::Cancel, UserEvent::Close], "Close user command".into()),
        (vec![UserEvent::NavigateDown], "Scroll down".into()),
        (vec![UserEvent::NavigateUp], "Scroll up".into()),
        (vec![UserEvent::PageDown], "Scroll page down".into()),
        (vec![UserEvent::PageUp], "Scroll page up".into()),
        (vec![UserEvent::HalfPageDown], "Scroll half page down".into()),
        (vec![UserEvent::HalfPageUp], "Scroll half page up".into()),
        (vec![UserEvent::GoToTop], "Go to top".into()),
        (vec![UserEvent::GoToBottom], "Go to bottom".into()),
        (vec![UserEvent::SelectDown], "Select older commit".into()),
        (vec![UserEvent::SelectUp], "Select newer commit".into()),
        (vec![UserEvent::Refresh], "Refresh".into()),
        (vec![UserEvent::Confirm], "Show commit details".into()),
    ];
    user_command_helps.extend(user_command_help_items);
    let (user_command_key_lines, user_command_value_lines) = build_block_lines("User Command:", user_command_helps, color_theme, keybind);

    let key_lines = join_line_groups_with_empty(vec![
        common_key_lines,
        help_key_lines,
        list_key_lines,
        detail_key_lines,
        refs_key_lines,
        local_changes_key_lines,
        dialog_key_lines,
        user_command_key_lines,
    ]);
    let value_lines = join_line_groups_with_empty(vec![
        common_value_lines,
        help_value_lines,
        list_value_lines,
        detail_value_lines,
        refs_value_lines,
        local_changes_value_lines,
        dialog_value_lines,
        user_command_value_lines,
    ]);

    (key_lines, value_lines)
}

fn build_block_lines(
    title: &'static str,
    helps: Vec<(Vec<UserEvent>, String)>,
    color_theme: &ColorTheme,
    keybind: &KeyBind,
) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
    let mut key_lines = Vec::new();
    let mut value_lines = Vec::new();

    let key_title_lines = vec![Line::from(title)
        .fg(color_theme.help_block_title_fg)
        .add_modifier(Modifier::BOLD)];
    let value_title_lines = vec![Line::from("")];
    let key_binding_lines: Vec<Line> = helps
        .clone()
        .into_iter()
        .map(|(events, _)| {
            join_span_groups_with_space(
                events
                    .iter()
                    .flat_map(|event| keybind.keys_for_event(*event))
                    .map(|key| vec!["<".into(), key.fg(color_theme.help_key_fg), ">".into()])
                    .collect(),
            )
        })
        .collect();
    let value_binding_lines: Vec<Line> = helps
        .into_iter()
        .map(|(_, value)| Line::raw(value))
        .collect();

    key_lines.extend(key_title_lines);
    key_lines.extend(key_binding_lines);
    value_lines.extend(value_title_lines);
    value_lines.extend(value_binding_lines);

    (key_lines, value_lines)
}

fn join_line_groups_with_empty(line_groups: Vec<Vec<Line<'static>>>) -> Vec<Line<'static>> {
    let mut result = Vec::new();
    let n = line_groups.len();
    for (i, lines) in line_groups.into_iter().enumerate() {
        result.extend(lines);
        if i < n - 1 {
            result.push(Line::raw(""));
        }
    }
    result
}

fn join_span_groups_with_space(span_groups: Vec<Vec<Span<'static>>>) -> Line<'static> {
    let mut spans: Vec<Span> = Vec::new();
    let n = span_groups.len();
    for (i, ss) in span_groups.into_iter().enumerate() {
        spans.extend(ss);
        if i < n - 1 {
            spans.push(Span::raw(" "));
        }
    }
    Line::from(spans)
}
