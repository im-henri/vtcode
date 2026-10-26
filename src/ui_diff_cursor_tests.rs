use super::*;

use crate::app::{ActivePane, DiffMode, Tab, TabKind};
use crate::editor::Editor;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use std::sync::Arc;

const CURSOR_BACKGROUND: Color = Color::Rgb(251, 17, 139);

fn comparison(base: &str, target: &str, mode: DiffMode, compact: bool) -> App {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text(target);
    editor.file_path = Some("readonly-cursor.txt".into());
    editor.read_only = true;
    editor.word_wrap = false;
    let mut tab = Tab::new_file(editor);
    tab.kind = TabKind::Diff(41);
    app.tabs = vec![tab];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.diff_tabs.owner = Some(41);
    app.diff_mode = mode;
    app.diff_target_path = app.editor().unwrap().file_path.clone();
    app.compare_base_content = Some(base.to_string().into());
    app.set_compare_diff_lines(crate::app::compute_file_diff(base, target));
    if compact {
        app.diff_render_cache = Arc::new(app.diff_render_cache.with_context(2));
    }
    app.show_minimap = false;
    app.sticky_scroll = false;
    app.show_indent_guides = false;
    app.show_color_preview = false;
    app.rulers.clear();
    app.cursor_visible = true;
    app.colors.bg_cursor = CURSOR_BACKGROUND;
    app
}

fn render(app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let mut rendered = None;
    terminal
        .draw(|frame| {
            let area = frame.area();
            app.editor_area = Some(area);
            match app.diff_mode {
                DiffMode::SideBySide => draw_side_by_side_diff(frame, app, area),
                DiffMode::Inline => draw_inline_diff(frame, app, area),
                DiffMode::Off => panic!("fixture requires a diff mode"),
            }
            // The terminal backend skips continuation cells of wide glyphs;
            // inspect the painted frame to verify the entire caret surface.
            rendered = Some(frame.buffer_mut().clone());
        })
        .unwrap();
    rendered.unwrap()
}

fn target_origin(app: &App, source_line: usize) -> (u16, u16) {
    let code = app.code_area.unwrap();
    let (row, first, inset) = match app.diff_mode {
        DiffMode::SideBySide => (
            app.diff_render_cache
                .rows
                .iter()
                .position(|row| row.hidden_lines == 0 && row.right_line == Some(source_line))
                .expect("target source row is visible in comparison"),
            app.aligned_diff_scroll_row(app.editor().unwrap().scroll_offset),
            2,
        ),
        DiffMode::Inline => (
            app.diff_render_cache
                .inline_rows
                .iter()
                .position(|inline| {
                    let row = &app.diff_render_cache.rows[inline.aligned_row];
                    inline.kind != '-'
                        && row.hidden_lines == 0
                        && row.right_line == Some(source_line)
                })
                .expect("target source row is visible in comparison"),
            app.diff_inline_scroll.row,
            0,
        ),
        DiffMode::Off => unreachable!(),
    };
    assert!(row >= first && row - first < code.height as usize);
    (code.x + inset, code.y + (row - first) as u16)
}

#[test]
fn find_centers_matches_in_rendered_diff_rows() {
    let base: String = (0..100).map(|line| format!("line_{line}\n")).collect();
    let target: String = (0..100)
        .filter(|line| !(35..55).contains(line))
        .map(|line| if line == 55 || line == 70 { "needle\n".into() } else { format!("line_{line}\n") })
        .collect();
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        let mut app = comparison(&base, &target, mode, false);
        render(&mut app, 120, 20);
        app.search_query = "needle".into();
        app.execute_palette_command("find");
        for line in [35, 50, 35] {
            render(&mut app, 120, 20);
            assert_eq!(app.editor().unwrap().cursor_line, line);
            let code = app.code_area.unwrap();
            assert_eq!(target_origin(&app, line).1, code.y + code.height / 2,
                "inline={}, source line={line}", mode == DiffMode::Inline);
            app.search_next();
        }
    }
}

fn caret_cells(app: &App, buffer: &Buffer) -> Vec<(u16, u16)> {
    let code = app.code_area.unwrap();
    buffer
        .content
        .iter()
        .enumerate()
        .filter_map(|(index, cell)| {
            let x = buffer.area.x + (index % buffer.area.width as usize) as u16;
            let y = buffer.area.y + (index / buffer.area.width as usize) as u16;
            // Overview cursor markers share the caret color, but are outside
            // the text surface whose cursor placement this test verifies.
            (cell.bg == CURSOR_BACKGROUND
                && x >= code.x
                && x < code.right()
                && y >= code.y
                && y < code.bottom())
            .then_some((x, y))
        })
        .collect()
}

#[test]
fn compact_diff_navigation_preserves_screen_column_before_native_selection_is_active() {
    let base = "abcdef\n界界ab\nold\nlast";
    let target = "abcdef\n界界ab\nnew\nlast";
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        let mut app = comparison(base, target, mode, true);
        app.editor_mut().unwrap().set_cursor(0, 5);
        render(&mut app, 120, 20);
        assert!(app.context_diff_navigation(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!((app.editor().unwrap().cursor_line, app.editor().unwrap().cursor_col), (1, 3));
        assert!(app.context_diff_navigation(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!((app.editor().unwrap().cursor_line, app.editor().unwrap().cursor_col), (0, 5));
    }
}

#[test]
fn readonly_diff_caret_survives_aligned_and_inline_repainting_at_text_and_eol() {
    let base = "first\nremoved_A\nremoved_B\nold\n\nlast\n";
    let target = "first\nnew\n\nlast\n";
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        let view = if mode == DiffMode::SideBySide {
            "side-by-side"
        } else {
            "inline"
        };
        for compact in [false, true] {
            for (line, column, symbol) in [
                (1, 1, "e"),
                (1, 3, " "),
                (2, 0, " "),
                (3, 2, "s"),
                (3, 4, " "),
            ] {
                let mut app = comparison(base, target, mode, compact);
                app.editor_mut().unwrap().set_cursor(line, column);
                let buffer = render(&mut app, 120, 20);
                let (x, y) = target_origin(&app, line);
                let position = (x + column as u16, y);
                assert_eq!(
                    buffer[position].symbol(),
                    symbol,
                    "mode={view}, compact={compact}, source={line}:{column}"
                );
                assert_eq!(
                    caret_cells(&app, &buffer),
                    vec![position],
                    "one target caret after final diff painting: {view}, compact={compact}"
                );
                assert!(app.editor().unwrap().read_only);
                assert_eq!(app.editor().unwrap().rope.to_string(), target);
                if line == 3 {
                    assert!(
                        y > app.code_area.unwrap().y + line as u16,
                        "deleted source rows must exercise actual alignment gaps"
                    );
                }
            }
        }
    }
}

#[test]
fn compact_diff_caret_uses_source_rows_after_omitted_context() {
    let prefix: String = (0..12).map(|line| format!("prefix_{line}\n")).collect();
    let suffix: String = (0..12).map(|line| format!("suffix_{line}\n")).collect();
    let base = format!("{prefix}old\nremoved_A\nremoved_B\nanchor\n{suffix}");
    let target = format!("{prefix}new\nanchor\n{suffix}");
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        let mut app = comparison(&base, &target, mode, true);
        assert!(
            app.diff_render_cache
                .rows
                .iter()
                .any(|row| row.hidden_lines > 0)
        );
        app.editor_mut().unwrap().set_cursor(13, 2);
        let buffer = render(&mut app, 120, 18);
        let (x, y) = target_origin(&app, 13);
        assert_eq!(buffer[(x + 2, y)].symbol(), "c");
        assert_eq!(caret_cells(&app, &buffer), vec![(x + 2, y)]);
        assert_eq!(app.editor().unwrap().rope.to_string(), target);
    }
}

#[test]
fn diff_caret_projects_wide_combining_and_horizontally_scrolled_text() {
    let target = format!("A界e\u{301}Z{}\n", " trailing".repeat(30));
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        let view = if mode == DiffMode::SideBySide {
            "side-by-side"
        } else {
            "inline"
        };
        for compact in [false, true] {
            for (column, pan, cell_column, symbol, cells) in [
                (1, 0, 1, "界", 2),
                (4, 0, 4, "Z", 1),
                (2, 0, 3, "e\u{301}", 1),
                (3, 0, 3, "e\u{301}", 1),
                (4, 1, 3, "Z", 1),
                (4, 2, if mode == DiffMode::Inline { 2 } else { 1 }, "Z", 1),
            ] {
                let mut app = comparison("old\n", &target, mode, compact);
                let editor = app.editor_mut().unwrap();
                editor.set_cursor(0, column);
                editor.horizontal_scroll = pan;
                let buffer = render(&mut app, 76, 8);
                let (x, y) = target_origin(&app, 0);
                let position = (x + cell_column, y);
                assert_eq!(
                    buffer[position].symbol(),
                    symbol,
                    "{view}, compact={compact}, source_column={column}, pan={pan}"
                );
                assert_eq!(
                    caret_cells(&app, &buffer),
                    (0..cells)
                        .map(|offset| (position.0 + offset, position.1))
                        .collect::<Vec<_>>(),
                    "caret follows grapheme cells: {view}, compact={compact}, pan={pan}"
                );
                assert_eq!(app.editor().unwrap().rope.to_string(), target);
            }
        }
    }
}

#[test]
fn diff_caret_stays_hidden_outside_horizontal_viewport_and_past_short_eol() {
    let target = format!("short\n{}\n\n", "long text ".repeat(30));
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        let view = if mode == DiffMode::SideBySide {
            "side-by-side"
        } else {
            "inline"
        };
        for compact in [false, true] {
            for (line, column, pan) in [(0, 0, 2), (1, 200, 0), (0, 5, 40), (2, 0, 40)] {
                let mut app = comparison("old\n", &target, mode, compact);
                let editor = app.editor_mut().unwrap();
                editor.set_cursor(line, column);
                editor.horizontal_scroll = pan;
                let buffer = render(&mut app, 76, 12);
                target_origin(&app, line);
                assert!(
                    caret_cells(&app, &buffer).is_empty(),
                    "offscreen caret leaked: {view}, compact={compact}, source={line}:{column}, pan={pan}"
                );
                assert_eq!(app.editor().unwrap().rope.to_string(), target);
            }
        }
    }
}

#[test]
fn diff_caret_hides_during_blink_inactive_rendering_and_status_details() {
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        let view = if mode == DiffMode::SideBySide {
            "side-by-side"
        } else {
            "inline"
        };
        for compact in [false, true] {
            for hidden in 0..3 {
                let mut app = comparison("old\n", "new\n", mode, compact);
                app.editor_mut().unwrap().set_cursor(0, 1);
                match hidden {
                    0 => app.cursor_visible = false,
                    1 => app.rendering_inactive_editor = true,
                    2 => app.status_details.open = true,
                    _ => unreachable!(),
                }
                let buffer = render(&mut app, 100, 8);
                assert!(
                    caret_cells(&app, &buffer).is_empty(),
                    "hidden caret leaked: {view}, compact={compact}, reason={hidden}"
                );
            }
        }
    }
}

#[test]
fn readonly_diff_keys_move_and_select_while_editing_keys_preserve_source() {
    let base = "first\nremoved_A\nremoved_B\nold\n\nlast\n";
    let target = "first\nnew\n\nlast\n";
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        for compact in [false, true] {
            let mut app = comparison(base, target, mode, compact);
            app.editor_mut().unwrap().set_cursor(1, 0);
            render(&mut app, 120, 20);
            crate::input::handle_editor_key(&mut app, KeyCode::Right, KeyModifiers::NONE);
            assert_eq!(
                (
                    app.editor().unwrap().cursor_line,
                    app.editor().unwrap().cursor_col
                ),
                (1, 1)
            );
            crate::input::handle_editor_key(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
            assert_eq!(app.editor().unwrap().selection_anchor, Some((1, 1)));
            assert_eq!(app.editor().unwrap().cursor_col, 2);
            crate::input::handle_editor_key(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
            assert_eq!(
                (
                    app.editor().unwrap().cursor_line,
                    app.editor().unwrap().cursor_col
                ),
                (2, 0)
            );
            assert_eq!(app.editor().unwrap().selection_anchor, Some((1, 1)));
            let buffer = render(&mut app, 120, 20);
            let position = target_origin(&app, 2);
            assert_eq!(caret_cells(&app, &buffer), vec![position]);

            let version = app.editor().unwrap().content_version;
            for key in [
                KeyCode::Char('X'),
                KeyCode::Enter,
                KeyCode::Backspace,
                KeyCode::Delete,
                KeyCode::Tab,
                KeyCode::BackTab,
            ] {
                crate::input::handle_editor_key(&mut app, key, KeyModifiers::NONE);
                assert_eq!(
                    app.editor().unwrap().rope.to_string(),
                    target,
                    "rejected {key:?}"
                );
                assert_eq!(app.editor().unwrap().content_version, version);
                assert!(app.editor().unwrap().read_only);
            }
            crate::input::handle_editor_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
            assert_eq!(app.editor().unwrap().cursor_line, 3);
            let buffer = render(&mut app, 120, 20);
            let (x, y) = target_origin(&app, 3);
            assert_eq!(
                caret_cells(&app, &buffer),
                vec![(x + app.editor().unwrap().cursor_col as u16, y)]
            );
            assert_eq!(app.editor().unwrap().rope.to_string(), target);
        }
    }
}

#[test]
fn full_diff_navigation_reveals_trailing_empty_logical_line() {
    let target: String = (0..50)
        .map(|line| format!("source_line_{line}\n"))
        .collect();
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        for (key, modifiers) in [
            (KeyCode::End, KeyModifiers::CONTROL),
            (KeyCode::Down, KeyModifiers::NONE),
        ] {
            let mut app = comparison(&target, &target, mode, false);
            if key == KeyCode::Down {
                app.editor_mut().unwrap().set_cursor(49, 0);
            }
            render(&mut app, 120, 8);
            let version = app.editor().unwrap().content_version;
            crate::input::handle_editor_key(&mut app, key, modifiers);
            let buffer = render(&mut app, 120, 8);
            let editor = app.editor().unwrap();
            assert_eq!((editor.cursor_line, editor.cursor_col), (50, 0));
            assert!(editor.scroll_offset > 0, "navigation must reveal EOF");
            let code = app.code_area.unwrap();
            let (terminal_row, first, inset) = match mode {
                DiffMode::SideBySide => (
                    app.diff_render_cache.rows.len(),
                    app.aligned_diff_scroll_row(editor.scroll_offset),
                    2,
                ),
                DiffMode::Inline => (
                    app.diff_render_cache.inline_rows.len(),
                    app.diff_inline_scroll.row,
                    0,
                ),
                DiffMode::Off => unreachable!(),
            };
            assert!(terminal_row >= first);
            assert!(terminal_row - first < code.height as usize);
            let position = (code.x + inset, code.y + (terminal_row - first) as u16);
            assert_eq!(buffer[position].symbol(), " ");
            assert_eq!(
                caret_cells(&app, &buffer),
                vec![position],
                "navigation={key:?}"
            );
            assert_eq!(editor.rope.to_string(), target);
            assert_eq!(editor.content_version, version);
            assert!(editor.read_only);
        }
    }
}

#[test]
fn full_diff_empty_target_has_a_blank_caret_after_deleted_rows() {
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        for base in ["", "removed_A\nremoved_B\n"] {
            let mut app = comparison(base, "", mode, false);
            let version = app.editor().unwrap().content_version;
            let buffer = render(&mut app, 120, 12);
            let editor = app.editor().unwrap();
            assert_eq!((editor.cursor_line, editor.cursor_col), (0, 0));
            let code = app.code_area.unwrap();
            let (terminal_row, first, inset) = match mode {
                DiffMode::SideBySide => (
                    app.diff_render_cache.rows.len(),
                    app.aligned_diff_scroll_row(editor.scroll_offset),
                    if base.is_empty() { 0 } else { 2 },
                ),
                DiffMode::Inline => (
                    app.diff_render_cache.inline_rows.len(),
                    app.diff_inline_scroll.row,
                    0,
                ),
                DiffMode::Off => unreachable!(),
            };
            assert!(terminal_row >= first);
            assert!(terminal_row - first < code.height as usize);
            let position = (code.x + inset, code.y + (terminal_row - first) as u16);
            assert_eq!(buffer[position].symbol(), " ");
            assert_eq!(caret_cells(&app, &buffer), vec![position], "base={base:?}");
            assert_eq!(editor.rope.to_string(), "");
            assert_eq!(editor.content_version, version);
            assert!(editor.read_only);
        }
    }
}
