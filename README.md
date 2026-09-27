<p align="center">
    <img width="200" alt="Alacritree Logo" src="alacritree/assets/icon.png">
</p>

<h1 align="center">Alacritree</h1>

<p align="center">
    A native terminal that turns Git worktrees into first-class workspaces, built on <a href="https://github.com/alacritty/alacritty">Alacritty</a>.
</p>

## About

The first ultrafast, FOSS alternative to the LLM/worktree management apps cropping up everywhere. Built around the [Alacritty] terminal emulator and drop-in compatible with your `alacritty.toml`.

Minimalist approach, with the terminal at the center:

- **Worktree management.** The sidebar lists projects and their worktrees, and one click opens a shell in one. A new worktree takes seconds and gets a copy of the project's AI assistant configs, such as `CLAUDE.md`, `AGENTS.md` and `.cursor/`.
- **Sessions per workspace.** Each worktree keeps its own terminal sessions. Switching worktrees leaves them running, scrollback and all.
- **Git status panel.** The right sidebar shows the branch, staged and unstaged files, and the changes against the base branch, refreshed in the background. With [`gh`][gh] the base is the open PR's base branch.
- **Branch diffs.** Clicking a file opens its diff in [Delta], in [tuicr] for a review whose comments agents can read, or in a command of your own.
- **Workspace scratchpads.** `Ctrl+Backtick` opens a minimal Markdown editor with one file per workspace. It saves every change, and agents read it over MCP.
- **Task lists.** An opt-in tab shows the workspace's [taskwarrior] lists, the same ones its agents write with `task`.
- **Multiplexer panes.** Agents running under [herdr] appear in the sidebar with their status, and [zellij] panes can too. Opening a row attaches a session to the pane.
- **Checkout hooks.** Commands of your own run when a worktree is created, opened or removed. Doppler scopes follow the main checkout into every worktree.
- **Scriptable.** An MCP server and a CLI reach everything the sidebar does.

No Chromium, no bundled agents, no telemetry. No company behind it, and there never will be.

[Alacritty]: https://github.com/alacritty/alacritty
[Delta]: https://github.com/dandavison/delta
[gh]: https://cli.github.com
[tuicr]: https://github.com/agavra/tuicr
[taskwarrior]: https://taskwarrior.org
[herdr]: https://github.com/herdrdev/herdr
[zellij]: https://zellij.dev

## Screenshots

https://github.com/user-attachments/assets/c0b0aa23-59f1-49d3-a3aa-dcdf1eff7363

## Install

Every release publishes builds for Linux, macOS and Windows at <https://github.com/alacritree/alacritree/releases>. On Windows, Alacritree also runs projects that live inside a WSL distro.

### Linux

Arch users have two AUR packages:

- `alacritree-bin` installs the prebuilt binary from the latest release, for `x86_64` and `aarch64`, with no Rust toolchain needed.
- `alacritree-git` compiles the latest `master` locally.

```sh
yay -S alacritree-bin      # or alacritree-git
```

On any other distro, take the tarball from the latest release:

```sh
arch=x86_64  # or aarch64
curl -fLO "https://github.com/alacritree/alacritree/releases/latest/download/alacritree-${arch}-unknown-linux-gnu.tar.gz"
tar -xzf "alacritree-${arch}-unknown-linux-gnu.tar.gz"
install -Dm755 "alacritree-${arch}-unknown-linux-gnu/alacritree" ~/.local/bin/alacritree
```

### macOS

```sh
brew tap alacritree/alacritree https://github.com/alacritree/alacritree
brew install alacritree
```

The formula lives in [`Formula/alacritree.rb`](Formula/alacritree.rb) and is bumped on every release. It ships only `aarch64-apple-darwin`. Intel Macs take `Alacritree-x86_64-apple-darwin.app.tar.gz` from the release, which holds an app bundle, or use the shell installer below.

### Linux and macOS shell installer

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/alacritree/alacritree/releases/latest/download/alacritree-installer.sh | sh
```

It installs the binary for your platform into `~/.cargo/bin`.

### Windows

```powershell
scoop bucket add alacritree https://github.com/alacritree/alacritree
scoop install alacritree
```

The manifest lives in [`bucket/alacritree.json`](bucket/alacritree.json) and is bumped on every release. The release zip works without Scoop too. Keep `conpty.dll` and `OpenConsole.exe` next to `alacritree.exe`, because Alacritree loads its console host from its own directory.

### From source

See the [Build](#build) section.

## Build

The minimum Rust version is the `rust-version` in the root `Cargo.toml`. System packages required on Debian/Ubuntu:

```sh
sudo apt install \
    cmake pkg-config \
    libfreetype6-dev libfontconfig1-dev \
    libxkbcommon-dev libxcb-shape0-dev libxcb-xfixes0-dev \
    libwayland-dev libgl1-mesa-dev libegl1-mesa-dev
```

On macOS, install `cmake`, `pkg-config`, `fontconfig` and `freetype` through Homebrew. Windows builds with the MSVC Rust toolchain.

Then:

```sh
cargo run                # debug
cargo build --release    # target/release/alacritree
```

`alacritree install` copies the binary it runs from into `~/.local/bin`, and `--dest` picks another directory.

## Configuration

Alacritree reads the same files Alacritty does, in the same order. On Linux and macOS:

1. `$XDG_CONFIG_HOME/alacritty/alacritty.toml`
2. `$XDG_CONFIG_HOME/alacritty.toml`
3. `$HOME/.config/alacritty/alacritty.toml`
4. `$HOME/.alacritty.toml`
5. `/etc/alacritty/alacritty.toml`

On Windows it reads `%APPDATA%\alacritty\alacritty.toml`.

After loading `alacritty.toml`, Alacritree deep-merges an optional `alacritree.toml` from the same locations on top. The merge follows Alacritty's rules. Arrays concatenate, so `[[keyboard.bindings]]` in `alacritree.toml` adds to the upstream bindings instead of replacing them. Tables merge recursively, and primitives replace.

Alacritree-only options live in `alacritree.toml`, under `[ui]`, `[workspace]` and `[integrations]`. [`docs/config-reference.md`](docs/config-reference.md) lists every key with its type, default and effect. `alacritree schema init` adds a header pointing the file at the published JSON Schema, so editors that run the TOML language server complete and validate it.

## MCP server

Alacritree can be driven by an LLM agent over the [Model Context Protocol](https://modelcontextprotocol.io). `alacritree mcp` starts a stdio MCP server that talks to the running app. Through it an agent can list your projects and worktrees, open shells in them, type into terminals, read their output, inspect git status, create worktrees and attach multiplexer panes. Register it with any MCP client:

```sh
claude mcp add alacritree -- alacritree mcp
```

An agent running inside an Alacritree session targets its host instance on its own, through the `ALACRITREE_SOCKET` environment variable. Other clients can pass `alacritree mcp --socket <path>`. The transport mirrors Alacritty's IPC design, and `ipc_socket = false` under `[general]` turns it off. See [`docs/alacritree.md`](docs/alacritree.md#mcp-server-to-drive-alacritree-from-an-llm) for the full tool list.

## Command line

The CLI covers the same operations as the MCP server, for agents that shell out instead, and for setting Alacritree up without the folder picker:

```sh
alacritree project add ~/Git/myrepo     # also: list, remove, refresh, rename
alacritree worktree create ~/Git/myrepo my-feature
alacritree git-status ~/Git/myrepo

alacritree session create --workspace ~/Git/myrepo   # prints the new session id
alacritree session send-text 3 'cargo test' --enter
alacritree session read-screen 3
```

`send-text` types the text and `--enter` submits it. A shell passes arguments through verbatim, so a trailing `\r` in the text would arrive as a backslash and an `r`.

Commands print a short human summary. `--json` prints the raw reply instead, which is what a script or an agent wants.

Anything that needs a window, such as sessions or workspace selection, requires a running Alacritree. The rest do not. With no instance listening, projects, git status and worktree creation are served straight from `state.toml` and git, so an agent can set Alacritree up before anyone has opened it.

`alacritree completions <shell>` writes a completion script to stdout, and `alacritree --help` lists the other subcommands.

### Diagnosing a setup

```sh
alacritree doctor          # --json for the machine-readable form
```

Alacritree degrades quietly on purpose. A missing `gh` falls back to the repo's default branch, a missing `doppler` skips scope mirroring, a malformed `alacritty.toml` loads defaults, and a corrupt `state.toml` opens an empty sidebar. Each is the right call on its own, since none of them should stop a terminal from opening. Together they make a broken setup look much like a working one.

`doctor` reports the external tools it found with their versions and paths, which config files were loaded and whether they parse, whether the persisted projects still exist on disk, and whether a running instance is reachable. It needs no running window, because "nothing happens when I run it" is exactly when it gets used.

On Windows it also reports each installed WSL distro and where `git`, `gh`, `delta` and `doppler` resolve inside it. Those are the paths Alacritree uses for a project that lives in a distro, and nothing else ever names them. A distro without `git` shows an empty git panel rather than an error.

It exits non-zero only when something is broken. A missing optional tool is a warning. A tool driving a feature you never turned on, like an absent `doppler` on a machine with no Doppler config, is not even that. A report that always carries a warning is a report nobody reads.

## Documentation

- [`docs/alacritree.md`](docs/alacritree.md) is the full feature reference: workspaces and sessions, the project and worktree sidebar, checkout hooks, task lists, herdr agents, the git status panel, the terminal grid, the two-file config model, the MCP server, and how Alacritree compares with other tools in the space.
- [`docs/config-reference.md`](docs/config-reference.md) lists every config key with its type, default and effect. It is generated from the same doc comments as the JSON Schema.
- [`docs/keyboard-shortcuts.md`](docs/keyboard-shortcuts.md) lists every key binding the app understands, the `action = "..."` values `[[keyboard.bindings]]` accepts, and which Alacritty actions are intentionally not wired up.
- [`docs/features.md`](docs/features.md) is upstream Alacritty's feature overview, covering vi mode, search, hints and selection expansion. It is kept for reference, and not everything in it is implemented in Alacritree.

## Repository layout

This is a Cargo workspace:

- `alacritree/` is the app: the GUI, the sidebars, the CLI and the MCP server.
- `crates/` holds the app's library crates. Each integration type, such as version control or multiplexers, has one crate for its trait and one per backend.
- `alacritty_terminal/` is vendored from upstream Alacritty and used as a library for the PTY, the VT parser and the grid.
- `alacritty/`, `alacritty_config/` and `alacritty_config_derive/` are vendored upstream crates and read-only here. The upstream `alacritty` GUI binary is not what this fork ships.
- `egui-winit/` is a vendored `egui-winit` with a one-line change.

## Relationship to upstream Alacritty

Alacritree is not a competitor to or replacement for Alacritty. It depends on upstream's terminal crate and would not exist without it.

## License

Released under the [Apache License, Version 2.0](LICENSE-APACHE), matching upstream Alacritty.

The bundled Alacritree Symbols font is a subset of DejaVu and remains under the [Bitstream Vera license](alacritree/assets/FONT-LICENSE.txt). Run `alacritree --licenses` to print its complete notice from an installed binary.
