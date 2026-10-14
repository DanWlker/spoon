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
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph},
    DefaultTerminal, Frame, Terminal,
};
use rustc_hash::FxHashMap;

use crate::{
    color::{ColorTheme, GraphColorSet},
    config::{CoreConfig, CursorType, UiConfig, UserCommand, UserCommandType},
    event::{AppEvent, EventController, UserEvent, UserEventWithCount},
    external::{
        copy_to_clipboard, exec_interactive, exec_user_command, exec_user_command_suspend,
        validate_marked_hash, ExternalCommandParameters,
    },
    git::{run_remote_command, Commit, CommitHash, FileChange, Head, Ref, Repository},
    graph::GraphImageManager,
    keybind::{keys_to_string, KeyBind, SequenceMatch},
    protocol::ImageProtocol,
    search::SearchOptions,
    view::{RefreshViewContext, View},
    widget::{
        commit_list::{CommitInfo, CommitListState},
        local_changes::LocalChangesState,
        remote_dialog::{
            DialogOutcome, RemoteAction, RemoteCommand, RemoteDialog, RemoteDialogWidget,
        },
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
    Refresh(RefreshRequest),
}

pub struct RefreshRequest {
    pub context: RefreshViewContext,
}

#[derive(Debug)]
pub struct AppContext {
    pub keybind: KeyBind,
    pub core_config: CoreConfig,
    pub ui_config: UiConfig,
    pub color_theme: ColorTheme,
    pub image_protocol: ImageProtocol,
    // what the fetch, pull or push running in the background is doing, kept across refreshes
    pub remote_task: RefCell<Option<String>>,
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
    // the fetch, pull or push popup, over any view
    remote_dialog: Option<RemoteDialog>,
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
        refresh_view_context: Option<RefreshViewContext>,
    ) -> Self {
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

        let mut app = Self {
            repository,
            view,
            remote_dialog: None,
            app_status: AppStatus::default(),
            ctx,
            ec,
        };

        if let Some(warning) = &graph.warning {
            app.warn_notification(warning.clone());
        }

        match refresh_view_context {
            Some(context) => app.init_with_context(context),
            // start on the local changes page; the graph is one Esc away
            None => app.open_local_changes(),
        }

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

                    if self.remote_dialog.is_some() {
                        self.handle_remote_dialog_key(key);
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

                    // key sequences wait for their next key; typed text (the search input) is no sequence
                    if !matches!(self.app_status.status_line, StatusLine::Input { .. }) {
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
                                if self.handle_remote_event(ue) {
                                    self.app_status.numeric_prefix.clear();
                                    continue;
                                }
                                let event_with_count = process_numeric_prefix(
                                    &self.app_status.numeric_prefix,
                                    ue,
                                    key,
                                );
                                self.view.handle_event(event_with_count, key);
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

                    let user_event = self.ctx.keybind.get(&key).copied().filter(|ue| {
                        // without panels to focus, digits fall through to the numeric prefix (or search input)
                        !matches!(ue, UserEvent::FocusPanel(_)) || self.view.has_panels()
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
                        Some(ue) if self.handle_remote_event(ue) => {
                            self.app_status.numeric_prefix.clear();
                        }
                        Some(ue) => {
                            let event_with_count =
                                process_numeric_prefix(&self.app_status.numeric_prefix, ue, key);
                            self.view.handle_event(event_with_count, key);
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
                AppEvent::OpenDetail => {
                    self.open_detail();
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
                AppEvent::OpenRefs => {
                    self.open_refs();
                }
                AppEvent::CloseRefs { to_local_changes } => {
                    self.close_refs(to_local_changes);
                }
                AppEvent::RunExternal(command) => {
                    self.run_external(&command);
                }
                AppEvent::RemoteFinished { command, result } => {
                    self.finish_remote_command(command, result);
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
                    let request = RefreshRequest { context };
                    return Ok(Ret::Refresh(request));
                }
                AppEvent::ClearViewImages => {
                    self.app_status.pending_clear = PendingClear::ViewImages;
                }
                AppEvent::ClearScreen => {
                    self.app_status.pending_clear = PendingClear::Screen;
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

        self.update_state(view_area);

        self.view.render(f, view_area);
        if let Some(dialog) = &self.remote_dialog {
            f.render_widget(
                RemoteDialogWidget::new(dialog, &self.ctx.color_theme),
                view_area,
            );
        }
        self.render_status_line(f, status_line_area);
    }
}

impl App<'_> {
    fn render_status_line(&self, f: &mut Frame, area: Rect) {
        let text: Line = match &self.app_status.status_line {
            StatusLine::None => {
                let picked = self.view.picked_commit_count();
                // the count and the keys of a key sequence typed so far
                let prefix = format!(
                    "{}{}",
                    self.app_status.numeric_prefix,
                    keys_to_string(&self.app_status.pending_keys)
                );
                if let (true, Some(task)) =
                    (prefix.is_empty(), self.ctx.remote_task.borrow().as_ref())
                {
                    let msg = format!("{task}…");
                    let msg_w = console::measure_text_width(&msg);
                    let pad_w = (area.width as usize).saturating_sub(msg_w + 2 /* pad */);
                    Line::from(vec![
                        " ".repeat(pad_w).into(),
                        msg.fg(self.ctx.color_theme.status_info_fg),
                    ])
                } else if prefix.is_empty() && picked > 0 {
                    let commits = if picked == 1 { "commit" } else { "commits" };
                    let msg = format!("{picked} {commits} picked  (Esc to clear)");
                    let msg_w = console::measure_text_width(&msg);
                    let pad_w = (area.width as usize).saturating_sub(msg_w + 2 /* pad */);
                    Line::from(vec![
                        " ".repeat(pad_w).into(),
                        msg.fg(self.ctx.color_theme.status_info_fg),
                    ])
                } else {
                    Line::raw(prefix).fg(self.ctx.color_theme.status_input_transient_fg)
                }
            }
            StatusLine::Input {
                message, metadata, ..
            } => status_line_with_metadata(
                message,
                metadata,
                Style::default().fg(self.ctx.color_theme.status_input_fg),
                Style::default().fg(self.ctx.color_theme.status_input_transient_fg),
                area.width,
            ),
            StatusLine::Transient(msg) => {
                let msg_w = console::measure_text_width(msg.as_str());
                let pad_w = (area.width as usize).saturating_sub(msg_w + 2 /* pad */);
                Line::from(vec![
                    " ".repeat(pad_w).into(),
                    msg.as_str()
                        .fg(self.ctx.color_theme.status_input_transient_fg),
                ])
            }
            StatusLine::SearchResult {
                message,
                options,
                matched,
            } => {
                let message_style = if *matched {
                    Style::default().fg(self.ctx.color_theme.status_info_fg)
                } else {
                    Style::default()
                        .fg(self.ctx.color_theme.status_warn_fg)
                        .add_modifier(Modifier::BOLD)
                };
                status_line_with_metadata(
                    message,
                    options,
                    message_style,
                    Style::default().fg(self.ctx.color_theme.status_input_transient_fg),
                    area.width,
                )
            }
            StatusLine::NotificationInfo(msg) => {
                Line::raw(msg).fg(self.ctx.color_theme.status_info_fg)
            }
            StatusLine::NotificationSuccess(msg) => Line::raw(msg)
                .add_modifier(Modifier::BOLD)
                .fg(self.ctx.color_theme.status_success_fg),
            StatusLine::NotificationWarn(msg) => Line::raw(msg)
                .add_modifier(Modifier::BOLD)
                .fg(self.ctx.color_theme.status_warn_fg),
            StatusLine::NotificationError(msg) => Line::raw(format!("ERROR: {msg}"))
                .add_modifier(Modifier::BOLD)
                .fg(self.ctx.color_theme.status_error_fg),
        };
        let paragraph = Paragraph::new(text).block(
            Block::default()
                .borders(Borders::TOP)
                .style(Style::default().fg(self.ctx.color_theme.divider_fg))
                .padding(Padding::horizontal(1)),
        );
        f.render_widget(paragraph, area);

        if let StatusLine::Input {
            cursor_position, ..
        } = &self.app_status.status_line
        {
            let (x, y) = (area.x + cursor_position + 1, area.y + 1);
            match &self.ctx.ui_config.common.cursor_type {
                CursorType::Native => {
                    f.set_cursor_position((x, y));
                }
                CursorType::Virtual(cursor) => {
                    let style = Style::default().fg(self.ctx.color_theme.virtual_cursor_fg);
                    f.buffer_mut().set_string(x, y, cursor, style);
                }
            }
        }
    }
}

fn split_app_areas(area: Rect) -> [Rect; 2] {
    Layout::vertical([Constraint::Min(0), Constraint::Length(2)]).areas(area)
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

    fn open_detail(&mut self) {
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

    fn open_refs(&mut self) {
        let (commit_list_state, local_changes_state, from_local_changes) = match self.view {
            View::List(ref mut view) => (
                view.take_list_state(),
                LocalChangesState::load(self.repository),
                false,
            ),
            View::LocalChanges(ref mut view) => (
                view.take_list_state(),
                view.take_local_changes_state(),
                true,
            ),
            _ => return,
        };
        let refs = self.repository.all_refs();
        self.view = View::of_refs(
            commit_list_state,
            &refs,
            local_changes_state,
            self.ctx.clone(),
            self.ec.sender(),
        );
        if from_local_changes {
            // tab cycles from the local changes page to the branches
            if let View::Refs(ref mut view) = self.view {
                view.select_branches();
            }
        }
    }

    fn close_refs(&mut self, to_local_changes: bool) {
        if let View::Refs(ref mut view) = self.view {
            let commit_list_state = view.take_list_state();
            if to_local_changes {
                if !view.is_showing_local_changes() {
                    // the graph is shown until now
                    self.app_status.pending_clear = PendingClear::ViewImages;
                }
                let local_changes_state = view.take_local_changes_state();
                self.view = View::of_local_changes(
                    commit_list_state,
                    local_changes_state,
                    self.ctx.clone(),
                    self.ec.sender(),
                );
            } else {
                self.view = View::of_list(commit_list_state, self.ctx.clone(), self.ec.sender());
            }
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

    // Opens the fetch, pull or push popup, or fetches all remotes, for the event bound to them.
    // Returns whether the event was one of them.
    fn handle_remote_event(&mut self, event: UserEvent) -> bool {
        let action = match event {
            UserEvent::Fetch => RemoteAction::Fetch,
            UserEvent::Pull => RemoteAction::Pull,
            UserEvent::Push => RemoteAction::Push,
            UserEvent::FetchAll => {
                self.start_remote_command(RemoteCommand::fetch_all());
                return true;
            }
            _ => return false,
        };
        // keys typed into the search of the list, or over the help, are not these
        if matches!(self.app_status.status_line, StatusLine::Input { .. })
            || matches!(self.view, View::Help(_) | View::Default)
        {
            return false;
        }
        if let Some(task) = self.ctx.remote_task.borrow().as_ref() {
            self.ec.send(AppEvent::NotifyWarn(format!(
                "{task}, wait for it to finish"
            )));
            return true;
        }
        match RemoteDialog::new(action, self.repository.remote_info()) {
            Ok(dialog) => {
                self.remote_dialog = Some(dialog);
                // images of the graph would cover the popup
                self.app_status.pending_clear = PendingClear::Screen;
            }
            Err(err) => self.ec.send(AppEvent::NotifyError(err)),
        }
        true
    }

    fn handle_remote_dialog_key(&mut self, key: KeyEvent) {
        let Some(dialog) = self.remote_dialog.as_mut() else {
            return;
        };
        let event = self.ctx.keybind.get(&key).copied();
        if event == Some(UserEvent::ForceQuit) {
            self.ec.send(AppEvent::Quit);
            return;
        }
        match dialog.handle_key(event, key) {
            DialogOutcome::None => {}
            DialogOutcome::Close => self.close_remote_dialog(),
            DialogOutcome::Error(err) => self.ec.send(AppEvent::NotifyError(err)),
            DialogOutcome::Run(command) => {
                self.close_remote_dialog();
                self.start_remote_command(command);
            }
        }
    }

    fn close_remote_dialog(&mut self) {
        self.remote_dialog = None;
        self.app_status.pending_clear = PendingClear::Screen;
    }

    // Runs the command in the background; the app is told when it finishes.
    fn start_remote_command(&mut self, command: RemoteCommand) {
        if let Some(task) = self.ctx.remote_task.borrow().as_ref() {
            self.ec.send(AppEvent::NotifyWarn(format!(
                "{task}, wait for it to finish"
            )));
            return;
        }
        *self.ctx.remote_task.borrow_mut() = Some(command.running.clone());
        let path = self.repository.path().to_path_buf();
        let tx = self.ec.sender();
        std::thread::spawn(move || {
            let result = run_remote_command(&path, &command.args);
            tx.send(AppEvent::RemoteFinished { command, result });
        });
    }

    fn finish_remote_command(&mut self, command: RemoteCommand, result: Result<String, String>) {
        *self.ctx.remote_task.borrow_mut() = None;
        let typing = self.view.is_capturing_keys()
            || matches!(self.app_status.status_line, StatusLine::Input { .. });
        // a pull may have changed files or stopped on conflicts, even when it failed;
        // refreshing would interrupt typing, so then it waits for a manual refresh
        if !typing {
            self.view.refresh();
        }
        match result {
            Ok(_) if typing => self.ec.send(AppEvent::NotifySuccess(format!(
                "{} (refresh to see the changes)",
                command.done
            ))),
            Ok(_) => self.ec.send(AppEvent::NotifySuccess(command.done)),
            Err(err) => self.ec.send(AppEvent::NotifyError(err)),
        }
    }

    fn open_local_changes(&mut self) {
        if let View::List(ref mut view) = self.view {
            let commit_list_state = view.take_list_state();
            let local_changes_state = LocalChangesState::load(self.repository);
            self.view = View::of_local_changes(
                commit_list_state,
                local_changes_state,
                self.ctx.clone(),
                self.ec.sender(),
            );
            self.app_status.pending_clear = PendingClear::ViewImages;
        }
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
                self.open_detail();
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
                self.open_local_changes();
                if let View::LocalChanges(ref mut view) = self.view {
                    view.reset_local_changes_with(local_changes_context);
                }
            }
            RefreshViewContext::Refs { refs_context, .. } => {
                self.open_refs();
                if let View::Refs(ref mut view) = self.view {
                    view.reset_refs_with(refs_context);
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

fn status_line_with_metadata(
    message: &str,
    metadata: &str,
    message_style: Style,
    metadata_style: Style,
    area_width: u16,
) -> Line<'static> {
    let content_width = area_width.saturating_sub(2) as usize;
    let message_width = console::measure_text_width(message);
    let metadata_width = console::measure_text_width(metadata);
    let min_gap_width = 2;

    if message_width + min_gap_width + metadata_width > content_width {
        return Line::from(Span::styled(message.to_owned(), message_style));
    }

    let pad_width = content_width - message_width - metadata_width;
    Line::from(vec![
        Span::styled(message.to_owned(), message_style),
        Span::raw(" ".repeat(pad_width)),
        Span::styled(metadata.to_owned(), metadata_style),
    ])
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
    fn test_status_line_with_metadata_right_aligns_metadata() {
        let line =
            status_line_with_metadata("left", "[meta]", Style::default(), Style::default(), 14);

        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[0].content.as_ref(), "left");
        assert_eq!(line.spans[1].content.as_ref(), "  ");
        assert_eq!(line.spans[2].content.as_ref(), "[meta]");
    }

    #[test]
    fn test_status_line_with_metadata_hides_metadata_when_area_is_too_narrow() {
        let line =
            status_line_with_metadata("left", "[meta]", Style::default(), Style::default(), 13);

        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content.as_ref(), "left");
    }
}
