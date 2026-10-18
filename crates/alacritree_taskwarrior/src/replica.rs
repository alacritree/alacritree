use crate::Taskwarrior;
use alacritree_common::jobs::Blocking;
use alacritree_common::side::Side;
use alacritree_tasks::TaskError;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
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
/// Size and mtime of the database and its wal. The shm changes on a read, so it is not a stamp. An
/// empty wal holds no commit, so it is not one either.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplicaStamp {
    db: FileStamp,
    wal: Option<FileStamp>,
}
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

    pub fn stamp(&self) -> Option<ReplicaStamp> {
        let db = self.dir.join(DB);
        let wal = self.dir.join(WAL);
        let wal = wal.is_file().then(|| FileStamp::of(&wal)).filter(|s| s.len > 0);
        db.is_file().then(|| ReplicaStamp { db: FileStamp::of(&db), wal })
    }
}
const KEY: &str = "data.location";
const DB: &str = "taskchampion.sqlite3";
const WAL: &str = "taskchampion.sqlite3-wal";
fn side_path(side: &Side, location: &str) -> PathBuf {
    match side {
        Side::Native => PathBuf::from(location),
        Side::Wsl(distro) => wsl_path(distro, location),
    }
}
fn wsl_path(distro: &str, location: &str) -> PathBuf {
    let mut prefix = String::from("\\wsl.localhost\\");
    prefix.insert(0, '\\');
    prefix.push_str(distro);
    let rest = location.trim_start_matches('/').replace('/', "\\");
    PathBuf::from(prefix).join(rest)
}
