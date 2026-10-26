use crate::app::{App, SplitDirection};
use crate::settings::Settings;
use std::time::Duration;

fn workspace() -> (tempfile::TempDir, App) {
    let directory = tempfile::tempdir().unwrap();
    for name in ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"] {
        std::fs::write(directory.path().join(name), format!("{name}\n")).unwrap();
    }
    let mut app = App::test_new();
    app.explorer.root = directory.path().to_path_buf();
    (directory, app)
}

fn wait_for_files(app: &mut App) {
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.tabs.iter().all(|tab| !tab.editor.bg_loading)
            && app
                .groups
                .iter()
                .all(|group| group.tabs.iter().all(|tab| !tab.editor.bg_loading))
    });
}

#[test]
fn preview_clicks_replace_loading_files_and_second_click_keeps_the_file() {
    let (directory, mut app) = workspace();
    let a = directory.path().join("a.txt");
    let b = directory.path().join("b.txt");
    let c = directory.path().join("c.txt");
    assert!(app.enable_preview_tabs);

    app.open_file_preview(&a);
    assert!(app.tabs[0].preview);
    assert!(app.tabs[0].editor.bg_loading);
    app.open_file_preview(&b);
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&b));
    wait_for_files(&mut app);
    assert_eq!(app.editor().unwrap().rope.to_string(), "b.txt\n");
    assert!(app.tabs[0].preview);

    app.open_file_preview(&b);
    assert!(!app.tabs[0].preview);
    assert!(!app.tabs[0].pinned);
    app.open_file_preview(&c);
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.tabs[0].editor.file_path.as_ref(), Some(&b));
    assert!(app.tabs[1].preview);
    // An explicit open also keeps an existing preview, without duplicating it.
    app.open_file(&c);
    assert!(!app.tabs[1].preview);
    assert_eq!(app.tabs.len(), 2);
}

#[test]
fn preview_browsing_preserves_edited_and_pinned_files() {
    for pinned in [false, true] {
        let (directory, mut app) = workspace();
        let a = directory.path().join("a.txt");
        app.open_file_preview(&a);
        wait_for_files(&mut app);
        if pinned {
            app.tabs[0].pinned = true;
        } else {
            app.execute_palette_command("duplicate_line");
            assert!(app.editor().unwrap().modified);
        }
        let content = app.editor().unwrap().rope.to_string();
        app.open_file_preview(&directory.path().join("b.txt"));
        assert_eq!(app.tabs.len(), 2);
        assert!(!app.tabs[0].preview);
        assert!(app.tabs[1].preview);
        assert_eq!(app.tabs[0].editor.rope.to_string(), content);
        assert_eq!(std::fs::read_to_string(a).unwrap(), "a.txt\n");
        assert!(!app.confirm_close_active);
    }
}

#[test]
fn preview_setting_disables_replacement_and_preserves_open_files() {
    let (directory, mut app) = workspace();
    assert_eq!(app.current_settings().enable_preview_tabs, Some(true));
    app.open_file_preview(&directory.path().join("a.txt"));
    let disabled: Settings = serde_json::from_str(r#"{"enablePreviewTabs":false}"#).unwrap();
    app.apply_settings(&disabled);
    assert!(!app.enable_preview_tabs);
    assert!(!app.tabs[0].preview);
    for name in ["b.txt", "c.txt"] {
        app.open_file_preview(&directory.path().join(name));
    }
    assert_eq!(app.tabs.len(), 3);
    assert!(app.tabs.iter().all(|tab| !tab.preview));
    let saved = serde_json::to_string(&app.current_settings()).unwrap();
    let mut restored = App::test_new();
    restored.apply_settings(&serde_json::from_str(&saved).unwrap());
    assert!(!restored.enable_preview_tabs);
    assert_eq!(App::setting_field_kind("preview_tabs"), "checkbox");
    assert_eq!(
        App::setting_json_key("preview_tabs"),
        Some("enablePreviewTabs")
    );

    app.set_preview_tabs_enabled(true);
    app.open_file_preview(&directory.path().join("d.txt"));
    assert_eq!(app.tabs.len(), 4);
    assert!(app.tabs[3].preview);
}

#[test]
fn preview_session_restore_preserves_the_reusable_tab() {
    let (directory, mut app) = workspace();
    app.open_file_preview(&directory.path().join("a.txt"));
    app.open_file(&directory.path().join("b.txt"));
    wait_for_files(&mut app);
    let serialized = serde_json::to_string(&app.build_session()).unwrap();
    let session = serde_json::from_str(&serialized).unwrap();
    let mut restored = App::test_new();
    restored.explorer.root = directory.path().to_path_buf();
    restored.restore_session(&session);
    wait_for_files(&mut restored);
    assert!(restored.tabs[0].preview);
    assert!(!restored.tabs[1].preview);
    restored.open_file_preview(&directory.path().join("c.txt"));
    assert_eq!(restored.tabs.len(), 2);
    assert_eq!(restored.tabs[0].editor.file_name(), "b.txt");
    assert_eq!(restored.tabs[1].editor.file_name(), "c.txt");
    assert!(restored.tabs[1].preview);

    let mut disabled = App::test_new();
    disabled.set_preview_tabs_enabled(false);
    disabled.restore_session(&session);
    assert!(disabled.tabs.iter().all(|tab| !tab.preview));
}

#[test]
fn preview_tabs_are_reused_within_their_own_split_group() {
    let (directory, mut app) = workspace();
    app.open_file(&directory.path().join("a.txt"));
    app.open_file(&directory.path().join("b.txt"));
    app.create_split_group_dir(1, SplitDirection::Horizontal);
    app.open_file_preview(&directory.path().join("c.txt"));
    let other_group = app.active_group;
    app.switch_group(0);
    app.open_file_preview(&directory.path().join("d.txt"));
    app.open_file_preview(&directory.path().join("e.txt"));
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.editor().unwrap().file_name(), "e.txt");
    assert_eq!(app.groups[other_group].tabs.len(), 2);
    assert!(app.groups[other_group].tabs[1].preview);
    assert_eq!(app.groups[other_group].tabs[1].editor.file_name(), "c.txt");

    assert!(app.move_tab_between_groups(other_group, 1, app.active_group));
    assert_eq!(app.tabs.iter().filter(|tab| tab.preview).count(), 1);
    assert_eq!(app.tabs.last().unwrap().editor.file_name(), "c.txt");
    assert!(!app.tabs.last().unwrap().preview);
    app.set_preview_tabs_enabled(false);
    assert!(app.tabs.iter().all(|tab| !tab.preview));
}

#[test]
fn preview_tab_is_italic_when_active_and_double_click_keeps_it_open() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend, layout::Rect, style::Modifier};

    let (directory, mut app) = workspace();
    app.open_file_preview(&directory.path().join("a.txt"));
    app.open_file(&directory.path().join("b.txt"));
    app.tab_bar_area = Some(Rect::new(0, 1, 80, 1));
    let area = app.tab_bar_area.unwrap();
    let mut terminal = Terminal::new(TestBackend::new(80, 3)).unwrap();
    let mut label_column = 0;
    for active in [0, 1] {
        app.active_tab = active;
        terminal
            .draw(|frame| crate::ui_chrome_tabs::draw_tab_bar(frame, &app, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        label_column = (0..70)
            .find(|&column| {
                let label: String = (column..column + 5)
                    .map(|x| buffer.cell((x, area.y)).unwrap().symbol())
                    .collect();
                label == "a.txt"
            })
            .expect("preview filename");
        assert!(
            buffer
                .cell((label_column, area.y))
                .unwrap()
                .modifier
                .contains(Modifier::ITALIC)
        );
    }
    let click = |app: &mut App, kind| {
        crate::mouse::handle_mouse(
            app,
            MouseEvent {
                kind,
                column: label_column,
                row: area.y,
                modifiers: KeyModifiers::NONE,
            },
        );
    };
    click(&mut app, MouseEventKind::Down(MouseButton::Left));
    assert_eq!(app.active_tab, 0);
    assert!(
        app.tabs[0].preview,
        "one click on an inactive tab only focuses it"
    );
    click(&mut app, MouseEventKind::Up(MouseButton::Left));
    click(&mut app, MouseEventKind::Down(MouseButton::Left));
    click(&mut app, MouseEventKind::Up(MouseButton::Left));
    assert!(!app.tabs[0].preview);
    assert!(!app.tabs[0].pinned);
    terminal
        .draw(|frame| crate::ui_chrome_tabs::draw_tab_bar(frame, &app, area))
        .unwrap();
    assert!(
        !terminal
            .backend()
            .buffer()
            .cell((label_column, area.y))
            .unwrap()
            .modifier
            .contains(Modifier::ITALIC)
    );
}
