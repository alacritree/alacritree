//! The session backends this build runs terminals on, dispatched by `match`
//! rather than a vtable. What a backend is asked lives in
//! `alacritree_session_backend`; this module decides which ones exist and
//! wires them to the window.

#[cfg(test)]
use std::collections::HashMap;

use alacritree_herdr_core::{
    CoreElsewhere, HcConfigError, HerdrCore, HerdrCoreConfig, HerdrCoreHandle, HerdrCores,
    SessionBackendChoice,
};
use alacritree_native::{Native, NativeHandle};
use alacritree_session_backend::{
    Adoptable, AttachRequest, OpenRequest, SessionBackend, SessionError, SessionHandle, Side,
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
}

/// The core that `[integrations.herdr_core] session_backend` prefers on each
/// side. Cloned into the job that opens a session.
#[derive(Clone)]
pub(crate) enum Cores {
    HerdrCore(HerdrCores),
    /// A scripted core per side, and none on a side it does not name.
    #[cfg(test)]
    Scripted(HashMap<Side, Scripted>),
}

impl Cores {
    /// `cores` when the config picks herdr-core, `None` when it picks native.
    pub(crate) fn from_config(config: &HerdrCoreConfig, cores: &HerdrCores) -> Option<Self> {
        match config.session_backend {
            SessionBackendChoice::Native => None,
            SessionBackendChoice::HerdrCore => Some(Self::HerdrCore(cores.clone())),
        }
    }

    /// Connect the native core ahead of the first session. When hc does not
    /// reach it, answer a core that listens where hc does not look.
    pub(crate) fn connect(&self) -> Option<CoreElsewhere> {
        match self {
            Self::HerdrCore(cores) => {
                let unreachable = cores.on(&Side::Native).connect().err()?;
                log::warn!("herdr-core: {unreachable}");
                unreachable.elsewhere
            },
            #[cfg(test)]
            Self::Scripted(_) => {
                if let Some(Err(error)) =
                    self.on(&Side::Native).map(|core| SessionBackend::<Handle>::ready(&core))
                {
                    log::warn!("herdr-core: {error}");
                }
                None
            },
        }
    }

    /// Reach the core that `found` names. Only the native side's herdr-core
    /// has an hc to point.
    pub(crate) fn use_core(&self, found: &CoreElsewhere) -> Result<(), HcConfigError> {
        match self {
            Self::HerdrCore(cores) => cores.on(&Side::Native).use_core(found),
            #[cfg(test)]
            Self::Scripted(_) => Ok(()),
        }
    }

    /// Open each distro's core connection when its WSL helper starts.
    pub(crate) fn open_beside_wsl_helpers(&self) {
        match self {
            Self::HerdrCore(cores) => cores.open_beside_wsl_helpers(),
            #[cfg(test)]
            Self::Scripted(_) => {},
        }
    }

    /// Every side's core that this run has used so far, with its side.
    pub(crate) fn all(&self) -> Vec<(Side, Backend)> {
        match self {
            Self::HerdrCore(cores) => cores
                .all()
                .into_iter()
                .map(|(side, core)| (side, Backend::HerdrCore(core)))
                .collect(),
            #[cfg(test)]
            Self::Scripted(cores) => cores
                .iter()
                .map(|(side, core)| (side.clone(), Backend::Scripted(core.clone())))
                .collect(),
        }
    }

    pub(crate) fn on(&self, side: &Side) -> Option<Backend> {
        match self {
            Self::HerdrCore(cores) => Some(Backend::HerdrCore(cores.on(side))),
            #[cfg(test)]
            Self::Scripted(cores) => cores.get(side).cloned().map(Backend::Scripted),
        }
    }
}

/// A terminal that opened, and why it opened native when another backend
/// was asked for.
pub(crate) struct Opened {
    pub(crate) handle: Handle,
    pub(crate) fell_back: Option<SessionError>,
}

/// Open on `preferred`, and on `native` when `preferred` is not ready or
/// cannot start the terminal, so a new session always gets one. A terminal
/// that `preferred` may have started opens nowhere else, since the backend
/// adopts it once it reports it.
pub(crate) fn open_preferring<L>(
    preferred: Option<&Backend>,
    native: &Backend,
    request: OpenRequest<L>,
) -> Result<Opened, SessionError>
where
    L: EventListener + Clone + Send + 'static,
{
    let fell_back = match preferred {
        Some(backend) => match backend.ready().and_then(|()| backend.open(request.clone())) {
            Ok(handle) => return Ok(Opened { handle, fell_back: None }),
            Err(error @ SessionError::Unconfirmed { .. }) => return Err(error),
            Err(error) => Some(error),
        },
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

    fn adoptable(&self) -> Vec<Adoptable> {
        match self {
            Self::Native(backend) => SessionBackend::<Handle>::adoptable(backend),
            Self::HerdrCore(backend) => SessionBackend::<Handle>::adoptable(backend),
            #[cfg(test)]
            Self::Scripted(backend) => SessionBackend::<Handle>::adoptable(backend),
        }
    }

    fn attach<L>(&self, request: AttachRequest<L>) -> Result<Handle, SessionError>
    where
        L: EventListener + Send + 'static,
    {
        match self {
            Self::Native(backend) => backend.attach(request),
            Self::HerdrCore(backend) => backend.attach(request),
            #[cfg(test)]
            Self::Scripted(backend) => backend.attach(request),
        }
    }

    fn watch<L>(&self, request: AttachRequest<L>) -> Result<Handle, SessionError>
    where
        L: EventListener + Send + 'static,
    {
        match self {
            Self::Native(backend) => backend.watch(request),
            Self::HerdrCore(backend) => backend.watch(request),
            #[cfg(test)]
            Self::Scripted(backend) => backend.watch(request),
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
