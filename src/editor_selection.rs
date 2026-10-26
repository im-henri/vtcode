use crate::editor::Editor;

#[path = "editor_paste_text.rs"]
mod prepared_paste;

/// Common LF clipboard content can be borrowed. When normalization is needed,
/// copy the payload once while handling CRLF pairs and lone carriage returns.
fn normalize_paste_line_endings(text: &str) -> std::borrow::Cow<'_, str> {
    let Some(first_cr) = text.find('\r') else {
        return std::borrow::Cow::Borrowed(text);
    };
    let mut normalized = String::with_capacity(text.len());
    normalized.push_str(&text[..first_cr]);
    for part in text[first_cr + 1..].split('\r') {
        normalized.push('\n');
        normalized.push_str(part.strip_prefix('\n').unwrap_or(part));
    }
    std::borrow::Cow::Owned(normalized)
}

impl Editor {
    /// Delete selected text and return it.
    pub(crate) fn delete_selection(&mut self) -> Option<String> {
        crate::editor_document::edit!(self, {
        self.desired_col = None;
        let Some(range) = self.clamped_selection_indices() else {
            self.clear_selection();
            return None;
        };
        self.save_undo();
        let text = self.rope.slice(range.0..range.1).to_string();
        self.remove_selection_range(range);
        self.ensure_visible();
        Some(text)
            })
    }

    /// Backspace, Delete and Tab discard the removed text. Keep the same undo
    /// and viewport behavior without materializing a potentially huge selection.
    pub(crate) fn discard_selection(&mut self) {
        crate::editor_document::edit!(self, {
        self.desired_col = None;
        let Some(range) = self.clamped_selection_indices() else {
            self.clear_selection();
            return;
        };
        self.save_undo();
        self.remove_selection_range(range);
        self.ensure_visible();
            })
    }

    /// Internal replacement operations already saved undo and do not need a
    /// copy of the deleted text. Count removed lines from rope metadata.
    pub(crate) fn delete_selection_for_replacement(&mut self) {
        crate::editor_document::edit!(self, {
        self.desired_col = None;
        if let Some(range) = self.clamped_selection_indices() {
            self.remove_selection_range(range);
        } else {
            self.clear_selection();
        }
            })
    }

    fn remove_selection_range(&mut self, (start, end, line, col): (usize, usize, usize, usize)) {
        crate::editor_document::edit!(self, {
        let deleted_lines = self.rope.char_to_line(end) - self.rope.char_to_line(start);
        self.rope.remove(start..end);
        if deleted_lines > 0 {
            self.shift_bookmarks(line + 1, -(deleted_lines as isize));
            self.clear_stale_folds();
        }
        self.cursor_line = line.min(self.line_count().saturating_sub(1));
        self.cursor_col = col.min(self.line_len(self.cursor_line));
        self.modified = true;
        self.clear_selection();
            })
    }

    /// Adjust a char index from the original rope for deletions that occurred before it.
    /// `sel_ranges` must contain original-rope (si, ei) positions (sorted descending by si).
    #[cfg(test)]
    pub(crate) fn adjust_char_idx_for_deletions(orig_idx: usize, sel_ranges: &[(usize, usize, usize, usize, usize)]) -> usize {
        let mut adjusted = orig_idx as isize;
        for &(si, ei, _, _, _) in sel_ranges {
            let deleted = (ei - si) as isize;
            if (ei as isize) <= adjusted {
                adjusted -= deleted;
            }
        }
        adjusted.max(0) as usize
    }

    /// Wrap the current selection with open/close characters
    pub(crate) fn wrap_selection(&mut self, open: char, close: char) {
        crate::editor_document::edit!(self, {
        self.desired_col = None;
        if let Some((start_idx, end_idx, sl, start_col)) = self.clamped_selection_indices() {
            self.save_undo();
            // Insert close char first (so start_idx stays valid)
            self.rope.insert_char(end_idx, close);
            self.rope.insert_char(start_idx, open);
            // Adjust selection to cover the wrapped text (excluding brackets)
            self.selection_anchor = Some((sl, start_col + 1));
            // Recalculate end position (open char added before end)
            let new_end_idx = end_idx + 1; // +1 for the open char we inserted
            let new_end_line = self.rope.char_to_line(new_end_idx);
            let new_end_col = new_end_idx - self.rope.line_to_char(new_end_line);
            self.cursor_line = new_end_line;
            self.cursor_col = new_end_col;
            self.clamp_cursor_col();
            self.modified = true;
            self.ensure_visible();
        }
            })
    }

    pub(crate) fn copy(&mut self) {
        self.copy_with(|text| crate::clipboard::set_text(text).is_ok());
    }

    fn copy_with(&mut self, write: impl FnOnce(&str) -> bool) {
        if let Some(view) = &self.markdown_preview_scroll {
            if let Some(text) = view.selected_text() {
                self.clipboard = text;
                self.clipboard_is_line = false;
                self.clipboard_failed = !write(&self.clipboard);
                crate::clipboard::remember_line_copy(&self.clipboard, self.clipboard_is_line);
            }
            return;
        }
        // Sort small ranges, then append shared Rope chunks directly to the
        // final clipboard. Keeping an owned String per selection would hold
        // another complete copy of a potentially large clipboard payload.
        if !self.extra_cursors.is_empty() {
            let mut all_selections = Vec::with_capacity(self.extra_cursors.len() + 1);
            for cursor in self.collect_multi_cursors() {
                if let Some(span) = self.selection_span_for_cursor(cursor) {
                    let line = self.rope.char_to_line(span.start_idx);
                    let col = span.start_idx - self.rope.line_to_char(line);
                    all_selections.push((line, col, span.start_idx..span.end_idx));
                } else {
                    let line = cursor.line.min(self.line_count() - 1);
                    // The join supplies the separator. Including each source
                    // line's terminator would add blank clipboard lines and
                    // prevent a subsequent multi-cursor paste distributing it.
                    let start = self.rope.line_to_char(line);
                    all_selections.push((line, 0, start..start + self.line_len(line)));
                }
            }
            // Sort by position (top to bottom)
            all_selections.sort_unstable_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
            all_selections.dedup();
            let bytes = all_selections.iter()
                .map(|(_, _, range)| self.rope.slice(range.clone()).len_bytes())
                .sum::<usize>() + all_selections.len().saturating_sub(1);
            let mut combined = String::with_capacity(bytes);
            for (index, (_, _, range)) in all_selections.iter().enumerate() {
                if index > 0 { combined.push('\n'); }
                for chunk in self.rope.slice(range.clone()).chunks() { combined.push_str(chunk); }
            }
            self.clipboard = combined;
            self.clipboard_is_line = false;
            self.clipboard_failed = !write(&self.clipboard);
            crate::clipboard::remember_line_copy(&self.clipboard, self.clipboard_is_line);
            return;
        }
        let (text, is_line) = if let Some(selected) = self.selected_text() {
            (selected, false)
        } else {
            // No selection: copy entire current line (VS Code behavior)
            let mut line_text = self.rope.line(self.cursor_line).to_string();
            if !line_text.ends_with(['\r', '\n']) {
                line_text.push_str(if self.cached_line_ending == "CRLF" { "\r\n" } else { "\n" });
            }
            (line_text, true)
        };
        self.clipboard = text;
        self.clipboard_is_line = is_line;
        self.clipboard_failed = !write(&self.clipboard);
        crate::clipboard::remember_line_copy(&self.clipboard, self.clipboard_is_line);
    }

    pub(crate) fn cut(&mut self) {
        crate::editor_document::edit!(self, {
        self.cut_with(|text| crate::clipboard::set_text(text).is_ok());
            })
    }

    fn cut_with(&mut self, write: impl FnOnce(&str) -> bool) {
        crate::editor_document::edit!(self, {
        // Multi-cursor cut: copy then delete from each cursor
        if !self.extra_cursors.is_empty() {
            self.copy_with(write);
            self.save_undo();
            // Collect all selection ranges (primary + extra) bottom-to-top for safe deletion
            let mut ranges: Vec<(usize, usize)> = Vec::new();
            for cursor in self.collect_multi_cursors() {
                if let Some(span) = self.selection_span_for_cursor(cursor) {
                    ranges.push((span.start_idx, span.end_idx));
                } else {
                    let line = cursor.line.min(self.line_count() - 1);
                    let mut start = self.rope.line_to_char(line);
                    let end = if line + 1 < self.rope.len_lines() {
                        self.rope.line_to_char(line + 1)
                    } else {
                        if line > 0 {
                            start = self.rope.line_to_char(line - 1) + self.line_len(line - 1);
                        }
                        self.rope.len_chars()
                    };
                    ranges.push((start, end));
                }
            }
            // Sort by start position descending to delete bottom-to-top
            ranges.sort_unstable_by(|a, b| b.0.cmp(&a.0));
            // Merge overlapping or adjacent ranges (sorted descending, a.start >= b.start)
            ranges.dedup_by(|b, a| {
                if b.1 >= a.0 {
                    // Ranges overlap or are adjacent: merge into a
                    a.0 = b.0;
                    a.1 = a.1.max(b.1);
                    true
                } else {
                    false
                }
            });
            let mut had_newlines = false;
            for (start, end) in &ranges {
                if *start < *end && *end <= self.rope.len_chars() {
                    let line = self.rope.char_to_line(*start);
                    let removed_lines = self.rope.char_to_line(*end) - line;
                    if removed_lines > 0 {
                        let from = if *start == self.rope.line_to_char(line) { line } else { line + 1 };
                        self.shift_bookmarks(from, -(removed_lines as isize));
                        had_newlines = true;
                    }
                    self.rope.remove(*start..*end);
                }
            }
            if had_newlines { self.clear_stale_folds(); }
            // Place cursor at the topmost deletion point
            if let Some(&(start, _)) = ranges.last() {
                let clamped = start.min(self.rope.len_chars());
                self.cursor_line = self.rope.char_to_line(clamped);
                self.cursor_col = clamped - self.rope.line_to_char(self.cursor_line);
                self.clamp_cursor_col();
            }
            self.extra_cursors.clear();
            self.extra_cursor_anchors.clear();
            self.clear_selection();
            self.modified = true;
            self.ensure_visible();
            return;
        }
        if self.has_selection() {
            if let Some(text) = self.delete_selection() {
                self.clipboard = text;
                self.clipboard_is_line = false;
                self.clipboard_failed = !write(&self.clipboard);
                crate::clipboard::remember_line_copy(&self.clipboard, self.clipboard_is_line);
            }
        } else {
            // No selection: cut entire line (VS Code behavior)
            self.copy_with(write);
            self.delete_line();
        }
            })
    }

    #[cfg(test)]
    pub(crate) fn paste(&mut self) {
        crate::editor_document::edit!(self, {
        self.paste_from_clipboard(crate::clipboard::get_text(), true);
            })
    }

    #[cfg(test)]
    fn paste_from_clipboard(&mut self, text: Result<String, arboard::Error>, smart: bool) {
        crate::editor_document::edit!(self, {
        // Empty or non-text clipboard contents are authoritative unless the
        // last copy/cut failed and its only surviving copy is internal.
        let text = match text {
            Ok(text) => { self.clipboard_failed = false; text }
            Err(arboard::Error::ContentNotAvailable) if !self.clipboard_failed => return,
            Err(_) => self.clipboard.clone(),
        };
        self.paste_text(&text, smart);
            })
    }

    /// Paste the given text directly, skipping system clipboard read.
    pub(crate) fn paste_with_text(&mut self, text: &str) {
        crate::editor_document::edit!(self, {
        self.paste_text(text, true);
            })
    }

    fn paste_text(&mut self, text: &str, smart: bool) {
        self.paste_snapshot_text(text, smart, None);
    }

    /// A worker uses the copy-kind decision captured on the UI thread, where
    /// clipboard ownership lives. It must not consult a worker's clipboard.
    pub(crate) fn paste_snapshot_text(&mut self, text: &str, smart: bool, line_copy: Option<bool>) {
        crate::editor_document::edit!(self, {
        if self.read_only || text.is_empty() {
            return;
        }
        self.desired_col = None;
        self.clamp_cursor();
        // LF clipboard content is already suitable for insertion. Avoid two
        // complete copies of the common case before constructing the rope.
        let line_paste = smart && !self.has_selection()
            && line_copy.or_else(|| crate::clipboard::line_copy_status(text)).unwrap_or_else(||
                self.clipboard_is_line && normalize_paste_line_endings(text) == normalize_paste_line_endings(&self.clipboard));
        if !self.extra_cursors.is_empty() {
            let text = normalize_paste_line_endings(text);
            self.multi_cursor_paste(&text);
            return;
        }
        self.save_undo();

        if let Some(range) = self.clamped_selection_indices() {
            self.remove_selection_range(range);
        } else {
            self.clear_selection();
        }

        if text.len() >= 4096 {
            let prefix = self.rope.line(self.cursor_line).slice(..self.cursor_col);
            let indent = (smart && !line_paste && text.contains(['\r', '\n'])
                && prefix.chunks().all(|chunk| chunk.bytes().all(|ch| matches!(ch, b' ' | b'\t'))))
                .then(|| prefix.to_string());
            let index = self.rope.line_to_char(self.cursor_line)
                + if line_paste { 0 } else { self.cursor_col };
            let (chars, lines) = if let Some(paste) = prepared_paste::prepare(text, indent.as_deref(), line_paste) {
                let counts = (paste.len_chars(), paste.len_lines() - 1);
                let tail = self.rope.split_off(index);
                self.rope.append(paste);
                self.rope.append(tail);
                counts
            } else {
                // No rewrite also proves LF normalization and indentation
                // analysis unnecessary. Insert the borrowed payload directly.
                let lines = if line_paste {
                    memchr::memchr_iter(b'\n', text.as_bytes()).count()
                } else {
                    usize::from(text.contains('\n'))
                };
                let before = self.rope.len_chars();
                self.rope.insert(index, text);
                (self.rope.len_chars() - before, lines)
            };
            if line_paste {
                self.shift_bookmarks(self.cursor_line, lines as isize);
                self.cursor_line += lines;
            } else {
                self.cursor_line = self.rope.char_to_line(index + chars);
                self.cursor_col = index + chars - self.rope.line_to_char(self.cursor_line);
            }
            self.modified = true;
            if lines > 0 { self.clear_stale_folds(); }
            self.ensure_visible();
            return;
        }
        let text = normalize_paste_line_endings(text);

        // Line-copy paste: insert above current line (VS Code behavior).
        if line_paste {
            let text = if text.ends_with('\n') { text } else {
                std::borrow::Cow::Owned(format!("{text}\n"))
            };
            let line_start = self.rope.line_to_char(self.cursor_line);
            let pasted_lines = text.bytes().filter(|&c| c == b'\n').count();
            self.rope.insert(line_start, &text);
            self.shift_bookmarks(self.cursor_line, pasted_lines as isize);
            self.cursor_line += pasted_lines;
            self.modified = true;
            self.clear_stale_folds();
            self.ensure_visible();
            return;
        }

        // Reindent only at a line's indentation, where a block's destination
        // is unambiguous. Include the first line in the common prefix: ignoring
        // it stripped the body indentation from e.g. "if ready:\n    run()".
        // Remove an exact space/tab prefix, never an arbitrary byte count.
        let prefix = self.rope.line(self.cursor_line).slice(..self.cursor_col);
        let paste_text = if smart && text.contains('\n')
            && prefix.chars().all(|ch| matches!(ch, ' ' | '\t'))
        {
            let mut common: Option<&str> = None;
            for line in text.split('\n').filter(|line| !line.trim().is_empty()) {
                let indent = &line[..line.len() - line.trim_start_matches([' ', '\t']).len()];
                common = Some(match common {
                    None => indent,
                    Some(previous) => {
                        let equal = previous.bytes().zip(indent.bytes()).take_while(|(a, b)| a == b).count();
                        &previous[..equal]
                    }
                });
                if common == Some("") { break; }
            }
            if common.is_none() || (common == Some("") && prefix.len_chars() == 0) {
                text
            } else {
                let strip = common.unwrap_or_default().len();
                let current_indent = prefix.to_string();
                let mut result = String::with_capacity(text.len());
                for (index, line) in text.split('\n').enumerate() {
                    if index > 0 { result.push('\n'); }
                    if line.trim().is_empty() {
                        result.push_str(line);
                    } else {
                        if index > 0 { result.push_str(&current_indent); }
                        result.push_str(&line[strip..]);
                    }
                }
                std::borrow::Cow::Owned(result)
            }
        } else {
            text
        };
        let idx = self.rope.line_to_char(self.cursor_line) + self.cursor_col;
        let before = self.rope.len_chars();
        self.rope.insert(idx, &paste_text);
        let new_idx = idx + (self.rope.len_chars() - before);
        self.cursor_line = self.rope.char_to_line(new_idx);
        self.cursor_col = new_idx - self.rope.line_to_char(self.cursor_line);
        self.modified = true;
        if paste_text.contains('\n') { self.clear_stale_folds(); }
        self.ensure_visible();
            })
    }

    /// Verify atomic plain replacement without changing the system clipboard
    /// or racing other tests. Runtime plain paste uses the App paste transaction.
    #[cfg(test)]
    pub(crate) fn paste_plain_with_text(&mut self, text: &str) {
        crate::editor_document::edit!(self, {
        self.paste_text(text, false);
            })
    }

    /// Insert char replacing selection if any
    pub(crate) fn insert_char_replacing_selection(&mut self, c: char) {
        crate::editor_document::edit!(self, {
        // If there's a selection and the typed char is an opening bracket/quote,
        // wrap the selection instead of replacing it
        if self.has_selection() {
            let wrap_close = match c {
                '(' => Some(')'),
                '[' => Some(']'),
                '{' => Some('}'),
                '"' => Some('"'),
                '\'' => Some('\''),
                '`' => Some('`'),
                _ => None,
            };
            if let Some(close) = wrap_close {
                self.wrap_selection(c, close);
                return;
            }
        }
        self.insert_char(c);
            })
    }

    /// Insert newline replacing selection if any
    pub(crate) fn insert_newline_replacing_selection(&mut self) {
        crate::editor_document::edit!(self, {
        self.insert_newline();
            })
    }

    /// Backspace replacing selection if any
    pub(crate) fn backspace_replacing_selection(&mut self) {
        crate::editor_document::edit!(self, {
        if self.has_selection() {
            self.discard_selection();
        } else {
            self.clear_selection();
            self.backspace();
        }
            })
    }

    /// Delete replacing selection if any
    pub(crate) fn delete_replacing_selection(&mut self) {
        crate::editor_document::edit!(self, {
        if self.has_selection() {
            self.discard_selection();
        } else {
            self.clear_selection();
            self.delete_char();
        }
            })
    }

    /// Tab replacing selection if any
    pub(crate) fn insert_tab_replacing_selection(&mut self) {
        crate::editor_document::edit!(self, {
        // If snippet mode is active, Tab advances to next tab stop
        if self.snippet_active && self.snippet_next_tab_stop() {
            return;
        }
        if self.has_selection() {
            self.insert_tab();
            return;
        }
        // Try snippet expansion first
        if self.try_expand_snippet() {
            return;
        }
        self.insert_tab();
            })
    }
}

#[cfg(test)]
mod tests {
    use crate::editor::Editor;

    #[test]
    fn large_streamed_pastes_preserve_replacement_line_copy_carets_and_undo() {
        let source = "head\n    oldtail\nend\n";
        let payload = "        café(界);\r\n        next();\r\n".repeat(200);
        let normalized = payload.replace("\r\n", "\n");
        let expected = format!("head\n    {}tail\nend\n", normalized
            .replacen("        ", "", 1).replace("\n        ", "\n    "));
        let mut editor = Editor::test_with_text(source);
        editor.cursor_line = 1;
        editor.cursor_col = 7;
        editor.selection_anchor = Some((1, 4));
        editor.paste_snapshot_text(&payload, true, Some(false));
        assert_eq!(editor.rope.to_string(), expected);
        assert_eq!((editor.cursor_line, editor.cursor_col), (401, 0));
        assert_eq!(editor.undo_stack.len(), 1);
        editor.undo();
        assert_eq!(editor.rope.to_string(), source);
        editor.redo();
        assert_eq!(editor.rope.to_string(), expected);

        let payload = payload.trim_end_matches(['\r', '\n']);
        let mut editor = Editor::test_with_text(source);
        editor.cursor_line = 1;
        editor.cursor_col = 6;
        editor.bookmarks.insert(1);
        editor.paste_snapshot_text(payload, true, Some(true));
        assert_eq!(editor.rope.to_string(), format!("head\n{}\n    oldtail\nend\n", payload.replace("\r\n", "\n")));
        assert_eq!((editor.cursor_line, editor.cursor_col), (401, 6));
        assert!(editor.bookmarks.contains(&401));
        editor.undo();
        assert_eq!(editor.rope.to_string(), source);
    }

    #[test]
    #[ignore = "manual optimized memory benchmark; run alone on one idle CPU"]
    fn benchmark_memory_multicursor_copy() {
        use std::{hint::black_box, time::Instant};
        let line = "a".repeat(128 * 1024);
        let text = format!("{line}\n").repeat(32);
        let mut editor = Editor::test_with_text(&text);
        editor.cursor_col = line.len();
        editor.selection_anchor = Some((0, 0));
        editor.extra_cursors = (1..32).map(|row| (row, line.len())).collect();
        editor.extra_cursor_anchors = (1..32).map(|row| Some((row, 0))).collect();
        let mut samples = Vec::new();
        for sample in 0..11 {
            editor.clipboard.clear();
            let started = Instant::now();
            editor.copy_with(|text| { black_box(text); true });
            let elapsed = started.elapsed();
            assert_eq!(editor.clipboard.len(), 32 * line.len() + 31);
            if sample > 1 { samples.push(elapsed); }
        }
        samples.sort();
        println!("4 MiB copy from 32 cursors: {:?}", samples[samples.len()/2]);
    }

    #[test]
    fn multicursor_copy_streams_overlapping_reversed_and_duplicate_unicode_ranges() {
        let mut editor = Editor::test_with_text("é🦀abc\r\n界xyz\nlast");
        editor.cursor_col = 1;
        editor.selection_anchor = Some((0, 4)); // reversed: 🦀ab
        editor.extra_cursors = vec![(1, 3), (0, 4), (0, 5), (2, 0)];
        editor.extra_cursor_anchors = vec![Some((1, 0)), Some((0, 1)), Some((0, 2)), None];
        editor.copy_with(|text| { assert_eq!(text, "🦀ab\nabc\n界xy\nlast"); true });
        assert_eq!(editor.clipboard, "🦀ab\nabc\n界xy\nlast");
        assert!(!editor.clipboard_is_line);
        assert_eq!(editor.rope, "é🦀abc\r\n界xyz\nlast");
        assert!(editor.undo_stack.is_empty());
    }

    #[test]
    fn copying_a_whole_line_pastes_above_the_caret_in_a_different_document() {
        let mut source = Editor::test_with_text("copied α\r\n");
        source.copy_with(|_| true);
        let mut target = Editor::test_with_text("target");
        target.cursor_col = 3;
        target.paste_from_clipboard(Ok(source.clipboard.clone()), true);
        assert_eq!(target.rope, "copied α\ntarget");
        assert_eq!((target.cursor_line, target.cursor_col), (1, 3));
        target.undo();
        assert_eq!(target.rope, "target");
        target.redo();
        assert_eq!(target.rope, "copied α\ntarget");
        // Copying a selection with identical bytes must clear line semantics.
        source.selection_anchor = Some((0, 0));
        (source.cursor_line, source.cursor_col) = (1, 0);
        source.copy_with(|_| true);
        target.undo();
        target.paste_from_clipboard(Ok(source.clipboard.clone()), true);
        assert_eq!(target.rope, "tarcopied α\nget");
    }

    #[test]
    fn empty_system_clipboard_does_not_paste_an_older_copy_or_delete_selection() {
        for smart in [false, true] {
            let mut editor = Editor::test_with_text("keep selected text");
            editor.clipboard = "old clipboard".into();
            editor.insert_char('x');
            editor.undo();
            editor.selection_anchor = Some((0, 5));
            editor.cursor_col = 13;
            editor.paste_from_clipboard(Ok(String::new()), smart);
            editor.paste_from_clipboard(Err(arboard::Error::ContentNotAvailable), smart);
            assert_eq!(editor.rope.to_string(), "keep selected text");
            assert_eq!(editor.selection_anchor, Some((0, 5)));
            assert!(editor.can_redo());
            editor.paste_from_clipboard(Err(arboard::Error::ClipboardOccupied), smart);
            assert_eq!(editor.rope.to_string(), "keep old clipboard text");
            editor.undo();
            assert_eq!(editor.rope.to_string(), "keep selected text");
        }
    }

    #[test]
    fn failed_cut_can_paste_its_internal_copy_when_the_os_has_no_text() {
        let mut editor = Editor::test_with_text("keep cut");
        editor.selection_anchor = Some((0, 5));
        editor.cursor_col = 8;
        editor.cut_with(|_| false);
        assert_eq!(editor.rope.to_string(), "keep ");
        editor.paste_from_clipboard(Err(arboard::Error::ContentNotAvailable), false);
        assert_eq!(editor.rope.to_string(), "keep cut");
        editor.undo();
        editor.undo();
        assert_eq!(editor.rope.to_string(), "keep cut");
        editor.clear_selection();
        editor.paste_from_clipboard(Ok("new".into()), false);
        let before = editor.rope.clone();
        editor.paste_from_clipboard(Err(arboard::Error::ContentNotAvailable), false);
        assert!(editor.rope.is_instance(&before));
    }

    #[test]
    fn cut_paste_typing_undo_redo_preserve_unicode_selections_and_internal_clipboard() {
        for available in [false, true] {
            for backwards in [false, true] {
                let original = "head αβ\r\nγδ tail";
                let mut editor = Editor::test_with_text(original);
                let (anchor, cursor) = if backwards { ((1, 2), (0, 5)) } else { ((0, 5), (1, 2)) };
                editor.selection_anchor = Some(anchor);
                (editor.cursor_line, editor.cursor_col) = cursor;
                editor.cut_with(|text| { assert_eq!(text, "αβ\r\nγδ"); available });
                assert_eq!(editor.rope.to_string(), "head  tail");
                assert_eq!(editor.clipboard_failed, !available);
                editor.paste_from_clipboard(Err(arboard::Error::ClipboardOccupied), false);
                assert_eq!(editor.rope.to_string(), "head αβ\nγδ tail");
                editor.insert_char('界');
                for expected in ["head αβ\nγδ tail", "head  tail", original] {
                    editor.undo();
                    assert_eq!(editor.rope.to_string(), expected);
                }
                assert_eq!(editor.selection_anchor, Some(anchor));
                assert_eq!((editor.cursor_line, editor.cursor_col), cursor);
                assert_eq!(editor.clipboard, "αβ\r\nγδ");
                assert!(!editor.can_undo());
                for expected in ["head  tail", "head αβ\nγδ tail", "head αβ\nγδ界 tail"] {
                    editor.redo();
                    assert_eq!(editor.rope.to_string(), expected);
                }
            }
        }
    }

    #[test]
    fn line_cut_has_exactly_one_undo_step_and_preserves_the_previous_edit() {
        let mut editor = Editor::test_with_text("head\ntail");
        editor.insert_char('x');
        editor.break_undo_group();
        editor.cursor_line = 1;
        editor.cursor_col = 2;
        editor.cut_with(|text| {
            assert_eq!(text, "tail\n");
            true
        });
        assert_eq!(editor.rope.to_string(), "xhead");
        assert_eq!(editor.undo_stack.len(), 2);
        editor.undo();
        assert_eq!(editor.rope.to_string(), "xhead\ntail");
        editor.undo();
        assert_eq!(editor.rope.to_string(), "head\ntail");
    }

    #[test]
    fn multi_cursor_line_copy_does_not_add_blank_lines_or_duplicate_a_line() {
        for ending in ["\n", "\r\n"] {
            let mut editor = Editor::test_with_text(&format!("α{ending}界{ending}last"));
            editor.cursor_line = 2;
            editor.extra_cursors = vec![(0, 0), (0, 1), (1, 1)];
            editor.extra_cursor_anchors = vec![None, None, Some((1, 1))];
            editor.copy_with(|text| {
                assert_eq!(text, "α\n界\nlast");
                true
            });
            assert!(!editor.clipboard_failed);
            let mut destination = Editor::test_with_text("\n\n");
            destination.extra_cursors = vec![(1, 0), (2, 0)];
            destination.paste_with_text(&editor.clipboard);
            assert_eq!(destination.rope.to_string(), "α\n界\nlast");
        }
    }

    #[test]
    fn copying_empty_last_line_keeps_a_pasteable_line_separator() {
        for ending in ["\n", "\r\n"] {
            let mut editor = Editor::test_with_text(&format!("head{ending}"));
            editor.detect_line_ending();
            editor.cursor_line = 1;
            editor.copy_with(|text| {
                assert_eq!(text, ending);
                false
            });
            assert!(editor.clipboard_failed);
            assert!(editor.clipboard_is_line);
            editor.paste_with_text(&editor.clipboard.clone());
            assert_eq!(editor.line_count(), 3);
            assert_eq!(editor.cursor_line, 2);
        }
    }

    #[test]
    fn multi_cursor_cut_at_eof_keeps_the_caret_after_the_retained_text() {
        let mut editor = Editor::test_with_text("prefix tail");
        editor.selection_anchor = Some((0, 7));
        editor.cursor_col = 9;
        editor.extra_cursors = vec![(0, 11)];
        editor.extra_cursor_anchors = vec![Some((0, 8))];
        editor.cut_with(|_| true);
        assert_eq!(editor.rope.to_string(), "prefix ");
        assert_eq!((editor.cursor_line, editor.cursor_col), (0, 7));
        editor.undo();
        assert_eq!(editor.rope.to_string(), "prefix tail");
        assert_eq!(editor.extra_cursors, [(0, 11)]);
    }

    #[test]
    fn multi_cursor_cut_treats_empty_anchors_as_line_carets_including_eof() {
        let before = "head\r\none\r\nkeep\r\ntail";
        let mut editor = Editor::test_with_text(before);
        editor.cursor_line = 1;
        editor.selection_anchor = Some((1, 0));
        editor.extra_cursors = vec![(3, 2)];
        editor.extra_cursor_anchors = vec![Some((3, 2))];
        editor.cut_with(|text| {
            assert_eq!(text, "one\ntail");
            true
        });
        assert_eq!(editor.rope.to_string(), "head\r\nkeep");
        editor.undo();
        assert_eq!(editor.rope.to_string(), before);
    }
    

    #[test]
    fn repeated_large_pastes_preserve_unicode_carets_and_undo() {
        let original = "abc\n".repeat(24);
        let payload = "é界value".repeat(1_024);
        let mut editor = Editor::test_with_text(&original);
        editor.cursor_col = 1;
        editor.extra_cursors = (1..24).map(|line| (line, 1)).collect();
        editor.extra_cursor_anchors = vec![None; 23];
        editor.paste_with_text(&payload);
        assert_eq!(
            editor.rope.to_string(),
            format!("a{payload}bc\n").repeat(24)
        );
        let column = 1 + payload.chars().count();
        assert_eq!((editor.cursor_line, editor.cursor_col), (0, column));
        assert_eq!(
            editor.extra_cursors,
            (1..24).map(|line| (line, column)).collect::<Vec<_>>()
        );
        assert_eq!(editor.undo_stack.len(), 1);
        editor.undo();
        assert_eq!(editor.rope.to_string(), original);
        assert_eq!(
            editor.extra_cursors,
            (1..24).map(|line| (line, 1)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn multiline_paste_fast_path_keeps_blank_lines_indent_and_character_offsets() {
        for plain in [false, true] {
            for payload in ["é界\nvalue\n", "é界\n \t\n", "é界\r\nvalue\r", "\n  \n\t\n"] {
                let mut editor = Editor::test_with_text("prefix suffix");
                editor.cursor_col = 6;
                if plain {
                    editor.paste_plain_with_text(payload);
                } else {
                    editor.paste_with_text(payload);
                }
                let normalized = payload.replace("\r\n", "\n").replace('\r', "\n");
                let expected = format!("prefix{normalized} suffix");
                assert_eq!(editor.rope.to_string(), expected);
                let offset = 6 + normalized.chars().count();
                let line = editor.rope.char_to_line(offset);
                assert_eq!(
                    (editor.cursor_line, editor.cursor_col),
                    (line, offset - editor.rope.line_to_char(line))
                );
                editor.undo();
                assert_eq!(editor.rope.to_string(), "prefix suffix");
            }
        }
    }

    #[test]
    #[ignore = "manual release benchmark of repeated large clipboard insertion"]
    fn benchmark_repeated_paste_shares_prepared_rope() {
        use std::{hint::black_box, time::Instant};
        let payload = "é界value\n".repeat(16_384);
        let source = ropey::Rope::from_str(&"original\n".repeat(24));
        let positions: Vec<_> = (0..24).map(|line| source.line_to_char(line) + 1).collect();
        let mut before = Vec::new();
        let mut after = Vec::new();
        for sample in 0..7 {
            for fast in [sample % 2 == 0, sample % 2 != 0] {
                let mut rope = source.clone();
                let start = Instant::now();
                if fast {
                    let insertion = ropey::Rope::from_str(&payload);
                    for &position in positions.iter().rev() {
                        let suffix = rope.split_off(position);
                        rope.append(insertion.clone());
                        rope.append(suffix);
                    }
                } else {
                    for &position in positions.iter().rev() {
                        rope.insert(position, &payload);
                    }
                }
                let elapsed = start.elapsed();
                assert_eq!(
                    black_box(rope.len_chars()),
                    source.len_chars() + payload.chars().count() * 24
                );
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
            "24 repeated large Unicode pastes: parsed per caret {:?}, shared {:?}",
            before[3], after[3]
        );
    }

    /// Helper: create an editor with the given text, cursor, and optional selection anchor.
    fn editor_with(text: &str, line: usize, col: usize, anchor: Option<(usize, usize)>) -> Editor {
        let mut ed = Editor::test_with_text(text);
        ed.cursor_line = line;
        ed.cursor_col = col;
        ed.selection_anchor = anchor;
        ed
    }

    // ── delete_selection ──────────────────────────────────────────────

    #[test]
    fn delete_selection_returns_selected_text() {
        let mut ed = editor_with("hello world", 0, 5, Some((0, 0)));
        let deleted = ed.delete_selection();
        assert_eq!(deleted.as_deref(), Some("hello"));
        assert_eq!(ed.rope.to_string(), " world");
        assert_eq!((ed.cursor_line, ed.cursor_col), (0, 0));
    }

    #[test]
    fn delete_selection_returns_none_without_anchor() {
        let mut ed = editor_with("hello", 0, 3, None);
        assert!(ed.delete_selection().is_none());
        assert_eq!(ed.rope.to_string(), "hello");
    }

    #[test]
    fn delete_selection_multiline() {
        let mut ed = editor_with("aaa\nbbb\nccc", 2, 1, Some((0, 2)));
        let deleted = ed.delete_selection();
        assert_eq!(deleted.as_deref(), Some("a\nbbb\nc"));
        assert_eq!(ed.rope.to_string(), "aacc");
        assert_eq!((ed.cursor_line, ed.cursor_col), (0, 2));
    }

    #[test]
    fn delete_selection_reverse_direction() {
        // Anchor after cursor — should still delete the range between them.
        let mut ed = editor_with("hello world", 0, 0, Some((0, 5)));
        let deleted = ed.delete_selection();
        assert_eq!(deleted.as_deref(), Some("hello"));
        assert_eq!(ed.rope.to_string(), " world");
    }

    #[test]
    fn delete_selection_marks_modified() {
        let mut ed = editor_with("abc", 0, 2, Some((0, 0)));
        ed.modified = false;
        ed.delete_selection();
        assert!(ed.modified);
    }

    #[test]
    fn delete_selection_clears_anchor() {
        let mut ed = editor_with("abc", 0, 3, Some((0, 0)));
        ed.delete_selection();
        assert!(ed.selection_anchor.is_none());
    }

    // ── wrap_selection ────────────────────────────────────────────────

    #[test]
    fn wrap_selection_with_parens() {
        let mut ed = editor_with("hello world", 0, 5, Some((0, 0)));
        ed.wrap_selection('(', ')');
        assert_eq!(ed.rope.to_string(), "(hello) world");
        // Selection anchor should move inside the open bracket
        assert_eq!(ed.selection_anchor, Some((0, 1)));
    }

    #[test]
    fn wrap_selection_with_quotes() {
        let mut ed = editor_with("abc", 0, 3, Some((0, 0)));
        ed.wrap_selection('"', '"');
        assert_eq!(ed.rope.to_string(), "\"abc\"");
    }

    // ── copy ──────────────────────────────────────────────────────────

    #[test]
    fn copy_selected_text_to_clipboard() {
        let mut ed = editor_with("hello world", 0, 5, Some((0, 0)));
        ed.copy();
        assert_eq!(ed.clipboard, "hello");
        assert!(!ed.clipboard_is_line);
        // Original text unchanged
        assert_eq!(ed.rope.to_string(), "hello world");
    }

    #[test]
    fn copy_whole_line_when_no_selection() {
        let mut ed = editor_with("first\nsecond\n", 1, 0, None);
        ed.copy();
        assert_eq!(ed.clipboard, "second\n");
        assert!(ed.clipboard_is_line);
    }

    // ── cut ───────────────────────────────────────────────────────────

    #[test]
    fn cut_selected_text() {
        let mut ed = editor_with("hello world", 0, 5, Some((0, 0)));
        ed.cut();
        assert_eq!(ed.clipboard, "hello");
        assert!(!ed.clipboard_is_line);
        assert_eq!(ed.rope.to_string(), " world");
    }

    #[test]
    fn cut_whole_line_when_no_selection() {
        let mut ed = editor_with("first\nsecond\nthird\n", 1, 0, None);
        ed.cut();
        assert_eq!(ed.clipboard, "second\n");
        assert!(ed.clipboard_is_line);
        assert_eq!(ed.rope.to_string(), "first\nthird\n");
    }

    // ── paste_with_text ───────────────────────────────────────────────

    #[test]
    fn paste_with_text_preserves_literal_terminal_packets() {
        // Edit: Paste reads the clipboard directly and reaches this path
        // without passing clipboard control codes through a terminal parser.
        let text = "literal \x1b[13;28;13;1;0;1_ \x1b[201~ \x10 text";
        let mut ed = editor_with("", 0, 0, None);
        ed.paste_with_text(text);
        assert_eq!(ed.rope.to_string(), text);
    }

    #[test]
    fn paste_with_text_inserts_at_cursor() {
        let mut ed = editor_with("ac", 0, 1, None);
        ed.paste_with_text("b");
        assert_eq!(ed.rope.to_string(), "abc");
        assert_eq!((ed.cursor_line, ed.cursor_col), (0, 2));
    }

    #[test]
    fn paste_with_text_replaces_selection() {
        let mut ed = editor_with("hello world", 0, 5, Some((0, 0)));
        ed.paste_with_text("goodbye");
        assert_eq!(ed.rope.to_string(), "goodbye world");
    }

    #[test]
    fn read_only_paste_preserves_selected_text_cursors_and_history() {
        for plain in [false, true] {
            let mut ed = editor_with("αβ\nsecond\nthird", 1, 3, Some((0, 1)));
            ed.extra_cursors = vec![(2, 2)];
            ed.extra_cursor_anchors = vec![Some((2, 0))];
            ed.read_only = true;
            let original = ed.rope.clone();
            if plain { ed.paste_plain_with_text("replacement\n"); }
            else { ed.paste_with_text("replacement\n"); }
            assert_eq!(ed.rope, original);
            assert_eq!((ed.cursor_line, ed.cursor_col), (1, 3));
            assert_eq!(ed.selection_anchor, Some((0, 1)));
            assert_eq!(ed.extra_cursors, [(2, 2)]);
            assert_eq!(ed.extra_cursor_anchors, [Some((2, 0))]);
            assert!(ed.undo_stack.is_empty() && ed.redo_stack.is_empty());
            assert!(!ed.modified);
        }
    }

    #[test]
    fn paste_with_text_empty_is_noop() {
        let mut ed = editor_with("abc", 0, 1, None);
        ed.paste_with_text("");
        assert_eq!(ed.rope.to_string(), "abc");
    }

    #[test]
    fn paste_with_text_multiline() {
        let mut ed = editor_with("ac", 0, 1, None);
        ed.paste_with_text("x\ny");
        // Should insert the text; cursor ends after pasted content
        assert!(ed.rope.to_string().contains("x\n"));
        assert_eq!(ed.cursor_line, 1);
    }

    #[test]
    fn paste_with_text_normalizes_crlf() {
        let mut ed = editor_with("ac", 0, 1, None);
        ed.paste_with_text("b\r\nd");
        // CRLF should be normalized to LF
        assert!(!ed.rope.to_string().contains('\r'));
        assert!(ed.rope.to_string().contains('\n'));
    }

    // ── insert_char_replacing_selection ────────────────────────────────

    #[test]
    fn insert_char_replacing_selection_replaces_text() {
        let mut ed = editor_with("hello world", 0, 5, Some((0, 0)));
        ed.insert_char_replacing_selection('X');
        assert_eq!(ed.rope.to_string(), "X world");
    }

    #[test]
    fn insert_char_replacing_selection_wraps_with_bracket() {
        // Typing '(' with a selection should wrap, not replace
        let mut ed = editor_with("hello", 0, 5, Some((0, 0)));
        ed.insert_char_replacing_selection('(');
        assert_eq!(ed.rope.to_string(), "(hello)");
    }

    #[test]
    fn insert_char_replacing_selection_wraps_with_curly() {
        let mut ed = editor_with("abc", 0, 3, Some((0, 0)));
        ed.insert_char_replacing_selection('{');
        assert_eq!(ed.rope.to_string(), "{abc}");
    }

    #[test]
    fn insert_char_replacing_selection_wraps_with_square() {
        let mut ed = editor_with("abc", 0, 3, Some((0, 0)));
        ed.insert_char_replacing_selection('[');
        assert_eq!(ed.rope.to_string(), "[abc]");
    }

    #[test]
    fn insert_char_without_selection_just_inserts() {
        let mut ed = editor_with("ac", 0, 1, None);
        ed.insert_char_replacing_selection('b');
        assert_eq!(ed.rope.to_string(), "abc");
    }

    // ── insert_newline_replacing_selection ─────────────────────────────

    #[test]
    fn insert_newline_replacing_selection_deletes_then_newlines() {
        let mut ed = editor_with("hello world", 0, 5, Some((0, 0)));
        ed.insert_newline_replacing_selection();
        // "hello" deleted, newline inserted — result starts with newline + " world"
        let text = ed.rope.to_string();
        assert!(text.contains('\n'));
        assert!(text.contains("world"));
        assert!(!text.contains("hello"));
    }

    // ── backspace_replacing_selection ──────────────────────────────────

    #[test]
    fn backspace_replacing_selection_deletes_selection() {
        let mut ed = editor_with("hello world", 0, 5, Some((0, 0)));
        ed.backspace_replacing_selection();
        assert_eq!(ed.rope.to_string(), " world");
    }

    #[test]
    fn backspace_replacing_selection_without_selection_does_backspace() {
        let mut ed = editor_with("abc", 0, 2, None);
        ed.backspace_replacing_selection();
        assert_eq!(ed.rope.to_string(), "ac");
    }

    // ── delete_replacing_selection ────────────────────────────────────

    #[test]
    fn delete_replacing_selection_deletes_selection() {
        let mut ed = editor_with("hello world", 0, 5, Some((0, 0)));
        ed.delete_replacing_selection();
        assert_eq!(ed.rope.to_string(), " world");
    }

    #[test]
    fn delete_replacing_selection_without_selection_deletes_forward() {
        let mut ed = editor_with("abc", 0, 1, None);
        ed.delete_replacing_selection();
        assert_eq!(ed.rope.to_string(), "ac");
    }

    // ── insert_tab_replacing_selection ─────────────────────────────────

    #[test]
    fn insert_tab_replacing_selection_deletes_selection_then_tabs() {
        let mut ed = editor_with("hello world", 0, 5, Some((0, 0)));
        ed.insert_tab_replacing_selection();
        let text = ed.rope.to_string();
        // Selection deleted, tab/spaces inserted
        assert!(!text.contains("hello"));
        assert!(text.contains("world"));
    }
}
