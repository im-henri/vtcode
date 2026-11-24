//! Optional whole-screen startup and exit animations, both off by default.
//!
//! Startup: the first workbench frames rise into place from the bottom in
//! staggered columns. Each column's leading edge is a bright, briefly
//! scrambled glyph that settles into the real cell, so text and colours end
//! exactly as the workbench paints them. Exit: a snapshot of the last frame
//! drains downward column by column, accelerating and dimming as it falls.
//!
//! Both effects transform finished frame buffers only; they never change what
//! the workbench renders. Unrevealed space uses the terminal's own background
//! so the workbench rises out of, and drains back into, the shell.
use std::time::{Duration, Instant};

use ratatui::{
    buffer::{Buffer, Cell},
    style::Color,
};
use unicode_width::UnicodeWidthStr;

pub(crate) const DEFAULT_DURATION_MS: u64 = 1_500;
/// Frame pacing for both effects (about 60 frames per second).
const FRAME: Duration = Duration::from_millis(16);
/// Rows behind the startup front that are still settling.
const TRAIL: f32 = 7.0;
/// Largest column stagger, as a fraction of the duration.
const STAGGER: f32 = 0.28;
/// Exit columns start further apart so the screen flows rather than drops.
const EXIT_STAGGER: f32 = 0.5;
/// Final fraction of the startup duration with every cell already exact.
const SETTLE: f32 = 0.1;
const GLYPHS: &[u8] = b"0123456789ABCDEFGHJKLMNPRSTUVWXYZabcdefghkmnopqrstuvwxyz<>/\\|=+*:;#%&$@";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    pub(crate) startup: bool,
    pub(crate) exit: bool,
    pub(crate) startup_ms: u64,
    pub(crate) exit_ms: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            startup: false,
            exit: false,
            startup_ms: DEFAULT_DURATION_MS,
            exit_ms: DEFAULT_DURATION_MS,
        }
    }
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) settings: Settings,
    startup: Option<Playback>,
    exit: Option<Exit>,
}

#[derive(Clone, Copy)]
struct Playback {
    /// Set by the first painted frame, so a slow first paint cannot eat it.
    started: Option<Instant>,
    last_frame: Option<Instant>,
    duration: Duration,
}

struct Exit {
    started: Instant,
    duration: Duration,
    snapshot: Option<Buffer>,
}

fn duration(ms: u64) -> Duration {
    Duration::from_millis(crate::animation_config::clamp_animation_duration_ms(ms))
}

impl State {
    /// Arm the startup effect for the first workbench frame, when enabled.
    pub(crate) fn arm_startup(&mut self) {
        self.startup = self.settings.startup.then_some(Playback {
            started: None,
            last_frame: None,
            duration: duration(self.settings.startup_ms),
        });
    }

    #[cfg(test)]
    pub(crate) fn startup_active(&self) -> bool {
        self.startup.is_some()
    }

    /// Input skips the rest of the startup effect. Returns whether it ran.
    pub(crate) fn skip_startup(&mut self) -> bool {
        self.startup.take().is_some()
    }

    /// Time until the next startup frame is due.
    pub(crate) fn next_wait(&self, now: Instant) -> Option<Duration> {
        let playback = self.startup?;
        Some(playback.last_frame.map_or(Duration::ZERO, |last| {
            (last + FRAME).saturating_duration_since(now)
        }))
    }

    /// Whether a startup frame is due now (or the final, exact frame is).
    pub(crate) fn startup_due(&self, now: Instant) -> bool {
        self.next_wait(now).is_some_and(|wait| wait.is_zero())
    }

    /// Transform a finished workbench frame while the startup effect plays.
    pub(crate) fn paint_startup(&mut self, buffer: &mut Buffer, now: Instant) {
        let Some(playback) = &mut self.startup else {
            return;
        };
        let started = *playback.started.get_or_insert(now);
        playback.last_frame = Some(now);
        let elapsed = now.saturating_duration_since(started);
        if elapsed >= playback.duration {
            self.startup = None;
            return;
        }
        let progress = elapsed.as_secs_f32() / playback.duration.as_secs_f32();
        let flicker = (elapsed.as_millis() / 60) as u64;
        let target = buffer.clone();
        startup_frame(&target, buffer, progress, flicker);
    }

    /// Begin the exit effect. Returns false when it is disabled.
    pub(crate) fn begin_exit(&mut self, now: Instant) -> bool {
        if !self.settings.exit {
            return false;
        }
        self.startup = None;
        self.exit.get_or_insert(Exit {
            started: now,
            duration: duration(self.settings.exit_ms),
            snapshot: None,
        });
        true
    }

    /// Whether the exit effect has finished (or was skipped).
    pub(crate) fn exit_done(&self, now: Instant) -> bool {
        self.exit
            .as_ref()
            .is_none_or(|exit| now.saturating_duration_since(exit.started) >= exit.duration)
    }

    pub(crate) fn skip_exit(&mut self) {
        if let Some(exit) = &mut self.exit {
            exit.duration = Duration::ZERO;
        }
    }

    /// Whether the exit effect still needs the workbench's last frame.
    pub(crate) fn exit_needs_snapshot(&self) -> bool {
        self.exit
            .as_ref()
            .is_some_and(|exit| exit.snapshot.is_none())
    }

    /// Paint one exit frame. The first call keeps `buffer` (the workbench's
    /// last frame) as the snapshot that every later frame drains.
    pub(crate) fn paint_exit(&mut self, buffer: &mut Buffer, now: Instant) {
        let Some(exit) = &mut self.exit else {
            return;
        };
        let snapshot = exit.snapshot.get_or_insert_with(|| buffer.clone());
        let elapsed = now.saturating_duration_since(exit.started);
        let progress = if exit.duration.is_zero() {
            1.0
        } else {
            (elapsed.as_secs_f32() / exit.duration.as_secs_f32()).min(1.0)
        };
        exit_frame(
            snapshot,
            buffer,
            progress,
            (elapsed.as_millis() / 60) as u64,
        );
    }

    pub(crate) fn frame_interval() -> Duration {
        FRAME
    }
}

/// SplitMix64 finaliser: stable per-cell randomness without any state.
fn hash(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn unit(value: u64) -> f32 {
    (hash(value) >> 40) as f32 / (1u64 << 24) as f32
}

fn glyph(x: u16, y: u16, flicker: u64) -> &'static str {
    let index =
        hash((u64::from(x) << 32) ^ (u64::from(y) << 16) ^ flicker.wrapping_mul(0x51)) as usize;
    let byte = &GLYPHS[index % GLYPHS.len()];
    // Every glyph is ASCII, so a one-byte slice is valid UTF-8.
    std::str::from_utf8(std::slice::from_ref(byte)).unwrap_or("0")
}

fn rgb(color: Color) -> Option<(f32, f32, f32)> {
    match color {
        Color::Rgb(r, g, b) => Some((f32::from(r), f32::from(g), f32::from(b))),
        _ => None,
    }
}

/// Blend two colours; non-RGB colours (terminal palette or default) switch
/// over halfway instead, since their real values are unknown.
fn mix(from: Color, to: Color, amount: f32) -> Color {
    let amount = amount.clamp(0.0, 1.0);
    match (rgb(from), rgb(to)) {
        (Some(a), Some(b)) => {
            let lerp = |a: f32, b: f32| (a + (b - a) * amount).round() as u8;
            Color::Rgb(lerp(a.0, b.0), lerp(a.1, b.1), lerp(a.2, b.2))
        }
        _ if amount < 0.5 => from,
        _ => to,
    }
}

/// The cell's own colour lifted toward white: the bright head of a stream.
fn glow(color: Color, amount: f32) -> Color {
    match color {
        Color::Rgb(..) => mix(color, Color::Rgb(255, 255, 255), amount),
        Color::Reset => Color::White,
        other => other,
    }
}

fn has_content(cell: &Cell) -> bool {
    !cell.symbol().trim().is_empty()
}

fn wide(cell: &Cell) -> bool {
    UnicodeWidthStr::width(cell.symbol()) > 1
}

fn empty(cell: &mut Cell) {
    cell.reset();
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// Column that owns `x`'s timing: a wide glyph's trailing cell moves with it.
fn owner(target: &Buffer, x: u16, y: u16) -> u16 {
    let area = target.area;
    if x > area.x && target.cell((x - 1, y)).is_some_and(wide) {
        x - 1
    } else {
        x
    }
}

/// Startup at `progress` (0..1): columns rise from the bottom; every cell is
/// exact once its column's trail has passed, and the whole frame is exact
/// for the final `SETTLE` fraction.
pub(crate) fn startup_frame(target: &Buffer, out: &mut Buffer, progress: f32, flicker: u64) {
    let area = target.area;
    let height = f32::from(area.height);
    for y in area.y..area.y + area.height {
        // Distance from the bottom row, in rows.
        let depth = f32::from(area.y + area.height - 1 - y);
        for x in area.x..area.x + area.width {
            let column = owner(target, x, y);
            let delay = unit(u64::from(column)) * STAGGER;
            let local = ((progress - delay) / (1.0 - SETTLE - delay)).clamp(0.0, 1.0);
            // A column's front climbs past the top plus its whole trail.
            let front = ease_out(local) * (height + TRAIL + 1.0);
            let age = front - depth;
            let Some(source) = target.cell((x, y)) else {
                continue;
            };
            let Some(cell) = out.cell_mut((x, y)) else {
                continue;
            };
            if age >= TRAIL {
                *cell = source.clone();
                continue;
            }
            if age <= 0.0 {
                empty(cell);
                continue;
            }
            let settled = age / TRAIL;
            *cell = source.clone();
            cell.set_bg(mix(Color::Reset, source.bg, (settled * 2.0).min(1.0)));
            if column != x {
                // Hidden behind its wide glyph, or blank beside a narrow
                // stand-in for it while scrambled.
                cell.set_symbol(" ");
                continue;
            }
            if has_content(source) {
                // The head scrambles; the trail resolves into the real glyph.
                let scramble = age < 1.5
                    || (settled < 0.3
                        && unit(hash(u64::from(x) ^ (u64::from(y) << 20)) ^ flicker) < 0.3);
                if scramble {
                    cell.set_symbol(glyph(x, y, flicker));
                }
                let head = (1.0 - settled).powi(2);
                cell.set_fg(glow(source.fg, 0.75 * head));
            } else if age < 1.0 {
                // Blank space still shows the passing front, faintly.
                cell.set_bg(glow(source.bg, 0.12));
            }
        }
    }
}

/// How far column `x` has fallen: its own progress and the rows it moved.
fn exit_fall(x: u16, progress: f32, height: f32) -> (f32, u16) {
    let delay = unit(u64::from(x) ^ 0xd1b5_4a32) * EXIT_STAGGER;
    let local = ((progress - delay) / (1.0 - delay)).clamp(0.0, 1.0);
    // Gravity: slow to let go, then quick to clear the screen.
    (local, (local.powf(2.2) * (height + TRAIL)).floor() as u16)
}

/// Exit at `progress` (0..1): every column drains downward under gravity,
/// staggered, dimming as it falls; at 1.0 the frame is empty.
pub(crate) fn exit_frame(snapshot: &Buffer, out: &mut Buffer, progress: f32, flicker: u64) {
    let area = snapshot.area;
    let height = f32::from(area.height);
    for x in area.x..area.x + area.width {
        let (local, shift) = exit_fall(x, progress, height);
        let next_shift = exit_fall(x + 1, progress, height).1;
        for y in area.y..area.y + area.height {
            let Some(cell) = out.cell_mut((x, y)) else {
                continue;
            };
            let offset = y - area.y;
            if progress >= 1.0 || offset < shift {
                empty(cell);
                continue;
            }
            let source_y = y - shift;
            let Some(source) = snapshot.cell((x, source_y)) else {
                empty(cell);
                continue;
            };
            *cell = source.clone();
            if shift == 0 {
                continue;
            }
            let dim = local.powf(0.7);
            cell.set_fg(mix(source.fg, source.bg, dim * 0.85));
            if source.bg != Color::Reset {
                cell.set_bg(mix(source.bg, Color::Reset, dim));
            }
            // Wide glyphs cannot straddle columns that fall at different
            // speeds; and the top of each falling stream dissolves.
            let edge = offset - shift < 2;
            if (wide(source) && next_shift != shift) || (edge && has_content(source)) {
                cell.set_symbol(glyph(x, source_y, flicker));
                if edge {
                    cell.set_fg(glow(source.fg, 0.5 * (1.0 - dim)));
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "screen_transition_tests.rs"]
mod tests;
