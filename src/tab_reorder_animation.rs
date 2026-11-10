//! Whole-label tab motion. Layout and drag thresholds remain immediate; only
//! the visible labels move, and pointer clicks use the last presented frame.
use super::{App, TabBarHits, CloseOverlay, close_overlay, strip_scroll_position};
use crate::animation_config::AnimationEffect;
use ratatui::{Frame, buffer::Buffer, layout::Rect, style::Style, text::Line, widgets::Paragraph};
use std::{
    ops::Range,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub(super) struct Label {
    pub id: u64,
    pub index: usize,
    pub x: i32,
    pub width: usize,
    pub close: Option<CloseOverlay>,
    pub line: Line<'static>,
    pub style: Style,
    pub icon: Option<Range<usize>>,
}

pub(super) struct Capture {
    pub area: Rect,
    pub bar: Rect,
    pub viewport: Rect,
    pub labels: Vec<Label>,
    pub fixed: Vec<Rect>,
    pub gradient: bool,
}

#[derive(Clone, Copy)]
struct Motion {
    from: f32,
    to: f32,
    velocity: f32,
    started: Instant,
    duration: Duration,
    rate: f32,
}

impl Motion {
    fn sample(self, now: Instant) -> (f32, f32) {
        let elapsed = now.saturating_duration_since(self.started);
        if elapsed >= self.duration {
            return (self.to, 0.0);
        }
        let seconds = self.duration.as_secs_f32();
        let time = elapsed.as_secs_f32();
        let progress = (time / seconds).clamp(0.0, 1.0);
        let remainder = 1.0 - progress.powi(4);
        let envelope = remainder.powi(3);
        let envelope_velocity = -12.0 * progress.powi(3) * remainder.powi(2) / seconds;
        let omega = self.rate / seconds;
        let displacement = self.from - self.to;
        let coefficient = self.velocity + omega * displacement;
        let decay = (-omega * time).exp();
        let residual = (displacement + coefficient * time) * decay;
        let speed = (self.velocity - omega * coefficient * time) * decay;
        (
            self.to + residual * envelope,
            speed * envelope + residual * envelope_velocity,
        )
    }
}

struct Track {
    label: Label,
    motion: Option<Motion>,
    visible_target: bool,
}

impl Track {
    fn sample(&self, now: Instant) -> (f32, f32) {
        self.motion
            .map_or((self.label.x as f32, 0.0), |motion| motion.sample(now))
    }
}

struct Scene {
    group: usize,
    bar: Rect,
    viewport: Rect,
    scroll: (bool, usize),
    underlay: Buffer,
    fixed: Vec<Rect>,
    backgrounds: Vec<ratatui::style::Color>,
    gradient: bool,
}

#[derive(Clone, Copy)]
struct Presented {
    id: u64,
    rect: Rect,
    close: Rect,
    clip: usize,
    width: usize,
}

#[derive(Default)]
pub(crate) struct State {
    scene: Option<Scene>,
    tracks: Vec<Track>,
    presented: Vec<Presented>,
    pending: bool,
    moved: Option<u64>,
    active: bool,
    visible: bool,
}

fn enabled(app: &App) -> bool {
    app.terminal_has_focus && app.animation_settings.enabled(AnimationEffect::TabSlide)
}

fn scroll(app: &App, group: usize) -> (bool, usize) {
    strip_scroll_position(app, group).map_or_else(
        || {
            (
                false,
                if group == app.active_group {
                    app.tab_scroll_offset
                } else {
                    app.groups
                        .get(group)
                        .map_or(0, |group| group.tab_scroll_offset)
                },
            )
        },
        |scroll| (true, scroll.columns),
    )
}

fn clip(x: i32, width: usize, viewport: Rect) -> (Rect, usize) {
    let first = i64::from(x).max(i64::from(viewport.x));
    let end = (i64::from(x) + width as i64).min(i64::from(viewport.right()));
    if end <= first {
        return (Rect::default(), 0);
    }
    (
        Rect::new(first as u16, viewport.y, (end - first) as u16, 1),
        (first - i64::from(x)) as usize,
    )
}

impl State {
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn request(&mut self, group: usize, moved: u64) {
        if self
            .scene
            .as_ref()
            .is_some_and(|scene| scene.group == group)
        {
            self.pending = true;
            self.moved = Some(moved);
        }
    }

    pub(crate) fn begin_frame(&mut self) {
        self.visible = false;
    }

    pub(crate) fn active(&self) -> bool {
        self.visible && self.active
    }

    pub(crate) fn advance(&mut self, now: Instant) {
        for track in &mut self.tracks {
            if track.motion.is_some_and(|motion| {
                now.saturating_duration_since(motion.started) >= motion.duration
            }) {
                track.motion = None;
            }
        }
        self.tracks
            .retain(|track| track.visible_target || track.motion.is_some());
        self.active = self.tracks.iter().any(|track| track.motion.is_some());
        if !self.active
            && let Some(scene) = &self.scene
        {
            self.presented.clear();
            for track in &self.tracks {
                let label = &track.label;
                let (rect, inset) = clip(label.x, label.width, scene.viewport);
                if rect.is_empty() {
                    continue;
                }
                let close = label.close.map_or_else(Rect::default, |close| close.rect(rect, inset));
                self.presented.push(Presented {
                    id: label.id,
                    rect,
                    close,
                    clip: inset,
                    width: label.width,
                });
            }
        }
    }

    fn capture(&mut self, frame: &mut Frame, app: &App, capture: Capture, now: Instant) {
        if !enabled(app) || capture.area.is_empty() || capture.viewport.is_empty() {
            self.clear();
            return;
        }
        let position = scroll(app, app.active_group);
        let compatible = self.scene.as_ref().is_some_and(|scene| {
            scene.group == app.active_group && scene.bar == capture.bar && scene.scroll == position
        });
        let animate = compatible && (self.pending || self.active);
        let mut previous = std::mem::take(&mut self.tracks);
        let first_index = capture
            .labels
            .iter()
            .map(|label| label.index)
            .min()
            .unwrap_or(0);
        let old_first = previous
            .iter()
            .map(|track| track.label.index)
            .min()
            .unwrap_or(0);
        let old_last = previous
            .iter()
            .map(|track| track.label.index)
            .max()
            .unwrap_or(0);
        let duration =
            Duration::from_millis(app.animation_settings.duration(AnimationEffect::TabSlide));
        let rate =
            9.0 + f32::from(app.animation_settings.intensity(AnimationEffect::TabSlide)) * 0.05;
        let motion = |from: f32, velocity: f32, to: f32| {
            ((from - to).abs() > 0.001 || velocity.abs() > 0.001).then_some(Motion {
                from,
                velocity,
                to,
                started: now,
                duration,
                rate,
            })
        };
        let visible_labels = capture.labels.len();

        let mut underlay = Buffer::empty(capture.area);
        for y in capture.area.y..capture.area.bottom() {
            for x in capture.area.x..capture.area.right() {
                underlay[(x, y)] = frame.buffer_mut()[(x, y)].clone();
            }
        }
        let gradient = &app.colors.gradients.bg_tabbar;
        let computed =
            crate::app_theme::GradientPrecomputed::new(capture.area.width, 1, gradient.angle_deg);
        let backgrounds: Vec<_> = (capture.area.x..capture.area.right())
            .map(|x| {
                if capture.gradient {
                    crate::app_theme::lerp_color(
                        gradient.start,
                        gradient.end,
                        computed.t_at(x - capture.area.x, 0),
                    )
                } else {
                    app.colors.bg_tabbar
                }
            })
            .collect();
        for label in &capture.labels {
            let (rect, _) = clip(label.x, label.width, capture.viewport);
            for x in rect.x..rect.right() {
                underlay[(x, rect.y)].reset();
                underlay[(x, rect.y)].set_bg(backgrounds[usize::from(x - capture.area.x)]);
            }
        }
        for label in capture.labels {
            let old = previous
                .iter()
                .position(|track| track.label.id == label.id)
                .map(|index| previous.swap_remove(index));
            let mut next = Track {
                label,
                motion: None,
                visible_target: true,
            };
            if animate {
                let target = next.label.x as f32;
                if let Some(old) = old {
                    if old.label.x == next.label.x && old.label.width == next.label.width {
                        next.motion = old.motion;
                    } else {
                        let (from, velocity) = old.sample(now);
                        next.motion = motion(from, velocity, target);
                    }
                } else {
                    let left = next.label.index < old_first
                        || (next.label.index <= old_last
                            && next.label.x
                                < i32::from(capture.viewport.x + capture.viewport.width / 2));
                    let from = if left {
                        i32::from(capture.viewport.x) - next.label.width as i32
                    } else {
                        i32::from(capture.viewport.right())
                    };
                    next.motion = motion(from as f32, 0.0, target);
                }
            }
            self.tracks.push(next);
        }
        if animate {
            for mut old in previous {
                let Some(index) = app
                    .tabs
                    .iter()
                    .position(|tab| tab.editor.navigation_id == old.label.id)
                else {
                    continue;
                };
                let target = if index < first_index {
                    i32::from(capture.viewport.x) - old.label.width as i32
                } else {
                    i32::from(capture.viewport.right())
                };
                let (from, velocity) = old.sample(now);
                let retarget = old.visible_target || old.label.x != target;
                old.label.index = index;
                old.label.x = target;
                old.visible_target = false;
                if retarget {
                    old.motion = motion(from, velocity, target as f32);
                }
                self.tracks.push(old);
            }
        }
        // A burst can bring many different tabs through one viewport before
        // time advances. Bound outgoing labels to one extra viewport.
        let mut outgoing = 0;
        self.tracks.retain(|track| {
            if track.visible_target {
                return true;
            }
            if clip(
                track.sample(now).0.round() as i32,
                track.label.width,
                capture.viewport,
            )
            .0
            .is_empty()
            {
                return false;
            }
            if Some(track.label.id) == self.moved {
                return true;
            }
            outgoing += 1;
            outgoing <= visible_labels
        });
        self.tracks
            .sort_by_key(|track| (Some(track.label.id) == self.moved, track.label.index));
        self.scene = Some(Scene {
            group: app.active_group,
            bar: capture.bar,
            viewport: capture.viewport,
            scroll: position,
            underlay,
            fixed: capture.fixed,
            backgrounds,
            gradient: capture.gradient,
        });
        self.pending = false;
        self.advance(now);
        self.visible = true;
    }

    pub(super) fn apply_hits(&self, app: &App, area: Rect, group: usize, hits: &mut TabBarHits) {
        if !enabled(app) || app.dragging_tab.is_some() || (!self.active && !self.pending) {
            return;
        }
        let Some(_) = self.scene.as_ref().filter(|scene| {
            scene.group == group && scene.bar == area && scene.scroll == scroll(app, group)
        }) else {
            return;
        };
        let tabs = if group == app.active_group {
            &app.tabs
        } else {
            &app.groups[group].tabs
        };
        hits.tabs.clear();
        hits.tab_clips.clear();
        hits.tab_widths.clear();
        for presented in self.presented.iter().rev() {
            if let Some(index) = tabs
                .iter()
                .position(|tab| tab.editor.navigation_id == presented.id)
            {
                hits.tabs.push((index, presented.rect, presented.close));
                hits.tab_clips.push(presented.clip);
                hits.tab_widths.push(presented.width);
            }
        }
    }

    fn paint(
        &mut self,
        frame: &mut Frame,
        app: &App,
        bar: Rect,
        now: Instant,
    ) -> Option<Option<Rect>> {
        if !enabled(app) {
            self.clear();
            return None;
        }
        let scene = self.scene.as_ref()?;
        if scene.group != app.active_group
            || scene.bar != bar
            || scene.scroll != scroll(app, app.active_group)
        {
            return None;
        }
        self.visible = true;
        if !self.active {
            return None;
        }
        let area = scene.underlay.area;
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                frame.buffer_mut()[(x, y)] = scene.underlay[(x, y)].clone();
            }
        }
        self.presented.clear();
        let mut active = None;
        let active_id = app.editor().map(|editor| editor.navigation_id);
        for track in &self.tracks {
            let label = &track.label;
            let x = track.sample(now).0.round() as i32;
            let (rect, inset) = clip(x, label.width, scene.viewport);
            if rect.is_empty() {
                continue;
            }
            frame.render_widget(
                Paragraph::new(label.line.clone())
                    .scroll((0, crate::helpers::len_as_u16(inset)))
                    .style(label.style),
                rect,
            );
            let icon = label
                .icon
                .as_ref()
                .map(|icon| clip(x + icon.start as i32, icon.len(), scene.viewport).0);
            if scene.gradient {
                for column in rect.x..rect.right() {
                    let cell = &mut frame.buffer_mut()[(column, rect.y)];
                    if cell.bg == app.colors.bg_tabbar || cell.bg == app.colors.bg_tab_inactive {
                        cell.set_bg(scene.backgrounds[usize::from(column - area.x)]);
                    }
                }
                crate::ui_colors::ensure_control_row_contrast_except(
                    frame.buffer_mut(),
                    rect,
                    &app.colors.ui,
                    icon.as_slice(),
                );
            }
            let close = label.close.map_or_else(Rect::default, |close| close.rect(rect, inset));
            let gap = label.close.map_or_else(Rect::default, |close| close.gap_rect(rect, inset));
            close_overlay::paint(frame, app, rect, close, gap, label.style);
            self.presented.push(Presented {
                id: label.id,
                rect,
                close,
                clip: inset,
                width: label.width,
            });
            if active_id == Some(label.id) {
                active = Some(rect);
            }
        }
        for rect in &scene.fixed {
            let rect = rect.intersection(area);
            for y in rect.y..rect.bottom() {
                for x in rect.x..rect.right() {
                    frame.buffer_mut()[(x, y)] = scene.underlay[(x, y)].clone();
                }
            }
        }
        Some(active)
    }
}

pub(super) fn capture(frame: &mut Frame, app: &App, capture: Capture, now: Instant) {
    app.animations
        .tab_reorder
        .borrow_mut()
        .capture(frame, app, capture, now);
}

pub(super) fn paint(
    frame: &mut Frame,
    app: &App,
    bar: Rect,
    active: Option<Rect>,
    now: Instant,
) -> Option<Rect> {
    app.animations
        .tab_reorder
        .borrow_mut()
        .paint(frame, app, bar, now)
        .unwrap_or(active)
}

#[cfg(test)]
#[path = "tab_reorder_animation_tests.rs"]
mod tests;
