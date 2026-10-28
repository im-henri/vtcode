use crate::{
    app::{App, Tab},
    editor::Editor,
};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};

fn render_fixture(case: &str) -> App {
    let (source, wrapped) = match case {
        "unicode_occurrences" => (("ä ".repeat(4_096) + "\n").repeat(60), false),
        "wrapped_decorations" => (("value #ff0000 ".repeat(1_000) + "\n").repeat(3), true),
        "wrapped_unicode_occurrences" => (("ä #ff0000 ".repeat(1_000) + "\n").repeat(3), true),
        "unicode_todo_markers" => (("界 TODO ".repeat(1_000) + "\n").repeat(60), false),
        "blank_tail_folding" => (format!("fn sample() {{}}\n{}", "\n".repeat(50_000)), false),
        _ => panic!("unknown render fixture"),
    };
    let mut editor = Editor::test_with_text(&source);
    editor.word_wrap = wrapped;
    editor.file_path = Some(std::path::PathBuf::from("render-profile.css"));
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.show_minimap = false;
    app.show_color_preview = true;
    app.cursor_visible = false;
    app
}

#[test]
#[ignore = "manual release render benchmark"]
fn benchmark_rich_editor_rendering() {
    use std::{
        hash::{Hash, Hasher},
        hint::black_box,
        time::Instant,
    };
    for case in [
        "unicode_occurrences",
        "wrapped_decorations",
        "wrapped_unicode_occurrences",
        "unicode_todo_markers",
        "blank_tail_folding",
    ] {
        let mut app = render_fixture(case);
        let mut terminal = Terminal::new(TestBackend::new(180, 60)).unwrap();
        let area = Rect::new(0, 0, 180, 60);
        terminal
            .draw(|frame| super::draw_editor(frame, &mut app, area))
            .unwrap();
        let mut samples = Vec::new();
        for _ in 0..7 {
            let started = Instant::now();
            for _ in 0..5 {
                terminal
                    .draw(|frame| super::draw_editor(frame, &mut app, area))
                    .unwrap();
                black_box(terminal.backend().buffer());
            }
            samples.push(started.elapsed() / 5);
        }
        samples.sort();
        let mut digest = std::collections::hash_map::DefaultHasher::new();
        format!("{:?}", terminal.backend().buffer()).hash(&mut digest);
        eprintln!(
            "rich render case={case} median={:?} cells={:016x}",
            samples[3],
            digest.finish()
        );
    }
}

#[test]
fn occurrence_positions_preserve_unicode_boundaries_and_nonoverlapping_selection_matches() {
    use crate::ui_editor_highlight::{OccurrenceCursor, word_occurrences_for_line};
    let text = "ä ää _ä ä_ ä xä ä界 Ä ä";
    let mut ranges = Vec::new();
    word_occurrences_for_line(text, "ä", 1, true, &mut ranges);
    assert_eq!(ranges, [(0, 1), (11, 12), (21, 22)]);
    word_occurrences_for_line(text, "ä", 1, false, &mut ranges);
    assert_eq!(
        ranges,
        [
            (0, 1),
            (2, 3),
            (3, 4),
            (6, 7),
            (8, 9),
            (11, 12),
            (14, 15),
            (16, 17),
            (21, 22)
        ]
    );
    for start in [0, 1, 6, 21, 22] {
        let mut cursor = OccurrenceCursor::new(&ranges, start);
        for col in start..24 {
            assert_eq!(
                cursor.contains(col),
                ranges.iter().any(|&(start, end)| col >= start && col < end)
            );
        }
    }
    word_occurrences_for_line("😀ä界 ä界x ä界", "ä界", 2, true, &mut ranges);
    assert_eq!(ranges, [(1, 3), (8, 10)]);
    word_occurrences_for_line("banana banana", "ana", 3, false, &mut ranges);
    assert_eq!(ranges, [(1, 4), (8, 11)]);
    word_occurrences_for_line("text", "", 0, false, &mut ranges);
    assert!(ranges.is_empty());
}

#[test]
fn decoration_runs_preserve_provider_priority_for_overlaps_and_viewport_seeks() {
    use crate::ui_editor_highlight::{ValueRangeCursor, flatten_priority_ranges};
    let mut runs = Vec::new();
    for seed in 0..32 {
        let mut ranges: Vec<_> = (0..64)
            .map(|index| {
                let start = (index * 37 + seed * 11) % 127;
                let length = (index * 13 + seed * 7) % 31;
                (start, start + length, index)
            })
            .collect();
        ranges.extend([(149, usize::MAX, 71), (142, 142, 72), (80, 12, 73)]);
        flatten_priority_ranges(&ranges, &mut runs);
        assert!(runs.windows(2).all(|pair| pair[0].1 <= pair[1].0));
        for start in [0, 1, 37, 100, 150, usize::MAX - 1] {
            let mut cursor = ValueRangeCursor::new(&runs, start);
            for col in start..start.saturating_add(40) {
                let expected = ranges
                    .iter()
                    .find(|&&(a, b, _)| a <= col && col < b)
                    .map(|&(_, _, value)| value);
                assert_eq!(cursor.at(col), expected, "seed={seed}, col={col}");
            }
        }
    }
}

#[test]
fn merged_selection_ranges_preserve_unsorted_and_nested_coverage() {
    use crate::ui_editor_highlight::{OccurrenceCursor, merge_column_ranges};
    let original = [
        (12, 15),
        (4, 9),
        (7, 20),
        (0, 4),
        (5, 5),
        (10, 8),
        (25, usize::MAX),
    ];
    let mut ranges = original.to_vec();
    merge_column_ranges(&mut ranges);
    assert_eq!(ranges, [(0, 20), (25, usize::MAX)]);
    let mut cursor = OccurrenceCursor::new(&ranges, 3);
    for col in 3..50 {
        assert_eq!(
            cursor.contains(col),
            original.iter().any(|&(a, b)| a <= col && col < b)
        );
    }
}

#[test]
fn inline_preview_cursor_preserves_literal_and_named_colors_after_unicode_prefixes() {
    use crate::ui_editor_highlight::InlineColorCursor;
    let text = "界 tomato #f00 coral rgb(1,2,3) gold #00ff00";
    let original = crate::ui_colors::find_inline_colors(text, true);
    let mut colors = original.clone();
    colors.sort_by_key(|&(col, _, _, _)| col);
    for start in [0, 5, 13, 28, 100] {
        let mut cursor = InlineColorCursor::new(&colors, start);
        for col in start..start + 30 {
            let expected = original
                .iter()
                .find(|&&(a, _, _, _)| a == col)
                .map(|&(_, r, g, b)| ratatui::style::Color::Rgb(r, g, b));
            assert_eq!(cursor.at(col), expected);
        }
    }
}

#[test]
fn cached_span_lengths_validate_unicode_without_recounting_text() {
    use crate::ui_editor_highlight::{HighlightedLine, PLAIN_STYLE};
    let spans = [(PLAIN_STYLE, "界".into()), (PLAIN_STYLE, "😀x".into())];
    let current = HighlightedLine::new(Some((&spans, 3)), "界😀x", 3, PLAIN_STYLE);
    assert_eq!(current.iter().count(), 2);
    let text = "界😀x";
    let fallback = HighlightedLine::new(Some((&spans, 4)), text, 3, PLAIN_STYLE);
    let (_, rendered) = fallback.iter().next().unwrap();
    assert_eq!(rendered, text);
    assert!(
        std::ptr::eq(rendered.as_ptr(), text.as_ptr()),
        "fallback borrows its input"
    );
    let plain = HighlightedLine::new(None, text, 3, PLAIN_STYLE);
    assert!(std::ptr::eq(
        plain.iter().next().unwrap().1.as_ptr(),
        text.as_ptr()
    ));
}

#[test]
fn borrowed_highlight_views_keep_worker_styles_and_reject_incomplete_runs() {
    use crate::ui_editor_highlight::{HighlightedLine, PLAIN_STYLE};
    let mut styled = PLAIN_STYLE;
    styled.font_style = crate::textmate::FontStyle::BOLD;
    styled.foreground.r = 7;
    let spans = [(styled, "界".to_owned()), (PLAIN_STYLE, "😀x\n".to_owned())];
    let view = HighlightedLine::new(Some((&spans, 3)), "界😀x", 3, PLAIN_STYLE);
    assert_eq!(
        view.iter().collect::<Vec<_>>(),
        [(styled, "界"), (PLAIN_STYLE, "😀x\n")]
    );
    let invalid = HighlightedLine::new(Some((&spans, 2)), "界😀x", 3, styled);
    assert_eq!(invalid.iter().collect::<Vec<_>>(), [(PLAIN_STYLE, "界😀x")]);
}

fn split_cache_fixture(diagnostics_per_file: usize) -> App {
    let mut app = App::test_new();
    app.show_minimap = false;
    app.show_color_preview = false;
    app.show_breadcrumbs = false;
    app.sticky_scroll = false;
    app.cursor_visible = false;
    for (name, word) in [("cache-first.rs", "alpha"), ("cache-second.rs", "beta")] {
        let path = std::path::PathBuf::from(name);
        let mut editor = Editor::test_with_text(&(format!("{word} {word};\n").repeat(2_000)));
        editor.file_path = Some(path.clone());
        app.tabs.push(Tab::new_file(editor));
        app.lsp_manager.diagnostics.insert(
            path,
            (0..diagnostics_per_file)
                .map(|index| crate::lsp::LspDiagnostic {
                    line: index % 2_000,
                    col: 0,
                    end_line: index % 2_000,
                    end_col: 3,
                    message: format!("provider diagnostic {index}: {}", "explanation ".repeat(10)),
                    severity: lsp_types::DiagnosticSeverity::WARNING,
                })
                .collect(),
        );
    }
    app.lsp_manager.diagnostics_version = 1;
    app.active_tab = 0;
    app.create_split_group_dir(1, crate::app::SplitDirection::Horizontal);
    app
}

#[test]
fn split_editor_redraws_share_diagnostic_and_overview_snapshots_without_cache_churn() {
    let mut app = split_cache_fixture(5);
    let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    assert_eq!(app.editor_diagnostic_cache.len(), 2);
    assert_eq!(app.editor_overview_cache.len(), 2);
    let diagnostic_snapshots: Vec<_> = app
        .editor_diagnostic_cache
        .iter()
        .map(|entry| {
            (
                entry.key.clone(),
                entry.diagnostics.clone(),
                entry.lines.clone(),
                entry.by_line.clone(),
            )
        })
        .collect();
    let overview_snapshots: Vec<_> = app
        .editor_overview_cache
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    for (key, diagnostics, lines, by_line) in diagnostic_snapshots {
        let current = app
            .editor_diagnostic_cache
            .iter()
            .find(|entry| entry.key == key)
            .unwrap();
        assert!(std::sync::Arc::ptr_eq(&diagnostics, &current.diagnostics));
        assert!(std::sync::Arc::ptr_eq(&lines, &current.lines));
        assert!(std::sync::Arc::ptr_eq(&by_line, &current.by_line));
    }
    for (key, overview) in overview_snapshots {
        let current = app
            .editor_overview_cache
            .iter()
            .find(|(cached, _)| cached == &key)
            .unwrap();
        assert!(std::sync::Arc::ptr_eq(&overview, &current.1));
    }
    let cursor_words = app.editor_cursor_word_cache.borrow();
    let words: Vec<_> = cursor_words
        .iter()
        .filter_map(|entry| entry.word.as_deref())
        .collect();
    assert!(
        words.contains(&"alpha") && words.contains(&"beta"),
        "pane identity keeps cursor words separate: {words:?}"
    );
    drop(cursor_words);
    app.lsp_manager.diagnostics_version += 1;
    super::refresh_cached_editor_diagnostics(&mut app);
    assert_eq!(
        app.editor_diagnostic_cache.len(),
        1,
        "provider updates retire obsolete snapshots"
    );
}

#[test]
fn per_editor_render_caches_remain_bounded_when_many_tabs_are_visited() {
    let mut app = App::test_new();
    for index in 0..40 {
        let mut editor = Editor::test_with_text("word\n");
        editor.file_path = Some(std::path::PathBuf::from(format!("cache-{index}.rs")));
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = index;
        super::refresh_cached_editor_diagnostics(&mut app);
        super::cached_overview_change_rows(&mut app, 20, None);
        super::refresh_cursor_word(
            &app.tabs[index].editor,
            &mut app.cached_cursor_word_key,
            &mut app.cached_cursor_word,
            app.editor_cursor_word_cache.get_mut(),
        );
    }
    assert_eq!(
        app.editor_diagnostic_cache.len(),
        super::MAX_CACHED_EDITOR_PANES
    );
    assert_eq!(
        app.editor_overview_cache.len(),
        super::MAX_CACHED_EDITOR_PANES
    );
    assert_eq!(
        app.editor_cursor_word_cache.borrow().len(),
        super::MAX_CACHED_EDITOR_PANES
    );
}

#[test]
fn bounded_editor_minimap_keeps_git_markers_when_a_compare_source_is_active() {
    use crate::ui_editor_minimap::{MinimapParams, draw_minimap};
    let mut target = "x".repeat(crate::highlight::MAX_SYNTAX_LINE_CHARS + 1) + "\n";
    target.push_str(&"ordinary line\n".repeat(99));
    let base = target.replacen("ordinary line", "previous line", 1);
    let mut editor = Editor::test_with_text(&target);
    editor.file_path = Some("bounded-compare.rs".into());
    editor.set_git_line_status(
        (0..editor.line_count())
            .map(|line| if line == 7 { 'S' } else { ' ' })
            .collect(),
    );
    assert!(!editor.large_file_mode);
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.show_minimap = true;
    app.sticky_scroll = false;
    app.diff_mode = crate::app::DiffMode::SideBySide;
    app.compare_base_content = Some(base.clone().into());
    app.set_compare_diff_lines(crate::app::compute_file_diff(&base, &target));
    assert!(app.should_render_diff_for_active_editor());
    let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            super::draw_editor(frame, &mut app, area);
        })
        .unwrap();
    let minimap = app.minimap_area.unwrap();
    let mut reference = Terminal::new(TestBackend::new(120, 20)).unwrap();
    reference
        .draw(|frame| {
            draw_minimap(
                frame,
                MinimapParams {
                    area: minimap,
                    editor: app.editor().unwrap(),
                    scroll: app.editor().unwrap().scroll_offset,
                    visible_lines: app.code_area.unwrap().height as usize,
                    search_matches: &app.search_matches,
                    diagnostic_lines: &[],
                    diagnostic_index: None,
                    change_index: None,
                    diff_changes: None,
                    colors: app.colors,
                    hovered: app.hovered_minimap,
                    active: app.dragging_minimap,
                },
            )
        })
        .unwrap();
    for y in minimap.top()..minimap.bottom() {
        for x in minimap.left()..minimap.right() {
            assert_eq!(
                terminal.backend().buffer()[(x, y)],
                reference.backend().buffer()[(x, y)]
            );
        }
    }
}

#[test]
#[ignore = "manual release split redraw benchmark"]
fn benchmark_split_editor_render_caches() {
    use std::{hint::black_box, time::Instant};
    let mut app = split_cache_fixture(10_000);
    let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let expected = terminal.backend().buffer().clone();
    let mut before = Vec::new();
    let mut after = Vec::new();
    for sample in 0..9 {
        for optimized in [sample % 2 == 0, sample % 2 != 0] {
            let started = Instant::now();
            for _ in 0..10 {
                if !optimized {
                    // The former single cache slot was overwritten by each
                    // other pane, rebuilding these snapshots every frame.
                    app.editor_diagnostic_cache.clear();
                    app.editor_overview_cache.clear();
                    app.editor_cursor_word_cache.get_mut().clear();
                }
                terminal
                    .draw(|frame| crate::ui::draw(frame, &mut app))
                    .unwrap();
                black_box(terminal.backend().buffer());
            }
            let elapsed = started.elapsed() / 10;
            assert_eq!(*terminal.backend().buffer(), expected);
            if optimized {
                after.push(elapsed);
            } else {
                before.push(elapsed);
            }
        }
    }
    before.sort();
    after.sort();
    eprintln!(
        "split redraw 10000 diagnostics/file: rebuild={:?} warm-cache={:?}",
        before[4], after[4]
    );
}

#[test]
#[ignore = "manual release borrowed line preparation benchmark"]
fn benchmark_plain_editor_line_preparation() {
    use crate::ui_editor_highlight::{HighlightedLine, PLAIN_STYLE};
    use std::{borrow::Cow, hint::black_box, time::Instant};
    for (name, line) in [
        ("ascii", "let value = example + other;\r\n"),
        ("unicode", "let 名称 = \"välue界😀\";\r\n"),
    ] {
        let rope = ropey::Rope::from_str(&line.repeat(5_000));
        let mut before = Vec::new();
        let mut after = Vec::new();
        let mut expected = None;
        for sample in 0..9 {
            for optimized in [sample % 2 == 0, sample % 2 != 0] {
                let started = Instant::now();
                let mut digest = 0u64;
                for _ in 0..500 {
                    for rope_line in rope.lines_at(2_500).take(60) {
                        let text = if optimized {
                            rope_line
                                .as_str()
                                .map_or_else(|| Cow::Owned(rope_line.to_string()), Cow::Borrowed)
                        } else {
                            let mut text = String::with_capacity(256);
                            for chunk in rope_line.chunks() {
                                text.push_str(chunk);
                            }
                            Cow::Owned(text)
                        };
                        let clean = text.trim_end_matches(['\n', '\r']);
                        if optimized {
                            let view = HighlightedLine::new(
                                None,
                                clean,
                                rope_line.len_chars() - (text.len() - clean.len()),
                                PLAIN_STYLE,
                            );
                            for (_, span) in view.iter() {
                                digest += black_box(span).chars().map(|ch| ch as u64).sum::<u64>();
                            }
                        } else {
                            // Former plain highlighting copied the line again.
                            let spans = vec![(PLAIN_STYLE, clean.to_string())];
                            for (_, span) in black_box(&spans) {
                                digest += span.chars().map(|ch| ch as u64).sum::<u64>();
                            }
                        }
                    }
                }
                assert_eq!(*expected.get_or_insert(digest), digest);
                let elapsed = started.elapsed() / 500;
                if optimized {
                    after.push(elapsed);
                } else {
                    before.push(elapsed);
                }
            }
        }
        before.sort();
        after.sort();
        eprintln!(
            "plain line preparation {name}: copies={:?} borrowed={:?}",
            before[4], after[4]
        );
    }
}

#[test]
fn semantic_color_cache_preserves_language_modifiers_and_missing_selector_behavior() {
    use crate::lsp::DecodedSemanticToken;
    use lsp_types::{SemanticTokenModifier, SemanticTokenType, SemanticTokensLegend};
    let legend = SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::new("variable"),
            SemanticTokenType::new("function"),
        ],
        token_modifiers: vec![SemanticTokenModifier::new("readonly")],
    };
    let theme = crate::semantic_theme::SemanticTheme::from_iter([
        ("variable".to_string(), (1, 2, 3)),
        ("variable.readonly:rust".to_string(), (4, 5, 6)),
        ("variable:python".to_string(), (7, 8, 9)),
    ]);
    let tokens: Vec<_> = [(0, 0), (0, 1), (0, 1), (1, 0), (99, 0)]
        .into_iter()
        .map(|(token_type, modifiers)| DecodedSemanticToken {
            line: 0,
            start_char: 0,
            length: 3,
            token_type,
            _token_modifiers: modifiers,
        })
        .collect();
    let mut cache = rustc_hash::FxHashMap::default();
    let colors: Vec<_> = tokens
        .iter()
        .map(|token| {
            super::semantic_color_for_token(token, Some(&legend), "rust", Some(&theme), &mut cache)
        })
        .collect();
    let expected: Vec<_> = tokens
        .iter()
        .map(|token| {
            crate::lsp::themed_semantic_token_style(token, &legend, "rust", Some(&theme))
        })
        .collect();
    assert_eq!(colors, expected);
    assert_eq!(
        cache.len(),
        4,
        "duplicate token combinations share a selector resolution"
    );
    assert!(
        super::semantic_color_for_token(&tokens[0], None, "rust", Some(&theme), &mut cache)
            .is_none()
    );
}

#[test]
#[ignore = "manual release decoration lookup benchmark"]
fn benchmark_dense_decoration_lookups() {
    use crate::ui_editor_highlight::{ValueRangeCursor, flatten_priority_ranges};
    use std::{hint::black_box, time::Instant};
    let ranges: Vec<_> = (0..4_096)
        .map(|index| (index * 3, index * 3 + 2, index % 7))
        .collect();
    let mut runs = Vec::new();
    let mut reference_samples = Vec::new();
    let mut cursor_samples = Vec::new();
    for sample in 0..9 {
        for optimized in [sample % 2 == 0, sample % 2 != 0] {
            let started = Instant::now();
            let mut digest = 0;
            for _ in 0..10 {
                // Include preparation, as the renderer does once per logical line.
                if optimized {
                    flatten_priority_ranges(black_box(&ranges), &mut runs);
                }
                for row in 0..60 {
                    let start = 2_000 + row * 120;
                    let mut cursor = ValueRangeCursor::new(&runs, start);
                    for col in start..start + 120 {
                        let value = if optimized {
                            cursor.at(black_box(col))
                        } else {
                            // Previous renderer: first matching decoration wins.
                            black_box(&ranges)
                                .iter()
                                .find(|&&(a, b, _)| a <= col && col < b)
                                .map(|&(_, _, value)| value)
                        };
                        digest += black_box(value).unwrap_or(0);
                    }
                }
            }
            black_box(digest);
            let elapsed = started.elapsed() / 10;
            if optimized {
                cursor_samples.push(elapsed);
            } else {
                reference_samples.push(elapsed);
            }
        }
    }
    reference_samples.sort();
    cursor_samples.sort();
    eprintln!(
        "dense decoration lookup scan={:?} cursor={:?}",
        reference_samples[4], cursor_samples[4]
    );
    let mut cursor = ValueRangeCursor::new(&runs, 0);
    for col in 0..12_288 {
        assert_eq!(
            cursor.at(col),
            ranges
                .iter()
                .find(|&&(a, b, _)| a <= col && col < b)
                .map(|&(_, _, value)| value)
        );
    }
}

#[test]
#[ignore = "manual release semantic selector benchmark"]
fn benchmark_semantic_selector_resolution() {
    use crate::lsp::DecodedSemanticToken;
    use lsp_types::{SemanticTokenModifier, SemanticTokenType, SemanticTokensLegend};
    use std::{hint::black_box, time::Instant};
    let legend = SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::new("variable"),
            SemanticTokenType::new("function"),
        ],
        token_modifiers: vec![SemanticTokenModifier::new("readonly")],
    };
    let mut theme = rustc_hash::FxHashMap::from_iter(
        (0..128).map(|index| (format!("unused_{index}:rust"), (1, 2, 3))),
    );
    theme.extend([
        ("variable".to_string(), (31, 47, 59)),
        ("function".to_string(), (61, 71, 83)),
        ("variable.readonly:rust".to_string(), (97, 101, 107)),
    ]);
    let theme = crate::semantic_theme::SemanticTheme::from_iter(theme);
    let tokens: Vec<_> = (0..512)
        .map(|index| DecodedSemanticToken {
            line: 0,
            start_char: index * 12,
            length: 12,
            token_type: index % 2,
            _token_modifiers: (index / 2) % 2,
        })
        .collect();
    let mut reference_samples = Vec::new();
    let mut cached_samples = Vec::new();
    for sample in 0..9 {
        for optimized in [sample % 2 == 0, sample % 2 != 0] {
            let started = Instant::now();
            for _ in 0..10 {
                // Start with a fresh frame cache, including all preparation.
                let mut cache = rustc_hash::FxHashMap::default();
                for token in &tokens {
                    let cached_color = if optimized {
                        super::semantic_color_for_token(
                            token,
                            Some(&legend),
                            "rust",
                            Some(&theme),
                            &mut cache,
                        )
                    } else {
                        None
                    };
                    for _ in 0..token.length {
                        let color = if optimized {
                            cached_color
                        } else {
                            crate::lsp::themed_semantic_token_style(
                                black_box(token),
                                &legend,
                                "rust",
                                Some(&theme),
                            )
                        };
                        black_box(color);
                    }
                }
            }
            let elapsed = started.elapsed() / 10;
            if optimized {
                cached_samples.push(elapsed);
            } else {
                reference_samples.push(elapsed);
            }
        }
    }
    reference_samples.sort();
    cached_samples.sort();
    eprintln!(
        "semantic selector lookup per-character={:?} cached={:?}",
        reference_samples[4], cached_samples[4]
    );
}

#[test]
fn unicode_comment_markers_preserve_existing_ascii_boundary_rules() {
    use crate::ui_editor_highlight::todo_markers_for_line;
    use ratatui::style::Color;
    let mut markers = Vec::new();
    for (text, expected) in [
        ("界 TODO", vec![(2, 6)]),
        ("XTODOTODO TODOx _TODO_", vec![(17, 21)]),
        ("ÄTODO", vec![(1, 5)]),
    ] {
        todo_markers_for_line(text, &[("TODO", Color::Red)], &mut markers);
        assert_eq!(
            markers,
            expected
                .into_iter()
                .map(|(start, end)| (start, end, Color::Red))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn occurrence_target_retains_normal_file_behavior_and_skips_large_file_analysis() {
    let mut app = render_fixture("unicode_occurrences");
    app.status_bar_pins.insert("word_occ".into(), true);
    assert_eq!(app.word_occurrence_target().unwrap().word.as_ref(), "ä");
    app.editor_mut().unwrap().large_file_mode = true;
    assert!(app.word_occurrence_target().is_none());
    app.editor_mut().unwrap().large_file_mode = false;
    app.editor_mut().unwrap().selection_anchor = Some((0, 2));
    assert!(app.word_occurrence_target().is_none());
    app.editor_mut().unwrap().selection_anchor = None;
    app.search_active = true;
    assert!(app.word_occurrence_target().is_none());
}

#[test]
fn wrapped_scrolling_preserves_cached_syntax_tabs_search_and_selection_styles() {
    use crate::app::WhitespaceMode;
    let source = ("\tlet value = \"välue界\";  ".repeat(30) + "  \n").repeat(3);
    for whitespace in [
        WhitespaceMode::None,
        WhitespaceMode::Boundary,
        WhitespaceMode::Selection,
        WhitespaceMode::Trailing,
        WhitespaceMode::All,
    ] {
        let path = std::path::PathBuf::from("wrapped.rs");
        let mut editor = Editor::test_with_text(&source);
        editor.word_wrap = true;
        editor.file_path = Some(path.clone());
        editor.whitespace_mode = whitespace;
        editor.cursor_col = 1;
        editor.selection_anchor = Some((0, 40));
        editor.extra_cursors = vec![(0, 150), (1, 75)];
        editor.extra_cursor_anchors = vec![Some((0, 90)), Some((1, 110))];
        let rope = editor.rope.clone();
        let mut app = App::test_new();
        app.tabs = vec![Tab::new_file(editor)];
        app.active_tab = 0;
        app.show_minimap = false;
        app.show_color_preview = false;
        app.sticky_scroll = false;
        app.cursor_visible = false;
        app.search_matches = vec![(0, 30, 90), (1, 40, 110)];
        app.lsp_document_highlights = vec![(0, 100, 0, 140), (1, 160, 1, 210)];
        let job = app
            .highlighter
            .prepare_job(&rope, &path, 0, None, 0..3, None, app.syntax_theme.as_str())
            .unwrap();
        let (syntax_set, theme_set) = app.highlighter.shared_sets();
        assert!(
            app.highlighter
                .apply_batch(job.run(&rope, &syntax_set, &theme_set, || false))
        );
        let (spans, _) = app
            .highlighter
            .highlighted_line_by_index_if_cached(0, Some(&path), None, app.syntax_theme.as_str(), 0)
            .unwrap();
        assert!(
            spans.len() > 4,
            "fixture must exercise multiple syntax spans"
        );
        let area = Rect::new(0, 0, 64, 48);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        let mut reference = None;
        terminal
            .draw(|frame| {
                super::draw_editor(frame, &mut app, area);
                reference = Some(frame.buffer_mut().clone());
            })
            .unwrap();
        let reference = reference.unwrap();
        let code = app.code_area.unwrap();
        for offset in [1, 5, 13] {
            app.editor_mut().unwrap().wrap_scroll_offset = offset;
            let mut scrolled = None;
            terminal
                .draw(|frame| {
                    super::draw_editor(frame, &mut app, area);
                    scrolled = Some(frame.buffer_mut().clone());
                })
                .unwrap();
            let scrolled = scrolled.unwrap();
            for row in 0..12 {
                for col in 0..code.width {
                    assert_eq!(
                        scrolled[(code.x + col, code.y + row)],
                        reference[(code.x + col, code.y + row + offset as u16)],
                        "offset={offset} row={row} col={col}"
                    );
                }
            }
        }
    }
}

#[test]
fn duplicate_buffer_sync_invalidates_fold_hints_when_versions_are_equal() {
    let path = std::path::PathBuf::from("shared.rs");
    let mut source = Editor::test_with_text("root\n  child\n");
    source.file_path = Some(path.clone());
    let mut duplicate = Editor::test_with_text("root\nchild\n");
    duplicate.file_path = Some(path);
    assert!(!duplicate.can_fold_line(0));
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(source), Tab::new_file(duplicate)];
    app.active_tab = 0;
    app.sync_duplicate_file_tabs();
    assert_eq!(
        app.tabs[0].editor.content_version,
        app.tabs[1].editor.content_version
    );
    assert!(app.tabs[1].editor.can_fold_line(0));
}

#[test]
#[ignore = "manual release large-file input benchmark"]
fn benchmark_disabled_large_file_occurrence_target() {
    use std::{hint::black_box, time::Instant};
    let mut app = App::test_new();
    app.status_bar_pins.insert("word_occ".into(), true);
    let mut editor = Editor::test_with_text(&"x".repeat(16 * 1024 * 1024));
    editor.large_file_mode = true;
    editor.cursor_col = 8 * 1024 * 1024;
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    let mut before = Vec::new();
    let mut after = Vec::new();
    for sample in 0..7 {
        for optimized in [sample % 2 == 0, sample % 2 != 0] {
            let started = Instant::now();
            for _ in 0..5 {
                let app = black_box(&app);
                let target = if optimized {
                    app.word_occurrence_target()
                } else {
                    // The previous order found/copied the word first, then
                    // discarded it because whole-file analysis was disabled.
                    let editor = app.editor().unwrap();
                    editor.word_at_cursor().and_then(|word| {
                        app.editor_analysis_target()
                            .map(|editor| crate::background::WordOccurrenceTarget { editor, word: word.into() })
                    })
                };
                assert!(black_box(target).is_none());
            }
            if optimized {
                after.push(started.elapsed() / 5);
            } else {
                before.push(started.elapsed() / 5);
            }
        }
    }
    before.sort();
    after.sort();
    eprintln!(
        "disabled 16MiB word target before={:?} after={:?}",
        before[3], after[3]
    );
}
