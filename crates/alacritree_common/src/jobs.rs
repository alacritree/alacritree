//! One pool for every piece of work that must not run on the UI thread.
//!
//! A handler that gathers its content synchronously cannot draw until the
//! gathering returns, which under CPU load is seconds. Work goes here
//! instead, and the `Blocking` token makes that structural. The helpers that
//! block take one and only a pool worker is handed one, so calling such a
//! helper from `update` does not compile.

use std::cell::Cell;
use std::collections::VecDeque;
use std::io::{self, Read};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::{Child, Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

/// Whether anything on screen is waiting for the job.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Priority {
    /// A pending state is showing until this lands.
    Interactive,
    /// Housekeeping nobody is looking at: status polls, PR lookups, liveness.
    Background,
}

/// A job's cancellation state, shared by its handle, its queued task, and the
/// `Blocking` its worker runs with.
#[derive(Default)]
struct Cancel {
    flag: AtomicBool,
    /// The child this job opted into having killed, while one is running.
    child: Mutex<Option<Child>>,
}

impl Cancel {
    /// Set the flag, then kill whatever child is registered. `Job::drop` runs
    /// on whatever thread drops the handle, sometimes the UI thread, so this
    /// must return without waiting on the child. See `kill_registered`.
    fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
        self.kill_registered();
    }

    /// Kill in place, without taking or waiting. Reaping stays with
    /// `run_cancellable`'s own poll loop, on the worker thread, so a caller
    /// of `cancel` never blocks on the child's exit. The tradeoff is that a
    /// worker whose task never returns leaves its child unreaped. A straight
    /// `wait()` here would carry the same exposure, since nothing else in
    /// this module reaps on the killer's behalf either way.
    fn kill_registered(&self) {
        if let Some(child) = self.child.lock().expect("the cancel slot is poisoned").as_mut() {
            let _ = child.kill();
        }
    }
}

/// How a job stopped running.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JobEnd {
    Returned,
    Panicked,
    /// The handle dropped before the job ended.
    Cancelled,
}

/// One step a job declared, and how it went once it ended.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Step {
    pub label: String,
    /// `None` while the step has not ended.
    pub outcome: Option<Result<(), String>>,
    /// From the previous step's end, or from `set_steps` for the first.
    pub took: Option<Duration>,
}

/// What a job has reported so far.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ProgressSnapshot {
    /// In the order the job declared them.
    pub steps: Vec<Step>,
    pub end: Option<JobEnd>,
}

/// The progress a job's worker, its handle and its readers share.
#[derive(Default)]
struct Progress {
    steps: Vec<Step>,
    /// When the running step began.
    mark: Option<Instant>,
    end: Option<JobEnd>,
}

type SharedProgress = Arc<Mutex<Progress>>;

/// The first end recorded wins. The worker, the handle's drop, and the
/// settled constructors can each reach a job's end, and a handle dropped just
/// after its value landed must still read as returned.
fn settle(progress: &SharedProgress, end: JobEnd) {
    let mut progress = progress.lock().unwrap_or_else(|e| e.into_inner());
    progress.end.get_or_insert(end);
}

/// Reads a job's progress. Outlives the job, so whoever owns the `Job` keeps
/// its cancel-on-drop while others watch.
#[derive(Clone)]
pub struct ProgressReader(SharedProgress);

impl ProgressReader {
    pub fn snapshot(&self) -> ProgressSnapshot {
        let progress = self.0.lock().unwrap_or_else(|e| e.into_inner());
        ProgressSnapshot { steps: progress.steps.clone(), end: progress.end }
    }
}

/// A reader a test writes to by hand, standing in for a job's worker.
#[cfg(any(test, feature = "test-support"))]
impl ProgressReader {
    pub fn scripted() -> Self {
        Self(SharedProgress::default())
    }

    pub fn script_steps(&self, labels: &[&str]) {
        let mut progress = self.0.lock().unwrap_or_else(|e| e.into_inner());
        progress.steps = labels
            .iter()
            .map(|l| Step { label: l.to_string(), outcome: None, took: None })
            .collect();
    }

    pub fn script_done(&self, label: &str, outcome: Result<(), String>, took: Duration) {
        let mut progress = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(step) = progress.steps.iter_mut().find(|s| s.label == label) {
            step.outcome = Some(outcome);
            step.took = Some(took);
        }
    }

    pub fn script_end(&self, end: JobEnd) {
        settle(&self.0, end);
    }
}

/// Proof that the holder runs on a pool worker. The constructor is private
/// to this module, so a blocking helper that takes one cannot be called from
/// the UI thread.
pub struct Blocking {
    cancel: Arc<Cancel>,
    progress: SharedProgress,
    /// The pool whose wake-up a step runs. `None` on the calling thread,
    /// where nothing on screen waits.
    pool: Option<Arc<Shared>>,
}

impl Blocking {
    /// Whether this job's handle has been dropped. Check between steps. A
    /// job doing local work has no child registered for a cancel to kill, so
    /// nothing else would stop it.
    pub fn cancelled(&self) -> bool {
        self.cancel.flag.load(Ordering::Relaxed)
    }

    /// Name every step this job will run, in order. Labels are unique within
    /// a job. Calling again replaces the list. Timing starts here, so work
    /// done before is charged to no step.
    pub fn set_steps(&self, labels: Vec<String>) {
        let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        progress.steps =
            labels.into_iter().map(|label| Step { label, outcome: None, took: None }).collect();
        progress.mark = Some(Instant::now());
    }

    /// Record how `label` ended, and wake the UI so the frame that reads it
    /// happens without input.
    pub fn step_done(&self, label: &str, outcome: Result<(), String>) {
        {
            let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            let took = progress.mark.map(|mark| now.saturating_duration_since(mark));
            progress.mark = Some(now);
            if let Some(step) = progress.steps.iter_mut().find(|s| s.label == label) {
                step.outcome = Some(outcome);
                step.took = took;
            }
        }
        if let Some(pool) = &self.pool {
            pool.run_waker();
        }
    }

    /// Run a child a cancel is allowed to kill, and return what it wrote.
    /// Registering is the opt-in. An unregistered child runs to completion
    /// whatever the caller does with the handle.
    ///
    /// The pipes are not drained until the child exits, so this suits a child
    /// whose output is bounded. A child that fills a pipe would block on the
    /// write and never reach the exit this waits for; use
    /// [`Blocking::run_drained`] for one whose output nobody bounds.
    #[allow(clippy::disallowed_methods)] // Spawning the child is this method's job.
    pub fn run_cancellable(&self, cmd: &mut Command) -> io::Result<Output> {
        let child = cmd.spawn()?;
        self.wait_registered(child, None)?.wait_with_output()
    }

    /// [`Blocking::run_cancellable`] for a child whose output is unbounded,
    /// such as a program the user configured. Its stdout and stderr are read
    /// while it runs, each kept up to [`DRAIN_CAP`] bytes, and it is killed
    /// once `limit` passes, which reads as [`io::ErrorKind::TimedOut`].
    #[allow(clippy::disallowed_methods)] // Spawning the child is this method's job.
    pub fn run_drained(&self, cmd: &mut Command, limit: Duration) -> io::Result<Output> {
        let mut child = cmd.spawn()?;
        let stdout = child.stdout.take().map(|pipe| std::thread::spawn(move || drain(pipe)));
        let stderr = child.stderr.take().map(|pipe| std::thread::spawn(move || drain(pipe)));
        // On a kill the readers are left to finish on their own. A grandchild
        // that inherited a pipe can hold it open past the child's death.
        let mut child = self.wait_registered(child, Some(Instant::now() + limit))?;
        let status = child.wait()?;
        let collect = |reader: Option<std::thread::JoinHandle<Vec<u8>>>| {
            reader.map(|r| r.join().unwrap_or_default()).unwrap_or_default()
        };
        Ok(Output { status, stdout: collect(stdout), stderr: collect(stderr) })
    }

    /// Register `child` for a cancel to kill, then wait for it to exit or for
    /// `deadline` to pass, returning it reaped.
    fn wait_registered(&self, child: Child, deadline: Option<Instant>) -> io::Result<Child> {
        *self.cancel.child.lock().expect("the cancel slot is poisoned") = Some(child);
        // The handle can drop between the spawn and the registration, in
        // which case `cancel` ran while there was nothing to kill. Killing
        // here does not skip the loop below, since reaping stays there
        // whichever path did the killing.
        if self.cancelled() {
            self.cancel.kill_registered();
        }
        loop {
            let mut slot = self.cancel.child.lock().expect("the cancel slot is poisoned");
            let status = match slot.as_mut() {
                Some(child) => child.try_wait(),
                // Nothing else in this module takes from the slot, so this
                // arm is unreached today; kept so a future caller that does
                // still gets a cancelled result instead of a panic.
                None => return Err(io::Error::new(io::ErrorKind::Interrupted, "job cancelled")),
            };
            match status {
                Ok(Some(_)) => {
                    let mut child = slot.take().expect("observed present on this iteration");
                    drop(slot);
                    // The exit observed above may be the kill landing rather
                    // than the child's own work finishing, in which case the
                    // caller wants "cancelled", not a killed process's status.
                    // `wait` here cannot block. `try_wait` already reaped the
                    // process and `Child` caches the status it collected.
                    if self.cancelled() {
                        let _ = child.wait();
                        return Err(io::Error::new(io::ErrorKind::Interrupted, "job cancelled"));
                    }
                    return Ok(child);
                },
                Ok(None) if deadline.is_some_and(|at| Instant::now() >= at) => {
                    let mut child = slot.take().expect("observed present on this iteration");
                    drop(slot);
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "ran past its limit"));
                },
                Ok(None) => {
                    drop(slot);
                    std::thread::sleep(CHILD_POLL);
                },
                Err(err) => {
                    // Taking the child out removes the only handle a cancel
                    // could have killed it through, and `Child::drop` neither
                    // kills nor reaps, so a live child is stopped and collected
                    // here or never. The slot is released first. A cancel
                    // reaching it now finds nothing to kill, which is right
                    // because this arm kills, and `cancel` runs on the UI
                    // thread where waiting on the lock would stall a frame.
                    let taken = slot.take();
                    drop(slot);
                    if let Some(mut child) = taken {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    return Err(err);
                },
            }
        }
    }
}

/// How much of each stream [`Blocking::run_drained`] keeps. The rest is
/// read and dropped, so the child never blocks on a full pipe.
pub const DRAIN_CAP: u64 = 1 << 20;

fn drain(pipe: impl Read) -> Vec<u8> {
    let mut kept = Vec::new();
    let mut pipe = pipe;
    let _ = (&mut pipe).take(DRAIN_CAP).read_to_end(&mut kept);
    let _ = io::copy(&mut pipe, &mut io::sink());
    kept
}

/// How often `run_cancellable` asks whether its child has exited. The killer
/// and the waiter both need `&mut Child`, so they take turns on the mutex
/// rather than one blocking inside it. Invisible against a fetch that runs
/// for seconds.
const CHILD_POLL: Duration = Duration::from_millis(25);

struct Task {
    cancel: Arc<Cancel>,
    progress: SharedProgress,
    run: Box<dyn FnOnce(&Blocking) + Send>,
    /// Reports a caught panic to the `Job`'s channel. Kept separate from
    /// `run` because `run` moves its sender into a closure that drops it,
    /// unsent, on unwind. This holds a clone that survives that unwind, so
    /// the caller sees an explicit failure instead of a channel that just
    /// went quiet.
    on_failure: Box<dyn FnOnce() + Send>,
}

#[derive(Default)]
struct State {
    interactive: VecDeque<Task>,
    background: VecDeque<Task>,
    interactive_running: usize,
    background_running: usize,
}

/// Which class a running task occupies, so the guard that frees its slot
/// knows which counter to decrement.
#[derive(Clone, Copy)]
enum Slot {
    Interactive,
    Background,
}

/// The next task this worker may run, and the slot it occupies. Each class
/// is capped one below the worker count, so neither can shut the other out:
/// a click never queues behind a pool full of git walks, and a burst of
/// creates never stops a status refresh. Interactive keeps first refusal.
fn take(state: &mut State, workers: usize) -> Option<(Task, Slot)> {
    if state.interactive_running + 1 < workers {
        if let Some(task) = state.interactive.pop_front() {
            state.interactive_running += 1;
            return Some((task, Slot::Interactive));
        }
    }
    if state.background_running + 1 < workers {
        if let Some(task) = state.background.pop_front() {
            state.background_running += 1;
            return Some((task, Slot::Background));
        }
    }
    None
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    workers: usize,
    /// How this pool wakes whatever is watching a job. A callback rather than
    /// an `egui::Context`, so the pool stays free of the UI toolkit. Anything
    /// with no screen, such as the CLI or a test that only wants a result,
    /// leaves it unset, and the wake is a no-op.
    wake_ui: OnceLock<Box<dyn Fn() + Send + Sync>>,
}

impl Shared {
    fn run_waker(&self) {
        if let Some(wake) = self.wake_ui.get() {
            wake();
        }
    }
}

/// A pool of worker threads that run for the life of the process. There is
/// no shutdown path, so a dropped `Pool` leaks its threads, parked forever on
/// the empty queue. Harmless for the process-wide singleton this crate uses;
/// don't construct one you intend to drop.
pub struct Pool {
    shared: Arc<Shared>,
}

/// A submitted job. Dropping the handle cancels the work: an unstarted task
/// is skipped, and a task already blocked in [`Blocking::run_cancellable`]
/// has its child killed, so a status scan for a workspace the user has left
/// stops costing a core either way.
pub struct Job<T> {
    rx: mpsc::Receiver<Result<T, JobFailed>>,
    cancel: Arc<Cancel>,
    progress: SharedProgress,
    /// Latched by `poll` the moment it drains a failure off the channel, so
    /// the signal survives every `poll` after that one too. `poll` itself
    /// can only report it on the one call that observes it, since its `T`
    /// return has no room for "failed".
    failed: Cell<bool>,
}

/// A job's closure unwound instead of returning. Carries no data, because
/// the panic itself is already logged from the pool worker that caught it, so
/// `Job::failed` is only ever asked "did it happen", not "with what".
pub struct JobFailed;

impl<T> Job<T> {
    /// The result if it has landed. Never blocks.
    ///
    /// A job whose closure panicked reports through [`Job::failed`] instead
    /// of a value here, so this returns `None` for it, same as "hasn't landed
    /// yet". Poll every frame and check `failed` after, because the failure
    /// only latches on the `poll` call that drains it off the channel.
    pub fn poll(&self) -> Option<T> {
        match self.rx.try_recv() {
            Ok(Ok(value)) => Some(value),
            Ok(Err(JobFailed)) => {
                self.failed.set(true);
                None
            },
            Err(_) => None,
        }
    }

    /// Whether the job's closure panicked, as observed by a previous `poll`.
    /// `false` for a job still running, and for one that panicked but hasn't
    /// been polled since. Call `poll` first on every frame, then check this.
    pub fn failed(&self) -> bool {
        self.failed.get()
    }

    /// A reader of this job's progress, which stays readable after the job
    /// and this handle are gone.
    pub fn progress_reader(&self) -> ProgressReader {
        ProgressReader(Arc::clone(&self.progress))
    }

    /// A job still running and never finishing, standing in for work a test
    /// build must not start.
    pub fn never() -> Self {
        let (_, rx) = mpsc::channel();
        Job {
            rx,
            cancel: Arc::new(Cancel::default()),
            progress: SharedProgress::default(),
            failed: Cell::new(false),
        }
    }
}

impl<T> Drop for Job<T> {
    fn drop(&mut self) {
        // Before the cancel, so a worker that notices it and returns cannot
        // record its end first.
        settle(&self.progress, JobEnd::Cancelled);
        self.cancel.cancel();
    }
}

/// Handles that already carry their outcome, so a module built on `Job` can
/// test its own logic without a pool and the scheduling that comes with one.
#[cfg(any(test, feature = "test-support"))]
impl<T> Job<T> {
    pub fn ready(value: T) -> Self {
        Self::settled(Ok(value))
    }

    /// Reports the way a worker reports a closure that unwound.
    pub fn panicked() -> Self {
        Self::settled(Err(JobFailed))
    }

    fn settled(result: Result<T, JobFailed>) -> Self {
        let (tx, rx) = mpsc::channel();
        let progress = SharedProgress::default();
        settle(&progress, if result.is_ok() { JobEnd::Returned } else { JobEnd::Panicked });
        // The sender drops here; `try_recv` still hands back what it buffered.
        let _ = tx.send(result);
        Job { rx, cancel: Arc::new(Cancel::default()), progress, failed: Cell::new(false) }
    }
}

impl Pool {
    /// `workers` is clamped to at least two, since each class's ceiling needs
    /// one worker beyond the one it holds free.
    pub fn new(workers: usize) -> Self {
        let workers = workers.max(2);
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            workers,
            wake_ui: OnceLock::new(),
        });
        for _ in 0..workers {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || worker(shared));
        }
        Self { shared }
    }

    /// The most background tasks this pool runs at once. Callers that keep
    /// their own admission count clamp against this rather than inventing a
    /// number that a differently sized pool would make wrong.
    pub fn background_ceiling(&self) -> usize {
        self.shared.workers - 1
    }

    /// Register the wake-up this pool runs after every job. The first
    /// registration wins; later ones are ignored.
    pub fn set_waker(&self, wake: impl Fn() + Send + Sync + 'static) {
        let _ = self.shared.wake_ui.set(Box::new(wake));
    }

    /// Runs the registered wake-up outside any job, for a thread of this
    /// crate's own that learns something the UI has to draw.
    pub fn wake_ui(&self) {
        self.shared.run_waker();
    }

    #[must_use = "dropping the handle cancels the job"]
    pub fn spawn<T, F>(&self, priority: Priority, f: F) -> Job<T>
    where
        F: FnOnce(&Blocking) -> T + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = mpsc::channel();
        let fail_tx = tx.clone();
        let cancel = Arc::new(Cancel::default());
        let progress = SharedProgress::default();
        let returned = Arc::clone(&progress);
        let task = Task {
            cancel: Arc::clone(&cancel),
            progress: Arc::clone(&progress),
            run: Box::new(move |blocking| {
                let value = f(blocking);
                // Before the send: the owner may drop the handle the moment
                // it takes the value, which would otherwise read as cancelled.
                settle(&returned, JobEnd::Returned);
                let _ = tx.send(Ok(value));
            }),
            on_failure: Box::new(move || {
                let _ = fail_tx.send(Err(JobFailed));
            }),
        };
        let mut state = self.shared.state.lock().expect("the job queue is poisoned");
        match priority {
            Priority::Interactive => state.interactive.push_back(task),
            Priority::Background => state.background.push_back(task),
        }
        drop(state);
        self.shared.wake.notify_one();
        Job { rx, cancel, progress, failed: Cell::new(false) }
    }
}

/// Releases a worker's slot on drop, whether the task returned normally or
/// unwound through a panic. A straight-line decrement after the call would
/// never run for a panicking job and would permanently shrink the pool.
struct SlotGuard<'a> {
    shared: &'a Shared,
    slot: Slot,
}

impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().expect("the job queue is poisoned");
        match self.slot {
            Slot::Interactive => state.interactive_running -= 1,
            Slot::Background => state.background_running -= 1,
        }
        drop(state);
        // A freed slot may admit a task another worker is asleep on.
        self.shared.wake.notify_all();
    }
}

/// Wakes the UI when a job ends, whether it returned or unwound. A job that
/// panics never reaches whatever repaint its own closure would have asked for,
/// and [`Job::failed`] is only ever read from a frame. Without this, a modal
/// with nothing else driving repaints sits on its last state until the user
/// happens to move the mouse.
struct WakeOnEnd<'a> {
    shared: &'a Shared,
}

impl Drop for WakeOnEnd<'_> {
    fn drop(&mut self) {
        self.shared.run_waker();
    }
}

fn worker(shared: Arc<Shared>) {
    loop {
        let mut state = shared.state.lock().expect("the job queue is poisoned");
        let (task, slot) = loop {
            if let Some(taken) = take(&mut state, shared.workers) {
                break taken;
            }
            state = shared.wake.wait(state).expect("the job queue is poisoned");
        };
        drop(state);

        let _slot = SlotGuard { shared: &shared, slot };

        if !task.cancel.flag.load(Ordering::Relaxed) {
            lower_this_thread(matches!(slot, Slot::Background));
            let _wake = WakeOnEnd { shared: &shared };
            let blocking = Blocking {
                cancel: Arc::clone(&task.cancel),
                progress: Arc::clone(&task.progress),
                pool: Some(Arc::clone(&shared)),
            };
            let outcome = catch_unwind(AssertUnwindSafe(|| (task.run)(&blocking)));
            if let Err(panic) = outcome {
                log::error!("a job panicked: {}", panic_message(&panic));
                settle(&task.progress, JobEnd::Panicked);
                (task.on_failure)();
            }
        }
    }
}

/// A job's closure and its `Send` payload panicked; the only recovery that
/// keeps the pool alive is to log it here, since `Job::failed` reports only
/// that a panic happened, not what it said.
fn panic_message(panic: &(dyn std::any::Any + Send)) -> &str {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

/// Housekeeping should yield to the UI thread when the CPU is contended, and a
/// worker outlives one job, so the class is set per job rather than at spawn.
#[cfg(windows)]
fn lower_this_thread(background: bool) {
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL, THREAD_PRIORITY_NORMAL,
    };
    let level = if background { THREAD_PRIORITY_BELOW_NORMAL } else { THREAD_PRIORITY_NORMAL };
    unsafe { SetThreadPriority(GetCurrentThread(), level) };
}

#[cfg(not(windows))]
fn lower_this_thread(_background: bool) {}

/// Block on the calling thread, deliberately. The CLI, the IPC connection
/// threads, and construction before the first frame all have nothing on
/// screen waiting on them, so blocking is correct there. A named entry point
/// rather than a public constructor, so the exception is one reviewable call
/// instead of a habit.
pub fn on_this_thread<T>(f: impl FnOnce(&Blocking) -> T) -> T {
    // Nothing holds this `Cancel`, so `run_cancellable` here behaves exactly
    // like a plain run.
    f(&Blocking {
        cancel: Arc::new(Cancel::default()),
        progress: SharedProgress::default(),
        pool: None,
    })
}

/// [`on_this_thread`], returning what `f` reported alongside its value, for
/// a test of a job's progress that needs no pool.
#[cfg(any(test, feature = "test-support"))]
pub fn recorded<T>(f: impl FnOnce(&Blocking) -> T) -> (T, ProgressSnapshot) {
    let progress = SharedProgress::default();
    let blocking = Blocking {
        cancel: Arc::new(Cancel::default()),
        progress: Arc::clone(&progress),
        pool: None,
    };
    let value = f(&blocking);
    settle(&progress, JobEnd::Returned);
    (value, ProgressReader(progress).snapshot())
}

/// The process-wide pool. Sized for IO-bound work, meaning subprocesses and
/// git walks that spend their time waiting instead of saturating a core. The
/// range is therefore chosen for concurrency headroom rather than derived
/// from core count, and `available_parallelism` only seeds where in that
/// range a given machine starts.
pub fn pool() -> &'static Pool {
    static POOL: OnceLock<Pool> = OnceLock::new();
    POOL.get_or_init(|| {
        let workers = std::thread::available_parallelism().map_or(4, |n| n.get().clamp(4, 8));
        Pool::new(workers)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command_ext;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn state_with(interactive: usize, background: usize) -> State {
        let mut state = State::default();
        for _ in 0..interactive {
            state.interactive.push_back(Task {
                cancel: Arc::new(Cancel::default()),
                progress: SharedProgress::default(),
                run: Box::new(|_| {}),
                on_failure: Box::new(|| {}),
            });
        }
        for _ in 0..background {
            state.background.push_back(Task {
                cancel: Arc::new(Cancel::default()),
                progress: SharedProgress::default(),
                run: Box::new(|_| {}),
                on_failure: Box::new(|| {}),
            });
        }
        state
    }

    #[test]
    fn interactive_work_goes_first() {
        let mut state = state_with(1, 1);
        let (_, slot) = take(&mut state, 4).expect("a runnable task");
        assert!(matches!(slot, Slot::Interactive));
        assert_eq!(state.background.len(), 1, "the background task is still queued");
    }

    #[test]
    fn a_worker_stays_free_for_interactive_work() {
        let mut state = state_with(0, 3);
        assert!(take(&mut state, 2).is_some(), "the first background task runs");
        assert!(take(&mut state, 2).is_none(), "the second would leave no worker for a click");
        assert_eq!(state.background.len(), 2);
    }

    #[test]
    fn a_finished_background_task_frees_its_slot() {
        let mut state = state_with(0, 2);
        take(&mut state, 2).expect("the first background task runs");
        state.background_running -= 1;
        assert!(take(&mut state, 2).is_some(), "the freed slot admits the next one");
    }

    #[test]
    fn a_result_reaches_the_handle() {
        let pool = Pool::new(2);
        let job = pool.spawn(Priority::Interactive, |_| 7_u32);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(value) = job.poll() {
                assert_eq!(value, 7);
                assert!(!job.failed(), "a job that reported a value did not fail");
                return;
            }
            assert!(Instant::now() < deadline, "the job never reported");
            std::thread::yield_now();
        }
    }

    fn poll_until<T>(job: &Job<T>, timeout: Duration) -> T {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(value) = job.poll() {
                return value;
            }
            assert!(Instant::now() < deadline, "the job never reported");
            std::thread::yield_now();
        }
    }

    #[test]
    fn a_second_background_job_runs_after_the_first_finishes() {
        let pool = Pool::new(2);
        let first = pool.spawn(Priority::Background, |_| 1_u32);
        assert_eq!(poll_until(&first, Duration::from_secs(5)), 1);
        let second = pool.spawn(Priority::Background, |_| 2_u32);
        assert_eq!(
            poll_until(&second, Duration::from_secs(5)),
            2,
            "the slot the first job held must be free for the second"
        );
    }

    #[test]
    fn a_panicking_background_job_frees_its_slot() {
        let pool = Pool::new(2);
        let panicking = pool.spawn(Priority::Background, |_: &Blocking| -> u32 { panic!("boom") });
        let next = pool.spawn(Priority::Background, |_| 5_u32);
        assert_eq!(
            poll_until(&next, Duration::from_secs(5)),
            5,
            "a panicking job must not permanently occupy its background slot"
        );
        drop(panicking);
    }

    /// A panicked closure must do more than stop resolving. It must report
    /// through `failed` once `poll` has drained the failure off the channel.
    #[test]
    fn a_panicking_jobs_failure_is_observable_through_failed() {
        let pool = Pool::new(2);
        let job = pool.spawn(Priority::Background, |_: &Blocking| -> u32 { panic!("boom") });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !job.failed() {
            assert!(job.poll().is_none(), "a panicking job never reports a value");
            assert!(Instant::now() < deadline, "the failure was never observed");
            std::thread::yield_now();
        }
    }

    #[test]
    fn a_job_still_running_has_not_failed() {
        let pool = Pool::new(1);
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let job = pool.spawn(Priority::Background, move |_| {
            let _ = release_rx.recv();
        });
        assert!(job.poll().is_none(), "the job has not landed yet");
        assert!(!job.failed(), "a job merely still running has not failed");
        let _ = release_tx.send(());
    }

    /// A closure that unwinds never reaches its own repaint, so without the
    /// pool's own wake-up nothing brings the frame that would read
    /// [`Job::failed`]. The wake is only worth anything if the failure is
    /// already on the channel when it fires, which is what the guard's
    /// position in `worker` buys. The second half of the test checks that.
    #[test]
    fn a_panicking_job_wakes_the_ui_with_its_failure_already_readable() {
        let pool = Pool::new(2);
        let (woken_tx, woken_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        // The waker holds its worker until the assertions below have run, so
        // "what the frame sees when the wake arrives" is exactly what the pool
        // had done by then. Nothing the worker does afterwards can drift into
        // the window and make a wrong order look right. `Sender` and
        // `Receiver` are `Send` but not `Sync`, hence the mutex.
        let gate = Mutex::new((woken_tx, release_rx));
        pool.set_waker(move || {
            let gate = gate.lock().expect("the wake gate is poisoned");
            let _ = gate.0.send(());
            let _ = gate.1.recv();
        });
        let job = pool.spawn(Priority::Background, |_: &Blocking| -> u32 { panic!("boom") });

        assert!(
            woken_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "a panicked job must wake the loop that reads its failure"
        );
        assert!(job.poll().is_none(), "a panicking job never reports a value");
        assert!(job.failed(), "the failure must be readable on the frame the wake brings");
        let _ = release_tx.send(());
    }

    #[test]
    fn the_calling_thread_can_take_a_token_explicitly() {
        assert_eq!(on_this_thread(|_| 3_u8), 3);
    }

    /// Interactive work must not be able to take every worker. A pool with all
    /// its workers on interactive jobs cannot refresh a git status, poll worktree
    /// liveness, or look up a PR, and nothing on screen says why.
    #[test]
    fn interactive_work_leaves_a_worker_for_background() {
        let pool = Pool::new(4);
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));

        let mut held = Vec::new();
        for _ in 0..4 {
            let rx = Arc::clone(&release_rx);
            held.push(pool.spawn(Priority::Interactive, move |_| {
                let _ = rx.lock().expect("the release channel is poisoned").recv();
            }));
        }

        let (ran_tx, ran_rx) = mpsc::channel();
        let background = pool.spawn(Priority::Background, move |_| {
            let _ = ran_tx.send(());
        });

        assert!(
            ran_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "the background job never ran: interactive work took every worker"
        );

        for _ in 0..4 {
            let _ = release_tx.send(());
        }
        drop(held);
        drop(background);
    }

    #[test]
    fn dropping_the_handle_cancels_work_that_has_not_started() {
        // `Pool::new` floors the worker count at two, which leaves exactly one
        // background slot. Holding that slot busy keeps the second submission
        // queued until after its handle drops.
        let pool = Pool::new(1);
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let blocker = pool.spawn(Priority::Background, move |_| {
            let _ = release_rx.recv();
        });
        let (ran_tx, ran_rx) = mpsc::channel::<()>();
        drop(pool.spawn(Priority::Background, move |_| {
            let _ = ran_tx.send(());
        }));
        let _ = release_tx.send(());
        drop(blocker);
        assert!(
            ran_rx.recv_timeout(Duration::from_millis(500)).is_err(),
            "a cancelled task must not run"
        );
    }

    /// A command that outlives any test, so only a kill ends it.
    ///
    /// `ping` rather than `timeout` on Windows, because `timeout` refuses to
    /// run when stdin is not a console and exits at once, which would let the
    /// test pass without ever killing anything.
    fn long_sleep() -> Command {
        let mut cmd = if cfg!(windows) {
            let mut c = command_ext::hidden("ping");
            c.args(["-n", "31", "127.0.0.1"]);
            c
        } else {
            let mut c = command_ext::hidden("sleep");
            c.arg("30");
            c
        };
        cmd.stdout(Stdio::null()).stderr(Stdio::piped());
        cmd
    }

    /// Dropping the handle of a job already waiting on a child must kill that
    /// child and free the worker. Checking the flag only before the task starts
    /// leaves a worker parked for as long as the child runs.
    #[test]
    fn dropping_a_running_job_kills_its_child_and_frees_the_worker() {
        let pool = Pool::new(2);
        let (started_tx, started_rx) = mpsc::channel();
        let job = pool.spawn(Priority::Interactive, move |blocking| {
            let _ = started_tx.send(());
            let _ = blocking.run_cancellable(&mut long_sleep());
        });
        started_rx.recv_timeout(Duration::from_secs(5)).expect("the job never started");

        drop(job);

        let (ran_tx, ran_rx) = mpsc::channel();
        let next = pool.spawn(Priority::Interactive, move |_| {
            let _ = ran_tx.send(());
        });
        assert!(
            ran_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "the worker was still parked on the killed child"
        );
        drop(next);
    }

    /// The handle can drop between the spawn and the registration, so `cancel`
    /// finds nothing to kill. Registering must re-check the flag, or that child
    /// runs to completion with nobody left to want it.
    #[test]
    fn a_cancel_racing_registration_still_kills_the_child() {
        let pool = Pool::new(2);
        let (started_tx, started_rx) = mpsc::channel();
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel();
        let job = pool.spawn(Priority::Interactive, move |blocking| {
            // The handshake has to come before the drop. A flag set while the
            // task is still queued is caught by the pre-start check, the task is
            // skipped, `done_tx` drops unsent, and the assertion below reports a
            // disconnect instead of the behaviour under test.
            let _ = started_tx.send(());
            // Hold here until the handle has already been dropped, so the flag is
            // set before any child exists to register.
            let _ = gate_rx.recv();
            let result = blocking.run_cancellable(&mut long_sleep());
            let _ = done_tx.send(result.err().map(|err| err.kind()));
        });
        started_rx.recv_timeout(Duration::from_secs(5)).expect("the job never started");

        drop(job);
        let _ = gate_tx.send(());

        // Collapsing every failure to `true` would also pass if `long_sleep`'s
        // command were missing from PATH, because `spawn` would fail at once
        // with no kill involved. The exact kind distinguishes "cancelled" from
        // "never started".
        assert_eq!(
            done_rx.recv_timeout(Duration::from_secs(5)),
            Ok(Some(io::ErrorKind::Interrupted)),
            "the child outlived a cancel that landed before it was registered"
        );
    }

    /// More output than a pipe buffer holds must not stall the child. A
    /// drained run reads both pipes while it waits.
    #[cfg(unix)]
    #[test]
    fn a_drained_child_that_fills_a_pipe_still_finishes() {
        let mut cmd = command_ext::hidden("sh");
        cmd.args(["-c", "head -c 200000 /dev/zero; head -c 200000 /dev/zero >&2"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = on_this_thread(|b| b.run_drained(&mut cmd, Duration::from_secs(10)))
            .expect("the child finishes");
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 200_000);
        assert_eq!(output.stderr.len(), 200_000);
    }

    fn labels(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// Poll a reader until its job has ended, since a pool job ends on its
    /// own schedule.
    fn end_of(reader: &ProgressReader) -> JobEnd {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(end) = reader.snapshot().end {
                return end;
            }
            assert!(Instant::now() < deadline, "the job never ended");
            std::thread::yield_now();
        }
    }

    #[test]
    fn a_job_reports_its_steps_as_they_end() {
        let pool = Pool::new(2);
        let wakes = Arc::new(Mutex::new(0_usize));
        let counter = Arc::clone(&wakes);
        pool.set_waker(move || *counter.lock().unwrap() += 1);
        let (reported_tx, reported_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let job = pool.spawn(Priority::Interactive, move |blocking| {
            blocking.set_steps(labels(&["a", "b", "c"]));
            blocking.step_done("a", Ok(()));
            blocking.step_done("b", Err("nope".into()));
            let _ = reported_tx.send(());
            let _ = release_rx.recv();
        });
        reported_rx.recv_timeout(Duration::from_secs(5)).expect("the job never reported");

        let snap = job.progress_reader().snapshot();
        assert_eq!(snap.end, None, "the job is still running");
        let got: Vec<_> = snap
            .steps
            .iter()
            .map(|s| (s.label.as_str(), s.outcome.clone(), s.took.is_some()))
            .collect();
        assert_eq!(got, [
            ("a", Some(Ok(())), true),
            ("b", Some(Err("nope".to_string())), true),
            ("c", None, false),
        ]);
        assert_eq!(*wakes.lock().unwrap(), 2, "each step wakes the UI");
        let _ = release_tx.send(());
    }

    #[test]
    fn a_job_that_returned_reads_returned() {
        let pool = Pool::new(2);
        let job = pool.spawn(Priority::Interactive, |_| 1_u8);
        let reader = job.progress_reader();
        assert_eq!(end_of(&reader), JobEnd::Returned);
        assert!(reader.snapshot().steps.is_empty(), "a job that never set steps reports none");
    }

    #[test]
    fn a_panicked_job_reads_panicked() {
        let pool = Pool::new(2);
        let job = pool.spawn(Priority::Interactive, |b: &Blocking| -> u8 {
            b.set_steps(labels(&["a"]));
            panic!("boom")
        });
        assert_eq!(end_of(&job.progress_reader()), JobEnd::Panicked);
    }

    #[test]
    fn dropping_a_running_job_reads_cancelled() {
        let pool = Pool::new(2);
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let job = pool.spawn(Priority::Interactive, move |_| {
            let _ = started_tx.send(());
            let _ = release_rx.recv();
        });
        started_rx.recv_timeout(Duration::from_secs(5)).expect("the job never started");
        let reader = job.progress_reader();
        drop(job);
        let _ = release_tx.send(());
        assert_eq!(reader.snapshot().end, Some(JobEnd::Cancelled));
    }

    /// The owner drops a handle as soon as it has taken the value, which must
    /// not rewrite a job that finished as one that was cancelled.
    #[test]
    fn a_returned_job_dropped_after_it_landed_stays_returned() {
        let pool = Pool::new(2);
        let job = pool.spawn(Priority::Interactive, |_| 2_u8);
        assert_eq!(poll_until(&job, Duration::from_secs(5)), 2);
        let reader = job.progress_reader();
        drop(job);
        assert_eq!(reader.snapshot().end, Some(JobEnd::Returned));
    }

    #[test]
    fn settled_handles_report_their_end() {
        assert_eq!(Job::ready(1_u8).progress_reader().snapshot().end, Some(JobEnd::Returned));
        assert_eq!(Job::<u8>::panicked().progress_reader().snapshot().end, Some(JobEnd::Panicked));
        assert_eq!(Job::<u8>::never().progress_reader().snapshot().end, None);
    }

    #[test]
    fn recorded_returns_what_the_closure_reported() {
        let (value, snap) = recorded(|b| {
            b.set_steps(labels(&["one", "two"]));
            b.step_done("one", Ok(()));
            5_u8
        });
        assert_eq!(value, 5);
        assert_eq!(snap.steps.len(), 2);
        assert_eq!(snap.steps[0].outcome, Some(Ok(())));
        assert_eq!(snap.steps[1].outcome, None);
    }

    #[test]
    fn a_drained_child_past_its_limit_is_killed() {
        let begun = Instant::now();
        let result =
            on_this_thread(|b| b.run_drained(&mut long_sleep(), Duration::from_millis(200)));
        assert_eq!(result.err().map(|e| e.kind()), Some(io::ErrorKind::TimedOut));
        assert!(begun.elapsed() < Duration::from_secs(10), "the child ran to its own end");
    }
}
