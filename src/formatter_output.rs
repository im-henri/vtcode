//! Prepare formatter results and apply prepared edits using native offsets.
use crate::editor::Editor;
use crate::rope_changes::{EditOffset as Change, map_offset};
use ropey::Rope;
use similar::DiffableStr;
use std::{ops::Range, time::Duration};

#[cfg(test)]
#[path = "formatter_output_tests.rs"]
mod tests;

#[derive(Debug)]
pub(crate) struct Prepared {
    rope: Rope,
    changes: Vec<Change>,
    dirty: Option<(usize, usize)>,
    structure_changed: bool,
}

impl Prepared {
    /// Search already knows the exact edits. Reuse their offsets instead of
    /// diffing or rebuilding the document again when the worker completes.
    pub(crate) fn from_changes(source: &Rope, rope: Rope, changes: Vec<Change>, structure_changed: bool) -> Self {
        let dirty = changes.first().zip(changes.last()).map(|(first, last)| {
            (source.char_to_line(first.start), source.char_to_line(last.end).max(rope.char_to_line(last.new_end)))
        });
        Self { rope, changes, dirty, structure_changed }
    }

    pub(crate) fn rope(&self) -> &Rope { &self.rope }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize { self.changes.len() }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool { self.changes.is_empty() }

    fn map_cursor(&self, cursor: usize) -> (usize, usize) {
        let cursor = map_offset(&self.changes, cursor).min(self.rope.len_chars());
        let line = self.rope.char_to_line(cursor);
        (line, (cursor - self.rope.line_to_char(line)).min(Editor::line_content_len(self.rope.line(line))))
    }

    /// The caller validates document identity/version before accepting this
    /// snapshot. All diffing, text allocation and rope edits already happened
    /// on the worker; the UI records undo and swaps the prepared tree once.
    pub(crate) fn apply(&self, editor: &mut Editor) -> Result<usize, String> {
        crate::editor_document::edit!(editor, {
        if editor.read_only || editor.save_content_error().is_some() {
            return Err("The target editor is not writable".into());
        }
        let Some((dirty_from, dirty_through)) = self.dirty else { return Ok(0); };
        let offset = |line: usize, col: usize| {
            let line = line.min(editor.rope.len_lines() - 1);
            editor.rope.line_to_char(line) + col.min(editor.line_len(line))
        };
        let cursor = self.map_cursor(offset(editor.cursor_line, editor.cursor_col));
        let extra_cursors = editor.extra_cursors.iter()
            .map(|&(line, col)| self.map_cursor(offset(line, col))).collect();
        // Formatting and subsequent typing must each be independently undoable.
        editor.break_undo_group();
        editor.save_undo_for_range(dirty_from, dirty_through);
        editor.clear_selection();
        editor.snippet_cancel();
        editor.rope = self.rope.clone();
        editor.extra_cursors = extra_cursors;
        editor.extra_cursor_anchors.clear();
        editor.desired_col = None;
        editor.modified = true;
        editor.mark_syntax_dirty_range(dirty_from, dirty_through);
        if self.structure_changed { editor.clear_stale_folds(); }
        (editor.cursor_line, editor.cursor_col) = cursor;
        editor.ensure_visible();
        editor.refresh_modified_from_saved();
        Ok(self.changes.len())
            })
    }
}

pub(crate) fn prepare(source: &Rope, output: &str) -> Prepared {
    let edits = changed_ranges(source, output);
    let mut rope = source.clone();
    let mut changes = Vec::with_capacity(edits.len());
    let (mut old_end, mut new_end) = (0, 0);
    for edit in &edits {
        let start = source.byte_to_char(edit.old.start);
        let end = source.byte_to_char(edit.old.end);
        new_end += start - old_end + output[edit.new.clone()].chars().count();
        old_end = end;
        changes.push(Change { start, end, new_end });
    }
    // Retain shared chunks for unchanged regions and undo snapshots instead
    // of rebuilding the entire document or cloning replacement strings.
    for (edit, change) in edits.iter().zip(&changes).rev() {
        rope.remove(change.start..change.end);
        rope.insert(change.start, &output[edit.new.clone()]);
    }
    let dirty = changes.first().zip(changes.last()).map(|(first, last)| {
        (source.char_to_line(first.start), source.char_to_line(last.end).max(rope.char_to_line(last.new_end)))
    });
    let structure_changed = edits.iter().zip(&changes).any(|(edit, change)|
        source.slice(change.start..change.end).len_lines() > 1 || output[edit.new.clone()].contains(['\r', '\n']));
    Prepared { rope, changes, dirty, structure_changed }
}

struct Edit {
    old: Range<usize>,
    new: Range<usize>,
}

fn changed_ranges(source: &Rope, output: &str) -> Vec<Edit> {
    if source == output { return Vec::new(); }
    let input = source.to_string();
    let old_lines = input.tokenize_lines();
    let new_lines = output.tokenize_lines();
    let prefix = old_lines.iter().zip(&new_lines).take_while(|(a, b)| a == b).count();
    let suffix = old_lines[prefix..].iter().rev().zip(new_lines[prefix..].iter().rev())
        .take_while(|(a, b)| a == b).count();
    let base = old_lines[..prefix].iter().map(|line| line.len()).sum::<usize>();
    let old_lines = &old_lines[prefix..old_lines.len() - suffix];
    let new_lines = &new_lines[prefix..new_lines.len() - suffix];
    let offsets = |lines: &[&str]| {
        let mut offsets = Vec::with_capacity(lines.len() + 1);
        offsets.push(base);
        for line in lines { offsets.push(offsets.last().unwrap() + line.len()); }
        offsets
    };
    let old_offsets = offsets(old_lines);
    let new_offsets = offsets(new_lines);
    // No alignment exists when every line in the changed region differs.
    // Prove that in linear time rather than exhausting Myers' time budget.
    let disjoint = old_lines.len().saturating_mul(new_lines.len()) > 4096 && {
        let (small, large) = if old_lines.len() < new_lines.len() {
            (old_lines, new_lines)
        } else { (new_lines, old_lines) };
        let lines: rustc_hash::FxHashSet<_> = small.iter().copied().collect();
        !large.iter().any(|line| lines.contains(line))
    };
    let ops = if old_lines.is_empty() || new_lines.is_empty() || disjoint {
        vec![similar::DiffOp::Replace {
            old_index: 0, old_len: old_lines.len(), new_index: 0, new_len: new_lines.len(),
        }]
    } else {
        similar::TextDiff::configure().timeout(Duration::from_millis(200))
            .diff_slices(old_lines, new_lines).ops().to_vec()
    };
    ops.iter().filter(|op| op.tag() != similar::DiffTag::Equal).map(|op| {
        let old_range = old_offsets[op.old_range().start]..old_offsets[op.old_range().end];
        let new_range = new_offsets[op.new_range().start]..new_offsets[op.new_range().end];
        let old = &input[old_range.clone()];
        let new = &output[new_range.clone()];
        let mut prefix = old.chars().zip(new.chars()).take_while(|(a, b)| a == b)
            .map(|(c, _)| c.len_utf8()).sum::<usize>();
        // Treat CRLF as one separator even though native offsets could split it.
        let splits_crlf = |text: &str, byte: usize| {
            byte > 0 && text.as_bytes().get(byte - 1) == Some(&b'\r')
                && text.as_bytes().get(byte) == Some(&b'\n')
        };
        if splits_crlf(old, prefix) || splits_crlf(new, prefix) { prefix -= 1; }
        let mut suffix = old[prefix..].chars().rev().zip(new[prefix..].chars().rev())
            .take_while(|(a, b)| a == b).map(|(c, _)| c.len_utf8()).sum::<usize>();
        if splits_crlf(old, old.len() - suffix) || splits_crlf(new, new.len() - suffix) { suffix -= 1; }
        Edit {
            old: old_range.start + prefix..old_range.end - suffix,
            new: new_range.start + prefix..new_range.end - suffix,
        }
    }).collect()
}
