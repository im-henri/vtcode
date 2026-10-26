//! Scope outlines stay in whitespace and surround both sides of visible code.
use super::*;

use ratatui::{
    buffer::Buffer,
    style::{Color, Modifier, Style},
};
use std::collections::{HashMap, HashSet};

#[path = "ui_scope_fill_tests.rs"]
mod fill_tests;

const PROFILE: &[&str] = &[
    "",
    "    AgentState {",
    "        small,",
    "        small,",
    "        much_longer_value,",
    "        small,",
    "        small,",
    "    }",
    "",
];
const PROFILE_PAIR: Pair = ((1, 15), (7, 4));
const BOXED: &[&str] = &[
    "    AgentState { {",
    "        field_is_full,",
    "        ..Default::default()",
    "    field_end } }",
];
const BOXED_PAIR: Pair = ((0, 15), (3, 14));
// The helper from the tightness examples: successive indentation levels on
// the left and meaningful recesses between longer lines on the right.
const NESTED_MAP: &[&str] = &[
    "",
    "fn text(terminal: &Terminal<TestBackend>) -> String {",
    "    let b = terminal.backend().buffer();",
    "    (0..b.area.height)",
    "        .map(|y| {",
    "            (0..b.area.width)",
    "                .map(|x| b[(x, y)].symbol())",
    "                .collect::<String>()",
    "        })",
    "        .collect::<Vec<_>>()",
    "        .join(\"\\n\")",
    "}",
    "",
];

fn nested_map_pairs() -> [Pair; 2] {
    [
        ((1, NESTED_MAP[1].len() - 1), (11, 0)),
        ((4, NESTED_MAP[4].len() - 1), (8, 8)),
    ]
}

fn fixture(text: &[&str], area: Rect) -> (Buffer, Geometry, Rect) {
    let colors = crate::app_theme::EditorTheme::Dark.colors();
    let mut buffer = Buffer::empty(Rect::new(0, 0, area.right() + 1, area.bottom() + 1));
    buffer.set_style(
        buffer.area,
        Style::default().fg(colors.fg_text).bg(colors.bg_editor),
    );
    let mut geometry = Geometry::new(area, true);
    for (row, line) in text.iter().enumerate().take(usize::from(area.height)) {
        let mut x = 0;
        for (col, ch) in line.chars().enumerate() {
            let width = if ch == '\t' {
                4 - x % 4
            } else {
                unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0)
            };
            if x + width > usize::from(area.width) {
                break;
            }
            geometry.push(row, col, ch, row, x, width);
            if ch != '\t' && width != 0 {
                buffer[(area.x + x as u16, area.y + row as u16)].set_symbol(&ch.to_string());
                for dx in 1..width {
                    buffer[(area.x + (x + dx) as u16, area.y + row as u16)].skip = true;
                }
            }
            x += width;
        }
        if x < usize::from(area.width) {
            geometry.push(row, line.chars().count(), '\0', row, x, 1);
        }
    }
    (
        buffer,
        geometry,
        Rect::new(area.x.saturating_sub(1), area.y, 1, area.height),
    )
}

fn painter<'a>(
    buffer: &'a mut Buffer,
    geometry: &Geometry,
    text: &[&str],
    pair: Pair,
) -> Painter<'a> {
    let mut painter = Painter::new(
        buffer,
        geometry.area,
        crate::app_theme::EditorTheme::Dark.colors(),
        HashSet::new(),
        HashSet::new(),
        geometry.unsafe_rows.clone(),
    );
    let editor = crate::editor::Editor::test_with_text(&text.join("\n"));
    for position in [pair.0, pair.1] {
        painter.scope_endpoints.insert(
            position,
            super::super::source_scope_endpoint(&editor, geometry, position),
        );
    }
    painter
}

fn paint(
    painter: &mut Painter<'_>,
    geometry: &Geometry,
    gutter: Rect,
    pair: Pair,
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
        Some(pair),
        gutter,
        Instant::now(),
        cache,
    );
    painter.finish();
}

fn points(route: &Route) -> Vec<(u16, u16, &'static str)> {
    route
        .cells
        .iter()
        .map(|cell| (cell.x, cell.y, cell.symbol))
        .collect()
}

fn assert_source_preserved(before: &Buffer, after: &Buffer, geometry: &Geometry) {
    for anchor in &geometry.anchors {
        if matches!(anchor.ch, ' ' | '\t' | '\0') {
            continue;
        }
        let position = (anchor.x, anchor.y);
        assert_eq!(
            after[position].symbol(),
            before[position].symbol(),
            "source at {position:?}"
        );
        assert_eq!(
            after[position].fg, before[position].fg,
            "syntax at {position:?}"
        );
        assert_eq!(
            after[position].modifier, before[position].modifier,
            "source modifiers at {position:?}"
        );
        for dx in 1..anchor.width {
            assert_eq!(
                after[(anchor.x + dx, anchor.y)],
                before[(anchor.x + dx, anchor.y)],
                "wide source at {position:?}"
            );
        }
    }
    for (before, after) in before.content.iter().zip(&after.content) {
        assert!(
            !after.modifier.contains(Modifier::UNDERLINED)
                || before.modifier.contains(Modifier::UNDERLINED),
            "scope tracing must never add an underline"
        );
    }
}

fn assert_smooth(route: &Route, buffer: &Buffer, area: Rect, complete: bool) {
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
    assert_eq!(
        cells.len(),
        route.cells.len(),
        "the contour visits each whitespace cell once"
    );
    assert!(!cells.is_empty());
    let top = cells.keys().map(|&(_, y)| y).min().unwrap();
    let bottom = cells.keys().map(|&(_, y)| y).max().unwrap();
    for (&(x, y), &ports) in &cells {
        assert!(
            (1..=2).contains(&ports.count_ones()),
            "smooth contours have no crossing or tee at ({x}, {y})"
        );
        if complete && route.first.y != route.second.y {
            assert_eq!(ports.count_ones(), 2, "a complete side has no loose end");
        }
        assert!(
            buffer[(x as u16, y as u16)].symbol().trim().is_empty(),
            "trace replaces content at ({x}, {y})"
        );
        for (direction, opposite, dx, dy) in
            [(1, 2, 0, -1), (2, 1, 0, 1), (4, 8, 1, 0), (8, 4, -1, 0)]
        {
            if ports & direction == 0 {
                continue;
            }
            let next = (x + dx, y + dy);
            if cells.get(&next).is_some_and(|ports| ports & opposite != 0) {
                continue;
            }
            let outside = next.0 < i32::from(area.x.saturating_sub(1))
                || next.0 >= i32::from(area.right())
                || next.1 < i32::from(area.y)
                || next.1 >= i32::from(area.bottom());
            let source = !outside
                && (!buffer[(next.0 as u16, next.1 as u16)]
                    .symbol()
                    .trim()
                    .is_empty()
                    || (1..=2).any(|offset| {
                        next.0 - offset >= i32::from(area.x)
                            && unicode_width::UnicodeWidthStr::width(
                                buffer[((next.0 - offset) as u16, next.1 as u16)].symbol(),
                            ) > offset as usize
                    }));
            assert!(
                outside
                    || source
                    || !complete && (direction == 1 && y == top || direction == 2 && y == bottom),
                "contour has an unmatched port at ({x}, {y}) -> {next:?}"
            );
        }
    }
}

fn assert_both_sides(route: &Route, geometry: &Geometry, row: usize) {
    let y = geometry.area.y + row as u16;
    let source: Vec<_> = geometry
        .anchors
        .iter()
        .filter(|a| a.y == y && !matches!(a.ch, ' ' | '\t' | '\0'))
        .collect();
    let left = source.iter().map(|a| a.x).min().unwrap();
    let right = source.iter().map(|a| a.x + a.width - 1).max().unwrap();
    assert!(
        route.cells.iter().any(|cell| cell.y == y && cell.x < left),
        "missing left side on row {row}"
    );
    assert!(
        route.cells.iter().any(|cell| cell.y == y && cell.x > right),
        "missing right side on row {row}"
    );
}

#[test]
fn preprocessor_navigation_paints_only_the_current_branch_and_preserves_directives() {
    let source = [
        "void run() {",
        "#ifdef FIRST",
        "    one();",
        "#elif SECOND",
        "    two();",
        "#else",
        "    other();",
        "#endif",
        "}",
    ];
    let mut editor = crate::editor::Editor::test_with_text(&source.join("\n"));
    editor.language_override = Some("C++".into());
    let index = crate::animation_structural_scope::StructuralScopeRequest::from_editor(&editor)
        .unwrap()
        .build(None, || false)
        .unwrap();
    let mut state = crate::animations::AnimationState::default();
    let area = Rect::new(4, 1, 52, source.len() as u16);
    let mut cache = RouteCache::default();
    for (line, first, last) in [(1, 1, 2), (3, 3, 4), (5, 5, 6), (2, 1, 2), (7, 1, 7)] {
        editor.cursor_line = line;
        let pair = state.scope(&editor, Some(&index)).unwrap();
        assert_eq!((pair.0.0, pair.1.0), (first, last));
        let (mut buffer, geometry, gutter) = fixture(&source, area);
        let original = buffer.clone();
        let mut painter = painter(&mut buffer, &geometry, &source, pair);
        let route = scope_route(&painter, &geometry, gutter, &rows(&geometry), pair).unwrap();
        for row in first..=last {
            assert_both_sides(&route, &geometry, row);
        }
        assert!(
            route
                .cells
                .iter()
                .all(|cell| (area.y + first as u16..=area.y + last as u16).contains(&cell.y))
        );
        paint(&mut painter, &geometry, gutter, pair, &mut cache);
        assert_source_preserved(&original, painter.buffer, &geometry);
        for row in 0..source.len() {
            if (first..=last).contains(&row) {
                continue;
            }
            for x in area.x - 1..area.right() {
                let point = (x, area.y + row as u16);
                assert_eq!(
                    painter.buffer[point], original[point],
                    "other branch row {row}"
                );
            }
        }
    }
}

#[test]
fn inline_scope_viewport_route_excludes_neighboring_blocks_and_statements() {
    let text = [
        "fn run() {",
        "    before(); { one(); { two(); } three(); } { after(); }",
        "}",
    ];
    let area = Rect::new(4, 1, 80, 5);
    let pairs = [
        (
            (1, text[1].find('{').unwrap()),
            (1, text[1].find("} { after").unwrap()),
        ),
        (
            (1, text[1].find("{ two").unwrap()),
            (1, text[1].find("} three").unwrap()),
        ),
        (
            (1, text[1].find("{ after").unwrap()),
            (1, text[1].rfind('}').unwrap()),
        ),
    ];
    for pair in pairs {
        for tight in [true, false] {
            let (mut buffer, geometry, gutter) = fixture(&text, area);
            let original = buffer.clone();
            let painter = painter(&mut buffer, &geometry, &text, pair);
            let route =
                scope_route_with_mode(&painter, &geometry, gutter, &rows(&geometry), pair, tight)
                    .unwrap();
            assert_smooth(&route, &original, area, true);
            assert!(
                route.cells.iter().all(|cell| {
                    cell.y == area.y + 1
                        && cell.x >= area.x + pair.0.1 as u16 - 1
                        && cell.x <= area.x + pair.1.1 as u16 + 1
                }),
                "inline routes must stay beside their own braces"
            );
            assert!(
                route
                    .cells
                    .iter()
                    .any(|cell| cell.x == area.x + pair.0.1 as u16 - 1)
            );
            assert!(
                route
                    .cells
                    .iter()
                    .any(|cell| cell.x == area.x + pair.1.1 as u16 + 1)
            );
        }
    }
}

#[test]
fn compact_inline_viewport_scope_marks_its_braces_without_crossing_neighbors() {
    let text = ["", "    {{work();}};", ""];
    let pair = ((1, 5), (1, 13));
    let area = Rect::new(4, 1, 40, 3);
    let (mut buffer, geometry, gutter) = fixture(&text, area);
    let original = buffer.clone();
    let mut painter = painter(&mut buffer, &geometry, &text, pair);
    let route = scope_route(&painter, &geometry, gutter, &rows(&geometry), pair).unwrap();
    assert!(
        route.cells.is_empty(),
        "the only free cells belong to neighboring code"
    );
    paint(
        &mut painter,
        &geometry,
        gutter,
        pair,
        &mut RouteCache::default(),
    );
    assert_source_preserved(&original, painter.buffer, &geometry);
    for x in area.x..area.right() {
        let point = (x, area.y + 1);
        if x == area.x + 5 || x == area.x + 13 {
            assert_ne!(painter.buffer[point].bg, original[point].bg);
        } else {
            assert_eq!(painter.buffer[point], original[point]);
        }
    }
}

#[test]
fn scope_bubble_starts_and_ends_beside_source_without_using_spare_rows() {
    let area = Rect::new(4, 1, 52, PROFILE.len() as u16);
    let (mut buffer, geometry, gutter) = fixture(PROFILE, area);
    let original = buffer.clone();
    let mut painter = painter(&mut buffer, &geometry, PROFILE, PROFILE_PAIR);
    let route = scope_route(&painter, &geometry, gutter, &rows(&geometry), PROFILE_PAIR).unwrap();
    assert_smooth(&route, &original, area, true);
    for row in 1..8 {
        assert_both_sides(&route, &geometry, row);
    }
    assert!(
        route
            .cells
            .iter()
            .all(|cell| (route.first.y..=route.second.y).contains(&cell.y)),
        "traces must stay between the opening and closing source rows"
    );
    paint(
        &mut painter,
        &geometry,
        gutter,
        PROFILE_PAIR,
        &mut RouteCache::default(),
    );
    assert_source_preserved(&original, painter.buffer, &geometry);
    for y in [area.y, area.bottom() - 1] {
        for x in area.x.saturating_sub(1)..area.right() {
            assert_eq!(
                painter.buffer[(x, y)],
                original[(x, y)],
                "spare row above or below the scope was decorated"
            );
        }
    }
}

#[test]
fn scope_bubble_follows_changing_row_extents_without_overlapping_notches() {
    let area = Rect::new(4, 1, 52, PROFILE.len() as u16);
    let (mut buffer, geometry, gutter) = fixture(PROFILE, area);
    let painter = painter(&mut buffer, &geometry, PROFILE, PROFILE_PAIR);
    let route = scope_route(&painter, &geometry, gutter, &rows(&geometry), PROFILE_PAIR).unwrap();
    assert_smooth(&route, painter.buffer, area, true);
    let right = |row| {
        route
            .cells
            .iter()
            .filter(|cell| cell.y == area.y + row)
            .map(|cell| cell.x)
            .max()
            .unwrap()
    };
    assert!(
        right(4) > right(2),
        "the right side follows the longer field rather than one bounding rectangle"
    );
    assert!(
        right(4) > right(6),
        "the trace comes back after the longer field"
    );
}

#[test]
fn nested_map_bubble_takes_the_tight_feasible_indent_steps_and_right_recesses() {
    let area = Rect::new(4, 1, 76, NESTED_MAP.len() as u16);
    for pair in nested_map_pairs() {
        let (mut buffer, geometry, gutter) = fixture(NESTED_MAP, area);
        let original = buffer.clone();
        let mut painter = painter(&mut buffer, &geometry, NESTED_MAP, pair);
        let route = scope_route(&painter, &geometry, gutter, &rows(&geometry), pair).unwrap();
        // The inner map ends in `})`: its suffix boxes the selected `}` on
        // the right. That side may end in a source-facing halfline while the
        // outer function, whose closing brace has no suffix, stays complete.
        let complete = NESTED_MAP[pair.1.0].trim_end().len() == pair.1.1 + 1;
        assert_smooth(&route, &original, area, complete);
        for line in pair.0.0 + 1..pair.1.0 {
            let extent = |line: usize| {
                let row = NESTED_MAP[line];
                let left = area.x + row.chars().position(|ch| ch != ' ').unwrap() as u16;
                let right = area.x + row.trim_end().len() as u16 - 1;
                (left, right)
            };
            let (left, right) = extent(line);
            let (previous_left, previous_right) = extent(line - 1);
            let (next_left, next_right) = extent(line + 1);
            // Each vertical join must clear source on both of its rows. A
            // horizontal corner on this row can join those two positions,
            // without inventing a one-row U turn or revisiting a cell.
            let tight_left = left.min(previous_left.max(next_left)) - 1;
            let tight_right = right.max(previous_right.min(next_right)) + 1;
            let y = area.y + line as u16;
            let actual_left = route
                .cells
                .iter()
                .filter(|cell| cell.y == y && cell.x < left)
                .map(|cell| cell.x)
                .max()
                .expect("each nested row has a left exterior");
            let actual_right = route
                .cells
                .iter()
                .filter(|cell| cell.y == y && cell.x > right)
                .map(|cell| cell.x)
                .min()
                .expect("each nested row has a right exterior");
            assert_eq!(
                actual_left, tight_left,
                "left boundary leaves avoidable indentation space on line {line}, pair {pair:?}"
            );
            assert_eq!(
                actual_right, tight_right,
                "right boundary leaves avoidable trailing space on line {line}, pair {pair:?}"
            );
        }
        paint(
            &mut painter,
            &geometry,
            gutter,
            pair,
            &mut RouteCache::default(),
        );
        assert_source_preserved(&original, painter.buffer, &geometry);
        for y in [area.y, area.bottom() - 1] {
            for x in area.x.saturating_sub(1)..area.right() {
                assert_eq!(
                    painter.buffer[(x, y)],
                    original[(x, y)],
                    "tightening must not acquire exterior cap rows"
                );
            }
        }
    }
}

#[test]
fn isolated_one_row_valleys_keep_a_smooth_vertical_contour() {
    let text = [
        "",
        "    Scope {",
        "        longer_field,",
        "            small,",
        "        longer_field,",
        "    }",
        "",
    ];
    let pair = ((1, 10), (5, 4));
    let area = Rect::new(4, 1, 52, text.len() as u16);
    let (mut buffer, geometry, gutter) = fixture(&text, area);
    let original = buffer.clone();
    let mut painter = painter(&mut buffer, &geometry, &text, pair);
    let route = scope_route(&painter, &geometry, gutter, &rows(&geometry), pair).unwrap();
    assert_smooth(&route, &original, area, true);
    // A one-row inward step has no room for two separate rounded corners.
    // Both sides therefore use the nearest common vertical join instead of
    // drawing a little loop over the same whitespace cells.
    let left = area.x + text[2].chars().position(|ch| ch != ' ').unwrap() as u16 - 1;
    let right = area.x + text[2].len() as u16;
    let middle_y = area.y + 3;
    for x in [left, right] {
        let cell = route
            .cells
            .iter()
            .find(|cell| cell.x == x && cell.y == middle_y)
            .expect("one-row valley keeps the common nearest exterior");
        assert_eq!(cell.symbol, "│", "the isolated valley must stay smooth");
    }
    paint(
        &mut painter,
        &geometry,
        gutter,
        pair,
        &mut RouteCache::default(),
    );
    assert_source_preserved(&original, painter.buffer, &geometry);
}

#[test]
fn dense_opening_and_closing_rows_are_surrounded_without_marking_source() {
    let text = ["    lorem ipsum { dolor", "        body();", "    } suffix"];
    let pair = ((0, 16), (2, 4));
    let area = Rect::new(4, 1, 48, 3);
    let (mut buffer, geometry, gutter) = fixture(&text, area);
    let original = buffer.clone();
    let mut painter = painter(&mut buffer, &geometry, &text, pair);
    let route = scope_route(&painter, &geometry, gutter, &rows(&geometry), pair).unwrap();
    assert_smooth(&route, &original, area, false);
    for row in 0..2 {
        assert_both_sides(&route, &geometry, row);
    }
    paint(
        &mut painter,
        &geometry,
        gutter,
        pair,
        &mut RouteCache::default(),
    );
    assert_source_preserved(&original, painter.buffer, &geometry);
}

fn internal_whitespace(geometry: &Geometry, line: usize) -> Vec<(u16, u16)> {
    let source: Vec<_> = geometry
        .anchors
        .iter()
        .filter(|anchor| anchor.line == line && !matches!(anchor.ch, ' ' | '\t' | '\0'))
        .collect();
    let left = source.iter().map(|anchor| anchor.x).min().unwrap();
    let right = source
        .iter()
        .map(|anchor| anchor.x + anchor.width - 1)
        .max()
        .unwrap();
    geometry
        .anchors
        .iter()
        .filter(|anchor| anchor.line == line && matches!(anchor.ch, ' ' | '\t'))
        .flat_map(|anchor| (0..anchor.width).map(move |dx| (anchor.x + dx, anchor.y)))
        .filter(|&(x, _)| x > left && x < right)
        .collect()
}

#[test]
fn delimiter_row_word_gaps_complete_the_bubble_with_horizontal_strokes() {
    let text = [
        "    if  frame.ready() \t {",
        "        draw( frame );",
        "    end  value \t }",
    ];
    let pair = (
        (0, text[0].chars().position(|ch| ch == '{').unwrap()),
        (2, text[2].chars().position(|ch| ch == '}').unwrap()),
    );
    let area = Rect::new(4, 1, 52, 3);
    let (mut buffer, geometry, gutter) = fixture(&text, area);
    let original = buffer.clone();
    let mut painter = painter(&mut buffer, &geometry, &text, pair);
    let route = scope_route(&painter, &geometry, gutter, &rows(&geometry), pair).unwrap();
    assert_smooth(&route, &original, area, true);
    paint(
        &mut painter,
        &geometry,
        gutter,
        pair,
        &mut RouteCache::default(),
    );
    for line in [0, 2] {
        let gaps = internal_whitespace(&geometry, line);
        assert!(gaps.len() >= 4, "fixture includes repeated and tabbed gaps");
        for position in gaps {
            assert_eq!(
                painter.buffer[position].symbol(),
                "─",
                "delimiter-row whitespace at {position:?} must carry the boundary"
            );
        }
    }
    for position in internal_whitespace(&geometry, 1) {
        assert_eq!(
            painter.buffer[position], original[position],
            "body word gaps remain inside the bubble"
        );
    }
    assert_source_preserved(&original, painter.buffer, &geometry);
}

#[test]
fn delimiter_row_gap_strokes_preserve_masks_and_annotation_obstacles() {
    let text = [
        "    if    frame.ready()    {",
        "        draw( frame );",
        "    end    value    }",
    ];
    let pair = ((0, text[0].len() - 1), (2, text[2].len() - 1));
    let area = Rect::new(4, 1, 52, 3);
    let (mut buffer, mut geometry, gutter) = fixture(&text, area);
    let gaps = internal_whitespace(&geometry, 0);
    let [selection, caret, diagnostic, annotation] = [gaps[0], gaps[1], gaps[2], gaps[3]];
    buffer[selection].set_style(
        Style::default()
            .fg(Color::White)
            .bg(Color::Red)
            .add_modifier(Modifier::REVERSED),
    );
    buffer[caret].set_style(
        Style::default()
            .fg(Color::Black)
            .bg(Color::White)
            .add_modifier(Modifier::BOLD),
    );
    buffer[diagnostic].set_style(
        Style::default()
            .fg(Color::Yellow)
            .bg(Color::Blue)
            .add_modifier(Modifier::UNDERLINED)
            .underline_color(Color::Red),
    );
    buffer[annotation].set_style(
        Style::default()
            .fg(Color::Gray)
            .bg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    );
    geometry.obstacles.insert(annotation);
    let original = buffer.clone();
    let mut painter = painter(&mut buffer, &geometry, &text, pair);
    painter.protected.to_mut().extend([selection, diagnostic]);
    painter.carets.to_mut().insert(caret);
    paint(
        &mut painter,
        &geometry,
        gutter,
        pair,
        &mut RouteCache::default(),
    );
    for position in [selection, caret, diagnostic, annotation] {
        assert_eq!(
            painter.buffer[position], original[position],
            "gap stroke must preserve a masked or annotation cell at {position:?}"
        );
    }
    for position in &gaps[4..] {
        assert_eq!(
            painter.buffer[*position].symbol(),
            "─",
            "unmasked whitespace still joins the boundary"
        );
    }
    assert_source_preserved(&original, painter.buffer, &geometry);
}

#[test]
fn clipped_delimiter_does_not_turn_visible_word_gaps_into_a_boundary() {
    let text = ["    if    ready() {", "        body();", "    }"];
    let pair = ((0, text[0].len() - 1), (2, 4));
    let area = Rect::new(4, 1, 16, 3);
    let (mut buffer, geometry, gutter) = fixture(&text, area);
    assert!(geometry.at(pair.0).is_none());
    let original = buffer.clone();
    let mut painter = painter(&mut buffer, &geometry, &text, pair);
    paint(
        &mut painter,
        &geometry,
        gutter,
        pair,
        &mut RouteCache::default(),
    );
    for position in internal_whitespace(&geometry, 0) {
        assert_eq!(
            painter.buffer[position], original[position],
            "a clipped opener does not create an invented top boundary"
        );
    }
    assert_source_preserved(&original, painter.buffer, &geometry);
}

#[test]
fn cached_bubble_updates_word_gap_strokes_when_row_extents_stay_unchanged() {
    let first = ["    if alpha  {", "        body();", "    end  value }"];
    let second = ["    if  alpha {", "        body();", "    end  value }"];
    let pair = ((0, 14), (2, 15));
    let area = Rect::new(4, 1, 52, 3);
    let mut cache = RouteCache::default();
    let mut boundaries = Vec::new();
    for (text, expected_builds) in [(&first, 1), (&first, 1), (&second, 2), (&second, 2)] {
        let (mut buffer, geometry, gutter) = fixture(text, area);
        let original = buffer.clone();
        let mut painter = painter(&mut buffer, &geometry, text, pair);
        paint(&mut painter, &geometry, gutter, pair, &mut cache);
        let gaps = internal_whitespace(&geometry, 0);
        for &position in &gaps {
            assert_eq!(
                painter.buffer[position].symbol(),
                "─",
                "cached boundary must reflect current source whitespace"
            );
        }
        assert_source_preserved(&original, painter.buffer, &geometry);
        assert_eq!(
            cache.builds, expected_builds,
            "word-gap layout changes must invalidate the contour even with identical extents"
        );
        boundaries.push(gaps);
    }
    assert_eq!(boundaries[0], boundaries[1]);
    assert_ne!(boundaries[1], boundaries[2]);
    assert_eq!(boundaries[2], boundaries[3]);
}

#[test]
fn single_row_scope_uses_only_source_facing_marks_on_its_own_row() {
    let text = ["", "    call { work(); } suffix", ""];
    let pair = ((1, 9), (1, 19));
    let area = Rect::new(4, 1, 48, 3);
    let (mut buffer, geometry, gutter) = fixture(&text, area);
    let original = buffer.clone();
    let mut painter = painter(&mut buffer, &geometry, &text, pair);
    let route = scope_route(&painter, &geometry, gutter, &rows(&geometry), pair).unwrap();
    assert_smooth(&route, &original, area, true);
    for x in [area.x + pair.0.1 as u16 - 1, area.x + pair.1.1 as u16 + 1] {
        assert!(
            route
                .cells
                .iter()
                .any(|cell| cell.x == x && cell.y == area.y + 1)
        );
    }
    assert!(
        route.cells.iter().all(|cell| cell.y == area.y + 1),
        "a one-row scope must not acquire rows above or below it"
    );
    assert!(
        route
            .cells
            .iter()
            .all(|cell| matches!(cell.symbol, "╶" | "╴" | "─")),
        "single-row endpoints face the source without extending vertically"
    );
    paint(
        &mut painter,
        &geometry,
        gutter,
        pair,
        &mut RouteCache::default(),
    );
    assert_source_preserved(&original, painter.buffer, &geometry);
    for y in [area.y, area.bottom() - 1] {
        for x in area.x.saturating_sub(1)..area.right() {
            assert_eq!(painter.buffer[(x, y)], original[(x, y)]);
        }
    }
}

#[test]
fn bubble_top_right_trace_starts_beside_the_brace_before_turning_around_a_wide_body() {
    let text = [
        "",
        "    AgentState {",
        "        a_really_wide_body_value,",
        "        a_really_wide_body_value,",
        "    }",
        "",
    ];
    let pair = ((1, 15), (4, 4));
    let area = Rect::new(4, 1, 52, text.len() as u16);
    let (mut buffer, geometry, gutter) = fixture(&text, area);
    let original = buffer.clone();
    let mut painter = painter(&mut buffer, &geometry, &text, pair);
    let route = scope_route(&painter, &geometry, gutter, &rows(&geometry), pair).unwrap();
    assert_smooth(&route, &original, area, true);
    let brace = geometry.at(pair.0).unwrap();
    let endpoint = route
        .cells
        .iter()
        .find(|cell| cell.y == brace.y && cell.x == brace.x + 1)
        .expect("the trace must start in the first whitespace cell beside the opening brace");
    assert_ne!(
        super::super::scope_connections(endpoint.symbol) & 8,
        0,
        "the nearest trace cell must face the opening brace"
    );
    assert!(
        route
            .cells
            .iter()
            .any(|cell| cell.y == brace.y && cell.x > brace.x + 1 && cell.symbol == "╮"),
        "the top right trace turns smoothly beyond the wider body"
    );
    assert!(
        route
            .cells
            .iter()
            .all(|cell| (brace.y..=geometry.at(pair.1).unwrap().y).contains(&cell.y))
    );
    paint(
        &mut painter,
        &geometry,
        gutter,
        pair,
        &mut RouteCache::default(),
    );
    assert_source_preserved(&original, painter.buffer, &geometry);
}

#[test]
fn boxed_delimiters_get_two_whitespace_sides_in_both_themes() {
    for theme in [
        crate::app_theme::EditorTheme::Dark,
        crate::app_theme::EditorTheme::Light,
    ] {
        let area = Rect::new(4, 1, 48, BOXED.len() as u16);
        let (mut buffer, geometry, gutter) = fixture(BOXED, area);
        let colors = theme.colors();
        buffer.set_style(
            buffer.area,
            Style::default().fg(colors.fg_text).bg(colors.bg_editor),
        );
        let original = buffer.clone();
        let mut painter = painter(&mut buffer, &geometry, BOXED, BOXED_PAIR);
        painter.colors = colors;
        let route = scope_route(&painter, &geometry, gutter, &rows(&geometry), BOXED_PAIR).unwrap();
        assert_smooth(&route, &original, area, false);
        for row in 0..BOXED.len() - 1 {
            assert_both_sides(&route, &geometry, row);
        }
        paint(
            &mut painter,
            &geometry,
            gutter,
            BOXED_PAIR,
            &mut RouteCache::default(),
        );
        assert_source_preserved(&original, painter.buffer, &geometry);
    }
}

#[test]
fn bubble_preserves_existing_diagnostics_selections_and_caret_styles() {
    let area = Rect::new(4, 1, 52, PROFILE.len() as u16);
    let (mut buffer, geometry, gutter) = fixture(PROFILE, area);
    let diagnostic = (area.x + 9, area.y + 4);
    buffer[diagnostic].set_style(
        Style::default()
            .fg(Color::Yellow)
            .bg(Color::Blue)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            .underline_color(Color::Red),
    );
    let mut painter = painter(&mut buffer, &geometry, PROFILE, PROFILE_PAIR);
    let route = scope_route(&painter, &geometry, gutter, &rows(&geometry), PROFILE_PAIR).unwrap();
    let protected = (route.cells[0].x, route.cells[0].y);
    let caret = (
        route.cells[route.cells.len() / 2].x,
        route.cells[route.cells.len() / 2].y,
    );
    painter.buffer[protected].set_style(
        Style::default()
            .bg(Color::Red)
            .add_modifier(Modifier::REVERSED),
    );
    painter.buffer[caret].set_style(
        Style::default()
            .fg(Color::Black)
            .bg(Color::White)
            .add_modifier(Modifier::BOLD),
    );
    let original = painter.buffer.clone();
    painter.protected.to_mut().insert(protected);
    painter.carets.to_mut().insert(caret);
    assert_eq!(
        points(&scope_route(&painter, &geometry, gutter, &rows(&geometry), PROFILE_PAIR).unwrap()),
        points(&route),
        "masking does not alter geometry"
    );
    paint(
        &mut painter,
        &geometry,
        gutter,
        PROFILE_PAIR,
        &mut RouteCache::default(),
    );
    for position in [diagnostic, protected, caret] {
        assert_eq!(painter.buffer[position], original[position]);
    }
    assert_source_preserved(&original, painter.buffer, &geometry);
}

#[test]
fn bubble_treats_tabs_wide_glyphs_and_literal_box_characters_as_source() {
    let text = ["", "\tScope 界 {", "\t\t界 │ value,", "\t}", ""];
    let pair = ((1, 9), (3, 1));
    let area = Rect::new(4, 1, 48, 5);
    let (mut buffer, geometry, gutter) = fixture(&text, area);
    let original = buffer.clone();
    let mut painter = painter(&mut buffer, &geometry, &text, pair);
    let route = scope_route(&painter, &geometry, gutter, &rows(&geometry), pair).unwrap();
    assert_smooth(&route, &original, area, true);
    assert_both_sides(&route, &geometry, 2);
    paint(
        &mut painter,
        &geometry,
        gutter,
        pair,
        &mut RouteCache::default(),
    );
    assert_source_preserved(&original, painter.buffer, &geometry);
}

#[test]
fn body_only_viewport_has_two_open_sides_and_no_invented_caps() {
    let text = [
        "        first();",
        "        wider_second();",
        "        third();",
    ];
    let area = Rect::new(4, 1, 48, 3);
    let (mut buffer, mut geometry, gutter) = fixture(&text, area);
    for anchor in &mut geometry.anchors {
        anchor.line += 10;
    }
    let original = buffer.clone();
    let mut painter = Painter::new(
        &mut buffer,
        area,
        crate::app_theme::EditorTheme::Dark.colors(),
        HashSet::new(),
        HashSet::new(),
        HashSet::new(),
    );
    let pair = ((0, 4), (40, 4));
    let route = scope_route(&painter, &geometry, gutter, &rows(&geometry), pair).unwrap();
    assert!(!route.first.visible && !route.second.visible);
    assert_smooth(&route, &original, area, false);
    for row in 0..3 {
        assert_both_sides(&route, &geometry, row);
    }
    assert!(
        route
            .cells
            .iter()
            .all(|cell| (area.y..area.bottom()).contains(&cell.y))
    );
    paint(
        &mut painter,
        &geometry,
        gutter,
        pair,
        &mut RouteCache::default(),
    );
    assert_source_preserved(&original, painter.buffer, &geometry);
}

#[test]
fn horizontally_clipped_delimiter_keeps_source_and_trace_within_the_viewport() {
    let area = Rect::new(4, 1, 15, BOXED.len() as u16);
    let (mut buffer, geometry, gutter) = fixture(BOXED, area);
    assert!(geometry.at(BOXED_PAIR.0).is_none());
    let original = buffer.clone();
    let mut painter = painter(&mut buffer, &geometry, BOXED, BOXED_PAIR);
    let route = scope_route(&painter, &geometry, gutter, &rows(&geometry), BOXED_PAIR).unwrap();
    assert_smooth(&route, &original, area, false);
    assert!(
        route
            .cells
            .iter()
            .all(|cell| cell.x >= gutter.x && cell.x < area.right())
    );
    paint(
        &mut painter,
        &geometry,
        gutter,
        BOXED_PAIR,
        &mut RouteCache::default(),
    );
    assert_source_preserved(&original, painter.buffer, &geometry);
}

#[test]
fn cached_bubble_tracks_body_growth_and_shrinkage_away_from_old_trace_cells() {
    let original_text = [
        "",
        "    Scope {",
        "        a_very_long_body_value,",
        "    }",
        "",
    ];
    let shorter_text = ["", "    Scope {", "        tiny,", "    }", ""];
    let pair = ((1, 10), (3, 4));
    let area = Rect::new(4, 1, 52, 5);
    let mut cache = RouteCache::default();
    let mut shapes = Vec::new();
    for (text, expected_builds) in [
        (&original_text, 1),
        (&original_text, 1),
        (&shorter_text, 2),
        (&shorter_text, 2),
        (&original_text, 2),
    ] {
        let (mut buffer, geometry, gutter) = fixture(text, area);
        let mut painter = painter(&mut buffer, &geometry, text, pair);
        paint(&mut painter, &geometry, gutter, pair, &mut cache);
        shapes.push(
            painter
                .buffer
                .content
                .iter()
                .enumerate()
                .filter_map(|(index, cell)| {
                    (super::super::scope_connections(cell.symbol()) != 0 && cell.symbol() != " ")
                        .then_some((index, cell.symbol().to_owned()))
                })
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            cache.builds, expected_builds,
            "outline extent changes must invalidate the cached contour"
        );
    }
    assert_eq!(shapes[0], shapes[1]);
    assert_ne!(shapes[1], shapes[2]);
    assert_eq!(shapes[2], shapes[3]);
    assert_eq!(shapes[0], shapes[4]);
}

#[test]
fn inline_annotation_moves_the_contour_without_drawing_over_blame() {
    let area = Rect::new(4, 1, 52, PROFILE.len() as u16);
    let (mut buffer, mut geometry, gutter) = fixture(PROFILE, area);
    let blame = (area.x + 30, area.y + 2);
    buffer.set_string(
        blame.0,
        blame.1,
        "Henri, 7d ago",
        Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::ITALIC),
    );
    for x in blame.0..blame.0 + 13 {
        geometry.obstacles.insert((x, blame.1));
    }
    let original = buffer.clone();
    let mut painter = painter(&mut buffer, &geometry, PROFILE, PROFILE_PAIR);
    let route = scope_route(&painter, &geometry, gutter, &rows(&geometry), PROFILE_PAIR).unwrap();
    assert_smooth(&route, &original, area, false);
    assert!(
        route
            .cells
            .iter()
            .all(|cell| !geometry.obstacles.contains(&(cell.x, cell.y)))
    );
    paint(
        &mut painter,
        &geometry,
        gutter,
        PROFILE_PAIR,
        &mut RouteCache::default(),
    );
    for x in blame.0..blame.0 + 13 {
        assert_eq!(painter.buffer[(x, blame.1)], original[(x, blame.1)]);
    }
    assert_source_preserved(&original, painter.buffer, &geometry);
}

#[test]
fn cached_bubble_returns_inward_when_a_registered_annotation_is_removed() {
    annotation_removal_restores_the_original_contour(true);
}

#[test]
fn cached_bubble_returns_inward_when_rendered_annotation_text_is_removed() {
    annotation_removal_restores_the_original_contour(false);
}

fn annotation_removal_restores_the_original_contour(registered: bool) {
    let area = Rect::new(4, 1, 52, PROFILE.len() as u16);
    let (base, geometry, gutter) = fixture(PROFILE, area);
    let mut frame = base.clone();
    let painter = painter(&mut frame, &geometry, PROFILE, PROFILE_PAIR);
    let ordinary =
        scope_route(&painter, &geometry, gutter, &rows(&geometry), PROFILE_PAIR).unwrap();
    let annotation_y = area.y + 4;
    let annotation_x = ordinary
        .cells
        .iter()
        .filter(|cell| cell.y == annotation_y)
        .map(|cell| cell.x)
        .max()
        .unwrap();
    let contour = |buffer: &Buffer| {
        buffer
            .content
            .iter()
            .enumerate()
            .filter_map(|(index, cell)| {
                (super::super::scope_connections(cell.symbol()) != 0)
                    .then_some((index, cell.symbol().to_owned()))
            })
            .collect::<Vec<_>>()
    };
    let mut cache = RouteCache::default();
    let mut shapes = Vec::new();
    for annotated in [false, true, true, false, false] {
        let (_, mut geometry, _) = fixture(PROFILE, area);
        let mut frame = base.clone();
        if annotated {
            frame.set_string(
                annotation_x,
                annotation_y,
                "annotation",
                Style::default().fg(Color::Gray),
            );
            if registered {
                for x in annotation_x..annotation_x + 10 {
                    geometry.obstacles.insert((x, annotation_y));
                }
            }
        }
        let annotation_before = frame.clone();
        let mut painter = self::painter(&mut frame, &geometry, PROFILE, PROFILE_PAIR);
        paint(&mut painter, &geometry, gutter, PROFILE_PAIR, &mut cache);
        if annotated {
            for x in annotation_x..annotation_x + 10 {
                assert_eq!(
                    painter.buffer[(x, annotation_y)],
                    annotation_before[(x, annotation_y)],
                    "tracing changes annotation text"
                );
            }
        }
        shapes.push(contour(painter.buffer));
    }
    assert_ne!(
        shapes[0], shapes[1],
        "the annotation must push the contour outwards"
    );
    assert_eq!(
        shapes[1], shapes[2],
        "an unchanged annotation keeps the contour stable"
    );
    assert_eq!(
        shapes[0], shapes[3],
        "removed annotation must restore the inward contour even though the old detour remains free"
    );
    assert_eq!(shapes[3], shapes[4]);
}

#[allow(clippy::too_many_arguments)]
fn settings_frame(
    base: &Buffer,
    geometry: &Geometry,
    gutter: Rect,
    text: &[&str],
    pair: Pair,
    settings: &AnimationSettings,
    events: &[AnimationEvent],
    now: Instant,
    cache: &mut RouteCache,
) -> Buffer {
    let mut frame = base.clone();
    let mut painter = painter(&mut frame, geometry, text, pair);
    draw_cached(
        &mut painter,
        geometry,
        settings,
        events,
        Some(pair),
        gutter,
        now,
        cache,
    );
    painter.finish();
    frame
}

fn motion_settings() -> AnimationSettings {
    let mut settings = AnimationSettings::default();
    settings.set(Effect::ScopeConnector, true);
    settings.set_duration(Effect::ScopeConnector, 400);
    settings.set_intensity(Effect::ScopeConnector, 100);
    settings
}

#[test]
fn cached_scope_bubble_switches_tightness_without_editing_the_document() {
    let area = Rect::new(4, 1, 76, NESTED_MAP.len() as u16);
    let (base, geometry, gutter) = fixture(NESTED_MAP, area);
    let pair = nested_map_pairs()[0];
    let now = Instant::now();
    let mut settings = motion_settings();
    let mut cache = RouteCache::default();
    let mut frames = Vec::new();
    let mut builds = Vec::new();
    for tight in [true, false, false, true] {
        settings.scope_tight = tight;
        let frame = settings_frame(
            &base,
            &geometry,
            gutter,
            NESTED_MAP,
            pair,
            &settings,
            &[],
            now,
            &mut cache,
        );
        assert_source_preserved(&base, &frame, &geometry);
        frames.push(frame);
        builds.push(cache.builds);
    }
    assert_ne!(
        frames[0], frames[1],
        "loose mode must change the contour immediately"
    );
    assert!(
        builds[1] > builds[0],
        "the cached tight contour cannot serve loose mode"
    );
    assert_eq!(frames[1], frames[2]);
    assert_eq!(
        builds[1], builds[2],
        "unchanged loose mode reuses its route"
    );
    assert_eq!(
        frames[0], frames[3],
        "returning to tight mode restores the original shape"
    );
}

#[test]
fn loose_scope_bubble_keeps_a_straight_left_edge_and_smooths_short_right_recesses() {
    let text = [
        "    Scope {",
        "        very_long_line_of_code,",
        "        very_long_line_of_code,",
        "        short,",
        "        short,",
        "        very_long_line_of_code,",
        "        very_long_line_of_code,",
        "    }",
    ];
    let pair = ((0, 10), (7, 4));
    let area = Rect::new(4, 1, 52, text.len() as u16);
    let (base, geometry, gutter) = fixture(&text, area);
    let mut frame = base.clone();
    let painter = painter(&mut frame, &geometry, &text, pair);
    let tight =
        scope_route_with_mode(&painter, &geometry, gutter, &rows(&geometry), pair, true).unwrap();
    let loose =
        scope_route_with_mode(&painter, &geometry, gutter, &rows(&geometry), pair, false).unwrap();
    assert_smooth(&tight, &base, area, true);
    assert_smooth(&loose, &base, area, true);
    let left = area.x + 3;
    for y in area.y..area.bottom() - 1 {
        assert!(
            loose.cells.iter().any(|cell| cell.x == left
                && cell.y == y
                && super::super::scope_connections(cell.symbol) & super::super::SOUTH != 0),
            "the left side indents on row {y}"
        );
        assert!(
            loose
                .cells
                .iter()
                .filter(|cell| cell.y == y && cell.x < area.x + 8)
                .all(|cell| cell.x == left),
            "loose mode leaves an interior left notch on row {y}"
        );
    }
    let middle = area.y + 3;
    let downward_right = |route: &Route| {
        route
            .cells
            .iter()
            .find(|cell| {
                cell.y == middle
                    && cell.x > area.x + 13
                    && super::super::scope_connections(cell.symbol) & super::super::SOUTH != 0
            })
            .map(|cell| cell.x)
            .unwrap()
    };
    assert!(
        downward_right(&loose) > downward_right(&tight) + 8,
        "loose mode should bridge a two-row indentation on the right instead of cutting into the code silhouette"
    );
}

#[test]
fn loose_scope_bubble_joins_indented_opening_and_closing_rows_to_its_outer_left_edge() {
    for (text, pair) in [
        (
            ["        Scope {", "    body();", "        }"],
            ((0, 14), (2, 8)),
        ),
        (
            ["    Scope {", "        body();", "        }"],
            ((0, 10), (2, 8)),
        ),
        (
            ["        Scope {", "        body();", "    }"],
            ((0, 14), (2, 4)),
        ),
    ] {
        let area = Rect::new(4, 1, 40, text.len() as u16);
        let (base, geometry, gutter) = fixture(&text, area);
        let mut scratch = base.clone();
        let painter = painter(&mut scratch, &geometry, &text, pair);
        let route =
            scope_route_with_mode(&painter, &geometry, gutter, &rows(&geometry), pair, false)
                .unwrap();
        assert_smooth(&route, &base, area, true);
        let outer_left = geometry
            .anchors
            .iter()
            .filter(|a| !matches!(a.ch, ' ' | '\t' | '\0'))
            .map(|a| a.x)
            .min()
            .unwrap()
            - 1;
        for (row, vertical) in [(0, super::super::SOUTH), (2, super::super::NORTH)] {
            let y = area.y + row;
            let source_left = geometry
                .anchors
                .iter()
                .filter(|a| a.y == y && !matches!(a.ch, ' ' | '\t' | '\0'))
                .map(|a| a.x)
                .min()
                .unwrap();
            for x in outer_left..source_left {
                let cell = route
                    .cells
                    .iter()
                    .find(|cell| cell.x == x && cell.y == y)
                    .unwrap_or_else(|| {
                        panic!("missing endpoint bridge at ({x}, {y}) for {text:?}")
                    });
                let ports = super::super::scope_connections(cell.symbol);
                let expected = if x == outer_left {
                    vertical | super::super::EAST
                } else {
                    super::super::EAST | super::super::WEST
                };
                assert_eq!(
                    ports, expected,
                    "the endpoint bridge must join the outer rail and terminate beside source for {text:?}"
                );
            }
        }
        let mut settings = motion_settings();
        settings.scope_tight = false;
        let painted = settings_frame(
            &base,
            &geometry,
            gutter,
            &text,
            pair,
            &settings,
            &[],
            Instant::now(),
            &mut RouteCache::default(),
        );
        assert_source_preserved(&base, &painted, &geometry);
    }
}

#[test]
fn scope_bubble_entrance_advances_within_a_long_horizontal_stroke() {
    use std::time::Duration;

    let area = Rect::new(4, 1, 76, NESTED_MAP.len() as u16);
    let (base, geometry, gutter) = fixture(NESTED_MAP, area);
    let pair = nested_map_pairs()[0];
    let mut source = base.clone();
    let painter = painter(&mut source, &geometry, NESTED_MAP, pair);
    let route = scope_route(&painter, &geometry, gutter, &rows(&geometry), pair).unwrap();
    let mut longest = Vec::new();
    let mut run = Vec::new();
    let mut horizontal: Vec<_> = route
        .cells
        .iter()
        .filter(|cell| cell.symbol == "─")
        .collect();
    horizontal.sort_unstable_by_key(|cell| (cell.y, cell.x));
    for cell in horizontal {
        if run
            .last()
            .is_some_and(|previous: &&RouteCell| previous.y != cell.y || previous.x + 1 != cell.x)
        {
            if run.len() > longest.len() {
                longest = std::mem::take(&mut run);
            }
            run.clear();
        }
        run.push(cell);
    }
    if run.len() > longest.len() {
        longest = run;
    }
    assert!(
        longest.len() >= 8,
        "the fixture needs a substantial horizontal arm"
    );
    for adjacent in longest.windows(2) {
        assert_ne!(
            adjacent[0].cost, adjacent[1].cost,
            "neighboring columns cannot share a reveal time"
        );
    }
    let started = Instant::now();
    let settings = motion_settings();
    let event = AnimationEvent {
        trigger: Trigger::Scope {
            pair,
            origin: pair.0,
            exiting: None,
        },
        started,
    };
    let mut cache = RouteCache::default();
    let counts: Vec<_> = (1..100)
        .map(|step| {
            let frame = settings_frame(
                &base,
                &geometry,
                gutter,
                NESTED_MAP,
                pair,
                &settings,
                std::slice::from_ref(&event),
                started + Duration::from_millis(step * 4),
                &mut cache,
            );
            longest
                .iter()
                .filter(|cell| frame[(cell.x, cell.y)].symbol() == cell.symbol)
                .count()
        })
        .collect();
    assert!(
        counts
            .iter()
            .any(|&count| count > 0 && count < longest.len()),
        "a horizontal arm must reveal in individual cells instead of appearing all at once: {counts:?}"
    );
    assert_eq!(counts.last().copied(), Some(longest.len()));
}

#[test]
fn scope_bubble_stays_completely_quiet_after_the_entrance() {
    use std::time::Duration;
    let area = Rect::new(4, 1, 52, PROFILE.len() as u16);
    let (base, geometry, gutter) = fixture(PROFILE, area);
    let started = Instant::now();
    let event = AnimationEvent {
        trigger: Trigger::Scope {
            pair: PROFILE_PAIR,
            origin: PROFILE_PAIR.0,
            exiting: None,
        },
        started,
    };
    let settings = motion_settings();
    let mut cache = RouteCache::default();
    let settled_at = started + Duration::from_millis(settings.duration(Effect::ScopeConnector));
    let first = settings_frame(
        &base,
        &geometry,
        gutter,
        PROFILE,
        PROFILE_PAIR,
        &settings,
        std::slice::from_ref(&event),
        settled_at,
        &mut cache,
    );
    for elapsed in [Duration::from_millis(600), Duration::from_secs(20)] {
        let later = settings_frame(
            &base,
            &geometry,
            gutter,
            PROFILE,
            PROFILE_PAIR,
            &settings,
            std::slice::from_ref(&event),
            settled_at + elapsed,
            &mut cache,
        );
        assert_eq!(
            first, later,
            "settled outlines must have no time-varying glow"
        );
    }
    let retained = settings_frame(
        &base,
        &geometry,
        gutter,
        PROFILE,
        PROFILE_PAIR,
        &settings,
        &[],
        settled_at + Duration::from_secs(20),
        &mut cache,
    );
    assert_eq!(first, retained);
    assert_source_preserved(&base, &first, &geometry);
}
#[test]
#[ignore = "exports rendered buffers for visual inspection"]
fn export_scope_bubble_motion_preview() {
    use std::time::Duration;

    fn rgb(color: Color, fallback: Color) -> String {
        let (r, g, b) = match color {
            Color::Rgb(r, g, b) => (r, g, b),
            _ => match fallback {
                Color::Rgb(r, g, b) => (r, g, b),
                _ => (224, 224, 224),
            },
        };
        format!("#{r:02x}{g:02x}{b:02x}")
    }
    let colors = crate::app_theme::EditorTheme::Dark.colors();
    let encode = |buffer: &Buffer| {
        buffer
            .content
            .iter()
            .map(|cell| {
                (
                    cell.symbol().to_owned(),
                    rgb(cell.fg, colors.fg_text),
                    rgb(cell.bg, colors.bg_editor),
                    cell.modifier.contains(Modifier::BOLD),
                    cell.modifier.contains(Modifier::ITALIC),
                    cell.modifier
                        .contains(Modifier::UNDERLINED)
                        .then(|| rgb(cell.underline_color, cell.fg)),
                )
            })
            .collect::<Vec<_>>()
    };
    let area = Rect::new(4, 1, 76, NESTED_MAP.len() as u16);
    let (mut base, geometry, gutter) = fixture(NESTED_MAP, area);
    for anchor in &geometry.anchors {
        if anchor.ch.is_alphabetic() {
            base[(anchor.x, anchor.y)].set_fg(Color::Rgb(98, 217, 232));
        }
        if matches!(anchor.ch, '{' | '}') {
            base[(anchor.x, anchor.y)].set_fg(Color::Rgb(220, 155, 112));
        }
    }
    let pair = nested_map_pairs()[0];
    let started = Instant::now();
    let event = AnimationEvent {
        trigger: Trigger::Scope {
            pair,
            origin: pair.0,
            exiting: None,
        },
        started,
    };
    let mut settings = motion_settings();
    let mut cache = RouteCache::default();
    let mut frames = Vec::new();
    let mut names = Vec::new();
    for (name, tight, millis, entrance) in [
        ("Tight bubble follows indentation", true, 400, false),
        ("Loose bubble keeps a straight left edge", false, 400, false),
        (
            "Entrance: individual columns start appearing",
            true,
            70,
            true,
        ),
        (
            "Entrance: a horizontal stroke fills progressively",
            true,
            150,
            true,
        ),
        ("Entrance: the outline completes", true, 250, true),
    ] {
        settings.scope_tight = tight;
        let frame = settings_frame(
            &base,
            &geometry,
            gutter,
            NESTED_MAP,
            pair,
            &settings,
            if entrance {
                std::slice::from_ref(&event)
            } else {
                &[]
            },
            started + Duration::from_millis(millis),
            &mut cache,
        );
        names.push(name);
        frames.push(encode(&frame));
    }
    let preview = serde_json::json!({
        "name": "Scope Bubble options and motion",
        "width": base.area.width,
        "height": base.area.height,
        "frame_names": names,
        "frames": frames,
    });
    std::fs::create_dir_all("target").unwrap();
    std::fs::write(
        "target/scope-bubble-motion-preview.json",
        serde_json::to_vec(&preview).unwrap(),
    )
    .unwrap();

    settings.scope_tight = true;
    let mut cache = RouteCache::default();
    let frames = (0..96)
        .map(|frame| {
            let buffer = settings_frame(
                &base,
                &geometry,
                gutter,
                NESTED_MAP,
                pair,
                &settings,
                std::slice::from_ref(&event),
                started + Duration::from_nanos(frame * 1_000_000_000 / 60),
                &mut cache,
            );
            encode(&buffer)
        })
        .collect::<Vec<_>>();
    let animation = serde_json::json!({
        "name": "Scope Bubble",
        "width": base.area.width,
        "height": base.area.height,
        "fps": 60,
        "duration_ms": 1600,
        "frames": frames,
    });
    std::fs::write(
        "target/scope-bubble-motion.json",
        serde_json::to_vec(&animation).unwrap(),
    )
    .unwrap();
}

#[test]
#[ignore = "exports rendered buffers for visual inspection"]
fn export_scope_bubble_preview() {
    fn rgb(color: Color, fallback: Color) -> String {
        let (r, g, b) = match color {
            Color::Rgb(r, g, b) => (r, g, b),
            _ => match fallback {
                Color::Rgb(r, g, b) => (r, g, b),
                _ => (224, 224, 224),
            },
        };
        format!("#{r:02x}{g:02x}{b:02x}")
    }
    let dense = [
        "",
        "    lorem ipsum { dolor",
        "        body();",
        "    } suffix",
        "",
    ];
    let wide = [
        "",
        "    Scope {",
        "        界 wide_text,",
        "        │ literal_source,",
        "    }",
        "",
    ];
    let condition = [
        "",
        "    if frame.ready() {",
        "        draw(frame);",
        "    }",
        "",
    ];
    let mut frames = Vec::new();
    let mut names = Vec::new();
    let area = Rect::new(4, 1, 76, NESTED_MAP.len() as u16);
    for (name, text, pair) in [
        ("Scope bubble follows the fields", PROFILE, PROFILE_PAIR),
        ("Dense delimiter rows", dense.as_slice(), ((1, 16), (3, 4))),
        (
            "Whitespace completes the condition boundary",
            condition.as_slice(),
            ((1, 21), (3, 4)),
        ),
        ("Boxed delimiters without underlines", BOXED, BOXED_PAIR),
        (
            "Tight nested map: outer scope",
            NESTED_MAP,
            nested_map_pairs()[0],
        ),
        (
            "Tight nested map: inner scope",
            NESTED_MAP,
            nested_map_pairs()[1],
        ),
        (
            "Wide glyphs and literal box source",
            wide.as_slice(),
            ((1, 10), (4, 4)),
        ),
    ] {
        let (mut buffer, geometry, gutter) = fixture(text, area);
        let colors = crate::app_theme::EditorTheme::Dark.colors();
        for anchor in &geometry.anchors {
            if anchor.ch.is_alphabetic() {
                buffer[(anchor.x, anchor.y)].set_fg(Color::Rgb(98, 217, 232));
            }
            if matches!(anchor.ch, '{' | '}') {
                buffer[(anchor.x, anchor.y)].set_fg(Color::Rgb(220, 155, 112));
            }
        }
        let mut painter = painter(&mut buffer, &geometry, text, pair);
        paint(
            &mut painter,
            &geometry,
            gutter,
            pair,
            &mut RouteCache::default(),
        );
        frames.push(
            painter
                .buffer
                .content
                .iter()
                .map(|cell| {
                    (
                        cell.symbol().to_owned(),
                        rgb(cell.fg, colors.fg_text),
                        rgb(cell.bg, colors.bg_editor),
                        cell.modifier.contains(Modifier::BOLD),
                        cell.modifier.contains(Modifier::ITALIC),
                        cell.modifier
                            .contains(Modifier::UNDERLINED)
                            .then(|| rgb(cell.underline_color, cell.fg)),
                    )
                })
                .collect::<Vec<_>>(),
        );
        names.push(name);
    }
    let data = serde_json::json!({ "name": "Whitespace scope bubbles", "width": area.right() + 1, "height": area.bottom() + 1, "frame_names": names, "frames": frames });
    std::fs::create_dir_all("target").unwrap();
    std::fs::write(
        "target/scope-bubble-preview.json",
        serde_json::to_vec(&data).unwrap(),
    )
    .unwrap();
}
