use super::*;
use crate::{
    app::Tab,
    editor::Editor,
    ui_formatter_settings::{self as form, Action},
};

fn fixture() -> (tempfile::TempDir, App, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.py");
    std::fs::write(&path, "value=1\n").unwrap();
    let mut app = App::new(dir.path().to_path_buf());
    app.settings_global_path = dir.path().join("user.json");
    app.settings_workspace_path = dir.path().join("workspace.json");
    app.settings_global = Settings::default();
    app.settings_workspace = Settings::default();
    app.settings_defaults.formatters = None;
    app.apply_effective_settings();
    let mut editor = Editor::new();
    editor.open_file(&path).unwrap();
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    (dir, app, path)
}

fn mock_external(
    app: &mut App,
    save_after: bool,
) -> (
    mpsc::Sender<Result<crate::formatters::Prepared, String>>,
    Arc<AtomicBool>,
) {
    let request = app.formatting_request(false, save_after).unwrap();
    let (sender, receiver) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    app.formatting.external = Some(External {
        request,
        receiver,
        cancelled: Arc::clone(&cancelled),
    });
    (sender, cancelled)
}

#[test]
fn formatter_first_use_asks_and_remembers_without_changing_lsp() {
    let (_dir, mut app, _path) = fixture();
    app.settings_global.lsp_enabled = Some(false);
    app.apply_effective_settings();
    app.execute_palette_command("format_document");
    assert!(app.formatter_settings.active);
    assert_eq!(app.formatter_settings.provider, Provider::Lsp);
    assert!(!app.is_formatting());
    form::activate(&mut app, Action::Provider(Provider::Disabled));
    form::activate(&mut app, Action::Save);
    assert!(!app.formatter_settings.active);
    assert_eq!(app.formatters["python"].provider, Provider::Disabled);
    assert!(!app.lsp_enabled);
    app.execute_palette_command("format_document");
    assert!(!app.formatter_settings.active);
    app.toggle_setting("formatters");
    assert!(app.formatter_settings.active);
    assert_eq!(app.formatter_settings.provider, Provider::Disabled);
    form::activate(&mut app, Action::Reset);
    app.execute_palette_command("format_document");
    assert!(app.formatter_settings.active);
}

#[test]
fn formatter_lsp_choice_resumes_first_request_without_restarting_server() {
    let (_dir, mut app, _) = fixture();
    let config = crate::lsp::LspServerConfig {
        language: "python".into(),
        command: "test-formatter-lsp".into(),
        args: vec![],
        file_patterns: vec!["*.py".into()],
        settings: None,
        init_options: None,
    };
    app.settings_global.lsp_servers = Some(vec![config]);
    app.apply_effective_settings();
    let (mut client, server) = crate::lsp::LspClient::test_pair("python", &["*.py"]);
    client.initialized = true;
    app.lsp_manager.clients.push(client);
    app.execute_palette_command("format_document");
    assert!(server.take_messages().is_empty());
    form::activate(&mut app, Action::Provider(Provider::Lsp));
    form::activate(&mut app, Action::Save);
    assert!(app.lsp_format_pending);
    assert!(!app.formatter_settings.active);
    assert_eq!(app.lsp_manager.clients.len(), 1);
    assert!(
        server
            .take_messages()
            .iter()
            .any(|message| message["method"] == "textDocument/formatting")
    );
}

#[test]
#[ignore = "requires Black and Pyright; set VTCODE_BLACK and optionally VTCODE_PYRIGHT"]
fn real_black_formatter_and_pyright_work_together() {
    let (_dir, mut app, path) = fixture();
    let config = crate::lsp::LspServerConfig {
        language: "python".into(),
        command: std::env::var("VTCODE_PYRIGHT").unwrap_or_else(|_| "pyright-langserver".into()),
        args: vec!["--stdio".into()],
        file_patterns: vec!["*.py".into()],
        settings: None,
        init_options: None,
    };
    app.settings_global.lsp_servers = Some(vec![config]);
    app.apply_effective_settings();
    app.lsp_manager.sync_document(
        &path,
        app.tabs[0].editor.content_version,
        app.tabs[0].editor.rope.clone(),
    );
    app.execute_palette_command("format_document");
    choose_black_external_command(&mut app);
    form::activate(&mut app, Action::Save);
    let start = std::time::Instant::now();
    while app.is_formatting()
        || !app
            .lsp_manager
            .clients
            .iter()
            .any(|client| client.initialized)
    {
        crate::app_lsp_poll::poll_lsp(&mut app, &mut false);
        assert!(
            start.elapsed() < Duration::from_secs(35),
            "Black/Pyright startup hung"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(app.formatting.error.is_none(), "{:?}", app.formatting.error);
    assert_eq!(app.editor().unwrap().rope.to_string(), "value = 1\n");
    assert!(app.lsp_enabled);
    assert_eq!(app.lsp_manager.clients.len(), 1);
    assert_eq!(app.lsp_manager.configs[0].language, "python");
    assert_eq!(app.formatters["python"].provider, Provider::Command);
}

#[test]
fn formatter_choice_persists_per_language_and_scope_and_survives_other_settings() {
    let (_dir, mut app, _path) = fixture();
    app.settings_persistence_enabled = true;
    std::fs::write(
        &app.settings_global_path,
        r#"{"theme":"dark","unknownPreference":42,"formatters":{"c":{"provider":"lsp"}}}"#,
    )
    .unwrap();
    let black = Config {
        provider: Provider::Command,
        command: "custom black.exe".into(),
        args: vec!["--line-length".into(), "88".into(), "{file}".into()],
        ..Default::default()
    };
    app.save_formatter(SettingsScope::Global, "python", Some(black.clone()))
        .unwrap();
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&app.settings_global_path).unwrap()).unwrap();
    assert_eq!(saved["unknownPreference"], 42);
    assert_eq!(saved["theme"], "dark");
    assert_eq!(app.formatters["c"].provider, Provider::Lsp);
    app.save_formatter(
        SettingsScope::Workspace,
        "python",
        Some(Config {
            provider: Provider::Disabled,
            ..Default::default()
        }),
    )
    .unwrap();
    assert_eq!(app.formatters["python"].provider, Provider::Disabled);
    app.toggle_setting("hover_enabled");
    assert_eq!(app.formatters["python"].provider, Provider::Disabled);
    let global = crate::settings::load_from_file(&app.settings_global_path);
    let workspace = crate::settings::load_from_file(&app.settings_workspace_path);
    assert_eq!(global.formatters.as_ref().unwrap()["python"], black);
    assert_eq!(
        global.merge(&workspace).formatters.unwrap()["python"].provider,
        Provider::Disabled
    );
    app.save_formatter(SettingsScope::Workspace, "python", None)
        .unwrap();
    assert_eq!(app.formatters["python"], black);
    app.save_formatter(SettingsScope::Global, "python", None)
        .unwrap();
    assert!(!app.formatters.contains_key("python"));
    assert!(app.formatters.contains_key("c"));
}

#[test]
fn formatter_invalid_settings_or_unsaved_settings_are_not_overwritten() {
    let (_dir, mut app, _path) = fixture();
    app.settings_persistence_enabled = true;
    std::fs::write(&app.settings_global_path, "broken JSON").unwrap();
    assert!(
        app.save_formatter(SettingsScope::Global, "python", Some(Config::default()))
            .is_err()
    );
    assert!(app.formatters.is_empty());
    assert_eq!(
        std::fs::read_to_string(&app.settings_global_path).unwrap(),
        "broken JSON"
    );
    std::fs::write(&app.settings_global_path, "{}").unwrap();
    let mut editor = Editor::test_with_text("{}");
    editor.file_path = Some(app.settings_global_path.clone());
    editor.modified = true;
    app.tabs.push(Tab::new_file(editor));
    assert!(
        app.save_formatter(SettingsScope::Global, "python", Some(Config::default()))
            .unwrap_err()
            .contains("unsaved")
    );
    assert_eq!(
        std::fs::read_to_string(&app.settings_global_path).unwrap(),
        "{}"
    );
}

#[test]
fn formatter_cancelled_first_use_save_still_saves_unsaved_text() {
    let (_dir, mut app, path) = fixture();
    app.editor_mut().unwrap().rope.insert(0, "# unsaved\n");
    app.editor_mut().unwrap().modified = true;
    app.format_on_save = true;
    app.save_current_file();
    assert!(app.formatter_settings.active);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "value=1\n");
    form::activate(&mut app, Action::Close);
    app.wait_for_background(Duration::from_secs(5), |app| {
        !app.pending_file_saves.contains_key(&path)
    });
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "# unsaved\nvalue=1\n"
    );
    assert!(app.formatters.is_empty());
}

#[test]
fn external_formatter_rejects_changed_closed_or_read_only_targets() {
    for change in 0..5 {
        let (_dir, mut app, _path) = fixture();
        let (sender, cancelled) = mock_external(&mut app, false);
        let edits = crate::formatters::prepare(&app.editor().unwrap().rope, "value = 1\n");
        match change {
            0 => {
                let editor = app.editor_mut().unwrap();
                editor.rope.insert(0, "# typed\n");
                editor.content_version += 1;
            }
            1 => app.tabs.clear(),
            2 => app.editor_mut().unwrap().read_only = true,
            3 => app.editor_mut().unwrap().file_path = Some(PathBuf::from("renamed.py")),
            _ => app.editor_mut().unwrap().bg_loading = true,
        }
        sender.send(Ok(edits)).unwrap();
        assert!(app.poll_external_format());
        assert!(cancelled.load(Ordering::Acquire));
        assert!(
            app.formatting
                .error
                .as_deref()
                .unwrap()
                .contains(["document changed", "closed", "read-only", "renamed", "reloaded"][change])
        );
        if let Some(editor) = app.editor() {
            assert!(!editor.rope.to_string().contains("value = 1"));
        }
    }
}

#[test]
fn external_formatter_targets_original_document_and_is_undoable() {
    let (_dir, mut app, path) = fixture();
    let (sender, _) = mock_external(&mut app, false);
    let edits = crate::formatters::prepare(&app.editor().unwrap().rope, "value = 1\n");
    let mut duplicate = Editor::test_with_text("other buffer\n");
    duplicate.file_path = Some(path);
    app.tabs.insert(0, Tab::new_file(duplicate));
    sender.send(Ok(edits)).unwrap();
    assert!(app.poll_external_format());
    assert_eq!(app.tabs[0].editor.rope.to_string(), "other buffer\n");
    assert_eq!(app.tabs[1].editor.rope.to_string(), "value = 1\n");
    app.tabs[1].editor.undo();
    assert_eq!(app.tabs[1].editor.rope.to_string(), "value=1\n");
}

#[test]
fn external_formatter_save_joins_existing_request_and_errors_still_save() {
    let (_dir, mut app, path) = fixture();
    let (sender, _) = mock_external(&mut app, false);
    app.format_on_save = true;
    app.save_current_file();
    assert!(app.formatting.external.as_ref().unwrap().request.save_after);
    sender
        .send(Err("tool configuration failed".into()))
        .unwrap();
    assert!(app.poll_external_format());
    app.wait_for_background(Duration::from_secs(5), |app| {
        !app.pending_file_saves.contains_key(&path)
    });
    assert!(
        app.formatting
            .error
            .as_deref()
            .unwrap()
            .contains("tool configuration failed")
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), "value=1\n");
}

#[test]
fn formatter_selection_excludes_next_line_when_selection_ends_at_its_start() {
    let (_dir, mut app, _path) = fixture();
    let editor = app.editor_mut().unwrap();
    editor.selection_anchor = Some((0, 0));
    editor.cursor_line = 1;
    editor.cursor_col = 0;
    assert_eq!(
        app.formatting_request(true, false).unwrap().lines,
        Some((1, 1))
    );
}

#[test]
#[ignore = "requires Black; set VTCODE_BLACK to its executable"]
fn real_black_formatter_runs_with_lsp_disabled_and_reuses_saved_choice() {
    let (_dir, mut app, path) = fixture();
    app.settings_global.lsp_enabled = Some(false);
    app.apply_effective_settings();
    app.execute_palette_command("format_document");
    assert!(app.formatter_settings.active);
    choose_black_external_command(&mut app);
    form::activate(&mut app, Action::Save);
    let start = std::time::Instant::now();
    while app.is_formatting() {
        crate::app_lsp_poll::poll_lsp(&mut app, &mut false);
        assert!(start.elapsed() < Duration::from_secs(35));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(app.formatting.error.is_none(), "{:?}", app.formatting.error);
    assert_eq!(app.editor().unwrap().rope.to_string(), "value = 1\n");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "value=1\n");
    assert!(!app.lsp_enabled);
    app.editor_mut().unwrap().undo();
    app.format_on_save = true;
    app.save_current_file();
    assert!(!app.formatter_settings.active);
    while app.is_formatting() {
        crate::app_lsp_poll::poll_lsp(&mut app, &mut false);
        assert!(start.elapsed() < Duration::from_secs(35));
        std::thread::sleep(Duration::from_millis(10));
    }
    app.wait_for_background(Duration::from_secs(5), |app| {
        !app.pending_file_saves.contains_key(&path)
    });
    assert_eq!(std::fs::read_to_string(path).unwrap(), "value = 1\n");
}

fn choose_black_external_command(app: &mut App) {
    form::activate(app, Action::Provider(Provider::Command));
    app.formatter_settings.fields[1].text =
        std::env::var("VTCODE_BLACK").unwrap_or_else(|_| "black".into());
    assert_eq!(app.formatter_settings.fields[2].text, "{file}");
}

#[test]
fn external_formatter_in_place_changes_are_undoable_and_save_only_when_requested() {
    let (dir, mut app, path) = fixture();
    let script = dir.path().join(if cfg!(windows) { "format.ps1" } else { "format.sh" });
    std::fs::write(&script, if cfg!(windows) {
        r#"$text = [IO.File]::ReadAllText($args[0]).Replace('value=', 'value = ')
[IO.File]::WriteAllText($args[0], $text, [Text.UTF8Encoding]::new($false))"#
    } else {
        r#"sed 's/value=/value = /g' "$1" > "$1.new"; mv "$1.new" "$1""#
    }).unwrap();
    let mut args: Vec<String> = if cfg!(windows) {
        ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"].map(String::from).into()
    } else { Vec::new() };
    args.extend([script.to_string_lossy().into_owned(), "{file}".into()]);
    app.save_formatter(SettingsScope::Global, "python", Some(Config {
        provider: Provider::Command,
        command: if cfg!(windows) { "powershell.exe" } else { "sh" }.into(),
        args,
        ..Default::default()
    })).unwrap();
    let editor = app.editor_mut().unwrap();
    editor.cursor_col = 7;
    editor.insert_char('0');
    assert_eq!(editor.rope.to_string(), "value=10\n");
    assert!(app.request_format(false, false));
    let started = std::time::Instant::now();
    while app.is_formatting() {
        app.poll_background_results();
        app.poll_external_format();
        assert!(started.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(app.formatting.error.is_none(), "{:?}", app.formatting.error);
    assert_eq!(app.editor().unwrap().rope.to_string(), "value = 10\n");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "value=1\n");
    assert!(app.editor().unwrap().modified);
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope.to_string(), "value=10\n");
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope.to_string(), "value=1\n");
    app.editor_mut().unwrap().redo();
    app.format_on_save = true;
    app.save_current_file();
    while app.is_formatting() {
        app.poll_background_results();
        app.poll_external_format();
        assert!(started.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(10));
    }
    app.wait_for_background(Duration::from_secs(5), |app| !app.pending_file_saves.contains_key(&path));
    assert!(app.formatting.error.is_none(), "{:?}", app.formatting.error);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "value = 10\n");
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope.to_string(), "value=10\n");
    assert!(app.editor().unwrap().modified);
}
