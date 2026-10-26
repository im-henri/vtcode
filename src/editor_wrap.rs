use crate::editor::{Editor, WrappedRowInfo};

/// Small buffers can build their wrap map synchronously without affecting an
/// input frame. Larger buffers use the application's cancellable wrap worker.
const MAX_SYNC_WRAP_LAYOUT_LINES: usize = 2_048;
const MAX_SYNC_WRAP_LAYOUT_BYTES: usize = 256 * 1024;
const MAX_INCREMENTAL_WRAP_LINES: usize = 64;
const MAX_INCREMENTAL_WRAP_BYTES: usize = 64 * 1024;
const MAX_CACHED_WRAP_LINES: usize = 10_000;

#[cfg(test)]
thread_local! {
    static REFERENCE_WRAP_LOOKUPS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static REFERENCE_WRAP_EVICTION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Visual-row prefix sums with logarithmic point updates and row lookup.
/// Editing one line must not rebuild every later document offset.
#[derive(Clone, Debug)]
pub(crate) struct WrapRowOffsets {
    tree: Vec<usize>,
    total: usize,
}

impl From<Vec<usize>> for WrapRowOffsets {
    fn from(offsets: Vec<usize>) -> Self {
        Self::from_offsets_cancellable(offsets, || false).expect("uncancelled offset conversion")
    }
}

impl WrapRowOffsets {
    /// Building the point-update tree is also document-sized worker work.
    /// Superseded resize jobs must stop during conversion as well as decoding.
    pub(crate) fn from_offsets_cancellable(
        mut offsets: Vec<usize>,
        cancelled: impl Fn() -> bool,
    ) -> Option<Self> {
        if cancelled() {
            return None;
        }
        let total = offsets.last().copied().unwrap_or(0);
        for index in (1..offsets.len()).rev() {
            if index % WRAP_CANCELLATION_WORK == 0 && cancelled() {
                return None;
            }
            offsets[index] -= offsets[index - 1];
        }
        for index in 1..offsets.len() {
            if index % WRAP_CANCELLATION_WORK == 0 && cancelled() {
                return None;
            }
            let parent = index + (index & index.wrapping_neg());
            if parent < offsets.len() {
                offsets[parent] += offsets[index];
            }
        }
        (!cancelled()).then_some(Self {
            tree: offsets,
            total,
        })
    }

    fn line_count(&self) -> usize {
        self.tree.len().saturating_sub(1)
    }

    fn row_for_line(&self, line: usize) -> usize {
        let mut index = line.min(self.line_count());
        let mut sum = 0;
        while index > 0 {
            sum += self.tree[index];
            index &= index - 1;
        }
        sum
    }

    fn set_line_rows(&mut self, line: usize, rows: usize) {
        let previous = self.row_for_line(line + 1) - self.row_for_line(line);
        if rows == previous {
            return;
        }
        let mut index = line + 1;
        while index < self.tree.len() {
            if rows >= previous {
                self.tree[index] += rows - previous;
            } else {
                self.tree[index] -= previous - rows;
            }
            index += index & index.wrapping_neg();
        }
        if rows >= previous {
            self.total += rows - previous;
        } else {
            self.total -= previous - rows;
        }
    }

    /// Return the logical line and its base row, skipping folded zero-row lines.
    fn line_for_row(&self, row: usize) -> Option<(usize, usize)> {
        if row >= self.total || self.line_count() == 0 {
            return None;
        }
        let mut index = 0;
        let mut sum = 0;
        let mut bit = 1usize << self.line_count().ilog2();
        while bit != 0 {
            let next = index + bit;
            if next < self.tree.len() && sum + self.tree[next] <= row {
                sum += self.tree[next];
                index = next;
            }
            bit >>= 1;
        }
        Some((index, sum))
    }
}

/// Shared segmentation for painting and count-only layout. The latter never
/// needs to allocate a vector with an entry for every wrapped row.
fn visit_wrap_ranges<T: Copy>(
    chars: &[T],
    width: usize,
    is_whitespace: impl Fn(T) -> bool,
    is_break: impl Fn(T) -> bool,
    mut emit: impl FnMut(usize, usize),
) {
    if chars.is_empty() {
        emit(0, 0);
        return;
    }
    let mut start = 0;
    while start < chars.len() {
        if chars.len() - start <= width {
            emit(start, chars.len());
            break;
        }
        let split_limit = start + width;
        let boundary = ((start + 1)..=split_limit)
            .rev()
            .find(|&end| is_break(chars[end - 1]));
        let mut end = boundary.unwrap_or(split_limit);
        if boundary.is_some() && is_whitespace(chars[end - 1]) {
            // More than one extra cell means falling back to split_limit.
            // Stop as soon as that is known, even for a megabyte of spaces.
            while end < chars.len() && end <= split_limit + 1 && is_whitespace(chars[end]) {
                end += 1;
            }
            if end > split_limit + 1 {
                end = split_limit;
            }
        }
        emit(start, end);
        start = end;
    }
}

fn ascii_wrap_break(c: u8) -> bool {
    c.is_ascii_whitespace() || matches!(c, b'-' | b'/' | b'\\' | b',' | b';')
}

fn unicode_wrap_break(c: char) -> bool {
    c.is_whitespace() || matches!(c, '-' | '/' | '\\' | ',' | ';')
}

/// Bound cancellation latency even when a document has one enormous logical
/// line. Work counts decoded/scanned characters, independently of row width.
const WRAP_CANCELLATION_WORK: usize = 4_096;

struct WrapCancellation<'a, F> {
    cancelled: &'a F,
    remaining: usize,
}

impl<'a, F: Fn() -> bool> WrapCancellation<'a, F> {
    fn new(cancelled: &'a F) -> Self {
        Self {
            cancelled,
            remaining: WRAP_CANCELLATION_WORK,
        }
    }

    fn tick(&mut self) -> Option<()> {
        self.remaining -= 1;
        if self.remaining == 0 {
            self.remaining = WRAP_CANCELLATION_WORK;
            if (self.cancelled)() {
                return None;
            }
        }
        Some(())
    }
}

/// ASCII lines need no UTF-8 decoding. Retain only borrowed chunks intersecting
/// the current width window and its two lookahead characters. Searching within
/// slices preserves the fast path for lines consisting mostly of whitespace.
fn count_ascii_wrap_rows<'a>(
    chunks: impl Iterator<Item = &'a str>,
    len: usize,
    width: usize,
    cancelled: &impl Fn() -> bool,
    is_break: impl Fn(u8) -> bool,
) -> Option<usize> {
    let mut rows = 0;
    visit_ascii_wrap_ranges(chunks, len, width, cancelled, is_break, |_, _| rows += 1)?;
    Some(rows)
}

fn visit_ascii_wrap_ranges<'a>(
    mut chunks: impl Iterator<Item = &'a str>,
    len: usize,
    width: usize,
    cancelled: &impl Fn() -> bool,
    is_break: impl Fn(u8) -> bool,
    mut emit: impl FnMut(usize, usize),
) -> Option<()> {
    if cancelled() {
        return None;
    }
    if len <= width {
        emit(0, len);
        return Some(());
    }
    let mut chunks_in_window: std::collections::VecDeque<(usize, &'a [u8])> =
        std::collections::VecDeque::new();
    let mut loaded = 0;
    let mut start = 0;
    let mut work = WrapCancellation::new(cancelled);
    while start < len {
        if len - start <= width {
            emit(start, len);
            break;
        }
        let limit = start + width;
        let target = limit.saturating_add(2).min(len);
        while loaded < target {
            if cancelled() {
                return None;
            }
            let chunk = chunks.next()?.as_bytes();
            chunks_in_window.push_back((loaded, chunk));
            loaded += chunk.len();
        }
        let mut boundary = None;
        for &(offset, chunk) in chunks_in_window.iter().rev() {
            if offset >= limit {
                continue;
            }
            let first = start.saturating_sub(offset).min(chunk.len());
            let last = (limit - offset).min(chunk.len());
            for index in (first..last).rev() {
                work.tick()?;
                if is_break(chunk[index]) {
                    boundary = Some((offset + index + 1, chunk[index].is_ascii_whitespace()));
                    break;
                }
            }
            if boundary.is_some() {
                break;
            }
        }
        let mut end = boundary.map_or(limit, |(end, _)| end);
        if boundary == Some((limit, true)) {
            let whitespace_at = |position: usize| {
                position < len
                    && chunks_in_window
                        .iter()
                        .rev()
                        .find_map(|&(offset, chunk)| {
                            (position >= offset && position - offset < chunk.len())
                                .then(|| chunk[position - offset].is_ascii_whitespace())
                        })
                        .unwrap_or(false)
            };
            if whitespace_at(limit) && !whitespace_at(limit + 1) {
                end += 1;
            }
        }
        emit(start, end);
        start = end;
        while chunks_in_window
            .front()
            .is_some_and(|&(offset, chunk)| offset + chunk.len() <= start)
        {
            work.tick()?;
            chunks_in_window.pop_front();
        }
        // Short whitespace rows may inspect only one character each.
        work.tick()?;
    }
    Some(())
}

/// There can be at most two decoded lookahead characters beyond a split. The
/// last three break positions therefore always include the latest eligible
/// break, without retaining a Vec<char> or rescanning decoded window contents.
fn count_unicode_wrap_rows(
    chars: impl Iterator<Item = char>,
    len: usize,
    width: usize,
    cancelled: &impl Fn() -> bool,
) -> Option<usize> {
    let mut rows = 0;
    visit_unicode_wrap_ranges(chars, len, width, cancelled, |_, _| rows += 1)?;
    Some(rows)
}

fn visit_unicode_wrap_ranges(
    mut chars: impl Iterator<Item = char>,
    len: usize,
    width: usize,
    cancelled: &impl Fn() -> bool,
    mut emit: impl FnMut(usize, usize),
) -> Option<()> {
    if cancelled() {
        return None;
    }
    if len <= width {
        emit(0, len);
        return Some(());
    }
    let mut breaks = [(usize::MAX, false); 3];
    let mut decoded = 0;
    let mut previous_whitespace = false;
    let mut last_whitespace = false;
    let mut start = 0;
    let mut work = WrapCancellation::new(cancelled);
    while start < len {
        if len - start <= width {
            emit(start, len);
            break;
        }
        let limit = start + width;
        let target = limit.saturating_add(2).min(len);
        while decoded < target {
            work.tick()?;
            let c = chars.next()?;
            previous_whitespace = last_whitespace;
            last_whitespace = c.is_whitespace();
            if last_whitespace || matches!(c, '-' | '/' | '\\' | ',' | ';') {
                breaks[0] = breaks[1];
                breaks[1] = breaks[2];
                breaks[2] = (decoded, last_whitespace);
            }
            decoded += 1;
        }
        let boundary = breaks
            .iter()
            .rev()
            .find(|&&(position, _)| position >= start && position < limit);
        let mut end = boundary.map_or(limit, |&(position, _)| position + 1);
        if boundary == Some(&(limit - 1, true)) {
            let two_lookahead = decoded - limit == 2;
            let first_whitespace = if two_lookahead {
                previous_whitespace
            } else {
                last_whitespace
            };
            let second_whitespace = two_lookahead && last_whitespace;
            if first_whitespace && !second_whitespace {
                end += 1;
            }
        }
        emit(start, end);
        start = end;
    }
    Some(())
}

/// Materialized maps can classify just the tail of each decoded row window.
/// Retain at most `width + 2` characters, reusing the allocation and any
/// lookahead after a split. This avoids classifying every source character or
/// retaining a character vector proportional to an enormous logical line.
/// Tiny widths favor the count visitor, and very wide widths keep its constant
/// scratch rather than reserving an unbounded window. Worker counts continue
/// using the independently cancellable visitor above.
fn visit_unicode_wrap_map(
    mut chars: impl Iterator<Item = char>,
    len: usize,
    width: usize,
    mut emit: impl FnMut(usize, usize),
) {
    if len <= width {
        emit(0, len);
        return;
    }
    if !(32..=4_094).contains(&width) {
        visit_unicode_wrap_ranges(chars, len, width, &|| false, emit)
            .expect("uncancelled line wrapping");
        return;
    }
    let capacity = len.min(width + 2);
    let mut buffer = Vec::with_capacity(capacity);
    let mut begin = 0;
    let mut start = 0;
    let mut loaded = 0;
    while start < len {
        if len - start <= width {
            emit(start, len);
            break;
        }
        let target = (start + width).saturating_add(2).min(len);
        let missing = target - loaded;
        if buffer.len() + missing > capacity {
            buffer.copy_within(begin.., 0);
            buffer.truncate(buffer.len() - begin);
            begin = 0;
        }
        buffer.extend(chars.by_ref().take(missing));
        loaded = target;
        let window = &buffer[begin..];
        let boundary = window[..width]
            .iter()
            .rposition(|&ch| unicode_wrap_break(ch));
        let mut end = boundary.map_or(width, |index| index + 1);
        if boundary.is_some() && window[end - 1].is_whitespace() {
            while end < window.len() && end <= width + 1 && window[end].is_whitespace() {
                end += 1;
            }
            if end > width + 1 {
                end = width;
            }
        }
        emit(start, start + end);
        start += end;
        begin += end;
    }
}

impl Editor {
    pub(crate) fn effective_wrap_width(&self) -> usize {
        let viewport = self.viewport_width.max(1);
        if self.word_wrap_column > 0 {
            self.word_wrap_column.min(viewport)
        } else {
            viewport
        }
    }

    /// Retain unaffected line ranges and update only the edited row counts.
    /// Structural/wide edits still use the cancellable document worker.
    pub(crate) fn refresh_wrap_caches(&mut self) {
        if self.wrap_cache_version == self.content_version && self.wrap_dirty_range.is_none() {
            return;
        }
        self.cached_wrapped_total = None;
        let same_lines = self.wrap_cache_line_count == self.line_count();
        let dirty = self
            .wrap_dirty_range
            .take()
            .filter(|_| same_lines)
            .map(|(start, end)| {
                (
                    start.saturating_sub(1).min(self.line_count() - 1),
                    end.max(start).saturating_add(1).min(self.line_count() - 1),
                )
            });
        if let Some((start, end)) = dirty {
            // Seek directly to affected rows, including every cached width
            // and tab size. Typing must not scan thousands of visited lines.
            let invalid: Vec<_> = self.wrap_cache
                .range((start, 0, 0)..=(end, usize::MAX, usize::MAX))
                .map(|(&key, _)| key).collect();
            for key in invalid { self.wrap_cache.remove(&key); }
            let bounded = end - start < MAX_INCREMENTAL_WRAP_LINES
                && self.rope.line_to_byte(end + 1) - self.rope.line_to_byte(start)
                    <= MAX_INCREMENTAL_WRAP_BYTES;
            if bounded {
                if let Some((width, mut offsets)) = self.cached_row_offsets.take() {
                    if offsets.line_count() == self.line_count() {
                        for line in start..=end {
                            let rows = if self.is_line_folded(line) {
                                0
                            } else {
                                Self::wrap_row_count_for_rope_line(self.rope.line(line), width)
                            };
                            offsets.set_line_rows(line, rows);
                        }
                        self.cached_wrapped_total = Some((width, offsets.total));
                        self.cached_row_offsets = Some((width, offsets));
                    }
                }
            } else {
                self.cached_row_offsets = None;
                self.cached_wrapped_total = None;
            }
        } else {
            self.wrap_cache.clear();
            self.cached_row_offsets = None;
            self.cached_wrapped_total = None;
        }
        self.wrap_cache_version = self.content_version;
        self.wrap_cache_line_count = self.line_count();
    }

    fn wrap_row_count_for_rope_line(line: ropey::RopeSlice<'_>, width: usize) -> usize {
        Self::wrap_row_count_for_rope_line_until(line, width, &|| false)
            .expect("uncancelled line wrapping")
    }

    fn wrap_row_count_for_rope_line_until(
        line: ropey::RopeSlice<'_>,
        width: usize,
        cancelled: &impl Fn() -> bool,
    ) -> Option<usize> {
        let width = width.max(1);
        let clean_len = line.len_chars()
            - line
                .chars_at(line.len_chars())
                .reversed()
                .take_while(|&c| c == '\n' || c == '\r')
                .count();
        if clean_len <= width {
            return Some(1);
        }
        // Rope metadata detects ASCII without a separate document scan.
        if line.len_bytes() == line.len_chars() {
            count_ascii_wrap_rows(line.chunks(), clean_len, width, cancelled, ascii_wrap_break)
        } else {
            count_unicode_wrap_rows(
                line.chunks().flat_map(str::chars),
                clean_len,
                width,
                cancelled,
            )
        }
    }

    /// Invalidate all layout/wrap caches after a full rope replacement.
    /// Call after `self.rope = Rope::from_str(...)` to ensure stale cached counts are discarded.
    pub(crate) fn invalidate_rope_caches(&mut self) {
        self.mark_syntax_dirty_range(0, self.rope.len_lines());
        self.content_version += 1;
        self.cached_visible_line_count = None;
        self.cached_wrapped_total = None;
        self.cached_row_offsets = None;
    }

    pub(crate) fn line_len(&self, line_idx: usize) -> usize {
        if line_idx >= self.rope.len_lines() {
            return 0;
        }
        Self::line_content_len(self.rope.line(line_idx))
    }

    pub(crate) fn line_content_len(line: ropey::RopeSlice<'_>) -> usize {
        let mut len = line.len_chars();
        let mut tail = line.chars_at(len).reversed();
        match tail.next() {
            Some('\n') => { len -= 1; if tail.next() == Some('\r') { len -= 1; } }
            Some('\r') => len -= 1,
            _ => {}
        }
        len
    }

    pub(crate) fn clamp_cursor_col(&mut self) {
        let max_col = self.current_line_len();
        if self.cursor_col > max_col {
            self.cursor_col = max_col;
        }
    }

    pub(crate) fn clamp_cursor(&mut self) {
        let max_line = self.line_count().saturating_sub(1);
        if self.cursor_line > max_line {
            self.cursor_line = max_line;
        }
        self.clamp_cursor_col();
    }

    #[cfg(test)]
    pub(crate) fn wrap_ranges_for_line_text(
        line_text: &str,
        wrap_width: usize,
    ) -> Vec<(usize, usize)> {
        Self::wrap_ranges_for_line_text_tabbed(line_text, wrap_width, 4)
    }

    /// Word-wrap a line using display column widths.
    /// Tabs are treated as 1 display column (matching the editor rendering).
    pub(crate) fn wrap_ranges_for_line_text_tabbed(
        line_text: &str,
        wrap_width: usize,
        _tab_size: usize,
    ) -> Vec<(usize, usize)> {
        let width = wrap_width.max(1);
        // Byte length bounds character length too. Most source lines fit without
        // wrapping, including Unicode lines; don't build a character vector.
        if line_text.len() <= width {
            return vec![(0, line_text.chars().count())];
        }
        // ASCII fast path: bytes == chars, skip Vec<char> allocation
        if line_text.is_ascii() {
            return Self::wrap_ranges_ascii(line_text.as_bytes(), width);
        }
        let mut ranges = Vec::new();
        visit_unicode_wrap_map(
            line_text.chars(),
            line_text.chars().count(),
            width,
            |start, end| {
                ranges.push((start, end));
            },
        );
        ranges
    }

    /// Fast word-wrap for ASCII lines — operates on bytes, no Vec<char> allocation.
    fn wrap_ranges_ascii(bytes: &[u8], width: usize) -> Vec<(usize, usize)> {
        let mut ranges = Vec::new();
        visit_wrap_ranges(
            bytes,
            width,
            |c| c.is_ascii_whitespace(),
            ascii_wrap_break,
            |start, end| {
                ranges.push((start, end));
            },
        );
        ranges
    }

    #[cfg(test)]
    fn wrap_row_count_for_line_text(line_text: &str, width: usize) -> usize {
        if line_text.len() <= width {
            return 1;
        }
        let mut count = 0;
        if line_text.is_ascii() {
            visit_wrap_ranges(
                line_text.as_bytes(),
                width,
                |c| c.is_ascii_whitespace(),
                ascii_wrap_break,
                |_, _| count += 1,
            );
        } else {
            let chars: Vec<char> = line_text.chars().collect();
            visit_wrap_ranges(
                &chars,
                width,
                char::is_whitespace,
                unicode_wrap_break,
                |_, _| count += 1,
            );
        }
        count
    }

    pub(crate) fn wrap_ranges_for_line(
        &mut self,
        line_idx: usize,
        wrap_width: usize,
    ) -> std::rc::Rc<Vec<(usize, usize)>> {
        if line_idx >= self.line_count() {
            return std::rc::Rc::new(vec![(0, 0)]);
        }
        self.refresh_wrap_caches();
        let key = (line_idx, wrap_width, self.tab_size);
        if let Some(cached) = self.wrap_cache.get(&key) {
            return std::rc::Rc::clone(cached);
        }

        let line_slice = self.rope.line(line_idx);
        let ranges = if let Some(text) = line_slice.as_str() {
            Self::wrap_ranges_for_line_text_tabbed(
                text.trim_end_matches(['\n', '\r']),
                wrap_width,
                self.tab_size,
            )
        } else {
            // Long lines span Rope chunks. Segment the borrowed chunks once;
            // neither a complete String nor a document-sized Vec<char> is
            // needed for the range map used by painting/pointer navigation.
            let len = line_slice.len_chars()
                - line_slice.chars_at(line_slice.len_chars()).reversed()
                    .take_while(|&c| c == '\n' || c == '\r').count();
            let mut ranges = Vec::new();
            if line_slice.len_bytes() == line_slice.len_chars() {
                visit_ascii_wrap_ranges(line_slice.chunks(), len, wrap_width.max(1), &|| false,
                    ascii_wrap_break, |start, end| ranges.push((start, end)))
                    .expect("uncancelled line wrapping");
            } else {
                visit_unicode_wrap_map(line_slice.chunks().flat_map(str::chars), len,
                    wrap_width.max(1), |start, end| ranges.push((start, end)));
            }
            ranges
        };
        let result = std::rc::Rc::new(ranges);
        if self.wrap_cache.len() >= MAX_CACHED_WRAP_LINES {
            // Keep nearby rows in either browsing direction. Always evicting
            // the first keys would discard recent rows when scrolling upward.
            // Reclaim just the next slot. Dropping half the map in one input
            // frame caused periodic stalls after browsing a large document.
            #[cfg(test)]
            let keep = if REFERENCE_WRAP_EVICTION.get() { 5000 } else { MAX_CACHED_WRAP_LINES - 1 };
            #[cfg(not(test))]
            let keep = MAX_CACHED_WRAP_LINES - 1;
            while self.wrap_cache.len() > keep {
                let first = self.wrap_cache.first_key_value().unwrap().0.0;
                let last = self.wrap_cache.last_key_value().unwrap().0.0;
                if line_idx.abs_diff(first) > line_idx.abs_diff(last) {
                    self.wrap_cache.pop_first();
                } else {
                    self.wrap_cache.pop_last();
                }
            }
        }
        self.wrap_cache.insert(key, std::rc::Rc::clone(&result));
        result
    }

    /// Build an exact cumulative wrapped-row map from an immutable Rope
    /// snapshot. Large documents call this on a background worker.
    pub(crate) fn build_wrap_row_offsets(
        rope: &ropey::Rope,
        wrap_width: usize,
        _tab_size: usize,
        folded_ranges: &[(usize, usize)],
        is_cancelled: impl Fn() -> bool,
    ) -> Option<Vec<usize>> {
        if is_cancelled() {
            return None;
        }
        let width = wrap_width.max(1);
        let total_lines = rope.len_lines();
        let mut offsets = Vec::with_capacity(total_lines + 1);
        let mut cumulative = 0usize;
        let mut line = 0usize;
        let mut fold_index = 0usize;
        let mut lines = rope.lines();

        while line < total_lines {
            if line % 128 == 0 && is_cancelled() {
                return None;
            }
            while folded_ranges
                .get(fold_index)
                .is_some_and(|(start, _)| *start < line)
            {
                fold_index += 1;
            }

            offsets.push(cumulative);
            let line_slice = lines.next()?;
            cumulative +=
                Self::wrap_row_count_for_rope_line_until(line_slice, width, &is_cancelled)?;

            if let Some((start, end)) = folded_ranges.get(fold_index) {
                if *start == line {
                    let fold_end = (*end).clamp(line + 1, total_lines);
                    while offsets.len() < fold_end {
                        if offsets.len() % WRAP_CANCELLATION_WORK == 0 && is_cancelled() {
                            return None;
                        }
                        offsets.push(cumulative);
                    }
                    line = fold_end;
                    lines = rope.lines_at(line);
                    fold_index += 1;
                    continue;
                }
            }
            line += 1;
        }

        while offsets.len() < total_lines {
            if offsets.len() % WRAP_CANCELLATION_WORK == 0 && is_cancelled() {
                return None;
            }
            offsets.push(cumulative);
        }
        offsets.push(cumulative);
        (!is_cancelled()).then_some(offsets)
    }

    pub(crate) fn wrap_layout_ready(&self, wrap_width: usize) -> bool {
        let width = wrap_width.max(1);
        self.wrap_dirty_range.is_none()
            && self.wrap_cache_version == self.content_version
            && self
                .cached_row_offsets
                .as_ref()
                .is_some_and(|(cached_width, offsets)| {
                    *cached_width == width && offsets.line_count() == self.line_count()
                })
    }

    pub(crate) fn needs_background_wrap_layout(&self, wrap_width: usize) -> bool {
        !self.can_build_wrap_layout_synchronously() && !self.wrap_layout_ready(wrap_width)
    }

    fn can_build_wrap_layout_synchronously(&self) -> bool {
        self.line_count() <= MAX_SYNC_WRAP_LAYOUT_LINES
            && self.rope.len_bytes() <= MAX_SYNC_WRAP_LAYOUT_BYTES
    }

    pub(crate) fn install_wrap_layout(
        &mut self,
        wrap_width: usize,
        offsets: impl Into<WrapRowOffsets>,
    ) -> bool {
        let offsets = offsets.into();
        if offsets.line_count() != self.line_count() {
            return false;
        }
        self.refresh_wrap_caches();
        let width = wrap_width.max(1);
        let total = offsets.total;
        self.cached_wrapped_total = Some((width, total));
        self.cached_row_offsets = Some((width, offsets));
        true
    }

    /// Ensure a wrap map exists without allowing a large document scan to run
    /// on the input/render thread. `false` selects the cheap logical-line
    /// fallback until the background result is installed.
    pub(crate) fn ensure_row_offsets(&mut self, wrap_width: usize) -> bool {
        self.refresh_wrap_caches();
        let width = wrap_width.max(1);
        if self.wrap_layout_ready(width) {
            return true;
        }
        if !self.can_build_wrap_layout_synchronously() {
            return false;
        }

        let folded_ranges: Vec<_> = self
            .folded_ranges
            .iter()
            .map(|(&start, &end)| (start, end))
            .collect();
        let Some(offsets) =
            Self::build_wrap_row_offsets(&self.rope, width, self.tab_size, &folded_ranges, || {
                false
            })
        else {
            return false;
        };
        self.install_wrap_layout(width, offsets)
    }

    pub(crate) fn wrapped_total_rows(&mut self, wrap_width: usize) -> usize {
        self.refresh_wrap_caches();
        let width = wrap_width.max(1);
        if let Some((cached_w, cached_total)) = self.cached_wrapped_total {
            if cached_w == width {
                return cached_total;
            }
        }
        if !self.ensure_row_offsets(width) {
            return self.line_count();
        }
        self.cached_row_offsets
            .as_ref()
            .map(|(_, offsets)| offsets.total)
            .unwrap_or(0)
    }

    pub(crate) fn wrapped_row_at(
        &mut self,
        target_row: usize,
        wrap_width: usize,
    ) -> Option<WrappedRowInfo> {
        let width = wrap_width.max(1);
        if !self.ensure_row_offsets(width) {
            return self.unwrapped_row_at(target_row);
        }

        // The prefix tree finds the containing line in O(log n), even after
        // local edits have changed its wrapped height.
        if let Some((cached_w, ref offsets)) = self.cached_row_offsets {
            if cached_w == width {
                let (line, base_row) = offsets.line_for_row(target_row)?;
                let seg_in_line = target_row - base_row;
                let ranges = self.wrap_ranges_for_line(line, width);
                if seg_in_line < ranges.len() {
                    let (start_col, end_col) = ranges[seg_in_line];
                    return Some(WrappedRowInfo {
                        line_idx: line,
                        start_col,
                        end_col,
                        line_len: self.wrapped_line_len(line, &ranges),
                        is_first: seg_in_line == 0,
                    });
                }
                return None;
            }
        }

        self.unwrapped_row_at(target_row)
    }

    pub(crate) fn wrapped_rows(
        &mut self,
        start_row: usize,
        count: usize,
        wrap_width: usize,
    ) -> Vec<WrappedRowInfo> {
        let mut rows = Vec::with_capacity(count);
        self.visit_wrapped_rows(start_row, count, wrap_width, |_, row| rows.push(row));
        rows
    }

    /// Stream a row range into a caller's retained storage.
    pub(crate) fn visit_wrapped_rows(
        &mut self,
        start_row: usize,
        count: usize,
        wrap_width: usize,
        mut visit: impl FnMut(&Self, WrappedRowInfo),
    ) {
        let width = wrap_width.max(1);
        if count == 0 {
            return;
        }
        if !self.ensure_row_offsets(width) {
            self.visit_unwrapped_rows(start_row, count, |row| visit(self, row));
            return;
        }

        // Use cached row offsets to skip directly to the starting line
        let (start_line, start_seg) = if let Some((cached_w, ref offsets)) = self.cached_row_offsets
        {
            if cached_w == width {
                let Some((line, base_row)) = offsets.line_for_row(start_row) else {
                    return;
                };
                (line, start_row - base_row)
            } else {
                (0, 0) // fallback
            }
        } else {
            (0, 0) // fallback
        };

        let mut emitted = 0;
        let total_lines = self.line_count();
        let mut line = start_line;
        let mut first_line = true;
        while line < total_lines && emitted < count {
            if let Some((_, fold_end)) = self.folded_range_containing(line) {
                line = fold_end;
                first_line = false;
                continue;
            }
            let ranges = self.wrap_ranges_for_line(line, width);
            let line_len = self.wrapped_line_len(line, &ranges);
            let skip = if first_line { start_seg } else { 0 };
            // Deep scrolling into one long logical line must seek directly to
            // the first visible segment, not revisit every preceding segment.
            for (seg, (start_col, end_col)) in ranges.iter().copied().enumerate().skip(skip) {
                visit(self, WrappedRowInfo {
                    line_idx: line,
                    start_col,
                    end_col,
                    line_len,
                    is_first: seg == 0,
                });
                emitted += 1;
                if emitted >= count {
                    break;
                }
            }
            if let Some(&end) = self.folded_ranges.get(&line) {
                line = end;
            } else {
                line += 1;
            }
            first_line = false;
        }
    }

    pub(crate) fn wrapped_row_for_position(
        &mut self,
        target_line: usize,
        target_col: usize,
        wrap_width: usize,
    ) -> usize {
        let width = wrap_width.max(1);
        let total_lines = self.line_count();
        let clamped_target_line = target_line.min(total_lines.saturating_sub(1));

        // Prefix lookup avoids updating every later offset after a local edit.
        if !self.ensure_row_offsets(width) {
            return clamped_target_line;
        }
        let base_row = if let Some((cached_w, ref offsets)) = self.cached_row_offsets {
            if cached_w == width && clamped_target_line < offsets.line_count() {
                offsets.row_for_line(clamped_target_line)
            } else {
                0
            }
        } else {
            0
        };

        // Now just compute the segment offset within this line
        if self.is_line_folded(clamped_target_line) {
            return base_row;
        }
        let ranges = self.wrap_ranges_for_line(clamped_target_line, width);
        let line_len = self.wrapped_line_len(clamped_target_line, &ranges);
        let segments = ranges.len();
        let col = target_col.min(line_len);
        let seg = ranges.partition_point(|&(_, end)| end <= col);
        base_row + seg.min(segments.saturating_sub(1))
    }

    /// The final segment already carries the content length; resolving it
    /// again through the Rope duplicated tree walks on every wrapped move.
    fn wrapped_line_len(&self, _line: usize, ranges: &[(usize, usize)]) -> usize {
        #[cfg(test)]
        if REFERENCE_WRAP_LOOKUPS.get() { return self.line_len(_line); }
        ranges.last().map_or(0, |&(_, end)| end)
    }

    pub(crate) fn wrapped_position_for_screen(
        &mut self,
        start_row: usize,
        screen_row: usize,
        screen_col: usize,
        wrap_width: usize,
    ) -> (usize, usize) {
        let target_row = start_row.saturating_add(screen_row);
        if let Some(row) = self.wrapped_row_at(target_row, wrap_width) {
            let col = self.source_column_at_cell(row.line_idx, row.start_col, row.end_col, screen_col).0;
            return (row.line_idx, col);
        }
        let line = self.line_count().saturating_sub(1);
        (line, self.line_len(line))
    }

    pub(crate) fn sync_line_scroll_from_wrap(&mut self, wrap_width: usize) {
        let width = wrap_width.max(1);
        // Scroll synchronization only needs a logical line. Materializing all
        // wrapped ranges of a multi-megabyte line here duplicated paint work
        // and could stall the input thread immediately after installing a map.
        let top_line = if self.ensure_row_offsets(width) {
            self.cached_row_offsets.as_ref()
                .and_then(|(_, offsets)| offsets.line_for_row(self.wrap_scroll_offset))
                .map_or(self.cursor_line, |(line, _)| line)
        } else {
            self.unwrapped_row_at(self.wrap_scroll_offset)
                .map_or(self.cursor_line, |row| row.line_idx)
        };
        self.scroll_offset = top_line;
    }

    pub(crate) fn set_wrap_scroll_offset(&mut self, offset: usize) {
        let wrap_width = self.effective_wrap_width();
        let max_wrap_row = self.wrapped_total_rows(wrap_width).saturating_sub(1);
        self.wrap_scroll_offset = offset.min(max_wrap_row);
        self.sync_line_scroll_from_wrap(wrap_width);
        self.horizontal_scroll = 0;
    }

    fn unwrapped_row_at(&self, target_row: usize) -> Option<WrappedRowInfo> {
        if target_row >= self.line_count() {
            return None;
        }
        let mut line = target_row;
        while let Some((_, fold_end)) = self.folded_range_containing(line) {
            line = fold_end;
        }
        if line >= self.line_count() {
            return None;
        }
        let line_len = self.line_len(line);
        Some(WrappedRowInfo {
            line_idx: line,
            start_col: 0,
            end_col: line_len,
            line_len,
            is_first: true,
        })
    }

    fn visit_unwrapped_rows(
        &self,
        start_row: usize,
        count: usize,
        mut visit: impl FnMut(WrappedRowInfo),
    ) {
        let mut emitted = 0;
        let mut line = start_row;
        while line < self.line_count() && emitted < count {
            if let Some((_, &end)) = self.folded_ranges.range(..line).next_back() {
                if line < end {
                    line = end;
                    continue;
                }
            }
            let line_len = self.line_len(line);
            visit(WrappedRowInfo {
                line_idx: line,
                start_col: 0,
                end_col: line_len,
                line_len,
                is_first: true,
            });
            emitted += 1;
            line = self.folded_ranges.get(&line).copied().unwrap_or(line + 1);
        }
    }

    /// Clear all folds when line structure changes (insert/delete/paste multi-line).
    pub(crate) fn clear_stale_folds(&mut self) {
        self.fold_buttons.clear();
        if !self.folded_ranges.is_empty() {
            self.folded_ranges.clear();
            self.cached_visible_line_count = None;
            self.cached_wrapped_total = None;
            self.cached_row_offsets = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    #[test]
    fn configured_column_clamping_reuses_exact_tree_across_wider_viewports() {
        for lines in [10, 3_000] {
            let mut editor =
                Editor::test_with_text(&format!("{}\n", "word ".repeat(30)).repeat(lines));
            editor.word_wrap = true;
            editor.word_wrap_column = 40;
            editor.viewport_width = 160;
            let offsets =
                Editor::build_wrap_row_offsets(&editor.rope, 40, editor.tab_size, &[], || false)
                    .unwrap();
            assert!(editor.install_wrap_layout(40, offsets));
            let tree = editor.cached_row_offsets.as_ref().unwrap().1.tree.as_ptr();
            let total = editor.wrapped_total_rows(40);
            for viewport in [160, 90, 50, 140] {
                editor.viewport_width = viewport;
                assert_eq!(editor.effective_wrap_width(), 40);
                editor.set_wrap_scroll_offset(usize::MAX);
                assert_eq!(editor.wrap_scroll_offset, total - 1);
                assert!(editor.wrap_layout_ready(40));
                assert_eq!(
                    editor.cached_row_offsets.as_ref().unwrap().1.tree.as_ptr(),
                    tree
                );
                assert_eq!(editor.scroll_offset, editor.line_count() - 1);
            }
        }
    }

    #[test]
    fn row_prefix_tree_updates_and_selects_zero_height_folded_lines() {
        let mut counts = vec![2, 0, 0, 3, 1, 0, 4];
        let prefixes = |counts: &[usize]| {
            let mut sum = 0;
            std::iter::once(0)
                .chain(counts.iter().map(|&count| {
                    sum += count;
                    sum
                }))
                .collect::<Vec<_>>()
        };
        let mut offsets = WrapRowOffsets::from(prefixes(&counts));
        for (line, rows) in [(0, 1), (3, 8), (6, 0), (1, 2), (3, 0)] {
            offsets.set_line_rows(line, rows);
            counts[line] = rows;
            let expected = prefixes(&counts);
            assert_eq!(offsets.total, *expected.last().unwrap());
            for (line, &row) in expected.iter().enumerate() {
                assert_eq!(offsets.row_for_line(line), row);
            }
            for row in 0..offsets.total {
                let line = expected.partition_point(|&base| base <= row) - 1;
                assert_eq!(offsets.line_for_row(row), Some((line, expected[line])));
            }
            assert_eq!(offsets.line_for_row(offsets.total), None);
        }
    }

    #[test]
    fn local_edits_retain_other_line_ranges_and_update_exact_row_positions() {
        let mut editor = Editor::test_with_text(&"abcdefghij\n".repeat(2_000));
        editor.word_wrap = true;
        editor.viewport_width = 5;
        assert!(editor.ensure_row_offsets(5));
        let unaffected = editor.wrap_ranges_for_line(10, 5);
        let affected = editor.wrap_ranges_for_line(100, 5);
        let tree_ptr = editor.cached_row_offsets.as_ref().unwrap().1.tree.as_ptr();
        editor.set_cursor(100, 4);
        editor.insert_char('界');
        assert!(editor.wrap_layout_ready(5));
        assert_eq!(
            editor.cached_row_offsets.as_ref().unwrap().1.tree.as_ptr(),
            tree_ptr
        );
        assert!(std::rc::Rc::ptr_eq(
            &unaffected,
            &editor.wrap_ranges_for_line(10, 5)
        ));
        assert!(!std::rc::Rc::ptr_eq(
            &affected,
            &editor.wrap_ranges_for_line(100, 5)
        ));
        let expected = Editor::build_wrap_row_offsets(&editor.rope, 5, 4, &[], || false).unwrap();
        let actual = &editor.cached_row_offsets.as_ref().unwrap().1;
        for (line, &row) in expected.iter().enumerate() {
            assert_eq!(actual.row_for_line(line), row);
        }
        editor.undo();
        assert_eq!(editor.wrapped_total_rows(5), 4_001);
    }

    #[test]
    fn large_document_keeps_exact_wrapping_during_local_typing() {
        let mut editor = Editor::test_with_text(&"abcdefghij\n".repeat(50_000));
        editor.word_wrap = true;
        editor.viewport_width = 5;
        let offsets = Editor::build_wrap_row_offsets(&editor.rope, 5, 4, &[], || false).unwrap();
        assert!(editor.install_wrap_layout(5, offsets));
        let ptr = editor.cached_row_offsets.as_ref().unwrap().1.tree.as_ptr();
        editor.set_cursor(25_000, 3);
        for c in "XYZ".chars() {
            editor.insert_char(c);
        }
        assert!(editor.wrap_layout_ready(5));
        assert!(!editor.needs_background_wrap_layout(5));
        assert_eq!(
            editor.cached_row_offsets.as_ref().unwrap().1.tree.as_ptr(),
            ptr
        );
        assert_eq!(editor.wrapped_total_rows(5), 100_002);
        assert_eq!(editor.wrapped_row_for_position(25_001, 0, 5), 50_003);
        editor.insert_newline();
        assert!(!editor.wrap_layout_ready(5));
        assert!(editor.needs_background_wrap_layout(5));
    }

    #[test]
    fn fold_commands_invalidate_warm_wrap_map_and_restore_exact_rows() {
        let mut editor = Editor::test_with_text("root\n  child\n  next\nend\n");
        editor.word_wrap = true;
        assert_eq!(editor.wrapped_total_rows(80), 5);
        editor.toggle_fold(0);
        assert_eq!(editor.wrapped_total_rows(80), 3);
        editor.unfold_all();
        assert_eq!(editor.wrapped_total_rows(80), 5);
        editor.fold_all();
        assert_eq!(editor.wrapped_total_rows(80), 3);
        editor.unfold_all();
        editor.fold_at_level(1);
        assert_eq!(editor.wrapped_total_rows(80), 3);
    }

    #[test]
    fn incremental_wrap_matches_full_layout_through_mixed_edits_history_and_folds() {
        let mut editor =
            Editor::test_with_text(&"fn test() {\n    let 界 = 123;  \n}\n".repeat(32));
        editor.word_wrap = true;
        editor.language_override = Some("Rust".into());
        let mut random = 0xabcdef12u64;
        for step in 0..180 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let width = [5, 11, 23][(step / 20) % 3];
            editor.viewport_width = width;
            assert!(editor.ensure_row_offsets(width));
            let line = (random >> 32) as usize % editor.line_count();
            let col = (random as usize % 13).min(editor.line_len(line));
            editor.set_cursor(line, col);
            match step % 16 {
                0 => editor.insert_char('界'),
                1 => editor.backspace(),
                2 => editor.delete_char(),
                3 => editor.insert_newline(),
                4 => editor.undo(),
                5 => editor.redo(),
                6 => editor.toggle_fold(line),
                7 => editor.unfold_all(),
                8 => editor.indent_lines(),
                9 => editor.dedent_line(),
                10 => editor.toggle_line_comment(),
                11 => editor.convert_indentation_to_tabs(),
                12 => {
                    editor.trim_trailing_whitespace();
                }
                13 => editor.move_line_down(),
                14 => editor.move_line_up(),
                _ => editor.join_lines(),
            }
            assert!(editor.ensure_row_offsets(width));
            let folds: Vec<_> = editor.folded_ranges.iter().map(|(&a, &b)| (a, b)).collect();
            let expected = Editor::build_wrap_row_offsets(
                &editor.rope,
                width,
                editor.tab_size,
                &folds,
                || false,
            )
            .unwrap();
            let tree = &editor.cached_row_offsets.as_ref().unwrap().1;
            for (line, &row) in expected.iter().enumerate() {
                assert_eq!(tree.row_for_line(line), row, "step {step} line {line}");
            }
            for row in [0, tree.total / 2, tree.total.saturating_sub(1)] {
                let expected_line = expected.partition_point(|&base| base <= row) - 1;
                assert_eq!(
                    tree.line_for_row(row),
                    Some((expected_line, expected[expected_line])),
                    "step {step}"
                );
            }
        }
    }

    #[test]
    #[ignore = "manual release incremental wrap benchmark"]
    fn benchmark_incremental_wrap_typing() {
        use std::{hint::black_box, time::Instant};
        let mut editor = Editor::test_with_text(&"abcdefghij\n".repeat(50_000));
        editor.word_wrap = true;
        editor.viewport_width = 5;
        let offsets = Editor::build_wrap_row_offsets(&editor.rope, 5, 4, &[], || false).unwrap();
        editor.install_wrap_layout(5, offsets);
        editor.set_cursor(25_000, 4);
        let mut reference = Vec::new();
        let mut incremental = Vec::new();
        for sample in 0..7 {
            for optimized in [sample % 2 == 0, sample % 2 != 0] {
                let started = Instant::now();
                for _ in 0..16 {
                    editor.insert_char('x');
                    if optimized {
                        black_box(editor.wrapped_total_rows(5));
                    } else {
                        let rebuilt =
                            Editor::build_wrap_row_offsets(&editor.rope, 5, 4, &[], || false)
                                .unwrap();
                        assert_eq!(editor.wrapped_total_rows(5), *rebuilt.last().unwrap());
                        black_box(rebuilt);
                    }
                    editor.backspace();
                }
                if optimized {
                    incremental.push(started.elapsed() / 16);
                } else {
                    reference.push(started.elapsed() / 16);
                }
            }
        }
        reference.sort();
        incremental.sort();
        eprintln!(
            "50k-line exact wrap after typing: rebuild={:?} incremental={:?}",
            reference[3], incremental[3]
        );
    }

    pub(super) fn reference_wrap(text: &str, width: usize) -> Vec<(usize, usize)> {
        let chars: Vec<_> = text.chars().collect();
        let width = width.max(1);
        let mut ranges = Vec::new();
        let mut start = 0;
        if chars.is_empty() {
            return vec![(0, 0)];
        }
        while start < chars.len() {
            if chars.len() - start <= width {
                ranges.push((start, chars.len()));
                break;
            }
            let limit = start + width;
            let boundary = ((start + 1)..=limit)
                .rev()
                .find(|&end| unicode_wrap_break(chars[end - 1]));
            let mut end = boundary.unwrap_or(limit);
            if boundary.is_some() && chars[end - 1].is_whitespace() {
                while end < chars.len() && chars[end].is_whitespace() {
                    end += 1;
                }
                if end > limit + 1 {
                    end = limit;
                }
            }
            ranges.push((start, end));
            start = end;
        }
        ranges
    }

    #[test]
    fn wrap_count_and_bounded_whitespace_scan_preserve_segmentation() {
        for text in [
            "",
            "     ",
            "x  y    z",
            "x-/,;\\ y",
            "界\u{2003} \t界    αβ",
            "a\rb\nc",
        ] {
            for width in 1..=text.len() + 2 {
                let expected = reference_wrap(text, width);
                assert_eq!(
                    Editor::wrap_ranges_for_line_text(text, width),
                    expected,
                    "{text:?} width={width}"
                );
                assert_eq!(
                    Editor::wrap_row_count_for_line_text(text, width),
                    expected.len()
                );
            }
        }
        let text = vec![b' '; 32_768];
        let inspections = std::cell::Cell::new(0);
        let mut rows = 0;
        visit_wrap_ranges(
            &text,
            1,
            |c| {
                inspections.set(inspections.get() + 1);
                c.is_ascii_whitespace()
            },
            ascii_wrap_break,
            |_, _| rows += 1,
        );
        assert_eq!(rows, text.len() - 1); // final two spaces share a row
        assert!(
            inspections.get() <= text.len() * 4,
            "whitespace work must stay linear"
        );
    }

    #[test]
    fn single_large_line_uses_background_wrap_layout() {
        let mut editor = Editor::test_with_text(&"x".repeat(MAX_SYNC_WRAP_LAYOUT_BYTES + 1));
        assert!(editor.needs_background_wrap_layout(80));
        assert!(!editor.ensure_row_offsets(80));
        assert!(editor.cached_row_offsets.is_none());
    }

    #[test]
    fn count_only_wrap_layout_preserves_folds_and_chunk_crossing_lines() {
        let text = format!(
            "{}\r\n{}\n{}\nend",
            "界".repeat(2_000),
            "abc ".repeat(3_000),
            " ".repeat(2_000)
        );
        let rope = Rope::from_str(&text);
        for width in [1, 3, 80, 16_000] {
            for folds in [vec![], vec![(0, 2)], vec![(1, 3)]] {
                let offsets =
                    Editor::build_wrap_row_offsets(&rope, width, 4, &folds, || false).unwrap();
                let mut total = 0;
                for (line, slice) in rope.lines().enumerate() {
                    if folds.iter().any(|&(start, end)| line > start && line < end) {
                        assert_eq!(offsets[line], total);
                    } else {
                        assert_eq!(offsets[line], total);
                        total +=
                            reference_wrap(slice.to_string().trim_end_matches(['\n', '\r']), width)
                                .len();
                    }
                }
                assert_eq!(offsets[rope.len_lines()], total);
            }
        }
    }

    #[test]
    #[ignore = "manual release wrap benchmark"]
    fn benchmark_whitespace_wrap_layout() {
        use std::{hint::black_box, time::Instant};
        for (name, text) in [
            ("ascii", " ".repeat(32_768)),
            ("unicode", "\u{2003}".repeat(32_768)),
        ] {
            let mut before = Vec::new();
            let mut after = Vec::new();
            for sample in 0..7 {
                for optimized in [sample % 2 == 0, sample % 2 != 0] {
                    let started = Instant::now();
                    let ranges = if optimized {
                        Editor::wrap_ranges_for_line_text(black_box(&text), 80)
                    } else {
                        reference_wrap(black_box(&text), 80)
                    };
                    black_box(&ranges);
                    if optimized {
                        after.push(started.elapsed());
                    } else {
                        before.push(started.elapsed());
                    }
                }
            }
            assert_eq!(
                Editor::wrap_ranges_for_line_text(&text, 80),
                reference_wrap(&text, 80)
            );
            before.sort();
            after.sort();
            eprintln!(
                "whitespace wrapping {name}: before={:?} after={:?}",
                before[3], after[3]
            );
        }
    }

    #[test]
    fn test_line_len_simple() {
        let ed = Editor::test_with_text("hello\n");
        assert_eq!(ed.line_len(0), 5);
    }

    #[test]
    fn test_line_len_empty_line() {
        let ed = Editor::test_with_text("hello\n\nworld\n");
        assert_eq!(ed.line_len(1), 0);
    }

    #[test]
    fn test_line_len_crlf() {
        let ed = Editor::test_with_text("hello\r\nworld\r\n");
        assert_eq!(ed.line_len(0), 5);
        assert_eq!(ed.line_len(1), 5);
    }

    #[test]
    fn test_wrap_short_line_no_wrap() {
        let ranges = Editor::wrap_ranges_for_line_text("hello", 80);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0], (0, 5));
    }

    #[test]
    fn test_wrap_long_line_at_word_boundary() {
        // "hello world foo" with wrap_width=10
        // "hello " is 6 chars, word break after space
        let ranges = Editor::wrap_ranges_for_line_text("hello world foo", 10);
        assert!(ranges.len() >= 2);
        // First segment should end at a word boundary (space at position 6)
        assert_eq!(ranges[0].0, 0);
        // All segments together should cover the whole string
        assert_eq!(ranges[0].0, 0);
        assert_eq!(ranges.last().unwrap().1, 15);
    }

    #[test]
    fn test_wrap_cache_invalidation() {
        let mut ed = Editor::test_with_text("hello world this is a test line\n");
        let ranges1 = ed.wrap_ranges_for_line(0, 10);
        let count1 = ranges1.len();
        assert!(count1 > 1);

        // Change content_version to simulate a content edit
        ed.content_version += 1;
        ed.rope = Rope::from_str("hi\n");
        let ranges2 = ed.wrap_ranges_for_line(0, 10);
        assert_eq!(ranges2.len(), 1); // "hi" fits in 10 cols
        assert_ne!(count1, ranges2.len());
    }

    // ── wrap_ranges_for_line_text / wrap_ranges_for_line_text_tabbed ──

    #[test]
    fn wrap_empty_line() {
        let ranges = Editor::wrap_ranges_for_line_text("", 80);
        assert_eq!(ranges, vec![(0, 0)]);
    }

    #[test]
    fn wrap_exact_width() {
        let ranges = Editor::wrap_ranges_for_line_text("12345", 5);
        assert_eq!(ranges, vec![(0, 5)]);
    }

    #[test]
    fn wrap_one_char_over() {
        let ranges = Editor::wrap_ranges_for_line_text("123456", 5);
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].0, 0);
        assert_eq!(ranges.last().unwrap().1, 6);
    }

    #[test]
    fn wrap_width_1() {
        let ranges = Editor::wrap_ranges_for_line_text("abc", 1);
        assert_eq!(ranges.len(), 3);
        assert_eq!(ranges[0], (0, 1));
        assert_eq!(ranges[1], (1, 2));
        assert_eq!(ranges[2], (2, 3));
    }

    #[test]
    fn wrap_width_0_treated_as_1() {
        let ranges = Editor::wrap_ranges_for_line_text_tabbed("ab", 0, 4);
        assert_eq!(ranges.len(), 2);
    }

    #[test]
    fn wrap_preserves_full_coverage() {
        let text = "the quick brown fox jumps over the lazy dog";
        let ranges = Editor::wrap_ranges_for_line_text(text, 12);
        assert_eq!(ranges[0].0, 0);
        assert_eq!(ranges.last().unwrap().1, text.len());
        // No gaps between segments
        for w in ranges.windows(2) {
            assert_eq!(w[0].1, w[1].0);
        }
    }

    #[test]
    fn wrap_at_comma() {
        // Comma is a preferred break point
        let ranges = Editor::wrap_ranges_for_line_text("aaa,bbb,ccc", 5);
        assert!(ranges.len() >= 2);
        // First break should be after comma at position 4
        assert_eq!(ranges[0], (0, 4));
    }

    #[test]
    fn wrap_at_semicolon() {
        let ranges = Editor::wrap_ranges_for_line_text("aaa;bbb;ccc", 5);
        assert!(ranges.len() >= 2);
        assert_eq!(ranges[0], (0, 4));
    }

    #[test]
    fn wrap_at_slash() {
        let ranges = Editor::wrap_ranges_for_line_text("path/to/file/name", 6);
        assert!(ranges.len() >= 2);
        assert_eq!(ranges[0], (0, 5)); // breaks after '/'
    }

    #[test]
    fn wrap_at_backslash() {
        let ranges = Editor::wrap_ranges_for_line_text("path\\to\\file", 6);
        assert!(ranges.len() >= 2);
        assert_eq!(ranges[0], (0, 5)); // breaks after '\'
    }

    #[test]
    fn wrap_at_hyphen() {
        let ranges = Editor::wrap_ranges_for_line_text("long-word-here", 6);
        assert!(ranges.len() >= 2);
        assert_eq!(ranges[0], (0, 5)); // breaks after '-'
    }

    #[test]
    fn wrap_no_break_points_splits_at_width() {
        // No spaces/punctuation — must hard-break at width
        let ranges = Editor::wrap_ranges_for_line_text("abcdefghij", 5);
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0], (0, 5));
        assert_eq!(ranges[1], (5, 10));
    }

    #[test]
    fn wrap_whitespace_consumed() {
        // When breaking at space, trailing whitespace is consumed
        let ranges = Editor::wrap_ranges_for_line_text("hello   world", 8);
        assert!(ranges.len() >= 2);
        assert_eq!(ranges[0].0, 0);
        // First segment should consume trailing spaces
        assert!(ranges[0].1 >= 6); // at least past "hello "
    }

    #[test]
    fn wrap_tab_counts_as_one_column() {
        let ranges = Editor::wrap_ranges_for_line_text_tabbed("\thello", 3, 4);
        // Tab = 1 col, total 6 chars, width 3 → 2 wraps minimum
        assert!(ranges.len() >= 2);
        assert_eq!(ranges[0].0, 0);
        assert_eq!(ranges.last().unwrap().1, 6);
    }

    #[test]
    fn wrap_unicode_chars() {
        // Each Unicode char is 1 char
        let ranges = Editor::wrap_ranges_for_line_text("αβγδε", 3);
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0], (0, 3));
        assert_eq!(ranges[1], (3, 5));
    }

    #[test]
    fn wrap_very_long_line() {
        let text = "x".repeat(1000);
        let ranges = Editor::wrap_ranges_for_line_text(&text, 80);
        assert_eq!(ranges.len(), 13); // 1000/80 = 12.5, ceil = 13
        assert_eq!(ranges[0], (0, 80));
        assert_eq!(ranges.last().unwrap().1, 1000);
    }

    // ── line_len ─────────────────────────────────────────────────────

    #[test]
    fn line_len_out_of_bounds() {
        let ed = Editor::test_with_text("hello\n");
        assert_eq!(ed.line_len(999), 0);
    }

    #[test]
    fn line_len_no_trailing_newline() {
        let ed = Editor::test_with_text("hello");
        assert_eq!(ed.line_len(0), 5);
    }

    #[test]
    fn line_len_multiple_lines() {
        let ed = Editor::test_with_text("abc\ndefgh\nij\n");
        assert_eq!(ed.line_len(0), 3);
        assert_eq!(ed.line_len(1), 5);
        assert_eq!(ed.line_len(2), 2);
    }

    // ── clamp_cursor ─────────────────────────────────────────────────

    #[test]
    fn clamp_cursor_within_bounds() {
        let mut ed = Editor::test_with_text("hello\nworld\n");
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        ed.clamp_cursor();
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 3);
    }

    #[test]
    fn clamp_cursor_line_beyond_end() {
        let mut ed = Editor::test_with_text("hello\n");
        ed.cursor_line = 999;
        ed.cursor_col = 0;
        ed.clamp_cursor();
        assert_eq!(ed.cursor_line, 1); // line count = 2 (hello\n + empty), max = 1
    }

    #[test]
    fn clamp_cursor_col_beyond_line() {
        let mut ed = Editor::test_with_text("hi\n");
        ed.cursor_line = 0;
        ed.cursor_col = 100;
        ed.clamp_cursor_col();
        assert_eq!(ed.cursor_col, 2);
    }

    // ── wrap_ranges_for_line (cached) ────────────────────────────────

    #[test]
    fn wrap_ranges_for_line_beyond_line_count() {
        let mut ed = Editor::test_with_text("hello\n");
        let ranges = ed.wrap_ranges_for_line(999, 80);
        assert_eq!(*ranges, vec![(0, 0)]);
    }

    #[test]
    fn wrap_ranges_cached_returns_same_rc() {
        let mut ed = Editor::test_with_text("hello world this is a test\n");
        let r1 = ed.wrap_ranges_for_line(0, 10);
        let r2 = ed.wrap_ranges_for_line(0, 10);
        assert!(std::rc::Rc::ptr_eq(&r1, &r2));
    }

    // ── wrapped_total_rows / wrapped_row_at ──────────────────────────

    #[test]
    fn wrapped_total_rows_single_short_line() {
        let mut ed = Editor::test_with_text("hi\n");
        assert_eq!(ed.wrapped_total_rows(80), 2); // "hi" + empty line
    }

    #[test]
    fn wrapped_total_rows_with_wrapping() {
        let mut ed = Editor::test_with_text("abcdefghij\n"); // 10 chars + empty
        // Width 5: line 0 wraps to 2 rows, line 1 = 1 row
        assert_eq!(ed.wrapped_total_rows(5), 3);
    }

    #[test]
    fn large_document_wrap_map_is_never_built_on_the_caller_thread() {
        let text = "0123456789\n".repeat(MAX_SYNC_WRAP_LAYOUT_LINES + 1);
        let mut ed = Editor::test_with_text(&text);

        // Until the worker result arrives, layout deliberately uses one cheap
        // visual row per logical line instead of scanning the entire Rope.
        assert_eq!(ed.wrapped_total_rows(5), ed.line_count());
        assert!(!ed.wrap_layout_ready(5));

        let offsets = Editor::build_wrap_row_offsets(&ed.rope, 5, ed.tab_size, &[], || false)
            .expect("uncancelled wrap layout");
        let exact_total = offsets.last().copied().expect("sentinel");
        assert!(exact_total > ed.line_count());
        assert!(ed.install_wrap_layout(5, offsets));
        assert_eq!(ed.wrapped_total_rows(5), exact_total);
    }

    #[test]
    fn background_wrap_map_honors_cancellation() {
        let rope = Rope::from_str(&"0123456789\n".repeat(300));
        assert!(Editor::build_wrap_row_offsets(&rope, 5, 4, &[], || true).is_none());
    }

    #[test]
    fn wrapped_row_at_first_row() {
        let mut ed = Editor::test_with_text("hello\n");
        let row = ed.wrapped_row_at(0, 80).unwrap();
        assert_eq!(row.line_idx, 0);
        assert_eq!(row.start_col, 0);
        assert_eq!(row.end_col, 5);
        assert!(row.is_first);
    }

    #[test]
    fn wrapped_row_at_beyond_total() {
        let mut ed = Editor::test_with_text("hi\n");
        assert!(ed.wrapped_row_at(999, 80).is_none());
    }

    #[test]
    fn wrapped_row_at_wrapped_second_segment() {
        let mut ed = Editor::test_with_text("abcdefghij\n"); // 10 chars
        // Width 5: row 0 = (0,5), row 1 = (5,10)
        let row = ed.wrapped_row_at(1, 5).unwrap();
        assert_eq!(row.line_idx, 0);
        assert_eq!(row.start_col, 5);
        assert_eq!(row.end_col, 10);
        assert!(!row.is_first);
    }

    // ── wrapped_rows ─────────────────────────────────────────────────

    #[test]
    fn wrapped_rows_count_zero() {
        let mut ed = Editor::test_with_text("hello\n");
        let rows = ed.wrapped_rows(0, 0, 80);
        assert!(rows.is_empty());
    }

    #[test]
    fn wrapped_rows_returns_requested_count() {
        let mut ed = Editor::test_with_text("line1\nline2\nline3\n");
        let rows = ed.wrapped_rows(0, 2, 80);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].line_idx, 0);
        assert_eq!(rows[1].line_idx, 1);
    }

    #[test]
    fn wrapped_rows_with_offset() {
        let mut ed = Editor::test_with_text("aaa\nbbb\nccc\n");
        let rows = ed.wrapped_rows(1, 2, 80);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].line_idx, 1);
        assert_eq!(rows[1].line_idx, 2);
    }

    // ── wrapped_row_for_position ─────────────────────────────────────

    #[test]
    fn wrapped_row_for_position_no_wrap() {
        let mut ed = Editor::test_with_text("aaa\nbbb\nccc\n");
        assert_eq!(ed.wrapped_row_for_position(0, 0, 80), 0);
        assert_eq!(ed.wrapped_row_for_position(1, 0, 80), 1);
        assert_eq!(ed.wrapped_row_for_position(2, 0, 80), 2);
    }

    #[test]
    fn wrapped_row_for_position_with_wrap() {
        let mut ed = Editor::test_with_text("abcdefghij\nxyz\n");
        // Line 0 at width 5: rows 0,1. Line 1: row 2.
        assert_eq!(ed.wrapped_row_for_position(0, 0, 5), 0);
        assert_eq!(ed.wrapped_row_for_position(0, 7, 5), 1);
        assert_eq!(ed.wrapped_row_for_position(1, 0, 5), 2);
    }

    // ── wrapped_position_for_screen ──────────────────────────────────

    #[test]
    fn wrapped_position_for_screen_basic() {
        let mut ed = Editor::test_with_text("hello\nworld\n");
        let (line, col) = ed.wrapped_position_for_screen(0, 1, 3, 80);
        assert_eq!(line, 1);
        assert_eq!(col, 3);
    }

    #[test]
    fn wrapped_position_for_screen_beyond_end() {
        let mut ed = Editor::test_with_text("hi\n");
        let (line, _col) = ed.wrapped_position_for_screen(0, 999, 0, 80);
        // Falls back to last line
        let last = ed.line_count().saturating_sub(1);
        assert_eq!(line, last);
    }

    // ── set_wrap_scroll_offset ───────────────────────────────────────

    #[test]
    fn set_wrap_scroll_offset_clamps_to_max() {
        let mut ed = Editor::test_with_text("a\nb\n");
        ed.viewport_width = 80;
        ed.set_wrap_scroll_offset(9999);
        // Should be clamped to max wrapped row
        let max = ed.wrapped_total_rows(80).saturating_sub(1);
        assert!(ed.wrap_scroll_offset <= max);
    }

    #[test]
    fn set_wrap_scroll_offset_resets_horizontal() {
        let mut ed = Editor::test_with_text("hello\n");
        ed.viewport_width = 80;
        ed.horizontal_scroll = 42;
        ed.set_wrap_scroll_offset(0);
        assert_eq!(ed.horizontal_scroll, 0);
    }

    // ── clear_stale_folds ────────────────────────────────────────────

    #[test]
    fn clear_stale_folds_clears_ranges() {
        let mut ed = Editor::test_with_text("a\nb\nc\nd\n");
        ed.folded_ranges.insert(1, 3);
        assert!(!ed.folded_ranges.is_empty());
        ed.clear_stale_folds();
        assert!(ed.folded_ranges.is_empty());
    }

    #[test]
    fn clear_stale_folds_noop_when_empty() {
        let mut ed = Editor::test_with_text("a\n");
        ed.cached_visible_line_count = Some(1);
        ed.clear_stale_folds();
        // No folds to clear, so cached_visible_line_count should remain
        assert_eq!(ed.cached_visible_line_count, Some(1));
    }
}

#[cfg(test)]
#[path = "editor_wrap_count_performance_tests.rs"]
mod count_performance_tests;

#[cfg(test)]
#[path = "editor_basic_responsiveness_tests.rs"]
mod basic_responsiveness_tests;
