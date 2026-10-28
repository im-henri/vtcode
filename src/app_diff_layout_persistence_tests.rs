use super::*;
use crate::app::DiffMode;

fn prepare_diff(app: &mut App) {
    app.tabs = vec![Tab::new_file(crate::editor::Editor::new())];
    app.active_tab = 0;
    app.diff_mode = app.preferred_diff_mode;
    app.compare_base_content = Some("before\n".into());
    app.diff_target_path = None;
}

#[test]
fn both_diff_commands_save_the_chosen_layout_immediately_without_erasing_other_settings() {
    for command in ["toggle_diff_layout", "toggle_diff_view"] {
        let (_directory, mut app) = fixture();
        assert!(crate::commit_review::mode(&app, 80) == DiffMode::Inline);
        app.settings_scope = SettingsScope::Workspace;
        std::fs::create_dir_all(app.settings_global_path.parent().unwrap()).unwrap();
        std::fs::write(&app.settings_global_path, r#"{"external.setting":123,"tabSize":6}"#).unwrap();
        prepare_diff(&mut app);
        app.execute_palette_command(command);
        assert!(app.diff_mode == DiffMode::Inline);
        assert!(app.preferred_diff_mode == DiffMode::Inline);
        let persisted: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&app.settings_global_path).unwrap()).unwrap();
        assert_eq!(persisted["diffEditor.renderSideBySide"], false);
        assert_eq!(persisted["external.setting"], 123);
        assert_eq!(persisted["tabSize"], 6);
        assert_eq!(persisted.as_object().unwrap().len(), 3);
        assert!(!app.settings_workspace_path.exists());
        let previous = std::fs::read(&app.settings_global_path).unwrap();
        app.execute_palette_command(command);
        if command == "toggle_diff_layout" {
            assert!(app.diff_mode == DiffMode::SideBySide);
            assert_eq!(saved(&app, SettingsScope::Global).diff_side_by_side, Some(true));
            // A temporary one-sided view must not replace the user's preference.
            app.diff_mode = DiffMode::Inline;
        } else {
            assert!(app.diff_mode == DiffMode::Off);
            assert_eq!(std::fs::read(&app.settings_global_path).unwrap(), previous);
            assert_eq!(saved(&app, SettingsScope::Global).diff_side_by_side, Some(false));
        }
        app.clear_diff_state();
        app.save_settings();
        assert_eq!(saved(&app, SettingsScope::Global).diff_side_by_side, Some(command == "toggle_diff_layout"));
    }
}

#[test]
fn diff_layout_survives_restart_folder_file_and_session_startup() {
    let (directory, mut app) = fixture();
    let file = directory.path().join("test.txt");
    std::fs::write(&file, "text\n").unwrap();
    let other = directory.path().join("other");
    std::fs::create_dir(&other).unwrap();
    for side_by_side in [false, true] {
        prepare_diff(&mut app);
        app.execute_palette_command("toggle_diff_layout");
        assert_eq!(saved(&app, SettingsScope::Global).diff_side_by_side, Some(side_by_side));
        app.clear_diff_state();
        let session = app.build_session();
        for (root, opened_file, restore) in [
            (directory.path(), None, false),
            (directory.path(), Some(file.clone()), false),
            (other.as_path(), None, false),
            (directory.path(), None, true),
        ] {
            let mut restarted = crate::startup::Prepared {
                target: crate::startup::Target { root: root.into(), file: opened_file, line: None, column: None },
                global: saved(&app, SettingsScope::Global),
                workspace: saved(&app, SettingsScope::Workspace),
                session: restore.then(|| session.clone()),
            }.into_app(Default::default());
            let expected = if side_by_side { DiffMode::SideBySide } else { DiffMode::Inline };
            assert!(restarted.preferred_diff_mode == expected);
            assert!(crate::commit_review::mode(&restarted, 80) == expected);
            assert!(crate::commit_review::mode(&restarted, 140) == expected);
            assert!(restarted.diff_mode == DiffMode::Off, "remembering a layout must not enable a comparison");
            restarted.apply_effective_settings();
            assert!(restarted.preferred_diff_mode == expected);
            assert_eq!(restarted.current_settings().diff_side_by_side, Some(side_by_side));
        }
    }
}

#[test]
fn diff_layout_updates_an_existing_workspace_override_instead_of_the_selected_settings_scope() {
    let (_directory, mut app) = fixture();
    app.patch_settings_scope(SettingsScope::Global, "diffEditor.renderSideBySide", |settings| {
        settings.diff_side_by_side = Some(false);
    }).unwrap();
    app.patch_settings_scope(SettingsScope::Workspace, "diffEditor.renderSideBySide", |settings| {
        settings.diff_side_by_side = Some(true);
    }).unwrap();
    app.settings_scope = SettingsScope::Global;
    prepare_diff(&mut app);
    app.execute_palette_command("toggle_diff_layout");
    assert!(app.diff_mode == DiffMode::Inline);
    assert_eq!(saved(&app, SettingsScope::Workspace).diff_side_by_side, Some(false));
    assert_eq!(saved(&app, SettingsScope::Global).diff_side_by_side, Some(false));
    app.execute_palette_command("toggle_diff_layout");
    assert!(app.preferred_diff_mode == DiffMode::SideBySide);
    assert_eq!(saved(&app, SettingsScope::Workspace).diff_side_by_side, Some(true));
    assert_eq!(saved(&app, SettingsScope::Global).diff_side_by_side, Some(false));
}

#[test]
fn an_unwritable_preference_file_does_not_prevent_changing_the_live_diff_layout() {
    let (directory, mut app) = fixture();
    app.settings_global_path = directory.path().join("blocker/settings.json");
    std::fs::write(directory.path().join("blocker"), "unchanged").unwrap();
    prepare_diff(&mut app);
    app.execute_palette_command("toggle_diff_layout");
    assert!(app.diff_mode == DiffMode::Inline);
    assert!(app.preferred_diff_mode == DiffMode::Inline);
    assert_eq!(app.settings_global.diff_side_by_side, None);
    assert_eq!(std::fs::read_to_string(directory.path().join("blocker")).unwrap(), "unchanged");
    assert!(!app.settings_workspace_path.exists());
}
