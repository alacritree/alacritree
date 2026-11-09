//! The keeper that holds a distro up while its core has a running terminal.
//!
//! WSL stops an idle distro soon after its last `wsl.exe` client exits, and
//! the core does not count as a client. The keeper is a hidden `wsl.exe`
//! that runs while the core has a live terminal, so the distro outlives the
//! app however the app ends. It asks `hc server status`, which never starts
//! a core. A lock per `HC_SOCKET` and `HC_SESSION` keeps one keeper per core,
//! where the distro has `flock`, so every start after the first is harmless.

use alacritree_common::{command_ext, side};

/// `sh` code that sets `dir` and `lock` to the lock of the core that
/// `HC_SOCKET` and `HC_SESSION` name.
macro_rules! find_lock {
    () => {
        r#"dir=${XDG_RUNTIME_DIR:-/tmp}/alacritree; core=$(printf %s "${HC_SOCKET:-}:${HC_SESSION:-}" | cksum | cut -d' ' -f1); lock=$dir/keeper-$core.lock; "#
    };
}

/// The keeper, with `$p` the distro's `hc`.
const SCRIPT: &str = concat!(
    pin_socket!(),
    find_lock!(),
    r#"mkdir -m 700 -p "$dir" 2>/dev/null; if command -v flock >/dev/null 2>&1 && command exec 9>"$lock"; then flock -n 9 || exit 0; fi; while "$p" server status --json 2>/dev/null | grep -Eq '"live": *[1-9]'; do sleep 10; done"#
);

/// Start the keeper of `distro`'s core, which `hc` reaches at `socket`, and
/// answer its pid. An empty `socket` leaves the choice to hc. Blocks for the
/// spawn.
pub(crate) fn start(distro: &str, hc: &str, socket: &str) -> Option<u32> {
    let mut keeper = side::wsl_script_with_program(distro, hc, SCRIPT);
    keeper.arg(socket);
    match command_ext::spawn_detached(&mut keeper) {
        Ok(keeper) => {
            log::info!("herdr-core ({distro}): started keeper {}", keeper.id());
            Some(keeper.id())
        },
        Err(error) => {
            log::warn!("herdr-core ({distro}): could not start a keeper: {error}");
            None
        },
    }
}

/// [`start`] on a thread of its own, for a caller that must not wait for
/// the spawn.
pub(crate) fn start_in_background(distro: String, hc: String, socket: String) {
    let spawned = std::thread::Builder::new().name("hc-keeper".into()).spawn(move || {
        start(&distro, &hc, &socket);
    });
    if let Err(error) = spawned {
        log::warn!("herdr-core: could not start a keeper thread: {error}");
    }
}

/// Answers whether a keeper holds the lock of `distro`'s core.
#[cfg(feature = "test-support")]
pub(crate) fn holds_lock(distro: &str, hc: &str, socket: &str) -> bool {
    const PROBE: &str =
        concat!(pin_socket!(), find_lock!(), r#"[ -e "$lock" ] && ! flock -n "$lock" true"#);
    let mut probe = side::wsl_script_with_program(distro, hc, PROBE);
    probe.arg(socket);
    // Only the end-to-end tests ask, from the test thread.
    #[allow(clippy::disallowed_methods)]
    probe.output().is_ok_and(|probed| probed.status.success())
}
