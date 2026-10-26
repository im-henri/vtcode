use rustc_hash::FxHashSet;

use crate::editor::Editor;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct MultiCursor {
    pub line: usize,
    pub col: usize,
    pub anchor: Option<(usize, usize)>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SelectionSpan {
    pub start_idx: usize,
    pub end_idx: usize,
}

/// Ordinary column selections usually contain one caret per ordered line.
/// Prove that common case without building a hash set or sorting scratch.
fn strictly_ordered_lines<T>(items: &[T], line: impl Fn(&T) -> usize) -> bool {
    strictly_ordered_keys(items, line)
}

fn strictly_ordered_keys<T, K: Ord>(items: &[T], key: impl Fn(&T) -> K) -> bool {
    let mut lines = items.iter().map(key);
    let Some(first) = lines.next() else {
        return true;
    };
    let Some(mut previous) = lines.next() else {
        return true;
    };
    let direction = first.cmp(&previous);
    if direction == std::cmp::Ordering::Equal {
        return false;
    }
    lines.all(|line| {
        let ordered = previous.cmp(&line) == direction;
        previous = line;
        ordered
    })
}

#[cfg(test)]
pub(crate) fn apply_col_offset(col: usize, offset: isize, line_len: usize) -> usize {
    let shifted = (col as isize) + offset;
    let clamped = if shifted < 0 { 0 } else { shifted as usize };
    clamped.min(line_len)
}

#[cfg(test)]
pub(crate) fn sort_for_offset_sensitive_edit(ordered: &mut [(usize, MultiCursor)]) {
    ordered.sort_unstable_by(|a, b| {
        b.1.line
            .cmp(&a.1.line)
            .then_with(|| a.1.col.cmp(&b.1.col))
            .then_with(|| a.0.cmp(&b.0))
    });
}

impl Editor {
    pub(crate) fn toggle_extra_cursor(&mut self, cursor: (usize, usize)) {
        self.extra_cursor_anchors
            .resize(self.extra_cursors.len(), None);
        if let Some(index) = self
            .extra_cursors
            .iter()
            .position(|existing| *existing == cursor)
        {
            self.extra_cursors.remove(index);
            self.extra_cursor_anchors.remove(index);
        } else if cursor == (self.cursor_line, self.cursor_col) {
            // There must always be one primary cursor. Make removing it look
            // like any other cursor removal by promoting the most recently
            // added secondary cursor into the primary slot.
            if let Some((line, col)) = self.extra_cursors.pop() {
                self.cursor_line = line;
                self.cursor_col = col;
                self.selection_anchor = self.extra_cursor_anchors.pop().flatten();
                self.desired_col = None;
            }
        } else {
            self.extra_cursors.push(cursor);
            self.extra_cursor_anchors.push(None);
        }
    }

    pub(crate) fn collect_multi_cursors(&self) -> Vec<MultiCursor> {
        let mut cursors = Vec::with_capacity(self.extra_cursors.len() + 1);
        cursors.push(MultiCursor {
            line: self.cursor_line,
            col: self.cursor_col,
            anchor: self.selection_anchor,
        });

        for (idx, &(line, col)) in self.extra_cursors.iter().enumerate() {
            let anchor = self.extra_cursor_anchors.get(idx).copied().flatten();
            cursors.push(MultiCursor { line, col, anchor });
        }

        if strictly_ordered_keys(&cursors, |cursor| (cursor.line, cursor.col)) {
            return cursors;
        }
        let mut seen = FxHashSet::default();
        seen.reserve(cursors.len());
        cursors.retain(|cursor| seen.insert(*cursor));
        cursors
    }

    pub(crate) fn selection_span_for_cursor(&self, cursor: MultiCursor) -> Option<SelectionSpan> {
        let (anchor_line, anchor_col) = cursor.anchor?;
        let max_line = self.line_count().saturating_sub(1);
        let cursor_line = cursor.line.min(max_line);
        let anchor_line = anchor_line.min(max_line);

        let (start_line, start_col, end_line, end_col) =
            if (anchor_line, anchor_col) <= (cursor_line, cursor.col) {
                (anchor_line, anchor_col, cursor_line, cursor.col)
            } else {
                (cursor_line, cursor.col, anchor_line, anchor_col)
            };

        let start_col_clamped = start_col.min(self.line_len(start_line));
        let end_col_clamped = end_col.min(self.line_len(end_line));
        let start_idx = self.rope.line_to_char(start_line) + start_col_clamped;
        let end_idx = self.rope.line_to_char(end_line) + end_col_clamped;
        if start_idx >= end_idx {
            return None;
        }

        Some(SelectionSpan { start_idx, end_idx })
    }

    pub(crate) fn set_multi_cursor_states(&mut self, mut states: Vec<MultiCursor>) {
        if states.is_empty() {
            return;
        }
        let max_line = self.line_count().saturating_sub(1);
        if states.iter().all(|state| state.line <= max_line)
            && strictly_ordered_keys(&states, |state| (state.line, state.col))
        {
            // Selection movement normally retains ordered carets. As with
            // ordinary movement, clamping keeps that order; adjacent dedup
            // retains the first cursor's anchor without allocating hash maps.
            let mut lengths = [None; 2];
            let mut previous = None;
            states.retain_mut(|state| {
                let mut length = |line: usize| {
                    if let Some((cached, len)) = lengths[0] {
                        if cached == line { return len; }
                    }
                    if let Some((cached, len)) = lengths[1] {
                        if cached == line {
                            lengths.swap(0, 1);
                            return len;
                        }
                    }
                    let len = self.line_len(line);
                    lengths[1] = lengths[0];
                    lengths[0] = Some((line, len));
                    len
                };
                state.col = state.col.min(length(state.line));
                if let Some((line, col)) = state.anchor {
                    let line = line.min(max_line);
                    state.anchor = Some((line, col.min(length(line))));
                }
                let position = (state.line, state.col);
                let unique = previous != Some(position);
                previous = Some(position);
                unique
            });
        } else {
            let mut seen = FxHashSet::default();
            seen.reserve(states.len());
            let mut line_lengths = rustc_hash::FxHashMap::default();
            states.retain_mut(|state| {
                state.line = state.line.min(max_line);
                let len = *line_lengths.entry(state.line).or_insert_with(|| self.line_len(state.line));
                state.col = state.col.min(len);
                if let Some((line, col)) = state.anchor {
                    let line = line.min(max_line);
                    let len = *line_lengths.entry(line).or_insert_with(|| self.line_len(line));
                    state.anchor = Some((line, col.min(len)));
                }
                seen.insert((state.line, state.col))
            });
        }
        let main = states[0];
        self.cursor_line = main.line;
        self.cursor_col = main.col;
        self.selection_anchor = main.anchor;
        self.extra_cursors.clear();
        self.extra_cursor_anchors.clear();
        self.extra_cursors.reserve(states.len() - 1);
        self.extra_cursor_anchors.reserve(states.len() - 1);
        for state in states.into_iter().skip(1) {
            self.extra_cursors.push((state.line, state.col));
            self.extra_cursor_anchors.push(state.anchor);
        }
        self.ensure_visible();
    }

    pub(crate) fn set_multi_cursor_positions(&mut self, mut positions: Vec<(usize, usize)>) {
        if positions.is_empty() {
            return;
        }
        let max_line = self.line_count().saturating_sub(1);
        if positions.iter().all(|&(line, _)| line <= max_line)
            && strictly_ordered_keys(&positions, |position| *position)
        {
            // Ordered positions stay ordered when columns are clamped. One
            // line cache and adjacent deduplication replace two hash tables.
            let mut line_length = None;
            let mut previous = None;
            positions.retain_mut(|position| {
                let (line, col) = position;
                let len = match line_length {
                    Some((cached, len)) if cached == *line => len,
                    _ => {
                        let len = self.line_len(*line);
                        line_length = Some((*line, len));
                        len
                    }
                };
                *col = (*col).min(len);
                let unique = previous != Some(*position);
                previous = Some(*position);
                unique
            });
        } else {
            let mut seen = FxHashSet::default();
            seen.reserve(positions.len());
            let mut line_lengths = rustc_hash::FxHashMap::default();
            positions.retain_mut(|(line, col)| {
                *line = (*line).min(max_line);
                let len = *line_lengths
                    .entry(*line)
                    .or_insert_with(|| self.line_len(*line));
                *col = (*col).min(len);
                seen.insert((*line, *col))
            });
        }
        self.assign_multi_cursor_positions(positions);
    }

    /// Edit planning already proves these positions lie in their final source
    /// lines. Keep that proof instead of measuring every line again afterward.
    pub(crate) fn set_valid_multi_cursor_positions(&mut self, mut positions: Vec<(usize, usize)>) {
        if positions.is_empty() {
            return;
        }
        if !strictly_ordered_keys(&positions, |position| *position) {
            let mut seen = FxHashSet::default();
            seen.reserve(positions.len());
            positions.retain(|position| seen.insert(*position));
        }
        self.assign_multi_cursor_positions(positions);
    }

    fn assign_multi_cursor_positions(&mut self, positions: Vec<(usize, usize)>) {
        (self.cursor_line, self.cursor_col) = positions[0];
        self.selection_anchor = None;
        self.extra_cursors.clear();
        self.extra_cursors.extend(positions.into_iter().skip(1));
        self.extra_cursor_anchors
            .resize(self.extra_cursors.len(), None);
        self.extra_cursor_anchors.fill(None);
        self.ensure_visible();
    }

    fn collect_multi_cursor_positions(&self) -> Vec<(usize, usize)> {
        let mut positions = Vec::with_capacity(self.extra_cursors.len() + 1);
        positions.push((self.cursor_line, self.cursor_col));
        positions.extend(self.extra_cursors.iter().copied());
        positions
    }

    fn collect_multi_cursor_states_for_selection(&self) -> Vec<MultiCursor> {
        let mut states = self.collect_multi_cursors();
        for state in &mut states {
            if state.anchor.is_none() {
                state.anchor = Some((state.line, state.col));
            }
        }
        states
    }

    fn move_to_line_start_pos(
        &self,
        line: usize,
        col: usize,
        metadata: Option<&mut rustc_hash::FxHashMap<usize, (usize, usize)>>,
    ) -> (usize, usize) {
        let line = line.min(self.line_count().saturating_sub(1));
        let compute = || {
            let rope_line = self.rope.line(line);
            let line_len = self.line_len(line);
            let first_non_ws = if let Some(text) = rope_line.as_str() {
                crate::editor::leading_whitespace_count(text)
            } else {
                rope_line
                    .chars()
                    .take_while(|c| c.is_whitespace() && *c != '\n')
                    .count()
            };
            (line_len, first_non_ws)
        };
        let (line_len, first_non_ws) = if let Some(metadata) = metadata {
            *metadata.entry(line).or_insert_with(compute)
        } else {
            compute()
        };
        let col = col.min(line_len);
        let new_col = if col == first_non_ws || first_non_ws >= line_len {
            0
        } else {
            first_non_ws
        };
        (line, new_col)
    }

    /// Scan each token interval at most once for carets sharing a line. Work
    /// order is separate from cursor order so primary/anchor precedence stays
    /// unchanged when normalization later merges duplicate destinations.
    fn move_word_positions<T>(
        &self,
        cursors: &mut [T],
        left: bool,
        position: impl Fn(&T) -> (usize, usize),
        place: impl Fn(&mut T, (usize, usize)),
    ) {
        let move_one = |(line, col)| {
            if left {
                self.word_left_position(line, col)
            } else {
                self.word_right_position(line, col)
            }
        };
        if cursors.len() < 8 || strictly_ordered_lines(cursors, |cursor| position(cursor).0) {
            for cursor in cursors {
                let target = move_one(position(cursor));
                place(cursor, target);
            }
            return;
        }
        let mut ordered: Vec<usize> = (0..cursors.len()).collect();
        ordered.sort_unstable_by(|&a, &b| {
            let (al, ac) = position(&cursors[a]);
            let (bl, bc) = position(&cursors[b]);
            al.cmp(&bl)
                .then_with(|| if left { bc.cmp(&ac) } else { ac.cmp(&bc) })
        });
        let max_line = self.line_count().saturating_sub(1);
        // Left maps (destination, source] to one destination; right maps
        // [source, destination). Excluding the opposite endpoint preserves
        // the next camelCase boundary, punctuation run and line-end jump.
        let mut interval: Option<(usize, usize, usize)> = None;
        for index in ordered {
            let (line, col) = position(&cursors[index]);
            let line = line.min(max_line);
            let target = if let Some((_, _, through)) =
                interval.filter(|&(cached_line, from, through)| {
                    cached_line == line
                        && if left {
                            through < col && col <= from
                        } else {
                            from <= col && col < through
                        }
                }) {
                (line, through)
            } else {
                let target = move_one((line, col));
                interval = (target.0 == line && if left { target.1 < col } else { target.1 > col })
                    .then_some((line, col, target.1));
                target
            };
            place(&mut cursors[index], target);
        }
    }

    fn move_to_line_end_pos(&self, line: usize) -> (usize, usize) {
        let max_line = self.line_count().saturating_sub(1);
        let line = line.min(max_line);
        (line, self.line_len(line))
    }

    pub(crate) fn multi_cursor_move_up(&mut self) {
        let mut positions = self.collect_multi_cursor_positions();
        if self.word_wrap {
            let wrap_width = self.effective_wrap_width();
            for (line, col) in &mut positions {
                let current_row = self.wrapped_row_for_position(*line, *col, wrap_width);
                if current_row > 0 {
                    if let (Some(current), Some(target)) = (
                        self.wrapped_row_at(current_row, wrap_width),
                        self.wrapped_row_at(current_row - 1, wrap_width),
                    ) {
                        let visual_col = self.cell_at_source_column(*line, current.start_col, *col);
                        *line = target.line_idx;
                        *col = self.column_on_wrapped_row(&target, visual_col);
                    }
                } else {
                    *col = 0;
                }
            }
        } else {
            for (line, col) in &mut positions {
                let visual_col = self.cell_at_source_column(*line, 0, *col);
                if *line > 0 {
                    *line -= 1;
                }
                *col = self.source_column_at_cell(*line, 0, self.line_len(*line), visual_col).0;
            }
        }
        self.set_multi_cursor_positions(positions);
    }

    pub(crate) fn multi_cursor_move_down(&mut self) {
        let mut positions = self.collect_multi_cursor_positions();
        if self.word_wrap {
            let wrap_width = self.effective_wrap_width();
            let total_rows = self.wrapped_total_rows(wrap_width);
            for (line, col) in &mut positions {
                let current_row = self.wrapped_row_for_position(*line, *col, wrap_width);
                if current_row + 1 < total_rows {
                    if let (Some(current), Some(target)) = (
                        self.wrapped_row_at(current_row, wrap_width),
                        self.wrapped_row_at(current_row + 1, wrap_width),
                    ) {
                        let visual_col = self.cell_at_source_column(*line, current.start_col, *col);
                        *line = target.line_idx;
                        *col = self.column_on_wrapped_row(&target, visual_col);
                    }
                } else {
                    let last_line = self.line_count().saturating_sub(1);
                    *line = last_line;
                    *col = self.line_len(last_line);
                }
            }
        } else {
            let max_line = self.line_count().saturating_sub(1);
            for (line, col) in &mut positions {
                let visual_col = self.cell_at_source_column(*line, 0, *col);
                if *line < max_line {
                    *line += 1;
                }
                *col = self.source_column_at_cell(*line, 0, self.line_len(*line), visual_col).0;
            }
        }
        self.set_multi_cursor_positions(positions);
    }

    pub(crate) fn multi_cursor_move_left(&mut self) {
        let mut positions = self.collect_multi_cursor_positions();
        for (line, col) in &mut positions {
            let line_len = self.line_len(*line);
            let clamped_col = (*col).min(line_len);
            if clamped_col > 0 {
                *col = clamped_col - 1;
            } else if *line > 0 {
                *line -= 1;
                *col = self.line_len(*line);
            } else {
                *col = 0;
            }
        }
        self.set_multi_cursor_positions(positions);
    }

    pub(crate) fn multi_cursor_move_right(&mut self) {
        let mut positions = self.collect_multi_cursor_positions();
        let max_line = self.line_count().saturating_sub(1);
        for (line, col) in &mut positions {
            let line_len = self.line_len(*line);
            let clamped_col = (*col).min(line_len);
            if clamped_col < line_len {
                *col = clamped_col + 1;
            } else if *line < max_line {
                *line += 1;
                *col = 0;
            } else {
                *col = line_len;
            }
        }
        self.set_multi_cursor_positions(positions);
    }

    pub(crate) fn multi_cursor_move_word_left(&mut self) {
        let mut positions = self.collect_multi_cursor_positions();
        self.move_word_positions(
            &mut positions,
            true,
            |&position| position,
            |slot, target| *slot = target,
        );
        self.set_multi_cursor_positions(positions);
    }

    pub(crate) fn multi_cursor_move_word_right(&mut self) {
        let mut positions = self.collect_multi_cursor_positions();
        self.move_word_positions(
            &mut positions,
            false,
            |&position| position,
            |slot, target| *slot = target,
        );
        self.set_multi_cursor_positions(positions);
    }

    pub(crate) fn multi_cursor_move_to_line_start(&mut self) {
        let mut positions = self.collect_multi_cursor_positions();
        let use_cache =
            positions.len() >= 8 && !strictly_ordered_lines(&positions, |&(line, _)| line);
        let mut metadata = rustc_hash::FxHashMap::default();
        for (line, col) in &mut positions {
            (*line, *col) =
                self.move_to_line_start_pos(*line, *col, use_cache.then_some(&mut metadata));
        }
        self.set_multi_cursor_positions(positions);
    }

    pub(crate) fn multi_cursor_move_to_line_end(&mut self) {
        let mut positions = self.collect_multi_cursor_positions();
        for (line, col) in &mut positions {
            (*line, *col) = self.move_to_line_end_pos(*line);
        }
        self.set_multi_cursor_positions(positions);
    }

    pub(crate) fn multi_cursor_select_move_up(&mut self) {
        let mut states = self.collect_multi_cursor_states_for_selection();
        if self.word_wrap {
            let wrap_width = self.effective_wrap_width();
            for state in &mut states {
                let current_row = self.wrapped_row_for_position(state.line, state.col, wrap_width);
                if current_row > 0 {
                    if let (Some(current), Some(target)) = (
                        self.wrapped_row_at(current_row, wrap_width),
                        self.wrapped_row_at(current_row - 1, wrap_width),
                    ) {
                        let visual_col = self.cell_at_source_column(state.line, current.start_col, state.col);
                        state.line = target.line_idx;
                        state.col = self.column_on_wrapped_row(&target, visual_col);
                    }
                } else {
                    state.col = 0;
                }
            }
        } else {
            for state in &mut states {
                let visual_col = self.cell_at_source_column(state.line, 0, state.col);
                if state.line > 0 {
                    state.line -= 1;
                }
                state.col = self.source_column_at_cell(state.line, 0, self.line_len(state.line), visual_col).0;
            }
        }
        self.set_multi_cursor_states(states);
    }

    pub(crate) fn multi_cursor_select_move_down(&mut self) {
        let mut states = self.collect_multi_cursor_states_for_selection();
        if self.word_wrap {
            let wrap_width = self.effective_wrap_width();
            let total_rows = self.wrapped_total_rows(wrap_width);
            for state in &mut states {
                let current_row = self.wrapped_row_for_position(state.line, state.col, wrap_width);
                if current_row + 1 < total_rows {
                    if let (Some(current), Some(target)) = (
                        self.wrapped_row_at(current_row, wrap_width),
                        self.wrapped_row_at(current_row + 1, wrap_width),
                    ) {
                        let visual_col = self.cell_at_source_column(state.line, current.start_col, state.col);
                        state.line = target.line_idx;
                        state.col = self.column_on_wrapped_row(&target, visual_col);
                    }
                } else {
                    let last_line = self.line_count().saturating_sub(1);
                    state.line = last_line;
                    state.col = self.line_len(last_line);
                }
            }
        } else {
            let max_line = self.line_count().saturating_sub(1);
            for state in &mut states {
                let visual_col = self.cell_at_source_column(state.line, 0, state.col);
                if state.line < max_line {
                    state.line += 1;
                }
                state.col = self.source_column_at_cell(state.line, 0, self.line_len(state.line), visual_col).0;
            }
        }
        self.set_multi_cursor_states(states);
    }

    pub(crate) fn multi_cursor_select_move_left(&mut self) {
        let mut states = self.collect_multi_cursor_states_for_selection();
        for state in &mut states {
            let line_len = self.line_len(state.line);
            let clamped_col = state.col.min(line_len);
            if clamped_col > 0 {
                state.col = clamped_col - 1;
            } else if state.line > 0 {
                state.line -= 1;
                state.col = self.line_len(state.line);
            } else {
                state.col = 0;
            }
        }
        self.set_multi_cursor_states(states);
    }

    pub(crate) fn multi_cursor_select_move_right(&mut self) {
        let mut states = self.collect_multi_cursor_states_for_selection();
        let max_line = self.line_count().saturating_sub(1);
        for state in &mut states {
            let line_len = self.line_len(state.line);
            let clamped_col = state.col.min(line_len);
            if clamped_col < line_len {
                state.col = clamped_col + 1;
            } else if state.line < max_line {
                state.line += 1;
                state.col = 0;
            } else {
                state.col = line_len;
            }
        }
        self.set_multi_cursor_states(states);
    }

    pub(crate) fn multi_cursor_select_move_word_left(&mut self) {
        let mut states = self.collect_multi_cursor_states_for_selection();
        self.move_word_positions(
            &mut states,
            true,
            |state| (state.line, state.col),
            |state, target| {
                (state.line, state.col) = target;
            },
        );
        self.set_multi_cursor_states(states);
    }

    pub(crate) fn multi_cursor_select_move_word_right(&mut self) {
        let mut states = self.collect_multi_cursor_states_for_selection();
        self.move_word_positions(
            &mut states,
            false,
            |state| (state.line, state.col),
            |state, target| {
                (state.line, state.col) = target;
            },
        );
        self.set_multi_cursor_states(states);
    }

    pub(crate) fn multi_cursor_select_move_to_line_start(&mut self) {
        let mut states = self.collect_multi_cursor_states_for_selection();
        let use_cache = states.len() >= 8 && !strictly_ordered_lines(&states, |state| state.line);
        let mut metadata = rustc_hash::FxHashMap::default();
        for state in &mut states {
            (state.line, state.col) = self.move_to_line_start_pos(
                state.line,
                state.col,
                use_cache.then_some(&mut metadata),
            );
        }
        self.set_multi_cursor_states(states);
    }

    pub(crate) fn multi_cursor_select_move_to_line_end(&mut self) {
        let mut states = self.collect_multi_cursor_states_for_selection();
        for state in &mut states {
            (state.line, state.col) = self.move_to_line_end_pos(state.line);
        }
        self.set_multi_cursor_states(states);
    }

    /// Add a cursor on the line aboveor below at the same column (column/box selection)
    pub(crate) fn add_cursor_above(&mut self) {
        let min_line = self
            .extra_cursors
            .iter()
            .map(|&(line, _)| line)
            .fold(self.cursor_line, usize::min);
        if min_line > 0 {
            let new_line = min_line - 1;
            let col = self.cursor_col.min(self.line_len(new_line));
            self.extra_cursors.push((new_line, col));
            self.extra_cursor_anchors.push(None);
        }
    }

    pub(crate) fn add_cursor_below(&mut self) {
        let max_line = self
            .extra_cursors
            .iter()
            .map(|&(line, _)| line)
            .fold(self.cursor_line, usize::max);
        if max_line + 1 < self.line_count() {
            let new_line = max_line + 1;
            let col = self.cursor_col.min(self.line_len(new_line));
            self.extra_cursors.push((new_line, col));
            self.extra_cursor_anchors.push(None);
        }
    }

    /// Add cursors at end of each line in selection (Alt+Shift+I in VSCode)
    pub(crate) fn add_cursors_to_line_ends(&mut self) {
        if let Some((sl, _sc, el, _ec)) = self.selection_range() {
            self.selection_anchor = None;
            self.extra_cursors.clear();
            self.extra_cursor_anchors.clear();
            // Put main cursor at end of first line
            let first_line_len = self.line_len(sl);
            self.cursor_line = sl;
            self.cursor_col = first_line_len;
            for line in (sl + 1)..=el {
                if line < self.line_count() {
                    let col = self.line_len(line);
                    self.extra_cursors.push((line, col));
                    self.extra_cursor_anchors.push(None);
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "editor_multicursor_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "editor_multicursor_performance_tests.rs"]
mod performance_tests;
