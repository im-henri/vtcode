use super::*;
use ratatui::{
    Terminal,
    backend::TestBackend,
    style::{Color, Modifier},
    text::Span,
};

#[test]
fn fused_cell_styles_preserve_wide_continuations_blank_tails_and_scalar_symbols() {
    let mut symbols = vec!["·", "→", "│", "界", "😀", "\u{301}", "\u{fe0f}", "\0", "\r", "\t", ""];
    let sampled: Vec<_> = (0..=0x10ffff).step_by(997).filter_map(char::from_u32).map(|ch| ch.to_string()).collect();
    symbols.extend(sampled.iter().map(String::as_str));
    for symbol in symbols {
        for width in [1, 2, 3, 7] {
            for origin in [0, 3] {
                let bounds = Rect::new(2, 2, 12, 4);
                let area = Rect::new(origin, 2, width, 3);
                let mut actual = Buffer::filled(bounds, ratatui::buffer::Cell::new("x"));
                actual.set_style(bounds, Style::default().fg(Color::Red).bg(Color::Green)
                    .add_modifier(Modifier::DIM | Modifier::ITALIC).underline_color(Color::Red));
                let mut expected = actual.clone();
                let base = Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD)
                    .remove_modifier(Modifier::ITALIC).underline_color(Color::Green);
                let line = Style::default().fg(Color::Yellow).add_modifier(Modifier::ITALIC)
                    .remove_modifier(Modifier::BOLD);
                let span = Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
                    .remove_modifier(Modifier::DIM).underline_color(Color::Blue);
                let lines = vec![Line::from(vec![Span::styled(symbol, span), Span::raw("a")]).style(line), Line::default()];
                paint(&mut actual, area, lines.clone(), base);
                Paragraph::new(lines).style(base).render(area, &mut expected);
                assert_eq!(actual, expected, "symbol={symbol:?}, width={width}, origin={origin}");
            }
        }
    }
}

#[test]
fn streamed_rows_match_paragraph_for_unicode_controls_styles_and_clipping() {
    let fragments = [
        "abc xyz",
        "\0\t\r\n\x7f",
        "界語",
        "e\u{301}",
        "\u{301}",
        "👩‍👩‍👧‍👦",
        "🇪🇪",
        "❤\u{fe0e}",
        "❤\u{fe0f}",
        "\u{200b}",
        " ",
        "→·│",
        "a\n界",
    ];
    for seed in 0..fragments.len() * 3 {
        let lines = (0..5)
            .map(|row| {
                Line::from(
                    (0..13)
                        .map(|i| {
                            Span::styled(
                                fragments[(seed + row * 3 + i * 7) % fragments.len()],
                                Style::default()
                                    .fg(Color::Indexed(i as u8))
                                    .add_modifier(if i % 2 == 0 {
                                        Modifier::BOLD
                                    } else {
                                        Modifier::ITALIC
                                    })
                                    .remove_modifier(Modifier::DIM),
                            )
                        })
                        .collect::<Vec<_>>(),
                )
                .style(Style::default().bg(Color::Blue))
            })
            .collect::<Vec<_>>();
        for width in [0, 1, 2, 3, 7, 19, 40] {
            let bounds = Rect::new(2, 3, 32, 8);
            for area in [
                Rect::new(4, 4, width, 3),
                Rect::new(0, 0, width, 5),
                Rect::new(30, 8, width, 8),
            ] {
                let mut actual = Buffer::filled(bounds, ratatui::buffer::Cell::new("x"));
                actual.set_style(
                    bounds,
                    Style::default().fg(Color::Red).add_modifier(Modifier::DIM),
                );
                let mut expected = actual.clone();
                let style = Style::default()
                    .bg(Color::Black)
                    .remove_modifier(Modifier::ITALIC);
                paint(&mut actual, area, lines.clone(), style);
                Paragraph::new(lines.clone())
                    .style(style)
                    .render(area, &mut expected);
                assert_eq!(actual, expected, "seed={seed}, area={area:?}");
            }
        }
    }
}

#[test]
fn aligned_rows_keep_the_general_layout_fallback() {
    let lines = [
        Line::from("right").right_aligned(),
        Line::from("界 centered").centered(),
    ];
    let area = Rect::new(0, 0, 21, 3);
    let mut actual = Buffer::empty(area);
    let mut expected = actual.clone();
    paint(&mut actual, area, lines.to_vec(), Style::default());
    Paragraph::new(lines.to_vec()).render(area, &mut expected);
    assert_eq!(actual, expected);
}

#[test]
fn editor_frames_match_layout_reference_while_editing_selecting_and_scrolling() {
    use crate::{
        app::{App, Tab},
        editor::Editor,
    };
    for wrapped in [false, true] {
        for bounded in [false, true] {
            let mut editor = Editor::test_with_text(
                &"\tfn café() { 界e\u{301}(); } // TODO #ff0000\r\n\t👩‍👩‍👧‍👦\tvalue;\n\n".repeat(30),
            );
            editor.word_wrap = wrapped;
            editor.large_file_mode = bounded;
            editor.file_path = Some("prepared.rs".into());
            let mut app = App::test_new();
            app.tabs = vec![Tab::new_file(editor)];
            app.active_tab = 0;
            app.show_minimap = false;
            app.sticky_scroll = false;
            for (step, width) in [9, 40, 120, 75, 40].into_iter().enumerate() {
                let editor = app.editor_mut().unwrap();
                editor.set_cursor(step * 3, step + 3);
                editor.insert_char('é');
                editor.selection_anchor = Some((step, 1));
                editor.scroll_down(step);
                editor.horizontal_scroll = if wrapped { 0 } else { step };
                let area = Rect::new(0, 0, width, 12);
                let render = |app: &mut App, reference| {
                    REFERENCE.set(reference);
                    let mut terminal = Terminal::new(TestBackend::new(width, 12)).unwrap();
                    terminal
                        .draw(|frame| super::super::draw_editor(frame, app, area))
                        .unwrap();
                    REFERENCE.set(false);
                    terminal.backend().buffer().clone()
                };
                let actual = render(&mut app, false);
                assert_eq!(
                    actual,
                    render(&mut app, true),
                    "wrap={wrapped}, bounded={bounded}, step={step}"
                );
            }
        }
    }
}

#[test]
#[ignore = "manual optimized-profile comparison"]
fn benchmark_prepared_editor_rows() {
    use std::{hint::black_box, time::Instant};
    for pattern in [
        "let result = values.iter().map(|value| value + 1); ",
        "界 e\u{301} café 👩‍👩‍👧‍👦 ",
    ] {
        let lines = (0..60)
            .map(|i| {
                Line::from(vec![
                    Span::styled(pattern.repeat(8), Style::default().fg(Color::Indexed(i))),
                    Span::raw(" ".repeat(180)),
                ])
            })
            .collect::<Vec<_>>();
        let area = Rect::new(0, 0, 180, 60);
        let mut buffer = Buffer::empty(area);
        let paragraph = Paragraph::new(lines.clone());
        let mut samples = [Vec::new(), Vec::new()];
        for sample in 0..7 {
            for reference in [sample % 2 == 0, sample % 2 != 0] {
                let now = Instant::now();
                for _ in 0..30 {
                    if reference {
                        (&paragraph).render(area, &mut buffer);
                    } else {
                        paint_rows(&mut buffer, area, &lines, Style::default());
                    }
                    black_box(&buffer);
                }
                samples[usize::from(reference)].push(now.elapsed() / 30);
            }
        }
        REFERENCE.set(false);
        samples.iter_mut().for_each(|s| s.sort());
        eprintln!(
            "prepared text ascii={} streamed={:?} Paragraph={:?}",
            pattern.is_ascii(),
            samples[0][3],
            samples[1][3]
        );
    }
}
