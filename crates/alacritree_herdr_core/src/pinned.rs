//! The socket a distro's `hc` reaches its core at, once a refusal named it.
//!
//! Every script that runs a distro's `hc` gets the socket through here, so
//! the resident connection, the helper's one-shot calls and the keeper all
//! reach the same core.

use std::process::Command;

use alacritree_common::side;
use alacritree_common::wsl_helper::{HelperClient, TransportError};

/// `sh` code that exports its first argument as `HC_SOCKET` unless it is
/// empty, then shifts it off.
const PIN: &str = r#"[ -z "$1" ] || export HC_SOCKET="$1"; shift; "#;

/// A command that runs the `sh` code `script` in `distro` with `$p` set to
/// `hc`, and with `HC_SOCKET` set to `socket` unless it is `None`.
pub(crate) fn command(distro: &str, hc: &str, socket: Option<&str>, script: &str) -> Command {
    let mut command = side::wsl_script_with_program(distro, hc, &format!("{PIN}{script}"));
    command.arg(socket.unwrap_or_default());
    command
}

/// `script` with `args` through the distro's helper, with `HC_SOCKET` set as
/// [`command`] sets it.
pub(crate) fn run(
    client: &HelperClient,
    socket: Option<&str>,
    script: &str,
    args: &[&str],
) -> Result<(i32, Vec<u8>), TransportError> {
    let args: Vec<&str> =
        std::iter::once(socket.unwrap_or_default()).chain(args.iter().copied()).collect();
    client.run(&format!("{PIN}{script}"), &args)
}
