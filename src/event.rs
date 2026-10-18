use std::{
    fmt::{self, Debug, Formatter},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
};

use ratatui::crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use serde::Deserialize;

use crate::{
    browse::{LinkAction, LinkTarget},
    git::{CommitHash, FilesSnapshot, GitTask},
    graph::Fold,
    keybind::Scope,
    view::RefreshViewContext,
    widget::commit_tabs::DetailTab,
};

#[derive(Debug)]
pub enum AppEvent {
    // a popup confirming a git command, from a page, like a commit
    OpenDialog(Box<crate::widget::git_dialog::GitDialog>),
    Key(KeyEvent),
    // pressing, dragging and releasing the left button, to drag the borders of the panels, and the
    // wheel, see `App::scroll_wheel`
    Mouse(MouseEvent),
    Resize(usize, usize),
    Quit,
    // opens the details of the selected commit on a tab
    OpenDetail(DetailTab),
    CloseDetail,
    // runs an interactive command, like an editor, in place of the app, then refreshes
    RunExternal(Vec<String>),
    // the working tree changed, the count of the local changes in the sidebar is updated
    LocalChangesChanged,
    // the local changes, loaded every few seconds in the background, for those made outside the app
    FilesTick(FilesSnapshot),
    // where the local branches and HEAD point, every few seconds, see `git::refs_snapshot`
    RefsSnapshot(String),
    // the terminal got the focus back, what changed meanwhile is checked right away
    FocusGained,
    // redraws the spinner of the git command running in the background
    SpinnerTick,
    // runs a git command in the background, see `App::start_task`
    StartTask(GitTask),
    // a git command running in the background finished
    TaskFinished {
        task: GitTask,
        result: Result<(), String>,
    },
    OpenHelp,
    // closes the help or another window over everything
    CloseOverlay,
    SelectNewerCommit,
    SelectOlderCommit,
    CopyToClipboard {
        name: String,
        value: String,
    },
    // opens the link in the browser, or copies it
    OpenUrl {
        url: String,
        action: LinkAction,
    },
    // opens the page of the target on the web host of its remote, or copies its link
    Link {
        target: LinkTarget,
        action: LinkAction,
    },
    Refresh(RefreshViewContext),
    // the blame of the file at `path` as of the commit, on `line` (from 0) when one is selected
    OpenBlame {
        commit: CommitHash,
        path: String,
        line: Option<usize>,
    },
    // the commits changing the file at `path`, on the last one up to the commit
    OpenTimeline {
        commit: CommitHash,
        path: String,
    },
    // asks to check out the commit, see `UserEvent::Checkout`
    Checkout(CommitHash),
    // selects the commit on the graph
    ShowCommit(CommitHash),
    // folds or unfolds the merge commit, the graph drawn again
    FoldMerge {
        commit: CommitHash,
        fold: Fold,
    },
    ClearStatusLine,
    UpdateStatusInput {
        message: String,
        cursor_position: u16,
        metadata: String,
    },
    UpdateStatusTransient(String),
    UpdateSearchResult {
        message: String,
        options: String,
        matched: bool,
    },
    #[expect(dead_code)]
    NotifyInfo(String),
    NotifySuccess(String),
    NotifyWarn(String),
    NotifyError(String),
}

impl AppEvent {
    // Copies `value` to the clipboard, telling it as `name`, like "Commit SHA".
    pub fn copy(name: &str, value: impl Into<String>) -> Self {
        AppEvent::CopyToClipboard {
            name: name.into(),
            value: value.into(),
        }
    }
}

#[derive(Clone)]
pub struct Sender {
    tx: mpsc::Sender<AppEvent>,
}

impl Sender {
    pub fn send(&self, event: AppEvent) {
        self.tx.send(event).unwrap();
    }

    // Sends `event` unless the app quit; returns whether it did.
    pub fn try_send(&self, event: AppEvent) -> bool {
        self.tx.send(event).is_ok()
    }

    // A sender and the receiver of what it sends, without the app.
    #[cfg(test)]
    pub fn channel() -> (Self, mpsc::Receiver<AppEvent>) {
        let (tx, rx) = mpsc::channel();
        (Self { tx }, rx)
    }
}

impl Debug for Sender {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Sender")
    }
}

pub struct Receiver {
    rx: mpsc::Receiver<AppEvent>,
}

impl Receiver {
    fn recv(&self) -> AppEvent {
        self.rx.recv().unwrap()
    }

    fn try_recv(&self) -> Option<AppEvent> {
        self.rx.try_recv().ok()
    }

    fn recv_timeout(&self, timeout: std::time::Duration) -> Option<AppEvent> {
        match self.rx.recv_timeout(timeout) {
            Ok(event) => Some(event),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => panic!("the event channel closed"),
        }
    }
}

impl Debug for Receiver {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Receiver")
    }
}

#[derive(Debug)]
pub struct EventController {
    tx: Sender,
    rx: Receiver,
    stop: Arc<AtomicBool>,
    handle: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
    // wakes the refresher up, see `refresh_now`
    poke: Mutex<Option<mpsc::Sender<()>>>,
    // the terminal is read and given to the commands run in it, but in tests
    terminal: bool,
}

impl EventController {
    pub fn init() -> Self {
        let (tx, rx) = mpsc::channel();
        let tx = Sender { tx };
        let rx = Receiver { rx };

        let controller = EventController {
            tx: tx.clone(),
            rx,
            stop: Arc::new(AtomicBool::new(false)),
            handle: Arc::new(Mutex::new(None)),
            poke: Mutex::new(None),
            terminal: true,
        };
        controller.start();

        controller
    }

    // Without reading the terminal, the tests sending the keys themselves, see `tests/app.rs`.
    #[cfg(test)]
    pub fn without_terminal() -> Self {
        let (tx, rx) = mpsc::channel();
        EventController {
            tx: Sender { tx },
            rx: Receiver { rx },
            stop: Arc::new(AtomicBool::new(false)),
            handle: Arc::new(Mutex::new(None)),
            poke: Mutex::new(None),
            terminal: false,
        }
    }

    pub fn start(&self) {
        self.stop.store(false, Ordering::Relaxed);
        let stop = self.stop.clone();
        let tx = self.tx.clone();
        let handle = thread::spawn(move || loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            match ratatui::crossterm::event::poll(std::time::Duration::from_millis(100)) {
                Ok(true) => match ratatui::crossterm::event::read() {
                    Ok(e) => match e {
                        ratatui::crossterm::event::Event::Key(key) => {
                            tx.send(AppEvent::Key(key));
                        }
                        ratatui::crossterm::event::Event::Mouse(mouse) => match mouse.kind {
                            MouseEventKind::ScrollDown
                            | MouseEventKind::ScrollUp
                            | MouseEventKind::Down(MouseButton::Left)
                            | MouseEventKind::Drag(MouseButton::Left)
                            | MouseEventKind::Up(MouseButton::Left) => {
                                tx.send(AppEvent::Mouse(mouse));
                            }
                            _ => {}
                        },
                        ratatui::crossterm::event::Event::Resize(w, h) => {
                            tx.send(AppEvent::Resize(w as usize, h as usize));
                        }
                        ratatui::crossterm::event::Event::FocusGained => {
                            tx.send(AppEvent::FocusGained);
                        }
                        _ => {}
                    },
                    Err(e) => {
                        panic!("Failed to read event: {e}");
                    }
                },
                Ok(false) => {
                    continue;
                }
                Err(e) => {
                    panic!("Failed to poll event: {e}");
                }
            }
        });
        *self.handle.lock().unwrap() = Some(handle);
    }

    pub fn resume(&self) {
        if !self.terminal {
            return;
        }
        ratatui::crossterm::execute!(
            std::io::stdout(),
            ratatui::crossterm::terminal::EnterAlternateScreen,
            ratatui::crossterm::event::EnableFocusChange,
            ratatui::crossterm::event::EnableMouseCapture
        )
        .unwrap();
        ratatui::crossterm::terminal::enable_raw_mode().unwrap();

        self.drain_crossterm_event();
        self.start();
    }

    pub fn suspend(&self) {
        if !self.terminal {
            return;
        }
        self.stop();

        ratatui::crossterm::terminal::disable_raw_mode().unwrap();
        ratatui::crossterm::execute!(
            std::io::stdout(),
            ratatui::crossterm::event::DisableFocusChange,
            ratatui::crossterm::event::DisableMouseCapture,
            ratatui::crossterm::terminal::LeaveAlternateScreen
        )
        .unwrap();
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.lock().unwrap().take() {
            handle.join().unwrap();
        }
    }

    fn drain_crossterm_event(&self) {
        while let Ok(true) = ratatui::crossterm::event::poll(std::time::Duration::from_millis(0)) {
            let _ = ratatui::crossterm::event::read();
        }
    }

    // Wakes the refresher returned by `refresher::spawn` up for `refresh_now`.
    pub fn set_refresher(&self, poke: mpsc::Sender<()>) {
        *self.poke.lock().unwrap() = Some(poke);
    }

    // Checks right away, like when the terminal gets the focus back.
    pub fn refresh_now(&self) {
        if let Some(poke) = self.poke.lock().unwrap().as_ref() {
            let _ = poke.send(());
        }
    }

    pub fn sender(&self) -> Sender {
        self.tx.clone()
    }

    pub fn send(&self, event: AppEvent) {
        self.tx.send(event);
    }

    pub fn recv(&self) -> AppEvent {
        self.rx.recv()
    }

    // The next event if one is waiting.
    pub fn try_recv(&self) -> Option<AppEvent> {
        self.rx.try_recv()
    }

    // The next event, or None once `timeout` passed without one.
    pub fn recv_timeout(&self, timeout: std::time::Duration) -> Option<AppEvent> {
        self.rx.recv_timeout(timeout)
    }
}

// A hint in the status bar: the events of a key, and what it does.
pub type KeyHint = (Vec<UserEvent>, String);

// The event triggered by user's key input
// Named in the keybind config like `go_to_top`; `focus_panel_1` is parsed by `parse_user_event`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserEvent {
    ForceQuit,
    Quit,
    HelpToggle,
    Cancel,
    NavigateUp,
    NavigateDown,
    NavigateRight,
    NavigateLeft,
    SelectUp,
    SelectDown,
    GoToTop,
    GoToBottom,
    ScrollUp,
    ScrollDown,
    PageUp,
    PageDown,
    HalfPageUp,
    HalfPageDown,
    GoToNext,
    GoToPrevious,
    Confirm,
    RefList,
    Search,
    Filter,
    #[serde(skip)]
    FocusPanel(usize),
    ResizeLeft,
    ResizeRight,
    ResizeUp,
    ResizeDown,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    KeysToggle,
    ToggleStage,
    ToggleSelect,
    NextPicked,
    PreviousPicked,
    NextFile,
    PreviousFile,
    NextTab,
    PreviousTab,
    Stash,
    Commit,
    Abort,
    Edit,
    ToggleAmend,
    Discard,
    DialogToggle,
    DialogEdit,
    DialogActivate,
    Fetch,
    FetchAll,
    Pull,
    Push,
    Diff,
    Rebase,
    Merge,
    Checkout,
    CherryPick,
    Squash,
    Revert,
    Reset,
    AmendCommit,
    DropCommit,
    CreateTag,
    CreateBranch,
    BranchOnly,
    CreateRef,
    DeleteRef,
    EditRef,
    ApplyStash,
    DismissNotifications,
    ToggleWrap,
    OpenInBrowser,
    CopyLink,
    NextSection,
    PreviousSection,
    ApplyChange,
    ResetFile,
    SearchTargetToggle,
    SmartCaseToggle,
    FuzzyToggle,
    Refresh,
    ShortCopy,
    FullCopy,
    Ignore,
    CommandLog,
    Blame,
    Timeline,
    OlderCommit,
    NewerCommit,
    #[serde(skip)]
    Unknown,
}

impl UserEvent {
    // Whether its command moves or rewrites HEAD, refused while an operation waits to be finished;
    // the commands themselves decide, see `GitTask::moves_head`.
    pub fn moves_head(self) -> bool {
        use crate::git::GitCommand;
        // the git command each runs: squashing, dropping and amending an older commit rebase
        let command = match self {
            UserEvent::Rebase
            | UserEvent::Squash
            | UserEvent::DropCommit
            | UserEvent::AmendCommit => GitCommand::Rebase,
            UserEvent::Merge => GitCommand::Merge,
            UserEvent::Checkout => GitCommand::Checkout,
            UserEvent::CherryPick => GitCommand::CherryPick,
            UserEvent::Revert => GitCommand::Revert,
            UserEvent::Reset => GitCommand::Reset,
            UserEvent::Pull => GitCommand::Pull,
            _ => return false,
        };
        command.moves_head()
    }

    // The scope a key of the event is bound in, when the keybind doesn't say: the keys of most
    // events work everywhere, those of the commands of a part of the app in that part only, so
    // the same key can do something else elsewhere. See `Scope`.
    pub fn home_scope(&self) -> Scope {
        match self {
            UserEvent::Diff
            | UserEvent::Rebase
            | UserEvent::Merge
            | UserEvent::Checkout
            | UserEvent::CherryPick
            | UserEvent::Squash
            | UserEvent::Revert
            | UserEvent::Reset
            | UserEvent::AmendCommit
            | UserEvent::DropCommit
            | UserEvent::CreateTag
            | UserEvent::CreateBranch
            | UserEvent::BranchOnly => Scope::Graph,
            UserEvent::CreateRef
            | UserEvent::DeleteRef
            | UserEvent::EditRef
            | UserEvent::ApplyStash
            | UserEvent::NextSection
            | UserEvent::PreviousSection => Scope::Sidebar,
            UserEvent::NextTab
            | UserEvent::PreviousTab
            | UserEvent::ApplyChange
            | UserEvent::ResetFile => Scope::Details,
            UserEvent::OlderCommit | UserEvent::NewerCommit => Scope::FileHistory,
            UserEvent::ToggleStage
            | UserEvent::Stash
            | UserEvent::Commit
            | UserEvent::NextFile
            | UserEvent::PreviousFile
            | UserEvent::Abort
            | UserEvent::ToggleAmend
            | UserEvent::Edit
            | UserEvent::Discard => Scope::LocalChanges,
            UserEvent::DialogToggle | UserEvent::DialogEdit | UserEvent::DialogActivate => {
                Scope::Dialog
            }
            _ => Scope::Global,
        }
    }
}
