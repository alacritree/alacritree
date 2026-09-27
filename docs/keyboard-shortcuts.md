# Keyboard shortcuts

Alacritree has three layers of shortcuts:

1. **Key bindings**, parsed from your `[[keyboard.bindings]]` tables in `alacritty.toml` / `alacritree.toml`. Alacritty's defaults are preloaded, and so are alacritree's own (sidebars, workspaces, sessions, palette). Everything in this layer is rebindable.
2. **Modal keys**, `Enter` and `Escape` while a dialog is open. The modal consumes them directly. They are not bindings and cannot be rebound.
3. **Navigation keys** inside the sidebars, the palette and modal lists: the arrows, `Enter`, `Escape` and `/`. These are fixed too, and are listed under [Navigation keys](#navigation-keys).

---

## Modal keys

| Shortcut             | Action                                                |
| -------------------- | ----------------------------------------------------- |
| `Enter`              | Confirm a modal (quit, delete worktree, create branch)|
| `Escape`             | Cancel a modal                                        |
| `Up` / `Down`        | Move through a modal's list, such as the base-branch picker |

These only fire while a modal is open and never reach the terminal grid.

---

## Key bindings

These are parsed from `[[keyboard.bindings]]` and matched against egui key events before the terminal sees them. The default set is preloaded, and your TOML entries are checked first, so any default can be rebound, or freed for the shell with `action = "ReceiveChar"`. Use `action = "None"` when the key should be consumed without running an action or reaching the shell.

The alacritree-specific defaults below are ordinary bindings like the rest.

### Defaults on every platform

| Shortcut             | Action                                                |
| -------------------- | ----------------------------------------------------- |
| `Ctrl+Shift+V`       | Paste from the clipboard                              |
| `Ctrl+Shift+C`       | Copy the current selection                            |
| `Ctrl+B`             | Toggle the left (projects/worktrees) sidebar          |
| `Ctrl+G`             | Toggle the right (git status) sidebar                 |
| `Ctrl+T`             | Open a new shell session in the current workspace     |
| `Ctrl+Tab`           | Cycle to the next session in the current workspace    |
| `Ctrl+Shift+Tab`     | Cycle to the previous session                         |
| `Alt+Right`          | Switch to the next workspace (home / worktrees)       |
| `Alt+Left`           | Switch to the previous workspace                      |
| `Shift+Alt+Down`     | Cycle to the next running agent, across workspaces    |
| `Shift+Alt+Up`       | Cycle to the previous running agent                   |
| `Ctrl+Shift+O`       | Add a project to the sidebar                          |
| `Ctrl+Q`             | Open the quit confirmation dialog                     |
| `Ctrl+Backtick`      | Toggle the workspace's persistent scratchpad tab      |
| `Ctrl+Shift+Backtick`| Open the workspace's tasks tab                        |
| `Shift+Insert`       | Paste from the primary (X11) selection                |
| `Ctrl+0`             | Reset font size (does nothing, see [Font size](#font-size)) |
| `Ctrl+=` / `Ctrl++`  | Increase font size (does nothing)                     |
| `Ctrl+-`             | Decrease font size (does nothing)                     |
| `Shift+Home`         | Scroll to the top of the scrollback                   |
| `Shift+End`          | Scroll to the bottom                                  |
| `Shift+PageUp`       | Scroll one page up                                    |
| `Shift+PageDown`     | Scroll one page down                                  |
| `Shift+Tab`          | Send `CSI Z` (reverse tab, for readline and vim)      |
| `Alt+Shift+Tab`      | Send `ESC` + `CSI Z`                                  |
| `Tab` / `Shift+Tab`  | Tasks tab: indent / dedent the current task           |
| `Alt+Up` / `Alt+Down`| Tasks tab: move the current task up / down            |
| `Ctrl+Shift+B`       | Toggle keyboard focus between terminal and sidebar    |
| `Ctrl+Shift+G`       | Move focus to the git status sidebar                  |
| `Ctrl+Shift+W`       | Close the cursored session (sidebar) or the current shell |
| `Enter`              | Exited session on screen: close it                    |
| `Home` / `End`       | Sidebar focused: cursor to the first / last row       |
| `PageUp` / `PageDown`| Sidebar focused: jump to the previous / next project  |
| `R`                  | Sidebar focused: rescan every project's worktrees     |
| `O`                  | Sidebar focused: expand/collapse the selected project |
| `Delete`             | Sidebar focused: close/delete/remove the selected row |
| `Shift+R`            | Sidebar focused: rename the selected project          |
| `S`                  | Projects sidebar focused: toggle the sessions filter  |
| `A`                  | Projects sidebar focused: toggle the attention filter |
| `M`                  | Git sidebar focused: toggle the modified-files filter |
| `D`                  | Git sidebar focused: toggle the deleted-files filter  |
| `U`                  | Git sidebar focused: toggle the untracked-files filter |
| `Enter`              | Sidebar search: keep the highlighted row and leave search |
| `Escape`             | Sidebar search: leave search, staying in the sidebar  |
| `Shift+Escape`       | Sidebar search: leave search and focus the terminal   |
| `Ctrl+K`             | Open the command palette                              |
| `Home` / `End`       | Palette open: cursor to the first / last row          |
| `PageUp` / `PageDown`| Palette open: cursor a screenful up / down            |

### Additional defaults on macOS

| Shortcut             | Action                                                |
| -------------------- | ----------------------------------------------------- |
| `Cmd+V`              | Paste                                                 |
| `Cmd+C`              | Copy                                                  |
| `Cmd+N` / `Cmd+T`    | Open a new shell session in the current workspace     |
| `Cmd+0`              | Reset font size (does nothing)                        |
| `Cmd+=` / `Cmd++`    | Increase font size (does nothing)                     |
| `Cmd+-`              | Decrease font size (does nothing)                     |
| `Cmd+Shift+]`        | Next session in the current workspace                 |
| `Cmd+Shift+[`        | Previous session                                      |
| `Cmd+1` to `Cmd+8`   | Select the Nth session in the current workspace       |
| `Cmd+9`              | Select the last session                               |
| `Ctrl+Cmd+F`         | Toggle fullscreen                                     |
| `Cmd+M`              | Minimize the window                                   |
| `Cmd+K`              | Clear the scrollback buffer                           |
| `Cmd+Q`              | Open the quit confirmation dialog                     |

---

## Supported actions

Use any of these as the `action = "..."` value in a `[[keyboard.bindings]]` entry. Names match alacritty's own action names, so existing configs port over.

### Clipboard

- `Paste`: paste from the system clipboard.
- `Copy`: copy the current mouse selection to the clipboard. Does nothing when nothing is selected.
- `CopySelection`: copy the current mouse selection to the primary (X11) selection. No default key.
- `PasteSelection`: paste from the primary (X11) selection.

### Font size

- `IncreaseFontSize`
- `DecreaseFontSize`
- `ResetFontSize`

These parse, so a shared `alacritty.toml` that binds them is not reported as unsupported, but nothing resizes the terminal font at runtime and they do nothing. Change `font.size` in the config instead.

### Scrolling

- `ScrollPageUp` / `ScrollPageDown`
- `ScrollHalfPageUp` / `ScrollHalfPageDown`
- `ScrollLineUp` / `ScrollLineDown`
- `ScrollToTop` / `ScrollToBottom`
- `ClearHistory`: drop the scrollback buffer. It does not clear the visible screen. Pair it with `chars = "\x0c"` on a separate binding if you also want a `Ctrl+L`-style screen clear.

### Sessions

- `SpawnNewInstance` / `CreateNewWindow` / `CreateNewTab`: all three open a new shell session in the current workspace. Alacritty distinguishes windows from tabs, but alacritree has a single window with sessions per workspace, so they collapse to the same action.
- `OpenScratchpad`: open or select the current workspace's persistent Markdown scratchpad tab in Alacritree's minimal built-in editor, or close it without confirmation when already active. Every text change is saved immediately, and closing the tab keeps its backing file. With `ui.return_to_previous_session`, closing it goes back to the session it was opened over, as closing the tasks tab does. Default: `Ctrl+Backtick`.
- `SelectNextTab` / `SelectPreviousTab`: cycle through sessions in the current workspace.
- `SelectNextSession` / `SelectPreviousSession`: cycle through every open session across all workspaces, as one flat ring with workspaces in sidebar order, switching workspace when the cycle crosses a boundary. Unbound by default. Example binding:
  ```toml
  [[keyboard.bindings]]
  key = "PageDown"
  mods = "Control|Shift"
  action = "SelectNextSession"
  ```
- `SelectNextAgentSession` / `SelectPreviousAgentSession`: the same ring, visiting only sessions with an agent running in them, which are the rows that draw an agent mark in the sidebar, whether the agent is idle, working or blocked. Plain shells, diffs, the scratchpad and tasks tabs, and exited sessions are skipped, as are multiplexer panes no session is attached to. From a shell, the press lands on the nearest agent in its direction. Default: `Shift+Alt+Down` / `Shift+Alt+Up`.
- `SelectTab1` to `SelectTab9`: select the Nth session in the current workspace. Out-of-range indices are ignored.
- `SelectLastTab`: select the last session in the current workspace.
- `CloseSession`: close the session under the sidebar cursor when the sidebar is focused on one, otherwise the active session in the current workspace. Honors the `confirm_session_close` policy, which may open a confirmation dialog; `"busy"` prompts only while a process is running. When the on-screen workspace stops having sessions, whether this close or a worktree deletion took the last one, `ui.last_session_close` decides what follows: `"respawn"` recycles a shell in place, `"navigate"` moves to the project's main checkout or home, and `"ring_global"` / `"ring_project"` move along the flat session ring instead.
- `CloseExitedSession`: close the session on screen once its child has exited. Default: unmodified `Enter`, which works only because the action is scoped to an exited session. In a live one the key goes to the PTY like any other. There is nothing to confirm and no sidebar cursor involved, since the child is already gone. `[ui] hold_exited_sessions` decides which exits leave a session on screen to be closed this way. A session held that way writes one line into its own grid naming this key, or the command palette when nothing is bound to it.
- `NewMultiplexerPane`: open a new pane in the terminal multiplexer (herdr) and a session on it, in the focused workspace and on the side the focused session's own pane belongs to. No default key. Also reachable from the command palette.
- `AttachAllMultiplexerPanes`: open a session on every detected multiplexer (herdr) pane no session holds yet, one per row the sidebar lists, leaving the panes that already hold one alone. It skips the herdr-core panes, since another client uses them. Each session files under the workspace its own pane belongs to, and the workspace on screen does not change. No default key. Also reachable from the command palette.
- `DetachAllMultiplexerPanes`: end every session attached to a multiplexer pane. The panes keep running under the multiplexer and their rows come back unattached. `[ui] confirm_session_detach` is asked once for the whole batch rather than once per session. No default key. Also reachable from the command palette.
- `KillTerminal`: end the program in a herdr-core terminal that alacritree did not create, after a confirmation. It acts on the pane row under the sidebar cursor while the sidebar has focus, and otherwise on the active session's terminal. The row's own menu offers the same kill. No default key. Also reachable from the command palette.
- `SpawnProfile1` to `SpawnProfile9`: spawn the Nth `[[ui.profiles]]` entry in the current workspace. Out-of-range indices show an error toast. Example binding:
  ```toml
  [[keyboard.bindings]]
  key = "2"
  mods = "Control|Shift"
  action = "SpawnProfile2"
  ```

### Window

- `ToggleFullscreen`
- `ToggleMaximized`
- `Minimize`
- `Quit`: open the quit confirmation dialog.

### Sidebar cursor

- `SidebarTop` / `SidebarBottom`: move the sidebar cursor to the first / last visible row.
- `SidebarPreviousProject` / `SidebarNextProject`: jump the sidebar cursor to the nearest project header above / below.
- `RefreshProjects`: re-run worktree discovery for every project in the sidebar. Default: unmodified `R`.
- `DeleteSelected`: act on the sidebar-cursored row. A session gets the close flow, a worktree the delete/prune dialog, a project the remove-from-sidebar prompt. Default: unmodified `Delete`.
- `RenameSelected`: rename the sidebar-cursored project, with the same rename dialog as its right-click menu. Only project rows have an editable label, and on other rows this does nothing. Default: `Shift+R`.
- `ToggleProjectExpanded`: expand or collapse the project owning the sidebar cursor, showing or hiding its worktrees and sessions. Works from the project header or from any of its worktree/session rows. Collapsing from a child moves the cursor up to the header. Default: unmodified `O`. The arrow keys and `Enter` also expand/collapse a cursored project without a binding.

These act only while the projects sidebar has keyboard focus. Anywhere else their keys pass through to the terminal untouched, so the unmodified defaults don't shadow Home/End/PageUp/PageDown, `r`, `R`, or forward-delete in TUIs.

### Command palette

- `TogglePalette`: open or close the command palette, one fuzzy-searchable, executable list of every keyboard action, every open session, and every switchable workspace. The same fzf-style matcher the sidebars use ranks the rows, so every action can be found without the docs. Rows are grouped under section headings and laid out in three columns: description, action name, and every key bound to that action, so a multi-bound action reads as one row listing all its keys. A query promotes whichever section holds the best match. Type to filter, `Up`/`Down` move the selection, `Enter` runs the highlighted row (dispatching an action, jumping to a session, or switching workspace), `Escape` clears the query and then closes, and a click outside dismisses it. On a window too narrow for the palette's usual width it takes four fifths of the window instead, and the three columns shrink together and wrap their text rather than running off the edge. Anything a column still has to cut short shows in full on hover. The default is `Ctrl+K`, which shadows `Ctrl+K` for terminal apps. Rebind it, or free it with `ReceiveChar`, if you need it there. While the palette is open, `Ctrl+K` closes it even when `TogglePalette` is bound elsewhere.
- `PaletteTop` / `PaletteBottom`: move the palette cursor to the first / last row. Defaults: unmodified `Home` / `End`.
- `PalettePageUp` / `PalettePageDown`: move the palette cursor a screenful (ten rows) up / down. Defaults: unmodified `PageUp` / `PageDown`.

The last four act only while the palette is open. The palette is a modal that owns every key while it is up, so they share the sidebar's unmodified defaults without shadowing them, and pass through to the terminal whenever the palette is closed. Rebinding one replaces the whole default trigger, sidebar action included: `key = "End"` bound to `PaletteTop` also unbinds `SidebarBottom`. Because the palette's query box has focus, `Home`/`End` no longer move the text caret there. Bind them elsewhere if you want the caret back.

### Sidebars and workspaces

- `ToggleLeftSidebar`: show or hide the projects/worktrees sidebar. Default: `Ctrl+B`.
- `ToggleRightSidebar`: show or hide the git status sidebar. Default: `Ctrl+G`.
- `ToggleTasksSidebar`: show or hide the tasks docked in a sidebar, leaving the rest of the sidebar. No default key.
- `SelectNextWorkspace` / `SelectPreviousWorkspace`: move between the home tab and the worktrees. Defaults: `Alt+Right` / `Alt+Left`.
- `AddProject`: open the picker that adds a project to the sidebar. Default: `Ctrl+Shift+O`.
- `SetBaseBranch`: open the base-branch picker for the sidebar-cursored worktree, or the current one when the terminal has focus. No default key.
- `ReviewStaged` / `ReviewUnstaged` / `ReviewBranch`: open the git panel's Staged, Unstaged, or `Changes vs` section as a whole in the diff viewer, or close that pane when it is already open. No default keys.
- `ToggleSessionRows` / `ToggleSessionTabs`: flip the runtime `session_display.sidebar_always` / `session_display.tabs_always` values, which control whether a lone session still gets its own sidebar row or tab segment. No default keys.
- `ToggleSessionDrag`: flip whether a session row can be dragged with the mouse to reorder it. Starts from `[ui.session_reorder] drag`. No default key.
- `MoveSessionUp` / `MoveSessionDown`: move a session one position in the sidebar and the tab strip. With the sidebar focused this moves the cursored session, or the cursored workspace's active session when the cursor is on a Home or worktree row. From the terminal it moves the session on screen. `[ui.session_reorder] scope` sets how far a session may travel. No default keys.

### Tasks tab

- `OpenTasks`: open the current workspace's tasks tab. Does nothing unless `[integrations.taskwarrior]` or `[integrations.tasks.command]` is enabled. Default: `Ctrl+Shift+Backtick`.

The rest act on the tasks tab's current task: the row being edited, or the last one that was. They fire only while the tasks tab is focused, so their keys reach the shell in every other session, and the palette lists them only while a task integration is on.

- `IndentTask` / `DedentTask`: nest the task under the one above it, or move it back out to its parent's level. Default keys `Tab` / `Shift+Tab`.
- `MoveTaskUp` / `MoveTaskDown`: move the task past its neighbouring sibling. Default keys `Alt+Up` / `Alt+Down`.
- `DeleteTask`: delete the task, asking first when it has subtasks, which go with it. No default key.
- `ToggleCompletedTasks`: show or hide completed tasks. No default key.

### Focus navigation

- `ToggleSidebarFocus`: flip keyboard focus between the terminal and the projects sidebar. Focusing a hidden sidebar shows it, and returning focus hides it again unless you toggled it open yourself.
- `FocusProjectsSidebar` / `FocusGitSidebar` / `FocusTerminal`: move focus to one specific panel. `FocusGitSidebar` defaults to `Ctrl+Shift+G`, and the other two have no default key.
- `FocusLeft` / `FocusRight`: directional focus moves between panels. While vim, nvim, tmux, zellij or herdr runs in the terminal, the terminal gets `Ctrl+Left` / `Ctrl+Right` instead, so it can move between its own splits. Each of them needs a binding that runs `alacritree action FocusLeft` or `alacritree action FocusRight` when it has no split left in that direction. Without one, focus stays inside it. A session attached to a single herdr agent moves panel focus directly. No default keys.

### Sidebar search

Typing `/` in a focused sidebar opens a fuzzy-search prompt. The confirm and cancel keys are rebindable, search-scoped actions. They act only while the focused panel is in search mode, so their default keys pass straight through to the terminal whenever it owns focus, and the terminal's own `Enter`/`Esc` are never affected:

- `SidebarSearchConfirm` (default `Enter`): leave search with the cursor on the highlighted row, scrolled into view. Selecting never opens the row. Press `Enter` again, outside search, to activate it. A worktree or session matched under a collapsed project expands that project so the row stays selectable.
- `SidebarSearchCancel` (default `Esc`): leave search, staying in the sidebar on the previously active row.
- `SidebarSearchCancelToTerminal` (default `Shift+Esc`): leave search and return focus to the terminal.

Rebind them to any key or chord that produces no text (`Enter`, `Esc`, `Shift+Esc`, function keys, `Ctrl`/`Shift` chords). Binding a search action to a key that also produces text (a bare, `Shift`-, or `AltGr`-printable) is unsupported, because the keystroke would both type into the query and fire the action.

### Sidebar filters

Toggle actions that narrow a sidebar's rows. Defaults act only while the owning sidebar has keyboard focus, the same way `R` and `O` do above.

In the projects sidebar, the session filter and the attention filter are independent and AND together. The four PR filters form one group whose active states union, so an open-or-draft filter shows both, and that group in turn ANDs with the other two. A worktree must pass the session and attention filters and match one active PR state to stay listed.

- `ToggleSessionsFilter` (default `S`) narrows to workspaces with a live session and lists only their sessions. With `[ui] sessions_filter_counts_detached` on, a listed detached herdr agent also counts and stays listed.
- `ToggleDetachedSessionsFilter` flips the runtime `sessions_filter_counts_detached` value. No default key. A binding acts only while the projects sidebar has focus, like the other filters.
- `ToggleAttentionFilter` (default `A`): narrow to workspaces whose session wants attention.
- `TogglePrOpenFilter` / `TogglePrDraftFilter` / `TogglePrMergedFilter` / `TogglePrClosedFilter` (no default key): narrow to worktrees with an open, draft, merged, or closed PR. These four exist only when `[integrations.gh] pr_status = true`. A query against `gh` runs per worktree, so filtered rows fill in as each answer arrives rather than appearing all at once.
- `ClearProjectFilters` (no default key): drop every projects-sidebar toggle at once.
- `RefreshPrStatus` (no default key): invalidate every worktree's cached PR state and re-query `gh`. Acts while the projects sidebar has focus.

In the git sidebar a file only ever matches one change kind, so the active toggles union:

- `ToggleModifiedFilter` (default `M`): narrow to modified and renamed files.
- `ToggleDeletedFilter` (default `D`): narrow to deleted files.
- `ToggleUntrackedFilter` (default `U`): narrow to untracked and added files.
- `ClearGitFilters` (no default key): drop every git-sidebar toggle at once.

Not scoped to either panel:

- `ToggleSearchScope` (no default key): switch a sidebar's fuzzy-search query between matching only the rows the active toggles leave and matching every row. Runtime-only: restarting returns to `[ui] search_scope`.

PR lookups for the worktrees under a collapsed project run only while the projects sidebar is painted, so hiding that sidebar stops those lookups. It does not stop PR lookups altogether. The git sidebar still polls the active workspace's PR state, and a lookup already running still finishes and is still picked up at the start of the next frame.

A binding you write replaces the built-in binding on the same key. If you bind a key that a filter toggle uses by default, that toggle loses its key without a warning. Two of your own bindings on one key both stay, and each runs only in the panel it belongs to, so you can give a key back its old meaning alongside the new one:

```toml
[[keyboard.bindings]]
key = "D"
action = "DeleteSelected"        # projects sidebar

[[keyboard.bindings]]
key = "D"
action = "ToggleDeletedFilter"   # git sidebar
```

### Misc

- `None`: consume the key without doing anything. Useful to unbind a default shortcut.
- `ReceiveChar`: send the key to the terminal as if nothing were bound to it. Useful to free a default shortcut for the shell.

---

## Navigation keys

These keys are fixed and cannot be rebound. Every other key keeps its binding while a sidebar has focus, and an unbound key reaches neither the shell nor the UI.

In the projects sidebar:

- `Up` / `Down` move between rows.
- `Right` expands a project, and on a session row activates that session and returns to the terminal.
- `Left` collapses a project, and on a worktree, session or pane row jumps the cursor out to its parent.
- `Enter` activates the selected workspace and returns to the terminal. On a project header it toggles expansion instead.
- `Escape` returns to the terminal without switching.
- `/` opens [sidebar search](#sidebar-search).

In the git sidebar:

- `Up` / `Down` move between files.
- `Enter` opens the selected file's diff.
- `Escape` returns to the terminal.
- `/` opens sidebar search.

In the command palette, `Up` / `Down` move the selection. In a modal with a list, such as the base-branch picker, `Up` / `Down` move through it.

---

## Not supported

These alacritty features are not implemented:

- **Vi mode** (`ToggleViMode`, all `ViAction`/`ViMotion` variants). Alacritree does not track terminal modes.
- **Search mode** (`SearchForward`, `SearchBackward`, all `SearchAction` variants). There is no scrollback search.
- **Hints** (`Hint(...)`). Link detection uses a fixed URL pattern instead, see [Clickable links](alacritree.md#clickable-links).
- **Selection actions** `ClearSelection` and `ExpandSelection`. Mouse selection works, and `Copy` / `CopySelection` copy it.
- **Platform-specific window actions** (`Hide`, `HideOtherApplications`, `ToggleSimpleFullscreen`). Alacritty calls into AppKit directly, and eframe doesn't expose the equivalent.
- **`ClearLogNotice`**. Alacritree has no in-app log notice.
- **`Command`** (`command = { ... }`). Alacritree does not run external programs from a key binding.

A binding with any `mode`, such as `mode = "Vi"`, `"~Search"` or `"AppCursor"`, is dropped when the config loads, since alacritree has no mode tracking. A binding naming one of the unsupported actions above is kept: its key is consumed, a `debug`-level log line names the action, and nothing else happens.

---

## Customizing

Add `[[keyboard.bindings]]` tables to `alacritty.toml` or `alacritree.toml` under your config directory (typically `~/.config/alacritty/`). Both files are deep-merged, so alacritree-specific overrides can live in `alacritree.toml` without touching the alacritty config.

```toml
# Example: bind Ctrl+Shift+T to open a new session, and disable Cmd+M on macOS.
[[keyboard.bindings]]
key = "T"
mods = "Control|Shift"
action = "CreateNewTab"

[[keyboard.bindings]]
key = "M"
mods = "Command"
action = "None"
```

Modifier names: `Control` / `Ctrl`, `Shift`, `Alt` / `Option`, `Super` / `Command` / `Meta`. Combine with `|`.

For raw byte sequences, use `chars = "..."` instead of `action`:

```toml
[[keyboard.bindings]]
key = "Return"
mods = "Alt"
chars = "\r"   # ESC + CR, meta-Enter for a tmux prefix
```
