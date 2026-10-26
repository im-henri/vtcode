use super::*;
use crate::{animation_config::AnimationEffect, app::Tab, editor::Editor};
use ratatui::{Terminal, backend::TestBackend};
use std::time::{Duration, Instant};

fn fixture(wrapped: bool, large: bool) -> App {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text(&format!(
        "fn outer() {{\n{}\n}}\n",
        "    process(\"界 café #f00\", value);\n".repeat(80)
    ));
    editor.file_path = Some("animation-frame.rs".into());
    editor.word_wrap = wrapped;
    editor.large_file_mode = large;
    editor.cursor_line = 4;
    editor.cursor_col = 8;
    app.tabs = vec![Tab::new_file(editor)];
    app.active_pane = crate::app::ActivePane::Editor;
    app.show_explorer = false;
    app.cursor_blinking = false;
    app.animation_settings
        .set(AnimationEffect::ScopeConnector, true);
    app
}

fn render(app: &mut App, terminal: &mut Terminal<TestBackend>, cosmetic: bool) -> Buffer {
    app.rendering_animation_frame = cosmetic;
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    app.rendering_animation_frame = false;
    terminal.backend().buffer().clone()
}

fn settle(app: &mut App, terminal: &mut Terminal<TestBackend>) {
    render(app, terminal, false);
    app.animations.advance_frame(
        &app.animation_settings,
        Instant::now() + Duration::from_secs(6),
    );
    render(app, terminal, false);
}

#[test]
fn input_frames_reuse_source_projection_without_reusing_selection_or_theme() {
    use crate::ui_editor_animations::REFERENCE_RETAINED_GEOMETRY;
    let mut app = fixture(false, false);
    app.tabs[0].editor = Editor::test_with_text(&format!(
        "fn outer() {{\n{}\n}}\n", "    process(value);\n".repeat(80)));
    app.tabs[0].editor.file_path = Some("stable-projection.rs".into());
    app.tabs[0].editor.cursor_line = 5;
    app.tabs[0].editor.cursor_col = 8;
    for effect in AnimationEffect::ALL { app.animation_settings.set(effect, effect == AnimationEffect::ScopeConnector); }
    app.show_minimap = false;
    app.sticky_scroll = false;
    let mut terminal = Terminal::new(TestBackend::new(130, 32)).unwrap();
    settle(&mut app, &mut terminal);
    let before = app.animation_frame_cache.projection_reuses;
    for step in 0..12 {
        let editor = app.editor_mut().unwrap();
        editor.cursor_col = 7 + step % 6;
        editor.selection_anchor = (step % 3 != 0).then_some((3, 8));
        let actual = render(&mut app, &mut terminal, false);
        assert!(app.animation_frame_cache.projection_reuses > before);
        REFERENCE_RETAINED_GEOMETRY.set(true);
        let expected = render(&mut app, &mut terminal, false);
        REFERENCE_RETAINED_GEOMETRY.set(false);
        assert_eq!(actual, expected, "cursor/selection step={step}");
    }
    for change in 0..9 {
        match change {
            0 => app.editor_mut().unwrap().insert_char('x'),
            1 => app.editor_mut().unwrap().undo(),
            2 => app.editor_mut().unwrap().scroll_offset = 10,
            3 => app.editor_mut().unwrap().horizontal_scroll = 7,
            4 => { app.editor_mut().unwrap().word_wrap = true; }
            5 => { terminal = Terminal::new(TestBackend::new(56, 24)).unwrap(); }
            6 => { app.colors = crate::app_theme::EditorTheme::Light.colors(); }
            7 => { app.editor_mut().unwrap().rope = ropey::Rope::from_str("fn other() {\n\tvalue(界);\n}\n"); }
            _ => { app.status_details.open = true; }
        }
        settle(&mut app, &mut terminal);
        let actual = render(&mut app, &mut terminal, false);
        REFERENCE_RETAINED_GEOMETRY.set(true);
        let expected = render(&mut app, &mut terminal, false);
        REFERENCE_RETAINED_GEOMETRY.set(false);
        assert_eq!(actual, expected, "change={change}");
    }
}

#[test]
fn cosmetic_frames_reuse_exact_static_cells_across_editor_paths_and_gradients() {
    for (width, wrapped, large) in [(140, false, false), (48, true, false), (100, false, true)] {
        let mut app = fixture(wrapped, large);
        let mut terminal = Terminal::new(TestBackend::new(width, 28)).unwrap();
        settle(&mut app, &mut terminal);
        let expected = render(&mut app, &mut terminal, false);
        let captures = app.animation_frame_cache.captures;
        let code = app.code_area;
        let gutter = app.gutter_area;
        for _ in 0..12 {
            assert_eq!(render(&mut app, &mut terminal, true), expected);
            assert_eq!(app.code_area, code);
            assert_eq!(app.gutter_area, gutter);
        }
        assert_eq!(app.animation_frame_cache.captures, captures);
        assert_eq!(app.animation_frame_cache.replays, 12);
        assert_eq!(
            app.animations
                .next_frame_wait(&app.animation_settings, Instant::now()),
            None
        );
    }
}

#[test]
fn scene_keys_reject_changed_source_carets_scroll_and_viewport() {
    let mut app = fixture(false, false);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    settle(&mut app, &mut terminal);
    for change in 0..4 {
        let before = app.animation_frame_cache.captures;
        let replays = app.animation_frame_cache.replays;
        match change {
            0 => app.tabs[0].editor.cursor_col += 1,
            1 => app.tabs[0].editor.insert_char('x'),
            2 => app.tabs[0].editor.scroll_offset += 1,
            _ => terminal = Terminal::new(TestBackend::new(90, 20)).unwrap(),
        }
        render(&mut app, &mut terminal, true);
        assert!(
            app.animation_frame_cache.captures > before,
            "change {change}"
        );
        assert_eq!(app.animation_frame_cache.replays, replays);
    }
}

#[test]
fn scene_rejects_source_replacement_even_when_revision_numbers_match() {
    let mut app = fixture(false, false);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    settle(&mut app, &mut terminal);
    let captures = app.animation_frame_cache.captures;
    let replays = app.animation_frame_cache.replays;
    let old = app.tabs[0].editor.rope.to_string();
    app.tabs[0].editor.rope = ropey::Rope::from_str(&old.replace("process", "changed"));
    render(&mut app, &mut terminal, true);
    assert!(app.animation_frame_cache.captures > captures);
    assert_eq!(app.animation_frame_cache.replays, replays);
}

#[test]
fn tab_slide_alone_reuses_editor_and_disabled_effects_release_scenes() {
    let mut app = fixture(false, false);
    app.animation_settings
        .set(AnimationEffect::ScopeConnector, false);
    app.animation_settings.set(AnimationEffect::TabSlide, true);
    let mut terminal = Terminal::new(TestBackend::new(100, 22)).unwrap();
    let expected = render(&mut app, &mut terminal, false);
    assert_eq!(render(&mut app, &mut terminal, true), expected);
    assert_eq!(app.animation_frame_cache.replays, 1);
    app.animation_settings.set(AnimationEffect::TabSlide, false);
    render(&mut app, &mut terminal, false);
    assert!(app.animation_frame_cache.scenes.is_empty());
}

#[test]
fn cached_scenes_preserve_split_panes_and_outer_modal_occlusion() {
    let mut app = fixture(false, false);
    app.tabs.push(Tab::new_file(Editor::test_with_text(
        "second pane\n    value\n",
    )));
    app.create_split_group_dir(1, crate::app::SplitDirection::Horizontal);
    let mut terminal = Terminal::new(TestBackend::new(150, 30)).unwrap();
    settle(&mut app, &mut terminal);
    let expected = render(&mut app, &mut terminal, false);
    let captures = app.animation_frame_cache.captures;
    assert_eq!(render(&mut app, &mut terminal, true), expected);
    assert_eq!(app.animation_frame_cache.captures, captures);
    assert_eq!(app.animation_frame_cache.replays, 2);
    app.exit_confirm_active = true;
    let expected = render(&mut app, &mut terminal, false);
    assert_eq!(render(&mut app, &mut terminal, true), expected);
}

#[test]
fn cosmetic_diff_frames_reuse_both_revisions_and_keep_revision_specific_carets() {
    use crate::app::{DiffMode, TabKind};
    for aligned in [false, true] {
        for compact in [false, true] {
            let base = format!(
                "fn outer() {{\n{}    old({});\n{}\n}}\n",
                "    unchanged(value);\n".repeat(12),
                "x".repeat(240),
                "    unchanged(value);\n".repeat(70),
            );
            let target = base.replace("old(", "new(");
            let mut app = fixture(false, false);
            app.tabs[0].editor = Editor::test_with_text(&target);
            app.tabs[0].editor.file_path = Some("cached-diff.rs".into());
            app.tabs[0].editor.word_wrap = false;
            app.tabs[0].editor.cursor_line = 13;
            app.tabs[0].editor.cursor_col = 8;
            app.tabs[0].kind = TabKind::Diff(97);
            app.diff_tabs.owner = Some(97);
            app.diff_mode = if aligned {
                DiffMode::SideBySide
            } else {
                DiffMode::Inline
            };
            app.diff_target_path = app.editor().unwrap().file_path.clone();
            app.compare_base_content = Some(base.clone().into());
            app.set_compare_diff_lines(crate::app::compute_file_diff(&base, &target));
            if compact {
                app.diff_render_cache = std::sync::Arc::new(app.diff_render_cache.with_context(2));
            }
            let mut terminal = Terminal::new(TestBackend::new(240, 30)).unwrap();
            settle(&mut app, &mut terminal);
            let expected = render(&mut app, &mut terminal, false);
            let captures = app.animation_frame_cache.captures;
            assert!(app.minimap_area.is_some() || compact);
            assert!(app.h_scrollbar_area.is_some());
            assert_eq!(
                render(&mut app, &mut terminal, true),
                expected,
                "aligned={aligned}, compact={compact}"
            );
            assert_eq!(app.animation_frame_cache.captures, captures);
            assert!(app.animation_frame_cache.replays > 0);
            assert_eq!(
                app.animations
                    .next_frame_wait(&app.animation_settings, Instant::now()),
                None
            );
        }
    }
}

#[test]
#[ignore = "manual release benchmark of whole-workbench cosmetic redraws"]
fn benchmark_animation_only_workbench_frames() {
    use std::hint::black_box;
    for (case, wrapped, large) in [
        ("rich", false, false),
        ("wrapped", true, false),
        ("bounded", false, true),
    ] {
        let mut app = fixture(wrapped, large);
        let mut terminal = Terminal::new(TestBackend::new(180, 60)).unwrap();
        settle(&mut app, &mut terminal);
        let mut full = Vec::new();
        let mut cosmetic = Vec::new();
        for batch in 0..7 {
            for cached in [batch % 2 == 0, batch % 2 != 0] {
                let start = Instant::now();
                app.rendering_animation_frame = cached;
                for _ in 0..50 {
                    terminal
                        .draw(|frame| crate::ui::draw(frame, &mut app))
                        .unwrap();
                    black_box(terminal.backend().buffer());
                }
                app.rendering_animation_frame = false;
                if cached {
                    cosmetic.push(start.elapsed() / 50)
                } else {
                    full.push(start.elapsed() / 50)
                }
            }
        }
        full.sort_unstable();
        cosmetic.sort_unstable();
        eprintln!(
            "animation workbench {case}: full {:?}, cached {:?}, {:.2}x",
            full[3],
            cosmetic[3],
            full[3].as_secs_f64() / cosmetic[3].as_secs_f64()
        );
    }
}

#[test]
#[ignore = "whole-workbench comparison with and without retained selection masks"]
fn benchmark_animation_selection_masks_workbench() {
    use std::hint::black_box;
    for selection in [false, true] {
        let mut app = fixture(false, false);
        let editor = &mut app.tabs[0].editor;
        editor.rope = ropey::Rope::from_str(&format!(
            "fn outer() {{\n{}\n}}\n",
            "    process(value, another_value, output);\n".repeat(80)
        ));
        if selection {
            editor.selection_anchor = Some((2, 0));
            editor.cursor_line = 50;
            editor.cursor_col = 35;
        }
        let mut terminal = Terminal::new(TestBackend::new(180, 60)).unwrap();
        settle(&mut app, &mut terminal);
        let expected = render(&mut app, &mut terminal, true);
        assert!(app.animation_frame_cache.scenes.iter().any(|scene| {
            if let Paint::Editor { geometry, .. } = &scene.paint {
                geometry.clear_protection_cache()
            } else {
                false
            }
        }), "fixture must exercise retained protection masks");
        let mut samples = [Vec::new(), Vec::new()];
        app.rendering_animation_frame = true;
        for round in 0..7 {
            for retained in [round % 2 == 0, round % 2 != 0] {
                let mut draw = || {
                    if !retained {
                        for scene in &app.animation_frame_cache.scenes {
                            if let Paint::Editor { geometry, .. } = &scene.paint {
                                geometry.clear_protection_cache();
                            }
                        }
                    }
                    terminal
                        .draw(|frame| crate::ui::draw(frame, &mut app))
                        .unwrap();
                    black_box(terminal.backend().buffer());
                };
                for _ in 0..10 {
                    draw();
                }
                let start = Instant::now();
                for _ in 0..100 {
                    draw();
                }
                samples[usize::from(retained)].push(start.elapsed() / 100);
                assert_eq!(terminal.backend().buffer(), &expected);
            }
        }
        for samples in &mut samples {
            samples.sort_unstable();
        }
        eprintln!(
            "animation workbench selection={selection}, rebuilding masks {:?}, retained masks {:?}",
            samples[0][3], samples[1][3]
        );
    }
}

#[test]
fn recycled_geometry_cannot_reuse_old_scroll_selection_theme_or_source() {
    let mut app = fixture(false, false);
    let mut terminal = Terminal::new(TestBackend::new(100, 35)).unwrap();
    settle(&mut app, &mut terminal);
    for step in 0..10 {
        let editor = &mut app.tabs[0].editor;
        editor.scroll_offset = step * 2;
        editor.cursor_line = step * 2 + 2;
        editor.cursor_col = step;
        editor.selection_anchor = (step % 2 == 0).then_some((step * 2, 0));
        editor.word_wrap = step % 3 == 0;
        editor.insert_char('x');
        app.colors = if step % 2 == 0 { crate::app_theme::EditorTheme::Light.colors() }
            else { crate::app_theme::EditorTheme::Dark.colors() };
        settle(&mut app, &mut terminal);
        REFERENCE_FRESH_GEOMETRY.set(false);
        let actual = render(&mut app, &mut terminal, false);
        REFERENCE_FRESH_GEOMETRY.set(true);
        let expected = render(&mut app, &mut terminal, false);
        REFERENCE_FRESH_GEOMETRY.set(false);
        assert_eq!(actual, expected, "step={step}");
        assert_eq!(render(&mut app, &mut terminal, true), expected);
    }
}

#[test]
#[ignore = "manual optimized full workbench geometry allocation comparison"]
fn benchmark_input_frame_geometry_reuse() {
    let mut times = [Vec::new(), Vec::new()];
    let mut app = fixture(false, false);
    let mut terminal = Terminal::new(TestBackend::new(180, 60)).unwrap();
    settle(&mut app, &mut terminal);
    for sample in 0..7 {
        for reuse in [sample % 2 == 0, sample % 2 != 0] {
            REFERENCE_FRESH_GEOMETRY.set(!reuse);
            let start = Instant::now();
            for _ in 0..100 {
                terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
                std::hint::black_box(terminal.backend().buffer());
            }
            times[usize::from(reuse)].push(start.elapsed() / 100);
        }
    }
    REFERENCE_FRESH_GEOMETRY.set(false);
    for samples in &mut times { samples.sort_unstable(); }
    eprintln!("180x60 workbench geometry: fresh={:?}, reused={:?}", times[0][3], times[1][3]);
}
