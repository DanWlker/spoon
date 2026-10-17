use std::{
    fmt::{self, Debug, Formatter},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
};

use ratatui::crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use serde::{
    de::{self, Deserializer, Visitor},
    Deserialize,
};

use crate::{
    browse::LinkTarget,
    git::{FilesSnapshot, GitTask},
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
    // the fetch of all remotes on launch finished, see `App::fetch_on_launch`
    LaunchFetched(Result<(), String>),
    OpenHelp,
    CloseHelp,
    SelectNewerCommit,
    SelectOlderCommit,
    CopyToClipboard {
        name: String,
        value: String,
    },
    // opens the link in the browser, or copies it
    OpenUrl {
        url: String,
        copy: bool,
    },
    // opens the page of the target on the web host of its remote, or copies its link
    Link {
        target: LinkTarget,
        copy: bool,
    },
    Refresh(RefreshViewContext),
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
        };
        controller.start();

        controller
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

// A hint for one event.
pub fn hint(event: UserEvent, description: &str) -> KeyHint {
    (vec![event], description.to_string())
}

// The event triggered by user's key input
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UserEvent {
    ForceQuit,
    Quit,
    HelpToggle,
    Cancel,
    Close,
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
    SearchTargetToggle,
    SmartCaseToggle,
    FuzzyToggle,
    Refresh,
    ShortCopy,
    FullCopy,
    Ignore,
    CommandLog,
    Unknown,
}

impl<'de> Deserialize<'de> for UserEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct UserEventVisitor;

        impl<'de> Visitor<'de> for UserEventVisitor {
            type Value = UserEvent;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a string representing a user event")
            }

            fn visit_str<E>(self, value: &str) -> Result<UserEvent, E>
            where
                E: de::Error,
            {
                if let Some(num) = value.strip_prefix("focus_panel_") {
                    match num.parse::<usize>() {
                        Ok(n) => Ok(UserEvent::FocusPanel(n)),
                        Err(_) => {
                            let msg = format!("Invalid focus_panel_n format: {value}");
                            Err(de::Error::custom(msg))
                        }
                    }
                } else {
                    match value {
                        "force_quit" => Ok(UserEvent::ForceQuit),
                        "quit" => Ok(UserEvent::Quit),
                        "help_toggle" => Ok(UserEvent::HelpToggle),
                        "cancel" => Ok(UserEvent::Cancel),
                        "close" => Ok(UserEvent::Close),
                        "navigate_up" => Ok(UserEvent::NavigateUp),
                        "navigate_down" => Ok(UserEvent::NavigateDown),
                        "navigate_right" => Ok(UserEvent::NavigateRight),
                        "navigate_left" => Ok(UserEvent::NavigateLeft),
                        "select_up" => Ok(UserEvent::SelectUp),
                        "select_down" => Ok(UserEvent::SelectDown),
                        "go_to_top" => Ok(UserEvent::GoToTop),
                        "go_to_bottom" => Ok(UserEvent::GoToBottom),
                        "scroll_up" => Ok(UserEvent::ScrollUp),
                        "scroll_down" => Ok(UserEvent::ScrollDown),
                        "page_up" => Ok(UserEvent::PageUp),
                        "page_down" => Ok(UserEvent::PageDown),
                        "half_page_up" => Ok(UserEvent::HalfPageUp),
                        "half_page_down" => Ok(UserEvent::HalfPageDown),
                        "go_to_next" => Ok(UserEvent::GoToNext),
                        "go_to_previous" => Ok(UserEvent::GoToPrevious),
                        "confirm" => Ok(UserEvent::Confirm),
                        "ref_list" => Ok(UserEvent::RefList),
                        "search" => Ok(UserEvent::Search),
                        "filter" => Ok(UserEvent::Filter),
                        "search_target_toggle" => Ok(UserEvent::SearchTargetToggle),
                        "smart_case_toggle" => Ok(UserEvent::SmartCaseToggle),
                        "fuzzy_toggle" => Ok(UserEvent::FuzzyToggle),
                        "refresh" => Ok(UserEvent::Refresh),
                        "resize_left" => Ok(UserEvent::ResizeLeft),
                        "resize_right" => Ok(UserEvent::ResizeRight),
                        "resize_up" => Ok(UserEvent::ResizeUp),
                        "resize_down" => Ok(UserEvent::ResizeDown),
                        "focus_left" => Ok(UserEvent::FocusLeft),
                        "focus_right" => Ok(UserEvent::FocusRight),
                        "focus_up" => Ok(UserEvent::FocusUp),
                        "focus_down" => Ok(UserEvent::FocusDown),
                        "keys_toggle" => Ok(UserEvent::KeysToggle),
                        "short_copy" => Ok(UserEvent::ShortCopy),
                        "full_copy" => Ok(UserEvent::FullCopy),
                        "ignore" => Ok(UserEvent::Ignore),
                        "command_log" => Ok(UserEvent::CommandLog),
                        "toggle_stage" => Ok(UserEvent::ToggleStage),
                        "toggle_select" => Ok(UserEvent::ToggleSelect),
                        "next_picked" => Ok(UserEvent::NextPicked),
                        "next_file" => Ok(UserEvent::NextFile),
                        "previous_file" => Ok(UserEvent::PreviousFile),
                        "next_tab" => Ok(UserEvent::NextTab),
                        "previous_tab" => Ok(UserEvent::PreviousTab),
                        "previous_picked" => Ok(UserEvent::PreviousPicked),
                        "stash" => Ok(UserEvent::Stash),
                        "commit" => Ok(UserEvent::Commit),
                        "edit" => Ok(UserEvent::Edit),
                        "abort" => Ok(UserEvent::Abort),
                        "toggle_amend" => Ok(UserEvent::ToggleAmend),
                        "discard" => Ok(UserEvent::Discard),
                        "dialog_toggle" => Ok(UserEvent::DialogToggle),
                        "dialog_edit" => Ok(UserEvent::DialogEdit),
                        "dialog_activate" => Ok(UserEvent::DialogActivate),
                        "fetch" => Ok(UserEvent::Fetch),
                        "fetch_all" => Ok(UserEvent::FetchAll),
                        "pull" => Ok(UserEvent::Pull),
                        "push" => Ok(UserEvent::Push),
                        "diff" => Ok(UserEvent::Diff),
                        "rebase" => Ok(UserEvent::Rebase),
                        "merge" => Ok(UserEvent::Merge),
                        "checkout" => Ok(UserEvent::Checkout),
                        "cherry_pick" => Ok(UserEvent::CherryPick),
                        "squash" => Ok(UserEvent::Squash),
                        "revert" => Ok(UserEvent::Revert),
                        "reset" => Ok(UserEvent::Reset),
                        "amend_commit" => Ok(UserEvent::AmendCommit),
                        "drop_commit" => Ok(UserEvent::DropCommit),
                        "create_tag" => Ok(UserEvent::CreateTag),
                        "create_branch" => Ok(UserEvent::CreateBranch),
                        "branch_only" => Ok(UserEvent::BranchOnly),
                        "create_ref" => Ok(UserEvent::CreateRef),
                        "delete_ref" => Ok(UserEvent::DeleteRef),
                        "edit_ref" => Ok(UserEvent::EditRef),
                        "apply_stash" => Ok(UserEvent::ApplyStash),
                        "open_in_browser" => Ok(UserEvent::OpenInBrowser),
                        "dismiss_notifications" => Ok(UserEvent::DismissNotifications),
                        "toggle_wrap" => Ok(UserEvent::ToggleWrap),
                        "copy_link" => Ok(UserEvent::CopyLink),
                        "next_section" => Ok(UserEvent::NextSection),
                        "previous_section" => Ok(UserEvent::PreviousSection),
                        "apply_change" => Ok(UserEvent::ApplyChange),
                        _ => {
                            let msg = format!("Unknown user event: {value}");
                            Err(de::Error::custom(msg))
                        }
                    }
                }
            }
        }

        deserializer.deserialize_str(UserEventVisitor)
    }
}

impl UserEvent {
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
            UserEvent::NextTab | UserEvent::PreviousTab | UserEvent::ApplyChange => Scope::Details,
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

    // The events of the whole app, which work wherever the focus is: quitting, help, refreshing,
    // switching pages and panels, and the remotes.
    pub fn is_app_wide(&self) -> bool {
        matches!(
            self,
            UserEvent::ForceQuit
                | UserEvent::Quit
                | UserEvent::HelpToggle
                | UserEvent::Refresh
                | UserEvent::RefList
                | UserEvent::FocusPanel(_)
                | UserEvent::ResizeLeft
                | UserEvent::ResizeRight
                | UserEvent::ResizeUp
                | UserEvent::ResizeDown
                | UserEvent::FocusLeft
                | UserEvent::FocusRight
                | UserEvent::FocusUp
                | UserEvent::FocusDown
                | UserEvent::KeysToggle
                | UserEvent::CommandLog
                | UserEvent::DismissNotifications
                | UserEvent::ToggleWrap
                | UserEvent::Fetch
                | UserEvent::FetchAll
                | UserEvent::Pull
                | UserEvent::Push
        )
    }
}
