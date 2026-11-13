# Keybindings

<kbd>?</kbd> opens the keys list: the actions of the focused panel first, which <kbd>Enter</kbd> runs, then those of the other panels, dimmed and named by their panel, to read. <kbd>/</kbd> filters it by what the actions do, so an action can be found without remembering its key, and actions with no key, like ignoring a file, are run from there. The keys list, the status bar and the keys popup list the keys of each part of the app from one place, where each part registers them: a key works only where it is listed.
Some keys are sequences, pressed one after another, like <kbd>y</kbd> <kbd>y</kbd>: after the first key a popup lists the keys finishing it there and what they do.
[The keymap](./keymap.html) shows every default key on one page, grouped by where it works; [the panels](./panels.html) show where each panel is and where the focus keys go; [coming from lazygit](./lazygit.html) lists its keys beside spoon's.
The status bar shows the keys working everywhere, like push, pull and fetch, as many as fit.
<kbd>H</kbd> shows the keys of the focused panel in a popup at the bottom right, and hides it again; while a popup has the focus or text is typed, the status bar shows their keys instead.

The default key bindings can be overridden.

## List of all default keybindings

#### Common

| Key                            | Description | Corresponding keybind |
| ------------------------------ | ----------- | --------------------- |
| <kbd>Ctrl-c</kbd> <kbd>q</kbd> | Quit app, waiting for the running git command; <kbd>Ctrl-c</kbd> also while typing | `force_quit` `quit`   |
| <kbd>?</kbd>                   | Open the keys list: search the actions of the focused panel and run one | `help_toggle` |
| <kbd>Tab</kbd>                 | Switch between the local changes page and the commit graph | `switch_page` |
| <kbd>1</kbd>                   | Focus the refs sidebar; again to shrink it to its title, and again to grow it back | `focus_panel_1` |
| <kbd>Alt-Left</kbd> <kbd>Alt-Right</kbd> <kbd>Alt-h</kbd> <kbd>Alt-l</kbd> | Move the border beside the focused panel: the sidebar's, the one between the files and the diff, or after the commits of the blame and the timeline | `resize_left` `resize_right` |
| <kbd>Alt-Up</kbd> <kbd>Alt-Down</kbd> <kbd>Alt-k</kbd> <kbd>Alt-j</kbd> | Move the border above or under the focused panel: the details', the commit message's, or the one between the unstaged and staged files | `resize_up` `resize_down` |
| <kbd>Ctrl-Left</kbd> <kbd>Ctrl-Right</kbd> <kbd>Ctrl-Up</kbd> <kbd>Ctrl-Down</kbd> <kbd>Ctrl-h</kbd> <kbd>Ctrl-l</kbd> <kbd>Ctrl-k</kbd> <kbd>Ctrl-j</kbd> | Focus the panel beside, above or under the focused one | `focus_left` `focus_right` `focus_up` `focus_down` |
| <kbd>H</kbd>                   | Show/hide the keys of the focused panel | `keys_toggle` |
| <kbd>@</kbd>                   | Show the command log (see below) | `command_log` |
| <kbd>y</kbd> <kbd>y</kbd> / <kbd>f</kbd> / <kbd>l</kbd> | Copy the short form (a short hash, a relative path, a name), the full one, or the link on the web host of what is selected; in a diff or the lines of a file, <kbd>y</kbd> <kbd>y</kbd> copies the selected lines, <kbd>y</kbd> <kbd>f</kbd> the file's absolute path and <kbd>y</kbd> <kbd>l</kbd> their link | `short_copy` `full_copy` `copy_link` |
| <kbd>o</kbd>                   | Open what is selected on the web host of its remote | `open_in_browser` |
| <kbd>,</kbd>                   | Dismiss the errors and warnings at the top right (they also go after 10 and 5 seconds) | `dismiss_notifications` |
| <kbd>w</kbd>                   | Wrap the long lines of the diffs and file previews, or not | `toggle_wrap` |
| <kbd>f</kbd>                   | Fetch from a remote (chosen in a popup) | `fetch` |
| <kbd>F</kbd>                   | Fetch all remotes | `fetch_all` |
| <kbd>p</kbd>                   | Pull a branch of a remote (chosen in a popup) | `pull` |
| <kbd>P</kbd>                   | Push a branch to a remote (chosen in a popup) | `push` |

The command log (<kbd>@</kbd>) lists the git commands the app ran that changed the repository, the newest first: whether each succeeded, when it ran, how long it took, and the command. The output of the selected one (its last 200 lines) is under them, the full story of an error shown at the top right. <kbd>1</kbd> and <kbd>2</kbd> focus the commands and the output. With the commands focused, every scroll key (<kbd>j/k</kbd>, <kbd>Ctrl-d/u</kbd>, the page and line keys, <kbd>g/G</kbd>) moves between them, the output shown from the top; with the output focused, they scroll it. <kbd>y</kbd> <kbd>y</kbd> copies the command and its output, <kbd>Esc</kbd> or <kbd>@</kbd> closes it. Reading commands, like those of the refresh every few seconds, aren't logged; the last 500 commands are kept until the app quits, and a password in a URL is shown as `***`.

#### Mouse

The mouse acts as the keys do, on the row under it, in the panel it focuses first:

- A click selects the row and does what <kbd>Enter</kbd> does there, but the focus stays on the panel clicked: a commit shows its details below the list, a file its diff or preview beside it, a ref its commit on the graph (<kbd>Enter</kbd>, or a click there, goes into them), and a folder of the sidebar opens or closes. On empty space under the rows, or on a panel's border or the padding inside it, a click only focuses the panel; on a border that drags, a press released without moving it. Where <kbd>Enter</kbd> would leave a window, on the commits of the blame and the timeline (shown on the graph) and the actions of the keys list (run), a click only selects, and a double click does what <kbd>Enter</kbd> does.
- A double click does what <kbd>Space</kbd> does on the row then under the pointer (but in those windows, <kbd>Enter</kbd>) (the first click may have moved the rows, opening the details); on a row not picked, it clears the picked ones first, so it acts on that row, those of the other panels too, as a right click does. A double click on <kbd>[x]</kbd>, <kbd>[Commit]</kbd> or the amend checkbox, or one whose first click closes a popup or the menu, acts once: its second click does nothing, rather than act on what shows there then. While a popup or the menu is open, borders don't move.
- Dragging over rows picks them, as <kbd>v</kbd> on the first, moving to the last, and <kbd>v</kbd> again (in a diff, going into its lines first; a drag picks only in the panel it started in); <kbd>Ctrl</kbd>+click picks or unpicks one row, as <kbd>v</kbd> <kbd>v</kbd>. Some terminals take <kbd>Ctrl</kbd>+click for opening links; the keys still pick there.
- A right click selects the row, clearing the picked ones when it isn't one of them: anywhere off them, on empty space, a diff's file header or another panel (but the commits or refs a diff open is of, which go with it) and opens a menu beside the pointer with the actions working on it, as the status bar lists them, but for moving about, closing, searching and picking, what a click does already (opening a folder of the sidebar, going into a diff or a preview, which a click on it does), the commands of the whole page (commit, amend, continue, abort) and the context of the diffs, which the keys keep. Commands git would refuse there are left out of it and of the status bar, their keys still saying why: no checkout, merge or rebase onto the commit or branch checked out, no squash, drop, amend or revert of a commit off its history or of a merge commit, no cherry-pick of one on it; on a stash, applying, diffing, the details and copying the hash only. In the menu of a commit, active branch only does what <kbd>L</kbd> does, and this branch only shows the history of the branch at that commit (a local one, else a remote one; or all of it again): listed on a commit with a branch, saying to choose in the sidebar when there are several. It is bound to no key (`this_branch_only`). In the menu of a branch or remote branch in the sidebar, this branch only shows that branch's commits; active branch only, about no row, is left out of the sidebar's menu, <kbd>L</kbd> still working there. The menu's actions start with a capital. An action bound to no key is in the menu all the same, without a key. The menu splits its actions into sections by a rule: showing (details, diff, blame, edit), git commands, the branch only filters, then copying and the web host. In the menu: <kbd>j/k</kbd> or the pointer select one, <kbd>Enter</kbd> or a click runs it as its key, <kbd>Esc</kbd> or a click outside closes the menu. On empty space it shows the actions on the panel's selection.
- A click on a tab of the details, or on <kbd>Local Changes</kbd> or <kbd>Commit Graph</kbd> in the bar at the top, switches to it.
- In the local changes, a double click stages or unstages a file (or a hunk or line in its diff); on a side of a conflict, it keeps that side. A click on a box of the commit message types in it, also while typing in the other box; while typing, a click elsewhere stops typing, keeping the message, and acts there as it would otherwise, focusing what it is on; <kbd>[Commit]</kbd> on its bottom border commits as <kbd>c</kbd> does, dimmed while there is nothing to commit; the amend checkbox at its bottom left is checked while amending, and a click toggles it as <kbd>A</kbd> does, dimmed where amending can't be turned on or off; a click on a submodule's commits focuses them, which the keys then move through.
- The wheel focuses the panel under the pointer and scrolls it, two rows a notch, leaving the selection where it is, even off the rows shown; the next move of the selection shows it again.
- In a popup, a click selects a field: on a checkbox it toggles it, on a chevron (‹ or ›) it goes to the previous or next option, on a text field it types there, the caret where clicked, ending the typing in another field first. <kbd>[Cancel]</kbd> and <kbd>[Confirm]</kbd> on its bottom border close it or run it, as <kbd>Esc</kbd> and <kbd>Enter</kbd>; Confirm is dimmed and does nothing while the popup can't run, like with a required field empty. Elsewhere the mouse does nothing while a popup is open.
- <kbd>[x]</kbd> at the top right of the commits shows all of them again while they are a branch's only; at the top right of the details, a window (the blame, the timeline, the command log, the keys list), the keys popup or a popup closes it.
- In the menu of a branch in the sidebar, fetch and push open their popup set to it (its remote, or the branch to push), pull is there on the checked-out branch only; on a remote branch or a remote, fetch fetches that remote. The keys do the same with the sidebar on them.
- While typing a search, a click only ends it, as <kbd>Esc</kbd>.

The borders between the panels can also be dragged with the mouse. The sizes are kept until the app quits.
With the mouse captured, the terminal selects text while Shift is held.

#### Fetch, pull and push

<kbd>f</kbd>, <kbd>p</kbd> and <kbd>P</kbd> open a popup over any page but the help:

- Fetch: the remote to fetch from.
- Pull: the remote and the branch to pull, chosen among those fetched (only the remotes with fetched branches; with none, it asks to fetch first), whether to rebase instead of merging (`pull.rebase` checks it), and whether to stash and reapply local changes (`--autostash` or `--no-autostash`; checked by `rebase.autoStash` when rebasing, or by default when it is not set, and by `merge.autoStash` when merging, until you change it).
- Push: the local branch, the remote and the branch to push it to, whether to push all tags too, whether to force push (`--force-with-lease`), and whether to skip the pre-push hook (`--no-verify`). A branch without an upstream starts tracking the branch it is pushed to.

In the popup, <kbd>j/k</kbd> move between the fields, <kbd>h/l</kbd> change the selected one (the branch to push to cycles through the branches of the remote), <kbd>Space</kbd> toggles a checkbox or goes to the next option of the field, else types into a text field (`select`), <kbd>e</kbd> types into a text field that also has options, like a new branch to push to (`dialog_edit`) (<kbd>Enter</kbd> or <kbd>Esc</kbd> to finish); <kbd>Enter</kbd> runs it and <kbd>Esc</kbd> cancels. While a required field (a name, a URL, the branch to push to) is empty, <kbd>Enter</kbd> goes on to type into it instead, also while typing into another field.
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
| <kbd>Ctrl-e/y</kbd>                  | Scroll down/up, the selection coming along once it would leave the view, like vim | `scroll_down` `scroll_up`                    |
| <kbd>Left/Right</kbd> <kbd>h/l</kbd> | Fold/Unfold the selected merge commit              | `navigate_left` `navigate_right`             |
| <kbd>Enter</kbd>                     | Show commit details<br>Apply search (if searching) | `confirm`                                    |
| <kbd>3</kbd> <kbd>4</kbd> <kbd>5</kbd> | Show commit details on the info / changes / files tab | `focus_panel_3` `focus_panel_4` `focus_panel_5` |
| <kbd>v</kbd>                         | Select a range of commits, from the first to the selected one; move to extend it (moving back unpicks what it went over; <kbd>v</kbd> <kbd>v</kbd> on a picked one unpicks it), <kbd>v</kbd> again to end it. A new selection adds to the picked ones; <kbd>Esc</kbd> clears them | `toggle_select` |
| <kbd>D</kbd>                         | Diff the checked out commit with the selected (or picked) one, or the two picked ones | `diff` |
| <kbd>r</kbd>                         | Rebase the current branch onto the selected commit | `rebase`                                     |
| <kbd>m</kbd>                         | Merge the selected commit into the current branch  | `merge`                                      |
| <kbd>Space</kbd>                     | Check out the selected branch or commit; apply a stash | `select` (`checkout`, no key)            |
| <kbd>C</kbd>                         | Cherry-pick the picked commits, or the selected one | `cherry_pick`                               |
| <kbd>s</kbd>                         | Squash the picked commits, or the selected one, into their parents | `squash`                     |
| <kbd>u</kbd>                         | Revert the picked commits, or the selected one     | `revert`                                     |
| <kbd>U</kbd>                         | Reset the current branch to the selected commit    | `reset`                                      |
| <kbd>A</kbd>                         | Amend the selected commit on the local changes page | `amend_commit`                              |
| <kbd>d</kbd>                         | Drop the picked commits, or the selected one       | `drop_commit`                                |
| <kbd>b</kbd>                         | Create a branch at the selected commit             | `create_branch`                              |
| <kbd>t</kbd>                         | Create a tag at the selected commit                | `create_tag`                                 |
| <kbd>L</kbd>                         | Show only the commits of the current branch, or all again | `active_branch_only`                         |
| (none)                               | Show only the commits of the branch at the selected commit (in the sidebar, of the selected branch), or all again | `this_branch_only` |
| <kbd>o</kbd> <kbd>y</kbd><kbd>l</kbd> | Open the commit on the web host of its remote; copy its link | `open_in_browser` `copy_link`       |
| <kbd>y</kbd><kbd>y</kbd> <kbd>y</kbd><kbd>f</kbd> | Copy the short/full hashes of the picked commits, or the selected one, one per line | `short_copy` `full_copy` |
| <kbd>/</kbd>                         | Start search                                       | `search`                                     |
| <kbd>Esc</kbd>                       | Clear picked commits, then the search, then show all the commits again after <kbd>L</kbd> | `cancel` |
| <kbd>n/N</kbd>                       | Go to next/previous search match                   | `go_to_next` `go_to_previous`                |
| <kbd>Ctrl-t</kbd>                    | Toggle search target                               | `search_target_toggle`                       |
| <kbd>Ctrl-g</kbd>                    | Toggle smart case or case-sensitive, for every search (see below)  | `smart_case_toggle`                         |
| <kbd>Ctrl-x</kbd>                    | Toggle fuzzy match, for every search but of text (see below) | `fuzzy_toggle`                               |
| <kbd>Ctrl-r</kbd>                    | Refresh                                            | `refresh`                                    |

#### Commands on the commit graph

<kbd>D</kbd> shows the files changed between two commits and their diff in the details, like the changes tab: the checked out commit and the selected one (or the picked one), or the two picked commits.
The older commit is the base. It errors with more than two picked commits. <kbd>Esc</kbd> closes it, from the commit list too, and clears the picks.
While a diff is open, of commits or of refs from the sidebar, no other is started: <kbd>D</kbd> and picking are refused until it is closed. Showing a branch's commits only (<kbd>L</kbd>) is refused too, but showing all of them again is not: with the commit list focused, <kbd>L</kbd> or <kbd>Esc</kbd> does it, the diff staying open, before <kbd>Esc</kbd> closes it.
<kbd>Enter</kbd> on the commit list shows the details of the selected commit over the diff; <kbd>Esc</kbd> goes back to the diff, and another <kbd>Esc</kbd> closes it.

While a rebase, merge, cherry-pick or revert is stopped (on conflicts, or to edit a commit), the commands that move or rewrite HEAD wait until it is continued or aborted on the local changes page: rebase, merge, checkout, cherry-pick, squash, revert, amend, drop, reset and pull are refused, and their keys are left out of the hints.

The other commands ask first, in a popup showing what they do and the git commands they run:

- <kbd>r</kbd> rebase: the current branch onto the selected commit, named by its hash (not a branch or tag at it), stashing and reapplying local changes (checked by `rebase.autoStash`, or by default when it is not set).
- <kbd>m</kbd> merge: the selected commit, named by its hash (not a branch or tag at it), into the current branch: fast-forward if possible, always a merge commit, fast-forward only, squash, or don't commit (`merge.ff` picks the default: `false` a merge commit, `only` fast-forward only). Squashing and not committing leave the changes on the local changes page, with the message git prepared.
- <kbd>Space</kbd> checkout: a branch at the selected commit (a remote branch becomes a new local branch tracking it) or the commit itself (detached). The local changes are kept (git refuses when they conflict), stashed and reapplied, or discarded (untracked files are kept). On a stash with no branch or tag at it, <kbd>Space</kbd> applies the stash instead, as in the sidebar.
- <kbd>C</kbd> cherry-pick: the picked commits (oldest first), or the selected one, committing them or leaving the changes on the local changes page, optionally appending "(cherry picked from commit …)" to the messages. Merge commits are refused.
- <kbd>s</kbd> squash: each picked commit, or the selected one, into its parent, combining their messages or keeping the parent's (fixup), with an interactive rebase from there, stashing and reapplying local changes like rebase. The commits must be on the current branch, with no merge commits after them.
- <kbd>d</kbd> drop: removes the picked commits, or the selected one, and their changes, with an interactive rebase; the commits after them are rebuilt on top, stashing and reapplying local changes like rebase.
- <kbd>u</kbd> revert: the picked commits (newest first), or the selected one, committing the reverts or leaving them on the local changes page. Merge commits are refused.
- <kbd>U</kbd> reset: the current branch to the selected commit, mixed (keep the changes, unstaged), soft (keep them staged) or hard (discard them).
- <kbd>A</kbd> amend: edits the selected commit on the local changes page, where amending is turned on: its changes are staged, combined with what is staged, and its message is in the message box, both to change. For the last commit it opens right away; an older one asks first, then an interactive rebase stops at it (`edit`), stashing and reapplying the local changes like rebase. Committing (<kbd>c</kbd>) amends it and goes on with the rebase; <kbd>m</kbd> <kbd>a</kbd> aborts. The commits after it must be on the current branch, with no merge commits.

- <kbd>b</kbd> branch: a new branch at the selected commit, checking it out or not.
- <kbd>t</kbd> tag: a new tag at the selected commit, annotated when it has a message.

<kbd>L</kbd> shows only the history of `HEAD` (the commits of the current branch, and of the branches merged into it), and again all commits (<kbd>Esc</kbd> too, after clearing the picked commits and the search, as the <kbd>[x]</kbd> on the list; also with the commit list focused beside the details or a diff, which stay open); the title of the commit list says so ("main only"). On a branch without commits yet, like an orphan one, there is nothing to show and it does nothing.
The commits that are not on the current branch are dimmed, and the branches and tags at a commit are shown as labels before its subject, the checked-out branch standing out. A branch whose upstream is at the same commit shows one label, with a plugged cable () before its name.
A merge commit is drawn as a ring, larger than a commit's dot, with a chevron in it pointing to the side the merged line comes in from. <kbd>h</kbd> folds it: the commits it brought in are hidden, even those a branch or tag points at (a commit still shown, like the checked-out one or a later commit of the branch, keeps what it reaches), and its chevron points down; <kbd>l</kbd> unfolds it. <kbd>Enter</kbd> on such a branch or tag in the refs sidebar unfolds every folded merge that brought its commit in, even when unfolding one would show it, and selects it. Merges stay folded across reloads.

Drop rewrites the commits after the ones dropped too, so like squash and amend they must be on the current branch, with no merge commits after them.
Rebase, merge, checkout and reset work on the selected commit only, so they error while commits are picked.
The commands run in the background like fetch, pull and push. When one stops on conflicts, or leaves changes to commit, the local changes page is shown to resolve and commit them. The popups of merge, rebase, cherry-pick, revert and drop warn of the first conflict the command would stop at, looked for in the background once the popup opens: "Checking for conflicts…" shows until it is found.

These keys are bound for the commit graph only: the local changes page uses <kbd>d</kbd> <kbd>s</kbd> <kbd>m</kbd> for its own commands. While the commit details are open, they work with the commit list focused (<kbd>2</kbd>), not from the tabs of the details.

#### Commit Detail

| Key                                  | Description                     | Corresponding keybind           |
| ------------------------------------ | ------------------------------- | ------------------------------- |
| <kbd>Esc</kbd>  | Clear the picked commits, then the search or filter, then close commit details | `cancel` |
| <kbd>Down/Up</kbd> <kbd>j/k</kbd>    | Scroll down/up                  | `navigate_down` `navigate_up`   |
| <kbd>Ctrl-f/b</kbd>                  | Select the files page down/up; scroll the diff or preview page down/up while it has the focus; jump a page while selecting hunks or lines | `page_down` `page_up`           |
| <kbd>Ctrl-d/u</kbd>                  | Select the files half a page down/up; jump half a page while selecting hunks or lines | `half_page_down` `half_page_up` |
| <kbd>g/G</kbd>                       | Go to top/bottom                | `go_to_top` `go_to_bottom`      |
| <kbd>J/K</kbd> <kbd>Shift-Down/Up</kbd>                       | Select older/newer commit       | `select_down` `select_up`       |
| <kbd>Ctrl-r</kbd>                    | Refresh                         | `refresh`                       |
| <kbd>v</kbd>                         | Select the files of the changes/files tree from the first to the selected one (moving back unpicks what it went over; <kbd>v</kbd> <kbd>v</kbd> on a picked one unpicks it); another selection adds, <kbd>Esc</kbd> clears | `toggle_select` |
| <kbd>y</kbd><kbd>y</kbd> <kbd>y</kbd><kbd>f</kbd> | Copy the picked commits' short/full hashes, or the current one; in the changes and files tabs, the relative/absolute paths of the selected files, one per line | `short_copy` `full_copy` |
| <kbd>i</kbd>                         | Add the selected files and directories to `.gitignore`, in the changes and files tabs | `ignore` |
| <kbd>2</kbd>                         | Focus the commit list: moving the selection shows the details of each commit, <kbd>Enter</kbd> goes back to the details | `focus_panel_2` |
| <kbd>3</kbd> <kbd>4</kbd> <kbd>5</kbd> | Show info / changes / files tab | `focus_panel_3` `focus_panel_4` `focus_panel_5` |
| <kbd>]</kbd> <kbd>[</kbd>            | Show the next/previous tab, around from files to info, while a tab has the focus | `next_tab` `previous_tab` |
| <kbd>Right/Left</kbd> <kbd>l/h</kbd> | Expand/Collapse directory (changes and files tabs) | `navigate_right` `navigate_left` |
| <kbd>/</kbd>                         | Filter files (changes and files tabs); search the diff or preview while it has the focus | `search` |
| <kbd>Space</kbd>                     | Apply the selected hunk or line of the diff to the working tree (changes tab) | `select` |
| <kbd>u</kbd>                         | Reset the selected files to how they were at the commit or before it, in the working tree and the index, after confirming (changes tab) | `reset_file` |
| <kbd>o</kbd> <kbd>y</kbd><kbd>l</kbd> | Open the commit, or the selected files at it, on the web host (with several files, each on its default remote); from the diff or the file, at the selected hunk, line or lines (<kbd>y</kbd><kbd>l</kbd> copies the link there); copy their links | `open_in_browser` `copy_link` |
| <kbd>n/N</kbd>                       | Go to next/previous search match, in the diff or preview | `go_to_next` `go_to_previous` |
| <kbd>B</kbd>                         | Blame the selected file (see below), from the selected line of its diff or preview; only with one file selected | `blame` |
| <kbd>T</kbd>                         | Show the commits changing the selected file (see below); only with one file selected | `timeline` |
| <kbd>e</kbd>                         | Open the selected files as they are in the working tree in `$VISUAL` or `$EDITOR`, in the changes and files tabs, from the diff or preview too | `edit` |
| <kbd>+</kbd> <kbd>-</kbd>            | Show more/fewer lines around the changes, in the diff of one file | `increase_diff_context` `decrease_diff_context` |

The commit detail has three tabs:

- **Info**: the commit information. <kbd>j/k</kbd> scroll it.
- **Changes**: the changed files as a tree under a `/` row standing for all of them, like on the local changes page, expanded by default, with the diff of the selected file (or directory, or `/` for the whole commit) beside it. <kbd>j/k</kbd> and the scroll keys select a file; <kbd>v</kbd> selects a range of files; the diff is scrolled with them only while it has the focus.
- **Files**: all files of the commit as a tree, collapsed by default, with a preview of the selected file beside it. <kbd>j/k</kbd> and the scroll keys select a file; <kbd>v</kbd> selects a range of files; the preview is scrolled with them only while it has the focus. A submodule says its commits are on the Changes tab instead of previewing it as a file.

In the files tab, <kbd>Enter</kbd> moves the focus into the preview of the selected file (on a directory, it expands or collapses it instead of closing the details): <kbd>j/k</kbd> select a line, <kbd>v</kbd> selects the lines from the first to the selected one (<kbd>y</kbd> <kbd>y</kbd> copies them; another selection adds to them), <kbd>/</kbd> searches the file (<kbd>n/N</kbd> next/previous match), and <kbd>Esc</kbd> clears the selection, then goes back to the files.

In the changes tab (and a diff of two commits), <kbd>Enter</kbd> moves the focus into the diff, like on the local changes page: <kbd>j/k</kbd> select hunks, <kbd>Enter</kbd> then selects the changed lines of the hunk, and <kbd>Esc</kbd> goes back to the hunks, then to the files (<kbd>h/l</kbd> expand and collapse directories there). In the diff:

- <kbd>/</kbd> searches its text (ignoring case unless the query has an uppercase letter), selecting the changed line matched, else its hunk; <kbd>n/N</kbd> go to the next/previous match.
- <kbd>v</kbd>, while selecting lines, selects the lines from the selected one, like in the file preview: moving extends the selection, <kbd>v</kbd> again ends it keeping them selected, another selection adds to them, and <kbd>Esc</kbd> clears them. While lines are selected, the selection stays in their hunk.
- <kbd>y</kbd> <kbd>y</kbd> copies the selected line or lines as in the file (without their `+`/`-`), or the selected hunk as a diff; <kbd>y</kbd> <kbd>f</kbd> copies the file's absolute path, and <kbd>y</kbd> <kbd>l</kbd> the link of the file at those lines, as everywhere.
- <kbd>Space</kbd> applies the selected hunk, line or lines to the working tree (`git apply`), when it applies there; the count of the local changes in the page bar follows.
- <kbd>+</kbd> and <kbd>-</kbd> show more or fewer lines around the changes, when the diff is of one file; at least one, which git needs to stage or apply a hunk.

While the diff or the preview has the focus, only its own keys work there: the keys of the files beside it (copying paths, filtering, <kbd>h/l</kbd>, <kbd>J/K</kbd> to the next commit) do nothing. In the preview, <kbd>v</kbd> selects lines and <kbd>y</kbd> <kbd>y</kbd> copies them. The same goes for the diff of the local changes page, where <kbd>/</kbd> searches the diff, <kbd>v</kbd> selects lines and <kbd>y</kbd> <kbd>y</kbd> copies the lines or hunk too (<kbd>Space</kbd> and <kbd>d</kbd> stage and discard the lines selected), and the keys of the files, like <kbd>h/l</kbd>, do nothing. The commands of the page still work, like committing or the commands of the commit graph.

#### Filtering files

The file trees of the local changes page and of the changes and files tabs are filtered by path with <kbd>/</kbd>, like lazygit's: only matching files are shown, with their directories expanded, and the matching letters of the names are highlighted. The diff or preview beside them is searched with <kbd>/</kbd> while it has the focus, matching the text as it is in the file (a tab, drawn as spaces, is matched as a tab). The lines picked stay picked, unless the match is in another hunk of the diff.

<kbd>Enter</kbd> keeps the query, <kbd>Esc</kbd> while typing clears it. Text pasted into a query or a popup's field is typed on one line, its newlines and tabs as spaces; pasted where nothing is typed, it does nothing. Afterwards, the panel title shows the active filter, and <kbd>Esc</kbd> clears it.
Matching ignores case unless the query has an uppercase letter.

Every search shares its options with the commit list's: while typing any of them, <kbd>Ctrl-g</kbd> toggles smart case and <kbd>Ctrl-x</kbd> fuzzy matching, and the status line shows the options that apply. Names, like paths and the commits of the blame and the timeline, match fuzzily or as a substring; text, like diffs, file previews and the blame's text, always matches exactly, so <kbd>Ctrl-x</kbd> does nothing there.
Like the file trees', the title of the diffs, previews, the blame and the timeline shows the active search, and the status line says how it went once kept: the match it is on of how many, or that nothing matched.
A search goes from where it started while the query is typed, and <kbd>n/N</kbd> go on from the selection. A match is shown as it is: a search moves the selection and scrolls the list only when the match isn't shown already, keeping the `scrolloff` rows around it like the movement keys do.
On the local changes page, the unstaged and staged files each have their own filter, applying to the focused section only, and staging, unstaging and discarding a directory only affect the files shown.

#### Resolving conflicts

Conflicted files are marked `U`. With the cursor on one, the diff is replaced by its conflicts, side by side, each with 3 lines of the file around it and the line numbers of the file in the working tree, with two tools below them. The sides are named for what they are in the operation stopped on the conflict, as git's "ours" and "theirs" swap in a rebase:

| Operation   | Ours (left)    | Theirs (right) |
| ----------- | -------------- | -------------- |
| merge       | Current branch | Merging in     |
| rebase      | Rebasing onto  | Your commit    |
| cherry-pick | Current        | Picking        |
| revert      | Current        | Reverting      |

With the `diff3` or `zdiff3` conflict style (`git config merge.conflictStyle zdiff3`), the common ancestor is shown between them as **Base**, and the lines of each side the ancestor doesn't have, what that side changed, are highlighted. A line under the sides says what <kbd>Space</kbd> does on the selected side or tool.
<kbd>Enter</kbd> moves into it, <kbd>Left/Right</kbd> choose a side and <kbd>Up/Down</kbd> a tool, then <kbd>Space</kbd> (`select`):

- on a side, resolves the file by keeping that side entirely (removing the file if that side deleted it, and checking a submodule out at that side's commit), and stages it.
- on `Edit in merge tool`, runs `git mergetool` for the file, with the tool of [`core.git.merge_tool`](../configurations/config-file-format.md) or else git's `merge.tool` config.
- on `Open in editor`, opens the file in `$VISUAL` or `$EDITOR` (like <kbd>e</kbd>), to edit the conflict markers by hand.

The app is suspended while the editor or merge tool runs, and refreshed after. Like lazygit, a file whose conflict markers are all gone is staged when the files are loaded again, unless both sides aren't text (a binary file, a symlink, a submodule or a side deleting it), which is resolved by keeping a side.

A conflicted file is never staged with <kbd>Space</kbd>, which would stage its markers: on the file it moves into the conflicts like <kbd>Enter</kbd>, and on a directory or picked files with a conflicted file among them it is refused.

#### Changes made outside the app

Like lazygit, the app picks up changes made elsewhere, in an editor or another terminal:

- every 10 seconds, the local changes are reloaded: the local changes page keeps its selection, and elsewhere the count in the page bar follows.
- every 2 seconds, it checks where the local branches and `HEAD` point, and whether a rebase, merge, cherry-pick or revert is in progress; when that changed (a commit, a checkout, or a merge stopped on conflicts in another terminal), everything is reloaded, like <kbd>Ctrl-r</kbd>.
- when the terminal gets the focus back, both are checked right away.

Nothing is reloaded while a git command runs, a popup is open, or something is being typed or confirmed; a check skipped meanwhile runs again as soon as that ends.

Everything the app shows, its keys included, is the repository as last loaded. A command runs on what you chose from it, without checking git again first: a change made outside since, before the next check, is not caught, so a popup that says "Reset main" resets whatever is checked out by then.

#### Picking files

On the local changes page, <kbd>v</kbd> starts selecting files and directories: moving extends the selection (highlighted, with the count in the panel title), and <kbd>v</kbd> again ends it, keeping them picked (<kbd>v</kbd> <kbd>v</kbd> on a picked row unpicks it). Another selection adds to them; <kbd>Esc</kbd> clears them.
A file git diffs as text converted from it (a `textconv` diff driver in `.gitattributes`, like for `.docx`) shows that text, but its hunks and lines can't be selected, since the text isn't in the file: stage, discard or stash the whole file. File names and lines that aren't UTF-8 show `�` for those bytes, and are staged, discarded and stashed as they are.

Ignoring (<kbd>i</kbd>) adds the selected files and directories to the `.gitignore` at the top of the repository, anchored there (`/build/`, `/notes.txt`), with the characters git reads as a pattern escaped. It works here and in the changes and files tabs of the commit details. A file git already tracks stays tracked; ignoring only keeps untracked files out, which the message says.

While files are picked, <kbd>Space</kbd>, <kbd>d</kbd> and <kbd>s</kbd> apply to them instead of the row under the cursor; <kbd>Enter</kbd> opens the diff of the row under the cursor only.
<kbd>e</kbd> opens the picked files only, and <kbd>B</kbd>/<kbd>T</kbd> work only when exactly one file is picked: a picked directory is not expanded into the files under it, and a submodule is not opened as a file.
<kbd>s</kbd> opens a popup asking for a stash message (it may be empty), and stashes including untracked files.

#### Refs Sidebar

| Key                                                  | Description      | Corresponding keybind            |
| ---------------------------------------------------- | ---------------- | -------------------------------- |
| <kbd>Esc</kbd>                  | Clear the picked refs, then the search, else back to the page | `cancel` |
| <kbd>/</kbd>                                       | Search the refs by the name shown (a stash by its message), in closed folders and sections too, opening them; <kbd>Ctrl-g</kbd> and <kbd>Ctrl-x</kbd> toggle smart case and fuzzy while typing | `search` |
| <kbd>n/N</kbd>                                     | Go to the next/previous match | `go_to_next` `go_to_previous` |
| <kbd>Enter</kbd>                                     | Open/Close node, else back to the page at the selected ref, unfolding the folded merges that brought its commit in | `confirm` |
| <kbd>2</kbd> - <kbd>5</kbd>                          | Focus a panel of the page | `focus_panel_2` ... `focus_panel_5` |
| <kbd>Down/Up</kbd> <kbd>j/k</kbd>                  | Move down/up     | `navigate_down` `navigate_up`    |
| <kbd>J/K</kbd> <kbd>Shift-Down/Up</kbd>                                     | Move down/up     | `select_down` `select_up`        |
| <kbd>g/G</kbd>                                     | Go to top/bottom | `go_to_top` `go_to_bottom`       |
| <kbd>Ctrl-d/u</kbd>                                | Jump half page down/up, scrolling as needed | `half_page_down` `half_page_up` |
| <kbd>Ctrl-f/b</kbd>                                | Scroll a page down/up | `page_down` `page_up`           |
| <kbd>Ctrl-e/y</kbd>                                | Scroll down/up, the selection coming along once it would leave the view, like vim | `scroll_down` `scroll_up`        |
| <kbd>Right/Left</kbd> <kbd>l/h</kbd>               | Open/Close node  | `navigate_right` `navigate_left` |
| <kbd>]/[</kbd>                                     | Go to the next/previous section (branches, remotes, tags, stashes), opening it | `next_section` `previous_section` |
| <kbd>Ctrl-r</kbd>                                  | Refresh          | `refresh`                        |
| <kbd>y</kbd><kbd>y</kbd>                           | Copy the selected refs' names, one per line | `short_copy` |
| <kbd>o</kbd> <kbd>y</kbd><kbd>l</kbd>              | Open the branch (or a new pull request from it, chosen in the popup), the remote branch, tag or remote on the web host; copy its link | `open_in_browser` `copy_link` |
| <kbd>a</kbd>                                       | New remote (on a remote or the remotes header) | `create_ref` |
| <kbd>b</kbd>                                       | New branch from the selected branch, remote branch or tag | `create_branch` |
| <kbd>t</kbd>                                       | New tag at the selected branch, remote branch or tag | `create_tag` |
| <kbd>d</kbd>                                       | Delete the branch or tag, drop the stash; remove the remote | `delete_ref` |
| <kbd>e</kbd>                                       | Rename the branch or change its upstream; edit the remote | `edit_ref`   |
| <kbd>Space</kbd>                                   | Apply the stash; on a branch, remote branch or tag, check it out | `select` |
| <kbd>v</kbd>                                       | Select refs from the first to the selected one (only refs of one kind together; moving back unpicks what it went over; <kbd>v</kbd> <kbd>v</kbd> on a picked one unpicks it); another selection adds to them, <kbd>Esc</kbd> clears them | `toggle_select` |
| <kbd>D</kbd>                                       | Diff the two picked refs, or the current branch with the picked or selected one | `diff` |
| <kbd>L</kbd>                                       | Show only the commits of the current branch, or all, on any row | `active_branch_only` |
| (none)                                             | Show only the commits of the selected branch or remote branch, or all again | `this_branch_only` |

The commands on the refs ask first, in a popup:

- <kbd>b</kbd>: a new branch from the selected branch, remote branch or tag (nothing on the other rows), checking it out or not. The start can be changed to one of the local branches with <kbd>h/l</kbd>, not typed; from the commit list (<kbd>b</kbd> there) it is the selected commit.
- <kbd>a</kbd> on a remote (like `origin`, under the remotes), or on the remotes header: a new remote, with its name and URL, fetched once added.
- <kbd>t</kbd>: a new tag at the selected branch, remote branch or tag (nothing on the other rows), annotated when it has a message. It goes where the cursor is, in the sidebar as in the commit list.
- <kbd>v</kbd> selects refs, moving to extend the selection and <kbd>v</kbd> again to end it; another selection adds to them, and <kbd>Esc</kbd> clears them. All picked refs are of one kind: branches, remote branches, tags or stashes; moving to another section stops the selection with a warning. The title of the sidebar shows how many are picked. <kbd>d</kbd> then deletes (or drops) all of them in one popup, and <kbd>D</kbd> diffs the two picked refs.
- <kbd>D</kbd> diffs two refs: the two picked ones (the first picked is the base), else the current branch and the picked or selected one. The changes between them open in the details, like <kbd>D</kbd> on the graph, and the commit graph shows only the history of the two (its title says which); closing the comparison shows all of it again.
- <kbd>d</kbd> deletes a local branch (warning when it isn't merged into `HEAD` or its upstream; a checkbox deletes its upstream on the remote too, when it is still there), a remote branch (on the remote, with `git push --delete`), or a tag (optionally from a remote too, `origin` or else the first one, named in the popup); and drops a stash. On a remote (like `origin`, under the remotes) it removes the remote, with its remote branches here; nothing changes on the remote itself.
- <kbd>e</kbd> on a local branch renames it, or changes its upstream (chosen among the remote branches with <kbd>h/l</kbd>, or None). On a remote (like `origin`, under the remotes) it renames it or changes its URL.
- the commands of the commit graph work on the selected branch, remote branch or tag: <kbd>m</kbd> merges it into the current branch and <kbd>r</kbd> rebases the current branch onto it (by its name), <kbd>Space</kbd> checks it out (a tag detached, a remote branch whose local branch is at another commit offering that branch first); <kbd>b</kbd> and <kbd>t</kbd> make a branch or tag from it; <kbd>D</kbd> diffs it. The commands on a commit (cherry-pick, squash, amend, drop, revert, reset) aren't in the sidebar, nor are their keys: they are the commit list's. The keys of the sidebar come first (<kbd>d</kbd> deletes the ref, it doesn't drop the commit).
- <kbd>L</kbd> shows only the commits of the current branch, on any row, like on the graph (nothing while the current branch has no commits). This branch only (`this_branch_only`, bound to no key, in the menu of a branch or remote branch) shows only the selected branch's commits, another branch's instead, and on the same one all of them again.
- <kbd>Space</kbd> on a stash applies it, keeping it in the stash list, or pops it (applies, then drops it; git keeps it when it stops on conflicts), optionally restoring what was staged as staged (`--index`); the local changes page shows the result.

The refs are always shown on the left. <kbd>1</kbd> focuses them, and the page shows what is selected:
`Local Changes` shows the local changes page, a branch, remote branch, tag or stash shows the commit graph at it, as the sidebar moves with `ui.refs.follow` on, else when <kbd>Enter</kbd> leaves the sidebar.
<kbd>Tab</kbd> switches between the local changes page and the commit graph, from anywhere, keeping the local changes page as it was (the commit message being written, the selected file). The bar at the top shows the page open and the count of local changes; the sidebar is on the commit graph only, and has the focus when switching to it, unless shrunk.

In the commit graph, <kbd>2</kbd> focuses the commit list and <kbd>3</kbd> <kbd>4</kbd> <kbd>5</kbd> open the details of the selected commit on the info / changes / files tab.
Coming back to the commit graph from the local changes page (<kbd>Tab</kbd>), the details open again as they were, with the focus on the commit list.
While the details are open, <kbd>2</kbd> moves the focus to the commit list above, where moving the selection shows the details of each commit, and <kbd>3</kbd> - <kbd>5</kbd> or <kbd>Enter</kbd> go back to the details; <kbd>Esc</kbd> there clears the picked commits, but doesn't close the details.

#### Local Changes

| Key                                  | Description                                                  | Corresponding keybind           |
| ------------------------------------ | ------------------------------------------------------------ | ------------------------------- |
| <kbd>Esc</kbd>  | Clear the selected lines, then back from lines to hunks, from hunks to files<br>Back from the commit section to files<br>Clear the picked files, then the filter | `cancel` |
| <kbd>1</kbd> <kbd>2</kbd>            | Focus unstaged/staged files                                  | `focus_panel_1` `focus_panel_2` |
| <kbd>3</kbd>                         | Focus the commit message, typing it when it is empty          | `focus_panel_3`                 |
| <kbd>Enter</kbd>                     | Select hunks in the diff<br>Select lines in the hunk<br>Commit, like <kbd>c</kbd>, in the commit section or on an empty unstaged section<br>Go to the unstaged section from an empty staged one | `confirm`    |
| <kbd>c</kbd>                         | Commit (or amend), finishing a merge, cherry-pick or revert, after confirming | `commit` |
| <kbd>e</kbd>                         | Open the selected files in `$VISUAL` or `$EDITOR`<br>Type the message, in the commit section | `edit`  |
| <kbd>A</kbd>                         | Toggle amending the last commit                              | `toggle_amend`                  |
| <kbd>m</kbd> <kbd>c</kbd>            | Continue the rebase in progress                              | `continue`                      |
| <kbd>m</kbd> <kbd>a</kbd>            | Abort the rebase, merge, cherry-pick or revert in progress (asks first) | `abort`              |
| <kbd>m</kbd> <kbd>s</kbd>            | Skip the commit the rebase, cherry-pick or revert stopped at (asks first) | `skip`             |
| <kbd>Space</kbd>                     | Stage/Unstage file, directory, hunk, line or selected lines (or the picked files)<br>Resolve a conflicted file, like <kbd>Enter</kbd><br>Type the message, in the commit section | `select` |
| <kbd>v</kbd>                         | Select a range of files or directories, from the first to the selected one; move to extend it (moving back unpicks what it went over; <kbd>v</kbd> <kbd>v</kbd> on a picked one unpicks it), <kbd>v</kbd> again to end it. A new selection adds to the picked ones; <kbd>Esc</kbd> clears them<br>While selecting lines in the diff, select lines like in the file preview, in the hunk | `toggle_select` |
| <kbd>]f</kbd> <kbd>[f</kbd>          | Go to next/previous file, staying in the diff                | `next_file` `previous_file`     |
| <kbd>]F</kbd> <kbd>[F</kbd>          | Go to the next/previous folder at the same depth, or a level up when none is left there (closed folders stay closed); from the root row, the first/last folder at the top; from the diff, on in the diff of the folder's first file | `next_folder` `previous_folder` |
| <kbd>s</kbd>                         | Stash all changes, or the picked files (asks for a message)  | `stash`                         |
| <kbd>d</kbd>                         | Discard unstaged file, directory, hunk, line or selected lines (asks first) | `discard`                       |
| <kbd>Right/Left</kbd> <kbd>l/h</kbd> | Expand/Collapse directory                                    | `navigate_right` `navigate_left` |
| <kbd>/</kbd>                         | Filter files<br>Search the diff, while it has the focus      | `search`                        |
| <kbd>y</kbd><kbd>y</kbd> <kbd>y</kbd><kbd>f</kbd> | Copy the relative/absolute paths of the selected files, one per line<br>Copy the line, selected lines or hunk (<kbd>y</kbd><kbd>y</kbd>), or the file's absolute path (<kbd>y</kbd><kbd>f</kbd>), in the diff | `short_copy` `full_copy` |
| <kbd>+</kbd> <kbd>-</kbd>            | Show more/fewer lines around the changes, in the diff of one file | `increase_diff_context` `decrease_diff_context` |
| <kbd>i</kbd>                         | Add the file or directory to `.gitignore` (see below) | `ignore`         |
| <kbd>B</kbd>                         | Blame the file as of the last commit (see below); the local changes aren't blamed, and only one file may be picked | `blame`     |
| <kbd>T</kbd>                         | Show the commits changing the file (see below); only one file may be picked | `timeline`                      |
| <kbd>n/N</kbd>                       | Go to next/previous search match                             | `go_to_next` `go_to_previous`   |
| <kbd>Down/Up</kbd> <kbd>j/k</kbd>    | Select next/previous file, hunk or line                      | `navigate_down` `navigate_up`   |
| <kbd>J/K</kbd> <kbd>Shift-Down/Up</kbd>                       | Select next/previous file, hunk or line                      | `select_down` `select_up`       |
| <kbd>g/G</kbd>                       | Select first/last file, hunk or line                         | `go_to_top` `go_to_bottom`      |
| <kbd>Ctrl-f/b</kbd>                  | Scroll page down/up; jump a page while selecting hunks or lines | `page_down` `page_up`           |
| <kbd>Ctrl-d/u</kbd>                  | Select the files half a page down/up; jump half a page while selecting hunks or lines | `half_page_down` `half_page_up` |
| <kbd>Ctrl-e/y</kbd>                  | Scroll down/up, the selection coming along once it would leave the view, like vim | `scroll_down` `scroll_up`       |
| <kbd>Ctrl-r</kbd>                    | Refresh                                                      | `refresh`                       |

While selecting hunks or lines the page keys move the selection to the next or previous hunk or changed line from the row half a page (or a page) on; a hunk taller than the page is read a page at a time and hands over once the page passes its end. From the files, every scroll key moves the files, like the other lists; the diff, the commit message or a submodule panel is scrolled with them only while it has the focus.

Selecting a submodule shows the panel beside the files in place of the gitlink diff: how many files are uncommitted inside it, and the commits between the two gitlinks the section compares — the index and the working tree when unstaged, the base commit and the index when staged — drawn with the graph like the commit list. Above them it says whether the gitlink moved forward, the commits added, or back, the commits removed (a rewind, like `git diff --submodule=log` calls it), as when the submodule is checked out before the commit the parent records. <kbd>Enter</kbd> focuses it, so the scroll keys move its commits; <kbd>Esc</kbd> goes back to the files. A commit that moves a submodule's gitlink shows the same list, between the gitlink it recorded and the previous one, on the changes tab of the commit details. The parent repository can only stage the gitlink, so those commits and files are made in the submodule itself. A submodule that isn't checked out says so.

<kbd>c</kbd> (or <kbd>Enter</kbd> in the commit section, or on an empty unstaged section) shows a popup with the commit to make: the branch, the message and the staged files; <kbd>Enter</kbd> or <kbd>c</kbd> again commits, <kbd>Esc</kbd> cancels. When the message is empty, it starts typing it instead.
While a rebase is stopped (on a conflict or to edit a commit), <kbd>m</kbd> <kbd>c</kbd> continues it, keeping the messages of the rebased commits, and <kbd>m</kbd> <kbd>s</kbd> skips the commit it stopped at; <kbd>c</kbd> still commits what is staged with the typed message, then goes on with the rebase. <kbd>m</kbd> does nothing while no operation is in progress.
While a merge, cherry-pick or revert is stopped on conflicts, the message box is filled with the message git prepared, the files that conflicted listed in the description, and <kbd>c</kbd> commits to finish it (going on with the next commits of a cherry-pick or revert of several commits).
Resolve the conflicts first; the files are staged as they are resolved. <kbd>m</kbd> <kbd>a</kbd> aborts any of them, <kbd>m</kbd> <kbd>s</kbd> skips the commit a cherry-pick or revert stopped at (a merge has none), and amending is not available meanwhile.
The commit section title shows when amending, rebasing, merging, cherry-picking or reverting.
The message has a one-line summary and a description under it. Focusing the commit section (<kbd>3</kbd>, or moving down from the diff) starts typing when the message is empty; in the commit section, <kbd>j</kbd>/<kbd>k</kbd> (and <kbd>g</kbd>/<kbd>G</kbd>) pick the summary or the description and <kbd>Space</kbd> or <kbd>e</kbd> start typing in it. While typing, <kbd>Enter</kbd> in the summary goes on to the description, <kbd>Tab</kbd> switches between them and <kbd>Esc</kbd> stops. The keys of the file list, like filtering, picking and discarding, do nothing in the commit section.
While editing the message, all keys are typed into it (<kbd>Enter</kbd> starts a new line in the description) until <kbd>Esc</kbd>, with Emacs-like editing keys: <kbd>Ctrl-u</kbd> undoes and <kbd>Ctrl-r</kbd> redoes, <kbd>Alt-b</kbd>/<kbd>Alt-f</kbd> or <kbd>Ctrl-Left</kbd>/<kbd>Ctrl-Right</kbd> move by word, <kbd>Ctrl-w</kbd> deletes the word before the cursor, <kbd>Ctrl-k</kbd> the rest of the line. Long lines wrap at words on screen; the message keeps them as typed. Text pasted into the message keeps its lines and tabs.

<kbd>A</kbd> turns amending on: the staged files show the changes of the last commit combined with what is already staged, the message box shows its message, and <kbd>c</kbd> amends it.
Unstaging while amending removes changes from the commit being amended. Turning it off restores the previous message, and so does HEAD moving meanwhile, by a checkout or a commit made elsewhere.

#### Blame

<kbd>B</kbd> on a file shows who last changed each line of it, in a window over everything, named in the page bar with the file: at the top, **[1] Commit**, the commit it is shown as of (the last one changing the file, up to the commit of the details) with its date and subject; below, **[2] Blame**, the commit, author and date of each line beside its text, given on the first line of each run of lines from the same commit.

| Key                                  | Description                                                  | Corresponding keybind           |
| ------------------------------------ | ------------------------------------------------------------ | ------------------------------- |
| <kbd>Esc</kbd>  | Clear the search, the selected lines, then back to the commit shown, then close | `cancel` |
| <kbd>1</kbd> <kbd>2</kbd> <kbd>Ctrl-Up/Down</kbd> | Focus the commit shown/the lines (<kbd>k</kbd> on the first line goes up too, <kbd>j</kbd> down) | `focus_panel_1` `focus_panel_2` `focus_up` `focus_down` |
| <kbd>Ctrl-Left/Right</kbd>           | Focus the commits/the text of the lines                     | `focus_left` `focus_right`      |
| <kbd>Left/Right</kbd> <kbd>h/l</kbd> | Act on the commit or the text of the selected line<br>Blame as of the older/newer commit, on the commit shown, like <kbd>[</kbd> <kbd>]</kbd> | `navigate_left` `navigate_right` |
| <kbd>[</kbd> <kbd>]</kbd>            | Blame as of the older/newer commit changing the file, renames followed, on the same line | `older_commit` `newer_commit` |
| <kbd>Enter</kbd>                     | Select the commit of the line on the graph, closing the blame | `confirm`                      |
| <kbd>Space</kbd>                     | Check out the commit, after confirming, closing the blame    | `select`                        |
| <kbd>y</kbd><kbd>y</kbd> <kbd>y</kbd><kbd>f</kbd> | Copy the commit short/full hash<br>Copy the line, or the selected lines, on its text | `short_copy` `full_copy` |
| <kbd>v</kbd>                         | Select the lines from the first to the selected one, on the text; another selection adds to them, <kbd>Esc</kbd> clears them | `toggle_select` |
| <kbd>o</kbd> <kbd>y</kbd><kbd>l</kbd> | Open the commit, or the file at the line or the lines selected, on the web host; copy its link (on the text, of the file at the lines) | `open_in_browser` `copy_link` |
| <kbd>e</kbd>                         | Open the file under its newest name, as it is in the working tree, in `$VISUAL` or `$EDITOR` | `edit` |
| <kbd>/</kbd> <kbd>n/N</kbd>          | Search the lines' commits (by hash, author or date, from one run of lines to the next) or the text, whichever has the focus; go to next/previous match | `search` `go_to_next` `go_to_previous` |
| <kbd>H</kbd>                         | Show/hide all the keys working where the focus is            | `keys_toggle`                   |

The commit keys act on the commit at the top while it has the focus, and on the commit of the selected line in the left column; in the text, <kbd>y</kbd> <kbd>y</kbd>, <kbd>y</kbd> <kbd>l</kbd> and <kbd>o</kbd> act on the line, the other keys of the file preview work too. The commit selected with <kbd>Enter</kbd> may not be in the graph, hidden by <kbd>L</kbd> or a folded merge: a warning says so.

#### Timeline

<kbd>T</kbd> on a file shows the commits changing it, renames followed, in a window over everything, named in the page bar with the file: **[1] Commits**, the newest first, on the last one changing it up to the commit of the details; beside them, **[2] Diff**, the file's changes in the selected commit.

| Key                                  | Description                                                  | Corresponding keybind           |
| ------------------------------------ | ------------------------------------------------------------ | ------------------------------- |
| <kbd>Esc</kbd>  | Clear the search, back out of the lines and hunks, then close | `cancel`               |
| <kbd>1</kbd> <kbd>2</kbd> <kbd>Ctrl-Left/Right</kbd> | Focus the commits/the diff                    | `focus_panel_1` `focus_panel_2` `focus_left` `focus_right` |
| <kbd>Right/Left</kbd> <kbd>l/h</kbd> | Into the diff (its hunks)/back out of it                     | `navigate_right` `navigate_left` |
| <kbd>[</kbd> <kbd>]</kbd>            | Select the older/newer commit, from the diff too, staying in it | `older_commit` `newer_commit` |
| <kbd>Enter</kbd>                     | Select the commit on the graph, closing the timeline<br>In the diff, select the lines of the hunk | `confirm` |
| <kbd>v</kbd>                         | While selecting lines in the diff, select lines like in the file preview, in the hunk | `toggle_select` |
| <kbd>Space</kbd>                     | Check out the commit, after confirming, closing the timeline<br>In the diff, apply the selected hunk, line or lines to the working tree, as in the details | `select`            |
| <kbd>y</kbd><kbd>y</kbd> <kbd>y</kbd><kbd>f</kbd> | Copy the commit short/full hash<br>Copy the hunk or line, in the diff | `short_copy` `full_copy` |
| <kbd>+</kbd> <kbd>-</kbd>            | Show more/fewer lines around the changes | `increase_diff_context` `decrease_diff_context` |
| <kbd>o</kbd> <kbd>y</kbd><kbd>l</kbd> | Open the commit, or from the diff the file at the selected lines, on the web host; copy its link | `open_in_browser` `copy_link` |
| <kbd>e</kbd>                         | Open the file under its newest name, as it is in the working tree, in `$VISUAL` or `$EDITOR` | `edit` |
| <kbd>/</kbd> <kbd>n/N</kbd>          | Search the commits (by hash, date or subject), or the diff while it has the focus; go to next/previous match | `search` `go_to_next` `go_to_previous` |
| <kbd>H</kbd>                         | Show/hide all the keys working where the focus is            | `keys_toggle`                   |

In both windows, <kbd>,</kbd> dismisses the notices, <kbd>w</kbd> wraps long lines and <kbd>Alt-Left/Right</kbd> move the border after the commits, as everywhere. Every scroll key moves the commits. A commit whose diff has no hunk, like a rename, puts the focus back on the commits.

#### Keys list

| Key                                  | Description                                        | Corresponding keybind           |
| ------------------------------------ | -------------------------------------------------- | ------------------------------- |
| <kbd>Enter</kbd>                     | Run the selected action, of the focused panel      | `confirm`                       |
| <kbd>/</kbd>                         | Filter the actions by what they do                 | `search`                        |
| <kbd>Esc</kbd>                       | Clear the filter, then close                       | `cancel`                        |
| <kbd>?</kbd>                         | Close                                              | `help_toggle`                   |
| <kbd>Down/Up</kbd> <kbd>j/k</kbd>    | Select the next/previous action                    | `navigate_down` `navigate_up`   |
| <kbd>g/G</kbd> <kbd>Ctrl-d/u</kbd> <kbd>Ctrl-f/b</kbd> | Go to the top/bottom, half a page, a page | `go_to_top` `go_to_bottom` `half_page_down` `half_page_up` `page_down` `page_up` |

----

- [Custom Keybindings](./custom-keybindings.md)
