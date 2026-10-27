use super::Editor;

#[test]
fn smart_paste_preserves_nested_blocks_and_relative_indentation() {
    for indent in ["", "    ", "\t", " \t"] {
        for payload in [
            "if True:\n    print('界')\n    return",
            "if True:\n\tprint('界')\n\treturn",
            "if True:\n\tif ready:\n\t    return",
        ] {
            let mut editor = Editor::test_with_text(indent);
            editor.cursor_col = indent.chars().count();
            editor.paste_with_text(payload);
            let expected = format!("{indent}{}", payload.replace('\n', &format!("\n{indent}")));
            assert_eq!(editor.rope.to_string(), expected, "indent={indent:?}");
            editor.undo();
            assert_eq!(editor.rope.to_string(), indent);
            assert!(!editor.can_undo());
            editor.redo();
            assert_eq!(editor.rope.to_string(), expected);
        }
    }
}

#[test]
fn smart_paste_at_column_zero_preserves_copied_indentation() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    for body in [
        "    // café comment",
        "    // café comment\n",
        "    // comment\n        // nested comment\n",
        "\t// comment\r\n\t\t// nested comment\r\n",
        " \t// comment\r \t    // nested comment\r",
        "    // comment\n \t\n        // nested comment\n",
    ] {
        for repeats in [1, 256] {
            let payload = body.repeat(repeats);
            crate::clipboard::remember_line_copy(&payload, false);
            let normalized = payload.replace("\r\n", "\n").replace('\r', "\n");
            for (source, row, col, anchor, before, after) in [
                ("", 0, 0, None, "", ""),
                ("before\n", 1, 0, None, "before\n", ""),
                ("before\n\nafter", 1, 0, None, "before\n", "\nafter"),
                ("before\nold\nafter", 1, 3, Some((1, 0)), "before\n", "\nafter"),
            ] {
                let mut editor = Editor::test_with_text(source);
                (editor.cursor_line, editor.cursor_col) = (row, col);
                editor.selection_anchor = anchor;
                editor.paste_with_text(&payload);
                let expected = format!("{before}{normalized}{after}");
                assert_eq!(editor.rope.to_string(), expected, "{body:?}, repeats={repeats}, source={source:?}");
                let end = before.chars().count() + normalized.chars().count();
                let line = editor.rope.char_to_line(end);
                assert_eq!((editor.cursor_line, editor.cursor_col), (line, end - editor.rope.line_to_char(line)));
                editor.undo();
                assert_eq!(editor.rope.to_string(), source);
                assert_eq!((editor.cursor_line, editor.cursor_col), (row, col));
                assert_eq!(editor.selection_anchor, anchor);
                assert!(!editor.can_undo());
                editor.redo();
                assert_eq!(editor.rope.to_string(), expected);
            }
        }
    }
}

#[test]
fn smart_paste_rebases_copied_blocks_at_existing_indentation() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    for source_indent in ["    ", "\t", " \t"] {
        for destination in ["  ", "\t", " \t"] {
            let payload = format!("{source_indent}// first\n{source_indent}    // nested\n{source_indent}// last");
            crate::clipboard::remember_line_copy(&payload, false);
            let mut editor = Editor::test_with_text(destination);
            editor.cursor_col = destination.chars().count();
            editor.paste_with_text(&payload);
            assert_eq!(editor.rope.to_string(), format!("{destination}// first\n{destination}    // nested\n{destination}// last"));
        }
    }
}

#[test]
fn smart_paste_in_the_middle_of_text_does_not_reindent_the_payload() {
    let mut editor = Editor::test_with_text("    prefix suffix");
    editor.cursor_col = 10;
    let payload = "first\n  second\n    third";
    editor.paste_with_text(payload);
    assert_eq!(
        editor.rope.to_string(),
        format!("    prefix{payload} suffix")
    );
}

#[test]
fn selection_replacement_is_one_undo_step_in_both_directions() {
    for backwards in [false, true] {
        for operation in [0, 1, 2, 3] {
            let original = "left αβ\r\nγδ right";
            let mut editor = Editor::test_with_text(original);
            let (anchor, cursor) = if backwards {
                ((1, 2), (0, 5))
            } else {
                ((0, 5), (1, 2))
            };
            editor.selection_anchor = Some(anchor);
            (editor.cursor_line, editor.cursor_col) = cursor;
            match operation {
                0 => editor.insert_char_replacing_selection('界'),
                1 => editor.insert_newline_replacing_selection(),
                2 => editor.paste_with_text("replacement\nnext"),
                _ => editor.paste_plain_with_text("replacement\nnext"),
            }
            let edited = editor.rope.clone();
            editor.undo();
            assert_eq!(
                editor.rope.to_string(),
                original,
                "operation={operation}, backwards={backwards}"
            );
            assert_eq!(editor.selection_anchor, Some(anchor));
            assert_eq!((editor.cursor_line, editor.cursor_col), cursor);
            assert!(!editor.can_undo(), "one user edit must have one undo step");
            editor.redo();
            assert_eq!(editor.rope, edited);
        }
    }
}

#[test]
fn replacing_selection_in_overwrite_mode_keeps_the_following_character() {
    let mut editor = Editor::test_with_text("before selected after");
    editor.overwrite_mode = true;
    editor.selection_anchor = Some((0, 7));
    editor.cursor_col = 15;
    editor.insert_char_replacing_selection('x');
    assert_eq!(editor.rope.to_string(), "before x after");
    editor.undo();
    assert_eq!(editor.rope.to_string(), "before selected after");
}

#[test]
fn replacing_selection_with_a_closer_inserts_instead_of_skipping() {
    let mut editor = Editor::test_with_text("(selected)");
    editor.selection_anchor = Some((0, 1));
    editor.cursor_col = 9;
    editor.insert_char_replacing_selection(')');
    assert_eq!(editor.rope.to_string(), "())");
    editor.undo();
    assert_eq!(editor.rope.to_string(), "(selected)");
}

#[test]
fn paste_separates_typing_groups_before_and_after_it() {
    let mut editor = Editor::test_with_text("");
    editor.insert_char('a');
    editor.paste_plain_with_text("bc");
    editor.insert_char('d');
    assert_eq!(editor.rope.to_string(), "abcd");
    for expected in ["abc", "a", ""] {
        editor.undo();
        assert_eq!(editor.rope.to_string(), expected);
    }
    for expected in ["a", "abc", "abcd"] {
        editor.redo();
        assert_eq!(editor.rope.to_string(), expected);
    }
}

#[test]
fn typing_over_a_closer_keeps_redo_and_does_not_change_the_document() {
    for closer in [')', ']', '}', '"', '\''] {
        let mut editor = Editor::test_with_text(&closer.to_string());
        editor.insert_char('x');
        editor.undo();
        let version = editor.content_version;
        editor.insert_char(closer);
        assert_eq!(editor.cursor_col, 1);
        assert_eq!(editor.content_version, version, "closer={closer}");
        assert!(!editor.modified);
        assert!(!editor.can_undo());
        assert!(editor.can_redo());
        editor.redo();
        assert_eq!(editor.rope.to_string(), format!("x{closer}"));
    }
}

#[test]
fn closing_bracket_dedent_is_modified_and_undoable() {
    for (prefix, tabs, expected) in [("    ", false, "}"), ("\t", true, "}"), (" \t", false, "}")] {
        let source = format!("{prefix}}}");
        let mut editor = Editor::test_with_text(&source);
        editor.tab_size = 4;
        editor.use_tabs = tabs;
        editor.cursor_col = prefix.chars().count();
        editor.insert_char('}');
        assert_eq!(editor.rope.to_string(), expected);
        assert!(
            editor.modified,
            "dedenting an existing closer changes the document"
        );
        editor.undo();
        assert_eq!(editor.rope.to_string(), source);
        assert!(!editor.modified);
        editor.redo();
        assert_eq!(editor.rope.to_string(), expected);
        assert!(editor.modified);
    }
}

#[test]
fn paste_at_a_virtual_column_stays_on_that_line() {
    for plain in [false, true] {
        for source in ["abc\r\nnext", "abc"] {
            let mut editor = Editor::test_with_text(source);
            editor.cursor_col = 100;
            if plain {
                editor.paste_plain_with_text("界");
            } else {
                editor.paste_with_text("界");
            }
            assert_eq!(editor.rope.to_string(), source.replacen("abc", "abc界", 1));
            assert_eq!((editor.cursor_line, editor.cursor_col), (0, 4));
            editor.undo();
            assert_eq!(editor.rope.to_string(), source);
        }
    }
}

#[test]
fn collapsed_shift_selection_does_not_swallow_the_next_edit() {
    for operation in [0, 1, 2, 3] {
        let mut editor = Editor::test_with_text("abc");
        editor.cursor_col = 1;
        editor.start_selection();
        editor.move_right();
        editor.move_left();
        assert!(!editor.has_selection());
        match operation {
            0 => editor.insert_char_replacing_selection('('),
            1 => editor.backspace_replacing_selection(),
            2 => editor.delete_replacing_selection(),
            _ => editor.paste_plain_with_text("界"),
        }
        let expected = ["a(bc", "bc", "ac", "a界bc"][operation];
        assert_eq!(editor.rope.to_string(), expected);
        editor.undo();
        assert_eq!(editor.rope.to_string(), "abc");
        assert!(!editor.can_undo());
        editor.redo();
        assert_eq!(editor.rope.to_string(), expected);
    }
}

#[test]
fn duplicate_tabs_share_undo_redo_and_branch_history() {
    use crate::app::{App, Tab};
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text("base");
    editor.file_path = Some("shared-edit-test.rs".into());
    app.tabs = vec![Tab::new_file(editor)];
    app.tabs[0].editor.paste_plain_with_text("first ");
    app.duplicate_tab(0);
    assert_eq!(app.active_tab, 1);
    app.tabs[1].editor.undo();
    assert_eq!(
        app.tabs[1].editor.rope.to_string(),
        "base",
        "duplicate inherits existing history"
    );
    app.sync_duplicate_file_tabs();
    app.active_tab = 0;
    app.tabs[0].editor.redo();
    assert_eq!(app.tabs[0].editor.rope.to_string(), "first base");
    app.sync_duplicate_file_tabs();
    app.tabs[0].editor.paste_plain_with_text("second ");
    app.sync_duplicate_file_tabs();
    app.active_tab = 1;
    app.tabs[1].editor.undo();
    assert_eq!(app.tabs[1].editor.rope.to_string(), "first base");
    app.tabs[1].editor.undo();
    assert_eq!(app.tabs[1].editor.rope.to_string(), "base");
    app.tabs[1].editor.paste_plain_with_text("branch ");
    app.sync_duplicate_file_tabs();
    assert!(
        !app.tabs[0].editor.can_redo(),
        "editing a branch invalidates redo in every view"
    );
    app.active_tab = 0;
    app.tabs[0].editor.undo();
    assert_eq!(app.tabs[0].editor.rope.to_string(), "base");
}

#[test]
fn switching_panes_before_a_frame_preserves_edits_and_saved_history() {
    use crate::app::{App, Tab};
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text("base");
    editor.file_path = Some("shared-pane-test.rs".into());
    app.tabs = vec![Tab::new_file(editor)];
    app.duplicate_tab(0);
    app.create_split_group(1);
    let first_group = app.active_group;
    let other_group = 1 - first_group;
    app.editor_mut().unwrap().paste_plain_with_text("one ");
    app.switch_group(other_group);
    assert_eq!(app.editor().unwrap().rope.to_string(), "one base");
    app.editor_mut().unwrap().paste_plain_with_text("two ");
    let saved = app.editor().unwrap().rope.clone();
    app.editor_mut().unwrap().record_saved_snapshot(saved);
    app.switch_group(first_group);
    assert!(!app.editor().unwrap().modified);
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope.to_string(), "one base");
    assert!(app.editor().unwrap().modified);
    app.switch_group(other_group);
    app.editor_mut().unwrap().redo();
    assert_eq!(app.editor().unwrap().rope.to_string(), "two one base");
    assert!(!app.editor().unwrap().modified);
    app.editor_mut().unwrap().undo();
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope.to_string(), "base");
    assert!(
        app.editor().unwrap().modified,
        "original contents are no longer the saved checkpoint"
    );
}

#[test]
fn keyboard_and_mouse_tab_switches_flush_edits_before_the_next_frame() {
    use crate::app::{ActivePane, App, Tab};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    for switch in 0..3 {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text("base");
        editor.file_path = Some("shared.rs".into());
        app.tabs = vec![Tab::new_file(editor)];
        app.active_pane = ActivePane::Editor;
        app.duplicate_tab(0);
        app.editor_mut().unwrap().paste_plain_with_text("edit ");
        match switch {
            0 => {
                crate::input_keyboard::handle_keyboard_event(
                    &mut app,
                    KeyEvent::new(KeyCode::PageUp, KeyModifiers::CONTROL),
                );
            }
            1 => {
                app.tab_switcher_active = true;
                app.tab_mru_order = vec![1, 0];
                app.tab_switcher_selected = 1;
                crate::input_keyboard::handle_keyboard_event(
                    &mut app,
                    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                );
            }
            _ => {
                assert!(crate::mouse::handle_tab_click(
                    &mut app,
                    6,
                    ratatui::layout::Rect::new(0, 0, 150, 1)
                ));
            }
        }
        assert_eq!(app.active_tab, 0, "switch={switch}");
        assert_eq!(app.editor().unwrap().rope.to_string(), "edit base");
        app.execute_palette_command("undo");
        assert_eq!(app.editor().unwrap().rope.to_string(), "base");
        app.execute_palette_command("redo");
        assert_eq!(app.editor().unwrap().rope.to_string(), "edit base");
    }
}

#[test]
fn synchronized_history_reuses_existing_cursor_storage_and_handles_history_limit() {
    let mut source = Editor::test_with_text("ab\n");
    let mut peer = Editor::test_with_text("ab\n");
    source.extra_cursors = vec![(1, 0)];
    source.extra_cursor_anchors = vec![None];
    source.multi_cursor_insert_char('x');
    peer.sync_undo_history_from(&source);
    let first = peer.undo_stack.front().unwrap().extra_cursors.as_ptr();
    source.extra_cursors.clear();
    source.extra_cursor_anchors.clear();
    for index in 0..510 {
        source.paste_plain_with_text("y");
        peer.sync_undo_history_from(&source);
        if index < 499 {
            assert_eq!(
                peer.undo_stack.front().unwrap().extra_cursors.as_ptr(),
                first
            );
        }
        assert_eq!(source.undo_history_key(), peer.undo_history_key());
        for (a, b) in source.undo_stack.read().iter().zip(peer.undo_stack.read().iter()) {
            assert!(a.rope.is_instance(&b.rope));
            assert_eq!(a.modified, b.modified);
        }
    }
    for _ in 0..10 {
        source.undo();
    }
    peer.sync_undo_history_from(&source);
    assert_eq!(source.undo_history_key(), peer.undo_history_key());
    source.paste_plain_with_text("new branch");
    peer.sync_undo_history_from(&source);
    assert!(!peer.can_redo());
    assert_eq!(source.undo_history_key(), peer.undo_history_key());
}

#[test]
fn undo_redo_cancel_snippet_stops_from_a_different_document_version() {
    let mut editor = Editor::test_with_text("fn");
    editor.file_path = Some("snippet.rs".into());
    editor.cursor_col = 2;
    assert!(editor.try_expand_snippet());
    assert!(editor.snippet_active);
    editor.undo();
    assert_eq!(editor.rope.to_string(), "fn");
    assert!(
        !editor.snippet_next_tab_stop(),
        "Undo must not leave phantom snippet positions"
    );
    editor.redo();
    assert!(!editor.snippet_active);
    editor.insert_char_replacing_selection('x');
    editor.undo();
    editor.redo();
    assert!(editor.cursor_line < editor.line_count());
    assert!(editor.cursor_col <= editor.line_len(editor.cursor_line));
}

#[test]
fn mixed_edit_paste_selection_and_history_sequences_match_a_plain_text_model() {
    #[derive(Clone)]
    struct State {
        text: String,
        cursor: (usize, usize),
        anchor: Option<(usize, usize)>,
        modified: bool,
    }
    fn offset(text: &str, (line, col): (usize, usize)) -> usize {
        text.split_inclusive('\n')
            .take(line)
            .map(|s| s.chars().count())
            .sum::<usize>()
            + col
    }
    fn position(text: &str, offset: usize) -> (usize, usize) {
        text.chars().take(offset).fold((0, 0), |(line, col), ch| {
            if ch == '\n' {
                (line + 1, 0)
            } else {
                (line, col + 1)
            }
        })
    }
    fn pick(text: &str, value: usize) -> (usize, usize) {
        let lines: Vec<_> = text.split('\n').collect();
        let line = value % lines.len();
        let col = (value / lines.len()) % (lines[line].trim_end_matches('\r').chars().count() + 1);
        (line, col)
    }
    for seed in 1..=16 {
        let initial = "α\tcode\r\nnext😀\n";
        let mut editor = Editor::test_with_text(initial);
        let mut current = State {
            text: initial.into(),
            cursor: (0, 0),
            anchor: None,
            modified: false,
        };
        let mut undo: Vec<State> = Vec::new();
        let mut redo: Vec<State> = Vec::new();
        let mut clipboard = String::new();
        let mut saved_text: Option<String> = None;
        let mut random = seed as u64;
        for step in 0..300 {
            random = random
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let action = (random >> 32) as usize % 10;
            if action < 5 || action == 9 {
                editor.break_undo_group();
                current.cursor = pick(&current.text, (random >> 12) as usize);
                current.anchor =
                    (step % 3 != 0).then(|| pick(&current.text, (random >> 36) as usize));
                (editor.cursor_line, editor.cursor_col) = current.cursor;
                editor.selection_anchor = current.anchor;
                let caret = offset(&current.text, current.cursor);
                let anchor = current.anchor.map_or(caret, |p| offset(&current.text, p));
                let (start, end) = (caret.min(anchor), caret.max(anchor));
                if action == 9 {
                    clipboard = current.text.chars().skip(start).take(end - start).collect();
                    assert_eq!(editor.selected_text().unwrap_or_default(), clipboard);
                } else {
                    let payload = match action {
                        0 => "界\r\n\tβ".to_owned(),
                        1 => clipboard.clone(),
                        2 => "q".to_owned(),
                        3 => String::new(),
                        _ => "xy".to_owned(),
                    };
                    let deletes = action == 3;
                    if deletes {
                        editor.delete_selection();
                    } else if action == 2 {
                        editor.insert_char_replacing_selection('q');
                    } else {
                        editor.paste_plain_with_text(&payload);
                    }
                    if !payload.is_empty() || (deletes && start != end) {
                        undo.push(current.clone());
                        redo.clear();
                        let normalized = payload.replace("\r\n", "\n").replace('\r', "\n");
                        let mut chars: Vec<_> = current.text.chars().collect();
                        chars.splice(start..end, normalized.chars());
                        current.text = chars.into_iter().collect();
                        current.cursor =
                            position(&current.text, start + normalized.chars().count());
                        current.anchor = None;
                        current.modified = true;
                    } else if deletes {
                        current.anchor = None;
                    }
                }
            } else if action == 5 {
                editor.undo();
                if let Some(previous) = undo.pop() {
                    redo.push(current);
                    current = previous;
                    if let Some(saved) = &saved_text {
                        current.modified = current.text != *saved;
                    }
                }
            } else if action == 6 {
                editor.redo();
                if let Some(next) = redo.pop() {
                    undo.push(current);
                    current = next;
                    if let Some(saved) = &saved_text {
                        current.modified = current.text != *saved;
                    }
                }
            } else if action == 7 {
                editor.record_saved_snapshot(editor.rope.clone());
                saved_text = Some(current.text.clone());
                current.modified = false;
            } else {
                editor.paste_plain_with_text("");
            }
            assert_eq!(
                editor.rope.to_string(),
                current.text,
                "seed={seed} step={step} action={action}"
            );
            assert_eq!(
                (editor.cursor_line, editor.cursor_col),
                current.cursor,
                "seed={seed} step={step}"
            );
            assert_eq!(
                editor.selection_anchor, current.anchor,
                "seed={seed} step={step}"
            );
            assert_eq!(
                editor.modified,
                current.modified,
                "checkpoint seed={seed} step={step}"
            );
            assert_eq!(
                editor.undo_stack.len(),
                undo.len(),
                "undo seed={seed} step={step}"
            );
            assert_eq!(
                editor.redo_stack.len(),
                redo.len(),
                "redo seed={seed} step={step}"
            );
        }
    }
}
