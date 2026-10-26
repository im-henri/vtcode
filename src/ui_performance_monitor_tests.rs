use super::*;
use crate::{
    app::Tab,
    editor::Editor,
    process_usage::{Counters, SAMPLE_INTERVAL},
};
use ratatui::{Terminal, backend::TestBackend};

fn fixture() -> App {
    let mut app = App::test_new();
    app.show_explorer = false;
    app.show_panel = false;
    app.show_minimap = false;
    app.active_pane = crate::app::ActivePane::Editor;
    app.tabs = vec![Tab::new_file(Editor::test_with_text(
        &"first line\nsecond line\n".repeat(200),
    ))];
    app.active_tab = 0;
    let start = Instant::now() - SAMPLE_INTERVAL * 12;
    for index in 0..=12 {
        app.performance_view
            .resources
            .refresh_with(true, start + SAMPLE_INTERVAL * index, || Counters {
                cpu_time: Some(Duration::from_secs(100 + (index * 2) as u64)),
                resident_bytes: Some((96 + index as u64) * 1024 * 1024),
            });
    }
    for ms in [1, 2, 3, 4, 26, 3, 2, 5, 2, 1, 2, 3] {
        app.background
            .activity
            .render
            .record(Duration::from_millis(ms));
    }
    app
}

fn click(app: &mut App, hit: Hit) {
    let area = app
        .performance_view
        .layout
        .controls
        .iter()
        .find(|(_, value)| *value == hit)
        .unwrap()
        .0;
    pointer(
        app,
        MouseEventKind::Down(MouseButton::Left),
        area.x + area.width / 2,
        area.y,
    );
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

fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, modifiers));
}

fn paint(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
    terminal
}

fn text(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn pinned_monitor_keeps_editor_keyboard_mouse_scroll_and_undo_working() {
    let mut app = fixture();
    toggle(&mut app);
    paint(&mut app, 140, 42);
    click(&mut app, Hit::Pin);
    assert!(app.performance_view.pinned);
    assert!(!captures_keyboard(&app));
    paint(&mut app, 140, 42);
    let monitor = app.performance_area.unwrap();
    let code = app.code_area.unwrap();
    assert!(monitor.intersection(code).is_empty());
    let original = app.editor().unwrap().rope.to_string();
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(app.editor().unwrap().cursor_col, 1);
    key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(app.editor().unwrap().rope.to_string().starts_with("fxirst"));
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(app.editor().unwrap().rope.to_string(), original);
    assert!(app.background.activity.details_open);
    for code in [
        KeyCode::Tab,
        KeyCode::Right,
        KeyCode::Esc,
        KeyCode::Char('c'),
        KeyCode::Char('r'),
    ] {
        assert!(!handle_key(
            &mut app,
            KeyEvent::new(code, KeyModifiers::CONTROL)
        ));
    }
    pointer(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        code.x + 3,
        code.y + 1,
    );
    assert_eq!(app.editor().unwrap().cursor_line, 1);
    pointer(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        code.x + 3,
        code.y + 1,
    );
    pointer(&mut app, MouseEventKind::ScrollDown, code.x + 3, code.y + 1);
    assert!(app.editor().unwrap().scroll_offset > 0);
    assert!(app.background.activity.details_open);
    assert!(!captures_keyboard(&app));
    app.middle_drag_start = Some((0, 0, 0));
    pointer(
        &mut app,
        MouseEventKind::Up(MouseButton::Middle),
        monitor.x + 2,
        monitor.y + 6,
    );
    assert!(
        app.middle_drag_start.is_none(),
        "the editor must receive a release over the dock"
    );
    paint(&mut app, 140, 42);
    click(&mut app, Hit::Tab(Section::Recent));
    assert!(captures_keyboard(&app));
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.performance_view.section, Section::Workspace);
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.background.activity.details_open);
    assert!(!captures_keyboard(&app));
    paint(&mut app, 140, 42);
    click(&mut app, Hit::Close);
    assert!(!app.background.activity.details_open);
    paint(&mut app, 140, 42);
    assert!(app.code_area.unwrap().width > code.width);
    app.execute_palette_command("performance");
    assert!(app.background.activity.details_open);
    assert!(app.performance_view.pinned);
    assert!(
        captures_keyboard(&app),
        "the palette must also focus a reopened dock"
    );
    app.execute_palette_command("performance");
    assert!(
        app.background.activity.details_open,
        "Show keeps an existing monitor open"
    );
}

#[test]
fn pinned_monitor_survives_palette_context_menu_status_details_and_focus_changes() {
    let mut app = fixture();
    toggle(&mut app);
    toggle_pin(&mut app);
    paint(&mut app, 140, 42);
    let dock = app.performance_area;
    for overlay in 0..4 {
        match overlay {
            0 => app.palette.active = true,
            1 => app.context_menu_active = true,
            2 => app.menu_open = true,
            _ => crate::ui_status::open_details(&mut app),
        }
        paint(&mut app, 140, 42);
        assert!(app.background.activity.details_open);
        assert_eq!(app.performance_area, dock);
        assert!(!captures_keyboard(&app));
        assert_eq!(resource_next_wait(&app, Instant::now()), None);
        app.palette.active = false;
        app.context_menu_active = false;
        app.menu_open = false;
        app.status_details.open = false;
    }
    app.terminal_has_focus = false;
    assert!(!poll_resources(&mut app, Instant::now() + SAMPLE_INTERVAL));
    paint(&mut app, 140, 42);
    assert!(app.background.activity.details_open);
    app.terminal_has_focus = true;
    assert!(resource_next_wait(&app, Instant::now()).is_some());
    let samples = app.performance_view.resources.samples;
    for _ in 0..100 {
        paint(&mut app, 140, 42);
    }
    assert_eq!(app.performance_view.resources.samples, samples);
}

#[test]
fn pinned_open_file_keeps_monitor_and_returns_focus_to_editor() {
    let mut app = fixture();
    let path = app.explorer.root.join("existing-monitor-test.rs");
    app.tabs[0].editor.file_path = Some(path.clone());
    let _ticket = app.background.activity.start("Highlighting", Some(&path));
    app.background.refresh_activity(Instant::now());
    toggle(&mut app);
    toggle_pin(&mut app);
    app.performance_view.switch(Section::Running);
    paint(&mut app, 160, 42);
    click(&mut app, Hit::OpenFile);
    assert!(app.background.activity.details_open);
    assert!(!captures_keyboard(&app));
    assert_eq!(app.tabs.len(), 1);
}

#[test]
fn pinning_keeps_the_live_cpu_baseline_and_sampling_deadline() {
    let mut app = fixture();
    toggle(&mut app);
    paint(&mut app, 140, 42);
    let mut sampled = app.performance_view.resources.sampled_at().unwrap();
    let mut cpu = 124;
    for _ in 0..4 {
        toggle_pin(&mut app);
        assert!(
            resources_visible(&app),
            "docking must not look like closing the view"
        );
        assert!(!poll_resources(&mut app, sampled));
        sampled += SAMPLE_INTERVAL;
        cpu += 2;
        app.performance_view
            .resources
            .refresh_with(true, sampled, || Counters {
                cpu_time: Some(Duration::from_secs(cpu)),
                resident_bytes: Some(100 * 1024 * 1024),
            });
        assert_eq!(app.performance_view.resources.cpu_percent, Some(20.0));
        paint(&mut app, 140, 42);
    }
}

#[test]
fn pinned_monitor_is_readable_in_builtin_and_imported_themes() {
    let mut app = fixture();
    toggle(&mut app);
    toggle_pin(&mut app);
    let mut failures = Vec::new();
    for theme in crate::ui_control_visibility_tests::audit_themes() {
        app.colors = theme.to_theme_colors();
        for (width, height) in [(80, 24), (140, 42)] {
            let terminal = paint(&mut app, width, height);
            let area = app.performance_area.unwrap();
            crate::ui_control_visibility_tests::check_region(
                terminal.backend().buffer(),
                Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2),
                &format!("{} pinned {width}x{height}", theme.name),
                &mut failures,
                &app.colors.ui,
            );
        }
    }
    crate::ui_control_visibility_tests::assert_readable(failures);
}

#[test]
fn pinned_layouts_leave_editor_space_and_all_controls_survive_resize() {
    let mut app = fixture();
    toggle(&mut app);
    toggle_pin(&mut app);
    for (width, height) in [(8, 4), (20, 8), (40, 14), (80, 24), (110, 32), (180, 48)] {
        for section in Section::ALL {
            app.performance_view.switch(section);
            let terminal = paint(&mut app, width, height);
            let area = app.performance_area.unwrap();
            assert_eq!(area, area.intersection(Rect::new(0, 0, width, height)));
            assert!(app.editor_area.unwrap().intersection(area).is_empty());
            for (control, _) in &app.performance_view.layout.controls {
                assert_eq!(*control, control.intersection(area));
            }
            assert!(
                app.performance_view
                    .layout
                    .controls
                    .iter()
                    .any(|(_, hit)| *hit == Hit::Close)
            );
            if width >= 40 {
                assert!(text(&terminal).contains("Unpin"));
            }
        }
    }
    paint(&mut app, 140, 42);
    click(&mut app, Hit::Pin);
    assert!(!app.performance_view.pinned);
    assert!(captures_keyboard(&app));
    paint(&mut app, 140, 42);
    assert!(app.performance_view.dock_area.is_none());
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.background.activity.details_open);
}

#[test]
fn overview_reports_real_values_scope_and_tail_latency_without_document_contents() {
    let mut app = fixture();
    toggle(&mut app);
    let terminal = paint(&mut app, 130, 40);
    let shown = text(&terminal);
    for value in [
        "CPU",
        "20.0%",
        "Memory",
        "108.0 MiB",
        "Peak sampled",
        "UI frame time",
        "P95",
        "over 16.7ms",
        "updates every 10s",
        "Pin",
        "Workspace",
    ] {
        assert!(shown.contains(value), "missing {value}");
    }
    let report = report(&app, Instant::now());
    for value in [
        "PID",
        "Peak sampled resident memory",
        "60 Hz budget",
        "not input latency or FPS",
        "ACTIVE DOCUMENT",
        "Word wrap: Off",
        "LANGUAGE SERVERS",
    ] {
        assert!(report.contains(value), "missing {value}");
    }
    assert!(!report.contains("first line"));
    let samples = app.performance_view.resources.samples;
    for _ in 0..100 {
        let _ = super::report(&app, Instant::now());
    }
    assert_eq!(app.performance_view.resources.samples, samples);
}

#[test]
fn monitor_snapshot_gallery_covers_popover_dock_and_compact_views() {
    let mut app = fixture();
    let path = app.explorer.root.join("src/syntax_cache.rs");
    let _ticket = app.background.activity.start("Highlighting", Some(&path));
    app.background.activity.active[0].started -= Duration::from_secs(3);
    app.background.refresh_activity(Instant::now());
    let _finished = app.background.activity.start("Refreshing Git", None);
    let finished = app.background.activity.active.last_mut().unwrap();
    finished.started -= Duration::from_millis(800);
    finished.finish_for_test(Duration::from_millis(800));
    app.background.refresh_activity(Instant::now());
    toggle(&mut app);
    use crate::performance::diagnostics::UiPhase;
    app.background.activity.ui.record(
        UiPhase::Preparation,
        Duration::from_millis(63),
        Instant::now(),
    );
    app.background.activity.ui.record(
        UiPhase::InputToFrame,
        Duration::from_millis(74),
        Instant::now(),
    );
    for _ in 0..120 {
        app.background.activity.totals.record(
            "Highlighting",
            Duration::from_millis(7),
            Some(Duration::from_millis(2)),
            false,
        );
    }
    for (name, width, height, pinned, section) in [
        ("Monitor overview", 120, 38, false, Section::Overview),
        ("Monitor running", 120, 38, false, Section::Running),
        ("Monitor recent", 120, 38, false, Section::Recent),
        ("Monitor workspace", 120, 38, false, Section::Workspace),
        ("Monitor timings", 120, 38, false, Section::Timings),
        ("Monitor timings dock", 160, 42, true, Section::Timings),
        ("Monitor pinned", 160, 42, true, Section::Overview),
        ("Monitor compact", 80, 24, true, Section::Overview),
    ] {
        app.performance_view.pinned = pinned;
        app.performance_view.switch(section);
        let terminal = paint(&mut app, width, height);
        crate::ui_control_visibility_tests::snapshot(terminal.backend().buffer(), name);
    }
}

#[test]
fn timings_view_scrolls_resets_and_reports_without_sampling_or_losing_task_history() {
    use crate::performance::diagnostics::UiPhase;
    let mut app = fixture();
    let ticket = app.background.activity.start("Working", None).running();
    let finished = app.background.activity.start("Finished task", None);
    app.background
        .activity
        .active
        .last()
        .unwrap()
        .finish_for_test(Duration::from_secs(1));
    app.background.refresh_activity(Instant::now());
    for _ in 0..50 {
        app.background.activity.totals.record(
            "Fast task",
            Duration::from_millis(2),
            Some(Duration::from_millis(1)),
            false,
        );
    }
    app.background.activity.ui.record(
        UiPhase::Preparation,
        Duration::from_millis(75),
        Instant::now(),
    );
    toggle(&mut app);
    app.performance_view.switch(Section::Timings);
    let samples = app.performance_view.resources.samples;
    let terminal = paint(&mut app, 120, 38);
    assert!(text(&terminal).contains("Input to draw"));
    assert!(text(&terminal).contains("Frame preparation"));
    assert!(text(&terminal).contains("UI painting"));
    assert!(text(&terminal).contains("Terminal diff/output"));
    assert!(!timings_need_redraw(&app));
    key(&mut app, KeyCode::End, KeyModifiers::NONE);
    let terminal = paint(&mut app, 120, 38);
    assert!(app.performance_view.scroll > 0);
    assert!(text(&terminal).contains("Fast task"));
    let report = report(&app, Instant::now());
    for expected in [
        "Input-to-draw:",
        "75.000ms",
        "50 calls",
        "2.000ms mean",
        "resident_bytes=",
        "Pool snapshot:",
    ] {
        assert!(report.contains(expected), "missing {expected}");
    }
    assert!(!report.contains("first line"));
    click(&mut app, Hit::ResetTimings);
    assert_eq!(app.background.activity.totals.completed, 0);
    assert_eq!(app.background.activity.ui.slow_count, 0);
    assert_eq!(app.background.activity.recent.len(), 1);
    assert_eq!(app.background.activity.active.len(), 1);
    assert_eq!(app.performance_view.resources.samples, samples);
    paint(&mut app, 80, 24);
    assert!(!timings_need_redraw(&app));
    drop((ticket, finished));
}

#[test]
#[ignore = "manual monitor overhead benchmark"]
fn benchmark_pinned_monitor_render_and_sampling() {
    let mut app = fixture();
    toggle(&mut app);
    toggle_pin(&mut app);
    let mut terminal = paint(&mut app, 160, 42);
    use crate::performance::diagnostics::UiPhase;
    for phase in UiPhase::ALL {
        for millis in 0..120 {
            app.background
                .activity
                .ui
                .record(phase, Duration::from_millis(millis), Instant::now());
        }
    }
    for label in [
        "Highlighting",
        "Searching",
        "Comparing files",
        "Analyzing",
        "Opening",
    ] {
        for _ in 0..100 {
            app.background.activity.totals.record(
                label,
                Duration::from_millis(20),
                Some(Duration::from_millis(2)),
                false,
            );
        }
    }
    for section in [Section::Overview, Section::Timings] {
        app.performance_view.switch(section);
        let samples = app.performance_view.resources.samples;
        let start = Instant::now();
        for _ in 0..1000 {
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        }
        eprintln!(
            "Pinned editor + {section:?} monitor: {:.3} ms/frame",
            start.elapsed().as_secs_f64()
        );
        assert_eq!(app.performance_view.resources.samples, samples);
    }
}
