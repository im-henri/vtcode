use super::*;
use crate::editor::Editor;
use ratatui::{Terminal, backend::TestBackend};

#[test]
fn cached_minimap_matches_reference_after_typing_history_line_shifts_and_theme_changes() {
    let mut editor = Editor::test_with_text(&"name = 123;\n\t界 = 'hello';\r\n  \n".repeat(60));
    let mut terminal = Terminal::new(TestBackend::new(134, 42)).unwrap();
    let mut reference_terminal = Terminal::new(TestBackend::new(134, 42)).unwrap();
    for step in 0..160 {
        editor.cursor_line = (step * 7) % editor.line_count();
        editor.cursor_col = (step * 3) % (editor.line_len(editor.cursor_line) + 1);
        match step % 10 {
            0 => editor.insert_char('界'),
            1 => editor.insert_char('\n'),
            2 => editor.insert_char('\t'),
            3 => editor.backspace(),
            4 => editor.delete_char(),
            5 => editor.undo(),
            6 => editor.redo(),
            7 => {
                editor.selection_anchor = Some((editor.cursor_line.saturating_sub(2), 0));
                editor.delete_selection();
            }
            8 => editor.insert_char('\r'),
            _ => editor.insert_char('"'),
        }
        editor.selection_anchor = (step % 3 == 0).then_some((0, 0));
        let width = [0, 1, 2, 3, 24, 128, 129][step / 10 % 7];
        let changes = [(editor.cursor_line, 'U')];
        let diagnostics = [(editor.cursor_line, 1)];
        let search = [(editor.cursor_line / 2, 0, 1)];
        let params = MinimapParams {
            area: Rect::new(2, 1, width, 40),
            editor: &editor,
            scroll: step % editor.line_count(),
            visible_lines: 20,
            search_matches: &search,
            diagnostic_lines: &diagnostics,
            diagnostic_index: None,
            change_index: None,
            diff_changes: (step % 2 == 0).then_some(&changes),
            colors: if step % 2 == 0 { crate::app_theme::EditorTheme::Dark } else { crate::app_theme::EditorTheme::Light }.colors(),
            hovered: step % 3 == 0,
            active: step % 3 == 1,
        };
        terminal.draw(|frame| draw_minimap(frame, params)).unwrap();
        reference_terminal.draw(|frame| reference::draw_minimap_reference(frame, params)).unwrap();
        assert_eq!(terminal.backend().buffer(), reference_terminal.backend().buffer(), "step {step}");
        editor.clear_selection();
    }
}

#[test]
fn direct_minimap_cells_match_paragraph_styles_for_unicode_markers_and_clipping() {
    let source = "let 界 = 123;\n\"text\" '\t',\n\r\n".repeat(48);
    let mut editor = Editor::test_with_text(&source);
    editor.selection_anchor = Some((5, 1));
    editor.cursor_line = 17;
    editor.cursor_col = 4;
    editor.bookmarks.extend([0, 9, 42, 101]);
    let changes = [(0, 'M'), (7, 'D'), (12, 'N'), (51, 'S')];
    let diagnostics = [(1, 2), (7, 1), (21, 3), (80, 2)];
    let search = [(0, 1, 3), (6, 2, 5), (34, 0, 2), (110, 0, 3)];
    let change_index = ChangeLineIndex::from_changes(&changes);
    let diagnostic_index = DiagnosticLineIndex::from_diagnostics(&diagnostics);
    for theme in [
        crate::app_theme::EditorTheme::Dark,
        crate::app_theme::EditorTheme::Light,
    ] {
        for width in [1, 2, 7, 24] {
            for height in [1, 5, 31] {
                for scroll in [0, 22, 170] {
                    for state in 0..3 {
                        let params = MinimapParams {
                            area: Rect::new(2, 1, width, height),
                            editor: &editor,
                            scroll,
                            visible_lines: 20,
                            search_matches: &search,
                            diagnostic_lines: &diagnostics,
                            diagnostic_index: (state != 0).then_some(&diagnostic_index),
                            change_index: (state != 0).then_some(&change_index),
                            diff_changes: Some(&changes),
                            colors: theme.colors(),
                            hovered: state == 1,
                            active: state == 2,
                        };
                        let mut expected = None;
                        for direct in [false, true] {
                            let mut terminal =
                                Terminal::new(TestBackend::new(width + 4, height + 2)).unwrap();
                            terminal
                                .draw(|frame| {
                                    let frame_area = frame.area();
                                    frame.buffer_mut().set_style(
                                        frame_area,
                                        Style::default()
                                            .fg(Color::Rgb(91, 92, 93))
                                            .add_modifier(Modifier::ITALIC),
                                    );
                                    if direct {
                                        draw_minimap(frame, params);
                                    } else {
                                        reference::draw_minimap_reference(frame, params);
                                    }
                                })
                                .unwrap();
                            let actual = terminal.backend().buffer().clone();
                            if let Some(expected) = &expected {
                                assert_eq!(&actual, expected);
                            } else {
                                expected = Some(actual);
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
#[ignore = "manual release direct minimap timing against the Paragraph oracle"]
fn benchmark_minimap_direct_cells() {
    use std::{hint::black_box, time::Instant};
    let (editor, diagnostics, changes) = dense_fixture(50_000);
    let diagnostic_index = DiagnosticLineIndex::from_diagnostics(&diagnostics);
    let change_index = ChangeLineIndex::from_changes(&changes);
    let params = MinimapParams {
        area: Rect::new(0, 0, 24, 60),
        editor: &editor,
        scroll: 20_000,
        visible_lines: 60,
        search_matches: &[],
        diagnostic_lines: &diagnostics,
        diagnostic_index: Some(&diagnostic_index),
        change_index: Some(&change_index),
        diff_changes: Some(&changes),
        colors: crate::app_theme::EditorTheme::Dark.colors(),
        hovered: false,
        active: false,
    };
    let mut timings = Vec::new();
    for direct in [false, true] {
        let mut terminal = Terminal::new(TestBackend::new(24, 60)).unwrap();
        let mut samples = Vec::new();
        for _ in 0..7 {
            let start = Instant::now();
            for _ in 0..100 {
                terminal
                    .draw(|frame| {
                        if direct {
                            draw_minimap(frame, params);
                        } else {
                            reference::draw_minimap_reference(frame, params);
                        }
                    })
                    .unwrap();
                black_box(terminal.backend().buffer());
            }
            samples.push(start.elapsed() / 100);
        }
        samples.sort();
        timings.push(samples[3]);
    }
    eprintln!(
        "24x60 minimap Paragraph={:?} direct cells={:?}",
        timings[0], timings[1]
    );
}

#[test]
fn direct_scrollbar_cells_match_paragraph_markers_mapped_ranges_and_empty_tracks() {
    let diagnostics = [(0, 3), (7, 1), (20, 2), (200, 1)];
    let search = [(2, 1, 3), (15, 0, 3), (90, 0, 2), (200, 0, 2)];
    let mapping: Vec<_> = (0..100).map(|line| line * 3 / 4).collect();
    let diagnostic_index = DiagnosticLineIndex::from_diagnostics(&diagnostics);
    for width in [1, 2, 7] {
        for height in [1, 9, 31] {
            for state in 0..4 {
                let changes = ChangeRows::from_lines(height as usize, 80, [(0, 'N'), (4, 'D'), (12, 'S'), (28, 'M')]);
                let params = ScrollbarParams {
                    area: Rect::new(3, 2, width, height),
                    total_lines: if state == 0 { 0 } else { 80 },
                    visible_lines: 20,
                    scroll: 23,
                    search_matches: &search,
                    diagnostic_lines: &diagnostics,
                    diagnostic_index: (state == 3).then_some(&diagnostic_index),
                    change_rows: &changes,
                    cursor_line: 13,
                    colors: if state == 2 {
                        crate::app_theme::EditorTheme::Light.colors()
                    } else {
                        crate::app_theme::EditorTheme::Dark.colors()
                    },
                    hovered: state == 1,
                    active: state == 2,
                };
                let mapping = (state >= 2).then_some(mapping.as_slice());
                let mut expected = None;
                for direct in [false, true] {
                    let mut terminal =
                        Terminal::new(TestBackend::new(width + 6, height + 4)).unwrap();
                    terminal
                        .draw(|frame| {
                            let buffer = frame.buffer_mut();
                            for cell in &mut buffer.content {
                                cell.set_char('q').set_style(
                                    Style::default()
                                        .fg(Color::Yellow)
                                        .add_modifier(Modifier::ITALIC),
                                );
                            }
                            if direct {
                                draw_scrollbar_with_mapping(frame, params, mapping);
                            } else {
                                reference::draw_scrollbar_reference(frame, params, mapping);
                            }
                        })
                        .unwrap();
                    let actual = terminal.backend().buffer().clone();
                    if let Some(expected) = &expected {
                        assert_eq!(&actual, expected);
                    } else {
                        expected = Some(actual);
                    }
                }
            }
        }
    }
}

fn dense_fixture(lines: usize) -> (Editor, Vec<(usize, u8)>, Vec<(usize, char)>) {
    let mut editor = Editor::test_with_text(&"let 界 = 123;\n".repeat(lines));
    let statuses: Vec<_> = (0..lines * 6 / 5)
        .map(|line| match line % 11 {
            0 => 'M',
            1 => 'D',
            2 => 'S',
            3 => 'U',
            4 => 'A',
            5 => 'N',
            _ => ' ',
        })
        .collect();
    editor.set_git_line_status(statuses);
    let diagnostics = (0..lines * 8 / 5)
        .map(|index| (index * 17 % lines, 2 + (index % 3) as u8))
        .collect::<Vec<_>>();
    let mut diagnostics = diagnostics;
    diagnostics.sort_unstable_by_key(|&(line, _)| line);
    let mut changes = editor.git_change_lines().to_vec();
    changes.push((usize::MAX, 'D'));
    (editor, diagnostics, changes)
}

#[test]
fn dense_priority_indexes_match_scan_oracles_and_clamp_stale_markers_at_every_height() {
    // Thousands of dense markers exercise every bucket/priority; document
    // scaling is measured separately by the full-size ignored benchmark.
    let (mut editor, diagnostics, changes) = dense_fixture(2_000);
    let diagnostic_index = DiagnosticLineIndex::from_diagnostics(&diagnostics);
    let change_index = ChangeLineIndex::from_changes(&changes);
    let git_index = ChangeLineIndex::from_changes(editor.git_change_lines());
    for height in [1, 2, 7, 31, 60, 87, 120] {
        assert_eq!(
            scrollbar_change_rows_indexed(&editor, height, true, &change_index),
            scrollbar_change_rows(&editor, height, Some(&changes))
        );
        assert_eq!(
            scrollbar_change_rows_indexed(&editor, height, false, &git_index),
            scrollbar_change_rows(&editor, height, None)
        );
        for row in 0..height {
            let start = row * editor.line_count() / height;
            let end = ((row + 1) * editor.line_count() / height).max(start + 1);
            assert_eq!(
                diagnostic_index.best_in_range(start, end),
                worst_diagnostic_in_range(&diagnostics, start, end)
            );
        }
    }
    editor.insert_char('x');
    assert_eq!(
        scrollbar_change_rows_indexed(&editor, 63, false, &git_index),
        scrollbar_change_rows(&editor, 63, None)
    );
    let tiny = Editor::test_with_text("a\nb\n");
    let beyond_eof = [(0, 'S'), (2, 'M'), (99, 'N'), (usize::MAX, 'D')];
    let index = ChangeLineIndex::from_changes(&beyond_eof);
    for height in [1, 2, 3, 5, 20, 100] {
        assert_eq!(
            scrollbar_change_rows_indexed(&tiny, height, true, &index),
            scrollbar_change_rows(&tiny, height, Some(&beyond_eof))
        );
    }
}

#[test]
fn indexed_minimap_and_scrollbar_frames_match_scans_across_resize_and_scroll() {
    let (editor, diagnostics, changes) = dense_fixture(2_000);
    let diagnostic_index = DiagnosticLineIndex::from_diagnostics(&diagnostics);
    let change_index = ChangeLineIndex::from_changes(&changes);
    let colors = crate::app::App::test_new().colors;
    for height in [1, 7, 30, 65] {
        let change_rows = scrollbar_change_rows_indexed(&editor, height, true, &change_index);
        for scroll in [0, editor.line_count() * 2 / 5, editor.line_count() - 11] {
            let mut expected = None;
            for indexed in [false, true] {
                let mut terminal = Terminal::new(TestBackend::new(14, height as u16)).unwrap();
                terminal
                    .draw(|frame| {
                        draw_minimap(
                            frame,
                            MinimapParams {
                                area: Rect::new(0, 0, 12, height as u16),
                                editor: &editor,
                                scroll,
                                visible_lines: 25,
                                search_matches: &[],
                                diagnostic_lines: &diagnostics,
                                diagnostic_index: indexed.then_some(&diagnostic_index),
                                change_index: indexed.then_some(&change_index),
                                diff_changes: Some(&changes),
                                colors,
                                hovered: true,
                                active: false,
                            },
                        );
                        draw_scrollbar(
                            frame,
                            ScrollbarParams {
                                area: Rect::new(12, 0, 2, height as u16),
                                total_lines: editor.line_count(),
                                visible_lines: 25,
                                scroll,
                                search_matches: &[],
                                diagnostic_lines: &diagnostics,
                                diagnostic_index: indexed.then_some(&diagnostic_index),
                                change_rows: &change_rows,
                                cursor_line: 25,
                                colors,
                                hovered: true,
                                active: false,
                            },
                        );
                    })
                    .unwrap();
                if let Some(expected) = &expected {
                    assert_eq!(terminal.backend().buffer(), expected);
                } else {
                    expected = Some(terminal.backend().buffer().clone());
                }
            }
        }
    }
}

#[test]
#[ignore = "manual release changing-height marker benchmark"]
fn benchmark_dense_overview_resizing() {
    use std::{hint::black_box, time::Instant};
    let (editor, diagnostics, changes) = dense_fixture(50_000);
    let diagnostic_index = DiagnosticLineIndex::from_diagnostics(&diagnostics);
    let change_index = ChangeLineIndex::from_changes(&changes);
    let mut before = Vec::new();
    let mut after = Vec::new();
    for sample in 0..9 {
        for indexed in [sample % 2 == 0, sample % 2 != 0] {
            let started = Instant::now();
            for _ in 0..20 {
                for height in 30..70 {
                    let rows = if indexed {
                        scrollbar_change_rows_indexed(&editor, height, true, &change_index)
                    } else {
                        scrollbar_change_rows(&editor, height, Some(&changes))
                    };
                    black_box(rows);
                    for row in 0..height {
                        let start = row * editor.line_count() / height;
                        let end = (row + 1) * editor.line_count() / height;
                        let severity = if indexed {
                            diagnostic_index.best_in_range(start, end)
                        } else {
                            worst_diagnostic_in_range(&diagnostics, start, end)
                        };
                        black_box(severity);
                    }
                }
            }
            let elapsed = started.elapsed() / 800;
            if indexed {
                after.push(elapsed);
            } else {
                before.push(elapsed);
            }
        }
    }
    before.sort();
    after.sort();
    eprintln!(
        "changing-height overview 80000 diagnostics/32729 markers: scans={:?} indexed={:?}",
        before[4], after[4]
    );
}
