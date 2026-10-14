# Keybindings

You can see the keybindings by pressing the `?` key.

The default key bindings can be overridden.

## List of all default keybindings

#### Common

| Key                            | Description | Corresponding keybind |
| ------------------------------ | ----------- | --------------------- |
| <kbd>Ctrl-c</kbd> <kbd>q</kbd> | Quit app    | `force_quit` `quit`   |
| <kbd>?</kbd> <kbd>F1</kbd>     | Open help   | `help_toggle`         |

#### Commit List

| Key                                  | Description                                        | Corresponding keybind                        |
| ------------------------------------ | -------------------------------------------------- | -------------------------------------------- |
| <kbd>Down/Up</kbd> <kbd>j/k</kbd>    | Move down/up                                       | `navigate_down` `navigate_up`                |
| <kbd>J/K</kbd>                       | Move down/up                                       | `select_down` `select_up`                    |
| <kbd>Alt-Down</kbd> <kbd>Alt-j</kbd> | Move to parent commit                              | `go_to_parent`                               |
| <kbd>g/G</kbd>                       | Go to top/bottom                                   | `go_to_top` `go_to_bottom`                   |
| <kbd>Ctrl-f/b</kbd>                  | Scroll page down/up                                | `page_down` `page_up`                        |
| <kbd>Ctrl-d/u</kbd>                  | Scroll half page down/up                           | `half_page_down` `half_page_up`              |
| <kbd>Ctrl-e/y</kbd>                  | Scroll down/up                                     | `scroll_down` `scroll_up`                    |
| <kbd>H/M/L</kbd>                     | Select top/middle/bottom of the screen             | `select_top` `select_middle` `select_bottom` |
| <kbd>Enter</kbd>                     | Show commit details<br>Apply search (if searching) | `confirm`                                    |
| <kbd>Tab</kbd>                       | Open refs list                                     | `ref_list`                                   |
| <kbd>m</kbd>                         | Pick/Unpick commit (the status line shows how many) | `toggle_select`                             |
| <kbd>]m</kbd> <kbd>[m</kbd>          | Go to next/previous picked commit                  | `next_picked` `previous_picked`              |
| <kbd>/</kbd>                         | Start search                                       | `search`                                     |
| <kbd>Esc</kbd>                       | Cancel search<br>Clear picked commits              | `cancel`                                     |
| <kbd>n/N</kbd>                       | Go to next/previous search match                   | `go_to_next` `go_to_previous`                |
| <kbd>Ctrl-t</kbd>                    | Toggle search target                               | `search_target_toggle`                       |
| <kbd>Ctrl-g</kbd>                    | Toggle ignore case                                 | `ignore_case_toggle`                         |
| <kbd>Ctrl-x</kbd>                    | Toggle fuzzy match                                 | `fuzzy_toggle`                               |
| <kbd>R</kbd>                         | Refresh                                            | `refresh`                                    |
| <kbd>y/Y</kbd>                       | Copy commit short/full hash                        | `short_copy` `full_copy`                     |

#### Commit Detail

| Key                                  | Description                     | Corresponding keybind           |
| ------------------------------------ | ------------------------------- | ------------------------------- |
| <kbd>Esc</kbd> <kbd>Backspace</kbd>  | Close commit details            | `close` `cancel`                |
| <kbd>Down/Up</kbd> <kbd>j/k</kbd>    | Scroll down/up                  | `navigate_down` `navigate_up`   |
| <kbd>Ctrl-f/b</kbd>                  | Scroll page down/up             | `page_down` `page_up`           |
| <kbd>Ctrl-d/u</kbd>                  | Scroll half page down/up        | `half_page_down` `half_page_up` |
| <kbd>g/G</kbd>                       | Go to top/bottom                | `go_to_top` `go_to_bottom`      |
| <kbd>J/K</kbd>                       | Select older/newer commit       | `select_down` `select_up`       |
| <kbd>Alt-Down</kbd> <kbd>Alt-j</kbd> | Select parent commit            | `go_to_parent`                  |
| <kbd>R</kbd>                         | Refresh                         | `refresh`                       |
| <kbd>y/Y</kbd>                       | Copy commit short/full hash     | `short_copy` `full_copy`        |
| <kbd>1</kbd> <kbd>2</kbd> <kbd>3</kbd> | Show info / changes / files tab | `focus_panel_1` `focus_panel_2` `focus_panel_3` |
| <kbd>Right/Left</kbd> <kbd>l/h</kbd> | Expand/Collapse directory (changes and files tabs) | `navigate_right` `navigate_left` |
| <kbd>/</kbd> <kbd>f</kbd>            | Search/Filter files (changes and files tabs) | `search` `filter` |
| <kbd>n/N</kbd>                       | Go to next/previous search match | `go_to_next` `go_to_previous` |

The commit detail has three tabs:

- **Info**: the commit information. <kbd>j/k</kbd> scroll it.
- **Changes**: the changed files as a tree, expanded by default, with the diff of the selected file (or directory) beside it. <kbd>j/k</kbd> select a file, the scroll keys scroll the diff.
- **Files**: all files of the commit as a tree, collapsed by default, with a preview of the selected file beside it. <kbd>j/k</kbd> select a file, the scroll keys scroll the preview.

In the changes and files tabs, <kbd>Enter</kbd> expands or collapses the selected directory instead of closing the details.

#### Searching and filtering files

The file trees of the local changes page and of the changes and files tabs can be searched and filtered by path:

- <kbd>/</kbd> searches: the selection jumps to the first match as you type, expanding the directories hiding it, and matching names are highlighted. <kbd>n/N</kbd> go to the next/previous match.
- <kbd>f</kbd> filters: only matching files are shown, with their directories expanded.

<kbd>Enter</kbd> keeps the query, <kbd>Esc</kbd> while typing clears it. Afterwards, the panel title shows the active search and filter, and <kbd>Esc</kbd> clears the search, then the filter.
Matching ignores case unless the query has an uppercase letter.
On the local changes page, the filter applies to both the unstaged and staged files, and staging, unstaging and discarding a directory only affect the files shown.

#### Resolving conflicts

Conflicted files are marked `U`. With the cursor on one, the diff is replaced by its conflicts: the existing side (ours) and the incoming side (theirs) of each conflict, side by side, with two tools below them.
<kbd>Enter</kbd> moves into it, <kbd>Left/Right</kbd> choose a side and <kbd>Up/Down</kbd> a tool, then <kbd>Enter</kbd>:

- on a side, resolves the file by keeping that side entirely (removing the file if that side deleted it), and stages it.
- on `Edit in merge tool`, runs `git mergetool` for the file, with the tool of [`core.git.merge_tool`](../configurations/config-file-format.md) or else git's `merge.tool` config.
- on `Open in editor`, opens the file in `$VISUAL` or `$EDITOR` (like <kbd>e</kbd>), to edit the conflict markers by hand; stage it with <kbd>Space</kbd> afterwards.

The app is suspended while the editor or merge tool runs, and refreshed after.

#### Picking files

On the local changes page, <kbd>m</kbd> picks files and directories (highlighted, with the count in the panel title).
While files are picked, <kbd>Space</kbd>, <kbd>d</kbd> and <kbd>s</kbd> apply to them instead of the row under the cursor, and <kbd>Enter</kbd> shows their combined diff to stage or unstage their hunks and lines.
<kbd>s</kbd> asks for a stash message in the status line (it may be empty), and stashes including untracked files.

#### Refs List

| Key                                                | Description      | Corresponding keybind            |
| -------------------------------------------------- | ---------------- | -------------------------------- |
| <kbd>Esc</kbd> <kbd>Backspace</kbd> <kbd>Tab</kbd> | Close refs list  | `close` `cancel` `ref_list`      |
| <kbd>Down/Up</kbd> <kbd>j/k</kbd>                  | Move down/up     | `navigate_down` `navigate_up`    |
| <kbd>J/K</kbd>                                     | Move down/up     | `select_down` `select_up`        |
| <kbd>g/G</kbd>                                     | Go to top/bottom | `go_to_top` `go_to_bottom`       |
| <kbd>Right/Left</kbd> <kbd>l/h</kbd>               | Open/Close node  | `navigate_right` `navigate_left` |
| <kbd>R</kbd>                                       | Refresh          | `refresh`                        |
| <kbd>y</kbd>                                       | Copy ref name    | `short_copy`                     |

The refs list opens with `Local Changes` selected from the commit list, and with `Branches` selected from the local changes page.
Closing it keeps showing what is selected, so <kbd>Tab</kbd> cycles: local changes → branches → commit list → local changes.

#### Local Changes

| Key                                  | Description                                                  | Corresponding keybind           |
| ------------------------------------ | ------------------------------------------------------------ | ------------------------------- |
| <kbd>Esc</kbd> <kbd>Backspace</kbd>  | Back from lines to hunks, from hunks to files<br>Back from the commit section to files<br>Clear the picked files, then the search, then the filter | `close` `cancel` |
| <kbd>Tab</kbd>                       | Open refs list on branches                                   | `ref_list`                      |
| <kbd>1</kbd> <kbd>2</kbd>            | Focus unstaged/staged files                                  | `focus_panel_1` `focus_panel_2` |
| <kbd>3</kbd>                         | Focus commit message                                         | `focus_panel_3`                 |
| <kbd>Enter</kbd>                     | Select hunks in the diff<br>Select lines in the hunk<br>Edit commit message | `confirm`    |
| <kbd>c</kbd>                         | Commit (or amend), finishing a merge, cherry-pick or revert; or continue a rebase | `commit` |
| <kbd>e</kbd>                         | Open the file in `$VISUAL` or `$EDITOR`                      | `edit`                          |
| <kbd>A</kbd>                         | Toggle amending the last commit                              | `toggle_amend`                  |
| <kbd>C</kbd>                         | Abort the rebase, merge, cherry-pick or revert in progress (asks first) | `abort`              |
| <kbd>Space</kbd>                     | Stage/Unstage file, directory, hunk or line (or the picked files) | `toggle_stage`             |
| <kbd>m</kbd>                         | Pick/Unpick file or directory                                | `toggle_select`                 |
| <kbd>]m</kbd> <kbd>[m</kbd>          | Go to next/previous picked file                              | `next_picked` `previous_picked` |
| <kbd>s</kbd>                         | Stash all changes, or the picked files (asks for a message)  | `stash`                         |
| <kbd>d</kbd>                         | Discard unstaged file, directory, hunk or line (asks first)  | `discard`                       |
| <kbd>Right/Left</kbd> <kbd>l/h</kbd> | Expand/Collapse directory                                    | `navigate_right` `navigate_left` |
| <kbd>/</kbd>                         | Search files                                                 | `search`                        |
| <kbd>f</kbd>                         | Filter files                                                 | `filter`                        |
| <kbd>n/N</kbd>                       | Go to next/previous search match                             | `go_to_next` `go_to_previous`   |
| <kbd>Down/Up</kbd> <kbd>j/k</kbd>    | Select next/previous file, hunk or line                      | `navigate_down` `navigate_up`   |
| <kbd>J/K</kbd>                       | Select next/previous file, hunk or line                      | `select_down` `select_up`       |
| <kbd>g/G</kbd>                       | Select first/last file, hunk or line                         | `go_to_top` `go_to_bottom`      |
| <kbd>Ctrl-f/b</kbd>                  | Scroll diff page down/up                                     | `page_down` `page_up`           |
| <kbd>Ctrl-d/u</kbd>                  | Scroll diff half page down/up                                | `half_page_down` `half_page_up` |
| <kbd>Ctrl-e/y</kbd>                  | Scroll diff down/up                                          | `scroll_down` `scroll_up`       |
| <kbd>R</kbd>                         | Refresh                                                      | `refresh`                       |

<kbd>c</kbd> commits the staged changes with the message, or starts typing the message when it is empty.
While a rebase is stopped (on a conflict or to edit a commit), <kbd>c</kbd> continues it instead, keeping the messages of the rebased commits.
While a merge, cherry-pick or revert is stopped on conflicts, the message box is filled with the message git prepared, and <kbd>c</kbd> commits to finish it (going on with the next commits of a cherry-pick or revert of several commits).
Resolve the conflicts and stage the files with <kbd>Space</kbd> first. <kbd>C</kbd> aborts any of them, and amending is not available meanwhile.
The commit section title shows when amending, rebasing, merging, cherry-picking or reverting.
While editing the message, all keys are typed into it (<kbd>Enter</kbd> starts a new line) until <kbd>Esc</kbd>.

<kbd>A</kbd> turns amending on: the staged files show the changes of the last commit combined with what is already staged, the message box shows its message, and <kbd>c</kbd> amends it.
Unstaging while amending removes changes from the commit being amended. Turning it off restores the previous message.

#### User Command

| Key                                  | Description                 | Corresponding keybind           |
| ------------------------------------ | --------------------------- | ------------------------------- |
| <kbd>Esc</kbd> <kbd>Backspace</kbd>  | Close user command          | `close` `cancel`                |
| <kbd>Down/Up</kbd> <kbd>j/k</kbd>    | Scroll down/up              | `navigate_down` `navigate_up`   |
| <kbd>J/K</kbd>                       | Scroll down/up              | `select_down` `select_up`       |
| <kbd>Ctrl-f/b</kbd>                  | Scroll page down/up         | `page_down` `page_up`           |
| <kbd>Ctrl-d/u</kbd>                  | Scroll half page down/up    | `half_page_down` `half_page_up` |
| <kbd>g/G</kbd>                       | Go to top/bottom            | `go_to_top` `go_to_bottom`      |
| <kbd>J/K</kbd>                       | Select older/newer commit   | `select_down` `select_up`       |
| <kbd>Alt-Down</kbd> <kbd>Alt-j</kbd> | Select parent commit        | `go_to_parent`                  |
| <kbd>R</kbd>                         | Refresh                     | `refresh`                       |

#### Help

| Key                                                            | Description              | Corresponding keybind           |
| -------------------------------------------------------------- | ------------------------ | ------------------------------- |
| <kbd>Esc</kbd> <kbd>Backspace</kbd> <kbd>?</kbd> <kbd>F1</kbd> | Close help               | `close` `cancel` `help_toggle`  |
| <kbd>Down/Up</kbd> <kbd>j/k</kbd>                              | Scroll down/up           | `navigate_down` `navigate_up`   |
| <kbd>J/K</kbd>                                                 | Scroll down/up           | `select_down` `select_up`       |
| <kbd>Ctrl-f/b</kbd>                                            | Scroll page down/up      | `page_down` `page_up`           |
| <kbd>Ctrl-d/u</kbd>                                            | Scroll half page down/up | `half_page_down` `half_page_up` |
| <kbd>g/G</kbd>                                                 | Go to top/bottom         | `go_to_top` `go_to_bottom`      |

</details>

----

- [Custom Keybindings](./custom-keybindings.md)
