//! Finite cursor fire with deterministic emission and analytic particle motion.
//!
//! The emission clock is independent of rendering cadence. Retargeting changes
//! the moving source without discarding the embers it already left behind.
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use crate::{
    animation_config::{AnimationEffect, AnimationSettings},
    animations::MotionPoint,
    cursor_smear::{SmearMotion, SmearSample},
};

const EMISSION_STEP: Duration = Duration::from_millis(8);
const MAX_LIFETIME: Duration = Duration::from_millis(1_120);
const COOL_DOWN: Duration = Duration::from_millis(60);
const MAX_PARTICLES: usize = 768;
const MAX_CATCH_UP_STEPS: usize = 144;
const MAX_BIRTHS_PER_STEP: usize = 18;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum FireParticleKind {
    Flame,
    Ember,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FireParticle {
    pub kind: FireParticleKind,
    /// Terminal cell coordinates. One row occupies two physical column units.
    pub position: MotionPoint,
    pub velocity: MotionPoint,
    pub heat: f32,
    pub opacity: f32,
    /// Radius in physical column units, rather than terminal rows.
    pub radius: f32,
    pub seed: u32,
}

#[derive(Clone, Copy)]
struct ParticleMotion {
    kind: FireParticleKind,
    position: MotionPoint,
    /// Physical units: upward velocity is negative, and a row is two units.
    velocity: MotionPoint,
    lifetime: f32,
    radius: f32,
    heat: f32,
    drag: f32,
    inverse_drag: f32,
    inverse_lifetime: f32,
    buoyancy: f32,
    curl: f32,
    frequency: f32,
    phase: f32,
    seed: u32,
}

impl ParticleMotion {
    fn sample(self, age: f32) -> Option<FireParticle> {
        if age >= self.lifetime {
            return None;
        }
        let progress = age * self.inverse_lifetime;
        let decay = (-self.drag * age).exp();
        let integral = (1.0 - decay) * self.inverse_drag;
        // Damped acceleration has an exact integral. It remains smooth even
        // when a render skips several frames or the cursor changes direction.
        let rise = self.buoyancy * (age - integral) * self.inverse_drag;
        let rise_velocity = self.buoyancy * integral;
        let opening_decay = (-5.0 * age).exp();
        let opening = 1.0 - opening_decay;
        let opening_velocity = 5.0 * opening_decay;
        let angle = self.phase + self.frequency * age;
        // A broad curl and a finer ripple break up the plume without any
        // frame-dependent randomness. Both velocities are analytic too.
        let ripple = self.phase * 1.7 + self.frequency * 2.3 * age;
        let (angle_sin, angle_cos) = angle.sin_cos();
        let (ripple_sin, ripple_cos) = ripple.sin_cos();
        let wave = angle_sin + ripple_sin * 0.28;
        let wave_velocity = self.frequency * (angle_cos + ripple_cos * 0.644);
        let curl = self.curl * opening * wave;
        let curl_velocity = self.curl * (opening_velocity * wave + opening * wave_velocity);
        let position = MotionPoint {
            x: self.position.x + self.velocity.x * integral + curl,
            y: self.position.y + (self.velocity.y * integral + rise) * 0.5,
        };
        let velocity = MotionPoint {
            x: self.velocity.x * decay + curl_velocity,
            y: (self.velocity.y * decay + rise_velocity) * 0.5,
        };
        // Warm cells cool continuously toward red. Each flame tongue flickers
        // on its own timeline instead of flickering anew on each rendered frame.
        let ember = self.kind == FireParticleKind::Ember;
        let flicker =
            0.88 + 0.09 * (self.phase + age * self.frequency * 1.7).sin() + 0.03 * ripple_sin;
        let cooling = if ember { 0.30 } else { 0.65 };
        let heat = (self.heat * (1.0 - progress).powf(cooling) * flicker).clamp(0.0, 1.0);
        let fade_start = if ember { 0.40 } else { 0.22 };
        let fade = 1.0 - smoothstep(((progress - fade_start) / (1.0 - fade_start)).clamp(0.0, 1.0));
        let opacity = (fade * (0.76 + self.heat * 0.24) * flicker).clamp(0.0, 1.0);
        let radius = self.radius * (0.72 + opening * 0.4) * (1.0 - progress * 0.55);
        Some(FireParticle {
            kind: self.kind,
            position,
            velocity,
            heat,
            opacity,
            radius,
            seed: self.seed,
        })
    }
}

#[derive(Clone, Copy)]
struct Birth {
    at: Instant,
    expires: Instant,
    motion: ParticleMotion,
}

impl Birth {
    fn sample(self, now: Instant) -> Option<FireParticle> {
        if now < self.at {
            return None;
        }
        self.motion
            .sample(now.duration_since(self.at).as_secs_f32())
    }
}

#[derive(Clone, Copy)]
struct Emitter {
    motion: SmearMotion,
    started: Instant,
    editing: bool,
    generation: u32,
}

#[derive(Clone, Copy)]
struct EmissionSample {
    at: Instant,
    duration: Duration,
    sample: SmearSample,
}

#[derive(Default)]
pub(crate) struct FireTrail {
    particles: VecDeque<Birth>,
    emitter: Option<Emitter>,
    next_emission: Option<Instant>,
    tick: u64,
    generation: u32,
    last_emission: Option<EmissionSample>,
    next_expiry: Option<Instant>,
}

impl FireTrail {
    pub(crate) fn retarget_cursor(
        &mut self,
        origin: MotionPoint,
        target: MotionPoint,
        started: Instant,
        editing: bool,
        settings: &AnimationSettings,
    ) {
        // Fire owns its motion clock, including when the smear effect has a
        // different duration. A turn inherits the fire core's actual shape.
        let previous = self.core(settings, started);
        self.retarget(
            SmearMotion::retarget(origin, target, previous, editing),
            started,
            editing,
            settings,
        );
    }

    pub(crate) fn core(&self, settings: &AnimationSettings, now: Instant) -> Option<SmearSample> {
        let emitter = self.emitter?;
        let elapsed = now.saturating_duration_since(emitter.started);
        (elapsed < motion_duration(settings)).then(|| {
            emitter
                .motion
                .sample_for_effect(settings, AnimationEffect::CursorFire, elapsed)
        })
    }

    pub(crate) fn retarget(
        &mut self,
        motion: SmearMotion,
        started: Instant,
        editing: bool,
        settings: &AnimationSettings,
    ) {
        self.advance(started, settings);
        self.generation = self.generation.wrapping_add(1);
        self.emitter = Some(Emitter {
            motion,
            started,
            editing,
            generation: self.generation,
        });
        self.last_emission = None;
        self.next_emission.get_or_insert(started + EMISSION_STEP);
    }

    pub(crate) fn advance(&mut self, now: Instant, settings: &AnimationSettings) {
        self.prune(now);
        let Some(mut next) = self.next_emission else {
            return;
        };
        // Editor observation and animation advancement can share a timestamp.
        // A frame without a due emission does not need to copy the emitter or
        // consult settings after the cheap expiry check.
        if next > now {
            return;
        }
        let Some(emitter) = self.emitter else {
            return;
        };
        let end = emitter.started + motion_duration(settings) + COOL_DOWN;
        // Births older than every possible particle can be skipped exactly.
        // Each tick owns its randomness, so skipping does not alter later fire.
        if let Some(oldest_visible) = now.checked_sub(MAX_LIFETIME)
            && next < oldest_visible
        {
            let skipped = oldest_visible.duration_since(next).as_nanos() / EMISSION_STEP.as_nanos();
            let skipped = skipped.min(u64::MAX as u128) as u64;
            next += tick_duration(skipped);
            self.tick = self.tick.wrapping_add(skipped);
        }
        let mut steps = 0;
        while next <= now && next <= end && steps < MAX_CATCH_UP_STEPS {
            if next >= emitter.started {
                self.emit(&emitter, next, self.tick, settings);
            }
            next += EMISSION_STEP;
            self.tick = self.tick.wrapping_add(1);
            steps += 1;
        }
        if now > end && next <= now {
            // Keep the shared clock aligned when another movement arrives
            // after a long idle; do not replay the idle interval on retarget.
            let skipped = now.duration_since(next).as_nanos() / EMISSION_STEP.as_nanos() + 1;
            let skipped = skipped.min(u64::MAX as u128) as u64;
            next += tick_duration(skipped);
            self.tick = self.tick.wrapping_add(skipped);
        }
        self.next_emission = Some(next);
        self.prune(now);
    }

    /// Stream samples directly into the renderer or a caller-owned buffer.
    /// Keeping the iterator lazy avoids an intermediate allocation per frame.
    pub(crate) fn sample_iter(&self, now: Instant) -> impl Iterator<Item = FireParticle> + '_ {
        self.particles
            .iter()
            .filter_map(move |birth| birth.sample(now))
    }

    #[cfg(test)]
    pub(crate) fn sample(&self, now: Instant) -> Vec<FireParticle> {
        self.sample_iter(now).collect()
    }

    pub(crate) fn is_active(&self, now: Instant, settings: &AnimationSettings) -> bool {
        self.emitter
            .is_some_and(|emitter| now < emitter.started + motion_duration(settings) + COOL_DOWN)
            || self
                .particles
                .iter()
                .any(|birth| now >= birth.at && now < birth.expires)
    }

    pub(crate) fn clear(&mut self) {
        // Cursor/view switches frequently clear and restart a trail. Preserve
        // its bounded allocation rather than freeing and regrowing it.
        self.particles.clear();
        self.emitter = None;
        self.next_emission = None;
        self.last_emission = None;
        self.next_expiry = None;
        self.tick = 0;
        self.generation = 0;
    }

    fn prune(&mut self, now: Instant) {
        if self.next_expiry.is_none_or(|next| now < next) {
            return;
        }
        let mut next = None;
        self.particles.retain(|birth| {
            if now >= birth.expires {
                return false;
            }
            next = Some(next.map_or(birth.expires, |at: Instant| at.min(birth.expires)));
            true
        });
        self.next_expiry = next;
    }

    fn emit(&mut self, emitter: &Emitter, at: Instant, tick: u64, settings: &AnimationSettings) {
        let elapsed = at.saturating_duration_since(emitter.started);
        let before = elapsed.saturating_sub(EMISSION_STEP);
        let motion_duration = motion_duration(settings);
        let previous = self
            .last_emission
            .filter(|cached| cached.at + EMISSION_STEP == at && cached.duration == motion_duration)
            .map_or_else(
                || {
                    emitter
                        .motion
                        .sample_for_effect(settings, AnimationEffect::CursorFire, before)
                },
                |cached| cached.sample,
            );
        let current =
            emitter
                .motion
                .sample_for_effect(settings, AnimationEffect::CursorFire, elapsed);
        self.last_emission = Some(EmissionSample {
            at,
            duration: motion_duration,
            sample: current,
        });
        let distance = current
            .corners
            .iter()
            .zip(previous.corners)
            .map(|(a, b)| physical_length(sub(*a, b)))
            .sum::<f32>()
            * 0.25;
        if !distance.is_finite()
            || !current
                .corners
                .iter()
                .chain(&current.velocities)
                .all(|point| finite(*point))
        {
            return;
        }
        let intensity = settings.intensity(AnimationEffect::CursorFire).min(100) as f32 / 100.0;
        let duration = motion_duration.as_secs_f32();
        let fade_start = duration * 0.45;
        let fade_end = duration + COOL_DOWN.as_secs_f32();
        let envelope = 1.0
            - smoothstep(
                ((elapsed.as_secs_f32() - fade_start) / (fade_end - fade_start)).clamp(0.0, 1.0),
            );
        let editing = if emitter.editing { 0.47 } else { 1.0 };
        // A single step should ignite promptly, then leave cooling embers.
        // Follow the actual swept speed so settling does not keep spraying the
        // same stationary cell at the rate used for a fast navigation jump.
        let activity = smoothstep(
            (distance / EMISSION_STEP.as_secs_f32() / 40.0)
                .sqrt()
                .clamp(0.0, 1.0),
        );
        let expected =
            ((130.0 + 160.0 * intensity) * EMISSION_STEP.as_secs_f32() * (0.12 + 0.88 * activity)
                + distance.min(24.0) * (1.4 + 2.2 * intensity))
                * editing
                * envelope;
        let tick_seed = hash(
            emitter.generation.wrapping_mul(0x9e37_79b9)
                ^ tick as u32
                ^ ((tick >> 32) as u32).rotate_left(11),
        );
        let count = (expected.floor() as usize
            + usize::from(random(tick_seed ^ 0xa73c_14d9) < expected.fract()))
        .min(MAX_BIRTHS_PER_STEP);
        for index in 0..count {
            let seed = hash(tick_seed ^ (index as u32).wrapping_mul(0x85eb_ca6b));
            let randoms: [f32; 12] = std::array::from_fn(|channel| {
                random(seed ^ (channel as u32 + 1).wrapping_mul(0xc2b2_ae35))
            });
            let along_time = (index as f32 + randoms[0]) / count.max(1) as f32;
            // Stratification places particles along the swept cursor shape.
            // Very large jumps use only a bounded piece nearest the new shape.
            let along_time = if distance > 24.0 {
                1.0 - (1.0 - along_time) * 24.0 / distance
            } else {
                along_time
            };
            let ember = randoms[3] > 0.55;
            let horizontal = randoms[1];
            // Let tongues and sparks peel off the upper edge, where they are
            // visible immediately instead of spending their early life hidden
            // underneath the solid cursor ribbon.
            let vertical = if ember {
                -0.15 + randoms[2] * 0.30
            } else {
                0.02 + randoms[2] * 0.48
            };
            let old_point = inside(&previous, horizontal, vertical);
            let new_point = inside(&current, horizontal, vertical);
            let position = lerp(old_point, new_point, along_time);
            let velocity = inside_velocity(&current, horizontal, vertical);
            let speed = physical_length(velocity);
            let energy = (speed / 120.0).sqrt().clamp(0.0, 1.4);
            let momentum_limit = if emitter.editing {
                2.2
            } else if ember {
                10.0
            } else {
                6.0
            };
            let inherited = cap(
                MotionPoint {
                    x: velocity.x * 0.055,
                    y: velocity.y * 0.11,
                },
                momentum_limit,
            );
            let scale = if emitter.editing { 0.62 } else { 1.0 };
            let lifetime = if emitter.editing {
                0.24 + randoms[4] * 0.28
            } else if ember {
                0.64 + randoms[4] * 0.48
            } else {
                0.28 + randoms[4] * 0.34
            };
            let radius = if ember {
                0.08 + randoms[5] * 0.12
            } else {
                0.30 + randoms[5] * 0.44
            } * (0.75 + intensity * 0.3)
                * scale;
            let drag = if ember {
                1.4 + randoms[9] * 1.4
            } else {
                2.2 + randoms[9] * 2.0
            };
            let expires = at + Duration::from_secs_f32(lifetime);
            self.next_expiry = Some(self.next_expiry.map_or(expires, |next| next.min(expires)));
            self.particles.push_back(Birth {
                at,
                expires,
                motion: ParticleMotion {
                    kind: if ember {
                        FireParticleKind::Ember
                    } else {
                        FireParticleKind::Flame
                    },
                    position,
                    velocity: MotionPoint {
                        x: inherited.x
                            + (randoms[6] - 0.5)
                                * (if ember { 5.0 } else { 2.5 } + energy * 2.0)
                                * scale,
                        y: inherited.y - (2.5 + randoms[7] * 4.0 + energy * 1.5) * scale,
                    },
                    lifetime,
                    radius,
                    heat: if ember {
                        0.65 + randoms[8] * 0.30
                    } else {
                        0.82 + randoms[8] * 0.18
                    },
                    drag,
                    inverse_drag: drag.recip(),
                    inverse_lifetime: lifetime.recip(),
                    buoyancy: -(if ember { 10.0 } else { 19.0 } + randoms[10] * 21.0) * scale,
                    curl: (0.2 + randoms[11] * if ember { 1.0 } else { 0.6 }) * scale,
                    frequency: 8.0 + randoms[6] * 9.0,
                    phase: randoms[7] * std::f32::consts::TAU,
                    seed,
                },
            });
            if self.particles.len() > MAX_PARTICLES {
                self.particles.pop_front();
            }
        }
    }
}

fn tick_duration(ticks: u64) -> Duration {
    Duration::from_secs(ticks / 125) + Duration::from_millis((ticks % 125) * 8)
}

fn motion_duration(settings: &AnimationSettings) -> Duration {
    Duration::from_millis(settings.duration(AnimationEffect::CursorFire))
}

fn finite(point: MotionPoint) -> bool {
    point.x.is_finite() && point.y.is_finite()
}

fn smoothstep(value: f32) -> f32 {
    value * value * (3.0 - 2.0 * value)
}

fn hash(mut value: u32) -> u32 {
    value ^= value >> 16;
    value = value.wrapping_mul(0x7feb_352d);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846c_a68b);
    value ^ (value >> 16)
}

fn random(seed: u32) -> f32 {
    (hash(seed) >> 8) as f32 / 16_777_216.0
}

fn sub(a: MotionPoint, b: MotionPoint) -> MotionPoint {
    MotionPoint {
        x: a.x - b.x,
        y: a.y - b.y,
    }
}

fn lerp(a: MotionPoint, b: MotionPoint, fraction: f32) -> MotionPoint {
    MotionPoint {
        x: a.x + (b.x - a.x) * fraction,
        y: a.y + (b.y - a.y) * fraction,
    }
}

fn physical_length(point: MotionPoint) -> f32 {
    point.x.hypot(point.y * 2.0)
}

fn cap(point: MotionPoint, maximum: f32) -> MotionPoint {
    let length = point.x.hypot(point.y);
    let scale = if length > maximum {
        maximum / length
    } else {
        1.0
    };
    MotionPoint {
        x: point.x * scale,
        y: point.y * scale,
    }
}

fn bilinear(points: [MotionPoint; 4], horizontal: f32, vertical: f32) -> MotionPoint {
    lerp(
        lerp(points[0], points[1], horizontal),
        lerp(points[3], points[2], horizontal),
        vertical,
    )
}

fn inside(sample: &SmearSample, horizontal: f32, vertical: f32) -> MotionPoint {
    bilinear(sample.corners, horizontal, vertical)
}

fn inside_velocity(sample: &SmearSample, horizontal: f32, vertical: f32) -> MotionPoint {
    bilinear(sample.velocities, horizontal, vertical)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn motion(target: MotionPoint, editing: bool) -> SmearMotion {
        SmearMotion::retarget(MotionPoint { x: 10.0, y: 12.0 }, target, None, editing)
    }

    fn setup(target: MotionPoint, editing: bool) -> (FireTrail, AnimationSettings, Instant) {
        let settings = AnimationSettings::default();
        let start = Instant::now();
        let mut fire = FireTrail::default();
        fire.retarget(motion(target, editing), start, editing, &settings);
        (fire, settings, start)
    }

    #[test]
    fn fire_duration_and_intensity_are_independent_of_other_effect_settings() {
        let now = Instant::now();
        let mut settings = AnimationSettings::default();
        settings.set_duration(AnimationEffect::CursorFire, 800);
        settings.set_intensity(AnimationEffect::CursorFire, 25);
        let mut other_settings = settings.clone();
        for effect in [
            AnimationEffect::CursorSmear,
            AnimationEffect::WhitespaceReveal,
            AnimationEffect::ScopeConnector,
            AnimationEffect::TabSlide,
        ] {
            other_settings.set_duration(effect, 80);
            other_settings.set_intensity(effect, 100);
        }
        let target = MotionPoint { x: 44.0, y: 12.0 };
        let mut first = FireTrail::default();
        let mut second = FireTrail::default();
        first.retarget_cursor(MotionPoint::default(), target, now, false, &settings);
        second.retarget_cursor(MotionPoint::default(), target, now, false, &other_settings);
        let at = now + Duration::from_millis(200);
        first.advance(at, &settings);
        second.advance(at, &other_settings);
        assert_eq!(first.sample(at), second.sample(at));
        assert!(
            first.core(&settings, at).is_some(),
            "fire still moves after the smear duration"
        );
        let before = first.core(&settings, at).unwrap();
        first.retarget_cursor(
            MotionPoint::default(),
            MotionPoint { x: 4.0, y: 8.0 },
            at,
            false,
            &settings,
        );
        let after = first.core(&settings, at).unwrap();
        for (old, new) in before.corners.into_iter().zip(after.corners) {
            assert!((old.x - new.x).abs() < 0.0002 && (old.y - new.y).abs() < 0.0002);
        }

        let mut brighter = settings.clone();
        brighter.set_intensity(AnimationEffect::CursorFire, 100);
        let mut stronger = FireTrail::default();
        stronger.retarget_cursor(MotionPoint::default(), target, now, false, &brighter);
        stronger.advance(at, &brighter);
        assert!(
            stronger.particles.len() > second.particles.len(),
            "only fire's own intensity changes emission"
        );
    }

    #[test]
    fn fire_core_obeys_one_and_five_thousand_ms_motion_durations() {
        let started = Instant::now();
        for duration_ms in [1, 5000] {
            let mut settings = AnimationSettings::default();
            settings.set_duration(AnimationEffect::CursorFire, duration_ms);
            let mut fire = FireTrail::default();
            fire.retarget_cursor(
                MotionPoint::default(),
                MotionPoint { x: 44.0, y: 12.0 },
                started,
                false,
                &settings,
            );
            assert_eq!(
                motion_duration(&settings),
                Duration::from_millis(duration_ms)
            );
            assert!(
                fire.core(
                    &settings,
                    started + Duration::from_micros(duration_ms * 500)
                )
                .is_some()
            );
            assert!(
                fire.core(&settings, started + Duration::from_millis(duration_ms))
                    .is_none()
            );
        }
    }

    #[test]
    fn fire_emission_and_particle_motion_do_not_depend_on_render_cadence() {
        let start = Instant::now();
        let settings = AnimationSettings::default();
        let movement = motion(MotionPoint { x: 44.0, y: 10.0 }, false);
        let mut frequent = FireTrail::default();
        let mut sparse = FireTrail::default();
        frequent.retarget(movement, start, false, &settings);
        sparse.retarget(movement, start, false, &settings);
        for elapsed in 1..=700 {
            frequent.advance(start + Duration::from_millis(elapsed), &settings);
        }
        let now = start + Duration::from_millis(700);
        sparse.advance(now, &settings);
        assert_eq!(frequent.sample(now), sparse.sample(now));
        assert!(!sparse.sample(now).is_empty());
    }

    #[test]
    fn reversing_the_cursor_keeps_existing_embers_and_the_shared_emission_clock() {
        let (mut fire, settings, start) = setup(MotionPoint { x: 44.0, y: 12.0 }, false);
        let turn = start + Duration::from_millis(39);
        fire.advance(turn, &settings);
        let before = fire.sample(turn);
        assert!(before.len() > 20);
        let movement = motion(MotionPoint { x: 44.0, y: 12.0 }, false);
        let shape = movement.sample(&settings, Duration::from_millis(39));
        fire.retarget(
            SmearMotion::retarget(
                MotionPoint::default(),
                MotionPoint { x: 4.0, y: 12.0 },
                Some(shape),
                false,
            ),
            turn,
            false,
            &settings,
        );
        assert_eq!(before, fire.sample(turn));
        fire.advance(start + Duration::from_millis(40), &settings);
        assert!(fire.sample(start + Duration::from_millis(40)).len() > before.len());
    }

    #[test]
    fn fire_coasts_then_expires_completely_without_stationary_animation() {
        let (mut fire, settings, start) = setup(MotionPoint { x: 30.0, y: 12.0 }, false);
        fire.advance(start + Duration::from_millis(240), &settings);
        assert!(fire.is_active(start + Duration::from_millis(400), &settings));
        let cold = start + Duration::from_secs(2);
        fire.advance(cold, &settings);
        assert!(fire.sample(cold).is_empty());
        assert!(!fire.is_active(cold, &settings));
        assert!(fire.particles.is_empty());
        fire.clear();
        assert!(fire.emitter.is_none() && fire.next_emission.is_none());
    }

    #[test]
    fn huge_jumps_reversals_and_dropped_frames_have_bounded_work_and_live_particles() {
        let (mut fire, mut settings, start) = setup(MotionPoint { x: 1.0e9, y: 1.0e8 }, false);
        settings.duration_ms = 1_500;
        settings.intensity = 100;
        for index in 1..100 {
            let now = start + Duration::from_millis(index * 8);
            fire.retarget(
                motion(
                    MotionPoint {
                        x: if index % 2 == 0 { -1.0e9 } else { 1.0e9 },
                        y: 1.0e8,
                    },
                    false,
                ),
                now,
                false,
                &settings,
            );
            assert!(fire.particles.len() <= MAX_PARTICLES);
            assert!(
                fire.sample(now)
                    .iter()
                    .all(|particle| finite(particle.position) && finite(particle.velocity))
            );
        }
        let now = start + Duration::from_secs(60 * 60 * 24 * 365);
        fire.advance(now, &settings);
        assert!(fire.sample(now).is_empty());
        assert!(!fire.is_active(now, &settings));
    }

    #[test]
    fn flames_rise_cool_and_narrow_smoothly() {
        let (mut fire, settings, start) = setup(MotionPoint { x: 10.5, y: 12.0 }, false);
        fire.advance(start + Duration::from_millis(40), &settings);
        let birth = *fire.particles.front().unwrap();
        let initial = birth.sample(birth.at).unwrap();
        // During ignition each tongue may brighten as it flickers. Compare a
        // genuinely cooling particle after it has entered its fade interval.
        let risen = birth
            .sample(birth.at + Duration::from_secs_f32(birth.motion.lifetime * 0.7))
            .unwrap();
        assert!(risen.position.y < initial.position.y - 0.3);
        assert!(risen.velocity.y < 0.0);
        assert!(risen.heat < initial.heat);
        assert!(risen.opacity < initial.opacity);
        assert!(risen.radius < initial.radius);
    }

    #[test]
    fn typing_uses_smaller_shorter_lived_and_calmer_flames_than_navigation() {
        let (mut typing, settings, start) = setup(MotionPoint { x: 11.0, y: 12.0 }, true);
        let mut navigation = FireTrail::default();
        navigation.retarget(
            motion(MotionPoint { x: 11.0, y: 12.0 }, false),
            start,
            false,
            &settings,
        );
        let now = start + Duration::from_millis(160);
        typing.advance(now, &settings);
        navigation.advance(now, &settings);
        assert!(!typing.particles.is_empty());
        assert!(typing.particles.len() < navigation.particles.len());
        assert!(
            typing
                .particles
                .iter()
                .all(|birth| birth.motion.radius < 0.5 && birth.motion.lifetime <= 0.52)
        );
        assert!(
            typing
                .sample(now)
                .iter()
                .all(|particle| (particle.position.x - 10.5).abs() < 2.5
                    && particle.position.y > 10.0)
        );
    }

    #[test]
    fn catch_up_skipping_is_identical_to_regular_frames_after_long_emission() {
        let start = Instant::now();
        let mut settings = AnimationSettings::default();
        settings.duration_ms = 1_500;
        settings.intensity = 100;
        let movement = motion(MotionPoint { x: 45.0, y: 16.0 }, false);
        let mut frequent = FireTrail::default();
        let mut skipped = FireTrail::default();
        frequent.retarget(movement, start, false, &settings);
        skipped.retarget(movement, start, false, &settings);
        for elapsed in (8..=1_400).step_by(8) {
            frequent.advance(start + Duration::from_millis(elapsed), &settings);
        }
        let now = start + Duration::from_millis(1_400);
        skipped.advance(now, &settings);
        assert_eq!(frequent.sample(now), skipped.sample(now));
    }

    #[test]
    fn repeated_sampling_is_read_only_and_disabled_clear_resets_the_clock() {
        let (mut fire, settings, start) = setup(MotionPoint { x: 24.0, y: 10.0 }, false);
        let now = start + Duration::from_millis(40);
        fire.advance(now, &settings);
        assert_eq!(fire.sample(now), fire.sample(now));
        fire.clear();
        assert!(!fire.is_active(now, &settings));
        assert!(fire.sample(now).is_empty());
        fire.retarget(
            motion(MotionPoint { x: 25.0, y: 10.0 }, false),
            now,
            false,
            &settings,
        );
        assert!(fire.sample(now).is_empty());
        fire.advance(now + EMISSION_STEP, &settings);
        assert!(!fire.sample(now + EMISSION_STEP).is_empty());
    }

    #[test]
    fn emission_geometry_cache_invalidates_when_motion_duration_changes() {
        let (mut cached, mut settings, start) = setup(MotionPoint { x: 44.0, y: 12.0 }, false);
        let (mut uncached, _, _) = setup(MotionPoint { x: 44.0, y: 12.0 }, false);
        // Keep the source clock identical while independently removing every
        // reused sample from the reference trail.
        uncached.clear();
        uncached.retarget(
            motion(MotionPoint { x: 44.0, y: 12.0 }, false),
            start,
            false,
            &settings,
        );
        for milliseconds in (8..=160).step_by(8) {
            if milliseconds == 40 {
                settings.set_duration(AnimationEffect::CursorFire, 500);
            }
            uncached.last_emission = None;
            let now = start + Duration::from_millis(milliseconds);
            cached.advance(now, &settings);
            uncached.advance(now, &settings);
            assert_eq!(cached.sample(now), uncached.sample(now));
        }
    }

    #[test]
    fn clearing_and_restarting_fire_reuses_the_bounded_particle_allocation() {
        let (mut fire, settings, start) = setup(MotionPoint { x: 44.0, y: 12.0 }, false);
        fire.advance(start + Duration::from_millis(160), &settings);
        let capacity = fire.particles.capacity();
        assert!(capacity > 0);
        fire.clear();
        assert_eq!(fire.particles.capacity(), capacity);
        assert!(fire.next_expiry.is_none() && fire.last_emission.is_none());
        fire.retarget(
            motion(MotionPoint { x: 44.0, y: 12.0 }, false),
            start,
            false,
            &settings,
        );
        fire.advance(start + Duration::from_millis(160), &settings);
        assert_eq!(fire.particles.capacity(), capacity);
    }

    #[test]
    fn single_step_ignites_above_the_cursor_and_quiets_as_it_settles() {
        let (mut fire, settings, start) = setup(MotionPoint { x: 11.0, y: 12.0 }, false);
        fire.advance(start + Duration::from_millis(112), &settings);
        let early = fire
            .particles
            .iter()
            .filter(|birth| birth.at <= start + Duration::from_millis(56))
            .count();
        let late = fire.particles.len() - early;
        assert!(early >= 6, "a single step must visibly ignite");
        assert!(
            late * 2 < early,
            "settling must soften the plume: {early} early births, {late} late births"
        );
        let fresh = fire.sample(start + Duration::from_millis(24));
        assert!(!fresh.is_empty());
        assert!(fresh.iter().all(|particle| particle.position.y < 12.5));
        assert!(
            fresh.iter().any(|particle| particle.position.y < 12.0),
            "fine sparks must emerge above the core promptly"
        );
        let before_turn = fire.sample(start + Duration::from_millis(112));
        let shape = motion(MotionPoint { x: 11.0, y: 12.0 }, false)
            .sample(&settings, Duration::from_millis(112));
        fire.retarget(
            SmearMotion::retarget(
                MotionPoint { x: 11.0, y: 12.0 },
                MotionPoint { x: 10.0, y: 12.0 },
                Some(shape),
                false,
            ),
            start + Duration::from_millis(112),
            false,
            &settings,
        );
        assert_eq!(
            before_turn,
            fire.sample(start + Duration::from_millis(112)),
            "a direction change keeps the existing embers"
        );
    }

    #[test]
    fn navigation_emits_distinct_flames_and_lingering_fine_embers() {
        let (mut fire, settings, start) = setup(MotionPoint { x: 44.0, y: 12.0 }, false);
        let now = start + Duration::from_millis(80);
        fire.advance(now, &settings);
        let particles = fire.sample(now);
        assert!(particles.iter().any(|p| p.kind == FireParticleKind::Flame));
        assert!(particles.iter().any(|p| p.kind == FireParticleKind::Ember));
        assert!(
            fire.particles
                .iter()
                .filter(|p| p.motion.kind == FireParticleKind::Ember)
                .all(|p| p.motion.radius < 0.25 && p.motion.lifetime >= 0.64)
        );
        let late = start + Duration::from_millis(950);
        fire.advance(late, &settings);
        let embers = fire.sample(late);
        assert!(!embers.is_empty());
        assert!(embers.iter().all(|p| p.kind == FireParticleKind::Ember));
        assert!(embers.iter().any(|p| p.position.y < 10.0));
    }

    #[test]
    fn curled_particle_velocity_matches_its_continuous_path_for_both_kinds() {
        let (mut fire, settings, start) = setup(MotionPoint { x: 28.0, y: 12.0 }, false);
        fire.advance(start + Duration::from_millis(80), &settings);
        for kind in [FireParticleKind::Flame, FireParticleKind::Ember] {
            let birth = fire
                .particles
                .iter()
                .find(|p| p.motion.kind == kind)
                .unwrap();
            let age = birth.motion.lifetime * 0.3;
            let before = birth.motion.sample(age - 0.001).unwrap();
            let current = birth.motion.sample(age).unwrap();
            let after = birth.motion.sample(age + 0.001).unwrap();
            assert!(
                ((after.position.x - before.position.x) / 0.002 - current.velocity.x).abs() < 0.02
            );
            assert!(
                ((after.position.y - before.position.y) / 0.002 - current.velocity.y).abs() < 0.02
            );
        }
    }
}
