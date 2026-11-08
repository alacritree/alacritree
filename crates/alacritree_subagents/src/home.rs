//! Where a harness keeps its files on each side, as this machine reaches
//! them.
//!
//! A harness reads one environment variable for its directory and falls
//! back to a folder in the user's home. The native side reads this process's
//! environment. A WSL distro's directory comes from the distro's resident
//! helper once, and its files are read over the distro's `\\wsl.localhost`
//! share, so a scan starts no `wsl.exe`. The sources read files at offsets,
//! which a share serves and a helper request per read would make slow.

use std::collections::HashMap;
use std::path::PathBuf;

use alacritree_common::{wsl, wsl_helper};

use crate::Side;

/// One harness's directory on each side, looked up once per side.
#[derive(Debug, Clone)]
pub struct Home {
    var: &'static str,
    folder: &'static str,
    /// Each side's directory once known. A distro missing here is asked
    /// for at its next scan.
    dirs: HashMap<Side, Option<PathBuf>>,
}

impl Home {
    /// A harness that keeps its files where `var` points, else in `folder`
    /// under the user's home, the way the harness itself resolves it.
    pub fn new(var: &'static str, folder: &'static str) -> Self {
        let native = match std::env::var_os(var) {
            Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
            _ => home::home_dir().map(|home| home.join(folder)),
        };
        Self { var, folder, dirs: HashMap::from([(Side::Native, native)]) }
    }

    /// Read `side`'s files from `dir`, or none of them for `None`.
    pub fn with(mut self, side: Side, dir: Option<PathBuf>) -> Self {
        self.dirs.insert(side, dir);
        self
    }

    /// `side`'s directory. The first ask for a distro blocks on the distro's
    /// helper.
    pub fn on(&mut self, side: &Side) -> Option<PathBuf> {
        if let Some(dir) = self.dirs.get(side) {
            return dir.clone();
        }
        let Side::Wsl(distro) = side else { return None };
        let linux = wsl_helper::dir_from_env(distro, self.var, self.folder)?;
        let dir = wsl::linux_to_windows(&linux, distro);
        self.dirs.insert(side.clone(), Some(dir.clone()));
        Some(dir)
    }
}
