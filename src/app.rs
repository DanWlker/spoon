use std::{
    cell::RefCell,
    io::{self, Write},
    rc::Rc,
};

use ratatui::{
    backend::Backend,
    crossterm::{
        event::{KeyCode, KeyEvent},
        SynchronizedUpdate,
    },
    layout::{Constraint, Layout, Rect},
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    DefaultTerminal, Frame, Terminal,
};
use rustc_hash::FxHashMap;

use crate::{
    color::{ColorTheme, GraphColorSet},
    config::{CoreConfig, CursorType, UiConfig, UserCommand, UserCommandType},
    event::{AppEvent, EventController, Page, UserEvent, UserEventWithCount},
    external::{
        copy_to_clipboard, exec_interactive, exec_user_command, exec_user_command_suspend,
        validate_marked_hash, ExternalCommandParameters,
    },
    git::{run_task, Commit, CommitHash, FileChange, GitTask, Head, Ref, Repository},
    graph::GraphImageManager,
    keybind::{keys_to_string, KeyBind, SequenceMatch},
    protocol::ImageProtocol,
    search::SearchOptions,
    view::{AppRefreshContext, LocalChangesRefreshViewContext, RefreshViewContext, View},
    widget::{
        commit_list::{CommitInfo, CommitListState},
        commit_tabs::DetailTab,
        git_dialog::{
            CheckoutTarget, DialogCommit, DialogOutcome, GitDialog, GitDialogWidget, RemoteAction,
        },
        local_changes::LocalChangesState,
        ref_list::{RefList, RefListState},
        style::key_hints,
    },
};

#[derive(Debug, Default)]
enum StatusLine {
    #[default]
    None,
    Input {
        message: String,
        cursor_position: u16,
        metadata: String,
    },
    Transient(String),
    SearchResult {
        message: String,
        options: String,
        matched: bool,
    },
    NotificationInfo(String),
    NotificationSuccess(String),
    NotificationWarn(String),
    NotificationError(String),
}

#[derive(Clone, Copy)]
pub enum InitialSelection {
    Latest,
    Head,
}

pub enum Ret {
    Quit,
    Refresh(Box<RefreshRequest>),
}

pub struct RefreshRequest {
    pub context: RefreshViewContext,
    pub app: AppRefreshContext,
}

#[derive(Debug)]
pub struct AppContext {
    pub keybind: KeyBind,
    pub core_config: CoreConfig,
    pub ui_config: UiConfig,
    pub color_theme: ColorTheme,
    pub image_protocol: ImageProtocol,
    // what the git command running in the background is doing, kept across refreshes
    pub running_task: RefCell<Option<String>>,
}

#[derive(Debug, Default)]
enum PendingClear {
    #[default]
    None,
    ViewImages,
    Screen,
}

#[derive(Debug, Default)]
struct AppStatus {
    status_line: StatusLine,
    numeric_prefix: String,
    // the keys pressed so far of a key sequence, like "]" of "]m"
    pending_keys: Vec<KeyEvent>,
    view_area: Rect,
    pending_clear: PendingClear,
}

#[derive(Debug)]
pub struct App<'a> {
    repository: &'a Repository,
    view: View<'a>,
    // the refs on the left, always shown beside the page (but the help)
    sidebar: RefListState,
    sidebar_focused: bool,
    // shrunk to its title, by pressing its number again
    sidebar_collapsed: bool,
    // the local changes page while the graph is shown, to come back to it as it was
    parked_local_changes: Option<LocalChangesRefreshViewContext>,
    // the tab of the commit details while the local changes page is shown, to reopen them
    parked_detail_tab: Option<DetailTab>,
    // the popup confirming a git command, over any view
    dialog: Option<GitDialog>,
    app_status: AppStatus,
    ctx: Rc<AppContext>,
    ec: &'a EventController,
}

impl<'a> App<'a> {
    pub fn new(
        repository: &'a Repository,
        graph_image_manager: GraphImageManager<'a>,
        graph_color_set: &'a GraphColorSet,
        initial_selection: InitialSelection,
        ctx: Rc<AppContext>,
        ec: &'a EventController,
        refresh: Option<Box<RefreshRequest>>,
    ) -> Self {
        let (refresh_view_context, app_context) = match refresh {
            Some(request) => (Some(request.context), Some(request.app)),
            None => (None, None),
        };
        let graph = graph_image_manager.graph();
        let mut ref_name_to_commit_index_map = FxHashMap::default();
        let commits = graph
            .commits
            .iter()
            .enumerate()
            .map(|(i, commit)| {
                let refs = repository.refs(&commit.commit_hash);
                for r in &refs {
                    ref_name_to_commit_index_map.insert(r.name(), i);
                }
                let (pos_x, _) = graph.commit_pos_map[&commit.commit_hash];
                let graph_color = graph_color_set.get(pos_x).to_ratatui_color();
                CommitInfo::new(commit, refs, graph_color)
            })
            .collect();
        let graph_cell_width = graph_image_manager.graph_cell_width();
        let head = repository.head();
        let mut commit_list_state = CommitListState::new(
            commits,
            graph_image_manager,
            graph_cell_width,
            head,
            ref_name_to_commit_index_map,
            SearchOptions {
                target: ctx.core_config.search.target,
                ignore_case: ctx.core_config.search.ignore_case,
                fuzzy: ctx.core_config.search.fuzzy,
            },
            ctx.ui_config.list.scrolloff as usize,
        );
        if let InitialSelection::Head = initial_selection {
            match repository.head() {
                Head::Branch { name } => commit_list_state.select_ref(name),
                Head::Detached { target } => commit_list_state.select_commit_hash(target),
                Head::None => {}
            }
        }
        let view = View::of_list(commit_list_state, ctx.clone(), ec.sender());

        let mut sidebar =
            RefListState::new(&repository.all_refs(), repository.local_changes().count());
        let mut sidebar_focused = false;
        let mut sidebar_collapsed = false;
        let mut parked_local_changes = None;
        let mut parked_detail_tab = None;
        if let Some(context) = app_context {
            sidebar.reset_tree_status(context.sidebar_selected, context.sidebar_opened);
            sidebar_focused = context.sidebar_focused;
            sidebar_collapsed = context.sidebar_collapsed;
            parked_local_changes = context.local_changes;
            parked_detail_tab = context.detail_tab;
        }

        let mut app = Self {
            repository,
            view,
            sidebar,
            sidebar_focused,
            sidebar_collapsed,
            parked_local_changes,
            parked_detail_tab,
            dialog: None,
            app_status: AppStatus::default(),
            ctx,
            ec,
        };

        if let Some(warning) = &graph.warning {
            app.warn_notification(warning.clone());
        }

        match refresh_view_context {
            Some(context) => app.init_with_context(context),
            // start on the local changes page; the graph is one Tab away
            None => app.show_local_changes_page(),
        }
        app.sync_sidebar_focus();

        app
    }
}

impl App<'_> {
    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<Ret, std::io::Error> {
        self.app_status.pending_clear = PendingClear::Screen;

        loop {
            self.prepare_render(terminal)?;
            // Keep image deletion, uploads, and text updates in the same frame.
            // Preparation and event handling stay outside the synchronized update.
            io::stdout().sync_update(|_| self.draw(terminal))??;
            match self.ec.recv() {
                AppEvent::Key(key) => {
                    match self.app_status.status_line {
                        StatusLine::None | StatusLine::Input { .. } => {
                            // do nothing
                        }
                        StatusLine::Transient(_)
                        | StatusLine::SearchResult { .. }
                        | StatusLine::NotificationInfo(_)
                        | StatusLine::NotificationSuccess(_)
                        | StatusLine::NotificationWarn(_) => {
                            // Clear message and pass key input as is
                            self.clear_status_line();
                        }
                        StatusLine::NotificationError(_) => {
                            // Clear message and cancel key input
                            self.clear_status_line();
                            continue;
                        }
                    }

                    if self.dialog.is_some() {
                        self.handle_dialog_key(key);
                        continue;
                    }

                    if self.view.is_capturing_keys() {
                        if let Some(UserEvent::ForceQuit) = self.ctx.keybind.get(&key) {
                            self.ec.send(AppEvent::Quit);
                        } else {
                            let event = UserEventWithCount::from_event(UserEvent::Unknown);
                            self.view.handle_event(event, key);
                        }
                        continue;
                    }

                    // key sequences wait for their next key; typed text (the search input) is no sequence,
                    // and a key of the page goes first, like [ cycling the tabs of the details
                    let pages = self.current_pages();
                    let page_key = self.app_status.pending_keys.is_empty()
                        && self.ctx.keybind.page_key(&pages, &key).is_some();
                    if !matches!(self.app_status.status_line, StatusLine::Input { .. }) && !page_key
                    {
                        let had_pending = !self.app_status.pending_keys.is_empty();
                        self.app_status.pending_keys.push(key);
                        match self
                            .ctx
                            .keybind
                            .match_sequence(&self.app_status.pending_keys)
                        {
                            SequenceMatch::Partial => continue,
                            SequenceMatch::Complete(ue) => {
                                self.app_status.pending_keys.clear();
                                let event_with_count = process_numeric_prefix(
                                    &self.app_status.numeric_prefix,
                                    ue,
                                    key,
                                );
                                self.dispatch(event_with_count, key);
                                self.app_status.numeric_prefix.clear();
                                continue;
                            }
                            SequenceMatch::None => {
                                self.app_status.pending_keys.clear();
                                // Esc only cancels the started sequence
                                if had_pending
                                    && self.ctx.keybind.get(&key) == Some(&UserEvent::Cancel)
                                {
                                    self.app_status.numeric_prefix.clear();
                                    continue;
                                }
                                // otherwise the key goes on as usual
                            }
                        }
                    }

                    let user_event = self.ctx.keybind.get_on(&pages, &key).filter(|ue| {
                        // digits continue a count, and without panels to focus they start one (or are typed)
                        match ue {
                            UserEvent::FocusPanel(n) => {
                                self.app_status.numeric_prefix.is_empty()
                                    && !matches!(self.view, View::Help(_))
                                    && (*n == 0 || self.view.has_panels())
                            }
                            _ => true,
                        }
                    });

                    if let Some(UserEvent::Cancel) = user_event {
                        if !self.app_status.numeric_prefix.is_empty() {
                            // Clear numeric prefix and cancel the event
                            self.app_status.numeric_prefix.clear();
                            continue;
                        }
                    }

                    match user_event {
                        Some(UserEvent::ForceQuit) => {
                            self.ec.send(AppEvent::Quit);
                        }
                        Some(ue) => {
                            let event_with_count =
                                process_numeric_prefix(&self.app_status.numeric_prefix, ue, key);
                            self.dispatch(event_with_count, key);
                            self.app_status.numeric_prefix.clear();
                        }
                        None => {
                            if let StatusLine::Input { .. } = self.app_status.status_line {
                                // In input mode, pass all key events to the view
                                // fixme: currently, the only thing that processes key_event is searching the list,
                                //        so this probably works, but it's not the right process...
                                self.app_status.numeric_prefix.clear();
                                self.view.handle_event(
                                    UserEventWithCount::from_event(UserEvent::Unknown),
                                    key,
                                );
                            } else if let KeyCode::Char(c) = key.code {
                                // Accumulate numeric prefix
                                if c.is_ascii_digit()
                                    && (c != '0' || !self.app_status.numeric_prefix.is_empty())
                                {
                                    self.app_status.numeric_prefix.push(c);
                                }
                            }
                        }
                    }
                }
                AppEvent::Resize(w, h) => {
                    let _ = (w, h);
                }
                AppEvent::Quit => {
                    self.cleanup_graph_images()?;
                    return Ok(Ret::Quit);
                }
                AppEvent::OpenDetail(tab) => {
                    self.open_detail(tab);
                    self.app_status.pending_clear = PendingClear::ViewImages;
                }
                AppEvent::CloseDetail => {
                    self.close_detail();
                    self.app_status.pending_clear = PendingClear::Screen;
                }
                AppEvent::OpenUserCommand(n) => {
                    self.open_user_command(n);
                }
                AppEvent::CloseUserCommand => {
                    self.close_user_command();
                    self.app_status.pending_clear = PendingClear::Screen;
                }
                AppEvent::RunExternal(command) => {
                    self.run_external(&command);
                }
                AppEvent::TaskFinished { task, result } => {
                    self.finish_task(task, result);
                }
                AppEvent::OpenHelp => {
                    self.open_help();
                    self.app_status.pending_clear = PendingClear::ViewImages;
                }
                AppEvent::CloseHelp => {
                    self.close_help();
                    self.app_status.pending_clear = PendingClear::Screen;
                }
                AppEvent::SelectOlderCommit => {
                    self.select_older_commit();
                }
                AppEvent::SelectNewerCommit => {
                    self.select_newer_commit();
                }
                AppEvent::SelectParentCommit => {
                    self.select_parent_commit();
                }
                AppEvent::CopyToClipboard { name, value } => {
                    self.copy_to_clipboard(name, value);
                }
                AppEvent::Refresh(context) => {
                    self.cleanup_graph_images()?;
                    let (sidebar_selected, sidebar_opened) = self.sidebar.current_tree_status();
                    let app = AppRefreshContext {
                        sidebar_selected,
                        sidebar_opened,
                        sidebar_focused: self.sidebar_focused,
                        sidebar_collapsed: self.sidebar_collapsed,
                        local_changes: self.parked_local_changes.take(),
                        detail_tab: match &self.view {
                            View::Detail(view) if view.is_comparing() => None,
                            View::Detail(view) => Some(view.tab()),
                            _ => self.parked_detail_tab.take(),
                        },
                    };
                    let request = RefreshRequest { context, app };
                    return Ok(Ret::Refresh(Box::new(request)));
                }
                AppEvent::ClearStatusLine => {
                    self.clear_status_line();
                }
                AppEvent::UpdateStatusInput {
                    message,
                    cursor_position,
                    metadata,
                } => {
                    self.update_status_input(message, cursor_position, metadata);
                }
                AppEvent::UpdateStatusTransient(msg) => {
                    self.update_status_transient(msg);
                }
                AppEvent::UpdateSearchResult {
                    message,
                    options,
                    matched,
                } => {
                    self.update_search_result(message, options, matched);
                }
                AppEvent::NotifyInfo(msg) => {
                    self.info_notification(msg);
                }
                AppEvent::NotifySuccess(msg) => {
                    self.success_notification(msg);
                }
                AppEvent::NotifyWarn(msg) => {
                    self.warn_notification(msg);
                }
                AppEvent::NotifyError(msg) => {
                    self.error_notification(msg);
                }
            }
        }
    }

    fn prepare_render(&mut self, terminal: &mut DefaultTerminal) -> Result<(), std::io::Error> {
        let area: Rect = terminal.size()?.into();
        let [view_area, _] = split_app_areas(area);
        let [_, view_area] = self.split_sidebar(view_area);
        self.update_state(view_area);
        self.view.update_layout(view_area);
        self.view.prepare_graph_uploads();
        Ok(())
    }

    fn draw(&mut self, terminal: &mut DefaultTerminal) -> Result<(), std::io::Error> {
        match self.app_status.pending_clear {
            PendingClear::None => {}
            PendingClear::ViewImages => self.clear_view_image_area(terminal)?,
            PendingClear::Screen => {
                self.ctx.image_protocol.clear();
                clear_fullscreen(terminal)?;
            }
        }
        self.app_status.pending_clear = PendingClear::None;
        self.flush_pending_graph_uploads()?;
        terminal.draw(|f| self.render(f))?;
        Ok(())
    }

    fn flush_pending_graph_uploads(&mut self) -> Result<(), std::io::Error> {
        let uploads = self.view.drain_pending_graph_uploads();
        if uploads.is_empty() {
            return Ok(());
        }

        let mut stdout = io::stdout().lock();
        for upload in uploads {
            stdout.write_all(upload.as_bytes())?;
        }
        stdout.flush()
    }

    fn cleanup_graph_images(&self) -> Result<(), std::io::Error> {
        let image_ids = self.view.graph_image_ids_sorted();
        self.ctx.image_protocol.delete_images(&image_ids)
    }

    fn render(&mut self, f: &mut Frame) {
        let base = Block::default()
            .fg(self.ctx.color_theme.fg)
            .bg(self.ctx.color_theme.bg);
        f.render_widget(base, f.area());

        let [view_area, status_line_area] = split_app_areas(f.area());
        let [sidebar_area, view_area] = self.split_sidebar(view_area);

        self.update_state(view_area);

        if let View::LocalChanges(view) = &self.view {
            self.sidebar
                .set_local_change_count(view.local_change_count());
        }
        // views are replaced while switching pages, they take the focus as they are drawn
        self.sync_sidebar_focus();
        let sidebar = RefList::new(
            self.ctx.clone(),
            self.sidebar_focused,
            self.sidebar_collapsed,
        );
        f.render_stateful_widget(sidebar, sidebar_area, &mut self.sidebar);
        self.view.render(f, view_area);
        if let Some(dialog) = &self.dialog {
            f.render_widget(
                GitDialogWidget::new(dialog, &self.ctx.color_theme),
                view_area,
            );
        }
        self.render_status_line(f, status_line_area);
    }
}

impl App<'_> {
    // A bar after lipgloss's status bar: the page and the branch as badges, then the message,
    // and on the right what runs in the background and the picked commits.
    fn render_status_line(&self, f: &mut Frame, area: Rect) {
        let theme = &self.ctx.color_theme;
        let bar = Style::default()
            .fg(theme.status_bar_fg)
            .bg(theme.status_bar_bg);
        let page = match &self.view {
            _ if self.sidebar_focused => "REFS",
            View::LocalChanges(_) => "CHANGES",
            View::UserCommand(_) => "COMMAND",
            View::Help(_) => "HELP",
            _ => "GRAPH",
        };
        let branch = match self.repository.head() {
            Head::Branch { name } => name.clone(),
            Head::Detached { target } => format!("{} (detached)", target.as_short_hash()),
            Head::None => "no commits".into(),
        };
        let mut left = vec![
            Span::raw(format!(" {page} "))
                .fg(theme.accent_text_fg)
                .bg(theme.accent_fg)
                .bold(),
            Span::raw(format!(" {branch} "))
                .fg(theme.status_bar_fg)
                .bg(theme.divider_fg),
            Span::raw(" "),
        ];
        let left_width: usize = left.iter().map(Span::width).sum();
        let mut right = vec![];

        match &self.app_status.status_line {
            StatusLine::None => {
                // the count and the keys of a key sequence typed so far
                let prefix = format!(
                    "{}{}",
                    self.app_status.numeric_prefix,
                    keys_to_string(&self.app_status.pending_keys)
                );
                left.push(Span::raw(prefix).fg(theme.accent_fg).bold());
            }
            StatusLine::Input {
                message, metadata, ..
            } => {
                left.push(Span::raw(message.clone()).fg(theme.status_bar_fg));
                right.push(Span::raw(format!("{metadata} ")).fg(theme.status_input_transient_fg));
            }
            StatusLine::Transient(msg) => {
                right.push(Span::raw(format!("{msg} ")).fg(theme.status_input_transient_fg));
            }
            StatusLine::SearchResult {
                message,
                options,
                matched,
            } => {
                let span = Span::raw(message.clone());
                left.push(if *matched {
                    span.fg(theme.accent_fg)
                } else {
                    span.fg(theme.status_warn_fg).bold()
                });
                right.push(Span::raw(format!("{options} ")).fg(theme.status_input_transient_fg));
            }
            StatusLine::NotificationInfo(msg) => {
                left.push(Span::raw(msg.clone()).fg(theme.accent_fg));
            }
            StatusLine::NotificationSuccess(msg) => {
                left.push(
                    Span::raw(format!("✓ {msg}"))
                        .fg(theme.status_success_fg)
                        .bold(),
                );
            }
            StatusLine::NotificationWarn(msg) => {
                left.push(
                    Span::raw(format!("! {msg}"))
                        .fg(theme.status_warn_fg)
                        .bold(),
                );
            }
            StatusLine::NotificationError(msg) => {
                left.push(
                    Span::raw(" ERROR ")
                        .fg(theme.accent_text_fg)
                        .bg(theme.status_error_fg)
                        .bold(),
                );
                left.push(Span::raw(format!(" {msg}")).fg(theme.status_error_fg));
            }
        }

        if matches!(self.app_status.status_line, StatusLine::None) {
            if let Some(task) = self.ctx.running_task.borrow().as_ref() {
                right.push(Span::raw(format!("{task}… ")).fg(theme.highlight_fg));
            }
            let picked = self.view.picked_commit_count();
            if picked > 0 {
                let commits = if picked == 1 { "commit" } else { "commits" };
                right.push(
                    Span::raw(format!(" {picked} {commits} picked "))
                        .fg(theme.list_picked_fg)
                        .bg(theme.list_picked_bg)
                        .bold(),
                );
                right.push(Span::raw(" esc to clear ").fg(theme.status_input_transient_fg));
            }
            if right.is_empty() {
                // the keys to find the way around, like bubbles' short help
                let key = |event| {
                    self.ctx
                        .keybind
                        .keys_for_event(event)
                        .into_iter()
                        .next()
                        .unwrap_or_default()
                        .to_lowercase()
                };
                let keys = [
                    (key(UserEvent::RefList), "switch page"),
                    (key(UserEvent::FocusPanel(0)), "refs"),
                    (key(UserEvent::HelpToggle), "help"),
                ];
                let keys: Vec<(&str, &str)> = keys
                    .iter()
                    .filter(|(key, _)| !key.is_empty())
                    .map(|(key, description)| (key.as_str(), *description))
                    .collect();
                right.extend(key_hints(&keys, theme).spans);
                right.push(Span::raw(" "));
            }
        }

        let line = status_bar_line(left, right, area.width);
        f.render_widget(Paragraph::new(line).style(bar), area);

        if let StatusLine::Input {
            cursor_position, ..
        } = &self.app_status.status_line
        {
            let (x, y) = (area.x + left_width as u16 + cursor_position, area.y);
            match &self.ctx.ui_config.common.cursor_type {
                CursorType::Native => {
                    f.set_cursor_position((x, y));
                }
                CursorType::Virtual(cursor) => {
                    let style = Style::default().fg(theme.virtual_cursor_fg);
                    f.buffer_mut().set_string(x, y, cursor, style);
                }
            }
        }
    }
}

impl App<'_> {
    // The sidebar on the left and the page; the help takes the whole area.
    fn split_sidebar(&self, area: Rect) -> [Rect; 2] {
        let width = if matches!(self.view, View::Help(_)) {
            0
        } else if self.sidebar_collapsed {
            COLLAPSED_SIDEBAR_WIDTH.min(area.width)
        } else {
            // leave the page some room on narrow terminals
            self.ctx.ui_config.refs.width.min(area.width / 3)
        };
        Layout::horizontal([Constraint::Length(width), Constraint::Min(0)]).areas(area)
    }
}

// " [0] " and the border on its right
const COLLAPSED_SIDEBAR_WIDTH: u16 = 6;

fn split_app_areas(area: Rect) -> [Rect; 2] {
    Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area)
}

fn clear_fullscreen<B: Backend>(terminal: &mut Terminal<B>) -> Result<(), B::Error> {
    // Terminal::clear queries the cursor position, competing with our input thread.
    // A fullscreen redraw positions the cursor itself, so only clear the screen and
    // reset both diff buffers. Swapping twice preserves the current buffer index.
    terminal.backend_mut().clear()?;
    terminal.swap_buffers();
    terminal.swap_buffers();
    Ok(())
}

impl App<'_> {
    fn update_state(&mut self, view_area: Rect) {
        self.app_status.view_area = view_area;
    }

    fn clear_view_image_area(&self, terminal: &mut DefaultTerminal) -> Result<(), std::io::Error> {
        if let Some(area) = self.view.image_clear_area(self.app_status.view_area) {
            let area = area.intersection(terminal.size()?.into());
            for y in area.top()..area.bottom() {
                self.ctx.image_protocol.clear_line(y);
            }
        }
        Ok(())
    }

    fn open_detail(&mut self, tab: DetailTab) {
        let commit_list_state = match self.view {
            View::List(ref mut view) => view.take_list_state(),
            View::UserCommand(ref mut view) => view.take_list_state(),
            _ => return,
        };
        let (commit, changes, refs) = selected_commit_details(self.repository, &commit_list_state);
        self.view = View::of_detail(
            commit_list_state,
            commit,
            changes,
            refs,
            self.repository,
            self.ctx.clone(),
            self.ec.sender(),
        );
        if let View::Detail(ref mut view) = self.view {
            view.show_tab(tab);
        }
    }

    fn close_detail(&mut self) {
        if let View::Detail(ref mut view) = self.view {
            let commit_list_state = view.take_list_state();
            self.view = View::of_list(commit_list_state, self.ctx.clone(), self.ec.sender());
        }
    }

    fn open_user_command(&mut self, user_command_number: usize) {
        match extract_user_command_by_number(user_command_number, &self.ctx).map(|c| &c.r#type) {
            Ok(UserCommandType::Inline) => {
                self.open_user_command_inline(user_command_number);
                self.app_status.pending_clear = PendingClear::ViewImages;
            }
            Ok(UserCommandType::Silent) => {
                self.open_user_command_silent(user_command_number);
                self.app_status.pending_clear = PendingClear::Screen;
            }
            Ok(UserCommandType::Suspend) => {
                self.open_user_command_suspend(user_command_number);
                self.app_status.pending_clear = PendingClear::Screen;
            }
            Err(err) => {
                self.ec.send(AppEvent::NotifyError(err));
            }
        };
    }

    fn open_user_command_inline(&mut self, user_command_number: usize) {
        let commit_list_state = match self.view {
            View::List(ref mut view) => view.as_list_state(),
            View::Detail(ref mut view) => view.as_list_state(),
            View::UserCommand(ref mut view) => view.as_list_state(),
            _ => return,
        };
        let (commit, _, refs) = selected_commit_details(self.repository, commit_list_state);
        let result = build_external_command_parameters_and_exec_command(
            &commit,
            &refs,
            commit_list_state.picked_commit_hashes(),
            user_command_number,
            self.app_status.view_area,
            &self.ctx,
        );
        match result {
            Ok(output) => {
                // take list state only when the command execution is successful, to avoid losing the state when the command fails
                let commit_list_state = match self.view {
                    View::List(ref mut view) => view.take_list_state(),
                    View::Detail(ref mut view) => view.take_list_state(),
                    View::UserCommand(ref mut view) => view.take_list_state(),
                    _ => return,
                };
                self.view = View::of_user_command(
                    commit_list_state,
                    output,
                    user_command_number,
                    self.ctx.clone(),
                    self.ec.sender(),
                );
            }
            Err(err) => {
                self.ec.send(AppEvent::NotifyError(err));
            }
        };
    }

    fn open_user_command_silent(&mut self, user_command_number: usize) {
        let commit_list_state = match self.view {
            View::List(ref mut view) => view.as_list_state(),
            View::Detail(ref mut view) => view.as_list_state(),
            View::UserCommand(ref mut view) => view.as_list_state(),
            _ => return,
        };
        let (commit, _, refs) = selected_commit_details(self.repository, commit_list_state);
        let result = build_external_command_parameters_and_exec_command(
            &commit,
            &refs,
            commit_list_state.picked_commit_hashes(),
            user_command_number,
            self.app_status.view_area,
            &self.ctx,
        );
        match result {
            Ok(_) => {
                if extract_user_command_refresh_by_number(user_command_number, &self.ctx) {
                    self.view.refresh();
                }
            }
            Err(err) => {
                self.ec.send(AppEvent::NotifyError(err));
            }
        }
    }

    fn open_user_command_suspend(&mut self, user_command_number: usize) {
        let commit_list_state = match self.view {
            View::List(ref mut view) => view.as_list_state(),
            View::Detail(ref mut view) => view.as_list_state(),
            View::UserCommand(ref mut view) => view.as_list_state(),
            _ => return,
        };
        let (commit, _, refs) = selected_commit_details(self.repository, commit_list_state);
        match build_external_command_parameters(
            &commit,
            &refs,
            commit_list_state.picked_commit_hashes(),
            user_command_number,
            self.app_status.view_area,
            &self.ctx,
        ) {
            Ok(params) => {
                self.ec.suspend();
                let exec_result = exec_user_command_suspend(params);
                self.ec.resume();

                if extract_user_command_refresh_by_number(user_command_number, &self.ctx) {
                    self.view.refresh();
                }

                // notify after resuming and refreshing
                if let Err(err) = exec_result {
                    self.ec.send(AppEvent::NotifyError(err));
                }
            }
            Err(err) => {
                self.ec.send(AppEvent::NotifyError(err));
            }
        }
    }

    fn close_user_command(&mut self) {
        if let View::UserCommand(ref mut view) = self.view {
            let commit_list_state = view.take_list_state();
            self.view = View::of_list(commit_list_state, self.ctx.clone(), self.ec.sender());
        }
    }

    fn run_external(&mut self, command: &[String]) {
        self.ec.suspend();
        let result = exec_interactive(command);
        self.ec.resume();
        self.app_status.pending_clear = PendingClear::Screen;
        // the command may have changed files, or resolved conflicts
        self.view.refresh();
        if let Err(err) = result {
            self.ec.send(AppEvent::NotifyError(err));
        }
    }

    // Handles what works on every page (the sidebar, switching pages, fetch, pull and push),
    // else hands the event to the focused sidebar or the page.
    fn dispatch(&mut self, event_with_count: UserEventWithCount, key: KeyEvent) {
        let event = event_with_count.event;
        // keys typed into the search of the list, or over the help, belong to the view
        let typing = matches!(self.app_status.status_line, StatusLine::Input { .. });
        if typing || matches!(self.view, View::Help(_)) {
            self.view.handle_event(event_with_count, key);
            return;
        }
        if self.handle_remote_event(event) {
            return;
        }
        if event.page() == Some(Page::Graph) {
            self.handle_graph_event(event);
            return;
        }
        match event {
            UserEvent::FocusPanel(0) => {
                // again on the focused sidebar, it shrinks out of the way; on a shrunk one, it grows back
                if self.sidebar_focused && !self.sidebar_collapsed {
                    self.sidebar_collapsed = true;
                    self.set_sidebar_focused(false);
                } else {
                    self.sidebar_collapsed = false;
                    self.set_sidebar_focused(true);
                }
                // the page moves, and the graph images with it
                self.app_status.pending_clear = PendingClear::Screen;
                return;
            }
            UserEvent::RefList => {
                self.switch_page();
                return;
            }
            _ => {}
        }
        if self.sidebar_focused {
            self.handle_sidebar_event(event_with_count, key);
        } else {
            self.view.handle_event(event_with_count, key);
        }
    }

    fn handle_sidebar_event(&mut self, event_with_count: UserEventWithCount, key: KeyEvent) {
        let count = event_with_count.count;
        match event_with_count.event {
            UserEvent::Quit => self.ec.send(AppEvent::Quit),
            UserEvent::HelpToggle => self.ec.send(AppEvent::OpenHelp),
            UserEvent::Refresh => self.view.refresh(),
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                (0..count).for_each(|_| self.sidebar.select_next());
                self.show_sidebar_selection();
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                (0..count).for_each(|_| self.sidebar.select_prev());
                self.show_sidebar_selection();
            }
            UserEvent::GoToTop => {
                self.sidebar.select_first();
                self.show_sidebar_selection();
            }
            UserEvent::GoToBottom => {
                self.sidebar.select_last();
                self.show_sidebar_selection();
            }
            UserEvent::NavigateRight => {
                self.sidebar.open_node();
                self.show_sidebar_selection();
            }
            UserEvent::NavigateLeft => {
                self.sidebar.close_node();
                self.show_sidebar_selection();
            }
            UserEvent::Confirm => {
                // a folder opens or closes, else back to the page showing what is selected
                if !self.sidebar.toggle_node() {
                    self.set_sidebar_focused(false);
                }
            }
            UserEvent::Cancel | UserEvent::Close => {
                self.set_sidebar_focused(false);
            }
            UserEvent::ShortCopy | UserEvent::FullCopy => self.copy_sidebar_ref_name(),
            UserEvent::FocusPanel(_) => {
                // a panel of the page
                self.set_sidebar_focused(false);
                self.view.handle_event(event_with_count, key);
            }
            _ => {}
        }
    }

    // The page whose keys apply: none of them while the sidebar has the focus.
    // The pages whose keys apply, the innermost first: none of them while the sidebar has the focus.
    fn current_pages(&self) -> Vec<Page> {
        if self.sidebar_focused {
            return vec![];
        }
        match &self.view {
            View::Detail(view) if !view.is_list_focused() => vec![Page::Details, Page::Graph],
            View::List(_) | View::Detail(_) => vec![Page::Graph],
            View::LocalChanges(_) => vec![Page::LocalChanges],
            _ => vec![],
        }
    }

    fn set_sidebar_focused(&mut self, focused: bool) {
        self.sidebar_focused = focused;
        self.sync_sidebar_focus();
    }

    // The page shows its selection dimmed while the sidebar has the focus.
    fn sync_sidebar_focus(&mut self) {
        self.view.set_focused(!self.sidebar_focused);
    }

    // Shows the page of the selected row: local changes, or the graph at the selected ref.
    fn show_sidebar_selection(&mut self) {
        if self.sidebar.is_local_changes_selected() {
            self.show_local_changes_page();
        } else {
            // the commit details stay open, showing the commit of the ref
            if !matches!(self.view, View::Detail(_)) {
                self.show_graph_page();
            }
            let Some(name) = self.sidebar.selected_ref_name() else {
                return;
            };
            match &mut self.view {
                View::List(view) => view.select_ref(&name),
                View::Detail(view) => view.select_ref(self.repository, &name),
                _ => {}
            }
        }
    }

    // Tab: between the local changes page and the graph, focusing the page.
    fn switch_page(&mut self) {
        if matches!(self.view, View::LocalChanges(_)) {
            self.show_graph_page();
            self.sidebar.select_branches();
        } else {
            self.show_local_changes_page();
            self.sidebar.select_local_changes();
        }
        self.set_sidebar_focused(false);
    }

    fn show_local_changes_page(&mut self) {
        let commit_list_state = match self.view {
            View::List(ref mut view) => view.take_list_state(),
            View::Detail(ref mut view) => {
                // they open again when coming back to the graph, but a comparison
                if !view.is_comparing() {
                    self.parked_detail_tab = Some(view.tab());
                }
                view.take_list_state()
            }
            View::UserCommand(ref mut view) => view.take_list_state(),
            _ => return,
        };
        self.view = View::of_local_changes(
            commit_list_state,
            LocalChangesState::load(self.repository),
            self.ctx.clone(),
            self.ec.sender(),
        );
        if let (Some(context), View::LocalChanges(view)) =
            (self.parked_local_changes.take(), &mut self.view)
        {
            view.reset_local_changes_with(context);
        }
        self.sync_sidebar_focus();
        self.app_status.pending_clear = PendingClear::ViewImages;
    }

    // The commit list, with the details when they were open before the local changes page.
    fn show_graph_page(&mut self) {
        let commit_list_state = match self.view {
            View::LocalChanges(ref mut view) => {
                self.parked_local_changes = Some(view.context());
                view.take_list_state()
            }
            View::Detail(ref mut view) => view.take_list_state(),
            View::UserCommand(ref mut view) => view.take_list_state(),
            _ => return,
        };
        self.view = View::of_list(commit_list_state, self.ctx.clone(), self.ec.sender());
        if let Some(tab) = self.parked_detail_tab.take() {
            self.open_detail(tab);
        }
        self.app_status.pending_clear = PendingClear::Screen;
    }

    fn copy_sidebar_ref_name(&self) {
        let (name, value) = if let Some(branch) = self.sidebar.selected_branch() {
            ("Branch Name", branch)
        } else if let Some(tag) = self.sidebar.selected_tag() {
            ("Tag Name", tag)
        } else {
            return;
        };
        self.ec.send(AppEvent::CopyToClipboard {
            name: name.into(),
            value,
        });
    }

    // Opens the fetch, pull or push popup, or fetches all remotes, for the event bound to them.
    // Returns whether the event was one of them.
    fn handle_remote_event(&mut self, event: UserEvent) -> bool {
        let action = match event {
            UserEvent::Fetch => RemoteAction::Fetch,
            UserEvent::Pull => RemoteAction::Pull,
            UserEvent::Push => RemoteAction::Push,
            UserEvent::FetchAll => {
                let task = GitTask {
                    remote: true,
                    ..GitTask::new(
                        &["fetch", "--all"],
                        "Fetching all remotes".into(),
                        "Fetched all remotes".into(),
                    )
                };
                self.start_task(task);
                return true;
            }
            _ => return false,
        };
        self.open_dialog(GitDialog::remote(action, self.repository.remote_info()));
        true
    }

    fn open_dialog(&mut self, dialog: Result<GitDialog, String>) {
        if let Some(task) = self.ctx.running_task.borrow().as_ref() {
            self.ec.send(AppEvent::NotifyWarn(format!(
                "{task}, wait for it to finish"
            )));
            return;
        }
        match dialog {
            Ok(dialog) => {
                self.dialog = Some(dialog);
                // images of the graph would cover the popup
                self.app_status.pending_clear = PendingClear::Screen;
            }
            Err(err) => self.ec.send(AppEvent::NotifyError(err)),
        }
    }

    fn handle_dialog_key(&mut self, key: KeyEvent) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        // the popups toggle and type like the local changes page
        let event = self.ctx.keybind.get_on(&[Page::LocalChanges], &key);
        if event == Some(UserEvent::ForceQuit) {
            self.ec.send(AppEvent::Quit);
            return;
        }
        match dialog.handle_key(event, key) {
            DialogOutcome::None => {}
            DialogOutcome::Close => self.close_dialog(),
            DialogOutcome::Error(err) => self.ec.send(AppEvent::NotifyError(err)),
            DialogOutcome::Run(task) => {
                self.close_dialog();
                self.start_task(task);
            }
        }
    }

    fn close_dialog(&mut self) {
        self.dialog = None;
        self.app_status.pending_clear = PendingClear::Screen;
    }

    // Runs the git command in the background; the app is told when it finishes.
    fn start_task(&mut self, task: GitTask) {
        if let Some(running) = self.ctx.running_task.borrow().as_ref() {
            self.ec.send(AppEvent::NotifyWarn(format!(
                "{running}, wait for it to finish"
            )));
            return;
        }
        *self.ctx.running_task.borrow_mut() = Some(task.running.clone());
        let path = self.repository.path().to_path_buf();
        let tx = self.ec.sender();
        std::thread::spawn(move || {
            let result = run_task(&path, &task);
            tx.send(AppEvent::TaskFinished { task, result });
        });
    }

    fn finish_task(&mut self, task: GitTask, result: Result<String, String>) {
        *self.ctx.running_task.borrow_mut() = None;
        let typing = self.view.is_capturing_keys()
            || matches!(self.app_status.status_line, StatusLine::Input { .. });
        if result.is_ok() && task.uses_picks {
            self.view.clear_picked_commits();
        }
        // what is left to resolve or commit is on the local changes page
        let to_local_changes = (result.is_ok() && task.show_local_changes)
            || self.repository.operation_in_progress().is_some();
        // the command may have changed files or stopped on conflicts, even when it failed;
        // refreshing would interrupt typing, so then it waits for a manual refresh
        if !typing {
            if to_local_changes && !matches!(self.view, View::LocalChanges(_)) {
                self.show_local_changes_page();
                self.sidebar.select_local_changes();
                self.set_sidebar_focused(false);
            }
            self.view.refresh();
        }
        match result {
            Ok(_) if typing => self.ec.send(AppEvent::NotifySuccess(format!(
                "{} (refresh to see the changes)",
                task.done
            ))),
            Ok(_) => self.ec.send(AppEvent::NotifySuccess(task.done)),
            Err(err) => self.ec.send(AppEvent::NotifyError(err)),
        }
    }

    // The selected commit and the picked ones, on the commit graph.
    fn graph_selection(&self) -> Option<(CommitHash, Vec<CommitHash>)> {
        let state = match &self.view {
            View::List(view) => view.as_list_state(),
            View::Detail(view) => view.as_list_state(),
            _ => return None,
        };
        Some((
            state.selected_commit_hash().clone(),
            state.picked_commit_hashes().to_vec(),
        ))
    }

    fn current_branch(&self) -> Option<String> {
        match self.repository.head() {
            Head::Branch { name } => Some(name.clone()),
            _ => None,
        }
    }

    // The commit as a line in a popup: its short hash and subject.
    fn dialog_commit(&self, hash: &CommitHash) -> DialogCommit {
        let subject = self
            .repository
            .commit(hash)
            .map(|c| c.subject.clone())
            .unwrap_or_default();
        DialogCommit {
            hash: hash.as_str().to_string(),
            line: format!("{} {subject}", hash.as_short_hash()),
        }
    }

    // What to give git for a commit, and how to show it: a local branch at it, else a remote
    // branch, else a tag, else the commit itself.
    fn ref_or_commit(&self, hash: &CommitHash) -> (String, String) {
        let refs = self.repository.refs(hash);
        let find = |f: fn(&Ref) -> bool| refs.iter().find(|r| f(r)).map(|r| r.name().to_string());
        find(|r| matches!(r, Ref::Branch { .. }))
            .or_else(|| find(|r| matches!(r, Ref::RemoteBranch { .. })))
            .or_else(|| find(|r| matches!(r, Ref::Tag { .. })))
            .map(|name| (name.clone(), name))
            .unwrap_or_else(|| {
                let line = self.dialog_commit(hash).line;
                (hash.as_str().to_string(), line)
            })
    }

    // Commits ordered as in the graph, newest first.
    fn sort_newest_first(&self, mut hashes: Vec<CommitHash>) -> Vec<CommitHash> {
        hashes.sort_by_key(|h| self.repository.commit_position(h).unwrap_or(usize::MAX));
        hashes
    }

    // The commands of the commit graph: diff, rebase, merge, checkout, cherry-pick, squash,
    // revert and reset.
    fn handle_graph_event(&mut self, event: UserEvent) {
        let Some((selected, picked)) = self.graph_selection() else {
            return;
        };
        let branch = self.current_branch();
        let branch_label = branch.clone().unwrap_or_else(|| "HEAD".into());
        let single = |name: &str| -> Result<(), String> {
            if picked.is_empty() {
                Ok(())
            } else {
                Err(format!(
                    "{name} works on the selected commit, clear the picked ones first"
                ))
            }
        };
        // the picked commits, else the selected one
        let targets = if picked.is_empty() {
            vec![selected.clone()]
        } else {
            picked.clone()
        };
        let dialog = match event {
            UserEvent::Diff => {
                if let Err(err) = self.open_compare(selected, picked) {
                    self.ec.send(AppEvent::NotifyError(err));
                }
                return;
            }
            UserEvent::Rebase => single("Rebase").and_then(|_| {
                let branch = branch.ok_or("Check out a branch to rebase it")?;
                let (onto, label) = self.ref_or_commit(&selected);
                Ok(GitDialog::rebase(&branch, onto, &label))
            }),
            UserEvent::Merge => single("Merge").and_then(|_| {
                let (source, label) = self.ref_or_commit(&selected);
                if Some(&source) == branch.as_ref() {
                    return Err(format!("{source} is checked out, choose another branch"));
                }
                Ok(GitDialog::merge(&branch_label, source, &label))
            }),
            UserEvent::Checkout => {
                single("Checkout").map(|_| GitDialog::checkout(self.checkout_targets(&selected)))
            }
            UserEvent::CherryPick => {
                let mut commits = self.sort_newest_first(targets);
                commits.reverse();
                let commits = commits.iter().map(|h| self.dialog_commit(h)).collect();
                Ok(GitDialog::cherry_pick(&branch_label, commits))
            }
            UserEvent::Squash => self.squash_dialog(&branch_label, targets),
            UserEvent::Revert => {
                let commits = self.sort_newest_first(targets);
                let commits = commits.iter().map(|h| self.dialog_commit(h)).collect();
                Ok(GitDialog::revert(&branch_label, commits))
            }
            UserEvent::Reset => single("Reset")
                .map(|_| GitDialog::reset(&branch_label, self.dialog_commit(&selected))),
            _ => return,
        };
        self.open_dialog(dialog);
    }

    // The branches at the commit to check out, and the commit itself.
    fn checkout_targets(&self, hash: &CommitHash) -> Vec<CheckoutTarget> {
        let current = self.current_branch();
        let all_refs = self.repository.all_refs();
        let local_exists = |name: &str| {
            all_refs
                .iter()
                .any(|r| matches!(r, Ref::Branch { .. }) && r.name() == name)
        };
        let mut targets = vec![];
        for r in self.repository.refs(hash) {
            match r {
                Ref::Branch { name, .. } if Some(name) != current.as_ref() => {
                    targets.push(CheckoutTarget {
                        label: name.clone(),
                        args: vec![name.clone()],
                    });
                }
                Ref::RemoteBranch { name, .. } => {
                    // a new local branch tracking it, unless there is one by that name
                    let Some((_, local)) = name.split_once('/') else {
                        continue;
                    };
                    if local != "HEAD" && !local_exists(local) {
                        targets.push(CheckoutTarget {
                            label: format!("{local} (new, tracking {name})"),
                            args: vec!["--track".into(), name.clone()],
                        });
                    }
                }
                _ => {}
            }
        }
        targets.push(CheckoutTarget {
            label: format!("{} (detached HEAD)", hash.as_short_hash()),
            args: vec!["--detach".into(), hash.as_str().into()],
        });
        targets
    }

    // Squashing rebases from the parent of the oldest commit's parent, so the history from there
    // must be a line on the current branch.
    fn squash_dialog(&self, branch: &str, commits: Vec<CommitHash>) -> Result<GitDialog, String> {
        let commits = self.sort_newest_first(commits);
        for hash in &commits {
            let short = hash.as_short_hash();
            if !self.repository.is_ancestor(hash, "HEAD") {
                return Err(format!("{short} is not on {branch}, check it out first"));
            }
            match self.repository.parents_hash(hash).len() {
                0 => return Err(format!("{short} is the first commit, it has no parent")),
                1 => {}
                _ => return Err(format!("{short} is a merge commit, it can't be squashed")),
            }
        }
        let oldest = commits.last().ok_or("No commit to squash")?;
        let parent = self.repository.parents_hash(oldest)[0].clone();
        let base = self
            .repository
            .parents_hash(&parent)
            .first()
            .map(|h| (*h).clone());
        if self.repository.has_merges_between(base.as_ref(), "HEAD") {
            return Err("There are merge commits after them, which squashing would flatten".into());
        }
        let commits = commits.iter().map(|h| self.dialog_commit(h)).collect();
        let base = base.map(|h| h.as_str().to_string());
        Ok(GitDialog::squash(branch, commits, base))
    }

    // Shows the changes between two commits in the details: the checked out commit and the
    // selected (or picked) one, or the two picked ones; the older one is the base.
    fn open_compare(
        &mut self,
        selected: CommitHash,
        picked: Vec<CommitHash>,
    ) -> Result<(), String> {
        let head = || {
            self.repository
                .head_commit()
                .ok_or("Nothing is checked out")
        };
        let (a, b) = match picked.as_slice() {
            [] => (head()?, selected),
            [one] => (head()?, one.clone()),
            [one, two] => (one.clone(), two.clone()),
            more => {
                return Err(format!(
                    "Diff compares two commits, {} are picked",
                    more.len()
                ))
            }
        };
        if a == b {
            return Err("That commit is checked out, there is nothing to compare".into());
        }
        let [target, base]: [CommitHash; 2] = self
            .sort_newest_first(vec![a, b])
            .try_into()
            .expect("two commits");
        let title = format!("Diff {} → {}", base.as_short_hash(), target.as_short_hash());

        let commit_list_state = match self.view {
            View::List(ref mut view) => view.take_list_state(),
            View::Detail(ref mut view) => view.take_list_state(),
            _ => return Ok(()),
        };
        let (commit, changes) = self.repository.commit_detail(&target);
        let refs = self.repository.refs(&target).into_iter().cloned().collect();
        self.view = View::of_detail(
            commit_list_state,
            commit,
            changes,
            refs,
            self.repository,
            self.ctx.clone(),
            self.ec.sender(),
        );
        if let View::Detail(ref mut view) = self.view {
            view.start_compare(self.repository, base, target, title);
        }
        self.app_status.pending_clear = PendingClear::ViewImages;
        Ok(())
    }

    fn open_help(&mut self) {
        let before_view = std::mem::take(&mut self.view);
        self.view = View::of_help(before_view, self.ctx.clone(), self.ec.sender());
    }

    fn close_help(&mut self) {
        if let View::Help(ref mut view) = self.view {
            self.view = view.take_before_view();
        }
    }

    fn select_older_commit(&mut self) {
        if let View::Detail(ref mut view) = self.view {
            view.select_older_commit(self.repository);
        } else if let View::UserCommand(ref mut view) = self.view {
            view.select_older_commit(
                self.repository,
                self.app_status.view_area,
                build_external_command_parameters_and_exec_command,
            );
        }
    }

    fn select_newer_commit(&mut self) {
        if let View::Detail(ref mut view) = self.view {
            view.select_newer_commit(self.repository);
        } else if let View::UserCommand(ref mut view) = self.view {
            view.select_newer_commit(
                self.repository,
                self.app_status.view_area,
                build_external_command_parameters_and_exec_command,
            );
        }
    }

    fn select_parent_commit(&mut self) {
        if let View::Detail(ref mut view) = self.view {
            view.select_parent_commit(self.repository);
        } else if let View::UserCommand(ref mut view) = self.view {
            view.select_parent_commit(
                self.repository,
                self.app_status.view_area,
                build_external_command_parameters_and_exec_command,
            );
        }
    }

    fn init_with_context(&mut self, context: RefreshViewContext) {
        if let View::List(ref mut view) = self.view {
            view.reset_commit_list_with(context.list_context());
        }
        match context {
            RefreshViewContext::List { .. } => {}
            RefreshViewContext::Detail { .. } => {
                let tab = self.parked_detail_tab.take().unwrap_or_default();
                self.open_detail(tab);
            }
            RefreshViewContext::UserCommand {
                user_command_context,
                ..
            } => {
                self.open_user_command(user_command_context.n);
            }
            RefreshViewContext::LocalChanges {
                local_changes_context,
                ..
            } => {
                self.show_local_changes_page();
                if let View::LocalChanges(ref mut view) = self.view {
                    view.reset_local_changes_with(local_changes_context);
                }
            }
        }
    }

    fn clear_status_line(&mut self) {
        self.app_status.status_line = StatusLine::None;
    }

    fn update_status_input(&mut self, message: String, cursor_position: u16, metadata: String) {
        self.app_status.status_line = StatusLine::Input {
            message,
            cursor_position,
            metadata,
        };
    }

    fn update_status_transient(&mut self, msg: String) {
        self.app_status.status_line = StatusLine::Transient(msg);
    }

    fn update_search_result(&mut self, message: String, options: String, matched: bool) {
        self.app_status.status_line = StatusLine::SearchResult {
            message,
            options,
            matched,
        };
    }

    fn info_notification(&mut self, msg: String) {
        self.app_status.status_line = StatusLine::NotificationInfo(msg);
    }

    fn success_notification(&mut self, msg: String) {
        self.app_status.status_line = StatusLine::NotificationSuccess(msg);
    }

    fn warn_notification(&mut self, msg: String) {
        self.app_status.status_line = StatusLine::NotificationWarn(msg);
    }

    fn error_notification(&mut self, msg: String) {
        self.app_status.status_line = StatusLine::NotificationError(msg);
    }

    fn copy_to_clipboard(&self, name: String, value: String) {
        match copy_to_clipboard(value, &self.ctx.core_config.external.clipboard) {
            Ok(_) => {
                let msg = format!("Copied {name} to clipboard successfully");
                self.ec.send(AppEvent::NotifySuccess(msg));
            }
            Err(msg) => {
                self.ec.send(AppEvent::NotifyError(msg));
            }
        }
    }
}

// The spans on the left, then those on the right, aligned to the right edge when they fit.
fn status_bar_line(
    mut left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    width: u16,
) -> Line<'static> {
    let left_width: usize = left.iter().map(Span::width).sum();
    let right_width: usize = right.iter().map(Span::width).sum();
    let width = width as usize;
    if left_width + right_width < width {
        left.push(Span::raw(" ".repeat(width - left_width - right_width)));
        left.extend(right);
    }
    Line::from(left)
}

fn selected_commit_details(
    repository: &Repository,
    commit_list_state: &CommitListState,
) -> (Commit, Vec<FileChange>, Vec<Ref>) {
    let selected = commit_list_state.selected_commit_hash().clone();
    let (commit, changes) = repository.commit_detail(&selected);
    let refs: Vec<Ref> = repository.refs(&selected).into_iter().cloned().collect();
    (commit, changes, refs)
}

fn process_numeric_prefix(
    numeric_prefix: &str,
    user_event: UserEvent,
    _key_event: KeyEvent,
) -> UserEventWithCount {
    if user_event.is_countable() {
        let count = if numeric_prefix.is_empty() {
            1
        } else {
            numeric_prefix.parse::<usize>().unwrap_or(1)
        };
        UserEventWithCount::new(user_event, count)
    } else {
        UserEventWithCount::from_event(user_event)
    }
}

fn extract_user_command_by_number(
    user_command_number: usize,
    ctx: &AppContext,
) -> Result<&UserCommand, String> {
    ctx.core_config
        .user_command
        .commands
        .get(&user_command_number.to_string())
        .ok_or_else(|| format!("No user command configured for number {user_command_number}",))
}

fn extract_user_command_refresh_by_number(user_command_number: usize, ctx: &AppContext) -> bool {
    extract_user_command_by_number(user_command_number, ctx)
        .map(|c| c.refresh)
        .unwrap_or_default()
}

fn build_external_command_parameters_and_exec_command(
    commit: &Commit,
    refs: &[Ref],
    picked_commit_hashes: &[CommitHash],
    user_command_number: usize,
    view_area: Rect,
    ctx: &AppContext,
) -> Result<String, String> {
    build_external_command_parameters(
        commit,
        refs,
        picked_commit_hashes,
        user_command_number,
        view_area,
        ctx,
    )
    .and_then(exec_user_command)
}

fn build_external_command_parameters<'a>(
    commit: &'a Commit,
    refs: &'a [Ref],
    picked_commit_hashes: &'a [CommitHash],
    user_command_number: usize,
    view_area: Rect,
    ctx: &'a AppContext,
) -> Result<ExternalCommandParameters<'a>, String> {
    let command = &extract_user_command_by_number(user_command_number, ctx)?.commands;
    validate_marked_hash(command, picked_commit_hashes.len())?;
    let target_hash = commit.commit_hash.as_str();
    let parent_hashes = commit
        .parent_commit_hashes
        .iter()
        .map(|c| c.as_str())
        .collect();

    let mut all_refs = vec![];
    let mut branches = vec![];
    let mut remote_branches = vec![];
    let mut tags = vec![];
    let mut stash = None;
    for r in refs {
        match r {
            Ref::Tag { .. } => tags.push(r.name()),
            Ref::Branch { .. } => branches.push(r.name()),
            Ref::RemoteBranch { .. } => remote_branches.push(r.name()),
            Ref::Stash { .. } => {
                stash = Some(r.name());
                continue; // skip stashes from {{refs}}
            }
        }
        all_refs.push(r.name());
    }

    let area_width = view_area.width.saturating_sub(4); // minus the left and right padding
    let area_height = (view_area.height.saturating_sub(1))
        .min(ctx.ui_config.user_command.height)
        .saturating_sub(1); // minus the top border
    Ok(ExternalCommandParameters {
        command,
        target_hash,
        picked_hashes: picked_commit_hashes
            .iter()
            .map(CommitHash::as_str)
            .collect(),
        parent_hashes,
        all_refs,
        branches,
        remote_branches,
        tags,
        stash,
        area_width,
        area_height,
    })
}

#[cfg(test)]
mod tests {
    use ratatui::{backend::TestBackend, buffer::Buffer};
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case(1)]
    #[case(2)]
    fn test_fullscreen_clear_redraws_unchanged_content(#[case] frames_before_clear: usize) {
        let mut terminal = Terminal::new(TestBackend::new(5, 1)).unwrap();
        let draw = |terminal: &mut Terminal<TestBackend>| {
            terminal
                .draw(|frame| frame.render_widget(Paragraph::new("hello"), frame.area()))
                .unwrap();
        };
        for _ in 0..frames_before_clear {
            draw(&mut terminal);
        }

        clear_fullscreen(&mut terminal).unwrap();
        assert_eq!(terminal.backend().buffer(), &Buffer::with_lines(["     "]));
        for _ in 0..2 {
            draw(&mut terminal);
            assert_eq!(terminal.backend().buffer(), &Buffer::with_lines(["hello"]));
        }
    }

    #[rustfmt::skip]
    #[rstest]
    #[case("",    UserEvent::NavigateDown, UserEventWithCount::new(UserEvent::NavigateDown, 1))] // no prefix
    #[case("5",   UserEvent::NavigateUp,   UserEventWithCount::new(UserEvent::NavigateUp, 5))] // with prefix
    #[case("0",   UserEvent::PageDown,     UserEventWithCount::new(UserEvent::PageDown, 1))] // zero should be converted to 1
    #[case("42",  UserEvent::ScrollDown,   UserEventWithCount::new(UserEvent::ScrollDown, 42))] // multi-digit number
    #[case("999", UserEvent::PageDown,     UserEventWithCount::new(UserEvent::PageDown, 999))] // large number
    #[case("abc", UserEvent::ScrollUp,     UserEventWithCount::new(UserEvent::ScrollUp, 1))] // should fallback to 1
    #[case("5",   UserEvent::Quit,         UserEventWithCount::new(UserEvent::Quit, 1))] // non-countable event with prefix
    #[case("",    UserEvent::Confirm,      UserEventWithCount::new(UserEvent::Confirm, 1))] // non-countable event without prefix
    fn test_process_numeric_prefix(
        #[case] numeric_prefix: &str,
        #[case] user_event: UserEvent,
        #[case] expected: UserEventWithCount,
    ) {
        let dummy_key_event = KeyEvent::from(KeyCode::Enter); // KeyEvent is not used in the logic
        let actual = process_numeric_prefix(numeric_prefix, user_event, dummy_key_event);
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_status_bar_line_right_aligns_the_right_spans() {
        let line = status_bar_line(vec![Span::raw("left")], vec![Span::raw("[meta]")], 14);
        let texts: Vec<&str> = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(texts, ["left", "    ", "[meta]"]);

        // without room, the right spans are left out
        let line = status_bar_line(vec![Span::raw("left")], vec![Span::raw("[meta]")], 10);
        assert_eq!(line.spans.len(), 1);
    }
}
