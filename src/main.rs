mod app;
mod browse;
mod check;
mod color;
mod config;
mod event;
mod external;
mod git;
mod graph;
mod graph_commands;
mod keybind;
mod keys;
mod layout;
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

use std::{cell::RefCell, path::PathBuf, rc::Rc};

use clap::{Parser, ValueEnum};
use serde::Deserialize;

use crate::{
    app::{App, Ret},
    graph::GraphImageManager,
};

/// Spoon - A lazygit-style git TUI with a rich commit graph
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Maximum number of commits to render
    #[arg(short = 'n', long, value_name = "NUMBER")]
    max_count: Option<usize>,

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ImageProtocolType {
    Auto,
    Iterm,
    Kitty,
    KittyUnicode,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
enum CommitOrderType {
    Chrono,
    Topo,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
enum GraphWidthType {
    Auto,
    Double,
    Single,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
enum GraphStyle {
    Rounded,
    Angular,
    Curved,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
enum InitialSelection {
    Latest,
    Head,
}

impl From<Option<InitialSelection>> for app::InitialSelection {
    fn from(selection: Option<InitialSelection>) -> Self {
        match selection {
            Some(InitialSelection::Latest) => app::InitialSelection::Latest,
            Some(InitialSelection::Head) => app::InitialSelection::Head,
            None => app::InitialSelection::Latest,
        }
    }
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

// How often the branches, HEAD and the operation in progress are checked for changes made outside
// the app: each check runs 3-4 git processes, so shorter costs CPU, longer leaves another terminal's checkout unseen.
const REFS_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
// How often the local changes are checked: git status walks the whole working tree, which is
// slow in big repositories; focusing the terminal checks right away, see `refresh_now`.
const FILES_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

fn main() -> Result<()> {
    let args = Args::parse();
    let (core_config, ui_config, graph_config, color_theme, keybind_patch) = config::load()?;
    let keybind = keybind::KeyBind::new(keybind_patch);

    let max_count = args.max_count;
    let image_protocol = args.protocol.or(core_config.option.protocol).into();
    let order = args.order.or(core_config.option.order).into();
    let graph_width = args.graph_width.or(core_config.option.graph_width);
    let graph_style = args.graph_style.or(core_config.option.graph_style).into();
    let graph_antialias = args.resolve_graph_antialias(core_config.option.graph_antialias);
    let graph_image_width_mode = graph_config.row_image_width;
    let initial_selection = args
        .initial_selection
        .or(core_config.option.initial_selection)
        .into();
    let mailmap = core_config.git.mailmap;
    let primary_branch = args.primary_branch.filter(|s| !s.is_empty());
    let launch = app::Launch {
        page: if args.local_changes {
            app::StartPage::LocalChanges
        } else {
            app::StartPage::Graph
        },
        fetch: true,
    };

    let graph_color_set = color::GraphColorSet::new(&graph_config.color);
    let loader = Loader {
        path: PathBuf::from("."),
        order,
        max_count,
        mailmap,
        primary_branch,
        graph_width,
        graph_style,
        graph_antialias,
        graph_image_width_mode,
        graph_color_set: graph_color_set.clone(),
        image_protocol,
        initial_selection,
        launch,
    };

    let ctx = Rc::new(app::AppContext {
        keybind,
        core_config,
        ui_config,
        color_theme,
        image_protocol,
        graph: RefCell::new(app::GraphRender {
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
        search_options: Default::default(),
    });

    let ec = event::EventController::init();
    let mut refresh = None;
    // the repository last loaded, shown again when reloading it fails
    let mut repository = None;
    let mut terminal_guard = None;
    let mut terminal = None;

    // false to draw the graph again only, see `Ret::Regraph`
    let mut load = true;
    let ret = loop {
        let repository = if load {
            loader.load(&mut refresh, &mut repository, &ctx.running_task)?
        } else {
            repository.as_ref().expect("loaded before")
        };

        if terminal.is_none() {
            terminal = Some(ratatui::init());
            terminal_guard = Some(TerminalGuard);
            // the focus coming back reloads what changed meanwhile, like lazygit; the mouse drags
            // the borders of the panels
            let _ = ratatui::crossterm::execute!(
                std::io::stdout(),
                ratatui::crossterm::event::EnableFocusChange,
                ratatui::crossterm::event::EnableMouseCapture
            );
            ec.set_refresher(refresher::spawn(
                ec.sender(),
                repository.path().to_path_buf(),
                repository.git_dir().to_path_buf(),
                REFS_INTERVAL,
                FILES_INTERVAL,
            ));
        }

        let terminal = terminal.as_mut().unwrap();
        let size = ratatui::crossterm::terminal::size()?;
        let ran = loader.with_app(repository, size, &ctx, &ec, refresh.take(), |mut app| {
            app.run(terminal).map(|ret| match ret {
                Ret::Quit => None,
                Ret::Refresh(context) => Some((app.into_shell(), context, true)),
                Ret::Regraph(context) => Some((app.into_shell(), context, false)),
            })
        });
        match ran {
            Ok(Ok(None)) => break Ok(()),
            Ok(Ok(Some((shell, context, reload)))) => {
                refresh = Some((shell, context));
                load = reload;
            }
            Ok(Err(e)) => break Err(e.into()),
            Err(e) => break Err(e),
        }
    };

    drop(terminal_guard);
    ret
}

// The app as it was before the repository was reloaded: its shell, and what its view showed.
type Refresh = Option<(app::AppShell, Box<view::RefreshViewContext>)>;

// What the repository and its graph are loaded with, the same on each reload; the tests load
// with it too, see `tests/app.rs`.
struct Loader {
    path: PathBuf,
    order: git::SortCommit,
    max_count: Option<usize>,
    mailmap: bool,
    primary_branch: Option<String>,
    graph_width: Option<GraphWidthType>,
    graph_style: graph::GraphStyle,
    graph_antialias: bool,
    graph_image_width_mode: graph::GraphImageWidthMode,
    graph_color_set: color::GraphColorSet,
    image_protocol: protocol::ImageProtocol,
    initial_selection: app::InitialSelection,
    launch: app::Launch,
}

impl Loader {
    // Loads the repository into `repository`, with the history the app shown before the reload
    // (`refresh`) shows. Reloading failing, like on a broken ref, keeps the one loaded before,
    // the app saying why.
    // `running` is the app's git command running in the background, see `git::RunningTask`.
    fn load<'r>(
        &self,
        refresh: &mut Refresh,
        repository: &'r mut Option<git::Repository>,
        running: &Rc<std::cell::RefCell<Option<git::RunningTask>>>,
    ) -> Result<&'r git::Repository> {
        let revisions = refresh
            .as_ref()
            .and_then(|(shell, _)| shell.graph_revisions());
        let load = |revisions: Option<&[String]>| {
            git::Repository::load_scoped(
                &self.path,
                self.order,
                self.max_count,
                self.mailmap,
                revisions,
                running.clone(),
            )
        };
        let loaded = match load(revisions.as_deref()) {
            // the refs shown may be gone, then all are shown again
            Err(_) if revisions.is_some() => {
                if let Some((shell, _)) = refresh.as_mut() {
                    shell.reset_graph_filter();
                }
                load(None)
            }
            loaded => loaded,
        };
        match (loaded, refresh.as_mut(), repository.take()) {
            (Ok(loaded), _, _) => Ok(repository.insert(loaded)),
            (Err(e), Some((shell, _)), Some(mut last)) => {
                shell.notify_error(e.to_string());
                last.take_refs_snapshot();
                Ok(repository.insert(last))
            }
            (Err(e), _, _) => Err(e),
        }
    }

    // Runs `run` on the app of `repository`, its graph laid out for a terminal of `size`
    // (columns, rows), as it was before when reloaded (`refresh`).
    fn with_app<R>(
        &self,
        repository: &git::Repository,
        size: (u16, u16),
        ctx: &Rc<app::AppContext>,
        ec: &event::EventController,
        refresh: Refresh,
        run: impl FnOnce(App) -> R,
    ) -> Result<R> {
        let folded = refresh
            .as_ref()
            .map(|(shell, _)| shell.folded_merges().clone())
            .unwrap_or_default();
        let graph = graph::calc_graph(repository, self.primary_branch.as_deref(), &folded);
        let cell_width_type = check::decide_cell_width_type(&graph, size, self.graph_width)?;
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
        let app = App::new(
            repository,
            graph_image_manager,
            self.initial_selection,
            self.launch,
            ctx.clone(),
            ec,
            refresh,
        );
        Ok(run(app))
    }
}

// Gives the terminal back however the app ends: quitting, an error while reloading, or a panic
// (ratatui's panic hook leaves the mouse captured).
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = ratatui::crossterm::execute!(
            std::io::stdout(),
            ratatui::crossterm::event::DisableFocusChange,
            ratatui::crossterm::event::DisableMouseCapture
        );
        ratatui::restore();
    }
}
