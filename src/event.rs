use std::{
    fmt::{self, Debug, Formatter},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
};

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use serde::{
    de::{self, Deserializer, Visitor},
    Deserialize,
};

use crate::{
    git::GitTask, keybind::Scope, view::RefreshViewContext, widget::commit_tabs::DetailTab,
};

#[derive(Debug)]
pub enum AppEvent {
    Key(KeyEvent),
    // pressing, dragging and releasing the left button, to drag the borders of the panels
    Mouse(MouseEvent),
    Resize(usize, usize),
    Quit,
    // opens the details of the selected commit on a tab
    OpenDetail(DetailTab),
    CloseDetail,
    OpenUserCommand(usize),
    CloseUserCommand,
    // runs an interactive command, like an editor, in place of the app, then refreshes
    RunExternal(Vec<String>),
    // the working tree changed, the count of the local changes in the sidebar is updated
    LocalChangesChanged,
    // the local changes are reloaded every few seconds, for those made outside the app
    FilesTick,
    // where the local branches and HEAD point, every few seconds, see `git::refs_snapshot`
    RefsSnapshot(String),
    // the terminal got the focus back, everything is reloaded
    FocusGained,
    // a git command running in the background finished
    TaskFinished {
        task: GitTask,
        result: Result<(), String>,
    },
    OpenHelp,
    CloseHelp,
    SelectNewerCommit,
    SelectOlderCommit,
    CopyToClipboard {
        name: String,
        value: String,
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

#[derive(Clone)]
pub struct Sender {
    tx: mpsc::Sender<AppEvent>,
}

impl Sender {
    pub fn send(&self, event: AppEvent) {
        self.tx.send(event).unwrap();
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
                            // the wheel moves like the arrow keys, as the terminal does without
                            // the mouse captured
                            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                                let code = if mouse.kind == MouseEventKind::ScrollDown {
                                    KeyCode::Down
                                } else {
                                    KeyCode::Up
                                };
                                tx.send(AppEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)));
                            }
                            MouseEventKind::Down(MouseButton::Left)
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

    // Checks for changes made outside the app like lazygit's refresher: the branches and HEAD every
    // `refs_interval`, the local changes every `files_interval` (a multiple of it).
    pub fn start_refresher(
        &self,
        path: std::path::PathBuf,
        refs_interval: std::time::Duration,
        files_interval: std::time::Duration,
    ) {
        let tx = self.tx.clone();
        let files_every = (files_interval.as_millis() / refs_interval.as_millis()).max(1);
        thread::spawn(move || {
            // stops once the app quits and the events are no longer received
            let mut tick = 0u128;
            loop {
                thread::sleep(refs_interval);
                tick += 1;
                if let Ok(snapshot) = crate::git::refs_snapshot(&path) {
                    if tx.tx.send(AppEvent::RefsSnapshot(snapshot)).is_err() {
                        break;
                    }
                }
                if tick.is_multiple_of(files_every) && tx.tx.send(AppEvent::FilesTick).is_err() {
                    break;
                }
            }
        });
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
    UserCommand(usize),
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
    BranchOnly,
    CreateRef,
    DeleteRef,
    EditRef,
    ApplyStash,
    ApplyChange,
    SearchTargetToggle,
    IgnoreCaseToggle,
    FuzzyToggle,
    Refresh,
    ShortCopy,
    FullCopy,
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
                if value.starts_with("user_command_") {
                    if let Some(num) = parse_user_command_number(value) {
                        Ok(UserEvent::UserCommand(num))
                    } else {
                        let msg = format!("Invalid user_command_n format: {value}",);
                        Err(de::Error::custom(msg))
                    }
                } else if let Some(num) = value.strip_prefix("focus_panel_") {
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
                        "ref_list" | "ref_list_toggle" => Ok(UserEvent::RefList),
                        "search" => Ok(UserEvent::Search),
                        "filter" => Ok(UserEvent::Filter),
                        "search_target_toggle" => Ok(UserEvent::SearchTargetToggle),
                        "ignore_case_toggle" => Ok(UserEvent::IgnoreCaseToggle),
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
                        "toggle_stage" => Ok(UserEvent::ToggleStage),
                        "toggle_select" => Ok(UserEvent::ToggleSelect),
                        "next_picked" => Ok(UserEvent::NextPicked),
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
                        "branch_only" => Ok(UserEvent::BranchOnly),
                        "create_ref" => Ok(UserEvent::CreateRef),
                        "delete_ref" => Ok(UserEvent::DeleteRef),
                        "edit_ref" => Ok(UserEvent::EditRef),
                        "apply_stash" => Ok(UserEvent::ApplyStash),
                        "apply_change" => Ok(UserEvent::ApplyChange),
                        // renamed
                        "mark_toggle" => Ok(UserEvent::ToggleSelect),
                        // removed, ignored so configs naming them still load
                        "go_to_parent" | "select_top" | "select_middle" | "select_bottom" => {
                            Ok(UserEvent::Unknown)
                        }
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

fn parse_user_command_number(s: &str) -> Option<usize> {
    if let Some(num_str) = s.strip_prefix("user_command_") {
        if num_str.parse::<usize>().is_ok() {
            return num_str.parse::<usize>().ok();
        }
        if let Some(num_str) = s.strip_prefix("user_command_view_toggle_") {
            if num_str.parse::<usize>().is_ok() {
                return num_str.parse::<usize>().ok();
            }
        }
    }
    None
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
            | UserEvent::BranchOnly => Scope::Graph,
            UserEvent::CreateRef
            | UserEvent::DeleteRef
            | UserEvent::EditRef
            | UserEvent::ApplyStash => Scope::Sidebar,
            UserEvent::NextTab | UserEvent::PreviousTab | UserEvent::ApplyChange => Scope::Details,
            UserEvent::ToggleStage
            | UserEvent::Stash
            | UserEvent::Commit
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
    // switching pages and panels, the remotes and the user commands.
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
                | UserEvent::Fetch
                | UserEvent::FetchAll
                | UserEvent::Pull
                | UserEvent::Push
                | UserEvent::UserCommand(_)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserEventWithCount {
    pub event: UserEvent,
    pub count: usize,
}

impl UserEventWithCount {
    pub fn new(event: UserEvent, count: usize) -> Self {
        Self {
            event,
            count: if count == 0 { 1 } else { count },
        }
    }

    pub fn from_event(event: UserEvent) -> Self {
        Self::new(event, 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_event_with_count_new() {
        let event = UserEventWithCount::new(UserEvent::NavigateUp, 5);
        assert_eq!(event.event, UserEvent::NavigateUp);
        assert_eq!(event.count, 5);
    }

    #[test]
    fn test_user_event_with_count_new_zero_count() {
        let event = UserEventWithCount::new(UserEvent::NavigateDown, 0);
        assert_eq!(event.event, UserEvent::NavigateDown);
        assert_eq!(event.count, 1); // zero should be converted to 1
    }

    #[test]
    fn test_user_event_with_count_from_event() {
        let event = UserEventWithCount::from_event(UserEvent::NavigateLeft);
        assert_eq!(event.event, UserEvent::NavigateLeft);
        assert_eq!(event.count, 1);
    }

    #[test]
    fn test_user_event_with_count_equality() {
        let event1 = UserEventWithCount::new(UserEvent::ScrollUp, 3);
        let event2 = UserEventWithCount::new(UserEvent::ScrollUp, 3);
        let event3 = UserEventWithCount::new(UserEvent::ScrollDown, 3);

        assert_eq!(event1, event2);
        assert_ne!(event1, event3);
    }
}
