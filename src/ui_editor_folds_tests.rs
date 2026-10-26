use super::*;
use crate::{app::Tab, editor::Editor};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn fixture(header: &str, wrap: bool) -> App {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text(&format!("{header}\n    one();\n    two();\n    three();\n}}\nnext();\n"));
    editor.word_wrap = wrap;
    editor.folded_ranges.insert(0, 4);
    editor.cursor_line = 5;
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.show_minimap = false;
    app.sticky_scroll = false;
    app.show_color_preview = false;
    app.error_lens = false;
    app
}

fn draw(app: &mut App, width: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, 15)).unwrap();
    terminal.draw(|frame| crate::ui_editor::draw_editor(frame, app, Rect::new(0, 0, width, 15))).unwrap();
    terminal.backend().buffer().clone()
}

fn text(buffer: &Buffer, area: Rect) -> String {
    (area.x..area.right()).map(|x| buffer[(x, area.y)].symbol()).collect()
}

fn mouse(app: &mut App, kind: MouseEventKind, col: u16, row: u16) {
    crate::mouse::handle_mouse(app, MouseEvent { kind, column: col, row, modifiers: KeyModifiers::NONE });
}

#[test]
fn folded_badge_is_clear_clickable_and_preserves_editing_state_in_both_layouts() {
    for wrap in [false, true] {
        let mut app = fixture("fn example() {", wrap);
        let editor = app.editor_mut().unwrap();
        editor.selection_anchor = Some((5, 1));
        editor.cursor_col = 4;
        let source = editor.rope.clone();
        let buffer = draw(&mut app, 100);
        let (button, line, end) = app.editor().unwrap().fold_buttons.hits[0];
        assert_eq!((line, end), (0, 4));
        assert_eq!(text(&buffer, button), " + 3 lines folded · Expand ");
        assert!(buffer[(button.x, button.y)].modifier.contains(Modifier::BOLD));
        let gutter = app.gutter_area.unwrap();
        assert_eq!(buffer[(gutter.right() - 1, gutter.y)].symbol(), "+");
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), button.x + 3, button.y);
        let editor = app.editor().unwrap();
        assert!(editor.folded_ranges.is_empty());
        assert_eq!(editor.rope, source);
        assert_eq!((editor.cursor_line, editor.cursor_col), (5, 4));
        assert_eq!(editor.selection_anchor, Some((5, 1)));
        assert!(!editor.can_undo());
        assert!(!app.dragging_text);
        let expanded = draw(&mut app, 100);
        assert!(app.editor().unwrap().fold_buttons.hits.is_empty());
        assert_eq!(expanded[(gutter.right() - 1, gutter.y)].symbol(), "▾");
    }
}

#[test]
fn folded_badges_remain_visible_with_long_headers_narrow_panes_and_horizontal_scroll() {
    for wrap in [false, true] {
        for width in [100, 45, 25, 15] {
            for pan in [0, 20, 300] {
                let mut app = fixture(&format!("\tfn 界_{}() {{", "argument_".repeat(30)), wrap);
                app.editor_mut().unwrap().horizontal_scroll = pan;
                let buffer = draw(&mut app, width);
                let editor = app.editor().unwrap();
                assert_eq!(editor.fold_buttons.hits.len(), 1, "wrap={wrap}, width={width}, pan={pan}");
                let button = editor.fold_buttons.hits[0].0;
                assert_eq!(button.intersection(app.code_area.unwrap()), button);
                assert!(text(&buffer, button).contains('+'));
                assert!(click(&mut app, button.x, button.y));
                assert!(app.editor().unwrap().folded_ranges.is_empty());
            }
        }
    }
}

#[test]
fn folded_badge_never_covers_the_primary_caret_on_a_long_header() {
    for col in [0, 10, 20, 30, 35, 40] {
        let mut app = fixture(&"a".repeat(100), false);
        let editor = app.editor_mut().unwrap();
        editor.cursor_line = 0;
        editor.cursor_col = col;
        editor.extra_cursors = vec![(0, 34)];
        let _ = draw(&mut app, 55);
        let code = app.code_area.unwrap();
        assert!(!app.editor().unwrap().fold_buttons.hits.is_empty());
        for (button, _, _) in &app.editor().unwrap().fold_buttons.hits {
            assert!(!button.contains((code.x + col as u16, code.y).into()), "col={col}");
            assert!(!button.contains((code.x + 34, code.y).into()), "secondary caret");
        }
    }
}

#[test]
fn folded_badge_hover_redraws_without_requesting_lsp_hover_behind_it() {
    let mut app = fixture("fn example() {", false);
    let buffer = draw(&mut app, 100);
    let button = app.editor().unwrap().fold_buttons.hits[0].0;
    mouse(&mut app, MouseEventKind::Moved, button.x + 2, button.y);
    assert!(app.mouse_hover_changed);
    assert!(app.hovered_editor_pos.is_none());
    let hovered = draw(&mut app, 100);
    assert_ne!(buffer[(button.x, button.y)].bg, hovered[(button.x, button.y)].bg);
    mouse(&mut app, MouseEventKind::Moved, button.right(), button.y);
    assert!(app.mouse_hover_changed);
}

#[test]
fn stale_fold_buttons_cannot_expand_a_different_view_or_recollapse_a_region() {
    for operation in 0..5 {
        let mut app = fixture("fn example() {", false);
        draw(&mut app, 100);
        let button = app.editor().unwrap().fold_buttons.hits[0].0;
        let editor = app.editor_mut().unwrap();
        match operation {
            0 => editor.scroll_offset += 1,
            1 => editor.horizontal_scroll += 1,
            2 => editor.insert_char('x'),
            3 => editor.toggle_fold(0),
            _ => { editor.unfold_all(); editor.folded_ranges.insert(0, 4); }
        }
        assert!(!click(&mut app, button.x, button.y), "operation={operation}");
    }
}

#[test]
fn folded_badge_counts_only_hidden_lines_and_handles_eof_and_singular() {
    for end in [2, 4, 7] {
        let mut app = fixture("fn example() {", false);
        app.editor_mut().unwrap().folded_ranges.insert(0, end);
        let buffer = draw(&mut app, 100);
        let button = app.editor().unwrap().fold_buttons.hits[0].0;
        assert_eq!(text(&buffer, button), format!(" + {} {} folded · Expand ", end - 1, if end == 2 { "line" } else { "lines" }));
    }
}

#[test]
fn folded_badges_survive_animation_only_redraws() {
    let mut app = fixture("fn example() {", false);
    app.animation_settings.set(crate::animation_config::AnimationEffect::CursorSmear, true);
    app.animation_frame_cache.begin_frame(false, true);
    let original = draw(&mut app, 100);
    let button = app.editor().unwrap().fold_buttons.hits[0].0;
    app.rendering_animation_frame = true;
    app.animation_frame_cache.begin_frame(true, true);
    let replay = draw(&mut app, 100);
    assert!(app.animation_frame_cache.replays > 0);
    assert_eq!(text(&original, button), text(&replay, button));
    assert!(click(&mut app, button.x, button.y));
}

#[test]
fn folded_badge_click_in_another_split_expands_only_that_pane() {
    let mut app = fixture("fn example() {", false);
    app.duplicate_tab(0);
    app.create_split_group(1);
    app.editor_mut().unwrap().folded_ranges.insert(0, 4);
    let first = app.active_group;
    let second = 1 - first;
    draw(&mut app, 100);
    app.switch_group(second);
    draw(&mut app, 100);
    let button = app.editor().unwrap().fold_buttons.hits[0].0;
    app.editor_area = Some(Rect::new(0, 0, 100, 15));
    app.switch_group(first);
    app.editor_area = Some(Rect::new(100, 0, 100, 15));
    crate::mouse_selection::switch_focus_if_split(&mut app, button.x, button.y, true);
    assert_eq!(app.active_group, second);
    assert!(click(&mut app, button.x, button.y));
    assert!(app.editor().unwrap().folded_ranges.is_empty());
    app.switch_group(first);
    assert_eq!(app.editor().unwrap().folded_ranges.get(&0), Some(&4));
}

#[test]
fn folded_headers_use_the_theme_and_keep_summary_text_readable() {
    use crate::app_theme::EditorTheme;
    for (name, theme, width, header) in [
        ("dark", EditorTheme::Dark, 100, "fn process_measurements() {".to_string()),
        ("light", EditorTheme::Light, 100, "fn process_measurements() {".to_string()),
        ("narrow", EditorTheme::Dark, 45, format!("fn process_measurements({}) {{", "argument, ".repeat(15))),
    ] {
        let mut app = fixture(&header, false);
        app.colors = theme.colors();
        let buffer = draw(&mut app, width);
        let code = app.code_area.unwrap();
        assert_eq!(buffer[(code.x, code.y)].bg, app.colors.bg_fold_indicator);
        let button = app.editor().unwrap().fold_buttons.hits[0].0;
        assert_ne!(buffer[(button.x, button.y)].fg, buffer[(button.x, button.y)].bg);
        if std::env::var_os("VTCODE_FOLD_PREVIEW").is_some() {
            let rgb = |color| match color { ratatui::style::Color::Rgb(r,g,b) => [r,g,b], _ => [128,128,128] };
            let cells: Vec<_> = buffer.content.iter().map(|cell| serde_json::json!({
                "text": cell.symbol(), "fg": rgb(cell.fg), "bg": rgb(cell.bg),
                "bold": cell.modifier.contains(Modifier::BOLD),
            })).collect();
            std::fs::write(format!("target/folding-preview-{name}.json"), serde_json::to_vec(&serde_json::json!({
                "width": width, "height": 8, "cells": cells,
            })).unwrap()).unwrap();
        }
    }
}

#[test]
fn sticky_headers_occlude_fold_buttons_instead_of_leaving_invisible_click_targets() {
    let mut app = fixture("fn parent() {", false);
    let editor = app.editor_mut().unwrap();
    editor.folded_ranges.clear();
    editor.folded_ranges.insert(1, 4);
    editor.scroll_offset = 1;
    app.sticky_scroll = true;
    app.cached_sticky_scopes = vec![crate::background::StickyScopeLine { line: 0, parent: None }].into();
    draw(&mut app, 100);
    assert!(app.editor().unwrap().fold_buttons.hits.is_empty());
    app.sticky_scroll = false;
    draw(&mut app, 100);
    assert_eq!(app.editor().unwrap().fold_buttons.hits.len(), 1);
}
