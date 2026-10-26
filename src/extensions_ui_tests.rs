use crate::{
    app::{ActivePane, App, TabKind},
    extensions::ExtensionState,
    ui_extensions::{self, Action},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};

fn fixture(count: usize) -> (tempfile::TempDir, App) {
    let root = tempfile::tempdir().unwrap();
    for index in 0..count {
        let directory = root.path().join(format!("ext{index:02}"));
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(
            directory.join("extension.json"),
            serde_json::json!({
                "id":format!("test.ext{index:02}"), "name":format!("Test Extension {index:02}"),
                "version":"1.0.0", "description":"Useful tools for your workspace",
                "main":"extension.wasm", "enabled":false,
                "repository":"https://github.com/vtcode/example"
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(directory.join("README.md"), format!("# Test Extension {index:02}\n\nDocumentation for this extension.\n\n## Getting started\n\n{}", (0..60).map(|line| format!("- Documentation line {line:02}: Unicode 世界 👩‍💻 e\u{301}\n")).collect::<String>())).unwrap();
    }
    let mut app = App::test_new();
    app.extension_manager.search_roots = vec![root.path().to_path_buf()];
    app.extension_manager.scan_and_sync();
    assert_eq!(app.extension_manager.len(), count);
    for (index, record) in app.extension_manager.records.iter_mut().enumerate() {
        record.state = match index % 3 {
            0 => ExtensionState::Loaded,
            1 => ExtensionState::Error,
            _ => ExtensionState::Unloaded,
        };
        record.manifest.enabled = record.state != ExtensionState::Unloaded;
        record.last_error =
            (record.state == ExtensionState::Error).then(|| "An example activation failure".into());
        record.status_text = "Ready".into();
        record.logs = (0..40)
            .map(|line| format!("log {line:02}: {}", "世界 👩‍💻 e\u{301} ".repeat(10)))
            .collect();
    }
    app.open_extensions_sidebar();
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    (root, app)
}
fn sidebar(app: &mut App, terminal: &mut Terminal<TestBackend>) {
    app.explorer_area = Some(terminal.backend().buffer().area);
    terminal
        .draw(|f| ui_extensions::draw(f, app, f.area()))
        .unwrap();
}
fn info(app: &mut App, terminal: &mut Terminal<TestBackend>) {
    terminal
        .draw(|f| crate::ui_extension_info::draw_extension_info(f, app, f.area()))
        .unwrap();
}
fn key(app: &mut App, code: KeyCode) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, KeyModifiers::empty()));
}
fn event(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::empty(),
    }
}
fn text(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}
fn find(terminal: &Terminal<TestBackend>, value: &str) -> (u16, u16) {
    let buffer = terminal.backend().buffer();
    for y in buffer.area.y..buffer.area.bottom() {
        let row: String = (buffer.area.x..buffer.area.right())
            .map(|x| buffer[(x, y)].symbol())
            .collect();
        if let Some(index) = row.find(value) {
            return (row[..index].chars().count() as u16, y);
        }
    }
    panic!("Missing {value}: {}", text(terminal));
}

#[test]
fn extensions_ui_navigation_respects_queries_and_status_filters() {
    let (_root, mut app) = fixture(9);
    let mut terminal = Terminal::new(TestBackend::new(44, 29)).unwrap();
    app.extensions_filter = "@error extension".into();
    sidebar(&mut app, &mut terminal);
    assert_eq!(app.extensions_selected, 1);
    key(&mut app, KeyCode::Down);
    assert_eq!(app.extensions_selected, 4);
    key(&mut app, KeyCode::End);
    assert_eq!(app.extensions_selected, 7);
    assert_eq!(
        crate::ui_extension_info::tab_extension_id(&app.tabs[app.active_tab]).as_deref(),
        Some("test.ext07")
    );
    key(&mut app, KeyCode::Home);
    assert_eq!(app.extensions_selected, 1);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.tabs[app.active_tab].kind, TabKind::ExtensionInfo);
    assert_eq!(
        crate::ui_extension_info::tab_extension_id(&app.tabs[app.active_tab]).as_deref(),
        Some("test.ext01")
    );
    app.active_pane = ActivePane::Explorer;
    app.extensions_filter = "no matches".into();
    sidebar(&mut app, &mut terminal);
    let count = app.tabs.len();
    key(&mut app, KeyCode::Enter);
    app.extensions_unload_selected();
    assert_eq!(app.tabs.len(), count);
    assert_eq!(
        app.extension_manager.records[0].state,
        ExtensionState::Loaded
    );
    assert!(app.ext_sidebar.rows.is_empty());
    key(&mut app, KeyCode::Esc);
    assert!(app.extensions_filter.is_empty());
}

#[test]
fn extensions_ui_browsing_reuses_previews_and_explicit_open_keeps_pages() {
    let (_root, mut app) = fixture(3);
    let mut terminal = Terminal::new(TestBackend::new(44, 29)).unwrap();
    sidebar(&mut app, &mut terminal);
    let first = app.ext_sidebar.rows[0].1;
    crate::mouse::handle_mouse(
        &mut app,
        event(
            MouseEventKind::Down(MouseButton::Left),
            first.x + 7,
            first.y,
        ),
    );
    assert!(app.tabs[app.active_tab].preview);
    assert_eq!(app.active_pane, ActivePane::Explorer);
    sidebar(&mut app, &mut terminal);
    let second = app.ext_sidebar.rows[1].1;
    crate::mouse::handle_mouse(
        &mut app,
        event(
            MouseEventKind::Down(MouseButton::Left),
            second.x + 7,
            second.y,
        ),
    );
    assert_eq!(
        app.tabs
            .iter()
            .filter(|tab| tab.kind == TabKind::ExtensionInfo)
            .count(),
        1
    );
    key(&mut app, KeyCode::Enter);
    assert!(!app.tabs[app.active_tab].preview);
    ui_extensions::handle_click(&mut app, 1, 2);
    assert!(app.ext_sidebar.search_focused);
    app.preview_extension_info_tab("test.ext00");
    assert_eq!(
        app.tabs
            .iter()
            .filter(|tab| tab.kind == TabKind::ExtensionInfo)
            .count(),
        2
    );
    assert!(
        !crate::ui_chrome_tabs::tab_display_name(&app, app.active_tab, &app.tabs[app.active_tab])
            .contains("ext-info:")
    );
}

#[test]
fn extensions_ui_scroll_does_not_change_selection_and_filter_clicks_preserve_search() {
    let (_root, mut app) = fixture(12);
    let mut terminal = Terminal::new(TestBackend::new(44, 29)).unwrap();
    sidebar(&mut app, &mut terminal);
    let selected = app.extensions_selected;
    let area = app.ext_sidebar.list_area;
    crate::mouse_scroll::handle_scroll_down(
        &mut app,
        area.x + 7,
        area.y + 1,
        KeyModifiers::empty(),
    );
    sidebar(&mut app, &mut terminal);
    assert!(app.extensions_scroll > 0);
    assert_eq!(app.extensions_selected, selected);
    app.extensions_filter = "Extension @ENABLED".into();
    sidebar(&mut app, &mut terminal);
    let (_, filter) = app
        .ext_sidebar
        .actions
        .iter()
        .find(|(action, _)| *action == Action::Filter("@disabled"))
        .copied()
        .unwrap();
    ui_extensions::handle_click(&mut app, filter.x, filter.y);
    assert_eq!(app.extensions_filter, "Extension @disabled");
    assert_eq!(app.extensions_selected, 2);
    sidebar(&mut app, &mut terminal);
    assert!(app.ext_sidebar.rows.iter().all(|(index, _)| index % 3 == 2));
    let (_, collapse) = app
        .ext_sidebar
        .actions
        .iter()
        .find(|(action, _)| *action == Action::Collapse)
        .copied()
        .unwrap();
    ui_extensions::handle_click(&mut app, collapse.x, collapse.y);
    sidebar(&mut app, &mut terminal);
    assert!(app.ext_sidebar.rows.is_empty());
    key(&mut app, KeyCode::Down);
    assert!(!app.ext_sidebar.collapsed);
}

#[test]
fn extensions_ui_info_actions_target_the_open_page_and_protect_it_from_edits() {
    let (_root, mut app) = fixture(3);
    app.open_extension_info_tab("test.ext00");
    app.extensions_selected = 1;
    let mut terminal = Terminal::new(TestBackend::new(110, 35)).unwrap();
    info(&mut app, &mut terminal);
    assert!(text(&terminal).contains("Getting started"));
    let (x, y) = find(&terminal, "Disable");
    crate::mouse::handle_mouse(
        &mut app,
        event(MouseEventKind::Down(MouseButton::Left), x, y),
    );
    assert_eq!(
        app.extension_manager.records[0].state,
        ExtensionState::Unloaded
    );
    assert_eq!(
        app.extension_manager.records[1].state,
        ExtensionState::Error
    );
    let before = app.tabs[app.active_tab].editor.rope.to_string();
    key(&mut app, KeyCode::Char('x'));
    assert_eq!(app.tabs[app.active_tab].editor.rope.to_string(), before);
    assert!(!app.tabs[app.active_tab].editor.modified);
    info(&mut app, &mut terminal);
    key(&mut app, KeyCode::End);
    info(&mut app, &mut terminal);
    assert!(text(&terminal).contains("Documentation line 59"));
    key(&mut app, KeyCode::Right);
    info(&mut app, &mut terminal);
    assert!(text(&terminal).contains("Commands"));
    key(&mut app, KeyCode::Right);
    info(&mut app, &mut terminal);
    key(&mut app, KeyCode::End);
    info(&mut app, &mut terminal);
    assert!(text(&terminal).contains("log 39"));
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.active_pane, ActivePane::Explorer);
}

#[test]
fn extensions_ui_tiny_and_offset_layouts_keep_hit_regions_inside_their_surface() {
    let (_root, mut app) = fixture(3);
    for width in [1, 5, 16, 24, 38, 60] {
        for height in [1, 2, 5, 12, 24] {
            let mut terminal = Terminal::new(TestBackend::new(80, 35)).unwrap();
            let bounds = Rect::new(3, 2, width, height);
            terminal
                .draw(|f| ui_extensions::draw(f, &mut app, bounds))
                .unwrap();
            assert!(
                app.ext_sidebar
                    .actions
                    .iter()
                    .all(|(_, area)| area.intersection(bounds) == *area)
            );
            assert!(
                app.ext_sidebar
                    .rows
                    .iter()
                    .all(|(_, area)| area.intersection(bounds) == *area)
            );
            app.open_extension_info_tab("test.ext00");
            terminal
                .draw(|f| crate::ui_extension_info::draw_extension_info(f, &mut app, bounds))
                .unwrap();
            app.active_pane = ActivePane::Explorer;
        }
    }
    assert_eq!(ui_extensions::clip("👩‍💻e\u{301}世界", 4), "👩‍💻e\u{301}…");
}

#[test]
fn extensions_ui_split_pages_keep_independent_sections_and_active_actions() {
    let (_root, mut app) = fixture(3);
    app.open_extension_info_tab("test.ext00");
    app.open_extension_info_tab("test.ext01");
    app.create_split_group(app.active_tab);
    let active_group = app.active_group;
    let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
    terminal
        .draw(|f| crate::ui_chrome::draw_editor_section(f, &mut app, f.area()))
        .unwrap();
    let (x, y) = find(&terminal, "Retry");
    crate::mouse::handle_mouse(
        &mut app,
        event(MouseEventKind::Down(MouseButton::Left), x, y),
    );
    assert_eq!(
        app.extension_manager.records[0].state,
        ExtensionState::Loaded
    );
    app.switch_group(1 - active_group);
    // Keys arriving between a tab switch and repaint must belong to the new page.
    key(&mut app, KeyCode::Right);
    terminal
        .draw(|f| crate::ui_chrome::draw_editor_section(f, &mut app, f.area()))
        .unwrap();
    assert!(text(&terminal).contains("Commands"));
    let (x, y) = find(&terminal, "Disable");
    crate::mouse::handle_mouse(
        &mut app,
        event(MouseEventKind::Down(MouseButton::Left), x, y),
    );
    assert_eq!(
        app.extension_manager.records[0].state,
        ExtensionState::Unloaded
    );
}

#[test]
fn ui_combinations_inactive_extension_page_scrolls_without_moving_the_active_file() {
    let (_root, mut app) = fixture(1);
    app.open_extension_info_tab("test.ext00");
    app.tabs.push(crate::app::Tab::new_file(
        crate::editor::Editor::test_with_text(&"file line\n".repeat(200)),
    ));
    app.create_split_group(app.tabs.len() - 1);
    let group = app.active_group;
    let mut terminal = Terminal::new(TestBackend::new(180, 42)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let (x, y) = find(&terminal, "Documentation line 00");
    let before = app.editor().unwrap().scroll_offset;
    for _ in 0..10 {
        crate::mouse::handle_mouse(&mut app, event(MouseEventKind::ScrollDown, x, y));
    }
    assert_eq!(app.active_group, group);
    assert_eq!(app.editor().unwrap().scroll_offset, before);
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    assert!(!text(&terminal).contains("Documentation line 00"));
}

#[test]
fn ui_combinations_inactive_extension_sections_activate_on_the_first_click() {
    let (_root, mut app) = fixture(1);
    app.open_extension_info_tab("test.ext00");
    app.tabs.push(crate::app::Tab::new_file(
        crate::editor::Editor::test_with_text("file line\n"),
    ));
    app.create_split_group(app.tabs.len() - 1);
    let group = app.active_group;
    let mut terminal = Terminal::new(TestBackend::new(180, 42)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let (x, y) = find(&terminal, "FEATURES");
    crate::mouse::handle_mouse(
        &mut app,
        event(MouseEventKind::Down(MouseButton::Left), x, y),
    );
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    assert_ne!(app.active_group, group);
    assert!(text(&terminal).contains("Commands"));
}

#[test]
fn ui_combinations_replaced_extension_identity_rejects_previous_page_hit_areas() {
    let (_root, mut app) = fixture(2);
    app.open_extension_info_tab("test.ext00");
    let mut terminal = Terminal::new(TestBackend::new(180, 42)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
    let (x, y) = find(&terminal, "FEATURES");
    // A preview can be replaced between an input event and the next frame.
    // Keep the view identity to verify the represented extension as well.
    let index = app.active_tab;
    app.tabs[index].editor.file_path = Some("Test Extension 01 [ext-info:test.ext01]".into());
    crate::mouse::handle_mouse(&mut app, event(MouseEventKind::Down(MouseButton::Left), x, y));
    terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
    assert!(text(&terminal).contains("Documentation line 00"));
    assert!(!text(&terminal).contains("Commands"));
}

#[test]
fn extensions_ui_presentation() {
    let (_root, mut app) = fixture(6);
    let names = [
        "Hex Editor",
        "Rust Tools",
        "JSON Toolkit",
        "Markdown Preview",
        "Git Utilities",
        "Color Themes",
    ];
    for (record, name) in app.extension_manager.records.iter_mut().zip(names) {
        record.manifest.name = name.into();
        record.manifest.description = match name {
            "Hex Editor" => "Inspect and edit binary files with a hex / ASCII view.",
            "Rust Tools" => "Language tools and diagnostics for Rust.",
            "JSON Toolkit" => "Format, validate, and explore JSON documents.",
            _ => "Useful tools for your everyday editing.",
        }
        .into();
    }
    app.open_extension_info_tab("test.ext00");
    app.active_pane = ActivePane::Explorer;
    let readme = app.extension_manager.records[0]
        .manifest_path
        .parent()
        .unwrap()
        .join("README.md");
    std::fs::write(readme, "# Hex Editor\n\nInspect and edit binary files directly in VTCode, with synchronized hexadecimal and ASCII views.\n\n## Getting started\n\nOpen a binary file and choose **Hex Editor** from the editor picker. Select a byte to inspect its value, then type to edit.\n\n## Built for binary editing\n\n- Browse bytes in hexadecimal and ASCII.\n- Find and replace byte sequences.\n- Undo and redo changes before saving.\n- Keep large files responsive while you work.\n\n## Keyboard shortcuts\n\n| Action | Shortcut |\n| --- | --- |\n| Find bytes | Ctrl+F |\n| Save changes | Ctrl+S |\n| Undo | Ctrl+Z |\n\n## Extension commands\n\nUse the **Features** tab to see registered commands. Open **Runtime** to inspect activation status and console output.\n").unwrap();
    for (width, height, sidebar_width, name) in [
        (168, 46, 44, "extensions-wide"),
        (100, 30, 36, "extensions-compact"),
    ] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| {
                ui_extensions::draw(f, &mut app, Rect::new(0, 0, sidebar_width, height));
                crate::ui_chrome::draw_editor_section(
                    f,
                    &mut app,
                    Rect::new(sidebar_width + 1, 0, width - sidebar_width - 1, height),
                );
            })
            .unwrap();
        assert!(text(&terminal).contains("Getting started"));
        crate::ui_control_visibility_tests::snapshot(terminal.backend().buffer(), name);
    }
}
