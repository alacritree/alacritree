//! Codex's running sub-agents, read from the rollout files it keeps in
//! `CODEX_HOME`.
//!
//! Each thread's rollout sits under `sessions/<year>/<month>/<day>/` and
//! opens with a `session_meta` line, which names a sub-agent's parent thread,
//! directory, nickname and task. The last `task_started`, `task_complete` or
//! `turn_aborted` event says whether it still works.
//!
//! Codex registers no process, so only the directory ties a thread to a
//! session: the working directory of the program the session's backend
//! reports in the foreground. Two Codex sessions in one directory list none,
//! since nothing says which thread is whose. Only the newest `RECENT_DAYS`
//! day folders are read.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use alacritree_subagents::tail::Endings;
use alacritree_subagents::{Home, Host, Scan, Side, Subagent, SubagentError, SubagentSource};
use serde::Deserialize;

/// How many day folders, newest first, a scan reads.
const RECENT_DAYS: usize = 2;

/// `[integrations.codex]`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct RawCodex {
    /// List the sub-agents each Codex session has running in the sidebar,
    /// under that session's row, or under its workspace's row when the
    /// session is the only one there. They are read from the rollouts Codex
    /// keeps in `CODEX_HOME` or `~/.codex`, inside the distro for a session
    /// running in WSL. A session's sub-agents are matched by the directory
    /// Codex started in, so two Codex sessions in one directory list none.
    subagents: bool,
}

impl Default for RawCodex {
    fn default() -> Self {
        Self { subagents: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CodexConfig {
    pub subagents: bool,
}

impl RawCodex {
    pub fn resolve(self) -> CodexConfig {
        CodexConfig { subagents: self.subagents }
    }
}

/// Where Codex keeps its state on each side: `CODEX_HOME` when set, the way
/// Codex itself resolves it, else `~/.codex`.
pub fn home() -> Home {
    Home::new("CODEX_HOME", ".codex")
}

/// Reads Codex's sub-agents across scans, remembering each rollout's header,
/// which never changes once written, and how each last ended.
#[derive(Debug)]
pub struct CodexSubagents {
    home: Home,
    /// Each rollout's header, `None` for a thread that is not a sub-agent.
    headers: HashMap<PathBuf, Option<Header>>,
    endings: Endings,
}

impl CodexSubagents {
    /// Reads the Codex files in `home`, usually [`home()`].
    pub fn at(home: Home) -> Self {
        Self { home, headers: HashMap::new(), endings: Endings::default() }
    }
}

/// What a sub-agent's `session_meta` says about it.
#[derive(Debug, Clone)]
struct Header {
    id: String,
    parent: String,
    cwd: PathBuf,
    nickname: Option<String>,
    task: Option<String>,
    role: Option<String>,
}

impl Header {
    fn subagent(&self) -> Subagent {
        let task = self.task.as_deref().and_then(|path| path.rsplit('/').find(|s| !s.is_empty()));
        let name = task.or(self.nickname.as_deref()).unwrap_or(&self.id).to_owned();
        let detail = [self.nickname.as_deref(), self.role.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ");
        Subagent { id: self.id.clone(), name, detail: (!detail.is_empty()).then_some(detail) }
    }
}

/// A thread's running sub-agents, and when the last of them wrote.
#[derive(Default)]
struct Thread {
    cwd: PathBuf,
    agents: Vec<Subagent>,
    last_write: Option<SystemTime>,
}

impl SubagentSource for CodexSubagents {
    fn scan(&mut self, hosts: &[Host]) -> Scan {
        let mut scan = Scan::empty(hosts.len());
        let codex: Vec<(usize, &Side, &Path)> = hosts
            .iter()
            .enumerate()
            .filter(|(_, host)| host.agent == Some("codex"))
            .filter_map(|(i, host)| {
                Some((i, &host.foreground.side, host.foreground.cwd.as_deref()?))
            })
            .collect();
        let mut sides: Vec<&Side> = Vec::new();
        for &(_, side, _) in &codex {
            if !sides.contains(&side) {
                sides.push(side);
            }
        }
        let mut rollouts = Vec::new();
        for side in sides {
            let Some(home) = self.home.on(side) else { continue };
            match recent_rollouts(&home.join("sessions")) {
                Ok(found) => rollouts.extend(found.into_iter().map(|path| (side.clone(), path))),
                Err(e) => scan.errors.push(e),
            }
        }
        let threads = self.threads(&rollouts);
        self.endings.sweep();

        for &(i, side, cwd) in &codex {
            let same_place =
                |other_side: &Side, other: &Path| other_side == side && same_dir(side, other, cwd);
            let shared = codex.iter().filter(|(_, s, other)| same_place(s, other)).count() > 1;
            if shared {
                continue;
            }
            let newest = threads
                .iter()
                .filter(|((s, _), thread)| same_place(s, &thread.cwd))
                .map(|(_, thread)| thread)
                .max_by_key(|thread| thread.last_write);
            if let Some(thread) = newest {
                scan.running[i] = thread.agents.clone();
            }
        }
        scan
    }
}

impl CodexSubagents {
    /// The threads with a sub-agent running, keyed by their side and the
    /// top-level thread that started them, its sub-agents' own sub-agents
    /// folded in.
    fn threads(&mut self, rollouts: &[(Side, PathBuf)]) -> HashMap<(Side, String), Thread> {
        let listed: HashSet<&PathBuf> = rollouts.iter().map(|(_, path)| path).collect();
        self.headers.retain(|path, _| listed.contains(path));
        for (_, path) in rollouts {
            if !self.headers.contains_key(path)
                && let Some(header) = read_header(path)
            {
                self.headers.insert(path.clone(), header);
            }
        }

        let parent_of: HashMap<&str, &str> = self
            .headers
            .values()
            .flatten()
            .map(|header| (header.id.as_str(), header.parent.as_str()))
            .collect();

        let mut running: Vec<(&Side, &Header, Option<SystemTime>)> = Vec::new();
        for (side, path) in rollouts {
            let Some(Some(header)) = self.headers.get(path) else { continue };
            if self.endings.running(path, turn_running) {
                let written = fs::metadata(path).and_then(|stat| stat.modified()).ok();
                running.push((side, header, written));
            }
        }
        // Codex's thread ids are UUIDv7, so their order is the order the
        // threads started in.
        running.sort_by(|(_, a, _), (_, b, _)| a.id.cmp(&b.id));

        let mut threads: HashMap<(Side, String), Thread> = HashMap::new();
        for (side, header, written) in running {
            let root = root_of(&parent_of, &header.id).to_owned();
            let thread = threads.entry((side.clone(), root)).or_default();
            thread.cwd.clone_from(&header.cwd);
            thread.agents.push(header.subagent());
            thread.last_write = thread.last_write.max(written);
        }
        threads
    }
}

/// The top-level thread above `id`, following `parent_of` up. Bounded, so a
/// cycle in what the files claim still ends.
fn root_of<'a>(parent_of: &HashMap<&'a str, &'a str>, mut id: &'a str) -> &'a str {
    for _ in 0..parent_of.len() {
        match parent_of.get(id) {
            Some(parent) => id = parent,
            None => break,
        }
    }
    id
}

/// Whether `a` and `b` name one directory on `side`, the way either was
/// spelled. A distro's paths are compared by their components alone, since
/// this machine cannot resolve them.
fn same_dir(side: &Side, a: &Path, b: &Path) -> bool {
    a == b
        || *side == Side::Native
            && matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

/// The rollouts in the newest `RECENT_DAYS` day folders.
fn recent_rollouts(sessions: &Path) -> Result<Vec<PathBuf>, SubagentError> {
    let mut days = Vec::new();
    'years: for year in dated_dirs(sessions)? {
        for month in dated_dirs(&year)? {
            for day in dated_dirs(&month)? {
                days.push(day);
                if days.len() == RECENT_DAYS {
                    break 'years;
                }
            }
        }
    }
    let mut rollouts = Vec::new();
    for day in days {
        rollouts.extend(list(&day)?.into_iter().filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
        }));
    }
    Ok(rollouts)
}

/// The numbered folders in `dir`, newest first.
fn dated_dirs(dir: &Path) -> Result<Vec<PathBuf>, SubagentError> {
    let mut dirs: Vec<PathBuf> = list(dir)?
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect();
    dirs.sort_by(|a, b| b.cmp(a));
    Ok(dirs)
}

/// The entries of `dir`, none when it does not exist.
fn list(dir: &Path) -> Result<Vec<PathBuf>, SubagentError> {
    match fs::read_dir(dir) {
        Ok(entries) => Ok(entries.flatten().map(|entry| entry.path()).collect()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(source) => Err(SubagentError::List { path: dir.to_path_buf(), source }),
    }
}

/// The rollout's header: `Some(None)` for a thread that is not a sub-agent,
/// and `None` while there is nothing to remember yet, a first line still
/// being written or a file that cannot be read.
fn read_header(path: &Path) -> Option<Option<Header>> {
    let mut line = String::new();
    BufReader::new(File::open(path).ok()?).read_line(&mut line).ok()?;
    if !line.ends_with('\n') {
        return None;
    }
    let Ok(meta) = serde_json::from_str::<MetaLine>(&line) else {
        return Some(None);
    };
    Some(meta.header())
}

#[derive(Deserialize)]
struct MetaLine {
    #[serde(rename = "type")]
    kind: String,
    payload: Meta,
}

#[derive(Deserialize)]
struct Meta {
    id: String,
    cwd: PathBuf,
    #[serde(default)]
    thread_source: Option<String>,
    #[serde(default)]
    parent_thread_id: Option<String>,
    #[serde(default)]
    agent_nickname: Option<String>,
    #[serde(default)]
    agent_path: Option<String>,
    #[serde(default)]
    agent_role: Option<String>,
    /// Older rollouts carry the spawn only here, as
    /// `{"subagent":{"thread_spawn":{...}}}`.
    #[serde(default)]
    source: Option<serde_json::Value>,
}

impl MetaLine {
    fn header(self) -> Option<Header> {
        if self.kind != "session_meta" {
            return None;
        }
        let meta = self.payload;
        let spawn =
            meta.source.as_ref().and_then(|source| source.pointer("/subagent/thread_spawn"));
        let spawned = |key: &str| spawn?.get(key)?.as_str().map(str::to_owned);
        let parent = meta.parent_thread_id.or_else(|| spawned("parent_thread_id"))?;
        if meta.thread_source.as_deref().is_some_and(|source| source != "subagent") {
            return None;
        }
        Some(Header {
            id: meta.id,
            parent,
            cwd: meta.cwd,
            nickname: meta.agent_nickname.or_else(|| spawned("agent_nickname")),
            task: meta.agent_path.or_else(|| spawned("agent_path")),
            role: meta.agent_role.or_else(|| spawned("agent_role")),
        })
    }
}

/// Whether a rollout line opens a turn, closes one, or is not about turns.
fn turn_running(line: &[u8]) -> Option<bool> {
    #[derive(Deserialize)]
    struct Line {
        #[serde(rename = "type")]
        kind: String,
        #[serde(default)]
        payload: Option<Payload>,
    }
    #[derive(Deserialize)]
    struct Payload {
        #[serde(default, rename = "type")]
        kind: String,
    }
    let line: Line = serde_json::from_slice(line).ok()?;
    if line.kind != "event_msg" {
        return None;
    }
    match line.payload?.kind.as_str() {
        "task_started" => Some(true),
        "task_complete" | "turn_aborted" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use alacritree_subagents::ForegroundProgram;

    use super::*;

    const ROOT: &str = "01a0f1ee-e22f-7642-9e86-af6d804c2fd9";

    struct Dir(tempfile::TempDir);

    impl Dir {
        fn new() -> Self {
            Self(tempfile::tempdir().unwrap())
        }

        fn rollout(&self, day: &str, id: &str, lines: &[String]) -> PathBuf {
            let dir = self.0.path().join("sessions").join(day);
            fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!("rollout-2026-09-30T15-39-57-{id}.jsonl"));
            fs::write(&path, lines.iter().map(|line| format!("{line}\n")).collect::<String>())
                .unwrap();
            path
        }

        fn scanner(&self) -> CodexSubagents {
            CodexSubagents::at(home().with(Side::Native, Some(self.0.path().to_path_buf())))
        }
    }

    fn child_meta(id: &str, parent: &str, cwd: &str, nickname: &str, task: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-09-30T13:39:57.206Z","type":"session_meta","payload":{{"id":"{id}","parent_thread_id":"{parent}","cwd":"{cwd}","thread_source":"subagent","agent_nickname":"{nickname}","agent_path":"{task}","source":{{"subagent":{{"thread_spawn":{{"parent_thread_id":"{parent}","depth":1}}}}}}}}}}"#
        )
    }

    fn root_meta(id: &str, cwd: &str) -> String {
        format!(
            r#"{{"type":"session_meta","payload":{{"id":"{id}","cwd":"{cwd}","thread_source":"user","source":"vscode"}}}}"#
        )
    }

    fn event(kind: &str) -> String {
        format!(r#"{{"type":"event_msg","payload":{{"type":"{kind}","turn_id":"t"}}}}"#)
    }

    const TOKENS: &str = r#"{"type":"event_msg","payload":{"type":"token_count","info":{}}}"#;
    const MESSAGE: &str =
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[]}}"#;

    /// A session whose backend reports `agent` in the foreground on `side`,
    /// working in `cwd`.
    fn host(side: Side, agent: Option<&'static str>, cwd: &str) -> Host {
        let foreground = ForegroundProgram {
            side,
            pid: Some(42),
            argv: vec![agent.unwrap_or("bash").into()],
            cwd: Some(PathBuf::from(cwd)),
        };
        Host { foreground, agent }
    }

    fn codex_in(cwd: &str) -> Host {
        host(Side::Native, Some("codex"), cwd)
    }

    fn names(scan: &Scan) -> Vec<Vec<&str>> {
        scan.running.iter().map(|agents| agents.iter().map(|a| a.name.as_str()).collect()).collect()
    }

    #[test]
    fn a_turn_runs_from_task_started_to_its_end() {
        let run = |lines: &[&str]| {
            let text = lines.iter().map(|line| format!("{line}\n")).collect::<String>();
            alacritree_subagents::tail::last_turn_running(
                &mut io::Cursor::new(text.as_bytes()),
                text.len() as u64,
                turn_running,
            )
            .unwrap()
        };
        assert!(run(&[&event("task_started"), MESSAGE, TOKENS]));
        assert!(!run(&[&event("task_started"), MESSAGE, &event("task_complete"), TOKENS]));
        assert!(!run(&[&event("task_started"), &event("turn_aborted")]));
        assert!(run(&[&event("task_complete"), &event("task_started")]));
    }

    #[test]
    fn a_codex_session_lists_its_threads_running_sub_agents() {
        let home = Dir::new();
        home.rollout("2026/09/30", ROOT, &[root_meta(ROOT, "/work/app"), event("task_started")]);
        home.rollout("2026/09/30", "01a0f28a-0000", &[
            child_meta("01a0f28a-0000", ROOT, "/work/app", "Wegener", "/root/sequencing"),
            event("task_started"),
            TOKENS.into(),
        ]);
        home.rollout("2026/09/30", "01a0f28b-0000", &[
            child_meta("01a0f28b-0000", ROOT, "/work/app", "Parfit", "/root/readiness"),
            event("task_started"),
            event("task_complete"),
        ]);
        let hosts = [codex_in("/work/app"), host(Side::Native, None, "/work/app")];
        let scan = home.scanner().scan(&hosts);
        assert_eq!(names(&scan), [vec!["sequencing"], vec![]]);
        assert_eq!(scan.running[0][0].detail.as_deref(), Some("Wegener"));
    }

    /// A distro's Codex keeps its rollouts in the distro, so each side's
    /// session lists the threads in its own home, though both sides spell the
    /// directory alike.
    #[test]
    fn each_side_lists_the_threads_in_its_own_home() {
        let (here, distro) = (Dir::new(), Dir::new());
        here.rollout("2026/09/30", "01a0f28a-0000", &[
            child_meta("01a0f28a-0000", ROOT, "/work/app", "Wegener", "/root/native"),
            event("task_started"),
        ]);
        distro.rollout("2026/09/30", "01a0f28b-0000", &[
            child_meta("01a0f28b-0000", ROOT, "/work/app/", "Parfit", "/root/linux"),
            event("task_started"),
        ]);
        let ubuntu = Side::Wsl("Ubuntu".into());
        let home = home()
            .with(Side::Native, Some(here.0.path().to_path_buf()))
            .with(ubuntu.clone(), Some(distro.0.path().to_path_buf()));
        let hosts = [codex_in("/work/app"), host(ubuntu, Some("codex"), "/work/app")];
        let scan = CodexSubagents::at(home).scan(&hosts);
        assert_eq!(names(&scan), [vec!["native"], vec!["linux"]]);
    }

    #[test]
    fn a_sub_agents_own_sub_agents_count_toward_the_top_thread() {
        let home = Dir::new();
        let child = "01a0f28a-0000";
        home.rollout("2026/09/30", child, &[
            child_meta(child, ROOT, "/work/app", "Wegener", "/root/sequencing"),
            event("task_started"),
        ]);
        home.rollout("2026/09/30", "01a0f28c-0000", &[
            child_meta("01a0f28c-0000", child, "/work/app", "Boole", "/root/sequencing/parse"),
            event("task_started"),
        ]);
        let scan = home.scanner().scan(&[codex_in("/work/app")]);
        assert_eq!(names(&scan), [vec!["sequencing", "parse"]]);
    }

    #[test]
    fn two_codex_sessions_in_one_directory_list_nothing() {
        let home = Dir::new();
        home.rollout("2026/09/30", "01a0f28a-0000", &[
            child_meta("01a0f28a-0000", ROOT, "/work/app", "Wegener", "/root/sequencing"),
            event("task_started"),
        ]);
        let scan = home.scanner().scan(&[codex_in("/work/app"), codex_in("/work/app")]);
        assert_eq!(names(&scan), [Vec::<&str>::new(), vec![]]);
    }

    #[test]
    fn only_the_newest_day_folders_are_read() {
        let home = Dir::new();
        let meta = |id: &str| child_meta(id, ROOT, "/work/app", "N", &format!("/root/{id}"));
        home.rollout("2026/09/28", "old", &[meta("old"), event("task_started")]);
        home.rollout("2026/09/29", "mid", &[meta("mid"), event("task_started")]);
        home.rollout("2026/10/01", "new", &[meta("new"), event("task_started")]);
        let scan = home.scanner().scan(&[codex_in("/work/app")]);
        assert_eq!(names(&scan), [vec!["mid", "new"]]);
    }

    #[test]
    fn a_header_still_being_written_is_read_again_later() {
        let home = Dir::new();
        let path = home.rollout("2026/09/30", "01a0f28a-0000", &[]);
        fs::write(&path, r#"{"type":"session_meta","payload":{"id":"#).unwrap();
        let mut scanner = home.scanner();
        let hosts = [codex_in("/work/app")];
        assert_eq!(names(&scanner.scan(&hosts)), [Vec::<&str>::new()]);
        let lines = [
            child_meta("01a0f28a-0000", ROOT, "/work/app", "Wegener", "/root/sequencing"),
            event("task_started"),
        ];
        fs::write(&path, lines.iter().map(|line| format!("{line}\n")).collect::<String>()).unwrap();
        assert_eq!(names(&scanner.scan(&hosts)), [vec!["sequencing"]]);
    }

    #[test]
    fn the_listing_is_on_unless_the_table_turns_it_off() {
        assert!(RawCodex::default().resolve().subagents);
        let raw: RawCodex = toml::from_str("subagents = false").unwrap();
        assert!(!raw.resolve().subagents);
    }
}
