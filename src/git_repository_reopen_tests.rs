use super::*;
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};

fn menu(app: &mut App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
    terminal
        .draw(|frame| crate::ui_dialogs::draw_context_menu(frame, app))
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

fn choose(app: &mut App, root: &Path, mouse: bool) {
    let index = app
        .repositories
        .reopen_candidates
        .iter()
        .position(|candidate| candidate == root)
        .unwrap();
    menu(app);
    if mouse {
        let inner = app.context_menu_inner.unwrap();
        crate::mouse_overlay::handle_context_menu_mouse(
            app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: inner.x + 1,
                row: inner.y + index as u16,
                modifiers: KeyModifiers::NONE,
            },
        );
    } else {
        crate::input_pickers::handle_context_menu_key(app, KeyCode::Home);
        for _ in 0..index {
            crate::input_pickers::handle_context_menu_key(app, KeyCode::Down);
        }
        crate::input_pickers::handle_context_menu_key(app, KeyCode::Enter);
    }
}

#[test]
fn reopen_dropdown_restores_one_closed_project_and_preserves_other_projects_and_drafts() {
    for mouse in [false, true] {
        let mut fixture = Fixture::new();
        let other = fixture.root.join("other-project");
        init(&other, "other\n");
        std::fs::write(
            fixture.root.join(".git/info/exclude"),
            "nested/\nother-project/\n",
        )
        .unwrap();
        let app = &mut fixture.app;
        app.repositories.rescan = true;
        refresh(app);
        for root in [&fixture.nested, &other] {
            app.close_source_control_repository(root);
            wait(app, |app| {
                !app.bg_git_status_running && !app.bg_git_status_pending
            });
        }
        app.source_control_commit_msg = "keep my outer commit draft".into();
        app.source_control_commit_cursor = app.source_control_commit_msg.len();
        let screen = draw(app, 48, 28);
        assert!(screen.contains("Reopen (2)"));
        let button = app.repositories.reopen_button.unwrap();
        click(app, button);
        assert!(app.context_menu_active);
        assert_eq!(app.context_menu_pos, (button.x, button.bottom()));
        assert_eq!(app.repositories.reopen_candidates.len(), 2);
        assert!(!app.repositories.reopen_candidates.contains(&fixture.root));
        assert!(menu(app).contains("nested — "));
        choose(app, &fixture.nested, mouse);
        assert!(!app.context_menu_active);
        wait(app, |app| {
            !app.bg_git_status_running && !app.bg_git_status_pending
        });
        assert_eq!(app.source_control_root(), fixture.nested);
        assert!(!app.repositories.closed.contains(&fixture.root));
        assert!(!app.repositories.closed.contains(&fixture.nested));
        assert!(app.repositories.closed.contains(&other));
        assert!(draw(app, 48, 28).contains("Reopen (1)"));
        assert_eq!(index_text(&fixture.root), "outer before\n");
        assert_eq!(index_text(&fixture.nested), "inner before\n");
        app.select_source_control_repository(&fixture.root);
        assert_eq!(app.source_control_commit_msg, "keep my outer commit draft");
        let saved = app.saved_repositories();
        assert_eq!(saved.closed, [other]);
    }
}

#[test]
fn reopen_button_allows_holding_dragging_and_releasing_on_one_closed_repository() {
    use crossterm::event::{MouseButton, MouseEventKind};
    for right in [false, true] {
        let mut fixture = Fixture::new();
        let other = fixture.root.join("other-project");
        init(&other, "other\n");
        let app = &mut fixture.app;
        app.sidebar_right = right;
        app.repositories.rescan = true;
        refresh(app);
        for root in [&fixture.nested, &other] {
            app.close_source_control_repository(root);
            wait(app, |app| !app.bg_git_status_running && !app.bg_git_status_pending);
        }
        draw(app, 48, 28);
        let button = app.repositories.reopen_button.unwrap();
        click(app, button);
        let send = |app: &mut App, kind, column, row| {
            crate::mouse_overlay::handle_context_menu_mouse(app, MouseEvent {
                kind, column, row, modifiers: KeyModifiers::NONE,
            });
        };
        send(app, MouseEventKind::Drag(MouseButton::Left), button.x, button.y);
        assert!(app.context_menu_active, "held motion before drawing the menu must not dismiss it");
        menu(app);
        let inner = app.context_menu_inner.unwrap();
        let index = app.repositories.reopen_candidates.iter().position(|root| root == &fixture.nested).unwrap();
        for (x, y) in [(button.x, button.y), (inner.x, inner.y - 1), (139, 29), (inner.x + 1, inner.y + index as u16)] {
            send(app, MouseEventKind::Drag(MouseButton::Left), x, y);
            assert!(app.context_menu_active);
            assert!(app.repositories.closed.contains(&fixture.nested));
            assert!(!app.bg_git_status_running && !app.bg_git_status_pending);
        }
        assert_eq!(app.context_menu_selected, index);
        send(app, MouseEventKind::Up(MouseButton::Left), inner.x + 1, inner.y + index as u16);
        assert!(!app.context_menu_active && app.context_menu_button_drag.is_none());
        wait(app, |app| !app.bg_git_status_running && !app.bg_git_status_pending);
        assert_eq!(app.source_control_root(), fixture.nested);
        assert!(!app.repositories.closed.contains(&fixture.nested));
        assert!(app.repositories.closed.contains(&other));
    }
}

#[test]
fn narrow_and_disabled_sidebars_keep_reopen_controls_consistent_and_cancel_is_read_only() {
    let mut fixture = Fixture::new();
    let app = &mut fixture.app;
    app.close_source_control_repository(&fixture.nested);
    wait(app, |app| {
        !app.bg_git_status_running && !app.bg_git_status_pending
    });
    for right in [false, true] {
        app.sidebar_right = right;
        for width in [1, 3, 15, 22, 26, 48] {
            draw(app, width, 28);
            let actions = app.source_control_actions_area.unwrap();
            if let Some(reopen) = app.repositories.reopen_button {
                assert!(reopen.x > actions.right());
                assert!(reopen.right() <= width);
            }
            if width >= 26 {
                assert!(app.repositories.reopen_button.is_some());
            }
            click(app, actions);
            let index = app
                .context_menu_items
                .iter()
                .position(|item| item.id == "git_open_repository")
                .unwrap();
            assert_eq!(app.context_menu_items[index].label, "Reopen Repository…");
            app.context_menu_selected = index;
            crate::input_pickers::handle_context_menu_key(app, KeyCode::Enter);
            assert!(
                app.context_menu_active,
                "the actions menu must transition to the repository picker"
            );
            assert_eq!(app.repositories.reopen_candidates, [fixture.nested.clone()]);
            let generation = app.repositories.membership_generation;
            crate::input_pickers::handle_context_menu_key(app, KeyCode::Esc);
            assert!(!app.context_menu_active);
            assert_eq!(app.repositories.membership_generation, generation);
            assert!(!app.bg_git_status_running);
            assert!(app.repositories.closed.contains(&fixture.nested));
        }
    }
    app.close_source_control_repository(&fixture.root);
    wait(app, |app| {
        !app.bg_git_status_running && !app.bg_git_status_pending
    });
    let compact = draw(app, 22, 28);
    assert!(compact.contains("Reopen All"));
    assert!(
        !compact.contains("Reopen All Repo"),
        "use the compact label rather than clipping it"
    );
    assert!(draw(app, 48, 28).contains("Reopen (2)"));
    let button = app.repositories.reopen_button.unwrap();
    click(app, button);
    choose(app, &fixture.nested, false);
    wait(app, |app| {
        !app.bg_git_status_running && !app.bg_git_status_pending
    });
    assert_eq!(app.source_control_root(), fixture.nested);
    assert!(app.repositories.closed.contains(&fixture.root));
    app.set_source_control_enabled(false);
    draw(app, 48, 28);
    assert!(app.repositories.reopen_button.is_none());
    app.open_repository_picker();
    assert!(!app.context_menu_active && !app.bg_git_status_running);
}

#[test]
fn reopening_a_project_reveals_its_header_when_the_repository_list_overflows() {
    let mut fixture = Fixture::new();
    for index in 0..7 {
        init(&fixture.root.join(format!("project-{index}")), "project\n");
    }
    let app = &mut fixture.app;
    app.repositories.rescan = true;
    refresh(app);
    let target = fixture.root.join("project-6");
    app.close_source_control_repository(&target);
    wait(app, |app| {
        !app.bg_git_status_running && !app.bg_git_status_pending
    });
    draw(app, 48, 24);
    assert_eq!(app.repositories.headers.len(), 6);
    assert_eq!(app.repositories.header_scroll, 0);
    let button = app.repositories.reopen_button.unwrap();
    click(app, button);
    choose(app, &target, false);
    wait(app, |app| {
        !app.bg_git_status_running && !app.bg_git_status_pending
    });
    draw(app, 48, 24);
    assert!(
        app.repositories
            .headers
            .iter()
            .any(|(_, root)| root == &target)
    );
    assert_eq!(app.source_control_root(), target);
    assert!(app.repositories.reopen_button.is_none());
}

#[test]
fn reopen_candidates_ignore_obsolete_paths_and_do_not_duplicate_a_closed_parent() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().to_owned();
    let mut app = App::new_deferred(root_path.clone());
    app.repositories.parent = Some(root_path.clone());
    app.repositories.entries.push(Repository {
        root: root_path.clone(),
        files: Vec::new(),
        branch: String::new(),
        counts: [0; Group::ALL.len()],
        view: Default::default(),
    });
    app.repositories
        .closed
        .extend([root_path.clone(), root_path.join("removed-project")]);
    assert_eq!(app.repositories.closed_roots().count(), 1);
    app.open_repository_picker();
    assert_eq!(app.repositories.reopen_candidates, [root_path.clone()]);
    app.repositories.entries.clear();
    app.open_repository_picker();
    assert_eq!(app.repositories.reopen_candidates, [root_path]);
    let generation = app.repositories.membership_generation;
    app.context_menu_selected = usize::MAX;
    app.reopen_selected_repository();
    assert_eq!(app.repositories.membership_generation, generation);
    assert_eq!(app.background.active_job_count(), 0);
}
