# spoon

A ratatui git TUI forked from serie. Behaviour follows lazygit (source in `~/projects/lazygit`), the look follows Catppuccin and the owner's neovim setup (`~/.config/nvim`). The general rules are in `~/.claude/CLAUDE.md`; these are spoon's own.

## Shared implementations

Dialogs (`GitDialog`), search (`tree_query`, `SearchOptions`: smart case, fuzzy toggle), file trees (`file_tree`) and the diff panel (`diff`) each have one implementation. Extend them; don't write a second. Crates already chosen: `similar` for diffs, `crokey` for keys, `percent-encoding`, `open`, `fuzzy-matcher`.

## UI and keybinds

- One key per action; a key does nothing, and is not shown, where it does not apply.
- Keys never act on a view other than the focused one, except page-level commands like committing.
- The selection bar, highlights, borders, titles, folder chevrons and colours look the same on every panel.
- Everything hjkl navigates, arrow keys navigate too.

## Tests and checks

- App-level flows go in `src/tests/app.rs`. CI has no git identity or `init.defaultBranch`, and its git creates `origin/HEAD` on fetch.
- Interactive checks run in tmux with `-L spoontest`.
- Checks: `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo sort-derives --check`, `cargo test`.
- Update README.md and `docs/src` when keys, options or behaviour change. Regenerate the images with `just demo` only when asked.
