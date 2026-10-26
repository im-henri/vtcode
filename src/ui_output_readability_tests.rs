use super::*;
use crate::{app_theme::EditorTheme, theme_workbench::ColorLayer};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, style::Color};

fn paint_colors(
    reader: &mut Reader,
    log: &Log,
    width: u16,
    height: u16,
    colors: &crate::app_theme::ThemeColors,
) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    loop {
        terminal
            .draw(|frame| {
                reader.draw(
                    frame,
                    colors,
                    frame.area(),
                    true,
                    (0, 0),
                    Some(log),
                    Source::RemoteSsh,
                )
            })
            .unwrap();
        if reader.view.version == log.version() {
            break;
        }
    }
    terminal.backend().buffer().clone()
}
fn paint(reader: &mut Reader, log: &Log, width: u16, height: u16) -> Buffer {
    paint_colors(reader, log, width, height, &EditorTheme::Dark.colors())
}
fn pointer(reader: &mut Reader, kind: MouseEventKind, x: u16, y: u16) {
    assert!(reader.mouse(
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE
        },
        3
    ));
}

#[test]
fn timestamp_level_and_message_use_distinct_theme_colors_and_severity_is_explicit() {
    let log = Log::default();
    for source in ["Test", "Disconnected", "Launch failed"] {
        log.event(source, "message");
    }
    let mut reader = Reader::default();
    for theme in [EditorTheme::Dark, EditorTheme::Light] {
        let colors = theme.colors();
        let buffer = paint_colors(&mut reader, &log, 150, 8, &colors);
        let ink = super::super::styles::Ink::new(&colors, colors.ui.bg_panel);
        for (line, label) in [(0, "[info]"), (1, "[warning]"), (2, "[error]")] {
            let source = reader.editor.rope.line(line).to_string();
            let level = source.find(label).unwrap();
            let message = source.find("message").unwrap();
            let header = super::super::styles::Header::parse(&source);
            assert_eq!(buffer[(0, line as u16)].fg, ink.foreground(header, 0));
            assert_eq!(
                buffer[(level as u16, line as u16)].fg,
                ink.foreground(header, level)
            );
            assert_eq!(
                buffer[(message as u16, line as u16)].fg,
                ink.foreground(header, message)
            );
            assert_ne!(
                buffer[(0, line as u16)].fg,
                buffer[(message as u16, line as u16)].fg
            );
            assert_ne!(
                buffer[(level as u16, line as u16)].fg,
                buffer[(message as u16, line as u16)].fg
            );
        }
    }
}

#[test]
fn changing_theme_recolors_retained_output_without_rebuilding_its_wrap_layout() {
    let log = Log::default();
    log.event("Test", "message");
    let mut reader = Reader::default();
    paint(&mut reader, &log, 100, 8);
    let scans = reader.layout.scanned_lines;
    let mut colors = EditorTheme::Dark.colors();
    colors.ui.output_timestamp = ColorLayer::opaque(Color::Rgb(180, 140, 180));
    let buffer = paint_colors(&mut reader, &log, 100, 8, &colors);
    assert_eq!(buffer[(0, 0)].fg, Color::Rgb(180, 140, 180));
    assert_eq!(reader.layout.scanned_lines, scans);
}

#[test]
fn imported_theme_timestamp_color_matches_its_comment_scope() {
    for key in ["dark", "light", "monokai", "high-contrast"] {
        let theme = crate::app_theme_loader::builtin_theme(key).unwrap();
        let style = syntect::highlighting::Highlighter::new(theme.syntect_theme.as_ref().unwrap())
            .style_for_stack(&[syntect::parsing::Scope::new("comment").unwrap()]);
        let colors = theme.to_theme_colors();
        let color = style.foreground;
        assert_eq!(
            colors.ui.output_timestamp.over(colors.ui.bg_panel),
            ColorLayer::rgba(color.r, color.g, color.b, color.a).over(colors.ui.bg_panel)
        );
    }
}

#[test]
fn wrapping_preserves_every_character_and_never_splits_graphemes() {
    for text in [
        "",
        "abc def/path-with-words",
        "\t界👩‍💻e\u{301}\t abc",
        "xxxxxxxxxxxxxxxxxxxx",
        "a     b     c",
    ] {
        for width in 1..20 {
            let mut reconstructed = String::new();
            let mut previous = 0;
            for segment in layout::segments(text, width) {
                assert_eq!(segment.start, previous);
                assert!(segment.end > segment.start || text.is_empty());
                let part: String = text
                    .chars()
                    .skip(segment.start)
                    .take(segment.end - segment.start)
                    .collect();
                let start_byte = text
                    .char_indices()
                    .nth(segment.start)
                    .map_or(text.len(), |(byte, _)| byte);
                assert!(
                    start_byte == text.len()
                        || text
                            .grapheme_indices(true)
                            .any(|(byte, _)| byte == start_byte)
                );
                reconstructed.push_str(&part);
                previous = segment.end;
            }
            assert_eq!(reconstructed, text, "width {width}");
        }
    }
    assert_eq!(line_width("界\tX"), 5);
    assert_eq!(line_width("ab\t\tX"), 9);
}

#[test]
fn wrapped_selection_copies_original_text_without_inserted_newlines_or_padding() {
    let log = Log::default();
    log.event("Test", &"alpha beta gamma delta ".repeat(20));
    let mut reader = Reader::default();
    paint(&mut reader, &log, 45, 8);
    assert!(reader.wrap && reader.horizontal_bar.is_none());
    reader.view.follow = false;
    reader.scroll_to(0);
    paint(&mut reader, &log, 45, 8);
    let start = reader.position_on_row(1, 3);
    let end = reader.position_on_row(3, 9);
    pointer(&mut reader, MouseEventKind::Down(MouseButton::Left), 3, 1);
    pointer(&mut reader, MouseEventKind::Drag(MouseButton::Left), 9, 3);
    pointer(&mut reader, MouseEventKind::Up(MouseButton::Left), 9, 3);
    assert_eq!(start.0, end.0);
    assert_eq!(
        reader.selected_text().unwrap(),
        reader
            .editor
            .rope
            .line(start.0)
            .slice(start.1..end.1)
            .to_string()
    );
    assert!(!reader.selected_text().unwrap().contains('\n'));
}

#[test]
fn vertical_keys_and_wheel_scroll_by_visual_rows() {
    let log = Log::default();
    log.event("Test", &"alpha beta gamma delta ".repeat(40));
    let mut reader = Reader::default();
    paint(&mut reader, &log, 40, 6);
    reader.view.follow = false;
    reader.scroll_to(0);
    reader.editor.set_cursor_without_scrolling(0, 0);
    reader.key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
    assert_eq!(reader.editor.cursor_line, 0);
    assert_eq!(reader.editor.cursor_col, reader.row_at(1).unwrap().1.start);
    assert_eq!(reader.editor.selection_anchor, Some((0, 0)));
    reader.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(
        (reader.editor.cursor_line, reader.editor.cursor_col),
        (0, 0)
    );
    pointer(&mut reader, MouseEventKind::ScrollDown, 3, 1);
    assert_eq!(reader.offset(), 3);
    assert_eq!(
        reader.editor.scroll_offset, 0,
        "these are continuations of one logical line"
    );
}

#[test]
fn wrapped_find_reveals_matches_and_selection_keeps_source_coordinates() {
    let log = Log::default();
    log.event(
        "Test",
        &format!(
            "{} needle {} needle",
            "word ".repeat(40),
            "more ".repeat(40)
        ),
    );
    let mut reader = Reader::default();
    paint(&mut reader, &log, 45, 8);
    reader.open_find();
    paint(&mut reader, &log, 45, 8);
    for character in "needle".chars() {
        reader.key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }
    assert_eq!(reader.editor.selected_text().as_deref(), Some("needle"));
    let first = reader.editor.cursor_col;
    reader.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(reader.editor.cursor_col > first);
    reader.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    assert_eq!(reader.editor.cursor_col, first);
    let row = reader.layout.row_for_position(
        &reader.editor,
        reader.editor.cursor_line,
        reader.editor.cursor_col,
    );
    assert!((reader.offset()..reader.offset() + reader.text.height as usize).contains(&row));
}

#[test]
fn wrap_toggle_and_resize_preserve_the_buffer_selection_and_reading_anchor() {
    let log = Log::default();
    log.event("Test", &"alpha beta gamma delta ".repeat(50));
    let mut reader = Reader::default();
    paint(&mut reader, &log, 45, 8);
    reader.view.follow = false;
    reader.scroll_to(3);
    let anchor = reader.row_at(reader.offset()).unwrap();
    reader.editor.set_cursor_without_scrolling(0, 80);
    reader.editor.selection_anchor = Some((0, 70));
    let selection = reader.editor.selected_text();
    paint(&mut reader, &log, 60, 8);
    let resized = reader.row_at(reader.offset()).unwrap();
    assert!(resized.1.start <= anchor.1.start && anchor.1.start <= resized.1.end);
    reader.toggle_wrap();
    paint(&mut reader, &log, 60, 8);
    assert!(reader.horizontal_bar.is_some());
    assert_eq!(reader.editor.selected_text(), selection);
    reader.toggle_wrap();
    paint(&mut reader, &log, 60, 8);
    assert!(reader.wrap && reader.horizontal_bar.is_none() && reader.horizontal == 0);
    assert_eq!(reader.editor.selected_text(), selection);
}

#[test]
fn appends_only_lay_out_new_lines_and_unchanged_paints_reuse_the_layout() {
    let log = Log::default();
    for index in 0..300 {
        log.event("Test", &format!("{index} {}", "word ".repeat(20)));
    }
    let mut reader = Reader::default();
    paint(&mut reader, &log, 50, 8);
    let scanned = reader.layout.scanned_lines;
    for _ in 0..10 {
        paint(&mut reader, &log, 50, 8);
    }
    assert_eq!(reader.layout.scanned_lines, scanned);
    log.event("Test", "one new line");
    paint(&mut reader, &log, 50, 8);
    assert_eq!(reader.layout.scanned_lines - scanned, 2);
}

#[test]
fn retention_removes_wrap_prefixes_without_relaying_out_retained_lines() {
    let log = Log::default();
    for _ in 0..1024 {
        log.event("Test", &"x".repeat(1024));
    }
    let mut reader = Reader::default();
    paint(&mut reader, &log, 60, 8);
    reader.view.follow = false;
    reader.editor.set_cursor_without_scrolling(500, 40);
    reader.editor.selection_anchor = Some((500, 30));
    reader.scroll_to(reader.layout.row_for_line(500));
    let scanned = reader.layout.scanned_lines;
    let selection = reader.editor.selected_text();
    log.event("Test", &"y".repeat(1024));
    paint(&mut reader, &log, 60, 8);
    assert_eq!(reader.view.omitted, 1);
    assert_eq!(reader.editor.scroll_offset, 499);
    assert_eq!(reader.editor.selected_text(), selection);
    assert_eq!(reader.layout.scanned_lines - scanned, 2);
}
