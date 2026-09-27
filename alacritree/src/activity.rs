//! What the left sidebar's status row shows: work the user asked for, how far
//! along it is, roughly how long it has left, and how it ended.
//!
//! Only work a keypress, click or palette command started lands here. Polls
//! that run every few seconds would make the row flicker. At most one
//! activity of each kind runs, and a trigger of a kind already running joins
//! it. Times are `Duration`s since an injected origin, as `PrCache` keeps
//! them, so a test can set the clock.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use alacritree_common::jobs::{JobEnd, ProgressReader, Step};
use alacritree_common::wsl;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ActivityKind {
    PrStatus,
    ProjectScan,
}

/// Every label in a PR refresh, one per request its batches made.
struct PrActivity {
    started: Duration,
    readers: Vec<ProgressReader>,
    /// (reader, label) pairs already folded into the timing table.
    folded: HashSet<(usize, String)>,
    /// `PrCache` has dropped its flag, so no batch will join.
    settled: bool,
}

/// Project roots a user-triggered scan waits on. The scans run in parallel on
/// the pool, so they get a count and no estimate.
struct ScanActivity {
    started: Duration,
    roots: Vec<(PathBuf, Option<Result<(), String>>)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Outcome {
    Finished,
    Failed(Failure),
    NothingToCheck,
    Off,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Failure {
    total: usize,
    /// Every failed label with its error, in step order.
    failures: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
struct LastResult {
    kind: ActivityKind,
    at: Duration,
    outcome: Outcome,
}

pub(crate) struct Activities {
    clock: Box<dyn Fn() -> Duration>,
    pr: Option<PrActivity>,
    scan: Option<ScanActivity>,
    /// The most recent successful duration of each PR step label, which is
    /// what the estimate sums.
    timings: HashMap<String, Duration>,
    last: Option<LastResult>,
}

impl Activities {
    pub(crate) fn new() -> Self {
        let origin = Instant::now();
        Self::with_clock(move || origin.elapsed())
    }

    pub(crate) fn with_clock(clock: impl Fn() -> Duration + 'static) -> Self {
        Self { clock: Box::new(clock), pr: None, scan: None, timings: HashMap::new(), last: None }
    }

    pub(crate) fn now(&self) -> Duration {
        (self.clock)()
    }

    /// Start an activity of `kind`, or join the one running.
    pub(crate) fn trigger(&mut self, kind: ActivityKind) {
        let started = self.now();
        match kind {
            ActivityKind::PrStatus => {
                self.pr.get_or_insert_with(|| PrActivity {
                    started,
                    readers: Vec::new(),
                    folded: HashSet::new(),
                    settled: false,
                });
            },
            ActivityKind::ProjectScan => {
                self.scan.get_or_insert_with(|| ScanActivity { started, roots: Vec::new() });
            },
        }
    }

    /// A batch started on the running PR refresh's behalf.
    pub(crate) fn add_pr_reader(&mut self, reader: ProgressReader) {
        if let Some(pr) = &mut self.pr {
            pr.readers.push(reader);
        }
    }

    /// No more batches will join the PR refresh; the next tick ends it.
    pub(crate) fn pr_settled(&mut self) {
        if let Some(pr) = &mut self.pr {
            pr.settled = true;
        }
    }

    /// The refresh was asked for with `[integrations.gh] pr_status` off.
    pub(crate) fn report_off(&mut self) {
        self.last = Some(LastResult {
            kind: ActivityKind::PrStatus,
            at: self.now(),
            outcome: Outcome::Off,
        });
    }

    /// Register a root the running scan waits on, whether its discovery just
    /// started or was already running.
    pub(crate) fn scan_started(&mut self, root: PathBuf) {
        if let Some(scan) = &mut self.scan
            && !scan.roots.iter().any(|(r, _)| *r == root)
        {
            scan.roots.push((root, None));
        }
    }

    pub(crate) fn scan_finished(&mut self, root: &Path, result: Result<(), String>) {
        if let Some(scan) = &mut self.scan
            && let Some((_, slot)) = scan.roots.iter_mut().find(|(r, _)| r == root)
        {
            slot.get_or_insert(result);
        }
    }

    /// The project went away before its scan finished, so it leaves the
    /// total rather than holding the scan open.
    pub(crate) fn scan_left(&mut self, root: &Path) {
        if let Some(scan) = &mut self.scan {
            scan.roots.retain(|(r, outcome)| r != root || outcome.is_some());
        }
    }

    /// Fold newly finished PR steps into the timing table and end whatever
    /// activity has nothing left to wait for. Runs once a frame.
    pub(crate) fn tick(&mut self, now: Duration) {
        if let Some(pr) = &mut self.pr {
            for (i, reader) in pr.readers.iter().enumerate() {
                for step in reader.snapshot().steps {
                    let (Some(outcome), Some(took)) = (&step.outcome, step.took) else { continue };
                    if pr.folded.insert((i, step.label.clone())) && outcome.is_ok() {
                        self.timings.insert(step.label, took);
                    }
                }
            }
        }
        if self.pr.as_ref().is_some_and(|pr| pr.settled) {
            let pr = self.pr.take().expect("checked above");
            self.last = Some(LastResult {
                kind: ActivityKind::PrStatus,
                at: now,
                outcome: pr_outcome(&pr),
            });
        }
        if self.scan.as_ref().is_some_and(|s| s.roots.iter().all(|(_, o)| o.is_some())) {
            let scan = self.scan.take().expect("checked above");
            let failures: Vec<_> = scan
                .roots
                .iter()
                .filter_map(|(root, o)| match o {
                    Some(Err(e)) => Some((wsl::display_path(root), e.clone())),
                    _ => None,
                })
                .collect();
            let outcome = match failures.is_empty() {
                true => Outcome::Finished,
                false => Outcome::Failed(Failure { total: scan.roots.len(), failures }),
            };
            self.last = Some(LastResult { kind: ActivityKind::ProjectScan, at: now, outcome });
        }
    }

    /// Every step the PR refresh's batches have declared, in batch order.
    fn pr_steps(pr: &PrActivity) -> Vec<Step> {
        pr.readers.iter().flat_map(|r| r.snapshot().steps).collect()
    }

    /// The time the pending steps took last time, if any of them has been
    /// timed. Groups in a batch run in sequence, so the sum fits one batch
    /// per refresh and overstates two running at once.
    fn estimate(&self, steps: &[Step]) -> Option<Duration> {
        let known: Vec<_> = steps
            .iter()
            .filter(|s| s.outcome.is_none())
            .filter_map(|s| self.timings.get(&s.label))
            .collect();
        (!known.is_empty()).then(|| known.into_iter().sum())
    }
}

/// How a settled PR refresh ended, read from its batches. A batch that
/// panicked or was cancelled, which `PrCache` does only past the TTL, fails
/// whatever it had not finished.
fn pr_outcome(pr: &PrActivity) -> Outcome {
    let mut steps = Vec::new();
    for reader in &pr.readers {
        let snap = reader.snapshot();
        let unfinished = match snap.end {
            Some(JobEnd::Panicked) => Some("worker panicked"),
            Some(JobEnd::Cancelled) => Some("timed out"),
            _ => None,
        };
        if snap.steps.is_empty() && snap.end == Some(JobEnd::Panicked) {
            steps.push(("PR lookup".to_string(), Err("worker panicked".to_string())));
        }
        for step in snap.steps {
            let outcome = step.outcome.or_else(|| unfinished.map(|e| Err(e.to_string())));
            steps.push((step.label, outcome.unwrap_or(Ok(()))));
        }
    }
    if steps.is_empty() {
        return Outcome::NothingToCheck;
    }
    let total = steps.len();
    let failures: Vec<_> =
        steps.into_iter().filter_map(|(label, o)| o.err().map(|e| (label, e))).collect();
    match failures.is_empty() {
        true => Outcome::Finished,
        false => Outcome::Failed(Failure { total, failures }),
    }
}

/// What the status row paints, worked out without a `Ui` so it can be tested.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct StatusLine {
    /// Something is running, so the row leads with a spinner.
    pub(crate) running: bool,
    pub(crate) text: String,
    /// The text reports a failure.
    pub(crate) failed: bool,
    pub(crate) tooltip: Vec<String>,
    /// When the row next changes on its own, if it does.
    pub(crate) repaint_after: Option<Duration>,
}

/// Running work shows while it runs; otherwise the last result.
pub(crate) fn status_line(a: &Activities, now: Duration) -> StatusLine {
    let mut running: Vec<(Duration, String, String)> = Vec::new();
    if let Some(pr) = &a.pr {
        let steps = Activities::pr_steps(pr);
        let elapsed = now.saturating_sub(pr.started);
        let (text, tip) = if steps.is_empty() {
            let text = format!("PRs · {}", elapsed_text(elapsed));
            (text.clone(), text)
        } else {
            let done = steps.iter().filter(|s| s.outcome.is_some()).count();
            let tail = match a.estimate(&steps) {
                Some(left) => format!("~{}", estimate_text(left)),
                None => elapsed_text(elapsed),
            };
            let listed: Vec<_> = steps
                .iter()
                .map(|s| match s.outcome {
                    Some(_) => format!("{} ✔", s.label),
                    None => s.label.clone(),
                })
                .collect();
            (format!("PRs {done}/{} · {tail}", steps.len()), format!("PRs: {}", listed.join(", ")))
        };
        running.push((pr.started, text, tip));
    }
    if let Some(scan) = &a.scan {
        let done = scan.roots.iter().filter(|(_, o)| o.is_some()).count();
        let text = format!("Scanning projects {done}/{}", scan.roots.len());
        running.push((scan.started, text.clone(), text));
    }
    // Newest first; a tie goes to the scan, pushed last.
    running.sort_by_key(|(started, ..)| std::cmp::Reverse(*started));
    if let Some((_, text, _)) = running.first() {
        let text = match running.len() {
            1 => text.clone(),
            n => format!("{text} +{}", n - 1),
        };
        return StatusLine {
            running: true,
            text,
            failed: false,
            tooltip: running.into_iter().map(|(_, _, tip)| tip).collect(),
            repaint_after: Some(Duration::from_secs(1)),
        };
    }
    let Some(last) = &a.last else { return StatusLine::default() };
    let age = now.saturating_sub(last.at);
    let (what, done) = match last.kind {
        ActivityKind::PrStatus => ("PR refresh", "PRs refreshed"),
        ActivityKind::ProjectScan => ("Project scan", "Projects scanned"),
    };
    match &last.outcome {
        Outcome::Finished => StatusLine {
            text: format!("{done} {}", age_text(age)),
            repaint_after: Some(Duration::from_secs(60)),
            ..Default::default()
        },
        Outcome::Failed(f) => StatusLine {
            text: format!(
                "{what}: {} of {} failed: {}",
                f.failures.len(),
                f.total,
                f.failures[0].1
            ),
            failed: true,
            tooltip: f.failures.iter().map(|(label, e)| format!("{label}: {e}")).collect(),
            ..Default::default()
        },
        Outcome::NothingToCheck => {
            StatusLine { text: "PRs: nothing to check".into(), ..Default::default() }
        },
        Outcome::Off => StatusLine { text: "PR status is off".into(), ..Default::default() },
    }
}

fn elapsed_text(d: Duration) -> String {
    span(d.as_secs())
}

/// Rounded up, so a step with half a second left never reads `~0s`.
fn estimate_text(d: Duration) -> String {
    span(d.as_secs() + u64::from(d.subsec_nanos() > 0))
}

fn span(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s => format!("{}h", s / 3600),
    }
}

/// Minutes at the finest, so the row repaints once a minute at most.
fn age_text(d: Duration) -> String {
    match d.as_secs() {
        s if s < 60 => "just now".to_string(),
        s => format!("{} ago", span(s)),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use alacritree_common::jobs::{JobEnd, ProgressReader};

    use super::*;

    const SEC: Duration = Duration::from_secs(1);

    struct Clock(Arc<Mutex<Duration>>);

    impl Clock {
        fn set(&self, at: Duration) {
            *self.0.lock().unwrap() = at;
        }
    }

    fn activities() -> (Activities, Clock) {
        let now = Arc::new(Mutex::new(Duration::ZERO));
        let reader = Arc::clone(&now);
        (Activities::with_clock(move || *reader.lock().unwrap()), Clock(now))
    }

    fn reader(steps: &[&str]) -> ProgressReader {
        let r = ProgressReader::scripted();
        r.script_steps(steps);
        r
    }

    fn text(a: &Activities, at: Duration) -> String {
        status_line(a, at).text
    }

    /// Run one PR refresh over `steps`, each finishing Ok in its duration.
    fn refresh_once(a: &mut Activities, steps: &[(&str, u64)]) {
        a.trigger(ActivityKind::PrStatus);
        let labels: Vec<_> = steps.iter().map(|(l, _)| *l).collect();
        let r = reader(&labels);
        for (label, secs) in steps {
            r.script_done(label, Ok(()), Duration::from_secs(*secs));
        }
        r.script_end(JobEnd::Returned);
        a.add_pr_reader(r);
        a.pr_settled();
        a.tick(a.now());
    }

    #[test]
    fn nothing_triggered_shows_nothing() {
        let (a, _) = activities();
        let line = status_line(&a, Duration::ZERO);
        assert_eq!(line.text, "");
        assert!(!line.running);
        assert_eq!(line.repaint_after, None);
    }

    #[test]
    fn a_pr_refresh_with_no_steps_yet_shows_elapsed_time() {
        let (mut a, clock) = activities();
        a.trigger(ActivityKind::PrStatus);
        clock.set(SEC);
        a.tick(SEC);
        let line = status_line(&a, SEC);
        assert_eq!(line.text, "PRs · 1s");
        assert!(line.running);
        assert_eq!(line.repaint_after, Some(SEC));
    }

    #[test]
    fn with_no_timings_the_count_shows_elapsed_time() {
        let (mut a, _) = activities();
        a.trigger(ActivityKind::PrStatus);
        a.add_pr_reader(reader(&["o/a", "o/b"]));
        a.tick(SEC);
        assert_eq!(text(&a, SEC), "PRs 0/2 · 1s");
    }

    #[test]
    fn the_estimate_sums_the_pending_steps_last_durations() {
        let (mut a, _) = activities();
        refresh_once(&mut a, &[("o/a", 2), ("o/b", 3), ("o/c", 4)]);

        a.trigger(ActivityKind::PrStatus);
        let r = reader(&["o/a", "o/b", "o/c", "o/new"]);
        r.script_done("o/a", Ok(()), 2 * SEC);
        a.add_pr_reader(r);
        a.tick(SEC);
        assert_eq!(text(&a, SEC), "PRs 1/4 · ~7s");
    }

    #[test]
    fn a_second_trigger_joins_the_running_refresh() {
        let (mut a, _) = activities();
        a.trigger(ActivityKind::PrStatus);
        a.add_pr_reader(reader(&["o/a"]));
        a.trigger(ActivityKind::PrStatus);
        a.add_pr_reader(reader(&["o/b"]));
        a.tick(SEC);
        assert_eq!(text(&a, SEC), "PRs 0/2 · 1s", "one activity counting both batches");
    }

    #[test]
    fn a_finished_refresh_says_when() {
        let (mut a, _) = activities();
        refresh_once(&mut a, &[("o/a", 1)]);
        let line = status_line(&a, 2 * 60 * SEC);
        assert_eq!(line.text, "PRs refreshed 2m ago");
        assert!(!line.running && !line.failed);
        assert_eq!(line.repaint_after, Some(60 * SEC));
        assert_eq!(text(&a, 10 * SEC), "PRs refreshed just now");
    }

    #[test]
    fn a_failure_counts_failed_steps_and_carries_the_first_error() {
        let (mut a, _) = activities();
        a.trigger(ActivityKind::PrStatus);
        let r = reader(&["o/a", "o/b", "o/c"]);
        r.script_done("o/a", Ok(()), SEC);
        r.script_done("o/b", Err("gh failed".into()), SEC);
        r.script_done("o/c", Err("later".into()), SEC);
        r.script_end(JobEnd::Returned);
        a.add_pr_reader(r);
        a.pr_settled();
        a.tick(SEC);
        let line = status_line(&a, SEC);
        assert_eq!(line.text, "PR refresh: 2 of 3 failed: gh failed");
        assert!(line.failed);
        assert_eq!(line.repaint_after, None);
        assert_eq!(line.tooltip, ["o/b: gh failed", "o/c: later"]);
    }

    #[test]
    fn a_failed_step_keeps_the_previous_duration() {
        let (mut a, _) = activities();
        refresh_once(&mut a, &[("o/a", 5)]);
        a.trigger(ActivityKind::PrStatus);
        let r = reader(&["o/a"]);
        r.script_done("o/a", Err("x".into()), 30 * SEC);
        r.script_end(JobEnd::Returned);
        a.add_pr_reader(r);
        a.pr_settled();
        a.tick(SEC);

        a.trigger(ActivityKind::PrStatus);
        a.add_pr_reader(reader(&["o/a"]));
        a.tick(SEC);
        assert_eq!(text(&a, SEC), "PRs 0/1 · ~5s");
    }

    #[test]
    fn a_newer_result_replaces_the_last_one() {
        let (mut a, clock) = activities();
        a.report_off();
        assert_eq!(text(&a, SEC), "PR status is off");
        refresh_once(&mut a, &[("o/a", 1)]);
        assert_eq!(text(&a, SEC), "PRs refreshed just now");
        a.trigger(ActivityKind::ProjectScan);
        a.tick(SEC);
        assert_eq!(text(&a, SEC), "Projects scanned just now");
    }

    #[test]
    fn a_panicked_lookup_fails_its_pending_steps() {
        let (mut a, _) = activities();
        a.trigger(ActivityKind::PrStatus);
        let r = reader(&["o/a", "o/b"]);
        r.script_done("o/a", Ok(()), SEC);
        r.script_end(JobEnd::Panicked);
        a.add_pr_reader(r);
        a.pr_settled();
        a.tick(SEC);
        assert_eq!(text(&a, SEC), "PR refresh: 1 of 2 failed: worker panicked");
    }

    #[test]
    fn a_lookup_that_panicked_before_naming_steps_is_one_failure() {
        let (mut a, _) = activities();
        a.trigger(ActivityKind::PrStatus);
        let r = ProgressReader::scripted();
        r.script_end(JobEnd::Panicked);
        a.add_pr_reader(r);
        a.pr_settled();
        a.tick(SEC);
        let line = status_line(&a, SEC);
        assert_eq!(line.text, "PR refresh: 1 of 1 failed: worker panicked");
        assert_eq!(line.tooltip, ["PR lookup: worker panicked"]);
    }

    #[test]
    fn a_cancelled_lookup_times_out_its_pending_steps() {
        let (mut a, _) = activities();
        a.trigger(ActivityKind::PrStatus);
        let r = reader(&["o/a"]);
        r.script_end(JobEnd::Cancelled);
        a.add_pr_reader(r);
        a.pr_settled();
        a.tick(SEC);
        assert_eq!(text(&a, SEC), "PR refresh: 1 of 1 failed: timed out");
    }

    #[test]
    fn a_refresh_with_nothing_to_look_up_says_so() {
        let (mut a, _) = activities();
        a.trigger(ActivityKind::PrStatus);
        a.pr_settled();
        a.tick(SEC);
        assert_eq!(text(&a, SEC), "PRs: nothing to check");
    }

    #[test]
    fn a_scan_counts_its_projects_off() {
        let (mut a, _) = activities();
        a.trigger(ActivityKind::ProjectScan);
        a.scan_started(PathBuf::from("/a"));
        a.scan_started(PathBuf::from("/b"));
        a.scan_finished(Path::new("/a"), Ok(()));
        a.tick(SEC);
        let line = status_line(&a, SEC);
        assert_eq!(line.text, "Scanning projects 1/2");
        assert!(line.running);
        a.scan_finished(Path::new("/b"), Err("the project refresh worker panicked".into()));
        a.tick(SEC);
        assert_eq!(
            text(&a, SEC),
            "Project scan: 1 of 2 failed: the project refresh worker panicked"
        );
    }

    /// A scan already under way when the user asks is the one that answers
    /// them, so a second trigger registering the same root joins it.
    #[test]
    fn a_root_already_scanning_counts_toward_the_scan() {
        let (mut a, _) = activities();
        a.trigger(ActivityKind::ProjectScan);
        a.scan_started(PathBuf::from("/a"));
        a.trigger(ActivityKind::ProjectScan);
        a.scan_started(PathBuf::from("/a"));
        a.tick(SEC);
        assert_eq!(text(&a, SEC), "Scanning projects 0/1");
        a.scan_finished(Path::new("/a"), Ok(()));
        a.tick(SEC);
        assert_eq!(text(&a, SEC), "Projects scanned just now");
    }

    #[test]
    fn a_project_removed_mid_scan_leaves_the_total() {
        let (mut a, _) = activities();
        a.trigger(ActivityKind::ProjectScan);
        a.scan_started(PathBuf::from("/a"));
        a.scan_started(PathBuf::from("/b"));
        a.scan_finished(Path::new("/a"), Ok(()));
        a.scan_left(Path::new("/b"));
        a.tick(SEC);
        assert_eq!(text(&a, SEC), "Projects scanned just now");
    }

    #[test]
    fn a_result_nobody_asked_for_changes_nothing() {
        let (mut a, _) = activities();
        a.scan_finished(Path::new("/a"), Ok(()));
        a.tick(SEC);
        assert_eq!(text(&a, SEC), "");
    }

    #[test]
    fn two_running_activities_show_the_newer_and_a_count() {
        let (mut a, clock) = activities();
        a.trigger(ActivityKind::PrStatus);
        clock.set(SEC);
        a.trigger(ActivityKind::ProjectScan);
        a.scan_started(PathBuf::from("/a"));
        a.tick(SEC);
        let line = status_line(&a, SEC);
        assert_eq!(line.text, "Scanning projects 0/1 +1");
        assert_eq!(line.tooltip, ["Scanning projects 0/1", "PRs · 1s"]);
    }

    #[test]
    fn the_tooltip_ticks_off_finished_steps() {
        let (mut a, _) = activities();
        a.trigger(ActivityKind::PrStatus);
        let r = reader(&["o/a", "o/b"]);
        r.script_done("o/a", Ok(()), SEC);
        a.add_pr_reader(r);
        a.tick(SEC);
        assert_eq!(status_line(&a, SEC).tooltip, ["PRs: o/a ✔, o/b"]);
    }
}
