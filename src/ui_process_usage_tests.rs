use super::*;
use crate::process_usage::{Counters, SAMPLE_INTERVAL};
use ratatui::{Terminal, backend::TestBackend};

fn measured_app() -> App {
    let mut app = App::test_new();
    app.terminal_has_focus = true;
    app.background.activity.details_open = true;
    app.background
        .activity
        .render
        .record(Duration::from_micros(2400));
    let now = Instant::now();
    app.performance_view
        .resources
        .refresh_with(true, now - SAMPLE_INTERVAL, || Counters {
            cpu_time: Some(Duration::from_secs(100)),
            resident_bytes: Some(64 * 1024 * 1024),
        });
    app.performance_view
        .resources
        .refresh_with(true, now, || Counters {
            cpu_time: Some(Duration::from_secs(102)),
            resident_bytes: Some(96 * 1024 * 1024),
        });
    app
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
fn resource_overview_and_report_explain_units_without_resampling_on_redraw_or_copy() {
    let mut app = measured_app();
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    for expected in [
        "CPU",
        "20.0%",
        "Memory",
        "96.0 MiB",
        "updates every 10s",
        "100% = 1 core",
        "Recent work",
        "2.4ms",
    ] {
        assert!(text(&terminal).contains(expected), "missing {expected}");
    }
    for _ in 0..100 {
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let report = report(&app, Instant::now());
        assert!(report.contains("CPU 20.0%, resident memory 96.0 MiB"));
        assert!(report.contains("averaged over 10.0s"));
        assert!(report.contains("excludes child processes"));
    }
    copy_report_with(&mut app, |report| report.contains("96.0 MiB"));
    assert_eq!(app.performance_view.resources.samples, 2);
}

#[test]
fn sampling_deadline_exists_only_for_a_visible_focused_overview() {
    let mut app = measured_app();
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
    let now = Instant::now();
    assert_eq!(resource_next_wait(&app, now), None, "not painted yet");
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    let wait = resource_next_wait(&app, now).unwrap();
    assert!(wait > Duration::from_secs(9) && wait <= SAMPLE_INTERVAL);
    app.terminal_has_focus = false;
    assert_eq!(resource_next_wait(&app, now + SAMPLE_INTERVAL), None);
    assert!(!poll_resources(&mut app, now + SAMPLE_INTERVAL));
    app.terminal_has_focus = true;
    for section in [Section::Running, Section::Recent] {
        app.performance_view.switch(section);
        assert_eq!(resource_next_wait(&app, now + SAMPLE_INTERVAL), None);
        assert!(!poll_resources(&mut app, now + SAMPLE_INTERVAL));
    }
    app.performance_view.switch(Section::Overview);
    app.context_menu_active = true;
    assert_eq!(resource_next_wait(&app, now + SAMPLE_INTERVAL), None);
    assert!(!poll_resources(&mut app, now + SAMPLE_INTERVAL));
    app.context_menu_active = false;
    assert!(resource_next_wait(&app, now).is_some());
    dismiss(&mut app);
    assert_eq!(resource_next_wait(&app, now + SAMPLE_INTERVAL), None);
    assert!(!poll_resources(&mut app, now + SAMPLE_INTERVAL));
    assert_eq!(app.performance_view.resources.samples, 2);
}

#[test]
fn opening_overview_samples_once_and_small_or_hidden_views_stay_idle() {
    let mut app = App::test_new();
    app.terminal_has_focus = true;
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    assert_eq!(app.performance_view.resources.samples, 0);
    toggle(&mut app);
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    assert_eq!(app.performance_view.resources.samples, 1);
    assert!(text(&terminal).contains("Measuring…"));
    for _ in 0..5 {
        toggle(&mut app);
        toggle(&mut app);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    }
    assert_eq!(app.performance_view.resources.samples, 1);
    let mut tiny = Terminal::new(TestBackend::new(3, 2)).unwrap();
    tiny.draw(|frame| draw(frame, &mut app)).unwrap();
    assert_eq!(resource_next_wait(&app, Instant::now()), None);
    assert!(!poll_resources(&mut app, Instant::now() + SAMPLE_INTERVAL));
    assert_eq!(app.performance_view.resources.samples, 1);
}

#[test]
fn resource_rows_fit_compact_cards_and_resource_hover_does_not_reset_them() {
    let mut app = measured_app();
    for (width, height) in [(8, 4), (20, 8), (32, 12), (60, 18), (100, 28)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let area = app.performance_area.unwrap();
        assert_eq!(area.intersection(Rect::new(0, 0, width, height)), area);
        if width >= 20 {
            assert!(text(&terminal).contains("CPU"));
            assert!(text(&terminal).contains("2.4ms"));
        }
        for (control, _) in &app.performance_view.layout.controls {
            assert_eq!(*control, control.intersection(area));
        }
        if width >= 60 {
            let &(rect, _) = app
                .performance_view
                .layout
                .controls
                .iter()
                .find(|(_, hit)| *hit == Hit::ResourceInfo)
                .unwrap();
            handle_mouse(
                &mut app,
                MouseEvent {
                    kind: MouseEventKind::Moved,
                    column: rect.x,
                    row: rect.y,
                    modifiers: KeyModifiers::NONE,
                },
                (0, 0),
            );
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            assert!(text(&terminal).contains("excludes LSPs and terminals"));
        }
    }
    assert_eq!(app.performance_view.resources.samples, 2);
}
