//! Regressions for the dense struct and inline blame layout from the editor.
use super::*;

use ratatui::{
    buffer::Buffer,
    style::{Color, Modifier, Style},
};
use std::collections::{HashMap, HashSet};

const STRUCT: &[&str] = &[
    "",
    "    AgentState {",
    "        agent: Some(AgentConfig::new(name, format!(\"{name}-agent\"))),",
    "        sessions: [(id.clone(), session)].into(),",
    "        selected: Some(id.clone()),",
    "        dock_selected: Some(id.clone()),",
    "        session_order: vec![id.clone()],",
    "        open_tabs: vec![id],",
    "        input: format!(\"{name} draft\"),",
    "        ..Default::default()",
    "    }",
    "",
];
const STRUCT_PAIR: Pair = ((1, 15), (10, 4));

fn fixture(text: &[&str]) -> (Buffer, Geometry, Rect) {
    let colors = crate::app_theme::EditorTheme::Dark.colors();
    let mut buffer = Buffer::empty(Rect::new(0, 0, 116, 18));
    buffer.set_style(
        buffer.area,
        Style::default().fg(colors.fg_text).bg(colors.bg_editor),
    );
    let area = Rect::new(4, 1, 108, text.len() as u16);
    let mut geometry = Geometry::new(area, true);
    for (row, line) in text.iter().enumerate() {
        for (col, ch) in line.chars().enumerate() {
            geometry.push(row, col, ch, row, col, 1);
            buffer[(area.x + col as u16, area.y + row as u16)].set_symbol(&ch.to_string());
        }
        geometry.push(row, line.len(), '\0', row, line.len(), 1);
    }
    (buffer, geometry, Rect::new(0, area.y, area.x, area.height))
}

fn struct_fixture() -> (Buffer, Geometry, Rect) {
    let (mut buffer, geometry, gutter) = fixture(STRUCT);
    // Inline blame is rendered content rather than a document anchor. Together
    // with the dense field below, it boxes in both sideways brace attachments.
    buffer.set_string(
        geometry.area.x + 17,
        geometry.area.y + 1,
        "Henri, 7d ago",
        Style::default()
            .fg(Color::Rgb(144, 144, 144))
            .add_modifier(Modifier::ITALIC),
    );
    (buffer, geometry, gutter)
}

fn painter<'a>(buffer: &'a mut Buffer, geometry: &Geometry) -> Painter<'a> {
    Painter::new(
        buffer,
        geometry.area,
        crate::app_theme::EditorTheme::Dark.colors(),
        HashSet::new(),
        HashSet::new(),
        HashSet::new(),
    )
}

fn exact_endpoints(painter: &mut Painter<'_>, geometry: &Geometry) {
    let editor = crate::editor::Editor::test_with_text(&STRUCT.join("\n"));
    for position in [STRUCT_PAIR.0, STRUCT_PAIR.1] {
        painter.scope_endpoints.insert(
            position,
            super::super::source_scope_endpoint(&editor, geometry, position),
        );
    }
}

fn paint_struct(
    painter: &mut Painter<'_>,
    geometry: &Geometry,
    gutter: Rect,
    cache: &mut RouteCache,
) {
    let mut settings = AnimationSettings::default();
    settings.set(Effect::ScopeConnector, true);
    settings.intensity = 100;
    draw_cached(
        painter,
        geometry,
        &settings,
        &[],
        Some(STRUCT_PAIR),
        gutter,
        Instant::now(),
        cache,
    );
    painter.finish();
}

fn route_points(route: &Route) -> Vec<(u16, u16, &'static str)> {
    route
        .cells
        .iter()
        .map(|cell| (cell.x, cell.y, cell.symbol))
        .collect()
}

fn assert_two_sided_contour(route: &Route, geometry: &Geometry, base: &Buffer) {
    assert_eq!(
        route
            .cells
            .iter()
            .map(|cell| (cell.x, cell.y))
            .collect::<HashSet<_>>()
            .len(),
        route.cells.len(),
        "the route must not visit a cell twice"
    );
    let cells: HashMap<_, _> = route
        .cells
        .iter()
        .map(|cell| {
            (
                (i32::from(cell.x), i32::from(cell.y)),
                super::super::scope_connections(cell.symbol),
            )
        })
        .collect();
    for (&(x, y), &ports) in &cells {
        assert!(
            (1..=2).contains(&ports.count_ones()),
            "the bubble has no crossing or tee"
        );
        for (direction, opposite, dx, dy) in
            [(1, 2, 0, -1), (2, 1, 0, 1), (4, 8, 1, 0), (8, 4, -1, 0)]
        {
            if ports & direction != 0 {
                let next = (x + dx, y + dy);
                let source = next.0 >= i32::from(geometry.area.x)
                    && next.0 < i32::from(geometry.area.right())
                    && next.1 >= i32::from(geometry.area.y)
                    && next.1 < i32::from(geometry.area.bottom())
                    && !base[(next.0 as u16, next.1 as u16)]
                        .symbol()
                        .trim()
                        .is_empty();
                assert!(
                    cells.get(&next).is_some_and(|ports| ports & opposite != 0) || source,
                    "bubble gap at ({x}, {y})"
                );
            }
        }
    }
    let top = geometry.at(STRUCT_PAIR.0).unwrap().y;
    let bottom = geometry.at(STRUCT_PAIR.1).unwrap().y;
    assert!(
        route
            .cells
            .iter()
            .all(|cell| (top..=bottom).contains(&cell.y)),
        "traces must not use the spare rows above and below the source"
    );
    for row in 2..10 {
        let anchors: Vec<_> = geometry
            .anchors
            .iter()
            .filter(|a| a.line == row && !matches!(a.ch, ' ' | '\t' | '\0'))
            .collect();
        let left = anchors.iter().map(|a| a.x).min().unwrap();
        let right = anchors.iter().map(|a| a.x + a.width - 1).max().unwrap();
        assert!(
            route
                .cells
                .iter()
                .any(|cell| cell.y == geometry.area.y + row as u16 && cell.x < left)
        );
        assert!(
            route
                .cells
                .iter()
                .any(|cell| cell.y == geometry.area.y + row as u16 && cell.x > right)
        );
    }
}

fn assert_rendered_content_preserved(base: &Buffer, painted: &Buffer, geometry: &Geometry) {
    for y in geometry.area.y..geometry.area.bottom() {
        for x in geometry.area.x..geometry.area.right() {
            if base[(x, y)].symbol() != " " {
                assert_eq!(
                    painted[(x, y)],
                    base[(x, y)],
                    "source or inline blame changed at ({x}, {y})"
                );
            }
            assert!(
                !painted[(x, y)].modifier.contains(Modifier::UNDERLINED)
                    || base[(x, y)].modifier.contains(Modifier::UNDERLINED),
                "scope added an underline at ({x}, {y})"
            );
        }
    }
}

#[test]
fn dense_struct_with_inline_blame_reuses_its_whitespace_bubble_within_the_source_rows() {
    let (base, geometry, gutter) = struct_fixture();
    let mut buffer = base.clone();
    let mut painter = painter(&mut buffer, &geometry);
    exact_endpoints(&mut painter, &geometry);
    let mapped = rows(&geometry);
    let route = scope_route(&painter, &geometry, gutter, &mapped, STRUCT_PAIR).unwrap();
    assert_two_sided_contour(&route, &geometry, &base);
    let points = route_points(&route);
    let mut cache = RouteCache::default();
    for _ in 0..4 {
        let mut frame = base.clone();
        let mut painter = self::painter(&mut frame, &geometry);
        exact_endpoints(&mut painter, &geometry);
        paint_struct(&mut painter, &geometry, gutter, &mut cache);
        assert_eq!(
            cache.builds, 1,
            "an unchanged contour with annotation obstacles stays cached"
        );
        assert_rendered_content_preserved(&base, &frame, &geometry);
        for &(x, y, symbol) in &points {
            assert_eq!(frame[(x, y)].symbol(), symbol);
        }
        for y in [geometry.area.y, geometry.area.bottom() - 1] {
            for x in geometry.area.x.saturating_sub(1)..geometry.area.right() {
                assert_eq!(
                    frame[(x, y)],
                    base[(x, y)],
                    "scope trace leaked into a spare row"
                );
            }
        }
    }
}

#[test]
fn caret_on_dense_struct_rail_preserves_its_cell_and_does_not_change_routing() {
    let (mut buffer, geometry, gutter) = struct_fixture();
    let mut painter = painter(&mut buffer, &geometry);
    exact_endpoints(&mut painter, &geometry);
    let mapped = rows(&geometry);
    let route = scope_route(&painter, &geometry, gutter, &mapped, STRUCT_PAIR).unwrap();
    assert_two_sided_contour(&route, &geometry, painter.buffer);
    let expected = route_points(&route);
    let caret = route
        .cells
        .iter()
        .find(|cell| cell.y > route.first.y && cell.y < route.second.y)
        .map(|cell| (cell.x, cell.y))
        .expect("the route crosses whitespace inside the struct");
    painter.buffer[caret].set_style(
        Style::default()
            .fg(Color::Rgb(18, 28, 38))
            .bg(Color::Rgb(235, 241, 247))
            .add_modifier(Modifier::BOLD | Modifier::REVERSED),
    );
    let original_caret = painter.buffer[caret].clone();
    let original = painter.buffer.clone();
    painter.carets.to_mut().insert(caret);
    assert_eq!(
        route_points(&scope_route(&painter, &geometry, gutter, &mapped, STRUCT_PAIR).unwrap()),
        expected,
        "caret masking must not move the continuous underlying route"
    );
    paint_struct(&mut painter, &geometry, gutter, &mut RouteCache::default());
    assert_eq!(painter.buffer[caret], original_caret);
    assert_rendered_content_preserved(&original, painter.buffer, &geometry);
    for (x, y, symbol) in expected {
        if (x, y) != caret {
            assert_eq!(painter.buffer[(x, y)].symbol(), symbol);
        }
    }
}

#[test]
fn fallback_arms_do_not_skip_inline_obstacles_and_emit_detached_fragments() {
    let (mut buffer, geometry, gutter) = fixture(&[
        "        if ready",
        "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        "        endif",
    ]);
    let obstacle = (geometry.area.x + 3, geometry.area.y);
    buffer[obstacle].set_symbol("B");
    let painter = painter(&mut buffer, &geometry);
    let route = scope_route(
        &painter,
        &geometry,
        gutter,
        &rows(&geometry),
        ((0, 8), (2, 8)),
    )
    .unwrap();
    assert!(!route.exact);
    let cells: HashMap<_, _> = route
        .cells
        .iter()
        .map(|cell| {
            (
                (i32::from(cell.x), i32::from(cell.y)),
                super::super::scope_connections(cell.symbol),
            )
        })
        .collect();
    for (&(x, y), &ports) in &cells {
        assert!(
            (1..=2).contains(&ports.count_ones()),
            "a partial side has no crossings"
        );
        assert!(
            painter.buffer[(x as u16, y as u16)]
                .symbol()
                .trim()
                .is_empty(),
            "the outline stays in whitespace"
        );
        for (direction, opposite, dx, dy) in
            [(1, 2, 0, -1), (2, 1, 0, 1), (4, 8, 1, 0), (8, 4, -1, 0)]
        {
            let next = (x + dx, y + dy);
            if ports & direction == 0 || cells.get(&next).is_some_and(|ports| ports & opposite != 0)
            {
                continue;
            }
            let in_buffer = next.0 >= 0
                && next.1 >= 0
                && next.0 < i32::from(painter.buffer.area.right())
                && next.1 < i32::from(painter.buffer.area.bottom());
            assert!(
                !in_buffer
                    || !painter.buffer[(next.0 as u16, next.1 as u16)]
                        .symbol()
                        .trim()
                        .is_empty()
                    || direction == 1 && y == i32::from(geometry.area.y)
                    || direction == 2 && y == i32::from(geometry.area.bottom()) - 1,
                "partial side skips a blank cell at ({x}, {y}) -> {next:?}"
            );
        }
    }
    assert_eq!(painter.buffer[obstacle].symbol(), "B");
}
