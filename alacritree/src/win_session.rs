//! Tells an operating-system session end apart from a user closing the window.
//!
//! winit handles neither `WM_QUERYENDSESSION` nor `WM_ENDSESSION`, so
//! `DefWindowProc` acknowledges the session end and the `WM_CLOSE` that
//! follows arrives as an ordinary `CloseRequested` — the same event the close
//! box produces. A logoff, a shutdown, and an installer's Restart Manager
//! closing alacritree to replace a file it holds are therefore all recorded
//! identically, and telling them apart afterwards means reading the Windows
//! Event Log. Reading the session-end message ourselves is what names which
//! one happened.
//!
//! The subclass only observes. Every message, the two session-end ones
//! included, is chained to the procedure it displaced, so the answer this
//! process gives Windows about whether it may shut down remains winit's and
//! `DefWindowProc`'s, unchanged and undelayed.

use std::sync::atomic::{AtomicIsize, Ordering};

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, DefWindowProcW, ENDSESSION_CLOSEAPP, ENDSESSION_LOGOFF, GWLP_WNDPROC,
    SetWindowLongPtrW, WM_ENDSESSION, WM_QUERYENDSESSION, WNDPROC,
};

use crate::crash_log::{self, ExitReason};

/// `WNDPROC` without its `Option`, which is what a stored procedure address
/// turns back into.
type RawWndProc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

/// The window procedure [`session_proc`] displaced, and the one it chains to.
/// Zero until `install` has succeeded.
static PREVIOUS_PROC: AtomicIsize = AtomicIsize::new(0);

/// Subclass the window so session-end messages are seen before winit drops
/// them.
///
/// Nothing uninstalls this: the process is on its way out by the time the hook
/// matters, and restoring the previous procedure during teardown would race
/// the window's own destruction.
///
/// Losing the reason detail must never keep the window from opening, so every
/// failure is logged at `debug` and otherwise ignored.
pub fn install(handle: &impl HasWindowHandle) {
    let Ok(handle) = handle.window_handle() else {
        log::debug!("no window handle; session-end reasons will not be recorded");
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        log::debug!("not a Win32 window; session-end reasons will not be recorded");
        return;
    };
    let hwnd = handle.hwnd.get() as HWND;

    // Only this thread dispatches messages for this window, and it is inside
    // this call, so no message can reach `session_proc` before the store below
    // gives it something to chain to.
    let previous =
        unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, session_proc as *const () as isize) };
    if previous == 0 {
        log::debug!("could not subclass the window; session-end reasons will not be recorded");
        return;
    }
    PREVIOUS_PROC.store(previous, Ordering::Relaxed);
}

unsafe extern "system" fn session_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_QUERYENDSESSION || msg == WM_ENDSESSION {
        // A session end can begin at either message, and the recorder latches
        // on its first caller, so whichever arrives second costs nothing.
        crash_log::record_reason(classify(lparam));
    }

    let previous = PREVIOUS_PROC.load(Ordering::Relaxed);
    if previous == 0 {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let previous: WNDPROC = Some(unsafe { std::mem::transmute::<isize, RawWndProc>(previous) });
    unsafe { CallWindowProcW(previous, hwnd, msg, wparam, lparam) }
}

/// A Restart Manager shutdown during a logoff sets both flags, so `CLOSEAPP`
/// is tested first: the installer is the more specific and the more useful of
/// the two answers.
fn classify(lparam: LPARAM) -> ExitReason {
    let flags = lparam as u32;
    if flags & ENDSESSION_CLOSEAPP != 0 {
        ExitReason::OsCloseApp
    } else if flags & ENDSESSION_LOGOFF != 0 {
        ExitReason::OsLogoff
    } else {
        ExitReason::OsShutdown
    }
}
