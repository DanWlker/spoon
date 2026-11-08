use std::{
    env,
    path::{Path, PathBuf},
};

use clap::ValueEnum;
use garde::Validate;
use rustc_hash::FxHashMap;
use serde::Deserialize;
use smart_default::SmartDefault;
use umbra::optional;

use crate::{
    browse::Forge,
    color::{ColorTheme, OptionalColorTheme},
    error::Result,
    graph::GraphImageWidthMode,
    keybind::KeyBind,
    search::SearchTarget,
};

const XDG_CONFIG_HOME_ENV_NAME: &str = "XDG_CONFIG_HOME";
const DEFAULT_CONFIG_DIR: &str = ".config";
const APP_DIR_NAME: &str = "spoon";
const CONFIG_FILE_NAME: &str = "config.toml";
const CONFIG_FILE_ENV_NAME: &str = "SPOON_CONFIG_FILE";

pub fn load() -> Result<(
    CoreConfig,
    UiConfig,
    GraphConfig,
    ColorTheme,
    Option<KeyBind>,
)> {
    let config = match config_file_path_from_env() {
        Some(user_path) => {
            if !user_path.exists() {
                let msg = format!(
                    "Config file specified by ${CONFIG_FILE_ENV_NAME} environment variable not found: {}",
                    user_path.display()
                );
                return Err(msg.into());
            }
            read_config_from_path(&user_path)
        }
        None => {
            if let Some(default_path) = config_file_path() {
                if default_path.exists() {
                    read_config_from_path(&default_path)
                } else {
                    Ok(Config::default())
                }
            } else {
                Ok(Config::default())
            }
        }
    }?;

    config.validate()?;

    Ok((
        config.core,
        config.ui,
        config.graph,
        config.color,
        config.keybind,
    ))
}

fn config_file_path_from_env() -> Option<PathBuf> {
    env::var(CONFIG_FILE_ENV_NAME).ok().map(PathBuf::from)
}

fn config_file_path() -> Option<PathBuf> {
    env::var(XDG_CONFIG_HOME_ENV_NAME)
        .ok()
        .map(PathBuf::from)
        .or_else(|| env::home_dir().map(|home| home.join(DEFAULT_CONFIG_DIR)))
        .map(|config_dir| config_dir.join(APP_DIR_NAME).join(CONFIG_FILE_NAME))
}

fn read_config_from_path(path: &Path) -> Result<Config> {
    let content = std::fs::read_to_string(path)?;
    let config: OptionalConfig = toml::from_str(&content)?;
    Ok(config.into())
}

#[optional(derives = [Deserialize], attrs = [serde(deny_unknown_fields)])]
#[derive(Debug, Default, Clone, PartialEq, Eq, Validate)]
struct Config {
    #[garde(dive)]
    #[nested]
    core: CoreConfig,
    #[garde(dive)]
    #[nested]
    ui: UiConfig,
    #[garde(dive)]
    #[nested]
    graph: GraphConfig,
    #[garde(skip)]
    #[nested]
    color: ColorTheme,
    // The user customed keybinds, please ref `assets/default-keybind.toml`
    #[garde(skip)]
    keybind: Option<KeyBind>,
}

#[optional(derives = [Deserialize], attrs = [serde(deny_unknown_fields)])]
#[derive(Debug, Default, Clone, PartialEq, Eq, Validate)]
pub struct CoreConfig {
    #[garde(skip)]
    #[nested]
    pub option: CoreOptionConfig,
    #[garde(skip)]
    #[nested]
    pub git: CoreGitConfig,
    #[garde(skip)]
    #[nested]
    pub search: CoreSearchConfig,
    #[garde(dive)]
    #[nested]
    pub external: CoreExternalConfig,
    #[garde(skip)]
    #[nested]
    pub browser: CoreBrowserConfig,
}

// Opening the pages of commits, branches, tags and files on the web host of a remote.
#[optional(derives = [Deserialize], attrs = [serde(deny_unknown_fields)])]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault)]
pub struct CoreBrowserConfig {
    // the kind of host of a domain the name doesn't tell: "git.company.com" = "gitlab"
    pub services: FxHashMap<String, Forge>,
}

#[optional(derives = [Deserialize], attrs = [serde(deny_unknown_fields)])]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault)]
pub struct CoreOptionConfig {
    pub protocol: Option<ImageProtocolType>,
    pub order: Option<CommitOrderType>,
    pub graph_width: Option<GraphWidthType>,
    pub graph_style: Option<GraphStyle>,
    pub graph_antialias: Option<bool>,
    pub initial_selection: Option<InitialSelection>,
}

#[optional(derives = [Deserialize], attrs = [serde(deny_unknown_fields)])]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault)]
pub struct CoreGitConfig {
    #[default = false]
    pub mailmap: bool,
    // the tool `git mergetool` runs, its own `merge.tool` config when None
    pub merge_tool: Option<String>,
}

#[optional(derives = [Deserialize], attrs = [serde(deny_unknown_fields)])]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault)]
pub struct CoreSearchConfig {
    #[default(SearchTarget::All)]
    pub target: SearchTarget,
    #[default = true]
    pub smart_case: bool,
    #[default = false]
    pub fuzzy: bool,
}

#[optional(derives = [Deserialize], attrs = [serde(deny_unknown_fields)])]
#[derive(Debug, Default, Clone, PartialEq, Eq, Validate)]
pub struct UiConfig {
    #[garde(skip)]
    #[nested]
    pub common: UiCommonConfig,
    #[garde(dive)]
    #[nested]
    pub list: UiListConfig,
    #[garde(dive)]
    #[nested]
    pub detail: UiDetailConfig,
    #[garde(dive)]
    #[nested]
    pub refs: UiRefsConfig,
}

#[optional(derives = [Deserialize], attrs = [serde(deny_unknown_fields)])]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault)]
pub struct UiCommonConfig {
    #[default(CursorType::Native)]
    pub cursor_type: CursorType,
    // the rows kept visible above and below the selected one in every list, like vim's
    #[default = 0]
    pub scrolloff: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorType {
    Native,
    Virtual(String),
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Validate)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum ClipboardConfig {
    #[default]
    Auto,
    Custom {
        #[garde(length(min = 1), inner(length(min = 1)))]
        commands: Vec<String>,
    },
}

#[optional(derives = [Deserialize], attrs = [serde(deny_unknown_fields)])]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault, Validate)]
pub struct CoreExternalConfig {
    #[garde(dive)]
    #[default(ClipboardConfig::Auto)]
    pub clipboard: ClipboardConfig,
}

#[optional(derives = [Deserialize], attrs = [serde(deny_unknown_fields)])]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault, Validate)]
pub struct UiListConfig {
    #[garde(length(min = 1))]
    #[default(vec![
        UserListColumnType::Graph,
        UserListColumnType::Marker,
        UserListColumnType::Subject,
        UserListColumnType::Name,
        UserListColumnType::Hash,
        UserListColumnType::Date,
    ])]
    pub columns: Vec<UserListColumnType>,
    #[garde(range(min = 1))]
    #[default = 20]
    pub subject_min_width: u16,
    #[garde(length(min = 1), custom(valid_date_format))]
    #[default = "%Y-%m-%d"]
    pub date_format: String,
    #[garde(skip)]
    #[default = 10]
    pub date_width: u16,
    #[garde(skip)]
    #[default = true]
    pub date_local: bool,
    #[garde(skip)]
    #[default = 20]
    pub name_width: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserListColumnType {
    Graph,
    Marker,
    Subject,
    Name,
    Hash,
    Date,
}

#[optional(derives = [Deserialize], attrs = [serde(deny_unknown_fields)])]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault, Validate)]
pub struct UiDetailConfig {
    #[garde(range(min = 1))]
    #[default = 20]
    pub height: u16,
    #[garde(length(min = 1), custom(valid_date_format))]
    #[default = "%Y-%m-%d %H:%M:%S %z"]
    pub date_format: String,
    #[garde(skip)]
    #[default = true]
    pub date_local: bool,
}

#[optional(derives = [Deserialize], attrs = [serde(deny_unknown_fields)])]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault, Validate)]
pub struct UiRefsConfig {
    #[garde(range(min = 1))]
    #[default = 26]
    pub width: u16,
    // whether the commit graph follows the sidebar, selecting the commit of the ref it shows
    #[garde(skip)]
    #[default = false]
    pub follow: bool,
}

#[optional(derives = [Deserialize], attrs = [serde(deny_unknown_fields)])]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault, Validate)]
pub struct GraphConfig {
    #[garde(skip)]
    #[default(GraphImageWidthMode::Compact)]
    pub row_image_width: GraphImageWidthMode,
    #[garde(dive)]
    #[nested]
    pub color: GraphColorConfig,
}

#[optional(derives = [Deserialize], attrs = [serde(deny_unknown_fields)])]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault, Validate)]
pub struct GraphColorConfig {
    #[garde(length(min = 1), inner(pattern(r"^#([0-9a-fA-F]{6}|[0-9a-fA-F]{8})$")))]
    #[default(vec![
        "#F38BA8".into(),
        "#A6E3A1".into(),
        "#F9E2AF".into(),
        "#89B4FA".into(),
        "#CBA6F7".into(),
        "#94E2D5".into(),
    ])]
    pub branches: Vec<String>,
    #[garde(pattern(r"^#([0-9a-fA-F]{6}|[0-9a-fA-F]{8})$"))]
    #[default = "#00000000"]
    pub edge: String,
    #[garde(pattern(r"^#([0-9a-fA-F]{6}|[0-9a-fA-F]{8})$"))]
    #[default = "#00000000"]
    pub background: String,
}

// A date format chrono can draw a date with: one it can't, like "%Q", would fail drawing it.
fn valid_date_format(format: &str, _: &()) -> garde::Result {
    use chrono::format::{Item, StrftimeItems};
    match StrftimeItems::new(format).any(|item| matches!(item, Item::Error)) {
        true => Err(garde::Error::new(format!(
            "{format:?} is no date format, see https://docs.rs/chrono/latest/chrono/format/strftime/index.html"
        ))),
        false => Ok(()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImageProtocolType {
    Auto,
    Iterm,
    Kitty,
    KittyUnicode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommitOrderType {
    Chrono,
    Topo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GraphWidthType {
    Auto,
    Double,
    Single,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GraphStyle {
    Rounded,
    Angular,
    Curved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InitialSelection {
    Latest,
    Head,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browse::Links;

    #[test]
    fn test_config_default() {
        let actual = Config::default();
        let expected = Config {
            core: CoreConfig {
                option: CoreOptionConfig {
                    protocol: None,
                    order: None,
                    graph_width: None,
                    graph_style: None,
                    graph_antialias: None,
                    initial_selection: None,
                },
                git: CoreGitConfig {
                    mailmap: false,
                    merge_tool: None,
                },
                search: CoreSearchConfig {
                    target: SearchTarget::All,
                    smart_case: true,
                    fuzzy: false,
                },
                external: CoreExternalConfig {
                    clipboard: ClipboardConfig::Auto,
                },
                browser: CoreBrowserConfig::default(),
            },
            ui: UiConfig {
                common: UiCommonConfig {
                    cursor_type: CursorType::Native,
                    scrolloff: 0,
                },
                list: UiListConfig {
                    columns: vec![
                        UserListColumnType::Graph,
                        UserListColumnType::Marker,
                        UserListColumnType::Subject,
                        UserListColumnType::Name,
                        UserListColumnType::Hash,
                        UserListColumnType::Date,
                    ],
                    subject_min_width: 20,
                    date_format: "%Y-%m-%d".into(),
                    date_width: 10,
                    date_local: true,
                    name_width: 20,
                },
                detail: UiDetailConfig {
                    height: 20,
                    date_format: "%Y-%m-%d %H:%M:%S %z".into(),
                    date_local: true,
                },
                refs: UiRefsConfig {
                    width: 26,
                    follow: false,
                },
            },
            graph: GraphConfig {
                row_image_width: GraphImageWidthMode::Compact,
                color: GraphColorConfig {
                    branches: vec![
                        "#F38BA8".into(),
                        "#A6E3A1".into(),
                        "#F9E2AF".into(),
                        "#89B4FA".into(),
                        "#CBA6F7".into(),
                        "#94E2D5".into(),
                    ],
                    edge: "#00000000".into(),
                    background: "#00000000".into(),
                },
            },
            color: ColorTheme::default(),
            keybind: None,
        };
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_config_complete_toml() {
        let toml = r##"
            [core.option]
            protocol = "kitty-unicode"
            order = "topo"
            graph_width = "single"
            graph_style = "angular"
            graph_antialias = true
            initial_selection = "head"
            [core.git]
            mailmap = true
            [core.search]
            target = "author"
            smart_case = true
            fuzzy = true
            [core.browser.services]
            "git.company.com" = "gitlab"
            "code.example.org" = { commit = "{base}/c/{hash}" }
            [ui.common]
            cursor_type = { virtual = "|" }
            scrolloff = 3
            [ui.list]
            columns = ["date", "subject", "hash", "graph"]
            subject_min_width = 40
            date_format = "%Y/%m/%d"
            date_width = 20
            date_local = false
            name_width = 30
            [ui.detail]
            height = 30
            date_format = "%Y/%m/%d %H:%M:%S"
            date_local = false
            [ui.refs]
            width = 40
            follow = true
            [graph]
            row_image_width = "fixed"
            [graph.color]
            branches = ["#ff0000", "#00ff00", "#0000ff"]
            edge = "#000000"
            background = "#ffffff"
        "##;
        let actual: Config = toml::from_str::<OptionalConfig>(toml).unwrap().into();
        let expected = Config {
            core: CoreConfig {
                option: CoreOptionConfig {
                    protocol: Some(ImageProtocolType::KittyUnicode),
                    order: Some(CommitOrderType::Topo),
                    graph_width: Some(GraphWidthType::Single),
                    graph_style: Some(GraphStyle::Angular),
                    graph_antialias: Some(true),
                    initial_selection: Some(InitialSelection::Head),
                },
                git: CoreGitConfig {
                    mailmap: true,
                    merge_tool: None,
                },
                search: CoreSearchConfig {
                    target: SearchTarget::Author,
                    smart_case: true,
                    fuzzy: true,
                },
                external: CoreExternalConfig {
                    clipboard: ClipboardConfig::Auto,
                },
                browser: CoreBrowserConfig {
                    services: FxHashMap::from_iter([
                        ("git.company.com".into(), Forge::Kind("gitlab".into())),
                        (
                            "code.example.org".into(),
                            Forge::Links(Links {
                                commit: Some("{base}/c/{hash}".into()),
                                branch: None,
                                pull_request: None,
                                tag: None,
                                file: None,
                                line: None,
                                lines: None,
                            }),
                        ),
                    ]),
                },
            },
            ui: UiConfig {
                common: UiCommonConfig {
                    cursor_type: CursorType::Virtual("|".into()),
                    scrolloff: 3,
                },
                list: UiListConfig {
                    columns: vec![
                        UserListColumnType::Date,
                        UserListColumnType::Subject,
                        UserListColumnType::Hash,
                        UserListColumnType::Graph,
                    ],
                    subject_min_width: 40,
                    date_format: "%Y/%m/%d".into(),
                    date_width: 20,
                    date_local: false,
                    name_width: 30,
                },
                detail: UiDetailConfig {
                    height: 30,
                    date_format: "%Y/%m/%d %H:%M:%S".into(),
                    date_local: false,
                },
                refs: UiRefsConfig {
                    width: 40,
                    follow: true,
                },
            },
            graph: GraphConfig {
                row_image_width: GraphImageWidthMode::Fixed,
                color: GraphColorConfig {
                    branches: vec!["#ff0000".into(), "#00ff00".into(), "#0000ff".into()],
                    edge: "#000000".into(),
                    background: "#ffffff".into(),
                },
            },
            color: ColorTheme::default(),
            keybind: None,
        };
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_config_partial_toml() {
        let toml = r#"
            [ui.common]
            cursor_type = "native"
            [ui.list]
            date_format = "%Y/%m/%d"
        "#;
        let actual: Config = toml::from_str::<OptionalConfig>(toml).unwrap().into();
        let expected = Config {
            core: CoreConfig {
                option: CoreOptionConfig {
                    protocol: None,
                    order: None,
                    graph_width: None,
                    graph_style: None,
                    graph_antialias: None,
                    initial_selection: None,
                },
                git: CoreGitConfig {
                    mailmap: false,
                    merge_tool: None,
                },
                search: CoreSearchConfig {
                    target: SearchTarget::All,
                    smart_case: true,
                    fuzzy: false,
                },
                external: CoreExternalConfig {
                    clipboard: ClipboardConfig::Auto,
                },
                browser: CoreBrowserConfig::default(),
            },
            ui: UiConfig {
                common: UiCommonConfig {
                    cursor_type: CursorType::Native,
                    scrolloff: 0,
                },
                list: UiListConfig {
                    columns: vec![
                        UserListColumnType::Graph,
                        UserListColumnType::Marker,
                        UserListColumnType::Subject,
                        UserListColumnType::Name,
                        UserListColumnType::Hash,
                        UserListColumnType::Date,
                    ],
                    subject_min_width: 20,
                    date_format: "%Y/%m/%d".into(),
                    date_width: 10,
                    date_local: true,
                    name_width: 20,
                },
                detail: UiDetailConfig {
                    height: 20,
                    date_format: "%Y-%m-%d %H:%M:%S %z".into(),
                    date_local: true,
                },
                refs: UiRefsConfig {
                    width: 26,
                    follow: false,
                },
            },
            graph: GraphConfig {
                row_image_width: GraphImageWidthMode::Compact,
                color: GraphColorConfig {
                    branches: vec![
                        "#F38BA8".into(),
                        "#A6E3A1".into(),
                        "#F9E2AF".into(),
                        "#89B4FA".into(),
                        "#CBA6F7".into(),
                        "#94E2D5".into(),
                    ],
                    edge: "#00000000".into(),
                    background: "#00000000".into(),
                },
            },
            color: ColorTheme::default(),
            keybind: None,
        };
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_config_clipboard_auto() {
        let toml = r#"
            [core.external]
            clipboard = "auto"
        "#;
        let config: Config = toml::from_str::<OptionalConfig>(toml).unwrap().into();
        assert_eq!(config.core.external.clipboard, ClipboardConfig::Auto);
    }

    #[test]
    fn test_config_clipboard_custom_single_command() {
        let toml = r#"
            [core.external]
            clipboard = { custom = { commands = ["wl-copy"] } }
        "#;
        let config: Config = toml::from_str::<OptionalConfig>(toml).unwrap().into();
        assert_eq!(
            config.core.external.clipboard,
            ClipboardConfig::Custom {
                commands: vec!["wl-copy".into()]
            }
        );
    }

    #[test]
    fn test_config_clipboard_custom_command_with_args() {
        let toml = r#"
            [core.external]
            clipboard = { custom = { commands = ["xclip", "-selection", "clipboard"] } }
        "#;
        let config: Config = toml::from_str::<OptionalConfig>(toml).unwrap().into();
        assert_eq!(
            config.core.external.clipboard,
            ClipboardConfig::Custom {
                commands: vec!["xclip".into(), "-selection".into(), "clipboard".into()]
            }
        );
    }

    #[test]
    fn test_config_unknown_keys() {
        // refused, naming the key
        let unknown = |toml: &str, key: &str| {
            let err = toml::from_str::<OptionalConfig>(toml)
                .err()
                .map(|e| e.to_string());
            assert!(
                err.as_deref().is_some_and(|e| e.contains(key)),
                "{toml}: {err:?}"
            );
        };
        // a misspelled key, table or color, and one of serie's removed since
        unknown("[ui.list]\ncolums = [\"graph\"]\n", "colums");
        unknown("[ui.lsit]\nheight = 3\n", "lsit");
        unknown("[color]\nlist_head_fg = \"red\"\n", "list_head_fg");
        unknown(
            "[core.external.clipboard.custom]\ncomands = [\"pbcopy\"]\n",
            "comands",
        );
        let services = "[core.browser.services.\"git.company.com\"]\ncomit = \"{base}/c/{hash}\"\n";
        unknown(services, "git.company.com");
    }

    // The defaults config.schema.json gives, written to a config, load as the code's defaults:
    // the schema is written by hand, and its default of a color had drifted.
    #[test]
    fn test_schema_defaults_are_the_code_defaults() {
        fn defaults(schema: &serde_json::Value) -> Option<toml::Value> {
            if let Some(properties) = schema["properties"].as_object() {
                let table: toml::Table = properties
                    .iter()
                    .filter_map(|(name, property)| Some((name.clone(), defaults(property)?)))
                    .collect();
                return (!table.is_empty()).then_some(toml::Value::Table(table));
            }
            let default = schema.get("default")?;
            Some(serde_json::from_value(default.clone()).unwrap())
        }
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../config.schema.json")).unwrap();
        let mut table = toml::Table::new();
        for section in ["core", "ui", "graph", "color"] {
            let section_defaults = defaults(&schema["properties"][section]).unwrap();
            table.insert(section.into(), section_defaults);
        }
        let toml = toml::to_string(&table).unwrap();
        let mut config: Config = toml::from_str::<OptionalConfig>(&toml)
            .unwrap_or_else(|e| panic!("{e}\n{toml}"))
            .into();
        // unset in the code, they fall back with the command line's options, see `main`
        // shortcut: the defaults of `core.option` aren't compared; resolve them here to check them.
        config.core.option = CoreOptionConfig::default();
        assert_eq!(config, Config::default(), "{toml}");
    }

    // Each value that crashes later, or shows nothing, is refused when loaded, naming it.
    #[test]
    fn test_config_invalid_values() {
        let refused = [
            ("[ui.list]\ncolumns = []\n", "ui.list.columns"),
            (
                "[ui.list]\nsubject_min_width = 0\n",
                "ui.list.subject_min_width",
            ),
            ("[ui.detail]\nheight = 0\n", "ui.detail.height"),
            ("[ui.refs]\nwidth = 0\n", "ui.refs.width"),
            ("[graph.color]\nbranches = []\n", "graph.color.branches"),
            (
                "[graph.color]\nbranches = [\"#aé000\"]\n",
                "graph.color.branches",
            ),
            ("[graph.color]\nedge = \"red\"\n", "graph.color.edge"),
            (
                "[graph.color]\nbackground = \"#12345\"\n",
                "graph.color.background",
            ),
            (
                "[core.external]\nclipboard = { custom = { commands = [] } }\n",
                "core.external.clipboard",
            ),
            (
                "[core.external]\nclipboard = { custom = { commands = [\"\"] } }\n",
                "core.external.clipboard",
            ),
        ];
        for (toml, field) in refused {
            let config: Config = toml::from_str::<OptionalConfig>(toml).unwrap().into();
            let err = config.validate().unwrap_err().to_string();
            assert!(err.contains(field), "{toml}: {err}");
        }
    }

    #[test]
    fn test_config_invalid_date_format() {
        // refused when loaded, naming it, else drawing a date would panic
        for table in ["list", "detail"] {
            let toml = format!("[ui.{table}]\ndate_format = \"%Y %Q\"\n");
            let config: Config = toml::from_str::<OptionalConfig>(&toml).unwrap().into();
            let err = config.validate().unwrap_err().to_string();
            assert!(err.contains(&format!("ui.{table}.date_format")), "{err}");
        }
        let config: Config =
            toml::from_str::<OptionalConfig>("[ui.list]\ndate_format = \"%Y-%m %H:%M\"\n")
                .unwrap()
                .into();
        assert!(config.validate().is_ok());
    }
}
