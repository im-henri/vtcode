use super::*;
use ratatui::{Terminal, backend::TestBackend};

fn render(
    app: &mut App,
    width: u16,
    height: u16,
    settings: bool,
) -> (String, ratatui::buffer::Buffer) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|f| {
            if settings {
                crate::ui_settings::draw_settings_editor(f, app, f.area());
            } else {
                crate::ui_modal::draw_active(f, app);
            }
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let text = buffer.content.iter().map(|cell| cell.symbol()).collect();
    if let Some(path) = std::env::var_os("VTCODE_EXCLUSIONS_CAPTURE") {
        let path = std::path::PathBuf::from(path);
        std::fs::create_dir_all(&path).unwrap();
        let cells = buffer
            .content
            .iter()
            .map(|cell| {
                serde_json::json!({"text":cell.symbol(),
            "fg":format!("{:?}",cell.fg),"bg":format!("{:?}",cell.bg)})
            })
            .collect::<Vec<_>>();
        let name = format!(
            "{}-{}-{width}-{height}.json",
            app.exclusion_settings.key,
            if settings {
                "settings"
            } else if app.exclusion_settings.editing {
                "edit"
            } else {
                "list"
            }
        );
        std::fs::write(
            path.join(name),
            serde_json::to_vec(&serde_json::json!({"width":width,"height":height,"cells":cells}))
                .unwrap(),
        )
        .unwrap();
    }
    (text, buffer)
}

fn key(app: &mut App, code: KeyCode) {
    crate::ui_modal::handle_key(app, KeyEvent::new(code, KeyModifiers::NONE));
}

fn paste(app: &mut App, index: usize, text: &str) {
    let id = TextInputId::Exclusion(index);
    assert!(
        app.edit_text_input(id, KeyCode::Null, KeyModifiers::NONE, Some(text))
            .changed
    );
    app.text_input_changed(id);
}

fn click(app: &mut App, action: Action) {
    render(app, 100, 35, false);
    let area = app
        .exclusion_settings
        .hits
        .iter()
        .find(|(_, a)| *a == action)
        .unwrap()
        .0;
    assert!(crate::ui_modal::handle_mouse(
        app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x,
            row: area.y,
            modifiers: KeyModifiers::NONE
        }
    ));
}

#[test]
fn exclusions_settings_render_own_defaults_and_add_patterns_with_mouse_and_keyboard() {
    for width in [46, 100, 140] {
        let mut app = App::test_new();
        app.settings_editor_active = true;
        app.settings_focus_categories = false;
        app.active_pane = crate::app::ActivePane::Editor;
        app.settings_category_selected = 3;
        app.settings_editor_search = "search exclude".into();
        app.settings_selected = app
            .settings_items()
            .iter()
            .position(|(_, _, id)| id == "search_exclude")
            .unwrap();
        app.ensure_settings_selection_visible();
        let (text, buffer) = render(&mut app, width, 40, true);
        assert!(text.contains("**/node_modules"));
        assert!(text.contains("**/bower_components"));
        assert!(text.contains("**/*.code-search"));
        assert!(
            !text.contains("**/.git"),
            "inherited patterns should not be displayed in Search: Exclude"
        );
        let index = buffer
            .content
            .windows(11)
            .position(|cells| cells.iter().map(|c| c.symbol()).collect::<String>() == "Add Pattern")
            .unwrap();
        crate::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: (index % width as usize) as u16,
                row: (index / width as usize) as u16,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(
            app.exclusion_settings.editing,
            "Add Pattern at width {width}"
        );
        assert_eq!(app.exclusion_settings.key, "search.exclude");
        paste(&mut app, 0, "**/generated");
        key(&mut app, KeyCode::Enter);
        assert_eq!(
            app.exclusion_settings.focused_input(),
            Some(TextInputId::Exclusion(1))
        );
        paste(&mut app, 1, "$(basename).ts");
        key(&mut app, KeyCode::Enter);
        assert!(!app.exclusion_settings.editing);
        assert_eq!(
            app.settings_global.search_exclude["**/generated"],
            serde_json::json!({"when":"$(basename).ts"})
        );
        click(&mut app, Action::Close);
        assert!(crate::ui_modal::active(&app).is_none());
    }
}

#[test]
fn exclusions_settings_scoped_writes_preserve_other_fields_and_reset_to_inherited_values() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::test_new();
    app.settings_global_path = dir.path().join("user.json");
    app.settings_workspace_path = dir.path().join("workspace.json");
    app.settings_persistence_enabled = true;
    std::fs::write(&app.settings_workspace_path,serde_json::to_vec(&serde_json::json!({
        "untouched":{"custom":true},"files.exclude":{"**/keep":true},"search.exclude":{"**/old":true}
    })).unwrap()).unwrap();
    app.settings_workspace = crate::settings::load_from_file(&app.settings_workspace_path);
    app.settings_scope = SettingsScope::Workspace;
    app.open_exclusion_settings("search_exclude", None, true);
    paste(&mut app, 0, "**/.git");
    click(&mut app, Action::Enabled);
    click(&mut app, Action::Save);
    assert_eq!(app.settings_workspace.search_exclude["**/.git"], false);
    assert!(app.settings_global.search_exclude.is_empty());
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&app.settings_workspace_path).unwrap()).unwrap();
    assert_eq!(document["untouched"]["custom"], true);
    assert_eq!(document["files.exclude"]["**/keep"], true);
    assert_eq!(document["search.exclude"]["**/old"], true);
    app.open_exclusion_settings("search_exclude", Some("**/.git".into()), false);
    click(&mut app, Action::Reset);
    assert!(
        !app.settings_workspace
            .search_exclude
            .contains_key("**/.git")
    );
    // Resetting the final map entry must write an object, not a null value that
    // cannot be deserialized back into Settings.
    app.save_exclusion_pattern(
        SettingsScope::Workspace,
        "search.exclude",
        None,
        "**/old",
        None,
    )
    .unwrap();
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&app.settings_workspace_path).unwrap()).unwrap();
    assert_eq!(document["search.exclude"], serde_json::json!({}));
    let loaded: crate::settings::Settings = serde_json::from_value(document).unwrap();
    assert!(loaded.search_exclude.is_empty());
    assert_eq!(
        app.settings_effective.search_exclude,
        crate::settings::default_search_exclude()
    );
    app.reset_setting_in_selected_scope("files_exclude");
    assert!(app.settings_workspace.files_exclude.is_empty());
    assert_eq!(
        app.settings_effective.files_exclude,
        crate::settings::default_files_exclude()
    );
}

#[test]
fn exclusions_settings_geometry_rebuilds_when_pattern_count_changes_and_dialogs_fit_small_views() {
    let mut app = App::test_new();
    let items = app.settings_items();
    let index = items
        .iter()
        .position(|(_, _, id)| id == "files_exclude")
        .unwrap();
    let first = app.settings_layout_cache.get(&items, &[index], 80);
    app.settings_global
        .files_exclude
        .insert("**/added".into(), serde_json::json!(true));
    let next = app
        .settings_layout_cache
        .get(&app.settings_items(), &[index], 80);
    assert_eq!(next.total, first.total + 1);
    for (width, height) in [(18, 7), (40, 12), (90, 32)] {
        app.open_exclusion_settings("files_exclude", None, false);
        render(&mut app, width, height, false);
        key(&mut app, KeyCode::Enter);
        render(&mut app, width, height, false);
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Esc);
        assert!(crate::ui_modal::active(&app).is_none());
    }
}
