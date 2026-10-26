//! Exercise scope routes against the inline blame rendered by the real editor.
use super::*;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn install_inline_blame(app: &mut App) {
    let editor = &app.tabs[0].editor;
    let commit = Arc::new(crate::git_blame::Commit {
        hash: "123456789abcdef".into(),
        author: "Henri".into(),
        email: "henri@example.test".into(),
        age: "7d ago".into(),
        date: "2026-10-06".into(),
        committed_at: "2026-10-06 12:00:00 +00:00".into(),
        summary: "Scope fixture".into(),
        path: "deep-scope.rs".into(),
        message: String::new(),
    });
    let document = crate::git_blame::Document {
        path: editor.file_path.clone().unwrap(),
        revision: None,
        rope: editor.rope.clone(),
        key: None,
        lines: vec![crate::git_blame::Line::Committed(commit); editor.rope.len_lines()],
        commit_lines: (0..editor.rope.len_lines()).map(Some).collect(),
        notice: None,
    };
    app.blame_documents
        .insert(editor.navigation_id, Arc::new(document));
    assert!(crate::git_blame::cached(app, &app.tabs[0].editor).is_some());
}

fn blame_extent(buffer: &Buffer) -> (u16, u16, u16) {
    let label = "Henri, 7d ago";
    let index = buffer
        .content
        .windows(label.len())
        .position(|cells| cells.iter().map(|cell| cell.symbol()).collect::<String>() == label)
        .expect("the inline blame fixture is visible");
    let y = index as u16 / buffer.area.width;
    let x = index as u16 % buffer.area.width;
    (y, x - 2, x + label.len() as u16)
}

/// Compare actual editor frames with and without the annotation. Every cell
/// outside its complete rendered extent must keep exactly the same contour;
/// inside, even spaces retain the annotation's ordinary frame.
fn assert_blame_overlays_unchanged_bubble(
    app: &mut App,
    connected: &Buffer,
    plain: &Buffer,
    pair: ((usize, usize), (usize, usize)),
    context: &str,
) -> usize {
    let (row, left, right) = blame_extent(plain);
    let id = app.tabs[0].editor.navigation_id;
    let document = app.blame_documents.remove(&id).unwrap();
    let intensity = app.animation_settings.intensity;
    app.animation_settings.intensity = 0;
    let source = render(app, plain.area.width, plain.area.height);
    app.animation_settings.intensity = intensity;
    let unobscured = render(app, plain.area.width, plain.area.height);
    app.blame_documents.insert(id, document);
    let code = app.code_area.unwrap();
    let mut hidden = 0;
    for y in code.y..code.bottom() {
        for x in code.x.saturating_sub(1)..code.right() {
            if y == row && x >= left && x < right {
                assert_eq!(
                    connected[(x, y)],
                    plain[(x, y)],
                    "blame covers bubble ink, including its spaces at ({x}, {y}): {context}"
                );
                hidden += usize::from(scope_rail_connections(unobscured[(x, y)].symbol()) != 0);
            } else {
                assert_eq!(
                    connected[(x, y)],
                    unobscured[(x, y)],
                    "inline blame changed the source contour at ({x}, {y}): {context}"
                );
            }
        }
    }
    assert_scope_route_source_safety(app, &unobscured, &source, pair, context, true);
    hidden
}

fn agent_state_case(source: &str, opening: usize, closing: usize) {
    let pair = ((opening, 15), (closing, 4));
    let mut app = indexed_scope_fixture(source, false, false);
    install_inline_blame(&mut app);
    let width = 110;
    let height = 20;

    // Reuse the view while blame moves between endpoints and body rows. It
    // covers the existing contour without changing that contour's route.
    for position in [
        (opening + 4, 13),
        pair.0,
        pair.1,
        (closing - 1, 8),
        (closing - 1, 3),
        pair.0,
        pair.1,
    ] {
        let editor = &mut app.tabs[0].editor;
        (editor.cursor_line, editor.cursor_col) = position;
        editor.scroll_offset = opening.saturating_sub(1);
        app.cursor_visible = true;
        assert_eq!(
            app.cached_structural_scope_index
                .as_ref()
                .unwrap()
                .enclosing_scope(&app.tabs[0].editor),
            Some(pair),
            "the AgentState block remains active at {position:?}"
        );
        app.animation_settings.intensity = 0;
        let plain = render(&mut app, width, height);
        let blame_start = plain
            .content
            .windows("Henri, 7d ago".len())
            .position(|cells| {
                cells.iter().map(|cell| cell.symbol()).collect::<String>() == "Henri, 7d ago"
            })
            .unwrap_or_else(|| panic!("the real inline blame must be visible at {position:?}"));
        let code = app.code_area.unwrap();
        let cursor = (
            code.x + position.1 as u16,
            code.y + (position.0 - app.tabs[0].editor.scroll_offset) as u16,
        );
        app.animation_settings.intensity = 100;
        render(&mut app, width, height);
        app.animations.advance_frame(
            &app.animation_settings,
            Instant::now() + Duration::from_secs(2),
        );
        let connected = render(&mut app, width, height);
        assert_eq!(
            connected[cursor], plain[cursor],
            "scope drawing changed the caret at {position:?}"
        );
        assert_eq!(
            connected.content[blame_start..blame_start + "Henri, 7d ago".len()],
            plain.content[blame_start..blame_start + "Henri, 7d ago".len()],
            "scope drawing changed inline blame at {position:?}"
        );
        for y in code.y..code.bottom() {
            for x in code.x..code.right() {
                if plain[(x, y)].symbol() != " " {
                    assert_eq!(
                        connected[(x, y)],
                        plain[(x, y)],
                        "source or blame changed at ({x}, {y}), caret={position:?}"
                    );
                }
            }
        }

        // Carets mask route ink without changing its geometry. Remove that
        // intentional one-cell mask to inspect every port and attachment.
        app.cursor_visible = false;
        app.animation_settings.intensity = 0;
        let plain = render(&mut app, width, height);
        app.animation_settings.intensity = 100;
        let connected = render(&mut app, width, height);
        assert_blame_overlays_unchanged_bubble(
            &mut app,
            &connected,
            &plain,
            pair,
            &format!("AgentState inline blame at {position:?}"),
        );
    }
    assert_eq!(app.tabs[0].editor.rope.to_string(), source);
}

#[test]
fn scope_connector_keeps_exact_agent_state_attachments_around_inline_blame() {
    let original = include_str!("app_acp_agents_tests.rs");
    let lines: Vec<_> = original.lines().collect();
    let opening = lines
        .iter()
        .position(|line| *line == "    AgentState {")
        .unwrap();
    let closing = opening + 9;
    assert_eq!(lines[closing], "    }");
    // Blank exterior rows allow both braces to remain exactly attached even
    // when blame closes the former route along an endpoint's own row.
    let source = format!("\n{}\n\n", lines[opening..=closing].join("\n"));
    agent_state_case(&source, 1, 10);
}

#[test]
fn scope_connector_preserves_actual_agent_state_source_blame_and_carets() {
    let source = include_str!("app_acp_agents_tests.rs");
    let opening = source
        .lines()
        .position(|line| line == "    AgentState {")
        .unwrap();
    // The preceding assignment and following field previously enclosed the
    // opener's whitespace pocket when inline blame immediately followed it.
    agent_state_case(source, opening, opening + 9);
}

#[test]
fn disabling_scope_connector_keeps_inline_blame_after_its_source_line() {
    let source = "\n    AgentState {\n        field: some_long_value(),\n    }\n";
    let mut app = indexed_scope_fixture(source, false, false);
    install_inline_blame(&mut app);
    app.tabs[0].editor.cursor_line = 1;
    app.tabs[0].editor.cursor_col = 15;
    app.animation_settings
        .set(AnimationEffect::ScopeConnector, false);
    let buffer = render(&mut app, 110, 16);
    let code = app.code_area.unwrap();
    let expected = "    AgentState {  Henri, 7d ago";
    for (column, ch) in expected.chars().enumerate() {
        assert_eq!(
            buffer[(code.x + column as u16, code.y + 1)].symbol(),
            ch.to_string(),
            "disabled connector shifted inline blame at column {column}"
        );
    }
}

#[test]
fn scope_bubble_blame_overlay_tracks_source_end_under_pan_and_color_preview() {
    let source = "fn outer() {\n    AgentState {\n\t\tfield:\t\"界#ff8000\";\n    }\n}\n";
    let pair = ((1, 15), (3, 4));
    for (pan, preview) in [(0, false), (4, false), (4, true)] {
        let mut app = indexed_scope_fixture(source, false, false);
        install_inline_blame(&mut app);
        app.show_color_preview = preview;
        app.cursor_visible = false;
        app.tabs[0].editor.tab_size = 4;
        app.tabs[0].editor.horizontal_scroll = pan;
        for position in [pair.0, pair.1] {
            (
                app.tabs[0].editor.cursor_line,
                app.tabs[0].editor.cursor_col,
            ) = position;
            app.animation_settings.intensity = 0;
            let plain = render(&mut app, 110, 16);
            assert_eq!(app.tabs[0].editor.horizontal_scroll, pan);
            let code = app.code_area.unwrap();
            let body_y = code.y + 2;
            assert_eq!(
                (code.x..code.right()).any(|x| plain[(x, body_y)].symbol() == "▉"),
                preview,
                "the fixture renders its requested color preview"
            );
            let blame_y = code.y + position.0 as u16;
            let blame_x = (code.x..code.right())
                .find(|&x| plain[(x, blame_y)].symbol() == "H")
                .expect("inline blame remains visible after padding");
            assert_eq!(
                blame_x,
                code.x + (source.lines().nth(position.0).unwrap().len() - pan) as u16 + 2,
                "blame stays after its own source row independently of the neighboring body: pan={pan}, preview={preview}, caret={position:?}"
            );
            app.animation_settings.intensity = 100;
            render(&mut app, 110, 16);
            app.animations.advance_frame(
                &app.animation_settings,
                Instant::now() + Duration::from_secs(2),
            );
            let connected = render(&mut app, 110, 16);
            assert_blame_overlays_unchanged_bubble(
                &mut app,
                &connected,
                &plain,
                pair,
                &format!("projected blame: pan={pan}, preview={preview}, caret={position:?}"),
            );
            for offset in 0.."Henri, 7d ago".len() as u16 {
                assert_eq!(
                    connected[(blame_x + offset, blame_y)],
                    plain[(blame_x + offset, blame_y)],
                    "projected blame retains its complete cells"
                );
            }
        }
    }
}

#[test]
fn inline_blame_glyphs_and_spaces_cover_both_bubble_styles_without_detours() {
    let source = "fn outer() {\n    tiny();\n    much_longer_value();\n    other();\n}\n";
    let pair = ((0, 11), (4, 0));
    for tight in [true, false] {
        for theme in [
            crate::app_theme::EditorTheme::Dark,
            crate::app_theme::EditorTheme::Light,
        ] {
            let mut app = indexed_scope_fixture(source, false, false);
            app.colors = theme.colors();
            app.animation_settings.scope_tight = tight;
            app.animation_settings.intensity = 100;
            app.cursor_visible = false;
            install_inline_blame(&mut app);
            let mut hidden = 0;
            for position in [(0, 11), (1, 4), (3, 4), (4, 0)] {
                (
                    app.tabs[0].editor.cursor_line,
                    app.tabs[0].editor.cursor_col,
                ) = position;
                render(&mut app, 90, 14);
                app.animations.advance_frame(
                    &app.animation_settings,
                    Instant::now() + Duration::from_secs(2),
                );
                app.animation_settings.intensity = 0;
                let plain = render(&mut app, 90, 14);
                app.animation_settings.intensity = 100;
                let connected = render(&mut app, 90, 14);
                hidden += assert_blame_overlays_unchanged_bubble(
                    &mut app,
                    &connected,
                    &plain,
                    pair,
                    &format!(
                        "tight={tight}, dark={}, caret={position:?}",
                        theme == crate::app_theme::EditorTheme::Dark
                    ),
                );
            }
            assert!(hidden > 0, "the fixture must actually cover tracing cells");
        }
    }
}

#[test]
#[ignore = "exports actual editor frames with inline blame overlaid on the bubble"]
fn export_scope_bubble_blame_overlay_preview() {
    fn rgb(color: ratatui::style::Color) -> String {
        match color {
            ratatui::style::Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            _ => "#aaaaaa".into(),
        }
    }
    let source = "fn outer() {\n    tiny();\n    much_longer_value();\n    other();\n}\n";
    let mut app = indexed_scope_fixture(source, false, false);
    app.animation_settings.intensity = 100;
    app.cursor_visible = false;
    app.tabs[0].editor.cursor_line = 1;
    app.tabs[0].editor.cursor_col = 4;
    let mut frames = Vec::new();
    let mut names = Vec::new();
    for tight in [true, false] {
        app.animation_settings.scope_tight = tight;
        for blame in [false, true] {
            if blame {
                install_inline_blame(&mut app);
            } else {
                app.blame_documents.clear();
            }
            render(&mut app, 70, 10);
            app.animations.advance_frame(
                &app.animation_settings,
                Instant::now() + Duration::from_secs(2),
            );
            let buffer = render(&mut app, 70, 10);
            names.push(format!(
                "{} bubble {} inline blame",
                if tight { "Tight" } else { "Loose" },
                if blame { "with" } else { "without" }
            ));
            frames.push(
                buffer
                    .content
                    .iter()
                    .map(|cell| {
                        serde_json::json!([
                            cell.symbol(),
                            rgb(cell.fg),
                            rgb(cell.bg),
                            cell.modifier.contains(ratatui::style::Modifier::BOLD),
                            cell.modifier.contains(ratatui::style::Modifier::ITALIC),
                            cell.modifier
                                .contains(ratatui::style::Modifier::UNDERLINED)
                                .then(|| rgb(cell.underline_color))
                        ])
                    })
                    .collect::<Vec<_>>(),
            );
        }
    }
    let data = serde_json::json!({ "name": "Inline blame overlays scope contours", "width": 70,
        "height": 10, "frame_names": names, "frames": frames });
    std::fs::create_dir_all("target").unwrap();
    std::fs::write(
        "target/scope-bubble-blame-overlay.json",
        serde_json::to_vec(&data).unwrap(),
    )
    .unwrap();
}

#[test]
fn actual_agent_state_shared_brace_has_a_whitespace_bubble_without_underlining() {
    use ratatui::style::Modifier;

    let original = include_str!("app_acp_agents_tests.rs");
    let mut lines: Vec<_> = original.lines().map(str::to_owned).collect();
    let opening = lines
        .iter()
        .position(|line| line == "    AgentState {")
        .unwrap();
    assert_eq!(
        lines[opening - 1],
        "    session.title = format!(\"{name} conversation\");"
    );
    let original_closing = opening + 9;
    assert_eq!(lines[original_closing], "    }");
    lines[opening] = "    AgentState { {".into();
    lines.insert(original_closing, "      }".into());
    let closing = original_closing + 1;
    let source = format!("{}\n", lines.join("\n"));
    let pair = ((opening, 15), (closing, 4));
    let mut app = indexed_scope_fixture(&source, false, false);
    install_inline_blame(&mut app);
    app.tabs[0].editor.cursor_line = closing;
    app.tabs[0].editor.cursor_col = 4;
    app.tabs[0].editor.scroll_offset = opening - 1;
    app.animation_settings.intensity = 100;
    assert_eq!(
        app.cached_structural_scope_index
            .as_ref()
            .unwrap()
            .enclosing_scope(&app.tabs[0].editor),
        Some(pair),
        "real structural discovery selects the first AgentState brace and the original outer closer"
    );
    render(&mut app, 110, 20);
    app.animations.advance_frame(
        &app.animation_settings,
        Instant::now() + Duration::from_secs(2),
    );
    app.cursor_visible = false;
    app.animation_settings.intensity = 0;
    let plain = render(&mut app, 110, 20);
    app.animation_settings.intensity = 100;
    let connected = render(&mut app, 110, 20);
    let (brace_x, heading_y) = scope_endpoint_screen_position(&mut app, &plain, pair.0)
        .expect("the first AgentState brace is visible");

    for x in brace_x - 11..=brace_x {
        let before = &plain[(x, heading_y)];
        let after = &connected[(x, heading_y)];
        assert!(!after.modifier.contains(Modifier::UNDERLINED));
        if before.symbol() == " " {
            assert_eq!(after.symbol(), "─", "heading gaps continue the boundary");
            continue;
        }
        assert_eq!(after.symbol(), before.symbol());
        assert_eq!(
            after.fg, before.fg,
            "the bubble preserves syntax foreground"
        );
        if x != brace_x {
            assert_eq!(
                after.bg, before.bg,
                "the bubble preserves the source background"
            );
        }
    }
    let other_brace = (brace_x + 2, heading_y);
    assert_eq!(
        connected[other_brace], plain[other_brace],
        "the second opening brace is not part of this scope"
    );
    assert!(
        !connected[other_brace]
            .modifier
            .contains(Modifier::UNDERLINED)
    );
    let code = app.code_area.unwrap();
    let body_y = heading_y + 1;
    let source_start = (code.x..code.right())
        .find(|&x| !plain[(x, body_y)].symbol().trim().is_empty())
        .unwrap();
    let source_end = (code.x..code.right())
        .rfind(|&x| !plain[(x, body_y)].symbol().trim().is_empty())
        .unwrap();
    for side in [
        code.x.saturating_sub(1)..source_start,
        source_end + 1..code.right(),
    ] {
        assert!(
            side.into_iter()
                .any(|x| scope_rail_connections(connected[(x, body_y)].symbol()) != 0),
            "the actual dense struct body needs a trace on both sides"
        );
    }
    assert_blame_overlays_unchanged_bubble(
        &mut app,
        &connected,
        &plain,
        pair,
        "actual AgentState shared brace and inline blame",
    );
    assert_eq!(app.tabs[0].editor.rope.to_string(), source);
}
