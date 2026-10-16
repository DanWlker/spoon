# Config File Format

## Example

```toml
[core.option]
protocol = "auto"
order = "chrono"
graph_width = "auto"
graph_style = "rounded"
graph_antialias = false
initial_selection = "latest"

[core.git]
mailmap = false
# merge_tool = "meld"

[core.search]
target = "all"
ignore_case = false
fuzzy = false

[core.user_command]
commands_1 = { name = "git diff", commands = ["git", "--no-pager", "diff", "--color=always", "{{first_parent_hash}}", "{{target_hash}}"]}
tab_width = 4

[core.external]
clipboard = "auto"

[core.browser]
# open_command = ["xdg-open"]
[core.browser.services]
# "git.company.com" = "gitlab"

[ui.common]
cursor_type = "native"

[ui.list]
scrolloff = 0
columns = ["graph", "marker", "subject", "name", "hash", "date"]
subject_min_width = 20
date_format = "%Y-%m-%d"
date_width = 10
date_local = true
name_width = 20

[ui.detail]
height = 20
date_format = "%Y-%m-%d %H:%M:%S %z"
date_local = true

[ui.user_command]
height = 20

[ui.refs]
width = 26

[graph]
row_image_width = "compact"

[graph.color]
branches = [
  "#F38BA8",
  "#A6E3A1",
  "#F9E2AF",
  "#89B4FA",
  "#CBA6F7",
  "#94E2D5",
]
edge = "#00000000"
background = "#00000000"

[color]
fg = "#cdd6f4"
bg = "reset"
list_selected_fg = "#cdd6f4"
list_selected_bg = "#313244"
list_picked_fg = "#11111b"
list_picked_bg = "#cba6f7"
list_ref_branch_fg = "#a6e3a1"
list_ref_head_fg = "#89b4fa"
list_ref_remote_branch_fg = "#a6e3a1"
list_ref_tag_fg = "#f9e2af"
list_ref_stash_fg = "#cba6f7"
list_subject_fg = "#cdd6f4"
list_name_fg = "#cdd6f4"
list_hash_fg = "#cdd6f4"
list_date_fg = "#cdd6f4"
list_dimmed_fg = "#6c7086"
list_match_fg = "#11111b"
list_match_bg = "#f9e2af"
detail_label_fg = "#7f849c"
detail_name_fg = "#cdd6f4"
detail_date_fg = "#cdd6f4"
detail_email_fg = "#89b4fa"
detail_hash_fg = "#cdd6f4"
detail_ref_branch_fg = "#a6e3a1"
detail_ref_remote_branch_fg = "#a6e3a1"
detail_ref_tag_fg = "#f9e2af"
detail_file_change_add_fg = "#a6e3a1"
detail_file_change_modify_fg = "#f9e2af"
detail_file_change_delete_fg = "#f38ba8"
detail_file_change_move_fg = "#cba6f7"
diff_add_bg = "#394545"
diff_delete_bg = "#493446"
diff_add_word_bg = "#4e6356"
diff_delete_word_bg = "#694459"
ref_selected_fg = "#cdd6f4"
ref_selected_bg = "#313244"
help_block_title_fg = "#cba6f7"
help_key_fg = "#89b4fa"
virtual_cursor_fg = "#cdd6f4"
status_input_transient_fg = "#7f849c"
status_info_fg = "#89b4fa"
status_success_fg = "#a6e3a1"
status_warn_fg = "#f9e2af"
status_error_fg = "#f38ba8"
divider_fg = "#45475a"
accent_fg = "#89b4fa"
accent_text_fg = "#11111b"
panel_title_focused_fg = "#89b4fa"
panel_title_focused_bg = "reset"
highlight_fg = "#cba6f7"
status_bar_fg = "#a6adc8"
status_bar_bg = "reset"

[keybind]
# See the separate Custom Keybindings section for details.
# ...
```

## Configuration Options

### `core.option.protocol`

The protocol type for rendering images of commit graphs.

- type: `string` (enum)
- default: `auto`
- possible values:
  - `auto`
  - `iterm`
  - `kitty`
  - `kitty-unicode`

The value specified in the command line argument takes precedence.

### `core.option.order`

The commit ordering algorithm.

- type: `string` (enum)
- default: `chrono`
- possible values:
  - `chrono`
  - `topo`

The value specified in the command line argument takes precedence.

### `core.option.graph_width`

The character width that a graph image unit cell occupies.

- type: `string` (enum)
- default: `auto`
- possible values:
  - `auto`
  - `double`
  - `single`

The value specified in the command line argument takes precedence.

### `core.option.graph_style`

The commit graph image edge style.

- type: `string` (enum)
- default: `rounded`
- possible values:
  - `rounded`
  - `angular`
  - `curved`

The value specified in the command line argument takes precedence.

### `core.option.graph_antialias`

Whether to antialias commit graph images.

- type: `boolean`
- default: `false`

The value specified in the command line argument takes precedence.

### `core.option.initial_selection`

The initial selection of commit when starting the application.

- type: `string` (enum)
- default: `latest`
- possible values:
  - `latest`
  - `head`

The value specified in the command line argument takes precedence.

### `core.git.mailmap`

Whether to resolve author and committer identities through the repository's [`.mailmap`](https://git-scm.com/docs/gitmailmap) file.

- type: `boolean`
- default: `false`

When enabled, names and emails are displayed as mapped by `.mailmap`, in the same way as `git log` and `git shortlog`. Repositories without a `.mailmap` file are unaffected.

### `core.git.merge_tool`

The merge tool to resolve conflicts with from the local changes page, passed to `git mergetool --tool`.

- type: `string`
- default: not set, `git mergetool` uses git's `merge.tool` config

### `graph.row_image_width`

The width mode for each graph row image.

- type: `string` (enum)
- default: `compact`
- possible values:
  - `compact`: trim each row image to the minimum required graph width
    - Reduces the file size for more lightweight operation. If you are using `kitty-unicode` protocol, it helps you avoid hitting [the image transfer size limit](https://sw.kovidgoyal.net/kitty/graphics-protocol/#image-persistence-and-storage-quotas).
  - `fixed`: use the same full graph width for every row image
    - This can be used when you want to set a background color for graphs in environments that cannot correctly handle transparent images, or in environments where rendering does not work well when there are images of various widths.

### `core.search.target`

The field to search when the application starts. The target can be toggled while the commit list is displayed.

- type: `string` (enum)
- default: `all`
- possible values:
  - `all`: Search refs, commit subjects, author names, and short commit hashes
  - `subject`: Search commit subjects
  - `author`: Search author names
  - `ref`: Search branch, remote branch, and tag names
  - `hash`: Search short commit hashes

### `core.search.ignore_case`

Whether to enable ignore case when the application starts. The option can be toggled while the commit list is displayed.

- type: `boolean`
- default: `false`

### `core.search.fuzzy`

Whether to enable fuzzy matching when the application starts. The option can be toggled while the commit list is displayed. The search (<kbd>/</kbd>) and filter (<kbd>?</kbd>) of the file trees match paths fuzzily too when it is on, without a toggle; searching a diff or a file stays a plain match.

- type: `boolean`
- default: `false`

### `core.user_command.commands_{n}`

The command definition for executing external commands.

Multiple commands can be specified in the format `commands_{n}`.
For details about user command, see the separate [User command](../features/user-command.md) section.

- type: `object`
- fields:
  - `name`: `string` - The name of the user command.
  - `type`: `string` (enum) - The type of user command.
    - default: `inline`
    - possible values:
      - `inline`: Display the output of the command in the user command view.
      - `silent`: Execute the command in the background without opening a view.
      - `suspend`: Execute the command by suspending the application. This is useful for interactive commands.
  - `commands`: `array of strings` - The command and its arguments.
  - `refresh`: `boolean` - Whether to reload the repository and refresh the display after executing the command. Available for `silent` and `suspend` commands.
    - default: `false`
- examples:
    - `commands_1 = { name = "git diff", commands = ["git", "--no-pager", "diff", "--color=always", "{{first_parent_hash}}", "{{target_hash}}"]}`
    - `commands_2 = { name = "delete branch", type = "silent", commands = ["git", "branch", "-D", "{{branches}}"], refresh = true }`
    - `commands_3 = { name = "amend commit", type = "suspend", commands = ["git", "commit", "--amend"], refresh = true }`
  
### `core.user_command.tab_width`

The number of spaces to replace tabs in the user command output.

- type: `u16`
- default: `4`

### `core.external.clipboard`

The clipboard command to use for copy operations.

- type: `object` (enum)
- default: `auto`
- possible values:
  - `auto`: Use the default clipboard library
  - `{ custom = { commands = ["..."] } }`: Use a custom command that receives text via stdin
    - `commands`: `array of strings` - The command and its arguments.
- examples:
    - `clipboard = "auto"`
    - `clipboard = { custom = { commands = ["wl-copy"] } }`
    - `clipboard = { custom = { commands = ["xclip", "-selection", "clipboard"] } }`

### `core.browser`

<kbd>o</kbd> opens the page of the selected commit, branch, tag or file on the web host of its remote, <kbd>O</kbd> copies its link:

- a commit in the graph or the details: the commit
- a local branch in the sidebar: a new pull request from it (from its upstream branch, when it has one)
- a remote branch, a tag or a remote in the sidebar: its page
- a file in the Changes or Files tab of the details: the file at that commit; from the diff or the file beside it, at the selected hunk or line (`#L5-L9`), where the host's links can point at lines. A line only removed has no line in the file at that commit, so it opens the file.

The remote is the one of the branch (a local branch's upstream), else the upstream remote of the current branch, else `origin`, else the first. Its URL (`git@host:owner/repo.git`, `https://…`, `ssh://…`) becomes `https://host/owner/repo`. A commit not pushed yet opens a page that doesn't exist.

#### `core.browser.open_command`

The command opening a link, given as its last argument.

- type: `array of strings`
- default: `["xdg-open"]`, `["open"]` on macOS

#### `core.browser.services`

How to link to the pages of a host. GitHub, GitLab, Bitbucket and Gitea/Forgejo (Codeberg) are known by a domain naming them; others are named here by their domain (with its port when the remote URL has one):

- a kind of host: `"git.company.com" = "gitlab"`, one of `github`, `gitlab`, `bitbucket` and `gitea`
- a kind and the domain of its pages, for an ssh alias: `"github.com-work" = "github:github.com"`
- link patterns of its own, where `{base}` is `https://host/owner/repo`, and `{hash}`, `{branch}`, `{tag}` and `{path}` what is linked to; `line` and `lines` are added to the file's link for a selected line (`{start}`) or lines (`{start}` to `{end}`):

  ```toml
  [core.browser.services."code.example.org"]
  commit = "{base}/commit/{hash}"
  branch = "{base}/branch/{branch}"
  pull_request = "{base}/new-review?from={branch}"
  tag = "{base}/tag/{tag}"
  file = "{base}/file/{hash}/{path}"
  line = "#L{start}"
  lines = "#L{start}-{end}"
  ```

### `ui.common.cursor_type`

The type of a cursor to display in the input.

- type: `object` (enum)
- default: `native`
- possible values:
  - `native`: Use the terminal native cursor.
  - `{ virtual = "|" }`: Use a virtual cursor with the specified string.
    - value: `string` - The string to display as the virtual cursor.

### `ui.list.scrolloff`

The minimum number of visible rows to keep above and below the selected commit where possible.

- type: `u16`
- default: `0`

### `ui.list.columns`

The order and visibility of columns in the commit list.

- type: `array of strings` (enum)
- default: `["graph", "marker", "subject", "name", "hash", "date"]`
- possible values:
  - `graph`
  - `marker`
  - `subject`
  - `name`
  - `hash`
  - `date`

### `ui.list.subject_min_width`

The minimum width of a subject in the commit list.

- type: `u16`
- default: `20`

### `ui.list.date_format`

The date format of a author date in the commit list.

- type: `string`
- default: `"%Y-%m-%d"`

The format must be specified in strftime format.
https://docs.rs/chrono/latest/chrono/format/strftime/index.html

### `ui.list.date_width`

The width of a author date in the commit list.

- type: `u16`
- default: `10`

### `ui.list.date_local`

Whether to show a author date in the commit list in local timezone.

- type: `boolean`
- default: `true`

### `ui.list.name_width`

The width of a author name in the commit list.

- type: `u16`
- default: `20`

### `ui.detail.height`

The height of a commit detail area.

- type: `u16`
- default: `20`

### `ui.detail.date_format`

The date format of a author/committer date in the commit detail.

- type: `string`
- default: `"%Y-%m-%d %H:%M:%S %z"`

The format must be specified in strftime format.
https://docs.rs/chrono/latest/chrono/format/strftime/index.html

### `ui.detail.date_local`

Whether to show a author/committer date in the commit list in local timezone.

- type: `boolean`
- default: `true`

### `ui.user_command.height`

The height of a user command area.

- type: `u16`
- default: `20`

### `ui.refs.width`

The width of the refs sidebar on the left.

- type: `u16`
- default: `26`

### `graph.color.branches`

Array of colors used for the commit graph.

- type: `array of strings`
- default:
  - `"#F38BA8"`
  - `"#A6E3A1"`
  - `"#F9E2AF"`
  - `"#89B4FA"`
  - `"#CBA6F7"`
  - `"#94E2D5"`

Colors should be specified in the format `#RRGGBB` or `#RRGGBBAA`.

### `graph.color.edge`

Color of the edge surrounding the commit circles in the graph.

- type: `string`
- default: `"#00000000"`

Colors should be specified in the format `#RRGGBB` or `#RRGGBBAA`.

### `graph.color.background`

Background color of the commit graph.

- type: `string`
- default: `"#00000000"`

Colors should be specified in the format `#RRGGBB` or `#RRGGBBAA`.

### `color`

The colors of each element of the application.

Note: Graph colors are specified with `[graph.color]`.

- type: `string`
- default: see the example above

The default colors are [Catppuccin Mocha](https://catppuccin.com/palette): blue for the focused panel, mauve for the mark beside the selected row and for picked commits, and overlay grays for hints and dividers. Any of them can be overridden here. They are 24-bit colors; set them to ANSI color names (like the defaults of serie) for a terminal without true color.

The title of the focused panel is colored text, `panel_title_focused_fg`, like lazygit. Give it a background with `panel_title_focused_bg` to show it as a badge instead, like `panel_title_focused_fg = "#11111b"` on `panel_title_focused_bg = "#89b4fa"`. The titles of the popups follow it, keeping their own colors: yellow for amending, red for a confirmation.

Colors should be specified in one of the following formats:

- ANSI color name
  - `"red"`, `"bright-blue"`, `"light-red"`, `"reset"`, ...
- 8-bit color (256-color) index values
  - `"34"`, `"128"`, `"255"`, ...
- 24-bit true color hex codes
  - `"#abcdef"`, ...

### `keybind`

Key bindings for various actions in the application.

See the separate [Custom Keybindings](../keybindings/custom-keybindings.md) section for details.
