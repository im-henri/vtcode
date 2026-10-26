use crate::{
    app::{App, Tab},
    editor::Editor,
};
use std::{fs, time::Duration};

#[test]
fn save_and_save_as_never_write_loading_binary_or_failed_open_messages() {
    for state in ["loading", "failed", "binary"] {
        for save_as in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let source = dir.path().join("source.txt");
            let target = if save_as {
                dir.path().join("copy.txt")
            } else {
                source.clone()
            };
            let mut editor = Editor::new_loading(&source, 0);
            if state != "loading" {
                editor.bg_loading = false;
            }
            if state == "failed" {
                editor.disk_unavailable = true;
                editor.load_failed = true;
                editor.rope = ropey::Rope::from_str("Error loading file: access denied");
            }
            if state == "binary" {
                editor.encoding = "Binary";
                editor.rope = ropey::Rope::from_str("Binary file - cannot display");
            }
            let identity = editor.save_identity;
            let mut app = App::new(dir.path().into());
            app.tabs = vec![Tab::new_file(editor)];
            app.format_on_save = false;
            if save_as {
                app.save_as_active = true;
                app.save_as_query = target.display().to_string();
                app.execute_save_as();
            } else {
                app.save_current_file();
            }
            app.wait_for_background(Duration::from_secs(10), |app| {
                !app.background.has_active_jobs() && !app.background.has_pending_results()
            });
            assert!(
                !target.exists(),
                "{state}, save_as={save_as}: must not write UI messages"
            );
            let editor = app.editor().unwrap();
            assert_eq!(editor.file_path.as_ref(), Some(&source));
            assert_eq!(editor.save_identity, identity);
            assert!(!editor.modified);
            assert!(
                editor
                    .background_save_request(false, false, false)
                    .is_none()
            );
            if save_as {
                assert!(app.save_as_active && app.modal.error.is_some());
            }
            assert!(
                app.editor_mut()
                    .unwrap()
                    .save_file_with_options(false, false)
                    .is_err()
            );
            assert!(!target.exists());
        }
    }
}

#[test]
fn save_as_still_copies_real_text_from_read_only_or_missing_source_files() {
    for missing in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("original.txt");
        let target = dir.path().join("copy.txt");
        let mut editor = Editor::test_with_text("real document\n");
        editor.file_path = Some(source.clone());
        editor.read_only = true;
        editor.disk_unavailable = missing;
        if !missing {
            fs::write(&source, "real document\n").unwrap();
        }
        let mut app = App::new(dir.path().into());
        app.tabs = vec![Tab::new_file(editor)];
        app.save_as_active = true;
        app.save_as_query = target.display().to_string();
        app.execute_save_as();
        app.wait_for_background(Duration::from_secs(10), |app| {
            !app.background.has_active_jobs() && !app.background.has_pending_results()
        });
        assert_eq!(fs::read_to_string(&target).unwrap(), "real document\n");
        assert_eq!(source.exists(), !missing);
        assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&target));
        assert!(!app.editor().unwrap().modified);
        assert!(app.save_error.is_none());
    }
}

#[test]
fn retrying_failed_open_recovers_shared_views_without_undoing_to_error_text() {
    for read_only in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recovered.txt");
        let mut editor = Editor::new_loading(&path, 0);
        editor.bg_loading = false;
        editor.load_failed = true;
        editor.disk_unavailable = true;
        editor.rope = ropey::Rope::from_str("Error loading file: permission denied");
        let mut other = Editor::new();
        other.copy_document_from(&editor);
        assert!(other.save_content_error().is_some());
        let mut app = App::new(dir.path().into());
        app.default_read_only = read_only;
        app.tabs = vec![Tab::new_file(editor), Tab::new_file(other)];
        fs::write(&path, "recovered text\n").unwrap();
        let targets = app
            .tabs
            .iter()
            .map(|tab| (tab.editor.navigation_id, tab.editor.content_version))
            .collect();
        app.background
            .sender
            .send(crate::background::BackgroundResult::FileReloaded {
                workspace_generation: app.workspace_generation,
                path: path.clone(),
                targets,
                result: Ok(crate::editor::load_text_file(&path).unwrap()),
            })
            .unwrap();
        app.wait_for_background(Duration::from_secs(10), |app| {
            !app.background.has_active_jobs() && !app.background.has_pending_results()
        });
        for tab in &mut app.tabs {
            tab.editor.refresh_document();
            assert_eq!(tab.editor.rope, "recovered text\n");
            assert!(tab.editor.save_content_error().is_none());
            assert_eq!(tab.editor.read_only, read_only);
            assert!(
                !tab.editor.can_undo(),
                "the open-error message is not document history"
            );
            assert!(!tab.editor.modified);
        }
    }
}

#[test]
fn duplicate_selection_up_clamps_virtual_columns_and_preserves_undo_redo() {
    for (source, duplicated) in [("a\nb", "a\nb\nb"), ("界\r\nβ", "界\r\nβ\r\nβ")] {
        for backwards in [false, true] {
            let mut editor = Editor::test_with_text(source);
            editor.record_saved_snapshot(editor.rope.clone());
            let (anchor, cursor) = if backwards {
                ((1, 50), (0, 50))
            } else {
                ((0, 50), (1, 50))
            };
            editor.selection_anchor = Some(anchor);
            (editor.cursor_line, editor.cursor_col) = cursor;
            let selected = editor.selected_text();
            editor.duplicate_line_up();
            assert_eq!(editor.rope, duplicated);
            assert_eq!(editor.selected_text(), selected);
            assert!(editor.cursor_col <= editor.line_len(editor.cursor_line));
            editor.undo();
            assert_eq!(editor.rope, source);
            assert!(!editor.modified && !editor.can_undo());
            editor.redo();
            assert_eq!(editor.rope, duplicated);
            assert_eq!(editor.selected_text(), selected);
            assert!(editor.modified);
        }
    }
}

#[cfg(unix)]
#[test]
fn save_preserves_executable_and_private_permissions() {
    use std::os::unix::fs::PermissionsExt;
    for mode in [0o755, 0o700, 0o640, 0o600] {
        for replace in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("script");
            fs::write(&path, "original\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            let mut editor = Editor::new();
            editor.open_file(&path).unwrap();
            editor.insert_char('X');
            let mut request = editor.background_save_request(false, false, false).unwrap();
            if replace {
                request.expected_disk = None;
            }
            crate::editor::write_background_save(&request).unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), "Xoriginal\n");
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                mode
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn save_follows_symlinks_without_replacing_them_and_remains_undoable() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("real.txt");
    let link = dir.path().join("link.txt");
    let chain = dir.path().join("chain.txt");
    fs::write(&path, "original\n").unwrap();
    symlink("real.txt", &link).unwrap();
    symlink("link.txt", &chain).unwrap();
    let mut editor = Editor::new();
    editor.open_file(&chain).unwrap();
    editor.insert_char('X');
    editor.save_file_with_options(false, false).unwrap();
    assert!(
        fs::symlink_metadata(&chain)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "Xoriginal\n");
    editor.undo();
    assert!(editor.modified);
    editor.save_file_with_options(false, false).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "original\n");
    editor.redo();
    assert!(editor.modified);
    assert_eq!(editor.rope, "Xoriginal\n");
}

#[cfg(unix)]
#[test]
fn saving_read_only_files_does_not_bypass_permissions_by_replacing_them() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("read-only.txt");
    fs::write(&path, "original\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
    let mut editor = Editor::new();
    editor.open_file(&path).unwrap();
    editor.insert_char('X');
    assert!(editor.save_file_with_options(false, false).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "original\n");
    assert_eq!(editor.rope, "Xoriginal\n");
    assert!(editor.modified && editor.can_undo());
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o444
    );
    assert_eq!(
        fs::read_dir(dir.path()).unwrap().count(),
        1,
        "no save temp file leaks"
    );
}

#[cfg(unix)]
#[test]
fn save_as_refuses_existing_and_dangling_links_without_touching_the_target() {
    use std::os::unix::fs::symlink;
    for dangling in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        let link = dir.path().join("link.txt");
        if !dangling {
            fs::write(&target, "original\n").unwrap();
        }
        symlink("target.txt", &link).unwrap();
        let mut editor = Editor::test_with_text("replacement\n");
        editor.file_path = Some(link.clone());
        let request = editor.background_save_request(false, false, true).unwrap();
        assert_eq!(
            crate::editor::write_background_save(&request)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        if dangling {
            assert!(!target.exists());
        } else {
            assert_eq!(fs::read_to_string(target).unwrap(), "original\n");
        }
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            if dangling { 1 } else { 2 }
        );
    }
}

#[cfg(unix)]
#[test]
fn symlink_save_still_checks_conflicts_and_read_only_targets() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    for read_only in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        let link = dir.path().join("link.txt");
        fs::write(&target, "original\n").unwrap();
        symlink("target.txt", &link).unwrap();
        let mut editor = Editor::new();
        editor.open_file(&link).unwrap();
        editor.insert_char('X');
        let disk_text = if read_only {
            "original\n"
        } else {
            "external\n"
        };
        if read_only {
            fs::set_permissions(&target, fs::Permissions::from_mode(0o444)).unwrap();
        } else {
            fs::write(&target, disk_text).unwrap();
        }
        assert!(editor.save_file_with_options(false, false).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), disk_text);
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(editor.rope, "Xoriginal\n");
        assert!(editor.modified && editor.can_undo());
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            2,
            "no save temp file leaks"
        );
    }
}
