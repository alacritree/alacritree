`alacritree` is an egui/eframe terminal that hosts `alacritty_terminal` sessions behind a sidebar of projects and their checkouts, such as git worktrees. Most modules open with a `//!` header giving their purpose and the invariants they hold. Read it before editing the module.

## Repository layout

This is a Cargo workspace. Edit `alacritree/` (the app) and `crates/` (its library crates) unless the user says otherwise.

- `alacritty/`, `alacritty_terminal/`, `alacritty_config/` and `alacritty_config_derive/` are vendored upstream alacritty and read-only. Only `alacritty_terminal` is used, for the PTY, VT parser and grid. The `alacritty` GUI binary is not what this fork ships.
- `egui-winit/` is a vendored `egui-winit` with a one-line change, wired in through `[patch.crates-io]` in the root `Cargo.toml`. Leave the `x11-clipboard` pin beside it alone unless asked.
- `vte/` is a vendored `vte` whose `ansi::Handler` gains `unhandled_osc`, wired in through `[patch.crates-io]`. Nothing else in it changes. Run its tests with `cargo nextest run -p vte`.
- `CONTRIBUTING.md` and the root `Makefile` are upstream alacritty's and do not govern `alacritree/`.

## Build and test

```sh
cargo run
cargo check
cargo test
cargo +nightly fmt   # rustfmt.toml needs nightly
```

From the workspace root these cover `alacritree/` and `crates/` and skip the vendored crates, through `default-members` in `Cargo.toml` and `ignore` in `rustfmt.toml`. `.github/workflows/ci.yml` holds the clippy flags CI runs.

## Architecture

- The app owns many PTY sessions and paints their grids itself. A `WorkspaceKey` is `Option<PathBuf>`: `None` is the home tab, `Some(path)` a checkout. Each workspace remembers its active session.
- Sessions outlive workspace switches, on screen or not. A session leaves only through `SessionList::remove`, and a close that changes focus goes through `AlacritreeApp::close_sessions`.
- PTY output wakes the UI only through `EventProxy`, which calls `request_repaint` when its session is on screen. A background thread that produces terminal events without it looks hung until the next input event.
- The CLI, the MCP server (`alacritree mcp`) and a running window all speak `ipc::protocol::IpcRequest`. A new operation is one `IpcRequest` variant, placed once in `ipc/route.rs`. MCP tool names match its serde tags. With no window listening, the CLI falls back to `cli/offline.rs`.
- `alacritree_common::wsl` is the only code that knows WSL exists.

## Integrations

Integrations such as version control, forges, multiplexers, tasks and checkout hooks each follow one shape:

- One crate per integration type holds the trait, shared models, a `thiserror` error type and a test fake (`alacritree_vcs`, `alacritree_multiplexer`, ...). One crate per backend implements it (`alacritree_git`, `alacritree_zellij`, ...). Only the `alacritree` app depends on backend crates.
- The trait is `#[ambassador::delegatable_trait]` and the app dispatches through a `#[derive(ambassador::Delegate)]` enum. `enum_dispatch` cannot link a trait and an enum in different crates, and `Box<dyn>` is not used. Trait signatures name types by absolute path, because ambassador copies them into the deriving crate. Closed sets of names use strum derives.
- A backend's config section lives in its crate. The app's `RawIntegrations` names it with one field.

## Config

`config.rs` loads `alacritty.toml` and deep-merges `alacritree.toml` over it with alacritty's semantics: arrays concatenate, tables merge, primitives replace. So `[[keyboard.bindings]]` in `alacritree.toml` adds to the upstream bindings.

To add a config key:

1. Put it in `alacritty.toml` when alacritty itself reads it, and in `alacritree.toml` otherwise.
2. Document it with a doc comment on its `Raw*` struct, in `config.rs` or in the integration crate that owns the section. The comment becomes the JSON Schema's hover text and says what the setting does, never its default.
3. Set its default only in the `Raw*` type's `Default` impl under `#[serde(default)]`. A key with no fixed default goes in `alacritree/tests/schema-defaults-allowlist.txt`.
4. Regenerate the schema. The key is done when the plain `config_schema` test passes, since it fails while `schema/alacritree-config.json` or `docs/config-reference.md` is stale.

```sh
ALACRITREE_UPDATE_SCHEMA=1 cargo test -p alacritree --test config_schema
```

## Conventions

- Mirror upstream alacritty. Before implementing input, config parsing, terminal behavior, key bindings, clipboard, scrolling or selection, read how `alacritty/` does it and follow that. This fork swaps the renderer and otherwise behaves like alacritty. Justify an unavoidable divergence in a comment.
- Errors are `thiserror` types, so a caller matches on a variant and the source `io::Error` or exit status travels with it. An error becomes text only where it is shown: the error dialog, a CLI line, or an IPC reply.
- Comments explain why, briefly, in the style of the existing module headers. Keep existing comments unless the change makes them wrong.
- Commits follow [Conventional Commits](https://www.conventionalcommits.org/) with an optional scope (`feat(sidebar):`), imperative subject under about 72 characters.
