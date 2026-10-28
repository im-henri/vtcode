use super::*;
use crate::{app::Tab, editor::Editor};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn fixture(text: &str, bounded: bool, wrapped: bool) -> App {
    let mut editor = Editor::test_with_text(text);
    editor.large_file_mode = bounded;
    editor.word_wrap = wrapped;
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.show_minimap = false;
    app.sticky_scroll = false;
    app
}

fn draw(app: &mut App, area: Rect, reference: bool) -> Buffer {
    REFERENCE_CELL_PAINT.set(reference);
    let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
    terminal
        .draw(|frame| draw_editor(frame, app, area))
        .unwrap();
    REFERENCE_CELL_PAINT.set(false);
    terminal.backend().buffer().clone()
}

#[test]
fn shared_tab_and_space_runs_preserve_text_and_bound_retained_storage() {
    for width in [1, 2, 4, 8, 511, 512, 513, 1024] {
        REFERENCE_CELL_PAINT.set(false);
        let spaces = cell_spaces(width);
        let tab = marked_tab(width);
        assert_eq!(spaces, " ".repeat(width));
        assert_eq!(tab, format!("→{}", " ".repeat(width - 1)));
        assert_eq!(
            matches!(spaces, std::borrow::Cow::Borrowed(_)),
            width <= usize::from(u16::MAX)
        );
        assert_eq!(matches!(tab, std::borrow::Cow::Borrowed(_)), width <= 512);
        REFERENCE_CELL_PAINT.set(true);
        assert_eq!(spaces, cell_spaces(width));
        assert_eq!(tab, marked_tab(width));
    }
    REFERENCE_CELL_PAINT.set(false);
}

#[test]
fn streamed_syntax_colors_match_original_windows_and_malformed_length_tails() {
    let styles: Vec<_> = (0..18)
        .map(|index| {
            let mut style = PLAIN_STYLE;
            style.foreground = crate::textmate::Color {
                r: index * 11,
                g: 90,
                b: 200,
                a: if index % 2 == 0 { 255 } else { 90 },
            };
            (style, "界 e\u{301}".to_owned())
        })
        .collect();
    for lengths in [
        vec![0, 0, 2, 10, 0, 2, 70],
        vec![3; 18],
        vec![1; 22],
        vec![],
    ] {
        for background in [Color::Reset, Color::Rgb(10, 20, 30)] {
            for start in [0, 1, 7, 12, 30, 80, 150] {
                let end = start + 19;
                let expected =
                    syntax_foregrounds_for_range(&styles, &lengths, background, start, end);
                let mut actual =
                    SyntaxForegroundCursor::new(Some((&styles, &lengths)), background, start, end);
                for (offset, color) in expected.into_iter().enumerate() {
                    assert_eq!(
                        actual.at(start + offset),
                        color,
                        "start={start}, offset={offset}, lengths={lengths:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn bounded_search_runs_preserve_first_match_priority_with_overlaps_and_clipping() {
    let mut scratch = Vec::new();
    let mut output = Vec::new();
    for seed in 0..32 {
        let mut matches: Vec<_> = (0..64)
            .map(|index| {
                let start = (index * 37 + seed * 11) % 127;
                (start, start + (index * 13 + seed * 7) % 31, index % 3 == 0)
            })
            .collect();
        matches.extend([(149, usize::MAX, true), (142, 142, false), (80, 12, true)]);
        for start in [0, 1, 37, 100, 150, usize::MAX - 40] {
            let end = start.saturating_add(40);
            visible_search_runs(&matches, start, end, &mut scratch, &mut output);
            let mut cursor = ValueRangeCursor::new(&output, start);
            for column in start..end {
                let expected = matches.iter().find_map(|&(from, to, current)| {
                    (from <= column && column < to).then_some(current)
                });
                assert_eq!(cursor.at(column), expected, "seed={seed}, column={column}");
            }
        }
    }
}

fn original_matches_for_line(
    matches: &[(usize, usize, usize)],
    rope: &ropey::Rope,
    line: usize,
    current: usize,
) -> Vec<(usize, usize, bool)> {
    if line >= rope.len_lines() {
        return Vec::new();
    }
    let origin = rope.line_to_char(line);
    let text = rope.line(line);
    let mut length = text.len_chars();
    while length > 0 && matches!(text.char(length - 1), '\r' | '\n') {
        length -= 1;
    }
    let first = matches
        .partition_point(|&(row, _, _)| row < line)
        .saturating_sub(1);
    matches[first..]
        .iter()
        .enumerate()
        .take_while(|(_, (row, _, _))| *row <= line)
        .filter_map(|(index, &(row, start, end))| {
            let from = rope.line_to_char(row) + start;
            let to = rope.line_to_char(row) + end;
            if row < line && to <= origin {
                return None;
            }
            let column = from.saturating_sub(origin).min(length);
            let end = to.saturating_sub(origin).min(length);
            Some((column, end.max(column + 1), first + index == current))
        })
        .collect()
}

#[test]
fn search_line_origin_reuse_matches_original_for_unicode_multiline_and_newlines() {
    let source = format!(
        "{}\r\n界e\u{301} hit hit\n\r\nlast hit",
        "界 hit ".repeat(2000)
    );
    let rope = ropey::Rope::from_str(&source);
    let mut matches: Vec<_> = (0..2000)
        .map(|index| (0, index * 6 + 2, index * 6 + 5))
        .collect();
    matches.extend([
        (0, 12_000, 12_010),
        (1, 4, 7),
        (1, 8, 11),
        (2, 0, 2),
        (3, 5, 8),
        (3, 8, 8),
    ]);
    let mut actual = Vec::new();
    for line in 0..=rope.len_lines() {
        for current in [0, 17, 1999, 2000, matches.len() - 1, usize::MAX] {
            matches_for_line(&matches, &rope, line, current, &mut actual);
            assert_eq!(
                actual,
                original_matches_for_line(&matches, &rope, line, current),
                "line={line}, current={current}"
            );
        }
    }
}

#[test]
fn bounded_ascii_batches_preserve_unicode_scalar_spans_search_and_cursor_layers() {
    let text = concat!(
        "ASCII names and spaces \t 123 (value)\r\n",
        "a\u{301}e\u{fe0f} keycap 1\u{fe0f}\u{20e3} 👩‍👩‍👧‍👦 界 flag 🇪🇪\n",
        "\0\x1b\x07 ASCII\tand\tcontrols\n",
        "short\n",
        "\t\t\t\t indentation and spaces\n"
    );
    for width in [1, 8, 16, 47, 120] {
        for h_scroll in [0, 1, 8, 23] {
            for cursors in [false, true] {
                let mut app = fixture(text, true, false);
                app.cursor_visible = cursors;
                app.rendering_inactive_editor = !cursors;
                let editor = app.editor_mut().unwrap();
                editor.cursor_line = 1;
                editor.cursor_col = 4;
                editor.horizontal_scroll = h_scroll;
                editor.selection_anchor = Some((0, 3));
                editor.extra_cursors = vec![(0, 7), (1, 2), (2, 13), (3, 5)];
                app.search_matches = vec![(0, 1, 9), (1, 2, 10), (2, 4, 11), (3, 5, 6)];
                let area = Rect::new(0, 0, width, 10);
                let actual = draw(&mut app, area, false);
                let expected = draw(&mut app, area, true);
                assert_eq!(
                    actual, expected,
                    "width={width}, scroll={h_scroll}, cursors={cursors}"
                );
            }
        }
    }
}

#[test]
fn rich_tabs_virtual_selections_and_focus_dimming_match_original_frames() {
    let text = "\t \t ASCII 界 e\u{301}\tend\r\n\nshort\n".repeat(12);
    for wrapped in [false, true] {
        for mode in [
            WhitespaceMode::None,
            WhitespaceMode::All,
            WhitespaceMode::Boundary,
            WhitespaceMode::Selection,
            WhitespaceMode::Trailing,
        ] {
            for width in [9, 24, 90] {
                let mut app = fixture(&text, false, wrapped);
                app.cursor_visible = true;
                app.focus_mode = true;
                let editor = app.editor_mut().unwrap();
                editor.cursor_line = 1;
                editor.cursor_col = 75;
                editor.selection_anchor = Some((1, 12));
                editor.extra_cursors = vec![(2, 7), (4, 70), (5, 0)];
                editor.extra_cursor_anchors = vec![Some((2, 80)), Some((4, 20)), Some((5, 65))];
                editor.whitespace_mode = mode;
                let area = Rect::new(0, 0, width, 24);
                let actual = draw(&mut app, area, false);
                let expected = draw(&mut app, area, true);
                assert_eq!(
                    actual, expected,
                    "wrap={wrapped}, mode={mode:?}, width={width}"
                );
            }
        }
    }
}

#[test]
#[ignore = "manual comparative release cell batching benchmark"]
fn benchmark_editor_cell_batches() {
    use std::{hint::black_box, time::Instant};
    for case in [
        "bounded_ascii",
        "bounded_unicode",
        "bounded_search",
        "tabs",
        "virtual_selection",
    ] {
        let source = match case {
            "bounded_ascii" => {
                ("let repeated_name = function(argument, other_argument); ".repeat(120) + "\n")
                    .repeat(200)
            }
            "bounded_unicode" => {
                ("let 界_name = function(e\u{301}, 😀); ".repeat(120) + "\n").repeat(200)
            }
            "bounded_search" => ("value ".repeat(2000) + "\n").repeat(60),
            "tabs" => ("\t".repeat(150) + "\n").repeat(200),
            _ => "\n".repeat(200),
        };
        let mut app = fixture(&source, case.starts_with("bounded"), false);
        app.show_indent_guides = false;
        app.cursor_visible = true;
        if case == "tabs" {
            app.editor_mut().unwrap().whitespace_mode = WhitespaceMode::All;
        }
        if case == "virtual_selection" {
            let editor = app.editor_mut().unwrap();
            editor.extra_cursors = (0..50).map(|line| (line, 170)).collect();
            editor.extra_cursor_anchors = (0..50).map(|line| Some((line, 0))).collect();
        }
        if case == "bounded_search" {
            app.search_matches = (0..60)
                .flat_map(|line| (0..2000).map(move |index| (line, index * 6, index * 6 + 5)))
                .collect();
            app.editor_mut().unwrap().horizontal_scroll = 10_000;
        }
        let area = Rect::new(0, 0, 180, 50);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| draw_editor(frame, &mut app, area))
            .unwrap();
        let expected = terminal.backend().buffer().clone();
        let mut actual = Vec::new();
        let mut original = Vec::new();
        for round in 0..9 {
            for reference in if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                REFERENCE_CELL_PAINT.set(reference);
                let started = Instant::now();
                for _ in 0..8 {
                    terminal
                        .draw(|frame| draw_editor(frame, &mut app, area))
                        .unwrap();
                    black_box(terminal.backend().buffer());
                }
                (if reference {
                    &mut original
                } else {
                    &mut actual
                })
                .push(started.elapsed() / 8);
                assert_eq!(
                    terminal.backend().buffer(),
                    &expected,
                    "case={case}, reference={reference}"
                );
            }
        }
        REFERENCE_CELL_PAINT.set(false);
        actual.sort();
        original.sort();
        eprintln!(
            "cell batching case={case} optimized={:?} original={:?}",
            actual[4], original[4]
        );
    }
}
