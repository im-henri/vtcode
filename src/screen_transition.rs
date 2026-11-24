//! Optional whole-screen startup and exit animations, both off by default.
//!
//! Startup: the first workbench frames rise into place from the bottom in
//! staggered columns. Each column's leading edge is a bright, briefly
//! scrambled glyph that settles into the real cell, so text and colours end
//! exactly as the workbench paints them. Exit: the text of the last frame
//! drains downward column by column, accelerating and fading as it falls.
//!
//! Both effects transform finished frame buffers only; they never change what
//! the workbench renders. Backgrounds appear at once and stay in place: only
//! text moves. Startup ticks reuse the last real frame, so the workbench
//! renderer runs only when the workbench itself changes.
use std::time::{Duration, Instant};

use ratatui::{
    buffer::{Buffer, Cell},
    style::{Color, Modifier},
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
    /// Theme colours that highlight text (selection, matches): they belong to
    /// the text layer and move with it instead of staying with backgrounds.
    pub(crate) highlights: Vec<Color>,
    startup: Option<Playback>,
    exit: Option<Exit>,
}

struct Playback {
    /// Set by the first painted frame, so a slow first paint cannot eat it.
    started: Option<Instant>,
    last_frame: Option<Instant>,
    duration: Duration,
    /// The last real workbench frame. Animation ticks transform this copy
    /// instead of running the workbench renderer every frame.
    target: Option<Buffer>,
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
    pub(crate) fn set_highlights(&mut self, colors: &crate::app_theme::ThemeColors) {
        self.highlights = vec![
            colors.bg_selection,
            colors.bg_word_highlight,
            colors.bg_bracket_match,
            colors.bg_search_match,
            colors.ui.bg_find_current,
            colors.ui.bg_selection_highlight,
            colors.ui.bg_terminal_selection,
        ];
    }

    /// Arm the startup effect for the first workbench frame, when enabled.
    pub(crate) fn arm_startup(&mut self) {
        self.startup = self.settings.startup.then_some(Playback {
            started: None,
            last_frame: None,
            duration: duration(self.settings.startup_ms),
            target: None,
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
        let playback = self.startup.as_ref()?;
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
        let target = playback.target.insert(buffer.clone());
        startup_frame(target, buffer, progress, flicker, &self.highlights);
    }

    /// Whether the next startup frame can reuse the cached workbench frame:
    /// one is due, a real frame exists, and it is not the final exact frame.
    pub(crate) fn cached_startup_ready(&self, now: Instant) -> bool {
        self.startup_due(now)
            && self.startup.as_ref().is_some_and(|playback| {
                playback.target.is_some()
                    && playback.started.is_some_and(|started| {
                        now.saturating_duration_since(started) < playback.duration
                    })
            })
    }

    /// Paint a startup frame from the cached workbench frame. The caller
    /// checks `cached_startup_ready` first; the final frame and any real
    /// change in the workbench take the normal render path instead.
    pub(crate) fn paint_startup_cached(&mut self, buffer: &mut Buffer, now: Instant) {
        let Some(playback) = &mut self.startup else {
            return;
        };
        let (Some(started), Some(target)) = (playback.started, &playback.target) else {
            return;
        };
        playback.last_frame = Some(now);
        let elapsed = now.saturating_duration_since(started);
        let progress = (elapsed.as_secs_f32() / playback.duration.as_secs_f32()).min(1.0);
        startup_frame(
            target,
            buffer,
            progress,
            (elapsed.as_millis() / 60) as u64,
            &self.highlights,
        );
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

    /// Before the exit snapshot is taken: the moment to read theme colours.
    pub(crate) fn exit_needs_snapshot_or_inactive(&self) -> bool {
        self.exit
            .as_ref()
            .is_none_or(|exit| exit.snapshot.is_none())
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
            &self.highlights,
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

/// Underlines and strikethroughs are drawn by the text layer, so they move
/// with the text, even on a blank cell (a tab or heading underline).
const TEXT_LINES: Modifier = Modifier::UNDERLINED.union(Modifier::CROSSED_OUT);

fn has_glyph(cell: &Cell) -> bool {
    !cell.symbol().trim().is_empty()
}

fn has_content(cell: &Cell) -> bool {
    has_glyph(cell) || cell.modifier.intersects(TEXT_LINES)
}

fn wide(cell: &Cell) -> bool {
    UnicodeWidthStr::width(cell.symbol()) > 1
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

/// For every cell whose background highlights text, the background it sits
/// on (`None` for plain cells). A highlight is a theme highlight colour, or a
/// short run of another background around text with the same background on
/// both sides, like a selection or match whose colour was blended.
pub(crate) fn highlight_bases(buffer: &Buffer, highlights: &[Color]) -> Vec<Option<Color>> {
    let area = buffer.area;
    let width = usize::from(area.width);
    let mut bases = vec![None; buffer.content.len()];
    for row in 0..usize::from(area.height) {
        let cells = &buffer.content[row * width..(row + 1) * width];
        // Runs of one background: (start, end, colour).
        let mut runs: Vec<(usize, usize, Color)> = Vec::new();
        for (index, cell) in cells.iter().enumerate() {
            match runs.last_mut() {
                Some(run) if run.2 == cell.bg => run.1 = index + 1,
                _ => runs.push((index, index + 1, cell.bg)),
            }
        }
        let marked: Vec<bool> = runs
            .iter()
            .enumerate()
            .map(|(index, &(start, end, bg))| {
                if highlights.contains(&bg) {
                    return true;
                }
                let (Some(left), Some(right)) = (
                    index.checked_sub(1).map(|i| runs[i].2),
                    runs.get(index + 1).map(|run| run.2),
                ) else {
                    return false;
                };
                let around: usize = runs
                    .iter()
                    .filter(|run| run.2 == left)
                    .map(|run| run.1 - run.0)
                    .sum();
                left == right
                    && end - start <= width / 3
                    && around > end - start
                    && cells[start..end].iter().any(has_glyph)
            })
            .collect();
        for (index, &(start, end, _)) in runs.iter().enumerate() {
            if !marked[index] {
                continue;
            }
            let base = (0..index)
                .rev()
                .chain(index + 1..runs.len())
                .find(|&other| !marked[other])
                .map(|other| runs[other].2);
            if let Some(base) = base {
                bases[row * width + start..row * width + end].fill(Some(base));
            }
        }
    }
    bases
}

fn index(area: ratatui::layout::Rect, x: u16, y: u16) -> usize {
    usize::from(y - area.y) * usize::from(area.width) + usize::from(x - area.x)
}

/// Leave only the cell's background: no glyph and no underline/strikethrough.
fn blank_text(cell: &mut Cell) {
    cell.set_symbol(" ");
    cell.modifier.remove(TEXT_LINES);
}

/// Startup at `progress` (0..1). Backgrounds are exact from the first frame;
/// only text rises into place, column by column from the bottom. Every cell
/// is exact once its column's trail has passed, and the whole frame is exact
/// for the final `SETTLE` fraction.
pub(crate) fn startup_frame(
    target: &Buffer,
    out: &mut Buffer,
    progress: f32,
    flicker: u64,
    highlights: &[Color],
) {
    let area = target.area;
    let height = f32::from(area.height);
    let bases = highlight_bases(target, highlights);
    for y in area.y..area.y + area.height {
        // Distance from the bottom row, in rows.
        let depth = f32::from(area.y + area.height - 1 - y);
        for x in area.x..area.x + area.width {
            let (Some(source), Some(cell)) = (target.cell((x, y)), out.cell_mut((x, y))) else {
                continue;
            };
            *cell = source.clone();
            let base = bases[index(area, x, y)];
            let column = owner(target, x, y);
            if column == x && !has_content(source) && base.is_none() {
                continue;
            }
            let delay = unit(u64::from(column)) * STAGGER;
            let local = ((progress - delay) / (1.0 - SETTLE - delay)).clamp(0.0, 1.0);
            // A column's front climbs past the top plus its whole trail.
            let front = ease_out(local) * (height + TRAIL + 1.0);
            let age = front - depth;
            if age >= TRAIL {
                continue;
            }
            let settled = age / TRAIL;
            if let Some(base) = base {
                // A highlight arrives with its text, fading in behind it.
                cell.set_bg(if age <= 0.0 {
                    base
                } else {
                    mix(base, source.bg, settled * 3.0)
                });
            }
            if age <= 0.0 || column != x {
                // Not risen yet; a wide glyph's trailing cell stays blank
                // until the glyph itself is exact.
                blank_text(cell);
                continue;
            }
            // The head scrambles; the trail resolves into the real glyph.
            let scramble = age < 1.5
                || (settled < 0.3
                    && unit(hash(u64::from(x) ^ (u64::from(y) << 20)) ^ flicker) < 0.3);
            if scramble && has_glyph(source) {
                cell.set_symbol(glyph(x, y, flicker));
            }
            let head = (1.0 - settled).powi(2);
            cell.set_fg(glow(source.fg, 0.75 * head));
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

/// Exit at `progress` (0..1). Backgrounds stay exactly in place; the text of
/// every column falls under gravity, staggered, fading into the background it
/// passes over. At 1.0 only the backgrounds remain.
pub(crate) fn exit_frame(
    snapshot: &Buffer,
    out: &mut Buffer,
    progress: f32,
    flicker: u64,
    highlights: &[Color],
) {
    let area = snapshot.area;
    let height = f32::from(area.height);
    let bases = highlight_bases(snapshot, highlights);
    for x in area.x..area.x + area.width {
        let (local, shift) = exit_fall(x, progress, height);
        let next_shift = exit_fall(x + 1, progress, height).1;
        let dim = local.powf(0.7);
        for y in area.y..area.y + area.height {
            let (Some(place), Some(cell)) = (snapshot.cell((x, y)), out.cell_mut((x, y))) else {
                continue;
            };
            *cell = place.clone();
            if shift == 0 && progress < 1.0 && !(wide(place) && next_shift != 0) {
                continue;
            }
            blank_text(cell);
            // A highlight leaves with its text: the background it sat on stays.
            let ground = bases[index(area, x, y)].unwrap_or(place.bg);
            cell.set_bg(ground);
            let offset = y - area.y;
            if progress >= 1.0 || offset < shift {
                continue;
            }
            let source_y = y - shift;
            let Some(source) = snapshot.cell((x, source_y)) else {
                continue;
            };
            if bases[index(area, x, source_y)].is_some() {
                cell.set_bg(mix(source.bg, ground, dim));
            }
            if !has_content(source) {
                continue;
            }
            cell.set_symbol(source.symbol());
            cell.modifier = source.modifier;
            cell.underline_color = source.underline_color;
            cell.set_fg(mix(source.fg, ground, dim * 0.85));
            // Wide glyphs cannot straddle columns that fall at different
            // speeds; and the top of each falling stream dissolves.
            let edge = shift > 0 && offset - shift < 2;
            if (wide(source) && next_shift != shift) || (edge && has_glyph(source)) {
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
