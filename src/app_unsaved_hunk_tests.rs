use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};

fn unsaved_app(saved: &str, current: &str) -> App {
    let mut editor = Editor::test_with_text(saved);
    editor.saved_rope = Some(editor.rope.clone());
    editor.file_path = Some("unsaved.rs".into());
    editor.save_undo_public();
    editor.rope = ropey::Rope::from_str(current);
    editor.modified = true;
    editor.mark_syntax_dirty_range(0, editor.line_count());
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.active_pane = crate::app::ActivePane::Editor;
    app.show_minimap = false;
    app.sticky_scroll = false;
    app
}

fn open(app: &mut App, line: usize) {
    let file = app.editor().unwrap().file_path.clone();
    app.open_hunk_popup(10, 4, file, line, 'N');
    assert!(app.hunk_peek.is_some(), "{:?}", app.notification_history);
}

#[test]
fn hunk_reverts_in_every_order_preserve_other_changes_and_round_trip_history() {
    let orders = [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]];
    for ending in ["\n", "\r\n", "\r"] {
        for last_newline in [false, true] {
            for kind in ["replacement", "insertion", "deletion"] {
                for order in orders {
                    let old = [format!("old α{ending}"), format!("old β{ending}"), format!("old 界{}", if last_newline { ending } else { "" })];
                    let mut parts = [format!("new α{ending}"), match kind {
                        "insertion" => format!("old β{ending}inserted{ending}"),
                        "deletion" => String::new(),
                        _ => format!("new β{ending}"),
                    }, format!("new 界{}", if last_newline { ending } else { "" })];
                    let join = |parts: &[String; 3]| format!("header{ending}{}keep a{ending}keep b{ending}{}keep c{ending}keep d{ending}{}", parts[0], parts[1], parts[2]);
                    let saved = join(&old);
                    let original = join(&parts);
                    let mut app = unsaved_app(&saved, &original);
                    app.editor_mut().unwrap().undo_stack.clear();
                    let mut remaining = vec![0, 1, 2];
                    let mut versions = vec![original];
                    for which in order {
                        let current = join(&parts);
                        let line = (0..app.editor().unwrap().line_count()).find(|&line| {
                            crate::ui_hunk_peek::HunkPeek::from_texts(&saved, &current, line, 4).is_ok()
                        }).expect("a remaining hunk must be reachable");
                        open(&mut app, line);
                        assert_eq!(app.hunk_peek.as_ref().unwrap().hunks.len(), remaining.len());
                        app.hunk_peek.as_mut().unwrap().selected = remaining.iter().position(|&index| index == which).unwrap();
                        app.revert_unsaved_hunk().unwrap();
                        parts[which] = old[which].clone();
                        remaining.retain(|&index| index != which);
                        let expected = join(&parts);
                        assert_eq!(app.editor().unwrap().rope, expected, "{ending:?}/{kind}/{order:?}");
                        versions.push(expected);
                    }
                    let editor = app.editor_mut().unwrap();
                    assert_eq!(editor.rope, saved);
                    assert!(!editor.modified);
                    for _ in 0..3 {
                        for expected in versions[..3].iter().rev() {
                            editor.undo();
                            assert_eq!(editor.rope, expected.as_str());
                            assert!(editor.modified);
                        }
                        for (index, expected) in versions[1..].iter().enumerate() {
                            editor.redo();
                            assert_eq!(editor.rope, expected.as_str());
                            assert_eq!(editor.modified, index != 2);
                        }
                    }
                }
            }
        }
    }
}

fn keyboard(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, modifiers));
}

fn draw(app: &mut App) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(120, 60)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui_editor::draw_editor(frame, app, Rect::new(10, 3, 100, 48));
            crate::ui_hunk_peek::draw(frame, app);
        })
        .unwrap();
    terminal
}

#[test]
fn unsaved_minus_click_reverts_the_clicked_hunk_and_preserves_the_other() {
    let saved = "a\nb\nc\nd\ne\nf\ng\nh\ni\n";
    let current = "a\nB\nc\nd\ne\nf\ng\nH\ni\n";
    let mut app = unsaved_app(saved, current);
    open(&mut app, 1);
    assert_eq!(app.hunk_peek.as_ref().unwrap().selected, 0);
    let terminal = draw(&mut app);
    let panel = app.hunk_popup_rect(120, 60);
    let mut minus = Vec::new();
    for row in panel.y + 2..panel.bottom() - 1 {
        for col in panel.x..panel.right() {
            let symbol = terminal.backend().buffer()[(col, row)].symbol();
            assert_ne!(symbol, "+", "unsaved hunks do not offer staging");
            if symbol == "−" {
                minus.push((col, row));
            }
        }
    }
    assert_eq!(minus.len(), 2);
    let (column, row) = minus[1];
    app.mouse_col = column;
    app.mouse_row = row;
    let terminal = draw(&mut app);
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(text.contains("Revert Unsaved Hunk"));
    crate::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        },
    );
    assert!(!app.hunk_popup_active);
    assert_eq!(app.editor().unwrap().rope, "a\nB\nc\nd\ne\nf\ng\nh\ni\n");
    assert!(app.editor().unwrap().modified);
    keyboard(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(app.editor().unwrap().rope, current);
    keyboard(&mut app, KeyCode::Char('y'), KeyModifiers::CONTROL);
    assert_eq!(app.editor().unwrap().rope, "a\nB\nc\nd\ne\nf\ng\nh\ni\n");
}

#[test]
fn unsaved_hunk_revert_preserves_exact_line_endings_unicode_and_eof() {
    for (saved, current, line) in [
        ("", "added", 0),
        ("", "added\n", 0),
        ("removed", "", 0),
        ("removed\n", "", 0),
        ("first\nremoved\n", "first\n", 1),
        ("removed\nlast\n", "last\n", 0),
        ("first\n", "first\nadded\n", 1),
        ("last\n", "added\nlast\n", 0),
        ("text", "text\n", 0),
        ("text\n", "text", 0),
        ("first\r\n\told_界🙂\r\nlast", "first\r\n\tnew_é\r\nlast", 1),
        ("first\r\nold\nlast\r\n", "first\r\nnew\r\nlast\r\n", 1),
        ("first\rold\rlast\r", "first\rnew\rlast\r", 1),
        ("\n\n\n", "\n", 0),
    ] {
        let mut app = unsaved_app(saved, current);
        let baseline = app.editor().unwrap().saved_rope.as_ref().unwrap().clone();
        open(&mut app, line);
        assert_eq!(app.hunk_peek.as_ref().unwrap().hunks.len(), 1);
        let before = app.editor().unwrap().undo_stack.len();
        app.execute_hunk_popup_action(0);
        assert_eq!(
            app.editor().unwrap().rope,
            saved,
            "{current:?}: {:?}",
            app.notification_history
        );
        let editor = app.editor_mut().unwrap();
        assert_eq!(editor.undo_stack.len(), before + 1);
        assert!(editor.saved_rope.as_ref().unwrap().is_instance(&baseline));
        assert!(!editor.modified, "the last hunk was reverted: {current:?}");
        assert!(editor.rope.is_instance(&baseline));
        assert_eq!(editor.unsaved_line_status(editor.cursor_line), ' ');
        assert!(!editor.has_unsaved_change_in_range(0, editor.line_count()));
        for _ in 0..3 {
            editor.undo();
            assert_eq!(editor.rope, current);
            assert!(editor.modified);
            editor.redo();
            assert_eq!(editor.rope, saved);
            assert!(!editor.modified);
        }
        editor.undo();
        editor.undo();
        assert_eq!(editor.rope, saved);
        assert!(!editor.modified);
    }
}

#[test]
fn unsaved_hunk_revert_uses_saved_snapshot_without_disk_or_index_changes() {
    let index = "committed\n";
    let saved = "last saved\n";
    let current = "editing\n";
    let (root, file, git_app) = make_git_app_with_file("unsaved-revert", index, saved);
    for disk in [Some(saved), Some("external changes\n"), None] {
        match disk {
            Some(contents) => std::fs::write(&file, contents).unwrap(),
            None => std::fs::remove_file(&file).unwrap(),
        }
        let mut app = unsaved_app(saved, current);
        app.editor_mut().unwrap().file_path = Some(file.clone());
        open(&mut app, 0);
        app.execute_hunk_popup_action(0);
        assert_eq!(app.editor().unwrap().rope, saved);
        assert_eq!(std::fs::read_to_string(&file).ok().as_deref(), disk);
        assert_eq!(git_stdout(&root, &["show", ":file.txt"]), index);
        assert!(app.pending_file_saves.is_empty());
        assert!(!app.bg_git_action_running);
        app.editor_mut().unwrap().undo();
        assert_eq!(app.editor().unwrap().rope, current);
    }
    drop(git_app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn unsaved_hunk_revert_works_for_untitled_documents_from_the_keyboard() {
    for width in [100, 11] {
        let mut app = unsaved_app("", "new document\n");
        app.editor_mut().unwrap().file_path = None;
        open(&mut app, 0);
        app.gutter_area = Some(Rect::new(0, 0, 6, 20));
        app.code_area = Some(Rect::new(6, 0, width - 6, 20));
        let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
        terminal
            .draw(|frame| crate::ui_hunk_peek::draw(frame, &mut app))
            .unwrap();
        keyboard(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        keyboard(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!app.hunk_popup_active);
        assert_eq!(app.editor().unwrap().rope, "");
        assert!(!app.save_as_active);
        keyboard(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(app.editor().unwrap().rope, "new document\n");
    }
}

#[test]
fn unsaved_hunk_revert_rejects_stale_or_unwritable_previews_without_an_undo_step() {
    for change in [
        "edited",
        "replaced",
        "saved",
        "reopened",
        "renamed",
        "untitled",
        "read-only",
        "loading",
    ] {
        let mut app = unsaved_app("saved\n", "unsaved\n");
        if change == "untitled" {
            app.editor_mut().unwrap().file_path = None;
        }
        open(&mut app, 0);
        let editor = app.editor_mut().unwrap();
        match change {
            "edited" => editor.insert_char('x'),
            "replaced" => editor.rope = ropey::Rope::from_str("NEW TEXT\n"),
            "saved" => editor.record_saved_snapshot(editor.rope.clone()),
            "reopened" | "untitled" => {
                let path = editor.file_path.clone();
                *editor = Editor::test_with_text("replacement document\n");
                editor.file_path = path;
                editor.saved_rope = Some(editor.rope.clone());
            }
            "renamed" => editor.file_path = Some("renamed.rs".into()),
            "read-only" => editor.read_only = true,
            "loading" => editor.bg_loading = true,
            _ => unreachable!(),
        }
        let text = editor.rope.to_string();
        let history = editor.undo_stack.len();
        let version = editor.content_version;
        let dirty = editor.modified;
        app.execute_hunk_popup_action(0);
        let editor = app.editor().unwrap();
        assert_eq!(editor.rope, text, "{change}");
        assert_eq!(editor.undo_stack.len(), history, "{change}");
        assert_eq!(editor.content_version, version, "{change}");
        assert_eq!(editor.modified, dirty, "{change}");
        assert!(
            app.notification_history
                .iter()
                .any(|note| note.0.contains("changed since") || note.0.contains("read-only")),
            "{change}"
        );
    }
}

#[test]
fn unsaved_hunk_revert_targets_exact_deletion_boundaries_and_separate_changes() {
    for (saved, current, after, line) in [
        ("remove\nkeep\nold\n", "keep\nnew\n", "keep\nold\n", 0),
        (
            "a\nb\nc\nd\ne\nf\n",
            "a\nnew\nrows\nb\nc\ne\nf\n",
            "a\nnew\nrows\nb\nc\nd\ne\nf\n",
            1,
        ),
        ("a\rb\rc\rd\r", "a\rB\rc\rD\r", "a\rB\rc\rd\r", 1),
    ] {
        let mut app = unsaved_app(saved, current);
        open(&mut app, line);
        let peek = app.hunk_peek.as_mut().unwrap();
        assert_eq!(peek.hunks.len(), 2);
        peek.selected = 1;
        // Deliberately leave the old navigation line: the selected preview
        // hunk, not a potentially ambiguous deletion marker, is authoritative.
        app.execute_hunk_popup_action(0);
        assert_eq!(app.editor().unwrap().rope, after, "{current:?}");
        app.editor_mut().unwrap().undo();
        assert_eq!(app.editor().unwrap().rope, current);
    }
}

#[test]
fn unsaved_hunk_revert_and_undo_redo_are_shared_across_split_panes() {
    let mut app = unsaved_app("before\n", "after\n");
    app.duplicate_tab(0);
    app.create_split_group(1);
    let first = app.active_group;
    open(&mut app, 0);
    app.execute_hunk_popup_action(0);
    app.switch_group(1 - first);
    assert_eq!(app.editor().unwrap().rope, "before\n");
    keyboard(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(app.editor().unwrap().rope, "after\n");
    assert!(app.editor().unwrap().modified);
    app.switch_group(first);
    assert_eq!(app.editor().unwrap().rope, "after\n");
    keyboard(&mut app, KeyCode::Char('y'), KeyModifiers::CONTROL);
    assert_eq!(app.editor().unwrap().rope, "before\n");
}

#[test]
fn unsaved_hunk_revert_remaps_cursors_and_undo_restores_selections() {
    let mut app = unsaved_app("a\nb\nlast\n", "a\nchanged\nextra\nlast\n");
    let editor = app.editor_mut().unwrap();
    editor.cursor_line = 3;
    editor.cursor_col = 2;
    editor.selection_anchor = Some((1, 0));
    editor.extra_cursors = vec![(0, 1), (1, 3), (3, 4)];
    editor.extra_cursor_anchors = vec![None, Some((1, 0)), Some((2, 0))];
    editor.snippet_active = true;
    editor.folded_ranges.insert(1, 3);
    open(&mut app, 1);
    app.execute_hunk_popup_action(0);
    let editor = app.editor_mut().unwrap();
    assert_eq!((editor.cursor_line, editor.cursor_col), (2, 2));
    assert_eq!(editor.extra_cursors, [(0, 1), (1, 0), (2, 4)]);
    assert!(editor.extra_cursor_anchors.is_empty());
    assert!(editor.selection_anchor.is_none());
    assert!(!editor.snippet_active);
    assert!(editor.folded_ranges.is_empty());
    editor.undo();
    assert_eq!((editor.cursor_line, editor.cursor_col), (3, 2));
    assert_eq!(editor.selection_anchor, Some((1, 0)));
    assert_eq!(editor.extra_cursors, [(0, 1), (1, 3), (3, 4)]);
    assert_eq!(
        editor.extra_cursor_anchors,
        [None, Some((1, 0)), Some((2, 0))]
    );
}

#[test]
fn unsaved_hunk_revert_is_separate_from_typing_and_preserves_save_checkpoints() {
    let mut app = unsaved_app("a\nb\nc\nd\n", "a\nB\nc\nD\n");
    open(&mut app, 1);
    app.execute_hunk_popup_action(0);
    assert_eq!(app.editor().unwrap().rope, "a\nb\nc\nD\n");
    app.editor_mut().unwrap().insert_char('x');
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope, "a\nb\nc\nD\n");
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope, "a\nB\nc\nD\n");
    app.editor_mut().unwrap().redo();
    open(&mut app, 3);
    app.execute_hunk_popup_action(0);
    let editor = app.editor_mut().unwrap();
    assert_eq!(editor.rope, "a\nb\nc\nd\n");
    assert!(!editor.can_redo());
    assert!(
        !editor.modified,
        "reverting every hunk must not require a save"
    );
    editor.undo();
    assert_eq!(editor.rope, "a\nb\nc\nD\n");
    assert!(editor.modified);
    editor.undo();
    assert_eq!(editor.rope, "a\nB\nc\nD\n");
    assert!(editor.modified);
    editor.redo();
    editor.redo();
    assert_eq!(editor.rope, "a\nb\nc\nd\n");
    assert!(!editor.modified);
}

#[test]
fn unsaved_hunk_revert_keeps_non_text_changes_unsaved() {
    let mut app = unsaved_app("saved\n", "changed\n");
    app.editor_mut().unwrap().encoding = "UTF-16 LE";
    open(&mut app, 0);
    app.execute_hunk_popup_action(0);
    let editor = app.editor().unwrap();
    assert_eq!(editor.rope, "saved\n");
    assert_eq!(editor.encoding, "UTF-16 LE");
    assert!(editor.modified);
    assert_eq!(editor.unsaved_line_status(0), ' ');
    assert!(!editor.has_unsaved_change_in_range(0, editor.line_count()));
}

#[test]
fn unsaved_hunk_revert_during_a_pending_save_keeps_the_new_checkpoint() {
    let mut app = unsaved_app("saved\n", "pending save\n");
    let request = app
        .editor_mut()
        .unwrap()
        .background_save_request(false, false, false)
        .unwrap();
    open(&mut app, 0);
    app.execute_hunk_popup_action(0);
    let editor = app.editor_mut().unwrap();
    assert_eq!(editor.rope, "saved\n");
    editor.apply_background_save(
        request.source_content_version,
        &request.rope,
        request.rope.clone(),
        request.encoding,
        None,
        None,
        None,
        u64::MAX,
    );
    assert!(
        editor.modified,
        "the disk checkpoint is now the version captured before reverting"
    );
    editor.undo();
    assert_eq!(editor.rope, "pending save\n");
    assert!(!editor.modified);
    editor.redo();
    assert_eq!(editor.rope, "saved\n");
    assert!(editor.modified);
}

#[test]
fn unsaved_minus_restores_space_replaced_code_and_redraws_it_immediately() {
    let source = include_str!("../examples/rebuild_syntax_cache.rs");
    let mut app = unsaved_app(source, source);
    app.editor_mut().unwrap().undo();
    app.quick_suggestions = false;
    let editor = app.editor_mut().unwrap();
    let start = editor.rope.byte_to_char(source.find("Ok((count").unwrap());
    let end = editor
        .rope
        .byte_to_char(source.find("folder.display()").unwrap() + "folder.display()".len());
    let position = |offset| {
        let line = editor.rope.char_to_line(offset);
        (line, offset - editor.rope.line_to_char(line))
    };
    editor.selection_anchor = Some(position(start));
    (editor.cursor_line, editor.cursor_col) = position(end);
    keyboard(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
    let replaced = app.editor().unwrap().rope.clone();
    assert!(!replaced.to_string().contains("folder.display()"));
    let mut terminal = Terminal::new(TestBackend::new(160, 60)).unwrap();
    terminal
        .draw(|frame| crate::ui_editor::draw_editor(frame, &mut app, frame.area()))
        .unwrap();
    let gutter = app.gutter_area.unwrap();
    let editor = app.editor().unwrap();
    let row = gutter.y + (editor.cursor_line - editor.scroll_offset) as u16;
    crate::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: gutter.x,
            row,
            modifiers: KeyModifiers::NONE,
        },
    );
    assert_eq!(app.hunk_popup_status, 'N');
    assert!(app.hunk_peek.is_some());
    terminal
        .draw(|frame| {
            crate::ui_editor::draw_editor(frame, &mut app, frame.area());
            crate::ui_hunk_peek::draw(frame, &mut app);
        })
        .unwrap();
    let panel = app.hunk_popup_rect(160, 60);
    let minus = (panel.y + 2..panel.bottom() - 1)
        .find_map(|row| {
            (panel.x..panel.right())
                .find(|&col| terminal.backend().buffer()[(col, row)].symbol() == "−")
                .map(|col| (col, row))
        })
        .expect("the unsaved hunk has a minus button");
    crate::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: minus.0,
            row: minus.1,
            modifiers: KeyModifiers::NONE,
        },
    );
    assert_eq!(app.editor().unwrap().rope, source);
    terminal
        .draw(|frame| crate::ui_editor::draw_editor(frame, &mut app, frame.area()))
        .unwrap();
    let rendered: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(rendered.contains("Ok((count, fingerprint))"));
    assert!(rendered.contains("folder.display()"));
    assert!(!app.tabs[0].has_unsaved_changes());
    terminal
        .draw(|frame| crate::ui_chrome_tabs::draw_tab_bar(frame, &app, frame.area()))
        .unwrap();
    assert!(
        !terminal
            .backend()
            .buffer()
            .content
            .iter()
            .any(|cell| cell.symbol() == "●")
    );
    keyboard(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(app.editor().unwrap().rope, replaced);
    assert!(app.editor().unwrap().modified);
    keyboard(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(app.editor().unwrap().rope, source);
    assert!(!app.editor().unwrap().modified);
}

#[test]
fn reverting_all_hunks_clears_the_unsaved_state_in_both_split_panes_and_allows_close() {
    let mut app = unsaved_app("saved\n", "edited\n");
    app.duplicate_tab(0);
    app.create_split_group(1);
    let second = app.active_group;
    let first = 1 - second;
    open(&mut app, 0);
    app.execute_hunk_popup_action(0);
    assert!(!app.tabs[app.active_tab].has_unsaved_changes());
    app.switch_group(first);
    assert!(!app.tabs[app.active_tab].has_unsaved_changes());
    keyboard(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    app.switch_group(second);
    assert!(app.tabs[app.active_tab].has_unsaved_changes());
    keyboard(&mut app, KeyCode::Char('y'), KeyModifiers::CONTROL);
    app.switch_group(first);
    assert!(!app.tabs[app.active_tab].has_unsaved_changes());
    app.close_tab(app.active_tab);
    assert!(
        !app.confirm_close_active,
        "an unchanged buffer needs no discard confirmation"
    );
}
