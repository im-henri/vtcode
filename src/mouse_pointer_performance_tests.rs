use super::*;
use crate::app::Tab;
use crate::editor::Editor;
use ratatui::layout::Rect;
use std::hint::black_box;
use std::time::Instant;

fn app_with_editor(text: &str, wrapped: bool) -> App {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text(text);
    editor.word_wrap = wrapped;
    editor.viewport_height = 40;
    editor.viewport_width = 9;
    app.tabs.push(Tab::new_file(editor));
    app.active_tab = 0;
    app.code_area = Some(Rect::new(5, 2, 80, 40));
    app.active_pane = ActivePane::Editor;
    app
}

#[test]
fn configured_wrap_column_keeps_pointer_and_block_selection_on_the_drawn_rows() {
    let text = "abcdefghijklmnop\n".repeat(30);
    let mut actual = app_with_editor(&text, true);
    let mut expected = app_with_editor(&text, true);
    actual.editor_mut().unwrap().word_wrap_column = 4;
    expected.editor_mut().unwrap().viewport_width = 4;
    let area = actual.code_area.unwrap();
    for row in 0..9 {
        for col in 0..8 {
            let actual_position = code_area_to_buffer_pos(
                actual.editor_mut().unwrap(),
                area,
                area.x + col,
                area.y + row,
            );
            let expected_position = reference_hit(
                expected.editor_mut().unwrap(),
                area,
                area.x + col,
                area.y + row,
            );
            assert_eq!(actual_position, expected_position, "row={row}, col={col}");
            assert!(actual.editor().unwrap().wrap_layout_ready(4));
        }
    }
    let start = (0, 1, 1);
    actual.middle_drag_start = Some(start);
    for row in [2, 5, 8, 5, 2] {
        let placements = reference_block(
            expected.editor_mut().unwrap(),
            area,
            start,
            area.x + 3,
            area.y + row,
        );
        handle_block_drag(&mut actual, area.x + 3, area.y + row);
        assert_eq!(state(actual.editor().unwrap()), placements);
        assert!(actual.editor().unwrap().wrap_layout_ready(4));
    }
}

// The former viewport-vector lookup remains a behavioral/timing reference.
fn reference_hit(editor: &mut Editor, area: Rect, col: u16, row: u16) -> (usize, usize) {
    if editor.word_wrap {
        return editor.wrapped_position_for_screen(
            editor.wrap_scroll_offset,
            row.saturating_sub(area.y) as usize,
            col.saturating_sub(area.x) as usize,
            editor.viewport_width.max(1),
        );
    }
    let visible = editor.visible_lines(editor.scroll_offset, editor.viewport_height);
    let line = visible
        .get(row.saturating_sub(area.y) as usize)
        .copied()
        .unwrap_or_else(|| editor.line_count().saturating_sub(1));
    let col = editor.source_column_at_cell(line, editor.horizontal_scroll, editor.line_len(line),
        col.saturating_sub(area.x) as usize).0;
    (line, col)
}

fn state(editor: &Editor) -> Vec<CursorPlacement> {
    std::iter::once((
        editor.cursor_line,
        editor.cursor_col,
        editor.selection_anchor,
    ))
    .chain(
        editor
            .extra_cursors
            .iter()
            .enumerate()
            .map(|(i, &(line, col))| (line, col, editor.extra_cursor_anchors[i])),
    )
    .collect()
}

// Original block projection: materialize rows, then filter into placements.
fn reference_block(
    editor: &mut Editor,
    area: Rect,
    start: (usize, usize, usize),
    col: u16,
    row: u16,
) -> Vec<CursorPlacement> {
    let end = reference_hit(editor, area, col, row);
    let mut rows = Vec::new();
    if editor.word_wrap {
        let width = editor.viewport_width.max(1);
        let first = editor.wrapped_row_for_position(start.0, start.1, width);
        let last = editor.wrapped_row_for_position(end.0, end.1, width);
        let screen_col = col.saturating_sub(area.x) as usize;
        let min_col = start.2.min(screen_col);
        let max_col = start.2.max(screen_col);
        for row in editor.wrapped_rows(first.min(last), first.abs_diff(last) + 1, width) {
            let col = editor.source_column_at_cell(row.line_idx, row.start_col, row.end_col, min_col).0;
            let anchor = editor.source_column_at_cell(row.line_idx, row.start_col, row.end_col, max_col).0;
            rows.push((
                row.line_idx,
                col,
                (anchor > col).then_some((row.line_idx, anchor)),
            ));
        }
    } else {
        let end_col = editor
            .horizontal_scroll
            .saturating_add(col.saturating_sub(area.x) as usize);
        let min_col = start.2.min(end_col);
        let max_col = start.2.max(end_col);
        for line in start.0.min(end.0)..=start.0.max(end.0) {
            if line >= editor.line_count() {
                continue;
            }
            let len = editor.line_len(line);
            let origin = editor.horizontal_scroll;
            let col = editor.source_column_at_cell(line, origin, len, min_col.saturating_sub(origin)).0;
            let anchor = editor.source_column_at_cell(line, origin, len, max_col.saturating_sub(origin)).0;
            rows.push((line, col, (anchor > col).then_some((line, anchor))));
        }
    }
    let any_span = rows.iter().any(|row| row.2.is_some());
    rows.into_iter()
        .filter(|row| !any_span || row.2.is_some())
        .collect()
}

#[test]
fn direct_visible_row_matches_vector_with_folds_and_stale_ranges() {
    let text = "row\n".repeat(60);
    for folds in [
        vec![],
        vec![(2, 30), (35, 50)],
        vec![(2, 30), (4, 10), (8, 18)],
        vec![(2, 9), (5, 16), (25, 500)],
        vec![(80, 90)],
    ] {
        let mut editor = Editor::test_with_text(&text);
        editor.folded_ranges.extend(folds);
        for from in 0..=65 {
            let expected = editor.visible_lines(from, 75);
            for offset in 0..75 {
                assert_eq!(
                    editor.visible_line_at(from, offset),
                    expected.get(offset).copied(),
                    "from={from}, offset={offset}, folds={:?}",
                    editor.folded_ranges
                );
            }
        }
        assert_eq!(editor.visible_line_at(usize::MAX, usize::MAX), None);
    }
}

#[test]
fn pointer_lookup_matches_reference_at_viewport_edges_and_unicode() {
    for wrapped in [false, true] {
        let mut app = app_with_editor("\tα界b\r\n\nlong Ελληνικά value here\nshort\nlast", wrapped);
        let area = app.code_area.unwrap();
        let editor = app.editor_mut().unwrap();
        editor.folded_ranges.insert(0, 3);
        editor.horizontal_scroll = 3;
        for height in [0, 1, 4, 40] {
            editor.viewport_height = height;
            for scroll in [0, 1, 3, 99] {
                editor.scroll_offset = scroll;
                editor.wrap_scroll_offset = scroll;
                for row in [0, 2, 3, 5, 40, 500] {
                    for col in [0, 5, 7, 80] {
                        let expected = reference_hit(editor, area, col, row);
                        assert_eq!(code_area_to_buffer_pos(editor, area, col, row), expected);
                    }
                }
            }
        }
    }
}

#[test]
fn gutter_row_lookup_matches_original_viewport_projection() {
    for wrapped in [false, true] {
        for height in [0, 1, 7] {
            let mut app = app_with_editor("abcdefghijklm\n\n界αβγdelta\nhidden\nlast", wrapped);
            app.gutter_area = Some(Rect::new(1, 2, 4, 10));
            app.editor_mut().unwrap().viewport_height = height;
            app.editor_mut().unwrap().folded_ranges.insert(2, 4);
            for screen_row in 0..9 {
                let editor = app.editor_mut().unwrap();
                // Every hit starts from the same caret/viewport. Reuse the
                // immutable source and App instead of rebuilding their state.
                editor.cursor_line = 0;
                editor.cursor_col = 0;
                editor.selection_anchor = None;
                editor.scroll_offset = 0;
                editor.wrap_scroll_offset = 0;
                let expected = if wrapped {
                    editor
                        .wrapped_rows(0, height.max(1), 9)
                        .get(screen_row)
                        .map(|row| row.line_idx)
                } else {
                    editor.visible_lines(0, height).get(screen_row).copied()
                };
                let old = (
                    editor.cursor_line,
                    editor.cursor_col,
                    editor.selection_anchor,
                );
                let expected_selection = expected
                    .map(|line| {
                        let mut reference = Editor::test_with_text(&editor.rope.to_string());
                        reference.cursor_line = line;
                        reference.cursor_col = 0;
                        reference.select_line();
                        (
                            reference.cursor_line,
                            reference.cursor_col,
                            reference.selection_anchor,
                        )
                    })
                    .unwrap_or(old);
                crate::mouse_selection::handle_editor_area_left_click(
                    &mut app,
                    2,
                    2 + screen_row as u16,
                    KeyModifiers::NONE,
                );
                let editor = app.editor().unwrap();
                assert_eq!(
                    (
                        editor.cursor_line,
                        editor.cursor_col,
                        editor.selection_anchor
                    ),
                    expected_selection
                );
            }
        }
    }
}

#[test]
fn gutter_lookup_preserves_folded_fallback_while_wrap_worker_is_pending() {
    for screen_row in [0, 1, 2, 6] {
        let mut app = app_with_editor(&"abcdefghijklmnop\n".repeat(3_000), true);
        app.gutter_area = Some(Rect::new(1, 2, 4, 10));
        let editor = app.editor_mut().unwrap();
        editor.viewport_height = 7;
        editor.folded_ranges.insert(0, 100);
        let expected_line = editor.wrapped_rows(0, 7, 9)[screen_row].line_idx;
        assert!(!editor.wrap_layout_ready(9));
        crate::mouse_selection::handle_editor_area_left_click(
            &mut app,
            2,
            2 + screen_row as u16,
            KeyModifiers::NONE,
        );
        assert_eq!(
            app.editor().unwrap().selection_anchor,
            Some((expected_line, 0))
        );
    }
}

#[test]
fn block_drag_matches_reference_through_direction_width_and_scroll_changes() {
    let text = (0..40)
        .map(|i| match i % 4 {
            0 => "".to_owned(),
            1 => "x".to_owned(),
            2 => "\t界αβγ xyz abcdefghijklmnop".to_owned(),
            _ => "abcdef".to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    for wrapped in [false, true] {
        let mut app = app_with_editor(&text, wrapped);
        let area = app.code_area.unwrap();
        let start = (6, 2, 2);
        app.middle_drag_start = Some(start);
        app.editor_mut()
            .unwrap()
            .folded_ranges
            .extend([(1, 5), (15, 20)]);
        for (index, (col, row)) in [
            (12, 15),
            (12, 15),
            (5, 8),
            (7, 8),
            (6, 3),
            (9, 2),
            (9, 22),
            (7, 41),
            (7, 41),
            (10, 4),
        ]
        .into_iter()
        .enumerate()
        {
            if index == 7 {
                let editor = app.editor_mut().unwrap();
                editor.scroll_offset += 1;
                editor.wrap_scroll_offset += 1;
            }
            let expected = reference_block(app.editor_mut().unwrap(), area, start, col, row);
            handle_block_drag(&mut app, col, row);
            assert_eq!(
                state(app.editor().unwrap()),
                expected,
                "wrapped={wrapped}, sample={index}"
            );
        }
    }
}

#[test]
fn identical_block_endpoint_restores_exact_public_cursor_state() {
    let mut app = app_with_editor("abcdefgh\nabcdefgh\nabcdefgh\nabcdefgh", false);
    app.middle_drag_start = Some((0, 1, 1));
    handle_block_drag(&mut app, 10, 5);
    let expected = state(app.editor().unwrap());
    let cached_capacity = app
        .block_drag_cache
        .as_ref()
        .unwrap()
        .projection
        .cursors
        .capacity();
    {
        let editor = app.editor_mut().unwrap();
        editor.cursor_line = 2;
        editor.cursor_col = 7;
        editor.selection_anchor = None;
        // Equal length/outer endpoints: a shallow output guard misses this.
        editor.extra_cursors[1].1 = 7;
        editor.extra_cursor_anchors[1] = None;
        editor.desired_col = Some(7);
    }
    handle_block_drag(&mut app, 10, 5);
    assert_eq!(state(app.editor().unwrap()), expected);
    assert_eq!(app.editor().unwrap().desired_col, None);
    assert_eq!(
        app.block_drag_cache
            .as_ref()
            .unwrap()
            .projection
            .cursors
            .capacity(),
        cached_capacity
    );
}

#[test]
fn incremental_block_tail_handles_first_and_last_real_span_transitions() {
    let mut lines = vec![""; 24];
    lines[8] = "abcdefgh";
    lines[15] = "abcdefgh";
    let mut app = app_with_editor(&lines.join("\n"), false);
    let area = app.code_area.unwrap();
    let start = (0, 1, 1);
    app.middle_drag_start = Some(start);
    for last in [6, 7, 8, 9, 10, 9, 7, 6, 15, 16, 14, 7, 0] {
        let row = area.y + last;
        let expected = reference_block(app.editor_mut().unwrap(), area, start, 10, row);
        handle_block_drag(&mut app, 10, row);
        assert_eq!(state(app.editor().unwrap()), expected, "last={last}");
    }
}

#[test]
fn block_projection_revalidates_async_wrap_installation() {
    let mut app = app_with_editor(&"abcdefghijklmnop\n".repeat(3_000), true);
    let area = app.code_area.unwrap();
    let start = (0, 1, 1);
    app.middle_drag_start = Some(start);
    handle_block_drag(&mut app, 10, 5);
    assert!(!app.editor().unwrap().wrap_layout_ready(9));
    let editor = app.editor_mut().unwrap();
    let offsets =
        Editor::build_wrap_row_offsets(&editor.rope, 9, editor.tab_size, &[], || false).unwrap();
    assert!(editor.install_wrap_layout(9, offsets));
    let expected = reference_block(editor, area, start, 10, 5);
    handle_block_drag(&mut app, 10, 5);
    assert_eq!(state(app.editor().unwrap()), expected);
    app.editor_mut().unwrap().viewport_width = 5;
    let expected = reference_block(app.editor_mut().unwrap(), area, start, 10, 5);
    handle_block_drag(&mut app, 10, 5);
    assert_eq!(state(app.editor().unwrap()), expected);
}

#[test]
fn ordered_outside_selection_samples_preserve_each_autoscroll_step() {
    for wrapped in [false, true] {
        let text = "abcdefghijklm\n".repeat(100);
        let mut app = app_with_editor(&text, wrapped);
        let mut reference = app_with_editor(&text, wrapped);
        let area = app.code_area.unwrap();
        // This unit test starts with an established selection gesture.
        app.editor_selection_drag = Some(app.editor().unwrap().navigation_id);
        for editor in [app.editor_mut().unwrap(), reference.editor_mut().unwrap()] {
            editor.selection_anchor = Some((20, 1));
            editor.cursor_line = 20;
            editor.cursor_col = 1;
        }
        for (row, scroll) in [(42, 1), (42, 2), (42, 3), (0, 2), (0, 1), (42, 2), (3, 2)] {
            let editor = reference.editor_mut().unwrap();
            if row < area.y {
                editor.scroll_up(1);
            } else if row >= area.bottom() {
                editor.scroll_down(1);
            }
            let clamped = row.max(area.y).min(area.bottom() - 1);
            let (line, col) = reference_hit(editor, area, 7, clamped);
            // The drag handler owns scrolling. Keyboard margins must not
            // turn a single outside sample into a several-row jump.
            editor.cursor_line = line;
            editor.cursor_col = col;
            editor.desired_col = None;
            handle_left_drag(&mut app, 7, row, 7, row, KeyModifiers::NONE);
            let editor = app.editor().unwrap();
            let expected = reference.editor().unwrap();
            assert_eq!(state(editor), state(expected));
            assert_eq!(editor.scroll_offset, expected.scroll_offset);
            assert_eq!(editor.wrap_scroll_offset, expected.wrap_scroll_offset);
            assert_eq!(if wrapped { editor.wrap_scroll_offset } else { editor.scroll_offset }, scroll);
        }
    }
}

#[test]
fn block_drag_outside_viewport_scrolls_gradually_and_releases_capture() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    for wrapped in [false, true] {
        for (button, modifiers) in [
            (MouseButton::Middle, KeyModifiers::NONE),
            (MouseButton::Left, KeyModifiers::ALT | KeyModifiers::SHIFT),
        ] {
            let mut app = app_with_editor(&"abcdefgh\n".repeat(10_000), wrapped);
            let area = app.code_area.unwrap();
            let column = area.x + 2;
            let send = |app: &mut App, kind, row| crate::mouse::handle_mouse(app, MouseEvent {
                kind, column, row, modifiers,
            });
            send(&mut app, MouseEventKind::Down(button), area.bottom() - 3);
            assert_eq!(app.middle_drag_start, Some((37, 2, 2)));

            // Cross the bottom by just 2–3 cells, then simulate a terminal
            // reporting a far-away/repeated coordinate during pointer capture.
            for (row, scroll, last) in [
                (area.bottom() - 1, 0, 39),
                (area.bottom() + 2, 1, 40),
                (area.bottom() + 3, 2, 41),
                (u16::MAX, 3, 42),
                (u16::MAX, 4, 43),
                (area.bottom() - 5, 4, 39),
            ] {
                send(&mut app, MouseEventKind::Drag(button), row);
                let editor = app.editor().unwrap();
                assert_eq!(state(editor), (37..=last).map(|line| (line, 2, None)).collect::<Vec<_>>(),
                    "wrapped={wrapped}, button={button:?}, row={row}");
                assert_eq!(editor.scroll_offset, scroll);
                assert_eq!(editor.wrap_scroll_offset, if wrapped { scroll } else { 0 });
            }
            let before_release = state(app.editor().unwrap());
            send(&mut app, MouseEventKind::Up(button), u16::MAX);
            assert!(app.middle_drag_start.is_none());
            assert!(app.block_drag_cache.is_none());
            assert_eq!(state(app.editor().unwrap()), before_release);
        }
    }
}

#[test]
fn block_drag_clamps_top_and_horizontal_edges_and_ignores_empty_viewports() {
    for wrapped in [false, true] {
        let mut app = app_with_editor(&"abcdefgh\n".repeat(100), wrapped);
        let area = Rect::new(5, 2, 6, 10);
        app.code_area = Some(area);
        let editor = app.editor_mut().unwrap();
        editor.viewport_height = area.height as usize;
        editor.scroll_down(20);
        crate::mouse_selection::handle_middle_button_down(&mut app, area.x + 2, area.y + 2);
        handle_block_drag(&mut app, 0, 0);
        let editor = app.editor().unwrap();
        assert_eq!(editor.scroll_offset, 19);
        assert_eq!(state(editor), (19..=22).map(|line| (line, 0, Some((line, 2)))).collect::<Vec<_>>());

        handle_block_drag(&mut app, u16::MAX, area.y + 2);
        assert_eq!(state(app.editor().unwrap()), (21..=22).map(|line| (line, 2, Some((line, 5)))).collect::<Vec<_>>());
        let expected = state(app.editor().unwrap());
        for empty in [Rect::new(5, 2, 0, 10), Rect::new(5, 2, 6, 0)] {
            app.code_area = Some(empty);
            handle_block_drag(&mut app, u16::MAX, u16::MAX);
            assert_eq!(state(app.editor().unwrap()), expected);
            assert_eq!(app.editor().unwrap().scroll_offset, 19);
        }
    }
}

#[test]
fn wrapped_block_drag_outside_uses_the_scrolled_visual_edge() {
    for ready in [false, true] {
        let mut app = app_with_editor(&"abcdefghijklmnopqrstuvwx\n".repeat(3_000), true);
        let area = Rect::new(5, 2, 9, 10);
        app.code_area = Some(area);
        let editor = app.editor_mut().unwrap();
        editor.viewport_height = 10;
        if ready {
            let rows = Editor::build_wrap_row_offsets(&editor.rope, 9, editor.tab_size, &[], || false).unwrap();
            assert!(editor.install_wrap_layout(9, rows));
        }
        crate::mouse_selection::handle_middle_button_down(&mut app, area.x + 2, area.y + 7);
        let start = app.middle_drag_start.unwrap();
        for scroll in 1..=3 {
            handle_block_drag(&mut app, area.x + 2, area.bottom() + 2);
            assert_eq!(app.editor().unwrap().wrap_scroll_offset, scroll);
            let expected = reference_block(app.editor_mut().unwrap(), area, start, area.x + 2, area.bottom() - 1);
            assert_eq!(state(app.editor().unwrap()), expected, "ready={ready}, scroll={scroll}");
            assert!(expected.iter().all(|&(line, _, _)| line < 15));
        }
    }
}

#[test]
fn block_projection_revalidates_same_metadata_rope_replacement_and_folds() {
    let mut app = app_with_editor("abcdefgh\nabcdefgh\nabcdefgh", false);
    let area = app.code_area.unwrap();
    let start = (0, 1, 1);
    app.middle_drag_start = Some(start);
    handle_block_drag(&mut app, 10, 4);
    let old_version = app.editor().unwrap().content_version;
    // Equal byte length and unchanged public revision: root identity differs.
    app.editor_mut().unwrap().rope = ropey::Rope::from_str("a\nabcdefghijklmnop\nabcdefgh");
    assert_eq!(app.editor().unwrap().content_version, old_version);
    let expected = reference_block(app.editor_mut().unwrap(), area, start, 10, 4);
    handle_block_drag(&mut app, 10, 4);
    assert_eq!(state(app.editor().unwrap()), expected);
    app.editor_mut().unwrap().folded_ranges.insert(0, 2);
    let expected = reference_block(app.editor_mut().unwrap(), area, start, 10, 4);
    handle_block_drag(&mut app, 10, 4);
    assert_eq!(state(app.editor().unwrap()), expected);
    handle_left_up(&mut app, 10, 4, KeyModifiers::NONE);
    assert!(app.block_drag_cache.is_none());
}

#[test]
fn oversized_block_does_not_retain_duplicate_cursor_storage() {
    let mut app = app_with_editor(&"abcdefgh\n".repeat(MAX_CACHED_BLOCK_ROWS + 20), false);
    app.middle_drag_start = Some((0, 1, 1));
    // Reach a large selection by scrolling to its endpoint, not by sending
    // an out-of-viewport coordinate that used to incorrectly map to EOF.
    let area = app.code_area.unwrap();
    let editor = app.editor_mut().unwrap();
    editor.scroll_offset = editor.line_count().saturating_sub(editor.viewport_height);
    handle_block_drag(&mut app, 10, area.bottom() - 1);
    assert!(app.editor().unwrap().extra_cursors.len() >= MAX_CACHED_BLOCK_ROWS);
    assert!(app.block_drag_cache.is_none());
}

#[test]
fn empty_block_prefix_then_real_span_drops_oversized_scratch_capacity() {
    // One primary plus >MAX secondary fallback carets already grows both
    // scratch vectors beyond their retention limit before the real span.
    let mut text = "\n".repeat(MAX_CACHED_BLOCK_ROWS + 2);
    text.push_str("abcdefgh");
    for cached_prefix in [false, true] {
        let mut app = app_with_editor(&text, false);
        let area = app.code_area.unwrap();
        let start = (0, 1, 1);
        app.middle_drag_start = Some(start);
        if cached_prefix {
            handle_block_drag(&mut app, 10, 8);
            assert!(app.block_drag_cache.is_some());
            assert!(!app.editor().unwrap().extra_cursors.is_empty());
        }
        // Scroll until the only nonempty final line is visible.
        // Earlier fallback carets disappear when that first real span appears.
        let editor = app.editor_mut().unwrap();
        editor.scroll_offset = editor.line_count().saturating_sub(editor.viewport_height);
        let expected = reference_block(editor, area, start, 10, area.bottom() - 1);
        handle_block_drag(&mut app, 10, area.bottom() - 1);
        let editor = app.editor().unwrap();
        assert_eq!(state(editor), expected);
        assert!(editor.extra_cursors.is_empty());
        assert!(editor.extra_cursor_anchors.is_empty());
        assert!(editor.extra_cursors.capacity() <= MAX_CACHED_BLOCK_ROWS);
        assert!(editor.extra_cursor_anchors.capacity() <= MAX_CACHED_BLOCK_ROWS);
        assert!(app.block_drag_cache.is_none());
    }
}

#[test]
#[ignore = "release-mode comparative pointer/drag microbenchmark"]
fn benchmark_pointer_lookup_and_repeated_block_drag() {
    let text = "αβγ界 abcdefghijklmnop\n".repeat(5_000);
    let mut app = app_with_editor(&text, false);
    let area = app.code_area.unwrap();
    let editor = app.editor_mut().unwrap();
    editor.viewport_height = 240;
    let started = Instant::now();
    for sample in 0..50_000 {
        black_box(reference_hit(editor, area, 12, 2 + (sample % 240) as u16));
    }
    let reference = started.elapsed();
    let started = Instant::now();
    for sample in 0..50_000 {
        black_box(code_area_to_buffer_pos(
            editor,
            area,
            12,
            2 + (sample % 240) as u16,
        ));
    }
    let optimized = started.elapsed();
    eprintln!(
        "pointer lookup 50k samples / 240 rows: reference={reference:?}, direct={optimized:?}"
    );

    editor.folded_ranges.extend([(100, 2_000), (2_100, 4_500)]);
    let started = Instant::now();
    for sample in 0..50_000 {
        black_box(reference_hit(editor, area, 12, 2 + (sample % 240) as u16));
    }
    let reference = started.elapsed();
    let started = Instant::now();
    for sample in 0..50_000 {
        black_box(code_area_to_buffer_pos(
            editor,
            area,
            12,
            2 + (sample % 240) as u16,
        ));
    }
    let optimized = started.elapsed();
    eprintln!(
        "folded pointer lookup 50k samples / 240 rows: reference={reference:?}, direct={optimized:?}"
    );
    editor.folded_ranges.clear();

    let start = (0, 1, 1);
    app.middle_drag_start = Some(start);
    app.editor_mut().unwrap().viewport_height = 4_000;
    let area = Rect { height: 4_000, ..area };
    app.code_area = Some(area);
    let expected = reference_block(app.editor_mut().unwrap(), area, start, 10, 3_002);
    handle_block_drag(&mut app, 10, 3_002);
    assert_eq!(state(app.editor().unwrap()), expected);
    let started = Instant::now();
    for _ in 0..500 {
        black_box(reference_block(
            app.editor_mut().unwrap(),
            area,
            start,
            10,
            3_002,
        ));
    }
    let reference = started.elapsed();
    let started = Instant::now();
    for _ in 0..500 {
        handle_block_drag(&mut app, 10, 3_002);
        black_box(app.editor().unwrap().extra_cursors.as_slice());
    }
    let optimized = started.elapsed();
    assert_eq!(state(app.editor().unwrap()), expected);
    eprintln!(
        "block drag 500 identical samples / 3001 rows: reference={reference:?}, cached={optimized:?}"
    );

    let started = Instant::now();
    for sample in 0..500 {
        black_box(reference_block(
            app.editor_mut().unwrap(),
            area,
            start,
            10,
            3_002 + sample % 2,
        ));
    }
    let reference = started.elapsed();
    let started = Instant::now();
    for sample in 0..500 {
        handle_block_drag(&mut app, 10, 3_002 + sample % 2);
        black_box(app.editor().unwrap().extra_cursors.as_slice());
    }
    let optimized = started.elapsed();
    let expected = reference_block(app.editor_mut().unwrap(), area, start, 10, 3_003);
    assert_eq!(state(app.editor().unwrap()), expected);
    eprintln!(
        "block drag 500 moving endpoints / 3001 rows: reference={reference:?}, tail={optimized:?}"
    );
}
