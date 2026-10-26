//! Leave CPU/terminal headroom when frames are expensive. Input redraws bypass
//! pacing; cosmetic and streamed background updates may share the next frame.
use std::time::{Duration, Instant};

const SAMPLE_LIFETIME: Duration = Duration::from_secs(1);
const BACKGROUND_FRAME_INTERVAL: Duration = Duration::from_millis(16);

#[derive(Default)]
pub(crate) struct FramePacing {
    finished: Option<Instant>,
    sampled: Option<Instant>,
    render_cost: Duration,
}

impl FramePacing {
    pub(crate) fn record(&mut self, finished: Instant, elapsed: Duration) {
        // Interactive frames reveal contention too. Waiting for an animation-
        // only sample kept scheduling extra work right after a slow edit.
        // React immediately, recover gradually, and bound terminal stalls.
        let cost = elapsed.min(Duration::from_millis(100));
        self.render_cost = if !self.sample_is_recent(finished) || cost >= self.render_cost {
            cost
        } else {
            self.render_cost - (self.render_cost - cost) / 4
        };
        self.sampled = Some(finished);
        self.finished = Some(finished);
    }

    pub(crate) fn wait(&self, now: Instant) -> Duration {
        self.wait_for(now, self.render_cost)
    }

    pub(crate) fn background_wait(&self, now: Instant) -> Duration {
        self.wait_for(now, self.render_cost.max(BACKGROUND_FRAME_INTERVAL))
    }

    fn wait_for(&self, now: Instant, interval: Duration) -> Duration {
        if !self.sample_is_recent(now) {
            return Duration::ZERO;
        }
        self.finished.map_or(Duration::ZERO, |finished| {
            interval.saturating_sub(now.saturating_duration_since(finished))
        })
    }

    fn sample_is_recent(&self, now: Instant) -> bool {
        self.sampled
            .is_some_and(|sample| now.saturating_duration_since(sample) <= SAMPLE_LIFETIME)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_cosmetic_frames_leave_headroom_without_accumulating_deadlines() {
        let mut pacing = FramePacing::default();
        let now = Instant::now();
        pacing.record(now, Duration::from_millis(40));
        assert_eq!(pacing.wait(now), Duration::from_millis(40));
        assert_eq!(
            pacing.wait(now + Duration::from_millis(15)),
            Duration::from_millis(25)
        );
        assert_eq!(pacing.wait(now + Duration::from_secs(1)), Duration::ZERO);
        // Input can repaint immediately and starts the next cosmetic rest.
        pacing.record(
            now + Duration::from_millis(5),
            Duration::from_millis(2),
        );
        assert_eq!(
            pacing.wait(now + Duration::from_millis(5)),
            Duration::from_micros(30_500)
        );
    }

    #[test]
    fn expensive_interactive_frames_leave_headroom_for_following_input() {
        let mut pacing = FramePacing::default();
        let now = Instant::now();
        pacing.record(now, Duration::from_secs(2));
        assert_eq!(pacing.wait(now), Duration::from_millis(100));
        pacing.record(now, Duration::from_millis(2));
        assert_eq!(pacing.wait(now), Duration::from_micros(75_500));
        pacing.record(now, Duration::from_millis(90));
        assert_eq!(pacing.wait(now), Duration::from_millis(90));
    }

    #[test]
    fn terminal_stalls_are_bounded_and_fast_rendering_recovers() {
        let mut pacing = FramePacing::default();
        let now = Instant::now();
        pacing.record(now, Duration::from_secs(5));
        assert_eq!(pacing.wait(now), Duration::from_millis(100));
        for _ in 0..16 {
            pacing.record(now, Duration::from_millis(1));
        }
        assert!(pacing.wait(now) < Duration::from_millis(2));
    }

    #[test]
    fn old_load_does_not_slow_new_animations_after_a_pause() {
        let mut pacing = FramePacing::default();
        let now = Instant::now();
        pacing.record(now, Duration::from_millis(80));
        let later = now + Duration::from_secs(2);
        assert_eq!(pacing.wait(later), Duration::ZERO);
        pacing.record(later, Duration::from_millis(1));
        assert_eq!(pacing.wait(later), Duration::from_millis(1));
    }

    #[test]
    fn streaming_results_coalesce_without_delaying_the_first_frame() {
        let mut pacing = FramePacing::default();
        let now = Instant::now();
        assert_eq!(pacing.background_wait(now), Duration::ZERO);
        pacing.record(now, Duration::from_millis(2));
        assert_eq!(pacing.background_wait(now), BACKGROUND_FRAME_INTERVAL);
        assert_eq!(pacing.background_wait(now + BACKGROUND_FRAME_INTERVAL), Duration::ZERO);
        pacing.record(now, Duration::from_millis(40));
        assert_eq!(pacing.background_wait(now), Duration::from_millis(40));
        assert_eq!(pacing.background_wait(now + Duration::from_secs(2)), Duration::ZERO);
    }
}
