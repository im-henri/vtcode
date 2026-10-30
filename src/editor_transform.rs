use crate::editor::{sort_json_value, split_into_words, Editor};
use base64::{engine::general_purpose, Engine as _};
use ropey::Rope;
#[cfg(test)]
use std::fs;
use std::io;

#[cfg(test)]
#[path = "editor_folding_tests.rs"]
mod folding_tests;

const IMPORT_PREFIXES: [&str; 7] = [
    "use ",
    "import ",
    "from ",
    "#include ",
    "require(",
    "require ",
    "@import ",
];

#[derive(Clone, Copy)]
enum LineTransform {
    Reverse,
    Shuffle,
    RemoveDuplicates,
    RemoveBlank,
}

#[derive(Default)]
pub(crate) struct FoldHintCache {
    version: u64,
    preprocessor: bool,
    source: Option<Rope>,
    dirty: Option<(usize, usize)>,
    hints: rustc_hash::FxHashMap<usize, (bool, usize)>,
    // Hints that skip blank lines or directives need a dependency index. Adjacent
    // hints can be invalidated directly without visiting cached offscreen rows.
    skipped_spans: std::collections::BTreeMap<usize, usize>,
    #[cfg(test)]
    pub(crate) builds: usize,
    #[cfg(test)]
    pub(crate) invalidation_visits: usize,
}

#[cfg(test)]
thread_local! {
    pub(crate) static REFERENCE_FOLD_INVALIDATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl FoldHintCache {
    pub(crate) fn clear(&mut self) {
        self.hints.clear();
        self.skipped_spans.clear();
        self.source = None;
        self.dirty = None;
    }

    fn remove_hint(&mut self, line: usize) {
        #[cfg(test)]
        {
            self.invalidation_visits += 1;
        }
        if self.hints.remove(&line)
            .is_some_and(|(_, through)| through > line.saturating_add(1))
        {
            self.skipped_spans.remove(&line);
        }
    }

    pub(crate) fn mark_dirty(&mut self, start: usize, end: usize) {
        self.dirty = Some(self.dirty.map_or((start, end.max(start)), |(from, through)| {
            (from.min(start), through.max(end))
        }));
    }

    fn synchronize(&mut self, rope: &Rope, version: u64, preprocessor: bool) {
        if self.preprocessor != preprocessor {
            self.clear();
            self.preprocessor = preprocessor;
        }
        if self.version == version
            && self.source.as_ref().is_some_and(|old| old.is_instance(rope))
            && self.dirty.is_none()
        {
            return;
        }
        if let Some((start, end)) = self.dirty.take()
            && let Some(old) = &self.source
        {
            let shifted = old.len_lines() != rope.len_lines();
            #[cfg(test)]
            let local = !REFERENCE_FOLD_INVALIDATION.get();
            #[cfg(not(test))]
            let local = true;
            // An indentation hint depends on its own line and the next source
            // line after blank lines/directives. Edits can change that dependency.
            if local && !shifted && end.saturating_sub(start) <= 64 {
                // Source-line dependency spans do not overlap: skipped rows
                // have no such hints. At most one older span crosses this edit.
                let crossing = self.skipped_spans.range(..start).next_back()
                    .and_then(|(&line, &through)| (through >= start).then_some(line));
                if let Some(line) = crossing {
                    self.remove_hint(line);
                }
                if let Some(line) = start.checked_sub(1)
                    && self.hints.get(&line).is_some_and(|&(_, through)| through >= start)
                {
                    self.remove_hint(line);
                }
                for line in start..=end {
                    self.remove_hint(line);
                }
            } else {
                #[cfg(test)]
                {
                    self.invalidation_visits += self.hints.len();
                }
                self.hints.retain(|&line, &mut (_, through)| {
                    through < start || (!shifted && line > end)
                });
                self.skipped_spans.retain(|&line, &mut through| {
                    through < start || (!shifted && line > end)
                });
            }
        } else {
            self.hints.clear();
            self.skipped_spans.clear();
        }
        self.source = Some(rope.clone());
        self.version = version;
    }
}

fn chars_start_with(mut chars: impl Iterator<Item = char>, prefix: &str) -> bool {
    prefix
        .chars()
        .all(|expected| chars.next() == Some(expected))
}

fn is_import_line(line: ropey::RopeSlice<'_>) -> bool {
    if let Some(text) = line.as_str() {
        let trimmed = text.trim_start();
        return IMPORT_PREFIXES.iter().any(|prefix| trimmed.starts_with(prefix));
    }
    let mut chars = line.chars().peekable();
    while chars.peek().is_some_and(|ch| ch.is_whitespace()) { chars.next(); }
    IMPORT_PREFIXES.iter().any(|prefix| chars_start_with(chars.clone(), prefix))
}

/// Region detection only needs a short prefix after indentation/comment
/// whitespace. Return true for a start marker and false for an end marker.
fn rope_line_region_marker(line: ropey::RopeSlice<'_>) -> Option<bool> {
    let mut chars = line.chars().skip_while(|c| c.is_whitespace());
    match chars.next() {
        Some('#') => {
            if chars_start_with(chars.clone(), "pragma region") { return Some(true); }
            if chars_start_with(chars.clone(), "pragma endregion") { return Some(false); }
            let chars = chars.skip_while(|c| c.is_whitespace());
            return if chars_start_with(chars.clone(), "region") { Some(true) }
                else if chars_start_with(chars, "endregion") { Some(false) }
                else { None };
        }
        Some('/') if chars.next() == Some('/') => {}
        Some('-') if chars.next() == Some('-') => {}
        Some('<')
            if chars.next() == Some('!')
                && chars.next() == Some('-')
                && chars.next() == Some('-') => {}
        _ => return None,
    }
    let mut chars = chars.skip_while(|c| c.is_whitespace()).peekable();
    if chars.peek() == Some(&'#') {
        chars.next();
    }
    if chars_start_with(chars.clone(), "region") { Some(true) }
    else if chars_start_with(chars, "endregion") { Some(false) }
    else { None }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PreprocessorDirective {
    If,
    Branch,
    End,
    Other,
}

fn preprocessor_directive(line: ropey::RopeSlice<'_>) -> Option<PreprocessorDirective> {
    let mut chars = line.chars().skip_while(|c| c.is_whitespace());
    if chars.next() != Some('#') { return None; }
    let word = chars.skip_while(|c| c.is_whitespace())
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_');
    Some(if ["if", "ifdef", "ifndef"].iter().any(|name| word.clone().eq(name.chars())) {
        PreprocessorDirective::If
    } else if ["else", "elif", "elifdef", "elifndef"].iter().any(|name| word.clone().eq(name.chars())) {
        PreprocessorDirective::Branch
    } else if word.eq("endif".chars()) {
        PreprocessorDirective::End
    } else {
        PreprocessorDirective::Other
    })
}

fn preprocessor_continues(line: ropey::RopeSlice<'_>) -> bool {
    line.chars_at(line.len_chars()).reversed().find(|c| !c.is_whitespace()) == Some('\\')
}

fn opens_brace(line: ropey::RopeSlice<'_>) -> bool {
    line.chars().find(|c| !c.is_whitespace()) == Some('{')
}

/// Rebuild only chunks with changed separators, sharing the untouched ranges
/// with the source and its undo snapshots. Adjacent changed chunks use a single
/// builder, avoiding a tree mutation and cache invalidation for every newline.
pub(crate) fn converted_line_endings(source: &Rope, crlf: bool, convert_lone_cr: bool) -> Option<(Rope, usize, usize)> {
    let mut result = Rope::new();
    let mut builder: Option<ropey::RopeBuilder> = None;
    let mut scratch = String::new();
    let mut offset = 0;
    let mut shared_from = 0;
    let mut first_change = None;
    let mut last_change = 0;
    for chunk in source.chunks() {
        let mut copied = 0;
        scratch.clear();
        // Rope chunks never split a CRLF pair, and both ASCII bytes are UTF-8
        // boundaries. No character indexing or line lookup is needed here.
        let bytes = chunk.as_bytes();
        let mut after_separator = 0;
        for newline in memchr::memchr2_iter(b'\r', b'\n', bytes) {
            if newline < after_separator { continue; }
            let pair = bytes[newline] == b'\r' && bytes.get(newline + 1) == Some(&b'\n');
            after_separator = newline + if pair { 2 } else { 1 };
            if (pair && crlf) || (!crlf && bytes[newline] == b'\n')
                || (!convert_lone_cr && bytes[newline] == b'\r' && !pair) { continue; }
            first_change.get_or_insert(offset + newline);
            last_change = offset + newline;
            scratch.push_str(&chunk[copied..newline]);
            scratch.push_str(if crlf { "\r\n" } else { "\n" });
            copied = after_separator;
        }
        if copied != 0 {
            if builder.is_none() {
                if shared_from < offset {
                    result.append(Rope::from(source.byte_slice(shared_from..offset)));
                }
                builder = Some(ropey::RopeBuilder::new());
            }
            scratch.push_str(&chunk[copied..]);
            builder.as_mut().unwrap().append(&scratch);
            shared_from = offset + chunk.len();
        } else if let Some(changed) = builder.take() {
            result.append(changed.finish());
        }
        offset += chunk.len();
    }
    let first_change = first_change?;
    if let Some(changed) = builder { result.append(changed.finish()); }
    if shared_from < source.len_bytes() {
        result.append(Rope::from(source.byte_slice(shared_from..)));
    }
    Some((result, source.byte_to_line(first_change), source.byte_to_line(last_change)))
}

impl Editor {
    pub(crate) fn sort_imports(&mut self) {
        crate::editor_document::edit!(self, {
        use crate::editor_line_ops::split_line_text;
        // Read the original snapshot sequentially. Sorting preserves each
        // block's character/line totals, so later source offsets stay valid.
        let source = self.rope.clone();
        let mut rows = source.lines().enumerate().peekable();
        let mut changed = false;
        while let Some((block_start, first)) = rows.next() {
            if !is_import_line(first)
                || !rows.peek().is_some_and(|(_, line)| is_import_line(*line))
            { continue; }
            let (first, ending) = split_line_text(first);
            let mut lines = vec![first];
            let mut endings = vec![ending];
            while rows.peek().is_some_and(|(_, line)| is_import_line(*line)) {
                let (_, line) = rows.next().unwrap();
                let (body, ending) = split_line_text(line);
                lines.push(body);
                endings.push(ending);
            }
            if lines.is_sorted() { continue; }
            let block_end = block_start + lines.len();
            lines.sort_unstable();
            let from = source.line_to_char(block_start);
            let to = if block_end < source.len_lines() { source.line_to_char(block_end) }
                else { source.len_chars() };
            let mut sorted = String::with_capacity(source.char_to_byte(to) - source.char_to_byte(from));
            // Separators remain in their original slots, including a missing
            // final newline. Only changed import blocks need owned text.
            for (line, ending) in lines.iter().zip(endings) {
                sorted.push_str(line);
                sorted.push_str(ending);
            }
            if !changed { self.save_undo(); }
            self.rope.remove(from..to);
            self.rope.insert(from, &sorted);
            self.mark_syntax_dirty_range(block_start, block_end - 1);
            changed = true;
        }
        if changed {
            self.modified = true;
            self.clear_stale_folds();
            // The number of rows is unchanged, but a shorter import can now
            // occupy any cursor or selection endpoint's row.
            self.clamp_cursor();
            if let Some((line, col)) = self.selection_anchor {
                self.selection_anchor = Some((line, col.min(self.line_len(line))));
            }
            for index in 0..self.extra_cursors.len() {
                let (line, col) = self.extra_cursors[index];
                self.extra_cursors[index].1 = col.min(self.line_len(line));
            }
            for index in 0..self.extra_cursor_anchors.len() {
                if let Some((line, col)) = self.extra_cursor_anchors[index] {
                    self.extra_cursor_anchors[index] = Some((line, col.min(self.line_len(line))));
                }
            }
        }
            })
    }

    pub(crate) fn convert_line_endings_to_lf(&mut self) {
        crate::editor_document::edit!(self, {
        self.convert_line_endings(false);
            })
    }

    pub(crate) fn convert_line_endings_to_crlf(&mut self) {
        crate::editor_document::edit!(self, {
        self.convert_line_endings(true);
            })
    }

    fn convert_line_endings(&mut self, crlf: bool) {
        crate::editor_document::edit!(self, {
        // Keep sparse-separator files on the bounded line-lookup path.
        if self.rope.len_lines() <= 64 || self.rope.len_bytes() / self.rope.len_lines() > 4096 {
            self.convert_line_endings_individually(crlf);
        } else if let Some((converted, first, last)) = converted_line_endings(&self.rope, crlf, true) {
            self.save_undo();
            self.rope = converted;
            self.mark_syntax_dirty_range(first, last);
            self.modified = true;
        }
        // Saving must use the selected convention immediately, including when
        // conversion is followed by Save before another frame is rendered.
        self.cached_line_ending = if crlf { "CRLF" } else { "LF" };
            })
    }

    fn convert_line_endings_individually(&mut self, crlf: bool) {
        crate::editor_document::edit!(self, {
        let mut changes = Vec::new();
        let ending = if crlf { "\r\n" } else { "\n" };
        for index in (0..self.rope.len_lines()).rev() {
            let line = self.rope.line(index);
            let count = line.len_chars();
            let Some(last) = count.checked_sub(1).map(|last| line.char(last)) else { continue; };
            if !matches!(last, '\r' | '\n') { continue; }
            let pair = last == '\n' && count > 1 && line.char(count - 2) == '\r';
            if (crlf && pair) || (!crlf && last == '\n' && !pair) { continue; }
            let end = self.rope.line_to_char(index) + count;
            let start = end - if pair { 2 } else { 1 };
            changes.push((index, start, end));
        }
        if changes.is_empty() { return; }
        self.save_undo();
        // Capture original offsets before replacing anything: adjacent CR and
        // LF temporarily form CRLF while a conversion is in progress.
        for (index, start, end) in changes {
            self.rope.remove(start..end);
            self.rope.insert(start, ending);
            self.mark_syntax_dirty_range(index, index);
        }
        self.modified = true;
            })
    }

    /// Toggle case: lowercase → UPPERCASE → Title Case → lowercase
    pub(crate) fn toggle_case(&mut self) {
        crate::editor_document::edit!(self, {
        if let Some(text) = self.selected_text() {
            let lower = text.to_lowercase();
            let upper = text.to_uppercase();
            let transformed = if text == lower {
                upper
            } else if text == upper {
                // Title case: capitalize first letter of each word, preserve whitespace
                let mut result = String::with_capacity(text.len());
                let mut capitalize_next = true;
                for ch in text.chars() {
                    if ch.is_whitespace() {
                        result.push(ch);
                        capitalize_next = true;
                    } else if capitalize_next {
                        for uc in ch.to_uppercase() {
                            result.push(uc);
                        }
                        capitalize_next = false;
                    } else {
                        for lc in ch.to_lowercase() {
                            result.push(lc);
                        }
                    }
                }
                result
            } else {
                lower
            };
            self.transform_selection(|_| transformed.clone());
        }
            })
    }

    pub(crate) fn convert_to_camel_case(&mut self) {
        crate::editor_document::edit!(self, {
        self.transform_selection(|text| {
            let words = split_into_words(text);
            let mut out = String::new();
            for (i, w) in words.iter().enumerate() {
                if i == 0 {
                    for ch in w.chars() {
                        for lc in ch.to_lowercase() {
                            out.push(lc);
                        }
                    }
                } else {
                    let mut chars = w.chars();
                    if let Some(c) = chars.next() {
                        for uc in c.to_uppercase() {
                            out.push(uc);
                        }
                        for ch in chars {
                            for lc in ch.to_lowercase() {
                                out.push(lc);
                            }
                        }
                    }
                }
            }
            out
        });
            })
    }

    pub(crate) fn convert_to_pascal_case(&mut self) {
        crate::editor_document::edit!(self, {
        self.transform_selection(|text| {
            let words = split_into_words(text);
            let mut out = String::new();
            for w in &words {
                let mut chars = w.chars();
                if let Some(c) = chars.next() {
                    for uc in c.to_uppercase() {
                        out.push(uc);
                    }
                    for ch in chars {
                        for lc in ch.to_lowercase() {
                            out.push(lc);
                        }
                    }
                }
            }
            out
        });
            })
    }

    pub(crate) fn convert_to_snake_case(&mut self) {
        crate::editor_document::edit!(self, {
        self.transform_selection(|text| {
            let words = split_into_words(text);
            let mut out = String::new();
            for (i, w) in words.iter().enumerate() {
                if i > 0 {
                    out.push('_');
                }
                for ch in w.chars() {
                    for lc in ch.to_lowercase() {
                        out.push(lc);
                    }
                }
            }
            out
        });
            })
    }

    pub(crate) fn convert_to_constant_case(&mut self) {
        crate::editor_document::edit!(self, {
        self.transform_selection(|text| {
            let words = split_into_words(text);
            let mut out = String::new();
            for (i, w) in words.iter().enumerate() {
                if i > 0 {
                    out.push('_');
                }
                for ch in w.chars() {
                    for uc in ch.to_uppercase() {
                        out.push(uc);
                    }
                }
            }
            out
        });
            })
    }

    pub(crate) fn convert_to_kebab_case(&mut self) {
        crate::editor_document::edit!(self, {
        self.transform_selection(|text| {
            let words = split_into_words(text);
            let mut out = String::new();
            for (i, w) in words.iter().enumerate() {
                if i > 0 {
                    out.push('-');
                }
                for ch in w.chars() {
                    for lc in ch.to_lowercase() {
                        out.push(lc);
                    }
                }
            }
            out
        });
            })
    }

    /// Convert selected text to Title Case (capitalize first letter of each word)
    pub(crate) fn convert_to_title_case(&mut self) {
        crate::editor_document::edit!(self, {
        self.transform_selection(|text| {
            let words = split_into_words(text);
            let mut out = String::new();
            for (i, w) in words.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                let mut chars = w.chars();
                if let Some(c) = chars.next() {
                    for uc in c.to_uppercase() {
                        out.push(uc);
                    }
                    for ch in chars {
                        for lc in ch.to_lowercase() {
                            out.push(lc);
                        }
                    }
                }
            }
            out
        });
            })
    }

    /// Transform selected text with a function
    pub(crate) fn transform_selection(&mut self, f: impl Fn(&str) -> String) {
        crate::editor_document::edit!(self, {
        self.desired_col = None;
        // If no selection, select the current word first (VS Code behavior)
        if !self.has_selection() {
            self.select_word_at_cursor();
        }
        if let Some(text) = self.selected_text() {
            let transformed = f(&text);
            self.replace_transformed_selection(&transformed, true);
        }
            })
    }

    /// Use the same clamped range for reading and replacing a selection.
    /// Virtual columns are view coordinates, never offsets into the next line.
    fn replace_transformed_selection(&mut self, text: &str, keep_selection: bool) {
        let Some((start, end, first_line, _)) = self.clamped_selection_indices() else { return; };
        if self.rope.slice(start..end) == text { return; }
        let last_line = self.rope.char_to_line(end);
        let inserted = text.chars().count();
        let offset = |(line, col): (usize, usize)| {
            let line = line.min(self.line_count() - 1);
            let offset = self.rope.line_to_char(line) + col.min(self.line_len(line));
            if offset < start { offset }
            else if offset >= end { start + inserted + (offset - end) }
            else { start + (offset - start).min(inserted) }
        };
        let cursors: Vec<_> = self.extra_cursors.iter().copied().map(offset).collect();
        let anchors: Vec<_> = self.extra_cursor_anchors.iter().map(|anchor| anchor.map(offset)).collect();
        self.save_undo_for_range(first_line, last_line);
        self.snippet_cancel();
        self.desired_col = None;
        self.rope.remove(start..end);
        self.rope.insert(start, text);
        let rope = &self.rope;
        let position = |offset| {
            let line = rope.char_to_line(offset);
            (line, (offset - rope.line_to_char(line)).min(Self::line_content_len(rope.line(line))))
        };
        let cursor = position(start + inserted);
        let anchor = keep_selection.then(|| position(start));
        for (cursor, offset) in self.extra_cursors.iter_mut().zip(cursors) { *cursor = position(offset); }
        for (anchor, offset) in self.extra_cursor_anchors.iter_mut().zip(anchors) { *anchor = offset.map(position); }
        (self.cursor_line, self.cursor_col) = cursor;
        self.selection_anchor = anchor;
        self.mark_syntax_dirty_range(first_line, last_line.max(self.cursor_line));
        self.clear_stale_folds();
        self.modified = true;
        self.refresh_modified_from_saved();
        self.ensure_visible();
    }

    /// Trim trailing whitespace from all lines
    pub(crate) fn trim_trailing_whitespace(&mut self) -> bool {
        crate::editor_document::edit!(self, {
        self.desired_col = None;
        let total = self.line_count();
        let mut any_trimmed = false;
        for i in (0..total).rev() {
            let line = self.rope.line(i);
            let len = line.len_chars();
            // Only the suffix can change. Walk backwards across rope chunks
            // rather than copying and replacing every complete source line.
            let mut tail = line.chars_at(len).reversed().peekable();
            let mut content_end = len;
            while matches!(tail.peek(), Some('\n' | '\r')) {
                tail.next();
                content_end -= 1;
            }
            let trailing = tail.take_while(|c| c.is_whitespace()).count();
            // Only modify if there was actual trailing whitespace (not just line endings)
            if trailing == 0 {
                continue;
            }
            let trimmed_len = content_end - trailing;
            let from = self.rope.line_to_char(i) + trimmed_len;
            if !any_trimmed { self.save_undo(); }
            self.rope.remove(from..from + trailing);
            self.mark_syntax_dirty_range(i, i);
            self.modified = true;
            any_trimmed = true;
        }
        if any_trimmed {
            self.set_multi_cursor_states(self.collect_multi_cursors());
        }
        any_trimmed
            })
    }

    /// Check if a line is hidden by a fold (i.e., it's inside a folded range but not the fold start)
    pub(crate) fn is_line_folded(&self, line: usize) -> bool {
        self.folded_range_containing(line).is_some()
    }

    /// Return the nearest folded interval containing `line`. Navigation uses
    /// its boundary to jump across even million-line folds in one tree lookup.
    pub(crate) fn folded_range_containing(&self, line: usize) -> Option<(usize, usize)> {
        self.folded_ranges
            .range(..line)
            .next_back()
            .and_then(|(&start, &end)| (line < end).then_some((start, end)))
    }

    /// Check if a line is the start of a fold
    pub(crate) fn is_fold_start(&self, line: usize) -> bool {
        self.folded_ranges.contains_key(&line)
    }

    /// Check if a line is a fold region start marker (e.g. #region, //region, etc.)
    #[cfg(test)]
    fn is_region_start(trimmed: &str) -> bool {
        // C/C++/C# style: #region, #pragma region
        if trimmed.starts_with("#region") || trimmed.starts_with("#pragma region") {
            return true;
        }
        // JS/TS/Java/Go style: //region, // region, //#region, // #region
        if let Some(rest) = trimmed.strip_prefix("//") {
            let rest = rest.trim_start();
            if rest.starts_with("region") || rest.starts_with("#region") {
                return true;
            }
        }
        // Python/Ruby style: # region
        if let Some(rest) = trimmed.strip_prefix('#') {
            let rest = rest.trim_start();
            if rest.starts_with("region") {
                return true;
            }
        }
        // Lua/SQL style: --region, -- region
        if let Some(rest) = trimmed.strip_prefix("--") {
            let rest = rest.trim_start();
            if rest.starts_with("region") || rest.starts_with("#region") {
                return true;
            }
        }
        // HTML/XML style: <!-- region or <!-- #region
        if let Some(rest) = trimmed.strip_prefix("<!--") {
            let rest = rest.trim_start();
            if rest.starts_with("region") || rest.starts_with("#region") {
                return true;
            }
        }
        false
    }

    /// Check if a line is a fold region end marker (e.g. #endregion, //endregion, etc.)
    #[cfg(test)]
    fn is_region_end(trimmed: &str) -> bool {
        if trimmed.starts_with("#endregion") || trimmed.starts_with("#pragma endregion") {
            return true;
        }
        if let Some(rest) = trimmed.strip_prefix("//") {
            let rest = rest.trim_start();
            if rest.starts_with("endregion") || rest.starts_with("#endregion") {
                return true;
            }
        }
        if let Some(rest) = trimmed.strip_prefix('#') {
            let rest = rest.trim_start();
            if rest.starts_with("endregion") {
                return true;
            }
        }
        if let Some(rest) = trimmed.strip_prefix("--") {
            let rest = rest.trim_start();
            if rest.starts_with("endregion") || rest.starts_with("#endregion") {
                return true;
            }
        }
        if let Some(rest) = trimmed.strip_prefix("<!--") {
            let rest = rest.trim_start();
            if rest.starts_with("endregion") || rest.starts_with("#endregion") {
                return true;
            }
        }
        false
    }

    fn has_preprocessor_folds(&self) -> bool {
        matches!(self.language_name(), "C" | "C++" | "C/C++ Header" | "C#"
            | "Objective-C" | "Objective-C++" | "GLSL" | "HLSL")
    }

    fn lsp_fold_end_at(&self, start_line: usize) -> Option<usize> {
        let first = self.lsp_folding_ranges.partition_point(|&(start, _)| start < start_line);
        self.lsp_folding_ranges[first..].iter()
            .take_while(|&&(start, _)| start == start_line)
            .filter_map(|&(_, end)| (end > start_line && end < self.line_count()).then(|| end + 1))
            .max()
    }

    fn preprocessor_fold_end(&self, start_line: usize, directive: PreprocessorDirective) -> Option<usize> {
        if directive == PreprocessorDirective::End { return None; }
        let mut continued = preprocessor_continues(self.rope.line(start_line));
        if directive == PreprocessorDirective::Other {
            if !continued { return None; }
            for (offset, line) in self.rope.lines_at(start_line + 1).enumerate() {
                if !preprocessor_continues(line) { return Some(start_line + offset + 2); }
            }
            return Some(self.line_count());
        }
        let mut depth = 0usize;
        for (offset, line) in self.rope.lines_at(start_line + 1).enumerate() {
            if continued {
                continued = preprocessor_continues(line);
                continue;
            }
            let Some(next) = preprocessor_directive(line) else { continue; };
            continued = preprocessor_continues(line);
            match next {
                PreprocessorDirective::If => depth += 1,
                PreprocessorDirective::End if depth > 0 => depth -= 1,
                PreprocessorDirective::End => return Some(start_line + offset + 1),
                PreprocessorDirective::Branch if depth == 0 && directive == PreprocessorDirective::Branch =>
                    return Some(start_line + offset + 1),
                _ => {}
            }
        }
        None
    }

    /// Find the end of a foldable region starting at the given line
    pub(crate) fn find_fold_end(&self, start_line: usize) -> Option<usize> {
        let total = self.line_count();
        if start_line >= total.saturating_sub(1) {
            return None;
        }

        // Check LSP folding ranges first
        if let Some(end) = self.lsp_fold_end_at(start_line) { return Some(end); }

        let start = self.rope.line(start_line);

        // Check for region marker fold first
        if rope_line_region_marker(start) == Some(true) {
            let mut depth = 1i32;
            for (offset, line) in self.rope.lines_at(start_line + 1).enumerate() {
                match rope_line_region_marker(line) {
                    Some(true) => depth += 1,
                    Some(false) => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(start_line + offset + 2);
                        }
                    }
                    None => {}
                }
            }
            return None; // unmatched region start
        }

        let preprocessor = self.has_preprocessor_folds();
        if preprocessor && let Some(directive) = preprocessor_directive(start) {
            return self.preprocessor_fold_end(start_line, directive)
                .filter(|&end| end > start_line + 1);
        }

        // Indent-based folding
        // Lines with no content (only whitespace) are not foldable on their own
        let start_indent = start.chars().position(|c| !c.is_whitespace())?;

        let mut last_deeper = start_line;
        let mut continued = false;
        let mut conditional_depth = 0usize;
        let mut first_code = true;
        for (offset, line) in self.rope.lines_at(start_line + 1).enumerate() {
            let row = start_line + offset + 1;
            // Directives are allowed at column zero inside any C-family
            // scope. Their indentation cannot close the surrounding block.
            let directive = preprocessor.then(|| preprocessor_directive(line)).flatten();
            if preprocessor && (continued || directive.is_some()) {
                if !continued {
                    match directive {
                        Some(PreprocessorDirective::If) => conditional_depth += 1,
                        Some(PreprocessorDirective::End) if conditional_depth > 0 => conditional_depth -= 1,
                        Some(PreprocessorDirective::End | PreprocessorDirective::Branch) if conditional_depth == 0 => break,
                        _ => {}
                    }
                }
                continued = preprocessor_continues(line);
                if last_deeper > start_line && conditional_depth == 0 {
                    last_deeper = row;
                }
                continue;
            }
            let Some(indent) = line.chars().position(|c| !c.is_whitespace()) else {
                // Blank lines are included in the fold
                continue;
            };
            let brace = first_code && indent == start_indent && opens_brace(line);
            first_code = false;
            if indent > start_indent || brace {
                last_deeper = row;
            } else {
                break;
            }
        }

        if last_deeper > start_line {
            Some(last_deeper + 1) // end is exclusive
        } else {
            None
        }
    }

    /// Check if a line can be folded (cheaper than find_fold_end — only checks next non-blank line)
    pub(crate) fn can_fold_line(&self, start_line: usize) -> bool {
        let total = self.line_count();
        if start_line >= total.saturating_sub(1) {
            return false;
        }

        // Check LSP folding ranges
        if self.lsp_fold_end_at(start_line).is_some() { return true; }

        let preprocessor = self.has_preprocessor_folds();
        {
            let mut cache = self.fold_hint_cache.borrow_mut();
            cache.synchronize(&self.rope, self.content_version, preprocessor);
            if let Some(&(hint, _)) = cache.hints.get(&start_line) {
                return hint;
            }
        }
        let result = self.text_can_fold_line(start_line, preprocessor);
        let mut cache = self.fold_hint_cache.borrow_mut();
        if cache.hints.len() >= 4_096 {
            cache.hints.clear();
            cache.skipped_spans.clear();
        }
        #[cfg(test)]
        { cache.builds += 1; }
        cache.hints.insert(start_line, result);
        if result.1 > start_line.saturating_add(1) {
            cache.skipped_spans.insert(start_line, result.1);
        }
        result.0
    }

    fn text_can_fold_line(&self, start_line: usize, preprocessor: bool) -> (bool, usize) {
        let line = self.rope.line(start_line);
        if rope_line_region_marker(line) == Some(true) {
            return (true, start_line);
        }
        if preprocessor && let Some(directive) = preprocessor_directive(line) {
            return (matches!(directive, PreprocessorDirective::If | PreprocessorDirective::Branch)
                || (directive == PreprocessorDirective::Other && preprocessor_continues(line)), start_line);
        }
        let Some(start_indent) = line.chars().position(|c| !c.is_whitespace()) else {
            return (false, start_line);
        };
        let mut continued = false;
        let mut conditional_depth = 0usize;
        for (offset, line) in self.rope.lines_at(start_line + 1).enumerate() {
            let directive = preprocessor.then(|| preprocessor_directive(line)).flatten();
            if preprocessor && (continued || directive.is_some()) {
                if !continued {
                    match directive {
                        Some(PreprocessorDirective::If) => conditional_depth += 1,
                        Some(PreprocessorDirective::End) if conditional_depth > 0 => conditional_depth -= 1,
                        Some(PreprocessorDirective::End | PreprocessorDirective::Branch) if conditional_depth == 0 =>
                            return (false, start_line + 1 + offset),
                        _ => {}
                    }
                }
                continued = preprocessor_continues(line);
                continue;
            }
            if let Some(indent) = line.chars().position(|c| !c.is_whitespace()) {
                return (indent > start_indent || (indent == start_indent && opens_brace(line)),
                    start_line + 1 + offset);
            }
        }
        (false, self.line_count().saturating_sub(1))
    }

    /// Toggle fold at the given line
    pub(crate) fn toggle_fold(&mut self, line: usize) {
        self.fold_buttons.clear();
        if self.folded_ranges.contains_key(&line) {
            self.folded_ranges.remove(&line);
        } else if let Some(end) = self.find_fold_end(line) {
            // Remove any sub-folds that are within the new fold range
            self.folded_ranges
                .retain(|&start, &mut sub_end| !(start > line && start <= end && sub_end <= end));
            self.folded_ranges.insert(line, end);
            // Move cursor out of the folded region if it's inside (end is exclusive)
            if self.cursor_line > line && self.cursor_line < end {
                self.cursor_line = line;
                self.cursor_col = 0;
                self.desired_col = None;
                self.clear_selection();
            }
        }
        self.cached_wrapped_total = None;
        self.cached_row_offsets = None;
        self.cached_visible_line_count = None;
    }

    /// Fold all foldable regions
    pub(crate) fn fold_all(&mut self) {
        self.fold_buttons.clear();
        self.folded_ranges.clear();
        let total = self.line_count();
        let mut line = 0;
        while line < total {
            if let Some(end) = self.find_fold_end(line) {
                self.folded_ranges.insert(line, end);
                line = end;
            } else {
                line += 1;
            }
        }
        self.cached_visible_line_count = None;
        self.cached_wrapped_total = None;
        self.cached_row_offsets = None;
    }

    /// Unfold all folded regions
    pub(crate) fn unfold_all(&mut self) {
        self.fold_buttons.clear();
        self.folded_ranges.clear();
        self.cached_visible_line_count = None;
        self.cached_wrapped_total = None;
        self.cached_row_offsets = None;
    }

    /// Fold regions at a specific indentation level
    pub(crate) fn fold_at_level(&mut self, level: usize) {
        self.fold_buttons.clear();
        self.folded_ranges.clear();
        let ts = self.tab_size.max(1);
        let mut covered_until = 0;
        for (line, text) in self.rope.lines().enumerate() {
            // Allman braces and outdented directives can share the selected
            // level with an enclosing header. Hidden child starts must not
            // shadow that outer interval in navigation and viewport lookups.
            if line < covered_until { continue; }
            let indent = text
                .chars()
                .take_while(|c| *c == ' ')
                .count()
                / ts;
            // Shallower scopes cannot be selected at this level. Resolving
            // their ends repeatedly walks code that we must visit again.
            if indent + 1 == level && let Some(end) = self.find_fold_end(line) {
                self.folded_ranges.insert(line, end);
                covered_until = end;
            }
        }
        self.cached_visible_line_count = None;
        self.cached_wrapped_total = None;
        self.cached_row_offsets = None;
    }

    /// Resolve one viewport row without materializing every preceding line.
    /// Plain runs are skipped together, so work depends on crossed folds.
    pub(crate) fn visible_line_at(&self, from: usize, mut offset: usize) -> Option<usize> {
        let total = self.line_count();
        if self.folded_ranges.is_empty() {
            return from.checked_add(offset).filter(|&line| line < total);
        }
        let mut line = from;
        while line < total {
            if let Some((_, &end)) = self.folded_ranges.range(..line).next_back()
                && line < end
            {
                line = end;
                continue;
            }
            if offset == 0 {
                return Some(line);
            }
            let Some((&start, &end)) = self.folded_ranges.range(line..).next() else {
                return line.checked_add(offset).filter(|&line| line < total);
            };
            let plain_count = start.min(total).saturating_sub(line);
            if offset < plain_count {
                return Some(line + offset);
            }
            offset -= plain_count;
            if start >= total {
                return None;
            }
            if offset == 0 {
                return (start < total).then_some(start);
            }
            offset -= 1; // The fold header itself remains visible.
            line = end;
        }
        None
    }

    /// Get visible lines (skipping folded ranges), returns list of actual line indices
    pub(crate) fn visible_lines(&self, from: usize, count: usize) -> Vec<usize> {
        let total = self.line_count();
        if self.folded_ranges.is_empty() {
            if from >= total {
                return Vec::new();
            }
            let end = from.saturating_add(count).min(total);
            return (from..end).collect();
        }
        let mut result = Vec::with_capacity(count.min(total.saturating_sub(from)));
        let mut line = from;
        while result.len() < count && line < total {
            if let Some((_, &end)) = self.folded_ranges.range(..line).next_back() {
                if line < end {
                    // Jump across the hidden interval in one step. Advancing
                    // one line at a time made a viewport request proportional
                    // to the size of a large collapsed block.
                    line = end;
                    continue;
                }
            }
            // Tree lookups are needed only at crossed folds, not for every
            // visible row between them.
            let next = self.folded_ranges.range(line..).next();
            let plain_end = next.map_or(total, |(&start, _)| start.min(total));
            let end = line.saturating_add(count - result.len()).min(plain_end);
            result.extend(line..end);
            line = end;
            if result.len() == count || line >= total { break; }
            if let Some((&start, &end)) = next {
                result.push(start);
                line = end.max(start.saturating_add(1));
            }
        }
        result
    }

    /// Count total visible lines (excluding folded), with caching
    pub(crate) fn visible_line_count(&mut self) -> usize {
        if let Some(count) = self.cached_visible_line_count {
            return count;
        }
        let total = self.line_count();
        if self.folded_ranges.is_empty() {
            self.cached_visible_line_count = Some(total);
            return total;
        }
        // Fold ranges are normally disjoint, but session data may contain
        // overlaps. Count the union of hidden `(start + 1)..end` intervals in
        // O(number of folds), never O(number of document lines).
        let mut hidden = 0usize;
        let mut covered_until = 0usize;
        for (&start, &end) in &self.folded_ranges {
            let hidden_start = start.saturating_add(1).min(total);
            let hidden_end = end.min(total);
            let uncovered_start = hidden_start.max(covered_until);
            hidden = hidden.saturating_add(hidden_end.saturating_sub(uncovered_start));
            covered_until = covered_until.max(hidden_end);
        }
        let count = total.saturating_sub(hidden);
        self.cached_visible_line_count = Some(count);
        count
    }

    pub(crate) fn toggle_bookmark(&mut self) {
        if !self.bookmarks.remove(&self.cursor_line) {
            self.bookmarks.insert(self.cursor_line);
        }
    }

    pub(crate) fn next_bookmark(&mut self) {
        if self.bookmarks.is_empty() {
            return;
        }
        self.clear_selection();
        // Find next bookmark after current line
        if let Some(&line) = self.bookmarks.range((self.cursor_line + 1)..).next() {
            self.set_cursor(line, 0);
        } else if let Some(&line) = self.bookmarks.iter().next() {
            // Wrap around
            self.set_cursor(line, 0);
        }
    }

    pub(crate) fn prev_bookmark(&mut self) {
        if self.bookmarks.is_empty() {
            return;
        }
        self.clear_selection();
        // Find previous bookmark before current line
        if let Some(&line) = self.bookmarks.range(..self.cursor_line).next_back() {
            self.set_cursor(line, 0);
        } else if let Some(&line) = self.bookmarks.iter().next_back() {
            // Wrap around
            self.set_cursor(line, 0);
        }
    }

    pub(crate) fn clear_bookmarks(&mut self) {
        self.bookmarks.clear();
    }

    /// Shift bookmark line numbers when lines are inserted or deleted.
    /// `from_line`: the line at which the change occurs.
    /// `delta`: positive = lines inserted, negative = lines deleted.
    /// Bookmarks on deleted lines are removed.
    pub(crate) fn shift_bookmarks(&mut self, from_line: usize, delta: isize) {
        if self.bookmarks.is_empty() {
            return;
        }
        let old: Vec<usize> = self.bookmarks.iter().copied().collect();
        self.bookmarks.clear();
        for line in old {
            if line < from_line {
                self.bookmarks.insert(line);
            } else if delta < 0 {
                let removed = (-delta) as usize;
                if line >= from_line + removed {
                    self.bookmarks.insert(line - removed);
                }
                // else: bookmark was on a deleted line, drop it
            } else {
                // delta >= 0 here (insertion), so addition is always safe
                self.bookmarks.insert(line.saturating_add(delta as usize));
            }
        }
    }

    /// Check if the file has been modified on disk since last read/save
    #[cfg(test)]
    pub(crate) fn has_disk_changed(&self) -> bool {
        if let Some(ref path) = self.file_path {
            if let Some(last_known) = self.last_disk_modified {
                if let Ok(meta) = fs::metadata(path) {
                    if let Ok(disk_modified) = meta.modified() {
                        return disk_modified > last_known;
                    }
                }
            }
        }
        false
    }

    /// Reload file from disk
    pub(crate) fn reload_from_disk(&mut self) -> io::Result<()> {
        crate::editor_document::edit!(self, {
        let Some(path) = self.file_path.clone() else {
            return Ok(());
        };
        let loaded = crate::editor::load_text_file(&path)?;
        if !self.apply_reloaded_file(loaded) {
            // Explicit Reload still resets the view, even when its content
            // is unchanged. Watcher notifications leave the view alone.
            self.reset_reloaded_view();
        }
        Ok(())
            })
    }

    /// Install a disk snapshot, returning whether the document text changed.
    pub(crate) fn apply_reloaded_file(&mut self, loaded: crate::editor::LoadedTextFile) -> bool {
        crate::editor_document::edit!(self, {
        self.remote_checkpoint = loaded.remote_checkpoint;
        if self.save_content_error().is_some() {
            // This is the first real document after a loading/error/binary
            // placeholder. UI messages must never become undoable edits.
            self.apply_bg_loaded(
                loaded.rope, loaded.encoding, loaded.file_size,
                crate::editor::LARGE_FILE_THRESHOLD,
            );
            self.last_disk_modified = loaded.last_disk_modified;
            self.read_only = loaded.encoding == "Binary";
            self.reset_reloaded_view();
            return true;
        }
        self.encoding = loaded.encoding;
        self.file_size = loaded.file_size;
        self.last_disk_modified = loaded.last_disk_modified;
        self.disk_unavailable = false;
        self.load_failed = false;

        if self.rope == loaded.rope {
            // Saves/discards can generate duplicate watcher events. Keep the
            // existing Rope identity, undo/redo stacks and cursor unchanged.
            self.record_saved_snapshot(self.rope.clone());
            self.update_large_file_mode(crate::editor::LARGE_FILE_THRESHOLD);
            return false;
        }
        let previous_lines = self.rope.len_lines();
        self.save_undo();
        self.rope = loaded.rope;
        self.update_large_file_mode(crate::editor::LARGE_FILE_THRESHOLD);
        if let Some(mut state) = self.undo_stack.back_mut() {
            // Reload can change text anywhere, independently of the cursor.
            // Undo and redo must invalidate the whole replaced document too.
            state.dirty_from = 0;
            state.dirty_through = previous_lines.max(self.rope.len_lines()).saturating_sub(1);
        }
        self.mark_syntax_dirty_range(0, self.rope.len_lines());
        self.break_undo_group();
        self.content_version += 1;
        self.record_saved_snapshot(self.rope.clone());
        self.reset_reloaded_view();
        true
            })
    }

    fn reset_reloaded_view(&mut self) {
        self.snippet_cancel();
        self.cached_visible_line_count = None;
        self.cached_wrapped_total = None;
        self.cached_row_offsets = None;
        self.folded_ranges.clear();
        self.clear_git_line_status();
        // Clear multi-cursor state (positions are invalid after content change)
        self.extra_cursors.clear();
        self.extra_cursor_anchors.clear();
        self.selection_anchor = None;
        self.detect_indentation();
        self.detect_line_ending();
        self.cursor_line = self.cursor_line.min(self.line_count().saturating_sub(1));
        self.cursor_col = self.cursor_col.min(self.line_len(self.cursor_line));
        // Reset scroll to a sane position (ensure_visible will fix it on next render)
        self.scroll_offset = self.cursor_line.saturating_sub(10);
        self.wrap_scroll_offset = 0;
        self.horizontal_scroll = 0;
    }

    /// Jump to next changed line (git diff)
    pub(crate) fn next_change(&mut self) {
        let total = self.line_count();
        for offset in 1..total {
            let line = (self.cursor_line + offset) % total;
            if self.line_diff_status(line) != ' ' {
                self.set_cursor(line, 0);
                return;
            }
        }
    }

    /// Jump to previous changed line (git diff)
    pub(crate) fn prev_change(&mut self) {
        let total = self.line_count();
        for offset in 1..total {
            let line = (self.cursor_line + total - offset) % total;
            if self.line_diff_status(line) != ' ' {
                self.set_cursor(line, 0);
                return;
            }
        }
    }

    pub(crate) fn remove_duplicate_lines(&mut self) {
        crate::editor_document::edit!(self, {
        self.transform_lines(LineTransform::RemoveDuplicates);
            })
    }

    /// Remove blank/empty lines.
    pub(crate) fn remove_blank_lines(&mut self) {
        crate::editor_document::edit!(self, {
        self.transform_lines(LineTransform::RemoveBlank);
            })
    }

    /// Reverse selected lines (or all lines).
    pub(crate) fn reverse_lines(&mut self) {
        crate::editor_document::edit!(self, {
        self.transform_lines(LineTransform::Reverse);
            })
    }

    /// Shuffle/randomize selected lines.
    pub(crate) fn shuffle_lines(&mut self) {
        crate::editor_document::edit!(self, {
        self.transform_lines(LineTransform::Shuffle);
            })
    }

    fn transform_lines(&mut self, transform: LineTransform) {
        crate::editor_document::edit!(self, {
        use crate::editor_line_ops::split_line_text;

        // Rope exposes an empty final row after a newline. It is an EOF
        // position, not another line to reorder or deduplicate.
        let total = self.line_count();
        let content_lines = total - usize::from(self.rope.line(total - 1).len_chars() == 0);
        let selection = if matches!(transform, LineTransform::RemoveBlank) { None }
            else { self.selection_range() };
        let (start, end) = selection.map_or((0, content_lines), |(sl, _, el, ec)| {
            (sl.min(content_lines), el.saturating_add(usize::from(ec != 0 || sl == el)).min(content_lines))
        });
        if start >= end { return; }
        // The common no-op needs neither a line table nor owned text. Stop
        // the preflight at the first blank row when filtering is necessary.
        if matches!(transform, LineTransform::RemoveBlank)
            && !self.rope.lines_at(start).take(end - start)
                .any(|line| line.chars().all(char::is_whitespace))
        { return; }
        let from = self.rope.line_to_char(start);
        let to = if end < total { self.rope.line_to_char(end) } else { self.rope.len_chars() };
        let lines: Vec<_> = self.rope.lines_at(start).take(end - start)
            .map(split_line_text).collect();
        let mut order: Vec<_> = (0..lines.len()).collect();
        match transform {
            LineTransform::Reverse => order.reverse(),
            LineTransform::Shuffle => {
                let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default().as_nanos() as u64;
                for i in (1..order.len()).rev() {
                    let mut h = seed ^ (i as u64).wrapping_mul(0x9e3779b97f4a7c15);
                    h ^= h >> 30;
                    h = h.wrapping_mul(0xbf58476d1ce4e5b9);
                    h ^= h >> 27;
                    h = h.wrapping_mul(0x94d049bb133111eb);
                    h ^= h >> 31;
                    order.swap(i, (h % (i as u64 + 1)) as usize);
                }
            }
            LineTransform::RemoveDuplicates => {
                let mut seen = rustc_hash::FxHashSet::default();
                order.retain(|&i| seen.insert(lines[i].0.as_ref()));
            }
            LineTransform::RemoveBlank => order.retain(|&i| !lines[i].0.trim().is_empty()),
        }
        if order.len() == lines.len() && order.iter().copied().eq(0..lines.len()) { return; }

        let reordering = matches!(transform, LineTransform::Reverse | LineTransform::Shuffle);
        let last_ending = lines.last().unwrap().1;
        let mut text = String::with_capacity(self.rope.char_to_byte(to) - self.rope.char_to_byte(from));
        for (row, &original) in order.iter().enumerate() {
            text.push_str(&lines[original].0);
            // Reordering keeps separators in their slots. Removing lines keeps
            // surviving separators, but always preserves the range's EOF or
            // its boundary with the untouched suffix.
            text.push_str(if row + 1 == order.len() { last_ending }
                else { lines[if reordering { row } else { original }].1 });
        }
        if order.is_empty() { text.push_str(last_ending); }
        if self.rope.slice(from..to) == text.as_str() { return; }

        let mut cursors = self.collect_multi_cursors();
        let mut line_map = vec![None; lines.len()];
        for (row, &original) in order.iter().enumerate() { line_map[original] = Some(start + row); }
        let replacement_rows = order.len().max(usize::from(!last_ending.is_empty()));
        let removed = lines.len() - replacement_rows;
        self.bookmarks = self.bookmarks.iter().filter_map(|&line| {
            if line < start { Some(line) }
            else if line >= end { Some(line - removed) }
            else { line_map[line - start] }
        }).collect();
        // Carets on a deleted row move to the next surviving row, or the last
        // one at EOF. Keep every surviving caret/anchor attached to its text.
        let mut next = start + order.len().saturating_sub(1);
        for row in line_map.iter_mut().rev() {
            if let Some(line) = *row { next = line; } else { *row = Some(next); }
        }
        let map_line = |line: usize| {
            if line < start { line } else if line >= end { line - removed }
            else { line_map[line - start].unwrap() }
        };
        for cursor in &mut cursors {
            cursor.line = map_line(cursor.line);
            if let Some((line, col)) = cursor.anchor { cursor.anchor = Some((map_line(line), col)); }
        }
        self.save_undo();
        self.rope.remove(from..to);
        self.rope.insert(from, &text);
        self.mark_syntax_dirty_range(start, end - 1);
        self.modified = true;
        self.desired_col = None;
        self.clear_stale_folds();
        if !matches!(transform, LineTransform::RemoveDuplicates) { cursors[0].anchor = None; }
        self.set_multi_cursor_states(cursors);
            })
    }

    /// Rewrite only indentation, retaining code, separators, selections, and
    /// each caret's visual position through tab/space conversions.
    fn replace_indentation(&mut self, changes: Vec<(usize, String, String)>) {
        crate::editor_document::edit!(self, {
        if changes.is_empty() { return; }
        let tab_size = self.tab_size.max(1);
        let mut cursors = self.collect_multi_cursors();
        let remap = |line: usize, col: usize| {
            let Ok(index) = changes.binary_search_by_key(&line, |change| change.0) else { return col; };
            let (_, old, new) = &changes[index];
            if col >= old.len() { return new.len() + col - old.len(); }
            let cells = old.bytes().take(col).fold(0, |cells, ch| {
                cells + if ch == b'\t' { tab_size - cells % tab_size } else { 1 }
            });
            let mut width = 0;
            for (index, ch) in new.bytes().enumerate() {
                if width >= cells { return index; }
                width += if ch == b'\t' { tab_size - width % tab_size } else { 1 };
            }
            new.len()
        };
        for cursor in &mut cursors {
            cursor.col = remap(cursor.line, cursor.col);
            if let Some((line, col)) = cursor.anchor { cursor.anchor = Some((line, remap(line, col))); }
        }
        let edits: Vec<_> = changes.iter().map(|(line, old, new)| crate::editor_line_ops::LinePrefixEdit {
            line: *line, index: self.rope.line_to_char(*line), col: 0,
            removed: old.len(), insert: new.as_str(),
        }).collect();
        self.apply_line_prefix_edits(&edits);
        self.desired_col = None;
        self.set_multi_cursor_states(cursors);
            })
    }

    fn convert_indentation(&mut self, tabs: bool) {
        crate::editor_document::edit!(self, {
        let size = self.tab_size.max(1);
        let mut changes = Vec::new();
        for (line, text) in self.rope.lines().enumerate() {
            let old: String = text.chars().take_while(|ch| matches!(ch, ' ' | '\t')).collect();
            if old.is_empty() { continue; }
            let width = old.bytes().fold(0, |width, ch| {
                width + if ch == b'\t' { size - width % size } else { 1 }
            });
            let new = if tabs { "\t".repeat(width / size) + &" ".repeat(width % size) }
                else { " ".repeat(width) };
            if old != new { changes.push((line, old, new)); }
        }
        self.replace_indentation(changes);
        self.use_tabs = tabs;
            })
    }

    pub(crate) fn convert_indentation_to_spaces(&mut self) {
        crate::editor_document::edit!(self, {
        self.convert_indentation(false);
            })
    }

    pub(crate) fn convert_indentation_to_tabs(&mut self) {
        crate::editor_document::edit!(self, {
        self.convert_indentation(true);
            })
    }

    /// Reindent selected lines (or all lines) based on brace/bracket nesting depth.
    pub(crate) fn reindent_lines(&mut self) {
        crate::editor_document::edit!(self, {
        let (start, end) = if let Some((sl, _, el, ec)) = self.selection_range() {
            (sl, el + usize::from(ec != 0 || el == sl))
        } else { (0, self.line_count()) };
        let indent = if self.use_tabs { "\t".to_string() } else { " ".repeat(self.tab_size.max(1)) };
        let mut depth = 0usize;
        let mut changes = Vec::new();
        for (line, text) in self.rope.lines().take(end).enumerate() {
            let old: String = text.chars().take_while(|ch| matches!(ch, ' ' | '\t')).collect();
            let content = text.slice(old.len()..);
            if line >= start && content.chars().any(|ch| !matches!(ch, '\r' | '\n')) {
                // Leading closers affect this line's indentation, but each
                // closer changes the running depth exactly once below.
                let closers = content.chars().take_while(|ch| matches!(ch, '}' | ')' | ']')).count();
                let new = indent.repeat(depth.saturating_sub(closers));
                if old != new { changes.push((line, old, new)); }
            }
            for ch in content.chars() {
                match ch {
                    '{' | '(' | '[' => depth = depth.saturating_add(1),
                    '}' | ')' | ']' => depth = depth.saturating_sub(1),
                    _ => {}
                }
            }
        }
        self.replace_indentation(changes);
            })
    }
    fn transform_selected(&mut self, f: impl FnOnce(&str) -> String) -> Option<String> {
        crate::editor_document::edit!(self, {
        let text = self.selected_text()?;
        let result = f(&text);
        self.replace_transformed_selection(&result, false);
        Some(result)
            })
    }

    pub(crate) fn base64_encode_selection(&mut self) -> bool {
        crate::editor_document::edit!(self, {
        self.transform_selected(|text| general_purpose::STANDARD.encode(text.as_bytes()))
            .is_some()
            })
    }

    pub(crate) fn surround_selection(&mut self, open: &str, close: &str) -> bool {
        crate::editor_document::edit!(self, {
        let o = open.to_string();
        let c = close.to_string();
        self.transform_selected(|text| format!("{o}{text}{c}"))
            .is_some()
            })
    }

    pub(crate) fn base64_decode_selection(&mut self) -> bool {
        crate::editor_document::edit!(self, {
        self.transform_selected(|text| match general_purpose::STANDARD.decode(text.trim()) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(_) => text.to_string(),
        })
        .is_some()
            })
    }

    pub(crate) fn url_encode_selection(&mut self) -> bool {
        crate::editor_document::edit!(self, {
        self.transform_selected(|text| {
            let mut out = String::with_capacity(text.len());
            for c in text.chars() {
                if c.is_ascii_alphanumeric() || "-_.~".contains(c) {
                    out.push(c);
                } else {
                    // Encode each UTF-8 byte of the character separately
                    let mut buf = [0u8; 4];
                    let encoded = c.encode_utf8(&mut buf);
                    for b in encoded.as_bytes() {
                        out.push('%');
                        out.push_str(&format!("{b:02X}"));
                    }
                }
            }
            out
        })
        .is_some()
            })
    }

    pub(crate) fn url_decode_selection(&mut self) -> bool {
        crate::editor_document::edit!(self, {
        self.transform_selected(|text| {
            let mut decoded_bytes: Vec<u8> = Vec::new();
            let bytes = text.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'%' && i + 2 < bytes.len() {
                    let hex = |byte: u8| match byte {
                        b'0'..=b'9' => Some(byte - b'0'),
                        b'a'..=b'f' => Some(byte - b'a' + 10),
                        b'A'..=b'F' => Some(byte - b'A' + 10),
                        _ => None,
                    };
                    if let (Some(high), Some(low)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                        decoded_bytes.push(high * 16 + low);
                        i += 3;
                        continue;
                    }
                }
                decoded_bytes.push(bytes[i]);
                i += 1;
            }
            String::from_utf8(decoded_bytes)
                .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
        })
        .is_some()
            })
    }

    pub(crate) fn format_json(&mut self) -> Result<(), String> {
        crate::editor_document::edit!(self, {
        let text = self.rope.to_string();
        let value: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("Invalid JSON: {e}"))?;
        let formatted =
            serde_json::to_string_pretty(&value).map_err(|e| format!("Format error: {e}"))?;
        self.apply_json_transform(&formatted, false);
        Ok(())
            })
    }

    pub(crate) fn minify_json(&mut self) -> Result<(), String> {
        crate::editor_document::edit!(self, {
        let text = self.rope.to_string();
        let value: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("Invalid JSON: {e}"))?;
        let minified = serde_json::to_string(&value).map_err(|e| format!("Minify error: {e}"))?;
        self.apply_json_transform(&minified, true);
        Ok(())
            })
    }

    pub(crate) fn sort_json_keys(&mut self) -> Result<(), String> {
        crate::editor_document::edit!(self, {
        let text = self.rope.to_string();
        let value: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("Invalid JSON: {e}"))?;
        let sorted = sort_json_value(&value);
        let formatted =
            serde_json::to_string_pretty(&sorted).map_err(|e| format!("Format error: {e}"))?;
        self.apply_json_transform(&formatted, false);
        Ok(())
            })
    }

    fn apply_json_transform(&mut self, formatted: &str, move_to_start: bool) {
        if self.rope.slice(..) == formatted { return; }
        self.save_undo_for_range(0, self.line_count() - 1);
        self.rope = Rope::from_str(formatted);
        self.invalidate_rope_caches();
        self.modified = true;
        self.snippet_cancel();
        self.clear_stale_folds();
        if move_to_start { self.cursor_line = 0; }
        self.cursor_col = 0;
        // Formatting can remove every row that a primary or secondary anchor
        // refers to. Reconcile all endpoints before another command uses them.
        self.set_multi_cursor_states(self.collect_multi_cursors());
        self.refresh_modified_from_saved();
    }
}

#[cfg(test)]
#[path = "editor_transform_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "editor_local_operation_performance_tests.rs"]
mod local_operation_tests;

#[cfg(test)]
#[path = "editor_responsiveness_tests.rs"]
mod responsiveness_tests;

#[cfg(test)]
#[path = "editor_fold_performance_tests.rs"]
mod fold_performance_tests;

#[cfg(test)]
#[path = "editor_line_ending_tests.rs"]
mod line_ending_tests;
