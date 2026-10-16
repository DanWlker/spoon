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
mod layout;
mod protocol;
mod ref_commands;
mod search;
mod view;
mod widget;

#[cfg(test)]
#[path = "tests/mailmap.rs"]
mod mailmap_tests;

#[cfg(test)]
mod test_support;

use std::{path::Path, rc::Rc};

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

    let graph_color_set = color::GraphColorSet::new(&graph_config.color);

    let ctx = Rc::new(app::AppContext {
        keybind,
        core_config,
        ui_config,
        color_theme,
        image_protocol,
        running_task: Default::default(),
        key_names: Default::default(),
        layout: Default::default(),
        wrap: Default::default(),
    });

    let ec = event::EventController::init();
    let mut refresh = None;
    let mut terminal_guard = None;
    let mut terminal = None;

    let ret = loop {
        let revisions = refresh
            .as_ref()
            .and_then(|(shell, _): &(app::AppShell, _)| shell.graph_revisions());
        let load = |revisions: Option<&[String]>| {
            git::Repository::load_scoped(Path::new("."), order, max_count, mailmap, revisions)
        };
        let repository = match load(revisions.as_deref()) {
            Ok(repository) => repository,
            // the refs shown may be gone, then all are shown again
            Err(_) if revisions.is_some() => {
                if let Some((shell, _)) = refresh.as_mut() {
                    shell.reset_graph_filter();
                }
                load(None)?
            }
            Err(e) => return Err(e),
        };

        let graph = graph::calc_graph(&repository, primary_branch.as_deref());

        let cell_width_type = check::decide_cell_width_type(&graph, graph_width)?;

        let image_params =
            graph::ImageParams::new(&graph_color_set, cell_width_type, graph_antialias)
                .fit_cell(protocol::cell_pixels());
        let graph_image_manager = GraphImageManager::new(
            &graph,
            image_params,
            cell_width_type,
            graph_style,
            graph_image_width_mode,
            image_protocol,
        );

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
            ec.start_refresher(
                repository.path().to_path_buf(),
                std::time::Duration::from_secs(2),
                std::time::Duration::from_secs(10),
            );
        }

        let mut app = App::new(
            &repository,
            graph_image_manager,
            initial_selection,
            ctx.clone(),
            &ec,
            refresh,
        );

        match app.run(terminal.as_mut().unwrap()) {
            Ok(Ret::Quit) => {
                break Ok(());
            }
            Ok(Ret::Refresh(context)) => {
                refresh = Some((app.into_shell(), context));
                continue;
            }
            Err(e) => {
                break Err(e);
            }
        }
    };

    drop(terminal_guard);
    ret.map_err(Into::into)
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
