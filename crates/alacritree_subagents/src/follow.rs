//! Folding a transcript's lines into a state, reading each line once.
//!
//! Some answers need a transcript's whole history rather than its last turn:
//! whether background work an agent started has reported back is decided by
//! two lines that can sit any distance apart. [`Follow`] keeps each file's
//! state with the offset it was read to, so the next read folds in only the
//! lines appended since. A line still being written is left for the next
//! read, and a file that shrank was rewritten, so it is read again from the
//! start.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// How much one read takes at a time, so a long history is folded without
/// holding all of it. A single line longer than this doubles the read until
/// it fits.
const CHUNK: u64 = 1024 * 1024;

#[derive(Debug)]
pub struct Follow<S> {
    files: HashMap<PathBuf, Followed<S>>,
    /// Asked about since the last sweep.
    touched: HashSet<PathBuf>,
}

impl<S> Default for Follow<S> {
    fn default() -> Self {
        Self { files: HashMap::new(), touched: HashSet::new() }
    }
}

#[derive(Debug, Default)]
struct Followed<S> {
    offset: u64,
    state: S,
}

impl<S: Default> Follow<S> {
    /// `path`'s state once every whole line added since the last read is
    /// folded in by `fold`, or `None` for a file that cannot be read.
    pub fn read(&mut self, path: &Path, fold: impl FnMut(&mut S, &[u8])) -> Option<&S> {
        self.touched.insert(path.to_path_buf());
        let len = fs::metadata(path).ok()?.len();
        let followed = self.files.entry(path.to_path_buf()).or_default();
        if len < followed.offset {
            *followed = Followed::default();
        }
        if len > followed.offset {
            let mut file = File::open(path).ok()?;
            followed.offset = fold_from(&mut file, followed.offset, len, &mut followed.state, fold)
                .unwrap_or(followed.offset);
        }
        Some(&followed.state)
    }

    /// Forget every transcript not asked about since the last sweep.
    pub fn sweep(&mut self) {
        let touched = std::mem::take(&mut self.touched);
        self.files.retain(|path, _| touched.contains(path));
    }
}

/// Fold the whole lines between `offset` and `len` into `state`, and answer
/// the offset just past the last of them.
fn fold_from<S>(
    file: &mut (impl Read + Seek),
    mut offset: u64,
    len: u64,
    state: &mut S,
    mut fold: impl FnMut(&mut S, &[u8]),
) -> io::Result<u64> {
    let mut chunk = CHUNK;
    while offset < len {
        let want = (len - offset).min(chunk);
        file.seek(SeekFrom::Start(offset))?;
        let mut buf = Vec::new();
        file.by_ref().take(want).read_to_end(&mut buf)?;
        match buf.iter().rposition(|&b| b == b'\n') {
            Some(end) => {
                for line in buf[..end].split(|&b| b == b'\n').filter(|line| !line.is_empty()) {
                    fold(state, line);
                }
                offset += end as u64 + 1;
                chunk = CHUNK;
            },
            // The rest is one line still being written.
            None if want == len - offset => break,
            None => chunk *= 2,
        }
    }
    Ok(offset)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn collect(lines: &mut Vec<String>, line: &[u8]) {
        lines.push(String::from_utf8_lossy(line).into_owned());
    }

    #[test]
    fn each_line_is_folded_once_and_a_partial_one_waits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.jsonl");
        fs::write(&path, "one\ntwo\nthr").unwrap();
        let mut follow = Follow::<Vec<String>>::default();
        assert_eq!(follow.read(&path, collect).unwrap(), &["one", "two"]);
        fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"ee\nfour\n").unwrap();
        assert_eq!(follow.read(&path, collect).unwrap(), &["one", "two", "three", "four"]);
        assert_eq!(follow.read(&path, collect).unwrap().len(), 4, "nothing new, nothing folded");
    }

    #[test]
    fn a_file_that_shrank_is_read_again_from_the_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.jsonl");
        fs::write(&path, "one\ntwo\n").unwrap();
        let mut follow = Follow::<Vec<String>>::default();
        follow.read(&path, collect);
        fs::write(&path, "new\n").unwrap();
        assert_eq!(follow.read(&path, collect).unwrap(), &["new"]);
    }

    #[test]
    fn a_line_longer_than_a_chunk_is_folded_whole() {
        let long = "x".repeat(CHUNK as usize * 2 + 3);
        let text = format!("one\n{long}\ntwo\n");
        let mut lines = Vec::new();
        let mut cursor = io::Cursor::new(text.as_bytes());
        let end = fold_from(&mut cursor, 0, text.len() as u64, &mut lines, collect).unwrap();
        assert_eq!(end, text.len() as u64);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1].len(), long.len());
    }

    #[test]
    fn a_sweep_forgets_what_the_scan_did_not_ask_about() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.jsonl");
        fs::write(&path, "one\n").unwrap();
        let mut follow = Follow::<Vec<String>>::default();
        follow.read(&path, collect);
        follow.sweep();
        assert_eq!(follow.files.len(), 1);
        follow.sweep();
        assert!(follow.files.is_empty());
    }
}
