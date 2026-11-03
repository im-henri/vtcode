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
    let roots = vec![Location::local(
        &app.explorer.root,
        &crate::portable::base_dir(),
    )];
    app.workspaces.sets.loaded = true;
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
    let sets = crate::ui_chrome::saved_workspace_sets_area(&app).unwrap();
    let (directory, add) = crate::ui_chrome::workspace_controls(&app).unwrap();
    click(&mut app, directory);
    draw(&mut app, 120, 28);
    let saved_tab = app.workspaces.popup.tabs[1];
    click(&mut app, saved_tab);
    assert!(matches!(app.workspaces.popup.sets.mode, Some(Mode::List)));
    draw(&mut app, 120, 28);
    click(&mut app, directory);
    assert!(app.workspaces.popup.open && app.workspaces.popup.sets.mode.is_none());
    click(&mut app, sets);
    assert!(matches!(app.workspaces.popup.sets.mode, Some(Mode::List)));
    click(&mut app, sets);
    assert!(!app.workspaces.popup.open);
    click(&mut app, directory);
    click(&mut app, add);
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
fn per_set_menu_targets_the_filtered_set_and_actions_never_delete_another_set() {
    let (_root, mut app) = fixture();
    crate::ui_workspace_sets::open(&mut app);
    crate::ui_workspace_sets::paste(&mut app, "λ");
    let buffer = draw(&mut app, 110, 26);
    assert!(!text(&buffer).contains("Update with current workspaces"));
    let area = app.workspaces.popup.area;
    let y = row_containing(&buffer, area, "Release λ");
    click(&mut app, Rect::new(area.right() - 3, y, 1, 1));
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
        assert!(app.workspaces.popup.close);
        draw(&mut app, width, height);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.workspaces.request.is_none() && !app.workspaces.popup.close);
    }
}

#[test]
fn scrolled_sets_keep_more_buttons_bound_to_the_visible_row() {
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
fn adding_a_workspace_names_the_action_in_the_folder_dialog() {
    let (_root, mut app) = fixture();
    crate::ui_workspaces::add(&mut app);
    let buffer = draw(&mut app, 110, 28);
    assert!(text(&buffer).contains("Add workspace"));
    assert!(!text(&buffer).contains("Select folder"));
}

#[test]
fn picker_text_and_actions_remain_readable_across_workbench_themes() {
    let (_root, mut app) = fixture();
    let mut failures = Vec::new();
    for theme in crate::ui_control_visibility_tests::audit_themes() {
        app.colors = theme.to_theme_colors();
        for mode in 0..3 {
            match mode {
                0 => crate::ui_workspaces::open(&mut app),
                1 => crate::ui_workspace_sets::open(&mut app),
                _ => crate::ui_workspace_sets::save(&mut app),
            }
            let buffer = draw(&mut app, 110, 24);
            crate::ui_control_visibility_tests::check_region(
                &buffer,
                app.workspaces.popup.area,
                &format!("{} workspace view {mode}", theme.name),
                &mut failures,
                &app.colors.ui,
            );
        }
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
    app.workspaces.parked[0].app.workspace_activity.unread = 2;
    app.workspaces.parked[0].app.workspace_activity.last = "Agent finished: tests pass".into();
    crate::ui_workspaces::open(&mut app);
    app.workspaces.popup.selected = 1;
    for (name, width, height) in [
        ("Workspace picker - open", 110, 24),
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
        "Workspace picker - set actions",
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
}
