use super::*;
use ratatui::{layout::Rect, style::Style};

fn workbench(width: u16, height: u16) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    for y in 0..height {
        let style = Style::default()
            .fg(Color::Rgb(200, 210, 220))
            .bg(Color::Rgb(30, 32, 40 + y as u8));
        buffer.set_string(0, y, format!("{y:>3} fn main() {{ 界 }}").as_str(), style);
        for x in 20..width {
            buffer[(x, y)].set_style(style);
        }
    }
    buffer
}

/// Cells showing any glyph: backgrounds never count.
fn revealed(buffer: &Buffer) -> usize {
    buffer
        .content
        .iter()
        .filter(|cell| !cell.symbol().trim().is_empty())
        .count()
}

/// Every frame keeps every background exactly where the workbench put it.
fn assert_backgrounds_exact(out: &Buffer, target: &Buffer, progress: f32) {
    for (index, (cell, place)) in out.content.iter().zip(&target.content).enumerate() {
        assert_eq!(cell.bg, place.bg, "cell {index} at {progress}");
    }
}

#[test]
fn startup_shows_backgrounds_at_once_and_text_rises_from_the_bottom() {
    let target = workbench(60, 20);
    let mut out = target.clone();
    startup_frame(&target, &mut out, 0.0, 0);
    assert_eq!(revealed(&out), 0, "no text before the first front");
    assert_backgrounds_exact(&out, &target, 0.0);

    let mut previous = 0;
    let mut saw_partial = false;
    for step in 1..=100 {
        let progress = step as f32 / 100.0;
        let mut out = target.clone();
        startup_frame(&target, &mut out, progress, step);
        assert_backgrounds_exact(&out, &target, progress);
        let count = revealed(&out);
        assert!(count >= previous, "text never retreats ({progress})");
        previous = count;
        if count > 0 && count < revealed(&target) {
            saw_partial = true;
            // The bottom row leads the top row.
            let row = |y: u16| {
                (0..60)
                    .filter(|&x| !out[(x, y)].symbol().trim().is_empty())
                    .count()
            };
            assert!(row(19) >= row(0), "rises from the bottom ({progress})");
        }
        if progress >= 1.0 - SETTLE {
            assert_eq!(out, target, "every cell is exact once settled ({progress})");
        }
    }
    assert!(saw_partial);
}

#[test]
fn exit_keeps_backgrounds_and_text_drains_down() {
    let snapshot = workbench(60, 20);
    let mut out = snapshot.clone();
    exit_frame(&snapshot, &mut out, 0.0, 0);
    assert_eq!(out, snapshot, "the first exit frame is the workbench");

    let mut previous = revealed(&snapshot);
    for step in 1..=100 {
        let progress = step as f32 / 100.0;
        out = snapshot.clone();
        exit_frame(&snapshot, &mut out, progress, step);
        assert_backgrounds_exact(&out, &snapshot, progress);
        let count = revealed(&out);
        assert!(count <= previous, "text only drains ({progress})");
        previous = count;
        // Text moves down, never up: a column's text never sits above the
        // highest row it occupied in the snapshot.
        for x in 0..60 {
            let top =
                |buffer: &Buffer| (0..20).position(|y| !buffer[(x, y)].symbol().trim().is_empty());
            if let (Some(now), Some(before)) = (top(&out), top(&snapshot)) {
                assert!(now >= before, "column {x} at {progress}");
            }
        }
    }
    assert_eq!(previous, 0);
    assert!(out.content.iter().all(|cell| cell.symbol() == " "));
    assert_backgrounds_exact(&out, &snapshot, 1.0);
}

#[test]
fn wide_glyphs_never_split_across_differently_timed_columns() {
    let target = workbench(40, 12);
    let wide_at: Vec<_> = (0..40)
        .filter(|&x| target[(x, 3)].symbol() == "界")
        .collect();
    assert_eq!(wide_at.len(), 1);
    let x = wide_at[0];
    for step in 0..=60 {
        let progress = step as f32 / 60.0;
        let mut out = target.clone();
        startup_frame(&target, &mut out, progress, step);
        // The trailing cell stays blank, so a narrow stand-in for the wide
        // glyph never shows stray text beside it.
        assert_eq!(out[(x + 1, 3)].symbol(), " ", "{progress}");
        let mut out = target.clone();
        exit_frame(&target, &mut out, progress, step);
        for y in 0..12 {
            for x in 0..39 {
                if UnicodeWidthStr::width(out[(x, y)].symbol()) > 1 {
                    // A wide glyph is only drawn where both of its columns
                    // are in step, so the cell it covers holds no text.
                    assert!(out[(x + 1, y)].symbol().trim().is_empty());
                }
            }
        }
    }
}

#[test]
fn state_is_off_by_default_and_arms_only_when_enabled() {
    let mut state = State::default();
    assert_eq!(state.settings, Settings::default());
    assert!(!state.settings.startup && !state.settings.exit);
    assert_eq!(state.settings.startup_ms, 1_500);
    state.arm_startup();
    assert!(!state.startup_active());
    assert!(!state.begin_exit(Instant::now()));

    state.settings.startup = true;
    state.arm_startup();
    let now = Instant::now();
    assert!(state.startup_due(now), "the first frame starts the clock");
    let mut buffer = workbench(20, 6);
    let target = buffer.clone();
    state.paint_startup(&mut buffer, now);
    assert_ne!(buffer, target);
    assert!(!state.startup_due(now + Duration::from_millis(5)));
    assert!(state.startup_due(now + FRAME));
    let mut buffer = target.clone();
    state.paint_startup(&mut buffer, now + Duration::from_millis(1_500));
    assert_eq!(buffer, target);
    assert!(!state.startup_active(), "finished after its duration");

    state.arm_startup();
    assert!(state.skip_startup());
    assert!(!state.skip_startup());

    state.settings.exit = true;
    state.settings.exit_ms = 200;
    let now = Instant::now();
    assert!(state.begin_exit(now));
    assert!(state.exit_needs_snapshot());
    assert!(!state.exit_done(now));
    let mut buffer = target.clone();
    state.paint_exit(&mut buffer, now);
    assert_eq!(buffer, target);
    assert!(!state.exit_needs_snapshot());
    assert!(state.exit_done(now + Duration::from_millis(200)));
    state.skip_exit();
    assert!(state.exit_done(now));
}

#[test]
fn startup_and_exit_settings_toggle_separately_persist_and_reset() {
    use crate::app::App;
    let mut app = App::test_new();
    let directory = tempfile::tempdir().unwrap();
    app.settings_global_path = directory.path().join("user/settings.json");
    app.settings_workspace_path = directory.path().join("project/.vtcode/settings.json");
    app.settings_persistence_enabled = true;
    app.settings_scope = crate::settings::SettingsScope::Global;
    assert_eq!(app.screen_transition.settings, Settings::default());
    for id in ["animation_startup", "animation_exit"] {
        assert_eq!(App::setting_field_kind(id), "checkbox");
        assert_eq!(App::animation_setting_section(id), Some("Startup & Exit"));
    }

    app.toggle_setting("animation_startup");
    assert!(app.screen_transition.settings.startup);
    assert!(!app.screen_transition.settings.exit, "independent toggles");
    app.toggle_setting("animation_exit");
    assert!(app.screen_transition.settings.exit);
    app.begin_numeric_setting_edit("animation_exit_duration_ms", "1500");
    app.settings_value_buffer = "900".into();
    assert!(app.commit_numeric_setting_edit());
    assert_eq!(app.screen_transition.settings.exit_ms, 900);
    assert_eq!(app.screen_transition.settings.startup_ms, 1_500);
    let saved = crate::settings::load_from_file(&app.settings_global_path);
    assert_eq!(
        (
            saved.animation_startup,
            saved.animation_exit,
            saved.animation_exit_duration_ms
        ),
        (Some(true), Some(true), Some(900))
    );

    for id in [
        "animation_startup",
        "animation_exit",
        "animation_exit_duration_ms",
    ] {
        assert!(app.reset_setting_in_selected_scope(id), "{id}");
    }
    assert_eq!(app.screen_transition.settings, Settings::default());

    let mut values: crate::settings::Settings = serde_json::from_str(
        r#"{"animationStartup":true,"animationStartupDurationMs":0,"animationExitDurationMs":99999}"#,
    )
    .unwrap();
    values.validate();
    assert_eq!(values.animation_startup, Some(true));
    assert_eq!(values.animation_startup_duration_ms, Some(1));
    assert_eq!(values.animation_exit_duration_ms, Some(5_000));
}

fn run_with_keys(
    app: &mut crate::app::App,
    delay: Duration,
    keys: Vec<crossterm::event::KeyEvent>,
) -> Duration {
    use crossterm::event::Event;
    let (sender, receiver) = std::sync::mpsc::channel();
    let (done, finished) = std::sync::mpsc::channel::<()>();
    let wake = app.background.wake_handle();
    let input = std::thread::spawn(move || {
        std::thread::sleep(delay);
        for key in keys {
            sender.send(Ok(Event::Key(key))).unwrap();
        }
        wake.notify();
        // Keep the pump connected until the loop has returned.
        let _ = finished.recv_timeout(Duration::from_secs(20));
    });
    let pump = crate::app_event_loop::TerminalEventPump::from_receiver(receiver);
    let mut terminal = ratatui::Terminal::with_options(
        ratatui::backend::CrosstermBackend::new(Vec::<u8>::new()),
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(Rect::new(0, 0, 100, 30)),
        },
    )
    .unwrap();
    let started = Instant::now();
    crate::app_event_loop::run_app(&mut terminal, app, pump, None).unwrap();
    let elapsed = started.elapsed();
    done.send(()).unwrap();
    input.join().unwrap();
    elapsed
}

fn quit_keys() -> Vec<crossterm::event::KeyEvent> {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    vec![
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
    ]
}

#[test]
fn event_loop_plays_the_startup_animation_then_the_exit_animation_on_quit() {
    let mut app = crate::app::App::test_new();
    app.screen_transition.settings = Settings {
        startup: true,
        exit: true,
        startup_ms: 200,
        exit_ms: 300,
    };
    let elapsed = run_with_keys(&mut app, Duration::from_millis(350), quit_keys());
    assert!(!app.screen_transition.startup_active(), "startup finished");
    assert!(app.shutdown_anim_active);
    assert!(app.screen_transition.exit_done(Instant::now()));
    // Quit arrives at 350 ms; the exit animation then plays for 300 ms.
    assert!(elapsed >= Duration::from_millis(640), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
}

#[test]
fn input_skips_the_startup_animation_and_quit_is_immediate_when_exit_is_off() {
    let mut app = crate::app::App::test_new();
    app.screen_transition.settings = Settings {
        startup: true,
        exit: false,
        startup_ms: 5_000,
        exit_ms: 5_000,
    };
    let elapsed = run_with_keys(&mut app, Duration::from_millis(50), quit_keys());
    assert!(
        !app.screen_transition.startup_active(),
        "the key skipped it"
    );
    assert!(
        !app.shutdown_anim_active,
        "no exit animation when it is off"
    );
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
}

/// Visual check: `cargo test --bin vtcode dump_screen_transition_frames -- --ignored`
/// writes frames of both effects over a real workbench to `$VTCODE_FRAME_DUMP`.
#[test]
#[ignore]
fn dump_screen_transition_frames() {
    use std::fmt::Write;
    let path = std::env::var("VTCODE_FRAME_DUMP").expect("VTCODE_FRAME_DUMP");
    let mut app = crate::app::App::test_new();
    let text: String = (0..60)
        .map(|i| format!("fn line_{i}(value: usize) -> usize {{ value * {i} + 1 }} // 界 {i}\n"))
        .collect();
    app.tabs = vec![crate::app::Tab::new_file(
        crate::editor::Editor::test_with_text(&text),
    )];
    app.active_tab = 0;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 28)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let target = terminal.backend().buffer().clone();
    let css = |color: Color, fallback: &str| match color {
        Color::Rgb(r, g, b) => format!("rgb({r},{g},{b})"),
        _ => fallback.to_string(),
    };
    let mut html = String::from(
        "<html><meta charset='utf-8'><body style='background:#0c0c0c;color:#ccc;font:12px monospace'>",
    );
    for (name, progress) in [
        ("startup", [0.08, 0.2, 0.35, 0.5, 0.7, 0.95]),
        ("exit", [0.1, 0.3, 0.5, 0.65, 0.8, 0.95]),
    ] {
        for p in progress {
            let mut out = target.clone();
            if name == "startup" {
                startup_frame(&target, &mut out, p, (p * 25.0) as u64);
            } else {
                exit_frame(&target, &mut out, p, (p * 25.0) as u64);
            }
            write!(html, "<h3>{name} {p}</h3><pre style='line-height:1.15'>").unwrap();
            for y in 0..out.area.height {
                let mut skip = 0;
                for x in 0..out.area.width {
                    if skip > 0 {
                        skip -= 1;
                        continue;
                    }
                    let cell = &out[(x, y)];
                    let symbol = cell.symbol();
                    skip = UnicodeWidthStr::width(symbol).saturating_sub(1);
                    let escaped = match symbol {
                        "<" => "&lt;".into(),
                        ">" => "&gt;".into(),
                        "&" => "&amp;".into(),
                        "" => " ".into(),
                        other => other.to_string(),
                    };
                    write!(
                        html,
                        "<span style='color:{};background:{}'>{escaped}</span>",
                        css(cell.fg, "#ccc"),
                        css(cell.bg, "#0c0c0c")
                    )
                    .unwrap();
                }
                html.push('\n');
            }
            html.push_str("</pre>");
        }
    }
    html.push_str("</body></html>");
    std::fs::write(path, html).unwrap();
}

#[test]
fn startup_ticks_reuse_the_last_real_frame_until_the_final_frame() {
    let mut state = State::default();
    state.settings.startup = true;
    state.settings.startup_ms = 300;
    state.arm_startup();
    let start = Instant::now();
    assert!(state.startup_due(start));
    assert!(
        !state.cached_startup_ready(start),
        "needs one real frame first"
    );

    let target = workbench(50, 16);
    let mut first = target.clone();
    state.paint_startup(&mut first, start);
    let tick = start + FRAME;
    assert!(state.cached_startup_ready(tick));
    // A tick paints into whatever the terminal hands over, from the cache.
    let mut cached = Buffer::empty(target.area);
    state.paint_startup_cached(&mut cached, tick);
    let mut expected = target.clone();
    let elapsed = tick - start;
    startup_frame(
        &target,
        &mut expected,
        elapsed.as_secs_f32() / 0.3,
        (elapsed.as_millis() / 60) as u64,
    );
    assert_eq!(cached, expected);
    assert!(!state.startup_due(tick + Duration::from_millis(5)));

    // The final frame takes the real render path, which ends the effect.
    let end = start + Duration::from_millis(300);
    assert!(state.startup_due(end));
    assert!(!state.cached_startup_ready(end));
    let mut last = target.clone();
    state.paint_startup(&mut last, end);
    assert_eq!(last, target);
    assert!(!state.startup_active());
}
