use crate::{
    app::{App, Tab},
    editor::Editor,
    ui_editor_highlight::{
        ExtraCursorLine, ExtraCursorViewport, HighlightedLine, PLAIN_STYLE,
        REFERENCE_EXTRA_CURSOR_PROJECTION, RenderLineCache,
    },
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect};

fn prime(cache: &mut RenderLineCache, rope: &ropey::Rope, line: usize, version: u64, wrap: u64) {
    let text = rope.line(line).to_string();
    let clean = text.trim_end_matches(['\r', '\n']);
    let view = HighlightedLine::new(None, clean, clean.chars().count(), PLAIN_STYLE);
    cache.line(line, (version, wrap, 4), rope, &view, None, 100, false);
}

#[test]
fn bracket_event_transforms_match_original_clamping_saturation_and_unicode_columns() {
    let mut cache = RenderLineCache::default();
    let texts = [
        "界)([a{😀}])(())) ((( { [ ] }",
        "))))((((()){}[]界",
        "[[[[))))}}}}(((({",
        "ordinary text \t e\u{301} without brackets",
    ];
    for text in texts {
        let rope = ropey::Rope::from_str(text);
        prime(&mut cache, &rope, 0, 0, 0);
        for depth in [0, 1, 2, 99, i32::MAX - 2, i32::MAX] {
            cache.reference_brackets = false;
            let actual = cache.brackets(0, text, depth);
            cache.reference_brackets = true;
            let expected = cache.brackets(0, text, depth);
            cache.reference_brackets = false;
            assert_eq!(
                actual.end_depth(),
                expected.end_depth(),
                "text={text}, depth={depth}"
            );
            for seek in [0, 1, 5, 12, 100] {
                let mut actual = actual.cursor(seek);
                let mut expected = expected.cursor(seek);
                for col in seek..text.chars().count() + 3 {
                    assert_eq!(
                        actual.at(col),
                        expected.at(col),
                        "text={text}, depth={depth}, col={col}"
                    );
                }
            }
        }
        assert_eq!(
            cache.bracket_builds,
            texts.iter().position(|&value| value == text).unwrap() + 1
        );
    }
}

#[test]
fn bracket_transforms_preserve_every_small_sequence_and_the_admitted_line_limit() {
    let mut cache = RenderLineCache::default();
    let mut cases: Vec<String> = (0..256usize)
        .map(|bits| {
            (0..8)
                .map(|bit| if bits & (1 << bit) == 0 { '(' } else { ')' })
                .collect()
        })
        .collect();
    cases.extend([
        "(".repeat(crate::background::MAX_BRACKET_LINE_CHARS),
        ")".repeat(crate::background::MAX_BRACKET_LINE_CHARS),
        ")(".repeat(crate::background::MAX_BRACKET_LINE_CHARS / 2),
        "()".repeat(crate::background::MAX_BRACKET_LINE_CHARS / 2),
    ]);
    for text in cases {
        let rope = ropey::Rope::from_str(&text);
        prime(&mut cache, &rope, 0, 0, 0);
        for start in [0, 1, 7, i32::MAX - 3, i32::MAX] {
            cache.reference_brackets = false;
            let actual = cache.brackets(0, &text, start);
            cache.reference_brackets = true;
            let expected = cache.brackets(0, &text, start);
            cache.reference_brackets = false;
            assert_eq!(
                actual.end_depth(),
                expected.end_depth(),
                "start={start}, text_len={}",
                text.len()
            );
            let mut actual = actual.cursor(0);
            let mut expected = expected.cursor(0);
            for column in 0..text.len() {
                assert_eq!(
                    actual.at(column),
                    expected.at(column),
                    "start={start}, column={column}"
                );
            }
        }
    }
}

#[test]
fn bracket_metadata_obeys_dirty_source_reuse_and_shared_memory_budget() {
    let mut rope = ropey::Rope::from_str(&("界()[]{}".repeat(500) + "\n").repeat(300));
    let mut cache = RenderLineCache::default();
    for line in 0..300 {
        prime(&mut cache, &rope, line, 0, 0);
        let text = rope.line(line).to_string();
        cache.brackets(line, text.trim_end(), 0);
        let (lines, bytes, _) = cache.decoration_storage();
        assert!(lines <= 256 && bytes <= 2 * 1024 * 1024);
    }
    let before = cache.bracket_builds;
    prime(&mut cache, &rope, 299, 0, 0);
    let text = rope.line(299).to_string();
    cache.brackets(299, text.trim_end(), 23);
    assert_eq!(
        cache.bracket_builds, before,
        "starting depth is not a source revision"
    );
    let changed = rope.line_to_char(298);
    rope.remove(changed..changed + 1);
    rope.insert(changed, "]");
    cache.mark_dirty(298, 298, 1, 1, &rope);
    prime(&mut cache, &rope, 298, 1, 1);
    let text = rope.line(298).to_string();
    let actual = cache.brackets(298, text.trim_end(), 1);
    cache.reference_brackets = true;
    let expected = cache.brackets(298, text.trim_end(), 1);
    assert_eq!(actual.end_depth(), expected.end_depth());
    assert_eq!(cache.bracket_builds, before + 1);
}

#[test]
fn viewport_multicursor_projection_matches_independent_per_line_scans() {
    let mut editor = Editor::test_with_text(&"value界\n".repeat(500));
    for seed in 0..24usize {
        editor.extra_cursors = (0..150)
            .map(|index| ((index * 37 + seed) % 501, (index * 19 + seed) % 64))
            .collect();
        editor
            .extra_cursors
            .extend([(10, 7), (10, 7), (100, usize::MAX), (200, 0)]);
        editor.extra_cursor_anchors = editor
            .extra_cursors
            .iter()
            .enumerate()
            .map(|(index, &(line, col))| {
                (index % 5 != 0).then_some(if index % 3 == 0 {
                    (line, col.saturating_sub(index % 9))
                } else {
                    ((index * 13 + seed) % 501, index % 17)
                })
            })
            .collect();
        // Mismatched anchor vectors are legal while a cursor list is rebuilt.
        if seed % 2 == 0 {
            editor.extra_cursor_anchors.truncate(140);
        }
        let visible = [0, 1, 10, 10, 20, 99, 100, 101, 200, 499, 500];
        let projected = ExtraCursorViewport::new(&editor, visible.into_iter());
        REFERENCE_EXTRA_CURSOR_PROJECTION.set(true);
        let reference = ExtraCursorViewport::new(&editor, visible.into_iter());
        REFERENCE_EXTRA_CURSOR_PROJECTION.set(false);
        let mut actual_scratch = ExtraCursorLine::default();
        let mut expected_scratch = ExtraCursorLine::default();
        for line in visible {
            let actual = projected.line(&editor, line, &mut actual_scratch);
            let expected = reference.line(&editor, line, &mut expected_scratch);
            assert_eq!(actual.columns, expected.columns, "seed={seed}, line={line}");
            assert_eq!(
                actual.selections, expected.selections,
                "seed={seed}, line={line}"
            );
            assert_eq!(
                actual.virtual_selections, expected.virtual_selections,
                "seed={seed}, line={line}"
            );
        }
    }
}

fn fixture(source: &str, wrapped: bool) -> App {
    let mut editor = Editor::test_with_text(source);
    editor.word_wrap = wrapped;
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.show_minimap = false;
    app.show_color_preview = false;
    app.sticky_scroll = false;
    app
}

fn reference(app: &App, enabled: bool) {
    app.editor()
        .unwrap()
        .render_line_cache
        .borrow_mut()
        .reference_brackets = enabled;
    REFERENCE_EXTRA_CURSOR_PROJECTION.set(enabled);
}

fn draw(app: &mut App, area: Rect) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
    terminal
        .draw(|frame| super::draw_editor(frame, app, area))
        .unwrap();
    terminal.backend().buffer().clone()
}

#[test]
fn bracket_and_multicursor_frames_preserve_folds_wraps_selection_priority_and_clipping() {
    let source: String = (0..80)
        .map(|line| {
            format!(
                "  界 }})([a{{😀}}])(())) e\u{301}\t{}\n",
                "({ token }) ".repeat(line % 9)
            )
        })
        .collect();
    for (wrapped, bounded) in [(false, false), (true, false), (false, true)] {
        let mut app = fixture(&source, wrapped);
        let editor = app.editor_mut().unwrap();
        editor.large_file_mode = bounded;
        editor.cursor_line = 9;
        editor.cursor_col = 8;
        editor.selection_anchor = Some((8, 3));
        editor.extra_cursors = vec![
            (9, 4),
            (9, 8),
            (9, 80),
            (10, 3),
            (10, 3),
            (11, 0),
            (70, 100),
            (2, 7),
        ];
        editor.extra_cursor_anchors = vec![
            Some((8, 2)),
            None,
            Some((9, 3)),
            Some((11, 6)),
            Some((10, 3)),
            Some((0, 0)),
            Some((79, 8)),
            Some((20, 4)),
        ];
        editor.folded_ranges.insert(2, 8);
        editor.folded_ranges.insert(20, 60);
        for width in [1, 9, 30, 90] {
            for scroll in [0, 8, 9, 10, 61] {
                for cursors in [false, true] {
                    app.cursor_visible = cursors;
                    app.rendering_inactive_editor = !cursors;
                    let editor = app.editor_mut().unwrap();
                    editor.scroll_offset = scroll;
                    editor.wrap_scroll_offset = scroll;
                    editor.horizontal_scroll = if wrapped { 0 } else { scroll % 7 };
                    let area = Rect::new(0, 0, width, 15);
                    reference(&app, false);
                    let actual = draw(&mut app, area);
                    reference(&app, true);
                    let expected = draw(&mut app, area);
                    reference(&app, false);
                    assert_eq!(
                        actual, expected,
                        "wrapped={wrapped}, width={width}, scroll={scroll}, cursors={cursors}"
                    );
                }
            }
        }
    }
}

#[test]
#[ignore = "manual comparative release bracket and multicursor redraw benchmark"]
fn benchmark_bracket_and_multicursor_redraws() {
    use std::{hint::black_box, time::Instant};
    for case in [
        "brackets",
        "wrapped_brackets",
        "primary_cursor",
        "one_caret",
        "100_carets",
        "10000_carets",
        "10000_multiline_selections",
        "bounded_10000_carets",
    ] {
        // Fixtures retain capacity only within themselves. A bracket-heavy
        // case must not impose a large clear() on later ordinary code lines.
        crate::ui_editor_highlight::reset_reference_bracket_pool();
        let bracket = case.contains("brackets");
        let source = if bracket {
            ("界)([{a}])(())) ".repeat(250) + "\n").repeat(100)
        } else {
            "let value = (x + 界);\n".repeat(50_000)
        };
        let mut app = fixture(&source, case == "wrapped_brackets");
        app.cursor_visible = true;
        app.show_indent_guides = false;
        if !bracket {
            let editor = app.editor_mut().unwrap();
            editor.large_file_mode = case.starts_with("bounded");
            let count = match case {
                "primary_cursor" => 0,
                "one_caret" => 1,
                "100_carets" => 100,
                _ => 10_000,
            };
            editor.extra_cursors = (0..count).map(|index| (index * 5, index % 18)).collect();
            if case.contains("selections") {
                editor.extra_cursor_anchors = (0..10_000)
                    .map(|index| Some((49_999 - index, index % 17)))
                    .collect();
            }
        }
        let area = Rect::new(0, 0, 180, 50);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| super::draw_editor(frame, &mut app, area))
            .unwrap();
        let expected = terminal.backend().buffer().clone();
        let mut fast = Vec::new();
        let mut original = Vec::new();
        for round in 0..9 {
            for old in if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                reference(&app, old);
                let start = Instant::now();
                for _ in 0..5 {
                    terminal
                        .draw(|frame| super::draw_editor(frame, &mut app, area))
                        .unwrap();
                    black_box(terminal.backend().buffer());
                }
                (if old { &mut original } else { &mut fast }).push(start.elapsed() / 5);
                assert_eq!(
                    terminal.backend().buffer(),
                    &expected,
                    "case={case}, reference={old}"
                );
            }
        }
        reference(&app, false);
        fast.sort();
        original.sort();
        eprintln!(
            "editor hotpath case={case} optimized={:?} pooled_original_scan_oracle={:?}",
            fast[4], original[4]
        );
    }
}
