use crate::{
    app::{App, Tab},
    editor::Editor,
    ui_editor_highlight::{
        HighlightedLine, PLAIN_STYLE, RenderLineCache, TodoCursor, WordOccurrenceCursor,
    },
};
use ratatui::{
    Terminal,
    backend::TestBackend,
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use std::rc::Rc;

fn app_for(source: &str, wrapped: bool) -> App {
    let mut editor = Editor::test_with_text(source);
    editor.word_wrap = wrapped;
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.show_minimap = false;
    app.show_breadcrumbs = false;
    app.show_color_preview = false;
    app.sticky_scroll = false;
    app.error_lens = true;
    app.cursor_visible = false;
    app
}

fn draw(app: &mut App, area: Rect) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
    let mut output = None;
    terminal
        .draw(|frame| {
            super::draw_editor(frame, app, area);
            output = Some(frame.buffer_mut().clone());
        })
        .unwrap();
    output.unwrap()
}

fn compare_reference(app: &mut App, area: Rect) {
    let cached = draw(app, area);
    app.editor()
        .unwrap()
        .render_line_cache
        .borrow_mut()
        .reference_decorations = true;
    let reference = draw(app, area);
    app.editor()
        .unwrap()
        .render_line_cache
        .borrow_mut()
        .reference_decorations = false;
    assert_eq!(
        cached, reference,
        "cached metadata/clipped annotations changed terminal cells"
    );
    let counts = {
        let cache = app.editor().unwrap().render_line_cache.borrow();
        (
            cache.occurrence_builds,
            cache.todo_builds,
            cache.inline_color_builds,
        )
    };
    assert_eq!(draw(app, area), reference, "warm frame");
    let cache = app.editor().unwrap().render_line_cache.borrow();
    assert_eq!(
        (
            cache.occurrence_builds,
            cache.todo_builds,
            cache.inline_color_builds
        ),
        counts,
        "unchanged frame rescanned text"
    );
    let (lines, bytes, _) = cache.decoration_storage();
    assert!(lines <= 256 && bytes <= 2 * 1024 * 1024);
}

fn prime(cache: &mut RenderLineCache, rope: &ropey::Rope, line: usize, end: usize) {
    let text = rope.line(line).to_string();
    let clean = text.trim_end_matches(['\r', '\n']);
    let highlighted = HighlightedLine::new(None, clean, clean.chars().count(), PLAIN_STYLE);
    cache.line(line, (0, 0, 4), rope, &highlighted, None, end, false);
}

#[test]
fn cached_inline_colors_survive_cursor_queries_and_invalidate_changed_source_or_named_mode() {
    let mut rope = ropey::Rope::from_str("界 tomato #f00 rgb(1,2,3)\nother #333\n");
    let mut cache = RenderLineCache::default();
    prime(&mut cache, &rope, 0, usize::MAX);
    prime(&mut cache, &rope, 1, usize::MAX);
    cache.prepare_occurrences((0, 0, 4), &rope, "tomato", 6, true);
    let first = cache.inline_colors(0, "界 tomato #f00 rgb(1,2,3)", true);
    let untouched = cache.inline_colors(1, "other #333", true);
    for query in ["界", "rgb", "other"] {
        cache.prepare_occurrences((0, 0, 4), &rope, query, query.chars().count(), true);
        assert!(Rc::ptr_eq(
            &first,
            &cache.inline_colors(0, "界 tomato #f00 rgb(1,2,3)", true)
        ));
    }
    let literals = cache.inline_colors(0, "界 tomato #f00 rgb(1,2,3)", false);
    let mut expected = crate::ui_colors::find_inline_colors("界 tomato #f00 rgb(1,2,3)", false);
    expected.sort_by_key(|&(column, _, _, _)| column);
    assert_eq!(literals.as_ref(), &expected);
    assert!(!Rc::ptr_eq(&first, &literals));

    cache.mark_dirty(0, 0, 0, 1, &rope);
    let end = rope.line_to_char(1);
    rope.remove(0..end);
    rope.insert(0, "blue #0f0\n");
    let text = rope.line(0).to_string();
    let text = text.trim_end_matches(['\r', '\n']);
    let highlighted = HighlightedLine::new(None, text, text.chars().count(), PLAIN_STYLE);
    cache.line(0, (1, 1, 4), &rope, &highlighted, None, usize::MAX, false);
    let edited = cache.inline_colors(0, text, true);
    let mut expected = crate::ui_colors::find_inline_colors(text, true);
    expected.sort_by_key(|&(column, _, _, _)| column);
    assert_eq!(edited.as_ref(), &expected);
    assert!(!Rc::ptr_eq(&first, &edited));
    // Dirty reconciliation intentionally includes adjacent source lines.
    assert!(!Rc::ptr_eq(
        &untouched,
        &cache.inline_colors(1, "other #333", true)
    ));
    let (lines, bytes, _) = cache.decoration_storage();
    assert!(lines <= 256 && bytes <= 2 * 1024 * 1024);
}

#[test]
fn inline_preview_frames_match_scans_when_wrapped_scrolled_selected_and_rethemed() {
    let source = ("界 tomato #f00 coral rgb(1,2,3) gold #00ff00 ".repeat(90) + "\n").repeat(8);
    for wrapped in [false, true] {
        let mut app = app_for(&source, wrapped);
        app.show_color_preview = true;
        app.editor_mut().unwrap().file_path = Some("color-cache.css".into());
        for scroll in [0, 31, 89] {
            app.editor_mut().unwrap().horizontal_scroll = if wrapped { 0 } else { scroll };
            app.editor_mut().unwrap().wrap_scroll_offset = if wrapped { scroll } else { 0 };
            app.editor_mut().unwrap().cursor_col = scroll;
            app.editor_mut().unwrap().selection_anchor = Some((0, scroll.saturating_sub(3)));
            app.colors = if scroll == 31 {
                crate::app_theme::EditorTheme::Light.colors()
            } else {
                crate::app_theme::EditorTheme::Dark.colors()
            };
            compare_reference(&mut app, Rect::new(0, 0, 100, 20));
        }
    }
}

#[test]
fn compact_occurrences_preserve_selection_phase_unicode_boundaries_and_viewport_seeks() {
    for (text, word, whole) in [
        ("abababababa aa aaaa", "aba", false),
        ("界 aaaaaaaa ä ää _ä ä_ e\u{301} ä界", "aa", false),
        ("ä ää _ä ä_ ä xä ä界 Ä ä", "ä", true),
        ("e\u{301} e _e e_ 界e e界", "e", true),
        ("\t TODO _TODO_ XTODOTODO\r\n", "TODO", true),
        ("😀🦀😀🦀😀", "😀🦀", false),
    ] {
        let rope = ropey::Rope::from_str(text);
        let mut cache = RenderLineCache::default();
        cache.prepare_occurrences((0, 0, 4), &rope, word, word.chars().count(), whole);
        prime(&mut cache, &rope, 0, usize::MAX);
        let cached = cache.occurrences(0, text);
        let mut expected = Vec::new();
        crate::ui_editor_highlight::word_occurrences_for_line(
            text,
            word,
            word.chars().count(),
            whole,
            &mut expected,
        );
        for start in 0..text.chars().count() + 2 {
            let mut cursor = WordOccurrenceCursor::new(&cached, start);
            for col in start..text.chars().count() + 2 {
                assert_eq!(
                    cursor.contains(col),
                    expected.iter().any(|&(from, end)| from <= col && col < end),
                    "text={text:?} word={word:?} start={start} col={col}"
                );
            }
        }
        assert!(Rc::ptr_eq(&cached, &cache.occurrences(0, text)));
    }
}

#[test]
fn cached_todos_keep_ascii_boundary_rules_marker_priority_and_theme_changes() {
    let source = "界 TODO FIXME _HACK_ NOTE BUG ÄTODO XTODOTODO TODOx _TODO_\r\n";
    let rope = ropey::Rope::from_str(source);
    let mut cache = RenderLineCache::default();
    prime(&mut cache, &rope, 0, usize::MAX);
    let markers = cache.todos(0, source.trim_end_matches(['\r', '\n']));
    let mut app = app_for(source, false);
    for changed_theme in [false, true] {
        if changed_theme {
            app.colors.fg_todo = Color::Magenta;
            app.colors.fg_fixme = Color::Cyan;
        }
        let mut reference = Vec::new();
        crate::ui_editor_highlight::todo_markers_for_line(
            source,
            &[
                ("TODO", app.colors.fg_todo),
                ("FIXME", app.colors.fg_fixme),
                ("HACK", app.colors.fg_hack),
                ("NOTE", app.colors.fg_info_label),
                ("BUG", app.colors.fg_error),
            ],
            &mut reference,
        );
        for start in 0..source.chars().count() {
            let mut cursor = TodoCursor::new(&markers, start);
            for col in start..source.chars().count() {
                assert_eq!(
                    cursor.at(col).map(|kind| kind.color(&app.colors)),
                    reference
                        .iter()
                        .find(|&&(from, end, _)| from <= col && col < end)
                        .map(|&(_, _, color)| color)
                );
            }
        }
        compare_reference(&mut app, Rect::new(0, 0, 90, 10));
    }
    assert!(Rc::ptr_eq(&markers, &cache.todos(0, source)));
}

#[test]
fn rich_decoration_frames_match_scans_during_horizontal_wrap_and_selection_changes() {
    let source = ("界 ababababababa aa aaaa e\u{301} 👩\u{200d}💻 TODO FIXME _HACK_ NOTE BUG \t "
        .repeat(80)
        + "\r\n")
        .repeat(8);
    for wrapped in [false, true] {
        let mut app = app_for(&source, wrapped);
        for mode in 0..4 {
            let editor = app.editor_mut().unwrap();
            editor.selection_anchor = match mode {
                1 => Some((0, 2)),
                2 => Some((0, 17)),
                3 => Some((1, 0)),
                _ => None,
            };
            (editor.cursor_line, editor.cursor_col) = match mode {
                1 => (0, 5),
                2 => (0, 19),
                3 => (0, 0),
                _ => (0, 0),
            };
            for offset in [0, 1, 17, 257, 1_003] {
                let editor = app.editor_mut().unwrap();
                if wrapped {
                    editor.wrap_scroll_offset = offset.min(35);
                } else {
                    editor.horizontal_scroll = offset;
                }
                compare_reference(&mut app, Rect::new(0, 0, 95, 20));
            }
        }
    }
}

#[test]
fn decoration_data_survives_distant_local_edits_and_rejects_equal_version_replacement() {
    let source = ("ä TODO ä FIXME ä NOTE \t".repeat(120) + "\r\n").repeat(24);
    let mut app = app_for(&source, false);
    let area = Rect::new(0, 0, 100, 22);
    compare_reference(&mut app, area);
    let (occurrences, todos) = {
        let cache = app.editor().unwrap().render_line_cache.borrow();
        (
            cache.cached_occurrences(10).unwrap(),
            cache.cached_todos(10).unwrap(),
        )
    };
    for operation in 0..4 {
        match operation {
            0 => app.editor_mut().unwrap().insert_char(' '),
            1 => app.editor_mut().unwrap().backspace(),
            2 => app.editor_mut().unwrap().undo(),
            _ => app.editor_mut().unwrap().redo(),
        }
        compare_reference(&mut app, area);
        let cache = app.editor().unwrap().render_line_cache.borrow();
        assert!(Rc::ptr_eq(
            &occurrences,
            &cache.cached_occurrences(10).unwrap()
        ));
        assert!(Rc::ptr_eq(&todos, &cache.cached_todos(10).unwrap()));
    }
    app.editor_mut().unwrap().rope = ropey::Rope::from_str(&source.replace("TODO", "HACK"));
    compare_reference(&mut app, area);
    let cache = app.editor().unwrap().render_line_cache.borrow();
    assert!(!Rc::ptr_eq(&todos, &cache.cached_todos(10).unwrap()));
}

#[test]
fn oversized_hidden_selection_keeps_the_combined_cache_bounded_and_query_growth_evicts() {
    let text = "a".repeat(8_192);
    let rope = ropey::Rope::from_str(&(text.clone() + "\n").repeat(128));
    let mut cache = RenderLineCache::default();
    cache.prepare_occurrences((0, 0, 4), &rope, "a", 1, false);
    for line in 0..128 {
        prime(&mut cache, &rope, line, usize::MAX);
    }
    let old_lines = cache.decoration_storage().0;
    let near_cap_query = "🦀".repeat(16_000);
    cache.prepare_occurrences((0, 0, 4), &rope, &near_cap_query, 16_000, false);
    assert!(
        cache.decoration_storage().0 < old_lines,
        "query growth did not reconcile retained bytes immediately"
    );
    assert!(cache.decoration_storage().1 <= 2 * 1024 * 1024);
    let raw_query = " ".repeat(2 * 1024 * 1024) + "ä";
    cache.prepare_occurrences(
        (0, 0, 4),
        &rope,
        &raw_query,
        raw_query.chars().count(),
        false,
    );
    assert_eq!(
        cache.decoration_storage().2,
        0,
        "oversized no-match query retained its raw text"
    );
    let mut app = app_for(&("normal ä TODO\n".repeat(30) + &raw_query + "\n"), false);
    let editor = app.editor_mut().unwrap();
    editor.cursor_line = 30;
    editor.cursor_col = raw_query.chars().count();
    editor.selection_anchor = Some((30, 0));
    assert!(super::selection_occurrence_text(editor).unwrap().len() > 2 * 1024 * 1024);
    compare_reference(&mut app, Rect::new(0, 0, 100, 20));
    assert_eq!(
        app.editor()
            .unwrap()
            .render_line_cache
            .borrow()
            .decoration_storage()
            .2,
        0
    );
}

#[test]
fn clipped_error_lens_matches_full_paragraph_for_wide_combining_and_zwj_edges() {
    let style = Style::default()
        .fg(Color::Yellow)
        .bg(Color::Blue)
        .add_modifier(Modifier::ITALIC);
    let long_message = "plain text ".repeat(500);
    let conjunct = "क्".repeat(6) + "क";
    let conjunct_ascii = conjunct.clone() + "X";
    for message in [
        "\u{301}message",
        "\u{200d}message",
        "a\tZ",
        "\t\u{301}message",
        "a\u{7}Z",
        "a\r\nZ",
        "界界界 tail",
        "👩\u{200d}💻 e\u{301} tail",
        &long_message,
        &conjunct,
        &conjunct_ascii,
    ] {
        for width in 4..20 {
            for prefix in ["x".repeat(width - 4), String::new(), conjunct.clone()] {
                let mut expected = vec![Span::raw(prefix.clone())];
                let mut actual = expected.clone();
                let mut occupied = 0;
                super::advance_error_lens_width(&mut occupied, &prefix, width);
                let mut expected_occupied = occupied;
                super::append_error_lens(
                    &mut super::prepared_text::Rows::new(Rect::new(0, 0, width as u16, 1), Style::default()),
                    &mut expected,
                    &mut String::new(),
                    &mut expected_occupied,
                    width,
                    "◆",
                    message,
                    style,
                    true,
                );
                super::append_error_lens(
                    &mut super::prepared_text::Rows::new(Rect::new(0, 0, width as u16, 1), Style::default()),
                    &mut actual,
                    &mut String::new(),
                    &mut occupied,
                    width,
                    "◆",
                    message,
                    style,
                    false,
                );
                let second = Style::default().fg(Color::Red).bg(Color::Green);
                super::append_error_lens(
                    &mut super::prepared_text::Rows::new(Rect::new(0, 0, width as u16, 1), Style::default()),
                    &mut expected,
                    &mut String::new(),
                    &mut expected_occupied,
                    width,
                    "!",
                    "next",
                    second,
                    true,
                );
                super::append_error_lens(
                    &mut super::prepared_text::Rows::new(Rect::new(0, 0, width as u16, 1), Style::default()),
                    &mut actual,
                    &mut String::new(),
                    &mut occupied,
                    width,
                    "!",
                    "next",
                    second,
                    false,
                );
                let render = |spans| {
                    let mut terminal = Terminal::new(TestBackend::new(width as u16, 1)).unwrap();
                    terminal
                        .draw(|frame| {
                            frame.render_widget(
                                Paragraph::new(Line::from(spans)),
                                Rect::new(0, 0, width as u16, 1),
                            )
                        })
                        .unwrap();
                    terminal.backend().buffer().clone()
                };
                assert_eq!(
                    render(actual),
                    render(expected),
                    "message={message:?} width={width}"
                );
            }
        }
    }
    let mut app = app_for("\n", false);
    let path = std::path::PathBuf::from("annotation.rs");
    app.editor_mut().unwrap().file_path = Some(path.clone());
    for message in [
        "\u{301}message",
        "\u{200d}message",
        "a\tZ",
        "\t\u{301}message",
        "a\u{7}Z",
        "界👩\u{200d}💻 e\u{301} tail",
        &conjunct,
        &conjunct_ascii,
    ] {
        app.lsp_manager.diagnostics.insert(
            path.clone(),
            vec![crate::lsp::LspDiagnostic {
                line: 0,
                col: 0,
                end_line: 0,
                end_col: 1,
                message: message.into(),
                severity: lsp_types::DiagnosticSeverity::WARNING,
            }],
        );
        app.lsp_manager.diagnostics_version += 1;
        for width in 9..30 {
            compare_reference(&mut app, Rect::new(0, 0, width, 8));
        }
    }
}

#[test]
fn bounded_long_line_fallback_does_not_build_rich_decoration_metadata() {
    let mut app = app_for(&("ä TODO ".repeat(4_000) + "\n"), false);
    compare_reference(&mut app, Rect::new(0, 0, 100, 12));
    let cache = app.editor().unwrap().render_line_cache.borrow();
    assert_eq!((cache.occurrence_builds, cache.todo_builds), (0, 0));
}

#[test]
#[ignore = "manual comparative release rich decoration benchmark"]
fn benchmark_rich_decoration_caches() {
    use std::{hint::black_box, time::Instant};
    for (name, source) in [
        (
            "unicode_occurrences",
            ("ä ".repeat(4_096) + "\n").repeat(60),
        ),
        (
            "unicode_todos",
            ("界 TODO ".repeat(1_000) + "\n").repeat(60),
        ),
        (
            "mixed_todos",
            ("ä FIXME HACK NOTE BUG TODO ".repeat(250) + "\n").repeat(60),
        ),
    ] {
        let area = Rect::new(0, 0, 180, 60);
        let mut states = Vec::new();
        for reference in [true, false] {
            let mut app = app_for(&source, false);
            app.editor()
                .unwrap()
                .render_line_cache
                .borrow_mut()
                .reference_decorations = reference;
            let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
            terminal
                .draw(|frame| super::draw_editor(frame, &mut app, area))
                .unwrap();
            states.push((app, terminal));
        }
        assert_eq!(
            states[0].1.backend().buffer(),
            states[1].1.backend().buffer()
        );
        let mut samples = [Vec::new(), Vec::new()];
        for sample in 0..7 {
            for index in if sample % 2 == 0 { [0, 1] } else { [1, 0] } {
                let (app, terminal) = &mut states[index];
                let started = Instant::now();
                for _ in 0..5 {
                    terminal
                        .draw(|frame| super::draw_editor(frame, app, area))
                        .unwrap();
                    black_box(terminal.backend().buffer());
                }
                samples[index].push(started.elapsed() / 5);
            }
            assert_eq!(
                states[0].1.backend().buffer(),
                states[1].1.backend().buffer()
            );
        }
        for samples in &mut samples {
            samples.sort();
        }
        eprintln!(
            "rich decorations {name} reference scans/full suffix={:?} cached/clipped={:?}",
            samples[0][3], samples[1][3]
        );
    }
}
