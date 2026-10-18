// The keys each part of the app takes, registered in one place for that part: the status bar, the
// keys popup and the help page all list them from there, and the app hands a part only the events
// of the keys it registered as working where the focus is, so no key works without being listed.

use crate::event::{KeyHint, UserEvent};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub events: Vec<UserEvent>,
    // in a few words, for the status bar and the keys popup; empty for the keys only the help
    // lists, like moving about
    pub short: String,
    // in a sentence, for the help page
    pub long: &'static str,
    // works where the focus is
    pub active: bool,
}

// The keys of a part of the app, registered on `view`, whose state decides which work and their
// short texts; without one, for the help page, all of them.
pub struct Keys<'a, T: ?Sized> {
    view: Option<&'a T>,
    keys: Vec<Key>,
}

impl<'a, T: ?Sized> Keys<'a, T> {
    pub fn of(view: &'a T) -> Self {
        Keys {
            view: Some(view),
            keys: vec![],
        }
    }

    pub fn all() -> Self {
        Keys {
            view: None,
            keys: vec![],
        }
    }

    // A key, shown as `short` where it works.
    pub fn add(&mut self, events: &[UserEvent], short: &str, long: &'static str) -> &mut Self {
        self.keys.push(Key {
            events: events.to_vec(),
            short: short.into(),
            long,
            active: true,
        });
        self
    }

    // A key working without being shown in the status bar or the keys popup, like moving about.
    pub fn quiet(&mut self, events: &[UserEvent], long: &'static str) -> &mut Self {
        self.add(events, "", long)
    }

    // The last key works only where `applies` says so of the view.
    pub fn when(&mut self, applies: impl FnOnce(&T) -> bool) -> &mut Self {
        if let (Some(view), Some(key)) = (self.view, self.keys.last_mut()) {
            key.active &= applies(view);
        }
        self
    }

    // The short text of the last key, from the view.
    pub fn short(&mut self, short: impl FnOnce(&T) -> String) -> &mut Self {
        if let (Some(view), Some(key)) = (self.view, self.keys.last_mut()) {
            key.short = short(view);
        }
        self
    }

    // A key listed where it works, unlisted where it only says why it doesn't, like a command
    // refused while a rebase waits, and off elsewhere.
    pub fn add_where(
        &mut self,
        events: &[UserEvent],
        short: &str,
        long: &'static str,
        availability: impl FnOnce(&T) -> Availability,
    ) -> &mut Self {
        let availability = self.view.map(availability);
        self.quiet(events, long)
            .when(|_| availability == Some(Availability::Warns));
        // the listed one last, for `short` to change
        self.add(events, short, long)
            .when(|_| availability == Some(Availability::Works))
    }

    // The view, None while registering every key for the help page.
    pub fn view(&self) -> Option<&'a T> {
        self.view
    }

    pub fn list(self) -> KeyList {
        KeyList(self.keys)
    }
}

// Where a key is, see `Keys::add_where`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Works,
    Warns,
    Off,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct KeyList(Vec<Key>);

impl KeyList {
    // Whether a key of the event works.
    pub fn takes(&self, event: UserEvent) -> bool {
        self.0
            .iter()
            .any(|key| key.active && key.events.contains(&event))
    }

    // The keys working and shown, for the status bar and the keys popup; those with the same text
    // together, like [ ] and h l stepping through commits.
    pub fn hints(&self) -> Vec<KeyHint> {
        let mut hints: Vec<KeyHint> = vec![];
        for key in self.0.iter().filter(|k| k.active && !k.short.is_empty()) {
            match hints.iter_mut().find(|(_, short)| *short == key.short) {
                Some((events, _)) => events.extend(key.events.iter().copied()),
                None => hints.push((key.events.clone(), key.short.clone())),
            }
        }
        hints
    }

    // Every key with its sentence, for the help page: the keys with the same sentence together,
    // like one registered for several states.
    pub fn help(&self) -> Vec<(Vec<UserEvent>, &'static str)> {
        let mut help: Vec<(Vec<UserEvent>, &'static str)> = vec![];
        for key in &self.0 {
            let events = match help.iter_mut().find(|(_, long)| *long == key.long) {
                Some((events, _)) => events,
                None => {
                    help.push((vec![], key.long));
                    &mut help.last_mut().unwrap().0
                }
            };
            for event in &key.events {
                if !events.contains(event) {
                    events.push(*event);
                }
            }
        }
        help
    }

    pub fn extend(&mut self, other: KeyList) {
        self.0.extend(other.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct View {
        on: bool,
    }

    fn register(k: &mut Keys<View>) {
        use UserEvent::*;
        k.add(
            &[OlderCommit, NewerCommit],
            "step",
            "Step through the commits",
        );
        k.add(&[NavigateLeft], "step", "Step back, on the commit")
            .when(|v| v.on);
        k.quiet(&[NavigateDown], "Move down");
        k.add(&[Cancel], "back", "Back, then close").when(|v| v.on);
        k.add(&[Cancel], "close", "Back, then close")
            .when(|v| !v.on);
    }

    #[test]
    fn test_registered_keys() {
        use UserEvent::*;
        let view = View { on: false };
        let mut keys = Keys::of(&view);
        register(&mut keys);
        let keys = keys.list();
        // only the keys working are taken, the quiet ones too, and shown
        assert!(keys.takes(NavigateDown) && keys.takes(Cancel));
        assert!(!keys.takes(NavigateLeft) && !keys.takes(Quit));
        assert_eq!(
            keys.hints(),
            [
                (vec![OlderCommit, NewerCommit], "step".to_string()),
                (vec![Cancel], "close".to_string())
            ]
        );
        let view = View { on: true };
        let mut keys = Keys::of(&view);
        register(&mut keys);
        // the same text together
        assert_eq!(
            keys.list().hints()[0],
            (
                vec![OlderCommit, NewerCommit, NavigateLeft],
                "step".to_string()
            )
        );

        // for the help, all of them, once
        let mut keys = Keys::all();
        register(&mut keys);
        let help = keys.list().help();
        assert_eq!(help.len(), 4);
        assert!(help.contains(&(vec![Cancel], "Back, then close")));
    }
}
