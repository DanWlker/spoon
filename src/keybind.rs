use crokey::{KeyCombinationFormat, OneToThree};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rustc_hash::FxHashMap;
use serde::{de::Deserializer, Deserialize};
use std::sync::LazyLock;

use crate::event::UserEvent;

const DEFAULT_KEY_BIND: &str = include_str!("../assets/default-keybind.toml");

// Where keys are bound: the parts of the app that can have the focus, nested. A key is looked up
// from the focused scope outward, the first scope binding it wins, so a scope overrides the keys of
// those around it; and each scope takes from around it only the events it lets through (see
// `Scope::passes`), like the diff beside the files, which takes the keys of the whole app only.
//
// global
// ├─ graph                  the commands of the commit graph, bound here
// │  ├─ sidebar             the refs; the commands of the graph work on the selected ref
// │  ├─ commit_list
// │  └─ details             not the commands of the graph
// │     └─ details_pane     the diff or preview beside the files
// ├─ local_changes
// │  └─ local_changes_diff
// └─ dialog                 the popups of the git commands
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    Global,
    Graph,
    Sidebar,
    CommitList,
    Details,
    DetailsPane,
    LocalChanges,
    LocalChangesDiff,
    Dialog,
}

impl Scope {
    const ALL: [Scope; 9] = [
        Scope::Global,
        Scope::Graph,
        Scope::Sidebar,
        Scope::CommitList,
        Scope::Details,
        Scope::DetailsPane,
        Scope::LocalChanges,
        Scope::LocalChangesDiff,
        Scope::Dialog,
    ];

    pub fn parent(self) -> Option<Scope> {
        match self {
            Scope::Global => None,
            Scope::Graph | Scope::LocalChanges | Scope::Dialog => Some(Scope::Global),
            Scope::Sidebar | Scope::CommitList | Scope::Details => Some(Scope::Graph),
            Scope::DetailsPane => Some(Scope::Details),
            Scope::LocalChangesDiff => Some(Scope::LocalChanges),
        }
    }

    // Whether the event of a key bound around this scope works in it.
    fn passes(self, event: UserEvent) -> bool {
        match self {
            // the commands of the graph act on the commit list, not on the details
            Scope::Details => event.home_scope() != Scope::Graph,
            // the diff or preview takes its own keys, moving in it, and those of the whole app
            Scope::DetailsPane => {
                event.is_app_wide()
                    || matches!(
                        event,
                        UserEvent::NavigateUp
                            | UserEvent::NavigateDown
                            | UserEvent::GoToTop
                            | UserEvent::GoToBottom
                            | UserEvent::ScrollUp
                            | UserEvent::ScrollDown
                            | UserEvent::PageUp
                            | UserEvent::PageDown
                            | UserEvent::HalfPageUp
                            | UserEvent::HalfPageDown
                            | UserEvent::Confirm
                            | UserEvent::Cancel
                            | UserEvent::Close
                            | UserEvent::Search
                            | UserEvent::GoToNext
                            | UserEvent::GoToPrevious
                            | UserEvent::ShortCopy
                            | UserEvent::NextTab
                            | UserEvent::PreviousTab
                            | UserEvent::ApplyChange
                            | UserEvent::OpenInBrowser
                            | UserEvent::CopyLink
                    )
            }
            // not the keys of the files beside it
            Scope::LocalChangesDiff => !matches!(
                event,
                UserEvent::ToggleSelect
                    | UserEvent::NextPicked
                    | UserEvent::PreviousPicked
                    | UserEvent::Filter
                    | UserEvent::FullCopy
                    | UserEvent::NavigateLeft
                    | UserEvent::NavigateRight
            ),
            _ => true,
        }
    }

    // The scope and those around it, the innermost first.
    pub fn chain(self) -> impl Iterator<Item = Scope> {
        std::iter::successors(Some(self), |scope| scope.parent())
    }

    // Whether `self` is `scope` or inside it.
    fn within(self, scope: Scope) -> bool {
        self.chain().any(|s| s == scope)
    }

    pub fn name(self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Graph => "graph",
            Scope::Sidebar => "sidebar",
            Scope::CommitList => "commit_list",
            Scope::Details => "details",
            Scope::DetailsPane => "details_pane",
            Scope::LocalChanges => "local_changes",
            Scope::LocalChangesDiff => "local_changes_diff",
            Scope::Dialog => "dialog",
        }
    }

    fn from_name(name: &str) -> Option<Scope> {
        Scope::ALL.into_iter().find(|scope| scope.name() == name)
    }
}

// The keys of each scope: single keys, and sequences of keys pressed one after another (written
// separated by spaces, like "] m").
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct KeyBind {
    bindings: FxHashMap<(Scope, Vec<KeyEvent>), UserEvent>,
}

// How the keys pressed so far match the bindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceMatch {
    // nothing is bound to them
    None,
    // they start a sequence, waiting for the next key
    Partial,
    Complete(UserEvent),
}

impl KeyBind {
    pub fn new(custom_keybind_patch: Option<KeyBind>) -> Self {
        let mut keybind: KeyBind =
            toml::from_str(DEFAULT_KEY_BIND).expect("default key bind should be correct");

        if let Some(custom_keybind_patch) = custom_keybind_patch {
            keybind.merge(custom_keybind_patch);
        }

        keybind
    }

    // Overrides with the keys and sequences of `patch`: a key bound in a scope replaces what that
    // key did in it and in the scopes inside it, sequences starting with it included; a sequence
    // replaces the key it starts with in its scope.
    fn merge(&mut self, patch: KeyBind) {
        for (scope, keys) in patch.bindings.keys() {
            match keys.as_slice() {
                [key] => self
                    .bindings
                    .retain(|(s, bound), _| !(s.within(*scope) && bound[0] == *key)),
                sequence => {
                    self.bindings.remove(&(*scope, vec![sequence[0]]));
                }
            }
        }
        self.bindings.extend(patch.bindings);
    }

    // What the keys pressed so far do in `scope`: from it outward, the first scope binding them (or
    // a sequence starting with them) to an event that works in `scope`.
    pub fn resolve(&self, scope: Scope, pressed: &[KeyEvent]) -> SequenceMatch {
        // the events bound around a scope work in it when every scope on the way lets them through
        let mut passes: Vec<Scope> = vec![];
        let allowed = |event: &UserEvent, passes: &[Scope]| passes.iter().all(|s| s.passes(*event));
        for s in scope.chain() {
            if let Some(event) = self
                .bindings
                .get(&(s, pressed.to_vec()))
                .filter(|e| allowed(e, &passes))
            {
                return SequenceMatch::Complete(*event);
            }
            if self.bindings.iter().any(|((bound_scope, keys), event)| {
                *bound_scope == s
                    && keys.len() > pressed.len()
                    && keys.starts_with(pressed)
                    && allowed(event, &passes)
            }) {
                return SequenceMatch::Partial;
            }
            passes.push(s);
        }
        SequenceMatch::None
    }

    // The event of a single key in `scope`, see `resolve`.
    pub fn event_in(&self, scope: Scope, key: &KeyEvent) -> Option<UserEvent> {
        match self.resolve(scope, std::slice::from_ref(key)) {
            SequenceMatch::Complete(event) => Some(event),
            _ => None,
        }
    }

    // The keys bound to the event, in any scope: single keys first.
    pub fn keys_for_event(&self, user_event: UserEvent) -> Vec<String> {
        let mut keys: Vec<&Vec<KeyEvent>> = self
            .bindings
            .iter()
            .filter(|(_, ue)| **ue == user_event)
            .map(|((_, keys), _)| keys)
            .collect();
        // KeyEvent orders partially, but key bindings compare
        keys.sort_by(|a, b| a.len().cmp(&b.len()).then(a.partial_cmp(b).unwrap()));
        keys.dedup();
        keys.into_iter().map(|keys| keys_to_string(keys)).collect()
    }

    // The keys of the events for a hint, like "j/k" or "space/e": the shortest of each, with names
    // like Enter in lower case (but letters as they are, "A" is shift-a); empty when none is bound.
    pub fn short_keys(&self, events: &[UserEvent]) -> String {
        let keys: Vec<String> = events
            .iter()
            .filter_map(|event| {
                let keys = self.keys_for_event(*event);
                keys.into_iter().min_by_key(|key| key.chars().count())
            })
            .map(key_name)
            .collect();
        keys.join("/")
    }

    // All the keys of the events, named like `short_keys`, for the help: "k/up".
    pub fn all_keys(&self, events: &[UserEvent]) -> String {
        let keys: Vec<String> = events
            .iter()
            .flat_map(|event| self.keys_for_event(*event))
            .map(key_name)
            .collect();
        keys.join("/")
    }

    pub fn user_command_event_numbers(&self) -> Vec<usize> {
        let mut numbers: Vec<usize> = self
            .bindings
            .values()
            .filter_map(|ue| {
                if let UserEvent::UserCommand(n) = ue {
                    Some(*n)
                } else {
                    None
                }
            })
            .collect();
        numbers.sort_unstable();
        numbers.dedup();
        numbers
    }

    // Binds the keys of `key_events` (sequences when separated by spaces) to the event in `scope`.
    fn bind(
        &mut self,
        scope: Scope,
        user_event: UserEvent,
        key_events: &[String],
    ) -> Result<(), String> {
        for key_event_str in key_events {
            let parsed: Result<Vec<KeyEvent>, String> = key_event_str
                .split_whitespace()
                .map(parse_key_event)
                .collect();
            let keys = match parsed {
                Ok(keys) if !keys.is_empty() => keys,
                Ok(_) => return Err(format!("{key_event_str:?} is not a valid key event: empty")),
                Err(s) => return Err(format!("{key_event_str:?} is not a valid key event: {s:}")),
            };
            if let Some(conflict_user_event) = self.bindings.insert((scope, keys), user_event) {
                return Err(format!(
                    "{key_event_str:?} map to multiple events in {}: {user_event:?}, {conflict_user_event:?}",
                    scope.name()
                ));
            }
        }
        Ok(())
    }
}

// The keybind table: `event = [keys]`, bound in the event's own scope (see
// `UserEvent::home_scope`), and tables of scopes, `[keybind.details_pane]`, binding in them.
#[derive(Deserialize)]
#[serde(untagged)]
enum KeyBindEntry {
    Keys(Vec<String>),
    Scope(FxHashMap<String, Vec<String>>),
}

fn parse_user_event(name: &str) -> Result<UserEvent, String> {
    use serde::de::{value::StrDeserializer, IntoDeserializer};
    let deserializer: StrDeserializer<serde::de::value::Error> = name.into_deserializer();
    UserEvent::deserialize(deserializer).map_err(|e| e.to_string())
}

impl<'de> Deserialize<'de> for KeyBind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let parsed_map = FxHashMap::<String, KeyBindEntry>::deserialize(deserializer)?;
        let mut keybind = KeyBind::default();
        let error = serde::de::Error::custom;
        for (name, entry) in parsed_map {
            let bindings = match entry {
                KeyBindEntry::Keys(keys) => {
                    let user_event = parse_user_event(&name).map_err(error)?;
                    vec![(user_event.home_scope(), user_event, keys)]
                }
                KeyBindEntry::Scope(table) => {
                    let scope = Scope::from_name(&name)
                        .ok_or_else(|| error(format!("Unknown keybind scope: {name}")))?;
                    table
                        .into_iter()
                        .map(|(event_name, keys)| {
                            parse_user_event(&event_name).map(|event| (scope, event, keys))
                        })
                        .collect::<Result<Vec<_>, String>>()
                        .map_err(error)?
                }
            };
            for (scope, user_event, keys) in bindings {
                // events that no longer exist are skipped
                if user_event != UserEvent::Unknown {
                    keybind.bind(scope, user_event, &keys).map_err(error)?;
                }
            }
        }

        // a key starting a sequence cannot do something on its own in the same scope, the sequence
        // could not be typed; a scope inside it can bind the key, the sequence is typed elsewhere
        let sequences = keybind
            .bindings
            .iter()
            .filter(|((_, keys), _)| keys.len() > 1);
        for ((scope, keys), user_event) in sequences {
            if let Some(key_user_event) = keybind.bindings.get(&(*scope, vec![keys[0]])) {
                let msg = format!(
                    "{} starts the key sequence of {user_event:?}, so it cannot map to {key_user_event:?}",
                    key_event_to_string(keys[0])
                );
                return Err(error(msg));
            }
        }

        Ok(keybind)
    }
}

// A key like "ctrl-x" or "H" (shift-h), see `crokey::parse`. Keys pressed together are not
// supported, only one after another, see `parse_sequence`.
fn parse_key_event(raw: &str) -> Result<KeyEvent, String> {
    let key = crokey::parse(&raw.replace(' ', ""))
        .map_err(|e| e.to_string())?
        .normalized();
    match key.codes {
        OneToThree::One(code) => Ok(KeyEvent::new(code, key.modifiers)),
        _ => Err(format!(
            "{raw:?} presses several keys at once, which isn't supported"
        )),
    }
}

// A sequence is displayed with its keys one after another, like "]m".
pub fn keys_to_string(keys: &[KeyEvent]) -> String {
    keys.iter().map(|key| key_event_to_string(*key)).collect()
}

fn key_event_to_string(key_event: KeyEvent) -> String {
    // a capital letter alone is shift and the letter, like "H"
    if let KeyCode::Char(c) = key_event.code {
        if key_event.modifiers == KeyModifiers::SHIFT {
            return c.to_ascii_uppercase().into();
        }
    }
    KEY_FORMAT.to_string(key_event)
}

// "Ctrl-Alt-Shift-x", "Enter", "PageUp", "F12"
static KEY_FORMAT: LazyLock<KeyCombinationFormat> = LazyLock::new(KeyCombinationFormat::default);

// A key as hints show it: names like Enter in lower case, letters as they are ("A" is shift-a).
fn key_name(key: String) -> String {
    if key.chars().count() > 1 {
        key.to_lowercase()
    } else {
        key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rustfmt::skip]
    #[test]
    fn test_deserialize_keybind() {
        let toml = r#"
            navigate_up = ["k"]
            navigate_down = ["j", "down"]
            navigate_left = ["ctrl-h", "shift-h", "alt-h"]
            navigate_right = ["ctrl-shift-l", "alt-shift-ctrl-l"]
            quit = ["esc", "f12"]
            user_command_1 = ["d"]
            user_command_view_toggle_10 = ["e"]
        "#;

        let expected = KeyBind {
            bindings: [
                (
                    (Scope::Global, KeyEvent::new(KeyCode::Char('k'), KeyModifiers::empty())),
                    UserEvent::NavigateUp,
                ),
                (
                    (Scope::Global, KeyEvent::new(KeyCode::Char('j'), KeyModifiers::empty())),
                    UserEvent::NavigateDown,
                ),
                (
                    (Scope::Global, KeyEvent::new(KeyCode::Down, KeyModifiers::empty())),
                    UserEvent::NavigateDown,
                ),
                (
                    (Scope::Global, KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL)),
                    UserEvent::NavigateLeft,
                ),
                (
                    (Scope::Global, KeyEvent::new(KeyCode::Char('h'), KeyModifiers::SHIFT)),
                    UserEvent::NavigateLeft,
                ),
                (
                    (Scope::Global, KeyEvent::new(KeyCode::Char('h'), KeyModifiers::ALT)),
                    UserEvent::NavigateLeft,
                ),
                (
                    (Scope::Global, KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL | KeyModifiers::SHIFT)),
                    UserEvent::NavigateRight,
                ),
                (
                    (Scope::Global, KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL | KeyModifiers::SHIFT | KeyModifiers::ALT)),
                    UserEvent::NavigateRight,
                ),
                (
                    (Scope::Global, KeyEvent::new(KeyCode::Esc, KeyModifiers::empty())),
                    UserEvent::Quit,
                ),
                (
                    (Scope::Global, KeyEvent::new(KeyCode::F(12), KeyModifiers::empty())),
                    UserEvent::Quit,
                ),
                (
                    (Scope::Global, KeyEvent::new(KeyCode::Char('d'), KeyModifiers::empty())),
                    UserEvent::UserCommand(1),
                ),
                (
                    (Scope::Global, KeyEvent::new(KeyCode::Char('e'), KeyModifiers::empty())),
                    UserEvent::UserCommand(10),
                ),
            ]
            .into_iter()
            .map(|((scope, key), event)| ((scope, vec![key]), event))
            .collect(),
        };

        let actual: KeyBind = toml::from_str(toml).unwrap();

        assert_eq!(actual, expected);
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty())
    }

    #[test]
    fn test_key_sequences() {
        let keybind: KeyBind = toml::from_str(
            r#"
            go_to_next = ["] m", "g n"]
            quit = ["q"]
        "#,
        )
        .unwrap();
        let global = |keys: &[KeyEvent]| keybind.resolve(Scope::Global, keys);
        assert_eq!(global(&[key(']')]), SequenceMatch::Partial);
        assert_eq!(
            global(&[key(']'), key('m')]),
            SequenceMatch::Complete(UserEvent::GoToNext)
        );
        assert_eq!(global(&[key(']'), key('x')]), SequenceMatch::None);
        assert_eq!(
            global(&[key('q')]),
            SequenceMatch::Complete(UserEvent::Quit)
        );
        assert_eq!(
            keybind.keys_for_event(UserEvent::GoToNext),
            vec!["]m", "gn"]
        );

        // a key cannot both start a sequence and do something on its own
        let err = toml::from_str::<KeyBind>(
            r#"
            go_to_next = ["] m"]
            quit = ["]"]
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("starts the key sequence"));

        // a custom key of its own replaces the default sequences starting with it, and the other way around
        let custom: KeyBind = toml::from_str(
            r#"quit = ["]"]
            go_to_top = ["g g"]"#,
        )
        .unwrap();
        let mut defaults: KeyBind = toml::from_str(
            r#"go_to_next = ["] m"]
            go_to_top = ["g"]"#,
        )
        .unwrap();
        defaults.merge(custom);
        assert_eq!(
            defaults.resolve(Scope::Global, &[key(']')]),
            SequenceMatch::Complete(UserEvent::Quit)
        );
        assert_eq!(defaults.event_in(Scope::Global, &key('g')), None);
        assert_eq!(
            defaults.resolve(Scope::Global, &[key('g'), key('g')]),
            SequenceMatch::Complete(UserEvent::GoToTop)
        );
    }

    #[test]
    fn test_scopes() {
        let keybind: KeyBind = toml::from_str(
            r#"
            discard = ["d"]
            drop_commit = ["d"]
            delete_ref = ["d"]
            quit = ["q"]
            toggle_select = ["v"]
            short_copy = ["y"]
            apply_change = ["space"]
            previous_tab = ["["]
            previous_picked = ["[ v"]
        "#,
        )
        .unwrap();
        let on = |scope, c| keybind.event_in(scope, &key(c));
        // the same key does something else in each scope, and nothing elsewhere
        assert_eq!(on(Scope::LocalChanges, 'd'), Some(UserEvent::Discard));
        assert_eq!(on(Scope::CommitList, 'd'), Some(UserEvent::DropCommit));
        assert_eq!(on(Scope::Global, 'd'), None);
        // the sidebar overrides the graph's key, and takes its other keys
        assert_eq!(on(Scope::Sidebar, 'd'), Some(UserEvent::DeleteRef));
        // the keys of the whole app work everywhere
        assert_eq!(on(Scope::DetailsPane, 'q'), Some(UserEvent::Quit));
        // the details don't take the commands of the graph, the pane only its own keys
        assert_eq!(on(Scope::Details, 'd'), None);
        assert_eq!(on(Scope::Details, 'v'), Some(UserEvent::ToggleSelect));
        assert_eq!(on(Scope::DetailsPane, 'v'), None);
        assert_eq!(on(Scope::DetailsPane, 'y'), Some(UserEvent::ShortCopy));
        let space = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::empty());
        assert_eq!(
            keybind.event_in(Scope::DetailsPane, &space),
            Some(UserEvent::ApplyChange)
        );
        assert_eq!(on(Scope::LocalChangesDiff, 'v'), None);
        assert_eq!(on(Scope::LocalChangesDiff, 'd'), Some(UserEvent::Discard));

        // a key of an inner scope comes before a sequence starting with it around it
        assert_eq!(on(Scope::Details, '['), Some(UserEvent::PreviousTab));
        assert_eq!(
            keybind.resolve(Scope::CommitList, &[key('[')]),
            SequenceMatch::Partial
        );
        assert_eq!(
            keybind.resolve(Scope::CommitList, &[key('['), key('v')]),
            SequenceMatch::Complete(UserEvent::PreviousPicked)
        );

        // a key bound around a scope takes over the keys inside it
        let mut merged = keybind.clone();
        merged.merge(toml::from_str(r#"quit = ["d"]"#).unwrap());
        assert_eq!(
            merged.event_in(Scope::CommitList, &key('d')),
            Some(UserEvent::Quit)
        );
        assert_eq!(
            merged.event_in(Scope::Sidebar, &key('d')),
            Some(UserEvent::Quit)
        );

        // a table binds in its scope only
        let mut merged = keybind.clone();
        merged.merge(
            toml::from_str(
                r#"[details_pane]
                quit = ["y"]"#,
            )
            .unwrap(),
        );
        assert_eq!(
            merged.event_in(Scope::DetailsPane, &key('y')),
            Some(UserEvent::Quit)
        );
        assert_eq!(
            merged.event_in(Scope::Details, &key('y')),
            Some(UserEvent::ShortCopy)
        );
        assert!(toml::from_str::<KeyBind>("[nowhere]\nquit = [\"q\"]").is_err());

        // a scope cannot map a key twice
        assert!(toml::from_str::<KeyBind>(
            r#"diff = ["d"]
reset = ["d"]"#
        )
        .is_err());
    }

    #[rustfmt::skip]
    #[test]
    fn test_parse_key_event() {
        let key = |code, modifiers| Ok(KeyEvent::new(code, modifiers));
        assert_eq!(parse_key_event("ctrl-x"), key(KeyCode::Char('x'), KeyModifiers::CONTROL));
        assert_eq!(parse_key_event("shift-h"), key(KeyCode::Char('H'), KeyModifiers::SHIFT));
        assert_eq!(parse_key_event("space"), key(KeyCode::Char(' '), KeyModifiers::empty()));
        // keys pressed together aren't supported, sequences are: "] v"
        assert!(parse_key_event("a-b").unwrap_err().contains("several keys"));
        assert!(parse_key_event("nope").is_err());
    }

    #[test]
    fn test_key_event_to_string() {
        let key_event = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::empty());
        assert_eq!(key_event_to_string(key_event), "k");

        let key_event = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::empty());
        assert_eq!(key_event_to_string(key_event), "j");

        let key_event = KeyEvent::new(KeyCode::Down, KeyModifiers::empty());
        assert_eq!(key_event_to_string(key_event), "Down");

        let key_event = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL);
        assert_eq!(key_event_to_string(key_event), "Ctrl-h");

        let key_event = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::SHIFT);
        assert_eq!(key_event_to_string(key_event), "H");

        let key_event = KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT);
        assert_eq!(key_event_to_string(key_event), "H");

        let key_event = KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT);
        assert_eq!(key_event_to_string(key_event), "Shift-Left");

        let key_event = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::ALT);
        assert_eq!(key_event_to_string(key_event), "Alt-h");

        let key_event = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        assert_eq!(key_event_to_string(key_event), "Ctrl-Shift-l");

        let key_event = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL | KeyModifiers::SHIFT | KeyModifiers::ALT);
        assert_eq!(key_event_to_string(key_event), "Ctrl-Alt-Shift-l");

        let key_event = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
        assert_eq!(key_event_to_string(key_event), "Esc");

        let key_event = KeyEvent::new(KeyCode::F(12), KeyModifiers::empty());
        assert_eq!(key_event_to_string(key_event), "F12");
    }
}
