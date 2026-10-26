use super::*;
use std::{hint::black_box, rc::Rc, time::Instant};

#[test]
fn prefix_presence_matches_text_queries_after_unicode_edits_and_outside_lines() {
    for source in ["", "one two\r\n", "界_café e\u{301}\t_ last", "\r\n\n", "x\u{2028}y\u{85}z"] {
        let mut editor = Editor::test_with_text(source);
        for step in 0..6 {
            for line in 0..editor.line_count() + 2 {
                editor.cursor_line = line;
                for col in (0..editor.line_len(line) + 3).chain([usize::MAX]) {
                    editor.cursor_col = col;
                    assert_eq!(editor.has_word_prefix_at_cursor(), editor.word_prefix_at_cursor().is_some(),
                        "source={source:?}, step={step}, {line}:{col}");
                }
            }
            editor.cursor_line = 0;
            editor.cursor_col = 0;
            match step {
                0 => editor.insert_char('界'),
                1 => editor.insert_char('_'),
                2 => { editor.cursor_col = 1; editor.backspace(); }
                3 => editor.undo(),
                4 => editor.redo(),
                _ => {}
            }
        }
    }
}

fn wrapped_editor() -> Editor {
    let mut editor = Editor::test_with_text(
        &("short\r\n\t界 alpha café βeta ".repeat(20) + "\r\n\nlast\u{2028}next\u{85}done\n").repeat(4));
    editor.word_wrap = true;
    editor.viewport_width = 13;
    editor.viewport_height = 5;
    editor
}

fn state(editor: &Editor) -> (usize, usize, usize, usize, Option<usize>) {
    (editor.cursor_line, editor.cursor_col, editor.scroll_offset, editor.wrap_scroll_offset, editor.desired_col)
}

#[test]
fn wrapped_navigation_and_visible_rows_match_rope_length_queries() {
    let mut actual = wrapped_editor();
    let mut expected = wrapped_editor();
    let operations: &[fn(&mut Editor)] = &[
        Editor::move_right, Editor::move_down, Editor::move_left, Editor::move_up,
        Editor::page_down, Editor::page_up, Editor::move_to_line_end,
        |ed| ed.scroll_down(3), |ed| ed.scroll_up(2),
        |ed| ed.insert_char('界'), Editor::backspace, Editor::insert_newline,
        Editor::undo, Editor::redo,
    ];
    for step in 0..280 {
        for editor in [&mut actual, &mut expected] {
            if step % 31 == 0 { editor.folded_ranges.insert(0, 3); editor.cached_row_offsets = None; }
        }
        REFERENCE_WRAP_LOOKUPS.set(true);
        operations[step % operations.len()](&mut expected);
        let reference_rows = expected.wrapped_rows(expected.wrap_scroll_offset, 12, 13);
        REFERENCE_WRAP_LOOKUPS.set(false);
        operations[step % operations.len()](&mut actual);
        let actual_rows = actual.wrapped_rows(actual.wrap_scroll_offset, 12, 13);
        assert_eq!(actual.rope, expected.rope, "step {step}");
        assert_eq!(state(&actual), state(&expected), "step {step}");
        let rows = |rows: Vec<WrappedRowInfo>| rows.into_iter()
            .map(|row| (row.line_idx, row.start_col, row.end_col, row.line_len, row.is_first)).collect::<Vec<_>>();
        assert_eq!(rows(actual_rows), rows(reference_rows), "step {step}");
    }
}

fn filled_wrap_cache(reverse: bool) -> Editor {
    let mut editor = Editor::test_with_text(&"alpha 界 beta gamma delta\n".repeat(MAX_CACHED_WRAP_LINES + 20));
    for line in 0..MAX_CACHED_WRAP_LINES {
        editor.wrap_ranges_for_line(line + if reverse { 20 } else { 0 }, 12);
    }
    editor
}

#[test]
fn wrap_cache_pressure_reclaims_one_far_entry_and_keeps_nearby_rows() {
    for reverse in [false, true] {
        let mut editor = filled_wrap_cache(reverse);
        let near = if reverse { 20 } else { MAX_CACHED_WRAP_LINES - 1 };
        let ranges = editor.wrap_ranges_for_line(near, 12);
        for line in 0..20 {
            let next = if reverse { 19 - line } else { MAX_CACHED_WRAP_LINES + line };
            let result = editor.wrap_ranges_for_line(next, 12);
            assert_eq!(*result, Editor::wrap_ranges_for_line_text("alpha 界 beta gamma delta", 12));
            assert_eq!(editor.wrap_cache.len(), MAX_CACHED_WRAP_LINES);
            assert!(Rc::ptr_eq(&ranges, &editor.wrap_ranges_for_line(near, 12)));
        }
        let far = if reverse { MAX_CACHED_WRAP_LINES + 19 } else { 0 };
        assert!(!editor.wrap_cache.contains_key(&(far, 12, editor.tab_size)));
        editor.cursor_line = near;
        editor.cursor_col = 0;
        editor.insert_char('x');
        assert_eq!(*editor.wrap_ranges_for_line(near, 12),
            Editor::wrap_ranges_for_line_text("xalpha 界 beta gamma delta", 12));
    }
}

#[test]
#[ignore = "manual release basic editor benchmark"]
fn benchmark_basic_wrap_cache_pressure() {
    for reverse in [false, true] {
        let mut before = Vec::new();
        let mut after = Vec::new();
        for sample in 0..11 {
            for optimized in [sample % 2 == 0, sample % 2 != 0] {
                let mut editor = filled_wrap_cache(reverse);
                REFERENCE_WRAP_EVICTION.set(!optimized);
                let started = Instant::now();
                let result = editor.wrap_ranges_for_line(if reverse { 19 } else { MAX_CACHED_WRAP_LINES }, 12);
                let elapsed = started.elapsed();
                REFERENCE_WRAP_EVICTION.set(false);
                assert_eq!(*result, Editor::wrap_ranges_for_line_text("alpha 界 beta gamma delta", 12));
                assert_eq!(editor.wrap_cache.len(), if optimized { MAX_CACHED_WRAP_LINES } else { 5001 });
                black_box(result);
                if optimized { after.push(elapsed); } else { before.push(elapsed); }
            }
        }
        before.sort(); after.sort();
        eprintln!("basic wrap cache at capacity reverse={reverse}: before={:?} after={:?}", before[5], after[5]);
    }
}

#[test]
#[ignore = "manual release basic editor benchmark"]
fn benchmark_basic_wrapped_cursor_and_prefix() {
    let mut editor = Editor::test_with_text(&"value argument another_value\n".repeat(20_000));
    editor.word_wrap = true;
    editor.viewport_width = 14;
    editor.viewport_height = 30;
    let offsets = Editor::build_wrap_row_offsets(&editor.rope, 14, 4, &[], || false).unwrap();
    editor.install_wrap_layout(14, offsets);
    let mut before = Vec::new();
    let mut after = Vec::new();
    for sample in 0..7 {
        for optimized in [sample % 2 == 0, sample % 2 != 0] {
            editor.cursor_line = 10_000;
            editor.cursor_col = 0;
            editor.desired_col = None;
            REFERENCE_WRAP_LOOKUPS.set(!optimized);
            let started = Instant::now();
            for _ in 0..2000 { editor.move_down(); editor.move_up(); black_box(state(&editor)); }
            let elapsed = started.elapsed() / 4000;
            REFERENCE_WRAP_LOOKUPS.set(false);
            assert_eq!(editor.cursor_line, 10_000);
            if optimized { after.push(elapsed); } else { before.push(elapsed); }
        }
    }
    before.sort(); after.sort();
    eprintln!("basic wrapped cursor movement: before={:?} after={:?}", before[3], after[3]);
    let mut editor = Editor::test_with_text(&"界".repeat(4096));
    editor.cursor_col = 4096;
    let mut before = Vec::new();
    let mut after = Vec::new();
    for sample in 0..7 {
        for optimized in [sample % 2 == 0, sample % 2 != 0] {
            let started = Instant::now();
            for _ in 0..2000 {
                assert!(black_box(if optimized { editor.has_word_prefix_at_cursor() }
                    else { editor.word_prefix_at_cursor().is_some_and(|word| !word.is_empty()) }));
            }
            let elapsed = started.elapsed() / 2000;
            if optimized { after.push(elapsed); } else { before.push(elapsed); }
        }
    }
    before.sort(); after.sort();
    eprintln!("basic Backspace prefix check 4096 Unicode chars: before={:?} after={:?}", before[3], after[3]);
}
