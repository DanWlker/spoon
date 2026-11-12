mod app;
mod background;
mod browse;
mod check;
mod color;
mod config;
mod context;
mod error;
mod event;
mod external;
mod git;
mod graph;
mod graph_commands;
mod keybind;
mod keys;
mod layout;
mod pointer;
mod protocol;
mod ref_commands;
mod refresher;
mod search;
mod view;
mod widget;

#[cfg(test)]
#[path = "tests/mailmap.rs"]
mod mailmap_tests;

#[cfg(test)]
mod test_support;

use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::Arc};

use clap::Parser;
use ratatui::style::Stylize;

use crate::{
    app::{App, Ret},
    config::{CommitOrderType, GraphStyle, GraphWidthType, ImageProtocolType, InitialSelection},
    error::Result,
    graph::GraphImageManager,
};

/// Spoon - A lazygit-style git TUI with a rich commit graph
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Maximum number of commits to render
    #[arg(short = 'n', long, value_name = "NUMBER")]
    max_count: Option<std::num::NonZeroUsize>,

    /// Image protocol to render graph [default: auto]
    #[arg(short, long, value_name = "TYPE")]
    protocol: Option<ImageProtocolType>,

    /// Commit ordering algorithm [default: chrono]
    #[arg(short, long, value_name = "TYPE")]
    order: Option<CommitOrderType>,

    /// Commit graph image cell width [default: auto]
    #[arg(short, long, value_name = "TYPE")]
    graph_width: Option<GraphWidthType>,

    /// Commit graph image edge style [default: rounded]
    #[arg(short = 's', long, value_name = "TYPE")]
    graph_style: Option<GraphStyle>,

    /// Enable antialiasing for commit graph images
    #[arg(long, conflicts_with = "no_graph_antialias")]
    graph_antialias: bool,

    /// Disable antialiasing for commit graph images
    #[arg(long, conflicts_with = "graph_antialias")]
    no_graph_antialias: bool,

    /// Initial selection of commit [default: latest]
    #[arg(short, long, value_name = "TYPE")]
    initial_selection: Option<InitialSelection>,

    /// Primary branch to keep on the leftmost column
    #[arg(short = 'b', long, value_name = "BRANCH")]
    primary_branch: Option<String>,

    /// Start on the local changes page
    #[arg(long)]
    local_changes: bool,
}

impl Args {
    fn resolve_graph_antialias(&self, configured: Option<bool>) -> bool {
        if self.graph_antialias {
            true
        } else if self.no_graph_antialias {
            false
        } else {
            configured.unwrap_or(false)
        }
    }
}

impl From<Option<ImageProtocolType>> for protocol::ImageProtocol {
    fn from(protocol: Option<ImageProtocolType>) -> Self {
        match protocol {
            Some(ImageProtocolType::Auto) => protocol::auto_detect(),
            Some(ImageProtocolType::Iterm) => protocol::ImageProtocol::Iterm2,
            Some(ImageProtocolType::Kitty) => protocol::ImageProtocol::Kitty,
            Some(ImageProtocolType::KittyUnicode) => protocol::ImageProtocol::KittyUnicode {
                tmux: protocol::detect_tmux(),
            },
            None => protocol::auto_detect(),
        }
    }
}

impl From<Option<CommitOrderType>> for git::SortCommit {
    fn from(order: Option<CommitOrderType>) -> Self {
        match order {
            Some(CommitOrderType::Chrono) => git::SortCommit::Chronological,
            Some(CommitOrderType::Topo) => git::SortCommit::Topological,
            None => git::SortCommit::Chronological,
        }
    }
}

impl From<Option<GraphStyle>> for graph::GraphStyle {
    fn from(style: Option<GraphStyle>) -> Self {
        match style {
            Some(GraphStyle::Rounded) => graph::GraphStyle::Rounded,
            Some(GraphStyle::Angular) => graph::GraphStyle::Angular,
            Some(GraphStyle::Curved) => graph::GraphStyle::Curved,
            None => graph::GraphStyle::Rounded,
        }
    }
}

// How often the branches, HEAD and the operation in progress are checked for changes made outside
// the app: each check runs 3-4 git processes, so shorter costs CPU, longer leaves another terminal's checkout unseen.
const REFS_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
// How often the local changes are checked: git status walks the whole working tree, which is
// slow in big repositories; focusing the terminal checks right away, see `refresh_now`.
const FILES_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

// The error in words, like toml's naming the line of the config, not as Rust's `Debug` shows it.
fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("Error: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let args = Args::parse();
    let (core_config, ui_config, graph_config, color_theme, keybind_patch) = config::load()?;
    let keybind = keybind::KeyBind::new(keybind_patch);

    let max_count = args.max_count.map(std::num::NonZeroUsize::get);
    let image_protocol = args.protocol.or(core_config.option.protocol).into();
    let order = args.order.or(core_config.option.order).into();
    let graph_width = args.graph_width.or(core_config.option.graph_width);
    let graph_style = args.graph_style.or(core_config.option.graph_style).into();
    let graph_antialias = args.resolve_graph_antialias(core_config.option.graph_antialias);
    let graph_image_width_mode = graph_config.row_image_width;
    let initial_selection = args
        .initial_selection
        .or(core_config.option.initial_selection)
        .unwrap_or(InitialSelection::Latest);
    let mailmap = core_config.git.mailmap;
    let primary_branch = args.primary_branch.filter(|s| !s.is_empty());
    let launch = app::Launch {
        page: if args.local_changes {
            app::StartPage::LocalChanges
        } else {
            app::StartPage::Graph
        },
        selection: initial_selection,
        fetch: true,
    };

    let graph_color_set = color::GraphColorSet::new(&graph_config.color);
    let loader = Loader {
        options: git::LoadOptions {
            path: PathBuf::from("."),
            sort: order,
            max_count,
            mailmap,
            primary_branch,
        },
        graph_width,
        graph_style,
        graph_antialias,
        graph_image_width_mode,
        graph_color_set: graph_color_set.clone(),
        image_protocol,
        launch,
    };

    let ctx = Rc::new(context::AppContext {
        keybind,
        core_config,
        ui_config,
        color_theme,
        image_protocol,
        graph: RefCell::new(context::GraphRender {
            color_set: graph_color_set,
            style: graph_style,
            antialias: graph_antialias,
            image_width_mode: graph_image_width_mode,
            cell_width_type: graph::CellWidthType::Double,
        }),
        running_task: Default::default(),
        key_names: Default::default(),
        layout: Default::default(),
        wrap: Default::default(),
        diff_context: std::cell::Cell::new(crate::git::DEFAULT_DIFF_CONTEXT),
        search_options: Default::default(),
        pasting: Default::default(),
    });

    let mut terminal = ratatui::init();
    // a panic is a bug, which stops the app: ratatui's hook gives the terminal back and prints it;
    // on another thread the main one would go on, so the app exits there, before `TerminalGuard`
    // is dropped. Set before the threads start, the input's first.
    let crash = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        crash(info);
        if std::thread::current().name() != Some("main") {
            std::process::exit(101);
        }
    }));
    let ec = event::EventController::init();
    // gives the terminal back however this ends, an error loading the repository included
    let terminal_guard = TerminalGuard;
    // the focus coming back reloads what changed meanwhile, like lazygit; the mouse drags the
    // borders of the panels
    let _ = ratatui::crossterm::execute!(
        std::io::stdout(),
        ratatui::crossterm::event::EnableFocusChange,
        ratatui::crossterm::event::EnableMouseCapture,
        ratatui::crossterm::event::EnableBracketedPaste
    );
    let Some((repository, layout)) = loader.load_showing(&ctx, &ec, &mut terminal)? else {
        return Ok(());
    };
    // loaded again in the background by the app, which hands it over, see `Ret::Reload`
    let mut repository = Arc::new(repository);
    ec.set_refresher(refresher::spawn(
        ec.sender(),
        repository.path().to_path_buf(),
        repository.git_dir().to_path_buf(),
        REFS_INTERVAL,
        FILES_INTERVAL,
    ));
    let mut restart = Restart {
        before: None,
        layout: Some(layout),
        images: None,
    };

    let ret = loop {
        let size = ratatui::crossterm::terminal::size()?;
        let ran = loader.with_app(&repository, size, &ctx, &ec, restart, |mut app| {
            app.run(&mut terminal).map(|ret| match ret {
                Ret::Quit => None,
                Ret::Reload {
                    context,
                    repository,
                    layout,
                    images,
                } => Some((app.into_shell(), context, layout, Some(repository), images)),
                Ret::Regraph {
                    context,
                    layout,
                    images,
                } => Some((app.into_shell(), context, layout, None, images)),
            })
        });
        match ran {
            Ok(Ok(None)) => break Ok(()),
            Ok(Ok(Some((shell, context, layout, reloaded, images)))) => {
                restart = Restart {
                    before: Some((shell, context)),
                    layout: Some(*layout),
                    images,
                };
                if let Some(reloaded) = reloaded {
                    repository = Arc::new(*reloaded);
                }
            }
            Ok(Err(e)) => break Err(e.into()),
            Err(e) => break Err(e),
        }
    };

    drop(terminal_guard);
    ret
}

// How the app is made again: as it was before, its shell and what its view showed (None when it
// starts), with the graph laid out on the thread loading the repository or folding a merge.
struct Restart {
    before: Option<(app::AppShell, Box<view::RefreshViewContext>)>,
    layout: Option<graph::GraphLayout>,
    // the graph images of the app before, see `Ret::Regraph`
    images: Option<graph::GraphImages>,
}

// What the repository and its graph are loaded with, the same on each reload; the tests load
// with it too, see `tests/app.rs`.
struct Loader {
    options: git::LoadOptions,
    graph_width: Option<GraphWidthType>,
    graph_style: graph::GraphStyle,
    graph_antialias: bool,
    graph_image_width_mode: graph::GraphImageWidthMode,
    graph_color_set: color::GraphColorSet,
    image_protocol: protocol::ImageProtocol,
    launch: app::Launch,
}

impl Loader {
    // Loads the repository the app starts with, and lays its graph out, as work to run on
    // another thread; the app reloads it in the background after.
    fn load_work(
        &self,
        running: &git::SharedTask,
    ) -> impl FnOnce() -> std::result::Result<(git::Repository, graph::GraphLayout), String> + Send
    {
        let (options, running) = (self.options.clone(), running.clone());
        move || {
            let (loaded, layout) =
                app::load_laid_out(&options, None, running, &Default::default())?;
            let (git::Loaded::Repository(repository) | git::Loaded::RefsGone(repository)) = loaded;
            Ok((repository, layout))
        }
    }

    // `load_work` on another thread, saying so on the screen once it takes long (see
    // `shows_loading`), blank before; None when quit before.
    fn load_showing(
        &self,
        ctx: &context::AppContext,
        ec: &event::EventController,
        terminal: &mut ratatui::DefaultTerminal,
    ) -> Result<Option<(git::Repository, graph::GraphLayout)>> {
        let job = background::Job::spawn(ec.sender(), self.load_work(&ctx.running_task));
        let path = std::fs::canonicalize(&self.options.path).unwrap_or(self.options.path.clone());
        let message = format!("Loading {}…", path.display());
        loop {
            if let Some(loaded) = job.try_take() {
                return Ok(Some(loaded?));
            }
            terminal.draw(|f| {
                if !background::shows_loading(job.started()) {
                    return;
                }
                let line = ratatui::text::Line::raw(message.as_str()).fg(ctx.color_theme.muted_fg);
                let area = f.area();
                let row =
                    ratatui::layout::Rect::new(area.x, area.y + area.height / 2, area.width, 1);
                f.render_widget(ratatui::widgets::Paragraph::new(line).centered(), row);
            })?;
            if let event::AppEvent::Key(key) = ec.recv() {
                let quits = matches!(
                    ctx.keybind.event_in(keybind::Scope::Global, &key),
                    Some(keybind::UserEvent::Quit | keybind::UserEvent::ForceQuit)
                );
                if quits {
                    return Ok(None);
                }
            }
        }
    }

    // Runs `run` on the app of `repository`, its graph's cells as wide as a terminal of `size`
    // (columns, rows) fits when it starts, as it was before when made again (`restart`).
    fn with_app<R>(
        &self,
        repository: &Arc<git::Repository>,
        size: (u16, u16),
        ctx: &Rc<context::AppContext>,
        ec: &event::EventController,
        restart: Restart,
        run: impl FnOnce(App) -> R,
    ) -> Result<R> {
        let Restart {
            before: refresh,
            layout,
            images,
        } = restart;
        let folded = refresh
            .as_ref()
            .map(|(shell, _)| shell.folded_merges().clone())
            .unwrap_or_default();
        // laid out on the thread reloading it, unless a merge was folded or unfolded meanwhile
        let primary_branch = self.options.primary_branch.as_deref();
        let graph = match layout {
            Some(layout) if layout.folded == folded => graph::Graph::of(repository, layout),
            _ => graph::calc_graph(repository, primary_branch, &folded),
        };
        // picked at the start and kept when made again: the graph then is cut like on a resize,
        // instead of the app quitting on a reload in a narrow terminal
        let cell_width_type = match &refresh {
            None => {
                // the commits panel, inside its borders, beside the sidebar
                let page = size
                    .0
                    .saturating_sub(app::sidebar_width(ctx, size.0))
                    .saturating_sub(2);
                check::decide_cell_width_type(&graph, size, page, self.graph_width)?
            }
            Some(_) => ctx.graph.borrow().cell_width_type,
        };
        ctx.graph.borrow_mut().cell_width_type = cell_width_type;
        let image_params =
            graph::ImageParams::new(&self.graph_color_set, cell_width_type, self.graph_antialias)
                .fit_cell(protocol::cell_pixels());
        let graph_image_manager = GraphImageManager::new(
            graph,
            image_params,
            cell_width_type,
            self.graph_style,
            self.graph_image_width_mode,
            self.image_protocol,
        );
        let graph_image_manager = match images {
            Some(images) => graph_image_manager.with_images(images),
            None => graph_image_manager,
        };
        let app = App::new(
            repository,
            self.options.clone(),
            graph_image_manager,
            self.launch,
            ctx.clone(),
            ec,
            refresh,
        );
        Ok(run(app))
    }
}

// Gives the terminal back however the app ends: quitting, an error while reloading, or a panic
// on the main thread.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

// Turns off what the app turned on in the terminal: ratatui's own restoring leaves the mouse
// captured.
fn restore_terminal() {
    let _ = ratatui::crossterm::execute!(
        std::io::stdout(),
        ratatui::crossterm::event::DisableFocusChange,
        ratatui::crossterm::event::DisableMouseCapture,
        ratatui::crossterm::event::DisableBracketedPaste
    );
    ratatui::restore();
}
