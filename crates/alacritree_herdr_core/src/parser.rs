//! The thread that feeds one session's `Term` from its stream.
//!
//! It mirrors `alacritty_terminal`'s event loop: the same read limits under
//! the `FairMutex` lease, the same synchronized-update timeout, and the same
//! wakeups and child exit through the app's listener. A change to those in
//! the native loop must also go here. It sends CREDIT for the bytes it has
//! parsed, so the core's flow control follows the parser and not the socket.

use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::event_loop::{MAX_LOCKED_READ, READ_BUFFER_SIZE};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Term;
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use serde::Deserialize;

/// What the demux thread hands a session's parser.
pub(crate) enum Chunk {
    Output(Vec<u8>),
    /// The stream is over. `None` answers this client's own CLOSE.
    Closed(Option<CloseReason>),
}

/// Why the core ended a stream.
#[derive(Debug, Deserialize)]
pub(crate) struct CloseReason {
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

/// The line a session ends on when its connection to the core drops.
const LOST: &[u8] = b"\x1b[0m\r\n[alacritree] lost the connection to herdr-core\r\n";

pub(crate) fn spawn<L>(
    term: Arc<FairMutex<Term<L>>>,
    listener: L,
    stream: Receiver<Chunk>,
    credit: impl Fn(usize) + Send + 'static,
) -> std::io::Result<()>
where
    L: EventListener + Send + 'static,
{
    std::thread::Builder::new()
        .name("hc-parser".into())
        .spawn(move || Parser { term, listener, stream, credit }.run())
        .map(drop)
}

struct Parser<L: EventListener, C> {
    term: Arc<FairMutex<Term<L>>>,
    listener: L,
    stream: Receiver<Chunk>,
    credit: C,
}

impl<L: EventListener, C: Fn(usize)> Parser<L, C> {
    fn run(self) {
        let mut parser = Processor::<StdSyncHandler>::new();
        loop {
            let timeout = parser
                .sync_timeout()
                .sync_timeout()
                .map(|at| at.saturating_duration_since(Instant::now()));
            let next = match timeout {
                Some(timeout) => self.stream.recv_timeout(timeout),
                None => self.stream.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            let closed = match next {
                Ok(Chunk::Output(bytes)) => self.parse(&mut parser, bytes),
                Ok(Chunk::Closed(reason)) => Some(reason),
                Err(RecvTimeoutError::Timeout) => {
                    parser.stop_sync(&mut *self.term.lock());
                    self.listener.send_event(Event::Wakeup);
                    continue;
                },
                Err(RecvTimeoutError::Disconnected) => {
                    parser.advance(&mut *self.term.lock(), LOST);
                    self.listener.send_event(Event::Wakeup);
                    return;
                },
            };
            if let Some(reason) = closed {
                return self.close(reason);
            }
        }
    }

    /// Parse `first` and whatever queued behind it, under one lease. Answers
    /// the close that arrived in the queue, if one did.
    fn parse(
        &self,
        parser: &mut Processor<StdSyncHandler>,
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
            match self.stream.try_recv() {
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
            match self.stream.try_recv() {
                Ok(Chunk::Output(bytes)) => pending = bytes,
                Ok(Chunk::Closed(reason)) => {
                    closed = Some(reason);
                    break;
                },
                Err(_) => break,
            }
        }
        drop(terminal);

        (self.credit)(processed);
        if parser.sync_bytes_count() < processed && processed > 0 {
            self.listener.send_event(Event::Wakeup);
        }
        closed
    }

    /// The program ended, or this client let go of the stream. Only an end
    /// the core reports reaches the app as a child exit.
    fn close(self, reason: Option<CloseReason>) {
        let Some(reason) = reason else { return };
        self.listener.send_event(Event::ChildExit(reason.status()));
        self.term.lock().exit();
        self.listener.send_event(Event::Wakeup);
    }
}
