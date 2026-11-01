//! The replica taskwarrior keeps its database in. A stamp is the size and
//! mtime of the database and its wal. The shm changes on a read, so it is
//! not a stamp. An empty wal holds no commit, so it is not one either. A
//! missing database file means the directory is not a replica.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use alacritree_common::jobs::Blocking;
use alacritree_common::side::Side;
use alacritree_common::wsl;
use alacritree_tasks::TaskError;

use crate::Taskwarrior;

const KEY: &str = "data.location";
const DB: &str = "taskchampion.sqlite3";
const WAL: &str = "taskchampion.sqlite3-wal";

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl FileStamp {
    fn of(path: &Path) -> Self {
        let meta = path.metadata().ok();
        Self {
            len: meta.as_ref().map(|m| m.len()).unwrap_or(0),
            modified: meta.and_then(|m| m.modified().ok()),
        }
    }
}

/// Size and mtime of the database and its wal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplicaStamp {
    db: FileStamp,
    wal: Option<FileStamp>,
}

impl ReplicaStamp {
    /// The bytes a caller compares between listings.
    pub fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(self.db.len.to_le_bytes());
        out.extend(self.db.modified.map(time_bytes).unwrap_or([0; 16]));
        match &self.wal {
            Some(wal) => {
                out.push(1);
                out.extend(wal.len.to_le_bytes());
                out.extend(wal.modified.map(time_bytes).unwrap_or([0; 16]));
            },
            None => out.push(0),
        }
        out
    }
}

fn time_bytes(time: SystemTime) -> [u8; 16] {
    let (sign, dur) = match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(dur) => (1_u8, dur),
        Err(e) => (0, e.duration()),
    };
    let mut out = [0; 16];
    out[0] = sign;
    out[1..9].copy_from_slice(&dur.as_secs().to_le_bytes());
    out[9..13].copy_from_slice(&dur.subsec_nanos().to_le_bytes());
    out
}

/// The directory `task _get rc.data.location` named, on the side that named it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replica {
    pub dir: PathBuf,
}

impl Replica {
    pub fn locate(
        tw: &Taskwarrior,
        side: &Side,
        blocking: &Blocking,
    ) -> Result<Option<Self>, TaskError> {
        let printed = tw.on(side.clone(), blocking).rc_value(KEY)?;
        let Some(location) = printed.filter(|s| !s.is_empty()) else { return Ok(None) };
        Ok(Some(Self { dir: side_path(side, &location) }))
    }

    /// None when the database file is not there. A later locate runs then.
    pub fn stamp(&self) -> Option<ReplicaStamp> {
        let db = self.dir.join(DB);
        let wal = self.dir.join(WAL);
        let wal = wal.is_file().then(|| FileStamp::of(&wal)).filter(|s| s.len > 0);
        db.is_file().then(|| ReplicaStamp { db: FileStamp::of(&db), wal })
    }
}

fn side_path(side: &Side, location: &str) -> PathBuf {
    match side {
        Side::Native => PathBuf::from(location),
        Side::Wsl(distro) => wsl::linux_to_windows(location, distro),
    }
}
