use super::*;
use crate::{
    app::{EditorGroup, Tab},
    editor::Editor,
};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn file(name: &str) -> Tab {
    let mut editor = Editor::new();
    editor.file_path = Some(format!("C:/project/{name}").into());
    Tab::new_file(editor)
}

fn fixture(names: &[&str], grouped: bool) -> App {
    let mut app = App::test_new();
    app.tabs = names.iter().map(|name| file(name)).collect();
    app.active_tab = 0;
    app.tab_mru_order = (0..names.len()).collect();
    app.tab_bar_area = Some(Rect::new(2, 1, 120, 1));
    app.animation_settings
        .set(crate::animation_config::AnimationEffect::TabSlide, false);
    if grouped {
        app.desks
            .selected_tabs
            .extend(app.tabs.iter().map(|tab| tab.editor.navigation_id));
        app.group_selected_tabs("Work", None);
    }
    app
}

fn draw(app: &App) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(124, 8)).unwrap();
    terminal
        .draw(|frame| draw_tab_bar(frame, app, app.tab_bar_area.unwrap()))
        .unwrap();
    terminal.backend().buffer().clone()
}

fn text(buffer: &Buffer, area: Rect) -> String {
    (area.x..area.right())
        .map(|column| buffer[(column, area.y)].symbol())
        .collect()
}

fn pointer(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
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

fn click(app: &mut App, column: u16, row: u16) {
    pointer(app, MouseEventKind::Down(MouseButton::Left), column, row);
    pointer(app, MouseEventKind::Up(MouseButton::Left), column, row);
}

#[test]
fn compact_tabs_keep_their_width_and_neighbors_fixed_when_hovered() {
    for grouped in [false, true] {
        let mut app = fixture(&["a", "ab", "app.rs", "long-name", "last"], grouped);
        let area = app.tab_bar_area.unwrap();
        let before = TabBarHits::new(&app, area);
        let buffer = draw(&app);
        assert!(
            !text(&buffer, area).contains('×'),
            "an active tab alone must not show close"
        );
        for (index, rect, close) in before.tabs.iter().copied() {
            assert_eq!(
                rect.width as usize,
                app.tabs[index].editor.file_display_name().width() + 2 + tab_icon(&app.tabs[index]).width()
            );
            assert!(!close.is_empty());
            pointer(&mut app, MouseEventKind::Moved, rect.x + 1, rect.y);
            let hovered = draw(&app);
            assert_eq!(hovered[(close.x, close.y)].symbol(), "×");
            if app.tabs[index].editor.file_name() == "app.rs" {
                assert!(text(&hovered, rect).contains("app. ×"));
            }
            assert_eq!(TabBarHits::new(&app, area).tabs, before.tabs);
            assert_eq!(TabBarHits::new(&app, area).new_tab, before.new_tab);
            pointer(
                &mut app,
                MouseEventKind::Moved,
                rect.x + 1,
                area.bottom() + 1,
            );
            assert!(!text(&draw(&app), area).contains('×'));
        }
    }
}

#[test]
fn short_names_stay_readable_and_have_distinct_select_and_close_targets() {
    for grouped in [false, true] {
        for name in ["a", "ab", "界", "e\u{301}", "👩‍💻"] {
            let mut app = fixture(&[name, "neighbor"], grouped);
            app.active_tab = 1;
            let area = app.tab_bar_area.unwrap();
            let (_, tab, close) = TabBarHits::new(&app, area).tabs[0];
            let id = app.tabs[0].editor.navigation_id;
            pointer(&mut app, MouseEventKind::Moved, tab.x + 1, tab.y);
            let buffer = draw(&app);
            assert!(text(&buffer, tab).contains(name), "hover obscured {name:?}");
            assert_eq!(close.width, 1);
            assert_eq!(close.x, tab.right() - 1);
            assert_eq!(buffer[(close.x, close.y)].symbol(), "×");
            click(&mut app, tab.x + 1, tab.y);
            assert_eq!(app.tabs.len(), 2, "clicking a short name must select it");
            assert_eq!(app.editor().unwrap().navigation_id, id);
            pointer(&mut app, MouseEventKind::Moved, close.x, close.y);
            assert_eq!(draw(&app)[(close.x, close.y)].symbol(), "×");
            click(&mut app, close.x, close.y);
            assert_eq!(app.tabs.len(), 1);
            assert!(app.tabs.iter().all(|tab| tab.editor.navigation_id != id));
        }
    }
}

#[test]
fn wide_last_graphemes_are_fully_overlaid_and_restored_when_the_pointer_leaves() {
    for grouped in [false, true] {
        for name in ["alpha界", "alpha👩‍💻", "alphae\u{301}", "alpha界z"] {
            let mut app = fixture(&[name, "neighbor"], grouped);
            let area = app.tab_bar_area.unwrap();
            let (_, tab, close) = TabBarHits::new(&app, area).tabs[0];
            let plain = draw(&app);
            assert!(text(&plain, tab).contains(name.strip_suffix('z').unwrap_or(name)));
            if name == "alpha界z" {
                assert_eq!(plain[(close.x, close.y)].symbol(), "z");
            }
            pointer(&mut app, MouseEventKind::Moved, tab.x + 1, tab.y);
            let hovered = draw(&app);
            assert!(text(&hovered, tab).contains("alph"));
            assert_eq!(hovered[(close.x - 1, close.y)].symbol(), " ");
            if name == "alpha界z" {
                assert_eq!(hovered[(close.x - 2, close.y)].symbol(), " ");
            }
            assert_eq!(hovered[(close.x, close.y)].symbol(), "×");
            for column in close.x + 1..close.right() {
                assert_eq!(
                    hovered[(column, close.y)].symbol(),
                    " ",
                    "a partial wide glyph survived the overlay"
                );
            }
            pointer(&mut app, MouseEventKind::Moved, 0, 7);
            assert_eq!(text(&draw(&app), tab), text(&plain, tab));
        }
    }
}

#[test]
fn pinned_names_and_unsaved_markers_remain_visible_on_hover() {
    let mut app = fixture(&["a", "long-name", "neighbor"], false);
    app.tabs[0].pinned = true;
    app.tabs[1].editor.modified = true;
    let area = app.tab_bar_area.unwrap();
    let hits = TabBarHits::new(&app, area);
    let (_, pinned, close) = hits.tabs[0];
    assert!(close.is_empty());
    pointer(&mut app, MouseEventKind::Moved, pinned.x + 1, pinned.y);
    assert!(!text(&draw(&app), pinned).contains('×'));
    let (_, dirty, close) = hits.tabs[1];
    pointer(&mut app, MouseEventKind::Moved, dirty.x + 1, dirty.y);
    let buffer = draw(&app);
    assert!(text(&buffer, dirty).contains('●'));
    assert_eq!(buffer[(close.x, close.y)].symbol(), "×");
    pointer(&mut app, MouseEventKind::Moved, close.x, close.y);
    click(&mut app, close.x, close.y);
    assert!(
        app.confirm_close_active,
        "the close overlay must preserve unsaved-file confirmation"
    );
    assert_eq!(app.tabs.len(), 3);
}

#[test]
fn scrolled_close_overlays_never_cover_scroll_controls_or_the_only_selection_cell() {
    let mut app = fixture(&["long-name界", "a", "b", "last"], false);
    let area = Rect::new(2, 1, 12, 1);
    app.tab_bar_area = Some(area);
    for _ in 0..24 {
        scroll_strip_by(&mut app, area, 0, 1);
        let hits = TabBarHits::new(&app, area);
        for (_, tab, close) in hits.tabs.iter().copied() {
            if !close.is_empty() {
                assert!(close.width < tab.width);
                assert_eq!(close.intersection(tab), close);
                for control in [hits.left, hits.right, hits.new_tab] {
                    assert!(close.intersection(control).is_empty());
                }
                pointer(&mut app, MouseEventKind::Moved, close.x, close.y);
                assert_eq!(draw(&app)[(close.x, close.y)].symbol(), "×");
            }
        }
    }
}

#[test]
fn inactive_pane_short_tabs_show_close_and_close_the_correct_tab() {
    let mut app = fixture(&["active"], false);
    let mut other = EditorGroup::new();
    other.tabs = vec![file("a"), file("keep")];
    let id = other.tabs[0].editor.navigation_id;
    let original = app.tabs[0].editor.navigation_id;
    let area = Rect::new(62, 1, 58, 1);
    other.tab_bar_area = Some(area);
    other.editor_area = Some(Rect::new(area.x, area.y + 1, area.width, 5));
    app.tab_bar_area = Some(Rect::new(2, 1, 58, 1));
    app.groups = vec![EditorGroup::new(), other];
    let (_, tab, close) = TabBarHits::for_group(&app, area, 1).tabs[0];
    pointer(&mut app, MouseEventKind::Moved, tab.x + 1, tab.y);
    let mut terminal = Terminal::new(TestBackend::new(124, 8)).unwrap();
    terminal
        .draw(|frame| draw_tab_bar_for_group(frame, &app, area, 1, std::time::Instant::now()))
        .unwrap();
    assert!(text(terminal.backend().buffer(), tab).contains('a'));
    assert_eq!(
        terminal.backend().buffer()[(close.x, close.y)].symbol(),
        "×"
    );
    pointer(&mut app, MouseEventKind::Moved, close.x, close.y);
    click(&mut app, close.x, close.y);
    assert_eq!(app.active_group, 1);
    assert!(app.tabs.iter().all(|tab| tab.editor.navigation_id != id));
    assert!(
        app.groups[0]
            .tabs
            .iter()
            .any(|tab| tab.editor.navigation_id == original)
    );
}
