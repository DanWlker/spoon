// Work for what is shown, like the diff of the file the cursor moved to, run on another thread so
// the screen keeps up with the keys meanwhile.

use std::sync::{Arc, Condvar, Mutex, PoisonError};

use crate::event::{AppEvent, Sender};

// The result of work running on another thread: taken when drawing once there, or waited for
// before a key acts on what it is for. Dropped, its result goes unused.
#[derive(Debug)]
pub struct Job<T> {
    result: Arc<(Mutex<Option<T>>, Condvar)>,
}

impl<T: Send + 'static> Job<T> {
    // Runs `work`, then wakes the app up through `tx` to draw its result.
    // shortcut: a thread per job, the work of one replaced still runs to its end; queue them on
    // one worker if moving fast through big diffs piles up git processes.
    pub fn spawn(tx: Sender, work: impl FnOnce() -> T + Send + 'static) -> Self {
        let result: Arc<(Mutex<Option<T>>, Condvar)> = Arc::default();
        let shared = result.clone();
        std::thread::spawn(move || {
            let value = work();
            let (slot, done) = &*shared;
            *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(value);
            done.notify_all();
            // the app may have quit meanwhile
            tx.try_send(AppEvent::Loaded);
        });
        Job { result }
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
            42
        });
        // not done until let go
        assert_eq!(job.try_take(), None);
        start.send(()).unwrap();
        assert_eq!(job.wait(), 42);
        // and the app is woken up to draw it
        assert!(matches!(ec.recv(), AppEvent::Loaded));
    }
}
