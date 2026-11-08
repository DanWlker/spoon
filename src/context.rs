// What every page draws and handles keys with: the config, the theme, the keys, and the state
// shared across pages and refreshes.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use rustc_hash::FxHashMap;

use crate::{
    color::ColorTheme,
    color::GraphColorSet,
    config::{CoreConfig, GraphColorConfig, UiConfig},
    git::{RunningTask, SharedTask},
    graph::{CellWidthType, Graph, GraphImageManager, GraphImageWidthMode, GraphStyle},
    keybind::{KeyBind, Scope, UserEvent},
    layout::PanelLayout,
    protocol::ImageProtocol,
    search::SearchOptions,
};

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
    pub key_names: RefCell<FxHashMap<(Scope, Vec<UserEvent>), String>>,
    // the sizes of the panels, set by the user, kept across refreshes
    pub layout: RefCell<PanelLayout>,
    // long lines of the diffs and file previews wrapped, see `UserEvent::ToggleWrap`
    pub wrap: Cell<bool>,
    // the lines shown around each change in a diff, see `AppContext::adjust_diff_context`
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
    // Shows a line more or fewer around the changes of every diff, by `event`, at least one: git
    // applies no hunk staged without any. Returns whether it changed.
    pub fn adjust_diff_context(&self, event: UserEvent) -> bool {
        let context = self.diff_context.get();
        let adjusted = match event {
            UserEvent::IncreaseDiffContext => context.saturating_add(1),
            _ => context.saturating_sub(1).max(1),
        };
        self.diff_context.set(adjusted);
        adjusted != context
    }

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
    pub fn short_keys(&self, scope: Scope, events: &[UserEvent]) -> String {
        let mut names = self.key_names.borrow_mut();
        names
            .entry((scope, events.to_vec()))
            .or_insert_with(|| self.keybind.short_keys(scope, events))
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
