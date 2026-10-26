//! One undoable edit transaction, with ranges evaluated against the original snapshot.
use crate::{editor::Editor, lsp_positions};
use lsp_types::TextEdit;

fn offset_map(edits: impl IntoIterator<Item = (usize, usize, usize)>) -> Vec<crate::rope_changes::EditOffset> {
    let (mut old_end, mut new_end) = (0, 0);
    edits.into_iter().map(|(start, end, inserted)| {
        new_end += start - old_end + inserted;
        old_end = end;
        crate::rope_changes::EditOffset { start, end, new_end }
    }).collect()
}

pub(crate) fn prepare(
    rope: &ropey::Rope,
    edits: &[TextEdit],
) -> Result<Vec<(usize, usize, String)>, String> {
    let mut result = edits
        .iter()
        .map(|edit| {
            lsp_positions::range(rope, edit.range)
                .map(|(start, end)| (start, end, edit.new_text.clone()))
                .ok_or_else(|| "Language server returned an invalid edit range".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    result.sort_by_key(|(start, end, _)| (*start, *end));
    if result.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err("Language server returned overlapping edits".into());
    }
    Ok(result)
}

pub(crate) fn apply(editor: &mut Editor, edits: &[TextEdit]) -> Result<usize, String> {
    crate::editor_document::edit!(editor, {
        let edits = prepare(&editor.rope, edits)?;
        apply_prepared(editor, edits)
    })
}

/// Apply sorted, non-overlapping edits in the current Rope's character coordinates.
/// Local renames use the same undo and cursor mapping as language-server edits.
pub(crate) fn apply_prepared(editor: &mut Editor, mut edits: Vec<(usize, usize, String)>) -> Result<usize, String> {
        crate::editor_document::edit!(editor, {
    if editor.read_only || editor.save_content_error().is_some() {
        return Err("The target editor is not writable".into());
    }
    // Validate the complete response first, then ignore edits that do nothing.
    // Format Document must not clear redo or dirty an already formatted file.
    edits.retain(|(start, end, text)| editor.rope.slice(*start..*end) != text.as_str());
    if edits.is_empty() {
        return Ok(0);
    }
    let dirty_from = editor.rope.char_to_line(edits[0].0);
    let source = editor.rope.clone();
    let dirty_through = source.char_to_line(edits.last().unwrap().1);
    let structure_changed = edits.iter().any(|(start, end, text)|
        source.slice(*start..*end).len_lines() > 1 || text.contains(['\r', '\n']));
    let changes = offset_map(edits.iter().map(|(start, end, text)| (*start, *end, text.chars().count())));
    let new_end = changes.last().unwrap().new_end;
    let mut cursors = editor.collect_multi_cursors();
    editor.save_undo_for_range(dirty_from, dirty_through);
    editor.clear_selection();
    editor.snippet_cancel();
    for (start, end, text) in edits.iter().rev() {
        editor.rope.remove(*start..*end);
        editor.rope.insert(*start, text);
    }
    editor.modified = true;
    editor.mark_syntax_dirty_range(dirty_from, dirty_through.max(editor.rope.char_to_line(new_end)));
    if structure_changed { editor.clear_stale_folds(); }
    for cursor in &mut cursors {
        let line = cursor.line.min(source.len_lines() - 1);
        let offset = source.line_to_char(line) + cursor.col.min(Editor::line_content_len(source.line(line)));
        let mapped = crate::rope_changes::map_offset(&changes, offset).min(editor.rope.len_chars());
        cursor.line = editor.rope.char_to_line(mapped);
        cursor.col = mapped - editor.rope.line_to_char(cursor.line);
        cursor.anchor = None;
    }
    editor.desired_col = None;
    editor.set_multi_cursor_states(cursors);
    editor.refresh_modified_from_saved();
    Ok(edits.len())
        })
    }

pub(crate) fn complete(
    editor: &mut Editor,
    item: &crate::lsp::LspCompletionItem,
) -> Result<(), String> {
    if editor.read_only || editor.bg_loading {
        return Err("The target editor is not writable".into());
    }
    let (mut start, mut end) = if let Some(((sl, sc), (el, ec))) = item.text_edit_range {
        lsp_positions::range(
            &editor.rope,
            lsp_types::Range::new(
                lsp_types::Position::new(sl, sc),
                lsp_types::Position::new(el, ec),
            ),
        )
        .ok_or("Language server returned an invalid completion range")?
    } else {
        let line = editor.rope.line(editor.cursor_line);
        let col = editor.cursor_col.min(Editor::line_content_len(line));
        let prefix = line
            .chars_at(col)
            .reversed()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .count();
        let end = editor.rope.line_to_char(editor.cursor_line) + col;
        (end - prefix, end)
    };
    let additional = prepare(&editor.rope, &item.additional_text_edits)?;
    if additional.iter().any(|(a, b, _)| *a < end && *b > start) {
        return Err("Language server returned an overlapping completion edit".into());
    }
    let source = editor.rope.clone();
    let snippet = item.is_snippet.then(|| crate::editor_snippets::parse_snippet_body(&item.insert_text));
    let inserted = snippet.as_ref().map_or(item.insert_text.as_str(), |(text, _)| text).chars().count();
    let structure_changed = source.slice(start..end).len_lines() > 1
        || item.insert_text.contains(['\r', '\n'])
        || additional.iter().any(|(a, b, text)| source.slice(*a..*b).len_lines() > 1 || text.contains(['\r', '\n']));
    let mut cursors = (!editor.extra_cursors.is_empty()).then(|| editor.collect_multi_cursors());
    let changes = if cursors.is_some() {
        let mut ranges: Vec<_> = additional.iter().map(|(a, b, text)| (*a, *b, text.chars().count())).collect();
        ranges.push((start, end, inserted));
        ranges.sort_by_key(|(a, b, _)| (*a, *b));
        offset_map(ranges)
    } else { Vec::new() };
    let dirty_from = editor
        .rope
        .char_to_line(additional.first().map_or(start, |edit| edit.0.min(start)));
    let dirty_through = editor
        .rope
        .char_to_line(additional.last().map_or(end, |edit| edit.1.max(end)))
        + item.insert_text.matches('\n').count()
        + additional
            .iter()
            .map(|(_, _, text)| text.matches('\n').count())
            .sum::<usize>();
    editor.save_undo_public();
    editor.clear_selection();
    editor.snippet_cancel();
    // Apply imports first, then insert the completion at its relocated offset.
    // Snippet placeholders are consequently created at their final positions.
    for (a, b, text) in additional.iter().rev() {
        editor.rope.remove(*a..*b);
        editor.rope.insert(*a, text);
        if *b <= start {
            let inserted = text.chars().count();
            start = start - (b - a) + inserted;
            end = end - (b - a) + inserted;
        }
    }
    editor.rope.remove(start..end);
    if let Some((text, stops)) = snippet {
        editor.insert_parsed_snippet_text(start, text, stops);
    } else {
        editor.rope.insert(start, &item.insert_text);
        let cursor = start + inserted;
        editor.cursor_line = editor.rope.char_to_line(cursor);
        editor.cursor_col = cursor - editor.rope.line_to_char(editor.cursor_line);
        editor.modified = true;
        editor.ensure_visible();
    }
    editor.mark_syntax_dirty_range(dirty_from, dirty_through);
    if structure_changed { editor.clear_stale_folds(); }
    editor.desired_col = None;
    if let Some(cursors) = cursors.as_mut() {
        let map = |(line, col): (usize, usize)| {
            let line = line.min(source.len_lines() - 1);
            let offset = source.line_to_char(line) + col.min(Editor::line_content_len(source.line(line)));
            let offset = crate::rope_changes::map_offset(&changes, offset).min(editor.rope.len_chars());
            let line = editor.rope.char_to_line(offset);
            (line, offset - editor.rope.line_to_char(line))
        };
        for cursor in cursors.iter_mut().skip(1) {
            (cursor.line, cursor.col) = map((cursor.line, cursor.col));
            cursor.anchor = cursor.anchor.map(map);
        }
        cursors[0] = crate::editor_multicursor::MultiCursor {
            line: editor.cursor_line, col: editor.cursor_col, anchor: editor.selection_anchor,
        };
    }
    if let Some(cursors) = cursors { editor.set_multi_cursor_states(cursors); }
    else { editor.ensure_visible(); }
    Ok(())
}

#[cfg(test)]
#[path = "app_lsp_edits_tests.rs"]
mod tests;
