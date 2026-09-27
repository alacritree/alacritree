#!/bin/sh
# Assemble Alacritree.app around the release binary.  A bundle is required on
# macOS: UNUserNotificationCenter (desktop notifications + click-to-focus)
# refuses to run in a process without a bundle identifier.  The inner binary
# stays terminal-launchable via Alacritree.app/Contents/MacOS/alacritree.
#
# Mirrors the root Makefile's `app` target (upstream alacritty's bundling),
# minus man pages, completions, and terminfo.
set -e

root="$(cd "$(dirname "$0")/../../.." && pwd)"
template="$root/alacritree/extra/osx/Alacritree.app"
app_dir="$root/target/release/osx"
app="$app_dir/Alacritree.app"

cargo build --manifest-path "$root/Cargo.toml" -p alacritree --release

rm -rf "$app"
mkdir -p "$app_dir"
cp -R "$template" "$app_dir/"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$root/target/release/alacritree" "$app/Contents/MacOS/"

# The icon is built from the PNGs the Linux desktop entry installs, so every
# platform shows the same one.  Retina slots take the next size up; the
# largest has no PNG of its own and is scaled down from the master icon.
assets="$root/alacritree/assets"
iconset="$app_dir/alacritree.iconset"
rm -rf "$iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    cp "$assets/icon-$size.png" "$iconset/icon_${size}x${size}.png"
done
for size in 16 32 128 256; do
    cp "$assets/icon-$((size * 2)).png" "$iconset/icon_${size}x${size}@2x.png"
done
sips -z 1024 1024 "$assets/icon.png" --out "$iconset/icon_512x512@2x.png" >/dev/null
iconutil -c icns "$iconset" -o "$app/Contents/Resources/alacritree.icns"
rm -rf "$iconset"

codesign --force --deep --sign - "$app"
echo "Created $app"
