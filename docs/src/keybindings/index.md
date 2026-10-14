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
| <kbd>/</kbd>                         | Start search                                       | `search`                                     |
| <kbd>Esc</kbd>                       | Cancel search                                      | `cancel`                                     |
| <kbd>n/N</kbd>                       | Go to next/previous search match                   | `go_to_next` `go_to_previous`                |
| <kbd>Ctrl-t</kbd>                    | Toggle search target                               | `search_target_toggle`                       |
| <kbd>Ctrl-g</kbd>                    | Toggle ignore case                                 | `ignore_case_toggle`                         |
| <kbd>Ctrl-x</kbd>                    | Toggle fuzzy match                                 | `fuzzy_toggle`                               |
| <kbd>m</kbd>                         | Toggle commit mark                                 | `mark_toggle`                                |
| <kbd>R</kbd>                         | Refresh                                            | `refresh`                                    |
| <kbd>c/C</kbd>                       | Copy commit short/full hash                        | `short_copy` `full_copy`                     |
| <kbd>D</kbd>                         | Toggle custom user command view                    | `user_command_1`                             |

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
| <kbd>m</kbd>                         | Toggle commit mark              | `mark_toggle`                   |
| <kbd>R</kbd>                         | Refresh                         | `refresh`                       |
| <kbd>c/C</kbd>                       | Copy commit short/full hash     | `short_copy` `full_copy`        |
| <kbd>D</kbd>                         | Toggle custom user command view | `user_command_1`                |
| <kbd>1</kbd> <kbd>2</kbd> <kbd>3</kbd> | Show info / changes / files tab | `focus_panel_1` `focus_panel_2` `focus_panel_3` |
| <kbd>Right/Left</kbd> <kbd>l/h</kbd> | Expand/Collapse directory (changes and files tabs) | `navigate_right` `navigate_left` |

The commit detail has three tabs:

- **Info**: the commit information. <kbd>j/k</kbd> scroll it.
- **Changes**: the changed files as a tree, expanded by default, with the diff of the selected file (or directory) beside it. <kbd>j/k</kbd> select a file, the scroll keys scroll the diff.
- **Files**: all files of the commit as a tree, collapsed by default, with a preview of the selected file beside it. <kbd>j/k</kbd> select a file, the scroll keys scroll the preview.

In the changes and files tabs, <kbd>Enter</kbd> expands or collapses the selected directory instead of closing the details.

#### Refs List

| Key                                                | Description      | Corresponding keybind            |
| -------------------------------------------------- | ---------------- | -------------------------------- |
| <kbd>Esc</kbd> <kbd>Backspace</kbd> <kbd>Tab</kbd> | Close refs list  | `close` `cancel` `ref_list`      |
| <kbd>Down/Up</kbd> <kbd>j/k</kbd>                  | Move down/up     | `navigate_down` `navigate_up`    |
| <kbd>J/K</kbd>                                     | Move down/up     | `select_down` `select_up`        |
| <kbd>g/G</kbd>                                     | Go to top/bottom | `go_to_top` `go_to_bottom`       |
| <kbd>Right/Left</kbd> <kbd>l/h</kbd>               | Open/Close node  | `navigate_right` `navigate_left` |
| <kbd>R</kbd>                                       | Refresh          | `refresh`                        |
| <kbd>c</kbd>                                       | Copy ref name    | `short_copy`                     |

The refs list opens with `Local Changes` selected from the commit list, and with `Branches` selected from the local changes page.
Closing it keeps showing what is selected, so <kbd>Tab</kbd> cycles: local changes → branches → commit list → local changes.

#### Local Changes

| Key                                  | Description                                                  | Corresponding keybind           |
| ------------------------------------ | ------------------------------------------------------------ | ------------------------------- |
| <kbd>Esc</kbd> <kbd>Backspace</kbd>  | Back from lines to hunks, from hunks to files<br>Back from the commit section to files<br>Close local changes | `close` `cancel` |
| <kbd>Tab</kbd>                       | Open refs list on branches                                   | `ref_list`                      |
| <kbd>1</kbd> <kbd>2</kbd>            | Focus unstaged/staged files                                  | `focus_panel_1` `focus_panel_2` |
| <kbd>3</kbd>                         | Focus commit message, amend checkbox and commit button       | `focus_panel_3`                 |
| <kbd>Enter</kbd>                     | Select hunks in the diff<br>Select lines in the hunk<br>Edit commit message / Toggle amend / Commit | `confirm` |
| <kbd>Space</kbd>                     | Stage/Unstage file, directory, hunk or line                  | `toggle_stage`                  |
| <kbd>d</kbd>                         | Discard unstaged file, directory, hunk or line (asks first)  | `discard`                       |
| <kbd>Right/Left</kbd> <kbd>l/h</kbd> | Expand/Collapse directory                                    | `navigate_right` `navigate_left` |
| <kbd>Down/Up</kbd> <kbd>j/k</kbd>    | Select next/previous file, hunk or line                      | `navigate_down` `navigate_up`   |
| <kbd>J/K</kbd>                       | Select next/previous file, hunk or line                      | `select_down` `select_up`       |
| <kbd>g/G</kbd>                       | Select first/last file, hunk or line                         | `go_to_top` `go_to_bottom`      |
| <kbd>Ctrl-f/b</kbd>                  | Scroll diff page down/up                                     | `page_down` `page_up`           |
| <kbd>Ctrl-d/u</kbd>                  | Scroll diff half page down/up                                | `half_page_down` `half_page_up` |
| <kbd>Ctrl-e/y</kbd>                  | Scroll diff down/up                                          | `scroll_down` `scroll_up`       |
| <kbd>R</kbd>                         | Refresh                                                      | `refresh`                       |

In the commit section, direction keys move between the message, the amend checkbox and the commit button.
The commit button amends the last commit while amend is checked.
While editing the message, all keys are typed into it (<kbd>Enter</kbd> starts a new line) until <kbd>Esc</kbd>.

Checking amend shows the changes of the last commit in the staged files, combined with what is already staged, and its message in the message box.
Unstaging while amending removes changes from the commit being amended. Unchecking it restores the previous message.

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
| <kbd>m</kbd>                         | Toggle commit mark          | `mark_toggle`                   |
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
