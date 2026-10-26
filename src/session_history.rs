//! Recovery histories: one base and edits between shared Ropes. Unsaved buffers
//! supply their already-persisted text as the base instead of duplicating it.
//! Restore clones the previous tree and replaces only changed regions, keeping
//! both text leaves and untouched subtrees shared across undo/redo snapshots.
use super::{SavedUndoEntry, UndoStack};
use ropey::Rope;
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::ops::Range;

const MAGIC: &[u8] = b"VTCODE_UNDO\0\x02";
const SHARED_BASE_MAGIC: &[u8] = b"VTCODE_UNDO\0\x03";
const FULL: u8 = 0;
const DELTA: u8 = 1;
const REFERENCE: u8 = 2;
// Optional diff anchors must not require an index proportional to file size.
// Sampling leaf identities bounds the hash table. Compact repeat-position runs
// retain exact matches without storing every repeated occurrence separately.
const MAX_CHUNK_ANCHORS: usize = 4096;

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid delta undo recovery content",
    )
}

// Repeat placements are usually regular after Rope appends. Keep their first
// arithmetic run inline instead of allocating one offset per repeated leaf.
#[derive(Clone, Copy)]
struct PositionRun {
    first: usize,
    last: usize,
    stride: usize,
}

impl PositionRun {
    fn at_or_after(&self, minimum: usize) -> Option<usize> {
        if minimum <= self.first {
            return Some(self.first);
        }
        if minimum > self.last {
            return None;
        }
        Some(self.first + (minimum - self.first).div_ceil(self.stride) * self.stride)
    }
}

#[derive(Default)]
struct Positions {
    first: Option<PositionRun>,
    rest: Vec<PositionRun>,
}

impl Positions {
    fn push(&mut self, position: usize) {
        let Some(first) = self.first.as_mut() else {
            self.first = Some(PositionRun {
                first: position,
                last: position,
                stride: 0,
            });
            return;
        };
        let last = self.rest.last_mut().unwrap_or(first);
        let stride = position - last.last;
        if last.stride == 0 || last.stride == stride {
            last.stride = stride;
            last.last = position;
        } else {
            self.rest.push(PositionRun {
                first: position,
                last: position,
                stride: 0,
            });
        }
    }

    fn at_or_after(&self, minimum: usize) -> Option<usize> {
        if let Some(position) = self.first.as_ref()?.at_or_after(minimum) {
            return Some(position);
        }
        self.rest
            .get(self.rest.partition_point(|run| run.last < minimum))?
            .at_or_after(minimum)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.first
            .iter()
            .chain(&self.rest)
            .map(|run| {
                if run.stride == 0 {
                    1
                } else {
                    (run.last - run.first) / run.stride + 1
                }
            })
            .sum()
    }
}

fn chunk_anchors(old: ropey::RopeSlice<'_>) -> rustc_hash::FxHashMap<(usize, usize), Positions> {
    let mut chunks = rustc_hash::FxHashMap::<_, Positions>::default();
    let spacing = old.len_bytes().div_ceil(MAX_CHUNK_ANCHORS).max(1);
    let mut offset = 0usize;
    let mut next_anchor = 0;
    for chunk in old.chunks() {
        if offset >= next_anchor {
            chunks
                .entry((chunk.as_ptr() as usize, chunk.len()))
                .or_default();
            next_anchor = offset.saturating_add(spacing);
        }
        offset += chunk.len();
    }
    // Collect every occurrence of the selected identities. Sampling individual
    // occurrences would lose nearby matches for repeated leaves and could
    // rewrite almost the whole file after a small edit. Runs compress regular
    // repetitions; irregular placements cost one run per change in spacing.
    offset = 0;
    for chunk in old.chunks() {
        if let Some(positions) = chunks.get_mut(&(chunk.as_ptr() as usize, chunk.len())) {
            positions.push(offset);
        }
        offset += chunk.len();
    }
    chunks
}

/// Shared leaves form monotonic anchors inside the minimal changed region.
/// Large regions use sampled identities and compact placement runs instead of
/// indexing every leaf of gigabyte documents.
fn edits(old: &Rope, new: &Rope) -> Vec<(Range<usize>, Range<usize>)> {
    let (old_range, new_range) = crate::rope_changes::changed_chars(old, new);
    if old_range.is_empty() && new_range.is_empty() {
        return Vec::new();
    }
    if old_range.is_empty() || new_range.is_empty() {
        return vec![(old_range, new_range)];
    }
    let old = old.slice(old_range.clone());
    let new = new.slice(new_range.clone());
    let chunks = chunk_anchors(old);
    let mut result = Vec::new();
    let (mut old_end, mut new_end, mut new_start) = (0, 0, 0);
    let mut anchored = false;
    let mut gap = |old_bytes: Range<usize>, new_bytes: Range<usize>| {
        if old_bytes.is_empty() && new_bytes.is_empty() {
            return;
        }
        let old_chars = old.byte_to_char(old_bytes.start)..old.byte_to_char(old_bytes.end);
        let new_chars = new.byte_to_char(new_bytes.start)..new.byte_to_char(new_bytes.end);
        let (removed, inserted) = crate::rope_changes::changed_slices(
            old.slice(old_chars.clone()),
            new.slice(new_chars.clone()),
        );
        if !removed.is_empty() || !inserted.is_empty() {
            result.push((
                old_range.start + old_chars.start + removed.start
                    ..old_range.start + old_chars.start + removed.end,
                new_range.start + new_chars.start + inserted.start
                    ..new_range.start + new_chars.start + inserted.end,
            ));
        }
    };
    for chunk in new.chunks() {
        if let Some(start) = chunks
            .get(&(chunk.as_ptr() as usize, chunk.len()))
            .and_then(|positions| positions.at_or_after(old_end))
        {
            gap(old_end..start, new_end..new_start);
            old_end = start + chunk.len();
            new_end = new_start + chunk.len();
            anchored = true;
        }
        new_start += chunk.len();
    }
    if anchored {
        gap(old_end..old.len_bytes(), new_end..new.len_bytes());
    } else {
        result.push((old_range, new_range));
    }
    result
}

struct HashWriter<'a, W> {
    output: &'a mut W,
    digest: Sha256,
}
impl<W: Write> Write for HashWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self.output.write(bytes)?;
        self.digest.update(&bytes[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}
struct HashReader<R> {
    input: R,
    digest: Sha256,
}
impl<R: Read> Read for HashReader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.input.read(bytes)?;
        self.digest.update(&bytes[..count]);
        Ok(count)
    }
}

fn number(writer: &mut impl Write, value: usize) -> io::Result<()> {
    writer.write_all(&(value as u64).to_le_bytes())
}
fn read_number(reader: &mut impl Read) -> io::Result<usize> {
    let mut bytes = [0; 8];
    reader.read_exact(&mut bytes)?;
    usize::try_from(u64::from_le_bytes(bytes)).map_err(|_| invalid())
}
fn read_rope(reader: &mut impl Read, bytes: usize) -> io::Result<Rope> {
    let rope = Rope::from_reader(reader.take(bytes as u64))?;
    if rope.len_bytes() != bytes {
        return Err(invalid());
    }
    Ok(rope)
}

pub(super) fn write_base(writer: &mut impl Write, base: &Rope) -> io::Result<[u8; 32]> {
    let mut writer = HashWriter { output: writer, digest: Sha256::new() };
    base.write_to(&mut writer)?;
    Ok(writer.digest.finalize().into())
}

pub(super) fn read_base(reader: impl Read) -> io::Result<(Rope, [u8; 32])> {
    let mut reader = HashReader { input: reader, digest: Sha256::new() };
    let content = Rope::from_reader(&mut reader)?;
    Ok((content, reader.digest.finalize().into()))
}

pub(super) fn write(
    writer: &mut impl Write,
    undo: &[crate::editor::UndoSnapshot],
    redo: &[crate::editor::UndoSnapshot],
) -> io::Result<[u8; 32]> {
    write_with_base(writer, undo, redo, None)
}

pub(super) fn write_with_base(
    writer: &mut impl Write,
    undo: &[crate::editor::UndoSnapshot],
    redo: &[crate::editor::UndoSnapshot],
    base: Option<&Rope>,
) -> io::Result<[u8; 32]> {
    let mut writer = HashWriter {
        output: writer,
        digest: Sha256::new(),
    };
    writer.write_all(if base.is_some() { SHARED_BASE_MAGIC } else { MAGIC })?;
    // Redo's newest entry is the nearest future. Traverse it backwards so
    // adjacent deltas follow editing order across the undo/redo boundary.
    let snapshots: Vec<_> = base.into_iter().chain(undo.iter()
        .chain(redo.iter().rev())
        .map(|entry| &entry.0))
        .collect();
    for (index, &rope) in snapshots.iter().enumerate().skip(usize::from(base.is_some())) {
        if let Some(previous) = snapshots[..index]
            .iter()
            .position(|other| other.is_instance(rope))
        {
            writer.write_all(&[REFERENCE])?;
            number(&mut writer, previous)?;
            continue;
        }
        let changes = index
            .checked_sub(1)
            .map(|previous| edits(snapshots[previous], rope));
        let delta_size = changes.as_ref().and_then(|changes| {
            changes.iter().try_fold(8usize, |size, (_, new)| {
                size.checked_add(24)?
                    .checked_add(rope.slice(new.clone()).len_bytes())
            })
        });
        if let Some(changes) = changes
            .filter(|_| delta_size.is_some_and(|size| size < rope.len_bytes().saturating_add(8)))
        {
            writer.write_all(&[DELTA])?;
            number(&mut writer, changes.len())?;
            for (old, new) in changes.into_iter().rev() {
                number(&mut writer, old.start)?;
                number(&mut writer, old.end)?;
                let text = rope.slice(new);
                number(&mut writer, text.len_bytes())?;
                for chunk in text.chunks() {
                    writer.write_all(chunk.as_bytes())?;
                }
            }
        } else {
            writer.write_all(&[FULL])?;
            number(&mut writer, rope.len_bytes())?;
            rope.write_to(&mut writer)?;
        }
    }
    Ok(writer.digest.finalize().into())
}

pub(super) fn read(
    reader: impl Read,
    undo: Vec<SavedUndoEntry>,
    redo: Vec<SavedUndoEntry>,
    expected_digest: [u8; 32],
) -> io::Result<(UndoStack, UndoStack)> {
    read_with_base(reader, undo, redo, expected_digest, None)
}

pub(super) fn read_with_base(
    reader: impl Read,
    undo: Vec<SavedUndoEntry>,
    redo: Vec<SavedUndoEntry>,
    expected_digest: [u8; 32],
    base: Option<Rope>,
) -> io::Result<(UndoStack, UndoStack)> {
    let mut reader = HashReader {
        input: reader,
        digest: Sha256::new(),
    };
    let mut magic = [0; MAGIC.len()];
    reader.read_exact(&mut magic)?;
    let shared_base = base.is_some();
    if magic != if shared_base { SHARED_BASE_MAGIC } else { MAGIC } {
        return Err(invalid());
    }
    let undo_len = undo.len();
    let mut snapshots: UndoStack = Vec::with_capacity(undo.len() + redo.len() + usize::from(shared_base));
    if let Some(base) = base {
        // Reference zero is the recovery text, not an undo entry. Deltas clone
        // its tree, so the restored editor and undo history share unchanged text.
        snapshots.push((base, 0, 0, vec![], vec![], None, false));
    }
    for entry in undo.into_iter().chain(redo.into_iter().rev()) {
        let mut tag = [0];
        reader.read_exact(&mut tag)?;
        let rope = match tag[0] {
            FULL => {
                let bytes = read_number(&mut reader)?;
                if entry.content_bytes != Some(bytes) {
                    return Err(invalid());
                }
                read_rope(&mut reader, bytes)?
            }
            DELTA => {
                let mut rope = snapshots.last().ok_or_else(invalid)?.0.clone();
                let count = read_number(&mut reader)?;
                if count > rope.len_chars().saturating_add(1) {
                    return Err(invalid());
                }
                let mut boundary = rope.len_chars();
                for _ in 0..count {
                    let start = read_number(&mut reader)?;
                    let end = read_number(&mut reader)?;
                    let bytes = read_number(&mut reader)?;
                    if start > end || end > boundary {
                        return Err(invalid());
                    }
                    let replacement = read_rope(&mut reader, bytes)?;
                    // Descending edits keep all remaining source offsets valid.
                    // Append Ropes directly, without flattening large inserts.
                    let tail = rope.split_off(end);
                    rope.remove(start..);
                    rope.append(replacement);
                    rope.append(tail);
                    boundary = start;
                }
                rope
            }
            REFERENCE => {
                let previous = read_number(&mut reader)?;
                snapshots.get(previous).ok_or_else(invalid)?.0.clone()
            }
            _ => return Err(invalid()),
        };
        if entry.content_bytes != Some(rope.len_bytes()) {
            return Err(invalid());
        }
        snapshots.push((
            rope,
            entry.cursor_line,
            entry.cursor_col,
            entry.extra_cursors,
            entry.extra_cursor_anchors,
            entry.selection_anchor,
            entry.modified,
        ));
    }
    if reader.read(&mut [0])? != 0 || <[u8; 32]>::from(reader.digest.finalize()) != expected_digest
    {
        return Err(invalid());
    }
    if shared_base { snapshots.remove(0); }
    let mut redo = snapshots.split_off(undo_len);
    redo.reverse();
    Ok((snapshots, redo))
}

#[cfg(test)]
#[path = "session_history_tests.rs"]
mod tests;
