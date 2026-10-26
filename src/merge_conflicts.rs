//! Conflict syntax and incremental indexing. Snapshots share Rope leaves;
//! only changed lines are rescanned, and indexing runs off the UI thread.
use ropey::Rope;
use std::{ops::Range, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Current,
    Base,
    Divider,
    Incoming,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Marker {
    line: usize,
    width: usize,
    kind: Kind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Region {
    pub start: usize,
    pub base: Option<usize>,
    pub divider: usize,
    pub end: usize,
}

impl Region {
    pub fn current(self) -> Range<usize> {
        self.start + 1..self.base.unwrap_or(self.divider)
    }
    pub fn incoming(self) -> Range<usize> {
        self.divider + 1..self.end
    }
    pub fn content_kind(self, line: usize) -> Option<usize> {
        if self.current().contains(&line) {
            Some(0)
        } else if self.incoming().contains(&line) {
            Some(1)
        } else if self
            .base
            .is_some_and(|base| line > base && line < self.divider)
        {
            Some(2)
        } else {
            None
        }
    }
}

fn marker(rope: &Rope, line: usize) -> Option<Marker> {
    let mut chars = rope.get_line(line)?.chars().peekable();
    let first = *chars.peek()?;
    let kind = match first {
        '<' => Kind::Current,
        '|' => Kind::Base,
        '=' => Kind::Divider,
        '>' => Kind::Incoming,
        _ => return None,
    };
    let width = chars.by_ref().take_while(|&ch| ch == first).count();
    if width < 7 {
        return None;
    }
    // take_while consumes the separator, so inspect it by character offset.
    let text = rope.line(line);
    if text.get_char(width).is_some_and(|ch| !ch.is_whitespace()) {
        return None;
    }
    if kind == Kind::Divider && text.chars_at(width).any(|ch| !ch.is_whitespace()) {
        return None;
    }
    Some(Marker { line, width, kind })
}

fn regions(markers: &[Marker]) -> Vec<Region> {
    let mut result = Vec::new();
    let mut pending: Option<(Marker, Option<usize>, Option<usize>)> = None;
    let mut nested = 0usize;
    for &m in markers {
        if m.kind == Kind::Current {
            if pending.is_some() {
                nested += 1;
            } else {
                pending = Some((m, None, None));
            }
            continue;
        }
        if nested > 0 {
            if m.kind == Kind::Incoming {
                nested -= 1;
                if nested == 0 {
                    pending = None;
                }
            }
            continue;
        }
        let Some((start, base, divider)) = pending.as_mut() else {
            continue;
        };
        if m.width != start.width {
            pending = None;
            continue;
        }
        match m.kind {
            Kind::Base if base.is_none() && divider.is_none() => *base = Some(m.line),
            Kind::Divider if divider.is_none() => *divider = Some(m.line),
            Kind::Incoming => {
                if let Some(divider) = *divider {
                    result.push(Region {
                        start: start.line,
                        base: *base,
                        divider,
                        end: m.line,
                    });
                }
                pending = None;
            }
            _ => pending = None,
        }
    }
    result
}

/// Point validation never trusts an old rendered control or a stale index.
pub(crate) fn at(rope: &Rope, start: usize) -> Option<Region> {
    let first = marker(rope, start)?;
    if first.kind != Kind::Current {
        return None;
    }
    let mut found = vec![first];
    for line in start + 1..rope.len_lines() {
        if let Some(m) = marker(rope, line) {
            if m.kind == Kind::Current {
                return None;
            }
            found.push(m);
            if m.kind == Kind::Incoming {
                return regions(&found).into_iter().next();
            }
        }
    }
    None
}

pub(crate) fn line_chars(rope: &Rope, lines: Range<usize>) -> Range<usize> {
    let offset = |line| {
        if line < rope.len_lines() {
            rope.line_to_char(line)
        } else {
            rope.len_chars()
        }
    };
    offset(lines.start)..offset(lines.end)
}

fn scan(rope: &Rope, lines: Range<usize>, output: &mut Vec<Marker>) {
    let absolute = rope.char_to_byte(line_chars(rope, lines.clone()).start);
    let text = rope.slice(line_chars(rope, lines));
    // Slice chunks preserve UTF-8 and CRLF; jump over complete line bodies
    // using memchr, without allocating Strings or seeking every Rope line.
    let mut relative = 0usize;
    let mut at_start = true;
    for chunk in text.chunks() {
        let bytes = chunk.as_bytes();
        let mut cursor = 0;
        while cursor < bytes.len() {
            if at_start && matches!(bytes[cursor], b'<' | b'>' | b'|' | b'=') {
                let global = absolute + relative + cursor;
                let line = rope.byte_to_line(global);
                if let Some(m) = marker(rope, line) {
                    output.push(m);
                }
            }
            if let Some(next) = memchr::memchr2(b'\n', b'\r', &bytes[cursor..]) {
                cursor += next + 1;
                at_start = true;
            } else {
                at_start = false;
                break;
            }
        }
        relative += chunk.len();
    }
}

pub(crate) struct Index {
    pub source: Rope,
    markers: Vec<Marker>,
    pub regions: Vec<Region>,
    #[cfg(test)]
    pub scanned_lines: usize,
}

impl Index {
    pub fn update(previous: Option<&Self>, source: Rope) -> Self {
        let (old, new) = previous
            .and_then(|old| crate::rope_changes::changed_lines(&old.source, &source, &|| false))
            .unwrap_or((0..0, 0..source.len_lines()));
        let mut markers = Vec::new();
        if let Some(previous) = previous {
            markers.extend(
                previous
                    .markers
                    .iter()
                    .copied()
                    .take_while(|m| m.line < old.start),
            );
        }
        scan(&source, new.clone(), &mut markers);
        if let Some(previous) = previous {
            markers.extend(
                previous
                    .markers
                    .iter()
                    .filter(|m| m.line >= old.end)
                    .map(|m| Marker {
                        line: new.end + (m.line - old.end),
                        ..*m
                    }),
            );
        }
        let regions = regions(&markers);
        Self {
            source,
            markers,
            regions,
            #[cfg(test)]
            scanned_lines: new.len(),
        }
    }
    pub fn region_for_line(&self, line: usize) -> Option<Region> {
        let position = self
            .regions
            .partition_point(|region| region.start <= line)
            .checked_sub(1)?;
        self.regions
            .get(position)
            .copied()
            .filter(|region| line <= region.end)
    }
    pub fn has_markers(&self) -> bool {
        !self.markers.is_empty()
    }
}

#[derive(Default)]
pub(crate) struct Cache {
    pub index: Option<Arc<Index>>,
    pub pending: bool,
    pub review: Option<Review>,
    pub comparing: bool,
    pub had_conflicts: bool,
    pub hover: Option<ratatui::layout::Rect>,
}

pub(crate) struct Review {
    pub source: Rope,
    pub region: Region,
    pub scroll: usize,
    pub chrome: crate::merge_conflict_ui::Chrome,
}

impl Cache {
    pub fn current(&self, rope: &Rope) -> Option<&Index> {
        self.index
            .as_deref()
            .filter(|index| index.source.is_instance(rope))
    }
}

/// A staging check is streaming and allocation-free, including binary files
/// and very long lines. Reject leftover markers only for paths Git marks as
/// conflicted, so ordinary tracked examples containing markers remain valid.
pub(crate) fn has_markers_in_reader(mut input: impl std::io::Read) -> std::io::Result<bool> {
    let mut buffer = [0; 16 * 1024];
    let (mut beginning, mut run, mut symbol) = (true, 0usize, 0u8);
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            return Ok(run >= 7);
        }
        for &byte in &buffer[..count] {
            if beginning {
                if run == 0 && matches!(byte, b'<' | b'>' | b'|' | b'=') {
                    symbol = byte;
                    run = 1;
                } else if run > 0 && byte == symbol {
                    run += 1;
                } else {
                    if run >= 7 && byte.is_ascii_whitespace() {
                        return Ok(true);
                    }
                    beginning = false;
                    run = 0;
                }
            }
            if matches!(byte, b'\r' | b'\n') {
                beginning = true;
                run = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;

    const CONFLICT: &str = "before\n<<<<<<< HEAD\nours 🌲\n||||||| base\nancestor must disappear\n=======\ntheirs 界\n>>>>>>> topic\nafter\n";

    #[test]
    fn merge_conflict_accept_choices_exclude_ancestor_and_are_one_undo_step() {
        for (action, expected) in [
            ("current", "ours 🌲\n"),
            ("incoming", "theirs 界\n"),
            ("both", "ours 🌲\ntheirs 界\n"),
        ] {
            for ending in ["\n", "\r\n", "\r"] {
                let original = CONFLICT.replace('\n', ending);
                let mut editor = Editor::test_with_text(&original);
                editor.saved_rope = Some(editor.rope.clone());
                editor.selection_anchor = Some((8, 2));
                editor.extra_cursors.push((8, 2));
                assert!(editor.resolve_conflict(1, action));
                assert_eq!(
                    editor.rope.to_string(),
                    format!("before\n{expected}after\n").replace('\n', ending)
                );
                assert!(editor.modified);
                assert!(editor.selection_anchor.is_none());
                assert!(editor.extra_cursors.is_empty());
                assert!(editor.cursor_line < editor.line_count());
                editor.undo();
                assert_eq!(editor.rope.to_string(), original);
                assert!(!editor.modified);
                editor.redo();
                assert_eq!(
                    editor.rope.to_string(),
                    format!("before\n{expected}after\n").replace('\n', ending)
                );
            }
        }
    }

    #[test]
    fn merge_conflict_rejects_malformed_stale_invalid_and_read_only_actions() {
        for text in [
            "<<<<<<< HEAD\nours\n=======\ntheirs\n",
            "<<<<<<< HEAD\na\n=======\nb\n=======\nc\n>>>>>>> incoming\n",
            "<<<<<<< HEAD\n<<<<<<< nested\na\n=======\nb\n>>>>>>> nested\n=======\nc\n>>>>>>> incoming\n",
            "<<<<<<< HEAD\na\n========\nb\n>>>>>>> incoming\n",
            "<<<<<<<not-a-marker\na\n=======\nb\n>>>>>>> incoming\n",
        ] {
            let mut editor = Editor::test_with_text(text);
            let before = editor.rope.clone();
            assert!(!editor.resolve_conflict(0, "current"), "{text}");
            assert!(editor.rope.is_instance(&before));
            assert!(editor.undo_stack.is_empty());
        }
        let mut editor = Editor::test_with_text(CONFLICT);
        assert!(!editor.resolve_conflict(usize::MAX, "current"));
        assert!(!editor.resolve_conflict(1, "invalid"));
        editor.read_only = true;
        assert!(!editor.resolve_conflict(1, "both"));
        assert!(editor.undo_stack.is_empty());
        assert_eq!(editor.rope.to_string(), CONFLICT);
    }

    #[test]
    fn merge_conflict_empty_sides_eof_and_custom_marker_widths() {
        for (text, action, expected) in [
            ("<<<<<<< HEAD\n=======\n>>>>>>> incoming", "both", ""),
            (
                "<<<<<<< HEAD\n=======\nincoming\n>>>>>>> incoming",
                "current",
                "",
            ),
            (
                "<<<<<<<<<<< HEAD\ncurrent\n===========\nincoming\n>>>>>>>>>>> topic",
                "both",
                "current\nincoming\n",
            ),
        ] {
            let mut editor = Editor::test_with_text(text);
            assert!(editor.resolve_conflict(0, action));
            assert_eq!(editor.rope.to_string(), expected);
            assert_eq!((editor.cursor_line, editor.cursor_col), (0, 0));
            editor.undo();
            assert_eq!(editor.rope.to_string(), text);
        }
    }

    #[test]
    fn merge_conflict_index_reuses_unchanged_text_and_tracks_line_shifts_and_undo() {
        let mut rope =
            Rope::from_str(&("ordinary\n".repeat(20_000) + CONFLICT + &"tail\n".repeat(20_000)));
        let original = rope.clone();
        let first = Index::update(None, rope.clone());
        assert_eq!(first.regions.len(), 1);
        rope.insert(rope.line_to_char(10), "new line\n");
        let edited = Index::update(Some(&first), rope.clone());
        assert!(
            edited.scanned_lines <= 2,
            "only changed lines should be scanned"
        );
        assert_eq!(edited.regions[0].start, first.regions[0].start + 1);
        assert_eq!(edited.regions, Index::update(None, rope.clone()).regions);
        let restored = Index::update(Some(&edited), original);
        assert_eq!(restored.regions, first.regions);
        assert!(restored.scanned_lines <= 2);
        let end = rope.line_to_char(edited.regions[0].divider);
        rope.remove(end..end + 1);
        let partial = Index::update(Some(&edited), rope.clone());
        assert!(partial.regions.is_empty());
        rope.insert(end, "=");
        let repaired = Index::update(Some(&partial), rope);
        assert_eq!(repaired.regions, edited.regions);
    }

    #[test]
    fn merge_conflict_index_handles_markers_crossing_rope_chunks_and_lf_crlf() {
        for ending in ["\n", "\r\n", "\r"] {
            for prefix in [0, 991, 1000, 1013, 2027] {
                let text = format!("{}\n{CONFLICT}", "x".repeat(prefix)).replace('\n', ending);
                let rope = Rope::from_str(&text);
                let index = Index::update(None, rope);
                assert_eq!(
                    index.regions,
                    vec![Region {
                        start: 2,
                        base: Some(4),
                        divider: 6,
                        end: 8
                    }]
                );
            }
        }
    }

    #[test]
    fn merge_conflict_incremental_index_matches_full_scans_through_repeated_edits() {
        let mut rope =
            Rope::from_str(&(CONFLICT.to_owned() + &"plain 界 🌲\r\n".repeat(300) + CONFLICT));
        let original = rope.clone();
        let mut index = Index::update(None, rope.clone());
        let mut state = 0xcafe_u64;
        let inserts = [
            "x",
            "界",
            "\r",
            "\n",
            "\r\n",
            "<<<<<<< HEAD\n",
            "=======\n",
            ">>>>>>> topic\n",
            CONFLICT,
        ];
        for step in 0..400 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let start = state as usize % (rope.len_chars() + 1);
            if step % 17 == 0 {
                rope = original.clone();
            } else if step % 3 == 0 && start < rope.len_chars() {
                rope.remove(start..(start + (state as usize % 40) + 1).min(rope.len_chars()));
            } else {
                rope.insert(start, inserts[step % inserts.len()]);
            }
            index = Index::update(Some(&index), rope.clone());
            let full = Index::update(None, rope.clone());
            assert_eq!(index.markers, full.markers, "step {step}");
            assert_eq!(index.regions, full.regions, "step {step}");
            let same = Index::update(Some(&index), rope.clone());
            assert_eq!(same.scanned_lines, 0);
            assert_eq!(same.regions, index.regions);
        }
    }

    #[test]
    fn merge_conflict_streaming_stage_check_handles_chunk_edges_and_long_lines() {
        use std::io::Read;
        struct Short<'a>(&'a [u8]);
        impl Read for Short<'_> {
            fn read(&mut self, target: &mut [u8]) -> std::io::Result<usize> {
                let count = target.len().min(3).min(self.0.len());
                target[..count].copy_from_slice(&self.0[..count]);
                self.0 = &self.0[count..];
                Ok(count)
            }
        }
        assert!(has_markers_in_reader(Short(CONFLICT.as_bytes())).unwrap());
        assert!(has_markers_in_reader(&b"ordinary\r<<<<<<< HEAD"[..]).unwrap());
        assert!(!has_markers_in_reader(&b"x<<<<<<< HEAD\n<<<<<<<example\n"[..]).unwrap());
        assert!(!has_markers_in_reader(std::io::repeat(b'x').take(10_000_000)).unwrap());
    }
}
