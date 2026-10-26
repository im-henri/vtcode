//! Real Wasm, worker queues, documents, and workbench rendering.
use crate::app::App;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use std::{path::Path, time::Duration};

const WASM_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/tabular-workbench/tabular-workbench.wasm"
);
fn app(root: &Path) -> App {
    let package = root.join("extensions/tabular-workbench");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("extension.json"),
        include_bytes!("../tests/fixtures/tabular-workbench/extension.json"),
    )
    .unwrap();
    std::fs::write(
        package.join("tabular-workbench.wasm"),
        std::fs::read(WASM_PATH).unwrap(),
    )
    .unwrap();
    let mut app = App::new(root.to_path_buf());
    app.extension_manager.search_roots = vec![root.join("extensions")];
    app.extension_manager.settings_file = root.join("state/extensions.json");
    app.extension_manager.scan_and_sync();
    assert_eq!(
        app.extension_manager.error_count(),
        0,
        "{:?}",
        app.extension_views()
    );
    app
}
fn drain_input(app: &mut App) {
    for _ in 0..1000 {
        app.extension_manager
            .dispatch_view_events(&mut app.background);
        app.wait_for_background(Duration::from_secs(15), |app| {
            !app.extension_manager.has_busy_runtimes()
        });
        if app.extension_manager.pending_view_events.is_empty() {
            return;
        }
    }
    panic!("input did not drain")
}
fn finish(app: &mut App) {
    drain_input(app);
    for _ in 0..20_000 {
        for (message, _, _) in &app.notification_history {
            assert!(
                !message.contains("wasm trap") && !message.contains("Failed calling"),
                "{message}\n{}",
                text(app)
            );
        }
        assert!(
            app.extension_manager.records[0].last_error.is_none(),
            "{:?}",
            app.extension_views()
        );
        if app.extension_manager.records[0].tick_handlers.is_empty() {
            app.apply_extension_text_edits();
            return;
        }
        app.extension_manager
            .invoke_tick_handlers_async(
                "vtcode.tabular-workbench",
                vec!["tabular_tick".into()],
                &mut app.background,
                None,
            )
            .unwrap();
        drain_input(app);
    }
    panic!(
        "tabular tasks did not finish: {}\nstatus={:?}\nlogs={:?}\npending={:?}\ninput_pending={}",
        text(app),
        app.extension_manager.records[0].status_text,
        app.extension_manager.records[0].logs,
        app.extension_manager.pending_view_events.len(),
        app.extension_manager.view_input_pending(
            "vtcode.tabular-workbench",
            &app.active_extension_view().unwrap().view_id
        )
    )
}
fn open(app: &mut App, path: &Path) {
    assert!(app.run_extension_command_with_optional_context(
        "vtcode.tabular-workbench",
        "table.open",
        Some(path)
    ));
    finish(app);
    assert!(
        app.active_extension_view().is_some(),
        "{:?}",
        app.extension_views()
    );
}
fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::input_keyboard::handle_keyboard_event(
        app,
        crossterm::event::KeyEvent::new(code, modifiers),
    );
    finish(app);
}
fn paste(app: &mut App, text: &str) {
    assert!(app.paste_into_extension_editor(text));
    finish(app);
}
fn text(app: &App) -> String {
    let view = app.active_extension_view().unwrap();
    app.extension_manager
        .get_view(&view.extension_id, &view.view_id)
        .unwrap()
        .frame
        .unwrap()
        .lines
        .iter()
        .map(|line| {
            line.iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn resize(app: &mut App, width: u16, height: u16) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            assert!(crate::extension_views::draw_active_view(
                frame,
                app,
                Rect::new(0, 0, width, height)
            ));
        })
        .unwrap();
    finish(app);
}
#[test]
fn tabular_wasm_imports_filters_formulas_and_profiles_actual_data() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("data.csv");
    std::fs::write(&path, "name,amount,qty\nalpha,10,2\nbeta,2,3\ngamma,,1\n").unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    resize(&mut app, 120, 30);
    assert!(text(&app).contains("alpha"), "{}", text(&app));
    key(&mut app, KeyCode::Char('='), KeyModifiers::NONE);
    paste(&mut app, "total");
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    paste(&mut app, "[amount] * [qty]");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(text(&app).contains("total"), "{}", text(&app));
    key(&mut app, KeyCode::Char('f'), KeyModifiers::NONE);
    paste(&mut app, "total > 10");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(text(&app).contains("1 of 3 rows"), "{}", text(&app));
    assert!(!text(&app).contains("beta"));
    key(&mut app, KeyCode::Char('2'), KeyModifiers::NONE);
    assert!(text(&app).contains("HISTOGRAM"), "{}", text(&app));
    assert!(!app.tabs[app.active_tab].has_unsaved_changes());
}

#[test]
fn tabular_wasm_persists_exploration_and_appends_deletes_and_redoes_source_rows() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("data.csv");
    let original = "name,n\nalpha,1\nbeta,2\n";
    std::fs::write(&path, original).unwrap();
    let mut first = app(root.path());
    open(&mut first, &path);
    key(&mut first, KeyCode::Char('='), KeyModifiers::NONE);
    paste(&mut first, "total");
    key(&mut first, KeyCode::Tab, KeyModifiers::NONE);
    paste(&mut first, "n * 2");
    key(&mut first, KeyCode::Enter, KeyModifiers::NONE);
    key(&mut first, KeyCode::Char('f'), KeyModifiers::NONE);
    paste(&mut first, "total > 2");
    key(&mut first, KeyCode::Enter, KeyModifiers::NONE);
    key(&mut first, KeyCode::Home, KeyModifiers::NONE);
    key(&mut first, KeyCode::Char('c'), KeyModifiers::NONE);
    key(&mut first, KeyCode::Char('+'), KeyModifiers::NONE);
    key(&mut first, KeyCode::Char('f'), KeyModifiers::NONE);
    key(&mut first, KeyCode::Down, KeyModifiers::NONE);
    key(&mut first, KeyCode::Char(' '), KeyModifiers::NONE);
    key(&mut first, KeyCode::Esc, KeyModifiers::NONE);
    first.close_tab(first.active_tab);
    finish(&mut first);
    assert!(first.tabs.is_empty());
    drop(first);

    let mut restored = app(root.path());
    open(&mut restored, &path);
    resize(&mut restored, 120, 30);
    let frame = text(&restored);
    assert!(frame.contains("1 of 2 rows"), "{frame}");
    assert!(frame.contains("total") && frame.contains("beta"), "{frame}");
    assert!(!frame.contains("alpha"), "{frame}");
    key(&mut restored, KeyCode::Char('c'), KeyModifiers::NONE);
    let columns = text(&restored);
    assert!(
        columns
            .lines()
            .any(|line| line.contains("name") && line.contains("frozen") && line.contains("22 ch")),
        "{columns}"
    );
    assert!(columns.contains("[ ]  n"), "{columns}");
    key(&mut restored, KeyCode::Esc, KeyModifiers::NONE);

    // Insert reveals the appended row even though the persisted filter excludes blanks.
    key(&mut restored, KeyCode::Insert, KeyModifiers::NONE);
    assert!(
        text(&restored).contains("filters cleared"),
        "{}",
        text(&restored)
    );
    key(&mut restored, KeyCode::Enter, KeyModifiers::NONE);
    paste(&mut restored, "added");
    key(&mut restored, KeyCode::Enter, KeyModifiers::NONE);
    assert!(text(&restored).contains("added"), "{}", text(&restored));
    key(&mut restored, KeyCode::Delete, KeyModifiers::NONE);
    key(&mut restored, KeyCode::Enter, KeyModifiers::NONE);
    assert!(!text(&restored).contains("added"), "{}", text(&restored));
    key(&mut restored, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert!(text(&restored).contains("added"), "{}", text(&restored));
    key(&mut restored, KeyCode::Char('y'), KeyModifiers::CONTROL);
    assert!(!text(&restored).contains("added"), "{}", text(&restored));
    key(&mut restored, KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn tabular_wasm_edits_save_undo_save_as_and_close_confirmation() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("data.csv");
    std::fs::write(&path, "name,n\nalpha,10\nbeta,2\n").unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    paste(&mut app, "42");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.tabs[app.active_tab].has_unsaved_changes());
    key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert!(
        std::fs::read_to_string(&path).unwrap().contains("alpha,42"),
        "disk={:?}\n{}\nhost={}",
        std::fs::read_to_string(&path),
        text(&app),
        app.status_message
    );
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert!(text(&app).contains("10"), "{}", text(&app));
    let saved = root.path().join("saved.csv");
    app.execute_palette_command("save_as");
    app.save_as_query = saved.display().to_string();
    app.execute_save_as();
    finish(&mut app);
    assert_eq!(
        std::fs::read_to_string(saved).unwrap(),
        "name,n\nalpha,10\nbeta,2\n"
    );
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    paste(&mut app, "17");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    app.close_tab(app.active_tab);
    assert!(app.confirm_close_active);
}
#[test]
fn tabular_wasm_export_opens_named_unsaved_buffer_without_touching_source() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("data.csv");
    let original = "name,n\nalpha,10\nbeta,2\n";
    std::fs::write(&path, original).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.active_extension_view().is_none());
    assert_eq!(app.editor().unwrap().file_name(), "data-view.csv");
    assert_eq!(
        app.editor().unwrap().rope.to_string(),
        "name,n\nbeta,2\nalpha,10\n"
    );
    assert!(app.editor().unwrap().file_path.is_none());
    assert!(app.editor().unwrap().modified);
    app.execute_palette_command("save_as");
    assert!(app.save_as_query.ends_with("data-view.csv"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}
#[test]
fn tabular_wasm_groups_and_compares_two_open_datasets() {
    let root = tempfile::tempdir().unwrap();
    let before = root.path().join("before.csv");
    let after = root.path().join("after.csv");
    std::fs::write(&before, "id,category,n\n1,a,10\n2,b,20\n3,a,30\n").unwrap();
    std::fs::write(&after, "id,category,n\n1,a,12\n2,b,20\n4,a,40\n").unwrap();
    let mut app = app(root.path());
    open(&mut app, &before);
    open(&mut app, &after);
    key(&mut app, KeyCode::Char('g'), KeyModifiers::NONE);
    paste(&mut app, "category");
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    paste(&mut app, "n");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(text(&app).contains("Mean"), "{}", text(&app));
    key(&mut app, KeyCode::Char('1'), KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        text(&app).contains("1 added   1 removed   1 changed   1 equal"),
        "{}",
        text(&app)
    );
    assert!(text(&app).contains("Changed"));
    assert!(text(&app).contains("Removed"));
}
#[test]
fn tabular_wasm_keyboard_mouse_unicode_and_narrow_panes_do_not_trap() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("unicode.tsv");
    std::fs::write(&path, "name\tvalue\n日本語\t1\ne\u{301} 👩‍💻\t2\n").unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    for (width, height) in [(80, 24), (40, 16), (24, 10), (12, 6), (160, 40)] {
        resize(&mut app, width, height);
        key(&mut app, KeyCode::Char('?'), KeyModifiers::NONE);
        key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    }
    key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    key(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.extension_manager.records[0].last_error.is_none());
}
#[test]
fn tabular_wasm_save_barrier_flushes_an_edit_before_the_next_task_tick() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("data.csv");
    std::fs::write(&path, "name,n\nalpha,10\n").unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    paste(&mut app, "99");
    assert!(app.handle_active_extension_view_key(KeyCode::Enter, KeyModifiers::NONE));
    drain_input(&mut app); // Enter is accepted, but guest background work has not run.
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        crossterm::event::KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
    );
    finish(&mut app);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "name,n\nalpha,99\n",
        "{}\nhost={}",
        text(&app),
        app.status_message
    );
}

#[test]
fn tabular_wasm_close_waits_for_accepted_input_then_confirms_dirty_document() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("data.csv");
    std::fs::write(&path, "name,n\nalpha,10\n").unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    paste(&mut app, "99");
    assert!(app.handle_active_extension_view_key(KeyCode::Enter, KeyModifiers::NONE));
    drain_input(&mut app);
    app.close_tab(app.active_tab);
    assert!(app.active_extension_view().is_some());
    finish(&mut app);
    app.close_tab(app.active_tab);
    assert!(app.confirm_close_active);
    crate::input_pickers::handle_confirm_close_key(&mut app, KeyCode::Char('y'));
    finish(&mut app);
    assert!(app.tabs.is_empty());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "name,n\nalpha,99\n"
    );
}

#[test]
fn tabular_wasm_json_paste_keeps_types_and_undo_is_one_operation() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("data.jsonl");
    let original = "{\"name\":\"alpha\",\"n\":10,\"v\":null}\n{\"name\":\"beta\",\"n\":2}\n";
    std::fs::write(&path, original).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    paste(&mut app, "true\t42\nhello\t7");
    key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
    let records: Vec<serde_json::Value> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(records[0]["name"], "true");
    assert_eq!(records[0]["n"], 42);
    assert!(records[1].get("v").is_none());
    assert!(records[0]["v"].is_null());
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn tabular_wasm_external_conflict_and_recovery_preserve_the_right_bytes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("data.csv");
    let original = "name,n\nalpha,10\n";
    std::fs::write(&path, original).unwrap();
    let mut first = app(root.path());
    open(&mut first, &path);
    key(&mut first, KeyCode::Right, KeyModifiers::NONE);
    key(&mut first, KeyCode::Enter, KeyModifiers::NONE);
    paste(&mut first, "77");
    key(&mut first, KeyCode::Enter, KeyModifiers::NONE);
    assert!(first.start_custom_exit(true));
    finish(&mut first);
    let recovery_dir = crate::extension_recovery::directory(&first.extension_manager.settings_file);
    assert_eq!(crate::extension_recovery::read(&recovery_dir).len(), 1);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    drop(first);
    let mut restored = app(root.path());
    open(&mut restored, &path);
    assert!(text(&restored).contains("77"));
    assert!(restored.tabs[restored.active_tab].has_unsaved_changes());
    std::fs::write(&path, "name,n\nexternal,1234\n").unwrap();
    key(&mut restored, KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert!(
        text(&restored).contains("changed on disk"),
        "{}",
        text(&restored)
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "name,n\nexternal,1234\n"
    );
    restored.close_tab(restored.active_tab);
    assert!(restored.confirm_close_active);
    crate::input_pickers::handle_confirm_close_key(&mut restored, KeyCode::Char('n'));
    finish(&mut restored);
    assert!(restored.tabs.is_empty());
    assert!(crate::extension_recovery::read(&recovery_dir).is_empty());
}

#[test]
fn tabular_wasm_hundred_thousand_rows_sort_profile_and_filter_without_traps() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("large.csv");
    let mut source = String::from("id,n,category\n");
    for row in 0..100_000 {
        source.push_str(&format!("{row},{},group{}\n", 100_000 - row, row % 7));
    }
    std::fs::write(&path, source).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    assert!(text(&app).contains("100,000"), "{}", text(&app));
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(text(&app).contains("100000"), "{}", text(&app));
    key(&mut app, KeyCode::Char('2'), KeyModifiers::NONE);
    assert!(
        text(&app)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .contains("Mean 50000.5"),
        "{}",
        text(&app)
    );
    key(&mut app, KeyCode::Char('f'), KeyModifiers::NONE);
    paste(&mut app, "n > 99990");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(text(&app).contains("10 of 100000 rows"), "{}", text(&app));
    assert!(app.extension_manager.records[0].last_error.is_none());
}

#[test]
fn tabular_wasm_pointer_sort_resize_selection_and_context_menu_work() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("data.csv");
    std::fs::write(&path, "name,n\nalpha,10\nbeta,2\n").unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    resize(&mut app, 100, 28);
    app.code_area = Some(Rect::new(0, 0, 100, 28));
    assert!(app.handle_active_extension_view_mouse_down(32, 4, KeyModifiers::NONE));
    finish(&mut app);
    assert!(text(&app).contains("Sort: n ↑"), "{}", text(&app));
    assert!(app.handle_active_extension_view_mouse_down(28, 4, KeyModifiers::NONE));
    finish(&mut app);
    assert!(app.handle_active_extension_view_mouse_drag(35, 4, KeyModifiers::NONE));
    finish(&mut app);
    assert!(app.handle_active_extension_view_mouse_up(35, 4, KeyModifiers::NONE));
    finish(&mut app);
    assert!(app.handle_active_extension_view_mouse_down(10, 6, KeyModifiers::NONE));
    finish(&mut app);
    assert!(app.handle_active_extension_view_mouse_drag(40, 7, KeyModifiers::NONE));
    finish(&mut app);
    assert!(app.handle_active_extension_view_mouse_up(40, 7, KeyModifiers::NONE));
    finish(&mut app);
    key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(
        app.clipboard_history.front().map(String::as_str),
        Some("beta\t2\nalpha\t10")
    );
    key(&mut app, KeyCode::F(10), KeyModifiers::SHIFT);
    assert!(text(&app).contains("Actions"));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
}

#[test]
fn tabular_wasm_command_paste_uses_its_captured_clipboard_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("data.csv");
    std::fs::write(&path, "n\n1\n").unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let view = app.active_extension_view().unwrap().clone();
    let context = app.extension_editor_context_snapshot();
    assert!(app.extension_manager.queue_view_event(
        &view.extension_id,
        &view.view_id,
        crate::extension_wasm::ViewEventHandlerKind::Key,
        r#"{"code":"command","text":"paste"}"#.into(),
        context,
        "123".into()
    ));
    finish(&mut app);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "n\n123\n");
}

#[test]
fn tabular_wasm_large_paste_yields_and_undo_restores_all_rows() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("paste.csv");
    let original = format!("n,text\n{}", "1,old\n".repeat(20_000));
    std::fs::write(&path, &original).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    paste(&mut app, &"2\tnew value with spaces\n".repeat(20_000));
    assert!(text(&app).contains("new value"), "{}", text(&app));
    assert!(app.tabs[app.active_tab].has_unsaved_changes());
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn tabular_wasm_escape_rolls_back_an_in_progress_sparse_paste() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("cancel.csv");
    let original = format!(
        "id,value\n{}",
        (0..10_000)
            .map(|i| format!("{i},old\n"))
            .collect::<String>()
    );
    std::fs::write(&path, &original).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    key(&mut app, KeyCode::Char('f'), KeyModifiers::NONE);
    paste(&mut app, "id % 2 == 0");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    assert!(app.paste_into_extension_editor(&"new\n".repeat(5_000)));
    drain_input(&mut app);
    for _ in 0..400 {
        app.extension_manager
            .invoke_tick_handlers_async(
                "vtcode.tabular-workbench",
                vec!["tabular_tick".into()],
                &mut app.background,
                None,
            )
            .unwrap();
        drain_input(&mut app);
        if text(&app).contains("Applying edits") {
            break;
        }
    }
    assert!(text(&app).contains("Applying edits"), "{}", text(&app));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.tabs[app.active_tab].has_unsaved_changes());
    key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
}

#[test]
fn tabular_wasm_save_as_finishes_a_queued_bulk_paste() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source.csv");
    let destination = root.path().join("saved.csv");
    std::fs::write(&path, format!("n,text\n{}", "1,old\n".repeat(20_000))).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    assert!(app.paste_into_extension_editor(&"2\tnew\n".repeat(20_000)));
    drain_input(&mut app);
    let view = app.active_extension_view().unwrap().clone();
    let context = app.extension_editor_context_snapshot();
    assert!(app.extension_manager.queue_document_action(
        &view.extension_id,
        &view.view_id,
        crate::extension_handlers::DocumentWorkbenchAction::SaveAs {
            path: destination.clone(),
            replace: false
        },
        context,
    ));
    finish(&mut app);
    assert_eq!(
        std::fs::read_to_string(destination).unwrap(),
        format!("n,text\n{}", "2,new\n".repeat(20_000))
    );
    assert!(!app.tabs[app.active_tab].has_unsaved_changes());
}

#[test]
fn tabular_wasm_megabyte_paste_and_twenty_thousand_groups_fit_task_budgets() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("wide.csv");
    std::fs::write(&path, format!("text\n{}", "old\n".repeat(20))).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    paste(&mut app, &format!("{}\n", "z".repeat(100_000)).repeat(20));
    key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert_eq!(std::fs::metadata(path).unwrap().len(), 5 + 20 * 100_001);
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    paste(&mut app, &format!("{}\n", "\0".repeat(125_000)).repeat(8));
    key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
    let before = std::fs::read(root.path().join("wide.csv")).unwrap();
    assert!(before.contains(&0));
    paste(&mut app, &format!("{}\n", "\0".repeat(125_000)).repeat(12));
    assert!(text(&app).contains("event budget"), "{}", text(&app));
    assert!(!app.tabs[app.active_tab].has_unsaved_changes());
    assert_eq!(std::fs::read(root.path().join("wide.csv")).unwrap(), before);
    let grouped = root.path().join("groups.csv");
    let source = format!(
        "id,n\n{}",
        (0..20_000)
            .map(|i| format!("{i},{}\n", i % 7))
            .collect::<String>()
    );
    std::fs::write(&grouped, source).unwrap();
    open(&mut app, &grouped);
    key(&mut app, KeyCode::Char('g'), KeyModifiers::NONE);
    paste(&mut app, "id");
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    paste(&mut app, "n");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(text(&app).contains("20,000"), "{}", text(&app));
    assert!(
        text(&app).contains("Grouped filtered rows"),
        "{}",
        text(&app)
    );
}
