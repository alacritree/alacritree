//! The thread that feeds one session's `Term` from its stream.
//!
//! It mirrors `alacritty_terminal`'s event loop: the same read limits under
//! the `FairMutex` lease, the same synchronized-update timeout, and the same
//! wakeups and child exit through the app's listener. A change to those in
//! the native loop must also go here. It sends CREDIT for the bytes it has
//! parsed, so the core's flow control follows the parser and not the socket.
//! After a drop or a loss it alone takes up the new stream, so nothing from
//! the earlier one reaches the grid after the reset. A session that opened
//! with no connection waits for its first stream the same way.

use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::event_loop::{MAX_LOCKED_READ, READ_BUFFER_SIZE};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Term;
use alacritty_terminal::vte::ansi::{Handler, Processor, StdSyncHandler};
use serde::Deserialize;

use crate::{Attached, Ending, Resumed};

/// What the demux thread hands a session's parser.
pub(crate) enum Chunk {
    Output(Vec<u8>),
    /// The stream is over. `None` answers this client's own CLOSE.
    Closed(Option<CloseReason>),
}

/// Why the core ended a stream. The default is a terminal that is gone with
/// no exit to report.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct CloseReason {
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    exit_code: Option<i32>,
    #[serde(default)]
    signal: Option<i32>,
}

impl CloseReason {
    /// The program's exit as the native loop reports a child's. A program
    /// that left no code, such as one the core killed, reads as a failure.
    fn status(&self) -> ExitStatus {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            match (self.exit_code, self.signal) {
                (Some(code), _) => ExitStatus::from_raw((code & 0xff) << 8),
                (None, Some(signal)) => ExitStatus::from_raw(signal & 0x7f),
                (None, None) => ExitStatus::from_raw(1 << 8),
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;
            let _ = self.signal;
            ExitStatus::from_raw(self.exit_code.unwrap_or(1) as u32)
        }
    }
}

/// The line a session shows while its connection to the core is down. The
/// reset that follows a reattach clears it.
const RECONNECTING: &[u8] =
    b"\x1b[0m\r\n[alacritree] lost the connection to herdr-core, reconnecting\r\n";

/// Follow `stream`, or with none, the first stream the session's side
/// gives once its connection is up.
pub(crate) fn spawn<L>(
    term: Arc<FairMutex<Term<L>>>,
    listener: L,
    stream: Option<Receiver<Chunk>>,
    attached: Arc<Attached>,
) -> std::io::Result<()>
where
    L: EventListener + Send + 'static,
{
    std::thread::Builder::new()
        .name("hc-parser".into())
        .spawn(move || Parser { term, listener, attached }.run(stream))
        .map(drop)
}

struct Parser<L: EventListener> {
    term: Arc<FairMutex<Term<L>>>,
    listener: L,
    attached: Arc<Attached>,
}

/// How a stream ended.
enum End {
    /// The core closed it, or answered this client's own CLOSE with `None`.
    Closed(Option<CloseReason>),
    /// The connection it rode on dropped.
    Dropped,
}

impl<L: EventListener> Parser<L> {
    /// Follow the session's stream, its first one once the side is up, and
    /// each that replaces it after a drop or a loss. A reattach repaints the
    /// whole screen and its scrollback, so the grid starts over from a reset.
    fn run(self, stream: Option<Receiver<Chunk>>) {
        let mut stream = match stream.map_or_else(|| self.attached.resume(false), Resumed::Stream) {
            Resumed::Stream(first) => first,
            Resumed::Gone => return self.close(CloseReason::default()),
            Resumed::Released | Resumed::Dropped => return,
        };
        loop {
            let (mut next, reason) = match self.follow(&stream) {
                End::Closed(None) => return,
                End::Closed(Some(reason)) => {
                    let next = match self.attached.ending() {
                        Ending::Final => return self.close(reason),
                        Ending::Released => return,
                        Ending::Lost => {
                            let why = reason.reason.as_deref().unwrap_or_default();
                            self.show(format!(
                                "\x1b[0m\r\n[alacritree] herdr-core lost the program ({why}); \
                                 waiting for a new shell\r\n"
                            ));
                            self.attached.restart()
                        },
                        Ending::Dropped => Resumed::Dropped,
                    };
                    (next, Some(reason))
                },
                End::Dropped => (Resumed::Dropped, None),
            };
            if let Resumed::Dropped = next {
                self.show(RECONNECTING);
                next = self.attached.resume(true);
            }
            stream = match next {
                Resumed::Stream(next) => next,
                // The reason the core closed the stream with, if it did,
                // carries the exit code.
                Resumed::Gone => return self.close(reason.unwrap_or_default()),
                Resumed::Released | Resumed::Dropped => return,
            };
            self.term.lock().reset_state();
        }
    }

    fn follow(&self, stream: &Receiver<Chunk>) -> End {
        let mut parser = Processor::<StdSyncHandler>::new();
        loop {
            let timeout = parser
                .sync_timeout()
                .sync_timeout()
                .map(|at| at.saturating_duration_since(Instant::now()));
            let next = match timeout {
                Some(timeout) => stream.recv_timeout(timeout),
                None => stream.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            let closed = match next {
                Ok(Chunk::Output(bytes)) => self.parse(&mut parser, stream, bytes),
                Ok(Chunk::Closed(reason)) => Some(reason),
                Err(RecvTimeoutError::Timeout) => {
                    parser.stop_sync(&mut *self.term.lock());
                    self.listener.send_event(Event::Wakeup);
                    continue;
                },
                Err(RecvTimeoutError::Disconnected) => return End::Dropped,
            };
            if let Some(reason) = closed {
                return End::Closed(reason);
            }
        }
    }

    /// Parse `first` and whatever queued behind it, under one lease. Answers
    /// the close that arrived in the queue, if one did.
    fn parse(
        &self,
        parser: &mut Processor<StdSyncHandler>,
        stream: &Receiver<Chunk>,
        first: Vec<u8>,
    ) -> Option<Option<CloseReason>> {
        let _lease = self.term.lease();
        let mut pending = first;
        let mut closed = None;

        // While the UI holds the grid, gather what is queued rather than
        // waiting, as the native loop keeps reading its PTY.
        let mut terminal = loop {
            if let Some(terminal) = self.term.try_lock_unfair() {
                break terminal;
            }
            if pending.len() >= READ_BUFFER_SIZE {
                break self.term.lock_unfair();
            }
            match stream.try_recv() {
                Ok(Chunk::Output(bytes)) => pending.extend(bytes),
                Ok(Chunk::Closed(reason)) => {
                    closed = Some(reason);
                    break self.term.lock_unfair();
                },
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => {
                    break self.term.lock_unfair();
                },
            }
        };

        let mut processed = 0;
        loop {
            parser.advance(&mut *terminal, &pending);
            processed += pending.len();
            if processed >= MAX_LOCKED_READ || closed.is_some() {
                break;
            }
            match stream.try_recv() {
                Ok(Chunk::Output(bytes)) => pending = bytes,
                Ok(Chunk::Closed(reason)) => {
                    closed = Some(reason);
                    break;
                },
                Err(_) => break,
            }
        }
        drop(terminal);

        self.attached.credit(processed);
        if parser.sync_bytes_count() < processed && processed > 0 {
            self.listener.send_event(Event::Wakeup);
        }
        closed
    }

    /// Print a line of alacritree's own into the grid.
    fn show(&self, line: impl AsRef<[u8]>) {
        Processor::<StdSyncHandler>::new().advance(&mut *self.term.lock(), line.as_ref());
        self.listener.send_event(Event::Wakeup);
    }

    /// The program ended, which reaches the app as a child exit.
    fn close(self, reason: CloseReason) {
        self.listener.send_event(Event::ChildExit(reason.status()));
        self.term.lock().exit();
        self.listener.send_event(Event::Wakeup);
    }
}
