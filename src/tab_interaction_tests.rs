use crate::{
    app::{App, EditorGroup, Tab},
    editor::Editor,
    mouse::handle_mouse,
    ui_chrome_tabs::{TabBarHits, scroll_strip_by, strip_scroll_position},
};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};

fn fixture(names: &[&str], width: u16) -> App {
    let mut app = App::test_new();
    app.tabs = names
        .iter()
        .map(|name| {
            let mut editor = Editor::new();
            editor.file_path = Some(format!("/project/{name}").into());
            Tab::new_file(editor)
        })
        .collect();
    app.active_tab = 0;
    app.tab_mru_order = (0..names.len()).collect();
    app.tab_bar_area = Some(Rect::new(3, 1, width, 1));
    app
}

fn many_tabs(width: u16) -> App {
    let names: Vec<_> = (0..24)
        .map(|index| format!("file-{index:02}-long-name.rs"))
        .collect();
    fixture(&names.iter().map(String::as_str).collect::<Vec<_>>(), width)
}

fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
    handle_mouse(
        app,
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        },
    );
    // Match the event loop: reveal and redraw between pointer samples.
    app.ensure_active_tab_visible();
    let area = app.tab_bar_area.unwrap();
    let mut terminal =
        Terminal::new(TestBackend::new(area.right() + 2, area.bottom() + 2)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui_chrome_tabs::draw_tab_bar_for_group(
                frame,
                app,
                area,
                app.active_group,
                std::time::Instant::now(),
            )
        })
        .unwrap();
}

fn ids(app: &App) -> Vec<u64> {
    app.tabs
        .iter()
        .map(|tab| tab.editor.navigation_id)
        .collect()
}

fn mru_ids(app: &App) -> Vec<u64> {
    app.tab_mru_order
        .iter()
        .map(|&index| app.tabs[index].editor.navigation_id)
        .collect()
}

#[test]
fn clicking_scrolled_tabs_preserves_all_hit_rectangles_through_release_and_redraw() {
    for width in [48, 93, 160] {
        for columns in [9, 143, isize::MAX] {
            let mut app = many_tabs(width);
            let area = app.tab_bar_area.unwrap();
            scroll_strip_by(&mut app, area, 0, columns);
            let before = TabBarHits::new(&app, area);
            let selected = before
                .tabs
                .iter()
                .copied()
                .filter(|(_, rect, close)| rect.x < close.x)
                .collect::<Vec<_>>();
            for (index, rect, _) in selected {
                let scroll = strip_scroll_position(&app, 0).unwrap().columns;
                mouse(
                    &mut app,
                    MouseEventKind::Down(MouseButton::Left),
                    rect.x,
                    area.y,
                );
                mouse(
                    &mut app,
                    MouseEventKind::Up(MouseButton::Left),
                    rect.x,
                    area.y,
                );
                assert_eq!(app.active_tab, index);
                assert_eq!(
                    strip_scroll_position(&app, 0).map(|scroll| scroll.columns),
                    Some(scroll)
                );
                let after = TabBarHits::new(&app, area);
                assert_eq!(after.tabs, before.tabs);
                assert_eq!(
                    (after.left, after.right, after.new_tab),
                    (before.left, before.right, before.new_tab)
                );
            }
        }
    }
}

#[test]
fn clicking_visible_index_scrolled_tabs_keeps_the_same_viewport() {
    let mut app = many_tabs(93);
    let area = app.tab_bar_area.unwrap();
    app.tab_scroll_offset = 20;
    app.active_tab = 20;
    let before = TabBarHits::new(&app, area).tabs;
    for (index, rect, _) in before.iter().copied().rev() {
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            rect.x + 1,
            area.y,
        );
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            rect.x + 1,
            area.y,
        );
        assert_eq!(app.active_tab, index);
        assert_eq!(app.tab_scroll_offset, 20);
        assert_eq!(TabBarHits::new(&app, area).tabs, before);
    }
}

#[test]
fn clicking_pinned_unicode_and_grouped_tabs_preserves_partial_labels_and_headers() {
    let mut app = fixture(
        &[
            "first/界😀.rs",
            "second/界😀.rs",
            "e\u{301}.rs",
            "wide-界界.rs",
            "last.rs",
        ],
        76,
    );
    app.tabs[1].pinned = true;
    app.tabs[2].editor.modified = true;
    app.desks
        .selected_tabs
        .extend(app.tabs[..3].iter().map(|tab| tab.editor.navigation_id));
    app.group_selected_tabs("Feature", None);
    let area = app.tab_bar_area.unwrap();
    for columns in [7, 29, isize::MAX] {
        scroll_strip_by(&mut app, area, 0, columns);
        let before = TabBarHits::new(&app, area);
        for (index, rect, close) in before.tabs.iter().copied() {
            if TabBarHits::contains(close, rect.x) {
                continue;
            }
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                rect.x,
                area.y,
            );
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                rect.x,
                area.y,
            );
            assert_eq!(app.active_tab, index);
            let after = TabBarHits::new(&app, area);
            assert_eq!(after.tabs, before.tabs);
            assert_eq!(after.groups, before.groups);
        }
    }
}

#[test]
fn selecting_a_scrolled_tab_in_an_inactive_pane_preserves_both_viewports() {
    let mut app = many_tabs(60);
    let area = Rect::new(70, 1, 60, 1);
    let mut other = EditorGroup::new();
    other.tabs = std::mem::take(&mut many_tabs(60).tabs);
    other.tab_bar_area = Some(area);
    other.editor_area = Some(Rect::new(area.x, area.y + 1, area.width, 10));
    app.groups = vec![EditorGroup::new(), other];
    let active_area = app.tab_bar_area.unwrap();
    scroll_strip_by(&mut app, active_area, 0, 13);
    scroll_strip_by(&mut app, area, 1, 207);
    let before = TabBarHits::for_group(&app, area, 1).tabs;
    let (index, rect, _) = before
        .iter()
        .copied()
        .find(|(_, rect, close)| rect.x < close.x)
        .unwrap();
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        rect.x,
        area.y,
    );
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        rect.x,
        area.y,
    );
    assert_eq!((app.active_group, app.active_tab), (1, index));
    assert_eq!(strip_scroll_position(&app, 1).unwrap().columns, 207);
    assert_eq!(strip_scroll_position(&app, 0).unwrap().columns, 13);
    assert_eq!(TabBarHits::new(&app, area).tabs, before);
}

#[test]
fn normalizing_named_groups_keeps_scroll_and_history_on_the_selected_view() {
    let mut app = many_tabs(93);
    app.desks
        .selected_tabs
        .extend([0, 2].map(|index| app.tabs[index].editor.navigation_id));
    app.group_selected_tabs("Feature", None);
    // Model an intermediate drag order with an ungrouped tab between members.
    app.tabs.swap(1, 2);
    app.active_tab = 2;
    let selected = app.editor().unwrap().navigation_id;
    let history = mru_ids(&app);
    let area = app.tab_bar_area.unwrap();
    scroll_strip_by(&mut app, area, 0, 12);
    app.normalize_tab_groups();
    app.ensure_active_tab_visible();
    assert_eq!(app.editor().unwrap().navigation_id, selected);
    assert_eq!(strip_scroll_position(&app, 0).unwrap().columns, 12);
    assert_eq!(mru_ids(&app), history);
}

#[test]
fn dropping_a_preview_into_a_group_before_the_midpoint_keeps_the_view_permanent() {
    let mut app = fixture(&["preview.rs", "a-much-wider-target.rs"], 150);
    app.desks
        .selected_tabs
        .insert(app.tabs[1].editor.navigation_id);
    app.group_selected_tabs("Feature", None);
    let group = app.tab_group(&app.tabs[1]).unwrap().id;
    app.tabs[0].preview = true;
    let moving = app.tabs[0].editor.navigation_id;
    let area = app.tab_bar_area.unwrap();
    let hits = TabBarHits::new(&app, area);
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        hits.tabs[0].1.x + 1,
        area.y,
    );
    let column = hits.tabs[1].1.x + 1;
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        column,
        area.y,
    );
    assert_eq!(
        app.dragging_tab,
        Some(0),
        "the target midpoint was not crossed"
    );
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        column,
        area.y,
    );
    assert_eq!(app.editor().unwrap().navigation_id, moving);
    assert_eq!(app.tab_group(&app.tabs[app.active_tab]).unwrap().id, group);
    assert!(!app.tabs[app.active_tab].preview);
}

#[test]
fn dragging_across_several_tabs_inserts_without_reordering_other_views_or_history() {
    for (source, target) in [(4, 0), (0, 4)] {
        let mut app = fixture(&["a.rs", "longer-file.rs", "c.rs", "d.rs", "last.rs"], 180);
        let area = app.tab_bar_area.unwrap();
        let before = ids(&app);
        let hits = TabBarHits::new(&app, area);
        let rect = hits.tabs[source].1;
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            rect.x + 1,
            area.y,
        );
        let history = mru_ids(&app);
        let rect = hits.tabs[target].1;
        let column = if target < source {
            rect.x
        } else {
            rect.right() - 1
        };
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            column,
            area.y,
        );
        let mut expected = before.clone();
        let moving = expected.remove(source);
        expected.insert(target, moving);
        assert_eq!(ids(&app), expected);
        assert_eq!(app.editor().unwrap().navigation_id, moving);
        assert_eq!(mru_ids(&app), history);
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            column,
            area.y,
        );
        assert_eq!(ids(&app), expected);
        assert!(app.dragging_tab.is_none());
    }
}

#[test]
fn dragging_waits_for_a_midpoint_and_repeated_samples_cannot_bounce_the_tab_back() {
    let mut app = fixture(&["a-much-wider-neighbor.rs", "b.rs", "c.rs"], 150);
    let area = app.tab_bar_area.unwrap();
    let before = ids(&app);
    let hits = TabBarHits::new(&app, area);
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        hits.tabs[1].1.x + 1,
        area.y,
    );
    let neighbor = hits.tabs[0].1;
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        neighbor.right() - 1,
        area.y,
    );
    assert_eq!(
        ids(&app),
        before,
        "entering a neighbor must not immediately swap tabs"
    );
    let column = neighbor.x + neighbor.width / 2 - 1;
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        column,
        area.y,
    );
    assert_eq!(ids(&app), [before[1], before[0], before[2]]);
    for _ in 0..5 {
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            column,
            area.y,
        );
        assert_eq!(ids(&app), [before[1], before[0], before[2]]);
    }
}

#[test]
fn continuous_drags_of_unequal_tabs_keep_the_viewport_and_move_only_in_the_pointer_direction() {
    for leftward in [true, false] {
        let mut app = fixture(
            &["a-very-wide-first-tab.rs", "b.rs", "c.rs", "last.rs"],
            130,
        );
        let area = app.tab_bar_area.unwrap();
        scroll_strip_by(&mut app, area, 0, 0);
        let before = ids(&app);
        let source = if leftward { 3 } else { 0 };
        let hits = TabBarHits::new(&app, area);
        let rect = hits.tabs[source].1;
        let start = rect.x + 1;
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            start,
            area.y,
        );
        let moving = before[source];
        let end = if leftward {
            hits.tabs[0].1.x
        } else {
            hits.tabs[3].1.right() - 1
        };
        let columns: Vec<_> = if leftward {
            (end..start).rev().collect()
        } else {
            (start..=end).collect()
        };
        let mut previous = source;
        for column in columns {
            for _ in 0..2 {
                mouse(
                    &mut app,
                    MouseEventKind::Drag(MouseButton::Left),
                    column,
                    area.y,
                );
                let current = app.dragging_tab.unwrap();
                assert!(if leftward {
                    current <= previous
                } else {
                    current >= previous
                });
                assert_eq!(app.editor().unwrap().navigation_id, moving);
                assert_eq!(strip_scroll_position(&app, 0).unwrap().columns, 0);
                assert_eq!(
                    ids(&app)
                        .into_iter()
                        .filter(|&id| id != moving)
                        .collect::<Vec<_>>(),
                    before
                        .iter()
                        .copied()
                        .filter(|&id| id != moving)
                        .collect::<Vec<_>>()
                );
                previous = current;
            }
        }
        assert_eq!(app.dragging_tab, Some(if leftward { 0 } else { 3 }));
    }
}

#[test]
fn dragging_in_a_partially_scrolled_strip_preserves_columns_and_selection_after_release() {
    let mut app = many_tabs(120);
    let area = app.tab_bar_area.unwrap();
    scroll_strip_by(&mut app, area, 0, 213);
    let hits = TabBarHits::new(&app, area);
    let (source, rect, _) = hits
        .tabs
        .iter()
        .copied()
        .rev()
        .find(|(_, rect, close)| rect.x < close.x)
        .unwrap();
    let moving = app.tabs[source].editor.navigation_id;
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        rect.x,
        area.y,
    );
    let first = hits.tabs[0].1.x;
    for column in (first..rect.x).rev() {
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            column,
            area.y,
        );
        assert_eq!(strip_scroll_position(&app, 0).unwrap().columns, 213);
        assert_eq!(app.editor().unwrap().navigation_id, moving);
    }
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        first,
        area.y,
    );
    assert_eq!(strip_scroll_position(&app, 0).unwrap().columns, 213);
    assert_eq!(app.editor().unwrap().navigation_id, moving);
}

#[test]
fn dragging_on_scroll_indicators_reaches_hidden_tabs_without_revealing_the_source() {
    for leftward in [true, false] {
        let mut app = many_tabs(93);
        let area = app.tab_bar_area.unwrap();
        scroll_strip_by(&mut app, area, 0, if leftward { isize::MAX } else { 0 });
        let hits = TabBarHits::new(&app, area);
        let &(index, rect, _) = if leftward {
            hits.tabs.last().unwrap()
        } else {
            hits.tabs.first().unwrap()
        };
        let moving = app.tabs[index].editor.navigation_id;
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            rect.x + 1,
            area.y,
        );
        let mut previous = strip_scroll_position(&app, 0).unwrap().columns;
        for _ in 0..300 {
            let hits = TabBarHits::new(&app, area);
            let indicator = if leftward { hits.left } else { hits.right };
            if indicator.is_empty() {
                break;
            }
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                indicator.x,
                area.y,
            );
            let current = strip_scroll_position(&app, 0).unwrap().columns;
            assert!(if leftward {
                current < previous
            } else {
                current > previous
            });
            assert_eq!(app.editor().unwrap().navigation_id, moving);
            previous = current;
        }
        let hits = TabBarHits::new(&app, area);
        assert!(if leftward {
            hits.left.is_empty()
        } else {
            hits.right.is_empty()
        });
        let &(target, rect, _) = if leftward {
            hits.tabs.first().unwrap()
        } else {
            hits.tabs.last().unwrap()
        };
        let column = if leftward { rect.x } else { rect.right() - 1 };
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            column,
            area.y,
        );
        assert_eq!(app.dragging_tab, Some(target));
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            column,
            area.y,
        );
        assert_eq!(app.tabs[target].editor.navigation_id, moving);
    }
}

#[test]
fn dragging_a_tab_wider_than_the_bar_can_reach_its_hidden_neighbors() {
    let wide = format!("{}.rs", "wide".repeat(40));
    let mut app = fixture(&[&wide, "middle.rs", "last.rs"], 60);
    let area = app.tab_bar_area.unwrap();
    let hits = TabBarHits::new(&app, area);
    assert!(
        hits.right.is_empty(),
        "the wide first label fills the legacy viewport"
    );
    let moving = app.tabs[0].editor.navigation_id;
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        hits.tabs[0].1.x + 1,
        area.y,
    );
    let edge = area.right() - 1;
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        edge,
        area.y,
    );
    assert!(strip_scroll_position(&app, 0).unwrap().columns > 0);
    for _ in 0..100 {
        if TabBarHits::new(&app, area).right.is_empty() {
            break;
        }
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            edge,
            area.y,
        );
    }
    let hits = TabBarHits::new(&app, area);
    assert!(hits.right.is_empty());
    let (target, rect, _) = *hits.tabs.last().unwrap();
    assert_eq!(target, 2);
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        rect.right() - 1,
        area.y,
    );
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        rect.right() - 1,
        area.y,
    );
    assert_eq!(app.tabs[2].editor.navigation_id, moving);
}
