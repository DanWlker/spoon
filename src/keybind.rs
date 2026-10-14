use std::ops::{Deref, DerefMut};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rustc_hash::FxHashMap;
use serde::{de::Deserializer, Deserialize};

use crate::event::UserEvent;

const DEFAULT_KEY_BIND: &str = include_str!("../assets/default-keybind.toml");

// Single keys, and sequences of keys pressed one after another (written separated by spaces, like "] m").
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct KeyBind {
    keys: FxHashMap<KeyEvent, UserEvent>,
    sequences: FxHashMap<Vec<KeyEvent>, UserEvent>,
}

impl Deref for KeyBind {
    type Target = FxHashMap<KeyEvent, UserEvent>;

    fn deref(&self) -> &Self::Target {
        &self.keys
    }
}

impl DerefMut for KeyBind {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.keys
    }
}

// How the keys pressed so far match the key sequences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceMatch {
    // no sequence starts with them
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

    // Overrides the keys and sequences of `patch`; a key of its own takes over the sequences
    // starting with it, and a sequence takes over the key it starts with.
    fn merge(&mut self, patch: KeyBind) {
        for (key_event, user_event) in patch.keys {
            self.sequences.retain(|keys, _| keys[0] != key_event);
            self.keys.insert(key_event, user_event);
        }
        for (key_events, user_event) in patch.sequences {
            self.keys.remove(&key_events[0]);
            self.sequences.insert(key_events, user_event);
        }
    }

    // Matches the keys pressed so far against the key sequences.
    pub fn match_sequence(&self, pressed: &[KeyEvent]) -> SequenceMatch {
        if let Some(user_event) = self.sequences.get(pressed) {
            return SequenceMatch::Complete(*user_event);
        }
        if self.sequences.keys().any(|keys| keys.starts_with(pressed)) {
            SequenceMatch::Partial
        } else {
            SequenceMatch::None
        }
    }

    pub fn keys_for_event(&self, user_event: UserEvent) -> Vec<String> {
        let mut key_events: Vec<KeyEvent> = self
            .iter()
            .filter(|(_, ue)| **ue == user_event)
            .map(|(ke, _)| *ke)
            .collect();
        key_events.sort_by(|a, b| a.partial_cmp(b).unwrap()); // At least when used for key bindings, it doesn't seem to be a problem...
        let mut sequences: Vec<&Vec<KeyEvent>> = self
            .sequences
            .iter()
            .filter(|(_, ue)| **ue == user_event)
            .map(|(keys, _)| keys)
            .collect();
        sequences.sort_by(|a, b| a.partial_cmp(b).unwrap());
        key_events
            .into_iter()
            .map(key_event_to_string)
            .chain(sequences.into_iter().map(|keys| keys_to_string(keys)))
            .collect()
    }

    pub fn user_command_event_numbers(&self) -> Vec<usize> {
        let mut numbers: Vec<usize> = self
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
        numbers
    }
}

impl<'de> Deserialize<'de> for KeyBind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let parsed_map = FxHashMap::<UserEvent, Vec<String>>::deserialize(deserializer)?;
        let mut keybind = KeyBind::default();
        for (user_event, key_events) in parsed_map {
            for key_event_str in key_events {
                // keys separated by whitespace are a sequence
                let parsed: Result<Vec<KeyEvent>, String> = key_event_str
                    .split_whitespace()
                    .map(parse_key_event)
                    .collect();
                let mut keys = match parsed {
                    Ok(keys) if !keys.is_empty() => keys,
                    Ok(_) => {
                        let msg = format!("{key_event_str:?} is not a valid key event: empty");
                        return Err(serde::de::Error::custom(msg));
                    }
                    Err(s) => {
                        let msg = format!("{key_event_str:?} is not a valid key event: {s:}");
                        return Err(serde::de::Error::custom(msg));
                    }
                };
                let conflict = if keys.len() == 1 {
                    keybind.keys.insert(keys.remove(0), user_event)
                } else {
                    keybind.sequences.insert(keys, user_event)
                };
                if let Some(conflict_user_event) = conflict {
                    let msg = format!(
                        "{key_event_str:?} map to multiple events: {user_event:?}, {conflict_user_event:?}"
                    );
                    return Err(serde::de::Error::custom(msg));
                }
            }
        }

        // a key starting a sequence cannot do something on its own, the sequence could not be typed
        for (keys, user_event) in &keybind.sequences {
            if let Some(key_user_event) = keybind.keys.get(&keys[0]) {
                let msg = format!(
                    "{} starts the key sequence of {user_event:?}, so it cannot map to {key_user_event:?}",
                    key_event_to_string(keys[0])
                );
                return Err(serde::de::Error::custom(msg));
            }
        }

        Ok(keybind)
    }
}

fn parse_key_event(raw: &str) -> Result<KeyEvent, String> {
    let raw_lower = raw.to_ascii_lowercase().replace(' ', "");
    let (remaining, modifiers) = extract_modifiers(&raw_lower);
    parse_key_code_with_modifiers(remaining, modifiers)
}

fn extract_modifiers(raw: &str) -> (&str, KeyModifiers) {
    let mut modifiers = KeyModifiers::empty();
    let mut current = raw;

    loop {
        match current {
            rest if rest.starts_with("ctrl-") => {
                modifiers.insert(KeyModifiers::CONTROL);
                current = &rest[5..];
            }
            rest if rest.starts_with("alt-") => {
                modifiers.insert(KeyModifiers::ALT);
                current = &rest[4..];
            }
            rest if rest.starts_with("shift-") => {
                modifiers.insert(KeyModifiers::SHIFT);
                current = &rest[6..];
            }
            _ => break, // break out of the loop if no known prefix is detected
        };
    }

    (current, modifiers)
}

fn parse_key_code_with_modifiers(
    raw: &str,
    mut modifiers: KeyModifiers,
) -> Result<KeyEvent, String> {
    let c = match raw {
        "esc" => KeyCode::Esc,
        "enter" => KeyCode::Enter,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "backtab" => {
            modifiers.insert(KeyModifiers::SHIFT);
            KeyCode::BackTab
        }
        "backspace" => KeyCode::Backspace,
        "delete" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        "f1" => KeyCode::F(1),
        "f2" => KeyCode::F(2),
        "f3" => KeyCode::F(3),
        "f4" => KeyCode::F(4),
        "f5" => KeyCode::F(5),
        "f6" => KeyCode::F(6),
        "f7" => KeyCode::F(7),
        "f8" => KeyCode::F(8),
        "f9" => KeyCode::F(9),
        "f10" => KeyCode::F(10),
        "f11" => KeyCode::F(11),
        "f12" => KeyCode::F(12),
        "space" => KeyCode::Char(' '),
        "hyphen" => KeyCode::Char('-'),
        "minus" => KeyCode::Char('-'),
        "tab" => KeyCode::Tab,
        c if c.len() == 1 => {
            let mut c = c.chars().next().unwrap();
            if modifiers.contains(KeyModifiers::SHIFT) {
                c = c.to_ascii_uppercase();
            }
            KeyCode::Char(c)
        }
        _ => return Err(format!("Unable to parse {raw}")),
    };
    Ok(KeyEvent::new(c, modifiers))
}

// A sequence is displayed with its keys one after another, like "]m".
pub fn keys_to_string(keys: &[KeyEvent]) -> String {
    keys.iter().map(|key| key_event_to_string(*key)).collect()
}

fn key_event_to_string(key_event: KeyEvent) -> String {
    if let KeyCode::Char(c) = key_event.code {
        if key_event.modifiers == KeyModifiers::SHIFT {
            return c.to_ascii_uppercase().into();
        }
    }

    let char;
    let key_code = match key_event.code {
        KeyCode::Backspace => "Backspace",
        KeyCode::Enter => "Enter",
        KeyCode::Left => "Left",
        KeyCode::Right => "Right",
        KeyCode::Up => "Up",
        KeyCode::Down => "Down",
        KeyCode::Home => "Home",
        KeyCode::End => "End",
        KeyCode::PageUp => "PageUp",
        KeyCode::PageDown => "PageDown",
        KeyCode::Tab => "Tab",
        KeyCode::BackTab => "BackTab",
        KeyCode::Delete => "Delete",
        KeyCode::Insert => "Insert",
        KeyCode::F(n) => {
            char = format!("F{n}");
            &char
        }
        KeyCode::Char(' ') => "Space",
        KeyCode::Char(c) => {
            char = c.to_string();
            &char
        }
        KeyCode::Esc => "Esc",
        KeyCode::Null => "",
        KeyCode::CapsLock => "",
        KeyCode::Menu => "",
        KeyCode::ScrollLock => "",
        KeyCode::Media(_) => "",
        KeyCode::NumLock => "",
        KeyCode::PrintScreen => "",
        KeyCode::Pause => "",
        KeyCode::KeypadBegin => "",
        KeyCode::Modifier(_) => "",
    };

    let mut modifiers = Vec::with_capacity(3);

    if key_event.modifiers.intersects(KeyModifiers::CONTROL) {
        modifiers.push("Ctrl");
    }

    if key_event.modifiers.intersects(KeyModifiers::SHIFT) {
        modifiers.push("Shift");
    }

    if key_event.modifiers.intersects(KeyModifiers::ALT) {
        modifiers.push("Alt");
    }

    let mut key = modifiers.join("-");

    if !key.is_empty() {
        key.push('-');
    }
    key.push_str(key_code);

    key
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
            keys: [
                (
                    KeyEvent::new(KeyCode::Char('k'), KeyModifiers::empty()),
                    UserEvent::NavigateUp,
                ),
                (
                    KeyEvent::new(KeyCode::Char('j'), KeyModifiers::empty()),
                    UserEvent::NavigateDown,
                ),
                (
                    KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
                    UserEvent::NavigateDown,
                ),
                (
                    KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL),
                    UserEvent::NavigateLeft,
                ),
                (
                    KeyEvent::new(KeyCode::Char('h'), KeyModifiers::SHIFT),
                    UserEvent::NavigateLeft,
                ),
                (
                    KeyEvent::new(KeyCode::Char('h'), KeyModifiers::ALT),
                    UserEvent::NavigateLeft,
                ),
                (
                    KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL | KeyModifiers::SHIFT),
                    UserEvent::NavigateRight,
                ),
                (
                    KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL | KeyModifiers::SHIFT | KeyModifiers::ALT),
                    UserEvent::NavigateRight,
                ),
                (
                    KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
                    UserEvent::Quit,
                ),
                (
                    KeyEvent::new(KeyCode::F(12), KeyModifiers::empty()),
                    UserEvent::Quit,
                ),
                (
                    KeyEvent::new(KeyCode::Char('d'), KeyModifiers::empty()),
                    UserEvent::UserCommand(1),
                ),
                (
                    KeyEvent::new(KeyCode::Char('e'), KeyModifiers::empty()),
                    UserEvent::UserCommand(10),
                ),
            ]
            .into_iter()
            .collect(),
            sequences: FxHashMap::default(),
        };

        let actual: KeyBind = toml::from_str(toml).unwrap();

        assert_eq!(actual, expected);
    }

    #[test]
    fn test_key_sequences() {
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty());
        let keybind: KeyBind = toml::from_str(
            r#"
            go_to_next = ["] m", "g n"]
            quit = ["q"]
        "#,
        )
        .unwrap();
        assert_eq!(keybind.match_sequence(&[key(']')]), SequenceMatch::Partial);
        assert_eq!(
            keybind.match_sequence(&[key(']'), key('m')]),
            SequenceMatch::Complete(UserEvent::GoToNext)
        );
        assert_eq!(
            keybind.match_sequence(&[key(']'), key('x')]),
            SequenceMatch::None
        );
        assert_eq!(keybind.match_sequence(&[key('q')]), SequenceMatch::None);
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
        assert_eq!(defaults.match_sequence(&[key(']')]), SequenceMatch::None);
        assert_eq!(defaults.get(&key(']')), Some(&UserEvent::Quit));
        assert_eq!(defaults.get(&key('g')), None);
        assert_eq!(
            defaults.match_sequence(&[key('g'), key('g')]),
            SequenceMatch::Complete(UserEvent::GoToTop)
        );
    }

    #[rustfmt::skip]
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
        assert_eq!(key_event_to_string(key_event), "Ctrl-Shift-Alt-l");

        let key_event = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
        assert_eq!(key_event_to_string(key_event), "Esc");

        let key_event = KeyEvent::new(KeyCode::F(12), KeyModifiers::empty());
        assert_eq!(key_event_to_string(key_event), "F12");
    }
}
