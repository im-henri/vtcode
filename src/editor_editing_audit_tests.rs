use super::Editor;
use ropey::Rope;

fn saved(text: &str) -> Editor {
    let mut editor = Editor::test_with_text(text);
    editor.saved_rope = Some(editor.rope.clone());
    editor
}

fn round_trip(editor: &mut Editor, before: &str, after: &str) {
    assert_eq!(editor.rope, after);
    assert!(editor.modified);
    for _ in 0..3 {
        editor.undo();
        assert_eq!(editor.rope, before);
        assert!(!editor.modified);
        assert!(
            !editor.can_undo(),
            "one operation must be one history entry"
        );
        editor.redo();
        assert_eq!(editor.rope, after);
        assert!(editor.modified);
    }
}

#[test]
fn indentation_conversion_preserves_code_tabs_and_visual_alignment() {
    for ending in ["\n", "\r\n", "\r"] {
        let source = format!(" \tlet s = \"\t\";{ending}\t  \tx\ty{ending}");
        let mut editor = saved(&source);
        editor.tab_size = 4;
        editor.convert_indentation_to_spaces();
        let expected = format!("    let s = \"\t\";{ending}        x\ty{ending}");
        round_trip(&mut editor, &source, &expected);
    }
}

#[test]
fn indentation_conversion_remaps_every_caret_and_selection_before_further_typing() {
    let source = "    αβ\r\n        γδ";
    let mut editor = saved(source);
    editor.cursor_col = 6;
    editor.selection_anchor = Some((0, 4));
    editor.extra_cursors = vec![(1, 10)];
    editor.extra_cursor_anchors = vec![Some((1, 8))];
    editor.tab_size = 4;
    editor.convert_indentation_to_tabs();
    assert_eq!(editor.rope, "\tαβ\r\n\t\tγδ");
    assert_eq!((editor.cursor_line, editor.cursor_col), (0, 3));
    assert_eq!(editor.selection_anchor, Some((0, 1)));
    assert_eq!(editor.extra_cursors, [(1, 4)]);
    assert_eq!(editor.extra_cursor_anchors, [Some((1, 2))]);
    editor.multi_cursor_insert_char('X');
    assert_eq!(editor.rope, "\tX\r\n\t\tX");
    editor.undo();
    assert_eq!(editor.rope, "\tαβ\r\n\t\tγδ");
    editor.undo();
    assert_eq!(editor.rope, source);
    assert_eq!(editor.cursor_col, 6);
    assert_eq!(editor.extra_cursors, [(1, 10)]);
    assert!(!editor.modified);
}

#[test]
fn no_op_indentation_commands_do_not_erase_redo_or_mark_saved_text_dirty() {
    for operation in [
        Editor::convert_indentation_to_spaces,
        Editor::convert_indentation_to_tabs,
        Editor::reindent_lines,
    ] {
        let mut editor = saved("plain text");
        editor.insert_char('X');
        editor.undo();
        let version = editor.content_version;
        operation(&mut editor);
        assert!(!editor.modified);
        assert_eq!(editor.content_version, version);
        assert!(!editor.can_undo());
        assert!(editor.can_redo());
        editor.redo();
        assert_eq!(editor.rope, "Xplain text");
    }
}

#[test]
fn reindent_preserves_line_endings_and_nested_closing_braces() {
    for ending in ["\n", "\r\n", "\r"] {
        let source = [
            "if (ready) {",
            "if (nested) {",
            "run();",
            "}",
            "next();",
            "}",
            "end();",
        ]
        .join(ending);
        let expected = [
            "if (ready) {",
            "    if (nested) {",
            "        run();",
            "    }",
            "    next();",
            "}",
            "end();",
        ]
        .join(ending);
        let mut editor = saved(&source);
        editor.tab_size = 4;
        editor.reindent_lines();
        round_trip(&mut editor, &source, &expected);
    }
}

#[test]
fn multicursor_paste_keeps_the_primary_caret_on_its_original_selection() {
    let mut editor = saved("alpha\nbeta");
    (editor.cursor_line, editor.cursor_col) = (1, 4);
    editor.selection_anchor = Some((1, 0));
    editor.extra_cursors = vec![(0, 5)];
    editor.extra_cursor_anchors = vec![Some((0, 0))];
    editor.paste_plain_with_text("X\nYZ");
    assert_eq!(editor.rope, "X\nYZ");
    assert_eq!((editor.cursor_line, editor.cursor_col), (1, 2));
    assert_eq!(editor.extra_cursors, [(0, 1)]);
    round_trip(&mut editor, "alpha\nbeta", "X\nYZ");
}

#[test]
fn overlapping_paste_selections_collapse_before_distributing_clipboard_lines() {
    for reversed in [false, true] {
        let mut editor = saved("abcdef");
        editor.cursor_col = if reversed { 0 } else { 4 };
        editor.selection_anchor = Some((0, if reversed { 4 } else { 0 }));
        editor.extra_cursors = vec![(0, 6)];
        editor.extra_cursor_anchors = vec![Some((0, 2))];
        editor.paste_plain_with_text("X\nY");
        assert_eq!(editor.rope, "X\nY");
        assert!(editor.extra_cursors.is_empty());
        round_trip(&mut editor, "abcdef", "X\nY");
    }
}

#[test]
fn multicursor_closers_skip_existing_delimiters_independently() {
    for closer in [')', ']', '}', '"', '\'', '`'] {
        let source = format!("{closer}\n");
        let mut editor = saved(&source);
        // A second caret with an empty anchor is equivalent to the primary
        // caret and must still advance when the merged edit skips a closer.
        editor.extra_cursors = vec![(0, 0), (1, 0)];
        editor.extra_cursor_anchors = vec![Some((0, 0)), None];
        editor.multi_cursor_insert_char(closer);
        let expected = format!(
            "{closer}\n{closer}{}",
            if matches!(closer, '"' | '\'' | '`') {
                closer.to_string()
            } else {
                String::new()
            }
        );
        assert_eq!(editor.rope, expected);
        assert_eq!(editor.cursor_col, 1);
        assert_eq!(editor.extra_cursors, [(1, 1)]);
        round_trip(&mut editor, &source, &expected);
    }
}

#[test]
fn multicursor_apostrophes_inside_words_do_not_insert_unwanted_quotes() {
    let mut editor = saved("don\nwon");
    editor.cursor_col = 3;
    editor.extra_cursors = vec![(1, 3)];
    editor.multi_cursor_insert_char('\'');
    round_trip(&mut editor, "don\nwon", "don'\nwon'");
}

#[test]
fn virtual_character_and_word_deletions_never_modify_the_following_line() {
    for operation in [
        Editor::backspace,
        Editor::delete_char,
        Editor::delete_word_left,
        Editor::delete_word_right,
    ] {
        for ending in ["\n", "\r\n", "\r"] {
            let source = format!("a{ending}0123456789{ending}");
            let mut editor = saved(&source);
            editor.cursor_col = 8;
            operation(&mut editor);
            assert_eq!(editor.rope, source);
            assert_eq!((editor.cursor_line, editor.cursor_col), (0, 1));
            assert!(!editor.modified);
            assert!(!editor.can_undo());
        }
    }
}

#[test]
fn imported_cr_history_never_restores_a_caret_inside_a_line_ending() {
    for redo in [false, true] {
        let mut editor = saved("now");
        let state = (
            Rope::from_str("ab\rcd"),
            0,
            100,
            vec![(0, 100)],
            vec![Some((0, 100))],
            Some((0, 100)),
            true,
        );
        if redo {
            editor.import_undo_redo(vec![], vec![state]);
            editor.redo();
        } else {
            editor.import_undo_redo(vec![state], vec![]);
            editor.undo();
        }
        assert_eq!(editor.cursor_col, 2);
        assert_eq!(editor.selection_anchor, Some((0, 2)));
        assert_eq!(editor.extra_cursors, [(0, 2)]);
        assert_eq!(editor.extra_cursor_anchors, [Some((0, 2))]);
        editor.extra_cursors.clear();
        editor.clear_selection();
        editor.insert_char('X');
        assert_eq!(editor.rope, "abX\rcd");
    }
}

#[test]
fn background_save_with_a_reused_version_never_adopts_unrelated_live_text() {
    for transformed in [false, true] {
        let mut editor = saved("reopened file with newer text");
        let original = editor.rope.clone();
        editor.apply_background_save(
            editor.content_version,
            &Rope::from_str("old saved text"),
            Rope::from_str("old saved text"),
            "UTF-8",
            None,
            None,
            transformed.then_some((0, usize::MAX)),
            u64::MAX,
        );
        assert_eq!(editor.rope, original);
        assert_eq!(editor.saved_rope.as_ref().unwrap(), "old saved text");
        assert!(editor.modified);
    }
}

#[test]
fn save_time_whitespace_edits_are_undoable_even_without_an_earlier_edit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trim.txt");
    let original = "content  ";
    std::fs::write(&path, original).unwrap();
    let mut editor = saved(original);
    editor.file_path = Some(path.clone());
    editor.cursor_col = original.len();
    editor.save_file_with_options(true, true).unwrap();
    assert_eq!(editor.rope, "content\n");
    assert!(!editor.modified);
    editor.undo();
    assert_eq!(editor.rope, original);
    assert!(editor.modified);
    editor.redo();
    assert_eq!(editor.rope, "content\n");
    assert!(!editor.modified);
    assert_eq!(std::fs::read_to_string(path).unwrap(), "content\n");
}

#[test]
fn save_rejects_unrepresentable_characters_without_losing_the_original_file() {
    let dir = tempfile::tempdir().unwrap();
    for (encoding, content) in [("ASCII", "café"), ("ISO-8859-1", "界🙂")] {
        let path = dir.path().join("encoded.txt");
        std::fs::write(&path, "on disk").unwrap();
        let mut editor = saved("on disk");
        editor.file_path = Some(path.clone());
        editor.paste_plain_with_text(content);
        editor.encoding = encoding;
        let before = editor.rope.clone();
        let result = editor.save_file_with_options(false, false);
        assert!(
            result.is_err(),
            "{encoding} must not silently write question marks"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "on disk");
        assert_eq!(editor.rope, before);
        assert!(editor.modified);
        editor.undo();
        assert_eq!(editor.rope, "on disk");
        editor.redo();
        assert_eq!(editor.rope, before);
    }
}

#[test]
fn line_ending_conversion_save_and_history_agree_even_for_consecutive_cr_lines() {
    let dir = tempfile::tempdir().unwrap();
    for repetitions in [1, 100] {
        for source_line in ["x\r\ny\r\n", "x\ry\r", "x\r\r\ny\n", "x\r\n\n\ry"] {
            for crlf in [false, true] {
                let source = source_line.repeat(repetitions);
                let mut editor = saved(&source);
                editor.detect_line_ending();
                let previous_ending = editor.cached_line_ending;
                let path = dir.path().join("endings.txt");
                std::fs::write(&path, &source).unwrap();
                editor.file_path = Some(path.clone());
                let normalized = source.replace("\r\n", "\n").replace('\r', "\n");
                let expected = if crlf {
                    normalized.replace('\n', "\r\n")
                } else {
                    normalized
                };
                if crlf {
                    editor.convert_line_endings_to_crlf();
                } else {
                    editor.convert_line_endings_to_lf();
                }
                assert_eq!(
                    editor.rope, expected,
                    "{source_line:?}, repetitions={repetitions}, crlf={crlf}"
                );
                assert_eq!(editor.cached_line_ending, if crlf { "CRLF" } else { "LF" });
                editor.save_file_with_options(false, false).unwrap();
                assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
                assert_eq!(editor.saved_rope.as_ref().unwrap(), expected.as_str());
                if expected != source {
                    editor.undo();
                    assert_eq!(editor.rope, source);
                    assert_eq!(editor.cached_line_ending, previous_ending);
                    assert!(editor.modified);
                    editor.redo();
                    assert_eq!(editor.rope, expected);
                    assert!(!editor.modified);
                }
            }
        }
    }
}

#[test]
fn saved_snapshot_matches_the_decoded_disk_after_pasting_into_a_crlf_document() {
    let dir = tempfile::tempdir().unwrap();
    for encoding in ["UTF-8", "UTF-8 with BOM", "UTF-16 LE", "UTF-16 BE"] {
        let path = dir.path().join("pasted.txt");
        std::fs::write(&path, "first\r\n").unwrap();
        let mut editor = saved("first\r\n");
        editor.cached_line_ending = "CRLF";
        editor.encoding = encoding;
        editor.file_path = Some(path.clone());
        (editor.cursor_line, editor.cursor_col) = (1, 0);
        editor.paste_plain_with_text("second\nthird\n");
        let source = editor.rope.clone();
        let request = editor.background_save_request(false, false, false).unwrap();
        let written = super::write_background_save(&request).unwrap().rope;
        let loaded = super::load_text_file(&path).unwrap();
        assert_eq!(written, loaded.rope, "{encoding}");
        assert_eq!(written, "first\r\nsecond\r\nthird\r\n");
        editor.apply_background_save(
            request.source_content_version,
            &request.rope,
            written.clone(),
            encoding,
            None,
            None,
            Some((0, usize::MAX)),
            u64::MAX,
        );
        assert_eq!(editor.rope, written);
        assert!(!editor.modified);
        editor.undo();
        assert_eq!(editor.rope, source);
        assert!(editor.modified);
        editor.redo();
        assert_eq!(editor.rope, loaded.rope);
        assert!(!editor.modified);
    }
}

#[test]
fn failed_save_transformations_leave_text_selections_and_redo_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let mut editor = saved("text  ");
    editor.insert_char('X');
    editor.undo();
    editor.cursor_col = 6;
    editor.selection_anchor = Some((0, 4));
    editor.file_path = Some(dir.path().join("missing-parent").join("text.txt"));
    let version = editor.content_version;
    assert!(editor.save_file_with_options(true, true).is_err());
    assert_eq!(editor.rope, "text  ");
    assert_eq!(editor.cursor_col, 6);
    assert_eq!(editor.selection_anchor, Some((0, 4)));
    assert_eq!(editor.content_version, version);
    assert!(!editor.modified);
    editor.redo();
    assert_eq!(editor.rope, "Xtext  ");
}

#[test]
fn multicursor_edit_sequences_match_an_independent_character_buffer() {
    // Character offsets are deliberately independent of Rope's line/chunk
    // representation; include reversed/overlapping selections and CRLF joins.
    for seed in 1..=32u64 {
        let mut random = seed;
        let mut next = |limit: usize| {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((random >> 32) as usize) % limit
        };
        let initial = "α\tbc\r\ndef\n界ghi\rjkl";
        let mut text: Vec<char> = initial.chars().collect();
        let mut editor = saved(initial);
        for step in 0..100 {
            let mut locations = Vec::new();
            let (mut offset, mut line, mut col) = (0, 0, 0);
            while offset < text.len() {
                locations.push((offset, line, col));
                if matches!(text[offset], '\r' | '\n') {
                    offset += if text[offset] == '\r' && text.get(offset + 1) == Some(&'\n') {
                        2
                    } else {
                        1
                    };
                    line += 1;
                    col = 0;
                } else {
                    offset += 1;
                    col += 1;
                }
            }
            locations.push((offset, line, col));
            let mut ranges = Vec::new();
            editor.extra_cursors.clear();
            editor.extra_cursor_anchors.clear();
            let operation = next(4);
            let count = 2 + next(3);
            for index in 0..count {
                let (position, line, col) = locations[next(locations.len())];
                let (anchor, al, ac) = if next(3) == 0 {
                    locations[next(locations.len())]
                } else {
                    (position, line, col)
                };
                if index == 0 {
                    (editor.cursor_line, editor.cursor_col) = (line, col);
                    editor.selection_anchor = (anchor != position).then_some((al, ac));
                } else {
                    editor.extra_cursors.push((line, col));
                    editor
                        .extra_cursor_anchors
                        .push((anchor != position).then_some((al, ac)));
                }
                let (mut start, mut end) = (position.min(anchor), position.max(anchor));
                if start == end {
                    if operation == 1 && start > 0 {
                        start -= if start >= 2 && text[start - 2..start] == ['\r', '\n'] {
                            2
                        } else {
                            1
                        };
                    } else if operation == 2 && end < text.len() {
                        end += if text[end] == '\r' && text.get(end + 1) == Some(&'\n') {
                            2
                        } else {
                            1
                        };
                    }
                }
                ranges.push((start, end));
            }
            ranges.sort_unstable();
            let mut merged: Vec<(usize, usize)> = Vec::new();
            for (start, end) in ranges {
                if let Some(last) = merged
                    .last_mut()
                    .filter(|last| start < last.1 || start == last.0)
                {
                    last.1 = last.1.max(end);
                } else {
                    merged.push((start, end));
                }
            }
            let payload = ["é", "X\nY", "\tP", "one\ntwo\nthree"][next(4)];
            let pieces: Vec<_> = payload.split('\n').collect();
            let before: String = text.iter().collect();
            let before_cursors = (
                editor.cursor_line,
                editor.cursor_col,
                editor.selection_anchor,
                editor.extra_cursors.clone(),
                editor.extra_cursor_anchors.clone(),
            );
            let undo_len = editor.undo_stack.len();
            let mut expected = Vec::new();
            let mut copied = 0;
            for (index, &(start, end)) in merged.iter().enumerate() {
                expected.extend_from_slice(&text[copied..start]);
                let inserted = match operation {
                    0 => "x",
                    3 if pieces.len() == merged.len() => pieces[index],
                    3 => payload,
                    _ => "",
                };
                expected.extend(inserted.chars());
                copied = end;
            }
            expected.extend_from_slice(&text[copied..]);
            match operation {
                0 => editor.multi_cursor_insert_char('x'),
                1 => editor.multi_cursor_backspace(),
                2 => editor.multi_cursor_delete(),
                _ => editor.paste_plain_with_text(payload),
            }
            let after: String = expected.iter().collect();
            assert_eq!(
                editor.rope, after,
                "seed={seed} step={step} operation={operation}, before={before:?}"
            );
            if editor.undo_stack.len() > undo_len {
                editor.undo();
                assert_eq!(editor.rope, before, "undo seed={seed} step={step}");
                assert_eq!(
                    (
                        editor.cursor_line,
                        editor.cursor_col,
                        editor.selection_anchor,
                        editor.extra_cursors.clone(),
                        editor.extra_cursor_anchors.clone()
                    ),
                    before_cursors
                );
                assert_eq!(
                    editor.modified,
                    before != editor.saved_rope.as_ref().unwrap().to_string()
                );
                editor.redo();
                assert_eq!(editor.rope, after, "redo seed={seed} step={step}");
                assert_eq!(
                    editor.modified,
                    after != editor.saved_rope.as_ref().unwrap().to_string()
                );
            }
            if step % 13 == 0 {
                editor.record_saved_snapshot(editor.rope.clone());
            }
            for cursor in editor.collect_multi_cursors() {
                assert!(cursor.line < editor.line_count());
                assert!(cursor.col <= editor.line_len(cursor.line));
            }
            text = expected;
        }
    }
}

#[test]
fn encoding_changes_update_save_versions_without_dirtying_unchanged_choices_or_losing_redo() {
    let mut editor = saved("saved");
    editor.insert_char('X');
    editor.undo();
    let version = editor.content_version;
    editor.set_encoding("UTF-8");
    assert_eq!(editor.content_version, version);
    assert!(!editor.modified);
    editor.set_encoding("UTF-16 LE");
    assert!(editor.content_version > version);
    assert!(editor.modified);
    editor.set_encoding("UTF-8");
    assert!(!editor.modified);
    assert!(editor.can_redo());
    editor.redo();
    assert_eq!(editor.rope, "Xsaved");
    assert!(editor.modified);
}
