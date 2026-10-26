use super::*;
use ropey::Rope;

impl Editor {
    pub(crate) fn reference_delete_line(&mut self) {
        self.desired_col = None;
        self.save_undo();

        // Collect all lines to delete from primary and extra cursors
        let mut lines_to_delete = std::collections::BTreeSet::new();
        if let Some((sl, _, el, ec)) = self.selection_range() {
            let end = if ec == 0 && el > sl { el - 1 } else { el };
            for l in sl..=end {
                lines_to_delete.insert(l);
            }
            self.clear_selection();
        } else {
            lines_to_delete.insert(self.cursor_line);
        }
        // Include extra cursor selection ranges (not just cursor lines)
        for (i, &(line, col)) in self.extra_cursors.iter().enumerate() {
            if let Some(Some(anchor)) = self.extra_cursor_anchors.get(i) {
                let (start, end) = if *anchor <= (line, col) {
                    (*anchor, (line, col))
                } else {
                    ((line, col), *anchor)
                };
                let end_line = if end.1 == 0 && end.0 > start.0 {
                    end.0 - 1
                } else {
                    end.0
                };
                for l in start.0..=end_line {
                    lines_to_delete.insert(l);
                }
            } else {
                lines_to_delete.insert(line);
            }
        }
        self.extra_cursors.clear();
        self.extra_cursor_anchors.clear();

        let total = self.line_count();
        if total <= 1 && lines_to_delete.contains(&0) {
            let len = self.rope.len_chars();
            if len > 0 {
                self.rope.remove(0..len);
            }
            self.cursor_line = 0;
            self.cursor_col = 0;
            self.modified = true;
            return;
        }

        // Delete lines from bottom to top to keep indices valid
        for &line_idx in lines_to_delete.iter().rev() {
            if line_idx >= self.rope.len_lines() {
                continue;
            }
            let line_start = self.rope.line_to_char(line_idx);
            let line_end = if line_idx + 1 < self.rope.len_lines() {
                self.rope.line_to_char(line_idx + 1)
            } else {
                self.rope.len_chars()
            };
            if line_idx + 1 >= self.rope.len_lines() && line_start > 0 {
                let separator = self.rope.line_to_char(line_idx - 1) + self.line_len(line_idx - 1);
                self.rope.remove(separator..line_end);
            } else {
                self.rope.remove(line_start..line_end);
            }
        }

        let first = *lines_to_delete.iter().next().unwrap_or(&0);
        let num_deleted = lines_to_delete.len();
        self.cursor_line = first.min(self.line_count().saturating_sub(1));
        self.clamp_cursor_col();
        self.modified = true;
        // Shift bookmarks: remove bookmarks on deleted lines and shift others
        // Process deleted lines bottom-to-top to maintain correct offsets
        for &del_line in lines_to_delete.iter().rev() {
            self.shift_bookmarks(del_line, -1);
        }
        // Handle single-line file case: bookmarks were cleared in the early return
        let _ = num_deleted;
        self.clear_stale_folds();
        self.ensure_visible();
    }

    fn reference_swap_blocks(&mut self, first: usize, middle: usize, last: usize) {
        self.mark_syntax_dirty_range(first, last);
        let start = self.rope.line_to_char(first);
        let split = self.rope.line_to_char(middle);
        let end = if last + 1 < self.line_count() {
            self.rope.line_to_char(last + 1)
        } else {
            self.rope.len_chars()
        };
        let left = self.rope.slice(start..split).to_string();
        let mut right = self.rope.slice(split..end).to_string();
        let left = if split_line_ending(&right).1.is_empty() {
            let (content, ending) = split_line_ending(&left);
            right.push_str(ending);
            content
        } else {
            &left
        };
        right.push_str(left);
        self.rope.remove(start..end);
        self.rope.insert(start, &right);
    }
}

#[test]
fn grouped_line_deletion_matches_per_line_edits_and_bookmarks() {
    for source in [
        "a\nbbb\n界界\nlast",
        "a\r\nbbb\r\n界界\r\nlast\r\n",
        "a\rbbb\r界界\rlast",
    ] {
        let rows = Rope::from_str(source).len_lines();
        for first in 0..rows {
            for last in first..rows {
                for column_zero in [true, false] {
                    for backwards in [true, false] {
                        for extras in [true, false] {
                            let make = || {
                                let mut ed = Editor::test_with_text(source);
                                let start = (first, 0);
                                let end = (last, if column_zero { 0 } else { ed.line_len(last) });
                                let (anchor, cursor) = if backwards {
                                    (end, start)
                                } else {
                                    (start, end)
                                };
                                ed.selection_anchor = Some(anchor);
                                (ed.cursor_line, ed.cursor_col) = cursor;
                                ed.bookmarks.extend(0..rows);
                                if extras {
                                    ed.extra_cursors = vec![(0, 1), (rows - 1, 0)];
                                    ed.extra_cursor_anchors = vec![None, Some((rows - 2, 1))];
                                }
                                ed
                            };
                            let mut actual = make();
                            let mut expected = make();
                            actual.delete_line();
                            expected.reference_delete_line();
                            assert_eq!(actual.rope, expected.rope, "{source:?} {first}..{last}");
                            assert_eq!(
                                (actual.cursor_line, actual.cursor_col),
                                (expected.cursor_line, expected.cursor_col)
                            );
                            assert_eq!(actual.bookmarks, expected.bookmarks);
                            assert_eq!(actual.extra_cursors, expected.extra_cursors);
                            assert_eq!(actual.selection_anchor, expected.selection_anchor);
                            actual.undo();
                            assert_eq!(actual.rope.to_string(), source);
                            actual.redo();
                            assert_eq!(actual.rope, expected.rope);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn deleting_entire_single_line_clears_its_bookmark() {
    let mut editor = Editor::test_with_text("hello");
    editor.bookmarks.insert(0);
    editor.delete_line();
    assert!(editor.rope.len_chars() == 0 && editor.bookmarks.is_empty());
}

#[test]
fn shared_line_swaps_match_text_reference_at_every_boundary() {
    for ending in ["\n", "\r\n", "\r"] {
        for terminated in [false, true] {
            let mut source = ["α", "", "界café", "z"].join(ending);
            if terminated {
                source.push_str(ending);
            }
            for first in 0..3 {
                for middle in first + 1..4 {
                    for last in middle..4 {
                        let mut actual = Editor::test_with_text(&source);
                        let mut expected = Editor::test_with_text(&source);
                        actual.swap_adjacent_line_blocks(first, middle, last);
                        expected.reference_swap_blocks(first, middle, last);
                        assert_eq!(
                            actual.rope, expected.rope,
                            "{source:?} {first},{middle},{last}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn shared_duplication_keeps_multiline_unicode_selection_and_undo() {
    for repeats in [1, 2048] {
        for ending in ["\n", "\r\n"] {
            let source = format!(
                "head{ending}α{}Z{ending}tail",
                format!("界café{ending}").repeat(repeats)
            );
            for backwards in [false, true] {
                let mut editor = Editor::test_with_text(&source);
                let start = editor.rope.line_to_char(1) + 1;
                let end = editor.rope.len_chars() - 2;
                let position = |offset| {
                    let line = editor.rope.char_to_line(offset);
                    (line, offset - editor.rope.line_to_char(line))
                };
                let (anchor, cursor) = if backwards {
                    (position(end), position(start))
                } else {
                    (position(start), position(end))
                };
                editor.selection_anchor = Some(anchor);
                (editor.cursor_line, editor.cursor_col) = cursor;
                let selected = editor.selected_text().unwrap();
                editor.duplicate_line_up();
                assert_eq!(editor.selected_text().as_deref(), Some(selected.as_str()));
                let mut expected = Rope::from_str(&source);
                expected.insert(start, &selected);
                assert_eq!(editor.rope, expected);
                editor.undo();
                assert_eq!(editor.rope.to_string(), source);
                assert_eq!(editor.selection_anchor, Some(anchor));
                assert_eq!((editor.cursor_line, editor.cursor_col), cursor);
                editor.duplicate_line();
                expected = Rope::from_str(&source);
                expected.insert(end, &selected);
                assert_eq!(editor.rope, expected);
                assert_eq!(editor.selection_anchor, None);
                assert_eq!(
                    editor.rope.line_to_char(editor.cursor_line) + editor.cursor_col,
                    end + end - start
                );
            }
        }
    }
}

#[test]
#[ignore = "manual optimized bulk editing comparison, setup excluded"]
fn benchmark_grouped_and_shared_editor_edits() {
    use std::{hint::black_box, time::Instant};
    for operation in ["delete", "move", "duplicate"] {
        let text = if operation == "delete" {
            "some code\n".repeat(4000)
        } else {
            format!("{}\n", "some code α界 ".repeat(80)).repeat(4000)
        };
        let mut timings = [Vec::new(), Vec::new()];
        for sample in 0..7 {
            for optimized in [sample % 2 == 0, sample % 2 != 0] {
                let mut ed = Editor::test_with_text(&text);
                ed.selection_anchor = Some((1, 0));
                ed.cursor_line = 3998;
                if operation == "delete" {
                    ed.bookmarks.extend((0..4000).step_by(4));
                }
                let start = Instant::now();
                match (operation, optimized) {
                    ("delete", true) => ed.delete_line(),
                    ("delete", false) => ed.reference_delete_line(),
                    ("move", true) => {
                        ed.save_undo();
                        ed.swap_adjacent_line_blocks(0, 1, 3998);
                    }
                    ("move", false) => {
                        ed.save_undo();
                        ed.reference_swap_blocks(0, 1, 3998);
                    }
                    (_, true) => ed.duplicate_line(),
                    (_, false) => {
                        ed.save_undo();
                        let selected = ed.selected_text().unwrap();
                        let end = ed.rope.line_to_char(ed.cursor_line);
                        ed.rope.insert(end, &selected);
                        let index = end + selected.chars().count();
                        ed.cursor_line = ed.rope.char_to_line(index);
                        ed.cursor_col = index - ed.rope.line_to_char(ed.cursor_line);
                        ed.clear_selection();
                        ed.modified = true;
                        ed.clear_stale_folds();
                        ed.ensure_visible();
                    }
                }
                timings[usize::from(optimized)].push(start.elapsed());
                black_box(&ed);
            }
        }
        for samples in &mut timings {
            samples.sort_unstable();
        }
        eprintln!(
            "bulk {operation}: before={:?}, after={:?}",
            timings[0][3], timings[1][3]
        );
    }
}

impl Editor {
    pub(crate) fn reference_duplicate_line(&mut self) {
        self.desired_col = None;
        self.save_undo();
        if let Some(text) = self.selected_text() {
            // Duplicate selection: insert copy after selection end
            let Some((_, _, el, ec)) = self.selection_range() else {
                return;
            };
            let end_line_start = self.rope.line_to_char(el);
            let insert_pos = end_line_start + ec;
            self.rope.insert(insert_pos, &text);
            self.modified = true;
            // Move cursor to end of duplicated text
            let new_idx = insert_pos + text.chars().count();
            let new_line = self.rope.char_to_line(new_idx);
            let new_col = new_idx - self.rope.line_to_char(new_line);
            self.cursor_line = new_line;
            self.cursor_col = new_col;
            self.clear_selection();
        } else {
            // Collect all unique lines from primary + extra cursors
            let mut lines: Vec<usize> = vec![self.cursor_line];
            for &(l, _) in &self.extra_cursors {
                lines.push(l);
            }
            lines.sort_unstable();
            lines.dedup();

            // Duplicate lines bottom-to-top to keep indices stable for earlier inserts
            for &line_idx in lines.iter().rev() {
                let line_slice = self.rope.line(line_idx);
                let line = line_slice.to_string();
                let line_start = self.rope.line_to_char(line_idx);
                let line_chars = line_slice.len_chars();
                let insert_pos = line_start + line_chars;
                if !split_line_ending(&line).1.is_empty() {
                    self.rope.insert(insert_pos, &line);
                } else {
                    let ending = if self.cached_line_ending == "CRLF" {
                        "\r\n"
                    } else {
                        "\n"
                    };
                    self.rope.insert(insert_pos, ending);
                    self.rope.insert(insert_pos + ending.len(), &line);
                }
            }
            // Move all cursors down by the number of duplicated lines
            self.cursor_line += lines.partition_point(|&line| line <= self.cursor_line);
            if let Some((ref mut al, _)) = self.selection_anchor {
                *al += lines.partition_point(|&line| line <= *al);
            }
            for ec in &mut self.extra_cursors {
                ec.0 += lines.partition_point(|&line| line <= ec.0);
            }
            for (al, _) in self.extra_cursor_anchors.iter_mut().flatten() {
                *al += lines.partition_point(|&line| line <= *al);
            }
            self.modified = true;
        }
        self.clear_stale_folds();
        self.ensure_visible();
    }
}

#[test]
fn shared_large_line_swap_and_duplication_preserve_eof_separators() {
    for ending in ["\n", "\r\n", "\r"] {
        for terminated in [false, true] {
            let mut text = ["α".repeat(2000), "z".repeat(3000), "界".repeat(4000)].join(ending);
            if terminated {
                text.push_str(ending);
            }
            let mut ed = Editor::test_with_text(&text);
            let mut reference = Editor::test_with_text(&text);
            ed.swap_adjacent_line_blocks(0, 1, 2);
            reference.reference_swap_blocks(0, 1, 2);
            assert_eq!(ed.rope, reference.rope);
            for line in 0..ed.line_count() {
                let mut ed = Editor::test_with_text(&text);
                let mut reference = Editor::test_with_text(&text);
                ed.cursor_line = line;
                reference.cursor_line = line;
                ed.duplicate_line();
                reference.reference_duplicate_line();
                assert_eq!(ed.rope, reference.rope);
                assert_eq!(
                    (ed.cursor_line, ed.cursor_col),
                    (reference.cursor_line, reference.cursor_col)
                );
            }
        }
    }
}

#[test]
#[ignore = "manual optimized everyday line editing comparison, setup excluded"]
fn benchmark_small_line_edits() {
    use std::{
        hint::black_box,
        time::{Duration, Instant},
    };
    let source = Rope::from_str(&"    let value = compute(input);\n".repeat(5000));
    for duplicate in [false, true] {
        let mut times = [Vec::new(), Vec::new()];
        for sample in 0..7 {
            for optimized in [sample % 2 == 0, sample % 2 != 0] {
                let mut elapsed = Duration::ZERO;
                for _ in 0..100 {
                    let mut ed = Editor::test_with_text("");
                    ed.rope = source.clone();
                    ed.cursor_line = 2500;
                    let start = Instant::now();
                    match (duplicate, optimized) {
                        (true, true) => ed.duplicate_line(),
                        (true, false) => ed.reference_duplicate_line(),
                        (false, true) => {
                            ed.save_undo();
                            ed.swap_adjacent_line_blocks(2500, 2501, 2501);
                        }
                        (false, false) => {
                            ed.save_undo();
                            ed.reference_swap_blocks(2500, 2501, 2501);
                        }
                    }
                    elapsed += start.elapsed();
                    black_box(&ed);
                }
                times[usize::from(optimized)].push(elapsed / 100);
            }
        }
        for samples in &mut times {
            samples.sort_unstable();
        }
        eprintln!(
            "small line duplicate={duplicate}: before={:?}, after={:?}",
            times[0][3], times[1][3]
        );
    }
}
