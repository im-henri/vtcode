use crate::editor::Editor;

fn with_cursors(text: &str, positions: &[(usize, usize)]) -> Editor {
    let mut editor = Editor::test_with_text(text);
    (editor.cursor_line, editor.cursor_col) = positions[0];
    editor.extra_cursors = positions[1..].to_vec();
    editor.extra_cursor_anchors = vec![None; positions.len() - 1];
    editor
}

#[test]
fn typing_merges_overlapping_selections_and_maps_remaining_carets() {
    let mut editor = with_cursors("0123456789", &[(0, 5), (0, 7), (0, 9)]);
    editor.selection_anchor = Some((0, 1));
    editor.extra_cursor_anchors[0] = Some((0, 3));
    editor.multi_cursor_insert_char('X');
    assert_eq!(editor.rope.to_string(), "0X78X9");
    assert_eq!((editor.cursor_line, editor.cursor_col), (0, 2));
    assert_eq!(editor.extra_cursors, vec![(0, 5)]);
    assert_eq!(editor.undo_stack.len(), 1);
    editor.undo();
    assert_eq!(editor.rope.to_string(), "0123456789");
    assert_eq!(editor.extra_cursor_anchors, vec![Some((0, 3)), None]);
}

#[test]
fn newline_typing_maps_cursors_after_all_preceding_insertions() {
    let mut editor = with_cursors("ab\ncd", &[(0, 1), (1, 1)]);
    editor.multi_cursor_insert_char('\n');
    assert_eq!(editor.rope.to_string(), "a\nb\nc\nd");
    assert_eq!((editor.cursor_line, editor.cursor_col), (1, 0));
    assert_eq!(editor.extra_cursors, vec![(3, 0)]);
    editor.multi_cursor_insert_char('界');
    assert_eq!(editor.rope.to_string(), "a\n界b\nc\n界d");
    assert_eq!(editor.extra_cursors, vec![(3, 1)]);
}

#[test]
fn deletion_maps_multiline_selections_in_original_coordinates() {
    let mut editor = with_cursors("ab\ncd\nef\ngh", &[(1, 1), (3, 1)]);
    editor.selection_anchor = Some((0, 1));
    editor.extra_cursor_anchors[0] = Some((2, 1));
    editor.multi_cursor_delete();
    assert_eq!(editor.rope.to_string(), "ad\neh");
    assert_eq!((editor.cursor_line, editor.cursor_col), (0, 1));
    assert_eq!(editor.extra_cursors, vec![(1, 1)]);
}

#[test]
fn backspace_and_delete_merge_overlap_and_handle_simultaneous_crlf_joins() {
    for backwards in [false, true] {
        let positions = if backwards {
            vec![(1, 0), (2, 0)]
        } else {
            vec![(0, 1), (1, 1)]
        };
        let mut editor = with_cursors("a\r\nb\r\nc", &positions);
        if backwards {
            editor.multi_cursor_backspace();
        } else {
            editor.multi_cursor_delete();
        }
        assert_eq!(editor.rope.to_string(), "abc");
        assert_eq!((editor.cursor_line, editor.cursor_col), (0, 1));
        assert_eq!(editor.extra_cursors, vec![(0, 2)]);
        editor.undo();
        assert_eq!(editor.rope.to_string(), "a\r\nb\r\nc");
    }
    let mut editor = with_cursors("abcdef", &[(0, 4), (0, 3)]);
    editor.selection_anchor = Some((0, 1));
    editor.multi_cursor_backspace();
    assert_eq!(editor.rope.to_string(), "aef");
    assert_eq!(editor.cursor_col, 1);
    assert!(editor.extra_cursors.is_empty());
}

#[test]
fn noop_deletion_preserves_undo_and_virtual_columns_do_not_remove_text() {
    let mut editor = with_cursors("abc", &[(0, 0)]);
    let version = editor.content_version;
    editor.multi_cursor_backspace();
    assert_eq!(editor.content_version, version);
    assert!(editor.undo_stack.is_empty());
    editor.cursor_col = 99;
    editor.extra_cursors = vec![(0, 1)];
    editor.extra_cursor_anchors = vec![None];
    editor.multi_cursor_backspace();
    assert_eq!(editor.rope.to_string(), "bc");
    assert_eq!(editor.cursor_col, 2);
    assert_eq!(editor.extra_cursors, vec![(0, 0)]);
}

#[test]
fn typing_preserves_unicode_overwrite_auto_close_and_overtype() {
    let mut editor = with_cursors("abcdef", &[(0, 1), (0, 4)]);
    editor.overwrite_mode = true;
    editor.multi_cursor_insert_char('界');
    assert_eq!(editor.rope.to_string(), "a界cd界f");
    assert_eq!(editor.cursor_col, 2);
    assert_eq!(editor.extra_cursors, vec![(0, 5)]);
    let mut editor = with_cursors("a  b ", &[(0, 1), (0, 4)]);
    editor.multi_cursor_insert_char('(');
    assert_eq!(editor.rope.to_string(), "a()  b() ");
    assert_eq!(editor.cursor_col, 2);
    assert_eq!(editor.extra_cursors, vec![(0, 7)]);
    let mut editor = with_cursors("() ()", &[(0, 1), (0, 4)]);
    editor.multi_cursor_insert_char(')');
    assert_eq!(editor.rope.to_string(), "() ()");
    assert_eq!(editor.cursor_col, 2);
    assert_eq!(editor.extra_cursors, vec![(0, 5)]);
    assert!(editor.undo_stack.is_empty());
}

#[test]
fn same_line_tab_alignment_accounts_for_preceding_tabs() {
    let mut editor = with_cursors("abcd", &[(0, 0), (0, 1), (0, 2)]);
    editor.multi_cursor_insert_tab();
    assert_eq!(editor.rope.to_string(), "    a   b   cd");
    assert_eq!(editor.cursor_col, 4);
    assert_eq!(editor.extra_cursors, vec![(0, 8), (0, 12)]);
    editor.undo();
    assert_eq!(editor.rope.to_string(), "abcd");
}

#[test]
fn multiple_cursor_word_deletion_uses_navigation_camel_case_and_unicode_boundaries() {
    let mut editor = with_cursors("fooBarBaz\nväärtusÄnderung", &[(0, 9), (1, 15)]);
    editor.multi_cursor_delete_word_left();
    assert_eq!(editor.rope.to_string(), "fooBar\nväärtus");
    assert_eq!(editor.cursor_col, 6);
    assert_eq!(editor.extra_cursors, vec![(1, 7)]);
    let mut editor = with_cursors("fooBar baz\nfooBar baz", &[(0, 0), (1, 6)]);
    editor.multi_cursor_delete_word_right();
    assert_eq!(editor.rope.to_string(), "Bar baz\nfooBarbaz");
    assert_eq!(editor.cursor_col, 0);
    assert_eq!(editor.extra_cursors, vec![(1, 6)]);
}

#[test]
fn line_boundary_deletion_merges_same_line_prefixes_and_suffixes() {
    for end in [false, true] {
        let mut editor = with_cursors("abcdef\r\n", &[(0, 2), (0, 4)]);
        if end {
            editor.delete_to_line_end();
        } else {
            editor.delete_to_line_start();
        }
        assert_eq!(
            editor.rope.to_string(),
            if end { "ab\r\n" } else { "ef\r\n" }
        );
        assert_eq!(editor.cursor_col, if end { 2 } else { 0 });
        assert!(editor.extra_cursors.is_empty());
        assert_eq!(editor.undo_stack.len(), 1);
    }
}

#[test]
fn indentation_updates_every_cursor_and_anchor_once_and_keeps_zero_column_end_rule() {
    let mut editor = with_cursors("    a\n  b\n\tc\nend", &[(0, 5), (1, 3), (2, 2)]);
    editor.selection_anchor = Some((0, 2));
    editor.extra_cursor_anchors = vec![Some((1, 1)), Some((2, 1))];
    editor.dedent_line();
    assert_eq!(editor.rope.to_string(), "a\nb\nc\nend");
    assert_eq!(editor.cursor_col, 1);
    assert_eq!(editor.extra_cursors, vec![(1, 1), (2, 1)]);
    assert_eq!(
        editor.extra_cursor_anchors,
        vec![Some((1, 0)), Some((2, 0))]
    );
    let mut editor = with_cursors("a\nb\nc", &[(2, 0)]);
    editor.selection_anchor = Some((0, 0));
    editor.indent_lines();
    assert_eq!(editor.rope.to_string(), "    a\n    b\nc");
    assert_eq!(editor.cursor_col, 0);
    assert_eq!(editor.selection_anchor, Some((0, 4)));
    assert_eq!(editor.undo_stack.len(), 1);
}

#[test]
fn comments_use_character_indentation_and_preserve_blank_line_boundaries() {
    let original = " \u{2003}let x = 1;\n \u{2003}let y = 2;\n";
    let mut editor = with_cursors(original, &[(0, 2), (1, 2)]);
    editor.toggle_line_comment();
    assert_eq!(
        editor.rope.to_string(),
        " \u{2003}// let x = 1;\n \u{2003}// let y = 2;\n"
    );
    assert_eq!(editor.cursor_col, 5);
    assert_eq!(editor.extra_cursors, vec![(1, 5)]);
    editor.toggle_line_comment();
    assert_eq!(editor.rope.to_string(), original);
    let mut editor = with_cursors("a\n\nb\n", &[(2, 1)]);
    editor.selection_anchor = Some((0, 0));
    editor.toggle_line_comment();
    assert_eq!(editor.rope.to_string(), "// a\n// \n// b\n");
    editor.toggle_line_comment();
    assert_eq!(editor.rope.to_string(), "a\n\nb\n");
}

#[test]
fn paired_comments_keep_unicode_indentation_in_character_coordinates() {
    let mut editor = with_cursors("\u{2003}<tag>\r\n", &[(0, 1)]);
    editor.file_path = Some("test.html".into());
    editor.toggle_line_comment();
    assert_eq!(editor.rope.to_string(), "\u{2003}<!-- <tag> -->\r\n");
    editor.toggle_line_comment();
    assert_eq!(editor.rope.to_string(), "\u{2003}<tag>\r\n");
}

#[test]
fn joins_skip_blank_indentation_and_preserve_unicode_crlf_and_final_ending() {
    let mut editor = with_cursors(" a \r\n \t界\r\n\r\n  z\r\noutside", &[(3, 3)]);
    editor.selection_anchor = Some((0, 0));
    editor.join_lines();
    assert_eq!(editor.rope.to_string(), " a  界 z\r\noutside");
    assert_eq!((editor.cursor_line, editor.cursor_col), (0, 4));
    editor.undo();
    assert_eq!(
        editor.rope.to_string(),
        " a \r\n \t界\r\n\r\n  z\r\noutside"
    );
}

#[test]
fn duplicate_line_maps_each_cursor_by_insertions_above_that_line() {
    let mut editor = with_cursors("a\nb\nc\nd", &[(0, 1), (2, 1)]);
    editor.duplicate_line();
    assert_eq!(editor.rope.to_string(), "a\na\nb\nc\nc\nd");
    assert_eq!((editor.cursor_line, editor.cursor_col), (1, 1));
    assert_eq!(editor.extra_cursors, vec![(4, 1)]);
    let mut editor = with_cursors("a\nb\nc\nd", &[(0, 1), (2, 1)]);
    editor.duplicate_line_up();
    assert_eq!(editor.rope.to_string(), "a\na\nb\nc\nc\nd");
    assert_eq!((editor.cursor_line, editor.cursor_col), (0, 1));
    assert_eq!(editor.extra_cursors, vec![(3, 1)]);
}

#[test]
fn transposing_unequal_unicode_words_reads_only_local_words_and_places_caret_at_end() {
    let mut editor = with_cursors("väike -- 界界界 long unrelated suffix", &[(0, 10)]);
    editor.transpose_words();
    assert_eq!(
        editor.rope.to_string(),
        "界界界 -- väike long unrelated suffix"
    );
    assert_eq!(editor.cursor_col, 12);
    assert_eq!(editor.undo_stack.len(), 1);
}

#[test]
fn wide_prefix_edits_update_distant_rows_in_a_warm_wrap_layout() {
    let mut editor = with_cursors(&"abcd\n".repeat(40), &[(39, 4)]);
    editor.selection_anchor = Some((0, 0));
    editor.word_wrap = true;
    editor.viewport_width = 4;
    assert_eq!(editor.wrapped_total_rows(4), 41);
    editor.indent_lines();
    assert_eq!(editor.wrapped_total_rows(4), 81);
    editor.dedent_line();
    assert_eq!(editor.wrapped_total_rows(4), 41);
    editor.toggle_line_comment();
    assert_eq!(editor.wrapped_total_rows(4), 81);
    assert_eq!(editor.wrap_ranges_for_line(38, 4).len(), 2);
}

fn legacy_multi_insert(editor: &mut Editor) {
    use crate::editor_multicursor::{apply_col_offset, sort_for_offset_sensitive_edit};
    let cursors = editor.collect_multi_cursors();
    editor.save_undo();
    let mut ordered: Vec<_> = cursors.iter().copied().enumerate().collect();
    sort_for_offset_sensitive_edit(&mut ordered);
    let mut offsets = rustc_hash::FxHashMap::default();
    let mut positions = vec![(0, 0); cursors.len()];
    for (index, cursor) in ordered {
        let line = cursor.line.min(editor.line_count() - 1);
        let len = editor.line_len(line);
        let col = apply_col_offset(cursor.col, *offsets.get(&line).unwrap_or(&0), len);
        let position = editor.rope.line_to_char(line) + col;
        editor.rope.insert(position, "X");
        *offsets.entry(line).or_insert(0) += 1;
        positions[index] = (line, col + 1);
    }
    // Use the same final state application on both sides to isolate edit
    // planning rather than exaggerating the allocation improvements there.
    editor.set_multi_cursor_positions(positions);
    editor.modified = true;
}

fn legacy_long_word_delete_left(editor: &mut Editor) {
    let cursors = editor.collect_multi_cursors();
    editor.save_undo();
    let mut ordered: Vec<_> = cursors.iter().copied().enumerate().collect();
    ordered.sort_unstable_by_key(|&(_, cursor)| std::cmp::Reverse((cursor.line, cursor.col)));
    let mut positions = vec![(0, 0); cursors.len()];
    for (index, cursor) in ordered {
        let line_start = editor.rope.line_to_char(cursor.line);
        let mut target = cursor.col;
        while target > 0 && editor.rope.char(line_start + target - 1).is_whitespace() {
            target -= 1;
        }
        while target > 0 {
            let c = editor.rope.char(line_start + target - 1);
            if !c.is_alphanumeric() && c != '_' {
                break;
            }
            target -= 1;
        }
        editor
            .rope
            .remove(line_start + target..line_start + cursor.col);
        positions[index] = (cursor.line, target);
    }
    editor.set_multi_cursor_positions(positions);
    editor.modified = true;
}

fn legacy_indent_with_per_line_cursor_scans(editor: &mut Editor) {
    editor.save_undo();
    let last = editor
        .extra_cursors
        .iter()
        .map(|&(line, _)| line)
        .max()
        .unwrap_or(editor.cursor_line);
    for line in 0..=last {
        let start = editor.rope.line_to_char(line);
        editor.rope.insert(start, "    ");
        if editor.cursor_line == line {
            editor.cursor_col += 4;
        }
        for cursor in &mut editor.extra_cursors {
            if cursor.0 == line {
                cursor.1 += 4;
            }
        }
        for anchor in editor.extra_cursor_anchors.iter_mut().flatten() {
            if anchor.0 == line {
                anchor.1 += 4;
            }
        }
    }
    editor.modified = true;
    editor.content_version += 1;
}

fn legacy_comment_long_line(editor: &mut Editor) {
    editor.save_undo();
    let all_commented = editor
        .rope
        .line(0)
        .to_string()
        .trim_start()
        .starts_with("//");
    assert!(!std::hint::black_box(all_commented));
    let min_indent = {
        let text = editor.rope.line(0).to_string();
        text.len() - text.trim_start().len()
    };
    let text = editor.rope.line(0).to_string();
    let indent = text.len() - text.trim_start().len();
    editor.rope.insert(min_indent.min(indent), "// ");
    editor.cursor_col += 3;
    editor.modified = true;
}

fn legacy_join_growing_suffix(editor: &mut Editor) {
    let (first, _, last, _) = editor.selection_range().unwrap();
    editor.save_undo();
    for line in (first..last).rev() {
        let current = editor.rope.line(line).to_string();
        let next = editor.rope.line(line + 1).to_string();
        let trimmed = next.trim_start();
        let content_end = current.trim_end_matches(['\n', '\r']).chars().count();
        let start = editor.rope.line_to_char(line) + content_end;
        let leading = next.chars().count() - trimmed.chars().count();
        editor
            .rope
            .remove(start..editor.rope.line_to_char(line + 1) + leading);
        if content_end > 0 && !trimmed.is_empty() {
            editor.rope.insert(start, " ");
        }
        if line == first {
            editor.cursor_col = content_end + usize::from(content_end > 0 && !trimmed.is_empty());
        }
    }
    editor.clear_selection();
    editor.extra_cursors.clear();
    editor.extra_cursor_anchors.clear();
    editor.modified = true;
    editor.clear_stale_folds();
    editor.ensure_visible();
}

fn compare_edit_operations(
    label: &str,
    text: &str,
    cursors: &[(usize, usize)],
    setup: impl Fn(&mut Editor),
    legacy: impl Fn(&mut Editor),
    optimized: impl Fn(&mut Editor),
) {
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut expected = None;
    for sample in 0..7 {
        for fast in [sample % 2 == 0, sample % 2 != 0] {
            let mut editor = with_cursors(text, cursors);
            setup(&mut editor);
            let started = std::time::Instant::now();
            if fast {
                optimized(&mut editor);
            } else {
                legacy(&mut editor);
            }
            let elapsed = started.elapsed();
            let result = (
                editor.rope.to_string(),
                (editor.cursor_line, editor.cursor_col),
                editor.extra_cursors,
            );
            if let Some(expected) = &expected {
                assert_eq!(&result, expected);
            } else {
                expected = Some(result);
            }
            if fast {
                after.push(elapsed);
            } else {
                before.push(elapsed);
            }
        }
    }
    before.sort();
    after.sort();
    eprintln!(
        "{label}: before={:?} after={:?} per operation",
        before[3], after[3]
    );
}

#[test]
#[ignore = "manual release editor benchmark"]
fn benchmark_multicursor_edit_planning_and_word_deletion() {
    let text = "abcd ".repeat(4_096);
    let positions: Vec<_> = (0..1_024).map(|index| (0, index * 20)).collect();
    compare_edit_operations(
        "1024 cursors typing / 20k line",
        &text,
        &positions,
        |_| {},
        legacy_multi_insert,
        |editor| editor.multi_cursor_insert_char('X'),
    );
    let text = format!("{}\n", "ä".repeat(16_384)).repeat(8);
    let positions: Vec<_> = (0..8).map(|line| (line, 16_384)).collect();
    compare_edit_operations(
        "8 cursors deleting 16k Unicode words",
        &text,
        &positions,
        |_| {},
        legacy_long_word_delete_left,
        Editor::multi_cursor_delete_word_left,
    );
}

#[test]
#[ignore = "manual release editor benchmark"]
fn benchmark_indent_comment_and_join_avoid_repeated_work() {
    let text = "body\n".repeat(1_024);
    let positions: Vec<_> = (0..1_024).map(|line| (line, 0)).collect();
    compare_edit_operations(
        "1024 lines/cursors indent",
        &text,
        &positions,
        |_| {},
        legacy_indent_with_per_line_cursor_scans,
        Editor::indent_lines,
    );
    let text = format!("    {}", "x".repeat(2 * 1024 * 1024));
    compare_edit_operations(
        "2MiB line comment toggle",
        &text,
        &[(0, 4)],
        |_| {},
        legacy_comment_long_line,
        Editor::toggle_line_comment,
    );
    let text = format!("    {}\n", "x".repeat(76)).repeat(1_024);
    compare_edit_operations(
        "1024-line selection join",
        &text,
        &[(0, 0)],
        |editor| {
            editor.selection_anchor = Some((1_023, 80));
        },
        legacy_join_growing_suffix,
        Editor::join_lines,
    );
}

#[test]
fn backspace_removes_only_the_last_of_adjacent_non_crlf_line_boundaries() {
    for boundary in ['\r', '\u{2028}', '\u{0085}'] {
        let mut editor = with_cursors(&format!("a\r{boundary}b"), &[(2, 0)]);
        if editor.line_count() < 3 {
            continue;
        } // Unicode line splitting is optional in Ropey.
        editor.multi_cursor_backspace();
        assert_eq!(editor.rope.to_string(), "a\rb");
        assert_eq!((editor.cursor_line, editor.cursor_col), (1, 0));
    }
}

#[test]
fn word_backspace_updates_warm_layout_only_after_deleting_the_word() {
    for text in ["abcdefghijkl", "ääääääääääää"] {
        let mut editor = with_cursors(text, &[(0, 12)]);
        editor.word_wrap = true;
        editor.viewport_width = 4;
        assert_eq!(editor.wrapped_total_rows(4), 3);
        editor.delete_word_left();
        assert_eq!(editor.rope.to_string(), "");
        assert_eq!(editor.cursor_col, 0);
        assert_eq!(editor.wrapped_total_rows(4), 1);
        assert_eq!(editor.wrap_ranges_for_line(0, 4).as_slice(), &[(0, 0)]);
        assert_eq!(editor.undo_stack.len(), 1);
        editor.undo();
        assert_eq!(editor.rope.to_string(), text);
        assert_eq!(editor.wrapped_total_rows(4), 3);
        assert_eq!(editor.cursor_col, 12);
    }
}

#[test]
fn dense_edits_preserve_unicode_crlf_primary_order_bookmarks_and_undo() {
    let original = "é界code\r\n".repeat(64);
    for reverse in [false, true] {
        for operation in 0..4 {
            let mut positions: Vec<_> = (0..64).map(|line| (line, 2)).collect();
            if reverse {
                positions.reverse();
            }
            let mut editor = with_cursors(&original, &positions);
            editor.bookmarks.extend([0, 17, 63]);
            match operation {
                0 => editor.multi_cursor_insert_char('λ'),
                1 => editor.multi_cursor_backspace(),
                2 => editor.multi_cursor_delete(),
                _ => editor.multi_cursor_insert_char('\n'),
            }
            let expected = match operation {
                0 => "é界λcode\r\n",
                1 => "écode\r\n",
                2 => "é界ode\r\n",
                _ => "é界\ncode\r\n",
            }
            .repeat(64);
            assert_eq!(editor.rope.to_string(), expected);
            let mapped: Vec<_> = positions
                .iter()
                .map(|&(line, _)| {
                    if operation == 3 {
                        (line * 2 + 1, 0)
                    } else {
                        (line, [3, 1, 2][operation])
                    }
                })
                .collect();
            assert_eq!((editor.cursor_line, editor.cursor_col), mapped[0]);
            assert_eq!(editor.extra_cursors, mapped[1..]);
            assert_eq!(
                editor.bookmarks.iter().copied().collect::<Vec<_>>(),
                if operation == 3 {
                    vec![0, 34, 126]
                } else {
                    vec![0, 17, 63]
                }
            );
            assert_eq!(editor.undo_stack.len(), 1);
            editor.undo();
            assert_eq!(editor.rope.to_string(), original);
            assert_eq!((editor.cursor_line, editor.cursor_col), positions[0]);
            assert_eq!(editor.extra_cursors, positions[1..]);
            editor.redo();
            assert_eq!(editor.rope.to_string(), expected);
            assert_eq!(editor.extra_cursors, mapped[1..]);
        }
    }
}

#[test]
fn dense_overlapping_replacements_merge_without_losing_primary_cursor_precedence() {
    let original = "0123456789\n".repeat(32);
    let positions: Vec<_> = (0..32).flat_map(|line| [(line, 5), (line, 7)]).collect();
    let mut editor = with_cursors(&original, &positions);
    editor.selection_anchor = Some((0, 1));
    for (index, anchor) in editor.extra_cursor_anchors.iter_mut().enumerate() {
        let cursor = positions[index + 1];
        *anchor = Some((cursor.0, if cursor.1 == 5 { 1 } else { 3 }));
    }
    editor.multi_cursor_insert_char('界');
    assert_eq!(editor.rope.to_string(), "0界789\n".repeat(32));
    assert_eq!((editor.cursor_line, editor.cursor_col), (0, 2));
    assert_eq!(
        editor.extra_cursors,
        (1..32).map(|line| (line, 2)).collect::<Vec<_>>()
    );
    editor.undo();
    assert_eq!(editor.rope.to_string(), original);
    assert_eq!(editor.extra_cursors, positions[1..]);
}
