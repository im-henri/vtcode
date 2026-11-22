//! Idle overview for the minimap. At rest the minimap shows the whole
//! document; scrolling zooms it into the usual detail window, holds there
//! while the viewport keeps moving, and zooms back out once it settles.
//!
//! The clock is a few `Instant`s per editor. It costs nothing while idle: the
//! only wakeups are one at the end of the hold and the frames of each short
//! transition. Painting itself stays bounded by the minimap height, because
//! every row queries sorted indexes instead of walking the lines it covers.
use std::cell::Cell;
use std::time::{Duration, Instant};

pub(super) const ZOOM_IN: Duration = Duration::from_millis(150);
pub(super) const HOLD: Duration = Duration::from_millis(600);
pub(super) const ZOOM_OUT: Duration = Duration::from_millis(220);
/// Transition frames; the event loop's pacing still caps the real rate.
const FRAME: Duration = Duration::from_millis(16);

thread_local! {
    // Earliest repaint any minimap asked for in the last full frame. The
    // event loop takes it before each full frame, so a hidden minimap never
    // keeps a stale deadline alive.
    static WAKE: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// The deadline at which a minimap transition needs its next full frame.
pub(crate) fn zoom_wake() -> Option<Instant> {
    WAKE.get()
}

/// Forget the armed deadline; painting the minimap arms it again if needed.
pub(crate) fn take_zoom_wake() -> Option<Instant> {
    WAKE.take()
}

fn arm(at: Instant) {
    WAKE.set(Some(WAKE.get().map_or(at, |current| current.min(at))));
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Motion {
    anchor: Instant,
    from: f32,
    hold_end: Instant,
}

impl Motion {
    fn rise(&self, at: Instant) -> f32 {
        let elapsed = at.saturating_duration_since(self.anchor).as_secs_f32();
        (self.from + elapsed / ZOOM_IN.as_secs_f32()).min(1.0)
    }

    /// Linear progress: 0 = whole document, 1 = detail window.
    fn level(&self, now: Instant) -> f32 {
        if now <= self.hold_end {
            return self.rise(now);
        }
        let falling = now.saturating_duration_since(self.hold_end).as_secs_f32();
        (self.rise(self.hold_end) - falling / ZOOM_OUT.as_secs_f32()).max(0.0)
    }

    fn next_wake(&self, now: Instant) -> Option<Instant> {
        let level = self.level(now);
        if level <= 0.0 && now > self.hold_end {
            None
        } else if level >= 1.0 && now < self.hold_end {
            Some(self.hold_end)
        } else {
            Some(now + FRAME)
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct MinimapZoom {
    enabled: bool,
    position: Option<(usize, usize)>,
    motion: Option<Motion>,
    frozen: Option<f32>,
    /// Eased level of the last paint. Hit testing uses exactly this geometry.
    painted: Option<f32>,
}

fn ease(level: f32) -> f32 {
    let t = level.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

impl MinimapZoom {
    fn linear(&self, now: Instant) -> f32 {
        if let Some(level) = self.frozen {
            return level;
        }
        self.motion.map_or(0.0, |motion| motion.level(now))
    }

    /// Advance the clock for one paint and return the eased zoom level.
    /// `position` is the viewport scroll; a change counts as scrolling.
    /// `held` freezes the level while the slider is dragged, so the mouse and
    /// the painted geometry agree for the whole drag.
    pub(super) fn advance(
        &mut self,
        enabled: bool,
        position: (usize, usize),
        held: bool,
        zoomable: bool,
        now: Instant,
    ) -> f32 {
        if enabled != self.enabled {
            *self = Self {
                enabled,
                ..Self::default()
            };
        }
        if !enabled {
            self.painted = None;
            return 1.0;
        }
        let moved = self.position.is_some_and(|old| old != position);
        self.position = Some(position);
        if held {
            let level = self.frozen.unwrap_or_else(|| self.linear(now));
            self.frozen = Some(level);
        } else if let Some(level) = self.frozen.take() {
            // Release: stay in detail through the hold, or settle outward
            // from wherever the drag left it.
            let hold_end = if level >= 1.0 { now + HOLD } else { now };
            self.motion = Some(Motion {
                anchor: now,
                from: level,
                hold_end,
            });
        } else if moved {
            self.motion = Some(Motion {
                anchor: now,
                from: self.linear(now),
                hold_end: now + HOLD,
            });
        }
        if let Some(motion) = self.motion
            && self.frozen.is_none()
        {
            match motion.next_wake(now) {
                Some(at) if zoomable => arm(at),
                Some(_) => {}
                None => self.motion = None,
            }
        }
        let level = ease(self.linear(now));
        self.painted = Some(level);
        level
    }

    /// The level used by the most recent paint (detail when disabled).
    pub(super) fn painted(&self) -> f32 {
        self.painted.unwrap_or(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    #[test]
    fn rests_on_the_overview_and_zooms_in_on_scroll() {
        let start = Instant::now();
        let mut zoom = MinimapZoom::default();
        assert_eq!(zoom.advance(true, (0, 0), false, true, start), 0.0);
        assert_eq!(zoom.advance(true, (0, 0), false, true, start + ms(500)), 0.0);
        let t = start + ms(1000);
        assert_eq!(zoom.advance(true, (3, 0), false, true, t), 0.0);
        let halfway = zoom.advance(true, (3, 0), false, true, t + ms(75));
        assert!(halfway > 0.2 && halfway < 0.8, "{halfway}");
        assert_eq!(zoom.advance(true, (3, 0), false, true, t + ms(150)), 1.0);
        // Held for HOLD after the last scroll, then settles outward.
        assert_eq!(zoom.advance(true, (3, 0), false, true, t + ms(590)), 1.0);
        let out = zoom.advance(true, (3, 0), false, true, t + ms(700));
        assert!(out > 0.0 && out < 1.0, "{out}");
        assert_eq!(zoom.advance(true, (3, 0), false, true, t + ms(900)), 0.0);
        assert_eq!(zoom.motion, None);
    }

    #[test]
    fn continued_scrolling_extends_the_hold_and_reverses_smoothly() {
        let t = Instant::now();
        let mut zoom = MinimapZoom::default();
        zoom.advance(true, (0, 0), false, true, t);
        zoom.advance(true, (1, 0), false, true, t);
        for step in 1..20 {
            assert!(zoom.advance(true, (1 + step, 0), false, true, t + ms(step as u64 * 100)) > 0.0);
        }
        assert_eq!(zoom.advance(true, (20, 0), false, true, t + ms(2300)), 1.0);
        // Zooming out halfway, then scrolling again starts from that level.
        let out_mid = t + ms(1900 + 600 + 110);
        let before = zoom.advance(true, (20, 0), false, true, out_mid);
        let after = zoom.advance(true, (21, 0), false, true, out_mid);
        assert_eq!(before, after);
        assert!(zoom.advance(true, (21, 0), false, true, out_mid + ms(40)) > after);
    }

    #[test]
    fn dragging_freezes_the_level_and_disabled_is_always_detail() {
        let t = Instant::now();
        let mut zoom = MinimapZoom::default();
        zoom.advance(true, (0, 0), false, true, t);
        assert_eq!(zoom.advance(true, (40, 0), true, true, t + ms(10)), 0.0);
        assert_eq!(zoom.advance(true, (80, 0), true, true, t + ms(900)), 0.0);
        assert_eq!(zoom.advance(true, (80, 0), false, true, t + ms(950)), 0.0);
        assert_eq!(zoom.motion.unwrap().next_wake(t + ms(1300)), None);
        let mut off = MinimapZoom::default();
        assert_eq!(off.advance(false, (0, 0), false, true, t), 1.0);
        assert_eq!(off.advance(false, (9, 0), false, true, t), 1.0);
        assert_eq!(off.painted(), 1.0);
    }

    #[test]
    fn idle_minimap_arms_no_wakeups() {
        let t = Instant::now();
        take_zoom_wake();
        let mut zoom = MinimapZoom::default();
        zoom.advance(true, (0, 0), false, true, t);
        zoom.advance(true, (0, 0), false, true, t + ms(5));
        assert_eq!(take_zoom_wake(), None);
        zoom.advance(true, (2, 0), false, true, t + ms(10));
        assert!(take_zoom_wake().is_some());
        // During the hold only one wake at its end is needed.
        zoom.advance(true, (2, 0), false, true, t + ms(300));
        assert_eq!(take_zoom_wake(), Some(t + ms(10) + HOLD));
        // A file that fits needs no transition frames at all.
        zoom.advance(true, (3, 0), false, false, t + ms(400));
        assert_eq!(take_zoom_wake(), None);
    }
}
