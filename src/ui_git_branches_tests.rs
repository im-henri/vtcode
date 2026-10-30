use super::*;
use crossterm::event::{KeyEvent, KeyEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use std::time::Duration;

fn fixture(remote: bool) -> (tempfile::TempDir, git2::Repository, App) {
    let directory = tempfile::tempdir().unwrap();
    let mut options = git2::RepositoryInitOptions::new();
    options.initial_head("main");
    let repo = git2::Repository::init_opts(directory.path(), &options).unwrap();
    std::fs::write(directory.path().join("file.txt"), "saved\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("file.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = git2::Signature::now("Test", "test@example.com").unwrap();
    let commit = repo
        .commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
        .unwrap();
    repo.branch(
        "feature/existing",
        &repo.find_commit(commit).unwrap(),
        false,
    )
    .unwrap();
    repo.remote("origin", "unused-test-remote").unwrap();
    repo.reference("refs/remotes/origin/topic", commit, true, "test")
        .unwrap();
    drop(tree);
    let mut app = if remote {
        let workspace = crate::remote::workspace::Workspace::new(crate::remote::tests::connection(
            directory.path(),
            Default::default(),
            Default::default(),
        ));
        let mut app = App::new_deferred(workspace.view_root.clone());
        workspace.client.set_wake(app.background.wake_handle());
        app.remote_workspace = Some(workspace);
        app
    } else {
        App::new_deferred(directory.path().to_owned())
    };
    app.source_control_enabled = true;
    app.git_branch_picker_active = true;
    app.git_branch_picker_items = crate::app_git_branch::branches(directory.path()).unwrap();
    app.cached_git_branch = "main ↑3".into();
    app.git_branch_dialog = State::for_repository(app.source_control_root().to_owned());
    (directory, repo, app)
}

fn paint(app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| crate::ui_pickers::draw_git_branch_picker(frame, app))
        .unwrap();
    terminal.backend().buffer().clone()
}
fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, modifiers));
}
fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16) {
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        },
    );
}
fn click(app: &mut App, action: Action) {
    let (area, _, _, enabled) = *app
        .git_branch_dialog
        .buttons
        .iter()
        .find(|(_, candidate, _, _)| {
            std::mem::discriminant(candidate) == std::mem::discriminant(&action)
        })
        .unwrap();
    assert!(enabled);
    mouse(app, MouseEventKind::Down(MouseButton::Left), area.x, area.y);
    mouse(app, MouseEventKind::Up(MouseButton::Left), area.x, area.y);
}
fn wait(app: &mut App) {
    app.wait_for_background(Duration::from_secs(15), |app| !app.bg_git_action_running);
}
fn contents(buffer: &Buffer) -> String {
    buffer.content.iter().map(|cell| cell.symbol()).collect()
}

fn action_area(app: &App, action: Action) -> Rect {
    app.git_branch_dialog.buttons.iter().find(|(_, candidate, _, _)|
        std::mem::discriminant(candidate) == std::mem::discriminant(&action)).unwrap().0
}

#[test]
fn branch_rows_hover_and_clear_without_changing_selection_or_keyboard_focus() {
    let themes = crate::app_theme::load_builtin_themes();
    for theme in [
        themes.iter().find(|theme| !theme.is_light && !theme.high_contrast).unwrap(),
        themes.iter().find(|theme| theme.is_light && !theme.high_contrast).unwrap(),
        themes.iter().find(|theme| theme.high_contrast).unwrap(),
    ] {
        let (_directory, repo, mut app) = fixture(false);
        app.colors = theme.to_theme_colors();
        let before = paint(&mut app, 100, 30);
        let (row, index) = app.quick_picker.rows[1];
        assert_ne!(index, app.git_branch_picker_selected);
        let (selected_row, _) = app.quick_picker.rows[0];
        let selection = app.git_branch_picker_selected;
        let focus = app.git_branch_dialog.focus;
        app.mouse_hover_changed = false;
        mouse(&mut app, MouseEventKind::Moved, row.x + 4, row.y);
        assert!(app.mouse_hover_changed);
        let over = paint(&mut app, 100, 30);
        assert_ne!(over[(row.x, row.y)].bg, before[(row.x, row.y)].bg, "{}", theme.config_key);
        for x in row.x..row.right() {
            assert_eq!(over[(x, row.y)].bg, over[(row.x, row.y)].bg, "the entire row, including its badge, responds to hover");
        }
        assert_eq!(over[(selected_row.x, selected_row.y)], before[(selected_row.x, selected_row.y)]);
        assert_eq!(app.git_branch_picker_selected, selection);
        assert_eq!(app.git_branch_dialog.focus, focus);
        mouse(&mut app, MouseEventKind::Moved, 0, 0);
        let left = paint(&mut app, 100, 30);
        for x in row.x..row.right() { assert_eq!(left[(x, row.y)], before[(x, row.y)]); }
        assert_eq!(repo.head().unwrap().shorthand(), Some("main"));
        assert!(!app.bg_git_action_running);
    }
}

#[test]
fn branch_row_hover_uses_the_theme_hover_background_and_foreground() {
    let (_directory, _repo, mut app) = fixture(false);
    let mut theme = crate::ui_control_visibility_tests::theme_with_popup_and_control_roles();
    theme.workbench_colors.insert("list.hoverBackground".into(), "#123456".into());
    theme.workbench_colors.insert("list.hoverForeground".into(), "#ffffff".into());
    app.colors = theme.to_theme_colors();
    paint(&mut app, 100, 30);
    let row = app.quick_picker.rows[1].0;
    mouse(&mut app, MouseEventKind::Moved, row.x + 3, row.y);
    let buffer = paint(&mut app, 100, 30);
    assert_eq!(buffer[(row.x + 3, row.y)].bg, ratatui::style::Color::Rgb(18, 52, 86));
    assert_eq!(buffer[(row.x + 3, row.y)].fg, ratatui::style::Color::Rgb(255, 255, 255));
}

#[test]
fn branch_buttons_hover_in_browse_create_and_confirmation_without_stealing_focus() {
    for mode in 0..3 {
        let (_directory, repo, mut app) = fixture(false);
        let mut theme = crate::ui_control_visibility_tests::theme_with_popup_and_control_roles();
        theme.workbench_colors.insert("button.hoverBackground".into(), "#225588".into());
        theme.workbench_colors.insert("button.secondaryHoverBackground".into(), "#556622".into());
        app.colors = theme.to_theme_colors();
        replace_input(&mut app, "feature/existing".into());
        if mode == 1 { activate(&mut app, Action::New); replace_input(&mut app, "feature/new".into()); }
        if mode == 2 { activate(&mut app, Action::Review); }
        let actions = if mode == 0 { vec![Action::New, Action::Review, Action::Close] }
            else { vec![Action::Back, Action::Submit] };
        for action in actions {
            mouse(&mut app, MouseEventKind::Moved, 0, 0);
            paint(&mut app, 100, 30);
            let (_, _, target, enabled) = *app.git_branch_dialog.buttons.iter().find(|(_, candidate, _, _)|
                std::mem::discriminant(candidate) == std::mem::discriminant(&action)).unwrap();
            assert!(enabled);
            // Confirmation starts on Back. Use the other keyboard target so
            // this action's pointer feedback can be tested independently.
            app.git_branch_dialog.focus = if mode == 2 { 1 - target } else { 0 };
            let before = paint(&mut app, 100, 30);
            let area = action_area(&app, action);
            let focus = app.git_branch_dialog.focus;
            mouse(&mut app, MouseEventKind::Moved, area.x + 1, area.y);
            let over = paint(&mut app, 100, 30);
            let primary = matches!(action, Action::Review | Action::Submit);
            assert_eq!(over[(area.x, area.y)].bg, if primary {
                ratatui::style::Color::Rgb(34, 85, 136)
            } else { ratatui::style::Color::Rgb(85, 102, 34) });
            assert_ne!(over[(area.x, area.y)].bg, before[(area.x, area.y)].bg);
            assert_eq!(app.git_branch_dialog.focus, focus);
            mouse(&mut app, MouseEventKind::Moved, 0, 0);
            let left = paint(&mut app, 100, 30);
            for x in area.x..area.right() { assert_eq!(left[(x, area.y)], before[(x, area.y)]); }
            assert_eq!(repo.head().unwrap().shorthand(), Some("main"));
            assert!(!app.bg_git_action_running);
        }
    }
}

#[test]
fn hovering_switch_does_not_arm_confirmation_or_change_enter_target() {
    let (_directory, repo, mut app) = fixture(false);
    replace_input(&mut app, "feature/existing".into());
    activate(&mut app, Action::Review);
    paint(&mut app, 100, 30);
    let submit = action_area(&app, Action::Submit);
    assert_eq!(app.git_branch_dialog.focus, 0);
    mouse(&mut app, MouseEventKind::Moved, submit.x, submit.y);
    paint(&mut app, 100, 30);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(app.git_branch_dialog.mode, Mode::Browse), "Enter still activates Back after a pointer-only hover");
    assert_eq!(repo.head().unwrap().shorthand(), Some("main"));
    assert!(!app.bg_git_action_running);
}

#[test]
fn branch_disabled_and_busy_buttons_do_not_gain_hover_styles() {
    for mode in 0..3 {
        let (_directory, _repo, mut app) = fixture(false);
        if mode == 1 { activate(&mut app, Action::New); }
        if mode == 2 {
            replace_input(&mut app, "feature/existing".into());
            activate(&mut app, Action::Review);
            app.git_branch_dialog.pending = true;
        }
        let before = paint(&mut app, 100, 30);
        let area = action_area(&app, if mode == 0 { Action::Review } else { Action::Submit });
        let focus = app.git_branch_dialog.focus;
        mouse(&mut app, MouseEventKind::Moved, area.x + 1, area.y);
        let over = paint(&mut app, 100, 30);
        for x in area.x..area.right() { assert_eq!(over[(x, area.y)], before[(x, area.y)]); }
        assert_eq!(app.git_branch_dialog.focus, focus);
        assert!(!app.bg_git_action_running);
    }
}

#[test]
fn scrollbar_owns_its_full_width_and_drag_never_selects_or_checks_out_a_branch() {
    let (_directory, repo, mut app) = fixture(false);
    app.git_branch_picker_items
        .extend((0..100).map(|i| GitBranchPickerItem::Local {
            name: format!("feature/branch-{i:03}"),
        }));
    for column in 0..3 {
        paint(&mut app, 100, 30);
        let track = app.git_branch_dialog.scrollbar;
        assert_eq!(track.width, 3);
        assert!(
            app.quick_picker
                .rows
                .iter()
                .all(|(row, _)| row.right() < track.x)
        );
        let selected = app.git_branch_picker_selected;
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            track.x + column,
            track.y,
        );
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            0,
            track.bottom() + 5,
        );
        paint(&mut app, 100, 30);
        let row = app.quick_picker.rows.last().unwrap().0;
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            row.x,
            row.y,
        );
        assert_eq!(app.git_branch_picker_selected, selected);
        assert!(app.git_branch_picker_scroll_offset > 80);
        assert!(!app.bg_git_action_running);
        assert!(app.git_branch_picker_active);
        assert!(matches!(app.git_branch_dialog.mode, Mode::Browse));
        assert_eq!(repo.head().unwrap().shorthand(), Some("main"));
        app.git_branch_picker_scroll_offset = 0;
    }
}

#[test]
fn wheel_changes_viewport_without_changing_selection_and_rows_only_select() {
    let (_directory, repo, mut app) = fixture(false);
    app.git_branch_picker_items
        .extend((0..50).map(|i| GitBranchPickerItem::Local {
            name: format!("branch-{i}"),
        }));
    paint(&mut app, 90, 24);
    let list = app.quick_picker.list;
    mouse(&mut app, MouseEventKind::ScrollDown, list.x, list.y);
    assert_eq!(app.git_branch_picker_selected, 0);
    assert_eq!(app.git_branch_picker_scroll_offset, 3);
    paint(&mut app, 90, 24);
    let (row, index) = app.quick_picker.rows[2];
    mouse(&mut app, MouseEventKind::Moved, row.x, row.y);
    assert_eq!(app.git_branch_picker_selected, 0);
    for _ in 0..2 {
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            row.x,
            row.y,
        );
    }
    assert_eq!(app.git_branch_picker_selected, index);
    assert!(matches!(app.git_branch_dialog.mode, Mode::Browse));
    assert_eq!(repo.head().unwrap().shorthand(), Some("main"));
    assert!(!app.bg_git_action_running);
}

#[test]
fn switching_requires_explicit_review_and_confirmation_locally_and_remotely() {
    for remote in [false, true] {
        let (_directory, repo, mut app) = fixture(remote);
        app.git_branch_picker_query = "feature/existing".into();
        paint(&mut app, 100, 30);
        click(&mut app, Action::Review);
        assert!(matches!(app.git_branch_dialog.mode, Mode::Confirm(_)));
        assert!(!app.bg_git_action_running);
        let confirmation = paint(&mut app, 100, 30);
        assert!(contents(&confirmation).contains("feature/existing"));
        assert!(contents(&confirmation).contains("Switch Branch?"));
        let query = app.git_branch_picker_query.clone();
        assert!(app.paste_into_focused_text_input("a different target"));
        assert_eq!(app.git_branch_picker_query, query);
        // Enter defaults to Back; repeated Enter cannot accept a new prompt.
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(app.git_branch_dialog.mode, Mode::Browse));
        paint(&mut app, 100, 30);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        paint(&mut app, 100, 30);
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.git_branch_dialog.pending);
        assert!(
            !app.paste_into_focused_text_input("ignored") || app.git_branch_picker_query == query
        );
        wait(&mut app);
        assert!(
            !app.git_branch_picker_active,
            "{:?}",
            app.notification_history
        );
        assert_eq!(repo.head().unwrap().shorthand(), Some("feature/existing"));
    }
}

#[test]
fn create_form_validates_names_and_preserves_filter_when_cancelled() {
    let (_directory, repo, mut app) = fixture(false);
    replace_input(&mut app, "feature/new".into());
    paint(&mut app, 100, 30);
    let tabs = app.tabs.len();
    key(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
    assert_eq!(
        app.tabs.len(),
        tabs,
        "Ctrl+N belongs to this modal, not New File"
    );
    assert!(matches!(app.git_branch_dialog.mode, Mode::Create));
    for invalid in [
        "",
        "two words",
        "HEAD",
        "-option",
        "topic..bad",
        "main",
        "main/child",
        "feature",
    ] {
        replace_input(&mut app, invalid.into());
        paint(&mut app, 100, 30);
        assert!(name_error(&app).is_some(), "{invalid}");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!app.bg_git_action_running);
    }
    replace_input(&mut app, "feature/café".into());
    assert!(name_error(&app).is_none());
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.git_branch_picker_query, "feature/new");
    assert_eq!(repo.head().unwrap().shorthand(), Some("main"));
}

#[test]
fn create_and_switch_is_explicit_and_remote_branches_describe_tracking() {
    for remote in [false, true] {
        let (_directory, repo, mut app) = fixture(remote);
        paint(&mut app, 100, 30);
        click(&mut app, Action::New);
        replace_input(&mut app, "feature/new-branch".into());
        let form = paint(&mut app, 100, 30);
        assert!(contents(&form).contains("Create & switch"));
        assert!(contents(&form).contains("Create from current branch: main"));
        assert!(
            repo.find_branch("feature/new-branch", git2::BranchType::Local)
                .is_err()
        );
        click(&mut app, Action::Submit);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        wait(&mut app);
        assert_eq!(repo.head().unwrap().shorthand(), Some("feature/new-branch"));
        assert!(!app.git_branch_picker_active);
    }
    let (_directory, repo, mut app) = fixture(false);
    replace_input(&mut app, "origin/topic".into());
    paint(&mut app, 100, 30);
    click(&mut app, Action::Review);
    let buffer = paint(&mut app, 100, 30);
    assert!(contents(&buffer).contains("tracking origin/topic"));
    click(&mut app, Action::Submit);
    wait(&mut app);
    assert_eq!(repo.head().unwrap().shorthand(), Some("topic"));
    assert_eq!(
        repo.find_branch("topic", git2::BranchType::Local)
            .unwrap()
            .upstream()
            .unwrap()
            .name()
            .unwrap(),
        Some("origin/topic")
    );
}

#[test]
fn failed_checkout_keeps_context_and_errors_and_repeated_enter_does_not_submit() {
    let (_directory, repo, mut app) = fixture(false);
    app.git_branch_picker_items
        .push(GitBranchPickerItem::Local {
            name: "deleted-branch".into(),
        });
    replace_input(&mut app, "deleted-branch".into());
    paint(&mut app, 100, 30);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    paint(&mut app, 100, 30);
    let mut repeat = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    repeat.kind = KeyEventKind::Repeat;
    crate::input_keyboard::handle_keyboard_event(&mut app, repeat);
    assert!(matches!(app.git_branch_dialog.mode, Mode::Confirm(_)));
    click(&mut app, Action::Submit);
    wait(&mut app);
    assert!(app.git_branch_picker_active);
    assert!(app.git_branch_dialog.error.is_some());
    assert_eq!(app.git_branch_picker_query, "deleted-branch");
    assert_eq!(repo.head().unwrap().shorthand(), Some("main"));
    assert!(!app.git_branch_dialog.pending);
}

#[test]
fn current_branch_badge_ignores_ahead_counts_and_views_fit_small_terminals() {
    let (directory, repo, mut app) = fixture(false);
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    for index in 0..30 {
        repo.branch(&format!("feature/workspace-{index:02}"), &head, false)
            .unwrap();
    }
    app.git_branch_picker_items = crate::app_git_branch::branches(directory.path()).unwrap();
    let browse = paint(&mut app, 100, 30);
    assert!(contents(&browse).contains("Current"));
    assert!(is_current(&app, &app.git_branch_picker_items[0]));
    assert!(
        !app.git_branch_dialog
            .buttons
            .iter()
            .any(|(_, action, _, enabled)| matches!(action, Action::Review) && *enabled)
    );
    crate::ui_control_visibility_tests::snapshot(&browse, "Git branches - browse");
    activate(&mut app, Action::New);
    replace_input(&mut app, "feature/better-search".into());
    let create = paint(&mut app, 100, 30);
    crate::ui_control_visibility_tests::snapshot(&create, "Git branches - create");
    back(&mut app);
    replace_input(&mut app, "origin/topic".into());
    activate(&mut app, Action::Review);
    let confirm = paint(&mut app, 100, 30);
    crate::ui_control_visibility_tests::snapshot(&confirm, "Git branches - confirm");
    for mode in [
        Mode::Browse,
        Mode::Create,
        Mode::Confirm(GitBranchPickerItem::Local {
            name: "feature/long-branch-name".into(),
        }),
    ] {
        app.git_branch_dialog.mode = mode;
        for (width, height) in [(80, 24), (42, 14), (28, 12), (12, 7), (3, 3), (1, 1)] {
            paint(&mut app, width, height);
            assert_eq!(
                app.quick_picker
                    .area
                    .intersection(Rect::new(0, 0, width, height)),
                app.quick_picker.area
            );
            for (area, _, _, _) in &app.git_branch_dialog.buttons {
                assert_eq!(area.intersection(app.quick_picker.area), *area);
            }
            if width < 28 {
                assert!(!app.git_branch_dialog.review_ready);
            }
        }
    }
}

#[test]
fn unborn_repository_explains_why_branch_creation_needs_a_commit() {
    let directory = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(directory.path()).unwrap();
    let error = crate::git_tasks::execute::<String>(
        directory.path(),
        crate::git_tasks::Task::CreateBranch {
            branch_name: "feature/new".into(),
        },
    )
    .unwrap_err();
    assert!(error.contains("initial commit"));
    assert!(
        repo.find_branch("feature/new", git2::BranchType::Local)
            .is_err()
    );
}

#[test]
fn tab_skips_disabled_actions_and_home_end_follow_the_list() {
    let (_directory, _repo, mut app) = fixture(false);
    paint(&mut app, 100, 30);
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.git_branch_dialog.focus, 1);
    key(&mut app, KeyCode::End, KeyModifiers::NONE);
    assert_eq!(
        app.git_branch_picker_selected,
        app.git_branch_picker_total_rows() - 1
    );
    key(&mut app, KeyCode::Home, KeyModifiers::NONE);
    assert_eq!(app.git_branch_picker_selected, 0);
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.git_branch_dialog.focus, 2);
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(
        app.git_branch_dialog.focus, 4,
        "Switch is disabled for the current branch"
    );
    activate(&mut app, Action::New);
    paint(&mut app, 100, 30);
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(
        app.git_branch_dialog.focus, 2,
        "invalid branch names cannot focus Create"
    );
}

#[test]
fn checkout_review_cannot_be_applied_to_a_different_repository() {
    let (directory, repo, mut app) = fixture(false);
    replace_input(&mut app, "feature/existing".into());
    activate(&mut app, Action::Review);
    paint(&mut app, 100, 30);
    app.git_branch_dialog.repository = Some(directory.path().join("different-repository"));
    activate(&mut app, Action::Submit);
    assert!(!app.bg_git_action_running);
    assert!(
        app.git_branch_dialog
            .error
            .as_deref()
            .unwrap()
            .contains("repository changed")
    );
    assert_eq!(repo.head().unwrap().shorthand(), Some("main"));
}
