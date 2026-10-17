use std::{
    cell::{Cell, RefCell},
    io::{self, Write},
    rc::Rc,
    sync::mpsc::RecvTimeoutError,
    time::{Duration, Instant},
};

use ratatui::{
    backend::Backend,
    crossterm::{
        event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind},
        SynchronizedUpdate,
    },
    layout::{Constraint, Layout, Rect},
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
    DefaultTerminal, Frame, Terminal,
};
use rustc_hash::{FxHashMap, FxHashSet};
use unicode_width::UnicodeWidthStr;

use crate::{
    browse::{self, LinkTarget},
    color::ColorTheme,
    config::{CoreConfig, CursorType, UiConfig},
    event::{hint, AppEvent, EventController, KeyHint, UserEvent},
    external::{copy_to_clipboard, exec_interactive},
    git::{
        moves_head_command, run_task, AfterTask, Commit, CommitHash, FileChange, FilesSnapshot,
        GitTask, Head, Ref, Repository,
    },
    graph::{Fold, GraphImageManager},
    graph_commands::{graph_command, ref_graph_command, GraphCommand},
    keybind::{keys_to_string, KeyBind, Scope, SequenceMatch},
    layout::{fit_size, Direction, Edge, Panel, PanelLayout},
    protocol::ImageProtocol,
    ref_commands::{ref_command, ref_command_applies},
    search::SearchOptions,
    view::{Compare, HelpView, LocalChangesRefreshViewContext, RefreshViewContext, View, ViewKind},
    widget::{
        command_log::{CommandLog, CommandLogState},
        commit_list::{CommitInfo, CommitListState},
        commit_tabs::DetailTab,
        git_dialog::{DialogOutcome, GitDialog, GitDialogWidget, RemoteAction},
        keys_popup::KeysPopup,
        local_changes::LocalChangesState,
        notices::{push_notice, render_notices, Notice},
        ref_list::{RefList, RefListState, RefSection, RefSelection, SidebarMode, DETACHED_HEAD},
        style::{key_hints, spinner, SPINNER_RATE},
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
}

#[derive(Clone, Copy)]
pub enum InitialSelection {
    Latest,
    Head,
}

// How the app starts, from the command line: on the local changes page; and fetching the remotes, which the tests leave out, see `fetch_on_launch`.
#[derive(Clone, Copy)]
pub struct Launch {
    pub local_changes: bool,
    pub fetch: bool,
}

// The longest the fetch on launch runs, for a remote that never answers; long enough for a big
// first fetch over a slow link, which would otherwise start over on each launch.
const LAUNCH_FETCH_TIMEOUT: Duration = Duration::from_secs(300);

// How long the fetch, pull and push popups wait for their key to be pressed again before showing,
// as a double press runs the command right away.
const QUICK_RUN_DELAY: Duration = Duration::from_millis(200);

pub enum Ret {
    Quit,
    // reload the repository and show the view of the context again
    Refresh(Box<RefreshViewContext>),
    // draw the graph again, of the repository as loaded, see `AppShell::folded_merges`
    Regraph(Box<RefreshViewContext>),
}

#[derive(Debug)]
pub struct AppContext {
    pub keybind: KeyBind,
    pub core_config: CoreConfig,
    pub ui_config: UiConfig,
    pub color_theme: ColorTheme,
    pub image_protocol: ImageProtocol,
    // the git command running in the background, kept across refreshes
    pub running_task: RefCell<Option<RunningTask>>,
    // the keys of the hints in the status bar, see `short_keys`
    pub key_names: RefCell<FxHashMap<Vec<UserEvent>, String>>,
    // the sizes of the panels, set by the user, kept across refreshes
    pub layout: RefCell<PanelLayout>,
    // long lines of the diffs and file previews wrapped, see `UserEvent::ToggleWrap`
    pub wrap: Cell<bool>,
    // the search options of the commit list, which the file trees, diffs and previews search
    // with too
    pub search_options: Cell<SearchOptions>,
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
        let running = self.running_task.borrow();
        running
            .as_ref()
            .map(|running| format!("{}, wait for it to finish", running.task.running))
    }
}

// The git command running in the background, and what waits for it to finish.
#[derive(Debug)]
pub struct RunningTask {
    pub task: GitTask,
    // HEAD before committing the message of the local changes page, which is cleared once the
    // commit is made, see `finish_task`
    head_before_commit: Option<CommitHash>,
    // quit was asked meanwhile, the app quits once it finishes
    quit_after: bool,
}

// The longest the events waiting are handled for before drawing, see `App::run`.
const MAX_BATCH: Duration = Duration::from_millis(100);

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
}

#[derive(Debug, Default)]
struct AppStatus {
    status_line: StatusLine,
    // the keys pressed so far of a key sequence, like "]" of "]m"
    pending_keys: Vec<KeyEvent>,
    view_area: Rect,
    pending_clear: PendingClear,
    // where the graph images were drawn in the last frame, see `App::draw`
    images: Vec<(u16, u16, u16)>,
}

// The parts of the app that don't depend on the loaded repository, kept when it is reloaded:
// the sidebar, the focus, the pages not shown, the popup and the status line.
#[derive(Debug)]
pub struct AppShell {
    // the refs on the left, beside the graph
    sidebar: RefListState,
    sidebar_mode: SidebarMode,
    // the local changes page while the graph is shown, to come back to it as it was
    parked_local_changes: Option<LocalChangesRefreshViewContext>,
    // the tab of the commit details while the local changes page is shown, to reopen them
    parked_detail_tab: Option<DetailTab>,
    // the popup confirming a git command, over any view
    dialog: Option<Popup>,
    // what the last frame showed, None before the first
    last_frame: Option<FrameLayout>,
    app_status: AppStatus,
    // the branches and HEAD as loaded, see `git::refs_snapshot`, so the app's own changes are not
    // taken for outside ones
    refs_snapshot: Option<String>,
    // the history the graph shows
    graph_filter: GraphFilter,
    // the keys of the focused panel shown in a popup, see `UserEvent::KeysToggle`
    keys_popup: bool,
    // the help or the command log, over the page and the sidebar
    overlay: Option<Overlay>,
    // errors and warnings at the top right, until dismissed
    notices: Vec<Notice>,
    // a rebase, merge, cherry-pick or revert waits to be finished, as last loaded, for the key hints
    operation_shown: bool,
    // the merge commits folded in the graph, see `UserEvent::NavigateLeft`
    folded_merges: FxHashSet<CommitHash>,
}

// What covers the page and the sidebar, taking the keys: the keys of the whole app (see
// `UserEvent::HelpToggle`), or the git commands run (see `UserEvent::CommandLog`), over everything.
#[derive(Debug)]
enum Overlay {
    Help(HelpView),
    CommandLog(CommandLogState),
}

// A popup confirming a git command; `reveal` is when it shows, while it waits for the key opening
// it to be pressed again, which runs the command right away, see `QUICK_RUN_DELAY`.
#[derive(Debug)]
struct Popup {
    dialog: GitDialog,
    reveal: Option<Instant>,
}

// The history the graph shows, its commits only are loaded.
#[derive(Debug, Default, Clone)]
enum GraphFilter {
    #[default]
    All,
    // a branch's, the current one for None, see `UserEvent::BranchOnly`
    BranchOnly(Option<String>),
    // that of two refs diffed from the sidebar while their comparison is shown, going back to
    // `before` once it isn't, see `end_stale_ref_compare`
    Compare {
        refs: RefCompare,
        before: Box<GraphFilter>,
    },
}

// The changes from `base` to `target`, refs (or HEAD) diffed from the sidebar.
#[derive(Debug, Clone)]
struct RefCompare {
    base: String,
    target: String,
}

impl AppShell {
    fn sidebar_focused(&self) -> bool {
        self.sidebar_mode == SidebarMode::Focused
    }

    fn sidebar_collapsed(&self) -> bool {
        self.sidebar_mode == SidebarMode::Collapsed
    }

    // The revisions whose history the graph shows, None for all.
    pub fn graph_revisions(&self) -> Option<Vec<String>> {
        match &self.graph_filter {
            GraphFilter::All => None,
            GraphFilter::BranchOnly(None) => Some(vec!["HEAD".into()]),
            // not a tag of the same name
            GraphFilter::BranchOnly(Some(branch)) => Some(vec![format!("refs/heads/{branch}")]),
            GraphFilter::Compare { refs, .. } => Some(vec![refs.base.clone(), refs.target.clone()]),
        }
    }

    // What the graph shows, when not all of the history, for the title of the commit list.
    fn graph_filter_note(&self, repository: &Repository) -> Option<String> {
        match &self.graph_filter {
            GraphFilter::All => None,
            GraphFilter::BranchOnly(branch) => Some(format!(
                "{} only",
                branch
                    .clone()
                    .or_else(|| repository.current_branch())
                    .unwrap_or_else(|| "HEAD".into())
            )),
            GraphFilter::Compare { refs, .. } => Some(format!("{} ↔ {}", refs.base, refs.target)),
        }
    }

    pub fn folded_merges(&self) -> &FxHashSet<CommitHash> {
        &self.folded_merges
    }

    pub fn reset_graph_filter(&mut self) {
        self.graph_filter = GraphFilter::All;
    }

    pub fn notify_error(&mut self, message: String) {
        push_notice(&mut self.notices, Notice::error(message));
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
        launch: Launch,
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
            .is_some_and(|s| matches!(s.graph_filter, GraphFilter::Compare { .. }));
        let commits = graph
            .commits
            .iter()
            .enumerate()
            .map(|(i, commit)| {
                let refs = repository.refs(&commit.commit_hash);
                for r in &refs {
                    ref_name_to_commit_index_map.insert(r.name(), i);
                }
                // the detached HEAD's row in the sidebar selects it
                if matches!(repository.head(), Head::Detached { target } if *target == commit.commit_hash)
                {
                    ref_name_to_commit_index_map.insert("HEAD", i);
                }
                // comparing two refs, both are shown alike
                let on_head = comparing_refs || head_history.contains(&commit.commit_hash);
                CommitInfo::new(commit, refs, repository.branch_tracking(), on_head)
            })
            .collect();
        let graph_cell_width = graph_image_manager.graph_cell_width();
        let search_options = SearchOptions {
            target: ctx.core_config.search.target,
            smart_case: ctx.core_config.search.smart_case,
            fuzzy: ctx.core_config.search.fuzzy,
        };
        ctx.search_options.set(search_options);
        let mut commit_list_state = CommitListState::new(
            commits,
            graph_image_manager,
            graph_cell_width,
            ref_name_to_commit_index_map,
            search_options,
            ctx.ui_config.common.scrolloff as usize,
        );
        // the checked-out branch, or the detached HEAD, marked in the graph and the sidebar
        commit_list_state.set_head(repository.head().clone());
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
        let loaded_refs = repository.refs_snapshot().map(String::from);
        let mut shell = match shell {
            Some(mut shell) => {
                shell
                    .sidebar
                    .set_refs(&refs, repository.remotes(), repository.head());
                shell.refs_snapshot = loaded_refs;
                shell
            }
            None => AppShell {
                sidebar: {
                    let mut sidebar = RefListState::new(&refs, repository.remotes());
                    sidebar.set_head(repository.head());
                    sidebar
                },
                sidebar_mode: SidebarMode::Shown,
                parked_local_changes: None,
                parked_detail_tab: None,
                dialog: None,
                last_frame: None,
                app_status: AppStatus::default(),
                refs_snapshot: loaded_refs,
                graph_filter: GraphFilter::All,
                keys_popup: false,
                overlay: None,
                notices: Vec::new(),
                operation_shown: false,
                folded_merges: FxHashSet::default(),
            },
        };
        // gone, like after a rebase
        shell
            .folded_merges
            .retain(|h| repository.commit(h).is_some());
        shell.operation_shown = repository.operation_in_progress().is_some();
        // a git command still running from before the reload
        let running = ctx
            .running_task
            .borrow()
            .as_ref()
            .map(|running| running.task.running.clone());
        repository.set_busy(running);
        shell
            .sidebar
            .set_tracking(repository.branch_tracking().clone());

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
            None => {
                if launch.local_changes {
                    app.show_local_changes_page();
                }
                if launch.fetch && !repository.remotes().is_empty() {
                    app.fetch_on_launch();
                }
            }
        }
        app.end_stale_ref_compare();

        app
    }

    // Takes the page out, leaving none until the next one, built on its commit list, replaces it.
    fn take_list_state(&mut self) -> Option<CommitListState<'a>> {
        std::mem::take(&mut self.view).into_list_state()
    }

    // Shows `view` in place of the page; once the comparison of refs diffed from the sidebar is
    // closed, the graph shows the history it showed before again.
    fn replace_view(&mut self, view: View<'a>) {
        self.view = view;
        self.end_stale_ref_compare();
    }

    // The graph shows the history of the refs diffed from the sidebar only while their comparison
    // is shown: once it closed, or wasn't opened again after a reload (a ref gone, another page
    // shown), the graph shows the history it showed before.
    fn end_stale_ref_compare(&mut self) {
        let comparing = matches!(&self.view, View::Detail(view) if view.is_comparing());
        if let (GraphFilter::Compare { before, .. }, false) = (&self.shell.graph_filter, comparing)
        {
            self.shell.graph_filter = (**before).clone();
            self.view.refresh();
        }
    }
}

impl App<'_> {
    // What is kept of the app when the repository is reloaded.
    pub fn into_shell(self) -> AppShell {
        self.shell
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<Ret, std::io::Error> {
        loop {
            self.prepare_render(terminal.size()?.into());
            // Keep image deletion, uploads, and text updates in the same frame.
            // Preparation and event handling stay outside the synchronized update.
            io::stdout().sync_update(|_| self.draw(terminal))??;
            // woken up to show the popup waiting for a second press, or to drop expired notices
            let expiry = self.shell.notices.iter().map(|n| n.until).min();
            let reveal = self.shell.dialog.as_ref().and_then(|popup| popup.reveal);
            let event = match reveal.into_iter().chain(expiry).min() {
                Some(at) => match self
                    .ec
                    .recv_timeout(at.saturating_duration_since(Instant::now()))
                {
                    Some(event) => event,
                    None => {
                        self.expire(Instant::now());
                        continue;
                    }
                },
                None => self.ec.recv(),
            };
            // handles it and what came meanwhile, like the repeats of a held key, before drawing
            // again, so the screen keeps up; what the views load for a selection waits for the draw
            let started = Instant::now();
            let mut next = Some(event);
            while let Some(event) = next {
                if let Some(ret) = self.handle_event(event)? {
                    return Ok(ret);
                }
                next = (started.elapsed() < MAX_BATCH)
                    .then(|| self.ec.try_recv())
                    .flatten();
            }
        }
    }

    // Handles one event; returns what ends the app or reloads it.
    fn handle_event(&mut self, event: AppEvent) -> Result<Option<Ret>, std::io::Error> {
        match event {
            AppEvent::Key(key) => {
                match self.shell.app_status.status_line {
                    StatusLine::None | StatusLine::Input { .. } => {
                        // do nothing
                    }
                    StatusLine::Transient(_)
                    | StatusLine::SearchResult { .. }
                    | StatusLine::NotificationInfo(_)
                    | StatusLine::NotificationSuccess(_) => {
                        // Clear message and pass key input as is
                        self.clear_status_line();
                    }
                }

                if matches!(self.shell.overlay, Some(Overlay::CommandLog(_))) {
                    let event = self.ctx.keybind.event_in(Scope::Global, &key);
                    self.handle_command_log_event(event);
                    return Ok(None);
                }
                if self.shell.dialog.is_some() {
                    self.handle_dialog_key(key);
                    return Ok(None);
                }
                if matches!(self.shell.overlay, Some(Overlay::Help(_))) {
                    let event = self.ctx.keybind.event_in(Scope::Global, &key);
                    let quit = self.force_quit(&key);
                    if let (false, Some(event), Some(Overlay::Help(help))) =
                        (quit, event, self.shell.overlay.as_mut())
                    {
                        help.handle_event(event);
                    }
                    return Ok(None);
                }

                let scope = self.current_scope();
                if self.view.is_capturing_keys() {
                    if !self.force_quit(&key) {
                        self.view.handle_event(UserEvent::Unknown, key);
                    }
                    return Ok(None);
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
                        SequenceMatch::Partial => return Ok(None),
                        SequenceMatch::Complete(ue) => {
                            self.shell.app_status.pending_keys.clear();
                            self.dispatch(ue, key);
                            return Ok(None);
                        }
                        SequenceMatch::None => {
                            self.shell.app_status.pending_keys.clear();
                            // Esc only cancels the started sequence
                            if had_pending
                                && self.ctx.keybind.event_in(scope, &key) == Some(UserEvent::Cancel)
                            {
                                return Ok(None);
                            }
                            // otherwise the key goes on as usual
                        }
                    }
                }

                match self.ctx.keybind.event_in(scope, &key) {
                    Some(ue) => self.dispatch(ue, key),
                    None => {
                        if let StatusLine::Input { .. } = self.shell.app_status.status_line {
                            // In input mode, pass all key events to the view
                            // fixme: currently, the only thing that processes key_event is searching the list,
                            //        so this probably works, but it's not the right process...
                            self.view.handle_event(UserEvent::Unknown, key);
                        }
                    }
                }
            }
            AppEvent::Mouse(mouse)
                if matches!(
                    mouse.kind,
                    MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
                ) =>
            {
                self.scroll_wheel(mouse.kind == MouseEventKind::ScrollDown);
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
                // quitting would kill the git command half done (a stash never popped), so it
                // waits; asking again quits right away
                let mut running = self.ctx.running_task.borrow_mut();
                if let Some(running) = running.as_mut().filter(|running| !running.quit_after) {
                    running.quit_after = true;
                    self.ec.send(AppEvent::NotifyWarn(format!(
                        "Quitting once {} finishes, quit again to quit now",
                        running.task.running
                    )));
                    return Ok(None);
                }
                drop(running);
                self.cleanup_graph_images()?;
                return Ok(Some(Ret::Quit));
            }
            AppEvent::OpenDetail(tab) => {
                self.open_detail(tab);
            }
            AppEvent::CloseDetail => {
                if let View::Detail(_) = self.view {
                    self.show_list();
                }
            }
            AppEvent::RunExternal(command) => {
                self.run_external(&command);
            }
            AppEvent::StartTask(task) => self.start_task(task),
            AppEvent::TaskFinished { task, result } => {
                if self.finish_task(task, result) {
                    self.ec.send(AppEvent::Quit);
                }
            }
            // what changed meanwhile, without reloading everything when nothing did, loaded in the
            // background
            AppEvent::LocalChangesChanged | AppEvent::FocusGained => self.ec.refresh_now(),
            AppEvent::FilesTick(files) if !self.refresh_paused() => self.reload_files(files),
            AppEvent::RefsSnapshot(snapshot) if !self.refresh_paused() => {
                self.check_refs(snapshot);
            }
            // checked again on the next tick
            AppEvent::FilesTick(_) | AppEvent::RefsSnapshot(_) => {}
            // only redraws
            AppEvent::SpinnerTick => {}
            AppEvent::OpenHelp => {
                let help = HelpView::new(self.ctx.clone(), self.ec.sender());
                self.shell.overlay = Some(Overlay::Help(help));
            }
            AppEvent::CloseHelp => {
                if matches!(self.shell.overlay, Some(Overlay::Help(_))) {
                    self.shell.overlay = None;
                }
            }
            AppEvent::SelectOlderCommit => {
                self.select_older_commit();
            }
            AppEvent::SelectNewerCommit => {
                self.select_newer_commit();
            }
            AppEvent::OpenDialog(dialog) => self.open_dialog(Ok(*dialog)),
            AppEvent::CopyToClipboard { name, value } => {
                self.copy_to_clipboard(name, value);
            }
            AppEvent::Link { target, copy } => self.open_link(&target, copy),
            AppEvent::OpenUrl { url, copy } => self.open_url(url, copy),
            AppEvent::Refresh(context) => {
                self.cleanup_graph_images()?;
                // the graph is drawn again from scratch
                self.shell.last_frame = None;
                return Ok(Some(Ret::Refresh(Box::new(context))));
            }
            AppEvent::FoldMerge { commit, fold } => {
                match fold {
                    Fold::Folded => self.shell.folded_merges.insert(commit),
                    Fold::Unfolded => self.shell.folded_merges.remove(&commit),
                };
                if let Some(context) = self.view.refresh_context() {
                    self.cleanup_graph_images()?;
                    self.shell.last_frame = None;
                    return Ok(Some(Ret::Regraph(Box::new(context))));
                }
            }
            AppEvent::ClearStatusLine => {
                self.clear_status_line();
            }
            AppEvent::UpdateStatusInput {
                message,
                cursor_position,
                metadata,
            } => {
                self.shell.app_status.status_line = StatusLine::Input {
                    message,
                    cursor_position,
                    metadata,
                };
            }
            AppEvent::UpdateStatusTransient(msg) => {
                self.shell.app_status.status_line = StatusLine::Transient(msg);
            }
            AppEvent::UpdateSearchResult {
                message,
                options,
                matched,
            } => {
                self.shell.app_status.status_line = StatusLine::SearchResult {
                    message,
                    options,
                    matched,
                };
            }
            AppEvent::NotifyInfo(msg) => {
                self.shell.app_status.status_line = StatusLine::NotificationInfo(msg);
            }
            AppEvent::NotifySuccess(msg) => {
                self.shell.app_status.status_line = StatusLine::NotificationSuccess(msg);
            }
            AppEvent::NotifyWarn(msg) => {
                self.warn_notification(msg);
            }
            AppEvent::NotifyError(msg) => {
                push_notice(&mut self.shell.notices, Notice::error(msg));
            }
        }
        Ok(None)
    }

    // Lays the page out in the screen's `area` before handling the next events and drawing.
    fn prepare_render(&mut self, area: Rect) {
        let [_, view_area, _] = split_app_areas(area);
        let [sidebar_area, view_area] = self.split_sidebar(view_area);
        self.shell.sidebar.set_height(sidebar_area);
        self.update_state(view_area);
        self.view.update_layout(view_area);
        self.view.prepare_graph_uploads();
    }

    fn draw(&mut self, terminal: &mut DefaultTerminal) -> Result<(), std::io::Error> {
        let keys = self.keys_popup_keys();
        let clear = self.frame_clear();
        match clear {
            PendingClear::None => {}
            PendingClear::ViewImages => self.clear_view_image_area(terminal)?,
            PendingClear::Screen => {
                self.ctx.image_protocol.clear();
                clear_fullscreen(terminal)?;
            }
        }
        self.flush_pending_graph_uploads()?;
        let images = self.shell.app_status.images.clone();
        terminal.draw(|f| self.render(f, &keys))?;
        // a graph image covered by something drawn over it, like a popup or the help, or no longer
        // covered: the terminal would keep showing what isn't drawn again, so the frame is drawn
        // again on a cleared screen
        if clear != PendingClear::Screen && self.shell.app_status.images != images {
            self.ctx.image_protocol.clear();
            clear_fullscreen(terminal)?;
            terminal.draw(|f| self.render(f, &keys))?;
        }
        Ok(())
    }

    // The keys of what has the focus: the popup, the sidebar or the view.
    fn local_hints(&self) -> Vec<KeyHint> {
        if matches!(self.shell.overlay, Some(Overlay::CommandLog(_))) {
            vec![
                (
                    vec![UserEvent::NavigateDown, UserEvent::NavigateUp],
                    "move".into(),
                ),
                (
                    vec![UserEvent::HalfPageDown, UserEvent::HalfPageUp],
                    "scroll output".into(),
                ),
                hint(UserEvent::ShortCopy, "copy"),
                hint(UserEvent::Cancel, "close"),
            ]
        } else if let Some(dialog) = self.visible_dialog() {
            dialog.key_hints()
        } else if matches!(self.shell.overlay, Some(Overlay::Help(_))) {
            vec![hint(UserEvent::Cancel, "close")]
        } else if self.shell.sidebar_focused() {
            self.sidebar_hints()
        } else {
            self.view.key_hints()
        }
    }

    // The keys and what they do, without those waiting for the operation in progress (their
    // commands are refused, see `blocked_by_operation`) or bound to no key.
    fn shown_keys(&self, mut hints: Vec<KeyHint>) -> Vec<(String, String)> {
        if self.shell.operation_shown {
            hints.retain(|(events, _)| !events.iter().copied().any(moves_head));
        }
        hints
            .into_iter()
            .map(|(events, description)| (self.ctx.short_keys(&events), description))
            .filter(|(keys, _)| !keys.is_empty())
            .collect()
    }

    // The keys working everywhere, for the status bar: the remotes, switching pages, the keys
    // popup and the help.
    fn global_hints(&self) -> Vec<KeyHint> {
        vec![
            hint(UserEvent::KeysToggle, "keys"),
            hint(UserEvent::Push, "push"),
            hint(UserEvent::Pull, "pull"),
            hint(UserEvent::Fetch, "fetch"),
            hint(UserEvent::RefList, "switch page"),
            hint(UserEvent::Refresh, "refresh"),
            hint(UserEvent::CommandLog, "log"),
            hint(UserEvent::HelpToggle, "help"),
        ]
    }

    // Moves in the command log, scrolls the output of the selected command, copies it, or closes.
    fn handle_command_log_event(&mut self, event: Option<UserEvent>) {
        let Some(Overlay::CommandLog(state)) = self.shell.overlay.as_mut() else {
            return;
        };
        match event {
            Some(UserEvent::ForceQuit | UserEvent::Quit) => self.ec.send(AppEvent::Quit),
            Some(UserEvent::Cancel | UserEvent::Close | UserEvent::CommandLog) => {
                self.shell.overlay = None
            }
            Some(UserEvent::NavigateDown | UserEvent::SelectDown) => state.select_next(),
            Some(UserEvent::NavigateUp | UserEvent::SelectUp) => state.select_prev(),
            Some(UserEvent::GoToTop) => state.select_first(),
            Some(UserEvent::GoToBottom) => state.select_last(),
            Some(event) if state.scroll_output(event) => {}
            Some(UserEvent::ShortCopy | UserEvent::FullCopy) => {
                if let Some(text) = state.selected_text() {
                    self.ec.send(AppEvent::copy("Command and output", text));
                }
            }
            _ => {}
        }
    }

    // Whether the keys go to a popup, typed text or the help, which take only their own: then
    // these are in the status bar, else those of the whole app, the others in the keys popup.
    fn keys_are_local(&self) -> bool {
        self.visible_dialog().is_some() || self.shell.overlay.is_some() || self.is_typing()
    }

    // Whether text is being typed or a prompt answered, in the view or the status bar.
    // The wheel moves the selection of what has the focus like j and k, whatever keys they are bound
    // to; over a popup, or while typing, it does nothing.
    fn scroll_wheel(&mut self, down: bool) {
        let event = if down {
            UserEvent::NavigateDown
        } else {
            UserEvent::NavigateUp
        };
        let idle = self.shell.dialog.is_none() && !self.is_typing();
        match self.shell.overlay.as_mut() {
            Some(Overlay::CommandLog(_)) => self.handle_command_log_event(Some(event)),
            Some(Overlay::Help(help)) => help.handle_event(event),
            None if idle => {
                let code = if down { KeyCode::Down } else { KeyCode::Up };
                self.dispatch(event, KeyEvent::new(code, KeyModifiers::NONE));
            }
            None => {}
        }
    }

    fn is_typing(&self) -> bool {
        self.view.is_capturing_keys()
            || matches!(self.shell.app_status.status_line, StatusLine::Input { .. })
    }

    // The keys of the focused panel and what they do, for the keys popup; empty when it is
    // hidden. Computed once a frame, for clearing the screen and drawing.
    fn keys_popup_keys(&self) -> Vec<(String, String)> {
        if !self.shell.keys_popup || self.keys_are_local() {
            return vec![];
        }
        self.shown_keys(self.local_hints())
    }

    // The keys for the status bar (see `keys_are_local`), as many as fit in `width`, the first
    // ones being the most useful.
    fn fit_key_hints(&self, width: usize) -> Line<'static> {
        let keys = self.shown_keys(if self.keys_are_local() {
            self.local_hints()
        } else {
            self.global_hints()
        });
        let keys: Vec<(&str, &str)> = keys
            .iter()
            .map(|(key, description)| (key.as_str(), description.as_str()))
            .collect();
        let theme = &self.ctx.color_theme;
        // each hint and the " • " before it, see `key_hints`
        let mut used = 0;
        let fitting = keys
            .iter()
            .take_while(|hint| {
                let separator = if used == 0 { 0 } else { 3 };
                used += separator + key_hints(std::slice::from_ref(hint), theme).width();
                used <= width
            })
            .count();
        key_hints(&keys[..fitting], theme)
    }

    // What to clear before drawing the frame: the graph images stay on the screen until cleared,
    // so they are when the layout changes. Text views cover the area of the images they replace,
    // and the whole screen is cleared when the graph comes back, or moves; what is drawn over the
    // images is seen to in `draw`.
    fn frame_clear(&mut self) -> PendingClear {
        let layout = FrameLayout {
            view: self.view.kind(),
            sidebar_collapsed: self.shell.sidebar_collapsed(),
            panels: self.ctx.layout.borrow().version(),
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
            Some(last) if last.view == layout.view => PendingClear::None,
            Some(last) if layout.view == ViewKind::List || last.view == ViewKind::LocalChanges => {
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

    fn render(&mut self, f: &mut Frame, keys: &[(String, String)]) {
        let base = Block::default()
            .fg(self.ctx.color_theme.fg)
            .bg(self.ctx.color_theme.bg);
        f.render_widget(base.clone(), f.area());

        let [page_bar_area, app_area, status_line_area] = split_app_areas(f.area());
        self.render_page_bar(f, page_bar_area);
        let [sidebar_area, view_area] = self.split_sidebar(app_area);
        // the panels place their borders to drag as they are drawn
        self.ctx.layout.borrow_mut().clear_borders();
        if self.sidebar_shown() && !self.shell.sidebar_collapsed() {
            self.ctx
                .layout
                .borrow_mut()
                .place(Panel::Sidebar, sidebar_area, Edge::Right);
        }

        self.update_state(view_area);

        // views are replaced while switching pages, they take the focus as they are drawn
        if self.sidebar_shown() {
            let sidebar = RefList::new(self.ctx.clone(), self.shell.sidebar_mode);
            f.render_stateful_widget(sidebar, sidebar_area, &mut self.shell.sidebar);
        }
        self.view
            .render(f, view_area, !self.shell.sidebar_focused());
        if let Some(Overlay::Help(help)) = self.shell.overlay.as_mut() {
            f.render_widget(Clear, app_area);
            f.render_widget(base, app_area);
            help.render(f, app_area);
        }
        if !keys.is_empty() {
            let popup = KeysPopup::new(keys, &self.ctx.color_theme);
            f.render_widget(popup, app_area);
        }
        if let Some(dialog) = self.visible_dialog() {
            f.render_widget(GitDialogWidget::new(dialog, &self.ctx), app_area);
        }
        if let Some(Overlay::CommandLog(state)) = self.shell.overlay.as_mut() {
            let scrolloff = self.ctx.ui_config.common.scrolloff as usize;
            CommandLog::new(&self.ctx.color_theme, scrolloff).render(
                app_area,
                f.buffer_mut(),
                state,
            );
        }
        let dismiss = self.ctx.short_keys(&[UserEvent::DismissNotifications]);
        let theme = &self.ctx.color_theme;
        render_notices(
            &self.shell.notices,
            &dismiss,
            app_area,
            f.buffer_mut(),
            theme,
        );
        self.render_status_line(f, status_line_area);
        // the graph images stop where a popup covers them
        self.shell.app_status.images = crate::protocol::crop_covered_images(f.buffer_mut());
    }
}

impl App<'_> {
    // The pages at the top, the one shown as a pill; the local changes with their count.
    fn render_page_bar(&self, f: &mut Frame, area: Rect) {
        let theme = &self.ctx.color_theme;
        let count = match &self.view {
            View::LocalChanges(view) => view.local_change_count(),
            _ => self.repository.loaded_local_change_count(),
        };
        let on_local_changes = !self.sidebar_shown();
        let pages = [
            (
                format!("Local Changes ({count})"),
                on_local_changes,
                theme.status_success_fg,
            ),
            (
                "Commit Graph".to_string(),
                !on_local_changes,
                theme.accent_fg,
            ),
        ];
        let mut spans = vec![Span::raw(" ")];
        for (name, shown, color) in pages {
            if shown {
                spans.extend([
                    Span::raw("\u{e0b6}").fg(color),
                    Span::raw(name).fg(theme.accent_text_fg).bg(color).bold(),
                    Span::raw("\u{e0b4}").fg(color),
                ]);
            } else {
                spans.push(Span::raw(format!(" {name} ")).fg(theme.muted_fg));
            }
            spans.push(Span::raw("  "));
        }
        f.render_widget(Line::from(spans), area);
    }

    // A bar after a neovim statusline: the message, or what runs in the background, or the
    // repository; on the right the keys of what has the focus and the position in the commit list.
    // The page is in the page bar.
    fn render_status_line(&self, f: &mut Frame, area: Rect) {
        let theme = &self.ctx.color_theme;
        let bar = Style::default()
            .fg(theme.status_bar_fg)
            .bg(theme.status_bar_bg);
        let mut left = vec![Span::raw(" ")];
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
                    Some(running) => left.push(
                        Span::raw(format!("{} {}", running.task.running, spinner()))
                            .fg(theme.highlight_fg),
                    ),
                    None => {
                        // shortened like neovim's pathshorten when it would crowd the keys
                        let mut path = home_relative(self.repository.path());
                        if path.width() > area.width as usize / 4 {
                            path = shorten_path(&path);
                        }
                        left.push(Span::raw(format!("󰉋 {path}")).fg(theme.status_bar_fg));
                        // the checked-out branch, as the sidebar is on the graph only
                        let head = match self.repository.head() {
                            Head::Branch { name } => Some(name.clone()),
                            Head::Detached { target } => {
                                Some(format!("detached at {}", target.as_short_hash()))
                            }
                            Head::None => None,
                        };
                        if let Some(head) = head {
                            left.push(
                                Span::raw(format!("   \u{e725} {head}")).fg(theme.list_ref_head_fg),
                            );
                        }
                    }
                }
            }
            StatusLine::Input {
                message, metadata, ..
            } => {
                left.push(Span::raw(message.clone()).fg(theme.status_bar_fg));
                right.push(Span::raw(format!("{metadata} ")).fg(theme.muted_fg));
            }
            StatusLine::Transient(msg) => {
                right.push(Span::raw(format!("{msg} ")).fg(theme.muted_fg));
            }
            StatusLine::SearchResult {
                message,
                options,
                matched,
            } => {
                let span = Span::raw(message.clone());
                left.push(if *matched {
                    span.fg(theme.status_info_fg)
                } else {
                    span.fg(theme.status_warn_fg).bold()
                });
                right.push(Span::raw(format!("{options} ")).fg(theme.muted_fg));
            }
            StatusLine::NotificationInfo(msg) => {
                left.push(Span::raw(msg.clone()).fg(theme.status_info_fg));
            }
            StatusLine::NotificationSuccess(msg) => {
                left.push(
                    Span::raw(format!("✓ {msg}"))
                        .fg(theme.status_success_fg)
                        .bold(),
                );
            }
        }

        // with a message, what runs in the background goes to the right
        let message = !matches!(self.shell.app_status.status_line, StatusLine::None);
        if let Some(running) = self.ctx.running_task.borrow().as_ref().filter(|_| message) {
            let text = format!("{} {} ", running.task.running, spinner());
            right.insert(0, Span::raw(text).fg(theme.highlight_fg));
        }
        // the selected commit and how many there are, like the line and column
        if let Some(state) = self.view.as_list_state() {
            let (selected, total) = state.position();
            right.push(
                Span::raw(format!("[{selected}/{total}] "))
                    .fg(theme.muted_fg)
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
    // The sidebar is on the graph only, beside the commits or their details.
    fn sidebar_shown(&self) -> bool {
        !matches!(self.view, View::LocalChanges(_))
    }

    // The sidebar on the left and the page.
    fn split_sidebar(&self, area: Rect) -> [Rect; 2] {
        let width = if !self.sidebar_shown() {
            0
        } else if self.shell.sidebar_collapsed() {
            COLLAPSED_SIDEBAR_WIDTH.min(area.width)
        } else {
            // leave the page some room on narrow terminals, unless resized
            let default = self.ctx.ui_config.refs.width.min(area.width / 3);
            let width = self.ctx.layout.borrow().size(Panel::Sidebar, default);
            fit_size(width, area.width, 10, 20)
        };
        Layout::horizontal([Constraint::Length(width), Constraint::Min(0)]).areas(area)
    }
}

// " [0] " and the border on its right
const COLLAPSED_SIDEBAR_WIDTH: u16 = 7;

// The page bar, the app, and the status bar.
fn split_app_areas(area: Rect) -> [Rect; 3] {
    Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area)
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
        if !matches!(self.view, View::List(_)) {
            return;
        }
        let Some(commit_list_state) = self.take_list_state() else {
            return;
        };
        let (commit, changes, refs) = selected_commit_details(self.repository, &commit_list_state);
        self.replace_view(View::of_detail(
            commit_list_state,
            commit,
            changes,
            refs,
            self.repository,
            self.ctx.clone(),
            self.ec.sender(),
        ));
        if let View::Detail(ref mut view) = self.view {
            view.show_tab(tab);
        }
    }

    // Runs `run` with the terminal given back to it, like an editor.
    fn suspended<T>(&self, run: impl FnOnce() -> T) -> T {
        self.ec.suspend();
        let result = run();
        self.ec.resume();
        result
    }

    fn run_external(&mut self, command: &[String]) {
        let result = self.suspended(|| exec_interactive(command));
        self.shell.app_status.pending_clear = PendingClear::Screen;
        // the command may have changed files, or resolved conflicts
        self.view.refresh();
        if let Err(err) = result {
            self.ec.send(AppEvent::NotifyError(err));
        }
    }

    // Handles what works on every page (the sidebar, switching pages, fetch, pull and push),
    // else hands the event to the focused sidebar or the page.
    fn dispatch(&mut self, event: UserEvent, key: KeyEvent) {
        if event == UserEvent::ForceQuit {
            self.ec.send(AppEvent::Quit);
            return;
        }
        // keys typed into the search of the list belong to the view
        let typing = matches!(self.shell.app_status.status_line, StatusLine::Input { .. });
        if typing {
            self.view.handle_event(event, key);
            return;
        }
        if self.handle_remote_event(event) {
            return;
        }
        // on the sidebar, they work on its selected ref, see `handle_sidebar_event`
        if event.home_scope() == Scope::Graph && !self.shell.sidebar_focused() {
            self.handle_graph_event(event);
            return;
        }
        match event {
            UserEvent::ResizeLeft => return self.move_border(Direction::Left),
            UserEvent::ResizeRight => return self.move_border(Direction::Right),
            UserEvent::ResizeUp => return self.move_border(Direction::Up),
            UserEvent::ResizeDown => return self.move_border(Direction::Down),
            UserEvent::FocusLeft => return self.move_focus(Direction::Left),
            UserEvent::FocusRight => return self.move_focus(Direction::Right),
            UserEvent::FocusUp => return self.move_focus(Direction::Up),
            UserEvent::FocusDown => return self.move_focus(Direction::Down),
            UserEvent::KeysToggle => {
                self.shell.keys_popup = !self.shell.keys_popup;
                return;
            }
            UserEvent::CommandLog => {
                self.shell.overlay = Some(Overlay::CommandLog(CommandLogState::new()));
                return;
            }
            UserEvent::DismissNotifications => {
                self.shell.notices.clear();
                return;
            }
            UserEvent::ToggleWrap => {
                self.ctx.wrap.set(!self.ctx.wrap.get());
                return;
            }
            UserEvent::FocusPanel(0) => {
                // again on the focused sidebar, it shrinks out of the way; on a shrunk one, it grows back
                if self.shell.sidebar_mode == SidebarMode::Focused {
                    self.shell.sidebar_mode = SidebarMode::Collapsed;
                } else {
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
        if self.shell.sidebar_focused() {
            self.handle_sidebar_event(event, key);
        } else {
            self.view.handle_event(event, key);
        }
    }

    fn handle_sidebar_event(&mut self, event: UserEvent, key: KeyEvent) {
        // space applies a stash, and checks out the other refs, as it does on the graph
        let stash = matches!(
            self.shell.sidebar.selection(),
            RefSelection::Ref(RefSection::Stashes, _)
        );
        let event = match event {
            UserEvent::ApplyStash if !stash => UserEvent::Checkout,
            event => event,
        };
        if !self.sidebar_command_applies(event) {
            return;
        }
        match event {
            UserEvent::Quit => self.ec.send(AppEvent::Quit),
            UserEvent::HelpToggle => self.ec.send(AppEvent::OpenHelp),
            UserEvent::Refresh => self.view.refresh(),
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                self.shell.sidebar.select_next();
                self.show_sidebar_selection();
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                self.shell.sidebar.select_prev();
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
            UserEvent::HalfPageDown | UserEvent::HalfPageUp => {
                self.shell.sidebar.jump_half_page(event);
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
            UserEvent::NextSection | UserEvent::PreviousSection => {
                self.shell
                    .sidebar
                    .select_section(event == UserEvent::NextSection);
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
            UserEvent::OpenInBrowser | UserEvent::CopyLink => {
                let target = match self.shell.sidebar.selection() {
                    RefSelection::Ref(RefSection::Branches, name) => LinkTarget::Branch(name),
                    RefSelection::Ref(RefSection::Remotes, name) => LinkTarget::RemoteBranch(name),
                    RefSelection::Ref(RefSection::Tags, name) => LinkTarget::Tag(name),
                    RefSelection::Folder(RefSection::Remotes, name) => LinkTarget::Remote(name),
                    _ => return,
                };
                let copy = event == UserEvent::CopyLink;
                self.ec.send(AppEvent::Link { target, copy });
            }
            event @ (UserEvent::CreateRef
            | UserEvent::DeleteRef
            | UserEvent::EditRef
            | UserEvent::ApplyStash) => {
                let selection = self.shell.sidebar.selection();
                let picked = self.shell.sidebar.picked();
                if let Some(dialog) = ref_command(self.repository, event, &selection, picked) {
                    self.open_dialog(dialog);
                }
            }
            UserEvent::FocusPanel(_) => {
                // a panel of the page
                self.set_sidebar_focused(false);
                self.view.handle_event(event, key);
            }
            event if event.home_scope() == Scope::Graph => self.handle_sidebar_graph_event(event),
            _ => {}
        }
    }

    // The scope of the keys, where the focus is, see `Scope`.
    fn current_scope(&self) -> Scope {
        if self.shell.sidebar_focused() {
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
        let selection = self.shell.sidebar.selection();
        let stashes =
            picked.section == Stashes || matches!(selection, RefSelection::Ref(Stashes, _));
        let edit = if let RefSelection::Folder(..) = selection {
            "edit remote"
        } else {
            "rename/upstream"
        };
        let mut hints = vec![];
        if !picked.is_empty() {
            hints.push(hint(UserEvent::Cancel, "clear picks"));
        }
        let commands = [
            hint(UserEvent::CreateRef, "new remote"),
            hint(UserEvent::EditRef, edit),
            hint(
                UserEvent::DeleteRef,
                if stashes { "drop" } else { "delete" },
            ),
            hint(UserEvent::ApplyStash, "apply"),
            hint(UserEvent::CreateBranch, "new branch"),
            hint(UserEvent::CreateTag, "new tag"),
            hint(UserEvent::Checkout, "checkout"),
            hint(UserEvent::Merge, "merge"),
            hint(UserEvent::Rebase, "rebase"),
            hint(UserEvent::Diff, "diff"),
            hint(UserEvent::ToggleSelect, "pick"),
            hint(UserEvent::ShortCopy, "copy name"),
            hint(UserEvent::OpenInBrowser, "open in browser"),
        ];
        hints.extend(
            commands
                .into_iter()
                .filter(|(events, _)| self.sidebar_command_applies(events[0])),
        );
        if picked.is_empty() {
            hints.extend([
                hint(UserEvent::Confirm, "open"),
                hint(UserEvent::Cancel, "back"),
            ]);
        }
        hints
    }

    // Whether the event does something on the selected row of the sidebar, see
    // `ref_command_applies`.
    fn sidebar_command_applies(&self, event: UserEvent) -> bool {
        let selection = self.shell.sidebar.selection();
        let picked = self.shell.sidebar.picked();
        ref_command_applies(self.repository, event, &selection, picked)
    }

    // Focuses the sidebar, growing it back when shrunk, or gives the focus back to the page.
    fn set_sidebar_focused(&mut self, focused: bool) {
        if focused && !self.sidebar_shown() {
            return;
        }
        if focused {
            self.shell.sidebar_mode = SidebarMode::Focused;
            self.view.blur();
        } else if self.shell.sidebar_mode == SidebarMode::Focused {
            self.shell.sidebar_mode = SidebarMode::Shown;
        }
    }

    // Shows the selected ref on the graph; the commit details stay open, showing its commit.
    fn show_sidebar_selection(&mut self) {
        let Some(name) = self.shell.sidebar.selected_ref_name() else {
            return;
        };
        match &mut self.view {
            View::List(view) => view.select_ref(&name),
            View::Detail(view) => view.select_ref(&name),
            _ => {}
        }
    }

    // Tab: between the local changes page and the graph, focusing the page.
    fn switch_page(&mut self) {
        if matches!(self.view, View::LocalChanges(_)) {
            self.show_graph_page();
        } else {
            self.show_local_changes_page();
        }
        self.set_sidebar_focused(false);
    }

    // Shows the commit list alone, from a view showing it; returns whether it did.
    fn show_list(&mut self) -> bool {
        let Some(commit_list_state) = self.take_list_state() else {
            return false;
        };
        self.replace_view(View::of_list(
            commit_list_state,
            self.ctx.clone(),
            self.ec.sender(),
        ));
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
        let Some(commit_list_state) = self.take_list_state() else {
            return;
        };
        // as it was when left, or before a refresh
        let snapshot = self.shell.parked_local_changes.take();
        self.replace_view(View::of_local_changes(
            commit_list_state,
            LocalChangesState::restore(self.repository, snapshot),
            self.ctx.clone(),
            self.ec.sender(),
        ));
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
        self.ec.send(AppEvent::copy(name, value));
    }

    // Opens the fetch, pull or push popup, or fetches all remotes, for the event bound to them.
    // Returns whether the event was one of them.
    fn handle_remote_event(&mut self, event: UserEvent) -> bool {
        let action = match event {
            UserEvent::Fetch => RemoteAction::Fetch,
            UserEvent::Pull => RemoteAction::Pull,
            UserEvent::Push => RemoteAction::Push,
            UserEvent::FetchAll => {
                self.start_task(fetch_all_task());
                return true;
            }
            _ => return false,
        };
        self.open_dialog(GitDialog::remote(action, self.repository.remote_info()));
        // pressing the key again quickly runs the command without showing the popup
        if let Some(popup) = self.shell.dialog.as_mut() {
            popup.reveal = Some(Instant::now() + QUICK_RUN_DELAY);
        }
        true
    }

    fn open_dialog(&mut self, dialog: Result<GitDialog, String>) {
        if let Some(message) = self.ctx.busy_message() {
            self.ec.send(AppEvent::NotifyWarn(message));
            return;
        }
        match dialog {
            // the popup would only lead to refusing its command
            Ok(dialog)
                if dialog
                    .command()
                    .is_ok_and(|task| self.blocked_by_operation(&task)) => {}
            Ok(dialog) => {
                self.shell.dialog = Some(Popup {
                    dialog,
                    reveal: None,
                });
            }
            Err(err) => self.ec.send(AppEvent::NotifyError(err)),
        }
    }

    fn handle_dialog_key(&mut self, key: KeyEvent) {
        if self.force_quit(&key) {
            return;
        }
        let Some(Popup { dialog, .. }) = self.shell.dialog.as_mut() else {
            return;
        };
        // the key that opened the popup runs it, in the part of the app it belongs to
        let event = match dialog.trigger() {
            Some(trigger)
                if self.ctx.keybind.event_in(trigger.home_scope(), &key) == Some(trigger) =>
            {
                Some(trigger)
            }
            _ => self.ctx.keybind.event_in(Scope::Dialog, &key),
        };
        match dialog.handle_key(event, key) {
            DialogOutcome::None => {}
            DialogOutcome::Close => self.close_dialog(),
            DialogOutcome::Quit => self.ec.send(AppEvent::Quit),
            DialogOutcome::Error(err) => self.ec.send(AppEvent::NotifyError(err)),
            DialogOutcome::Send(event) => {
                self.close_dialog();
                self.ec.send(*event);
                return;
            }
        }
        // any other key shows the popup it acts on
        if let Some(popup) = self.shell.dialog.as_mut() {
            popup.reveal = None;
        }
    }

    // Quits for the force-quit key, which works even where the keys are typed, like in a
    // popup or the commit message; returns whether it was that key.
    fn force_quit(&self, key: &KeyEvent) -> bool {
        let quit = self.ctx.keybind.event_in(Scope::Global, key) == Some(UserEvent::ForceQuit);
        if quit {
            self.ec.send(AppEvent::Quit);
        }
        quit
    }

    // What waited until `now`: the popup waiting for a second press shows, the notices go.
    fn expire(&mut self, now: Instant) {
        if let Some(popup) = self.shell.dialog.as_mut() {
            popup.reveal = popup.reveal.filter(|at| *at > now);
        }
        self.shell.notices.retain(|n| n.until > now);
    }

    fn close_dialog(&mut self) {
        self.shell.dialog = None;
    }

    // The popup, once it shows.
    fn visible_dialog(&self) -> Option<&GitDialog> {
        self.shell
            .dialog
            .as_ref()
            .filter(|popup| popup.reveal.is_none())
            .map(|popup| &popup.dialog)
    }

    // Fetches all remotes in the background like lazygit, without taking the place of the git
    // command running, so the commands of the user are not held back by it. Nothing is said when
    // it fails, offline say, but in the command log; the remote branches it moved show once the
    // refs are next checked, see `check_refs`.
    fn fetch_on_launch(&self) {
        let path = self.repository.path().to_path_buf();
        let task = GitTask {
            timeout: Some(LAUNCH_FETCH_TIMEOUT),
            ..fetch_all_task()
        };
        std::thread::spawn(move || run_task(&path, &task));
    }

    // Runs the git command in the background; the app is told when it finishes.
    fn start_task(&mut self, task: GitTask) {
        if let Some(message) = self.ctx.busy_message() {
            self.ec.send(AppEvent::NotifyWarn(message));
            return;
        }
        if self.blocked_by_operation(&task) {
            return;
        }
        // a commit made clears the message box, see `finish_task`
        let head_before_commit = task
            .commits()
            .then(|| self.repository.head_commit().unwrap_or_default());
        *self.ctx.running_task.borrow_mut() = Some(RunningTask {
            task: task.clone(),
            head_before_commit,
            quit_after: false,
        });
        self.repository.set_busy(Some(task.running.clone()));
        let path = self.repository.path().to_path_buf();
        // signing may ask for a passphrase in the terminal, which the app gives back meanwhile
        if task.may_sign() && self.repository.signs() {
            self.ec.suspend();
            println!("{}…", task.running);
            let result = run_task(&path, &task);
            self.ec.resume();
            self.shell.app_status.pending_clear = PendingClear::Screen;
            self.ec.send(AppEvent::TaskFinished { task, result });
            return;
        }
        let tx = self.ec.sender();
        std::thread::spawn(move || {
            // the app redraws the spinner on each tick, until `done` is dropped
            let (done, ticking) = std::sync::mpsc::channel::<()>();
            let ticker = tx.clone();
            std::thread::spawn(move || {
                while ticking.recv_timeout(SPINNER_RATE) == Err(RecvTimeoutError::Timeout) {
                    ticker.send(AppEvent::SpinnerTick);
                }
            });
            let result = run_task(&path, &task);
            drop(done);
            tx.send(AppEvent::TaskFinished { task, result });
        });
    }

    // Shows what the git command did; returns whether quitting waited for it.
    fn finish_task(&mut self, task: GitTask, mut result: Result<(), String>) -> bool {
        let running = self.ctx.running_task.borrow_mut().take();
        let quit = running.as_ref().is_some_and(|running| running.quit_after);
        self.repository.set_busy(None);
        if let (Err(_), Some(failed)) = (&result, &task.failed) {
            result = Err(failed.clone());
        }
        // the message is committed, even when going on with a rebase stopped afterwards
        let committed = running
            .and_then(|running| running.head_before_commit)
            .is_some_and(|before| self.repository.head_commit().unwrap_or_default() != before);
        if committed {
            match &mut self.view {
                View::LocalChanges(view) => view.clear_draft(),
                _ => {
                    if let Some(snapshot) = self.shell.parked_local_changes.as_mut() {
                        *snapshot.commit_draft = Default::default();
                    }
                }
            }
        }
        // only the working tree changed, the page stays as it is
        if task.after == AfterTask::KeepView {
            self.ec.send(AppEvent::LocalChangesChanged);
            match result {
                Ok(_) => self.ec.send(AppEvent::NotifySuccess(task.done)),
                Err(err) => self.ec.send(AppEvent::NotifyError(err)),
            }
            return quit;
        }
        let typing = self.is_typing();
        if result.is_ok() && task.uses_picks {
            self.view.clear_picked_commits();
        }
        // what is left to resolve or commit is on the local changes page
        let to_local_changes = (result.is_ok() && task.after == AfterTask::LocalChanges)
            || self.repository.operation_in_progress().is_some();
        // the command may have changed files or stopped on conflicts, even when it failed;
        // refreshing would interrupt typing, so then it waits for the refs to be checked once
        // typing stopped, see `check_refs`
        if let Some(mut context) = self.view.refresh_context().filter(|_| !typing) {
            // the rewritten commit stays selected
            let select = match &task.after {
                AfterTask::Select(revision) if result.is_ok() => Some(revision.as_str()),
                _ => None,
            };
            if let Some(hash) = select.and_then(|revision| self.repository.resolve(revision)) {
                let list_context = context.list_context_mut();
                list_context.commit_hash = hash.as_str().to_string();
                list_context.scroll_to_top = false;
            }
            if to_local_changes && !matches!(self.view, View::LocalChanges(_)) {
                // the details open again when coming back to the graph
                if let RefreshViewContext::Detail {
                    tab, compare: None, ..
                } = context
                {
                    self.shell.parked_detail_tab = Some(tab);
                }
                // the page opens once the repository is reloaded
                let snapshot = self.shell.parked_local_changes.take().unwrap_or_default();
                context = RefreshViewContext::LocalChanges {
                    list_context: context.list_context().clone(),
                    local_changes_context: snapshot,
                };
                self.set_sidebar_focused(false);
            }
            self.ec.send(AppEvent::Refresh(context));
        }
        match result {
            Ok(_) => self.ec.send(AppEvent::NotifySuccess(task.done)),
            Err(err) => self.ec.send(AppEvent::NotifyError(err)),
        }
        quit
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
        if self.refused_while_diffing(event) {
            return;
        }
        if event == UserEvent::BranchOnly {
            // the commits are loaded again, of the current branch or of all
            self.shell.graph_filter = match self.shell.graph_filter {
                GraphFilter::BranchOnly(_) => GraphFilter::All,
                _ => GraphFilter::BranchOnly(None),
            };
            self.view.refresh();
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
        if self.refused_while_diffing(event) {
            return;
        }
        if event == UserEvent::BranchOnly {
            // the selected branch's history, or the current one's off a branch
            let branch = match self.shell.sidebar.selection() {
                RefSelection::Ref(RefSection::Branches, name) if name != DETACHED_HEAD => {
                    Some(name)
                }
                _ => None,
            };
            self.shell.graph_filter = match &self.shell.graph_filter {
                GraphFilter::BranchOnly(shown) if *shown == branch => GraphFilter::All,
                _ => GraphFilter::BranchOnly(branch),
            };
            self.view.refresh();
            return;
        }
        if event == UserEvent::Diff {
            match self.sidebar_compare() {
                Ok(compare) => self.request_ref_compare(compare),
                Err(err) => self.ec.send(AppEvent::NotifyError(err)),
            }
            return;
        }
        // a branch, remote branch or tag, see `ref_command_applies`, of the row's section, as a
        // tag may have a branch's name
        let RefSelection::Ref(section, name) = self.shell.sidebar.selection() else {
            return;
        };
        let selected = self.repository.all_refs().into_iter().find(|r| {
            r.name() == name
                && matches!(
                    (r, section),
                    (Ref::Branch { .. }, RefSection::Branches)
                        | (Ref::RemoteBranch { .. }, RefSection::Remotes)
                        | (Ref::Tag { .. }, RefSection::Tags)
                )
        });
        let command = match (selected, self.repository.head()) {
            (Some(selected), _) => ref_graph_command(self.repository, event, selected),
            // the row of a detached HEAD, its commit
            (None, Head::Detached { target }) if name == DETACHED_HEAD => {
                graph_command(self.repository, event, target.clone(), vec![])
            }
            _ => return,
        };
        self.run_graph_command(event, command);
    }

    // Moves the border beside the focused panel with ctrl-left and ctrl-right (the sidebar, or the
    // files beside a diff), the one above or under it with ctrl-up and ctrl-down.
    fn move_border(&mut self, direction: Direction) {
        let (vertical, step) = match direction {
            Direction::Left => (true, -2),
            Direction::Right => (true, 2),
            Direction::Up => (false, -1),
            Direction::Down => (false, 1),
        };
        let sidebar = (!self.shell.sidebar_collapsed()).then_some(Panel::Sidebar);
        let panel = match &self.view {
            _ if self.shell.sidebar_focused() => sidebar.filter(|_| vertical),
            View::Detail(view) if vertical && view.shows_tree() => Some(Panel::DetailTree),
            View::Detail(_) if !vertical => Some(Panel::Details),
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
    fn move_focus(&mut self, direction: Direction) {
        if self.shell.sidebar_focused() {
            if direction == Direction::Right {
                self.set_sidebar_focused(false);
            }
        } else if !self.view.move_focus(direction) && direction == Direction::Left {
            self.set_sidebar_focused(true);
        }
    }

    // Whether a diff of two commits or refs is open, also under the details of one of its commits.
    fn is_diffing(&self) -> bool {
        matches!(self.shell.graph_filter, GraphFilter::Compare { .. })
            || matches!(&self.view, View::Detail(view) if view.is_comparing())
    }

    // Whether `event` would open another diff, or change the commits shown, while a diff is
    // open; tells so when it does.
    fn refused_while_diffing(&self, event: UserEvent) -> bool {
        let refused = matches!(event, UserEvent::Diff | UserEvent::BranchOnly) && self.is_diffing();
        if refused {
            let msg = "A diff is open, close it first (esc)".to_string();
            self.ec.send(AppEvent::NotifyWarn(msg));
        }
        refused
    }

    // Whether the task moves or rewrites HEAD while a rebase, merge, cherry-pick or revert waits to
    // be continued or aborted, which git refuses or tangles up; tells so when it does.
    fn blocked_by_operation(&self, task: &GitTask) -> bool {
        let Some(operation) = task
            .moves_head()
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

    // Diffs refs from the sidebar: the graph is loaded again with only their history, showing
    // their comparison, see `end_stale_ref_compare`.
    fn request_ref_compare(&mut self, compare: RefCompare) {
        let Some(context) = self.view.refresh_context() else {
            return;
        };
        // the local changes page is kept as it was, for coming back to it
        if let View::LocalChanges(view) = &self.view {
            self.shell.parked_local_changes = Some(view.context());
        }
        // the history shown before, going back to it once the comparison closes
        let before = match std::mem::take(&mut self.shell.graph_filter) {
            GraphFilter::Compare { before, .. } => before,
            filter => Box::new(filter),
        };
        // reloaded for the history of both, opening their comparison like after any reload
        let shown = Compare {
            base: compare.base.clone(),
            target: compare.target.clone(),
            title: format!("Diff {} → {}", compare.base, compare.target),
        };
        self.shell.graph_filter = GraphFilter::Compare {
            refs: compare,
            before,
        };
        self.shell.sidebar.clear_picks();
        self.set_sidebar_focused(false);
        self.ec.send(AppEvent::Refresh(RefreshViewContext::Detail {
            list_context: context.list_context().clone(),
            tab: DetailTab::Changes,
            compare: Some(shown),
        }));
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
            })) => self.open_compare(Compare {
                base: base.as_str().into(),
                target: target.as_str().into(),
                title,
            }),
            // the popups tell first that a command is running
            Some(Err(err)) if event != UserEvent::Diff => self.open_dialog(Err(err)),
            Some(Err(err)) => self.ec.send(AppEvent::NotifyError(err)),
        }
    }

    // Whether reloading in the background would get in the way, like lazygit pausing its refreshes:
    // while a git command runs, a popup is open or something is being typed or confirmed.
    fn refresh_paused(&self) -> bool {
        self.ctx.busy_message().is_some() || self.shell.dialog.is_some() || self.is_typing()
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
    // count in the page bar.
    fn reload_files(&mut self, files: FilesSnapshot) {
        self.shell.operation_shown = self.repository.operation_in_progress().is_some();
        match &mut self.view {
            View::LocalChanges(view) => view.reload_files(files),
            _ => {
                self.repository.set_local_changes(files.changes);
            }
        }
    }

    // Shows the local changes page with the last commit being amended.
    fn amend_on_local_changes(&mut self) {
        self.show_local_changes_page();
        self.set_sidebar_focused(false);
        if let View::LocalChanges(view) = &mut self.view {
            if let Err(err) = view.start_amend() {
                self.ec.send(AppEvent::NotifyError(err));
            }
        }
    }

    // Shows the changes from the base to the target of `compare` in the details.
    fn open_compare(&mut self, compare: Compare) {
        if !matches!(self.view, View::List(_) | View::Detail(_)) {
            return;
        }
        let resolved = self
            .repository
            .resolve(&compare.base)
            .zip(self.repository.resolve(&compare.target));
        let Some((base, target)) = resolved else {
            // gone meanwhile, like a ref deleted
            self.ec.send(AppEvent::NotifyError(format!(
                "Cannot diff {} and {}, one of them is gone",
                compare.base, compare.target
            )));
            return;
        };
        // the details show the target, else (left out by --max-count) the selected commit
        let shown = match self.repository.commit(&target) {
            Some(_) => target.clone(),
            None => match self.view.as_list_state() {
                Some(state) => state.selected_commit_hash().clone(),
                None => return,
            },
        };
        let Some(commit_list_state) = self.take_list_state() else {
            return;
        };
        let (commit, changes) = self.repository.commit_detail(&shown);
        let refs = self.repository.refs(&shown).into_iter().cloned().collect();
        let mut view = View::of_detail(
            commit_list_state,
            commit,
            changes,
            refs,
            self.repository,
            self.ctx.clone(),
            self.ec.sender(),
        );
        // comparing before it is shown, see `end_stale_ref_compare`
        if let View::Detail(ref mut view) = view {
            view.start_compare(base, target, compare);
        }
        self.replace_view(view);
    }

    fn select_older_commit(&mut self) {
        if let View::Detail(ref mut view) = self.view {
            view.select_older_commit();
        }
    }

    fn select_newer_commit(&mut self) {
        if let View::Detail(ref mut view) = self.view {
            view.select_newer_commit();
        }
    }

    fn init_with_context(&mut self, context: RefreshViewContext) {
        if let View::List(ref mut view) = self.view {
            view.reset_commit_list_with(context.list_context());
        }
        match context {
            RefreshViewContext::List { .. } => {}
            RefreshViewContext::Detail {
                compare: Some(compare),
                ..
            } => self.open_compare(compare),
            RefreshViewContext::Detail { tab, .. } => self.open_detail(tab),
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

    fn warn_notification(&mut self, message: String) {
        push_notice(&mut self.shell.notices, Notice::warning(message));
    }

    // Asks to open the page of `target` in the browser, choosing between a branch's page and a pull
    // request from it; copies its link right away, but a branch's which.
    fn open_link(&mut self, target: &LinkTarget, copy: bool) {
        let services = &self.ctx.core_config.browser.services;
        match browse::links(self.repository, services, target) {
            Ok(mut links) if copy && links.len() == 1 => self.open_url(links.remove(0).1, true),
            Ok(links) => self.open_dialog(Ok(GitDialog::open_link(links, copy))),
            Err(err) => self.ec.send(AppEvent::NotifyError(err)),
        }
    }

    // Opens `url` in the browser, or copies it.
    fn open_url(&self, url: String, copy: bool) {
        let event = if copy {
            AppEvent::copy("Link", url)
        } else {
            match open::that_detached(&url) {
                Ok(()) => AppEvent::NotifySuccess(format!("Opened {url}")),
                Err(err) => AppEvent::NotifyError(format!("Failed to open {url}: {err}")),
            }
        };
        self.ec.send(event);
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

// Whether the task commits the message of the local changes page.
// Whether the command of `event` moves or rewrites HEAD, for its key not to be shown while an
// operation is in progress; the commands themselves decide, see `GitTask::moves_head`.
fn moves_head(event: UserEvent) -> bool {
    // the git command each runs: squashing, dropping and amending an older commit rebase
    let command = match event {
        UserEvent::Rebase | UserEvent::Squash | UserEvent::DropCommit | UserEvent::AmendCommit => {
            "rebase"
        }
        UserEvent::Merge => "merge",
        UserEvent::Checkout => "checkout",
        UserEvent::CherryPick => "cherry-pick",
        UserEvent::Revert => "revert",
        UserEvent::Reset => "reset",
        UserEvent::Pull => "pull",
        _ => return false,
    };
    moves_head_command(command)
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

fn fetch_all_task() -> GitTask {
    GitTask {
        remote: true,
        ..GitTask::new(
            &["fetch", "--all"],
            "Fetching all remotes".into(),
            "Fetched all remotes".into(),
        )
    }
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

#[cfg(test)]
#[path = "tests/app.rs"]
mod app_tests;

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
