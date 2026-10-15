# Custom Keybindings

You can set your own custom key bindings.

Custom key bindings can be applied by writing them in the `[keybind]` section of [the config file](../configurations/config-file-format.md).

The default key binding settings are described in [`./assets/default-keybind.toml`](https://github.com/lusingander/serie/blob/master/assets/default-keybind.toml).
You can set key bindings for each action in the same format.

- It is possible to set multiple key bindings for one action.
- If you do not set key bindings for an action, the default key bindings will be assigned.
- You can disable an action by setting `[]` as the key bindings.

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

Keys separated by spaces are a sequence, pressed one after another, for example `"] v"` (the default of `next_picked`).
After the first keys, the status line shows them while waiting for the next one; <kbd>Esc</kbd> cancels, and a key that does not continue the sequence acts as usual.

A key cannot both start a sequence and be bound on its own, since the sequence could not be typed.
Binding a key on its own in your config replaces the default sequences starting with it, and binding a sequence replaces the default binding of its first key.

### Keys of a page

The commands of the commit graph (`diff`, `rebase`, `merge`, `checkout`, `cherry_pick`, `squash`, `revert`, `reset`) and of the local changes page (`toggle_stage`, `stash`, `commit`, `abort`, `toggle_amend`, `edit`, `discard`) only apply on their page, so the two pages can use the same key for different commands, like <kbd>d</kbd> for `diff` and `discard`. Likewise `dialog_toggle` and `dialog_edit` only apply in the popups of the git commands.
A key bound to any other event applies on every page, and takes over the keys of the pages.
The tabs of the commit details are a page inside the commit graph: `next_tab` and `previous_tab` apply while a tab has the focus, and the commands of the commit graph apply there too.

A key of a page may also start a key sequence: on its page it acts right away, elsewhere it starts the sequence. For example <kbd>]</kbd> shows the next tab of the details, while <kbd>]</kbd> <kbd>v</kbd> goes to the next picked commit in the commit list.
