// Tests of the whole app, run as `main` runs it but without a terminal: the keys and the changes
// made to the repository meanwhile are the steps of a script, see `Harness::run`.

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::Rect,
};

use super::{
    App, AppContext, AppEvent, GraphFilter, InitialSelection, Launch, PendingClear, Ret, StartPage,
    StatusLine,
};
use crate::{
    color::GraphColorSet,
    config::GraphColorConfig,
    event::EventController,
    git::{refs_snapshot, CommitHash, FilesSnapshot, LoadOptions, NamedRef, SortCommit},
    graph::{CellWidthType, GraphImageWidthMode, GraphLayout, GraphStyle},
    keybind::{parse_key_event, KeyHint, Scope, UserEvent},
    layout::Panel,
    protocol::ImageProtocol,
    search::SearchOutcome,
    test_support::{git::git, ui::test_ctx},
    view::{DetailFocus, View},
    widget::commit_tabs::DetailTab,
    widget::git_dialog::GitDialog,
    widget::ref_list::RefSelection,
    widget::style::WHEEL_ROWS,
    Loader, Restart,
};

// The terminal the app is laid out in, (columns, rows).
const SIZE: (u16, u16) = (160, 40);
// The longest a git command run in the background, or a condition waited for, may take.
const TIMEOUT: Duration = Duration::from_secs(30);

enum Step {
    // a key, named as in the keybind config: "j", "shift-d", "esc", "ctrl-c"
    Key(KeyEvent),
    // keys pressed one right after another, before what the first does is done
    KeysAtOnce(Vec<KeyEvent>),
    // an event the app sends itself, like running a command in the terminal
    Event(Box<AppEvent>),
    // something done outside the app, like in another terminal
    Outside(Box<dyn FnOnce(&Path)>),
    // the refs and the local changes checked, as the refresher does every few seconds
    Tick,
    // the refs only checked, as the refresher does more often than the local changes
    RefsTick,
    // waits until the repository is so, like once a fetch in the background is done
    WaitFor(Box<dyn Fn(&Path) -> bool>),
    // time passing without a key, for what the app shows after a while
    Sleep(Duration),
    // a frame drawn, placing the borders that ctrl and the arrow keys move, then its text checked
    Draw(Box<dyn FnOnce(&str)>),
    Check(Box<dyn FnOnce(&App)>),
    // acts on the app right away, before what it starts in the background is done
    Act(Box<dyn FnOnce(&mut App)>),
    // a frame drawn, and the cell of the text in it (moved by the offset) kept for `Mouse`
    Locate(&'static str, (i32, i32)),
    // the mouse at the cell located last, in the frame drawn then
    Mouse(TestGesture),
    // steps whose events come together, handled before the app draws again
    Together(Vec<Step>),
}

// What the mouse does in a test, at the cell located last, see `mouse_on`.
#[derive(Debug, Clone, Copy)]
enum TestGesture {
    Click,
    CtrlClick,
    RightClick,
    // pressed there, dragged through the cells of these texts in the same frame, released
    Drag(&'static [&'static str]),
    // pressed there, dragged by so many cells, released
    DragBy(i32, i32),
    WheelDown,
    WheelUp,
    Hover,
}

// A frame drawn, cell by cell.
type Cells = Vec<Vec<String>>;

thread_local! {
    // the frame drawn by the last `Step::Locate`, and the cell it located
    static LOCATED: RefCell<(Cells, (u16, u16))> = RefCell::default();
}

// `gesture` on the first `text` drawn, standing alone (not inside a word or a hash).
fn mouse_on(text: &'static str, gesture: TestGesture) -> Vec<Step> {
    mouse_at(text, (0, 0), gesture)
}

// `gesture` on the cell `offset` from the first `text` drawn.
fn mouse_at(text: &'static str, offset: (i32, i32), gesture: TestGesture) -> Vec<Step> {
    vec![Step::Locate(text, offset), Step::Mouse(gesture)]
}

// A double click on `text`: a click, the frame drawn again as between two real clicks, a click on
// the same cell, whatever it shows then.
fn double_click(text: &'static str) -> Vec<Step> {
    let mut steps = mouse_on(text, TestGesture::Click);
    steps.extend(draw());
    steps.push(Step::Mouse(TestGesture::Click));
    steps
}

// The cell where `text` is drawn first in `cells`, standing alone.
fn find_cell(cells: &[Vec<String>], text: &str) -> (u16, u16) {
    let chars: Vec<String> = text.chars().map(String::from).collect();
    let alone = |cell: Option<&String>| {
        cell.is_none_or(|c| !c.chars().all(|c| c.is_alphanumeric()) || c.is_empty())
    };
    for (y, row) in cells.iter().enumerate() {
        for x in 0..row.len().saturating_sub(chars.len() - 1) {
            let at = row[x..x + chars.len()] == chars[..];
            let before = x.checked_sub(1).and_then(|b| row.get(b));
            if at && alone(before) && alone(row.get(x + chars.len())) {
                return (x as u16, y as u16);
            }
        }
    }
    let screen: Vec<String> = cells.iter().map(|row| row.concat()).collect();
    panic!("{text:?} not drawn in\n{}", screen.join("\n"));
}

fn mouse_event(kind: MouseEventKind, (x, y): (u16, u16), modifiers: KeyModifiers) -> AppEvent {
    AppEvent::Mouse(MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers,
    })
}

// The keys of `keys`, separated by spaces.
fn keys(keys: &str) -> Vec<Step> {
    keys.split(' ')
        .map(|key| Step::Key(parse_key_event(key).unwrap()))
        .collect()
}

fn keys_at_once(keys: &str) -> Vec<Step> {
    let keys = keys.split(' ').map(|key| parse_key_event(key).unwrap());
    vec![Step::KeysAtOnce(keys.collect())]
}

// The keys typing `text`.
fn typing(text: &str) -> Vec<Step> {
    text.chars()
        .map(|c| Step::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)))
        .collect()
}

fn outside(f: impl FnOnce(&Path) + 'static) -> Vec<Step> {
    vec![Step::Outside(Box::new(f))]
}

// A git command run outside the app.
fn outside_git(args: &'static [&'static str]) -> Vec<Step> {
    outside(move |path| {
        git(path, args);
    })
}

fn wait_for(f: impl Fn(&Path) -> bool + 'static) -> Vec<Step> {
    vec![Step::WaitFor(Box::new(f))]
}

fn check(f: impl FnOnce(&App) + 'static) -> Vec<Step> {
    vec![Step::Check(Box::new(f))]
}

fn draw() -> Vec<Step> {
    screen(|_| {})
}

// A frame drawn, its rows one per line.
fn screen(f: impl FnOnce(&str) + 'static) -> Vec<Step> {
    vec![Step::Draw(Box::new(f))]
}

fn sleep(millis: u64) -> Vec<Step> {
    vec![Step::Sleep(Duration::from_millis(millis))]
}

fn tick() -> Vec<Step> {
    vec![Step::Tick]
}

struct Harness {
    path: PathBuf,
    loader: Loader,
    ctx: Rc<AppContext>,
    ec: EventController,
    // how many times the repository was loaded, the first time and those failing included
    loads: std::cell::Cell<usize>,
    quit: bool,
}

impl Harness {
    fn new(path: &Path, launch: Launch) -> Self {
        let loader = Loader {
            options: LoadOptions {
                path: path.to_path_buf(),
                sort: SortCommit::Chronological,
                max_count: None,
                mailmap: false,
                primary_branch: None,
            },
            graph_width: None,
            graph_style: GraphStyle::Rounded,
            graph_antialias: false,
            graph_image_width_mode: GraphImageWidthMode::Compact,
            graph_color_set: GraphColorSet::new(&GraphColorConfig::default()),
            image_protocol: ImageProtocol::Kitty,
            launch,
        };
        Harness {
            path: path.to_path_buf(),
            loader,
            ctx: test_ctx(),
            ec: EventController::without_terminal(),
            loads: Default::default(),
            quit: false,
        }
    }

    // Starts the app and runs the steps on it, one after another; after each, the events it sent,
    // the git command it runs in the background and the reloads are handled, as the app would
    // before the next key.
    fn run(&mut self, steps: Vec<Vec<Step>>) {
        let mut steps = steps.into_iter().flatten();
        let (path, ctx, ec, quit) = (&self.path, &self.ctx, &self.ec, &mut self.quit);
        let loads = &self.loads;
        let (repository, layout) = self.loader.load_work(&ctx.running_task)().unwrap();
        let mut repository = Arc::new(repository);
        loads.set(loads.get() + 1);
        let mut restart = Restart {
            before: None,
            layout: Some(layout),
            images: None,
        };
        loop {
            // what the app was before reloading, None once done
            let next = self
                .loader
                .with_app(&repository, SIZE, ctx, ec, restart, |mut app| loop {
                    if let Some(ret) = settle(&mut app, ec, ctx, loads) {
                        return match ret {
                            Ret::Quit => {
                                *quit = true;
                                None
                            }
                            Ret::Reload {
                                context,
                                repository,
                                layout,
                                images,
                            } => {
                                Some((app.into_shell(), context, layout, Some(repository), images))
                            }
                            Ret::Regraph {
                                context,
                                layout,
                                images,
                            } => Some((app.into_shell(), context, layout, None, images)),
                        };
                    }
                    take_step(steps.next()?, &mut app, path, ec);
                })
                .unwrap();
            match next {
                None => return,
                Some((shell, context, layout, reloaded, images)) => {
                    restart = Restart {
                        before: Some((shell, context)),
                        layout: Some(*layout),
                        images,
                    };
                    if let Some(reloaded) = reloaded {
                        repository = Arc::new(*reloaded);
                    }
                }
            }
        }
    }
}

// Handles the events sent meanwhile, waiting for the git command and the reload running in the
// background; returns what ends or reloads the app.
fn settle(
    app: &mut App,
    ec: &EventController,
    ctx: &AppContext,
    loads: &std::cell::Cell<usize>,
) -> Option<Ret> {
    // a frame drawn after the events handled, as the app draws after each batch of them
    let mut handled = false;
    loop {
        app.expire(Instant::now());
        app.prepare_render(Rect::new(0, 0, SIZE.0, SIZE.1));
        let busy = ctx.running_task.lock().is_some() || app.is_reloading() || app.is_loading();
        let event = if busy {
            ec.recv_timeout(TIMEOUT)
                .expect("the git command, the reload or the diff loading to finish")
        } else {
            match ec.try_recv() {
                Some(event) => event,
                None if handled => {
                    handled = false;
                    render(app);
                    continue;
                }
                None => return None,
            }
        };
        handled = true;
        if let AppEvent::Reloaded { .. } = event {
            loads.set(loads.get() + 1);
        }
        if let Some(ret) = app.handle_event(event).unwrap() {
            return Some(ret);
        }
    }
}

// A frame of the app as it is, its text.
fn render(app: &mut App) -> Vec<Vec<String>> {
    let backend = ratatui::backend::TestBackend::new(SIZE.0, SIZE.1);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    let keys = app.keys_popup_keys();
    let frame = terminal.draw(|f| app.render(f, &keys)).unwrap();
    frame
        .buffer
        .content
        .chunks(SIZE.0 as usize)
        .map(|row| row.iter().map(|c| c.symbol().to_string()).collect())
        .collect()
}

fn take_step(step: Step, app: &mut App, path: &Path, ec: &EventController) {
    match step {
        Step::Together(steps) => steps
            .into_iter()
            .for_each(|step| take_step(step, app, path, ec)),
        Step::Key(key) => ec.send(AppEvent::Key(key)),
        Step::KeysAtOnce(keys) => keys.into_iter().for_each(|key| ec.send(AppEvent::Key(key))),
        Step::Event(event) => ec.send(*event),
        Step::Outside(f) => f(path),
        Step::RefsTick => {
            let git_dir = app.repository.git_dir();
            ec.send(AppEvent::RefsSnapshot(
                refs_snapshot(path, git_dir).unwrap(),
            ));
        }
        Step::Tick => {
            let git_dir = app.repository.git_dir();
            ec.send(AppEvent::RefsSnapshot(
                refs_snapshot(path, git_dir).unwrap(),
            ));
            ec.send(AppEvent::FilesTick(FilesSnapshot::load(path, git_dir)));
        }
        Step::WaitFor(f) => {
            let started = Instant::now();
            while !f(path) {
                assert!(started.elapsed() < TIMEOUT, "waited too long");
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        Step::Sleep(duration) => std::thread::sleep(duration),
        Step::Draw(f) => {
            let backend = ratatui::backend::TestBackend::new(SIZE.0, SIZE.1);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            let keys = app.keys_popup_keys();
            let frame = terminal.draw(|f| app.render(f, &keys)).unwrap();
            let rows = frame.buffer.content.chunks(SIZE.0 as usize);
            let text: Vec<String> = rows
                .map(|r| r.iter().map(|c| c.symbol()).collect())
                .collect();
            f(&text.join("\n"));
        }
        Step::Check(f) => f(app),
        Step::Act(f) => f(app),
        Step::Locate(text, (dx, dy)) => {
            let backend = ratatui::backend::TestBackend::new(SIZE.0, SIZE.1);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            let keys = app.keys_popup_keys();
            let frame = terminal.draw(|f| app.render(f, &keys)).unwrap();
            let cells: Vec<Vec<String>> = frame
                .buffer
                .content
                .chunks(SIZE.0 as usize)
                .map(|row| row.iter().map(|c| c.symbol().to_string()).collect())
                .collect();
            let (x, y) = find_cell(&cells, text);
            let at = ((x as i32 + dx) as u16, (y as i32 + dy) as u16);
            LOCATED.with(|located| *located.borrow_mut() = (cells, at));
        }
        Step::Mouse(gesture) => {
            let (cells, at) = LOCATED.with(|located| located.borrow().clone());
            let none = KeyModifiers::NONE;
            let left = MouseButton::Left;
            let press = |at, modifiers| {
                ec.send(mouse_event(MouseEventKind::Down(left), at, modifiers));
                ec.send(mouse_event(MouseEventKind::Up(left), at, modifiers));
            };
            let drag = |through: Vec<(u16, u16)>| {
                ec.send(mouse_event(MouseEventKind::Down(left), at, none));
                for cell in &through {
                    ec.send(mouse_event(MouseEventKind::Drag(left), *cell, none));
                }
                let end = through.last().copied().unwrap_or(at);
                ec.send(mouse_event(MouseEventKind::Up(left), end, none));
            };
            match gesture {
                TestGesture::Click => press(at, none),
                TestGesture::CtrlClick => press(at, KeyModifiers::CONTROL),
                TestGesture::RightClick => ec.send(mouse_event(
                    MouseEventKind::Down(MouseButton::Right),
                    at,
                    none,
                )),
                TestGesture::Drag(texts) => {
                    drag(texts.iter().map(|text| find_cell(&cells, text)).collect())
                }
                TestGesture::DragBy(dx, dy) => {
                    drag(vec![((at.0 as i32 + dx) as u16, (at.1 as i32 + dy) as u16)])
                }
                TestGesture::WheelDown => {
                    ec.send(mouse_event(MouseEventKind::ScrollDown, at, none))
                }
                TestGesture::WheelUp => ec.send(mouse_event(MouseEventKind::ScrollUp, at, none)),
                TestGesture::Hover => ec.send(mouse_event(MouseEventKind::Moved, at, none)),
            }
        }
    }
}

fn launch() -> Launch {
    Launch {
        page: StartPage::Graph,
        selection: InitialSelection::Latest,
        fetch: false,
    }
}

// A repository at `dir/work`: f.txt changed on master after the branch feat, which changes it too.
// The commits a minute apart, so the graph shows them in the same order however fast they are made:
// c2, feat1, c1.
fn repository(dir: &Path) -> PathBuf {
    let path = dir.join("work");
    std::fs::create_dir(&path).unwrap();
    let run = |args: &[&str]| {
        git(&path, args);
    };
    run(&["init", "-q", "-b", "master"]);
    run(&["config", "user.name", "t"]);
    run(&["config", "user.email", "t@t"]);
    run(&["config", "commit.gpgsign", "false"]);
    let commit = |file: &str, content: &str, message: &str, minute: u64| {
        std::fs::write(path.join(file), content).unwrap();
        git(&path, &["add", file]);
        let date = format!("@{} +0000", 1_700_000_000 + minute * 60);
        let output = crate::git::git_command()
            .args(["commit", "-q", "-m", message])
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .current_dir(&path)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    };
    commit("f.txt", "a\nb\nc\n", "c1", 0);
    run(&["checkout", "-q", "-b", "feat"]);
    commit("f.txt", "a\nFEAT\nc\n", "feat1", 1);
    run(&["checkout", "-q", "master"]);
    commit("f.txt", "a\nMASTER\nc\n", "c2", 2);
    path
}

// `repository` pushed to `dir/remote.git` as origin, master and feat tracking their branches there.
fn with_remote(dir: &Path) -> PathBuf {
    let path = repository(dir);
    git(dir, &["init", "-q", "--bare", "-b", "master", "remote.git"]);
    let remote = dir.join("remote.git");
    git(
        &path,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&path, &["push", "-q", "-u", "origin", "master", "feat"]);
    // origin/HEAD, as a clone has: else git 2.48+ creates it on the first fetch
    git(&path, &["remote", "set-head", "origin", "master"]);
    path
}

// A commit made on `branch` of the remote, as pushed from another clone, adding `file`.
fn pushed_elsewhere(dir: &Path, branch: &str, file: &str) {
    let other = dir.join("other");
    if !other.exists() {
        git(dir, &["clone", "-q", "remote.git", "other"]);
        git(&other, &["config", "user.name", "o"]);
        git(&other, &["config", "user.email", "o@o"]);
        git(&other, &["config", "commit.gpgsign", "false"]);
    }
    git(&other, &["checkout", "-q", branch]);
    std::fs::write(other.join(file), "x\n").unwrap();
    git(&other, &["add", file]);
    git(&other, &["commit", "-q", "-m", file]);
    git(&other, &["push", "-q", "origin", branch]);
}

fn notices(app: &App) -> Vec<String> {
    app.shell
        .notices
        .iter()
        .map(|n| n.message.clone())
        .collect()
}

fn assert_notice(app: &App, part: &str) {
    let notices = notices(app);
    assert!(
        notices.iter().any(|n| n.contains(part)),
        "no notice saying {part:?} in {notices:?}"
    );
}

// What the commit list's title says the graph shows, besides all the history.
fn graph_note(app: &App) -> Option<String> {
    let note = app.shell.graph_filter_note(app.repository);
    note.map(|note| note.text().to_string())
}

// The files the comparison shows, None without one.
fn compared(app: &App) -> Option<Vec<String>> {
    match &app.view {
        View::Detail(view) if view.is_comparing() => {
            Some(view.changed_paths().into_iter().map(String::from).collect())
        }
        _ => None,
    }
}

fn draft(app: &App) -> String {
    match &app.view {
        View::LocalChanges(view) => view.context().commit_draft_message(),
        _ => panic!("not on the local changes page"),
    }
}

// The file selected in the local changes page.
fn local_change_path(app: &App) -> Option<String> {
    match &app.view {
        View::LocalChanges(view) => view.context().selected_path,
        _ => panic!("not on the local changes page"),
    }
}

// The file selected in the tree of the open details tab.
fn detail_path(app: &App) -> Option<String> {
    match &app.view {
        View::Detail(view) => view.selected_path(),
        _ => panic!("not in the commit details"),
    }
}

// The launch options

#[test]
fn test_launch_on_local_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let launch = Launch {
        page: StartPage::LocalChanges,
        ..launch()
    };
    let mut harness = Harness::new(&path, launch);
    harness.run(vec![check(|app| {
        assert!(matches!(app.view, View::LocalChanges(_)));
    })]);
}

#[test]
fn test_a_key_after_moving_acts_on_the_diff_moved_to() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    std::fs::write(path.join("g.txt"), "g\n").unwrap();
    git(&path, &["add", "g.txt"]);
    git(&path, &["commit", "-q", "-m", "g"]);
    std::fs::write(path.join("f.txt"), "F\n").unwrap();
    std::fs::write(path.join("g.txt"), "G\n").unwrap();
    let launch = Launch {
        page: StartPage::LocalChanges,
        ..launch()
    };
    let mut harness = Harness::new(&path, launch);
    harness.run(vec![
        check(|app| assert_eq!(local_change_path(app), Some(":/".into()))),
        // the diff of g.txt loads in the background after j: the keys after it wait for it
        keys_at_once("j j enter enter space"),
        check(|app| assert_eq!(local_change_path(app), Some("g.txt".into()))),
    ]);
    let staged = git(&path, &["diff", "--cached"]);
    // the first changed line of g.txt, nothing of f.txt
    assert!(
        staged.contains("-g") && !staged.contains("f.txt"),
        "{staged}"
    );
}

#[test]
fn test_a_key_after_moving_in_the_details_acts_on_the_diff_moved_to() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        // from the root row to f.txt, whose diff loads in the background: enter waits for it
        keys("4"),
        keys_at_once("j enter"),
        check(|app| {
            let View::Detail(view) = &app.view else {
                panic!("no details")
            };
            assert_eq!(view.focus(), DetailFocus::Pane);
        }),
        screen(|text| assert!(!text.contains("Loading…"), "{text}")),
    ]);
}

#[test]
fn test_a_key_after_staging_acts_on_the_files_reloaded() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    std::fs::write(path.join("g.txt"), "g\n").unwrap();
    git(&path, &["add", "g.txt"]);
    git(&path, &["commit", "-q", "-m", "g"]);
    std::fs::write(path.join("f.txt"), "F\n").unwrap();
    std::fs::write(path.join("g.txt"), "G\n").unwrap();
    let launch = Launch {
        page: StartPage::LocalChanges,
        ..launch()
    };
    let mut harness = Harness::new(&path, launch);
    harness.run(vec![
        // f.txt staged, the files reload in the background: the second space waits for them,
        // then stages g.txt, on the row f.txt left
        keys_at_once("j space space"),
    ]);
    let staged = git(&path, &["diff", "--cached", "--name-only"]);
    assert_eq!(staged, "f.txt\ng.txt\n");
}

#[test]
fn test_a_key_after_moving_to_another_commit_acts_on_its_details() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    std::fs::write(path.join("g.txt"), "g\n").unwrap();
    git(&path, &["add", "g.txt"]);
    // the newest, the first row
    let commit = crate::git::git_command()
        .args(["commit", "-q", "-m", "g"])
        .env("GIT_COMMITTER_DATE", "@4000000000 +0000")
        .current_dir(&path)
        .output()
        .unwrap();
    assert!(
        commit.status.success(),
        "{}",
        String::from_utf8_lossy(&commit.stderr)
    );
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        // the changes of g, the newest commit, then the next one, which changed f.txt: its files
        // load in the background after J, j to its file waits for them, enter for its diff
        keys("4 j"),
        check(|app| assert_eq!(detail_path(app), Some("g.txt".into()))),
        keys_at_once("shift-j j enter"),
        check(|app| {
            let View::Detail(view) = &app.view else {
                panic!("no details")
            };
            assert_eq!(view.focus(), DetailFocus::Pane);
            assert_eq!(detail_path(app), Some("f.txt".into()));
        }),
    ]);
}

#[test]
fn test_several_notices_show_until_dismissed() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    let event = |e: AppEvent| vec![Step::Event(Box::new(e))];
    harness.run(vec![
        event(AppEvent::NotifyWarn("first".into())),
        event(AppEvent::NotifyError("second".into())),
        event(AppEvent::NotifyWarn("third".into())),
        check(|app| assert_eq!(notices(app), ["first", "second", "third"])),
        keys(","),
        check(|app| assert!(notices(app).is_empty())),
    ]);
}

#[test]
fn test_sidebar_focused_shrunk_and_grown_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    let mode = |focused, collapsed| {
        check(move |app: &App| {
            assert_eq!(app.shell.sidebar_focused(), focused);
            assert_eq!(app.shell.sidebar_collapsed(), collapsed);
        })
    };
    harness.run(vec![
        mode(false, false),
        keys("1"),
        mode(true, false),
        // again, it shrinks and the page has the focus
        keys("1"),
        mode(false, true),
        keys("1"),
        mode(true, false),
        keys("2"),
        mode(false, false),
    ]);
}

#[test]
fn test_sidebar_only_beside_the_graph() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    let focused =
        |focused| check(move |app: &App| assert_eq!(app.shell.sidebar_focused(), focused));
    harness.run(vec![
        keys("1"),
        focused(true),
        // the local changes page takes the focus from it, and has none to give it
        keys("tab"),
        check(|app| assert!(matches!(app.view, View::LocalChanges(_)))),
        focused(false),
        keys("1 ctrl-h"),
        focused(false),
        // back on the graph, the sidebar has it
        keys("tab"),
        focused(true),
    ]);
}

// What covers the app: "none", "help", "blame", "timeline" or "log".
fn overlay(expected: &'static str) -> Vec<Step> {
    check(move |app: &App| {
        let shown = match &app.shell.overlay {
            None => "none",
            Some(super::Overlay::Help(_)) => "help",
            Some(super::Overlay::File(super::FileWindow::Blame(_))) => "blame",
            Some(super::Overlay::File(super::FileWindow::Timeline(_))) => "timeline",
            Some(super::Overlay::CommandLog(_)) => "log",
        };
        assert_eq!(shown, expected);
    })
}

// The page bar, as text.
fn page_bar(app: &App) -> String {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(SIZE.0, 1)).unwrap();
    let frame = terminal.draw(|f| app.render_page_bar(f, f.area())).unwrap();
    frame.buffer.content.iter().map(|c| c.symbol()).collect()
}

#[test]
fn test_page_bar_names_the_window_over_the_pages() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    let named = |name: &'static str| {
        check(move |app| {
            let bar = page_bar(app);
            assert!(bar.contains(name), "no {name:?} in {bar:?}");
        })
    };
    harness.run(vec![
        keys("?"),
        named("Keys"),
        // the command log has no key, it is run from the keys list
        keys("/"),
        typing("git commands run"),
        keys("enter enter"),
        named("Command Log"),
        keys("esc 5 shift-b"),
        named("Blame"),
        named("f.txt"),
        // back to the commit shown, then closed
        keys("esc esc shift-t"),
        named("Timeline"),
    ]);
}

#[test]
fn test_command_log_keys_like_everywhere() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    std::fs::write(path.join("f.txt"), "changed\n").unwrap();
    let mut harness = Harness::new(&path, launch());
    let copy_named = |expected: &'static str| {
        check(move |app| {
            let hints = app.shown_keys(app.local_hints());
            let copy = hints.iter().find(|k| k.action == "copy").unwrap();
            assert_eq!(copy.keys, expected);
        })
    };
    harness.run(vec![
        // y starts copying there too, the popup listing what finishes it
        keys("@"),
        overlay("log"),
        copy_named("yy"),
        keys("y"),
        check(|app| {
            assert_eq!(app.shell.app_status.pending_keys.len(), 1);
            let next: Vec<(String, String)> = app
                .keys_popup_keys()
                .into_iter()
                .map(|k| (k.keys, k.action))
                .collect();
            assert_eq!(next, [("y".to_string(), "copy".to_string())]);
        }),
        keys("y"),
        check(|app| assert!(app.shell.app_status.pending_keys.is_empty())),
        keys("esc"),
        // named the same over a diff, whose y copies at once
        keys("tab enter @"),
        overlay("log"),
        copy_named("yy"),
    ]);
}

#[test]
fn test_help_and_command_log_cover_the_app_one_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("?"),
        overlay("help"),
        // one panel, like the command log's, scrolled with its scrollbar
        screen(|screen| {
            assert!(screen.contains("╭ Keys "), "{screen}");
            let thumb = screen.lines().any(|l| l.ends_with('█'));
            assert!(thumb, "{screen}");
        }),
        // the keys list takes the keys
        keys("tab"),
        overlay("help"),
        keys("esc"),
        overlay("none"),
        // the command log has no key, it is run from the keys list
        keys("? /"),
        typing("git commands run"),
        keys("enter enter"),
        overlay("log"),
        keys("?"),
        overlay("log"),
        keys("esc"),
        overlay("none"),
    ]);
}

// The fetch on launch

#[test]
fn test_launch_fetch_shows_what_it_fetched() {
    let dir = tempfile::tempdir().unwrap();
    let path = with_remote(dir.path());
    pushed_elsewhere(dir.path(), "master", "new.txt");
    let fetched =
        |path: &Path| git(path, &["log", "-1", "--format=%s", "origin/master"]) == "new.txt\n";
    let mut harness = Harness::new(
        &path,
        Launch {
            fetch: true,
            ..launch()
        },
    );
    harness.run(vec![
        wait_for(fetched),
        // the refs moved, the app reloads
        tick(),
        check(|app| {
            let head = app.repository.resolve("origin/master").unwrap();
            assert!(app.repository.commit(&head).is_some());
            assert!(notices(app).is_empty());
        }),
    ]);
    assert_eq!(harness.loads.get(), 2);
}

#[test]
fn test_launch_fetch_bringing_nothing_reloads_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = with_remote(dir.path());
    let fetched = |path: &Path| path.join(".git/FETCH_HEAD").exists();
    let mut harness = Harness::new(
        &path,
        Launch {
            fetch: true,
            ..launch()
        },
    );
    harness.run(vec![wait_for(fetched), tick(), tick()]);
    assert_eq!(harness.loads.get(), 1);
}

#[test]
fn test_launch_fetch_holds_no_command_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = with_remote(dir.path());
    // a remote slow to answer
    git(
        &path,
        &[
            "config",
            "remote.origin.uploadpack",
            "sleep 2; git-upload-pack",
        ],
    );
    let mut harness = Harness::new(
        &path,
        Launch {
            fetch: true,
            ..launch()
        },
    );
    harness.run(vec![
        keys("f"),
        check(|app| {
            assert!(app.shell.dialog.is_some(), "{:?}", notices(app));
            assert!(app.ctx.running_task.lock().is_none());
        }),
    ]);
}

#[test]
fn test_launch_fetch_failing_says_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = with_remote(dir.path());
    git(&path, &["remote", "set-url", "origin", "/nowhere"]);
    let mut harness = Harness::new(
        &path,
        Launch {
            fetch: true,
            ..launch()
        },
    );
    // nothing tells when it is done: given the time a failing fetch takes
    harness.run(vec![
        outside(|_| std::thread::sleep(Duration::from_secs(1))),
        tick(),
        check(|app| assert!(notices(app).is_empty())),
    ]);
}

// The reloads for changes made outside the app

#[test]
fn test_reload_waits_for_the_popup_to_close() {
    let dir = tempfile::tempdir().unwrap();
    let path = with_remote(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("f"),
        outside_git(&["update-ref", "refs/remotes/origin/elsewhere", "HEAD"]),
        tick(),
        check(|app| assert!(app.shell.dialog.is_some())),
        keys("esc"),
        tick(),
        check(|app| assert!(app.repository.resolve("origin/elsewhere").is_some())),
    ]);
    assert_eq!(harness.loads.get(), 2);
}

#[test]
fn test_moving_while_reloading_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        // j comes while the first reload runs, which the second one makes stale
        keys_at_once("ctrl-r j ctrl-r"),
        check(|app| {
            assert!(!app.is_reloading());
            let state = app.view.as_list_state().unwrap();
            assert_eq!(state.cursor().selected(), 1);
        }),
    ]);
    // the first, and both reloads, the stale one dropped
    assert_eq!(harness.loads.get(), 3);
}

#[test]
fn test_reload_failing_keeps_the_app_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    std::fs::write(path.join("f.txt"), "changed\n").unwrap();
    let launch = Launch {
        page: StartPage::LocalChanges,
        ..launch()
    };
    let broken = |path: &Path| path.join(".git/refs/heads/broken");
    let mut harness = Harness::new(&path, launch);
    harness.run(vec![
        keys("3"),
        typing("keep me"),
        keys("esc"),
        // a branch to an object that doesn't exist
        outside(move |path| {
            std::fs::write(broken(path), "1234567890123456789012345678901234567890\n").unwrap()
        }),
        tick(),
        check(|app| {
            assert_notice(app, "git log failed: fatal: bad object");
            assert_eq!(draft(app), "keep me");
        }),
        // not again until the refs change
        tick(),
        outside(move |path| std::fs::remove_file(broken(path)).unwrap()),
        tick(),
        check(|app| assert_eq!(draft(app), "keep me")),
    ]);
    // the first, the failing one, and once fixed
    assert_eq!(harness.loads.get(), 3);
}

// The keys of the whole app

#[test]
fn test_ctrl_c_quits() {
    for steps in ["ctrl-c", "/ ctrl-c", "tab ctrl-c"] {
        let dir = tempfile::tempdir().unwrap();
        let path = repository(dir.path());
        let mut harness = Harness::new(&path, launch());
        harness.run(vec![keys(steps)]);
        assert!(harness.quit, "{steps}");
    }
}

// q quits from the menu and the popups too: it is only ever the key quitting the app.
#[test]
fn test_q_quits_from_the_menu_and_popups() {
    for open in ["menu", "popup"] {
        let dir = tempfile::tempdir().unwrap();
        let path = repository(dir.path());
        let mut harness = Harness::new(&path, launch());
        let opening = match open {
            "menu" => mouse_on("c2", TestGesture::RightClick),
            _ => keys("b esc"),
        };
        harness.run(vec![opening, keys("q")]);
        assert!(harness.quit, "{open}");
    }
}

// Comparing refs diffed from the sidebar, and commits diffed in the graph

// Diffs feat with HEAD from the sidebar.
fn compare_feat() -> Vec<Step> {
    let mut steps = keys("1 j shift-d");
    steps.extend(check(|app| {
        assert_eq!(graph_note(app).as_deref(), Some("master ↔ feat"));
        assert_eq!(compared(app), Some(vec!["f.txt".to_string()]));
    }));
    steps
}

#[test]
fn test_compare_takes_the_branch_not_its_tag() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    // a tag with the branch's name, at master: git takes the tag for the short name
    git(&path, &["tag", "feat", "master"]);
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![compare_feat()]);
}

#[test]
fn test_sidebar_branch_shows_its_commit_not_its_tags() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let feat = git(&path, &["rev-parse", "feat"]).trim().to_string();
    // a tag with the branch's name, at another commit
    git(&path, &["tag", "feat", "master"]);
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        // from c2, the first row; enter shows the branch on the graph, which doesn't follow the
        // sidebar by default
        keys("1 j enter"),
        check(move |app| {
            let state = app.view.as_list_state().unwrap();
            assert_eq!(state.selected_commit_hash().as_str(), feat);
        }),
    ]);
}

#[test]
fn test_compare_follows_the_reloads_until_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        compare_feat(),
        keys("ctrl-r"),
        check(|app| assert_eq!(compared(app), Some(vec!["f.txt".to_string()]))),
        // feat moved in another terminal, the comparison follows it
        outside(|path| {
            git(path, &["checkout", "-q", "feat"]);
            std::fs::write(path.join("new.txt"), "x\n").unwrap();
            git(path, &["add", "new.txt"]);
            git(path, &["commit", "-q", "-m", "feat2"]);
            git(path, &["checkout", "-q", "master"]);
        }),
        tick(),
        check(|app| {
            let paths = vec!["f.txt".to_string(), "new.txt".to_string()];
            assert_eq!(compared(app), Some(paths));
        }),
        keys("esc"),
        check(|app| {
            assert_eq!(compared(app), None);
            assert_eq!(graph_note(app), None);
        }),
    ]);
}

#[test]
fn test_reload_keeps_the_graph_width_of_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        check(|app| assert_eq!(app.ctx.graph.borrow().cell_width_type, CellWidthType::Double)),
        // branches made in another terminal, too many side by side for the terminal's width
        outside(|path| {
            use std::io::Write;
            let master = git(path, &["rev-parse", "master"]);
            let stream: String = (0..SIZE.0)
                .map(|i| {
                    let message = format!("fan{i}");
                    format!(
                        "commit refs/heads/fan{i}\ncommitter a <b> 0 +0000\ndata {}\n{message}\nfrom {}\n\n",
                        message.len(),
                        master.trim()
                    )
                })
                .collect();
            let mut import = crate::git::git_command()
                .args(["fast-import", "--quiet"])
                .current_dir(path)
                .stdin(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            import.stdin.take().unwrap().write_all(stream.as_bytes()).unwrap();
            assert!(import.wait().unwrap().success());
        }),
        // reloaded, the graph is cut like on a resize, instead of the app quitting
        keys("ctrl-r"),
        check(|app| assert_eq!(app.ctx.graph.borrow().cell_width_type, CellWidthType::Double)),
    ]);
}

#[test]
fn test_compare_of_a_ref_gone_ends() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        compare_feat(),
        outside_git(&["branch", "-q", "-D", "feat"]),
        tick(),
        check(|app| {
            assert_notice(app, "Cannot diff master and feat, one of them is gone");
            assert_eq!(compared(app), None);
            assert_eq!(graph_note(app), None);
        }),
    ]);
}

#[test]
fn test_compare_ends_on_the_local_changes_page() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        compare_feat(),
        keys("tab"),
        check(|app| {
            assert!(matches!(app.view, View::LocalChanges(_)));
            assert_eq!(graph_note(app), None);
        }),
        keys("tab"),
        check(|app| assert_eq!(graph_note(app), None)),
    ]);
}

#[test]
fn test_files_keys_do_nothing_in_their_diff() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        // the files of the changes tab reset a file; their diff doesn't
        keys("4 u"),
        check(|app| assert!(app.shell.dialog.is_some())),
        keys("esc enter u"),
        check(|app| assert!(app.shell.dialog.is_none())),
    ]);
}

#[test]
fn test_keys_unlisted_where_they_only_warn() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    let listed = |hints: Vec<KeyHint>, short: &str| hints.iter().any(|(_, s)| s == short);
    harness.run(vec![
        // no remote: push, pull and fetch say so, unlisted
        check(move |app| assert!(!listed(app.app_keys().hints(), "push"))),
        keys("p"),
        check(|app| assert_notice(app, "No remotes configured")),
        // with a commit picked, the commands of one commit ask to clear it, unlisted
        keys(", v"),
        check(move |app| assert!(!listed(app.view.keys().hints(), "new branch"))),
        keys("b"),
        check(|app| {
            assert_notice(
                app,
                "works on the selected commit, clear the picked ones first",
            )
        }),
    ]);
}

#[test]
fn test_details_list_only_their_own_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    let shown = |app: &App| -> Vec<String> {
        app.view
            .keys()
            .hints()
            .into_iter()
            .map(|(_, s)| s)
            .collect()
    };
    harness.run(vec![
        // the files of the changes tab, not the commit list above them
        keys("4"),
        check(move |app| {
            let shown = shown(app);
            assert!(shown.contains(&"copy path".to_string()), "{shown:?}");
            assert!(!shown.contains(&"copy hash".to_string()), "{shown:?}");
            assert!(!shown.contains(&"details".to_string()), "{shown:?}");
        }),
        // and the other way round
        keys("2"),
        check(move |app| {
            let shown = shown(app);
            assert!(shown.contains(&"copy hash".to_string()), "{shown:?}");
            assert!(!shown.contains(&"copy path".to_string()), "{shown:?}");
        }),
    ]);
}

#[test]
fn test_commands_moving_head_unlisted_while_an_operation_waits() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    let listed = |app: &App, short: &str| app.view.keys().hints().iter().any(|(_, s)| s == short);
    harness.run(vec![
        // feat1: the commit checked out has nothing to check out
        keys("j"),
        check(move |app| assert!(listed(app, "checkout"))),
        // a merge stopped, as git leaves it: the keys follow the repository as loaded, which the
        // next check of the refs reloads
        outside(|path| {
            let head = git(path, &["rev-parse", "HEAD"]);
            std::fs::write(path.join(".git/MERGE_HEAD"), head).unwrap();
        }),
        check(move |app| assert!(listed(app, "checkout"))),
        vec![Step::RefsTick],
        check(move |app| {
            assert!(!listed(app, "checkout"));
            // the others stay
            assert!(listed(app, "new branch"));
        }),
        keys("space"),
        check(|app| assert_notice(app, "A merge is in progress")),
    ]);
}

#[test]
fn test_operation_started_outside_reloads_on_the_refs_check() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let launch = Launch {
        page: StartPage::LocalChanges,
        ..launch()
    };
    let aborts = |app: &App| app.view.keys().takes(crate::keybind::UserEvent::Abort);
    let mut harness = Harness::new(&path, launch);
    harness.run(vec![
        check(move |app| assert!(!aborts(app))),
        // a merge stopped, as git leaves it, which moves no ref and changes no file
        outside(|path| {
            let head = git(path, &["rev-parse", "HEAD"]);
            std::fs::write(path.join(".git/MERGE_HEAD"), head).unwrap();
        }),
        // seen by the frequent check of the refs, without waiting for the local changes
        vec![Step::RefsTick],
        check(move |app| assert!(aborts(app))),
    ]);
}

#[test]
fn test_checks_skipped_under_a_popup_run_once_it_closes() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    // the pokes asking the refresher to check right away
    let (poke, poked) = std::sync::mpsc::channel();
    harness.ec.set_refresher(poke);
    let poked = Rc::new(poked);
    let (before, after) = (poked.clone(), poked);
    harness.run(vec![
        // the popup of a reset pauses reloading, the check of the refs and files is skipped
        keys("G shift-u"),
        tick(),
        check(move |_| assert!(before.try_recv().is_err())),
        // closing it checks again right away, rather than on the next tick
        keys("esc"),
        check(move |_| assert!(after.try_recv().is_ok())),
    ]);
}

#[test]
fn test_repository_refuses_changes_while_the_app_runs_a_command() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    std::fs::write(path.join("f.txt"), "changed\n").unwrap();
    // the app's command running, as the repository sees it, also once reloaded
    let running = |app: &App, on: bool| {
        *app.ctx.running_task.lock() = on.then(|| crate::git::RunningTask {
            task: crate::git::GitTask {
                running: "Pushing master".into(),
                ..Default::default()
            },
            head_before_commit: None,
            quit_after: false,
        });
    };
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("ctrl-r"),
        check(move |app| {
            running(app, true);
            let refused = app.repository.stage_paths(&["f.txt"]);
            running(app, false);
            assert_eq!(refused, Err("Pushing master, wait for it to finish".into()));
            app.repository.stage_paths(&["f.txt"]).unwrap();
        }),
    ]);
}

#[test]
fn test_no_file_keys_on_a_commit_without_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    // the newest, first in the graph
    let status = crate::git::git_command()
        .args(["commit", "-q", "--allow-empty", "-m", "empty"])
        .env("GIT_COMMITTER_DATE", "2099-01-01 00:00:00 +0000")
        .env("GIT_AUTHOR_DATE", "2099-01-01 00:00:00 +0000")
        .current_dir(&path)
        .status()
        .unwrap();
    assert!(status.success());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("4"),
        check(|app| {
            let shown: Vec<String> = app
                .view
                .keys()
                .hints()
                .into_iter()
                .map(|(_, s)| s)
                .collect();
            for short in [
                "select hunks",
                "search",
                "filter",
                "copy path",
                "ignore",
                "copy link",
            ] {
                assert!(!shown.contains(&short.to_string()), "{short} in {shown:?}");
            }
        }),
    ]);
}

#[test]
fn test_no_other_diff_while_one_is_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    // in the commit list over the diff, picking and diffing say why they don't work
    harness.run(vec![
        compare_feat(),
        keys("2 v"),
        check(|app| assert_notice(app, "Close the diff first (esc) to pick other commits")),
        keys(", shift-d"),
        check(|app| assert_notice(app, "A diff is open, close it first (esc)")),
        keys(", shift-l"),
        check(|app| assert_notice(app, "A diff is open, close it first (esc)")),
    ]);
}

#[test]
fn test_compare_gives_the_branch_only_graph_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("shift-l"),
        check(|app| assert_eq!(graph_note(app).as_deref(), Some("master only"))),
        compare_feat(),
        keys("esc"),
        check(|app| assert_eq!(graph_note(app).as_deref(), Some("master only"))),
    ]);
}

#[test]
fn test_no_branch_only_of_a_branch_without_commits() {
    use crate::keybind::UserEvent::BranchOnly;
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    git(&path, &["checkout", "-q", "--orphan", "fresh"]);
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        check(|app| assert!(!app.view.keys().takes(BranchOnly))),
        // fresh, first in the branches, then feat
        keys("1 j"),
        check(|app| assert!(!app.sidebar_keys().takes(BranchOnly))),
        keys("j shift-l"),
        check(|app| assert_eq!(graph_note(app).as_deref(), Some("feat only"))),
        // back to all of them from the graph
        keys("2"),
        check(|app| assert!(app.view.keys().takes(BranchOnly))),
        keys("shift-l"),
        check(|app| assert_eq!(graph_note(app), None)),
    ]);
}

#[test]
fn test_branch_only_of_the_sidebar_branch() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        // feat, under master
        keys("1 j shift-l"),
        check(|app| assert_eq!(graph_note(app).as_deref(), Some("feat only"))),
        // another branch shows its commits instead, the same one all of them again
        keys("k shift-l"),
        check(|app| assert_eq!(graph_note(app).as_deref(), Some("master only"))),
        keys("shift-l"),
        check(|app| assert_eq!(graph_note(app), None)),
        // a stash or the headers, the current branch's
        keys("g shift-l"),
        check(|app| {
            assert!(matches!(
                app.shell.graph_filter,
                GraphFilter::BranchOnly(None)
            ));
            assert_eq!(graph_note(app).as_deref(), Some("master only"));
        }),
        keys("j shift-l"),
        check(|app| assert_eq!(graph_note(app).as_deref(), Some("feat only"))),
        // its commits only are loaded
        check(|app| {
            let subjects: Vec<&str> = app
                .repository
                .all_commits()
                .iter()
                .map(|c| c.subject.as_str())
                .collect();
            assert_eq!(subjects, ["feat1", "c1"]);
        }),
    ]);
}

#[test]
fn test_space_in_the_sidebar_checks_out_and_applies() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    std::fs::write(path.join("f.txt"), "stashed\n").unwrap();
    git(&path, &["stash", "push", "-q", "-m", "wip"]);
    let mut harness = Harness::new(&path, launch());
    let command = |app: &App| {
        let popup = app.shell.dialog.as_ref().expect("no popup");
        popup.dialog.command().unwrap().unwrap().commands[0].to_string()
    };
    harness.run(vec![
        // a branch is checked out, as on the graph
        keys("1 j space"),
        check(move |app| assert_eq!(command(app), "checkout feat")),
        keys("esc"),
        // a stash is applied
        keys("] ] ] j space"),
        check(move |app| assert_eq!(command(app), "stash apply stash@{0}")),
        // in the popup, v no longer toggles the checkbox; space does
        keys("v"),
        check(move |app| assert_eq!(command(app), "stash apply stash@{0}")),
        keys("space"),
        check(move |app| assert_eq!(command(app), "stash apply --index stash@{0}")),
    ]);
}

#[test]
fn test_space_on_a_stash_in_the_graph_applies_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    std::fs::write(path.join("f.txt"), "stashed\n").unwrap();
    git(&path, &["stash", "push", "-q", "-m", "wip"]);
    let mut harness = Harness::new(&path, launch());
    let command = |app: &App| {
        let popup = app.shell.dialog.as_ref().expect("no popup");
        popup.dialog.command().unwrap().unwrap().commands[0].to_string()
    };
    // space listed once, under what it does
    let space = |short: &'static str| {
        check(move |app| {
            let hints = app.view.keys().hints();
            let with_space: Vec<&str> = hints
                .iter()
                .filter(|(events, _)| events.contains(&crate::keybind::UserEvent::Select))
                .map(|(_, s)| s.as_str())
                .collect();
            assert_eq!(with_space, [short]);
        })
    };
    harness.run(vec![
        // the stash, the newest commit
        keys("g g"),
        space("apply"),
        keys("space"),
        check(move |app| assert_eq!(command(app), "stash apply stash@{0}")),
        keys("esc"),
        // the same from the commit list over the details
        keys("enter 2 space"),
        check(move |app| assert_eq!(command(app), "stash apply stash@{0}")),
        keys("esc esc"),
        // other commits are checked out, feat1 past the one checked out
        keys("j j"),
        space("checkout"),
    ]);
}

#[test]
fn test_graph_page_opens_on_the_sidebar() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("tab tab"),
        check(|app| assert!(app.shell.sidebar_focused())),
        // shrunk out of the way, it stays so
        keys("1 tab tab"),
        check(|app| assert!(!app.shell.sidebar_focused())),
    ]);
}

#[test]
fn test_sidebar_search_opens_folders_and_steps_through_matches() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    git(&path, &["branch", "fix/deep"]);
    git(&path, &["tag", "deep-tag"]);
    let mut harness = Harness::new(&path, launch());
    let selected = |expected: RefSelection| {
        check(move |app| assert_eq!(app.shell.sidebar.selection(), expected))
    };
    let branch = |name: &str| RefSelection::Ref(NamedRef::Branch(name.into()));
    let tag = |name: &str| RefSelection::Ref(NamedRef::Tag(name.into()));
    harness.run(vec![
        // in the closed fix folder: opened to select it
        keys("1 /"),
        typing("deep"),
        selected(branch("fix/deep")),
        keys("enter"),
        check(|app| {
            let said = &app.shell.app_status.status_line;
            assert!(
                matches!(
                    said,
                    StatusLine::SearchResult {
                        outcome: SearchOutcome::Found {
                            index: 1,
                            total: 2,
                            ..
                        },
                        ..
                    }
                ),
                "{said:?}"
            );
        }),
        // the tag, in the closed tags section, and round to the branch
        keys("n"),
        selected(tag("deep-tag")),
        keys("n"),
        selected(branch("fix/deep")),
        keys("shift-n"),
        selected(tag("deep-tag")),
        // esc clears the search first, then leaves the sidebar
        keys("esc"),
        check(|app| assert!(app.shell.sidebar.query().search().is_none())),
        check(|app| assert!(app.shell.sidebar_focused())),
        keys("esc"),
        check(|app| assert!(!app.shell.sidebar_focused())),
    ]);
}

#[test]
fn test_tabs_from_git_show_as_spaces() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    std::fs::write(path.join("x\ty.txt"), "x\n").unwrap();
    git(&path, &["add", "-A"]);
    git(
        &path,
        &["commit", "-q", "-m", "sub\tject\n\n\tindented body"],
    );
    std::fs::write(path.join("f.txt"), "stashed\n").unwrap();
    git(&path, &["stash", "push", "-q", "-m", "st\tash"]);
    std::fs::write(path.join("un\ttracked"), "u\n").unwrap();
    let shows = |texts: &'static [&'static str]| {
        screen(move |screen| {
            for text in texts {
                assert!(screen.contains(text), "{text:?} not in\n{screen}");
            }
        })
    };
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        // ratatui drops a tab: drawn as four spaces, like the diffs and the previews
        shows(&["sub    ject", "st    ash"]),
        keys("j enter"),
        shows(&["sub    ject", "    indented body", "x    y.txt"]),
        keys("esc"),
        keys("1 ] ] ] l"),
        shows(&["On master: st    ash"]),
        keys("esc tab"),
        shows(&["un    tracked"]),
    ]);
}

#[test]
fn test_paste_is_typed() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    std::fs::write(path.join("f.txt"), "changed\n").unwrap();
    std::fs::write(path.join("a.txt"), "new\n").unwrap();
    let paste = |text: &'static str| {
        vec![Step::Act(Box::new(move |app: &mut App| {
            app.ec.send(AppEvent::Paste(text.into()));
        }))]
    };
    let shows = |texts: &'static [&'static str]| {
        screen(move |screen| {
            for text in texts {
                assert!(screen.contains(text), "{text:?} not in\n{screen}");
            }
        })
    };
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        // on the graph, nothing is typed: the text isn't taken for keys, `q` quitting
        paste("q"),
        check(|app| assert!(matches!(app.view, View::List(_)))),
        // into a search, applied once it is all typed
        keys("/"),
        paste("feat"),
        check(|app| {
            let View::List(view) = &app.view else {
                panic!("no commit list")
            };
            let state = view.as_list_state();
            let hash = state.selected_commit_hash();
            assert_eq!(app.repository.commit(hash).unwrap().subject, "feat1");
        }),
        // a newline is a space instead of applying it
        paste("c2\nx"),
        check(|app| assert!(app.is_typing())),
        keys("esc"),
        // into the filter of the files of the local changes page too
        keys("tab /"),
        paste("f.t"),
        check(|app| {
            let said = &app.shell.app_status.status_line;
            assert!(
                matches!(said, StatusLine::Input { message, .. } if message == "filter: f.t"),
                "{said:?}"
            );
        }),
        keys("esc"),
        // into the commit message, as typed: the tab and the new lines in the description, without
        // the tab switching to the summary
        keys("3"),
        // terminals send a newline as \r, some as \r\n
        paste("summary\rfn main() {\r\n\tcall();\n}"),
        keys("esc"),
        shows(&["summary", "fn main() {", "    call();"]),
        check(|app| {
            let View::LocalChanges(view) = &app.view else {
                panic!("no local changes page")
            };
            assert_eq!(
                view.context().commit_draft_message(),
                "summary\n\nfn main() {\n\tcall();\n}"
            );
        }),
    ]);
}

#[test]
fn test_half_page_jump_in_the_sidebar() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    for i in 0..60 {
        git(&path, &["branch", &format!("b{i:02}")]);
    }
    let mut harness = Harness::new(&path, launch());
    let selected = |name: &'static str| {
        check(move |app| {
            let selection = app.shell.sidebar.selection();
            assert_eq!(selection, RefSelection::Ref(NamedRef::Branch(name.into())));
        })
    };
    harness.run(vec![
        keys("1 g j"),
        selected("b00"),
        // half of the 36 rows shown
        keys("ctrl-d"),
        selected("b18"),
        keys("ctrl-d ctrl-u"),
        selected("b18"),
    ]);
}

#[test]
fn test_half_page_jump_in_the_local_changes_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    for i in 0..30 {
        std::fs::write(path.join(format!("u{i:02}.txt")), "x\n").unwrap();
    }
    let launch = Launch {
        page: StartPage::LocalChanges,
        ..launch()
    };
    let mut harness = Harness::new(&path, launch);
    let first = Rc::new(RefCell::new(None));
    let jumped = Rc::new(RefCell::new(None));
    let record_first = first.clone();
    let record_jumped = jumped.clone();
    let compare = (first.clone(), jumped.clone());
    let back = first.clone();
    harness.run(vec![
        check(move |app| *record_first.borrow_mut() = local_change_path(app)),
        // half of the rows shown, like the other lists
        keys("ctrl-d"),
        check(move |app| *record_jumped.borrow_mut() = local_change_path(app)),
        check(move |_app| {
            let (first, jumped) = &compare;
            assert!(first.borrow().is_some());
            assert_ne!(*jumped.borrow(), *first.borrow());
        }),
        keys("ctrl-u"),
        check(move |app| assert_eq!(local_change_path(app), *back.borrow())),
    ]);
}

#[test]
fn test_half_page_jump_in_the_details_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("work");
    std::fs::create_dir(&path).unwrap();
    git(&path, &["init", "-q", "-b", "master"]);
    git(&path, &["config", "user.name", "t"]);
    git(&path, &["config", "user.email", "t@t"]);
    git(&path, &["config", "commit.gpgsign", "false"]);
    for i in 0..30 {
        std::fs::write(path.join(format!("f{i:02}.txt")), "x\n").unwrap();
    }
    git(&path, &["add", "."]);
    git(&path, &["commit", "-q", "-m", "many"]);
    let mut harness = Harness::new(&path, launch());
    let first = Rc::new(RefCell::new(None));
    let jumped = Rc::new(RefCell::new(None));
    let record_first = first.clone();
    let record_jumped = jumped.clone();
    let compare = (first.clone(), jumped.clone());
    let back = first.clone();
    harness.run(vec![
        // the files of the changes tab, focused
        keys("4"),
        check(move |app| *record_first.borrow_mut() = detail_path(app)),
        keys("ctrl-d"),
        check(move |app| *record_jumped.borrow_mut() = detail_path(app)),
        check(move |_app| {
            let (first, jumped) = &compare;
            assert!(first.borrow().is_some());
            assert_ne!(*jumped.borrow(), *first.borrow());
        }),
        keys("ctrl-u"),
        check(move |app| assert_eq!(detail_path(app), *back.borrow())),
    ]);
}

#[test]
fn test_page_keys_move_the_details_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("work");
    std::fs::create_dir(&path).unwrap();
    git(&path, &["init", "-q", "-b", "master"]);
    git(&path, &["config", "user.name", "t"]);
    git(&path, &["config", "user.email", "t@t"]);
    git(&path, &["config", "commit.gpgsign", "false"]);
    for i in 0..30 {
        std::fs::write(path.join(format!("f{i:02}.txt")), "x\n").unwrap();
    }
    git(&path, &["add", "."]);
    git(&path, &["commit", "-q", "-m", "many"]);
    let mut harness = Harness::new(&path, launch());
    let first = Rc::new(RefCell::new(None));
    let jumped = Rc::new(RefCell::new(None));
    let record_first = first.clone();
    let record_jumped = jumped.clone();
    let compare = (first.clone(), jumped.clone());
    harness.run(vec![
        // the files tab, its tree focused: the page keys move the files, not the hidden info tab
        keys("5"),
        draw(),
        check(move |app| *record_first.borrow_mut() = detail_path(app)),
        keys("ctrl-f"),
        check(move |app| *record_jumped.borrow_mut() = detail_path(app)),
        check(move |_app| {
            let (first, jumped) = &compare;
            assert!(first.borrow().is_some());
            assert_ne!(*jumped.borrow(), *first.borrow());
        }),
    ]);
}

#[test]
fn test_fold_merge() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    git(
        &path,
        &["merge", "-q", "--no-ff", "--no-edit", "-s", "ours", "feat"],
    );
    git(&path, &["branch", "-q", "-d", "feat"]);
    let mut harness = Harness::new(&path, launch());
    let shown = |count: usize, folded: usize| {
        check(move |app| {
            let View::List(view) = &app.view else {
                panic!("no commit list")
            };
            assert_eq!(view.as_list_state().position(), (1, count));
            assert_eq!(app.shell.folded_merges().len(), folded);
        })
    };
    harness.run(vec![
        // the merge, selected; right does nothing while it is unfolded
        keys("l"),
        shown(4, 0),
        // feat1, only it brought in, goes
        keys("h"),
        shown(3, 1),
        keys("h"),
        shown(3, 1),
        keys("l"),
        shown(4, 0),
    ]);
    // the graph is drawn again, the repository isn't loaded again
    assert_eq!(harness.loads.get(), 1);
}

#[test]
fn test_fold_merge_laid_out_before_a_reload() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    // a branch of its own, below the merge, to delete in another terminal
    let date = "@1700000180 +0000";
    let output = crate::git::git_command()
        .args([
            "commit-tree",
            "master^{tree}",
            "-p",
            "master",
            "-m",
            "other1",
        ])
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .current_dir(&path)
        .output()
        .unwrap();
    git(
        &path,
        &[
            "branch",
            "other",
            String::from_utf8(output.stdout).unwrap().trim(),
        ],
    );
    git(
        &path,
        &["merge", "-q", "--no-ff", "--no-edit", "-s", "ours", "feat"],
    );
    git(&path, &["branch", "-q", "-d", "feat"]);
    // the repository a layout was made of, and the layout
    type Stale = Option<(Arc<crate::git::Repository>, GraphLayout)>;
    let stale: Rc<RefCell<Stale>> = Rc::default();
    let (keep, send) = (stale.clone(), stale);
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("h"),
        check(|app| assert_eq!(app.shell.folded_merges().len(), 1)),
        // the graph laid out on another thread meanwhile, from the repository before the reload
        vec![Step::Act(Box::new(move |app: &mut App| {
            let folded = app.shell.folded_merges().clone();
            let layout = GraphLayout::calc(app.repository, None, &folded);
            keep.replace(Some((Arc::clone(app.repository), layout)));
        }))],
        outside_git(&["branch", "-q", "-D", "other"]),
        keys("ctrl-r"),
        check(|app| {
            assert!(app
                .repository
                .all_commits()
                .iter()
                .all(|c| c.subject != "other1"))
        }),
        // landing after the reload, laid out of commits it no longer has
        vec![Step::Act(Box::new(move |app: &mut App| {
            let (from, layout) = send.take().unwrap();
            let layout = Box::new(layout);
            app.ec.send(AppEvent::Regraphed { from, layout });
        }))],
        check(|app| assert_eq!(app.shell.folded_merges().len(), 1)),
    ]);
}

#[test]
fn test_fold_merge_draws_over_the_last_frame() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    git(
        &path,
        &["merge", "-q", "--no-ff", "--no-edit", "-s", "ours", "feat"],
    );
    git(&path, &["branch", "-q", "-d", "feat"]);
    // what the next frame clears before it is drawn
    let clears = |clear: PendingClear| {
        vec![Step::Act(Box::new(move |app: &mut App| {
            assert_eq!(app.frame_clear(), clear);
        }))]
    };
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        // a frame drawn, from scratch
        clears(PendingClear::Screen),
        clears(PendingClear::None),
        keys("h"),
        // the rows that changed are drawn again over it, not the whole screen
        check(|app| assert_eq!(app.shell.folded_merges().len(), 1)),
        clears(PendingClear::None),
    ]);
}

#[test]
fn test_reload_draws_over_the_last_frame() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let clears = |clear: PendingClear| {
        vec![Step::Act(Box::new(move |app: &mut App| {
            assert_eq!(app.frame_clear(), clear);
        }))]
    };
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        clears(PendingClear::Screen),
        clears(PendingClear::None),
        // a commit made outside, the repository loaded again
        outside_git(&["commit", "-q", "--allow-empty", "-m", "c3"]),
        tick(),
        clears(PendingClear::None),
    ]);
    assert_eq!(harness.loads.get(), 2);
}

#[test]
fn test_fold_merge_while_reloading_or_laying_out() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    git(
        &path,
        &["merge", "-q", "--no-ff", "--no-edit", "-s", "ours", "feat"],
    );
    git(&path, &["branch", "-q", "-d", "feat"]);
    let mut harness = Harness::new(&path, launch());
    let shown = |count: usize, folded: usize| {
        check(move |app| {
            assert!(!app.is_reloading() && !app.is_loading());
            let View::List(view) = &app.view else {
                panic!("no commit list")
            };
            assert_eq!(view.as_list_state().position(), (1, count));
            assert_eq!(app.shell.folded_merges().len(), folded);
        })
    };
    harness.run(vec![
        // the reload lays the graph out with the merge unfolded, as it was asked before the
        // fold: that layout isn't used, the merge stays folded
        keys_at_once("ctrl-r h"),
        shown(3, 1),
        // unfolded while the folded layout runs: that one is dropped
        keys_at_once("l h l"),
        shown(4, 0),
    ]);
}

#[test]
fn test_enter_on_a_ref_unfolds_the_merge_hiding_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    git(
        &path,
        &["merge", "-q", "--no-ff", "--no-edit", "-s", "ours", "feat"],
    );
    let feat1 = git(&path, &["rev-parse", "feat"]).trim().to_string();
    let mut harness = Harness::new(&path, launch());
    let shown = |count: usize, folded: usize| {
        check(move |app| {
            let View::List(view) = &app.view else {
                panic!("no commit list")
            };
            assert_eq!(view.as_list_state().position().1, count);
            assert_eq!(app.shell.folded_merges().len(), folded);
        })
    };
    harness.run(vec![
        // feat1 goes with the merge folded, though feat points at it
        keys("h"),
        shown(3, 1),
        // moving to feat in the sidebar leaves the merge folded
        keys("1 j"),
        check(|app| {
            assert_eq!(
                app.shell.sidebar.selected_git_name().as_deref(),
                Some("refs/heads/feat")
            )
        }),
        shown(3, 1),
        // enter unfolds it and selects feat1 once the graph is drawn again
        keys("enter"),
        shown(4, 0),
        check(move |app| {
            let View::List(view) = &app.view else {
                panic!("no commit list")
            };
            assert_eq!(view.as_list_state().selected_commit_hash().as_str(), feat1);
        }),
    ]);
}

#[test]
fn test_fold_merge_with_branch_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    git(
        &path,
        &["merge", "-q", "--no-ff", "--no-edit", "-s", "ours", "feat"],
    );
    git(&path, &["branch", "-q", "-d", "feat"]);
    let mut harness = Harness::new(&path, launch());
    let shown = |count: usize, note: Option<&'static str>| {
        check(move |app| {
            let View::List(view) = &app.view else {
                panic!("no commit list")
            };
            assert_eq!(view.as_list_state().position().1, count);
            assert_eq!(graph_note(app).as_deref(), note);
        })
    };
    harness.run(vec![
        keys("h"),
        shown(3, None),
        // the merge stays folded in the history of the branch, and back in all of it
        keys("shift-l"),
        shown(3, Some("master only")),
        keys("l"),
        shown(4, Some("master only")),
        keys("h"),
        shown(3, Some("master only")),
        keys("shift-l"),
        shown(3, None),
        check(|app| assert_eq!(app.shell.folded_merges().len(), 1)),
    ]);
    // the branch only view loads the repository again, folding draws the graph again only
    assert_eq!(harness.loads.get(), 3);
}

#[test]
fn test_blame_and_timeline_open_once_loaded() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let head = git(&path, &["rev-parse", "HEAD"]).trim().to_string();
    let mut harness = Harness::new(&path, launch());
    let open = |event: AppEvent| {
        vec![Step::Act(Box::new(move |app: &mut App| {
            app.handle_event(event).unwrap();
            // the history loads on another thread: no window yet, and nothing said in the status
            // line while it is quick, see `shows_loading`
            assert!(app.shell.overlay.is_none());
            assert!(matches!(app.shell.app_status.status_line, StatusLine::None));
            assert_eq!(app.background_activity(), None);
        }))]
    };
    let opened = |name: &'static str| {
        let cleared = check(|app| {
            assert!(matches!(app.shell.app_status.status_line, StatusLine::None));
        });
        [overlay(name), cleared]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
    };
    let commit = CommitHash::from(head.as_str());
    harness.run(vec![
        open(AppEvent::OpenBlame {
            commit: commit.clone(),
            path: "f.txt".into(),
            line: None,
        }),
        opened("blame"),
        keys("esc esc"),
        open(AppEvent::OpenTimeline {
            commit,
            path: "f.txt".into(),
        }),
        opened("timeline"),
        keys("esc"),
        // a file with no history says so once loaded, opening nothing
        open(AppEvent::OpenTimeline {
            commit: CommitHash::from(head.as_str()),
            path: "nothing.txt".into(),
        }),
        overlay("none"),
        check(|app| assert_notice(app, "nothing.txt has no history")),
    ]);
}

// Moving in the commit list, the details of the commit before stay shown until those of the one
// selected load, the title saying so once that takes long.
#[test]
fn test_details_stay_while_the_next_load() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let tips =
        ["master", "feat"].map(|branch| git(&path, &["rev-parse", branch]).trim().to_string());
    // the tips whose full hash is on the screen, as the info shows it
    fn shown(tips: &[String], screen: &str) -> Vec<String> {
        tips.iter()
            .filter(|hash| screen.contains(hash.as_str()))
            .cloned()
            .collect()
    }
    let after = tips.clone();
    let draw_now = |app: &mut App| -> String {
        let backend = ratatui::backend::TestBackend::new(SIZE.0, SIZE.1);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let keys = app.keys_popup_keys();
        let frame = terminal.draw(|f| app.render(f, &keys)).unwrap();
        let rows = frame.buffer.content.chunks(SIZE.0 as usize);
        rows.map(|r| r.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        // the info of the first tip, its full hash
        keys("3 2"),
        vec![Step::Act(Box::new(move |app: &mut App| {
            let before = shown(&tips, &draw_now(app));
            assert_eq!(before.len(), 1, "one commit's details");
            app.handle_event(AppEvent::Key(parse_key_event("j").unwrap()))
                .unwrap();
            app.prepare_render(Rect::new(0, 0, SIZE.0, SIZE.1));
            // loaded already on a fast machine, nothing to see in between
            if !app.is_loading() {
                return;
            }
            let screen = draw_now(app);
            assert_eq!(shown(&tips, &screen), before);
            // no note while it is quick, see `shows_loading`
            assert!(!screen.contains("loading…"), "{screen}");
        }))],
        // once loaded, the other tip's
        screen(move |screen| {
            assert!(!screen.contains("loading…"));
            assert_eq!(shown(&after, screen).len(), 1);
        }),
    ]);
}

// Moving to another file of the local changes, the diff before stays shown until the file's
// loads, the title saying so once that takes long.
#[test]
fn test_local_changes_diff_stays_while_the_next_loads() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    std::fs::write(path.join("g.txt"), "g\n").unwrap();
    git(&path, &["add", "g.txt"]);
    git(&path, &["commit", "-q", "-m", "g"]);
    std::fs::write(path.join("f.txt"), "FFFF\n").unwrap();
    std::fs::write(path.join("g.txt"), "GGGG\n").unwrap();
    let draw_now = |app: &mut App| -> String {
        let backend = ratatui::backend::TestBackend::new(SIZE.0, SIZE.1);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let keys = app.keys_popup_keys();
        let frame = terminal.draw(|f| app.render(f, &keys)).unwrap();
        let rows = frame.buffer.content.chunks(SIZE.0 as usize);
        rows.map(|r| r.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let launch = Launch {
        page: StartPage::LocalChanges,
        ..launch()
    };
    let mut harness = Harness::new(&path, launch);
    harness.run(vec![
        // the diff of all the files, then of f.txt alone
        check(|app| assert_eq!(local_change_path(app), Some(":/".into()))),
        vec![Step::Act(Box::new(move |app: &mut App| {
            let screen = draw_now(app);
            assert!(
                screen.contains("FFFF") && screen.contains("GGGG"),
                "{screen}"
            );
            app.handle_event(AppEvent::Key(parse_key_event("j").unwrap()))
                .unwrap();
            app.prepare_render(Rect::new(0, 0, SIZE.0, SIZE.1));
            // loaded already on a fast machine, nothing to see in between
            if !app.is_loading() {
                return;
            }
            let screen = draw_now(app);
            // no note while it is quick, see `shows_loading`
            assert!(
                screen.contains("GGGG") && !screen.contains("loading…"),
                "{screen}"
            );
        }))],
        screen(|screen| {
            assert!(
                screen.contains("FFFF") && !screen.contains("GGGG"),
                "{screen}"
            );
            assert!(!screen.contains("loading…"));
        }),
    ]);
}

#[test]
fn test_blame_shows_a_commit_on_the_graph_and_checks_one_out() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let c1 = git(&path, &["rev-parse", "master~1"]).trim().to_string();
    let mut harness = Harness::new(&path, launch());
    let selected = move |subject: &'static str| {
        check(move |app| {
            let View::Detail(view) = &app.view else {
                panic!("no details")
            };
            let commit = view.as_list_state().selected_commit_hash();
            assert_eq!(app.repository.commit(commit).unwrap().subject, subject);
        })
    };
    harness.run(vec![
        // f.txt in the files of c2, the latest commit
        keys("5 shift-b"),
        overlay("blame"),
        // the commit of its first line, c1, on the graph, the details still open
        keys("enter"),
        overlay("none"),
        selected("c1"),
        // checking out the commit shown, c1 again, closes the blame once confirmed
        keys("k 5 shift-b ["),
        keys("1 space"),
        overlay("blame"),
        keys("enter"),
        overlay("none"),
        check(move |app| assert_eq!(app.repository.head_commit().unwrap().as_str(), c1)),
    ]);
}

#[test]
fn test_keys_finishing_a_sequence_popped_up() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    let popup = |expected: &'static [(&'static str, &'static str)]| {
        check(move |app| {
            let shown: Vec<(String, String)> = app
                .keys_popup_keys()
                .into_iter()
                .map(|k| (k.keys, k.action))
                .collect();
            let expected: Vec<(String, String)> = expected
                .iter()
                .map(|(k, a)| (k.to_string(), a.to_string()))
                .collect();
            assert_eq!(shown, expected);
        })
    };
    harness.run(vec![
        // y on the commit list: what copying can go on with, the keys popup hidden
        popup(&[]),
        keys("y"),
        popup(&[
            ("f", "copy full hash"),
            ("l", "copy link"),
            ("y", "copy hash"),
        ]),
        keys("esc"),
        popup(&[]),
        // the keys popup names them as typed there
        keys("shift-h"),
        check(|app| {
            let keys = app.keys_popup_keys();
            let copy = keys.iter().find(|k| k.action == "copy hash").unwrap();
            assert_eq!(copy.keys, "yy");
        }),
        keys("shift-h"),
        // m with no operation in progress starts nothing on the local changes page
        keys("tab m"),
        check(|app| assert!(app.shell.app_status.pending_keys.is_empty())),
        popup(&[]),
    ]);
}

#[test]
fn test_keys_list_runs_the_action() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        // checking out has no key but space; filtered, run from the list, it asks to confirm
        keys("? /"),
        typing("Check out the branch"),
        keys("enter enter"),
        overlay("none"),
        check(|app| assert!(app.shell.dialog.is_some())),
        keys("esc"),
        // on another panel's action, enter does nothing
        keys("? /"),
        typing("Stash all changes"),
        keys("enter enter"),
        overlay("help"),
        check(|app| assert!(app.shell.dialog.is_none())),
    ]);
}

#[test]
fn test_keys_popup_over_the_blame_and_the_timeline() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    let popup_has = |keys: &'static [&'static str]| {
        check(move |app| {
            let shown: Vec<String> = app
                .keys_popup_keys()
                .into_iter()
                .map(|k| k.action)
                .collect();
            for key in keys {
                assert!(shown.iter().any(|d| d == key), "no {key:?} in {shown:?}");
            }
        })
    };
    harness.run(vec![
        keys("5 shift-b shift-h"),
        popup_has(&[
            "older/newer commit",
            "show on graph",
            "checkout",
            "search",
            "back",
        ]),
        keys("esc esc shift-t"),
        popup_has(&["into the diff", "copy hash", "move"]),
        // typing a search, the popup gives way
        keys("/"),
        popup_has(&[]),
        check(|app| assert!(app.keys_popup_keys().is_empty())),
        // over the command log, not the keys list, which lists them itself
        keys("esc esc ? /"),
        typing("git commands run"),
        keys("enter enter"),
        popup_has(&["move", "copy", "close"]),
        keys("esc ?"),
        check(|app| assert!(app.keys_popup_keys().is_empty())),
    ]);
}

#[test]
fn test_search_options_toggled_in_any_search() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    // toggled or not from the config's
    let options = |smart_case: bool, fuzzy: bool| {
        check(move |app| {
            let options = app.ctx.search_options.get();
            let config = &app.ctx.core_config.search;
            let toggled = (
                options.smart_case != config.smart_case,
                options.fuzzy != config.fuzzy,
            );
            assert_eq!(toggled, (smart_case, fuzzy));
            // the commit list searches with them too
            let View::Detail(view) = &app.view else {
                panic!("no details")
            };
            assert_eq!(view.as_list_state().search_options(), options);
        })
    };
    harness.run(vec![
        // in the files tree, both
        keys("5 / ctrl-g ctrl-x"),
        options(true, true),
        // in the preview, searched as text, only smart case
        keys("esc enter / ctrl-g ctrl-x"),
        options(false, true),
    ]);
}

#[test]
fn test_keys_over_everything_work_over_the_blame_and_the_timeline() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("5 shift-b l"),
        vec![Step::Event(Box::new(AppEvent::NotifyWarn("warned".into())))],
        check(|app| assert_notice(app, "warned")),
        // dismissed, and the long lines wrapped, without closing the blame
        keys(", w"),
        overlay("blame"),
        check(|app| {
            assert!(notices(app).is_empty());
            assert!(app.ctx.wrap.get());
        }),
        // the commits resized, the focus moved up to the commit shown, like on the pages
        draw(),
        keys("alt-right"),
        check(|app| assert_ne!(app.ctx.layout.borrow().size(Panel::BlameCommits, 0), 0)),
        keys("ctrl-up shift-h"),
        check(|app| {
            let shown: Vec<String> = app
                .keys_popup_keys()
                .into_iter()
                .map(|k| k.action)
                .collect();
            assert!(shown.iter().any(|d| d == "to the lines"), "{shown:?}");
        }),
        // and in the timeline, into the diff
        keys("shift-h esc shift-t"),
        draw(),
        keys("alt-left ctrl-right shift-h"),
        overlay("timeline"),
        check(|app| {
            assert_ne!(app.ctx.layout.borrow().size(Panel::TimelineCommits, 0), 0);
            let shown: Vec<String> = app
                .keys_popup_keys()
                .into_iter()
                .map(|k| k.action)
                .collect();
            assert!(shown.iter().any(|d| d == "select lines"), "{shown:?}");
        }),
    ]);
}

#[test]
fn test_esc_clears_the_picks_before_the_search() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    let state = |picked: usize, searched: bool| {
        check(move |app| {
            let state = app.view.as_list_state().unwrap();
            let shown = (state.picked_commit_hashes().len(), state.search_applied());
            assert_eq!(shown, (picked, searched));
        })
    };
    harness.run(vec![
        keys("v /"),
        typing("c"),
        keys("enter"),
        state(1, true),
        keys("esc"),
        state(0, true),
        keys("esc"),
        state(0, false),
    ]);
}

// The branch only graph is left last: after the search being typed, the picks and the search.
#[test]
fn test_esc_shows_all_commits_after_the_picks_and_the_search() {
    use crate::keybind::UserEvent::Cancel;
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    let state = |picked: usize, searched: bool, note: Option<&'static str>| {
        check(move |app| {
            let state = app.view.as_list_state().unwrap();
            let shown = (state.picked_commit_hashes().len(), state.search_applied());
            assert_eq!(shown, (picked, searched));
            assert_eq!(graph_note(app).as_deref(), note);
        })
    };
    let esc_means = |short: Option<&'static str>| {
        check(move |app| assert_eq!(app.view.keys().describe(Cancel), short))
    };
    harness.run(vec![
        keys("shift-l v /"),
        typing("c"),
        keys("esc"),
        check(|app| assert!(!app.is_typing())),
        state(1, false, Some("master only")),
        keys("/"),
        typing("c"),
        keys("enter"),
        state(1, true, Some("master only")),
        keys("esc"),
        state(0, true, Some("master only")),
        esc_means(Some("clear search")),
        keys("esc"),
        state(0, false, Some("master only")),
        esc_means(Some("show all")),
        keys("esc"),
        state(0, false, None),
        esc_means(None),
    ]);
}

// With the commit list focused over the details or a diff of picked commits, L and Esc show all
// the commits again; the diff stays, Esc closing it next.
#[test]
fn test_branch_only_ends_from_the_list_over_the_details() {
    use crate::keybind::UserEvent::Cancel;
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let state = |detail: bool, note: Option<&'static str>, diff: bool| {
        check(move |app| {
            assert_eq!(matches!(app.view, View::Detail(_)), detail);
            assert_eq!(graph_note(app).as_deref(), note);
            assert_eq!(compared(app).is_some(), diff);
        })
    };
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("shift-l enter 2"),
        check(|app| assert_eq!(app.view.keys().describe(Cancel), Some("show all"))),
        keys("esc"),
        state(true, None, false),
        keys("shift-l"),
        state(true, Some("master only"), false),
    ]);
    for end in ["shift-l", "esc"] {
        let mut harness = Harness::new(&path, launch());
        harness.run(vec![
            keys("shift-l v j v shift-d 2"),
            state(true, Some("master only"), true),
            check(|app| assert_eq!(app.view.keys().describe(Cancel), Some("show all"))),
            keys(end),
            state(true, None, true),
            check(|app| {
                assert_eq!(
                    app.view
                        .as_list_state()
                        .unwrap()
                        .picked_commit_hashes()
                        .len(),
                    2
                )
            }),
            // showing a branch's only stays refused, it could hide the picked commits
            keys("shift-l"),
            check(|app| assert_notice(app, "A diff is open, close it first (esc)")),
            keys("esc"),
            state(false, None, false),
        ]);
    }
}

// The commit list beside the details clears the picks first too: it used to clear the search,
// its hint saying it cleared the picks.
#[test]
fn test_esc_clears_the_picks_before_the_search_beside_the_details() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    let state = |picked: usize, searched: bool| {
        check(move |app| {
            assert!(matches!(app.view, View::Detail(_)));
            let state = app.view.as_list_state().unwrap();
            let shown = (state.picked_commit_hashes().len(), state.search_applied());
            assert_eq!(shown, (picked, searched));
        })
    };
    harness.run(vec![
        keys("v /"),
        typing("c"),
        keys("enter v enter 2"),
        state(1, true),
        keys("esc"),
        state(0, true),
        keys("esc"),
        state(0, false),
    ]);
}

// The subject of the commit selected in the commit list.
fn selected_subject(app: &App) -> String {
    let hash = app.view.as_list_state().unwrap().selected_commit_hash();
    app.repository.commit(hash).unwrap().subject.clone()
}

fn commits_picked(count: usize) -> Vec<Step> {
    check(move |app| {
        let state = app.view.as_list_state().unwrap();
        assert_eq!(state.picked_commit_hashes().len(), count);
    })
}

// The command of the popup open.
fn popup_command(app: &App) -> String {
    let popup = app.shell.dialog.as_ref().expect("no popup");
    popup.dialog.command().unwrap().unwrap().commands[0].to_string()
}

#[test]
fn test_click_on_a_commit_opens_its_details() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("1"),
        mouse_on("c1", TestGesture::Click),
        check(|app| assert!(matches!(app.view, View::Detail(_)))),
        check(|app| assert!(!app.shell.sidebar_focused())),
        check(|app| assert_eq!(selected_subject(app), "c1")),
    ]);
}

#[test]
fn test_double_click_on_a_commit_checks_it_out() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        // the first click opens the details, the second acts on what is drawn there then
        double_click("feat1"),
        check(|app| assert_eq!(selected_subject(app), "feat1")),
        check(|app| assert_eq!(popup_command(app), "checkout feat")),
    ]);
}

#[test]
fn test_ctrl_click_toggles_one_pick() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        mouse_on("c1", TestGesture::CtrlClick),
        commits_picked(1),
        check(|app| assert!(matches!(app.view, View::List(_)))),
        mouse_on("c1", TestGesture::CtrlClick),
        commits_picked(0),
    ]);
}

#[test]
fn test_drag_picks_the_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        // on through the sidebar, which picks nothing more
        mouse_on("c2", TestGesture::Drag(&["c1", "Branches"])),
        commits_picked(3),
        check(|app| assert!(!app.view.as_list_state().unwrap().is_visual())),
        check(|app| assert!(app.shell.sidebar.picked().is_empty())),
        check(|app| assert!(matches!(app.view, View::List(_)))),
    ]);
}

#[test]
fn test_double_click_on_an_unpicked_row_clears_the_picks() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("v v"),
        commits_picked(1),
        double_click("c1"),
        commits_picked(0),
        check(|app| assert!(popup_command(app).starts_with("checkout"))),
    ]);
}

// The spinner turning while git runs, or the checks every few seconds finding nothing changed,
// keep a click on what the last frame showed; they used to drop it.
#[test]
fn test_click_after_a_tick() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        vec![
            Step::Locate("c1", (0, 0)),
            // handled before the next frame
            Step::Together(vec![
                Step::Event(Box::new(AppEvent::SpinnerTick)),
                Step::Tick,
                Step::Mouse(TestGesture::Click),
            ]),
        ],
        check(|app| assert!(matches!(app.view, View::Detail(_)))),
        check(|app| assert_eq!(selected_subject(app), "c1")),
    ]);
}

// A double click on a button or menu entry closing it runs nothing on the row under it then.
#[test]
fn test_double_click_on_a_button_acts_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    // rows under the buttons
    for n in 0..40 {
        git(
            &path,
            &["commit", "-q", "--allow-empty", "-m", &format!("e{n}")],
        );
    }
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("space"),
        double_click("[Cancel]"),
        check(|app| assert!(app.shell.dialog.is_none())),
        keys("enter"),
        double_click("[x]"),
        check(|app| assert!(matches!(app.view, View::List(_)))),
        check(|app| assert!(app.shell.dialog.is_none())),
        mouse_on("e38", TestGesture::RightClick),
        double_click("details"),
        check(|app| assert!(matches!(app.view, View::Detail(_)))),
        check(|app| assert!(app.shell.dialog.is_none())),
    ]);
}

#[test]
fn test_clicks_in_the_sidebar() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        // a branch: back to the graph at its commit, as enter
        mouse_on("feat", TestGesture::Click),
        check(|app| {
            let feat = RefSelection::Ref(NamedRef::Branch("feat".into()));
            assert_eq!(app.shell.sidebar.selection(), feat);
            assert!(!app.shell.sidebar_focused());
            assert_eq!(selected_subject(app), "feat1");
        }),
        double_click("feat"),
        check(|app| assert_eq!(popup_command(app), "checkout feat")),
        keys("esc"),
        // a section closes, the sidebar keeping the focus, which enter leaves where it doesn't
        mouse_on("Branches", TestGesture::Click),
        check(|app| assert!(app.shell.sidebar_focused())),
        check(|app| {
            let branches = RefSelection::Section(crate::widget::ref_list::RefSection::Branches);
            assert_eq!(app.shell.sidebar.selection(), branches);
        }),
    ]);
}

#[test]
fn test_click_while_typing_only_dismisses() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("/"),
        typing("c"),
        mouse_on("c1", TestGesture::Click),
        check(|app| assert!(!app.is_typing())),
        check(|app| assert!(!app.view.as_list_state().unwrap().search_applied())),
        check(|app| assert!(matches!(app.view, View::List(_)))),
        // the next click acts, a click and not the second of a double click
        mouse_on("c1", TestGesture::Click),
        check(|app| assert!(matches!(app.view, View::Detail(_)))),
        check(|app| assert!(app.shell.dialog.is_none())),
    ]);
}

#[test]
fn test_click_with_a_popup_open_does_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("space"),
        check(|app| assert!(app.shell.dialog.is_some())),
        mouse_on("c1", TestGesture::Click),
        mouse_on("c1", TestGesture::RightClick),
        mouse_on("c1", TestGesture::WheelDown),
        check(|app| assert!(app.shell.dialog.is_some())),
        check(|app| assert!(matches!(app.view, View::List(_)))),
        check(|app| assert_eq!(selected_subject(app), "c2")),
    ]);
}

fn popup<'a>(app: &'a App) -> &'a GitDialog {
    &app.shell.dialog.as_ref().expect("a popup").dialog
}

// [Cancel] and [Confirm] on the border of a popup close it or run it, Confirm only once it can
// run: dimmed before, a click on it does nothing.
#[test]
fn test_popup_buttons() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    let branch = |name: &'static str| {
        move |path: &Path| git(path, &["branch", "--list", name]).contains(name)
    };
    harness.run(vec![
        keys("b"),
        check(|app| assert!(popup(app).is_editing())),
        check(|app| assert!(!popup(app).can_run())),
        // without a name: typing stops, the popup stays
        mouse_on("[Confirm]", TestGesture::Click),
        check(|app| assert!(!popup(app).is_editing())),
        check(|app| assert!(notices(app).is_empty(), "{:?}", notices(app))),
        mouse_on("Name", TestGesture::Click),
        check(|app| assert!(popup(app).is_editing())),
        typing("x1"),
        check(|app| assert!(popup(app).can_run())),
        mouse_on("[Confirm]", TestGesture::Click),
        check(|app| assert!(app.shell.dialog.is_none())),
        wait_for(branch("x1")),
        keys("b"),
        typing("x2"),
        mouse_on("[Cancel]", TestGesture::Click),
        check(|app| assert!(app.shell.dialog.is_none())),
        check(move |_| assert!(!branch("x2")(&path))),
    ]);
}

// A click on a chevron of a choice goes to the next or previous option, quick clicks too.
#[test]
fn test_popup_chevrons_cycle() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    let shows = |text: &'static str| screen(move |s| assert!(s.contains(text), "{s}"));
    harness.run(vec![
        keys("shift-u"),
        shows("Mixed: keep"),
        mouse_on("›", TestGesture::Click),
        shows("Soft: keep"),
        mouse_on("‹", TestGesture::Click),
        draw(),
        vec![Step::Mouse(TestGesture::Click)],
        shows("Hard: discard"),
        check(|app| assert!(app.shell.dialog.is_some())),
    ]);
}

// A click on a field selects it: a checkbox toggles, a text field is typed in where clicked.
#[test]
fn test_popup_click_on_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    let shows = |text: &'static str| screen(move |s| assert!(s.contains(text), "{s}"));
    harness.run(vec![
        keys("b"),
        typing("abcd"),
        shows("[•] Check it out"),
        // while typing, a click on another field acts at once
        mouse_on("Check it out", TestGesture::Click),
        check(|app| assert!(!popup(app).is_editing())),
        shows("[ ] Check it out"),
        mouse_at("abcd", (2, 0), TestGesture::Click),
        check(|app| assert!(popup(app).is_editing())),
        typing("X"),
        shows("abXcd"),
        // in the text being typed, a click moves the caret
        mouse_at("abXcd", (0, 0), TestGesture::Click),
        typing("Y"),
        shows("YabXcd"),
    ]);
}

fn menu_events(app: &App) -> Vec<UserEvent> {
    let menu = app.shell.menu.as_ref().expect("a menu");
    // past the rules between the sections
    (0..usize::from(u16::MAX))
        .filter_map(|row| menu.event(row))
        .collect()
}

// The menu of a branch in the sidebar fetches and pushes it, pulls into the checked-out one only;
// a remote branch or a remote fetches its remote. Each opens its popup set to them.
#[test]
fn test_sidebar_menu_fetch_pull_push() {
    let dir = tempfile::tempdir().unwrap();
    let path = with_remote(dir.path());
    let mut harness = Harness::new(&path, launch());
    let has = |events: &'static [UserEvent], not: &'static [UserEvent]| {
        check(move |app| {
            let menu = menu_events(app);
            assert!(events.iter().all(|e| menu.contains(e)), "{menu:?}");
            assert!(!not.iter().any(|e| menu.contains(e)), "{menu:?}");
        })
    };
    use UserEvent::{Fetch, Pull, Push};
    harness.run(vec![
        // feat, then master, checked out
        mouse_at("Branches", (2, 1), TestGesture::RightClick),
        has(&[Fetch, Push], &[Pull]),
        keys("esc"),
        mouse_at("Branches", (2, 2), TestGesture::RightClick),
        has(&[Fetch, Pull, Push], &[]),
        keys("esc"),
        // nothing acting on a header, opening it being a click's
        mouse_on("Tags", TestGesture::RightClick),
        check(|app| assert!(app.shell.menu.is_none())),
        // pushing feat from its menu
        mouse_at("Branches", (2, 1), TestGesture::RightClick),
        mouse_on("push", TestGesture::Click),
        draw(),
        screen(|s| assert!(s.contains("‹ feat ›"), "{s}")),
        keys("esc"),
        mouse_on("Remotes", TestGesture::Click),
        mouse_on("origin", TestGesture::RightClick),
        has(&[Fetch], &[Pull, Push]),
    ]);
}

fn menu_has(on: &'static [UserEvent], off: &'static [UserEvent]) -> Vec<Step> {
    check(move |app| {
        let menu = menu_events(app);
        assert!(
            on.iter().all(|e| menu.contains(e)),
            "{menu:?} lacks one of {on:?}"
        );
        assert!(
            !off.iter().any(|e| menu.contains(e)),
            "{menu:?} has one of {off:?}"
        );
    })
}

// The menu of a commit lists the commands that work on it: none moving to the commit checked
// out, none rewriting a commit off its history or picking one on it; on a stash, applying it,
// diffing and copying only. Moving about, picking and searching are left to the keys.
#[test]
fn test_menu_on_commits_lists_what_works() {
    use UserEvent::*;
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    std::fs::write(path.join("s.txt"), "s\n").unwrap();
    git(&path, &["add", "s.txt"]);
    git(&path, &["stash", "-q", "-m", "stashed"]);
    let mut harness = Harness::new(&path, launch());
    let moves = &[Cancel, Search, ToggleSelect];
    harness.run(vec![
        mouse_on("c2", TestGesture::RightClick),
        menu_has(
            &[Squash, DropCommit, AmendCommit, Revert, Reset],
            &[Checkout, Merge, Rebase, CherryPick],
        ),
        menu_has(&[], moves),
        keys("esc"),
        mouse_on("feat1", TestGesture::RightClick),
        menu_has(
            &[Checkout, Merge, Rebase, CherryPick, Reset],
            &[Squash, DropCommit, AmendCommit, Revert],
        ),
        keys("esc"),
        mouse_on("stashed", TestGesture::RightClick),
        menu_has(
            &[Select, Diff, Confirm, ShortCopy],
            &[
                Merge,
                Rebase,
                CherryPick,
                Squash,
                CreateBranch,
                CreateTag,
                BranchOnly,
                Reset,
                OpenInBrowser,
                CopyLink,
            ],
        ),
        keys("esc"),
        // the keys say so, unlisted
        mouse_on("feat1", TestGesture::Click),
        keys("esc"),
        keys("s"),
        check(|app| assert_notice(app, "not on master")),
    ]);
}

// The menu of the checked-out branch in the sidebar has no checkout, merge, rebase or delete; a
// ref's commands follow its commit, and branch only is on branches alone.
#[test]
fn test_sidebar_menu_lists_what_works() {
    use UserEvent::*;
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    git(&path, &["tag", "v1", "HEAD~1"]);
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        // feat, then master
        mouse_at("Branches", (2, 1), TestGesture::RightClick),
        // the commands on a commit are the commit list's, no ref's
        menu_has(
            &[Checkout, Merge, Rebase, DeleteRef, BranchOnly],
            &[CherryPick, Squash, AmendCommit, DropCommit, Revert, Reset],
        ),
        keys("esc"),
        mouse_at("Branches", (2, 2), TestGesture::RightClick),
        menu_has(
            &[BranchOnly],
            &[Checkout, Merge, Rebase, DeleteRef, Squash, Revert, Reset],
        ),
        keys("esc"),
        // nor their keys
        keys("u"),
        keys("U"),
        keys("A"),
        keys("C"),
        check(|app| assert!(app.shell.dialog.is_none())),
        // opening a node is a click's: no menu on a header
        mouse_on("Branches", TestGesture::RightClick),
        check(|app| assert!(app.shell.menu.is_none())),
        mouse_on("Tags", TestGesture::Click),
        mouse_at("Tags", (2, 1), TestGesture::RightClick),
        menu_has(&[Checkout], &[BranchOnly, CherryPick, Revert, Reset]),
    ]);
}

// From the menu of a commit, this branch only (bound to no key) shows the branch at it, or all
// again: listed on a commit with one branch, saying so with several. Branch only does what L does.
#[test]
fn test_menu_branch_only_of_the_commit() {
    use crate::keybind::UserEvent::{BranchOnly, BranchOnlyAtCommit};
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    let filter = |expected: GraphFilter| {
        check(move |app| {
            assert_eq!(
                format!("{:?}", app.shell.graph_filter),
                format!("{expected:?}")
            )
        })
    };
    let run = |row: &'static str, entry: &'static str| {
        let mut steps = mouse_on(row, TestGesture::RightClick);
        steps.extend(mouse_on(entry, TestGesture::Click));
        steps.extend(draw());
        steps
    };
    let feat = || GraphFilter::BranchOnly(Some(NamedRef::Branch("feat".into())));
    harness.run(vec![
        mouse_on("feat1", TestGesture::RightClick),
        menu_has(&[BranchOnly, BranchOnlyAtCommit], &[]),
        // no key shown for it
        screen(|s| {
            let after = s.split("this branch only").nth(1).unwrap();
            assert!(after.trim_start().starts_with('│'), "{s}");
        }),
        keys("esc"),
        run("feat1", "this branch only"),
        filter(feat()),
        run("feat1", "this branch only"),
        filter(GraphFilter::All),
        // as L: the current branch's
        run("feat1", "branch only"),
        filter(GraphFilter::BranchOnly(None)),
        keys("shift-l"),
        filter(GraphFilter::All),
        // no branch at c1
        mouse_on("c1", TestGesture::RightClick),
        menu_has(&[BranchOnly], &[BranchOnlyAtCommit]),
        keys("esc"),
        // two at c2, to choose in the sidebar
        outside_git(&["branch", "other"]),
        keys("ctrl-r"),
        draw(),
        mouse_on("c2", TestGesture::RightClick),
        menu_has(&[BranchOnly], &[BranchOnlyAtCommit]),
        keys("esc"),
        vec![Step::Act(Box::new(|app| app.run_event(BranchOnlyAtCommit)))],
        filter(GraphFilter::All),
        check(|app| assert_notice(app, "sidebar")),
    ]);
}

// Branch only of a remote branch: from the menu of a commit with only it, or of its row in the
// sidebar; the [x] on the commits shows all of them again.
#[test]
fn test_branch_only_of_a_remote_branch() {
    let dir = tempfile::tempdir().unwrap();
    let path = with_remote(dir.path());
    // feat on origin only
    git(&path, &["branch", "-q", "-D", "feat"]);
    let mut harness = Harness::new(&path, launch());
    let filter = |expected: &'static str| {
        check(move |app| assert_eq!(format!("{:?}", app.shell.graph_filter), expected))
    };
    let remote_feat = "BranchOnly(Some(RemoteBranch(\"origin/feat\")))";
    harness.run(vec![
        mouse_on("feat1", TestGesture::RightClick),
        mouse_on("this branch only", TestGesture::Click),
        draw(),
        filter(remote_feat),
        screen(|s| assert!(s.contains("origin/feat only"), "{s}")),
        mouse_on("[x]", TestGesture::Click),
        filter("All"),
        // the remotes, origin under them, origin/feat first under it
        mouse_on("Remotes", TestGesture::Click),
        mouse_at("Remotes", (2, 1), TestGesture::Click),
        mouse_at("Remotes", (4, 2), TestGesture::RightClick),
        check(|app| {
            let feat = RefSelection::Ref(NamedRef::RemoteBranch("origin/feat".into()));
            assert_eq!(app.shell.sidebar.selection(), feat);
        }),
        menu_has(&[UserEvent::BranchOnly], &[]),
        mouse_on("branch only", TestGesture::Click),
        filter(remote_feat),
    ]);
}

// The menus of the local changes leave out the page's commands and the diff's context.
#[test]
fn test_local_changes_menus_leave_out_the_page_commands() {
    use UserEvent::*;
    let dir = tempfile::tempdir().unwrap();
    let (_, mut harness) = local_changes_harness(dir.path());
    let page = &[
        Commit,
        ToggleAmend,
        Abort,
        Continue,
        Cancel,
        Search,
        ToggleSelect,
    ];
    harness.run(vec![
        mouse_on("b.txt", TestGesture::RightClick),
        menu_has(&[Select, Discard], page),
        keys("esc"),
        mouse_on("f.txt", TestGesture::Click),
        draw(),
        mouse_on("changed", TestGesture::RightClick),
        menu_has(
            &[Select],
            &[IncreaseDiffContext, DecreaseDiffContext, NavigateLeft],
        ),
        menu_has(&[], page),
    ]);
}

// In the diffs too, y starts copying: y the hunk or lines, f the file's absolute path, l the link,
// as in the lists.
#[test]
fn test_copy_keys_alike_in_the_diffs() {
    let dir = tempfile::tempdir().unwrap();
    let path = with_remote(dir.path());
    std::fs::write(path.join("f.txt"), "changed\n").unwrap();
    let mut harness = Harness::new(&path, launch());
    let next = |expected: &'static [(&'static str, &'static str)]| {
        check(move |app| {
            assert_eq!(app.shell.app_status.pending_keys.len(), 1);
            let next: Vec<(String, String)> = app
                .keys_popup_keys()
                .into_iter()
                .map(|k| (k.keys, k.action))
                .collect();
            let expected: Vec<(String, String)> = expected
                .iter()
                .map(|(k, a)| (k.to_string(), a.to_string()))
                .collect();
            assert_eq!(next, expected);
        })
    };
    harness.run(vec![
        // the diff of the commit's file
        keys("enter 4 j enter"),
        keys("y"),
        next(&[
            ("f", "copy full path"),
            ("l", "copy link"),
            ("y", "copy hunk"),
        ]),
        keys("esc esc esc esc"),
        // the diff of a local change
        keys("tab"),
        mouse_on("f.txt", TestGesture::Click),
        keys("y"),
        // no link: the change isn't on the web host
        next(&[("f", "copy full path"), ("y", "copy hunk")]),
    ]);
}

// [x] at the top right of the details, the keys popup, a window or a popup closes it.
#[test]
fn test_close_buttons() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("enter"),
        check(|app| assert!(matches!(app.view, View::Detail(_)))),
        mouse_on("[x]", TestGesture::Click),
        check(|app| assert!(matches!(app.view, View::List(_)))),
        keys("shift-h"),
        check(|app| assert!(app.shell.keys_popup)),
        mouse_on("[x]", TestGesture::Click),
        check(|app| assert!(!app.shell.keys_popup)),
        keys("?"),
        check(|app| assert!(app.shell.overlay.is_some())),
        mouse_on("[x]", TestGesture::Click),
        check(|app| assert!(app.shell.overlay.is_none())),
        keys("b"),
        mouse_on("[x]", TestGesture::Click),
        check(|app| assert!(app.shell.dialog.is_none())),
    ]);
}

#[test]
fn test_click_below_the_last_row_only_focuses() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("1"),
        mouse_at("c1", (0, 10), TestGesture::Click),
        check(|app| assert!(!app.shell.sidebar_focused())),
        check(|app| assert!(matches!(app.view, View::List(_)))),
        check(|app| assert_eq!(selected_subject(app), "c2")),
    ]);
    // a new repository, no commit to click
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("work");
    std::fs::create_dir(&path).unwrap();
    git(&path, &["init", "-q", "-b", "master"]);
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        mouse_at("Commits", (0, 3), TestGesture::Click),
        mouse_at("Commits", (0, 3), TestGesture::CtrlClick),
        mouse_at("Commits", (0, 3), TestGesture::DragBy(0, 2)),
        check(|app| assert!(matches!(app.view, View::List(_)))),
    ]);
}

#[test]
fn test_border_press_still_resizes() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    let width = |app: &App| app.ctx.layout.borrow().size(Panel::Sidebar, 0);
    harness.run(vec![
        // the right border of the sidebar, its corner at the top
        mouse_at("╮", (0, 5), TestGesture::DragBy(6, 0)),
        draw(),
        check(move |app| assert!(width(app) > 0)),
        commits_picked(0),
        check(|app| assert!(!app.shell.sidebar_focused())),
    ]);
}

// While a popup or the menu is open, the mouse moves no border: it used to, from the popup's own
// border over one too.
#[test]
fn test_no_border_drag_over_a_popup_or_the_menu() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    let width = |app: &App| app.ctx.layout.borrow().size(Panel::Sidebar, 0);
    harness.run(vec![
        keys("space"),
        mouse_at("╮", (0, 5), TestGesture::DragBy(6, 0)),
        draw(),
        check(move |app| assert_eq!(width(app), 0)),
        check(|app| assert!(app.shell.dialog.is_some())),
        keys("esc"),
        mouse_on("c1", TestGesture::RightClick),
        mouse_at("╮", (0, 5), TestGesture::DragBy(6, 0)),
        draw(),
        check(move |app| assert_eq!(width(app), 0)),
    ]);
}

#[test]
fn test_wheel_scrolls_the_panel_under_the_pointer() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    let selection = Rc::new(RefCell::new(None));
    let before = selection.clone();
    harness.run(vec![
        keys("1"),
        check(move |app| *before.borrow_mut() = Some(app.shell.sidebar.selection())),
        keys("2"),
        mouse_on("Branches", TestGesture::WheelDown),
        check(|app| assert!(app.shell.sidebar_focused())),
        // the wheel scrolls the rows shown, not the selection
        check(move |app| assert_eq!(Some(app.shell.sidebar.selection()), *selection.borrow())),
        check(|app| assert_eq!(selected_subject(app), "c2")),
    ]);
}

// The wheel scrolls the graph, the selected commit staying even once off the rows shown; moving
// the selection shows it again.
#[test]
fn test_wheel_scrolls_the_graph_not_the_selection() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    for i in 0..60 {
        let message = format!("commit-{i:02}");
        git(&path, &["commit", "-q", "--allow-empty", "-m", &message]);
    }
    let offset = |app: &App| app.view.as_list_state().unwrap().cursor().offset();
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        check(|app| assert_eq!(selected_subject(app), "commit-59")),
        mouse_on("commit-50", TestGesture::WheelDown),
        mouse_on("commit-50", TestGesture::WheelDown),
        mouse_on("commit-50", TestGesture::WheelDown),
        check(|app| assert_eq!(selected_subject(app), "commit-59")),
        check(move |app| assert_eq!(offset(app), 3 * WHEEL_ROWS)),
        // drawn again, the rows stay scrolled
        draw(),
        check(move |app| assert_eq!(offset(app), 3 * WHEEL_ROWS)),
        mouse_on("commit-50", TestGesture::WheelUp),
        check(move |app| assert_eq!(offset(app), 2 * WHEEL_ROWS)),
        keys("j"),
        check(|app| assert_eq!(selected_subject(app), "commit-58")),
        check(move |app| assert!(offset(app) <= 1, "{}", offset(app))),
    ]);
}

#[test]
fn test_click_after_a_reload_uses_the_drawn_frame() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        vec![Step::Locate("c1", (0, 0))],
        // reloaded after the frame: what it showed may have moved
        keys("ctrl-r"),
        vec![Step::Mouse(TestGesture::Click)],
        check(|app| assert!(matches!(app.view, View::List(_)))),
        // drawn again, a click acts
        mouse_on("c1", TestGesture::Click),
        check(|app| assert!(matches!(app.view, View::Detail(_)))),
    ]);
}

// The menu open drawn with `shown` in it, and none of `left_out`; nothing else on the screen
// shows those words, the status bar showing the keys of the menu.
fn menu_shows(shown: &'static [&'static str], left_out: &'static [&'static str]) -> Vec<Step> {
    let mut steps = check(|app| assert!(app.shell.menu.is_some()));
    steps.extend(screen(move |screen| {
        for text in shown {
            assert!(screen.contains(text), "{text:?} not in\n{screen}");
        }
        for text in left_out {
            assert!(!screen.contains(text), "{text:?} in\n{screen}");
        }
    }));
    steps
}

#[test]
fn test_right_click_menu_on_a_commit() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        mouse_on("c1", TestGesture::RightClick),
        check(|app| assert_eq!(selected_subject(app), "c1")),
        // the keys of the page, not those of the whole app, nor the moves
        menu_shows(&["checkout", "merge"], &["push", "move"]),
        // the status line lists the keys of the menu, which takes them all
        screen(|screen| {
            let status = screen.lines().last().unwrap();
            assert!(
                status.contains("run") && !status.contains("fetch"),
                "{status}"
            );
        }),
        // a click on an entry runs it, as its key
        mouse_on("merge", TestGesture::Click),
        check(|app| assert!(app.shell.menu.is_none())),
        check(|app| assert!(popup_command(app).starts_with("merge"))),
        keys("esc"),
        // the keys move in it and run the selected entry
        mouse_on("c1", TestGesture::RightClick),
        keys("j"),
        check(|app| assert_eq!(app.shell.menu.as_ref().unwrap().selected(), 1)),
        keys("esc"),
        check(|app| assert!(app.shell.menu.is_none())),
        check(|app| assert!(app.shell.dialog.is_none())),
        // hovering selects an entry, enter runs it
        mouse_on("c1", TestGesture::RightClick),
        mouse_on("merge", TestGesture::Hover),
        keys("enter"),
        check(|app| assert!(popup_command(app).starts_with("merge"))),
        keys("esc"),
        // a click outside only closes it
        mouse_on("c1", TestGesture::RightClick),
        mouse_on("c2", TestGesture::Click),
        check(|app| assert!(app.shell.menu.is_none())),
        check(|app| assert!(matches!(app.view, View::List(_)))),
        check(|app| assert_eq!(selected_subject(app), "c1")),
    ]);
}

#[test]
fn test_right_click_on_an_unpicked_row_clears_the_picks() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("v v"),
        commits_picked(1),
        mouse_on("c1", TestGesture::RightClick),
        commits_picked(0),
        check(|app| assert!(app.shell.menu.is_some())),
    ]);
}

#[test]
fn test_right_click_on_a_picked_row_keeps_them() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("v j v"),
        commits_picked(2),
        mouse_on("feat1", TestGesture::RightClick),
        commits_picked(2),
        // c2 and feat1: diffing both, not cherry-picking c2 on master already
        menu_shows(&["diff"], &["cherry-pick"]),
    ]);
}

// Where the focus is in the details, on their tab.
fn detail_focus(focus: DetailFocus, tab: DetailTab) -> Vec<Step> {
    check(move |app| {
        let View::Detail(view) = &app.view else {
            panic!("no details");
        };
        assert_eq!((view.focus(), view.tab()), (focus, tab));
    })
}

#[test]
fn test_click_on_a_detail_tab_switches_to_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        mouse_on("c1", TestGesture::Click),
        detail_focus(DetailFocus::Info, DetailTab::Info),
        // the top bar says Local Changes too
        mouse_on("[4]", TestGesture::Click),
        detail_focus(DetailFocus::Files, DetailTab::Changes),
        mouse_on("Files", TestGesture::Click),
        detail_focus(DetailFocus::Files, DetailTab::Files),
        mouse_on("Info", TestGesture::Click),
        detail_focus(DetailFocus::Info, DetailTab::Info),
    ]);
}

#[test]
fn test_click_on_a_file_opens_its_diff() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("enter 4"),
        detail_focus(DetailFocus::Files, DetailTab::Changes),
        mouse_on("f.txt", TestGesture::Click),
        detail_focus(DetailFocus::Pane, DetailTab::Changes),
        // the preview of the files tab
        keys("5"),
        mouse_on("f.txt", TestGesture::Click),
        detail_focus(DetailFocus::Pane, DetailTab::Files),
        // a line of the preview, the focus staying in it
        mouse_on("MASTER", TestGesture::Click),
        detail_focus(DetailFocus::Pane, DetailTab::Files),
    ]);
}

#[test]
fn test_click_in_the_commit_list_over_the_details() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("enter 4"),
        // as enter on the list: that commit's details, the focus in them
        mouse_on("c1", TestGesture::Click),
        check(|app| assert_eq!(selected_subject(app), "c1")),
        detail_focus(DetailFocus::Files, DetailTab::Changes),
    ]);
}

#[test]
fn test_double_click_on_a_diff_line_applies_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    // on feat, a new file of two lines, which the working tree on master doesn't have
    git(&path, &["checkout", "-q", "feat"]);
    std::fs::write(path.join("h.txt"), "one\ntwo\n").unwrap();
    git(&path, &["add", "h.txt"]);
    git(&path, &["commit", "-q", "-m", "feat2"]);
    git(&path, &["checkout", "-q", "master"]);
    let mut harness = Harness::new(&path, launch());
    let applied = path.join("h.txt");
    harness.run(vec![
        mouse_on("feat2", TestGesture::Click),
        keys("4"),
        // the line clicked, not its hunk
        double_click("two"),
        check(move |_| assert_eq!(std::fs::read_to_string(&applied).unwrap(), "two\n")),
    ]);
}

// A right click on a line not picked clears the picked lines, so its menu acts on that line, as
// on the other lists: in the preview, the blame and the diff of the timeline.
#[test]
fn test_right_click_on_an_unpicked_line_clears_the_picks() {
    use crate::keybind::UserEvent::ShortCopy;
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let copies = |short: &'static str| {
        check(move |app| assert_eq!(app.panel_keys().describe(ShortCopy), Some(short)))
    };
    // the lines MASTER and c of f.txt in c2, picked, then a right click on one of them and on a
    let pick_then_right_click = |open: Vec<Step>, pick: &'static str| {
        let mut steps = open;
        steps.extend(mouse_on("MASTER", TestGesture::Click));
        steps.extend(keys(pick));
        steps.extend(copies("copy lines"));
        steps.extend(mouse_on("MASTER", TestGesture::RightClick));
        steps.extend(keys("esc"));
        steps.extend(copies("copy lines"));
        steps.extend(mouse_at("MASTER", (0, -1), TestGesture::RightClick));
        steps.extend(keys("esc"));
        steps.extend(copies("copy line"));
        steps
    };
    for (open, pick) in [("enter 5 shift-b", "v j v"), ("enter 5 shift-t", "v v")] {
        let mut harness = Harness::new(&path, launch());
        harness.run(vec![pick_then_right_click(keys(open), pick)]);
    }
    let mut harness = Harness::new(&path, launch());
    let mut preview = keys("enter 5");
    preview.extend(mouse_on("f.txt", TestGesture::Click));
    harness.run(vec![pick_then_right_click(preview, "v j v")]);
}

// A double click on a line not picked acts on it alone, as on the other lists.
#[test]
fn test_double_click_on_an_unpicked_diff_line_clears_the_picks() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    git(&path, &["checkout", "-q", "feat"]);
    std::fs::write(path.join("h.txt"), "one\ntwo\nthree\n").unwrap();
    git(&path, &["add", "h.txt"]);
    git(&path, &["commit", "-q", "-m", "feat2"]);
    git(&path, &["checkout", "-q", "master"]);
    let mut harness = Harness::new(&path, launch());
    let applied = path.join("h.txt");
    harness.run(vec![
        mouse_on("feat2", TestGesture::Click),
        keys("4"),
        mouse_on("one", TestGesture::Drag(&["two"])),
        double_click("three"),
        check(move |_| assert_eq!(std::fs::read_to_string(&applied).unwrap(), "three\n")),
    ]);
}

#[test]
fn test_click_on_a_diff_line_selects_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    git(&path, &["checkout", "-q", "feat"]);
    std::fs::write(path.join("h.txt"), "one\ntwo\n").unwrap();
    git(&path, &["add", "h.txt"]);
    git(&path, &["commit", "-q", "-m", "feat2"]);
    git(&path, &["checkout", "-q", "master"]);
    let mut harness = Harness::new(&path, launch());
    let applied = path.join("h.txt");
    harness.run(vec![
        mouse_on("feat2", TestGesture::Click),
        keys("4"),
        // into the lines of its hunk, on the line clicked rather than the first
        mouse_on("two", TestGesture::Click),
        keys("space"),
        check(move |_| assert_eq!(std::fs::read_to_string(&applied).unwrap(), "two\n")),
    ]);
}

#[test]
fn test_drag_in_a_hunk_diff_picks_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        mouse_on("feat1", TestGesture::Click),
        keys("4"),
        // from FEAT up to the b it replaces, the diff in its hunks
        mouse_at("FEAT", (0, 0), TestGesture::DragBy(0, -1)),
        detail_focus(DetailFocus::Pane, DetailTab::Changes),
        keys("space"),
        check(|app| assert_notice(app, "The lines doesn't apply")),
    ]);
}

// `repository` with three changed files: a.txt, b.txt and f.txt, on the local changes page; the
// diff's title is the selected row, /, so the files are drawn only in their tree.
fn local_changes_harness(dir: &Path) -> (PathBuf, Harness) {
    let path = repository(dir);
    std::fs::write(path.join("f.txt"), "changed\n").unwrap();
    std::fs::write(path.join("a.txt"), "a\n").unwrap();
    std::fs::write(path.join("b.txt"), "b\n").unwrap();
    let launch = Launch {
        page: StartPage::LocalChanges,
        ..launch()
    };
    let harness = Harness::new(&path, launch);
    (path, harness)
}

fn staged(path: &Path) -> String {
    git(path, &["diff", "--cached", "--name-only"])
}

fn local_scope(scope: Scope) -> Vec<Step> {
    check(move |app| assert_eq!(app.current_scope(), scope))
}

#[test]
fn test_double_click_stages_and_unstages_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let (path, mut harness) = local_changes_harness(dir.path());
    let (once, twice) = (path.clone(), path.clone());
    harness.run(vec![
        double_click("f.txt"),
        check(move |_| assert_eq!(staged(&once).trim(), "f.txt")),
        // now in the staged files
        double_click("f.txt"),
        check(move |_| assert_eq!(staged(&twice).trim(), "")),
    ]);
}

#[test]
fn test_click_on_a_changed_file_opens_its_diff() {
    let dir = tempfile::tempdir().unwrap();
    let (_, mut harness) = local_changes_harness(dir.path());
    harness.run(vec![
        mouse_on("b.txt", TestGesture::Click),
        local_scope(Scope::LocalChangesDiff),
        check(|app| assert_eq!(local_change_path(app).as_deref(), Some("b.txt"))),
    ]);
}

#[test]
fn test_drag_and_ctrl_click_pick_files() {
    let dir = tempfile::tempdir().unwrap();
    let (_, mut harness) = local_changes_harness(dir.path());
    let picked = |shown: &'static str| {
        screen(move |screen| assert!(screen.contains(shown), "{shown:?} not in\n{screen}"))
    };
    harness.run(vec![
        mouse_on("b.txt", TestGesture::Drag(&["f.txt"])),
        picked("2 picked"),
        local_scope(Scope::LocalChanges),
        mouse_on("b.txt", TestGesture::CtrlClick),
        picked("1 picked"),
        // the same cell: the diff's title is b.txt now
        vec![Step::Mouse(TestGesture::CtrlClick)],
        picked("2 picked"),
    ]);
}

// [Commit] on the border of the commit message commits as the key does, while typing too; with
// nothing to commit it is dimmed and does nothing.
#[test]
fn test_commit_button() {
    let dir = tempfile::tempdir().unwrap();
    let (_, mut harness) = local_changes_harness(dir.path());
    harness.run(vec![
        mouse_on("[Commit]", TestGesture::Click),
        check(|app| assert!(app.shell.dialog.is_none())),
        check(|app| assert!(!app.is_typing())),
        check(|app| assert!(notices(app).is_empty(), "{:?}", notices(app))),
        double_click("b.txt"),
        mouse_on("Summary", TestGesture::Click),
        typing("msg"),
        mouse_on("[Commit]", TestGesture::Click),
        check(|app| assert!(app.shell.dialog.is_some())),
    ]);
}

// The amend checkbox at the bottom left of the commit message is checked while amending, a click
// toggling it as the key does, while typing too.
#[test]
fn test_amend_checkbox() {
    let dir = tempfile::tempdir().unwrap();
    let (_, mut harness) = local_changes_harness(dir.path());
    let amending = |on: bool| {
        let mark = if on { "[•] amend" } else { "[ ] amend" };
        screen(move |s| assert!(s.contains(mark), "{s}"))
    };
    harness.run(vec![
        draw(),
        amending(false),
        mouse_on("[ ] amend", TestGesture::Click),
        draw(),
        amending(true),
        // typing the summary, now the last commit's
        mouse_at("[3] Commit", (2, 1), TestGesture::Click),
        check(|app| assert!(app.is_typing())),
        mouse_on("[•] amend", TestGesture::Click),
        draw(),
        amending(false),
        check(|app| assert!(!app.is_typing())),
    ]);
}

// A click on a box of the commit message types in it; while typing, a click on a box types in the
// box clicked, and a double click keeps typing.
#[test]
fn test_click_on_the_message_box_types_in_it() {
    let dir = tempfile::tempdir().unwrap();
    let (_, mut harness) = local_changes_harness(dir.path());
    harness.run(vec![
        mouse_on("Summary", TestGesture::Click),
        check(|app| assert!(app.is_typing())),
        typing("title"),
        mouse_on("Description", TestGesture::Click),
        check(|app| assert!(app.is_typing())),
        typing("body"),
        double_click("title"),
        check(|app| assert!(app.is_typing())),
        typing("!"),
        check(|app| assert_eq!(draft(app), "title!\n\nbody")),
    ]);
}

#[test]
fn test_click_while_typing_the_message_stops_typing() {
    let dir = tempfile::tempdir().unwrap();
    let (_, mut harness) = local_changes_harness(dir.path());
    harness.run(vec![
        keys("3"),
        typing("abc"),
        check(|app| assert!(app.is_typing())),
        mouse_on("b.txt", TestGesture::Click),
        check(|app| assert!(!app.is_typing())),
        check(|app| assert_eq!(draft(app), "abc")),
        check(|app| assert_ne!(local_change_path(app).as_deref(), Some("b.txt"))),
    ]);
}

#[test]
fn test_double_click_in_a_conflict_keeps_a_side() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let merge = crate::git::git_command()
        .args(["merge", "-q", "feat"])
        .current_dir(&path)
        .output()
        .unwrap();
    assert!(!merge.status.success(), "{merge:?}");
    let launch = Launch {
        page: StartPage::LocalChanges,
        ..launch()
    };
    let mut harness = Harness::new(&path, launch);
    let kept = path.clone();
    harness.run(vec![
        // the conflicted file, under the root row, its conflict panel in place of the diff (whose
        // first line names f.txt too)
        mouse_at("Unstaged", (0, 2), TestGesture::Click),
        double_click("Current"),
        check(move |_| {
            assert_eq!(git(&kept, &["ls-files", "-u"]), "");
            assert_eq!(
                std::fs::read_to_string(kept.join("f.txt")).unwrap(),
                "a\nMASTER\nc\n"
            );
        }),
    ]);
}

#[test]
fn test_click_on_the_page_bar_switches_page() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        mouse_on("Local", TestGesture::Click),
        check(|app| assert!(matches!(app.view, View::LocalChanges(_)))),
        // the top bar comes first, before the commit section of the page
        mouse_on("Commit", TestGesture::Click),
        check(|app| assert!(matches!(app.view, View::List(_)))),
        // as tab: the sidebar has the focus
        check(|app| assert!(app.shell.sidebar_focused())),
        // the page shown: nothing to switch
        mouse_on("Commit", TestGesture::Click),
        check(|app| assert!(matches!(app.view, View::List(_)))),
    ]);
}

#[test]
fn test_clicks_in_the_blame() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("enter 5 shift-b"),
        // a line's text, the commit shown at the top
        mouse_on("MASTER", TestGesture::Click),
        local_scope(Scope::FileHistoryPane),
        mouse_on("[1]", TestGesture::Click),
        local_scope(Scope::FileHistory),
        // a right click shows the keys of the blame
        mouse_on("MASTER", TestGesture::RightClick),
        check(|app| assert!(app.shell.menu.is_some())),
        keys("esc"),
        // the commit of a line, beside it: selected only, the window stays
        mouse_at("MASTER", (-6, 0), TestGesture::Click),
        check(|app| assert!(app.shell.overlay.is_some())),
        local_scope(Scope::FileHistory),
        // a double click shows it, as enter does
        mouse_at("MASTER", (-6, 0), TestGesture::Click),
        check(|app| assert!(app.shell.overlay.is_none())),
        check(|app| assert_eq!(selected_subject(app), "c2")),
    ]);
}

#[test]
fn test_click_in_the_timeline_selects_a_commit() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("enter 5 shift-t"),
        // a line of the diff of the selected commit, c2
        mouse_on("MASTER", TestGesture::Click),
        local_scope(Scope::FileHistoryPane),
        // the older commit: selected only, the window stays
        mouse_on("c1", TestGesture::Click),
        check(|app| assert!(app.shell.overlay.is_some())),
        // later, a double click shows it, as enter does
        sleep(500),
        double_click("c1"),
        check(|app| assert!(app.shell.overlay.is_none())),
        check(|app| assert_eq!(selected_subject(app), "c1")),
    ]);
}

#[test]
fn test_double_click_on_an_action_in_the_keys_list_runs_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("?"),
        // a click selects it only
        mouse_on("Search the commits", TestGesture::Click),
        check(|app| assert!(app.shell.overlay.is_some())),
        check(|app| assert!(!app.is_typing())),
        sleep(500),
        double_click("Search the commits"),
        check(|app| assert!(app.shell.overlay.is_none())),
        check(|app| assert!(app.is_typing())),
    ]);
}

#[test]
fn test_clicks_in_the_command_log() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    let focus = |focus| {
        check(move |app| {
            let Some(super::Overlay::CommandLog(state)) = &app.shell.overlay else {
                panic!("no command log");
            };
            assert_eq!(state.focus(), focus);
        })
    };
    harness.run(vec![
        // a command logged: feat checked out
        keys("j space enter"),
        keys("@"),
        mouse_on("Output", TestGesture::Click),
        focus(crate::widget::command_log::CommandLogFocus::Output),
        mouse_on("checkout", TestGesture::Click),
        focus(crate::widget::command_log::CommandLogFocus::Commands),
    ]);
}

#[test]
fn test_right_click_menu_in_every_panel() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    std::fs::write(path.join("g.txt"), "new\n").unwrap();
    let mut harness = Harness::new(&path, launch());
    let menu = || {
        let mut steps = check(|app| assert!(app.shell.menu.is_some()));
        steps.extend(keys("esc"));
        steps
    };
    let right_click = |text| mouse_on(text, TestGesture::RightClick);
    harness.run(vec![
        // c2, whose diff and blame have MASTER
        right_click("c2"),
        menu(),
        mouse_at("Branches", (2, 1), TestGesture::RightClick),
        menu(),
        // the details: the files and the diff of the changes tab, the preview of the files tab
        keys("esc enter 4"),
        right_click("f.txt"),
        // selecting the hunks or the lines is a click's
        menu_has(&[], &[UserEvent::Confirm]),
        menu(),
        right_click("MASTER"),
        menu_has(&[], &[UserEvent::Confirm]),
        menu(),
        keys("5"),
        right_click("f.txt"),
        menu_has(&[], &[UserEvent::Confirm]),
        menu(),
        // the windows: the blame, the keys list, the command log
        keys("shift-b"),
        right_click("MASTER"),
        menu(),
        // the first action listed, under the name of the window in the bar at the top
        keys("esc esc ?"),
        mouse_at("Keys", (0, 2), TestGesture::RightClick),
        menu(),
        keys("esc @"),
        right_click("Output"),
        menu(),
        // the local changes
        keys("esc tab"),
        right_click("g.txt"),
        menu(),
    ]);
}

#[test]
fn test_click_after_the_files_reload_in_place_does_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (_, mut harness) = local_changes_harness(dir.path());
    harness.run(vec![
        vec![Step::Locate("b.txt", (0, 0))],
        // a file added before b.txt, the rows moving under the frame drawn
        outside(|path| std::fs::write(path.join("a0.txt"), "x\n").unwrap()),
        // the files checked as the click comes, before the app draws again
        vec![Step::Together(vec![
            Step::Tick,
            Step::Mouse(TestGesture::Click),
        ])],
        check(|app| assert_eq!(local_change_path(app).as_deref(), Some(":/"))),
        // drawn again, a click acts
        mouse_on("b.txt", TestGesture::Click),
        check(|app| assert_eq!(local_change_path(app).as_deref(), Some("b.txt"))),
    ]);
}

// Beside the details too, the wheel scrolls the commits and leaves the one shown.
#[test]
fn test_wheel_beside_the_details_keeps_the_commit() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    for i in 0..60 {
        let message = format!("commit-{i:02}");
        git(&path, &["commit", "-q", "--allow-empty", "-m", &message]);
    }
    let offset = |app: &App| app.view.as_list_state().unwrap().cursor().offset();
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("enter"),
        keys("2"),
        mouse_on("commit-55", TestGesture::WheelDown),
        check(|app| assert!(matches!(app.view, View::Detail(_)))),
        check(|app| assert_eq!(selected_subject(app), "commit-59")),
        check(move |app| assert_eq!(offset(app), WHEEL_ROWS)),
    ]);
}

#[test]
fn test_wheel_over_the_bars_switches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        mouse_on("Local", TestGesture::WheelDown),
        check(|app| assert!(matches!(app.view, View::List(_)))),
        keys("enter"),
        mouse_on("[4]", TestGesture::WheelDown),
        detail_focus(DetailFocus::Info, DetailTab::Info),
    ]);
}

#[test]
fn test_click_on_a_notice_does_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        vec![Step::Event(Box::new(AppEvent::NotifyWarn("warned".into())))],
        // over the commit list
        mouse_on("warned", TestGesture::Click),
        mouse_on("warned", TestGesture::RightClick),
        check(|app| assert!(matches!(app.view, View::List(_)))),
        check(|app| assert!(app.shell.menu.is_none())),
    ]);
}

#[test]
fn test_v_v_toggles_the_pick_of_a_row() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    std::fs::write(path.join("f.txt"), "changed\n").unwrap();
    std::fs::write(path.join("g.txt"), "new\n").unwrap();
    let mut harness = Harness::new(&path, launch());
    let commits = |count: usize| {
        check(move |app| {
            let state = app.view.as_list_state().unwrap();
            assert_eq!(state.picked_commit_hashes().len(), count);
        })
    };
    let refs = |count: usize| check(move |app| assert_eq!(app.shell.sidebar.picked().len(), count));
    let files = |shown: &'static str| {
        screen(move |screen| assert!(screen.contains(shown), "{shown:?} not in\n{screen}"))
    };
    harness.run(vec![
        keys("v v"),
        commits(1),
        keys("v v"),
        commits(0),
        // a range picks all it covers, the picked one too
        keys("v v v j v"),
        commits(2),
        // the refs sidebar
        keys("esc 1 j v v"),
        refs(1),
        keys("v v"),
        refs(0),
        // the files of the local changes
        keys("esc tab 1 v v"),
        files("1 picked"),
        keys("v v"),
        screen(|screen| assert!(!screen.contains("picked"), "{screen}")),
    ]);
}

#[test]
fn test_visual_selects_the_commits_from_the_first_to_the_last_moved_to() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    let picked = |count: usize| {
        check(move |app| {
            assert_eq!(
                app.view
                    .as_list_state()
                    .unwrap()
                    .picked_commit_hashes()
                    .len(),
                count
            );
        })
    };
    harness.run(vec![
        keys("v"),
        picked(1),
        keys("j j"),
        picked(3),
        // v ends the selection, the commits staying picked for the commands
        keys("v"),
        picked(3),
        // a new visual selection keeps them too
        keys("g v"),
        picked(3),
        // ended where it started, on a picked commit: v v unpicks it
        keys("v"),
        picked(2),
        keys("esc"),
        picked(0),
    ]);
}

#[test]
fn test_no_match_said_alike_on_every_page() {
    // the keys to a search, and the page they are on
    let pages = [
        ("", StartPage::Graph),
        // the file trees are filtered, their diff or preview searched
        ("5 enter", StartPage::Graph),
        ("5 shift-b l", StartPage::Graph),
        ("5 shift-t", StartPage::Graph),
        ("enter", StartPage::LocalChanges),
        // the refs sidebar
        ("1", StartPage::Graph),
    ];
    for (to_search, page) in pages {
        let dir = tempfile::tempdir().unwrap();
        let path = repository(dir.path());
        std::fs::write(path.join("f.txt"), "changed\n").unwrap();
        let mut harness = Harness::new(&path, Launch { page, ..launch() });
        let mut steps = vec![];
        if !to_search.is_empty() {
            steps.push(keys(to_search));
        }
        steps.extend([
            keys("/"),
            typing("zzz"),
            keys("enter n"),
            check(move |app| {
                let said = &app.shell.app_status.status_line;
                assert!(
                    matches!(said, StatusLine::SearchResult {
                        outcome: SearchOutcome::NotFound { query },
                        ..
                    } if query == "zzz"),
                    "{to_search:?} {said:?}"
                );
            }),
        ]);
        harness.run(steps);
    }
}

#[test]
fn test_found_match_counted_alike_on_every_page() {
    // the keys to a search, the page they are on, the query, and what it matches there
    let pages = [
        ("", StartPage::Graph, "feat1", 1),
        // the file trees are filtered, their diff or preview searched: c2's
        ("4 enter", StartPage::Graph, "MASTER", 1),
        ("5 enter", StartPage::Graph, "MASTER", 1),
        // the graph selects c2 first, whose file has an "a" line
        ("5 shift-b l", StartPage::Graph, "a", 2),
        // from feat1, whose file's history has it
        ("j 5 shift-t", StartPage::Graph, "feat1", 1),
        ("enter", StartPage::LocalChanges, "changed", 1),
    ];
    for (to_search, page, query, total) in pages {
        let dir = tempfile::tempdir().unwrap();
        let path = repository(dir.path());
        std::fs::write(path.join("f.txt"), "changed\n").unwrap();
        let mut harness = Harness::new(&path, Launch { page, ..launch() });
        let mut steps = vec![];
        if !to_search.is_empty() {
            steps.push(keys(to_search));
        }
        steps.extend([
            keys("/"),
            typing(query),
            keys("enter"),
            check(move |app| {
                let said = &app.shell.app_status.status_line;
                // which match it is on depends on the order the page shows them in, the count
                // doesn't
                assert!(
                    matches!(said, StatusLine::SearchResult {
                        outcome: SearchOutcome::Found { total: found, query: asked, .. },
                        ..
                    } if *found == total && asked == query),
                    "{to_search:?} {said:?}"
                );
            }),
        ]);
        harness.run(steps);
    }
}

#[test]
fn test_diff_keys_alike_on_every_page() {
    // the keys into a diff, at its hunks, and the page they are on
    let pages = [
        ("4 enter", StartPage::Graph),
        ("5 shift-t l", StartPage::Graph),
        ("enter", StartPage::LocalChanges),
    ];
    // what the keys popup names enter and y y
    let named = |enter: Option<&'static str>, copy: &'static str, to: &'static str| {
        check(move |app| {
            let keys = app.keys_popup_keys();
            let action = |key: &str| {
                let label = keys.iter().find(|k| k.keys == key);
                label.map(|k| k.action.as_str())
            };
            assert_eq!(
                (action("enter"), action("yy")),
                (enter, Some(copy)),
                "{to:?}"
            );
        })
    };
    for (to, page) in pages {
        let dir = tempfile::tempdir().unwrap();
        let path = repository(dir.path());
        std::fs::write(path.join("f.txt"), "changed\n").unwrap();
        let mut harness = Harness::new(&path, Launch { page, ..launch() });
        harness.run(vec![
            keys(to),
            keys("shift-h"),
            named(Some("select lines"), "copy hunk", to),
            keys("shift-h enter shift-h"),
            // already on the lines, enter does nothing
            named(None, "copy line", to),
        ]);
    }
}

#[test]
fn test_search_shown_in_the_title_of_its_panel() {
    // the keys to the panel searched, the page they are on, and its title
    let panels = [
        ("4 enter", StartPage::Graph, "[3] Info"),
        ("5 shift-t", StartPage::Graph, "[1] Commits"),
        ("5 shift-t l", StartPage::Graph, "[2] Diff"),
        ("5 shift-b l", StartPage::Graph, "[2] Blame"),
        ("enter", StartPage::LocalChanges, "╭ / "),
    ];
    for (to, page, label) in panels {
        let dir = tempfile::tempdir().unwrap();
        let path = repository(dir.path());
        std::fs::write(path.join("f.txt"), "a\nchanged\nc\n").unwrap();
        let mut harness = Harness::new(&path, Launch { page, ..launch() });
        harness.run(vec![
            keys(to),
            keys("/ c enter"),
            screen(move |screen| {
                let title = screen.lines().find_map(|l| l.split_once(label));
                let title = title.map(|(_, rest)| rest.split('╮').next().unwrap());
                assert!(
                    title.is_some_and(|t| t.contains(" /c ")),
                    "{label}: {title:?}\n{screen}"
                );
            }),
        ]);
    }
}

#[test]
fn test_timeline_shows_a_commit_on_the_graph() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    git(&path, &["branch", "-q", "-D", "feat"]);
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        // f.txt in the files of c2, then c1, the commit before changing it
        keys("5 shift-t"),
        overlay("timeline"),
        keys("j enter"),
        overlay("none"),
        check(|app| {
            let View::Detail(view) = &app.view else {
                panic!("no details")
            };
            let commit = view.as_list_state().selected_commit_hash();
            assert_eq!(app.repository.commit(commit).unwrap().subject, "c1");
        }),
    ]);
}

#[test]
fn test_timeline_diff_applies_to_the_working_tree() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    // the file as before c2
    std::fs::write(path.join("f.txt"), "a\nb\nc\n").unwrap();
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        // f.txt in the files of c2, then the hunk of c2 applied from its diff
        keys("5 shift-t"),
        overlay("timeline"),
        keys("l space"),
        wait_for(|path| std::fs::read_to_string(path.join("f.txt")).unwrap() == "a\nMASTER\nc\n"),
    ]);
}

#[test]
fn test_diff_of_commits_stays_open_on_reload() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        // c1, the first, diffed with HEAD
        keys("shift-g shift-d"),
        check(|app| assert_eq!(compared(app), Some(vec!["f.txt".to_string()]))),
        keys("ctrl-r"),
        check(|app| assert_eq!(compared(app), Some(vec!["f.txt".to_string()]))),
        keys("esc"),
        check(|app| assert_eq!(compared(app), None)),
    ]);
}

#[test]
fn test_compare_follows_a_ref_the_launch_fetch_moves() {
    let dir = tempfile::tempdir().unwrap();
    let path = with_remote(dir.path());
    pushed_elsewhere(dir.path(), "feat", "fetched.txt");
    // slow to answer, so the comparison is open before the fetch is done
    git(
        &path,
        &[
            "config",
            "remote.origin.uploadpack",
            "sleep 2; git-upload-pack",
        ],
    );
    let fetched =
        |path: &Path| git(path, &["log", "-1", "--format=%s", "origin/feat"]) == "fetched.txt\n";
    let mut harness = Harness::new(
        &path,
        Launch {
            fetch: true,
            ..launch()
        },
    );
    harness.run(vec![
        // origin/feat, diffed with HEAD
        keys("1 ] j l j shift-d"),
        check(|app| {
            assert_eq!(graph_note(app).as_deref(), Some("master ↔ origin/feat"));
            assert_eq!(compared(app), Some(vec!["f.txt".to_string()]));
        }),
        wait_for(fetched),
        tick(),
        check(|app| {
            assert_eq!(graph_note(app).as_deref(), Some("master ↔ origin/feat"));
            let paths = vec!["f.txt".to_string(), "fetched.txt".to_string()];
            assert_eq!(compared(app), Some(paths));
        }),
    ]);
}

// Git commands failing

// With a commit on master here, and another there.
fn diverged(dir: &Path) -> PathBuf {
    let path = with_remote(dir);
    pushed_elsewhere(dir, "master", "theirs.txt");
    std::fs::write(path.join("f.txt"), "a\nOURS\nc\n").unwrap();
    git(&path, &["commit", "-q", "-am", "ours"]);
    git(&path, &["fetch", "-q"]);
    path
}

#[test]
fn test_push_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = diverged(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("shift-p enter"),
        check(|app| assert_notice(app, "git push failed: ! [rejected]")),
    ]);
}

#[test]
fn test_pull_conflicting_then_aborted() {
    let dir = tempfile::tempdir().unwrap();
    let path = diverged(dir.path());
    // theirs changes f.txt too
    pushed_elsewhere(dir.path(), "master", "f.txt");
    git(&path, &["fetch", "-q"]);
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("p enter"),
        check(|app| {
            assert_notice(app, "git pull failed: CONFLICT");
            assert!(matches!(app.view, View::LocalChanges(_)));
            assert!(app.repository.operation_in_progress().is_some());
            assert!(
                draft(app).starts_with("Merge branch 'master'"),
                "{}",
                draft(app)
            );
        }),
        keys("m a enter"),
        check(|app| {
            assert!(app.repository.operation_in_progress().is_none());
            // the merge's message went with it
            assert_eq!(draft(app), "");
        }),
    ]);
}

#[test]
fn test_stage_while_the_index_is_locked() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    std::fs::write(path.join("f.txt"), "changed\n").unwrap();
    std::fs::write(path.join(".git/index.lock"), "").unwrap();
    let mut harness = Harness::new(
        &path,
        Launch {
            page: StartPage::LocalChanges,
            ..launch()
        },
    );
    harness.run(vec![
        keys("space"),
        check(|app| assert_notice(app, "index.lock': File exists")),
    ]);
}

#[test]
fn test_new_repository_opens_and_makes_its_first_commit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("work");
    std::fs::create_dir(&path).unwrap();
    git(&path, &["init", "-q", "-b", "master"]);
    git(&path, &["config", "user.name", "t"]);
    git(&path, &["config", "user.email", "t@t"]);
    git(&path, &["config", "commit.gpgsign", "false"]);
    std::fs::write(path.join("f.txt"), "a\n").unwrap();
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        // no commit to show the details of
        keys("enter j y"),
        keys("tab space 3"),
        typing("first"),
        keys("esc c"),
        screen(|screen| assert!(screen.contains("Commit to master"), "{screen}")),
        keys("enter"),
    ]);
    assert_eq!(git(&path, &["log", "--format=%s"]), "first\n");
}

#[test]
fn test_commit_refused_by_a_hook_keeps_the_message() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    std::fs::write(path.join("f.txt"), "changed\n").unwrap();
    let hook = path.join(".git/hooks/pre-commit");
    std::fs::write(&hook, "#!/bin/sh\necho 'hook says no' >&2\nexit 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut harness = Harness::new(
        &path,
        Launch {
            page: StartPage::LocalChanges,
            ..launch()
        },
    );
    harness.run(vec![
        keys("space 3"),
        typing("my message"),
        keys("esc c enter"),
        check(|app| {
            assert_notice(app, "git commit failed: hook says no");
            assert_eq!(draft(app), "my message");
        }),
    ]);
}

#[test]
fn test_checkout_refused_over_local_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    std::fs::write(path.join("f.txt"), "changed\n").unwrap();
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        // c1, the first commit
        keys("shift-g space enter"),
        check(|app| {
            assert_notice(app, "would be overwritten by checkout");
            assert!(app.repository.current_branch().as_deref() == Some("master"));
        }),
    ]);
}

#[test]
fn test_popup_checks_for_conflicts_in_the_background() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("j"),
        vec![Step::Act(Box::new(|app: &mut App| {
            // cherry-picking feat1, the second row, onto master, which changed its line of f.txt
            // too: the popup opens before the conflict is found, saying it looks for them
            let key = parse_key_event("shift-c").unwrap();
            app.handle_event(AppEvent::Key(key)).unwrap();
            let dialog = app.visible_dialog().expect("the popup");
            assert!(dialog.is_checking_conflicts());
            let backend = ratatui::backend::TestBackend::new(SIZE.0, SIZE.1);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            let keys = app.keys_popup_keys();
            let frame = terminal.draw(|f| app.render(f, &keys)).unwrap();
            let text: String = frame.buffer.content.iter().map(|c| c.symbol()).collect();
            assert!(text.contains("Checking for conflicts…"));
        }))],
        // then the warning
        screen(|text| {
            assert!(!text.contains("Checking for conflicts…"), "{text}");
            assert!(text.contains("Conflicts at"), "{text}");
        }),
        keys("esc"),
    ]);
}

#[test]
fn test_revert_conflicting_then_aborted() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let head = git(&path, &["rev-parse", "HEAD"]);
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        // c1 made f.txt, which c2 changed since
        keys("shift-g u enter"),
        check(|app| {
            assert_notice(app, "git revert failed: CONFLICT");
            assert!(matches!(app.view, View::LocalChanges(_)));
        }),
        keys("m a enter"),
        check(|app| {
            assert!(app.repository.operation_in_progress().is_none());
            assert_eq!(draft(app), "");
        }),
    ]);
    assert_eq!(git(&path, &["rev-parse", "HEAD"]), head);
}

#[test]
fn test_discard_while_merging() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    // feat and master changed the same line
    let merge = crate::git::git_command()
        .args(["merge", "-q", "feat"])
        .current_dir(&path)
        .output()
        .unwrap();
    assert!(!merge.status.success());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        // discarding restores files and moves no HEAD, which the merge stopped allows
        keys("tab d enter"),
        check(|app| {
            let notices = notices(app);
            assert!(
                !notices.iter().any(|n| n.contains("in progress")),
                "{notices:?}"
            );
        }),
    ]);
    let content = std::fs::read_to_string(path.join("f.txt")).unwrap();
    assert_eq!(content, "a\nMASTER\nc\n");
}

#[test]
fn test_squash_off_the_branch_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    let mut steps = vec![keys("/")];
    steps.push(typing("feat1"));
    steps.push(keys("enter esc s"));
    steps.push(check(|app| {
        assert_notice(app, "is not on master, check it out first")
    }));
    harness.run(steps);
}

// The sidebar

#[test]
fn test_delete_tag_also_on_an_unreachable_remote_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = with_remote(dir.path());
    git(&path, &["tag", "v1"]);
    git(&path, &["push", "-q", "origin", "v1"]);
    git(&path, &["remote", "set-url", "origin", "/nowhere"]);
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("1 ] ] j"),
        check(|app| {
            let selection = app.shell.sidebar.selection();
            assert_eq!(selection, RefSelection::Ref(NamedRef::Tag("v1".into())));
        }),
        // also from origin
        keys("d space enter"),
        check(|app| assert_notice(app, "git push failed")),
    ]);
    assert_eq!(git(&path, &["tag"]), "v1\n");
}

#[test]
fn test_edit_branch_upstream_only_chosen() {
    let dir = tempfile::tempdir().unwrap();
    let path = with_remote(dir.path());
    let mut harness = Harness::new(&path, launch());
    let mut steps = vec![keys("1 j e")];
    // renamed; e doesn't type into the upstream, l chooses a remote branch
    steps.push(typing("x"));
    steps.push(keys("esc j e l enter"));
    harness.run(steps);
    let branch = git(
        &path,
        &[
            "for-each-ref",
            "--format=%(refname:short) %(upstream:short)",
            "refs/heads/featx",
        ],
    );
    assert!(branch.starts_with("featx origin/"), "{branch}");
}

#[test]
fn test_add_remote_fetching_nothing_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = with_remote(dir.path());
    let mut harness = Harness::new(&path, launch());
    let mut steps = vec![keys("1 ] j a")];
    steps.push(typing("bad"));
    steps.push(keys("esc j e"));
    steps.push(typing("/nonexistent/repo.git"));
    steps.push(keys("esc enter"));
    steps.push(check(|app| assert_notice(app, "git fetch failed")));
    harness.run(steps);
    // added, to edit or remove it
    assert_eq!(git(&path, &["remote"]), "bad\norigin\n");
}

// The popups of fetch, pull and push

#[test]
fn test_pressed_twice_runs_without_the_popup() {
    let dir = tempfile::tempdir().unwrap();
    let path = with_remote(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("f f"),
        check(|app| {
            assert!(app.shell.dialog.is_none());
            let done = &app.shell.app_status.status_line;
            assert!(
                matches!(done, StatusLine::NotificationSuccess(m) if m == "Fetched origin"),
                "{done:?}"
            );
        }),
    ]);
    assert!(path.join(".git/FETCH_HEAD").exists());
}

#[test]
fn test_pressed_once_the_popup_shows_after_a_while() {
    let dir = tempfile::tempdir().unwrap();
    let path = with_remote(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("f"),
        check(|app| {
            assert!(app.shell.dialog.is_some());
            assert!(app.visible_dialog().is_none());
        }),
        sleep(300),
        // shown, and the key still runs it
        check(|app| assert!(app.visible_dialog().is_some())),
        keys("f"),
        check(|app| assert!(app.shell.dialog.is_none())),
    ]);
    assert!(path.join(".git/FETCH_HEAD").exists());
}

#[test]
fn test_another_key_shows_the_popup_right_away() {
    let dir = tempfile::tempdir().unwrap();
    let path = with_remote(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("f j"),
        check(|app| assert!(app.visible_dialog().is_some())),
    ]);
    assert!(!path.join(".git/FETCH_HEAD").exists());
}

// Commands run in the terminal, like the editor: here stand-ins for them, as the editor is
// chosen by the environment, which the tests running at once share

fn run_in_terminal(command: &[&str]) -> Vec<Step> {
    let command = command.iter().map(|a| a.to_string()).collect();
    vec![Step::Event(Box::new(AppEvent::RunExternal(command)))]
}

#[test]
fn test_command_in_the_terminal_reloads_after() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(
        &path,
        Launch {
            page: StartPage::LocalChanges,
            ..launch()
        },
    );
    harness.run(vec![
        check(|app| assert_eq!(app.repository.loaded_local_change_count(), 0)),
        // like an editor saving a file; run where the app is, which here isn't the repository
        run_in_terminal(&["sh", "-c", &format!("echo x > {}/new.txt", path.display())]),
        check(|app| {
            assert!(matches!(app.view, View::LocalChanges(_)));
            assert_eq!(app.repository.loaded_local_change_count(), 1);
            assert!(notices(app).is_empty());
        }),
    ]);
    assert_eq!(harness.loads.get(), 2);
}

#[test]
fn test_command_in_the_terminal_failing_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        run_in_terminal(&["false"]),
        check(|app| assert_notice(app, "false exited with non-zero status: exit status: 1")),
        run_in_terminal(&["/nonexistent/editor"]),
        check(|app| assert_notice(app, "Failed to execute /nonexistent/editor")),
    ]);
}

// The git command running in the background

#[test]
fn test_quit_waits_for_the_git_command_running() {
    let dir = tempfile::tempdir().unwrap();
    let path = with_remote(dir.path());
    git(
        &path,
        &[
            "config",
            "remote.origin.uploadpack",
            "sleep 1; git-upload-pack",
        ],
    );
    let mut harness = Harness::new(&path, launch());
    // fetching, then quit while it runs
    harness.run(vec![keys_at_once("f f q")]);
    assert!(harness.quit);
    // quit only once the fetch was done
    assert!(path.join(".git/FETCH_HEAD").exists());
}

#[test]
fn test_commit_made_clears_the_message() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    std::fs::write(path.join("f.txt"), "changed\n").unwrap();
    let mut harness = Harness::new(
        &path,
        Launch {
            page: StartPage::LocalChanges,
            ..launch()
        },
    );
    harness.run(vec![
        keys("space 3"),
        typing("my message"),
        keys("esc c enter"),
        check(|app| assert_eq!(draft(app), "")),
    ]);
    assert_eq!(git(&path, &["log", "-1", "--format=%s"]), "my message\n");
}

#[test]
fn test_fold_merge_keeps_the_focus_of_the_commit_list_in_the_details() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    git(
        &path,
        &["merge", "-q", "--no-ff", "--no-edit", "-s", "ours", "feat"],
    );
    git(&path, &["branch", "-q", "-d", "feat"]);
    let mut harness = Harness::new(&path, launch());
    let focused = |expected: DetailFocus| {
        check(move |app| {
            let View::Detail(view) = &app.view else {
                panic!("no details")
            };
            assert_eq!(view.focus(), expected);
        })
    };
    harness.run(vec![
        // the details on the changes tab, the commit list above them focused
        keys("4 2"),
        focused(DetailFocus::List),
        // folding the merge draws the graph again, a new page from the same context
        keys("h"),
        check(|app| assert_eq!(app.shell.folded_merges().len(), 1)),
        focused(DetailFocus::List),
        // and into the details, which the focus leaves the list for
        keys("enter"),
        focused(DetailFocus::Files),
    ]);
}

#[test]
fn test_edit_opens_the_files_of_the_details() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    let edits = |expected: bool| {
        check(move |app| {
            assert_eq!(
                app.view.keys().takes(crate::keybind::UserEvent::Edit),
                expected
            )
        })
    };
    // gone from the working tree, it can't be opened: the editor isn't run here
    std::fs::remove_file(path.join("f.txt")).unwrap();
    harness.run(vec![
        // not on the commits
        edits(false),
        // the files of the changes tab, and the diff beside them
        keys("4"),
        edits(true),
        keys("enter"),
        edits(true),
        keys("e"),
        check(|app| assert_notice(app, "f.txt does not exist in the working tree")),
        // the files tab
        keys("esc 5"),
        edits(true),
    ]);
}

#[test]
fn test_refresh_keeps_the_focus_of_the_panels_in_the_details() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    let focused = |expected: DetailFocus| {
        check(move |app| {
            let View::Detail(view) = &app.view else {
                panic!("no details")
            };
            assert_eq!(view.focus(), expected);
        })
    };
    harness.run(vec![
        // the diff beside the files of the changes tab, then refreshed
        keys("4 enter"),
        focused(DetailFocus::Pane),
        keys("ctrl-r"),
        focused(DetailFocus::Pane),
        // and the preview beside the files of the files tab
        keys("esc 5 enter"),
        focused(DetailFocus::Pane),
        keys("ctrl-r"),
        focused(DetailFocus::Pane),
        // the files, back from the preview
        keys("esc"),
        focused(DetailFocus::Files),
        keys("ctrl-r"),
        focused(DetailFocus::Files),
    ]);
}

#[test]
fn test_refresh_keeps_the_focus_of_the_panels_on_the_local_changes_page() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    std::fs::write(path.join("f.txt"), "a\nCHANGED\nc\n").unwrap();
    let mut harness = Harness::new(
        &path,
        Launch {
            page: StartPage::LocalChanges,
            ..launch()
        },
    );
    // whether the diff beside the files has the keys, by what only it lists
    let in_diff = |expected: bool| {
        let listed = |app: &App| {
            app.view
                .keys()
                .hints()
                .iter()
                .any(|(_, short)| short == "stage hunk")
        };
        check(move |app| assert_eq!(listed(app), expected))
    };
    harness.run(vec![
        in_diff(false),
        // into the diff of the selected file, then refreshed
        keys("enter"),
        in_diff(true),
        keys("ctrl-r"),
        in_diff(true),
        // and back to the files
        keys("esc"),
        in_diff(false),
        keys("ctrl-r"),
        in_diff(false),
    ]);
}

#[test]
fn test_clearing_a_search_clears_its_status_line() {
    // the keys to a search, and the page they are on
    let pages = [
        ("", StartPage::Graph),
        // the file trees are filtered, their diff or preview searched
        ("4 enter", StartPage::Graph),
        ("5 shift-b l", StartPage::Graph),
        ("5 shift-t", StartPage::Graph),
        ("enter", StartPage::LocalChanges),
        // the refs sidebar
        ("1", StartPage::Graph),
    ];
    for (to_search, page) in pages {
        let dir = tempfile::tempdir().unwrap();
        let path = repository(dir.path());
        std::fs::write(path.join("f.txt"), "changed\n").unwrap();
        let mut harness = Harness::new(&path, Launch { page, ..launch() });
        let mut steps = vec![];
        if !to_search.is_empty() {
            steps.push(keys(to_search));
        }
        steps.extend([
            keys("/"),
            typing("f"),
            keys("enter"),
            check(move |app| {
                let said = &app.shell.app_status.status_line;
                assert!(
                    matches!(said, StatusLine::SearchResult { .. }),
                    "{to_search:?} {said:?}"
                );
            }),
            keys("esc"),
            check(move |app| {
                let said = &app.shell.app_status.status_line;
                assert!(matches!(said, StatusLine::None), "{to_search:?} {said:?}");
            }),
        ]);
        harness.run(steps);
    }
}

#[test]
fn test_compare_with_the_current_branch_dims_the_commits_off_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    // the commits shown with whether they are on the current branch, in any order, as the two
    // newest ones share a timestamp and either may come first
    let on_head = |app: &App| -> Vec<String> {
        let state = app.view.as_list_state().expect("a commit list");
        let mut shown: Vec<String> = state
            .on_head()
            .into_iter()
            .map(|(subject, on_head)| format!("{subject}{}", if on_head { "" } else { " dimmed" }))
            .collect();
        shown.sort();
        shown
    };
    harness.run(vec![
        check(move |app| assert_eq!(on_head(app), ["c1", "c2", "feat1 dimmed"])),
        // comparing with feat, its own commit stays dimmed, the graph loaded from the two refs
        compare_feat(),
        check(move |app| assert_eq!(on_head(app), ["c1", "c2", "feat1 dimmed"])),
    ]);
}

// What a sidebar diff compares, and whether the graph then dims the commits off the current
// branch: only when one of the two is it.
#[test]
fn test_compare_with_the_current_branch() {
    use crate::git::{NamedRef, Repository as Repo, Rev, SortCommit};
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let loaded = Repo::load(&path, SortCommit::Chronological, None, false).unwrap();
    let branch = |name: &str| Rev::Ref(NamedRef::Branch(name.into()));
    let compare = |base: Rev, target: Rev| super::Compare { base, target };

    // the current branch is master
    assert!(super::compare_with_head(
        &compare(branch("master"), branch("feat")),
        &loaded
    ));
    assert!(super::compare_with_head(
        &compare(Rev::Head, branch("feat")),
        &loaded
    ));
    // two others, or a commit, are not it
    assert!(!super::compare_with_head(
        &compare(branch("feat"), Rev::Commit("a".repeat(40).as_str().into())),
        &loaded
    ));
}

#[test]
fn test_the_graph_follows_the_sidebar_only_when_configured() {
    // the subject of the commit the graph selects
    fn selected(app: &App) -> String {
        let state = app.view.as_list_state().expect("a commit list");
        let hash = state.selected_commit_hash();
        app.repository.commit(hash).unwrap().subject.clone()
    }

    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());

    // by default the sidebar moves alone; leaving it selects the ref it was on, master's commit
    let first = Rc::new(RefCell::new(String::new()));
    let (start, unchanged) = (first.clone(), first.clone());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        check(move |app| *start.borrow_mut() = selected(app)),
        // the sidebar walks on to master
        keys("1 j j"),
        check(move |app| assert_eq!(selected(app), *unchanged.borrow())),
        keys("enter"),
        check(|app| assert_eq!(selected(app), "c2")),
    ]);

    // following it, the graph selects the commit of the ref the sidebar shows
    let mut harness = Harness::new(&path, launch());
    Rc::get_mut(&mut harness.ctx).unwrap().ui_config.refs.follow = true;
    harness.run(vec![
        keys("1 j j"),
        check(|app| assert_eq!(selected(app), "c2")),
    ]);
}
