// Work for what is shown, like the diff of the file the cursor moved to, run on another thread so
// the screen keeps up with the keys meanwhile.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex, PoisonError,
    },
    time::{Duration, Instant},
};

use crate::event::{AppEvent, Sender};

// The value of work on a scoped thread, see `std::thread::scope`. A panic in it stops the app in
// the hook of `main` before it gets here; without that hook, in the tests, it goes on here.
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

// Whether the job some work runs for was dropped, replaced by another, nobody waiting for what it
// makes: long work checks it now and then to stop early, see `Job::spawn_cancellable`. The default
// is never cancelled, for the same work run right away.
#[derive(Debug, Default, Clone)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    // One cancelled already, for the tests of the work stopping early.
    #[cfg(test)]
    pub fn cancelled() -> Self {
        Cancel(Arc::new(AtomicBool::new(true)))
    }
}

// The result of work running on another thread: taken when drawing once there, or waited for
// before a key acts on what it is for. Dropped, its result goes unused, and the work is told to
// stop, see `Cancel`.
#[derive(Debug)]
pub struct Job<T> {
    result: Arc<(Mutex<Option<T>>, Condvar)>,
    started: Instant,
    cancel: Cancel,
}

impl<T> Drop for Job<T> {
    fn drop(&mut self) {
        self.cancel.0.store(true, Ordering::Relaxed);
    }
}

impl<T: Send + 'static> Job<T> {
    // Runs `work`, then wakes the app up through `tx` to draw its result; also once it has run
    // long enough to be shown loading, see `shows_loading`. A panic in it stops the app, see
    // `main`. It runs to its end even once the job is dropped: what changes the repository, like
    // staging, isn't stopped halfway.
    pub fn spawn(tx: Sender, work: impl FnOnce() -> T + Send + 'static) -> Self {
        Self::spawn_cancellable(tx, |_| work())
    }

    // `spawn` for work reading only, which stops early once the job is dropped, as `Cancel`
    // tells it: a load replaced by another, like the diff of a file the cursor moved past.
    pub fn spawn_cancellable(tx: Sender, work: impl FnOnce(&Cancel) -> T + Send + 'static) -> Self {
        let cancel = Cancel::default();
        let cancelled = cancel.clone();
        let result: Arc<(Mutex<Option<T>>, Condvar)> = Arc::default();
        let shared = result.clone();
        let waiting = Arc::downgrade(&result);
        let wake = tx.clone();
        std::thread::spawn(move || {
            let value = work(&cancelled);
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
            cancel,
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

    // A job dropped, replaced by another, tells its work to stop; one kept doesn't.
    #[test]
    fn test_dropped_job_cancels_its_work() {
        let ec = EventController::without_terminal();
        let (told, stopped) = std::sync::mpsc::channel();
        let work = |told: std::sync::mpsc::Sender<bool>| {
            move |cancel: &Cancel| {
                let until = Instant::now() + Duration::from_secs(30);
                while !cancel.is_cancelled() && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(1));
                }
                told.send(cancel.is_cancelled()).unwrap();
            }
        };
        let kept = Job::spawn_cancellable(ec.sender(), work(told.clone()));
        drop(Job::spawn_cancellable(ec.sender(), work(told)));
        assert_eq!(stopped.recv_timeout(Duration::from_secs(10)), Ok(true));
        assert!(kept.try_take().is_none());
    }

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
