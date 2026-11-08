# Homebrew packaging

Alacritree's Homebrew formula lives in this repository, at [`Formula/alacritree.rb`](../../Formula/alacritree.rb), rather than in a separate `homebrew-alacritree` tap. Homebrew only taps a repository by its short name when it is called `homebrew-<name>`, so users tap this one by URL once:

```sh
brew tap alacritree/alacritree https://github.com/alacritree/alacritree
brew install alacritree
```

After that, `brew upgrade alacritree` picks up new releases.

The formula installs the Apple Silicon binary only. Releases carry an `x86_64-apple-darwin` archive too, but the update workflow stamps a single sha256, so on an Intel Mac the install fails rather than downloading nothing.

## Updates

[`.github/workflows/homebrew-update.yml`](../../.github/workflows/homebrew-update.yml) bumps the formula's version and sha256 through an auto-merging PR, the same way `scoop-update.yml` bumps the Scoop manifest. release-please publishes each GitHub Release before its assets are attached, so the `release: published` run finds no archive and skips. The bump that lands is the one `publish-downstream.yml` dispatches once the release pipeline has attached the assets. The shared `ALACRITREE_BOT_TOKEN` secret covers it; Homebrew needs no secret of its own.

## Formula or cask

Releases ship `Alacritree-<target>.app.tar.gz` bundles for both macOS architectures beside the bare binaries, so a cask installing the `.app` is possible. [`release-macos-app.yml`](../../.github/workflows/release-macos-app.yml) builds them, and only the bundle gets desktop notifications. No cask exists yet, and the formula installs the bare binary into `bin`.
