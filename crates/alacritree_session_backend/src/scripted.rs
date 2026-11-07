//! A session backend whose terminals are records a test reads back.
//!
//! Nothing here starts a process. Each open or attach adds a [`Record`] that
//! the handle writes into, and a test reads it through [`Scripted::session`]
//! after the handle has moved into the app.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use alacritty_terminal::event::{Event, EventListener, WindowSize};

use crate::{
    ForegroundProgram, OpenRequest, Operation, Screen, SessionBackend, SessionBackendKind,
    SessionError, SessionHandle, ShellCommand, Signals, Terminal,
};

/// What a session sent its scripted terminal, in the order it sent it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    Input(Vec<u8>),
    Resize { columns: u16, lines: u16 },
}

/// Everything a scripted terminal was started with and asked to do.
#[derive(Debug, Clone, Default)]
pub struct Record {
    pub session_id: Option<u64>,
    pub shell: Option<ShellCommand>,
    pub working_directory: Option<PathBuf>,
    pub env: HashMap<String, String>,
    /// The size an open started the terminal at, as `(columns, lines)`.
    pub opened_at: Option<(u16, u16)>,
    /// The terminal an attach or a watch took over.
    pub terminal: Option<Terminal>,
    pub watching: bool,
    pub calls: Vec<Call>,
    pub focused: bool,
    pub killed: bool,
    pub detached: bool,
    /// The handle is gone, whether or not anything ended the terminal first.
    pub dropped: bool,
    pub foreground: Option<ForegroundProgram>,
    pub signals: Signals,
}

/// One started terminal: what it records, and the listener it reports
/// through.
struct Started {
    record: Arc<Mutex<Record>>,
    emit: Box<dyn Fn(Event) + Send>,
}

#[derive(Default)]
struct State {
    failures: VecDeque<SessionError>,
    adoptable: Vec<Terminal>,
    started: Vec<Started>,
}

impl State {
    fn opened_for(&self, session_id: u64) -> &Started {
        self.started
            .iter()
            .find(|started| lock(&started.record).session_id == Some(session_id))
            .expect("no terminal for that session")
    }
}

impl Record {
    /// Every byte written, in order.
    pub fn input(&self) -> Vec<u8> {
        self.calls
            .iter()
            .filter_map(|call| match call {
                Call::Input(bytes) => Some(bytes.as_slice()),
                Call::Resize { .. } => None,
            })
            .flatten()
            .copied()
            .collect()
    }
}

#[derive(Clone, Default)]
pub struct Scripted {
    state: Arc<Mutex<State>>,
}

pub struct ScriptedHandle {
    record: Arc<Mutex<Record>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Scripted {
    /// The next open, attach or watch fails with `error`. Failures are taken in the
    /// order they were pushed.
    pub fn fail_next(&self, error: SessionError) -> &Self {
        lock(&self.state).failures.push_back(error);
        self
    }

    pub fn set_adoptable(&self, terminals: Vec<Terminal>) -> &Self {
        lock(&self.state).adoptable = terminals;
        self
    }

    /// Every terminal started so far, in the order they started.
    pub fn records(&self) -> Vec<Record> {
        lock(&self.state).started.iter().map(|started| lock(&started.record).clone()).collect()
    }

    /// The terminal opened for `session_id`.
    pub fn session(&self, session_id: u64) -> Option<Record> {
        self.records().into_iter().find(|record| record.session_id == Some(session_id))
    }

    pub fn set_signals(&self, session_id: u64, signals: Signals) -> &Self {
        self.with_record(session_id, |record| record.signals = signals);
        self
    }

    pub fn set_foreground(&self, session_id: u64, program: Option<ForegroundProgram>) -> &Self {
        self.with_record(session_id, |record| record.foreground = program);
        self
    }

    /// The program in `session_id`'s terminal exits with `status`, which
    /// reaches the app the way a native child's exit does.
    pub fn exit(&self, session_id: u64, status: ExitStatus) {
        (lock(&self.state).opened_for(session_id).emit)(Event::ChildExit(status));
    }

    fn with_record(&self, session_id: u64, change: impl FnOnce(&mut Record)) {
        change(&mut lock(&lock(&self.state).opened_for(session_id).record));
    }

    fn start<L, H>(&self, record: Record, listener: L) -> Result<H, SessionError>
    where
        L: EventListener + Send + 'static,
        H: From<ScriptedHandle>,
    {
        let mut state = lock(&self.state);
        if let Some(error) = state.failures.pop_front() {
            return Err(error);
        }
        let record = Arc::new(Mutex::new(record));
        state.started.push(Started {
            record: record.clone(),
            emit: Box::new(move |event| listener.send_event(event)),
        });
        Ok(ScriptedHandle { record }.into())
    }

    fn take_over<L, H>(
        &self,
        terminal: &Terminal,
        screen: Screen<L>,
        watching: bool,
    ) -> Result<H, SessionError>
    where
        L: EventListener + Send + 'static,
        H: From<ScriptedHandle>,
    {
        let record = Record { terminal: Some(terminal.clone()), watching, ..Record::default() };
        self.start(record, screen.listener)
    }
}

impl<H: From<ScriptedHandle>> SessionBackend<H> for Scripted {
    fn open<L>(&self, request: OpenRequest<L>) -> Result<H, SessionError>
    where
        L: EventListener + Send + 'static,
    {
        let size = request.screen.window_size;
        let record = Record {
            session_id: Some(request.session_id),
            shell: request.shell,
            working_directory: request.working_directory,
            env: request.env,
            opened_at: Some((size.num_cols, size.num_lines)),
            ..Record::default()
        };
        self.start(record, request.screen.listener)
    }

    fn adoptable(&self) -> Vec<Terminal> {
        lock(&self.state).adoptable.clone()
    }

    fn attach<L>(&self, terminal: &Terminal, screen: Screen<L>) -> Result<H, SessionError>
    where
        L: EventListener + Send + 'static,
    {
        self.take_over(terminal, screen, false)
    }

    fn watch<L>(&self, terminal: &Terminal, screen: Screen<L>) -> Result<H, SessionError>
    where
        L: EventListener + Send + 'static,
    {
        self.take_over(terminal, screen, true)
    }
}

impl SessionHandle for ScriptedHandle {
    /// A watching session never types into its terminal.
    fn write(&self, bytes: Vec<u8>) {
        let mut record = lock(&self.record);
        if !record.watching && !bytes.is_empty() {
            record.calls.push(Call::Input(bytes));
        }
    }

    /// A watching session never sizes its terminal.
    fn resize(&mut self, size: WindowSize) {
        let mut record = lock(&self.record);
        if !record.watching {
            record.calls.push(Call::Resize { columns: size.num_cols, lines: size.num_lines });
        }
    }

    fn set_focused(&self, focused: bool) -> bool {
        lock(&self.record).focused = focused;
        false
    }

    fn kill(&mut self) {
        lock(&self.record).killed = true;
    }

    /// Only a terminal the session took over can be let go of. One it opened
    /// is the session's own, as a native PTY is.
    fn detach(&mut self) -> Result<(), SessionError> {
        let mut record = lock(&self.record);
        if record.terminal.is_none() {
            return Err(SessionError::Unsupported {
                backend: SessionBackendKind::Scripted,
                operation: Operation::Detach,
            });
        }
        record.detached = true;
        Ok(())
    }

    fn foreground(&self) -> Option<ForegroundProgram> {
        lock(&self.record).foreground.clone()
    }

    fn signals(&self) -> Signals {
        lock(&self.record).signals
    }
}

impl Drop for ScriptedHandle {
    fn drop(&mut self) {
        lock(&self.record).dropped = true;
    }
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::sync::FairMutex;
    use alacritty_terminal::term::{Config, Term};

    use super::*;
    use crate::{ProcessTree, Side};

    struct Size;

    impl alacritty_terminal::grid::Dimensions for Size {
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

    fn screen() -> Screen<VoidListener> {
        let term = Arc::new(FairMutex::new(Term::new(Config::default(), &Size, VoidListener)));
        let window_size =
            WindowSize { num_lines: 24, num_cols: 80, cell_width: 8, cell_height: 16 };
        Screen { term, listener: VoidListener, window_size }
    }

    fn request(session_id: u64) -> OpenRequest<VoidListener> {
        OpenRequest {
            session_id,
            shell: None,
            escape_args: false,
            working_directory: None,
            env: HashMap::new(),
            wsl_probe: None,
            process_tree: ProcessTree::default(),
            screen: screen(),
        }
    }

    fn terminal() -> Terminal {
        Terminal { side: Side::Native, id: "t1".to_string() }
    }

    /// A watching session follows a terminal without disturbing it, so the
    /// program sees neither its keys nor its pane size.
    #[test]
    fn a_watched_terminal_takes_no_input_and_no_resize() {
        let backend = Scripted::default();
        let mut handle: ScriptedHandle = backend.watch(&terminal(), screen()).unwrap();
        handle.write(b"ls\n".to_vec());
        handle.resize(screen().window_size);

        assert!(backend.records()[0].calls.is_empty());
    }

    /// Detach leaves a program running, so it is refused for a terminal the
    /// session opened itself and allowed for one it took over.
    #[test]
    fn only_a_terminal_taken_over_detaches() {
        let backend = Scripted::default();
        let mut opened: ScriptedHandle = backend.open(request(1)).unwrap();
        let mut attached: ScriptedHandle = backend.attach(&terminal(), screen()).unwrap();

        assert!(matches!(
            opened.detach(),
            Err(SessionError::Unsupported { operation: Operation::Detach, .. })
        ));
        attached.detach().unwrap();

        let records = backend.records();
        assert!(!records[0].detached && !records[0].killed);
        assert!(records[1].detached && !records[1].killed);
    }

    #[test]
    fn a_scripted_failure_answers_the_next_open_only() {
        let backend = Scripted::default();
        backend.fail_next(SessionError::Scripted("no terminal".into()));

        let first: Result<ScriptedHandle, _> = backend.open(request(1));
        let second: Result<ScriptedHandle, _> = backend.open(request(2));

        assert_eq!(first.err().map(|e| e.to_string()).as_deref(), Some("no terminal"));
        assert!(second.is_ok());
        assert!(backend.session(1).is_none());
        assert!(backend.session(2).is_some());
    }
}
