# Keybindings

You can see the keybindings by pressing the `F1` key.
The status bar shows the keys of what has the focus, like the panel or the popup, as many as fit; not the ones moving around.

The default key bindings can be overridden.

## List of all default keybindings

#### Common

| Key                            | Description | Corresponding keybind |
| ------------------------------ | ----------- | --------------------- |
| <kbd>Ctrl-c</kbd> <kbd>q</kbd> | Quit app    | `force_quit` `quit`   |
| <kbd>F1</kbd>                  | Open help   | `help_toggle`         |
| <kbd>Tab</kbd>                 | Switch between the local changes page and the commit graph | `ref_list` |
| <kbd>0</kbd>                   | Focus the refs sidebar; again to shrink it to its title, and again to grow it back | `focus_panel_0` |
| <kbd>Ctrl-Left</kbd> <kbd>Ctrl-Right</kbd> <kbd>Ctrl-h</kbd> <kbd>Ctrl-l</kbd> | Move the border beside the focused panel: the sidebar's, or the one between the files and the diff | `resize_left` `resize_right` |
| <kbd>Ctrl-Up</kbd> <kbd>Ctrl-Down</kbd> <kbd>Ctrl-k</kbd> <kbd>Ctrl-j</kbd> | Move the border above or under the focused panel: the details', the commit message's, or the one between the unstaged and staged files | `resize_up` `resize_down` |
| <kbd>f</kbd>                   | Fetch from a remote (chosen in a popup) | `fetch` |
| <kbd>F</kbd>                   | Fetch all remotes | `fetch_all` |
| <kbd>p</kbd>                   | Pull a branch of a remote (chosen in a popup) | `pull` |
| <kbd>P</kbd>                   | Push a branch to a remote (chosen in a popup) | `push` |

The borders between the panels can also be dragged with the mouse. The sizes are kept until the app quits.
With the mouse captured, the terminal selects text while Shift is held.

#### Fetch, pull and push

<kbd>f</kbd>, <kbd>p</kbd> and <kbd>P</kbd> open a popup over any page but the help:

- Fetch: the remote to fetch from.
- Pull: the remote and the branch to pull, whether to rebase instead of merging (`pull.rebase` checks it), and whether to stash and reapply local changes (`--autostash` or `--no-autostash`; checked by `rebase.autoStash` when rebasing and `merge.autoStash` when merging, until you change it).
- Push: the local branch, the remote and the branch to push it to, whether to push all tags too, and whether to force push (`--force-with-lease`). A branch without an upstream starts tracking the branch it is pushed to.

In the popup, <kbd>j/k</kbd> move between the fields, <kbd>h/l</kbd> change the selected one (the branches cycle through the branches of the remote), <kbd>v</kbd> toggles a checkbox or goes to the next option of the field (`dialog_toggle`), <kbd>e</kbd> types a branch name (`dialog_edit`) (<kbd>Enter</kbd> or <kbd>Esc</kbd> to finish), and <kbd>Space</kbd> does either, toggling when the field can be toggled (`dialog_activate`); <kbd>Enter</kbd> runs it and <kbd>Esc</kbd> cancels.
The key that opened the popup runs it too, and the popup waits a moment before showing: pressing <kbd>f</kbd> <kbd>f</kbd>, <kbd>p</kbd> <kbd>p</kbd> or <kbd>P</kbd> <kbd>P</kbd> quickly runs the command with its defaults without showing the popup.
The popup shows the git command it will run.

The command runs in the background, the status line shows it while it runs, and the app refreshes when it is done.
Git cannot ask for a password or passphrase meanwhile, so use a credential helper or an SSH agent: a prompt fails the command instead.

#### Commit List

| Key                                  | Description                                        | Corresponding keybind                        |
| ------------------------------------ | -------------------------------------------------- | -------------------------------------------- |
| <kbd>Down/Up</kbd> <kbd>j/k</kbd>    | Move down/up                                       | `navigate_down` `navigate_up`                |
| <kbd>J/K</kbd> <kbd>Shift-Down/Up</kbd>                       | Move down/up                                       | `select_down` `select_up`                    |
| <kbd>g/G</kbd>                       | Go to top/bottom                                   | `go_to_top` `go_to_bottom`                   |
| <kbd>Ctrl-f/b</kbd>                  | Scroll page down/up                                | `page_down` `page_up`                        |
| <kbd>Ctrl-d/u</kbd>                  | Scroll half page down/up                           | `half_page_down` `half_page_up`              |
| <kbd>Ctrl-e/y</kbd>                  | Scroll down/up                                     | `scroll_down` `scroll_up`                    |
| <kbd>Enter</kbd>                     | Show commit details<br>Apply search (if searching) | `confirm`                                    |
| <kbd>2</kbd> <kbd>3</kbd> <kbd>4</kbd> | Show commit details on the info / changes / files tab | `focus_panel_2` `focus_panel_3` `focus_panel_4` |
| <kbd>v</kbd>                         | Pick/Unpick commit (the status line shows how many) | `toggle_select`                             |
| <kbd>]v</kbd> <kbd>[v</kbd>          | Go to next/previous picked commit                  | `next_picked` `previous_picked`              |
| <kbd>D</kbd>                         | Diff the checked out commit with the selected (or picked) one, or the two picked ones | `diff` |
| <kbd>r</kbd>                         | Rebase the current branch onto the selected commit | `rebase`                                     |
| <kbd>m</kbd>                         | Merge the selected commit into the current branch  | `merge`                                      |
| <kbd>c</kbd>                         | Check out the selected branch or commit            | `checkout`                                   |
| <kbd>C</kbd>                         | Cherry-pick the picked commits, or the selected one | `cherry_pick`                               |
| <kbd>s</kbd>                         | Squash the picked commits, or the selected one, into their parents | `squash`                     |
| <kbd>u</kbd>                         | Revert the picked commits, or the selected one     | `revert`                                     |
| <kbd>U</kbd>                         | Reset the current branch to the selected commit    | `reset`                                      |
| <kbd>A</kbd>                         | Amend the selected commit on the local changes page | `amend_commit`                              |
| <kbd>d</kbd>                         | Drop the picked commits, or the selected one       | `drop_commit`                                |
| <kbd>t</kbd>                         | Create a tag at the selected commit                | `create_tag`                                 |
| <kbd>o</kbd>                         | Show only the commits of the current branch, or all again | `branch_only`                         |
| <kbd>/</kbd>                         | Start search                                       | `search`                                     |
| <kbd>Esc</kbd>                       | Cancel search<br>Clear picked commits              | `cancel`                                     |
| <kbd>n/N</kbd>                       | Go to next/previous search match                   | `go_to_next` `go_to_previous`                |
| <kbd>Ctrl-t</kbd>                    | Toggle search target                               | `search_target_toggle`                       |
| <kbd>Ctrl-g</kbd>                    | Toggle ignore case                                 | `ignore_case_toggle`                         |
| <kbd>Ctrl-x</kbd>                    | Toggle fuzzy match                                 | `fuzzy_toggle`                               |
| <kbd>R</kbd>                         | Refresh                                            | `refresh`                                    |

#### Commands on the commit graph

<kbd>D</kbd> shows the files changed between two commits and their diff in the details, like the changes tab: the checked out commit and the selected one (or the picked one), or the two picked commits.
The older commit is the base. It errors with more than two picked commits. <kbd>Esc</kbd> closes it, from the commit list too, and clears the picks.
While a diff is open, of commits or of refs from the sidebar, no other is started: <kbd>D</kbd> and picking are refused until it is closed.
<kbd>Enter</kbd> on the commit list shows the details of the selected commit over the diff; <kbd>Esc</kbd> goes back to the diff, and another <kbd>Esc</kbd> closes it.

While a rebase, merge, cherry-pick or revert is stopped (on conflicts, or to edit a commit), the commands that move or rewrite HEAD wait until it is continued or aborted on the local changes page: rebase, merge, checkout, cherry-pick, squash, revert, amend, drop and pull are refused, and their keys are left out of the status bar.

The other commands ask first, in a popup showing what they do and the git commands they run:

- <kbd>r</kbd> rebase: the current branch onto the selected commit, named by its hash (not a branch or tag at it), stashing and reapplying local changes (checked by `rebase.autoStash`, or by default when it is not set).
- <kbd>m</kbd> merge: the selected commit, named by its hash (not a branch or tag at it), into the current branch: fast-forward if possible, always a merge commit, fast-forward only, squash, or don't commit (`merge.ff` picks the default: `false` a merge commit, `only` fast-forward only). Squashing and not committing leave the changes on the local changes page, with the message git prepared.
- <kbd>c</kbd> checkout: a branch at the selected commit (a remote branch becomes a new local branch tracking it) or the commit itself (detached). The local changes are kept (git refuses when they conflict), stashed and reapplied, or discarded (untracked files are kept).
- <kbd>C</kbd> cherry-pick: the picked commits (oldest first), or the selected one, committing them or leaving the changes on the local changes page, optionally appending "(cherry picked from commit …)" to the messages.
- <kbd>s</kbd> squash: each picked commit, or the selected one, into its parent, combining their messages or keeping the parent's (fixup), with an interactive rebase from there, stashing and reapplying local changes like rebase. The commits must be on the current branch, with no merge commits after them.
- <kbd>d</kbd> drop: removes the picked commits, or the selected one, and their changes, with an interactive rebase; the commits after them are rebuilt on top, stashing and reapplying local changes like rebase.
- <kbd>u</kbd> revert: the picked commits (newest first), or the selected one, committing the reverts or leaving them on the local changes page.
- <kbd>U</kbd> reset: the current branch to the selected commit, mixed (keep the changes, unstaged), soft (keep them staged) or hard (discard them).
- <kbd>A</kbd> amend: edits the selected commit on the local changes page, where amending is turned on: its changes are staged, combined with what is staged, and its message is in the message box, both to change. For the last commit it opens right away; an older one asks first, then an interactive rebase stops at it (`edit`), stashing and reapplying the local changes like rebase. Committing (<kbd>c</kbd>) amends it and goes on with the rebase; <kbd>C</kbd> aborts. The commits after it must be on the current branch, with no merge commits.

- <kbd>t</kbd> tag: a new tag at the selected commit, annotated when it has a message.

<kbd>o</kbd> shows only the history of `HEAD` (the commits of the current branch, and of the branches merged into it), and again all commits; the title of the commit list says so ("main only").
The commits that are not on the current branch are dimmed, and the branches and tags at a commit are shown as labels before its subject, the checked-out branch standing out.

Drop rewrites the commits after the ones dropped too, so like squash and amend they must be on the current branch, with no merge commits after them.
Rebase, merge, checkout and reset work on the selected commit only, so they error while commits are picked.
The commands run in the background like fetch, pull and push. When one stops on conflicts, or leaves changes to commit, the local changes page is shown to resolve and commit them.

These keys are bound for the commit graph only: the local changes page uses <kbd>d</kbd> <kbd>c</kbd> <kbd>C</kbd> <kbd>s</kbd> for its own commands. While the commit details are open, they work with the commit list focused (<kbd>1</kbd>), not from the tabs of the details.
| <kbd>y/Y</kbd>                       | Copy commit short/full hash                        | `short_copy` `full_copy`                     |

#### Commit Detail

| Key                                  | Description                     | Corresponding keybind           |
| ------------------------------------ | ------------------------------- | ------------------------------- |
| <kbd>Esc</kbd> <kbd>Backspace</kbd>  | Clear the picked commits, then the search and filter, then close commit details | `close` `cancel` |
| <kbd>Down/Up</kbd> <kbd>j/k</kbd>    | Scroll down/up                  | `navigate_down` `navigate_up`   |
| <kbd>Ctrl-f/b</kbd>                  | Scroll page down/up             | `page_down` `page_up`           |
| <kbd>Ctrl-d/u</kbd>                  | Scroll half page down/up        | `half_page_down` `half_page_up` |
| <kbd>g/G</kbd>                       | Go to top/bottom                | `go_to_top` `go_to_bottom`      |
| <kbd>J/K</kbd> <kbd>Shift-Down/Up</kbd>                       | Select older/newer commit       | `select_down` `select_up`       |
| <kbd>R</kbd>                         | Refresh                         | `refresh`                       |
| <kbd>y/Y</kbd>                       | Copy commit short/full hash; in the changes and files tabs, the relative/absolute path of the selected file | `short_copy` `full_copy` |
| <kbd>1</kbd>                         | Focus the commit list: moving the selection shows the details of each commit, <kbd>Enter</kbd> goes back to the details | `focus_panel_1` |
| <kbd>2</kbd> <kbd>3</kbd> <kbd>4</kbd> | Show info / changes / files tab | `focus_panel_2` `focus_panel_3` `focus_panel_4` |
| <kbd>]</kbd> <kbd>[</kbd>            | Show the next/previous tab, around from files to info, while a tab has the focus (with the commit list focused, they start <kbd>]v</kbd> <kbd>[v</kbd>) | `next_tab` `previous_tab` |
| <kbd>Right/Left</kbd> <kbd>l/h</kbd> | Expand/Collapse directory (changes and files tabs) | `navigate_right` `navigate_left` |
| <kbd>/</kbd> <kbd>?</kbd>            | Search/Filter files (changes and files tabs); search the diff while it has the focus | `search` `filter` |
| <kbd>Space</kbd>                     | Apply the selected hunk or line of the diff to the working tree (changes tab) | `apply_change` |
| <kbd>n/N</kbd>                       | Go to next/previous search match | `go_to_next` `go_to_previous` |

The commit detail has three tabs:

- **Info**: the commit information. <kbd>j/k</kbd> scroll it.
- **Changes**: the changed files as a tree, expanded by default, with the diff of the selected file (or directory) beside it. <kbd>j/k</kbd> select a file, the scroll keys scroll the diff (from the files too).
- **Files**: all files of the commit as a tree, collapsed by default, with a preview of the selected file beside it. <kbd>j/k</kbd> select a file, the scroll keys scroll the preview.

In the files tab, <kbd>Enter</kbd> moves the focus into the preview of the selected file (on a directory, it expands or collapses it instead of closing the details): <kbd>j/k</kbd> select a line, <kbd>y</kbd> copies it as in the file, <kbd>/</kbd> searches the file (<kbd>n/N</kbd> next/previous match), and <kbd>Esc</kbd> goes back to the files.

In the changes tab (and a diff of two commits), <kbd>Enter</kbd> moves the focus into the diff, like on the local changes page: <kbd>j/k</kbd> select hunks, <kbd>Enter</kbd> then selects the changed lines of the hunk, and <kbd>Esc</kbd> goes back to the hunks, then to the files (<kbd>h/l</kbd> expand and collapse directories there). In the diff:

- <kbd>/</kbd> searches its text (ignoring case unless the query has an uppercase letter), selecting the changed line matched, else its hunk; <kbd>n/N</kbd> go to the next/previous match.
- <kbd>y</kbd> copies the selected line as in the file, or the selected hunk as a diff.
- <kbd>Space</kbd> applies the selected hunk or line to the working tree (`git apply`), when it applies there; the count of the local changes in the sidebar follows.

While the diff or the preview has the focus, only its own keys work there: the keys of the files beside it (<kbd>Y</kbd>, <kbd>?</kbd>, <kbd>h/l</kbd>, <kbd>J/K</kbd> to the next commit) do nothing. The same goes for the diff of the local changes page, where <kbd>/</kbd> searches the diff and <kbd>y</kbd> copies the line or hunk too, and <kbd>v</kbd>, <kbd>]v</kbd> <kbd>[v</kbd>, <kbd>?</kbd>, <kbd>Y</kbd>, <kbd>h/l</kbd> do nothing. The commands of the page still work, like committing or the commands of the commit graph.

#### Searching and filtering files

The file trees of the local changes page and of the changes and files tabs can be searched and filtered by path:

- <kbd>/</kbd> searches: the selection jumps to the first match as you type, expanding the directories hiding it, and matching names are highlighted. <kbd>n/N</kbd> go to the next/previous match.
- <kbd>?</kbd> filters: only matching files are shown, with their directories expanded.

<kbd>Enter</kbd> keeps the query, <kbd>Esc</kbd> while typing clears it. Afterwards, the panel title shows the active search and filter, and <kbd>Esc</kbd> clears the search, then the filter.
Matching ignores case unless the query has an uppercase letter.
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

- every 10 seconds, the local changes are reloaded: the local changes page keeps its selection, and elsewhere the count in the sidebar follows.
- every 2 seconds, it checks where the local branches and `HEAD` point; when they changed (a commit, a checkout or a rebase from another terminal), everything is reloaded, like <kbd>R</kbd>.
- when the terminal gets the focus back, both are checked right away.

Nothing is reloaded while a git command runs, a popup is open, or something is being typed or confirmed; the next check catches up.

#### Picking files

On the local changes page, <kbd>v</kbd> picks files and directories (highlighted, with the count in the panel title).
While files are picked, <kbd>Space</kbd>, <kbd>d</kbd> and <kbd>s</kbd> apply to them instead of the row under the cursor, and <kbd>Enter</kbd> shows their combined diff to stage or unstage their hunks and lines.
<kbd>s</kbd> asks for a stash message in the status line (it may be empty), and stashes including untracked files.

#### Refs Sidebar

| Key                                                  | Description      | Corresponding keybind            |
| ---------------------------------------------------- | ---------------- | -------------------------------- |
| <kbd>Esc</kbd> <kbd>Backspace</kbd>                  | Back to the page | `close` `cancel`                 |
| <kbd>Enter</kbd>                                     | Open/Close node, else back to the page | `confirm`              |
| <kbd>1</kbd> - <kbd>4</kbd>                          | Focus a panel of the page | `focus_panel_1` ... `focus_panel_4` |
| <kbd>Down/Up</kbd> <kbd>j/k</kbd>                  | Move down/up     | `navigate_down` `navigate_up`    |
| <kbd>J/K</kbd> <kbd>Shift-Down/Up</kbd>                                     | Move down/up     | `select_down` `select_up`        |
| <kbd>g/G</kbd>                                     | Go to top/bottom | `go_to_top` `go_to_bottom`       |
| <kbd>Right/Left</kbd> <kbd>l/h</kbd>               | Open/Close node  | `navigate_right` `navigate_left` |
| <kbd>R</kbd>                                       | Refresh          | `refresh`                        |
| <kbd>y</kbd>                                       | Copy ref name    | `short_copy`                     |
| <kbd>a</kbd>                                       | New branch (on the branches and remotes) or tag (on the tags) | `create_ref` |
| <kbd>d</kbd>                                       | Delete the branch or tag, drop the stash | `delete_ref` |
| <kbd>e</kbd>                                       | Rename the branch or change its upstream; edit the remote | `edit_ref`   |
| <kbd>Space</kbd>                                   | Apply the stash; elsewhere open/close node | `apply_stash` |
| <kbd>v</kbd>                                       | Pick/Unpick the ref (only refs of one kind together) | `toggle_select` |
| <kbd>D</kbd>                                       | Diff the two picked refs, or the current branch with the picked or selected one | `diff` |

The commands on the refs ask first, in a popup:

- <kbd>a</kbd> on the branches: a new branch from the selected branch (or remote branch, which it then tracks), else from the current branch, checking it out or not. The start can be typed, or chosen among the branches with <kbd>v</kbd>.
- <kbd>a</kbd> on the tags: a new tag at `HEAD` (or a revision typed), annotated when it has a message.
- <kbd>v</kbd> picks refs, all of one kind: branches, remote branches, tags or stashes (picking another kind warns; <kbd>Esc</kbd> clears the picks first). The title of the sidebar shows how many are picked. <kbd>d</kbd> then deletes (or drops) all of them in one popup, and <kbd>D</kbd> diffs the two picked refs.
- <kbd>D</kbd> diffs two refs: the two picked ones (the first picked is the base), else the current branch and the picked or selected one. The changes between them open in the details, like <kbd>D</kbd> on the graph, and the commit graph shows only the history of the two (its title says which); closing the comparison shows all of it again.
- <kbd>d</kbd> deletes a local branch (warning when it isn't merged into `HEAD` or its upstream), a remote branch (on the remote, with `git push --delete`), or a tag (optionally from the remote too); and drops a stash.
- <kbd>e</kbd> on a local branch renames it, or changes its upstream (typed, or chosen among the remote branches; empty for none). On a remote (like `origin`, under the remotes) it renames it or changes its URL.
- the commands of the commit graph work on the selected branch, remote branch or tag: <kbd>m</kbd> merges it into the current branch and <kbd>r</kbd> rebases the current branch onto it (by its name), <kbd>c</kbd> checks it out (a tag detached); the others, like <kbd>D</kbd> diff or <kbd>t</kbd> tag, work on the commit it points at. The keys of the sidebar come first (<kbd>d</kbd> deletes the ref, it doesn't drop the commit).
- <kbd>Space</kbd> on a stash applies it, keeping it in the stash list, or pops it (applies, then drops it; git keeps it when it stops on conflicts), optionally restoring what was staged as staged (`--index`); the local changes page shows the result.

The refs are always shown on the left. <kbd>0</kbd> focuses them, and the page shows what is selected:
`Local Changes` shows the local changes page, a branch, remote branch, tag or stash shows the commit graph at it.
<kbd>Tab</kbd> switches between the local changes page and the commit graph, from anywhere, keeping the local changes page as it was (the commit message being written, the selected file).

In the commit graph, <kbd>1</kbd> focuses the commit list and <kbd>2</kbd> <kbd>3</kbd> <kbd>4</kbd> open the details of the selected commit on the info / changes / files tab.
Coming back to the commit graph from the local changes page (<kbd>Tab</kbd>), the details open again as they were, with the focus on the commit list.
While the details are open, <kbd>1</kbd> moves the focus to the commit list above, where moving the selection shows the details of each commit, and <kbd>2</kbd> - <kbd>4</kbd> or <kbd>Enter</kbd> go back to the details; <kbd>Esc</kbd> there clears the picked commits, but doesn't close the details.

#### Local Changes

| Key                                  | Description                                                  | Corresponding keybind           |
| ------------------------------------ | ------------------------------------------------------------ | ------------------------------- |
| <kbd>Esc</kbd> <kbd>Backspace</kbd>  | Back from lines to hunks, from hunks to files<br>Back from the commit section to files<br>Clear the picked files, then the search, then the filter | `close` `cancel` |
| <kbd>1</kbd> <kbd>2</kbd>            | Focus unstaged/staged files                                  | `focus_panel_1` `focus_panel_2` |
| <kbd>3</kbd>                         | Focus the commit message and type it                          | `focus_panel_3`                 |
| <kbd>Enter</kbd>                     | Select hunks in the diff<br>Select lines in the hunk<br>Commit, like <kbd>c</kbd>, in the commit section | `confirm`    |
| <kbd>c</kbd>                         | Commit (or amend), finishing a merge, cherry-pick or revert, after confirming; or continue a rebase | `commit` |
| <kbd>e</kbd>                         | Open the file in `$VISUAL` or `$EDITOR`<br>Type the message, in the commit section | `edit`  |
| <kbd>A</kbd>                         | Toggle amending the last commit                              | `toggle_amend`                  |
| <kbd>C</kbd>                         | Abort the rebase, merge, cherry-pick or revert in progress (asks first) | `abort`              |
| <kbd>Space</kbd>                     | Stage/Unstage file, directory, hunk or line (or the picked files)<br>Type the message, in the commit section | `toggle_stage` |
| <kbd>v</kbd>                         | Pick/Unpick file or directory                                | `toggle_select`                 |
| <kbd>]v</kbd> <kbd>[v</kbd>          | Go to next/previous picked file                              | `next_picked` `previous_picked` |
| <kbd>s</kbd>                         | Stash all changes, or the picked files (asks for a message)  | `stash`                         |
| <kbd>d</kbd>                         | Discard unstaged file, directory, hunk or line (asks first)  | `discard`                       |
| <kbd>Right/Left</kbd> <kbd>l/h</kbd> | Expand/Collapse directory                                    | `navigate_right` `navigate_left` |
| <kbd>/</kbd>                         | Search files<br>Search the diff, while it has the focus      | `search`                        |
| <kbd>?</kbd>                         | Filter files                                                 | `filter`                        |
| <kbd>y/Y</kbd>                       | Copy the relative/absolute path of the file<br>Copy the line or hunk (<kbd>y</kbd>), in the diff | `short_copy` `full_copy` |
| <kbd>n/N</kbd>                       | Go to next/previous search match                             | `go_to_next` `go_to_previous`   |
| <kbd>Down/Up</kbd> <kbd>j/k</kbd>    | Select next/previous file, hunk or line                      | `navigate_down` `navigate_up`   |
| <kbd>J/K</kbd> <kbd>Shift-Down/Up</kbd>                       | Select next/previous file, hunk or line                      | `select_down` `select_up`       |
| <kbd>g/G</kbd>                       | Select first/last file, hunk or line                         | `go_to_top` `go_to_bottom`      |
| <kbd>Ctrl-f/b</kbd>                  | Scroll diff page down/up                                     | `page_down` `page_up`           |
| <kbd>Ctrl-d/u</kbd>                  | Scroll diff half page down/up                                | `half_page_down` `half_page_up` |
| <kbd>Ctrl-e/y</kbd>                  | Scroll diff down/up                                          | `scroll_down` `scroll_up`       |
| <kbd>R</kbd>                         | Refresh                                                      | `refresh`                       |

<kbd>c</kbd> (or <kbd>Enter</kbd> in the commit section) shows a popup with the commit to make: the branch, the message and the staged files; <kbd>Enter</kbd> or <kbd>c</kbd> again commits, <kbd>Esc</kbd> cancels. When the message is empty, it starts typing it instead.
While a rebase is stopped (on a conflict or to edit a commit), <kbd>c</kbd> continues it instead, keeping the messages of the rebased commits.
While a merge, cherry-pick or revert is stopped on conflicts, the message box is filled with the message git prepared, and <kbd>c</kbd> commits to finish it (going on with the next commits of a cherry-pick or revert of several commits).
Resolve the conflicts and stage the files with <kbd>Space</kbd> first. <kbd>C</kbd> aborts any of them, and amending is not available meanwhile.
The commit section title shows when amending, rebasing, merging, cherry-picking or reverting.
<kbd>3</kbd> starts typing the message; in the commit section, <kbd>Space</kbd> or <kbd>e</kbd> start typing it again.
While editing the message, all keys are typed into it (<kbd>Enter</kbd> starts a new line) until <kbd>Esc</kbd>.

<kbd>A</kbd> turns amending on: the staged files show the changes of the last commit combined with what is already staged, the message box shows its message, and <kbd>c</kbd> amends it.
Unstaging while amending removes changes from the commit being amended. Turning it off restores the previous message.

#### User Command

| Key                                  | Description                 | Corresponding keybind           |
| ------------------------------------ | --------------------------- | ------------------------------- |
| <kbd>Esc</kbd> <kbd>Backspace</kbd>  | Close user command          | `close` `cancel`                |
| <kbd>Down/Up</kbd> <kbd>j/k</kbd>    | Scroll down/up              | `navigate_down` `navigate_up`   |
| <kbd>J/K</kbd> <kbd>Shift-Down/Up</kbd>                       | Scroll down/up              | `select_down` `select_up`       |
| <kbd>Ctrl-f/b</kbd>                  | Scroll page down/up         | `page_down` `page_up`           |
| <kbd>Ctrl-d/u</kbd>                  | Scroll half page down/up    | `half_page_down` `half_page_up` |
| <kbd>g/G</kbd>                       | Go to top/bottom            | `go_to_top` `go_to_bottom`      |
| <kbd>J/K</kbd> <kbd>Shift-Down/Up</kbd>                       | Select older/newer commit   | `select_down` `select_up`       |
| <kbd>R</kbd>                         | Refresh                     | `refresh`                       |

#### Help

| Key                                                            | Description              | Corresponding keybind           |
| -------------------------------------------------------------- | ------------------------ | ------------------------------- |
| <kbd>Esc</kbd> <kbd>Backspace</kbd> <kbd>F1</kbd>              | Close help               | `close` `cancel` `help_toggle`  |
| <kbd>Down/Up</kbd> <kbd>j/k</kbd>                              | Scroll down/up           | `navigate_down` `navigate_up`   |
| <kbd>J/K</kbd> <kbd>Shift-Down/Up</kbd>                                                 | Scroll down/up           | `select_down` `select_up`       |
| <kbd>Ctrl-f/b</kbd>                                            | Scroll page down/up      | `page_down` `page_up`           |
| <kbd>Ctrl-d/u</kbd>                                            | Scroll half page down/up | `half_page_down` `half_page_up` |
| <kbd>g/G</kbd>                                                 | Go to top/bottom         | `go_to_top` `go_to_bottom`      |

</details>

----

- [Custom Keybindings](./custom-keybindings.md)
