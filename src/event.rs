use std::{
    fmt::{self, Debug, Formatter},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
};

use ratatui::crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};

use crate::{
    browse::{LinkAction, LinkTarget},
    git::{CommitHash, FilesSnapshot, GitTask},
    graph::Fold,
    keybind::UserEvent,
    search::SearchOutcome,
    view::RefreshViewContext,
    widget::commit_tabs::DetailTab,
};

#[derive(Debug)]
pub enum AppEvent {
    // a popup confirming a git command, from a page, like a commit
    OpenDialog(Box<crate::widget::git_dialog::GitDialog>),
    Key(KeyEvent),
    // text pasted in the terminal, see `App::paste`
    Paste(String),
    // pressing, dragging and releasing the left button, to drag the borders of the panels, and the
    // wheel, see `App::handle_event`
    Mouse(MouseEvent),
    // only redraws
    Resize,
    Quit,
    // opens the details of the selected commit on a tab
    OpenDetail(DetailTab),
    CloseDetail,
    // the graph shows all the commits again after showing a branch's only, see `UserEvent::ActiveBranchOnly`
    ShowAllCommits,
    // runs an interactive command, like an editor, in place of the app, then refreshes
    RunExternal(Vec<String>),
    // the working tree changed, the count of the local changes in the sidebar is updated
    LocalChangesChanged,
    // the local changes, loaded every few seconds in the background, for those made outside the app
    FilesTick(FilesSnapshot),
    // where the refs and HEAD point and the operation in progress, every few seconds, see
    // `git::refs_snapshot`
    RefsSnapshot(String),
    // the terminal got the focus back, what changed meanwhile is checked right away
    FocusGained,
    // work for what is shown finished on another thread, drawn with the next frame, see `Job`
    Loaded,
    // redraws the spinner of the git command running in the background
    SpinnerTick,
    // runs a git command in the background, see `App::start_task`
    StartTask(GitTask),
    // the git command running in the background finished, see `AppContext::running_task`
    TaskFinished(Result<(), String>),
    OpenHelp,
    // runs the action chosen in the keys list, closing it, as if its key were pressed
    RunAction(UserEvent),
    // closes the help or another window over everything
    CloseOverlay,
    CopyToClipboard {
        name: String,
        value: String,
    },
    // opens the links in the browser, or copies them
    Links {
        targets: Vec<LinkTarget>,
        action: LinkAction,
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
    // reloads the repository in the background, showing the view as it is once loaded, see
    // `App::start_reload`
    Reload,
    // reloads it showing what a command asked for, like the commit it rewrote selected
    Refresh(RefreshViewContext),
    // the repository loaded again, see `App::start_reload`
    Reloaded {
        generation: u64,
        result: Result<Box<crate::app::Reloaded>, String>,
    },
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
    // the graph laid out again on another thread after a merge was folded or unfolded, see
    // `App::start_regraph`, of the repository `from`
    Regraphed {
        from: std::sync::Arc<crate::git::Repository>,
        layout: Box<crate::graph::GraphLayout>,
    },
    // folds or unfolds the merge commit, the graph drawn again
    FoldMerge {
        commit: CommitHash,
        fold: Fold,
    },
    ClearStatusLine,
    UpdateStatusInput {
        message: String,
        cursor_byte_index: usize,
        metadata: String,
    },
    UpdateStatusTransient(String),
    UpdateSearchResult {
        outcome: SearchOutcome,
        options: String,
    },
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
    // the pointer moving is read only while something shows where it is, see `set_hover`
    hover: Arc<AtomicBool>,
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
            tx,
            rx,
            stop: Arc::new(AtomicBool::new(false)),
            hover: Arc::new(AtomicBool::new(false)),
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
            hover: Arc::new(AtomicBool::new(false)),
            handle: Arc::new(Mutex::new(None)),
            poke: Mutex::new(None),
            terminal: false,
        }
    }

    // Reads the pointer moving, which redraws the app each time, only while `on`: while the menu
    // of a right click follows it.
    pub fn set_hover(&self, on: bool) {
        self.hover.store(on, Ordering::Relaxed);
    }

    pub fn start(&self) {
        self.stop.store(false, Ordering::Relaxed);
        let (stop, hover) = (self.stop.clone(), self.hover.clone());
        let tx = self.tx.clone();
        // without the keys the app can't go on: failing to read them panics, which stops it
        let handle = thread::spawn(move || read_events(&stop, &hover, &tx));
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
            ratatui::crossterm::event::EnableMouseCapture,
            ratatui::crossterm::event::EnableBracketedPaste
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
            ratatui::crossterm::event::DisableBracketedPaste,
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

// Sends the keys, mouse and terminal events read until `stop` is set, or the app quit.
fn read_events(stop: &AtomicBool, hover: &AtomicBool, tx: &Sender) {
    use ratatui::crossterm::event::Event;
    while !stop.load(Ordering::Relaxed) {
        match ratatui::crossterm::event::poll(std::time::Duration::from_millis(100)) {
            Ok(true) => {
                let event = match ratatui::crossterm::event::read() {
                    Ok(Event::Key(key)) => AppEvent::Key(key),
                    Ok(Event::Mouse(mouse)) => match mouse.kind {
                        MouseEventKind::ScrollDown
                        | MouseEventKind::ScrollUp
                        | MouseEventKind::Down(MouseButton::Left | MouseButton::Right)
                        | MouseEventKind::Drag(MouseButton::Left)
                        | MouseEventKind::Up(MouseButton::Left) => AppEvent::Mouse(mouse),
                        MouseEventKind::Moved if hover.load(Ordering::Relaxed) => {
                            AppEvent::Mouse(mouse)
                        }
                        _ => continue,
                    },
                    Ok(Event::Paste(text)) => AppEvent::Paste(text),
                    Ok(Event::Resize(..)) => AppEvent::Resize,
                    Ok(Event::FocusGained) => AppEvent::FocusGained,
                    Ok(_) => continue,
                    Err(e) => panic!("Failed to read event: {e}"),
                };
                if !tx.try_send(event) {
                    break;
                }
            }
            Ok(false) => {}
            Err(e) => panic!("Failed to poll event: {e}"),
        }
    }
}
