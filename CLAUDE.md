# spoon

A ratatui git TUI forked from serie. Behaviour follows lazygit (source in `~/projects/lazygit`), the look follows Catppuccin and the owner's neovim setup (`~/.config/nvim`). The general rules are in `~/.claude/CLAUDE.md`; these are spoon's own.

## Shared implementations

Dialogs (`GitDialog`), search (`tree_query`, `SearchOptions`: smart case, fuzzy toggle), file trees (`file_tree`), the diff panel (`diff`, `DiffPane`), picking several items (`Picks`) and the selection bar (`mark_selected_bar`, `mark_commit_list_bar`) each have one implementation. Extend them; don't write a second: copies drift, and the review found every copy had drifted. Crates already chosen: `similar` for diffs, `crokey` for keys, `percent-encoding`, `open`, `fuzzy-matcher`.

## Pitfalls the review found

- **Text from git is untrusted.** Commit messages, file names, stash messages and command output can hold any byte. Parse git output on NUL (`-z`, `%x00`), never on a character a message can contain. Draw tabs as 4 spaces, since ratatui drops control characters. Copy and pass the original text to git.
- **Name refs to git in full** (`refs/heads/x`, `refs/tags/x`) wherever a branch and a tag could share a name. Refuse typed names and URLs starting with `-`. Add to git's environment config; don't replace it (`GIT_CONFIG_COUNT`).
- **A conflict isn't always text.** It can be a symlink, a submodule (gitlink) or a deletion on one side. Never write through a symlink in the work tree.
- **Background results arrive late.** Use a `Job`, which drops a late result because a new job replaces the old one. Otherwise check what the result was made from before using it: the repository `Arc`, the reload generation, the folds. When background work fails, go back to what the screen shows, not to a copy taken when the work started. Panics in threads go through `catching`.
- **Sizes reach zero and the maximum.** Use `saturating_*` on terminal sizes and on config widths. Validate config values that can crash later, like formats and ranges, with garde when the config loads.
- **Scale.** Diffs and lists reach 100k+ rows. Keep no per-row allocations, do no per-row linear search (use a set), and load on a thread. Measure in a release build.
- **Exclusive state is an enum.** Two `Option`s that can't both be set are one enum (`Beside`, `Shows`).
- **Generated files** (`config.schema.json`) have a test comparing them with the code.

## UI and keybinds

- One key per action; a key does nothing, and is not shown, where it does not apply.
- Keys never act on a view other than the focused one, except page-level commands like committing.
- The selection bar, highlights, borders, titles, folder chevrons and colours look the same on every panel.
- Everything hjkl navigates, arrow keys navigate too.

## Tests and checks

- App-level flows go in `src/tests/app.rs`. CI has no git identity or `init.defaultBranch`, and its git creates `origin/HEAD` on fetch.
- Interactive checks run in tmux with `-L spoontest`. Set `VISUAL` as well as `EDITOR`, because the owner's `VISUAL` wins. Run a background race by delaying one side with a temporary probe, and remove the probe afterwards.
- A test for a bug must fail without the fix. Check that once, by reverting the fix or with a probe.
- To check drawing at every size, a temporary test can draw each view from 1×1 to 40×16 and catch the panics.
- Checks: `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo sort-derives --check`, `cargo test`.
- Update README.md and `docs/src` when keys, options or behaviour change. Regenerate the images with `just demo` only when asked.
