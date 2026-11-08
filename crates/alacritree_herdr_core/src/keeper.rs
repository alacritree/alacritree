//! The keeper a distro's core gets when alacritree quits.
//!
//! WSL stops an idle distro soon after its last `wsl.exe` client exits, and
//! the core does not count as a client. The keeper is a hidden `wsl.exe`
//! that runs while the core has a live terminal. It asks `hc server status`,
//! which never starts a core. A lock per `HC_SOCKET` and `HC_SESSION` keeps
//! one keeper per core, where the distro has `flock`.

use alacritree_common::{command_ext, side};

/// The keeper, with `$p` the distro's `hc`.
const SCRIPT: &str = r#"dir=${XDG_RUNTIME_DIR:-/tmp}/alacritree; mkdir -m 700 -p "$dir" 2>/dev/null; core=$(printf %s "${HC_SOCKET:-}:${HC_SESSION:-}" | cksum | cut -d' ' -f1); if command -v flock >/dev/null 2>&1 && command exec 9>"$dir/keeper-$core.lock"; then flock -n 9 || exit 0; fi; while "$p" server status --json 2>/dev/null | grep -Eq '"live": *[1-9]'; do sleep 10; done"#;

/// Start the keeper of `distro`'s core, which `hc` reaches, and answer its
/// pid.
pub(crate) fn leave(distro: &str, hc: &str) -> Option<u32> {
    let mut keeper = side::wsl_script_with_program(distro, hc, SCRIPT);
    match command_ext::spawn_detached(&mut keeper) {
        Ok(keeper) => {
            log::info!("herdr-core ({distro}): left keeper {}", keeper.id());
            Some(keeper.id())
        },
        Err(error) => {
            log::warn!("herdr-core ({distro}): could not leave a keeper: {error}");
            None
        },
    }
}
