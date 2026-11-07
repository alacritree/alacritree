//! The session backends this build runs terminals on, dispatched by `match`
//! rather than a vtable. What a backend is asked lives in
//! `alacritree_session_backend`; this module only decides which ones exist.

use alacritree_native::{Native, NativeHandle};
use alacritree_session_backend::{
    OpenRequest, Screen, SessionBackend, SessionError, SessionHandle, Terminal,
    ambassador_impl_SessionHandle,
};
#[cfg(test)]
use alacritree_session_backend::{Scripted, ScriptedHandle};
use alacritty_terminal::event::EventListener;
use ambassador::Delegate;

/// Whatever opens a session's terminal. Cloned into the job that opens one,
/// so it holds nothing a frame owns.
#[derive(Clone)]
pub(crate) enum Backend {
    Native(Native),
    /// Answers from a script instead of a terminal, so app behavior can be
    /// tested at the trait.
    #[cfg(test)]
    Scripted(Scripted),
}

impl Backend {
    pub(crate) fn native() -> Self {
        Self::Native(Native::new(crate::frame_log::spawn_phase))
    }
}

// Written out rather than derived: every backend answers for any handle type
// its own handle converts into, so ambassador's generated match cannot tell
// which `SessionBackend<H>` it means.
impl SessionBackend<Handle> for Backend {
    fn open<L>(&self, request: OpenRequest<L>) -> Result<Handle, SessionError>
    where
        L: EventListener + Send + 'static,
    {
        match self {
            Self::Native(backend) => backend.open(request),
            #[cfg(test)]
            Self::Scripted(backend) => backend.open(request),
        }
    }

    fn adoptable(&self) -> Vec<Terminal> {
        match self {
            Self::Native(backend) => SessionBackend::<Handle>::adoptable(backend),
            #[cfg(test)]
            Self::Scripted(backend) => SessionBackend::<Handle>::adoptable(backend),
        }
    }

    fn attach<L>(&self, terminal: &Terminal, screen: Screen<L>) -> Result<Handle, SessionError>
    where
        L: EventListener + Send + 'static,
    {
        match self {
            Self::Native(backend) => backend.attach(terminal, screen),
            #[cfg(test)]
            Self::Scripted(backend) => backend.attach(terminal, screen),
        }
    }

    fn watch<L>(&self, terminal: &Terminal, screen: Screen<L>) -> Result<Handle, SessionError>
    where
        L: EventListener + Send + 'static,
    {
        match self {
            Self::Native(backend) => backend.watch(terminal, screen),
            #[cfg(test)]
            Self::Scripted(backend) => backend.watch(terminal, screen),
        }
    }
}

/// A running session's terminal, on whichever backend opened it.
#[derive(Delegate)]
#[delegate(SessionHandle)]
pub(crate) enum Handle {
    Native(NativeHandle),
    #[cfg(test)]
    Scripted(ScriptedHandle),
}

impl From<NativeHandle> for Handle {
    fn from(handle: NativeHandle) -> Self {
        Self::Native(handle)
    }
}

#[cfg(test)]
impl From<ScriptedHandle> for Handle {
    fn from(handle: ScriptedHandle) -> Self {
        Self::Scripted(handle)
    }
}
