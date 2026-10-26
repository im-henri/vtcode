use super::*;
use std::{hint::black_box, time::Instant};

fn reference_brackets(editor: &Editor, start: usize, end: usize) -> Option<(usize, usize)> {
    let openers = ['(', '[', '{'];
    let closers = [')', ']', '}'];
    // Independent oracle: explicitly pair every delimiter using stacks.
    let mut stacks = [Vec::new(), Vec::new(), Vec::new()];
    let mut enclosing = None;
    for (index, ch) in editor.rope.chars().enumerate() {
        if let Some(kind) = openers.iter().position(|&open| ch == open) {
            stacks[kind].push(index);
        } else if let Some(kind) = closers.iter().position(|&close| ch == close) {
            if let Some(open) = stacks[kind].pop() {
                if open < start && index >= end && enclosing.is_none_or(|(previous, _)| open > previous) {
                    enclosing = Some((open, index));
                }
            }
        }
    }
    enclosing
}

fn reference_expand(editor: &mut Editor) {
    editor.desired_col = None;
    editor.remember_selection();
    if !editor.has_selection() {
        let line = editor.rope.line(editor.cursor_line);
        if editor.cursor_col < line.len_chars()
            && (line.char(editor.cursor_col).is_alphanumeric()
                || line.char(editor.cursor_col) == '_')
        {
            let (start, end) = editor.word_boundaries_at(editor.cursor_line, editor.cursor_col);
            editor.selection_anchor = Some((editor.cursor_line, start));
            editor.cursor_col = end;
        } else {
            editor.selection_anchor = Some((editor.cursor_line, 0));
            editor.cursor_col = editor.line_len(editor.cursor_line);
        }
        return;
    }
    let (sl, sc, el, ec) = editor.selection_range().unwrap();
    let start = editor.rope.line_to_char(sl) + sc;
    let end = editor.rope.line_to_char(el) + ec;
    if let Some((open, close)) = reference_brackets(editor, start, end) {
        let (start, end) = if open + 1 == start && close == end {
            (open, close + 1)
        } else {
            (open + 1, close)
        };
        let sl = editor.rope.char_to_line(start);
        let el = editor.rope.char_to_line(end);
        editor.selection_anchor = Some((sl, start - editor.rope.line_to_char(sl)));
        editor.cursor_line = el;
        editor.cursor_col = end - editor.rope.line_to_char(el);
    } else {
        editor.select_all();
    }
}

fn assert_navigation_equal(actual: &Editor, expected: &Editor) {
    assert_eq!(
        (
            actual.cursor_line,
            actual.cursor_col,
            actual.selection_anchor,
            actual.desired_col
        ),
        (
            expected.cursor_line,
            expected.cursor_col,
            expected.selection_anchor,
            expected.desired_col
        )
    );
    assert_eq!(actual.extra_cursors, expected.extra_cursors);
    assert_eq!(actual.extra_cursor_anchors, expected.extra_cursor_anchors);
    assert_eq!(
        (
            actual.scroll_offset,
            actual.horizontal_scroll,
            actual.wrap_scroll_offset
        ),
        (
            expected.scroll_offset,
            expected.horizontal_scroll,
            expected.wrap_scroll_offset
        )
    );
    assert_eq!(actual.selection_history, expected.selection_history);
    assert_eq!(actual.modified, expected.modified);
    assert_eq!(actual.content_version, expected.content_version);
    assert_eq!(actual.undo_stack.len(), expected.undo_stack.len());
    assert_eq!(actual.redo_stack.len(), expected.redo_stack.len());
}

fn fixture(source: &str, line: usize, col: usize, wrapped: bool) -> Editor {
    let mut editor = Editor::test_with_text(source);
    editor.cursor_line = line;
    editor.cursor_col = col;
    editor.viewport_height = 7;
    editor.viewport_width = 11;
    editor.word_wrap = wrapped;
    editor.desired_col = Some(17);
    editor.selection_anchor = Some((0, 0));
    editor
}

#[test]
fn enclosing_pairs_match_stack_oracle_with_malformed_unicode_nesting() {
    let alphabet = ['(', ')', '[', ']', '{', '}', 'x', '界', '\r', '\n'];
    let mut random = 0x1937a4u64;
    for _ in 0..240 {
        let mut source = String::new();
        for _ in 0..90 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            source.push(alphabet[(random >> 32) as usize % alphabet.len()]);
        }
        let editor = Editor::test_with_text(&source);
        for _ in 0..8 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let first = (random >> 32) as usize % (editor.rope.len_chars() + 1);
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let second = (random >> 32) as usize % (editor.rope.len_chars() + 1);
            let (start, end) = (first.min(second), first.max(second));
            assert_eq!(
                editor.enclosing_bracket_range(start, end),
                reference_brackets(&editor, start, end),
                "source={source:?} start={start} end={end}"
            );
        }
    }
}

#[test]
fn selection_expansion_preserves_inner_delimiters_history_direction_and_shrink() {
    for source in [
        "([界word])",
        "{outer\r\n[inner(value)]\r\n}",
        "(([{word xxxx",
        "x[y)]z",
        "word",
    ] {
        for reverse in [false, true] {
            let mut actual = fixture(source, 0, 0, false);
            let mut expected = fixture(source, 0, 0, false);
            let byte = source
                .find("word")
                .or_else(|| source.find("value"))
                .unwrap_or(1);
            let start = source[..byte].chars().count();
            let end = (start + 2).min(actual.rope.len_chars());
            let position = |editor: &Editor, index: usize| {
                let line = editor.rope.char_to_line(index);
                (line, index - editor.rope.line_to_char(line))
            };
            for editor in [&mut actual, &mut expected] {
                let (anchor, cursor) = if reverse { (end, start) } else { (start, end) };
                editor.selection_anchor = Some(position(editor, anchor));
                (editor.cursor_line, editor.cursor_col) = position(editor, cursor);
            }
            for _ in 0..6 {
                actual.expand_selection();
                reference_expand(&mut expected);
                assert_navigation_equal(&actual, &expected);
            }
            for _ in 0..6 {
                actual.shrink_selection();
                expected.shrink_selection();
                assert_navigation_equal(&actual, &expected);
            }
        }
    }
}

fn reference_paragraph(editor: &mut Editor, operation: usize) {
    let blank = |line| editor.rope.line(line).chars().all(char::is_whitespace);
    let max = editor.line_count() - 1;
    if operation == 0 {
        if editor.cursor_line == 0 {
            return;
        }
        let mut line = editor.cursor_line - 1;
        while line > 0 && !blank(line) {
            line -= 1;
        }
        while line > 0 && blank(line) {
            line -= 1;
        }
        editor.cursor_line = line;
        editor.cursor_col = 0;
    } else if operation == 1 {
        if editor.cursor_line >= max {
            return;
        }
        let mut line = editor.cursor_line + 1;
        while line < max && !blank(line) {
            line += 1;
        }
        while line < max && blank(line) {
            line += 1;
        }
        editor.cursor_line = line;
        editor.cursor_col = 0;
    } else {
        let mut start = editor.cursor_line;
        while start > 0 && !blank(start - 1) {
            start -= 1;
        }
        let mut end = editor.cursor_line;
        while end < max && !blank(end + 1) {
            end += 1;
        }
        editor.desired_col = None;
        editor.selection_anchor = Some((start, 0));
        editor.cursor_line = end;
        editor.cursor_col = editor.line_len(end);
    }
    editor.ensure_visible();
}

#[test]
fn paragraph_iterators_preserve_boundaries_blank_cursor_crlf_unicode_and_folds() {
    let large = "word\n".repeat(3_000);
    for source in [
        "",
        "a",
        "\n",
        "\r\n\r\n",
        "a\n\n b\n\n\n c\n",
        "\u{2003}\r\nα\r\n界\r\n\t\r\n\u{a0}\r\nlast",
        " a\r\nb\rc\n\nend",
        large.as_str(),
    ] {
        for wrapped in [false, true] {
            let total = ropey::Rope::from_str(source).len_lines();
            let lines = if total < 20 {
                (0..total).collect::<Vec<_>>()
            } else {
                vec![0, 1, total / 2, total - 1]
            };
            for line in lines {
                for operation in 0..3 {
                    let mut actual = fixture(source, line, 1, wrapped);
                    let mut expected = fixture(source, line, 1, wrapped);
                    for editor in [&mut actual, &mut expected] {
                        editor.cursor_col = editor.cursor_col.min(editor.line_len(line));
                        if total > 6 {
                            editor.folded_ranges.extend([(1, 4), (5, 7)]);
                        }
                    }
                    match operation {
                        0 => actual.move_paragraph_up(),
                        1 => actual.move_paragraph_down(),
                        _ => actual.select_paragraph(),
                    }
                    reference_paragraph(&mut expected, operation);
                    assert_navigation_equal(&actual, &expected);
                }
            }
        }
    }
}

fn reference_home(editor: &mut Editor, selecting: bool) {
    let mut states = editor.collect_multi_cursors();
    for state in &mut states {
        if selecting && state.anchor.is_none() {
            state.anchor = Some((state.line, state.col));
        }
        state.line = state.line.min(editor.line_count() - 1);
        let len = editor.line_len(state.line);
        let line = editor.rope.line(state.line);
        let first = if let Some(text) = line.as_str() {
            crate::editor::leading_whitespace_count(text)
        } else {
            line.chars()
                .take_while(|c| c.is_whitespace() && *c != '\n')
                .count()
        };
        state.col = state.col.min(len);
        state.col = if state.col == first || first >= len {
            0
        } else {
            first
        };
    }
    if selecting {
        editor.set_multi_cursor_states(states);
    } else {
        editor.set_multi_cursor_positions(
            states
                .into_iter()
                .map(|state| (state.line, state.col))
                .collect(),
        );
    }
}

fn reference_words(editor: &mut Editor, left: bool, selecting: bool) {
    let mut states = editor.collect_multi_cursors();
    for state in &mut states {
        if selecting && state.anchor.is_none() {
            state.anchor = Some((state.line, state.col));
        }
        (state.line, state.col) = if left {
            editor.word_left_position(state.line, state.col)
        } else {
            editor.word_right_position(state.line, state.col)
        };
    }
    if selecting {
        editor.set_multi_cursor_states(states);
    } else {
        editor.set_multi_cursor_positions(
            states
                .into_iter()
                .map(|state| (state.line, state.col))
                .collect(),
        );
    }
}

fn cursor_fixture(source: &str, positions: &[(usize, usize)], wrapped: bool) -> Editor {
    let mut editor = fixture(source, positions[0].0, positions[0].1, wrapped);
    editor.extra_cursors.extend_from_slice(&positions[1..]);
    editor.extra_cursor_anchors.extend(
        positions[1..]
            .iter()
            .enumerate()
            .map(|(index, &position)| (index % 3 == 0).then_some(position)),
    );
    editor
}

#[test]
fn shared_line_home_preserves_toggle_clamping_and_anchor_precedence() {
    let large_indent = " \t\u{2003}".repeat(5_000);
    for source in [
        "    α界word\r\n\t\tthing\n\u{2003}value",
        large_indent.as_str(),
        "word\r\n\r\nlast",
    ] {
        let rope = ropey::Rope::from_str(source);
        let positions: Vec<_> = (0..80)
            .map(|index| (index % rope.len_lines(), (index * 17) % 15_020))
            .collect();
        for selecting in [false, true] {
            let mut actual = cursor_fixture(source, &positions, false);
            let mut expected = cursor_fixture(source, &positions, false);
            for _ in 0..3 {
                if selecting {
                    actual.multi_cursor_select_move_to_line_start();
                } else {
                    actual.multi_cursor_move_to_line_start();
                }
                reference_home(&mut expected, selecting);
                assert_navigation_equal(&actual, &expected);
            }
        }
    }
}

#[test]
fn word_interval_reuse_matches_each_independent_caret_for_unicode_camelcase_and_punctuation() {
    let words = "fooBar9ABC äÄ_界 x-X abcDEF 123ABC\t \u{2003}...!? ".repeat(2);
    let source = format!("{words}\r\n\r\n{words}\n\nlast\nend");
    let rope = ropey::Rope::from_str(&source);
    // Exercise every character and both sides of EOL rather than repeatedly
    // sampling equivalent copies of the same tokens. Reverse/rotate the input
    // and include duplicate/stale carets to retain grouping and anchor cases.
    let mut positions: Vec<_> = (0..rope.len_lines())
        .flat_map(|line| {
            (0..=rope.line(line).len_chars() + 2)
                .rev()
                .map(move |col| (line, col))
        })
        .collect();
    positions.extend([
        (0, 0),
        (0, 0),
        (0, 3),
        (0, 3),
        (1, usize::MAX),
        (rope.len_lines() + 5, 17),
    ]);
    let rotation = positions.len() / 3;
    positions.rotate_left(rotation);
    for wrapped in [false, true] {
        for left in [false, true] {
            for selecting in [false, true] {
                let mut actual = cursor_fixture(&source, &positions, wrapped);
                let mut expected = cursor_fixture(&source, &positions, wrapped);
                // Keep the primary cursor valid for ensure_visible; secondary
                // positions still exercise normalization after stale metadata.
                for editor in [&mut actual, &mut expected] {
                    editor.cursor_line = 0;
                    editor.cursor_col = editor.cursor_col.min(editor.line_len(0));
                    editor.folded_ranges.insert(2, 5);
                }
                for _ in 0..3 {
                    match (left, selecting) {
                        (true, true) => actual.multi_cursor_select_move_word_left(),
                        (true, false) => actual.multi_cursor_move_word_left(),
                        (false, true) => actual.multi_cursor_select_move_word_right(),
                        (false, false) => actual.multi_cursor_move_word_right(),
                    }
                    reference_words(&mut expected, left, selecting);
                    assert_navigation_equal(&actual, &expected);
                }
            }
        }
    }
}

#[test]
fn word_interval_endpoints_keep_every_camel_punctuation_whitespace_and_crlf_transition() {
    for source in [
        "fooBar",
        "aABCDef",
        "foo99BAR_äÄ界",
        "...!?  \twordNext",
        "word \u{2003}\t\r\nnext",
        "\r\n",
        "",
    ] {
        let len = ropey::Rope::from_str(source).line(0).len_chars();
        let mut positions: Vec<_> = (0..len + 12).rev().map(|col| (0, col)).collect();
        positions.rotate_left(3);
        for left in [false, true] {
            let mut actual = cursor_fixture(source, &positions, false);
            let mut expected = cursor_fixture(source, &positions, false);
            if left {
                actual.multi_cursor_select_move_word_left();
            } else {
                actual.multi_cursor_select_move_word_right();
            }
            reference_words(&mut expected, left, true);
            assert_navigation_equal(&actual, &expected);
        }
    }
}

fn reference_insert_line(editor: &mut Editor, below: bool) {
    editor.desired_col = None;
    editor.save_undo();
    let line = editor.rope.line(editor.cursor_line).to_string();
    let base_indent = crate::editor::leading_whitespace(&line).to_owned();
    let extra = if below {
        matches!(
            line.trim_end_matches(['\n', '\r', ' ']).chars().next_back(),
            Some('{' | '(' | '[')
        )
    } else {
        matches!(line.trim_start().chars().next(), Some('}' | ')' | ']'))
    };
    let one = if editor.use_tabs {
        "\t"
    } else {
        &crate::helpers::spaces(editor.tab_size)
    };
    let indent = if extra {
        format!("{base_indent}{one}")
    } else {
        base_indent
    };
    let start = editor.rope.line_to_char(editor.cursor_line);
    if below {
        let end = start + editor.rope.line(editor.cursor_line).len_chars();
        let insertion = if line.ends_with('\n') {
            format!("{indent}\n")
        } else {
            format!("\n{indent}")
        };
        editor.rope.insert(end, &insertion);
        editor.cursor_line += 1;
    } else {
        editor.rope.insert(start, &format!("{indent}\n"));
    }
    editor.cursor_col = indent.chars().count();
    editor.modified = true;
    editor.shift_bookmarks(editor.cursor_line, 1);
    editor.clear_stale_folds();
    editor.ensure_visible();
}

#[test]
fn new_line_commands_copy_only_indent_and_preserve_unicode_crlf_bookmarks_and_undo() {
    let large_line = "word ".repeat(30_000);
    for source in [
        "",
        "\r\n",
        "\u{2003}\t}\r\nnext",
        " \t界 {   \r\nnext",
        "  {\t\r\n",
        large_line.as_str(),
    ] {
        for below in [false, true] {
            for use_tabs in [false, true] {
                let mut actual = fixture(source, 0, 0, false);
                let mut expected = fixture(source, 0, 0, false);
                for editor in [&mut actual, &mut expected] {
                    editor.use_tabs = use_tabs;
                    editor.bookmarks.extend([0, 1, 2]);
                    editor.folded_ranges.insert(0, 3);
                }
                if below {
                    actual.insert_line_below();
                } else {
                    actual.insert_line_above();
                }
                reference_insert_line(&mut expected, below);
                assert_eq!(actual.rope.to_string(), expected.rope.to_string());
                assert_eq!(actual.bookmarks, expected.bookmarks);
                assert_eq!(actual.folded_ranges, expected.folded_ranges);
                assert_navigation_equal(&actual, &expected);
                actual.undo();
                expected.undo();
                assert_eq!(actual.rope.to_string(), source);
                assert_navigation_equal(&actual, &expected);
            }
        }
    }
}

#[test]
fn html_close_tag_candidate_seek_preserves_prefix_parser_for_long_unicode_lines() {
    for tag in [
        "div",
        "span class=\"x\"",
        "/div",
        "input",
        "br/",
        "界tag",
        "!--comment--",
        "?xml",
        "div>>",
        "a attr=\"<inner\"",
    ] {
        let prefix = "α界prefix ".repeat(8_000) + "<" + tag;
        let mut actual = Editor::test_with_text(&(prefix.clone() + " suffix"));
        actual.language_override = Some("HTML".into());
        actual.cursor_col = prefix.chars().count();
        let before = prefix.clone() + ">";
        let close = crate::editor::extract_opening_tag(&before)
            .map(|tag| format!("</{tag}>"))
            .unwrap_or_default();
        actual.insert_char('>');
        assert_eq!(
            actual.rope.to_string(),
            before + &close + " suffix",
            "tag={tag:?}"
        );
        actual.undo();
        assert_eq!(actual.rope.to_string(), prefix + " suffix");
    }
}

fn medians(mut before: Vec<std::time::Duration>, mut after: Vec<std::time::Duration>, label: &str) {
    before.sort();
    after.sort();
    eprintln!(
        "{label}: reference={:?} optimized={:?}",
        before[before.len() / 2],
        after[after.len() / 2]
    );
}

#[test]
#[ignore = "manual release cursor/selection/navigation benchmark"]
fn benchmark_cursor_navigation_hot_paths() {
    let source = "([{(".repeat(750) + "needle" + &"界x ".repeat(4_000);
    let editor = Editor::test_with_text(&source);
    let start = 3_000;
    let end = start + 6;
    assert_eq!(
        editor.enclosing_bracket_range(start, end),
        reference_brackets(&editor, start, end)
    );
    let mut before = Vec::new();
    let mut after = Vec::new();
    for sample in 0..7 {
        for optimized in [sample % 2 == 0, sample % 2 != 0] {
            let started = Instant::now();
            let range = if optimized {
                editor.enclosing_bracket_range(black_box(start), end)
            } else {
                reference_brackets(&editor, black_box(start), end)
            };
            black_box(range);
            if optimized {
                after.push(started.elapsed());
            } else {
                before.push(started.elapsed());
            }
        }
    }
    medians(
        before,
        after,
        "3k unmatched openers / 12k Unicode suffix expansion",
    );

    let source = "    word\n".repeat(60_000);
    let mut before = Vec::new();
    let mut after = Vec::new();
    for sample in 0..7 {
        for optimized in [sample % 2 == 0, sample % 2 != 0] {
            let mut editor = fixture(&source, 0, 0, false);
            let started = Instant::now();
            if optimized {
                editor.move_paragraph_down();
            } else {
                reference_paragraph(&mut editor, 1);
            }
            black_box(editor.cursor_line);
            if optimized {
                after.push(started.elapsed());
            } else {
                before.push(started.elapsed());
            }
        }
    }
    medians(before, after, "60k-line paragraph traversal");

    for home in [true, false] {
        let source = if home {
            " ".repeat(32_000) + "value"
        } else {
            "x".repeat(32_000)
        };
        let positions: Vec<_> = (0..1_024).map(|index| (0, 1 + index * 31)).collect();
        let mut verifier = cursor_fixture(&source, &positions, false);
        let mut reference = cursor_fixture(&source, &positions, false);
        if home {
            verifier.multi_cursor_select_move_to_line_start();
            reference_home(&mut reference, true);
        } else {
            verifier.multi_cursor_select_move_word_right();
            reference_words(&mut reference, false, true);
        }
        assert_navigation_equal(&verifier, &reference);
        let mut before = Vec::new();
        let mut after = Vec::new();
        for sample in 0..7 {
            for optimized in [sample % 2 == 0, sample % 2 != 0] {
                let mut editor = cursor_fixture(&source, &positions, false);
                let started = Instant::now();
                match (home, optimized) {
                    (true, true) => editor.multi_cursor_select_move_to_line_start(),
                    (true, false) => reference_home(&mut editor, true),
                    (false, true) => editor.multi_cursor_select_move_word_right(),
                    (false, false) => reference_words(&mut editor, false, true),
                }
                black_box(&editor.extra_cursors);
                if optimized {
                    after.push(started.elapsed());
                } else {
                    before.push(started.elapsed());
                }
            }
        }
        medians(
            before,
            after,
            if home {
                "1024 same-line Shift+Home carets / 32k indent"
            } else {
                "1024 same-token Ctrl+Shift+Right carets / 32k word"
            },
        );
    }

    let ordinary_source = ("    fooBar value ".repeat(4) + "\n").repeat(1_024);
    for order in ["ordered", "rotated", "shuffled"] {
        let mut positions: Vec<_> = (0..1_024).map(|line| (line, 6)).collect();
        if order == "rotated" {
            positions.rotate_left(341);
        }
        if order == "shuffled" {
            let mut seed = 0x8937abdu64;
            for index in (1..positions.len()).rev() {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                positions.swap(index, (seed >> 32) as usize % (index + 1));
            }
        }
        for home in [true, false] {
            let mut verifier = cursor_fixture(&ordinary_source, &positions, false);
            let mut reference = cursor_fixture(&ordinary_source, &positions, false);
            if home {
                verifier.multi_cursor_select_move_to_line_start();
                reference_home(&mut reference, true);
            } else {
                verifier.multi_cursor_select_move_word_right();
                reference_words(&mut reference, false, true);
            }
            assert_navigation_equal(&verifier, &reference);
            let mut before = Vec::new();
            let mut after = Vec::new();
            for sample in 0..7 {
                for optimized in [sample % 2 == 0, sample % 2 != 0] {
                    let mut editor = cursor_fixture(&ordinary_source, &positions, false);
                    let started = Instant::now();
                    match (home, optimized) {
                        (true, true) => editor.multi_cursor_select_move_to_line_start(),
                        (true, false) => reference_home(&mut editor, true),
                        (false, true) => editor.multi_cursor_select_move_word_right(),
                        (false, false) => reference_words(&mut editor, false, true),
                    }
                    black_box(&editor.extra_cursors);
                    if optimized {
                        after.push(started.elapsed());
                    } else {
                        before.push(started.elapsed());
                    }
                }
            }
            let action = if home {
                "Shift+Home"
            } else {
                "Ctrl+Shift+Right"
            };
            medians(
                before,
                after,
                &format!("1024 {order} distinct-line {action} carets"),
            );
        }
    }

    for below in [false, true] {
        let source = "    word ".repeat(220_000) + "{";
        let mut before = Vec::new();
        let mut after = Vec::new();
        for sample in 0..7 {
            for optimized in [sample % 2 == 0, sample % 2 != 0] {
                let mut editor = fixture(&source, 0, 0, false);
                let started = Instant::now();
                if optimized {
                    if below {
                        editor.insert_line_below();
                    } else {
                        editor.insert_line_above();
                    }
                } else {
                    reference_insert_line(&mut editor, below);
                }
                black_box(editor.cursor_col);
                if optimized {
                    after.push(started.elapsed());
                } else {
                    before.push(started.elapsed());
                }
            }
        }
        medians(
            before,
            after,
            if below {
                "2MiB line insert-below"
            } else {
                "2MiB line insert-above"
            },
        );
    }
}
