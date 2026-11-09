//! One `hc connect` child in framed mode, carrying every stream of a side.
//!
//! A writer thread owns the child's stdin, so a send never blocks the caller.
//! A demux thread owns its stdout: it answers calls, keeps every terminal's
//! facts from the event stream, and routes each stream channel's bytes into
//! that session's byte channel. It never takes a `Term` lock, so one busy
//! session cannot hold up another one's output.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::CoreError;
use crate::parser::Chunk;
use crate::wire::{self, Frame};

/// The protocol generation this client speaks.
pub(crate) const GENERATION: u32 = 1;

/// How long `hc connect` may take to answer `hello`. It starts the core
/// when none is running.
const HELLO_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a call may take. A kill escalates from hangup to kill, so it is
/// the slowest call this client makes.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// What the core says about a terminal, kept current from events.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Facts {
    pub(crate) foreground: Option<Foreground>,
    pub(crate) state: State,
    pub(crate) pid: Option<u32>,
    pub(crate) labels: HashMap<String, String>,
    /// The name a client gave the terminal.
    pub(crate) name: Option<String>,
    /// The title the program set.
    pub(crate) title: Option<String>,
    /// The directory the terminal started in.
    pub(crate) cwd: Option<PathBuf>,
}

/// Where a terminal's program stands. Only the core knows whether it keeps a
/// terminal, so its events are what tell a loss from a final end.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum State {
    Live,
    /// The core lost the program of a kept terminal and starts a shell in
    /// its place.
    Lost,
    #[default]
    Finished,
}

impl State {
    /// A record's state. A live record with no program is between a loss
    /// and its new shell.
    pub(crate) fn of_record(state: &str, pid: Option<u32>) -> Self {
        match (state, pid) {
            ("live", Some(_)) => Self::Live,
            ("live", None) => Self::Lost,
            _ => Self::Finished,
        }
    }
}

/// Called when the core reports a terminal that is new, back after a loss,
/// labelled anew, finished or removed, which is when a terminal may become
/// adoptable or change what the pane listing shows.
pub(crate) type Waker = Arc<OnceLock<Box<dyn Fn() + Send + Sync>>>;

/// What runs in a terminal now: the foreground process group's leader, or
/// on Windows the program the terminal started.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct Foreground {
    pub(crate) pid: i32,
    pub(crate) name: String,
    pub(crate) argv: Vec<String>,
    pub(crate) cwd: PathBuf,
}

/// The parts of a terminal record this client reads.
#[derive(Debug, Deserialize)]
struct Record {
    address: String,
    foreground: Option<Foreground>,
    state: String,
    pid: Option<u32>,
    #[serde(default)]
    labels: HashMap<String, String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    cwd: Option<PathBuf>,
}

impl Record {
    fn facts(self) -> (String, Facts) {
        let facts = Facts {
            foreground: self.foreground,
            state: State::of_record(&self.state, self.pid),
            pid: self.pid,
            labels: self.labels,
            name: self.name,
            title: self.title,
            cwd: self.cwd,
        };
        (self.address, facts)
    }
}

/// What a call's reply opens, which the demux thread must route before it
/// reads the next frame: the core sends a stream's first bytes right after
/// the reply that names its channel.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Opens {
    Nothing,
    Stream,
    Events,
}

/// A call's result, and the stream its reply opened.
pub(crate) struct Answer {
    pub(crate) result: Value,
    pub(crate) stream: Option<(u32, Receiver<Chunk>)>,
}

struct Pending {
    opens: Opens,
    reply: Sender<Result<Answer, CoreError>>,
}

#[derive(Default)]
struct Routes {
    calls: HashMap<u64, Pending>,
    streams: HashMap<u32, Sender<Chunk>>,
    events: Option<u32>,
    /// Event frames that arrived before the reply naming their channel. The
    /// core starts the event stream without waiting for that reply, while
    /// it holds an attach stream back until its reply is out.
    early_events: Vec<Frame>,
    /// Bytes of an event line that a frame cut off.
    partial_event: Vec<u8>,
    terminals: HashMap<String, Facts>,
    /// The facts last showed a running terminal.
    running: bool,
}

impl Routes {
    /// A terminal that has not finished. A lost program counts, since the
    /// core starts its new shell.
    fn has_running_terminal(&self) -> bool {
        self.terminals.values().any(|facts| facts.state != State::Finished)
    }
}

/// Called on the demux thread, with the facts locked, when the core goes
/// from no running terminal to one. The first report counts.
pub(crate) type OnRunning = Option<Box<dyn Fn() + Send + Sync>>;

struct Shared {
    alive: AtomicBool,
    routes: Mutex<Routes>,
    /// Signalled when the terminals' facts change or the connection closes.
    changed: Condvar,
    waker: Waker,
    on_running: OnRunning,
}

impl Shared {
    fn routes(&self) -> MutexGuard<'_, Routes> {
        self.routes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Call [`Self::on_running`] when the facts that just changed show a
    /// running terminal and the previous ones showed none.
    fn note_running(&self, routes: &mut Routes) {
        let running = routes.has_running_terminal();
        if running
            && !routes.running
            && let Some(on_running) = &self.on_running
        {
            on_running();
        }
        routes.running = running;
    }

    /// End the connection when either pipe breaks. Dropping the senders
    /// ends every parser's stream and fails every call still waiting, so
    /// nothing the child still writes reaches a session.
    fn close(&self) {
        self.alive.store(false, Ordering::Relaxed);
        let mut routes = self.routes();
        routes.streams.clear();
        for (_, pending) in routes.calls.drain() {
            let _ = pending.reply.send(Err(CoreError::Closed));
        }
        self.changed.notify_all();
    }
}

pub(crate) struct Connection {
    frames: Sender<Vec<u8>>,
    shared: Arc<Shared>,
    next_id: AtomicU64,
    /// The core accepts AUTOMATIC_INPUT on its local streams.
    automatic_input: bool,
    child: Arc<Mutex<Child>>,
}

impl Connection {
    /// Start `command`, which runs `hc connect`, switch it to framed mode and
    /// follow its events. `program` names it in a spawn error.
    pub(crate) fn open(
        mut command: Command,
        program: &str,
        waker: Waker,
        on_running: OnRunning,
    ) -> Result<Arc<Self>, CoreError> {
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        // Backend calls may block, as the trait says. The app connects on a
        // job at startup so the first session finds the connection open.
        #[allow(clippy::disallowed_methods)]
        let mut child = command
            .spawn()
            .map_err(|source| CoreError::Spawn { program: program.to_string(), source })?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let mut stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        let stderr = drain(child.stderr.take().expect("piped stderr"));
        let child = Arc::new(Mutex::new(child));

        let automatic_input = match hello(&mut stdin, &mut stdout, &child) {
            Ok(automatic_input) => automatic_input,
            Err(CoreError::Closed) => {
                let text = stderr.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
                let mut child = lock(&child);
                let code = exit_code(&mut child);
                let _ = child.kill();
                return Err(match code {
                    Some(alacritree_common::side::NOT_FOUND) => CoreError::Spawn {
                        program: program.to_string(),
                        source: std::io::Error::new(std::io::ErrorKind::NotFound, text),
                    },
                    _ => CoreError::Refused(text),
                });
            },
            Err(error) => {
                let _ = lock(&child).kill();
                return Err(error);
            },
        };

        let shared = Arc::new(Shared {
            alive: AtomicBool::new(true),
            routes: Default::default(),
            changed: Condvar::new(),
            waker,
            on_running,
        });
        let (frames, outgoing) = mpsc::channel();
        spawn_writer(stdin, outgoing, shared.clone())?;
        spawn_demux(stdout, frames.clone(), shared.clone())?;

        let connection =
            Arc::new(Self { frames, shared, next_id: AtomicU64::new(1), automatic_input, child });
        connection.call_opening("events.subscribe", json!({}), Opens::Events)?;
        Ok(connection)
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn kill(&self) {
        let _ = lock(&self.child).kill();
    }

    pub(crate) fn alive(&self) -> bool {
        self.shared.alive.load(Ordering::Relaxed)
    }

    pub(crate) fn automatic_input(&self) -> bool {
        self.automatic_input
    }

    pub(crate) fn send(&self, channel: u32, kind: u8, payload: &[u8]) {
        let _ = self.frames.send(wire::encode(channel, kind, payload));
    }

    /// Tell the core that `bytes` of `channel`'s output were consumed.
    pub(crate) fn credit(&self, channel: u32, bytes: usize) {
        let _ = self.frames.send(credit(channel, bytes));
    }

    pub(crate) fn call(&self, method: &str, params: Value) -> Result<Value, CoreError> {
        self.call_opening(method, params, Opens::Nothing).map(|answer| answer.result)
    }

    /// Make a call without waiting for its answer, for the UI thread.
    pub(crate) fn cast(&self, method: &str, params: Value) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply, _ignored) = mpsc::channel();
        self.shared.routes().calls.insert(id, Pending { opens: Opens::Nothing, reply });
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.send(0, wire::DATA, request.to_string().as_bytes());
    }

    /// Open a stream with `method`, whose reply names its channel.
    pub(crate) fn call_streaming(&self, method: &str, params: Value) -> Result<Answer, CoreError> {
        self.call_opening(method, params, Opens::Stream)
    }

    fn call_opening(&self, method: &str, params: Value, opens: Opens) -> Result<Answer, CoreError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply, answer) = mpsc::channel();
        self.shared.routes().calls.insert(id, Pending { opens, reply });
        if !self.alive() {
            self.shared.routes().calls.remove(&id);
            return Err(CoreError::Closed);
        }
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.send(0, wire::DATA, request.to_string().as_bytes());
        match answer.recv_timeout(CALL_TIMEOUT) {
            Ok(answer) => answer,
            Err(RecvTimeoutError::Timeout) => {
                // A late reply that opens a stream must still find its call,
                // so the demux thread can close the stream nobody reads.
                if opens != Opens::Stream {
                    self.shared.routes().calls.remove(&id);
                }
                Err(CoreError::Timeout(method.to_string()))
            },
            Err(RecvTimeoutError::Disconnected) => Err(CoreError::Closed),
        }
    }

    /// What the core last said about `address`.
    pub(crate) fn facts(&self, address: &str) -> Option<Facts> {
        self.shared.routes().terminals.get(address).cloned()
    }

    /// Block until `check` finds an answer in the terminals' facts, which it
    /// reads again on every change. `None` when the connection closes first.
    pub(crate) fn wait<T>(
        &self,
        mut check: impl FnMut(&HashMap<String, Facts>) -> Option<T>,
    ) -> Option<T> {
        let mut routes = self.shared.routes();
        while self.alive() {
            if let Some(answer) = check(&routes.terminals) {
                return Some(answer);
            }
            routes = self.shared.changed.wait(routes).unwrap_or_else(PoisonError::into_inner);
        }
        None
    }

    /// Make every [`Self::wait`] check again, for a change its check reads
    /// outside the facts.
    pub(crate) fn wake(&self) {
        let _routes = self.shared.routes();
        self.shared.changed.notify_all();
    }

    /// Answers whether the core last reported a terminal that has not
    /// finished.
    pub(crate) fn has_running_terminal(&self) -> bool {
        self.shared.routes().has_running_terminal()
    }

    /// What `pick` takes from each terminal the core last reported.
    pub(crate) fn pick_terminals<T>(
        &self,
        mut pick: impl FnMut(&str, &Facts) -> Option<T>,
    ) -> Vec<T> {
        let routes = self.shared.routes();
        routes.terminals.iter().filter_map(|(address, facts)| pick(address, facts)).collect()
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = lock(&self.child).kill();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Send `hello` on the plain line protocol and read its one-line reply,
/// answering whether the core takes AUTOMATIC_INPUT. A watchdog kills the
/// child when it does not answer in time, which ends the read.
fn hello(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<ChildStdout>,
    child: &Arc<Mutex<Child>>,
) -> Result<bool, CoreError> {
    let (answered, watchdog) = mpsc::channel::<()>();
    let watched = child.clone();
    std::thread::Builder::new()
        .name("hc-hello-watchdog".into())
        .spawn(move || {
            if let Err(RecvTimeoutError::Timeout) = watchdog.recv_timeout(HELLO_TIMEOUT) {
                let _ = lock(&watched).kill();
            }
        })
        .map_err(CoreError::Io)?;

    let params = json!({ "framed": true, "generation": GENERATION });
    let request = json!({ "jsonrpc": "2.0", "id": 0, "method": "hello", "params": params });
    let mut line = String::new();
    let read = writeln!(stdin, "{request}")
        .and_then(|()| stdin.flush())
        .and_then(|()| stdout.read_line(&mut line));
    drop(answered);
    if !matches!(read, Ok(1..)) {
        return Err(CoreError::Closed);
    }

    let reply: Value =
        serde_json::from_str(&line).map_err(|_| CoreError::Protocol(line.clone()))?;
    if let Some(error) = reply.get("error") {
        return Err(CoreError::rpc(error));
    }
    let result = &reply["result"];
    if result["framed"] != true {
        return Err(CoreError::Protocol(line));
    }
    Ok(result["features"]
        .as_array()
        .is_some_and(|features| features.iter().any(|feature| feature == "automatic_input")))
}

/// The status of a child whose pipes closed. On Windows the pipes close a
/// moment before the status is set, so it waits a little.
fn exit_code(child: &mut Child) -> Option<i32> {
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => return None,
        }
    }
}

/// Read the child's stderr to its end, so a full pipe never stalls it, and
/// hand back the text it wrote.
fn drain(mut stderr: impl Read + Send + 'static) -> Receiver<String> {
    let (text, received) = mpsc::channel();
    let _ = std::thread::Builder::new().name("hc-stderr".into()).spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        let _ = text.send(String::from_utf8_lossy(&bytes).trim().to_string());
    });
    received
}

fn spawn_writer(
    mut stdin: impl Write + Send + 'static,
    outgoing: Receiver<Vec<u8>>,
    shared: Arc<Shared>,
) -> Result<(), CoreError> {
    std::thread::Builder::new()
        .name("hc-writer".into())
        .spawn(move || {
            while let Ok(frame) = outgoing.recv() {
                if stdin.write_all(&frame).and_then(|()| stdin.flush()).is_err() {
                    return shared.close();
                }
            }
        })
        .map(drop)
        .map_err(CoreError::Io)
}

fn spawn_demux(
    mut stdout: BufReader<ChildStdout>,
    frames: Sender<Vec<u8>>,
    shared: Arc<Shared>,
) -> Result<(), CoreError> {
    std::thread::Builder::new()
        .name("hc-demux".into())
        .spawn(move || {
            while let Ok(Some(frame)) = wire::read_frame(&mut stdout) {
                route(&shared, &frames, frame);
            }
            shared.close();
        })
        .map(drop)
        .map_err(CoreError::Io)
}

fn route(shared: &Shared, frames: &Sender<Vec<u8>>, Frame { channel, kind, payload }: Frame) {
    let mut routes = shared.routes();
    if channel == 0 {
        if kind == wire::DATA {
            answer(&mut routes, frames, &shared.waker, &payload);
            shared.note_running(&mut routes);
        }
        return;
    }
    if routes.events == Some(channel) {
        if kind == wire::DATA {
            take_events(&mut routes, frames, &shared.waker, channel, &payload);
            shared.note_running(&mut routes);
            shared.changed.notify_all();
        }
        return;
    }
    let Some(stream) = routes.streams.get(&channel) else {
        if routes.events.is_none() && kind == wire::DATA {
            routes.early_events.push(Frame { channel, kind, payload });
        }
        return;
    };
    let chunk = match kind {
        wire::DATA => Chunk::Output(payload),
        wire::CLOSE => Chunk::Closed(serde_json::from_slice(&payload).ok()),
        _ => return,
    };
    let closed = matches!(chunk, Chunk::Closed(_));
    if stream.send(chunk).is_err() || closed {
        routes.streams.remove(&channel);
    }
}

fn credit(channel: u32, bytes: usize) -> Vec<u8> {
    wire::encode(channel, wire::CREDIT, &(bytes as u32).to_le_bytes())
}

fn take_events(
    routes: &mut Routes,
    frames: &Sender<Vec<u8>>,
    waker: &Waker,
    channel: u32,
    payload: &[u8],
) {
    let _ = frames.send(credit(channel, payload.len()));
    routes.partial_event.extend_from_slice(payload);
    if apply_events(routes)
        && let Some(wake) = waker.get()
    {
        wake();
    }
}

/// Hand a reply to its call, routing the channel it opens first.
fn answer(routes: &mut Routes, frames: &Sender<Vec<u8>>, waker: &Waker, payload: &[u8]) {
    let Ok(reply) = serde_json::from_slice::<Value>(payload) else { return };
    let Some(pending) = reply["id"].as_u64().and_then(|id| routes.calls.remove(&id)) else {
        return;
    };
    if let Some(error) = reply.get("error") {
        let _ = pending.reply.send(Err(CoreError::rpc(error)));
        return;
    }
    let result = reply["result"].clone();
    let channel = result["channel"].as_u64().map(|channel| channel as u32);
    let stream = match (pending.opens, channel) {
        (Opens::Stream, Some(channel)) => {
            let (sender, receiver) = mpsc::channel();
            routes.streams.insert(channel, sender);
            Some((channel, receiver))
        },
        (Opens::Events, Some(channel)) => {
            routes.events = Some(channel);
            routes.terminals = snapshot(&result["terminals"]);
            for early in std::mem::take(&mut routes.early_events) {
                if early.channel == channel {
                    take_events(routes, frames, waker, channel, &early.payload);
                }
            }
            None
        },
        _ => None,
    };
    let delivered = pending.reply.send(Ok(Answer { result, stream }));
    if let Err(mpsc::SendError(Ok(Answer { stream: Some((channel, _)), .. }))) = delivered {
        routes.streams.remove(&channel);
        let _ = frames.send(wire::encode(channel, wire::CLOSE, &[]));
    }
}

fn snapshot(terminals: &Value) -> HashMap<String, Facts> {
    let records: Vec<Record> = serde_json::from_value(terminals.clone()).unwrap_or_default();
    records.into_iter().map(Record::facts).filter(|(address, _)| on_this_machine(address)).collect()
}

/// Answers whether `address` is a terminal of this side's own core. The core
/// also reports the terminals of each machine that `hc machine add` connected,
/// with the machine's name in front, as in `box/t3`.
pub(crate) fn on_this_machine(address: &str) -> bool {
    !address.contains('/')
}

/// Apply every whole event line buffered so far. Answers whether one may
/// have made a terminal adoptable or changed the pane listing.
fn apply_events(routes: &mut Routes) -> bool {
    let mut arrived = false;
    while let Some(end) = routes.partial_event.iter().position(|&byte| byte == b'\n') {
        let line: Vec<u8> = routes.partial_event.drain(..=end).collect();
        if let Ok(message) = serde_json::from_slice::<Value>(&line) {
            arrived |= apply_event(&mut routes.terminals, &message);
        }
    }
    arrived
}

/// Fold one notification from the event stream into the terminals' facts.
/// Answers whether it may have made a terminal adoptable or changed the pane
/// listing. A foreground change does neither, and it is the most frequent.
fn apply_event(terminals: &mut HashMap<String, Facts>, message: &Value) -> bool {
    let params = &message["params"];
    if message["method"] == "missed" {
        *terminals = snapshot(&params["terminals"]);
        return true;
    }
    if message["method"] != "event" {
        return false;
    }
    let Some(address) = params["terminal"].as_str() else { return false };
    if !on_this_machine(address) {
        return false;
    }
    let data = &params["data"];
    match params["type"].as_str() {
        Some("created" | "restarted") => {
            let Ok(record) = serde_json::from_value::<Record>(data.clone()) else { return false };
            let (address, facts) = record.facts();
            terminals.insert(address, facts);
            true
        },
        Some("labels") => {
            let Some(facts) = terminals.get_mut(address) else { return false };
            facts.labels = serde_json::from_value(data["labels"].clone()).unwrap_or_default();
            true
        },
        Some("foreground") => {
            let facts = terminals.entry(address.to_string()).or_default();
            facts.foreground = serde_json::from_value(data.clone()).ok();
            false
        },
        Some(kind @ ("lost" | "finished")) => {
            let Some(facts) = terminals.get_mut(address) else { return false };
            facts.foreground = None;
            facts.state = if kind == "lost" { State::Lost } else { State::Finished };
            facts.pid = None;
            true
        },
        Some("removed") => terminals.remove(address).is_some(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    fn event(kind: &str, terminal: &str, data: Value) -> Value {
        json!({ "jsonrpc": "2.0", "method": "event",
                "params": { "seq": 1, "type": kind, "terminal": terminal, "data": data } })
    }

    fn foreground(pid: i32, name: &str) -> Value {
        json!({ "pid": pid, "name": name, "argv": [name], "cwd": "/src" })
    }

    /// A program the shell starts shows up in the facts, and its exit takes
    /// it back out, so the sidebar follows the core's foreground.
    #[test]
    fn events_keep_a_terminals_foreground_current() {
        let mut terminals = HashMap::new();
        let created = json!({ "address": "t1", "pid": 10, "foreground": foreground(10, "sh"),
                              "labels": {}, "argv": [], "cwd": "/", "state": "live",
                              "created_ms": 0, "screen": "exact", "cols": 80, "rows": 24 });
        apply_event(&mut terminals, &event("created", "t1", created));
        apply_event(&mut terminals, &event("foreground", "t1", foreground(11, "claude")));

        let foreground = terminals["t1"].foreground.as_ref();
        assert_eq!(foreground.map(|f| f.name.as_str()), Some("claude"));

        apply_event(&mut terminals, &event("finished", "t1", json!({})));
        assert_eq!(terminals["t1"].foreground, None);
        apply_event(&mut terminals, &event("removed", "t1", json!({})));
        assert!(terminals.is_empty());
    }

    /// A terminal on a machine that `hc machine add` connected never enters
    /// the facts, through the subscribe snapshot or through any event.
    #[test]
    fn terminals_on_other_machines_never_enter_the_facts() {
        let live = |address: &str| json!({ "address": address, "pid": 10, "foreground": null, "state": "live" });
        let mut routes = Routes::default();
        let (reply, _answer) = mpsc::channel();
        routes.calls.insert(1, Pending { opens: Opens::Events, reply });
        let (frames, _sent) = mpsc::channel();
        let subscribed = json!({ "jsonrpc": "2.0", "id": 1,
                                 "result": { "channel": 5, "terminals": [live("t1"), live("box/t1")] } });

        answer(&mut routes, &frames, &Waker::default(), subscribed.to_string().as_bytes());
        let mut terminals = routes.terminals;
        let created = event("created", "box/t2", live("box/t2"));
        let moved = event("foreground", "box/t3", foreground(11, "vim"));

        assert!(!apply_event(&mut terminals, &created));
        assert!(!apply_event(&mut terminals, &moved));
        assert_eq!(terminals.keys().collect::<Vec<_>>(), ["t1"]);
    }

    /// A command that exits with `code` before it answers anything.
    fn exiting(code: u8) -> Command {
        let (shell, flag) = if cfg!(windows) { ("cmd", "/c") } else { ("sh", "-c") };
        let mut command = alacritree_common::command_ext::hidden(shell);
        command.args([flag, &format!("exit {code}")]);
        command
    }

    /// A distro's `hc connect` goes through a shell, which exits 127 when it
    /// finds no `hc`. That is the same miss as a native spawn that finds none.
    #[test]
    fn a_shell_that_finds_no_hc_reads_as_a_spawn_that_found_nothing() {
        let error = Connection::open(exiting(127), "hc connect", Waker::default(), None).err();

        assert!(
            matches!(&error, Some(CoreError::Spawn { source, .. }) if source.kind() == std::io::ErrorKind::NotFound),
            "{error:?}"
        );
    }

    #[test]
    fn any_other_exit_before_hello_is_a_refusal() {
        let error = Connection::open(exiting(1), "hc connect", Waker::default(), None).err();

        assert!(matches!(&error, Some(CoreError::Refused(_))), "{error:?}");
    }

    /// A label change can make a terminal adoptable, so it replaces the
    /// label set and asks for a wake. Output facts such as the foreground
    /// do not.
    #[test]
    fn a_label_event_replaces_the_labels_and_wakes() {
        let mut terminals = HashMap::new();
        let created = json!({ "address": "t1", "pid": 10, "foreground": null, "state": "live",
                              "labels": { "where": "a / b" } });
        assert!(apply_event(&mut terminals, &event("created", "t1", created)));
        let labels = json!({ "labels": { "alacritree.workspace": "/src" } });

        assert!(apply_event(&mut terminals, &event("labels", "t1", labels)));
        assert!(!apply_event(&mut terminals, &event("foreground", "t1", foreground(11, "vim"))));

        let facts = &terminals["t1"];
        assert_eq!(facts.state, State::Live);
        assert_eq!(facts.labels, HashMap::from([("alacritree.workspace".into(), "/src".into())]));
    }

    /// The core sends `lost` only for a kept terminal it restarts, and
    /// `finished` for every final end, a dead holder of an unkept one too.
    #[test]
    fn a_lost_terminal_waits_for_its_shell_and_a_finished_one_does_not() {
        let mut terminals = HashMap::new();
        for address in ["t1", "t2"] {
            let created =
                json!({ "address": address, "pid": 10, "foreground": null, "state": "live" });
            apply_event(&mut terminals, &event("created", address, created));
        }

        apply_event(&mut terminals, &event("lost", "t1", json!({ "reason": "holder_lost" })));
        apply_event(&mut terminals, &event("finished", "t2", json!({ "reason": "holder_lost" })));

        assert_eq!((terminals["t1"].state, terminals["t1"].pid), (State::Lost, None));
        assert_eq!((terminals["t2"].state, terminals["t2"].pid), (State::Finished, None));
    }

    /// A broken pipe to `hc connect` ends the connection even while its
    /// output still arrives, so what it says afterwards reaches no session,
    /// whose parser has moved on to a new connection.
    #[test]
    fn once_the_writer_breaks_late_output_reaches_no_session() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let shared = Arc::new(Shared {
            alive: AtomicBool::new(true),
            routes: Default::default(),
            changed: Condvar::new(),
            waker: Default::default(),
            on_running: None,
        });
        let (session, stream) = mpsc::channel();
        shared.routes().streams.insert(3, session);
        let (frames, outgoing) = mpsc::channel();
        spawn_writer(Broken, outgoing, shared.clone()).expect("the writer thread");

        frames.send(wire::encode(3, wire::DATA, b"typed")).expect("the writer reads frames");
        let deadline = Instant::now() + Duration::from_secs(5);
        while shared.alive.load(Ordering::Relaxed) {
            assert!(Instant::now() < deadline, "the broken pipe ends the connection");
            std::thread::sleep(Duration::from_millis(5));
        }
        route(&shared, &frames, Frame { channel: 3, kind: wire::DATA, payload: b"late".to_vec() });

        assert!(matches!(stream.try_recv(), Err(mpsc::TryRecvError::Disconnected)));
    }

    /// A stream whose attach gave up waiting is closed when its reply comes
    /// in after all, so the core does not hold it open with nobody reading.
    #[test]
    fn a_stream_opened_after_its_call_gave_up_is_closed() {
        let mut routes = Routes::default();
        let (reply, gave_up) = mpsc::channel();
        drop(gave_up);
        routes.calls.insert(7, Pending { opens: Opens::Stream, reply });
        let (frames, sent) = mpsc::channel();
        let late = json!({ "jsonrpc": "2.0", "id": 7, "result": { "channel": 9 } });

        answer(&mut routes, &frames, &Waker::default(), late.to_string().as_bytes());

        assert_eq!(sent.try_recv().ok(), Some(wire::encode(9, wire::CLOSE, &[])));
        assert!(!routes.streams.contains_key(&9));
    }

    /// The hook runs when the subscribe snapshot shows a running terminal,
    /// not for a second one, and again once a terminal starts after all of
    /// them finished.
    #[test]
    fn on_running_follows_the_core_from_no_running_terminal_to_one() {
        let starts = Arc::new(AtomicU64::new(0));
        let counted = starts.clone();
        let shared = Shared {
            alive: AtomicBool::new(true),
            routes: Default::default(),
            changed: Condvar::new(),
            waker: Default::default(),
            on_running: Some(Box::new(move || {
                counted.fetch_add(1, Ordering::Relaxed);
            })),
        };
        let (reply, _answer) = mpsc::channel();
        shared.routes().calls.insert(1, Pending { opens: Opens::Events, reply });
        let (frames, _sent) = mpsc::channel();
        let live = |address: &str| json!({ "address": address, "pid": 10, "foreground": null, "state": "live" });
        let events = |messages: &[Value]| Frame {
            channel: 5,
            kind: wire::DATA,
            payload: messages
                .iter()
                .map(|message| format!("{message}\n"))
                .collect::<String>()
                .into(),
        };
        let subscribed = json!({ "jsonrpc": "2.0", "id": 1,
                                 "result": { "channel": 5, "terminals": [live("t1")] } });

        let payload = subscribed.to_string().into_bytes();
        route(&shared, &frames, Frame { channel: 0, kind: wire::DATA, payload });
        route(&shared, &frames, events(&[event("created", "t2", live("t2"))]));
        assert_eq!(starts.load(Ordering::Relaxed), 1);

        let finished = json!({});
        let all_finish =
            [event("finished", "t1", finished.clone()), event("finished", "t2", finished)];
        route(&shared, &frames, events(&all_finish));
        route(&shared, &frames, events(&[event("created", "t3", live("t3"))]));
        assert_eq!(starts.load(Ordering::Relaxed), 2);
    }

    /// The core may cut an event line across frames.
    #[test]
    fn an_event_split_across_frames_applies_once_whole() {
        let mut routes = Routes::default();
        let line = format!("{}\n", event("foreground", "t2", foreground(5, "vim")));
        let (head, tail) = line.as_bytes().split_at(line.len() / 2);

        routes.partial_event.extend_from_slice(head);
        apply_events(&mut routes);
        assert!(routes.terminals.is_empty());

        routes.partial_event.extend_from_slice(tail);
        apply_events(&mut routes);
        assert_eq!(routes.terminals["t2"].foreground.as_ref().map(|f| f.pid), Some(5));
    }
}
