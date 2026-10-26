use crate::{
    app::{App, Tab},
    editor::Editor,
};
use std::{fs, path::Path, time::Duration};

fn app_with_file(path: &Path, text: &str) -> App {
    fs::write(path, text).unwrap();
    let mut editor = Editor::new();
    editor.open_file(path).unwrap();
    let mut app = App::new(path.parent().unwrap().into());
    app.tabs = vec![Tab::new_file(editor)];
    app.format_on_save = false;
    app.trim_trailing_on_save = false;
    app.insert_final_newline = false;
    app
}

fn finish(app: &mut App) {
    app.wait_for_background(Duration::from_secs(10), |app| {
        !app.background.has_active_jobs() && !app.background.has_pending_results()
    });
}

#[test]
fn failed_save_as_preserves_original_filename_checkpoint_and_undo() {
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("original.txt");
    let target = dir.path().join("copy.txt");
    let mut app = app_with_file(&original, "original\n");
    let identity = app.editor().unwrap().save_identity;
    app.editor_mut().unwrap().insert_char('界');
    app.editor_mut().unwrap().set_encoding("ASCII");
    app.save_as_active = true;
    app.save_as_query = target.display().to_string();
    app.execute_save_as();
    finish(&mut app);
    assert!(
        app.save_error.is_some(),
        "ASCII cannot encode this document"
    );
    assert!(!target.exists());
    assert_eq!(fs::read_to_string(&original).unwrap(), "original\n");
    let editor = app.editor_mut().unwrap();
    assert_eq!(editor.file_path.as_ref(), Some(&original));
    assert_eq!(editor.save_identity, identity);
    assert_eq!(editor.saved_rope.as_ref().unwrap(), "original\n");
    assert_eq!(editor.rope, "界original\n");
    editor.set_encoding("UTF-8");
    editor.undo();
    assert_eq!(editor.rope, "original\n");
    assert!(!editor.modified);
    editor.redo();
    app.save_current_file();
    finish(&mut app);
    assert_eq!(fs::read_to_string(&original).unwrap(), "界original\n");
}

#[test]
fn save_as_does_not_replace_another_open_documents_unsaved_edits() {
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("original.txt");
    let target = dir.path().join("target.txt");
    let mut app = app_with_file(&original, "source\n");
    fs::write(&target, "target\n").unwrap();
    let mut other = Editor::new();
    other.open_file(&target).unwrap();
    other.insert_char('Y');
    app.tabs.push(Tab::new_file(other));
    app.editor_mut().unwrap().insert_char('X');
    app.save_as_active = true;
    app.save_as_query = target.display().to_string();
    app.execute_save_as();
    app.execute_save_as(); // Even confirming disk replacement must not discard another buffer.
    finish(&mut app);
    app.sync_duplicate_file_tabs();
    assert_eq!(app.tabs[1].editor.rope, "Ytarget\n");
    assert_eq!(app.tabs[0].editor.rope, "Xsource\n");
    assert_eq!(app.tabs[0].editor.file_path.as_ref(), Some(&original));
    assert_eq!(fs::read_to_string(&target).unwrap(), "target\n");
    assert!(app.save_as_active && app.modal.error.is_some());
}

#[test]
fn save_as_commits_shared_views_after_writing_and_keeps_later_edits_undoable() {
    for untitled in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.txt");
        let target = dir.path().join("copy.txt");
        let mut app = app_with_file(&original, "original\n");
        if untitled {
            app.tabs[0].editor = Editor::new();
            app.editor_mut()
                .unwrap()
                .paste_plain_with_text("original\n");
            app.editor_mut().unwrap().cursor_col = 0;
            app.editor_mut().unwrap().cursor_line = 0;
        }
        app.editor_mut().unwrap().insert_char('X');
        let main = app.desks.active;
        let other = app.new_desk("Copy", true);
        let identity = app.editor().unwrap().save_identity;
        app.save_as_active = true;
        app.save_as_query = target.display().to_string();
        app.execute_save_as();
        assert!(app.pending_save_as.contains_key(&identity));
        assert_eq!(
            app.editor().unwrap().file_path.as_ref(),
            (!untitled).then_some(&original)
        );
        assert!(app.tab_operation_pending(&app.tabs[app.active_tab]));
        let count = app.tabs.len();
        app.close_tab(app.active_tab);
        app.open_file(&target);
        assert_eq!(
            app.tabs.len(),
            count,
            "pending Save As must not lose or fork its document"
        );
        app.editor_mut().unwrap().insert_char('Y');
        if !untitled {
            app.save_current_file();
        }
        app.switch_desk(main);
        finish(&mut app);
        assert!(app.pending_save_as.is_empty());
        assert_eq!(fs::read_to_string(&original).unwrap(), "original\n");
        assert_eq!(fs::read_to_string(&target).unwrap(), "Xoriginal\n");
        for desk in [main, other] {
            app.switch_desk(desk);
            let editor = app.editor_mut().unwrap();
            assert_eq!(editor.file_path.as_ref(), Some(&target));
            assert_eq!(editor.rope, "XYoriginal\n");
            assert!(editor.modified);
        }
        app.editor_mut().unwrap().undo();
        assert_eq!(app.editor().unwrap().rope, "Xoriginal\n");
        assert!(!app.editor().unwrap().modified);
        app.switch_desk(main);
        assert!(!app.editor().unwrap().modified);
        app.editor_mut().unwrap().redo();
        app.save_current_file();
        finish(&mut app);
        assert_eq!(fs::read_to_string(&target).unwrap(), "XYoriginal\n");
        assert!(app.save_error.is_none());
    }
}

#[test]
fn rejected_save_as_keeps_document_identity_and_dialog() {
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("original.txt");
    let target = dir.path().join("copy.txt");
    let mut app = app_with_file(&original, "original\n");
    app.editor_mut().unwrap().insert_char('X');
    let identity = app.editor().unwrap().save_identity;
    app.chat.rollback.mutating = true;
    app.save_as_active = true;
    app.save_as_query = target.display().to_string();
    app.execute_save_as();
    assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&original));
    assert_eq!(app.editor().unwrap().save_identity, identity);
    assert!(app.pending_save_as.is_empty() && app.pending_file_saves.is_empty());
    assert!(app.save_as_active && app.modal.error.is_some());
    assert!(!target.exists());
}

#[test]
fn failed_untitled_save_as_keeps_all_shared_views_untitled() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("copy.txt");
    let mut app = App::new(dir.path().into());
    app.tabs = vec![Tab::new_file(Editor::new())];
    app.editor_mut().unwrap().insert_char('界');
    app.editor_mut().unwrap().set_encoding("ASCII");
    let main = app.desks.active;
    let other = app.new_desk("Other", true);
    app.save_as_active = true;
    app.save_as_query = target.display().to_string();
    app.execute_save_as();
    finish(&mut app);
    assert!(app.save_error.is_some() && !target.exists());
    for desk in [main, other] {
        app.switch_desk(desk);
        let editor = app.editor().unwrap();
        assert!(editor.file_path.is_none() && editor.modified && editor.can_undo());
        assert_eq!(editor.rope, "界");
    }
}

#[test]
fn binary_placeholder_cannot_be_unlocked_and_reencoded_as_text() {
    for command in ["toggle_read_only", "read_only"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("binary.dat");
        let bytes = b"\0\x01\x02\xff\0\0";
        fs::write(&path, bytes).unwrap();
        let mut editor = Editor::new();
        editor.open_file(&path).unwrap();
        assert_eq!(editor.encoding, "Binary");
        assert!(editor.read_only);
        let mut app = App::new(dir.path().into());
        app.tabs = vec![Tab::new_file(editor)];
        app.format_on_save = false;
        if command == "read_only" {
            app.toggle_setting(command);
            app.toggle_setting(command);
        } else {
            app.execute_palette_command(command);
        }
        app.execute_editor_command("encoding_utf8");
        app.save_current_file();
        finish(&mut app);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(app.editor().unwrap().encoding, "Binary");
        assert!(app.editor().unwrap().read_only, "{command}");
    }
}

#[test]
fn loading_and_failed_open_messages_stay_read_only_after_preference_changes() {
    for loading in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unreadable.txt");
        let mut editor = Editor::new_loading(&path, 0);
        editor.bg_loading = loading;
        editor.load_failed = !loading;
        let original = editor.rope.clone();
        let mut app = App::new(dir.path().into());
        app.tabs = vec![Tab::new_file(editor)];
        app.execute_editor_command("toggle_read_only");
        app.toggle_setting("read_only");
        app.toggle_setting("read_only");
        app.execute_editor_command("encoding_ascii");
        app.execute_editor_command("delete_line");
        assert_eq!(app.editor().unwrap().rope, original);
        assert_eq!(app.editor().unwrap().encoding, "UTF-8");
        assert!(app.editor().unwrap().read_only);
        assert!(!app.editor().unwrap().modified);
    }
}

#[test]
fn fallback_rename_uses_character_offsets_and_is_one_undo_step() {
    for (old, new, original, expected) in [
        (
            "name",
            "renamed",
            "界 name + name\nname_suffix name",
            "界 renamed + renamed\nname_suffix renamed",
        ),
        (
            "变量",
            "résultat",
            "界 变量 + 变量\n变量_extra 变量",
            "界 résultat + résultat\n变量_extra résultat",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with_file(&dir.path().join("text.txt"), original);
        app.rename.symbol_old = old.into();
        app.rename.symbol_new = new.into();
        app.rename.symbol_active = true;
        app.execute_rename_symbol();
        let editor = app.editor_mut().unwrap();
        assert_eq!(editor.rope, expected);
        editor.undo();
        assert_eq!(editor.rope, original);
        assert!(!editor.modified && !editor.can_undo());
        editor.redo();
        assert_eq!(editor.rope, expected);
    }
}

#[test]
fn fallback_rename_respects_read_only_and_leaves_no_op_history_alone() {
    for read_only in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with_file(&dir.path().join("text.txt"), "name");
        app.editor_mut().unwrap().insert_char('X');
        app.editor_mut().unwrap().undo();
        app.editor_mut().unwrap().read_only = read_only;
        app.rename.symbol_old = if read_only { "name" } else { "absent" }.into();
        app.rename.symbol_new = "other".into();
        app.execute_rename_symbol();
        let editor = app.editor_mut().unwrap();
        assert_eq!(editor.rope, "name");
        assert!(!editor.modified && !editor.can_undo());
        assert!(editor.can_redo());
    }
}

#[test]
fn multicursor_line_edits_only_touch_selected_lines_including_extra_anchors() {
    for extension in ["rs", "html"] {
        for operation in ["indent", "dedent", "toggle", "add", "remove"] {
            for backwards in [false, true] {
                let prefix = if operation == "dedent" {
                    "    "
                } else if operation == "remove" && extension == "rs" {
                    "// "
                } else if operation == "remove" {
                    "<!-- "
                } else {
                    ""
                };
                let suffix = if operation == "remove" && extension == "html" {
                    " -->"
                } else {
                    ""
                };
                let lines: Vec<_> = (0..7).map(|n| format!("{prefix}line{n}{suffix}")).collect();
                let original = lines.join("\r\n");
                let mut editor = Editor::test_with_text(&original);
                editor.file_path = Some(format!("text.{extension}").into());
                editor.tab_size = 4;
                editor.use_tabs = false;
                editor.cursor_col = prefix.chars().count();
                let (cursor, anchor) = if backwards {
                    ((4, 0), (6, 0))
                } else {
                    ((6, 0), (4, 0))
                };
                editor.extra_cursors = vec![cursor];
                editor.extra_cursor_anchors = vec![Some(anchor)];
                let mut expected = lines.clone();
                for i in [0, 4, 5] {
                    expected[i] = match operation {
                        "indent" => format!("    {}", lines[i]),
                        "dedent" | "remove" => format!("line{i}"),
                        _ if extension == "html" => format!("<!-- {} -->", lines[i]),
                        _ => format!("// {}", lines[i]),
                    };
                }
                match operation {
                    "indent" => editor.indent_lines(),
                    "dedent" => editor.dedent_line(),
                    "toggle" => editor.toggle_line_comment(),
                    "add" => editor.add_line_comment(),
                    _ => editor.remove_line_comment(),
                }
                assert_eq!(
                    editor.rope,
                    expected.join("\r\n"),
                    "{extension} {operation} backwards={backwards}"
                );
                editor.undo();
                assert_eq!(editor.rope, original);
                assert_eq!(editor.extra_cursors, [cursor]);
                assert_eq!(editor.extra_cursor_anchors, [Some(anchor)]);
                assert!(!editor.modified && !editor.can_undo());
                editor.redo();
                assert_eq!(editor.rope, expected.join("\r\n"));
            }
        }
    }
}

#[test]
fn overlapping_multicursor_line_ranges_are_edited_once_and_do_not_scan_gaps() {
    let mut editor = Editor::test_with_text(&"line\n".repeat(10_001));
    editor.selection_anchor = Some((2, 0));
    editor.extra_cursors = vec![(2, 4), (10_000, 4), (2, 1)];
    editor.extra_cursor_anchors = vec![Some((1, 1)), None, None];
    let visited: Vec<_> = editor
        .line_prefix_edit_lines()
        .map(|(line, _, _)| line)
        .collect();
    assert_eq!(visited, [0, 1, 2, 10_000]);
    editor.toggle_line_comment();
    for line in [0, 1, 2, 10_000] {
        assert_eq!(editor.rope.line(line), "// line\n");
    }
    assert_eq!(editor.rope.line(3), "line\n");
    assert_eq!(editor.rope.line(9_999), "line\n");
    editor.undo();
    assert!(!editor.modified);
}
