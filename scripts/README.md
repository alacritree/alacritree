# Scripts

## Flamegraph

Runs a release build of alacritree while `perf` records call stacks. When alacritree exits, `cargo-flamegraph` writes the flamegraph. Arguments are passed through to alacritree.

```sh
./create-flamegraph.sh
```

It needs `perf`, so it runs on Linux only. It installs `cargo-flamegraph` if it is missing, and offers to uninstall it afterwards.

## ANSI color tests

Scripts for checking how a terminal draws color. `fg-bg.sh` prints foreground and background combinations, `colors.sh` every pairing of the eight foreground and background colors under each text attribute, and `24-bit-color.sh` a truecolor gradient.

```sh
./fg-bg.sh
./colors.sh
./24-bit-color.sh
```
