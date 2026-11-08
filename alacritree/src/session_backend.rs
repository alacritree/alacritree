//! The session backends this build runs terminals on, dispatched by `match`
//! rather than a vtable. What a backend is asked lives in
//! `alacritree_session_backend`; this module only decides which ones exist.

use alacritree_herdr_core::{HerdrCore, HerdrCoreConfig, HerdrCoreHandle, SessionBackendChoice};
use alacritree_native::{Native, NativeHandle};
use alacritree_session_backend::{
    OpenRequest, PaneKey, Screen, SessionBackend, SessionError, SessionHandle,
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
    HerdrCore(HerdrCore),
    /// Answers from a script instead of a terminal, so app behavior can be
    /// tested at the trait.
    #[cfg(test)]
    Scripted(Scripted),
}

impl Backend {
    pub(crate) fn native() -> Self {
        Self::Native(Native::new(crate::frame_log::spawn_phase))
    }

    /// The backend that `[integrations.herdr_core] session_backend` picks for
    /// shells on the native side, or `None` when it picks native.
    pub(crate) fn for_native_shells(config: &HerdrCoreConfig) -> Option<Self> {
        match config.session_backend {
            SessionBackendChoice::Native => None,
            SessionBackendChoice::HerdrCore => Some(Self::HerdrCore(HerdrCore::new(&config.path))),
        }
    }
}

/// A terminal that opened, and why it opened native when another backend
/// was asked for.
pub(crate) struct Opened {
    pub(crate) handle: Handle,
    pub(crate) fell_back: Option<SessionError>,
}

/// Open on `preferred` when it is ready, and on `native` when it is not, so
/// a new session always gets a terminal.
pub(crate) fn open_preferring<L>(
    preferred: Option<&Backend>,
    native: &Backend,
    request: OpenRequest<L>,
) -> Result<Opened, SessionError>
where
    L: EventListener + Send + 'static,
{
    let fell_back = match preferred.map(|backend| (backend, backend.ready())) {
        Some((backend, Ok(()))) => {
            return backend.open(request).map(|handle| Opened { handle, fell_back: None });
        },
        Some((_, Err(error))) => Some(error),
        None => None,
    };
    native.open(request).map(|handle| Opened { handle, fell_back })
}

// Written out rather than derived: every backend answers for any handle type
// its own handle converts into, so ambassador's generated match cannot tell
// which `SessionBackend<H>` it means.
impl SessionBackend<Handle> for Backend {
    fn ready(&self) -> Result<(), SessionError> {
        match self {
            Self::Native(backend) => SessionBackend::<Handle>::ready(backend),
            Self::HerdrCore(backend) => SessionBackend::<Handle>::ready(backend),
            #[cfg(test)]
            Self::Scripted(backend) => SessionBackend::<Handle>::ready(backend),
        }
    }

    fn open<L>(&self, request: OpenRequest<L>) -> Result<Handle, SessionError>
    where
        L: EventListener + Send + 'static,
    {
        match self {
            Self::Native(backend) => backend.open(request),
            Self::HerdrCore(backend) => backend.open(request),
            #[cfg(test)]
            Self::Scripted(backend) => backend.open(request),
        }
    }

    fn adoptable(&self) -> Vec<PaneKey> {
        match self {
            Self::Native(backend) => SessionBackend::<Handle>::adoptable(backend),
            Self::HerdrCore(backend) => SessionBackend::<Handle>::adoptable(backend),
            #[cfg(test)]
            Self::Scripted(backend) => SessionBackend::<Handle>::adoptable(backend),
        }
    }

    fn attach<L>(&self, pane: &PaneKey, screen: Screen<L>) -> Result<Handle, SessionError>
    where
        L: EventListener + Send + 'static,
    {
        match self {
            Self::Native(backend) => backend.attach(pane, screen),
            Self::HerdrCore(backend) => backend.attach(pane, screen),
            #[cfg(test)]
            Self::Scripted(backend) => backend.attach(pane, screen),
        }
    }

    fn watch<L>(&self, pane: &PaneKey, screen: Screen<L>) -> Result<Handle, SessionError>
    where
        L: EventListener + Send + 'static,
    {
        match self {
            Self::Native(backend) => backend.watch(pane, screen),
            Self::HerdrCore(backend) => backend.watch(pane, screen),
            #[cfg(test)]
            Self::Scripted(backend) => backend.watch(pane, screen),
        }
    }
}

/// A running session's terminal, on whichever backend opened it.
#[derive(Delegate)]
#[delegate(SessionHandle)]
pub(crate) enum Handle {
    Native(NativeHandle),
    HerdrCore(HerdrCoreHandle),
    #[cfg(test)]
    Scripted(ScriptedHandle),
}

impl From<NativeHandle> for Handle {
    fn from(handle: NativeHandle) -> Self {
        Self::Native(handle)
    }
}

impl From<HerdrCoreHandle> for Handle {
    fn from(handle: HerdrCoreHandle) -> Self {
        Self::HerdrCore(handle)
    }
}

#[cfg(test)]
impl From<ScriptedHandle> for Handle {
    fn from(handle: ScriptedHandle) -> Self {
        Self::Scripted(handle)
    }
}
