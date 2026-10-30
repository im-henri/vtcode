use crate::app::{App, Tab};
use crate::editor::Editor;
use crate::animation_structural_scope::StructuralScopeRequest;
use crate::highlight::Highlighter;
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{backend::TestBackend, Terminal};
use std::{path::Path, time::Duration};

fn write_grammar(folder: &Path, name: &str, scope: &str) {
    std::fs::create_dir_all(folder).unwrap();
    std::fs::write(folder.join("Example.tmLanguage.json"), serde_json::json!({
        "name":name,"scopeName":"source.reloadtest","fileTypes":["reloadtest"],
        "patterns":[{"match":"magic","name":scope}]
    }).to_string()).unwrap();
}

fn test_app(root: &Path) -> App {
    let mut app = App::new(root.to_path_buf());
    app.highlighter = Highlighter::for_syntax_folder(&root.join("syntaxes"));
    let mut editor = Editor::test_with_text("magic\n");
    editor.file_path = Some(root.join("example.reloadtest"));
    editor.cursor_col = 2;
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.syntax_theme = "dark-plus".into();
    app.refresh_palette_commands();
    app
}

fn wait_loaded(app: &mut App) {
    app.wait_for_background(Duration::from_secs(5), |app| {
        !app.bg_syntax_asset_refresh_running
    });
}

#[test]
fn custom_scope_defaults_update_open_documents_after_grammar_reload() {
    let directory = tempfile::tempdir().unwrap();
    let syntaxes = directory.path().join("syntaxes");
    std::fs::create_dir_all(&syntaxes).unwrap();
    let path = syntaxes.join("Example.tmLanguage.json");
    std::fs::write(&path, include_bytes!("testdata/scope-bubble-custom.tmLanguage.json")).unwrap();
    let mut app = test_app(directory.path());
    app.tabs[0].editor = Editor::test_with_text("module {\n work();\n}\n");
    app.tabs[0].editor.file_path = Some(directory.path().join("example.customscope"));
    app.tabs[0].editor.cursor_line = 1;
    assert!(StructuralScopeRequest::from_editor(app.editor().unwrap()).is_none());
    app.request_syntax_assets_async();
    wait_loaded(&mut app);
    let before = StructuralScopeRequest::from_editor(app.editor().unwrap()).unwrap();
    let automatic = app.highlighter.scope_bubble_automatic().unwrap();
    assert!(std::sync::Arc::ptr_eq(&automatic, &app.highlighter.scope_bubble_automatic().unwrap()));
    let rules = app.editor().unwrap().scope_bubble_rules.clone();
    app.apply_settings(&crate::settings::Settings::default());
    assert!(std::sync::Arc::ptr_eq(&rules, &app.editor().unwrap().scope_bubble_rules), "unrelated settings retain scope caches");
    assert!(before.matches(app.editor().unwrap()));
    std::fs::remove_file(&path).unwrap();
    app.request_syntax_assets_async();
    wait_loaded(&mut app);
    assert!(!before.matches(app.editor().unwrap()), "old grammar workers are rejected");
    assert!(StructuralScopeRequest::from_editor(app.editor().unwrap()).is_none());
}

fn first_color(app: &mut App) -> Option<crate::textmate::Color> {
    let path = app.editor().unwrap().file_path.clone();
    app.highlighter
        .highlighted_line_by_index_if_cached(0, path.as_deref(), None, &app.syntax_theme, 0)
        .and_then(|(spans, _)| spans.first().map(|(style, _)| style.foreground))
}

#[test]
fn syntax_reload_builds_on_first_launch_and_recolors_open_files_without_restarting() {
    let directory = tempfile::tempdir().unwrap();
    let syntaxes = directory.path().join("syntaxes");
    write_grammar(&syntaxes, "First", "keyword.control.reloadtest");
    let mut app = test_app(directory.path());
    assert!(!app.highlighter.syntax_names().contains(&"First".into()));
    app.refresh_visible_highlights_async();
    app.request_syntax_assets_async();
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    assert!(app.background.activity.active.iter().any(|task| task.label == "Loading syntaxes"));
    let &(_, width, action) = app.status_bar_items.last().expect("compact activity control");
    assert_eq!(action, "performance");
    assert!(width <= 3);
    wait_loaded(&mut app);
    assert!(syntaxes.join(".cache/textmate-registry.json.gz").is_file());
    assert!(syntaxes.join(".cache/source.fingerprint").is_file());
    app.wait_for_background(Duration::from_secs(5), |app| {
        !app.background.has_active_jobs() && !app.background.has_pending_results()
    });
    let before = first_color(&mut app).unwrap();
    assert_eq!((before.r, before.g, before.b), (197, 134, 192));
    assert!(app
        .palette
        .commands
        .iter()
        .any(|command| command.name == "Language: First"));

    // The real palette keyboard path must reload the registry and existing editor.
    write_grammar(&syntaxes, "Other", "comment.line.reloadtest");
    app.open_palette();
    app.palette.query = "Reload Syntaxes".into();
    app.update_palette_filter();
    assert_eq!(
        app.palette.commands[app.palette.filtered[app.palette.selected]].id,
        "reload_syntaxes"
    );
    crate::input_pickers::handle_palette_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.bg_syntax_asset_refresh_running);
    assert!(app.syntax_reload_requested);
    wait_loaded(&mut app);
    app.wait_for_background(Duration::from_secs(5), |app| {
        !app.background.has_active_jobs() && !app.background.has_pending_results()
    });
    assert_ne!(first_color(&mut app).unwrap(), before);
    assert!(app
        .palette
        .commands
        .iter()
        .any(|command| command.name == "Language: Other"));
    assert!(!app
        .palette
        .commands
        .iter()
        .any(|command| command.name == "Language: First"));
    assert_eq!(app.editor().unwrap().rope.to_string(), "magic\n");
    assert_eq!(app.editor().unwrap().cursor_col, 2);
    assert!(app
        .notification_history
        .iter()
        .any(|notice| notice.0 == "Syntaxes reloaded"));
}

#[test]
fn syntax_reload_menu_click_handles_requests_received_during_a_build() {
    let directory = tempfile::tempdir().unwrap();
    let syntaxes = directory.path().join("syntaxes");
    write_grammar(&syntaxes, "First", "keyword.control.reloadtest");
    let mut app = test_app(directory.path());
    app.request_syntax_assets_async();
    write_grammar(&syntaxes, "Latest", "comment.line.reloadtest");
    app.menu_active = true;
    app.menu_open = true;
    app.menu_selected = 3;
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let row = app
        .menu_items(3)
        .iter()
        .position(|(_, command)| *command == "reload_syntaxes")
        .unwrap();
    let column = crate::ui_chrome::menu_dropdown_anchor_x(&app) + 2;
    let row = app.menu_bar_area.unwrap().y + 2 + row as u16;
    crate::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        },
    );
    assert!(app.bg_syntax_asset_refresh_pending);
    wait_loaded(&mut app);
    assert!(app.highlighter.syntax_names().contains(&"Latest".into()));
    assert!(!app.highlighter.syntax_names().contains(&"First".into()));
    assert!(!app.syntax_reload_requested);
    assert!(!app.bg_syntax_asset_refresh_pending);
}

#[test]
fn syntax_reload_keeps_the_previous_registry_when_edited_grammars_are_invalid() {
    let directory = tempfile::tempdir().unwrap();
    let syntaxes = directory.path().join("syntaxes");
    write_grammar(&syntaxes, "Valid", "keyword.control.reloadtest");
    let mut app = test_app(directory.path());
    app.request_syntax_assets_async();
    wait_loaded(&mut app);
    let cache = std::fs::read(syntaxes.join(".cache/textmate-registry.json.gz")).unwrap();
    std::fs::write(syntaxes.join("Example.tmLanguage.json"), "contexts: [").unwrap();
    app.execute_palette_command("reload_syntaxes");
    wait_loaded(&mut app);
    assert!(app.highlighter.syntax_names().contains(&"Valid".into()));
    assert!(app
        .notification_history
        .iter()
        .any(|notice| notice.0.contains("Could not load syntaxes:")));
    assert_eq!(
        std::fs::read(syntaxes.join(".cache/textmate-registry.json.gz")).unwrap(),
        cache
    );
}

#[test]
fn syntax_reload_first_launch_still_highlights_when_the_cache_cannot_be_written() {
    let directory = tempfile::tempdir().unwrap();
    let syntaxes = directory.path().join("syntaxes");
    write_grammar(&syntaxes, "Valid", "keyword.control.reloadtest");
    // A file in place of the cache directory prevents writing on every OS,
    // without depending on administrator/root permission behavior.
    std::fs::write(syntaxes.join(".cache"), "blocked").unwrap();
    let mut app = test_app(directory.path());
    app.request_syntax_assets_async();
    wait_loaded(&mut app);
    app.wait_for_background(Duration::from_secs(5), |app| {
        !app.background.has_active_jobs() && !app.background.has_pending_results()
    });
    let color = first_color(&mut app).unwrap();
    assert_eq!((color.r, color.g, color.b), (197, 134, 192));
    assert!(app.highlighter.syntax_names().contains(&"Valid".into()));
    assert!(app
        .notification_history
        .iter()
        .any(|notice| { notice.0.contains("Syntaxes loaded for this session, but") }));
}
