use crate::app::App;

pub(crate) const MAX_SEARCH_MATCHES: usize = 19_999;

#[cfg(test)]
mod search_performance_tests {
    /// Frozen old literal path for a selection in one line: materialize the
    /// full line, clip afterward, then convert byte positions incrementally.
    fn reference_selected_literal(rope: &ropey::Rope, query: &str, row: usize,
        first: usize, last: usize, whole_word: bool) -> Vec<(usize, usize, usize)>
    {
        let line = rope.line(row);
        let text = line.to_string();
        let clean = text.trim_end_matches(['\n', '\r']);
        let clean_chars = line.len_chars() - (text.len() - clean.len());
        let first = first.min(line.len_chars());
        let last = last.min(clean_chars);
        if first > last { return Vec::new(); }
        let begin = if first == 0 { 0 } else { line.char_to_byte(first) };
        let end = if last == clean_chars { clean.len() } else { line.char_to_byte(last) };
        let selected = &clean[begin..end];
        let finder = memchr::memmem::Finder::new(query.as_bytes());
        let query_chars = query.chars().count();
        let mut matches = Vec::new();
        let (mut start, mut prev_byte, mut prev_col) = (0, 0, first);
        while let Some(pos) = finder.find(&selected.as_bytes()[start..]) {
            let byte = start + pos;
            let end = byte + query.len();
            let col = prev_col + selected[prev_byte..byte].chars().count();
            prev_byte = end;
            prev_col = col + query_chars;
            if !whole_word || super::is_whole_word_match(selected, byte, end) {
                matches.push((row, col, col + query_chars));
            }
            start = end;
        }
        matches
    }

    #[test]
    fn clipped_literal_rope_slices_match_original_on_fragmented_lines() {
        let text = format!("{}xfoo café界foo\r\n{}tail", "界é ".repeat(2_000), "é".repeat(2_000));
        let rope = ropey::Rope::from_str(&text);
        let cancel = crate::background::CancellationToken::new();
        for row in 0..rope.len_lines() {
            let len = rope.line(row).len_chars();
            for first in [0, 1, 511, 1_023, len.saturating_sub(20), len, len + 99] {
                for last in [first, first + 1, first + 9, len, len + 100] {
                    for query in ["foo", "é", "界", "café", "tail", "\r"] {
                        for whole in [false, true] {
                            let expected = reference_selected_literal(&rope, query, row, first, last, whole);
                            let (actual, truncated, error) = super::scan_search_matches(&rope, query, true,
                                false, whole, Some((row, first, row, last)), &cancel);
                            assert_eq!(actual, expected, "row={row} range={first}..{last} query={query} whole={whole}");
                            assert!(!truncated && error.is_none());
                        }
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "manual comparative release benchmark"]
    fn benchmark_tiny_find_selection_in_long_line() {
        let prefix = "界é ".repeat(1_000_000);
        let first = prefix.chars().count();
        let rope = ropey::Rope::from_str(&format!("{prefix}needle café needle{}\r\n", "z".repeat(1_000_000)));
        let last = first + 18;
        let expected = reference_selected_literal(&rope, "needle", 0, first, last, false);
        let cancel = crate::background::CancellationToken::new();
        let mut samples = [Vec::new(), Vec::new()];
        for sample in 0..7 {
            for clipped in [sample % 2 == 0, sample % 2 != 0] {
                let started = std::time::Instant::now();
                let actual = if clipped {
                    let (matches, truncated, error) = super::scan_search_matches(&rope, "needle", true,
                        false, false, Some((0, first, 0, last)), &cancel);
                    assert!(!truncated && error.is_none());
                    matches
                } else { reference_selected_literal(&rope, "needle", 0, first, last, false) };
                samples[usize::from(clipped)].push(started.elapsed());
                assert_eq!(std::hint::black_box(actual), expected);
            }
        }
        samples.iter_mut().for_each(|samples| samples.sort());
        eprintln!("Find 18 selected characters in a seven-million-byte Unicode line: original={:?}, clipped={:?}",
            samples[0][3], samples[1][3]);
    }

    #[test]
    fn incremental_find_reuses_result_storage_and_keeps_exact_suffix_positions() {
        use crate::{
            app::{App, Tab},
            editor::Editor,
        };
        let mut app = App::test_new();
        app.tabs = vec![Tab::new_file(Editor::test_with_text(
            &"needle value\n".repeat(5_000),
        ))];
        app.search_active = true;
        app.search_input_focused = false;
        app.search_query = "needle".into();
        app.search_case_sensitive = true;
        app.update_search();
        assert_eq!(app.search_matches.len(), 5_000);
        let allocation = app.search_matches.as_ptr();
        for (line, ch) in [(2_500, 'x'), (2_500, '\n'), (4_000, 'y')] {
            app.editor_mut().unwrap().set_cursor(line, 8);
            let before = app.file_search_edit_snapshot().unwrap();
            app.editor_mut().unwrap().insert_char(ch);
            app.refresh_search_after_edit(&before);
            assert!(!app.file_search_refresh_pending);
            assert_eq!(app.search_matches.as_ptr(), allocation);
            let (expected, truncated, error) = super::scan_search_matches(
                &app.editor().unwrap().rope,
                "needle",
                true,
                false,
                false,
                None,
                &crate::background::CancellationToken::new(),
            );
            assert_eq!(app.search_matches, expected);
            assert!(!truncated && error.is_none());
        }
    }

    #[test]
    fn literal_selection_fast_path_preserves_clipped_word_boundaries_and_unicode_columns() {
        let rope = ropey::Rope::from_str("😀xfoo äfoo x\r\nfoo界foo\r\n");
        let cancel = crate::background::CancellationToken::new();
        for selection in [Some((0, 2, 0, 5)), Some((0, 1, 1, 7)), Some((0, 99, 0, 100)), Some((1, 0, 1, 3))] {
            for query in ["foo", "界", "äfoo", "x"] {
                for whole_word in [false, true] {
                    let expected = crate::search_regex::SearchRegex::editor(query, true, whole_word, false).unwrap().scan(&rope, selection, super::MAX_SEARCH_MATCHES, &cancel);
                    let (matches, truncated, error) = super::scan_search_matches(&rope, query, true, false, whole_word, selection, &cancel);
                    assert!(error.is_none());
                    assert_eq!((matches, truncated), expected, "{query:?} {selection:?} whole={whole_word}");
                }
            }
        }
    }

    #[test]
    #[ignore = "manual comparative release benchmark"]
    fn benchmark_literal_find_in_selection() {
        let source = format!("{}needle", "界 needle ".repeat(10_000));
        let rope = ropey::Rope::from_str(&source);
        let selection = Some((0, 2, 0, rope.len_chars()));
        let cancel = crate::background::CancellationToken::new();
        let regex = crate::search_regex::SearchRegex::editor("needle", true, false, false).unwrap();
        let expected = regex.scan(&rope, selection, super::MAX_SEARCH_MATCHES, &cancel);
        let mut before = Vec::new(); let mut after = Vec::new();
        for sample in 0..7 {
            for literal in [sample % 2 == 0, sample % 2 != 0] {
                let started = std::time::Instant::now();
                let results = if literal {
                    let (matches, truncated, error) = super::scan_search_matches(&rope, "needle", true, false, false, selection, &cancel);
                    assert!(error.is_none()); (matches, truncated)
                } else { regex.scan(&rope, selection, super::MAX_SEARCH_MATCHES, &cancel) };
                let elapsed = started.elapsed();
                assert_eq!(std::hint::black_box(results), expected);
                if literal { after.push(elapsed); } else { before.push(elapsed); }
            }
        }
        before.sort(); after.sort();
        eprintln!("Find in selection / 10,000 Unicode matches (regex compilation excluded): regex={:?} literal={:?}", before[3], after[3]);
    }
    #[test]
    fn selection_on_dense_line_does_not_exhaust_capacity_before_selection() {
        let rope = ropey::Rope::from_str(&format!("{}hit", "hit ".repeat(11_000)));
        let cancel = crate::background::CancellationToken::new();
        for regex in [false, true] {
            let (matches, truncated, error) = super::scan_search_matches(
                &rope,
                "hit",
                true,
                regex,
                false,
                Some((0, 44_000, 0, 44_003)),
                &cancel,
            );
            assert_eq!(matches, vec![(0, 44_000, 44_003)]);
            assert!(!truncated);
            assert!(error.is_none());
        }
    }

    #[test]
    fn literal_search_uses_nonoverlapping_ranges_across_unicode_rope_chunks() {
        let prefix = "é".repeat(2_000);
        let rope = ropey::Rope::from_str(&format!("{prefix}ababa\r\nÉ needle NEEDLE\n"));
        let cancel = crate::background::CancellationToken::new();
        let scan = |query, case, whole| {
            super::scan_search_matches(&rope, query, case, false, whole, None, &cancel).0
        };
        assert_eq!(
            scan("aba", true, false),
            vec![(0, 2000, 2003)]
        );
        assert_eq!(scan("needle", false, true), vec![(1, 2, 8), (1, 9, 15)]);
        assert_eq!(scan("é", false, false).last(), Some(&(1, 0, 1)));
        cancel.cancel();
        assert!(scan("aba", true, false).is_empty());
    }

    #[test]
    #[ignore = "manual release benchmark"]
    fn benchmark_search_in_file() {
        let cancel = crate::background::CancellationToken::new();
        for (name, text, query, case_sensitive, use_regex, expected) in [
            (
                "absent-sensitive",
                "ordinary source code without a match\n".repeat(100_000),
                "needle",
                true,
                false,
                0,
            ),
            (
                "absent-insensitive",
                "Ordinary Source Code Without A Match\n".repeat(100_000),
                "needle",
                false,
                false,
                0,
            ),
            (
                "regex",
                "ordinary source code without a match\n".repeat(100_000),
                "n[e]+dle",
                true,
                true,
                0,
            ),
            (
                "dense",
                "needle ".repeat(8_000),
                "needle",
                true,
                false,
                8_000,
            ),
        ] {
            let rope = ropey::Rope::from_str(&text);
            let mut samples = Vec::new();
            for _ in 0..7 {
                let started = std::time::Instant::now();
                let (matches, truncated, error) = super::scan_search_matches(
                    std::hint::black_box(&rope),
                    query,
                    case_sensitive,
                    use_regex,
                    false,
                    None,
                    &cancel,
                );
                samples.push(started.elapsed());
                assert_eq!(matches.len(), expected);
                assert!(!truncated);
                assert!(error.is_none());
                std::hint::black_box(matches);
            }
            samples.sort();
            eprintln!("search_in_file {name} median={:?}", samples[3]);
        }
    }
}
const MAX_INCREMENTAL_SEARCH_LINES: usize = 256;
const MAX_INCREMENTAL_SEARCH_BYTES: usize = 256 * 1024;
pub(crate) const FILE_SEARCH_EDIT_QUIET_PERIOD: std::time::Duration =
    std::time::Duration::from_millis(75);

pub(crate) struct FileSearchEditSnapshot {
    pub(crate) target: Option<crate::background::EditorAnalysisTarget>,
    pub(crate) content_version: u64,
    pub(crate) line_count: usize,
    pub(crate) first_affected_line: usize,
    pub(crate) last_affected_line: usize,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct FileSearchKey {
    target: crate::background::EditorAnalysisTarget,
    query: String,
    case_sensitive: bool,
    use_regex: bool,
    whole_word: bool,
    selection: Option<(usize, usize, usize, usize)>,
}

#[cfg(test)]
pub(crate) fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

pub(crate) fn is_whole_word_match(haystack: &str, start: usize, end: usize) -> bool {
    crate::search_regex::is_whole_word_match(haystack, start, end)
}

#[cfg(test)]
pub(crate) use crate::search_replace::preserve_case;

/// Apply regex replacement with capture group references ($1, $2, etc.)
#[cfg(test)]
pub(crate) fn regex_replace_with_groups(
    pattern: &str,
    matched_text: &str,
    replacement: &str,
    case_sensitive: bool,
) -> String {
    let Ok(regex) = crate::search_regex::SearchRegex::editor(pattern, case_sensitive, false, true) else { return matched_text.to_owned(); };
    regex.captures_at(matched_text, 0).map_or_else(|| matched_text.to_owned(), |captures|
        crate::search_replace::Replacement::new(replacement, true).expand(matched_text, &captures, false))
}

pub(crate) fn scan_search_matches(
    rope: &ropey::Rope,
    query: &str,
    case_sensitive: bool,
    use_regex: bool,
    whole_word: bool,
    selection: Option<(usize, usize, usize, usize)>,
    cancel: &crate::background::CancellationToken,
) -> (Vec<(usize, usize, usize)>, bool, Option<String>) {
    scan_search_matches_in_lines(
        rope,
        query,
        case_sensitive,
        use_regex,
        whole_word,
        selection,
        0,
        rope.len_lines(),
        cancel,
    )
}

#[allow(clippy::too_many_arguments)]
fn scan_search_matches_in_lines(
    rope: &ropey::Rope,
    query: &str,
    case_sensitive: bool,
    use_regex: bool,
    whole_word: bool,
    selection: Option<(usize, usize, usize, usize)>,
    start_line: usize,
    end_line: usize,
    cancel: &crate::background::CancellationToken,
) -> (Vec<(usize, usize, usize)>, bool, Option<String>) {
    let mut matches = Vec::new();
    let mut truncated = false;
    if query.is_empty() || cancel.is_cancelled() {
        return (matches, truncated, None);
    }

    let total_lines = rope.len_lines();
    let mut end_line = end_line.min(total_lines);
    let mut start_line = start_line.min(end_line);
    if let Some((sl, _, el, _)) = selection {
        start_line = start_line.max(sl).min(end_line);
        end_line = end_line.min(el.saturating_add(1)).max(start_line);
    }
    let mut line_buf = String::new();
    if use_regex || !case_sensitive || query.contains('\n') {
        return match crate::search_regex::SearchRegex::cached_editor(query, case_sensitive, whole_word, use_regex) {
            Ok(regex) => {
                let (matches, truncated) = regex.scan_lines(rope, selection, start_line, end_line, MAX_SEARCH_MATCHES, cancel);
                (matches, truncated, None)
            }
            Err(error) => (Vec::new(), false, Some(error)),
        };
    } else {
        let search_query_len = query.len();
        let search_query_chars = query.chars().count();
        let finder = memchr::memmem::Finder::new(query.as_bytes());
        for (i, line) in (start_line..end_line).zip(rope.lines_at(start_line)) {
            if cancel.is_cancelled() {
                return (Vec::new(), false, None);
            }
            // Clip in Rope coordinates before making fragmented text contiguous.
            // A tiny selection in a multi-megabyte logical line should only
            // copy its selected bytes, rather than the entire hidden prefix.
            let clean_chars = if selection.is_some() {
                line.len_chars() - line.chars_at(line.len_chars()).reversed()
                    .take_while(|&ch| matches!(ch, '\n' | '\r')).count()
            } else { line.len_chars() };
            let (base_col, end_col) = selection.map_or((0, clean_chars), |(sl, sc, el, ec)| {
                (if i == sl { sc.min(line.len_chars()) } else { 0 },
                 if i == el { ec.min(clean_chars) } else { clean_chars })
            });
            if base_col > end_col { continue; }
            let selected = if base_col == 0 && end_col == line.len_chars() {
                line
            } else { line.slice(base_col..end_col) };
            let search_text = if let Some(text) = selected.as_str() {
                text
            } else {
                line_buf.clear();
                for chunk in selected.chunks() {
                    if cancel.is_cancelled() {
                        return (Vec::new(), false, None);
                    }
                    line_buf.push_str(chunk);
                }
                &line_buf
            };
            let search_text = if selection.is_none() {
                search_text.trim_end_matches(['\n', '\r'])
            } else { search_text };
            let mut start_byte = 0;
            let mut prev_byte = 0;
            let mut prev_col = base_col;
            while let Some(pos) = finder.find(&search_text.as_bytes()[start_byte..]) {
                if cancel.is_cancelled() {
                    return (Vec::new(), false, None);
                }
                let col_byte = start_byte + pos;
                let end_byte = col_byte + search_query_len;
                let col = prev_col + search_text[prev_byte..col_byte].chars().count();
                prev_byte = end_byte;
                prev_col = col + search_query_chars;
                if !whole_word || is_whole_word_match(search_text, col_byte, end_byte) {
                    if matches.len() == MAX_SEARCH_MATCHES {
                        truncated = true;
                        break;
                    }
                    matches.push((i, col, col + search_query_chars));
                }
                start_byte = end_byte;
            }
            if truncated { break; }
        }
    }

    (matches, truncated, None)
}

impl App {
    /// Undo snapshots record the actual edit, which may be far from the
    /// current caret after navigating Find results.
    pub(crate) fn file_search_history_snapshot(
        &self,
        redo: bool,
    ) -> Option<FileSearchEditSnapshot> {
        let mut snapshot = self.file_search_edit_snapshot()?;
        let editor = self.editor()?;
        let state = if redo {
            editor.redo_stack.back()
        } else {
            editor.undo_stack.back()
        }?;
        let delta = snapshot.line_count.abs_diff(state.rope.len_lines());
        snapshot.first_affected_line = state.dirty_from.saturating_sub(1);
        snapshot.last_affected_line = state
            .dirty_through
            .max(state.dirty_from.saturating_add(delta))
            .saturating_add(1)
            .min(snapshot.line_count - 1);
        Some(snapshot)
    }

    pub(crate) fn file_search_edit_snapshot(&self) -> Option<FileSearchEditSnapshot> {
        let track_find = self.search_active && !self.search_query.is_empty();
        // The editor identity/version is useful to every post-edit hook, not
        // only Find. Keeping it here also lets palette-driven undo/redo perform
        // the same LSP/cache handoff as direct typing.
        let target = self.editor_content_target();
        let editor = self.editor()?;
        let line_count = editor.rope.len_lines().max(1);
        let (first_affected_line, last_affected_line) = if track_find {
            (
                editor.earliest_edit_line().saturating_sub(1),
                editor
                    .latest_edit_line()
                    .saturating_add(1)
                    .min(line_count - 1),
            )
        } else {
            (0, 0)
        };
        Some(FileSearchEditSnapshot {
            target,
            content_version: editor.content_version,
            line_count,
            // The neighboring lines cover Backspace/Delete joining lines.
            first_affected_line,
            last_affected_line,
        })
    }

    pub(crate) fn refresh_search_after_edit(&mut self, previous: &FileSearchEditSnapshot) {
        self.last_edit_time = std::time::Instant::now();
        self.update_search_after_editor_edit(
            previous.target.as_ref(),
            previous.line_count,
            previous.first_affected_line,
            previous.last_affected_line,
        );
    }

    /// Complete the non-visual invalidation shared by every editor mutation.
    /// Display caches intentionally remain populated (stale-while-revalidate)
    /// so undo, paste, transforms and ordinary typing cannot blank decorations
    /// for an intermediate frame.
    pub(crate) fn refresh_document_feedback_after_edit(
        &mut self,
        previous: &FileSearchEditSnapshot,
    ) -> bool {
        let Some(before) = previous.target.as_ref() else {
            return false;
        };
        let Some(after) = self.editor_content_target() else {
            return false;
        };
        if !before.same_editor(&after) || previous.content_version == after.content_version {
            return false;
        }

        self.pin_preview_tab();
        self.last_edit_time = std::time::Instant::now();
        self.cancel_lsp_semantic_decode();
        self.lsp_semantic_tokens_last_version = u64::MAX;
        self.lsp_inlay_hint_last_version = u64::MAX;
        self.lsp_on_type_format_target = None;
        if let Some(path) = after.file_path {
            self.lsp_manager.invalidate_document_feedback(&path);
        }
        true
    }

    fn cancel_file_search_request(&mut self) {
        if let Some(token) = self.file_search_cancel_token.take() {
            token.cancel();
        }
        self.file_search_generation = self.file_search_generation.wrapping_add(1);
        self.bg_file_search_running = false;
    }

    fn reset_file_search_results(&mut self) {
        self.file_search_refresh_pending = false;
        self.search_matches.clear();
        self.search_current = 0;
        self.search_truncated = false;
        self.search_error = None;
        self.file_search_matches_target = None;
        self.file_search_key = None;
    }

    pub(crate) fn clear_file_search_results(&mut self) {
        self.cancel_file_search_request();
        self.reset_file_search_results();
    }

    pub(crate) fn apply_file_search_matches(
        &mut self,
        target: crate::background::EditorAnalysisTarget,
        matches: Vec<(usize, usize, usize)>,
        truncated: bool,
        error: Option<String>,
    ) {
        let previous_match = self.search_matches.get(self.search_current).copied();
        self.search_matches = matches;
        self.finish_file_search_matches(target, previous_match, truncated, error);
    }

    fn finish_file_search_matches(
        &mut self,
        target: crate::background::EditorAnalysisTarget,
        previous_match: Option<(usize, usize, usize)>,
        truncated: bool,
        error: Option<String>,
    ) {
        let fallback_position = self.editor().map_or((0, 0), |editor| {
            editor.selection_range().map_or(
                (editor.cursor_line, editor.cursor_col),
                |(line, col, _, _)| (line, col),
            )
        });

        self.search_truncated = truncated;
        self.file_search_matches_target = Some(target);
        self.search_current = if self.search_matches.is_empty() {
            0
        } else if let Some(previous) = previous_match {
            self.search_matches
                .binary_search(&previous)
                .unwrap_or_else(|index| index.min(self.search_matches.len() - 1))
        } else {
            let index = self
                .search_matches
                .partition_point(|&(line, col, _)| (line, col) < fallback_position);
            if index < self.search_matches.len() {
                index
            } else {
                0
            }
        };
        self.search_error = error.as_deref().map(crate::search_regex::error_summary);
        if let Some(error) = error {
            self.status_message = format!("Invalid regex: {error}");
        }
        if self.search_active && self.search_input_focused && !self.search_focus_replace {
            self.jump_to_current_match();
        }
    }

    fn defer_file_search_refresh(&mut self) {
        self.cancel_file_search_request();
        self.file_search_key = None;
        self.file_search_refresh_pending = self.search_active && !self.search_query.is_empty();
    }

    pub(crate) fn defer_search_after_unbounded_edit(&mut self) {
        self.last_edit_time = std::time::Instant::now();
        self.defer_file_search_refresh();
    }

    /// Keep Find exact after ordinary edits by rescanning only the bounded line
    /// range that can have changed. Large or ambiguous edits retain the current
    /// highlights and schedule one full worker scan after typing settles.
    pub(crate) fn update_search_after_editor_edit(
        &mut self,
        previous_target: Option<&crate::background::EditorAnalysisTarget>,
        old_line_count: usize,
        old_start_line: usize,
        old_end_line: usize,
    ) {
        if !self.search_active || self.search_query.is_empty() {
            self.file_search_refresh_pending = false;
            return;
        }
        let Some(previous_target) = previous_target else {
            self.defer_file_search_refresh();
            return;
        };
        if self.bg_file_search_running
            || self.file_search_refresh_pending
            || self.search_truncated
            || self.search_in_selection
            // Regex compilation and execution have pattern-dependent cost;
            // keep that work off the UI thread even for a small edit.
            || self.search_regex
            || self.search_query.contains('\n')
            || self.file_search_matches_target.as_ref() != Some(previous_target)
        {
            self.defer_file_search_refresh();
            return;
        }

        let Some(target) = self.editor_content_target() else {
            self.defer_file_search_refresh();
            return;
        };
        let Some(rope) = self.editor().map(|editor| editor.rope.clone()) else {
            self.defer_file_search_refresh();
            return;
        };
        let new_line_count = rope.len_lines();
        let old_line_count = old_line_count.max(1);
        let old_start_line = old_start_line.min(old_line_count - 1);
        let old_end_line = old_end_line.max(old_start_line).min(old_line_count - 1);
        let old_suffix_start = old_end_line.saturating_add(1);
        let new_suffix_start = if new_line_count >= old_line_count {
            old_suffix_start.saturating_add(new_line_count - old_line_count)
        } else {
            old_suffix_start.saturating_sub(old_line_count - new_line_count)
        }
        .min(new_line_count);
        let new_start_line = old_start_line.min(new_line_count.saturating_sub(1));
        let new_end_line = new_suffix_start.max(new_start_line.saturating_add(1));
        let affected_lines = new_end_line.saturating_sub(new_start_line);
        if affected_lines > MAX_INCREMENTAL_SEARCH_LINES {
            self.defer_file_search_refresh();
            return;
        }
        let affected_bytes = rope.line_to_byte(new_end_line) - rope.line_to_byte(new_start_line);
        if affected_bytes > MAX_INCREMENTAL_SEARCH_BYTES {
            self.defer_file_search_refresh();
            return;
        }

        self.cancel_file_search_request();
        self.file_search_refresh_pending = false;
        let cancel = crate::background::CancellationToken::new();
        let (changed_matches, range_truncated, error) = scan_search_matches_in_lines(
            &rope,
            &self.search_query,
            self.search_case_sensitive,
            self.search_regex,
            self.search_whole_word,
            None,
            new_start_line,
            new_end_line,
            &cancel,
        );
        if error.is_some() {
            self.apply_file_search_matches(target, Vec::new(), false, error);
            return;
        }

        let line_growth = new_line_count.saturating_sub(old_line_count);
        let line_shrink = old_line_count.saturating_sub(new_line_count);
        let previous_match = self.search_matches.get(self.search_current).copied();
        let first = self
            .search_matches
            .partition_point(|&(line, _, _)| line < old_start_line);
        let last = self
            .search_matches
            .partition_point(|&(line, _, _)| line <= old_end_line);
        let suffix = first + changed_matches.len();
        // An ordinary character edit usually replaces the same number of
        // matches on one row. Splice in place instead of copying every result
        // into another allocation; unchanged suffix rows need no walk at all.
        drop(self.search_matches.splice(first..last, changed_matches));
        if line_growth != 0 || line_shrink != 0 {
            for (line, _, _) in &mut self.search_matches[suffix..] {
                *line = if line_growth > 0 {
                    line.saturating_add(line_growth)
                } else {
                    line.saturating_sub(line_shrink)
                };
            }
        }
        let truncated = range_truncated || self.search_matches.len() > MAX_SEARCH_MATCHES;
        self.search_matches.truncate(MAX_SEARCH_MATCHES);
        if let Some(key) = self.file_search_key.as_mut() {
            key.target = target.clone();
        }
        self.finish_file_search_matches(target, previous_match, truncated, None);
    }

    pub(crate) fn refresh_pending_file_search(&mut self) -> bool {
        if !self.file_search_refresh_pending
            || self.last_edit_time.elapsed() < FILE_SEARCH_EDIT_QUIET_PERIOD
        {
            return false;
        }
        self.file_search_refresh_pending = false;
        if self.search_active && !self.search_query.is_empty() {
            self.update_search();
            return true;
        }
        false
    }

    pub(crate) fn update_search(&mut self) {
        let _ = self.editor_mut();
        if self.status_message.starts_with("Invalid regex:") { self.status_message.clear(); }
        if self.search_query.is_empty() {
            self.clear_file_search_results();
            return;
        }

        let Some(target) = self.editor_content_target() else {
            self.cancel_file_search_request();
            self.reset_file_search_results();
            return;
        };
        // FindModel.research expands a multiline scope to complete lines,
        // excluding the final line when the selection ends at its start.
        if let Some((start, _, end, end_col)) = self.search_selection_range.filter(|(start, _, end, _)| self.search_in_selection && start != end) {
            if let Some(editor) = self.editor() {
                let end = if end_col == 0 { end.saturating_sub(1) } else { end }.min(editor.rope.len_lines() - 1);
                self.search_selection_range = Some((start.min(end), 0, end, editor.line_len(end)));
            }
        }
        let key = FileSearchKey {
            target: target.clone(),
            query: self.search_query.clone(),
            case_sensitive: self.search_case_sensitive,
            use_regex: self.search_regex,
            whole_word: self.search_whole_word,
            selection: self
                .search_in_selection
                .then_some(self.search_selection_range)
                .flatten(),
        };
        // Reuse completed results and identical work already in flight. Find
        // focus, cursor movement, and repeated commands do not change the query.
        if self.file_search_key.as_ref() == Some(&key) {
            return;
        }
        self.cancel_file_search_request();
        self.file_search_refresh_pending = false;
        self.search_error = None;
        if self.file_search_key.as_ref().is_some_and(|previous|
            previous.query != key.query || previous.case_sensitive != key.case_sensitive
                || previous.use_regex != key.use_regex || previous.whole_word != key.whole_word
                || previous.selection != key.selection)
        {
            self.search_matches.clear();
            self.search_current = 0;
        }
        self.file_search_key = Some(key);
        let Some(rope) = self.editor().map(|editor| editor.rope.clone()) else {
            return;
        };
        #[cfg(not(test))]
        let generation = self.file_search_generation;
        let query = self.search_query.clone();
        let case_sensitive = self.search_case_sensitive;
        let use_regex = self.search_regex;
        let whole_word = self.search_whole_word;
        let selection = self
            .search_in_selection
            .then_some(self.search_selection_range)
            .flatten();

        #[cfg(test)]
        {
            let cancel = crate::background::CancellationToken::new();
            let (matches, truncated, error) = scan_search_matches(
                &rope,
                &query,
                case_sensitive,
                use_regex,
                whole_word,
                selection,
                &cancel,
            );
            self.apply_file_search_matches(target, matches, truncated, error);
        }

        #[cfg(not(test))]
        {
            self.bg_file_search_running = true;
            let sender = self.background.sender.clone();
            let activity = self.background.activity.start("Searching", target.file_path.as_deref());
            let token = self.background.submit_interactive(move |cancel| {
                let _activity = activity.cancellable(cancel.clone()).running();
                let (matches, truncated, error) = scan_search_matches(
                    &rope,
                    &query,
                    case_sensitive,
                    use_regex,
                    whole_word,
                    selection,
                    &cancel,
                );
                if cancel.is_cancelled() {
                    return;
                }
                let _ = sender.send(crate::background::BackgroundResult::SearchMatches {
                    generation,
                    target,
                    matches,
                    truncated,
                    error,
                });
            });
            self.file_search_cancel_token = Some(token);
        }
    }
}
