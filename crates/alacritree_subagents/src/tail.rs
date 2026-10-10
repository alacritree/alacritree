//! Whether a transcript's last turn is still going, read from its end.
//!
//! A harness appends one JSON entry per line and ends a turn with an entry of
//! its own, so the answer sits in the last entry that is a turn at all. Each
//! backend says which lines those are; this module finds the last one
//! without reading the whole file, and [`Endings`] keeps the answer until the
//! file changes, so a scan of settled agents costs one `stat` each.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// How much of a transcript's tail the first read takes. A turn's last entry
/// is usually far smaller, but a tool result carrying a whole file is not, so
/// a window holding no turn grows until one does.
const TAIL: u64 = 64 * 1024;

/// A transcript whose last this-many bytes hold no turn counts as running,
/// rather than being read whole on every scan.
const TAIL_LIMIT: u64 = 16 * 1024 * 1024;

/// Whether the last turn in a `len`-byte transcript is still going, by
/// `verdict` on its lines from the last: `Some(running)` for a line that
/// settles it, `None` for one that is not a turn, a line still being written
/// included. A transcript with no turn yet has only just started.
pub fn last_turn_running(
    file: &mut (impl Read + Seek),
    len: u64,
    mut verdict: impl FnMut(&[u8]) -> Option<bool>,
) -> io::Result<bool> {
    let mut window = TAIL.min(len);
    loop {
        let start = len - window;
        file.seek(SeekFrom::Start(start))?;
        let mut buf = Vec::new();
        file.by_ref().take(window).read_to_end(&mut buf)?;
        let mut lines = buf.split(|&b| b == b'\n');
        // Unless the window reaches the start, its first line is cut.
        if start > 0 {
            lines.next();
        }
        if let Some(running) = lines.rev().find_map(&mut verdict) {
            return Ok(running);
        }
        if start == 0 || window >= TAIL_LIMIT {
            return Ok(true);
        }
        window = (window * 4).min(len).min(TAIL_LIMIT);
    }
}

/// How each transcript last ended, so a scan re-reads only the ones that
/// changed.
#[derive(Debug, Default)]
pub struct Endings {
    seen: HashMap<PathBuf, Ending>,
    /// Asked about since the last sweep.
    touched: HashSet<PathBuf>,
}

#[derive(Debug, Clone, Copy)]
struct Ending {
    len: u64,
    modified: Option<SystemTime>,
    running: bool,
}

impl Endings {
    /// Whether the transcript at `path` is mid-turn, by `verdict` as
    /// [`last_turn_running`] applies it. It is read again only once its size
    /// or mtime moves, and one that cannot be read counts as finished without
    /// being remembered, so the next scan tries it again.
    pub fn running(&mut self, path: &Path, verdict: impl FnMut(&[u8]) -> Option<bool>) -> bool {
        self.touched.insert(path.to_path_buf());
        let Ok(stat) = fs::metadata(path) else {
            return false;
        };
        let (len, modified) = (stat.len(), stat.modified().ok());
        if let Some(ending) = self.seen.get(path)
            && ending.len == len
            && ending.modified == modified
        {
            return ending.running;
        }
        let Ok(running) =
            File::open(path).and_then(|mut file| last_turn_running(&mut file, len, verdict))
        else {
            return false;
        };
        self.seen.insert(path.to_path_buf(), Ending { len, modified, running });
        running
    }

    /// Forget every transcript not asked about since the last sweep.
    pub fn sweep(&mut self) {
        let touched = std::mem::take(&mut self.touched);
        self.seen.retain(|path, _| touched.contains(path));
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// `open` and `close` lines open and settle a turn; anything else is not
    /// one.
    fn verdict(line: &[u8]) -> Option<bool> {
        match line {
            b"open" => Some(true),
            b"close" => Some(false),
            _ => None,
        }
    }

    fn running(text: &str) -> bool {
        last_turn_running(&mut Cursor::new(text.as_bytes()), text.len() as u64, verdict).unwrap()
    }

    #[test]
    fn the_last_turn_line_decides() {
        assert!(running("open\n"));
        assert!(!running("open\nclose\n"));
        assert!(!running("open\nclose\nnoise\nnoise\n"));
        assert!(running("close\nopen\nnoise\n"));
    }

    #[test]
    fn a_transcript_with_no_turn_yet_is_starting() {
        assert!(running(""));
        assert!(running("noise\n"));
    }

    #[test]
    fn a_turn_further_back_than_the_first_window_is_still_found() {
        let text = format!("open\nclose\n{}\n", "x".repeat(TAIL as usize * 3));
        assert!(!running(&text));
    }

    #[test]
    fn a_window_that_starts_mid_line_skips_the_cut_line() {
        // The first window opens inside "notclose", on a piece that reads as
        // a closing line: kept, it would settle a turn that never closed.
        let padding = "x".repeat(TAIL as usize - 7);
        let text = format!("open\nnotclose\n{padding}\n");
        assert_eq!(text.len() as u64 - TAIL, "open\nnot".len() as u64);
        assert!(running(&text));
    }

    #[test]
    fn endings_reread_a_transcript_only_once_it_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.jsonl");
        fs::write(&path, "open\n").unwrap();
        let mut endings = Endings::default();
        let reads = std::cell::Cell::new(0);
        let counted = |line: &[u8]| {
            reads.set(reads.get() + 1);
            verdict(line)
        };
        assert!(endings.running(&path, counted));
        let first = reads.get();
        assert!(endings.running(&path, counted));
        assert_eq!(reads.get(), first, "an unchanged transcript was read again");
        fs::write(&path, "open\nclose\n").unwrap();
        assert!(!endings.running(&path, verdict));
    }

    #[test]
    fn a_sweep_forgets_what_the_scan_did_not_ask_about() {
        let mut endings = Endings::default();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.jsonl");
        fs::write(&path, "open\n").unwrap();
        endings.running(&path, verdict);
        endings.sweep();
        assert_eq!(endings.seen.len(), 1);
        endings.sweep();
        assert!(endings.seen.is_empty());
    }
}
