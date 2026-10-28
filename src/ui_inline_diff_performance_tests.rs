use super::*;
use crate::app::{DiffMode, Tab};
use crate::editor::Editor;
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use std::rc::Rc;

fn reference<T>(enabled: bool, run: impl FnOnce() -> T) -> T {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            REFERENCE_DIFF_RENDER.with(|flag| flag.set(self.0));
        }
    }
    let _reset = Reset(REFERENCE_DIFF_RENDER.with(|flag| flag.replace(enabled)));
    run()
}

fn style(index: usize) -> crate::textmate::Style {
    crate::textmate::Style {
        foreground: crate::textmate::Color {
            r: (index * 71) as u8,
            g: (index * 29 + 80) as u8,
            b: 190,
            a: if index % 2 == 0 { 255 } else { 128 },
        },
        background: crate::textmate::Color {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
        },
        font_style: match index % 4 {
            0 => crate::textmate::FontStyle::BOLD,
            1 => crate::textmate::FontStyle::ITALIC,
            2 => crate::textmate::FontStyle::UNDERLINE,
            _ => crate::textmate::FontStyle::empty(),
        },
    }
}

#[test]
fn indexed_diff_syntax_seeks_match_full_prefix_styles_and_character_flags() {
    let spans: Vec<_> = (0..120)
        .map(|index| {
            (
                style(index),
                match index % 5 {
                    0 => String::new(),
                    1 => "\t界αe\u{301} 👩\u{200d}💻".repeat(8),
                    2 => "plain ASCII source".repeat(13),
                    3 => "\r\n".into(),
                    _ => "Σä/🦀 token \n".into(),
                },
            )
        })
        .collect();
    let index = DiffHighlightIndex::new(&spans);
    let flags: Vec<_> = (0..index.char_len()).map(|i| i % 7 < 3).collect();
    for width in [0, 1, 17, 81] {
        for scroll in [
            0,
            1,
            63,
            64,
            65,
            499,
            1_800,
            index.char_len() - 1,
            index.char_len() + 20,
        ] {
            let args = (Color::Rgb(20, 30, 40), Some(Color::Rgb(180, 190, 200)));
            let expected = render_diff_highlighted_spans_reference(
                &spans,
                width,
                scroll,
                args.0,
                args.1,
                Some(&flags),
                Color::White,
            );
            let actual = render_diff_highlighted_spans(
                &spans,
                &index,
                width,
                scroll,
                args.0,
                args.1,
                Some(&flags),
                Color::White,
            );
            assert_eq!(actual, expected, "width={width}, scroll={scroll}");
        }
    }
}

#[test]
fn borrowed_plain_diff_fallback_matches_cached_miss_and_truncated_styles() {
    for source in ["α界e\u{301} text", "plain text", "text\r\n", "", "\n"] {
        let short = Rc::new(vec![(style(0), String::new())]);
        for cached in [None, Some(&short)] {
            for fg in [Color::White, Color::Rgb(25, 45, 65)] {
                for scroll in [0, 1, 3, 99] {
                    let width = 16;
                    let window: String = source.chars().skip(scroll).take(width).collect();
                    let flags: Vec<_> = (0..source.chars().count()).map(|i| i % 2 == 0).collect();
                    let draw = || {
                        render_diff_line(
                            cached,
                            source,
                            source.chars().count(),
                            || window.as_str(),
                            width,
                            scroll,
                            Color::Rgb(20, 20, 20),
                            Some(Color::Rgb(80, 90, 100)),
                            Some(&flags),
                            fg,
                        )
                    };
                    assert_eq!(
                        reference(false, draw),
                        reference(true, draw),
                        "source={source:?}, scroll={scroll}"
                    );
                }
            }
        }
    }
}

#[test]
fn diff_span_metadata_reuses_immutable_identity_and_stays_bounded() {
    let mut cache = DiffHighlightCache::default();
    let first = Rc::new(vec![(style(0), "界".repeat(1_000))]);
    let a = cache.get(&first);
    assert!(Rc::ptr_eq(&a, &cache.get(&first)));
    let weak = Rc::downgrade(&first);
    drop(first);
    assert!(weak.upgrade().is_none());
    let second = Rc::new(vec![(style(0), "other".into())]);
    assert_eq!(cache.get(&second).char_len(), 5);
    let mut sources = Vec::new();
    for _ in 0..DiffHighlightCache::MAX_ENTRIES + 3 {
        let spans = Rc::new(vec![(style(0), "界".repeat(1_000))]);
        cache.get(&spans);
        sources.push(spans);
        assert!(cache.entries.len() <= DiffHighlightCache::MAX_ENTRIES);
        assert!(cache.bytes <= DiffHighlightCache::MAX_BYTES);
    }
    let oversized = Rc::new(vec![
        (style(0), String::new());
        DiffHighlightCache::MAX_BYTES
            / std::mem::size_of::<DiffSpanIndex>()
            + 1
    ]);
    let entries = cache.entries.len();
    assert!(cache.get(&oversized).storage_bytes() > DiffHighlightCache::MAX_BYTES);
    assert_eq!(cache.entries.len(), entries);
    assert!(cache.bytes <= DiffHighlightCache::MAX_BYTES);
}

fn fixture(base: &str, target: &str, syntax: bool, collapsed: bool, dense: usize) -> App {
    let mut app = App::test_new();
    let path = std::path::PathBuf::from("inline-diff-performance.rs");
    let mut editor = Editor::test_with_text(target);
    editor.file_path = Some(path.clone());
    editor.word_wrap = false;
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.diff_mode = DiffMode::Inline;
    app.show_breadcrumbs = false;
    app.sticky_scroll = false;
    app.compare_base_content = Some(base.to_owned().into());
    app.set_compare_diff_lines(crate::app::compute_file_diff(base, target));
    if collapsed {
        app.diff_render_cache = std::sync::Arc::new(app.diff_render_cache.with_context(2));
    }
    let mut diagnostics = Vec::new();
    // Valid source rows keep the legacy projected vectors sorted. Stale
    // out-of-EOF default-zero behavior is covered by the mapped API tests.
    let source_lines = app
        .diff_render_cache
        .inline_target_rows()
        .len()
        .min(app.editor().unwrap().line_count());
    for line in 0..source_lines {
        for repeat in 0..dense {
            app.search_matches.push((line, repeat, repeat + 1));
            diagnostics.push(crate::lsp::LspDiagnostic {
                line,
                col: repeat,
                end_line: line,
                end_col: repeat + 1,
                severity: match repeat % 4 {
                    0 => lsp_types::DiagnosticSeverity::WARNING,
                    1 => lsp_types::DiagnosticSeverity::INFORMATION,
                    2 => lsp_types::DiagnosticSeverity::ERROR,
                    _ => lsp_types::DiagnosticSeverity::HINT,
                },
                message: "dense inline diagnostic".into(),
            });
        }
    }
    app.lsp_manager
        .diagnostics
        .insert(path.clone(), diagnostics);
    app.lsp_manager.diagnostics_version = 1;
    if syntax {
        for row in &app.diff_render_cache.rows {
            for (text, line, hash) in [
                (&row.left_text, row.left_line, row.left_highlight_hash()),
                (&row.right_text, row.right_line, row.right_highlight_hash()),
            ] {
                let Some(line) = line else {
                    continue;
                };
                let spans = text
                    .chars()
                    .collect::<Vec<_>>()
                    .chunks(17)
                    .enumerate()
                    .map(|(i, chars)| (style(i), chars.iter().collect()))
                    .collect();
                app.highlighter.insert_fragment_cached(
                    &path,
                    &app.syntax_theme,
                    None,
                    line,
                    hash,
                    spans,
                );
            }
        }
    }
    app
}

fn draw(app: &mut App, width: u16, height: u16, baseline: bool) -> Buffer {
    reference(baseline, || {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                if app.diff_mode == DiffMode::SideBySide {
                    draw_side_by_side_diff(frame, app, area);
                } else {
                    draw_inline_diff(frame, app, area);
                }
            })
            .unwrap();
        terminal.backend().buffer().clone()
    })
}

#[test]
fn dense_inline_frames_match_full_remapping_with_unicode_and_collapsed_rows() {
    let target: String = (0..160)
        .map(|line| {
            format!(
                "let value_{line} = \"{}\"; // tail\n",
                "界e\u{301} 👩\u{200d}💻 token ".repeat(24)
            )
        })
        .collect();
    let base = target
        .replace("value_40", "before_40")
        .replace("value_110", "before_110");
    for syntax in [false, true] {
        for collapsed in [false, true] {
            let mut app = fixture(&base, &target, syntax, collapsed, 8);
            for (width, height, line, column, minimap) in [
                (90, 16, 0, 0, true),
                (67, 13, 39, 161, false),
                (91, 18, 108, 350, true),
                (45, 12, 159, 900, false),
            ] {
                app.show_minimap = minimap;
                let editor = app.editor_mut().unwrap();
                editor.cursor_line = line;
                editor.scroll_offset = line;
                editor.horizontal_scroll = column;
                draw(&mut app, width, height, true); // Establish identical layout state.
                let expected = draw(&mut app, width, height, true);
                assert_eq!(
                    draw(&mut app, width, height, false),
                    expected,
                    "syntax={syntax}, collapsed={collapsed}, line={line}, column={column}"
                );
            }
        }
    }
}

#[test]
fn removed_only_and_side_by_side_frames_keep_reference_styles() {
    for (base, target) in [
        ("gone\n".repeat(100), String::new()),
        (
            "let old = \"界αβ\";\n".repeat(40),
            "let new = \"界αβ\";\n".repeat(40),
        ),
    ] {
        let mut app = fixture(&base, &target, true, false, 4);
        app.show_minimap = false;
        for mode in [DiffMode::Inline, DiffMode::SideBySide] {
            app.diff_mode = mode;
            app.editor_mut().unwrap().horizontal_scroll = 7;
            draw(&mut app, 80, 14, true);
            let expected = draw(&mut app, 80, 14, true);
            assert_eq!(draw(&mut app, 80, 14, false), expected);
        }
    }
}

#[test]
#[ignore = "release-mode inline diff overview/full-frame comparison"]
fn benchmark_dense_inline_diff_redraw_and_syntax_seek() {
    let target: String = (0..20_000)
        .map(|line| format!("let value_{line} = {line};\n"))
        .collect();
    let base = target
        .replace("value_1000", "before_1000")
        .replace("value_18000", "before_18000");
    let mut app = fixture(&base, &target, false, false, 6);
    app.show_minimap = false;
    draw(&mut app, 100, 42, false);
    for baseline in [true, false] {
        let start = std::time::Instant::now();
        for _ in 0..50 {
            std::hint::black_box(draw(&mut app, 100, 42, baseline));
        }
        eprintln!(
            "inline diff 50 frames / 120k search+diagnostics: reference={baseline}, elapsed={:?}",
            start.elapsed()
        );
    }
    let source = "界αβ token ".repeat(1_400);
    let spans = Rc::new(vec![(style(1), source.clone())]);
    let len = source.chars().count();
    let window: String = source.chars().skip(12_000).take(80).collect();
    diff_highlight_index(&spans);
    for baseline in [true, false] {
        let start = std::time::Instant::now();
        for _ in 0..1_000 {
            std::hint::black_box(reference(baseline, || {
                render_diff_line(
                    Some(&spans),
                    &source,
                    len,
                    || window.as_str(),
                    80,
                    12_000,
                    Color::Rgb(20, 20, 20),
                    None,
                    None,
                    Color::White,
                )
            }));
        }
        eprintln!(
            "diff syntax 1000 deep Unicode windows: reference={baseline}, elapsed={:?}",
            start.elapsed()
        );
    }
}
