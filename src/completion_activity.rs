//! Per-window cooperative scheduling for optional completion work. Input and
//! presentation only write atomics; workers sleep while the foreground is busy.
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

const QUIET: Duration = Duration::from_millis(24);
const MAINTENANCE_QUIET: Duration = Duration::from_millis(350);
const CANCEL_WAIT: Duration = Duration::from_millis(10);

struct Shared {
    origin: Instant,
    deadline: AtomicU64,
    generation: AtomicU64,
    foreground: AtomicUsize,
    wait: Mutex<()>,
    wake: Condvar,
}

#[derive(Clone)]
pub(crate) struct Activity(Arc<Shared>);

impl Default for Activity {
    fn default() -> Self {
        Self(Arc::new(Shared {
            origin: Instant::now(),
            deadline: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            foreground: AtomicUsize::new(0),
            wait: Mutex::new(()),
            wake: Condvar::new(),
        }))
    }
}

impl Activity {
    fn now(&self) -> u64 {
        self.0
            .origin
            .elapsed()
            .as_micros()
            .min(u128::from(u64::MAX - 1)) as u64
            + 1
    }

    pub(crate) fn note_input(&self) {
        self.0.deadline.fetch_max(
            self.now().saturating_add(QUIET.as_micros() as u64),
            Ordering::Release,
        );
        self.0.generation.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn enter(&self) -> Foreground {
        self.0.foreground.fetch_add(1, Ordering::AcqRel);
        self.note_input();
        Foreground {
            activity: self.clone(),
            quiet_on_drop: true,
        }
    }

    /// Cosmetic animation frames must not restart the input debounce forever.
    /// Park new work for painting; OS priority still favors the renderer over
    /// an already-running completion whose source and caret have not changed.
    pub(crate) fn present(&self) -> Foreground {
        self.0.foreground.fetch_add(1, Ordering::AcqRel);
        Foreground {
            activity: self.clone(),
            quiet_on_drop: false,
        }
    }

    pub(crate) fn remaining(&self) -> Option<Duration> {
        if self.0.foreground.load(Ordering::Acquire) != 0 {
            return Some(CANCEL_WAIT);
        }
        let remaining = self
            .0
            .deadline
            .load(Ordering::Acquire)
            .saturating_sub(self.now());
        (remaining > 0).then(|| Duration::from_micros(remaining))
    }

    pub(crate) fn generation(&self) -> u64 {
        self.0.generation.load(Ordering::Acquire)
    }

    pub(crate) fn maintenance_remaining(&self) -> Option<Duration> {
        if self.0.foreground.load(Ordering::Acquire) != 0 {
            return Some(CANCEL_WAIT);
        }
        let deadline = self.0.deadline.load(Ordering::Acquire);
        if deadline == 0 {
            return None;
        }
        let remaining = deadline
            .saturating_add((MAINTENANCE_QUIET - QUIET).as_micros() as u64)
            .saturating_sub(self.now());
        (remaining > 0).then(|| Duration::from_micros(remaining))
    }

    pub(crate) fn interrupted(&self, generation: u64) -> bool {
        self.generation() != generation
    }

    // Called only for a real pending request. No idle timer or sampler exists.
    pub(crate) fn wait(&self, cancelled: impl Fn() -> bool) -> bool {
        loop {
            if cancelled() {
                return false;
            }
            let Some(delay) = self.remaining() else {
                return true;
            };
            let guard = self.0.wait.lock().unwrap_or_else(|e| e.into_inner());
            if cancelled() {
                return false;
            }
            drop(
                self.0
                    .wake
                    .wait_timeout(guard, delay.min(CANCEL_WAIT))
                    .unwrap_or_else(|e| e.into_inner()),
            );
        }
    }

    pub(crate) fn wake(&self) {
        self.0.wake.notify_all();
    }
}

pub(crate) struct Foreground {
    activity: Activity,
    quiet_on_drop: bool,
}
impl Drop for Foreground {
    fn drop(&mut self) {
        if self.quiet_on_drop {
            self.activity.note_input();
        }
        self.activity.0.foreground.fetch_sub(1, Ordering::AcqRel);
        self.activity.wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn input_invalidates_running_work_and_nested_presentation_keeps_it_parked() {
        let activity = Activity::default();
        assert!(activity.remaining().is_none());
        let before = activity.generation();
        let outer = activity.enter();
        let inner = activity.enter();
        assert!(activity.interrupted(before));
        activity.0.deadline.store(0, Ordering::Release);
        drop(inner);
        activity.0.deadline.store(0, Ordering::Release);
        assert!(activity.remaining().is_some());
        drop(outer);
        assert!(activity.remaining().is_some());
        activity.0.deadline.store(0, Ordering::Release);
        assert!(activity.remaining().is_none());
    }

    #[test]
    fn a_cancelled_request_does_not_wait_for_foreground_work() {
        let activity = Activity::default();
        let _foreground = activity.enter();
        assert!(!activity.wait(|| true));
    }

    #[test]
    fn cosmetic_paint_does_not_starve_completion_or_train_a_new_input_generation() {
        let activity = Activity::default();
        let generation = activity.generation();
        let paint = activity.present();
        assert!(activity.remaining().is_some());
        assert!(!activity.interrupted(generation));
        drop(paint);
        assert!(activity.remaining().is_none());
    }
}
