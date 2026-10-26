use super::*;
use crate::{app::Tab, editor::Editor};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn pending_fixture(line: &str) -> App {
    let text = format!("{line}\n").repeat(100);
    assert!(text.len() > 256 * 1024);
    assert!(line.chars().count() < MAX_RICH_RENDER_LINE_CHARS);
    let mut editor = Editor::test_with_text(&text);
    editor.word_wrap = true;
    editor.file_path = Some("pending-wrap.css".into());
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.show_minimap = false;
    app.sticky_scroll = false;
    app.show_indent_guides = false;
    app.show_color_preview = true;
    app
}

fn draw(app: &mut App, area: Rect, original: bool) -> (Buffer, PendingWrapWork) {
    REFERENCE_PENDING_WRAP_PAINT.set(original);
    PENDING_WRAP_WORK.set(PendingWrapWork::default());
    let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
    terminal
        .draw(|frame| draw_editor(frame, app, area))
        .unwrap();
    REFERENCE_PENDING_WRAP_PAINT.set(false);
    (terminal.backend().buffer().clone(), PENDING_WRAP_WORK.get())
}

fn install_ready(app: &mut App) {
    let editor = app.editor_mut().unwrap();
    let width = editor.effective_wrap_width();
    let folds: Vec<_> = editor.folded_ranges.iter().map(|(&a, &b)| (a, b)).collect();
    let offsets =
        Editor::build_wrap_row_offsets(&editor.rope, width, editor.tab_size, &folds, || false)
            .unwrap();
    assert!(editor.install_wrap_layout(width, offsets));
}

#[test]
fn pending_prefix_preserves_tabs_skipped_graphemes_controls_and_combining_sequences() {
    assert_eq!(pending_wrap_paint_end("abcdef", 3, 4), 4);
    assert_eq!(pending_wrap_paint_end("界a界b", 1, 4), 4);
    assert_eq!(pending_wrap_paint_end("e\u{301}fghi", 1, 4), 3);
    assert_eq!(pending_wrap_paint_end("a\tbc", 3, 4), 2);
    assert_eq!(pending_wrap_paint_end("\0\x1ba\x07b", 1, 4), 5);
    assert_eq!(pending_wrap_paint_end("anything", 0, 4), 0);
}

#[test]
fn pending_wrapped_frames_match_original_cells_with_folds_unicode_decorations_and_clipped_eol() {
    for pattern in [
        "plain ({ value }) ",
        "界 e\u{301} 👩‍👩‍👧‍👦 () ",
        "⌚\u{fe0e}界 ae\u{301} () ",
        "\t  tomato #ff0000 rgb(12,34,56) \t ",
        "\0\x1ba\x07 界\t e\u{301} ",
        "界界界界", // width-one Paragraph ignores these entire graphemes
    ] {
        let line = pattern.repeat(4_000 / pattern.chars().count() + 1);
        let mut app = pending_fixture(&line);
        let editor = app.editor_mut().unwrap();
        editor.cursor_line = 2;
        editor.cursor_col = 5;
        editor.selection_anchor = Some((1, 3));
        editor.extra_cursors = vec![(2, 4), (2, 4), (3, line.chars().count()), (4, 1)];
        editor.extra_cursor_anchors = vec![Some((2, 1)), Some((3, 0)), None, Some((0, 2))];
        editor.folded_ranges.insert(6, 20);
        app.search_matches = vec![
            (1, 1, 20),
            (2, 4, 30),
            (3, line.chars().count(), line.chars().count() + 1),
        ];
        for width in [1, 7, 9, 23, 80, 181] {
            for mode in [
                WhitespaceMode::None,
                WhitespaceMode::Boundary,
                WhitespaceMode::Selection,
            ] {
                for visible in [false, true] {
                    app.cursor_visible = visible;
                    app.rendering_inactive_editor = !visible;
                    app.editor_mut().unwrap().whitespace_mode = mode;
                    let area = Rect::new(0, 0, width, 12);
                    let (actual, _) = draw(&mut app, area, false);
                    assert!(
                        !app.editor()
                            .unwrap()
                            .wrap_layout_ready(app.editor().unwrap().effective_wrap_width())
                    );
                    let (expected, _) = draw(&mut app, area, true);
                    assert_eq!(
                        actual, expected,
                        "pattern={pattern:?}, width={width}, mode={mode:?}, visible={visible}"
                    );
                }
            }
        }
    }
}

#[test]
fn pending_printable_work_is_bounded_by_viewport_and_ready_transition_keeps_exact_geometry() {
    for line_chars in [3_750, MAX_RICH_RENDER_LINE_CHARS - 2] {
        let line = "a".repeat(line_chars);
        let mut app = pending_fixture(&line);
        app.editor_mut().unwrap().cursor_col = line_chars;
        let area = Rect::new(0, 0, 180, 20);
        let (_, work) = draw(&mut app, area, false);
        assert!(work.pending_rows > 0);
        let width = app.code_area.unwrap().width as usize;
        assert!(work.styled <= (width + 1) * work.pending_rows, "{work:?}");
        assert!(
            work.requested <= (width + 2) * work.pending_rows,
            "{work:?}"
        );
        assert!(work.scanned <= (width + 1) * work.pending_rows, "{work:?}");
        assert!(
            work.spans <= (width * 2 + 4) * work.pending_rows,
            "{work:?}"
        );
        assert!(work.padding <= width * work.pending_rows, "{work:?}");
        for line in 0..work.pending_rows {
            let cache = app.editor().unwrap().render_line_cache.borrow();
            if let Some(glyphs) = cache.cached_line(line) {
                assert!(glyphs.chars.len() <= width + 2);
            }
        }
        install_ready(&mut app);
        let (actual, ready_work) = draw(&mut app, area, false);
        assert_eq!(ready_work.pending_rows, 0);
        let (expected, _) = draw(&mut app, area, true);
        assert_eq!(actual, expected);
        let editor = app.editor_mut().unwrap();
        let width = editor.effective_wrap_width();
        assert!(editor.wrap_layout_ready(width));
        assert!(editor.wrapped_total_rows(width) > editor.line_count());
        let row = editor.wrapped_row_for_position(0, line_chars, width);
        let info = editor.wrapped_row_at(row, width).unwrap();
        assert_eq!(info.line_idx, 0);
        assert_eq!(info.end_col, line_chars);
    }
}

#[test]
fn configured_wrap_column_reuses_exact_map_and_clamps_final_visual_rows() {
    let mut app = pending_fixture(&"word ".repeat(800));
    app.editor_mut().unwrap().word_wrap_column = 40;
    let area = Rect::new(0, 0, 180, 20);
    draw(&mut app, area, false);
    install_ready(&mut app);
    let editor = app.editor_mut().unwrap();
    let total = editor.wrapped_total_rows(40);
    editor.set_wrap_scroll_offset(usize::MAX);
    assert_eq!(editor.wrap_scroll_offset, total - 1);
    for _ in 0..4 {
        draw(&mut app, area, false);
        let editor = app.editor_mut().unwrap();
        assert!(editor.wrap_layout_ready(40));
        assert_eq!(editor.wrapped_total_rows(40), total);
        assert_eq!(editor.wrap_scroll_offset, total - 1);
    }
    // A resize still wider than the configured column must preserve this map.
    draw(&mut app, Rect::new(0, 0, 110, 20), false);
    assert!(app.editor().unwrap().wrap_layout_ready(40));
    draw(&mut app, Rect::new(0, 0, 30, 20), false);
    assert!(app.editor().unwrap().effective_wrap_width() < 40);
    assert!(
        !app.editor()
            .unwrap()
            .wrap_layout_ready(app.editor().unwrap().effective_wrap_width())
    );
}

#[test]
#[ignore = "comparative release pending versus exact ready wrap painting benchmark"]
fn benchmark_pending_and_ready_wrap_painting() {
    use std::{hint::black_box, time::Instant};
    for state in ["pending", "ready", "ready_column40"] {
        let mut app = pending_fixture(&"界)([{a}])(())) ".repeat(250));
        app.cursor_visible = true;
        if state.ends_with("40") {
            app.editor_mut().unwrap().word_wrap_column = 40;
        }
        let area = Rect::new(0, 0, 180, 50);
        draw(&mut app, area, false);
        if state != "pending" {
            install_ready(&mut app);
        }
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| draw_editor(frame, &mut app, area))
            .unwrap();
        let expected = terminal.backend().buffer().clone();
        let mut optimized = Vec::new();
        let mut original = Vec::new();
        for round in 0..9 {
            for reference in if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                REFERENCE_PENDING_WRAP_PAINT.set(reference);
                let start = Instant::now();
                for _ in 0..5 {
                    terminal
                        .draw(|frame| draw_editor(frame, &mut app, area))
                        .unwrap();
                    black_box(terminal.backend().buffer());
                }
                (if reference {
                    &mut original
                } else {
                    &mut optimized
                })
                .push(start.elapsed() / 5);
                assert_eq!(
                    terminal.backend().buffer(),
                    &expected,
                    "state={state}, reference={reference}"
                );
            }
        }
        REFERENCE_PENDING_WRAP_PAINT.set(false);
        optimized.sort();
        original.sort();
        eprintln!(
            "wrapped paint state={state} optimized={:?} original_full_prefix={:?}",
            optimized[4], original[4]
        );
    }
}
