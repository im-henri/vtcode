use super::*;
use crate::{
    app::{EditorGroup, Tab},
    editor::Editor,
    scope_bubble_config::Rules,
};
use ratatui::{Terminal, backend::TestBackend};
use std::sync::Arc;

fn fixture() -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::test_new();
    app.settings_global_path = dir.path().join("user.json");
    app.settings_workspace_path = dir.path().join("workspace.json");
    app.settings_persistence_enabled = true;
    let mut editor = Editor::test_with_text("custom {\n work();\n}\n");
    editor.language_override = Some("My DSL".into());
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    (dir, app)
}
fn render(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|f| crate::ui_modal::draw_active(f, app))
        .unwrap();
    if let Some(folder) = std::env::var_os("VTCODE_SCOPE_SETTINGS_CAPTURE") {
        let buffer = terminal.backend().buffer();
        let cells: Vec<_> = buffer.content.iter().map(|cell| serde_json::json!({
            "text":cell.symbol(), "fg":format!("{:?}",cell.fg), "bg":format!("{:?}",cell.bg),
            "bold":cell.modifier.contains(ratatui::style::Modifier::BOLD),
        })).collect();
        let folder = std::path::PathBuf::from(folder);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join(format!(
                "scope-{}-{width}-{height}.json",
                if app.scope_bubble_settings.editing {
                    "edit"
                } else {
                    "table"
                }
            )),
            serde_json::to_vec(&serde_json::json!({"width":width,"height":height,"cells":cells}))
                .unwrap(),
        )
        .unwrap();
    }
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect()
}
fn click(app: &mut App, action: Action) {
    render(app, 100, 35);
    let area = app
        .scope_bubble_settings
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
            modifiers: KeyModifiers::NONE
        }
    ));
}
fn key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    crate::ui_modal::handle_key(app, KeyEvent::new(code, mods));
}
fn rule(syntax: Syntax, delimiters: &[Delimiter]) -> Config {
    Config {
        syntax,
        delimiters: Some(delimiters.to_vec()),
        language_blocks: false,
    }
}

#[test]
fn scope_profile_ui_mouse_save_persists_and_updates_existing_and_new_editors() {
    let (dir, mut app) = fixture();
    let mut other = Editor::test_with_text("{\n value\n}");
    other.language_override = Some("My DSL".into());
    app.groups.push(EditorGroup {
        tabs: vec![Tab::new_file(other)],
        ..Default::default()
    });
    app.open_scope_bubble_settings();
    let text = render(&mut app, 100, 35);
    assert!(text.contains("Using built-in language rules."));
    click(&mut app, Action::New);
    click(&mut app, Action::ChooseSyntax);
    // Scroll the dropdown with keyboard; C++ is immediately visible.
    click(&mut app, Action::PickSyntax(Syntax::Cpp));
    click(&mut app, Action::Pair(Delimiter::Parentheses));
    click(&mut app, Action::Blocks);
    assert!(render(&mut app, 100, 35).contains("Parentheses ( )"));
    click(&mut app, Action::Save);
    assert!(
        !app.scope_bubble_settings.editing,
        "{:?}",
        app.scope_bubble_settings.error
    );
    let saved: crate::settings::Settings =
        serde_json::from_slice(&std::fs::read(dir.path().join("user.json")).unwrap()).unwrap();
    let expected = rule(Syntax::Cpp, &[Delimiter::Braces, Delimiter::Parentheses]);
    assert_eq!(
        saved.animation_scope_bubble_languages.unwrap()["my dsl"],
        expected
    );
    for tab in app
        .tabs
        .iter()
        .chain(app.groups.iter().flat_map(|g| &g.tabs))
    {
        assert_eq!(
            tab.editor.scope_bubble_rules.get("MY DSL").unwrap().syntax,
            Syntax::Cpp
        );
    }
    assert_eq!(
        app.new_editor().scope_bubble_rules.configs["my dsl"],
        expected
    );
    let table = render(&mut app, 100, 35);
    assert!(
        table.contains("Language")
            && table.contains("Syntax")
            && table.contains("Scopes")
            && table.contains("my dsl")
    );
}

#[test]
fn scope_profile_ui_workspace_inheritance_reset_and_cache_identity() {
    let (_dir, mut app) = fixture();
    let user = rule(Syntax::C, &[Delimiter::Braces]);
    app.write_scope_bubble_rule(SettingsScope::Global, None, "My DSL", Some(user.clone()))
        .unwrap();
    app.write_scope_bubble_rule(
        SettingsScope::Global,
        None,
        "C++",
        Some(rule(Syntax::Automatic, &Delimiter::ALL)),
    )
    .unwrap();
    let before = app.editor().unwrap().scope_bubble_rules.clone();
    let effective = app.settings_for_scope(SettingsScope::Workspace);
    app.apply_resolved_settings(&effective);
    assert!(
        Arc::ptr_eq(&before, &app.editor().unwrap().scope_bubble_rules),
        "unrelated settings replay must not discard warm caches"
    );
    app.open_scope_bubble_settings();
    click(&mut app, Action::Scope(SettingsScope::Workspace));
    click(&mut app, Action::Rule(1));
    assert!(render(&mut app, 100, 35).contains("Inherited from User."));
    assert!(!app.scope_bubble_settings.can_reset());
    click(&mut app, Action::Pair(Delimiter::Brackets));
    click(&mut app, Action::Save);
    assert!(
        app.settings_workspace
            .animation_scope_bubble_languages
            .as_ref()
            .unwrap()
            .contains_key("my dsl")
    );
    assert_eq!(app.animation_settings.scope_languages.configs.len(), 2);
    click(&mut app, Action::Rule(1));
    click(&mut app, Action::Reset);
    assert!(
        app.settings_workspace
            .animation_scope_bubble_languages
            .is_none()
    );
    assert_eq!(
        app.editor().unwrap().scope_bubble_rules.configs["my dsl"],
        user
    );
    assert_eq!(app.animation_settings.scope_languages.configs.len(), 2);
}

#[test]
fn scope_profile_ui_keyboard_text_history_cancel_validation_and_disable() {
    let (_dir, mut app) = fixture();
    app.open_scope_bubble_settings();
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        app.scope_bubble_settings.focused_input(),
        Some(TextInputId::ScopeBubbleLanguage)
    );
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert!(
        app.edit_text_input(
            TextInputId::ScopeBubbleLanguage,
            KeyCode::Null,
            KeyModifiers::NONE,
            Some("Other DSL")
        )
        .changed
    );
    app.text_input_changed(TextInputId::ScopeBubbleLanguage);
    assert_eq!(app.scope_bubble_settings.field.text, "Other DSL");
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(app.scope_bubble_settings.field.text, "My DSL");
    activate(&mut app, Action::Save);
    assert!(
        app.scope_bubble_settings
            .error
            .as_ref()
            .unwrap()
            .contains("syntax style")
    );
    activate(&mut app, Action::PickSyntax(Syntax::Disabled));
    assert!(app.scope_bubble_settings.error.is_none());
    assert!(!render(&mut app, 100, 35).contains("Delimiter scopes"));
    activate(&mut app, Action::Save);
    assert_eq!(
        app.animation_settings
            .scope_languages
            .get("My DSL")
            .unwrap()
            .syntax,
        Syntax::Disabled
    );
    click(&mut app, Action::Rule(0));
    activate(&mut app, Action::PickSyntax(Syntax::Rust));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.scope_bubble_settings.editing);
    assert_eq!(
        app.animation_settings
            .scope_languages
            .get("My DSL")
            .unwrap()
            .syntax,
        Syntax::Disabled
    );
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.scope_bubble_settings.active);
}

#[test]
fn scope_profile_ui_language_picker_resolves_aliases_and_keeps_existing_rules() {
    let (_dir, mut app) = fixture();
    app.write_scope_bubble_rule(
        SettingsScope::Global,
        None,
        "cpp",
        Some(rule(Syntax::Automatic, &Delimiter::ALL)),
    )
    .unwrap();
    app.open_scope_bubble_settings();
    activate(&mut app, Action::New);
    app.scope_bubble_settings.field = Field::new("C++".into());
    activate(&mut app, Action::ChooseLanguage);
    let choices = app.scope_bubble_settings.choices();
    assert!(!choices.is_empty());
    click(&mut app, choices[0]);
    assert_eq!(app.scope_bubble_settings.original.as_deref(), Some("cpp"));
    assert_eq!(app.scope_bubble_settings.pairs(), Delimiter::ALL);
}

#[test]
fn scope_profile_ui_narrow_layout_scroll_focus_and_dropdown_hit_regions() {
    let (_dir, mut app) = fixture();
    app.open_scope_bubble_settings();
    activate(&mut app, Action::New);
    activate(&mut app, Action::PickSyntax(Syntax::Cpp));
    for (width, height) in [(100, 35), (48, 18), (32, 12), (22, 9), (12, 5)] {
        app.scope_bubble_settings.focus = Action::Pair(Delimiter::Brackets);
        app.scope_bubble_settings.reveal_focus = true;
        render(&mut app, width, height);
        let viewport = Rect::new(0, 0, width, height);
        for (rect, _) in &app.scope_bubble_settings.hits {
            assert_eq!(rect.intersection(viewport), *rect);
        }
        if width >= 22 && height >= 9 {
            assert!(
                app.scope_bubble_settings
                    .hits
                    .iter()
                    .any(|(_, a)| *a == Action::Pair(Delimiter::Brackets))
            );
            assert!(
                app.scope_bubble_settings
                    .hits
                    .iter()
                    .any(|(_, a)| *a == Action::Save)
            );
        }
    }
    app.scope_bubble_settings.focus = Action::ChooseSyntax;
    app.scope_bubble_settings.reveal_focus = true;
    activate(&mut app, Action::ChooseSyntax);
    render(&mut app, 48, 18);
    let popup = app.scope_bubble_settings.popup;
    for (rect, action) in &app.scope_bubble_settings.hits {
        if rect.intersects(popup) {
            assert!(matches!(action, Action::PickSyntax(_)));
        }
    }
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.scope_bubble_settings.menu.is_none() && app.scope_bubble_settings.editing);
}

#[test]
fn scope_profile_ui_setting_entry_is_searchable_and_opens_correct_scope() {
    let (_dir, mut app) = fixture();
    let id = "animation_scope_bubble_languages";
    assert_eq!(App::setting_field_kind(id), "action");
    assert_eq!(
        App::setting_json_key(id),
        Some("animationScopeBubbleLanguages")
    );
    assert!(app.settings_items().iter().any(|(_, _, key)| key == id));
    app.settings_editor_active = true;
    app.settings_scope = SettingsScope::Workspace;
    app.toggle_setting(id);
    assert_eq!(
        crate::ui_modal::active(&app),
        Some(crate::ui_modal::Kind::ScopeBubbleSettings)
    );
    assert_eq!(app.scope_bubble_settings.scope, SettingsScope::Workspace);
    assert!(Arc::ptr_eq(
        &Rules::empty(),
        &app.new_editor().scope_bubble_rules
    ));
}
