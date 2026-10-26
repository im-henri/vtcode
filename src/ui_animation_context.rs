//! Trace a rounded bubble through the empty space on both sides of a scope.
//! The outline follows rendered code, never replacing or underlining source text.
use std::{collections::BTreeMap, time::Instant};

use ratatui::layout::Rect;

use super::{Effect, Geometry, Painter, animation_layers};
use crate::{
    animation_config::AnimationSettings,
    animations::{AnimationEvent, Trigger},
    app_theme::lerp_color,
};

type Position = (usize, usize);
type Pair = (Position, Position);

const SETTLED_INK: f32 = 0.70;
const ROUTE_ROW_MARGIN: u16 = 2;
const MAX_ROUTE_CELLS: usize = 32_768;

fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    // Evaluate the nearest endpoint to avoid cancellation in the final
    // frames. In particular, a fully settled rail has exactly zero heat.
    let near = t.min(1.0 - t);
    let eased = near * near * near * (near * (near * 6.0 - 15.0) + 10.0);
    if t <= 0.5 { eased } else { 1.0 - eased }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Endpoint {
    x: u16,
    y: u16,
    visible: bool,
    precise: bool,
}

fn shared_delimiter(geometry: &Geometry, anchor: &super::Anchor) -> bool {
    super::delimiter_family(anchor.ch).is_some()
        && geometry
            .line_anchors(anchor.line)
            .iter()
            .filter(|other| other.y == anchor.y && other.ch == anchor.ch)
            .take(2)
            .count()
            > 1
}

fn rows(geometry: &Geometry) -> &BTreeMap<usize, (u16, u16)> {
    geometry.source_rows.get_or_init(|| {
        let mut rows = BTreeMap::new();
        for &(line, _, y) in &geometry.scope_rows {
            let entry = rows.entry(line).or_insert((y, y));
            entry.0 = entry.0.min(y);
            entry.1 = entry.1.max(y);
        }
        for anchors in geometry.anchors.chunk_by(|a, b| a.line == b.line) {
            let first = anchors[0];
            let (top, bottom) = anchors.iter().fold((first.y, first.y), |(top, bottom), a| {
                (top.min(a.y), bottom.max(a.y))
            });
            let entry = rows.entry(first.line).or_insert((top, bottom));
            entry.0 = entry.0.min(top);
            entry.1 = entry.1.max(bottom);
        }
        rows
    })
}

fn endpoint(
    geometry: &Geometry,
    rows: &BTreeMap<usize, (u16, u16)>,
    position: Position,
) -> Option<Endpoint> {
    if let Some(anchor) = geometry.at(position) {
        let precise = shared_delimiter(geometry, &anchor);
        // Shared rows need the actual delimiter. Otherwise a brace can follow
        // an entire condition, so stop before its row's first source glyph.
        let x = if precise {
            anchor.x
        } else {
            geometry
                .line_anchors(anchor.line)
                .iter()
                .filter(|other| {
                    other.y == anchor.y
                        && other.x <= anchor.x
                        && !matches!(other.ch, ' ' | '\t' | '\0')
                })
                .map(|other| other.x)
                .min()
                .unwrap_or(anchor.x)
        };
        return Some(Endpoint {
            x,
            y: anchor.y,
            visible: true,
            precise,
        });
    }
    if let Some(&(_, _, y)) = geometry
        .omitted_lines
        .iter()
        .find(|&&(start, end, _)| position.0 >= start && position.0 < end)
    {
        return Some(Endpoint {
            x: geometry.area.x,
            y,
            visible: false,
            precise: false,
        });
    }
    let (&first, _) = rows.first_key_value()?;
    let (&last, _) = rows.last_key_value()?;
    if position.0 < first {
        Some(Endpoint {
            x: geometry.area.x,
            y: geometry.area.y,
            visible: false,
            precise: false,
        })
    } else if position.0 > last {
        Some(Endpoint {
            x: geometry.area.x,
            y: geometry.area.bottom().saturating_sub(1),
            visible: false,
            precise: false,
        })
    } else {
        // A horizontally clipped endpoint still has a known row. Prefer the
        // nearest logical anchor on wrapped lines. A folded source row has no
        // anchor, and must not acquire a fabricated connection.
        let &(first_y, _) = rows.get(&position.0)?;
        let y = geometry
            .line_anchors(position.0)
            .iter()
            .filter(|anchor| anchor.line == position.0)
            .min_by_key(|anchor| anchor.col.abs_diff(position.1))
            .map_or(first_y, |anchor| anchor.y);
        Some(Endpoint {
            x: geometry.area.x,
            y,
            visible: false,
            precise: false,
        })
    }
}

fn source_endpoint(
    painter: &Painter<'_>,
    geometry: &Geometry,
    rows: &BTreeMap<usize, (u16, u16)>,
    position: Position,
) -> Option<Endpoint> {
    let Some(hint) = painter.scope_endpoints.get(&position) else {
        return endpoint(geometry, rows, position);
    };
    let hint = hint.as_ref()?;
    // Keep the existing vertical/wrapped projection and its folded-row guard;
    // source metadata replaces only the viewport-dependent attachment choice.
    let mut end = endpoint(geometry, rows, position)?;
    end.precise = hint.precise;
    end.x = hint.x;
    end.visible = false;
    if let Some(anchor) = geometry.at(hint.attachment) {
        end.x = anchor.x;
        end.y = anchor.y;
        end.visible = true;
    }
    Some(end)
}

#[derive(Clone, Copy, Debug)]
struct RouteCell {
    x: u16,
    y: u16,
    symbol: &'static str,
    cost: f32,
    terminal: bool,
}

struct Route {
    cells: Vec<RouteCell>,
    fill: Vec<fill::Span>,
    first: Endpoint,
    second: Endpoint,
    length: f32,
    exact: bool,
}

#[path = "ui_scope_route_cache.rs"]
mod route_cache;
pub(super) use route_cache::RouteCache;

#[path = "ui_scope_bubble.rs"]
mod bubble;

#[path = "ui_scope_fill.rs"]
pub(super) mod fill;

#[path = "ui_scope_canonical.rs"]
pub(crate) mod canonical;

fn rail_symbol(symbol: &str) -> bool {
    matches!(symbol, "│" | "┊" | "┆" | "┃" | "·" | "→")
}

fn route_cell(painter: &Painter<'_>, x: u16, y: u16) -> bool {
    if let Some(flags) = painter.mask.and_then(|mask| mask.get((x, y))) {
        if flags & animation_layers::BLOCKED_ROW != 0 { return false; }
        if flags & animation_layers::EXCLUDED != 0 { return true; }
    } else {
        if painter.blocked_rows.contains(&y) { return false; }
        if painter.overlays.contains(&(x, y)) { return true; }
    }
    route_symbol(painter, x, y)
}

fn route_symbol(painter: &Painter<'_>, x: u16, y: u16) -> bool {
    painter
        .buffer
        .cell((x, y))
        .is_some_and(|cell| (cell.symbol() == " " || rail_symbol(cell.symbol())) && !cell.skip)
        && (1..=2u16).all(|offset| {
            x.checked_sub(offset).is_none_or(|previous| {
                painter.buffer.cell((previous, y)).is_none_or(|cell| {
                    unicode_width::UnicodeWidthStr::width(cell.symbol()) <= offset as usize
                })
            })
        })
}

fn free_cell(painter: &Painter<'_>, x: u16, y: u16) -> bool {
    if let Some(flags) = painter.mask.and_then(|mask| mask.get((x, y))) {
        return flags & (animation_layers::EXCLUDED | animation_layers::PROTECTED | animation_layers::CARET) == 0
            && route_symbol(painter, x, y);
    }
    route_cell(painter, x, y)
        && !painter.overlays.contains(&(x, y))
        && !painter.protected.contains(&(x, y))
        && !painter.carets.contains(&(x, y))
}

#[cfg(test)]
fn scope_route(
    painter: &Painter<'_>,
    geometry: &Geometry,
    gutter: Rect,
    rows: &BTreeMap<usize, (u16, u16)>,
    pair: Pair,
) -> Option<Route> {
    scope_route_with_mode(painter, geometry, gutter, rows, pair, true)
}

fn scope_route_with_mode(
    painter: &Painter<'_>,
    geometry: &Geometry,
    gutter: Rect,
    rows: &BTreeMap<usize, (u16, u16)>,
    pair: Pair,
    tight: bool,
) -> Option<Route> {
    // A fully occluded/unsafe editor cannot display a route or its fill.
    // Return a failed route that the cache can retain instead of repeatedly
    // building empty contours and walking every source cell.
    if !painter.blocked_rows.is_empty()
        && (geometry.area.y..geometry.area.bottom()).all(|y| painter.blocked_rows.contains(&y))
    {
        return None;
    }
    if let Some(shape) = painter.scope_shapes.get(&pair) {
        return shape.contour.project(
            painter,
            geometry,
            gutter,
            rows,
            pair,
            &shape.rows,
            &shape.origins,
        );
    }
    bubble::route(painter, geometry, gutter, rows, pair, tight)
}

fn origin_cost(
    route: &Route,
    geometry: &Geometry,
    rows: &BTreeMap<usize, (u16, u16)>,
    origin: Position,
) -> f32 {
    let origin = geometry
        .at(origin)
        .map(|anchor| Endpoint {
            x: anchor.x,
            y: anchor.y,
            visible: true,
            precise: false,
        })
        .or_else(|| endpoint(geometry, rows, origin))
        .unwrap_or(route.first);
    let y = origin.y.clamp(route.first.y, route.second.y);
    route
        .cells
        .iter()
        .map(|cell| (cell.x, cell.y, cell.cost))
        .min_by_key(|&(x, row, _)| (row.abs_diff(y), x.abs_diff(origin.x)))
        .map_or(0.0, |(_, _, cost)| cost)
}

#[derive(Clone, Copy)]
struct Signal {
    ink: f32,
    shade: f32,
    heat: f32,
}

/// Two continuous fronts leave the entry cell. A narrow bright head leads a
/// longer tapered wake; the wake settles into a useful, motionless guide.
#[cfg(test)]
fn signal(cost: f32, origin: f32, length: f32, terminal: bool, p: f32) -> Signal {
    SignalFrame::new(origin, length, p).at(cost, terminal)
}

#[test]
fn shared_signal_frame_matches_original_scope_ink_and_shade() {
    fn original(cost: f32, origin: f32, length: f32, terminal: bool, p: f32) -> Signal {
        if p >= 1.0 {
            return Signal {
                ink: SETTLED_INK,
                shade: 0.022,
                heat: 0.0,
            };
        }
        let p = p.clamp(0.0, 1.0);
        let distance = (cost - origin).abs();
        let reach = if cost < origin {
            origin
        } else {
            length - origin
        }
        .max(0.01);
        let feather = (0.8 + reach * 0.035).min(6.0);
        let head = (reach + feather) * smooth(p / 0.72);
        let reveal = smooth((head - distance + feather) / (feather * 2.0));
        let attack = smooth(p / 0.12);
        let release = smooth((1.0 - p) / 0.33);
        let difference = distance - head;
        let head_light = (-1.9 * (difference / feather).powi(2)).exp();
        let wake = (-(-difference).max(0.0) / (1.4 + reach.sqrt() * 0.5)).exp()
            * smooth((feather - difference) / feather);
        let wave = attack * release * (head_light * 0.76 + wake * 0.20);
        let arrival = if terminal {
            (-2.0 * ((p - 0.73) / 0.15).powi(2)).exp() * smooth((1.0 - p) / 0.20) * 0.18
        } else {
            0.0
        };
        Signal {
            ink: reveal * (SETTLED_INK * attack + wave + arrival),
            shade: reveal * (0.022 * attack + wave * 0.16 + arrival * 0.34),
            heat: (head_light * attack * release + arrival).clamp(0.0, 1.0),
        }
    }
    for length in [1.0, 13.5, 128.0, 2048.0] {
        let origin = length * 0.27;
        for step in 0..=120 {
            let p = step as f32 / 100.0;
            let frame = SignalFrame::new(origin, length, p);
            for index in 0..=64 {
                let cost = length * index as f32 / 64.0;
                for terminal in [false, true] {
                    let expected = original(cost, origin, length, terminal, p);
                    let actual = frame.at(cost, terminal);
                    assert_eq!(actual.ink.to_bits(), expected.ink.to_bits());
                    assert_eq!(actual.shade.to_bits(), expected.shade.to_bits());
                    if actual.ink != 0.0 {
                        assert!((actual.heat - expected.heat).abs() <= f32::EPSILON);
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct SignalSide {
    feather: f32,
    head: f32,
    wake_length: f32,
}

impl SignalSide {
    fn new(reach: f32, front: f32) -> Self {
        let reach = reach.max(0.01);
        // Long routes cross more cells per frame; widen their fractional front so
        // it transfers light instead of skipping whole bright terminal cells.
        let feather = (0.8 + reach * 0.035).min(6.0);
        Self {
            feather,
            head: (reach + feather) * front,
            wake_length: 1.4 + reach.sqrt() * 0.5,
        }
    }
}

/// Every contour cell shares its attack, release and one of two fronts.
/// Compute these once per scope, including square roots and terminal pulses.
struct SignalFrame {
    origin: f32,
    settled: bool,
    attack: f32,
    wave_strength: f32,
    arrival: f32,
    sides: [SignalSide; 2],
}

impl SignalFrame {
    fn new(origin: f32, length: f32, p: f32) -> Self {
        let settled = p >= 1.0;
        if settled {
            return Self {
                origin,
                settled,
                attack: 1.0,
                wave_strength: 0.0,
                arrival: 0.0,
                sides: [SignalSide {
                    feather: 1.0,
                    head: 0.0,
                    wake_length: 1.0,
                }; 2],
            };
        }
        let p = p.clamp(0.0, 1.0);
        let attack = smooth(p / 0.12);
        let release = smooth((1.0 - p) / 0.33);
        let front = smooth(p / 0.72);
        Self {
            origin,
            settled,
            attack,
            wave_strength: attack * release,
            arrival: (-2.0 * ((p - 0.73) / 0.15).powi(2)).exp() * smooth((1.0 - p) / 0.20) * 0.18,
            sides: [
                SignalSide::new(origin, front),
                SignalSide::new(length - origin, front),
            ],
        }
    }

    fn at(&self, cost: f32, terminal: bool) -> Signal {
        if self.settled {
            return Signal {
                ink: SETTLED_INK,
                shade: 0.022,
                heat: 0.0,
            };
        }
        let distance = (cost - self.origin).abs();
        let side = &self.sides[usize::from(cost >= self.origin)];
        let feather = side.feather;
        let reveal = smooth((side.head - distance + feather) / (feather * 2.0));
        // Fully unrevealed cells cannot contribute any light or ink.
        if reveal == 0.0 {
            return Signal {
                ink: 0.0,
                shade: 0.0,
                heat: 0.0,
            };
        }
        let difference = distance - side.head;
        let head_light = (-1.9 * (difference / feather).powi(2)).exp();
        let wake = (-(-difference).max(0.0) / side.wake_length).exp()
            * smooth((feather - difference) / feather);
        let wave = self.wave_strength * (head_light * 0.76 + wake * 0.20);
        let arrival = if terminal { self.arrival } else { 0.0 };
        Signal {
            ink: reveal * (SETTLED_INK * self.attack + wave + arrival),
            shade: reveal * (0.022 * self.attack + wave * 0.16 + arrival * 0.34),
            heat: (head_light * self.wave_strength + arrival).clamp(0.0, 1.0),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_scope(
    painter: &mut Painter<'_>,
    geometry: &Geometry,
    gutter: Rect,
    rows: &BTreeMap<usize, (u16, u16)>,
    pair: Pair,
    origin: Position,
    p: f32,
    fill_progress: f32,
    strength: f32,
    opacity: f32,
    active: bool,
    cache: &mut RouteCache,
) -> bool {
    if opacity <= 0.002 || strength <= 0.0 {
        return false;
    }
    let Some(route) = cache.route(painter, geometry, gutter, rows, pair) else {
        return false;
    };
    let origin = if p >= 1.0 {
        0.0
    } else {
        painter.scope_shapes.get(&pair).map_or_else(
            || origin_cost(route, geometry, rows, origin),
            |shape| shape.origin_cost(origin),
        )
    };
    let code = painter.area;
    let accent = painter
        .scope_colors
        .get(&pair)
        .copied()
        .unwrap_or(painter.colors.fg_accent_blue);
    let settled_color = (p >= 1.0).then(|| lerp_color(accent, painter.colors.fg_text, 0.20));
    let signals = SignalFrame::new(origin, route.length, p);
    let energy = strength * opacity;
    let mut visible = false;
    for cell in &route.cells {
        // Gutter numbers, diagnostics and markers retain complete styles, as
        // do source characters, selections and every caret.
        if !free_cell(painter, cell.x, cell.y) {
            continue;
        }
        visible = true;
        painter.set_area(if gutter.contains((cell.x, cell.y).into()) {
            gutter
        } else {
            code
        });
        let signal = signals.at(cell.cost, cell.terminal);
        if signal.ink * energy <= 0.015 && signal.shade * energy <= 0.002 {
            continue;
        }
        let color = settled_color.unwrap_or_else(|| {
            lerp_color(accent, painter.colors.fg_text, 0.20 + signal.heat * 0.45)
        });
        painter.tint_to(cell.x, cell.y, color, signal.shade * energy);
        painter.scope_symbol_to(
            (cell.x, cell.y),
            cell.symbol,
            color,
            signal.ink * energy,
            active,
        );
    }
    painter.set_area(code);
    visible |= fill::draw(
        painter,
        geometry,
        pair,
        &route.fill,
        accent,
        fill_progress,
        energy,
    );
    if active {
        // Reinforce exact attachment when several blocks share a rendered row.
        // Preserve the delimiter itself and all selection and caret styles.
        for (position, cost) in [(pair.0, 0.0), (pair.1, route.length)] {
            let Some(anchor) = geometry.at(position) else {
                continue;
            };
            if painter.is_caret((anchor.x, anchor.y)) {
                continue;
            }
            if pair.0.0 != pair.1.0 && !shared_delimiter(geometry, &anchor) {
                continue;
            }
            visible = true;
            let signal = signals.at(cost, true);
            painter.tint_to(
                anchor.x,
                anchor.y,
                accent,
                signal.ink * strength * opacity * 0.50,
            );
        }
    }
    visible
}
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_cached(
    painter: &mut Painter<'_>,
    geometry: &Geometry,
    settings: &AnimationSettings,
    events: &[AnimationEvent],
    scope: Option<Pair>,
    gutter: Rect,
    now: Instant,
    cache: &mut RouteCache,
) -> bool {
    if !settings.enabled(Effect::ScopeConnector)
        || settings.intensity(Effect::ScopeConnector) == 0
        || (scope.is_none()
            && !events
                .iter()
                .any(|event| matches!(event.trigger, Trigger::Scope { .. })))
    {
        return false;
    }
    cache.set_tight(settings.scope_tight);
    let strength = settings.intensity(Effect::ScopeConnector).min(100) as f32 / 100.0;
    let rows = rows(geometry);
    let mut top = u16::MAX;
    let mut bottom = 0;
    for pair in events
        .iter()
        .filter_map(|event| match event.trigger {
            Trigger::Scope { pair, .. } => Some(pair),
            _ => None,
        })
        .chain(scope)
    {
        if let Some((first, second)) = source_endpoint(painter, geometry, &rows, pair.0)
            .zip(source_endpoint(painter, geometry, &rows, pair.1))
        {
            // A wrapped heading can start several screen rows before its
            // delimiter. Cache the complete projected source rows as well.
            top = top.min(first.y.min(second.y));
            bottom = bottom.max(first.y.max(second.y));
            if let Some(&(first, _)) = rows.get(&pair.0.0) {
                top = top.min(first);
            }
            if let Some(&(_, last)) = rows.get(&pair.1.0) {
                bottom = bottom.max(last);
            }
        }
    }
    if top == u16::MAX {
        return false;
    }
    cache.prepare_rows(
        painter,
        geometry,
        gutter,
        top.saturating_sub(ROUTE_ROW_MARGIN),
        bottom.saturating_add(ROUTE_ROW_MARGIN),
    );
    let mut incoming = false;
    let mut visible = false;
    for event in events {
        let Trigger::Scope {
            pair,
            origin,
            exiting,
        } = &event.trigger
        else {
            continue;
        };
        if exiting.is_none() && Some(*pair) != scope {
            continue;
        }
        let phase_at = exiting.unwrap_or(now);
        let p = event.effect_progress(settings, Effect::ScopeConnector, phase_at);
        let fill_progress = event.scope_fill_progress(settings, phase_at);
        let opacity = exiting.map_or(1.0, |started| {
            smooth(
                1.0 - now.saturating_duration_since(started).as_secs_f32()
                    / AnimationEvent::scope_exit_duration(settings).as_secs_f32(),
            )
        });
        incoming |= exiting.is_none();
        let drawn = draw_scope(
            painter,
            geometry,
            gutter,
            &rows,
            *pair,
            *origin,
            p,
            fill_progress,
            strength,
            opacity,
            Some(*pair) == scope,
            cache,
        );
        visible |= exiting.is_none() && drawn;
    }
    if !incoming && let Some(pair) = scope {
        visible |= draw_scope(
            painter,
            geometry,
            gutter,
            &rows,
            pair,
            pair.0,
            1.0,
            if settings.scope_fill { 1.0 } else { 0.0 },
            strength,
            1.0,
            true,
            cache,
        );
    }
    visible
}

#[cfg(test)]
pub(super) fn draw(
    painter: &mut Painter<'_>,
    geometry: &Geometry,
    settings: &AnimationSettings,
    events: &[AnimationEvent],
    scope: Option<Pair>,
    gutter: Rect,
    now: Instant,
) {
    draw_cached(
        painter,
        geometry,
        settings,
        events,
        scope,
        gutter,
        now,
        &mut RouteCache::default(),
    );
}

#[cfg(test)]
#[path = "ui_scope_connector_regression_tests.rs"]
mod connector_regression_tests;

#[cfg(test)]
#[path = "ui_scope_bubble_tests.rs"]
mod bubble_tests;

#[cfg(test)]
#[path = "ui_scope_cell_tests.rs"]
mod cell_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{buffer::Buffer, style::Style};
    use std::{collections::HashSet, time::Duration};

    fn fixture(text: &[&str], first_line: usize) -> (Buffer, Geometry) {
        let colors = crate::app_theme::EditorTheme::Dark.colors();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 34, 14));
        buffer.set_style(
            buffer.area,
            Style::default().fg(colors.fg_text).bg(colors.bg_editor),
        );
        let mut geometry = Geometry::new(Rect::new(4, 1, 28, text.len() as u16), true);
        for (row, text) in text.iter().enumerate() {
            for (col, ch) in text.chars().enumerate() {
                geometry.push(first_line + row, col, ch, row, col, 1);
                buffer[(4 + col as u16, 1 + row as u16)].set_symbol(&ch.to_string());
            }
            geometry.push(first_line + row, text.len(), '\0', row, text.len(), 1);
        }
        (buffer, geometry)
    }

    fn settings() -> AnimationSettings {
        let mut settings = AnimationSettings::default();
        settings.set(Effect::ScopeConnector, true);
        settings.intensity = 100;
        settings.duration_ms = 400;
        settings
    }

    fn paint(
        base: &Buffer,
        geometry: &Geometry,
        events: &[AnimationEvent],
        scope: Option<Pair>,
        now: Instant,
    ) -> Buffer {
        let mut buffer = base.clone();
        let mut painter = Painter::new(
            &mut buffer,
            geometry.area,
            crate::app_theme::EditorTheme::Dark.colors(),
            HashSet::new(),
            HashSet::new(),
            HashSet::new(),
        );
        draw(
            &mut painter,
            geometry,
            &settings(),
            events,
            scope,
            Rect::new(0, geometry.area.y, 3, geometry.area.height),
            now,
        );
        painter.finish();
        buffer
    }

    #[test]
    fn mixed_else_scope_bubble_traces_both_sides_without_replacing_tokens() {
        let lines = [
            "            previous_work();",
            "            } else {",
            "            dense_body();",
            "        }",
        ];
        let (base, geometry) = fixture(&lines, 0);
        let editor = crate::editor::Editor::test_with_text(&lines.join("\n"));
        let pair = ((1, 19), (3, 8));
        let mut buffer = base.clone();
        let mut painter = Painter::new(
            &mut buffer,
            geometry.area,
            crate::app_theme::EditorTheme::Dark.colors(),
            HashSet::new(),
            HashSet::new(),
            HashSet::new(),
        );
        for position in [pair.0, pair.1] {
            painter.scope_endpoints.insert(
                position,
                super::super::source_scope_endpoint(&editor, &geometry, position),
            );
        }
        let route = scope_route(
            &painter,
            &geometry,
            Rect::new(0, 1, 3, 4),
            &rows(&geometry),
            pair,
        )
        .unwrap();
        assert!(route.cells.iter().any(|cell| cell.x < geometry.area.x + 12));
        assert!(route.cells.iter().any(|cell| cell.x > geometry.area.x + 24));
        assert_eq!(
            route
                .cells
                .iter()
                .map(|cell| (cell.x, cell.y))
                .collect::<HashSet<_>>()
                .len(),
            route.cells.len()
        );
        draw(
            &mut painter,
            &geometry,
            &settings(),
            &[],
            Some(pair),
            Rect::new(0, 1, 3, 4),
            Instant::now(),
        );
        painter.finish();
        assert_eq!(
            buffer[(geometry.area.x + 13, geometry.area.y + 1)].symbol(),
            "─",
            "the boundary continues through the gap before the else token"
        );
        for anchor in &geometry.anchors {
            if !matches!(anchor.ch, ' ' | '\t' | '\0') {
                assert_eq!(buffer[(anchor.x, anchor.y)], base[(anchor.x, anchor.y)]);
            }
        }
    }

    #[test]
    fn boxed_brace_is_enclosed_without_underlining_or_replacing_the_heading() {
        let lines = [
            "            previous_work();",
            "            } else {xxxxxxxx",
            "            xxxxxxxxxxxxxxxx",
            "        }",
        ];
        let (base, geometry) = fixture(&lines, 0);
        let editor = crate::editor::Editor::test_with_text(&lines.join("\n"));
        let pair = ((1, 19), (3, 8));
        let mut buffer = base.clone();
        let mut painter = Painter::new(
            &mut buffer,
            geometry.area,
            crate::app_theme::EditorTheme::Dark.colors(),
            HashSet::new(),
            HashSet::new(),
            HashSet::new(),
        );
        for position in [pair.0, pair.1] {
            painter.scope_endpoints.insert(
                position,
                super::super::source_scope_endpoint(&editor, &geometry, position),
            );
        }
        let route = scope_route(
            &painter,
            &geometry,
            Rect::new(0, 1, 3, 4),
            &rows(&geometry),
            pair,
        )
        .unwrap();
        assert!(!route.cells.is_empty());
        assert!(
            route
                .cells
                .iter()
                .all(|cell| route_cell(&painter, cell.x, cell.y))
        );
        draw(
            &mut painter,
            &geometry,
            &settings(),
            &[],
            Some(pair),
            Rect::new(0, 1, 3, 4),
            Instant::now(),
        );
        painter.finish();
        assert_ne!(buffer, base, "the scope outline remains visible");
        for anchor in &geometry.anchors {
            if !matches!(anchor.ch, ' ' | '\t' | '\0') {
                assert_eq!(buffer[(anchor.x, anchor.y)], base[(anchor.x, anchor.y)]);
            }
        }
        assert!(
            buffer
                .content
                .iter()
                .all(|cell| !cell.modifier.contains(ratatui::style::Modifier::UNDERLINED))
        );
    }

    #[test]
    fn explicit_omitted_ranges_precede_viewport_clipping_without_fabricating_folded_rows() {
        let (_, mut geometry) = fixture(&["    work()", "    more()"], 10);
        geometry.area.height = 8;
        geometry.omit_lines(0, 10, 0);
        geometry.omit_lines(12, 30, 3);
        let row_map = rows(&geometry).clone();
        let prefix = endpoint(&geometry, &row_map, (5, 0)).unwrap();
        assert_eq!(prefix.y, geometry.area.y);
        assert!(!prefix.visible);
        let suffix = endpoint(&geometry, &row_map, (20, 0)).unwrap();
        assert_eq!(
            suffix.y,
            geometry.area.y + 3,
            "visible marker takes precedence over blank viewport filler"
        );
        assert!(!suffix.visible);
        geometry.omitted_lines.clear();
        let suffix = endpoint(&geometry, &row_map, (20, 0)).unwrap();
        assert_eq!(suffix.y, geometry.area.bottom() - 1);
        geometry.anchors.retain(|anchor| anchor.line != 11);
        geometry.push(12, 0, '}', 2, 0, 1);
        assert!(
            endpoint(&geometry, &rows(&geometry), (11, 0)).is_none(),
            "ordinary folded rows remain unprojected"
        );
    }

    #[test]
    fn horizontally_clipped_diff_endpoints_keep_their_interior_display_rows() {
        for pan in [1, 12] {
            let mut geometry = Geometry::new(Rect::new(4, 1, 28, 8), true);
            geometry.set_diff_projection(pan, false);
            geometry.map_source_row(10, 1);
            geometry.map_source_row(11, 2);
            geometry.map_source_row(12, 3);
            // The long body still paints a glyph after panning past the short
            // opening/closing rows. Those rows remain visible, not offscreen.
            geometry.push_diff(11, pan, 'x', 2, 0, 1);
            if pan == 1 {
                geometry.push_diff(10, 1, '\0', 1, 0, 1);
                geometry.push_diff(12, 1, '\0', 3, 0, 1);
            }
            let row_map = rows(&geometry);
            let opening = endpoint(&geometry, &row_map, (10, 0)).unwrap();
            let closing = endpoint(&geometry, &row_map, (12, 0)).unwrap();
            assert_eq!(opening.y, geometry.area.y + 1, "pan={pan}");
            assert_eq!(closing.y, geometry.area.y + 3, "pan={pan}");
            assert!(!opening.visible && !closing.visible);
        }
    }

    #[test]
    fn scope_column_zero_uses_gutter_and_preserves_entire_document_cells() {
        let (base, geometry) = fixture(&["if ready", "    work()", "endif"], 0);
        let buffer = paint(
            &base,
            &geometry,
            &[],
            Some(((0, 0), (2, 0))),
            Instant::now(),
        );
        for anchor in &geometry.anchors {
            if !matches!(anchor.ch, ' ' | '\t' | '\0') {
                assert_eq!(buffer[(anchor.x, anchor.y)], base[(anchor.x, anchor.y)]);
            }
        }
        assert_eq!(buffer[(2, 2)].symbol(), "│");
        for y in [1, 3] {
            assert_eq!(
                buffer[(2, y)].symbol(),
                "│",
                "the separator prevents a continuous arm to column-zero source"
            );
        }
        for y in 0..base.area.height {
            assert_eq!(buffer[(3, y)], base[(3, y)], "separator remains untouched");
        }
    }

    #[test]
    fn scope_bubble_curves_replace_decorative_guides_without_crossings() {
        let (mut base, geometry) = fixture(&["        if ready", "        work()", "    endif"], 0);
        base[(7, 1)].set_symbol("┊");
        base[(7, 2)].set_symbol("┊");
        base[(7, 3)].set_symbol("┊");
        base[(8, 1)].set_symbol("│");
        base[(9, 1)].set_symbol("·");
        base[(10, 1)].set_symbol("→");
        let settled = paint(
            &base,
            &geometry,
            &[],
            Some(((0, 8), (2, 4))),
            Instant::now(),
        );
        let guides: Vec<_> = settled
            .content
            .iter()
            .enumerate()
            .filter(|(i, cell)| {
                super::super::scope_connections(cell.symbol()) != 0
                    && cell.symbol() != base.content[*i].symbol()
            })
            .collect();
        assert!(!guides.is_empty());
        assert!(
            guides
                .iter()
                .all(|(_, cell)| super::super::scope_connections(cell.symbol()).count_ones() <= 2)
        );
        for anchor in &geometry.anchors {
            if !matches!(anchor.ch, ' ' | '\t' | '\0') {
                assert_eq!(settled[(anchor.x, anchor.y)], base[(anchor.x, anchor.y)]);
            }
        }
    }

    #[test]
    fn scope_settled_guide_is_legible_at_default_intensity_in_dark_and_light_themes() {
        for theme in [
            crate::app_theme::EditorTheme::Dark,
            crate::app_theme::EditorTheme::Light,
        ] {
            let colors = theme.colors();
            let (mut base, geometry) = fixture(&["    if ready", "        work()", "    endif"], 0);
            base.set_style(
                base.area,
                Style::default().fg(colors.fg_text).bg(colors.bg_editor),
            );
            let mut settings = AnimationSettings::default();
            settings.set(Effect::ScopeConnector, true);
            let mut painter = Painter::new(
                &mut base,
                geometry.area,
                colors,
                HashSet::new(),
                HashSet::new(),
                HashSet::new(),
            );
            draw(
                &mut painter,
                &geometry,
                &settings,
                &[],
                Some(((0, 4), (2, 4))),
                Rect::new(0, 1, 3, 3),
                Instant::now(),
            );
            painter.finish();
            for y in 1..=3 {
                let cell = &base[(7, y)];
                assert_eq!(
                    cell.symbol(),
                    match y {
                        1 => "╭",
                        2 => "│",
                        _ => "╰",
                    }
                );
                assert!(
                    crate::ui_colors::contrast_ratio(cell.fg, cell.bg)
                        .is_some_and(|ratio| ratio >= 2.0),
                    "the entire settled guide must stay readable at default intensity: {cell:?}",
                );
            }
        }
    }

    #[test]
    fn scope_reveals_outward_in_both_directions_then_remains_quiet() {
        let origin = 5.0;
        let center = signal(origin, origin, 10.0, false, 0.17);
        let top = signal(0.0, origin, 10.0, true, 0.17);
        let bottom = signal(10.0, origin, 10.0, true, 0.17);
        assert!(center.ink > 0.2);
        assert_eq!(top.ink, 0.0);
        assert_eq!(bottom.ink, 0.0);
        let top = signal(1.0, origin, 10.0, false, 0.46);
        let bottom = signal(9.0, origin, 10.0, false, 0.46);
        assert!(top.ink > SETTLED_INK);
        assert!((top.ink - bottom.ink).abs() < 0.00001);
        let settled = signal(1.0, origin, 10.0, false, 1.0);
        assert!((settled.ink - SETTLED_INK).abs() < 0.00001);
        assert_eq!(settled.heat, 0.0);
        assert_eq!(settled.ink, signal(9.0, origin, 10.0, false, 1.0).ink);
    }

    #[test]
    fn scope_signal_has_continuous_heads_and_smooth_start_and_settle() {
        for sample in 0..1000 {
            let p = sample as f32 / 1000.0;
            let before = signal(3.0, 7.0, 12.0, false, p);
            let after = signal(3.0, 7.0, 12.0, false, p + 0.001);
            assert!((before.ink - after.ink).abs() < 0.045);
            assert!((before.shade - after.shade).abs() < 0.008);
        }
        assert_eq!(signal(7.0, 7.0, 12.0, false, 0.0).ink, 0.0);
        assert!((signal(3.0, 7.0, 12.0, false, 0.999).ink - SETTLED_INK).abs() < 0.00001);
    }

    #[test]
    fn scope_settled_connection_matches_final_entrance_and_has_no_time_dependence() {
        let (base, geometry) = fixture(&["    if ready", "        work()", "    endif"], 0);
        let pair = ((0, 4), (2, 4));
        let now = Instant::now();
        let event = AnimationEvent {
            trigger: Trigger::Scope {
                pair,
                origin: (1, 8),
                exiting: None,
            },
            started: now,
        };
        let final_frame = paint(
            &base,
            &geometry,
            &[event],
            Some(pair),
            now + Duration::from_secs(1),
        );
        let settled = paint(
            &base,
            &geometry,
            &[],
            Some(pair),
            now + Duration::from_secs(2),
        );
        assert_eq!(final_frame, settled);
        assert_eq!(
            settled,
            paint(
                &base,
                &geometry,
                &[],
                Some(pair),
                now + Duration::from_secs(20)
            )
        );
        assert_ne!(settled, base);
    }

    #[test]
    fn scope_exit_starts_from_partial_reveal_and_dissolves_exactly_to_baseline() {
        let (base, geometry) = fixture(
            &[
                "if ready",
                "  one()",
                "  two()",
                "  three()",
                "  four()",
                "  five()",
                "endif",
            ],
            0,
        );
        let pair = ((0, 0), (6, 0));
        let now = Instant::now();
        let exit = now + Duration::from_millis(100);
        let entering = AnimationEvent {
            trigger: Trigger::Scope {
                pair,
                origin: (3, 2),
                exiting: None,
            },
            started: now,
        };
        let leaving = AnimationEvent {
            trigger: Trigger::Scope {
                pair,
                origin: (3, 2),
                exiting: Some(exit),
            },
            started: now,
        };
        let entrance = paint(&base, &geometry, &[entering], Some(pair), exit);
        assert_eq!(
            entrance,
            paint(&base, &geometry, &[leaving.clone()], None, exit)
        );
        let halfway = paint(
            &base,
            &geometry,
            &[leaving.clone()],
            None,
            exit + Duration::from_millis(84),
        );
        assert_ne!(halfway, entrance);
        assert_ne!(halfway, base);
        assert_eq!(
            base,
            paint(
                &base,
                &geometry,
                &[leaving],
                None,
                exit + Duration::from_millis(168)
            )
        );
    }

    #[test]
    fn scope_rapid_reentry_starts_from_the_fading_frame_without_brightness_stacking() {
        let (base, geometry) = fixture(
            &["if ready", "    one()", "    two()", "    three()", "endif"],
            0,
        );
        let pair = ((0, 0), (4, 0));
        let now = Instant::now();
        let exiting_at = now + Duration::from_millis(90);
        let reentered_at = exiting_at + Duration::from_millis(35);
        let outgoing = AnimationEvent {
            trigger: Trigger::Scope {
                pair,
                origin: (2, 4),
                exiting: Some(exiting_at),
            },
            started: now,
        };
        let fading = paint(&base, &geometry, &[outgoing.clone()], None, reentered_at);
        let incoming = AnimationEvent {
            trigger: Trigger::Scope {
                pair,
                origin: (2, 4),
                exiting: None,
            },
            started: reentered_at,
        };
        assert_eq!(
            fading,
            paint(
                &base,
                &geometry,
                &[outgoing.clone(), incoming],
                Some(pair),
                reentered_at
            )
        );
        assert_eq!(
            fading,
            paint(
                &base,
                &geometry,
                &[outgoing.clone(), outgoing],
                None,
                reentered_at
            )
        );
    }

    #[test]
    fn scope_routes_do_not_shift_when_the_caret_crosses_indentation() {
        let (mut base, geometry) = fixture(&["    if ready", "        work()", "    endif"], 0);
        let colors = crate::app_theme::EditorTheme::Dark.colors();
        let pair = ((0, 4), (2, 4));
        let gutter = Rect::new(0, 1, 3, 3);
        let mapped = rows(&geometry);
        let mut painter = Painter::new(
            &mut base,
            geometry.area,
            colors,
            HashSet::new(),
            HashSet::new(),
            HashSet::new(),
        );
        let before = scope_route(&painter, &geometry, gutter, &mapped, pair).unwrap();
        painter.protected.to_mut().insert((7, 1));
        painter.carets.to_mut().insert((7, 2));
        let after = scope_route(&painter, &geometry, gutter, &mapped, pair).unwrap();
        assert_eq!(
            before
                .cells
                .iter()
                .map(|cell| (cell.x, cell.y))
                .collect::<Vec<_>>(),
            after
                .cells
                .iter()
                .map(|cell| (cell.x, cell.y))
                .collect::<Vec<_>>()
        );
        assert!(before.cells.iter().any(|cell| cell.x < geometry.area.x + 4));
        assert!(
            before
                .cells
                .iter()
                .any(|cell| cell.x >= geometry.area.x + 14)
        );
    }

    #[test]
    fn scope_literal_box_drawing_characters_are_source_text_not_indentation_guides() {
        let (base, geometry) = fixture(&["    if ready", "││││ diagram", "    endif"], 0);
        let settled = paint(
            &base,
            &geometry,
            &[],
            Some(((0, 4), (2, 4))),
            Instant::now(),
        );
        for x in 4..8 {
            assert_eq!(settled[(x, 2)], base[(x, 2)]);
        }
        assert_eq!(
            settled[(2, 2)].symbol(),
            "│",
            "literal source forces a safe gutter rail"
        );
    }

    #[test]
    fn scope_clipped_endpoints_use_visible_edges_and_folded_rows_are_not_invented() {
        let (mut base, geometry) = fixture(&["    work()", "    more()", "    done()"], 10);
        let painter = Painter::new(
            &mut base,
            geometry.area,
            crate::app_theme::EditorTheme::Dark.colors(),
            HashSet::new(),
            HashSet::new(),
            HashSet::new(),
        );
        let route = scope_route(
            &painter,
            &geometry,
            Rect::new(0, 1, 3, 3),
            &rows(&geometry),
            ((2, 0), (40, 0)),
        )
        .unwrap();
        assert_eq!(route.first.y, geometry.area.y);
        assert_eq!(route.second.y, geometry.area.bottom() - 1);
        assert!(route.cells.iter().any(|cell| cell.x < geometry.area.x + 4));
        assert!(
            route
                .cells
                .iter()
                .any(|cell| cell.x >= geometry.area.x + 10)
        );
        let mut folded = Geometry::new(geometry.area, true);
        folded.push(10, 0, 'a', 0, 0, 1);
        folded.push(12, 0, 'b', 2, 0, 1);
        assert!(endpoint(&folded, &rows(&folded), (11, 0)).is_none());
    }

    #[test]
    fn source_endpoint_columns_survive_upper_lower_and_both_vertical_clipping() {
        let source = [
            "    {",
            "        work()",
            "        more()",
            "        more()",
            "        more()",
            "        more()",
            "        done()",
            "    }",
        ];
        let editor = crate::editor::Editor::test_with_text(&source.join("\n"));
        let pair = ((0, 4), (7, 4));
        for (first, end) in [(0, 8), (1, 8), (0, 7), (1, 7)] {
            let (mut buffer, geometry) = fixture(&source[first..end], first);
            let mut painter = Painter::new(
                &mut buffer,
                geometry.area,
                crate::app_theme::EditorTheme::Dark.colors(),
                HashSet::new(),
                HashSet::new(),
                HashSet::new(),
            );
            for position in [pair.0, pair.1] {
                painter.scope_endpoints.insert(
                    position,
                    super::super::source_scope_endpoint(&editor, &geometry, position),
                );
            }
            let route = scope_route(
                &painter,
                &geometry,
                Rect::new(3, 1, 1, geometry.area.height),
                &rows(&geometry),
                pair,
            )
            .unwrap();
            assert!(
                route.cells.iter().any(|cell| cell.x < geometry.area.x + 8)
                    && route
                        .cells
                        .iter()
                        .any(|cell| cell.x >= geometry.area.x + 14),
                "both exterior boundaries remain visible for {first}..{end}"
            );
            assert_eq!(route.first.visible, first == 0);
            assert_eq!(route.second.visible, end == source.len());
        }
    }

    #[test]
    fn source_hints_keep_shared_attachment_after_sibling_delimiters_are_clipped() {
        let source = ["let x = { { {", "    work()", "}"];
        let editor = crate::editor::Editor::test_with_text(&source.join("\n"));
        let (mut buffer, mut geometry) = fixture(&source, 0);
        geometry
            .anchors
            .retain(|anchor| anchor.line != 0 || anchor.col < 9);
        let position = (0, 8);
        assert!(
            !endpoint(&geometry, &rows(&geometry), position)
                .unwrap()
                .precise
        );
        let mut painter = Painter::new(
            &mut buffer,
            geometry.area,
            crate::app_theme::EditorTheme::Dark.colors(),
            HashSet::new(),
            HashSet::new(),
            HashSet::new(),
        );
        painter.scope_endpoints.insert(
            position,
            super::super::source_scope_endpoint(&editor, &geometry, position),
        );
        let end = source_endpoint(&painter, &geometry, &rows(&geometry), position).unwrap();
        assert!(end.precise);
        assert!(end.visible);
        assert_eq!(end.x, geometry.area.x + 8);
    }

    #[test]
    fn clipped_delimiter_uses_visible_keyword_but_clipped_keyword_has_no_corner() {
        let source = "        let Some(value) = a_long_function_call() else {";
        let mut editor = crate::editor::Editor::test_with_text(&format!("{source}\n        }}"));
        let position = (0, source.chars().count() - 1);
        for horizontal_scroll in [0, 3, 10] {
            let clipped: String = source.chars().skip(horizontal_scroll).take(24).collect();
            let (mut buffer, mut geometry) = fixture(&[&clipped, "        }"], 0);
            for anchor in &mut geometry.anchors {
                if anchor.line == 0 {
                    anchor.col += horizontal_scroll;
                }
            }
            editor.horizontal_scroll = horizontal_scroll;
            let mut painter = Painter::new(
                &mut buffer,
                geometry.area,
                crate::app_theme::EditorTheme::Dark.colors(),
                HashSet::new(),
                HashSet::new(),
                HashSet::new(),
            );
            painter.scope_endpoints.insert(
                position,
                super::super::source_scope_endpoint(&editor, &geometry, position),
            );
            let end = source_endpoint(&painter, &geometry, &rows(&geometry), position).unwrap();
            assert_eq!(end.visible, horizontal_scroll <= 8);
            assert_eq!(
                end.x,
                (geometry.area.x + 8).saturating_sub(horizontal_scroll as u16)
            );
            assert_eq!(end.y, geometry.area.y);
        }
    }

    #[test]
    fn source_scope_color_survives_clipped_selected_and_caret_endpoints() {
        let source = [
            "    {",
            "        work()",
            "        more()",
            "        done()",
            "    }",
        ];
        let pair = ((0, 4), (4, 4));
        let mut reference = None;
        for (first, end) in [(0, 5), (1, 5), (1, 4), (0, 4)] {
            for protect_endpoints in [false, true] {
                let (mut base, geometry) = fixture(&source[first..end], first);
                let mut protected = HashSet::new();
                for anchor in &geometry.anchors {
                    if matches!(anchor.ch, '{' | '}') {
                        // A selected/caret delimiter can have an unrelated
                        // foreground. It must not determine the guide color.
                        base[(anchor.x, anchor.y)].set_fg(ratatui::style::Color::Blue);
                        if protect_endpoints {
                            protected.insert((anchor.x, anchor.y));
                        }
                    }
                }
                let mut painter = Painter::new(
                    &mut base,
                    geometry.area,
                    crate::app_theme::EditorTheme::Dark.colors(),
                    protected.clone(),
                    protected,
                    HashSet::new(),
                );
                painter
                    .scope_colors
                    .insert(pair, ratatui::style::Color::Magenta);
                draw(
                    &mut painter,
                    &geometry,
                    &settings(),
                    &[],
                    Some(pair),
                    Rect::new(0, geometry.area.y, 3, geometry.area.height),
                    Instant::now(),
                );
                painter.finish();
                let rails: Vec<_> = base
                    .content
                    .iter()
                    .filter(|cell| cell.symbol() == "│")
                    .collect();
                assert!(!rails.is_empty());
                let color = *reference.get_or_insert(rails[0].fg);
                assert!(
                    rails.iter().all(|cell| cell.fg == color),
                    "scope color changed for viewport {first}..{end}, protected={protect_endpoints}"
                );
            }
        }
    }

    #[test]
    fn scope_fallback_color_does_not_depend_on_visible_brace_foregrounds() {
        let source = [
            "    {",
            "        work()",
            "        more()",
            "        done()",
            "    }",
        ];
        let pair = ((0, 4), (4, 4));
        let mut reference = None;
        for (first, end) in [(0, 5), (1, 5), (1, 4), (0, 4)] {
            let (mut base, geometry) = fixture(&source[first..end], first);
            for anchor in &geometry.anchors {
                if matches!(anchor.ch, '{' | '}') {
                    base[(anchor.x, anchor.y)].set_fg(ratatui::style::Color::Magenta);
                }
            }
            let buffer = paint(&base, &geometry, &[], Some(pair), Instant::now());
            let rails: Vec<_> = buffer
                .content
                .iter()
                .filter(|cell| cell.symbol() == "│")
                .collect();
            assert!(!rails.is_empty());
            let color = *reference.get_or_insert(rails[0].fg);
            assert!(
                rails.iter().all(|cell| cell.fg == color),
                "fallback changed for viewport {first}..{end}"
            );
        }
    }

    #[test]
    fn scope_clipped_and_wrapped_endpoints_keep_their_actual_rendered_rows() {
        let (_, mut geometry) = fixture(&["        first", "continuation", "    endif"], 0);
        geometry.anchors.retain(|anchor| anchor.line != 1);
        for col in 20..32 {
            geometry.push(0, col, 'x', 1, col - 20, 1);
        }
        let mapped = rows(&geometry);
        assert_eq!(endpoint(&geometry, &mapped, (0, 24)).unwrap().y, 2);
        assert_eq!(endpoint(&geometry, &mapped, (0, 80)).unwrap().y, 2);
        assert_eq!(endpoint(&geometry, &mapped, (2, 80)).unwrap().y, 3);
    }

    #[test]
    fn scope_preserves_selection_carets_and_diagnostics_while_replacing_decorative_rails() {
        let (mut base, geometry) = fixture(&["    if ready", "        work()", "    endif"], 0);
        base[(7, 2)].set_symbol("┊");
        base[(2, 1)].set_symbol("7");
        let original = base.clone();
        let protected = HashSet::from([(7, 1)]);
        let carets = HashSet::from([(7, 3)]);
        let mut painter = Painter::new(
            &mut base,
            geometry.area,
            crate::app_theme::EditorTheme::Dark.colors(),
            protected,
            carets,
            HashSet::new(),
        );
        draw(
            &mut painter,
            &geometry,
            &settings(),
            &[],
            Some(((0, 4), (2, 4))),
            Rect::new(0, 1, 3, 3),
            Instant::now(),
        );
        painter.finish();
        for position in [(7, 1), (7, 3), (2, 1)] {
            assert_eq!(base[position], original[position]);
        }
        assert_eq!(base[(7, 2)].symbol(), "│");
        for anchor in &geometry.anchors {
            if !matches!(anchor.ch, ' ' | '\t' | '\0') {
                assert_eq!(base[(anchor.x, anchor.y)], original[(anchor.x, anchor.y)]);
            }
        }
    }

    #[test]
    fn scope_arm_stops_before_condition_text_and_disabling_restores_baseline() {
        let (base, geometry) = fixture(&["    if (ready) {", "        work();", "    }"], 0);
        let pair = ((0, 15), (2, 4));
        let endpoint = endpoint(&geometry, &rows(&geometry), pair.0).unwrap();
        assert_eq!(endpoint.x, 8);
        let settled = paint(&base, &geometry, &[], Some(pair), Instant::now());
        assert_ne!(settled, base);
        let mut disabled = settings();
        disabled.set(Effect::ScopeConnector, false);
        let mut buffer = base.clone();
        let mut painter = Painter::new(
            &mut buffer,
            geometry.area,
            crate::app_theme::EditorTheme::Dark.colors(),
            HashSet::new(),
            HashSet::new(),
            HashSet::new(),
        );
        draw(
            &mut painter,
            &geometry,
            &disabled,
            &[],
            Some(pair),
            Rect::new(0, 1, 3, 3),
            Instant::now(),
        );
        painter.finish();
        assert_eq!(buffer, base);
    }

    #[test]
    fn shared_opening_rows_enclose_code_and_mark_only_the_active_pair() {
        let (base, geometry) = fixture(
            &[
                "        { { {",
                "            work()",
                "        }",
                "    }",
                "}",
            ],
            0,
        );
        let outer = ((0, 8), (4, 0));
        let inner = ((0, 12), (2, 8));
        let now = Instant::now();
        let outgoing = AnimationEvent {
            trigger: Trigger::Scope {
                pair: outer,
                origin: (1, 12),
                exiting: Some(now),
            },
            started: now - Duration::from_secs(1),
        };
        let settled = paint(&base, &geometry, &[], Some(inner), now);
        let contour: Vec<_> = settled
            .content
            .iter()
            .enumerate()
            .filter(|(i, cell)| {
                super::super::scope_connections(cell.symbol()) != 0
                    && base.content[*i].symbol() == " "
            })
            .map(|(i, cell)| (i, cell.symbol().to_owned()))
            .collect();
        assert!(!contour.is_empty());
        assert!(
            contour
                .iter()
                .all(|(_, glyph)| super::super::scope_connections(glyph).count_ones() <= 2)
        );
        assert_ne!(settled[(16, 1)].bg, base[(16, 1)].bg);
        for x in [12, 14] {
            assert_eq!(settled[(x, 1)], base[(x, 1)]);
        }
        for anchor in &geometry.anchors {
            if !matches!(anchor.ch, ' ' | '\t' | '\0') {
                assert_eq!(
                    settled[(anchor.x, anchor.y)].symbol(),
                    base[(anchor.x, anchor.y)].symbol()
                );
            }
        }
        let transition = paint(&base, &geometry, &[outgoing], Some(inner), now);
        for (index, glyph) in contour {
            assert_eq!(
                transition.content[index].symbol(),
                glyph,
                "the active bubble owns overlapping fading strokes"
            );
        }
        assert_eq!(transition[(16, 1)], settled[(16, 1)]);
    }

    #[test]
    fn exact_shared_row_routes_continue_through_clipped_viewport_edges() {
        for (text, first_line, pair, visible_row, edge_row, direction) in [
            (
                ["    { { {", "        work()", "", ""],
                0,
                ((0, 8), (8, 8)),
                1,
                4,
                super::super::SOUTH,
            ),
            (
                ["", "", "        work()", "    } } }"],
                10,
                ((0, 8), (13, 8)),
                4,
                1,
                super::super::NORTH,
            ),
        ] {
            let (mut buffer, geometry) = fixture(&text, first_line);
            let original = buffer.clone();
            let mut painter = Painter::new(
                &mut buffer,
                geometry.area,
                crate::app_theme::EditorTheme::Dark.colors(),
                HashSet::new(),
                HashSet::new(),
                HashSet::new(),
            );
            for position in [pair.0, pair.1] {
                painter.scope_endpoints.insert(
                    position,
                    Some(super::super::ScopeEndpointHint {
                        attachment: position,
                        x: geometry.area.x + 8,
                        precise: true,
                        delimiter_x: Some(geometry.area.x + 8),
                    }),
                );
            }
            draw(
                &mut painter,
                &geometry,
                &settings(),
                &[],
                Some(pair),
                Rect::new(3, 1, 1, geometry.area.height),
                Instant::now(),
            );
            painter.finish();
            let source: Vec<_> = geometry
                .anchors
                .iter()
                .filter(|a| a.y == visible_row && !matches!(a.ch, ' ' | '\t' | '\0'))
                .collect();
            let left = source.iter().map(|a| a.x).min().unwrap();
            let right = source.iter().map(|a| a.x + a.width - 1).max().unwrap();
            let guides: Vec<_> = (geometry.area.x..geometry.area.right())
                .filter(|&x| {
                    super::super::scope_connections(buffer[(x, visible_row)].symbol()) != 0
                })
                .collect();
            assert!(
                guides.iter().any(|&x| x < left) && guides.iter().any(|&x| x > right),
                "the visible code is surrounded on both sides"
            );
            assert!(
                (geometry.area.x..geometry.area.right()).any(|x| super::super::scope_connections(
                    buffer[(x, edge_row)].symbol()
                ) & direction
                    != 0),
                "the clipped contour continues at the viewport edge"
            );
            for anchor in &geometry.anchors {
                if !matches!(anchor.ch, ' ' | '\t' | '\0') {
                    assert_eq!(
                        buffer[(anchor.x, anchor.y)].symbol(),
                        original[(anchor.x, anchor.y)].symbol()
                    );
                }
            }
        }
    }

    #[test]
    fn shared_opening_and_closing_rows_mark_the_exact_pair_and_respect_selection() {
        let (mut base, geometry) = fixture(&["    { { {", "        work()", "    } } }"], 0);
        let original = base.clone();
        let pair = ((0, 8), (2, 4));
        let settled = paint(&base, &geometry, &[], Some(pair), Instant::now());
        for position in [(12, 1), (8, 3)] {
            assert_ne!(settled[position].bg, original[position].bg);
        }
        for position in [(8, 1), (10, 1), (10, 3), (12, 3)] {
            assert_eq!(settled[position], original[position]);
        }
        let mut painter = Painter::new(
            &mut base,
            geometry.area,
            crate::app_theme::EditorTheme::Dark.colors(),
            HashSet::from([(12, 1)]),
            HashSet::from([(8, 3)]),
            HashSet::new(),
        );
        draw(
            &mut painter,
            &geometry,
            &settings(),
            &[],
            Some(pair),
            Rect::new(0, 1, 3, 3),
            Instant::now(),
        );
        painter.finish();
        for position in [(12, 1), (8, 3)] {
            assert_eq!(base[position], original[position]);
        }
    }

    #[test]
    fn shared_path_junctions_are_order_independent_and_keep_the_active_color() {
        use ratatui::style::Color;
        let (base, geometry) = fixture(&["    {", "        work()", "    }"], 0);
        let mut frames = Vec::new();
        for reverse in [false, true] {
            let mut buffer = base.clone();
            let mut painter = Painter::new(
                &mut buffer,
                geometry.area,
                crate::app_theme::EditorTheme::Dark.colors(),
                HashSet::new(),
                HashSet::new(),
                HashSet::new(),
            );
            let mut branches = [("╰", Color::Red, 0.8, false), ("│", Color::Blue, 0.2, true)];
            if reverse {
                branches.reverse();
            }
            for (symbol, color, amount, active) in branches {
                painter.scope_symbol_to((7, 2), symbol, color, amount, active);
            }
            assert_eq!(
                painter.layers[&(7, 2)].symbol.as_ref().unwrap().1,
                Color::Blue
            );
            painter.finish();
            assert_eq!(buffer[(7, 2)].symbol(), "│");
            frames.push(buffer);
        }
        assert_eq!(frames[0], frames[1]);
    }
}
