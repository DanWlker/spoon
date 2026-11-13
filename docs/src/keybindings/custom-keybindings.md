# Custom Keybindings

You can set your own custom key bindings.

Custom key bindings can be applied by writing them in the `[keybind]` section of [the config file](../configurations/config-file-format.md).

The default key binding settings are described in [`./assets/default-keybind.toml`](https://github.com/DanWlker/spoon/blob/spoon/assets/default-keybind.toml).
You can set key bindings for each action in the same format.

- It is possible to set multiple key bindings for one action.
- If you do not set key bindings for an action, the default key bindings will be assigned.
- You can disable an action by setting `[]` as the key bindings.
- Some actions have no default key (`checkout`, which <kbd>Space</kbd> does, and `ignore`); run them from the keys list (<kbd>?</kbd>), or bind a key to them.

## Key Formats

You can use the following formats to define key bindings.

### Modifier Keys

- `ctrl-`
- `alt-`
- `shift-`

Modifiers can be combined, for example: `ctrl-shift-a`.

### Special Keys

| Key | Description |
| --- | --- |
| `esc` | Escape |
| `enter` | Enter |
| `left` | Left arrow |
| `right` | Right arrow |
| `up` | Up arrow |
| `down` | Down arrow |
| `home` | Home |
| `end` | End |
| `pageup` | Page Up |
| `pagedown` | Page Down |
| `backtab` | Back Tab (Shift + Tab) |
| `backspace` | Backspace |
| `delete` | Delete |
| `insert` | Insert |
| `f1` - `f12` | Function keys |
| `space` | Space |
| `hyphen`, `minus` | Hyphen (-) |
| `tab` | Tab |

### Character Keys

Any single character not listed above (e.g., `a`, `b`, `1`, `!`) can be used as a key.

### Key Sequences

Keys separated by spaces are a sequence, pressed one after another, for example `"] f"` (the default of `next_file`).
After the first keys, a popup lists the keys finishing a sequence there and what they do, like which-key, and the status line shows the keys pressed; <kbd>Esc</kbd> cancels, and a key that does not continue the sequence acts as usual.
The popup is made from the bindings, so a group of keys is whatever starts with the same key: with `abort = ["x a"]`, <kbd>m</kbd> lists continue and skip, and <kbd>x</kbd> abort. A first key whose sequences do nothing where the focus is, like <kbd>m</kbd> with no rebase in progress, does nothing.

A key cannot both start a sequence and be bound on its own, since the sequence could not be typed.
Binding a key on its own in your config replaces the default sequences starting with it, and binding a sequence replaces the default binding of its first key.

### Scopes

Keys are bound in scopes, the parts of the app that can have the focus, nested:

```
global
├─ graph                  the commands of the commit graph
│  ├─ sidebar             the refs
│  ├─ commit_list
│  ├─ file_history        the blame and the timeline of a file
│  │  └─ file_history_pane the lines of the blame, the diff of the timeline
│  └─ details             the tabs of the commit details
│     └─ details_pane     the diff or preview beside the files
├─ local_changes
│  └─ local_changes_diff
└─ dialog                 the popups of the git commands
```

A key is looked up from the scope having the focus outward: the first scope binding it wins, so a scope can use a key differently from those around it.
The key then works only where its command does: the status bar, the keys popup (<kbd>H</kbd>) and the keys list (<kbd>?</kbd>) show what works where the focus is, and other keys do nothing. The commands of the graph don't act on the details, say, and the diff beside the files doesn't take the keys of the files.

An event written in `[keybind]` is bound in its own scope:
the commands of the commit graph (`diff`, `rebase`, `merge`, `checkout`, `cherry_pick`, `squash`, `revert`, `reset`, `amend_commit`, `drop_commit`, `create_tag`, `create_branch`, `branch_only`, `branch_only_at_commit`) in `graph`;
those of the refs (`create_ref`, `delete_ref`, `edit_ref`, `next_section`, `previous_section`) in `sidebar`;
`older_commit` and `newer_commit` in `file_history`;
those of the local changes page (`stash`, `commit`, `next_file`, `previous_file`, `next_folder`, `previous_folder`, `continue`, `abort`, `skip`, `toggle_amend`, `discard`) in `local_changes`;
`next_tab`, `previous_tab` and `reset_file` in `details`;
`dialog_edit` in `dialog`;
and the others in `global`, working everywhere.
So <kbd>d</kbd> is `drop_commit` on the commit list, `delete_ref` on the sidebar (which overrides the graph's key) and `discard` on the local changes page.

`select` (<kbd>Space</kbd>) is the main action on what is selected, like lazygit's: it stages a file, hunk or line on the local changes page, applies a hunk or line of a commit's diff to the working tree, checks out a commit, branch or tag (like `checkout`), applies a stash, keeps a side of a conflict, and toggles a field of a popup. Rebinding it changes all of them; a table of a scope rebinds it there only.

A table of a scope binds keys in that scope only, overriding those around it:

```toml
[keybind]
quit = ["q"]

# in the diff or preview beside the files, x copies the selected lines
[keybind.details_pane]
short_copy = ["x"]
```

A key bound in your config replaces what that key did by default in its scope and in the scopes inside it.
A key of an inner scope may also start a key sequence around it: there it acts right away, elsewhere it starts the sequence. For example <kbd>]</kbd> shows the next tab of the details, while <kbd>]</kbd> <kbd>f</kbd> goes to the next file in local changes.
