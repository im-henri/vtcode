//! Frozen command references from 05c1a19 for equivalence tests and timings.
use super::{Editor, PairedCommentAction};
use ropey::Rope;

fn assert_same(actual: &Editor, expected: &Editor) {
    assert_eq!(actual.rope, expected.rope);
    assert_eq!(
        (
            actual.cursor_line,
            actual.cursor_col,
            actual.selection_anchor,
            actual.modified
        ),
        (
            expected.cursor_line,
            expected.cursor_col,
            expected.selection_anchor,
            expected.modified
        )
    );
    assert_eq!(actual.extra_cursors, expected.extra_cursors);
    assert_eq!(actual.extra_cursor_anchors, expected.extra_cursor_anchors);
    assert_eq!(actual.content_version, expected.content_version);
    assert_eq!(actual.undo_stack.len(), expected.undo_stack.len());
    assert_eq!(actual.redo_stack.len(), expected.redo_stack.len());
}

fn editor(source: &str, extension: &str, backwards: bool) -> Editor {
    let mut editor = Editor::test_with_text(source);
    editor.saved_rope = Some(editor.rope.clone());
    editor.file_path = Some(format!("source.{extension}").into());
    let end_line = editor.rope.len_lines() - 1;
    let end = (end_line, editor.line_len(end_line));
    (editor.cursor_line, editor.cursor_col) = if backwards { (0, 0) } else { end };
    editor.selection_anchor = Some(if backwards { end } else { (0, 0) });
    editor.extra_cursors = (0..editor.line_count())
        .step_by(3)
        .map(|line| (line, editor.line_len(line) / 2))
        .collect();
    editor.extra_cursor_anchors = editor
        .extra_cursors
        .iter()
        .map(|&(line, _)| Some((line, 0)))
        .collect();
    editor
}

#[test]
fn comment_edge_edits_match_original_commands_and_history() {
    for extension in ["rs", "html", "lua", "css"] {
        for backwards in [false, true] {
            for source in [
                "",
                " ",
                "\tα界\u{a0}",
                " /* x */ ",
                "/**/",
                "/*/",
                "<!-- -->",
                "<!---->",
                "<!--nested <!--x--> -->",
                "\t/*\r\n  α\r\n  β\n*/ ",
                "--[[ \u{2003}λ ]]",
                "/*\n\n*/",
                "a\r\nb\rc\n",
            ] {
                let mut actual = editor(source, extension, backwards);
                let mut expected = editor(source, extension, backwards);
                for _ in 0..2 {
                    actual.toggle_block_comment();
                    expected.reference_block_comment();
                    assert_same(&actual, &expected);
                    actual.undo();
                    expected.undo();
                    assert_same(&actual, &expected);
                    actual.redo();
                    expected.redo();
                    assert_same(&actual, &expected);
                }
            }
        }
    }
}

#[test]
fn paired_line_comments_preserve_unicode_whitespace_sparse_carets_and_crlf() {
    let sources = [
        " \t<div>α</div>\r\n \u{2003}<!-- β --> \t\r\n\r\nlast\n",
        "/*/\n/**/\n /* α */ \n\t/** nested /* β */ **/\r\nλ   ",
        " \t<!-- -->\n<!--\n-->\n\u{2003}<!---->\r\n \t\n",
    ];
    for extension in ["html", "css"] {
        for backwards in [false, true] {
            for source in sources {
                for sparse in [false, true] {
                    let mut actual = editor(source, extension, backwards);
                    let mut expected = editor(source, extension, backwards);
                    if sparse {
                        actual.selection_anchor = None;
                        expected.selection_anchor = None;
                    }
                    let (open, close) = actual.guess_line_comment_pair().unwrap();
                    for action in [0, 0, 1, 2, 2] {
                        match action {
                            0 => {
                                actual.toggle_line_comment();
                                expected.reference_paired_comments(
                                    open,
                                    close,
                                    PairedCommentAction::Toggle,
                                );
                            }
                            1 => {
                                actual.add_line_comment();
                                expected.reference_paired_comments(
                                    open,
                                    close,
                                    PairedCommentAction::Add,
                                );
                            }
                            _ => {
                                actual.remove_line_comment();
                                expected.reference_paired_comments(
                                    open,
                                    close,
                                    PairedCommentAction::Remove,
                                );
                            }
                        }
                        assert_same(&actual, &expected);
                        actual.undo();
                        expected.undo();
                        assert_same(&actual, &expected);
                        actual.redo();
                        expected.redo();
                        assert_same(&actual, &expected);
                    }
                }
            }
        }
    }
}

#[test]
fn large_block_comments_keep_body_chunks_shared() {
    let source = "let value = αβ界;\r\n".repeat(80_000);
    let mut actual = editor(&source, "rs", false);
    actual.extra_cursors.clear();
    actual.extra_cursor_anchors.clear();
    let original = actual.rope.clone();
    let middle = original.len_bytes() / 2;
    let old_chunk = original.chunk_at_byte(middle).0;
    actual.toggle_block_comment();
    assert_eq!(actual.rope.len_chars(), original.len_chars() + 6);
    assert_eq!(
        actual.rope.chunk_at_byte(middle + 3).0.as_ptr(),
        old_chunk.as_ptr()
    );
    actual.undo();
    assert!(!actual.modified);
    assert_eq!(actual.rope, original);
    actual.redo();
    actual.selection_anchor = Some((0, 0));
    actual.toggle_block_comment();
    assert_eq!(actual.rope.to_string(), source.trim());
    actual.undo();
    actual.redo();
    assert_eq!(actual.rope.to_string(), source.trim());
}

#[test]
#[ignore = "manual optimized performance measurement"]
fn benchmark_comment_edge_edits() {
    use std::{hint::black_box, time::Instant};
    for (name, source, paired) in [
        ("small block", "let value = αβ;".into(), false),
        ("large block", "let value = αβ界;\n".repeat(180_000), false),
        (
            "long markup lines",
            format!("  <p>{}</p>\r\n", "content αβ ".repeat(20_000)).repeat(16),
            true,
        ),
    ] {
        let rope = Rope::from_str(&source);
        let mut samples = [Vec::new(), Vec::new()];
        for round in 0..9 {
            for fast in [round % 2 == 0, round % 2 != 0] {
                let mut editor = Editor::new();
                editor.rope = rope.clone();
                editor.file_path = Some(if paired { "file.html" } else { "file.rs" }.into());
                editor.selection_anchor = Some((0, 0));
                editor.cursor_line = rope.len_lines() - 1;
                editor.cursor_col = editor.line_len(editor.cursor_line);
                let now = Instant::now();
                if paired {
                    if fast {
                        editor.toggle_line_comment();
                    } else {
                        editor.reference_paired_comments(
                            "<!--",
                            "-->",
                            PairedCommentAction::Toggle,
                        );
                    }
                } else if fast {
                    editor.toggle_block_comment();
                } else {
                    editor.reference_block_comment();
                }
                samples[usize::from(fast)].push(now.elapsed());
                black_box(&editor.rope);
            }
        }
        for values in &mut samples {
            values.sort();
        }
        eprintln!(
            "{name}: copied body={:?}, edge edits={:?}",
            samples[0][4], samples[1][4]
        );
    }
}

fn strip_comment_pair<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    text.strip_prefix(open)?.strip_suffix(close)
}
struct ReferenceLineEdit {
    line: usize,
    index: usize,
    start_col: usize,
    old_end_col: usize,
    kept_start: usize,
    kept_end: usize,
    added_prefix: usize,
    new_end_col: usize,
    text: String,
}

impl Editor {
    fn reference_paired_comments(&mut self, open: &str, close: &str, action: PairedCommentAction) {
        crate::editor_document::edit!(self, {
            let lines = self.line_prefix_edit_lines();
            let remove = match action {
                PairedCommentAction::Add => false,
                PairedCommentAction::Remove => true,
                PairedCommentAction::Toggle => lines.clone().all(|(_, _, line)| {
                    let text = line.to_string();
                    let trimmed = text.trim();
                    trimmed.is_empty() || strip_comment_pair(trimmed, open, close).is_some()
                }),
            };
            let mut edits = Vec::new();
            for (line_number, line_start, line) in lines {
                let line = line.to_string();
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let indent_bytes = line.len() - line.trim_start().len();
                let start_col = line[..indent_bytes].chars().count();
                let old_end_col = start_col + trimmed.chars().count();
                let (text, kept_start, kept_end, added_prefix) = if remove {
                    let Some(inner) = strip_comment_pair(trimmed, open, close) else {
                        continue;
                    };
                    let text = inner.trim();
                    let prefix_bytes = open.len() + inner.len() - inner.trim_start().len();
                    let kept_start = start_col + trimmed[..prefix_bytes].chars().count();
                    (
                        text.to_string(),
                        kept_start,
                        kept_start + text.chars().count(),
                        0,
                    )
                } else {
                    (
                        format!("{open} {trimmed} {close}"),
                        start_col,
                        old_end_col,
                        open.chars().count() + 1,
                    )
                };
                let new_end_col = start_col + text.chars().count();
                edits.push(ReferenceLineEdit {
                    line: line_number,
                    index: line_start + start_col,
                    start_col,
                    old_end_col,
                    kept_start,
                    kept_end,
                    added_prefix,
                    new_end_col,
                    text,
                });
            }
            if edits.is_empty() {
                return;
            }
            self.desired_col = None;
            self.save_undo();
            self.mark_syntax_dirty_range(edits[0].line, edits.last().unwrap().line);
            for edit in edits.iter().rev() {
                self.rope
                    .remove(edit.index..edit.index + edit.old_end_col - edit.start_col);
                self.rope.insert(edit.index, &edit.text);
            }
            // Each caret/anchor is mapped once, including carets in indentation,
            // delimiters and trailing whitespace. Unchanged rows remain untouched.
            let remap = |(line, col): &mut (usize, usize)| {
                if let Ok(index) = edits.binary_search_by_key(line, |edit| edit.line) {
                    let edit = &edits[index];
                    if *col > edit.old_end_col {
                        *col = edit.new_end_col + *col - edit.old_end_col;
                    } else if *col >= edit.start_col {
                        *col = edit.start_col
                            + edit.added_prefix
                            + (*col).clamp(edit.kept_start, edit.kept_end)
                            - edit.kept_start;
                    }
                }
            };
            let mut cursor = (self.cursor_line, self.cursor_col);
            remap(&mut cursor);
            (self.cursor_line, self.cursor_col) = cursor;
            if let Some(anchor) = self.selection_anchor.as_mut() {
                remap(anchor);
            }
            for cursor in &mut self.extra_cursors {
                remap(cursor);
            }
            for anchor in self.extra_cursor_anchors.iter_mut().flatten() {
                remap(anchor);
            }
            self.modified = true;
            self.ensure_visible();
        })
    }

    fn reference_block_comment(&mut self) {
        crate::editor_document::edit!(self, {
            self.desired_col = None;
            let Some((open, close)) = self.guess_block_comment() else {
                return;
            };

            // Compute the implicit whole-line range without changing the selection
            // captured by undo.
            let (start_idx, end_idx, sl) =
                if let Some((start, end, line, _)) = self.clamped_selection_indices() {
                    (start, end, line)
                } else {
                    let line = self.cursor_line.min(self.line_count() - 1);
                    let start = self.rope.line_to_char(line);
                    (start, start + self.line_len(line), line)
                };
            if start_idx == end_idx {
                return;
            }
            let text = self.rope.slice(start_idx..end_idx).to_string();

            self.save_undo();
            let end_line = self.rope.char_to_line(end_idx);
            // Capture secondary carets in the original document before a multiline
            // uncomment can remove their rows.
            let position = |(line, col): (usize, usize)| {
                let line = line.min(self.line_count() - 1);
                self.rope.line_to_char(line) + col.min(self.line_len(line))
            };
            let extra_positions: Vec<_> =
                self.extra_cursors.iter().copied().map(position).collect();
            let extra_anchors: Vec<_> = self
                .extra_cursor_anchors
                .iter()
                .map(|anchor| anchor.map(position))
                .collect();

            // Check if already wrapped in block comment
            let trimmed = text.trim();
            let (result, kept_start, kept_end, added_prefix) = if let Some(inner) =
                strip_comment_pair(trimmed, open, close)
            {
                let result = inner.trim();
                let prefix_bytes = text.len() - text.trim_start().len() + open.len() + inner.len()
                    - inner.trim_start().len();
                let kept_start = start_idx + text[..prefix_bytes].chars().count();
                let kept_end = kept_start + result.chars().count();
                (std::borrow::Cow::Borrowed(result), kept_start, kept_end, 0)
            } else {
                (
                    std::borrow::Cow::Owned(format!("{open} {text} {close}")),
                    start_idx,
                    end_idx,
                    open.chars().count() + 1,
                )
            };
            let new_end = start_idx + result.chars().count();
            self.rope.remove(start_idx..end_idx);
            self.rope.insert(start_idx, &result);
            let remap = |index: usize| {
                let index = if index < start_idx {
                    index
                } else if index > end_idx {
                    new_end + index - end_idx
                } else {
                    start_idx + added_prefix + index.clamp(kept_start, kept_end) - kept_start
                };
                let line = self.rope.char_to_line(index);
                (line, index - self.rope.line_to_char(line))
            };
            for (cursor, index) in self.extra_cursors.iter_mut().zip(extra_positions) {
                *cursor = remap(index);
            }
            for (anchor, index) in self.extra_cursor_anchors.iter_mut().zip(extra_anchors) {
                *anchor = index.map(remap);
            }
            self.cursor_line = self.rope.char_to_line(new_end);
            self.cursor_col = new_end - self.rope.line_to_char(self.cursor_line);
            self.mark_syntax_dirty_range(sl, end_line.max(self.cursor_line));

            self.modified = true;
            self.clear_selection();
            self.clear_stale_folds();
            self.ensure_visible();
        })
    }
}
