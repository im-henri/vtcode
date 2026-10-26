use super::*;
use crate::syntax_install::{install, source_path};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
use std::{fs, path::Path, time::Duration};
use syntect::parsing::{SyntaxSet, SyntaxSetBuilder};

const GRAMMAR: &str = "%YAML 1.2\n---\nname: Installed Example\nfile_extensions: [syntaxtest]\nscope: source.syntaxtest\ncontexts:\n  main:\n    - match: magic\n      scope: keyword.control.syntaxtest\n";

fn plain() -> SyntaxSet {
    let mut builder = SyntaxSetBuilder::new();
    builder.add_plain_text_syntax();
    builder.build()
}

#[test]
fn paths_are_local_and_relative_to_the_workspace() {
    let dir = tempfile::tempdir().unwrap();
    for input in [
        "",
        "   ",
        "\"\"",
        "https://example.org/test.sublime-syntax",
        "HTTP:example.json",
        "file:///test.json",
        "ftp://host/test.json",
        "archive.zip",
        "file.tmLanguage",
    ] {
        assert!(source_path(input, dir.path()).is_err(), "{input}");
    }
    for input in [
        "new syntax.sublime-syntax",
        "  \"new syntax.sublime-syntax\"  ",
        "'new syntax.sublime-syntax'",
    ] {
        assert_eq!(
            source_path(input, dir.path()).unwrap(),
            dir.path().join("new syntax.sublime-syntax")
        );
    }
    let absolute = dir.path().join("語法.JSON");
    assert_eq!(
        source_path(
            &format!("\"{}\"", absolute.display()),
            Path::new("elsewhere")
        )
        .unwrap(),
        absolute
    );
}

#[test]
fn validated_install_is_persistent_and_invalid_replacements_preserve_the_old_syntax() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Example.SUBLIME-SYNTAX");
    let root = dir.path().join("installed");
    fs::write(&source, GRAMMAR).unwrap();
    let installed = install(&source, &root, &plain()).unwrap();
    assert_eq!(installed.name, "Installed Example");
    let installed_path = root.join("Example.sublime-syntax");
    assert!(installed_path.is_file());
    assert_eq!(fs::read_to_string(&source).unwrap(), GRAMMAR);
    let loaded = crate::syntax_assets::build_syntax_set(&[root.clone()]).unwrap();
    assert_eq!(
        loaded.find_syntax_by_extension("syntaxtest").unwrap().name,
        installed.name
    );

    for invalid in [
        "missing: grammar",
        "%YAML 1.2\n---\nname: Bad\nscope: source.bad\ncontexts:\n  main:\n    - include: scope:source.missing_dependency\n",
        "%YAML 1.2\n---\nname: Bad\nscope: source.bad\ncontexts:\n  main:\n    - include: missing_context\n",
        "%YAML 1.2\n---\nname: Bad\nscope: source.bad\ncontexts:\n  main:\n    - match: magic\n      push: scope:source.missing_dependency\n",
        "%YAML 1.2\n---\nname: Bad\nscope: source.bad\ncontexts:\n  main:\n    - match: '['\n",
        "%YAML 1.2\n---\nname: Bad\nscope: source.bad\nextends: Missing.sublime-syntax\ncontexts:\n  main: []\n",
    ] {
        fs::write(&source, invalid).unwrap();
        assert!(install(&source, &root, &loaded).is_err(), "{invalid}");
        assert_eq!(fs::read_to_string(&installed_path).unwrap(), GRAMMAR);
        assert_eq!(
            fs::read_dir(&root).unwrap().count(),
            1,
            "no staging files left behind"
        );
    }
    let updated = GRAMMAR.replace("Installed Example", "Updated Example");
    fs::write(&source, &updated).unwrap();
    install(&source, &root, &loaded).unwrap();
    assert_eq!(fs::read_to_string(&installed_path).unwrap(), updated);
    // Installing a file already in the syntax directory remains safe.
    install(&installed_path, &root, &loaded).unwrap();
    assert_eq!(fs::read_to_string(&installed_path).unwrap(), updated);

    // References to installed grammars, local contexts, and inherited rules
    // remain valid; only unresolved references should reject an installation.
    let dependent = dir.path().join("Dependent.sublime-syntax");
    fs::write(&dependent, "%YAML 1.2\n---\nname: Dependent\nscope: source.dependent\nextends: Example.sublime-syntax\ncontexts:\n  main:\n    - include: local\n  local:\n    - include: scope:source.syntaxtest\n").unwrap();
    install(&dependent, &root, &loaded).unwrap();
}

#[test]
fn textmate_json_keeps_its_name_extensions_and_rules_without_a_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Custom.tmLanguage.json");
    let json = r#"{"name":"Custom TextMate","scopeName":"source.custom","fileTypes":["custom"],"patterns":[{"match":"magic","name":"keyword.control.custom"}]}"#;
    fs::write(&source, json).unwrap();
    let root = dir.path().join("installed");
    let installed = install(&source, &root, &plain()).unwrap();
    let loaded = crate::syntax_assets::build_syntax_set(&[root.clone()]).unwrap();
    let syntax = loaded.find_syntax_by_extension("custom").unwrap();
    assert_eq!(syntax.name, "Custom TextMate");
    assert!(!syntax.hidden);
    assert_eq!(fs::read_to_string(source).unwrap(), json);
    assert_eq!(installed.name, "Custom TextMate");
    assert!(root.join("Custom.tmLanguage.sublime-syntax").is_file());
    let themes = syntect::highlighting::ThemeSet::load_defaults();
    let mut highlighter =
        syntect::easy::HighlightLines::new(syntax, &themes.themes["base16-ocean.dark"]);
    let spans = highlighter
        .highlight_line("magic other\n", &loaded)
        .unwrap();
    assert_ne!(
        spans.first().unwrap().0.foreground,
        spans.last().unwrap().0.foreground
    );
}

#[test]
fn missing_folders_binary_and_oversized_sources_cannot_create_an_install() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.sublime-syntax");
    let root = dir.path().join("installed");
    assert!(install(&source, &root, &plain()).is_err());
    fs::create_dir(&source).unwrap();
    assert!(install(&source, &root, &plain()).is_err());
    fs::remove_dir(&source).unwrap();
    fs::write(&source, [0xff, 0xfe]).unwrap();
    assert!(
        install(&source, &root, &plain())
            .unwrap_err()
            .contains("UTF-8")
    );
    fs::File::create(&source)
        .unwrap()
        .set_len(16 * 1024 * 1024 + 1)
        .unwrap();
    assert!(
        install(&source, &root, &plain())
            .unwrap_err()
            .contains("16 MiB")
    );
    assert!(!root.exists());
}

#[test]
fn syntax_command_is_discoverable_and_rejects_links_from_keyboard_and_mouse() {
    for mouse in [false, true] {
        let mut app = App::test_new();
        app.open_palette();
        app.palette.query = "install syntax".into();
        app.update_palette_filter();
        assert!(
            app.palette
                .filtered
                .iter()
                .any(|&index| app.palette.commands[index].id == "install_syntax")
        );
        app.execute_palette_command("install_syntax");
        assert!(app.palette.install_syntax_mode);
        assert!(!app.palette.install_theme_mode && !app.palette.install_extension_mode);
        assert!(
            app.menu_items(3)
                .iter()
                .any(|(_, command)| *command == "install_syntax")
        );
        app.paste_into_focused_text_input("https://example.org/Example.sublime-syntax");
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| crate::ui_pickers::draw_command_palette(frame, &mut app))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Install Syntax"));
        assert!(text.contains("Syntax file path"));
        assert!(!text.contains("Get VSIX"));
        assert_eq!(app.modal.buttons.len(), 2);
        if mouse {
            let button = app.modal.buttons[0].0;
            crate::mouse::handle_mouse(
                &mut app,
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: button.x,
                    row: button.y,
                    modifiers: KeyModifiers::NONE,
                },
            );
        } else {
            crate::input_keyboard::handle_keyboard_event(
                &mut app,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            );
        }
        assert!(!app.palette.syntax_install_running);
        assert!(
            app.palette
                .theme_install_error
                .as_deref()
                .unwrap()
                .contains("Web links")
        );
        crate::input_pickers::handle_palette_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.theme_install_prompt_visible());
    }
}

#[test]
fn installation_reloads_open_files_and_the_language_picker_without_restart() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Example.sublime-syntax");
    fs::write(&source, GRAMMAR).unwrap();
    let root = dir.path().join("installed");
    fs::create_dir(&root).unwrap();
    let mut app = App::new(dir.path().to_owned());
    app.highlighter = crate::highlight::Highlighter::for_syntax_folder(&root);
    let mut editor = crate::editor::Editor::test_with_text("magic other\n");
    editor.file_path = Some(dir.path().join("open.syntaxtest"));
    editor.cursor_col = 3;
    app.tabs = vec![crate::app::Tab::new_file(editor)];
    app.active_tab = 0;
    app.syntax_theme = "dark-plus".into();
    app.open_syntax_install_prompt();
    app.palette.query = source.to_string_lossy().into_owned();
    app.start_syntax_install_in(root.clone());
    assert!(app.palette.syntax_install_running);
    app.wait_for_background(Duration::from_secs(10), |app| {
        !app.palette.syntax_install_running
            && !app.bg_syntax_asset_refresh_running
            && app
                .highlighter
                .syntax_names()
                .contains(&"Installed Example".into())
    });
    assert!(!app.palette.active);
    assert_eq!(app.editor().unwrap().rope, "magic other\n");
    assert_eq!(app.editor().unwrap().cursor_col, 3);
    assert!(
        app.palette
            .commands
            .iter()
            .any(|command| command.name == "Language: Installed Example")
    );
    assert!(
        app.notification_history
            .iter()
            .any(|notice| notice.0.starts_with("Installed syntax: Installed Example"))
    );
    let reloaded = crate::highlight::Highlighter::for_syntax_folder(&root);
    assert!(
        reloaded
            .syntax_names()
            .contains(&"Installed Example".into())
    );
    app.wait_for_background(Duration::from_secs(10), |app| {
        !app.background.has_active_jobs() && !app.background.has_pending_results()
    });
    let path = app.editor().unwrap().syntax_file_path().map(Path::to_owned);
    let spans = app
        .highlighter
        .highlighted_line_by_index_if_cached(0, path.as_deref(), None, &app.syntax_theme, 0)
        .unwrap()
        .0;
    assert_ne!(
        spans.first().unwrap().0.foreground,
        spans.last().unwrap().0.foreground
    );
}

#[test]
fn busy_close_retry_and_old_installer_results_do_not_change_another_dialog() {
    let mut app = App::test_new();
    app.open_syntax_install_prompt();
    let generation = app.palette.syntax_install_generation;
    app.palette.query = "Example.sublime-syntax".into();
    app.palette.cursor = app.palette.query.len();
    app.palette.syntax_install_running = true;
    assert!(app.paste_into_focused_text_input("ignored"));
    assert_eq!(app.palette.query, "Example.sublime-syntax");
    app.finish_syntax_install(generation, Err("Invalid syntax".into()));
    assert!(!app.palette.syntax_install_running);
    assert_eq!(
        app.palette.theme_install_error.as_deref(),
        Some("Invalid syntax")
    );
    app.close_palette();
    app.open_syntax_install_prompt();
    app.palette.query = "Another.sublime-syntax".into();
    app.finish_syntax_install(generation, Err("Older failure".into()));
    assert!(app.palette.theme_install_error.is_none());
    app.finish_theme_install(Ok(Vec::new()));
    app.finish_theme_install(Err("Older theme failure".into()));
    assert!(app.palette.install_syntax_mode);
    assert_eq!(app.palette.query, "Another.sublime-syntax");
    assert!(app.palette.theme_install_error.is_none());
    for (width, height) in [(100, 30), (40, 12), (12, 5), (1, 1)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        for running in [false, true] {
            app.palette.syntax_install_running = running;
            terminal
                .draw(|frame| crate::ui_pickers::draw_command_palette(frame, &mut app))
                .unwrap();
        }
    }
    app.open_extension_install_prompt();
    assert!(!app.palette.install_syntax_mode);
    assert!(app.palette.install_extension_mode);
}
