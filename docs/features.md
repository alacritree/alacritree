# Terminal features

Alacritree runs its terminals on `alacritty_terminal`, the PTY, VT parser and grid inside Alacritty, so escape sequence support is Alacritty's. See [Alacritty's escape sequence support](https://github.com/alacritty/alacritty/blob/master/docs/escape_support.md) for the list.

Alacritree adds to that set. It draws images sent with the kitty graphics protocol or as sixel, see [Images](./alacritree.md#images). It also reads OSC sequences that Alacritty drops: notifications, progress, the working directory and the pointer shape. Each one has a `[vt]` key in the [config reference](./config-reference.md).

The features Alacritty builds on top of that grid live in its own window code, which alacritree does not use. This page says which of them alacritree has. For the worktree, session and sidebar features alacritree adds, see [`alacritree.md`](./alacritree.md).

## Selection

Mouse selection works the way it does in Alacritty:

- Dragging selects text. Holding <kbd>Ctrl</kbd> when the drag starts makes a block selection.
- Double-clicking selects a word, bounded by `[selection] semantic_escape_chars`.
- Triple-clicking selects whole lines.
- Right-clicking moves the far edge of the current selection to the click.

A finished selection is copied to the primary selection, which is the system clipboard on platforms without one. `[selection] save_to_clipboard` copies it to the clipboard as well. `Copy` and `CopySelection` copy it on demand, see [Keyboard shortcuts](./keyboard-shortcuts.md#clipboard).

When an application has turned on mouse reporting, clicks and drags go to it instead. Hold <kbd>Shift</kbd> to select text anyway.

Double and triple clicks always start a new selection rather than expanding the current one, and <kbd>Ctrl</kbd> with a right-click does not switch an existing selection to block mode.

## Opening links

Clicking a link opens it with the system handler. OSC 8 hyperlinks take priority, and plain text is matched against Alacritty's default URL pattern. The modifiers and the program that opens a link are fixed. See [Clickable links](./alacritree.md#clickable-links) for the details.

## Not implemented

- **Vi mode.** There is no keyboard cursor mode, and bindings with a `mode` are dropped.
- **Search.** There is no scrollback search. The `/` search in the sidebars filters rows, not terminal text.
- **Hints.** The `[hints]` config section is not read. The URL click above is the only built-in pattern.
- **Multiple windows.** Alacritree has one window. `CreateNewWindow` opens a new session in it, and there is no `create-window` subcommand.
