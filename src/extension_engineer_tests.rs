//! The real Engineer Buddy Wasm through VTCode's worker queues and input routing.
use crate::app::App;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use std::{path::Path, time::Duration};
const ID: &str = "vtcode.engineer-buddy";

fn app(root: &Path) -> App {
    let package = root.join("extensions/engineer-buddy");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("extension.json"),
        include_bytes!("../tests/fixtures/engineer-buddy/extension.json"),
    )
    .unwrap();
    std::fs::write(
        package.join("engineer-buddy.wasm"),
        include_bytes!("../tests/fixtures/engineer-buddy/engineer-buddy.wasm"),
    )
    .unwrap();
    let mut app = App::new(root.to_path_buf());
    app.source_control_enabled = false;
    app.extension_manager.search_roots = vec![root.join("extensions")];
    app.extension_manager.settings_file = root.join("state/extensions.json");
    let notices = app.extension_manager.scan_and_sync();
    assert_eq!(app.extension_manager.error_count(), 0, "{notices:?}");
    app
}
fn finish(app: &mut App) {
    for _ in 0..200 {
        app.extension_manager
            .dispatch_view_events(&mut app.background);
        app.wait_for_background(Duration::from_secs(15), |app| {
            !app.extension_manager.has_busy_runtimes()
        });
        assert!(
            app.extension_manager.records[0].last_error.is_none(),
            "{:?}",
            app.extension_views()
        );
        if !app.extension_manager.pending_view_events.is_empty() {
            continue;
        }
        if app.extension_manager.records[0].tick_handlers.is_empty() {
            app.apply_extension_text_edits();
            return;
        }
        app.extension_manager
            .invoke_tick_handlers_async(ID, vec!["buddy_task".into()], &mut app.background, None)
            .unwrap();
    }
    panic!("Engineer Buddy did not settle")
}
fn open(app: &mut App) {
    assert!(app.run_extension_command_with_optional_context(ID, "buddy.open", None));
    finish(app);
    let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
    terminal
        .draw(|frame| {
            assert!(crate::extension_views::draw_active_view(
                frame,
                app,
                Rect::new(0, 0, 140, 40)
            ));
        })
        .unwrap();
    finish(app);
}
fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::input_keyboard::handle_keyboard_event(
        app,
        crossterm::event::KeyEvent::new(code, modifiers),
    );
    finish(app);
}
fn paste(app: &mut App, s: &str) {
    assert!(app.paste_into_extension_editor(s));
    finish(app);
}
fn evaluate(app: &mut App, s: &str) {
    key(app, KeyCode::Char('l'), KeyModifiers::CONTROL);
    paste(app, s);
    key(app, KeyCode::Enter, KeyModifiers::NONE);
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

#[test]
fn engineer_buddy_wasm_saves_wide_values_and_recovers_them_after_reload() {
    let root = tempfile::tempdir().unwrap();
    let mut app = app(root.path());
    open(&mut app);
    evaluate(&mut app, "wide = mask(256)");
    evaluate(&mut app, "popcount(wide, 256)");
    assert!(text(&app).contains("= 256"), "{}", text(&app));
    app.extension_manager.reload_extension(0).unwrap();
    open(&mut app);
    evaluate(&mut app, "wide >> 252");
    assert!(text(&app).contains("= 15"), "{}", text(&app));
    assert!(!app.tabs[app.active_tab].has_unsaved_changes());
    key(&mut app, KeyCode::F(7), KeyModifiers::NONE);
    assert!(text(&app).contains("FUNCTION LIBRARY"));
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    paste(&mut app, "rotate");
    assert!(text(&app).contains("rol(value"));
    assert!(!text(&app).contains("mask(bits)"));
    assert!(!app.route_custom_editor_shortcut(KeyCode::F(1), KeyModifiers::NONE));
    let close = app
        .keybindings
        .iter()
        .position(|binding| binding.id == "close_tab")
        .unwrap();
    let original = app.keybindings[close].code;
    app.keybindings[close].code = KeyCode::Char('l');
    assert!(
        !app.route_custom_editor_shortcut(KeyCode::Char('l'), KeyModifiers::CONTROL),
        "A user-rebound workbench shortcut keeps priority"
    );
    app.keybindings[close].code = original;
}

#[test]
fn engineer_buddy_wasm_custom_width_wrapping_bits_and_registers_use_workbench_keys() {
    let root = tempfile::tempdir().unwrap();
    let mut app = app(root.path());
    open(&mut app);
    key(&mut app, KeyCode::F(2), KeyModifiers::NONE);
    paste(&mut app, "129");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    key(&mut app, KeyCode::F(4), KeyModifiers::NONE);
    key(&mut app, KeyCode::F(3), KeyModifiers::NONE);
    assert!(text(&app).contains("F3 Signed"));
    evaluate(&mut app, "2**129 + 7");
    assert!(text(&app).contains("= 7"), "{}", text(&app));
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('0'), KeyModifiers::NONE);
    assert!(text(&app).contains("= 6"), "{}", text(&app));
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert!(text(&app).contains("= 7"));
    key(&mut app, KeyCode::F(6), KeyModifiers::NONE);
    assert!(text(&app).contains("REGISTER FIELDS"));
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    key(&mut app, KeyCode::Insert, KeyModifiers::NONE);
    paste(&mut app, "opcode:6:0");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(text(&app).contains("opcode"), "{}", text(&app));
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    paste(&mut app, "51");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(text(&app).contains("= 51"), "{}", text(&app));
    key(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
    assert!(text(&app).contains("Go to bit"));
}
