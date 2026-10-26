use super::*;
use crate::{app::Tab, editor::Editor};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{backend::TestBackend, buffer::Buffer, Terminal};

fn contents(buffer: &Buffer) -> String {
    buffer.content.iter().map(|cell| cell.symbol()).collect()
}

#[test]
fn untitled_names_update_rendering_and_mouse_targets_in_plain_and_grouped_tab_bars() {
    for grouped in [false, true] {
        let mut app = App::test_new();
        let mut neighbor = Editor::test_with_text("saved\n");
        neighbor.file_path = Some("neighbor.rs".into());
        app.tabs = vec![Tab::new_file(Editor::new()), Tab::new_file(neighbor)];
        if grouped {
            app.desks.selected_tabs.extend(app.tabs.iter().map(|tab| tab.editor.navigation_id));
            app.group_selected_tabs("Feature", None);
        }
        let area = Rect::new(0, 0, 160, 1);
        app.tab_bar_area = Some(area);
        let mut terminal = Terminal::new(TestBackend::new(160, 1)).unwrap();
        for (step, expected) in [
            "[NoFile]", "hello worl… [NoFile]", "[NoFile]", "hello worl… [NoFile]",
        ].into_iter().enumerate() {
            match step {
                1 => app.tabs[0].editor.paste_with_text("hello world\nnot the title"),
                2 => app.tabs[0].editor.undo(),
                3 => app.tabs[0].editor.redo(),
                _ => {}
            }
            terminal.draw(|frame| draw_tab_bar(frame, &app, area)).unwrap();
            assert!(contents(terminal.backend().buffer()).contains(expected));
            let hits = TabBarHits::new(&app, area);
            assert_eq!(hits.tabs.len(), 2);
            for &(index, rect, close) in &hits.tabs {
                assert_eq!(hits.tab_at(rect.x + 3).unwrap().0, index);
                assert_eq!(rect.width, tab_render_width(&app, index, &app.tabs[index]));
                // The active tab's close button must track the changing name.
                if index == app.active_tab {
                    assert_eq!(terminal.backend().buffer()[(close.x + 1, close.y)].symbol(), "×");
                }
            }
            let (_, neighbor, _) = hits.tabs[1];
            for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
                crate::mouse::handle_mouse(&mut app, MouseEvent {
                    kind, column: neighbor.x + 3, row: area.y, modifiers: KeyModifiers::NONE,
                });
            }
            assert_eq!(app.active_tab, 1, "the neighbor remains clickable after a title change");
            app.active_tab = 0;
        }
    }
}

#[test]
fn untitled_names_are_consistent_in_open_editors_and_the_tab_switcher() {
    let mut app = App::test_new();
    app.tabs = ["one\nsecond line", "two\nother line"].into_iter()
        .map(|text| Tab::new_file(Editor::test_with_text(text))).collect();
    app.tab_mru_order = vec![0, 1];
    app.show_open_editors = true;
    let mut terminal = Terminal::new(TestBackend::new(110, 30)).unwrap();
    terminal.draw(|frame| {
        crate::ui_chrome::draw_explorer(frame, &mut app, Rect::new(0, 0, 60, 30));
    }).unwrap();
    let explorer = contents(terminal.backend().buffer());
    terminal.draw(|frame| crate::ui_pickers::draw_tab_switcher(frame, &app)).unwrap();
    let switcher = contents(terminal.backend().buffer());
    for (index, expected) in ["one [NoFile]", "two [NoFile]"].into_iter().enumerate() {
        assert_eq!(tab_display_name(&app, index, &app.tabs[index]), expected);
        assert!(explorer.contains(expected), "Open Editors: {expected}");
        assert!(switcher.contains(expected), "tab switcher: {expected}");
    }
}

#[test]
fn untitled_names_reveal_the_correct_tab_in_narrow_strips() {
    let mut app = App::test_new();
    app.tabs = (0..8).map(|i| {
        Tab::new_file(Editor::test_with_text(&format!("note {i} with a longer title")))
    }).collect();
    for width in [30, 60, 100] {
        let area = Rect::new(0, 0, width, 1);
        app.tab_bar_area = Some(area);
        for active in (0..app.tabs.len()).rev() {
            app.active_tab = active;
            app.ensure_active_tab_visible();
            let hits = TabBarHits::new(&app, area);
            assert!(hits.tabs.iter().any(|(i, _, _)| *i == active), "width={width}, active={active}");
        }
    }
}
