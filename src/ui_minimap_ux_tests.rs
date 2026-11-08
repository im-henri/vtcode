use super::*;
use crate::editor::Editor;
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn render(editor: &Editor, scroll: usize, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            draw_minimap(
                frame,
                MinimapParams {
                    area: Rect::new(0, 0, width, height),
                    editor,
                    scroll,
                    visible_lines: height as usize,
                    search_matches: &[],
                    diagnostic_lines: &[],
                    diagnostic_index: None,
                    change_index: None,
                    diff_changes: None,
                    colors: crate::app_theme::EditorTheme::Dark.colors(),
                    hovered: false,
                    active: false,
                },
            )
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

#[test]
fn deep_indentation_stays_visible_and_tabs_follow_editor_settings() {
    let rope = ropey::Rope::from_str(&format!(
        "{}return nested_result;\n\t\tvalue = 123;\n   \t  \n",
        " ".repeat(160)
    ));
    let mut cache = MinimapCache::default();
    cache.synchronize(&rope, 12, 4);
    let row = cache.row(&rope, 0);
    assert!(row[..4].iter().all(|sample| sample.glyph == Glyph::Blank));
    assert!(row[4..].iter().all(|sample| sample.glyph != Glyph::Blank));
    assert!(
        cache
            .row(&rope, 2)
            .iter()
            .all(|sample| sample.glyph == Glyph::Blank)
    );
    cache.synchronize(&rope, 24, 4);
    let before = cache
        .row(&rope, 1)
        .iter()
        .take_while(|sample| sample.glyph == Glyph::Blank)
        .count();
    cache.synchronize(&rope, 24, 8);
    let after = cache
        .row(&rope, 1)
        .iter()
        .take_while(|sample| sample.glyph == Glyph::Blank)
        .count();
    assert_eq!((before, after), (2, 4));
    for width in [1, 2, 3, 8] {
        cache.synchronize(&rope, width, 4);
        assert!(
            cache
                .row(&rope, 0)
                .iter()
                .any(|sample| sample.glyph != Glyph::Blank),
            "width={width}"
        );
    }
}

#[test]
fn all_four_source_lines_contribute_even_when_the_first_is_blank() {
    let editor = Editor::test_with_text(
        &[
            "",
            "                first_value",
            "",
            "                second_value",
        ]
        .repeat(20)
        .join("\n"),
    );
    let buffer = render(&editor, 0, 12, 20);
    // Rows 1 and 3 in the source group occupy separate braille pixels.
    assert_eq!(buffer[(4, 10)].symbol(), "⣒");
    assert_eq!(buffer[(0, 10)].symbol(), " ");
}

#[test]
fn smooth_scroll_keeps_groups_stable_and_existing_rows_translate_without_resampling() {
    let text = (0..4_000)
        .map(|line| {
            format!(
                "{}{}",
                " ".repeat((line % 7) * 8),
                match line % 4 {
                    0 => "fn update_value() {",
                    1 => "let result = 123;",
                    2 => "}",
                    _ => "",
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut editor = Editor::test_with_text(&text);
    editor.scroll_past_end = false;
    let mut previous: Option<(usize, Buffer)> = None;
    for scroll in 0..100 {
        let layout = MinimapLayout::new(editor.visible_line_count(), scroll, 24, 24, false);
        let frame = render(&editor, scroll, 12, 24);
        assert_eq!(layout.window.start % 4, 0);
        if let Some((old_start, old_frame)) = &previous {
            let shift = (layout.window.start - old_start) / 4;
            for row in 8..24 - shift {
                for column in 0..12 {
                    assert_eq!(
                        frame[(column, row as u16)],
                        old_frame[(column, (row + shift) as u16)],
                        "scroll={scroll}, source shift={shift}, row={row}"
                    );
                }
            }
        }
        previous = Some((layout.window.start, frame));
    }
}

#[test]
fn viewport_stays_inside_the_preview_at_file_edges_and_small_heights() {
    for total in [1_usize, 7, 80, 121, 1_025] {
        for visible in [1_usize, 17, 40, 80] {
            for height in [1, 2, 3, 7, 24, 81] {
                for past_end in [false, true] {
                    let max_scroll = if past_end {
                        total - 1
                    } else {
                        total.saturating_sub(visible)
                    };
                    let mut previous = 0;
                    for scroll in 0..=max_scroll {
                        let layout = MinimapLayout::new(total, scroll, visible, height, past_end);
                        assert!(layout.window.start <= scroll);
                        assert!(layout.window.end >= scroll + layout.visible);
                        assert!(layout.window.start >= previous);
                        assert!(layout.view_start < layout.view_end);
                        assert!(layout.view_end <= height);
                        previous = layout.window.start;
                    }
                }
            }
        }
    }
}

#[test]
fn cached_bands_warm_once_and_only_decode_new_lines_while_scrolling() {
    let rope = ropey::Rope::from_str(&"                let value = 123;\n".repeat(2_000));
    let mut cache = MinimapCache::default();
    cache.synchronize(&rope, 12, 4);
    let samples = |start| minimap_samples(start, start + 4);
    for start in (0..240).step_by(4) {
        cache.band(&rope, samples(start));
    }
    let builds = cache.builds;
    for _ in 0..10 {
        for start in (0..240).step_by(4) {
            cache.band(&rope, samples(start));
        }
    }
    assert_eq!(cache.builds, builds);
    for start in (4..244).step_by(4) {
        cache.band(&rope, samples(start));
    }
    assert_eq!(cache.builds - builds, 4);
}

#[test]
#[ignore = "manual minimap visual preview; set VTCODE_THEME_SNAPSHOTS"]
fn preview_minimap_indentation_and_scroll() {
    let deep = (0..4_000)
        .map(|line| {
            format!(
                "{}{}",
                " ".repeat(64 + line % 4 * 4),
                match line % 8 {
                    0 => "if request.is_ready() {",
                    1 => "let response = send_request();",
                    2 => "update_progress(response);",
                    3 => "}",
                    4 => "",
                    5 => "return result;",
                    6 => "log_result(result);",
                    _ => "}",
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mixed = (0..4_000)
        .map(|line| {
            format!(
                "{}{}",
                " ".repeat(line % 5 * 4),
                match line % 7 {
                    0 => "fn handle_request() {",
                    1 => "let value = 123;",
                    2 => "return cached_value;",
                    3 => "}",
                    4 => "",
                    5 => "// next block",
                    _ => "}",
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    for (theme, name) in [
        (crate::app_theme::EditorTheme::Dark, "dark"),
        (crate::app_theme::EditorTheme::Light, "light"),
    ] {
        let colors = theme.colors();
        let mut deep = Editor::test_with_text(&deep);
        let mut mixed = Editor::test_with_text(&mixed);
        deep.scroll_past_end = false;
        mixed.scroll_past_end = false;
        for step in 0..12 {
            let scroll = 1_500 + step;
            let mut terminal = Terminal::new(TestBackend::new(92, 30)).unwrap();
            terminal
                .draw(|frame| {
                    let area = frame.area();
                    frame.buffer_mut().set_style(
                        area,
                        Style::default().fg(colors.fg_text).bg(colors.bg_editor),
                    );
                    frame.render_widget(
                        Paragraph::new("Minimap: deep indentation and scrolling")
                            .style(Style::default().fg(colors.fg_text)),
                        Rect::new(3, 1, 86, 1),
                    );
                    for (x, label, editor, legacy) in [
                        (6, "Deep / before", &deep, true),
                        (28, "Deep / after", &deep, false),
                        (50, "Mixed / before", &mixed, true),
                        (72, "Mixed / after", &mixed, false),
                    ] {
                        frame.render_widget(Paragraph::new(label), Rect::new(x - 1, 3, 19, 1));
                        let area = Rect::new(x, 5, 12, 23);
                        if legacy {
                            draw_legacy(frame, editor, scroll, area, colors);
                        } else {
                            draw_minimap(
                                frame,
                                MinimapParams {
                                    area,
                                    editor,
                                    scroll,
                                    visible_lines: 23,
                                    search_matches: &[],
                                    diagnostic_lines: &[],
                                    diagnostic_index: None,
                                    change_index: None,
                                    diff_changes: None,
                                    colors,
                                    hovered: false,
                                    active: false,
                                },
                            );
                        }
                    }
                })
                .unwrap();
            crate::ui_control_visibility_tests::snapshot(
                terminal.backend().buffer(),
                &format!("minimap-{name}-{step:02}"),
            );
        }
    }
}

fn draw_legacy(
    frame: &mut Frame,
    editor: &Editor,
    scroll: usize,
    area: Rect,
    colors: crate::app_theme::ThemeColors,
) {
    let visible = area.height as usize;
    let len = visible * 3;
    let max_scroll = editor.line_count() - visible;
    let start = scroll * (editor.line_count() - len) / max_scroll;
    let top = scroll * (visible - visible.div_ceil(3)) / max_scroll;
    for row in 0..visible {
        let bg = if (top..top + visible.div_ceil(3)).contains(&row) {
            colors.ui.bg_minimap_slider
        } else {
            colors.ui.bg_minimap
        };
        let line = editor.rope.line(start + row * 3);
        let chars: Vec<_> = line.chars().take(area.width as usize * 2).collect();
        for column in 0..area.width as usize {
            let occupied = chars
                .get(column * 2..(column * 2 + 2).min(chars.len()))
                .is_some_and(|pair| pair.iter().any(|ch| !ch.is_whitespace()));
            let cell = &mut frame.buffer_mut()[(area.x + column as u16, area.y + row as u16)];
            cell.set_char(if occupied { '▪' } else { ' ' })
                .set_fg(colors.fg_dim)
                .set_bg(bg);
        }
    }
    draw_minimap_viewport_border(frame, area, top, top + visible.div_ceil(3), false, &colors);
}
