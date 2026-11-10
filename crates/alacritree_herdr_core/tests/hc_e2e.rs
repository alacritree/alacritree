//! The herdr-core backend against a throwaway core.
//!
//! Each test starts its own core, isolated by its own `HC_SOCKET`,
//! `HC_SESSION` and state and config homes in a temporary directory, so no
//! test touches the user's core. Opt-in: they need `hc` on PATH.

// A test has no UI thread for a blocking wait to stall.
#![allow(clippy::disallowed_methods)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use alacritree_herdr_core::{
    CoreElsewhere, HerdrCore, HerdrCoreConfig, HerdrCoreHandle, HerdrCorePanes, HerdrCores, Remedy,
};
use alacritree_multiplexer::{MultiplexerSession, Side};
use alacritree_session_backend::{
    Adoptable, AttachRequest, OpenRequest, Placement, ProcessTree, Screen, SessionBackend,
    SessionHandle, ShellCommand,
};
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Cell;
use alacritty_terminal::term::{Config, Term};
use serde_json::Value;

const PATIENCE: Duration = Duration::from_secs(20);

/// A core of this test's own, stopped when the test ends.
struct Core {
    home: tempfile::TempDir,
    /// Where it listens, which a test may hide from hc.
    socket: String,
}

impl Core {
    fn start() -> Self {
        Self::start_in(&format!("alacritree-e2e-{}", std::process::id()))
    }

    /// A core of the hc session `session`. No other core listens where hc
    /// puts that session's default socket: on Unix the runtime dir is this
    /// test's own, and on Windows the pipe names carry a user of its own.
    fn start_in(session: &str) -> Self {
        let home = tempfile::tempdir().expect("a temp dir");
        let socket = if cfg!(windows) {
            format!(r"\\.\pipe\alacritree-hc-e2e-{}", std::process::id())
        } else {
            home.path().join("hc.sock").display().to_string()
        };
        let (runtime, isolated) = if cfg!(windows) {
            ("USERNAME", format!("alacritreee2e{}", std::process::id()))
        } else {
            ("XDG_RUNTIME_DIR", home.path().join("run").display().to_string())
        };
        let vars = [
            ("HC_SOCKET", socket.clone()),
            ("HC_SESSION", session.to_string()),
            ("XDG_STATE_HOME", home.path().join("state").display().to_string()),
            ("XDG_CONFIG_HOME", home.path().join("config").display().to_string()),
            (runtime, isolated),
            ("TERM", "xterm-256color".to_string()),
        ];
        for (name, value) in vars {
            // SAFETY: the `e2e` task runs these under nextest, which gives each
            // test a process of its own, and this runs before any thread.
            unsafe { std::env::set_var(name, value) };
        }
        // SAFETY: as above.
        unsafe { std::env::remove_var("HC_TERMINAL_ID") };
        let core = Self { home, socket };
        let started = hc(&["server", "start"]);
        assert!(started.status.success(), "the throwaway core starts: {started:?}");
        core
    }

    fn records(&self) -> Vec<Value> {
        let listed = hc(&["ls", "--all", "--json"]);
        let listed: Value = serde_json::from_slice(&listed.stdout).expect("hc ls answers JSON");
        listed["terminals"].as_array().cloned().unwrap_or_default()
    }

    fn record(&self, address: &str) -> Option<Value> {
        self.records().into_iter().find(|record| record["address"] == address)
    }

    /// The whole record of a live terminal.
    fn get(&self, address: &str) -> Value {
        serde_json::from_slice(&hc(&["get", address, "--json"]).stdout).unwrap_or_default()
    }

    fn dir(&self) -> &Path {
        self.home.path()
    }

    /// The process that serves, which a restart replaces.
    fn pid(&self) -> u64 {
        let status: Value = serde_json::from_slice(&hc(&["server", "status", "--json"]).stdout)
            .expect("server status answers JSON");
        status["pid"].as_u64().expect("a serving core names its pid")
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        let _ = Command::new("hc").args(["server", "stop"]).env("HC_SOCKET", &self.socket).output();
    }
}

fn hc(args: &[&str]) -> std::process::Output {
    Command::new("hc").args(args).output().expect("hc is on PATH; these tests are opt-in")
}

/// Every child exit the session's terminal reports.
#[derive(Clone)]
struct Exits(Sender<ExitStatus>);

impl EventListener for Exits {
    fn send_event(&self, event: Event) {
        if let Event::ChildExit(status) = event {
            let _ = self.0.send(status);
        }
    }
}

struct Size(usize, usize);

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.1
    }

    fn screen_lines(&self) -> usize {
        self.1
    }

    fn columns(&self) -> usize {
        self.0
    }
}

struct Opened {
    handle: HerdrCoreHandle,
    term: Arc<FairMutex<Term<Exits>>>,
    exits: Receiver<ExitStatus>,
}

fn window(cols: u16, lines: u16) -> WindowSize {
    WindowSize { num_lines: lines, num_cols: cols, cell_width: 8, cell_height: 16 }
}

/// A shell that prints `>` as its prompt, so a test can tell typed text from
/// its echo.
fn shell() -> ShellCommand {
    if cfg!(windows) {
        ShellCommand::new("cmd.exe".into(), vec!["/q".into(), "/k".into(), "prompt $g".into()])
    } else {
        ShellCommand::new("sh".into(), vec![])
    }
}

fn open(backend: &HerdrCore, shell: ShellCommand, dir: &Path) -> Opened {
    open_at(backend, shell, dir, Placement::default())
}

/// A grid for a session to paint into, and the exits it reports.
fn screen_for() -> (Screen<Exits>, Arc<FairMutex<Term<Exits>>>, Receiver<ExitStatus>) {
    screen_sized(window(80, 24))
}

fn screen_sized(
    window_size: WindowSize,
) -> (Screen<Exits>, Arc<FairMutex<Term<Exits>>>, Receiver<ExitStatus>) {
    let (sender, exits) = mpsc::channel();
    let listener = Exits(sender);
    let size = Size(window_size.num_cols.into(), window_size.num_lines.into());
    let term = Arc::new(FairMutex::new(Term::new(Config::default(), &size, listener.clone())));
    let screen = Screen { term: term.clone(), listener, window_size, history: 1000 };
    (screen, term, exits)
}

fn open_at(backend: &HerdrCore, shell: ShellCommand, dir: &Path, placement: Placement) -> Opened {
    let (screen, term, exits) = screen_for();
    let request = OpenRequest {
        session_id: 1,
        shell: Some(shell),
        escape_args: true,
        working_directory: Some(dir.to_path_buf()),
        env: HashMap::new(),
        wsl_probe: None,
        process_tree: ProcessTree::default(),
        colors: Default::default(),
        placement,
        screen,
    };
    let handle = backend.open(request).expect("the core opens a terminal");
    Opened { handle, term, exits }
}

/// Take over a running terminal, as the app adopts one.
fn attach(backend: &HerdrCore, terminal: &str) -> Opened {
    let (screen, term, exits) = screen_for();
    let request = AttachRequest { terminal: terminal.into(), colors: Default::default(), screen };
    let handle = backend.attach(request).expect("the core attaches the terminal");
    Opened { handle, term, exits }
}

/// What `backend` hands out to adopt, once a terminal shows up there.
fn adopted(backend: &HerdrCore, count: usize) -> Vec<Adoptable> {
    let mut found = Vec::new();
    wait_until("the terminals to adopt", PATIENCE, || {
        found.extend(SessionBackend::<HerdrCoreHandle>::adoptable(backend));
        found.len() >= count
    });
    found.sort_by_key(|adoptable| adoptable.placement.position);
    found
}

fn placed(workspace: &Path, position: usize, title: &str) -> Placement {
    Placement {
        workspace: Some(workspace.to_path_buf()),
        position,
        title: Some(title.into()),
        session_id: None,
    }
}

fn screen(term: &FairMutex<Term<Exits>>) -> String {
    rows(term, |grid| 0..grid.screen_lines() as i32)
}

/// The lines scrolled off the top of the screen.
fn scrollback(term: &FairMutex<Term<Exits>>) -> String {
    rows(term, |grid| -(grid.history_size() as i32)..0)
}

fn rows(
    term: &FairMutex<Term<Exits>>,
    range: impl FnOnce(&Grid<Cell>) -> std::ops::Range<i32>,
) -> String {
    let term = term.lock();
    let grid = term.grid();
    let mut text = String::new();
    for line in range(grid) {
        let row = &grid[alacritty_terminal::index::Line(line)];
        text.extend((0..grid.columns()).map(|col| row[alacritty_terminal::index::Column(col)].c));
        text.push('\n');
    }
    text
}

fn wait_until(what: &str, patience: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + patience;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn shows(opened: &Opened, needle: &str) {
    wait_until(needle, PATIENCE, || screen(&opened.term).contains(needle));
}

/// A job the shell runs makes the session busy, as the close confirmation
/// reads it, on every platform the core runs on.
#[test]
#[ignore = "needs hc on PATH"]
fn a_job_under_the_shell_reads_as_busy() {
    let core = Core::start();
    let backend = HerdrCore::new("hc");
    let opened = open(&backend, shell(), core.dir());
    opened.handle.write(b"echo shell-is-up\r".to_vec());
    shows(&opened, "shell-is-up");
    assert!(!opened.handle.signals().foreground_job, "an idle shell reads as idle");

    let job: &[u8] = if cfg!(windows) { b"ping -n 60 127.0.0.1\r" } else { b"sleep 60\r" };
    opened.handle.write(job.to_vec());
    wait_until("the job to read as busy", PATIENCE, || opened.handle.signals().foreground_job);
}

#[test]
#[ignore = "needs hc on PATH"]
fn a_session_paints_takes_keys_resizes_and_reports_its_foreground() {
    let core = Core::start();
    let backend = HerdrCore::new("hc");
    let mut opened = open(&backend, shell(), core.dir());

    opened.handle.write(b"echo painted-by-core\r".to_vec());
    shows(&opened, "painted-by-core");

    opened.handle.resize(window(100, 30));
    let address = opened.handle.terminal().to_string();
    wait_until("the core takes the new size", PATIENCE, || {
        let record = core.get(&address);
        record["cols"] == 100 && record["rows"] == 30
    });

    // Windows has no foreground group, so the core names the program the
    // terminal started there.
    let (typed, expected): (&[u8], &str) =
        if cfg!(windows) { (b"", "cmd") } else { (b"sleep 30\r", "sleep") };
    opened.handle.write(typed.to_vec());
    wait_until("the foreground program", PATIENCE, || {
        opened.handle.foreground().is_some_and(|program| {
            program.argv.iter().any(|arg| arg.to_ascii_lowercase().contains(expected))
        })
    });
    let program = opened.handle.foreground().expect("a foreground program");
    let cwd = program.cwd.expect("the core reports the foreground's cwd");
    assert_eq!(
        cwd.canonicalize().expect("the cwd exists"),
        core.dir().canonicalize().expect("the temp dir exists")
    );
    opened.handle.kill();
}

/// A shell such as nushell refuses a `PWD` that ends in a separator, so a
/// working directory spelled with one reaches the terminal without it.
#[test]
#[ignore = "needs hc on PATH"]
fn a_trailing_separator_never_reaches_the_shells_pwd() {
    let core = Core::start();
    let backend = HerdrCore::new("hc");
    let dir = PathBuf::from(format!("{}{}", core.dir().display(), std::path::MAIN_SEPARATOR));
    let opened = open(&backend, shell(), &dir);

    let print_pwd: &[u8] = if cfg!(windows) { b"echo [%PWD%]\r" } else { b"echo \"[$PWD]\"\r" };
    opened.handle.write(print_pwd.to_vec());
    shows(&opened, &format!("[{}]", core.dir().display()));
    assert!(!screen(&opened.term).contains(&format!("[{}]", dir.display())));
}

#[test]
#[ignore = "needs hc on PATH"]
fn closing_a_session_kills_its_terminal_and_removes_its_record() {
    let core = Core::start();
    let backend = HerdrCore::new("hc");
    let mut opened = open(&backend, shell(), core.dir());
    let address = opened.handle.terminal().to_string();
    assert!(core.record(&address).is_some_and(|record| record["state"] == "live"));

    opened.handle.kill();

    wait_until("the record to go", PATIENCE, || core.record(&address).is_none());
}

#[test]
#[ignore = "needs hc on PATH"]
fn a_program_exit_closes_its_session_and_removes_its_record() {
    let core = Core::start();
    let backend = HerdrCore::new("hc");
    let exiting = if cfg!(windows) {
        ShellCommand::new("cmd.exe".into(), vec![
            "/c".into(),
            "ping -n 2 127.0.0.1 >nul & exit 3".into(),
        ])
    } else {
        ShellCommand::new("sh".into(), vec!["-c".into(), "sleep 1; exit 3".into()])
    };
    let mut opened = open(&backend, exiting, core.dir());
    let address = opened.handle.terminal().to_string();

    let status = opened.exits.recv_timeout(PATIENCE).expect("the program's exit arrives");
    assert_eq!(status.code(), Some(3));

    // The app closes a session on its child exit, and closing kills.
    opened.handle.kill();
    wait_until("the finished record to go", PATIENCE, || core.record(&address).is_none());
}

/// The holder that owns the core's terminals.
fn holder_pid() -> u32 {
    let status: Value = serde_json::from_slice(&hc(&["server", "status", "--json"]).stdout)
        .expect("server status answers JSON");
    let pid = status["holder"]["pid"].as_u64().expect("a holder runs the terminals");
    u32::try_from(pid).expect("a pid")
}

fn kill_process(pid: u32) {
    let pid = pid.to_string();
    let killed = if cfg!(windows) {
        Command::new("taskkill").args(["/F", "/PID", &pid]).output()
    } else {
        Command::new("kill").args(["-9", &pid]).output()
    };
    assert!(killed.expect("the kill command runs").status.success(), "the holder dies");
}

/// A lost program is not an exit. The core restarts a kept terminal with a
/// fresh shell, and the session stays open and shows that shell.
#[test]
#[ignore = "needs hc on PATH"]
fn a_lost_holder_keeps_each_session_open_on_its_new_shell() {
    let core = Core::start();
    let backend = HerdrCore::new("hc");
    let sessions = [open(&backend, shell(), core.dir()), open(&backend, shell(), core.dir())];
    for (index, opened) in sessions.iter().enumerate() {
        opened.handle.write(format!("echo before-loss-{index}\r").into_bytes());
        shows(opened, &format!("before-loss-{index}"));
    }
    let addresses: Vec<String> =
        sessions.iter().map(|opened| opened.handle.terminal().to_string()).collect();

    kill_process(holder_pid());

    for (index, opened) in sessions.iter().enumerate() {
        wait_until("the new shell", PATIENCE, || {
            opened.handle.write(format!("echo after-loss-{index}\r").into_bytes());
            std::thread::sleep(Duration::from_millis(500));
            screen(&opened.term).contains(&format!("after-loss-{index}"))
        });
        assert!(opened.exits.try_recv().is_err(), "a loss reached the session as an exit");
        let record = core.record(&addresses[index]).expect("the terminal's record stays");
        assert_eq!(record["state"], "live");
    }
}

/// A terminal that is not kept finishes when its holder dies, so the loss
/// reaches the session as its program's exit.
#[test]
#[ignore = "needs hc on PATH"]
fn a_lost_holder_ends_a_session_whose_terminal_is_not_kept() {
    let core = Core::start();
    let backend = HerdrCore::new("hc");
    let opened = open(&backend, shell(), core.dir());
    opened.handle.write(b"echo before-loss\r".to_vec());
    shows(&opened, "before-loss");
    let address = opened.handle.terminal().to_string();
    let unkept = hc(&["keep", &address, "--off"]);
    assert!(unkept.status.success(), "{unkept:?}");

    kill_process(holder_pid());

    opened.exits.recv_timeout(PATIENCE).expect("the loss arrives as an exit");
    assert_eq!(
        core.record(&address).map(|record| record["state"].clone()),
        Some("finished".into())
    );
}

/// A server stop parks a kept terminal and drops the connection. The next
/// core gives it a new shell, and the session reconnects onto that shell.
#[test]
#[ignore = "needs hc on PATH"]
fn a_server_stop_keeps_the_session_open_on_its_new_shell() {
    let core = Core::start();
    let backend = HerdrCore::new("hc");
    let opened = open(&backend, shell(), core.dir());
    opened.handle.write(b"echo before-stop\r".to_vec());
    shows(&opened, "before-stop");
    let address = opened.handle.terminal().to_string();

    let stopped = hc(&["server", "stop"]);
    assert!(stopped.status.success(), "{stopped:?}");

    let deadline = Instant::now() + PATIENCE;
    while !screen(&opened.term).contains("after-stop") {
        assert!(
            Instant::now() < deadline,
            "no new shell; exit {:?}; record {:?}; screen:\n{}",
            opened.exits.try_recv(),
            core.record(&address),
            screen(&opened.term)
        );
        opened.handle.write(b"echo after-stop\r".to_vec());
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(opened.exits.try_recv().is_err(), "a loss reached the session as an exit");
    assert_eq!(core.record(&address).map(|record| record["state"].clone()), Some("live".into()));
}

/// A kill is final even for a kept terminal, so it reaches the session as
/// its program's exit.
#[test]
#[ignore = "needs hc on PATH"]
fn a_kill_from_outside_ends_the_session_as_an_exit() {
    let core = Core::start();
    let backend = HerdrCore::new("hc");
    let opened = open(&backend, shell(), core.dir());
    let address = opened.handle.terminal().to_string();

    let killed = hc(&["kill", &address]);
    assert!(killed.status.success(), "{killed:?}");

    opened.exits.recv_timeout(PATIENCE).expect("the kill arrives as an exit");
    assert_eq!(
        core.record(&address).map(|record| record["state"].clone()),
        Some("finished".into())
    );
}

/// A session whose parser is stalled, as one is behind a UI that holds its
/// grid, must not hold up the stream of another session on the same core.
#[test]
#[ignore = "needs hc on PATH"]
fn a_flooding_session_does_not_delay_echo_in_another() {
    let core = Core::start();
    let backend = HerdrCore::new("hc");
    let flood = if cfg!(windows) {
        ShellCommand::new("powershell.exe".into(), vec![
            "-NoProfile".into(),
            "-Command".into(),
            "$s = 'flood ' * 500; while ($true) { [Console]::Out.Write($s) }".into(),
        ])
    } else {
        ShellCommand::new("sh".into(), vec!["-c".into(), "yes flood flood flood".into()])
    };
    let flooding = open(&backend, flood, core.dir());
    let quiet = open(&backend, shell(), core.dir());
    shows(&flooding, "flood");

    let held = flooding.term.lock();
    let mut echoed = Vec::new();
    for round in 0..5 {
        let needle = format!("echo-round-{round}");
        let typed = Instant::now();
        quiet.handle.write(format!("echo {needle}\r").into_bytes());
        shows(&quiet, &needle);
        echoed.push(typed.elapsed());
    }
    drop(held);

    let slowest = echoed.into_iter().max().unwrap_or_default();
    assert!(slowest < Duration::from_secs(2), "an echo took {slowest:?} beside the flood");
}

/// Quitting detaches every session and leaves its terminal running. The
/// next launch finds each one with the workspace and position it had, and
/// its screen comes back with it.
#[test]
#[ignore = "needs hc on PATH"]
fn after_a_quit_the_next_launch_finds_each_terminal_where_it_was() {
    let core = Core::start();
    let workspace = core.dir().join("feature");
    std::fs::create_dir_all(&workspace).expect("the workspace dir");
    let (first, second) = {
        let before = HerdrCore::new("hc");
        let mut first = open_at(&before, shell(), &workspace, placed(&workspace, 0, "app / one"));
        let mut second = open_at(&before, shell(), core.dir(), Placement::default());
        first.handle.write(b"echo left-running\r".to_vec());
        shows(&first, "left-running");
        // The session moved after it opened, so the labels follow it.
        first.handle.place(&placed(&workspace, 1, "app / one"));
        let terminals = (first.handle.terminal().to_string(), second.handle.terminal().to_string());
        first.handle.detach().expect("a core terminal detaches");
        second.handle.detach().expect("a core terminal detaches");
        terminals
    };
    wait_until("the moved session's labels", PATIENCE, || {
        core.get(&first)["labels"]["alacritree.position"] == "1"
    });

    let after = HerdrCore::new("hc");
    SessionBackend::<HerdrCoreHandle>::ready(&after).expect("the next launch connects");
    let found = adopted(&after, 2);

    assert_eq!(found, vec![
        Adoptable { terminal: second.clone(), placement: Placement::default() },
        Adoptable { terminal: first.clone(), placement: placed(&workspace, 1, "app / one") },
    ]);
    assert!(core.get(&first)["state"] == "live" && core.get(&second)["state"] == "live");
    let reattached = attach(&after, &first);
    shows(&reattached, "left-running");
}

/// Killing the `hc connect` child shows the reconnecting state in each
/// session. Then each one repaints from the core with its scrollback, and
/// takes keys again.
#[test]
#[ignore = "needs hc on PATH"]
fn after_the_connection_drops_each_session_comes_back_with_its_scrollback() {
    let core = Core::start();
    let backend = HerdrCore::new("hc");
    let mut sessions = [open(&backend, shell(), core.dir()), open(&backend, shell(), core.dir())];
    let count: &[u8] = if cfg!(windows) {
        b"for /L %i in (1,1,40) do @echo line-%i\r"
    } else {
        b"i=0; while [ $i -lt 40 ]; do i=$((i+1)); echo line-$i; done\r"
    };
    for (index, opened) in sessions.iter().enumerate() {
        opened.handle.write(format!("echo scrolled-away-{index}\r").into_bytes());
        opened.handle.write(count.to_vec());
        shows(opened, "line-40");
        assert!(scrollback(&opened.term).contains(&format!("scrolled-away-{index}")));
    }

    backend.kill_connection();

    for opened in &sessions {
        shows(opened, "reconnecting");
    }
    // A session that moves while the connection is down records its new
    // place once it is back.
    sessions[0].handle.place(&placed(core.dir(), 4, "app / moved"));
    for (index, opened) in sessions.iter().enumerate() {
        let marker = format!("scrolled-away-{index}");
        wait_until("the repaint from the core", PATIENCE, || {
            let (screen, scrollback) = (screen(&opened.term), scrollback(&opened.term));
            !screen.contains("reconnecting")
                && !scrollback.contains("reconnecting")
                && scrollback.contains(&marker)
        });
        opened.handle.write(format!("echo typed-after-{index}\r").into_bytes());
        shows(opened, &format!("typed-after-{index}"));
    }
    let moved = sessions[0].handle.terminal().to_string();
    wait_until("the moved session's labels", PATIENCE, || {
        core.get(&moved)["labels"]["alacritree.position"] == "4"
    });
}

/// An agent puts a terminal into a workspace with a label alone. It is
/// handed out once, and a terminal without the label never is.
#[test]
#[ignore = "needs hc on PATH"]
fn a_terminal_an_agent_labels_with_a_workspace_is_adoptable_once() {
    let core = Core::start();
    let backend = HerdrCore::new("hc");
    SessionBackend::<HerdrCoreHandle>::ready(&backend).expect("the backend connects");
    let workspace = core.dir().display().to_string();
    let label = format!("alacritree.workspace={workspace}");
    let program = if cfg!(windows) { "cmd.exe" } else { "sh" };
    let unlabelled = hc(&["run", "--bg", "--keep", "--no-parent", "--json", program]);
    assert!(unlabelled.status.success(), "{unlabelled:?}");
    let labelled =
        hc(&["run", "--bg", "--keep", "--no-parent", "--label", &label, "--json", program]);
    assert!(labelled.status.success(), "{labelled:?}");
    let address = serde_json::from_slice::<Value>(&labelled.stdout).expect("run answers JSON")
        ["address"]
        .as_str()
        .expect("an address")
        .to_string();

    let found = adopted(&backend, 1);

    assert_eq!(found.len(), 1, "only the labelled terminal: {found:?}");
    assert_eq!(found[0].terminal, address);
    assert_eq!(found[0].placement.workspace.as_deref(), Some(core.dir()));
    assert!(SessionBackend::<HerdrCoreHandle>::adoptable(&backend).is_empty());
}

/// The labels of a terminal that an agent starts in a workspace while the
/// connection is up, once alacritree adopts it with `share_with_herdr_app`
/// set to `share`.
fn labels_once_adopted(share: bool) -> serde_json::Map<String, Value> {
    let core = Core::start();
    let config = HerdrCoreConfig { share_with_herdr_app: share, ..HerdrCoreConfig::default() };
    let backend = HerdrCores::new(&config).on(&Side::Native);
    SessionBackend::<HerdrCoreHandle>::ready(&backend).expect("the backend connects");
    let label = format!("alacritree.workspace={}", core.dir().display());
    let program = if cfg!(windows) { "cmd.exe" } else { "sh" };
    let run = hc(&["run", "--bg", "--keep", "--no-parent", "--label", &label, "--json", program]);
    assert!(run.status.success(), "{run:?}");
    let address = serde_json::from_slice::<Value>(&run.stdout).expect("run answers JSON")
        ["address"]
        .as_str()
        .expect("an address")
        .to_string();

    let found = adopted(&backend, 1);
    assert_eq!(found[0].terminal, address);

    let labels = || core.get(&address)["labels"].as_object().cloned().unwrap_or_default();
    let shown = || labels().contains_key("herdr.space");
    if share {
        wait_until("the herdr-app labels", PATIENCE, shown);
    } else {
        // The label call goes out without an answer, so give it time to land.
        std::thread::sleep(Duration::from_secs(1));
    }
    labels()
}

/// herdr-app shows a terminal an agent puts into a workspace mid-run, in
/// that checkout's space, without waiting for the next launch.
#[test]
#[ignore = "needs hc on PATH"]
fn a_terminal_adopted_mid_run_is_shared_with_herdr_app() {
    let labels = labels_once_adopted(true);

    assert!(labels["herdr.space"].as_str().is_some_and(|space| space.starts_with("at-")));
    assert!(labels.contains_key("herdr.tab"), "{labels:?}");
}

#[test]
#[ignore = "needs hc on PATH"]
fn a_terminal_adopted_mid_run_without_sharing_gets_no_herdr_label() {
    let labels = labels_once_adopted(false);

    assert!(labels.keys().all(|key| !key.starts_with("herdr.")), "{labels:?}");
}

/// A core of `session` with a live terminal, on a socket hc stops looking
/// at, and the core's pid and terminal address.
fn hidden_core(session: &str) -> (Core, u64, String) {
    let core = Core::start_in(session);
    let terminal = {
        let before = HerdrCore::new("hc");
        let mut opened = open(&before, shell(), core.dir());
        opened.handle.detach().expect("a core terminal detaches");
        opened.handle.terminal().to_string()
    };
    let pid = core.pid();
    // SAFETY: as in `Core::start`; no thread reads the environment here.
    unsafe { std::env::remove_var("HC_SOCKET") };
    (core, pid, terminal)
}

/// The core that `backend`'s refused connect finds elsewhere. It comes
/// without waiting out hc's start of a second core, so a new session never
/// stalls on a core it cannot reach.
fn found_elsewhere(backend: &HerdrCore) -> CoreElsewhere {
    let asked = Instant::now();
    let refused = backend.connect().expect_err("hc does not reach the core");
    assert!(asked.elapsed() < Duration::from_secs(5), "the refusal took {:?}", asked.elapsed());
    refused.elsewhere.expect("the core's lock names where it listens")
}

/// A core of the default session that listens where hc no longer looks is
/// found through its lock. hc's config takes its socket, so alacritree and
/// `hc` both reach that same core, its terminal still live.
#[test]
#[ignore = "needs hc on PATH"]
fn a_default_core_hc_does_not_resolve_is_reached_through_hcs_config() {
    let (core, pid, terminal) = hidden_core("default");

    let backend = HerdrCore::new("hc");
    let found = found_elsewhere(&backend);
    assert_eq!(found, CoreElsewhere {
        socket: PathBuf::from(&core.socket),
        remedy: Remedy::WriteConfig
    });

    backend.use_core(&found).expect("hc's config takes the socket");

    backend.connect().expect("alacritree reaches the core");
    assert!(hc(&["ls", "--json"]).status.success(), "hc ls reaches the core");
    assert_eq!(core.pid(), pid, "the same core answers");
    assert_eq!(core.get(&terminal)["state"], "live");
}

/// hc's `socket` key serves every session, so a core of another session
/// is reached through `HC_SOCKET` on alacritree's own hc, and hc's config
/// stays untouched.
#[test]
#[ignore = "needs hc on PATH"]
fn a_core_of_another_session_is_reached_without_touching_hcs_config() {
    let (core, pid, terminal) = hidden_core(&format!("alacritree-e2e-{}", std::process::id()));

    let backend = HerdrCore::new("hc");
    let found = found_elsewhere(&backend);
    assert_eq!(found, CoreElsewhere {
        socket: PathBuf::from(&core.socket),
        remedy: Remedy::SetEnv
    });

    backend.use_core(&found).expect("alacritree's own hc takes the socket");

    backend.connect().expect("alacritree reaches the core");
    assert!(!core.dir().join("config").join("herdr-core").join("config.toml").exists());
    let ours = attach(&backend, &terminal);
    assert_eq!(ours.handle.terminal(), terminal);
    // `Core::pid` runs hc outside alacritree, which still looks elsewhere.
    // SAFETY: as in `Core::start`; no thread reads the environment here.
    unsafe { std::env::set_var("HC_SOCKET", &core.socket) };
    assert_eq!(core.pid(), pid, "the same core answers");
}

/// alacritree's terminals carry no `herdr.*` label, so herdr-app neither
/// adopts nor kills them, and they keep a lost program's place as a shell.
#[test]
#[ignore = "needs hc on PATH"]
fn own_terminals_carry_alacritree_labels_and_where_and_keep() {
    let core = Core::start();
    let backend = HerdrCore::new("hc");
    let mut opened = open_at(&backend, shell(), core.dir(), placed(core.dir(), 0, "app / main"));
    let address = opened.handle.terminal().to_string();

    let record = core.get(&address);
    let labels = record["labels"].as_object().expect("labels").clone();
    assert!(
        labels.keys().all(|key| key.starts_with("alacritree.") || key == "where"),
        "{labels:?}"
    );
    assert_eq!(labels["where"], "app / main");
    assert_eq!(record["keep"], true);
    let listed = core.records().into_iter().find(|r| r["address"] == address.as_str());
    assert_eq!(listed.map(|r| r["where"].clone()), Some(Value::from("app / main")));
    opened.handle.kill();
}

#[cfg(windows)]
use alacritree_common::{side, wsl};

/// A throwaway core inside a WSL distro: the one `ALACRITREE_E2E_DISTRO`
/// names, else the default. Its `HC_*` and XDG paths reach every `wsl.exe`
/// this test starts through `WSLENV`, the helper and the resident
/// connection included.
#[cfg(windows)]
struct DistroCore {
    distro: String,
    dir: String,
}

#[cfg(windows)]
impl DistroCore {
    /// The variables every `wsl.exe` of this test passes into the distro.
    const SHARED: &str = "HC_SOCKET:HC_SESSION:XDG_STATE_HOME:XDG_CONFIG_HOME";

    /// `None` when this machine has no distro.
    fn start() -> Option<Self> {
        let distro = std::env::var("ALACRITREE_E2E_DISTRO")
            .ok()
            .or_else(|| wsl::distros().into_iter().find(|d| d.is_default).map(|d| d.name))?;
        let dir = format!("/tmp/alacritree-hc-e2e-{}", std::process::id());
        let vars = [
            ("HC_SOCKET", format!("{dir}/hc.sock")),
            ("HC_SESSION", format!("alacritree-e2e-{}", std::process::id())),
            ("XDG_STATE_HOME", format!("{dir}/state")),
            ("XDG_CONFIG_HOME", format!("{dir}/config")),
            ("WSLENV", Self::SHARED.to_string()),
        ];
        for (name, value) in vars {
            // SAFETY: nextest gives each test a process of its own, and this
            // runs before any thread.
            unsafe { std::env::set_var(name, value) };
        }
        let made = wsl::command(&distro, None).args(["mkdir", "-p", &dir]).output();
        assert!(made.is_ok_and(|out| out.status.success()), "the core's directory exists");
        let core = Self { distro, dir };
        let started = core.hc(&["server", "start"]);
        assert!(started.status.success(), "the throwaway core starts: {started:?}");
        Some(core)
    }

    /// Stop passing `HC_SOCKET` to the `wsl.exe` launches the backend makes,
    /// as a shell rc file's export never reaches them.
    fn hide_socket(&self) {
        let shared =
            Self::SHARED.strip_prefix("HC_SOCKET:").expect("`SHARED` leads with HC_SOCKET");
        // SAFETY: as in `start`, before the backend starts a thread.
        unsafe { std::env::set_var("WSLENV", shared) };
    }

    /// `hc` inside the distro, found the way the resident connection finds
    /// it. It always reaches the core, even after [`Self::hide_socket`].
    fn hc(&self, args: &[&str]) -> std::process::Output {
        side::wsl_command_with_clean_stdout(&self.distro, "hc", args)
            .env("WSLENV", Self::SHARED)
            .output()
            .expect("wsl.exe runs")
    }

    fn live_terminals(&self) -> usize {
        let listed: Value =
            serde_json::from_slice(&self.hc(&["ls", "--json"]).stdout).unwrap_or_default();
        listed["terminals"].as_array().map_or(0, Vec::len)
    }

    fn windows_dir(&self) -> std::path::PathBuf {
        wsl::linux_to_windows(&self.dir, &self.distro)
    }
}

#[cfg(windows)]
impl Drop for DistroCore {
    fn drop(&mut self) {
        let _ = self.hc(&["server", "stop"]);
        let _ = wsl::command(&self.distro, None).args(["rm", "-rf", &self.dir]).output();
    }
}

/// The `wsl.exe` processes this test started that still run.
#[cfg(windows)]
fn wsl_launches() -> usize {
    let filter = format!("ParentProcessId={} and Name='wsl.exe'", std::process::id());
    let count = format!("@(Get-CimInstance Win32_Process -Filter \"{filter}\").Count");
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-Command", &count])
        .output()
        .expect("powershell runs");
    String::from_utf8_lossy(&out.stdout).trim().parse().expect("a count")
}

/// Every process this test process starts from [`Launches::track`] on,
/// counted by a job object, so one that already exited counts too.
#[cfg(windows)]
struct Launches(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Launches {
    fn track() -> Self {
        use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        // SAFETY: a null name and attributes ask for an unnamed job with
        // default security, and the pseudo handle needs no closing.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        assert!(!job.is_null(), "a job object");
        let assigned = unsafe { AssignProcessToJobObject(job, GetCurrentProcess()) };
        assert_ne!(assigned, 0, "the test process joins the job");
        Self(job)
    }

    /// Processes the job held since it was made, this test process included.
    fn total(&self) -> u32 {
        use windows_sys::Win32::System::JobObjects::{
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JobObjectBasicAccountingInformation,
            QueryInformationJobObject,
        };

        // SAFETY: the struct is plain integers, for which zero is valid.
        let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: the buffer is the struct this information class fills, at
        // its own size.
        let read = unsafe {
            QueryInformationJobObject(
                self.0,
                JobObjectBasicAccountingInformation,
                (&raw mut info).cast(),
                size_of_val(&info) as u32,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(read, 0, "the job's accounting");
        info.TotalProcesses
    }
}

/// A distro's sessions run in the distro's own core, start where they were
/// asked to, and launch no process once the helper, the resident
/// connection and the distro's keeper are up. A foreign terminal starts the
/// keeper first, since the core's first running terminal does.
#[cfg(windows)]
#[test]
#[ignore = "needs a WSL distro with hc"]
fn distro_sessions_share_one_resident_connection() {
    use alacritree_common::wsl_helper;
    use alacritree_herdr_core::{HerdrCoreConfig, HerdrCores};
    use alacritree_session_backend::{SessionHandle, Side};

    let launches = Launches::track();
    let Some(core) = DistroCore::start() else {
        eprintln!("skip: this machine has no WSL distro");
        return;
    };
    let cores = HerdrCores::new(&HerdrCoreConfig::default());
    cores.open_beside_wsl_helpers();
    let backend = cores.on(&Side::Wsl(core.distro.clone()));
    wait_until("the distro's helper", Duration::from_secs(60), || {
        wsl_helper::client(&core.distro).is_some()
    });
    wait_until("the resident connection beside the helper", Duration::from_secs(30), || {
        wsl_launches() == 2
    });
    let foreign = core.hc(&["run", "--bg", "--keep", "--no-parent", "--json"]);
    assert!(foreign.status.success(), "a terminal that starts the distro's keeper: {foreign:?}");
    let foreign: Value = serde_json::from_slice(&foreign.stdout).expect("run answers JSON");
    wait_until("the distro's keeper", PATIENCE, || backend.keeper_runs());
    let before = launches.total();

    let mut first = open(&backend, shell(), &core.windows_dir());
    first.handle.write(b"echo first-in-distro\r".to_vec());
    shows(&first, "first-in-distro");
    let mut second = open(&backend, shell(), &core.windows_dir());
    second.handle.write(b"echo second-in-distro\r".to_vec());
    shows(&second, "second-in-distro");

    assert_eq!(launches.total(), before, "a session launched a process");
    wait_until("the core's cwd", PATIENCE, || first.handle.cwd().is_some());
    assert_eq!(first.handle.cwd(), Some(core.windows_dir()));
    assert_eq!(core.live_terminals(), 3, "both sessions run beside the foreign terminal");

    first.handle.kill();
    second.handle.kill();
    let ended = core.hc(&["kill", foreign["address"].as_str().expect("an address")]);
    assert!(ended.status.success(), "the foreign terminal ends: {ended:?}");
    wait_until("the keeper to exit after the last terminal", Duration::from_secs(30), || {
        !backend.keeper_runs()
    });
}

#[cfg(windows)]
fn running(pid: u32) -> bool {
    let probe = format!("[bool](Get-Process -Id {pid} -ErrorAction SilentlyContinue)");
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-Command", &probe])
        .output()
        .expect("powershell runs");
    String::from_utf8_lossy(&out.stdout).trim() == "True"
}

/// A distro core gets one keeper while it runs a terminal, with no quit.
/// The keeper outlasts its status polls, a quit's keeper for the same core
/// exits at once, and the keeper exits once the last terminal ends. The
/// next terminal gets a new one.
#[cfg(windows)]
#[test]
#[ignore = "needs a WSL distro with hc"]
fn a_distro_core_keeps_one_keeper_while_it_runs_a_terminal() {
    use alacritree_herdr_core::HerdrCores;
    use alacritree_session_backend::Side;

    let Some(core) = DistroCore::start() else {
        eprintln!("skip: this machine has no WSL distro");
        return;
    };
    let cores = HerdrCores::new(&HerdrCoreConfig::default());
    let backend = cores.on(&Side::Wsl(core.distro.clone()));
    let mut opened = open(&backend, shell(), &core.windows_dir());
    opened.handle.write(b"echo kept-in-distro\r".to_vec());
    shows(&opened, "kept-in-distro");
    let terminal = opened.handle.terminal().to_string();
    opened.handle.detach().expect("a detach");
    wait_until("a keeper with no quit", PATIENCE, || backend.keeper_runs());

    let [quit] = cores.leave_keepers()[..] else { panic!("a quit's keeper to turn away") };
    wait_until("the quit's keeper to find the lock held", PATIENCE, || !running(quit));
    std::thread::sleep(Duration::from_secs(15));
    assert!(backend.keeper_runs(), "the keeper outlasts a status poll while a terminal lives");

    let killed = core.hc(&["kill", &terminal]);
    assert!(killed.status.success(), "the terminal ends: {killed:?}");
    wait_until("the keeper to exit after the last terminal", Duration::from_secs(30), || {
        !backend.keeper_runs()
    });

    let mut next = open(&backend, shell(), &core.windows_dir());
    wait_until("a keeper for the next terminal", PATIENCE, || backend.keeper_runs());
    next.handle.kill();
    wait_until("the next keeper to exit", Duration::from_secs(30), || !backend.keeper_runs());
}

/// A distro core that listens at a socket the `wsl.exe` environment does not
/// name, as one started from a shell that exports `HC_SOCKET`, still runs
/// the distro's sessions, and its keeper holds it up.
#[cfg(windows)]
#[test]
#[ignore = "needs a WSL distro with hc"]
fn a_distro_core_on_a_socket_hc_does_not_resolve_runs_its_sessions() {
    use alacritree_herdr_core::HerdrCores;
    use alacritree_session_backend::Side;

    let Some(core) = DistroCore::start() else {
        eprintln!("skip: this machine has no WSL distro");
        return;
    };
    core.hide_socket();
    let cores = HerdrCores::new(&HerdrCoreConfig::default());
    let backend = cores.on(&Side::Wsl(core.distro.clone()));

    let mut opened = open(&backend, shell(), &core.windows_dir());
    opened.handle.write(b"echo found-where-hc-did-not-look\r".to_vec());
    shows(&opened, "found-where-hc-did-not-look");
    assert_eq!(core.live_terminals(), 1, "the session runs in the core that listens elsewhere");
    wait_until("a keeper for that core", PATIENCE, || backend.keeper_runs());

    opened.handle.kill();
    wait_until("the keeper to exit after the last terminal", Duration::from_secs(30), || {
        !backend.keeper_runs()
    });
}

/// A terminal another client starts in a distro's core lists as a pane of
/// the distro's side, under the workspace its Linux directory maps to, and a
/// kill reaches the distro's core.
#[cfg(windows)]
#[test]
#[ignore = "needs a WSL distro with hc"]
fn a_terminal_hc_runs_in_a_distro_lists_as_a_pane_of_that_side() {
    use alacritree_multiplexer::MultiplexerKind;

    let Some(core) = DistroCore::start() else {
        eprintln!("skip: this machine has no WSL distro");
        return;
    };
    let side = Side::Wsl(core.distro.clone());
    let cores = HerdrCores::new(&HerdrCoreConfig::default());
    assert!(cores.on(&side).connect().is_ok(), "the distro's connection opens");
    let space = format!("herdr.space={}", core.dir);
    let started = core.hc(&[
        "run",
        "--bg",
        "--keep",
        "--no-parent",
        "--json",
        "--name",
        "hook",
        "--cwd",
        &core.dir,
        "--label",
        &space,
    ]);
    assert!(started.status.success(), "{started:?}");
    let record: Value = serde_json::from_slice(&started.stdout).expect("run answers JSON");
    let address = record["address"].as_str().expect("an address").to_string();
    let mut panes = HerdrCorePanes::new(cores.clone(), HerdrCoreConfig::default());

    wait_until("the distro's pane to list", PATIENCE, || {
        panes.poll(&|_| false);
        panes.find(&side, &address).is_some()
    });
    let workspace = core.windows_dir();
    let listed = panes.listed(&[], std::slice::from_ref(&workspace));
    let row = listed.iter().find(|row| row.key.terminal_id == address).expect("a listed row");
    assert_eq!(row.key.multiplexer, MultiplexerKind::HerdrCore);
    assert_eq!(row.key.side, side);
    assert_eq!(row.workspace.as_ref(), Some(&workspace));

    let key = row.key.clone();
    panes.kill_terminal(&key);
    wait_until("the kill to reach the distro's core", PATIENCE, || core.live_terminals() == 0);
}

/// Start `argv` the way another client does: kept, and with no alacritree
/// label. Answers its address.
fn run_foreign(argv: &[&str]) -> String {
    let mut args = vec!["run", "--bg", "--keep", "--no-parent", "--json"];
    args.extend_from_slice(argv);
    let started = hc(&args);
    assert!(started.status.success(), "{started:?}");
    let record: Value = serde_json::from_slice(&started.stdout).expect("run answers JSON");
    record["address"].as_str().expect("an address").to_string()
}

fn foreign_shell() -> String {
    run_foreign(&[])
}

/// Watch a running terminal from a grid of `window_size`.
fn watch(backend: &HerdrCore, terminal: &str, window_size: WindowSize) -> Opened {
    let (screen, term, exits) = screen_sized(window_size);
    let request = AttachRequest { terminal: terminal.into(), colors: Default::default(), screen };
    let handle = backend.watch(request).expect("the core lets a session watch the terminal");
    Opened { handle, term, exits }
}

/// The size the core keeps for `address`, as `(cols, rows)`.
fn size_of(core: &Core, address: &str) -> (Value, Value) {
    let record = core.get(address);
    (record["cols"].clone(), record["rows"].clone())
}

/// A terminal that `hc run` started carries no alacritree label, so it is
/// listed as a pane, and opening it types into it.
#[test]
#[ignore = "needs hc on PATH"]
fn a_terminal_hc_runs_without_labels_lists_as_a_pane_and_opens_interactively() {
    let core = Core::start();
    let cores = HerdrCores::new(&HerdrCoreConfig::default());
    let backend = cores.on(&Side::Native);
    let own = open(&backend, shell(), core.dir());
    let address = foreign_shell();
    let mut panes = HerdrCorePanes::new(cores, HerdrCoreConfig::default());

    wait_until("the foreign pane to list", PATIENCE, || {
        panes.poll(&|_| false);
        panes.find(&Side::Native, &address).is_some()
    });
    let listed: Vec<&str> = panes.panes().iter().map(|(_, p)| p.terminal_id.as_str()).collect();
    assert_eq!(listed, vec![address.as_str()], "alacritree's own terminal is no pane");
    assert!(panes.find(&Side::Native, own.handle.terminal()).is_none());

    let opened = attach(&backend, &address);
    opened.handle.write(b"echo typed-into-foreign\r".to_vec());
    shows(&opened, "typed-into-foreign");
}

/// A watching session follows the terminal's output, and what it types or
/// how it is sized never reaches the terminal.
#[test]
#[ignore = "needs hc on PATH"]
fn watching_a_terminal_never_types_into_it_or_resizes_it() {
    let core = Core::start();
    let backend = HerdrCore::new("hc");
    let address = foreign_shell();
    wait_until("the terminal to start", PATIENCE, || core.get(&address)["state"] == "live");
    let before = size_of(&core, &address);

    let mut watched = watch(&backend, &address, window(97, 33));
    watched.handle.write(b"echo typed-by-watcher\r".to_vec());
    watched.handle.resize(window(101, 31));
    assert!(hc(&["send", &address, "echo sent-by-hc"]).status.success());

    shows(&watched, "sent-by-hc");
    assert!(!screen(&watched.term).contains("typed-by-watcher"), "{}", screen(&watched.term));
    assert_eq!(size_of(&core, &address), before, "the watch left the terminal's size alone");
}

/// Closing a session on a foreign terminal only lets go of it. The terminal
/// keeps running and takes keys from the next session.
#[test]
#[ignore = "needs hc on PATH"]
fn closing_a_foreign_session_leaves_its_terminal_running() {
    let core = Core::start();
    let backend = HerdrCore::new("hc");
    let address = foreign_shell();
    let mut first = attach(&backend, &address);
    first.handle.write(b"echo first-session\r".to_vec());
    shows(&first, "first-session");

    first.handle.kill();

    let second = attach(&backend, &address);
    second.handle.write(b"echo second-session\r".to_vec());
    shows(&second, "second-session");
    assert_eq!(core.get(&address)["state"], "live");
}

/// The record of a foreign terminal whose program ended is its creator's to
/// remove, so closing the session it ended in leaves the record.
#[test]
#[ignore = "needs hc on PATH"]
fn a_foreign_terminal_keeps_its_record_after_its_program_ends() {
    let core = Core::start();
    let backend = HerdrCore::new("hc");
    let exiting: &[&str] = if cfg!(windows) {
        &["cmd.exe", "/c", "ping -n 3 127.0.0.1 >nul & exit 3"]
    } else {
        &["sh", "-c", "sleep 2; exit 3"]
    };
    let address = run_foreign(exiting);
    let mut opened = attach(&backend, &address);

    let status = opened.exits.recv_timeout(PATIENCE).expect("the program's exit arrives");
    assert_eq!(status.code(), Some(3));
    opened.handle.kill();
    // Closing an own terminal removes its record, so once that one is gone
    // a removal of the foreign record would have landed too.
    let mut own = open(&backend, shell(), core.dir());
    let own_address = own.handle.terminal().to_string();
    own.handle.kill();
    wait_until("the own record to go", PATIENCE, || core.record(&own_address).is_none());

    assert!(core.record(&address).is_some(), "the foreign record stays");
}
