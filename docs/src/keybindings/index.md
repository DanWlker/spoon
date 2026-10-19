# Keybindings

You can see the keybindings by pressing the `F1` key. The help, the status bar and the keys popup list the keys of each part of the app from one place, where each part registers them: a key works only where it is listed.
The status bar shows the keys working everywhere, like push, pull and fetch, as many as fit.
<kbd>H</kbd> shows the keys of the focused panel in a popup at the bottom right, and hides it again; while a popup has the focus or text is typed, the status bar shows their keys instead.

The default key bindings can be overridden.

## List of all default keybindings

#### Common

| Key                            | Description | Corresponding keybind |
| ------------------------------ | ----------- | --------------------- |
| <kbd>Ctrl-c</kbd> <kbd>q</kbd> | Quit app    | `force_quit` `quit`   |
| <kbd>F1</kbd>                  | Open help   | `help_toggle`         |
| <kbd>Tab</kbd>                 | Switch between the local changes page and the commit graph | `ref_list` |
| <kbd>1</kbd>                   | Focus the refs sidebar; again to shrink it to its title, and again to grow it back | `focus_panel_1` |
| <kbd>Ctrl-Left</kbd> <kbd>Ctrl-Right</kbd> <kbd>Ctrl-h</kbd> <kbd>Ctrl-l</kbd> | Move the border beside the focused panel: the sidebar's, the one between the files and the diff, or after the commits of the blame and the timeline | `resize_left` `resize_right` |
| <kbd>Ctrl-Up</kbd> <kbd>Ctrl-Down</kbd> <kbd>Ctrl-k</kbd> <kbd>Ctrl-j</kbd> | Move the border above or under the focused panel: the details', the commit message's, or the one between the unstaged and staged files | `resize_up` `resize_down` |
| <kbd>Alt-Left</kbd> <kbd>Alt-Right</kbd> <kbd>Alt-Up</kbd> <kbd>Alt-Down</kbd> <kbd>Alt-h</kbd> <kbd>Alt-l</kbd> <kbd>Alt-k</kbd> <kbd>Alt-j</kbd> | Focus the panel beside, above or under the focused one | `focus_left` `focus_right` `focus_up` `focus_down` |
| <kbd>H</kbd>                   | Show/hide the keys of the focused panel | `keys_toggle` |
| <kbd>@</kbd>                   | Show the command log (see below) | `command_log` |
| <kbd>,</kbd>                   | Dismiss the errors and warnings at the top right (they also go after 10 and 5 seconds) | `dismiss_notifications` |
| <kbd>w</kbd>                   | Wrap the long lines of the diffs and file previews, or not | `toggle_wrap` |
| <kbd>f</kbd>                   | Fetch from a remote (chosen in a popup) | `fetch` |
| <kbd>F</kbd>                   | Fetch all remotes | `fetch_all` |
| <kbd>p</kbd>                   | Pull a branch of a remote (chosen in a popup) | `pull` |
| <kbd>P</kbd>                   | Push a branch to a remote (chosen in a popup) | `push` |

The command log (<kbd>@</kbd>) lists the git commands the app ran that changed the repository, the newest first: whether each succeeded, when it ran, how long it took, and the command. The output of the selected one (its last 200 lines) is under them, the full story of an error shown at the top right. <kbd>1</kbd> and <kbd>2</kbd> focus the commands and the output. With the commands focused, every scroll key (<kbd>j/k</kbd>, <kbd>Ctrl-d/u</kbd>, the page and line keys, <kbd>g/G</kbd>) moves between them, the output shown from the top; with the output focused, they scroll it. <kbd>y</kbd> copies the command and its output, <kbd>Esc</kbd> or <kbd>@</kbd> closes it. Reading commands, like those of the refresh every few seconds, aren't logged; the last 500 commands are kept until the app quits, and a password in a URL is shown as `***`.

The borders between the panels can also be dragged with the mouse. The sizes are kept until the app quits.
With the mouse captured, the terminal selects text while Shift is held.

#### Fetch, pull and push

<kbd>f</kbd>, <kbd>p</kbd> and <kbd>P</kbd> open a popup over any page but the help:

- Fetch: the remote to fetch from.
- Pull: the remote and the branch to pull, chosen among those fetched (only the remotes with fetched branches; with none, it asks to fetch first), whether to rebase instead of merging (`pull.rebase` checks it), and whether to stash and reapply local changes (`--autostash` or `--no-autostash`; checked by `rebase.autoStash` when rebasing, or by default when it is not set, and by `merge.autoStash` when merging, until you change it).
- Push: the local branch, the remote and the branch to push it to, whether to push all tags too, whether to force push (`--force-with-lease`), and whether to skip the pre-push hook (`--no-verify`). A branch without an upstream starts tracking the branch it is pushed to.

In the popup, <kbd>j/k</kbd> move between the fields, <kbd>h/l</kbd> change the selected one (the branch to push to cycles through the branches of the remote), <kbd>v</kbd> toggles a checkbox or goes to the next option of the field (`dialog_toggle`), <kbd>e</kbd> types into a text field, like a new branch to push to (`dialog_edit`) (<kbd>Enter</kbd> or <kbd>Esc</kbd> to finish), and <kbd>Space</kbd> does either, toggling when the field can be toggled (`dialog_activate`); <kbd>Enter</kbd> runs it and <kbd>Esc</kbd> cancels. While a required field (a name, a URL, the branch to push to) is empty, <kbd>Enter</kbd> goes on to type into it instead, also while typing into another field.
The key that opened the popup runs it too, and the popup waits a moment before showing: pressing <kbd>f</kbd> <kbd>f</kbd>, <kbd>p</kbd> <kbd>p</kbd> or <kbd>P</kbd> <kbd>P</kbd> quickly runs the command with its defaults without showing the popup.
The popup shows the git command it will run.

The command runs in the background, the status line shows it with a spinner while it runs (a push or pull also beside its branch in the sidebar, like lazygit), and the app refreshes when it is done.
Git cannot ask for a password or passphrase meanwhile, so use a credential helper or an SSH agent: a prompt fails the command instead.

#### Commit List

| Key                                  | Description                                        | Corresponding keybind                        |
| ------------------------------------ | -------------------------------------------------- | -------------------------------------------- |
| <kbd>Down/Up</kbd> <kbd>j/k</kbd>    | Move down/up                                       | `navigate_down` `navigate_up`                |
| <kbd>J/K</kbd> <kbd>Shift-Down/Up</kbd>                       | Move down/up                                       | `select_down` `select_up`                    |
| <kbd>g/G</kbd>                       | Go to top/bottom                                   | `go_to_top` `go_to_bottom`                   |
| <kbd>Ctrl-f/b</kbd>                  | Scroll page down/up                                | `page_down` `page_up`                        |
| <kbd>Ctrl-d/u</kbd>                  | Jump half page down/up, scrolling as needed       | `half_page_down` `half_page_up`              |
| <kbd>Ctrl-e/y</kbd>                  | Scroll down/up                                     | `scroll_down` `scroll_up`                    |
| <kbd>Left/Right</kbd> <kbd>h/l</kbd> | Fold/Unfold the selected merge commit              | `navigate_left` `navigate_right`             |
| <kbd>Enter</kbd>                     | Show commit details<br>Apply search (if searching) | `confirm`                                    |
| <kbd>3</kbd> <kbd>4</kbd> <kbd>5</kbd> | Show commit details on the info / changes / files tab | `focus_panel_3` `focus_panel_4` `focus_panel_5` |
| <kbd>v</kbd>                         | Select a range of commits, from the first to the selected one; move to extend it, <kbd>v</kbd> again to end it. A new selection adds to the picked ones; <kbd>Esc</kbd> clears them | `toggle_select` |
| <kbd>D</kbd>                         | Diff the checked out commit with the selected (or picked) one, or the two picked ones | `diff` |
| <kbd>r</kbd>                         | Rebase the current branch onto the selected commit | `rebase`                                     |
| <kbd>m</kbd>                         | Merge the selected commit into the current branch  | `merge`                                      |
| <kbd>c</kbd> <kbd>Space</kbd>        | Check out the selected branch or commit            | `checkout`                                   |
| <kbd>C</kbd>                         | Cherry-pick the picked commits, or the selected one | `cherry_pick`                               |
| <kbd>s</kbd>                         | Squash the picked commits, or the selected one, into their parents | `squash`                     |
| <kbd>u</kbd>                         | Revert the picked commits, or the selected one     | `revert`                                     |
| <kbd>U</kbd>                         | Reset the current branch to the selected commit    | `reset`                                      |
| <kbd>A</kbd>                         | Amend the selected commit on the local changes page | `amend_commit`                              |
| <kbd>d</kbd>                         | Drop the picked commits, or the selected one       | `drop_commit`                                |
| <kbd>b</kbd>                         | Create a branch at the selected commit             | `create_branch`                              |
| <kbd>t</kbd>                         | Create a tag at the selected commit                | `create_tag`                                 |
| <kbd>L</kbd>                         | Show only the commits of the current branch, or all again | `branch_only`                         |
| <kbd>o/O</kbd>                       | Open the commit on the web host of its remote; copy its link | `open_in_browser` `copy_link`       |
| <kbd>/</kbd>                         | Start search                                       | `search`                                     |
| <kbd>Esc</kbd>                       | Cancel search<br>Clear picked commits              | `cancel`                                     |
| <kbd>n/N</kbd>                       | Go to next/previous search match                   | `go_to_next` `go_to_previous`                |
| <kbd>Ctrl-t</kbd>                    | Toggle search target                               | `search_target_toggle`                       |
| <kbd>Ctrl-g</kbd>                    | Toggle smart case or case-sensitive, for every search (see below)  | `smart_case_toggle`                         |
| <kbd>Ctrl-x</kbd>                    | Toggle fuzzy match, for every search but of text (see below) | `fuzzy_toggle`                               |
| <kbd>R</kbd>                         | Refresh                                            | `refresh`                                    |

#### Commands on the commit graph

<kbd>D</kbd> shows the files changed between two commits and their diff in the details, like the changes tab: the checked out commit and the selected one (or the picked one), or the two picked commits.
The older commit is the base. It errors with more than two picked commits. <kbd>Esc</kbd> closes it, from the commit list too, and clears the picks.
While a diff is open, of commits or of refs from the sidebar, no other is started: <kbd>D</kbd> and picking are refused until it is closed.
<kbd>Enter</kbd> on the commit list shows the details of the selected commit over the diff; <kbd>Esc</kbd> goes back to the diff, and another <kbd>Esc</kbd> closes it.

While a rebase, merge, cherry-pick or revert is stopped (on conflicts, or to edit a commit), the commands that move or rewrite HEAD wait until it is continued or aborted on the local changes page: rebase, merge, checkout, cherry-pick, squash, revert, amend, drop, reset and pull are refused, and their keys are left out of the hints.

The other commands ask first, in a popup showing what they do and the git commands they run:

- <kbd>r</kbd> rebase: the current branch onto the selected commit, named by its hash (not a branch or tag at it), stashing and reapplying local changes (checked by `rebase.autoStash`, or by default when it is not set).
- <kbd>m</kbd> merge: the selected commit, named by its hash (not a branch or tag at it), into the current branch: fast-forward if possible, always a merge commit, fast-forward only, squash, or don't commit (`merge.ff` picks the default: `false` a merge commit, `only` fast-forward only). Squashing and not committing leave the changes on the local changes page, with the message git prepared.
- <kbd>c</kbd> checkout: a branch at the selected commit (a remote branch becomes a new local branch tracking it) or the commit itself (detached). The local changes are kept (git refuses when they conflict), stashed and reapplied, or discarded (untracked files are kept).
- <kbd>C</kbd> cherry-pick: the picked commits (oldest first), or the selected one, committing them or leaving the changes on the local changes page, optionally appending "(cherry picked from commit …)" to the messages. Merge commits are refused.
- <kbd>s</kbd> squash: each picked commit, or the selected one, into its parent, combining their messages or keeping the parent's (fixup), with an interactive rebase from there, stashing and reapplying local changes like rebase. The commits must be on the current branch, with no merge commits after them.
- <kbd>d</kbd> drop: removes the picked commits, or the selected one, and their changes, with an interactive rebase; the commits after them are rebuilt on top, stashing and reapplying local changes like rebase.
- <kbd>u</kbd> revert: the picked commits (newest first), or the selected one, committing the reverts or leaving them on the local changes page. Merge commits are refused.
- <kbd>U</kbd> reset: the current branch to the selected commit, mixed (keep the changes, unstaged), soft (keep them staged) or hard (discard them).
- <kbd>A</kbd> amend: edits the selected commit on the local changes page, where amending is turned on: its changes are staged, combined with what is staged, and its message is in the message box, both to change. For the last commit it opens right away; an older one asks first, then an interactive rebase stops at it (`edit`), stashing and reapplying the local changes like rebase. Committing (<kbd>c</kbd>) amends it and goes on with the rebase; <kbd>C</kbd> aborts. The commits after it must be on the current branch, with no merge commits.

- <kbd>b</kbd> branch: a new branch at the selected commit, checking it out or not.
- <kbd>t</kbd> tag: a new tag at the selected commit, annotated when it has a message.

<kbd>L</kbd> shows only the history of `HEAD` (the commits of the current branch, and of the branches merged into it), and again all commits; the title of the commit list says so ("main only").
The commits that are not on the current branch are dimmed, and the branches and tags at a commit are shown as labels before its subject, the checked-out branch standing out. A branch whose upstream is at the same commit shows one label, with a cloud (󰅠) before its name.
A merge commit is drawn as a ring, larger than a commit's dot, with a chevron in it pointing to the side the merged line comes in from. <kbd>h</kbd> folds it: the commits only it brought in are hidden (those a branch, tag or other commit still reaches stay) and its chevron points down; <kbd>l</kbd> unfolds it. Merges stay folded across reloads.

Drop rewrites the commits after the ones dropped too, so like squash and amend they must be on the current branch, with no merge commits after them.
Rebase, merge, checkout and reset work on the selected commit only, so they error while commits are picked.
The commands run in the background like fetch, pull and push. When one stops on conflicts, or leaves changes to commit, the local changes page is shown to resolve and commit them.

These keys are bound for the commit graph only: the local changes page uses <kbd>d</kbd> <kbd>c</kbd> <kbd>C</kbd> <kbd>s</kbd> for its own commands. While the commit details are open, they work with the commit list focused (<kbd>2</kbd>), not from the tabs of the details.
| <kbd>y/Y</kbd>                       | Copy the selected commits' short/full hashes, one per line | `short_copy` `full_copy` |

#### Commit Detail

| Key                                  | Description                     | Corresponding keybind           |
| ------------------------------------ | ------------------------------- | ------------------------------- |
| <kbd>Esc</kbd>  | Clear the picked commits, then the search and filter, then close commit details | `cancel` |
| <kbd>Down/Up</kbd> <kbd>j/k</kbd>    | Scroll down/up                  | `navigate_down` `navigate_up`   |
| <kbd>Ctrl-f/b</kbd>                  | Select the files page down/up; scroll the diff or preview page down/up while it has the focus; jump a page while selecting hunks or lines | `page_down` `page_up`           |
| <kbd>Ctrl-d/u</kbd>                  | Select the files half a page down/up; jump half a page while selecting hunks or lines | `half_page_down` `half_page_up` |
| <kbd>g/G</kbd>                       | Go to top/bottom                | `go_to_top` `go_to_bottom`      |
| <kbd>J/K</kbd> <kbd>Shift-Down/Up</kbd>                       | Select older/newer commit       | `select_down` `select_up`       |
| <kbd>R</kbd>                         | Refresh                         | `refresh`                       |
| <kbd>v</kbd>                         | Select the files of the changes/files tree from the first to the selected one; another selection adds, <kbd>Esc</kbd> clears | `toggle_select` |
| <kbd>y/Y</kbd>                       | Copy the picked commits' short/full hashes, or the current one; in the changes and files tabs, the relative/absolute paths of the selected files, one per line | `short_copy` `full_copy` |
| <kbd>i</kbd>                         | Add the selected files and directories to `.gitignore`, in the changes and files tabs | `ignore` |
| <kbd>2</kbd>                         | Focus the commit list: moving the selection shows the details of each commit, <kbd>Enter</kbd> goes back to the details | `focus_panel_2` |
| <kbd>3</kbd> <kbd>4</kbd> <kbd>5</kbd> | Show info / changes / files tab | `focus_panel_3` `focus_panel_4` `focus_panel_5` |
| <kbd>]</kbd> <kbd>[</kbd>            | Show the next/previous tab, around from files to info, while a tab has the focus | `next_tab` `previous_tab` |
| <kbd>Right/Left</kbd> <kbd>l/h</kbd> | Expand/Collapse directory (changes and files tabs) | `navigate_right` `navigate_left` |
| <kbd>/</kbd> <kbd>?</kbd>            | Search/Filter files (changes and files tabs); search the diff while it has the focus | `search` `filter` |
| <kbd>Space</kbd>                     | Apply the selected hunk or line of the diff to the working tree (changes tab) | `apply_change` |
| <kbd>u</kbd>                         | Reset the selected files to how they were at the commit or before it, in the working tree and the index, after confirming (changes tab) | `reset_file` |
| <kbd>o/O</kbd>                       | Open the commit, or the selected files at it, on the web host (with several files, each on its default remote); from the diff or the file, at the selected hunk or line; copy their links | `open_in_browser` `copy_link` |
| <kbd>n/N</kbd>                       | Go to next/previous search match | `go_to_next` `go_to_previous` |
| <kbd>B</kbd>                         | Blame the selected file (see below), from the selected line of its diff or preview; only with one file selected | `blame` |
| <kbd>T</kbd>                         | Show the commits changing the selected file (see below); only with one file selected | `timeline` |
| <kbd>+</kbd> <kbd>-</kbd>            | Show more/fewer lines around the changes, in the diff of one file | `increase_diff_context` `decrease_diff_context` |

The commit detail has three tabs:

- **Info**: the commit information. <kbd>j/k</kbd> scroll it.
- **Changes**: the changed files as a tree under a `/` row standing for all of them, like on the local changes page, expanded by default, with the diff of the selected file (or directory, or `/` for the whole commit) beside it. <kbd>j/k</kbd> and the scroll keys select a file; <kbd>v</kbd> selects a range of files; the diff is scrolled with them only while it has the focus.
- **Files**: all files of the commit as a tree, collapsed by default, with a preview of the selected file beside it. <kbd>j/k</kbd> and the scroll keys select a file; <kbd>v</kbd> selects a range of files; the preview is scrolled with them only while it has the focus. A submodule says its commits are on the Changes tab instead of previewing it as a file.

In the files tab, <kbd>Enter</kbd> moves the focus into the preview of the selected file (on a directory, it expands or collapses it instead of closing the details): <kbd>j/k</kbd> select a line, <kbd>v</kbd> selects the lines from the first to the selected one (<kbd>y</kbd> copies them; another selection adds to them), <kbd>/</kbd> searches the file (<kbd>n/N</kbd> next/previous match), and <kbd>Esc</kbd> clears the selection, then goes back to the files.

In the changes tab (and a diff of two commits), <kbd>Enter</kbd> moves the focus into the diff, like on the local changes page: <kbd>j/k</kbd> select hunks, <kbd>Enter</kbd> then selects the changed lines of the hunk, and <kbd>Esc</kbd> goes back to the hunks, then to the files (<kbd>h/l</kbd> expand and collapse directories there). In the diff:

- <kbd>/</kbd> searches its text (ignoring case unless the query has an uppercase letter), selecting the changed line matched, else its hunk; <kbd>n/N</kbd> go to the next/previous match.
- <kbd>y</kbd> copies the selected line as in the file, or the selected hunk as a diff.
- <kbd>Space</kbd> applies the selected hunk or line to the working tree (`git apply`), when it applies there; the count of the local changes in the page bar follows.
- <kbd>+</kbd> and <kbd>-</kbd> show more or fewer lines around the changes, when the diff is of one file.

While the diff or the preview has the focus, only its own keys work there: the keys of the files beside it (<kbd>Y</kbd>, <kbd>?</kbd>, <kbd>h/l</kbd>, <kbd>J/K</kbd> to the next commit) do nothing. In the preview, <kbd>v</kbd> selects lines and <kbd>y</kbd> copies them. The same goes for the diff of the local changes page, where <kbd>/</kbd> searches the diff and <kbd>y</kbd> copies the line or hunk too, and <kbd>v</kbd>, <kbd>?</kbd>, <kbd>Y</kbd>, <kbd>h/l</kbd> do nothing. The commands of the page still work, like committing or the commands of the commit graph.

#### Searching and filtering files

The file trees of the local changes page and of the changes and files tabs can be searched and filtered by path:

- <kbd>/</kbd> searches: the selection jumps to the first match as you type, expanding the directories hiding it, and matching names are highlighted. <kbd>n/N</kbd> go to the next/previous match.
- <kbd>?</kbd> filters: only matching files are shown, with their directories expanded.

<kbd>Enter</kbd> keeps the query, <kbd>Esc</kbd> while typing clears it. Afterwards, the panel title shows the active search and filter, and <kbd>Esc</kbd> clears the search, then the filter.
Matching ignores case unless the query has an uppercase letter.

Every search shares its options with the commit list's: while typing any of them, <kbd>Ctrl-g</kbd> toggles smart case and <kbd>Ctrl-x</kbd> fuzzy matching, and the status line shows the options that apply. Names, like paths and the commits of the blame and the timeline, match fuzzily or as a substring; text, like diffs, file previews and the blame's text, always matches exactly, so <kbd>Ctrl-x</kbd> does nothing there.
Like the file trees', the title of the diffs, previews, the blame and the timeline shows the active search; the commit list's search instead shows its matches in the status line.
On the local changes page, the unstaged and staged files each have their own search and filter, applying to the focused section only, and staging, unstaging and discarding a directory only affect the files shown.

#### Resolving conflicts

Conflicted files are marked `U`. With the cursor on one, the diff is replaced by its conflicts: the existing side (ours) and the incoming side (theirs) of each conflict, side by side, with two tools below them.
<kbd>Enter</kbd> moves into it, <kbd>Left/Right</kbd> choose a side and <kbd>Up/Down</kbd> a tool, then <kbd>Enter</kbd>:

- on a side, resolves the file by keeping that side entirely (removing the file if that side deleted it), and stages it.
- on `Edit in merge tool`, runs `git mergetool` for the file, with the tool of [`core.git.merge_tool`](../configurations/config-file-format.md) or else git's `merge.tool` config.
- on `Open in editor`, opens the file in `$VISUAL` or `$EDITOR` (like <kbd>e</kbd>), to edit the conflict markers by hand; stage it with <kbd>Space</kbd> afterwards.

The app is suspended while the editor or merge tool runs, and refreshed after.

#### Changes made outside the app

Like lazygit, the app picks up changes made elsewhere, in an editor or another terminal:

- every 10 seconds, the local changes are reloaded: the local changes page keeps its selection, and elsewhere the count in the page bar follows.
- every 2 seconds, it checks where the local branches and `HEAD` point, and whether a rebase, merge, cherry-pick or revert is in progress; when that changed (a commit, a checkout, or a merge stopped on conflicts in another terminal), everything is reloaded, like <kbd>R</kbd>.
- when the terminal gets the focus back, both are checked right away.

Nothing is reloaded while a git command runs, a popup is open, or something is being typed or confirmed; a check skipped meanwhile runs again as soon as that ends.

Everything the app shows, its keys included, is the repository as last loaded. A command runs on what you chose from it, without checking git again first: a change made outside since, before the next check, is not caught, so a popup that says "Reset main" resets whatever is checked out by then.

#### Picking files

On the local changes page, <kbd>v</kbd> starts selecting files and directories: moving extends the selection (highlighted, with the count in the panel title), and <kbd>v</kbd> again ends it, keeping them picked. Another selection adds to them; <kbd>Esc</kbd> clears them.
A file git diffs as text converted from it (a `textconv` diff driver in `.gitattributes`, like for `.docx`) shows that text, but its hunks and lines can't be selected, since the text isn't in the file: stage, discard or stash the whole file.

<kbd>i</kbd> adds the selected files and directories to the `.gitignore` at the top of the repository, anchored there (`/build/`, `/notes.txt`), with the characters git reads as a pattern escaped. It works here and in the changes and files tabs of the commit details. A file git already tracks stays tracked; ignoring only keeps untracked files out, which the message says.

While files are picked, <kbd>Space</kbd>, <kbd>d</kbd> and <kbd>s</kbd> apply to them instead of the row under the cursor; <kbd>Enter</kbd> opens the diff of the row under the cursor only.
<kbd>e</kbd> opens the picked files only, and <kbd>B</kbd>/<kbd>T</kbd> work only when exactly one file is picked: a picked directory is not expanded into the files under it, and a submodule is not opened as a file.
<kbd>s</kbd> opens a popup asking for a stash message (it may be empty), and stashes including untracked files.

#### Refs Sidebar

| Key                                                  | Description      | Corresponding keybind            |
| ---------------------------------------------------- | ---------------- | -------------------------------- |
| <kbd>Esc</kbd>                  | Back to the page | `cancel`                 |
| <kbd>Enter</kbd>                                     | Open/Close node, else back to the page | `confirm`              |
| <kbd>2</kbd> - <kbd>5</kbd>                          | Focus a panel of the page | `focus_panel_2` ... `focus_panel_5` |
| <kbd>Down/Up</kbd> <kbd>j/k</kbd>                  | Move down/up     | `navigate_down` `navigate_up`    |
| <kbd>J/K</kbd> <kbd>Shift-Down/Up</kbd>                                     | Move down/up     | `select_down` `select_up`        |
| <kbd>g/G</kbd>                                     | Go to top/bottom | `go_to_top` `go_to_bottom`       |
| <kbd>Ctrl-d/u</kbd>                                | Jump half page down/up, scrolling as needed | `half_page_down` `half_page_up` |
| <kbd>Ctrl-f/b</kbd>                                | Scroll a page down/up | `page_down` `page_up`           |
| <kbd>Ctrl-e/y</kbd>                                | Scroll down/up   | `scroll_down` `scroll_up`        |
| <kbd>Right/Left</kbd> <kbd>l/h</kbd>               | Open/Close node  | `navigate_right` `navigate_left` |
| <kbd>]/[</kbd>                                     | Go to the next/previous section (branches, remotes, tags, stashes), opening it | `next_section` `previous_section` |
| <kbd>R</kbd>                                       | Refresh          | `refresh`                        |
| <kbd>y</kbd>                                       | Copy the selected refs' names, one per line | `short_copy` |
| <kbd>o/O</kbd>                                     | Open the branch (or a new pull request from it, chosen in the popup), the remote branch, tag or remote on the web host; copy its link | `open_in_browser` `copy_link` |
| <kbd>a</kbd>                                       | New remote (on a remote or the remotes header) | `create_ref` |
| <kbd>b</kbd>                                       | New branch from the selected branch, remote branch or tag | `create_branch` |
| <kbd>t</kbd>                                       | New tag at the selected branch, remote branch or tag | `create_tag` |
| <kbd>d</kbd>                                       | Delete the branch or tag, drop the stash; remove the remote | `delete_ref` |
| <kbd>e</kbd>                                       | Rename the branch or change its upstream; edit the remote | `edit_ref`   |
| <kbd>Space</kbd>                                   | Apply the stash; on a branch, remote branch or tag, check it out | `apply_stash` |
| <kbd>v</kbd>                                       | Select refs from the first to the selected one (only refs of one kind together); another selection adds to them, <kbd>Esc</kbd> clears them | `toggle_select` |
| <kbd>D</kbd>                                       | Diff the two picked refs, or the current branch with the picked or selected one | `diff` |
| <kbd>L</kbd>                                       | Show only the commits of the selected local branch (else the current one), or all | `branch_only` |

The commands on the refs ask first, in a popup:

- <kbd>b</kbd>: a new branch from the selected branch, remote branch or tag (nothing on the other rows), checking it out or not. The start can be changed to one of the local branches with <kbd>h/l</kbd>, not typed; from the commit list (<kbd>b</kbd> there) it is the selected commit.
- <kbd>a</kbd> on a remote (like `origin`, under the remotes), or on the remotes header: a new remote, with its name and URL, fetched once added.
- <kbd>t</kbd>: a new tag at the selected branch, remote branch or tag (nothing on the other rows), annotated when it has a message. It goes where the cursor is, in the sidebar as in the commit list.
- <kbd>v</kbd> selects refs, moving to extend the selection and <kbd>v</kbd> again to end it; another selection adds to them, and <kbd>Esc</kbd> clears them. All picked refs are of one kind: branches, remote branches, tags or stashes; moving to another section stops the selection with a warning. The title of the sidebar shows how many are picked. <kbd>d</kbd> then deletes (or drops) all of them in one popup, and <kbd>D</kbd> diffs the two picked refs.
- <kbd>D</kbd> diffs two refs: the two picked ones (the first picked is the base), else the current branch and the picked or selected one. The changes between them open in the details, like <kbd>D</kbd> on the graph, and the commit graph shows only the history of the two (its title says which); closing the comparison shows all of it again.
- <kbd>d</kbd> deletes a local branch (warning when it isn't merged into `HEAD` or its upstream; a checkbox deletes its upstream on the remote too, when it is still there), a remote branch (on the remote, with `git push --delete`), or a tag (optionally from a remote too, `origin` or else the first one, named in the popup); and drops a stash. On a remote (like `origin`, under the remotes) it removes the remote, with its remote branches here; nothing changes on the remote itself.
- <kbd>e</kbd> on a local branch renames it, or changes its upstream (chosen among the remote branches with <kbd>h/l</kbd>, or None). On a remote (like `origin`, under the remotes) it renames it or changes its URL.
- the commands of the commit graph work on the selected branch, remote branch or tag: <kbd>m</kbd> merges it into the current branch and <kbd>r</kbd> rebases the current branch onto it (by its name), <kbd>c</kbd> checks it out (a tag detached); <kbd>b</kbd> and <kbd>t</kbd> make a branch or tag from it; the others, like <kbd>D</kbd> diff, work on the commit it points at. The keys of the sidebar come first (<kbd>d</kbd> deletes the ref, it doesn't drop the commit).
- <kbd>L</kbd> on a local branch shows only its commits in the graph, and on it again all of them; on other rows, only those of the current branch, like on the graph.
- <kbd>Space</kbd> on a stash applies it, keeping it in the stash list, or pops it (applies, then drops it; git keeps it when it stops on conflicts), optionally restoring what was staged as staged (`--index`); the local changes page shows the result.

The refs are always shown on the left. <kbd>1</kbd> focuses them, and the page shows what is selected:
`Local Changes` shows the local changes page, a branch, remote branch, tag or stash shows the commit graph at it.
<kbd>Tab</kbd> switches between the local changes page and the commit graph, from anywhere, keeping the local changes page as it was (the commit message being written, the selected file). The bar at the top shows the page open and the count of local changes; the sidebar is on the commit graph only.

In the commit graph, <kbd>2</kbd> focuses the commit list and <kbd>3</kbd> <kbd>4</kbd> <kbd>5</kbd> open the details of the selected commit on the info / changes / files tab.
Coming back to the commit graph from the local changes page (<kbd>Tab</kbd>), the details open again as they were, with the focus on the commit list.
While the details are open, <kbd>2</kbd> moves the focus to the commit list above, where moving the selection shows the details of each commit, and <kbd>3</kbd> - <kbd>5</kbd> or <kbd>Enter</kbd> go back to the details; <kbd>Esc</kbd> there clears the picked commits, but doesn't close the details.

#### Local Changes

| Key                                  | Description                                                  | Corresponding keybind           |
| ------------------------------------ | ------------------------------------------------------------ | ------------------------------- |
| <kbd>Esc</kbd>  | Back from lines to hunks, from hunks to files<br>Back from the commit section to files<br>Clear the picked files, then the search, then the filter | `cancel` |
| <kbd>1</kbd> <kbd>2</kbd>            | Focus unstaged/staged files                                  | `focus_panel_1` `focus_panel_2` |
| <kbd>3</kbd>                         | Focus the commit message, typing it when it is empty          | `focus_panel_3`                 |
| <kbd>Enter</kbd>                     | Select hunks in the diff<br>Select lines in the hunk<br>Commit, like <kbd>c</kbd>, in the commit section or on an empty unstaged section<br>Go to the unstaged section from an empty staged one | `confirm`    |
| <kbd>c</kbd>                         | Commit (or amend), finishing a merge, cherry-pick or revert, after confirming; or continue a rebase | `commit` |
| <kbd>e</kbd>                         | Open the selected files in `$VISUAL` or `$EDITOR`<br>Type the message, in the commit section | `edit`  |
| <kbd>A</kbd>                         | Toggle amending the last commit                              | `toggle_amend`                  |
| <kbd>C</kbd>                         | Abort the rebase, merge, cherry-pick or revert in progress (asks first) | `abort`              |
| <kbd>Space</kbd>                     | Stage/Unstage file, directory, hunk or line (or the picked files)<br>Type the message, in the commit section | `toggle_stage` |
| <kbd>v</kbd>                         | Select a range of files or directories, from the first to the selected one; move to extend it, <kbd>v</kbd> again to end it. A new selection adds to the picked ones; <kbd>Esc</kbd> clears them | `toggle_select` |
| <kbd>]f</kbd> <kbd>[f</kbd>          | Go to next/previous file, staying in the diff                | `next_file` `previous_file`     |
| <kbd>s</kbd>                         | Stash all changes, or the picked files (asks for a message)  | `stash`                         |
| <kbd>d</kbd>                         | Discard unstaged file, directory, hunk or line (asks first)  | `discard`                       |
| <kbd>Right/Left</kbd> <kbd>l/h</kbd> | Expand/Collapse directory                                    | `navigate_right` `navigate_left` |
| <kbd>/</kbd>                         | Search files<br>Search the diff, while it has the focus      | `search`                        |
| <kbd>?</kbd>                         | Filter files                                                 | `filter`                        |
| <kbd>y/Y</kbd>                       | Copy the relative/absolute paths of the selected files, one per line<br>Copy the line or hunk (<kbd>y</kbd>), in the diff | `short_copy` `full_copy` |
| <kbd>+</kbd> <kbd>-</kbd>            | Show more/fewer lines around the changes, in the diff of one file | `increase_diff_context` `decrease_diff_context` |
| <kbd>i</kbd>                         | Add the file or directory to `.gitignore` (see below)        | `ignore`                        |
| <kbd>B</kbd>                         | Blame the file as of the last commit (see below); the local changes aren't blamed, and only one file may be picked | `blame`     |
| <kbd>T</kbd>                         | Show the commits changing the file (see below); only one file may be picked | `timeline`                      |
| <kbd>n/N</kbd>                       | Go to next/previous search match                             | `go_to_next` `go_to_previous`   |
| <kbd>Down/Up</kbd> <kbd>j/k</kbd>    | Select next/previous file, hunk or line                      | `navigate_down` `navigate_up`   |
| <kbd>J/K</kbd> <kbd>Shift-Down/Up</kbd>                       | Select next/previous file, hunk or line                      | `select_down` `select_up`       |
| <kbd>g/G</kbd>                       | Select first/last file, hunk or line                         | `go_to_top` `go_to_bottom`      |
| <kbd>Ctrl-f/b</kbd>                  | Scroll page down/up; jump a page while selecting hunks or lines | `page_down` `page_up`           |
| <kbd>Ctrl-d/u</kbd>                  | Select the files half a page down/up; jump half a page while selecting hunks or lines | `half_page_down` `half_page_up` |
| <kbd>Ctrl-e/y</kbd>                  | Scroll down/up                                                | `scroll_down` `scroll_up`       |
| <kbd>R</kbd>                         | Refresh                                                      | `refresh`                       |

While selecting hunks or lines the page keys move the selection to the next or previous hunk or changed line from the row half a page (or a page) on; a hunk taller than the page is read a page at a time and hands over once the page passes its end. From the files, every scroll key moves the files, like the other lists; the diff, the commit message or a submodule panel is scrolled with them only while it has the focus.

Selecting a submodule shows the panel beside the files in place of the gitlink diff: how many files are uncommitted inside it, and the commits between the two gitlinks the section compares — the index and the working tree when unstaged, the base commit and the index when staged — drawn with the graph like the commit list. <kbd>Enter</kbd> focuses it, so the scroll keys move its commits; <kbd>Esc</kbd> goes back to the files. A commit that moves a submodule's gitlink shows the same list, between the gitlink it recorded and the previous one, on the changes tab of the commit details. The parent repository can only stage the gitlink, so those commits and files are made in the submodule itself. A submodule that isn't checked out says so.

<kbd>c</kbd> (or <kbd>Enter</kbd> in the commit section, or on an empty unstaged section) shows a popup with the commit to make: the branch, the message and the staged files; <kbd>Enter</kbd> or <kbd>c</kbd> again commits, <kbd>Esc</kbd> cancels. When the message is empty, it starts typing it instead.
While a rebase is stopped (on a conflict or to edit a commit), <kbd>c</kbd> continues it instead, keeping the messages of the rebased commits.
While a merge, cherry-pick or revert is stopped on conflicts, the message box is filled with the message git prepared, the files that conflicted listed in the description, and <kbd>c</kbd> commits to finish it (going on with the next commits of a cherry-pick or revert of several commits).
Resolve the conflicts and stage the files with <kbd>Space</kbd> first. <kbd>C</kbd> aborts any of them, and amending is not available meanwhile.
The commit section title shows when amending, rebasing, merging, cherry-picking or reverting.
The message has a one-line summary and a description under it. Focusing the commit section (<kbd>3</kbd>, or moving down from the diff) starts typing when the message is empty; in the commit section, <kbd>j</kbd>/<kbd>k</kbd> pick the summary or the description and <kbd>Space</kbd> or <kbd>e</kbd> start typing in it. While typing, <kbd>Enter</kbd> in the summary goes on to the description, <kbd>Tab</kbd> switches between them and <kbd>Esc</kbd> stops. The keys of the file list, like search, filter, picking and discarding, do nothing in the commit section.
While editing the message, all keys are typed into it (<kbd>Enter</kbd> starts a new line in the description) until <kbd>Esc</kbd>, with Emacs-like editing keys: <kbd>Ctrl-u</kbd> undoes and <kbd>Ctrl-r</kbd> redoes, <kbd>Alt-b</kbd>/<kbd>Alt-f</kbd> or <kbd>Ctrl-Left</kbd>/<kbd>Ctrl-Right</kbd> move by word, <kbd>Ctrl-w</kbd> deletes the word before the cursor, <kbd>Ctrl-k</kbd> the rest of the line. Long lines wrap at words on screen; the message keeps them as typed.

<kbd>A</kbd> turns amending on: the staged files show the changes of the last commit combined with what is already staged, the message box shows its message, and <kbd>c</kbd> amends it.
Unstaging while amending removes changes from the commit being amended. Turning it off restores the previous message, and so does HEAD moving meanwhile, by a checkout or a commit made elsewhere.

#### Blame

<kbd>B</kbd> on a file shows who last changed each line of it, in a window over everything, named in the page bar with the file: at the top, **[1] Commit**, the commit it is shown as of (the last one changing the file, up to the commit of the details) with its date and subject; below, **[2] Blame**, the commit, author and date of each line beside its text, given on the first line of each run of lines from the same commit.

| Key                                  | Description                                                  | Corresponding keybind           |
| ------------------------------------ | ------------------------------------------------------------ | ------------------------------- |
| <kbd>Esc</kbd>  | Clear the search, the selected lines, then back to the commit shown, then close | `cancel` |
| <kbd>1</kbd> <kbd>2</kbd> <kbd>Alt-Up/Down</kbd> | Focus the commit shown/the lines (<kbd>k</kbd> on the first line goes up too, <kbd>j</kbd> down) | `focus_panel_1` `focus_panel_2` `focus_up` `focus_down` |
| <kbd>Alt-Left/Right</kbd>            | Focus the commits/the text of the lines                     | `focus_left` `focus_right`      |
| <kbd>Left/Right</kbd> <kbd>h/l</kbd> | Act on the commit or the text of the selected line<br>Blame as of the older/newer commit, on the commit shown, like <kbd>[</kbd> <kbd>]</kbd> | `navigate_left` `navigate_right` |
| <kbd>[</kbd> <kbd>]</kbd>            | Blame as of the older/newer commit changing the file, renames followed, on the same line | `older_commit` `newer_commit` |
| <kbd>Enter</kbd>                     | Select the commit of the line on the graph, closing the blame | `confirm`                      |
| <kbd>c</kbd> <kbd>Space</kbd>        | Check out the commit, after confirming, closing the blame    | `checkout`                      |
| <kbd>y/Y</kbd>                       | Copy the commit short/full hash<br>Copy the line, or the selected lines (<kbd>y</kbd>), on its text | `short_copy` `full_copy` |
| <kbd>v</kbd>                         | Select the lines from the first to the selected one, on the text; another selection adds to them, <kbd>Esc</kbd> clears them | `toggle_select` |
| <kbd>o/O</kbd>                       | Open the commit, or the file at the line, on the web host; copy its link | `open_in_browser` `copy_link` |
| <kbd>/</kbd> <kbd>n/N</kbd>          | Search the lines' commits (by hash, author or date, from one run of lines to the next) or the text, whichever has the focus; go to next/previous match | `search` `go_to_next` `go_to_previous` |
| <kbd>H</kbd>                         | Show/hide all the keys working where the focus is            | `keys_toggle`                   |

The commit keys act on the commit at the top while it has the focus, and on the commit of the selected line in the left column; in the text, <kbd>y</kbd> and <kbd>o/O</kbd> act on the line, the other keys of the file preview work too. The commit selected with <kbd>Enter</kbd> may not be in the graph, hidden by <kbd>L</kbd> or a folded merge: a warning says so.

#### Timeline

<kbd>T</kbd> on a file shows the commits changing it, renames followed, in a window over everything, named in the page bar with the file: **[1] Commits**, the newest first, on the last one changing it up to the commit of the details; beside them, **[2] Diff**, the file's changes in the selected commit.

| Key                                  | Description                                                  | Corresponding keybind           |
| ------------------------------------ | ------------------------------------------------------------ | ------------------------------- |
| <kbd>Esc</kbd>  | Clear the search, back out of the lines and hunks, then close | `cancel`               |
| <kbd>1</kbd> <kbd>2</kbd> <kbd>Alt-Left/Right</kbd> | Focus the commits/the diff                    | `focus_panel_1` `focus_panel_2` `focus_left` `focus_right` |
| <kbd>Right/Left</kbd> <kbd>l/h</kbd> | Into the diff (its hunks)/back out of it                     | `navigate_right` `navigate_left` |
| <kbd>[</kbd> <kbd>]</kbd>            | Select the older/newer commit, from the diff too, staying in it | `older_commit` `newer_commit` |
| <kbd>Enter</kbd>                     | Select the commit on the graph, closing the timeline<br>In the diff, select the lines of the hunk | `confirm` |
| <kbd>c</kbd> <kbd>Space</kbd>        | Check out the commit, after confirming, closing the timeline | `checkout`                      |
| <kbd>y/Y</kbd>                       | Copy the commit short/full hash<br>Copy the hunk or line (<kbd>y</kbd>), in the diff | `short_copy` `full_copy` |
| <kbd>+</kbd> <kbd>-</kbd>            | Show more/fewer lines around the changes | `increase_diff_context` `decrease_diff_context` |
| <kbd>o/O</kbd>                       | Open the commit, or from the diff the file at the selected lines, on the web host; copy its link | `open_in_browser` `copy_link` |
| <kbd>/</kbd> <kbd>n/N</kbd>          | Search the commits (by hash, date or subject), or the diff while it has the focus; go to next/previous match | `search` `go_to_next` `go_to_previous` |
| <kbd>H</kbd>                         | Show/hide all the keys working where the focus is            | `keys_toggle`                   |

In both windows, <kbd>,</kbd> dismisses the notices, <kbd>w</kbd> wraps long lines and <kbd>Ctrl-Left/Right</kbd> move the border after the commits, as everywhere. Every scroll key moves the commits. A commit whose diff has no hunk, like a rename, puts the focus back on the commits.

#### Help

| Key                                                            | Description              | Corresponding keybind           |
| -------------------------------------------------------------- | ------------------------ | ------------------------------- |
| <kbd>Esc</kbd> <kbd>F1</kbd>              | Close help               | `cancel` `help_toggle`  |
| <kbd>Down/Up</kbd> <kbd>j/k</kbd>                              | Scroll down/up           | `navigate_down` `navigate_up`   |
| <kbd>J/K</kbd> <kbd>Shift-Down/Up</kbd>                                                 | Scroll down/up           | `select_down` `select_up`       |
| <kbd>Ctrl-f/b</kbd>                                            | Scroll page down/up      | `page_down` `page_up`           |
| <kbd>Ctrl-d/u</kbd>                                            | Scroll half page down/up | `half_page_down` `half_page_up` |
| <kbd>g/G</kbd>                                                 | Go to top/bottom         | `go_to_top` `go_to_bottom`      |

</details>

----

- [Custom Keybindings](./custom-keybindings.md)
