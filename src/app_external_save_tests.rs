use crate::{
    app::{App, AutoSaveMode, Tab},
    background::{BackgroundResult, WorkspacePathChange},
    editor::Editor,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::{
    fs,
    path::Path,
    time::{Duration, Instant, UNIX_EPOCH},
};

fn app(path: &Path) -> App {
    fs::write(path, "original\n").unwrap();
    let mut editor = Editor::new();
    editor.open_file(path).unwrap();
    let mut app = App::new(path.parent().unwrap().into());
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.format_on_save = false;
    app.trim_trailing_on_save = false;
    app.insert_final_newline = false;
    app
}

fn send(app: &mut App, result: BackgroundResult) {
    app.background.sender.send(result).unwrap();
    app.poll_background_results();
}

fn finish_saves(app: &mut App) {
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.pending_file_saves.is_empty() && app.deferred_file_saves.is_empty()
    });
}

#[test]
fn unchanged_save_keeps_explorer_cache_and_does_not_queue_git_refreshes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.txt");
    let mut app = app(&path);
    app.explorer_line_count_cache.insert(path.clone(), 2);
    // A duplicate refresh would set these pending flags while a scan is running.
    app.bg_git_status_running = true;
    app.bg_git_diff_running = true;
    for _ in 0..3 {
        app.save_current_file();
        finish_saves(&mut app);
        assert!(app.save_error.is_none());
        assert!(!app.editor().unwrap().modified);
        assert_eq!(app.explorer_line_count_cache.get(&path), Some(&2));
        assert!(!app.bg_git_status_pending);
        assert!(!app.bg_git_diff_pending);
    }
}

fn saved_result(
    app: &App,
    request: crate::editor::BackgroundSaveRequest,
    saved: ropey::Rope,
) -> BackgroundResult {
    let metadata = fs::metadata(&request.path).unwrap();
    BackgroundResult::FileSaved {
        source_document_id: request.source_document_id,
        workspace_generation: app.workspace_generation,
        path: request.path,
        error: None,
        last_disk_modified: metadata.modified().ok(),
        file_size: Some(metadata.len()),
        source_content_version: request.source_content_version,
        source_rope: request.rope,
        previous_disk: request.expected_disk,
        saved_encoding: request.encoding,
        transformed_lines: None,
        disk_written: true,
        saved_rope: Some(saved),
    }
}

#[test]
fn external_save_conflict_is_visible_and_never_overwrites_in_all_save_paths() {
    for mode in ["save", "save_all", "focus", "window", "close", "quit"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        let mut app = app(&path);
        app.editor_mut().unwrap().insert_char('X');
        fs::write(&path, "external\n").unwrap();
        match mode {
            "save" => app.save_current_file(),
            "save_all" => {
                app.execute_palette_command("save_all");
            }
            "focus" => {
                app.auto_save = AutoSaveMode::OnFocusChange;
                app.auto_save_on_focus_change();
            }
            "window" => {
                app.auto_save = AutoSaveMode::OnWindowChange;
                app.auto_save_on_window_change();
            }
            "close" => {
                app.confirm_close_tab = 0;
                app.confirm_close_active = true;
                crate::input_pickers::handle_confirm_close_key(&mut app, KeyCode::Char('y'));
                assert!(app.confirm_close_active);
            }
            "quit" => {
                app.exit_confirm_active = true;
                assert_eq!(
                    crate::input_dialogs::handle_exit_confirm_key(
                        &mut app,
                        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)
                    ),
                    Some(false)
                );
                assert!(app.exit_confirm_active);
            }
            _ => unreachable!(),
        }
        finish_saves(&mut app);
        assert_eq!(
            crate::ui_modal::active(&app),
            Some(crate::ui_modal::Kind::SaveError),
            "{mode}"
        );
        assert!(app.save_conflict.is_some(), "{mode} must offer Overwrite and View Diff");
        assert!(
            app.save_error.as_ref().unwrap().contains("changed on disk"),
            "{mode}"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "external\n", "{mode}");
        assert_eq!(app.tabs[0].editor.rope, "Xoriginal\n", "{mode}");
        assert!(app.tabs[0].editor.modified && app.tabs[0].editor.can_undo());
        crate::ui_modal::handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.save_error.is_none());
        if mode == "close" {
            assert!(app.confirm_close_active);
        }
        if mode == "quit" {
            assert!(app.exit_confirm_active);
        }
    }
}

#[test]
fn external_save_conflict_dialog_can_be_read_without_opening_notifications() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(&dir.path().join("file.txt"));
    app.show_save_error("file.txt changed on disk. Save cancelled; your edits remain open.");
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 28)).unwrap();
    terminal
        .draw(|frame| crate::ui_modal::draw_active(frame, &mut app))
        .unwrap();
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(
        text.contains("Save failed")
            && text.contains("changed on disk")
            && text.contains("Dismiss")
    );
    assert!(!app.modal.close.is_empty());
}

#[test]
fn external_save_resolution_popup_has_two_actions_and_enter_safely_opens_the_diff() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.txt");
    let mut app = app(&path);
    app.editor_mut().unwrap().insert_char('X');
    fs::write(&path, "external\n").unwrap();
    app.save_current_file();
    finish_saves(&mut app);
    assert!(app.save_conflict.is_some());
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 28)).unwrap();
    terminal.draw(|frame| crate::ui_modal::draw_active(frame, &mut app)).unwrap();
    let text: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
    for label in ["File changed outside VTCode", "Overwrite", "View Diff", "lose the external changes"] {
        assert!(text.contains(label), "missing {label}: {text}");
    }
    assert_eq!(app.modal.buttons.len(), 2);
    assert_eq!(app.modal.focus, 1);
    crate::ui_modal::handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.pending_file_saves.is_empty());
    app.wait_for_background(Duration::from_secs(10), |app| app.save_error.is_none());
    assert!(matches!(app.tabs[app.active_tab].kind, crate::app::TabKind::Diff(_)));
    assert_eq!(app.compare_base_content.as_deref(), Some("external\n"));
    assert_eq!(app.editor().unwrap().rope, "Xoriginal\n");
    assert_eq!(fs::read_to_string(&path).unwrap(), "external\n");
    assert!(app.tabs[0].editor.modified && app.tabs[0].editor.can_undo());
}

#[test]
fn external_save_resolution_diff_uses_latest_disk_and_live_edits_without_focus_autosave() {
    for mode in [crate::app::DiffMode::Inline, crate::app::DiffMode::SideBySide] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("extensionless");
        let mut app = app(&path);
        app.preferred_diff_mode = mode;
        app.editor_mut().unwrap().language_override = Some("Python".into());
        app.editor_mut().unwrap().insert_char('X');
        fs::write(&path, "first outside version\n").unwrap();
        app.save_current_file();
        finish_saves(&mut app);
        app.editor_mut().unwrap().break_undo_group();
        app.editor_mut().unwrap().insert_char('Y');
        fs::write(&path, "latest outside version\n").unwrap();
        let source = app.editor().unwrap().rope.clone();
        app.auto_save = AutoSaveMode::OnFocusChange;
        app.auto_save_attempted_versions.clear();
        app.confirm_close_active = true;
        app.view_save_conflict();
        app.wait_for_background(Duration::from_secs(10), |app| app.save_error.is_none());
        assert!(app.diff_mode == mode);
        assert_eq!(app.compare_base_content.as_deref(), Some("latest outside version\n"));
        assert_eq!(app.editor().unwrap().rope, source);
        assert!(app.editor().unwrap().read_only);
        assert_eq!(app.editor().unwrap().language_name(), "Python");
        assert!(app.pending_file_saves.is_empty() && app.deferred_file_saves.is_empty());
        assert!(!app.confirm_close_active);
        assert_eq!(app.tabs[0].editor.rope, source);
        assert!(app.tabs[0].editor.modified);
        assert_eq!(fs::read_to_string(&path).unwrap(), "latest outside version\n");
        app.active_tab = 0;
        app.sync_diff_tab();
        app.editor_mut().unwrap().undo();
        assert_eq!(app.editor().unwrap().rope, "Xoriginal\n");
        app.editor_mut().unwrap().redo();
        assert_eq!(app.editor().unwrap().rope, source);
    }
}

#[test]
fn external_save_resolution_overwrite_saves_latest_edits_and_rebases_a_deferred_save() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.txt");
    let mut app = app(&path);
    app.editor_mut().unwrap().insert_char('X');
    fs::write(&path, "external\n").unwrap();
    app.save_current_file();
    finish_saves(&mut app);
    app.editor_mut().unwrap().break_undo_group();
    app.editor_mut().unwrap().insert_char('Y');
    let overwritten = app.editor().unwrap().rope.clone();
    app.overwrite_save_conflict();
    assert!(app.save_error.is_none());
    app.editor_mut().unwrap().insert_char('Z');
    app.save_current_file();
    let latest = app.editor().unwrap().rope.clone();
    finish_saves(&mut app);
    assert!(app.save_error.is_none(), "{:?}", app.save_error);
    assert_eq!(fs::read_to_string(&path).unwrap(), latest.to_string());
    assert!(!app.editor().unwrap().modified);
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope, overwritten);
    assert!(app.editor().unwrap().modified);
    app.editor_mut().unwrap().redo();
    assert_eq!(app.editor().unwrap().rope, latest);
    assert!(!app.editor().unwrap().modified);
}

#[test]
fn external_save_resolution_targets_original_document_and_rejects_closed_or_read_only_views() {
    for action in ["overwrite", "compare", "closed", "read_only"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        let mut app = app(&path);
        app.editor_mut().unwrap().insert_char('X');
        fs::write(&path, "external\n").unwrap();
        app.save_current_file();
        finish_saves(&mut app);
        if action == "closed" {
            app.tabs.remove(0);
            let mut replacement = Editor::new();
            replacement.open_file(&path).unwrap();
            app.tabs.push(Tab::new_file(replacement));
        } else if action == "read_only" { app.editor_mut().unwrap().set_read_only(true); }
        let other = dir.path().join("other.txt");
        fs::write(&other, "other\n").unwrap();
        let mut editor = Editor::new();
        editor.open_file(&other).unwrap();
        editor.insert_char('!');
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 1;
        if action == "compare" {
            app.view_save_conflict();
            app.wait_for_background(Duration::from_secs(10), |app| app.save_error.is_none());
            assert_eq!(app.editor().unwrap().rope, "Xoriginal\n");
        } else {
            app.overwrite_save_conflict();
            finish_saves(&mut app);
        }
        assert_eq!(fs::read_to_string(&other).unwrap(), "other\n");
        assert!(app.tabs[1].editor.modified);
        assert_eq!(fs::read_to_string(&path).unwrap(), if action == "overwrite" { "Xoriginal\n" } else { "external\n" });
        if matches!(action, "closed" | "read_only") {
            assert!(app.save_conflict.is_some() && app.modal.error.is_some());
        }
    }
}

#[test]
fn external_save_resolution_revalidates_disk_after_overwrite_preparation_and_supports_deleted_files() {
    for deleted in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        let mut app = app(&path);
        app.editor_mut().unwrap().insert_char('X');
        let mut request = app.editor().unwrap().background_save_request(false, false, false).unwrap();
        if deleted { fs::remove_file(&path).unwrap(); } else { fs::write(&path, "external\n").unwrap(); }
        crate::app_save_conflict::prepare_overwrite(&mut request, None).unwrap();
        assert_eq!(request.fail_if_exists, deleted);
        fs::write(&path, "new external version\n").unwrap();
        let error = crate::editor::write_background_save(&request).err().unwrap();
        assert!(crate::editor::save::is_conflict(&error) || error.kind() == std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&path).unwrap(), "new external version\n");
    }
}

#[test]
fn external_save_resolution_overwrite_checks_external_encoding() {
    for external in [b"\xEF\xBB\xBFexternal\n".as_slice(), b"\xFF\xFEe\0x\0t\0\n\0"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        let mut app = app(&path);
        app.editor_mut().unwrap().insert_char('X');
        fs::write(&path, external).unwrap();
        app.save_current_file();
        finish_saves(&mut app);
        assert!(app.save_conflict.is_some());
        app.overwrite_save_conflict();
        finish_saves(&mut app);
        assert!(app.save_error.is_none(), "external={external:?}, save_error={:?}, modal_error={:?}", app.save_error, app.modal.error);
        assert_eq!(fs::read(&path).unwrap(), b"Xoriginal\n");
        assert!(!app.editor().unwrap().modified);
    }
}

#[test]
fn external_save_resolution_cannot_recreate_a_file_recovered_to_untitled_after_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.txt");
    let mut app = app(&path);
    app.editor_mut().unwrap().insert_char('X');
    fs::write(&path, "external\n").unwrap();
    app.save_current_file();
    finish_saves(&mut app);
    assert!(app.save_conflict.is_some());
    fs::remove_file(&path).unwrap();
    send(&mut app, BackgroundResult::FileDiskState(WorkspacePathChange {
        path: path.clone(), observed_at: Instant::now(), exists: false, modified: None, len: None,
    }));
    assert!(app.editor().unwrap().file_path.is_none());
    app.overwrite_save_conflict();
    assert!(app.modal.error.is_some());
    assert!(app.pending_file_saves.is_empty());
    assert!(!path.exists());
    assert_eq!(app.editor().unwrap().rope.to_string(), "Xoriginal\n");
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope.to_string(), "original\n");
    assert!(app.editor().unwrap().modified);
}

#[test]
fn external_save_resolution_mouse_actions_and_small_popups_keep_cancel_available() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    for overwrite in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        let mut app = app(&path);
        app.editor_mut().unwrap().insert_char('X');
        fs::write(&path, "external\n").unwrap();
        app.save_current_file();
        finish_saves(&mut app);
        for (width, height) in [(1, 1), (16, 7), (42, 14), (100, 28)] {
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| crate::ui_modal::draw_active(frame, &mut app)).unwrap();
            if width >= 42 { assert!(!app.modal.close.is_empty()); }
            crate::ui_modal::handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            assert!(app.save_error.is_none());
            assert_eq!(fs::read_to_string(&path).unwrap(), "external\n");
            app.save_current_file();
            finish_saves(&mut app);
        }
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 28)).unwrap();
        terminal.draw(|frame| crate::ui_modal::draw_active(frame, &mut app)).unwrap();
        let button = app.modal.buttons.iter().find(|(_, id)| *id == usize::from(!overwrite)).unwrap().0;
        crate::ui_modal::handle_mouse(&mut app, MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left), column: button.x, row: button.y, modifiers: KeyModifiers::NONE,
        });
        if overwrite { finish_saves(&mut app); }
        else { app.wait_for_background(Duration::from_secs(10), |app| app.save_error.is_none()); }
        assert!(app.save_error.is_none());
        assert_eq!(fs::read_to_string(&path).unwrap(), if overwrite { "Xoriginal\n" } else { "external\n" });
    }
}

#[test]
fn external_save_resolution_overwrite_uses_shared_document_edits_from_another_split() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.txt");
    let mut app = app(&path);
    app.editor_mut().unwrap().insert_char('X');
    fs::write(&path, "external\n").unwrap();
    app.save_current_file();
    finish_saves(&mut app);
    app.create_split_group_dir(0, crate::app::SplitDirection::Horizontal);
    app.editor_mut().unwrap().break_undo_group();
    app.editor_mut().unwrap().insert_char('Y');
    let expected = app.editor().unwrap().rope.clone();
    app.overwrite_save_conflict();
    finish_saves(&mut app);
    assert!(app.save_error.is_none());
    assert_eq!(fs::read_to_string(&path).unwrap(), expected.to_string());
    for tab in app.tabs.iter_mut().chain(app.groups.iter_mut().flat_map(|group| &mut group.tabs)) {
        if tab.editor.file_path.as_ref() == Some(&path) {
            tab.editor.refresh_document();
            assert_eq!(tab.editor.rope, expected);
            assert!(!tab.editor.modified);
        }
    }
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope, "Xoriginal\n");
    app.editor_mut().unwrap().redo();
    assert_eq!(app.editor().unwrap().rope, expected);
    assert!(!app.editor().unwrap().modified);
}

#[test]
fn external_save_deferred_write_rebases_only_our_successful_disk_checkpoint() {
    for external in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        let mut app = app(&path);
        app.editor_mut().unwrap().insert_char('X');
        let first = app
            .editor()
            .unwrap()
            .background_save_request(false, false, false)
            .unwrap();
        app.pending_file_saves.insert(
            path.clone(),
            (app.workspace_generation, first.source_content_version.into()),
        );
        app.editor_mut().unwrap().break_undo_group();
        app.editor_mut().unwrap().insert_char('Y');
        let second = app
            .editor()
            .unwrap()
            .background_save_request(false, false, false)
            .unwrap();
        app.queue_background_save_job(second);
        let saved = crate::editor::write_background_save(&first).unwrap().rope;
        let result = saved_result(&app, first, saved);
        if external {
            fs::write(&path, "another writer\n").unwrap();
        }
        send(&mut app, result);
        finish_saves(&mut app);
        assert_eq!(app.editor().unwrap().rope, "XYoriginal\n");
        assert_eq!(app.editor().unwrap().modified, external);
        assert_eq!(app.save_error.is_some(), external);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            if external {
                "another writer\n"
            } else {
                "XYoriginal\n"
            }
        );
        app.editor_mut().unwrap().undo();
        assert_eq!(app.editor().unwrap().rope, "Xoriginal\n");
    }
}

#[test]
fn external_save_two_new_documents_cannot_inherit_each_others_overwrite_permission() {
    for mode in ["other", "same", "duplicate"] {
        let same_document = mode != "other";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.txt");
        let mut app = App::new(dir.path().into());
        let mut editor = Editor::test_with_text("first draft");
        editor.file_path = Some(path.clone());
        editor.modified = true;
        app.tabs = vec![Tab::new_file(editor)];
        app.active_tab = 0;
        let first = app
            .editor()
            .unwrap()
            .background_save_request(false, false, true)
            .unwrap();
        app.pending_file_saves.insert(
            path.clone(),
            (app.workspace_generation, first.source_content_version.into()),
        );
        if !same_document {
            let mut other = Editor::test_with_text("second draft");
            other.file_path = Some(path.clone());
            other.modified = true;
            app.tabs = vec![Tab::new_file(other)];
        }
        if mode == "duplicate" {
            app.duplicate_tab(0);
        }
        app.editor_mut().unwrap().insert_char('X');
        let second = app
            .editor()
            .unwrap()
            .background_save_request(false, false, true)
            .unwrap();
        app.queue_background_save_job(second);
        let saved = crate::editor::write_background_save(&first).unwrap().rope;
        let result = saved_result(&app, first, saved);
        send(&mut app, result);
        finish_saves(&mut app);
        assert_eq!(app.save_error.is_some(), !same_document);
        assert_eq!(app.editor().unwrap().modified, !same_document);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            if same_document {
                "Xfirst draft"
            } else {
                "first draft"
            }
        );
        if !same_document {
            assert_eq!(app.editor().unwrap().rope, "Xsecond draft");
            assert!(app.editor().unwrap().saved_rope.is_none());
            // Another ordinary Save still requires confirmation; the first
            // document's completion must not establish this one's baseline.
            app.format_on_save = false;
            app.save_current_file();
            finish_saves(&mut app);
            assert_eq!(fs::read_to_string(&path).unwrap(), "first draft");
        }
    }
}

#[test]
fn external_save_as_requires_explicit_replacement_and_can_save_a_copy() {
    for replace in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("original.txt");
        let target = if replace {
            path.clone()
        } else {
            dir.path().join("copy.txt")
        };
        let mut app = app(&path);
        app.editor_mut().unwrap().insert_char('X');
        fs::write(&path, "external\n").unwrap();
        app.save_as_active = true;
        app.save_as_query = target.to_string_lossy().into_owned();
        app.execute_save_as();
        if replace {
            assert!(app.save_as_active && app.pending_file_saves.is_empty());
            assert_eq!(fs::read_to_string(&path).unwrap(), "external\n");
            app.execute_save_as(); // the explicitly labelled Replace File confirmation
        }
        finish_saves(&mut app);
        assert!(app.save_error.is_none());
        assert_eq!(fs::read_to_string(&target).unwrap(), "Xoriginal\n");
        if !replace {
            assert_eq!(fs::read_to_string(&path).unwrap(), "external\n");
        }
        assert!(!app.editor().unwrap().modified);
        app.editor_mut().unwrap().insert_char('Y');
        app.save_current_file();
        finish_saves(&mut app);
        assert!(
            app.save_error.is_none(),
            "ordinary saves after Save As still work (replace={replace}): {:?}",
            app.save_error,
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "XYoriginal\n");
    }
}

#[test]
fn external_save_watcher_preserves_unsaved_edits_and_warns_once_for_backward_timestamps() {
    for watcher in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        let mut app = app(&path);
        app.editor_mut().unwrap().insert_char('X');
        app.duplicate_tab(0);
        app.create_split_group(1);
        fs::write(&path, "external\n").unwrap();
        let change = WorkspacePathChange {
            path: path.clone(),
            observed_at: Instant::now(),
            exists: true,
            modified: Some(UNIX_EPOCH + Duration::from_secs(1)),
            len: Some(9),
        };
        for _ in 0..2 {
            let event = if watcher {
                BackgroundResult::FilesChanged {
                    root: dir.path().into(),
                    changes: vec![change.clone()],
                    structure_changed: false,
                    rescan: false,
                    git_metadata_changed: false,
                }
            } else {
                BackgroundResult::FileDiskState(change.clone())
            };
            send(&mut app, event);
        }
        assert_eq!(
            app.notification_history
                .iter()
                .filter(|entry| entry.0.contains("changed on disk"))
                .count(),
            1
        );
        for tab in app
            .tabs
            .iter()
            .chain(app.groups.iter().flat_map(|group| &group.tabs))
        {
            assert_eq!(tab.editor.rope, "Xoriginal\n");
            assert!(tab.editor.modified && tab.editor.can_undo());
        }
        app.save_current_file();
        finish_saves(&mut app);
        assert!(
            app.save_error.is_some(),
            "watcher metadata must not authorize overwriting the new disk contents"
        );
        assert_eq!(fs::read_to_string(path).unwrap(), "external\n");
    }
}

#[test]
fn external_save_stale_reload_cannot_replace_a_saved_or_reopened_document() {
    for reopen in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        let mut app = app(&path);
        let targets = vec![(
            app.editor().unwrap().navigation_id,
            app.editor().unwrap().content_version,
        )];
        let old = crate::editor::load_text_file(&path).unwrap();
        if reopen {
            fs::write(&path, "reopened\n").unwrap();
            let mut editor = Editor::new();
            editor.open_file(&path).unwrap();
            app.tabs = vec![Tab::new_file(editor)];
        } else {
            app.editor_mut().unwrap().insert_char('X');
            app.editor_mut()
                .unwrap()
                .save_file_with_options(false, false)
                .unwrap();
        }
        let expected = app.editor().unwrap().rope.clone();
        send(
            &mut app,
            BackgroundResult::FileReloaded {
                workspace_generation: 0,
                path,
                targets,
                result: Ok(old),
            },
        );
        assert!(app.editor().unwrap().rope.is_instance(&expected));
        assert!(!app.editor().unwrap().modified);
    }
}

#[test]
fn external_save_pending_write_blocks_reload_even_after_undo_to_clean_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.txt");
    let mut app = app(&path);
    app.editor_mut().unwrap().insert_char('X');
    let request = app
        .editor()
        .unwrap()
        .background_save_request(false, false, false)
        .unwrap();
    app.editor_mut().unwrap().undo();
    app.pending_file_saves
        .insert(path.clone(), (0, request.source_content_version.into()));
    let old = crate::editor::load_text_file(&path).unwrap();
    let targets = vec![(
        app.editor().unwrap().navigation_id,
        app.editor().unwrap().content_version,
    )];
    send(
        &mut app,
        BackgroundResult::FileReloaded {
            workspace_generation: 0,
            path: path.clone(),
            targets,
            result: Ok(old),
        },
    );
    assert_eq!(app.editor().unwrap().rope, "original\n");
    let saved = crate::editor::write_background_save(&request).unwrap().rope;
    let result = saved_result(&app, request, saved);
    send(&mut app, result);
    assert!(app.editor().unwrap().modified);
    assert_eq!(app.editor().unwrap().rope, "original\n");
    assert_eq!(fs::read_to_string(path).unwrap(), "Xoriginal\n");
}

#[test]
fn external_save_clean_editor_still_reloads_external_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.txt");
    let mut app = app(&path);
    fs::write(&path, "external replacement\n").unwrap();
    let metadata = fs::metadata(&path).unwrap();
    send(
        &mut app,
        BackgroundResult::FileDiskState(WorkspacePathChange {
            path: path.clone(),
            observed_at: Instant::now(),
            exists: true,
            modified: metadata.modified().ok(),
            len: Some(metadata.len()),
        }),
    );
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.editor().unwrap().rope == "external replacement\n"
    });
    assert!(!app.editor().unwrap().modified);
    app.editor_mut().unwrap().insert_char('X');
    app.save_current_file();
    finish_saves(&mut app);
    assert!(app.save_error.is_none());
    assert_eq!(fs::read_to_string(path).unwrap(), "Xexternal replacement\n");
}
