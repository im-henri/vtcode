use crate::{app::{ActivePane, App, Tab, WhitespaceMode}, editor::Editor};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect};

fn fixture(text: &str, tab_size: usize, wrapped: bool) -> App {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text(text);
    editor.tab_size = tab_size;
    editor.word_wrap = wrapped;
    editor.whitespace_mode = WhitespaceMode::None;
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.show_minimap = false;
    app.show_breadcrumbs = false;
    app.sticky_scroll = false;
    app.animation_settings = Default::default();
    app
}

fn draw(app: &mut App, width: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, 14)).unwrap();
    terminal.draw(|frame| crate::ui_editor::draw_editor(frame, app, Rect::new(0, 0, width, 14))).unwrap();
    terminal.backend().buffer().clone()
}

fn cell(buffer: &Buffer, area: Rect, needle: &str) -> (u16, u16) {
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if needle.chars().enumerate().all(|(i, ch)| {
                x as usize + i < area.right() as usize
                    && buffer[(x + i as u16, y)].symbol() == ch.to_string()
            }) { return (x, y); }
        }
    }
    panic!("{needle} was not rendered in {area:?}");
}

fn mouse(app: &mut App, at: (u16, u16), kind: MouseEventKind, modifiers: KeyModifiers) {
    crate::mouse::handle_mouse(app, MouseEvent { kind, column: at.0, row: at.1, modifiers });
}

#[test]
fn tabs_and_spaces_map_cells_to_source_before_paint_and_after_cache_changes() {
    let mut app = fixture(" \t\tX \tY\r\n", 4, false);
    let expected = [0, 1, 1, 1, 2, 2, 2, 2, 3, 4, 5, 5, 6, 7, 7];
    for painted in [false, true] {
        if painted { draw(&mut app, 80); }
        let editor = app.editor().unwrap();
        for (x, &column) in expected.iter().enumerate() {
            assert_eq!(editor.source_column_at_cell(0, 0, usize::MAX, x), (column, x < 13));
        }
        for (x, column) in [2, 2, 2, 2, 3, 4, 5, 5, 6, 7].into_iter().enumerate() {
            assert_eq!(editor.source_column_at_cell(0, 2, 7, x).0, column);
        }
    }
    app.editor_mut().unwrap().tab_size = 2;
    for (x, column) in [0, 1, 2, 2, 3, 4, 5, 5, 6, 7].into_iter().enumerate() {
        assert_eq!(app.editor().unwrap().source_column_at_cell(0, 0, 7, x).0, column);
    }
    // Text edits invalidate the painted glyph cache, including equal-length edits.
    let editor = app.editor_mut().unwrap();
    editor.rope = ropey::Rope::from_str("       Z\n");
    assert_eq!(editor.source_column_at_cell(0, 0, 8, 7), (7, true));
}

#[test]
fn clicks_double_clicks_and_alt_clicks_hit_the_painted_word_after_indentation() {
    for prefix in ["\t", " \t", "\t \t", "  \t\t ", "\t\t\t", "界\t", "e\u{301}\t"] {
        for tab_size in [2, 4, 8] {
            for pan in [0, 1] {
                for large in [false, true] {
                    let mut app = fixture(&format!("{prefix}TARGET tail\r\n"), tab_size, false);
                    app.editor_mut().unwrap().horizontal_scroll = pan;
                    app.editor_mut().unwrap().large_file_mode = large;
                    let buffer = draw(&mut app, 80);
                    let at = cell(&buffer, app.code_area.unwrap(), "TARGET");
                    let column = prefix.chars().count();
                    assert_eq!(crate::app_lsp_links::editor_position_at(&mut app, at.0, at.1), Some((0, column)));
                    mouse(&mut app, at, MouseEventKind::Down(MouseButton::Left), KeyModifiers::NONE);
                    mouse(&mut app, at, MouseEventKind::Up(MouseButton::Left), KeyModifiers::NONE);
                    assert_eq!(app.editor().unwrap().horizontal_scroll, pan, "click moved the text under the pointer");
                    assert_eq!((app.editor().unwrap().cursor_line, app.editor().unwrap().cursor_col),
                        (0, column), "prefix={prefix:?}, tabs={tab_size}, pan={pan}, large={large}");
                    mouse(&mut app, at, MouseEventKind::Down(MouseButton::Left), KeyModifiers::NONE);
                    mouse(&mut app, at, MouseEventKind::Up(MouseButton::Left), KeyModifiers::NONE);
                    assert_eq!(app.editor().unwrap().selected_text().as_deref(), Some("TARGET"));
                    let editor = app.editor_mut().unwrap();
                    editor.clear_selection();
                    editor.cursor_col = 0;
                    mouse(&mut app, at, MouseEventKind::Down(MouseButton::Left), KeyModifiers::ALT);
                    assert_eq!(app.editor().unwrap().extra_cursors, [(0, column)]);
                }
            }
        }
    }
}

#[test]
fn drag_and_shift_selection_use_the_same_tab_mapping_as_clicks() {
    for wrapped in [false, true] {
        let mut app = fixture("\t  TARGET tail\n", 4, wrapped);
        let buffer = draw(&mut app, 80);
        let at = cell(&buffer, app.code_area.unwrap(), "TARGET");
        let end = (at.0 + 6, at.1);
        for shifted in [false, true] {
            app.last_click_time = std::time::Instant::now() - std::time::Duration::from_secs(1);
            mouse(&mut app, at, MouseEventKind::Down(MouseButton::Left), KeyModifiers::NONE);
            if shifted {
                mouse(&mut app, at, MouseEventKind::Up(MouseButton::Left), KeyModifiers::NONE);
                mouse(&mut app, end, MouseEventKind::Down(MouseButton::Left), KeyModifiers::SHIFT);
            } else {
                mouse(&mut app, end, MouseEventKind::Drag(MouseButton::Left), KeyModifiers::NONE);
            }
            mouse(&mut app, end, MouseEventKind::Up(MouseButton::Left), KeyModifiers::NONE);
            assert_eq!(app.editor().unwrap().selected_text().as_deref(), Some("TARGET"));
        }
    }
}

#[test]
fn rectangular_selection_matches_visual_columns_with_mixed_indentation() {
    for wrapped in [false, true] {
        for middle in [false, true] {
            let mut app = fixture("\t\tabcdef\n  \t    abcdef\n        abcdef\n", 4, wrapped);
            let buffer = draw(&mut app, 80);
            let at = cell(&buffer, app.code_area.unwrap(), "abcdef");
            let (button, modifiers) = if middle { (MouseButton::Middle, KeyModifiers::NONE) }
                else { (MouseButton::Left, KeyModifiers::ALT | KeyModifiers::SHIFT) };
            mouse(&mut app, at, MouseEventKind::Down(button), modifiers);
            for last_row in [1, 2, 1, 2] {
                mouse(&mut app, (at.0 + 3, at.1 + last_row), MouseEventKind::Drag(button), modifiers);
                let editor = app.editor().unwrap();
                let positions: Vec<_> = std::iter::once(((editor.cursor_line, editor.cursor_col), editor.selection_anchor))
                    .chain(editor.extra_cursors.iter().copied().zip(editor.extra_cursor_anchors.iter().copied()))
                    .collect();
                assert_eq!(positions.len(), usize::from(last_row) + 1);
                for ((line, column), anchor) in positions {
                    let prefix = [2, 7, 8][line];
                    assert_eq!((column, anchor), (prefix, Some((line, prefix + 3))));
                }
            }
        }
    }
}

#[test]
fn wrapped_continuation_and_folded_rows_hit_the_rendered_source() {
    for text in ["prefix prefix prefix prefix \tTARGET tail\n", "first\n hidden\n hidden\n \tTARGET\n"] {
        let mut app = fixture(text, 4, true);
        if text.starts_with("first") { app.editor_mut().unwrap().folded_ranges.insert(0, 3); }
        let buffer = draw(&mut app, 38);
        let at = cell(&buffer, app.code_area.unwrap(), "TARGET");
        assert!(at.1 > app.code_area.unwrap().y);
        let line = if text.starts_with("first") { 3 } else { 0 };
        let column = app.editor().unwrap().rope.line(line).chars().position(|ch| ch == 'T').unwrap();
        mouse(&mut app, at, MouseEventKind::Down(MouseButton::Left), KeyModifiers::NONE);
        assert_eq!((app.editor().unwrap().cursor_line, app.editor().unwrap().cursor_col), (line, column));
    }
}

#[test]
fn changing_tab_size_during_column_drag_reprojects_every_selected_line() {
    let mut app = fixture("\t\tabcdef\n\t\tabcdef\n\t\tabcdef\n", 4, false);
    draw(&mut app, 80);
    let area = app.code_area.unwrap();
    mouse(&mut app, (area.x, area.y), MouseEventKind::Down(MouseButton::Middle), KeyModifiers::NONE);
    mouse(&mut app, (area.x + 4, area.y + 1), MouseEventKind::Drag(MouseButton::Middle), KeyModifiers::NONE);
    assert_eq!(app.editor().unwrap().selection_anchor, Some((0, 1)));
    app.editor_mut().unwrap().tab_size = 2;
    mouse(&mut app, (area.x + 4, area.y + 2), MouseEventKind::Drag(MouseButton::Middle), KeyModifiers::NONE);
    let editor = app.editor().unwrap();
    assert_eq!((editor.cursor_line, editor.cursor_col, editor.selection_anchor), (0, 0, Some((0, 2))));
    assert_eq!(editor.extra_cursors, [(1, 0), (2, 0)]);
    assert_eq!(editor.extra_cursor_anchors, [Some((1, 2)), Some((2, 2))]);
}

#[test]
fn double_click_inside_a_tab_selects_the_mixed_whitespace_run() {
    let mut app = fixture("\t  \tTARGET\n", 4, false);
    draw(&mut app, 80);
    let area = app.code_area.unwrap();
    for _ in 0..2 {
        mouse(&mut app, (area.x + 2, area.y), MouseEventKind::Down(MouseButton::Left), KeyModifiers::NONE);
        mouse(&mut app, (area.x + 2, area.y), MouseEventKind::Up(MouseButton::Left), KeyModifiers::NONE);
    }
    assert_eq!(app.editor().unwrap().selected_text().as_deref(), Some("\t  \t"));
}

#[test]
fn distant_plain_text_hit_does_not_need_a_decoded_prefix() {
    let editor = Editor::test_with_text(&"a".repeat(2_000_000));
    assert_eq!(editor.source_column_at_cell(0, 1_900_000, 2_000_000, 17), (1_900_017, true));
    assert!(editor.render_line_cache.borrow().pointer_glyphs(0,
        (editor.content_version, editor.wrap_revision, editor.tab_size), &editor.rope).is_none());
}
