# UI catalog

Run from the workspace root:

```sh
cargo run --bin ui_catalog
```

Search the page list, resize the preview canvas, and change the font size to
inspect compact and enlarged layouts. Hover controls for their tooltips.

## Coverage

| Page | Visual states and interactions |
| --- | --- |
| Foundations | Theme colors, typography, buttons, path labels, loaders |
| Icons and badges | Navigation and action icons, PR and upstream badges |
| Status indicators | Agent lifecycle, attention, busy shells, dots and symbols |
| Project sidebar | Expandable project headers, reorder grip, home and session rows |
| Worktrees and PRs | Active, focused, missing, deleting and creating checkouts; PR states; upstream tracking; context menus |
| Multiplexer panes | Detached herdr agent states, plain shells, shared Zellij panes, attached sessions |
| Search and navigation | Fuzzy search, filter combinations, search scope, focus outline, floating and solid scrollbars |
| Session tabs | Active and attention segments, switching tabs, adding sample tabs, profile menu |
| Git sidebar | Branch/base labels, review controls, section counts, file-change kinds and diff stats |
| Command palette | Search and ranking, section headers, selection, action shortcuts, session and workspace rows |
| Tasks | Editable lists, nested tasks, completion, folding, row menus, drag handles and docked progress |
| Scratchpad | Empty notes, editable workspace notes and autosave errors |
| Dialogs | Create, rename, delete, prune, close, detach, quit, error, base branch, progress and success samples |
| Terminal typography | ANSI colors, regular/bold/italic font faces, Unicode and symbol samples |
| Activity row | Running, successful and failed background activity |

The catalog uses the app's shared row, tab, editor and control painters with
fixed sample data and default configuration. Edits to tasks and notes live only
in memory. Sample tabs and confirmations never start shells or modify projects.

Dialog samples combine the production frames, buttons and dirty-state warnings
with representative content. They do not run the full application workflows.
Terminal samples show font and palette choices; the GPU terminal grid, cursor,
selection, terminal decorations, OS notifications and native folder picker still
need inspection in the main application.

## Checks

```sh
cargo test -p alacritree --lib app::catalog::tests
cargo test -p alacritree --lib tasks::view::preview::tests
cargo check --all-targets
```

The catalog tests cover font-scale round trips, readable status and palette labels,
activity-panel IDs, every page at narrow and wide sizes across three font sizes,
opening and dismissing every sample dialog, and scratchpad typing and focus.
