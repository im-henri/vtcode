use super::*;
use crate::app::{SplitDirection, Tab, TabDropZone};
use crate::editor::Editor;
use crate::ui_chrome_tabs::TabBarHits;
use crossterm::event::{MouseButton, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};

const EDGES: [TabDropZone; 4] = [
    TabDropZone::Left,
    TabDropZone::Right,
    TabDropZone::Top,
    TabDropZone::Bottom,
];

#[test]
fn panel_resize_uses_rendered_geometry_with_and_without_status_bar() {
    for status in [false, true] {
        let mut app = App::test_new();
        app.resizing_panel = true;
        app.show_status_bar = status;
        app.zen_mode = false;
        app.panel_area = Some(Rect::new(0, 60, 160, 20));
        let screen_bottom = 80 + u16::from(status);
        for row in [75, 70, 40, 90] {
            handle_left_drag(&mut app, 80, row, 80, row, KeyModifiers::NONE);
            assert_eq!(
                app.panel_height,
                screen_bottom
                    .saturating_sub(row)
                    .clamp(4, screen_bottom / 2)
            );
        }
    }
}

#[test]
fn dragging_terminal_between_docks_and_split_directions_preserves_rendered_logs() {
    let mut app = App::test_new();
    app.tabs = vec![tab("file")];
    app.active_tab = 0;
    app.show_terminal_sidebar = true;
    app.terminal_sidebar_revealed = true;
    let output = "BEGINLOGabcdefghijklmnopqrstuvwxyz0123456789\r\nsecond log\r\nMOVE_LOG_READY\r\n> ";
    app.terminals[0].terminal_pty = Some(crate::terminal::TerminalEmulator::test_with_output(80, 30, output));
    let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
    draw(&mut app, &mut terminal);
    let sidebar_tab = app.terminal_sidebar_tab_areas[0].0;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), (sidebar_tab.x + 2, sidebar_tab.y));
    assert_eq!(app.dragging_terminal_tab, Some(0));

    for edge in [TabDropZone::Top, TabDropZone::Left, TabDropZone::Bottom, TabDropZone::Right] {
        if app.terminal_is_in_editor(0) {
            let source = app.active_group;
            start_drag(&mut app, &mut terminal, source);
        }
        let file_area = area_for(&app, "file");
        let (mut x, mut y) = edge_position(file_area, edge);
        // Stay inside the editor split target, clear of the outer dock targets.
        if edge == TabDropZone::Bottom { y = file_area.bottom() - 4; }
        if edge == TabDropZone::Right { x = file_area.right() - 4; }
        finish_drag(&mut app, &mut terminal, (x, y));
        assert_eq!(app.tabs[app.active_tab].terminal_idx, Some(0));
        assert_placement(app.editor_area.unwrap(), area_for(&app, "file"), edge);
        assert_terminal_log(&app, &terminal, output);
    }

    // Return the same session to the right dock.
    let source = app.active_group;
    start_drag(&mut app, &mut terminal, source);
    let body = app.workbench_body_area.unwrap();
    finish_drag(&mut app, &mut terminal, (body.right() - 1, body.y + body.height / 2));
    assert!(app.terminal_is_in_dock(0, TerminalDock::Right));
    assert_terminal_log(&app, &terminal, output);
}

fn assert_terminal_log(app: &App, terminal: &Terminal<TestBackend>, expected: &str) {
    let pty = app.terminals[0].terminal_pty.as_ref().unwrap();
    assert_eq!(pty.profile_name, "Log fixture", "moving must reuse the session");
    let text: String = pty.grid.scrollback.iter().chain(&pty.grid.cells).flatten()
        .map(|cell| cell.ch).filter(|ch| !ch.is_whitespace()).collect();
    assert_eq!(text, expected.chars().filter(|ch| !ch.is_whitespace()).collect::<String>());
    let buffer = terminal.backend().buffer();
    let visible = (0..buffer.area.height).any(|y| {
        (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect::<String>()
            .contains("MOVE_LOG_READY")
    });
    assert!(visible, "existing output must be visible without new PTY output");
}

#[test]
fn terminal_center_drop_renders_live_output_without_a_split() {
    for dock in [TerminalDock::Right] {
        for keep_file in [false, true] {
            let mut app = App::test_new();
            if keep_file { app.tabs = vec![tab("file")]; }
            app.active_tab = 0;
            app.move_terminal_to_dock(0, dock);
            let output = "existing log\r\nMOVE_LOG_READY\r\n> ";
            app.terminals[0].terminal_pty = Some(crate::terminal::TerminalEmulator::test_with_output(80, 24, output));
            let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
            draw(&mut app, &mut terminal);
            let start = match dock {
                TerminalDock::Right => {
                    let area = app.terminal_sidebar_tab_areas[0].0;
                    (area.x + 2, area.y)
                }
            };
            let area = app.editor_area.unwrap();
            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), start);
            assert_eq!(app.dragging_terminal_tab, Some(0));
            finish_drag(&mut app, &mut terminal, (area.x + area.width / 2, area.y + area.height / 2));
            assert_single_terminal_view(&app, &terminal, output);
        }
    }
}

#[test]
fn terminal_stays_live_when_the_last_neighboring_split_is_closed() {
    for direction in [SplitDirection::Horizontal, SplitDirection::Vertical] {
        let mut app = App::test_new();
        app.tabs = vec![tab("file")];
        app.active_tab = 0;
        let output = "existing log\r\nMOVE_LOG_READY\r\n> ";
        app.terminals[0].terminal_pty = Some(crate::terminal::TerminalEmulator::test_with_output(80, 24, output));
        app.detach_terminal_to_tab();
        app.create_split_group_dir(app.active_tab, direction);
        let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
        draw(&mut app, &mut terminal);
        app.switch_group(0);
        draw(&mut app, &mut terminal);
        assert!(app.code_area.is_some(), "neighboring file leaves editor hit areas");
        app.close_tab(0);
        draw(&mut app, &mut terminal);
        assert_single_terminal_view(&app, &terminal, output);
        app.zen_mode = true;
        draw(&mut app, &mut terminal);
        assert_single_terminal_view(&app, &terminal, output);
    }
}

fn assert_single_terminal_view(app: &App, terminal: &Terminal<TestBackend>, output: &str) {
    assert!(!app.split_active());
    assert_eq!(app.tabs[app.active_tab].kind, crate::app::TabKind::Terminal);
    assert_eq!(app.tabs[app.active_tab].terminal_idx, Some(0));
    assert_terminal_log(app, terminal, output);
    let area = app.editor_area.unwrap();
    let grid = &app.terminals[0].terminal_pty.as_ref().unwrap().grid;
    assert_eq!((grid.width, grid.height), (area.width as usize, area.height as usize));
    assert!(app.code_area.is_none());
    assert!(app.gutter_area.is_none());
    assert!(app.minimap_area.is_none());
    assert!(app.scrollbar_area.is_none());
    assert!(app.h_scrollbar_area.is_none());
    assert!(app.breadcrumb_area.is_none());
    assert!(!app.settings_editor_active);
}

fn tab(name: &str) -> Tab {
    let mut editor = Editor::test_with_text(name);
    editor.file_path = Some(std::path::PathBuf::from(format!("{name}.txt")));
    Tab::new_file(editor)
}

fn draw(app: &mut App, terminal: &mut Terminal<TestBackend>) {
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
}

fn mouse(app: &mut App, kind: MouseEventKind, (column, row): (u16, u16)) {
    crate::mouse::handle_mouse(
        app,
        crossterm::event::MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        },
    );
}

fn area_for(app: &App, name: &str) -> Rect {
    super::tests::tile_area_for_text(app, name)
}

pub(super) fn edge_position(area: Rect, edge: TabDropZone) -> (u16, u16) {
    let center = (area.x + area.width / 2, area.y + area.height / 2);
    match edge {
        TabDropZone::Left => (area.x + 1, center.1),
        TabDropZone::Right => (area.right() - 2, center.1),
        TabDropZone::Top => (center.0, area.y + 2),
        TabDropZone::Bottom => (center.0, area.bottom() - 2),
    }
}

pub(super) fn assert_placement(dragged: Rect, target: Rect, edge: TabDropZone) {
    match edge {
        TabDropZone::Left | TabDropZone::Right => {
            assert_eq!((dragged.y, dragged.height), (target.y, target.height));
            if edge == TabDropZone::Left {
                assert!(
                    dragged.right() <= target.x,
                    "{dragged:?} must be left of {target:?}"
                );
            } else {
                assert!(
                    target.right() <= dragged.x,
                    "{dragged:?} must be right of {target:?}"
                );
            }
        }
        TabDropZone::Top | TabDropZone::Bottom => {
            assert_eq!((dragged.x, dragged.width), (target.x, target.width));
            if edge == TabDropZone::Top {
                assert!(
                    dragged.bottom() <= target.y,
                    "{dragged:?} must be above {target:?}"
                );
            } else {
                assert!(
                    target.bottom() <= dragged.y,
                    "{dragged:?} must be below {target:?}"
                );
            }
        }
    }
}

fn start_drag(app: &mut App, terminal: &mut Terminal<TestBackend>, source: usize) {
    start_tab_drag(app, terminal, source, 0);
}

fn start_tab_drag(app: &mut App, terminal: &mut Terminal<TestBackend>, source: usize, index: usize) {
    let area = if source == app.active_group {
        app.tab_bar_area.unwrap()
    } else {
        app.groups[source].tab_bar_area.unwrap()
    };
    let tab = crate::ui_chrome_tabs::TabBarHits::for_group(app, area, source)
        .tabs.into_iter().find(|(i, _, _)| *i == index).unwrap().1;
    mouse(
        app,
        MouseEventKind::Down(MouseButton::Left),
        (tab.x + 3, area.y),
    );
    assert_eq!(app.dragging_tab_source_group, Some(source));
    assert_eq!(app.dragging_tab, Some(index));
    draw(app, terminal);
}

fn finish_drag(app: &mut App, terminal: &mut Terminal<TestBackend>, position: (u16, u16)) {
    mouse(app, MouseEventKind::Drag(MouseButton::Left), position);
    draw(app, terminal);
    mouse(app, MouseEventKind::Up(MouseButton::Left), position);
    draw(app, terminal);
    assert!(app.dragging_tab.is_none());
    assert!(app.tab_drop_target_group.is_none());
    assert!(app.tab_drop_zone.is_none());
}

fn grouped_tabs(count: usize) -> App {
    let mut app = App::test_new();
    app.tabs = (0..count).map(|i| {
        let mut tab = tab(&format!("f{i}"));
        tab.editor.saved_rope = Some(tab.editor.rope.clone());
        tab
    }).collect();
    app.active_tab = 0;
    app.tab_mru_order = (0..count).rev().collect();
    app.desks.selected_tabs.extend(app.tabs.iter().map(|t| t.editor.navigation_id));
    app.group_selected_tabs("Feature", None);
    app
}

#[test]
fn tab_group_drag_to_empty_space_ungroups_without_changing_the_document_or_mru() {
    for count in [1, 3] {
        for source in 0..count {
            for onto_plus in [false, true] {
                let mut app = grouped_tabs(count);
                let group = app.tab_group(&app.tabs[source]).unwrap().id;
                let view = app.tabs[source].editor.navigation_id;
                app.tabs[source].editor.insert_char('!');
                let other_views: Vec<_> = app.tabs.iter().filter(|t| t.editor.navigation_id != view)
                    .map(|t| t.editor.navigation_id).collect();
                let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
                draw(&mut app, &mut terminal);
                let pane = app.active_group;
                start_tab_drag(&mut app, &mut terminal, pane, source);
                let mru: Vec<_> = app.tab_mru_order.iter().map(|&i| app.tabs[i].editor.navigation_id).collect();
                let area = app.tab_bar_area.unwrap();
                let hits = TabBarHits::new(&app, area);
                assert!(!hits.new_tab.is_empty());
                let x = if onto_plus { hits.new_tab.x + 1 } else { area.right() - 2 };
                finish_drag(&mut app, &mut terminal, (x, area.y));

                assert_eq!(app.tabs.len(), count, "dropping on + must not create another tab");
                assert_eq!(app.active_tab, count - 1);
                assert_eq!(app.editor().unwrap().navigation_id, view);
                assert!(app.tab_group(&app.tabs[count - 1]).is_none());
                assert_eq!(app.tabs[..count - 1].iter().map(|t| t.editor.navigation_id).collect::<Vec<_>>(), other_views);
                assert!(app.tabs[..count - 1].iter().all(|t| app.tab_group(t).unwrap().id == group));
                assert_eq!(app.tab_mru_order.iter().map(|&i| app.tabs[i].editor.navigation_id).collect::<Vec<_>>(), mru);
                assert_eq!(TabBarHits::new(&app, area).groups.len(), usize::from(count > 1));
                let editor = app.editor_mut().unwrap();
                assert_eq!(editor.rope.to_string(), format!("!f{source}"));
                assert!(editor.modified);
                editor.undo();
                assert_eq!(editor.rope.to_string(), format!("f{source}"));
                assert!(!editor.modified);
                editor.redo();
                assert_eq!(editor.rope.to_string(), format!("!f{source}"));
            }
        }
    }
}

#[test]
fn tab_group_drag_past_a_collapsed_group_preserves_hidden_members() {
    let mut app = grouped_tabs(4);
    let first = app.tab_group(&app.tabs[0]).unwrap().id;
    app.desks.selected_tabs.extend(app.tabs[2..].iter().map(|t| t.editor.navigation_id));
    app.group_selected_tabs("Tests", None);
    let second = app.tab_group(&app.tabs[2]).unwrap().id;
    app.desks.groups.get_mut(&second).unwrap().collapsed = true;
    let views: Vec<_> = app.tabs.iter().map(|t| t.editor.navigation_id).collect();
    let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
    draw(&mut app, &mut terminal);
    let pane = app.active_group;
    start_drag(&mut app, &mut terminal, pane);
    let area = app.tab_bar_area.unwrap();
    finish_drag(&mut app, &mut terminal, (area.right() - 2, area.y));
    assert_eq!(app.tabs.iter().map(|t| t.editor.navigation_id).collect::<Vec<_>>(), [views[1], views[2], views[3], views[0]]);
    assert_eq!(app.desks.membership.get(&views[1]), Some(&first));
    assert_eq!(app.desks.membership.get(&views[2]), Some(&second));
    assert_eq!(app.desks.membership.get(&views[3]), Some(&second));
    assert!(!app.desks.membership.contains_key(&views[0]));
    assert!(app.desks.groups[&second].collapsed);
    assert_eq!(TabBarHits::new(&app, area).tabs.len(), 2);
}

#[test]
fn tab_group_drag_uses_release_position_and_preserves_grouped_reordering() {
    for release_in_empty_space in [false, true] {
        let mut app = grouped_tabs(3);
        let view = app.tabs[0].editor.navigation_id;
        let group = app.tab_group(&app.tabs[0]).unwrap().id;
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        draw(&mut app, &mut terminal);
        let pane = app.active_group;
        start_drag(&mut app, &mut terminal, pane);
        let area = app.tab_bar_area.unwrap();
        let other_tab = TabBarHits::new(&app, area).tabs[1].1;
        let tab_position = (other_tab.x + 3, area.y);
        let empty = (area.right() - 2, area.y);
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), if release_in_empty_space { tab_position } else { empty });
        draw(&mut app, &mut terminal);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), if release_in_empty_space { empty } else { tab_position });
        draw(&mut app, &mut terminal);
        assert!(app.dragging_tab.is_none());
        assert_eq!(app.editor().unwrap().navigation_id, view);
        assert_eq!(app.desks.membership.get(&view).copied(), (!release_in_empty_space).then_some(group));
        if release_in_empty_space {
            assert_eq!(app.active_tab, 2);
        }
    }
}

#[test]
fn tab_group_drag_across_a_header_keeps_capture_until_released_in_empty_space() {
    let mut app = grouped_tabs(2);
    let view = app.tabs[0].editor.navigation_id;
    let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
    draw(&mut app, &mut terminal);
    let pane = app.active_group;
    start_drag(&mut app, &mut terminal, pane);
    let area = app.tab_bar_area.unwrap();
    let header = TabBarHits::new(&app, area).groups[0].1;
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), (header.x + 3, area.y));
    assert!(app.drag_has_moved);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), (area.right() - 2, area.y));
    assert!(app.dragging_tab.is_none());
    assert_eq!(app.active_tab, 1);
    assert_eq!(app.editor().unwrap().navigation_id, view);
    assert!(!app.desks.membership.contains_key(&view));
}

#[test]
fn tab_group_drag_only_ungroups_on_an_actual_drop_after_the_tabs() {
    for target in 0..5 {
        let mut app = grouped_tabs(2);
        let members = app.desks.membership.clone();
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        draw(&mut app, &mut terminal);
        let pane = app.active_group;
        start_drag(&mut app, &mut terminal, pane);
        let area = app.tab_bar_area.unwrap();
        let tab = TabBarHits::new(&app, area).tabs[0].1;
        let position = match target {
            0 => (tab.x + 3, area.y), // Ordinary click.
            1 => (area.right() - 2, area.y), // No drag event.
            2 => (tab.x + 4, area.y), // Movement within the tab.
            3 => (area.x + 1, area.y), // New-desk control.
            _ => { // Editor center, not an edge that creates a split.
                let editor = app.editor_area.unwrap();
                (editor.x + editor.width / 2, editor.y + editor.height / 2)
            }
        };
        if target >= 2 {
            mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), position);
        }
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), position);
        assert!(app.dragging_tab.is_none());
        assert_eq!(app.desks.membership, members);
        assert_eq!(app.tabs.len(), 2);
        assert!(app.desks.popup.mode.is_none());
    }
}

#[test]
fn tab_group_drag_to_another_panes_empty_strip_ungroups_even_when_source_closes() {
    for keep_source in [false, true] {
        let mut app = grouped_tabs(if keep_source { 3 } else { 2 });
        let view = app.tabs[0].editor.navigation_id;
        let group = app.tab_group(&app.tabs[0]).unwrap().id;
        app.create_split_group_dir(app.tabs.len() - 1, SplitDirection::Horizontal);
        app.group_selected_tabs("Destination", None);
        let target = app.active_group;
        let source = if target == 0 { 1 } else { 0 };
        let mut terminal = Terminal::new(TestBackend::new(200, 40)).unwrap();
        draw(&mut app, &mut terminal);
        let area = app.tab_bar_area.unwrap();
        let position = (area.right() - 2, area.y);
        start_drag(&mut app, &mut terminal, source);
        finish_drag(&mut app, &mut terminal, position);
        assert_eq!(app.editor().unwrap().navigation_id, view);
        assert!(!app.desks.membership.contains_key(&view));
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.split_active(), keep_source);
        if keep_source {
            assert!(app.groups.iter().flat_map(|g| &g.tabs).any(|t| app.tab_group(t).is_some_and(|g| g.id == group)));
        }
    }
}

#[test]
fn tab_group_drag_onto_group_headers_and_ungrouped_tabs_still_works() {
    for onto_group in [false, true] {
        let mut app = grouped_tabs(2);
        let view = app.tabs[0].editor.navigation_id;
        app.desks.membership.remove(&app.tabs[1].editor.navigation_id);
        if onto_group {
            app.desks.selected_tabs.insert(app.tabs[1].editor.navigation_id);
            app.group_selected_tabs("Tests", None);
        }
        let expected = app.desks.membership.get(&app.tabs[1].editor.navigation_id).copied();
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        draw(&mut app, &mut terminal);
        let pane = app.active_group;
        start_drag(&mut app, &mut terminal, pane);
        let area = app.tab_bar_area.unwrap();
        let hits = TabBarHits::new(&app, area);
        let target = if onto_group { hits.groups[1].1 } else { hits.tabs[1].1 };
        finish_drag(&mut app, &mut terminal, (target.x + 3, area.y));
        assert_eq!(app.editor().unwrap().navigation_id, view);
        assert_eq!(app.desks.membership.get(&view).copied(), expected);
        assert_eq!(app.tabs.len(), 2);
    }
}

#[test]
fn tab_group_drag_empty_drop_area_excludes_controls_and_clipped_tabs() {
    let app = grouped_tabs(3);
    let area = Rect::new(10, 2, 120, 1);
    let hits = TabBarHits::new(&app, area);
    assert!(hits.after_tabs.contains((hits.new_tab.x + 1, area.y).into()));
    assert_eq!(hits.after_tabs.right(), area.right());
    assert!(hits.groups.iter().all(|(_, r)| r.right() <= hits.after_tabs.x));
    assert!(hits.tabs.iter().all(|(_, r, _)| r.right() <= hits.after_tabs.x));
    assert!(!hits.after_tabs.contains((area.x + 1, area.y).into()));

    let clipped = (20..80).map(|width| TabBarHits::new(&app, Rect::new(10, 2, width, 1)))
        .find(|hits| !hits.right.is_empty() && !hits.new_tab.is_empty()).unwrap();
    assert!(clipped.after_tabs.is_empty(), "space after a scroll arrow is not the end of the strip");
}

fn assert_groups(app: &App, count: usize, tab_count: usize) {
    assert_eq!(app.groups.len(), count);
    assert_eq!(
        app.tabs.len() + app.groups.iter().map(|g| g.tabs.len()).sum::<usize>(),
        tab_count
    );
    if count == 0 {
        assert!(app.split_tree.is_none());
    } else {
        let mut leaves = app.split_tree.as_ref().unwrap().leaves();
        leaves.sort_unstable();
        assert_eq!(leaves, (0..count).collect::<Vec<_>>());
        assert!(app.groups[app.active_group].tabs.is_empty());
        assert!(
            app.groups
                .iter()
                .enumerate()
                .all(|(i, g)| i == app.active_group || !g.tabs.is_empty())
        );
    }
}

#[test]
fn pane_edge_drops_work_in_both_directions_on_both_axes() {
    for direction in [SplitDirection::Horizontal, SplitDirection::Vertical] {
        for source in 0..2 {
            for edge in EDGES {
                let mut app = App::test_new();
                app.tabs = vec![tab("first"), tab("second")];
                app.create_split_group_dir(1, direction);
                app.switch_group(source);
                let editor = app.editor_mut().unwrap();
                editor.modified = true;
                editor.cursor_col = 2;
                app.tabs[0].preview = true;
                // Begin from an unfocused source pane, as a user can do.
                app.switch_group(1 - source);
                let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
                draw(&mut app, &mut terminal);
                let names = ["first", "second"];
                let position = edge_position(area_for(&app, names[1 - source]), edge);
                start_drag(&mut app, &mut terminal, source);
                finish_drag(&mut app, &mut terminal, position);
                assert_groups(&app, 2, 2);
                assert_placement(
                    area_for(&app, names[source]),
                    area_for(&app, names[1 - source]),
                    edge,
                );
                let editor = app.editor().unwrap();
                assert_eq!(editor.rope.to_string(), names[source]);
                assert!(editor.modified);
                assert_eq!(editor.cursor_col, 2);
                assert!(!app.tabs[app.active_tab].preview);
            }
        }
    }
}

#[test]
fn pane_edge_drops_split_only_the_target_in_nested_layouts() {
    for source in 0..3 {
        for target in 0..3 {
            for edge in EDGES {
                let mut app = App::test_new();
                app.tabs = vec![tab("left"), tab("right")];
                app.create_split_group_dir(1, SplitDirection::Horizontal);
                app.tabs.push(tab("nested"));
                app.create_split_group_dir(1, SplitDirection::Vertical);
                app.switch_group(source);
                app.tabs.insert(0, tab("dragged"));
                app.active_tab = 0;
                let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
                draw(&mut app, &mut terminal);
                let names = ["left", "right", "nested"];
                let original: Vec<_> = names.iter().map(|name| area_for(&app, name)).collect();
                let position = edge_position(original[target], edge);
                start_drag(&mut app, &mut terminal, source);
                finish_drag(&mut app, &mut terminal, position);
                assert_groups(&app, 4, 4);
                assert_eq!(app.editor().unwrap().rope.to_string(), "dragged");
                assert_placement(
                    area_for(&app, "dragged"),
                    area_for(&app, names[target]),
                    edge,
                );
                for other in 0..3 {
                    if other != target {
                        let area = area_for(&app, names[other]);
                        // Flattened splits may round differently by one cell.
                        assert!(area.x.abs_diff(original[other].x) <= 1);
                        assert!(area.y.abs_diff(original[other].y) <= 1);
                        assert!(area.width.abs_diff(original[other].width) <= 1);
                        assert!(area.height.abs_diff(original[other].height) <= 1);
                    }
                }
            }
        }
    }
}

#[test]
fn pane_center_and_tab_bar_drops_focus_the_moved_tab() {
    for direction in [SplitDirection::Horizontal, SplitDirection::Vertical] {
        for source in 0..2 {
            for keep_source in [false, true] {
                for tab_bar in [false, true] {
                    let mut app = App::test_new();
                    app.tabs = vec![tab("first"), tab("second")];
                    app.create_split_group_dir(1, direction);
                    app.switch_group(source);
                    if keep_source {
                        app.tabs.push(tab("retained"));
                    }
                    let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
                    draw(&mut app, &mut terminal);
                    let target = &app.groups[1 - source];
                    let position = if tab_bar {
                        let area = target.tab_bar_area.unwrap();
                        (area.x + 3, area.y)
                    } else {
                        let area = target.editor_area.unwrap();
                        (area.x + area.width / 2, area.y + area.height / 2)
                    };
                    start_drag(&mut app, &mut terminal, source);
                    finish_drag(&mut app, &mut terminal, position);
                    assert_groups(
                        &app,
                        if keep_source { 2 } else { 0 },
                        if keep_source { 3 } else { 2 },
                    );
                    assert_eq!(app.tabs.len(), 2);
                    assert_eq!(
                        app.editor().unwrap().rope.to_string(),
                        if source == 0 { "first" } else { "second" }
                    );
                }
            }
        }
    }
}

#[test]
fn pane_drop_uses_release_position_even_without_a_final_drag_event() {
    for outside in [false, true] {
        let mut app = App::test_new();
        app.tabs = vec![tab("first"), tab("second")];
        app.create_split_group_dir(1, SplitDirection::Horizontal);
        let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
        draw(&mut app, &mut terminal);
        let original = [area_for(&app, "first"), area_for(&app, "second")];
        let hover = edge_position(original[0], TabDropZone::Bottom);
        start_drag(&mut app, &mut terminal, 1);
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), hover);
        draw(&mut app, &mut terminal);
        let release = if outside {
            (0, 0)
        } else {
            edge_position(original[0], TabDropZone::Left)
        };
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), release);
        draw(&mut app, &mut terminal);
        assert_groups(&app, 2, 2);
        if outside {
            assert_eq!(area_for(&app, "first"), original[0]);
            assert_eq!(area_for(&app, "second"), original[1]);
        } else {
            assert_placement(
                area_for(&app, "second"),
                area_for(&app, "first"),
                TabDropZone::Left,
            );
        }
    }
}

#[test]
fn pane_dropping_the_only_tab_on_its_own_edge_keeps_the_layout() {
    for split in [false, true] {
        for edge in EDGES {
            let mut app = App::test_new();
            app.tabs = vec![tab("first")];
            if split {
                app.tabs.push(tab("second"));
                app.create_split_group_dir(1, SplitDirection::Horizontal);
            }
            let name = if split { "second" } else { "first" };
            let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
            draw(&mut app, &mut terminal);
            let original = area_for(&app, name);
            let source = app.active_group;
            start_drag(&mut app, &mut terminal, source);
            finish_drag(&mut app, &mut terminal, edge_position(original, edge));
            assert_groups(&app, if split { 2 } else { 0 }, if split { 2 } else { 1 });
            assert_eq!(area_for(&app, name), original);
        }
    }
}

#[test]
fn pane_last_tab_drops_keep_the_nested_destination_after_compaction() {
    for source in 0..3 {
        for target in (0..3).filter(|&target| target != source) {
            for edge in EDGES {
                let mut app = App::test_new();
                app.tabs = vec![tab("left"), tab("right")];
                app.create_split_group_dir(1, SplitDirection::Horizontal);
                app.tabs.push(tab("nested"));
                app.create_split_group_dir(1, SplitDirection::Vertical);
                let names = ["left", "right", "nested"];
                let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
                draw(&mut app, &mut terminal);
                let position = edge_position(area_for(&app, names[target]), edge);
                start_drag(&mut app, &mut terminal, source);
                finish_drag(&mut app, &mut terminal, position);
                assert_groups(&app, 3, 3);
                assert_eq!(app.editor().unwrap().rope.to_string(), names[source]);
                assert_placement(
                    area_for(&app, names[source]),
                    area_for(&app, names[target]),
                    edge,
                );
            }
        }
    }
}

#[test]
fn pane_terminal_drops_honor_each_edge() {
    for edge in EDGES {
        let mut app = App::test_new();
        app.tabs = vec![tab("file")];
        let terminal_count = app.terminals.len();
        let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
        draw(&mut app, &mut terminal);
        let position = edge_position(area_for(&app, "file"), edge);
        app.dragging_terminal_tab = Some(0);
        app.drag_has_moved = true;
        // Stay clear of the outer terminal dock targets to request an editor split.
        let position = match edge {
            TabDropZone::Right => (position.0 - 2, position.1),
            TabDropZone::Bottom => (position.0, position.1 - 2),
            _ => position,
        };
        finish_drag(&mut app, &mut terminal, position);
        assert_groups(&app, 2, 2);
        assert_eq!(app.terminals.len(), terminal_count);
        assert_eq!(app.tabs[app.active_tab].kind, TabKind::Terminal);
        assert_eq!(app.tabs[app.active_tab].terminal_idx, Some(0));
        assert_placement(app.editor_area.unwrap(), area_for(&app, "file"), edge);
    }
}
