//! Cache text classification, never colors or document decorations. Moving a
//! caret, selecting, or changing a theme should not decode minimap text again.
use crate::ui_editor_highlight::line_lru::LineLru;
use ropey::Rope;

// Keep four decoded source lines per minimap row without evicting the usual
// viewport while drawing it. Rows and allocations are recycled as we scroll.
const MAX_ROWS: usize = 1_024;
const MAX_COLUMNS: usize = 128;
const MAX_INDENT_CHARS: usize = 4_096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Glyph {
    Blank,
    Text,
    String,
    Number,
    Punctuation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Sample {
    pub glyph: Glyph,
    // Each source character occupies one horizontal braille pixel.
    pixels: u8,
}

impl Sample {
    const BLANK: Self = Self {
        glyph: Glyph::Blank,
        pixels: 0,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct MiniCell {
    pub glyph: Glyph,
    pub dots: u8,
}

impl MiniCell {
    pub(super) fn symbol(self) -> char {
        if self.dots == 0 {
            ' '
        } else {
            char::from_u32(0x2800 + u32::from(self.dots)).unwrap()
        }
    }

    pub(super) fn add(&mut self, sample: Sample, dots: u8) {
        let glyph = sample.glyph;
        if glyph == Glyph::Blank {
            return;
        }
        if sample.pixels & 1 != 0 {
            self.dots |= dots & 0x47;
        }
        if sample.pixels & 2 != 0 {
            self.dots |= dots & 0xb8;
        }
        // Prefer word content to isolated braces when several source lines
        // share a terminal cell. Decorations and theme colors stay uncached.
        let priority = |kind| match kind {
            Glyph::Blank => 0,
            Glyph::Punctuation => 1,
            Glyph::Number => 2,
            Glyph::String => 3,
            Glyph::Text => 4,
        };
        if priority(glyph) > priority(self.glyph) {
            self.glyph = glyph;
        }
    }
}

#[derive(Default)]
pub(crate) struct MinimapCache {
    source: Option<Rope>,
    dirty: Option<(usize, usize)>,
    columns: usize,
    tab_size: usize,
    rows: LineLru<Vec<Sample>>,
    scratch: Vec<Sample>,
    band: Vec<MiniCell>,
    #[cfg(test)]
    pub(super) builds: usize,
    #[cfg(test)]
    reference_clear: bool,
}

impl MinimapCache {
    pub(crate) fn clear(&mut self) {
        self.source = None;
        self.dirty = None;
        self.rows.clear();
    }

    pub(crate) fn mark_dirty(&mut self, start: usize, end: usize) {
        self.dirty = Some(
            self.dirty
                .map_or((start, end.max(start)), |(from, through)| {
                    (from.min(start), through.max(end))
                }),
        );
    }

    pub(super) fn synchronize(&mut self, rope: &Rope, columns: usize, tab_size: usize) {
        let tab_size = tab_size.max(1);
        if self.columns == columns
            && self.tab_size == tab_size
            && self.dirty.is_none()
            && self
                .source
                .as_ref()
                .is_some_and(|old| old.is_instance(rope))
        {
            return;
        }
        if self.columns == columns
            && self.tab_size == tab_size
            && let Some((start, end)) = self.dirty.take()
            && let Some(old) = &self.source
        {
            // Include neighboring rows for changes to CRLF boundaries.
            let start = start.saturating_sub(1);
            let end = end.saturating_add(1);
            let shifted = old.len_lines() != rope.len_lines();
            if !shifted && end.saturating_sub(start) <= 64 {
                for line in start..=end {
                    self.rows.remove(line);
                }
            } else {
                self.rows
                    .retain(|(&line, _)| line < start || (!shifted && line > end));
            }
        } else {
            self.rows.clear();
        }
        self.dirty = None;
        self.columns = columns;
        self.tab_size = tab_size;
        self.source = Some(rope.clone());
    }

    pub(super) fn row(&mut self, rope: &Rope, line: usize) -> &[Sample] {
        if self.columns > MAX_COLUMNS {
            Self::decode(&mut self.scratch, rope, line, self.columns, self.tab_size);
            return &self.scratch;
        }
        #[cfg(test)]
        if self.reference_clear && self.rows.len() >= MAX_ROWS && self.rows.get(line).is_none() {
            self.rows.clear();
        }
        let columns = self.columns;
        let tab_size = self.tab_size;
        #[cfg(test)]
        let builds = &mut self.builds;
        self.rows
            .get_or_insert_recycled(line, MAX_ROWS, |recycled| {
                // Scrolling across the capacity boundary should decode one new
                // row, not erase the whole viewport. Reuse the evicted allocation.
                let mut glyphs = recycled.unwrap_or_else(|| Vec::with_capacity(columns));
                #[cfg(test)]
                {
                    *builds += 1;
                }
                Self::decode(&mut glyphs, rope, line, columns, tab_size);
                glyphs
            })
    }

    pub(super) fn band(
        &mut self,
        rope: &Rope,
        samples: impl IntoIterator<Item = (usize, u8)>,
    ) -> &[MiniCell] {
        let mut band = std::mem::take(&mut self.band);
        band.clear();
        band.resize(
            self.columns,
            MiniCell {
                glyph: Glyph::Blank,
                dots: 0,
            },
        );
        for (line, dots) in samples {
            for (cell, &glyph) in band.iter_mut().zip(self.row(rope, line)) {
                cell.add(glyph, dots);
            }
        }
        self.band = band;
        &self.band
    }

    pub(super) fn decode(
        glyphs: &mut Vec<Sample>,
        rope: &Rope,
        line: usize,
        columns: usize,
        tab_size: usize,
    ) {
        glyphs.clear();
        if columns == 0 {
            return;
        }
        let line = rope.line(line);
        let mut chars = line.chars().take_while(|&ch| !matches!(ch, '\n' | '\r'));
        let mut indent = 0usize;
        let mut first = None;
        // Keep pathological whitespace-only/minified lines bounded on the UI
        // thread, while allowing real deeply nested code to reach the preview.
        for ch in chars.by_ref().take(MAX_INDENT_CHARS) {
            if !ch.is_whitespace() {
                first = Some(ch);
                break;
            }
            indent = indent.saturating_add(if ch == '\t' {
                tab_size.max(1) - indent % tab_size.max(1)
            } else {
                1
            });
        }
        let Some(first) = first else {
            glyphs.resize(columns, Sample::BLANK);
            return;
        };
        // Retain a compact indentation silhouette, reserving at least two
        // thirds of the preview for code even in deeply nested blocks.
        glyphs.resize(indent.div_ceil(4).min(columns / 3), Sample::BLANK);
        let mut chars = std::iter::once(first).chain(chars);
        while glyphs.len() < columns {
            let Some(ch) = chars.next() else { break };
            let next = chars.next().unwrap_or(' ');
            let lead = if ch.is_whitespace() { next } else { ch };
            let glyph = if ch.is_whitespace() && next.is_whitespace() {
                Glyph::Blank
            } else if matches!(lead, '"' | '\'' | '`') {
                Glyph::String
            } else if lead.is_ascii_digit() {
                Glyph::Number
            } else if !lead.is_alphabetic() && lead != '_' {
                Glyph::Punctuation
            } else {
                Glyph::Text
            };
            glyphs.push(Sample {
                glyph,
                pixels: u8::from(!ch.is_whitespace()) | u8::from(!next.is_whitespace()) << 1,
            });
        }
        glyphs.resize(columns, Sample::BLANK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimap_reuses_rows_and_only_decodes_edit_neighbors() {
        let mut rope = Rope::from_str(&"name = 123;\n".repeat(100));
        let mut cache = MinimapCache::default();
        cache.synchronize(&rope, 24, 4);
        for line in 0..100 {
            cache.row(&rope, line);
        }
        let builds = cache.builds;
        for _ in 0..20 {
            cache.synchronize(&rope, 24, 4);
            for line in 0..100 {
                cache.row(&rope, line);
            }
        }
        assert_eq!(cache.builds, builds);
        cache.mark_dirty(30, 30);
        rope.insert(rope.line_to_char(30), "'hello' ");
        cache.synchronize(&rope, 24, 4);
        for line in 0..100 {
            cache.row(&rope, line);
        }
        assert_eq!(cache.builds - builds, 3);
        assert_eq!(cache.row(&rope, 30)[0].glyph, Glyph::String);

        // A replacement without edit metadata must never reuse old glyphs.
        rope = Rope::from_str(&"123\n".repeat(100));
        cache.synchronize(&rope, 24, 4);
        assert_eq!(cache.row(&rope, 30)[0].glyph, Glyph::Number);
        cache.synchronize(&rope, 8, 4);
        assert_eq!(cache.row(&rope, 30).len(), 8);
    }

    #[test]
    fn minimap_cache_memory_is_bounded_after_scrolling_and_resizing() {
        let rope = Rope::from_str(&"界\tname = '123'\n".repeat(MAX_ROWS + 2));
        let mut cache = MinimapCache::default();
        cache.synchronize(&rope, MAX_COLUMNS, 4);
        for line in 0..MAX_ROWS + 2 {
            assert_eq!(cache.row(&rope, line).len(), MAX_COLUMNS);
            assert!(cache.rows.len() <= MAX_ROWS);
        }
        cache.synchronize(&rope, MAX_COLUMNS + 1, 4);
        for line in 0..10 {
            assert_eq!(cache.row(&rope, line).len(), MAX_COLUMNS + 1);
        }
        assert!(cache.rows.is_empty());
    }

    #[test]
    fn scrolling_past_capacity_keeps_visible_rows_and_recycles_the_evicted_storage() {
        let rope = Rope::from_str(&"name = 123;\n".repeat(MAX_ROWS * 3));
        let mut cache = MinimapCache::default();
        cache.synchronize(&rope, 24, 4);
        for line in 0..MAX_ROWS {
            cache.row(&rope, line);
        }
        let evicted = cache.rows.get(0).unwrap().as_ptr();
        for line in MAX_ROWS - 60..MAX_ROWS {
            cache.row(&rope, line);
        }
        cache.row(&rope, MAX_ROWS);
        assert_eq!(cache.rows.get(MAX_ROWS).unwrap().as_ptr(), evicted);
        assert_eq!(cache.rows.len(), MAX_ROWS);
        let builds = cache.builds;
        for line in MAX_ROWS - 60..=MAX_ROWS {
            cache.row(&rope, line);
        }
        assert_eq!(
            cache.builds, builds,
            "the existing viewport survives eviction"
        );
        assert!(cache.rows.get(0).is_none());
    }

    #[test]
    fn minimap_lru_matches_fresh_rows_after_eviction_edits_history_and_resize() {
        let mut rope = Rope::from_str(&"界\tname = '123'\r\n".repeat(MAX_ROWS * 3));
        let original = rope.clone();
        let mut cache = MinimapCache::default();
        for step in 0..8 {
            if step == 2 || step == 3 {
                cache.mark_dirty(600, 601);
                rope.insert(rope.line_to_char(600), if step == 2 { "42 " } else { "\n" });
            } else if step == 4 {
                cache.mark_dirty(0, rope.len_lines());
                rope = original.clone();
            } else if step == 5 {
                rope = Rope::from_str(&"0123\n".repeat(MAX_ROWS * 3));
            }
            let columns = if step == 6 { 3 } else { 24 };
            cache.synchronize(&rope, columns, 4);
            let mut fresh = Vec::new();
            for line in (0..MAX_ROWS * 3).rev().chain(0..MAX_ROWS * 3) {
                MinimapCache::decode(&mut fresh, &rope, line, columns, 4);
                assert_eq!(cache.row(&rope, line), fresh, "step={step}, line={line}");
                assert!(cache.rows.len() <= MAX_ROWS);
            }
        }
    }

    #[test]
    #[ignore = "manual optimized-profile minimap scrolling cache benchmark"]
    fn benchmark_minimap_cache_eviction() {
        use std::{hint::black_box, time::Instant};
        let rope = Rope::from_str(&("name = 'value'; ".repeat(8) + "\n").repeat(6_000));
        for reference in [true, false] {
            let mut times = Vec::new();
            let mut decoded = 0;
            for _ in 0..7 {
                let mut cache = MinimapCache {
                    reference_clear: reference,
                    ..Default::default()
                };
                cache.synchronize(&rope, 60, 4);
                for line in 0..MAX_ROWS {
                    cache.row(&rope, line);
                }
                let started = Instant::now();
                for scroll in MAX_ROWS - 60..5_000 {
                    for line in scroll..scroll + 60 {
                        black_box(cache.row(&rope, line));
                    }
                }
                times.push(started.elapsed());
                decoded = cache.builds;
            }
            times.sort();
            eprintln!(
                "minimap scroll clear={reference}: median={:?}, decoded={decoded}",
                times[3]
            );
        }
    }
}
