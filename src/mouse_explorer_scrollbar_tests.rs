use super::*;
use crate::app::{SidebarView, Tab};
use crate::editor::Editor;
use crate::explorer::{FileEntry, GitStatus, GitStatusSummary};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn fixture(sidebar_right: bool, show_open_editors: bool) -> App {
    let mut app = App::test_new();
    app.show_explorer = true;
    app.sidebar_view = SidebarView::Explorer;
    app.sidebar_right = sidebar_right;
    app.show_open_editors = show_open_editors;
    app.explorer_width = 30;
    app.show_panel = false;
    app.tabs = (0..12).map(|_| Tab::new_file(Editor::new())).collect();
    app.explorer.entries = (0..80)
        .map(|index| {
            let name = format!("file{index:02}.rs");
            FileEntry {
                path: app.explorer.root.join(&name),
                name,
                is_dir: false,
                depth: 0,
                expanded: false,
                git_status: GitStatus::Unmodified,
                git_status_summary: GitStatusSummary::default(),
            }
        })
        .collect();
    app
}

fn draw(app: &mut App, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(100, height)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, app))
        .unwrap()
        .buffer
        .clone()
}

// Find the actual painted track, including the workbench's separator pass.
fn scrollbar(buffer: &Buffer, app: &App) -> Rect {
    let sidebar = app.explorer_area.unwrap();
    let x = sidebar.right() - if app.sidebar_right { 1 } else { 2 };
    let first_file = (sidebar.y..sidebar.bottom())
        .find(|&y| {
            (sidebar.x..sidebar.right())
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .contains("file")
        })
        .expect("visible Explorer file row");
    let rows: Vec<_> = (first_file..sidebar.bottom())
        .filter(|&y| buffer[(x, y)].symbol() == "▐")
        .collect();
    let top = *rows.first().expect("visible Explorer scrollbar");
    Rect::new(x, top, 1, rows.len() as u16)
}

fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
    handle_mouse(
        app,
        event::MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        },
    );
}

#[test]
fn explorer_scrollbar_thumb_drag_keeps_resize_and_file_actions_separate() {
    for sidebar_right in [false, true] {
        for show_open_editors in [false, true] {
            let mut app = fixture(sidebar_right, show_open_editors);
            app.explorer.scroll_offset = 13;
            let buffer = draw(&mut app, 40);
            let bar = scrollbar(&buffer, &app);
            let sidebar = app.explorer_area.unwrap();
            let seam = if sidebar_right {
                sidebar.x
            } else {
                sidebar.right() - 1
            };
            let thumb_color = app
                .colors
                .ui
                .scrollbar_colors(app.colors.bg_sidebar, false, false)
                .1;
            let grab_row = (bar.y..bar.bottom())
                .rfind(|&y| buffer[(bar.x, y)].fg == thumb_color)
                .expect("visible scrollbar thumb");
            assert_ne!(bar.x, seam);
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                bar.x,
                grab_row,
            );
            assert_eq!(
                app.explorer.scroll_offset, 13,
                "grabbing the thumb must not jump"
            );
            assert_eq!(
                app.explorer.selected, 0,
                "scrollbar clicks must not select files"
            );
            assert_eq!(app.tabs.len(), 12, "scrollbar clicks must not open files");
            assert!(app.dragging_explorer_path.is_none());
            assert!(!app.resizing_explorer);

            // Crossing the resize edge must keep the scrollbar gesture captured.
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                seam,
                grab_row,
            );
            assert_eq!(
                app.explorer.scroll_offset, 13,
                "horizontal movement must not jump the rounded thumb"
            );
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                seam,
                grab_row + 2,
            );
            assert!(app.explorer.scroll_offset > 13);
            assert!(app.explorer.scroll_offset < 80 - bar.height as usize);
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                seam,
                1000,
            );
            assert_eq!(app.explorer.scroll_offset, 80 - bar.height as usize);
            assert_eq!(app.explorer_width, 30);
            let buffer = draw(&mut app, 40);
            let last_row: String = (sidebar.x..sidebar.right())
                .map(|x| buffer[(x, bar.bottom() - 1)].symbol())
                .collect();
            assert!(last_row.contains("file79.rs"));

            // Capture also survives leaving the sidebar and releasing over chrome.
            mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 50, 0);
            assert_eq!(app.explorer.scroll_offset, 0);
            assert_eq!(app.explorer_width, 30);
            mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 50, 0);
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                bar.x,
                1000,
            );
            assert_eq!(
                app.explorer.scroll_offset, 0,
                "release ends scrollbar capture"
            );

            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                seam,
                bar.y,
            );
            assert!(
                app.resizing_explorer,
                "the separate resize edge stays usable"
            );
            let resized_edge = if sidebar_right { seam - 4 } else { seam + 4 };
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                resized_edge,
                bar.y,
            );
            assert_eq!(app.explorer_width, 34);
            assert_eq!(app.explorer.scroll_offset, 0);
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                resized_edge,
                bar.y,
            );
            assert!(!app.resizing_explorer);
        }
    }
}

#[test]
fn explorer_scrollbar_track_clicks_follow_layout_and_content_changes() {
    for sidebar_right in [false, true] {
        let mut app = fixture(sidebar_right, true);
        for (height, show_open_editors) in [(40, true), (24, true), (24, false)] {
            app.show_open_editors = show_open_editors;
            let buffer = draw(&mut app, height);
            let bar = scrollbar(&buffer, &app);
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                bar.x,
                bar.bottom() - 1,
            );
            assert_eq!(app.explorer.scroll_offset, 80 - bar.height as usize);
            assert_eq!(app.explorer_width, 30);
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                bar.x,
                bar.bottom() - 1,
            );
            draw(&mut app, height);
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                bar.x,
                bar.y,
            );
            assert_eq!(app.explorer.scroll_offset, 0);
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                bar.x,
                bar.y,
            );
            assert_eq!(app.explorer.selected, 0);
            assert_eq!(app.tabs.len(), 12);
        }

        let old_bar = scrollbar(&draw(&mut app, 24), &app);
        app.explorer.entries.truncate(2);
        let buffer = draw(&mut app, 24);
        assert!((old_bar.y..old_bar.bottom()).all(|y| buffer[(old_bar.x, y)].symbol() != "▐"));
        handle_mouse(
            &mut app,
            event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: old_bar.x,
                row: old_bar.y + 1,
                modifiers: KeyModifiers::CONTROL,
            },
        );
        assert_eq!(
            app.explorer.selected, 1,
            "without overflow the old track is file content"
        );
        assert!(!app.resizing_explorer);
    }
}
