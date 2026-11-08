//! The session backend that runs each terminal in a PTY alacritree owns.
//!
//! Opening one is process creation, the job that holds its tree, and the
//! event loop that drains it into the app's `Term`. A native PTY cannot
//! outlive its handle, so this backend adopts, attaches, watches and
//! detaches nothing.

mod focus_priority;
#[cfg(windows)]
mod pty_rearm;
pub mod wsl_spare;

use std::path::Path;
use std::time::{Duration, Instant};

use alacritree_common::wsl_helper::{self, WslProbe};
use alacritree_session_backend::{
    Adoptable, AttachRequest, ForegroundProgram, OpenRequest, Operation, Placement, ProcessTree,
    Screen, SessionBackend, SessionBackendKind, SessionError, SessionHandle, Signals,
};
use alacritty_terminal::event::{EventListener, Notify, OnResize, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, Msg, Notifier};
use alacritty_terminal::tty::{self, Options as PtyOptions, Shell};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

use self::focus_priority::PriorityJob;
pub use self::focus_priority::set_self_boosted;
use alacritree_process_probe::ProbeHandle;

/// Receives each phase of an open and how long it took, for the frame log.
pub type PhaseLog = fn(session: Option<u64>, phase: &str, elapsed: Duration);

#[derive(Clone, Copy)]
pub struct Native {
    phase_log: PhaseLog,
}

impl Native {
    pub fn new(phase_log: PhaseLog) -> Self {
        Self { phase_log }
    }
}

#[cfg(test)]
impl Default for Native {
    fn default() -> Self {
        Self::new(|_, _, _| {})
    }
}

/// A running native session. Dropping it shuts the PTY down, which is what
/// happens when a session closes while its PTY is opening.
pub struct NativeHandle {
    notifier: Notifier,
    probe: ProbeHandle,
    /// Holds the shell and, as they are created, everything it starts, so
    /// focus can raise the whole tree in one call. `None` unless a
    /// [`ProcessTree`] setting wants it, or when the shell refused the job.
    priority_job: Option<PriorityJob>,
    /// Registered with the WSL helper while this handle lives.
    wsl_probe: Option<WslProbe>,
    /// The cell size a shimmed WSL shell was last told, in device pixels.
    wsl_cell_size: Option<(u16, u16)>,
}

impl<H: From<NativeHandle>> SessionBackend<H> for Native {
    /// A PTY needs nothing running first.
    fn ready(&self) -> Result<(), SessionError> {
        Ok(())
    }

    fn open<L>(&self, request: OpenRequest<L>) -> Result<H, SessionError>
    where
        L: EventListener + Send + 'static,
    {
        let spare = request.shell.as_ref().filter(|_| wsl_spare::enabled()).and_then(|shell| {
            wsl_spare::Launch::parse(
                &shell.program,
                &shell.args,
                request.working_directory.as_deref(),
            )
        });
        open(request, spare, self.phase_log).map(H::from)
    }

    fn adoptable(&self) -> Vec<Adoptable> {
        Vec::new()
    }

    fn attach<L>(&self, _request: AttachRequest<L>) -> Result<H, SessionError>
    where
        L: EventListener + Send + 'static,
    {
        Err(unsupported(Operation::Attach))
    }

    fn watch<L>(&self, _request: AttachRequest<L>) -> Result<H, SessionError>
    where
        L: EventListener + Send + 'static,
    {
        Err(unsupported(Operation::Watch))
    }
}

fn unsupported(operation: Operation) -> SessionError {
    SessionError::Unsupported { backend: SessionBackendKind::Native, operation }
}

impl SessionHandle for NativeHandle {
    fn write(&self, bytes: Vec<u8>) {
        self.notifier.notify(bytes);
    }

    /// A PTY carries no tag, so a reply is written like typing.
    fn reply(&self, bytes: Vec<u8>) {
        self.write(bytes);
    }

    fn resize(&mut self, size: WindowSize) {
        let _ = self.notifier.0.send(Msg::Resize(size));
        let cell = (size.cell_width, size.cell_height);
        if self.wsl_cell_size != Some(cell) {
            self.wsl_cell_size = Some(cell);
            self.send_wsl_cell_size(cell);
        }
    }

    /// A session with no job holds nothing and always answers false.
    fn set_focused(&self, focused: bool) -> bool {
        let Some(job) = &self.priority_job else {
            return false;
        };
        job.set_boosted(focused);
        focused
    }

    fn kill(&mut self) {
        let _ = self.notifier.0.send(Msg::Shutdown);
    }

    fn detach(&mut self) -> Result<(), SessionError> {
        Err(unsupported(Operation::Detach))
    }

    /// A PTY dies with the app, so nothing outlives it to read a placement.
    fn place(&mut self, _placement: &Placement) {}

    fn cwd(&self) -> Option<std::path::PathBuf> {
        None
    }

    fn foreground(&self) -> Option<ForegroundProgram> {
        self.probe.foreground()
    }

    fn signals(&self) -> Signals {
        self.probe.signals()
    }
}

impl NativeHandle {
    /// Have a shimmed WSL shell's tty report the pixel size ConPTY drops,
    /// which programs such as `kitten icat` read to size images. The distro
    /// side keeps it in step with the grid, so only a new cell size needs
    /// sending.
    fn send_wsl_cell_size(&self, (width, height): (u16, u16)) {
        if let Some(probe) = &self.wsl_probe {
            let cell = wsl_helper::CellSize { width, height };
            wsl_helper::set_cell_size(&probe.distro, &probe.key, cell);
        }
    }
}

impl Drop for NativeHandle {
    fn drop(&mut self) {
        if let Some(probe) = &self.wsl_probe {
            wsl_helper::unregister_probe(&probe.distro, &probe.key);
        }
        let _ = self.notifier.0.send(Msg::Shutdown);
    }
}

fn ensure_working_directory(dir: Option<&Path>) -> Result<(), SessionError> {
    match dir {
        Some(d) if !d.is_dir() => Err(SessionError::MissingDirectory(d.to_path_buf())),
        _ => Ok(()),
    }
}

/// Open the PTY: process creation, the job that owns it, and the event loop
/// that drains it. `spare` is the warm WSL terminal to take instead, when
/// one fits.
fn open<L>(
    request: OpenRequest<L>,
    spare: Option<wsl_spare::Launch>,
    phase_log: PhaseLog,
) -> Result<NativeHandle, SessionError>
where
    L: EventListener + Send + 'static,
{
    let started = Instant::now();
    let OpenRequest {
        session_id: id,
        shell,
        escape_args,
        working_directory,
        env,
        wsl_probe,
        process_tree: ProcessTree { boost_on_focus: boost, reap_on_close: reap },
        colors: _,
        placement: _,
        screen: Screen { term, listener, window_size, history: _ },
    } = request;

    ensure_working_directory(working_directory.as_deref())?;

    #[cfg(not(windows))]
    let _ = escape_args;
    let pty_options = PtyOptions {
        shell: shell.map(|shell| Shell::new(shell.program, shell.args)),
        working_directory,
        // Without this the loop drops whatever the child wrote and it had
        // not yet read: the last output a held session exists to show.
        // Unconditional because any session can be held.
        drain_on_exit: true,
        env,
        #[cfg(windows)]
        escape_args,
    };

    let template = wsl_spare::Template::new(&pty_options.env, boost, reap);
    let claimed = spare.and_then(|launch| wsl_spare::claim(&launch, &id.to_string(), &template));
    let (pty, shell_pid, priority_job, line) = match claimed {
        Some(mut claimed) => {
            claimed.pty.on_resize(window_size);
            Processor::<StdSyncHandler>::new().advance(&mut *term.lock(), &claimed.preamble);
            phase_log(Some(id), "spare", started.elapsed());
            (claimed.pty, claimed.shell_pid, claimed.priority_job, Some(claimed.line))
        },
        None => {
            // `tty::new` is where `LoadLibraryW("conpty.dll")` happens, and the
            // module it loads answers every later one for the life of the process.
            alacritree_common::dll_search::harden_dll_search_path();

            // alacritty routes OSC 7 and signals by this id, so each PTY
            // needs its own.
            let pty = tty::new(&pty_options, window_size, next_window_id())?;
            phase_log(Some(id), "pty", started.elapsed());
            let shell_pid = shell_pid_of(&pty);

            // Jobbed here rather than on focus: a process joins a job when it is
            // created, so anything the shell starts before the job exists escapes
            // it for its whole life. One job serves both settings, so it is
            // created when either wants it.
            let priority_job =
                shell_pid.filter(|_| boost || reap).and_then(|pid| PriorityJob::adopt(pid, reap));
            (pty, shell_pid, priority_job, None)
        },
    };

    #[cfg(windows)]
    let pty = pty_rearm::RearmingPty::new(pty);

    let event_loop = EventLoop::new(term, listener, pty, pty_options.drain_on_exit, false)?;
    let sender = event_loop.channel();
    event_loop.spawn();
    if let Some(line) = line {
        let _ = sender.send(Msg::Input(line.into()));
    }
    phase_log(Some(id), "open", started.elapsed());

    // Registered once the PTY exists, and unregistered only by the handle's
    // drop, so a probe never outlives the session it reads.
    if let Some(probe) = &wsl_probe {
        wsl_helper::register_probe(&probe.distro, &probe.key);
    }
    Ok(NativeHandle {
        notifier: Notifier(sender),
        probe: ProbeHandle::new(shell_pid, wsl_probe.clone()),
        priority_job,
        wsl_probe,
        wsl_cell_size: None,
    })
}

/// The pid of the shell a PTY started, where the platform will say.
#[cfg(unix)]
fn shell_pid_of(pty: &tty::Pty) -> Option<u32> {
    Some(pty.child().id())
}

/// The pid of the shell a PTY started, where the platform will say.
#[cfg(windows)]
fn shell_pid_of(pty: &tty::Pty) -> Option<u32> {
    // Under ConPTY the PTY child *is* the shell; everything the user runs
    // is spawned beneath it.
    pty.child_watcher().pid().map(std::num::NonZeroU32::get)
}

/// The pid of the shell a PTY started, where the platform will say.
#[cfg(not(any(unix, windows)))]
fn shell_pid_of(_pty: &tty::Pty) -> Option<u32> {
    None
}

fn next_window_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
pub(crate) mod tests {
    // Fixtures drive real processes and wait on them; no frame is pending.
    #![allow(clippy::disallowed_methods)]

    use std::collections::HashMap;
    use std::sync::Arc;

    use alacritree_session_backend::ShellCommand;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::sync::FairMutex;
    use alacritty_terminal::term::{Config, Term};

    use super::*;

    pub(crate) type TestTerm = Arc<FairMutex<Term<VoidListener>>>;

    struct Size;

    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            24
        }

        fn screen_lines(&self) -> usize {
            24
        }

        fn columns(&self) -> usize {
            80
        }
    }

    /// A request to run `program args` in an 80x24 grid this test can read.
    pub(crate) fn request(program: &str, args: &[&str]) -> (OpenRequest<VoidListener>, TestTerm) {
        let term = Arc::new(FairMutex::new(Term::new(Config::default(), &Size, VoidListener)));
        let mut env = HashMap::new();
        env.insert("TERM".to_string(), "xterm-256color".to_string());
        let request = OpenRequest {
            session_id: 1,
            shell: Some(ShellCommand::new(
                program.to_string(),
                args.iter().map(|arg| arg.to_string()).collect(),
            )),
            escape_args: true,
            working_directory: std::env::current_dir().ok(),
            env,
            wsl_probe: None,
            process_tree: ProcessTree::default(),
            colors: Default::default(),
            placement: Default::default(),
            screen: Screen {
                term: term.clone(),
                listener: VoidListener,
                window_size: WindowSize {
                    num_lines: 24,
                    num_cols: 80,
                    cell_width: 8,
                    cell_height: 16,
                },
                history: 0,
            },
        };
        (request, term)
    }

    /// Poll the grid until `needle` appears. A deadline rather than a sleep:
    /// the shells these tests drive take wildly different times to come up
    /// on a loaded runner.
    #[cfg(windows)]
    fn grid_contains(term: &TestTerm, needle: &str, patience: Duration) -> bool {
        let deadline = Instant::now() + patience;
        while Instant::now() < deadline {
            let text: String = term.lock().grid().display_iter().map(|cell| cell.c).collect();
            if text.contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        false
    }

    #[test]
    fn a_missing_working_directory_is_refused_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("gone");
        let (mut request, _) = request("sh", &[]);
        request.working_directory = Some(gone.clone());

        let opened: Result<NativeHandle, _> = Native::default().open(request);

        let error = opened.err().expect("a missing directory opens nothing");
        assert!(matches!(&error, SessionError::MissingDirectory(path) if *path == gone));
        assert!(error.to_string().contains("no longer exists"));
    }

    /// A native PTY dies with its handle, so it can neither be left running
    /// nor taken over by another session.
    #[test]
    fn a_native_terminal_neither_detaches_nor_is_taken_over() {
        let (mut request, term) = request("sh", &[]);
        request.shell = None;
        let screen = Screen { term, listener: VoidListener, ..request.screen };
        let running = AttachRequest { terminal: "t".into(), colors: request.colors, screen };

        let attached: Result<NativeHandle, _> = Native::default().attach(running);

        assert!(matches!(
            attached.err(),
            Some(SessionError::Unsupported { operation: Operation::Attach, .. })
        ));
        assert!(SessionBackend::<NativeHandle>::adoptable(&Native::default()).is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_job_under_a_real_shell_reads_as_the_foreground() {
        use std::time::{Duration, Instant};

        // `-f` skips the user's rc files, whose plugins run transient
        // foreground jobs of their own that would race the probe.
        let (request, _term) = request("/bin/zsh", &["-f", "-i"]);
        let session = open(request, None, |_, _, _| {}).unwrap();

        // Absolute path: a `sleep` from PATH may be a differently-named
        // multi-call binary (uutils coreutils).
        session.write(b"/bin/sleep 30\n".to_vec());
        let deadline = Instant::now() + Duration::from_secs(10);
        let program = loop {
            let program = session.foreground().filter(|_| session.signals().foreground_job);
            if let Some(program) = program.filter(|program| program.argv.len() == 2) {
                break program;
            }
            assert!(Instant::now() < deadline, "the job never became the foreground");
            std::thread::sleep(Duration::from_millis(10));
        };

        assert_eq!(program.argv, ["/bin/sleep", "30"]);
        assert_eq!(
            program.cwd.and_then(|cwd| cwd.canonicalize().ok()),
            std::env::current_dir().ok().and_then(|cwd| cwd.canonicalize().ok())
        );
    }

    /// A WSL launch opens cold while its distro has no spare ready, so a
    /// spare that is still starting never holds a tab up.
    #[cfg(windows)]
    #[test]
    fn a_launch_with_no_spare_ready_opens_cold() {
        let (request, term) = request("cmd.exe", &["/q", "/k", "echo opened cold"]);
        let spare = Some(wsl_spare::launch_with_no_spare_ready("spare-less"));

        let _handle = open(request, spare, |_, _, _| {}).expect("open the pty");

        assert!(
            grid_contains(&term, "opened cold", Duration::from_secs(20)),
            "the cold launch never ran"
        );
    }

    /// The shell's newest child is what a Windows terminal is running, so
    /// the foreground program names the command the user started last.
    #[cfg(windows)]
    #[test]
    fn the_foreground_program_is_the_command_the_shell_started() {
        let (request, term) = request("cmd.exe", &["/q", "/k", "prompt $g"]);
        let handle = open(request, None, |_, _, _| {}).expect("open the pty");
        handle.write(b"ping -n 60 127.0.0.1\r\n".to_vec());

        let deadline = Instant::now() + Duration::from_secs(30);
        let program = loop {
            let program = handle.foreground();
            if program.as_ref().is_some_and(|p| p.argv.iter().any(|arg| arg.contains("ping"))) {
                break program.unwrap();
            }
            assert!(
                Instant::now() < deadline,
                "the foreground never named ping (last: {program:?}, screen: {})",
                term.lock().grid().display_iter().map(|cell| cell.c).collect::<String>()
            );
            std::thread::sleep(Duration::from_millis(100));
        };

        assert_eq!(program.side, alacritree_session_backend::Side::Native);
        assert!(program.pid.is_some());
        assert_eq!(
            program
                .cwd
                .as_deref()
                .map(|cwd| cwd.to_string_lossy().trim_end_matches('\\').to_lowercase()),
            std::env::current_dir().ok().map(|cwd| cwd.to_string_lossy().to_lowercase())
        );
    }
}
