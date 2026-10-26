//! Read-only comparisons use the same cursor effects as editable source views.
use crate::{
    animation_config::AnimationEffect,
    app::{ActivePane, App, DiffMode, Tab, TabKind},
    editor::Editor,
};
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect, style::Color};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

const CURSOR: Color = Color::Rgb(251, 17, 139);

fn view_name(mode: DiffMode) -> &'static str {
    match mode {
        DiffMode::SideBySide => "aligned",
        DiffMode::Inline => "inline",
        DiffMode::Off => unreachable!(),
    }
}

fn comparison(mode: DiffMode, compact: bool, commit: bool) -> (App, String, String) {
    let prefix = "unchanged prefix\n".repeat(12);
    let suffix = "unchanged suffix\n".repeat(12);
    let base = format!(
        "{prefix}old_value_abcdefghijklmnop\nremoved_more\nanchor_text_abcdefghijklmnop\n{suffix}"
    );
    let target = format!("{prefix}new_value\nanchor_text_abcdefghijklmnop\n{suffix}");
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text(&target);
    editor.file_path = Some("diff-animation.txt".into());
    editor.read_only = true;
    editor.word_wrap = false;
    editor.set_cursor(13, 0);
    let mut tab = Tab::new_file(editor);
    tab.kind = if commit {
        TabKind::CommitDetails
    } else {
        TabKind::Diff(97)
    };
    app.tabs = vec![tab];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.diff_tabs.owner = Some(if commit {
        crate::diff_tabs::COMMIT_DIFF_ID
    } else {
        97
    });
    app.diff_mode = mode;
    app.diff_target_path = app.editor().unwrap().file_path.clone();
    app.compare_base_content = Some(base.clone().into());
    app.set_compare_diff_lines(crate::app::compute_file_diff(&base, &target));
    if commit {
        app.preferred_diff_mode = mode;
        app.commit_details.diff_full_cache = Some(app.diff_render_cache.clone());
        app.commit_details.diff_context = crate::commit_details::DiffContext { changes_only: compact, lines: 2 };
    }
    if compact {
        app.diff_render_cache = Arc::new(app.diff_render_cache.with_context(2));
    }
    if commit {
        app.commit_details.active = true;
        app.commit_details.focus = crate::commit_details::Focus::Diff;
        app.commit_details.data.files = vec![crate::commit_details::CommitFile::new(
            'M',
            "diff-animation.txt",
        )];
        app.commit_details.diff_file = Some(0);
    }
    app.show_explorer = false;
    app.show_minimap = false;
    app.show_breadcrumbs = false;
    app.sticky_scroll = false;
    app.show_indent_guides = false;
    app.show_color_preview = false;
    app.rulers.clear();
    app.cursor_visible = true;
    app.cursor_blinking = false;
    app.colors.bg_cursor = CURSOR;
    app.animation_settings.duration_ms = 1500;
    (app, base, target)
}

fn render(app: &mut App) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(180, 40)).unwrap();
    let mut painted = None;
    terminal
        .draw(|frame| {
            crate::ui::draw(frame, app);
            painted = Some(frame.buffer_mut().clone());
        })
        .unwrap();
    painted.unwrap()
}

fn source_origin(app: &App, line: usize, original: bool) -> (u16, u16) {
    let aligned = app.diff_mode == DiffMode::SideBySide;
    let area = source_area(app, original);
    let cache = &app.diff_render_cache;
    let row = if aligned {
        cache.rows.iter().position(|row| {
            row.hidden_lines == 0
                && if original {
                    row.left_line == Some(line)
                } else {
                    row.right_line == Some(line)
                }
        })
    } else {
        cache.inline_rows.iter().position(|entry| {
            let row = &cache.rows[entry.aligned_row];
            row.hidden_lines == 0
                && if original {
                    entry.kind == '-' && row.left_line == Some(line)
                } else {
                    entry.kind != '-' && row.right_line == Some(line)
                }
        })
    }
    .expect("fixture source row is represented");
    let first = app.diff_inline_scroll.row;
    assert!(row >= first && row - first < area.height as usize);
    (area.x, area.y + (row - first) as u16)
}

fn source_area(app: &App, original: bool) -> Rect {
    if app.diff_mode == DiffMode::SideBySide && original {
        app.diff_left_code_area.unwrap()
    } else {
        let mut area = app.code_area.unwrap();
        if app.diff_mode == DiffMode::SideBySide {
            area.x += 2;
            area.width = area.width.saturating_sub(2);
        }
        area
    }
}

fn click(app: &mut App, point: (u16, u16)) {
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::mouse::handle_mouse(
            app,
            MouseEvent {
                kind,
                column: point.0,
                row: point.1,
                modifiers: KeyModifiers::NONE,
            },
        );
    }
}

fn navigate(app: &mut App, count: usize, modifiers: KeyModifiers) {
    for _ in 0..count {
        crate::input::handle_editor_key(app, KeyCode::Right, modifiers);
    }
}

fn assert_sources(app: &App, base: &str, target: &str, version: u64) {
    let editor = app.editor().unwrap();
    assert_eq!(editor.rope.to_string(), target);
    assert_eq!(app.compare_base_content.as_deref(), Some(base));
    assert_eq!(editor.content_version, version);
    assert!(editor.read_only && !editor.modified);
}

#[test]
fn multicursor_suspends_diff_animations_on_both_sides_and_resumes_cleanly() {
    for mode in [DiffMode::Inline, DiffMode::SideBySide] {
        for original in [false, true] {
            let (mut app, base, target) = comparison(mode, false, false);
            app.animation_settings.set(AnimationEffect::CursorSmear, true);
            app.animation_settings.set(AnimationEffect::CursorFire, true);
            let version = app.editor().unwrap().content_version;
            render(&mut app);
            if original {
                let point = source_origin(&app, 12, true);
                click(&mut app, point);
                render(&mut app);
            }
            navigate(&mut app, 6, KeyModifiers::NONE);
            render(&mut app);
            let id = crate::diff_scope_source::animation_view_id(&mut app).unwrap();
            assert!(app.animations.cursor_smear_active(
                id,
                &app.animation_settings,
                Instant::now(),
            ));

            app.tabs[0].editor.extra_cursors.push((12, 2));
            app.rendering_animation_frame = true;
            let paused = render(&mut app);
            app.rendering_animation_frame = false;
            assert!(app.animations.take_events(id).is_empty());
            assert!(
                app.animations
                    .fire_particles(id, &app.animation_settings, Instant::now())
                    .is_empty()
            );
            assert_eq!(
                app.animations
                    .next_frame_wait(&app.animation_settings, Instant::now()),
                None,
            );
            let settings = app.animation_settings.clone();
            app.animation_settings.intensity = 0;
            assert_eq!(paused, render(&mut app));
            app.animation_settings = settings;

            app.tabs[0].editor.extra_cursors.clear();
            render(&mut app);
            // Adding native carets returns comparison focus to the target.
            let resumed_id = crate::diff_scope_source::animation_view_id(&mut app).unwrap();
            assert!(!app.animations.cursor_smear_active(
                resumed_id,
                &app.animation_settings,
                Instant::now(),
            ));
            navigate(&mut app, 1, KeyModifiers::NONE);
            render(&mut app);
            assert!(app.animations.cursor_smear_active(
                resumed_id,
                &app.animation_settings,
                Instant::now(),
            ));
            assert_sources(&app, &base, &target, version);
        }
    }
}

#[test]
fn diff_cursor_effects_survive_final_paint_in_full_compact_and_commit_views() {
    for (mode, compact, commit) in [
        (DiffMode::SideBySide, false, false),
        (DiffMode::SideBySide, true, false),
        (DiffMode::Inline, false, false),
        (DiffMode::Inline, true, false),
        (DiffMode::SideBySide, true, true),
        (DiffMode::Inline, true, true),
    ] {
        for effect in [AnimationEffect::CursorSmear, AnimationEffect::CursorFire] {
            let context = format!(
                "{}, compact={compact}, commit={commit}, {effect:?}",
                view_name(mode)
            );
            let (mut app, base, target) = comparison(mode, compact, commit);
            let version = app.editor().unwrap().content_version;
            app.animation_settings.set(effect, true);
            let initial = render(&mut app);
            let old = source_origin(&app, 13, false);
            assert_eq!(initial[old].bg, CURSOR, "initial caret: {context}");
            assert!(
                app.animations
                    .next_frame_wait(&app.animation_settings, Instant::now())
                    .is_none(),
                "an untouched diff does not need animation frames: {context}"
            );
            let probe = Instant::now();
            navigate(&mut app, 12, KeyModifiers::NONE);
            let moving = render(&mut app);
            let origin = source_origin(&app, 13, false);
            assert_eq!(
                origin, old,
                "navigation keeps the viewport fixed: {context}"
            );
            let destination = (origin.0 + 12, origin.1);
            assert!(
                app.animations.cursor_smear_active(
                    app.editor().unwrap().navigation_id,
                    &app.animation_settings,
                    probe,
                ),
                "the final mapped caret must observe cursor motion: {context}"
            );
            if effect == AnimationEffect::CursorSmear {
                assert_eq!(moving[old].bg, CURSOR, "animated origin: {context}");
            }
            assert_ne!(
                moving[destination].bg, CURSOR,
                "stationary destination is hidden: {context}"
            );
            assert!(
                app.animations
                    .next_frame_wait(&app.animation_settings, probe)
                    .is_some(),
                "moving diff cursor requests frames: {context}"
            );
            if effect == AnimationEffect::CursorFire {
                app.animations
                    .advance_frame(&app.animation_settings, probe + Duration::from_millis(80));
                assert!(
                    !app.animations
                        .fire_particles(
                            app.editor().unwrap().navigation_id,
                            &app.animation_settings,
                            probe + Duration::from_millis(80),
                        )
                        .is_empty(),
                    "Fire Hazard independently emits particles: {context}"
                );
            }
            app.cursor_visible = false;
            let blinking = render(&mut app);
            app.animation_settings.set(effect, false);
            let uncovered = render(&mut app);
            assert_ne!(
                blinking, uncovered,
                "blink preserves a live cursor effect: {context}"
            );
            app.cursor_visible = true;
            let ordinary = render(&mut app);
            assert_ne!(
                moving[old].bg, ordinary[old].bg,
                "the animated cursor starts at the old caret: {context}"
            );
            assert_eq!(
                ordinary[destination].bg, CURSOR,
                "disabled effect restores caret: {context}"
            );
            for (index, cell) in ordinary.content.iter().enumerate() {
                if !cell.symbol().trim().is_empty() {
                    assert_eq!(
                        moving.content[index].symbol(),
                        cell.symbol(),
                        "effect overwrote source or diff decoration at {index}: {context}"
                    );
                }
            }
            if commit {
                app.animation_settings.set(effect, true);
                render(&mut app);
                navigate(&mut app, 4, KeyModifiers::NONE);
                render(&mut app);
                let id = app.editor().unwrap().navigation_id;
                assert!(
                    app.animations
                        .cursor_smear_active(id, &app.animation_settings, Instant::now(),),
                    "the focus check begins with live cursor motion: {context}"
                );
                for focus in [
                    crate::commit_details::Focus::Files,
                    crate::commit_details::Focus::Message,
                ] {
                    app.commit_details.focus = focus;
                    let unfocused = render(&mut app);
                    assert!(
                        app.animations
                            .next_frame_wait(&app.animation_settings, Instant::now())
                            .is_none(),
                        "leaving the patch retires cursor animation frames: {context}, {focus:?}"
                    );
                    app.animation_settings.set(effect, false);
                    let plain_unfocused = render(&mut app);
                    assert_eq!(
                        unfocused, plain_unfocused,
                        "cursor effects cannot leak into an unfocused commit patch: {context}, {focus:?}"
                    );
                    app.animation_settings.set(effect, true);
                }
                app.commit_details.focus = crate::commit_details::Focus::Diff;
                let returned = render(&mut app);
                let origin = source_origin(&app, 13, false);
                assert_eq!(returned[(origin.0 + 16, origin.1)].bg, CURSOR);
                assert!(
                    !app.animations.cursor_smear_active(
                        id,
                        &app.animation_settings,
                        Instant::now()
                    ),
                    "returning to the patch restores its ordinary caret without replay: {context}"
                );
            }
            assert_sources(&app, &base, &target, version);
        }
    }
}

#[test]
fn original_diff_cursor_effects_follow_navigation_and_preserve_copy_selection() {
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        for effect in [AnimationEffect::CursorSmear, AnimationEffect::CursorFire] {
            let context = format!("{}, {effect:?}", view_name(mode));
            let (mut app, base, target) = comparison(mode, true, false);
            let version = app.editor().unwrap().content_version;
            app.animation_settings.set(effect, true);
            render(&mut app);
            let old = source_origin(&app, 12, true);
            click(&mut app, old);
            let initial = render(&mut app);
            assert_eq!(initial[old].bg, CURSOR, "original caret: {context}");
            let probe = Instant::now();
            navigate(&mut app, 12, KeyModifiers::NONE);
            let moving = render(&mut app);
            let destination = (old.0 + 12, old.1);
            if effect == AnimationEffect::CursorSmear {
                assert_eq!(
                    moving[old].bg, CURSOR,
                    "original smear begins at its old caret: {context}"
                );
            }
            assert_ne!(
                moving[destination].bg, CURSOR,
                "original stationary caret is suppressed: {context}"
            );
            assert!(
                app.animations
                    .next_frame_wait(&app.animation_settings, probe)
                    .is_some(),
                "original navigation requests frames without Scope Connector: {context}"
            );
            navigate(&mut app, 3, KeyModifiers::SHIFT);
            render(&mut app);
            assert_eq!(
                app.diff_selected_text().as_deref(),
                Some("cde"),
                "original selection: {context}"
            );
            let selected = app.diff_selected_text();
            app.animation_settings.set(effect, false);
            let ordinary = render(&mut app);
            assert_ne!(
                moving[old].bg, ordinary[old].bg,
                "the animated original cursor starts at the old caret: {context}"
            );
            assert_eq!(
                ordinary[(old.0 + 15, old.1)].bg,
                CURSOR,
                "original destination caret: {context}"
            );
            assert_eq!(
                app.diff_selected_text(),
                selected,
                "effect preserves copy range: {context}"
            );
            assert_sources(&app, &base, &target, version);
        }
    }
}

fn scoped_comparison(mode: DiffMode) -> (App, &'static str, &'static str) {
    let base = "fn run() {\n    if ready {\n        obsolete();\n        removed();\n    }\n}\n";
    let target = "fn run() {\n    if ready {\n        work_new();\n    }\n}\n";
    let (mut app, _, _) = comparison(mode, false, false);
    let mut editor = Editor::test_with_text(target);
    editor.file_path = Some("diff-animation.rs".into());
    editor.language_override = Some("Rust".into());
    editor.read_only = true;
    editor.word_wrap = false;
    editor.set_cursor(2, 8);
    let index = crate::animation_structural_scope::StructuralScopeRequest::from_editor(&editor)
        .unwrap()
        .build(None, || false)
        .unwrap();
    app.tabs[0].editor = editor;
    app.diff_target_path = app.editor().unwrap().file_path.clone();
    app.compare_base_content = Some(base.into());
    app.set_compare_diff_lines(crate::app::compute_file_diff(base, target));
    app.cached_structural_scope_index = Some(Arc::new(index));
    (app, base, target)
}

#[test]
fn diff_scope_and_cursor_effects_share_motion_through_repeated_diff_frames() {
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        let (mut app, base, target) = scoped_comparison(mode);
        for effect in [
            AnimationEffect::ScopeConnector,
            AnimationEffect::CursorSmear,
            AnimationEffect::CursorFire,
        ] {
            app.animation_settings.set(effect, true);
        }
        let initial = render(&mut app);
        let origin = source_origin(&app, 2, false);
        let old = (origin.0 + 8, origin.1);
        assert_eq!(initial[old].bg, CURSOR);
        let probe = Instant::now();
        navigate(&mut app, 8, KeyModifiers::NONE);
        let moving = render(&mut app);
        assert_ne!(moving[(origin.0 + 16, origin.1)].bg, CURSOR);
        let id = app.editor().unwrap().navigation_id;
        for frame in 0..2 {
            render(&mut app);
            assert!(
                app.animations
                    .cursor_smear_active(id, &app.animation_settings, probe),
                "the scope pass must retain cursor motion: {}, frame={frame}",
                view_name(mode)
            );
        }
        for _ in 0..4 {
            crate::input::handle_editor_key(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
        }
        render(&mut app);
        assert_eq!(
            app.editor().unwrap().selected_text().as_deref(),
            Some("_new")
        );
        assert!(
            app.animations
                .cursor_smear_active(id, &app.animation_settings, probe),
            "selection and scope preserve cursor motion: {}",
            view_name(mode)
        );
        app.animations
            .advance_frame(&app.animation_settings, probe + Duration::from_millis(80));
        assert!(
            !app.animations
                .fire_particles(
                    id,
                    &app.animation_settings,
                    probe + Duration::from_millis(80)
                )
                .is_empty(),
            "the scope pass must not clear fire particles"
        );
        app.animation_settings
            .set(AnimationEffect::CursorSmear, false);
        app.animation_settings
            .set(AnimationEffect::CursorFire, false);
        let ordinary = render(&mut app);
        assert_ne!(
            moving[old].bg,
            ordinary[old].bg,
            "the shared pass paints the moving cursor: {}",
            view_name(mode)
        );
        assert_eq!(app.editor().unwrap().rope.to_string(), target);
        assert_eq!(
            app.editor().unwrap().selected_text().as_deref(),
            Some("_new")
        );
        assert_eq!(app.compare_base_content.as_deref(), Some(base));
    }
}

#[test]
fn backward_diff_selection_remains_under_the_hidden_smear_destination() {
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        let (mut app, base, target) = comparison(mode, true, false);
        let version = app.editor().unwrap().content_version;
        app.editor_mut().unwrap().set_cursor(13, 20);
        app.animation_settings
            .set(AnimationEffect::CursorSmear, true);
        render(&mut app);
        for _ in 0..12 {
            crate::input::handle_editor_key(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
        }
        let moving = render(&mut app);
        let origin = source_origin(&app, 13, false);
        let destination = (origin.0 + 8, origin.1);
        let selected = app.editor().unwrap().selected_text();
        assert_eq!(selected.as_deref(), Some("ext_abcdefgh"));
        app.animation_settings
            .set(AnimationEffect::CursorSmear, false);
        app.cursor_visible = false;
        let uncovered = render(&mut app);
        assert_eq!(
            moving[destination],
            uncovered[destination],
            "the stationary caret must restore the selected source surface: {}",
            view_name(mode)
        );
        assert_eq!(moving[destination].bg, app.colors.bg_selection);
        assert_eq!(app.editor().unwrap().selected_text(), selected);
        assert_sources(&app, &base, &target, version);
    }
}

#[test]
fn editable_diff_retains_insert_animation_when_scope_source_revision_changes() {
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        let (mut app, base, target) = scoped_comparison(mode);
        app.tabs[0].kind = TabKind::File;
        app.diff_tabs.owner = None;
        app.editor_mut().unwrap().read_only = false;
        app.animation_settings
            .set(AnimationEffect::ScopeConnector, true);
        app.animation_settings
            .set(AnimationEffect::WhitespaceReveal, true);
        render(&mut app);
        app.animations.advance_frame(
            &app.animation_settings,
            Instant::now() + Duration::from_secs(2),
        );
        render(&mut app);
        assert!(
            app.animations
                .next_frame_wait(&app.animation_settings, Instant::now())
                .is_none(),
            "the initial scope entrance is settled"
        );
        let version = app.editor().unwrap().content_version;
        for _ in 0..4 {
            crate::input::handle_editor_key(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
        }
        let edited = target.replacen("        work_new", "            work_new", 1);
        assert_eq!(app.editor().unwrap().rope.to_string(), edited);
        assert_eq!(app.editor().unwrap().cursor_col, 12);
        assert!(app.editor().unwrap().content_version > version);
        // Install the worker's new comparison before painting, as the live
        // editable diff does. Endpoint cache invalidation must preserve the
        // insertion observed from the previous source revision.
        app.set_compare_diff_lines(crate::app::compute_file_diff(base, &edited));
        let index = crate::animation_structural_scope::StructuralScopeRequest::from_editor(
            app.editor().unwrap(),
        )
        .unwrap()
        .build(None, || false)
        .unwrap();
        app.cached_structural_scope_index = Some(Arc::new(index));
        let painted = render(&mut app);
        let origin = source_origin(&app, 2, false);
        assert_eq!(painted[(origin.0 + 12, origin.1)].bg, CURSOR);
        app.animation_settings
            .set(AnimationEffect::ScopeConnector, false);
        render(&mut app);
        assert!(
            app.animations
                .next_frame_wait(&app.animation_settings, Instant::now())
                .is_some(),
            "Whitespace Reveal retains its own live event after the scope pass: {}",
            view_name(mode)
        );
        app.animation_settings
            .set(AnimationEffect::WhitespaceReveal, false);
        render(&mut app);
        assert!(
            app.animations
                .next_frame_wait(&app.animation_settings, Instant::now())
                .is_none(),
            "disabled insert effect leaves an idle diff"
        );
        assert_eq!(app.editor().unwrap().rope.to_string(), edited);
        assert_eq!(app.compare_base_content.as_deref(), Some(base));
        assert!(app.editor().unwrap().modified);
    }
}

#[test]
fn cosmetic_diff_frames_retain_geometry_and_refresh_it_for_selection_and_source_sides() {
    for mode in [DiffMode::SideBySide, DiffMode::Inline] {
        let (mut app, _, _) = scoped_comparison(mode);
        app.animation_settings
            .set(AnimationEffect::ScopeConnector, true);
        render(&mut app);
        app.animations.advance_frame(
            &app.animation_settings,
            Instant::now() + Duration::from_secs(6),
        );
        let expected = render(&mut app);
        let builds = app.animations.diff_geometry.builds;
        let reuses = app.animations.diff_geometry.reuses;
        for _ in 0..20 {
            app.rendering_animation_frame = true;
            assert_eq!(
                render(&mut app),
                expected,
                "{} target geometry",
                view_name(mode)
            );
            app.rendering_animation_frame = false;
        }
        assert_eq!(app.animations.diff_geometry.builds, builds);
        assert_eq!(app.animations.diff_geometry.reuses, reuses + 20);

        // Selection protection changes even if the caret and source do not.
        let cursor = app.editor().unwrap().cursor_line;
        app.editor_mut().unwrap().selection_anchor = Some((cursor.saturating_sub(1), 0));
        app.rendering_animation_frame = true;
        render(&mut app);
        app.rendering_animation_frame = false;
        assert!(app.animations.diff_geometry.builds > builds);

        let old = source_origin(&app, 2, true);
        click(&mut app, old);
        render(&mut app);
        app.animations.advance_frame(
            &app.animation_settings,
            Instant::now() + Duration::from_secs(6),
        );
        let original = render(&mut app);
        assert!(app.diff_scope_cursor().unwrap().0);
        let builds = app.animations.diff_geometry.builds;
        let reuses = app.animations.diff_geometry.reuses;
        app.rendering_animation_frame = true;
        assert_eq!(
            render(&mut app),
            original,
            "{} original geometry",
            view_name(mode)
        );
        app.rendering_animation_frame = false;
        assert_eq!(app.animations.diff_geometry.builds, builds);
        assert_eq!(app.animations.diff_geometry.reuses, reuses + 1);
    }
}
