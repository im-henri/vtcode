//! Compact views of native diffs. Source text and source coordinates stay intact.
use crate::app::App;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    widgets::{Clear, Paragraph},
};
use std::sync::Arc;

impl App {
    pub(crate) fn context_diff_active(&self) -> bool {
        self.diff_render_cache.context_lines.is_some()
            && self.should_render_diff_for_active_editor()
    }

    pub(crate) fn set_commit_diff_context(&mut self, changes_only: bool, lines: usize) {
        if !self.is_commit_details_tab() {
            return;
        }
        let lines = lines.min(999);
        if self.commit_details.diff_context.changes_only == changes_only
            && self.commit_details.diff_context.lines == lines
        {
            return;
        }
        let old = self.commit_details.diff_context;
        self.commit_details.diff_context = crate::commit_details::DiffContext {
            changes_only,
            lines,
        };
        self.reset_commit_review_context(old);
        self.commit_details.context_revealed_line = None;
        self.rebuild_commit_diff_context();
    }

    fn rebuild_commit_diff_context(&mut self) {
        let Some(full) = self.commit_details.diff_full_cache.clone() else {
            return;
        };
        let crate::commit_details::DiffContext {
            changes_only,
            lines,
        } = self.commit_details.diff_context;
        self.diff_render_cache = if changes_only {
            Arc::new(full.with_context_around(lines, self.commit_details.context_revealed_line))
        } else {
            full
        };
        self.diff_refresh_generation = self.diff_refresh_generation.wrapping_add(1);
        self.diff_inline_scroll = Default::default();
        let cache = self.diff_render_cache.clone();
        if let Some(editor) = self.editor_mut() {
            editor.diff_display_rows = None;
            if changes_only && !cache.visible_target_lines.is_empty() {
                let index = cache
                    .visible_target_lines
                    .partition_point(|line| *line < editor.cursor_line)
                    .min(cache.visible_target_lines.len() - 1);
                let line = cache.visible_target_lines[index];
                if line != editor.cursor_line {
                    editor.clear_selection();
                }
                editor.set_cursor(line, editor.cursor_col.min(editor.line_len(line)));
            }
            editor.scroll_offset = editor.cursor_line.saturating_sub(lines);
        }
    }

    /// Find can reveal a match inside an omitted section without losing the
    /// chosen context setting. Closing Find restores the compact view.
    pub(crate) fn refresh_commit_diff_search_context(&mut self) {
        if self.rendering_inactive_editor || !self.context_diff_active() {
            return;
        }
        let cursor = self.editor().map(|editor| editor.cursor_line);
        let matched =
            self.search_active && self.search_matches.iter().any(|hit| Some(hit.0) == cursor);
        let reveal = if matched {
            cursor.filter(|line| {
                self.commit_details.context_revealed_line.is_some()
                    || self
                        .diff_render_cache
                        .visible_target_lines
                        .binary_search(line)
                        .is_err()
            })
        } else {
            None
        };
        if reveal != self.commit_details.context_revealed_line {
            self.commit_details.context_revealed_line = reveal;
            self.rebuild_commit_diff_context();
        }
    }

    pub(crate) fn context_diff_navigation(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> bool {
        if !self.context_diff_active() || modifiers.contains(KeyModifiers::ALT) {
            return false;
        }
        let Some(editor) = self.editor() else {
            return false;
        };
        let ctrl = modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
        let shift = modifiers.contains(KeyModifiers::SHIFT);
        if matches!(code, KeyCode::PageUp | KeyCode::PageDown) && !ctrl && !shift {
            let mut scroll = std::mem::take(&mut self.diff_inline_scroll);
            let delta = scroll.text_area.height.max(1) as isize
                * if code == KeyCode::PageUp { -1 } else { 1 };
            if let Some(editor) = self.editor_mut() {
                scroll.scroll(editor, false, delta);
            }
            self.diff_inline_scroll = scroll;
            return true;
        }
        let lines = &self.diff_render_cache.visible_target_lines;
        if lines.is_empty() {
            return matches!(
                code,
                KeyCode::Up | KeyCode::Down | KeyCode::Home | KeyCode::End
            );
        }
        let index = lines
            .partition_point(|line| *line < editor.cursor_line)
            .min(lines.len() - 1);
        let row_for_line = |line| self.diff_render_cache.rows
            .get(self.diff_render_cache.aligned_row_for_target_line(line))
            .filter(|row| row.right_line == Some(line));
        let desired = editor.desired_col.unwrap_or_else(|| row_for_line(editor.cursor_line)
            .map_or(0, |row| row.right_cell_column(editor.cursor_col)));
        let (line, col) = match code {
            KeyCode::Up | KeyCode::Down if ctrl => {
                let blank = |line: &usize| {
                    (
                        *line,
                        editor
                            .rope
                            .line(*line)
                            .chunks()
                            .all(|chunk| chunk.trim().is_empty()),
                    )
                };
                let line = if code == KeyCode::Down {
                    crate::diff_selection::paragraph_boundary(
                        lines[index + 1..].iter().map(blank),
                        *lines.last().unwrap(),
                    )
                } else {
                    crate::diff_selection::paragraph_boundary(
                        lines[..index].iter().rev().map(blank),
                        lines[0],
                    )
                };
                (
                    line,
                    if line == editor.cursor_line {
                        editor.cursor_col
                    } else {
                        0
                    },
                )
            }
            KeyCode::Up | KeyCode::Down if !ctrl => {
                let next = index
                    .saturating_add_signed(if code == KeyCode::Up { -1 } else { 1 })
                    .min(lines.len() - 1);
                let line = lines[next];
                (line, row_for_line(line).map_or(0, |row| row.right_column_at_cell(desired)))
            }
            KeyCode::PageUp | KeyCode::PageDown if !ctrl => {
                let amount = editor.viewport_height.max(1) as isize
                    * if code == KeyCode::PageUp { -1 } else { 1 };
                let line = lines[index.saturating_add_signed(amount).min(lines.len() - 1)];
                (line, row_for_line(line).map_or(0, |row| row.right_column_at_cell(desired)))
            }
            KeyCode::Home if ctrl => (lines[0], 0),
            KeyCode::End if ctrl => (*lines.last().unwrap(), usize::MAX),
            KeyCode::Left if editor.cursor_col == 0 => {
                if index == 0 {
                    (lines[0], 0)
                } else {
                    (lines[index - 1], usize::MAX)
                }
            }
            KeyCode::Right if editor.cursor_col >= editor.line_len(editor.cursor_line) => {
                if index + 1 == lines.len() {
                    (lines[index], usize::MAX)
                } else {
                    (lines[index + 1], 0)
                }
            }
            _ => return false,
        };
        let cache = self.diff_render_cache.clone();
        let mut scroll = std::mem::take(&mut self.diff_inline_scroll);
        if let Some(editor) = self.editor_mut() {
            if shift {
                editor
                    .selection_anchor
                    .get_or_insert((editor.cursor_line, editor.cursor_col));
            } else {
                editor.clear_selection();
            }
            editor.set_cursor(line, col.min(editor.line_len(line)));
            editor.desired_col = (!ctrl
                && matches!(
                    code,
                    KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
                ))
            .then_some(desired);
            scroll.layout(cache, editor);
        }
        self.diff_inline_scroll = scroll;
        true
    }

    pub(crate) fn aligned_diff_scroll_row(&self, source_line: usize) -> usize {
        if self.diff_mode == crate::app::DiffMode::SideBySide
            && self.should_render_diff_for_active_editor()
            && self.diff_inline_scroll.aligned
            && Arc::ptr_eq(&self.diff_inline_scroll.cache, &self.diff_render_cache)
            && self.diff_inline_scroll.source_scroll_matches(source_line)
        {
            self.diff_inline_scroll.row
        } else if source_line > 0
            && self.diff_render_cache.context_lines.is_none()
            && source_line == self.diff_render_cache.target_content_lines()
            && self.editor().is_some_and(|editor| {
                source_line + 1 == editor.line_count()
                    && editor.rope.line(source_line).len_chars() == 0
            })
        {
            self.diff_render_cache.rows.len()
        } else {
            self.diff_render_cache
                .aligned_row_for_target_line(source_line)
        }
    }
}

/// Attach actual source rows to the editor's existing mouse selection path.
pub(crate) fn map_visible_rows(app: &mut App, aligned: bool) {
    let start = app.diff_inline_scroll.row;
    let height = app.code_area.map_or(0, |area| area.height as usize);
    let cache = &app.diff_render_cache;
    let rows = (start..start + height)
        .map(|index| {
            if aligned {
                cache.rows.get(index).and_then(|row| row.right_line)
            } else {
                cache.inline_rows.get(index).and_then(|entry| {
                    (entry.kind != '-')
                        .then_some(cache.rows[entry.aligned_row].right_line)
                        .flatten()
                })
            }
        })
        .collect();
    if let Some(editor) = app.editor_mut() {
        editor.diff_display_rows = Some(rows);
        editor.diff_text_inset = if aligned { 2 } else { 0 };
    }
}

pub(crate) fn draw_scrollbar(f: &mut Frame, app: &App) {
    if crate::commit_review::painting_row(app).is_some() { return; }
    let scroll = &app.diff_inline_scroll;
    f.render_widget(Clear, scroll.vertical_area);
    let Some(editor) = app.editor() else {
        return;
    };
    let mapping = if scroll.aligned {
        app.diff_render_cache.aligned_target_rows()
    } else {
        app.diff_render_cache.inline_target_rows()
    };
    crate::ui_editor_minimap::draw_mapped_scrollbar(
        f,
        crate::ui_editor_minimap::ScrollbarParams {
            area: scroll.vertical_area,
            total_lines: scroll.row_count().max(1),
            visible_lines: scroll.text_area.height as usize,
            scroll: scroll.row,
            search_matches: &app.search_matches,
            diagnostic_lines: &app.cached_diag_lines,
            diagnostic_index: Some(&app.cached_diagnostic_index),
            change_rows: &scroll.change_rows,
            cursor_line: scroll.row_for_line(&app.diff_render_cache, editor.cursor_line),
            colors: app.colors,
            hovered: app.hovered_scrollbar,
            active: app.dragging_v_scrollbar,
        },
        mapping,
    );
}

/// Shared native rendering has already painted syntax and character changes.
/// Paint search results using the original file's coordinates. Selections and
/// carets are projected separately from the exact diff text windows.
pub(crate) fn draw_search_matches(f: &mut Frame, app: &App) {
    if !app.search_active {
        return;
    }
    let (Some(editor), Some(area)) = (app.editor(), app.code_area) else {
        return;
    };
    let Some(rows) = &editor.diff_display_rows else {
        return;
    };
    for (offset, source) in rows.iter().enumerate() {
        let Some(line) = source else {
            continue;
        };
        let y = area.y + offset as u16;
        let mut column = editor.horizontal_scroll;
        let first_hit = app.search_matches.partition_point(|hit| hit.0 < *line);
        let end_hit = app.search_matches.partition_point(|hit| hit.0 <= *line);
        let hits = &app.search_matches[first_hit..end_hit];
        for x in area.x.saturating_add(editor.diff_text_inset as u16)..area.right() {
            let cell = &mut f.buffer_mut()[(x, y)];
            let index = hits.partition_point(|hit| hit.2 <= column);
            if app.search_active
                && !app.rendering_inactive_editor
                && hits
                    .get(index)
                    .is_some_and(|hit| hit.1 <= column && column < hit.2)
            {
                cell.set_style(app.colors.ui.find_style(
                    cell.style(),
                    first_hit + index == app.search_current,
                    app.colors.bg_search_match,
                ));
            }
            column += cell.symbol().chars().count().max(1);
        }
    }
}

pub(crate) fn draw_compact_side_by_side(f: &mut Frame, app: &mut App, area: Rect) {
    let layout = crate::ui_diff_display::side_by_side_diff_layout(area, app.diff_split_ratio);
    let word_wrap = app.editor().is_some_and(|editor| editor.word_wrap);
    let show_minimap = app.show_minimap;
    let cursor_visible = app.cursor_visible;
    if app.diff_selection_owns_cursor() {
        app.cursor_visible = false;
    }
    app.show_minimap = false;
    if let Some(editor) = app.editor_mut() {
        editor.word_wrap = false;
        editor.diff_display_rows = None;
    }
    crate::ui_editor::draw_editor(f, app, layout.right);
    app.cursor_visible = cursor_visible;
    app.show_minimap = show_minimap;
    if let Some(editor) = app.editor_mut() {
        editor.word_wrap = word_wrap;
    }
    crate::ui_diff_display::layout_aligned_diff_scroll(app, area);
    crate::ui_diff_display::draw_diff_original(f, app, layout.left);
    crate::ui_diff_display::draw_diff_target_overlay(f, app);
    // A full-file minimap would suggest hidden rows were still scrollable.
    if let Some(minimap) = app.minimap_area.take() {
        f.render_widget(
            Paragraph::new("").style(Style::default().bg(app.colors.bg_editor)),
            minimap,
        );
    }
    map_visible_rows(app, true);
    draw_scrollbar(f, app);
    draw_search_matches(f, app);
}
