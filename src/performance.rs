//! Task timing stays on the UI thread. Workers publish one atomic completion;
//! there is no sampler thread, resource polling, or timer while the UI is idle.
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

use crate::background::{CancellationToken, UiWake};

pub(crate) const ACTIVITY_DELAY: Duration = Duration::from_millis(400);
const UPDATE_INTERVAL: Duration = Duration::from_secs(1);
pub(crate) const HISTORY_LIMIT: usize = 32;
pub(crate) const FRAME_HISTORY: usize = 120;
const INTERRUPTED: u64 = 1 << 63;

#[path = "performance_diagnostics.rs"]
pub(crate) mod diagnostics;

#[derive(Default)]
struct ActivityProgress {
    completion: AtomicU64,
    // Microseconds from submission to execution, plus one; zero means queued.
    running_after: AtomicU64,
}

pub(crate) struct Activity {
    pub id: u64,
    pub label: &'static str,
    pub path: Option<PathBuf>,
    pub started: Instant,
    progress: Arc<ActivityProgress>,
}

impl Activity {
    pub(crate) fn queue_time(&self) -> Option<Duration> {
        self.progress
            .running_after
            .load(Ordering::Acquire)
            .checked_sub(1)
            .map(Duration::from_micros)
    }
}

#[cfg(test)]
impl Activity {
    /// Publish the same completion as a worker without sleeping in UI tests.
    pub(crate) fn finish_for_test(&self, elapsed: Duration) {
        self.progress.running_after.store(1, Ordering::Release);
        self.progress
            .completion
            .store(elapsed.as_micros() as u64 + 1, Ordering::Release);
    }
}

pub(crate) struct CompletedActivity {
    pub id: u64,
    pub label: &'static str,
    pub path: Option<PathBuf>,
    pub elapsed: Duration,
    pub interrupted: bool,
    pub queue_time: Option<Duration>,
    finished: Instant,
}

/// Dropping the ticket handles early returns, cancellation, and unwinding too.
pub(crate) struct ActivityGuard {
    started: Instant,
    progress: Arc<ActivityProgress>,
    wake: UiWake,
    cancel: Option<CancellationToken>,
}

impl ActivityGuard {
    /// Mark actual worker entry, separately from time waiting for a worker.
    pub(crate) fn running(self) -> Self {
        let delay = self
            .started
            .elapsed()
            .as_micros()
            .min(u128::from(INTERRUPTED - 2)) as u64;
        self.progress
            .running_after
            .store(delay + 1, Ordering::Release);
        self
    }

    pub(crate) fn cancellable(mut self, cancel: CancellationToken) -> Self {
        self.cancel = Some(cancel);
        self
    }
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        let elapsed = self
            .started
            .elapsed()
            .as_micros()
            .min(u128::from(INTERRUPTED - 2)) as u64;
        let interrupted = std::thread::panicking()
            || self.progress.running_after.load(Ordering::Relaxed) == 0
            || self
                .cancel
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled);
        self.progress.completion.store(
            (elapsed + 1) | if interrupted { INTERRUPTED } else { 0 },
            Ordering::Release,
        );
        // Ordinary short tasks already wake the UI through their result and
        // the existing worker count. Only a potentially visible task needs a
        // separate completion wake (including cancellation without a result).
        if elapsed >= ACTIVITY_DELAY.as_micros() as u64 {
            self.wake.notify();
        }
    }
}

pub(crate) struct RenderTiming {
    pub last_ms: f64,
    pub average_ms: f64,
    pub peak_ms: f64,
    samples: u64,
    frames: [f32; FRAME_HISTORY],
    next_frame: usize,
}

impl Default for RenderTiming {
    fn default() -> Self {
        Self {
            last_ms: 0.0,
            average_ms: 0.0,
            peak_ms: 0.0,
            samples: 0,
            frames: [0.0; FRAME_HISTORY],
            next_frame: 0,
        }
    }
}

impl RenderTiming {
    pub(crate) fn record(&mut self, elapsed: Duration) {
        let ms = elapsed.as_secs_f64() * 1_000.0;
        self.last_ms = ms;
        self.average_ms = if self.samples == 0 {
            ms
        } else {
            self.average_ms * 0.9 + ms * 0.1
        };
        self.peak_ms = self.peak_ms.max(ms);
        self.frames[self.next_frame] = ms as f32;
        self.next_frame = (self.next_frame + 1) % FRAME_HISTORY;
        self.samples = self.samples.saturating_add(1);
    }

    pub(crate) fn has_samples(&self) -> bool {
        self.samples > 0
    }

    pub(crate) fn sample_count(&self) -> u64 {
        self.samples
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Recent tail latency and slow-frame count, computed only for the visible
    /// monitor/report. Recording a frame remains O(1) and allocation-free.
    pub(crate) fn recent_summary(&self) -> (f32, usize, usize) {
        let count = self.samples.min(FRAME_HISTORY as u64) as usize;
        if count == 0 {
            return (0.0, 0, 0);
        }
        let mut frames = self.frames;
        let recent = &mut frames[..count];
        let slow = recent.iter().filter(|&&ms| ms > 16.7).count();
        let nth = (count * 95).div_ceil(100) - 1;
        let (_, p95, _) = recent.select_nth_unstable_by(nth, f32::total_cmp);
        (*p95, slow, count)
    }

    /// A fixed ring keeps frame history allocation-free and independent of FPS.
    pub(crate) fn recent_ms(&self) -> impl Iterator<Item = f32> + '_ {
        let count = self.samples.min(FRAME_HISTORY as u64) as usize;
        let first = if count == FRAME_HISTORY {
            self.next_frame
        } else {
            0
        };
        (0..count).map(move |index| self.frames[(first + index) % FRAME_HISTORY])
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct DisplayKey {
    primary: Option<u64>,
    seconds: u64,
    jobs: usize,
    history_revision: u64,
}

pub(crate) struct ActivityMonitor {
    pub active: Vec<Activity>,
    pub recent: VecDeque<CompletedActivity>,
    pub render: RenderTiming,
    pub ui: diagnostics::UiTimings,
    pub totals: diagnostics::TaskTotals,
    pub details_open: bool,
    wake: UiWake,
    next_id: u64,
    jobs: usize,
    fallback_started: Option<Instant>,
    last_draw: Instant,
    displayed: DisplayKey,
    history_revision: u64,
}

impl ActivityMonitor {
    pub(crate) fn new(wake: UiWake) -> Self {
        Self {
            active: Vec::new(),
            recent: VecDeque::with_capacity(HISTORY_LIMIT),
            render: RenderTiming::default(),
            ui: diagnostics::UiTimings::default(),
            totals: diagnostics::TaskTotals::default(),
            details_open: false,
            wake,
            next_id: 1,
            jobs: 0,
            fallback_started: None,
            last_draw: Instant::now(),
            displayed: DisplayKey::default(),
            history_revision: 0,
        }
    }

    /// Track submission. The worker must call `running()` when it begins work.
    pub(crate) fn start(&mut self, label: &'static str, path: Option<&Path>) -> ActivityGuard {
        let started = Instant::now();
        let progress = Arc::new(ActivityProgress::default());
        self.active.push(Activity {
            id: self.next_id,
            label,
            path: path.map(Path::to_path_buf),
            started,
            progress: Arc::clone(&progress),
        });
        self.next_id = self.next_id.wrapping_add(1).max(1);
        ActivityGuard {
            started,
            progress,
            wake: self.wake.clone(),
            cancel: None,
        }
    }

    /// Called only on existing workbench wakes. No file contents are inspected.
    pub(crate) fn refresh(&mut self, now: Instant, worker_count: usize) {
        let mut index = 0;
        while index < self.active.len() {
            let result = self.active[index]
                .progress
                .completion
                .load(Ordering::Acquire);
            if result == 0 {
                index += 1;
                continue;
            }
            let activity = self.active.swap_remove(index);
            let elapsed = Duration::from_micros((result & !INTERRUPTED) - 1);
            let finished = activity.started + elapsed;
            let queue_time = activity
                .queue_time()
                .map(|wait| wait.min(elapsed))
                .or_else(|| (result & INTERRUPTED != 0).then_some(elapsed));
            // A completion can wait for the UI across Reset. Attribute it by
            // its worker timestamp, not by the later time it was collected.
            if finished >= self.ui.since {
                self.totals.record(
                    activity.label,
                    elapsed,
                    queue_time,
                    result & INTERRUPTED != 0,
                );
            }
            if elapsed >= ACTIVITY_DELAY {
                self.history_revision = self.history_revision.wrapping_add(1);
                let completed = CompletedActivity {
                    id: activity.id,
                    label: activity.label,
                    path: activity.path,
                    elapsed,
                    interrupted: result & INTERRUPTED != 0,
                    queue_time,
                    finished,
                };
                // Several workers may finish between frames; keep completion order.
                let position = self
                    .recent
                    .iter()
                    .position(|other| other.finished < completed.finished)
                    .unwrap_or(self.recent.len());
                self.recent.insert(position, completed);
                self.recent.truncate(HISTORY_LIMIT);
            }
        }
        self.jobs = worker_count.max(self.active.len());
        if self.jobs == 0 {
            self.fallback_started = None;
        } else {
            self.fallback_started.get_or_insert(now);
        }
    }

    pub(crate) fn job_count(&self) -> usize {
        self.jobs
    }

    /// Clearing diagnostic history never cancels or modifies background work.
    pub(crate) fn remove_recent(&mut self, id: Option<u64>) -> bool {
        let before = self.recent.len();
        if let Some(id) = id {
            self.recent.retain(|task| task.id != id);
        } else {
            self.recent.clear();
        }
        if self.recent.len() == before {
            return false;
        }
        self.history_revision = self.history_revision.wrapping_add(1);
        true
    }

    pub(crate) fn primary(&self, now: Instant) -> Option<&Activity> {
        self.active
            .iter()
            .filter(|task| now.saturating_duration_since(task.started) >= ACTIVITY_DELAY)
            .min_by_key(|task| (task.path.is_none(), task.started))
    }

    pub(crate) fn busy_since(&self, now: Instant) -> Option<Instant> {
        self.primary(now).map(|task| task.started).or_else(|| {
            self.fallback_started
                .filter(|start| now.saturating_duration_since(*start) >= ACTIVITY_DELAY)
        })
    }

    fn display_key(&self, now: Instant) -> DisplayKey {
        let since = self.busy_since(now);
        DisplayKey {
            primary: since.map(|_| self.primary(now).map_or(0, |task| task.id)),
            seconds: since.map_or(0, |start| now.saturating_duration_since(start).as_secs()),
            jobs: if since.is_some() || self.details_open {
                self.jobs
            } else {
                0
            },
            history_revision: if self.details_open {
                self.history_revision
            } else {
                0
            },
        }
    }

    pub(crate) fn needs_redraw(&self, now: Instant) -> bool {
        self.display_key(now) != self.displayed
            || (self.details_open
                && self.jobs > 0
                && now.saturating_duration_since(self.last_draw) >= UPDATE_INTERVAL)
    }

    pub(crate) fn mark_drawn(&mut self, now: Instant) {
        self.displayed = self.display_key(now);
        self.last_draw = now;
    }

    pub(crate) fn next_wait(&self, now: Instant) -> Option<Duration> {
        if self.needs_redraw(now) {
            return Some(Duration::ZERO);
        }
        let busy = self.busy_since(now).is_some();
        let mut next = if busy || (self.details_open && self.jobs > 0) {
            Some(UPDATE_INTERVAL.saturating_sub(now.saturating_duration_since(self.last_draw)))
        } else {
            None
        };
        // An arriving file task may replace a generic workspace task after the
        // grace period. Other running tasks do not get individual tick timers.
        let primary = self.primary(now);
        let pending = self
            .active
            .iter()
            .filter(|task| {
                primary.is_none_or(|current| current.path.is_none() && task.path.is_some())
            })
            .filter_map(|task| {
                ACTIVITY_DELAY.checked_sub(now.saturating_duration_since(task.started))
            })
            .min();
        if let Some(wait) = pending {
            next = Some(next.map_or(wait, |current| current.min(wait)));
        }
        if !busy && let Some(start) = self.fallback_started {
            let wait = ACTIVITY_DELAY.saturating_sub(now.saturating_duration_since(start));
            next = Some(next.map_or(wait, |current| current.min(wait)));
        }
        next
    }
}

pub(crate) fn format_elapsed(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds >= 60 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else if seconds > 0 {
        format!("{seconds}s")
    } else {
        format!("{}ms", elapsed.as_millis())
    }
}

pub(crate) fn worker_label(name: &'static str) -> &'static str {
    match name {
        "file-load" => "Opening",
        "file-reload" => "Reloading",
        "session-file-load" => "Restoring",
        "file-check" => "Checking file",
        "bg-save" => "Saving",
        "explorer-refresh" => "Refreshing explorer",
        "git-status" | "git-branch" => "Refreshing Git",
        "git-line-diff" | "git-compare" | "source-control-diffs" | "diff-refresh" => {
            "Comparing files"
        }
        "git-fetch" => "Fetching Git",
        "git-pull" => "Pulling Git",
        "git-push" => "Pushing Git",
        "workspace-replace" => "Replacing in workspace",
        "global-search" => "Searching workspace",
        "quick-open" => "Finding files",
        "git-stage" | "git-stage-all" => "Staging changes",
        "merge-conflicts" => "Checking merge conflicts",
        "merge-compare" => "Comparing merge conflict",
        "git-unstage" | "git-unstage-all" => "Unstaging changes",
        "git-commit" | "git-stage-and-commit" | "git-amend" => "Committing changes",
        "git-checkout" | "git-checkout-remote" | "git-create-branch" => "Switching branch",
        "git-branches" => "Loading branches",
        "git-stash" | "git-stash-pop" => "Updating stash",
        "ext-command" => "Running extension",
        "ext-tick" => "Updating extension",
        "external-open" => "Opening externally",
        "breadcrumb-list" => "Listing folder",
        "lsp-shutdown" => "Stopping language servers",
        "extension-manager-drop" => "Stopping extensions",
        "blame-history" | "blame-file" | "blame-cache" => "Loading file history",
        "chat-checkpoint" => "Saving chat checkpoint",
        _ => "Background work",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor() -> ActivityMonitor {
        ActivityMonitor::new(UiWake::default())
    }

    #[test]
    fn performance_short_tasks_never_flash_or_keep_an_idle_timer() {
        let mut monitor = monitor();
        let ticket = monitor.start("Opening", Some(Path::new("small.rs")));
        let now = ticket.started;
        monitor.refresh(now, 1);
        assert!(monitor.busy_since(now).is_none());
        assert!(!monitor.needs_redraw(now));
        assert_eq!(monitor.next_wait(now), Some(ACTIVITY_DELAY));
        drop(ticket);
        monitor.refresh(Instant::now(), 0);
        assert!(monitor.active.is_empty());
        assert!(monitor.recent.is_empty());
        assert_eq!(monitor.next_wait(Instant::now()), None);
        monitor.details_open = true;
        monitor.mark_drawn(Instant::now());
        assert_eq!(monitor.next_wait(Instant::now()), None);
    }

    #[test]
    fn performance_queue_time_is_distinct_from_execution_and_fast_tasks_are_counted() {
        let mut monitor = monitor();
        let ticket = monitor.start("Analysis", None);
        assert_eq!(monitor.active[0].queue_time(), None);
        let ticket = ticket.running();
        assert!(monitor.active[0].queue_time().is_some());
        ticket
            .progress
            .running_after
            .store(200_001, Ordering::Release);
        ticket
            .progress
            .completion
            .store(1_000_001, Ordering::Release);
        monitor.refresh(ticket.started + Duration::from_secs(1), 0);
        assert_eq!(
            monitor.recent[0].queue_time,
            Some(Duration::from_millis(200))
        );
        assert_eq!(monitor.totals.rows[0].queued, Duration::from_millis(200));
        assert_eq!(monitor.totals.rows[0].elapsed, Duration::from_secs(1));
        let fast = monitor.start("Analysis", None);
        monitor.active[0].finish_for_test(Duration::from_millis(2));
        monitor.refresh(Instant::now(), 0);
        assert_eq!(monitor.recent.len(), 1);
        assert_eq!(monitor.totals.completed, 2);
        assert_eq!(monitor.totals.rows[0].elapsed, Duration::from_millis(1002));
        assert_eq!(monitor.totals.rows[0].queue_samples, 2);
        drop(fast);
        let never_started = monitor.start("Discarded", None);
        drop(never_started);
        monitor.refresh(Instant::now(), 0);
        let discarded = monitor
            .totals
            .rows
            .iter()
            .find(|row| row.label == "Discarded")
            .unwrap();
        assert_eq!(discarded.stopped, 1);
        assert_eq!(discarded.queued, discarded.elapsed);
    }

    #[test]
    fn performance_reset_excludes_late_collected_completions_from_the_previous_period() {
        let mut monitor = monitor();
        let now = Instant::now();
        let _before = monitor.start("Before reset", None);
        let _after = monitor.start("After reset", None);
        for task in &mut monitor.active {
            task.started = now;
        }
        monitor.active[0].finish_for_test(Duration::from_millis(500));
        monitor.active[1].finish_for_test(Duration::from_millis(1500));
        monitor.ui.reset(now + Duration::from_secs(1));
        monitor.totals.reset();
        monitor.refresh(now + Duration::from_secs(2), 0);
        assert_eq!(monitor.totals.completed, 1);
        assert_eq!(monitor.totals.rows[0].label, "After reset");
        assert_eq!(monitor.totals.rows[0].elapsed, Duration::from_millis(1500));
        assert_eq!(monitor.recent.len(), 2, "Recent is independent of Reset");
    }

    #[test]
    fn performance_long_work_has_one_tick_and_returns_to_complete_quiescence() {
        let mut monitor = monitor();
        let ticket = monitor.start("Highlighting", Some(Path::new("large.rs")));
        let now = ticket.started + ACTIVITY_DELAY;
        monitor.refresh(now, 3);
        assert!(monitor.needs_redraw(now));
        assert_eq!(
            monitor.primary(now).unwrap().path.as_deref(),
            Some(Path::new("large.rs"))
        );
        assert_eq!(
            now.duration_since(monitor.busy_since(now).unwrap()),
            ACTIVITY_DELAY
        );
        assert_eq!(monitor.job_count(), 3);
        monitor.mark_drawn(now);
        assert_eq!(monitor.next_wait(now), Some(UPDATE_INTERVAL));
        assert_eq!(
            monitor.next_wait(now + Duration::from_millis(100)),
            Some(Duration::from_millis(900))
        );

        // Workers publish elapsed time without requiring the UI to sample them.
        ticket
            .progress
            .completion
            .store(2_000_001, Ordering::Release);
        monitor.refresh(ticket.started + Duration::from_secs(2), 0);
        let finished = ticket.started + Duration::from_secs(2);
        assert!(monitor.busy_since(finished).is_none());
        assert_eq!(
            monitor.recent.front().unwrap().elapsed,
            Duration::from_secs(2)
        );
        monitor.mark_drawn(finished);
        assert_eq!(monitor.next_wait(finished), None);
    }

    #[test]
    fn performance_file_work_takes_priority_and_history_is_bounded_and_ordered() {
        let mut monitor = monitor();
        let _workspace = monitor.start("Refreshing Git", None);
        let _file = monitor.start("Opening", Some(Path::new("important.rs")));
        let now = Instant::now() + Duration::from_secs(2);
        monitor.refresh(now, 2);
        assert_eq!(monitor.primary(now).unwrap().label, "Opening");
        let mut tickets = Vec::new();
        for number in 0..(HISTORY_LIMIT as u64 + 4) {
            tickets.push(monitor.start("Analyzing", None));
            let task = monitor.active.last_mut().unwrap();
            task.started = now - Duration::from_secs(number + 1);
            task.progress.completion.store(1_000_001, Ordering::Release);
        }
        monitor.refresh(now, 2);
        assert_eq!(monitor.recent.len(), HISTORY_LIMIT);
        assert!(
            monitor
                .recent
                .iter()
                .zip(monitor.recent.iter().skip(1))
                .all(|(newer, older)| newer.finished >= older.finished)
        );
    }

    #[test]
    fn performance_cancelled_and_panicking_tasks_release_their_activity() {
        for panic in [false, true] {
            let mut monitor = monitor();
            let mut ticket = monitor.start("Highlighting", None);
            ticket.started -= Duration::from_secs(1);
            monitor.active[0].started = ticket.started;
            let wake = monitor.wake.generation();
            if panic {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _ticket = ticket;
                    panic!("test worker failure");
                }));
                assert!(result.is_err());
            } else {
                let cancel = CancellationToken::new();
                cancel.cancel();
                drop(ticket.cancellable(cancel));
            }
            assert_ne!(monitor.wake.generation(), wake);
            monitor.refresh(Instant::now(), 0);
            assert!(monitor.active.is_empty());
            assert!(monitor.recent.front().unwrap().interrupted);
        }
    }

    #[test]
    fn performance_unlabelled_pool_work_uses_the_existing_job_count() {
        let mut monitor = monitor();
        let now = Instant::now();
        monitor.refresh(now, 4);
        let later = now + Duration::from_secs(1);
        assert!(monitor.primary(later).is_none());
        assert_eq!(monitor.job_count(), 4);
        assert_eq!(
            later.duration_since(monitor.busy_since(later).unwrap()),
            Duration::from_secs(1)
        );
        monitor.refresh(later, 0);
        monitor.mark_drawn(later);
        assert_eq!(monitor.next_wait(later), None);
    }

    #[test]
    fn performance_frame_history_keeps_the_latest_samples_in_order() {
        let mut timing = RenderTiming::default();
        assert_eq!(timing.recent_ms().count(), 0);
        for ms in 1..=124 {
            timing.record(Duration::from_millis(ms));
        }
        assert_eq!(
            timing.recent_ms().collect::<Vec<_>>(),
            (5..=124).map(|ms| ms as f32).collect::<Vec<_>>()
        );
        assert_eq!(timing.last_ms, 124.0);
        assert_eq!(timing.peak_ms, 124.0);
        assert!(timing.average_ms < timing.last_ms);
        assert_eq!(timing.recent_summary(), (118.0, 108, 120));
        timing.reset();
        assert_eq!(timing.recent_summary(), (0.0, 0, 0));
    }

    #[test]
    fn performance_history_edits_and_render_resets_never_leave_an_idle_deadline() {
        let mut monitor = monitor();
        let _ticket = monitor.start("Opening", Some(Path::new("large.rs")));
        let id = monitor.active[0].id;
        monitor.active[0].started -= Duration::from_secs(2);
        monitor.active[0].finish_for_test(Duration::from_secs(2));
        monitor.refresh(Instant::now(), 0);
        assert_eq!(monitor.recent[0].id, id);
        monitor.details_open = true;
        monitor.mark_drawn(Instant::now());
        assert_eq!(monitor.next_wait(Instant::now()), None);
        monitor.render.record(Duration::from_secs(1));
        monitor.render.reset();
        assert_eq!(monitor.render.sample_count(), 0);
        assert_eq!(monitor.render.recent_ms().count(), 0);
        assert_eq!(monitor.next_wait(Instant::now()), None);
        assert!(monitor.remove_recent(Some(id)));
        assert!(monitor.needs_redraw(Instant::now()));
        monitor.mark_drawn(Instant::now());
        assert_eq!(monitor.next_wait(Instant::now()), None);
        assert!(!monitor.remove_recent(None));
        assert_eq!(monitor.next_wait(Instant::now()), None);
    }

    #[test]
    #[ignore = "manual bookkeeping benchmark; run with --ignored --nocapture"]
    fn performance_bookkeeping_benchmark() {
        let mut monitor = monitor();
        let path = Path::new("workspace/src/large_document.rs");
        let iterations = 100_000;
        let started = Instant::now();
        for _ in 0..iterations {
            let ticket = monitor.start("Highlighting", Some(std::hint::black_box(path)));
            drop(ticket);
            monitor.refresh(Instant::now(), 0);
        }
        let per_task = started.elapsed().as_nanos() as f64 / f64::from(iterations);
        eprintln!("Activity start + completion + reconciliation: {per_task:.0} ns/task");
        assert!(monitor.active.is_empty());
        assert_eq!(monitor.next_wait(Instant::now()), None);
        let started = Instant::now();
        for _ in 0..iterations {
            monitor
                .render
                .record(std::hint::black_box(Duration::from_micros(2_400)));
        }
        let per_frame = started.elapsed().as_nanos() as f64 / f64::from(iterations);
        eprintln!("Frame timing + fixed history: {per_frame:.0} ns/frame");
    }
}
