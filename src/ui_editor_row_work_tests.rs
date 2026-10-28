use super::*;
use crate::{app::Tab, editor::Editor};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn reference(enabled: bool) {
    REFERENCE_ROW_PREPARATION.set(enabled);
    REFERENCE_EDITOR_CHROME.set(enabled);
}

fn fixture(pattern: &str, repetitions: usize) -> App {
    let line = pattern.repeat(repetitions);
    let mut editor = Editor::test_with_text(&format!("{line}\r\n{line}\nshort\n"));
    editor.word_wrap = true;
    editor.file_path = Some("row-work.rs".into());
    editor.cursor_col = 9;
    editor.selection_anchor = Some((0, 3));
    editor.extra_cursors = (0..line.chars().count()).step_by(29).map(|col| (0, col)).collect();
    editor.extra_cursor_anchors = editor.extra_cursors.iter().map(|&(row, col)| Some((row, col.saturating_sub(2)))).collect();
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.sticky_scroll = false;
    app.show_minimap = false;
    app.show_semantic_tokens = true;
    app.lsp_manager.semantic_token_legend = Some(lsp_types::SemanticTokensLegend {
        token_types: vec![lsp_types::SemanticTokenType::VARIABLE, lsp_types::SemanticTokenType::FUNCTION],
        token_modifiers: vec![],
    });
    app.semantic_theme_colors = Some(crate::semantic_theme::SemanticTheme::from_iter([
        ("variable".into(), (40, 210, 90)), ("function".into(), (210, 40, 90)),
    ]));
    for row in 0..2 {
        for col in (0..line.chars().count()).step_by(2) {
            app.lsp_semantic_tokens.push(crate::lsp::DecodedSemanticToken {
                line: row, start_char: col as u32, length: if col % 17 == 0 { 7 } else { 1 },
                token_type: (col % 3) as u32, _token_modifiers: 0,
            });
            app.search_matches.push((row as usize, col, col + 1));
        }
        app.search_matches.push((row as usize, line.chars().count(), line.chars().count() + 1));
    }
    app
}

fn draw(app: &mut App, area: Rect, original: bool) -> Buffer {
    reference(original);
    let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
    terminal.draw(|frame| draw_editor(frame, app, area)).unwrap();
    reference(false);
    terminal.backend().buffer().clone()
}

#[test]
fn wrapped_continuation_work_preserves_semantic_overlaps_search_eol_and_cursor_pixels() {
    for pattern in ["a b(c) ", "界 e\u{301}\t#ff0000 ", "\0\x1b x 👩‍👩‍👧‍👦 "] {
        let mut app = fixture(pattern, 32);
        for width in [9, 21, 80, 180] {
            for scroll in [0, 1, 3, 19, 100] {
                for mode in [WhitespaceMode::None, WhitespaceMode::Boundary, WhitespaceMode::Selection] {
                    let editor = app.editor_mut().unwrap();
                    editor.wrap_scroll_offset = scroll;
                    editor.word_wrap_column = 13;
                    editor.whitespace_mode = mode;
                    let area = Rect::new(0, 0, width, 15);
                    let actual = draw(&mut app, area, false);
                    let expected = draw(&mut app, area, true);
                    assert_eq!(actual, expected, "pattern={pattern:?}, width={width}, scroll={scroll}, mode={mode:?}");
                }
            }
        }
    }
}

fn chrome_frame(area: Rect, total: usize, visible: usize, scroll: usize, hovered: bool, active: bool, original: bool, offset: bool) -> Buffer {
    reference(original);
    let options = ratatui::TerminalOptions {
        viewport: if offset { ratatui::Viewport::Fixed(Rect::new(4, 2, 40, 8)) } else { ratatui::Viewport::Fullscreen },
    };
    let mut terminal = Terminal::with_options(TestBackend::new(48, 12), options).unwrap();
    let colors = crate::app_theme::EditorTheme::Dark.colors();
    terminal.draw(|frame| {
        let bounds = frame.area();
        frame.buffer_mut().set_style(bounds, Style::default().fg(Color::Cyan).bg(Color::Magenta)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED).underline_color(Color::Red));
        for y in bounds.y..bounds.bottom() {
            frame.buffer_mut().set_string(bounds.x, y, "界 occupied cells", Style::default());
        }
        draw_horizontal_scrollbar(frame, HScrollbarParams {
            area, total_width: total, visible_width: visible, scroll,
            bg_editor: colors.bg_editor, hovered, active, colors: &colors.ui,
        });
        draw_minimap_separator(frame, Rect::new(area.x.saturating_add(1), area.y, area.width, area.height), Color::Yellow, Color::Blue);
    }).unwrap();
    reference(false);
    terminal.backend().buffer().clone()
}

#[test]
fn direct_editor_chrome_matches_original_over_existing_styled_cells_and_clipped_rectangles() {
    for area in [Rect::new(0, 0, 0, 0), Rect::new(0, 0, 1, 1), Rect::new(0, 0, 24, 1),
        Rect::new(3, 2, 36, 4), Rect::new(44, 9, 12, 6), Rect::new(60, 3, 8, 2)] {
        for (total, visible, scroll) in [(0, 0, 0), (24, 24, 0), (1000, 50, 0), (1000, 50, 511), (1000, 50, 2000)] {
            for (hovered, active) in [(false, false), (true, false), (true, true)] {
                for offset in [false, true] {
                    assert_eq!(chrome_frame(area, total, visible, scroll, hovered, active, false, offset),
                        chrome_frame(area, total, visible, scroll, hovered, active, true, offset),
                        "area={area:?}, total={total}, visible={visible}, scroll={scroll}, hovered={hovered}, active={active}, offset={offset}");
                }
            }
        }
    }
}

#[test]
#[ignore = "manual release comparative wrapped-row benchmark"]
fn benchmark_wrapped_row_preparation_and_editor_chrome() {
    use std::{hint::black_box, time::Instant};
    for case in ["deep_wrapped_tokens_and_search", "ordinary_wrapped_lines", "horizontal_scrollbar_and_separator"] {
        let mut app = match case {
            "deep_wrapped_tokens_and_search" => fixture("a ", 7000),
            _ => fixture("fn example() { x() } ", 4),
        };
        app.cursor_visible = true;
        let area = Rect::new(0, 0, 180, 60);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        let colors = app.colors;
        let mut render = |original: bool, terminal: &mut Terminal<TestBackend>| {
            reference(original);
            if case == "horizontal_scrollbar_and_separator" {
                terminal.draw(|frame| {
                    draw_horizontal_scrollbar(frame, HScrollbarParams {
                        area: Rect::new(0, 59, 179, 1), total_width: 100_000, visible_width: 179, scroll: 40_000,
                        bg_editor: colors.bg_editor, hovered: true, active: false, colors: &colors.ui,
                    });
                    draw_minimap_separator(frame, Rect::new(179, 0, 1, 60), colors.fg_dim, colors.bg_editor);
                }).unwrap();
            } else {
                if case == "deep_wrapped_tokens_and_search" {
                    app.editor_mut().unwrap().wrap_scroll_offset = 55;
                }
                terminal.draw(|frame| draw_editor(frame, &mut app, area)).unwrap();
            }
        };
        render(false, &mut terminal);
        let expected = terminal.backend().buffer().clone();
        let mut samples = [Vec::new(), Vec::new()];
        for round in 0..9 {
            for original in if round % 2 == 0 { [false, true] } else { [true, false] } {
                let started = Instant::now();
                for _ in 0..10 { render(original, &mut terminal); black_box(terminal.backend().buffer()); }
                samples[usize::from(original)].push(started.elapsed() / 10);
                assert_eq!(terminal.backend().buffer(), &expected, "case={case}, original={original}");
            }
        }
        reference(false);
        for sample in &mut samples { sample.sort(); }
        eprintln!("editor row work case={case} original={:?} optimized={:?}", samples[1][4], samples[0][4]);
    }
}
