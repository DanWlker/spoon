use std::{
    cell::{Cell, RefCell},
    io::{self, Write},
    rc::Rc,
    sync::{mpsc::RecvTimeoutError, Arc},
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

use crate::widget::tree_query::Toward;
use crate::{
    background::{shows_loading, Job},
    browse::{self, LinkAction, LinkTarget},
    color::{ColorTheme, GraphColorSet},
    config::{CoreConfig, CursorType, GraphColorConfig, UiConfig},
    event::{AppEvent, EventController, KeyHint, Sender, UserEvent},
    external::{copy_to_clipboard, exec_interactive},
    git::{
        read_head_commit, read_operation, refs_snapshot, run_task, AfterTask, Commit, CommitHash,
        FileBlameResult, FileChange, FileHistoryResult, FilesSnapshot, GitCommand, GitTask, Head,
        LoadOptions, Loaded, NamedRef, Ref, Repository, Rev, RunningTask, SharedTask,
    },
    graph::{
        CellWidthType, Fold, Graph, GraphImageManager, GraphImageWidthMode, GraphImages,
        GraphLayout, GraphStyle,
    },
    graph_commands::{graph_command, ref_graph_command, GraphCommand},
    keybind::{keys_to_string, KeyBind, Scope, SequenceMatch},
    keys::{Availability, KeyList, Keys},
    layout::{fit_size, Direction, Edge, Panel, PanelLayout, Size},
    protocol::ImageProtocol,
    ref_commands::{diff_refs, ref_command, ref_command_applies},
    search::{SearchOptions, SearchOutcome},
    view::{
        register_graph_commands, BlameView, Compare, DetailFocus, HelpView,
        LocalChangesRefreshViewContext, RefreshViewContext, Reselect, TimelineView, View, ViewKind,
    },
    widget::{
        command_log::{CommandLog, CommandLogState},
        commit_list::{CommitInfo, CommitListState},
        commit_tabs::DetailTab,
        git_dialog::{DialogOutcome, GitDialog, GitDialogWidget, RemoteAction},
        keys_popup::KeysPopup,
        local_changes::LocalChangesState,
        notices::{push_notice, render_notices, Notice},
        ref_list::{RefList, RefListState, RefSection, RefSelection, SidebarMode},
        style::{badge, key_hints, spinner, KeyLabel, SPINNER_RATE},
    },
    InitialSelection,
};

#[derive(Debug, Default)]
enum StatusLine {
    #[default]
    None,
    Input {
        message: String,
        cursor_byte_index: usize,
        metadata: String,
    },
    Transient(String),
    SearchResult {
        outcome: SearchOutcome,
        options: String,
    },
    NotificationSuccess(String),
}

// How the app starts, from the command line: on the local changes page; on which commit; and
// fetching the remotes, which the tests leave out, see `fetch_on_launch`.
#[derive(Clone, Copy)]
pub struct Launch {
    pub page: StartPage,
    pub selection: InitialSelection,
    pub fetch: bool,
}

// The page the app opens on.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StartPage {
    Graph,
    LocalChanges,
}

// The longest the fetch on launch runs, for a remote that never answers; long enough for a big
// first fetch over a slow link, which would otherwise start over on each launch.
const LAUNCH_FETCH_TIMEOUT: Duration = Duration::from_secs(300);

// How long the fetch, pull and push popups wait for their key to be pressed again before showing,
// as a double press runs the command right away.
const QUICK_RUN_DELAY: Duration = Duration::from_millis(200);

pub enum Ret {
    Quit,
    // the repository reloaded in the background, to show the view of the context again on, see
    // `App::start_reload`
    Reload {
        context: Box<RefreshViewContext>,
        repository: Box<Repository>,
        // its graph, laid out on that thread too
        layout: Box<GraphLayout>,
        // see `Ret::Regraph`
        images: Option<GraphImages>,
    },
    // draw the graph again as laid out after folding a merge, see `App::start_regraph`
    Regraph {
        context: Box<RefreshViewContext>,
        layout: Box<GraphLayout>,
        // those of the rows drawn the same are kept, see `GraphImageManager::with_images`
        images: Option<GraphImages>,
    },
}

#[derive(Debug)]
pub struct AppContext {
    pub keybind: KeyBind,
    pub core_config: CoreConfig,
    pub ui_config: UiConfig,
    pub color_theme: ColorTheme,
    pub image_protocol: ImageProtocol,
    // the graph images are drawn with, shared by the main graph and the submodule graphs the
    // pages build beside it, see `graph_image_manager`
    pub graph: RefCell<GraphRender>,
    // the git command running in the background, kept across refreshes, shared with the repository
    pub running_task: SharedTask,
    // the keys of the hints in the status bar, see `short_keys`
    pub key_names: RefCell<FxHashMap<Vec<UserEvent>, String>>,
    // the sizes of the panels, set by the user, kept across refreshes
    pub layout: RefCell<PanelLayout>,
    // long lines of the diffs and file previews wrapped, see `UserEvent::ToggleWrap`
    pub wrap: Cell<bool>,
    // the lines shown around each change in a diff, see `UserEvent::IncreaseDiffContext`
    pub diff_context: Cell<usize>,
    // the options of every search, toggled while typing any of them, shared with the commit list
    pub search_options: Rc<Cell<SearchOptions>>,
    // typing the keys of a paste but its last, which a search applies once, see `App::paste`
    pub pasting: Cell<bool>,
}

// What a commit graph image is drawn with. The color set, style and antialias come from the
// config; the cell width is decided once for the main graph and reused by the submodule graphs.
#[derive(Debug, Clone)]
pub struct GraphRender {
    pub color_set: GraphColorSet,
    pub style: GraphStyle,
    pub antialias: bool,
    pub image_width_mode: GraphImageWidthMode,
    pub cell_width_type: CellWidthType,
}

impl Default for GraphRender {
    fn default() -> Self {
        Self {
            color_set: GraphColorSet::new(&GraphColorConfig::default()),
            style: GraphStyle::Rounded,
            antialias: true,
            image_width_mode: GraphImageWidthMode::Compact,
            cell_width_type: CellWidthType::Double,
        }
    }
}

impl AppContext {
    // The image manager of a graph built beside the main one, drawn with the same settings.
    pub fn graph_image_manager<'a>(&self, graph: Graph<'a>) -> GraphImageManager<'a> {
        let render = self.graph.borrow();
        let image_params = crate::graph::ImageParams::new(
            &render.color_set,
            render.cell_width_type,
            render.antialias,
        )
        .fit_cell(crate::protocol::cell_pixels());
        GraphImageManager::new(
            graph,
            image_params,
            render.cell_width_type,
            render.style,
            render.image_width_mode,
            self.image_protocol,
        )
    }

    // The keys of the events for a hint, see `KeyBind::short_keys`, found once for each.
    pub fn short_keys(&self, events: &[UserEvent]) -> String {
        let mut names = self.key_names.borrow_mut();
        names
            .entry(events.to_vec())
            .or_insert_with(|| self.keybind.short_keys(events))
            .clone()
    }

    // Why the repository can't be changed now: a git command is running in the background.
    pub fn busy_message(&self) -> Option<String> {
        self.running_task
            .lock()
            .as_ref()
            .map(RunningTask::busy_message)
    }
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
    // the refs, HEAD and the operation in progress as loaded, see `git::refs_snapshot`, so the app's own changes are not
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
    // the merge commits folded in the graph, see `UserEvent::NavigateLeft`
    folded_merges: FxHashSet<CommitHash>,
    // a check of the refs or the files came while reloading was paused, see `handle_event`
    missed_check: bool,
    reload: Option<PendingReload>,
    // counts the reloads asked for, see `PendingReload`
    reload_generation: u64,
    // the blame or timeline opening, see `App::open_window`
    opening: Option<Opening>,
    // when each layout of the graph running after folding merges started, see `App::start_regraph`
    regraphs: Vec<Instant>,
}

// What the app shows again once the repository reloaded in the background lands.
#[derive(Debug)]
enum Restore {
    // what it shows then, moving meanwhile kept; else what it showed when asked, when what it
    // shows then can't be reloaded, like the help
    Current(Box<RefreshViewContext>),
    // what a command asked for, like the commit it rewrote selected
    Given(Box<RefreshViewContext>),
}

// The repository loaded again in the background, with its graph laid out there too.
#[derive(Debug)]
pub struct Reloaded {
    loaded: Loaded,
    layout: GraphLayout,
}

// A window over everything whose file history loads on another thread, see `App::open_window`.
#[derive(Debug)]
struct Opening {
    window: OpeningWindow,
    // the file, named in the status bar while it loads, see `App::background_activity`
    path: String,
    // redraws the spinner meanwhile
    _spinning: std::sync::mpsc::Sender<()>,
}

#[derive(Debug)]
enum OpeningWindow {
    Blame {
        job: Job<FileBlameResult>,
        line: Option<usize>,
    },
    Timeline(Job<FileHistoryResult>),
}

impl Opening {
    // What the status bar says once it takes long, see `shows_loading`.
    fn activity(&self) -> Option<String> {
        let (name, started) = match &self.window {
            OpeningWindow::Blame { job, .. } => ("blame", job.started()),
            OpeningWindow::Timeline(job) => ("timeline", job.started()),
        };
        shows_loading(started).then(|| format!("Loading the {name} of {}", self.path))
    }
}

// The reload of the repository running in the background, see `App::start_reload`.
#[derive(Debug)]
struct PendingReload {
    // of the first one asked for while the others ran, see `App::background_activity`
    started: Instant,
    // of the latest one asked for: a result of one asked for before is dropped
    generation: u64,
    restore: Restore,
}

// What covers the page and the sidebar, taking the keys: the keys of the whole app (see
// `UserEvent::HelpToggle`), the blame or the timeline of a file, or the git commands run (see
// `UserEvent::CommandLog`), over everything.
#[derive(Debug)]
enum Overlay {
    Help(HelpView),
    File(FileWindow),
    CommandLog(CommandLogState),
}

impl Overlay {
    // Its name in the page bar, with the path of the file of the blame and the timeline.
    fn title(&self) -> (&'static str, Option<&str>) {
        match self {
            Overlay::Help(_) => ("Help", None),
            Overlay::File(FileWindow::Blame(blame)) => ("Blame", Some(blame.path())),
            Overlay::File(FileWindow::Timeline(timeline)) => ("Timeline", Some(timeline.path())),
            Overlay::CommandLog(_) => ("Command Log", None),
        }
    }
}

// The blame or the timeline of a file, see `UserEvent::Blame` and `UserEvent::Timeline`.
#[derive(Debug)]
enum FileWindow {
    Blame(Box<BlameView>),
    Timeline(Box<TimelineView>),
}

impl FileWindow {
    // While a search is typed, keys go to it as they are.
    fn is_typing(&self) -> bool {
        match self {
            FileWindow::Blame(blame) => blame.is_typing(),
            FileWindow::Timeline(timeline) => timeline.is_typing(),
        }
    }

    fn handle_event(&mut self, event: UserEvent, key: KeyEvent, repository: &Repository) {
        match self {
            FileWindow::Blame(blame) => blame.handle_event(event, key, repository),
            FileWindow::Timeline(timeline) => timeline.handle_event(event, key, repository),
        }
    }

    fn keys(&self) -> KeyList {
        match self {
            FileWindow::Blame(blame) => blame.keys(),
            FileWindow::Timeline(timeline) => timeline.keys(),
        }
    }

    // See `View::resizable_panel`.
    fn resizable_panel(&self, size: Size) -> Option<Panel> {
        match (self, size) {
            (FileWindow::Blame(_), Size::Width) => Some(Panel::BlameCommits),
            (FileWindow::Timeline(_), Size::Width) => Some(Panel::TimelineCommits),
            (_, Size::Height) => None,
        }
    }

    fn move_focus(&mut self, direction: Direction) {
        match self {
            FileWindow::Blame(blame) => blame.move_focus(direction),
            FileWindow::Timeline(timeline) => timeline.move_focus(direction),
        }
    }

    fn render(&mut self, f: &mut Frame, area: Rect) {
        match self {
            FileWindow::Blame(blame) => blame.render(f, area),
            FileWindow::Timeline(timeline) => timeline.render(f, area),
        }
    }

    // Before drawing: what loads in the background, once loaded: the blame of another commit,
    // the diff of the timeline.
    fn update(&mut self, repository: &Repository) {
        match self {
            FileWindow::Blame(blame) => blame.poll_loading(),
            FileWindow::Timeline(timeline) => timeline.poll_loading(repository),
        }
    }
}

// A popup confirming a git command; `reveal` is when it shows, while it waits for the key opening
// it to be pressed again, which runs the command right away, see `QUICK_RUN_DELAY`.
#[derive(Debug)]
struct Popup {
    dialog: GitDialog,
    reveal: Option<Instant>,
}

// Whether diffing `compare` is diffing with the current branch, whose commits the graph then dims
// off it like everywhere; two other refs are shown alike.
fn compare_with_head(compare: &Compare, repository: &Repository) -> bool {
    let branch = repository.current_branch();
    let is_head = |rev: &Rev| match rev {
        Rev::Head => true,
        Rev::Ref(NamedRef::Branch(name)) => branch.as_deref() == Some(name),
        Rev::Commit(_) | Rev::Ref(_) => false,
    };
    is_head(&compare.base) || is_head(&compare.target)
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
        refs: Compare,
        before: Box<GraphFilter>,
    },
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
            GraphFilter::Compare { refs, .. } => {
                Some(vec![refs.base.git_name(), refs.target.git_name()])
            }
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
            GraphFilter::Compare { refs, .. } => {
                Some(format!("{} ↔ {}", refs.base.shown(), refs.target.shown()))
            }
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
    // shared with the threads laying its graph out again, see `AppEvent::FoldMerge`
    repository: &'a Arc<Repository>,
    // how to load it again
    load_options: LoadOptions,
    view: View<'a>,
    shell: AppShell,
    ctx: Rc<AppContext>,
    ec: &'a EventController,
}

impl<'a> App<'a> {
    pub fn new(
        repository: &'a Arc<Repository>,
        load_options: LoadOptions,
        graph_image_manager: GraphImageManager<'a>,
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
        // kept past the move of the manager into the commit list below
        let warning = graph.warning.clone();
        let mut ref_name_to_commit_index_map = FxHashMap::default();
        let head_history = repository.head_history();
        // diffing a ref from the sidebar, the commits off the current branch are dimmed as
        // everywhere when one of the two is it, else both are shown alike
        let dim_off_head = shell.as_ref().is_none_or(|s| match &s.graph_filter {
            GraphFilter::Compare { refs, .. } => compare_with_head(refs, repository),
            _ => true,
        });
        let commits = graph
            .commits
            .iter()
            .enumerate()
            .map(|(i, commit)| {
                let refs = repository.refs(&commit.commit_hash);
                // by the names git takes for them alone, as a tag may have a branch's name
                for r in &refs {
                    let name = r.named().map_or_else(|| r.name().to_string(), |n| n.full_name());
                    ref_name_to_commit_index_map.insert(name, i);
                }
                // the detached HEAD's row in the sidebar selects it
                if matches!(repository.head(), Head::Detached { target } if *target == commit.commit_hash)
                {
                    ref_name_to_commit_index_map.insert("HEAD".to_string(), i);
                }
                let on_head = !dim_off_head || head_history.contains(&commit.commit_hash);
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
            ctx.search_options.clone(),
            ctx.ui_config.common.scrolloff as usize,
        );
        // the checked-out branch, or the detached HEAD, marked in the graph and the sidebar
        commit_list_state.set_head(repository.head().clone());
        commit_list_state.set_filter_note(
            shell
                .as_ref()
                .and_then(|shell| shell.graph_filter_note(repository)),
        );
        if let InitialSelection::Head = launch.selection {
            match repository.head() {
                Head::Branch { name, .. } => {
                    commit_list_state.select_ref(&NamedRef::Branch(name.clone()).full_name())
                }
                Head::Detached { target } => {
                    commit_list_state.select_commit_hash(target);
                }
                Head::None => {}
            }
        }
        let view = View::of_list(commit_list_state, repository, ctx.clone(), ec.sender());

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
                folded_merges: FxHashSet::default(),
                missed_check: false,
                reload: None,
                reload_generation: 0,
                opening: None,
                regraphs: Vec::new(),
            },
        };
        // gone, like after a rebase
        shell
            .folded_merges
            .retain(|h| repository.commit(h).is_some());
        shell
            .sidebar
            .set_tracking(repository.branch_tracking().clone());

        let mut app = Self {
            repository,
            load_options,
            view,
            shell,
            ctx,
            ec,
        };

        if let Some(warning) = &warning {
            app.warn_notification(warning.clone());
        }

        match refresh_view_context {
            Some(context) => app.init_with_context(context),
            None => {
                if launch.page == StartPage::LocalChanges {
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

    // Handles one event; returns what ends the app or reloads it. A check of the refs or the files
    // skipped meanwhile, while reloading was paused, is made again once it isn't, so the repository
    // as loaded keeps up as well as it can.
    fn handle_event(&mut self, event: AppEvent) -> Result<Option<Ret>, std::io::Error> {
        let ret = self.handle(event)?;
        if self.shell.missed_check && !self.refresh_paused() {
            self.shell.missed_check = false;
            self.ec.refresh_now();
        }
        Ok(ret)
    }

    fn handle(&mut self, event: AppEvent) -> Result<Option<Ret>, std::io::Error> {
        match event {
            AppEvent::Key(key) => {
                match self.shell.app_status.status_line {
                    StatusLine::None | StatusLine::Input { .. } => {
                        // do nothing
                    }
                    StatusLine::Transient(_)
                    | StatusLine::SearchResult { .. }
                    | StatusLine::NotificationSuccess(_) => {
                        // Clear message and pass key input as is
                        self.clear_status_line();
                    }
                }

                if matches!(self.shell.overlay, Some(Overlay::CommandLog(_))) {
                    if let Some(event) = self.ctx.keybind.event_in(Scope::Global, &key) {
                        self.dispatch(event, key);
                    }
                    return Ok(None);
                }
                if self.shell.dialog.is_some() {
                    self.handle_dialog_key(key);
                    return Ok(None);
                }
                // typing a search in the blame or the timeline, keys go to it as they are; else
                // they are resolved like the page's, sequences too, see `dispatch`
                if matches!(&self.shell.overlay, Some(Overlay::File(w)) if w.is_typing()) {
                    if let (false, Some(Overlay::File(window))) =
                        (self.force_quit(&key), self.shell.overlay.as_mut())
                    {
                        window.handle_event(UserEvent::Unknown, key, self.repository);
                    }
                    return Ok(None);
                }
                if matches!(self.shell.overlay, Some(Overlay::Help(_))) {
                    if let Some(event) = self.ctx.keybind.event_in(Scope::Global, &key) {
                        self.dispatch(event, key);
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

                // key sequences wait for their next key; the focused scope decides, like ]
                // cycling the tabs of the details where elsewhere it does nothing
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

                if let Some(event) = self.ctx.keybind.event_in(scope, &key) {
                    self.dispatch(event, key);
                }
            }
            // the wheel moves the selection of what has the focus like j and k, whatever keys they
            // are bound to; over a popup, or while typing, it does nothing
            AppEvent::Mouse(mouse)
                if matches!(
                    mouse.kind,
                    MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
                ) =>
            {
                let (event, code) = match mouse.kind {
                    MouseEventKind::ScrollDown => (UserEvent::NavigateDown, KeyCode::Down),
                    _ => (UserEvent::NavigateUp, KeyCode::Up),
                };
                if self.shell.dialog.is_none() && !self.is_typing() {
                    self.dispatch(event, KeyEvent::new(code, KeyModifiers::NONE));
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
            AppEvent::Quit => {
                // quitting would kill the git command half done (a stash never popped), so it
                // waits; asking again quits right away
                let mut running = self.ctx.running_task.lock();
                if let Some(running) = running.as_mut().filter(|running| !running.quit_after) {
                    running.quit_after = true;
                    self.ec.send(AppEvent::NotifyWarn(format!(
                        "Quitting once {} finishes, quit again to quit now",
                        running.task.running
                    )));
                    return Ok(None);
                }
                drop(running);
                return Ok(Some(Ret::Quit));
            }
            AppEvent::Paste(text) => self.paste(&text)?,
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
            AppEvent::TaskFinished(result) => {
                if self.finish_task(result) {
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
            // checked again once reloading isn't paused, see `handle_event`
            AppEvent::FilesTick(_) | AppEvent::RefsSnapshot(_) => self.shell.missed_check = true,
            // only redraws
            AppEvent::SpinnerTick | AppEvent::Resize | AppEvent::Loaded => {}
            AppEvent::OpenHelp => {
                let help = HelpView::new(self.ctx.clone(), self.ec.sender());
                self.shell.overlay = Some(Overlay::Help(help));
            }
            AppEvent::CloseOverlay => {
                self.shell.overlay = None;
            }
            // walking the file's history takes long in a big repository: the window opens once it
            // is loaded on another thread, see `open_window`
            AppEvent::OpenBlame { commit, path, line } => {
                let work = self.repository.file_blame_work(&commit, &path);
                let job = Job::spawn(self.ec.sender(), work);
                self.shell.opening = Some(Opening {
                    window: OpeningWindow::Blame { job, line },
                    path,
                    _spinning: spin(self.ec.sender()),
                });
            }
            AppEvent::OpenTimeline { commit, path } => {
                let work = self.repository.file_history_work(&commit, &path);
                self.shell.opening = Some(Opening {
                    window: OpeningWindow::Timeline(Job::spawn(self.ec.sender(), work)),
                    path,
                    _spinning: spin(self.ec.sender()),
                });
            }
            AppEvent::Checkout(commit) => {
                let command = graph_command(self.repository, UserEvent::Checkout, commit, vec![]);
                self.run_graph_command(UserEvent::Checkout, command);
            }
            AppEvent::ShowCommit(commit) => self.show_commit(&commit),
            AppEvent::OpenDialog(dialog) => self.open_dialog(Ok(*dialog)),
            AppEvent::CopyToClipboard { name, value } => {
                self.copy_to_clipboard(name, value);
            }
            AppEvent::Link { target, action } => self.open_link(&target, action),
            AppEvent::Links { targets, action } => self.handle_links(&targets, action),
            AppEvent::OpenUrl { url, action } => self.open_url(url, action),
            AppEvent::Reload => {
                if let Some(context) = self.view.refresh_context() {
                    self.start_reload(Restore::Current(Box::new(context)));
                }
            }
            AppEvent::Refresh(context) => self.start_reload(Restore::Given(Box::new(context))),
            AppEvent::Reloaded { generation, result } => {
                return Ok(self.land_reload(generation, result));
            }
            AppEvent::FoldMerge { commit, fold } => {
                match fold {
                    Fold::Folded => self.shell.folded_merges.insert(commit),
                    Fold::Unfolded => self.shell.folded_merges.remove(&commit),
                };
                self.start_regraph();
            }
            AppEvent::Regraphed { from, layout } => {
                if !self.shell.regraphs.is_empty() {
                    self.shell.regraphs.remove(0);
                }
                // laid out before the repository was loaded again, of commits it may no longer
                // have; the app made again on it laid its graph out with the merges folded now
                if !Arc::ptr_eq(&from, self.repository) {
                    return Ok(None);
                }
                // one of merges folded since is laid out after it
                let context = self.view.refresh_context();
                if let Some(context) = context.filter(|_| layout.folded == self.shell.folded_merges)
                {
                    // drawn over the last frame: only the rows that changed are drawn again
                    let images = self
                        .take_list_state()
                        .map(CommitListState::into_graph_images);
                    let context = Box::new(context);
                    return Ok(Some(Ret::Regraph {
                        context,
                        layout,
                        images,
                    }));
                }
            }
            AppEvent::ClearStatusLine => {
                self.clear_status_line();
            }
            AppEvent::UpdateStatusInput {
                message,
                cursor_byte_index,
                metadata,
            } => {
                self.shell.app_status.status_line = StatusLine::Input {
                    message,
                    cursor_byte_index,
                    metadata,
                };
            }
            AppEvent::UpdateStatusTransient(msg) => {
                self.shell.app_status.status_line = StatusLine::Transient(msg);
            }
            AppEvent::UpdateSearchResult { outcome, options } => {
                self.shell.app_status.status_line = StatusLine::SearchResult { outcome, options };
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

    // Types pasted text where keys are typed, as if pressed one at a time; elsewhere it is no keys,
    // which could run anything. A newline or a tab would finish typing or switch fields, so it is
    // a space, but in the commit message, where it is the new line or the tab itself.
    fn paste(&mut self, text: &str) -> Result<(), std::io::Error> {
        if self.shell.dialog.is_none() && !self.is_typing() {
            return Ok(());
        }
        let message = self.view.is_typing_message();
        // terminals send a newline as \r, some as \r\n
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let count = text.chars().count();
        for (i, c) in text.chars().enumerate() {
            let code = match c {
                '\n' if message => KeyCode::Enter,
                '\n' | '\t' if !message => KeyCode::Char(' '),
                c => KeyCode::Char(c),
            };
            self.ctx.pasting.set(i + 1 < count);
            let handled = self.handle(AppEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)));
            self.ctx.pasting.set(false);
            handled?;
        }
        Ok(())
    }

    // Lays the page out in the screen's `area` before handling the next events and drawing.
    fn prepare_render(&mut self, area: Rect) {
        let [_, view_area, _] = split_app_areas(area);
        let [sidebar_area, view_area] = self.split_sidebar(view_area);
        let scrolloff = self.ctx.ui_config.common.scrolloff as usize;
        self.shell.sidebar.fit(sidebar_area, scrolloff);
        self.update_state(view_area);
        self.view.update_layout(view_area);
        self.view.prepare_graph_uploads();
        self.open_window();
        if let Some(popup) = &mut self.shell.dialog {
            popup.dialog.poll_conflicts();
        }
        if let Some(Overlay::File(window)) = &mut self.shell.overlay {
            window.update(self.repository);
        }
    }

    // Opens the blame or timeline once its file history is loaded, see `AppEvent::OpenBlame`.
    fn open_window(&mut self) {
        let (ctx, tx) = (self.ctx.clone(), self.ec.sender());
        let Some(opening) = self.shell.opening.take() else {
            return;
        };
        let window = match opening.window {
            OpeningWindow::Blame { job, line } => match job.try_take() {
                None => {
                    let window = OpeningWindow::Blame { job, line };
                    self.shell.opening = Some(Opening { window, ..opening });
                    return;
                }
                Some(loaded) => loaded.map(|(history, index, blame)| {
                    let view = BlameView::of(history, index, blame, line, ctx, tx);
                    FileWindow::Blame(Box::new(view))
                }),
            },
            OpeningWindow::Timeline(job) => match job.try_take() {
                None => {
                    let window = OpeningWindow::Timeline(job);
                    self.shell.opening = Some(Opening { window, ..opening });
                    return;
                }
                Some(loaded) => loaded.map(|(history, selected)| {
                    let mut view = TimelineView::of(history, selected, ctx, tx);
                    view.show_selected(self.repository);
                    FileWindow::Timeline(Box::new(view))
                }),
            },
        };
        match window {
            Ok(window) => self.shell.overlay = Some(Overlay::File(window)),
            Err(err) => self.ec.send(AppEvent::NotifyError(err)),
        }
    }

    // Whether what is shown loads in the background, see `Job`.
    #[cfg(test)]
    pub fn is_loading(&self) -> bool {
        let window = match &self.shell.overlay {
            Some(Overlay::File(FileWindow::Timeline(timeline))) => timeline.is_loading(),
            Some(Overlay::File(FileWindow::Blame(blame))) => blame.is_loading(),
            _ => false,
        };
        let shell = &self.shell;
        let checking = shell
            .dialog
            .as_ref()
            .is_some_and(|p| p.dialog.is_checking_conflicts());
        window
            || checking
            || shell.opening.is_some()
            || !shell.regraphs.is_empty()
            || self.view.is_loading()
    }

    fn draw(&mut self, terminal: &mut DefaultTerminal) -> Result<(), std::io::Error> {
        let keys = self.keys_popup_keys();
        let clear = self.frame_clear();
        match clear {
            PendingClear::None => {}
            PendingClear::ViewImages => self.clear_view_image_area(terminal)?,
            PendingClear::Screen => {
                self.ctx.image_protocol.clear();
                repaint_fullscreen(terminal);
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
            repaint_fullscreen(terminal);
            terminal.draw(|f| self.render(f, &keys))?;
        }
        Ok(())
    }

    // The keys of what has the focus: the popup, the sidebar or the view.
    fn local_hints(&self) -> Vec<KeyHint> {
        if let Some(dialog) = self.visible_dialog() {
            dialog.keys().hints()
        } else if let Some(mut keys) = self.overlay_keys() {
            // and the keys of the app working over it
            keys.extend(self.app_keys());
            keys.hints()
        } else if self.shell.sidebar_focused() {
            self.sidebar_keys().hints()
        } else {
            self.view.keys().hints()
        }
    }

    // The keys and what they do, without those bound to no key.
    fn shown_keys(&self, hints: Vec<KeyHint>) -> Vec<KeyLabel> {
        hints
            .into_iter()
            .map(|(events, action)| KeyLabel::new(self.ctx.short_keys(&events), action))
            .filter(|label| !label.keys.is_empty())
            .collect()
    }

    // Moves in the command log, scrolls the output of the selected command, copies it, or closes.
    fn handle_command_log_event(&mut self, event: UserEvent) {
        let Some(Overlay::CommandLog(state)) = self.shell.overlay.as_mut() else {
            return;
        };
        match event {
            UserEvent::Cancel | UserEvent::CommandLog => self.shell.overlay = None,
            UserEvent::FocusPanel(n) => state.focus_panel(n),
            event if state.move_selection(event) => {}
            event if state.scroll_output(event) => {}
            UserEvent::ShortCopy | UserEvent::FullCopy => {
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

    // Whether a search or filter is typed, in the window over everything or the page.
    fn is_typing(&self) -> bool {
        match &self.shell.overlay {
            Some(Overlay::File(window)) => window.is_typing(),
            _ => self.view.is_capturing_keys(),
        }
    }

    // The keys of the focused panel and what they do, for the keys popup; empty when it is
    // hidden. Computed once a frame, for clearing the screen and drawing.
    fn keys_popup_keys(&self) -> Vec<KeyLabel> {
        // over the windows with keys of their own, not the help, which lists them all
        let shown = match self.shell.overlay {
            Some(Overlay::File(_) | Overlay::CommandLog(_)) => {
                self.visible_dialog().is_none() && !self.is_typing()
            }
            _ => !self.keys_are_local(),
        };
        if !self.shell.keys_popup || !shown {
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
            self.app_keys().hints()
        });
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

    fn render(&mut self, f: &mut Frame, keys: &[KeyLabel]) {
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
        // the windows under the popups; the command log is drawn over them, below
        if let Some(Overlay::Help(_) | Overlay::File(_)) = self.shell.overlay {
            f.render_widget(Clear, app_area);
            f.render_widget(base, app_area);
        }
        match self.shell.overlay.as_mut() {
            Some(Overlay::Help(help)) => help.render(f, app_area),
            Some(Overlay::File(window)) => window.render(f, app_area),
            _ => {}
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
        // over the command log too, which takes the keys it lists
        if !keys.is_empty() {
            let popup = KeysPopup::new(keys, &self.ctx.color_theme);
            f.render_widget(popup, app_area);
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
        // a window over them, like the help, after them, with its file
        let window = self.shell.overlay.as_ref().map(Overlay::title);
        let pill = |name: String, color| {
            [
                Span::raw("\u{e0b6}").fg(color),
                badge(name, color, theme),
                Span::raw("\u{e0b4}").fg(color),
            ]
        };
        let mut spans = vec![Span::raw(" ")];
        for (name, shown, color) in pages {
            if shown && window.is_none() {
                spans.extend(pill(name, color));
            } else {
                spans.push(Span::raw(format!(" {name} ")).fg(theme.muted_fg));
            }
            spans.push(Span::raw("  "));
        }
        if let Some((name, path)) = window {
            spans.extend(pill(name.into(), theme.accent_fg));
            if let Some(path) = path {
                spans.push(Span::raw(format!(" {path}")).fg(theme.muted_fg));
            }
        }
        f.render_widget(Line::from(spans), area);
    }

    // What runs in the background, with a spinner in the status bar: the git command, else what
    // loads once it takes long (see `shows_loading`): the repository, the graph laid out again, or
    // the file history of a window.
    fn background_activity(&self) -> Option<String> {
        if let Some(running) = self.ctx.running_task.lock().as_ref() {
            return Some(running.task.running.clone());
        }
        let shell = &self.shell;
        let reload = shell.reload.as_ref().map(|r| (r.started, "Reloading"));
        let regraph = shell.regraphs.first().map(|&s| (s, "Drawing the graph"));
        reload
            .into_iter()
            .chain(regraph)
            .find(|&(started, _)| shows_loading(started))
            .map(|(_, activity)| activity.into())
            .or_else(|| shell.opening.as_ref().and_then(Opening::activity))
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
                match self.background_activity() {
                    Some(activity) => left.push(
                        Span::raw(format!("{activity} {}", spinner())).fg(theme.highlight_fg),
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
                            Head::Branch { name, .. } => Some(name.clone()),
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
                message,
                cursor_byte_index,
                metadata,
            } => {
                let input = status_input_line(
                    message,
                    *cursor_byte_index,
                    &self.ctx.ui_config.common.cursor_type,
                    Style::default().fg(theme.status_bar_fg),
                    Style::default().fg(theme.virtual_cursor_fg),
                );
                left.extend(input.spans);
                right.push(Span::raw(format!("{metadata} ")).fg(theme.muted_fg));
            }
            StatusLine::Transient(msg) => {
                right.push(Span::raw(format!("{msg} ")).fg(theme.muted_fg));
            }
            StatusLine::SearchResult { outcome, options } => {
                let span = Span::raw(outcome.to_string());
                left.push(match outcome {
                    SearchOutcome::Found { .. } => span.fg(theme.status_info_fg),
                    SearchOutcome::NotFound { .. } => span.fg(theme.status_warn_fg).bold(),
                });
                right.push(Span::raw(format!("{options} ")).fg(theme.muted_fg));
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
        if let Some(activity) = self.background_activity().filter(|_| message) {
            let text = format!("{activity} {} ", spinner());
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

        if let (
            StatusLine::Input {
                message,
                cursor_byte_index,
                ..
            },
            CursorType::Native,
        ) = (
            &self.shell.app_status.status_line,
            &self.ctx.ui_config.common.cursor_type,
        ) {
            let cursor_position = message[..*cursor_byte_index].width();
            let x = area.x + (left_width + cursor_position) as u16;
            f.set_cursor_position((x, area.y));
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

// " [1] " and the border on its right
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

// Has the next frame written over every cell of the screen, as on a cleared one but without the
// screen going blank in between: what is there without having been drawn, like what a program run
// in the terminal printed, is written over too. The frame is diffed with a buffer it differs from
// in every cell; Terminal::clear would also query the cursor position, competing with our input
// thread.
fn repaint_fullscreen<B: Backend>(terminal: &mut Terminal<B>) {
    // all the modifiers at once, which nothing draws
    for cell in &mut terminal.current_buffer_mut().content {
        cell.modifier = ratatui::style::Modifier::all();
    }
    // the next frame drawn into the other, reset, diffed with this one
    terminal.swap_buffers();
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

    // Hands the event to the app, when it is one of its keys (see `register_app_keys`), else to
    // the window over everything, the focused sidebar or the page, when one of their keys works
    // there: no key works without being listed in the hints and the help.
    fn dispatch(&mut self, event: UserEvent, key: KeyEvent) {
        if self.app_keys().takes(event) {
            self.handle_app_event(event);
            return;
        }
        if let Some(keys) = self.overlay_keys() {
            if keys.takes(event) {
                let event = keys.meaning(event);
                match self.shell.overlay.as_mut() {
                    Some(Overlay::File(window)) => window.handle_event(event, key, self.repository),
                    Some(Overlay::Help(help)) => help.handle_event(event),
                    Some(Overlay::CommandLog(_)) => self.handle_command_log_event(event),
                    None => {}
                }
            }
            return;
        }
        if self.shell.sidebar_focused() {
            self.handle_sidebar_event(event, key);
            return;
        }
        let keys = self.view.keys();
        if !keys.takes(event) {
            return;
        }
        match keys.meaning(event) {
            event if event.home_scope() == Scope::Graph => self.handle_graph_event(event),
            event => self.view.handle_event(event, key),
        }
    }

    // The keys of the whole app: those of the pages work wherever the focus is on them, those of
    // how things look (the keys popup, wrapping, resizing and moving the focus) over the blame and
    // the timeline too, so a page or window has them without handling them itself.
    pub fn register_app_keys(k: &mut Keys<Self>) {
        use UserEvent::*;
        let page = |app: &Self| app.shell.overlay.is_none();
        let page_or_window =
            |app: &Self| matches!(app.shell.overlay, None | Some(Overlay::File(_)));
        k.add(
            &[KeysToggle],
            "keys",
            "Show/hide the keys of the focused panel",
        )
        .when(|app| !matches!(app.shell.overlay, Some(Overlay::Help(_))));
        // with no remote, pulling on a detached HEAD or while an operation waits, unlisted but
        // saying so when pressed
        let remote_keys = [
            (Push, "push", "Push a branch to a remote"),
            (Pull, "pull", "Pull a branch of a remote"),
            (Fetch, "fetch", "Fetch from a remote"),
            (FetchAll, "", "Fetch all remotes"),
        ];
        for (event, short, long) in remote_keys {
            k.add_where(&[event], short, long, move |app| {
                let runs = !app.repository.remotes().is_empty()
                    && (event != Pull || app.repository.current_branch().is_some())
                    && !(event.moves_head() && app.repository.operation_in_progress().is_some());
                match (page(app), runs) {
                    (false, _) => Availability::Off,
                    (true, false) => Availability::Warns,
                    (true, true) => Availability::Works,
                }
            });
        }
        k.add(
            &[RefList],
            "switch page",
            "Switch between local changes and the commit graph",
        )
        .when(page);
        k.add(&[Refresh], "refresh", "Refresh").when(page);
        k.add(
            &[CommandLog],
            "log",
            "Show the git commands run that changed the repository, with their output",
        )
        .when(page);
        k.add(&[HelpToggle], "help", "Open help").when(page);
        k.add(
            &[ToggleWrap],
            "wrap",
            "Wrap the long lines of the diffs and file previews, or not",
        )
        .when(page_or_window);
        k.quiet(
            &[DismissNotifications],
            "Dismiss the errors and warnings at the top right",
        )
        .when(page_or_window);
        k.quiet(
            &[ResizeLeft, ResizeRight],
            "Move the border beside the focused panel (or drag it)",
        )
        .when(page_or_window);
        k.quiet(
            &[ResizeUp, ResizeDown],
            "Move the border above or under the focused panel (or drag it)",
        )
        .when(page_or_window);
        k.quiet(
            &[FocusLeft, FocusRight, FocusUp, FocusDown],
            "Focus the panel beside, above or under the focused one",
        )
        .when(page_or_window);
        k.quiet(
            &[FocusPanel(1)],
            "Focus the refs sidebar, again to shrink/grow it",
        )
        .when(|app| app.sidebar_shown() && app.shell.overlay.is_none());
        k.quiet(&[ForceQuit, Quit], "Quit app");
    }

    // The keys of the window over everything, if any.
    fn overlay_keys(&self) -> Option<KeyList> {
        Some(match self.shell.overlay.as_ref()? {
            Overlay::File(window) => window.keys(),
            Overlay::Help(help) => {
                let mut keys = Keys::of(help);
                HelpView::register_keys(&mut keys);
                keys.list()
            }
            Overlay::CommandLog(state) => {
                let mut keys = Keys::of(state);
                CommandLogState::register_keys(&mut keys);
                keys.list()
            }
        })
    }

    // The keys working everywhere, also in the status bar: the remotes, switching pages, the keys
    // popup and the help.
    fn app_keys(&self) -> KeyList {
        let mut keys = Keys::of(self);
        Self::register_app_keys(&mut keys);
        keys.list()
    }

    // The keys of `register_app_keys`, one implementation for every page and window.
    fn handle_app_event(&mut self, event: UserEvent) {
        match event {
            UserEvent::ForceQuit | UserEvent::Quit => self.ec.send(AppEvent::Quit),
            UserEvent::HelpToggle => self.ec.send(AppEvent::OpenHelp),
            UserEvent::Refresh => self.view.refresh(),
            UserEvent::Fetch | UserEvent::FetchAll | UserEvent::Pull | UserEvent::Push => {
                self.handle_remote_event(event);
            }
            UserEvent::ResizeLeft => self.move_border(Direction::Left),
            UserEvent::ResizeRight => self.move_border(Direction::Right),
            UserEvent::ResizeUp => self.move_border(Direction::Up),
            UserEvent::ResizeDown => self.move_border(Direction::Down),
            UserEvent::FocusLeft => self.move_focus(Direction::Left),
            UserEvent::FocusRight => self.move_focus(Direction::Right),
            UserEvent::FocusUp => self.move_focus(Direction::Up),
            UserEvent::FocusDown => self.move_focus(Direction::Down),
            UserEvent::KeysToggle => self.shell.keys_popup = !self.shell.keys_popup,
            UserEvent::CommandLog => {
                self.shell.overlay = Some(Overlay::CommandLog(CommandLogState::new()));
            }
            UserEvent::DismissNotifications => self.shell.notices.clear(),
            UserEvent::ToggleWrap => self.ctx.wrap.set(!self.ctx.wrap.get()),
            UserEvent::FocusPanel(1) => {
                // again on the focused sidebar, it shrinks out of the way; on a shrunk one, it grows back
                if self.shell.sidebar_mode == SidebarMode::Focused {
                    self.shell.sidebar_mode = SidebarMode::Collapsed;
                } else {
                    self.focus_sidebar();
                }
            }
            UserEvent::RefList => self.switch_page(),
            _ => {}
        }
    }

    fn handle_sidebar_event(&mut self, event: UserEvent, key: KeyEvent) {
        let keys = self.sidebar_keys();
        if !keys.takes(event) {
            return;
        }
        // space applies a stash, and checks out the other refs, as it does on the graph
        let event = keys.meaning(event);
        match event {
            _ if self.shell.sidebar.handle_move(event) => {
                if let Some(message) = self.shell.sidebar.visual_outside_section() {
                    self.ec.send(AppEvent::NotifyWarn(message));
                }
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
                    .select_section(Toward::of(event, UserEvent::NextSection));
                if let Some(message) = self.shell.sidebar.visual_outside_section() {
                    self.ec.send(AppEvent::NotifyWarn(message));
                }
                self.show_sidebar_selection();
            }
            UserEvent::Confirm => {
                // a folder opens or closes, else back to the page showing what is selected
                if !self.shell.sidebar.toggle_node() {
                    self.select_sidebar_selection();
                    self.blur_sidebar();
                }
            }
            // the picked refs are cleared first
            UserEvent::Cancel if !self.shell.sidebar.picked().is_empty() => {
                self.shell.sidebar.clear_picks();
            }
            UserEvent::Cancel => {
                self.blur_sidebar();
            }
            // on a ref, see `ref_command_applies`
            UserEvent::ToggleSelect => {
                if let Err(err) = self.shell.sidebar.toggle_visual() {
                    self.ec.send(AppEvent::NotifyWarn(err));
                }
            }
            UserEvent::ShortCopy | UserEvent::FullCopy => self.copy_sidebar_ref_name(),
            UserEvent::OpenInBrowser | UserEvent::CopyLink => {
                let target = match self.shell.sidebar.selection() {
                    RefSelection::Ref(NamedRef::Branch(name)) => LinkTarget::Branch(name),
                    RefSelection::Ref(NamedRef::RemoteBranch(name)) => {
                        LinkTarget::RemoteBranch(name)
                    }
                    RefSelection::Ref(NamedRef::Tag(name)) => LinkTarget::Tag(name),
                    RefSelection::Folder(RefSection::Remotes, name) => LinkTarget::Remote(name),
                    _ => return,
                };
                let action = LinkAction::of(event);
                self.ec.send(AppEvent::Link { target, action });
            }
            event @ (UserEvent::CreateRef
            | UserEvent::DeleteRef
            | UserEvent::EditRef
            | UserEvent::Select) => {
                let selection = self.shell.sidebar.selection();
                let picked = self.shell.sidebar.picked();
                if let Some(dialog) = ref_command(self.repository, event, &selection, picked) {
                    self.open_dialog(dialog);
                }
            }
            UserEvent::FocusPanel(_) => {
                // a panel of the page
                self.blur_sidebar();
                self.view.handle_event(event, key);
            }
            event if event.home_scope() == Scope::Graph => self.handle_sidebar_graph_event(event),
            _ => {}
        }
    }

    // The scope of the keys, where the focus is, see `Scope`.
    fn current_scope(&self) -> Scope {
        if let Some(Overlay::File(_)) = self.shell.overlay {
            return Scope::FileHistory;
        }
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

    // The keys of the refs sidebar, see `Keys`: the commands on the selected row (or the picked
    // refs) where they work, see `ref_command_applies`, then moving about.
    pub fn register_sidebar_keys(k: &mut Keys<Self>) {
        use RefSection::*;
        use UserEvent::*;
        let applies = |event| move |app: &Self| app.sidebar_command_applies(event);
        let picks = |app: &Self| !app.shell.sidebar.picked().is_empty();
        let on_stash = |app: &Self| {
            app.shell.sidebar.picked().section() == Some(Stashes)
                || matches!(app.shell.sidebar.selection(), RefSelection::Stash(_))
        };
        k.add(
            &[Cancel],
            "clear picks",
            "Clear the picked refs, else back to the page",
        )
        .when(picks);
        k.add(
            &[CreateRef],
            "new remote",
            "New remote, on a remote or the remotes header",
        )
        .when(applies(CreateRef));
        k.add(
            &[EditRef],
            "rename/upstream",
            "Rename the branch or change its upstream; edit the remote",
        )
        .when(applies(EditRef))
        .short(|app| match app.shell.sidebar.selection() {
            RefSelection::Folder(..) => "edit remote".into(),
            _ => "rename/upstream".into(),
        });
        k.add(
            &[DeleteRef],
            "delete",
            "Delete the branches or tags, drop the stashes; remove the remote",
        )
        .when(applies(DeleteRef))
        .short(|app| if on_stash(app) { "drop" } else { "delete" }.into());
        k.add(&[Select], "apply", "Apply the stash")
            .when(|app| on_stash(app) && app.sidebar_command_applies(Select));
        // the picked refs aren't commits, see `ref_command_applies`
        register_graph_commands(
            k,
            |app: &Self, event| match app.sidebar_command_applies(event) {
                true => Availability::Works,
                false => Availability::Off,
            },
            |_| false,
            |app| app.repository.operation_in_progress().is_some(),
        );
        k.add(
            &[ToggleSelect],
            "select",
            "Select the refs from the first to the selected one, of one kind",
        )
        .when(applies(ToggleSelect))
        .short(|app| {
            if app.shell.sidebar.is_visual() {
                "end select"
            } else {
                "select"
            }
            .into()
        });
        k.add(
            &[ShortCopy, FullCopy],
            "copy name",
            "Copy the name of the ref",
        )
        .when(applies(ShortCopy));
        k.add(
            &[OpenInBrowser],
            "open in browser",
            "Open the branch (or a new pull request), the remote branch, tag or remote on the web host",
        )
        .when(applies(OpenInBrowser));
        k.add(
            &[CopyLink],
            "copy link",
            "Copy the link of the branch, remote branch, tag or remote",
        )
        .when(applies(CopyLink));
        k.add(
            &[Confirm],
            "open",
            "Open/close a node, else back to the page",
        );
        k.add(
            &[Cancel],
            "back",
            "Clear the picked refs, else back to the page",
        )
        .when(|app| !picks(app));
        k.quiet(
            &[NavigateDown, SelectDown, NavigateUp, SelectUp],
            "Move down/up",
        );
        k.quiet(&[GoToTop, GoToBottom], "Go to the top/bottom");
        k.quiet(&[HalfPageDown, HalfPageUp], "Jump half a page down/up");
        k.quiet(&[PageDown, PageUp], "Scroll a page down/up");
        k.quiet(&[ScrollDown, ScrollUp], "Scroll down/up");
        k.quiet(&[NavigateRight, NavigateLeft], "Open/close the node");
        k.quiet(
            &[NextSection, PreviousSection],
            "Go to the next/previous section, opening it",
        );
        k.quiet(
            &[FocusPanel(2), FocusPanel(3), FocusPanel(4), FocusPanel(5)],
            "Focus a panel of the page",
        );
    }

    fn sidebar_keys(&self) -> KeyList {
        let mut keys = Keys::of(self);
        Self::register_sidebar_keys(&mut keys);
        keys.list()
    }

    // Whether the event does something on the selected row of the sidebar, see
    // `ref_command_applies`.
    fn sidebar_command_applies(&self, event: UserEvent) -> bool {
        if event == UserEvent::ToggleSelect && self.shell.sidebar.is_visual() {
            return true;
        }
        let selection = self.shell.sidebar.selection();
        let picked = self.shell.sidebar.picked();
        ref_command_applies(self.repository, event, &selection, picked)
    }

    // Focuses the sidebar where the page has one, growing it back when collapsed.
    fn focus_sidebar(&mut self) {
        if self.sidebar_shown() {
            self.shell.sidebar_mode = SidebarMode::Focused;
            self.view.blur();
        }
    }

    // Gives the focus back to the page from the sidebar.
    fn blur_sidebar(&mut self) {
        if self.shell.sidebar_mode == SidebarMode::Focused {
            self.shell.sidebar_mode = SidebarMode::Shown;
        }
    }

    // Shows the selected ref on the graph as the sidebar moves, when `ui.refs.follow` is on.
    fn show_sidebar_selection(&mut self) {
        if self.ctx.ui_config.refs.follow {
            self.select_sidebar_selection();
        }
    }

    // Shows the selected ref on the graph; the commit details stay open, showing its commit.
    fn select_sidebar_selection(&mut self) {
        let Some(name) = self.shell.sidebar.selected_git_name() else {
            return;
        };
        match &mut self.view {
            View::List(view) => view.select_ref(&name),
            View::Detail(view) => view.select_ref(&name),
            _ => {}
        }
    }

    // Selects the commit on the graph, from the blame, which closes; warns when the graph doesn't show it.
    fn show_commit(&mut self, commit: &CommitHash) {
        self.shell.overlay = None;
        // the details stay open
        if matches!(self.view, View::LocalChanges(_)) {
            self.show_graph_page();
        }
        self.blur_sidebar();
        if !self.view.select_commit(commit) {
            let message = format!("{} isn't shown in the graph", commit.as_short_hash());
            self.ec.send(AppEvent::NotifyWarn(message));
        }
    }

    // Tab: between the local changes page and the graph, focusing the page.
    fn switch_page(&mut self) {
        if matches!(self.view, View::LocalChanges(_)) {
            self.show_graph_page();
        } else {
            self.show_local_changes_page();
        }
        self.blur_sidebar();
    }

    // Shows the commit list alone, from a view showing it; returns whether it did.
    fn show_list(&mut self) -> bool {
        let Some(commit_list_state) = self.take_list_state() else {
            return false;
        };
        self.replace_view(View::of_list(
            commit_list_state,
            self.repository,
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
            LocalChangesState::restore(
                self.repository,
                self.ctx.clone(),
                snapshot,
                Some(self.ec.sender()),
            ),
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
        let picked = self.shell.sidebar.picked();
        if !picked.is_empty() {
            self.ec
                .send(AppEvent::copy("Ref Names", picked.names().join("\n")));
            return;
        }
        let (name, value) = match self.shell.sidebar.selection() {
            RefSelection::Ref(NamedRef::Branch(branch) | NamedRef::RemoteBranch(branch)) => {
                ("Branch Name", branch)
            }
            RefSelection::Ref(NamedRef::Tag(tag)) => ("Tag Name", tag),
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
                if dialog.command().is_some_and(|task| {
                    task.is_ok_and(|task| self.blocked_by_operation(&task))
                }) => {}
            Ok(mut dialog) => {
                dialog.check_conflicts(self.ec.sender());
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
        // only its keys working there, while not typing, which takes every key
        let event = event.filter(|event| dialog.is_editing() || dialog.keys().takes(*event));
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
    // command running; a fetch, pull or push of the user waits for it, see `run_task`. Nothing is
    // said when it fails, offline say, but in the command log; the remote branches it moved show
    // once the refs are next checked, see `check_refs`.
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
        // a checkout from the blame or the timeline shows the graph
        if task.moves_head() && matches!(self.shell.overlay, Some(Overlay::File(_))) {
            self.shell.overlay = None;
        }
        // a commit made clears the message box, see `finish_task`; read from git, as what the
        // command does is measured
        let head_before_commit = task
            .commits()
            .then(|| read_head_commit(self.repository.path()).unwrap_or_default());
        *self.ctx.running_task.lock() = Some(RunningTask {
            task: task.clone(),
            head_before_commit,
            quit_after: false,
        });
        let path = self.repository.path().to_path_buf();
        // signing may ask for a passphrase in the terminal, which the app gives back meanwhile
        if task.may_sign() && self.repository.signs() {
            self.ec.suspend();
            println!("{}…", task.running);
            let result = run_task(&path, &task);
            self.ec.resume();
            self.shell.app_status.pending_clear = PendingClear::Screen;
            self.ec.send(AppEvent::TaskFinished(result));
            return;
        }
        let tx = self.ec.sender();
        std::thread::spawn(move || {
            let spinning = spin(tx.clone());
            let result = run_task(&path, &task);
            drop(spinning);
            tx.send(AppEvent::TaskFinished(result));
        });
    }

    // Shows what the git command did; returns whether quitting waited for it.
    fn finish_task(&mut self, mut result: Result<(), String>) -> bool {
        // set by `start_task` before the command runs
        let Some(RunningTask {
            task,
            head_before_commit,
            quit_after: quit,
        }) = self.ctx.running_task.lock().take()
        else {
            return false;
        };
        if let (Err(_), Some(failed)) = (&result, &task.failed) {
            result = Err(failed.clone());
        }
        // the message is committed, even when going on with a rebase stopped afterwards
        let committed = head_before_commit.is_some_and(|before| {
            read_head_commit(self.repository.path()).unwrap_or_default() != before
        });
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
        // what is left to resolve or commit is on the local changes page; read from git, as the
        // command may have stopped on conflicts
        let to_local_changes = (result.is_ok() && task.after == AfterTask::LocalChanges)
            || read_operation(self.repository.git_dir()).is_some();
        // the command may have changed files or stopped on conflicts, even when it failed;
        // refreshing would interrupt typing, so then it waits for the refs to be checked once
        // typing stopped, see `check_refs`
        if let Some(mut context) = self.view.refresh_context().filter(|_| !typing) {
            // the rewritten commit stays selected
            let select = match task.after {
                AfterTask::SelectBehindHead(n) if result.is_ok() => Some(format!("HEAD~{n}")),
                _ => None,
            };
            if let Some(hash) = select.and_then(|revision| self.repository.resolve(&revision)) {
                context.list_context_mut().select = Reselect::Commit(hash);
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
                self.blur_sidebar();
            }
            self.ec.send(AppEvent::Refresh(context));
        }
        match result {
            Ok(_) => self.ec.send(AppEvent::NotifySuccess(task.done)),
            Err(err) => self.ec.send(AppEvent::NotifyError(err)),
        }
        quit
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
        let Some(state) = self.view.as_list_state() else {
            return;
        };
        let selected = state.selected_commit_hash().clone();
        let picked = state.picked_commit_hashes().to_vec();
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
                RefSelection::Ref(NamedRef::Branch(name)) => Some(name),
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
            let selection = self.shell.sidebar.selection();
            // there are some, see `ref_command_applies`
            let Some(refs) = diff_refs(&selection, self.shell.sidebar.picked()) else {
                return;
            };
            match self.sidebar_compare(&refs) {
                Ok(compare) => self.request_ref_compare(compare),
                Err(err) => self.ec.send(AppEvent::NotifyError(err)),
            }
            return;
        }
        // a branch, remote branch or tag, see `ref_command_applies`; or the row of a detached
        // HEAD, its commit
        let selected = match self.shell.sidebar.selection() {
            RefSelection::Ref(named) => named,
            RefSelection::DetachedHead => {
                if let Head::Detached { target } = self.repository.head() {
                    let command = graph_command(self.repository, event, target.clone(), vec![]);
                    self.run_graph_command(event, command);
                }
                return;
            }
            _ => return,
        };
        let target = self
            .repository
            .all_refs()
            .into_iter()
            .find(|r| r.named().as_ref() == Some(&selected))
            .map(|r| r.target().clone());
        let Some(target) = target else {
            return;
        };
        let command = ref_graph_command(self.repository, event, &selected, target);
        self.run_graph_command(event, command);
    }

    // Moves the border beside the focused panel with ctrl-left and ctrl-right (the sidebar, or the
    // files beside a diff), the one above or under it with ctrl-up and ctrl-down.
    fn move_border(&mut self, direction: Direction) {
        let (size, step) = match direction {
            Direction::Left => (Size::Width, -2),
            Direction::Right => (Size::Width, 2),
            Direction::Up => (Size::Height, -1),
            Direction::Down => (Size::Height, 1),
        };
        let sidebar =
            (!self.shell.sidebar_collapsed() && size == Size::Width).then_some(Panel::Sidebar);
        // the window over everything, else the sidebar or the page with the focus
        let panel = match &self.shell.overlay {
            Some(Overlay::File(window)) => window.resizable_panel(size),
            _ if self.shell.sidebar_focused() => sidebar,
            _ => self.view.resizable_panel(size).or(sidebar),
        };
        if let Some(panel) = panel {
            self.ctx.layout.borrow_mut().move_border(panel, step);
        }
    }

    // Moves the focus to the panel that way: the sidebar on the left of the page, the page on its
    // right, else between the panels of the page.
    fn move_focus(&mut self, direction: Direction) {
        if let Some(Overlay::File(window)) = self.shell.overlay.as_mut() {
            window.move_focus(direction);
        } else if self.shell.sidebar_focused() {
            if direction == Direction::Right {
                self.blur_sidebar();
            }
        } else if !self.view.move_focus(direction) && direction == Direction::Left {
            self.focus_sidebar();
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
    fn sidebar_compare(&self, refs: &[NamedRef]) -> Result<Compare, String> {
        let head = || {
            self.repository
                .current_branch()
                .map_or(Rev::Head, |branch| Rev::Ref(NamedRef::Branch(branch)))
        };
        let (base, target) = match refs {
            [one] => (head(), Rev::Ref(one.clone())),
            [one, two] => (Rev::Ref(one.clone()), Rev::Ref(two.clone())),
            more => return Err(format!("Diff compares two refs, {} are picked", more.len())),
        };
        if base == target {
            return Err(format!(
                "{} is checked out, there is nothing to compare",
                base.shown()
            ));
        }
        Ok(Compare { base, target })
    }

    // Diffs refs from the sidebar: the graph is loaded again with only their history, showing
    // their comparison, see `end_stale_ref_compare`.
    fn request_ref_compare(&mut self, compare: Compare) {
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
        let shown = compare.clone();
        self.shell.graph_filter = GraphFilter::Compare {
            refs: compare,
            before,
        };
        self.shell.sidebar.clear_picks();
        self.blur_sidebar();
        self.ec.send(AppEvent::Refresh(RefreshViewContext::Detail {
            list_context: context.list_context().clone(),
            tab: DetailTab::Changes,
            compare: Some(shown),
            // the changes of the comparison have it, like one opened from the graph
            focus: DetailFocus::Files,
        }));
    }

    fn run_graph_command(
        &mut self,
        event: UserEvent,
        command: Option<Result<GraphCommand, String>>,
    ) {
        match command {
            None => {}
            Some(Ok(GraphCommand::Dialog(dialog))) => self.open_dialog(Ok(*dialog)),
            Some(Ok(GraphCommand::AmendHead)) => self.amend_on_local_changes(),
            Some(Ok(GraphCommand::Compare(compare))) => self.open_compare(compare),
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

    // Reloads everything when the refs, HEAD or the operation in progress changed since they were
    // loaded, from outside the app, like lazygit.
    fn check_refs(&mut self, snapshot: String) {
        let previous = self.shell.refs_snapshot.replace(snapshot);
        // the reload running sees what changed, or the check after it does, as it takes the
        // refs as it loaded them
        if self.shell.reload.is_some() {
            return;
        }
        if previous.is_some_and(|p| Some(&p) != self.shell.refs_snapshot.as_ref()) {
            self.view.refresh();
        }
    }

    // Lays the graph out again on another thread with the merges folded now, the app going on
    // meanwhile; once it lands, the app is made again on it, see `AppEvent::Regraphed`.
    fn start_regraph(&mut self) {
        self.shell.regraphs.push(Instant::now());
        let repository = Arc::clone(self.repository);
        let primary_branch = self.load_options.primary_branch.clone();
        let folded = self.shell.folded_merges.clone();
        let tx = self.ec.sender();
        std::thread::spawn(move || {
            let spinning = spin(tx.clone());
            let layout = GraphLayout::calc(&repository, primary_branch.as_deref(), &folded);
            drop(spinning);
            // the app may have quit meanwhile
            let layout = Box::new(layout);
            tx.try_send(AppEvent::Regraphed {
                from: repository,
                layout,
            });
        });
    }

    // Loads the repository again on another thread, the app going on meanwhile; once it lands, the
    // app is made again on it showing what `restore` says. Asked again meanwhile, it loads again,
    // keeping where a command asked to go.
    fn start_reload(&mut self, restore: Restore) {
        let started = self
            .shell
            .reload
            .as_ref()
            .map_or_else(Instant::now, |p| p.started);
        let restore = match (self.shell.reload.take(), restore) {
            (
                Some(PendingReload {
                    restore: given @ Restore::Given(_),
                    ..
                }),
                Restore::Current(_),
            ) => given,
            (_, restore) => restore,
        };
        self.shell.reload_generation += 1;
        let generation = self.shell.reload_generation;
        self.shell.reload = Some(PendingReload {
            started,
            generation,
            restore,
        });
        let options = self.load_options.clone();
        let revisions = self.shell.graph_revisions();
        let running = self.ctx.running_task.clone();
        let folded = self.shell.folded_merges.clone();
        let tx = self.ec.sender();
        std::thread::spawn(move || {
            let spinning = spin(tx.clone());
            let result = options.load(revisions.as_deref(), running).map(|loaded| {
                let repository = match &loaded {
                    Loaded::Repository(repository) | Loaded::RefsGone(repository) => repository,
                };
                let primary_branch = options.primary_branch.as_deref();
                let layout = GraphLayout::calc(repository, primary_branch, &folded);
                Box::new(Reloaded { loaded, layout })
            });
            drop(spinning);
            // the app may have quit meanwhile
            tx.try_send(AppEvent::Reloaded { generation, result });
        });
    }

    // The app to make again on the repository reloaded, see `start_reload`; None for the result of
    // a reload asked for before the latest, or one failing, which keeps the repository as it was.
    fn land_reload(
        &mut self,
        generation: u64,
        result: std::result::Result<Box<Reloaded>, String>,
    ) -> Option<Ret> {
        let pending = self
            .shell
            .reload
            .take_if(|pending| pending.generation == generation)?;
        let Reloaded { loaded, layout } = match result {
            Ok(reloaded) => *reloaded,
            Err(err) => {
                self.shell.notify_error(err);
                // the refs as they are now, else the change that made it fail would reload again
                // and again
                let repository = self.repository;
                self.shell.refs_snapshot =
                    refs_snapshot(repository.path(), repository.git_dir()).ok();
                return None;
            }
        };
        let repository = match loaded {
            Loaded::Repository(repository) => repository,
            // a ref the graph showed is gone, all of them are shown again
            Loaded::RefsGone(repository) => {
                self.shell.reset_graph_filter();
                repository
            }
        };
        let context = match pending.restore {
            Restore::Current(asked) => self.view.refresh_context().unwrap_or(*asked),
            Restore::Given(context) => *context,
        };
        // drawn over the last frame, like after folding a merge
        let images = self
            .take_list_state()
            .map(CommitListState::into_graph_images);
        Some(Ret::Reload {
            context: Box::new(context),
            repository: Box::new(repository),
            layout: Box::new(layout),
            images,
        })
    }

    // Whether a reload runs in the background, see `start_reload`.
    #[cfg(test)]
    pub fn is_reloading(&self) -> bool {
        self.shell.reload.is_some()
    }

    // Reloads the local changes, made outside the app too: the page showing them, else their
    // count in the page bar.
    fn reload_files(&mut self, files: FilesSnapshot) {
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
        self.blur_sidebar();
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
            .resolve(&compare.base.git_name())
            .zip(self.repository.resolve(&compare.target.git_name()));
        let Some((base, target)) = resolved else {
            // gone meanwhile, like a ref deleted
            self.ec.send(AppEvent::NotifyError(format!(
                "Cannot diff {} and {}, one of them is gone",
                compare.base.shown(),
                compare.target.shown()
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

    fn init_with_context(&mut self, context: RefreshViewContext) {
        if let View::List(ref mut view) = self.view {
            view.reset_commit_list_with(context.list_context());
        }
        match context {
            RefreshViewContext::List { .. } => {}
            RefreshViewContext::Detail {
                compare: Some(compare),
                focus,
                ..
            } => {
                self.open_compare(compare);
                self.restore_details_focus(focus);
            }
            RefreshViewContext::Detail { tab, focus, .. } => {
                self.open_detail(tab);
                self.restore_details_focus(focus);
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

    // Puts the focus of the details back where it was before the page was reloaded.
    fn restore_details_focus(&mut self, focus: DetailFocus) {
        if let View::Detail(view) = &mut self.view {
            view.restore_focus(focus);
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
    fn open_link(&mut self, target: &LinkTarget, action: LinkAction) {
        let services = &self.ctx.core_config.browser.services;
        match browse::links(self.repository, services, target) {
            Ok(mut links) if action == LinkAction::Copy && links.len() == 1 => {
                self.open_url(links.remove(0).url, action)
            }
            Ok(links) => self.open_dialog(Ok(GitDialog::open_link(links, action))),
            Err(err) => self.ec.send(AppEvent::NotifyError(err)),
        }
    }

    // Opens each of `targets` in the browser, or copies all their links, one per line.
    fn handle_links(&mut self, targets: &[LinkTarget], action: LinkAction) {
        let services = &self.ctx.core_config.browser.services;
        if action == LinkAction::Open {
            // one dialog per target would replace the one before it, so several files open their
            // default link (the first remote's page) right away; one file still chooses the remote
            // and page in a dialog
            for target in targets {
                match browse::links(self.repository, services, target) {
                    Ok(links) => {
                        if let Some(link) = links.into_iter().next() {
                            self.open_url(link.url, action);
                        }
                    }
                    Err(err) => {
                        self.ec.send(AppEvent::NotifyError(err));
                        return;
                    }
                }
            }
            return;
        }
        let mut urls = Vec::new();
        for target in targets {
            match browse::links(self.repository, services, target) {
                Ok(links) => urls.extend(links.into_iter().map(|link| link.url)),
                Err(err) => {
                    self.ec.send(AppEvent::NotifyError(err));
                    return;
                }
            }
        }
        if !urls.is_empty() {
            self.ec.send(AppEvent::copy("Links", urls.join("\n")));
        }
    }

    // Opens `url` in the browser, or copies it.
    fn open_url(&self, url: String, action: LinkAction) {
        let event = match action {
            LinkAction::Copy => AppEvent::copy("Link", url),
            LinkAction::Open => match open::that_detached(&url) {
                Ok(()) => AppEvent::NotifySuccess(format!("Opened {url}")),
                Err(err) => AppEvent::NotifyError(format!("Failed to open {url}: {err}")),
            },
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

// The typed `message`, with a virtual cursor inserted at `cursor_byte_index` rather than drawn over
// a character.
fn status_input_line<'a>(
    message: &'a str,
    cursor_byte_index: usize,
    cursor_type: &'a CursorType,
    input_style: Style,
    cursor_style: Style,
) -> Line<'a> {
    match cursor_type {
        CursorType::Native => Line::from(Span::styled(message, input_style)),
        CursorType::Virtual(cursor) => {
            let (before, after) = message.split_at(cursor_byte_index);
            Line::from(vec![
                Span::styled(before, input_style),
                Span::styled(cursor.as_str(), cursor_style),
                Span::styled(after, input_style),
            ])
        }
    }
}

// The spans on the left, then those on the right, aligned to the right edge when they fit.
fn status_bar_line<'a>(mut left: Vec<Span<'a>>, right: Vec<Span<'a>>, width: u16) -> Line<'a> {
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
    GitTask::new(
        GitCommand::Fetch,
        &["--all"],
        "Fetching all remotes".into(),
        "Fetched all remotes".into(),
    )
}

// Redraws the spinner in the status bar on each tick, until the sender returned is dropped.
fn spin(tx: Sender) -> std::sync::mpsc::Sender<()> {
    let (spinning, ticking) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        while ticking.recv_timeout(SPINNER_RATE) == Err(RecvTimeoutError::Timeout) {
            // the app may have quit meanwhile
            if !tx.try_send(AppEvent::SpinnerTick) {
                break;
            }
        }
    });
    spinning
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
    fn test_fullscreen_repaint_writes_over_what_was_not_drawn(#[case] frames_before: usize) {
        let mut terminal = Terminal::new(TestBackend::new(5, 1)).unwrap();
        let draw = |terminal: &mut Terminal<TestBackend>| {
            terminal
                .draw(|frame| frame.render_widget(Paragraph::new("he lo"), frame.area()))
                .unwrap();
        };
        for _ in 0..frames_before {
            draw(&mut terminal);
        }
        // printed on the screen by something else where the frame has a blank, which drawing the same
        // frame leaves
        let junk = ratatui::buffer::Cell::new("x");
        terminal
            .backend_mut()
            .draw([(2, 0, &junk)].into_iter())
            .unwrap();

        repaint_fullscreen(&mut terminal);
        // not blank meanwhile
        assert_eq!(terminal.backend().buffer(), &Buffer::with_lines(["hexlo"]));
        for _ in 0..2 {
            draw(&mut terminal);
            assert_eq!(terminal.backend().buffer(), &Buffer::with_lines(["he lo"]));
        }
    }

    #[test]
    fn test_shorten_path() {
        assert_eq!(shorten_path("~/projects/spoon"), "~/p/spoon");
        assert_eq!(shorten_path("/tmp/.cache/dr"), "/t/.c/dr");
        assert_eq!(shorten_path("~"), "~");
    }

    #[rstest]
    #[case("/", 1, "|", "/|")]
    #[case("/abc", 1, "|", "/|abc")]
    #[case("/abc", 2, "|", "/a|bc")]
    #[case("/abc", 4, "|", "/abc|")]
    #[case("/a界b", 2, "|", "/a|界b")]
    #[case("/a界b", 5, "|", "/a界|b")]
    #[case("/ae\u{301}b", 5, "|", "/ae\u{301}|b")]
    #[case("/a👩‍💻b", 13, "|", "/a👩‍💻|b")]
    #[case("/abc", 2, "||", "/a||bc")]
    #[case("/abc", 2, "界", "/a界bc")]
    #[case("/abc", 2, "", "/abc")]
    fn test_status_input_line_inserts_virtual_cursor_without_hiding_text(
        #[case] message: &str,
        #[case] cursor_byte_index: usize,
        #[case] cursor: &str,
        #[case] expected: &str,
    ) {
        let cursor_type = CursorType::Virtual(cursor.into());
        let mut terminal = Terminal::new(TestBackend::new(20, 1)).unwrap();
        terminal
            .draw(|frame| {
                let line = status_input_line(
                    message,
                    cursor_byte_index,
                    &cursor_type,
                    Style::default(),
                    Style::default(),
                );
                frame.render_widget(Paragraph::new(line), frame.area());
            })
            .unwrap();

        let mut expected_buffer = Buffer::with_lines([expected]);
        expected_buffer.resize(Rect::new(0, 0, 20, 1));
        assert_eq!(terminal.backend().buffer(), &expected_buffer);
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
