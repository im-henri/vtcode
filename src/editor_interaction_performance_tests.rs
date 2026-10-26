use super::Editor;

#[test]
fn configured_wrap_column_reuses_the_render_layout_for_navigation_and_scrolling() {
    let operations: &[(&str, fn(&mut Editor))] = &[
        ("ensure visible", Editor::ensure_visible),
        ("center", Editor::center_cursor),
        ("up", Editor::move_up),
        ("down", Editor::move_down),
        ("page up", Editor::page_up),
        ("page down", Editor::page_down),
        ("home", Editor::move_to_line_start),
        ("end", Editor::move_to_line_end),
        ("scroll up", |editor| editor.scroll_up(2)),
        ("scroll down", |editor| editor.scroll_down(3)),
        ("multi up", Editor::multi_cursor_move_up),
        ("multi down", Editor::multi_cursor_move_down),
        ("multi select up", Editor::multi_cursor_select_move_up),
        ("multi select down", Editor::multi_cursor_select_move_down),
    ];
    for large in [false, true] {
        let text = "abcdefghijklmnop\n".repeat(if large { 20_000 } else { 30 });
        for &(name, operation) in operations {
            let mut actual = Editor::test_with_text(&text);
            let mut expected = Editor::test_with_text(&text);
            actual.viewport_width = 80;
            actual.word_wrap_column = 4;
            expected.viewport_width = 4;
            for editor in [&mut actual, &mut expected] {
                editor.word_wrap = true;
                editor.viewport_height = 7;
                editor.cursor_line = 8;
                editor.cursor_col = 6;
                editor.selection_anchor = Some((8, 1));
                editor.extra_cursors = vec![(5, 9), (12, 6)];
                editor.extra_cursor_anchors = vec![Some((5, 3)), None];
                let offsets =
                    Editor::build_wrap_row_offsets(&editor.rope, 4, editor.tab_size, &[], || false)
                        .unwrap();
                assert!(editor.install_wrap_layout(4, offsets));
                editor.set_wrap_scroll_offset(32);
            }
            let ranges = actual.wrap_ranges_for_line(8, 4);
            operation(&mut actual);
            operation(&mut expected);
            assert_eq!(
                (
                    actual.cursor_line,
                    actual.cursor_col,
                    actual.selection_anchor,
                    actual.desired_col,
                    actual.scroll_offset,
                    actual.wrap_scroll_offset
                ),
                (
                    expected.cursor_line,
                    expected.cursor_col,
                    expected.selection_anchor,
                    expected.desired_col,
                    expected.scroll_offset,
                    expected.wrap_scroll_offset
                ),
                "{name}, large={large}",
            );
            assert_eq!(actual.extra_cursors, expected.extra_cursors, "{name}");
            assert_eq!(
                actual.extra_cursor_anchors, expected.extra_cursor_anchors,
                "{name}"
            );
            assert!(actual.wrap_layout_ready(4), "{name}, large={large}");
            assert!(
                std::rc::Rc::ptr_eq(&ranges, &actual.wrap_ranges_for_line(8, 4)),
                "{name}"
            );
        }
    }
}

#[test]
fn wrapped_navigation_keeps_segment_boundary_and_eof_positions() {
    let mut editor = Editor::test_with_text("abcdefghijklmnop");
    editor.word_wrap = true;
    editor.viewport_width = 4;
    editor.viewport_height = 4;
    for (column, row) in [(0, 0), (3, 0), (4, 1), (8, 2), (15, 3), (16, 3)] {
        assert_eq!(editor.wrapped_row_for_position(0, column, 4), row);
    }
    editor.cursor_col = 8;
    editor.move_to_line_start();
    assert_eq!(editor.cursor_col, 4);
    editor.move_to_line_start();
    assert_eq!(editor.cursor_col, 0);
    editor.cursor_col = 5;
    editor.move_to_line_end();
    assert_eq!(editor.cursor_col, 8);
    editor.move_to_line_end();
    assert_eq!(editor.cursor_col, 16);
    editor.move_to_line_end();
    assert_eq!(editor.cursor_col, 16);
}

#[test]
fn word_navigation_preserves_unicode_camel_case_punctuation_and_crlf() {
    for text in ["fooBarBaz\r\n", "välueÄnderung_42\r\n", "foo::bar\r\n"] {
        let mut editor = Editor::test_with_text(text);
        let stops: &[usize] = match text {
            "fooBarBaz\r\n" => &[0, 3, 6, 9],
            "välueÄnderung_42\r\n" => &[0, 5, 16],
            _ => &[0, 3, 5, 8],
        };
        for &expected in &stops[1..] {
            editor.move_word_right();
            assert_eq!(editor.cursor_col, expected, "right: {text}");
        }
        for &expected in stops[..stops.len() - 1].iter().rev() {
            editor.move_word_left();
            assert_eq!(editor.cursor_col, expected, "left: {text}");
        }
        assert_eq!(editor.cursor_line, 0);
    }
}

#[test]
fn multiple_cursor_word_selection_uses_the_same_camel_case_stops() {
    let mut editor = Editor::test_with_text("fooBarBaz\nvälueÄnderung_42");
    editor.extra_cursors = vec![(1, 0)];
    editor.extra_cursor_anchors = vec![None];
    editor.multi_cursor_select_move_word_right();
    assert_eq!(editor.cursor_col, 3);
    assert_eq!(editor.selection_anchor, Some((0, 0)));
    assert_eq!(editor.extra_cursors, vec![(1, 5)]);
    assert_eq!(editor.extra_cursor_anchors, vec![Some((1, 0))]);
    editor.multi_cursor_move_word_right();
    assert_eq!(editor.cursor_col, 6);
    assert_eq!(editor.extra_cursors, vec![(1, 16)]);
    editor.multi_cursor_move_word_left();
    assert_eq!(editor.cursor_col, 3);
    assert_eq!(editor.extra_cursors, vec![(1, 5)]);
    editor.multi_cursor_move_word_left();
    assert_eq!(editor.cursor_col, 0);
    assert_eq!(editor.extra_cursors, vec![(1, 0)]);
}

#[test]
fn word_selection_and_completion_cross_rope_chunks_without_losing_unicode() {
    // Span multiple rope chunks while keeping both sides of the caret within
    // completion's existing 4,096-character scan limit.
    let word = "ä界_".repeat(2_048);
    let mut editor = Editor::test_with_text(&format!("  {word}\r\nnext"));
    editor.cursor_col = 3_000;
    assert_eq!(editor.word_boundaries_at(0, 3_000), (2, 6_146));
    assert_eq!(editor.word_at_cursor().as_deref(), Some(word.as_str()));
    assert_eq!(
        editor.word_prefix_at_cursor().unwrap().chars().count(),
        2_998
    );
    editor.select_word_at_cursor();
    assert_eq!(editor.selected_text().as_deref(), Some(word.as_str()));
    editor.clear_selection();
    editor.cursor_col = 3_000;
    editor.complete_word("replacement");
    assert_eq!(editor.rope.to_string(), "  replacement\r\nnext");
    editor.undo();
    assert_eq!(editor.rope.to_string(), format!("  {word}\r\nnext"));
}

#[test]
fn smart_home_scans_only_indentation_and_keeps_toggle_and_blank_line_behavior() {
    for (indent, count) in [(" \t", 2), ("\u{2003} ", 2), ("", 0)] {
        let mut editor = Editor::test_with_text(&format!("{indent}{}\r\n", "x".repeat(100_000)));
        editor.cursor_col = 50_000;
        editor.move_to_line_start();
        assert_eq!(editor.cursor_col, count);
        editor.move_to_line_start();
        assert_eq!(editor.cursor_col, 0);
        editor.move_to_line_end();
        assert_eq!(editor.cursor_col, count + 100_000);
    }
    let mut editor = Editor::test_with_text("  \t\r\n");
    editor.cursor_col = 2;
    editor.move_to_line_start();
    assert_eq!(editor.cursor_col, 0);
}

/// Keep the old indexed scan here only as a benchmark reference, so results
/// can be reproduced on one machine without checking out another revision.
fn indexed_word_boundaries(editor: &Editor, col: usize) -> (usize, usize) {
    let line = editor.rope.line(0);
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut start = col;
    while start > 0 && is_word(line.char(start - 1)) {
        start -= 1;
    }
    let mut end = col;
    while end < line.len_chars() && is_word(line.char(end)) {
        end += 1;
    }
    (start, end)
}

#[test]
#[ignore = "manual release benchmark"]
fn benchmark_word_boundaries_and_smart_home() {
    use std::{hint::black_box, time::Instant};
    for unicode in [false, true] {
        let source = if unicode {
            "ä界_".repeat(10_922)
        } else {
            "x".repeat(32_768)
        };
        let editor = Editor::test_with_text(&source);
        let col = editor.line_len(0) / 2;
        let expected = (0, editor.line_len(0));
        let mut before = Vec::new();
        let mut after = Vec::new();
        for sample in 0..7 {
            for optimized in [sample % 2 == 0, sample % 2 != 0] {
                let started = Instant::now();
                for _ in 0..16 {
                    let range = if optimized {
                        editor.word_boundaries_at(0, black_box(col))
                    } else {
                        indexed_word_boundaries(&editor, black_box(col))
                    };
                    assert_eq!(black_box(range), expected);
                }
                if optimized {
                    after.push(started.elapsed());
                } else {
                    before.push(started.elapsed());
                }
            }
        }
        before.sort();
        after.sort();
        eprintln!(
            "32k word boundaries unicode={unicode}: before={:?} after={:?} per operation",
            before[3] / 16,
            after[3] / 16
        );
    }

    let mut editor = Editor::test_with_text(&format!("    {}", "x".repeat(2 * 1024 * 1024)));
    let mut before = Vec::new();
    let mut after = Vec::new();
    for sample in 0..7 {
        for optimized in [sample % 2 == 0, sample % 2 != 0] {
            let started = Instant::now();
            for _ in 0..128 {
                editor.cursor_col = black_box(1_000_000);
                if optimized {
                    editor.move_to_line_start();
                } else {
                    let text = editor.rope.line(0).to_string();
                    editor.cursor_col = crate::editor::leading_whitespace_count(&text);
                    editor.ensure_visible();
                }
                assert_eq!(black_box(editor.cursor_col), 4);
            }
            if optimized {
                after.push(started.elapsed());
            } else {
                before.push(started.elapsed());
            }
        }
    }
    before.sort();
    after.sort();
    eprintln!(
        "2MiB line Home: before={:?} after={:?} per operation",
        before[3] / 128,
        after[3] / 128
    );
}

#[test]
fn streaming_occurrences_preserve_unicode_ranges_chunk_boundaries_and_non_overlap() {
    let text = format!("{}ä界ä界ä\r\nä界", "x".repeat(1_019));
    let rope = ropey::Rope::from_str(&text);
    let needle: Vec<char> = "ä界ä".chars().collect();
    let needle_text: String = needle.iter().collect();
    let search = super::RopeSearch::new(&needle_text);
    assert_eq!(
        search
            .matches(&rope, 0, rope.len_chars())
            .collect::<Vec<_>>(),
        vec![1_019]
    );
    assert_eq!(
        search.matches(&rope, 1_020, 1_024).collect::<Vec<_>>(),
        vec![1_021]
    );
    assert_eq!(search.matches(&rope, 0, 1_021).next(), None);
    assert_eq!(
        search
            .matches(&rope, rope.len_chars() + 1, rope.len_chars())
            .next(),
        None
    );
    assert_eq!(search.matches(&rope, 0, rope.len_chars() + 1).next(), None);
    assert_eq!(
        super::RopeSearch::new("")
            .matches(&rope, 0, rope.len_chars())
            .next(),
        None
    );
}

#[test]
fn occurrence_selection_skips_selected_matches_wraps_and_terminates() {
    let mut editor = Editor::test_with_text("ä ä ä");
    editor.selection_anchor = Some((0, 0));
    editor.cursor_col = 1;
    editor.extra_cursors = vec![(0, 3)];
    editor.extra_cursor_anchors = vec![Some((0, 2))];
    editor.select_next_occurrence();
    assert_eq!(editor.cursor_col, 5);
    assert_eq!(editor.selection_anchor, Some((0, 4)));
    assert_eq!(editor.extra_cursors.len(), 2);
    editor.select_next_occurrence();
    assert_eq!(editor.cursor_col, 5);
    assert_eq!(editor.extra_cursors.len(), 2);
    editor.extra_cursors.remove(0);
    editor.extra_cursor_anchors.remove(0);
    editor.select_next_occurrence();
    assert_eq!(editor.cursor_col, 3);
    assert_eq!(editor.selection_anchor, Some((0, 2)));
}

#[test]
fn select_all_occurrences_streams_many_matches_and_keeps_multiline_anchors() {
    let mut editor = Editor::test_with_text(&"ä界\n".repeat(4_096));
    editor.selection_anchor = Some((0, 0));
    editor.cursor_line = 1;
    editor.cursor_col = 0;
    editor.select_all_occurrences();
    assert_eq!(editor.extra_cursors.len(), 4_095);
    assert_eq!(editor.selection_anchor, Some((0, 0)));
    assert_eq!((editor.cursor_line, editor.cursor_col), (1, 0));
    assert_eq!(editor.extra_cursors.last(), Some(&(4_096, 0)));
    assert_eq!(editor.extra_cursor_anchors.last(), Some(&Some((4_095, 0))));
}

#[test]
fn paste_replacement_saves_one_snapshot_and_undo_restores_selection() {
    let mut editor = Editor::test_with_text("hello world");
    editor.selection_anchor = Some((0, 0));
    editor.cursor_col = 5;
    let version = editor.content_version;
    editor.paste_with_text("goodbye");
    assert_eq!(editor.undo_stack.len(), 1);
    assert_eq!(editor.content_version, version + 1);
    assert_eq!(editor.rope.to_string(), "goodbye world");
    editor.undo();
    assert_eq!(editor.rope.to_string(), "hello world");
    assert_eq!(editor.selection_anchor, Some((0, 0)));
    assert_eq!(editor.cursor_col, 5);
}

#[test]
fn deleting_no_selection_does_not_invalidate_content_or_create_undo() {
    let mut editor = Editor::test_with_text("text");
    let version = editor.content_version;
    assert_eq!(editor.delete_selection(), None);
    assert_eq!(editor.content_version, version);
    assert!(editor.undo_stack.is_empty());
}

#[test]
fn multicursor_empty_replacement_merges_overlap_and_maps_selected_and_eof_cursors() {
    let mut editor = Editor::test_with_text("abcdefghij");
    editor.selection_anchor = Some((0, 1));
    editor.cursor_col = 5;
    editor.extra_cursors = vec![(0, 7), (0, 10), (0, 4)];
    editor.extra_cursor_anchors = vec![Some((0, 3)), None, None];
    editor.multi_cursor_paste("");
    assert_eq!(editor.rope.to_string(), "ahij");
    assert_eq!((editor.cursor_line, editor.cursor_col), (0, 1));
    // The live replacement kernel merges carets that land at the primary one.
    assert_eq!(editor.extra_cursors, vec![(0, 4)]);
    assert!(editor.extra_cursor_anchors.iter().all(Option::is_none));
    assert_eq!(editor.undo_stack.len(), 1);
    editor.undo();
    assert_eq!(editor.rope.to_string(), "abcdefghij");
}

#[test]
fn multicursor_empty_replacement_maps_carets_across_several_lines() {
    let mut editor = Editor::test_with_text("aa\nbb\ncc\ndd\nee");
    editor.selection_anchor = Some((0, 1));
    editor.cursor_line = 1;
    editor.cursor_col = 1;
    editor.extra_cursors = vec![(3, 1), (4, 2)];
    editor.extra_cursor_anchors = vec![Some((2, 1)), None];
    editor.multi_cursor_paste("");
    assert_eq!(editor.rope.to_string(), "ab\ncd\nee");
    assert_eq!((editor.cursor_line, editor.cursor_col), (0, 1));
    assert_eq!(editor.extra_cursors, vec![(1, 1), (2, 2)]);
    assert_eq!(editor.undo_stack.len(), 1);
    editor.undo();
    assert_eq!(editor.rope.to_string(), "aa\nbb\ncc\ndd\nee");
}

#[test]
fn same_line_paste_adjusts_every_caret_for_preceding_insertions() {
    for (paste, expected, carets) in [
        ("界", "a界bc界d", vec![(0, 2), (0, 5)]),
        ("X\nYY", "aXbcYYd", vec![(0, 2), (0, 6)]),
        ("x\ny\nz", "ax\ny\nzbcx\ny\nzd", vec![(2, 1), (4, 1)]),
    ] {
        let mut editor = Editor::test_with_text("abcd");
        editor.cursor_col = 1;
        editor.extra_cursors = vec![(0, 3)];
        editor.extra_cursor_anchors = vec![None];
        editor.paste_with_text(paste);
        assert_eq!(editor.rope.to_string(), expected, "{paste:?}");
        assert_eq!(
            (editor.cursor_line, editor.cursor_col),
            carets[0],
            "{paste:?}"
        );
        assert_eq!(editor.extra_cursors, carets[1..], "{paste:?}");
        editor.undo();
        assert_eq!(editor.rope.to_string(), "abcd");
    }
}

#[test]
fn newline_handles_long_unicode_lines_and_indent_without_materializing_content() {
    let content = "界".repeat(100_000);
    let mut editor = Editor::test_with_text(&format!("\u{2003} {content}\r\n"));
    editor.cursor_col = 50_002;
    editor.insert_newline();
    assert_eq!((editor.cursor_line, editor.cursor_col), (1, 2));
    assert_eq!(editor.line_len(0), 50_002);
    assert_eq!(editor.line_len(1), 50_002);
    assert_eq!(editor.rope.line(1).slice(..2).to_string(), "\u{2003} ");
    editor.undo();
    assert_eq!(editor.rope.to_string(), format!("\u{2003} {content}\r\n"));
}

#[test]
fn newline_keeps_language_triggers_and_single_mutation_bracket_expansion() {
    for (language, source, col, expected, caret) in [
        ("Python", "    if ready:", 13, "    if ready:\n        ", 8),
        ("Rust", "    value =>", 12, "    value =>\n        ", 8),
        ("Plain Text", "  {}", 3, "  {\n      \n  }", 6),
        ("Plain Text", "  }", 3, "  }\n  ", 2),
        ("Plain Text", "    }", 5, "    }\n    ", 4),
    ] {
        let mut editor = Editor::test_with_text(source);
        editor.language_override = Some(language.into());
        editor.cursor_col = col;
        editor.insert_newline();
        assert_eq!(editor.rope.to_string(), expected, "{language}: {source}");
        assert_eq!((editor.cursor_line, editor.cursor_col), (1, caret));
        assert_eq!(editor.undo_stack.len(), 1);
    }
}

#[test]
fn grouped_typing_invalidates_once_per_edit_and_keeps_undo_boundaries_local() {
    let mut editor = Editor::test_with_text("line\nline");
    let version = editor.content_version;
    editor.insert_char('a');
    editor.insert_char('b');
    assert_eq!(editor.content_version, version + 2);
    assert_eq!(editor.undo_stack.len(), 1);
    editor.cursor_line = 1;
    editor.cursor_col = 0;
    editor.break_undo_group();
    editor.insert_char('c');
    assert_eq!(editor.content_version, version + 3);
    assert_eq!(editor.undo_stack.len(), 2);
    assert_eq!(editor.undo_stack.front().unwrap().dirty_through, 0);
    assert_eq!(editor.undo_stack.back().unwrap().dirty_from, 1);
}

#[test]
fn bracket_matching_walks_chunked_lines_and_ignores_quotes_comments_and_escapes() {
    let source = format!("({} \"escaped \\\" )\" /* ) */ (x))", "界".repeat(20_000));
    let editor = Editor::test_with_text(&source);
    let end = editor.line_len(0) - 1;
    assert_eq!(editor.find_matching_bracket_for('(', 0, 0), Some((0, end)));
    assert_eq!(editor.find_matching_bracket_for(')', 0, end), Some((0, 0)));
    assert_eq!(editor.find_matching_bracket_for('(', 20, 0), None);
}

/// Previous occurrence search retained only for interleaved local benchmarks.
fn legacy_ring_find(
    rope: &ropey::Rope,
    needle: &[char],
    start: usize,
    end: usize,
) -> Option<usize> {
    let mut ring = Vec::with_capacity(needle.len());
    for (offset, c) in rope.chars_at(start).take(end - start).enumerate() {
        if ring.len() < needle.len() {
            ring.push(c);
        } else {
            ring[offset % needle.len()] = c;
        }
        if offset + 1 >= needle.len() {
            let base = offset + 1 - needle.len();
            if needle
                .iter()
                .enumerate()
                .all(|(index, &c)| ring[(base + index) % needle.len()] == c)
            {
                return Some(start + base);
            }
        }
    }
    None
}

#[test]
#[ignore = "manual release benchmark"]
fn benchmark_occurrence_search_repeated_prefixes_and_many_matches() {
    use std::{hint::black_box, time::Instant};
    for unicode in [false, true] {
        let repeated = if unicode { 'ä' } else { 'a' };
        let last = if unicode { '界' } else { 'b' };
        let text: String = std::iter::repeat_n(repeated, 65_536)
            .chain(std::iter::once(last))
            .collect();
        let needle: Vec<char> = std::iter::repeat_n(repeated, 255)
            .chain(std::iter::once(last))
            .collect();
        let rope = ropey::Rope::from_str(&text);
        let needle_text: String = needle.iter().collect();
        let search = super::RopeSearch::new(&needle_text);
        let mut before = Vec::new();
        let mut after = Vec::new();
        for sample in 0..7 {
            for optimized in [sample % 2 == 0, sample % 2 != 0] {
                let started = Instant::now();
                for _ in 0..2 {
                    let found = if optimized {
                        search.matches(&rope, 0, rope.len_chars()).next()
                    } else {
                        legacy_ring_find(&rope, &needle, 0, rope.len_chars())
                    };
                    assert_eq!(black_box(found), Some(65_281));
                }
                if optimized {
                    after.push(started.elapsed());
                } else {
                    before.push(started.elapsed());
                }
            }
        }
        before.sort();
        after.sort();
        eprintln!(
            "64k occurrence search / 256-char repeated-prefix pattern unicode={unicode}: before={:?} after={:?} per operation",
            before[3] / 2,
            after[3] / 2
        );
    }
    let rope = ropey::Rope::from_str(&"word ".repeat(20_000));
    let needle: Vec<char> = "word".chars().collect();
    let needle_text: String = needle.iter().collect();
    let search = super::RopeSearch::new(&needle_text);
    let mut before = Vec::new();
    let mut after = Vec::new();
    for sample in 0..7 {
        for optimized in [sample % 2 == 0, sample % 2 != 0] {
            let started = Instant::now();
            let matches = if optimized {
                search.matches(&rope, 0, rope.len_chars()).count()
            } else {
                let mut count = 0;
                let mut start = 0;
                while let Some(found) = legacy_ring_find(&rope, &needle, start, rope.len_chars()) {
                    count += 1;
                    start = found + needle.len();
                }
                count
            };
            assert_eq!(black_box(matches), 20_000);
            if optimized {
                after.push(started.elapsed());
            } else {
                before.push(started.elapsed());
            }
        }
    }
    before.sort();
    after.sort();
    eprintln!(
        "20k occurrence matches / 100k document: before={:?} after={:?} per operation",
        before[3], after[3]
    );
}

/// Original Enter path for the benchmark's plain long line. This deliberately
/// retains its two full-line materializations and separate rope insertions.
fn legacy_plain_line_newline(editor: &mut Editor) {
    editor.desired_col = None;
    editor.save_undo();
    let idx = editor.rope.line_to_char(editor.cursor_line) + editor.cursor_col;
    let line = editor.rope.line(editor.cursor_line);
    let indent = match line.as_str() {
        Some(text) => super::leading_whitespace(text).to_string(),
        None => super::leading_whitespace(&line.to_string()).to_string(),
    };
    let full_line = line.to_string();
    let trimmed = full_line.trim_end_matches(['\n', '\r', ' ']);
    assert!(!matches!(
        std::hint::black_box(trimmed.chars().next_back()),
        Some('{' | '(' | '[' | '}' | ')' | ']')
    ));
    editor.rope.insert_char(idx, '\n');
    let new_start = editor.rope.line_to_char(editor.cursor_line + 1);
    editor.rope.insert(new_start, &indent);
    editor.cursor_line += 1;
    editor.cursor_col = indent.chars().count();
    editor.modified = true;
    editor.shift_bookmarks(editor.cursor_line, 1);
    editor.clear_stale_folds();
    editor.ensure_visible();
}

#[test]
#[ignore = "manual release benchmark"]
fn benchmark_newline_and_backspace_avoid_long_line_work() {
    use std::{hint::black_box, time::Instant};
    let original = ropey::Rope::from_str(&format!("    {}", "x".repeat(2 * 1024 * 1024)));
    let mut editor = Editor::test_with_text("");
    let mut before = Vec::new();
    let mut after = Vec::new();
    for sample in 0..7 {
        for optimized in [sample % 2 == 0, sample % 2 != 0] {
            let mut elapsed = std::time::Duration::ZERO;
            for _ in 0..8 {
                editor.rope = original.clone();
                editor.cursor_line = 0;
                editor.cursor_col = 1_000_000;
                editor.undo_stack.clear();
                editor.break_undo_group();
                let started = Instant::now();
                if optimized {
                    editor.insert_newline();
                } else {
                    legacy_plain_line_newline(&mut editor);
                }
                elapsed += started.elapsed();
                assert_eq!(black_box((editor.cursor_line, editor.cursor_col)), (1, 4));
            }
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
        "2MiB line Enter: before={:?} after={:?} per operation",
        before[3] / 8,
        after[3] / 8
    );

    let original = ropey::Rope::from_str(&format!("{}x", " ".repeat(100_000)));
    before.clear();
    after.clear();
    for sample in 0..7 {
        for optimized in [sample % 2 == 0, sample % 2 != 0] {
            let mut elapsed = std::time::Duration::ZERO;
            for _ in 0..4 {
                editor.rope = original.clone();
                editor.cursor_line = 0;
                editor.cursor_col = 100_001;
                editor.undo_stack.clear();
                editor.break_undo_group();
                let started = Instant::now();
                if optimized {
                    editor.backspace();
                } else {
                    editor.desired_col = None;
                    editor.maybe_save_undo();
                    let all_spaces = (0..editor.cursor_col).all(|col| editor.rope.char(col) == ' ');
                    assert!(!black_box(all_spaces));
                    editor.rope.remove(editor.cursor_col - 1..editor.cursor_col);
                    editor.cursor_col -= 1;
                    editor.modified = true;
                    editor.ensure_visible();
                }
                elapsed += started.elapsed();
                assert_eq!(black_box(editor.cursor_col), 100_000);
            }
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
        "100k indentation Backspace after first code character: before={:?} after={:?} per operation",
        before[3] / 4,
        after[3] / 4
    );
}

#[test]
fn overlapping_paste_preserves_clipboard_newlines_after_selections_merge() {
    let mut editor = Editor::test_with_text("abcdef");
    editor.selection_anchor = Some((0, 1));
    editor.cursor_col = 4;
    editor.extra_cursors = vec![(0, 5)];
    editor.extra_cursor_anchors = vec![Some((0, 2))];
    editor.paste_with_text("X\nYY");
    // There is now one effective selection, so paste the complete clipboard
    // once instead of concatenating its lines at the same insertion point.
    assert_eq!(editor.rope.to_string(), "aX\nYYf");
    assert_eq!((editor.cursor_line, editor.cursor_col), (1, 2));
    assert!(editor.extra_cursors.is_empty());
    editor.undo();
    assert_eq!(editor.rope, "abcdef");
    editor.redo();
    assert_eq!(editor.rope, "aX\nYYf");
}

#[test]
fn paste_resets_vertical_navigation_preferred_column() {
    let mut editor = Editor::test_with_text("abcdef\nabcdef");
    editor.cursor_col = 2;
    editor.desired_col = Some(5);
    editor.paste_with_text("XY");
    assert_eq!(editor.cursor_col, 4);
    assert_eq!(editor.desired_col, None);
    editor.move_down();
    assert_eq!(editor.cursor_col, 4);
}

#[test]
fn plain_paste_keeps_literal_indentation_normalizes_crlf_and_has_atomic_undo() {
    let mut editor = Editor::test_with_text("    old\n    keep");
    editor.selection_anchor = Some((0, 4));
    editor.cursor_col = 7;
    editor.desired_col = Some(11);
    let version = editor.content_version;
    editor.paste_plain_with_text("界\r\n x\ry\r\r\nz");
    assert_eq!(editor.rope.to_string(), "    界\n x\ny\n\nz\n    keep");
    assert_eq!((editor.cursor_line, editor.cursor_col), (4, 1));
    assert_eq!(editor.desired_col, None);
    assert_eq!(editor.content_version, version + 1);
    assert_eq!(editor.undo_stack.len(), 1);
    editor.undo();
    assert_eq!(editor.rope.to_string(), "    old\n    keep");
    assert_eq!(editor.selection_anchor, Some((0, 4)));
    assert_eq!((editor.cursor_line, editor.cursor_col), (0, 7));
}

#[test]
fn plain_paste_without_selection_has_one_snapshot_and_empty_paste_does_nothing() {
    let mut editor = Editor::test_with_text("ac");
    editor.cursor_col = 1;
    editor.paste_plain_with_text("ä");
    assert_eq!(editor.rope.to_string(), "aäc");
    assert_eq!(editor.cursor_col, 2);
    assert_eq!(editor.undo_stack.len(), 1);
    let version = editor.content_version;
    editor.paste_plain_with_text("");
    assert_eq!(editor.content_version, version);
    assert_eq!(editor.undo_stack.len(), 1);
}
