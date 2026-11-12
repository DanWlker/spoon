# Mouse support

Date: 2026-11-12. Status: design, not approved yet.

## Intent

The owner wants to drive spoon with the mouse as well as the keys: pointing at a row and acting on it should work in every panel, the way it does in lazygit, plus a right-click menu lazygit doesn't have. The keyboard stays the full interface; the mouse is another way to reach the same actions, never a second set of behaviours.

## What exists today

- Mouse capture is on (`main.rs`, `event.rs`). `read_events` drops every mouse event but the wheel and the left button.
- The wheel dispatches `NavigateDown`/`NavigateUp` to whatever has the focus, as if `j`/`k` were pressed; it does nothing over a popup or while typing (`App`, `AppEvent::Mouse`).
- Left press, drag and release resize panels by their borders: `PanelLayout` (`layout.rs`, in `ctx.layout`) records the borders drawn in the last frame, `press` starts dragging the one under the pointer, `drag_to` moves it.
- Nothing else reacts to clicks. Nothing records which panel or row was drawn where.
- What a key does where the focus is comes from one place: each part registers its keys (`Keys`, `keys.rs`); `KeyList::hints()` is what the status bar and the keys popup show, `KeyList::takes` decides whether a key works, `KeyList::meaning` resolves Space (`select`) to the action it runs there. The keys list (`?`, `HelpView`) runs an action with `AppEvent::RunAction(event)`.

## Goals

The gestures, the same in every panel (the owner's words, made precise):

| Gesture | What it does |
| --- | --- |
| Left click on a row | Focuses the panel, selects the row, then does what Enter (`confirm`) does on it: a commit opens its details, a file opens its diff, a sidebar folder opens/closes, a sidebar ref jumps to its commit on the graph (leaving the sidebar, as Enter does). Picks stay, as when moving with the keys. |
| Double click | On an unpicked row it clears all picks first, as right click does (else Space would act on the picks, staging three picked files when a fourth was double clicked). Then does what Space (`select`) does, on whatever is under the pointer at the second click. If the first click changed the layout (opening the details shrinks the commit list), the second click acts on the row or panel now under the pointer, not the one first clicked: the owner chose this. The second click does Space instead of the click action. Where Space does nothing, it does nothing. |
| Left drag across rows | Picks the rows from the press to the release, exactly as `v` on the first row, moving to the last, `v` again. A range always picks, even one starting on a picked row. In a diff it first goes into line mode (what Enter does there), then picks lines as `v` does. What was picked before stays picked, as with `v`. |
| Ctrl + left click | Toggles the pick of that one row only, exactly as `v v` on it (see "`v v` toggles" below). |
| Right click on a row | Focuses the panel and selects the row, then opens the action menu (below) at the pointer. On an unpicked row it clears all picks first; on a picked row the picks stay, so the menu acts on them, as `C` or `d` would. |
| Click on a tab | Switches to it: the details' `[3] Info [4] Changes [5] Files`, and `Local Changes` / `Commit Graph` in the top bar. |

"Every panel" means: the refs sidebar, the commit list (graph page and over the details), the details (info, changes file tree and diff, files tree and preview), the local changes page (unstaged, staged, commit message, diff, conflict views, submodules), the blame and timeline windows, the command log, and the keys list (`?`).

### `v v` toggles (a keyboard change this needs)

The mouse may not do what the keys can't, and no key unpicks a single row today: `v v` on a picked row leaves it picked. The owner decided:

- `v` then `v` again on the same row, without moving, toggles that row: a picked row is unpicked, an unpicked one picked (as today).
- A range (`v`, moving, `v`) always picks what it covers, as today, whether it starts on a picked row or not.

This applies in every panel that picks with `v` (commit list, refs sidebar, file trees, diff lines), through the shared `Picks` / visual-selection code, with docs and tests. Ctrl + click sends the same toggle.

### Clicking while typing

While a search or filter is being typed, or the commit message is being edited, a click (left or right) does what Esc does there and nothing else: the commit message keeps its text and stops being edited, a search being typed is cleared. The owner chose that the click doesn't then act; the next click does.

### The action menu (right click)

The owner picked a small menu next to the pointer (mockup A in the brainstorm, with rounded corners):

- Bordered with rounded corners (`╭╮╰╯`), the same border as the panels, over the page, beside the pointer, so the clicked row stays visible.
- One entry per action that works on that row, as the status bar hints list them: the key, then its short label (`space  checkout`, `m  merge`, `C  cherry-pick`, `yy  copy hash`). It is built from the focused part's `KeyList::hints()` after the right click focused and selected the row, so it lists exactly the keys that work there and can't drift from them. Keys whose short text is empty (moving about) are not listed.
- It leaves out the hints that only move: the selection, the tab, or the commit shown (`move`, `top/bottom`, `page`, `scroll`, `tab`, the timeline's and blame's `step` and `next/prev`). Several of these are one hint standing for two opposite events, which can't be one row, and none is an action on the row. (Claude's call.)
- `j`/`k` and the arrow keys move in it, hovering the pointer over an entry selects it, Enter or a click on an entry runs it, Esc or a click outside closes it. Running an entry runs its event exactly as pressing its key would (the `RunAction` path the keys list already uses), so popups, refusals and warnings are the same.
- It opens below and to the right of the pointer and flips up or left where it would leave the screen (builder's detail; at tiny sizes it must not panic, see Constraints).
- The selection bar, highlight and colours of its rows look like those of every other list (`mark_selected_row`).

## Non-goals

- Clicking inside the git popups (`GitDialog`) and the commit message text: no clickable fields, checkboxes or cursor placement. A click on the commit message box focuses it, nothing more.
- Text selection: the terminal does it with Shift held, as the docs already say.
- Middle click, hover effects outside the menu, drag and drop, and dragging to scroll.
- Configurable mouse bindings.

## Anti-goals (a failure even if it works)

- A second implementation of an action for the mouse. Every gesture ends in the same event the keys send (or the same picking code `v` uses); if the mouse can do something the keys can't, or does it differently, that's a bug.
- The menu listing an action that doesn't work on the row, or missing one that does.
- A click landing on the wrong row: hit-testing must use what was drawn in the last frame (scroll offset, folded merges, filters, wrapped rows), never a recomputation that can disagree with the screen.
- A slow click on a 100k-row list or diff.

## Constraints (from the project rules)

- One shared mechanism, extended rather than copied (`CLAUDE.md`, "Shared implementations"): picking goes through `Picks` and each panel's existing visual-selection code; the menu reads `KeyList`; hit-testing is one mechanism every panel feeds, recorded at draw time like `PanelLayout` records borders, not a per-panel copy of "which row is at y".
- Keys never act on a view other than the focused one: every gesture first focuses the panel under the pointer, then acts through it.
- Background results arrive late: a click resolves against the frame on screen; if a reload replaced the rows since, it must not act on a different row than the one drawn (drop it, or resolve by what was drawn).
- Sizes reach zero: the menu and hit-testing use `saturating_*`; draw the menu at every size from 1×1 to 40×16 without panics (the temporary test the project guide describes).
- Scale: hit-testing maps a screen row to a list row by the scroll offset, with no per-row search.
- `read_events` must pass the right button and modifiers through; crossterm reports Ctrl on mouse events.
- Some terminals use Ctrl+click to open links and never pass it on; that's acceptable, the keys still pick. Mention it in the docs.
- Update README.md and `docs/src/keybindings` (the mouse section by the "borders can be dragged" paragraph). App-level flows get tests in `src/tests/app.rs`, which means the test harness needs a way to send mouse events. Each bug fix gets a test that fails without it.

## Decided by Claude (the owner can overrule while reading)

1. The wheel acts on the panel under the pointer, focusing it, then moves its selection like `j`/`k`, as today but on the hovered panel instead of the focused one (lazygit scrolls the hovered view).
2. Double click: two left presses on the same screen cell within 400 ms. Terminals don't report double clicks, so spoon times them.
3. While a git popup is open, clicks and the wheel do nothing, so a stray click can't dismiss a confirmation; the right-click menu itself takes clicks while open and closes on a click outside. While typing, the wheel does nothing, as today (clicks dismiss, see above).
4. A click on empty space in a panel (below the last row) only focuses the panel.
5. A click on a panel border keeps resizing it, as today: borders win over rows.
6. Right click where the panel has no row under the pointer (empty space) opens the menu for the panel's current selection; if nothing works there, no menu opens.

## Left to the builder

- How hit-testing is recorded and looked up (a shared "drawn regions" record next to `PanelLayout` is the obvious shape), and how each panel maps a screen row to its own row (file trees, the graph with folded merges, diffs with wrapped lines).
- How the menu is drawn and owns input (an overlay or popup in `App`), and its exact placement rule.
- How the harness sends mouse events in tests.
- The order of the work, as long as each step ships working and tested.
