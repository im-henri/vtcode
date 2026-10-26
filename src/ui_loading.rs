//! Local, demand-driven feedback for asynchronous work. Bars decorate existing
//! headers, so neither content nor pointer targets move when work starts/stops.
//! No workers, polling, RPCs, or per-frame progress messages are needed.
use std::time::{Duration, Instant};

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
};

use crate::{
    app::{App, SidebarView},
    editor::Editor,
};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const CYCLE_DURATION: Duration = Duration::from_millis(1920);
const SUBCELL: i64 = 256;

#[derive(Default)]
pub(crate) struct State {
    visible: bool,
    animated: bool,
    started: Option<Instant>,
    sampled: Option<Instant>,
    phase_ns: u64,
    next: Option<Instant>,
    tick_in_flight: bool,
    tick_painted: bool,
}

impl State {
    pub(crate) fn begin_frame(&mut self) {
        self.visible = false;
        self.animated = false;
    }

    pub(crate) fn active(&self) -> bool {
        self.visible || self.next.is_some()
    }

    pub(crate) fn finish_frame(&mut self, now: Instant) {
        if self.visible {
            self.started.get_or_insert(now);
            if !self.animated {
                self.next = None;
            } else if self.tick_in_flight {
                // One shared phase is sampled before painting. Never schedule
                // behind that sample, including with a synthetic test clock.
                self.next = Some(self.sampled.unwrap_or(now).max(now) + FRAME_INTERVAL);
            } else {
                self.next.get_or_insert(now + FRAME_INTERVAL);
            }
        } else {
            self.next = None;
            self.started = None;
            self.sampled = None;
            self.phase_ns = 0;
        }
        self.tick_painted = self.animated && self.tick_in_flight;
        self.tick_in_flight = false;
    }

    /// Include terminal output in the quiet gap, without letting unrelated
    /// input redraws postpone loading indefinitely. Slow displays/CPUs keep
    /// headroom; cheap cached frames can update at the normal 16 ms cadence.
    pub(crate) fn record_render(&mut self, finished: Instant, elapsed: Duration) {
        if std::mem::take(&mut self.tick_painted) && self.next.is_some() {
            let gap = elapsed.min(Duration::from_millis(100)).max(FRAME_INTERVAL);
            self.next = Some(finished + gap);
        }
    }

    pub(crate) fn next_wait(&self, focused: bool, now: Instant) -> Option<Duration> {
        focused
            .then_some(self.next)
            .flatten()
            .map(|next| next.saturating_duration_since(now))
    }

    pub(crate) fn advance(&mut self, focused: bool, now: Instant) -> bool {
        if self.next_wait(focused, now) != Some(Duration::ZERO) {
            return false;
        }
        // Sample elapsed time once for all bars. Delayed frames skip directly
        // to the current position instead of slowing the sweep or replaying
        // missed ticks after CPU contention or refocusing.
        let started = *self.started.get_or_insert(now);
        self.phase_ns =
            (now.saturating_duration_since(started).as_nanos() % CYCLE_DURATION.as_nanos()) as u64;
        self.sampled = Some(now);
        self.next = Some(now + FRAME_INTERVAL);
        self.tick_in_flight = true;
        true
    }
}

pub(crate) fn finish_frame(app: &mut App, now: Instant) {
    let was_active = app.loading.active();
    app.loading.finish_frame(now);
    if was_active && !app.loading.active() && !app.animation_settings.any_enabled() {
        // Cosmetic loading frames borrow the existing bounded render caches.
        // Release them as soon as the last visible operation finishes.
        app.animation_frame_cache.begin_frame(false, false);
        for group in &mut app.groups {
            group.inactive_chrome_cache = None;
        }
    }
}

/// Apply a thin moving underline without changing text, backgrounds, or hits.
/// Paint after static header caches, keeping animation out of their snapshots.
pub(crate) fn draw_header(f: &mut Frame, app: &mut App, area: Rect) {
    let area = area.intersection(f.area());
    if area.is_empty() {
        return;
    }
    app.loading.visible = true;
    app.loading.animated |= area.width > 1;
    let width = i64::from(area.width);
    let span = (width / 4).max(2).min(width);
    // The segment enters at the left and exits at the right. Even a one-cell
    // header remains meaningful; no underflow or width-dependent allocation.
    let travel = width + span - 2;
    let start = (1 - span) * SUBCELL
        + travel * SUBCELL * app.loading.phase_ns as i64 / CYCLE_DURATION.as_nanos() as i64;
    let end = start + span * SUBCELL;
    let y = area.bottom() - 1;
    let accent = app.colors.fg_accent_blue;
    let mut previous = None;
    for column in
        start.div_euclid(SUBCELL).max(0)..((end + SUBCELL - 1).div_euclid(SUBCELL)).min(width)
    {
        if let Some(cell) = f.buffer_mut().cell_mut((area.x + column as u16, y)) {
            let color = match previous {
                Some((background, color)) if background == cell.bg => color,
                _ => app.colors.ui.readable_foreground(accent, cell.bg, 3.0),
            };
            previous = Some((cell.bg, color));
            // Cross-fade the two boundary cells as the segment moves between
            // columns. Text/symbols, backgrounds and pointer geometry stay put.
            let coverage = (end.min((column + 1) * SUBCELL) - start.max(column * SUBCELL)) as f32
                / SUBCELL as f32;
            let color = if coverage < 1.0 {
                crate::app_theme::lerp_color(cell.bg, color, coverage)
            } else {
                color
            };
            cell.set_style(
                Style::default()
                    .underline_color(color)
                    .add_modifier(Modifier::UNDERLINED),
            );
        }
    }
}

pub(crate) fn draw_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    let busy = match app.sidebar_view {
        SidebarView::Explorer => {
            app.bg_explorer_refresh_running || app.bg_filesystem_action_running
        }
        SidebarView::Search => app.bg_global_search_running || app.bg_global_replace_running,
        SidebarView::SourceControl => {
            app.source_control_enabled
                && (app.bg_git_status_running
                    || app.bg_git_action_running
                    || app.bg_git_remote_running
                    || app.bg_git_log_running
                    || app.bg_git_branch_running)
        }
        _ => false,
    };
    if busy && !area.is_empty() {
        let inset = u16::from(app.sidebar_right).min(area.width);
        draw_header(
            f,
            app,
            Rect::new(area.x + inset, area.y, area.width.saturating_sub(1), 1),
        );
    }
}

pub(crate) fn editor_label(app: &App, editor: &Editor) -> Option<&'static str> {
    if editor.bg_loading {
        Some("Opening file…")
    } else if app.paste_pending_for(editor.navigation_id) {
        Some("Preparing paste…")
    } else if editor.file_path.as_ref().is_some_and(|path| {
        app.pending_file_saves
            .get(path)
            .is_some_and(|(generation, _)| *generation == app.workspace_generation)
    }) || app
        .pending_save_as
        .get(&editor.save_identity)
        .is_some_and(|(generation, _)| *generation == app.workspace_generation)
    {
        Some("Saving file…")
    } else {
        None
    }
}

pub(crate) fn draw_editor_header(f: &mut Frame, app: &mut App, tabs: Rect, breadcrumbs: Rect) {
    if app
        .editor()
        .is_some_and(|editor| editor_label(app, editor).is_some())
    {
        draw_header(
            f,
            app,
            if breadcrumbs.is_empty() {
                tabs
            } else {
                breadcrumbs
            },
        );
    }
}

#[cfg(test)]
#[path = "ui_loading_tests.rs"]
mod tests;
