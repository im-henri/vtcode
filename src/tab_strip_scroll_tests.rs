use super::*;
use crate::{
    app::{EditorGroup, Tab},
    editor::Editor,
};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn fixture(count: usize) -> App {
    let mut app = App::test_new();
    app.tabs = (0..count)
        .map(|index| {
            let mut editor = Editor::new();
            editor.file_path = Some(format!("/project/file-{index:02}-long-name.rs").into());
            Tab::new_file(editor)
        })
        .collect();
    app.active_tab = 0;
    app.tab_bar_area = Some(Rect::new(3, 1, 48, 1));
    app.scroll_speed = 3;
    app
}

fn draw(app: &App, group: usize, area: Rect) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(120, 5)).unwrap();
    terminal
        .draw(|frame| {
            super::super::draw_tab_bar_for_group(frame, app, area, group, std::time::Instant::now())
        })
        .unwrap()
        .buffer
        .clone()
}

fn wheel(app: &mut App, kind: MouseEventKind, area: Rect) {
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind,
            column: area.x + 3,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        },
    );
}

#[test]
fn both_wheel_axes_pan_partial_tabs_and_manual_scrolling_survives_redraws() {
    for (forward, backward) in [
        (MouseEventKind::ScrollDown, MouseEventKind::ScrollUp),
        (MouseEventKind::ScrollRight, MouseEventKind::ScrollLeft),
    ] {
        let mut app = fixture(12);
        let area = app.tab_bar_area.unwrap();
        for columns in [3, 6, 9] {
            wheel(&mut app, forward, area);
            app.ensure_active_tab_visible();
            assert_eq!(position(&app, 0).unwrap().columns, columns);
            assert_eq!(app.active_tab, 0);
            let hits = TabBarHits::new(&app, area);
            assert_eq!(
                hits.tabs[0].0, 0,
                "movement must not jump past the first filename"
            );
            assert_eq!(hits.tab_clips[0], columns);
            draw(&app, 0, area);
        }
        wheel(&mut app, backward, area);
        assert_eq!(position(&app, 0).unwrap().columns, 6);
        app.active_tab = 11;
        app.ensure_active_tab_visible();
        assert!(
            position(&app, 0).is_none(),
            "explicit tab navigation resumes active-tab reveal"
        );
        assert!(
            TabBarHits::new(&app, area)
                .tabs
                .iter()
                .any(|(index, _, _)| *index == 11)
        );
    }
}

#[test]
fn wheel_reaches_every_tab_and_the_full_final_controls_then_returns_to_the_start() {
    let mut app = fixture(12);
    // The actual displayed names include path hints for duplicates.
    app.tabs[10].editor.file_path = Some("/other/file-11-long-name.rs".into());
    let area = app.tab_bar_area.unwrap();
    let mut visited = std::collections::HashSet::new();
    let mut previous = None;
    for _ in 0..1000 {
        wheel(&mut app, MouseEventKind::ScrollDown, area);
        app.ensure_active_tab_visible();
        let columns = position(&app, 0).unwrap().columns;
        let hits = TabBarHits::new(&app, area);
        visited.extend(hits.tabs.iter().map(|(index, _, _)| *index));
        if previous == Some(columns) {
            assert!(hits.right.is_empty());
            assert_eq!(hits.tabs.last().unwrap().0, 11);
            assert_eq!(
                hits.tabs.last().unwrap().2.width,
                3,
                "the last close control must fit"
            );
            assert_eq!(
                hits.new_tab.width, 3,
                "the complete new-tab control must fit"
            );
            let buffer = draw(&app, 0, area);
            assert_eq!(buffer[(hits.new_tab.x + 1, area.y)].symbol(), "+");
            break;
        }
        previous = Some(columns);
    }
    assert_eq!(visited.len(), 12);
    for _ in 0..1000 {
        wheel(&mut app, MouseEventKind::ScrollUp, area);
    }
    assert_eq!(position(&app, 0).unwrap().columns, 0);
    assert!(TabBarHits::new(&app, area).left.is_empty());
}

#[test]
fn even_one_filename_wider_than_the_strip_can_be_scrolled_to_its_end() {
    let mut app = fixture(1);
    app.tabs[0].editor.file_path =
        Some(format!("/project/界😀{}-end.rs", "long".repeat(30)).into());
    let area = app.tab_bar_area.unwrap();
    scroll_by(&mut app, area, 0, isize::MAX);
    let hits = TabBarHits::new(&app, area);
    assert!(hits.right.is_empty());
    assert!(hits.tab_clips[0] > 50);
    assert_eq!(hits.tabs[0].2.width, 3);
    let close = hits.tabs[0].2;
    let buffer = draw(&app, 0, area);
    assert_eq!(buffer[(close.x + 1, close.y)].symbol(), "×");
    assert!(
        (area.x..close.x)
            .map(|x| buffer[(x, area.y)].symbol())
            .collect::<String>()
            .contains("-end.rs")
    );
}

#[test]
fn clipped_tabs_keep_close_and_new_tab_clicks_aligned_with_the_painted_controls() {
    let mut app = fixture(4);
    let area = app.tab_bar_area.unwrap();
    scroll_by(&mut app, area, 0, 9);
    let hits = TabBarHits::new(&app, area);
    let (index, _, close) = hits.tabs[0];
    let closed = app.tabs[index].editor.navigation_id;
    assert_eq!(draw(&app, 0, area)[(close.x + 1, close.y)].symbol(), "×");
    assert!(crate::mouse::handle_tab_click(&mut app, close.x + 1, area));
    assert!(
        app.tabs
            .iter()
            .all(|tab| tab.editor.navigation_id != closed)
    );
    scroll_by(&mut app, area, 0, isize::MAX);
    let button = TabBarHits::new(&app, area).new_tab;
    assert_eq!(draw(&app, 0, area)[(button.x + 1, button.y)].symbol(), "+");
    let before = app.tabs.len();
    crate::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: button.x + 1,
            row: button.y,
            modifiers: KeyModifiers::NONE,
        },
    );
    assert_eq!(app.tabs.len(), before + 1);
    assert!(app.editor().unwrap().file_path.is_none());
}

#[test]
fn wheel_over_an_unfocused_pane_scrolls_that_strip_without_changing_focus() {
    let mut app = fixture(8);
    let mut group = EditorGroup::new();
    group.desk = app.desks.active;
    group.tabs = std::mem::take(&mut fixture(8).tabs);
    let area = Rect::new(65, 1, 48, 1);
    group.tab_bar_area = Some(area);
    app.groups = vec![EditorGroup::new(), group];
    wheel(&mut app, MouseEventKind::ScrollDown, area);
    assert_eq!(app.active_group, 0);
    assert!(position(&app, 0).is_none());
    assert_eq!(position(&app, 1).unwrap().columns, 3);
    crate::mouse::ensure_group_active_tab_visible(&mut app.groups[1]);
    assert_eq!(position(&app, 1).unwrap().columns, 3);
    draw(&app, 1, area);
    app.switch_group(1);
    app.ensure_active_tab_visible();
    assert_eq!(position(&app, 1).unwrap().columns, 3);
}

#[test]
fn scroll_geometry_clamps_after_resize_rename_and_tab_removal() {
    let mut app = fixture(8);
    app.group_selected_tabs("Work", None);
    let area = app.tab_bar_area.unwrap();
    scroll_by(&mut app, area, 0, isize::MAX);
    for width in [0, 1, 2, 4, 20, 48, 100] {
        let area = Rect { width, ..area };
        let hits = TabBarHits::new(&app, area);
        for rect in hits
            .tabs
            .iter()
            .map(|(_, rect, _)| *rect)
            .chain(hits.groups.iter().map(|(_, rect)| *rect))
            .chain([hits.left, hits.right, hits.new_tab])
        {
            assert!(rect.is_empty() || rect.intersection(area) == rect);
        }
        draw(&app, 0, area);
    }
    app.tabs.truncate(1);
    app.tabs[0].editor.file_path = Some("a.rs".into());
    let hits = TabBarHits::new(&app, area);
    assert!(hits.right.is_empty());
    assert_eq!(hits.tabs[0].0, 0);
}

#[test]
fn collapsed_groups_hide_their_members_but_keep_pinned_tabs_in_the_scroll_range() {
    let mut app = fixture(8);
    for index in 0..4 {
        app.desks
            .selected_tabs
            .insert(app.tabs[index].editor.navigation_id);
    }
    app.group_selected_tabs("Work", None);
    let id = app.tab_group(&app.tabs[0]).unwrap().id;
    app.desks.groups.get_mut(&id).unwrap().collapsed = true;
    app.tabs[2].pinned = true;
    let area = app.tab_bar_area.unwrap();
    scroll_by(&mut app, area, 0, 0);
    let strip = Strip::new(&app, area, 0, None);
    let visible: Vec<_> = strip
        .items
        .iter()
        .filter_map(|item| match item.kind {
            Kind::Tab(index) => Some(index),
            _ => None,
        })
        .collect();
    assert_eq!(visible, [0, 2, 4, 5, 6, 7]);
    scroll_by(&mut app, area, 0, isize::MAX);
    let hits = TabBarHits::new(&app, area);
    assert!(hits.right.is_empty());
    assert_eq!(hits.tabs.last().unwrap().0, 7);
    assert_eq!(hits.new_tab.width, 3);
}
