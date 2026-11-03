use super::*;
use crate::{app::Tab, editor::Editor};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, style::Color};
use std::collections::BTreeMap;

fn fixture(names: &[&str], width: u16) -> (App, Terminal<TestBackend>, Rect) {
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
    app.terminal_has_focus = true;
    app.animation_settings.set(AnimationEffect::TabSlide, true);
    app.animation_settings
        .set_duration(AnimationEffect::TabSlide, 240);
    let area = Rect::new(3, 1, width, 1);
    app.tab_bar_area = Some(area);
    (
        app,
        Terminal::new(TestBackend::new(width + 8, 4)).unwrap(),
        area,
    )
}

fn draw(app: &mut App, terminal: &mut Terminal<TestBackend>, area: Rect, now: Instant) {
    app.animations.begin_frame();
    terminal
        .draw(|frame| {
            let full = frame.area();
            frame.buffer_mut().set_style(
                full,
                Style::default()
                    .fg(app.colors.fg_text)
                    .bg(app.colors.bg_editor),
            );
            super::super::draw_tab_bar_at(frame, app, area, now);
        })
        .unwrap();
}

fn positions(app: &App, area: Rect) -> BTreeMap<u64, Rect> {
    TabBarHits::new(app, area)
        .tabs
        .into_iter()
        .map(|(index, rect, _)| (app.tabs[index].editor.navigation_id, rect))
        .collect()
}

fn text(terminal: &Terminal<TestBackend>, area: Rect) -> String {
    (area.x..area.right())
        .map(|x| terminal.backend().buffer()[(x, area.y)].symbol())
        .collect()
}

fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16) {
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        },
    );
    app.ensure_active_tab_visible();
}

#[test]
fn reordered_tab_and_every_displaced_neighbor_slide_then_leave_no_frame_timer() {
    let (mut app, mut terminal, area) = fixture(
        &["ALPHA.rs", "BRAVO-WIDE.rs", "CHARLIE.rs", "DELTA.rs"],
        100,
    );
    let start = Instant::now();
    draw(&mut app, &mut terminal, area, start);
    let before = positions(&app, area);
    let moving = app.tabs[0].editor.navigation_id;
    app.move_tab(0, 3);
    assert_eq!(
        app.tabs[3].editor.navigation_id, moving,
        "the order changes immediately"
    );
    let target: BTreeMap<_, _> =
        super::super::TabBarHits::for_group_with_names(&app, area, 0, None)
            .tabs
            .into_iter()
            .map(|(index, rect, _)| (app.tabs[index].editor.navigation_id, rect))
            .collect();
    let began = start + Duration::from_millis(1);
    draw(&mut app, &mut terminal, area, began);
    assert_eq!(
        positions(&app, area),
        before,
        "the first animation frame must not jump"
    );
    assert!(
        app.animations
            .next_frame_wait(&app.animation_settings, began)
            .is_some()
    );

    app.rendering_animation_frame = true;
    draw(
        &mut app,
        &mut terminal,
        area,
        began + Duration::from_millis(35),
    );
    let midway = positions(&app, area);
    for (&id, old) in &before {
        let current = midway[&id].x;
        let destination = target[&id].x;
        assert!(
            current > old.x.min(destination) && current < old.x.max(destination),
            "every displaced label must have an intermediate position: {old:?} -> {current} -> {destination}"
        );
    }
    assert!(text(&terminal, area).contains("ALPHA.rs"));

    let finished = start + Duration::from_secs(2);
    assert!(
        app.animations
            .advance_frame(&app.animation_settings, finished)
    );
    draw(&mut app, &mut terminal, area, finished);
    assert_eq!(positions(&app, area), target);
    assert!(
        app.animations
            .next_frame_wait(&app.animation_settings, finished)
            .is_none()
    );
    let animated = terminal.backend().buffer().clone();
    app.animation_settings.set(AnimationEffect::TabSlide, false);
    app.rendering_animation_frame = false;
    draw(&mut app, &mut terminal, area, finished);
    for (animated, plain) in animated
        .content
        .iter()
        .zip(&terminal.backend().buffer().content)
    {
        assert_eq!(
            (animated.symbol(), animated.fg, animated.bg),
            (plain.symbol(), plain.fg, plain.bg)
        );
    }
}

#[test]
fn rapid_reversals_preserve_all_current_positions_and_momentum() {
    let (mut app, mut terminal, area) =
        fixture(&["ALPHA.rs", "BRAVO.rs", "CHARLIE.rs", "DELTA.rs"], 100);
    let start = Instant::now();
    draw(&mut app, &mut terminal, area, start);
    app.move_tab(0, 3);
    draw(&mut app, &mut terminal, area, start);
    let interrupted = start + Duration::from_millis(30);
    draw(&mut app, &mut terminal, area, interrupted);
    let before = positions(&app, area);
    let samples: BTreeMap<_, _> = app
        .animations
        .tab_reorder
        .borrow()
        .tracks
        .iter()
        .map(|track| (track.label.id, track.sample(interrupted)))
        .collect();
    app.move_tab(3, 0);
    draw(&mut app, &mut terminal, area, interrupted);
    assert_eq!(positions(&app, area), before);
    for track in &app.animations.tab_reorder.borrow().tracks {
        let (position, velocity) = track.sample(interrupted);
        let (old_position, old_velocity) = samples[&track.label.id];
        assert!((position - old_position).abs() < 0.0001);
        assert!((velocity - old_velocity).abs() < 0.001);
    }
    let finished = start + Duration::from_secs(2);
    app.animations
        .advance_frame(&app.animation_settings, finished);
    draw(&mut app, &mut terminal, area, finished);
    assert!(text(&terminal, area).find("ALPHA.rs") < text(&terminal, area).find("BRAVO.rs"));
    assert!(
        app.animations
            .next_frame_wait(&app.animation_settings, finished)
            .is_none()
    );
}

#[test]
fn drag_targets_stay_logical_and_clicks_and_close_buttons_follow_the_presented_labels() {
    let (mut app, mut terminal, area) = fixture(
        &["ALPHA.rs", "BRAVO-WIDE.rs", "CHARLIE.rs", "DELTA.rs"],
        100,
    );
    let start = Instant::now();
    draw(&mut app, &mut terminal, area, start);
    let hits = TabBarHits::new(&app, area);
    let moving = app.tabs[0].editor.navigation_id;
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        hits.tabs[0].1.x + 1,
        area.y,
    );
    let target = hits.tabs.last().unwrap().1.right() - 1;
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        target,
        area.y,
    );
    draw(&mut app, &mut terminal, area, start);
    for step in 1..5 {
        let logical = TabBarHits::new(&app, area);
        assert_eq!(logical.tabs.last().unwrap().0, 3);
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            target,
            area.y,
        );
        draw(
            &mut app,
            &mut terminal,
            area,
            start + Duration::from_millis(step * 5),
        );
        assert_eq!(app.dragging_tab, Some(3));
        assert_eq!(app.editor().unwrap().navigation_id, moving);
    }
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        target,
        area.y,
    );
    let hits = TabBarHits::new(&app, area);
    let (_, rect, _) = hits
        .tabs
        .iter()
        .copied()
        .find(|(index, _, _)| app.tabs[*index].editor.navigation_id == moving)
        .unwrap();
    app.active_tab = 0;
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        rect.x + 1,
        area.y,
    );
    assert_eq!(app.editor().unwrap().navigation_id, moving);
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        rect.x + 1,
        area.y,
    );
    draw(
        &mut app,
        &mut terminal,
        area,
        start + Duration::from_millis(25),
    );
    let hits = TabBarHits::new(&app, area);
    let close = hits
        .tabs
        .iter()
        .find(|(index, _, _)| app.tabs[*index].editor.navigation_id == moving)
        .unwrap()
        .2;
    assert_eq!(
        terminal.backend().buffer()[(close.x + 1, close.y)].symbol(),
        "×"
    );
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        close.x + 1,
        area.y,
    );
    assert!(
        !app.tabs
            .iter()
            .any(|tab| tab.editor.navigation_id == moving)
    );
}

#[test]
fn scrolling_resizing_losing_focus_and_disabling_motion_cancel_reorder_cleanly() {
    for action in 0..4 {
        let (mut app, mut terminal, mut area) =
            fixture(&["ALPHA.rs", "BRAVO.rs", "CHARLIE.rs"], 100);
        let start = Instant::now();
        draw(&mut app, &mut terminal, area, start);
        app.move_tab(0, 2);
        draw(&mut app, &mut terminal, area, start);
        assert!(
            app.animations
                .next_frame_wait(&app.animation_settings, start)
                .is_some()
        );
        match action {
            0 => super::super::scroll_strip_by(&mut app, area, 0, 3),
            1 => {
                area.width = 60;
                app.tab_bar_area = Some(area);
            }
            2 => app.terminal_has_focus = false,
            _ => app.animation_settings.set(AnimationEffect::TabSlide, false),
        }
        let now = start + Duration::from_millis(15);
        draw(&mut app, &mut terminal, area, now);
        assert!(!app.animations.tab_reorder.borrow().active());
        assert!(
            app.animations
                .next_frame_wait(&app.animation_settings, now)
                .is_none()
        );
    }
}

#[test]
fn keyboard_reorder_preserves_a_scrolled_viewport_and_animates_both_tabs() {
    let (mut app, mut terminal, area) =
        fixture(&["ALPHA.rs", "BRAVO.rs", "CHARLIE.rs", "DELTA.rs"], 42);
    let start = Instant::now();
    super::super::scroll_strip_by(&mut app, area, 0, 3);
    draw(&mut app, &mut terminal, area, start);
    let before = positions(&app, area);
    let moving = app.tabs[0].editor.navigation_id;
    app.execute_palette_command("move_tab_right");
    app.ensure_active_tab_visible();
    assert_eq!(strip_scroll_position(&app, 0).unwrap().columns, 3);
    assert_eq!(app.editor().unwrap().navigation_id, moving);
    draw(&mut app, &mut terminal, area, start);
    assert_eq!(positions(&app, area), before);
    draw(
        &mut app,
        &mut terminal,
        area,
        start + Duration::from_millis(40),
    );
    assert_ne!(positions(&app, area), before);
    assert_eq!(strip_scroll_position(&app, 0).unwrap().columns, 3);
}

#[test]
fn clipped_unicode_labels_and_named_groups_keep_fixed_controls_and_pins_intact() {
    let (mut app, mut terminal, area) = fixture(
        &[
            "界😀-ALPHA.rs",
            "BRAVO.rs",
            "e\u{301}-CHARLIE.rs",
            "DELTA.rs",
            "ECHO.rs",
        ],
        95,
    );
    app.tabs[1].pinned = true;
    app.desks
        .selected_tabs
        .extend(app.tabs[..3].iter().map(|tab| tab.editor.navigation_id));
    app.group_selected_tabs("Feature", None);
    super::super::scroll_strip_by(&mut app, area, 0, 9);
    let start = Instant::now();
    draw(&mut app, &mut terminal, area, start);
    let prefix = crate::ui_desks::controls(&app, area, 0).2;
    let controls: Vec<_> = (area.x..area.x + prefix)
        .map(|x| terminal.backend().buffer()[(x, area.y)].clone())
        .collect();
    app.move_tab(2, 0);
    draw(&mut app, &mut terminal, area, start);
    app.rendering_animation_frame = true;
    for elapsed in [10, 35, 70, 150] {
        draw(
            &mut app,
            &mut terminal,
            area,
            start + Duration::from_millis(elapsed),
        );
        let hits = TabBarHits::new(&app, area);
        for (index, rect, close) in hits.tabs {
            assert_eq!(rect.intersection(area), rect);
            if app.tabs[index].pinned {
                assert!(close.is_empty());
            }
        }
        for (offset, control) in controls.iter().enumerate() {
            assert_eq!(
                &terminal.backend().buffer()[(area.x + offset as u16, area.y)],
                control
            );
        }
        assert_eq!(strip_scroll_position(&app, 0).unwrap().columns, 9);
    }
}

#[test]
fn disabled_tab_motion_keeps_reordering_immediate_and_allocates_no_scene() {
    let (mut app, mut terminal, area) = fixture(&["ALPHA.rs", "BRAVO.rs"], 70);
    app.animation_settings.set(AnimationEffect::TabSlide, false);
    let now = Instant::now();
    draw(&mut app, &mut terminal, area, now);
    app.move_tab(0, 1);
    draw(&mut app, &mut terminal, area, now);
    let content = text(&terminal, area);
    assert!(content.find("BRAVO.rs") < content.find("ALPHA.rs"));
    assert!(app.animations.tab_reorder.borrow().scene.is_none());
    assert!(
        app.animations
            .next_frame_wait(&app.animation_settings, now)
            .is_none()
    );
}

#[test]
fn overflow_reorders_animate_entering_and_leaving_labels_without_covering_scroll_arrows() {
    let (mut app, mut terminal, area) = fixture(
        &[
            "ALPHA.rs",
            "BRAVO-WIDE.rs",
            "CHARLIE.rs",
            "DELTA.rs",
            "ECHO.rs",
        ],
        40,
    );
    let start = Instant::now();
    draw(&mut app, &mut terminal, area, start);
    let old = positions(&app, area);
    let moving = app.tabs[0].editor.navigation_id;
    app.move_tab(0, 3);
    draw(&mut app, &mut terminal, area, start);
    assert_eq!(positions(&app, area)[&moving], old[&moving]);
    assert!(
        app.animations
            .tab_reorder
            .borrow()
            .tracks
            .iter()
            .any(|track| !track.visible_target)
    );
    app.rendering_animation_frame = true;
    for elapsed in [15, 40, 90, 170] {
        draw(
            &mut app,
            &mut terminal,
            area,
            start + Duration::from_millis(elapsed),
        );
        let hits = TabBarHits::new(&app, area);
        assert_eq!(
            terminal.backend().buffer()[(hits.right.x + 1, area.y)].symbol(),
            "▸"
        );
        assert!(hits.tab_at(hits.right.x).is_none());
        for (_, rect, _) in &hits.tabs {
            assert_eq!(rect.intersection(area), *rect);
        }
    }
    let end = start + Duration::from_secs(1);
    app.animations.advance_frame(&app.animation_settings, end);
    draw(&mut app, &mut terminal, area, end);
    assert!(
        app.animations
            .next_frame_wait(&app.animation_settings, end)
            .is_none()
    );
}

#[test]
fn reorder_state_stays_bounded_by_the_viewport_during_a_coalesced_burst() {
    let names: Vec<_> = (0..150)
        .map(|index| format!("file-{index:03}.rs"))
        .collect();
    let (mut app, mut terminal, area) =
        fixture(&names.iter().map(String::as_str).collect::<Vec<_>>(), 60);
    let start = Instant::now();
    draw(&mut app, &mut terminal, area, start);
    for _ in 0..60 {
        app.move_tab(149, 0);
        draw(&mut app, &mut terminal, area, start);
        let state = app.animations.tab_reorder.borrow();
        let visible = state
            .tracks
            .iter()
            .filter(|track| track.visible_target)
            .count();
        assert!(state.tracks.len() <= visible * 2 + 1);
    }
    let end = start + Duration::from_secs(1);
    app.animations.advance_frame(&app.animation_settings, end);
    draw(&mut app, &mut terminal, area, end);
    assert!(
        app.animations
            .next_frame_wait(&app.animation_settings, end)
            .is_none()
    );
}

#[test]
fn motion_keeps_file_icon_colors_and_restores_exact_gradient_backgrounds() {
    let (mut app, mut terminal, area) = fixture(&["ALPHA.rs", "BRAVO.py", "CHARLIE.json"], 100);
    app.colors.gradients.bg_tabbar.start = Color::Rgb(18, 25, 37);
    app.colors.gradients.bg_tabbar.end = Color::Rgb(80, 95, 110);
    let start = Instant::now();
    draw(&mut app, &mut terminal, area, start);
    let moving = app.tabs[0].editor.navigation_id;
    let icon_color = crate::file_icons::for_editor(&app.tabs[0].editor).1;
    app.move_tab(0, 2);
    draw(&mut app, &mut terminal, area, start);
    app.rendering_animation_frame = true;
    let mid = start + Duration::from_millis(35);
    draw(&mut app, &mut terminal, area, mid);
    {
        let state = app.animations.tab_reorder.borrow();
        let track = state
            .tracks
            .iter()
            .find(|track| track.label.id == moving)
            .unwrap();
        let icon = track.label.icon.as_ref().unwrap();
        if !icon.is_empty() {
            let x = track.sample(mid).0.round() as u16 + icon.start as u16;
            assert_eq!(terminal.backend().buffer()[(x, area.y)].fg, icon_color);
        }
    }
    let end = start + Duration::from_secs(1);
    app.animations.advance_frame(&app.animation_settings, end);
    draw(&mut app, &mut terminal, area, end);
    let animated = terminal.backend().buffer().clone();
    app.animation_settings.set(AnimationEffect::TabSlide, false);
    app.rendering_animation_frame = false;
    draw(&mut app, &mut terminal, area, end);
    for (animated, plain) in animated
        .content
        .iter()
        .zip(&terminal.backend().buffer().content)
    {
        assert_eq!(
            (animated.symbol(), animated.fg, animated.bg),
            (plain.symbol(), plain.fg, plain.bg)
        );
    }
}

#[test]
#[ignore = "exports deterministic production reorder frames for visual review"]
fn export_tab_reorder_animation_preview() {
    let (mut app, mut terminal, area) = fixture(
        &["main.rs", "scope_connector.rs", "theme.json", "README.md"],
        85,
    );
    app.animation_settings
        .set_duration(AnimationEffect::TabSlide, 400);
    let start = Instant::now();
    draw(&mut app, &mut terminal, area, start);
    fn color(color: Color) -> String {
        match color {
            Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            _ => "#c0c5ce".into(),
        }
    }
    let mut frames = Vec::new();
    for index in 0..110 {
        match index {
            8 => app.move_tab(0, 3),
            35 => app.move_tab(3, 1),
            41 => app.move_tab(1, 0),
            70 => app.move_tab(0, 2),
            _ => {}
        }
        draw(
            &mut app,
            &mut terminal,
            area,
            start + Duration::from_millis(index * 16),
        );
        frames.push(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| {
                    (
                        cell.symbol().to_owned(),
                        color(cell.fg),
                        color(cell.bg),
                        false,
                        false,
                        cell.modifier
                            .contains(ratatui::style::Modifier::UNDERLINED)
                            .then(|| {
                                color(if cell.underline_color == Color::Reset {
                                    cell.fg
                                } else {
                                    cell.underline_color
                                })
                            }),
                    )
                })
                .collect::<Vec<_>>(),
        );
    }
    let scene = serde_json::json!({ "name": "Reordering tabs", "width": area.width + 8,
        "height": 4, "frames": frames });
    std::fs::write(
        "target/tab-reorder-animation-preview.json",
        serde_json::to_vec(&scene).unwrap(),
    )
    .unwrap();
}
