use super::*;

use crate::app::{ActivePane, DiffMode, Tab, TabKind};
use crate::editor::Editor;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use std::sync::Arc;

fn comparison(base: &str, target: &str, mode: DiffMode, compact: bool) -> App {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text(target);
    editor.file_path = Some("readonly-selection.txt".into());
    editor.read_only = true;
    editor.word_wrap = false;
    let mut tab = Tab::new_file(editor);
    tab.kind = TabKind::Diff(73);
    app.tabs = vec![tab];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.diff_tabs.owner = Some(73);
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
    app.cursor_visible = false;
    app.colors.bg_selection = Color::Rgb(11, 217, 141);
    app
}

fn render(app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let mut painted = None;
    terminal
        .draw(|frame| {
            let area = frame.area();
            app.editor_area = Some(area);
            match app.diff_mode {
                DiffMode::SideBySide => draw_side_by_side_diff(frame, app, area),
                DiffMode::Inline => draw_inline_diff(frame, app, area),
                DiffMode::Off => panic!("fixture requires a diff mode"),
            }
            painted = Some(frame.buffer_mut().clone());
        })
        .unwrap();
    painted.unwrap()
}

fn source_origin(app: &App, line: usize, original: bool) -> (u16, u16) {
    let (area, row, first, inset) = match app.diff_mode {
        DiffMode::SideBySide => {
            let area = if original {
                app.diff_left_code_area.unwrap()
            } else {
                app.code_area.unwrap()
            };
            let row = app
                .diff_render_cache
                .rows
                .iter()
                .position(|row| {
                    row.hidden_lines == 0
                        && if original {
                            row.left_line == Some(line)
                        } else {
                            row.right_line == Some(line)
                        }
                })
                .unwrap();
            (
                area,
                row,
                app.aligned_diff_scroll_row(app.editor().unwrap().scroll_offset),
                u16::from(!original) * 2,
            )
        }
        DiffMode::Inline => {
            let row = app
                .diff_render_cache
                .inline_rows
                .iter()
                .position(|entry| {
                    let row = &app.diff_render_cache.rows[entry.aligned_row];
                    row.hidden_lines == 0
                        && if original {
                            entry.kind == '-' && row.left_line == Some(line)
                        } else {
                            entry.kind != '-' && row.right_line == Some(line)
                        }
                })
                .unwrap();
            (app.code_area.unwrap(), row, app.diff_inline_scroll.row, 0)
        }
        DiffMode::Off => unreachable!(),
    };
    assert!(row >= first && row - first < area.height as usize);
    (area.x + inset, area.y + (row - first) as u16)
}

fn pointer(app: &mut App, kind: MouseEventKind, at: (u16, u16), modifiers: KeyModifiers) {
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind,
            column: at.0,
            row: at.1,
            modifiers,
        },
    );
}

fn drag(app: &mut App, first: (u16, u16), last: (u16, u16)) {
    pointer(
        app,
        MouseEventKind::Down(MouseButton::Left),
        first,
        KeyModifiers::NONE,
    );
    pointer(
        app,
        MouseEventKind::Drag(MouseButton::Left),
        last,
        KeyModifiers::NONE,
    );
    pointer(
        app,
        MouseEventKind::Up(MouseButton::Left),
        last,
        KeyModifiers::NONE,
    );
}

#[test]
fn copy_animation_covers_original_diff_selections_in_both_layouts() {
    for mode in [DiffMode::Inline, DiffMode::SideBySide] {
        let mut app = comparison("alpha\nremoved abcdef\nlast\n", "alpha\nadded xyz\nlast\n", mode, false);
        app.copy_animation.duration_ms = 1_000;
        render(&mut app, 100, 20);
        let origin = source_origin(&app, 1, true);
        drag(&mut app, origin, (origin.0 + 7, origin.1));
        app.cursor_visible = false;
        let before = render(&mut app, 100, 20);
        assert!(app.copy_diff_selection_with(|text| { assert_eq!(text, "removed"); true }));
        app.start_copy_animation(std::time::Instant::now() - std::time::Duration::from_millis(350));
        let during = render(&mut app, 100, 20);
        assert_ne!(before, during);
        assert!(before.content.iter().zip(&during.content).all(|(a, b)| a.symbol() == b.symbol()));
        app.copy_animation.clear();
        let after = render(&mut app, 100, 20);
        assert_eq!(before, after);
    }
}

fn assert_source(app: &App, base: &str, target: &str, version: u64) {
    let editor = app.editor().unwrap();
    assert!(editor.read_only);
    assert!(!editor.modified);
    assert_eq!(editor.content_version, version);
    assert_eq!(editor.rope.to_string(), target);
    assert_eq!(app.compare_base_content.as_deref(), Some(base));
}

#[test]
fn diff_vertical_selection_preserves_screen_column_on_both_sides() {
    let source = "abcdef\n界界ab\ne\u{301}abcde";
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        for compact in [false, true] {
            for original in [false, true] {
                // Keep these rows adjacent in inline mode too: paired removals
                // and additions intentionally interleave the two sources.
                let (base, target) = if original { (source, "") } else { ("", source) };
                let mut app = comparison(base, target, mode, compact);
                let version = app.editor().unwrap().content_version;
                render(&mut app, 140, 20);
                let at = source_origin(&app, 0, original);
                drag(&mut app, (at.0 + 5, at.1), (at.0 + 5, at.1));
                for (key, cursor, expected) in [
                    (KeyCode::Down, (1, 3), Some("f\n界界a")),
                    (KeyCode::Down, (2, 6), Some("f\n界界ab\ne\u{301}abcd")),
                    (KeyCode::Up, (1, 3), Some("f\n界界a")),
                    (KeyCode::Up, (0, 5), None),
                ] {
                    crate::input::handle_editor_key(&mut app, key, KeyModifiers::SHIFT);
                    render(&mut app, 140, 20);
                    assert_eq!(app.diff_scope_cursor(), Some((original, cursor)));
                    let selected = if mode == DiffMode::SideBySide && !original {
                        app.editor().unwrap().selected_text()
                    } else if app.has_diff_selection() {
                        app.diff_selected_text()
                    } else {
                        // The diff copy helper copies the current line when
                        // no selection exists, so check selection state first.
                        None
                    };
                    assert_eq!(selected.as_deref(), expected,
                        "compact={compact}, original={original}, key={key:?}");
                }
                assert_source(&app, base, target, version);
            }
        }
    }
}

#[test]
fn diff_keyboard_shortcuts_move_and_select_words_on_both_sides() {
    let source = "  fooBar_value == café界   tail\nnext";
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        for compact in [false, true] {
            for original in [false, true] {
                let (base, target) = if original {
                    (source, "replacement\n")
                } else {
                    ("removed\n", source)
                };
                let mut app = comparison(base, target, mode, compact);
                let version = app.editor().unwrap().content_version;
                render(&mut app, 140, 20);
                let at = source_origin(&app, 0, original);
                drag(&mut app, (at.0 + 2, at.1), (at.0 + 2, at.1));
                let mut expected = Editor::test_with_text(source);
                expected.set_cursor(0, 2);
                for (right, shift) in [
                    (true, false),
                    (true, true),
                    (true, true),
                    (true, true),
                    (true, true),
                    (false, false),
                    (false, true),
                    (false, true),
                    (false, true),
                ] {
                    if shift {
                        expected.start_selection();
                    } else {
                        expected.clear_selection();
                    }
                    if right {
                        expected.move_word_right();
                    } else {
                        expected.move_word_left();
                    }
                    let modifiers = KeyModifiers::CONTROL
                        | if shift {
                            KeyModifiers::SHIFT
                        } else {
                            KeyModifiers::NONE
                        };
                    crate::input_keyboard::handle_keyboard_event(
                        &mut app,
                        KeyEvent::new(
                            if right { KeyCode::Right } else { KeyCode::Left },
                            modifiers,
                        ),
                    );
                    render(&mut app, 140, 20);
                    assert_eq!(
                        app.diff_scope_cursor(),
                        Some((original, (expected.cursor_line, expected.cursor_col))),
                        "inline={}, compact={compact}, original={original}, right={right}, shift={shift}",
                        mode == DiffMode::Inline
                    );
                    if let Some(selected) = expected.selected_text() {
                        assert_eq!(
                            app.diff_selected_text()
                                .or_else(|| app.editor().unwrap().selected_text())
                                .as_deref(),
                            Some(selected.as_str())
                        );
                    }
                    assert_source(&app, base, target, version);
                }
            }
        }
    }
}

#[test]
fn diff_keyboard_shortcuts_move_and_select_paragraphs_on_both_sides() {
    let source = "first\n  second\n\n\t\n  third\nfourth\n\nfifth";
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        for original in [false, true] {
            let (base, target) = if original { (source, "") } else { ("", source) };
            let mut app = comparison(base, target, mode, false);
            let version = app.editor().unwrap().content_version;
            render(&mut app, 140, 20);
            let at = source_origin(&app, 0, original);
            drag(&mut app, at, at);
            for (code, shift, expected, selected) in [
                (KeyCode::Down, false, (4, 0), None),
                (KeyCode::Down, true, (7, 0), Some("  third\nfourth\n\n")),
                (KeyCode::Up, false, (5, 0), None),
                (KeyCode::Up, true, (1, 0), Some("  second\n\n\t\n  third\n")),
                (KeyCode::Home, false, (0, 0), None),
            ] {
                crate::input_keyboard::handle_keyboard_event(
                    &mut app,
                    KeyEvent::new(
                        code,
                        KeyModifiers::CONTROL
                            | if shift {
                                KeyModifiers::SHIFT
                            } else {
                                KeyModifiers::NONE
                            },
                    ),
                );
                render(&mut app, 140, 20);
                assert_eq!(
                    app.diff_scope_cursor(),
                    Some((original, expected)),
                    "inline={}, original={original}, key={code:?}, shift={shift}",
                    mode == DiffMode::Inline
                );
                if let Some(selected) = selected {
                    assert_eq!(
                        app.diff_selected_text()
                            .or_else(|| app.editor().unwrap().selected_text())
                            .as_deref(),
                        Some(selected)
                    );
                }
                assert_source(&app, base, target, version);
            }
            crate::input_keyboard::handle_keyboard_event(
                &mut app,
                KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
            );
            render(&mut app, 140, 20);
            // An all-deleted inline diff ends with the empty target document.
            let empty_target = original && mode == DiffMode::Inline;
            assert_eq!(
                app.diff_scope_cursor(),
                Some(if empty_target {
                    (false, (0, 0))
                } else {
                    (original, (7, 5))
                })
            );
            let selected = if empty_target {
                format!("{source}\n")
            } else {
                source.to_owned()
            };
            assert_eq!(
                app.diff_selected_text()
                    .or_else(|| app.editor().unwrap().selected_text())
                    .as_deref(),
                Some(selected.as_str())
            );
            assert_source(&app, base, target, version);
        }
    }
}

#[test]
fn diff_keyboard_shortcuts_cross_hidden_context_without_losing_the_caret() {
    let base: String = (0..12).map(|line| format!("line_{line}\n")).collect();
    let target = base
        .replace("line_2", "changed_2")
        .replace("line_9", "changed_9");
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        let mut app = comparison(&base, &target, mode, false);
        app.diff_render_cache = Arc::new(app.diff_render_cache.with_context(0));
        app.editor_mut().unwrap().set_cursor(2, 9);
        let version = app.editor().unwrap().content_version;
        render(&mut app, 120, 20);
        assert_eq!(
            app.diff_render_cache.visible_target_lines.as_slice(),
            &[2, 9]
        );
        for (key, expected) in [
            (KeyCode::Right, (9, 0)),
            (KeyCode::Left, (2, 9)),
            (KeyCode::Down, (9, 0)),
            (KeyCode::Up, (2, 0)),
            (KeyCode::Left, (2, 0)),
            (KeyCode::End, (9, 9)),
            (KeyCode::Right, (9, 9)),
            (KeyCode::Home, (2, 0)),
        ] {
            crate::input_keyboard::handle_keyboard_event(
                &mut app,
                KeyEvent::new(key, KeyModifiers::CONTROL),
            );
            render(&mut app, 120, 20);
            assert_eq!(
                app.diff_scope_cursor(),
                Some((false, expected)),
                "inline={}, {key:?}",
                mode == DiffMode::Inline
            );
            source_origin(&app, expected.0, false);
            assert_source(&app, &base, &target, version);
        }
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
        );
        assert_eq!(app.editor().unwrap().selection_range(), Some((2, 0, 9, 0)));
        render(&mut app, 120, 20);
        assert_eq!(app.diff_scope_cursor(), Some((false, (9, 0))));
    }
}

#[test]
fn diff_keyboard_shortcuts_smart_home_and_line_selection_match_the_editor() {
    let source = "\t  fooBar == café界\nnext";
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        for original in [false, true] {
            let (base, target) = if original {
                (source, "replacement")
            } else {
                ("removed", source)
            };
            let mut app = comparison(base, target, mode, false);
            render(&mut app, 140, 20);
            let at = source_origin(&app, 0, original);
            drag(&mut app, at, at);
            for (code, shift, column, selected) in [
                (KeyCode::End, false, 18, None),
                (KeyCode::Home, true, 3, Some("fooBar == café界")),
                (KeyCode::Home, true, 0, Some("\t  fooBar == café界")),
                (KeyCode::Home, false, 3, None),
                (KeyCode::Home, false, 0, None),
            ] {
                crate::input_keyboard::handle_keyboard_event(
                    &mut app,
                    KeyEvent::new(
                        code,
                        if shift {
                            KeyModifiers::SHIFT
                        } else {
                            KeyModifiers::NONE
                        },
                    ),
                );
                render(&mut app, 140, 20);
                assert_eq!(
                    app.diff_scope_cursor(),
                    Some((original, (0, column))),
                    "inline={}, original={original}, {code:?}",
                    mode == DiffMode::Inline
                );
                if let Some(selected) = selected {
                    assert_eq!(
                        app.diff_selected_text()
                            .or_else(|| app.editor().unwrap().selected_text())
                            .as_deref(),
                        Some(selected)
                    );
                }
            }
        }
    }
}

#[test]
fn side_by_side_drag_copies_either_source_without_spacers_or_diff_gutters() {
    let base = "first\nremoved_A\nremoved_B\nold\n\nlast\n";
    let target = "first\nnew\n\nlast\n";
    for (original, expected) in [(true, "moved_A\nremoved_B\nol"), (false, "ew\n\nla")] {
        for reverse in [false, true] {
            let mut app = comparison(base, target, DiffMode::SideBySide, false);
            let version = app.editor().unwrap().content_version;
            let baseline = render(&mut app, 120, 20);
            let first = source_origin(&app, 1, original);
            let last = source_origin(&app, 3, original);
            let first = (first.0 + if original { 2 } else { 1 }, first.1);
            let last = (last.0 + 2, last.1);
            if reverse {
                drag(&mut app, last, first);
            } else {
                drag(&mut app, first, last);
            }
            assert_eq!(app.diff_selected_text().as_deref(), Some(expected));
            let selected = render(&mut app, 120, 20);
            assert_ne!(selected[first].bg, baseline[first].bg);
            assert_eq!(selected[first].symbol(), baseline[first].symbol());
            assert_source(&app, base, target, version);
        }
    }
}

#[test]
fn original_shift_click_and_shift_arrows_extend_the_same_selection() {
    let base = "first\nremoved_A\nremoved_B\nold\nlast";
    let target = "first\nnew\nlast";
    let mut app = comparison(base, target, DiffMode::SideBySide, false);
    render(&mut app, 120, 20);
    let first = source_origin(&app, 1, true);
    pointer(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        (first.0 + 1, first.1),
        KeyModifiers::NONE,
    );
    pointer(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        (first.0 + 1, first.1),
        KeyModifiers::NONE,
    );
    crate::input::handle_editor_key(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
    crate::input::handle_editor_key(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
    assert_eq!(app.diff_selected_text().as_deref(), Some("em"));
    let last = source_origin(&app, 3, true);
    pointer(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        (last.0 + 2, last.1),
        KeyModifiers::SHIFT,
    );
    pointer(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        (last.0 + 2, last.1),
        KeyModifiers::SHIFT,
    );
    assert_eq!(
        app.diff_selected_text().as_deref(),
        Some("emoved_A\nremoved_B\nol")
    );
    let selected = render(&mut app, 120, 20);
    assert_eq!(selected[(first.0 + 1, first.1)].bg, app.colors.bg_selection);
    assert_eq!(app.editor().unwrap().rope.to_string(), target);
}

#[test]
fn inline_drag_copies_removed_and_added_text_preserving_literal_signs() {
    let base = "alpha\n-literal_old\nomega";
    let target = "alpha\n+literal_new\nomega";
    for compact in [false, true] {
        let mut app = comparison(base, target, DiffMode::Inline, compact);
        let version = app.editor().unwrap().content_version;
        let baseline = render(&mut app, 100, 16);
        let removed = source_origin(&app, 1, true);
        let added = source_origin(&app, 1, false);
        drag(
            &mut app,
            removed,
            (added.0 + "+literal_new".len() as u16, added.1),
        );
        assert_eq!(
            app.diff_selected_text().as_deref(),
            Some("-literal_old\n+literal_new")
        );
        let selected = render(&mut app, 100, 16);
        for point in [removed, added] {
            assert_ne!(selected[point].bg, baseline[point].bg);
            assert_eq!(selected[point].symbol(), baseline[point].symbol());
        }
        assert_source(&app, base, target, version);
    }
}

#[test]
fn compact_copy_skips_hidden_context_markers_without_inventing_blank_source_rows() {
    let prefix: String = (0..12).map(|line| format!("prefix_{line}\n")).collect();
    let middle: String = (0..12).map(|line| format!("middle_{line}\n")).collect();
    let suffix: String = (0..12).map(|line| format!("suffix_{line}\n")).collect();
    let base = format!("{prefix}old_A\n{middle}old_B\n{suffix}");
    let target = format!("{prefix}new_A\n{middle}new_B\n{suffix}");
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        for original in [false, true] {
            if mode == DiffMode::Inline && original {
                continue;
            }
            let mut app = comparison(&base, &target, mode, true);
            render(&mut app, 140, 30);
            assert!(
                app.diff_render_cache
                    .rows
                    .iter()
                    .any(|row| row.hidden_lines > 0)
            );
            let first = source_origin(&app, 12, original || mode == DiffMode::Inline);
            let last = source_origin(&app, 25, original);
            drag(&mut app, first, (last.0 + 5, last.1));
            let expected = match (mode, original) {
                (DiffMode::SideBySide, true) => {
                    "old_A\nmiddle_0\nmiddle_1\nmiddle_10\nmiddle_11\nold_B"
                }
                (DiffMode::SideBySide, false) => {
                    "new_A\nmiddle_0\nmiddle_1\nmiddle_10\nmiddle_11\nnew_B"
                }
                (DiffMode::Inline, _) => {
                    "old_A\nnew_A\nmiddle_0\nmiddle_1\nmiddle_10\nmiddle_11\nold_B\nnew_B"
                }
                _ => unreachable!(),
            };
            assert_eq!(app.diff_selected_text().as_deref(), Some(expected));
            assert!(
                !app.diff_selected_text()
                    .unwrap()
                    .contains("unchanged lines hidden")
            );
        }
    }
}

#[test]
fn diff_selection_uses_grapheme_cells_after_horizontal_pan_on_both_sources() {
    let tail = "_padding".repeat(20);
    let base = format!("prefix界e\u{301}🦀tail_old{tail}\nnext");
    let target = format!("prefix界e\u{301}🦀tail_new{tail}\nnext");
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        for compact in [false, true] {
            for original in [false, true] {
                let mut app = comparison(&base, &target, mode, compact);
                app.editor_mut().unwrap().set_cursor(0, 10);
                app.editor_mut().unwrap().horizontal_scroll = 6;
                app.diff_left_horizontal_scroll = 6;
                render(&mut app, 100, 12);
                let first = source_origin(&app, 0, original);
                drag(&mut app, (first.0 + 2, first.1), (first.0 + 5, first.1));
                assert_eq!(app.diff_selected_text().as_deref(), Some("e\u{301}🦀"));
                let selected = render(&mut app, 100, 12);
                assert_eq!(selected[(first.0 + 2, first.1)].symbol(), "e\u{301}");
                assert_eq!(selected[(first.0 + 2, first.1)].bg, app.colors.bg_selection);
                assert_eq!(selected[(first.0 + 3, first.1)].bg, app.colors.bg_selection);
            }
        }
    }
}

#[test]
fn control_characters_keep_visible_diff_glyphs_selectable_without_losing_source_text() {
    let caret = Color::Rgb(251, 17, 139);
    for control in ['\t', '\u{7}'] {
        let base = format!("a{control}b_old\nnext");
        let target = format!("a{control}b_new\nnext");
        for mode in [DiffMode::SideBySide, DiffMode::Inline] {
            for compact in [false, true] {
                for original in [false, true] {
                    let mut app = comparison(&base, &target, mode, compact);
                    app.colors.bg_cursor = caret;
                    let version = app.editor().unwrap().content_version;
                    let plain = render(&mut app, 120, 16);
                    let first = source_origin(&app, 0, original);
                    let b = (first.0..plain.area.right())
                        .find(|x| plain[(*x, first.1)].symbol() == "b")
                        .expect("comparison paints b after the control character");
                    assert_eq!(b, first.0 + 1, "control characters occupy no diff cells");
                    let point = (b, first.1);
                    pointer(
                        &mut app,
                        MouseEventKind::Down(MouseButton::Left),
                        point,
                        KeyModifiers::NONE,
                    );
                    pointer(
                        &mut app,
                        MouseEventKind::Up(MouseButton::Left),
                        point,
                        KeyModifiers::NONE,
                    );
                    let clicked = render(&mut app, 120, 16);
                    assert_eq!(clicked[point].symbol(), "b");
                    assert_eq!(clicked[point].bg, caret);
                    app.clear_diff_selection();
                    render(&mut app, 120, 16);
                    drag(&mut app, point, (b + 1, first.1));
                    assert_eq!(app.diff_selected_text().as_deref(), Some("b"));
                    let selected = render(&mut app, 120, 16);
                    assert_eq!(selected[point].symbol(), "b");
                    assert_eq!(selected[point].bg, app.colors.bg_selection);
                    drag(&mut app, first, (b + 1, first.1));
                    assert_eq!(app.diff_selected_text(), Some(format!("a{control}b")));
                    assert_source(&app, &base, &target, version);
                }
            }
        }
    }
}

#[test]
fn inline_horizontal_pan_counts_visible_cells_while_selection_keeps_source_columns() {
    for control in ['\t', '\u{7}'] {
        let tail = "_padding".repeat(20);
        let base = format!("a{control}b_old{tail}\nnext");
        let target = format!("a{control}b_new{tail}\nnext");
        for compact in [false, true] {
            for original in [false, true] {
                let mut app = comparison(&base, &target, DiffMode::Inline, compact);
                app.editor_mut().unwrap().set_cursor(0, 3);
                app.editor_mut().unwrap().horizontal_scroll = 1;
                let plain = render(&mut app, 100, 16);
                let first = source_origin(&app, 0, original);
                assert_eq!(app.diff_inline_scroll.column, 1);
                assert_eq!(plain[first].symbol(), "b");
                drag(&mut app, first, (first.0 + 1, first.1));
                assert_eq!(app.diff_selected_text().as_deref(), Some("b"));
                let selected = render(&mut app, 100, 16);
                assert_eq!(selected[first].symbol(), "b");
                assert_eq!(selected[first].bg, app.colors.bg_selection);
            }
        }
    }
}

#[test]
fn empty_original_eof_copy_does_not_fall_through_to_the_target_clipboard() {
    let mut app = comparison("", "target_text", DiffMode::SideBySide, false);
    app.editor_mut().unwrap().clipboard = "keep_target_clipboard".into();
    render(&mut app, 120, 16);
    let area = app.diff_left_code_area.unwrap();
    let eof = (area.x, area.y + app.diff_render_cache.rows.len() as u16);
    assert!(app.diff_selection_mouse_down(eof.0, eof.1, KeyModifiers::NONE));
    assert!(app.diff_selection_mouse_up(eof.0, eof.1));
    assert_eq!(app.diff_selected_text().as_deref(), Some(""));
    assert!(app.copy_diff_selection_with(|_| panic!("empty original EOF reached clipboard")));
    assert_eq!(app.editor().unwrap().clipboard, "keep_target_clipboard");
    assert!(app.clipboard_history.is_empty());
    assert_source(
        &app,
        "",
        "target_text",
        app.editor().unwrap().content_version,
    );
}

#[test]
fn double_and_triple_click_keep_word_and_line_expansion_after_mouse_up() {
    let base = "alpha old_word\nsecond\n";
    let target = "alpha new_word\nsecond\n";
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        for original in [false, true] {
            let mut app = comparison(base, target, mode, false);
            render(&mut app, 120, 16);
            let first = source_origin(&app, 0, original);
            let point = (first.0 + 8, first.1);
            for clicks in 1..=3 {
                pointer(
                    &mut app,
                    MouseEventKind::Down(MouseButton::Left),
                    point,
                    KeyModifiers::NONE,
                );
                pointer(
                    &mut app,
                    MouseEventKind::Up(MouseButton::Left),
                    point,
                    KeyModifiers::NONE,
                );
                if clicks == 2 {
                    assert_eq!(
                        app.diff_selected_text().as_deref(),
                        Some(if original { "old_word" } else { "new_word" })
                    );
                } else if clicks == 3 {
                    assert_eq!(
                        app.diff_selected_text().as_deref(),
                        Some(if original {
                            "alpha old_word\n"
                        } else {
                            "alpha new_word\n"
                        })
                    );
                }
            }
            assert_eq!(app.editor().unwrap().rope.to_string(), target);

            let mut app = comparison(base, target, mode, false);
            render(&mut app, 120, 16);
            let first = source_origin(&app, 0, original);
            let point = (first.0 + 8, first.1);
            pointer(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                point,
                KeyModifiers::NONE,
            );
            pointer(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                point,
                KeyModifiers::NONE,
            );
            pointer(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                point,
                KeyModifiers::NONE,
            );
            let last = source_origin(&app, 1, original && mode == DiffMode::SideBySide);
            pointer(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                (last.0 + 3, last.1),
                KeyModifiers::NONE,
            );
            pointer(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                (last.0 + 3, last.1),
                KeyModifiers::NONE,
            );
            let expected = match (mode, original) {
                (DiffMode::Inline, true) => "old_word\nalpha new_word\nsecond",
                (_, true) => "old_word\nsecond",
                (_, false) => "new_word\nsecond",
            };
            assert_eq!(app.diff_selected_text().as_deref(), Some(expected));
        }
    }
}

#[test]
fn select_all_in_diff_uses_the_focused_source_and_preserves_trailing_newlines() {
    for trailing in ["", "\n"] {
        let base = format!("old\nnext{trailing}");
        let target = format!("new\nnext{trailing}");
        for mode in [DiffMode::SideBySide, DiffMode::Inline] {
            for original in [false, true] {
                let mut app = comparison(&base, &target, mode, false);
                let version = app.editor().unwrap().content_version;
                render(&mut app, 120, 16);
                let first = source_origin(&app, 0, original);
                pointer(
                    &mut app,
                    MouseEventKind::Down(MouseButton::Left),
                    first,
                    KeyModifiers::NONE,
                );
                pointer(
                    &mut app,
                    MouseEventKind::Up(MouseButton::Left),
                    first,
                    KeyModifiers::NONE,
                );
                crate::input::handle_editor_key(
                    &mut app,
                    KeyCode::Char('a'),
                    KeyModifiers::CONTROL,
                );
                let expected = if mode == DiffMode::Inline {
                    format!("old\nnew\nnext{trailing}")
                } else if original {
                    base.clone()
                } else {
                    target.clone()
                };
                assert_eq!(app.diff_selected_text(), Some(expected));
                assert_source(&app, &base, &target, version);
            }
        }
    }
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        let mut app = comparison("", "", mode, false);
        render(&mut app, 120, 16);
        let area = if mode == DiffMode::SideBySide {
            app.diff_left_code_area.unwrap()
        } else {
            app.code_area.unwrap()
        };
        assert!(app.diff_selection_mouse_down(area.x, area.y, KeyModifiers::NONE));
        assert!(app.diff_selection_mouse_up(area.x, area.y));
        crate::input::handle_editor_key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
        let mut called = false;
        assert!(app.copy_diff_selection_with(|_| {
            called = true;
            true
        }));
        assert!(!called);
        assert_eq!(app.editor().unwrap().rope.to_string(), "");
    }
}

#[test]
fn diff_drag_capture_stays_on_original_when_pointer_crosses_to_target() {
    let base = "old_value\nnext";
    let target = "new_value\nnext";
    let mut app = comparison(base, target, DiffMode::SideBySide, false);
    render(&mut app, 120, 16);
    let original = source_origin(&app, 0, true);
    let target_point = source_origin(&app, 0, false);
    drag(&mut app, original, (target_point.0 + 5, target_point.1));
    assert_eq!(app.diff_selected_text().as_deref(), Some("old_value"));
    assert_eq!(app.editor().unwrap().rope.to_string(), target);
}

#[test]
fn diff_selection_does_not_leak_to_another_tab_or_equal_text_cache_replacement() {
    let base = "old_value\nnext";
    let target = "new_value\nnext";
    let mut app = comparison(base, target, DiffMode::SideBySide, false);
    render(&mut app, 120, 16);
    let first = source_origin(&app, 0, true);
    drag(&mut app, first, (first.0 + 3, first.1));
    assert_eq!(app.diff_selected_text().as_deref(), Some("old"));
    let mut editor = Editor::test_with_text(target);
    editor.file_path = Some("readonly-selection.txt".into());
    editor.read_only = true;
    let mut second = Tab::new_file(editor);
    second.kind = TabKind::Diff(74);
    app.tabs.push(second);
    app.active_tab = 1;
    app.diff_tabs.owner = Some(74);
    assert!(app.diff_selected_text().is_none());
    render(&mut app, 120, 16);
    app.active_tab = 0;
    app.diff_tabs.owner = Some(73);
    render(&mut app, 120, 16);
    assert!(app.diff_selected_text().is_none());
    let first = source_origin(&app, 0, true);
    drag(&mut app, first, (first.0 + 3, first.1));
    assert_eq!(app.diff_selected_text().as_deref(), Some("old"));
    app.diff_render_cache = Arc::new((*app.diff_render_cache).clone());
    assert!(app.diff_selected_text().is_none());
    render(&mut app, 120, 16);
    assert!(app.diff_selected_text().is_none());
}

#[test]
fn native_target_shift_selection_survives_final_diff_paint_and_readonly_edit_keys() {
    let base = "first\nold_value\nlast";
    let target = "first\nnew_value\nlast";
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        for compact in [false, true] {
            let mut app = comparison(base, target, mode, compact);
            app.editor_mut().unwrap().set_cursor(1, 0);
            let version = app.editor().unwrap().content_version;
            let plain = render(&mut app, 120, 16);
            for _ in 0..3 {
                crate::input::handle_editor_key(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
            }
            assert_eq!(
                app.editor().unwrap().selected_text().as_deref(),
                Some("new")
            );
            assert_eq!(
                app.diff_selected_text()
                    .or_else(|| app.editor().unwrap().selected_text())
                    .as_deref(),
                Some("new")
            );
            let selected = render(&mut app, 120, 16);
            let first = source_origin(&app, 1, false);
            for offset in 0..3 {
                let point = (first.0 + offset, first.1);
                assert_eq!(selected[point].symbol(), plain[point].symbol());
                assert_eq!(selected[point].bg, app.colors.bg_selection);
            }
            for key in [
                KeyCode::Char('X'),
                KeyCode::Enter,
                KeyCode::Backspace,
                KeyCode::Delete,
            ] {
                crate::input::handle_editor_key(&mut app, key, KeyModifiers::NONE);
            }
            assert_source(&app, base, target, version);
        }
    }
}

#[test]
fn diff_context_menu_preserves_selection_and_copy_dispatch_uses_raw_source() {
    let base = "alpha\n-literal_old\nomega";
    let target = "alpha\n+literal_new\nomega";
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        for copied in [true, false] {
            let mut app = comparison(base, target, mode, false);
            let version = app.editor().unwrap().content_version;
            render(&mut app, 120, 16);
            assert!(app.is_menu_cmd_disabled("copy"));
            assert!(!app.copy_diff_selection_with(|_| panic!("empty selection reached clipboard")));
            let first = source_origin(&app, 1, true);
            drag(&mut app, first, (first.0 + 8, first.1));
            let expected = "-literal";
            assert_eq!(app.diff_selected_text().as_deref(), Some(expected));
            assert!(!app.editor().unwrap().has_selection());
            assert!(!app.is_menu_cmd_disabled("copy"));
            let target_point = source_origin(&app, 1, false);
            pointer(
                &mut app,
                MouseEventKind::Down(MouseButton::Right),
                target_point,
                KeyModifiers::NONE,
            );
            assert!(app.context_menu_active);
            assert_eq!(app.diff_selected_text().as_deref(), Some(expected));
            assert!(!app.is_menu_cmd_disabled("copy"));
            app.editor_mut().unwrap().clipboard_is_line = true;
            let mut received = String::new();
            assert!(app.copy_diff_selection_with(|text| {
                received.push_str(text);
                copied
            }));
            assert_eq!(received, expected);
            assert_eq!(app.editor().unwrap().clipboard, expected);
            assert!(!app.editor().unwrap().clipboard_is_line);
            assert_eq!(app.editor().unwrap().clipboard_failed, !copied);
            assert_eq!(
                app.clipboard_history.front().map(String::as_str),
                Some(expected)
            );
            assert_eq!(
                app.notification_history
                    .iter()
                    .any(|(message, _, _)| message.contains("internal clipboard only")),
                !copied
            );
            assert_source(&app, base, target, version);
        }
    }
}

#[test]
fn commit_details_diff_focus_releases_copy_and_shift_navigation_to_native_diff() {
    let mut app = comparison(
        "old_value\nnext",
        "new_value\nnext",
        DiffMode::SideBySide,
        false,
    );
    render(&mut app, 120, 16);
    app.tabs[0].kind = TabKind::CommitDetails;
    app.diff_tabs.owner = Some(crate::diff_tabs::COMMIT_DIFF_ID);
    app.commit_details.active = true;
    app.commit_details.focus = crate::commit_details::Focus::Diff;
    app.commit_details.data.files = vec![crate::commit_details::CommitFile::new(
        'M',
        "readonly-selection.txt",
    )];
    app.commit_details.diff_file = Some(0);
    assert!(crate::ui_commit_details::focused(&app));
    assert!(app.should_render_diff_for_active_editor());
    for event in [
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER),
        KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT),
        KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT),
    ] {
        assert!(!crate::ui_commit_details::owns_key(&app, event));
    }
    assert!(crate::ui_commit_details::owns_key(
        &app,
        KeyEvent::new(KeyCode::Down, KeyModifiers::ALT)
    ));
    let first = source_origin(&app, 0, true);
    assert!(app.diff_selection_mouse_down(first.0, first.1, KeyModifiers::NONE));
    assert!(app.diff_selection_mouse_up(first.0, first.1));
    crate::input::handle_editor_key(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
    crate::input::handle_editor_key(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
    assert_eq!(app.diff_selected_text().as_deref(), Some("ol"));
    let mut received = String::new();
    assert!(app.copy_diff_selection_with(|text| {
        received.push_str(text);
        true
    }));
    assert_eq!(received, "ol");
    assert_eq!(app.editor().unwrap().rope.to_string(), "new_value\nnext");
}

fn long_deletion() -> (String, &'static str) {
    let deleted: String = (0..100)
        .map(|line| format!("deleted_{line:03}\n"))
        .collect();
    (format!("start\n{deleted}end\n"), "start\nend\n")
}

fn source_slice(text: &str, start: (usize, usize), end: (usize, usize)) -> String {
    let rope = ropey::Rope::from_str(text);
    let first = rope.line_to_char(start.0) + start.1;
    let last = rope.line_to_char(end.0) + end.1;
    rope.slice(first..last).to_string()
}

#[test]
fn original_keyboard_selection_reveals_interior_deleted_rows_and_survives_wheel_scroll() {
    let (base, target) = long_deletion();
    let mut app = comparison(&base, target, DiffMode::SideBySide, false);
    app.colors.bg_cursor = Color::Rgb(251, 17, 139);
    let version = app.editor().unwrap().content_version;
    render(&mut app, 120, 10);
    let first = source_origin(&app, 1, true);
    pointer(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        first,
        KeyModifiers::NONE,
    );
    pointer(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        first,
        KeyModifiers::NONE,
    );
    for line in 2..=31 {
        crate::input::handle_editor_key(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        let frame = render(&mut app, 120, 10);
        let cursor = source_origin(&app, line, true);
        assert_eq!(frame[cursor].symbol(), "d");
        assert_eq!(frame[cursor].bg, app.colors.bg_cursor);
        assert_eq!(
            app.diff_selected_text(),
            Some(source_slice(&base, (1, 0), (line, 0)))
        );
    }
    let selected = app.diff_selected_text().unwrap();
    let before = app.aligned_diff_scroll_row(app.editor().unwrap().scroll_offset);
    assert!(
        before > 20,
        "viewport must represent the middle of the deleted run"
    );
    let area = app.diff_left_code_area.unwrap();
    let point = (area.x + 1, area.y + 1);
    pointer(
        &mut app,
        MouseEventKind::ScrollUp,
        point,
        KeyModifiers::NONE,
    );
    render(&mut app, 120, 10);
    assert!(app.aligned_diff_scroll_row(app.editor().unwrap().scroll_offset) < before);
    assert_eq!(app.diff_selected_text().as_deref(), Some(selected.as_str()));
    pointer(
        &mut app,
        MouseEventKind::ScrollDown,
        point,
        KeyModifiers::NONE,
    );
    render(&mut app, 120, 10);
    assert_eq!(app.diff_selected_text().as_deref(), Some(selected.as_str()));
    assert_source(&app, &base, target, version);
}

#[test]
fn original_drag_autoscroll_reaches_and_reverses_inside_a_long_deleted_run() {
    let (base, target) = long_deletion();
    let mut app = comparison(&base, target, DiffMode::SideBySide, false);
    app.colors.bg_cursor = Color::Rgb(251, 17, 139);
    let version = app.editor().unwrap().content_version;
    render(&mut app, 120, 10);
    let first = source_origin(&app, 1, true);
    let anchor = (first.0 + 2, first.1);
    pointer(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        anchor,
        KeyModifiers::NONE,
    );
    for _ in 0..32 {
        let area = app.diff_left_code_area.unwrap();
        pointer(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            (area.x + 3, area.bottom()),
            KeyModifiers::NONE,
        );
        render(&mut app, 120, 10);
    }
    let bottom_first = app.aligned_diff_scroll_row(app.editor().unwrap().scroll_offset);
    assert!(bottom_first >= 30);
    let area = app.diff_left_code_area.unwrap();
    let last_row = bottom_first + area.height as usize - 1;
    let last_source = app.diff_render_cache.rows[last_row].left_line.unwrap();
    assert_eq!(
        app.diff_selected_text(),
        Some(source_slice(&base, (1, 2), (last_source, 3)))
    );
    for _ in 0..12 {
        let area = app.diff_left_code_area.unwrap();
        pointer(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            (area.x + 3, area.y.saturating_sub(1)),
            KeyModifiers::NONE,
        );
        render(&mut app, 120, 10);
    }
    let top_first = app.aligned_diff_scroll_row(app.editor().unwrap().scroll_offset);
    assert!(top_first < bottom_first && top_first > 10);
    let top_source = app.diff_render_cache.rows[top_first].left_line.unwrap();
    let area = app.diff_left_code_area.unwrap();
    pointer(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        (area.x + 3, area.y),
        KeyModifiers::NONE,
    );
    let frame = render(&mut app, 120, 10);
    let cursor = source_origin(&app, top_source, true);
    assert_eq!(frame[(cursor.0 + 3, cursor.1)].symbol(), "e");
    assert_eq!(frame[(cursor.0 + 3, cursor.1)].bg, app.colors.bg_cursor);
    assert_eq!(
        app.diff_selected_text(),
        Some(source_slice(&base, (1, 2), (top_source, 3)))
    );
    assert_source(&app, &base, target, version);
}

#[test]
fn original_focus_does_not_edit_an_editable_target_through_alt_history_or_palette_commands() {
    let base = "before\nold_value\nafter\n";
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        let mut app = comparison(base, "before\nnew_value\nafter\n", mode, false);
        app.tabs[0].kind = TabKind::File;
        app.diff_tabs.owner = None;
        let editor = app.editor_mut().unwrap();
        editor.read_only = false;
        editor.set_cursor(2, 0);
        editor.insert_char('!');
        assert!(editor.can_undo());
        editor.set_cursor(1, 0);
        let target = editor.rope.to_string();
        let version = editor.content_version;
        let source = editor.rope.clone();
        app.set_compare_diff_lines(crate::app::compute_file_diff(base, &target));
        app.clipboard_history.push_back("unexpected_paste".into());
        render(&mut app, 120, 16);
        let first = source_origin(&app, 1, true);
        drag(&mut app, first, (first.0 + 3, first.1));
        assert!(app.diff_selection_owns_cursor());
        assert_eq!(app.diff_selected_text().as_deref(), Some("old"));
        for modifiers in [KeyModifiers::ALT, KeyModifiers::ALT | KeyModifiers::CONTROL] {
            for key in [KeyCode::Up, KeyCode::Down] {
                crate::input::handle_editor_key(&mut app, key, modifiers);
            }
        }
        crate::input::handle_editor_key(
            &mut app,
            KeyCode::Char('V'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(
                KeyCode::Char('V'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
        );
        for code in ['z', 'y', '/', ']', '[', 'j', 't'] {
            crate::input_keyboard::handle_keyboard_event(
                &mut app,
                KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL),
            );
        }
        for command in [
            "undo",
            "redo",
            "toggle_comment",
            "move_line_up",
            "transpose_chars",
            "eol_crlf",
            "format_selection",
        ] {
            // Dispatch's return value describes which handler ran, not whether
            // this read-only operation was blocked. Assert editor state below.
            app.execute_palette_command(command);
        }
        let editor = app.editor().unwrap();
        assert!(editor.rope.is_instance(&source));
        assert_eq!(editor.rope.to_string(), target);
        assert_eq!(editor.content_version, version);
        assert!(editor.extra_cursors.is_empty());
        assert!(editor.can_undo());
        assert!(!editor.can_redo());
        assert!(!app.clipboard_cycle_active);
        assert_eq!(app.diff_selected_text().as_deref(), Some("old"));
    }
}
