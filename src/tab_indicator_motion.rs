//! A finite, analytic spring for the two edges of the active tab underline.
//!
//! Giving the leading edge a faster response stretches the underline toward the
//! destination before its trailing edge gathers. Each edge retains its own
//! velocity when interrupted; sampling never depends on the render cadence.

use crate::animation_config::{MAX_ANIMATION_DURATION_MS, MIN_ANIMATION_DURATION_MS};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TabIndicatorSample {
    pub left: f32,
    pub right: f32,
    pub left_velocity: f32,
    pub right_velocity: f32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TabIndicatorMotion {
    origin: TabIndicatorSample,
    left: f32,
    right: f32,
    left_rate: f32,
    right_rate: f32,
    duration: Duration,
}

impl TabIndicatorMotion {
    pub(crate) fn stationary(left: f32, right: f32) -> Self {
        let (left, right) = valid_span(left, right);
        Self {
            origin: TabIndicatorSample {
                left,
                right,
                left_velocity: 0.0,
                right_velocity: 0.0,
            },
            left,
            right,
            left_rate: 0.0,
            right_rate: 0.0,
            duration: Duration::ZERO,
        }
    }

    pub(crate) fn retarget(
        current: TabIndicatorSample,
        left: f32,
        right: f32,
        duration: Duration,
        intensity: u8,
    ) -> Self {
        let (left, right) = valid_span(left, right);
        let (origin_left, origin_right) = valid_span(current.left, current.right);
        let origin = TabIndicatorSample {
            left: origin_left,
            right: origin_right,
            left_velocity: finite_or_zero(current.left_velocity),
            right_velocity: finite_or_zero(current.right_velocity),
        };
        if origin.left == left
            && origin.right == right
            && origin.left_velocity == 0.0
            && origin.right_velocity == 0.0
        {
            return Self::stationary(left, right);
        }
        let duration = duration.clamp(
            Duration::from_millis(MIN_ANIMATION_DURATION_MS),
            Duration::from_millis(MAX_ANIMATION_DURATION_MS),
        );
        let strength = intensity.min(100) as f32 / 100.0;
        let lead = 11.0 + strength * 4.0;
        let trail = 9.0 - strength * 2.0;
        let direction = (left + right) - (origin.left + origin.right);
        let (left_rate, right_rate) = if direction > 0.001 {
            (trail, lead)
        } else if direction < -0.001 {
            (lead, trail)
        } else {
            // A width change around a fixed center should gather symmetrically.
            let rate = (lead + trail) * 0.5;
            (rate, rate)
        };
        Self {
            origin,
            left,
            right,
            left_rate,
            right_rate,
            duration,
        }
    }

    #[cfg(test)]
    pub(crate) fn duration(self) -> Duration {
        self.duration
    }

    pub(crate) fn is_settled(self, elapsed: Duration) -> bool {
        elapsed >= self.duration
    }

    pub(crate) fn sample(self, elapsed: Duration) -> TabIndicatorSample {
        if self.is_settled(elapsed) {
            return TabIndicatorSample {
                left: self.left,
                right: self.right,
                left_velocity: 0.0,
                right_velocity: 0.0,
            };
        }
        let seconds = self.duration.as_secs_f32();
        let time = elapsed.as_secs_f32();
        let progress = (time / seconds).clamp(0.0, 1.0);
        // The compact envelope is flat at the start and has two flat
        // derivatives at the end. It removes the spring's tiny asymptotic tail
        // without an end-of-animation snap or a permanent frame timer.
        let remainder = 1.0 - progress.powi(4);
        let envelope = remainder.powi(3);
        let envelope_velocity = -12.0 * progress.powi(3) * remainder.powi(2) / seconds;
        let edge = |origin: f32, target: f32, velocity: f32, rate: f32| {
            let omega = rate / seconds;
            let displacement = origin - target;
            let coefficient = velocity + omega * displacement;
            let decay = (-omega * time).exp();
            let residual = (displacement + coefficient * time) * decay;
            let speed = (velocity - omega * coefficient * time) * decay;
            (
                target + residual * envelope,
                speed * envelope + residual * envelope_velocity,
            )
        };
        let (mut left, mut left_velocity) = edge(
            self.origin.left,
            self.left,
            self.origin.left_velocity,
            self.left_rate,
        );
        let (mut right, mut right_velocity) = edge(
            self.origin.right,
            self.right,
            self.origin.right_velocity,
            self.right_rate,
        );
        // Several reversals within a frame can send the edges past each other.
        // Preserve the center and its velocity while keeping a visible filament.
        if right - left < 0.25 {
            let center = (left + right) * 0.5;
            let center_velocity = (left_velocity + right_velocity) * 0.5;
            left = center - 0.125;
            right = center + 0.125;
            left_velocity = center_velocity;
            right_velocity = center_velocity;
        }
        TabIndicatorSample {
            left,
            right,
            left_velocity,
            right_velocity,
        }
    }
}

fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

fn valid_span(left: f32, right: f32) -> (f32, f32) {
    let left = finite_or_zero(left);
    let right = finite_or_zero(right);
    (left.min(right), left.max(right).max(left.min(right) + 0.25))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn motion(left: f32, right: f32, target_left: f32, target_right: f32) -> TabIndicatorMotion {
        TabIndicatorMotion::retarget(
            TabIndicatorMotion::stationary(left, right).sample(Duration::ZERO),
            target_left,
            target_right,
            Duration::from_millis(240),
            75,
        )
    }

    #[test]
    fn tab_spring_leading_edge_stretches_then_trailing_edge_gathers_in_both_directions() {
        for direction in [1.0, -1.0] {
            let start_left = if direction > 0.0 { 0.0 } else { 30.0 };
            let moving = motion(
                start_left,
                start_left + 10.0,
                30.0 - start_left,
                40.0 - start_left,
            );
            let early = moving.sample(Duration::from_millis(40));
            let gathered = moving.sample(Duration::from_millis(180));
            let left_progress = (early.left - start_left).abs();
            let right_progress = (early.right - start_left - 10.0).abs();
            if direction > 0.0 {
                assert!(right_progress > left_progress + 4.0);
            } else {
                assert!(left_progress > right_progress + 4.0);
            }
            assert!(early.right - early.left > 14.0);
            assert!((gathered.right - gathered.left - 10.0).abs() < 0.5);
        }
    }

    #[test]
    fn tab_spring_interruption_preserves_both_edge_positions_and_velocities() {
        let first = motion(0.0, 8.0, 50.0, 75.0);
        let current = first.sample(Duration::from_millis(37));
        let reverse = TabIndicatorMotion::retarget(current, 2.0, 14.0, first.duration(), 90);
        let restarted = reverse.sample(Duration::ZERO);
        for (a, b) in [
            (current.left, restarted.left),
            (current.right, restarted.right),
            (current.left_velocity, restarted.left_velocity),
            (current.right_velocity, restarted.right_velocity),
        ] {
            assert!((a - b).abs() < 0.0001, "{a} became {b}");
        }
        let next = reverse.sample(Duration::from_micros(100));
        assert!((next.left - current.left - current.left_velocity * 0.0001).abs() < 0.001);
        assert!((next.right - current.right - current.right_velocity * 0.0001).abs() < 0.001);
    }

    #[test]
    fn tab_spring_has_an_exact_quiet_endpoint_with_no_last_frame_snap() {
        let moving = motion(0.0, 8.0, 70.0, 90.0);
        let before = moving.sample(moving.duration() - Duration::from_micros(100));
        let end = moving.sample(moving.duration());
        assert_eq!((end.left, end.right), (70.0, 90.0));
        assert_eq!((end.left_velocity, end.right_velocity), (0.0, 0.0));
        assert!((before.left - end.left).abs() < 0.0001);
        assert!((before.right - end.right).abs() < 0.0001);
        assert!(before.left_velocity.abs() < 0.001);
        assert!(before.right_velocity.abs() < 0.001);
        assert!(moving.is_settled(moving.duration()));
        assert_eq!(end, moving.sample(Duration::from_secs(60)));
    }

    #[test]
    fn tab_spring_is_independent_of_observation_cadence() {
        let moving = motion(2.0, 11.0, 60.0, 93.0);
        let expected = moving.sample(Duration::from_millis(127));
        for micros in (0..127_000).step_by(733) {
            let _ = moving.sample(Duration::from_micros(micros));
        }
        assert_eq!(expected, moving.sample(Duration::from_millis(127)));
    }

    #[test]
    fn tab_spring_many_rapid_reversals_keep_finite_visible_geometry() {
        let mut moving = motion(0.0, 25.0, 80.0, 81.0);
        for step in 0..500 {
            let elapsed = Duration::from_millis((step % 31 + 1) as u64);
            let current = moving.sample(elapsed);
            assert!(current.left.is_finite() && current.right.is_finite());
            assert!(current.left_velocity.is_finite() && current.right_velocity.is_finite());
            assert!(current.right - current.left >= 0.2499);
            let left = if step % 2 == 0 { 1.0 } else { 100.0 };
            moving = TabIndicatorMotion::retarget(
                current,
                left,
                left + (step % 23 + 1) as f32,
                Duration::from_millis(240),
                100,
            );
        }
        assert!(moving.is_settled(Duration::from_secs(2)));
    }

    #[test]
    fn tab_spring_stationary_and_invalid_inputs_do_not_create_motion_or_nan() {
        let still = TabIndicatorMotion::stationary(15.0, 30.0);
        assert_eq!(still.duration(), Duration::ZERO);
        assert!(still.is_settled(Duration::ZERO));
        assert_eq!(
            still.sample(Duration::ZERO),
            still.sample(Duration::from_secs(4))
        );
        let invalid = TabIndicatorMotion::stationary(f32::NAN, f32::INFINITY);
        let moving = TabIndicatorMotion::retarget(
            invalid.sample(Duration::ZERO),
            f32::NEG_INFINITY,
            10.0,
            Duration::ZERO,
            255,
        );
        assert_eq!(moving.duration(), Duration::from_millis(1));
        let sample = moving.sample(Duration::from_millis(25));
        assert!(sample.left.is_finite() && sample.right.is_finite());
        assert!(sample.right > sample.left);
    }

    #[test]
    fn tab_spring_equal_geometry_replacement_stays_quiet_but_live_velocity_is_retained() {
        let still = TabIndicatorMotion::stationary(12.0, 30.0).sample(Duration::ZERO);
        let replacement =
            TabIndicatorMotion::retarget(still, 12.0, 30.0, Duration::from_millis(240), 75);
        assert!(replacement.is_settled(Duration::ZERO));
        assert_eq!(replacement.duration(), Duration::ZERO);
        assert_eq!(replacement.sample(Duration::ZERO), still);

        let moving = TabIndicatorSample {
            left_velocity: 20.0,
            right_velocity: 30.0,
            ..still
        };
        let retargeted =
            TabIndicatorMotion::retarget(moving, 12.0, 30.0, Duration::from_millis(240), 75);
        assert!(!retargeted.is_settled(Duration::ZERO));
        assert_eq!(retargeted.sample(Duration::ZERO), moving);
    }

    #[test]
    fn tab_spring_duration_accepts_one_to_five_thousand_ms_and_clamps_invalid_bounds() {
        for (requested, expected) in [
            (Duration::ZERO, Duration::from_millis(1)),
            (Duration::from_millis(1), Duration::from_millis(1)),
            (Duration::from_millis(5000), Duration::from_millis(5000)),
            (Duration::MAX, Duration::from_millis(5000)),
        ] {
            let moving = TabIndicatorMotion::retarget(
                TabIndicatorMotion::stationary(0.0, 10.0).sample(Duration::ZERO),
                20.0,
                30.0,
                requested,
                55,
            );
            assert_eq!(moving.duration(), expected);
            assert!(!moving.is_settled(expected / 2));
            let halfway = moving.sample(expected / 2);
            assert!(halfway.left.is_finite() && halfway.right.is_finite());
            assert_eq!(
                (moving.sample(expected).left, moving.sample(expected).right),
                (20.0, 30.0)
            );
            assert!(moving.is_settled(expected));
        }
    }
}
