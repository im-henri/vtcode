use super::*;
use crate::{animation_config::AnimationEffect, app::{ActivePane, Tab}, editor::Editor};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

const SOURCE: &str = "fn calculate(input: usize) -> usize {\n    let measurements = input + 17;\n    // Ordinary comment with repeated  spaces and a tab\t\n    /* comment */ let output = measurements * 3;\n    println!(\"café 界 e\u{301} 👩‍💻\", output);\n    output\n}\n\n";

fn fixture(source: &str, syntax: bool, animations: bool) -> App {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text(source);
    editor.file_path = Some("plain-run-render.rs".into());
    editor.word_wrap = false;
    editor.cursor_line = 1;
    editor.cursor_col = 4;
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.show_explorer = false;
    app.show_minimap = false;
    app.sticky_scroll = false;
    app.cursor_blinking = false;
    for effect in AnimationEffect::ALL { app.animation_settings.set(effect, false); }
    if animations { app.animation_settings.set(AnimationEffect::ScopeConnector, true); }
    if syntax {
        let editor = app.editor().unwrap();
        let rope = editor.rope.clone();
        let path = editor.file_path.clone().unwrap();
        let job = app.highlighter.prepare_job(&rope, &path, 0, None,
            0..rope.len_lines(), None, app.syntax_theme.as_str()).unwrap();
        let (syntaxes, themes) = app.highlighter.shared_sets();
        assert!(app.highlighter.apply_batch(job.run(&rope, &syntaxes, &themes, || false)));
    }
    app
}

fn draw(app: &mut App, terminal: &mut Terminal<TestBackend>, reference: bool) -> Buffer {
    plain_runs::REFERENCE.set(reference);
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    plain_runs::REFERENCE.set(false);
    terminal.backend().buffer().clone()
}

fn compare(app: &mut App, terminal: &mut Terminal<TestBackend>, label: &str) {
    let expected = draw(app, terminal, true);
    let actual = draw(app, terminal, false);
    assert_eq!(actual, expected, "{label}");
}

#[test]
fn decorated_runs_preserve_overlapping_ranges_whitespace_and_unicode_boundaries() {
    let source = "    let alpha = beta + alpha; // TODO: alpha FIXME\n\té界 é alpha  beta\t\r\n".repeat(20);
    let mut app = fixture(&source, true, false);
    app.lsp_document_highlights = (0..30).flat_map(|line| [(line, 4, line, 13), (line, 18, line, 40)]).collect();
    app.lsp_manager.diagnostics.insert("plain-run-render.rs".into(), (0..30).flat_map(|line| {
        [(1, 17, lsp_types::DiagnosticSeverity::WARNING), (12, 35, lsp_types::DiagnosticSeverity::ERROR)]
            .into_iter().map(move |(col, end_col, severity)| crate::lsp::LspDiagnostic {
                line, col, end_line: line, end_col, message: "test".into(), severity,
            })
    }).collect());
    app.lsp_manager.diagnostics_version += 1;
    app.search_matches = (0..30).flat_map(|line| [(line, 4, 11), (line, 9, 24), (line, 35, 35)]).collect();
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    for mode in [WhitespaceMode::None, WhitespaceMode::All, WhitespaceMode::Boundary, WhitespaceMode::Selection] {
        for width in [1, 8, 60, 100] {
            terminal.backend_mut().resize(width, 30);
            terminal.autoresize().unwrap();
            for scroll in [0, 5, 11, 17, 33] {
                for selection in [None, Some((2, 8)), Some((5, 14))] {
                    let editor = app.editor_mut().unwrap();
                    editor.whitespace_mode = mode;
                    editor.cursor_line = 4;
                    editor.cursor_col = 10;
                    editor.selection_anchor = selection;
                    editor.horizontal_scroll = scroll;
                    app.search_current = scroll;
                    compare(&mut app, &mut terminal, "overlapping range runs");
                }
            }
        }
    }
}

#[test]
fn unwrapped_runs_preserve_every_whitespace_mode_selection_and_horizontal_clipping() {
    for syntax in [false, true] {
        let mut app = fixture(&SOURCE.repeat(8), syntax, false);
        let mut terminal = Terminal::new(TestBackend::new(96, 30)).unwrap();
        plain_runs::CELLS.set(0);
        compare(&mut app, &mut terminal, "ordinary source");
        assert!(plain_runs::CELLS.get() > 100, "ordinary text never used the run renderer");
        for mode in [WhitespaceMode::None, WhitespaceMode::All, WhitespaceMode::Boundary,
            WhitespaceMode::Selection, WhitespaceMode::Trailing] {
            for comment in [None, Some(WhitespaceMode::All), Some(WhitespaceMode::Boundary)] {
                for scroll in [0, 5, 18, 43] {
                    for selection in [None, Some((1, 9)), Some((0, 2)), Some((5, 1))] {
                        app.comment_whitespace_mode = comment;
                        app.show_indent_guides = scroll % 2 == 0;
                        app.indent_rainbow = scroll % 3 == 0;
                        let editor = app.editor_mut().unwrap();
                        editor.whitespace_mode = mode;
                        editor.cursor_line = 3;
                        editor.cursor_col = 22;
                        editor.selection_anchor = selection;
                        editor.horizontal_scroll = scroll;
                        compare(&mut app, &mut terminal, "whitespace / selection / scroll");
                    }
                }
            }
        }
    }
}

#[test]
fn unwrapped_runs_keep_dynamic_decorations_and_cursor_layers() {
    let mut app = fixture(&SOURCE.repeat(5), true, false);
    let mut terminal = Terminal::new(TestBackend::new(92, 26)).unwrap();
    for case in 0..9 {
        match case {
            0 => { app.search_active = true; app.search_matches = vec![(0, 3, 12), (2, 4, 90)]; }
            1 => { app.search_active = false; app.search_matches.clear(); app.cached_matching_bracket = Some((6, 0)); }
            2 => { app.cached_matching_bracket = None; app.editor_mut().unwrap().extra_cursors = vec![(2, 8), (4, 20)]; }
            3 => { app.editor_mut().unwrap().extra_cursor_anchors = vec![Some((0, 1)), Some((4, 2))]; }
            4 => { app.editor_mut().unwrap().extra_cursors.clear(); app.editor_mut().unwrap().extra_cursor_anchors.clear(); app.cursor_visible = false; }
            5 => { app.rendering_inactive_editor = true; }
            6 => { app.rendering_inactive_editor = false; app.cursor_visible = true; app.focus_mode = true; }
            7 => { app.focus_mode = false; app.editor_mut().unwrap().folded_ranges.insert(0, 6); }
            _ => { app.editor_mut().unwrap().folded_ranges.clear(); app.bracket_pair_colorization = false; }
        }
        compare(&mut app, &mut terminal, "dynamic overlays");
    }
}

#[test]
fn unwrapped_runs_preserve_input_frames_edits_undo_and_resize() {
    let mut app = fixture(&SOURCE.repeat(15), true, false);
    let mut terminal = Terminal::new(TestBackend::new(130, 36)).unwrap();
    let events = [KeyCode::Down, KeyCode::End, KeyCode::Left, KeyCode::Char('x'),
        KeyCode::Char(' '), KeyCode::Char('界'), KeyCode::Backspace, KeyCode::Enter,
        KeyCode::Tab, KeyCode::Char('('), KeyCode::Char(')'), KeyCode::Up, KeyCode::Home];
    for index in 0..80 {
        let key = KeyEvent::new(events[index % events.len()], KeyModifiers::NONE);
        crate::input_keyboard::handle_keyboard_event(&mut app, key);
        compare(&mut app, &mut terminal, "input frame");
        if index % 9 == 0 {
            app.editor_mut().unwrap().undo();
            compare(&mut app, &mut terminal, "undo frame");
        }
        if index % 11 == 0 {
            let width = [28, 72, 130][index % 3];
            terminal.backend_mut().resize(width, 36);
            terminal.autoresize().unwrap();
            compare(&mut app, &mut terminal, "resize frame");
        }
    }
}

#[test]
fn unwrapped_runs_stop_at_semantic_tokens_diagnostics_and_document_highlights() {
    let mut app = fixture(&SOURCE.repeat(5), true, false);
    app.show_semantic_tokens = true;
    let (client, _server) = crate::lsp::LspClient::test_pair("rust", &["*.rs"]);
    app.lsp_manager.clients.push(client);
    app.lsp_manager.semantic_token_legend = Some(lsp_types::SemanticTokensLegend {
        token_types: vec![lsp_types::SemanticTokenType::VARIABLE], token_modifiers: vec![],
    });
    app.semantic_theme_colors = Some(crate::semantic_theme::SemanticTheme::from_iter([("variable".into(), (15, 140, 230))]));
    let mut terminal = Terminal::new(TestBackend::new(110, 30)).unwrap();
    for line in 0..8 {
        for (start, length) in [(0, 2), (3, 7), (11, 0), (12, 1), (13, 14), (40, 5)] {
            app.lsp_semantic_tokens.push(crate::lsp::DecodedSemanticToken {
                line, start_char: start, length, token_type: 0, _token_modifiers: 0,
            });
        }
    }
    compare(&mut app, &mut terminal, "semantic token boundaries");
    let code = app.code_area.unwrap();
    assert_eq!(terminal.backend().buffer()[(code.x + 6, code.y)].fg, Color::Rgb(15, 140, 230));
    app.semantic_theme_colors = Some(crate::semantic_theme::SemanticTheme::from_iter([("variable".into(), (180, 150, 75))]));
    compare(&mut app, &mut terminal, "semantic theme change");
    assert_eq!(terminal.backend().buffer()[(code.x + 6, code.y)].fg, Color::Rgb(180, 150, 75));
    app.lsp_document_highlights = vec![(0, 3, 0, 12), (1, 8, 1, 20)];
    app.lsp_manager.diagnostics.insert("plain-run-render.rs".into(), vec![crate::lsp::LspDiagnostic {
        line: 2, col: 5, end_line: 4, end_col: 18, message: "example diagnostic".into(),
        severity: lsp_types::DiagnosticSeverity::WARNING,
    }]);
    app.lsp_manager.diagnostics_version += 1;
    for scroll in [0, 1, 8, 13, 18] {
        app.editor_mut().unwrap().horizontal_scroll = scroll;
        compare(&mut app, &mut terminal, "LSP decorations");
    }
}

#[test]
fn unwrapped_runs_preserve_animation_source_geometry() {
    let mut app = fixture(&SOURCE.repeat(6), true, true);
    let mut terminal = Terminal::new(TestBackend::new(120, 32)).unwrap();
    draw(&mut app, &mut terminal, true);
    app.animations.advance_frame(&app.animation_settings,
        std::time::Instant::now() + std::time::Duration::from_secs(6));
    for scroll in [0, 1, 4, 15] {
        app.editor_mut().unwrap().scroll_offset = scroll;
        compare(&mut app, &mut terminal, "animation geometry");
    }
}

#[test]
fn single_column_unicode_uses_runs_without_changing_glyph_clusters_or_decorations() {
    let mut app = fixture(&format!("{}\n", "éλЯø".repeat(40)).repeat(30), false, false);
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    plain_runs::CELLS.set(0);
    compare(&mut app, &mut terminal, "one-column Unicode uses batched styles");
    assert!(plain_runs::CELLS.get() > 500);

    let source = "fn café(τιμή: usize) {\n    // описание éλЯ  é  👩‍💻 🇪🇪 界 \t\n    let größe = \"éλЯ\u{85}\u{a0}e\u{301}\"; // TODO\n}\n".repeat(12);
    for syntax in [false, true] {
        let mut app = fixture(&source, syntax, true);
        draw(&mut app, &mut terminal, true);
        app.animations.advance_frame(&app.animation_settings,
            std::time::Instant::now() + std::time::Duration::from_secs(6));
        app.search_matches = (0..30).map(|line| (line, 5, 13)).collect();
        app.lsp_document_highlights = (0..30).map(|line| (line, 15, line, 21)).collect();
        for mode in [WhitespaceMode::None, WhitespaceMode::All, WhitespaceMode::Selection] {
            for scroll in [0, 7, 19, 28] {
                let editor = app.editor_mut().unwrap();
                editor.whitespace_mode = mode;
                editor.horizontal_scroll = scroll;
                editor.cursor_col = 26;
                editor.selection_anchor = Some((0, 4));
                compare(&mut app, &mut terminal, "Unicode styles, selection, clipping and animation");
            }
        }
    }
}

#[test]
#[ignore = "manual complete-frame comparison with the previous ASCII-only run path"]
fn benchmark_international_source_frames() {
    use std::{hint::black_box, time::Instant};
    let source = "fn café(τιμή: usize) {\n    // Änderungsgröße regelmäßig prüfen: описание результата, επιστροφή τιμής.\n    let αποτέλεσμα = τιμή + 17;\n    // fenêtre ouverte, Größenänderung, обработка данных и завершение.\n}\n".repeat(400);
    for case in ["cursor", "selection", "scroll"] {
        let mut samples = [Vec::new(), Vec::new()];
        for round in 0..7 {
            let mut outputs = Vec::new();
            for ascii_only in [round % 2 == 0, round % 2 != 0] {
                plain_runs::ASCII_ONLY.set(ascii_only);
                let mut app = fixture(&source, true, false);
                let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
                draw(&mut app, &mut terminal, false);
                let start = Instant::now();
                for index in 0..100 {
                    let editor = app.editor_mut().unwrap();
                    if case == "scroll" {
                        editor.scroll_offset = index;
                    } else {
                        editor.cursor_line = index % 30;
                        editor.cursor_col = 14;
                        editor.selection_anchor = (case == "selection").then_some((0, 3));
                    }
                    terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
                    black_box(terminal.backend().buffer());
                }
                samples[usize::from(ascii_only)].push(start.elapsed() / 100);
                outputs.push(terminal.backend().buffer().clone());
            }
            assert_eq!(outputs[0], outputs[1], "international {case}");
        }
        plain_runs::ASCII_ONLY.set(false);
        for values in &mut samples { values.sort(); }
        eprintln!("international {case} full frame: ASCII-only={:?}, Unicode runs={:?}", samples[1][3], samples[0][3]);
    }
}

#[test]
#[ignore = "manual optimized-profile unwrapped input + full UI benchmark"]
fn benchmark_unwrapped_input_frames() {
    use std::time::Instant;
    // Real, ordinary Rust lines: no minified input, wrapping or giant tokens.
    let source = include_str!("editor_text_ops.rs");
    for animations in [false, true] {
        for case in ["cursor", "selection", "scroll", "typing"] {
            let mut samples = [Vec::new(), Vec::new()];
            for round in 0..7 {
                let mut states = Vec::new();
                for reference in [round % 2 == 0, round % 2 != 0] {
                    let mut app = fixture(source, true, animations);
                    let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
                    app.editor_mut().unwrap().cursor_line = 130;
                    app.editor_mut().unwrap().scroll_offset = 115;
                    draw(&mut app, &mut terminal, reference);
                    plain_runs::REFERENCE.set(reference);
                    let started = Instant::now();
                    for index in 0..120 {
                        // The event loop favors input-triggered work only;
                        // release the scheduling hint between input frames.
                        let _input_priority = crate::worker_priority::InputPriority::new(!reference);
                        match case {
                            "scroll" => { app.editor_mut().unwrap().scroll_offset = 100 + index; }
                            _ => {
                                let code = match case {
                                    "typing" => match index % 4 { 0 => KeyCode::Char('x'), 1 => KeyCode::Char('y'), _ => KeyCode::Backspace },
                                    _ => if index % 4 < 2 { KeyCode::Down } else { KeyCode::Right },
                                };
                                crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::new(code,
                                    if case == "selection" { KeyModifiers::SHIFT } else { KeyModifiers::NONE }));
                            }
                        }
                        terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
                    }
                    samples[usize::from(reference)].push(started.elapsed() / 120);
                    plain_runs::REFERENCE.set(false);
                    // Separate instances run at different wall-clock times.
                    // Compare the final source scene without in-flight effects;
                    // geometry and settled effects have exact tests above.
                    for effect in AnimationEffect::ALL { app.animation_settings.set(effect, false); }
                    draw(&mut app, &mut terminal, reference);
                    let editor = app.editor().unwrap();
                    states.push((editor.rope.clone(), editor.cursor_line, editor.cursor_col,
                        editor.selection_anchor, terminal.backend().buffer().clone()));
                }
                assert_eq!(states[0], states[1], "case={case}, animations={animations}");
            }
            for samples in &mut samples { samples.sort(); }
            eprintln!("unwrapped {case} animations={animations}: old={:?}, new={:?}, speedup={:.2}x",
                samples[1][3], samples[0][3], samples[1][3].as_secs_f64() / samples[0][3].as_secs_f64());
        }
    }
}
