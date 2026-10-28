use super::*;

fn init(root: &Path) {
    let repo = git2::Repository::init(root).unwrap();
    repo.config()
        .unwrap()
        .set_bool("core.autocrlf", false)
        .unwrap();
    std::fs::write(root.join("tracked.txt"), "original\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("tracked.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = git2::Signature::now("Test", "test@example.com").unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "Initial", &tree, &[])
        .unwrap();
    std::fs::write(root.join("tracked.txt"), "modified\n").unwrap();
    std::fs::create_dir_all(root.join("generated/deep")).unwrap();
    std::fs::write(root.join("generated/deep/new.txt"), "untracked\n").unwrap();
    std::fs::write(root.join("staged.txt"), "staged\n").unwrap();
    index.add_path(Path::new("staged.txt")).unwrap();
    index.write().unwrap();
}

#[test]
fn headless_status_omits_untracked_discovery_but_keeps_staged_and_modified_files() {
    let root = tempfile::tempdir().unwrap();
    init(root.path());
    let mut service = Service::new(root.path()).unwrap();
    for include_untracked in [false, true, false] {
        let Reply::GitStatus { scans, .. } = service
            .execute(Command::GitStatus {
                changed: None,
                known: None,
                parent_open: false,
                closed: Vec::new(),
                include_untracked,
            })
            .unwrap()
        else {
            panic!("Git status reply");
        };
        assert_eq!(scans.len(), 1);
        let files = &scans[0].files;
        assert_eq!(files.len(), if include_untracked { 3 } else { 2 });
        assert!(
            files
                .iter()
                .any(|file| file.path == path("staged.txt") && file.staged && file.status == 'A')
        );
        assert!(
            files
                .iter()
                .any(|file| file.path == path("tracked.txt") && !file.staged && file.status == 'M')
        );
        assert_eq!(
            files.iter().any(|file| file.status == '?'),
            include_untracked
        );
    }
}

#[test]
fn remote_cache_rescans_all_roots_when_untracked_mode_changes_and_reuses_matching_snapshots() {
    let root = tempfile::tempdir().unwrap();
    for name in ["first", "second"] {
        init(&root.path().join(name));
    }
    let traffic = Arc::new(AtomicUsize::new(0));
    let workspace = workspace::Workspace::new(tests::connection(
        root.path(),
        Arc::default(),
        traffic.clone(),
    ));
    let cancel = crate::background::CancellationToken::new();
    let (initial, _) = workspace
        .git_snapshot(true, None, false, Vec::new(), None, &cancel)
        .unwrap();
    let known = discovery(&workspace, &initial);
    for (include_untracked, expected_rows) in [(false, 2), (true, 4), (false, 2)] {
        for name in ["first", "second"] {
            std::fs::write(
                root.path().join(name).join("another.txt"),
                "new since the previous snapshot\n",
            )
            .unwrap();
        }
        // Even an event outside both repos cannot reuse snapshots collected
        // with the opposite visibility setting.
        let before = traffic.load(Ordering::Relaxed);
        let (_, scans) = workspace
            .git_snapshot(
                include_untracked,
                Some(known.clone()),
                false,
                Vec::new(),
                Some(vec![path("notes.txt")]),
                &cancel,
            )
            .unwrap();
        assert!(traffic.load(Ordering::Relaxed) > before);
        assert_eq!(scans.len(), 2);
        for scan in &scans {
            assert_eq!(scan.include_untracked, include_untracked);
            assert_eq!(scan.files.len(), expected_rows);
            assert_eq!(
                scan.files.iter().any(|file| file.status == '?'),
                include_untracked
            );
        }
        let before = traffic.load(Ordering::Relaxed);
        workspace
            .git_snapshot(
                include_untracked,
                Some(known.clone()),
                false,
                Vec::new(),
                Some(vec![path("notes.txt")]),
                &cancel,
            )
            .unwrap();
        assert_eq!(
            traffic.load(Ordering::Relaxed),
            before,
            "Matching cached status must cause zero Git traffic"
        );
    }
}

#[test]
fn remote_app_forwards_visibility_and_recovers_untracked_files_on_enable() {
    let root = tempfile::tempdir().unwrap();
    init(root.path());
    let workspace = workspace::Workspace::new(tests::connection(
        root.path(),
        Arc::default(),
        Arc::default(),
    ));
    let mut app = crate::app::App::new_deferred(workspace.view_root.clone());
    app.remote_workspace = Some(workspace);
    app.workspace_watcher_active = true;
    app.set_source_control_visibility(crate::source_control_rows::Visibility { untracked: false });
    app.refresh_source_control_async();
    app.wait_for_background(Duration::from_secs(15), |app| {
        !app.bg_git_status_running && !app.bg_git_status_pending
    });
    assert_eq!(app.source_control_files.len(), 2);
    assert!(
        app.source_control_files
            .iter()
            .all(|file| file.status != '?')
    );
    assert_eq!(app.source_control_change_count(), 2);
    app.set_source_control_visibility(crate::source_control_rows::Visibility { untracked: true });
    app.wait_for_background(Duration::from_secs(15), |app| {
        !app.bg_git_status_running && !app.bg_git_status_pending
    });
    assert_eq!(app.source_control_files.len(), 3);
    assert!(
        app.source_control_files
            .iter()
            .any(|file| file.status == '?')
    );
    assert_eq!(app.source_control_change_count(), 3);
    app.set_source_control_visibility(crate::source_control_rows::Visibility { untracked: false });
    assert!(!app.bg_git_status_running);
    assert_eq!(app.source_control_files.len(), 2);
}

#[test]
fn remote_reopen_dropdown_is_local_and_reopens_only_the_selected_closed_repository() {
    use crate::app::{ActivePane, App, SidebarView};
    use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};

    let root = tempfile::tempdir().unwrap();
    for name in ["first", "second", "third"] { init(&root.path().join(name)); }
    let traffic = Arc::new(AtomicUsize::new(0));
    let workspace = workspace::Workspace::new(tests::connection(root.path(), Arc::default(), traffic.clone()));
    let mut app = App::new_deferred(workspace.view_root.clone());
    app.remote_workspace = Some(workspace.clone());
    app.show_explorer = true;
    app.sidebar_view = SidebarView::SourceControl;
    app.active_pane = ActivePane::Explorer;
    app.workspace_watcher_active = true;
    app.refresh_source_control_async();
    let settle = |app: &mut App| app.wait_for_background(Duration::from_secs(15), |app| {
        !app.bg_git_status_running && !app.bg_git_status_pending
            && !app.bg_git_branch_running && !app.bg_git_log_running
    });
    settle(&mut app);
    let first = workspace.target_key("first").unwrap();
    let second = workspace.target_key("second").unwrap();
    let third = workspace.target_key("third").unwrap();
    assert_eq!(app.repositories.entries.len(), 3);
    for closed in [&second, &third] {
        app.close_source_control_repository(closed);
        settle(&mut app);
    }
    assert_eq!(app.source_control_root(), first);
    app.explorer_area = Some(Rect::new(0, 0, 60, 28));
    let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
    let before = traffic.load(Ordering::Relaxed);
    terminal.draw(|frame| crate::ui_source_control::draw_source_control_sidebar(
        frame, &mut app, Rect::new(0, 0, 60, 28))).unwrap();
    let button = app.repositories.reopen_button.unwrap();
    crate::mouse::handle_mouse(&mut app, MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left), column: button.x,
        row: button.y, modifiers: KeyModifiers::NONE,
    });
    assert!(app.context_menu_active);
    assert_eq!(app.repositories.reopen_candidates.len(), 2);
    assert!(app.context_menu_items.iter().any(|item| item.label.starts_with("second — ")));
    assert!(app.context_menu_items.iter().any(|item| item.label.starts_with("third — ")));
    assert!(app.context_menu_items.iter().all(|item| !item.label.contains(&workspace.view_root.to_string_lossy().to_string())));
    terminal.draw(|frame| crate::ui_dialogs::draw_context_menu(frame, &mut app)).unwrap();
    assert_eq!(traffic.load(Ordering::Relaxed), before, "listing closed repositories must send no requests");
    app.context_menu_selected = app.repositories.reopen_candidates.iter().position(|root| root == &second).unwrap();
    crate::input_pickers::handle_context_menu_key(&mut app, KeyCode::Enter);
    settle(&mut app);
    assert_eq!(app.source_control_root(), second);
    assert!(!app.repositories.closed.contains(&first));
    assert!(!app.repositories.closed.contains(&second));
    assert!(app.repositories.closed.contains(&third));
    assert_eq!(app.repositories.closed_roots().count(), 1);
    workspace.client.disconnect();
}
