use crate::{app::{App, Tab}, background::BackgroundWorkers, editor::Editor};
use std::{path::Path, sync::mpsc, time::Duration};

const WAIT: Duration = Duration::from_secs(10);

#[test]
#[ignore = "cold-process startup measurement"]
fn benchmark_first_frame_startup() {
    use std::time::Instant;
    if std::env::var_os("VTCODE_STARTUP_BENCH_CHILD").is_none() {
        for _ in 0..5 {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "startup_performance_tests::benchmark_first_frame_startup", "--ignored", "--nocapture"])
                .env("VTCODE_STARTUP_BENCH_CHILD", "1")
                .output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            for line in String::from_utf8_lossy(&output.stdout).lines().filter(|line| line.starts_with("startup:")) {
                println!("{line}");
            }
        }
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
    terminal.draw(crate::ui_welcome::draw_bootstrap_screen).unwrap();
    let first_frame = started.elapsed();
    let prepared = crate::startup::Prepared::load(Some(root.path().as_os_str().to_owned()));
    let manifest = started.elapsed();
    let mut app = prepared.into_app(crate::background::UiWake::default());
    let constructed = started.elapsed();
    terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
    println!("startup: first_frame={first_frame:?} manifest={manifest:?} constructed={constructed:?} workbench_frame={:?}", started.elapsed());
}

#[test]
fn stalled_startup_loader_keeps_resize_and_quit_live() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let wake = crate::background::UiWake::default();
    let (release, blocked) = mpsc::channel();
    let (started, running) = mpsc::channel();
    let loader = crate::startup::Loader::start(wake, move || {
        started.send(()).unwrap();
        blocked.recv_timeout(WAIT).unwrap();
    }).unwrap();
    let _release = Release(release);
    running.recv_timeout(WAIT).unwrap();
    let (sender, receiver) = mpsc::channel();
    let mut events = crate::app_event_loop::TerminalEventPump::from_receiver(receiver);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
    terminal.draw(crate::ui_welcome::draw_bootstrap_screen).unwrap();
    let frame = terminal.get_frame().count();
    terminal.backend_mut().resize(70, 20);
    sender.send(Ok(Event::Resize(70, 20))).unwrap();
    sender.send(Ok(Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)))).unwrap();
    assert!(loader.wait(&mut terminal, &mut events).unwrap().is_none());
    assert_eq!(terminal.get_frame().count(), frame + 1);
    assert_eq!(terminal.backend().buffer().area.width, 70);
    assert!(terminal.backend().buffer().content.iter().any(|cell| cell.symbol() == "█"));
}

#[test]
fn startup_preserves_keyboard_and_paste_order_without_replaying_logo_clicks() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind, MouseButton};
    let wake = crate::background::UiWake::default();
    let loader = crate::startup::Loader::start(wake, || 42).unwrap();
    let (sender, receiver) = mpsc::channel();
    let first = Event::Key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
    let paste = Event::Paste("café\n🦀".into());
    sender.send(Ok(first.clone())).unwrap();
    sender.send(Ok(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 5, row: 3, modifiers: KeyModifiers::NONE }))).unwrap();
    sender.send(Ok(paste.clone())).unwrap();
    let mut events = crate::app_event_loop::TerminalEventPump::from_receiver(receiver);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    assert_eq!(loader.wait(&mut terminal, &mut events).unwrap(), Some(42));
    let last = Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    sender.send(Ok(last.clone())).unwrap();
    assert_eq!(events.try_recv().unwrap(), Some(first));
    assert_eq!(events.try_recv().unwrap(), Some(paste));
    assert_eq!(events.try_recv().unwrap(), Some(last));
    assert_eq!(events.try_recv().unwrap(), None);
}

fn prepared(root: &Path, theme: &str) -> crate::startup::Prepared {
    crate::startup::Prepared {
        target: crate::startup::Target { root: root.into(), file: None, line: None, column: None },
        global: crate::settings::Settings { theme: Some(theme.into()), tab_size: Some(8), ..Default::default() },
        workspace: crate::settings::Settings { tab_size: Some(2), ..Default::default() },
        session: None,
    }
}

#[test]
fn startup_installed_theme_is_optional_and_does_not_overwrite_preferences_or_edits() {
    let root = tempfile::tempdir().unwrap();
    let key = "startup-custom";
    let mut app = prepared(root.path(), key).into_app(Default::default());
    app.settings_persistence_enabled = false;
    assert_eq!(app.default_tab_size, 2);
    assert_eq!(app.current_settings().theme.as_deref(), Some(key));
    assert_eq!(app.build_session().theme.as_deref(), Some(key));
    app.tabs.push(Tab::new_file(app.new_editor()));
    app.active_tab = 0;
    app.editor_mut().unwrap().paste_with_text("hello 🦀");
    let mut custom = crate::app_theme_loader::builtin_theme("light").unwrap().clone();
    custom.config_key = key.into();
    custom.syntax_theme = key.into();
    app.install_external_themes(vec![custom]);
    assert!(app.startup_theme_placeholder.is_none());
    assert_eq!(app.theme_key, key);
    assert_eq!(app.colors.bg_editor, crate::app_theme::EditorTheme::Light.colors().bg_editor);
    assert_eq!(app.editor().unwrap().rope.to_string(), "hello 🦀");
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope.to_string(), "");
    app.editor_mut().unwrap().redo();
    assert_eq!(app.editor().unwrap().rope.to_string(), "hello 🦀");
    assert_eq!(app.settings_global.theme.as_deref(), Some(key));
    assert_eq!(app.settings_effective.theme.as_deref(), Some(key));
}

#[test]
fn startup_theme_catalog_does_not_override_a_new_user_choice() {
    let root = tempfile::tempdir().unwrap();
    let key = "startup-custom";
    let mut app = prepared(root.path(), key).into_app(Default::default());
    app.settings_persistence_enabled = false;
    assert!(app.set_theme_by_key("dark"));
    app.save_settings();
    let mut custom = crate::app_theme_loader::builtin_theme("light").unwrap().clone();
    custom.config_key = key.into();
    custom.syntax_theme = key.into();
    app.install_external_themes(vec![custom]);
    assert_eq!(app.theme_key, "dark");
}

#[test]
fn unavailable_startup_theme_finishes_loading_without_rewriting_its_setting() {
    let root = tempfile::tempdir().unwrap();
    let key = "missing-theme";
    let mut app = prepared(root.path(), key).into_app(Default::default());
    let background = app.colors.bg_editor;
    assert!(app.install_external_themes(Vec::new()));
    assert!(app.startup_theme_placeholder.is_none());
    assert!(app.theme_name.contains("Unavailable"));
    assert_eq!(app.colors.bg_editor, background);
    app.save_settings();
    assert_eq!(app.settings_global.theme.as_deref(), Some(key));
    assert_eq!(app.build_session().theme.as_deref(), Some(key));
}

#[test]
fn startup_target_handles_directories_files_coordinates_and_missing_files() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file.rs");
    std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
    let dir = crate::startup::Target::resolve(Some(root.path().as_os_str().to_owned()));
    assert_eq!(dir.root, root.path());
    assert!(dir.file.is_none());
    for (suffix, line, column) in [("", None, None), (":2", Some(2), None), (":3:2", Some(3), Some(2))] {
        let target = crate::startup::Target::resolve(Some(format!("{}{suffix}", path.display()).into()));
        assert_eq!(target.root, root.path());
        assert_eq!(target.file.as_deref(), Some(path.as_path()));
        assert_eq!((target.line, target.column), (line, column));
    }
    let missing = root.path().join("new.rs");
    assert_eq!(crate::startup::Target::resolve(Some(missing.as_os_str().to_owned())).file, Some(missing));
    let mut launch = prepared(root.path(), "light");
    launch.target = crate::startup::Target::resolve(Some(format!("{}:3:2", path.display()).into()));
    let mut app = launch.into_app(Default::default());
    app.wait_for_background(WAIT, |app| app.editor().is_some_and(|editor| !editor.bg_loading));
    assert_eq!((app.editor().unwrap().cursor_line, app.editor().unwrap().cursor_col), (2, 1));
    assert_eq!(app.theme_key, "light");
    assert_eq!(app.editor().unwrap().tab_size, 2);
}

// Releases a blocked worker even if an assertion fails.
struct Release(mpsc::Sender<()>);
impl Drop for Release { fn drop(&mut self) { let _ = self.0.send(()); } }

fn block_assets(app: &mut App) -> Release {
    let (started, running) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    app.background.maintenance_sender().send(Box::new(move || {
        started.send(()).unwrap();
        let _ = resume.recv_timeout(WAIT);
    })).unwrap();
    running.recv_timeout(WAIT).unwrap();
    Release(release)
}

fn block_reads(workers: &mut BackgroundWorkers) -> (Release, mpsc::Receiver<()>) {
    let (started, running) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    workers.read_for_file("test-gate", Path::new("gate"), true, move || {
        started.send(()).unwrap();
        let _ = resume.recv_timeout(WAIT);
    });
    (Release(release), running)
}

fn write_grammar(folder: &Path, name: &str) {
    std::fs::create_dir_all(folder).unwrap();
    std::fs::write(folder.join("Example.tmLanguage.json"), serde_json::json!({
        "name":name,"scopeName":"source.startup","fileTypes":["startup"],
        "patterns":[{"match":"magic","name":"keyword.control.startup"}]
    }).to_string()).unwrap();
}

#[test]
fn opening_editing_and_cpu_jobs_do_not_wait_for_startup_assets() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("example.startup");
    std::fs::write(&path, "magic\n").unwrap();
    write_grammar(&root.path().join("syntaxes"), "Startup");
    let mut app = App::new_deferred(root.path().into());
    app.highlighter = crate::highlight::Highlighter::for_startup_syntax_folder(&root.path().join("syntaxes"));
    app.extension_manager.search_roots.clear();
    let blocked = block_assets(&mut app);
    app.request_syntax_assets_async();
    app.initialize_extensions();
    app.open_file(&path);
    app.wait_for_background(WAIT, |app| app.editor().is_some_and(|e| !e.bg_loading));
    assert!(app.highlighter.syntax_names().contains(&"Rust".into()));
    assert!(app.bg_syntax_asset_refresh_running && app.bg_extension_scan_running);
    let (cpu, done) = mpsc::channel();
    app.background.pool_sender().send(Box::new(move || cpu.send(()).unwrap())).unwrap();
    done.recv_timeout(WAIT).expect("asset work must not occupy the editor CPU workers");
    let editor = app.editor_mut().unwrap();
    editor.insert_char('x');
    assert_eq!(editor.rope.to_string(), "xmagic\n");
    editor.undo();
    assert_eq!(editor.rope.to_string(), "magic\n");
    editor.redo();
    assert_eq!(editor.rope.to_string(), "xmagic\n");
    let version = editor.content_version;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
    drop(blocked);
    app.wait_for_background(WAIT, |app| !app.bg_syntax_asset_refresh_running && !app.bg_extension_scan_running);
    assert!(app.highlighter.syntax_names().contains(&"Startup".into()));
    assert_eq!(app.editor().unwrap().rope.to_string(), "xmagic\n");
    assert_eq!(app.editor().unwrap().content_version, version);
    assert!(app.extension_manager.engine.get().is_none(), "no Wasm engine for an empty extension list");
}

#[test]
fn cached_syntaxes_are_published_before_validation_and_newer_sources_win() {
    let root = tempfile::tempdir().unwrap();
    write_grammar(root.path(), "Cached");
    crate::syntax_assets::rebuild_syntax_cache(root.path()).unwrap();
    let mut highlighter = crate::highlight::Highlighter::for_startup_syntax_folder(root.path());
    assert!(!highlighter.syntax_names().contains(&"Cached".into()));
    let mut published = false;
    let refreshed = highlighter.syntax_asset_refresh(Vec::new()).run_with_cached(|loaded| {
        published = true;
        highlighter.install_syntax_assets(loaded);
        assert!(highlighter.syntax_names().contains(&"Cached".into()));
        // Validation has not run yet. Even an edit at this point is observed.
        write_grammar(root.path(), "Updated");
    }).unwrap().unwrap();
    assert!(published);
    highlighter.install_syntax_assets(refreshed);
    assert!(highlighter.syntax_names().contains(&"Updated".into()));
    assert!(!highlighter.syntax_names().contains(&"Cached".into()));
}

#[test]
fn cached_startup_registry_highlights_without_waiting_for_maintenance_completion() {
    let root = tempfile::tempdir().unwrap();
    write_grammar(root.path(), "Cached");
    crate::syntax_assets::rebuild_syntax_cache(root.path()).unwrap();
    let mut app = App::new_deferred(root.path().into());
    app.highlighter = crate::highlight::Highlighter::for_startup_syntax_folder(root.path());
    let mut editor = Editor::test_with_text("magic\n");
    editor.file_path = Some(root.path().join("test.startup"));
    app.tabs.push(Tab::new_file(editor));
    app.bg_syntax_asset_refresh_running = true;
    let refresh = app.highlighter.syntax_asset_refresh(Vec::new());
    let (release, resume) = mpsc::channel();
    let release = Release(release);
    let sender = app.background.sender.clone();
    let path = root.path().to_path_buf();
    app.background.maintenance_sender().send(Box::new(move || {
        refresh.run_with_cached(|loaded| {
            sender.send(crate::background::BackgroundResult::StartupSyntaxCacheLoaded {
                root: path.clone(), loaded,
            }).unwrap();
            let _ = resume.recv_timeout(WAIT);
        }).unwrap();
    })).unwrap();
    app.wait_for_background(WAIT, |app| app.highlighter.syntax_names().contains(&"Cached".into()));
    app.wait_for_background(WAIT, |app| app.background.active_job_count() == 1
        && !app.background.has_pending_results());
    let path = app.editor().unwrap().file_path.clone().unwrap();
    assert!(app.highlighter.highlighted_line_by_index_if_cached(0, Some(&path), None, &app.syntax_theme, 0).is_some());
    assert!(app.bg_syntax_asset_refresh_running);
    drop(release);
    app.wait_for_background(WAIT, |app| !app.background.has_active_jobs());
}

fn saved(path: Option<String>, text: Option<&str>) -> crate::session::SavedTab {
    crate::session::SavedTab {
        file_path: path, cursor_line: 0, cursor_col: 0, scroll_offset: 0,
        horizontal_scroll: 0, pinned: false, preview: false, unsaved_file: None,
        pending_unsaved_content: text.map(ropey::Rope::from_str), undo_history_file: None,
        language_override: None, undo_entries: None, redo_entries: None,
        bookmarks: vec![], folded_ranges: vec![],
    }
}

#[test]
fn restore_prioritizes_active_untitled_and_visible_panes_over_hidden_files() {
    let root = tempfile::tempdir().unwrap();
    let visible = root.path().join("visible.txt");
    let hidden = root.path().join("hidden.txt");
    std::fs::write(&visible, "visible").unwrap();
    std::fs::write(&hidden, "hidden").unwrap();
    let mut app = App::new_deferred(root.path().into());
    app.background.use_single_read_worker();
    let (first_gate, started) = block_reads(&mut app.background);
    started.recv_timeout(WAIT).unwrap();
    app.restore_session(&crate::session::Session {
        tabs: vec![saved(Some(hidden.display().to_string()), None), saved(None, Some("draft"))],
        active_tab: 1,
        groups: vec![crate::session::SavedGroup { tabs: vec![], active_tab: 0 },
            crate::session::SavedGroup { tabs: vec![saved(Some(visible.display().to_string()), None)], active_tab: 0 }],
        ..Default::default()
    });
    let (after_visible, visible_done) = block_reads(&mut app.background);
    drop(first_gate);
    visible_done.recv_timeout(WAIT).unwrap();
    app.wait_for_background(WAIT, |app| !app.tabs[1].editor.bg_loading && !app.groups[1].tabs[0].editor.bg_loading);
    assert_eq!(app.tabs[1].editor.rope.to_string(), "draft");
    assert!(app.tabs[0].editor.bg_loading, "hidden tabs should still be queued");
    assert_eq!(app.groups[1].tabs[0].editor.rope.to_string(), "visible");
    drop(after_visible);
}

#[test]
fn focusing_a_restoring_tab_promotes_its_existing_read() {
    let root = tempfile::tempdir().unwrap();
    let mut app = App::new_deferred(root.path().into());
    app.background.use_single_read_worker();
    let (gate, started) = block_reads(&mut app.background);
    started.recv_timeout(WAIT).unwrap();
    let (done, completed) = mpsc::channel();
    for (name, id) in [("old", 0), ("focused", 1), ("other", 2)] {
        let done = done.clone();
        app.background.read_for_file("session-file-load", Path::new(name), false, move || { done.send(id).unwrap(); });
    }
    app.tabs.push(Tab::new_file(Editor::new_loading(Path::new("focused"), 0)));
    for _ in 0..20 { app.refresh_visible_highlights_async(); }
    drop(gate);
    let order: Vec<_> = (0..3).map(|_| completed.recv_timeout(WAIT).unwrap()).collect();
    assert_eq!(order, [1, 0, 2], "promote without duplicating disk reads");
}

#[test]
fn initial_wasm_activation_finishes_on_worker_and_rejects_obsolete_workspace() {
    let root = tempfile::tempdir().unwrap();
    let extension = root.path().join("extensions/test");
    std::fs::create_dir_all(&extension).unwrap();
    std::fs::write(extension.join("extension.json"), r#"{
        "id":"startup.test", "name":"Startup", "version":"0.1.0",
        "repository":"https://github.com/im-henri/vtcode",
        "apiVersion":"1", "main":"test.wat", "enabled":true
    }"#).unwrap();
    std::fs::write(extension.join("test.wat"), r#"(module
        (func (export "vtcode_activate") (result i32) i32.const 0)
    )"#).unwrap();
    let mut app = App::new_deferred(root.path().into());
    app.extension_manager.search_roots = vec![root.path().join("extensions")];
    assert!(app.extension_manager.engine.get().is_none());
    let gate = block_assets(&mut app);
    app.request_extension_scan();
    assert!(app.extension_manager.records.is_empty());
    drop(gate);
    app.wait_for_background(WAIT, |app| !app.bg_extension_scan_running);
    assert_eq!(app.extension_manager.records.len(), 1);
    assert_eq!(app.extension_manager.records[0].state, crate::extensions::ExtensionState::Loaded);
    assert!(app.extension_manager.engine.get().is_some());
    let obsolete = app.extension_manager.initial_scan_manager();
    let generation = app.extension_scan_generation;
    app.extension_scan_generation += 1;
    assert!(!app.apply_initialized_extensions(generation, Box::new(obsolete), vec![]));
    assert_eq!(app.extension_manager.records[0].manifest.id, "startup.test");
}

#[test]
fn lsp_only_extensions_do_not_initialize_wasm() {
    let root = tempfile::tempdir().unwrap();
    let mut manager = crate::extensions::ExtensionManager::new(root.path());
    let manifest = serde_json::from_value(serde_json::json!({
        "id":"startup.lsp", "name":"Language", "version":"1.0.0",
        "repository":"https://github.com/im-henri/vtcode", "main":"",
        "lsp": { "language":"test", "command":"test-server", "file_patterns":["*.test"] }
    })).unwrap();
    manager.sync_discovered(vec![crate::extension_manifest::DiscoveredExtension {
        manifest, manifest_path: root.path().join("extension.json"), wasm_path: root.path().into(),
        manifest_mtime: None, wasm_mtime: None,
    }], vec![]);
    assert_eq!(manager.records[0].state, crate::extensions::ExtensionState::Loaded);
    assert!(manager.engine.get().is_none());
}
