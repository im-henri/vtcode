//! Bounded diagnostics recorded at existing work boundaries, never by polling.
use super::RenderTiming;
use std::time::{Duration, Instant};

pub(crate) const SLOW_UI: Duration = Duration::from_millis(50);
const SLOW_HISTORY: usize = 16;
const TASK_KINDS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UiPhase {
    Input,
    LanguageServers,
    Workers,
    Terminals,
    Agents,
    Extensions,
    Preparation,
    Painting,
    TerminalOutput,
    InputToFrame,
}

impl UiPhase {
    pub(crate) const ALL: [Self; 10] = [
        Self::Input,
        Self::LanguageServers,
        Self::Workers,
        Self::Terminals,
        Self::Agents,
        Self::Extensions,
        Self::Preparation,
        Self::Painting,
        Self::TerminalOutput,
        Self::InputToFrame,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Input => "Input handling",
            Self::LanguageServers => "LSP callbacks",
            Self::Workers => "Worker results",
            Self::Terminals => "Terminal output",
            Self::Agents => "Agent callbacks",
            Self::Extensions => "Extension callbacks",
            Self::Preparation => "Frame preparation",
            Self::Painting => "UI painting",
            Self::TerminalOutput => "Terminal diff/output",
            Self::InputToFrame => "Input to draw",
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct SlowUi {
    pub label: &'static str,
    pub elapsed: Duration,
    pub at: Instant,
}

pub(crate) struct UiTimings {
    series: [RenderTiming; UiPhase::ALL.len()],
    slow: [Option<SlowUi>; SLOW_HISTORY],
    next_slow: usize,
    pub slow_count: u64,
    pub since: Instant,
    pending_input: Option<Instant>,
}

impl Default for UiTimings {
    fn default() -> Self {
        Self {
            series: std::array::from_fn(|_| RenderTiming::default()),
            slow: [None; SLOW_HISTORY],
            next_slow: 0,
            slow_count: 0,
            since: Instant::now(),
            pending_input: None,
        }
    }
}

impl UiTimings {
    pub(crate) fn timing(&self, phase: UiPhase) -> &RenderTiming {
        &self.series[phase as usize]
    }

    pub(crate) fn record(&mut self, phase: UiPhase, elapsed: Duration, at: Instant) {
        self.series[phase as usize].record(elapsed);
        self.record_slow(phase.label(), elapsed, at);
    }

    fn record_slow(&mut self, label: &'static str, elapsed: Duration, at: Instant) {
        if elapsed >= SLOW_UI {
            self.slow[self.next_slow] = Some(SlowUi { label, elapsed, at });
            self.next_slow = (self.next_slow + 1) % SLOW_HISTORY;
            self.slow_count = self.slow_count.saturating_add(1);
        }
    }

    pub(crate) fn slow_events(&self) -> impl Iterator<Item = SlowUi> + '_ {
        let count = self.slow_count.min(SLOW_HISTORY as u64) as usize;
        (0..count).filter_map(move |offset| {
            self.slow[(self.next_slow + SLOW_HISTORY - 1 - offset) % SLOW_HISTORY]
        })
    }

    /// Oldest handled input represented by the next draw, including a batch.
    /// This excludes time in the OS/input queue and the terminal's own paint.
    pub(crate) fn input_handled(&mut self, started: Instant, finished: Instant, redraw: bool) {
        self.record(
            UiPhase::Input,
            finished.saturating_duration_since(started),
            finished,
        );
        if redraw {
            self.pending_input.get_or_insert(started);
        }
    }

    pub(crate) fn presented(&mut self, now: Instant) {
        if let Some(started) = self.pending_input.take() {
            self.record(
                UiPhase::InputToFrame,
                now.saturating_duration_since(started),
                now,
            );
        }
    }

    pub(crate) fn draw_completed(&mut self, elapsed: Duration, now: Instant) {
        self.record_slow("Draw + output", elapsed, now);
        self.presented(now);
    }

    pub(crate) fn cancel_input(&mut self) {
        self.pending_input = None;
    }

    pub(crate) fn reset(&mut self, now: Instant) {
        for series in &mut self.series {
            series.reset();
        }
        self.slow.fill(None);
        self.next_slow = 0;
        self.slow_count = 0;
        self.since = now;
        self.pending_input = None;
    }
}

pub(crate) struct TaskTotal {
    pub label: &'static str,
    pub count: u64,
    pub stopped: u64,
    pub elapsed: Duration,
    pub peak: Duration,
    pub queued: Duration,
    pub queue_samples: u64,
}

/// Aggregates include fast tasks, without allocating a path for their history.
/// A fixed category limit also bounds callers using many distinct static labels.
#[derive(Default)]
pub(crate) struct TaskTotals {
    pub rows: Vec<TaskTotal>,
    pub completed: u64,
}

impl TaskTotals {
    pub(crate) fn record(
        &mut self,
        label: &'static str,
        elapsed: Duration,
        queued: Option<Duration>,
        stopped: bool,
    ) {
        let label = if self.rows.len() >= TASK_KINDS - 1
            && !self.rows.iter().any(|row| row.label == label)
        {
            "Other tracked tasks"
        } else {
            label
        };
        let index = self
            .rows
            .iter()
            .position(|row| row.label == label)
            .unwrap_or_else(|| {
                self.rows.push(TaskTotal {
                    label,
                    count: 0,
                    stopped: 0,
                    elapsed: Duration::ZERO,
                    peak: Duration::ZERO,
                    queued: Duration::ZERO,
                    queue_samples: 0,
                });
                self.rows.len() - 1
            });
        let row = &mut self.rows[index];
        row.count = row.count.saturating_add(1);
        row.stopped = row.stopped.saturating_add(u64::from(stopped));
        row.elapsed = row.elapsed.saturating_add(elapsed);
        row.peak = row.peak.max(elapsed);
        if let Some(queued) = queued {
            row.queued = row.queued.saturating_add(queued.min(elapsed));
            row.queue_samples = row.queue_samples.saturating_add(1);
        }
        self.completed = self.completed.saturating_add(1);
    }

    pub(crate) fn reset(&mut self) {
        self.rows.clear();
        self.completed = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responsiveness_keeps_oldest_batched_input_and_excludes_idle_and_cancelled_work() {
        let mut ui = UiTimings::default();
        let start = Instant::now();
        ui.presented(start);
        assert!(!ui.timing(UiPhase::InputToFrame).has_samples());
        ui.input_handled(start, start + Duration::from_millis(2), true);
        ui.input_handled(
            start + Duration::from_millis(3),
            start + Duration::from_millis(5),
            true,
        );
        ui.presented(start + Duration::from_millis(9));
        assert_eq!(ui.timing(UiPhase::InputToFrame).last_ms, 9.0);
        ui.presented(start + Duration::from_secs(100));
        assert_eq!(ui.timing(UiPhase::InputToFrame).sample_count(), 1);
        ui.input_handled(start, start, true);
        ui.cancel_input();
        ui.presented(start + Duration::from_secs(100));
        assert_eq!(ui.timing(UiPhase::InputToFrame).sample_count(), 1);
        ui.input_handled(start, start, false);
        ui.presented(start + Duration::from_secs(100));
        assert_eq!(ui.timing(UiPhase::InputToFrame).sample_count(), 1);
    }

    #[test]
    fn responsiveness_history_is_bounded_and_reset_removes_pending_input() {
        let mut ui = UiTimings::default();
        let now = Instant::now();
        for value in 0..150 {
            ui.record(
                UiPhase::Preparation,
                Duration::from_millis(value),
                now + Duration::from_secs(value),
            );
        }
        assert_eq!(ui.timing(UiPhase::Preparation).recent_ms().count(), 120);
        assert_eq!(ui.slow_count, 100);
        assert_eq!(ui.slow_events().count(), SLOW_HISTORY);
        assert_eq!(
            ui.slow_events().next().unwrap().elapsed,
            Duration::from_millis(149)
        );
        ui.input_handled(now, now, true);
        ui.reset(now);
        ui.presented(now + Duration::from_secs(1));
        assert!(ui.slow_events().next().is_none());
        assert!(!ui.timing(UiPhase::InputToFrame).has_samples());
        ui.draw_completed(Duration::from_millis(90), now);
        assert_eq!(ui.slow_events().next().unwrap().label, "Draw + output");
        assert_eq!(ui.slow_count, 1);
    }

    #[test]
    fn fast_tasks_contribute_totals_and_categories_are_bounded() {
        let mut totals = TaskTotals::default();
        for _ in 0..1000 {
            totals.record(
                "Analysis",
                Duration::from_millis(2),
                Some(Duration::from_millis(1)),
                false,
            );
        }
        totals.record("Analysis", Duration::from_millis(10), None, true);
        assert_eq!(totals.completed, 1001);
        let row = &totals.rows[0];
        assert_eq!(row.elapsed, Duration::from_millis(2010));
        assert_eq!(row.queued, Duration::from_secs(1));
        assert_eq!(row.queue_samples, 1000);
        assert_eq!(row.stopped, 1);
        // Unique static slices without leaking test allocations.
        const LABELS: &str =
            "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!@#$%^&*()_+-=[]{}";
        for index in 0..LABELS.len() {
            totals.record(&LABELS[index..], Duration::ZERO, None, false);
        }
        assert_eq!(totals.rows.len(), TASK_KINDS);
        assert!(
            totals
                .rows
                .iter()
                .any(|row| row.label == "Other tracked tasks" && row.count > 1)
        );
        totals.reset();
        assert!(totals.rows.is_empty());
        assert_eq!(totals.completed, 0);
    }

    #[test]
    #[ignore = "manual diagnostics recording overhead benchmark"]
    fn benchmark_ui_timing_record_cost() {
        let mut ui = UiTimings::default();
        let mut times = Vec::new();
        for _ in 0..9 {
            let started = Instant::now();
            for _ in 0..100_000 {
                let start = Instant::now();
                let end = Instant::now();
                ui.record(UiPhase::Workers, end.duration_since(start), end);
                std::hint::black_box(&ui);
            }
            times.push(started.elapsed());
        }
        times.sort_unstable();
        eprintln!(
            "UI phase timing (two clocks + bounded recording): {:.1} ns/call median",
            times[4].as_nanos() as f64 / 100_000.0
        );
        assert_eq!(ui.timing(UiPhase::Workers).sample_count(), 900_000);
    }
}
