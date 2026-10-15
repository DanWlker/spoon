use std::{
    cell::RefCell,
    io::{self, Write},
    rc::Rc,
    time::{Duration, Instant},
};

use ratatui::{
    backend::Backend,
    crossterm::{
        event::{KeyEvent, MouseEventKind},
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
    color::ColorTheme,
    config::{CoreConfig, CursorType, UiConfig, UserCommand, UserCommandType},
    event::{hint, AppEvent, EventController, KeyHint, UserEvent, UserEventWithCount},
    external::{
        copy_to_clipboard, exec_interactive, exec_user_command, exec_user_command_suspend,
        validate_marked_hash, ExternalCommandParameters,
    },
    git::{
        refs_snapshot, run_task, Commit, CommitHash, FileChange, GitTask, Head, Ref, Repository,
    },
    graph::GraphImageManager,
    graph_commands::{graph_command, ref_graph_command, GraphCommand},
    keybind::{keys_to_string, KeyBind, Scope, SequenceMatch},
    layout::{Border, Direction, Panel, PanelLayout},
    protocol::ImageProtocol,
    ref_commands::ref_command,
    search::SearchOptions,
    view::{
        user_command_height, LocalChangesRefreshViewContext, RefreshViewContext, View, ViewKind,
    },
    widget::{
        commit_list::{CommitInfo, CommitListState},
        commit_tabs::DetailTab,
        git_dialog::{DialogOutcome, GitDialog, GitDialogWidget, RemoteAction},
        keys_popup::KeysPopup,
        local_changes::LocalChangesState,
        ref_list::{RefList, RefListState, RefSection, RefSelection},
        style::{badge, key_hints},
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

// How long the fetch, pull and push popups wait for their key to be pressed again before showing,
// as a double press runs the command right away.
const QUICK_RUN_DELAY: Duration = Duration::from_millis(200);

pub enum Ret {
    Quit,
    // reload the repository and show the view of the context again
    Refresh(Box<RefreshViewContext>),
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
    // the keys of the hints in the status bar, see `short_keys`
    pub key_names: RefCell<FxHashMap<Vec<UserEvent>, String>>,
    // the sizes of the panels, set by the user, kept across refreshes
    pub layout: RefCell<PanelLayout>,
}

impl AppContext {
    // Why the repository can't be changed now: a git command is running in the background.
    // The keys of the events for a hint, see `KeyBind::short_keys`, found once for each.
    pub fn short_keys(&self, events: &[UserEvent]) -> String {
        let mut names = self.key_names.borrow_mut();
        names
            .entry(events.to_vec())
            .or_insert_with(|| self.keybind.short_keys(events))
            .clone()
    }

    pub fn busy_message(&self) -> Option<String> {
        let task = self.running_task.borrow();
        task.as_ref()
            .map(|task| format!("{task}, wait for it to finish"))
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PendingClear {
    #[default]
    None,
    ViewImages,
    Screen,
}

// What the last frame showed, see `App::frame_clear`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameLayout {
    view: ViewKind,
    sidebar_collapsed: bool,
    // the sizes of the panels, see `PanelLayout`
    panels: u64,
    dialog: bool,
    // the size of the keys popup
    keys_popup: Option<(u16, u16)>,
}

#[derive(Debug, Default)]
struct AppStatus {
    status_line: StatusLine,
    // the keys pressed so far of a key sequence, like "]" of "]m"
    pending_keys: Vec<KeyEvent>,
    view_area: Rect,
    pending_clear: PendingClear,
}

// The parts of the app that don't depend on the loaded repository, kept when it is reloaded:
// the sidebar, the focus, the pages not shown, the popup and the status line.
#[derive(Debug)]
pub struct AppShell {
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
    // when the popup shows, while it waits for the key opening it to be pressed again, which
    // runs the command right away, see `QUICK_RUN_DELAY`
    dialog_reveal: Option<Instant>,
    // what the last frame showed, None before the first
    last_frame: Option<FrameLayout>,
    app_status: AppStatus,
    // the branches and HEAD as loaded, see `git::refs_snapshot`, so the app's own changes are not
    // taken for outside ones
    refs_snapshot: Option<String>,
    // only the commits of the current branch are loaded, see `UserEvent::BranchOnly`
    branch_only: bool,
    // two refs diffed from the sidebar, the graph showing only their history while their
    // comparison is open: asked for, until the repository is reloaded for it, then open
    compare_request: Option<RefCompare>,
    compare: Option<RefCompare>,
    // the keys of the focused panel shown in a popup, see `UserEvent::KeysToggle`
    keys_popup: bool,
}

// The changes from `base` to `target`, refs (or HEAD) diffed from the sidebar.
#[derive(Debug, Clone)]
struct RefCompare {
    base: String,
    target: String,
}

impl AppShell {
    // The revisions whose history the graph shows, None for all.
    pub fn graph_revisions(&self) -> Option<Vec<String>> {
        match self.compare_request.as_ref().or(self.compare.as_ref()) {
            Some(compare) => Some(vec![compare.base.clone(), compare.target.clone()]),
            None if self.branch_only => Some(vec!["HEAD".into()]),
            None => None,
        }
    }

    // What the graph shows, when not all of the history, for the title of the commit list.
    fn graph_filter_note(&self, repository: &Repository) -> Option<String> {
        match self.compare_request.as_ref().or(self.compare.as_ref()) {
            Some(compare) => Some(format!("{} ↔ {}", compare.base, compare.target)),
            None if self.branch_only => Some(format!(
                "{} only",
                repository.current_branch().unwrap_or_else(|| "HEAD".into())
            )),
            None => None,
        }
    }

    pub fn reset_graph_filter(&mut self) {
        self.branch_only = false;
        self.compare_request = None;
        self.compare = None;
    }
}

#[derive(Debug)]
pub struct App<'a> {
    repository: &'a Repository,
    view: View<'a>,
    shell: AppShell,
    ctx: Rc<AppContext>,
    ec: &'a EventController,
}

impl<'a> App<'a> {
    pub fn new(
        repository: &'a Repository,
        graph_image_manager: GraphImageManager<'a>,
        initial_selection: InitialSelection,
        ctx: Rc<AppContext>,
        ec: &'a EventController,
        // the shell and view of the app before the repository was reloaded
        refresh: Option<(AppShell, Box<RefreshViewContext>)>,
    ) -> Self {
        let (shell, refresh_view_context) = match refresh {
            Some((shell, context)) => (Some(shell), Some(*context)),
            None => (None, None),
        };
        let graph = graph_image_manager.graph();
        let mut ref_name_to_commit_index_map = FxHashMap::default();
        let head_history = repository.head_history();
        let comparing_refs = shell
            .as_ref()
            .is_some_and(|s| s.compare_request.is_some() || s.compare.is_some());
        let commits = graph
            .commits
            .iter()
            .enumerate()
            .map(|(i, commit)| {
                let refs = repository.refs(&commit.commit_hash);
                for r in &refs {
                    ref_name_to_commit_index_map.insert(r.name(), i);
                }
                // comparing two refs, both are shown alike
                let on_head = comparing_refs || head_history.contains(&commit.commit_hash);
                CommitInfo::new(commit, refs, on_head)
            })
            .collect();
        let graph_cell_width = graph_image_manager.graph_cell_width();
        let mut commit_list_state = CommitListState::new(
            commits,
            graph_image_manager,
            graph_cell_width,
            ref_name_to_commit_index_map,
            SearchOptions {
                target: ctx.core_config.search.target,
                ignore_case: ctx.core_config.search.ignore_case,
                fuzzy: ctx.core_config.search.fuzzy,
            },
            ctx.ui_config.list.scrolloff as usize,
        );
        commit_list_state.set_filter_note(
            shell
                .as_ref()
                .and_then(|shell| shell.graph_filter_note(repository)),
        );
        if let InitialSelection::Head = initial_selection {
            match repository.head() {
                Head::Branch { name } => commit_list_state.select_ref(name),
                Head::Detached { target } => commit_list_state.select_commit_hash(target),
                Head::None => {}
            }
        }
        let view = View::of_list(commit_list_state, ctx.clone(), ec.sender());

        let refs = repository.all_refs();
        let local_change_count = repository.loaded_local_change_count();
        let loaded_refs = refs_snapshot(repository.path()).ok();
        let mut shell = match shell {
            Some(mut shell) => {
                shell.sidebar.set_refs(&refs, local_change_count);
                shell.refs_snapshot = loaded_refs;
                shell
            }
            None => AppShell {
                sidebar: RefListState::new(&refs, local_change_count),
                sidebar_focused: false,
                sidebar_collapsed: false,
                parked_local_changes: None,
                parked_detail_tab: None,
                dialog: None,
                dialog_reveal: None,
                last_frame: None,
                app_status: AppStatus::default(),
                refs_snapshot: loaded_refs,
                branch_only: false,
                compare_request: None,
                compare: None,
                keys_popup: false,
            },
        };
        shell.sidebar.set_tracking(repository.branch_tracking());

        let mut app = Self {
            repository,
            view,
            shell,
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
        app.open_ref_compare();

        app
    }
}

impl App<'_> {
    // What is kept of the app when the repository is reloaded.
    pub fn into_shell(self) -> AppShell {
        self.shell
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<Ret, std::io::Error> {
        loop {
            self.end_ref_compare();
            self.prepare_render(terminal)?;
            // Keep image deletion, uploads, and text updates in the same frame.
            // Preparation and event handling stay outside the synchronized update.
            io::stdout().sync_update(|_| self.draw(terminal))??;
            let event = match self.shell.dialog_reveal {
                Some(at) => match self
                    .ec
                    .recv_timeout(at.saturating_duration_since(Instant::now()))
                {
                    Some(event) => event,
                    // no second press, the popup shows
                    None => {
                        self.shell.dialog_reveal = None;
                        continue;
                    }
                },
                None => self.ec.recv(),
            };
            match event {
                AppEvent::Key(key) => {
                    match self.shell.app_status.status_line {
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

                    if self.shell.dialog.is_some() {
                        self.handle_dialog_key(key);
                        continue;
                    }

                    let scope = self.current_scope();
                    if self.view.is_capturing_keys() {
                        if let Some(UserEvent::ForceQuit) =
                            self.ctx.keybind.event_in(Scope::Global, &key)
                        {
                            self.ec.send(AppEvent::Quit);
                        } else {
                            let event = UserEventWithCount::from_event(UserEvent::Unknown);
                            self.view.handle_event(event, key);
                        }
                        continue;
                    }

                    // key sequences wait for their next key, typed text (the search input) is no
                    // sequence; the focused scope decides, like ] cycling the tabs of the details
                    // where elsewhere it starts "] v"
                    if !matches!(self.shell.app_status.status_line, StatusLine::Input { .. }) {
                        let had_pending = !self.shell.app_status.pending_keys.is_empty();
                        self.shell.app_status.pending_keys.push(key);
                        match self
                            .ctx
                            .keybind
                            .resolve(scope, &self.shell.app_status.pending_keys)
                        {
                            SequenceMatch::Partial => continue,
                            SequenceMatch::Complete(ue) => {
                                self.shell.app_status.pending_keys.clear();
                                self.dispatch(UserEventWithCount::from_event(ue), key);
                                continue;
                            }
                            SequenceMatch::None => {
                                self.shell.app_status.pending_keys.clear();
                                // Esc only cancels the started sequence
                                if had_pending
                                    && self.ctx.keybind.event_in(scope, &key)
                                        == Some(UserEvent::Cancel)
                                {
                                    continue;
                                }
                                // otherwise the key goes on as usual
                            }
                        }
                    }

                    let user_event = self.ctx.keybind.event_in(scope, &key).filter(|ue| {
                        // digits focus the panels of the page, where it has them
                        match ue {
                            UserEvent::FocusPanel(n) => {
                                !matches!(self.view, View::Help(_))
                                    && (*n == 0 || self.view.has_panels())
                            }
                            _ => true,
                        }
                    });

                    match user_event {
                        Some(UserEvent::ForceQuit) => {
                            self.ec.send(AppEvent::Quit);
                        }
                        Some(ue) => self.dispatch(UserEventWithCount::from_event(ue), key),
                        None => {
                            if let StatusLine::Input { .. } = self.shell.app_status.status_line {
                                // In input mode, pass all key events to the view
                                // fixme: currently, the only thing that processes key_event is searching the list,
                                //        so this probably works, but it's not the right process...
                                self.view.handle_event(
                                    UserEventWithCount::from_event(UserEvent::Unknown),
                                    key,
                                );
                            }
                        }
                    }
                }
                AppEvent::Mouse(mouse) => {
                    let mut layout = self.ctx.layout.borrow_mut();
                    match mouse.kind {
                        MouseEventKind::Down(_) => {
                            layout.press(mouse.column, mouse.row);
                        }
                        MouseEventKind::Drag(_) => {
                            layout.drag_to(mouse.column, mouse.row);
                        }
                        _ => layout.release(),
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
                }
                AppEvent::CloseDetail => {
                    self.close_detail();
                }
                AppEvent::OpenUserCommand(n) => {
                    self.open_user_command(n);
                }
                AppEvent::CloseUserCommand => {
                    self.close_user_command();
                }
                AppEvent::RunExternal(command) => {
                    self.run_external(&command);
                }
                AppEvent::TaskFinished { task, result } => {
                    self.finish_task(task, result);
                }
                AppEvent::LocalChangesChanged => {
                    let count = self.repository.reload_local_changes();
                    self.shell.sidebar.set_local_change_count(count);
                }
                AppEvent::FilesTick if !self.refresh_paused() => self.reload_files(),
                AppEvent::RefsSnapshot(snapshot) if !self.refresh_paused() => {
                    self.check_refs(snapshot);
                }
                // what changed meanwhile, without reloading everything when nothing did
                AppEvent::FocusGained if !self.refresh_paused() => {
                    self.reload_files();
                    if let Ok(snapshot) = refs_snapshot(self.repository.path()) {
                        self.check_refs(snapshot);
                    }
                }
                // checked again on the next tick
                AppEvent::FilesTick | AppEvent::RefsSnapshot(_) | AppEvent::FocusGained => {}
                AppEvent::OpenHelp => {
                    self.open_help();
                }
                AppEvent::CloseHelp => {
                    self.close_help();
                }
                AppEvent::SelectOlderCommit => {
                    self.select_older_commit();
                }
                AppEvent::SelectNewerCommit => {
                    self.select_newer_commit();
                }
                AppEvent::CopyToClipboard { name, value } => {
                    self.copy_to_clipboard(name, value);
                }
                AppEvent::Refresh(context) => {
                    self.cleanup_graph_images()?;
                    // the details open again on the same tab, but a comparison
                    match &self.view {
                        View::Detail(view) if view.is_comparing() => {
                            self.shell.parked_detail_tab = None
                        }
                        View::Detail(view) => self.shell.parked_detail_tab = Some(view.tab()),
                        _ => {}
                    }
                    // the graph is drawn again from scratch
                    self.shell.last_frame = None;
                    return Ok(Ret::Refresh(Box::new(context)));
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
        match self.frame_clear() {
            PendingClear::None => {}
            PendingClear::ViewImages => self.clear_view_image_area(terminal)?,
            PendingClear::Screen => {
                self.ctx.image_protocol.clear();
                clear_fullscreen(terminal)?;
            }
        }
        self.flush_pending_graph_uploads()?;
        terminal.draw(|f| self.render(f))?;
        Ok(())
    }

    // The keys of what has the focus: the popup, the sidebar or the view; without those waiting for
    // the operation in progress, see `blocked_by_operation`.
    fn local_hints(&self) -> Vec<KeyHint> {
        let mut hints = if let Some(dialog) = self.visible_dialog() {
            dialog.key_hints()
        } else if self.shell.sidebar_focused {
            self.sidebar_hints()
        } else {
            self.view.key_hints()
        };
        if self.visible_dialog().is_none() && self.repository.operation_in_progress().is_some() {
            hints.retain(|(events, _)| !events.iter().copied().any(moves_head));
        }
        hints
    }

    // The keys working everywhere, for the status bar: the remotes, switching pages, the keys
    // popup and the help.
    fn global_hints(&self) -> Vec<KeyHint> {
        let keys = if self.shell.keys_popup {
            "hide keys"
        } else {
            "keys"
        };
        let mut hints = vec![
            hint(UserEvent::KeysToggle, keys),
            hint(UserEvent::Push, "push"),
            hint(UserEvent::Pull, "pull"),
            hint(UserEvent::Fetch, "fetch"),
            hint(UserEvent::RefList, "switch page"),
            hint(UserEvent::Refresh, "refresh"),
            hint(UserEvent::HelpToggle, "help"),
        ];
        if self.repository.operation_in_progress().is_some() {
            hints.retain(|(events, _)| !events.iter().copied().any(moves_head));
        }
        hints
    }

    // Whether the keys go to a popup, typed text or the help, which take only their own: then
    // these are in the status bar, else those of the whole app, the others in the keys popup.
    fn keys_are_local(&self) -> bool {
        self.visible_dialog().is_some()
            || self.view.is_capturing_keys()
            || matches!(self.shell.app_status.status_line, StatusLine::Input { .. })
            || matches!(self.view, View::Help(_))
    }

    // The keys of the focused panel and what they do, for the keys popup; empty when it is
    // hidden.
    fn keys_popup_keys(&self) -> Vec<(String, String)> {
        if !self.shell.keys_popup || self.keys_are_local() {
            return vec![];
        }
        let global: Vec<UserEvent> = self
            .global_hints()
            .into_iter()
            .flat_map(|(events, _)| events)
            .collect();
        self.local_hints()
            .into_iter()
            .filter(|(events, _)| !events.iter().all(|e| global.contains(e)))
            .map(|(events, description)| (self.ctx.short_keys(&events), description))
            .filter(|(keys, _)| !keys.is_empty())
            .collect()
    }

    fn keys_popup_title(&self) -> &'static str {
        if self.shell.sidebar_focused {
            "Refs"
        } else {
            self.view.focus_name()
        }
    }

    // The width and height of the keys popup, None when it is hidden.
    fn keys_popup_size(&self) -> Option<(u16, u16)> {
        let keys = self.keys_popup_keys();
        if keys.is_empty() {
            return None;
        }
        let popup = KeysPopup::new(self.keys_popup_title(), &keys, &self.ctx.color_theme);
        let area = popup.area(Rect::new(0, 0, u16::MAX / 2, u16::MAX / 2));
        Some((area.width, area.height))
    }

    // The keys for the status bar (see `keys_are_local`), as many as fit in `width`, the first
    // ones being the most useful.
    fn fit_key_hints(&self, width: usize) -> Line<'static> {
        let hints = if self.keys_are_local() {
            self.local_hints()
        } else {
            self.global_hints()
        };
        let keys: Vec<(String, String)> = hints
            .into_iter()
            .map(|(events, description)| (self.ctx.short_keys(&events), description))
            .filter(|(keys, _)| !keys.is_empty())
            .collect();
        let theme = &self.ctx.color_theme;
        let mut fitted = Line::default();
        for n in 1..=keys.len() {
            let keys: Vec<(&str, &str)> = keys[..n]
                .iter()
                .map(|(key, description)| (key.as_str(), description.as_str()))
                .collect();
            let line = key_hints(&keys, theme);
            if line.width() > width {
                break;
            }
            fitted = line;
        }
        fitted
    }

    // What to clear before drawing the frame: the graph images stay on the screen until cleared,
    // so they are when the layout changes. Text views cover the area of the images they replace,
    // and the whole screen is cleared when the graph comes back, or moves.
    fn frame_clear(&mut self) -> PendingClear {
        let layout = FrameLayout {
            view: self.view.kind(),
            sidebar_collapsed: self.shell.sidebar_collapsed,
            panels: self.ctx.layout.borrow().version(),
            dialog: self.visible_dialog().is_some(),
            keys_popup: self.keys_popup_size(),
        };
        let clear = match self.shell.last_frame.replace(layout) {
            None => PendingClear::Screen,
            // the graph moves
            Some(last)
                if last.sidebar_collapsed != layout.sidebar_collapsed
                    || last.panels != layout.panels =>
            {
                PendingClear::Screen
            }
            // images of the graph would cover the popup
            Some(last) if last.dialog != layout.dialog || last.keys_popup != layout.keys_popup => {
                PendingClear::Screen
            }
            Some(last) if last.view == layout.view => PendingClear::None,
            Some(last)
                if layout.view == ViewKind::List
                    || matches!(last.view, ViewKind::Help | ViewKind::LocalChanges) =>
            {
                PendingClear::Screen
            }
            Some(_) => PendingClear::ViewImages,
        };
        // a program run in the terminal also needs the screen cleared
        std::mem::take(&mut self.shell.app_status.pending_clear).max(clear)
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

        let [app_area, status_line_area] = split_app_areas(f.area());
        let [sidebar_area, view_area] = self.split_sidebar(app_area);
        // the panels place their borders to drag as they are drawn
        self.ctx.layout.borrow_mut().clear_borders();
        if !self.shell.sidebar_collapsed && !sidebar_area.is_empty() {
            let border = Border {
                panel: Panel::Sidebar,
                vertical: true,
                at: sidebar_area.right() - 1,
                from: sidebar_area.top(),
                to: sidebar_area.bottom(),
                anchor: sidebar_area.left(),
                forward: true,
            };
            self.ctx
                .layout
                .borrow_mut()
                .place(border, sidebar_area.width);
        }

        self.update_state(view_area);

        if let View::LocalChanges(view) = &self.view {
            self.shell
                .sidebar
                .set_local_change_count(view.local_change_count());
        }
        // views are replaced while switching pages, they take the focus as they are drawn
        let sidebar = RefList::new(
            self.ctx.clone(),
            self.shell.sidebar_focused,
            self.shell.sidebar_collapsed,
        );
        f.render_stateful_widget(sidebar, sidebar_area, &mut self.shell.sidebar);
        self.view.render(f, view_area, !self.shell.sidebar_focused);
        let keys = self.keys_popup_keys();
        if !keys.is_empty() {
            let popup = KeysPopup::new(self.keys_popup_title(), &keys, &self.ctx.color_theme);
            f.render_widget(popup, app_area);
        }
        if let Some(dialog) = self.visible_dialog() {
            f.render_widget(GitDialogWidget::new(dialog, &self.ctx), view_area);
        }
        self.render_status_line(f, status_line_area);
    }
}

impl App<'_> {
    // A bar after a neovim statusline: the page as a rounded pill like a mode, then the message,
    // or what runs in the background, or the repository; on the right the keys of what has the
    // focus and the position in the commit list.
    fn render_status_line(&self, f: &mut Frame, area: Rect) {
        let theme = &self.ctx.color_theme;
        let bar = Style::default()
            .fg(theme.status_bar_fg)
            .bg(theme.status_bar_bg);
        let (page, color) = match &self.view {
            _ if self.shell.sidebar_focused => ("REFS", theme.status_warn_fg),
            View::LocalChanges(_) => ("CHANGES", theme.status_success_fg),
            View::UserCommand(_) => ("COMMAND", theme.highlight_fg),
            View::Help(_) => ("HELP", theme.highlight_fg),
            _ => ("GRAPH", theme.accent_fg),
        };
        // the first letter only on narrow terminals, like the mode
        let page = if area.width < 115 { &page[..1] } else { page };
        let mut left = vec![
            Span::raw(" \u{e0b6}").fg(color),
            Span::raw(page).fg(theme.accent_text_fg).bg(color).bold(),
            Span::raw("\u{e0b4}").fg(color),
            Span::raw("   "),
        ];
        let left_width: usize = left.iter().map(Span::width).sum();
        let mut right = vec![];

        match &self.shell.app_status.status_line {
            StatusLine::None => {
                // the keys of a key sequence typed so far
                let prefix = keys_to_string(&self.shell.app_status.pending_keys);
                if !prefix.is_empty() {
                    left.push(Span::raw(format!("{prefix} ")).fg(theme.accent_fg).bold());
                }
                // what runs in the background, else where the repository is
                match self.ctx.running_task.borrow().as_ref() {
                    Some(task) => left.push(Span::raw(format!("󱥸 {task}…")).fg(theme.highlight_fg)),
                    None => {
                        // shortened like neovim's pathshorten when it would crowd the keys
                        let mut path = home_relative(self.repository.path());
                        if path.chars().count() > area.width as usize / 4 {
                            path = shorten_path(&path);
                        }
                        left.push(Span::raw(format!("󰉋 {path}")).fg(theme.status_bar_fg));
                    }
                }
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
                left.push(badge(" ERROR ", theme.status_error_fg, theme));
                left.push(Span::raw(format!(" {msg}")).fg(theme.status_error_fg));
            }
        }

        // with a message, what runs in the background goes to the right
        let message = !matches!(self.shell.app_status.status_line, StatusLine::None);
        if let Some(task) = self.ctx.running_task.borrow().as_ref().filter(|_| message) {
            right.insert(0, Span::raw(format!("{task}… ")).fg(theme.highlight_fg));
        }
        // the selected commit and how many there are, like the line and column
        if let Some(state) = self.view.as_list_state() {
            let (selected, total) = state.position();
            right.push(
                Span::raw(format!("[{selected}/{total}] "))
                    .fg(theme.status_input_transient_fg)
                    .italic(),
            );
        }

        // the keys of what has the focus, as many as fit, before the rest of the right side
        let used: usize = left.iter().chain(&right).map(Span::width).sum();
        let available = (area.width as usize).saturating_sub(used + 2);
        let hints = self.fit_key_hints(available);
        if !hints.spans.is_empty() {
            let mut spans = hints.spans;
            spans.push(Span::raw(" "));
            right.splice(0..0, spans);
        }

        let line = status_bar_line(left, right, area.width);
        f.render_widget(Paragraph::new(line).style(bar), area);

        if let StatusLine::Input {
            cursor_position, ..
        } = &self.shell.app_status.status_line
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
        } else if self.shell.sidebar_collapsed {
            COLLAPSED_SIDEBAR_WIDTH.min(area.width)
        } else {
            // leave the page some room on narrow terminals, unless resized
            let default = self.ctx.ui_config.refs.width.min(area.width / 3);
            let width = self.ctx.layout.borrow().size(Panel::Sidebar, default);
            width
                .clamp(10, area.width.saturating_sub(20).max(10))
                .min(area.width)
        };
        Layout::horizontal([Constraint::Length(width), Constraint::Min(0)]).areas(area)
    }
}

// " [0] " and the border on its right
const COLLAPSED_SIDEBAR_WIDTH: u16 = 7;

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
        self.shell.app_status.view_area = view_area;
    }

    fn clear_view_image_area(&self, terminal: &mut DefaultTerminal) -> Result<(), std::io::Error> {
        if let Some(area) = self.view.image_clear_area(self.shell.app_status.view_area) {
            let area = area.intersection(terminal.size()?.into());
            for y in area.top()..area.bottom() {
                self.ctx.image_protocol.clear_line(y);
            }
        }
        Ok(())
    }

    fn open_detail(&mut self, tab: DetailTab) {
        if !matches!(self.view, View::List(_) | View::UserCommand(_)) {
            return;
        }
        let Some(commit_list_state) = self.view.take_list_state() else {
            return;
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
        if let View::Detail(_) = self.view {
            self.show_list();
        }
    }

    fn open_user_command(&mut self, user_command_number: usize) {
        match extract_user_command_by_number(user_command_number, &self.ctx).map(|c| &c.r#type) {
            Ok(UserCommandType::Inline) => {
                self.open_user_command_inline(user_command_number);
            }
            Ok(UserCommandType::Silent) => {
                self.open_user_command_silent(user_command_number);
                self.shell.app_status.pending_clear = PendingClear::Screen;
            }
            Ok(UserCommandType::Suspend) => {
                self.open_user_command_suspend(user_command_number);
                self.shell.app_status.pending_clear = PendingClear::Screen;
            }
            Err(err) => {
                self.ec.send(AppEvent::NotifyError(err));
            }
        };
    }

    fn open_user_command_inline(&mut self, user_command_number: usize) {
        let Some(commit_list_state) = self.view.as_list_state() else {
            return;
        };
        let (commit, _, refs) = selected_commit_details(self.repository, commit_list_state);
        let result = build_external_command_parameters_and_exec_command(
            &commit,
            &refs,
            commit_list_state.picked_commit_hashes(),
            user_command_number,
            self.shell.app_status.view_area,
            &self.ctx,
        );
        match result {
            Ok(output) => {
                // take list state only when the command execution is successful, to avoid losing the state when the command fails
                let Some(commit_list_state) = self.view.take_list_state() else {
                    return;
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
        let Some(commit_list_state) = self.view.as_list_state() else {
            return;
        };
        let (commit, _, refs) = selected_commit_details(self.repository, commit_list_state);
        let result = build_external_command_parameters_and_exec_command(
            &commit,
            &refs,
            commit_list_state.picked_commit_hashes(),
            user_command_number,
            self.shell.app_status.view_area,
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
        let Some(commit_list_state) = self.view.as_list_state() else {
            return;
        };
        let (commit, _, refs) = selected_commit_details(self.repository, commit_list_state);
        match build_external_command_parameters(
            &commit,
            &refs,
            commit_list_state.picked_commit_hashes(),
            user_command_number,
            self.shell.app_status.view_area,
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
        if let View::UserCommand(_) = self.view {
            self.show_list();
        }
    }

    fn run_external(&mut self, command: &[String]) {
        self.ec.suspend();
        let result = exec_interactive(command);
        self.ec.resume();
        self.shell.app_status.pending_clear = PendingClear::Screen;
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
        let typing = matches!(self.shell.app_status.status_line, StatusLine::Input { .. });
        if typing || matches!(self.view, View::Help(_)) {
            self.view.handle_event(event_with_count, key);
            return;
        }
        if self.handle_remote_event(event) {
            return;
        }
        // on the sidebar, they work on its selected ref, see `handle_sidebar_event`
        if event.home_scope() == Scope::Graph && !self.shell.sidebar_focused {
            self.handle_graph_event(event);
            return;
        }
        match event {
            UserEvent::ResizeLeft
            | UserEvent::ResizeRight
            | UserEvent::ResizeUp
            | UserEvent::ResizeDown => {
                self.move_border(event);
                return;
            }
            UserEvent::FocusLeft
            | UserEvent::FocusRight
            | UserEvent::FocusUp
            | UserEvent::FocusDown => {
                self.move_focus(event);
                return;
            }
            UserEvent::KeysToggle => {
                self.shell.keys_popup = !self.shell.keys_popup;
                return;
            }
            UserEvent::FocusPanel(0) => {
                // again on the focused sidebar, it shrinks out of the way; on a shrunk one, it grows back
                if self.shell.sidebar_focused && !self.shell.sidebar_collapsed {
                    self.shell.sidebar_collapsed = true;
                    self.set_sidebar_focused(false);
                } else {
                    self.shell.sidebar_collapsed = false;
                    self.set_sidebar_focused(true);
                }
                return;
            }
            UserEvent::RefList => {
                self.switch_page();
                return;
            }
            _ => {}
        }
        if self.shell.sidebar_focused {
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
                (0..count).for_each(|_| self.shell.sidebar.select_next());
                self.show_sidebar_selection();
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                (0..count).for_each(|_| self.shell.sidebar.select_prev());
                self.show_sidebar_selection();
            }
            UserEvent::GoToTop => {
                self.shell.sidebar.select_first();
                self.show_sidebar_selection();
            }
            UserEvent::GoToBottom => {
                self.shell.sidebar.select_last();
                self.show_sidebar_selection();
            }
            UserEvent::NavigateRight => {
                self.shell.sidebar.open_node();
                self.show_sidebar_selection();
            }
            UserEvent::NavigateLeft => {
                self.shell.sidebar.close_node();
                self.show_sidebar_selection();
            }
            UserEvent::Confirm => {
                // a folder opens or closes, else back to the page showing what is selected
                if !self.shell.sidebar.toggle_node() {
                    self.set_sidebar_focused(false);
                }
            }
            // the picked refs are cleared first
            UserEvent::Cancel | UserEvent::Close if !self.shell.sidebar.picked().is_empty() => {
                self.shell.sidebar.clear_picks();
            }
            UserEvent::Cancel | UserEvent::Close => {
                self.set_sidebar_focused(false);
            }
            UserEvent::ToggleSelect => {
                if let Err(err) = self.shell.sidebar.toggle_pick() {
                    self.ec.send(AppEvent::NotifyWarn(err));
                }
            }
            UserEvent::ShortCopy | UserEvent::FullCopy => self.copy_sidebar_ref_name(),
            event @ (UserEvent::CreateRef
            | UserEvent::CreateBranch
            | UserEvent::CreateTag
            | UserEvent::DeleteRef
            | UserEvent::EditRef
            | UserEvent::ApplyStash) => {
                let selection = self.shell.sidebar.selection();
                let picked = self.shell.sidebar.picked();
                match ref_command(self.repository, event, &selection, picked) {
                    Some(dialog) => self.open_dialog(dialog),
                    // space opens or closes a folder like enter
                    None if event == UserEvent::ApplyStash => {
                        self.shell.sidebar.toggle_node();
                    }
                    None => {}
                }
            }
            UserEvent::FocusPanel(_) => {
                // a panel of the page
                self.set_sidebar_focused(false);
                self.view.handle_event(event_with_count, key);
            }
            event if event.home_scope() == Scope::Graph => self.handle_sidebar_graph_event(event),
            _ => {}
        }
    }

    // The scope of the keys, where the focus is, see `Scope`.
    fn current_scope(&self) -> Scope {
        if self.shell.sidebar_focused {
            return Scope::Sidebar;
        }
        match &self.view {
            View::List(_) => Scope::CommitList,
            View::Detail(view) => view.scope(),
            View::LocalChanges(view) => view.scope(),
            _ => Scope::Global,
        }
    }

    // The commands on the selected row of the sidebar, then moving about.
    fn sidebar_hints(&self) -> Vec<KeyHint> {
        use RefSection::*;
        let picked = self.shell.sidebar.picked();
        if !picked.is_empty() {
            let mut hints = vec![
                hint(UserEvent::Cancel, "clear picks"),
                hint(UserEvent::ToggleSelect, "pick"),
            ];
            if picked.section == Stashes {
                hints.push(hint(UserEvent::DeleteRef, "drop"));
            } else {
                hints.extend([
                    hint(UserEvent::DeleteRef, "delete"),
                    hint(UserEvent::Diff, "diff"),
                ]);
            }
            return hints;
        }
        let mut hints = match self.shell.sidebar.selection() {
            RefSelection::LocalChanges => vec![],
            RefSelection::Folder(Remotes, name)
                if self.repository.remote_info().remotes.contains(&name) =>
            {
                vec![
                    hint(UserEvent::CreateRef, "new remote"),
                    hint(UserEvent::EditRef, "edit remote"),
                ]
            }
            RefSelection::Section(Remotes) | RefSelection::Folder(Remotes, _) => {
                vec![hint(UserEvent::CreateRef, "new remote")]
            }
            RefSelection::Section(Branches | Tags | Stashes)
            | RefSelection::Folder(Branches | Tags | Stashes, _) => vec![],
            RefSelection::Ref(Branches, _) => vec![
                hint(UserEvent::EditRef, "rename/upstream"),
                hint(UserEvent::DeleteRef, "delete"),
            ],
            RefSelection::Ref(Remotes, _) => vec![
                hint(UserEvent::CreateRef, "new remote"),
                hint(UserEvent::DeleteRef, "delete"),
            ],
            RefSelection::Ref(Tags, _) => vec![hint(UserEvent::DeleteRef, "delete")],
            RefSelection::Ref(Stashes, _) => vec![
                hint(UserEvent::ApplyStash, "apply"),
                hint(UserEvent::DeleteRef, "drop"),
            ],
        };
        // the commands of the graph, on the ref
        if let RefSelection::Ref(Branches | Remotes | Tags, _) = self.shell.sidebar.selection() {
            hints.extend([
                hint(UserEvent::CreateBranch, "new branch"),
                hint(UserEvent::CreateTag, "new tag"),
                hint(UserEvent::Checkout, "checkout"),
                hint(UserEvent::Merge, "merge"),
                hint(UserEvent::Rebase, "rebase"),
                hint(UserEvent::Diff, "diff"),
            ]);
        }
        if let RefSelection::Ref(..) = self.shell.sidebar.selection() {
            hints.push(hint(UserEvent::ToggleSelect, "pick"));
        }
        hints.push(hint(UserEvent::Confirm, "open"));
        // what copying the name works on, see `copy_sidebar_ref_name`
        if let RefSelection::Ref(Branches | Remotes | Tags, _) = self.shell.sidebar.selection() {
            hints.push(hint(UserEvent::ShortCopy, "copy name"));
        }
        hints.push(hint(UserEvent::Cancel, "back"));
        hints
    }

    fn set_sidebar_focused(&mut self, focused: bool) {
        self.shell.sidebar_focused = focused;
        if focused {
            self.view.blur();
        }
    }

    // Shows the page of the selected row: local changes, or the graph at the selected ref.
    fn show_sidebar_selection(&mut self) {
        if self.shell.sidebar.is_local_changes_selected() {
            self.show_local_changes_page();
        } else {
            // the commit details stay open, showing the commit of the ref
            if !matches!(self.view, View::Detail(_)) {
                self.show_graph_page();
            }
            let Some(name) = self.shell.sidebar.selected_ref_name() else {
                return;
            };
            match &mut self.view {
                View::List(view) => view.select_ref(&name),
                View::Detail(view) => view.select_ref(&name),
                _ => {}
            }
        }
    }

    // Tab: between the local changes page and the graph, focusing the page.
    fn switch_page(&mut self) {
        if matches!(self.view, View::LocalChanges(_)) {
            self.show_graph_page();
            self.shell.sidebar.select_branches();
        } else {
            self.show_local_changes_page();
            self.shell.sidebar.select_local_changes();
        }
        self.set_sidebar_focused(false);
    }

    // Shows the commit list alone, from a view showing it; returns whether it did.
    fn show_list(&mut self) -> bool {
        let Some(commit_list_state) = self.view.take_list_state() else {
            return false;
        };
        self.view = View::of_list(commit_list_state, self.ctx.clone(), self.ec.sender());
        true
    }

    fn show_local_changes_page(&mut self) {
        match &self.view {
            View::LocalChanges(_) => return,
            // they open again when coming back to the graph, but a comparison
            View::Detail(view) if !view.is_comparing() => {
                self.shell.parked_detail_tab = Some(view.tab())
            }
            _ => {}
        }
        let Some(commit_list_state) = self.view.take_list_state() else {
            return;
        };
        // as it was when left, or before a refresh
        let snapshot = self.shell.parked_local_changes.take();
        self.view = View::of_local_changes(
            commit_list_state,
            LocalChangesState::restore(self.repository, snapshot),
            self.ctx.clone(),
            self.ec.sender(),
        );
    }

    // The commit list, with the details when they were open before the local changes page.
    fn show_graph_page(&mut self) {
        match &self.view {
            View::List(_) => return,
            View::LocalChanges(view) => self.shell.parked_local_changes = Some(view.context()),
            _ => {}
        }
        if !self.show_list() {
            return;
        }
        if let Some(tab) = self.shell.parked_detail_tab.take() {
            self.open_detail(tab);
            // coming back to the graph, the commit list has the focus
            if let View::Detail(view) = &mut self.view {
                view.focus_list();
            }
        }
    }

    fn copy_sidebar_ref_name(&self) {
        let (name, value) = match self.shell.sidebar.selection() {
            RefSelection::Ref(RefSection::Branches | RefSection::Remotes, branch) => {
                ("Branch Name", branch)
            }
            RefSelection::Ref(RefSection::Tags, tag) => ("Tag Name", tag),
            _ => return,
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
            UserEvent::Pull if self.blocked_by_operation(event) => return true,
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
        // pressing the key again quickly runs the command without showing the popup
        if self.shell.dialog.is_some() {
            self.shell.dialog_reveal = Some(Instant::now() + QUICK_RUN_DELAY);
        }
        true
    }

    fn open_dialog(&mut self, dialog: Result<GitDialog, String>) {
        if let Some(message) = self.ctx.busy_message() {
            self.ec.send(AppEvent::NotifyWarn(message));
            return;
        }
        match dialog {
            Ok(dialog) => {
                self.shell.dialog = Some(dialog);
            }
            Err(err) => self.ec.send(AppEvent::NotifyError(err)),
        }
    }

    fn handle_dialog_key(&mut self, key: KeyEvent) {
        let Some(dialog) = self.shell.dialog.as_mut() else {
            return;
        };
        let event = self.ctx.keybind.event_in(Scope::Dialog, &key);
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
                return;
            }
        }
        // any other key shows the popup it acts on
        self.shell.dialog_reveal = None;
    }

    fn close_dialog(&mut self) {
        self.shell.dialog = None;
        self.shell.dialog_reveal = None;
    }

    // The popup, once it shows.
    fn visible_dialog(&self) -> Option<&GitDialog> {
        self.shell
            .dialog
            .as_ref()
            .filter(|_| self.shell.dialog_reveal.is_none())
    }

    // Runs the git command in the background; the app is told when it finishes.
    fn start_task(&mut self, task: GitTask) {
        if let Some(message) = self.ctx.busy_message() {
            self.ec.send(AppEvent::NotifyWarn(message));
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

    fn finish_task(&mut self, task: GitTask, result: Result<(), String>) {
        *self.ctx.running_task.borrow_mut() = None;
        // the page under the help shows the outcome, which could not be refreshed under it
        self.close_help();
        let typing = self.view.is_capturing_keys()
            || matches!(self.shell.app_status.status_line, StatusLine::Input { .. });
        if result.is_ok() && task.uses_picks {
            self.view.clear_picked_commits();
        }
        // what is left to resolve or commit is on the local changes page
        let to_local_changes = (result.is_ok() && task.show_local_changes)
            || self.repository.operation_in_progress().is_some();
        // the command may have changed files or stopped on conflicts, even when it failed;
        // refreshing would interrupt typing, so then it waits for a manual refresh
        if let Some(mut context) = self.view.refresh_context().filter(|_| !typing) {
            // the rewritten commit stays selected
            let select = task.select.as_deref().filter(|_| result.is_ok());
            if let Some(hash) = select.and_then(|revision| self.repository.resolve(revision)) {
                let list_context = context.list_context_mut();
                list_context.commit_hash = hash.as_str().to_string();
                list_context.scroll_to_top = false;
            }
            if to_local_changes && !matches!(self.view, View::LocalChanges(_)) {
                // the page opens once the repository is reloaded
                let snapshot = self.shell.parked_local_changes.take().unwrap_or_default();
                context = RefreshViewContext::LocalChanges {
                    list_context: context.list_context().clone(),
                    local_changes_context: snapshot,
                };
                self.shell.sidebar.select_local_changes();
                self.set_sidebar_focused(false);
            }
            self.ec.send(AppEvent::Refresh(context));
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

    // The commands of the commit graph, see `graph_command`.
    fn handle_graph_event(&mut self, event: UserEvent) {
        if self.blocked_by_operation(event) {
            return;
        }
        if event == UserEvent::BranchOnly && self.is_diffing() {
            self.notify_already_diffing();
            return;
        }
        if event == UserEvent::BranchOnly {
            // the commits are loaded again, of the current branch or of all
            self.shell.branch_only = !self.shell.branch_only;
            self.view.refresh();
            return;
        }
        if event == UserEvent::Diff && self.is_diffing() {
            self.notify_already_diffing();
            return;
        }
        let Some((selected, picked)) = self.graph_selection() else {
            return;
        };
        let command = graph_command(self.repository, event, selected, picked);
        self.run_graph_command(event, command);
    }

    // The commands of the commit graph on the branch, remote branch or tag selected in the
    // sidebar, see `ref_graph_command`.
    fn handle_sidebar_graph_event(&mut self, event: UserEvent) {
        if self.blocked_by_operation(event) {
            return;
        }
        if event == UserEvent::BranchOnly {
            self.handle_graph_event(event);
            return;
        }
        if event == UserEvent::Diff && self.is_diffing() {
            self.notify_already_diffing();
            return;
        }
        if event == UserEvent::Diff {
            match self.sidebar_compare() {
                Ok(compare) => self.request_ref_compare(compare),
                Err(err) => self.ec.send(AppEvent::NotifyError(err)),
            }
            return;
        }
        if !self.shell.sidebar.picked().is_empty() {
            let err = "This works on the selected ref, clear the picked ones first".to_string();
            self.ec.send(AppEvent::NotifyError(err));
            return;
        }
        let name = match self.shell.sidebar.selection() {
            RefSelection::Ref(
                RefSection::Branches | RefSection::Remotes | RefSection::Tags,
                name,
            ) => name,
            _ => {
                let err = "Select a branch or a tag for the commands of the graph".to_string();
                self.ec.send(AppEvent::NotifyError(err));
                return;
            }
        };
        let target = self
            .repository
            .all_refs()
            .into_iter()
            .find_map(|r| match r {
                Ref::Branch { name: n, target }
                | Ref::RemoteBranch { name: n, target }
                | Ref::Tag { name: n, target }
                    if *n == name =>
                {
                    Some(target.clone())
                }
                _ => None,
            });
        let Some(target) = target else {
            return;
        };
        let command = ref_graph_command(self.repository, event, &name, target);
        self.run_graph_command(event, command);
    }

    // Moves the border beside the focused panel with ctrl-left and ctrl-right (the sidebar, or the
    // files beside a diff), the one above or under it with ctrl-up and ctrl-down.
    fn move_border(&mut self, event: UserEvent) {
        let (vertical, step) = match event {
            UserEvent::ResizeLeft => (true, -2),
            UserEvent::ResizeRight => (true, 2),
            UserEvent::ResizeUp => (false, -1),
            _ => (false, 1),
        };
        let sidebar = (!self.shell.sidebar_collapsed).then_some(Panel::Sidebar);
        let panel = match &self.view {
            _ if self.shell.sidebar_focused => sidebar.filter(|_| vertical),
            View::Detail(view) if vertical && view.shows_tree() => Some(Panel::DetailTree),
            View::Detail(_) if !vertical => Some(Panel::Details),
            View::UserCommand(_) if !vertical => Some(Panel::UserCommand),
            View::LocalChanges(_) if vertical => Some(Panel::Files),
            View::LocalChanges(view) => Some(view.height_panel()),
            _ if vertical => sidebar,
            _ => None,
        };
        if let Some(panel) = panel {
            self.ctx.layout.borrow_mut().move_border(panel, step);
        }
    }

    // Moves the focus to the panel that way: the sidebar on the left of the page, the page on its
    // right, else between the panels of the page.
    fn move_focus(&mut self, event: UserEvent) {
        let direction = match event {
            UserEvent::FocusLeft => Direction::Left,
            UserEvent::FocusRight => Direction::Right,
            UserEvent::FocusUp => Direction::Up,
            _ => Direction::Down,
        };
        if self.shell.sidebar_focused {
            if direction == Direction::Right {
                self.set_sidebar_focused(false);
            }
        } else if !self.view.move_focus(direction) && direction == Direction::Left {
            self.shell.sidebar_collapsed = false;
            self.set_sidebar_focused(true);
        }
    }

    // Whether a diff of two commits or refs is open, also under the details of one of its commits.
    fn is_diffing(&self) -> bool {
        self.shell.compare.is_some()
            || self.shell.compare_request.is_some()
            || matches!(&self.view, View::Detail(view) if view.is_comparing())
    }

    fn notify_already_diffing(&self) {
        let msg = "A diff is open, close it first (esc)".to_string();
        self.ec.send(AppEvent::NotifyWarn(msg));
    }

    // Whether `event` moves or rewrites HEAD while a rebase, merge, cherry-pick or revert waits to
    // be continued or aborted, which git refuses or tangles up; tells so when it does.
    fn blocked_by_operation(&self, event: UserEvent) -> bool {
        let Some(operation) = moves_head(event)
            .then(|| self.repository.operation_in_progress())
            .flatten()
        else {
            return false;
        };
        let msg = format!(
            "A {} is in progress, continue or abort it on the local changes page first",
            operation.name()
        );
        self.ec.send(AppEvent::NotifyWarn(msg));
        true
    }

    // The refs to diff from the sidebar: the two picked ones, else HEAD and the picked or
    // selected one.
    fn sidebar_compare(&self) -> Result<RefCompare, String> {
        let picked = self.shell.sidebar.picked();
        let (section, names) = if picked.is_empty() {
            match self.shell.sidebar.selection() {
                RefSelection::Ref(section, name) => (section, vec![name]),
                _ => return Err("Select a branch or a tag to diff it".into()),
            }
        } else {
            (picked.section, picked.names.clone())
        };
        if section == RefSection::Stashes {
            return Err("Diff compares branches and tags".into());
        }
        let head = || {
            self.repository
                .current_branch()
                .unwrap_or_else(|| "HEAD".into())
        };
        let (base, target) = match names.as_slice() {
            [one] => (head(), one.clone()),
            [one, two] => (one.clone(), two.clone()),
            more => return Err(format!("Diff compares two refs, {} are picked", more.len())),
        };
        if base == target {
            return Err(format!(
                "{base} is checked out, there is nothing to compare"
            ));
        }
        Ok(RefCompare { base, target })
    }

    // Diffs refs from the sidebar: the graph is loaded again with only their history, then
    // their comparison opens, see `open_ref_compare`.
    fn request_ref_compare(&mut self, compare: RefCompare) {
        let Some(context) = self.view.refresh_context() else {
            return;
        };
        // the local changes page is kept as it was, for coming back to it
        if let View::LocalChanges(view) = &self.view {
            self.shell.parked_local_changes = Some(view.context());
        }
        self.shell.compare_request = Some(compare);
        self.shell.sidebar.clear_picks();
        self.set_sidebar_focused(false);
        let list_context = context.list_context().clone();
        self.ec
            .send(AppEvent::Refresh(RefreshViewContext::List { list_context }));
    }

    // Opens the comparison of the refs diffed from the sidebar, once the repository is loaded
    // for it, and again after a refresh while it is open.
    fn open_ref_compare(&mut self) {
        let Some(compare) = self
            .shell
            .compare_request
            .take()
            .or_else(|| self.shell.compare.take())
        else {
            return;
        };
        let resolved = self
            .repository
            .resolve(&compare.base)
            .zip(self.repository.resolve(&compare.target));
        let Some((base, target)) = resolved else {
            // gone meanwhile, the graph shows all again
            self.ec.send(AppEvent::NotifyError(format!(
                "Cannot diff {} and {}, one of them is gone",
                compare.base, compare.target
            )));
            self.view.refresh();
            return;
        };
        let title = format!("Diff {} → {}", compare.base, compare.target);
        self.open_compare(base, target, title);
        self.shell.compare = Some(compare);
    }

    // Once the comparison of refs diffed from the sidebar is closed, the graph shows all the
    // history again.
    fn end_ref_compare(&mut self) {
        let comparing = match &self.view {
            View::Detail(view) => view.is_comparing(),
            // over the comparison
            View::Help(_) => true,
            _ => false,
        };
        if self.shell.compare.is_some() && !comparing {
            self.shell.compare = None;
            self.view.refresh();
        }
    }

    fn run_graph_command(
        &mut self,
        event: UserEvent,
        command: Option<Result<GraphCommand, String>>,
    ) {
        match command {
            None => {}
            Some(Ok(GraphCommand::Dialog(dialog))) => self.open_dialog(Ok(dialog)),
            Some(Ok(GraphCommand::AmendHead)) => self.amend_on_local_changes(),
            Some(Ok(GraphCommand::Compare {
                base,
                target,
                title,
            })) => self.open_compare(base, target, title),
            // the popups tell first that a command is running
            Some(Err(err)) if event != UserEvent::Diff => self.open_dialog(Err(err)),
            Some(Err(err)) => self.ec.send(AppEvent::NotifyError(err)),
        }
    }

    // Whether reloading in the background would get in the way, like lazygit pausing its refreshes:
    // while a git command runs, a popup is open or something is being typed or confirmed.
    fn refresh_paused(&self) -> bool {
        self.ctx.busy_message().is_some()
            || self.shell.dialog.is_some()
            || self.view.is_capturing_keys()
            || matches!(self.view, View::Help(_))
            || matches!(self.shell.app_status.status_line, StatusLine::Input { .. })
    }

    // Reloads everything when the branches or HEAD changed since they were loaded, from outside the
    // app, like lazygit.
    fn check_refs(&mut self, snapshot: String) {
        let previous = self.shell.refs_snapshot.replace(snapshot);
        if previous.is_some_and(|p| Some(&p) != self.shell.refs_snapshot.as_ref()) {
            self.view.refresh();
        }
    }

    // Reloads the local changes, made outside the app too: the page showing them, else their
    // count in the sidebar.
    fn reload_files(&mut self) {
        match &mut self.view {
            View::LocalChanges(view) => view.reload_files(),
            _ => {
                let count = self.repository.reload_local_changes();
                self.shell.sidebar.set_local_change_count(count);
            }
        }
    }

    // Shows the local changes page with the last commit being amended.
    fn amend_on_local_changes(&mut self) {
        self.show_local_changes_page();
        self.shell.sidebar.select_local_changes();
        self.set_sidebar_focused(false);
        if let View::LocalChanges(view) = &mut self.view {
            if let Err(err) = view.start_amend() {
                self.ec.send(AppEvent::NotifyError(err));
            }
        }
    }

    // Shows the changes from `base` to `target` in the details.
    fn open_compare(&mut self, base: CommitHash, target: CommitHash, title: String) {
        if !matches!(self.view, View::List(_) | View::Detail(_)) {
            return;
        }
        let Some(commit_list_state) = self.view.take_list_state() else {
            return;
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
            view.start_compare(base, target, title);
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
            view.select_older_commit();
        } else if let View::UserCommand(ref mut view) = self.view {
            view.select_older_commit(
                self.repository,
                self.shell.app_status.view_area,
                build_external_command_parameters_and_exec_command,
            );
        }
    }

    fn select_newer_commit(&mut self) {
        if let View::Detail(ref mut view) = self.view {
            view.select_newer_commit();
        } else if let View::UserCommand(ref mut view) = self.view {
            view.select_newer_commit(
                self.repository,
                self.shell.app_status.view_area,
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
                let tab = self.shell.parked_detail_tab.take().unwrap_or_default();
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
                self.shell.parked_local_changes = Some(local_changes_context);
                self.show_local_changes_page();
            }
        }
    }

    fn clear_status_line(&mut self) {
        self.shell.app_status.status_line = StatusLine::None;
    }

    fn update_status_input(&mut self, message: String, cursor_position: u16, metadata: String) {
        self.shell.app_status.status_line = StatusLine::Input {
            message,
            cursor_position,
            metadata,
        };
    }

    fn update_status_transient(&mut self, msg: String) {
        self.shell.app_status.status_line = StatusLine::Transient(msg);
    }

    fn update_search_result(&mut self, message: String, options: String, matched: bool) {
        self.shell.app_status.status_line = StatusLine::SearchResult {
            message,
            options,
            matched,
        };
    }

    fn info_notification(&mut self, msg: String) {
        self.shell.app_status.status_line = StatusLine::NotificationInfo(msg);
    }

    fn success_notification(&mut self, msg: String) {
        self.shell.app_status.status_line = StatusLine::NotificationSuccess(msg);
    }

    fn warn_notification(&mut self, msg: String) {
        self.shell.app_status.status_line = StatusLine::NotificationWarn(msg);
    }

    fn error_notification(&mut self, msg: String) {
        self.shell.app_status.status_line = StatusLine::NotificationError(msg);
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

// Whether `event` moves or rewrites HEAD, which waits while an operation is in progress.
fn moves_head(event: UserEvent) -> bool {
    matches!(
        event,
        UserEvent::Rebase
            | UserEvent::Merge
            | UserEvent::Checkout
            | UserEvent::CherryPick
            | UserEvent::Squash
            | UserEvent::Revert
            | UserEvent::AmendCommit
            | UserEvent::DropCommit
            | UserEvent::Pull
    )
}

// The path with the home directory as ~, like a shell prompt.
fn home_relative(path: &std::path::Path) -> String {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    match home
        .as_deref()
        .and_then(|home| path.strip_prefix(home).ok())
    {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

// The directories of the path by their first letter, like "~/p/spoon".
fn shorten_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    let last = parts.len().saturating_sub(1);
    parts
        .iter()
        .enumerate()
        .map(|(i, part)| match part.chars().next() {
            // hidden directories keep the letter after the dot
            Some('.') if i < last => part.chars().take(2).collect(),
            Some(c) if i < last => c.to_string(),
            _ => part.to_string(),
        })
        .collect::<Vec<_>>()
        .join("/")
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
                                                        // minus the top and bottom borders
    let area_height = user_command_height(ctx, view_area.height).saturating_sub(2);
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

    #[test]
    fn test_shorten_path() {
        assert_eq!(shorten_path("~/projects/spoon"), "~/p/spoon");
        assert_eq!(shorten_path("/tmp/.cache/dr"), "/t/.c/dr");
        assert_eq!(shorten_path("~"), "~");
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
