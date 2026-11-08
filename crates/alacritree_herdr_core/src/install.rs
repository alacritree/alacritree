//! hc's official installer for a side: the shell script inside a distro and
//! on Unix, the PowerShell script on Windows. The commands are the ones
//! herdr-core's README gives, so the user reads exactly what runs.

use std::process::Output;
use std::time::Duration;

use alacritree_common::jobs::Blocking;
use alacritree_common::side::{self, Program, Ran, Side};

const SHELL: &str = "curl -fsSL https://releases.herdr.dev/core/install.sh | sh";
const POWERSHELL_SCRIPT: &str = "irm https://releases.herdr.dev/core/install.ps1 | iex";
const POWERSHELL: &str = r#"powershell -c "irm https://releases.herdr.dev/core/install.ps1 | iex""#;

/// How long the installer may run before it is stopped. It downloads a
/// release, so a slow link takes minutes.
const LIMIT: Duration = Duration::from_secs(300);

/// Why the installer did not install `hc`.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("could not start the installer: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("could not find `{0}` to run the installer")]
    NoShell(String),
    #[error("the installer ran past {} seconds and was stopped", LIMIT.as_secs())]
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

    /// Run the installer and wait for it, up to [`LIMIT`]. Blocks, so call
    /// it from a job. Answers what the installer printed.
    pub fn run(&self, blocking: &Blocking) -> Result<String, InstallError> {
        let (shell, args) = self.shell();
        let ran = side::run_within(&self.side, &shell, None, &args, blocking, LIMIT)
            .map_err(InstallError::Spawn)?;
        let Output { status, stdout, stderr } = match ran {
            Ran::Finished(output) => output,
            Ran::Missing => return Err(InstallError::NoShell(shell.native)),
            Ran::TimedOut => return Err(InstallError::TimedOut),
        };
        let printed = [stdout, stderr]
            .iter()
            .map(|bytes| String::from_utf8_lossy(bytes).trim().to_string())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(
                "
",
            );
        if status.success() {
            Ok(printed)
        } else {
            Err(InstallError::Failed { status, output: printed })
        }
    }

    /// The shell that runs the script, and its arguments. A distro's `sh` is
    /// on the system PATH that `wsl.exe` hands down, so it runs as written.
    fn shell(&self) -> (Program, Vec<String>) {
        let (shell, script) =
            if self.powershell() { ("powershell", POWERSHELL_SCRIPT) } else { ("sh", SHELL) };
        let program = Program {
            native: shell.to_string(),
            wsl: Some("sh".to_string()),
            name: "sh".to_string(),
        };
        (program, vec!["-c".to_string(), script.to_string()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A distro shows and runs the shell script, inside the distro, whatever
    /// the host is.
    #[test]
    fn a_distro_installs_with_the_shell_script() {
        let installer = Installer::on(Side::Wsl("Ubuntu".into()));
        let (shell, args) = installer.shell();

        assert_eq!(
            installer.command_line(),
            "curl -fsSL https://releases.herdr.dev/core/install.sh | sh"
        );
        assert_eq!(shell.wsl.as_deref(), Some("sh"));
        assert_eq!(args, ["-c", SHELL]);
    }

    /// The command the popup shows is the one that runs.
    #[test]
    fn the_native_command_line_is_what_runs() {
        let (shell, args) = Installer::on(Side::Native).shell();
        let argv: Vec<&str> =
            std::iter::once(shell.native.as_str()).chain(args.iter().map(String::as_str)).collect();

        if cfg!(windows) {
            // Windows joins argv into one command line, quoting each
            // argument that holds a space.
            let quoted: Vec<String> = argv
                .iter()
                .map(|arg| if arg.contains(' ') { format!("\"{arg}\"") } else { arg.to_string() })
                .collect();
            assert_eq!(quoted.join(" "), POWERSHELL);
        } else {
            assert_eq!(argv, ["sh", "-c", SHELL]);
        }
    }
}
