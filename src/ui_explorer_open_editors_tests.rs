use super::*;
use crate::app::{SidebarView, Tab};
use crate::editor::Editor;
use crate::explorer::FileEntry;
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn fixture(count: usize) -> App {
    let mut app = App::test_new();
    app.sidebar_view = SidebarView::Explorer;
    app.show_open_editors = true;
    app.tabs = (0..count)
        .map(|index| {
            let mut editor = Editor::new();
            editor.file_path = Some(format!("open-{index:02}.rs").into());
            Tab::new_file(editor)
        })
        .collect();
    app.explorer.entries = (0..20)
        .map(|index| {
            let name = format!("tree-{index:02}.rs");
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

fn draw(app: &mut App, area: Rect) -> Buffer {
    app.explorer_area = Some(area);
    let mut terminal = Terminal::new(TestBackend::new(60, 45)).unwrap();
    terminal
        .draw(|frame| draw_explorer(frame, app, area))
        .unwrap()
        .buffer
        .clone()
}

fn row_text(buffer: &Buffer, area: Rect, row: u16) -> String {
    (area.x..area.right())
        .map(|x| buffer.cell((x, row)).unwrap().symbol())
        .collect()
}

fn row_for(buffer: &Buffer, area: Rect, label: &str) -> u16 {
    (area.y..area.bottom())
        .find(|&row| row_text(buffer, area, row).contains(label))
        .unwrap_or_else(|| panic!("{label} should be visible"))
}

fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        },
    );
}

#[test]
fn explorer_open_editors_grows_past_five_and_keeps_tree_pointer_actions_aligned() {
    for sidebar_right in [false, true] {
        let area = Rect::new(5, 2, 40, 30);
        let mut app = fixture(0);
        app.sidebar_right = sidebar_right;
        for count in 1..=12 {
            let mut editor = Editor::new();
            editor.file_path = Some(format!("open-{:02}.rs", count - 1).into());
            app.tabs.push(Tab::new_file(editor));
            let buffer = draw(&mut app, area);
            for index in 0..count {
                assert_eq!(
                    row_for(&buffer, area, &format!("open-{index:02}.rs")),
                    area.y + 2 + index as u16
                );
            }
            let last_row = row_for(&buffer, area, &format!("open-{:02}.rs", count - 1));
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                area.x + 8,
                last_row,
            );
            assert_eq!(app.active_tab, count - 1);
            assert_eq!(
                row_for(&buffer, area, "tree-00.rs"),
                area.y + 3 + count as u16
            );
        }

        let buffer = draw(&mut app, area);
        let tree_row = row_for(&buffer, area, "tree-01.rs");
        mouse(&mut app, MouseEventKind::Moved, area.x + 8, tree_row);
        assert_eq!(app.hovered_explorer, Some(1));
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Right),
            area.x + 8,
            tree_row,
        );
        assert_eq!(
            app.explorer_context_target,
            Some(app.explorer.entries[1].path.clone())
        );
        app.explorer_context_active = false;
        app.dragging_explorer_path = Some(app.explorer.entries[0].path.clone());
        crate::mouse_drag::handle_left_drag(
            &mut app,
            area.x + 8,
            tree_row,
            area.x + 8,
            tree_row - 1,
            KeyModifiers::NONE,
        );
        assert_eq!(app.explorer_drop_target_idx, Some(1));
        app.dragging_explorer_path = None;
        crate::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: area.x + 8,
                row: tree_row,
                modifiers: KeyModifiers::CONTROL,
            },
        );
        assert_eq!(app.explorer.selected, 1);

        let close_row = row_for(&buffer, area, "open-10.rs");
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            area.right() - 2,
            close_row,
        );
        assert_eq!(app.tabs.len(), 11);
        assert!(
            app.tabs
                .iter()
                .all(|tab| tab.editor.file_name() != "open-10.rs")
        );
        let buffer = draw(&mut app, area);
        assert_eq!(row_for(&buffer, area, "tree-00.rs"), area.y + 14);
    }
}

#[test]
fn explorer_open_editors_overflow_scrolls_independently_and_targets_visible_tabs() {
    let area = Rect::new(5, 2, 40, 12);
    let mut app = fixture(18);
    app.active_tab = 17;
    let buffer = draw(&mut app, area);
    let last_row = row_for(&buffer, area, "open-17.rs");
    let tree_row = row_for(&buffer, area, "tree-00.rs");
    assert!(last_row < tree_row);
    assert!(row_text(&buffer, area, area.y + 1).contains("/18"));

    // Manual scrolling must survive redraws even while the active tab is offscreen.
    for _ in 0..20 {
        mouse(&mut app, MouseEventKind::ScrollUp, area.x + 8, area.y + 2);
        draw(&mut app, area);
    }
    let buffer = draw(&mut app, area);
    assert_eq!(app.open_editors_scroll_offset, 0);
    row_for(&buffer, area, "open-00.rs");
    assert_eq!(app.active_tab, 17);
    assert_eq!(app.explorer.scroll_offset, 0);

    for _ in 0..20 {
        mouse(&mut app, MouseEventKind::ScrollDown, area.x + 8, area.y + 2);
        draw(&mut app, area);
    }
    let buffer = draw(&mut app, area);
    row_for(&buffer, area, "open-17.rs");
    let offset = app.open_editors_scroll_offset;
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        area.x + 8,
        area.y + 2,
    );
    assert_eq!(app.active_tab, offset);
    mouse(
        &mut app,
        MouseEventKind::Moved,
        area.right() - 4,
        area.y + 3,
    );
    let buffer = draw(&mut app, area);
    let close_col = (area.x..area.right())
        .find(|&x| buffer[(x, area.y + 3)].symbol() == "×")
        .expect("visible close control beside the scrollbar");
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        close_col,
        area.y + 3,
    );
    assert_eq!(app.tabs.len(), 17);
    assert!(
        app.tabs
            .iter()
            .all(|tab| tab.editor.file_name() != format!("open-{:02}.rs", offset + 1))
    );
    draw(&mut app, area);
    let offset = app.open_editors_scroll_offset;
    mouse(&mut app, MouseEventKind::ScrollDown, area.x + 8, tree_row);
    assert!(app.explorer.scroll_offset > 0);
    assert_eq!(app.open_editors_scroll_offset, offset);

    // A newly selected tab is revealed after manually scrolling away from it.
    mouse(&mut app, MouseEventKind::ScrollUp, area.x + 8, area.y + 2);
    app.active_tab = 0;
    let buffer = draw(&mut app, area);
    row_for(&buffer, area, "open-00.rs");
    assert_eq!(app.open_editors_scroll_offset, 0);

    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        area.x + 8,
        area.y + 1,
    );
    let buffer = draw(&mut app, area);
    assert!(!app.show_open_editors);
    assert!((area.y..area.bottom()).all(|row| !row_text(&buffer, area, row).contains("open-")));
    app.explorer.scroll_offset = 0;
    let buffer = draw(&mut app, area);
    assert_eq!(row_for(&buffer, area, "tree-00.rs"), area.y + 3);
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        area.x + 8,
        area.y + 1,
    );
    let buffer = draw(&mut app, area);
    row_for(&buffer, area, "open-00.rs");

    app.tabs.truncate(2);
    let buffer = draw(&mut app, area);
    row_for(&buffer, area, "open-01.rs");
    assert!(!row_text(&buffer, area, area.y + 1).contains('/'));
    assert_eq!(row_for(&buffer, area, "tree-00.rs"), area.y + 5);
}

#[test]
fn explorer_open_editors_resize_keeps_active_tab_visible_and_small_surfaces_contained() {
    let mut app = fixture(18);
    app.active_tab = 17;
    for height in [30, 12, 6, 40, 4, 3, 2, 1, 0] {
        for width in [40, 1, 0] {
            let area = Rect::new(5, 2, width, height);
            let buffer = draw(&mut app, area);
            if height >= 4 && width == 40 {
                row_for(&buffer, area, "open-17.rs");
            }
            for y in 0..45 {
                for x in 0..60 {
                    if !area.contains((x, y).into()) {
                        assert_eq!(buffer.cell((x, y)).unwrap().symbol(), " ");
                    }
                }
            }
        }
    }
}

fn workbench(app: &mut App) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, app))
        .unwrap()
        .buffer
        .clone()
}

#[test]
fn explorer_open_editors_scrollbar_is_visible_and_draggable_without_closing_tabs() {
    for sidebar_right in [false, true] {
        let mut app = fixture(40);
        app.sidebar_right = sidebar_right;
        app.explorer_width = 32;
        app.show_panel = false;
        app.active_tab = 39;
        let buffer = workbench(&mut app);
        let area = app.explorer_area.unwrap();
        let rows: Vec<_> = (area.y..area.bottom())
            .filter(|&row| row_text(&buffer, area, row).contains("open-"))
            .collect();
        let bar_x = area.right() - if sidebar_right { 1 } else { 2 };
        let bar = Rect::new(bar_x, rows[0], 1, rows.len() as u16);
        let seam = if sidebar_right {
            area.x
        } else {
            area.right() - 1
        };
        assert!(
            rows.iter().all(|&row| buffer[(bar.x, row)].symbol() == "▐"),
            "every visible Open Editors row must have a scrollbar track"
        );
        assert_ne!(bar.x, seam);

        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            bar.x,
            bar.y,
        );
        assert_eq!(app.open_editors_scroll_offset, 0);
        assert_eq!(app.active_tab, 39);
        assert_eq!(app.tabs.len(), 40);
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            bar.x,
            bar.y,
        );
        let buffer = workbench(&mut app);
        row_for(&buffer, area, "open-00.rs");
        assert_ne!(buffer[(bar.x, bar.y)].fg, buffer[(bar.x, bar.y)].bg);
        for _ in 0..3 {
            mouse(&mut app, MouseEventKind::ScrollDown, bar.x, bar.y);
        }
        let buffer = workbench(&mut app);
        assert!(app.open_editors_scroll_offset > 0);
        assert_eq!(
            buffer[(bar.x, bar.y)].fg,
            buffer[(bar.x, bar.y)].bg,
            "the thumb must move with wheel scrolling"
        );
        let grab = (bar.y..bar.bottom())
            .rfind(|&row| buffer[(bar.x, row)].fg != buffer[(bar.x, row)].bg)
            .expect("visible Open Editors thumb");
        let offset = app.open_editors_scroll_offset;
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            bar.x,
            grab,
        );
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            seam,
            grab,
        );
        assert_eq!(app.open_editors_scroll_offset, offset);
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            seam,
            1000,
        );
        let buffer = workbench(&mut app);
        assert_eq!(app.open_editors_scroll_offset, 40 - bar.height as usize);
        row_for(&buffer, area, "open-39.rs");
        assert_eq!(app.explorer.scroll_offset, 0);
        assert_eq!(app.explorer_width, 32);
        assert_eq!(app.active_tab, 39);
        assert_eq!(app.tabs.len(), 40);
        assert!(!app.resizing_explorer);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 50, 0);
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), bar.x, 0);
        assert_eq!(app.open_editors_scroll_offset, 40 - bar.height as usize);

        // A long filename must still leave a visible, separate close control.
        let first = app.open_editors_scroll_offset;
        let name = "a-very-long-open-editor-filename-that-needs-truncation.rs";
        app.tabs[first].editor.file_path = Some(name.into());
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            area.x + 8,
            bar.y,
        );
        assert_eq!(app.active_tab, first);
        let buffer = workbench(&mut app);
        let close = (area.x..bar.x)
            .find(|&x| buffer[(x, bar.y)].symbol() == "×")
            .expect("close button beside the scrollbar");
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            close,
            bar.y,
        );
        assert_eq!(app.tabs.len(), 39);
        assert!(app.tabs.iter().all(|tab| tab.editor.file_name() != name));
    }
}

#[test]
fn explorer_open_editors_scrollbar_disappears_when_collapsed_or_all_tabs_fit() {
    for sidebar_right in [false, true] {
        let mut app = fixture(18);
        app.sidebar_right = sidebar_right;
        app.explorer.entries.clear();
        let area = Rect::new(5, 2, 40, 12);
        let x = area.right() - if sidebar_right { 1 } else { 2 };
        let buffer = draw(&mut app, area);
        assert_eq!(buffer[(x, area.y + 2)].symbol(), "▐");
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            area.x + 8,
            area.y + 1,
        );
        let buffer = draw(&mut app, area);
        assert!(!app.show_open_editors);
        assert!((area.y..area.bottom()).all(|y| buffer[(x, y)].symbol() != "▐"));
        app.show_open_editors = true;
        let buffer = draw(&mut app, Rect::new(area.x, area.y, area.width, 40));
        assert!((area.y..area.y + 40).all(|y| buffer[(x, y)].symbol() != "▐"));
        app.tabs.truncate(2);
        let buffer = draw(&mut app, area);
        assert!((area.y..area.bottom()).all(|y| buffer[(x, y)].symbol() != "▐"));
    }
}
