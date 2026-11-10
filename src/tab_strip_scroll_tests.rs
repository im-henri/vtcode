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
    let mut terminal = Terminal::new(TestBackend::new(
        120.max(area.right() + 2),
        5.max(area.bottom() + 2),
    ))
    .unwrap();
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
            position(&app, 0).is_some_and(|scroll| !scroll.manual),
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
                2,
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
fn oversized_unicode_filename_is_ellipsized_and_keeps_its_suffix_and_close_control() {
    let mut app = fixture(1);
    app.tabs[0].editor.file_path =
        Some(format!("/project/界😀{}-end.rs", "long".repeat(30)).into());
    let area = app.tab_bar_area.unwrap();
    scroll_by(&mut app, area, 0, isize::MAX);
    let hits = TabBarHits::new(&app, area);
    assert!(hits.right.is_empty());
    assert_eq!(hits.tab_clips[0], 0);
    assert!(hits.tabs[0].1.width < area.width - 4);
    assert_eq!(hits.tabs[0].2.width, 2);
    let close = hits.tabs[0].2;
    app.mouse_position_known = true;
    app.mouse_col = close.x;
    app.mouse_row = close.y;
    let buffer = draw(&app, 0, area);
    assert_eq!(buffer[(close.x, close.y)].symbol(), "×");
    let label = (area.x..close.x)
        .map(|x| buffer[(x, area.y)].symbol())
        .collect::<String>();
    assert!(label.contains("…"));
    assert!(label.contains("-end."));
    assert_eq!(buffer[(close.x - 1, close.y)].symbol(), " ");
}

#[test]
fn clipped_tabs_keep_close_and_new_tab_clicks_aligned_with_the_painted_controls() {
    let mut app = fixture(4);
    let area = app.tab_bar_area.unwrap();
    scroll_by(&mut app, area, 0, 9);
    let hits = TabBarHits::new(&app, area);
    let (index, _, close) = hits.tabs[0];
    let closed = app.tabs[index].editor.navigation_id;
    app.mouse_position_known = true;
    app.mouse_col = close.x;
    app.mouse_row = close.y;
    assert_eq!(draw(&app, 0, area)[(close.x, close.y)].symbol(), "×");
    assert!(crate::mouse::handle_tab_click(&mut app, close.x, area));
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
    reveal(&mut app, area, 1);
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

#[test]
fn opening_a_long_diff_keeps_previous_tabs_and_a_partial_label_to_its_left() {
    let mut app = fixture(3);
    app.enable_preview_tabs = false;
    for (tab, name) in app.tabs.iter_mut().zip(["aaa", "bbb", "ccc"]) {
        tab.editor.file_path = Some(format!("/project/{name}").into());
    }
    let area = Rect::new(3, 1, 40, 1);
    app.tab_bar_area = Some(area);
    app.active_tab = 2;
    app.ensure_active_tab_visible();
    assert_eq!(position(&app, 0).unwrap().columns, 0);
    app.create_snapshot_diff_tab();
    // The full filename arrives after opening the diff, just like async loads.
    let filename = format!("{}-end.rs", "a_very_long_filename_".repeat(8));
    app.editor_mut().unwrap().file_path = Some(format!("/project/{filename}").into());
    let mut terminal = Terminal::new(TestBackend::new(100, 5)).unwrap();
    let buffer = terminal
        .draw(|frame| super::super::draw_active_tab_bar(frame, &mut app, area))
        .unwrap()
        .buffer
        .clone();
    let scroll = position(&app, 0).unwrap();
    let visible = TabBarHits::new(&app, area);
    assert!(!scroll.manual);
    assert_eq!(
        visible
            .tabs
            .iter()
            .map(|(index, _, _)| *index)
            .collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
    assert!(
        visible.tab_clips[0] > 0,
        "keep even the remaining part of the old label"
    );
    let (_, rect, close) = *visible.tabs.last().unwrap();
    assert_eq!(visible.tab_clips.last(), Some(&0));
    assert_eq!(usize::from(rect.width), *visible.tab_widths.last().unwrap());
    assert!(
        rect.right() >= area.right() - 2,
        "reveal against the right edge without trailing gaps"
    );
    let text = (rect.x..rect.right())
        .map(|x| buffer[(x, area.y)].symbol())
        .collect::<String>();
    assert!(text.contains('…'));
    assert!(text.contains("(Diff)"));
    assert!(super::super::tab_display_name(&app, 3, &app.tabs[3]).contains(&filename));
    let previous = hits(
        &app,
        area,
        0,
        StripScroll {
            columns: scroll.columns - 1,
            ..scroll
        },
        None,
    );
    assert!(
        previous.tabs.last().unwrap().1.width < rect.width,
        "scroll only the minimum columns"
    );
    app.mouse_position_known = true;
    app.mouse_col = close.x;
    app.mouse_row = close.y;
    assert_eq!(draw(&app, 0, area)[(close.x, close.y)].symbol(), "×");
}

#[test]
fn reveal_leaves_an_already_visible_tab_in_place_and_scrolls_back_only_as_needed() {
    let mut app = fixture(8);
    let area = Rect::new(3, 1, 72, 1);
    app.tab_bar_area = Some(area);
    app.active_tab = 6;
    app.ensure_active_tab_visible();
    let before = position(&app, 0).unwrap().columns;
    let visible = TabBarHits::new(&app, area);
    let fully_visible = visible
        .tabs
        .iter()
        .enumerate()
        .find_map(|(position, (index, rect, _))| {
            (*index != 6
                && visible.tab_clips[position] == 0
                && usize::from(rect.width) == visible.tab_widths[position])
                .then_some(*index)
        });
    app.active_tab = fully_visible.expect("a second tab is fully visible beside the active tab");
    app.ensure_active_tab_visible();
    assert_eq!(position(&app, 0).unwrap().columns, before);
    app.active_tab = 0;
    app.ensure_active_tab_visible();
    assert_eq!(position(&app, 0).unwrap().columns, 0);
    assert_eq!(TabBarHits::new(&app, area).tabs[0].0, 0);
}

#[test]
fn automatic_reveal_recalculates_after_resize_and_removal_without_losing_neighbours() {
    let mut app = fixture(4);
    app.active_tab = 3;
    app.tabs[3].editor.file_path = Some(format!("{}.rs", "界🧑‍💻e\u{301}".repeat(30)).into());
    for width in [48, 24, 120] {
        let area = Rect::new(3, 1, width, 1);
        app.tab_bar_area = Some(area);
        app.ensure_active_tab_visible();
        let visible = TabBarHits::new(&app, area);
        assert!(visible.tabs.len() > 1, "width={width}");
        let position = visible
            .tabs
            .iter()
            .position(|(index, _, _)| *index == 3)
            .unwrap();
        assert_eq!(visible.tab_clips[position], 0);
        assert_eq!(
            usize::from(visible.tabs[position].1.width),
            visible.tab_widths[position]
        );
        draw(&app, 0, area);
    }
    app.tabs.truncate(1);
    app.active_tab = 0;
    app.ensure_active_tab_visible();
    assert_eq!(position(&app, 0).unwrap().columns, 0);
    assert!(
        TabBarHits::new(&app, app.tab_bar_area.unwrap())
            .right
            .is_empty()
    );
}

#[test]
fn split_panes_save_automatic_scroll_and_reveal_inactive_navigation_independently() {
    let mut app = fixture(8);
    let area = app.tab_bar_area.unwrap();
    app.active_tab = 7;
    app.ensure_active_tab_visible();
    let first = position(&app, 0).unwrap();
    let mut other = fixture(8);
    let mut group = EditorGroup::new();
    group.desk = app.desks.active;
    group.tabs = std::mem::take(&mut other.tabs);
    group.tab_bar_area = Some(Rect::new(65, 1, 48, 1));
    app.groups = vec![EditorGroup::new(), group];
    reveal(&mut app, Rect::new(65, 1, 48, 1), 1);
    app.groups[1].active_tab = 7;
    reveal(&mut app, Rect::new(65, 1, 48, 1), 1);
    assert!(!position(&app, 1).unwrap().manual);
    assert!(
        TabBarHits::for_group(&app, Rect::new(65, 1, 48, 1), 1)
            .tabs
            .iter()
            .any(|(index, _, _)| *index == 7)
    );
    assert_eq!(position(&app, 0), Some(first));
    app.switch_group(1);
    app.switch_group(0);
    assert_eq!(position(&app, 0), Some(first));
    assert_eq!(app.tab_scroll_manual_anchor, None);
    app.active_tab = 0;
    app.ensure_active_tab_visible();
    assert_eq!(position(&app, 0).unwrap().columns, 0);
    draw(&app, 0, area);
}

#[test]
fn ellipsized_labels_share_geometry_in_plain_and_scrolling_renderers() {
    let mut app = fixture(3);
    app.tabs[0].editor.file_path = Some(format!("{}-end.rs", "long".repeat(30)).into());
    let area = app.tab_bar_area.unwrap();
    let visible = TabBarHits::new(&app, area);
    let (_, _, close) = visible.tabs[0];
    app.mouse_position_known = true;
    app.mouse_col = close.x;
    app.mouse_row = close.y;
    let mut terminal = Terminal::new(TestBackend::new(120, 5)).unwrap();
    let plain = terminal
        .draw(|frame| super::super::draw_tab_bar(frame, &app, area))
        .unwrap()
        .buffer
        .clone();
    let grouped = draw(&app, 0, area);
    assert_eq!(plain[(close.x, close.y)].symbol(), "×");
    for x in visible.tabs[0].1.x..visible.tabs[0].1.right() {
        assert_eq!(plain[(x, area.y)].symbol(), grouped[(x, area.y)].symbol());
    }
}

#[test]
fn clearing_manual_scroll_resumes_reveal_after_switching_panes() {
    let mut app = fixture(8);
    let area = app.tab_bar_area.unwrap();
    scroll_by(&mut app, area, 0, isize::MAX);
    assert!(
        TabBarHits::new(&app, area)
            .tabs
            .iter()
            .all(|(index, _, _)| *index != 0)
    );
    let mut other = fixture(1);
    let mut group = EditorGroup::new();
    group.desk = app.desks.active;
    group.tabs = std::mem::take(&mut other.tabs);
    app.groups = vec![EditorGroup::new(), group];
    app.tab_scroll_manual_anchor = None;
    app.switch_group(1);
    app.switch_group(0);
    assert_eq!(app.tab_scroll_manual_anchor, None);
    app.ensure_active_tab_visible();
    assert_eq!(position(&app, 0).unwrap().columns, 0);
}
