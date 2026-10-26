use crate::editor::Editor;
use ropey::Rope;
use std::{hint::black_box, time::Instant};

// Full-line references keep newline, Unicode whitespace, snippet placeholder
// and undo behavior explicit while production avoids copying source text.
fn reference_snippet(editor: &mut Editor) -> bool {
    let line_text = editor.rope.line(editor.cursor_line).to_string();
    let before: String = line_text.chars().take(editor.cursor_col).collect();
    let word_start_byte = before.rfind(|c: char| !c.is_alphanumeric() && c != '_')
        .map_or(0, |i| i + before[i..].chars().next().map_or(1, char::len_utf8));
    let trigger = &before[word_start_byte..];
    if trigger.is_empty() { return false; }
    if let Some(body) = crate::editor_snippets::get_snippet(trigger, editor.language_name()) {
        editor.save_undo();
        let line_start = editor.rope.line_to_char(editor.cursor_line);
        let trigger_start = line_start + before[..word_start_byte].chars().count();
        editor.rope.remove(trigger_start..line_start + editor.cursor_col);
        let indent = crate::editor::leading_whitespace(&line_text);
        let expanded = body.replace('\n', &format!("\n{indent}"));
        editor.insert_snippet_text(trigger_start, &expanded);
        return true;
    }
    false
}

fn reference_fold(editor: &Editor, start_line: usize) -> Option<usize> {
    let total = editor.line_count();
    if start_line >= total.saturating_sub(1) { return None; }
    if let Ok(idx) = editor.lsp_folding_ranges.binary_search_by_key(&start_line, |&(s, _)| s) {
        let (_, end) = editor.lsp_folding_ranges[idx];
        if end < total { return Some(end + 1); }
    }
    let start_text = editor.rope.line(start_line).to_string();
    let start_trimmed = start_text.trim();
    if Editor::is_region_start(start_trimmed) {
        let mut depth = 1;
        for i in start_line + 1..total {
            let text = editor.rope.line(i).to_string();
            let trimmed = text.trim();
            if Editor::is_region_start(trimmed) { depth += 1; }
            else if Editor::is_region_end(trimmed) {
                depth -= 1;
                if depth == 0 { return Some(i + 1); }
            }
        }
        return None;
    }
    let start_indent = start_text.chars().take_while(|c| c.is_whitespace()).count();
    if start_trimmed.is_empty() { return None; }
    let mut last_deeper = start_line;
    for i in start_line + 1..total {
        let line = editor.rope.line(i);
        let indent = line.chars().take_while(|c| c.is_whitespace()).count();
        if indent == line.len_chars() { continue; }
        if indent > start_indent { last_deeper = i; } else { break; }
    }
    (last_deeper > start_line).then_some(last_deeper + 1)
}

fn reference_trim(editor: &mut Editor) -> bool {
    editor.desired_col = None;
    let total = editor.line_count();
    let mut any_trimmed = false;
    for i in (0..total).rev() {
        let text = editor.rope.line(i).to_string();
        let without_eol = text.trim_end_matches(['\n', '\r']);
        let trimmed = without_eol.trim_end();
        if trimmed.len() == without_eol.len() { continue; }
        // A trim must preserve every separator and leave undo/redo untouched
        // when there is no whitespace to remove.
        if !any_trimmed { editor.save_undo(); }
        let new_line = format!("{trimmed}{}", &text[without_eol.len()..]);
        let from = editor.rope.line_to_char(i);
        let to = from + editor.rope.line(i).len_chars();
        editor.rope.remove(from..to);
        editor.rope.insert(from, &new_line);
        editor.mark_syntax_dirty_range(i, i);
        editor.modified = true;
        any_trimmed = true;
    }
    if any_trimmed {
        editor.set_multi_cursor_states(editor.collect_multi_cursors());
    }
    any_trimmed
}

fn assert_editor_equal(actual: &Editor, expected: &Editor) {
    assert_eq!(actual.rope, expected.rope);
    assert_eq!((actual.cursor_line, actual.cursor_col), (expected.cursor_line, expected.cursor_col));
    assert_eq!(actual.selection_anchor, expected.selection_anchor);
    assert_eq!(actual.desired_col, expected.desired_col);
    assert_eq!(actual.extra_cursors, expected.extra_cursors);
    assert_eq!(actual.extra_cursor_anchors, expected.extra_cursor_anchors);
    assert_eq!(actual.bookmarks, expected.bookmarks);
    assert_eq!(actual.folded_ranges, expected.folded_ranges);
    assert_eq!(actual.content_version, expected.content_version);
    assert_eq!(actual.modified, expected.modified);
    assert_eq!(actual.undo_stack.len(), expected.undo_stack.len());
    assert_eq!(actual.redo_stack.len(), expected.redo_stack.len());
    assert_eq!((actual.scroll_offset, actual.horizontal_scroll), (expected.scroll_offset, expected.horizontal_scroll));
    assert_eq!(actual.snippet_active, expected.snippet_active);
    assert_eq!(actual.snippet_active_stop, expected.snippet_active_stop);
    assert_eq!(actual.snippet_tab_stops.len(), expected.snippet_tab_stops.len());
    for (a, e) in actual.snippet_tab_stops.iter().zip(&expected.snippet_tab_stops) {
        assert_eq!((a.index, a.line, a.col, a.placeholder_len), (e.index, e.line, e.col, e.placeholder_len));
    }
}

#[test]
fn snippet_local_lookup_matches_full_line_reference() {
    for (extension, trigger) in [("rs", "fn"), ("rs", "iflet"), ("js", "for"), ("py", "def"), ("html", "textarea"), ("css", "bgc"), ("txt", "fn")] {
        for prefix in [String::new(), " \t\u{2003}".into(), "界;".into(), "unknown_".into(), format!("  {};", "界".repeat(20_000))] {
            for suffix in ["", "\r\nnext", " suffix"] {
                let text = format!("{prefix}{trigger}{suffix}");
                let col = prefix.chars().count() + trigger.chars().count();
                let mut actual = Editor::test_with_text(&text);
                let mut expected = Editor::test_with_text(&text);
                for ed in [&mut actual, &mut expected] {
                    ed.file_path = Some(format!("example.{extension}").into());
                    ed.cursor_col = col;
                    ed.desired_col = Some(col + 2);
                }
                assert_eq!(actual.try_expand_snippet(), reference_snippet(&mut expected), "{extension} {trigger}");
                assert_editor_equal(&actual, &expected);
                actual.undo(); expected.undo();
                assert_editor_equal(&actual, &expected);
                actual.redo(); expected.redo();
                assert_editor_equal(&actual, &expected);
            }
        }
    }
    for text in ["", " ", "界", "hello_", "hello;", "\n", "\r\n"] {
        let mut editor = Editor::test_with_text(text);
        editor.cursor_col = editor.line_len(0) + 10;
        assert!(!editor.try_expand_snippet());
    }
    for token in ["a", "_", "界"] {
        let text = format!("{}fn", token.repeat(100_000));
        let mut actual = Editor::test_with_text(&text);
        let mut expected = Editor::test_with_text(&text);
        for ed in [&mut actual, &mut expected] {
            ed.file_path = Some("example.rs".into());
            ed.cursor_col = ed.line_len(0);
        }
        assert_eq!(actual.try_expand_snippet(), reference_snippet(&mut expected));
        assert_editor_equal(&actual, &expected);
    }
}

#[test]
fn discarded_selection_keeps_undo_bookmarks_folds_and_virtual_space_behavior() {
    for text in ["", "界ab\r\nsecond\nthird\n", "first\n\nlast", "  tiny selection"] {
        for reverse in [false, true] {
            for operation in [Editor::backspace_replacing_selection, Editor::delete_replacing_selection] {
                let mut actual = Editor::test_with_text(text);
                let mut expected = Editor::test_with_text(text);
                for ed in [&mut actual, &mut expected] {
                    let last = ed.line_count() - 1;
                    ed.selection_anchor = Some(if reverse { (last, 100) } else { (0, 1) });
                    (ed.cursor_line, ed.cursor_col) = if reverse { (0, 1) } else { (last, 100) };
                    ed.desired_col = Some(9);
                    ed.bookmarks.extend([0, 1, 2, 3]);
                    ed.folded_ranges.insert(0, 3);
                    ed.extra_cursors = vec![(0, 0)];
                    ed.extra_cursor_anchors = vec![Some((0, 1))];
                }
                operation(&mut actual);
                expected.delete_selection();
                assert_editor_equal(&actual, &expected);
                actual.undo(); expected.undo();
                assert_editor_equal(&actual, &expected);
                actual.redo(); expected.redo();
                assert_editor_equal(&actual, &expected);
            }
        }
    }
    for (text, anchor, cursor, edited) in [
        ("  fnselected", (0, 4), (0, 12), "  fn    "),
        ("before\r\nselected\nlast", (0, 2), (2, 3), "be  t"),
        ("abc", (0, 100), (0, 100), "abc "),
    ] {
        let mut actual = Editor::test_with_text(text);
        actual.file_path = Some("example.rs".into());
        actual.selection_anchor = Some(anchor);
        (actual.cursor_line, actual.cursor_col) = cursor;
        actual.insert_tab_replacing_selection();
        assert_eq!(actual.rope.to_string(), edited);
        assert_eq!(actual.selection_anchor, None);
        actual.undo();
        assert_eq!(actual.rope.to_string(), text);
        assert_eq!(actual.selection_anchor, Some(anchor));
        assert_eq!((actual.cursor_line, actual.cursor_col), cursor);
        assert!(!actual.can_undo());
        actual.redo();
        assert_eq!(actual.rope.to_string(), edited);
    }
}

#[test]
fn streaming_fold_lookup_matches_region_and_indentation_reference() {
    let mut text = String::new();
    let prefixes = ["#", "# ", "#pragma ", "//", "// #", "-- ", "<!-- ", "<!-- #", "// # ", "# pragma ", ""];
    for prefix in prefixes {
        for marker in ["region", "endregion", "regional", "endregions", "regio", "something"] {
            let line = format!(" \t\u{2003}{prefix}{marker} {}\r\n", "界".repeat(700));
            let rope = Rope::from_str(&line);
            let reference = if Editor::is_region_start(line.trim()) { Some(true) }
                else if Editor::is_region_end(line.trim()) { Some(false) } else { None };
            assert_eq!(super::rope_line_region_marker(rope.line(0)), reference, "{prefix}{marker}");
            text.push_str(&line);
        }
    }
    text.push_str("// region outer\nfirst\n// region inner\nsecond\n// endregion\n// endregion\nheader\n  child\n\n\tchild\nnext\n");
    let mut editor = Editor::test_with_text(&text);
    for lsp in [vec![], vec![(1, 5), (10, 100), (15, usize::MAX)]] {
        editor.lsp_folding_ranges = lsp;
        for line in 0..=editor.line_count() {
            assert_eq!(editor.find_fold_end(line), reference_fold(&editor, line), "line {line}");
        }
    }
}

#[test]
fn suffix_only_trimming_matches_line_reference_for_unicode_and_mixed_endings() {
    for ending in ["", "\n", "\r\n", "\r"] {
        for whitespace in ["", " ", "\t", "\u{2003}", "\u{a0} \t\u{2028}"] {
            let text = format!("界{whitespace}{ending}a{whitespace}\r\n  b{whitespace}\rc{whitespace}\n{whitespace}");
            for cursor in 0..Rope::from_str(&text).len_lines() {
                let mut actual = Editor::test_with_text(&text);
                let mut expected = Editor::test_with_text(&text);
                for ed in [&mut actual, &mut expected] {
                    ed.cursor_line = cursor;
                    ed.cursor_col = 100;
                    ed.selection_anchor = Some((0, 0));
                    ed.desired_col = Some(100);
                    ed.bookmarks.extend([0, 1, 2]);
                }
                assert_eq!(actual.trim_trailing_whitespace(), reference_trim(&mut expected));
                assert_editor_equal(&actual, &expected);
                actual.undo(); expected.undo();
                assert_editor_equal(&actual, &expected);
                actual.redo(); expected.redo();
                assert_editor_equal(&actual, &expected);
            }
        }
    }
}

#[test]
#[ignore = "manual release benchmark"]
fn benchmark_local_editor_operations() {
    enum Operation { Snippet, Delete, Trim, Fold }
    for (name, operation, text) in [
        ("Tab snippet lookup after 2MiB prefix", Operation::Snippet, format!("{};unknown", "界".repeat(700_000))),
        ("Tab snippet lookup in 2MiB identifier", Operation::Snippet, "界".repeat(700_000)),
        ("Delete 4MiB selection", Operation::Delete, "界x\n".repeat(800_000)),
        ("Trim 4MiB generated source", Operation::Trim, format!("{}  \r\n", "界".repeat(10_000)).repeat(140)),
        ("Fold region with 4MiB body", Operation::Fold, format!("// region\n{}// endregion\n", format!("{}\n", "界".repeat(10_000)).repeat(140))),
    ] {
        let source = Rope::from_str(&text);
        let mut samples = [Vec::new(), Vec::new()];
        for sample in 0..7 {
            let mut results = Vec::new();
            for optimized in [sample % 2 == 0, sample % 2 != 0] {
                let mut editor = Editor::test_with_text("");
                editor.rope = source.clone();
                editor.file_path = Some("example.rs".into());
                if matches!(operation, Operation::Snippet | Operation::Delete) {
                    editor.cursor_line = editor.line_count() - 1;
                    editor.cursor_col = editor.line_len(editor.cursor_line);
                }
                if matches!(operation, Operation::Delete) { editor.selection_anchor = Some((0, 0)); }
                let started = Instant::now();
                match operation {
                    Operation::Snippet => { black_box(if optimized { editor.try_expand_snippet() } else { reference_snippet(&mut editor) }); }
                    Operation::Delete => { if optimized { editor.delete_replacing_selection(); } else { black_box(editor.delete_selection()); } }
                    Operation::Trim => { black_box(if optimized { editor.trim_trailing_whitespace() } else { reference_trim(&mut editor) }); }
                    Operation::Fold => { black_box(if optimized { editor.find_fold_end(0) } else { reference_fold(&editor, 0) }); }
                }
                samples[usize::from(optimized)].push(started.elapsed());
                results.push(editor);
            }
            assert_editor_equal(&results[0], &results[1]);
            if matches!(operation, Operation::Fold) { assert_eq!(results[0].find_fold_end(0), reference_fold(&results[1], 0)); }
        }
        for sample in &mut samples { sample.sort(); }
        eprintln!("{name}: reference={:?}, optimized={:?}", samples[0][3], samples[1][3]);
    }
}
