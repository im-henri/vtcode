//! Input delivery feedback uses remote acknowledgements, including silent input.
use std::{
    cell::Cell,
    time::{Duration, Instant},
};

use crate::remote::process::InputProgress;

const DELAY: Duration = Duration::from_millis(200);
const INTERVAL: Duration = Duration::from_millis(120);
// Match the three-cell terminal icon so tab geometry never changes.
const FRAMES: [&str; 10] = [
    "⠋  ", "⠙  ", "⠹  ", "⠸  ", "⠼  ", "⠴  ", "⠦  ", "⠧  ", "⠇  ", "⠏  ",
];

pub(crate) const HINT: &str = "Input received; waiting for the remote terminal.";

#[derive(Default)]
pub(crate) struct Activity {
    painted_at: Cell<Option<Instant>>,
}

pub(crate) fn symbol(progress: InputProgress, now: Instant) -> Option<&'static str> {
    if !progress.pending {
        return None;
    }
    let elapsed = now.saturating_duration_since(progress.pending_since?);
    let spinning = elapsed.checked_sub(DELAY)?;
    Some(FRAMES[(spinning.as_millis() / INTERVAL.as_millis() % FRAMES.len() as u128) as usize])
}

impl Activity {
    pub(crate) fn begin_frame(&self) {
        self.painted_at.set(None);
    }

    /// Register only a visible icon, even during the initial anti-flicker delay.
    pub(crate) fn show(&self, now: Instant) {
        self.painted_at.set(Some(now));
    }

    pub(crate) fn visible(&self) -> bool {
        self.painted_at.get().is_some()
    }

    pub(crate) fn next_wait(&self, progress: InputProgress, now: Instant) -> Option<Duration> {
        if !progress.pending {
            return None;
        }
        let painted_at = self.painted_at.get()?;
        let show_at = progress.pending_since? + DELAY;
        let next = if painted_at < show_at {
            show_at
        } else {
            painted_at + INTERVAL
        };
        Some(next.saturating_duration_since(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delayed_feedback_spins_until_acknowledged_and_only_schedules_visible_icons() {
        let activity = Activity::default();
        let start = Instant::now();
        let progress = InputProgress {
            pending: true,
            pending_since: Some(start),
            acknowledged: None,
        };
        assert_eq!(symbol(progress, start), None);
        assert_eq!(activity.next_wait(progress, start), None);
        activity.show(start);
        assert_eq!(activity.next_wait(progress, start), Some(DELAY));
        let shown = start + DELAY;
        assert_eq!(activity.next_wait(progress, shown), Some(Duration::ZERO));
        assert_eq!(symbol(progress, shown), Some(FRAMES[0]));
        activity.show(shown);
        assert_eq!(activity.next_wait(progress, shown), Some(INTERVAL));
        assert_eq!(symbol(progress, shown + INTERVAL), Some(FRAMES[1]));
        assert_eq!(symbol(progress, shown + INTERVAL * 10), Some(FRAMES[0]));
        let acknowledged = InputProgress {
            acknowledged: Some(shown),
            ..Default::default()
        };
        assert_eq!(symbol(acknowledged, shown), None);
        assert_eq!(activity.next_wait(acknowledged, shown), None);
        activity.begin_frame();
        assert_eq!(activity.next_wait(progress, shown), None);
        assert!(!activity.visible());
    }
}
