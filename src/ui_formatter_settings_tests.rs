use super::*;
use crate::{app::Tab, editor::Editor};
use ratatui::{Terminal, backend::TestBackend};

fn key(app: &mut App, code: KeyCode) {
    crate::ui_modal::handle_key(app, KeyEvent::new(code, KeyModifiers::NONE));
}

fn replace_input(app: &mut App, value: &str) {
    crate::ui_modal::handle_key(
        app,
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
    );
    for ch in value.chars() {
        key(app, KeyCode::Char(ch));
    }
}

fn capture(app: &mut App, name: &str, width: u16, height: u16) {
    let Some(folder) = std::env::var_os("VTCODE_FORMATTER_CAPTURE") else {
        return;
    };
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    let cells: Vec<_> = terminal.backend().buffer().content.iter().map(|cell| serde_json::json!({
        "text": cell.symbol(), "fg": format!("{:?}", cell.fg), "bg": format!("{:?}", cell.bg),
        "bold": cell.modifier.contains(ratatui::style::Modifier::BOLD)
    })).collect();
    let folder = std::path::PathBuf::from(folder);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join(format!("formatter-{name}.json")),
        serde_json::to_vec(&serde_json::json!({
            "width": width, "height": height, "cells": cells
        }))
        .unwrap(),
    )
    .unwrap();
}

fn check_hits(app: &App, width: u16, height: u16) {
    let viewport = Rect::new(0, 0, width, height);
    for (index, (rect, action)) in app.formatter_settings.hits.iter().enumerate() {
        assert!(!rect.is_empty(), "empty {action:?}");
        assert_eq!(rect.intersection(viewport), *rect, "offscreen {action:?}");
        for (other, target) in &app.formatter_settings.hits[index + 1..] {
            assert!(
                !rect.intersects(*other),
                "overlap {action:?} and {target:?}"
            );
        }
    }
}

fn app() -> App {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text("value=1\n");
    editor.file_path = Some(std::path::PathBuf::from("sample.py"));
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app
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

fn click(app: &mut App, action: Action) {
    render(app, 100, 35);
    let area = app
        .formatter_settings
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

#[test]
fn formatter_settings_mouse_keyboard_scroll_and_validation() {
    let mut app = app();
    app.open_formatter_settings(None);
    assert!(
        app.formatter_settings.fields[1].text.is_empty(),
        "no formatter executable should be hardcoded for Python"
    );
    assert_eq!(
        crate::ui_modal::active(&app),
        Some(crate::ui_modal::Kind::FormatterSettings)
    );
    let screen = render(&mut app, 100, 35);
    for text in [
        "Formatter",
        "python",
        "Language Server",
        "User",
        "Workspace",
        "Save",
        "Cancel",
    ] {
        assert!(screen.contains(text), "missing {text}");
    }
    assert!(!screen.contains("External Command"));
    assert!(!screen.contains("Executable"));
    assert!(!screen.contains("Black"));
    click(&mut app, Action::ChooseProvider);
    let screen = render(&mut app, 100, 35);
    for text in [
        "Language Server",
        "External Command",
        "Disabled",
    ] {
        assert!(screen.contains(text), "missing formatter choice {text}");
    }
    assert!(!screen.contains("Black"));
    assert_eq!(menu_actions(&app.formatter_settings).len(), 3);
    click(&mut app, Action::Provider(Provider::Command));
    let screen = render(&mut app, 100, 35);
    assert!(screen.contains("Executable"));
    assert!(screen.contains("Arguments (1)"));
    assert!(screen.contains("Edit file in place"));
    assert!(screen.contains("Nothing is appended automatically"));
    assert_eq!(app.formatter_settings.config().args, ["{file}"]);
    click(&mut app, Action::Save);
    assert!(app.formatter_settings.active);
    assert!(
        app.formatter_settings
            .error
            .as_deref()
            .unwrap()
            .contains("executable")
    );
    assert_eq!(app.formatter_settings.focus, Action::Field(1));
    capture(&mut app, "validation", 100, 35);
    click(&mut app, Action::Field(1));
    assert_eq!(
        app.focused_text_input_id(),
        Some(TextInputId::FormatterSetting(1))
    );
    for c in "formatter.exe".chars() {
        crate::ui_modal::handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
        );
    }
    assert_eq!(app.formatter_settings.fields[1].text, "formatter.exe");
    assert!(app.formatter_settings.error.is_none());
    capture(&mut app, "external-command", 100, 35);
    for _ in 0..20 {
        activate(&mut app, Action::AddArgument);
    }
    for (width, height) in [(100, 35), (60, 20), (32, 10)] {
        app.formatter_settings.reveal_focus = true;
        let screen = render(&mut app, width, height);
        assert!(screen.contains("Save"));
        assert!(screen.contains("Cancel"));
        assert!(app.formatter_settings.scroll > 0);
        let focused = app.formatter_settings.focus;
        assert!(
            app.formatter_settings
                .hits
                .iter()
                .any(|(_, action)| *action == focused)
        );
        check_hits(&app, width, height);
        capture(&mut app, &format!("arguments-{width}"), width, height);
    }
    crate::ui_modal::handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!app.formatter_settings.active);
    assert!(app.formatters.is_empty());
}

#[test]
fn formatter_form_is_compact_with_separate_controls_and_optional_arguments() {
    for light in [false, true] {
        let mut app = app();
        app.open_formatter_settings(None);
        activate(&mut app, Action::Provider(Provider::Command));
        if light {
            app.colors = crate::app_theme::EditorTheme::Light.colors();
        }
        render(&mut app, 100, 35);
        check_hits(&app, 100, 35);
        let hits = &app.formatter_settings.hits;
        let top = hits.iter().map(|(rect, _)| rect.y).min().unwrap();
        let bottom = hits.iter().map(|(rect, _)| rect.bottom()).max().unwrap();
        assert!(
            bottom - top < 30,
            "external command form should fit its content"
        );
        assert!(app.formatter_settings.arguments_open);
        assert!(
            hits
                .iter()
                .any(|(_, action)| *action == Action::AddArgument)
        );
        let language = hits
            .iter()
            .find(|(_, action)| *action == Action::Field(0))
            .unwrap()
            .0;
        let provider = hits
            .iter()
            .find(|(_, action)| *action == Action::ChooseProvider)
            .unwrap()
            .0;
        let executable = hits
            .iter()
            .find(|(_, action)| *action == Action::Field(1))
            .unwrap()
            .0;
        assert_eq!(language.x, provider.x);
        assert_eq!(language.x, executable.x);
        capture(&mut app, &format!("command-light-{light}"), 100, 35);
        click(&mut app, Action::ChooseProvider);
        render(&mut app, 100, 35);
        check_hits(&app, 100, 35);
        capture(&mut app, &format!("providers-light-{light}"), 100, 35);
        key(&mut app, KeyCode::Esc);
        assert!(app.formatter_settings.active);
        assert!(app.formatter_settings.menu.is_none());
        click(&mut app, Action::AddArgument);
        replace_input(&mut app, "--line-length");
        click(&mut app, Action::AddArgument);
        replace_input(&mut app, "100");
        capture(&mut app, &format!("command-arguments-light-{light}"), 100, 35);
        click(&mut app, Action::Arguments);
        assert!(!app.formatter_settings.arguments_open);
        assert_eq!(
            app.formatter_settings.config().args,
            ["{file}", "--line-length", "100"]
        );
        click(&mut app, Action::ChooseProvider);
        click(&mut app, Action::Provider(Provider::Lsp));
        assert!(!render(&mut app, 100, 35).contains("Executable"));
        capture(&mut app, &format!("lsp-light-{light}"), 100, 35);
    }
}

#[test]
fn formatter_drafts_survive_provider_scope_and_language_switches_without_leaking() {
    let mut app = app();
    let user = Config {
        provider: Provider::Command,
        command: "user-formatter".into(),
        args: vec!["--fast".into(), "{file}".into()],
        mode: CommandMode::File,
    };
    let workspace = Config {
        provider: Provider::Command,
        command: "workspace-formatter".into(),
        args: vec!["--stdin".into()],
        mode: CommandMode::Stdin,
    };
    app.save_formatter(SettingsScope::Global, "python", Some(user.clone()))
        .unwrap();
    app.save_formatter(SettingsScope::Workspace, "python", Some(workspace.clone()))
        .unwrap();
    app.save_formatter(SettingsScope::Global, "cpp", Some(Config::default()))
        .unwrap();
    app.open_formatter_settings(None);
    assert_eq!(app.formatter_settings.scope, SettingsScope::Workspace);
    click(&mut app, Action::Field(1));
    replace_input(&mut app, "draft-formatter");
    let draft = app.formatter_settings.config();
    activate(&mut app, Action::Provider(Provider::Disabled));
    activate(&mut app, Action::Provider(Provider::Command));
    assert_eq!(app.formatter_settings.config(), draft);
    click(&mut app, Action::Scope(SettingsScope::Global));
    assert_eq!(app.formatter_settings.config(), user);
    click(&mut app, Action::Field(0));
    replace_input(&mut app, "cpp");
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.formatter_settings.config(), Config::default());
    click(&mut app, Action::Scope(SettingsScope::Workspace));
    assert_eq!(app.formatter_settings.config(), Config::default());
    click(&mut app, Action::Field(0));
    replace_input(&mut app, "python");
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.formatter_settings.config(), draft);
    click(&mut app, Action::Close);
    assert_eq!(
        app.settings_workspace.formatters.as_ref().unwrap()["python"],
        workspace
    );
    assert_eq!(
        app.settings_global.formatters.as_ref().unwrap()["python"],
        user
    );

    // An empty User scope must never retain a Workspace command on a fresh visit.
    app.save_formatter(SettingsScope::Global, "python", None)
        .unwrap();
    app.open_formatter_settings(None);
    click(&mut app, Action::Scope(SettingsScope::Global));
    assert_eq!(app.formatter_settings.config(), Config::default());
    click(&mut app, Action::Save);
    assert_eq!(
        app.settings_global.formatters.as_ref().unwrap()["python"],
        Config::default()
    );
    assert_eq!(
        app.settings_workspace.formatters.as_ref().unwrap()["python"],
        workspace
    );
}

#[test]
fn formatter_language_picker_search_navigation_and_dismissal() {
    let mut app = app();
    app.open_formatter_settings(None);
    click(&mut app, Action::ChooseLanguage);
    assert!(app.formatter_settings.known.len() > 20);
    key(&mut app, KeyCode::End);
    render(&mut app, 45, 16);
    let choices = menu_actions(&app.formatter_settings);
    assert!(
        app.formatter_settings
            .hits
            .iter()
            .any(|(_, action)| Some(action) == choices.last())
    );
    check_hits(&app, 45, 16);
    replace_input(&mut app, "rust");
    render(&mut app, 100, 35);
    assert!(menu_actions(&app.formatter_settings).iter().any(
        |action| matches!(action, Action::Language(i) if app.formatter_settings.known[*i] == "rust")
    ));
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.formatter_settings.language(), "rust");
    assert_eq!(app.formatter_settings.provider, Provider::Lsp);
    assert!(app.formatter_settings.menu.is_none());
    click(&mut app, Action::ChooseProvider);
    key(&mut app, KeyCode::End);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.formatter_settings.provider, Provider::Disabled);
    click(&mut app, Action::ChooseProvider);
    render(&mut app, 100, 35);
    let cancel = app
        .formatter_settings
        .hits
        .iter()
        .find(|(_, action)| *action == Action::Close)
        .unwrap()
        .0;
    crate::ui_modal::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: cancel.x,
            row: cancel.y,
            modifiers: KeyModifiers::NONE,
        },
    );
    assert!(
        app.formatter_settings.active,
        "an outside click dismisses the dropdown first"
    );
    assert!(app.formatter_settings.menu.is_none());
    key(&mut app, KeyCode::Esc);
    assert!(!app.formatter_settings.active);
}

#[test]
fn formatter_first_use_has_fixed_language_and_reachable_choices_on_small_screens() {
    let mut app = app();
    app.execute_palette_command("format_document");
    capture(&mut app, "first-use", 100, 35);
    activate(&mut app, Action::Provider(Provider::Command));
    for _ in 0..30 {
        render(&mut app, 32, 10);
        let state = &app.formatter_settings;
        if !matches!(state.focus, Action::Save | Action::Close) {
            assert!(state.hits.iter().any(|(_, action)| *action == state.focus));
        }
        assert!(
            !state
                .hits
                .iter()
                .any(|(_, action)| *action == Action::Field(0))
        );
        crate::ui_modal::handle_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    }
    activate(&mut app, Action::Provider(Provider::Disabled));
    click(&mut app, Action::Save);
    assert!(!app.formatter_settings.active);
    assert_eq!(app.formatters["python"].provider, Provider::Disabled);
}

#[test]
fn changing_language_does_not_focus_or_edit_a_disappearing_executable_field() {
    let mut app = app();
    app.open_formatter_settings(None);
    activate(&mut app, Action::Provider(Provider::Command));
    render(&mut app, 100, 35);
    let executable = app
        .formatter_settings
        .hits
        .iter()
        .find(|(_, action)| *action == Action::Field(1))
        .unwrap()
        .0;
    click(&mut app, Action::Field(0));
    replace_input(&mut app, "C++");
    crate::ui_modal::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: executable.x,
            row: executable.y,
            modifiers: KeyModifiers::NONE,
        },
    );
    assert_eq!(app.formatter_settings.language(), "cpp");
    assert_eq!(app.formatter_settings.config(), Config::default());
    assert_eq!(app.formatter_settings.focus, Action::ChooseProvider);
    assert!(app.focused_text_input_id().is_none());
    assert!(!render(&mut app, 100, 35).contains("Executable"));
    click(&mut app, Action::Save);
    assert_eq!(app.formatters["cpp"], Config::default());
    assert!(!app.formatters.contains_key("c++"));
}

#[test]
fn formatter_settings_reset_inherits_user_choice_and_known_language_can_be_edited() {
    let mut app = app();
    app.save_formatter(
        SettingsScope::Global,
        "python",
        Some(Config {
            provider: Provider::Command,
            command: "formatter".into(),
            args: vec!["{file}".into()],
            ..Default::default()
        }),
    )
    .unwrap();
    app.save_formatter(SettingsScope::Global, "cpp", Some(Config::default()))
        .unwrap();
    app.save_formatter(
        SettingsScope::Workspace,
        "python",
        Some(Config {
            provider: Provider::Disabled,
            ..Default::default()
        }),
    )
    .unwrap();
    app.open_formatter_settings(None);
    assert_eq!(app.formatter_settings.scope, SettingsScope::Workspace);
    activate(&mut app, Action::Reset);
    assert_eq!(app.formatters["python"].provider, Provider::Command);
    app.open_formatter_settings(None);
    let index = app
        .formatter_settings
        .known
        .iter()
        .position(|language| language == "cpp")
        .unwrap();
    click(&mut app, Action::ChooseLanguage);
    app.formatter_settings.menu_index = index;
    click(&mut app, Action::Language(index));
    assert_eq!(app.formatter_settings.language(), "cpp");
    assert_eq!(app.formatter_settings.provider, Provider::Lsp);
    assert!(!render(&mut app, 100, 35).contains("Black (Python)"));
}

#[test]
fn formatter_input_output_mode_is_explicit_and_keeps_arguments_and_drafts() {
    let mut app = app();
    app.open_formatter_settings(None);
    click(&mut app, Action::ChooseProvider);
    click(&mut app, Action::Provider(Provider::Command));
    assert_eq!(app.formatter_settings.config().args, ["{file}"]);
    assert_eq!(app.formatter_settings.mode, CommandMode::File);
    click(&mut app, Action::Field(1));
    replace_input(&mut app, "formatter");
    click(&mut app, Action::ChooseMode);
    let screen = render(&mut app, 100, 35);
    for mode in modes() { assert!(screen.contains(mode.label())); }
    check_hits(&app, 100, 35);
    capture(&mut app, "input-output-menu", 100, 35);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.formatter_settings.mode, CommandMode::Stdin);
    assert_eq!(app.formatter_settings.config().args, ["{file}"]);
    let argument = app.formatter_settings.arguments[0];
    click(&mut app, Action::RemoveArgument(argument));
    assert!(app.formatter_settings.config().args.is_empty());
    activate(&mut app, Action::Provider(Provider::Disabled));
    activate(&mut app, Action::Provider(Provider::Command));
    assert_eq!(app.formatter_settings.mode, CommandMode::Stdin);
    assert!(app.formatter_settings.config().args.is_empty(), "do not reinsert defaults into a draft");
    click(&mut app, Action::Save);
    assert_eq!(app.formatters["python"].mode, CommandMode::Stdin);
    app.open_formatter_settings(None);
    click(&mut app, Action::ChooseMode);
    click(&mut app, Action::Mode(CommandMode::File));
    click(&mut app, Action::Save);
    assert!(app.formatter_settings.active);
    assert!(app.formatter_settings.error.as_ref().unwrap().contains("Add {file}"));
    assert!(app.formatter_settings.arguments_open);
    assert_eq!(app.formatter_settings.focus, Action::Arguments);
    click(&mut app, Action::AddArgument);
    replace_input(&mut app, "{file}");
    click(&mut app, Action::Save);
    assert_eq!(app.formatters["python"].mode, CommandMode::File);
    assert_eq!(app.formatters["python"].args, ["{file}"]);
}
