//! Copies the PTY byte stream on its way through, so the OSC tap can see the
//! sequences `vte`'s `ansi` layer drops.
//!
//! The copy is all this layer does. The read thread can be holding the
//! terminal lock, so it hands the bytes to a thread of their own rather than
//! waiting on anything. Buffers cycle through a return channel rather than
//! being allocated per read.

use std::io::{self, Read};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};

use alacritty_terminal::event::{OnResize, WindowSize};
use alacritty_terminal::tty::{ChildEvent, EventedPty, EventedReadWrite};
use polling::{Event, PollMode, Poller};

/// How many reads may be in flight before the tap starts dropping them.
/// Deep enough to absorb a burst, shallow enough that a tap thread which
/// cannot keep up costs bounded memory rather than growing memory.
pub(crate) const QUEUE_DEPTH: usize = 64;

/// One read's worth of bytes on its way to the tap.
pub(crate) struct Chunk {
    pub(crate) bytes: Vec<u8>,
    /// Set when the queue was full and a read was dropped before this one.
    /// A hole in the stream makes framing meaningless, so the tap resets.
    pub(crate) gap_before: bool,
}

/// The read thread's half of the tap.
pub(crate) struct TapHandle {
    tx: SyncSender<Chunk>,
    pool: Receiver<Vec<u8>>,
    dropped: bool,
    dead: bool,
}

impl TapHandle {
    pub(crate) fn new(tx: SyncSender<Chunk>, pool: Receiver<Vec<u8>>) -> Self {
        Self { tx, pool, dropped: false, dead: false }
    }

    /// Never blocks and never fails the read it was called from.
    pub(crate) fn offer(&mut self, filled: &[u8]) {
        if self.dead {
            return;
        }

        let mut bytes = self.pool.try_recv().unwrap_or_default();
        bytes.clear();
        bytes.extend_from_slice(filled);
        let chunk = Chunk { bytes, gap_before: self.dropped };
        match self.tx.try_send(chunk) {
            Ok(()) => self.dropped = false,
            Err(TrySendError::Full(_)) => {
                if !self.dropped {
                    log::debug!("osc tap fell behind; dropping reads until it catches up");
                }
                self.dropped = true;
            },
            // The tap thread is gone. Reads carry on without it.
            Err(TrySendError::Disconnected(_)) => self.dead = true,
        }
    }
}

pub(crate) struct TeeReader<R> {
    inner: R,
    tap: Option<TapHandle>,
}

impl<R: Read> TeeReader<R> {
    pub(crate) fn new(inner: R, tap: Option<TapHandle>) -> Self {
        Self { inner, tap }
    }
}

impl<R: Read> Read for TeeReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            if let Some(tap) = self.tap.as_mut() {
                tap.offer(&buf[..n]);
            }
        }
        Ok(n)
    }
}

pub(crate) struct PtyReader<P> {
    pty: P,
}

impl<P: EventedReadWrite> Read for PtyReader<P> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.pty.reader().read(buf)
    }
}

pub(crate) struct TeePty<P> {
    reader: TeeReader<PtyReader<P>>,
}

impl<P: EventedReadWrite> TeePty<P> {
    pub(crate) fn new(inner: P, tap: Option<TapHandle>) -> Self {
        Self { reader: TeeReader::new(PtyReader { pty: inner }, tap) }
    }
}

impl<P: EventedReadWrite> EventedReadWrite for TeePty<P> {
    type Reader = TeeReader<PtyReader<P>>;
    type Writer = P::Writer;

    unsafe fn register(
        &mut self,
        poll: &Arc<Poller>,
        interest: Event,
        poll_opts: PollMode,
    ) -> io::Result<()> {
        unsafe { self.reader.inner.pty.register(poll, interest, poll_opts) }
    }

    fn reregister(
        &mut self,
        poll: &Arc<Poller>,
        interest: Event,
        poll_opts: PollMode,
    ) -> io::Result<()> {
        self.reader.inner.pty.reregister(poll, interest, poll_opts)
    }

    fn deregister(&mut self, poll: &Arc<Poller>) -> io::Result<()> {
        self.reader.inner.pty.deregister(poll)
    }

    fn reader(&mut self) -> &mut Self::Reader {
        &mut self.reader
    }

    fn writer(&mut self) -> &mut Self::Writer {
        self.reader.inner.pty.writer()
    }
}

impl<P: EventedPty> EventedPty for TeePty<P> {
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        self.reader.inner.pty.next_child_event()
    }
}

impl<P: EventedReadWrite + OnResize> OnResize for TeePty<P> {
    fn on_resize(&mut self, window_size: WindowSize) {
        self.reader.inner.pty.on_resize(window_size);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::sync::mpsc;

    use super::*;

    /// A reader that hands out short reads, the way a PTY does.
    struct Choppy {
        data: Vec<u8>,
        at: usize,
        step: usize,
    }

    impl Read for Choppy {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let end = (self.at + self.step).min(self.data.len());
            let n = (end - self.at).min(buf.len());
            buf[..n].copy_from_slice(&self.data[self.at..self.at + n]);
            self.at += n;
            Ok(n)
        }
    }

    struct Acknowledged<R> {
        inner: R,
        acknowledgements: mpsc::Receiver<()>,
        wait_for_acknowledgement: bool,
    }

    impl<R: Read> Read for Acknowledged<R> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.wait_for_acknowledgement {
                self.acknowledgements.recv().expect("tap receiver acknowledges each chunk");
                self.wait_for_acknowledgement = false;
            }

            let n = self.inner.read(buf)?;
            self.wait_for_acknowledgement = n > 0;
            Ok(n)
        }
    }

    fn drain_all(mut reader: impl Read) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = [0u8; 64];
        loop {
            match reader.read(&mut buf).unwrap() {
                0 => return out,
                n => out.extend_from_slice(&buf[..n]),
            }
        }
    }

    #[test]
    fn every_byte_reaches_the_reader_unchanged() {
        let data: Vec<u8> = (0..10_000u32).map(|n| (n % 251) as u8).collect();
        let (tx, rx) = mpsc::sync_channel(QUEUE_DEPTH);
        let (_pool_tx, pool_rx) = mpsc::channel();
        let (acknowledgements, acknowledgement_rx) = mpsc::sync_channel(0);
        let reader = TeeReader::new(
            Acknowledged {
                inner: Choppy { data: data.clone(), at: 0, step: 7 },
                acknowledgements: acknowledgement_rx,
                wait_for_acknowledgement: false,
            },
            Some(TapHandle::new(tx, pool_rx)),
        );

        let reader = std::thread::spawn(move || drain_all(reader));
        let mut seen = Vec::new();
        for chunk in rx {
            seen.extend_from_slice(&chunk.bytes);
            acknowledgements.send(()).unwrap();
        }

        assert_eq!(reader.join().unwrap(), data);
        assert_eq!(seen, data);
    }

    #[test]
    fn a_full_queue_drops_a_chunk_and_flags_the_gap() {
        let (tx, rx) = mpsc::sync_channel(1);
        let (_pool_tx, pool_rx) = mpsc::channel();
        let mut tap = TapHandle::new(tx, pool_rx);

        tap.offer(b"first");
        tap.offer(b"lost");

        let first = rx.recv().unwrap();
        assert_eq!(first.bytes, b"first");
        assert!(!first.gap_before);

        tap.offer(b"after");
        let after = rx.recv().unwrap();
        assert_eq!(after.bytes, b"after");
        assert!(after.gap_before, "the drop has to be announced");
    }

    #[test]
    fn a_disconnected_tap_skips_later_copying() {
        let (tx, rx) = mpsc::sync_channel(QUEUE_DEPTH);
        drop(rx);
        let (pool_tx, pool_rx) = mpsc::channel();
        let mut tap = TapHandle::new(tx, pool_rx);

        tap.offer(b"first");

        let mut recycled = Vec::with_capacity(4096);
        recycled.extend_from_slice(b"sentinel");
        pool_tx.send(recycled).unwrap();

        tap.offer(b"second");

        let preserved = tap.pool.try_recv().expect("dead tap consumed the pool");
        assert_eq!(preserved, b"sentinel");
    }

    #[test]
    fn a_returned_buffer_is_reused_instead_of_allocated() {
        let (tx, rx) = mpsc::sync_channel(QUEUE_DEPTH);
        let (pool_tx, pool_rx) = mpsc::channel();
        let mut tap = TapHandle::new(tx, pool_rx);

        tap.offer(b"first");
        let mut recycled = rx.recv().unwrap().bytes;
        recycled.clear();
        recycled.reserve(4096);
        let addr = recycled.as_ptr();
        pool_tx.send(recycled).unwrap();

        tap.offer(b"second");
        let next = rx.recv().unwrap();
        assert_eq!(next.bytes, b"second");
        assert_eq!(next.bytes.as_ptr(), addr, "the pooled allocation was not reused");
    }
}
