//! Brief copy feedback. State is constant-sized; painting uses the existing
//! viewport selection renderer, never the selected source text or its lines.
use std::time::{Duration, Instant};

use crate::{app::App, ui_colors::EditorSelectionStyle};

pub(crate) const DEFAULT_DURATION_MS: u64 = 220;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Key {
    view: u64,
    version: u64,
    cursor: (usize, usize),
    anchor: Option<(usize, usize)>,
    extra: (usize, usize),
    diff: Option<(usize, u8, usize, usize, usize, usize)>,
}

struct Pulse {
    key: Key,
    started: Instant,
    duration: Duration,
    last_frame: Instant,
}

pub(crate) struct CopyAnimation {
    pub(crate) enabled: bool,
    pub(crate) duration_ms: u64,
    pulse: Option<Pulse>,
    visible: bool,
}

impl Default for CopyAnimation {
    fn default() -> Self {
        Self {
            enabled: true,
            duration_ms: DEFAULT_DURATION_MS,
            pulse: None,
            visible: false,
        }
    }
}

impl CopyAnimation {
    pub(crate) fn clear(&mut self) {
        self.pulse = None;
        self.visible = false;
    }

    pub(crate) fn active(&self) -> bool {
        self.pulse.is_some()
    }

    pub(crate) fn observe(&mut self, style: &EditorSelectionStyle) {
        self.visible |= style.copy_feedback_visible();
    }

    fn start(&mut self, key: Key, now: Instant) {
        self.clear();
        if self.enabled {
            self.pulse = Some(Pulse {
                key,
                started: now,
                last_frame: now,
                duration: Duration::from_millis(
                    crate::animation_config::clamp_animation_duration_ms(self.duration_ms),
                ),
            });
        }
    }

    fn fade(&self, now: Instant) -> Option<f32> {
        let pulse = self.pulse.as_ref()?;
        let progress = now.saturating_duration_since(pulse.started).as_secs_f32()
            / pulse.duration.as_secs_f32();
        if progress >= 1.0 {
            return None;
        }
        // A brisk fade-out, followed by a softer return. Smoothstep keeps both
        // endpoints still and does not need trigonometry or per-cell timing.
        let t = if progress < 0.35 {
            progress / 0.35
        } else {
            (1.0 - progress) / 0.65
        };
        Some(t * t * (3.0 - 2.0 * t))
    }
}

impl App {
    fn copy_animation_key(&self) -> Option<Key> {
        let editor = self.editor()?;
        if self.settings_editor_active
            || editor.markdown_preview_scroll.is_some()
            || editor.bg_loading
        {
            return None;
        }
        Some(Key {
            view: editor.navigation_id,
            version: editor.content_version,
            cursor: (editor.cursor_line, editor.cursor_col),
            anchor: editor.selection_anchor,
            extra: (
                editor.extra_cursors.len(),
                editor.extra_cursor_anchors.len(),
            ),
            diff: self
                .diff_selection
                .copy_animation_key()
                .filter(|_| self.has_diff_selection()),
        })
    }

    pub(crate) fn start_copy_animation(&mut self, now: Instant) {
        if !self.terminal_has_focus {
            return;
        }
        let Some(key) = self.copy_animation_key() else {
            return;
        };
        if key.anchor.is_some_and(|anchor| anchor != key.cursor)
            || key.extra.1 != 0
            || key.diff.is_some()
        {
            self.copy_animation.start(key, now);
        }
    }

    pub(crate) fn begin_copy_animation_frame(&mut self, now: Instant) {
        if self.copy_animation.pulse.as_ref().is_some_and(|pulse| {
            !self.copy_animation.enabled
                || !self.terminal_has_focus
                || Some(pulse.key) != self.copy_animation_key()
                || now.saturating_duration_since(pulse.started) >= pulse.duration
        }) {
            self.copy_animation.clear();
        }
        self.copy_animation.visible = false;
    }

    pub(crate) fn copy_animation_selection_style(&self) -> EditorSelectionStyle {
        let mut style = EditorSelectionStyle::new(&self.colors);
        if !self.rendering_inactive_editor
            && self.terminal_has_focus
            && self.copy_animation.enabled
            && let Some(pulse) = &self.copy_animation.pulse
            && Some(pulse.key) == self.copy_animation_key()
            && let Some(fade) = self.copy_animation.fade(Instant::now())
        {
            style = style.with_copy_feedback(self.colors.bg_editor, fade);
        }
        style
    }

    pub(crate) fn copy_animation_next_wait(&self, now: Instant) -> Option<Duration> {
        let pulse = self.copy_animation.pulse.as_ref()?;
        if !self.copy_animation.enabled
            || !self.copy_animation.visible
            || !self.terminal_has_focus
            || Some(pulse.key) != self.copy_animation_key()
        {
            return None;
        }
        if now.saturating_duration_since(pulse.started) >= pulse.duration {
            return Some(Duration::ZERO);
        }
        let interval =
            Duration::from_secs_f64(1.0 / self.animation_settings.fps.clamp(15, 120) as f64);
        Some(
            interval
                .saturating_sub(now.saturating_duration_since(pulse.last_frame))
                .max(self.animations.background_frame_wait(now)),
        )
    }

    pub(crate) fn advance_copy_animation(&mut self, now: Instant) -> bool {
        if !self
            .copy_animation_next_wait(now)
            .is_some_and(|wait| wait.is_zero())
        {
            return false;
        }
        let pulse = self.copy_animation.pulse.as_mut().unwrap();
        pulse.last_frame = now;
        if now.saturating_duration_since(pulse.started) >= pulse.duration {
            self.copy_animation.clear();
        }
        true
    }
}

#[cfg(test)]
#[path = "copy_animation_tests.rs"]
mod tests;
