// Work for what is shown, like the diff of the file the cursor moved to, run on another thread so
// the screen keeps up with the keys meanwhile.

use std::{
    sync::{Arc, Condvar, Mutex, PoisonError},
    time::{Duration, Instant},
};

use crate::event::{AppEvent, Sender};

// The value of work on a scoped thread, see `std::thread::scope`; a panic in it goes on here.
pub fn joined<T>(handle: std::thread::ScopedJoinHandle<'_, T>) -> T {
    handle
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

// How long work runs before the screen says it is loading, so quick work doesn't flash it.
pub const LOADING_SHOWN_AFTER: Duration = Duration::from_secs(1);

// Whether work running `since` then is shown as loading, see `LOADING_SHOWN_AFTER`.
pub fn shows_loading(since: Instant) -> bool {
    since.elapsed() >= LOADING_SHOWN_AFTER
}

// The result of work running on another thread: taken when drawing once there, or waited for
// before a key acts on what it is for. Dropped, its result goes unused.
#[derive(Debug)]
pub struct Job<T> {
    result: Arc<(Mutex<Option<T>>, Condvar)>,
    started: Instant,
}

impl<T: Send + 'static> Job<T> {
    // Runs `work`, then wakes the app up through `tx` to draw its result; also once it has run
    // long enough to be shown loading, see `shows_loading`. A panic in it stops the app, see
    // `main`.
    // shortcut: a thread per job, the work of one replaced still runs to its end; queue them on
    // one worker if moving fast through big diffs piles up git processes.
    pub fn spawn(tx: Sender, work: impl FnOnce() -> T + Send + 'static) -> Self {
        let result: Arc<(Mutex<Option<T>>, Condvar)> = Arc::default();
        let shared = result.clone();
        let waiting = Arc::downgrade(&result);
        let wake = tx.clone();
        std::thread::spawn(move || {
            let value = work();
            let (slot, done) = &*shared;
            *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(value);
            done.notify_all();
            // the app may have quit meanwhile
            tx.try_send(AppEvent::Loaded);
        });
        std::thread::spawn(move || {
            std::thread::sleep(LOADING_SHOWN_AFTER);
            // held by the job and its work: still running and waited for
            if waiting.strong_count() == 2 {
                wake.try_send(AppEvent::Loaded);
            }
        });
        Job {
            result,
            started: Instant::now(),
        }
    }

    // When it started, see `shows_loading`.
    pub fn started(&self) -> Instant {
        self.started
    }

    // The result, once the work is done.
    pub fn try_take(&self) -> Option<T> {
        let (slot, _) = &*self.result;
        slot.lock().unwrap_or_else(PoisonError::into_inner).take()
    }

    // The result, waiting for the work to be done.
    pub fn wait(self) -> T {
        let (slot, done) = &*self.result;
        let mut value = slot.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(value) = value.take() {
                return value;
            }
            value = done.wait(value).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventController;

    #[test]
    fn test_job_result_is_taken_once_done() {
        let ec = EventController::without_terminal();
        let (start, started) = std::sync::mpsc::channel::<()>();
        let job = Job::spawn(ec.sender(), move || {
            started.recv().unwrap();
            Ok::<_, String>(42)
        });
        // not done until let go
        assert_eq!(job.try_take(), None);
        start.send(()).unwrap();
        assert_eq!(job.wait(), Ok(42));
        // and the app is woken up to draw it
        assert!(matches!(ec.recv(), AppEvent::Loaded));
    }

    #[test]
    fn test_job_wakes_the_app_once_it_runs_long_enough_to_show() {
        let ec = EventController::without_terminal();
        let wait = Duration::from_secs(5);
        let quick = Job::spawn(ec.sender(), || Ok::<_, String>(1));
        let (finish, finished) = std::sync::mpsc::channel::<()>();
        let slow = Job::spawn(ec.sender(), move || {
            finished.recv().unwrap();
            Ok::<_, String>(())
        });
        // the quick one, done
        assert!(matches!(ec.recv_timeout(wait), Some(AppEvent::Loaded)));
        assert_eq!(quick.try_take(), Some(Ok(1)));
        assert!(!shows_loading(slow.started()));
        // the slow one, still running, wakes the app to say it is loading
        assert!(matches!(ec.recv_timeout(wait), Some(AppEvent::Loaded)));
        assert!(shows_loading(slow.started()));
        // the quick one, done before, doesn't
        std::thread::sleep(Duration::from_millis(200));
        assert!(ec.try_recv().is_none());
        finish.send(()).unwrap();
        slow.wait().unwrap();
        assert!(matches!(ec.recv_timeout(wait), Some(AppEvent::Loaded)));
    }
}
