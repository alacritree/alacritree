//! hc's official installer for a side: the shell script inside a distro and
//! on Unix, the PowerShell script on Windows. The commands are the ones
//! herdr-core's README gives, so the user reads exactly what runs.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use alacritree_common::jobs::Blocking;
use alacritree_common::side::{self, Side};
use alacritree_common::{command_ext, wsl};

const SHELL: &str = "curl -fsSL https://releases.herdr.dev/core/install.sh | sh";
const POWERSHELL_SCRIPT: &str = "irm https://releases.herdr.dev/core/install.ps1 | iex";
const POWERSHELL: &str = r#"powershell -c "irm https://releases.herdr.dev/core/install.ps1 | iex""#;

/// Why the installer did not install `hc`.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("could not start the installer: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("the installer ran past {} seconds and was stopped", side::LIMIT.as_secs())]
    TimedOut,
    #[error("the installer failed ({status}): {output}")]
    Failed { status: std::process::ExitStatus, output: String },
    /// The job that waited on the installer panicked, so its outcome is
    /// unknown.
    #[error("lost track of the installer")]
    Lost,
}

/// The installer that puts `hc` on one side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installer {
    side: Side,
}

impl Installer {
    pub fn on(side: Side) -> Self {
        Self { side }
    }

    pub fn side(&self) -> &Side {
        &self.side
    }

    /// The command as the user reads it, and as it runs.
    pub fn command_line(&self) -> &'static str {
        if self.powershell() { POWERSHELL } else { SHELL }
    }

    fn powershell(&self) -> bool {
        cfg!(windows) && self.side == Side::Native
    }

    /// Run the installer and wait for it, up to [`side::LIMIT`]. Blocks, so
    /// call it from a job. Answers what the installer printed.
    pub fn run(&self, blocking: &Blocking) -> Result<String, InstallError> {
        let mut command = self.command();
        command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let output =
            blocking.run_drained(&mut command, side::LIMIT).map_err(|error| {
                match error.kind() {
                    std::io::ErrorKind::TimedOut => InstallError::TimedOut,
                    _ => InstallError::Spawn(error),
                }
            })?;
        let Output { status, stdout, stderr } = output;
        let printed = [stdout, stderr]
            .iter()
            .map(|bytes| String::from_utf8_lossy(bytes).trim().to_string())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if status.success() {
            Ok(printed)
        } else {
            Err(InstallError::Failed { status, output: printed })
        }
    }

    fn command(&self) -> Command {
        match &self.side {
            Side::Native if self.powershell() => {
                let mut command = command_ext::hidden("powershell");
                command.args(["-c", POWERSHELL_SCRIPT]);
                command
            },
            Side::Native => {
                let mut command = command_ext::hidden("sh");
                command.args(["-c", SHELL]);
                command
            },
            Side::Wsl(distro) => {
                let mut command = wsl::command(distro, None);
                command.args(["sh", "-c", SHELL]);
                command
            },
        }
    }
}

/// Where the native installer puts `hc`. The installer adds that folder to
/// the user's PATH, which reaches only processes started after it, so a
/// running app looks there itself.
pub(crate) fn native_location() -> Option<PathBuf> {
    let dir = match std::env::var_os("HC_INSTALL_DIR") {
        Some(dir) => PathBuf::from(dir),
        None if cfg!(windows) => {
            PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Programs").join("herdr-core")
        },
        None => PathBuf::from(std::env::var_os("HOME")?).join(".local").join("bin"),
    };
    let hc = dir.join(if cfg!(windows) { "hc.exe" } else { "hc" });
    hc.is_file().then_some(hc)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A distro shows and runs the shell script, inside the distro, whatever
    /// the host is.
    #[test]
    fn a_distro_installs_with_the_shell_script() {
        let installer = Installer::on(Side::Wsl("Ubuntu".into()));
        let command = installer.command();
        let args: Vec<String> =
            command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();

        assert_eq!(
            installer.command_line(),
            "curl -fsSL https://releases.herdr.dev/core/install.sh | sh"
        );
        assert_eq!(command.get_program(), "wsl.exe");
        assert_eq!(args[..2], ["-d", "Ubuntu"]);
        assert_eq!(args[args.len() - 3..], ["sh", "-c", SHELL]);
    }

    /// The command the popup shows is the one that runs.
    #[test]
    fn the_native_command_line_is_what_runs() {
        let installer = Installer::on(Side::Native);
        let command = installer.command();
        let mut argv = vec![command.get_program().to_string_lossy().into_owned()];
        argv.extend(command.get_args().map(|arg| arg.to_string_lossy().into_owned()));

        if cfg!(windows) {
            // Windows joins argv into one command line, quoting each
            // argument that holds a space.
            let quoted: Vec<String> = argv
                .iter()
                .map(|arg| if arg.contains(' ') { format!("\"{arg}\"") } else { arg.clone() })
                .collect();
            assert_eq!(quoted.join(" "), POWERSHELL);
        } else {
            assert_eq!(argv, ["sh", "-c", SHELL]);
        }
    }
}
