use super::*;
use crate::{
    app::{ActivePane, Tab},
    background::CancellationToken,
    editor::Editor,
    search_regex::SearchRegex,
    ui_search_controls::{SearchControl, SearchScope},
};
use ratatui::{Terminal, backend::TestBackend};
use std::rc::Rc;

#[test]
fn selected_and_cursor_words_follow_find_options_in_wrapped_and_unwrapped_editors() {
    let source = "foo Foo food FOO xfoo foo_bar\n";
    let occurrence_bg = Color::Rgb(20, 90, 50);
    for wrapped in [false, true] {
        for selected in [false, true] {
            let mut editor = Editor::test_with_text(source);
            editor.word_wrap = wrapped;
            editor.cursor_col = if selected { 3 } else { 1 };
            editor.selection_anchor = selected.then_some((0, 0));
            let mut app = App::test_new();
            app.tabs = vec![Tab::new_file(editor)];
            app.active_tab = 0;
            app.active_pane = ActivePane::Editor;
            app.settings_persistence_enabled = false;
            app.show_line_numbers = false;
            app.show_minimap = false;
            app.show_breadcrumbs = false;
            app.sticky_scroll = false;
            app.cursor_visible = false;
            app.colors.ui.preserve_theme_colors = true;
            app.colors.bg_word_highlight = occurrence_bg;
            app.colors.ui.bg_selection_highlight = occurrence_bg;
            let width = if wrapped { 14 } else { 48 };
            let mut terminal = Terminal::new(TestBackend::new(width, 16)).unwrap();
            let mut previous = None;
            for (case_sensitive, whole_word, expected) in [
                (
                    false,
                    false,
                    vec![(0, 3), (4, 7), (8, 11), (13, 16), (18, 21), (22, 25)],
                ),
                (true, false, vec![(0, 3), (8, 11), (18, 21), (22, 25)]),
                (true, true, vec![(0, 3)]),
                (false, true, vec![(0, 3), (4, 7), (13, 16)]),
                (
                    false,
                    false,
                    vec![(0, 3), (4, 7), (8, 11), (13, 16), (18, 21), (22, 25)],
                ),
            ] {
                if app.search_case_sensitive != case_sensitive {
                    crate::ui_search_controls::activate(
                        &mut app,
                        SearchScope::File,
                        SearchControl::MatchCase,
                    );
                }
                if app.search_whole_word != whole_word {
                    crate::ui_search_controls::activate(
                        &mut app,
                        SearchScope::File,
                        SearchControl::WholeWord,
                    );
                }
                terminal
                    .draw(|frame| draw_editor(frame, &mut app, frame.area()))
                    .unwrap();
                let editor = app.editor().unwrap();
                let cached = editor
                    .render_line_cache
                    .borrow()
                    .cached_occurrences(0)
                    .unwrap();
                let mut cursor = WordOccurrenceCursor::new(&cached, 0);
                for col in 0..source.chars().count() {
                    assert_eq!(
                        cursor.contains(col),
                        expected
                            .iter()
                            .any(|&(start, end)| start <= col && col < end),
                        "wrapped={wrapped} selected={selected} case={case_sensitive} whole={whole_word} col={col}"
                    );
                }
                let colored = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .filter(|cell| cell.bg == occurrence_bg)
                    .count();
                assert_eq!(
                    colored,
                    (expected.len() - usize::from(selected)) * 3,
                    "wrapped={wrapped} selected={selected} case={case_sensitive} whole={whole_word}"
                );
                if let Some(old) = previous {
                    assert!(
                        !Rc::ptr_eq(&old, &cached),
                        "changed Find options reused stale highlights"
                    );
                }
                terminal
                    .draw(|frame| draw_editor(frame, &mut app, frame.area()))
                    .unwrap();
                assert!(Rc::ptr_eq(
                    &cached,
                    &app.editor()
                        .unwrap()
                        .render_line_cache
                        .borrow()
                        .cached_occurrences(0)
                        .unwrap()
                ));
                previous = Some(cached);
                assert_eq!(app.editor().unwrap().rope.to_string(), source);
                assert_eq!(
                    app.editor().unwrap().cursor_col,
                    if selected { 3 } else { 1 }
                );
            }
        }
    }
}

#[test]
fn highlight_matching_shares_find_unicode_case_folding_boundaries_and_literal_rules() {
    for (source, word) in [
        ("k K K kk _k k_ 😀k k😀", "k"),
        ("ſ s S wordſ ſ_word", "ſ"),
        ("σ Σ ς aσ σ界", "σ"),
        ("𐐀 𐐨 a𐐨 𐐨a", "𐐀"),
        ("é É e\u{301} éclair", "é"),
        ("e\u{301} e E e界 😀e", "e"),
        ("foo.bar FOO.BAR fooxbar", "foo.bar"),
        ("banana BANANA", "ana"),
    ] {
        for case_sensitive in [false, true] {
            for whole_word in [false, true] {
                let regex =
                    SearchRegex::cached_editor(word, case_sensitive, whole_word, false).unwrap();
                let expected = regex
                    .scan(
                        &ropey::Rope::from_str(source),
                        None,
                        usize::MAX,
                        &CancellationToken::new(),
                    )
                    .0;
                let expected: Vec<_> = expected
                    .into_iter()
                    .map(|(line, start, end)| {
                        assert_eq!(line, 0);
                        (start, end)
                    })
                    .collect();
                let mut actual = Vec::new();
                word_occurrences_for_line(
                    source,
                    word,
                    word.chars().count(),
                    case_sensitive,
                    whole_word,
                    &mut actual,
                );
                assert_eq!(
                    actual, expected,
                    "source={source:?} word={word:?} case={case_sensitive} whole={whole_word}"
                );
            }
        }
    }
}

#[test]
fn cached_highlights_refresh_after_settings_change_without_rebuilding_line_glyphs() {
    let source = "foo FOO food";
    let rope = ropey::Rope::from_str(source);
    let mut cache = RenderLineCache::default();
    let highlighted = HighlightedLine::new(None, source, source.chars().count(), PLAIN_STYLE);
    let mut old = None;
    for (case_sensitive, whole_word, count) in [
        (true, true, 1),
        (false, true, 2),
        (false, false, 3),
        (true, false, 2),
    ] {
        cache.prepare_occurrences((0, 0, 4), &rope, "foo", 3, case_sensitive, whole_word);
        cache.line(0, (0, 0, 4), &rope, &highlighted, None, usize::MAX, false);
        let current = cache.occurrences(0, source);
        let mut cursor = WordOccurrenceCursor::new(&current, 0);
        assert_eq!(
            (0..source.chars().count())
                .filter(|&col| cursor.contains(col))
                .count(),
            count * 3
        );
        if let Some(old) = old {
            assert!(!Rc::ptr_eq(&old, &current));
        }
        assert!(Rc::ptr_eq(&current, &cache.occurrences(0, source)));
        old = Some(current);
    }
    assert_eq!(
        cache.builds, 1,
        "search options should invalidate occurrences, not line glyphs"
    );
}
