use super::*;
use crate::editor::Editor;

fn prepare_query_cache() -> (ropey::Rope, RenderLineCache) {
    let text = "alpha beta TODO alpha beta";
    let rope = ropey::Rope::from_str(&(text.to_owned() + "\n").repeat(240));
    let mut cache = RenderLineCache::default();
    cache.prepare_occurrences((0, 0, 4), &rope, "alpha", 5, true);
    for line in 0..240 {
        let highlighted = HighlightedLine::new(None, text, text.len(), PLAIN_STYLE);
        cache.line(line, (0, 0, 4), &rope, &highlighted, None, 80, false);
        cache.todos(line, text);
        cache.occurrences(line, text);
    }
    (rope, cache)
}

#[test]
fn cursor_word_changes_invalidate_lazily_without_dropping_offscreen_rows() {
    let (rope, mut cache) = prepare_query_cache();
    let old = cache.cached_occurrences(200).unwrap();
    let retained = std::rc::Rc::downgrade(&old);
    drop(old);
    let count = cache.occurrence_builds;
    for query in ["beta", "TODO", "a", "界", "", "alpha"] {
        cache.prepare_occurrences((0, 0, 4), &rope, query, query.chars().count(), true);
        assert!(retained.upgrade().is_some(), "changing the caret word swept offscreen rows");
        assert!(cache.cached_occurrences(200).is_none(), "a stale query must never match");
        let occurrences = cache.occurrences(0, "alpha beta TODO alpha beta");
        let mut expected = Vec::new();
        word_occurrences_for_line("alpha beta TODO alpha beta", query, query.chars().count(), true, &mut expected);
        let mut cursor = WordOccurrenceCursor::new(&occurrences, 0);
        for col in 0..30 {
            assert_eq!(cursor.contains(col), expected.iter().any(|&(a, b)| a <= col && col < b));
        }
        assert_eq!(cache.decoration_bytes, cache.decorations.values().map(LineDecorations::bytes).sum::<usize>()
            + cache.occurrence_query.as_ref().unwrap().0.capacity());
        assert!(cache.storage_bytes + cache.decoration_bytes <= RenderLineCache::MAX_BYTES);
    }
    assert_eq!(cache.occurrence_builds, count + 6);
    cache.occurrence_generation = u64::MAX;
    cache.prepare_occurrences((0, 0, 4), &rope, "beta", 4, true);
    assert!(cache.cached_occurrences(200).is_none());
    assert!(retained.upgrade().is_none(), "epoch wrap must retire old query stamps");
    assert_eq!(cache.decoration_bytes, cache.decorations.values().map(LineDecorations::bytes).sum::<usize>() + 4);
}

#[test]
#[ignore = "manual optimized-profile comparison"]
fn benchmark_cursor_word_query_invalidation() {
    use std::{hint::black_box, time::Instant};
    let mut samples = [Vec::new(), Vec::new()];
    for round in 0..9 {
        for reference in [round % 2 == 0, round % 2 != 0] {
            let (rope, mut cache) = prepare_query_cache();
            cache.reference_query_invalidation = reference;
            let started = Instant::now();
            for index in 0..10_000 {
                let query = if index % 2 == 0 { "beta" } else { "alpha" };
                cache.prepare_occurrences((0, 0, 4), &rope, black_box(query), query.len(), true);
            }
            samples[usize::from(reference)].push(started.elapsed() / 10_000);
        }
    }
    for sample in &mut samples { sample.sort(); }
    eprintln!("cursor word cache invalidation: old={:?}, new={:?}", samples[1][4], samples[0][4]);
}

fn cached_width(editor: &Editor, first: usize, end: usize, cursor: usize) -> usize {
    editor.render_line_cache.borrow_mut().visible_line_width(
        (
            editor.content_version,
            editor.wrap_revision,
            editor.tab_size,
        ),
        &editor.rope,
        first,
        end,
        cursor,
    )
}

fn check(editor: &Editor, first: usize, end: usize, cursor: usize) {
    let expected = (first..end.min(editor.line_count()))
        .map(|line| editor.line_len(line))
        .max()
        .unwrap_or(0)
        .max(editor.line_len(cursor));
    assert_eq!(cached_width(editor, first, end, cursor), expected);
}

#[test]
fn viewport_width_reuses_metadata_during_navigation_and_tracks_offscreen_cursor() {
    let editor = Editor::test_with_text(&format!("a\r\né界\n\n{}\r\ntail\n", "b".repeat(50_000)));
    for _ in 0..30 {
        for cursor in 0..editor.line_count() {
            check(&editor, 0, 3, cursor);
        }
    }
    assert_eq!(editor.render_line_cache.borrow().width_builds, 1);
    check(&editor, 2, 5, 0);
    assert_eq!(editor.render_line_cache.borrow().width_builds, 2);
    check(&editor, 900, 900, 0);
    check(&editor, 900, 901, 999);
}

#[test]
fn viewport_width_invalidates_for_edits_newlines_history_and_same_version_replacement() {
    let mut editor = Editor::test_with_text("abcd\r\ne界\nlast\n");
    check(&editor, 0, 3, 0);
    editor.set_cursor(0, 4);
    editor.insert_char('x');
    check(&editor, 0, 3, 0);
    editor.insert_newline();
    check(&editor, 0, 3, 3);
    editor.undo();
    check(&editor, 0, 3, 0);
    editor.redo();
    check(&editor, 0, 3, 0);
    editor.rope = ropey::Rope::from_str("0123456789\r\nshort\n");
    check(&editor, 0, 3, 0);
    editor.rope = ropey::Rope::from_str("");
    check(&editor, 0, 0, 0);
}

#[test]
fn prepared_bracket_guides_preserve_overlap_order_and_replacement() {
    let source = vec![
        (4, 2, 9, 3),
        (4, 0, 12, 1),
        (0, 1, 8, 2),
        (8, 7, 7, 2),
        (1, 8, 3, 4),
    ];
    let mut prepared = BracketGuides::from(source.clone());
    for line in 0..16 {
        for col in 0..10 {
            let expected = source
                .iter()
                .find(|&&(c, s, e, _)| c == col && line >= s && line < e)
                .map(|&(_, s, e, d)| (s, e, d));
            assert_eq!(prepared.at(line, col).copied(), expected);
        }
    }
    prepared.clear();
    assert!(prepared.is_empty());
    assert_eq!(prepared.at(3, 4), None);
    prepared = vec![(4, 1, 4, 8)].into();
    assert_eq!(prepared.at(3, 4), Some(&(1, 4, 8)));
}

#[test]
#[ignore = "manual optimized-profile comparison"]
fn benchmark_viewport_width_metadata() {
    use std::{hint::black_box, time::Instant};
    let mut editor = Editor::test_with_text(&"界 value\r\n".repeat(50_000));
    editor.cursor_line = 20_010;
    check(&editor, 20_000, 20_060, editor.cursor_line);
    let mut samples = [Vec::new(), Vec::new()];
    for sample in 0..7 {
        for cached in [sample % 2 == 0, sample % 2 != 0] {
            let started = Instant::now();
            for _ in 0..3_000 {
                let width = if cached {
                    cached_width(black_box(&editor), 20_000, 20_060, editor.cursor_line)
                } else {
                    (20_000..20_060)
                        .map(|line| black_box(&editor).line_len(line))
                        .max()
                        .unwrap_or(0)
                        .max(editor.current_line_len())
                };
                black_box(width);
            }
            samples[usize::from(cached)].push(started.elapsed() / 3_000);
        }
    }
    samples.iter_mut().for_each(|s| s.sort());
    eprintln!(
        "viewport width: per-frame scans={:?} retained={:?}",
        samples[0][3], samples[1][3]
    );
}
