//! Desktop notifications for sessions the user isn't looking at, and the way
//! back to the UI thread when one is clicked.
//!
//! Every platform notifier here is synchronous, and the freedesktop one blocks
//! until the toast is dismissed, so posting runs on a throwaway thread. That
//! thread holds no handle on the app, which is why a click travels back
//! through a static channel instead of a callback: it only ever carries a
//! session id, and the UI drains the channel on its next frame.
//!
//! The backend is per-platform: freedesktop on Unix, WinRT on Windows, the
//! UserNotifications framework on macOS. Only macOS needs a module of its
//! own, because there a click arrives through a long-lived delegate rather
//! than through the worker thread that posted the notification.

#[cfg(target_os = "macos")]
pub(crate) mod macos;

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock};

use crate::repaint::Repaint;
use crate::session::SessionId;

/// What one desktop notification says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Toast {
    /// Who wants attention, and the name of the session's row.
    pub title: String,
    /// What it wants.
    pub body: String,
    /// The workspace the session lives in.
    pub place: String,
}

/// Channel from notification-worker threads back to the app. Set once by
/// `channel`; each worker reads it to deliver the session the user clicked
/// on. Static because the worker has no other handle to the app and there's
/// only ever one app instance per process.
static NOTIFY_TX: OnceLock<Mutex<Sender<SessionId>>> = OnceLock::new();

/// Open the click channel, keeping the sending half for the workers and
/// handing the receiving half to the app.
pub(crate) fn channel() -> Receiver<SessionId> {
    let (notify_tx, notify_rx) = mpsc::channel();
    // `set` may fail only if a previous instance already initialized the
    // static (e.g. tests). In that case the old sender points at a dead
    // app, so overwriting via `Mutex` would be ideal — but since we only
    // ever spawn one app per process, ignoring the error is fine.
    let _ = NOTIFY_TX.set(Mutex::new(notify_tx));
    notify_rx
}

/// Drain every queued notification click, keeping only the newest. Clicks
/// can pile up while the window is unfocused; the user most likely meant
/// the latest one.
pub(crate) fn latest_click(rx: &Receiver<SessionId>) -> Option<SessionId> {
    let mut latest = None;
    while let Ok(id) = rx.try_recv() {
        latest = Some(id);
    }
    latest
}

// Toasts `attention` would have posted, for tests that assert what the user
// would see without a desktop to show it on.
#[cfg(test)]
thread_local! {
    pub(crate) static TOASTS: std::cell::RefCell<Vec<Toast>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Spawn a throwaway thread so the platform notifier's synchronous calls
/// don't stall the paint loop. The thread posts the session's id back
/// through `NOTIFY_TX` when the user clicks the notification.
pub(crate) fn attention(id: SessionId, repaint: &impl Repaint, toast: Toast) {
    #[cfg(test)]
    {
        let _ = (id, repaint);
        TOASTS.with_borrow_mut(|toasts| toasts.push(toast));
    }
    #[cfg(not(test))]
    {
        let repaint = repaint.clone();
        std::thread::Builder::new()
            .name("alacritree-notify".into())
            .spawn(move || worker(toast, id, repaint))
            .ok();
    }
}

/// Deliver a clicked notification's session id to the UI thread.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn click(id: SessionId, repaint: &impl Repaint) {
    if let Some(lock) = NOTIFY_TX.get() {
        if let Ok(tx) = lock.lock() {
            let _ = tx.send(id);
            repaint.wake();
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
#[cfg_attr(test, allow(dead_code))]
fn worker(toast: Toast, id: SessionId, repaint: impl Repaint) {
    // `default` is the action id freedesktop notifiers fire on body-click.
    // The icon and the desktop entry are the names the packages install them
    // under, and GNOME takes the app's name and icon from the entry.
    let result = notify_rust::Notification::new()
        .icon("alacritree")
        .hint(notify_rust::Hint::DesktopEntry("alacritree".into()))
        .summary(&toast.title)
        .body(&format!("{}\n{}", toast.body, toast.place))
        .action("default", "Open")
        .show();
    let handle = match result {
        Ok(h) => h,
        Err(e) => {
            log::debug!("desktop notification failed: {e}");
            return;
        },
    };
    handle.wait_for_action(|action| {
        if action == "__closed" {
            return;
        }
        click(id, &repaint);
    });
}

#[cfg(windows)]
#[cfg_attr(test, allow(dead_code))]
fn worker(toast: Toast, id: SessionId, repaint: impl Repaint) {
    // notify-rust doesn't surface WinRT activation, so drive its own backend
    // crate directly. `show` returns immediately; the WinRT runtime holds
    // the activation handler, so this worker thread can exit right away.
    let result = tauri_winrt_notification::Toast::new(windows_identity::app_id())
        .title(&toast.title)
        .text1(&toast.body)
        .text2(&toast.place)
        .on_activated(move |_action| {
            click(id, &repaint);
            Ok(())
        })
        .show();
    if let Err(e) = result {
        log::debug!("desktop notification failed: {e}");
    }
}

#[cfg(target_os = "macos")]
#[cfg_attr(test, allow(dead_code))]
fn worker(toast: Toast, id: SessionId, _repaint: impl Repaint) {
    // Clicks come back through the UNUserNotificationCenter delegate that
    // `macos::init` installed, not through this worker.
    macos::notify(&toast.title, &toast.place, &toast.body, id);
}

/// Windows takes a toast's name and icon from the registry entry of the
/// AppUserModelID it is posted under. An unpackaged app has no entry until it
/// writes one, so alacritree writes its own once per run, at startup.
#[cfg(windows)]
#[cfg_attr(test, allow(dead_code))]
pub(crate) mod windows_identity {
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use tauri_winrt_notification::Toast;
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    const APP_ID: &str = "alacritree";
    const ICON: &[u8] = include_bytes!("../../assets/icon-64.png");

    /// The ID toasts go out under, registered by the first call. Startup makes
    /// that call, and a later one only reads the result. An unregistered ID
    /// shows no toast at all, so a failed registration falls back to
    /// PowerShell's.
    pub(crate) fn app_id() -> &'static str {
        static ID: OnceLock<&'static str> = OnceLock::new();
        ID.get_or_init(|| match register() {
            Ok(()) => APP_ID,
            Err(e) => {
                log::debug!("toast identity registration failed: {e}");
                Toast::POWERSHELL_APP_ID
            },
        })
    }

    fn register() -> std::io::Result<()> {
        let local = std::env::var_os("LOCALAPPDATA").ok_or(std::io::ErrorKind::NotFound)?;
        let dir = PathBuf::from(local).join("alacritree");
        std::fs::create_dir_all(&dir)?;
        let icon = dir.join("toast-icon.png");
        if std::fs::read(&icon).ok().as_deref() != Some(ICON) {
            std::fs::write(&icon, ICON)?;
        }
        let path = format!(r"Software\Classes\AppUserModelId\{APP_ID}");
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(path)?;
        key.set_value("DisplayName", &"alacritree")?;
        key.set_value("IconUri", &icon.to_string_lossy().into_owned())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pile_of_notification_clicks_resolves_to_the_newest() {
        let (tx, rx) = mpsc::channel();
        assert_eq!(latest_click(&rx), None);
        tx.send(3).unwrap();
        tx.send(7).unwrap();
        tx.send(5).unwrap();
        assert_eq!(latest_click(&rx), Some(5));
        // The drain consumed everything, not just the returned click.
        assert_eq!(latest_click(&rx), None);
    }
}
