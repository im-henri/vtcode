use super::*;
use crate::{animation_config::AnimationEffect, app::Tab, app_theme::EditorTheme, editor::Editor};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use std::{collections::VecDeque, hint::black_box, sync::Arc, time::Instant};

#[test]
fn cursor_word_cache_reuses_words_and_respects_unicode_boundaries() {
    let mut editor =
        Editor::test_with_text("prefix_界long_identifier café e\u{301} \t second\r\nend");
    let mut key = (0, 0, 0, 0);
    let mut word = None;
    let mut cache = VecDeque::new();
    refresh_cursor_word(&editor, &mut key, &mut word, &mut cache);
    let original = word.clone().unwrap();
    for col in 0..23 {
        editor.cursor_col = col;
        refresh_cursor_word(&editor, &mut key, &mut word, &mut cache);
        assert!(Arc::ptr_eq(&original, word.as_ref().unwrap()));
    }
    for line in 0..editor.line_count() + 1 {
        editor.cursor_line = line;
        for col in 0..50 {
            editor.cursor_col = col;
            refresh_cursor_word(&editor, &mut key, &mut word, &mut cache);
            assert_eq!(
                word.as_deref(),
                editor.word_at_cursor().as_deref(),
                "{line}:{col}"
            );
        }
    }
}

#[test]
fn cursor_word_cache_invalidates_edits_undo_and_equal_version_sources() {
    let mut editor = Editor::test_with_text("alpha beta\n");
    let mut key = (0, 0, 0, 0);
    let mut word = None;
    let mut cache = VecDeque::new();
    for step in 0..5 {
        match step {
            1 => editor.insert_char('x'),
            2 => editor.undo(),
            3 => editor.redo(),
            4 => editor.rope = ropey::Rope::from_str("changed word\n"),
            _ => {}
        }
        refresh_cursor_word(&editor, &mut key, &mut word, &mut cache);
        assert_eq!(word.as_deref(), editor.word_at_cursor().as_deref());
        assert_eq!(cache.len(), 1);
    }
    assert_eq!(word.as_deref(), Some("changed"));
}

#[test]
fn occurrence_targets_share_the_render_word_through_motion_edits_and_split_views() {
    let mut app = crate::app::App::test_new();
    app.status_bar_pins.insert("word_occ".into(), true);
    app.tabs = vec![Tab::new_file(Editor::test_with_text("long_界_identifier next\r\nother"))];
    app.active_tab = 0;
    let original = app.word_occurrence_target().unwrap().word;
    for col in 0..17 {
        app.tabs[0].editor.cursor_col = col;
        let target = app.word_occurrence_target().unwrap();
        refresh_cursor_word(&app.tabs[0].editor, &mut app.cached_cursor_word_key,
            &mut app.cached_cursor_word, app.editor_cursor_word_cache.get_mut());
        assert!(Arc::ptr_eq(&original, &target.word));
        assert!(Arc::ptr_eq(&original, app.cached_cursor_word.as_ref().unwrap()));
        assert!(Arc::ptr_eq(&target.word, &target.clone().word));
    }
    for step in 0..7 {
        let editor = &mut app.tabs[0].editor;
        match step {
            0 => editor.insert_char('x'),
            1 => editor.undo(),
            2 => editor.redo(),
            3 => editor.rope = ropey::Rope::from_str("replaced_word entirely_different"),
            4 => { editor.cursor_col = 0; editor.selection_anchor = Some((0, 5)); }
            5 => { editor.selection_anchor = None; editor.cursor_line = 1; }
            _ => { editor.cursor_line = 0; editor.large_file_mode = true; }
        }
        let expected = app.editor().filter(|ed| !ed.has_selection() && !ed.large_file_mode)
            .and_then(Editor::word_at_cursor);
        assert_eq!(app.word_occurrence_target().map(|target| target.word).as_deref(), expected.as_deref());
    }
    app.tabs[0].editor.large_file_mode = false;
    let first = app.word_occurrence_target().unwrap().word;
    app.tabs.push(Tab::new_file(Editor::test_with_text("other_view")));
    app.active_tab = 1;
    assert_eq!(app.word_occurrence_target().unwrap().word.as_ref(), "other_view");
    app.active_tab = 0;
    assert!(Arc::ptr_eq(&first, &app.word_occurrence_target().unwrap().word));
    app.search_active = true;
    assert!(app.word_occurrence_target().is_none());
}

#[test]
#[ignore = "manual release basic editor benchmark"]
fn benchmark_basic_occurrence_targets() {
    for length in [32, 4096] {
        let mut app = crate::app::App::test_new();
        app.status_bar_pins.insert("word_occ".into(), true);
        app.tabs = vec![Tab::new_file(Editor::test_with_text(&("界".repeat(length) + "\n")))];
        app.active_tab = 0;
        let expected = app.word_occurrence_target().unwrap();
        let mut before = Vec::new();
        let mut after = Vec::new();
        for sample in 0..7 {
            for optimized in [sample % 2 == 0, sample % 2 != 0] {
                let start = Instant::now();
                for col in 0..2000 {
                    app.tabs[0].editor.cursor_col = col % length;
                    if optimized {
                        black_box(app.word_occurrence_target());
                    } else {
                        // The previous target owned a String, with no Arc conversion.
                        black_box(app.editor_analysis_target().and_then(|editor| {
                            app.editor().unwrap().word_at_cursor().map(|word| (editor, word))
                        }));
                    }
                }
                let elapsed = start.elapsed() / 2000;
                if optimized { after.push(elapsed); } else { before.push(elapsed); }
            }
        }
        assert_eq!(app.word_occurrence_target(), Some(expected));
        before.sort(); after.sort();
        eprintln!("basic occurrence targets {length} Unicode chars: before={:?} after={:?}", before[3], after[3]);
    }
}

#[test]
fn selection_words_reuse_text_and_reject_stale_or_unbounded_entries() {
    let mut cache = VecDeque::new();
    let mut editor = Editor::test_with_text("  selected_界  \nsecond line\n");
    editor.selection_anchor = Some((0, 0));
    editor.cursor_col = editor.line_len(0);
    let original = cached_selection_occurrence_text(&editor, &mut cache).unwrap();
    assert!(Arc::ptr_eq(
        &original,
        &cached_selection_occurrence_text(&editor, &mut cache).unwrap()
    ));
    for step in 0..5 {
        match step {
            0 => editor.cursor_col -= 1,
            1 => {
                editor.cursor_line = 1;
                editor.cursor_col = 4;
            }
            2 => {
                editor.cursor_line = 0;
                editor.rope = ropey::Rope::from_str("different word\n");
            }
            3 => {
                editor.selection_anchor = None;
            }
            _ => {
                editor.selection_anchor = Some((0, 0));
                editor.insert_char('x');
            }
        }
        assert_eq!(
            cached_selection_occurrence_text(&editor, &mut cache).as_deref(),
            selection_occurrence_text(&editor).as_deref()
        );
    }
    for size in [32, 5_000] {
        for _ in 0..40 {
            let mut editor = Editor::test_with_text(&(" ".repeat(size) + "text"));
            editor.selection_anchor = Some((0, 0));
            editor.cursor_col = editor.line_len(0);
            assert_eq!(
                cached_selection_occurrence_text(&editor, &mut cache).as_deref(),
                selection_occurrence_text(&editor).as_deref()
            );
            assert!(cache.len() <= MAX_CACHED_EDITOR_PANES);
            assert!(
                cache
                    .iter()
                    .all(|entry| entry.text.as_ref().is_none_or(|text| text.len() <= 4096))
            );
        }
    }
}

fn gutter_fixture(wrapped: bool, bounded: bool, theme: EditorTheme) -> App {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text(
        &"fn sample() {\n    let result = calculate(界, value, argument);\n}\n\n".repeat(30),
    );
    editor.file_path = Some("gutter.rs".into());
    editor.word_wrap = wrapped;
    editor.large_file_mode = bounded;
    editor.cursor_line = 5;
    editor.cursor_col = 12;
    editor.selection_anchor = Some((2, 0));
    editor.bookmarks.extend([0, 5, 12, 20]);
    editor.folded_ranges.insert(8, 11);
    editor.set_git_line_status(
        (0..editor.line_count())
            .map(|line| match line % 4 {
                0 => '+',
                1 => 'M',
                2 => '-',
                _ => ' ',
            })
            .collect(),
    );
    app.tabs = vec![Tab::new_file(editor)];
    app.active_pane = crate::app::ActivePane::Editor;
    app.colors = theme.colors();
    app.show_minimap = false;
    app.sticky_scroll = false;
    app.cursor_blinking = false;
    app.hovered_gutter_line = Some(4);
    for effect in AnimationEffect::ALL {
        app.animation_settings.set(effect, false);
    }
    app
}

fn render_gutter(app: &mut App, width: u16, height: u16, reference: bool) -> Buffer {
    gutter::REFERENCE.set(reference);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| draw_editor(frame, app, frame.area()))
        .unwrap();
    gutter::REFERENCE.set(false);
    terminal.backend().buffer().clone()
}

#[test]
fn direct_gutter_matches_paragraph_for_scrolling_selection_folds_and_themes() {
    for theme in [EditorTheme::Dark, EditorTheme::Light] {
        for (wrapped, bounded) in [(false, false), (true, false), (false, true)] {
            for (width, height) in [(120, 32), (28, 18), (8, 4), (2, 1)] {
                let mut app = gutter_fixture(wrapped, bounded, theme);
                for variant in 0..4 {
                    app.show_line_numbers = variant != 3;
                    app.relative_line_numbers = variant == 1;
                    app.tabs[0].editor.scroll_offset = if variant == 2 { 114 } else { 0 };
                    app.tabs[0].editor.wrap_scroll_offset = if variant == 2 { 114 } else { 0 };
                    let reference = render_gutter(&mut app, width, height, true);
                    assert_eq!(
                        render_gutter(&mut app, width, height, false),
                        reference,
                        "wrapped={wrapped} bounded={bounded} {width}x{height} variant={variant}"
                    );
                }
            }
        }
    }
}

#[test]
#[ignore = "manual release cursor word cache benchmark"]
fn benchmark_cursor_word_responsiveness() {
    for length in [32, 4096] {
        for reference in [true, false] {
            let mut editor = Editor::test_with_text(&("界".repeat(length) + "\n"));
            let mut key = (0, 0, 0, 0);
            let mut word = None;
            let mut cache = VecDeque::new();
            refresh_cursor_word(&editor, &mut key, &mut word, &mut cache);
            let mut samples = Vec::new();
            for _ in 0..7 {
                let start = Instant::now();
                for col in 0..4000 {
                    editor.cursor_col = col % length;
                    if reference {
                        black_box(editor.word_at_cursor());
                    } else {
                        refresh_cursor_word(&editor, &mut key, &mut word, &mut cache);
                    }
                }
                samples.push(start.elapsed() / 4000);
            }
            assert_eq!(word.as_deref(), editor.word_at_cursor().as_deref());
            samples.sort();
            eprintln!(
                "cursor word chars={length} reference={reference}: {:?}",
                samples[3]
            );
        }
    }
}
