# AUR packaging

Two packages live in the [Arch User Repository](https://aur.archlinux.org/):

- **`alacritree-git`** is the VCS package. It builds from the latest `master`, and `pkgver()` derives the version from `git describe --tags`, so it counts up from the newest release tag. [`.github/workflows/aur-publish.yml`](../../.github/workflows/aur-publish.yml) syncs it to AUR on every push to `master`. The deploy action does nothing when the PKGBUILD is byte-identical to what is already on AUR, so users only see commits when the recipe changes.

- **`alacritree-bin`** is the prebuilt-binary package. It downloads the `alacritree-x86_64-unknown-linux-gnu.tar.gz` and `alacritree-aarch64-unknown-linux-gnu.tar.gz` archives from the matching GitHub release. The archives carry no desktop metadata, so the `.desktop` file and icons come from the repository at that release's tag. [`.github/workflows/aur-bin-publish.yml`](../../.github/workflows/aur-bin-publish.yml) stamps the PKGBUILD with the release version and asset sha256 sums and hands it to the deploy action.

## One-time setup

1. **Generate an SSH key dedicated to AUR** (no passphrase, so the Actions runner can use it non-interactively):

   ```sh
   ssh-keygen -t ed25519 -f ~/.ssh/aur -C 'aur@alacritree-ci' -N ''
   ```

2. **Register the public key on AUR**: copy `~/.ssh/aur.pub` into your account's *SSH Public Key* field at <https://aur.archlinux.org/account/>.

3. **Reserve both package names** by pushing an initial commit yourself (the GitHub Actions assume the AUR repos already exist). Repeat the block below for each of `alacritree-git` and `alacritree-bin`:

   ```sh
   pkg=alacritree-git   # then repeat with pkg=alacritree-bin
   GIT_SSH_COMMAND='ssh -i ~/.ssh/aur' \
     git clone ssh://aur@aur.archlinux.org/$pkg.git /tmp/aur-$pkg
   cp packaging/aur/$pkg/PKGBUILD /tmp/aur-$pkg/
   cd /tmp/aur-$pkg
   makepkg --printsrcinfo > .SRCINFO
   git add PKGBUILD .SRCINFO
   git commit -m 'Initial import'
   GIT_SSH_COMMAND='ssh -i ~/.ssh/aur' git push
   cd -
   ```

4. **Expose the private key to Actions** as a repository secret named `AUR_SSH_PRIVATE_KEY` (Settings > Secrets and variables > Actions). Paste the full contents of `~/.ssh/aur` including the BEGIN/END lines.

Subsequent updates are automatic:

- `alacritree-git` is pushed on every commit to `master`, and reaches AUR users only when its PKGBUILD changed.
- `alacritree-bin` follows each release. release-please publishes the GitHub Release before any assets are attached, so the `release: published` run finds none and skips. The run that publishes is the one `publish-downstream.yml` dispatches once the assets are attached. A release that has assets but is missing either Linux archive fails the workflow instead of publishing a broken package.
