use crate::app::{ActivePane, App, SidebarView, SourceControlRepositoryState as State, Tab};
use crate::background::BackgroundResult;
use crate::editor::{Editor, PreparedGitLineStatus};
use crate::settings::{Settings, SettingsScope};
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use std::time::Duration;

fn fixture() -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    drop(git2::Repository::init(dir.path()).unwrap());
    let path = dir.path().join("file.txt");
    std::fs::write(&path, "original\n").unwrap();
    let mut app = App::new_deferred(dir.path().into());
    let mut editor = Editor::test_with_text("");
    editor.open_file(&path).unwrap();
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.settings_global_path = dir.path().join("user/settings.json");
    app.settings_workspace_path = dir.path().join("workspace/settings.json");
    (dir, app)
}

fn assert_no_git_jobs(app: &App) {
    assert!(!app.bg_git_status_running && !app.bg_git_status_pending);
    assert!(!app.bg_git_branch_running && !app.bg_git_branch_pending);
    assert!(!app.bg_git_diff_running && !app.bg_git_diff_pending);
    assert!(!app.bg_git_log_running && !app.bg_git_compare_running);
    assert!(!app.bg_blame_running && !app.bg_git_branch_picker_running);
    assert!(!app.bg_git_action_running && !app.bg_git_remote_running);
}

#[test]
fn disabled_startup_and_session_restore_do_not_schedule_git_work() {
    let (_dir, mut app) = fixture();
    app.settings_global = Settings { source_control_enabled: Some(false), ..Default::default() };
    app.apply_effective_settings();
    app.sidebar_view = SidebarView::SourceControl;
    let session = app.build_session();
    app.restore_session(&session);
    for _ in 0..3 {
        app.refresh_source_control_async();
        app.refresh_source_control_for_navigation();
        app.refresh_git_branch_async();
        app.refresh_git_line_status_async();
        app.refresh_git_log_async();
        app.refresh_source_control_diffs();
        crate::git_blame::refresh(&mut app);
        app.open_git_branch_picker();
        app.git_fetch_async();
        app.git_push_async();
        app.git_stage_all();
        assert!(!app.request_compare_with_git_async());
    }
    assert_no_git_jobs(&app);
    assert_eq!(app.source_control_repository_state, State::Disabled);
    assert!(app.source_control_cancel.is_cancelled());
    let root = app.source_control_root().to_owned();
    assert!(app.discover_repo_at(&root).is_none());
}

#[test]
fn queued_and_late_results_cannot_revive_disabled_or_reenabled_state() {
    let (_dir, mut app) = fixture();
    let old = app.source_control_sender();
    let cancel = app.source_control_cancel.clone();
    old.send(BackgroundResult::GitBranch("obsolete branch".into())).unwrap();
    old.send(BackgroundResult::GitRepositories {
        root: app.explorer.root.clone(), workspace_generation: app.workspace_generation,
        repository_generation: app.repositories.membership_generation,
        discovery: crate::git_repositories::Discovery {
            errors: Vec::new(),
            workspace: app.explorer.root.clone(), roots: vec![app.explorer.root.clone()], parent: None,
        },
        scans: vec![crate::git_repositories::Scan {
            root: app.explorer.root.clone(), branch: String::new(),
            files: vec![crate::app::GitFileChange { directory: false, path: "obsolete.txt".into(), status: '?', staged: false, original_path: None }],
            worktree_paths: None, include_untracked: true,
        }],
    }).unwrap();
    old.send(BackgroundResult::GitLineDiff {
        file_path: app.editor().unwrap().file_path.clone().unwrap(),
        status: PreparedGitLineStatus::from_diffs(2, &[], &['U', 'U']),
    }).unwrap();
    app.set_source_control_enabled(false);
    app.poll_background_results();
    assert!(app.cached_git_branch.is_empty() && app.source_control_files.is_empty());
    assert!(app.editor().unwrap().git_line_status().is_empty());
    assert_eq!(app.source_control_repository_state, State::Disabled);
    assert_no_git_jobs(&app);

    app.set_source_control_enabled(true);
    assert!(old.is_cancelled());
    assert!(!app.source_control_cancel.same_task(&cancel));
    // Model the send/cancel race: cancellation happened after the worker's
    // final check, but before the result reached the receiver.
    app.background.sender.send(BackgroundResult::SourceControlRead {
        cancel, result: Box::new(BackgroundResult::GitBranch("obsolete branch".into())),
    }).unwrap();
    old.send(BackgroundResult::GitBranch("late branch".into())).unwrap();
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_git_status_running && !app.bg_git_branch_running);
    assert_eq!(app.source_control_repository_state, State::Available);
    assert!(!app.cached_git_branch.contains("obsolete") && !app.cached_git_branch.contains("late"));
    assert!(app.source_control_files.iter().any(|file| file.path == "file.txt"));
    assert!(!app.source_control_files.iter().any(|file| file.path == "obsolete.txt"));
}

#[test]
fn disabling_keeps_unsaved_hunks_undo_redo_and_saving_intact() {
    let (dir, mut app) = fixture();
    app.editor_mut().unwrap().set_git_line_status(vec!['U', ' ']);
    app.editor_mut().unwrap().multi_cursor_insert_char('x');
    assert!(app.editor().unwrap().modified);
    app.set_source_control_enabled(false);
    assert!(app.editor().unwrap().git_line_status().is_empty());
    assert!(app.editor().unwrap().modified);
    let mut terminal = Terminal::new(TestBackend::new(50, 2)).unwrap();
    terminal.draw(|frame| {
        crate::editor_blame::header(frame, &mut app, Rect::new(0, 0, 50, 1));
    }).unwrap();
    assert!(app.editor().unwrap().blame.button.is_none());
    app.hunk_popup_file = app.editor().unwrap().file_path.clone();
    app.hunk_popup_status = 'N';
    app.hunk_popup_active = true;
    app.hunk_popup_line = 0;
    let peek = app.load_hunk_peek().unwrap();
    assert!(peek.unsaved.is_some());
    app.hunk_peek = Some(peek);
    app.revert_unsaved_hunk().unwrap();
    assert_eq!(app.editor().unwrap().rope.to_string(), "original\n");
    assert!(!app.editor().unwrap().modified);
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope.to_string(), "xoriginal\n");
    app.editor_mut().unwrap().redo();
    assert_eq!(app.editor().unwrap().rope.to_string(), "original\n");
    app.editor_mut().unwrap().multi_cursor_insert_char('y');
    let expected = app.editor().unwrap().rope.to_string();
    app.save_current_file();
    app.wait_for_background(Duration::from_secs(5), |app| app.pending_file_saves.is_empty());
    assert_eq!(std::fs::read_to_string(dir.path().join("file.txt")).unwrap(), expected);
    assert!(!app.editor().unwrap().modified);
    assert_no_git_jobs(&app);
}

#[test]
fn git_only_watcher_events_do_not_refresh_explorer_when_disabled() {
    let (_dir, mut app) = fixture();
    app.set_source_control_enabled(false);
    let generation = app.explorer_refresh_generation;
    app.background.sender.send(BackgroundResult::FilesChanged {
        root: app.explorer.root.clone(), changes: vec![], structure_changed: false,
        rescan: false, git_metadata_changed: true,
    }).unwrap();
    app.poll_background_results();
    assert_no_git_jobs(&app);
    assert_eq!(app.explorer_refresh_generation, generation);
}

#[test]
fn settings_persist_scopes_reset_and_restart_without_changing_other_preferences() {
    let (_dir, mut app) = fixture();
    app.settings_persistence_enabled = true;
    app.settings_scope = SettingsScope::Global;
    app.toggle_setting("source_control_enabled");
    assert!(!app.source_control_enabled);
    let global = crate::settings::load_from_file(&app.settings_global_path);
    assert_eq!(global.source_control_enabled, Some(false));
    assert_eq!(global.tab_size, None);
    assert_eq!(global.lsp_enabled, None);
    assert!(!app.settings_workspace_path.exists());
    app.toggle_workspace_source_control();
    assert!(app.source_control_enabled);
    let workspace = crate::settings::load_from_file(&app.settings_workspace_path);
    assert_eq!(workspace.source_control_enabled, Some(true));
    let mut reopened = App::new_deferred(app.explorer.root.clone());
    reopened.settings_global = global;
    reopened.settings_workspace = workspace;
    reopened.apply_effective_settings();
    assert!(reopened.source_control_enabled);
    // The User checkbox must still display the User value, even when the
    // Workspace override enables the running integration.
    reopened.settings_scope = SettingsScope::Global;
    assert_eq!(reopened.selected_scope_settings().source_control_enabled, Some(false));
    reopened.settings_scope = SettingsScope::Workspace;
    assert!(reopened.reset_setting_in_selected_scope("source_control_enabled"));
    assert!(!reopened.source_control_enabled);
    assert_eq!(reopened.source_control_repository_state, State::Disabled);
}

#[test]
fn failed_setting_write_leaves_source_control_running() {
    let (dir, mut app) = fixture();
    app.settings_persistence_enabled = true;
    app.settings_workspace_path = dir.path().join("file.txt/settings.json");
    let epoch = app.source_control_cancel.clone();
    app.toggle_workspace_source_control();
    assert!(app.source_control_enabled);
    assert!(!epoch.is_cancelled());
    assert_eq!(app.settings_workspace.source_control_enabled, None);
}

#[test]
fn sidebar_toggle_works_during_loading_and_in_narrow_left_and_right_panels() {
    for width in [15, 24, 40, 80] {
        for right in [false, true] {
            let (_dir, mut app) = fixture();
            app.sidebar_view = SidebarView::SourceControl;
            app.sidebar_right = right;
            app.show_explorer = true;
            app.source_control_repository_state = State::Loading;
            app.source_control_input_mode = true;
            let area = Rect::new(4, 1, width, 16);
            app.explorer_area = Some(area);
            let mut terminal = Terminal::new(TestBackend::new(90, 20)).unwrap();
            terminal.draw(|frame| crate::ui_source_control::draw_source_control_sidebar(frame, &mut app, area)).unwrap();
            let button = app.source_control_actions_area.unwrap();
            assert!(area.contains((button.x, button.y).into()));
            assert!(button.right() <= area.right());
            crate::mouse::handle_mouse(&mut app, MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left), column: button.x + 1, row: button.y, modifiers: KeyModifiers::NONE,
            });
            assert!(app.source_control_action_menu_open());
            crate::input_pickers::handle_context_menu_key(&mut app, KeyCode::End);
            crate::input_pickers::handle_context_menu_key(&mut app, KeyCode::Enter);
            assert_eq!(app.source_control_repository_state, State::Disabled, "width={width}, right={right}");
            assert!(!app.source_control_input_mode);
            assert_eq!(app.settings_workspace.source_control_enabled, Some(false));
            terminal.draw(|frame| crate::ui_source_control::draw_source_control_sidebar(frame, &mut app, area)).unwrap();
            assert!(app.source_control_actions_area.is_some());
            assert!(app.source_control_commit_area.is_none());
            assert!(app.source_control_initialize_area.is_none());
            crate::ui_control_visibility_tests::snapshot(terminal.backend().buffer(), &format!("Source control disabled {width} right {right}"));
            app.active_pane = ActivePane::Explorer;
            crate::ui_source_control::handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(app.source_control_enabled);
            app.wait_for_background(Duration::from_secs(5), |app| !app.bg_git_status_running);
            terminal.draw(|frame| crate::ui_source_control::draw_source_control_sidebar(frame, &mut app, area)).unwrap();
            assert!(app.source_control_toggle_area.is_none());
            let actions = app.source_control_actions_area.unwrap();
            assert!(actions.right() <= area.right());
        }
    }
}

#[test]
fn cancelling_a_read_session_also_protects_folder_switches() {
    let (_dir, mut app) = fixture();
    let old = app.source_control_sender();
    old.send(BackgroundResult::GitBranch("previous workspace".into())).unwrap();
    app.cancel_source_control_reads();
    app.poll_background_results();
    assert!(app.cached_git_branch.is_empty());
    assert!(old.is_cancelled());
    assert!(!app.source_control_cancel.is_cancelled());
}

#[test]
fn reenable_resumes_a_diff_cancelled_before_its_first_result() {
    let (_dir, mut app) = fixture();
    let path = app.editor().unwrap().file_path.clone().unwrap();
    app.open_source_control_diff_tab(&path, false, false);
    assert!(app.pending_git_compare_path.is_some());
    app.set_source_control_enabled(false);
    app.poll_background_results();
    assert!(!app.bg_git_compare_running);
    assert!(app.pending_git_compare_path.is_some());
    app.set_source_control_enabled(true);
    app.wait_for_background(Duration::from_secs(5), |app| app.pending_git_compare_path.is_none());
    assert!(app.compare_base_content.is_some());
    assert!(!app.compare_diff_lines.is_empty());
}

#[test]
fn source_control_settings_page_renders_in_compact_and_wide_layouts() {
    let (_dir, mut app) = fixture();
    app.open_settings_tab();
    let category = App::settings_category_for_id("source_control_enabled");
    app.select_settings_page(category, None);
    assert!(app.settings_category_visual_rows().iter().any(|&(id, _, label)| id == category && label == "Source Control"));
    for width in [48, 110] {
        let mut terminal = Terminal::new(TestBackend::new(width, 25)).unwrap();
        terminal.draw(|frame| crate::ui_settings::draw_settings_editor(frame, &mut app, frame.area())).unwrap();
        let text: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("Enable Source Control"));
        crate::ui_control_visibility_tests::snapshot(terminal.backend().buffer(), &format!("Source control settings {width}"));
    }
}

#[test]
fn reenable_can_reload_an_interrupted_commit_diff() {
    let (dir, mut app) = fixture();
    let repo = git2::Repository::open(dir.path()).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("file.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let author = git2::Signature::now("Test", "test@example.test").unwrap();
    repo.commit(Some("HEAD"), &author, &author, "Initial", &tree, &[]).unwrap();
    app.refresh_git_log_async();
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_git_log_running);
    app.open_source_control_commit_details(0);
    app.wait_for_background(Duration::from_secs(5), |app| !app.commit_details.loading);
    app.commit_details.review.collapsed.remove(&0);
    app.request_commit_diff();
    assert!(app.commit_details.diff_loading);
    app.set_source_control_enabled(false);
    app.poll_background_results();
    assert!(!app.commit_details.diff_loading);
    app.set_source_control_enabled(true);
    app.wait_for_background(Duration::from_secs(5), |app| !app.commit_details.loading);
    app.request_commit_diff();
    assert!(app.commit_details.diff_loading);
    app.wait_for_background(Duration::from_secs(5), |app| !app.commit_details.diff_loading);
    assert!(app.commit_details.diff_error.is_none());
    assert!(app.commit_details.diff_full_cache.is_some());
}
