//! Claude Code's running sub-agents, read from the files it keeps in its
//! config directory.
//!
//! Each Claude Code process registers itself in `sessions/<pid>.json` with
//! the conversation it holds and the directory it started in, so a process
//! is tied to its session by pid, through the app's [`Lineage`]. That
//! conversation's sub-agents write their transcripts under
//! `projects/<directory slug>/<session id>/subagents/`, each `agent-<id>.jsonl`
//! beside an `agent-<id>.meta.json` that says what it was asked to do, and a
//! workflow's agents one level deeper, in `subagents/workflows/<run id>/`.
//! None of this is a published format, so a file that does not parse is
//! skipped rather than failing the scan, and only the fields named here are
//! read.
//!
//! A sub-agent counts as running until it hands its report back, which ends
//! its run with a tool result Claude Code marks `toolEndsTurn`, or until the
//! user interrupts it. A plain reply also stops it, unless background work it
//! started is still out: it is then waiting on that work, whose report comes
//! back as a `<task-notification>` and resumes it. Telling the two apart takes
//! the transcript's whole history, so each one is folded line by line, and a
//! scan reads only what was appended since the last.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use alacritree_subagents::follow::Follow;
use alacritree_subagents::{Host, Lineage, Scan, Subagent, SubagentError, SubagentSource};
use serde::Deserialize;

/// What Claude Code writes as a user turn when the user stops an agent,
/// mid-reply or mid-tool.
const INTERRUPTED: &str = "[Request interrupted by user";

/// Where Claude Code keeps its state: `CLAUDE_CONFIG_DIR` when set, the way
/// Claude Code itself resolves it, else `~/.claude`.
pub fn config_dir() -> Option<PathBuf> {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => home::home_dir().map(|home| home.join(".claude")),
    }
}

/// A Claude Code process, as it registered itself.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Registration {
    pid: u32,
    /// The conversation the process holds now.
    session_id: String,
    /// The directory it started in, which names its transcripts' folder.
    cwd: PathBuf,
}

/// Every registered Claude Code process. The registry outlives a process
/// that crashed, so an entry does not prove its pid is still Claude Code:
/// only a pid the app's lineage places in a session counts.
fn registrations(config_dir: &Path) -> Result<Vec<Registration>, SubagentError> {
    let dir = config_dir.join("sessions");
    let Some(paths) = list(&dir)? else {
        return Ok(Vec::new());
    };
    Ok(paths
        .into_iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            let raw: RawRegistration =
                serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
            Some(Registration { pid: raw.pid, session_id: raw.session_id, cwd: raw.cwd })
        })
        .collect())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRegistration {
    pid: u32,
    session_id: String,
    cwd: PathBuf,
}

/// What an agent's meta file says about it.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Meta {
    agent_type: Option<String>,
    description: Option<String>,
    workflow_phase: Option<String>,
}

impl Meta {
    /// A row names it by its description; the hover text adds its workflow
    /// and phase, or for a plain sub-agent its type.
    fn subagent(self, id: String, workflow: Option<&str>) -> Subagent {
        let detail = match workflow {
            Some(workflow) => Some(
                [Some(workflow), self.workflow_phase.as_deref()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            None => self.agent_type.clone(),
        };
        let name = self.description.or(self.agent_type).unwrap_or_else(|| id.clone());
        Subagent { id, name, detail }
    }
}

/// Reads Claude Code's sub-agents across scans, remembering where each
/// session's transcripts are and how each transcript last ended.
#[derive(Debug)]
pub struct ClaudeSubagents {
    dir: Option<PathBuf>,
    /// Each session's folder under `projects/`, once found.
    folders: HashMap<String, PathBuf>,
    progress: Follow<Progress>,
}

impl ClaudeSubagents {
    /// Reads the config directory [`config_dir`] resolves.
    pub fn new() -> Self {
        Self::at(config_dir())
    }

    pub fn at(dir: Option<PathBuf>) -> Self {
        Self { dir, folders: HashMap::new(), progress: Follow::default() }
    }
}

impl Default for ClaudeSubagents {
    fn default() -> Self {
        Self::new()
    }
}

/// A running sub-agent and when it started, for ordering.
type Found = (Option<SystemTime>, Subagent);

impl SubagentSource for ClaudeSubagents {
    fn scan(&mut self, hosts: &[Host], lineage: &dyn Lineage) -> Scan {
        let mut scan = Scan::empty(hosts.len());
        let Some(dir) = self.dir.clone() else {
            return scan;
        };
        let registered = registrations(&dir).unwrap_or_else(|e| {
            scan.errors.push(e);
            Vec::new()
        });
        let mut reached = HashSet::new();
        for session in registered {
            let Some(host) = lineage.host_of(session.pid).filter(|&host| host < hosts.len()) else {
                continue;
            };
            reached.insert(session.session_id.clone());
            match self.running(&dir, &session) {
                Ok(agents) => scan.running[host].extend(agents),
                Err(e) => scan.errors.push(e),
            }
        }
        self.progress.sweep();
        self.folders.retain(|id, _| reached.contains(id));
        scan
    }
}

impl ClaudeSubagents {
    /// `session`'s running sub-agents, oldest first.
    fn running(
        &mut self,
        config_dir: &Path,
        session: &Registration,
    ) -> Result<Vec<Subagent>, SubagentError> {
        let Some(folder) = self.folder(config_dir, session) else {
            return Ok(Vec::new());
        };
        let conversation = folder.join(&session.session_id);
        let root = conversation.join("subagents");
        let mut found = Vec::new();
        self.collect(&root, None, &mut found)?;
        if let Some(runs) = list(&root.join("workflows"))? {
            let scripts = list(&conversation.join("workflows").join("scripts"))?
                .unwrap_or_default()
                .into_iter()
                .filter_map(|path| Some(path.file_stem()?.to_str()?.to_owned()))
                .collect::<Vec<_>>();
            for run in runs {
                let Some(id) = run.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                let workflow = workflow_name(&scripts, id);
                self.collect(&run, workflow.as_deref(), &mut found)?;
            }
        }
        found.sort_by(|(a_at, a), (b_at, b)| (a_at, &a.id).cmp(&(b_at, &b.id)));
        Ok(found.into_iter().map(|(_, agent)| agent).collect())
    }

    /// The folder holding `session`'s transcripts. The slug Claude Code
    /// derives from the directory is tried first, and every folder searched
    /// when it misses, which is what a long path, shortened with a hash,
    /// needs.
    fn folder(&mut self, config_dir: &Path, session: &Registration) -> Option<PathBuf> {
        if let Some(folder) = self.folders.get(&session.session_id) {
            return Some(folder.clone());
        }
        let projects = config_dir.join("projects");
        let transcript = format!("{}.jsonl", session.session_id);
        let guess = projects.join(slug(&session.cwd));
        let folder = if guess.join(&transcript).is_file() {
            guess
        } else {
            fs::read_dir(&projects)
                .ok()?
                .flatten()
                .map(|entry| entry.path())
                .find(|dir| dir.join(&transcript).is_file())?
        };
        self.folders.insert(session.session_id.clone(), folder.clone());
        Some(folder)
    }

    fn collect(
        &mut self,
        dir: &Path,
        workflow: Option<&str>,
        found: &mut Vec<Found>,
    ) -> Result<(), SubagentError> {
        for path in list(dir)?.unwrap_or_default() {
            let Some(id) = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix("agent-")?.strip_suffix(".jsonl"))
            else {
                continue;
            };
            let running = self.progress.read(&path, Progress::fold).is_some_and(Progress::running);
            if !running {
                continue;
            }
            let (meta, started) = read_meta(&dir.join(format!("agent-{id}.meta.json")));
            found.push((started, meta.subagent(id.to_owned(), workflow)));
        }
        Ok(())
    }
}

/// The folder name Claude Code files a directory's transcripts under: every
/// character other than an ASCII letter or digit becomes `-`.
fn slug(cwd: &Path) -> String {
    cwd.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// A workflow run's name, from the script Claude Code saved it as,
/// `<name>-<run id>.js`.
fn workflow_name(scripts: &[String], run: &str) -> Option<String> {
    let suffix = format!("-{run}");
    scripts
        .iter()
        .find_map(|stem| stem.strip_suffix(&suffix).filter(|name| !name.is_empty()))
        .map(str::to_owned)
}

/// The entries of `dir`, or `None` when it does not exist, which is the
/// common case for a session that has started no sub-agent.
fn list(dir: &Path) -> Result<Option<Vec<PathBuf>>, SubagentError> {
    match fs::read_dir(dir) {
        Ok(entries) => Ok(Some(entries.flatten().map(|entry| entry.path()).collect())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(SubagentError::List { path: dir.to_path_buf(), source }),
    }
}

/// An agent's meta file and when it was written, which is when the agent
/// started: Claude Code writes it once, at spawn.
fn read_meta(path: &Path) -> (Meta, Option<SystemTime>) {
    let Ok(text) = fs::read_to_string(path) else {
        return (Meta::default(), None);
    };
    let started = fs::metadata(path).and_then(|stat| stat.modified()).ok();
    (serde_json::from_str(&text).unwrap_or_default(), started)
}

/// How far a sub-agent has got, folded from its transcript.
#[derive(Debug, Default)]
struct Progress {
    turn: Turn,
    /// Background work it started that has not reported back, by the id the
    /// report will name.
    pending: HashSet<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Turn {
    /// No turn yet: the agent has only just started.
    #[default]
    Starting,
    /// A reply still streaming, or a tool call out.
    Open,
    /// Stopped with a plain reply.
    Parked,
    /// Handed its report back, or interrupted.
    Ended,
}

impl Progress {
    fn running(&self) -> bool {
        match self.turn {
            Turn::Starting | Turn::Open => true,
            Turn::Parked => !self.pending.is_empty(),
            Turn::Ended => false,
        }
    }

    /// Fold in the entry on `line`. One that is not a turn, an attachment or
    /// a summary, changes nothing, and neither does one that does not parse.
    fn fold(&mut self, line: &[u8]) {
        let Ok(entry) = serde_json::from_slice::<Entry>(line) else {
            return;
        };
        let message = entry.message.unwrap_or_default();
        match entry.kind.as_str() {
            "assistant" => {
                let calls_tool = message.blocks().any(|block| block.kind == "tool_use");
                let stopped = matches!(
                    message.stop_reason.as_deref(),
                    Some("end_turn" | "stop_sequence" | "refusal")
                );
                self.turn = if stopped && !calls_tool { Turn::Parked } else { Turn::Open };
            },
            "user" => {
                for text in message.texts() {
                    for id in notified_tasks(text) {
                        self.pending.remove(id);
                    }
                }
                if let Some(id) = entry.tool_use_result.and_then(Launched::id) {
                    self.pending.insert(id);
                }
                let interrupted = message.texts().any(|text| text.starts_with(INTERRUPTED));
                self.turn = if entry.tool_ends_turn == Some(true) || interrupted {
                    Turn::Ended
                } else {
                    Turn::Open
                };
            },
            _ => {},
        }
    }
}

/// The ids of the background work a `<task-notification>` reports on.
fn notified_tasks(text: &str) -> impl Iterator<Item = &str> {
    let body = text.find("<task-notification>").map_or("", |at| &text[at..]);
    body.split("<task-id>")
        .skip(1)
        .filter_map(|rest| rest.split_once("</task-id>").map(|(id, _)| id.trim()))
}

#[derive(Deserialize)]
struct Entry {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    message: Option<Message>,
    #[serde(default, rename = "toolEndsTurn")]
    tool_ends_turn: Option<bool>,
    #[serde(default, rename = "toolUseResult", deserialize_with = "launched")]
    tool_use_result: Option<Launched>,
}

/// What a tool's result says it left running: a background command, or an
/// agent or workflow launched to run on its own.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Launched {
    background_task_id: Option<String>,
    status: Option<String>,
    agent_id: Option<String>,
    task_id: Option<String>,
}

impl Launched {
    fn id(self) -> Option<String> {
        let launched = self.status.as_deref() == Some("async_launched");
        self.background_task_id.or(if launched { self.agent_id.or(self.task_id) } else { None })
    }
}

/// A `toolUseResult`, which is an object for most tools but a bare string
/// for an error, so any shape but the one read here counts as none.
fn launched<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Option<Launched>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Shape {
        Launched(Launched),
        Other(serde::de::IgnoredAny),
    }
    Ok(match Shape::deserialize(de)? {
        Shape::Launched(launched) => Some(launched),
        Shape::Other(_) => None,
    })
}

#[derive(Default, Deserialize)]
struct Message {
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    content: Content,
}

impl Message {
    fn blocks(&self) -> impl Iterator<Item = &Block> {
        match &self.content {
            Content::Blocks(blocks) => blocks.as_slice(),
            _ => &[],
        }
        .iter()
    }

    fn texts(&self) -> impl Iterator<Item = &str> {
        let plain = match &self.content {
            Content::Text(text) => Some(text.as_str()),
            _ => None,
        };
        plain.into_iter().chain(self.blocks().filter_map(|block| block.text.as_deref()))
    }
}

/// A message's content: a prompt's plain string, or a list of blocks. Any
/// other shape is kept as opaque, so a change to it costs the entry its
/// content rather than its turn.
#[derive(Deserialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Blocks(Vec<Block>),
    Other(serde::de::IgnoredAny),
}

impl Default for Content {
    fn default() -> Self {
        Self::Blocks(Vec::new())
    }
}

#[derive(Deserialize)]
struct Block {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default, deserialize_with = "text_or_none")]
    text: Option<String>,
}

/// A block's `text`, or `None` for one that carries something other than a
/// string under that name.
fn text_or_none<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Option<String>, D::Error> {
    Ok(match serde_json::Value::deserialize(de)? {
        serde_json::Value::String(text) => Some(text),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "0b4c3662-b176-4e0f-8efa-42b2e70d80de";

    fn assistant(stop: Option<&str>, blocks: &str) -> String {
        let stop = stop.map_or("null".to_owned(), |s| format!("\"{s}\""));
        format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","stop_reason":{stop},"content":[{blocks}]}}}}"#
        )
    }

    fn tool_result(ends_turn: bool) -> String {
        format!(
            r#"{{"type":"user","toolEndsTurn":{ends_turn},"message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t","content":[{{"type":"text","text":"ok"}}]}}]}}}}"#
        )
    }

    const TEXT: &str = r#"{"type":"text","text":"done"}"#;
    const TOOL_USE: &str = r#"{"type":"tool_use","id":"t","name":"Bash","input":{}}"#;
    const ATTACHMENT: &str =
        r#"{"type":"attachment","attachment":{"type":"total_tokens_reminder","text":"x"}}"#;
    const INTERRUPT: &str = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#;
    const PROMPT: &str =
        r#"{"type":"user","message":{"role":"user","content":"Map the service install"}}"#;

    fn running(lines: &[&str]) -> bool {
        let mut progress = Progress::default();
        for line in lines {
            progress.fold(line.as_bytes());
        }
        progress.running()
    }

    fn running_owned(lines: &[String]) -> bool {
        running(&lines.iter().map(String::as_str).collect::<Vec<_>>())
    }

    fn launched_in_background(id: &str) -> String {
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t","content":"Command running in background with ID: {id}."}}]}},"toolUseResult":{{"stdout":"","stderr":"","interrupted":false,"backgroundTaskId":"{id}"}}}}"#
        )
    }

    fn agent_launched(id: &str) -> String {
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t","content":"Async agent launched"}}]}},"toolUseResult":{{"isAsync":true,"status":"async_launched","agentId":"{id}"}}}}"#
        )
    }

    fn notified(id: &str) -> String {
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":"[SYSTEM NOTIFICATION - NOT USER INPUT]\n\n<task-notification>\n<task-id>{id}</task-id>\n<status>completed</status>\n</task-notification>"}}}}"#
        )
    }

    /// A tool error, whose `toolUseResult` is a bare string.
    const BLOCKED: &str = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"Blocked","is_error":true}]},"toolUseResult":"Error: Blocked: standalone sleep 60."}"#;

    #[test]
    fn a_turn_ends_on_a_reply_that_calls_no_tool() {
        assert!(!running(&[PROMPT, &assistant(Some("end_turn"), TEXT)]));
        assert!(running(&[PROMPT, &assistant(Some("tool_use"), TOOL_USE)]));
        assert!(running(&[PROMPT, &assistant(None, TEXT)]), "a reply still streaming");
    }

    #[test]
    fn a_tool_result_keeps_the_turn_open_unless_it_ends_it() {
        assert!(running(&[&assistant(Some("tool_use"), TOOL_USE), &tool_result(false)]));
        assert!(!running(&[&assistant(Some("tool_use"), TOOL_USE), &tool_result(true)]));
    }

    #[test]
    fn an_interrupt_ends_the_turn_and_a_new_prompt_reopens_it() {
        assert!(!running(&[PROMPT, &assistant(None, TEXT), INTERRUPT]));
        assert!(running(&[PROMPT, &assistant(Some("end_turn"), TEXT), PROMPT]));
    }

    #[test]
    fn entries_that_are_not_turns_are_passed_over() {
        assert!(running(&[
            &assistant(Some("tool_use"), TOOL_USE),
            &tool_result(false),
            ATTACHMENT
        ]));
        assert!(!running(&[PROMPT, &assistant(Some("end_turn"), TEXT), ATTACHMENT]));
        assert!(
            !running(&[PROMPT, &assistant(Some("end_turn"), TEXT), r#"{"type":"assist"#]),
            "a line still being written"
        );
    }

    /// The run that showed a turn's end is not the agent's: it started a
    /// background `sleep`, stopped with a plain reply to wait on it, was woken
    /// by its report, and only then handed back.
    #[test]
    fn an_agent_waiting_on_its_own_background_work_is_still_running() {
        let bash = assistant(Some("tool_use"), TOOL_USE);
        let mut lines = vec![
            PROMPT.to_owned(),
            bash.clone(),
            BLOCKED.to_owned(),
            bash,
            launched_in_background("b3hklaplj"),
            assistant(Some("end_turn"), TEXT),
        ];
        assert!(running_owned(&lines), "parked on its background sleep");
        lines.push(notified("b3hklaplj"));
        assert!(running_owned(&lines), "woken by the sleep's report");
        lines.extend([assistant(Some("tool_use"), TOOL_USE), tool_result(true)]);
        assert!(!running_owned(&lines), "handed back");
    }

    #[test]
    fn a_plain_reply_with_every_report_in_has_finished() {
        assert!(!running_owned(&[
            PROMPT.to_owned(),
            agent_launched("a9416fcb657e364d1"),
            notified("a9416fcb657e364d1"),
            assistant(Some("end_turn"), TEXT),
        ]));
        assert!(running_owned(&[
            PROMPT.to_owned(),
            agent_launched("a9416fcb657e364d1"),
            assistant(Some("end_turn"), TEXT),
        ]));
    }

    #[test]
    fn handing_back_or_an_interrupt_ends_the_run_whatever_is_still_out() {
        let out = launched_in_background("b1");
        assert!(!running(&[PROMPT, &out, &tool_result(true)]));
        assert!(!running(&[PROMPT, &out, INTERRUPT]));
    }

    #[test]
    fn a_notification_names_each_task_it_reports() {
        let text = "<task-notification>\n<task-id>a</task-id>\n</task-notification>\n\
                    <task-notification>\n<task-id> b </task-id>";
        assert_eq!(notified_tasks(text).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(notified_tasks("<task-id>x</task-id>, outside any notification").count(), 0);
    }

    #[test]
    fn the_slug_replaces_everything_but_letters_and_digits() {
        assert_eq!(
            slug(Path::new("/home/me/.alacritree/worktrees/app-1e59/fix_it")),
            "-home-me--alacritree-worktrees-app-1e59-fix-it"
        );
    }

    /// Places every pid in `PIDS` in the host at its index, and nothing else.
    struct Pids(&'static [u32]);

    impl Lineage for Pids {
        fn host_of(&self, pid: u32) -> Option<usize> {
            self.0.iter().position(|&known| known == pid)
        }
    }

    fn host() -> Host {
        Host { cwd: Some(PathBuf::from("/work/app")), agent: Some("claude") }
    }

    struct Home(tempfile::TempDir);

    impl Home {
        fn new() -> Self {
            Self(tempfile::tempdir().unwrap())
        }

        fn path(&self) -> &Path {
            self.0.path()
        }

        fn write(&self, relative: &str, text: &str) {
            let path = self.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }

        fn append(&self, relative: &str, line: &str) {
            use std::io::Write;
            let mut file =
                fs::OpenOptions::new().append(true).open(self.path().join(relative)).unwrap();
            writeln!(file, "{line}").unwrap();
        }

        /// Claude Code process 42, holding a conversation started in
        /// `/work/app` and filed under that directory's slug.  Answers the
        /// conversation's folder.
        fn conversation(&self) -> String {
            self.write(
                "sessions/42.json",
                &format!(
                    r#"{{"pid":42,"sessionId":"{SESSION}","cwd":"/work/app","kind":"interactive"}}"#
                ),
            );
            let folder = "projects/-work-app";
            self.write(&format!("{folder}/{SESSION}.jsonl"), &format!("{PROMPT}\n"));
            format!("{folder}/{SESSION}")
        }

        fn scan(&self, source: &mut ClaudeSubagents, pids: &'static [u32]) -> Vec<Vec<Subagent>> {
            let hosts = vec![host(); pids.len().max(1)];
            let scan = source.scan(&hosts, &Pids(pids));
            assert!(scan.errors.is_empty(), "{:?}", scan.errors);
            scan.running
        }

        fn source(&self) -> ClaudeSubagents {
            ClaudeSubagents::at(Some(self.path().to_path_buf()))
        }
    }

    fn names(running: &[Vec<Subagent>]) -> Vec<Vec<&str>> {
        running.iter().map(|agents| agents.iter().map(|a| a.name.as_str()).collect()).collect()
    }

    #[test]
    fn the_registry_skips_what_is_not_a_process() {
        let home = Home::new();
        home.conversation();
        home.write("sessions/42.abc.key", "secret");
        home.write("sessions/7.json", "{ not json");
        assert_eq!(
            registrations(home.path()).unwrap(),
            vec![Registration {
                pid: 42,
                session_id: SESSION.into(),
                cwd: PathBuf::from("/work/app"),
            }]
        );
        assert!(registrations(&home.path().join("missing")).unwrap().is_empty());
    }

    #[test]
    fn only_running_agents_are_listed_with_what_their_meta_says() {
        let home = Home::new();
        let dir = home.conversation();
        home.write(
            &format!("{dir}/subagents/agent-a1.meta.json"),
            r#"{"agentType":"Explore","description":"Map the service install","spawnDepth":1}"#,
        );
        home.write(
            &format!("{dir}/subagents/agent-a1.jsonl"),
            &format!("{PROMPT}\n{}\n", tool_result(false)),
        );
        home.write(&format!("{dir}/subagents/agent-a2.meta.json"), r#"{"description":"Finished"}"#);
        home.write(
            &format!("{dir}/subagents/agent-a2.jsonl"),
            &format!("{PROMPT}\n{}\n", assistant(Some("end_turn"), TEXT)),
        );
        let running = home.scan(&mut home.source(), &[42]);
        assert_eq!(
            running,
            vec![vec![Subagent {
                id: "a1".into(),
                name: "Map the service install".into(),
                detail: Some("Explore".into()),
            }]]
        );
    }

    #[test]
    fn a_process_no_session_started_lists_nothing() {
        let home = Home::new();
        let dir = home.conversation();
        home.write(&format!("{dir}/subagents/agent-a1.jsonl"), &format!("{PROMPT}\n"));
        assert_eq!(names(&home.scan(&mut home.source(), &[7])), [Vec::<&str>::new()]);
    }

    #[test]
    fn a_workflow_agent_carries_its_run_name_and_phase() {
        let home = Home::new();
        let dir = home.conversation();
        let run = "wf_77077ed6-501";
        home.write(&format!("{dir}/workflows/scripts/aut-968-labos-run-id-{run}.js"), "");
        home.write(
            &format!("{dir}/subagents/workflows/{run}/agent-b1.meta.json"),
            r#"{"agentType":"workflow-subagent","description":"implement:labos","workflowPhase":"Implement"}"#,
        );
        home.write(
            &format!("{dir}/subagents/workflows/{run}/agent-b1.jsonl"),
            &format!("{PROMPT}\n"),
        );
        home.write(
            &format!("{dir}/subagents/workflows/{run}/journal.jsonl"),
            r#"{"type":"launched"}"#,
        );
        let running = home.scan(&mut home.source(), &[42]);
        assert_eq!(running[0][0].name, "implement:labos");
        assert_eq!(running[0][0].detail.as_deref(), Some("aut-968-labos-run-id, Implement"));
    }

    #[test]
    fn an_agent_drops_off_once_its_transcript_ends_the_turn() {
        let home = Home::new();
        let dir = home.conversation();
        let transcript = format!("{dir}/subagents/agent-a1.jsonl");
        home.write(&transcript, &format!("{PROMPT}\n"));
        let mut source = home.source();
        assert_eq!(names(&home.scan(&mut source, &[42])), [["a1"]]);
        home.append(&transcript, &assistant(Some("end_turn"), TEXT));
        assert_eq!(names(&home.scan(&mut source, &[42])), [Vec::<&str>::new()]);
    }

    #[test]
    fn a_folder_the_slug_misses_is_found_by_its_transcript() {
        let home = Home::new();
        home.write(
            "sessions/42.json",
            &format!(r#"{{"pid":42,"sessionId":"{SESSION}","cwd":"/a/very/long/path"}}"#),
        );
        home.write(&format!("projects/-shortened-12ab/{SESSION}.jsonl"), "");
        home.write(
            &format!("projects/-shortened-12ab/{SESSION}/subagents/agent-a1.jsonl"),
            &format!("{PROMPT}\n"),
        );
        assert_eq!(names(&home.scan(&mut home.source(), &[42])), [["a1"]]);
    }

    #[test]
    fn a_session_with_no_transcript_yet_has_no_agents() {
        let home = Home::new();
        home.write(
            "sessions/42.json",
            &format!(r#"{{"pid":42,"sessionId":"{SESSION}","cwd":"/work/app"}}"#),
        );
        assert_eq!(names(&home.scan(&mut home.source(), &[42])), [Vec::<&str>::new()]);
    }
}
