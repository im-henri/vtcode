//! Scope geometry must come from the comparison that is finally painted.
use super::*;

use crate::{
    animation_config::AnimationEffect,
    animation_structural_scope::StructuralScopeRequest,
    app::{ActivePane, DiffMode, Tab, TabKind},
    editor::Editor,
};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, style::Modifier};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn comparison(base: &str, target: &str, mode: DiffMode, compact: bool) -> App {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text(target);
    editor.file_path = Some("scope-comparison.rs".into());
    editor.language_override = Some("Rust".into());
    editor.read_only = true;
    editor.word_wrap = false;
    let index = StructuralScopeRequest::from_editor(&editor)
        .unwrap()
        .build(None, || false)
        .unwrap();
    let mut tab = Tab::new_file(editor);
    tab.kind = TabKind::Diff(91);
    app.tabs = vec![tab];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.diff_tabs.owner = Some(91);
    app.diff_mode = mode;
    app.diff_target_path = app.editor().unwrap().file_path.clone();
    app.compare_base_content = Some(base.into());
    app.set_compare_diff_lines(crate::app::compute_file_diff(base, target));
    if compact {
        app.diff_render_cache = Arc::new(app.diff_render_cache.with_context(2));
    }
    app.cached_structural_scope_index = Some(Arc::new(index));
    app.show_minimap = false;
    app.show_breadcrumbs = false;
    app.show_indent_guides = false;
    app.sticky_scroll = false;
    app.rulers.clear();
    app.show_color_preview = false;
    app.bracket_pair_colorization = false;
    app.cursor_visible = true;
    app.cursor_blinking = false;
    app.animation_settings
        .set(AnimationEffect::ScopeConnector, true);
    app.colors.bg_cursor = Color::Rgb(251, 17, 139);
    app.colors.bg_selection = Color::Rgb(11, 217, 141);
    app
}

fn view_name(mode: DiffMode) -> &'static str {
    match mode {
        DiffMode::SideBySide => "SideBySide",
        DiffMode::Inline => "Inline",
        DiffMode::Off => "Off",
    }
}

fn render(app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let mut painted = None;
    terminal
        .draw(|frame| {
            let area = frame.area();
            app.editor_area = Some(area);
            match app.diff_mode {
                DiffMode::SideBySide => draw_side_by_side_diff(frame, app, area),
                DiffMode::Inline => draw_inline_diff(frame, app, area),
                DiffMode::Off => unreachable!(),
            }
            painted = Some(frame.buffer_mut().clone());
        })
        .unwrap();
    painted.unwrap()
}

fn settled(app: &mut App, width: u16, height: u16) -> Buffer {
    render(app, width, height);
    app.animations.advance_frame(
        &app.animation_settings,
        Instant::now() + Duration::from_secs(2),
    );
    render(app, width, height)
}

fn without_scope_ink(app: &mut App, width: u16, height: u16) -> Buffer {
    // Keep the enabled effect's layout while suppressing its painted ink.
    let intensity = app.animation_settings.intensity;
    app.animation_settings.intensity = 0;
    let frame = render(app, width, height);
    app.animation_settings.intensity = intensity;
    frame
}

fn area(app: &App, original: bool) -> Rect {
    if app.diff_mode == DiffMode::SideBySide && original {
        app.diff_left_code_area.unwrap()
    } else {
        let mut area = app.code_area.unwrap();
        if app.diff_mode == DiffMode::SideBySide {
            area.x += 2;
            area.width = area.width.saturating_sub(2);
        }
        area
    }
}

fn display_row(app: &App, line: usize, original: bool) -> usize {
    let cache = &app.diff_render_cache;
    if app.diff_mode == DiffMode::SideBySide {
        cache
            .rows
            .iter()
            .position(|row| {
                row.hidden_lines == 0
                    && if original {
                        row.left_line == Some(line)
                    } else {
                        row.right_line == Some(line)
                    }
            })
            .unwrap()
    } else {
        cache
            .inline_rows
            .iter()
            .position(|entry| {
                let row = &cache.rows[entry.aligned_row];
                row.hidden_lines == 0
                    && if original {
                        entry.kind != '+' && row.left_line == Some(line)
                    } else {
                        entry.kind != '-' && row.right_line == Some(line)
                    }
            })
            .unwrap()
    }
}

fn first_display_row(app: &App) -> usize {
    if app.diff_mode == DiffMode::SideBySide {
        app.aligned_diff_scroll_row(app.editor().unwrap().scroll_offset)
    } else {
        app.diff_inline_scroll.row
    }
}

fn origin(app: &App, line: usize, original: bool) -> (u16, u16) {
    let area = area(app, original);
    let row = display_row(app, line, original);
    let first = first_display_row(app);
    assert!(row >= first && row - first < area.height as usize);
    (area.x, area.y + (row - first) as u16)
}

fn scope_glyph(symbol: &str) -> bool {
    matches!(
        symbol,
        "│" | "─"
            | "╭"
            | "╰"
            | "╮"
            | "╯"
            | "╷"
            | "╵"
            | "╶"
            | "╴"
            | "├"
            | "┤"
            | "┬"
            | "┴"
            | "┼"
    )
}

fn rail(app: &App, frame: &Buffer, original: bool) -> Vec<(u16, u16, String, Color)> {
    let area = area(app, original);
    (area.y..area.bottom())
        .flat_map(|y| (area.x.saturating_sub(1)..area.right()).map(move |x| (x, y)))
        .filter_map(|(x, y)| {
            if app.diff_mode == DiffMode::SideBySide && x >= area.x {
                let row = first_display_row(app) + usize::from(y - area.y);
                if app.diff_render_cache.rows.get(row).is_some_and(|row| {
                    row.hidden_lines == 0
                        && if original {
                            row.left_line.is_none()
                        } else {
                            row.right_line.is_none()
                        }
                }) {
                    // The hatch borders are diff content, while a connector
                    // can still continue through the blank prefix lane.
                    return None;
                }
            }
            let cell = &frame[(x, y)];
            scope_glyph(cell.symbol()).then(|| (x, y, cell.symbol().to_owned(), cell.fg))
        })
        .collect()
}

fn assert_scope_surrounds_endpoint(
    plain: &Buffer,
    frame: &Buffer,
    route: &[(u16, u16, String, Color)],
    code: Rect,
    endpoint: (u16, u16),
    context: &str,
) {
    let (brace, row) = endpoint;
    assert_eq!(
        frame[endpoint].symbol(),
        plain[endpoint].symbol(),
        "{context}"
    );
    for left_side in [true, false] {
        assert!(
            route.iter().any(|(x, y, _, _)| {
                *y == row
                    && (if left_side { *x < brace } else { *x > brace })
                    && plain[(*x, *y)].symbol().trim().is_empty()
            }),
            "scope has no {} boundary beside {endpoint:?}: {context}",
            if left_side { "left" } else { "right" }
        );
    }
    for x in code.x.saturating_sub(1)..code.right() {
        assert!(
            !frame[(x, row)].modifier.contains(Modifier::UNDERLINED)
                || plain[(x, row)].modifier.contains(Modifier::UNDERLINED),
            "scope adds an underline: {context}"
        );
    }
}

fn preserve_source_symbols(plain: &Buffer, decorated: &Buffer) {
    assert_eq!(plain.area, decorated.area);
    for (position, cell) in plain.content.iter().enumerate() {
        if !cell.symbol().trim().is_empty() && !scope_glyph(cell.symbol()) {
            assert_eq!(
                decorated.content[position].symbol(),
                cell.symbol(),
                "scope replaced source at buffer cell {position}"
            );
        }
        assert!(
            !decorated.content[position]
                .modifier
                .contains(Modifier::UNDERLINED)
                || cell.modifier.contains(Modifier::UNDERLINED),
            "scope added an underline at buffer cell {position}"
        );
    }
}

fn click(app: &mut App, point: (u16, u16)) {
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::mouse::handle_mouse(
            app,
            MouseEvent {
                kind,
                column: point.0,
                row: point.1,
                modifiers: KeyModifiers::NONE,
            },
        );
    }
}

fn prime_original(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        crate::diff_scope_source::refresh(app);
        crate::diff_scope_source::poll(app);
        let editor = app.editor().unwrap();
        if app
            .diff_scope_source
            .editor_and_index(
                app.compare_base_content.as_ref().unwrap(),
                editor.syntax_file_path(),
                editor.syntax_language_token(),
            )
            .is_some()
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "original source scope worker did not finish"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn set_first_row(app: &mut App, first: usize) {
    let mut scroll = std::mem::take(&mut app.diff_inline_scroll);
    scroll.row = first.min(scroll.max_row());
    scroll.sync_editor(app.editor_mut().unwrap());
    app.diff_inline_scroll = scroll;
}

#[test]
fn scope_fill_leaves_the_other_diff_revision_unchanged() {
    let base = "fn example() {\n    old_call();\n    shared_call();\n}\n";
    let target = "fn example() {\n    new_call();\n    shared_call();\n}\n";
    for mode in [DiffMode::Inline, DiffMode::SideBySide] {
        let mut app = comparison(base, target, mode, false);
        app.editor_mut().unwrap().cursor_line = 2;
        app.editor_mut().unwrap().cursor_col = 4;
        app.animation_settings.scope_fill = true;
        let filled = settled(&mut app, 100, 16);
        app.animation_settings.scope_fill = false;
        let plain = render(&mut app, 100, 16);
        let original = origin(&app, 1, true);
        for col in 0.."    old_call();".len() {
            let point = (original.0 + col as u16, original.1);
            assert_eq!(
                filled[point],
                plain[point],
                "filled the other revision: {}",
                view_name(mode)
            );
        }
        let target = origin(&app, 2, false);
        let point = (target.0 + 8, target.1);
        assert_eq!(filled[point].symbol(), plain[point].symbol());
        assert_ne!(
            filled[point].bg,
            plain[point].bg,
            "active scope was not filled: {}",
            view_name(mode)
        );
    }
}

#[test]
fn inline_scopes_render_in_both_diff_revisions_and_layouts() {
    let base = "fn run() {\n  if ok { old(); } next();\n}\n";
    let target = base.replace("old()", "new()");
    let line = base.lines().nth(1).unwrap();
    let open = line.find('{').unwrap() as u16;
    let close = line.find('}').unwrap() as u16;
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        for original in [false, true] {
            let mut app = comparison(base, &target, mode, false);
            render(&mut app, 120, 18);
            let (x, y) = origin(&app, 1, original);
            click(&mut app, (x + 11, y));
            if original {
                prime_original(&mut app);
            }
            let plain = without_scope_ink(&mut app, 120, 18);
            let frame = settled(&mut app, 120, 18);
            let (x, y) = origin(&app, 1, original);
            let route = rail(&app, &frame, original);
            assert!(!route.is_empty(), "{} original={original}", view_name(mode));
            assert!(
                route
                    .iter()
                    .all(|(col, row, _, _)| *row == y
                        && (x + open - 1..=x + close + 1).contains(col)),
                "{} original={original}, origin={x}:{y}, route={route:?}",
                view_name(mode)
            );
            for col in [open, close] {
                assert_ne!(
                    frame[(x + col, y)].bg,
                    plain[(x + col, y)].bg,
                    "the inline braces should be emphasized"
                );
            }
            preserve_source_symbols(&plain, &frame);
        }
    }
}

#[test]
fn diff_scope_survives_final_paint_and_uses_added_deleted_display_rows() {
    let prefix = "// unchanged prefix\n".repeat(12);
    let suffix = "// unchanged suffix\n".repeat(12);
    let base = format!(
        "{prefix}fn run() {{\n    if ready {{\n        obsolete_one();\n        obsolete_two();\n        work_old();\n    }}\n}}\n{suffix}"
    );
    let target = format!(
        "{prefix}fn run() {{\n    if ready {{\n        added_one();\n        work_new();\n    }}\n}}\n{suffix}"
    );
    let opening = 13;
    let closing = 16;
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        for compact in [false, true] {
            let mut expected_route = None;
            for cursor in [(opening + 1, 8), (closing, 4)] {
                let view = view_name(mode);
                let mut app = comparison(&base, &target, mode, compact);
                app.editor_mut().unwrap().set_cursor(cursor.0, cursor.1);
                assert_eq!(
                    app.cached_structural_scope_index
                        .as_ref()
                        .unwrap()
                        .enclosing_scope(app.editor().unwrap()),
                    Some(((opening, 13), (closing, 4)))
                );
                let plain = without_scope_ink(&mut app, 140, 50);
                let frame = settled(&mut app, 140, 50);
                let route = rail(&app, &frame, false);
                assert!(
                    !route.is_empty(),
                    "missing scope after final diff paint: {view}, compact={compact}, cursor={cursor:?}"
                );
                let upper = origin(&app, opening, false).1;
                let lower = origin(&app, closing, false).1;
                let code = area(&app, false);
                for endpoint in [(code.x + 13, upper), (code.x + 4, lower)] {
                    assert_scope_surrounds_endpoint(
                        &plain,
                        &frame,
                        &route,
                        code,
                        endpoint,
                        &format!("{view}, compact={compact}, cursor={cursor:?}"),
                    );
                }
                assert!(route.iter().all(|(_, y, _, _)| (upper..=lower).contains(y)));
                assert!(
                    display_row(&app, closing, false) - display_row(&app, opening, false)
                        > closing - opening
                );
                let shape: Vec<_> = route
                    .iter()
                    .map(|(x, y, glyph, _)| (*x, *y, glyph.clone()))
                    .collect();
                if let Some(expected) = &expected_route {
                    assert_eq!(&shape, expected);
                } else {
                    expected_route = Some(shape);
                }
                preserve_source_symbols(&plain, &frame);
                assert_eq!(app.editor().unwrap().rope.to_string(), target);
                assert!(app.editor().unwrap().read_only && !app.editor().unwrap().modified);
            }
        }
    }
}

#[test]
fn screenshot_row_for_line_else_scope_uses_actual_source_body_and_closing_brace() {
    let source = include_str!("inline_diff.rs");
    let lines: Vec<_> = source.lines().collect();
    let first = lines
        .iter()
        .position(|line| line.contains("fn row_for_line(&self,"))
        .unwrap();
    let mut depth = 0isize;
    let last = (first..lines.len())
        .find(|&line| {
            depth += lines[line].chars().filter(|ch| *ch == '{').count() as isize;
            depth -= lines[line].chars().filter(|ch| *ch == '}').count() as isize;
            depth == 0
        })
        .unwrap();
    let function = format!("{}\n", lines[first..=last].join("\n"));
    let prefix = "// surrounding context\n".repeat(12);
    let target = format!("{prefix}{function}// after the function\n");
    let base = target.replace(
        "            cache.inline_row_for_target_line(line)",
        "            previous_probe();\n            cache.legacy_row_for_line(line)",
    );
    let body = target
        .lines()
        .position(|line| line.trim() == "cache.inline_row_for_target_line(line)")
        .unwrap();
    let closing = body + 1;
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        for compact in [false, true] {
            let mut expected = None;
            for cursor in [(body, 12), (closing, 8)] {
                let view = view_name(mode);
                let mut app = comparison(&base, &target, mode, compact);
                app.editor_mut().unwrap().set_cursor(cursor.0, cursor.1);
                let pair = app
                    .cached_structural_scope_index
                    .as_ref()
                    .unwrap()
                    .enclosing_scope(app.editor().unwrap())
                    .unwrap();
                assert_eq!(pair.1, (closing, 8));
                assert!(target.lines().nth(pair.0.0).unwrap().contains("} else {"));
                let frame = settled(&mut app, 150, 40);
                let route = rail(&app, &frame, false);
                assert!(
                    !route.is_empty(),
                    "row_for_line else connector missing: {view}, compact={compact}, cursor={cursor:?}"
                );
                let upper = origin(&app, pair.0.0, false).1;
                let lower = origin(&app, pair.1.0, false).1;
                let plain = without_scope_ink(&mut app, 150, 40);
                // The contour surrounds the complete source silhouette,
                // including dense delimiter rows.
                for (row, delimiter) in [(upper, "{"), (lower, "}")] {
                    let code = area(&app, false);
                    let brace = (code.x..code.right())
                        .find(|&x| frame[(x, row)].symbol() == delimiter)
                        .unwrap();
                    assert_scope_surrounds_endpoint(
                        &plain,
                        &frame,
                        &route,
                        code,
                        (brace, row),
                        &format!("exact {delimiter}: {view}, compact={compact}"),
                    );
                }
                preserve_source_symbols(&plain, &frame);
                let shape: Vec<_> = route
                    .iter()
                    .map(|(x, y, glyph, _)| (*x, *y, glyph.clone()))
                    .collect();
                if let Some(expected) = &expected {
                    assert_eq!(&shape, expected);
                } else {
                    expected = Some(shape);
                }
            }
        }
    }
}

#[test]
fn original_and_deleted_scope_focus_queries_the_original_source() {
    let base = "fn run() {\n    if old {\n        old_work();\n    }\n    common();\n}\n";
    let target = "fn run() {\n    new_work();\n    common();\n}\n";
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        for compact in [false, true] {
            let view = view_name(mode);
            let mut app = comparison(base, target, mode, compact);
            render(&mut app, 140, 24);
            let body = origin(&app, 2, true);
            click(&mut app, (body.0 + 8, body.1));
            assert_eq!(app.diff_scope_cursor(), Some((true, (2, 8))));
            prime_original(&mut app);
            let plain = without_scope_ink(&mut app, 140, 24);
            let frame = settled(&mut app, 140, 24);
            let editor = app.editor().unwrap();
            let (original, index) = app
                .diff_scope_source
                .editor_and_index(
                    app.compare_base_content.as_ref().unwrap(),
                    editor.syntax_file_path(),
                    editor.syntax_language_token(),
                )
                .unwrap();
            assert_eq!(
                index.unwrap().enclosing_scope(original),
                Some(((1, 11), (3, 4)))
            );
            let route = rail(&app, &frame, true);
            assert!(
                !route.is_empty(),
                "original scope missing: {view}, compact={compact}"
            );
            let upper = origin(&app, 1, true).1;
            let lower = origin(&app, 3, true).1;
            assert!(route.iter().all(|(_, y, _, _)| (upper..=lower).contains(y)));
            if mode == DiffMode::SideBySide {
                // Alignment hatches contain literal box borders too. Compare
                // their cells rather than mistaking them for target scope ink.
                let target_area = area(&app, false);
                for y in target_area.y..target_area.bottom() {
                    for x in target_area.x.saturating_sub(1)..target_area.right() {
                        assert_eq!(
                            frame[(x, y)],
                            plain[(x, y)],
                            "original focus painted target cell {x},{y}: compact={compact}"
                        );
                    }
                }
            }
            assert_eq!(app.editor().unwrap().rope.to_string(), target);
            assert_eq!(app.compare_base_content.as_deref(), Some(base));
            assert!(app.editor().unwrap().read_only && !app.editor().unwrap().modified);
        }
    }
}

#[test]
fn diff_scope_clipping_and_horizontal_pan_keep_both_sides_around_visible_code() {
    let old_body = "        obsolete();\n".repeat(20);
    let new_body = "        work();\n".repeat(20);
    let condition = "condition_".repeat(24);
    let base = format!(
        "fn outer() {{\n    if old_{condition} && other_old_condition {{\n{old_body}    }}\n}}\n"
    );
    let target = format!(
        "fn outer() {{\n    if ready_{condition} && another_condition {{\n{new_body}    }}\n}}\n"
    );
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        for compact in [false, true] {
            for pan in [0, 2] {
                let view = view_name(mode);
                let mut app = comparison(&base, &target, mode, compact);
                // Guide ink composites against its surface. Use equal diff
                // surfaces to isolate source-accent stability from that blend.
                app.colors.bg_diff_added = app.colors.bg_editor;
                app.colors.bg_diff_removed = app.colors.bg_editor;
                app.colors.bg_line_highlight = app.colors.bg_editor;
                app.editor_mut().unwrap().set_cursor(10, 8);
                app.editor_mut().unwrap().horizontal_scroll = pan;
                let full = settled(&mut app, 90, 80);
                let text = area(&app, false);
                let full_rail = rail(&app, &full, false);
                let full_first = first_display_row(&app);
                let full_top = text.y;
                let full_left = text.x;
                let mut columns: Vec<_> = full_rail
                    .iter()
                    .map(|(x, _, _, _)| i32::from(*x) - i32::from(text.x))
                    .collect();
                columns.sort_unstable();
                columns.dedup();
                assert_eq!(columns.first(), Some(&(3 - pan as i32)));
                assert!(
                    columns.last() > columns.first(),
                    "the full scope needs a right side too"
                );
                let color = full_rail
                    .iter()
                    .find(|(_, _, glyph, _)| glyph == "│")
                    .unwrap()
                    .3;
                let opening = display_row(&app, 1, false);
                let closing = display_row(&app, 22, false);
                for first in [
                    0,
                    opening + 1,
                    (opening + closing) / 2,
                    closing.saturating_sub(3),
                ] {
                    render(&mut app, 90, 8);
                    set_first_row(&mut app, first);
                    let frame = settled(&mut app, 90, 8);
                    let text = area(&app, false);
                    let rendered_first = first_display_row(&app);
                    let route = rail(&app, &frame, false);
                    let mut clipped_columns: Vec<_> = route
                        .iter()
                        .map(|(x, _, _, _)| i32::from(*x) - i32::from(text.x))
                        .collect();
                    clipped_columns.sort_unstable();
                    clipped_columns.dedup();
                    let expected: Vec<_> = full_rail
                        .iter()
                        .filter_map(|(x, y, glyph, _)| {
                            let display = full_first + usize::from(*y - full_top);
                            (display >= rendered_first
                                && display - rendered_first < usize::from(text.height))
                            .then(|| {
                                (
                                    i32::from(*x) - i32::from(full_left),
                                    display - rendered_first,
                                    glyph.as_str(),
                                )
                            })
                        })
                        .collect();
                    let expected_left = expected.iter().map(|(x, _, _)| *x).min();
                    assert_eq!(
                        clipped_columns.first(),
                        expected_left.as_ref(),
                        "scrolling must clip the full source contour: {view}, compact={compact}, pan={pan}, requested={first}, rendered={rendered_first}"
                    );
                    let mut actual: Vec<_> = route
                        .iter()
                        .map(|(x, y, glyph, _)| {
                            (
                                i32::from(*x) - i32::from(text.x),
                                usize::from(*y - text.y),
                                glyph.as_str(),
                            )
                        })
                        .collect();
                    let mut expected = expected;
                    actual.sort_unstable();
                    expected.sort_unstable();
                    assert_eq!(
                        actual, expected,
                        "clipped strokes changed shape: {view}, compact={compact}, pan={pan}, requested={first}, rendered={rendered_first}"
                    );
                    assert!(
                        clipped_columns.last() > clipped_columns.first(),
                        "clipped scope needs two sides: {view}, first={first}"
                    );
                    let colors: Vec<_> = route
                        .iter()
                        .filter(|(_, _, glyph, _)| glyph == "│")
                        .map(|(_, _, _, fg)| *fg)
                        .collect();
                    assert!(
                        colors.iter().all(|fg| *fg == color),
                        "clipped accent changed on a uniform surface: {view}, compact={compact}, pan={pan}, first={first}, expected={color:?}, actual={colors:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn diff_scope_final_pass_preserves_native_caret_selection_and_text() {
    let base = "fn run() {\n    if ready {\n        work_old();\n    }\n}\n";
    let target = "fn run() {\n    if ready {\n        work_new();\n    }\n}\n";
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        for compact in [false, true] {
            let mut app = comparison(base, target, mode, compact);
            let editor = app.editor_mut().unwrap();
            editor.set_cursor(2, 12);
            editor.selection_anchor = Some((2, 8));
            let selection = editor.selection_range();
            let source = editor.rope.clone();
            let version = editor.content_version;
            let plain = without_scope_ink(&mut app, 140, 20);
            let frame = settled(&mut app, 140, 20);
            assert!(!rail(&app, &frame, false).is_empty());
            let first = origin(&app, 2, false);
            for col in 8..12 {
                assert_eq!(
                    frame[(first.0 + col, first.1)],
                    plain[(first.0 + col, first.1)]
                );
            }
            assert_eq!(
                frame[(first.0 + 12, first.1)],
                plain[(first.0 + 12, first.1)]
            );
            preserve_source_symbols(&plain, &frame);
            let editor = app.editor().unwrap();
            assert!(editor.rope.is_instance(&source));
            assert_eq!(editor.selection_range(), selection);
            assert_eq!(editor.content_version, version);
            assert_eq!((editor.cursor_line, editor.cursor_col), (2, 12));
            assert!(editor.read_only && !editor.modified);
        }
    }
}

#[test]
fn hidden_scope_endpoints_are_masked_by_context_markers_without_leaking_into_blank_tail() {
    let body: String = (0..20)
        .map(|line| format!("        work_{line:02}();\n"))
        .collect();
    let base = format!("fn run() {{\n    if ready {{\n{body}    }}\n}}\n");
    let target = base.replace("work_10", "updated_10");
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        let mut app = comparison(&base, &target, mode, true);
        app.editor_mut().unwrap().set_cursor(12, 8);
        let plain = without_scope_ink(&mut app, 140, 24);
        let frame = settled(&mut app, 140, 24);
        let text = area(&app, false);
        let markers: Vec<_> = if mode == DiffMode::SideBySide {
            app.diff_render_cache
                .rows
                .iter()
                .enumerate()
                .filter_map(|(row, entry)| (entry.hidden_lines > 0).then_some(row))
                .collect()
        } else {
            app.diff_render_cache
                .inline_rows
                .iter()
                .enumerate()
                .filter_map(|(row, entry)| {
                    (app.diff_render_cache.rows[entry.aligned_row].hidden_lines > 0).then_some(row)
                })
                .collect()
        };
        assert_eq!(markers.len(), 2);
        let first = first_display_row(&app);
        let upper = text.y + (markers[0] - first) as u16;
        let lower = text.y + (markers[1] - first) as u16;
        assert!(
            lower + 3 < text.bottom(),
            "fixture must leave a blank suffix"
        );
        let route = rail(&app, &frame, false);
        assert!(!route.is_empty());
        assert!(route.iter().all(|(_, y, _, _)| (upper..=lower).contains(y)));
        for y in [upper, lower] {
            for x in text.x.saturating_sub(1)..text.right() {
                assert_eq!(
                    frame[(x, y)],
                    plain[(x, y)],
                    "context markers mask the fixed contour instead of moving it into the gutter"
                );
            }
        }
        let visible_source_rows: Vec<_> = if mode == DiffMode::SideBySide {
            app.diff_render_cache
                .rows
                .iter()
                .enumerate()
                .filter_map(|(row, entry)| {
                    (row >= first
                        && row - first < usize::from(text.height)
                        && entry.hidden_lines == 0
                        && entry.right_line.is_some())
                    .then(|| text.y + (row - first) as u16)
                })
                .collect()
        } else {
            app.diff_render_cache
                .inline_rows
                .iter()
                .enumerate()
                .filter_map(|(row, entry)| {
                    let source = &app.diff_render_cache.rows[entry.aligned_row];
                    (row >= first
                        && row - first < usize::from(text.height)
                        && source.hidden_lines == 0
                        && entry.kind != '-'
                        && source.right_line.is_some())
                    .then(|| text.y + (row - first) as u16)
                })
                .collect()
        };
        let visible_source_rows: Vec<_> = visible_source_rows
            .into_iter()
            .filter(|y| *y > upper && *y < lower)
            .collect();
        assert!(!visible_source_rows.is_empty());
        for y in [
            visible_source_rows.first().unwrap(),
            visible_source_rows.last().unwrap(),
        ] {
            assert!(
                route.iter().any(|(_, row, _, _)| row == y),
                "the source contour must remain visible beside both ends of the displayed context"
            );
        }
        preserve_source_symbols(&plain, &frame);
    }
}

#[test]
fn inline_scope_treats_a_literal_box_glyph_in_the_other_revision_as_source() {
    let base = "fn run() {\n    {\n// │ literal source glyph\n    }\n}\n";
    let target = "fn run() {\n    {\n        work();\n    }\n}\n";
    for compact in [false, true] {
        let mut app = comparison(base, target, DiffMode::Inline, compact);
        app.editor_mut().unwrap().set_cursor(2, 8);
        let plain = without_scope_ink(&mut app, 140, 20);
        let literal = origin(&app, 2, true);
        let point = (literal.0 + 3, literal.1);
        assert_eq!(plain[point].symbol(), "│");
        let frame = settled(&mut app, 140, 20);
        assert_eq!(frame[point], plain[point]);
        // Both fixed source rails meet tokens in the other revision. Mask
        // those strokes; searching for another gap would change the contour
        // as diff rows enter or leave the viewport.
        let trace: Vec<_> = rail(&app, &frame, false)
            .into_iter()
            .filter(|(x, y, _, _)| *y == literal.1 && plain[(*x, *y)].symbol().trim().is_empty())
            .collect();
        assert!(
            trace.is_empty(),
            "an obstructed source rail must not acquire an invented detour"
        );
        let route = rail(&app, &frame, false);
        assert!(route.iter().any(|(_, y, _, _)| *y < literal.1));
        assert!(route.iter().any(|(_, y, _, _)| *y > literal.1));
        preserve_source_symbols(&plain, &frame);
    }
}

#[test]
fn source_selection_over_blank_indentation_masks_the_rail_without_moving_it() {
    let base = "fn run() {\n    {\n        old_work();\n    }\n}\n";
    let target = "fn run() {\n    {\n        new_work();\n    }\n}\n";
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        for original in [false, true] {
            let mut app = comparison(base, target, mode, false);
            render(&mut app, 140, 20);
            let body = origin(&app, 2, original);
            for (kind, x) in [
                (MouseEventKind::Down(MouseButton::Left), body.0),
                (MouseEventKind::Drag(MouseButton::Left), body.0 + 8),
                (MouseEventKind::Up(MouseButton::Left), body.0 + 8),
            ] {
                crate::mouse::handle_mouse(
                    &mut app,
                    MouseEvent {
                        kind,
                        column: x,
                        row: body.1,
                        modifiers: KeyModifiers::NONE,
                    },
                );
            }
            assert_eq!(app.diff_selected_text().as_deref(), Some("        "));
            if original {
                prime_original(&mut app);
            }
            let plain = without_scope_ink(&mut app, 140, 20);
            let frame = settled(&mut app, 140, 20);
            for x in body.0..=body.0 + 8 {
                assert_eq!(frame[(x, body.1)], plain[(x, body.1)]);
            }
            let text = area(&app, original);
            assert!(
                rail(&app, &frame, original)
                    .iter()
                    .any(|(x, y, _, _)| *x == text.x + 3 && *y != body.1)
            );
            assert_eq!(app.diff_selected_text().as_deref(), Some("        "));
            assert_eq!(app.editor().unwrap().rope.to_string(), target);
        }
    }
}

#[test]
fn editable_target_scope_waits_for_a_diff_cache_of_the_same_content_revision() {
    let base = "fn run() {\n    if ready {\n        old_work();\n    }\n}\n";
    let target = "fn run() {\n    if ready {\n        new_work();\n    }\n}\n";
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        let mut app = comparison(base, target, mode, false);
        app.tabs[0].kind = TabKind::File;
        app.diff_tabs.owner = None;
        app.editor_mut().unwrap().read_only = false;
        app.editor_mut().unwrap().set_cursor(2, 8);
        let frame = settled(&mut app, 140, 20);
        assert!(!rail(&app, &frame, false).is_empty());
        app.editor_mut().unwrap().insert_char('X');
        let editor = app.editor().unwrap();
        let updated = editor.rope.to_string();
        assert_ne!(
            app.compare_diff_target_version,
            Some(editor.content_version)
        );
        let index = StructuralScopeRequest::from_editor(editor)
            .unwrap()
            .build(None, || false)
            .unwrap();
        app.cached_structural_scope_index = Some(Arc::new(index));
        let stale = settled(&mut app, 140, 20);
        assert!(rail(&app, &stale, false).is_empty());
        app.set_compare_diff_lines(crate::app::compute_file_diff(base, &updated));
        let fresh = settled(&mut app, 140, 20);
        assert!(!rail(&app, &fresh, false).is_empty());
        assert_eq!(app.editor().unwrap().rope.to_string(), updated);
    }
}

#[test]
fn diff_scope_preserves_the_projected_blank_caret_when_its_source_character_is_a_tab() {
    let base = "fn run() {\n    {\n   \t     work_old();\n    }\n}\n";
    let target = "fn run() {\n    {\n   \t     work_new();\n    }\n}\n";
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        for compact in [false, true] {
            let mut app = comparison(base, target, mode, compact);
            app.editor_mut().unwrap().set_cursor(2, 3);
            let plain = without_scope_ink(&mut app, 140, 20);
            let body = origin(&app, 2, false);
            // Paragraph omits the tab; the source cursor on it belongs to the
            // following visible blank at the connector's canonical column.
            let caret = (body.0 + 3, body.1);
            assert_eq!(plain[caret].symbol(), " ");
            assert_eq!(plain[caret].bg, app.colors.bg_cursor);
            let frame = settled(&mut app, 140, 20);
            assert_eq!(frame[caret], plain[caret]);
            assert!(
                rail(&app, &frame, false)
                    .iter()
                    .any(|(x, y, _, _)| *x == body.0 + 3 && *y != body.1)
            );
            assert_eq!(
                (
                    app.editor().unwrap().cursor_line,
                    app.editor().unwrap().cursor_col
                ),
                (2, 3)
            );
            assert_eq!(app.editor().unwrap().rope.to_string(), target);
            assert!(app.editor().unwrap().read_only && !app.editor().unwrap().modified);
        }
    }
}
