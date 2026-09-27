# Homebrew packaging

Alacritree's Homebrew formula lives in **this repo** —
[`Formula/alacritree.rb`](../../Formula/alacritree.rb) — rather than a
dedicated `homebrew-alacritree` tap repo. That keeps everything in one
place at the cost of one extra step for users:

```sh
brew tap alacritree/alacritree https://github.com/alacritree/alacritree
brew install alacritree
```

(Homebrew's auto-tap only fires when the repo is named
`homebrew-<name>`. Since this repo isn't, users have to tap by URL
once. After that, `brew upgrade alacritree` Just Works.)

[`.github/workflows/homebrew-update.yml`](../../.github/workflows/homebrew-update.yml)
bumps the formula via an auto-merging PR on every published release —
exactly the same shape as `scoop-update.yml`. No Homebrew-specific
secret is required; the shared `ALACRITREE_BOT_TOKEN` covers it.

## Why a formula, not a cask?

The formula installs the bare binary from the macOS release tarball. Releases also carry `Alacritree.app`, built by [`release-macos-app.yml`](../../.github/workflows/release-macos-app.yml), and only the bundle gets desktop notifications. A cask that installs that bundle would replace the formula, and none exists yet.
