// Checks for changes made outside the app like lazygit's refresher, in the background.

use std::{
    path::PathBuf,
    sync::{mpsc, Arc, Weak},
    thread,
    time::Duration,
};

use crate::{
    event::{AppEvent, Sender},
    git::{refs_snapshot, FilesSnapshot, IndexRefresh},
};

// What the refresher sent last, held by its events until the app handled them: the next check
// waits for that, like lazygit's refresher waiting for each refresh to finish, so none pile up
// while the app is busy or gives the terminal to an editor. The tests send events with a default one.
#[derive(Debug, Default)]
pub struct Unhandled(#[expect(dead_code, reason = "held until handled")] Arc<()>);

// Checks in the background, without holding up the app, for changes made outside it like lazygit's
// refresher: the refs, HEAD and the operation in progress every `refs_interval`, the local changes every `files_interval`
// (a multiple of it), and both whenever poked through the sender returned, once the app handled
// what it sent before, see `Unhandled`.
pub fn spawn(
    tx: Sender,
    work_tree: PathBuf,
    git_dir: PathBuf,
    refs_interval: Duration,
    files_interval: Duration,
) -> mpsc::Sender<()> {
    let (poke, poked) = mpsc::channel();
    // the files load every few ticks of the refs, at least every one
    let files_every = (files_interval.as_millis() / refs_interval.as_millis().max(1)).max(1);
    thread::spawn(move || {
        // stops once the app quits and the events are no longer received
        let mut tick = 0u128;
        let mut sent = Weak::new();
        // a poke while the app hadn't handled the last check, checked once it has
        let mut owed = false;
        loop {
            owed |= match poked.recv_timeout(refs_interval) {
                Ok(()) => true,
                Err(mpsc::RecvTimeoutError::Timeout) => false,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            if sent.strong_count() > 0 {
                continue;
            }
            tick += 1;
            let unhandled = Arc::new(());
            sent = Arc::downgrade(&unhandled);
            // sent unless the app quit
            if let Ok(refs) = refs_snapshot(&work_tree, &git_dir) {
                let event = AppEvent::RefsSnapshot(refs, Unhandled(unhandled.clone()));
                if !tx.try_send(event) {
                    break;
                }
            }
            // poked on getting the focus back or after the app's own change, see `IndexRefresh`
            let refresh = match std::mem::take(&mut owed) {
                true => Some(IndexRefresh::Saved),
                false => tick
                    .is_multiple_of(files_every)
                    .then_some(IndexRefresh::Skipped),
            };
            if let Some(refresh) = refresh {
                let files = FilesSnapshot::load(&work_tree, &git_dir, refresh);
                if !tx.try_send(AppEvent::FilesTick(files, Unhandled(unhandled))) {
                    break;
                }
            }
        }
    });
    poke
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::git::GitRepository;

    // A repository with an untracked file, and a refresher reporting to the receiver returned.
    fn refresher(
        refs_interval: Duration,
        files_interval: Duration,
    ) -> (
        tempfile::TempDir,
        mpsc::Sender<()>,
        mpsc::Receiver<AppEvent>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let git = GitRepository::new(path);
        git.init();
        git.commit("init", "2024-01-01");
        std::fs::write(path.join("a"), "a\n").unwrap();
        let (tx, rx) = Sender::channel();
        let poke = spawn(
            tx,
            path.to_path_buf(),
            path.join(".git"),
            refs_interval,
            files_interval,
        );
        (dir, poke, rx)
    }

    fn next(rx: &mpsc::Receiver<AppEvent>) -> AppEvent {
        rx.recv_timeout(Duration::from_secs(10)).unwrap()
    }

    #[test]
    fn test_refresher_ticks() {
        let ms = Duration::from_millis;
        let (_dir, _poke, rx) = refresher(ms(10), ms(20));
        // the refs every tick, the files every other one
        assert!(matches!(next(&rx), AppEvent::RefsSnapshot(..)));
        assert!(matches!(next(&rx), AppEvent::RefsSnapshot(..)));
        let AppEvent::FilesTick(files, _) = next(&rx) else {
            panic!("no files after the second tick");
        };
        assert_eq!(files.changes.unstaged[0].path, "a");
    }

    // A check the app doesn't handle, like while an editor runs, holds the next ones back rather
    // than piling up; a poke meanwhile checks the files once it is handled.
    #[test]
    fn test_refresher_waits_for_the_last_check_handled() {
        let ms = Duration::from_millis;
        let (_dir, poke, rx) = refresher(ms(10), ms(3_600_000));
        let held = next(&rx);
        assert!(matches!(held, AppEvent::RefsSnapshot(..)));
        poke.send(()).unwrap();
        std::thread::sleep(ms(200));
        assert!(rx.try_recv().is_err());
        drop(held);
        assert!(matches!(next(&rx), AppEvent::RefsSnapshot(..)));
        assert!(matches!(next(&rx), AppEvent::FilesTick(..)));
    }

    // The files checked on a tick leave the index as it is; checked when poked, like on getting
    // the focus back, git saves what it refreshed of it: a file touched, unchanged, isn't looked
    // at again by the next checks.
    #[test]
    fn test_poked_check_saves_the_index_refreshed() {
        let ms = Duration::from_millis;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let git = GitRepository::new(path);
        git.init();
        std::fs::write(path.join("a"), "a\n").unwrap();
        git.add(&["a"]);
        git.commit("init", "2024-01-01");
        let index = path.join(".git/index");
        let touched = std::time::SystemTime::now() + Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(path.join("a"))
            .unwrap()
            .set_modified(touched)
            .unwrap();
        let written = || std::fs::metadata(&index).unwrap().modified().unwrap();
        let before = written();
        let (tx, rx) = Sender::channel();
        let poke = spawn(tx, path.to_path_buf(), path.join(".git"), ms(10), ms(20));
        // the first tick, then the files on the second
        assert!(matches!(next(&rx), AppEvent::RefsSnapshot(..)));
        assert!(matches!(next(&rx), AppEvent::RefsSnapshot(..)));
        // held, the next check waits for it, so the poke is the one after
        let held = next(&rx);
        assert!(matches!(held, AppEvent::FilesTick(..)));
        assert_eq!(written(), before);
        poke.send(()).unwrap();
        drop(held);
        assert!(matches!(next(&rx), AppEvent::RefsSnapshot(..)));
        assert!(matches!(next(&rx), AppEvent::FilesTick(..)));
        assert_ne!(written(), before);
    }

    #[test]
    fn test_refresher_poked() {
        // the intervals don't pass during the test, only a poke wakes it up
        let hour = Duration::from_secs(3600);
        let (_dir, poke, rx) = refresher(hour, hour);
        poke.send(()).unwrap();
        assert!(matches!(next(&rx), AppEvent::RefsSnapshot(..)));
        assert!(matches!(next(&rx), AppEvent::FilesTick(..)));
        // it stops once the app no longer pokes it
        drop(poke);
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(10)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
    }
}
