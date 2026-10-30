//! Exercise the shipped Wasm through the actual extension host and workbench.
use crate::app::App;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use std::path::{Path, PathBuf};
use std::time::Duration;

const WASM: &[u8] = include_bytes!("../tests/fixtures/hex-editor/hex-editor.wasm");

#[test]
fn remote_hex_wasm_keeps_edits_and_undo_local_and_saves_at_remote_node() {
    let local = tempfile::tempdir().unwrap();
    let remote_dir = tempfile::tempdir().unwrap();
    let file = remote_dir.path().join("sample.bin");
    std::fs::write(&file, [0u8; 16]).unwrap();
    let workspace = crate::remote::workspace::Workspace::new(crate::remote::tests::connection(
        remote_dir.path(), Default::default(), Default::default(),
    ));
    let mut app = app(local.path());
    app.remote_workspace = Some(workspace.clone());
    let key_path = workspace.target_key("sample.bin").unwrap();
    open(&mut app, &key_path);
    for ch in "CAFEBABE".chars() { key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE); }
    key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
    drain(&mut app);
    assert_eq!(&std::fs::read(&file).unwrap()[..4], [0xCA, 0xFE, 0xBA, 0xBE]);
    assert!(!app.tabs[app.active_tab].has_unsaved_changes());
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    drain(&mut app);
    assert!(app.tabs[app.active_tab].has_unsaved_changes());
    key(&mut app, KeyCode::Char('y'), KeyModifiers::CONTROL);
    drain(&mut app);
    assert!(!app.tabs[app.active_tab].has_unsaved_changes());
    app.execute_palette_command("save_as");
    app.save_as_query = "copy.bin".into();
    app.execute_save_as();
    drain(&mut app);
    assert_eq!(std::fs::read(remote_dir.path().join("copy.bin")).unwrap(), std::fs::read(&file).unwrap());
}

#[test]
fn large_remote_binary_can_open_as_information_or_in_hex_viewer_without_a_full_transfer() {
    use std::io::Write;
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    let local = tempfile::tempdir().unwrap();
    let remote_dir = tempfile::tempdir().unwrap();
    let path = remote_dir.path().join("large.bin");
    let size = 32 * 1024 * 1024;
    let mut file = std::fs::File::create(&path).unwrap();
    file.set_len(size).unwrap();
    file.write_all(&vec![b'x'; 32 * 1024]).unwrap();
    drop(file);
    let downloaded = Arc::new(AtomicUsize::new(0));
    let workspace = crate::remote::workspace::Workspace::new(crate::remote::tests::connection_with_traffic(
        remote_dir.path(), Arc::default(), Arc::default(), Arc::default(), downloaded.clone(),
    ));
    let key_path = workspace.target_key("large.bin").unwrap();
    for hex in [false, true] {
        let mut app = app(local.path());
        app.remote_workspace = Some(workspace.clone());
        app.open_file(&key_path);
        app.wait_for_background(Duration::from_secs(15), |app| app.binary_open_prompt_active);
        assert!(!app.binary_open_prompt_open_normally);
        let command = app.binary_open_prompt_commands.iter().position(|command|
            command.extension_id == "vtcode.hex-editor" && command.command_id == "hex.open")
            .expect("Hex Viewer remains an available open option");
        app.execute_binary_open_prompt_choice(if hex { command + 1 } else { 0 });
        if hex {
            drain(&mut app);
            assert!(app.active_extension_view().is_some());
            assert!(text(&app).contains("78 78 78"), "the viewer reads the actual binary bytes");
        } else {
            app.wait_for_background(Duration::from_secs(15), |app|
                app.editor().is_some_and(|editor| !editor.bg_loading));
            let editor = app.editor().unwrap();
            assert_eq!(editor.encoding, "Binary");
            assert_eq!(editor.file_size, size);
            assert!(editor.read_only);
            assert!(editor.rope.len_bytes() < 1024);
            assert!(editor.rope.to_string().contains("Binary file - cannot display"));
        }
    }
    assert!(downloaded.load(Ordering::Relaxed) < 1024 * 1024,
        "neither the information tab nor Hex Viewer downloads the whole binary file");
}

fn app(root: &Path) -> App {
    let package = root.join("extensions/hex-editor");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("extension.json"),
        include_bytes!("../tests/fixtures/hex-editor/extension.json"),
    )
    .unwrap();
    std::fs::write(package.join("hex-editor.wasm"), WASM).unwrap();
    let mut app = App::new(root.to_path_buf());
    app.extension_manager.search_roots = vec![root.join("extensions")];
    app.extension_manager.settings_file = root.join("state/extension-settings.json");
    app.extension_manager.scan_and_sync();
    assert_eq!(
        app.extension_manager.error_count(),
        0,
        "{:?}",
        app.extension_views()
    );
    app
}

fn drain(app: &mut App) {
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
    panic!("extension input queue did not drain");
}

fn open(app: &mut App, path: &Path) {
    assert!(app.run_extension_command_with_optional_context(
        "vtcode.hex-editor",
        "hex.open",
        Some(path)
    ));
    drain(app);
    assert!(
        app.active_extension_view().is_some(),
        "{:?}",
        app.extension_views()
    );
}

fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    assert!(app.handle_active_extension_view_key(code, modifiers));
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
fn hex_wasm_edits_queue_without_dropping_input_and_save_binary_bytes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("sample.bin");
    std::fs::write(&path, [0u8; 16]).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    for ch in "CAFEBABE00112233".chars() {
        key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
    }
    key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
    drain(&mut app);
    assert_eq!(
        &std::fs::read(&path).unwrap()[..8],
        [0xCA, 0xFE, 0xBA, 0xBE, 0, 0x11, 0x22, 0x33]
    );
    assert!(!app.tabs[app.active_tab].has_unsaved_changes());
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    drain(&mut app);
    assert!(app.tabs[app.active_tab].has_unsaved_changes());
    assert!(
        app.extension_manager
            .unload_extension(0)
            .unwrap_err()
            .contains("Save or close")
    );
    assert!(
        app.extension_manager
            .reload_extension(0)
            .unwrap_err()
            .contains("Save or close")
    );
}

#[test]
fn hex_wasm_save_as_and_close_confirmation_use_the_binary_document() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("original.bin");
    let saved = root.path().join("saved.bin");
    std::fs::write(&path, [0, 1, 2]).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    assert!(app.paste_into_extension_editor("FE ED"));
    drain(&mut app);
    app.execute_palette_command("save_as");
    assert!(app.save_as_active);
    app.save_as_query = saved.display().to_string();
    app.execute_save_as();
    drain(&mut app);
    assert_eq!(std::fs::read(&saved).unwrap(), [0xFE, 0xED, 2]);
    assert_eq!(std::fs::read(&path).unwrap(), [0, 1, 2]);
    assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&saved));
    key(&mut app, KeyCode::Home, KeyModifiers::CONTROL);
    assert!(app.paste_into_extension_editor("AA"));
    drain(&mut app);
    app.close_tab(app.active_tab);
    assert!(app.confirm_close_active);
    crate::input_pickers::handle_confirm_close_key(&mut app, KeyCode::Char('y'));
    drain(&mut app);
    assert!(app.tabs.is_empty());
    assert_eq!(std::fs::read(&saved).unwrap()[0], 0xAA);
}

#[test]
fn hex_wasm_discard_and_external_conflicts_keep_the_correct_disk_content() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("sample.bin");
    std::fs::write(&path, [0, 1, 2]).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    assert!(app.paste_into_extension_editor("FF"));
    drain(&mut app);
    std::fs::write(&path, [10, 20, 30, 40]).unwrap();
    key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
    drain(&mut app);
    assert!(text(&app).contains("changed on disk"));
    assert!(app.tabs[app.active_tab].has_unsaved_changes());
    app.close_tab(app.active_tab);
    crate::input_pickers::handle_confirm_close_key(&mut app, KeyCode::Char('n'));
    drain(&mut app);
    assert!(app.tabs.is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), [10, 20, 30, 40]);
    open(&mut app, &path);
    assert!(text(&app).contains("0A 14 1E 28"));
}

#[test]
fn hex_wasm_large_search_yields_and_can_be_cancelled_without_losing_the_editor() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("large.bin");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(5 * 1024 * 1024 * 1024).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    assert!(app.paste_into_extension_editor("DE AD BE EF"));
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    drain(&mut app);
    assert!(text(&app).contains("Searching"), "{}", text(&app));
    app.idle_quiescent = true;
    app.terminal_has_focus = false;
    assert!(crate::app_event_loop::next_visible_extension_tick_wait(&app).is_some());
    let exports = app.extension_manager.records[0]
        .tick_handlers
        .iter()
        .map(|handler| handler.export_name.clone())
        .collect();
    app.extension_manager
        .invoke_tick_handlers_async("vtcode.hex-editor", exports, &mut app.background, None)
        .unwrap();
    drain(&mut app);
    assert!(text(&app).contains("Searching"));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    drain(&mut app);
    assert!(text(&app).contains("cancelled"));
    assert!(app.extension_manager.records[0].tick_handlers.is_empty());
}

fn workbench_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::input_keyboard::handle_keyboard_event(
        app,
        crossterm::event::KeyEvent::new(code, modifiers),
    );
}

#[test]
fn hex_wasm_long_replace_all_cancels_without_partial_changes_or_fuel_traps() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("dense.bin");
    let original = vec![b'a'; 200_000];
    std::fs::write(&path, &original).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    workbench_key(&mut app, KeyCode::Char('h'), KeyModifiers::CONTROL);
    assert!(app.paste_into_extension_editor("61"));
    workbench_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert!(app.paste_into_extension_editor(&"62 ".repeat(32)));
    workbench_key(
        &mut app,
        KeyCode::Enter,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    drain(&mut app);
    assert!(text(&app).contains("Replacing"), "{}", text(&app));
    assert!(app.tabs[app.active_tab].has_unsaved_changes());
    app.extension_manager
        .invoke_tick_handlers_async(
            "vtcode.hex-editor",
            vec!["hex_tick".into()],
            &mut app.background,
            None,
        )
        .unwrap();
    drain(&mut app);
    workbench_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    drain(&mut app);
    assert!(text(&app).contains("cancelled"));
    assert!(!app.tabs[app.active_tab].has_unsaved_changes());
    assert!(app.extension_manager.records[0].tick_handlers.is_empty());
    let info = app.extension_manager.records[0]
        .runtime
        .as_ref()
        .unwrap()
        .store
        .data()
        .documents[0]
        .info();
    assert!(!info.can_undo);
    assert_eq!(std::fs::read(path).unwrap(), original);
}

#[test]
fn hex_wasm_views_focus_existing_split_and_receive_the_first_click_in_inactive_panes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("data.bin");
    std::fs::write(&path, [0u8; 32]).unwrap();
    let mut app = app(root.path());
    app.new_file();
    open(&mut app, &path);
    app.create_split_group(1);
    assert_eq!(app.groups.len(), 2);
    let hex_group = app.active_group;
    let other_group = 1 - hex_group;
    let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    drain(&mut app);
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    app.switch_group(other_group);
    open(&mut app, &path);
    assert_eq!(app.active_group, hex_group);
    app.switch_group(other_group);
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    drain(&mut app);
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let area = app.groups[hex_group].code_area.unwrap();
    crate::mouse::handle_mouse(
        &mut app,
        crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: area.x + 13,
            row: area.y + 2,
            modifiers: KeyModifiers::NONE,
        },
    );
    drain(&mut app);
    assert_eq!(app.active_group, hex_group);
    assert!(app.active_extension_view().is_some());
    assert!(text(&app).contains("Offset 0x1 (1)"));
    assert_eq!(
        app.tabs
            .iter()
            .chain(
                app.groups
                    .iter()
                    .filter(|group| !group.tabs.is_empty())
                    .flat_map(|group| group.tabs.iter())
            )
            .filter(|tab| tab.extension_view.is_some())
            .count(),
        1
    );
}

#[test]
fn hex_wasm_find_replace_all_undo_and_save_as_use_real_keyboard_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("sample.bin");
    std::fs::write(&path, b"abcabc").unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    workbench_key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    drain(&mut app);
    assert!(text(&app).contains("Find [HEX]"));
    assert!(app.paste_into_extension_editor("61 62 63"));
    workbench_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    drain(&mut app);
    assert!(text(&app).contains("Match at 0x0"));
    workbench_key(&mut app, KeyCode::F(3), KeyModifiers::NONE);
    drain(&mut app);
    assert!(text(&app).contains("Match at 0x3"));
    workbench_key(&mut app, KeyCode::Char('h'), KeyModifiers::CONTROL);
    drain(&mut app);
    workbench_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert!(app.paste_into_extension_editor("FF"));
    workbench_key(
        &mut app,
        KeyCode::Enter,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    drain(&mut app);
    assert!(text(&app).contains("Replaced 2 matches"), "{}", text(&app));
    workbench_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    workbench_key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    drain(&mut app);
    assert!(!app.tabs[app.active_tab].has_unsaved_changes());
    assert!(text(&app).contains("61 62 63 61 62 63"));
    workbench_key(&mut app, KeyCode::Char('y'), KeyModifiers::CONTROL);
    drain(&mut app);
    workbench_key(
        &mut app,
        KeyCode::Char('s'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    assert!(app.save_as_active);
    let saved = root.path().join("copy.bin");
    app.save_as_query = saved.display().to_string();
    app.execute_save_as();
    drain(&mut app);
    assert_eq!(std::fs::read(saved).unwrap(), [255, 255]);
    assert_eq!(std::fs::read(path).unwrap(), b"abcabc");
}

#[test]
fn hex_wasm_rejects_dirty_text_in_other_tabs_and_preserves_edits_after_failed_exit() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("sample.bin");
    std::fs::write(&path, b"abc").unwrap();
    let mut app = app(root.path());
    app.new_file();
    app.editor_mut().unwrap().file_path = Some(path.clone());
    app.editor_mut().unwrap().modified = true;
    app.new_file();
    app.run_extension_command_with_optional_context("vtcode.hex-editor", "hex.open", Some(&path));
    drain(&mut app);
    assert!(app.active_extension_view().is_none());
    assert_eq!(std::fs::read(&path).unwrap(), b"abc");
    app.tabs[0].editor.modified = false;
    open(&mut app, &path);
    assert!(app.paste_into_extension_editor("FF"));
    drain(&mut app);
    app.tabs[0].editor.modified = true;
    assert!(app.start_custom_exit(false));
    drain(&mut app);
    assert!(app.custom_exit_failed);
    assert_eq!(app.custom_exit_remaining, 0);
    assert!(app.tabs[app.active_tab].has_unsaved_changes());
    assert_eq!(std::fs::read(&path).unwrap(), b"abc");
    app.tabs[0].editor.modified = false;
    assert!(app.start_custom_discard_exit());
    drain(&mut app);
    assert!(!app.custom_exit_failed);
    assert_eq!(std::fs::read(path).unwrap(), b"abc");
}

#[test]
fn hex_wasm_renders_themed_wide_narrow_and_tiny_surfaces() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("firmware.bin");
    std::fs::write(&path, (0..=255).cycle().take(4096).collect::<Vec<u8>>()).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    for (width, height) in [(140, 32), (100, 28), (60, 20), (16, 5), (1, 1)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        app.code_area = Some(Rect::new(0, 0, width, height));
        terminal
            .draw(|frame| {
                assert!(crate::extension_views::draw_active_view(
                    frame,
                    &mut app,
                    frame.area()
                ));
            })
            .unwrap();
        drain(&mut app);
        terminal
            .draw(|frame| {
                assert!(crate::extension_views::draw_active_view(
                    frame,
                    &mut app,
                    frame.area()
                ));
            })
            .unwrap();
        assert!(!app.extension_manager.has_busy_runtimes());
        if width >= 60 {
            let shown: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(shown.contains("OFFSET"));
            assert!(shown.contains("DATA INSPECTOR"));
        }
        if let Some(output) = std::env::var_os("VTCODE_HEX_VISUAL_DIR") {
            let directory = PathBuf::from(output);
            std::fs::create_dir_all(&directory).unwrap();
            let cells = terminal.backend().buffer().content.iter().map(|cell| serde_json::json!({
                "text": cell.symbol(), "fg": format!("{:?}", cell.fg), "bg": format!("{:?}", cell.bg)
            })).collect::<Vec<_>>();
            std::fs::write(
                directory.join(format!("hex-{width}x{height}.json")),
                serde_json::to_vec(
                    &serde_json::json!({"width":width,"height":height,"cells":cells}),
                )
                .unwrap(),
            )
            .unwrap();
        }
    }
}

#[test]
fn hex_wasm_edited_and_search_surfaces_match_workbench_theme_contrast() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("firmware.bin");
    std::fs::write(&path, (0..=255).cycle().take(1024).collect::<Vec<u8>>()).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    assert!(app.paste_into_extension_editor("CA FE BA BE"));
    drain(&mut app);
    workbench_key(&mut app, KeyCode::Char('h'), KeyModifiers::CONTROL);
    assert!(app.paste_into_extension_editor("CA FE BA BE"));
    drain(&mut app);
    let mut failures = Vec::new();
    for (index, theme) in crate::ui_control_visibility_tests::audit_themes()
        .into_iter()
        .enumerate()
    {
        app.colors = theme.to_theme_colors();
        let (width, height) = (140, 32);
        let area = Rect::new(0, 0, width, height);
        app.code_area = Some(area);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                crate::extension_views::draw_active_view(frame, &mut app, area);
            })
            .unwrap();
        drain(&mut app);
        terminal
            .draw(|frame| {
                crate::extension_views::draw_active_view(frame, &mut app, area);
            })
            .unwrap();
        crate::ui_control_visibility_tests::check_region(
            terminal.backend().buffer(),
            area,
            &theme.name,
            &mut failures,
            &app.colors.ui,
        );
        if index < 3 {
            write_visual(&terminal, &format!("hex-edit-search-theme-{index}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    app.colors = crate::app_theme::load_builtin_themes()[0].to_theme_colors();
    workbench_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    drain(&mut app);
    app.code_area = None;
    let mut terminal = Terminal::new(TestBackend::new(160, 44)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    drain(&mut app);
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    write_visual(&terminal, "hex-workbench");
    let code = app
        .code_area
        .expect("custom editors publish their actual workbench hit area");
    assert!(code.x > 0);
    assert!(code.width < 160);
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let click = |app: &mut App, column, row, modifiers| {
        crate::mouse::handle_mouse(
            app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row,
                modifiers,
            },
        )
    };
    click(&mut app, code.x + 10, code.y + 2, KeyModifiers::NONE);
    drain(&mut app);
    assert!(text(&app).contains("Offset 0x0 (0)"));
    click(&mut app, code.x + 16, code.y + 2, KeyModifiers::SHIFT);
    drain(&mut app);
    assert!(text(&app).contains("2 selected"));
    let shown = text(&app);
    let save_column = shown.lines().next().unwrap().find("Save").unwrap() as u16;
    click(&mut app, code.x + save_column, code.y, KeyModifiers::NONE);
    drain(&mut app);
    assert_eq!(
        &std::fs::read(&path).unwrap()[..4],
        [0xCA, 0xFE, 0xBA, 0xBE]
    );
}

fn write_visual(terminal: &Terminal<TestBackend>, name: &str) {
    let Some(output) = std::env::var_os("VTCODE_HEX_VISUAL_DIR") else {
        return;
    };
    let directory = PathBuf::from(output);
    std::fs::create_dir_all(&directory).unwrap();
    let buffer = terminal.backend().buffer();
    let cells: Vec<_> = buffer.content.iter().map(|cell| serde_json::json!({"text":cell.symbol(),"fg":format!("{:?}",cell.fg),"bg":format!("{:?}",cell.bg)})).collect();
    std::fs::write(directory.join(format!("{name}.json")), serde_json::to_vec(&serde_json::json!({"width":buffer.area.width,"height":buffer.area.height,"cells":cells})).unwrap()).unwrap();
}

#[test]
fn hex_wasm_keyboard_shortcuts_and_session_recovery_use_the_workbench_routes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("sample.bin");
    std::fs::write(&path, [0u8; 4]).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    for ch in "CAFE".chars() {
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            crossterm::event::KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
        );
    }
    drain(&mut app);
    assert!(app.tabs[app.active_tab].has_unsaved_changes());
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        crossterm::event::KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
    );
    drain(&mut app);
    assert_eq!(&std::fs::read(&path).unwrap()[..2], [0xCA, 0xFE]);
    assert!(app.paste_into_extension_editor("BA BE"));
    drain(&mut app);
    assert!(app.start_custom_exit(true));
    drain(&mut app);
    assert_eq!(app.custom_exit_remaining, 0);
    assert!(!app.custom_exit_failed);
    let records = crate::extension_recovery::read(&crate::extension_recovery::directory(
        &app.extension_manager.settings_file,
    ));
    assert_eq!(records.len(), 1);
    drop(app);
    let mut restored = self::app(root.path());
    restored.pending_custom_recoveries = records;
    restored.restore_pending_custom_documents();
    drain(&mut restored);
    assert!(restored.active_extension_view().is_some());
    assert!(restored.tabs[restored.active_tab].has_unsaved_changes());
    assert!(text(&restored).contains("CA FE BA BE"));
    restored.execute_palette_command("save_all");
    drain(&mut restored);
    assert_eq!(std::fs::read(&path).unwrap(), [0xCA, 0xFE, 0xBA, 0xBE]);
    assert!(
        crate::extension_recovery::read(&crate::extension_recovery::directory(
            &restored.extension_manager.settings_file
        ))
        .is_empty()
    );
}
