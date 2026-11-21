use super::*;
use crate::{
    app::{
        ActivePane, GitFileChange, SidebarView, SourceControlRepositoryState, SplitDirection, Tab,
    },
    editor::Editor,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use std::{
    io::{Read, Write},
    time::{Duration, Instant},
};

fn app_with_tabs(names: &[&str]) -> App {
    let mut app = App::test_new();
    app.tabs = names
        .iter()
        .map(|name| {
            let mut editor = Editor::test_with_text("sample text\n");
            editor.file_path = Some(std::path::PathBuf::from(name));
            Tab::new_file(editor)
        })
        .collect();
    app
}

#[test]
fn moving_between_windows_retains_the_edited_view() {
    let mut app = app_with_tabs(&["one.txt", "two.txt", "three.txt", "four.txt"]);
    app.tabs[0].editor.insert_char('X');
    app.tabs[0].pinned = true;
    let view = app.tabs[0].editor.navigation_id;
    let document = app.tabs[0].editor.save_identity;
    let second = app.tabs[1].editor.navigation_id;
    let third = app.tabs[2].editor.navigation_id;
    app.create_split_group_dir(2, SplitDirection::Horizontal);
    assert!(app.open_detached_window(1, view));
    assert!(app.open_detached_window(2, third));
    assert!(app.move_tab_to_window(second, 1));
    assert!(app.move_tab_to_window(view, 2));
    assert!(app.activate_window(2));
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.editor().unwrap().navigation_id, view);
    assert_eq!(app.editor().unwrap().save_identity, document);
    assert_eq!(app.editor().unwrap().rope.to_string(), "Xsample text\n");
    assert!(app.tabs[app.active_tab].has_unsaved_changes());
    assert!(app.tabs[app.active_tab].pinned);
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope.to_string(), "sample text\n");
    assert!(app.move_tab_to_window(view, MAIN_WINDOW));
    assert!(app.return_detached_tabs(1));
    assert!(app.return_detached_tabs(2));
    assert!(app.activate_window(MAIN_WINDOW));
    assert_eq!(app.tabs.len(), 4);
    assert_eq!(app.editor().unwrap().navigation_id, view);
    assert!(app.editor().unwrap().rope.to_string().starts_with("sample"));
    assert_eq!(app.desk_count(), 1);
}

#[test]
fn source_control_tab_keeps_its_draft_when_detached_and_returned() {
    let root = tempfile::tempdir().unwrap();
    let mut app = app_with_tabs(&["one.txt"]);
    app.explorer = crate::explorer::FileExplorer::new_deferred(root.path().to_owned());
    app.show_explorer = true;
    app.sidebar_view = SidebarView::SourceControl;
    app.explorer_width = 30;
    app.source_control_repository_state = SourceControlRepositoryState::Available;
    app.source_control_commit_msg = "draft".into();
    app.source_control_commit_cursor = 5;
    let mut terminal = Terminal::new(TestBackend::new(125, 32)).unwrap();
    let draw = |terminal: &mut Terminal<TestBackend>, app: &mut App| {
        terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    };
    let click = |app: &mut App, area: Rect| {
        for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
            crate::mouse::handle_mouse(app, MouseEvent { kind, column: area.x, row: area.y, modifiers: KeyModifiers::NONE });
        }
    };

    draw(&mut terminal, &mut app);
    let open = app.source_control_placement_area.unwrap();
    click(&mut app, open);
    let view = app.editor().unwrap().navigation_id;
    assert!(app.source_control_tab_active());
    assert_eq!(app.sidebar_view, SidebarView::Explorer);
    draw(&mut terminal, &mut app);
    let detach = app.source_control_placement_area.unwrap();
    click(&mut app, detach);
    assert!(matches!(app.tab_window_request.take(), Some(Request::Open { view: id, .. }) if id == view));
    assert!(app.open_detached_window(1, view));

    // The same activity shortcut finds the existing view, even in a different window.
    app.activate_activity_item(2);
    assert!(matches!(app.tab_window_request.take(), Some(Request::FocusSourceControl { view: id, window: 1 }) if id == view));
    assert!(app.activate_window(1));
    app.source_control_repository_state = SourceControlRepositoryState::Available;
    draw(&mut terminal, &mut app);
    assert!(screen_text(&terminal).contains("⑂ Source Control"));
    let panels = crate::ui_source_control::Panels::visible(&app).unwrap();
    assert!(panels.side_by_side);
    assert!(panels.changes.right() < panels.graph.x);
    assert!(app.explorer_area.is_none());
    assert!(app.code_area.is_none());
    for code in [KeyCode::Char('m'), KeyCode::End, KeyCode::Char('!')] {
        crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::new(code, KeyModifiers::NONE));
    }
    assert_eq!(app.source_control_commit_msg, "draft!");
    let reattach = app.source_control_placement_area.unwrap();
    click(&mut app, reattach);
    assert!(matches!(app.tab_window_request.take(), Some(Request::Move { view: id, window: MAIN_WINDOW }) if id == view));

    assert!(app.return_detached_tabs(1));
    assert_eq!(app.tab_window(view), Some(MAIN_WINDOW));
    app.open_source_control_tab(false);
    assert_eq!(app.editor().unwrap().navigation_id, view);
    draw(&mut terminal, &mut app);
    let actions = app.source_control_actions_area.unwrap();
    click(&mut app, actions);
    assert!(app.context_menu_items.iter().any(|item| item.id == "source_control_move_sidebar"));
    app.execute_palette_command("source_control_move_sidebar");
    assert!(app.source_control_sidebar_visible());
    assert!(app.tab_location(view).is_none());
    assert_eq!(app.source_control_commit_msg, "draft!");
}

#[test]
fn source_control_tab_is_restored_in_the_workbench_session() {
    let root = tempfile::tempdir().unwrap();
    let mut app = App::new(root.path().to_owned());
    app.open_source_control_tab(false);
    let session = app.build_session();
    let mut restored = App::new(root.path().to_owned());
    restored.restore_session(&session);
    assert!(restored.source_control_tab_active());
    assert_eq!(restored.editor().unwrap().display_name(), "Source Control");
    assert!(restored.editor().unwrap().file_path.is_none());
}

#[test]
fn source_control_tab_in_an_inactive_split_accepts_commit_input() {
    use crate::text_input::TextInputId;

    let root = tempfile::tempdir().unwrap();
    let mut app = app_with_tabs(&["one.txt"]);
    app.explorer = crate::explorer::FileExplorer::new_deferred(root.path().to_owned());
    app.show_explorer = false;
    app.open_source_control_tab(false);
    let file_group = app.active_group;
    app.create_split_group_dir(app.active_tab, SplitDirection::Horizontal);
    assert_ne!(app.active_group, file_group);
    app.switch_group(file_group);
    app.source_control_repository_state = SourceControlRepositoryState::Available;
    let mut terminal = Terminal::new(TestBackend::new(120, 28)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
    let (source_group, _) = app.source_control_tab_surface().unwrap();
    assert_ne!(source_group, app.active_group);
    let panels = crate::ui_source_control::Panels::visible(&app).unwrap();
    assert!(!panels.side_by_side);
    assert_eq!(panels.changes.bottom(), panels.graph.y);
    let area = app.text_input_area(TextInputId::SourceControlCommit).unwrap();
    crate::mouse::handle_mouse(&mut app, MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left), column: area.x, row: area.y, modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.active_group, source_group);
    assert!(app.source_control_has_focus());
    assert_eq!(app.focused_text_input_id(), Some(TextInputId::SourceControlCommit));
    crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
    assert_eq!(app.source_control_commit_msg, "a");
}

#[test]
fn opening_a_file_in_a_new_window_loads_it_only_in_that_window() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("new.txt");
    std::fs::write(&path, "new window\n").unwrap();
    let mut app = app_with_tabs(&["original.txt"]);
    let original = app.editor().unwrap().navigation_id;
    let mut windows = Windows::new(&app);
    windows.windows.insert(1, Window {
        workspace: app.workspaces.active,
        source: WindowSource::File { path, generation: app.workspace_generation },
        surface: None,
        launch: None,
    });
    windows.poll(&mut app);
    assert!(windows.windows.contains_key(&1));
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.editor().unwrap().navigation_id, original);

    let listener = transport::Listener::bind().unwrap();
    let endpoint = listener.endpoint();
    let notices = windows.notices.clone();
    let wake = windows.wake.clone();
    let server_wake = wake.clone();
    let server = std::thread::spawn(move || {
        frontend::accept(listener, move |notice| {
            notices.send((1, notice)).map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
            server_wake.notify();
            Ok(())
        }, UiWake::default(), Arc::new(transport::Opening::new().unwrap()))
    });
    let mut peer = transport::connect(&endpoint).unwrap();
    let hello = frontend::Input::Hello { size: (80, 24), host: None };
    transport::write_packet(&mut peer, &serde_json::to_vec(&hello).unwrap()).unwrap();
    until(&wake, "file window handshake", || {
        windows.poll(&mut app);
        windows.windows.get(&1).is_some_and(|window| window.surface.is_some())
    });

    assert_eq!(windows.active(), 1);
    assert_eq!(app.detached_window_id(), Some(1));
    app.wait_for_background(Duration::from_secs(5), |app| !app.editor().unwrap().bg_loading);
    assert_eq!(app.editor().unwrap().rope, "new window\n");
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.tab_window(original), Some(MAIN_WINDOW));
    assert!(app.activate_window(MAIN_WINDOW));
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.editor().unwrap().navigation_id, original);
    assert!(windows.close(&mut app, 1));
    drop(peer);
    server.join().unwrap().unwrap();
}

#[test]
fn opening_a_file_in_an_existing_window_shares_the_live_document() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("shared.txt");
    let mut app = app_with_tabs(&["original.txt", "destination.txt"]);
    app.tabs[0].editor.file_path = Some(path.clone());
    app.tabs[0].editor.insert_char('X');
    let original = app.tabs[0].editor.navigation_id;
    let destination = app.tabs[1].editor.navigation_id;
    assert!(app.open_detached_window(1, destination));
    let mut windows = Windows::new(&app);

    app.request_file_window(path.clone(), true);
    for code in [KeyCode::Down, KeyCode::Enter] {
        crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::new(code, KeyModifiers::NONE));
    }
    let request = app.tab_window_request.take().unwrap();
    assert!(matches!(&request, Request::OpenFile { path: target, window: Some(1) } if target == &path));
    assert!(windows.request(&mut app, request));
    assert_eq!(app.detached_window_id(), Some(1));
    let opened = app.editor().unwrap();
    let original_group = app.tab_location(original).unwrap().0;
    assert!(opened.shares_document(&app.groups[original_group].tabs[0].editor));
    assert_eq!(opened.rope, "Xsample text\n");
    assert_eq!(app.tabs.len(), 2);
    assert!(app.activate_window(MAIN_WINDOW));
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.editor().unwrap().navigation_id, original);
}

#[test]
fn detached_frame_keeps_tabs_and_the_interactive_status_bar() {
    let mut app = app_with_tabs(&["first.txt", "second.txt"]);
    let view = app.tabs[0].editor.navigation_id;
    app.show_menu_bar = true;
    app.show_activity_bar = true;
    app.show_status_bar = true;
    app.show_tab_bar = false;
    app.show_explorer = true;
    assert!(app.open_detached_window(1, view));
    assert!(app.activate_window(1));
    app.show_explorer = true;
    app.show_panel = true;
    app.cached_git_branch = "detached-branch".into();
    app.editor_mut().unwrap().set_cursor(0, 3);
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    assert_eq!(app.tab_bar_area.unwrap().y, 0);
    assert_eq!(app.tab_bar_area.unwrap().width, 100);
    assert!(app.menu_bar_area.is_none());
    assert!(app.activity_bar_area.is_none());
    assert!(app.explorer_area.is_none());
    assert!(app.panel_area.is_none());
    let status_area = Rect::new(0, 29, 100, 1);
    assert_eq!(app.status_bar_area, Some(status_area));
    let text = screen_text(&terminal);
    for label in ["detached-branch", "Ln 1, Col 4", "GPTComplete"] {
        assert!(text.contains(label), "status label {label:?} is missing");
    }
    let click_status = |app: &mut App, action| {
        let &(column, _, _) = app.status_bar_items.iter().find(|item| item.2 == action).unwrap();
        crate::mouse::handle_mouse(app, MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row: status_area.y,
            modifiers: KeyModifiers::NONE,
        });
    };
    click_status(&mut app, "neural_completion");
    assert!(crate::ui_status::gptcomplete_menu_open(&app));
    crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::from(KeyCode::Esc));
    click_status(&mut app, "performance");
    assert!(app.background.activity.details_open);
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL),
    );
    assert!(app.performance_view.pinned);
    terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
    let dock = app.performance_area.unwrap();
    assert!(dock.bottom() <= status_area.y);
    assert_eq!(app.status_bar_area, Some(status_area));
    let close = (dock.y..dock.bottom())
        .flat_map(|row| (dock.x..dock.right()).map(move |column| (column, row)))
        .find(|&point| terminal.backend().buffer()[point].symbol() == "×")
        .unwrap();
    let mut main = Terminal::new(TestBackend::new(80, 22)).unwrap();
    main.draw(|frame| crate::ui::draw_background_window(frame, &mut app, MAIN_WINDOW)).unwrap();
    assert_eq!(app.performance_area, Some(dock));
    crate::mouse::handle_mouse(&mut app, MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: close.0,
        row: close.1,
        modifiers: KeyModifiers::NONE,
    });
    assert!(!app.background.activity.details_open);

    for (show_status_bar, zen_mode) in [(false, false), (true, true)] {
        app.show_status_bar = show_status_bar;
        app.zen_mode = zen_mode;
        terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
        assert!(app.status_bar_area.is_none());
        assert!(app.status_bar_items.is_empty());
    }
    app.zen_mode = false;
    assert!(app.activate_window(MAIN_WINDOW));
    assert!(app.show_explorer);
    assert!(!app.show_tab_bar);
}

#[test]
fn background_detached_status_uses_its_editor_and_preserves_foreground_hit_targets() {
    let mut app = app_with_tabs(&["main.txt", "detached.txt"]);
    app.show_status_bar = true;
    app.tabs[0].editor.set_cursor(0, 3);
    app.tabs[0].editor.selection_anchor = Some((0, 0));
    let detached = &mut app.tabs[1].editor;
    detached.set_cursor(0, 8);
    let view = detached.navigation_id;
    app.editor_analysis_memo.remember(
        (view, detached.content_version, detached.rope.len_bytes()),
        crate::app_event_loop::AnalysisSummary {
            errors: 0,
            warnings: 2,
            max_line_width: 11,
            word_count: 2,
            sticky_scopes: Default::default(),
        },
    );
    assert!(app.open_detached_window(1, view));
    app.cached_error_count = 9;
    let mut foreground = Terminal::new(TestBackend::new(120, 30)).unwrap();
    foreground.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
    let hits = app.status_bar_items.clone();
    let status_area = app.status_bar_area.unwrap();
    assert_eq!(app.cached_sel_stats.as_deref(), Some("(3 selected)"));

    let mut background = Terminal::new(TestBackend::new(90, 22)).unwrap();
    background.draw(|frame| crate::ui::draw_background_window(frame, &mut app, 1)).unwrap();
    let text = screen_text(&background);
    assert!(text.contains("Ln 1, Col 9"));
    assert!(text.contains("⚠ 2"));
    assert!(!text.contains("3 selected"));
    assert_eq!(app.cached_sel_stats.as_deref(), Some("(3 selected)"));
    assert_eq!(app.status_bar_area, Some(status_area));
    assert_eq!(app.status_bar_items, hits);
}

fn screen_text(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn background_frames_wait_only_for_damaged_windows_in_the_changed_workspace() {
    let mut app = app_with_tabs(&["main.txt", "detached.txt"]);
    let view = app.tabs[1].editor.navigation_id;
    assert!(app.open_detached_window(1, view));
    assert!(app.activate_window(1));
    app.terminal_has_focus = true;
    let mut windows = Windows::new(&app);
    windows.id = 1;
    let now = Instant::now();
    windows.invalidate_workspace(windows.main_workspace + 1);
    assert_eq!(windows.background_draw_wait(true, now), None);
    windows.invalidate_workspace(windows.main_workspace);
    assert_eq!(
        windows.background_draw_wait(true, now),
        Some(Duration::ZERO)
    );
    windows.pace_background_draws(now);
    assert!(
        windows
            .background_draw_wait(true, now)
            .is_some_and(|delay| !delay.is_zero())
    );
    assert_eq!(windows.background_draw_wait(false, now), None);
    let mut main = Terminal::new(TestBackend::new(80, 24)).unwrap();
    windows.paint_main_background(&mut main, &mut app).unwrap();
    assert_eq!(windows.background_draw_wait(true, now), None);

    app.highlight_words_across_tabs = true;
    let mut detached = Terminal::new(TestBackend::new(90, 24)).unwrap();
    detached.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
    windows.invalidate_pending_workspace_redraw(&mut app);
    assert!(windows.background_draw_ready(true));
    windows.paint_main_background(&mut main, &mut app).unwrap();
    assert!(!windows.background_draw_ready(true));
}

#[test]
fn a_window_waiting_for_its_first_focused_frame_cannot_block_another_windows_input() {
    let mut windows = Windows::new(&App::test_new());
    let (input, queue) = mpsc::channel();
    let mut main = TerminalEventPump::from_receiver(queue);
    windows.select_window(1);
    let key = Event::Key(KeyEvent::from(KeyCode::Char('x')));
    windows.defer_input_until_focused_frame(key.clone());
    input.send(Ok(Event::FocusGained)).unwrap();
    assert_eq!(
        windows.next_event(&mut main).unwrap(),
        Some((MAIN_WINDOW, Event::FocusGained))
    );
    windows.select_window(MAIN_WINDOW);
    windows.focused_frame_drawn();
    assert!(windows.next_event(&mut main).unwrap().is_none());
    windows.select_window(1);
    windows.focused_frame_drawn();
    assert_eq!(windows.next_event(&mut main).unwrap(), Some((1, key)));
}

#[test]
fn background_main_refreshes_its_own_sidebar_while_the_detached_editor_keeps_focus() {
    for sidebar in [SidebarView::Explorer, SidebarView::SourceControl] {
        let folder = tempfile::tempdir().unwrap();
        let mut app = app_with_tabs(&["main-view.txt", "detached-view.txt"]);
        app.explorer.root = folder.path().to_owned();
        app.sidebar_view = sidebar;
        app.show_explorer = true;
        app.explorer_width = 40;
        app.set_sidebar_auto_hide(true);
        app.active_pane = ActivePane::Explorer;
        app.source_control_enabled = true;
        app.source_control_repository_state = SourceControlRepositoryState::Available;
        app.sc_graph_collapsed = true;

        let view = app.tabs[1].editor.navigation_id;
        assert!(app.open_detached_window(1, view));
        app.active_pane = ActivePane::Explorer;
        assert!(app.activate_window(1));
        app.terminal_has_focus = true;
        app.show_explorer = false;
        app.sidebar_view = SidebarView::Extensions;

        let mut windows = Windows::new(&app);
        windows.id = 1;
        let mut main = Terminal::new(TestBackend::new(120, 30)).unwrap();
        let mut detached = Terminal::new(TestBackend::new(80, 24)).unwrap();
        detached
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let focused_code = app.code_area;
        windows.invalidate_background();
        windows.paint_main_background(&mut main, &mut app).unwrap();
        assert!(screen_text(&main).contains("main-view.txt"));
        assert!(!screen_text(&main).contains("fresh-change.txt"));

        std::fs::write(
            folder.path().join("fresh-change.txt"),
            "created while detached",
        )
        .unwrap();
        app.explorer.refresh();
        app.source_control_files.push(GitFileChange {
            path: "fresh-change.txt".into(),
            directory: false,
            status: 'M',
            staged: false,
            original_path: None,
        });
        windows.invalidate_background();
        app.terminal_has_focus = false;
        assert!(!windows.background_draw_ready(false));
        windows.paint_main_background(&mut main, &mut app).unwrap();
        assert!(!screen_text(&main).contains("fresh-change.txt"));

        app.terminal_has_focus = true;
        assert!(windows.background_draw_ready(true));
        windows.paint_main_background(&mut main, &mut app).unwrap();
        assert!(
            screen_text(&main).contains("fresh-change.txt"),
            "{sidebar:?}"
        );
        assert!(!windows.background_draw_ready(true));
        assert_eq!(windows.active(), 1);
        assert_eq!(app.detached_window_id(), Some(1));
        assert_eq!(app.editor().unwrap().navigation_id, view);
        assert!(app.terminal_has_focus);
        assert_eq!(app.code_area, focused_code);
        assert_eq!(app.sidebar_view, SidebarView::Extensions);
        assert!(!app.show_explorer);
        crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::from(KeyCode::Char('Z')));
        assert!(app.editor().unwrap().rope.to_string().starts_with('Z'));
    }
}

#[test]
fn resizing_a_detached_split_keeps_the_window_active() {
    for direction in [SplitDirection::Horizontal, SplitDirection::Vertical] {
        let mut app = app_with_tabs(&["main.txt", "first.txt", "second.txt"]);
        app.show_explorer = false;
        app.show_panel = false;
        app.show_minimap = false;
        let main_view = app.tabs[0].editor.navigation_id;
        let first = app.tabs[1].editor.navigation_id;
        let second = app.tabs[2].editor.navigation_id;
        assert!(app.open_detached_window(1, first));
        assert!(app.move_tab_to_window(second, 1));
        assert!(app.activate_window(1));
        app.create_split_group_dir(app.active_tab, direction);
        app.terminal_has_focus = true;

        let mut detached = Terminal::new(TestBackend::new(100, 32)).unwrap();
        detached.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
        let divider = app.split_dividers[0].0;
        let column = divider.x + divider.width / 2;
        let row = divider.y + divider.height / 2;

        // Repaint the main window behind the detached split. Its geometry
        // overlaps the divider in terminal coordinates. For a horizontal
        // split the main window's right scrollbar is directly under it.
        let width = if direction == SplitDirection::Horizontal { column + 1 } else { 100 };
        let mut main = Terminal::new(TestBackend::new(width, 38)).unwrap();
        let mut windows = Windows::new(&app);
        windows.id = 1;
        windows.invalidate_background();
        windows.paint_main_background(&mut main, &mut app).unwrap();
        assert!(screen_text(&main).contains("main.txt"));

        let mouse = |app: &mut App, kind, column, row| {
            crate::mouse::handle_mouse(app, MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE });
        };
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), column, row);
        assert_eq!(app.detached_window_id(), Some(1), "{direction:?}");
        assert!(app.resizing_split, "{direction:?}");

        let end = match direction {
            SplitDirection::Horizontal => (column + 6, row),
            SplitDirection::Vertical => (column, row + 6),
        };
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), end.0, end.1);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), end.0, end.1);
        detached.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
        let resized = app.split_dividers[0].0;
        assert!(match direction {
            SplitDirection::Horizontal => resized.x > divider.x,
            SplitDirection::Vertical => resized.y > divider.y,
        });
        assert_eq!(app.detached_window_id(), Some(1));
        assert!(!app.resizing_split);
        assert!(app.tab_window_request.is_none());
        for (view, window) in [(first, 1), (second, 1), (main_view, MAIN_WINDOW)] {
            assert_eq!(app.tab_window(view), Some(window));
        }
    }
}

#[test]
fn a_displayed_parked_workspace_processes_git_changes_without_a_focus_event() {
    check_parked_workspace_git_changes(false);
}

#[test]
fn a_displayed_headless_workspace_processes_git_changes_without_a_focus_event() {
    check_parked_workspace_git_changes(true);
}

fn check_parked_workspace_git_changes(headless: bool) {
    let main_folder = tempfile::tempdir().unwrap();
    let detached_folder = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(main_folder.path()).unwrap();
    let filename = "main-change.txt";
    let path = main_folder.path().join(filename);
    std::fs::write(&path, "committed content\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new(filename)).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let author = git2::Signature::now("Test", "test@example.com").unwrap();
    repo.commit(Some("HEAD"), &author, &author, "Initial", &tree, &[])
        .unwrap();
    let wake = UiWake::default();
    let remote = headless.then(|| connect_headless(main_folder.path(), &wake));
    let mut app = if let Some(remote) = &remote {
        headless_app(remote, wake.clone())
    } else {
        App::new_startup(main_folder.path().to_owned(), wake.clone())
    };
    app.show_explorer = true;
    app.explorer_width = 46;
    app.sidebar_view = SidebarView::SourceControl;
    app.source_control_enabled = true;
    app.sc_graph_collapsed = true;
    app.refresh_explorer_async();
    app.start_workspace_watcher();
    app.refresh_source_control_async();
    let poll_workers = |app: &mut App| {
        app.service_workspaces_until(
            crate::ui_work::Service::Workers,
            Instant::now() + Duration::from_millis(20),
            || true,
        )
    };
    until(&wake, "workspace setup", || {
        poll_workers(&mut app);
        app.workspace_watcher_active
            && app.source_control_repository_state == SourceControlRepositoryState::Available
    });
    app.repositories.expanded = true;
    let mut windows = Windows::new(&app);
    let main_workspace = app.workspaces.active;
    windows.id = 1;
    windows.observe_main(&mut app, Some(main_workspace));
    app.insert_workspace(App::new_startup(
        detached_folder.path().to_owned(),
        wake.clone(),
    ));
    app.new_file();
    let view = app.editor().unwrap().navigation_id;
    assert!(app.open_detached_window(1, view));
    assert!(app.activate_window(1));
    app.terminal_has_focus = true;
    assert_ne!(app.workspaces.active, main_workspace);
    assert!(
        workspace(&app, main_workspace)
            .unwrap()
            .workspace_is_displayed()
    );

    let mut main = Terminal::new(TestBackend::new(120, 30)).unwrap();
    windows.invalidate_background();
    windows.paint_main_background(&mut main, &mut app).unwrap();
    assert!(
        screen_text(&main).contains("No changes"),
        "{}",
        screen_text(&main)
    );
    std::fs::write(&path, "modified outside VTCode\n").unwrap();
    let created = "added-to-explorer.txt";
    std::fs::write(main_folder.path().join(created), "new file\n").unwrap();
    until(&wake, "visible parked workspace Git refresh", || {
        if poll_workers(&mut app) {
            windows.invalidate_background();
        }
        windows.paint_main_background(&mut main, &mut app).unwrap();
        let owner = workspace(&app, main_workspace).unwrap();
        owner
            .source_control_files
            .iter()
            .any(|file| file.path == filename)
            && owner
                .explorer
                .entries
                .iter()
                .any(|entry| entry.name == created)
    });
    assert!(
        screen_text(&main).contains(filename),
        "{}",
        screen_text(&main)
    );
    assert_ne!(app.workspaces.active, main_workspace);
    assert_eq!(app.detached_window_id(), Some(1));
    assert_eq!(app.editor().unwrap().navigation_id, view);
    if let Some(remote) = &remote {
        assert!(remote.client.connected());
        remote.disconnect();
    }
}

fn connect_headless(root: &std::path::Path, wake: &UiWake) -> crate::remote::workspace::Workspace {
    let client = crate::remote::tests::connection(root, Arc::default(), Arc::default());
    client.set_wake(wake.clone());
    crate::remote::workspace::Workspace::new(client)
}

fn headless_app(remote: &crate::remote::workspace::Workspace, wake: UiWake) -> App {
    let mut app = App::new_startup(remote.view_root.clone(), wake);
    app.remote_workspace = Some(remote.clone());
    app.lsp_manager.remote_workspace = Some(remote.clone());
    app.lsp_manager.configs.clear();
    app
}

#[test]
fn terminal_tab_moves_with_the_same_input_stream() {
    let mut app = app_with_tabs(&["main.txt"]);
    app.execute_palette_command("new_terminal_in_editor_tab");
    let index = app.tabs[app.active_tab].terminal_idx.unwrap();
    let view = app.tabs[app.active_tab].editor.navigation_id;
    let mut pty = crate::terminal::TerminalEmulator::test_with_output(40, 20, "running\r\n");
    let input = pty.test_capture_input();
    app.terminals[index].terminal_pty = Some(pty);
    assert!(app.open_detached_window(3, view));
    assert!(app.activate_window(3));
    let mut screen = Terminal::new(TestBackend::new(100, 20)).unwrap();
    screen
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::from(KeyCode::Char('x')));
    assert_eq!(&*input.lock().unwrap(), b"x");
    assert!(app.return_detached_tabs(3));
    assert!(app.terminals[index].terminal_pty.is_some());
    assert_eq!(app.tab_window(view), Some(MAIN_WINDOW));
}

#[test]
fn an_outside_viewport_release_requests_the_same_view() {
    let mut app = app_with_tabs(&["one.txt", "two.txt"]);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let area = app.tab_bar_area.unwrap();
    let hits = crate::ui_chrome_tabs::TabBarHits::new(&app, area);
    let title = hits.tabs[0].1;
    let mouse = |kind, column, row| MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };
    let view = app.tabs[0].editor.navigation_id;
    crate::mouse::handle_mouse(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), title.x, title.y),
    );
    crate::mouse::handle_mouse(
        &mut app,
        mouse(MouseEventKind::Drag(MouseButton::Left), 82, 25),
    );
    crate::mouse::handle_mouse(
        &mut app,
        mouse(MouseEventKind::Up(MouseButton::Left), 82, 25),
    );
    assert!(matches!(app.tab_window_request, Some(Request::Open { view: id, .. }) if id == view));
    assert_eq!(
        app.tab_window(view),
        Some(MAIN_WINDOW),
        "move waits for the handshake"
    );
}

fn until(wake: &UiWake, stage: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let generation = wake.generation();
        if condition() {
            return;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "detached terminal timed out during {stage}"
        );
        wake.wait_for_change(generation, Some(remaining.min(Duration::from_millis(20))));
    }
}

#[test]
#[cfg_attr(
    windows,
    ignore = "requires an unsandboxed private ConPTY and named pipe"
)]
fn tab_window_frontend_presents_frames_and_routes_input_over_a_real_pty() {
    const ENDPOINT: &str = "VTCODE_TEST_DETACHED_TERMINAL_ENDPOINT";
    const TEST: &str =
        "tab_windows::tests::tab_window_frontend_presents_frames_and_routes_input_over_a_real_pty";
    if let Some(endpoint) = std::env::var_os(ENDPOINT) {
        frontend::run(endpoint).unwrap();
        return;
    }
    for move_back in [false, true] {
        let app = app_with_tabs(&["detachable-probe.txt", "detachable-probe.txt", "extra.txt"]);
        check_frontend_lifetime(TEST, ENDPOINT, app, move_back);
    }
}

#[test]
#[cfg_attr(
    windows,
    ignore = "requires an unsandboxed private ConPTY and named pipe"
)]
fn detached_headless_tabs_save_on_the_original_connection() {
    const ENDPOINT: &str = "VTCODE_TEST_DETACHED_HEADLESS_ENDPOINT";
    const TEST: &str = "tab_windows::tests::detached_headless_tabs_save_on_the_original_connection";
    if let Some(endpoint) = std::env::var_os(ENDPOINT) {
        frontend::run(endpoint).unwrap();
        return;
    }
    for move_back in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let filename = "detachable-probe.txt";
        let disk_path = directory.path().join(filename);
        std::fs::write(&disk_path, "sample text\n").unwrap();
        let wake = UiWake::default();
        let remote = connect_headless(directory.path(), &wake);
        let mut app = headless_app(&remote, wake);
        app.source_control_enabled = false;
        let key = remote.target_key(filename).unwrap();
        app.open_file(&key);
        app.wait_for_background(Duration::from_secs(10), |app| {
            app.editor().is_some_and(|editor| !editor.bg_loading)
        });
        assert_eq!(app.editor().unwrap().rope.to_string(), "sample text\n");
        app.duplicate_tab(0);
        app.new_file();
        app.active_tab = 0;

        let app = check_frontend_lifetime(TEST, ENDPOINT, app, move_back);
        assert!(
            app.remote_workspace
                .as_ref()
                .unwrap()
                .client
                .same_connection(&remote.client)
        );
        assert!(matches!(
            remote.client.call(crate::remote::Command::Ping).unwrap(),
            crate::remote::Reply::Done
        ));
        assert_eq!(
            std::fs::read_to_string(disk_path).unwrap(),
            "ZLIVE-UPDATEsample text\n"
        );
        remote.disconnect();
    }
}

fn check_frontend_lifetime(test: &str, endpoint_var: &str, mut app: App, move_back: bool) -> App {
    let view = app.tabs[0].editor.navigation_id;
    let main_view = app.tabs[1].editor.navigation_id;
    let extra = app.tabs[2].editor.navigation_id;
    let wake = app.background.wake_handle();
    let mut windows = Windows::new(&app);
    windows.windows.insert(
        1,
        Window {
            workspace: app.workspaces.active,
            source: WindowSource::Tab { view, window: MAIN_WINDOW },
            surface: None,
            launch: None,
        },
    );
    let listener = transport::Listener::bind().unwrap();
    let (mut child, master) = spawn_frontend(test, endpoint_var, listener.endpoint());
    let mut reader = master.try_clone_reader().unwrap();
    let mut writer = master.take_writer().unwrap();
    let output_wake = wake.clone();
    let (output_tx, output_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = [0; 8192];
        while let Ok(read) = reader.read(&mut bytes) {
            if read == 0 || output_tx.send(bytes[..read].to_vec()).is_err() {
                break;
            }
            output_wake.notify();
        }
    });
    let server_wake = wake.clone();
    let send_wake = wake.clone();
    let notices = windows.notices.clone();
    let server = std::thread::spawn(move || {
        let notify = move |notice| {
            notices
                .send((1, notice))
                .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
            send_wake.notify();
            Ok(())
        };
        let result = frontend::accept(
            listener,
            notify.clone(),
            server_wake,
            Arc::new(transport::Opening::new().unwrap()),
        );
        if let Err(error) = &result {
            let _ = notify(frontend::Notice::Closed(Err(io::Error::other(
                error.to_string(),
            ))));
        }
        result
    });

    assert_eq!(app.tab_window(view), Some(MAIN_WINDOW));
    let mut output = Vec::new();
    until(&wake, "handshake", || {
        output.extend(output_rx.try_iter().flatten());
        if let Some(status) = child.0.try_wait().unwrap() {
            panic!(
                "frontend exited during handshake ({status:?}): {}",
                String::from_utf8_lossy(&output)
            );
        }
        windows.poll(&mut app);
        assert!(
            windows.windows.contains_key(&1),
            "{}\n{}",
            app.status_message,
            String::from_utf8_lossy(&output)
        );
        app.tab_window(view) == Some(1)
    });
    let mut screen = crate::terminal::TermGrid::new(100, 28);
    let mut parser = vte::Parser::new();
    let mut screen_contains = |text: &str| {
        for byte in output.drain(..).chain(output_rx.try_iter().flatten()) {
            parser.advance(&mut screen, byte);
        }
        screen.cells.iter().any(|row| {
            row.iter()
                .map(|cell| cell.ch)
                .collect::<String>()
                .contains(text)
        })
    };
    until(&wake, "first frame", || {
        windows.paint_background(&mut app);
        screen_contains("sample text")
    });
    assert!(windows.is_main());
    app.active_tab = app.tab_location(main_view).unwrap().1;
    app.terminal_has_focus = true;
    for ch in "LIVE-UPDATE".chars() {
        app.editor_mut().unwrap().insert_char(ch);
    }
    app.sync_duplicate_file_tabs();
    let (group, tab) = app.tab_location(view).unwrap();
    assert_eq!(
        app.groups[group].tabs[tab].editor.rope.to_string(),
        "LIVE-UPDATEsample text\n"
    );
    windows.poll(&mut app);
    until(&wake, "background update", || {
        windows.paint_background(&mut app);
        screen_contains("LIVE-UPDATEsample text")
    });
    assert!(windows.is_main());

    // VT input is sent only to the private process created by this test.
    writer.write_all(b"Z").unwrap();
    writer.flush().unwrap();
    let (_main, main_rx) = mpsc::channel();
    let mut main = TerminalEventPump::from_receiver(main_rx);
    until(&wake, "typing", || {
        let Some((source, Event::Key(key))) = windows.next_event(&mut main).unwrap() else {
            return false;
        };
        assert_eq!(source, 1);
        assert!(windows.accept_focus(&mut app, source, &Event::Key(key)));
        crate::app_event_loop::handle_keyboard_input(&mut app, windows.active_pump(&mut main), key);
        app.editor()
            .is_some_and(|editor| editor.rope.get_char(0) == Some('Z'))
    });
    if app.remote_workspace.is_some() {
        app.save_current_file();
        app.wait_for_background(Duration::from_secs(10), |app| {
            app.pending_file_saves.is_empty() && app.editor().is_some_and(|editor| !editor.modified)
        });
        assert!(app.editor().is_some_and(|editor| !editor.modified));
    }
    assert!(app.move_tab_to_window(extra, 1));
    app.close_tab(app.active_tab);
    windows.poll(&mut app);
    assert!(windows.windows.contains_key(&1));
    assert_eq!(app.tab_window(view), Some(1));
    assert!(child.0.try_wait().unwrap().is_none());

    if move_back {
        assert!(windows.request(
            &mut app,
            Request::Move {
                view,
                window: MAIN_WINDOW
            }
        ));
        assert_eq!(app.tab_window(view), Some(MAIN_WINDOW));
    } else {
        app.close_tab(app.active_tab);
        assert_eq!(app.tab_window(view), None);
    }
    assert!(windows.poll(&mut app));
    assert!(windows.windows.is_empty());
    assert!(windows.is_main());
    assert_eq!(
        app.editor().unwrap().rope.to_string(),
        "ZLIVE-UPDATEsample text\n"
    );
    until(&wake, "shutdown", || {
        child.0.try_wait().unwrap().is_some_and(|status| {
            assert!(status.success(), "frontend shutdown returned {status:?}");
            true
        })
    });
    drop(writer);
    assert!(server.join().unwrap().is_ok());
    app
}

fn spawn_frontend(
    test: &str,
    endpoint_var: &str,
    endpoint: OsString,
) -> (PtyChild, Box<dyn portable_pty::MasterPty + Send>) {
    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            cols: 100,
            rows: 28,
            pixel_height: 0,
            pixel_width: 0,
        })
        .unwrap();
    let mut command = portable_pty::CommandBuilder::new(std::env::current_exe().unwrap());
    command.args([
        "--exact",
        test,
        "--include-ignored",
        "--nocapture",
        "--test-threads=1",
    ]);
    command.env(endpoint_var, endpoint);
    command.env("TERM_PROGRAM", "vtcode");
    let child = PtyChild(pair.slave.spawn_command(command).unwrap());
    (child, pair.master)
}

#[cfg(target_os = "linux")]
#[test]
fn a_detached_frontend_exits_even_when_the_terminal_stops_draining_output() {
    const ENDPOINT: &str = "VTCODE_TEST_STALLED_WINDOW_ENDPOINT";
    const TEST: &str = "tab_windows::tests::a_detached_frontend_exits_even_when_the_terminal_stops_draining_output";
    if let Some(endpoint) = std::env::var_os(ENDPOINT) {
        frontend::run(endpoint).unwrap();
        return;
    }
    let listener = transport::Listener::bind().unwrap();
    let (mut child, _unread_pty) = spawn_frontend(TEST, ENDPOINT, listener.endpoint());
    let (send, receive) = mpsc::channel();
    let wake = UiWake::default();
    let server_wake = wake.clone();
    let server = std::thread::spawn(move || {
        frontend::accept(
            listener,
            move |notice| {
                send.send(notice)
                    .map_err(|_| io::ErrorKind::BrokenPipe.into())
            },
            server_wake,
            Arc::new(transport::Opening::new().unwrap()),
        )
    });
    let frontend::Notice::Ready(peer) = receive.recv_timeout(Duration::from_secs(5)).unwrap()
    else {
        panic!("detached terminal did not start");
    };
    for _ in 0..8 {
        let generation = wake.generation();
        if !peer.output.ready.swap(false, Ordering::AcqRel) {
            wake.wait_for_change(generation, Some(Duration::from_millis(250)));
            if !peer.output.ready.swap(false, Ordering::AcqRel) {
                break;
            }
        }
        peer.output
            .frames
            .try_send(vec![b'x'; 1024 * 1024])
            .unwrap();
    }
    assert!(!peer.output.ready.load(Ordering::Acquire));
    drop(peer);
    let deadline = Instant::now() + Duration::from_secs(4);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "the backpressured detached terminal lingered"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success());
    assert!(server.join().unwrap().is_ok());
}

#[test]
#[cfg_attr(windows, ignore = "requires an unsandboxed private named pipe")]
fn closing_a_source_tab_cancels_a_launch_even_during_a_partial_handshake() {
    let mut app = app_with_tabs(&["pending.txt"]);
    let view = app.tabs[0].editor.navigation_id;
    let mut windows = Windows::new(&app);
    let listener = transport::Listener::bind().unwrap();
    let endpoint = listener.endpoint();
    let opening = Arc::new(transport::Opening::new().unwrap());
    windows.windows.insert(
        1,
        Window {
            workspace: app.workspaces.active,
            source: WindowSource::Tab { view, window: MAIN_WINDOW },
            surface: None,
            launch: Some(frontend::Launch(opening.clone())),
        },
    );
    let (accepted, handshake_started) = mpsc::channel();
    let (done, result) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut reader = listener.accept(&opening).unwrap();
        accepted.send(()).unwrap();
        done.send(transport::read_handshake(
            &mut reader,
            &opening,
            &mut Vec::new(),
        ))
        .unwrap();
    });
    let mut peer = transport::connect(&endpoint).unwrap();
    peer.write_all(&[16, 0]).unwrap();
    handshake_started
        .recv_timeout(Duration::from_secs(3))
        .unwrap();
    app.close_tab(0);
    assert!(windows.poll(&mut app));
    assert!(windows.windows.is_empty());
    assert_eq!(
        result
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap_err()
            .kind(),
        io::ErrorKind::ConnectionAborted
    );
    worker.join().unwrap();
    drop(peer);
    // A launcher that starts after the owner retires it exits successfully.
    frontend::run(endpoint).unwrap();
}

#[test]
fn failed_start_keeps_the_dirty_tab_in_its_original_window() {
    let mut app = app_with_tabs(&["original.txt"]);
    let view = app.tabs[0].editor.navigation_id;
    app.tabs[0].editor.insert_char('X');
    let mut windows = Windows::new(&app);
    windows.windows.insert(
        1,
        Window {
            workspace: app.workspaces.active,
            source: WindowSource::Tab { view, window: MAIN_WINDOW },
            surface: None,
            launch: None,
        },
    );
    windows
        .notices
        .send((
            1,
            frontend::Notice::Closed(Err(io::Error::other("launcher failed"))),
        ))
        .unwrap();
    assert!(windows.poll(&mut app));
    assert!(windows.windows.is_empty());
    assert_eq!(app.tab_window(view), Some(MAIN_WINDOW));
    assert!(app.tabs[app.active_tab].has_unsaved_changes());
    assert_eq!(app.editor().unwrap().rope.to_string(), "Xsample text\n");
}

struct PtyChild(Box<dyn portable_pty::Child + Send + Sync>);

impl Drop for PtyChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}
