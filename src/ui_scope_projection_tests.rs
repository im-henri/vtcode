//! The editor viewport crops one source contour; scrolling never reshapes it.
use super::*;

use crate::{
    animation_structural_scope::StructuralScopeRequest,
    app::{ActivePane, Tab},
    editor::Editor,
};
use ratatui::{Terminal, backend::TestBackend};
use std::{sync::Arc, time::Duration};

fn source() -> String {
    let mut lines = vec!["fn tracing_scope() {".to_owned()];
    for row in 0..42 {
        lines.push(match row % 7 {
            0 => "    first();".into(),
            1 => "        indented_first();".into(),
            2 => "        indented_second();".into(),
            3 => "    small();".into(),
            4 => format!("    process_{}();", "a_long_source_identifier_".repeat(4)),
            5 => "    next();".into(),
            _ => "    last();".into(),
        });
    }
    lines.push("}".into());
    format!("{}\n", lines.join("\n"))
}

fn fixture(source: &str, tight: bool) -> App {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text(source);
    editor.file_path = Some("scope-projection.rs".into());
    editor.language_override = Some("Rust".into());
    editor.word_wrap = false;
    editor.cursor_line = 2;
    editor.cursor_col = 8;
    let index = StructuralScopeRequest::from_editor(&editor)
        .unwrap()
        .build(None, || false)
        .unwrap();
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
    app.cursor_visible = false;
    app.cursor_blinking = false;
    app.bracket_pair_colorization = false;
    app.cached_structural_scope_index = Some(Arc::new(index));
    app.colors.gradients.bg_editor = crate::app_theme::ThemeGradientSpec {
        start: app.colors.bg_editor,
        end: app.colors.bg_editor,
        angle_deg: 0.0,
    };
    app.colors.bg_line_highlight = app.colors.bg_editor;
    app.colors.gradients.bg_line_highlight = app.colors.gradients.bg_editor;
    for effect in Effect::ALL {
        app.animation_settings.set(effect, false);
    }
    app.animation_settings.set(Effect::ScopeConnector, true);
    app.animation_settings.scope_tight = tight;
    app.animation_settings
        .set_intensity(Effect::ScopeConnector, 100);
    app
}

fn render(app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    terminal.backend().buffer().clone()
}

fn settle(app: &mut App, width: u16, height: u16) -> Buffer {
    render(app, width, height);
    app.animations.advance_frame(
        &app.animation_settings,
        Instant::now() + Duration::from_secs(2),
    );
    render(app, width, height)
}

fn assert_idle(app: &App) {
    assert_eq!(
        app.animations
            .next_frame_wait(&app.animation_settings, Instant::now()),
        None,
        "viewport changes must not replay a settled entrance"
    );
}

#[test]
fn inline_scope_projection_tracks_columns_and_clips_wrapped_neighboring_code() {
    let line = "    before(); before(); before(); { one(); two(); three(); } { other(); } after(); after(); after();";
    let source = format!("fn run() {{\n{line}\n}}\n");
    let pairs = [
        ((1, line.find('{').unwrap()), (1, line.find('}').unwrap())),
        ((1, line.rfind('{').unwrap()), (1, line.rfind('}').unwrap())),
    ];
    for (width, wrapped, large) in [(120, false, false), (34, true, false), (120, false, true)] {
        for (tight, fill) in [(true, false), (false, false), (true, true), (false, true)] {
            let mut app = fixture(&source, tight);
            app.animation_settings.scope_fill = fill;
            app.tabs[0].editor.word_wrap = wrapped;
            app.tabs[0].editor.large_file_mode = large;
            for pair in pairs.into_iter().chain(pairs.into_iter().rev()) {
                app.animation_settings.set(Effect::ScopeConnector, true);
                app.tabs[0].editor.cursor_line = 1;
                app.tabs[0].editor.cursor_col = pair.0.1 + 2;
                let frame = settle(&mut app, width, 18);
                let code = app.code_area.unwrap();
                assert_eq!(
                    app.animations.scope(
                        &app.tabs[0].editor,
                        app.cached_structural_scope_index.as_deref()
                    ),
                    Some(pair)
                );
                let ranges = if wrapped && !large {
                    Editor::wrap_ranges_for_line_text_tabbed(
                        line,
                        app.tabs[0].editor.viewport_width,
                        app.tabs[0].editor.tab_size,
                    )
                } else {
                    vec![(0, line.len())]
                };
                let position = |col| {
                    let (row, &(start, _)) = ranges
                        .iter()
                        .enumerate()
                        .find(|(_, (start, end))| *start <= col && col < *end)
                        .unwrap();
                    (code.x + (col - start) as u16, code.y + 1 + row as u16)
                };
                let first = position(pair.0.1);
                let last = position(pair.1.1);
                let mut ink = 0;
                for y in code.y..code.bottom() {
                    for x in code.x.saturating_sub(1)..code.right() {
                        if scope_connections(frame[(x, y)].symbol()) != 0 {
                            ink += 1;
                            assert!(
                                (first.1..=last.1).contains(&y),
                                "a wrapped neighbor acquired the contour at {x}:{y}, pair={pair:?}, wrapped={wrapped}, tight={tight}"
                            );
                            if first.1 == last.1 {
                                assert!((first.0 - 1..=last.0 + 1).contains(&x));
                            }
                        }
                    }
                }
                assert!(
                    ink > 0,
                    "the inline scope must be visible: pair={pair:?}, wrapped={wrapped}, large={large}, tight={tight}"
                );
                app.animation_settings.set(Effect::ScopeConnector, false);
                let mut plain = render(&mut app, width, 18);
                let extra_width = app.code_area.unwrap().width - code.width;
                if extra_width > 0 {
                    plain = render(&mut app, width - extra_width, 18);
                }
                let plain_code = app.code_area.unwrap();
                for y in code.y..code.bottom() {
                    for x in code.x..code.right().min(code.x + plain_code.width) {
                        let original = &plain[(plain_code.x + x - code.x, y)];
                        if !original.symbol().trim().is_empty() {
                            assert_eq!(
                                frame[(x, y)].symbol(),
                                original.symbol(),
                                "source text must be preserved"
                            );
                        }
                    }
                }
                for (col, ch) in line
                    .char_indices()
                    .filter(|(_, ch)| matches!(ch, '{' | '}'))
                {
                    let point = position(col);
                    let original = &plain[(plain_code.x + point.0 - code.x, point.1)];
                    assert_eq!(frame[point].symbol(), ch.to_string());
                    if col == pair.0.1 || col == pair.1.1 {
                        assert_ne!(
                            frame[point].bg, original.bg,
                            "the selected braces must be emphasized"
                        );
                    } else {
                        assert_eq!(
                            frame[point], *original,
                            "neighboring braces must stay unchanged"
                        );
                    }
                }
                assert_eq!(app.tabs[0].editor.rope.to_string(), source);
            }
        }
    }
}

#[test]
fn inline_scope_emphasizes_compact_braces_without_overwriting_adjacent_tokens() {
    let source = "fn run() {\n    {{work();}};\n}\n";
    let mut app = fixture(source, true);
    app.tabs[0].editor.cursor_line = 1;
    app.tabs[0].editor.cursor_col = 8;
    let frame = settle(&mut app, 80, 12);
    let code = app.code_area.unwrap();
    app.animation_settings.set(Effect::ScopeConnector, false);
    let plain = render(&mut app, 80, 12);
    let plain_code = app.code_area.unwrap();
    for (col, ch) in source.lines().nth(1).unwrap().char_indices() {
        let decorated = &frame[(code.x + col as u16, code.y + 1)];
        let original = &plain[(plain_code.x + col as u16, plain_code.y + 1)];
        assert_eq!(decorated.symbol(), ch.to_string());
        if col == 5 || col == 13 {
            assert_ne!(
                decorated.bg, original.bg,
                "the inner delimiters identify a scope with no room for an outline"
            );
        } else {
            assert_eq!(
                decorated, original,
                "compact neighbors stay untouched at {col}"
            );
        }
    }
}

#[test]
fn oversized_source_scope_uses_visible_routes_without_decoding_or_damaging_source() {
    let source = format!("fn outer() {{\n{}}}\n", "    body();\n".repeat(2_048));
    let mut app = fixture(&source, true);
    let frame = settle(&mut app, 100, 20);
    let id = app.tabs[0].editor.navigation_id;
    assert_eq!(app.animations.scope_paint.view(id).decoded_rows(), 0);
    assert!(
        frame
            .content
            .iter()
            .any(|cell| scope_connections(cell.symbol()) != 0),
        "an oversized source scope must retain its visible outline"
    );
    assert!(
        app.animations.scope_paint.stats(id).0 > 0,
        "the viewport fallback is routed"
    );
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
                    ch.to_string()
                );
            }
        }
    }
    assert_eq!(app.tabs[0].editor.rope.to_string(), source);
    assert_idle(&app);
}

fn assert_crop(
    reference: &Buffer,
    reference_code: Rect,
    cropped: &Buffer,
    app: &App,
    pan_cells: usize,
    context: &str,
) -> usize {
    let code = app.code_area.unwrap();
    let scroll = app.tabs[0].editor.scroll_offset;
    let mut compared = 0;
    for y in code.y..code.bottom() {
        let reference_y = reference_code.y + scroll as u16 + y - code.y;
        if reference_y >= reference_code.bottom() {
            continue;
        }
        for x in code.x.saturating_sub(1)..code.right() {
            let reference_x = (i32::from(reference_code.x) + i32::from(x) - i32::from(code.x)
                + pan_cells as i32) as u16;
            if reference_x >= reference_code.right() {
                continue;
            }
            let expected = &reference[(reference_x, reference_y)];
            let actual = &cropped[(x, y)];
            if scope_connections(expected.symbol()) != 0 || scope_connections(actual.symbol()) != 0
            {
                assert_eq!(
                    actual.symbol(),
                    expected.symbol(),
                    "viewport changed contour ports at ({x}, {y}), source row {}: {context}",
                    scroll + usize::from(y - code.y)
                );
                assert_eq!(
                    actual.fg, expected.fg,
                    "viewport changed contour ink at ({x}, {y}): {context}"
                );
                assert_eq!(
                    actual.bg, expected.bg,
                    "viewport changed contour brightness at ({x}, {y}): {context}"
                );
                compared += 1;
            }
        }
    }
    compared
}

#[test]
fn scope_fill_is_an_idle_crop_when_scrolling_or_resizing_the_editor() {
    let source = source();
    for tight in [true, false] {
        let mut app = fixture(&source, tight);
        app.animation_settings.scope_fill = true;
        let wide = settle(&mut app, 190, 52);
        let wide_code = app.code_area.unwrap();
        let mut compared = 0;
        for (scroll, pan, width) in [
            (0, 0, 80),
            (5, 9, 62),
            (17, 52, 40),
            (33, 0, 88),
            (0, 0, 80),
        ] {
            app.tabs[0].editor.scroll_offset = scroll;
            app.tabs[0].editor.horizontal_scroll = pan;
            let cropped = render(&mut app, width, 12);
            let code = app.code_area.unwrap();
            for y in code.y..code.bottom() {
                let reference_y = wide_code.y + scroll as u16 + y - code.y;
                for x in code.x..code.right().saturating_sub(1) {
                    let reference_x = wide_code.x + pan as u16 + x - code.x;
                    if reference_y < wide_code.bottom() && reference_x < wide_code.right() {
                        assert_eq!(
                            cropped[(x, y)].bg,
                            wide[(reference_x, reference_y)].bg,
                            "fill moved at {x}:{y}: tight={tight}, pan={pan}, scroll={scroll}"
                        );
                        compared += 1;
                    }
                }
            }
            assert_idle(&app);
        }
        assert!(compared > 1000);
    }
}

#[test]
fn source_bubble_in_a_narrow_editor_is_an_exact_crop_of_a_wider_editor() {
    let source = source();
    for tight in [true, false] {
        let mut app = fixture(&source, tight);
        let wide = settle(&mut app, 170, 52);
        let wide_code = app.code_area.unwrap();
        assert!(source.lines().map(str::len).max().unwrap() < wide_code.width as usize);
        for width in [88, 54, 31, 88] {
            let narrow = render(&mut app, width, 18);
            assert!(
                source.lines().map(str::len).max().unwrap() > app.code_area.unwrap().width as usize
            );
            assert!(
                assert_crop(
                    &wide,
                    wide_code,
                    &narrow,
                    &app,
                    0,
                    &format!("tight={tight}, width={width}")
                ) > 0
            );
            assert_idle(&app);
        }
    }
}

#[test]
fn horizontal_panning_translates_the_ascii_source_contour_without_edge_rails() {
    let source = source();
    for tight in [true, false] {
        let mut app = fixture(&source, tight);
        let wide = settle(&mut app, 190, 52);
        let wide_code = app.code_area.unwrap();
        let mut compared = 0;
        for pan in [0, 9, 26, 52, 9, 0] {
            app.tabs[0].editor.horizontal_scroll = pan;
            let panned = render(&mut app, 62, 18);
            assert_eq!(app.tabs[0].editor.horizontal_scroll, pan);
            compared += assert_crop(
                &wide,
                wide_code,
                &panned,
                &app,
                pan,
                &format!("tight={tight}, pan={pan}"),
            );
            assert_idle(&app);
        }
        assert!(
            compared > 20,
            "the comparisons include actual visible tracing cells"
        );
    }
}

#[test]
fn repeated_middle_and_bottom_scroll_preserves_contour_ports_colors_and_idle_state() {
    let source = source();
    for tight in [true, false] {
        let mut app = fixture(&source, tight);
        let wide = settle(&mut app, 170, 52);
        let wide_code = app.code_area.unwrap();
        for scroll in [0, 5, 17, 33, 40, 7, 33, 0] {
            app.tabs[0].editor.scroll_offset = scroll;
            let scrolled = render(&mut app, 88, 12);
            assert!(
                assert_crop(
                    &wide,
                    wide_code,
                    &scrolled,
                    &app,
                    0,
                    &format!("tight={tight}, scroll={scroll}")
                ) > 0
            );
            assert_idle(&app);
        }
    }
}

#[test]
fn scrolling_during_the_entrance_keeps_its_original_start_time() {
    let source = source();
    for tight in [true, false] {
        let mut app = fixture(&source, tight);
        app.animation_settings
            .set_duration(Effect::ScopeConnector, 1500);
        render(&mut app, 88, 14);
        let id = app.tabs[0].editor.navigation_id;
        let pair = app
            .animations
            .scope(
                &app.tabs[0].editor,
                app.cached_structural_scope_index.as_deref(),
            )
            .unwrap();
        let started = app
            .animations
            .update_scope(id, Some(pair), &app.animation_settings, Instant::now())
            .into_iter()
            .find(|event| matches!(event.trigger, Trigger::Scope { exiting: None, .. }))
            .unwrap()
            .started;
        for scroll in [9, 25, 35, 0] {
            app.tabs[0].editor.scroll_offset = scroll;
            render(&mut app, 88, 14);
            let incoming = app.animations.update_scope(
                id,
                Some(pair),
                &app.animation_settings,
                Instant::now(),
            );
            assert_eq!(
                incoming
                    .iter()
                    .filter(|event| matches!(event.trigger, Trigger::Scope { exiting: None, .. }))
                    .count(),
                1
            );
            assert_eq!(
                incoming
                    .iter()
                    .find(|event| matches!(event.trigger, Trigger::Scope { exiting: None, .. }))
                    .unwrap()
                    .started,
                started,
                "scroll={scroll}, tight={tight} replayed the entrance"
            );
        }
    }
}

#[test]
fn tab_and_wide_glyph_body_clipping_preserves_the_source_contour() {
    let source = concat!(
        "fn unicode_scope() {\n",
        "\tfirst();\n",
        "\tlet label = \"界界界界界界界界界界界界界界界界界界\";\n",
        "\t\tindented();\n",
        "\t\tcontinued();\n",
        "\tlast();\n",
        "}\n",
    );
    for tight in [true, false] {
        let mut app = fixture(source, tight);
        app.tabs[0].editor.tab_size = 4;
        for pan in [0, 1, 4, 8] {
            app.tabs[0].editor.horizontal_scroll = pan;
            app.tabs[0].editor.scroll_offset = 0;
            let wide = settle(&mut app, 100, 18);
            let wide_code = app.code_area.unwrap();
            for (width, scroll) in [(34, 0), (24, 1), (34, 2), (24, 4), (24, 0)] {
                app.tabs[0].editor.scroll_offset = scroll;
                let cropped = render(&mut app, width, 8);
                assert!(
                    assert_crop(
                        &wide,
                        wide_code,
                        &cropped,
                        &app,
                        0,
                        &format!(
                            "wide glyphs: tight={tight}, pan={pan}, width={width}, scroll={scroll}"
                        )
                    ) > 0
                );
                assert_idle(&app);
            }
        }
    }
}

#[test]
#[ignore = "exports native editor crops of one source contour for visual review"]
fn export_scope_bubble_clipping_preview() {
    fn rgb(color: Color, fallback: Color) -> String {
        let color = if matches!(color, Color::Rgb(..)) {
            color
        } else {
            fallback
        };
        match color {
            Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            _ => "#aaaaaa".into(),
        }
    }
    let source = source();
    let mut names = Vec::new();
    let mut frames = Vec::new();
    for tight in [true, false] {
        let mut app = fixture(&source, tight);
        settle(&mut app, 160, 52);
        for (name, width, height, scroll, pan) in [
            ("Wide editor", 160, 20, 0, 0),
            ("Narrow editor", 58, 20, 0, 0),
            ("Middle viewport", 88, 12, 17, 0),
            ("Bottom viewport", 88, 12, 36, 0),
            ("Horizontal pan", 88, 20, 0, 26),
        ] {
            app.tabs[0].editor.scroll_offset = scroll;
            app.tabs[0].editor.horizontal_scroll = pan;
            let native = render(&mut app, width, height);
            let mut padded = Buffer::empty(Rect::new(0, 0, 160, 20));
            padded.set_style(
                padded.area,
                Style::default()
                    .fg(app.colors.fg_text)
                    .bg(app.colors.bg_editor),
            );
            for y in 0..height {
                for x in 0..width {
                    padded[(x, y)] = native[(x, y)].clone();
                }
            }
            names.push(format!("{}: {name}", if tight { "Tight" } else { "Loose" }));
            frames.push(
                padded
                    .content
                    .iter()
                    .map(|cell| {
                        serde_json::json!([
                            cell.symbol(),
                            rgb(cell.fg, app.colors.fg_text),
                            rgb(cell.bg, app.colors.bg_editor),
                            false,
                            false,
                            null
                        ])
                    })
                    .collect::<Vec<_>>(),
            );
        }
    }
    let data = serde_json::json!({ "name": "One source bubble cropped by the viewport", "width": 160,
        "height": 20, "frame_names": names, "frames": frames });
    std::fs::create_dir_all("target").unwrap();
    std::fs::write(
        "target/scope-bubble-clipping-preview.json",
        serde_json::to_vec(&data).unwrap(),
    )
    .unwrap();
}
