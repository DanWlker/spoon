// The time of the app in the tests, see `AppContext::now`: it stands still until a test moves it
// on, so how fast a machine runs a test doesn't tell a double click from two clicks, nor when the
// popup waiting for a second press shows. Each test's own, on its thread.

use std::{
    cell::Cell,
    time::{Duration, Instant},
};

thread_local! {
    static NOW: Cell<Option<Instant>> = const { Cell::new(None) };
}

pub fn now() -> Instant {
    NOW.with(|now| {
        let at = now.get().unwrap_or_else(Instant::now);
        now.set(Some(at));
        at
    })
}

pub fn advance(by: Duration) {
    NOW.with(|now| now.set(Some(now.get().unwrap_or_else(Instant::now) + by)));
}
