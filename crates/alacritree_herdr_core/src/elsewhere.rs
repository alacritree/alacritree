//! A core that hc does not reach because it listens on a socket other than
//! the one hc resolves, such as a WSL core started before `/run/user/<uid>`
//! existed. Its `core.lock` names where it listens. Writing that path as
//! `socket` in hc's `config.toml` points hc there and leaves the core
//! running. A restart would lose every terminal, since the holder socket
//! moves with the core socket.

use std::path::{Path, PathBuf};

/// Why hc's `config.toml` could not take a new `socket`.
#[derive(Debug, thiserror::Error)]
pub enum HcConfigError {
    #[error("could not read {}: {source}", path.display())]
    Read { path: PathBuf, source: std::io::Error },
    #[error("{} is not valid TOML: {source}", path.display())]
    Parse { path: PathBuf, source: toml_edit::TomlError },
    #[error("could not write {}: {source}", path.display())]
    Write { path: PathBuf, source: std::io::Error },
}

/// Where hc keeps this session's core state and its own settings, found
/// the way hc finds them.
pub(crate) struct HcPaths {
    lock: PathBuf,
    config: PathBuf,
    /// `HC_SOCKET` is set, and it wins over the `socket` key.
    env_socket: bool,
}

impl HcPaths {
    pub(crate) fn from_env() -> Self {
        let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        let session = std::env::var("HC_SESSION").unwrap_or_default();
        let session = Some(session.trim()).filter(|name| !name.is_empty()).unwrap_or("default");
        let (state_home, config_home) = data_dirs();
        let state_home = var("XDG_STATE_HOME").map_or(state_home, PathBuf::from);
        let config_home = var("XDG_CONFIG_HOME").map_or(config_home, PathBuf::from);
        Self {
            lock: state_home.join("herdr-core").join(session).join("core.lock"),
            config: config_home.join("herdr-core").join("config.toml"),
            env_socket: var("HC_SOCKET").is_some(),
        }
    }
}

/// hc's state and config homes when the XDG variables are unset.
#[cfg(windows)]
fn data_dirs() -> (PathBuf, PathBuf) {
    let profile = std::env::var_os("USERPROFILE").map_or_else(|| r"C:\".into(), PathBuf::from);
    let local =
        std::env::var_os("LOCALAPPDATA").map_or_else(|| profile.join(r"AppData\Local"), Into::into);
    let roaming =
        std::env::var_os("APPDATA").map_or_else(|| profile.join(r"AppData\Roaming"), Into::into);
    (local, roaming)
}

#[cfg(not(windows))]
fn data_dirs() -> (PathBuf, PathBuf) {
    let home = std::env::var_os("HOME").map_or_else(|| "/".into(), PathBuf::from);
    (home.join(".local/state"), home.join(".config"))
}

/// The socket of a core that serves hc's state where hc does not look.
/// `HC_SOCKET` overrides the config, so with it set there is nothing a
/// config write could fix. A socket the config already names is where hc
/// looks, so hc refused it for another reason.
pub(crate) fn socket(paths: &HcPaths) -> Option<PathBuf> {
    if paths.env_socket {
        return None;
    }
    let configured = configured_socket(&paths.config);
    read_lock(&paths.lock)
        .filter(|socket| configured.as_ref() != Some(socket))
        .filter(|socket| answers(socket))
}

/// The socket that the `socket` key of hc's `config.toml` names.
fn configured_socket(config: &Path) -> Option<PathBuf> {
    let config: toml_edit::DocumentMut = std::fs::read_to_string(config).ok()?.parse().ok()?;
    config.get("socket")?.as_str().map(PathBuf::from)
}

/// Something accepts a connection at `socket`. A lock left by a core that
/// stopped names a socket nobody serves.
#[cfg(unix)]
fn answers(socket: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(socket).is_ok()
}

#[cfg(windows)]
fn answers(socket: &Path) -> bool {
    std::fs::OpenOptions::new().read(true).write(true).open(socket).is_ok()
}

/// The socket path the core wrote into its lock.
fn read_lock(lock: &Path) -> Option<PathBuf> {
    match std::fs::read_to_string(lock) {
        Ok(socket) => (!socket.is_empty()).then(|| socket.into()),
        Err(error) => read_locked_pipe(lock, &error),
    }
}

/// herdr-core v0.1.18's `serve` holds `try_lock` on the first byte of
/// `core.lock`, and on Windows that lock fails every read that covers it.
/// That byte is the leading `\` of a `\\.\pipe\` path, so the rest is read
/// past it and taken only when it reads as the rest of a pipe path.
#[cfg(windows)]
fn read_locked_pipe(lock: &Path, error: &std::io::Error) -> Option<PathBuf> {
    use std::io::{Read, Seek, SeekFrom};
    const ERROR_LOCK_VIOLATION: i32 = 33;
    if error.raw_os_error() != Some(ERROR_LOCK_VIOLATION) {
        return None;
    }
    let mut file = std::fs::File::open(lock).ok()?;
    file.seek(SeekFrom::Start(1)).ok()?;
    let mut rest = String::new();
    file.read_to_string(&mut rest).ok()?;
    rest.starts_with(r"\.\pipe\").then(|| format!(r"\{rest}").into())
}

/// Only Windows locks are mandatory, so elsewhere a failed read means
/// there is no lock to read.
#[cfg(not(windows))]
fn read_locked_pipe(_lock: &Path, _error: &std::io::Error) -> Option<PathBuf> {
    None
}

/// Write `socket` as the `socket` key of hc's `config.toml`, through a
/// rename so an hc that reads it meanwhile never sees half a file.
pub(crate) fn point_hc_at(paths: &HcPaths, socket: &Path) -> Result<(), HcConfigError> {
    let path = &paths.config;
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => return Err(HcConfigError::Read { path: path.clone(), source }),
    };
    let mut config: toml_edit::DocumentMut =
        text.parse().map_err(|source| HcConfigError::Parse { path: path.clone(), source })?;
    config["socket"] = toml_edit::value(socket.display().to_string());

    let written = path.with_extension("toml.tmp");
    let write = || {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&written, config.to_string())?;
        std::fs::rename(&written, path)
    };
    write().map_err(|source| HcConfigError::Write { path: path.clone(), source })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths_in(dir: &Path) -> HcPaths {
        HcPaths {
            lock: dir.join("state").join("core.lock"),
            config: dir.join("config").join("config.toml"),
            env_socket: false,
        }
    }

    /// A lock left behind by a core that stopped names a socket nobody
    /// serves, so pointing hc there would reach nothing.
    #[test]
    fn a_lock_whose_socket_does_not_answer_names_no_core() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let paths = paths_in(dir.path());
        let nobody = if cfg!(windows) {
            format!(r"\\.\pipe\alacritree-elsewhere-test-{}", std::process::id())
        } else {
            dir.path().join("nobody.sock").display().to_string()
        };
        std::fs::create_dir_all(paths.lock.parent().unwrap()).unwrap();
        std::fs::write(&paths.lock, nobody).unwrap();

        assert_eq!(socket(&paths), None);
    }

    /// A core that answers where hc does not look is offered. Once hc's
    /// config names that socket, hc refusing again has another cause, so
    /// offering the same write again would only repeat the question.
    #[cfg(unix)]
    #[test]
    fn a_core_is_offered_until_hcs_config_names_its_socket() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let paths = paths_in(dir.path());
        let listening = dir.path().join("core.sock");
        let _core = std::os::unix::net::UnixListener::bind(&listening).expect("a socket");
        std::fs::create_dir_all(paths.lock.parent().unwrap()).unwrap();
        std::fs::write(&paths.lock, listening.display().to_string()).unwrap();

        assert_eq!(socket(&paths), Some(listening.clone()));

        point_hc_at(&paths, &listening).unwrap();
        assert_eq!(socket(&paths), None);
    }

    /// Only `socket` changes, so the user's other settings and comments
    /// stay as written.
    #[test]
    fn pointing_hc_at_a_socket_keeps_the_rest_of_its_config() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let paths = paths_in(dir.path());
        std::fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
        std::fs::write(&paths.config, "# my shell\nshell = [\"nu\"]\nsocket = \"/old.sock\"\n")
            .unwrap();

        point_hc_at(&paths, Path::new("/run/user/1000/herdr-core/default.sock")).unwrap();

        assert_eq!(
            std::fs::read_to_string(&paths.config).unwrap(),
            "# my shell\nshell = [\"nu\"]\nsocket = \"/run/user/1000/herdr-core/default.sock\"\n"
        );
    }

    #[test]
    fn pointing_hc_at_a_socket_creates_a_missing_config() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let paths = paths_in(dir.path());

        point_hc_at(&paths, Path::new("/tmp/hc.sock")).unwrap();

        assert_eq!(std::fs::read_to_string(&paths.config).unwrap(), "socket = \"/tmp/hc.sock\"\n");
    }
}
