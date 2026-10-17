use std::{cell::RefCell, path::Path, process::Command};

use arboard::Clipboard;

use crate::config::ClipboardConfig;

thread_local! {
    static CLIPBOARD: RefCell<Option<Clipboard>> = const { RefCell::new(None) };
}

pub fn copy_to_clipboard(value: String, config: &ClipboardConfig) -> Result<(), String> {
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

    child
        .wait()
        .map_err(|e| format!("{} failed: {e}", commands[0]))?;

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

// Opens `path` in the user's editor: $VISUAL, then $EDITOR, then vi.
// The variables are expanded by the shell, so they may include arguments, like "code --wait".
pub fn editor_command(path: &Path) -> Vec<String> {
    vec![
        "sh".into(),
        "-c".into(),
        r#"exec ${VISUAL:-${EDITOR:-vi}} "$1""#.into(),
        "sh".into(),
        path.to_string_lossy().into_owned(),
    ]
}

// Resolves the conflicts of `path` (relative to `toplevel`) in a merge tool, git's merge.tool unless given.
pub fn merge_tool_command(toplevel: &Path, path: &str, tool: Option<&str>) -> Vec<String> {
    let mut command: Vec<String> = vec![
        "git".into(),
        "-C".into(),
        toplevel.to_string_lossy().into_owned(),
        "mergetool".into(),
        "--no-prompt".into(),
    ];
    if let Some(tool) = tool {
        command.extend(["--tool".into(), tool.into()]);
    }
    command.extend(["--".into(), path.into()]);
    command
}

// Runs an interactive command in the terminal, which the app gives up meanwhile, see App::run_external.
pub fn exec_interactive(command: &[String]) -> Result<(), String> {
    let status = Command::new(&command[0])
        .args(&command[1..])
        .status()
        .map_err(|e| format!("Failed to execute {}: {e}", command[0]))?;
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
fn command_name(command: &[String]) -> String {
    if command[0] != "sh" {
        return command[0].clone();
    }
    let set = |var| std::env::var(var).ok().filter(|v| !v.is_empty());
    set("VISUAL")
        .or_else(|| set("EDITOR"))
        .unwrap_or_else(|| "vi".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_not_found() {
        let missing = ["nosuchcommand-spoon".to_string()];
        assert!(exec_interactive(&missing)
            .unwrap_err()
            .starts_with("Failed to execute"));
        let sh = |script: &str| ["sh", "-c", script].map(String::from).to_vec();
        let err = exec_interactive(&sh("exit 127")).unwrap_err();
        assert!(err.ends_with(": command not found"), "{err}");
        let err = exec_interactive(&sh("exit 3")).unwrap_err();
        assert!(
            err.ends_with("exited with non-zero status: exit status: 3"),
            "{err}"
        );
    }

    #[test]
    fn test_editor_and_merge_tool_commands() {
        let editor = editor_command(Path::new("/repo/a b.txt"));
        assert_eq!(editor[0], "sh");
        assert_eq!(editor[4], "/repo/a b.txt");

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
        assert!(!tool.contains(&"--tool".to_string()));
    }
}
