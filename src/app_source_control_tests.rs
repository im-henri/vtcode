use super::*;
use crate::app::{ActivePane, SidebarView};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect};
use std::path::Path;
use std::time::Duration;

fn grouped_changes() -> Vec<GitFileChange> {
    [("staged.rs", true, 'M'), ("tracked.rs", false, 'M'), ("new.rs", false, '?')]
        .into_iter().map(|(path, staged, status)| GitFileChange { path: path.into(), staged, status, original_path: None }).collect()
}

#[test]
fn untracked_filter_excludes_hidden_files_from_selection_navigation_and_actions() {
    let mut app = App::test_new();
    app.source_control_repository_state = crate::app::SourceControlRepositoryState::Available;
    app.source_control_files = grouped_changes();
    app.select_all_source_control_files();
    assert_eq!(selection(&app).len(), 3);
    app.set_source_control_visibility(Visibility { untracked: false });
    app.select_source_control_file(0, false, false);
    app.select_source_control_file(1, false, true);
    app.select_source_control_file(2, false, true);
    assert_eq!(selection(&app), vec![("staged.rs".into(), true), ("tracked.rs".into(), false)]);
    app.select_all_source_control_files();
    assert_eq!(selection(&app).len(), 2);
    assert!(app.source_control_group_changes(Group::Untracked).is_empty());
    app.source_control_selected = 0;
    crate::ui_source_control::handle_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(app.source_control_selected, 1);
    crate::ui_source_control::handle_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(app.source_control_selected, 0);
    app.update_source_control_files(vec![grouped_changes().pop().unwrap()]);
    for code in [KeyCode::Enter, KeyCode::Char(' '), KeyCode::Delete, KeyCode::End] {
        crate::ui_source_control::handle_key(&mut app, code, KeyModifiers::NONE);
    }
    assert!(selection(&app).is_empty());
    assert!(!app.bg_git_action_running && !app.sc_discard_confirm_active);
    assert!(app.diff_tabs.owner.is_none());
}

#[test]
fn status_refresh_prunes_selection_when_a_file_moves_into_a_hidden_group() {
    let mut app = App::test_new();
    app.update_source_control_files(grouped_changes());
    app.set_source_control_visibility(Visibility { untracked: false });
    app.select_source_control_file(0, false, false);
    let mut files = grouped_changes();
    files[0].staged = false;
    files[0].status = '?';
    files.sort_by_key(Group::of);
    app.update_source_control_files(files);
    assert!(selection(&app).is_empty());
    assert_eq!(app.source_control_files[app.source_control_selected].path, "tracked.rs");
    app.set_source_control_visibility(Visibility::default());
    assert!(!app.source_control_file_selected(1), "showing a group must not revive its old selection");
}

#[test]
fn filtered_keyboard_navigation_keeps_selected_rows_in_view_across_collapsed_groups() {
    let mut app = App::test_new();
    app.source_control_repository_state = crate::app::SourceControlRepositoryState::Available;
    app.sc_graph_collapsed = true;
    app.explorer_area = Some(Rect::new(0, 0, 40, 16));
    app.source_control_files = Group::ALL.into_iter().flat_map(|group| (0..30).map(move |index| GitFileChange {
        path: format!("{group:?}-{index}.txt"), staged: group == Group::Staged,
        status: if group == Group::Merge { '!' } else if group == Group::Untracked { '?' } else { 'M' }, original_path: None,
    })).collect();
    app.set_source_control_visibility(Visibility { untracked: false });
    app.sc_changes_collapsed = true;
    let (height, _) = crate::ui_source_control::source_control_split_heights(16, true, 10);
    let visible = crate::ui_source_control::source_control_file_list_visible_rows(height);
    for code in [KeyCode::End, KeyCode::PageUp, KeyCode::PageUp, KeyCode::Up, KeyCode::Home, KeyCode::PageDown, KeyCode::Down] {
        crate::ui_source_control::handle_key(&mut app, code, KeyModifiers::NONE);
        let layout = app.source_control_layout();
        assert!(layout.rows(app.source_control_scroll).take(visible).any(|row| row == crate::source_control_rows::Row::File(app.source_control_selected)), "{code:?}");
        assert_ne!(Group::of(&app.source_control_files[app.source_control_selected]), Group::Untracked);
    }
    assert!(!app.sc_changes_collapsed);
}

#[test]
fn filter_menu_only_offers_untracked_files_and_supports_keyboard_toggle() {
    let mut app = App::test_new();
    app.source_control_files = grouped_changes();
    app.open_source_control_filter_menu(4, 2);
    assert_eq!(app.context_menu_items.len(), 1);
    assert_eq!(app.context_menu_items[0].label, "✓ Show untracked files");
    assert!(!app.context_menu_items[0].separator_after);
    crate::input_pickers::handle_context_menu_key(&mut app, KeyCode::Char(' '));
    assert!(!app.sc_visibility.untracked);
    assert!(app.source_control_filter_menu_open());
    assert_eq!(app.context_menu_selected, 0);
    assert!(!app.context_menu_items[0].label.starts_with('✓'));
    crate::input_pickers::handle_context_menu_key(&mut app, KeyCode::Down);
    crate::input_pickers::handle_context_menu_key(&mut app, KeyCode::Enter);
    assert_eq!(app.sc_visibility, Visibility::default());
    crate::input_pickers::handle_context_menu_key(&mut app, KeyCode::Esc);
    assert!(!app.context_menu_active);
    for command in ["git_show_staged", "git_show_unstaged", "git_changes_show_all"] {
        assert!(!app.execute_git_command(command));
    }
}

#[test]
fn untracked_filter_round_trips_with_workspace_session_and_defaults_to_visible() {
    let mut app = App::test_new();
    app.sc_visibility = Visibility { untracked: false };
    let value = serde_json::to_value(app.build_session()).unwrap();
    let session = serde_json::from_value(value.clone()).unwrap();
    let mut restored = App::test_new();
    restored.restore_session(&session);
    assert_eq!(restored.sc_visibility, app.sc_visibility);
    let mut missing_filter = value;
    missing_filter.as_object_mut().unwrap().remove("scVisibility");
    let missing_filter: crate::session::Session = serde_json::from_value(missing_filter).unwrap();
    assert_eq!(missing_filter.sc_visibility, Visibility::default());
}

#[test]
fn actions_dropdown_is_clickable_in_narrow_sidebars_and_can_restore_filtered_files() {
    for width in [15, 24, 34, 80] {
        for right in [false, true] {
            let mut app = App::test_new();
            app.sidebar_view = SidebarView::SourceControl;
            app.source_control_repository_state = crate::app::SourceControlRepositoryState::Available;
            app.sidebar_right = right;
            app.sc_graph_collapsed = true;
            app.source_control_files = vec![grouped_changes().pop().unwrap()];
            app.set_source_control_visibility(Visibility { untracked: false });
            let area = Rect::new(5, 1, width, 20);
            app.explorer_area = Some(area);
            let mut terminal = Terminal::new(TestBackend::new(110, 28)).unwrap();
            terminal.draw(|frame| crate::ui_source_control::draw_source_control_sidebar(frame, &mut app, area)).unwrap();
            let button = app.source_control_actions_area.unwrap();
            mouse(&mut app, MouseButton::Left, button.x + 1, button.y, KeyModifiers::NONE);
            assert!(app.source_control_action_menu_open(), "width={width}, right={right}");
            terminal.draw(|frame| {
                crate::ui_source_control::draw_source_control_sidebar(frame, &mut app, area);
                crate::ui_dialogs::draw_context_menu(frame, &mut app);
            }).unwrap();
            crate::ui_control_visibility_tests::snapshot(terminal.backend().buffer(), &format!("Source control filter width {width} right {right}"));
            let inner = app.context_menu_inner.unwrap();
            let filter = app.context_menu_items.iter().position(|item| item.id == "git_show_untracked").unwrap();
            let row = filter + app.context_menu_items[..filter].iter().filter(|item| item.separator_after).count();
            mouse(&mut app, MouseButton::Left, inner.x + 2, inner.y + row as u16, KeyModifiers::NONE);
            assert_eq!(app.sc_visibility, Visibility::default());
            assert!(app.source_control_action_menu_open(), "checkbox keeps the actions menu open");
            app.context_menu_active = false;
            terminal.draw(|frame| crate::ui_source_control::draw_source_control_sidebar(frame, &mut app, area)).unwrap();
            crate::ui_control_visibility_tests::snapshot(terminal.backend().buffer(), &format!("Source control groups width {width} right {right}"));
        }
    }
}

#[test]
fn unstaged_group_actions_leave_untracked_files_untouched() {
    let (directory, mut app) = fixture();
    std::fs::write(directory.path().join("new.txt"), "keep\n").unwrap();
    refresh(&mut app);
    app.set_source_control_visibility(Visibility { untracked: false });
    draw(&mut app, false);
    let stage = app.source_control_stage_all_area.unwrap();
    mouse(&mut app, MouseButton::Left, stage.x + 1, stage.y, KeyModifiers::NONE);
    wait(&mut app);
    assert_eq!(index_text(directory.path(), "a.txt").as_deref(), Some("modified\n"));
    assert!(index_text(directory.path(), "new.txt").is_none());
    std::fs::write(directory.path().join("a.txt"), "discard this\n").unwrap();
    refresh(&mut app);
    draw(&mut app, false);
    let discard = app.source_control_discard_all_area.unwrap();
    mouse(&mut app, MouseButton::Left, discard.x + 1, discard.y, KeyModifiers::NONE);
    assert!(app.sc_discard_confirm_active);
    assert_eq!(app.sc_discard_request.as_ref().unwrap().changes.len(), 1);
    assert_eq!(std::fs::read_to_string(directory.path().join("a.txt")).unwrap(), "discard this\n");
    app.execute_source_control_discard_confirm(true);
    wait(&mut app);
    assert_eq!(std::fs::read_to_string(directory.path().join("a.txt")).unwrap(), "modified\n");
    assert_eq!(std::fs::read_to_string(directory.path().join("new.txt")).unwrap(), "keep\n");
}

#[test]
fn untracked_group_actions_only_target_untracked_files_and_confirm_discard() {
    let (directory, mut app) = fixture();
    std::fs::write(directory.path().join("new.txt"), "new\n").unwrap();
    refresh(&mut app);
    draw(&mut app, false);
    let discard = app.source_control_untracked_discard_area.unwrap();
    mouse(&mut app, MouseButton::Left, discard.x + 1, discard.y, KeyModifiers::NONE);
    let request = app.sc_discard_request.as_ref().unwrap();
    assert_eq!(request.changes.len(), 1);
    assert_eq!(request.changes[0].path, "new.txt");
    app.execute_source_control_discard_confirm(false);
    assert!(directory.path().join("new.txt").exists());
    draw(&mut app, false);
    let stage = app.source_control_untracked_stage_area.unwrap();
    mouse(&mut app, MouseButton::Left, stage.x + 1, stage.y, KeyModifiers::NONE);
    wait(&mut app);
    assert_eq!(index_text(directory.path(), "new.txt").as_deref(), Some("new\n"));
    assert_eq!(index_text(directory.path(), "a.txt").as_deref(), Some("base\n"));
}

#[test]
fn selection_distinguishes_staged_and_unstaged_paths_across_refreshes() {
    let change = |path: &str, staged| GitFileChange {
        path: path.into(), staged, status: 'M', original_path: None,
    };
    let mut app = App::test_new();
    app.update_source_control_files(vec![change("same.rs", true), change("same.rs", false)]);
    app.select_source_control_file(0, false, false);
    app.select_source_control_file(1, true, false);
    assert_eq!(selection(&app), vec![("same.rs".into(), true), ("same.rs".into(), false)]);
    app.select_source_control_file(0, true, false);
    assert_eq!(selection(&app), vec![("same.rs".into(), false)]);
    app.update_source_control_files(vec![change("same.rs", true), change("another.rs", false)]);
    assert!(selection(&app).is_empty());
    app.sc_staged_collapsed = true;
    app.select_all_source_control_files();
    assert_eq!(selection(&app), vec![("another.rs".into(), false)]);
    app.update_source_control_files(vec![change("another.rs", false), change("same.rs", false)]);
    assert_eq!(selection(&app), vec![("another.rs".into(), false)]);
}

#[test]
#[ignore = "manual performance measurement"]
fn benchmark_source_control_selection_lookup() {
    use std::{hint::black_box, time::Instant};
    let mut app = App::test_new();
    app.source_control_files = (0..50_000).map(|index| GitFileChange {
        path: format!("src/long/module/path/file-{index}.rs"),
        staged: index < 25_000, status: 'M', original_path: None,
    }).collect();
    app.select_all_source_control_files();
    let old: FxHashSet<_> = app.source_control_files.iter().map(key).collect();
    let mut before = Vec::new();
    let mut after = Vec::new();
    for run in 0..25 {
        for reference in if run % 2 == 0 { [true, false] } else { [false, true] } {
            let start = Instant::now();
            let count = if reference {
                app.source_control_files.iter().filter(|file| black_box(&old).contains(&key(file))).count()
            } else {
                (0..app.source_control_files.len()).filter(|&index| black_box(&app).source_control_file_selected(index)).count()
            };
            assert_eq!(count, 50_000);
            if run >= 4 { (if reference { &mut before } else { &mut after }).push(start.elapsed()); }
        }
    }
    before.sort(); after.sort();
    eprintln!("source_control_selection files=50000 before={:?} after={:?}", before[10], after[10]);
}

fn fixture() -> (tempfile::TempDir, App) {
    let directory = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(directory.path()).unwrap();
    repo.config()
        .unwrap()
        .set_bool("core.autocrlf", false)
        .unwrap();
    let mut index = repo.index().unwrap();
    for path in ["a.txt", "b.txt", "c.txt", ".gitignore"] {
        std::fs::write(
            directory.path().join(path),
            if path == ".gitignore" {
                "ignored.txt\n"
            } else {
                "base\n"
            },
        )
        .unwrap();
        index.add_path(Path::new(path)).unwrap();
    }
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = git2::Signature::now("Test", "test@example.com").unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
        .unwrap();
    for path in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(directory.path().join(path), "modified\n").unwrap();
    }
    let mut app = App::new_deferred(directory.path().to_path_buf());
    app.show_explorer = true;
    app.sidebar_view = SidebarView::SourceControl;
    app.active_pane = ActivePane::Explorer;
    app.sc_graph_collapsed = true;
    app.refresh_source_control_async();
    wait(&mut app);
    (directory, app)
}

fn wait(app: &mut App) {
    app.wait_for_background(Duration::from_secs(10), |app| {
        !app.bg_git_action_running && !app.bg_git_status_running
    });
}

fn refresh(app: &mut App) {
    app.refresh_source_control_async();
    wait(app);
}

fn file(app: &App, path: &str, staged: bool) -> usize {
    app.source_control_files
        .iter()
        .position(|file| file.path == path && file.staged == staged)
        .unwrap()
}

fn selection(app: &App) -> Vec<(String, bool)> {
    app.selected_source_control_changes()
        .iter()
        .map(key)
        .collect()
}

fn draw(app: &mut App, modal: bool) -> Buffer {
    let area = Rect::new(0, 0, 44, 24);
    app.explorer_area = Some(area);
    let mut terminal = Terminal::new(TestBackend::new(110, 28)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui_source_control::draw_source_control_sidebar(frame, app, area);
            if modal {
                crate::ui_modal::draw_active(frame, app);
            }
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

fn mouse(app: &mut App, button: MouseButton, col: u16, row: u16, modifiers: KeyModifiers) {
    for kind in [MouseEventKind::Down(button), MouseEventKind::Up(button)] {
        let event = MouseEvent { kind, column: col, row, modifiers };
        // Match the event loop: generic dropdowns own mouse input while open.
        if app.context_menu_active {
            crate::mouse_overlay::handle_context_menu_mouse(app, event);
        } else {
            crate::mouse::handle_mouse(app, event);
        }
    }
}

fn click_file(app: &mut App, path: &str, button: MouseButton, modifiers: KeyModifiers) {
    let buffer = draw(app, false);
    let row = (0..24)
        .find(|&row| {
            (0..44)
                .map(|col| buffer[(col, row)].symbol())
                .collect::<String>()
                .contains(path)
        })
        .expect("rendered file row");
    mouse(app, button, 10, row, modifiers);
}

fn keyboard(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, modifiers));
}

fn context_action(app: &mut App, name: &str) {
    let index = app
        .source_control_context_menu_items()
        .iter()
        .position(|&label| label == name)
        .unwrap();
    app.execute_source_control_context(index);
}

fn index_text(root: &Path, path: &str) -> Option<String> {
    let repo = git2::Repository::open(root).unwrap();
    let entry = repo.index().unwrap().get_path(Path::new(path), 0)?;
    Some(String::from_utf8(repo.find_blob(entry.id).unwrap().content().to_vec()).unwrap())
}

#[test]
fn ctrl_and_shift_click_select_files_and_context_stages_the_whole_selection() {
    let (directory, mut app) = fixture();
    click_file(&mut app, "b.txt", MouseButton::Left, KeyModifiers::CONTROL);
    assert_eq!(
        selection(&app),
        vec![("a.txt".into(), false), ("b.txt".into(), false)]
    );
    click_file(&mut app, "a.txt", MouseButton::Left, KeyModifiers::CONTROL);
    assert_eq!(selection(&app), vec![("b.txt".into(), false)]);
    click_file(&mut app, "c.txt", MouseButton::Left, KeyModifiers::SHIFT);
    assert_eq!(
        selection(&app).len(),
        3,
        "shift uses the last toggled file as its anchor"
    );
    click_file(&mut app, "a.txt", MouseButton::Left, KeyModifiers::CONTROL);
    assert_eq!(
        selection(&app),
        vec![("b.txt".into(), false), ("c.txt".into(), false)]
    );
    assert!(app.tabs.is_empty(), "selection gestures do not open diffs");
    let buffer = draw(&mut app, false);
    crate::ui_control_visibility_tests::snapshot(&buffer, "Source control - multiple selected");
    click_file(&mut app, "c.txt", MouseButton::Right, KeyModifiers::NONE);
    assert_eq!(
        selection(&app).len(),
        2,
        "right-click preserves the selection"
    );
    context_action(&mut app, "Stage Selected");
    wait(&mut app);
    assert_eq!(
        index_text(directory.path(), "a.txt").as_deref(),
        Some("base\n")
    );
    for path in ["b.txt", "c.txt"] {
        assert_eq!(
            index_text(directory.path(), path).as_deref(),
            Some("modified\n")
        );
    }
    assert!(
        selection(&app).is_empty(),
        "refresh never selects unrelated rows by index"
    );
    app.select_source_control_file(file(&app, "b.txt", true), false, false);
    app.select_source_control_file(file(&app, "c.txt", true), true, false);
    click_file(&mut app, "b.txt", MouseButton::Right, KeyModifiers::NONE);
    context_action(&mut app, "Unstage Selected");
    wait(&mut app);
    assert!(app.source_control_files.iter().all(|file| !file.staged));
}

#[test]
fn keyboard_selection_and_refresh_use_file_identity_and_staging_section() {
    let (_directory, mut app) = fixture();
    keyboard(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
    assert_eq!(selection(&app).len(), 2);
    keyboard(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert_eq!(selection(&app).len(), 3);
    app.execute_palette_command("source_control");
    app.execute_palette_command("source_control");
    wait(&mut app);
    assert_eq!(
        app.source_control_selected, 1,
        "reopening preserves keyboard focus with the selection"
    );
    assert_eq!(selection(&app).len(), 3);
    keyboard(&mut app, KeyCode::Char(' '), KeyModifiers::CONTROL);
    assert_eq!(
        selection(&app),
        vec![("a.txt".into(), false), ("c.txt".into(), false)]
    );
    let mut files = app.source_control_files.clone();
    files.insert(
        0,
        GitFileChange {
            path: "a.txt".into(),
            staged: true,
            status: 'M',
            original_path: None,
        },
    );
    app.update_source_control_files(files);
    assert_eq!(
        selection(&app),
        vec![("a.txt".into(), false), ("c.txt".into(), false)]
    );
    let files = app
        .source_control_files
        .iter()
        .filter(|file| file.path != "a.txt")
        .cloned()
        .collect();
    app.update_source_control_files(files);
    assert_eq!(selection(&app), vec![("c.txt".into(), false)]);
    click_file(&mut app, "b.txt", MouseButton::Right, KeyModifiers::NONE);
    assert_eq!(
        selection(&app),
        vec![("b.txt".into(), false)],
        "right-click outside selects just that file"
    );
}

#[test]
fn discard_all_requires_confirmation_preserves_staging_and_only_discards_approved_files() {
    let (directory, mut app) = fixture();
    let repo = git2::Repository::open(directory.path()).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("a.txt")).unwrap();
    std::fs::write(directory.path().join("staged-new.txt"), "staged new\n").unwrap();
    index.add_path(Path::new("staged-new.txt")).unwrap();
    index.write().unwrap();
    std::fs::write(directory.path().join("a.txt"), "unstaged addition\n").unwrap();
    std::fs::remove_file(directory.path().join("c.txt")).unwrap();
    std::fs::write(directory.path().join("new.txt"), "untracked\n").unwrap();
    std::fs::write(directory.path().join("ignored.txt"), "ignored\n").unwrap();
    refresh(&mut app);
    draw(&mut app, false);
    let button = app
        .source_control_discard_all_area
        .expect("Discard All button");
    mouse(
        &mut app,
        MouseButton::Left,
        button.x + 1,
        button.y,
        KeyModifiers::NONE,
    );
    assert!(app.sc_discard_confirm_active);
    assert!(!app.bg_git_action_running);
    let buffer = draw(&mut app, true);
    crate::ui_control_visibility_tests::snapshot(
        &buffer,
        "Source control - discard all confirmation",
    );
    let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
    assert!(text.contains("Discard Changes"));
    assert!(text.contains("3 files"));
    assert!(!text.contains("New files will be permanently deleted."));
    assert_eq!(app.modal.focus, 1, "Cancel is the default");
    keyboard(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(!app.sc_discard_confirm_active && !app.bg_git_action_running);
    assert_eq!(
        std::fs::read_to_string(directory.path().join("a.txt")).unwrap(),
        "unstaged addition\n"
    );
    assert!(directory.path().join("new.txt").exists());

    app.request_source_control_discard_all();
    std::fs::write(
        directory.path().join("created-after-confirmation.txt"),
        "keep\n",
    )
    .unwrap();
    refresh(&mut app);
    draw(&mut app, true);
    let confirm = app.modal.buttons[0].0;
    mouse(
        &mut app,
        MouseButton::Left,
        confirm.x + 1,
        confirm.y,
        KeyModifiers::NONE,
    );
    wait(&mut app);
    assert!(!app.sc_discard_confirm_active);
    assert_eq!(
        std::fs::read_to_string(directory.path().join("a.txt")).unwrap(),
        "modified\n"
    );
    assert_eq!(
        index_text(directory.path(), "a.txt").as_deref(),
        Some("modified\n")
    );
    assert_eq!(
        std::fs::read_to_string(directory.path().join("b.txt")).unwrap(),
        "base\n"
    );
    assert_eq!(
        std::fs::read_to_string(directory.path().join("c.txt")).unwrap(),
        "base\n"
    );
    assert!(!directory.path().join("new.txt").exists());
    for path in [
        "ignored.txt",
        "created-after-confirmation.txt",
        "staged-new.txt",
    ] {
        assert!(directory.path().join(path).exists(), "kept {path}");
    }
}

#[test]
fn selected_discard_can_cancel_and_leaves_unselected_files_alone() {
    let (directory, mut app) = fixture();
    app.select_source_control_file(1, false, false);
    app.select_source_control_file(2, true, false);
    click_file(&mut app, "b.txt", MouseButton::Right, KeyModifiers::NONE);
    context_action(&mut app, "Discard Selected");
    assert_eq!(app.sc_discard_request.as_ref().unwrap().changes.len(), 2);
    keyboard(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.sc_discard_request.is_none());
    assert_eq!(
        std::fs::read_to_string(directory.path().join("b.txt")).unwrap(),
        "modified\n"
    );
    keyboard(&mut app, KeyCode::Delete, KeyModifiers::NONE);
    assert!(app.sc_discard_confirm_active);
    app.execute_source_control_discard_confirm(true);
    wait(&mut app);
    assert_eq!(
        std::fs::read_to_string(directory.path().join("a.txt")).unwrap(),
        "modified\n"
    );
    for path in ["b.txt", "c.txt"] {
        assert_eq!(
            std::fs::read_to_string(directory.path().join(path)).unwrap(),
            "base\n"
        );
    }
}

#[test]
fn discard_request_cannot_follow_a_workspace_switch_or_delete_a_directory() {
    let (directory, mut app) = fixture();
    app.request_source_control_discard_all();
    let other = tempfile::tempdir().unwrap();
    app.explorer.root = other.path().to_path_buf();
    app.execute_source_control_discard_confirm(true);
    assert!(!app.bg_git_action_running);
    assert_eq!(
        std::fs::read_to_string(directory.path().join("a.txt")).unwrap(),
        "modified\n"
    );
    app.explorer.root = directory.path().to_path_buf();
    let nested = directory.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    std::fs::write(nested.join("keep.txt"), "keep\n").unwrap();
    app.git_discard_changes(vec![GitFileChange {
        path: "nested".into(),
        original_path: None,
        status: '?',
        staged: false,
    }]);
    wait(&mut app);
    assert!(nested.join("keep.txt").exists());
    assert!(
        app.notification_history
            .iter()
            .any(|(message, _, _)| message.contains("Cannot discard directory"))
    );
}

#[test]
fn mixed_selection_offers_both_stage_actions_and_deselecting_everything_is_safe() {
    let (directory, mut app) = fixture();
    app.git_stage_file(file(&app, "a.txt", false));
    wait(&mut app);
    app.select_source_control_file(file(&app, "a.txt", true), false, false);
    app.select_source_control_file(file(&app, "b.txt", false), true, false);
    let target = app.source_control_files[file(&app, "b.txt", false)].clone();
    app.open_source_control_context_menu(1, 1, target);
    let items = app.source_control_context_menu_items();
    assert!(items.contains(&"Stage Selected") && items.contains(&"Unstage Selected"));
    context_action(&mut app, "Stage Selected");
    wait(&mut app);
    assert_eq!(
        index_text(directory.path(), "a.txt").as_deref(),
        Some("modified\n")
    );
    assert_eq!(
        index_text(directory.path(), "b.txt").as_deref(),
        Some("modified\n")
    );
    assert_eq!(
        index_text(directory.path(), "c.txt").as_deref(),
        Some("base\n")
    );
    app.select_source_control_file(file(&app, "c.txt", false), false, false);
    keyboard(&mut app, KeyCode::Char(' '), KeyModifiers::CONTROL);
    assert!(selection(&app).is_empty());
    keyboard(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
    keyboard(&mut app, KeyCode::Delete, KeyModifiers::NONE);
    assert!(!app.bg_git_action_running && !app.sc_discard_confirm_active);
}

#[test]
fn collapsed_sections_do_not_interfere_with_clicking_unstaged_files() {
    let (_directory, mut app) = fixture();
    let mut files: Vec<_> = (0..50)
        .map(|i| GitFileChange {
            path: format!("staged-{i}.txt"),
            original_path: None,
            staged: true,
            status: 'M',
        })
        .collect();
    files.extend(app.source_control_files.clone());
    app.update_source_control_files(files);
    app.sc_staged_collapsed = true;
    app.select_source_control_file(file(&app, "a.txt", false), false, false);
    click_file(&mut app, "b.txt", MouseButton::Left, KeyModifiers::CONTROL);
    assert_eq!(
        selection(&app),
        vec![("a.txt".into(), false), ("b.txt".into(), false)]
    );
    click_file(&mut app, "b.txt", MouseButton::Right, KeyModifiers::NONE);
    assert!(
        app.source_control_context_menu_items()
            .contains(&"Discard Selected")
    );
}

#[test]
fn discard_refuses_a_new_file_staged_after_the_confirmation_opened() {
    let (directory, mut app) = fixture();
    let path = directory.path().join("new.txt");
    std::fs::write(&path, "keep staged\n").unwrap();
    refresh(&mut app);
    app.request_source_control_discard_all();
    let repo = git2::Repository::open(directory.path()).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("new.txt")).unwrap();
    index.write().unwrap();
    app.execute_source_control_discard_confirm(true);
    wait(&mut app);
    assert_eq!(std::fs::read_to_string(path).unwrap(), "keep staged\n");
    assert_eq!(
        std::fs::read_to_string(directory.path().join("a.txt")).unwrap(),
        "modified\n"
    );
    assert!(
        app.notification_history
            .iter()
            .any(|(message, _, _)| message.contains("changed staging state"))
    );
}

#[test]
fn literal_paths_and_unborn_repositories_work_with_batch_unstage() {
    let directory = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(directory.path()).unwrap();
    let mut index = repo.index().unwrap();
    for path in ["file[1].txt", "file1.txt", "keep.txt"] {
        std::fs::write(directory.path().join(path), "new\n").unwrap();
        index.add_path(Path::new(path)).unwrap();
    }
    index.write().unwrap();
    let mut app = App::new_deferred(directory.path().to_path_buf());
    refresh(&mut app);
    let changes = app
        .source_control_files
        .iter()
        .filter(|file| file.path != "file1.txt")
        .cloned()
        .collect();
    app.git_unstage_changes(changes);
    wait(&mut app);
    assert!(index_text(directory.path(), "file1.txt").is_some());
    assert!(index_text(directory.path(), "file[1].txt").is_none());
    assert!(index_text(directory.path(), "keep.txt").is_none());
    assert!(directory.path().join("file[1].txt").exists());
}
