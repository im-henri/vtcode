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

const CONDITIONAL: &str = "#ifdef FEATURE\n    one();\n    two();\n#elif OTHER\n    other();\n    more();\n#else\n    fallback();\n    last();\n#endif\ntail();\n";

fn conditional_fixture(wrap: bool) -> App {
    let mut app = fixture("unused", wrap);
    let mut editor = Editor::test_with_text(CONDITIONAL);
    editor.file_path = Some("conditional.cpp".into());
    editor.word_wrap = wrap;
    editor.cursor_line = 10;
    app.tabs = vec![Tab::new_file(editor)];
    app
}

#[test]
fn conditional_summaries_and_scope_controls_distinguish_branch_from_entire_tree() {
    for wrap in [false, true] {
        let mut app = conditional_fixture(wrap);
        let editor = app.editor_mut().unwrap();
        editor.selection_anchor = Some((10, 2));
        editor.cursor_col = 4;
        editor.toggle_fold(0);
        let source = editor.rope.clone();
        let branch = draw(&mut app, 130);
        let primary = app.editor().unwrap().fold_buttons.hits[0].0;
        let label = text(&branch, primary);
        assert!(label.contains("Branch only"), "{label}");
        assert!(label.contains("2 lines → #elif"), "{label}");
        assert!(!app.editor().unwrap().is_line_folded(3));
        assert!(!app.editor().unwrap().is_line_folded(4));
        assert!(!app.editor().unwrap().is_line_folded(6));
        let (button, _, _, action) = app.editor().unwrap().fold_buttons.scope_hits[0];
        assert_eq!(action, ScopeAction::EntireConditional);
        assert!(text(&branch, button).contains("Fold all 3 branches"));
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), button.x + 1, button.y);
        let all = draw(&mut app, 130);
        let primary = app.editor().unwrap().fold_buttons.hits[0].0;
        let label = text(&all, primary);
        assert!(label.contains("All 3 branches"), "{label}");
        assert!(label.contains("8 lines → #endif"), "{label}");
        assert_eq!(app.editor().unwrap().visible_lines(0, 3), [0, 9, 10]);
        let (button, _, _, action) = app.editor().unwrap().fold_buttons.scope_hits[0];
        assert_eq!(action, ScopeAction::BranchOnly);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), button.x + 1, button.y);
        let editor = app.editor().unwrap();
        assert_eq!(editor.folded_ranges.get(&0), Some(&3));
        assert!(!editor.is_line_folded(4));
        assert!(!editor.is_line_folded(7));
        assert_eq!(editor.rope, source);
        assert_eq!((editor.cursor_line, editor.cursor_col), (10, 4));
        assert_eq!(editor.selection_anchor, Some((10, 2)));
        assert!(!editor.can_undo());
    }
}

#[test]
fn gutter_folding_keeps_alternatives_visible_and_explains_scope_before_clicking() {
    for wrap in [false, true] {
        let mut app = conditional_fixture(wrap);
        draw(&mut app, 110);
        let gutter = app.gutter_area.unwrap();
        let (x, y) = (gutter.right() - 1, gutter.y);
        mouse(&mut app, MouseEventKind::Moved, x, y);
        let hint = hover_text(&mut app).unwrap().join(" ");
        assert!(hint.contains("Fold this branch only"), "{hint}");
        assert!(hint.contains("#elif on line 4 stays visible"), "{hint}");
        assert!(app.mouse_hover_changed);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, y);
        assert_eq!(app.editor().unwrap().folded_ranges.get(&0), Some(&3));
        assert!(!app.editor().unwrap().is_line_folded(6));
        let buffer = draw(&mut app, 110);
        let code = app.code_area.unwrap();
        assert!(text(&buffer, Rect::new(code.x, code.y + 1, code.width, 1)).contains("#elif OTHER"));
    }
}

#[test]
fn narrow_conditional_badges_keep_the_scope_and_expose_full_details_on_hover() {
    for width in [130, 70, 45, 25, 15] {
        for wrap in [false, true] {
            let mut app = conditional_fixture(wrap);
            app.editor_mut().unwrap().toggle_fold(0);
            let branch = draw(&mut app, width);
            let button = app.editor().unwrap().fold_buttons.hits[0].0;
            let branch_label = text(&branch, button);
            assert!(branch_label.contains("Branch") || branch_label == "B", "width={width}, label={branch_label}");
            mouse(&mut app, MouseEventKind::Moved, button.x, button.y);
            let hint = hover_text(&mut app).unwrap().join(" ");
            assert!(hint.contains("This branch only"));
            assert!(hint.contains("#elif on line 4"));
            app.editor_mut().unwrap().collapse_conditional(0);
            let all = draw(&mut app, width);
            let button = app.editor().unwrap().fold_buttons.hits[0].0;
            let all_label = text(&all, button);
            assert!(all_label.contains("All") || all_label == "A", "width={width}, label={all_label}");
            assert_ne!(branch_label, all_label);
            mouse(&mut app, MouseEventKind::Moved, button.x, button.y);
            let hint = hover_text(&mut app).unwrap().join(" ");
            assert!(hint.contains("all 3 branches"), "{hint}");
            assert!(hint.contains("#elif, #else"), "{hint}");
            assert!(hint.contains("line 10"), "{hint}");
        }
    }
}

#[test]
fn scope_button_hover_changes_and_stale_clicks_are_handled_without_editing_source() {
    let mut app = conditional_fixture(false);
    app.editor_mut().unwrap().toggle_fold(0);
    draw(&mut app, 130);
    let primary = app.editor().unwrap().fold_buttons.hits[0].0;
    let secondary = app.editor().unwrap().fold_buttons.scope_hits[0].0;
    mouse(&mut app, MouseEventKind::Moved, primary.x, primary.y);
    mouse(&mut app, MouseEventKind::Moved, secondary.x, secondary.y);
    assert!(app.mouse_hover_changed);
    let hint = hover_text(&mut app).unwrap().join(" ");
    assert!(hint.contains("entire conditional: all 3 branches"), "{hint}");
    assert!(app.hovered_editor_pos.is_none());
    app.editor_mut().unwrap().insert_char('x');
    assert!(!click(&mut app, secondary.x, secondary.y));
}

#[test]
fn conditional_context_menu_offers_named_scopes_on_headers_gutter_and_bodies() {
    for gutter in [false, true] {
        let mut app = conditional_fixture(false);
        draw(&mut app, 130);
        let code = app.code_area.unwrap();
        let x = if gutter { app.gutter_area.unwrap().right() - 1 } else { code.x + 2 };
        mouse(&mut app, MouseEventKind::Down(MouseButton::Right), x, code.y);
        assert!(app.context_menu_active);
        assert_eq!(app.context_menu_items[0].id, "fold_conditional_branch");
        assert!(app.context_menu_items[0].label.contains("to #elif"));
        let index = app.context_menu_items.iter().position(|item| item.id == "fold_conditional").unwrap();
        assert!(app.context_menu_items[index].label.contains("3 branches"));
        app.context_menu_selected = index;
        crate::input_pickers::handle_context_menu_key(&mut app, crossterm::event::KeyCode::Enter);
        assert_eq!(app.editor().unwrap().folded_ranges.get(&0), Some(&9));
        assert!(!app.editor().unwrap().is_line_folded(9));
    }
    let mut app = conditional_fixture(false);
    app.editor_mut().unwrap().cursor_line = 7;
    app.context_menu_items = App::build_context_menu();
    add_context_actions(&mut app);
    app.execute_palette_command("fold_conditional_branch");
    assert_eq!(app.editor().unwrap().folded_ranges.get(&6), Some(&9));
}

#[test]
fn restored_whole_conditionals_are_labeled_explicitly_and_can_switch_to_one_branch() {
    let mut app = conditional_fixture(false);
    // Older provider ranges sometimes include #endif itself.
    app.editor_mut().unwrap().folded_ranges.insert(0, 10);
    let buffer = draw(&mut app, 130);
    let button = app.editor().unwrap().fold_buttons.hits[0].0;
    assert!(text(&buffer, button).contains("All 3 branches"));
    app.execute_palette_command("fold_conditional_branch");
    // The caret was in tail(), so select the folded header as a keyboard user.
    app.editor_mut().unwrap().cursor_line = 0;
    app.execute_palette_command("fold_conditional_branch");
    assert_eq!(app.editor().unwrap().folded_ranges.get(&0), Some(&3));
    assert!(!app.editor().unwrap().is_line_folded(9));
}

#[test]
fn conditional_fold_preview_frames_show_expanded_branch_and_whole_states() {
    for (name, theme) in [("dark", crate::app_theme::EditorTheme::Dark), ("light", crate::app_theme::EditorTheme::Light)] {
        let mut app = conditional_fixture(false);
        app.colors = theme.colors();
        for state in ["expanded", "branch", "whole"] {
            match state {
                "branch" => app.editor_mut().unwrap().toggle_fold(0),
                "whole" => { app.editor_mut().unwrap().collapse_conditional(0); }
                _ => {}
            }
            let buffer = draw(&mut app, 110);
            if std::env::var_os("VTCODE_CONDITIONAL_PREVIEW").is_some() {
                let rgb = |color| match color { ratatui::style::Color::Rgb(r,g,b) => [r,g,b], _ => [128,128,128] };
                let cells: Vec<_> = buffer.content.iter().map(|cell| serde_json::json!({
                    "text": cell.symbol(), "fg": rgb(cell.fg), "bg": rgb(cell.bg),
                    "bold": cell.modifier.contains(Modifier::BOLD),
                })).collect();
                std::fs::write(format!("target/conditional-fold-{name}-{state}.json"), serde_json::to_vec(&serde_json::json!({
                    "width": 110, "height": 12, "cells": cells,
                })).unwrap()).unwrap();
            }
        }
    }
}

#[test]
fn pending_wrapped_gutter_actions_target_the_visible_conditional_branch_after_scrolling() {
    for wrap in [false, true] {
        let mut app = conditional_fixture(wrap);
        let mut editor = Editor::test_with_text(&format!("{CONDITIONAL}{}", "tail();\n".repeat(5000)));
        editor.file_path = Some("conditional.cpp".into());
        editor.word_wrap = wrap;
        editor.toggle_fold(0);
        app.tabs = vec![Tab::new_file(editor)];
        draw(&mut app, 110);
        app.editor_mut().unwrap().scroll_down(2);
        let buffer = draw(&mut app, 110);
        let code = app.code_area.unwrap();
        let gutter = app.gutter_area.unwrap();
        let y = code.y + 2;
        assert!(text(&buffer, Rect::new(code.x, y, code.width, 1)).contains("#else"));
        mouse(&mut app, MouseEventKind::Moved, gutter.right() - 1, y);
        assert_eq!(app.hovered_gutter_line, Some(6));
        let hint = hover_text(&mut app).unwrap().join(" ");
        assert!(hint.contains("#endif on line 10 stays visible"), "{hint}");
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), gutter.right() - 1, y);
        assert_eq!(app.editor().unwrap().folded_ranges.get(&6), Some(&9));
    }
}
