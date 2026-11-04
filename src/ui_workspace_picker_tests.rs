use super::*;
use crate::{
    app::{ActivePane, Tab},
    app_workspaces::{Parked, Request},
    ui_workspace_sets::Mode,
    workspace_sets::{Catalog, Location, SavedSet, Scope},
};
use crossterm::event::{KeyCode, KeyEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn fixture() -> (tempfile::TempDir, App) {
    let root = tempfile::tempdir().unwrap();
    let wake = crate::background::UiWake::default();
    let mut app = App::new_startup(root.path().join("api"), wake.clone());
    app.tabs = vec![Tab::new_file(crate::editor::Editor::test_with_text(
        "fn main() {\n    run();\n}\n",
    ))];
    app.active_pane = ActivePane::Editor;
    app.source_control_enabled = false;
    for (id, name) in [(1, "web"), (2, "infra")] {
        let mut hidden = App::new_startup(root.path().join(name), wake.clone());
        hidden.workspace_visible = false;
        hidden.source_control_enabled = false;
        app.workspaces.parked.push(Parked {
            id,
            app: Box::new(hidden),
        });
    }
    let roots = std::iter::once(&app.explorer.root)
        .chain(
            app.workspaces
                .parked
                .iter()
                .map(|parked| &parked.app.explorer.root),
        )
        .map(|path| Location::local(path, &crate::portable::base_dir()))
        .collect::<Vec<_>>();
    app.workspaces.sets.loaded = true;
    app.workspaces.sets.active = Some(10);
    app.workspaces.sets.catalog = Catalog::default();
    app.workspaces.sets.catalog.sets = ["Engineering", "Operations", "Release λ"]
        .into_iter()
        .enumerate()
        .map(|(index, name)| SavedSet {
            id: index as u64 + 10,
            name: name.into(),
            scope: Scope::Local,
            roots: roots.clone(),
            active: 0,
        })
        .collect();
    app.workspaces.sets.path = root.path().join("sets.json");
    (root, app)
}
fn draw(app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
    terminal.backend().buffer().clone()
}
fn text(buffer: &Buffer) -> String {
    buffer.content.iter().map(|cell| cell.symbol()).collect()
}
fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::ui_workspaces::handle_key(app, KeyEvent::new(code, modifiers));
}
fn click(app: &mut App, rect: Rect) {
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        },
    );
}
fn row_containing(buffer: &Buffer, area: Rect, name: &str) -> u16 {
    (area.y..area.bottom())
        .find(|&y| {
            (area.x..area.right())
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .contains(name)
        })
        .unwrap()
}

#[test]
fn title_controls_and_tabs_switch_in_one_click_and_toggle_closed() {
    let (_root, mut app) = fixture();
    draw(&mut app, 120, 28);
    let directory = crate::ui_chrome::workspace_button_area(&app).unwrap();
    click(&mut app, directory);
    draw(&mut app, 120, 28);
    let saved_tab = app.workspaces.popup.tabs[1];
    click(&mut app, saved_tab);
    assert!(matches!(app.workspaces.popup.sets.mode, Some(Mode::List)));
    draw(&mut app, 120, 28);
    click(&mut app, directory);
    assert!(!app.workspaces.popup.open);
    click(&mut app, directory);
    draw(&mut app, 120, 28);
    let saved_tab = app.workspaces.popup.tabs[1];
    click(&mut app, saved_tab);
    draw(&mut app, 120, 28);
    let workspace_tab = app.workspaces.popup.tabs[0];
    click(&mut app, workspace_tab);
    assert!(app.workspaces.popup.open && app.workspaces.popup.sets.mode.is_none());
    let buffer = draw(&mut app, 120, 28);
    let area = app.workspaces.popup.area;
    let add_row = row_containing(&buffer, area, "+ Add folder");
    let add_col = (area.x..area.right())
        .find(|&x| buffer[(x, add_row)].symbol() == "+")
        .unwrap();
    click(&mut app, Rect::new(add_col, add_row, 1, 1));
    assert!(app.change_dir_active && app.folder_browser.add_workspace);
    assert!(!app.workspaces.popup.open);
}

#[test]
fn filtering_switches_the_matching_workspace_and_empty_results_do_nothing() {
    let (_root, mut app) = fixture();
    crate::ui_workspaces::open(&mut app);
    crate::ui_workspace_sets::paste(&mut app, "no such workspace");
    draw(&mut app, 100, 24);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.workspaces.request.is_none() && !app.change_dir_active);
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.workspaces.popup.open && app.workspaces.popup.search.text.is_empty());
    crate::ui_workspace_sets::paste(&mut app, "INFRA");
    let mut event = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    event.kind = KeyEventKind::Release;
    crate::ui_workspaces::handle_key(&mut app, event);
    assert!(app.workspaces.request.is_none());
    event.kind = KeyEventKind::Repeat;
    crate::ui_workspaces::handle_key(&mut app, event);
    assert!(app.workspaces.request.is_none());
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(app.workspaces.request, Some(Request::Switch(2))));
}

#[test]
fn set_details_target_the_filtered_set_and_actions_never_delete_another_set() {
    let (_root, mut app) = fixture();
    crate::ui_workspace_sets::open(&mut app);
    crate::ui_workspace_sets::paste(&mut app, "λ");
    let buffer = draw(&mut app, 110, 26);
    assert!(!text(&buffer).contains("Update from open folders"));
    let area = app.workspaces.popup.area;
    let y = row_containing(&buffer, area, "Release λ");
    click(&mut app, Rect::new(area.x + 3, y, 1, 1));
    assert!(matches!(
        app.workspaces.popup.sets.mode,
        Some(Mode::Manage(12))
    ));
    key(&mut app, KeyCode::F(2), KeyModifiers::NONE);
    assert!(matches!(
        app.workspaces.popup.sets.mode,
        Some(Mode::Rename(12))
    ));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(
        app.workspaces.popup.sets.mode,
        Some(Mode::Manage(12))
    ));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(
        app.workspaces.popup.search.text, "λ",
        "return to the same filtered list"
    );
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    key(&mut app, KeyCode::End, KeyModifiers::NONE);
    key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
    assert!(matches!(app.workspaces.popup.sets.mode, Some(Mode::List)));
}

#[test]
fn saving_prefills_a_selected_name_and_supports_unicode_replacement_and_cancel() {
    let (_root, mut app) = fixture();
    crate::ui_workspace_sets::save(&mut app);
    assert_eq!(app.workspaces.popup.sets.text, "api + web + infra");
    crate::ui_workspace_sets::paste(&mut app, "Engineering λ🦀");
    assert_eq!(app.workspaces.popup.sets.text, "Engineering λ🦀");
    for (width, height) in [(4, 4), (20, 6), (48, 10), (120, 28)] {
        draw(&mut app, width, height);
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        draw(&mut app, width, height);
        key(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT);
    }
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(app.workspaces.popup.sets.mode, Some(Mode::List)));
    assert!(!app.workspace_sets_saving());
}

#[test]
fn tiny_lists_keep_actions_reachable_and_close_defaults_to_cancel() {
    let (_root, mut app) = fixture();
    for (width, height) in [(4, 4), (20, 6), (48, 12)] {
        crate::ui_workspaces::open(&mut app);
        key(&mut app, KeyCode::End, KeyModifiers::NONE);
        draw(&mut app, width, height);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(
            app.workspaces.popup.sets.mode,
            Some(Mode::Save(None))
        ));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        crate::ui_workspaces::close(&mut app);
        assert!(app.workspaces.popup.close);
        draw(&mut app, width, height);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.workspaces.request.is_none() && !app.workspaces.popup.close);
    }
}

#[test]
fn scrolled_set_rows_open_the_correct_details() {
    let (_root, mut app) = fixture();
    let template = app.workspaces.sets.catalog.sets[0].clone();
    app.workspaces.sets.catalog.sets = (0..24)
        .map(|id| SavedSet {
            id,
            name: format!("Set {id:02}"),
            ..template.clone()
        })
        .collect();
    crate::ui_workspace_sets::open(&mut app);
    for _ in 0..19 {
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    }
    let buffer = draw(&mut app, 100, 15);
    let area = app.workspaces.popup.area;
    let y = row_containing(&buffer, area, "Set 19");
    click(&mut app, Rect::new(area.right() - 3, y, 1, 1));
    assert!(matches!(
        app.workspaces.popup.sets.mode,
        Some(Mode::Manage(19))
    ));
}

#[test]
fn saved_sets_preview_each_directory_before_opening_and_mark_the_starting_folder() {
    let (_root, mut app) = fixture();
    app.workspaces.sets.catalog.sets[0].active = 1;
    crate::ui_workspace_sets::open(&mut app);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(
        app.workspaces.popup.sets.mode,
        Some(Mode::Manage(10))
    ));
    assert!(
        app.workspaces.request.is_none(),
        "inspection must not switch workspaces"
    );
    let buffer = draw(&mut app, 110, 24);
    let area = app.workspaces.popup.area;
    let api = row_containing(&buffer, area, &format!("{}api", std::path::MAIN_SEPARATOR));
    let web = row_containing(&buffer, area, &format!("{}web", std::path::MAIN_SEPARATOR));
    let infra = row_containing(
        &buffer,
        area,
        &format!("{}infra", std::path::MAIN_SEPARATOR),
    );
    assert_eq!(web, api + 1);
    assert_eq!(infra, web + 1);
    assert_eq!(row_containing(&buffer, area, "Start folder"), web);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(app.workspaces.request, Some(Request::OpenSet(10))));
    assert!(!app.workspaces.popup.open);
}

#[test]
fn duplicate_workspace_names_are_disambiguated_and_path_rows_switch_the_correct_folder() {
    let (root, mut app) = fixture();
    app.workspaces.parked[0].app.explorer.root = root.path().join("frontend").join("api");
    crate::ui_workspaces::open(&mut app);
    let buffer = draw(&mut app, 110, 24);
    let area = app.workspaces.popup.area;
    let y = row_containing(&buffer, area, "frontend");
    click(&mut app, Rect::new(area.x + 5, y, 1, 1));
    assert!(matches!(app.workspaces.request, Some(Request::Switch(1))));
}

#[test]
fn large_set_contents_scroll_without_moving_action_focus_and_survive_resizes() {
    let (root, mut app) = fixture();
    app.workspaces.sets.catalog.sets[0].roots = (0..24)
        .map(|index| {
            Location::local(
                &root.path().join(format!("service-{index:02} λ🦀")),
                &crate::portable::base_dir(),
            )
        })
        .collect();
    app.workspaces.sets.catalog.sets[0].active = 23;
    crate::ui_workspace_sets::open(&mut app);
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    let buffer = draw(&mut app, 100, 20);
    assert!(text(&buffer).contains("service-00"));
    for _ in 0..6 {
        key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        draw(&mut app, 100, 20);
    }
    let buffer = draw(&mut app, 100, 20);
    let area = app.workspaces.popup.area;
    assert!(text(&buffer).contains("service-23"));
    assert_eq!(
        row_containing(&buffer, area, "Start folder"),
        row_containing(&buffer, area, "service-23")
    );
    let before = row_containing(&buffer, area, "service-23");
    crate::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: area.x + 4,
            row: before,
            modifiers: KeyModifiers::NONE,
        },
    );
    assert_eq!(app.workspaces.popup.selected, 0);
    for (width, height) in [(4, 4), (12, 8), (24, 12), (40, 13), (110, 32)] {
        draw(&mut app, width, height);
        assert!(app.workspaces.popup.area.right() <= width);
        assert!(app.workspaces.popup.area.bottom() <= height);
    }
    crate::ui_control_visibility_tests::snapshot(
        &draw(&mut app, 100, 20),
        "Workspace picker - many directories",
    );
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(app.workspaces.request, Some(Request::OpenSet(10))));
}

#[test]
fn disappearing_saved_set_has_only_a_back_action() {
    let (_root, mut app) = fixture();
    crate::ui_workspace_sets::open(&mut app);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    app.workspaces.sets.catalog.sets.remove(0);
    let buffer = draw(&mut app, 100, 20);
    assert!(text(&buffer).contains("Workspace is no longer available"));
    key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
    key(&mut app, KeyCode::F(2), KeyModifiers::NONE);
    assert!(matches!(
        app.workspaces.popup.sets.mode,
        Some(Mode::Manage(10))
    ));
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(app.workspaces.popup.sets.mode, Some(Mode::List)));
    assert!(app.workspaces.request.is_none());
}

#[test]
fn set_details_keep_back_reachable_in_short_terminals() {
    let (_root, mut app) = fixture();
    for height in 4..=12 {
        crate::ui_workspace_sets::open(&mut app);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        key(&mut app, KeyCode::End, KeyModifiers::NONE);
        let buffer = draw(&mut app, 48, height);
        assert!(
            text(&buffer).contains("‹ Saved workspaces"),
            "height {height}"
        );
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(app.workspaces.popup.sets.mode, Some(Mode::List)));
        assert!(app.workspaces.request.is_none());
    }
}

#[test]
fn short_set_previews_keep_directories_visible_before_secondary_commands() {
    let (_root, mut app) = fixture();
    for height in 5..=12 {
        crate::ui_workspace_sets::open(&mut app);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let buffer = draw(&mut app, 48, height);
        row_containing(
            &buffer,
            app.workspaces.popup.area,
            &format!("{}api", std::path::MAIN_SEPARATOR),
        );
        assert!(text(&buffer).contains("Open workspace"), "height {height}");
    }
}

#[test]
fn cancelling_set_actions_returns_to_the_origin_and_keeps_action_focus() {
    let (_root, mut app) = fixture();
    crate::ui_workspaces::open(&mut app);
    app.workspaces.popup.selected = 4; // Save workspace, after the three open folders.
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(
        app.workspaces.popup.sets.mode,
        Some(Mode::Save(None))
    ));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.workspaces.popup.open && app.workspaces.popup.sets.mode.is_none());
    assert_eq!(app.workspaces.popup.selected, 4);
    crate::ui_workspace_sets::open(&mut app);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    for _ in 0..3 {
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    }
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(
        app.workspaces.popup.sets.mode,
        Some(Mode::Delete(10))
    ));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(
        app.workspaces.popup.sets.mode,
        Some(Mode::Manage(10))
    ));
    assert_eq!(app.workspaces.popup.selected, 3);
}

#[test]
fn directory_preview_keeps_foreign_paths_and_escapes_control_characters() {
    use crate::workspace_sets::NativePath;
    let (_root, mut app) = fixture();
    app.workspaces.sets.catalog.sets[0].roots = vec![Location::Local {
        path: NativePath::Utf8("/other-system/project\nλ".into()),
        relative: false,
        windows: !cfg!(windows),
    }];
    crate::ui_workspace_sets::open(&mut app);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let buffer = draw(&mut app, 110, 24);
    assert!(text(&buffer).contains("/other-system/project\\nλ"));
}

#[test]
fn a_single_open_folder_has_an_explicit_close_button_and_separate_save_action() {
    let (_root, mut app) = fixture();
    app.workspaces.parked.clear();
    for (width, height) in [(4, 4), (24, 8), (48, 12), (110, 24)] {
        crate::ui_workspaces::open(&mut app);
        let buffer = draw(&mut app, width, height);
        assert!(!text(&buffer).contains("Close current folder"));
        assert!(!text(&buffer).contains("Quit"));
        key(&mut app, KeyCode::End, KeyModifiers::NONE);
        assert_eq!(app.workspaces.popup.selected, 2);
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.workspaces.popup.selected, 0);
        key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(
            app.workspaces.popup.sets.mode,
            Some(Mode::Save(None))
        ));
        assert!(app.workspaces.request.is_none());
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    }
    crate::ui_workspaces::dismiss(&mut app);
    app.execute_palette_command("workspace_close");
    assert!(app.workspaces.popup.open && app.workspaces.popup.close);
    assert!(app.workspaces.request.is_none());
    crate::ui_workspaces::open(&mut app);
    crate::ui_control_visibility_tests::snapshot(
        &draw(&mut app, 110, 24),
        "Workspace picker - single folder",
    );
}

#[test]
fn close_folder_visibility_uses_all_open_folders_and_names_the_current_target() {
    let (_root, mut app) = fixture();
    crate::ui_workspaces::open(&mut app);
    crate::ui_workspace_sets::paste(&mut app, "web");
    draw(&mut app, 110, 24);
    assert_eq!(app.workspaces.popup.close_hits.len(), 1);
    assert_eq!(app.workspaces.popup.close_hits[0].1, 1);
    crate::ui_workspaces::close(&mut app);
    let buffer = draw(&mut app, 110, 24);
    assert!(text(&buffer).contains("Close “api”?"));
    assert!(app.workspaces.popup.close);
    assert!(!text(&buffer).contains("Quit"));
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        app.workspaces.request.is_none(),
        "confirmation defaults to Cancel"
    );
}

#[test]
fn file_menu_and_palette_offer_safe_close_for_the_last_folder_including_recent_commands() {
    let (_root, mut app) = fixture();
    let parked = app.workspaces.parked.pop().unwrap();
    app.workspaces.parked.clear();
    app.palette.commands = App::build_command_list();
    app.palette.recent = vec!["workspace_close".into()];
    for query in ["", "close", "folder"] {
        app.palette.query = query.into();
        app.filter_palette_commands();
        assert!(
            app.palette
                .filtered
                .iter()
                .any(|&index| app.palette.commands[index].id == "workspace_close")
        );
    }
    assert!(
        app.menu_entries(0)
            .iter()
            .any(|entry| entry.command == "workspace_close")
    );
    app.workspaces.parked.push(parked);
    app.palette.query = "close current folder".into();
    app.filter_palette_commands();
    assert!(
        app.palette
            .filtered
            .iter()
            .any(|&index| app.palette.commands[index].id == "workspace_close")
    );
    assert!(
        app.menu_entries(0)
            .iter()
            .any(|entry| entry.command == "workspace_close"
                && entry.label == "Close Current Folder…")
    );
}

#[test]
fn stale_close_confirmation_cannot_close_a_missing_folder() {
    let (_root, mut app) = fixture();
    crate::ui_workspaces::close(&mut app);
    key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    app.workspaces.parked.clear();
    app.explorer.root = std::path::PathBuf::new();
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.workspaces.request.is_none());
    assert!(!app.workspaces.popup.close);
    crate::ui_workspaces::close(&mut app);
    assert!(!app.workspaces.popup.close);
}

#[test]
fn compact_dialog_buttons_keep_cancel_and_the_primary_action_readable() {
    let (_root, app) = fixture();
    for (label, verb) in [
        ("Save workspace", "Save"),
        ("Open workspace", "Open"),
        ("Update workspace", "Update"),
        ("Delete workspace", "Delete"),
        ("Close folder", "Close"),
    ] {
        let mut terminal = Terminal::new(TestBackend::new(20, 4)).unwrap();
        terminal
            .draw(|frame| {
                buttons(
                    frame,
                    &app,
                    Rect::new(1, 1, 18, 1),
                    label,
                    false,
                    true,
                    true,
                );
            })
            .unwrap();
        let shown = text(terminal.backend().buffer());
        assert!(shown.contains("[Cancel]"), "{label}: {shown}");
        assert!(shown.contains(&format!("[{verb}]")), "{label}: {shown}");
    }
}

#[test]
fn picker_uses_folder_and_workspace_names_consistently() {
    let (_root, mut app) = fixture();
    crate::ui_workspaces::open(&mut app);
    let shown = text(&draw(&mut app, 110, 24));
    for label in [
        "Open folders",
        "Saved workspaces",
        "Add folder",
        "Save workspace",
    ] {
        assert!(shown.contains(label), "missing {label}");
    }
    assert!(!shown.contains("sets"));
    crate::ui_workspace_sets::open(&mut app);
    let shown = text(&draw(&mut app, 110, 24));
    assert!(shown.contains("3 folders"));
    assert!(!shown.contains("sets"));
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let shown = text(&draw(&mut app, 110, 24));
    for label in [
        "Saved workspaces",
        "Open workspace",
        "Update folders",
        "Rename…",
        "Delete…",
    ] {
        assert!(shown.contains(label), "missing {label}");
    }
    assert!(!shown.contains("sets"));
    assert_eq!(crate::settings::SettingsScope::Workspace.label(), "Folder");
    let commands = App::build_command_list();
    for command in commands
        .iter()
        .filter(|command| command.id.starts_with("workspace_"))
    {
        assert!(!command.name.contains("Set"));
    }
}

#[test]
fn adding_a_workspace_names_the_action_in_the_folder_dialog() {
    let (_root, mut app) = fixture();
    crate::ui_workspaces::add(&mut app);
    let buffer = draw(&mut app, 110, 28);
    assert!(text(&buffer).contains("Add folder"));
    assert!(!text(&buffer).contains("Select folder"));
}

#[test]
fn picker_text_and_actions_remain_readable_across_workbench_themes() {
    let (_root, mut app) = fixture();
    app.tabs[0].editor = crate::editor::Editor::test_with_text(
        &"Background build output and agent messages fill the workbench with text.\n".repeat(60),
    );
    app.cursor_blinking = false;
    let mut failures = Vec::new();
    for theme in crate::ui_control_visibility_tests::audit_themes() {
        app.colors = theme.to_theme_colors();
        crate::ui_workspaces::dismiss(&mut app);
        let behind = draw(&mut app, 150, 36);
        let code = app.code_area.unwrap();
        let context = (code.x, code.y + 1);
        assert!(!behind[context].symbol().trim().is_empty());
        for mode in 0..6 {
            match mode {
                0 => crate::ui_workspaces::open(&mut app),
                1 => crate::ui_workspace_sets::open(&mut app),
                2 => crate::ui_workspace_sets::save(&mut app),
                3 => {
                    crate::ui_workspace_sets::open(&mut app);
                    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
                }
                4 => crate::ui_workspaces::close(&mut app),
                _ => {
                    crate::ui_workspace_sets::open(&mut app);
                    key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
                }
            }
            let buffer = draw(&mut app, 150, 36);
            crate::ui_control_visibility_tests::check_region(
                &buffer,
                app.workspaces.popup.area,
                &format!("{} workspace view {mode}", theme.name),
                &mut failures,
                &app.colors.ui,
            );
            assert!(!app.workspaces.popup.area.contains(context.into()));
            let before = &behind[context];
            let after = &buffer[context];
            assert_eq!(
                after.symbol(),
                before.symbol(),
                "the backdrop retains context"
            );
            assert!(
                crate::ui_colors::contrast_ratio(after.fg, after.bg).unwrap()
                    < crate::ui_colors::contrast_ratio(before.fg, before.bg).unwrap(),
                "{} workspace view {mode}: background text competes with the popup",
                theme.name,
            );
            app.rendering_animation_frame = true;
            let cached = draw(&mut app, 150, 36);
            app.rendering_animation_frame = false;
            assert_eq!(
                cached[context], buffer[context],
                "the backdrop must not accumulate"
            );
        }
        crate::ui_workspaces::dismiss(&mut app);
        assert_eq!(draw(&mut app, 150, 36)[context], behind[context]);
    }
    assert!(
        failures.is_empty(),
        "{}",
        failures
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn workspace_picker_rendered_previews() {
    let (_root, mut app) = fixture();
    app.tabs[0].editor = crate::editor::Editor::test_with_text(
        &"Build output and agent chat: working on the project, checking files, running commands and reporting results.\n".repeat(60),
    );
    app.cursor_blinking = false;
    app.workspaces.parked[0].app.workspace_activity.unread = 2;
    app.workspaces.parked[0].app.workspace_activity.last = "Agent finished: tests pass".into();
    crate::ui_workspaces::open(&mut app);
    app.workspaces.popup.selected = 1;
    for (name, width, height) in [
        ("Workspace picker - open", 150, 36),
        ("Workspace picker - narrow", 48, 16),
    ] {
        crate::ui_control_visibility_tests::snapshot(&draw(&mut app, width, height), name);
    }
    crate::ui_workspace_sets::open(&mut app);
    crate::ui_control_visibility_tests::snapshot(
        &draw(&mut app, 110, 24),
        "Workspace picker - saved sets",
    );
    key(&mut app, KeyCode::Menu, KeyModifiers::NONE);
    crate::ui_control_visibility_tests::snapshot(
        &draw(&mut app, 110, 24),
        "Workspace picker - set contents",
    );
    crate::ui_control_visibility_tests::snapshot(
        &draw(&mut app, 40, 13),
        "Workspace picker - set contents narrow",
    );
    crate::ui_workspace_sets::save(&mut app);
    crate::ui_control_visibility_tests::snapshot(
        &draw(&mut app, 110, 24),
        "Workspace picker - save",
    );
    crate::ui_workspace_sets::open(&mut app);
    key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
    crate::ui_control_visibility_tests::snapshot(
        &draw(&mut app, 110, 24),
        "Workspace picker - delete",
    );
    assert!(matches!(
        app.workspaces.popup.sets.mode,
        Some(Mode::Delete(_))
    ));
    for theme in ["monokai", "light", "high-contrast"] {
        app.colors = crate::builtin_themes::THEMES
            .iter()
            .find(|entry| entry.key == theme)
            .unwrap()
            .theme()
            .to_theme_colors();
        crate::ui_workspaces::open(&mut app);
        crate::ui_control_visibility_tests::snapshot(
            &draw(&mut app, 150, 36),
            &format!("Workspace picker - busy {theme}"),
        );
    }
}

#[test]
fn workspace_manager_is_centered_and_keeps_actions_separate_from_folders() {
    for (width, height) in [(120, 32), (200, 70), (80, 20), (40, 12), (12, 8), (4, 4)] {
        let (_root, mut app) = fixture();
        crate::ui_workspaces::open(&mut app);
        let buffer = draw(&mut app, width, height);
        let area = app.workspaces.popup.area;
        assert!((area.x as i32 - (width - area.right()) as i32).abs() <= 1);
        assert!((area.y as i32 - (height - area.bottom()) as i32).abs() <= 1);
        if width >= 80 && height >= 20 {
            assert!(text(&buffer).contains("Engineering"));
            assert!(text(&buffer).contains("Saved · 3 folders"));
            let actions_y = row_containing(&buffer, area, "+ Add folder");
            assert_eq!(app.workspaces.popup.close_hits.len(), 3);
            for (close, _) in &app.workspaces.popup.close_hits {
                assert!(close.y < actions_y);
            }
        }
    }
}

#[test]
fn workspace_manager_clarifies_active_modified_and_unsaved_collections() {
    let (_root, mut app) = fixture();
    app.workspaces.sets.active = Some(12);
    crate::ui_workspace_sets::open(&mut app);
    let shown = text(&draw(&mut app, 120, 32));
    assert!(shown.contains("Release λ"));
    assert_eq!(shown.matches("Active").count(), 1);
    app.workspaces.parked.pop();
    let shown = text(&draw(&mut app, 120, 32));
    assert!(shown.contains("Unsaved changes"));
    assert!(shown.contains("Active · Unsaved"));
    app.workspaces.sets.catalog.sets.retain(|set| set.id != 12);
    let shown = text(&draw(&mut app, 120, 32));
    assert!(shown.contains("Unsaved workspace"));
    assert!(!shown.contains("Active  3 folders"));
    app.workspaces.sets.catalog.sets.clear();
    crate::ui_workspaces::open(&mut app);
    let shown = text(&draw(&mut app, 120, 32));
    assert!(shown.contains("Unsaved workspace"));
    assert!(shown.contains("Not saved"));
}

#[test]
fn each_folder_close_button_confirms_its_own_target_and_cancel_keeps_the_filter() {
    let (_root, mut app) = fixture();
    crate::ui_workspaces::open(&mut app);
    crate::ui_workspace_sets::paste(&mut app, "web");
    draw(&mut app, 120, 32);
    let close = app
        .workspaces
        .popup
        .close_hits
        .iter()
        .find(|(_, id)| *id == 1)
        .unwrap()
        .0;
    click(&mut app, close);
    assert_eq!(app.workspaces.active, 0);
    assert_eq!(app.workspaces.popup.close_target, Some(1));
    let shown = text(&draw(&mut app, 120, 32));
    assert!(shown.contains("Close “web”?"));
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(!app.workspaces.popup.close);
    assert_eq!(app.workspaces.popup.search.text, "web");
    assert!(app.workspaces.request.is_none());
    draw(&mut app, 120, 32);
    let close = app.workspaces.popup.close_hits[0].0;
    click(&mut app, close);
    key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(
        app.workspaces.request,
        Some(Request::CloseFolder(1))
    ));
}

#[test]
fn stale_folder_close_target_cannot_close_another_folder() {
    let (_root, mut app) = fixture();
    crate::ui_workspaces::close_folder(&mut app, 1);
    app.workspaces.parked.remove(0);
    key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.workspaces.request.is_none());
    assert!(!app.workspaces.popup.close);
}
