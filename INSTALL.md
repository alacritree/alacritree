# Installing Alacritree

Every release publishes prebuilt binaries for Linux (`x86_64`, `aarch64`), macOS (Apple Silicon, Intel) and Windows (`x86_64`) on the [releases page](https://github.com/alacritree/alacritree/releases). Building from source is covered [further down](#building-from-source).

Alacritree reads your existing `alacritty.toml`, so there is nothing to configure before the first launch.

## Prebuilt binaries

### Linux

On Arch, install from the AUR. `alacritree-bin` repackages the release binary and `alacritree-git` compiles the latest `master`. Both install the desktop entry and icons.

```sh
yay -S alacritree-bin
```

Elsewhere, the shell installer downloads the release binary for your architecture into `~/.cargo/bin` (or `$CARGO_HOME/bin`):

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/alacritree/alacritree/releases/latest/download/alacritree-installer.sh | sh
```

To place the binary yourself, take the tarball for your architecture:

```sh
arch=x86_64   # or aarch64
curl -fLO "https://github.com/alacritree/alacritree/releases/latest/download/alacritree-${arch}-unknown-linux-gnu.tar.gz"
tar -xzf "alacritree-${arch}-unknown-linux-gnu.tar.gz"
install -Dm755 "alacritree-${arch}-unknown-linux-gnu/alacritree" ~/.local/bin/alacritree
```

The shell installer and the tarball carry only the binary. Add the [desktop entry](#desktop-entry-linux) yourself if you want Alacritree in your application launcher. The tarball also holds `conpty.dll` and `OpenConsole.exe`, which only the Windows build uses.

### macOS

Take `Alacritree.app` from the release. macOS delivers desktop notifications, and focuses the right tab when you click one, only to a process with a bundle identifier, so the app bundle is the one to use if you want notifications.

```sh
arch=aarch64   # or x86_64 on an Intel Mac
curl -fLO "https://github.com/alacritree/alacritree/releases/latest/download/Alacritree-${arch}-apple-darwin.app.tar.gz"
tar -xzf "Alacritree-${arch}-apple-darwin.app.tar.gz" -C /Applications
```

The bundle is ad-hoc signed, not notarized. `curl` does not mark the download as quarantined, but a copy downloaded through a browser is, and macOS refuses to open it until you clear the flag:

```sh
xattr -dr com.apple.quarantine /Applications/Alacritree.app
```

The command line lives inside the bundle. Link it onto your `PATH` to run `alacritree doctor` and the other subcommands from a shell:

```sh
ln -s /Applications/Alacritree.app/Contents/MacOS/alacritree ~/.local/bin/alacritree
```

Homebrew installs the bare binary, without the app bundle, and only for Apple Silicon. It also pulls in `git` and `git-delta`.

```sh
brew tap alacritree/alacritree https://github.com/alacritree/alacritree
brew install alacritree
```

The [shell installer](#linux) works on macOS too, with the same bare-binary limitation.

### Windows

Scoop installs the release, adds a Start menu shortcut, and pulls in `git` and `delta`:

```powershell
scoop bucket add alacritree https://github.com/alacritree/alacritree
scoop install alacritree
```

Without Scoop, download `alacritree-x86_64-pc-windows-msvc.zip` from the [latest release](https://github.com/alacritree/alacritree/releases/latest), extract it to a folder of your choice and add that folder to your `PATH`.

Keep `alacritree.exe`, `conpty.dll` and `OpenConsole.exe` in the same folder. Alacritree loads the console host from its own directory, and without those two files every terminal falls back to the console host built into Windows, which is slower.

## Building from source

### Toolchain

Install Rust through [rustup](https://rustup.rs). The minimum version is the `rust-version` in the root `Cargo.toml`. The latest stable toolchain always works.

### System libraries

Debian and Ubuntu:

```sh
sudo apt install \
    cmake pkg-config \
    libfreetype6-dev libfontconfig1-dev \
    libxkbcommon-dev libxcb-shape0-dev libxcb-xfixes0-dev \
    libwayland-dev libgl1-mesa-dev libegl1-mesa-dev
```

Arch:

```sh
sudo pacman -S --needed git rust cmake pkgconf fontconfig freetype2 libxkbcommon libxcb wayland libglvnd
```

On other distributions, install the development packages for the same libraries: FreeType, Fontconfig, xkbcommon, XCB, Wayland and the OpenGL/EGL headers, plus CMake and pkg-config.

macOS:

```sh
brew install cmake pkg-config fontconfig freetype
```

Windows needs only the MSVC toolchain that rustup installs by default, which in turn needs the C++ build tools from Visual Studio.

### Build

```sh
git clone https://github.com/alacritree/alacritree.git
cd alacritree
cargo build --release --locked -p alacritree
```

The binary lands at `target/release/alacritree`. On Windows the build also copies `conpty.dll` and `OpenConsole.exe` from `alacritree/vendor/conpty` next to `target\release\alacritree.exe`.

### Install the build

On Linux, `alacritree install` copies the binary into `~/.local/bin`, or into the directory `--dest` names. It replaces a copy that is still running without disturbing it, so it is safe to run while Alacritree is open.

```sh
target/release/alacritree install
```

On macOS, build the app bundle instead. The script builds a release binary, wraps it in `target/release/osx/Alacritree.app` and ad-hoc signs it.

```sh
./alacritree/extra/osx/make-app.sh
cp -R target/release/osx/Alacritree.app /Applications/
```

On Windows, `alacritree install` copies `alacritree.exe` alone, which leaves the faster console host behind. Copy the three files from `target\release` into a folder on your `PATH` instead, as with the [release zip](#windows).

## After installing

### Desktop entry (Linux)

The AUR packages install these for you. For any other install, run this from a clone of the repository:

```sh
install -Dm644 alacritree/assets/alacritree.desktop ~/.local/share/applications/alacritree.desktop
for size in 16 24 32 48 64 128 256 512; do
    install -Dm644 "alacritree/assets/icon-${size}.png" \
        "$HOME/.local/share/icons/hicolor/${size}x${size}/apps/alacritree.png"
done
```

The entry launches `alacritree` by name, so the binary has to be on the `PATH` your desktop session sees, not only the one your shell sets.

### Shell completions

`alacritree completions <shell>` prints a completion script for `bash`, `zsh`, `fish`, `elvish` or `powershell`.

```sh
# Bash
alacritree completions bash > ~/.local/share/bash-completion/completions/alacritree

# Zsh: any directory on your $fpath works
alacritree completions zsh > ~/.zfunc/_alacritree

# Fish
alacritree completions fish > ~/.config/fish/completions/alacritree.fish
```

For PowerShell, add this line to your `$PROFILE`:

```powershell
alacritree completions powershell | Out-String | Invoke-Expression
```

### Terminfo

Alacritree sets `TERM=alacritty` when the `alacritty` terminfo entry is installed and falls back to `xterm-256color` otherwise. If Alacritty is already installed, you have the entry. Otherwise, install it from a clone of the repository:

```sh
tic -xe alacritty,alacritty-direct extra/alacritty.info
```

### External tools

Alacritree shells out to other programs for parts of the UI:

- `git` runs worktree operations and feeds the diff view.
- `delta` renders branch diffs.
- `gh` finds the pull request a branch belongs to, so the git panel diffs against its base.
- `doppler` copies a project's Doppler scopes into each new worktree, and matters only for projects that use Doppler.

The optional integrations (Taskwarrior, herdr, zellij, tuicr) call their own tools. Alacritree opens without any of these, and the feature a missing tool drives quietly does less. Run `alacritree doctor` to see which tools it found, which config files it loaded, and whether they parse.

### Configuration

Alacritree reads `alacritty.toml` from the same places Alacritty does and layers its own options from `alacritree.toml` on top. See [`docs/alacritree.md`](docs/alacritree.md) for how the two files combine and [`docs/config-reference.md`](docs/config-reference.md) for every option.
