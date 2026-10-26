use super::*;
use crate::{
    app::{EditorGroup, Tab},
    editor::Editor,
};
use ratatui::{Terminal, backend::TestBackend};
use std::{path::PathBuf, sync::Arc};

fn fixture(filename: &str) -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::test_new();
    app.settings_global_path = dir.path().join("user.json");
    app.settings_workspace_path = dir.path().join("workspace.json");
    app.settings_persistence_enabled = true;
    let mut editor = Editor::test_with_text("value = 1\n");
    editor.file_path = Some(dir.path().join(filename));
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    (dir, app)
}

fn render(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| crate::ui_modal::draw_active(frame, app))
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::ui_modal::handle_key(app, KeyEvent::new(code, modifiers));
}

fn click(app: &mut App, action: Action) {
    render(app, 100, 35);
    let area = app
        .file_associations
        .hits
        .iter()
        .find(|(_, candidate)| *candidate == action)
        .unwrap()
        .0;
    assert!(crate::ui_modal::handle_mouse(
        app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        }
    ));
}

fn capture(app: &mut App, name: &str, width: u16, height: u16) {
    let Some(folder) = std::env::var_os("VTCODE_FILE_ASSOCIATIONS_CAPTURE") else {
        return;
    };
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    let buffer = terminal.backend().buffer();
    let cells: Vec<_> = buffer.content.iter().map(|cell| serde_json::json!({
        "text": cell.symbol(), "fg": format!("{:?}", cell.fg), "bg": format!("{:?}", cell.bg),
        "bold": cell.modifier.contains(Modifier::BOLD)
    })).collect();
    let folder = std::path::PathBuf::from(folder);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join(format!("associations-{name}.json")),
        serde_json::to_vec(&serde_json::json!({
            "width": width, "height": height, "cells": cells
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn association_overview_is_a_compact_table_and_edit_fields_share_one_row() {
    for light in [false, true] {
        let (_dir, mut app) = fixture("model.cps");
        app.save_file_association(SettingsScope::Global, "*.cps", Some("C++"))
            .unwrap();
        app.save_file_association(SettingsScope::Global, "*.custom", Some("Python"))
            .unwrap();
        app.open_file_associations();
        if light {
            app.colors = crate::app_theme::EditorTheme::Light.colors();
        }
        let screen = render(&mut app, 100, 35);
        for text in [
            "Item", "Value", "*.cps", "C++", "*.custom", "Python", "Add Item", "Edit", "Remove",
        ] {
            assert!(screen.contains(text), "missing {text}");
        }
        assert!(!app.file_associations.editing);
        assert!(app.text_input_regions.is_empty());
        let top = app
            .file_associations
            .hits
            .iter()
            .map(|(rect, _)| rect.y)
            .min()
            .unwrap();
        let bottom = app
            .file_associations
            .hits
            .iter()
            .map(|(rect, _)| rect.bottom())
            .max()
            .unwrap();
        assert!(
            bottom - top < 12,
            "a short table must not occupy the full screen"
        );
        capture(&mut app, &format!("overview-light-{light}"), 100, 30);
        click(&mut app, Action::Rule(0));
        render(&mut app, 100, 35);
        let item = app
            .file_associations
            .hits
            .iter()
            .find(|(_, action)| *action == Action::Field(0))
            .unwrap()
            .0;
        let value = app
            .file_associations
            .hits
            .iter()
            .find(|(_, action)| *action == Action::Field(1))
            .unwrap()
            .0;
        assert_eq!(item.y, value.y);
        assert!(item.right() < value.x);
        capture(&mut app, &format!("editing-light-{light}"), 100, 30);
        app.file_associations.fields[0] = Field::new("*.unsaved".into());
        click(&mut app, Action::Cancel);
        assert!(!app.file_associations.editing);
        assert!(!app.language_associations.contains_key("*.unsaved"));
        click(&mut app, Action::Scope(SettingsScope::Workspace));
        assert!(render(&mut app, 100, 35).contains("(User)"));
        assert!(
            app.file_associations
                .hits
                .iter()
                .all(|(_, action)| !matches!(action, Action::Delete(_)))
        );
        capture(&mut app, &format!("workspace-light-{light}"), 100, 30);
        for (width, height) in [(60, 20), (42, 12), (20, 8)] {
            render(&mut app, width, height);
            let hits = &app.file_associations.hits;
            for (index, (rect, _)) in hits.iter().enumerate() {
                assert!(rect.right() <= width && rect.bottom() <= height);
                assert!(
                    hits[index + 1..]
                        .iter()
                        .all(|(other, _)| rect.intersection(*other).is_empty()),
                    "controls must not overlap on narrow screens"
                );
            }
            capture(
                &mut app,
                &format!("width-{width}-light-{light}"),
                width,
                height,
            );
        }
    }
}

#[test]
fn association_inline_rename_is_atomic_and_rejects_duplicate_keys() {
    let (_dir, mut app) = fixture("model.old");
    app.save_file_association(SettingsScope::Global, "*.old", Some("Python"))
        .unwrap();
    app.save_file_association(SettingsScope::Global, "*.taken", Some("Rust"))
        .unwrap();
    app.open_file_associations();
    click(&mut app, Action::Rule(0));
    app.file_associations.fields = [Field::new("*.new".into()), Field::new("cpp".into())];
    click(&mut app, Action::Save);
    assert!(!app.file_associations.editing);
    assert!(!app.language_associations.contains_key("*.old"));
    assert_eq!(app.language_associations["*.new"], "C++");
    let persisted = crate::settings::load_from_file(&app.settings_global_path)
        .language_associations
        .unwrap();
    assert!(!persisted.contains_key("*.old"));
    assert_eq!(persisted["*.new"], "C++");
    assert_eq!(persisted["*.taken"], "Rust");
    click(&mut app, Action::Rule(0));
    app.file_associations.fields[0] = Field::new("*.taken".into());
    click(&mut app, Action::Save);
    assert!(app.file_associations.editing);
    assert!(
        app.file_associations
            .error
            .as_ref()
            .unwrap()
            .contains("already exists")
    );
    assert_eq!(app.language_associations["*.new"], "C++");
    assert_eq!(app.language_associations["*.taken"], "Rust");
    capture(&mut app, "duplicate-error", 100, 30);
    app.file_associations.fields = [
        Field::new("*.next".into()),
        Field::new("missing-language".into()),
    ];
    click(&mut app, Action::Save);
    assert!(app.file_associations.editing);
    assert!(!app.language_associations.contains_key("*.next"));
    assert_eq!(app.language_associations["*.new"], "C++");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.file_associations.editing && app.file_associations.active);
}

#[test]
fn editing_at_the_end_of_a_long_table_reveals_the_input_and_language_choices() {
    let (_dir, mut app) = fixture("model.custom");
    app.settings_global.language_associations = Some(
        (0..30)
            .map(|index| (format!("*.extension{index:02}"), "Python".into()))
            .collect(),
    );
    app.open_file_associations();
    click(&mut app, Action::New);
    render(&mut app, 100, 30);
    assert!(
        app.file_associations
            .hits
            .iter()
            .any(|(_, action)| *action == Action::Field(0))
    );
    assert!(
        app.file_associations
            .hits
            .iter()
            .any(|(_, action)| *action == Action::Suggestion(0))
    );
    click(&mut app, Action::Suggestion(0));
    render(&mut app, 100, 30);
    assert!(
        app.file_associations
            .hits
            .iter()
            .any(|(_, action)| *action == Action::Field(1))
    );
    assert!(
        app.file_associations
            .hits
            .iter()
            .any(|(_, action)| matches!(action, Action::Language(_)))
    );
    capture(&mut app, "long-table-edit", 100, 30);
    click(&mut app, Action::Cancel);
    activate(&mut app, Action::Rule(29));
    render(&mut app, 42, 12);
    assert!(
        app.file_associations
            .hits
            .iter()
            .any(|(_, action)| *action == Action::Field(1))
    );
    assert!(
        app.file_associations
            .hits
            .iter()
            .any(|(_, action)| matches!(action, Action::Language(_)))
    );
}

#[test]
fn language_picker_keeps_all_choices_reachable_and_accepts_typing_after_navigation() {
    let (_dir, mut app) = fixture("model.custom");
    app.open_file_associations();
    capture(&mut app, "empty", 100, 30);
    click(&mut app, Action::New);
    click(&mut app, Action::Suggestion(0));
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    let choices = language_matches(&app.file_associations);
    assert!(choices.len() > 14);
    for _ in 0..14 {
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    }
    assert_eq!(app.file_associations.focus, Action::Language(choices[13]));
    render(&mut app, 100, 35);
    assert!(
        app.file_associations
            .hits
            .iter()
            .any(|(_, action)| *action == Action::Language(choices[13]))
    );
    capture(&mut app, "language-picker", 100, 30);
    let option = app
        .file_associations
        .hits
        .iter()
        .find(|(_, action)| *action == Action::Language(choices[13]))
        .unwrap()
        .0;
    crate::ui_modal::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: option.x,
            row: option.y,
            modifiers: KeyModifiers::NONE,
        },
    );
    assert_eq!(app.file_associations.focus, Action::Language(choices[16]));
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    for ch in "pyth".chars() {
        key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
    }
    assert_eq!(app.file_associations.fields[1].text, "pyth");
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.file_associations.fields[1].text, "Python");
    render(&mut app, 100, 35);
    assert!(
        app.file_associations
            .hits
            .iter()
            .all(|(_, action)| !matches!(action, Action::Language(_)))
    );
    click(&mut app, Action::Save);
    assert_eq!(app.language_associations["*.custom"], "Python");
    assert!(!app.file_associations.editing);
}

#[test]
fn association_is_discoverable_from_language_selector_and_settings() {
    let (_dir, mut app) = fixture("model.custom");
    app.execute_editor_command("change_language");
    let index = app
        .palette
        .filtered
        .iter()
        .position(|&index| app.palette.commands[index].id == "file_associations")
        .unwrap();
    app.palette.selected = index;
    crate::input_pickers::handle_palette_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.file_associations.active);
    assert!(!app.palette.active);
    assert_eq!(app.file_associations.fields[0].text, "*.custom");
    assert_eq!(
        app.file_associations.suggestions,
        ["*.custom", "model.custom"]
    );
    let screen = render(&mut app, 100, 35);
    for label in [
        "File Associations",
        "User",
        "Workspace",
        "Item",
        "Value",
        "Add Item",
        "Done",
    ] {
        assert!(screen.contains(label), "{label}");
    }
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    app.settings_editor_active = true;
    app.settings_scope = SettingsScope::Workspace;
    app.toggle_setting("language_associations");
    assert_eq!(app.file_associations.scope, SettingsScope::Workspace);
    assert!(app.focused_text_input_id().is_none());
    click(&mut app, Action::New);
    assert_eq!(
        app.focused_text_input_id(),
        Some(TextInputId::FileAssociation(0))
    );
    assert_eq!(App::settings_category_for_id("language_associations"), 4);
    assert_eq!(App::setting_field_kind("language_associations"), "action");
}

#[test]
fn association_search_keyboard_selection_and_mouse_save_apply_immediately() {
    let (_dir, mut app) = fixture("model.custom");
    app.open_file_associations();
    click(&mut app, Action::New);
    click(&mut app, Action::Suggestion(0));
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    for ch in "pyth".chars() {
        key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
    }
    assert_eq!(app.file_associations.fields[1].text, "pyth");
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert!(matches!(app.file_associations.focus, Action::Language(_)));
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.file_associations.fields[1].text, "Python");
    click(&mut app, Action::Save);
    assert!(app.file_associations.active);
    assert!(!app.file_associations.editing);
    let screen = render(&mut app, 100, 35);
    assert!(screen.contains("*.custom") && screen.contains("Python") && screen.contains("Remove"));
    assert_eq!(app.editor().unwrap().language_name(), "Python");
    assert_eq!(crate::formatters::language(app.editor().unwrap()), "python");
    let persisted = crate::settings::load_from_file(&app.settings_global_path);
    assert_eq!(
        persisted.language_associations.unwrap()["*.custom"],
        "Python"
    );
    assert!(!app.editor().unwrap().modified);
}

#[test]
fn association_scopes_inheritance_removal_and_open_groups_are_consistent() {
    let (_dir, mut app) = fixture("model.custom");
    let mut other = Editor::test_with_text("// text");
    other.file_path = Some(PathBuf::from("another.custom"));
    let mut group = EditorGroup::new();
    group.tabs.push(Tab::new_file(other));
    app.groups.push(group);
    app.save_file_association(SettingsScope::Global, "*.custom", Some("Python"))
        .unwrap();
    assert_eq!(
        app.groups.last().unwrap().tabs[0].editor.language_name(),
        "Python"
    );
    app.save_file_association(SettingsScope::Workspace, "*.custom", Some("C++"))
        .unwrap();
    assert_eq!(app.editor().unwrap().language_name(), "C++");
    app.open_file_associations();
    assert_eq!(app.file_associations.scope, SettingsScope::Workspace);
    activate(&mut app, Action::Rule(0));
    assert!(app.file_associations.can_remove());
    activate(&mut app, Action::Remove);
    assert_eq!(app.editor().unwrap().language_name(), "Python");
    assert_eq!(
        app.groups.last().unwrap().tabs[0].editor.language_name(),
        "Python"
    );
    app.settings_scope = SettingsScope::Workspace;
    app.toggle_setting("hover_enabled");
    assert_eq!(
        app.editor().unwrap().language_name(),
        "Python",
        "unrelated settings must not revert associations"
    );
    let mut restored = App::test_new();
    restored.settings_global = crate::settings::load_from_file(&app.settings_global_path);
    restored.settings_workspace = crate::settings::load_from_file(&app.settings_workspace_path);
    restored.apply_effective_settings();
    assert_eq!(restored.language_associations["*.custom"], "Python");
    app.save_file_association(SettingsScope::Global, "*.custom", None)
        .unwrap();
    assert_eq!(app.editor().unwrap().language_name(), "Plain Text");
}

#[test]
fn filename_rule_overrides_extension_and_current_temporary_mode() {
    let (_dir, mut app) = fixture("special.h");
    app.save_file_association(SettingsScope::Global, "*.h", Some("C"))
        .unwrap();
    app.editor_mut().unwrap().language_override = Some("Rust".into());
    app.open_file_associations();
    click(&mut app, Action::New);
    click(&mut app, Action::Suggestion(1));
    app.file_associations.fields[1] = Field::new("cpp".into());
    click(&mut app, Action::Save);
    assert_eq!(app.editor().unwrap().language_name(), "C++");
    assert!(app.editor().unwrap().language_override.is_none());
    assert_eq!(app.language_associations["*.h"], "C");
    app.open_file_associations();
    assert_eq!(app.file_associations.fields[0].text, "special.h");
    let index = app
        .file_associations
        .rules
        .iter()
        .position(|rule| rule.0 == "special.h")
        .unwrap();
    activate(&mut app, Action::Rule(index));
    activate(&mut app, Action::Remove);
    assert_eq!(app.editor().unwrap().language_name(), "C");
    app.editor_mut().unwrap().file_path = Some(PathBuf::from(".env"));
    app.open_file_associations();
    assert_eq!(app.file_associations.suggestions, [".env"]);
    assert_eq!(app.file_associations.fields[0].text, ".env");
}

#[test]
fn association_validation_cancel_and_failed_writes_do_not_change_runtime() {
    let (_dir, mut app) = fixture("model.custom");
    app.open_file_associations();
    activate(&mut app, Action::New);
    for (pattern, language) in [("src/**", "Python"), ("*.custom", "not a language")] {
        app.file_associations.fields = [Field::new(pattern.into()), Field::new(language.into())];
        activate(&mut app, Action::Save);
        assert!(app.file_associations.active);
        assert!(app.file_associations.error.is_some());
        assert!(app.language_associations.is_empty());
    }
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.file_associations.editing);
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.settings_global_path.exists());
    std::fs::write(&app.settings_global_path, "{invalid").unwrap();
    assert!(
        app.save_file_association(SettingsScope::Global, "*.custom", Some("Python"))
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(&app.settings_global_path).unwrap(),
        "{invalid"
    );
    assert!(app.language_associations.is_empty());
    std::fs::write(&app.settings_global_path, "{}").unwrap();
    let mut settings_editor = Editor::test_with_text("{}");
    settings_editor.file_path = Some(app.settings_global_path.clone());
    settings_editor.modified = true;
    app.tabs.push(Tab::new_file(settings_editor));
    assert!(
        app.save_file_association(SettingsScope::Global, "*.custom", Some("Python"))
            .unwrap_err()
            .contains("unsaved")
    );
    assert!(app.language_associations.is_empty());
}

#[test]
fn association_patch_preserves_unknown_and_unrelated_settings() {
    let (_dir, mut app) = fixture("model.custom");
    std::fs::write(
        &app.settings_global_path,
        r#"{"extension.custom":{"a":1},"tabSize":8,"languageAssociations":{"*.old":"Rust"}}"#,
    )
    .unwrap();
    app.save_file_association(SettingsScope::Global, "*.custom", Some("Python"))
        .unwrap();
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&app.settings_global_path).unwrap()).unwrap();
    assert_eq!(json["extension.custom"]["a"], 1);
    assert_eq!(json["tabSize"], 8);
    assert_eq!(json["languageAssociations"]["*.old"], "Rust");
    assert_eq!(json["languageAssociations"]["*.custom"], "Python");
}

#[test]
fn unsupported_rules_from_json_can_still_be_removed() {
    let (_dir, mut app) = fixture("model.custom");
    std::fs::write(
        &app.settings_global_path,
        r#"{"languageAssociations":{"src/**/*.py":"Python"}}"#,
    )
    .unwrap();
    app.settings_global = crate::settings::load_from_file(&app.settings_global_path);
    app.apply_effective_settings();
    app.open_file_associations();
    activate(&mut app, Action::Rule(0));
    activate(&mut app, Action::Remove);
    assert!(!app.file_associations.editing);
    assert!(app.language_associations.is_empty());
}

#[test]
fn association_modal_resizes_scrolls_and_drops_stale_hit_regions() {
    let (_dir, mut app) = fixture("model.custom");
    app.open_file_associations();
    for (width, height) in [(100, 35), (42, 12), (20, 8), (9, 3)] {
        render(&mut app, width, height);
        for (rect, _) in &app.file_associations.hits {
            assert!(rect.right() <= width && rect.bottom() <= height);
        }
        if width < 12 {
            assert!(app.text_input_regions.is_empty());
        }
    }
    assert!(app.file_associations.hits.is_empty());
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.file_associations.active);
    app.settings_global.language_associations = Some(
        (0..20)
            .map(|i| (format!("*.extension{i:02}"), "Python".into()))
            .collect(),
    );
    app.open_file_associations();
    render(&mut app, 42, 12);
    for _ in 0..12 {
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        render(&mut app, 42, 12);
    }
    assert!(app.file_associations.scroll > 0);
    assert!(
        app.file_associations
            .hits
            .iter()
            .any(|(_, action)| *action == Action::Close)
    );
}

fn config(language: &str, patterns: &[&str]) -> crate::lsp::LspServerConfig {
    crate::lsp::LspServerConfig {
        language: language.into(),
        command: "unused-test-server".into(),
        args: vec![],
        file_patterns: patterns.iter().map(|p| (*p).into()).collect(),
        settings: None,
        init_options: None,
    }
}

#[test]
fn association_routes_language_services_and_did_open_language_id() {
    let (dir, mut app) = fixture("model.custom");
    app.settings_global.lsp_servers = Some(vec![
        config("python", &["*.py"]),
        config("cpp", &["*.h", "*.cpp"]),
    ]);
    app.apply_effective_settings();
    app.save_file_association(SettingsScope::Global, "*.custom", Some("Python"))
        .unwrap();
    app.save_file_association(SettingsScope::Global, "special.h", Some("Python"))
        .unwrap();
    app.save_file_association(SettingsScope::Global, "*.module", Some("C++"))
        .unwrap();
    let (mut python, python_server) = crate::lsp::LspClient::test_pair("python", &["*.py"]);
    let (mut clang, clang_server) = crate::lsp::LspClient::test_pair("cpp", &["*.h", "*.cpp"]);
    python.file_associations = Arc::clone(&app.lsp_manager.file_associations);
    clang.file_associations = Arc::clone(&app.lsp_manager.file_associations);
    app.lsp_manager.clients = vec![clang, python];
    for (name, language, client) in [
        ("model.custom", "python", 1),
        ("special.h", "python", 1),
        ("main.module", "cpp", 0),
    ] {
        let path = dir.path().join(name);
        assert!(app.lsp_manager.has_config_for_file(&path));
        assert_eq!(app.lsp_manager.ensure_client_for_file(&path), Some(client));
        app.lsp_manager
            .sync_document_owned(&path, 0, "value = 1\n".into());
        app.lsp_manager.request_completion(&path, 0, 1, "v");
        let messages = if client == 1 {
            python_server.take_messages()
        } else {
            clang_server.take_messages()
        };
        let open = messages
            .iter()
            .find(|m| m["method"] == "textDocument/didOpen")
            .unwrap();
        assert_eq!(open["params"]["textDocument"]["languageId"], language);
        assert!(
            messages
                .iter()
                .any(|m| m["method"] == "textDocument/completion")
        );
    }
    let child_id = app.lsp_manager.clients[1].child.id();
    app.apply_effective_settings();
    assert_eq!(
        app.lsp_manager.clients[1].child.id(),
        child_id,
        "unchanged associations must not restart LSP"
    );
    app.save_file_association(SettingsScope::Global, "*.custom", Some("Plain Text"))
        .unwrap();
    assert!(
        !app.lsp_manager
            .has_config_for_file(&dir.path().join("model.custom"))
    );
    assert!(
        app.lsp_manager.clients.is_empty(),
        "old language responses must be retired on reassociation"
    );
}

#[test]
fn inherited_association_is_editable_but_not_removable_from_workspace() {
    let (_dir, mut app) = fixture("model.custom");
    app.save_file_association(SettingsScope::Global, "*.custom", Some("Python"))
        .unwrap();
    app.settings_editor_active = true;
    app.settings_scope = SettingsScope::Workspace;
    app.open_file_associations();
    activate(&mut app, Action::Rule(0));
    assert!(!app.file_associations.can_remove());
    // The inherited value is labeled in the overview, before editing it.
    activate(&mut app, Action::Cancel);
    assert!(render(&mut app, 100, 35).contains("(User)"));
    activate(&mut app, Action::Rule(0));
    activate(&mut app, Action::Remove);
    assert!(app.file_associations.active);
    assert_eq!(
        app.settings_global.language_associations.as_ref().unwrap()["*.custom"],
        "Python"
    );
    app.file_associations.fields[1] = Field::new("C".into());
    activate(&mut app, Action::Save);
    assert_eq!(
        app.settings_workspace
            .language_associations
            .as_ref()
            .unwrap()["*.custom"],
        "C"
    );
}

#[test]
fn suggestions_and_associations_work_for_named_unsaved_buffers() {
    let mut editor = Editor::test_with_text("value = 1");
    editor.suggested_file_name = Some("script.custom".into());
    editor
        .language_associations
        .insert("*.custom".into(), "Python".into());
    assert_eq!(editor.language_name(), "Python");
    editor.language_override = Some("C".into());
    assert_eq!(editor.language_name(), "C");
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.open_file_associations();
    assert_eq!(
        app.file_associations.suggestions,
        ["*.custom", "script.custom"]
    );
}
