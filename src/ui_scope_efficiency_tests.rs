//! Exercise retained scope state through the ordinary editor renderer.
use super::*;

use crate::{
    animation_structural_scope::StructuralScopeRequest,
    app::{ActivePane, Tab},
    app_theme::EditorTheme,
    editor::Editor,
};
use ratatui::{Terminal, backend::TestBackend};
use std::{sync::Arc, time::Duration};

const SOURCE: &str = concat!(
    "fn outer() {\n",
    "    if ready {\n",
    "        process(value);\n",
    "        finish();\n",
    "    }\n",
    "}\n",
);

fn flat_surface(app: &mut App) {
    app.colors.gradients.bg_editor = crate::app_theme::ThemeGradientSpec {
        start: app.colors.bg_editor,
        end: app.colors.bg_editor,
        angle_deg: 0.0,
    };
    app.colors.bg_line_highlight = app.colors.bg_editor;
    app.colors.gradients.bg_line_highlight = app.colors.gradients.bg_editor;
}

fn fixture() -> App {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text(SOURCE);
    editor.file_path = Some("scope-efficiency.rs".into());
    editor.language_override = Some("Rust".into());
    editor.word_wrap = false;
    editor.cursor_line = 2;
    editor.cursor_col = 12;
    let structural = StructuralScopeRequest::from_editor(&editor)
        .unwrap()
        .build(None, || false)
        .unwrap();
    let brackets = crate::background::BracketDepthIndex::build(
        editor.rope.clone(),
        editor.navigation_id,
        editor.content_version,
        editor.wrap_revision,
    );
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.show_explorer = false;
    app.show_activity_bar = false;
    app.show_menu_bar = false;
    app.show_tab_bar = false;
    app.show_status_bar = false;
    app.show_breadcrumbs = false;
    app.show_minimap = false;
    app.show_indent_guides = false;
    app.show_color_preview = false;
    app.sticky_scroll = false;
    app.rulers.clear();
    app.cursor_blinking = false;
    app.bracket_pair_colorization = true;
    app.cached_structural_scope_index = Some(Arc::new(structural));
    app.cached_bracket_depth_index = Some(Arc::new(brackets));
    for effect in Effect::ALL {
        app.animation_settings.set(effect, false);
    }
    app.animation_settings.set(Effect::ScopeConnector, true);
    app.animation_settings.intensity = 100;
    flat_surface(&mut app);
    app
}

fn render(app: &mut App) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(100, 18)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    terminal.backend().buffer().clone()
}

fn settle(app: &mut App) -> Buffer {
    render(app);
    app.animations.advance_frame(
        &app.animation_settings,
        Instant::now() + Duration::from_secs(2),
    );
    render(app)
}

fn rail(app: &App, frame: &Buffer) -> Vec<(u16, u16, Color)> {
    let area = app.code_area.unwrap();
    (area.y..area.bottom())
        .flat_map(|y| (area.x..area.right()).map(move |x| (x, y)))
        .filter_map(|(x, y)| {
            let cell = &frame[(x, y)];
            (scope_connections(cell.symbol()) != 0).then_some((x, y, cell.fg))
        })
        .collect()
}

fn rail_color(app: &App, frame: &Buffer) -> Color {
    let cells = rail(app, frame);
    let color = cells.first().expect("a visible connector").2;
    assert!(
        cells.iter().all(|cell| cell.2 == color),
        "a settled guide has one hue on a flat surface: {cells:?}"
    );
    color
}

fn assert_idle(app: &App) {
    assert_eq!(
        app.animations
            .next_frame_wait(&app.animation_settings, Instant::now()),
        None,
        "editing inside a retained scope must not replay its entrance"
    );
}

#[test]
fn scope_profiles_render_selected_pairs_without_raw_fallback_and_keep_settled_routes() {
    use crate::scope_bubble_config::{Config, Delimiter, Rules, Syntax};
    let mut app = fixture();
    let mut editor = Editor::test_with_text("call(\n    value,\n    other\n);\n");
    editor.language_override = Some("Custom".into());
    editor.cursor_col = 4;
    let profile = |pairs| Rules::compile(&[("Custom".into(), Config {
        syntax: Syntax::Cpp, delimiters: Some(pairs), language_blocks: false,
    })].into());
    editor.scope_bubble_rules = profile(vec![]);
    app.tabs[0] = Tab::new_file(editor);
    app.cached_structural_scope_index = None;
    app.cached_bracket_depth_index = None;
    app.cached_matching_bracket = Some((3, 0));
    app.cached_cursor_match_target = app.cursor_match_target();
    let hidden = settle(&mut app);
    assert!(rail(&app, &hidden).is_empty(), "raw matching must not override the selected scope rules");
    app.tabs[0].editor.scope_bubble_rules = profile(vec![Delimiter::Parentheses]);
    app.tabs[0].editor.cursor_line = 1;
    app.tabs[0].editor.cursor_col = 5;
    let visible = settle(&mut app);
    assert!(!rail(&app, &visible).is_empty());
    assert_source_preserved(&app, &visible);
    assert_idle(&app);
    let before = stats(&app);
    for col in 5..8 {
        app.tabs[0].editor.cursor_col = col;
        let visible = render(&mut app);
        assert!(!rail(&app, &visible).is_empty());
        assert_eq!(stats(&app), before, "cursor steps must reuse the contour");
        assert_idle(&app);
    }
}

fn assert_source_preserved(app: &App, frame: &Buffer) {
    let area = app.code_area.unwrap();
    for (row, line) in app.tabs[0]
        .editor
        .rope
        .lines()
        .take(area.height as usize)
        .enumerate()
    {
        for (col, ch) in line.chars().take(area.width as usize).enumerate() {
            if !ch.is_whitespace() {
                assert_eq!(
                    frame[(area.x + col as u16, area.y + row as u16)].symbol(),
                    ch.to_string(),
                    "scope ink replaced source at ({row}, {col})"
                );
            }
        }
    }
}

fn stats(app: &App) -> (usize, usize) {
    app.animations
        .scope_paint
        .stats(app.tabs[0].editor.navigation_id)
}

#[test]
fn language_scopes_render_without_covering_source_or_restarting_on_navigation() {
    for (language, source, expected) in [
        (
            "Python",
            "def outer():\n    if ready:\n        process(value)\n        finish()\n    after()\n",
            ((1, 4), (3, 15)),
        ),
        (
            "C++",
            "void outer() {\n    if (ready)\n        process(value);\n    after();\n}\n",
            ((1, 4), (2, 22)),
        ),
    ] {
        let mut app = fixture();
        let mut editor = Editor::test_with_text(source);
        editor.language_override = Some(language.into());
        editor.cursor_line = 2;
        editor.cursor_col = 12;
        editor.word_wrap = false;
        let index = StructuralScopeRequest::from_editor(&editor)
            .unwrap()
            .build(None, || false)
            .unwrap();
        app.tabs[0] = Tab::new_file(editor);
        app.cached_structural_scope_index = Some(Arc::new(index));
        app.cached_bracket_depth_index = None;
        app.bracket_pair_colorization = false;
        assert_eq!(scope(&mut app), expected, "{language}");
        let first = settle(&mut app);
        assert!(!rail(&app, &first).is_empty(), "{language}");
        assert_source_preserved(&app, &first);
        assert_idle(&app);
        let baseline = stats(&app);
        for col in 8..18 {
            app.tabs[0].editor.cursor_col = col;
            let painted = render(&mut app);
            assert_source_preserved(&app, &painted);
            assert_eq!(scope(&mut app), expected, "{language}:{col}");
            assert_eq!(stats(&app), baseline, "{language} rebuilt its contour");
            assert_idle(&app);
        }
        assert_eq!(app.tabs[0].editor.rope.to_string(), source);
    }
}

#[test]
fn scope_language_disable_immediately_clears_motion_without_disabling_other_effects() {
    let mut app = fixture();
    let visible = settle(&mut app);
    assert!(!rail(&app, &visible).is_empty());
    app.animation_settings
        .set_duration(Effect::ScopeConnector, 5_000);
    // Change scopes to exercise both the old outgoing outline and a new
    // entrance. A language preference must remove both on the next paint.
    app.tabs[0].editor.cursor_line = 0;
    app.tabs[0].editor.cursor_col = 4;
    render(&mut app);
    assert!(
        app.animations
            .next_frame_wait(&app.animation_settings, Instant::now())
            .is_some()
    );
    app.animation_settings.apply(&crate::settings::Settings {
        animation_scope_bubble_disabled_languages: Some(vec!["RUST".into()]),
        ..Default::default()
    });
    let disabled = render(&mut app);
    assert!(
        rail(&app, &disabled).is_empty(),
        "language disable retained a fading outline"
    );
    assert_source_preserved(&app, &disabled);
    assert_idle(&app);
    assert!(app.animation_settings.enabled(Effect::ScopeConnector));

    app.animation_settings.set(Effect::CursorSmear, true);
    render(&mut app);
    app.tabs[0].editor.cursor_line = 2;
    app.tabs[0].editor.cursor_col = 12;
    let moved = render(&mut app);
    assert!(rail(&app, &moved).is_empty());
    assert!(
        app.animations.cursor_smear_active(
            app.tabs[0].editor.navigation_id,
            &app.animation_settings,
            Instant::now(),
        ),
        "the language preference must leave cursor effects active"
    );

    // A language override changes the effective language without changing the
    // document identity. Scope Bubble should become available immediately.
    app.animation_settings
        .set_duration(Effect::ScopeConnector, 240);
    app.tabs[0].editor.language_override = Some("C++".into());
    let index = StructuralScopeRequest::from_editor(&app.tabs[0].editor)
        .unwrap()
        .build(None, || false)
        .unwrap();
    app.cached_structural_scope_index = Some(Arc::new(index));
    let enabled = settle(&mut app);
    assert!(
        !rail(&app, &enabled).is_empty(),
        "another language was disabled globally"
    );
    assert!(app.animation_settings.enabled(Effect::CursorSmear));
    assert_idle(&app);
}

#[test]
fn scope_language_disable_immediately_clears_target_diff_motion() {
    for mode in [
        crate::app::DiffMode::SideBySide,
        crate::app::DiffMode::Inline,
    ] {
        let mut app = fixture();
        let base = SOURCE.replace("process(value)", "process(previous)");
        app.tabs[0].kind = crate::app::TabKind::Diff(91);
        app.tabs[0].editor.read_only = true;
        app.diff_tabs.owner = Some(91);
        app.diff_mode = mode;
        app.diff_target_path = app.tabs[0].editor.file_path.clone();
        app.compare_base_content = Some(base.clone().into());
        app.set_compare_diff_lines(crate::app::compute_file_diff(&base, SOURCE));
        let visible = settle(&mut app);
        assert!(
            !rail(&app, &visible).is_empty(),
            "the target diff should start with a bubble"
        );
        app.animation_settings
            .set_duration(Effect::ScopeConnector, 5_000);
        app.tabs[0].editor.cursor_line = 0;
        app.tabs[0].editor.cursor_col = 4;
        render(&mut app);
        assert!(
            app.animations
                .next_frame_wait(&app.animation_settings, Instant::now())
                .is_some()
        );
        app.animation_settings.apply(&crate::settings::Settings {
            animation_scope_bubble_disabled_languages: Some(vec!["rust".into()]),
            ..Default::default()
        });
        let disabled = render(&mut app);
        assert!(
            rail(&app, &disabled).is_empty(),
            "a disabled target diff retained scope ink"
        );
        assert_idle(&app);
        assert!(app.animation_settings.enabled(Effect::ScopeConnector));
    }
}

fn scope(app: &mut App) -> ScopePair {
    app.animations
        .scope(
            &app.tabs[0].editor,
            app.cached_structural_scope_index.as_deref(),
        )
        .expect("the cursor remains inside a complete scope")
}

#[test]
fn scope_hue_and_membership_survive_typing_with_stale_worker_indexes() {
    let mut app = fixture();
    let first = settle(&mut app);
    let color = rail_color(&app, &first);
    let pair = scope(&mut app);
    assert_idle(&app);

    for step in 0..4 {
        let editor = &mut app.tabs[0].editor;
        match step {
            0 => editor.insert_char('x'),
            1 => editor.backspace(),
            2 => {
                editor.cursor_line = 1;
                editor.cursor_col = 10;
                editor.insert_char('x');
            }
            3 => {
                editor.cursor_line = 1;
                editor.cursor_col = 11;
                editor.backspace();
            }
            _ => unreachable!(),
        }
        // Header edits move the opener while the rendered cursor remains in
        // the body, so this is still the same semantic scope.
        editor.cursor_line = 2;
        editor.cursor_col = if step == 0 { 13 } else { 12 };
        assert!(
            !app.cached_bracket_depth_index
                .as_ref()
                .unwrap()
                .matches(editor)
        );
        assert!(
            !app.cached_structural_scope_index
                .as_ref()
                .unwrap()
                .matches(editor)
        );
        let painted = render(&mut app);
        assert_eq!(
            rail_color(&app, &painted),
            color,
            "edit {step} lost its hue"
        );
        assert_idle(&app);
        assert_source_preserved(&app, &painted);
        let current = scope(&mut app);
        assert_eq!(current.1, pair.1);
        assert_eq!(current.0, (pair.0.0, pair.0.1 + usize::from(step == 2)));
    }
}

#[test]
fn repeated_frames_carets_themes_and_body_edits_reuse_scope_geometry() {
    let mut app = fixture();
    let first = settle(&mut app);
    let dark = rail_color(&app, &first);
    let baseline = stats(&app);
    assert!(
        baseline.0 > 0 && baseline.1 > 0,
        "the cold frame builds geometry"
    );

    for frame in 0..240 {
        let editor = &mut app.tabs[0].editor;
        editor.cursor_col = frame % 8;
        editor.selection_anchor = (frame % 3 == 0).then_some((2, 0));
        app.colors = if frame % 2 == 0 {
            EditorTheme::Light.colors()
        } else {
            EditorTheme::Dark.colors()
        };
        flat_surface(&mut app);
        let painted = render(&mut app);
        if frame == 0 {
            assert_ne!(
                rail_color(&app, &painted),
                dark,
                "the current theme supplies ink"
            );
        }
        assert_eq!(
            stats(&app),
            baseline,
            "frame {frame} rebuilt scope geometry"
        );
        assert_idle(&app);
        assert_source_preserved(&app, &painted);
    }

    for edit in 0..24 {
        let editor = &mut app.tabs[0].editor;
        editor.cursor_line = 2;
        editor.cursor_col = 9;
        editor.selection_anchor = Some((2, 8));
        editor.insert_char_replacing_selection(if edit % 2 == 0 { 'q' } else { 'p' });
        let painted = render(&mut app);
        assert_eq!(
            stats(&app),
            baseline,
            "a same-width token edit must reuse the route and untouched endpoint lines"
        );
        assert_eq!(rail_color(&app, &painted), dark);
        assert_idle(&app);
        assert_source_preserved(&app, &painted);
    }
}

#[test]
fn occupying_cached_route_whitespace_rebuilds_without_overwriting_source() {
    let mut app = fixture();
    let first = settle(&mut app);
    let color = rail_color(&app, &first);
    let baseline = stats(&app);
    let area = app.code_area.unwrap();
    let (x, y, _) = rail(&app, &first)
        .into_iter()
        .find(|&(x, y, _)| {
            let row = usize::from(y - area.y);
            (2..=3).contains(&row)
                && app.tabs[0]
                    .editor
                    .rope
                    .line(row)
                    .get_char(usize::from(x - area.x))
                    == Some(' ')
        })
        .expect("the route crosses existing body whitespace");
    let row = usize::from(y - area.y);
    let col = usize::from(x - area.x);
    let editor = &mut app.tabs[0].editor;
    assert_eq!(editor.rope.line(row).char(col), ' ');
    editor.cursor_line = row;
    editor.cursor_col = col + 1;
    editor.selection_anchor = Some((row, col));
    editor.insert_char_replacing_selection('X');

    let painted = render(&mut app);
    assert_eq!(painted[(x, y)].symbol(), "X");
    let updated = stats(&app);
    assert_eq!(
        updated.0,
        baseline.0 + 1,
        "changed passability needs one new route"
    );
    assert_eq!(
        updated.1, baseline.1,
        "body occupancy does not change endpoint projection"
    );
    assert_eq!(rail_color(&app, &painted), color);
    assert_idle(&app);
    assert_source_preserved(&app, &painted);
}

#[test]
fn scope_endpoints_rebase_across_newline_insert_and_delete_without_reentry() {
    let mut app = fixture();
    let first = settle(&mut app);
    let color = rail_color(&app, &first);
    let pair = scope(&mut app);
    let editor = &mut app.tabs[0].editor;
    editor.cursor_col = editor.line_len(2);
    editor.insert_newline();
    editor.cursor_line = 2;
    editor.cursor_col = 12;
    let inserted = render(&mut app);
    assert_eq!(scope(&mut app), (pair.0, (pair.1.0 + 1, pair.1.1)));
    assert_eq!(rail_color(&app, &inserted), color);
    assert_idle(&app);
    assert_source_preserved(&app, &inserted);

    let editor = &mut app.tabs[0].editor;
    editor.cursor_line = 3;
    editor.cursor_col = 0;
    editor.backspace();
    editor.cursor_line = 2;
    editor.cursor_col = 12;
    let deleted = render(&mut app);
    assert_eq!(scope(&mut app), pair);
    assert_eq!(rail_color(&app, &deleted), color);
    assert_idle(&app);
    assert_source_preserved(&app, &deleted);
}

#[test]
fn deleting_a_scope_delimiter_invalidates_retained_membership_and_geometry() {
    let mut app = fixture();
    settle(&mut app);
    let pair = scope(&mut app);
    let baseline = stats(&app);
    let editor = &mut app.tabs[0].editor;
    editor.cursor_line = pair.0.0;
    editor.cursor_col = pair.0.1;
    editor.delete_char();
    editor.cursor_line = 2;
    editor.cursor_col = 12;
    render(&mut app);
    let changed = scope(&mut app);
    assert_ne!(
        changed, pair,
        "a deleted opener cannot retain its old scope"
    );
    assert_eq!(
        changed.0.0, 0,
        "the remaining enclosing block owns the connector"
    );
    assert!(
        stats(&app).0 > baseline.0,
        "new membership needs new geometry"
    );
    let settled = settle(&mut app);
    assert!(!rail(&app, &settled).is_empty());
    assert_source_preserved(&app, &settled);
}
