use crate::editor::Editor;
use crate::rope_line_blocks::{BLOCK_LINES, Block};
use ropey::Rope;
use std::{sync::Arc, time::Instant};

/// The same limit applies to analysis and rendering, so skipped pathological
/// lines cannot change the depth of subsequent visible source lines.
pub(crate) const MAX_BRACKET_LINE_CHARS: usize = 5_000;
const MAX_DEPTH_BYTES: usize = 16 * 1024 * 1024;
const MAX_SOURCE_BYTES: usize = 32 * 1024 * 1024;
const MAX_SOURCE_LINES: usize = 2_000_000;
const _: () = assert!(BLOCK_LINES <= u8::MAX as usize + 1);

/// An exact depth transfer: output = max(input + delta, floor). Unlike a net
/// bracket count this preserves unmatched closing brackets, which clamp at 0.
/// The source-byte bound keeps all sums well within i32 without saturation.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Transfer {
    delta: i32,
    floor: i32,
}
impl Transfer {
    fn apply(self, depth: i32) -> i32 {
        (depth + self.delta).max(self.floor)
    }
    fn bracket(&mut self, open: bool) {
        if open {
            self.delta += 1;
            self.floor += 1;
        } else {
            self.delta -= 1;
            self.floor = (self.floor - 1).max(0);
        }
    }
}

struct Checkpoint {
    line: u8,
    transfer: Transfer,
}
struct Summary {
    checkpoints: Box<[Checkpoint]>,
    end: Transfer,
    has_brackets: bool,
}

/// Immutable, document-coordinate index. Unchanged 128-line summaries stay
/// shared across edits; only block starting depths are propagated afterwards.
/// Queries use two small binary searches, never a whole-document prefix scan.
pub struct BracketDepthIndex {
    source: Rope,
    navigation_id: u64,
    content_version: u64,
    wrap_revision: u64,
    blocks: Box<[Block<Summary>]>,
    depths: Box<[i32]>,
    enabled: bool,
    pub(crate) has_brackets: bool,
    #[cfg(test)]
    pub(crate) scanned_lines: usize,
}

impl BracketDepthIndex {
    #[cfg(test)]
    pub(crate) fn build(
        source: Rope,
        navigation_id: u64,
        content_version: u64,
        wrap_revision: u64,
    ) -> Self {
        Self::build_with_limit(
            source,
            navigation_id,
            content_version,
            wrap_revision,
            MAX_DEPTH_BYTES,
        )
    }

    #[cfg(test)]
    pub(crate) fn build_until(
        source: Rope,
        navigation_id: u64,
        content_version: u64,
        wrap_revision: u64,
        deadline: Instant,
    ) -> Self {
        Self::build_inner(
            source,
            navigation_id,
            content_version,
            wrap_revision,
            MAX_DEPTH_BYTES,
            (MAX_SOURCE_BYTES, MAX_SOURCE_LINES),
            Some(deadline),
        )
    }

    #[cfg(test)]
    pub(crate) fn build_cancellable(
        source: Rope,
        navigation_id: u64,
        content_version: u64,
        wrap_revision: u64,
        deadline: Instant,
        cancelled: &impl Fn() -> bool,
    ) -> Option<Self> {
        Self::update_cancellable(
            None,
            source,
            navigation_id,
            content_version,
            wrap_revision,
            deadline,
            cancelled,
        )
    }

    /// A cancelled request publishes nothing. Resource limits still produce a
    /// disabled snapshot, so the same expensive work is not retried each frame.
    pub(crate) fn update_cancellable(
        previous: Option<&Self>,
        source: Rope,
        navigation_id: u64,
        content_version: u64,
        wrap_revision: u64,
        deadline: Instant,
        cancelled: &impl Fn() -> bool,
    ) -> Option<Self> {
        Self::build_inner_cancellable(
            previous,
            source,
            navigation_id,
            content_version,
            wrap_revision,
            MAX_DEPTH_BYTES,
            (MAX_SOURCE_BYTES, MAX_SOURCE_LINES),
            Some(deadline),
            cancelled,
        )
    }

    #[cfg(test)]
    fn build_with_limit(
        source: Rope,
        navigation_id: u64,
        content_version: u64,
        wrap_revision: u64,
        max_bytes: usize,
    ) -> Self {
        Self::build_inner(
            source,
            navigation_id,
            content_version,
            wrap_revision,
            max_bytes,
            (MAX_SOURCE_BYTES, MAX_SOURCE_LINES),
            None,
        )
    }

    #[cfg(test)]
    fn build_inner(
        source: Rope,
        navigation_id: u64,
        content_version: u64,
        wrap_revision: u64,
        max_bytes: usize,
        work_limits: (usize, usize),
        deadline: Option<Instant>,
    ) -> Self {
        Self::build_inner_cancellable(
            None,
            source,
            navigation_id,
            content_version,
            wrap_revision,
            max_bytes,
            work_limits,
            deadline,
            &|| false,
        )
        .expect("uncancelled index")
    }

    fn build_inner_cancellable(
        previous: Option<&Self>,
        source: Rope,
        navigation_id: u64,
        content_version: u64,
        wrap_revision: u64,
        max_bytes: usize,
        work_limits: (usize, usize),
        deadline: Option<Instant>,
        cancelled: &impl Fn() -> bool,
    ) -> Option<Self> {
        if cancelled() {
            return None;
        }
        let mut index = Self {
            source,
            navigation_id,
            content_version,
            wrap_revision,
            blocks: Box::default(),
            depths: Box::default(),
            enabled: false,
            has_brackets: false,
            #[cfg(test)]
            scanned_lines: 0,
        };
        if index.source.len_bytes() > work_limits.0 || index.source.len_lines() > work_limits.1 {
            return Some(index);
        }
        let previous = previous.filter(|old| old.enabled && old.navigation_id == navigation_id);
        let stopped = || cancelled() || deadline.is_some_and(|end| Instant::now() >= end);
        index.enabled = index
            .populate(previous, max_bytes, &stopped, cancelled)
            .is_some();
        if cancelled() { None } else { Some(index) }
    }

    fn populate(
        &mut self,
        previous: Option<&Self>,
        max_bytes: usize,
        stopped: &impl Fn() -> bool,
        cancelled: &impl Fn() -> bool,
    ) -> Option<()> {
        if stopped() {
            return None;
        }
        let mut blocks = Vec::new();
        let mut retained = 0;
        let mut push = |block: Block<Summary>| -> Option<()> {
            retained += std::mem::size_of::<Block<Summary>>()
                + std::mem::size_of::<i32>()
                + std::mem::size_of::<Summary>()
                + 2 * std::mem::size_of::<usize>()
                + std::mem::size_of_val(&*block.summary.checkpoints);
            if retained > max_bytes {
                return None;
            }
            blocks.push(block);
            Some(())
        };
        let mut rescan = 0..self.source.len_lines();
        let mut suffix = &[][..];
        if let Some(old) = previous {
            let plan =
                crate::rope_line_blocks::plan(&old.blocks, &old.source, &self.source, stopped)?;
            for block in &old.blocks[..plan.prefix] {
                if stopped() {
                    return None;
                }
                push(block.clone())?;
            }
            rescan = plan.rescan;
            suffix = &old.blocks[plan.suffix..];
        }
        let mut start = rescan.start;
        let mut lines = self.source.lines_at(start);
        while start < rescan.end {
            if stopped() {
                return None;
            }
            let count = BLOCK_LINES.min(rescan.end - start);
            let mut transfer = Transfer::default();
            let mut last = transfer;
            let mut checkpoints = Vec::new();
            let mut has_brackets = false;
            for (offset, line) in lines.by_ref().take(count).enumerate() {
                if cancelled() {
                    return None;
                }
                if transfer != last {
                    checkpoints.push(Checkpoint {
                        line: offset as u8,
                        transfer,
                    });
                    last = transfer;
                }
                #[cfg(test)]
                {
                    self.scanned_lines += 1;
                }
                if line.len_chars() > MAX_BRACKET_LINE_CHARS {
                    continue;
                }
                // UTF-8 continuation bytes cannot equal ASCII delimiters.
                for byte in line.chunks().flat_map(str::bytes) {
                    match byte {
                        b'(' | b'[' | b'{' => {
                            has_brackets = true;
                            transfer.bracket(true);
                        }
                        b')' | b']' | b'}' => {
                            has_brackets = true;
                            transfer.bracket(false);
                        }
                        _ => {}
                    }
                }
            }
            push(Block {
                start,
                lines: count,
                summary: Arc::new(Summary {
                    checkpoints: checkpoints.into_boxed_slice(),
                    end: transfer,
                    has_brackets,
                }),
            })?;
            start += count;
        }
        for block in suffix {
            if stopped() {
                return None;
            }
            push(Block {
                start,
                lines: block.lines,
                summary: block.summary.clone(),
            })?;
            start += block.lines;
        }
        let mut depth = 0;
        let mut depths = Vec::with_capacity(blocks.len());
        let mut has_brackets = false;
        for block in &blocks {
            if stopped() {
                return None;
            }
            depths.push(depth);
            depth = block.summary.end.apply(depth);
            has_brackets |= block.summary.has_brackets;
        }
        self.blocks = blocks.into_boxed_slice();
        self.depths = depths.into_boxed_slice();
        self.has_brackets = has_brackets;
        Some(())
    }

    pub(crate) fn matches(&self, editor: &Editor) -> bool {
        self.navigation_id == editor.navigation_id
            && self.content_version == editor.content_version
            && self.wrap_revision == editor.wrap_revision
            && self.source.is_instance(&editor.rope)
    }

    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn line_starting_depth(&self, line: usize) -> Option<i32> {
        if !self.enabled || line >= self.source.len_lines() {
            return None;
        }
        let index = self.blocks.partition_point(|block| block.start <= line) - 1;
        let block = &self.blocks[index];
        let checkpoints = &block.summary.checkpoints;
        let end =
            checkpoints.partition_point(|point| usize::from(point.line) <= line - block.start);
        let transfer = end
            .checked_sub(1)
            .map_or(Transfer::default(), |i| checkpoints[i].transfer);
        Some(transfer.apply(self.depths[index]))
    }

    /// Palette depth at either delimiter of a pair, including an offscreen or
    /// selected endpoint. Never scans more than one bounded source line.
    pub(crate) fn depth_at(&self, editor: &Editor, line: usize, col: usize) -> Option<usize> {
        if !self.matches(editor) {
            return None;
        }
        let mut depth = self.line_starting_depth(line)?;
        let source = self.source.line(line);
        if source.len_chars() > MAX_BRACKET_LINE_CHARS {
            return None;
        }
        for (column, ch) in source.chars().enumerate() {
            match ch {
                '(' | '[' | '{' => {
                    if column == col {
                        return Some(depth as usize);
                    }
                    depth = depth.saturating_add(1);
                }
                ')' | ']' | '}' => {
                    depth = (depth - 1).max(0);
                    if column == col {
                        return Some(depth as usize);
                    }
                }
                _ => {}
            }
            if column >= col {
                break;
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_discards_partial_index_and_allows_a_fresh_build() {
        let source = Rope::from_str(&"{\n界(value);\n}\r\n".repeat(1_000));
        let checks = std::cell::Cell::new(0);
        let result = BracketDepthIndex::build_cancellable(
            source.clone(),
            1,
            2,
            3,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
            &|| {
                checks.set(checks.get() + 1);
                checks.get() >= 8
            },
        );
        assert!(result.is_none());
        assert!((8..=9).contains(&checks.get()));
        let index = BracketDepthIndex::build(source, 1, 2, 3);
        assert!(index.enabled());
        assert_eq!(index.line_starting_depth(2_998), Some(1));
    }

    #[test]
    #[ignore = "manual performance measurement; run on one logical processor"]
    fn benchmark_single_core_bracket_index() {
        use std::{hint::black_box, time::Instant};
        // The original character iterator, retained as a reference for exactly
        // the same compressed index and source-line limit.
        fn reference(source: &Rope) -> Vec<(usize, i32)> {
            let mut checkpoints = vec![(0, 0)];
            let mut depth = 0i32;
            for (line_idx, line) in source.lines().enumerate() {
                if checkpoints.last().unwrap().1 != depth {
                    checkpoints.push((line_idx, depth));
                }
                if line.len_chars() > MAX_BRACKET_LINE_CHARS {
                    continue;
                }
                for ch in line.chars() {
                    match ch {
                        '(' | '[' | '{' => depth = depth.saturating_add(1),
                        ')' | ']' | '}' => depth = (depth - 1).max(0),
                        _ => {}
                    }
                }
            }
            checkpoints
        }
        for (label, text) in [
            (
                "ASCII",
                "fn calculate(value: usize) {\n    let output = value + 1;\n    call(output);\n}\n",
            ),
            (
                "Unicode",
                "fn calculate() {\n    call(\"界 café 🦀 αβγ\");\n}\n",
            ),
        ] {
            let source = Rope::from_str(&text.repeat(15_000));
            let expected = reference(&source);
            assert_eq!(
                {
                    let index = BracketDepthIndex::build(source.clone(), 1, 0, 0);
                    let mut points = vec![(0, 0)];
                    for line in 0..source.len_lines() {
                        let depth = index.line_starting_depth(line).unwrap();
                        if points.last().unwrap().1 != depth {
                            points.push((line, depth));
                        }
                    }
                    points
                },
                expected
            );
            let mut old = Vec::new();
            let mut new = Vec::new();
            for iteration in 0..10 {
                for faster in [iteration % 2 == 0, iteration % 2 != 0] {
                    let start = Instant::now();
                    if faster {
                        black_box(BracketDepthIndex::build(black_box(source.clone()), 1, 0, 0));
                        new.push(start.elapsed());
                    } else {
                        black_box(reference(black_box(&source)));
                        old.push(start.elapsed());
                    }
                }
            }
            old.sort_unstable();
            new.sort_unstable();
            eprintln!(
                "single core bracket index {label}: chars={:?}, bytes={:?}, speedup={:.2}x",
                old[5],
                new[5],
                old[5].as_secs_f64() / new[5].as_secs_f64()
            );
        }
    }

    fn build(editor: &Editor) -> BracketDepthIndex {
        BracketDepthIndex::build(
            editor.rope.clone(),
            editor.navigation_id,
            editor.content_version,
            editor.wrap_revision,
        )
    }

    #[test]
    fn source_depth_survives_moving_lookback_boundaries_and_hidden_lines() {
        let text = format!(
            "{{\n{}[\n{}]}}\n",
            "plain\n".repeat(330),
            "plain\n".repeat(17)
        );
        let editor = Editor::test_with_text(&text);
        let index = build(&editor);
        assert_eq!(index.line_starting_depth(199), Some(1));
        assert_eq!(index.line_starting_depth(200), Some(1));
        assert_eq!(index.line_starting_depth(201), Some(1));
        assert_eq!(index.depth_at(&editor, 331, 0), Some(1));
        // A projection can skip all intervening lines and still query the
        // closing endpoint with the same absolute depth as its opener.
        assert_eq!(index.depth_at(&editor, 349, 0), Some(1));
        assert_eq!(index.depth_at(&editor, 349, 1), Some(0));
    }

    #[test]
    fn source_identity_rejects_other_editors_equal_version_reload_and_dirty_revision() {
        let mut editor = Editor::test_with_text("{\n}\n");
        let index = build(&editor);
        assert!(index.matches(&editor));
        let other = Editor::test_with_text("{\n}\n");
        assert!(!index.matches(&other));
        editor.rope = Rope::from_str("[\n]\n");
        assert!(!index.matches(&editor));
        let index = build(&editor);
        editor.wrap_revision += 1;
        assert!(!index.matches(&editor));
        let index = build(&editor);
        editor.content_version += 1;
        assert!(!index.matches(&editor));
    }

    #[test]
    fn compressed_depth_queries_match_a_full_prefix_oracle_for_unicode_crlf_and_malformed_brackets()
    {
        let text = [
            "界]\r\n",
            "([{\r\n",
            "empty\r\n",
            "\"{\"}\n",
            "}])\n",
            "[🦀]\n",
            "\n",
        ]
        .repeat(100)
        .concat();
        let editor = Editor::test_with_text(&text);
        let index = build(&editor);
        let mut depth = 0i32;
        for (line_idx, line) in editor.rope.lines().enumerate() {
            assert_eq!(index.line_starting_depth(line_idx), Some(depth));
            for (col, ch) in line.chars().enumerate() {
                match ch {
                    '(' | '[' | '{' => {
                        assert_eq!(index.depth_at(&editor, line_idx, col), Some(depth as usize));
                        depth += 1;
                    }
                    ')' | ']' | '}' => {
                        depth = (depth - 1).max(0);
                        assert_eq!(index.depth_at(&editor, line_idx, col), Some(depth as usize));
                    }
                    _ => assert_eq!(index.depth_at(&editor, line_idx, col), None),
                }
            }
        }
        assert_eq!(index.line_starting_depth(editor.line_count()), None);
    }

    #[test]
    fn pathological_lines_are_consistently_excluded_and_storage_is_bounded() {
        let mut editor = Editor::test_with_text(&format!(
            "{}\n[\n]\n",
            "{".repeat(MAX_BRACKET_LINE_CHARS + 1)
        ));
        let index = build(&editor);
        assert_eq!(index.depth_at(&editor, 0, 0), None);
        assert_eq!(index.depth_at(&editor, 1, 0), Some(0));
        editor.rope = Rope::from_str("{\n{\n{\n{\n");
        let index = BracketDepthIndex::build_with_limit(
            editor.rope.clone(),
            editor.navigation_id,
            editor.content_version,
            editor.wrap_revision,
            std::mem::size_of::<(usize, i32)>() * 2,
        );
        assert!(index.matches(&editor));
        assert!(!index.enabled());
        assert!(index.blocks.is_empty());
        assert_eq!(index.depth_at(&editor, 0, 0), None);
    }

    #[test]
    fn excessive_work_and_expired_deadlines_produce_completed_disabled_snapshots() {
        let editor = Editor::test_with_text("{}\n{}\n{}\n");
        for work_limits in [(4, usize::MAX), (usize::MAX, 2)] {
            let index = BracketDepthIndex::build_inner(
                editor.rope.clone(),
                editor.navigation_id,
                editor.content_version,
                editor.wrap_revision,
                MAX_DEPTH_BYTES,
                work_limits,
                None,
            );
            assert!(index.matches(&editor));
            assert!(!index.enabled());
            assert!(index.blocks.is_empty());
        }
        let index = BracketDepthIndex::build_until(
            editor.rope.clone(),
            editor.navigation_id,
            editor.content_version,
            editor.wrap_revision,
            std::time::Instant::now(),
        );
        assert!(index.matches(&editor));
        assert!(!index.enabled());
        assert!(index.blocks.is_empty());
    }
}

#[cfg(test)]
#[path = "bracket_depth_incremental_tests.rs"]
mod incremental_tests;
