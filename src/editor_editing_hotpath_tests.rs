use super::Editor;
use crate::editor_multicursor::MultiCursor;
use std::{hint::black_box, time::Instant};

// Frozen original operations keep the less obvious quote precedence, escaped
// delimiter, overlapping occurrence, and first-anchor-wins behavior explicit.
fn reference_quotes(editor: &mut Editor) {
    let chars: Vec<_> = editor.rope.line(editor.cursor_line).chars().collect();
    let col = editor.cursor_col.min(chars.len());
    for quote in ['"', '\'', '`'] {
        let unescaped =
            |index: usize| (0..index).rev().take_while(|&j| chars[j] == '\\').count() % 2 == 0;
        let opening = (0..col).rev().find(|&i| chars[i] == quote && unescaped(i));
        if let Some(opening) = opening {
            let start = col + usize::from(chars.get(col) == Some(&quote));
            if let Some(closing) = (start..chars.len()).find(|&i| chars[i] == quote && unescaped(i))
            {
                editor.selection_anchor = Some((editor.cursor_line, opening + 1));
                editor.cursor_col = closing;
                editor.ensure_visible();
                return;
            }
        }
        if chars.get(col) == Some(&quote) && unescaped(col) {
            if let Some(closing) =
                (col + 1..chars.len()).find(|&i| chars[i] == quote && unescaped(i))
            {
                editor.selection_anchor = Some((editor.cursor_line, col + 1));
                editor.cursor_col = closing;
                editor.ensure_visible();
                return;
            }
        }
    }
}

fn reference_inside_brackets(editor: &mut Editor) {
    let openers = ['(', '[', '{', '<'];
    let closers = [')', ']', '}', '>'];
    let cursor = editor.rope.line_to_char(editor.cursor_line) + editor.cursor_col;
    let mut depths = [0i32; 4];
    let mut opening = None;
    for index in (0..cursor).rev() {
        let Some(ch) = editor.rope.get_char(index) else {
            continue;
        };
        for kind in 0..4 {
            if ch == closers[kind] {
                depths[kind] += 1;
            }
            if ch == openers[kind] {
                if depths[kind] == 0 {
                    opening = Some((index, kind));
                    break;
                }
                depths[kind] -= 1;
            }
        }
        if opening.is_some() {
            break;
        }
    }
    let Some((opening, kind)) = opening else {
        return;
    };
    let mut depth = 0;
    for index in opening + 1..editor.rope.len_chars() {
        let ch = editor.rope.char(index);
        if ch == openers[kind] {
            depth += 1;
        }
        if ch == closers[kind] {
            if depth == 0 {
                let line = editor.rope.char_to_line(opening + 1);
                editor.selection_anchor =
                    Some((line, opening + 1 - editor.rope.line_to_char(line)));
                editor.cursor_line = editor.rope.char_to_line(index);
                editor.cursor_col = index - editor.rope.line_to_char(editor.cursor_line);
                editor.ensure_visible();
                return;
            }
            depth -= 1;
        }
    }
}

fn assert_navigation(actual: &Editor, expected: &Editor) {
    assert_eq!(
        (
            actual.cursor_line,
            actual.cursor_col,
            actual.selection_anchor,
            actual.scroll_offset,
            actual.horizontal_scroll,
            &actual.extra_cursors,
            &actual.extra_cursor_anchors
        ),
        (
            expected.cursor_line,
            expected.cursor_col,
            expected.selection_anchor,
            expected.scroll_offset,
            expected.horizontal_scroll,
            &expected.extra_cursors,
            &expected.extra_cursor_anchors
        ),
    );
}

fn fixture(source: &str, index: usize) -> Editor {
    let mut editor = Editor::test_with_text(source);
    editor.viewport_width = 80;
    editor.viewport_height = 12;
    editor.cursor_line = editor.rope.char_to_line(index);
    editor.cursor_col = index - editor.rope.line_to_char(editor.cursor_line);
    editor.selection_anchor = Some((0, 0));
    editor
}

#[test]
fn local_quote_selection_matches_materialized_reference_at_every_cursor() {
    let mut cases = vec![
        "".to_owned(),
        "no quotes\r\nlast".into(),
        "\"one\" 'two' `three`".into(),
        "\"'` nested `x`'\" tail".into(),
        "'a\\'b' \"c\\\\\"d\" `e\\`f`".into(),
        "\"unclosed 'yes' `ok`".into(),
        "α界\\\\\"a\\\"b\"\r\n\"next\"".into(),
        format!("{}\"near\"{}", "界".repeat(2048), "é".repeat(2048)),
    ];
    // Escape parity at the caret, at both delimiters, and across rope chunks.
    for slashes in 0..8 {
        cases.push(format!(
            "'a'{}\"b{}\"c\" `d`",
            "\\".repeat(slashes),
            "\\".repeat(7 - slashes)
        ));
    }
    let alphabet = ['"', '\'', '`', '\\', 'α', '界', ' ', 'x'];
    let mut random = 0xD61C_0A27_u64;
    for _ in 0..128 {
        cases.push(
            (0..32)
                .map(|_| {
                    random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                    alphabet[(random >> 32) as usize % alphabet.len()]
                })
                .collect(),
        );
    }
    for source in cases {
        let rope = ropey::Rope::from_str(&source);
        let step = if rope.len_chars() > 1024 { 97 } else { 1 };
        for index in (0..=rope.len_chars())
            .step_by(step)
            .chain((2040..2060).filter(|&index| index <= rope.len_chars()))
            .chain([rope.len_chars()])
        {
            let mut actual = fixture(&source, index);
            let mut expected = fixture(&source, index);
            actual.select_inside_quotes();
            reference_quotes(&mut expected);
            assert_navigation(&actual, &expected);
        }
    }
}

#[test]
fn streaming_inside_brackets_matches_indexed_reference_for_malformed_and_chunked_text() {
    for source in [
        "(a[b{c<d>e}f]g)\r\n",
        "([)] {x <y> [z]}",
        "()) ( unmatched {",
        "α界(é\r\n[文])",
        &format!(
            "{}({}界){}",
            "x".repeat(4090),
            "é".repeat(1100),
            "z".repeat(1030)
        ),
    ] {
        let rope = ropey::Rope::from_str(source);
        let step = if rope.len_chars() > 1000 { 97 } else { 1 };
        for index in (0..=rope.len_chars())
            .step_by(step)
            .chain([rope.len_chars()])
        {
            let mut actual = fixture(source, index);
            let mut expected = fixture(source, index);
            actual.select_inside_brackets();
            reference_inside_brackets(&mut expected);
            assert_navigation(&actual, &expected);
        }
    }
    let mut actual = fixture("(value)", 0);
    let mut expected = fixture("(value)", 0);
    actual.cursor_col = 99;
    expected.cursor_col = 99;
    actual.select_inside_brackets();
    reference_inside_brackets(&mut expected);
    assert_navigation(&actual, &expected);
}

fn reference_definition(editor: &Editor, word: &str) -> Option<(usize, usize)> {
    let patterns: Vec<_> = [
        "fn ",
        "pub(crate) fn ",
        "struct ",
        "pub struct ",
        "enum ",
        "pub enum ",
        "trait ",
        "pub trait ",
        "impl ",
        "type ",
        "pub type ",
        "const ",
        "pub const ",
        "static ",
        "pub static ",
        "let ",
        "let mut ",
        "class ",
        "def ",
        "function ",
        "var ",
        "const ",
        "interface ",
    ]
    .iter()
    .map(|prefix| format!("{prefix}{word}"))
    .collect();
    for (index, line) in editor.rope.lines().enumerate() {
        let text = line.to_string();
        let trimmed = text.trim_start();
        for pattern in &patterns {
            if trimmed.starts_with(pattern.as_str()) {
                return Some((
                    index,
                    text.chars().count() - trimmed.chars().count() + pattern.chars().count()
                        - word.chars().count(),
                ));
            }
        }
    }
    None
}

#[test]
fn definition_prefix_scanning_matches_full_line_reference_for_unicode_and_split_rope_lines() {
    for prefix in [
        "fn ",
        "pub(crate) fn ",
        "let ",
        "let mut ",
        "class ",
        "interface ",
    ] {
        for word in ["value", "界é", "", "mut", "prefix", "a b"] {
            for indent in ["", "  \t", "\u{2003}", &" ".repeat(4093)] {
                let source = format!(
                    "{}\n{indent}{prefix}{word}suffix{}\r\nfn later()",
                    "unrelated".repeat(1024),
                    "界".repeat(2048)
                );
                let editor = fixture(&source, 0);
                assert_eq!(
                    editor.find_definition(word),
                    reference_definition(&editor, word),
                    "prefix={prefix:?}, word={word:?}, indent length={}",
                    indent.len()
                );
            }
        }
    }
}

#[test]
fn extensionless_language_rejects_long_non_shebang_lines_and_preserves_interpreter_priority() {
    for (source, language) in [
        ("", "Plain Text"),
        ("#", "Plain Text"),
        ("!# python", "Plain Text"),
        (" #! python", "Plain Text"),
        ("#! /usr/bin/env python3\r\n", "Python"),
        ("#! node python", "Python"),
        ("#! deno", "JavaScript"),
        ("#! bun", "JavaScript"),
        ("#!/bin/zsh", "Shell Script"),
        ("#! ruby perl", "Ruby"),
        ("#! perl", "Perl"),
        ("#! php", "PHP"),
        ("#! lua", "Lua"),
        ("#! elixir", "Elixir"),
        ("#! pwsh", "PowerShell"),
        ("#! powershell", "PowerShell"),
        ("#! unknown\npython", "Plain Text"),
        (&format!("界{}#!python", "x".repeat(65536)), "Plain Text"),
        (&format!("#!{}python", "x".repeat(8192)), "Python"),
    ] {
        for path in [None, Some(std::path::PathBuf::from("script"))] {
            let mut editor = fixture(source, 0);
            editor.file_path = path;
            assert_eq!(
                editor.language_name(),
                language,
                "source length={}",
                source.len()
            );
        }
    }
}

fn reference_states(editor: &mut Editor, mut states: Vec<MultiCursor>) {
    let mut seen = rustc_hash::FxHashSet::default();
    let mut line_lengths = rustc_hash::FxHashMap::default();
    states.retain_mut(|state| {
        state.line = state.line.min(editor.line_count() - 1);
        state.col = state.col.min(
            *line_lengths
                .entry(state.line)
                .or_insert_with(|| editor.line_len(state.line)),
        );
        state.anchor = state.anchor.map(|(line, col)| {
            let line = line.min(editor.line_count() - 1);
            (
                line,
                col.min(
                    *line_lengths
                        .entry(line)
                        .or_insert_with(|| editor.line_len(line)),
                ),
            )
        });
        seen.insert((state.line, state.col))
    });
    let first = states[0];
    editor.cursor_line = first.line;
    editor.cursor_col = first.col;
    editor.selection_anchor = first.anchor;
    editor.extra_cursors.clear();
    editor.extra_cursor_anchors.clear();
    editor.extra_cursors.reserve(states.len() - 1);
    editor.extra_cursor_anchors.reserve(states.len() - 1);
    for state in states.into_iter().skip(1) {
        editor.extra_cursors.push((state.line, state.col));
        editor.extra_cursor_anchors.push(state.anchor);
    }
    editor.ensure_visible();
}

#[test]
fn ordered_selection_normalization_retains_first_anchor_and_matches_unordered_fallback() {
    let source = "α界\r\nx\n\nlonger value\nlast";
    for reverse in [false, true] {
        for stale in [false, true] {
            let mut states: Vec<_> = (0..7)
                .flat_map(|line| {
                    (0..9).map(move |col| MultiCursor {
                        line: if stale { line } else { line.min(4) },
                        col,
                        anchor: Some((line.saturating_sub(1), col * 4)),
                    })
                })
                .collect();
            if reverse {
                states.reverse();
            }
            let mut actual = fixture(source, 0);
            let mut expected = fixture(source, 0);
            actual.set_multi_cursor_states(states.clone());
            reference_states(&mut expected, states);
            assert_navigation(&actual, &expected);
        }
    }
    // Strictly ordered input that collapses to equal clamped destinations.
    for reverse in [false, true] {
        let mut states: Vec<_> = (0..=8)
            .map(|col| MultiCursor {
                line: 0,
                col,
                anchor: Some((0, col)),
            })
            .collect();
        if reverse {
            states.reverse();
        }
        let mut actual = fixture(source, 0);
        let mut expected = fixture(source, 0);
        actual.set_multi_cursor_states(states.clone());
        reference_states(&mut expected, states);
        assert_navigation(&actual, &expected);
    }
    let states: Vec<_> = (0..5)
        .map(|col| MultiCursor {
            line: 4,
            col,
            anchor: Some((col, 99)),
        })
        .collect();
    let mut actual = fixture(source, 0);
    let mut expected = fixture(source, 0);
    actual.set_multi_cursor_states(states.clone());
    reference_states(&mut expected, states);
    assert_navigation(&actual, &expected);
}

fn reference_occurrences(editor: &mut Editor) {
    editor.desired_col = None;
    if editor.clamped_selection_indices().is_none() {
        editor.select_word_at_cursor();
    }
    let Some(text) = editor.selected_text().filter(|text| !text.is_empty()) else {
        return;
    };
    let needle: Vec<_> = text.chars().collect();
    let needle_text: String = needle.iter().collect();
    let search = super::RopeSearch::new(&needle_text);
    let positions: Vec<_> = search
        .matches(&editor.rope, 0, editor.rope.len_chars())
        .collect();
    if positions.is_empty() {
        return;
    }
    let position = |index| {
        let line = editor.rope.char_to_line(index);
        (line, index - editor.rope.line_to_char(line))
    };
    let first_start = position(positions[0]);
    let first_end = position(positions[0] + needle.len());
    editor.cursor_line = first_end.0;
    editor.cursor_col = first_end.1;
    editor.selection_anchor = Some(first_start);
    editor.extra_cursors.clear();
    editor.extra_cursor_anchors.clear();
    for start in positions.into_iter().skip(1) {
        editor.extra_cursor_anchors.push(Some(position(start)));
        editor.extra_cursors.push(position(start + needle.len()));
    }
    editor.ensure_visible();
}

#[test]
fn streamed_occurrence_projection_matches_original_for_overlaps_eof_and_multiline_matches() {
    for (source, selected) in [
        ("aaaaa", 2),
        ("α界 α界\r\nα界", 2),
        ("a\nb\na\nb\na\nb", 3),
        ("a\r\na\r\na\r\n", 3),
        ("\n\n\n", 1),
        ("last", 4),
        (&"界a ".repeat(8192), 2),
    ] {
        let mut actual = fixture(source, selected);
        let mut expected = fixture(source, selected);
        for editor in [&mut actual, &mut expected] {
            editor.extra_cursors = vec![(0, 0), (0, 1)];
            editor.extra_cursor_anchors = vec![None, Some((0, 0))];
        }
        actual.select_all_occurrences();
        reference_occurrences(&mut expected);
        assert_navigation(&actual, &expected);
    }
}

#[test]
fn single_mutation_auto_pairs_keep_neighbor_rules_unicode_crlf_cursor_and_undo() {
    for source in [
        "",
        "word",
        "α界é",
        "  \r\nnext",
        "('x')",
        "\"'`",
        "word suffix",
    ] {
        let rope = ropey::Rope::from_str(source);
        let first_line_len = Editor::test_with_text(source).line_len(0);
        for col in 0..=first_line_len {
            for (opening, closing) in [
                ('(', ')'),
                ('[', ']'),
                ('{', '}'),
                ('"', '"'),
                ('\'', '\''),
                ('`', '`'),
            ] {
                for overwrite in [false, true] {
                    let mut actual = fixture(source, col);
                    actual.selection_anchor = None;
                    actual.overwrite_mode = overwrite;
                    let next = rope.get_char(col);
                    let skip = matches!(opening, '"' | '\'' | '`') && next == Some(opening);
                    let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';
                    let pair = !skip
                        && !next.is_some_and(is_word)
                        && !(matches!(opening, '"' | '\'' | '`')
                            && col > 0
                            && rope.char(col - 1).is_alphanumeric())
                        && !(matches!(opening, '"' | '\'' | '`')
                            && col > 0
                            && rope.char(col - 1) == '_');
                    let mut expected = source.to_owned();
                    if !skip {
                        let byte = rope.char_to_byte(col);
                        let insertion = if pair {
                            format!("{opening}{closing}")
                        } else {
                            opening.to_string()
                        };
                        expected.insert_str(byte, &insertion);
                    }
                    actual.insert_char(opening);
                    assert_eq!(
                        actual.rope.to_string(),
                        expected,
                        "source={source:?}, col={col}, opening={opening}"
                    );
                    assert_eq!(actual.cursor_col, col + 1);
                    if skip {
                        assert!(!actual.can_undo(), "moving over a delimiter is not an edit");
                        assert_eq!(actual.content_version, 0);
                    }
                    actual.undo();
                    assert_eq!(actual.rope.to_string(), source);
                    assert_eq!((actual.cursor_line, actual.cursor_col), (0, col + usize::from(skip)));
                    actual.redo();
                    assert_eq!(actual.rope.to_string(), expected);
                }
            }
        }
    }
}

fn median_pair(mut run: impl FnMut(bool)) -> (std::time::Duration, std::time::Duration) {
    let mut times = [Vec::new(), Vec::new()];
    for sample in 0..7 {
        for fast in [sample % 2 == 0, sample % 2 != 0] {
            let start = Instant::now();
            run(fast);
            times[usize::from(fast)].push(start.elapsed());
        }
    }
    for time in &mut times {
        time.sort();
    }
    (times[0][3], times[1][3])
}

#[test]
#[ignore = "manual release comparison of quote, bracket, definition, selection and occurrence hot paths"]
fn benchmark_additional_editing_hotpaths() {
    let untitled = fixture(&"x".repeat(2 * 1024 * 1024), 0);
    let (before, after) = median_pair(|fast| {
        let result = if fast {
            untitled.language_name()
        } else {
            let first = untitled.rope.line(0).to_string();
            assert!(!black_box(&first).starts_with("#!"));
            "Plain Text"
        };
        assert_eq!(black_box(result), "Plain Text");
    });
    eprintln!("2MiB untitled language probe: whole first line {before:?}, prefix {after:?}");

    let quote_text = format!(
        "{}\"value\"{}",
        "x".repeat(1024 * 1024),
        "y".repeat(1024 * 1024)
    );
    let mut quote = fixture(&quote_text, 1024 * 1024 + 3);
    let (before, after) = median_pair(|fast| {
        quote.cursor_col = 1024 * 1024 + 3;
        quote.selection_anchor = None;
        if fast {
            quote.select_inside_quotes();
        } else {
            reference_quotes(&mut quote);
        }
        black_box((&quote.selection_anchor, quote.cursor_col));
        assert_eq!(quote.selection_anchor, Some((0, 1024 * 1024 + 1)));
        assert_eq!(quote.cursor_col, 1024 * 1024 + 6);
    });
    eprintln!("2MiB local quote selection: original {before:?}, streaming {after:?}");

    let bracket_text = format!("({})", "α界x".repeat(60_000));
    let mut bracket = fixture(&bracket_text, 90_000);
    let (before, after) = median_pair(|fast| {
        bracket.cursor_col = 90_000;
        bracket.selection_anchor = None;
        if fast {
            bracket.select_inside_brackets();
        } else {
            reference_inside_brackets(&mut bracket);
        }
        black_box((&bracket.selection_anchor, bracket.cursor_col));
        assert_eq!(bracket.selection_anchor, Some((0, 1)));
        assert_eq!(bracket.cursor_col, 180_001);
    });
    eprintln!("180k-character bracket selection: indexed {before:?}, streaming {after:?}");

    let definition = fixture(
        &format!(
            "{}\n  fn target() {}",
            "unrelated".repeat(256_000),
            "body".repeat(256_000)
        ),
        0,
    );
    let (before, after) = median_pair(|fast| {
        let result = if fast {
            definition.find_definition("target")
        } else {
            reference_definition(&definition, "target")
        };
        assert_eq!(black_box(result), Some((1, 5)));
    });
    eprintln!("3MiB generated-source definition: full lines {before:?}, prefixes {after:?}");

    let source = "value ".repeat(30_000);
    let mut occurrences = fixture(&source, 5);
    let (before, after) = median_pair(|fast| {
        occurrences.cursor_col = 5;
        occurrences.selection_anchor = Some((0, 0));
        if fast {
            occurrences.select_all_occurrences();
        } else {
            reference_occurrences(&mut occurrences);
        }
        assert_eq!(black_box(occurrences.extra_cursors.len()), 29_999);
        assert_eq!(occurrences.extra_cursors.last(), Some(&(0, 179_999)));
    });
    eprintln!("30k same-line occurrences: original {before:?}, streamed bounds {after:?}");

    let source = "value\n".repeat(4096);
    let states: Vec<_> = (0..4096)
        .map(|line| MultiCursor {
            line,
            col: 4,
            anchor: Some((line, 1)),
        })
        .collect();
    let mut editor = fixture(&source, 0);
    let (before, after) = median_pair(|fast| {
        if fast {
            editor.set_multi_cursor_states(states.clone());
        } else {
            reference_states(&mut editor, states.clone());
        }
        assert_eq!(black_box(editor.extra_cursors.len()), 4095);
    });
    eprintln!("4096 ordered selections: hash normalization {before:?}, ordered {after:?}");

    let source = format!("{}\n{}", "x".repeat(4096), "value\n".repeat(4096));
    let states: Vec<_> = (0..4096)
        .map(|col| MultiCursor {
            line: 0,
            col,
            anchor: Some((col + 1, 1)),
        })
        .collect();
    let mut editor = fixture(&source, 0);
    let (before, after) = median_pair(|fast| {
        if fast {
            editor.set_multi_cursor_states(states.clone());
        } else {
            reference_states(&mut editor, states.clone());
        }
        assert_eq!(black_box(editor.extra_cursors.len()), 4095);
    });
    eprintln!(
        "4096 same-line selections with distinct anchor lines: hash normalization {before:?}, ordered LRU {after:?}"
    );
}
