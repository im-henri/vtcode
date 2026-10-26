use crate::{
    app::{App, Tab, WhitespaceMode},
    editor::Editor,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect};

fn fixture(wrapped: bool, syntax: bool) -> App {
    let source = (" \tlet value = \"ä界e\u{301} 👩\u{200d}💻 #ff0000\"; // TODO  ".repeat(220)
        + " \t \r\n")
        .repeat(4);
    let path = std::path::PathBuf::from("viewport-seek.rs");
    let mut editor = Editor::test_with_text(&source);
    editor.word_wrap = wrapped;
    editor.file_path = syntax.then(|| path.clone());
    editor.selection_anchor = Some((0, 270));
    editor.cursor_col = 310;
    editor.extra_cursors = vec![(0, 330), (1, 9_000)];
    editor.extra_cursor_anchors = vec![Some((0, 290)), Some((1, 8_900))];
    let rope = editor.rope.clone();
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.show_minimap = false;
    app.show_breadcrumbs = false;
    app.sticky_scroll = false;
    app.cursor_visible = true;
    app.search_matches = vec![(0, 280, 320), (1, 8_900, 9_100)];
    app.lsp_document_highlights = vec![(0, 260, 0, 340), (1, 8_800, 1, 9_200)];
    if syntax {
        app.lsp_manager.diagnostics.insert(
            path.clone(),
            vec![crate::lsp::LspDiagnostic {
                line: 0,
                col: 250,
                end_line: 1,
                end_col: 9_300,
                message: "viewport diagnostic".into(),
                severity: lsp_types::DiagnosticSeverity::WARNING,
            }],
        );
        app.lsp_manager.diagnostics_version = 1;
        let job = app
            .highlighter
            .prepare_job(&rope, &path, 0, None, 0..4, None, app.syntax_theme.as_str())
            .unwrap();
        let (syntaxes, themes) = app.highlighter.shared_sets();
        assert!(
            app.highlighter
                .apply_batch(job.run(&rope, &syntaxes, &themes, || false))
        );
    }
    app
}

fn draw(app: &mut App, area: Rect) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
    let mut result = None;
    terminal
        .draw(|frame| {
            super::draw_editor(frame, app, area);
            result = Some(frame.buffer_mut().clone());
        })
        .unwrap();
    result.unwrap()
}

#[test]
fn repeated_navigation_keeps_the_caret_painted_on_tabbed_and_wide_lines() {
    use crate::{animation_config::AnimationEffect, app::ActivePane};
    use ratatui::style::Color;
    let caret = Color::Rgb(251, 17, 139);
    for bounded in [false, true] {
        for source in ["\t\t\t\t\t\t\t\t\tEND", "界界界界界界界界界界界界界界界界界END", " \t界 a\t界界\t b\t END"] {
            let mut app = App::test_new();
            let mut editor = Editor::test_with_text(source);
            editor.large_file_mode = bounded;
            app.tabs = vec![Tab::new_file(editor)];
            app.active_tab = 0;
            app.active_pane = ActivePane::Editor;
            app.show_minimap = false;
            app.show_breadcrumbs = false;
            app.sticky_scroll = false;
            app.show_indent_guides = false;
            app.cursor_visible = true;
            app.colors.bg_cursor = caret;
            for effect in AnimationEffect::ALL { app.animation_settings.set(effect, false); }
            let area = Rect::new(0, 0, 30, 8);
            draw(&mut app, area);
            let len = app.editor().unwrap().line_len(0);
            for col in (0..=len).chain((0..=len).rev()) {
                app.editor_mut().unwrap().set_cursor(0, col);
                let buffer = draw(&mut app, area);
                let code = app.code_area.unwrap();
                assert!((code.x..code.right()).any(|x| buffer[(x, code.y)].bg == caret),
                    "missing caret: bounded={bounded} source={source:?} col={col} scroll={}",
                    app.editor().unwrap().horizontal_scroll);
            }
        }
    }
}

#[test]
fn wrapped_viewport_decodes_only_visible_prefix_and_preserves_boundary_lookahead() {
    let source = "    value  ".repeat(1_000) + "\n";
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text(&source);
    editor.word_wrap = true;
    editor.whitespace_mode = WhitespaceMode::Boundary;
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.show_minimap = false;
    app.show_breadcrumbs = false;
    app.sticky_scroll = false;
    let area = Rect::new(0, 0, 80, 14);
    let actual = draw(&mut app, area);
    let decoded = app
        .editor()
        .unwrap()
        .render_line_cache
        .borrow()
        .cached_line(0)
        .unwrap();
    assert!(
        decoded.chars.len() < 2_000,
        "the top viewport must not decode the rest of a long logical line"
    );
    assert!(decoded.whitespace.is_some());
    app.editor()
        .unwrap()
        .render_line_cache
        .borrow_mut()
        .reference_scan = true;
    assert_eq!(
        actual,
        draw(&mut app, area),
        "the full-line oracle retains whitespace-pair lookahead"
    );
}

#[test]
fn viewport_glyph_cache_matches_full_prefix_scan_for_tabs_unicode_and_decorations() {
    for wrapped in [false, true] {
        for syntax in [false, true] {
            let mut app = fixture(wrapped, syntax);
            for mode in [
                WhitespaceMode::None,
                WhitespaceMode::Boundary,
                WhitespaceMode::Trailing,
                WhitespaceMode::Selection,
                WhitespaceMode::All,
            ] {
                app.editor_mut().unwrap().whitespace_mode = mode;
                for (width, offset, tab_size) in [
                    (64, 0, 4),
                    (71, 1, 4),
                    (55, 277, 4),
                    (61, 8_900, 7),
                    (64, 10_500, 4),
                ] {
                    let editor = app.editor_mut().unwrap();
                    editor.tab_size = tab_size;
                    if wrapped {
                        editor.wrap_scroll_offset = offset.min(150);
                    } else {
                        editor.horizontal_scroll = offset;
                    }
                    editor.render_line_cache.borrow_mut().reference_scan = true;
                    let area = Rect::new(0, 0, width, 14);
                    let reference = draw(&mut app, area);
                    app.editor()
                        .unwrap()
                        .render_line_cache
                        .borrow_mut()
                        .reference_scan = false;
                    assert_eq!(
                        draw(&mut app, area),
                        reference,
                        "wrapped={wrapped} syntax={syntax} width={width} offset={offset} tab_size={tab_size}"
                    );
                    let builds = app.editor().unwrap().render_line_cache.borrow().builds;
                    assert_eq!(draw(&mut app, area), reference, "warm redraw");
                    assert_eq!(
                        app.editor().unwrap().render_line_cache.borrow().builds,
                        builds,
                        "warm redraw decoded unchanged lines"
                    );
                }
            }
        }
    }
}

#[test]
fn equal_version_rope_replacement_invalidates_warm_viewport_text() {
    let mut app = App::test_new();
    app.tabs
        .push(Tab::new_file(Editor::test_with_text("before \t ä界\n")));
    app.active_tab = 0;
    app.show_minimap = false;
    app.sticky_scroll = false;
    let area = Rect::new(0, 0, 60, 10);
    let before = draw(&mut app, area);
    let editor = app.editor_mut().unwrap();
    editor.rope = ropey::Rope::from_str("after! \t 界ä\n");
    let after = draw(&mut app, area);
    assert_ne!(before, after);
    app.editor()
        .unwrap()
        .render_line_cache
        .borrow_mut()
        .reference_scan = true;
    assert_eq!(draw(&mut app, area), after);
}

fn editing_fixture() -> App {
    let source = ("let value = \"ä界e\u{301}\";\t // TODO #ff0000  ".repeat(60) + "\n").repeat(32);
    let mut editor = Editor::test_with_text(&source);
    editor.cursor_line = 3;
    editor.cursor_col = 300;
    editor.horizontal_scroll = 270;
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.show_minimap = false;
    app.show_color_preview = false;
    app.show_breadcrumbs = false;
    app.sticky_scroll = false;
    app.cursor_visible = false;
    app.search_active = true;
    app.bracket_pair_colorization = false;
    app
}

fn assert_fresh_glyph_frame(app: &mut App, area: Rect) {
    let optimized = draw(app, area);
    let saved = app
        .editor()
        .unwrap()
        .render_line_cache
        .replace(Default::default());
    let reference = draw(app, area);
    app.editor().unwrap().render_line_cache.replace(saved);
    assert_eq!(
        optimized, reference,
        "incremental cache differs from a fresh glyph/text cache"
    );
}

#[test]
fn newline_batches_retain_prefix_and_match_fresh_frames_through_undo_and_selection_deletion() {
    let mut app = editing_fixture();
    app.editor_mut().unwrap().cursor_line = 15;
    let area = Rect::new(0, 0, 100, 24);
    draw(&mut app, area);
    let prefix = app.editor().unwrap().render_line_cache.borrow().cached_line(2).unwrap();
    for operation in 0..9 {
        let editor = app.editor_mut().unwrap();
        match operation {
            0 => editor.insert_newline(),
            1 => editor.backspace(),
            2 => { editor.insert_newline(); editor.backspace(); }
            3 => { editor.insert_newline(); editor.insert_char('界'); }
            4 | 7 => editor.undo(),
            5 | 8 => editor.redo(),
            _ => {
                editor.selection_anchor = Some((10, 10));
                editor.cursor_line = 14;
                editor.cursor_col = 20;
                editor.backspace_replacing_selection();
            }
        }
        assert_fresh_glyph_frame(&mut app, area);
        let retained = app.editor().unwrap().render_line_cache.borrow().cached_line(2).unwrap();
        assert!(std::rc::Rc::ptr_eq(&prefix, &retained), "operation={operation} rebuilt untouched prefix");
    }
}

#[test]
#[ignore = "comparative optimized newline editing benchmark"]
fn benchmark_newline_editor_frames() {
    let area = Rect::new(0, 0, 100, 24);
    let mut states = Vec::new();
    for full in [true, false] {
        let mut app = editing_fixture();
        app.editor_mut().unwrap().cursor_line = 18;
        app.editor().unwrap().render_line_cache.borrow_mut().full_invalidation = full;
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal.draw(|frame| super::draw_editor(frame, &mut app, area)).unwrap();
        states.push((app, terminal));
    }
    let mut samples = [Vec::new(), Vec::new()];
    for round in 0..7 {
        for index in if round % 2 == 0 { [0, 1] } else { [1, 0] } {
            let (app, terminal) = &mut states[index];
            let started = std::time::Instant::now();
            for _ in 0..30 {
                app.editor_mut().unwrap().insert_newline();
                terminal.draw(|frame| super::draw_editor(frame, app, area)).unwrap();
                app.editor_mut().unwrap().backspace();
                terminal.draw(|frame| super::draw_editor(frame, app, area)).unwrap();
                std::hint::black_box(terminal.backend().buffer());
            }
            samples[index].push(started.elapsed() / 60);
        }
    }
    for times in &mut samples { times.sort(); }
    eprintln!("newline editing frame: full={:?} retained prefix={:?}", samples[0][3], samples[1][3]);
}

#[test]
#[ignore = "comparative optimized syntax refresh and editor repaint benchmark"]
fn benchmark_syntax_refresh_editor_frames() {
    let area = Rect::new(0, 0, 160, 40);
    let mut states = Vec::new();
    for replace in [true, false] {
        let mut app = editing_fixture();
        let path = std::path::PathBuf::from("syntax-refresh.rs");
        app.editor_mut().unwrap().file_path = Some(path.clone());
        let rope = app.editor().unwrap().rope.clone();
        let job = app.highlighter.prepare_job(&rope, &path, 0, None, 0..32, None, &app.syntax_theme).unwrap();
        let (syntaxes, themes) = app.highlighter.shared_sets();
        let batch = job.run_with_budget(&rope, &syntaxes, &themes, &mut || false, std::time::Duration::from_secs(10));
        assert!(app.highlighter.apply_batch(batch.clone()));
        app.highlighter.replace_unchanged_snapshots = replace;
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal.draw(|frame| super::draw_editor(frame, &mut app, area)).unwrap();
        states.push((app, terminal, batch));
    }
    assert_eq!(states[0].1.backend().buffer(), states[1].1.backend().buffer());
    let mut samples = [Vec::new(), Vec::new()];
    for round in 0..7 {
        for index in if round % 2 == 0 { [0, 1] } else { [1, 0] } {
            let (app, terminal, template) = &mut states[index];
            let mut elapsed = std::time::Duration::ZERO;
            for _ in 0..30 {
                // Simulate an already completed worker message; parsing and
                // its transport allocation are outside this UI timing.
                let batch = template.clone();
                let started = std::time::Instant::now();
                assert!(app.highlighter.apply_batch(batch));
                terminal.draw(|frame| super::draw_editor(frame, app, area)).unwrap();
                elapsed += started.elapsed();
                std::hint::black_box(terminal.backend().buffer());
            }
            samples[index].push(elapsed / 30);
        }
        assert_eq!(states[0].1.backend().buffer(), states[1].1.backend().buffer());
    }
    for times in &mut samples { times.sort(); }
    eprintln!("syntax refresh + editor frame: replace={:?} reuse={:?}", samples[0][3], samples[1][3]);
}

#[test]
fn local_edits_retain_distant_glyphs_and_owned_text_through_history_and_fold_frames() {
    let mut app = editing_fixture();
    app.editor_mut().unwrap().whitespace_mode = WhitespaceMode::Boundary;
    let area = Rect::new(0, 0, 100, 24);
    draw(&mut app, area);
    let distant = app
        .editor()
        .unwrap()
        .render_line_cache
        .borrow()
        .cached_line(12)
        .unwrap();
    let distant_text = distant
        .owned_text_snapshot()
        .expect("fixture must cross rope chunks");
    for operation in 0..4 {
        match operation {
            0 => app.editor_mut().unwrap().insert_char('x'),
            1 => app.editor_mut().unwrap().backspace(),
            2 => app.editor_mut().unwrap().undo(),
            _ => app.editor_mut().unwrap().redo(),
        }
        draw(&mut app, area);
        let retained = app
            .editor()
            .unwrap()
            .render_line_cache
            .borrow()
            .cached_line(12)
            .unwrap();
        assert!(
            std::rc::Rc::ptr_eq(&distant, &retained),
            "operation={operation} rebuilt a distant line"
        );
        assert!(std::rc::Rc::ptr_eq(
            &distant_text,
            &retained.owned_text_snapshot().unwrap()
        ));
        assert_fresh_glyph_frame(&mut app, area);
    }
    app.editor_mut().unwrap().lsp_folding_ranges = vec![(5, 10)];
    app.editor_mut().unwrap().toggle_fold(5);
    assert!(!app.editor().unwrap().folded_ranges.is_empty());
    assert_fresh_glyph_frame(&mut app, area);
    app.editor_mut().unwrap().unfold_all();
    assert_fresh_glyph_frame(&mut app, area);
    let editor = app.editor_mut().unwrap();
    editor.rope = ropey::Rope::from_str(&editor.rope.to_string().replace("value", "other"));
    draw(&mut app, area);
    let replacement = app
        .editor()
        .unwrap()
        .render_line_cache
        .borrow()
        .cached_line(12)
        .unwrap();
    assert!(
        !std::rc::Rc::ptr_eq(&distant, &replacement),
        "unmarked replacement retained stale text"
    );
    assert_fresh_glyph_frame(&mut app, area);
}

#[test]
#[ignore = "manual release local editing viewport benchmark"]
fn benchmark_incremental_editor_glyph_edits() {
    use std::{hint::black_box, time::Instant};
    let area = Rect::new(0, 0, 100, 24);
    let mut states = Vec::new();
    for full_invalidation in [true, false] {
        let mut app = editing_fixture();
        app.editor_mut().unwrap().whitespace_mode = WhitespaceMode::Boundary;
        app.editor()
            .unwrap()
            .render_line_cache
            .borrow_mut()
            .full_invalidation = full_invalidation;
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| super::draw_editor(frame, &mut app, area))
            .unwrap();
        states.push((app, terminal));
    }
    let mut samples = [Vec::new(), Vec::new()];
    for sample in 0..7 {
        for index in if sample % 2 == 0 { [0, 1] } else { [1, 0] } {
            let (app, terminal) = &mut states[index];
            let started = Instant::now();
            for _ in 0..20 {
                app.editor_mut().unwrap().insert_char('x');
                terminal
                    .draw(|frame| super::draw_editor(frame, app, area))
                    .unwrap();
                black_box(terminal.backend().buffer());
                app.editor_mut().unwrap().backspace();
                terminal
                    .draw(|frame| super::draw_editor(frame, app, area))
                    .unwrap();
                black_box(terminal.backend().buffer());
            }
            samples[index].push(started.elapsed() / 40);
        }
        assert_eq!(
            states[0].1.backend().buffer(),
            states[1].1.backend().buffer()
        );
    }
    for (app, _) in &mut states {
        assert_fresh_glyph_frame(app, area);
    }
    for samples in &mut samples {
        samples.sort();
    }
    eprintln!(
        "local edit + draw clear all glyphs={:?} retain unaffected={:?}",
        samples[0][3], samples[1][3]
    );
}

#[test]
#[ignore = "manual release viewport rendering benchmark"]
fn benchmark_editor_viewport_glyph_cache() {
    use std::{hint::black_box, time::Instant};
    for wrapped in [false, true] {
        let mut app = fixture(wrapped, true);
        app.show_color_preview = false;
        app.bracket_pair_colorization = false;
        app.editor_mut().unwrap().selection_anchor = None;
        app.editor_mut().unwrap().whitespace_mode = WhitespaceMode::Boundary;
        if wrapped {
            app.editor_mut().unwrap().wrap_scroll_offset = 120;
        } else {
            app.editor_mut().unwrap().horizontal_scroll = 8_900;
        }
        let area = Rect::new(0, 0, 80, 20);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        let mut reference = None;
        let mut results = Vec::new();
        for full_scan in [true, false] {
            app.editor()
                .unwrap()
                .render_line_cache
                .borrow_mut()
                .reference_scan = full_scan;
            terminal
                .draw(|frame| super::draw_editor(frame, &mut app, area))
                .unwrap();
            let buffer = terminal.backend().buffer().clone();
            if let Some(reference) = reference.as_ref() {
                assert_eq!(&buffer, reference);
            } else {
                reference = Some(buffer);
            }
            let mut samples = Vec::new();
            for _ in 0..7 {
                let started = Instant::now();
                for _ in 0..20 {
                    terminal
                        .draw(|frame| super::draw_editor(frame, &mut app, area))
                        .unwrap();
                    black_box(terminal.backend().buffer());
                }
                samples.push(started.elapsed() / 20);
            }
            samples.sort();
            results.push(samples[3]);
        }
        eprintln!(
            "viewport wrapped={wrapped} cold rebuild/prefix scan={:?} cached seek={:?}",
            results[0], results[1]
        );
    }
}

#[test]
#[ignore = "manual release horizontal syntax seek benchmark"]
fn benchmark_highlighted_horizontal_seek() {
    use crate::ui_editor_highlight::{HighlightedLine, PLAIN_STYLE, RenderLineCache};
    use std::{hint::black_box, time::Instant};
    let mut app = fixture(false, true);
    let rope = app.editor().unwrap().rope.clone();
    let text = rope.line(0).to_string();
    let clean = text.trim_end_matches(['\r', '\n']);
    let path = app.editor().unwrap().file_path.clone().unwrap();
    let (spans, lengths) = app
        .highlighter
        .highlighted_line_by_index_if_cached(0, Some(&path), None, app.syntax_theme.as_str(), 0)
        .unwrap();
    let view = HighlightedLine::new(
        Some((&spans, lengths.iter().sum())),
        clean,
        clean.chars().count(),
        PLAIN_STYLE,
    );
    let start = black_box(8_900);
    let end = start + 100;
    let mut cache = RenderLineCache::default();
    let glyphs = cache.line(
        0,
        (0, 0, 4),
        &rope,
        &view,
        Some((&spans, &lengths)),
        end,
        false,
    );
    let scan = || {
        let mut output = Vec::new();
        let mut column = 0usize;
        let mut visual = 0usize;
        'spans: for (style, text) in view.iter() {
            for ch in text.trim_end_matches('\n').chars() {
                if column >= end {
                    break 'spans;
                }
                if column >= start {
                    output.push((style, ch, visual));
                }
                visual += if ch == '\t' { 4 - visual % 4 } else { 1 };
                column += 1;
            }
        }
        output
    };
    let seek = || {
        let mut output = Vec::new();
        let mut column = start;
        let first = glyphs
            .span_ends
            .partition_point(|&span_end| span_end <= start);
        for ((style, _), &span_end) in view.iter_from_span(first).zip(&glyphs.span_ends[first..]) {
            if column >= end {
                break;
            }
            for ch in glyphs.chars[column..span_end.min(end)].iter().copied() {
                output.push((style, ch, glyphs.visual_column(column)));
                column += 1;
            }
        }
        output
    };
    assert_eq!(scan(), seek());
    let mut results = Vec::new();
    for cached in [false, true] {
        let mut samples = Vec::new();
        for _ in 0..7 {
            let started = Instant::now();
            for _ in 0..1_000 {
                black_box(if cached { seek() } else { scan() });
            }
            samples.push(started.elapsed() / 1_000);
        }
        samples.sort();
        results.push(samples[3]);
    }
    eprintln!(
        "horizontal syntax traversal previous UTF8 prefix={:?} cached glyph seek={:?}",
        results[0], results[1]
    );
}
