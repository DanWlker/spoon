use std::{
    cell::RefCell,
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
};

use arboard::Clipboard;

use crate::config::ClipboardConfig;

thread_local! {
    static CLIPBOARD: RefCell<Option<Clipboard>> = const { RefCell::new(None) };
}

pub fn copy_to_clipboard(value: String, config: &ClipboardConfig) -> Result<(), String> {
    // a byte that isn't UTF-8, kept from git, copied as U+FFFD, see `text_from_git`
    let value = match value.chars().any(crate::git::is_kept_byte) {
        true => String::from_utf8_lossy(&crate::git::git_bytes(&value)).into_owned(),
        false => value,
    };
    match config {
        ClipboardConfig::Auto => copy_to_clipboard_auto(value),
        ClipboardConfig::Custom { commands } => copy_to_clipboard_custom(value, commands),
    }
}

fn copy_to_clipboard_custom(value: String, commands: &[String]) -> Result<(), String> {
    use std::io::Write;
    use std::process::Stdio;

    if commands.is_empty() {
        return Err("No clipboard command specified".to_string());
    }

    let mut child = Command::new(&commands[0])
        .args(&commands[1..])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to run {}: {e}", commands[0]))?;

    child
        .stdin
        .take()
        .expect("stdin should be available")
        .write_all(value.as_bytes())
        .map_err(|e| format!("Failed to write to {}: {e}", commands[0]))?;

    let status = child
        .wait()
        .map_err(|e| format!("{} failed: {e}", commands[0]))?;
    if !status.success() {
        return Err(format!("{} failed: {status}", commands[0]));
    }
    Ok(())
}

fn copy_to_clipboard_auto(value: String) -> Result<(), String> {
    CLIPBOARD.with_borrow_mut(|clipboard| {
        if clipboard.is_none() {
            *clipboard = Clipboard::new()
                .map(Some)
                .map_err(|e| format!("Failed to create clipboard: {e:?}"))?;
        }

        clipboard
            .as_mut()
            .expect("The clipboard should have been initialized above")
            .set_text(value)
            .map_err(|e| format!("Failed to copy to clipboard: {e:?}"))
    })
}

// Opens `paths` in the user's editor: $VISUAL, then $EDITOR, then vi; one path at `line`, from
// 1, when given, see `line_args`.
// The variables are expanded by the shell, so they may include arguments, like "code --wait".
pub fn editor_command(paths: &[PathBuf], line: Option<usize>) -> Vec<OsString> {
    let mut command: Vec<OsString> = vec![
        "sh".into(),
        "-c".into(),
        r#"exec ${VISUAL:-${EDITOR:-vi}} "$@""#.into(),
        "sh".into(),
    ];
    match (paths, line) {
        ([path], Some(line)) => command.extend(line_args(&editor(), path, line)),
        _ => command.extend(paths.iter().map(|path| path.into())),
    }
    command
}

// The arguments opening `path` at `line` in `editor`, as its name takes them, like lazygit's
// editor presets; an editor not known here opens the file at its start.
fn line_args(editor: &str, path: &Path, line: usize) -> Vec<OsString> {
    let first = editor.split_whitespace().next().unwrap_or_default();
    let name = first.rsplit('/').next().unwrap_or_default();
    let at_line = || {
        let mut arg = path.as_os_str().to_owned();
        arg.push(format!(":{line}"));
        arg
    };
    match name {
        "vi" | "vim" | "nvim" | "gvim" | "nano" | "emacs" | "emacsclient" | "micro" | "kak" => {
            vec![format!("+{line}").into(), path.into()]
        }
        "code" | "code-insiders" | "codium" | "cursor" => vec!["--goto".into(), at_line()],
        "hx" | "helix" | "zed" | "subl" => vec![at_line()],
        _ => vec![path.into()],
    }
}

// The editor `editor_command` runs, as the variables name it.
fn editor() -> String {
    let set = |var| std::env::var(var).ok().filter(|v| !v.is_empty());
    set("VISUAL")
        .or_else(|| set("EDITOR"))
        .unwrap_or_else(|| "vi".into())
}

// Resolves the conflicts of `path` (relative to `toplevel`) in a merge tool, git's merge.tool unless given.
pub fn merge_tool_command(toplevel: &Path, path: &str, tool: Option<&str>) -> Vec<OsString> {
    let mut command: Vec<OsString> = vec![
        "git".into(),
        "-C".into(),
        toplevel.into(),
        "mergetool".into(),
        "--no-prompt".into(),
    ];
    if let Some(tool) = tool {
        command.extend(["--tool".into(), tool.into()]);
    }
    command.extend(["--".into(), crate::git::git_arg(path)]);
    command
}

// Runs an interactive command in the terminal, which the app gives up meanwhile, see App::run_external.
pub fn exec_interactive(command: &[OsString]) -> Result<(), String> {
    let status = Command::new(&command[0])
        .args(&command[1..])
        .status()
        .map_err(|e| format!("Failed to execute {}: {e}", command[0].display()))?;
    if status.success() {
        return Ok(());
    }
    // the shell's message is gone with the screen drawn again, so its codes are told in words
    let name = command_name(command);
    Err(match status.code() {
        Some(127) => format!("{name}: command not found"),
        Some(126) => format!("{name}: not executable"),
        _ => format!("{name} exited with non-zero status: {status}"),
    })
}

// What the user knows the command by: the editor of `editor_command`, not the shell running it.
fn command_name(command: &[OsString]) -> String {
    if command[0] != "sh" {
        return command[0].to_string_lossy().into_owned();
    }
    editor()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_not_found() {
        let missing = [OsString::from("nosuchcommand-spoon")];
        assert!(exec_interactive(&missing)
            .unwrap_err()
            .starts_with("Failed to execute"));
        let sh = |script: &str| ["sh", "-c", script].map(OsString::from).to_vec();
        let err = exec_interactive(&sh("exit 127")).unwrap_err();
        assert!(err.ends_with(": command not found"), "{err}");
        let err = exec_interactive(&sh("exit 3")).unwrap_err();
        assert!(
            err.ends_with("exited with non-zero status: exit status: 3"),
            "{err}"
        );
    }

    // A clipboard command failing used to say it copied.
    #[test]
    fn test_failing_clipboard_command() {
        let sh = |script: &str| ["sh", "-c", script].map(String::from).to_vec();
        assert!(copy_to_clipboard_custom("x".into(), &sh("cat >/dev/null")).is_ok());
        let err = copy_to_clipboard_custom("x".into(), &sh("cat >/dev/null; exit 1")).unwrap_err();
        assert!(err.contains("failed"), "{err}");
    }

    // A byte that isn't UTF-8, kept from git, is copied as U+FFFD, not as the character keeping it.
    #[test]
    fn test_copy_of_bytes_not_in_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("copied");
        let script = format!("cat > '{}'", out.display());
        let commands = ["sh", "-c", &script].map(String::from).to_vec();
        let config = ClipboardConfig::Custom { commands };
        copy_to_clipboard(crate::git::text_from_git(b"caf\xe9"), &config).unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "caf\u{FFFD}");
    }

    #[test]
    fn test_editor_and_merge_tool_commands() {
        let editor = editor_command(
            &[
                PathBuf::from("/repo/a b.txt"),
                PathBuf::from("/repo/b c.txt"),
            ],
            Some(3),
        );
        assert_eq!(editor[0], "sh");
        assert_eq!(editor[4], "/repo/a b.txt");
        assert_eq!(editor[5], "/repo/b c.txt");

        let tool = merge_tool_command(Path::new("/repo"), "a.txt", Some("meld"));
        let expected = [
            "git",
            "-C",
            "/repo",
            "mergetool",
            "--no-prompt",
            "--tool",
            "meld",
            "--",
            "a.txt",
        ];
        assert_eq!(tool, expected);
        let tool = merge_tool_command(Path::new("/repo"), "a.txt", None);
        assert!(!tool.contains(&"--tool".into()));
    }

    // The line goes as the editor takes it; to one it doesn't know, the file alone.
    #[test]
    fn test_editor_at_a_line() {
        let path = Path::new("/repo/a.txt");
        let args = |editor| line_args(editor, path, 7);
        assert_eq!(args("nvim"), ["+7", "/repo/a.txt"]);
        assert_eq!(args("/usr/bin/vim -p"), ["+7", "/repo/a.txt"]);
        assert_eq!(args("code --wait"), ["--goto", "/repo/a.txt:7"]);
        assert_eq!(args("hx"), ["/repo/a.txt:7"]);
        assert_eq!(args("ed"), ["/repo/a.txt"]);
    }

    // A file name that isn't UTF-8 reached the editor and the merge tool as U+FFFD.
    #[test]
    fn test_editor_and_merge_tool_get_the_bytes_of_the_name() {
        use std::os::unix::ffi::OsStrExt;
        let name = crate::git::text_from_git(b"caf\xe9.txt");
        let path = crate::git::work_path(Path::new("/repo"), &name);
        let editor = editor_command(std::slice::from_ref(&path), None);
        assert_eq!(editor[4].as_bytes(), b"/repo/caf\xe9.txt");
        let tool = merge_tool_command(Path::new("/repo"), &name, None);
        assert_eq!(tool.last().unwrap().as_bytes(), b"caf\xe9.txt");
    }
}
