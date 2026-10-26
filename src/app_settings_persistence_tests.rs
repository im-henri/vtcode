use crate::app::{App, Tab, WhitespaceMode};
use crate::settings::{Settings, SettingsScope};
use crate::ui_search_controls::{SearchControl, SearchScope};

fn fixture() -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(dir.path().to_path_buf());
    app.settings_global_path = dir.path().join("user/settings.json");
    app.settings_workspace_path = dir.path().join("workspace/settings.json");
    app.apply_effective_settings();
    app.settings_persistence_enabled = true;
    (dir, app)
}

fn saved(app: &App, scope: SettingsScope) -> Settings {
    crate::settings::load_from_file(match scope {
        SettingsScope::Global => &app.settings_global_path,
        SettingsScope::Workspace => &app.settings_workspace_path,
    })
}

fn numeric(app: &mut App, id: &str, value: &str) {
    app.begin_numeric_setting_edit(id, "0");
    app.settings_value_buffer = value.into();
    assert!(app.commit_numeric_setting_edit());
}

fn toggle_untracked_menu(app: &mut App, actions: bool) {
    if actions {
        app.open_source_control_action_menu(0, 1);
    } else {
        app.open_source_control_filter_menu(0, 1);
    }
    app.context_menu_selected = app.context_menu_items.iter()
        .position(|item| item.id == "git_show_untracked").unwrap();
    crate::input_pickers::handle_context_menu_key(app, crossterm::event::KeyCode::Enter);
    assert!(app.context_menu_active, "the checkbox menu stays open");
    let item = app.context_menu_items.iter().find(|item| item.id == "git_show_untracked").unwrap();
    assert_eq!(item.label.starts_with('✓'), app.sc_visibility.untracked);
}

fn assert_untracked_preference(app: &mut App, show: bool) {
    assert_eq!(app.sc_visibility.untracked, show);
    assert_eq!(app.current_settings().source_control_show_untracked, Some(show));
    app.source_control_files = [("staged.rs", true, 'M'), ("tracked.rs", false, 'M'), ("new.rs", false, '?')]
        .into_iter().map(|(path, staged, status)| crate::app::GitFileChange {
            path: path.into(), staged, status, original_path: None,
        }).collect();
    assert_eq!(app.source_control_change_count(), if show { 3 } else { 2 });
    let visibility = app.explorer_git_badge_visibility();
    assert_eq!(visibility.color_status(crate::explorer::GitStatus::Untracked),
        if show { crate::explorer::GitStatus::Untracked } else { crate::explorer::GitStatus::Unmodified });
    assert_eq!(visibility.color_status(crate::explorer::GitStatus::Modified), crate::explorer::GitStatus::Modified);
}

#[test]
fn untracked_menu_preference_is_saved_immediately_in_both_directions_and_menus() {
    for actions in [false, true] {
        let (_dir, mut app) = fixture();
        // A previously visited Settings scope must not turn a User menu
        // preference into a workspace override.
        app.settings_scope = SettingsScope::Workspace;
        std::fs::create_dir_all(app.settings_global_path.parent().unwrap()).unwrap();
        std::fs::write(&app.settings_global_path, r#"{"external.setting":123,"tabSize":6}"#).unwrap();
        for show in [false, true] {
            toggle_untracked_menu(&mut app, actions);
            assert_untracked_preference(&mut app, show);
            // No quit, session write, or other Settings interaction is needed.
            let persisted: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(&app.settings_global_path).unwrap()).unwrap();
            assert_eq!(persisted["sourceControlShowUntracked"], show);
            assert_eq!(persisted["external.setting"], 123);
            assert_eq!(persisted["tabSize"], 6);
            assert_eq!(persisted.as_object().unwrap().len(), 3);
            assert!(!app.settings_workspace_path.exists());
        }
    }
}

#[test]
fn untracked_preference_survives_file_folder_and_stale_session_startup() {
    let (directory, mut app) = fixture();
    let path = directory.path().join("test.txt");
    std::fs::write(&path, "text\n").unwrap();
    let other = directory.path().join("other");
    std::fs::create_dir(&other).unwrap();
    for show in [false, true] {
        toggle_untracked_menu(&mut app, false);
        for (root, file, restore) in [
            (directory.path(), None, false),
            (directory.path(), Some(path.clone()), false),
            (other.as_path(), None, false),
            (directory.path(), None, true),
        ] {
            let mut session = app.build_session();
            session.sc_visibility.untracked = !show;
            let mut restarted = crate::startup::Prepared {
                target: crate::startup::Target { root: root.into(), file, line: None, column: None },
                global: saved(&app, SettingsScope::Global),
                workspace: saved(&app, SettingsScope::Workspace),
                session: restore.then_some(session),
            }.into_app(Default::default());
            assert_untracked_preference(&mut restarted, show);
            restarted.apply_effective_settings();
            assert_untracked_preference(&mut restarted, show);
        }
    }
}

#[test]
fn untracked_preference_respects_scope_overrides_and_reset() {
    let (_directory, mut app) = fixture();
    app.toggle_setting("source_control_show_untracked");
    assert_eq!(saved(&app, SettingsScope::Global).source_control_show_untracked, Some(false));
    assert_untracked_preference(&mut app, false);
    app.settings_scope = SettingsScope::Workspace;
    app.toggle_setting("source_control_show_untracked");
    assert_eq!(saved(&app, SettingsScope::Workspace).source_control_show_untracked, Some(true));
    assert_untracked_preference(&mut app, true);
    toggle_untracked_menu(&mut app, true);
    assert_eq!(saved(&app, SettingsScope::Workspace).source_control_show_untracked, Some(false));
    assert_eq!(saved(&app, SettingsScope::Global).source_control_show_untracked, Some(false));
    assert_untracked_preference(&mut app, false);
    // Editing User settings must show that scope's value while a Workspace
    // override continues to determine the live filter.
    app.settings_scope = SettingsScope::Global;
    app.toggle_setting("source_control_show_untracked");
    assert_untracked_preference(&mut app, false);
    let items = app.settings_items();
    assert_eq!(items.iter().find(|item| item.2 == "source_control_show_untracked").unwrap().1, "On");
    assert_eq!(App::settings_category_for_id("source_control_show_untracked"), 9);
    assert_eq!(App::setting_field_kind("source_control_show_untracked"), "checkbox");
    app.settings_scope = SettingsScope::Workspace;
    assert!(app.reset_setting_in_selected_scope("source_control_show_untracked"));
    assert_eq!(saved(&app, SettingsScope::Workspace).source_control_show_untracked, None);
    assert_untracked_preference(&mut app, true);
    app.settings_scope = SettingsScope::Global;
    assert!(app.reset_setting_in_selected_scope("source_control_show_untracked"));
    assert_eq!(saved(&app, SettingsScope::Global).source_control_show_untracked, None);
    assert_untracked_preference(&mut app, true);
}

#[test]
fn failed_untracked_preference_write_preserves_live_filter_and_settings() {
    let (directory, mut app) = fixture();
    app.settings_global_path = directory.path().join("blocker/settings.json");
    std::fs::write(directory.path().join("blocker"), "unchanged").unwrap();
    toggle_untracked_menu(&mut app, true);
    assert_untracked_preference(&mut app, true);
    assert_eq!(app.settings_global.source_control_show_untracked, None);
    assert_eq!(std::fs::read_to_string(directory.path().join("blocker")).unwrap(), "unchanged");
    assert!(!app.settings_workspace_path.exists());
}

#[test]
fn untracked_preference_in_remote_mode_is_saved_on_client_without_network_requests() {
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    let (directory, mut app) = fixture();
    let traffic = Arc::new(AtomicUsize::new(0));
    let client = crate::remote::tests::connection(directory.path(), Arc::default(), traffic.clone());
    // Client::connect starts its writer asynchronously. Finish a round trip
    // before measuring so its initial handshake newline is excluded.
    assert!(matches!(client.call(crate::remote::Command::Ping).unwrap(), crate::remote::Reply::Done));
    let remote = crate::remote::workspace::Workspace::new(client);
    app.explorer.root = remote.view_root.clone();
    app.remote_workspace = Some(remote.clone());
    let before = traffic.load(Ordering::Relaxed);
    toggle_untracked_menu(&mut app, true);
    assert_untracked_preference(&mut app, false);
    assert_eq!(saved(&app, SettingsScope::Global).source_control_show_untracked, Some(false));
    let mut restarted = crate::startup::Prepared {
        target: crate::startup::Target { root: remote.view_root.clone(), file: None, line: None, column: None },
        global: saved(&app, SettingsScope::Global),
        workspace: Settings::default(), session: None,
    }.into_app_with_remote(Default::default(), Some(remote));
    assert_untracked_preference(&mut restarted, false);
    assert_eq!(traffic.load(Ordering::Relaxed), before, "the preference is entirely local");
    assert!(!app.settings_workspace_path.exists());
}

#[test]
fn palette_theme_survives_every_settings_control_and_reload() {
    let (_dir, mut app) = fixture();
    app.execute_palette_command(&crate::app_theme::theme_command_id("light"));
    assert_eq!(
        saved(&app, SettingsScope::Global).theme.as_deref(),
        Some("light")
    );
    app.open_settings_tab();
    app.toggle_setting("minimap");
    assert_eq!(app.theme_key, "light");
    numeric(&mut app, "scroll_speed", "7");
    assert_eq!(app.theme_key, "light");
    app.apply_settings_dropdown_value("cursor_style", "Block");
    assert_eq!(app.theme_key, "light");
    assert!(app.reset_setting_in_selected_scope("minimap"));
    assert_eq!(app.theme_key, "light");

    let global = saved(&app, SettingsScope::Global);
    assert_eq!(global.theme.as_deref(), Some("light"));
    assert_eq!(global.scroll_speed, Some(7));
    assert_eq!(global.cursor_style.as_deref(), Some("block"));
    assert_eq!(global.show_minimap, None);
    assert_eq!(global.tab_size, None);
    assert_eq!(global.animation_scope_bubble_duration_ms, None);
    assert!(!app.settings_workspace_path.exists());

    let mut restarted = App::new(app.explorer.root.clone());
    restarted.settings_global = global;
    restarted.settings_workspace = saved(&app, SettingsScope::Workspace);
    restarted.apply_effective_settings();
    assert_eq!(restarted.theme_key, "light");
    assert_eq!(restarted.scroll_speed, 7);
    assert_eq!(restarted.cursor_style, crate::app::CursorStyle::Block);
}

#[test]
fn settings_already_open_reads_later_ui_changes() {
    let (_dir, mut app) = fixture();
    app.open_settings_tab();
    app.execute_palette_command("toggle_minimap");
    let minimap = app.show_minimap;
    app.execute_palette_command(&crate::app_theme::theme_command_id("light"));
    app.toggle_setting("line_numbers");
    assert_eq!(app.theme_key, "light");
    assert_eq!(app.show_minimap, minimap);
    assert_eq!(
        saved(&app, SettingsScope::Global).show_minimap,
        Some(minimap)
    );
}

#[test]
fn preference_commands_save_both_toggle_directions_without_unrelated_overrides() {
    for (command, setting) in [
        ("toggle_minimap", "minimap"),
        ("toggle_error_lens", "error_lens"),
        ("toggle_word_wrap", "word_wrap"),
        ("toggle_whitespace", "whitespace"),
        ("toggle_cursor_blinking", "cursor_blinking"),
        ("toggle_indent_rainbow", "indent_rainbow"),
        ("toggle_sidebar_position", "sidebar_pos"),
        ("toggle_inlay_hints", "inlay_hints"),
    ] {
        let (_dir, mut app) = fixture();
        app.tabs.push(Tab::new_file(app.new_editor()));
        let key = App::setting_json_key(setting).unwrap();
        for _ in 0..2 {
            let before = serde_json::to_value(app.current_settings()).unwrap();
            app.execute_palette_command(command);
            let after = serde_json::to_value(app.current_settings()).unwrap();
            assert_ne!(before[key], after[key], "{command}");
            let persisted = serde_json::to_value(saved(&app, SettingsScope::Global)).unwrap();
            assert_eq!(persisted[key], after[key], "{command}");
            app.toggle_setting("line_numbers");
            let reapplied = serde_json::to_value(app.current_settings()).unwrap();
            assert_eq!(reapplied[key], after[key], "{command}");
        }
        assert_eq!(saved(&app, SettingsScope::Global).tab_size, None);
        assert!(!app.settings_workspace_path.exists());
    }
}

#[test]
fn ui_commands_update_existing_workspace_overrides_and_otherwise_user_settings() {
    let (_dir, mut app) = fixture();
    app.settings_global.theme = Some("dark".into());
    app.settings_global.show_minimap = Some(false);
    app.settings_workspace.theme = Some("light".into());
    app.settings_workspace.tab_size = Some(8);
    app.apply_effective_settings();

    app.settings_scope = SettingsScope::Global;
    app.execute_palette_command(&crate::app_theme::theme_command_id("dark"));
    assert_eq!(
        saved(&app, SettingsScope::Workspace).theme.as_deref(),
        Some("dark")
    );
    assert!(!app.settings_global_path.exists());

    // The last visited Settings scope is not the destination for UI commands.
    app.settings_scope = SettingsScope::Workspace;
    app.execute_palette_command("toggle_minimap");
    assert_eq!(saved(&app, SettingsScope::Global).show_minimap, Some(true));
    assert_eq!(saved(&app, SettingsScope::Global).tab_size, None);
    assert_eq!(app.settings_workspace.show_minimap, None);
    assert_eq!(app.settings_workspace.tab_size, Some(8));
    app.toggle_setting("line_numbers");
    assert_eq!(app.theme_key, "dark");
    assert!(app.show_minimap);
    assert_eq!(app.default_tab_size, 8);
    assert_eq!(app.settings_global.show_line_numbers, None);
    assert!(app.settings_workspace.show_line_numbers.is_some());
}

#[test]
fn settings_commands_use_the_explicit_scope_without_saving_temporary_values() {
    for setting in [
        "inlay_hints",
        "code_lens",
        "semantic_highlighting",
        "format_on_type",
        "screencast_mode",
        "zen",
    ] {
        let (_dir, mut app) = fixture();
        app.settings_scope = SettingsScope::Workspace;
        app.toggle_setting(setting);
        let key = App::setting_json_key(setting).unwrap();
        let workspace = serde_json::to_value(saved(&app, SettingsScope::Workspace)).unwrap();
        assert!(!workspace[key].is_null(), "{setting}");
        assert!(!app.settings_global_path.exists(), "{setting}");
        assert_eq!(app.settings_workspace.show_explorer, None);
        assert_eq!(app.settings_workspace.show_minimap, None);
        assert!(app.reset_setting_in_selected_scope(setting));
        assert!(!app.settings_global_path.exists(), "{setting}");
    }
}

#[test]
fn canceled_theme_previews_do_not_get_saved_by_other_ui_changes() {
    for settings_dropdown in [false, true] {
        let (_dir, mut app) = fixture();
        let light_name = app
            .loaded_themes
            .iter()
            .find(|theme| theme.config_key == "light")
            .unwrap()
            .name
            .clone();
        if settings_dropdown {
            app.open_settings_dropdown("theme", "Dark+");
            app.preview_settings_dropdown_value("theme", &light_name);
        } else {
            app.open_palette();
            let theme_id = crate::app_theme::theme_command_id("light");
            let index = app
                .palette
                .commands
                .iter()
                .position(|command| command.id == theme_id)
                .unwrap();
            app.palette.filtered = vec![index];
            app.palette.selected = 0;
            app.preview_palette_theme();
        }
        assert_eq!(app.theme_key, "light");
        app.execute_palette_command("toggle_minimap");
        assert_eq!(app.theme_key, "light");
        assert_eq!(saved(&app, SettingsScope::Global).theme, None);
        assert_eq!(app.current_settings().theme.as_deref(), Some("dark"));
        if settings_dropdown {
            app.close_settings_dropdown();
        } else {
            app.close_palette();
        }
        app.toggle_setting("line_numbers");
        assert_eq!(app.theme_key, "dark");
        assert_eq!(saved(&app, SettingsScope::Global).theme, None);
    }
}

#[test]
fn confirmed_settings_theme_preview_only_writes_the_selected_scope() {
    let (_dir, mut app) = fixture();
    let light_name = app
        .loaded_themes
        .iter()
        .find(|theme| theme.config_key == "light")
        .unwrap()
        .name
        .clone();
    app.settings_scope = SettingsScope::Workspace;
    app.open_settings_dropdown("theme", "Dark+");
    app.preview_settings_dropdown_value("theme", &light_name);
    app.apply_settings_dropdown_value("theme", &light_name);
    app.close_settings_dropdown();
    assert_eq!(
        saved(&app, SettingsScope::Workspace).theme.as_deref(),
        Some("light")
    );
    assert!(!app.settings_global_path.exists());
    app.toggle_setting("minimap");
    assert_eq!(app.theme_key, "light");
}

#[test]
fn search_buttons_persist_and_survive_unrelated_settings_edits() {
    let (_dir, mut app) = fixture();
    for scope in [SearchScope::File, SearchScope::Workspace] {
        for control in [
            SearchControl::MatchCase,
            SearchControl::WholeWord,
            SearchControl::Regex,
            SearchControl::PreserveCase,
        ] {
            crate::ui_search_controls::activate(&mut app, scope, control);
        }
        let expected = (
            app.search_case_sensitive,
            app.search_whole_word,
            app.search_regex,
            app.search_preserve_case,
        );
        let global = saved(&app, SettingsScope::Global);
        assert_eq!(
            (
                global.search_case_sensitive,
                global.search_whole_word,
                global.search_regex,
                global.search_preserve_case
            ),
            (
                Some(expected.0),
                Some(expected.1),
                Some(expected.2),
                Some(expected.3)
            )
        );
        app.toggle_setting("minimap");
        assert_eq!(
            (
                app.search_case_sensitive,
                app.search_whole_word,
                app.search_regex,
                app.search_preserve_case
            ),
            expected
        );
    }
}

#[test]
fn pending_layout_changes_survive_opening_editing_and_resetting_settings() {
    let (_dir, mut app) = fixture();
    app.explorer_width = 47;
    app.open_settings_tab();
    assert_eq!(saved(&app, SettingsScope::Global).explorer_width, Some(47));
    app.show_open_editors = false;
    app.toggle_setting("minimap");
    assert!(!app.show_open_editors);
    assert_eq!(
        saved(&app, SettingsScope::Global).show_open_editors,
        Some(false)
    );
    app.panel_height = 21;
    assert!(app.reset_setting_in_selected_scope("minimap"));
    assert_eq!(app.panel_height, 21);
    assert_eq!(saved(&app, SettingsScope::Global).panel_height, Some(21));
}

#[test]
fn unrelated_settings_preserve_document_choices_but_explicit_defaults_propagate() {
    let (_dir, mut app) = fixture();
    app.tabs.push(Tab::new_file(app.new_editor()));
    app.execute_palette_command("indent_2");
    app.execute_palette_command("toggle_read_only");
    app.tabs[0].editor.word_wrap = true;
    app.tabs[0].editor.whitespace_mode = WhitespaceMode::All;
    app.settings_workspace.tab_size = Some(8);
    app.apply_effective_settings();
    app.tabs[0].editor.tab_size = 2;
    // Editing User settings must not temporarily propagate User indentation
    // over this document while an unrelated workspace override is active.
    app.open_settings_tab();
    app.toggle_setting("minimap");
    numeric(&mut app, "scroll_speed", "6");
    app.apply_settings_dropdown_value("cursor_style", "Block");
    assert_eq!(app.tabs[0].editor.tab_size, 2);
    assert!(app.tabs[0].editor.read_only);
    assert!(app.tabs[0].editor.word_wrap);
    assert_eq!(app.tabs[0].editor.whitespace_mode, WhitespaceMode::All);
    let global = saved(&app, SettingsScope::Global);
    assert_eq!(global.tab_size, None);
    assert_eq!(global.read_only, None);
    assert_eq!(global.word_wrap, None);
    assert_eq!(global.whitespace_mode, None);

    app.settings_scope = SettingsScope::Workspace;
    numeric(&mut app, "tab_size", "6");
    assert_eq!(app.tabs[0].editor.tab_size, 6);
    assert_eq!(app.new_editor().tab_size, 6);
    app.apply_settings_dropdown_value("whitespace", "Boundary");
    assert_eq!(app.tabs[0].editor.whitespace_mode, WhitespaceMode::Boundary);
}

#[test]
fn opening_settings_without_changes_does_not_create_override_files() {
    let (_dir, mut app) = fixture();
    app.open_settings_tab();
    app.open_settings_tab();
    app.flush_live_settings();
    assert!(!app.settings_global_path.exists());
    assert!(!app.settings_workspace_path.exists());
}

#[test]
fn keybinding_capture_preserves_ui_changes_and_workspace_defaults() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let (_dir, mut app) = fixture();
    app.settings_workspace.tab_size = Some(8);
    app.settings_workspace.theme = Some("light".into());
    app.apply_effective_settings();
    app.tabs.push(Tab::new_file(app.new_editor()));
    app.tabs[0].editor.tab_size = 2;
    app.open_settings_tab();
    app.execute_palette_command("toggle_minimap");
    let minimap = app.show_minimap;

    // The Settings editor captures keys in the selected User scope while the
    // workspace theme, indentation and other live preferences remain visible.
    app.settings_editor_search = "Save File".into();
    let items = app.settings_items();
    let (index, (_, _, id)) = items
        .iter()
        .enumerate()
        .find(|(_, (_, _, id))| id.starts_with("kb_"))
        .unwrap();
    let id = id.clone();
    app.settings_selected = index;
    app.toggle_setting(&id);
    assert!(app.settings_kb_editing);
    assert_eq!(app.theme_key, "light");
    assert_eq!(app.tabs[0].editor.tab_size, 2);
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::F(12), KeyModifiers::CONTROL | KeyModifiers::ALT),
    );
    assert!(!app.settings_kb_editing);
    assert_eq!(app.theme_key, "light");
    assert_eq!(app.show_minimap, minimap);
    assert_eq!(app.tabs[0].editor.tab_size, 2);
    let global = saved(&app, SettingsScope::Global);
    assert!(global.keybindings.is_some());
    assert_eq!(global.tab_size, None);
    assert_eq!(global.theme, None);
    assert_eq!(app.settings_workspace.keybindings, None);
}

#[test]
fn unrelated_settings_keep_color_customizations_and_animation_inheritance() {
    let (_dir, mut app) = fixture();
    app.settings_global
        .workbench_color_customizations
        .insert("editor.background".into(), "#123456".into());
    app.settings_global.animation_duration_ms = Some(430);
    app.settings_workspace.animation_scope_bubble_duration_ms = Some(730);
    app.apply_effective_settings();
    app.execute_palette_command("toggle_minimap");
    numeric(&mut app, "scroll_speed", "8");
    assert_eq!(
        app.colors.bg_editor,
        ratatui::style::Color::Rgb(0x12, 0x34, 0x56)
    );
    let global = saved(&app, SettingsScope::Global);
    assert_eq!(global.animation_duration_ms, Some(430));
    assert_eq!(global.animation_scope_bubble_duration_ms, None);
    assert_eq!(global.animation_cursor_smear_duration_ms, None);
    assert_eq!(
        app.settings_workspace.animation_scope_bubble_duration_ms,
        Some(730)
    );
}
