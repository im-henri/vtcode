use super::*;
use crate::editor_line_ops::LinePrefixEdit;
use std::{hint::black_box, time::Instant};

#[test]
fn soft_tabs_match_scalar_edits_with_overwrite_virtual_columns_wrapping_and_undo() {
    for source in ["hello界world\r\nnext", "\tcode\n", "", "x\n"] {
        for size in [1, 2, 4, 8, 513] {
            for col in [0, 1, 4, 16] {
                for (wrapped, overwrite) in [(false, false), (true, false), (false, true)] {
                    let mut actual = Editor::test_with_text(source);
                    let mut expected = Editor::test_with_text(source);
                    for editor in [&mut actual, &mut expected] {
                        editor.tab_size = size;
                        editor.cursor_col = col;
                        editor.word_wrap = wrapped;
                        editor.overwrite_mode = overwrite;
                        editor.viewport_width = 21;
                        editor.viewport_height = 5;
                    }
                    let version = actual.content_version;
                    actual.insert_tab();
                    // Tab stops follow painted columns, including wide glyphs
                    // and hard tabs. A virtual caret inserts at the real EOL.
                    let visual = expected.rope.line(0).chars()
                        .take(col.min(expected.line_len(0)))
                        .fold(0, |cells, ch| cells + if ch == '\t' { size - cells % size }
                            else { unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) });
                    for _ in 0..size - visual % size {
                        expected.insert_char(' ');
                    }
                    assert_eq!(actual.rope, expected.rope);
                    assert_eq!(
                        (actual.cursor_line, actual.cursor_col),
                        (expected.cursor_line, expected.cursor_col)
                    );
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
                    assert_eq!(actual.undo_stack.len(), expected.undo_stack.len());
                    assert_eq!(actual.content_version, version + 1);
                    let edited = actual.rope.clone();
                    actual.undo();
                    assert_eq!(actual.rope.to_string(), source);
                    actual.redo();
                    assert_eq!(actual.rope, edited);
                }
            }
        }
    }
}

#[test]
fn soft_tabs_keep_whitespace_undo_groups_and_handle_zero_width_defensively() {
    let mut editor = Editor::test_with_text("code");
    editor.insert_char(' ');
    editor.insert_tab();
    editor.insert_char(' ');
    assert_eq!(editor.undo_stack.len(), 1);
    editor.undo();
    assert_eq!(editor.rope.to_string(), "code");
    editor.tab_size = 0;
    editor.insert_tab();
    assert_eq!(editor.rope.to_string(), " code");
}

#[test]
fn dense_prefix_edits_preserve_unicode_endings_cursor_anchors_and_history() {
    for ending in ["\n", "\r\n", "\r"] {
        for count in [15, 16, 96, 2000] {
            let source = format!("  界é value{ending}").repeat(count);
            let mut editor = Editor::test_with_text(&source);
            editor.cursor_line = count - 1;
            editor.cursor_col = 5;
            editor.selection_anchor = Some((0, 1));
            editor.extra_cursors = vec![(2, 3), (8, 0)];
            editor.extra_cursor_anchors = vec![Some((1, 5)), None];
            editor.bookmarks.insert(4);
            let edits: Vec<_> = (0..count)
                .map(|line| LinePrefixEdit {
                    line,
                    index: editor.rope.line_to_char(line),
                    col: 0,
                    removed: 2,
                    insert: "\t//界 ",
                })
                .collect();
            editor.apply_line_prefix_edits(&edits);
            assert_eq!(
                editor.rope.to_string(),
                format!("\t//界 界é value{ending}").repeat(count)
            );
            assert_eq!(editor.cursor_col, 8);
            assert_eq!(editor.selection_anchor, Some((0, 5)));
            assert_eq!(editor.extra_cursors, [(2, 6), (8, 5)]);
            assert_eq!(editor.extra_cursor_anchors, [Some((1, 8)), None]);
            assert!(editor.bookmarks.contains(&4));
            let edited = editor.rope.clone();
            editor.undo();
            assert_eq!(editor.rope.to_string(), source);
            assert_eq!(editor.cursor_col, 5);
            assert_eq!(editor.selection_anchor, Some((0, 1)));
            editor.redo();
            assert_eq!(editor.rope, edited);
        }
    }
}

#[test]
fn dense_replacement_agrees_with_reverse_edits_and_rejects_sparse_work() {
    let rope = ropey::Rope::from_str(&"界 alpha é beta 🦀\n".repeat(800));
    let edits: Vec<_> = (0..96)
        .map(|i| {
            (
                i * 47,
                i * 47 + i % 9,
                if i % 2 == 0 { "" } else { "🦀test" },
            )
        })
        .collect();
    let mut expected = rope.clone();
    for &(start, end, text) in edits.iter().rev() {
        expected.remove(start..end);
        expected.insert(start, text);
    }
    let (start, end, text) = dense_edit_text(&rope, edits.iter().copied()).unwrap();
    let mut actual = rope.clone();
    actual.remove(start..end);
    actual.insert(start, &text);
    assert_eq!(actual, expected);
    assert!(dense_edit_text(&rope, edits[..15].iter().copied()).is_none());
    let sparse = ropey::Rope::from_str(&"x".repeat(100_000));
    assert!(dense_edit_text(&sparse, (0..16).map(|i| (i * 5000, i * 5000, "x"))).is_none());
}

#[test]
#[ignore = "manual optimized-profile input comparison"]
fn benchmark_soft_tab_input() {
    for wrapped in [false, true] {
        for size in [4, 8, 64] {
            let mut samples = [Vec::new(), Vec::new()];
            for round in 0..7 {
                for batched in [round % 2 == 0, round % 2 != 0] {
                    let mut editor =
                        Editor::test_with_text(&"fn example() { work(); }\n".repeat(2000));
                    editor.cursor_line = 1000;
                    editor.tab_size = size;
                    editor.word_wrap = wrapped;
                    editor.viewport_width = 80;
                    editor.viewport_height = 40;
                    editor.ensure_visible();
                    let start = Instant::now();
                    for _ in 0..100 {
                        if batched {
                            editor.insert_tab();
                        } else {
                            for _ in 0..size {
                                editor.insert_char(' ');
                            }
                        }
                        editor.undo();
                    }
                    black_box(&editor.rope);
                    samples[usize::from(batched)].push(start.elapsed() / 100);
                }
            }
            for sample in &mut samples {
                sample.sort();
            }
            eprintln!(
                "soft tab size={size} wrapped={wrapped}: scalar={:?} batched={:?}",
                samples[0][3], samples[1][3]
            );
        }
    }
}
