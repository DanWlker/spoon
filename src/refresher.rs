// Checks for changes made outside the app like lazygit's refresher, in the background.

use std::{path::PathBuf, sync::mpsc, thread, time::Duration};

use crate::{
    background::catching,
    event::{AppEvent, Sender},
    git::{refs_snapshot, FilesSnapshot},
};

// Checks in the background, without holding up the app, for changes made outside it like lazygit's
// refresher: the refs, HEAD and the operation in progress every `refs_interval`, the local changes every `files_interval`
// (a multiple of it), and both whenever poked through the sender returned.
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
        // a check panicking is told once, and checked again on the next tick
        let mut told = false;
        loop {
            let now = match poked.recv_timeout(refs_interval) {
                Ok(()) => true,
                Err(mpsc::RecvTimeoutError::Timeout) => false,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            tick += 1;
            // sends what a check found unless the app quit; returns whether it did
            let mut send = |checked: Result<Option<AppEvent>, String>| match checked {
                Ok(Some(event)) => tx.try_send(event),
                Ok(None) => true,
                Err(_) if told => true,
                Err(err) => {
                    told = true;
                    tx.try_send(AppEvent::NotifyError(err))
                }
            };
            let refs = catching(|| refs_snapshot(&work_tree, &git_dir).ok());
            if !send(refs.map(|refs| refs.map(AppEvent::RefsSnapshot))) {
                break;
            }
            if now || tick.is_multiple_of(files_every) {
                let files = catching(|| FilesSnapshot::load(&work_tree, &git_dir));
                if !send(files.map(|files| Some(AppEvent::FilesTick(files)))) {
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
        assert!(matches!(next(&rx), AppEvent::RefsSnapshot(_)));
        assert!(matches!(next(&rx), AppEvent::RefsSnapshot(_)));
        let AppEvent::FilesTick(files) = next(&rx) else {
            panic!("no files after the second tick");
        };
        assert_eq!(files.changes.unstaged[0].path, "a");
    }

    #[test]
    fn test_refresher_poked() {
        // the intervals don't pass during the test, only a poke wakes it up
        let hour = Duration::from_secs(3600);
        let (_dir, poke, rx) = refresher(hour, hour);
        poke.send(()).unwrap();
        assert!(matches!(next(&rx), AppEvent::RefsSnapshot(_)));
        assert!(matches!(next(&rx), AppEvent::FilesTick(_)));
        // it stops once the app no longer pokes it
        drop(poke);
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(10)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
    }
}
