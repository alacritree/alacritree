For bug reports, describe what happened and what you expected to happen instead. The details below help reproduce it.

### System

OS: Linux/macOS/Windows (and the WSL distro, if the project lives in one)
Version: `alacritree --version` output
Linux: X11/Wayland, DE or compositor

### Diagnostics

`alacritree doctor` output.

Crashes: `alacritree crashes` lists the crash logs alacritree kept. Attach the one for this crash.

Logs: run with `RUST_LOG=debug`, or pass `--log-file <path>` to write the log to a file. A release build started from a shortcut has no console, so `--log-file` or `[debug] persistent_logging = true` is how to capture it.
