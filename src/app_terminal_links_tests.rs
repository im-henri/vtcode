use super::*;
use crate::{app::{Tab, TabKind}, editor::Editor, terminal::TerminalEmulator};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use std::{path::Path, time::Duration};

fn fixture(output: &str) -> App {
    let mut app = App::test_new();
    app.source_control_enabled = false;
    app.tabs = vec![Tab::new_file(Editor::test_with_text("origin\n"))];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.show_terminal_sidebar = true;
    app.terminal_panel_focused = true;
    app.terminals[0].terminal_pty = Some(TerminalEmulator::test_with_output(200, 8, output));
    app.terminal_surfaces = vec![TerminalSurface { index: 0, area: Rect::new(2, 3, 200, 8), group: None }];
    app
}

fn event(kind: MouseEventKind, modifiers: KeyModifiers, column: u16) -> MouseEvent {
    MouseEvent { kind, modifiers, column: column + 2, row: 3 }
}

fn click(app: &mut App, column: u16, modifiers: KeyModifiers) {
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
        crate::mouse::handle_mouse(app, event(kind, modifiers, column));
    }
}

#[test]
fn ctrl_click_opens_a_local_diagnostic_at_the_exact_line_and_column_in_docks_and_tabs() {
    for detached in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.c");
        std::fs::write(&path, ("x".repeat(80) + "\n").repeat(5000)).unwrap();
        let mut app = fixture("Error in \"source.c\", line 4755, column 49: Undeclared variable.");
        app.explorer = crate::explorer::FileExplorer::new_deferred(directory.path().into());
        app.terminals[0].terminal_cwd = directory.path().into();
        if detached {
            assert!(app.detach_terminal_to_group(0, app.active_group));
            app.terminal_surfaces[0].group = Some(app.active_group);
        }
        let terminal = app.terminals[0].terminal_pty.as_mut().unwrap();
        terminal.grid.mouse_tracking = 1003;
        let input = terminal.test_capture_input();
        click(&mut app, 14, KeyModifiers::CONTROL);
        assert!(!app.terminal_links.captured);
        assert!(app.terminal_selection_drag.is_none());
        assert!(app.terminal_mouse_capture.is_none());
        assert!(input.lock().unwrap().is_empty(), "link presses/releases must not reach the child");
        app.wait_for_background(Duration::from_secs(10), |app| app.editor().is_some_and(|editor|
            !editor.bg_loading && editor.file_path.as_deref().is_some_and(|other| App::same_open_path(other, &path))));
        assert_eq!(app.tabs[app.active_tab].kind, TabKind::File);
        assert!(App::same_open_path(app.editor().unwrap().file_path.as_ref().unwrap(), &path));
        assert_eq!((app.editor().unwrap().cursor_line, app.editor().unwrap().cursor_col), (4754, 48));
        assert!(!app.terminal_panel_focused);
        if detached { assert!(app.tabs.iter().any(|tab| tab.kind == TabKind::Terminal)); }
    }
}

#[test]
fn ordinary_terminal_clicks_and_selection_modifiers_do_not_open_files() {
    for modifiers in [KeyModifiers::NONE, KeyModifiers::SHIFT, KeyModifiers::CONTROL | KeyModifiers::SHIFT] {
        let mut app = fixture("source.c:12:7");
        click(&mut app, 4, modifiers);
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.editor().unwrap().rope.to_string(), "origin\n");
        assert_eq!(app.terminal_links.cache.scans, 0);
    }
}

#[test]
fn ctrl_hover_is_a_cached_frame_overlay_and_idle_frames_do_no_detection() {
    let mut app = fixture("Error in \"src/source.c\", line 12, column 7");
    app.mouse_col = 16;
    app.mouse_row = 3;
    let surface = app.terminal_surfaces[0];
    let mut terminal = Terminal::new(TestBackend::new(204, 12)).unwrap();
    let draw = |terminal: &mut Terminal<TestBackend>, app: &mut App| {
        app.terminal_links.begin_frame();
        terminal.draw(|frame| {
            crate::ui_helpers::draw_terminal_emulator(frame, app.terminals[0].terminal_pty.as_ref().unwrap(),
                surface.area, &app.colors.ui, true);
            super::draw(frame, app, surface);
        }).unwrap();
    };
    for _ in 0..100 { draw(&mut terminal, &mut app); }
    assert_eq!(app.terminal_links.cache.scans, 0);
    app.update_link_hover_modifiers(KeyModifiers::CONTROL);
    for _ in 0..100 { draw(&mut terminal, &mut app); }
    assert_eq!(app.terminal_links.cache.scans, 1);
    assert!(terminal.backend().buffer()[(16, 3)].modifier.contains(Modifier::UNDERLINED));
    assert!(!app.terminals[0].terminal_pty.as_ref().unwrap().grid.display_row(0).unwrap()[14].underline);
    app.update_link_hover_modifiers(KeyModifiers::NONE);
    draw(&mut terminal, &mut app);
    assert!(!terminal.backend().buffer()[(16, 3)].modifier.contains(Modifier::UNDERLINED));
}

#[test]
fn shell_directory_hints_resolve_relative_links_without_filesystem_queries() {
    let directory = tempfile::tempdir().unwrap();
    let nested = directory.path().join("my dir #1;100%");
    std::fs::create_dir(&nested).unwrap();
    let uri = url::Url::from_directory_path(&nested).unwrap();
    let mut app = fixture(&format!("\x1b]7;{uri}\x07source.c:3:4"));
    app.terminals[0].terminal_cwd = directory.path().into();
    assert_eq!(resolve_path(&app, 0, "source.c").unwrap(), nested.join("source.c"));
    assert_eq!(resolve_path(&app, 0, "../other.c").unwrap(), nested.join("../other.c"));
    assert_eq!(app.terminal_links.cache.scans, 0);
    assert!(Path::new(&resolve_path(&app, 0, "source.c").unwrap()).is_absolute());
}

#[cfg(windows)]
#[test]
fn windows_bash_directory_hints_map_drives_and_preserve_escaped_names() {
    for uri in ["file:///c/work/my%20dir%20%231", "file:///cygdrive/c/work/my%20dir%20%231"] {
        assert_eq!(local_directory(uri), Some(PathBuf::from(r"c:\work\my dir #1")));
    }
}

#[test]
fn directory_links_expand_select_and_scroll_the_workspace_explorer_from_docks_and_tabs() {
    for detached in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let shell = root.path().join("shell");
        let target = root.path().join("z-last/資料 folder");
        std::fs::create_dir(&shell).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("child.rs"), "child\n").unwrap();
        for index in 0..30 {
            std::fs::create_dir(root.path().join(format!("a-{index:02}"))).unwrap();
        }
        let mut app = fixture("See \"../z-last/資料 folder/\"");
        app.terminals[0].terminal_cwd = shell;
        app.explorer = crate::explorer::FileExplorer::new_deferred(root.path().into());
        app.explorer.visible_height = 5;
        app.explorer.find.open = true;
        app.explorer.find.query = "unrelated".into();
        app.explorer.find.filter = true;
        app.explorer.prepare_find();
        app.show_explorer = false;
        app.sidebar_view = crate::app::SidebarView::SourceControl;
        if detached {
            assert!(app.detach_terminal_to_group(0, app.active_group));
            app.terminal_surfaces[0].group = Some(app.active_group);
        }
        let tabs = app.tabs.len();
        click(&mut app, 8, KeyModifiers::CONTROL);
        app.wait_for_background(Duration::from_secs(10), |app| app.explorer.entries.get(app.explorer.selected)
            .is_some_and(|entry| entry.path == target && entry.expanded));
        assert_eq!(app.explorer.root, root.path());
        assert_eq!(app.tabs.len(), tabs);
        assert!(app.show_explorer);
        assert_eq!(app.sidebar_view, crate::app::SidebarView::Explorer);
        assert_eq!(app.active_pane, ActivePane::Explorer);
        assert!(!app.terminal_panel_focused);
        assert!(!app.explorer.find.filter);
        assert_eq!(app.explorer.find.query, "unrelated");
        assert!(app.explorer.scroll_offset > 0);
        assert!(app.explorer.viewport_indices(5).contains(&app.explorer.selected));
        assert!(!app.side_notice.visible());
    }
}

fn failure(app: &mut App) {
    app.wait_for_background(Duration::from_secs(10), |app| app.side_notice.visible());
    assert!(app.side_notice.visible());
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.editor().unwrap().rope.to_string(), "origin\n");
    assert!(app.terminal_panel_focused);
    assert_eq!(app.active_pane, ActivePane::Editor);
}

#[test]
fn outside_directory_is_rejected_with_a_small_explanatory_notice_and_success_clears_it() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(root.path().join("workspace-other")).unwrap();
    let mut app = fixture("workspace-other/");
    app.terminals[0].terminal_cwd = root.path().into();
    app.explorer = crate::explorer::FileExplorer::new_deferred(workspace.clone());
    app.show_explorer = false;
    click(&mut app, 4, KeyModifiers::CONTROL);
    failure(&mut app);
    assert!(!app.show_explorer);
    assert_eq!(app.explorer.root, workspace);
    assert!(app.notification_history.last().unwrap().0.contains("outside the current workspace"));
    let mut output = Terminal::new(TestBackend::new(90, 14)).unwrap();
    app.status_bar_area = Some(Rect::new(0, 13, 90, 1));
    output.draw(|frame| crate::ui_side_notice::draw(frame, &mut app)).unwrap();
    let text: String = output.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
    assert!(text.contains("Cannot open terminal path"));
    assert!(text.contains("outside the current workspace"));
    assert!(text.contains("Details"));
    assert_eq!(app.side_notice.area.unwrap().height, 5);
    app.terminals[0].terminal_pty = Some(TerminalEmulator::test_with_output(200, 8, "workspace/"));
    click(&mut app, 4, KeyModifiers::CONTROL);
    app.wait_for_background(Duration::from_secs(10), |app| app.active_pane == ActivePane::Explorer);
    assert_eq!(app.active_pane, ActivePane::Explorer);
    assert!(!app.side_notice.visible());
    assert_eq!(app.explorer.root, workspace);
}

#[test]
fn missing_terminal_paths_report_the_reason_without_creating_empty_file_tabs() {
    for path in ["missing.rs", "missing/folder/"] {
        let root = tempfile::tempdir().unwrap();
        let mut app = fixture(path);
        app.terminals[0].terminal_cwd = root.path().into();
        app.explorer = crate::explorer::FileExplorer::new_deferred(root.path().into());
        click(&mut app, 3, KeyModifiers::CONTROL);
        failure(&mut app);
        let message = &app.notification_history.last().unwrap().0;
        assert!(message.contains("Path does not exist"), "{message}");
        assert!(message.contains(path.trim_end_matches('/')), "{message}");
    }
}

#[test]
fn a_folder_excluded_from_the_explorer_has_visible_failure_feedback() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".git")).unwrap();
    let mut app = fixture("./.git/");
    app.terminals[0].terminal_cwd = root.path().into();
    app.explorer = crate::explorer::FileExplorer::new_deferred(root.path().into());
    click(&mut app, 4, KeyModifiers::CONTROL);
    app.wait_for_background(Duration::from_secs(10), |app| app.side_notice.visible());
    assert!(app.side_notice.visible());
    assert!(app.notification_history.last().unwrap().0.contains("excluded"));
    assert!(app.terminal_panel_focused);
    assert_eq!(app.active_pane, ActivePane::Editor);
    assert_eq!(app.explorer.root, root.path());
    assert_eq!(app.tabs.len(), 1);
}
