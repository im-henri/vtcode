//! Elastic cursor geometry, implemented independently of editor rendering.
//!
//! The four corners have different spring rates. A smooth bound on their
//! deformation keeps long jumps and repeated direction changes controlled.
//! Sampling is analytic: its shape depends on elapsed time, never frame count.
use std::time::Duration;

use crate::{
    animation_config::{AnimationEffect, AnimationSettings},
    animations::MotionPoint,
};

const OFFSETS: [MotionPoint; 4] = [
    MotionPoint { x: -0.5, y: -0.5 },
    MotionPoint { x: 0.5, y: -0.5 },
    MotionPoint { x: 0.5, y: 0.5 },
    MotionPoint { x: -0.5, y: 0.5 },
];
const NAVIGATION_LIMIT: f32 = 16.0;
const TYPING_LIMIT: f32 = 1.4;

#[derive(Clone, Copy, Debug)]
pub(crate) struct SmearSample {
    /// Absolute terminal-cell boundaries, ordered top-left, top-right,
    /// bottom-right, bottom-left. A settled cursor occupies one whole cell.
    pub corners: [MotionPoint; 4],
    /// Corner velocities in terminal cells per second.
    pub velocities: [MotionPoint; 4],
    pub head: MotionPoint,
    pub tail: MotionPoint,
    /// Physical unit vector for the color gradient (one row is two columns).
    /// It turns continuously when movement reverses instead of swapping ends.
    pub shade_direction: MotionPoint,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SmearMotion {
    center: MotionPoint,
    center_velocity: MotionPoint,
    target: MotionPoint,
    raw_corners: [MotionPoint; 4],
    raw_velocities: [MotionPoint; 4],
    corner_rates: [f32; 4],
    center_rate: f32,
    limit_from: f32,
    limit_to: f32,
    shade_from: MotionPoint,
    shade_to: MotionPoint,
    shade_angle: f32,
    shade_rotation: f32,
}

fn add(a: MotionPoint, b: MotionPoint) -> MotionPoint {
    MotionPoint {
        x: a.x + b.x,
        y: a.y + b.y,
    }
}

fn sub(a: MotionPoint, b: MotionPoint) -> MotionPoint {
    MotionPoint {
        x: a.x - b.x,
        y: a.y - b.y,
    }
}

fn scale(a: MotionPoint, value: f32) -> MotionPoint {
    MotionPoint {
        x: a.x * value,
        y: a.y * value,
    }
}

fn dot(a: MotionPoint, b: MotionPoint) -> f32 {
    a.x * b.x + a.y * b.y
}

fn physical(a: MotionPoint) -> MotionPoint {
    MotionPoint {
        x: a.x,
        y: a.y * 2.0,
    }
}

fn cell_units(a: MotionPoint) -> MotionPoint {
    MotionPoint {
        x: a.x,
        y: a.y * 0.5,
    }
}

fn mean(points: &[MotionPoint; 4]) -> MotionPoint {
    scale(
        points.iter().copied().fold(MotionPoint::default(), add),
        0.25,
    )
}

fn norm(points: &[MotionPoint; 4]) -> f32 {
    points
        .iter()
        .map(|point| dot(*point, *point))
        .sum::<f32>()
        .sqrt()
}

fn spring(
    origin: MotionPoint,
    target: MotionPoint,
    velocity: MotionPoint,
    omega: f32,
    time: f32,
) -> (MotionPoint, MotionPoint) {
    let displacement = sub(origin, target);
    let coefficient = add(velocity, scale(displacement, omega));
    let decay = (-omega * time).exp();
    (
        add(
            target,
            scale(add(displacement, scale(coefficient, time)), decay),
        ),
        scale(sub(velocity, scale(coefficient, omega * time)), decay),
    )
}

/// Invert the deformation bound and its Jacobian at the retarget instant.
/// This preserves both visible position and visible velocity, including a
/// shape that was already stretched by the previous movement.
fn unbound(
    deformation: [MotionPoint; 4],
    velocity: [MotionPoint; 4],
    limit: f32,
) -> ([MotionPoint; 4], [MotionPoint; 4]) {
    let radius = norm(&deformation);
    if radius < 0.00001 {
        return (deformation, velocity);
    }
    let fraction = (radius / limit).clamp(0.0, 0.99999);
    let raw_radius = limit * fraction.atanh();
    let tangent_scale = raw_radius / radius;
    let radial_scale = 1.0 / (1.0 - fraction * fraction);
    let radial_velocity = deformation
        .iter()
        .zip(velocity)
        .map(|(d, v)| dot(*d, v))
        .sum::<f32>()
        / radius;
    (
        deformation.map(|point| scale(point, tangent_scale)),
        std::array::from_fn(|index| {
            let direction = scale(deformation[index], 1.0 / radius);
            add(
                scale(
                    sub(velocity[index], scale(direction, radial_velocity)),
                    tangent_scale,
                ),
                scale(direction, radial_velocity * radial_scale),
            )
        }),
    )
}

fn bound(
    deformation: [MotionPoint; 4],
    velocity: [MotionPoint; 4],
    limit: f32,
    limit_velocity: f32,
) -> ([MotionPoint; 4], [MotionPoint; 4]) {
    let radius = norm(&deformation);
    if radius < 0.00001 {
        return (deformation, velocity);
    }
    let fraction = radius / limit;
    let saturation = fraction.tanh();
    let derivative = 1.0 - saturation * saturation;
    let bounded_radius = limit * saturation;
    let tangent_scale = bounded_radius / radius;
    let radial_velocity = deformation
        .iter()
        .zip(velocity)
        .map(|(d, v)| dot(*d, v))
        .sum::<f32>()
        / radius;
    let bounded_velocity =
        derivative * radial_velocity + limit_velocity * (saturation - fraction * derivative);
    (
        deformation.map(|point| scale(point, tangent_scale)),
        std::array::from_fn(|index| {
            let direction = scale(deformation[index], 1.0 / radius);
            add(
                scale(
                    sub(velocity[index], scale(direction, radial_velocity)),
                    tangent_scale,
                ),
                scale(direction, bounded_velocity),
            )
        }),
    )
}

impl SmearMotion {
    pub(crate) fn retarget(
        origin: MotionPoint,
        target: MotionPoint,
        previous: Option<SmearSample>,
        editing: bool,
    ) -> Self {
        let initial_center = add(origin, MotionPoint { x: 0.5, y: 0.5 });
        let corners = previous.map_or_else(
            || OFFSETS.map(|offset| add(initial_center, offset)),
            |sample| sample.corners,
        );
        let velocities = previous.map_or([MotionPoint::default(); 4], |sample| sample.velocities);
        let center = mean(&corners);
        let center_velocity = mean(&velocities);
        let target_center = add(target, MotionPoint { x: 0.5, y: 0.5 });
        let travel = physical(sub(target_center, center));
        let travel_length = dot(travel, travel).sqrt();
        let (heading, heading_length) = if travel_length > 0.0001 {
            (travel, travel_length)
        } else {
            let heading = physical(center_velocity);
            (heading, dot(heading, heading).sqrt())
        };
        let direction = if heading_length > 0.0001 {
            scale(heading, 1.0 / heading_length)
        } else {
            MotionPoint { x: 1.0, y: 0.0 }
        };
        let deformation =
            std::array::from_fn(|index| physical(sub(sub(corners[index], center), OFFSETS[index])));
        let deformation_velocity =
            std::array::from_fn(|index| physical(sub(velocities[index], center_velocity)));
        let limit_to = if editing {
            TYPING_LIMIT
        } else {
            NAVIGATION_LIMIT
        };
        // A navigation-to-typing transition contracts the inherited shape
        // smoothly. Shrinking it immediately would break retarget continuity.
        let limit_from = limit_to.max(norm(&deformation) * 1.0001 + 0.0001);
        let (raw_deformation, raw_velocity) =
            unbound(deformation, deformation_velocity, limit_from);
        let center_rate = if editing { 24.0 } else { 18.0 };
        let contrast =
            (if editing { 0.5 } else { 0.7 }) * (0.5 + 0.5 * (travel_length / 3.0).clamp(0.0, 1.0));
        let projection_extent = direction.x.abs() * 0.5 + direction.y.abs();
        let corner_rates = OFFSETS.map(|offset| {
            let projection = dot(physical(offset), direction) / projection_extent;
            center_rate * (contrast * projection).exp()
        });
        let shade_from = previous.map_or(direction, |sample| sample.shade_direction);
        let shade_angle = shade_from.y.atan2(shade_from.x);
        let shade_rotation = (direction.y.atan2(direction.x) - shade_angle + std::f32::consts::PI)
            .rem_euclid(std::f32::consts::TAU)
            - std::f32::consts::PI;
        Self {
            center,
            center_velocity,
            target,
            raw_corners: std::array::from_fn(|index| {
                add(
                    add(center, OFFSETS[index]),
                    cell_units(raw_deformation[index]),
                )
            }),
            raw_velocities: std::array::from_fn(|index| {
                add(center_velocity, cell_units(raw_velocity[index]))
            }),
            corner_rates,
            center_rate,
            limit_from,
            limit_to,
            shade_from,
            shade_to: direction,
            shade_angle,
            shade_rotation,
        }
    }

    #[cfg(test)]
    pub(crate) fn sample(&self, settings: &AnimationSettings, elapsed: Duration) -> SmearSample {
        self.sample_for_effect(settings, AnimationEffect::CursorSmear, elapsed)
    }

    pub(crate) fn sample_for_effect(
        &self,
        settings: &AnimationSettings,
        effect: AnimationEffect,
        elapsed: Duration,
    ) -> SmearSample {
        let duration = Duration::from_millis(settings.duration(effect));
        let seconds = duration.as_secs_f32();
        let time = elapsed.as_secs_f32();
        let target_center = add(self.target, MotionPoint { x: 0.5, y: 0.5 });
        if elapsed >= duration {
            return self.result(
                OFFSETS.map(|offset| add(target_center, offset)),
                [MotionPoint::default(); 4],
                self.shade_to,
            );
        }
        let (raw_center, raw_center_velocity) = spring(
            self.center,
            target_center,
            self.center_velocity,
            self.center_rate / seconds,
            time,
        );
        let raw_samples: [(MotionPoint, MotionPoint); 4] = std::array::from_fn(|index| {
            spring(
                self.raw_corners[index],
                add(target_center, OFFSETS[index]),
                self.raw_velocities[index],
                self.corner_rates[index] / seconds,
                time,
            )
        });
        let mut deformation = std::array::from_fn(|index| {
            physical(sub(sub(raw_samples[index].0, raw_center), OFFSETS[index]))
        });
        let mut velocity =
            std::array::from_fn(|index| physical(sub(raw_samples[index].1, raw_center_velocity)));
        // Keep translation and shape separate. One common bound preserves the
        // zero-mean deformation and cannot grow under repeated retargets.
        let mean_deformation = mean(&deformation);
        let mean_velocity = mean(&velocity);
        for index in 0..4 {
            deformation[index] = sub(deformation[index], mean_deformation);
            velocity[index] = sub(velocity[index], mean_velocity);
        }
        let limit_delta = self.limit_from - self.limit_to;
        // Most retargets inherit a shape already inside the same bound. Their
        // bound is constant, so no contraction exponential is needed.
        let (limit, limit_velocity) = if limit_delta == 0.0 {
            (self.limit_to, 0.0)
        } else {
            let contraction_rate = 30.0 / seconds;
            let contraction = (-contraction_rate * time).exp();
            (
                self.limit_to + limit_delta * (1.0 + contraction_rate * time) * contraction,
                -limit_delta * contraction_rate * contraction_rate * time * contraction,
            )
        };
        let (deformation, velocity) = bound(deformation, velocity, limit, limit_velocity);
        // A quintic finish gives an exact, stationary final cell without a
        // last-frame snap, even after a large jump or a dropped frame.
        let phase = ((time / seconds - 0.55) / 0.45).clamp(0.0, 1.0);
        let finish = 1.0 - phase.powi(3) * (10.0 + phase * (-15.0 + 6.0 * phase));
        let finish_velocity = -30.0 * phase.powi(2) * (phase - 1.0).powi(2) / (0.45 * seconds);
        let center_error = sub(raw_center, target_center);
        let center = add(target_center, scale(center_error, finish));
        let center_velocity = add(
            scale(raw_center_velocity, finish),
            scale(center_error, finish_velocity),
        );
        self.result(
            std::array::from_fn(|index| {
                add(
                    add(center, OFFSETS[index]),
                    cell_units(scale(deformation[index], finish)),
                )
            }),
            std::array::from_fn(|index| {
                add(
                    center_velocity,
                    cell_units(add(
                        scale(velocity[index], finish),
                        scale(deformation[index], finish_velocity),
                    )),
                )
            }),
            self.shade_direction(time / seconds),
        )
    }

    fn shade_direction(&self, progress: f32) -> MotionPoint {
        let phase = (progress / 0.3).clamp(0.0, 1.0);
        if phase <= 0.0 {
            // Preserve the exact inherited vector at the retarget instant.
            self.shade_from
        } else if phase >= 1.0 || self.shade_rotation == 0.0 {
            self.shade_to
        } else {
            let ease = phase.powi(3) * (10.0 + phase * (-15.0 + 6.0 * phase));
            let angle = self.shade_angle + self.shade_rotation * ease;
            let (y, x) = angle.sin_cos();
            MotionPoint { x, y }
        }
    }

    fn result(
        &self,
        corners: [MotionPoint; 4],
        velocities: [MotionPoint; 4],
        shade_direction: MotionPoint,
    ) -> SmearSample {
        let center = mean(&corners);
        let projections = corners.map(|corner| dot(physical(sub(corner, center)), shade_direction));
        let first = projections.into_iter().fold(f32::NEG_INFINITY, f32::max);
        let last = projections.into_iter().fold(f32::INFINITY, f32::min);
        SmearSample {
            corners,
            velocities,
            head: add(center, cell_units(scale(shade_direction, first))),
            tail: add(center, cell_units(scale(shade_direction, last))),
            shade_direction,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extent(sample: &SmearSample) -> f32 {
        sample
            .corners
            .iter()
            .flat_map(|first| {
                sample.corners.iter().map(move |last| {
                    let difference = physical(sub(*first, *last));
                    dot(difference, difference).sqrt()
                })
            })
            .fold(0.0, f32::max)
    }

    fn close(a: MotionPoint, b: MotionPoint, tolerance: f32) {
        assert!(
            (a.x - b.x).abs() <= tolerance && (a.y - b.y).abs() <= tolerance,
            "{a:?} differs from {b:?}"
        );
    }

    #[test]
    fn leading_corners_move_first_and_stretch_the_cursor_body() {
        let settings = AnimationSettings::default();
        let motion = SmearMotion::retarget(
            MotionPoint::default(),
            MotionPoint { x: 20.0, y: 0.0 },
            None,
            false,
        );
        let sample = motion.sample(&settings, Duration::from_millis(20));
        assert!(sample.corners[1].x - sample.corners[0].x > 3.0);
        assert!(sample.corners[1].x > sample.corners[0].x + 1.0);
        assert!(sample.head.x > sample.tail.x);
        assert!(sample.head.x < 21.0);
        assert!(sample.corners[1].x > 1.0);
    }

    #[test]
    fn smear_settles_at_one_ms_and_can_keep_moving_for_five_thousand_ms() {
        let motion = SmearMotion::retarget(
            MotionPoint::default(),
            MotionPoint { x: 20.0, y: 7.0 },
            None,
            false,
        );
        let mut settings = AnimationSettings::default();
        settings.set_duration(AnimationEffect::CursorSmear, 1);
        let settled = motion.sample(&settings, Duration::from_millis(1));
        let later = motion.sample(&settings, Duration::from_secs(6));
        assert_eq!(settled.corners, later.corners);
        assert_eq!(settled.velocities, later.velocities);
        assert!(
            settled
                .velocities
                .iter()
                .all(|velocity| *velocity == MotionPoint::default())
        );
        settings.set_duration(AnimationEffect::CursorSmear, 5000);
        let moving = motion.sample(&settings, Duration::from_secs(2));
        assert_ne!(moving.corners, settled.corners);
        assert!(
            moving
                .corners
                .iter()
                .chain(&moving.velocities)
                .all(|point| point.x.is_finite() && point.y.is_finite())
        );
        assert_eq!(
            motion.sample(&settings, Duration::from_secs(5)).corners,
            settled.corners
        );
    }

    #[test]
    fn retarget_preserves_all_corner_positions_and_velocities() {
        let settings = AnimationSettings::default();
        let first = SmearMotion::retarget(
            MotionPoint::default(),
            MotionPoint { x: 24.0, y: 7.0 },
            None,
            false,
        );
        let before = first.sample(&settings, Duration::from_millis(32));
        let second = SmearMotion::retarget(
            MotionPoint::default(),
            MotionPoint { x: 3.0, y: 12.0 },
            Some(before),
            false,
        );
        let after = second.sample(&settings, Duration::ZERO);
        for index in 0..4 {
            close(before.corners[index], after.corners[index], 0.0002);
            close(before.velocities[index], after.velocities[index], 0.01);
        }
        close(before.shade_direction, after.shade_direction, 0.0);
        close(before.head, after.head, 0.0003);
        close(before.tail, after.tail, 0.0003);
    }

    #[test]
    fn quarter_and_half_turns_rotate_shading_continuously_and_remain_finite() {
        let settings = AnimationSettings::default();
        let first = SmearMotion::retarget(
            MotionPoint::default(),
            MotionPoint { x: 24.0, y: 0.0 },
            None,
            false,
        );
        let before = first.sample(&settings, Duration::from_millis(30));
        let center = mean(&before.corners);
        for target in [
            MotionPoint {
                x: center.x - 0.5,
                y: 20.0,
            },
            MotionPoint { x: -20.0, y: 0.0 },
        ] {
            let next = SmearMotion::retarget(MotionPoint::default(), target, Some(before), false);
            let at_start = next.sample(&settings, Duration::ZERO);
            close(before.shade_direction, at_start.shade_direction, 0.0);
            close(before.head, at_start.head, 0.0003);
            close(before.tail, at_start.tail, 0.0003);
            let mut previous = at_start.shade_direction;
            for milliseconds in 1..=72 {
                let sample = next.sample(&settings, Duration::from_millis(milliseconds));
                assert!(sample.head.x.is_finite() && sample.head.y.is_finite());
                assert!(sample.tail.x.is_finite() && sample.tail.y.is_finite());
                assert!((dot(sample.shade_direction, sample.shade_direction) - 1.0).abs() < 0.0001);
                assert!(
                    dot(previous, sample.shade_direction) > 0.99,
                    "gradient jumped at {milliseconds}ms"
                );
                previous = sample.shade_direction;
            }
            close(previous, next.shade_to, 0.00001);
        }
    }

    #[test]
    fn shade_rotation_takes_the_short_path_across_the_angle_boundary() {
        let settings = AnimationSettings::default();
        let mut before = SmearMotion::retarget(
            MotionPoint::default(),
            MotionPoint { x: 10.0, y: 0.0 },
            None,
            false,
        )
        .sample(&settings, Duration::from_millis(30));
        let angle = 179.0_f32.to_radians();
        before.shade_direction = MotionPoint {
            x: angle.cos(),
            y: angle.sin(),
        };
        let center = mean(&before.corners);
        let target_angle = -179.0_f32.to_radians();
        let target = add(
            sub(center, MotionPoint { x: 0.5, y: 0.5 }),
            cell_units(MotionPoint {
                x: target_angle.cos() * 20.0,
                y: target_angle.sin() * 20.0,
            }),
        );
        let next = SmearMotion::retarget(MotionPoint::default(), target, Some(before), false);
        assert!(next.shade_rotation.abs() < 3.0_f32.to_radians());
        let middle = next.sample(&settings, Duration::from_millis(36));
        assert!(middle.shade_direction.x < -0.999);
    }

    #[test]
    fn rapid_reversals_keep_the_shape_bounded_and_finite() {
        let settings = AnimationSettings::default();
        let mut motion = SmearMotion::retarget(
            MotionPoint::default(),
            MotionPoint { x: 180.0, y: 60.0 },
            None,
            false,
        );
        for index in 0..200 {
            let sample = motion.sample(&settings, Duration::from_millis(8));
            assert!(
                extent(&sample) < 25.1,
                "unbounded shape at step {index}: {}",
                extent(&sample)
            );
            assert!(
                sample
                    .corners
                    .iter()
                    .chain(&sample.velocities)
                    .all(|point| point.x.is_finite() && point.y.is_finite())
            );
            let target = if index % 2 == 0 {
                MotionPoint { x: 2.0, y: 1.0 }
            } else {
                MotionPoint { x: 180.0, y: 60.0 }
            };
            motion = SmearMotion::retarget(MotionPoint::default(), target, Some(sample), false);
        }
    }

    #[test]
    fn typing_is_compact_and_settles_faster_than_navigation() {
        let settings = AnimationSettings::default();
        for distance in [1.0, 4.0, 80.0, 1_000.0] {
            let motion = SmearMotion::retarget(
                MotionPoint::default(),
                MotionPoint {
                    x: distance,
                    y: 0.0,
                },
                None,
                true,
            );
            for milliseconds in (0..240).step_by(4) {
                let sample = motion.sample(&settings, Duration::from_millis(milliseconds));
                let left = sample
                    .corners
                    .iter()
                    .map(|point| point.x)
                    .fold(f32::INFINITY, f32::min);
                let right = sample
                    .corners
                    .iter()
                    .map(|point| point.x)
                    .fold(f32::NEG_INFINITY, f32::max);
                assert!(right - left < 3.01);
            }
        }
        let target = MotionPoint { x: 1.0, y: 0.0 };
        let typing = SmearMotion::retarget(MotionPoint::default(), target, None, true)
            .sample(&settings, Duration::from_millis(32));
        let navigation = SmearMotion::retarget(MotionPoint::default(), target, None, false)
            .sample(&settings, Duration::from_millis(32));
        assert!(mean(&typing.corners).x > mean(&navigation.corners).x);
    }

    #[test]
    fn typing_contracts_inherited_navigation_without_a_retarget_jump() {
        let settings = AnimationSettings::default();
        let before = SmearMotion::retarget(
            MotionPoint::default(),
            MotionPoint { x: 100.0, y: 0.0 },
            None,
            false,
        )
        .sample(&settings, Duration::from_millis(20));
        let typing = SmearMotion::retarget(
            MotionPoint::default(),
            MotionPoint { x: 101.0, y: 0.0 },
            Some(before),
            true,
        );
        let at_start = typing.sample(&settings, Duration::ZERO);
        for index in 0..4 {
            close(before.corners[index], at_start.corners[index], 0.0002);
            close(before.velocities[index], at_start.velocities[index], 0.02);
        }
        assert!(extent(&typing.sample(&settings, Duration::from_millis(64))) < 4.3);
    }

    #[test]
    fn frame_skips_and_expiry_finish_at_the_exact_destination() {
        let settings = AnimationSettings::default();
        let target = MotionPoint { x: 75.0, y: 32.0 };
        let motion = SmearMotion::retarget(MotionPoint::default(), target, None, false);
        let direct = motion.sample(&settings, Duration::from_millis(137));
        for milliseconds in 0..137 {
            let _ = motion.sample(&settings, Duration::from_millis(milliseconds));
        }
        let stepped = motion.sample(&settings, Duration::from_millis(137));
        assert_eq!(direct.corners, stepped.corners);
        let settled = motion.sample(&settings, Duration::from_secs(2));
        for index in 0..4 {
            close(
                settled.corners[index],
                add(add(target, MotionPoint { x: 0.5, y: 0.5 }), OFFSETS[index]),
                0.0,
            );
            close(settled.velocities[index], MotionPoint::default(), 0.0);
        }
    }
}
