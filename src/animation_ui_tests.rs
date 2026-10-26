//! Exercise the real workbench renderer with optional effects enabled.
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

use crate::{
    animation_config::AnimationEffect,
    app::{App, Tab},
    editor::Editor,
};

#[path = "animation_scope_blame_tests.rs"]
mod scope_blame_tests;

#[path = "animation_multicursor_tests.rs"]
mod multicursor_tests;

fn render(app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    terminal.backend().buffer().clone()
}

fn fixture(text: &str) -> App {
    let mut app = App::test_new();
    app.tabs.push(Tab::new_file(Editor::test_with_text(text)));
    app.active_pane = crate::app::ActivePane::Editor;
    app.show_explorer = false;
    app.show_status_bar = false;
    app.cursor_blinking = false;
    app
}

#[test]
fn all_effects_preserve_source_and_selection_across_editor_layouts() {
    for (width, height, wrapped, large) in [
        (120, 32, false, false),
        (45, 16, true, false),
        (20, 8, false, false),
        (100, 24, false, true),
        (3, 3, true, false),
    ] {
        let mut app = fixture("\t界 cafe\u{301}\n    if ready\n        echo 'value'\n    endif\n");
        app.tabs[0].editor.word_wrap = wrapped;
        app.tabs[0].editor.large_file_mode = large;
        for effect in AnimationEffect::ALL {
            app.animation_settings.set(effect, true);
        }
        render(&mut app, width, height);
        app.tabs[0].editor.cursor_line = 2;
        app.tabs[0].editor.cursor_col = 8;
        app.tabs[0].editor.selection_anchor = Some((1, 4));
        app.tabs[0].editor.extra_cursors.push((3, 4));
        app.tabs[0].editor.bookmarks.insert(2);
        let editor = &app.tabs[0].editor;
        let source = editor.rope.to_string();
        let selection = editor.selection_range();
        let cursor = (editor.cursor_line, editor.cursor_col);
        let extra = editor.extra_cursors.clone();
        let version = editor.content_version;
        render(&mut app, width, height);
        render(&mut app, width, height);
        let editor = &app.tabs[0].editor;
        assert_eq!(editor.rope.to_string(), source);
        assert_eq!(editor.selection_range(), selection);
        assert_eq!((editor.cursor_line, editor.cursor_col), cursor);
        assert_eq!(editor.extra_cursors, extra);
        assert_eq!(editor.content_version, version);
        assert!(editor.bookmarks.contains(&2));
    }
}

#[test]
fn smear_replaces_the_stationary_primary_caret_in_every_editor_path() {
    for (width, wrapped, large, target) in [
        (120, false, false, 20),
        (45, true, false, 48),
        (100, false, true, 20),
        (120, false, false, 80),
        (45, true, false, 80),
        (100, false, true, 80),
    ] {
        let mut app = fixture(&"x".repeat(80));
        app.show_minimap = false;
        // The EOL cases exercise a ruler drawn through the blank caret cell.
        // That decoration must preserve the source anchor used by the smear.
        app.rulers = vec![80];
        app.colors.gradients.bg_editor = crate::app_theme::ThemeGradientSpec {
            start: ratatui::style::Color::Rgb(12, 18, 34),
            end: ratatui::style::Color::Rgb(42, 56, 78),
            angle_deg: 45.0,
        };
        app.colors.gradients.bg_line_highlight = crate::app_theme::ThemeGradientSpec {
            start: ratatui::style::Color::Rgb(24, 34, 48),
            end: ratatui::style::Color::Rgb(54, 64, 88),
            angle_deg: 20.0,
        };
        app.tabs[0].editor.word_wrap = wrapped;
        app.tabs[0].editor.large_file_mode = large;
        app.animation_settings
            .set(AnimationEffect::CursorSmear, true);
        app.animation_settings.duration_ms = 1500;
        let initial = render(&mut app, width, 24);
        let initial_area = app.code_area;
        let old_caret = initial
            .content
            .iter()
            .position(|cell| cell.bg == app.colors.bg_cursor)
            .expect("initial caret");
        app.tabs[0].editor.cursor_col = target;
        let moving = render(&mut app, width, 24);
        assert_eq!(
            app.code_area, initial_area,
            "the test keeps the viewport fixed ({wrapped}, {large}, {target})"
        );
        assert!(
            app.animations.cursor_smear_active(
                app.tabs[0].editor.navigation_id,
                &app.animation_settings,
                std::time::Instant::now(),
            ),
            "the visible cursor move creates smear motion ({wrapped}, {large}, {target})"
        );
        app.cursor_visible = false;
        let during_blink = render(&mut app, width, 24);
        app.animation_settings
            .set(AnimationEffect::CursorSmear, false);
        let uncovered = render(&mut app, width, 24);
        app.cursor_visible = true;
        let plain = render(&mut app, width, 24);
        let target_caret = plain
            .content
            .iter()
            .position(|cell| cell.bg == app.colors.bg_cursor)
            .expect("target caret");
        assert_ne!(old_caret, target_caret);
        assert_eq!(
            moving.content[old_caret].bg, app.colors.bg_cursor,
            "animated caret starts at its old position ({wrapped}, {large}, {target})"
        );
        assert_ne!(
            moving.content[target_caret].bg, app.colors.bg_cursor,
            "destination caret is hidden ({wrapped}, {large}, {target})"
        );
        assert_eq!(
            moving.content[target_caret].bg, uncovered.content[target_caret].bg,
            "the hidden destination retains its actual gradient ({wrapped}, {large}, {target})"
        );
        assert_ne!(
            during_blink.content[old_caret].bg, uncovered.content[old_caret].bg,
            "blinking must not remove a live smear ({wrapped}, {large}, {target})"
        );
        assert_eq!(app.tabs[0].editor.rope.to_string(), "x".repeat(80));
        assert_eq!(app.tabs[0].editor.cursor_col, target);
    }
}

#[test]
fn cursor_effects_animate_the_first_move_from_eol_in_every_editor_path() {
    use std::time::{Duration, Instant};

    for effect in [AnimationEffect::CursorFire, AnimationEffect::CursorSmear] {
        for (width, wrapped, large) in [(120, false, false), (45, true, false), (100, false, true)]
        {
            for line in [
                "last character".to_owned(),
                "\tending with 界".to_owned(),
                "x".repeat(70),
            ] {
                let source = format!("\n\n{line}\n\nnext line\n");
                let mut app = fixture(&source);
                app.show_minimap = false;
                app.tabs[0].editor.word_wrap = wrapped;
                app.tabs[0].editor.large_file_mode = large;
                app.tabs[0].editor.cursor_line = 2;
                let end = app.tabs[0].editor.line_len(2);
                app.tabs[0].editor.cursor_col = end;
                app.rulers = vec![end as u16];
                app.animation_settings.set(effect, true);
                render(&mut app, width, 24);
                let viewport = app.code_area;
                let probe = Instant::now();
                app.tabs[0].editor.cursor_col = end - 1;
                render(&mut app, width, 24);
                assert_eq!(app.code_area, viewport);
                let id = app.tabs[0].editor.navigation_id;
                assert!(
                    app.animations
                        .cursor_smear_active(id, &app.animation_settings, probe),
                    "the first move from EOL must animate ({effect:?}, {wrapped}, {large}, {line:?})"
                );
                if effect == AnimationEffect::CursorFire {
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
                        "the first move from EOL must emit particles"
                    );
                }
                assert_eq!(app.tabs[0].editor.rope.to_string(), source);
            }
            let mut app = fixture("previous\n\nnext\n");
            app.show_minimap = false;
            app.tabs[0].editor.word_wrap = wrapped;
            app.tabs[0].editor.large_file_mode = large;
            app.tabs[0].editor.cursor_line = 1;
            app.animation_settings.set(effect, true);
            render(&mut app, width, 24);
            let probe = Instant::now();
            app.tabs[0].editor.cursor_line = 2;
            render(&mut app, width, 24);
            assert!(
                app.animations.cursor_smear_active(
                    app.tabs[0].editor.navigation_id,
                    &app.animation_settings,
                    probe
                ),
                "leaving an empty line must animate ({effect:?}, {wrapped}, {large})"
            );
        }
    }
}

fn fire_navigation_case(width: u16, wrapped: bool, large: bool, target: usize, selection: bool) {
    use ratatui::style::Color;
    use std::time::{Duration, Instant};

    let text = format!(
        "{}\n    selected region stays legible\n\t界 unicode text\n    extra marker Z\n",
        "x".repeat(80)
    );
    let mut app = fixture(&text);
    app.show_minimap = false;
    app.rulers = vec![80];
    app.tabs[0].editor.word_wrap = wrapped;
    app.tabs[0].editor.large_file_mode = large;
    app.colors.gradients.bg_editor = crate::app_theme::ThemeGradientSpec {
        start: Color::Rgb(12, 18, 34),
        end: Color::Rgb(42, 56, 78),
        angle_deg: 45.0,
    };
    app.colors.gradients.bg_line_highlight = crate::app_theme::ThemeGradientSpec {
        start: Color::Rgb(24, 34, 48),
        end: Color::Rgb(54, 64, 88),
        angle_deg: 20.0,
    };
    app.animation_settings
        .set(AnimationEffect::CursorFire, true);
    app.animation_settings.duration_ms = 1500;
    assert!(!app.animation_settings.enabled(AnimationEffect::CursorSmear));

    render(&mut app, width, 24);
    let initial_area = app.code_area;
    assert!(
        app.animations
            .next_frame_wait(&app.animation_settings, Instant::now())
            .is_none(),
        "fire must not animate an untouched cursor ({wrapped}, {large}, {target})"
    );

    // Probe from before the render: saturating elapsed time makes these motion
    // assertions independent of machine speed, with no sleeps or frame races.
    let probe = Instant::now();
    app.tabs[0].editor.cursor_col = target;
    if selection {
        app.tabs[0].editor.selection_anchor = Some((1, 4));
    }
    let editor = &app.tabs[0].editor;
    let selected = editor.selection_range();
    assert_eq!(selected.is_some(), selection);
    let version = editor.content_version;
    let view_id = editor.navigation_id;
    let moving = render(&mut app, width, 24);
    assert_eq!(
        app.code_area, initial_area,
        "the test keeps the viewport fixed ({wrapped}, {large}, {target})"
    );
    let code_area = app.code_area.expect("the editor code viewport is visible");
    assert!(
        app.animations
            .cursor_smear_active(view_id, &app.animation_settings, probe),
        "Fire Hazard supplies its own animated cursor without Cursor Smear ({wrapped}, {large}, {target})"
    );
    assert!(
        app.animations
            .next_frame_wait(&app.animation_settings, probe)
            .is_some(),
        "cursor fire has a live redraw deadline ({wrapped}, {large}, {target})"
    );

    app.animation_settings
        .set(AnimationEffect::CursorFire, false);
    let disabled = render(&mut app, width, 24);
    assert!(
        app.animations
            .next_frame_wait(&app.animation_settings, probe)
            .is_none(),
        "disabled fire stops requesting animation frames"
    );
    app.animations
        .advance_frame(&app.animation_settings, probe + Duration::from_secs(2));
    let plain = render(&mut app, width, 24);
    assert_eq!(
        disabled, plain,
        "disabling fire restores the exact ordinary frame before state cleanup ({wrapped}, {large}, {target})"
    );
    assert_ne!(
        moving, plain,
        "Fire Hazard alone visibly animates cursor navigation ({wrapped}, {large}, {target})"
    );
    for (animated, ordinary) in moving.content.iter().zip(&plain.content) {
        if ordinary.symbol() != " " {
            assert_eq!(
                animated.symbol(),
                ordinary.symbol(),
                "fire preserves source glyphs, selection text and EOL rulers ({wrapped}, {large}, {target})"
            );
        }
    }
    for y in plain.area.top()..plain.area.bottom() {
        for x in plain.area.left()..plain.area.right() {
            if !code_area.contains((x, y).into()) {
                assert_eq!(
                    moving[(x, y)],
                    plain[(x, y)],
                    "fire stays inside the code viewport ({wrapped}, {large}, {target})"
                );
            }
        }
    }
    let editor = &app.tabs[0].editor;
    assert_eq!(editor.rope.to_string(), text);
    assert_eq!((editor.cursor_line, editor.cursor_col), (0, target));
    assert_eq!(editor.selection_range(), selected);
    assert_eq!(editor.content_version, version);
}

#[test]
fn fire_hazard_alone_preserves_selection_in_every_editor_path() {
    for (width, wrapped, large, target) in [
        (120, false, false, 20),
        (45, true, false, 48),
        (100, false, true, 20),
    ] {
        fire_navigation_case(width, wrapped, large, target, true);
    }
}

#[test]
fn fire_hazard_alone_preserves_eol_rulers_and_disables_cleanly_in_every_editor_path() {
    for (width, wrapped, large) in [(120, false, false), (45, true, false), (100, false, true)] {
        fire_navigation_case(width, wrapped, large, 80, false);
    }
}

#[test]
fn scope_bubble_keeps_rounded_contours_after_settling_without_idle_redraws() {
    use std::time::{Duration, Instant};

    let source = format!(
        "    if ready\n        call {}\n        call second\n    endif\noutside\n",
        "first value ".repeat(8)
    );
    for (width, wrapped, large) in [(120, false, false), (45, true, false), (100, false, true)] {
        let mut app = fixture(&source);
        app.show_minimap = false;
        app.tabs[0].editor.file_path = Some("scope.vim".into());
        app.tabs[0].editor.word_wrap = wrapped;
        app.tabs[0].editor.large_file_mode = large;
        app.tabs[0].editor.cursor_line = 1;
        app.tabs[0].editor.cursor_col = 8;
        app.animation_settings
            .set(AnimationEffect::ScopeConnector, true);
        render(&mut app, width, 32);
        assert!(
            app.animations
                .next_frame_wait(&app.animation_settings, Instant::now())
                .is_some(),
            "entering a scope animates its connector ({wrapped}, {large})"
        );

        // Expire the finite entrance without sleeping. Scope membership must
        // still produce the settled guide in the real editor draw path.
        let settled_at = Instant::now() + Duration::from_secs(2);
        assert!(
            app.animations
                .advance_frame(&app.animation_settings, settled_at)
        );
        let mut settled_frames = Vec::new();
        for line in [1, 2] {
            app.tabs[0].editor.cursor_line = line;
            let settled = render(&mut app, width, 32);
            let corners: Vec<_> = settled
                .content
                .iter()
                .enumerate()
                .filter_map(|(index, cell)| {
                    matches!(cell.symbol(), "╭" | "╰" | "╮" | "╯").then_some(index)
                })
                .collect();
            assert!(
                corners.len() >= 2,
                "the contour keeps its curves after entrance and movement ({wrapped}, {large}, {line})"
            );
            for &corner in &corners {
                let cell = &settled.content[corner];
                let contrast = crate::ui_colors::contrast_ratio(cell.fg, cell.bg)
                    .expect("settled connector curves have resolved terminal colors");
                assert!(
                    contrast >= 2.0,
                    "settled curves stay legible at default intensity ({wrapped}, {large}, {line}): {contrast:.3}"
                );
            }
            assert!(
                settled
                    .content
                    .iter()
                    .all(|cell| !matches!(cell.symbol(), "├" | "┤" | "┬" | "┴" | "┼")),
                "the bubble has smooth corners rather than overlapping junctions"
            );
            assert!(
                app.animations
                    .next_frame_wait(&app.animation_settings, settled_at)
                    .is_none(),
                "the persistent connector needs no idle animation frames ({wrapped}, {large}, {line})"
            );

            assert_eq!(app.tabs[0].editor.rope.to_string(), source);
            settled_frames.push((settled, corners));
        }

        app.animation_settings
            .set(AnimationEffect::ScopeConnector, false);
        let disabled = render(&mut app, width, 32);
        let (settled, corners) = settled_frames.last().unwrap();
        for &corner in corners {
            assert_ne!(
                settled.content[corner], disabled.content[corner],
                "turning the connector off restores each endpoint cell"
            );
        }
        assert!(
            disabled
                .content
                .iter()
                .all(|cell| !matches!(cell.symbol(), "╭" | "╰" | "╮" | "╯")),
            "disabled scope decoration leaves no curves behind"
        );
        app.animations
            .advance_frame(&app.animation_settings, settled_at);
        assert_eq!(
            disabled,
            render(&mut app, width, 32),
            "disabling restores the ordinary frame immediately ({wrapped}, {large})"
        );
    }
}

#[test]
fn scope_connector_identifies_the_active_pair_when_braces_share_a_row() {
    use std::time::{Duration, Instant};
    let source = "    { { {\n        work();\n    } } }\n";
    for (width, wrapped, large) in [(100, false, false), (35, true, false), (100, false, true)] {
        let mut app = fixture(source);
        app.show_minimap = false;
        app.tabs[0].editor.file_path = Some("shared-row.rs".into());
        app.tabs[0].editor.word_wrap = wrapped;
        app.tabs[0].editor.large_file_mode = large;
        app.tabs[0].editor.cursor_line = 1;
        app.tabs[0].editor.cursor_col = 8;
        app.animation_settings
            .set(AnimationEffect::ScopeConnector, true);
        render(&mut app, width, 16);
        let settled_at = Instant::now() + Duration::from_secs(2);
        app.animations
            .advance_frame(&app.animation_settings, settled_at);
        let settled = render(&mut app, width, 16);
        let settled_code = app.code_area.unwrap();
        app.animation_settings
            .set(AnimationEffect::ScopeConnector, false);
        let plain = render(&mut app, width, 16);
        let code_shift = settled_code.x - app.code_area.unwrap().x;
        for (delimiter, active_index) in [("{", 2), ("}", 0)] {
            let positions: Vec<_> = plain
                .content
                .iter()
                .enumerate()
                .filter_map(|(index, cell)| (cell.symbol() == delimiter).then_some(index))
                .collect();
            assert_eq!(positions.len(), 3);
            for (index, position) in positions.into_iter().enumerate() {
                let animated_position = position + code_shift as usize;
                assert_eq!(settled.content[animated_position].symbol(), delimiter);
                if index == active_index {
                    assert_ne!(
                        settled.content[animated_position].bg,
                        plain.content[position].bg
                    );
                } else {
                    assert_eq!(settled.content[animated_position], plain.content[position]);
                }
            }
        }
        assert_eq!(app.tabs[0].editor.rope.to_string(), source);
    }
}

#[test]
fn scope_connector_routes_multiline_signatures_to_the_braces_through_empty_cells() {
    use std::time::{Duration, Instant};

    // The first row used to attach to `)`. Splitting the return type onto
    // another row changed the apparent attachment to the `b` in `bool`.
    // Blank body rows give the route room to turn between source glyphs.
    for (signature, body) in [
        ("    ) -> bool {", "        code();\n"),
        ("    ) -> bool {", "\n        code();\n"),
        ("    ) ->\n        bool {", "\n\n        code();\n"),
        (
            "    ) -> bool {",
            "        wide_expression();\n        code();\n",
        ),
    ] {
        let source = format!(
            "impl Example {{\n    fn accepts(\n        &self,\n        input: Option<Input>,\n{signature}\n{body}    }}\n}}\n"
        );
        let body_line = source
            .lines()
            .position(|line| line.contains("code();"))
            .unwrap();
        let opening_line = source
            .lines()
            .position(|line| line.contains("bool {"))
            .unwrap();
        let opening_col = source
            .lines()
            .nth(opening_line)
            .unwrap()
            .chars()
            .position(|ch| ch == '{')
            .unwrap();
        let pair = ((opening_line, opening_col), (body_line + 1, 4));

        for (width, wrapped, large) in [(100, false, false), (45, true, false), (100, false, true)]
        {
            let context =
                format!("signature={signature:?}, body={body:?}, wrap={wrapped}, large={large}");
            let mut app = indexed_scope_fixture(&source, wrapped, false);
            app.cursor_visible = false;
            app.tabs[0].editor.large_file_mode = large;
            app.tabs[0].editor.cursor_line = body_line;
            app.tabs[0].editor.cursor_col = 8;
            assert_eq!(
                app.cached_structural_scope_index
                    .as_ref()
                    .unwrap()
                    .enclosing_scope(&app.tabs[0].editor),
                Some(pair),
                "the fixture selects the function body: {context}"
            );
            let intensity = app.animation_settings.intensity;
            app.animation_settings.intensity = 0;
            let plain = render(&mut app, width, 18);
            app.animation_settings.intensity = intensity;
            render(&mut app, width, 18);
            app.animations.advance_frame(
                &app.animation_settings,
                Instant::now() + Duration::from_secs(2),
            );
            let connected = render(&mut app, width, 18);
            assert_scope_route_source_safety(&mut app, &connected, &plain, pair, &context, true);
            assert_eq!(app.tabs[0].editor.rope.to_string(), source);
        }
    }
}

#[test]
fn scope_connector_and_brace_colors_stay_fixed_while_scrolling() {
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };

    let source = format!(
        "fn outer() {{\n{}    match key.code {{\n{}    }}\n}}\n",
        "\n".repeat(300),
        "        work(); // a wide body row with more text for wrapping\n".repeat(24)
    );
    let opening = 301;
    let closing = opening + 25;
    for (width, wrapped, colorize) in [
        (100, false, true),
        (45, true, true),
        (100, false, false),
        (45, true, false),
    ] {
        let mut app = fixture(&source);
        app.show_minimap = false;
        app.show_breadcrumbs = false;
        app.show_indent_guides = false;
        app.bracket_pair_colorization = colorize;
        app.rulers.clear();
        app.sticky_scroll = false;
        app.colors.gradients.bg_editor = crate::app_theme::ThemeGradientSpec {
            start: app.colors.bg_editor,
            end: app.colors.bg_editor,
            angle_deg: 0.0,
        };
        app.colors.bg_line_highlight = app.colors.bg_editor;
        app.colors.gradients.bg_line_highlight = app.colors.gradients.bg_editor;
        app.animation_settings
            .set(AnimationEffect::ScopeConnector, true);
        let editor = &mut app.tabs[0].editor;
        editor.file_path = Some("scrolling-scope.rs".into());
        editor.word_wrap = wrapped;
        editor.cursor_line = opening + 2;
        editor.cursor_col = 8;
        editor.scroll_offset = opening - 1;
        editor.wrap_scroll_offset = opening - 1;
        app.cached_bracket_depth_index =
            Some(Arc::new(crate::background::BracketDepthIndex::build(
                editor.rope.clone(),
                editor.navigation_id,
                editor.content_version,
                editor.wrap_revision,
            )));
        // Leave the viewport analysis anchored at the original scroll position
        // throughout, as happens while a coalesced background job is pending.
        app.cached_bracket_analysis_target = Some(crate::background::BracketAnalysisTarget {
            editor: app.editor_analysis_target().unwrap(),
            navigation_id: app.tabs[0].editor.navigation_id,
            first_line: opening - 1,
            visible_lines: 50,
        });
        app.cached_bracket_starting_depth = 1;
        render(&mut app, width, 18);
        app.animations.advance_frame(
            &app.animation_settings,
            Instant::now() + Duration::from_secs(2),
        );
        let mut reference = None;
        for (scroll, wrap_scroll, select_endpoints) in [
            (opening - 1, 0, false),
            (opening, 0, false),
            (opening + 1, 0, false),
            (opening + 12, 0, false),
            (opening + 2, usize::from(wrapped), false),
            (closing - 1, 0, false),
            (closing - 1, 0, true),
            (opening - 1, 0, false),
        ] {
            let editor = &mut app.tabs[0].editor;
            if wrapped {
                let row = editor.wrapped_row_for_position(scroll, 0, editor.viewport_width);
                editor.set_wrap_scroll_offset(row + wrap_scroll);
            } else {
                editor.scroll_offset = scroll;
            }
            editor.selection_anchor = None;
            editor.cursor_line = opening + 2;
            editor.cursor_col = 8;
            if select_endpoints {
                // Extra carets at the endpoints suspend the scope. Returning
                // to one caret must restore its original nesting color.
                let opener = editor
                    .rope
                    .line(opening)
                    .chars()
                    .position(|ch| ch == '{')
                    .unwrap();
                editor.extra_cursors = vec![(opening, opener + 1), (closing, 4)];
                editor.extra_cursor_anchors = vec![Some((opening, opener)), None];
            } else {
                editor.extra_cursors.clear();
                editor.extra_cursor_anchors.clear();
            }
            render(&mut app, width, 18);
            if select_endpoints {
                assert!(app.animations.take_events(app.tabs[0].editor.navigation_id).is_empty(),
                    "multiple cursors suspend the scope until the next single-cursor frame");
                continue;
            }
            app.animations.advance_frame(
                &app.animation_settings,
                Instant::now() + Duration::from_secs(2),
            );
            let buffer = render(&mut app, width, 18);
            let code = app.code_area.unwrap();
            let rails: Vec<_> = buffer
                .content
                .iter()
                .enumerate()
                .filter(|(index, cell)| {
                    cell.symbol() == "│"
                        && (*index % width as usize) >= code.x.saturating_sub(1) as usize
                        && (*index % width as usize) < code.right() as usize
                })
                .map(|(_, cell)| cell.fg)
                .collect();
            assert!(
                !rails.is_empty(),
                "missing scope rail: wrapped={wrapped}, scroll={scroll}"
            );
            let color = *reference.get_or_insert(rails[0]);
            assert!(
                rails.iter().all(|&fg| fg == color),
                "scope hue changed: wrapped={wrapped}, scroll={scroll}, selected={select_endpoints}, colors={rails:?}"
            );
            if !select_endpoints && colorize {
                for row in code.y..code.bottom() {
                    let cell = &buffer[(code.x + 4, row)];
                    if matches!(cell.symbol(), "{" | "}") {
                        assert_eq!(
                            cell.fg,
                            app.colors.ui.bracket_color(1).unwrap(),
                            "brace nesting changed: wrapped={wrapped}, scroll={scroll}"
                        );
                    }
                }
            }
        }
        assert_eq!(app.tabs[0].editor.rope.to_string(), source);
    }
}

#[test]
fn scope_bubble_follows_visible_body_when_both_endpoints_leave_the_viewport() {
    use std::time::{Duration, Instant};

    fn rail_columns(
        source: &str,
        wrapped: bool,
        width: u16,
        height: u16,
        scroll: usize,
        pan: usize,
        cursor: (usize, usize),
    ) -> (Vec<i32>, [bool; 2]) {
        fn draw(app: &mut App, width: u16, height: u16) -> Buffer {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| {
                    crate::ui_editor::draw_editor_content(
                        frame,
                        app,
                        ratatui::layout::Rect::new(0, 0, width, height),
                    )
                })
                .unwrap();
            terminal.backend().buffer().clone()
        }
        let mut app = fixture(source);
        app.show_minimap = false;
        app.show_breadcrumbs = false;
        app.show_indent_guides = false;
        app.rulers.clear();
        app.sticky_scroll = false;
        app.animation_settings
            .set(AnimationEffect::ScopeConnector, true);
        let editor = &mut app.tabs[0].editor;
        editor.file_path = Some("clipped-scope.rs".into());
        editor.word_wrap = wrapped;
        editor.cursor_line = cursor.0;
        editor.cursor_col = cursor.1;
        editor.scroll_offset = scroll;
        editor.wrap_scroll_offset = scroll;
        editor.horizontal_scroll = pan;
        // Start directly in the clipped viewport, without remembering a
        // previous frame in which the entire scope was visible.
        draw(&mut app, width, height);
        if wrapped {
            let editor = &mut app.tabs[0].editor;
            let row = editor.wrapped_row_for_position(scroll, 0, editor.viewport_width);
            editor.set_wrap_scroll_offset(row);
        }
        app.animations.advance_frame(
            &app.animation_settings,
            Instant::now() + Duration::from_secs(2),
        );
        let buffer = draw(&mut app, width, height);
        let code = app.code_area.unwrap();
        let pair = app.animations.scope(&app.tabs[0].editor, None).unwrap();
        let visible = [pair.0, pair.1]
            .map(|position| scope_endpoint_screen_position(&mut app, &buffer, position).is_some());
        let intensity = app.animation_settings.intensity;
        app.animation_settings.intensity = 0;
        let plain = draw(&mut app, width, height);
        app.animation_settings.intensity = intensity;
        assert_scope_route_source_safety(
            &mut app,
            &buffer,
            &plain,
            pair,
            &format!("wrap={wrapped}, width={width}, scroll={scroll}, pan={pan}"),
            false,
        );
        let mut columns: Vec<_> = buffer
            .content
            .iter()
            .enumerate()
            .filter(|(index, cell)| {
                cell.symbol() == "│"
                    && (*index / width as usize) >= code.y as usize
                    && (*index / width as usize) < code.bottom() as usize
                    && (*index % width as usize) >= code.x.saturating_sub(1) as usize
                    && (*index % width as usize) < code.right() as usize
            })
            .map(|(index, _)| (index % width as usize) as i32 - code.x as i32)
            .collect();
        columns.sort_unstable();
        columns.dedup();
        assert!(
            !columns.is_empty(),
            "missing rail: wrap={wrapped}, width={width}, scroll={scroll}, pan={pan}"
        );
        (columns, visible)
    }

    for indent in ["    ", "\t"] {
        let source = format!(
            "fn outer() {{\n{indent}if ready {{\n{}{indent}}}\n}}\n",
            format!("{indent}{indent}work();\n").repeat(18)
        );
        for wrapped in [false, true] {
            for (height, scroll) in [(30, 0), (8, 0), (8, 2), (8, 8), (8, 17)] {
                let (columns, visible) = rail_columns(
                    &source,
                    wrapped,
                    75,
                    height,
                    scroll,
                    0,
                    (2, indent.chars().count() * 2),
                );
                // A body-only viewport follows the visible text envelope,
                // including both sides, without an earlier full frame.
                if !visible[0] && !visible[1] {
                    assert_eq!(
                        columns.first(),
                        Some(&7),
                        "left side does not hug the visible body: indent={indent:?}, wrap={wrapped}, scroll={scroll}"
                    );
                    assert!(columns.last() > columns.first());
                }
            }
        }
        for scroll in [2, 8] {
            let expected = if indent == "\t" { 3 } else { 6 };
            let (columns, visible) = rail_columns(
                &source,
                false,
                75,
                8,
                scroll,
                1,
                (2, indent.chars().count() * 2),
            );
            assert_eq!(visible, [false, false]);
            assert_eq!(
                columns.first(),
                Some(&expected),
                "clipped source column moved after panning: indent={indent:?}, scroll={scroll}"
            );
        }
    }

    let source = format!(
        "fn outer() {{\n    if ready && a_long_condition && another_long_condition {{\n{}    }}\n}}\n",
        "        work();\n".repeat(18)
    );
    for wrapped in [false, true] {
        for (height, scroll) in [(30, 0), (8, 0), (8, 2), (8, 8), (8, 17)] {
            let (columns, visible) = rail_columns(&source, wrapped, 45, height, scroll, 0, (2, 8));
            if !visible[0] && !visible[1] {
                assert_eq!(
                    columns.first(),
                    Some(&7),
                    "clipped long opener jumped: wrap={wrapped}, scroll={scroll}"
                );
            }
        }
    }

    // The screenshot's let-else brace is outside the horizontal viewport,
    // although the start of that source row and its closing brace are visible.
    let source = "impl App {\n    fn key(&mut self) {\n        let Some(ChatAction::FocusTerminal(id)) = self.chat.focus.clone() else {\n            return false;\n        };\n        true\n    }\n}\n";
    for (width, pan) in [(120, 0), (60, 0), (60, 3), (60, 6)] {
        let (columns, visible) = rail_columns(source, false, width, 12, 0, pan, (3, 12));
        if !visible[0] {
            assert_eq!(
                columns.first(),
                Some(&(7 - pan as i32)),
                "horizontal clipping moved the let-else rail: width={width}, pan={pan}"
            );
        }
    }
}

fn indexed_scope_fixture(source: &str, wrapped: bool, colorize: bool) -> App {
    use std::sync::Arc;

    let mut app = fixture(source);
    app.show_activity_bar = false;
    app.show_menu_bar = false;
    app.show_tab_bar = false;
    app.show_breadcrumbs = false;
    app.show_minimap = false;
    app.show_indent_guides = false;
    app.sticky_scroll = false;
    app.rulers.clear();
    app.bracket_pair_colorization = colorize;
    app.animation_settings
        .set(AnimationEffect::ScopeConnector, true);
    let editor = &mut app.tabs[0].editor;
    editor.file_path = Some("deep-scope.rs".into());
    editor.language_override = Some("Rust".into());
    editor.word_wrap = wrapped;
    let request = crate::animation_structural_scope::StructuralScopeRequest::from_editor(editor)
        .expect("the Rust fixture supports structural scope discovery");
    app.cached_structural_scope_index = Some(Arc::new(
        request
            .build(None, || false)
            .expect("the synchronous fixture is never cancelled"),
    ));
    app
}

fn scope_rail_connections(symbol: &str) -> u8 {
    match symbol {
        "╭" => 2 | 4,
        "╮" => 2 | 8,
        "╰" => 1 | 4,
        "╯" => 1 | 8,
        "─" => 4 | 8,
        "│" => 1 | 2,
        "╷" => 2,
        "╵" => 1,
        "╶" => 4,
        "╴" => 8,
        "├" => 1 | 2 | 4,
        "┤" => 1 | 2 | 8,
        "┬" => 2 | 4 | 8,
        "┴" => 1 | 4 | 8,
        "┼" => 1 | 2 | 4 | 8,
        _ => 0,
    }
}

fn scope_endpoint_screen_position(
    app: &mut App,
    buffer: &Buffer,
    position: (usize, usize),
) -> Option<(u16, u16)> {
    let code = app.code_area.unwrap();
    let editor = &mut app.tabs[0].editor;
    let text = editor.rope.line(position.0).to_string();
    let chars: Vec<_> = text.trim_end_matches(['\r', '\n']).chars().collect();
    let (row, start) = if editor.word_wrap && !editor.large_file_mode {
        let row = editor.wrapped_row_for_position(position.0, position.1, editor.viewport_width);
        let ranges = Editor::wrap_ranges_for_line_text_tabbed(
            text.trim_end_matches(['\r', '\n']),
            editor.viewport_width,
            editor.tab_size,
        );
        let &(start, _) = ranges
            .iter()
            .find(|&&(start, end)| position.1 >= start && position.1 < end)?;
        (row.checked_sub(editor.wrap_scroll_offset)?, start)
    } else {
        (
            position.0.checked_sub(editor.scroll_offset)?,
            editor.horizontal_scroll,
        )
    };
    if row >= code.height as usize || position.1 < start {
        return None;
    }
    let mut columns = vec![0usize];
    let mut tab_column = 0usize;
    for &ch in &chars {
        let width = if ch == '\t' {
            editor.tab_size.max(1) - tab_column % editor.tab_size.max(1)
        } else {
            unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0)
        };
        tab_column += if ch == '\t' { width } else { 1 };
        columns.push(columns.last().unwrap() + width);
    }
    let x = code
        .x
        .checked_add(u16::try_from(columns[position.1] - columns[start]).ok()?)?;
    let y = code.y + row as u16;
    (x < code.right() && buffer[(x, y)].symbol() == chars[position.1].to_string()).then_some((x, y))
}

fn assert_scope_route_source_safety(
    app: &mut App,
    connected: &Buffer,
    plain: &Buffer,
    pair: ((usize, usize), (usize, usize)),
    context: &str,
    require_both: bool,
) {
    use std::collections::{HashMap, HashSet, VecDeque};

    let code = app.code_area.unwrap();
    let boundary_endpoints =
        [pair.0, pair.1].map(|position| scope_endpoint_screen_position(app, plain, position));
    let endpoints: Vec<_> = boundary_endpoints.iter().flatten().copied().collect();
    let route: HashMap<_, _> = scope_rail_cells(app, connected)
        .into_iter()
        .filter(|&(x, y, _)| plain[(x, y)].symbol().trim().is_empty())
        .map(|(x, y, symbol)| {
            (
                (i32::from(x), i32::from(y)),
                scope_rail_connections(&symbol),
            )
        })
        .collect();
    assert!(!route.is_empty(), "missing connector: {context}");
    for y in code.y..code.bottom() {
        for x in code.x.saturating_sub(1)..code.right() {
            let symbol = plain[(x, y)].symbol();
            if symbol != " " {
                assert_eq!(
                    connected[(x, y)].symbol(),
                    symbol,
                    "source glyph changed at ({x}, {y}): {context}"
                );
            }
            assert!(
                !connected[(x, y)]
                    .modifier
                    .contains(ratatui::style::Modifier::UNDERLINED)
                    || plain[(x, y)]
                        .modifier
                        .contains(ratatui::style::Modifier::UNDERLINED),
                "scope added an underline at ({x}, {y}): {context}"
            );
            if route.contains_key(&(i32::from(x), i32::from(y))) {
                assert_eq!(
                    symbol, " ",
                    "route replaced source at ({x}, {y}): {context}"
                );
            }
        }
    }
    let first_row = route.keys().map(|&(_, y)| y).min().unwrap();
    let last_row = route.keys().map(|&(_, y)| y).max().unwrap();
    let source_cell = |x: i32, y: i32| {
        x >= i32::from(code.x)
            && x < i32::from(code.right())
            && y >= i32::from(code.y)
            && y < i32::from(code.bottom())
            && (!plain[(x as u16, y as u16)].symbol().trim().is_empty()
                || (1..=2).any(|offset| {
                    let previous = x - offset;
                    previous >= i32::from(code.x)
                        && unicode_width::UnicodeWidthStr::width(
                            plain[(previous as u16, y as u16)].symbol(),
                        ) > offset as usize
                }))
    };
    let directions = [(1, 2, 0, -1), (2, 1, 0, 1), (4, 8, 1, 0), (8, 4, -1, 0)];
    for (&(x, y), &ports) in &route {
        assert!(
            (1..=2).contains(&ports.count_ones()),
            "a contour junction must be a line or a rounded corner: {context}"
        );
        for &(direction, opposite, dx, dy) in &directions {
            if ports & direction == 0 {
                continue;
            }
            let next = (x + dx, y + dy);
            if route.get(&next).is_some_and(|ports| ports & opposite != 0) {
                continue;
            }
            let next_is_source = source_cell(next.0, next.1);
            // A bubble can stay open at a clipped edge or an occupied cap.
            // It must never resume across a blank cell in its middle.
            assert!(
                next_is_source
                    || direction == 1 && y == first_row
                    || direction == 2 && y == last_row
                    || next.0 < i32::from(code.x.saturating_sub(1))
                    || next.0 >= i32::from(code.right()),
                "contour has a gap at ({x}, {y}) -> {next:?}: {context}"
            );
        }
    }
    if require_both {
        assert_eq!(endpoints.len(), 2, "both braces must be visible: {context}");
        let min_x = route.keys().map(|&(x, _)| x).min().unwrap();
        let max_x = route.keys().map(|&(x, _)| x).max().unwrap();
        assert!(
            endpoints
                .iter()
                .all(|&(x, _)| min_x < i32::from(x) && max_x > i32::from(x)),
            "the bubble must surround the selected scope on both sides: {context}"
        );
    }
    let mut remaining: HashSet<_> = route.keys().copied().collect();
    let mut components = 0;
    let mut clipped_components = 0;
    while let Some(&start) = remaining.iter().next() {
        remaining.remove(&start);
        let mut pending = VecDeque::from([start]);
        let mut component = Vec::new();
        while let Some((x, y)) = pending.pop_front() {
            component.push((x, y));
            for &(direction, opposite, dx, dy) in &directions {
                let next = (x + dx, y + dy);
                if route[&(x, y)] & direction != 0
                    && route.get(&next).is_some_and(|ports| ports & opposite != 0)
                    && remaining.remove(&next)
                {
                    pending.push_back(next);
                }
            }
        }
        // Source glyphs complete the heading baseline. Only source-bounded
        // horizontal runs on a visible scope boundary may be separate from
        // the two exterior sides; body-row fragments still count as gaps.
        let y = start.1;
        let visible_boundary = y == first_row && boundary_endpoints[0].is_some()
            || y == last_row && boundary_endpoints[1].is_some();
        let horizontal = component
            .iter()
            .all(|&(x, row)| row == y && route[&(x, row)] == (4 | 8));
        let left = component.iter().map(|&(x, _)| x).min().unwrap();
        let right = component.iter().map(|&(x, _)| x).max().unwrap();
        let source_bounded_gap =
            visible_boundary && horizontal && source_cell(left - 1, y) && source_cell(right + 1, y);
        if !source_bounded_gap {
            components += 1;
            // Cropping a source contour can expose several pieces of one
            // side. Count only a real outward port as proof of clipping;
            // unexplained interior fragments still fail the continuity check.
            clipped_components += usize::from(component.iter().any(|&(x, y)| {
                let ports = route[&(x, y)];
                x == i32::from(code.x.saturating_sub(1)) && ports & 8 != 0
                    || x + 1 == i32::from(code.right()) && ports & 4 != 0
                    || y == i32::from(code.y) && ports & 1 != 0
                    || y + 1 == i32::from(code.bottom()) && ports & 2 != 0
            }));
        }
    }
    let clear_exterior = (first_row..=last_row).all(|y| {
        let occupied: Vec<_> = (code.x..code.right())
            .filter(|&x| !plain[(x, y as u16)].symbol().trim().is_empty())
            .collect();
        occupied.last().is_none_or(|&x| x + 1 < code.right())
    });
    if clear_exterior {
        assert!(
            components - clipped_components <= 2,
            "an unobstructed bubble has at most two uncut sides: {context}"
        );
    }
}

fn scope_rail_cells(app: &App, buffer: &Buffer) -> Vec<(u16, u16, String)> {
    let code = app.code_area.expect("real editor code area");
    let width = buffer.area.width as usize;
    buffer
        .content
        .iter()
        .enumerate()
        .filter_map(|(index, cell)| {
            let x = (index % width) as u16;
            let y = (index / width) as u16;
            (x >= code.x.saturating_sub(1)
                && x < code.right()
                && y >= code.y
                && y < code.bottom()
                && scope_rail_connections(cell.symbol()) != 0)
                .then(|| (x, y, cell.symbol().to_owned()))
        })
        .collect()
}

fn place_indexed_scope_cursor(
    app: &mut App,
    position: (usize, usize),
    first_line: usize,
    width: u16,
) {
    let editor = &mut app.tabs[0].editor;
    (editor.cursor_line, editor.cursor_col) = position;
    editor.scroll_offset = first_line;
    editor.wrap_scroll_offset = first_line;
    let bracket = editor.find_matching_bracket();
    let tag = editor.find_matching_tag();
    app.cached_cursor_match_target = app.cursor_match_target();
    app.cached_matching_bracket = bracket;
    app.cached_matching_tag = tag;
    // Let the production draw establish the actual wrapped content width,
    // then project this logical viewport through the normal editor row API.
    render(app, width, 18);
    let editor = &mut app.tabs[0].editor;
    if editor.word_wrap {
        let row = editor.wrapped_row_for_position(first_line, 0, editor.viewport_width);
        editor.set_wrap_scroll_offset(row);
    }
}

fn assert_indexed_scope_membership(
    app: &mut App,
    positions: &[(usize, usize)],
    pair: ((usize, usize), (usize, usize)),
    first_line: usize,
    width: u16,
) {
    use std::time::{Duration, Instant};

    let index = app.cached_structural_scope_index.clone().unwrap();
    let mut expected_rail = None;
    for &position in positions {
        place_indexed_scope_cursor(app, position, first_line, width);
        assert_eq!(index.enclosing_scope(&app.tabs[0].editor), Some(pair));
        app.animations.advance_frame(
            &app.animation_settings,
            Instant::now() + Duration::from_secs(2),
        );
        let buffer = render(app, width, 18);
        let rail = scope_rail_cells(app, &buffer);
        assert!(
            !rail.is_empty(),
            "missing deep scope connector at {position:?}"
        );
        let intensity = app.animation_settings.intensity;
        app.animation_settings.intensity = 0;
        let plain = render(app, width, 18);
        app.animation_settings.intensity = intensity;
        let context = format!(
            "deep scope at {position:?}, wrap={}, width={width}",
            app.tabs[0].editor.word_wrap
        );
        assert_scope_route_source_safety(app, &buffer, &plain, pair, &context, false);
        if let Some(expected) = &expected_rail {
            assert_eq!(
                &rail, expected,
                "moving inside the same deep scope changed its route at {position:?}"
            );
        } else {
            expected_rail = Some(rail);
        }
        assert!(
            app.animations
                .next_frame_wait(
                    &app.animation_settings,
                    Instant::now() + Duration::from_secs(2),
                )
                .is_none(),
            "settled deep scope continues requesting animation frames"
        );
    }
}

#[test]
fn scope_connector_persists_inside_blocks_beyond_old_line_and_character_limits() {
    use std::time::{Duration, Instant};

    for prefix in [
        "// padding keeps lexical state known before the deep block\n".repeat(600),
        format!("// {}\n", "wide prefix ".repeat(3_000)),
    ] {
        let opening = prefix.lines().count() + 1;
        let closing = opening + 3;
        let source = format!(
            "{prefix}fn outer() {{\n    if ready {{\n        first_with_a_long_argument(\"this body row wraps across several terminal rows\");\n        second();\n    }}\n}}\noutside();\n"
        );
        let pair = ((opening, 13), (closing, 4));
        assert!(source[..source.find("fn outer").unwrap()].chars().count() > 32 * 1024);
        // Bracket palette behavior has its own scrolling/color tests. These
        // cases cover both rendering paths and scope discovery with colors off.
        for (wrapped, colorize) in [(false, true), (true, false)] {
            let width = if wrapped { 45 } else { 100 };
            let mut app = indexed_scope_fixture(&source, wrapped, colorize);
            assert_indexed_scope_membership(
                &mut app,
                &[
                    (opening + 1, 8),
                    (opening + 2, 8),
                    pair.0,
                    pair.1,
                    (opening + 1, 10),
                ],
                pair,
                opening.saturating_sub(1),
                width,
            );
            let outside = (closing + 2, 0);
            place_indexed_scope_cursor(&mut app, outside, opening.saturating_sub(1), width);
            assert_eq!(
                app.cached_structural_scope_index
                    .as_ref()
                    .unwrap()
                    .enclosing_scope(&app.tabs[0].editor),
                None
            );
            app.animations.advance_frame(
                &app.animation_settings,
                Instant::now() + Duration::from_secs(2),
            );
            let outside_frame = render(&mut app, width, 18);
            assert!(
                scope_rail_cells(&app, &outside_frame).is_empty(),
                "scope connector remained outside its block: wrap={wrapped}, colorize={colorize}"
            );
            assert_eq!(app.tabs[0].editor.rope.to_string(), source);
        }
    }
}

#[test]
fn scope_connector_renders_app_git_branch_body_without_touching_a_brace_first() {
    let source = include_str!("app.rs");
    let lines: Vec<_> = source.lines().collect();
    let branch = lines
        .iter()
        .position(|line| line.contains("BackgroundResult::GitBranch(branch) => {"))
        .unwrap();
    let opening = branch
        + lines[branch..]
            .iter()
            .position(|line| line.trim() == "if self.bg_git_branch_pending {")
            .unwrap();
    let closing = opening
        + lines[opening + 1..]
            .iter()
            .position(|line| line.trim() == "}")
            .unwrap()
        + 1;
    let opening_col = lines[opening].chars().position(|ch| ch == '{').unwrap();
    let closing_col = lines[closing].chars().position(|ch| ch == '}').unwrap();
    let body_col = lines[opening + 1]
        .chars()
        .take_while(|ch| ch.is_whitespace())
        .count();
    assert!(
        opening > 1_700,
        "the regression must exercise the reported deep app.rs block"
    );
    let pair = ((opening, opening_col), (closing, closing_col));
    for (wrapped, colorize) in [(false, true), (true, true), (false, false), (true, false)] {
        let width = if wrapped { 45 } else { 140 };
        let mut app = indexed_scope_fixture(source, wrapped, colorize);
        assert_indexed_scope_membership(
            &mut app,
            &[
                (opening + 1, body_col),
                (opening + 2, body_col),
                pair.0,
                pair.1,
            ],
            pair,
            opening.saturating_sub(2),
            width,
        );
        assert_eq!(app.tabs[0].editor.rope.to_string(), source);
    }
}

#[test]
fn scope_connector_renders_body_after_a_multi_megabyte_comment_prefix() {
    // Keep the rendered viewport small while exercising structural discovery
    // beyond a document-size cutoff. The long line is only a source prefix.
    let source = format!(
        "// {}\nfn outer() {{\n    if ready {{\n        first();\n        second();\n    }}\n}}\n",
        "padding".repeat(600_000),
    );
    let mut app = indexed_scope_fixture(&source, false, false);
    assert!(app.tabs[0].editor.rope.line_to_char(3) > 4 * 1024 * 1024);
    assert_indexed_scope_membership(
        &mut app,
        &[(3, 8), (4, 8), (2, 13)],
        ((2, 13), (5, 4)),
        1,
        100,
    );
    assert_eq!(app.tabs[0].editor.rope.to_string(), source);
}

#[test]
fn scope_connector_appears_when_deep_index_arrives_without_cursor_or_source_changes() {
    use std::time::{Duration, Instant};

    let prefix = "// known lexical prefix before the pending scope\n".repeat(600);
    let source = format!(
        "{prefix}fn outer() {{\n    if ready {{\n        first_with_a_long_argument(\"this row wraps in the narrow viewport\");\n        second();\n    }}\n}}\n"
    );
    let opening = 601;
    for wrapped in [false, true] {
        let width = if wrapped { 45 } else { 100 };
        let mut app = indexed_scope_fixture(&source, wrapped, false);
        let arriving_index = app.cached_structural_scope_index.take().unwrap();
        place_indexed_scope_cursor(&mut app, (opening + 1, 8), opening - 1, width);
        let before = render(&mut app, width, 18);
        assert!(
            scope_rail_cells(&app, &before).is_empty(),
            "the fixture must first cache a missing deep scope"
        );
        assert!(app.cached_structural_scope_index.is_none());
        let editor = &app.tabs[0].editor;
        let source_snapshot = editor.rope.clone();
        let caret = (editor.cursor_line, editor.cursor_col);
        let version = editor.content_version;

        // Model a completed worker result at the same rendering anchor. A
        // cached None must not mask newly available lexical membership.
        app.cached_structural_scope_index = Some(arriving_index);
        render(&mut app, width, 18);
        app.animations.advance_frame(
            &app.animation_settings,
            Instant::now() + Duration::from_secs(2),
        );
        let after = render(&mut app, width, 18);
        let rail = scope_rail_cells(&app, &after);
        assert!(!rail.is_empty(), "arriving index must reveal the connector");
        let intensity = app.animation_settings.intensity;
        app.animation_settings.intensity = 0;
        let plain = render(&mut app, width, 18);
        app.animation_settings.intensity = intensity;
        assert_scope_route_source_safety(
            &mut app,
            &after,
            &plain,
            ((opening, 13), (opening + 3, 4)),
            &format!("arriving deep scope index, wrap={wrapped}"),
            false,
        );
        let editor = &app.tabs[0].editor;
        assert_eq!((editor.cursor_line, editor.cursor_col), caret);
        assert_eq!(editor.content_version, version);
        assert!(source_snapshot.is_instance(&editor.rope));
    }
}

#[test]
fn scope_connector_does_not_invent_deep_scopes_inside_raw_strings_or_comments() {
    use std::time::{Duration, Instant};

    for (start, finish) in [
        ("let payload = r###\"\n", "\"###;\n"),
        ("/* outer /* nested */\n", "*/\n"),
    ] {
        let prefix = format!(
            "{start}{}",
            "masked payload row with braces {{ }}\n".repeat(600)
        );
        let opening = prefix.lines().count();
        let source = format!(
            "{prefix}    if pretend {{\n        fake_work();\n    }}\n{finish}outside();\n"
        );
        for (wrapped, colorize) in [(false, true), (true, false)] {
            let width = if wrapped { 45 } else { 100 };
            let mut app = indexed_scope_fixture(&source, wrapped, colorize);
            let opener = app.tabs[0]
                .editor
                .rope
                .line(opening)
                .chars()
                .position(|ch| ch == '{')
                .unwrap();
            for position in [
                (opening + 1, 8),
                (opening, opener),
                (opening, opener + 1),
                (opening + 2, 4),
                (opening + 2, 5),
            ] {
                place_indexed_scope_cursor(&mut app, position, opening, width);
                if position.0 != opening + 1 {
                    assert!(
                        app.cached_matching_bracket.is_some(),
                        "the raw bracket cache must offer a misleading pair at {position:?}"
                    );
                    assert_eq!(app.cached_cursor_match_target, app.cursor_match_target());
                }
                assert_eq!(
                    app.cached_structural_scope_index
                        .as_ref()
                        .unwrap()
                        .enclosing_scope(&app.tabs[0].editor),
                    None
                );
                app.animations.advance_frame(
                    &app.animation_settings,
                    Instant::now() + Duration::from_secs(2),
                );
                let buffer = render(&mut app, width, 18);
                assert!(
                    scope_rail_cells(&app, &buffer).is_empty(),
                    "literal/comment invented a deep scope: wrap={wrapped}, colorize={colorize}, start={start:?}, caret={position:?}"
                );
            }
            assert_eq!(app.tabs[0].editor.rope.to_string(), source);
        }
    }
}

#[test]
#[ignore = "exports scope connector frames for visual review"]
fn export_scope_connector_preview() {
    use std::time::{Duration, Instant};
    fn rgb(color: ratatui::style::Color) -> String {
        match color {
            ratatui::style::Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            _ => "#aaaaaa".into(),
        }
    }
    let mut scenes = Vec::new();
    for (name, source, cursor) in [
        (
            "Crossing indentation guides",
            "                    {\n        {\n            work();\n\n        }\n    }\n",
            (0, 20),
        ),
        (
            "Several braces on one row",
            "            { { {\n                work();\n\n            }\n        }\n    }\n",
            (1, 16),
        ),
        (
            "Nested scope transition",
            "            { { {\n                work();\n\n            }\n        }\n    }\n",
            (0, 12),
        ),
        (
            "Multiline function signature",
            "impl Example {\n    fn accepts(\n        &self,\n        input: Option<Input>,\n    ) -> bool {\n        code();\n    }\n}\n",
            (5, 8),
        ),
        (
            "Function signature with a blank body row",
            "impl Example {\n    fn accepts(\n        &self,\n        input: Option<Input>,\n    ) -> bool {\n\n        code();\n    }\n}\n",
            (6, 8),
        ),
        (
            "Return type on the following row",
            "impl Example {\n    fn accepts(\n        &self,\n        input: Option<Input>,\n    ) ->\n        bool {\n\n\n        code();\n    }\n}\n",
            (8, 8),
        ),
        (
            "Dense body requires a wider whitespace route",
            "impl Example {\n    fn accepts(\n        &self,\n        input: Option<Input>,\n    ) -> bool {\n        call_with_a_very_wide_expression();\n        work();\n    }\n}\n",
            (6, 8),
        ),
    ] {
        let mut app = fixture(source);
        app.show_minimap = false;
        app.show_indent_guides = true;
        app.cursor_visible = false;
        app.tabs[0].editor.file_path = Some("scope-preview.rs".into());
        app.tabs[0].editor.cursor_line = cursor.0;
        app.tabs[0].editor.cursor_col = cursor.1;
        app.animation_settings
            .set(AnimationEffect::ScopeConnector, true);
        render(&mut app, 64, 16);
        app.animations.advance_frame(
            &app.animation_settings,
            Instant::now() + Duration::from_secs(2),
        );
        if name == "Nested scope transition" {
            app.tabs[0].editor.cursor_line = 1;
            app.tabs[0].editor.cursor_col = 16;
            render(&mut app, 64, 16);
            std::thread::sleep(Duration::from_millis(60));
        }
        let buffer = render(&mut app, 64, 16);
        let cells: Vec<_> = buffer
            .content
            .iter()
            .map(|cell| (cell.symbol().to_owned(), rgb(cell.fg), rgb(cell.bg)))
            .collect();
        scenes.push(serde_json::json!({"name": name, "width": 64, "height": 16, "cells": cells}));
    }
    std::fs::write(
        "target/scope-connector-preview.json",
        serde_json::to_vec(&scenes).unwrap(),
    )
    .unwrap();
}

/// Source frames for `scripts/render_animation_gifs.py`. Capture the production
/// workbench at its normal clock rate, with only one optional effect enabled.
#[test]
#[ignore = "records the five README animation previews (about 30 seconds)"]
fn export_readme_animation_frames() {
    use ratatui::style::{Color, Modifier};
    use std::time::{Duration, Instant};

    const WIDTH: u16 = 48;
    const HEIGHT: u16 = 12;
    const FRAMES: u32 = 180;

    fn rgb(color: Color, fallback: Color) -> String {
        let color = if color == Color::Reset {
            fallback
        } else {
            color
        };
        match color {
            Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            _ => panic!("preview colors must resolve to RGB: {color:?}"),
        }
    }

    fn prepare_source(app: &mut App) {
        let editor = app.editor().unwrap();
        let rope = editor.rope.clone();
        let path = editor.file_path.clone().unwrap();
        let version = editor.content_version;
        let bracket = editor.find_matching_bracket();
        let tag = editor.find_matching_tag();
        app.cached_cursor_match_target = app.cursor_match_target();
        app.cached_matching_bracket = bracket;
        app.cached_matching_tag = tag;
        if let Some(job) = app.highlighter.prepare_job(
            &rope,
            &path,
            version,
            None,
            0..rope.len_lines(),
            None,
            &app.syntax_theme,
        ) {
            let (syntaxes, themes) = app.highlighter.shared_sets();
            app.highlighter
                .apply_batch(job.run(&rope, &syntaxes, &themes, || false));
        }
    }

    let navigation_source = concat!(
        "\n\nfn render_frame() {\n",
        "    let cursor = editor.cursor();\n",
        "    let theme = editor.theme();\n\n",
        "    draw_cursor(cursor, theme);\n",
        "    flush_frame();\n}\n",
    );
    let whitespace_source = concat!(
        "fn main() {\n",
        "let editor = Editor::new();\n",
        "let theme = Theme::dark();\n",
        "render(editor, theme);\n}\n",
    );
    let scope_source = concat!(
        "fn render() { { {\n",
        "    let frame = next_frame();\n\n",
        "    if frame.ready() {\n",
        "        draw(frame);\n",
        "    }\n\n} } }\n\n",
    );
    std::fs::create_dir_all("target/readme-animations").unwrap();
    for (effect, slug, source) in [
        (
            AnimationEffect::CursorSmear,
            "cursor-smear",
            navigation_source,
        ),
        (
            AnimationEffect::CursorFire,
            "fire-hazard",
            navigation_source,
        ),
        (
            AnimationEffect::WhitespaceReveal,
            "whitespace-reveal",
            whitespace_source,
        ),
        (
            AnimationEffect::ScopeConnector,
            "scope-connector",
            scope_source,
        ),
        (AnimationEffect::TabSlide, "sliding-tabs", navigation_source),
    ] {
        let mut app = fixture(source);
        app.show_activity_bar = false;
        app.show_menu_bar = false;
        app.show_breadcrumbs = false;
        app.show_panel = false;
        app.show_terminal_sidebar = false;
        app.show_minimap = false;
        app.show_tab_bar = effect == AnimationEffect::TabSlide;
        app.show_line_numbers = true;
        app.relative_line_numbers = false;
        app.show_indent_guides = true;
        app.sticky_scroll = false;
        app.rulers.clear();
        app.cursor_style = crate::app::CursorStyle::Block;
        app.cursor_visible = true;
        app.terminal_has_focus = true;
        app.animation_settings = Default::default();
        app.animation_settings.set(effect, true);
        app.animation_settings.duration_ms = 420;
        app.animation_settings.fps = 30;
        app.animation_settings.intensity = 70;
        app.tabs[0].editor.file_path = Some("main.rs".into());
        app.tabs[0].editor.word_wrap = false;
        app.tabs[0].editor.whitespace_mode = crate::app::WhitespaceMode::None;
        let initial_cursor = match effect {
            AnimationEffect::WhitespaceReveal => (1, 0),
            AnimationEffect::ScopeConnector => (8, 0),
            _ => (3, 0),
        };
        (
            app.tabs[0].editor.cursor_line,
            app.tabs[0].editor.cursor_col,
        ) = initial_cursor;
        if effect == AnimationEffect::TabSlide {
            for name in ["theme.rs", "lib.rs"] {
                let mut editor = Editor::test_with_text(source);
                editor.file_path = Some(name.into());
                (editor.cursor_line, editor.cursor_col) = initial_cursor;
                app.tabs.push(Tab::new_file(editor));
            }
        }
        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
        // Allow installed theme/syntax discovery to finish before recording.
        for _ in 0..20 {
            app.poll_background_results();
            prepare_source(&mut app);
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
        let started = Instant::now();
        let mut frames = Vec::new();
        for index in 0..FRAMES {
            let due = started + Duration::from_nanos(u64::from(index) * 1_000_000_000 / 30);
            std::thread::sleep(due.saturating_duration_since(Instant::now()));
            match effect {
                AnimationEffect::CursorSmear | AnimationEffect::CursorFire => {
                    let destination = match index {
                        15 => Some((3, 33)),
                        22 => Some((3, 0)),
                        34 => Some((4, 31)),
                        48 => Some((6, 31)),
                        61 => Some((6, 0)),
                        75 => Some((3, 33)),
                        91 => Some((4, 0)),
                        105 => Some((6, 31)),
                        121 => Some((4, 31)),
                        136 => Some(initial_cursor),
                        _ => None,
                    };
                    let editor = app.editor_mut().unwrap();
                    if index == 91 {
                        editor.selection_anchor = Some((3, 0));
                    } else if index == 121 {
                        editor.clear_selection();
                    }
                    if let Some((line, col)) = destination {
                        editor.cursor_line = line;
                        editor.cursor_col = col;
                    }
                }
                AnimationEffect::WhitespaceReveal => {
                    let editor = app.editor_mut().unwrap();
                    match index {
                        15 | 47 | 79 => {
                            editor.cursor_line = 1 + (index as usize - 15) / 32;
                            editor.cursor_col = 0;
                            editor.insert_char('\t');
                        }
                        111 | 127 | 143 => {
                            editor.cursor_line = 3 - (index as usize - 111) / 16;
                            editor.cursor_col = 1;
                            editor.backspace();
                        }
                        _ => {}
                    }
                }
                AnimationEffect::ScopeConnector => {
                    let destination = match index {
                        15 => Some((1, 0)),
                        48 => Some((0, 14)),
                        78 => Some((4, 0)),
                        108 => Some((1, 0)),
                        138 => Some((0, 12)),
                        162 => Some(initial_cursor),
                        _ => None,
                    };
                    if let Some((line, col)) = destination {
                        let editor = app.editor_mut().unwrap();
                        editor.cursor_line = line;
                        editor.cursor_col = col;
                    }
                }
                AnimationEffect::TabSlide => match index {
                    15 => app.active_tab = 2,
                    22 => app.active_tab = 1,
                    48 => app.active_tab = 0,
                    78 => app.active_tab = 2,
                    108 => app.active_tab = 1,
                    138 => app.active_tab = 0,
                    _ => {}
                },
            }
            app.poll_background_results();
            prepare_source(&mut app);
            app.animations
                .advance_frame(&app.animation_settings, Instant::now());
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            let cells: Vec<_> = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| {
                    (
                        cell.symbol().to_owned(),
                        rgb(cell.fg, app.colors.fg_text),
                        rgb(cell.bg, app.colors.bg_editor),
                        cell.modifier.contains(Modifier::BOLD),
                        cell.modifier.contains(Modifier::ITALIC),
                        cell.modifier
                            .contains(Modifier::UNDERLINED)
                            .then(|| rgb(cell.underline_color, cell.fg)),
                    )
                })
                .collect();
            frames.push(cells);
        }
        let data = serde_json::json!({
            "name": effect.label(), "width": WIDTH, "height": HEIGHT,
            "fps": 30, "frames": frames,
        });
        std::fs::write(
            format!("target/readme-animations/{slug}.json"),
            serde_json::to_vec(&data).unwrap(),
        )
        .unwrap();
        eprintln!("Captured {slug}");
    }
}
