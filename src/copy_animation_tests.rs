use super::*;
use crate::{
    app::{ActivePane, Tab},
    editor::Editor,
};
use ratatui::{
    Terminal,
    backend::TestBackend,
    buffer::Buffer,
    style::{Color, Style},
};

fn fixture(text: &str) -> App {
    let mut app = App::test_new();
    app.tabs.push(Tab::new_file(Editor::test_with_text(text)));
    app.active_pane = ActivePane::Editor;
    app.show_explorer = false;
    app.show_status_bar = false;
    app.show_tab_bar = false;
    app.show_breadcrumbs = false;
    app.show_minimap = false;
    app.sticky_scroll = false;
    app.cursor_blinking = false;
    app.cursor_visible = false;
    app.copy_animation.duration_ms = 1_000;
    app
}

fn render(app: &mut App) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    terminal.backend().buffer().clone()
}

fn select(app: &mut App, start: (usize, usize), end: (usize, usize)) {
    let editor = app.editor_mut().unwrap();
    editor.selection_anchor = Some(start);
    (editor.cursor_line, editor.cursor_col) = end;
}

#[test]
fn copy_animation_defaults_and_fade_are_finite_and_constant_sized() {
    let mut app = fixture("alpha beta\n");
    select(&mut app, (0, 0), (0, 5));
    assert!(CopyAnimation::default().enabled);
    assert_eq!(CopyAnimation::default().duration_ms, 220);
    assert!(
        !app.animation_settings.any_enabled(),
        "copy feedback needs no decorative scene geometry"
    );
    assert!(std::mem::size_of::<CopyAnimation>() < 256);
    let now = Instant::now();
    app.copy_animation.duration_ms = 220;
    app.start_copy_animation(now);
    assert_eq!(app.copy_animation.fade(now), Some(0.0));
    assert!(
        (app.copy_animation
            .fade(now + Duration::from_millis(77))
            .unwrap()
            - 1.0)
            .abs()
            < 0.0001
    );
    assert!(
        app.copy_animation
            .fade(now + Duration::from_millis(219))
            .unwrap()
            < 0.001
    );
    assert_eq!(
        app.copy_animation.fade(now + Duration::from_millis(220)),
        None
    );
    assert_eq!(
        app.copy_animation_next_wait(now),
        None,
        "no timer until selected cells are visible"
    );
    app.copy_animation.visible = true;
    assert!(app.copy_animation_next_wait(now).unwrap() > Duration::ZERO);
    assert!(app.advance_copy_animation(now + Duration::from_millis(221)));
    assert_eq!(
        app.copy_animation_next_wait(now + Duration::from_secs(2)),
        None
    );
    assert!(
        !app.advance_copy_animation(now + Duration::from_secs(2)),
        "one final clean frame"
    );
}

#[test]
fn copy_animation_only_changes_visible_selection_colors_and_restores_them() {
    for (wrapped, large, multiple) in [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        let mut app = fixture("\talpha 界 cafe\u{301} beta\nsecond selected line\nunchanged\n");
        app.editor_mut().unwrap().word_wrap = wrapped;
        app.editor_mut().unwrap().large_file_mode = large;
        select(&mut app, (0, 2), (1, 8));
        if multiple {
            let editor = app.editor_mut().unwrap();
            editor.extra_cursors.push((2, 5));
            editor.extra_cursor_anchors.push(Some((2, 1)));
        }
        let before = render(&mut app);
        let source = app.editor().unwrap().rope.clone();
        let version = app.editor().unwrap().content_version;
        let selection = app.editor().unwrap().selection_range();
        app.start_copy_animation(Instant::now() - Duration::from_millis(350));
        app.rendering_animation_frame = true;
        let during = render(&mut app);
        assert!(
            app.copy_animation.visible,
            "wrap={wrapped}, large={large}, multi={multiple}"
        );
        let mut changed = 0;
        for (a, b) in before.content.iter().zip(&during.content) {
            assert_eq!(a.symbol(), b.symbol(), "feedback never changes text");
            if a.bg != b.bg {
                changed += 1;
                assert_ne!(
                    a.bg, app.colors.bg_editor,
                    "unselected source stays untouched"
                );
            }
        }
        assert!(changed > 0 && changed < 80 * 24);
        assert!(source.is_instance(&app.editor().unwrap().rope));
        assert_eq!(app.editor().unwrap().content_version, version);
        assert_eq!(app.editor().unwrap().selection_range(), selection);
        app.copy_animation.clear();
        let after = render(&mut app);
        assert_eq!(before, after);
    }
}

#[test]
fn copy_animation_million_line_selection_never_enumerates_hidden_lines() {
    let mut app = fixture(&"selected code\n".repeat(1_000_000));
    let editor = app.editor_mut().unwrap();
    editor.large_file_mode = true;
    editor.scroll_offset = 500_000;
    select(&mut app, (0, 0), (999_999, 12));
    render(&mut app);
    app.start_copy_animation(Instant::now() - Duration::from_millis(350));
    let key = app.copy_animation.pulse.as_ref().unwrap().key;
    let before = app.editor().unwrap().rope.clone();
    render(&mut app);
    assert!(app.copy_animation.visible);
    assert_eq!(
        app.copy_animation.pulse.as_ref().unwrap().key.view,
        key.view
    );
    assert!(app.editor().unwrap().rope.is_instance(&before));
    // Only the actual viewport's selected cells can request another frame.
    select(&mut app, (900_000, 0), (999_999, 12));
    app.start_copy_animation(Instant::now());
    render(&mut app);
    assert!(!app.copy_animation.visible);
    assert_eq!(app.copy_animation_next_wait(Instant::now()), None);
}

#[test]
fn copy_animation_restarts_cancels_and_does_not_follow_other_documents() {
    let mut app = fixture("alpha beta\n");
    select(&mut app, (0, 0), (0, 5));
    let now = Instant::now();
    app.start_copy_animation(now);
    app.start_copy_animation(now + Duration::from_millis(50));
    assert_eq!(
        app.copy_animation.pulse.as_ref().unwrap().started,
        now + Duration::from_millis(50)
    );
    app.editor_mut().unwrap().cursor_col = 6;
    app.begin_copy_animation_frame(now);
    assert!(!app.copy_animation.active());
    app.start_copy_animation(now);
    app.editor_mut().unwrap().content_version += 1;
    app.begin_copy_animation_frame(now);
    assert!(!app.copy_animation.active());
    app.start_copy_animation(now);
    app.tabs
        .push(Tab::new_file(Editor::test_with_text("other\n")));
    app.active_tab = 1;
    app.begin_copy_animation_frame(now);
    assert!(!app.copy_animation.active());
    app.active_tab = 0;
    app.copy_animation.enabled = false;
    app.start_copy_animation(now);
    assert!(!app.copy_animation.active());
    app.copy_animation.enabled = true;
    app.start_copy_animation(now);
    app.terminal_has_focus = false;
    app.begin_copy_animation_frame(now);
    assert!(!app.copy_animation.active());
}

#[test]
fn copy_animation_selection_style_preserves_high_contrast_text_at_mid_blink() {
    let colors = crate::app_theme_loader::builtin_theme("high-contrast")
        .unwrap()
        .to_theme_colors();
    let input = Style::default().fg(Color::White).bg(colors.bg_editor);
    let mut style = EditorSelectionStyle::new(&colors).with_copy_feedback(colors.bg_editor, 1.0);
    let result = style.apply(input);
    assert_eq!(result.bg, Some(colors.bg_editor));
    assert_eq!(result.fg, input.fg);
    assert!(style.copy_feedback_visible());
}

#[test]
fn copy_animation_theme_snapshots_restore_exact_selection_colors() {
    for theme in crate::app_theme::load_builtin_themes() {
        let mut app =
            fixture("fn main() {\n    let copied = \"hello\";\n    println!(\"{copied}\");\n}\n");
        app.colors = theme.to_theme_colors();
        select(&mut app, (0, 0), (2, 27));
        let before = render(&mut app);
        crate::ui_control_visibility_tests::snapshot(
            &before,
            &format!("copy {} before", theme.name),
        );
        for (name, elapsed) in [("fade out", 150), ("blink", 350), ("return", 650)] {
            app.start_copy_animation(Instant::now() - Duration::from_millis(elapsed));
            let during = render(&mut app);
            assert_ne!(before, during, "{} {name}", theme.name);
            crate::ui_control_visibility_tests::snapshot(
                &during,
                &format!("copy {} {name}", theme.name),
            );
        }
        app.copy_animation.clear();
        assert_eq!(before, render(&mut app));
    }
}

#[test]
fn copy_animation_uses_the_copy_action_and_frames_send_no_clipboard_traffic() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    let mut app = fixture("alpha beta\n");
    select(&mut app, (0, 0), (0, 5));
    let source = app.editor().unwrap().rope.clone();
    for keyboard in [true, false] {
        if keyboard {
            crate::input_keyboard::handle_keyboard_event(
                &mut app,
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            );
        } else {
            app.execute_palette_command("copy");
        }
        assert!(app.copy_animation.active());
        assert_eq!(app.editor().unwrap().clipboard, "alpha");
        let traffic = crate::clipboard::test_terminal_output();
        app.start_copy_animation(Instant::now() - Duration::from_millis(350));
        for _ in 0..3 {
            render(&mut app);
        }
        assert_eq!(
            crate::clipboard::test_terminal_output(),
            traffic,
            "cosmetic frames perform no remote/clipboard writes"
        );
        assert!(app.editor().unwrap().rope.is_instance(&source));
        assert!(!app.editor().unwrap().modified);
    }
    // A non-copy key owns the interaction immediately, including changes to
    // secondary cursors whose count and primary selection could stay unchanged.
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(!app.copy_animation.active());
    app.editor_mut().unwrap().selection_anchor = None;
    app.execute_palette_command("copy");
    assert!(
        !app.copy_animation.active(),
        "whole-line copy has no selected region to blink"
    );
}

#[test]
#[ignore = "manual viewport performance benchmark"]
fn benchmark_copy_animation_visible_work_is_independent_of_selection_size() {
    use std::hint::black_box;
    for lines in [1_000, 1_000_000] {
        let mut app = fixture(&"selected code\n".repeat(lines));
        app.editor_mut().unwrap().large_file_mode = true;
        select(&mut app, (0, 0), (lines - 1, 12));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut times = [Duration::ZERO; 31];
        for elapsed in &mut times {
            app.start_copy_animation(Instant::now() - Duration::from_millis(350));
            let started = Instant::now();
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            *elapsed = started.elapsed();
            black_box(terminal.backend().buffer());
        }
        times.sort_unstable();
        eprintln!(
            "copy animation: selected_lines={lines}, viewport=80x24, median={:?}",
            times[15]
        );
    }
}

#[test]
fn copy_animation_settings_are_scoped_editable_persisted_and_resettable() {
    let mut app = App::test_new();
    let directory = tempfile::tempdir().unwrap();
    app.settings_global_path = directory.path().join("user/settings.json");
    app.settings_workspace_path = directory.path().join("project/.vtcode/settings.json");
    app.settings_persistence_enabled = true;
    assert!(app.copy_animation.enabled);
    assert_eq!(App::setting_field_kind("animation_copy"), "checkbox");
    assert_eq!(
        App::setting_field_kind("animation_copy_duration_ms"),
        "number"
    );
    assert_eq!(
        App::animation_setting_section("animation_copy"),
        Some("Editing")
    );
    app.settings_scope = crate::settings::SettingsScope::Global;
    app.toggle_setting("animation_copy");
    assert!(!app.copy_animation.enabled);
    assert_eq!(app.settings_global.animation_copy, Some(false));
    assert_eq!(
        crate::settings::load_from_file(&app.settings_global_path).animation_copy,
        Some(false)
    );
    app.begin_numeric_setting_edit("animation_copy_duration_ms", "220");
    app.settings_value_buffer = "415".into();
    assert!(app.commit_numeric_setting_edit());
    assert_eq!(app.copy_animation.duration_ms, 415);
    app.settings_scope = crate::settings::SettingsScope::Workspace;
    app.toggle_setting("animation_copy");
    assert!(app.copy_animation.enabled);
    assert_eq!(app.settings_global.animation_copy, Some(false));
    assert_eq!(app.settings_workspace.animation_copy, Some(true));
    assert!(app.reset_setting_in_selected_scope("animation_copy"));
    assert!(!app.copy_animation.enabled);
    app.settings_scope = crate::settings::SettingsScope::Global;
    assert!(app.reset_setting_in_selected_scope("animation_copy"));
    assert!(app.reset_setting_in_selected_scope("animation_copy_duration_ms"));
    assert!(app.copy_animation.enabled);
    assert_eq!(app.copy_animation.duration_ms, DEFAULT_DURATION_MS);
    let mut values: crate::settings::Settings =
        serde_json::from_str(r#"{"animationCopy":false,"animationCopyDurationMs":999999}"#)
            .unwrap();
    values.validate();
    assert_eq!(values.animation_copy_duration_ms, Some(5_000));
    let mut zero = crate::settings::Settings {
        animation_copy_duration_ms: Some(0),
        ..Default::default()
    };
    zero.validate();
    assert_eq!(zero.animation_copy_duration_ms, Some(1));
}
