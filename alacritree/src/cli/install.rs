//! `alacritree install` copies the running binary, and the console host
//! beside it, into a bin directory.
//!
//! Reading a running image is always allowed, so the source is simply
//! `current_exe()`. What may be pinned is the *destination*: a window or MCP
//! bridge still running from an earlier install, or a pane's `conpty.dll` and
//! `OpenConsole.exe`. A pinned image cannot be overwritten, but it can be
//! renamed. The running process keeps working from the renamed file, and a
//! later install sweeps it once the process has exited.

use std::path::{Path, PathBuf};
use std::{fs, io};

use crate::stale_exe;

/// The console host `alacritty_terminal` loads from the exe's own directory
/// (see `dll_search`). Installing the exe without it leaves every pane on
/// the slower console server built into Windows.
const CONSOLE_HOST: &[&str] = if cfg!(windows) { &["conpty.dll", "OpenConsole.exe"] } else { &[] };

pub(super) fn run(dest: Option<PathBuf>, as_json: bool) -> i32 {
    let installed = std::env::current_exe().and_then(|source| {
        let dir = destination(dest)?;
        install_all(&source, &dir)
            .map_err(|e| io::Error::other(format!("installing into {}: {e}", dir.display())))
    });
    match installed {
        Ok(installed) => {
            report(&installed, as_json);
            0
        },
        // In JSON mode the error goes to stdout as JSON too, matching how the
        // IPC-backed commands behave.
        Err(e) if as_json => {
            println!("{:#}", serde_json::json!({ "error": e.to_string() }));
            1
        },
        Err(e) => {
            eprintln!("alacritree: {e}");
            1
        },
    }
}

#[derive(serde::Serialize)]
struct Installed {
    path: PathBuf,
    renamed_aside: Option<PathBuf>,
}

fn destination(dest: Option<PathBuf>) -> io::Result<PathBuf> {
    match dest {
        Some(dir) => Ok(dir),
        None => home::home_dir()
            .map(|home| home.join(".local").join("bin"))
            .ok_or_else(|| io::Error::other("no home directory, pass --dest")),
    }
}

/// Install the exe and whichever console host files sit beside it. A build
/// without the vendored host is supported, as in `build.rs`: its panes just
/// run slower.
fn install_all(exe: &Path, dir: &Path) -> io::Result<Vec<Installed>> {
    fs::create_dir_all(dir)?;
    stale_exe::sweep_stale(dir);
    let exe_name = format!("alacritree{}", std::env::consts::EXE_SUFFIX);
    let mut installed = vec![install_file(exe, dir, &exe_name)?];
    let source_dir = exe.parent().unwrap_or(Path::new(""));
    for name in CONSOLE_HOST {
        let source = source_dir.join(name);
        if source.is_file() {
            installed.push(install_file(&source, dir, name)?);
        }
    }
    Ok(installed)
}

/// The target name never points at a partial file: the copy lands under a
/// temp name and takes the target name in one rename.
fn install_file(source: &Path, dir: &Path, name: &str) -> io::Result<Installed> {
    let path = dir.join(name);
    // The source may be the target itself, in a self-install from the
    // installed binary, so the copy must land before the target's name is
    // freed.
    let temp = dir.join(format!("{name}{}{}", stale_exe::TEMP_MARKER, std::process::id()));
    fs::copy(source, &temp)?;
    let renamed_aside = match stale_exe::rename_aside_if_locked(&path) {
        Ok(moved) => moved,
        Err(e) => {
            let _ = fs::remove_file(&temp);
            return Err(e);
        },
    };
    if let Err(e) = fs::rename(&temp, &path) {
        let _ = fs::remove_file(&temp);
        return Err(e);
    }
    Ok(Installed { path, renamed_aside })
}

fn report(installed: &[Installed], as_json: bool) {
    if as_json {
        println!("{:#}", serde_json::json!({ "installed": installed }));
        return;
    }
    for file in installed {
        println!("installed {}", file.path.display());
        if let Some(old) = &file.renamed_aside {
            println!(
                "a running alacritree still holds the old {}, moved to {} until it exits",
                file.path.file_name().unwrap_or_default().to_string_lossy(),
                old.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn source_exe(dir: &Path, content: &str) -> PathBuf {
        let path = dir.join("source-build.exe");
        fs::write(&path, content).unwrap();
        path
    }

    fn target_in(dir: &Path) -> PathBuf {
        dir.join(format!("alacritree{}", std::env::consts::EXE_SUFFIX))
    }

    /// `--dest` may name a directory that does not exist yet; a first install
    /// must not demand a manual mkdir.
    #[test]
    fn installs_into_a_directory_that_does_not_exist_yet() {
        let dir = TempDir::new().unwrap();
        let source = source_exe(dir.path(), "v2");
        let dest = dir.path().join("bin");

        let installed = install_all(&source, &dest).unwrap();

        assert_eq!(installed[0].path, target_in(&dest));
        assert_eq!(fs::read_to_string(&installed[0].path).unwrap(), "v2");
        assert_eq!(installed[0].renamed_aside, None);
    }

    #[test]
    fn replaces_a_previous_install_nothing_is_running_from() {
        let dir = TempDir::new().unwrap();
        let source = source_exe(dir.path(), "v2");
        let dest = dir.path().join("bin");
        fs::create_dir_all(&dest).unwrap();
        fs::write(target_in(&dest), "v1").unwrap();

        let installed = install_all(&source, &dest).unwrap();

        assert_eq!(fs::read_to_string(&installed[0].path).unwrap(), "v2");
        assert_eq!(installed[0].renamed_aside, None);
    }

    /// The point of the subcommand: installing over a binary the window or a
    /// bridge still runs from must succeed, not fail with access denied.
    #[cfg(windows)]
    #[test]
    fn a_pinned_previous_install_is_renamed_aside_and_replaced() {
        let dir = TempDir::new().unwrap();
        let source = source_exe(dir.path(), "v2");
        let dest = dir.path().join("bin");
        fs::create_dir_all(&dest).unwrap();
        fs::write(target_in(&dest), "v1").unwrap();
        let _running = crate::test_util::hold_like_a_running_image(&target_in(&dest));

        let installed = install_all(&source, &dest).unwrap();

        assert_eq!(fs::read_to_string(&installed[0].path).unwrap(), "v2");
        let aside = installed[0].renamed_aside.clone().expect("the pinned exe was moved");
        assert_eq!(fs::read_to_string(&aside).unwrap(), "v1", "the running image is intact");
    }

    /// Without the console host beside it, the installed exe opens every pane
    /// on the slower console server. A pane pins its host just as a window
    /// pins the exe, so the same rename-aside applies.
    #[cfg(windows)]
    #[test]
    fn the_console_host_beside_the_exe_is_installed_with_it() {
        let dir = TempDir::new().unwrap();
        let source = source_exe(dir.path(), "v2");
        fs::write(dir.path().join("conpty.dll"), "dll v2").unwrap();
        fs::write(dir.path().join("OpenConsole.exe"), "host v2").unwrap();
        let dest = dir.path().join("bin");
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("conpty.dll"), "dll v1").unwrap();
        let _pane = crate::test_util::hold_like_a_running_image(&dest.join("conpty.dll"));

        let installed = install_all(&source, &dest).unwrap();

        assert_eq!(fs::read_to_string(dest.join("conpty.dll")).unwrap(), "dll v2");
        assert_eq!(fs::read_to_string(dest.join("OpenConsole.exe")).unwrap(), "host v2");
        let dll = installed.iter().find(|f| f.path == dest.join("conpty.dll")).unwrap();
        assert!(dll.renamed_aside.is_some(), "the pane's dll was moved aside");
    }

    #[test]
    fn an_exe_without_a_console_host_installs_alone() {
        let dir = TempDir::new().unwrap();
        let source = source_exe(dir.path(), "v2");
        let dest = dir.path().join("bin");

        let installed = install_all(&source, &dest).unwrap();

        assert_eq!(installed.len(), 1);
        assert!(!dest.join("conpty.dll").exists());
    }

    #[test]
    fn leftovers_from_earlier_installs_are_swept() {
        let dir = TempDir::new().unwrap();
        let source = source_exe(dir.path(), "v2");
        let dest = dir.path().join("bin");
        fs::create_dir_all(&dest).unwrap();
        let exe_leftover = dest.join("alacritree.exe.stale-9-0");
        let host_leftover = dest.join("conpty.dll.stale-9-0");
        fs::write(&exe_leftover, "v0").unwrap();
        fs::write(&host_leftover, "v0").unwrap();

        install_all(&source, &dest).unwrap();

        assert!(!exe_leftover.exists());
        assert!(!host_leftover.exists());
    }

    #[test]
    fn the_default_destination_is_local_bin() {
        let dest = destination(None).unwrap();

        assert!(dest.ends_with(Path::new(".local").join("bin")), "{}", dest.display());
    }

    /// `alacritree install` run from the installed binary itself: the source
    /// IS the target, and the process holds it. The copy must land in the
    /// temp file before the target's name is freed, or a self-install deletes
    /// the very binary it is installing.
    #[cfg(windows)]
    #[test]
    fn a_self_install_survives_the_source_being_the_target() {
        let dir = TempDir::new().unwrap();
        let dest = dir.path().join("bin");
        fs::create_dir_all(&dest).unwrap();
        let target = target_in(&dest);
        fs::write(&target, "v1").unwrap();
        let _running = crate::test_util::hold_like_a_running_image(&target);

        let installed = install_all(&target, &dest).unwrap();

        assert_eq!(fs::read_to_string(&installed[0].path).unwrap(), "v1");
        assert!(installed[0].renamed_aside.is_some(), "the held image was moved aside");
    }
}
