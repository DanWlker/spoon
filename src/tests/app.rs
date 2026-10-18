// Tests of the whole app, run as `main` runs it but without a terminal: the keys and the changes
// made to the repository meanwhile are the steps of a script, see `Harness::run`.

use std::{
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    layout::Rect,
};

use super::{
    App, AppContext, AppEvent, GraphFilter, InitialSelection, Launch, Ret, StartPage, StatusLine,
};
use crate::{
    color::GraphColorSet,
    config::GraphColorConfig,
    event::{EventController, KeyHint},
    git::{refs_snapshot, FilesSnapshot, NamedRef, SortCommit},
    graph::{GraphImageWidthMode, GraphStyle},
    keybind::parse_key_event,
    layout::Panel,
    protocol::ImageProtocol,
    test_support::{git::git, ui::test_ctx},
    view::View,
    widget::{commit_list::SearchOutcome, ref_list::RefSelection},
    Loader, Refresh,
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
    // how many times the repository was loaded, the first time included
    loads: usize,
    quit: bool,
}

impl Harness {
    fn new(path: &Path, launch: Launch) -> Self {
        let loader = Loader {
            path: path.to_path_buf(),
            order: SortCommit::Chronological,
            max_count: None,
            mailmap: false,
            primary_branch: None,
            graph_width: None,
            graph_style: GraphStyle::Rounded,
            graph_antialias: false,
            graph_image_width_mode: GraphImageWidthMode::Compact,
            graph_color_set: GraphColorSet::new(&GraphColorConfig::default()),
            image_protocol: ImageProtocol::Kitty,
            initial_selection: InitialSelection::Latest,
            launch,
        };
        Harness {
            path: path.to_path_buf(),
            loader,
            ctx: test_ctx(),
            ec: EventController::without_terminal(),
            loads: 0,
            quit: false,
        }
    }

    // Starts the app and runs the steps on it, one after another; after each, the events it sent,
    // the git command it runs in the background and the reloads are handled, as the app would
    // before the next key.
    fn run(&mut self, steps: Vec<Vec<Step>>) {
        let mut steps = steps.into_iter().flatten();
        let mut refresh: Refresh = None;
        let mut repository = None;
        let mut load = true;
        let (path, ctx, ec, quit) = (&self.path, &self.ctx, &self.ec, &mut self.quit);
        loop {
            let loaded = if load {
                self.loads += 1;
                self.loader
                    .load(&mut refresh, &mut repository, &ctx.running_task)
                    .unwrap()
            } else {
                repository.as_ref().unwrap()
            };
            // what the app was before reloading, None once done
            let next = self
                .loader
                .with_app(loaded, SIZE, ctx, ec, refresh.take(), |mut app| loop {
                    if let Some(ret) = settle(&mut app, ec, ctx) {
                        return match ret {
                            Ret::Quit => {
                                *quit = true;
                                None
                            }
                            Ret::Refresh(context) => Some((app.into_shell(), context, true)),
                            Ret::Regraph(context) => Some((app.into_shell(), context, false)),
                        };
                    }
                    take_step(steps.next()?, &mut app, path, ec);
                })
                .unwrap();
            match next {
                None => return,
                Some((shell, context, reload)) => {
                    refresh = Some((shell, context));
                    load = reload;
                }
            }
        }
    }
}

// Handles the events sent meanwhile, waiting for the git command running in the background;
// returns what ends or reloads the app.
fn settle(app: &mut App, ec: &EventController, ctx: &AppContext) -> Option<Ret> {
    loop {
        app.expire(Instant::now());
        app.prepare_render(Rect::new(0, 0, SIZE.0, SIZE.1));
        let event = if ctx.running_task.borrow().is_some() {
            ec.recv_timeout(TIMEOUT)
                .expect("the git command running to finish")
        } else {
            ec.try_recv()?
        };
        if let Some(ret) = app.handle_event(event).unwrap() {
            return Some(ret);
        }
    }
}

fn take_step(step: Step, app: &mut App, path: &Path, ec: &EventController) {
    match step {
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
    }
}

fn launch() -> Launch {
    Launch {
        page: StartPage::Graph,
        fetch: false,
    }
}

// A repository at `dir/work`: f.txt changed on master after the branch feat, which changes it too.
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
    let commit = |file: &str, content: &str, message: &str| {
        std::fs::write(path.join(file), content).unwrap();
        git(&path, &["add", file]);
        git(&path, &["commit", "-q", "-m", message]);
    };
    commit("f.txt", "a\nb\nc\n", "c1");
    run(&["checkout", "-q", "-b", "feat"]);
    commit("f.txt", "a\nFEAT\nc\n", "feat1");
    run(&["checkout", "-q", "master"]);
    commit("f.txt", "a\nMASTER\nc\n", "c2");
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
    app.shell.graph_filter_note(app.repository)
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
        keys("0"),
        mode(true, false),
        // again, it shrinks and the page has the focus
        keys("0"),
        mode(false, true),
        keys("0"),
        mode(true, false),
        keys("1"),
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
        keys("0"),
        focused(true),
        // the local changes page takes the focus from it, and has none to give it
        keys("tab"),
        check(|app| assert!(matches!(app.view, View::LocalChanges(_)))),
        focused(false),
        keys("0 alt-h"),
        focused(false),
        keys("tab 0"),
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
        keys("f1"),
        named("Help"),
        keys("esc @"),
        named("Command Log"),
        keys("esc 4 shift-b"),
        named("Blame"),
        named("f.txt"),
        // back to the commit shown, then closed
        keys("esc esc shift-t"),
        named("Timeline"),
    ]);
}

#[test]
fn test_help_and_command_log_cover_the_app_one_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        keys("f1"),
        overlay("help"),
        // one panel, like the command log's, scrolled with its scrollbar
        screen(|screen| {
            assert!(screen.contains("╭ Help "), "{screen}");
            let thumb = screen.lines().any(|l| l.ends_with('█'));
            assert!(thumb, "{screen}");
        }),
        // the help takes the keys
        keys("@"),
        overlay("help"),
        keys("esc"),
        overlay("none"),
        keys("@"),
        overlay("log"),
        keys("f1"),
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
    assert_eq!(harness.loads, 2);
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
    assert_eq!(harness.loads, 1);
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
            assert!(app.ctx.running_task.borrow().is_none());
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
    assert_eq!(harness.loads, 2);
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
    assert_eq!(harness.loads, 3);
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

// Comparing refs diffed from the sidebar, and commits diffed in the graph

// Diffs feat with HEAD from the sidebar.
fn compare_feat() -> Vec<Step> {
    let mut steps = keys("0 j shift-d");
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
        keys("0 j"),
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
        keys("shift-r"),
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
        keys("3 u"),
        check(|app| assert!(app.shell.dialog.is_some())),
        keys("esc enter u"),
        check(|app| assert!(app.shell.dialog.is_none())),
        // nor filters them
        keys("?"),
        check(|app| assert!(!app.is_typing())),
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
        keys("3"),
        check(move |app| {
            let shown = shown(app);
            assert!(shown.contains(&"copy path".to_string()), "{shown:?}");
            assert!(!shown.contains(&"copy hash".to_string()), "{shown:?}");
            assert!(!shown.contains(&"details".to_string()), "{shown:?}");
        }),
        // and the other way round
        keys("1"),
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
        keys("c"),
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
    let aborts = |app: &App| app.view.keys().takes(crate::event::UserEvent::Abort);
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
        *app.ctx.running_task.borrow_mut() = on.then(|| crate::git::RunningTask {
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
        keys("shift-r"),
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
        keys("3"),
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
        keys("1 v"),
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
fn test_branch_only_of_the_sidebar_branch() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        // feat, under master
        keys("0 j shift-l"),
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
        keys("0 g j"),
        selected("b00"),
        // half of the 36 rows shown
        keys("ctrl-d"),
        selected("b18"),
        keys("ctrl-d ctrl-u"),
        selected("b18"),
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
    assert_eq!(harness.loads, 1);
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
    assert_eq!(harness.loads, 3);
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
        keys("4 shift-b"),
        overlay("blame"),
        // the commit of its first line, c1, on the graph, the details still open
        keys("enter"),
        overlay("none"),
        selected("c1"),
        // checking out the commit shown, c1 again, closes the blame once confirmed
        keys("k 4 shift-b ["),
        keys("0 space"),
        overlay("blame"),
        keys("enter"),
        overlay("none"),
        check(move |app| assert_eq!(app.repository.head_commit().unwrap().as_str(), c1)),
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
        keys("4 shift-b shift-h"),
        popup_has(&[
            "older/newer commit",
            "show on graph",
            "checkout",
            "search",
            "back",
        ]),
        keys("esc esc shift-t"),
        popup_has(&["into the diff", "copy hash", "move", "focus panel"]),
        // typing a search, the popup gives way
        keys("/"),
        popup_has(&[]),
        check(|app| assert!(app.keys_popup_keys().is_empty())),
        // over the command log, not the help, which lists every key
        keys("esc esc @"),
        popup_has(&["move", "copy", "close"]),
        keys("esc f1"),
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
        keys("4 / ctrl-g ctrl-x"),
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
        keys("4 shift-b l"),
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
        keys("ctrl-right"),
        check(|app| assert_ne!(app.ctx.layout.borrow().size(Panel::BlameCommits, 0), 0)),
        keys("alt-up shift-h"),
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
        keys("ctrl-left alt-right shift-h"),
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

#[test]
fn test_no_match_said_alike_on_every_page() {
    // the keys to a search, and the page they are on
    let pages = [
        ("", StartPage::Graph),
        ("4", StartPage::Graph),
        ("4 shift-b l", StartPage::Graph),
        ("4 shift-t", StartPage::Graph),
        ("", StartPage::LocalChanges),
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
fn test_diff_keys_alike_on_every_page() {
    // the keys into a diff, at its hunks, and the page they are on
    let pages = [
        ("3 enter", StartPage::Graph),
        ("4 shift-t l", StartPage::Graph),
        ("enter", StartPage::LocalChanges),
    ];
    // what the keys popup names enter and y
    let named = |enter: Option<&'static str>, copy: &'static str, to: &'static str| {
        check(move |app| {
            let keys = app.keys_popup_keys();
            let action = |key: &str| {
                let label = keys.iter().find(|k| k.keys == key);
                label.map(|k| k.action.as_str())
            };
            assert_eq!(
                (action("enter"), action("y")),
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
        ("3 enter", StartPage::Graph, "[2] Info"),
        ("4 shift-t", StartPage::Graph, "[0] Commits"),
        ("4 shift-t l", StartPage::Graph, "[1] Diff"),
        ("4 shift-b l", StartPage::Graph, "[1] Blame"),
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
        keys("4 shift-t"),
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
fn test_diff_of_commits_stays_open_on_reload() {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path());
    let mut harness = Harness::new(&path, launch());
    harness.run(vec![
        // c1, the first, diffed with HEAD
        keys("shift-g shift-d"),
        check(|app| assert_eq!(compared(app), Some(vec!["f.txt".to_string()]))),
        keys("shift-r"),
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
        keys("0 ] j l j shift-d"),
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
        keys("shift-c enter"),
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
        keys("shift-g c enter"),
        check(|app| {
            assert_notice(app, "would be overwritten by checkout");
            assert!(app.repository.current_branch().as_deref() == Some("master"));
        }),
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
        keys("shift-c enter"),
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
        keys("0 ] ] j"),
        check(|app| {
            let selection = app.shell.sidebar.selection();
            assert_eq!(selection, RefSelection::Ref(NamedRef::Tag("v1".into())));
        }),
        // also from origin
        keys("d v enter"),
        check(|app| assert_notice(app, "git push failed")),
    ]);
    assert_eq!(git(&path, &["tag"]), "v1\n");
}

#[test]
fn test_edit_branch_upstream_only_chosen() {
    let dir = tempfile::tempdir().unwrap();
    let path = with_remote(dir.path());
    let mut harness = Harness::new(&path, launch());
    let mut steps = vec![keys("0 j e")];
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
    let mut steps = vec![keys("0 ] j a")];
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
    assert_eq!(harness.loads, 2);
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
