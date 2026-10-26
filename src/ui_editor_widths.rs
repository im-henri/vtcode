//! Sliding viewport metadata. Scrolling measures newly exposed lines; local
//! edits refresh only their affected rows. Values never depend on syntax/theme.
use std::collections::VecDeque;

const MAX_ROWS: usize = 1024;

#[derive(Default)]
pub(super) struct VisibleWidths {
    first: usize,
    widths: VecDeque<usize>,
    maximum: usize,
    dirty: Option<(usize, usize)>,
    ready: bool,
    #[cfg(test)]
    pub(super) measured: usize,
}

pub(super) fn content_len(line: ropey::RopeSlice<'_>) -> usize {
    crate::editor::Editor::line_content_len(line)
}

fn line_widths(rope: &ropey::Rope, lines: std::ops::Range<usize>)
    -> impl DoubleEndedIterator<Item = usize> + '_
{
    // A line iterator can visit many leaves to cross one giant log record.
    // Widths need only the indexed character counts and each line's tail.
    lines.map(move |line| content_len(rope.line(line)))
}

impl VisibleWidths {
    pub(super) fn clear(&mut self) {
        self.widths.clear();
        self.dirty = None;
        self.ready = false;
    }

    pub(super) fn invalidate(&mut self, first: usize, last: usize) {
        self.dirty = Some(self.dirty.map_or((first, last), |(a, b)| (a.min(first), b.max(last))));
    }

    pub(super) fn get(&mut self, rope: &ropey::Rope, first: usize, end: usize) -> (usize, bool) {
        if end - first > MAX_ROWS {
            self.clear();
            return (line_widths(rope, first..end).max().unwrap_or(0), true);
        }
        let old_end = self.first + self.widths.len();
        if self.ready && self.first == first && old_end == end && self.dirty.is_none() {
            return (self.maximum, false);
        }
        if !self.ready || first >= old_end || end <= self.first {
            self.widths.clear();
            self.first = first;
        } else {
            let remove = first.saturating_sub(self.first);
            self.widths.drain(..remove);
            self.first = self.first.max(first);
            self.widths.truncate(end.saturating_sub(self.first));
            if first < self.first {
                for width in line_widths(rope, first..self.first).rev() {
                    self.widths.push_front(width);
                    #[cfg(test)]
                    { self.measured += 1; }
                }
                self.first = first;
            }
        }
        let append = self.first + self.widths.len();
        for width in line_widths(rope, append..end) {
            self.widths.push_back(width);
            #[cfg(test)]
            { self.measured += 1; }
        }
        if let Some((a, b)) = self.dirty.take() {
            let a = a.max(first);
            let b = b.saturating_add(1).min(end);
            if a < b {
                for (offset, width) in line_widths(rope, a..b).enumerate() {
                    self.widths[a - first + offset] = width;
                    #[cfg(test)]
                    { self.measured += 1; }
                }
            }
        }
        self.maximum = self.widths.iter().copied().max().unwrap_or(0);
        self.ready = true;
        (self.maximum, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> ropey::Rope {
        ropey::Rope::from_str(&(0..5000).map(|i| format!("{}{}", "é界 x".repeat(i % 73),
            if i % 3 == 0 { "\r\n" } else if i % 3 == 1 { "\n" } else { "\r" })).collect::<String>())
    }

    fn check(cache: &mut VisibleWidths, rope: &ropey::Rope, first: usize, end: usize) {
        let expected = rope.lines_at(first).take(end - first).map(content_len).max().unwrap_or(0);
        assert_eq!(cache.get(rope, first, end).0, expected, "range={first}..{end}");
        assert!(cache.widths.len() <= MAX_ROWS);
    }

    #[test]
    fn sliding_widths_match_full_scan_across_scroll_resize_empty_and_oversized_views() {
        let rope = source();
        let mut cache = VisibleWidths::default();
        for i in 0..800 {
            let first = match i % 4 { 0 => 190, 1 => 191, 2 => 187, _ => 180 + i % 75 };
            check(&mut cache, &rope, first, first + [0, 1, 60, 80, 300, 2000][i % 6]);
        }
        check(&mut cache, &rope, rope.len_lines(), rope.len_lines());
        check(&mut cache, &rope, 0, rope.len_lines());
        check(&mut cache, &rope, 4990, rope.len_lines());
    }

    #[test]
    fn scrolling_measures_only_exposed_rows_and_edits_only_invalidated_rows() {
        let mut rope = source();
        let mut cache = VisibleWidths::default();
        check(&mut cache, &rope, 200, 260);
        assert_eq!(cache.measured, 60);
        for first in (201usize..250).chain((190..250).rev()) {
            let before = cache.measured;
            let distance = first.abs_diff(cache.first);
            check(&mut cache, &rope, first, first + 60);
            assert_eq!(cache.measured - before, distance);
        }
        let at = rope.line_to_char(205);
        rope.insert(at, &"x".repeat(1000));
        cache.invalidate(205, 205);
        let before = cache.measured;
        check(&mut cache, &rope, 190, 250);
        assert_eq!(cache.measured, before + 1);
        assert!(!cache.get(&rope, 190, 250).1);
        assert_eq!(cache.measured, before + 1);
        cache.clear();
        rope = ropey::Rope::from_str("a\r\n界\nlast\r");
        check(&mut cache, &rope, 0, rope.len_lines());
    }

    #[test]
    fn long_record_widths_survive_scroll_direction_resize_and_local_edits() {
        let mut rope = ropey::Rope::from_str(&format!("head\r\n{}界\r\ntail\n", "x".repeat(2_000_000)));
        let mut cache = VisibleWidths::default();
        for (start, end) in [(0, 2), (1, 3), (0, 4), (2, 4), (1, 2), (0, 0)] {
            check(&mut cache, &rope, start, end);
        }
        check(&mut cache, &rope, 0, 4);
        let before = cache.measured;
        rope.insert_char(rope.line_to_char(1) + 1_000_000, '🙂');
        cache.invalidate(1, 1);
        check(&mut cache, &rope, 0, 4);
        assert_eq!(cache.measured, before + 1);
        assert_eq!(cache.maximum, 2_000_002);
    }

    #[test]
    #[ignore = "manual optimized-profile scrolling metadata benchmark"]
    fn benchmark_sliding_viewport_widths() {
        use std::{hint::black_box, time::Instant};
        let rope = source();
        let mut samples = [Vec::new(), Vec::new()];
        for round in 0..9 {
            for fresh in [round % 2 == 0, round % 2 != 0] {
                let mut cache = VisibleWidths::default();
                let start = Instant::now();
                for frame in 0..5000 {
                    let offset = frame % 600;
                    let first = if offset < 300 { offset } else { 599 - offset };
                    if fresh {
                        black_box(black_box(&rope).lines_at(first).take(60).map(content_len).max().unwrap_or(0));
                    } else {
                        black_box(cache.get(black_box(&rope), first, first + 60));
                    }
                }
                samples[usize::from(fresh)].push(start.elapsed() / 5000);
            }
        }
        for sample in &mut samples { sample.sort(); }
        eprintln!("scroll viewport widths: before={:?}, after={:?}", samples[1][4], samples[0][4]);
    }
}
