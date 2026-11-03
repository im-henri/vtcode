use super::{Editor, FoldedRanges};
use crate::{
    app::{App, Tab},
    ui_editor_minimap::*,
};
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect};

fn editor(lines: usize) -> Editor {
    let text = (0..lines)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut editor = Editor::test_with_text(&text);
    editor.viewport_width = 80;
    editor.viewport_height = 8;
    editor.cursor_surrounding_lines = 0;
    editor.scroll_past_end = false;
    editor
}

#[test]
fn fold_rows_match_visible_source_lines_and_invalidate_after_mutation() {
    let mut editor = editor(60);
    for folds in [
        vec![],
        vec![(2, 20), (30, 50)],
        vec![(0, 20), (3, 10)],
        vec![(0, 20), (3, 30)],
        vec![(2, 60)],
        vec![(0, 100)],
    ] {
        editor.folded_ranges.clear();
        editor.folded_ranges.extend(folds.iter().copied());
        let visible: Vec<_> = (0..60)
            .filter(|&line| !folds.iter().any(|&(s, e)| s < line && line < e))
            .collect();
        assert_eq!(editor.visible_line_count(), visible.len());
        for (row, &line) in visible.iter().enumerate() {
            assert_eq!(editor.visible_row_for_line(line), row);
            assert_eq!(editor.line_for_visible_row(row), line);
        }
        assert_eq!(editor.line_for_visible_row(visible.len()), 60);
        for source in 0..60 {
            let row = visible.partition_point(|&line| line <= source) - 1;
            assert_eq!(editor.visible_row_for_line(source), row);
            assert_eq!(editor.is_line_folded(source), !visible.contains(&source));
            let expected: Vec<_> = visible
                .iter()
                .copied()
                .filter(|&line| line >= source)
                .take(12)
                .collect();
            assert_eq!(editor.visible_lines(source, 12), expected);
        }
        editor.word_wrap = true;
        editor.cached_row_offsets = None;
        editor.cached_wrapped_total = None;
        assert_eq!(editor.wrapped_total_rows(80), visible.len());
        let wrapped: Vec<_> = editor
            .wrapped_rows(0, 60, 80)
            .iter()
            .map(|row| row.line_idx)
            .collect();
        assert_eq!(wrapped, visible);
    }
    // Replacing an interval without changing the map length must also discard
    // the retained index. Restores and split views mutate folds directly.
    editor.folded_ranges.clear();
    editor.folded_ranges.insert(2, 20);
    assert_eq!(editor.line_for_visible_row(3), 20);
    editor.folded_ranges.insert(2, 30);
    assert_eq!(editor.line_for_visible_row(3), 30);
    let clone = editor.folded_ranges.clone();
    editor.folded_ranges.clear();
    assert_eq!(clone.line_for_row(3, 60), 30);
    assert_eq!(editor.visible_line_count(), 60);
}

#[test]
fn keyboard_page_navigation_skips_folded_bodies() {
    let mut app = App::test_new();
    let mut editor = editor(500);
    editor.folded_ranges.insert(2, 490);
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    crate::input::handle_editor_key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
    assert_eq!(app.editor().unwrap().scroll_offset, 492);
    assert_eq!(app.editor().unwrap().cursor_line, 493);
    crate::input::handle_editor_key(&mut app, KeyCode::PageUp, KeyModifiers::NONE);
    assert_eq!(app.editor().unwrap().scroll_offset, 0);
    assert_eq!(app.editor().unwrap().cursor_line, 0);
}

#[test]
fn sparse_fold_index_handles_huge_hidden_ranges_without_per_line_storage() {
    let mut folds = FoldedRanges::default();
    folds.extend([(1, 1000), (2000, usize::MAX)]);
    assert_eq!(folds.visible_count(usize::MAX), 1003);
    assert_eq!(folds.line_for_row(2, usize::MAX), 1000);
    assert_eq!(folds.line_for_row(1002, usize::MAX), 2000);
    assert_eq!(folds.line_for_row(1003, usize::MAX), usize::MAX);
    assert_eq!(folds.row_for_line(usize::MAX - 1), 1002);
}

#[test]
fn wheel_scrolling_crosses_folds_in_one_row_and_clamps_to_visible_content() {
    for wrap in [false, true] {
        let mut editor = editor(500);
        editor.word_wrap = wrap;
        editor.folded_ranges.insert(2, 490);
        editor.scroll_down(3);
        assert_eq!(editor.scroll_offset, 490);
        editor.scroll_up(1);
        assert_eq!(editor.scroll_offset, 2);
        editor.scroll_down(usize::MAX);
        assert_eq!(editor.scroll_offset, 492);
        editor.scroll_up(usize::MAX);
        assert_eq!(editor.scroll_offset, 0);
        editor.scroll_past_end = true;
        editor.scroll_down(usize::MAX);
        assert_eq!(editor.scroll_offset, 499);
        editor.scroll_up(1);
        assert_eq!(editor.scroll_offset, 498);
        assert_eq!(editor.cursor_line, 0);
    }
}

#[test]
fn cursor_visibility_centering_and_page_navigation_count_visible_rows() {
    for wrap in [false, true] {
        let mut editor = editor(500);
        editor.word_wrap = wrap;
        editor.folded_ranges.insert(2, 490);
        editor.set_cursor(491, 1);
        assert_eq!(
            editor.scroll_offset, 0,
            "cursor already fits below the fold"
        );
        editor.page_down();
        assert_eq!(editor.cursor_line, 497);
        assert_eq!(editor.scroll_offset, 492);
        editor.page_up();
        assert_eq!(editor.cursor_line, 491);
        assert_eq!(editor.scroll_offset, 0);
        editor.cursor_line = 499;
        editor.ensure_visible();
        assert_eq!(editor.scroll_offset, 492);
        editor.cursor_line = 491;
        editor.center_cursor();
        assert_eq!(editor.scroll_offset, 0);
        editor.typewriter_mode = true;
        editor.cursor_line = 495;
        editor.ensure_visible();
        assert_eq!(editor.scroll_offset, 491);
    }
}

#[test]
fn folding_preserves_visible_source_anchor_and_resolves_hidden_anchor() {
    for wrap in [false, true] {
        let mut editor = editor(500);
        editor.word_wrap = wrap;
        editor.lsp_folding_ranges = vec![(2, 490)];
        editor.cursor_line = 499;
        editor.scroll_offset = 491;
        if wrap {
            editor.set_wrap_scroll_offset(491);
        }
        editor.toggle_fold(2);
        assert_eq!(editor.scroll_offset, 491);
        editor.toggle_fold(2);
        assert_eq!(editor.scroll_offset, 491);
        editor.cursor_line = 350;
        editor.scroll_offset = 350;
        if wrap {
            editor.set_wrap_scroll_offset(350);
        }
        editor.toggle_fold(2);
        assert_eq!(editor.scroll_offset, 2);
        assert_eq!(editor.cursor_line, 2);
    }
}

#[test]
fn pending_wrap_layout_uses_folded_rows_for_positions_and_scrolling() {
    let mut editor = editor(5000);
    editor.word_wrap = true;
    editor.folded_ranges.insert(2, 4990);
    assert!(!editor.ensure_row_offsets(80));
    assert_eq!(editor.wrapped_total_rows(80), 13);
    assert_eq!(editor.wrapped_row_for_position(4991, 0, 80), 4);
    assert_eq!(editor.wrapped_row_at(3, 80).unwrap().line_idx, 4990);
    let rows: Vec<_> = editor
        .wrapped_rows(1, 5, 80)
        .iter()
        .map(|row| row.line_idx)
        .collect();
    assert_eq!(rows, [1, 2, 4990, 4991, 4992]);
    editor.scroll_down(3);
    assert_eq!((editor.scroll_offset, editor.wrap_scroll_offset), (4990, 3));
    let offsets =
        Editor::build_wrap_row_offsets(&editor.rope, 80, 4, &[(2, 4990)], || false).unwrap();
    assert!(editor.install_wrap_layout(80, offsets));
    assert_eq!(editor.wrapped_row_at(3, 80).unwrap().line_idx, 4990);
    assert_eq!(editor.wrapped_total_rows(80), 13);
}

#[test]
fn rendered_editor_advances_immediately_after_a_large_fold() {
    for large_file in [false, true] {
        for wrap in [false, true] {
            let mut app = App::test_new();
            let mut editor = editor(5000);
            editor.large_file_mode = large_file;
            editor.word_wrap = wrap;
            editor.folded_ranges.insert(2, 4990);
            app.tabs = vec![Tab::new_file(editor)];
            app.active_tab = 0;
            app.scroll_past_end = false;
            app.sticky_scroll = false;
            app.show_minimap = true;
            let area = Rect::new(0, 0, 100, 8);
            let mut terminal = Terminal::new(TestBackend::new(100, 8)).unwrap();
            terminal
                .draw(|frame| crate::ui_editor::draw_editor(frame, &mut app, area))
                .unwrap();
            app.editor_mut().unwrap().scroll_down(3);
            for source in [4990, 4991] {
                terminal
                    .draw(|frame| crate::ui_editor::draw_editor(frame, &mut app, area))
                    .unwrap();
                let code = app.code_area.unwrap();
                let first: String = (code.x..code.right())
                    .map(|x| terminal.backend().buffer()[(x, code.y)].symbol())
                    .collect();
                assert!(
                    first.starts_with(&format!("line {source}")),
                    "large_file={large_file}, wrap={wrap}, first={first:?}"
                );
                app.editor_mut().unwrap().scroll_down(1);
            }
        }
    }
}

fn overview(
    editor: &Editor,
    minimap: bool,
    search: &[(usize, usize, usize)],
    diagnostics: &[(usize, u8)],
    changes: &[(usize, char)],
) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(10, 12)).unwrap();
    let colors = crate::app_theme::EditorTheme::Dark.colors();
    let diagnostic_index = DiagnosticLineIndex::from_diagnostics(diagnostics);
    let change_index = ChangeLineIndex::from_changes(changes);
    let change_rows = scrollbar_change_rows_indexed(editor, 12, true, &change_index);
    terminal
        .draw(|frame| {
            if minimap {
                draw_minimap(
                    frame,
                    MinimapParams {
                        area: Rect::new(0, 0, 10, 12),
                        editor,
                        scroll: editor.scroll_offset,
                        visible_lines: editor.viewport_height,
                        search_matches: search,
                        diagnostic_lines: diagnostics,
                        diagnostic_index: Some(&diagnostic_index),
                        change_index: Some(&change_index),
                        diff_changes: Some(changes),
                        colors,
                        hovered: true,
                        active: false,
                    },
                );
            } else {
                draw_editor_scrollbar(
                    frame,
                    ScrollbarParams {
                        area: Rect::new(0, 0, 2, 12),
                        total_lines: editor.overview_total_rows(),
                        visible_lines: editor.viewport_height,
                        scroll: if editor.word_wrap {
                            editor.wrap_scroll_offset
                        } else {
                            editor.visible_scroll_offset()
                        },
                        search_matches: search,
                        diagnostic_lines: diagnostics,
                        diagnostic_index: Some(&diagnostic_index),
                        change_rows: &change_rows,
                        cursor_line: editor.overview_row_for_line(editor.cursor_line),
                        colors,
                        hovered: true,
                        active: false,
                    },
                    editor,
                );
            }
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

#[test]
fn minimap_and_scrollbar_match_a_document_with_hidden_lines_removed() {
    let mut folded = editor(500);
    folded.folded_ranges.extend([(2, 200), (205, 490)]);
    let source_lines = folded.visible_lines(0, 500);
    let text = source_lines
        .iter()
        .map(|&line| {
            folded
                .rope
                .line(line)
                .to_string()
                .trim_end_matches('\n')
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut compact = Editor::test_with_text(&text);
    compact.viewport_height = folded.viewport_height;
    compact.scroll_past_end = false;
    let search = [(3, 0, 1), (490, 0, 1)];
    let diagnostics = [(204, 2), (300, 1), (495, 3)];
    let changes = [(1, 'S'), (100, 'U'), (202, 'N'), (499, 'D')];
    let compact_search: Vec<_> = search
        .iter()
        .map(|&(line, s, e)| (folded.visible_row_for_line(line), s, e))
        .collect();
    let compact_diagnostics: Vec<_> = diagnostics
        .iter()
        .map(|&(line, sev)| (folded.visible_row_for_line(line), sev))
        .collect();
    let compact_changes: Vec<_> = changes
        .iter()
        .map(|&(line, status)| (folded.visible_row_for_line(line), status))
        .collect();
    for row in [0, 1, 3, 8, 18] {
        folded.set_visible_scroll_offset(row);
        compact.set_visible_scroll_offset(row);
        folded.cursor_line = 493;
        compact.cursor_line = folded.visible_row_for_line(493);
        for minimap in [false, true] {
            assert_eq!(
                overview(&folded, minimap, &search, &diagnostics, &changes),
                overview(
                    &compact,
                    minimap,
                    &compact_search,
                    &compact_diagnostics,
                    &compact_changes
                ),
                "minimap={minimap}, scroll={row}"
            );
        }
    }
}

#[test]
fn overview_marker_cache_changes_when_fold_positions_change() {
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(editor(500))];
    app.active_tab = 0;
    let changes = [(250, 'U')];
    app.editor_mut().unwrap().folded_ranges.insert(2, 200);
    let first = crate::ui_editor::cached_overview_change_rows(&mut app, 20, Some(&changes));
    let cached = crate::ui_editor::cached_overview_change_rows(&mut app, 20, Some(&changes));
    assert!(std::sync::Arc::ptr_eq(&first, &cached));
    let editor = app.editor_mut().unwrap();
    editor.folded_ranges.clear();
    editor.folded_ranges.insert(300, 498);
    let next = crate::ui_editor::cached_overview_change_rows(&mut app, 20, Some(&changes));
    assert!(!std::sync::Arc::ptr_eq(&first, &next));
    assert_ne!(first, next);
}

#[test]
fn wrapped_scrollbar_places_folded_markers_on_their_visible_rows() {
    let mut lines = vec!["hidden"; 102];
    lines[0] = "top";
    lines[1] = "abcdefghij";
    lines[99] = "after";
    lines[100] = "abcdefghijklmno";
    lines[101] = "last";
    let mut folded = Editor::test_with_text(&lines.join("\n"));
    folded.word_wrap = true;
    folded.viewport_width = 5;
    folded.viewport_height = 2;
    folded.scroll_past_end = false;
    folded.folded_ranges.insert(1, 99);
    assert_eq!(folded.wrapped_total_rows(5), 8);
    folded.set_wrap_scroll_offset(3);
    folded.cursor_line = 101;
    let mut compact = Editor::test_with_text("top\nabcde\nfghij\nafter\nabcde\nfghij\nklmno\nlast");
    compact.viewport_height = 2;
    compact.scroll_past_end = false;
    compact.set_visible_scroll_offset(3);
    compact.cursor_line = 7;
    assert_eq!(
        overview(&folded, false, &[(99, 0, 1)], &[(100, 1)], &[(50, 'U')]),
        overview(&compact, false, &[(3, 0, 1)], &[(4, 1)], &[(1, 'U')]),
    );
}

#[test]
fn dragging_minimap_and_scrollbar_uses_the_visible_scroll_range() {
    for wrap in [false, true] {
        for past_end in [false, true] {
            let mut app = App::test_new();
            let mut editor = editor(5000);
            editor.word_wrap = wrap;
            editor.scroll_past_end = past_end;
            editor.folded_ranges.insert(100, 4900);
            let last_top = if past_end { 4999 } else { 4992 };
            app.tabs = vec![Tab::new_file(editor)];
            app.active_tab = 0;
            app.code_area = Some(Rect::new(5, 2, 80, 8));
            app.minimap_area = Some(Rect::new(90, 2, 10, 12));
            crate::mouse_selection::handle_editor_area_left_click(
                &mut app,
                95,
                2,
                KeyModifiers::NONE,
            );
            crate::mouse_drag::handle_left_drag(&mut app, 95, 13, 95, 2, KeyModifiers::NONE);
            assert_eq!(app.editor().unwrap().scroll_offset, last_top);
            crate::mouse_drag::handle_left_drag(&mut app, 95, 2, 95, 13, KeyModifiers::NONE);
            assert_eq!(app.editor().unwrap().scroll_offset, 0);
            crate::mouse_drag::handle_left_up(&mut app, 95, 2, KeyModifiers::NONE);
            app.scrollbar_area = Some(Rect::new(89, 2, 1, 12));
            crate::mouse_selection::handle_editor_area_left_click(
                &mut app,
                89,
                13,
                KeyModifiers::NONE,
            );
            assert_eq!(app.editor().unwrap().scroll_offset, last_top);
            crate::mouse_drag::handle_left_drag(&mut app, 89, 2, 89, 13, KeyModifiers::NONE);
            assert_eq!(app.editor().unwrap().scroll_offset, 0);
            assert_eq!(app.editor().unwrap().cursor_line, 0);
        }
    }
}
