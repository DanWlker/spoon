use std::{cell::RefCell, path::Path, process::Command};

use arboard::Clipboard;

use crate::config::ClipboardConfig;

const USER_COMMAND_MARKER_PREFIX: &str = "{{";
const USER_COMMAND_TARGET_HASH_MARKER: &str = "{{target_hash}}";
pub const USER_COMMAND_MARKED_HASH_MARKER: &str = "{{marked_hash}}";
const USER_COMMAND_PICKED_HASHES_MARKER: &str = "{{picked_hashes}}";
const USER_COMMAND_FIRST_PARENT_HASH_MARKER: &str = "{{first_parent_hash}}";
const USER_COMMAND_PARENT_HASHES_MARKER: &str = "{{parent_hashes}}";
const USER_COMMAND_REFS_MARKER: &str = "{{refs}}";
const USER_COMMAND_BRANCHES_MARKER: &str = "{{branches}}";
const USER_COMMAND_REMOTE_BRANCHES_MARKER: &str = "{{remote_branches}}";
const USER_COMMAND_TAGS_MARKER: &str = "{{tags}}";
const USER_COMMAND_STASH_MARKER: &str = "{{stash}}";
const USER_COMMAND_AREA_WIDTH_MARKER: &str = "{{area_width}}";
const USER_COMMAND_AREA_HEIGHT_MARKER: &str = "{{area_height}}";

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

pub struct ExternalCommandParameters<'a> {
    pub command: &'a [String],
    pub target_hash: &'a str,
    // commits picked with space, in picking order
    pub picked_hashes: Vec<&'a str>,
    pub parent_hashes: Vec<&'a str>,
    pub all_refs: Vec<&'a str>,
    pub branches: Vec<&'a str>,
    pub remote_branches: Vec<&'a str>,
    pub tags: Vec<&'a str>,
    pub stash: Option<&'a str>,
    pub area_width: u16,
    pub area_height: u16,
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
        Ok(())
    } else {
        Err(format!(
            "{} exited with non-zero status: {status}",
            command[0]
        ))
    }
}

// {{marked_hash}} refers to a single picked commit, so it is ambiguous with several.
pub fn validate_marked_hash(command: &[String], picked_count: usize) -> Result<(), String> {
    let uses_marked_hash = command
        .iter()
        .any(|arg| arg.contains(USER_COMMAND_MARKED_HASH_MARKER));
    if uses_marked_hash && picked_count > 1 {
        return Err(format!(
            "{USER_COMMAND_MARKED_HASH_MARKER} needs one picked commit, but {picked_count} are picked; use {USER_COMMAND_PICKED_HASHES_MARKER} for several"
        ));
    }
    Ok(())
}

pub fn exec_user_command(params: ExternalCommandParameters) -> Result<String, String> {
    let command = build_user_command(&params);

    let output = Command::new(&command[0])
        .args(&command[1..])
        .output()
        .map_err(|e| format!("Failed to execute command: {e:?}"))?;

    if !output.status.success() {
        let msg = format!(
            "Command exited with non-zero status: {}, stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        return Err(msg);
    }

    Ok(String::from_utf8_lossy(&output.stdout).into())
}

pub fn exec_user_command_suspend(params: ExternalCommandParameters) -> Result<(), String> {
    let command = build_user_command(&params);

    let output = Command::new(&command[0])
        .args(&command[1..])
        .status()
        .map_err(|e| format!("Failed to execute command: {e:?}"))?;

    if !output.success() {
        let msg = format!("Command exited with non-zero status: {output}");
        return Err(msg);
    }

    Ok(())
}

fn build_user_command(params: &ExternalCommandParameters) -> Vec<String> {
    fn to_vec(ss: &[&str]) -> Vec<String> {
        ss.iter().map(|s| s.to_string()).collect()
    }
    let mut command = Vec::new();
    for arg in params.command {
        if !arg.contains(USER_COMMAND_MARKER_PREFIX) {
            command.push(arg.clone());
            continue;
        }
        match arg.as_str() {
            // If the marker is used as a standalone argument, expand it into multiple arguments.
            // This allows the command to receive each item as a separate argument and correctly handle items that contain spaces.
            USER_COMMAND_BRANCHES_MARKER => command.extend(to_vec(&params.branches)),
            USER_COMMAND_REMOTE_BRANCHES_MARKER => command.extend(to_vec(&params.remote_branches)),
            USER_COMMAND_TAGS_MARKER => command.extend(to_vec(&params.tags)),
            USER_COMMAND_REFS_MARKER => command.extend(to_vec(&params.all_refs)),
            USER_COMMAND_PARENT_HASHES_MARKER => command.extend(to_vec(&params.parent_hashes)),
            USER_COMMAND_PICKED_HASHES_MARKER => command.extend(to_vec(&params.picked_hashes)),
            // Otherwise, replace the marker within the single argument string.
            _ => command.push(replace_command_arg(arg, params)),
        }
    }
    command
}

fn replace_command_arg(s: &str, params: &ExternalCommandParameters) -> String {
    let sep = " ";
    let target_hash = params.target_hash;
    // with several picked commits, using {{marked_hash}} is rejected earlier, see `validate_marked_hash`
    let marked_hash = match params.picked_hashes.as_slice() {
        [hash] => hash,
        _ => "",
    };
    let picked_hashes = &params.picked_hashes.join(sep);
    let first_parent_hash = &params.parent_hashes.first().cloned().unwrap_or_default();
    let parent_hashes = &params.parent_hashes.join(sep);
    let all_refs = &params.all_refs.join(sep);
    let branches = &params.branches.join(sep);
    let remote_branches = &params.remote_branches.join(sep);
    let tags = &params.tags.join(sep);
    let stash = params.stash.unwrap_or_default();
    let area_width = &params.area_width.to_string();
    let area_height = &params.area_height.to_string();

    s.replace(USER_COMMAND_TARGET_HASH_MARKER, target_hash)
        .replace(USER_COMMAND_MARKED_HASH_MARKER, marked_hash)
        .replace(USER_COMMAND_PICKED_HASHES_MARKER, picked_hashes)
        .replace(USER_COMMAND_FIRST_PARENT_HASH_MARKER, first_parent_hash)
        .replace(USER_COMMAND_PARENT_HASHES_MARKER, parent_hashes)
        .replace(USER_COMMAND_REFS_MARKER, all_refs)
        .replace(USER_COMMAND_BRANCHES_MARKER, branches)
        .replace(USER_COMMAND_REMOTE_BRANCHES_MARKER, remote_branches)
        .replace(USER_COMMAND_TAGS_MARKER, tags)
        .replace(USER_COMMAND_STASH_MARKER, stash)
        .replace(USER_COMMAND_AREA_WIDTH_MARKER, area_width)
        .replace(USER_COMMAND_AREA_HEIGHT_MARKER, area_height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params<'a>(
        command: &'a [String],
        picked_hashes: Vec<&'a str>,
    ) -> ExternalCommandParameters<'a> {
        ExternalCommandParameters {
            command,
            target_hash: "target",
            picked_hashes,
            parent_hashes: vec![],
            all_refs: vec![],
            branches: vec![],
            remote_branches: vec![],
            tags: vec![],
            stash: None,
            area_width: 0,
            area_height: 0,
        }
    }

    #[test]
    fn test_picked_hashes() {
        let command: Vec<String> = [
            "diff",
            "{{marked_hash}}",
            "{{target_hash}}",
            "{{picked_hashes}}",
            "x={{picked_hashes}}",
        ]
        .map(String::from)
        .to_vec();

        // one pick is the marked hash
        let actual = build_user_command(&params(&command, vec!["a"]));
        assert_eq!(actual, vec!["diff", "a", "target", "a", "x=a"]);

        // none: empty, and a standalone list disappears
        let actual = build_user_command(&params(&command, vec![]));
        assert_eq!(actual, vec!["diff", "", "target", "x="]);

        let command: Vec<String> = ["log", "{{picked_hashes}}"].map(String::from).to_vec();
        let actual = build_user_command(&params(&command, vec!["a", "b"]));
        assert_eq!(actual, vec!["log", "a", "b"]);
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

    #[test]
    fn test_validate_marked_hash() {
        let marked: Vec<String> = ["diff", "{{marked_hash}}"].map(String::from).to_vec();
        let picked: Vec<String> = ["log", "{{picked_hashes}}"].map(String::from).to_vec();
        assert!(validate_marked_hash(&marked, 0).is_ok());
        assert!(validate_marked_hash(&marked, 1).is_ok());
        assert!(validate_marked_hash(&marked, 2).is_err());
        assert!(validate_marked_hash(&picked, 2).is_ok());
    }
}
