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

use alacritree_herdr_core::{HerdrCore, HerdrCoreHandle};
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
        let home = tempfile::tempdir().expect("a temp dir");
        let socket = if cfg!(windows) {
            format!(r"\\.\pipe\alacritree-hc-e2e-{}", std::process::id())
        } else {
            home.path().join("hc.sock").display().to_string()
        };
        let vars = [
            ("HC_SOCKET", socket.clone()),
            ("HC_SESSION", format!("alacritree-e2e-{}", std::process::id())),
            ("XDG_STATE_HOME", home.path().join("state").display().to_string()),
            ("XDG_CONFIG_HOME", home.path().join("config").display().to_string()),
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
    let (sender, exits) = mpsc::channel();
    let listener = Exits(sender);
    let term =
        Arc::new(FairMutex::new(Term::new(Config::default(), &Size(80, 24), listener.clone())));
    let screen =
        Screen { term: term.clone(), listener, window_size: window(80, 24), history: 1000 };
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
    Placement { workspace: Some(workspace.to_path_buf()), position, title: Some(title.into()) }
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

/// A core that listens where hc no longer looks is found through its lock.
/// Pointing hc there reaches that same core, its terminal still live, from
/// alacritree and from `hc` alike.
#[test]
#[ignore = "needs hc on PATH"]
fn a_core_hc_does_not_resolve_is_found_and_reached_without_a_restart() {
    let core = Core::start();
    let terminal = {
        let before = HerdrCore::new("hc");
        let mut opened = open(&before, shell(), core.dir());
        opened.handle.detach().expect("a core terminal detaches");
        opened.handle.terminal().to_string()
    };
    let pid = core.pid();
    // SAFETY: as in `Core::start`; no thread reads the environment here.
    unsafe { std::env::remove_var("HC_SOCKET") };

    let backend = HerdrCore::new("hc");
    let refused = backend.connect().expect_err("hc does not reach the core");
    let socket = refused.elsewhere.expect("the core's lock names where it listens");
    assert_eq!(socket, PathBuf::from(&core.socket));

    backend.point_hc_at(&socket).expect("hc's config takes the socket");

    backend.connect().expect("alacritree reaches the core");
    assert!(hc(&["ls", "--json"]).status.success(), "hc ls reaches the core");
    assert_eq!(core.pid(), pid, "the same core answers");
    assert_eq!(core.get(&terminal)["state"], "live");
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
