//! Bounded optional scoring is separate from the ordinary Quick Open path.
use super::*;

const BYTE_QUANTUM: usize = 4096;
const CELL_QUANTUM: usize = 1024;
type Stop = FuzzyScoreOutcome;

fn capacity_bytes<T>(values: &Vec<T>) -> Option<usize> {
    values.capacity().checked_mul(std::mem::size_of::<T>())
}

fn sum_bytes(parts: &[Option<usize>]) -> Option<usize> {
    parts
        .iter()
        .try_fold(0usize, |sum, part| sum.checked_add((*part)?))
}

fn reserve<T>(
    values: &mut Vec<T>,
    len: usize,
    other_bytes: Option<usize>,
    limit: usize,
) -> Result<(), Stop> {
    let required = len
        .max(values.capacity())
        .checked_mul(std::mem::size_of::<T>())
        .and_then(|bytes| bytes.checked_add(other_bytes?))
        .ok_or(Stop::WorkLimited)?;
    if required > limit {
        return Err(Stop::WorkLimited);
    }
    if len > values.capacity() {
        values
            .try_reserve_exact(len.saturating_sub(values.len()))
            .map_err(|_| Stop::WorkLimited)?;
    }
    // Allocators may give reserve_exact a larger capacity than requested.
    if capacity_bytes(values)
        .and_then(|bytes| bytes.checked_add(other_bytes?))
        .is_none_or(|bytes| bytes > limit)
    {
        *values = Vec::new();
        return Err(Stop::WorkLimited);
    }
    Ok(())
}

fn checkpoint(cancelled: &mut impl FnMut() -> bool) -> Result<(), Stop> {
    if cancelled() {
        Err(Stop::Cancelled)
    } else {
        Ok(())
    }
}

fn charge_bytes(
    budget: &mut FuzzyCandidateBudget<'_>,
    bytes: usize,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<(), Stop> {
    checkpoint(cancelled)?;
    if budget.charge_bytes(bytes) {
        Ok(())
    } else {
        Err(Stop::WorkLimited)
    }
}

fn charge_cells(
    budget: &mut FuzzyCandidateBudget<'_>,
    cells: usize,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<(), Stop> {
    checkpoint(cancelled)?;
    if budget.charge_cells(cells) {
        Ok(())
    } else {
        Err(Stop::WorkLimited)
    }
}

fn is_ascii(
    text: &str,
    budget: &mut FuzzyCandidateBudget<'_>,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<bool, Stop> {
    for chunk in text.as_bytes().chunks(BYTE_QUANTUM) {
        charge_bytes(budget, chunk.len(), cancelled)?;
        if !chunk.is_ascii() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn decode(
    text: &str,
    output: &mut Vec<char>,
    other_bytes: Option<usize>,
    budget: &mut FuzzyCandidateBudget<'_>,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<(), Stop> {
    output.clear();
    let mut count = 0usize;
    for chunk in text.as_bytes().chunks(BYTE_QUANTUM) {
        charge_bytes(budget, chunk.len(), cancelled)?;
        // Every UTF-8 scalar has exactly one non-continuation byte. Counting
        // this way permits fixed byte checkpoints without splitting scalars.
        count += chunk.iter().filter(|&&byte| byte & 0xc0 != 0x80).count();
    }
    reserve(output, count, other_bytes, budget.scratch_limit())?;
    let mut start = 0;
    while start < text.len() {
        let mut end = start.saturating_add(BYTE_QUANTUM).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        charge_bytes(budget, end - start, cancelled)?;
        output.extend(text[start..end].chars());
        start = end;
    }
    Ok(())
}

impl PreparedFuzzyScore<'_> {
    fn retained_scratch_bytes(&self) -> Option<usize> {
        sum_bytes(&[
            capacity_bytes(&self.pattern_chars),
            capacity_bytes(&self.choice_chars),
            self.scratch.retained_bytes(),
        ])
    }

    fn clear_scratch(&mut self) {
        self.pattern_chars = Vec::new();
        self.choice_chars = Vec::new();
        self.scratch = ScoreScratch::default();
        self.cancellable_pattern_prepared = false;
    }

    /// Exact fresh-engine scores with bounded optional work. Linear scans and
    /// scratch initialization are cancellable as well as the DP recurrence.
    pub(crate) fn score_cancellable(
        &mut self,
        choice: &str,
        budget: &mut FuzzyCandidateBudget<'_>,
        mut cancelled: impl FnMut() -> bool,
    ) -> FuzzyScoreOutcome {
        if self
            .retained_scratch_bytes()
            .is_none_or(|bytes| bytes > budget.scratch_limit())
        {
            self.clear_scratch();
        }
        let result = self.score_bounded(choice, budget, &mut cancelled);
        if !self.cancellable_pattern_prepared {
            // An interrupted Unicode decode must not look like a complete
            // prepared query to either this API or the unchanged score API.
            self.pattern_chars.clear();
        }
        if cancelled() {
            Stop::Cancelled
        } else {
            match result {
                Ok(score) => Stop::Scored(score),
                Err(stop) => stop,
            }
        }
    }

    fn score_bounded(
        &mut self,
        choice: &str,
        budget: &mut FuzzyCandidateBudget<'_>,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Option<i64>, Stop> {
        checkpoint(cancelled)?;
        if self.query.is_empty() {
            return Ok(Some(0));
        }
        // These scalar bounds need no scan or allocation, even for a giant
        // query that cannot possibly fit in a small completion word.
        if self.query.len().div_ceil(4) > choice.len() {
            return Ok(None);
        }
        let choice_ascii = is_ascii(choice, budget, cancelled)?;
        let sensitive = self.case_sensitive;
        if self.ascii && choice_ascii {
            let external = sum_bytes(&[
                capacity_bytes(&self.pattern_chars),
                capacity_bytes(&self.choice_chars),
            ]);
            return self.scratch.score_bounded(
                choice.as_bytes(),
                self.query.as_bytes(),
                0,
                |a, b| {
                    if sensitive {
                        a == b
                    } else {
                        a.eq_ignore_ascii_case(&b)
                    }
                },
                boundary_bonus,
                external,
                budget,
                cancelled,
            );
        }
        if choice_ascii && !self.ascii {
            return Ok(None);
        }
        if !self.cancellable_pattern_prepared {
            let other = sum_bytes(&[
                capacity_bytes(&self.choice_chars),
                self.scratch.retained_bytes(),
            ]);
            decode(
                self.query,
                &mut self.pattern_chars,
                other,
                budget,
                cancelled,
            )?;
            self.cancellable_pattern_prepared = true;
        }
        let other = sum_bytes(&[
            capacity_bytes(&self.pattern_chars),
            self.scratch.retained_bytes(),
        ]);
        decode(choice, &mut self.choice_chars, other, budget, cancelled)?;
        let external = sum_bytes(&[
            capacity_bytes(&self.pattern_chars),
            capacity_bytes(&self.choice_chars),
        ]);
        self.scratch.score_bounded(
            &self.choice_chars,
            &self.pattern_chars,
            '\0',
            |a, b| {
                if sensitive {
                    a == b
                } else {
                    a.eq_ignore_ascii_case(&b)
                }
            },
            |a, b| {
                boundary_bonus(
                    if a.is_ascii() { a as u8 } else { b'a' },
                    if b.is_ascii() { b as u8 } else { b'a' },
                )
            },
            external,
            budget,
            cancelled,
        )
    }
}

impl ScoreScratch {
    fn retained_bytes(&self) -> Option<usize> {
        sum_bytes(&[
            capacity_bytes(&self.first_matches),
            capacity_bytes(&self.bonuses),
            capacity_bytes(&self.cells),
        ])
    }

    #[allow(clippy::too_many_arguments)]
    fn score_bounded<T: Copy + Eq>(
        &mut self,
        choice: &[T],
        pattern: &[T],
        empty: T,
        equal: impl Fn(T, T) -> bool,
        boundary: impl Fn(T, T) -> i32,
        external: Option<usize>,
        budget: &mut FuzzyCandidateBudget<'_>,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Option<i64>, Stop> {
        if pattern.len() > choice.len() {
            return Ok(None);
        }
        // The pinned recurrence stores i32 scores. Configurable unlimited
        // budgets must not permit arithmetic overflow for enormous inputs.
        if pattern.len() > (i32::MAX as usize - 31) / 24 + 1 {
            return Err(Stop::WorkLimited);
        }
        let read_bytes = |len: usize| {
            len.checked_mul(std::mem::size_of::<T>())
                .ok_or(Stop::WorkLimited)
        };
        if pattern.len() == 1 {
            let mut previous = empty;
            let mut best: Option<i64> = None;
            for (chunk_index, chunk) in choice.chunks(CELL_QUANTUM).enumerate() {
                charge_bytes(budget, read_bytes(chunk.len())?, cancelled)?;
                for (within, &candidate) in chunk.iter().enumerate() {
                    if equal(candidate, pattern[0]) {
                        let index = chunk_index * CELL_QUANTUM + within;
                        let bonus = boundary(previous, candidate) * if index == 0 { 2 } else { 1 };
                        let score =
                            i64::from(bonus + if candidate == pattern[0] { 16 } else { 14 } - 1);
                        best = Some(best.map_or(score, |old| old.max(score)));
                    }
                    previous = candidate;
                }
            }
            return Ok(best);
        }

        self.first_matches.clear();
        let other = sum_bytes(&[
            external,
            capacity_bytes(&self.bonuses),
            capacity_bytes(&self.cells),
        ]);
        reserve(
            &mut self.first_matches,
            pattern.len(),
            other,
            budget.scratch_limit(),
        )?;
        'eligibility: for (chunk_index, chunk) in choice.chunks(CELL_QUANTUM).enumerate() {
            charge_bytes(budget, read_bytes(chunk.len())?, cancelled)?;
            for (within, &candidate) in chunk.iter().enumerate() {
                if equal(candidate, pattern[self.first_matches.len()]) {
                    self.first_matches.push(chunk_index * CELL_QUANTUM + within);
                    if self.first_matches.len() == pattern.len() {
                        break 'eligibility;
                    }
                }
            }
        }
        if self.first_matches.len() != pattern.len() {
            return Ok(None);
        }

        let mut end = choice.len();
        'tail: for chunk in choice.rchunks(CELL_QUANTUM) {
            charge_bytes(budget, read_bytes(chunk.len())?, cancelled)?;
            for within in (0..chunk.len()).rev() {
                if equal(chunk[within], pattern[pattern.len() - 1]) {
                    end -= chunk.len() - within - 1;
                    break 'tail;
                }
            }
            end -= chunk.len();
        }
        let choice = &choice[..end];
        let columns = end.checked_add(1).ok_or(Stop::WorkLimited)?;
        let cells = columns.checked_mul(2).ok_or(Stop::WorkLimited)?;
        let final_scan = columns - self.first_matches[pattern.len() - 1];
        let mut updates = cells;
        for chunk in self
            .first_matches
            .chunks(BYTE_QUANTUM / std::mem::size_of::<usize>())
        {
            charge_bytes(
                budget,
                chunk.len() * std::mem::size_of::<usize>(),
                cancelled,
            )?;
            for first in chunk {
                updates = updates.checked_add(end - first).ok_or(Stop::WorkLimited)?;
            }
        }
        let updates = updates.checked_add(final_scan).ok_or(Stop::WorkLimited)?;
        if updates > budget.remaining_cells() {
            return Err(Stop::WorkLimited);
        }
        checkpoint(cancelled)?;
        let other = sum_bytes(&[
            external,
            capacity_bytes(&self.first_matches),
            capacity_bytes(&self.cells),
        ]);
        reserve(&mut self.bonuses, columns, other, budget.scratch_limit())?;
        let other = sum_bytes(&[
            external,
            capacity_bytes(&self.first_matches),
            capacity_bytes(&self.bonuses),
        ]);
        reserve(&mut self.cells, cells, other, budget.scratch_limit())?;

        self.bonuses.clear();
        self.bonuses.push(0);
        let mut previous = empty;
        for chunk in choice.chunks(CELL_QUANTUM) {
            charge_bytes(budget, read_bytes(chunk.len())?, cancelled)?;
            for &candidate in chunk {
                self.bonuses.push(boundary(previous, candidate));
                previous = candidate;
            }
        }
        self.bonuses[1] *= 2;
        self.cells.clear();
        for initial in [true, false] {
            let value = if initial {
                ScoreCell {
                    skipped: -1,
                    ..ScoreCell::default()
                }
            } else {
                ScoreCell::default()
            };
            let mut remaining = columns;
            while remaining != 0 {
                let count = remaining.min(CELL_QUANTUM);
                charge_cells(budget, count, cancelled)?;
                self.cells.extend(std::iter::repeat_n(value, count));
                remaining -= count;
            }
        }

        for (pattern_index, &byte) in pattern.iter().enumerate() {
            let current_row = ((pattern_index + 1) & 1) * columns;
            let previous_row = (pattern_index & 1) * columns;
            let mut start = self.first_matches[pattern_index];
            while start < end {
                let stop = start.saturating_add(CELL_QUANTUM).min(end);
                charge_cells(budget, stop - start, cancelled)?;
                for position in start..stop {
                    let left = current_row + position;
                    let current = left + 1;
                    let previous = previous_row + position;
                    if equal(choice[position], byte) {
                        let bonus = self.cells[left]
                            .bonus
                            .max(self.bonuses[position + 1])
                            .max(4);
                        self.cells[left].bonus = bonus;
                        let consecutive = self.cells[previous].matched + bonus;
                        let separated = self.cells[previous].skipped + self.bonuses[position + 1];
                        self.cells[current].matched = consecutive.max(separated)
                            + if choice[position] == byte { 16 } else { 14 };
                    } else {
                        self.cells[current].matched = NEGATIVE_INFINITY;
                        self.cells[current].bonus = 0;
                    }
                    self.cells[current].skipped =
                        (self.cells[left].matched - 4).max(self.cells[left].skipped - 1);
                }
                start = stop;
            }
        }
        let final_row = (pattern.len() & 1) * columns;
        let first = self.first_matches[pattern.len() - 1];
        let mut best = None;
        for chunk in self.cells[final_row + first..final_row + columns].chunks(CELL_QUANTUM) {
            charge_cells(budget, chunk.len(), cancelled)?;
            for cell in chunk {
                let score = i64::from(cell.matched);
                best = Some(best.map_or(score, |old: i64| old.max(score)));
            }
        }
        Ok(best)
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;

    #[test]
    fn scratch_capacity_overflow_is_rejected_before_allocation() {
        let mut cells = Vec::<u64>::new();
        assert_eq!(
            reserve(&mut cells, usize::MAX, Some(0), usize::MAX),
            Err(Stop::WorkLimited)
        );
        assert_eq!(
            reserve(&mut cells, 1, Some(usize::MAX), usize::MAX),
            Err(Stop::WorkLimited)
        );
        assert_eq!(cells.capacity(), 0);
    }
}
