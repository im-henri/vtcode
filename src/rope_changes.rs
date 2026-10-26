//! Exact changed ranges and position mapping between immutable Rope snapshots.

/// A sorted, non-overlapping edit in original character offsets. `new_end`
/// includes the cumulative displacement of all preceding edits.
#[derive(Debug)]
pub(crate) struct EditOffset {
    pub start: usize,
    pub end: usize,
    pub new_end: usize,
}

pub(crate) fn map_offset(changes: &[EditOffset], cursor: usize) -> usize {
    // Insertions move a caret after new text; the start of a replacement stays
    // before it. Binary search avoids walking every edit for every caret.
    let index = changes.partition_point(|change| change.start < cursor || change.end == cursor);
    if index == 0 { cursor } else {
        let change = &changes[index - 1];
        change.new_end + cursor.saturating_sub(change.end)
    }
}

/// Compare shared chunks before their bytes. A local Rope edit keeps almost
/// every leaf shared, so unchanged text is skipped without decoding or hashing
/// the document. The exact byte comparison also handles undo/replaced Ropes.
fn matching_bytes<'a>(
    a: impl Iterator<Item = &'a str>,
    b: impl Iterator<Item = &'a str>,
    limit: usize,
    backwards: bool,
    cancelled: &impl Fn() -> bool,
) -> Option<usize> {
    let mut a = a.map(str::as_bytes);
    let mut b = b.map(str::as_bytes);
    let mut left = a.next().unwrap_or_default();
    let mut right = b.next().unwrap_or_default();
    let mut matched = 0;
    while matched < limit && !left.is_empty() && !right.is_empty() {
        if cancelled() {
            return None;
        }
        let len = left.len().min(right.len()).min(limit - matched);
        let (aa, bb) = if backwards {
            (&left[left.len() - len..], &right[right.len() - len..])
        } else {
            (&left[..len], &right[..len])
        };
        if aa.as_ptr() != bb.as_ptr() && aa != bb {
            matched += if backwards {
                aa.iter()
                    .rev()
                    .zip(bb.iter().rev())
                    .take_while(|(a, b)| a == b)
                    .count()
            } else {
                aa.iter().zip(bb).take_while(|(a, b)| a == b).count()
            };
            return Some(matched);
        }
        matched += len;
        if backwards {
            left = &left[..left.len() - len];
            right = &right[..right.len() - len];
        } else {
            left = &left[len..];
            right = &right[len..];
        }
        if left.is_empty() {
            left = a.next().unwrap_or_default();
        }
        if right.is_empty() {
            right = b.next().unwrap_or_default();
        }
    }
    Some(matched)
}

/// Exact equality without allocating text or comparing shared Rope leaves.
/// Size mismatches and live saved checkpoints take constant time.
pub(crate) fn equal(old: &ropey::Rope, new: &ropey::Rope) -> bool {
    old.is_instance(new)
        || (old.len_bytes() == new.len_bytes()
            && old.len_chars() == new.len_chars()
            && old.len_lines() == new.len_lines()
            && matching_bytes(old.chunks(), new.chunks(), old.len_bytes(), false, &|| false)
                == Some(old.len_bytes()))
}

pub(crate) fn equal_slices(old: ropey::RopeSlice<'_>, new: ropey::RopeSlice<'_>) -> bool {
    old.len_bytes() == new.len_bytes()
        && matching_bytes(old.chunks(), new.chunks(), old.len_bytes(), false, &|| false)
            == Some(old.len_bytes())
}

/// Minimal changed character ranges, rounded outwards at UTF-8 boundaries.
/// Shared Rope leaves are skipped; no full-document strings are allocated.
pub(crate) fn changed_chars(old: &ropey::Rope, new: &ropey::Rope)
    -> (std::ops::Range<usize>, std::ops::Range<usize>)
{
    if old.is_instance(new) { return (0..0, 0..0); }
    changed_slices(old.slice(..), new.slice(..))
}

/// Completion compares immutable snapshots speculatively. A new foreground
/// action must be able to stop even a large equal/independently reloaded Rope.
pub(crate) fn changed_chars_cancellable(old: &ropey::Rope, new: &ropey::Rope,
    cancelled: &impl Fn() -> bool)
    -> Option<(std::ops::Range<usize>, std::ops::Range<usize>)>
{
    if cancelled() { return None; }
    if old.is_instance(new) { return Some((0..0, 0..0)); }
    changed_slices_cancellable(old.slice(..), new.slice(..), cancelled)
}

/// The same exact comparison for a bounded region. Recovery can trim gaps
/// between shared anchors without flattening or copying unchanged text.
pub(crate) fn changed_slices(old: ropey::RopeSlice<'_>, new: ropey::RopeSlice<'_>)
    -> (std::ops::Range<usize>, std::ops::Range<usize>)
{
    changed_slices_cancellable(old, new, &|| false).unwrap()
}

fn changed_slices_cancellable(old: ropey::RopeSlice<'_>, new: ropey::RopeSlice<'_>,
    cancelled: &impl Fn() -> bool)
    -> Option<(std::ops::Range<usize>, std::ops::Range<usize>)>
{
    let common = old.len_bytes().min(new.len_bytes());
    let prefix = matching_bytes(old.chunks(), new.chunks(), common, false, cancelled)?;
    if prefix == old.len_bytes() && prefix == new.len_bytes() { return Some((0..0, 0..0)); }
    let start = old.byte_to_char(prefix);
    let suffix = matching_bytes(old.chunks_at_byte(old.len_bytes()).0.reversed(),
        new.chunks_at_byte(new.len_bytes()).0.reversed(),
        common - old.char_to_byte(start), true, cancelled)?;
    let end = |rope: ropey::RopeSlice<'_>| {
        let byte = rope.len_bytes() - suffix;
        let ch = rope.byte_to_char(byte);
        ch + usize::from(rope.char_to_byte(ch) != byte)
    };
    Some((start..end(old), start..end(new)))
}

pub(crate) fn changed_lines(
    old: &ropey::Rope,
    new: &ropey::Rope,
    cancelled: &impl Fn() -> bool,
) -> Option<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    if old.is_instance(new) {
        return (!cancelled()).then_some((0..0, 0..0));
    }
    let common = old.len_bytes().min(new.len_bytes());
    let prefix = matching_bytes(old.chunks(), new.chunks(), common, false, cancelled)?;
    if prefix == old.len_bytes() && prefix == new.len_bytes() {
        return Some((0..0, 0..0));
    }
    let prefix_char = old.byte_to_char(prefix);
    let prefix_byte = old.char_to_byte(prefix_char);
    // A shared trailing CR can be a complete old line break but only the
    // first half of a new CRLF. Invalidate the earlier logical line in either
    // Rope rather than treating equal prefix characters as equal line maps.
    let first = old
        .char_to_line(prefix_char)
        .min(new.char_to_line(prefix_char));
    let suffix = matching_bytes(
        old.chunks_at_byte(old.len_bytes()).0.reversed(),
        new.chunks_at_byte(new.len_bytes()).0.reversed(),
        common - prefix_byte,
        true,
        cancelled,
    )?;
    if suffix == 0 {
        return Some((first..old.len_lines(), first..new.len_lines()));
    }
    let suffix_position = |rope: &ropey::Rope| {
        let byte = rope.len_bytes() - suffix;
        let mut ch = rope.byte_to_char(byte);
        if rope.char_to_byte(ch) != byte {
            ch += 1;
        }
        let line = rope.char_to_line(ch);
        (line, rope.line_to_char(line) == ch)
    };
    let (old_line, old_boundary) = suffix_position(old);
    let (new_line, new_boundary) = suffix_position(new);
    // The first whole reusable line must begin at the SAME position inside
    // the shared suffix. If either Rope starts partway through that line,
    // skip its remainder in both, including CRLF split/join boundaries.
    let skip = usize::from(!old_boundary || !new_boundary);
    Some((first..old_line + skip, first..new_line + skip))
}
