# Mouse Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Click, double click, ctrl+click, drag and right click (with an action menu) work in every panel of spoon, each ending in the same events the keys send.

**Architecture:** Panels record where they drew each row into one shared per-frame record (`Hits`, next to the drag borders in `layout.rs`). A pure recognizer turns raw crossterm mouse events into gestures. `App` resolves a gesture against the last frame, asks the part under the pointer to focus and select that row (`point`), then dispatches ordinary `UserEvent`s through `App::dispatch`, so every action, refusal and hint stays the keys'. The right-click menu is built from `KeyList::hints()` of the focused part.

**Tech Stack:** Rust, ratatui, crossterm (already dependencies; nothing new).

**Spec:** `docs/superpowers/specs/2026-11-12-mouse-support-design.md` (read it with this plan).

## Global Constraints

- Never commit: the owner commits. Each task ends with the checks, not a commit.
- Checks before a task is done: `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo sort-derives --check`, `cargo test` (the whole suite).
- No second implementation of an action for the mouse: gestures end in `App::dispatch(event, KeyEvent::new(KeyCode::Null, KeyModifiers::NONE))`, the path `AppEvent::RunAction` already uses, or in the picking code `v` uses.
- One shared hit-testing mechanism (`Hits`); no per-panel copy of "which row is at y".
- Every gesture first focuses the panel under the pointer (keys never act on an unfocused view).
- `saturating_*` on every screen coordinate and size; no per-row search (map a screen line to a row by the drawn offset).
- Double click: two left presses on the same screen cell within 400 ms.
- While a git popup is open, clicks and the wheel do nothing; while typing (search, filter, commit message), a click does what Esc does there and nothing else, and the wheel does nothing.
- Borders win over rows: a press on a panel border resizes, as today.
- The menu: rounded border (`╭╮╰╯`, as the panels), below-right of the pointer, flipping up/left at the screen edges; one row per hint of `KeyList::hints()`, leaving out the hints that only move.
- Ctrl+click sends `ToggleSelect` twice on the clicked row (`v v`), which toggles its pick (Task 1).
- Drag picks: `v` at the press row, the selection follows the pointer, `v` at release; a range always picks. In a diff in hunk mode it first dispatches `Confirm` (into line mode).
- Tests: app flows in `src/tests/app.rs`; no dependence on the user's git config or home. A bug-fix test must fail without the fix (check once).
- Update README.md and `docs/src/keybindings/index.md` (the paragraph about dragging borders, line 37) in the task that changes the behaviour.

## Review Focus

1. A reload lands between the frame and the click (rows changed under the pointer): the click must not act on another row than the one drawn. Hits are cleared when the view is replaced or refreshed; a click with no hits only does nothing until the next frame. Test in Task 4.
2. A click below the last row, in an empty list, or in a new repository with no commits: only focuses, never panics. Test in Task 4.
3. Tiny terminals: the menu and hit-testing at every size from 1×1 to 40×16 never panic. Test in Task 5.
4. A drag that leaves its panel (onto a border, another panel, off screen): the selection stays clamped to the panel it started in. Test in Task 4.
5. A double click whose first click changed the layout (opening the details shrinks the commit list): the second click acts on what is drawn under the pointer then, with no panic. Test in Task 4.

---

### Task 1: `v v` toggles a pick (keyboard)

**Files:**
- Modify: `src/widget/picks.rs` (`Picks`)
- Modify: `src/widget/commit_list.rs:174-184` (`toggle_visual`), `src/widget/file_tree.rs:463-470` (`TreeSelection::toggle_visual`), `src/widget/ref_list.rs` (`RefPicks`, `RefListState::toggle_visual`)
- Test: `src/widget/picks.rs` tests, `src/tests/app.rs`
- Docs: `docs/src/keybindings/index.md` (the `v` rows and the sidebar `v` paragraph)

**Interfaces:**
- Produces: `Picks::finish_visual(&mut self)`: ends a visual selection the user ended with `v`; when it covered exactly one key and that key was picked before it started, unpicks it. `Picks::end_visual` keeps its meaning (cancelled or stopped: what it picked stays) for the other callers (`commit_list.rs:446`, `ref_list.rs` `visual_outside_section` and `clear_picks`). `RefPicks::finish_visual(&mut self)` forwards to it.

- [ ] **Step 1: Write the failing unit test** `test_v_v_on_a_picked_key_unpicks_it` in `picks.rs`: picks `a`; `start_visual`, `sync_visual(&["a"])`, `finish_visual` → `!contains("a")`; then on unpicked `b`: same sequence → `contains("b")`; then a range `start_visual`, `sync_visual(&["a","b","c"])`, `finish_visual` with `b` picked before → all three picked.
- [ ] **Step 2: Run** `cargo test test_v_v_on_a_picked_key` → FAIL (no `finish_visual`).
- [ ] **Step 3: Implement** `finish_visual`: remember in `sync_visual` the keys of the last sync when there is exactly one (a field `single: Option<K>`, cleared by `start_visual`/`end_visual`/`clear`/`retain`); `finish_visual` removes it from `order` and `set` when it is in the `before_visual` set, then ends as `end_visual` does. Call `finish_visual` from the three `toggle_visual` ends.
- [ ] **Step 4: Write the app test** `test_v_v_toggles_the_pick_of_a_row` in `src/tests/app.rs`: on the graph `v v` picks the selected commit (`picked_commit_hashes().len() == 1`), `v v` again unpicks it (`0`); `v j v` then `k v v` keeps both picked except the toggled one; the same `v v` toggle in the sidebar on a branch (`app.shell.sidebar.picked().len()`) and in the local changes unstaged files.
- [ ] **Step 5: Run** the tests → PASS; revert `finish_visual`'s removal once to see Step 4 fail.
- [ ] **Step 6: Docs**: the `v` rows say `v v` on a picked row unpicks it.
- [ ] **Step 7: Run the checks** (Global Constraints) → all pass.

### Task 2: Gestures from mouse events

**Files:**
- Create: `src/pointer.rs` (registered in `main.rs` as `mod pointer;`)
- Modify: `src/event.rs:344-350` (`read_events`: also pass `Down(MouseButton::Right)` and `Moved`; modifiers come with the event; crossterm's `EnableMouseCapture` already turns on motion reporting)
- Test: `src/pointer.rs` tests

**Interfaces:**
- Produces, in `src/pointer.rs`:
  - `pub enum Gesture { Click(Cell), DoubleClick(Cell), CtrlClick(Cell), RightClick(Cell), DragStart { from: Cell, to: Cell }, DragTo(Cell), DragEnd(Cell), Wheel { at: Cell, toward: Toward }, Hover(Cell) }` with `pub struct Cell { pub x: u16, pub y: u16 }`.
  - `pub struct Pointer` (default) with `pub fn gesture(&mut self, mouse: MouseEvent, now: Instant) -> Option<Gesture>`. Press-left records the press; release on the same cell gives `Click`, or `DoubleClick` when the previous click was on the same cell less than `DOUBLE_CLICK = Duration::from_millis(400)` before its press; left press with Ctrl gives `CtrlClick` on release; the first `Drag(Left)` onto another cell gives `DragStart`, later ones `DragTo`, release after a drag `DragEnd`; right press gives `RightClick`; wheel gives `Wheel`; `Moved` gives `Hover` (only the menu uses it). A drag never ends in a click.
  - `pub fn cancel(&mut self)`: forgets a press, for a press taken by a border drag.

- [ ] **Step 1: Write the failing tests** in `pointer.rs`: `test_click_and_double_click` (press+release = Click; again at +300 ms = DoubleClick; again at +900 ms = Click; at another cell = Click), `test_drag_never_clicks` (press, drag to another cell = DragStart{from,to}, drag = DragTo, release = DragEnd, no Click), `test_ctrl_and_right_click`, `test_wheel`.
- [ ] **Step 2: Run** `cargo test pointer` → FAIL (module missing).
- [ ] **Step 3: Implement** `Pointer` and pass right presses through `read_events`.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Run the checks** → all pass.

### Task 3: Where rows are drawn: `Hits`

**Files:**
- Modify: `src/layout.rs` (next to `PanelLayout`, which `ctx.layout` holds)
- Modify: `src/app.rs:1242` (cleared with `clear_borders`), `App::replace_view` and view refreshes (cleared too)
- Test: `src/layout.rs` tests

**Interfaces:**
- Produces, in `src/layout.rs`:
  - `pub enum Target { Sidebar, Commits, DetailTab(DetailTab), Info, DetailFiles, DetailPane, Unstaged, Staged, Commit(CommitField), Submodule, ChangesSide, BlameRevision, BlameCommits, BlameText, TimelineCommits, TimelineDiff, LogCommands, LogOutput, KeysList, Page(ViewKind), Menu }` (one per thing a click can focus; extend when a task finds another).
  - `pub enum RowMap { None, Rows { first: usize, count: usize }, Lines(Vec<Option<usize>>) }`: a screen line `i` of the area is row `first + i` while `< count`; `Lines` for wrapped text (one entry per screen line).
  - `PanelLayout::record(&mut self, target: Target, area: Rect, rows: RowMap)`, `PanelLayout::hit(&self, x: u16, y: u16) -> Option<Hit>` with `pub struct Hit { pub target: Target, pub row: Option<usize> }` (the last recorded area containing the cell wins, as drawn last), `PanelLayout::clear_hits(&mut self)`.
- Each panel records its inner area (inside the border) when it draws; tabs and the page bar record each tab's own rect with `RowMap::None`.

- [ ] **Step 1: Write the failing tests** `test_hit_maps_screen_lines_to_rows` (Rows{first: 10, count: 12} at y 5: y 5 → 10, y 6 → 11, y 7 → None past count, outside area → None), `test_hit_takes_the_last_drawn` (overlapping areas: the later wins), `test_hit_lines_for_wrapped_text`, `test_hits_at_zero_size` (empty `Rect` records nothing, `hit` returns None).
- [ ] **Step 2: Run** `cargo test test_hit` → FAIL.
- [ ] **Step 3: Implement** `record`, `hit`, `clear_hits`; clear in `App::render` beside `clear_borders` and wherever `App` replaces or refreshes the view (Review Focus 1).
- [ ] **Step 4: Run** → PASS; checks → pass.

### Task 4: Clicks on the graph page: sidebar and commit list

**Files:**
- Modify: `src/app.rs` (`AppEvent::Mouse` handling at ~775-800: border press first, then `Pointer`, then `handle_gesture`)
- Modify: `src/tests/app.rs` harness: `Step::Mouse(MouseEvent)` (sent as `AppEvent::Mouse`), and `fn mouse_on(text: &'static str, gesture: TestGesture) -> Vec<Step>` that draws a frame, finds the first cell of `text` in it (panicking with the screen when absent) and sends the raw events of `enum TestGesture { Click, DoubleClick, CtrlClick, RightClick, DragTo(&'static str), WheelDown, WheelUp }` (a double click sends its two presses at once, well inside 400 ms)
- Modify: `src/view/views.rs` (`View::point`), `src/view/list.rs` (`ListView::point`), `src/widget/commit_list.rs` (select a row and sync visual picks, as `handle_navigation` does), `src/widget/commit_list.rs` render (record `Target::Commits` with `RowMap::Rows`), `src/widget/ref_list.rs` (`RefListState::point`, and record `Target::Sidebar` in `RefList::render`)
- Test: `src/tests/app.rs`
- Docs: README.md and `docs/src/keybindings/index.md` mouse paragraph (line 37) for click, double click, ctrl+click, drag, wheel

**Interfaces:**
- Consumes: `Pointer`, `Gesture` (Task 2), `PanelLayout::hit`, `Target`, `RowMap` (Task 3), `Picks::finish_visual` via `ToggleSelect` (Task 1).
- Produces:
  - `pub enum OnPicks { Keep, ClearUnlessPicked }`, in `src/pointer.rs`.
  - `View::point(&mut self, target: Target, row: Option<usize>, picks: OnPicks) -> bool` and `RefListState::point(&mut self, row: usize, picks: OnPicks)`: focus the target's panel, select `row` when `Some` and within the rows (keeping the visual selection in sync), clear the picks when `ClearUnlessPicked` and the row isn't picked; false when the target isn't this view's.
  - `App::handle_gesture(&mut self, gesture: Gesture)`, with the mapping (spec table): `Click` → point(Keep) + `Confirm`; `DoubleClick` → point(ClearUnlessPicked) + `Select`; `CtrlClick` → point(Keep) + `ToggleSelect` ×2; `RightClick` → point(ClearUnlessPicked) + open the menu (Task 5; until then nothing); `DragStart` → point(Keep) at `from` + `ToggleSelect` + point at `to`; `DragTo` → point(Keep), ignored when the hit's target isn't the drag's (Review Focus 4); `DragEnd` → `ToggleSelect`; `Wheel` → point(Keep, row None) + `NavigateDown/Up`; `Hover` → nothing outside the menu. A right click on empty space (row None) keeps the panel's selection and opens the menu for it. A sidebar target focuses the sidebar (`focus_sidebar`) before acting; another target blurs it.
  - Typing (`App::is_typing`): a click or right click sends `KeyCode::Esc` to `type_key` and stops; the wheel does nothing. A git popup open: nothing.

- [ ] **Step 1: Write the failing tests** in `src/tests/app.rs`:
  - `test_click_on_a_commit_opens_its_details`: `mouse_on("c1", Click)` → `View::Detail`, the selected commit's subject is `c1`.
  - `test_double_click_on_a_commit_checks_it_out`: `mouse_on("feat1", DoubleClick)` → a popup whose command starts with `checkout` (Review Focus 5: it is the commit drawn under the pointer at the second click).
  - `test_ctrl_click_toggles_one_pick`: `CtrlClick` on `c1` → 1 picked; again → 0.
  - `test_drag_picks_the_rows`: `mouse_on("c2", DragTo("c1"))` → 3 commits picked (c2, feat1, c1 in this repo's order); a drag continuing onto the sidebar picks nothing more (Review Focus 4).
  - `test_double_click_on_an_unpicked_row_clears_the_picks`: pick `c2` with `v v`, double click `c1` → picks 0 and the checkout popup is for `c1`.
  - `test_clicks_in_the_sidebar`: click `feat` → selects it, the sidebar loses the focus (Enter); click `Tags` header → opens it; double click `feat` → checkout popup.
  - `test_click_while_typing_only_dismisses`: `/` typing `x`, click `c1` → not typing, no search, still `View::List`; the next click opens the details.
  - `test_click_with_a_popup_open_does_nothing`, `test_click_below_the_last_row_only_focuses` (Review Focus 2, also in an empty repository), `test_border_press_still_resizes` (press on the sidebar border, drag: sidebar width changes, nothing picked), `test_wheel_scrolls_the_panel_under_the_pointer` (wheel over the sidebar moves its selection and focuses it), `test_click_after_a_reload_uses_the_drawn_frame` (a reload between draw and click: nothing happens until drawn again; Review Focus 1).
- [ ] **Step 2: Run** `cargo test -- test_click test_double test_ctrl test_drag test_border test_wheel` → FAIL.
- [ ] **Step 3: Implement** recording in the sidebar and commit list renders, `point` for both, `handle_gesture`, the typing and popup rules.
- [ ] **Step 4: Run** → PASS; check one test fails with its rule reverted (e.g. remove the ClearUnlessPicked step).
- [ ] **Step 5: Docs**, then the checks → all pass.

### Task 5: The right-click menu

**Files:**
- Create: `src/widget/action_menu.rs` (`pub mod action_menu;` in `src/widget.rs`)
- Modify: `src/app.rs` (`AppShell::menu: Option<ActionMenu>`; keys and mouse go to it while open; drawn last, over everything but notices)
- Test: `src/widget/action_menu.rs` tests, `src/tests/app.rs`
- Docs: README.md and `docs/src/keybindings/index.md`

**Interfaces:**
- Consumes: `KeyList::hints()`, `App::focused_keys()`, `App::dispatch`, `Gesture::RightClick` (Tasks 2, 4), `mark_selected_row`, `render_panel`-style rounded border from `src/widget/style.rs`.
- Produces:
  - `pub struct ActionMenu { entries: Vec<KeyHint>, at: Cell, cursor: ListCursor }`, `ActionMenu::new(hints: Vec<KeyHint>, at: Cell) -> Option<ActionMenu>` (None when no entry is left after filtering), `fn handle_key(&mut self, event: UserEvent) -> MenuOutcome` with `pub enum MenuOutcome { Stay, Close, Run(UserEvent) }` (j/k and arrows move, Enter runs the first event of the selected hint, Esc closes), `fn handle_gesture(&mut self, gesture: Gesture, area: Rect) -> MenuOutcome` (`Hover` over an entry selects it, a click on an entry runs it, a click outside closes), `fn area(&self, screen: Rect) -> Rect` (placement), `fn render(&mut self, buf: &mut Buffer, screen: Rect, theme: &ColorTheme)` (records `Target::Menu`).
  - `pub fn only_moves(events: &[UserEvent]) -> bool`: true for the hints left out (every event is a move of the selection, a tab, or the commit shown: `UserEvent::moves_selection`, `NextTab/PreviousTab`, `OlderCommit/NewerCommit`, `GoToNext/GoToPrevious`).
  - Each entry shows the hint's keys (as the status bar renders them, `keys_to_string`) then its short text. Running an entry: close the menu, then `App::dispatch(event, Null key)`.
- Placement: below-right of `at`; if it would pass the right edge, left of `at`; if past the bottom, above; clamped inside the screen with `saturating_*`; height capped to the screen, scrolling with `cursor`.

- [ ] **Step 1: Write the failing tests**: in `action_menu.rs` `test_placement_flips_at_the_edges` (at (2,2) on 80×24 → x ≥ 2, y ≥ 3; at (78,22) → right edge ≤ 80 and above the pointer), `test_moving_hints_are_left_out` (hints `move`, `tab`, `checkout` → one entry), `test_draws_at_every_size` (screens 1×1 to 40×16, every `at` corner: no panic; Review Focus 3); in `src/tests/app.rs` `test_right_click_menu_on_a_commit` (right click `c1` → menu open, an entry `checkout` and `merge`, no `move`; `j` then Enter runs the second entry's event, as its key would; Esc closes; a click outside closes), `test_right_click_on_an_unpicked_row_clears_the_picks`, `test_right_click_on_a_picked_row_keeps_them` (the menu shows the picked-commits actions like `cherry-pick`).
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** `ActionMenu`, its drawing (rounded border, `mark_selected_row` on the cursor row), its routing in `App` (keys and mouse to the menu while open; `local_hints` shows its keys: `enter run • esc close`).
- [ ] **Step 4: Run** → PASS; checks → pass. Docs.

### Task 6: The details

**Files:**
- Modify: `src/view/detail.rs` (`DetailView::point`; record the commit list over the details, the tab titles `Target::DetailTab`, `Target::Info`, `Target::DetailFiles`, `Target::DetailPane`), `src/widget/commit_tabs.rs` (file tree rows; tab rects in `render_tab_titles`), `src/widget/diff_pane.rs` (`RowMap::Lines` for the diff or preview, wrapped lines included; select a line or the hunk holding it)
- Test: `src/tests/app.rs`

**Interfaces:**
- Consumes: `View::point`, `Hits`, `handle_gesture` (Tasks 3-4).
- Produces: `DiffPane::point(&mut self, row: usize)` (selects the hunk or, in line mode, the line drawn at that row) and `DiffPane::is_selecting_lines(&self) -> bool` (already used by `register_diff_keys`; make it reachable). `handle_gesture`'s `DragStart` dispatches `Confirm` first when the target is a diff pane not selecting lines.

- [ ] **Step 1: Write the failing tests**: `test_click_on_a_detail_tab_switches_to_it` (click `[4] Changes` → changes tab), `test_click_on_a_file_opens_its_diff` (changes tab, click a file → its diff focused, as Enter), `test_double_click_on_a_diff_line_applies_it` (as Space: the apply popup or result Space gives), `test_drag_in_a_hunk_diff_picks_lines` (drag over three lines → line mode with those three selected), `test_click_in_the_commit_list_over_the_details` (focuses the list, selects, opens that commit's details).
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS; checks → pass.

### Task 7: The local changes page

**Files:**
- Modify: `src/view/local_changes.rs` (`LocalChangesView::point`), `src/widget/local_changes.rs` (record the unstaged and staged trees, the side pane, the submodule panel; `point` focusing a section or `Focus::Side`), `src/widget/local_changes/commit.rs` (record `Target::Commit(CommitField)` for the summary and the description boxes; a click focuses `Focus::Commit(field)`, never starts typing), `src/widget/conflict.rs`, `src/widget/submodule.rs` (rows)
- Test: `src/tests/app.rs`

- [ ] **Step 1: Write the failing tests**: `test_double_click_stages_and_unstages_a_file` (double click `f.txt` in unstaged → staged; double click it in staged → unstaged), `test_click_on_a_file_opens_its_diff`, `test_drag_picks_files` (two changed files, drag over both → 2 picked; `d` then offers both), `test_ctrl_click_toggles_a_file_pick`, `test_click_on_the_message_box_focuses_it` (not typing: `is_typing()` false; the summary selected), `test_click_while_typing_the_message_stops_typing` (typing `abc`, click elsewhere → not typing, the summary still `abc`, nothing else changed), `test_double_click_in_a_conflict_keeps_a_side` (a conflicted file: double click a choice = Space).
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS; checks → pass.

### Task 8: Windows and bars: blame, timeline, command log, keys list, page bar

**Files:**
- Modify: `src/view/blame.rs`, `src/view/timeline.rs`, `src/widget/command_log.rs`, `src/view/help.rs` (each records its panels and implements `point`), `src/app.rs` (`render_page_bar` records `Target::Page(ViewKind)` per tab; a click on one switches the page as `ref_list` (Tab) does; routing to the overlay's `point` while a window is open)
- Test: `src/tests/app.rs`
- Docs: README.md and `docs/src/keybindings/index.md`: the full mouse section (spec table, the menu, ctrl+click may be taken by some terminals for links, Shift+drag selects text)

- [ ] **Step 1: Write the failing tests**: `test_click_on_the_page_bar_switches_page` (click `Local Changes` → `View::LocalChanges`; click `Commit Graph` → back, the sidebar focused as `Tab` does), `test_click_in_the_blame_selects_a_line`, `test_click_in_the_timeline_selects_a_commit`, `test_click_on_an_action_in_the_keys_list_runs_it` (the keys list `?`: click an action row = Enter = runs it), `test_click_in_the_command_log_selects_a_command`, `test_right_click_menu_in_every_panel` (for each panel reached in this plan, a right click opens a menu or, where nothing works, none; a temporary loop over the panels is fine).
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS; checks → pass.
- [ ] **Step 5: Interactive check** in tmux (`-L spoontest`, with `VISUAL` and `EDITOR` set): tmux passes mouse events only with `set -g mouse on` in that server; click, double click, drag and right click once on each page, and note what was not checked.
