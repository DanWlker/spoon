// Running git: the commands the app runs, as tasks in the background, and the log of every
// command run.

use std::{
    collections::VecDeque,
    ffi::OsStr,
    io::Read,
    path::Path,
    process::{Child, Command, Output, Stdio},
    sync::Mutex,
    time::{Duration, Instant},
};

use chrono::{DateTime, Local};

use super::{without_comments, CommitHash, Operation, Repository};

impl Repository {
    // The message of the commit at HEAD, of which git leaves out the name: `log -1 --format=%B`.
    pub fn head_commit_message(&self) -> std::result::Result<String, String> {
        git_output(&self.path, &["log", "-1", "--format=%B"]).map(|s| s.trim_end().to_string())
    }

    // Runs a git command changing the repository, unless one runs in the background, see
    // `check_idle`.
    pub(super) fn run(
        &self,
        args: &[&str],
        stdin: Option<&str>,
    ) -> std::result::Result<(), String> {
        self.check_idle()?;
        run_logged(
            git_command().args(args).current_dir(&self.path),
            args,
            stdin,
            None,
        )
    }

    // An error while a git command runs in the background, which a change could fail on (its
    // locked index) or end up in the middle of (its stash and pop), like lazygit.
    pub(super) fn check_idle(&self) -> std::result::Result<(), String> {
        match self.running.lock().as_ref() {
            Some(running) => Err(running.busy_message()),
            None => Ok(()),
        }
    }

    // Whether a rebase stopped to edit a commit (rather than on conflicts), which is amended then.
    pub fn rebase_stopped_to_edit(&self) -> bool {
        self.git_dir.join("rebase-merge/amend").exists()
    }

    // The commit message git prepared for the operation in progress, without its comment lines.
    // With the files git lists as conflicted, which it comments out, under "Conflicts:".
    pub fn prepared_message(&self) -> Option<String> {
        let content = self.git_file("MERGE_MSG")?;
        let message = without_comments(&content)?;
        let conflicts: Vec<String> = content
            .lines()
            .skip_while(|l| l.trim() != "# Conflicts:")
            .skip(1)
            .map_while(|l| l.strip_prefix('#').map(str::trim))
            .filter(|path| !path.is_empty())
            .map(|path| format!("  {path}"))
            .collect();
        if conflicts.is_empty() {
            return Some(message);
        }
        Some(format!("{message}\n\nConflicts:\n{}", conflicts.join("\n")))
    }

    // The message `git merge --squash` prepared, until the squash is committed.
    pub fn squash_message(&self) -> Option<String> {
        without_comments(&self.git_file("SQUASH_MSG")?)
    }

    // A file of the repository's git directory, like MERGE_MSG.
    fn git_file(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.git_dir.join(name)).ok()
    }

    // The command continuing a rebase, or the remaining commits of a cherry-pick or revert of
    // several commits, after the commit concluding the stop; None when that commit concludes it.
    pub fn continue_args(&self, operation: Operation) -> Option<GitArgs> {
        // a merge, or a single cherry-pick or revert, is done once committed
        let goes_on = operation == Operation::Rebase
            || (operation != Operation::Merge && self.git_dir.join("sequencer").exists());
        goes_on.then(|| GitCommand::Resume(operation).args(["--continue"]))
    }
}

// The git commands the app runs in the background. What each does to HEAD, to signing and to the
// remotes is told by its variant, so a new command can't be left out of those checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitCommand {
    Apply,
    Branch,
    Checkout,
    CherryPick,
    Clean,
    Commit,
    Fetch,
    Merge,
    Pull,
    Push,
    Rebase,
    Remote,
    Reset,
    Restore,
    Revert,
    Rm,
    Stash,
    Tag,
    // going on with the operation stopped, or aborting it: `rebase --continue`, `merge --abort`
    Resume(Operation),
}

impl GitCommand {
    pub fn name(self) -> &'static str {
        match self {
            GitCommand::Apply => "apply",
            GitCommand::Branch => "branch",
            GitCommand::Checkout => "checkout",
            GitCommand::CherryPick => "cherry-pick",
            GitCommand::Clean => "clean",
            GitCommand::Commit => "commit",
            GitCommand::Fetch => "fetch",
            GitCommand::Merge => "merge",
            GitCommand::Pull => "pull",
            GitCommand::Push => "push",
            GitCommand::Rebase => "rebase",
            GitCommand::Remote => "remote",
            GitCommand::Reset => "reset",
            GitCommand::Restore => "restore",
            GitCommand::Revert => "revert",
            GitCommand::Rm => "rm",
            GitCommand::Stash => "stash",
            GitCommand::Tag => "tag",
            GitCommand::Resume(operation) => operation.name(),
        }
    }

    // Whether it moves or rewrites HEAD, which git refuses or tangles up while a rebase, merge,
    // cherry-pick or revert waits to be continued or aborted; going on with it or aborting it doesn't.
    pub fn moves_head(self) -> bool {
        use GitCommand::*;
        match self {
            Checkout | Rebase | Merge | CherryPick | Revert | Reset | Pull => true,
            Apply | Branch | Clean | Commit | Fetch | Push | Remote | Restore | Rm | Stash
            | Tag | Resume(_) => false,
        }
    }

    // Whether it may make a commit or a tag, which git signs when configured to.
    pub fn may_sign(self) -> bool {
        use GitCommand::*;
        match self {
            Commit | Rebase | CherryPick | Merge | Revert | Tag | Pull | Resume(_) => true,
            Apply | Branch | Checkout | Clean | Fetch | Push | Remote | Reset | Restore | Rm
            | Stash => false,
        }
    }

    // Whether it talks to a remote, one at a time, see `run_task`.
    pub fn is_remote(self) -> bool {
        use GitCommand::*;
        match self {
            Fetch | Pull | Push => true,
            Apply | Branch | Checkout | CherryPick | Clean | Commit | Merge | Rebase | Remote
            | Reset | Restore | Revert | Rm | Stash | Tag | Resume(_) => false,
        }
    }

    // The command with its arguments.
    pub fn args<S: Into<String>>(self, args: impl IntoIterator<Item = S>) -> GitArgs {
        GitArgs {
            command: self,
            args: args.into_iter().map(Into::into).collect(),
        }
    }
}

// A git command and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitArgs {
    pub command: GitCommand,
    pub args: Vec<String>,
}

impl GitArgs {
    // What git is run with, the command first.
    pub fn argv(&self) -> Vec<String> {
        std::iter::once(self.command.name().to_string())
            .chain(self.args.iter().cloned())
            .collect()
    }
}

// As in the popups and the command log: "commit --quiet --file=-".
impl std::fmt::Display for GitArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.argv().join(" "))
    }
}

// A lock held only while read or set; a panic while holding it left a value that is still whole.
pub(super) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

// The app's git command running in the background, shared with the repositories it loads, which
// load on other threads, see `RunningTask`.
#[derive(Debug, Default, Clone)]
pub struct SharedTask(std::sync::Arc<std::sync::Mutex<Option<RunningTask>>>);

impl SharedTask {
    // Held only while read or set, never across another lock of it: it isn't reentrant.
    pub fn lock(&self) -> std::sync::MutexGuard<'_, Option<RunningTask>> {
        lock(&self.0)
    }
}

// The git command running in the background, and what waits for it to finish. The app sets it
// and the repository reads it, sharing one, see `Repository::check_idle`.
#[derive(Debug)]
pub struct RunningTask {
    pub task: GitTask,
    // HEAD before committing the message of the local changes page, which is cleared once the
    // commit is made, see `App::finish_task`
    pub head_before_commit: Option<CommitHash>,
    // quit was asked meanwhile, the app quits once it finishes
    pub quit_after: bool,
}

impl RunningTask {
    // Why the repository can't be changed now.
    pub fn busy_message(&self) -> String {
        format!("{}, wait for it to finish", self.task.running)
    }
}

// Git commands run in the background, like fetch, merge or checkout, and what to say about them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GitTask {
    // the git commands, each run when the one before succeeded
    pub commands: Vec<GitArgs>,
    // stash the local changes before the command, and reapply them after
    pub stash: bool,
    pub env: Vec<(String, String)>,
    // "Pushing main to origin/main"
    pub running: String,
    // "Pushed main to origin/main"
    pub done: String,
    // made of the picked commits, which are cleared once it is done
    pub uses_picks: bool,
    pub after: AfterTask,
    // the local branch it pushes or pulls, shown beside it in the sidebar while it runs, like lazygit
    pub branch_status: Option<BranchStatus>,
    // what the first command reads, like the message of a commit
    pub input: Option<String>,
    // said when it fails instead of git's error, which may tell little
    pub failed: Option<String>,
    // stopped once it runs longer, like a fetch from a remote that never answers
    pub timeout: Option<Duration>,
}

// What the app shows once a git command is done.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum AfterTask {
    // the repository reloaded, the page as it was
    #[default]
    Reload,
    // and the commit this many before HEAD selected, as rewriting the history changes its hash
    SelectBehindHead(usize),
    // the local changes page, it left changes to commit
    LocalChanges,
    // only the working tree changed, the page stays as it is instead of being reloaded
    KeepView,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchStatus {
    pub branch: String,
    // "Pushing", and "↑" where that doesn't fit
    pub label: String,
    pub short: &'static str,
}

impl GitTask {
    // Whether it may make a commit or a tag, see `GitCommand::may_sign`.
    pub fn may_sign(&self) -> bool {
        self.commands.iter().any(|c| c.command.may_sign())
    }

    // Whether it moves or rewrites HEAD, see `GitCommand::moves_head`.
    pub fn moves_head(&self) -> bool {
        self.commands.iter().any(|c| c.command.moves_head())
    }

    // Whether it talks to a remote, see `GitCommand::is_remote`.
    pub fn is_remote(&self) -> bool {
        self.commands.iter().any(|c| c.command.is_remote())
    }

    // Whether it makes a commit with a typed message, which the message box is then cleared of.
    pub fn commits(&self) -> bool {
        self.commands
            .first()
            .is_some_and(|c| c.command == GitCommand::Commit)
            && self.input.is_some()
    }

    pub fn new(command: GitCommand, args: &[&str], running: String, done: String) -> Self {
        GitTask {
            commands: vec![command.args(args.iter().copied())],
            running,
            done,
            ..Default::default()
        }
    }

    // An interactive rebase of the commits after `base` (all of them when None), its todo list
    // changed by the sed `expressions`, see `todo_action`.
    pub fn rebase_todo(base: Option<&str>, auto_stash: bool, expressions: &[String]) -> Self {
        let stash = if auto_stash {
            "--autostash"
        } else {
            "--no-autostash"
        };
        // each expression changes its line, else the rebase stops before starting, as git's
        // config could have changed the todo list
        let sed = std::iter::once("sed -i.bak -E".to_string())
            .chain(expressions.iter().map(|e| format!("-e '{e}'")))
            .collect::<Vec<_>>()
            .join(" ");
        let editor = format!(
            "todo() {{ {sed} \"$1\" && test \"$(diff \"$1.bak\" \"$1\" | grep -c '^>')\" = {} \
             || {{ echo 'spoon: a commit is missing from the todo list of the rebase' >&2; exit 1; }}; }}; todo",
            expressions.len()
        );
        let env = [
            ("GIT_SEQUENCE_EDITOR", editor.as_str()),
            // the todo list names the commits with full hashes, of SHA-1 and SHA-256 alike; set as
            // `git -c` does, leaving the user's GIT_CONFIG_COUNT entries as they are
            ("GIT_CONFIG_PARAMETERS", "'core.abbrev=no'"),
        ];
        GitTask {
            // autosquash would move and squash `fixup!` commits nobody picked
            commands: vec![GitCommand::Rebase.args([
                "-i",
                "--no-autosquash",
                stash,
                base.unwrap_or("--root"),
            ])],
            env: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
        }
    }
}

// A sed expression turning the pick of the commit (a full hash) in a rebase todo list into
// `action`, like "squash" or "drop".
pub fn todo_action(hash: &str, action: &str) -> String {
    format!("s/^(pick|p) {hash} /{action} {hash} /")
}

// Runs the command of a task, between stashing and reapplying the local changes if it says so.
pub fn run_task(path: &Path, task: &GitTask) -> std::result::Result<(), String> {
    // one remote command at a time, like lazygit's sync mutex: a pull waits for the fetch on
    // launch instead of both locking the same remote refs
    static REMOTE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _remote = task.is_remote().then(|| {
        REMOTE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    });
    // stashed when the stash changed: git stashes nothing for some changes it lists, like those
    // in a submodule, and popping then would take an older stash
    let stash_top = || git_output(path, &["rev-parse", "--verify", "--quiet", "refs/stash"]).ok();
    let stashed = task.stash && {
        let before = stash_top();
        let message = format!("spoon: {}", task.running);
        let args = GitCommand::Stash.args(["push", "--include-untracked", "-m", &message]);
        run_task_step(path, &args, None, task)?;
        stash_top() != before
    };
    let pop = || run_task_step(path, &GitCommand::Stash.args(["pop"]), None, task);

    let run = || {
        task.commands.iter().enumerate().try_for_each(|(i, args)| {
            let input = task.input.as_deref().filter(|_| i == 0);
            run_task_step(path, args, input, task)
        })
    };
    match run() {
        Ok(()) => {}
        Err(err) if stashed => {
            // nothing changed, the local changes go back
            return Err(match pop() {
                Ok(()) => err,
                Err(_) => format!("{err} (the local changes are kept in the stash)"),
            });
        }
        Err(err) => return Err(err),
    }
    if stashed {
        pop().map_err(|err| {
            format!(
                "{}, but reapplying the local changes failed (they are kept in the stash): {err}",
                task.done
            )
        })?;
    }
    Ok(())
}

// It runs while the app owns the terminal, so git must not prompt: no editor (the messages git
// prepares are used as they are), and credentials come from a helper or an agent.
fn run_task_step(
    path: &Path,
    args: &GitArgs,
    input: Option<&str>,
    task: &GitTask,
) -> std::result::Result<(), String> {
    let args = args.argv();
    let mut cmd = git_command();
    cmd.args(&args)
        .current_dir(path)
        .env("GIT_EDITOR", "true")
        .env("GIT_TERMINAL_PROMPT", "0")
        .envs(task.env.iter().map(|(k, v)| (k, v)));
    if task.is_remote() {
        let has_ssh_command = std::env::var_os("GIT_SSH_COMMAND").is_some()
            || std::env::var_os("GIT_SSH").is_some()
            || git_output(path, &["config", "--get", "core.sshCommand"]).is_ok();
        if !has_ssh_command {
            cmd.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
        }
    }
    run_logged(&mut cmd, &args, input, task.timeout)
}

// Runs `cmd`, git with `args`, writing `input` to it, and logs it in the command log; the error
// says what went wrong in git's words when it fails.
pub(super) fn run_logged(
    cmd: &mut Command,
    args: &[impl AsRef<str>],
    input: Option<&str>,
    timeout: Option<Duration>,
) -> std::result::Result<(), String> {
    let name = args.first().map_or("", |a| a.as_ref());
    let command_error = |e: std::io::Error| {
        let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
        format!("failed to run git {}: {e}", args.join(" "))
    };
    let started = Instant::now();
    let mut child = cmd
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(command_error)?;
    if let Some(input) = input {
        use std::io::Write;
        // closed once written, so git can finish
        let mut stdin = child.stdin.take().expect("stdin is piped");
        match stdin.write_all(input.as_bytes()) {
            // git stopped before reading it, like on a failing hook: its output says why
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
            result => result.map_err(command_error)?,
        }
    }
    let output = match timeout {
        None => child.wait_with_output().map_err(command_error)?,
        Some(timeout) => match wait_with_timeout(child, timeout).map_err(command_error)? {
            Some(output) => output,
            None => {
                let error = format!("git {name} timed out after {}s", timeout.as_secs());
                log_command(args, false, &error, started);
                return Err(error);
            }
        },
    };
    // git reports progress and results of many commands on stderr
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    log_command(args, output.status.success(), &text, started);
    if output.status.success() {
        return Ok(());
    }
    // a rejected ref or a conflict says what went wrong best, then the error lines, and the
    // hints after them how to fix it
    let lines = || text.lines().map(str::trim);
    let error = lines()
        .find(|line| line.starts_with("! ") || line.starts_with("CONFLICT"))
        .or_else(|| lines().find(|line| line.starts_with("error:") || line.starts_with("fatal:")))
        .or_else(|| lines().rfind(|line| !line.is_empty()))
        .unwrap_or_default();
    Err(format!("git {name} failed: {error}"))
}

// Like `Child::wait_with_output`, but stops the command once `timeout` passed, None then.
fn wait_with_timeout(mut child: Child, timeout: Duration) -> std::io::Result<Option<Output>> {
    // read meanwhile, a command filling a pipe would wait for it to be read
    fn read(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut buf = vec![];
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            buf
        })
    }
    let stdout = read(child.stdout.take());
    let stderr = read(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            // SIGTERM, not `Child::kill`'s SIGKILL: git then removes its lock files and the pack it
            // was receiving, which would otherwise make the next fetch fail
            let _ = Command::new("kill").arg(child.id().to_string()).status();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    Ok(Some(Output {
        status,
        stdout,
        stderr,
    }))
}

// The command stashing the changes of `paths` (all changes when empty), including untracked files.
pub fn stash_args(paths: &[String], message: &str) -> GitArgs {
    let mut args: Vec<String> = ["push", "--include-untracked", "--quiet"]
        .map(String::from)
        .to_vec();
    if !message.is_empty() {
        args.extend(["--message".into(), message.into()]);
    }
    if !paths.is_empty() {
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        args.push("--".into());
        args.extend(pathspecs(&paths));
    }
    GitCommand::Stash.args(args)
}

// A git command of the app, taking no lock it can do without, like `git status` refreshing the
// index, which would make the user's own git commands fail meanwhile. In tests, the git config of
// the user's home and system is left out, so the tests behave alike everywhere.
pub fn git_command() -> Command {
    let mut cmd = Command::new("git");
    cmd.env("GIT_OPTIONAL_LOCKS", "0");
    #[cfg(test)]
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    cmd
}

// Paths of files or directories for git, taken as they are, not as patterns: a file named
// `*.log` is only that file. The root of the repository stays ":/".
pub fn pathspecs(paths: &[&str]) -> Vec<String> {
    paths
        .iter()
        .map(|path| match *path {
            ":/" => path.to_string(),
            path => format!(":(literal){path}"),
        })
        .collect()
}

// The output of git whatever its exit status, with the status: some commands tell with it, like
// `merge-tree` a conflict or `diff --no-index` files that differ. None when git didn't run.
pub(super) fn git_output_with_status(
    path: &Path,
    args: &[impl AsRef<OsStr>],
) -> Option<(Option<i32>, String)> {
    let output = git_command()
        .args(args)
        .current_dir(path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let out = String::from_utf8_lossy(&output.stdout).into_owned();
    Some((output.status.code(), out))
}

pub(super) fn git_output(path: &Path, args: &[&str]) -> std::result::Result<String, String> {
    git_output_bytes(path, args).map(|out| String::from_utf8_lossy(&out).into_owned())
}

pub(super) fn git_output_bytes(path: &Path, args: &[&str]) -> std::result::Result<Vec<u8>, String> {
    let output = git_command()
        .args(args)
        .current_dir(path)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to run git {}: {e}", args.join(" ")))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("git {} failed: {}", args[0], stderr.trim()))
    }
}

// A git command the app ran that changes the repository, for the command log. Reads, like
// those of the refresh every few seconds, are left out.
#[derive(Debug, Clone)]
pub struct LoggedCommand {
    pub at: DateTime<Local>,
    // "git push origin main", with passwords in URLs hidden
    pub command: String,
    pub ok: bool,
    // stdout then stderr, its last lines
    pub output: String,
    pub duration: Duration,
}

// shortcut: kept in memory, the last ones only; written to a file if they're needed after quitting
const COMMAND_LOG_SIZE: usize = 500;

const COMMAND_LOG_OUTPUT_LINES: usize = 200;

static COMMAND_LOG: Mutex<VecDeque<LoggedCommand>> = Mutex::new(VecDeque::new());

fn log_command(args: &[impl AsRef<str>], ok: bool, output: &str, started: Instant) {
    let args: Vec<String> = args.iter().map(|a| hide_password(a.as_ref())).collect();
    let lines: Vec<&str> = output.lines().collect();
    let output = lines[lines.len().saturating_sub(COMMAND_LOG_OUTPUT_LINES)..].join("\n");
    let entry = LoggedCommand {
        at: Local::now(),
        command: format!("git {}", args.join(" ")),
        ok,
        output,
        duration: started.elapsed(),
    };
    // a command run while another thread panicked holding it still gets logged
    let mut log = COMMAND_LOG.lock().unwrap_or_else(|e| e.into_inner());
    log.push_front(entry);
    log.truncate(COMMAND_LOG_SIZE);
}

// The commands logged, the newest first.
pub fn command_log() -> Vec<LoggedCommand> {
    let log = COMMAND_LOG.lock().unwrap_or_else(|e| e.into_inner());
    log.iter().cloned().collect()
}

// "https://user:token@host/r.git" -> "https://***@host/r.git"
fn hide_password(arg: &str) -> String {
    let Some(scheme_end) = arg.find("://").map(|i| i + 3) else {
        return arg.to_string();
    };
    let host_end = arg[scheme_end..]
        .find('/')
        .map_or(arg.len(), |i| scheme_end + i);
    match arg[scheme_end..host_end].rfind('@') {
        Some(at) => format!("{}***{}", &arg[..scheme_end], &arg[scheme_end + at..]),
        None => arg.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::refs_snapshot;
    use crate::git::SortCommit;

    // A git command running in the background, saying `running`.
    fn running(running: &str) -> RunningTask {
        let task = GitTask {
            running: running.into(),
            ..Default::default()
        };
        RunningTask {
            task,
            head_before_commit: None,
            quit_after: false,
        }
    }

    #[test]
    fn test_busy_repository_refuses_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let git = crate::test_support::git::GitRepository::new(path);
        git.init();
        git.commit("init", "2024-01-01");
        std::fs::write(path.join("a"), "a\n").unwrap();
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();

        // while a git command runs in the background
        *repository.running.lock() = Some(running("Pushing main"));
        assert_eq!(
            repository.stage_paths(&["a"]),
            Err("Pushing main, wait for it to finish".into())
        );
        *repository.running.lock() = None;
        repository.stage_paths(&["a"]).unwrap();
    }

    #[test]
    fn test_rebase_todo() {
        use crate::test_support::git::git;
        // SHA-256 hashes are longer than the 40 characters of SHA-1
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        git(
            path,
            &["init", "-q", "-b", "master", "--object-format=sha256"],
        );
        for (key, value) in [
            ("user.name", "T"),
            ("user.email", "t@e"),
            ("commit.gpgsign", "false"),
            ("rebase.autoSquash", "true"),
        ] {
            git(path, &["config", key, value]);
        }
        for subject in ["c1", "c2", "fixup! c1", "c3"] {
            git(path, &["commit", "-q", "--allow-empty", "-m", subject]);
        }
        let log = || git(path, &["log", "--format=%s"]);
        let hash = |rev: &str| git(path, &["rev-parse", rev]).trim().to_string();
        let base = hash("HEAD~3");

        // dropping the fixup drops it, nothing is squashed though autoSquash is set
        let todo = [todo_action(&hash("HEAD~1"), "drop")];
        run_task(path, &GitTask::rebase_todo(Some(&base), false, &todo)).unwrap();
        assert_eq!(log(), "c3\nc2\nc1\n");

        // a commit not in the todo list stops the rebase before it changes anything
        let todo = [todo_action(&"0".repeat(64), "drop")];
        let err = run_task(path, &GitTask::rebase_todo(Some(&base), false, &todo)).unwrap_err();
        assert!(err.contains("missing from the todo list"), "{err}");
        assert_eq!(log(), "c3\nc2\nc1\n");
        assert!(!path.join(".git/rebase-merge").exists());
    }

    #[test]
    fn test_task_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let git = crate::test_support::git::GitRepository::new(path);
        git.init();
        git.commit("init", "2024-01-01");
        // the first command reads it, like the message of a commit
        let task = GitTask {
            input: Some("from stdin".into()),
            ..GitTask::new(
                GitCommand::Commit,
                &["--allow-empty", "--no-verify", "-F", "-"],
                String::new(),
                String::new(),
            )
        };
        run_task(path, &task).unwrap();
        let message = git_output(path, &["log", "-1", "--format=%s"]).unwrap();
        assert_eq!(message.trim(), "from stdin");
    }

    #[test]
    fn test_task_moves_head() {
        use GitCommand::*;
        let task =
            |command, args: &[&str]| GitTask::new(command, args, String::new(), String::new());
        assert!(task(Checkout, &["-b", "topic", "HEAD"]).moves_head());
        assert!(task(Pull, &["origin", "main"]).moves_head());
        assert!(!task(Branch, &["topic", "HEAD"]).moves_head());
        // going on with the operation, or aborting it
        assert!(!task(Resume(Operation::Rebase), &["--continue"]).moves_head());
        assert!(!task(Resume(Operation::Merge), &["--abort"]).moves_head());
        // discarding files restores them
        assert!(!task(Restore, &["--", "f"]).moves_head());
        let mut tag_then_reset = task(Tag, &["v1"]);
        tag_then_reset.commands.push(Reset.args(["--hard"]));
        assert!(tag_then_reset.moves_head());
    }

    #[test]
    fn test_task_may_sign() {
        use GitCommand::*;
        let task =
            |command, args: &[&str]| GitTask::new(command, args, String::new(), String::new());
        assert!(task(Rebase, &["-i", "main"]).may_sign());
        assert!(!task(Fetch, &["origin"]).may_sign());
        let mut delete_tag = task(Branch, &["-D", "x"]);
        delete_tag.commands.push(Tag.args(["v1"]));
        assert!(delete_tag.may_sign());
    }

    #[test]
    fn test_stash_kept_when_not_reapplied() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let git = |args: &[&str]| git_output(path, args).unwrap();
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(path.join("f"), "1\n").unwrap();
        git(&["add", "f"]);
        git(&["commit", "-q", "-m", "c1"]);
        git(&["checkout", "-q", "-b", "other"]);
        std::fs::write(path.join("f"), "other\n").unwrap();
        git(&["commit", "-q", "-am", "c2"]);
        git(&["checkout", "-q", "main"]);
        let stashed = |args: &[&str]| GitTask {
            stash: true,
            ..GitTask::new(
                GitCommand::Checkout,
                args,
                "Checking out".into(),
                "Checked out".into(),
            )
        };

        // the command failing, the changes come back as they were
        std::fs::write(path.join("f"), "local\n").unwrap();
        let err = run_task(path, &stashed(&["-q", "nowhere"])).unwrap_err();
        assert!(err.starts_with("git checkout failed:"), "{err}");
        assert_eq!(std::fs::read_to_string(path.join("f")).unwrap(), "local\n");
        assert_eq!(git(&["stash", "list"]), "");

        // reapplying them conflicting, they are kept in the stash, and the user told so
        let err = run_task(path, &stashed(&["-q", "other"])).unwrap_err();
        assert!(err.starts_with("Checked out, but reapplying the local changes failed (they are kept in the stash):"), "{err}");
        assert_eq!(
            git(&["stash", "list", "--format=%s"]),
            "On main: spoon: Checking out\n"
        );
    }

    #[test]
    fn test_hook_failing_before_reading_the_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let git = |args: &[&str]| git_output(path, args).unwrap();
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(path.join("f"), "1\n").unwrap();
        git(&["add", "f"]);
        let hook = path.join(".git/hooks/pre-commit");
        std::fs::write(&hook, "#!/bin/sh\necho 'hook says no' >&2\nexit 1\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();

        // longer than a pipe holds, so writing it fails once git is gone
        let task = GitTask {
            input: Some("m".repeat(1 << 20)),
            ..GitTask::new(
                GitCommand::Commit,
                &["--quiet", "--file=-"],
                String::new(),
                String::new(),
            )
        };
        let err = run_task(path, &task).unwrap_err();
        assert_eq!(err, "git commit failed: hook says no");
    }

    #[test]
    fn test_stash_only_what_git_stashes() {
        let dir = tempfile::tempdir().unwrap();
        let (sub, path) = (dir.path().join("sub"), dir.path().join("work"));
        let git = |dir: &Path, args: &[&str]| git_output(dir, args).unwrap();
        for dir in [&sub, &path] {
            std::fs::create_dir(dir).unwrap();
            git(dir, &["init", "-q", "-b", "main"]);
            git(dir, &["config", "user.name", "t"]);
            git(dir, &["config", "user.email", "t@t"]);
            git(dir, &["config", "commit.gpgsign", "false"]);
            std::fs::write(dir.join("f"), "1\n").unwrap();
            git(dir, &["add", "f"]);
            git(dir, &["commit", "-q", "-m", "c1"]);
        }
        let sub_url = sub.to_str().unwrap();
        let add = [
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            sub_url,
            "sub",
        ];
        git(&path, &add);
        git(&path, &["commit", "-q", "-m", "sub"]);
        // an older stash, and changes only in the submodule, which git doesn't stash
        std::fs::write(path.join("f"), "2\n").unwrap();
        git(&path, &["stash", "-q", "-m", "older"]);
        std::fs::write(path.join("sub/f"), "2\n").unwrap();
        assert!(!git(&path, &["status", "--porcelain"]).is_empty());

        let task = GitTask {
            stash: true,
            ..GitTask::new(GitCommand::Branch, &["other"], String::new(), String::new())
        };
        run_task(&path, &task).unwrap();
        // the older stash is left alone
        let stashes = git(&path, &["stash", "list", "--format=%s"]);
        assert_eq!(stashes, "On main: older\n");
    }

    #[test]
    fn test_command_log() {
        use crate::test_support::git::GitRepository;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let repo = GitRepository::new(path);
        repo.init();
        repo.commit("a", "2024-01-01");
        let repository = Repository::load(path, SortCommit::Chronological, None, false).unwrap();
        // the log is shared by the tests running at the same time, so this one is found by name
        assert!(repository
            .stage_paths(&["no-such-file-for-the-log"])
            .is_err());
        let log = command_log();
        let entry = log
            .iter()
            .find(|e| e.command.contains("no-such-file-for-the-log"))
            .unwrap();
        assert!(!entry.ok);
        assert!(entry.command.starts_with("git add -- "));
        assert!(
            entry.output.contains("did not match any files"),
            "{}",
            entry.output
        );

        assert_eq!(
            hide_password("https://me:token@github.com/a/b.git"),
            "https://***@github.com/a/b.git"
        );
        assert_eq!(
            hide_password("git@github.com:a/b.git"),
            "git@github.com:a/b.git"
        );
        assert_eq!(hide_password("https://host/a@b"), "https://host/a@b");
    }

    #[test]
    fn test_launch_fetch_moves_snapshot_and_times_out() {
        use crate::test_support::git::GitRepository;

        let dir = tempfile::tempdir().unwrap();
        let remote = dir.path().join("remote.git");
        let work = dir.path().join("work");
        std::fs::create_dir(&work).unwrap();
        git_output(dir.path(), &["init", "-q", "--bare", "remote.git"]).unwrap();
        let repo = GitRepository::new(&work);
        repo.init();
        repo.commit("first", "2024-01-01");
        let url = remote.to_str().unwrap();
        git_output(&work, &["remote", "add", "origin", url]).unwrap();
        git_output(&work, &["push", "-q", "origin", "HEAD:refs/heads/main"]).unwrap();
        let fetch = |timeout| GitTask {
            timeout,
            ..GitTask::new(GitCommand::Fetch, &["--all"], String::new(), String::new())
        };

        // a branch made on the remote shows in the fingerprint once fetched, which reloads the app
        let head = git_output(&work, &["rev-parse", "HEAD"]).unwrap();
        let other = [
            "--git-dir",
            url,
            "update-ref",
            "refs/heads/other",
            head.trim(),
        ];
        git_output(dir.path(), &other).unwrap();
        let before = refs_snapshot(&work, &work.join(".git")).unwrap();
        run_task(&work, &fetch(None)).unwrap();
        assert_ne!(refs_snapshot(&work, &work.join(".git")).unwrap(), before);

        // a remote that doesn't answer is given up on, and leaves nothing in the way of the next fetch
        let slow = [
            "config",
            "remote.origin.uploadpack",
            "sleep 5; git-upload-pack",
        ];
        git_output(&work, &slow).unwrap();
        let started = Instant::now();
        let err = run_task(&work, &fetch(Some(Duration::from_millis(300)))).unwrap_err();
        assert_eq!(err, "git fetch timed out after 0s");
        assert!(started.elapsed() < Duration::from_secs(3));
        git_output(&work, &["config", "--unset", "remote.origin.uploadpack"]).unwrap();
        run_task(&work, &fetch(None)).unwrap();
    }

    #[test]
    fn test_rebase_todo_keeps_config_of_environment() {
        let task = GitTask::rebase_todo(None, false, &[]);
        let output = git_command()
            .args(["config", "--get-regexp", r"^(core\.abbrev|spoon\.kept)$"])
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "spoon.kept")
            .env("GIT_CONFIG_VALUE_0", "yes")
            .envs(task.env.iter().map(|(k, v)| (k, v)))
            .output()
            .unwrap();
        let config = String::from_utf8_lossy(&output.stdout);
        assert!(config.contains("spoon.kept yes"), "{config}");
        assert!(config.contains("core.abbrev no"), "{config}");
    }
}
