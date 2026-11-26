// Work for what is shown, like the diff of the file the cursor moved to, run on another thread so
// the screen keeps up with the keys meanwhile.

use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Condvar, Mutex, MutexGuard, PoisonError,
    },
    time::{Duration, Instant},
};

use crate::{
    event::{AppEvent, Sender},
    keybind::UserEvent,
};

// Where keys act on what loads in the background: a page, a window, a popup. A key acting on what
// still loads is kept until it is loaded, see `App::must_wait`, and loads it before it acts; esc
// and the force quit act at once, see `UserEvent::never_waits`.
pub trait Loads {
    // Whether what `event` acts on still loads: not shown yet, or shown as it was before, like
    // the files of the commit moved from or the diff before a reload. A list reloading in place
    // stays shown, the keys moving through it acting on it; moving to another row leaves what that
    // row shows to load once drawn.
    fn acts_on_loading(&self, event: UserEvent) -> bool;

    // Loads what `event` acts on, waiting for what loads.
    fn load_for(&mut self, event: UserEvent);

    fn waits_for(&self, event: UserEvent) -> bool {
        !event.never_waits() && self.acts_on_loading(event)
    }

    // Before `event` acts.
    fn load_before(&mut self, event: UserEvent) {
        if !event.never_waits() {
            self.load_for(event);
        }
    }
}

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

// When each job running and waited for started, by its id, for the app to draw again once one
// runs long enough to be shown loading, see `next_shown`.
static RUNNING: Mutex<Vec<(u64, Instant)>> = Mutex::new(Vec::new());

fn running() -> MutexGuard<'static, Vec<(u64, Instant)>> {
    lock(&RUNNING)
}

// A mutex locked even when another thread panicked holding it, whose state is still whole for
// what locks it here: the panic stops the app anyway, see the hook in `main`.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// When a job running is shown loading next, after the frame drawn at `drawn`: the app waits for
// events until then, to draw it, see `shows_loading`.
pub fn next_shown(drawn: Instant) -> Option<Instant> {
    running()
        .iter()
        .map(|&(_, started)| started + LOADING_SHOWN_AFTER)
        .filter(|&at| at > drawn)
        .min()
}

// Whether the job some work runs for was dropped, replaced by another, nobody waiting for what it
// makes: long work checks it now and then to stop early, see `Job::read`. The default
// is never cancelled, for the same work run right away.
#[derive(Debug, Default, Clone)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    // Tells the work to stop, for work run without a `Job`, like a reload replaced by another.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
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
    // in `RUNNING` until its work is done or it is dropped
    id: u64,
    started: Instant,
    cancel: Cancel,
}

impl<T> Drop for Job<T> {
    fn drop(&mut self) {
        self.cancel.cancel();
        running().retain(|&(id, _)| id != self.id);
    }
}

// Frees `value` on another thread: a whole repository, of 100k commits, takes the screen ~100ms.
pub fn drop_elsewhere<T: Send + 'static>(value: T) {
    std::thread::spawn(move || drop(value));
}

impl<T: Send + 'static> Job<T> {
    // Runs `work` changing the repository, like staging, which runs to its end even once the job
    // is dropped: it isn't stopped halfway. See `read`.
    pub fn command(tx: Sender, work: impl FnOnce() -> T + Send + 'static) -> Self {
        Self::read(tx, |_| work())
    }

    // Runs `work` reading only, then wakes the app up through `tx` to draw its result; once it
    // has run long enough to be shown loading, the app draws that, see `next_shown`. It stops
    // early once the job is dropped, as `Cancel` tells it: a load replaced by another, like the
    // diff of a file the cursor moved past, or of what was closed. A panic in it stops the app,
    // see `main`.
    pub fn read(tx: Sender, work: impl FnOnce(&Cancel) -> T + Send + 'static) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let started = Instant::now();
        running().push((id, started));
        let cancel = Cancel::default();
        let cancelled = cancel.clone();
        let result: Arc<(Mutex<Option<T>>, Condvar)> = Arc::default();
        let shared = result.clone();
        std::thread::spawn(move || {
            let value = work(&cancelled);
            let (slot, done) = &*shared;
            *lock(slot) = Some(value);
            done.notify_all();
            running().retain(|&(running, _)| running != id);
            // the app may have quit meanwhile
            tx.try_send(AppEvent::Loaded);
        });
        Job {
            result,
            id,
            started,
            cancel,
        }
    }

    // What tells its work to stop, for the tests to see it told.
    #[cfg(test)]
    pub fn cancel_of(&self) -> Cancel {
        self.cancel.clone()
    }

    // When it started, see `shows_loading`.
    pub fn started(&self) -> Instant {
        self.started
    }

    // The result, once the work is done.
    pub fn try_take(&self) -> Option<T> {
        let (slot, _) = &*self.result;
        lock(slot).take()
    }

    // The result, waiting for the work to be done.
    pub fn wait(self) -> T {
        let (slot, done) = &*self.result;
        let mut value = lock(slot);
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
        let kept = Job::read(ec.sender(), work(told.clone()));
        drop(Job::read(ec.sender(), work(told)));
        assert_eq!(stopped.recv_timeout(Duration::from_secs(10)), Ok(true));
        assert!(kept.try_take().is_none());
    }

    #[test]
    fn test_job_result_is_taken_once_done() {
        let ec = EventController::without_terminal();
        let (start, started) = std::sync::mpsc::channel::<()>();
        let job = Job::command(ec.sender(), move || {
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

    // The app draws again once a job running and waited for runs long enough to be shown
    // loading: its deadline is there until its work is done or it is dropped. Other tests' jobs
    // run meanwhile, so only these are looked at.
    #[test]
    fn test_job_is_shown_loading_once_it_runs_long() {
        let ec = EventController::without_terminal();
        let waits = |job_id: u64| running().iter().any(|&(id, _)| id == job_id);
        let quick = Job::command(ec.sender(), || Ok::<_, String>(1));
        let (finish, finished) = std::sync::mpsc::channel::<()>();
        let slow = Job::command(ec.sender(), move || {
            finished.recv().unwrap();
            Ok::<_, String>(())
        });
        let (_, dropped_finished) = std::sync::mpsc::channel::<()>();
        let dropped = Job::command(ec.sender(), move || dropped_finished.recv().ok());
        let dropped_id = dropped.id;
        drop(dropped);
        assert!(!waits(dropped_id));
        assert_eq!(quick.wait(), Ok(1));
        assert!(waits(slow.id));
        let shown = slow.started() + LOADING_SHOWN_AFTER;
        assert!(next_shown(slow.started()).is_some_and(|at| at <= shown));
        finish.send(()).unwrap();
        let slow_id = slow.id;
        slow.wait().unwrap();
        // removed by its thread once done, right after waking the app
        let until = Instant::now() + Duration::from_secs(10);
        while waits(slow_id) {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
