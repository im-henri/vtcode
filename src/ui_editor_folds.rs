//! Visible, clickable summaries for collapsed source regions.
use crate::{app::{ActivePane, App}, app_theme::ThemeColors, editor::Editor};
use ratatui::{Frame, layout::Rect, style::{Modifier, Style}};
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Default)]
pub(crate) struct Buttons {
    stamp: (u64, u64, usize, usize, usize),
    pub(crate) hits: Vec<(Rect, usize, usize)>,
}

fn stamp(editor: &Editor) -> (u64, u64, usize, usize, usize) {
    (editor.content_version, editor.wrap_revision, editor.scroll_offset,
        editor.horizontal_scroll, editor.wrap_scroll_offset)
}

impl Buttons {
    pub(crate) fn clear(&mut self) { self.hits.clear(); }

    pub(crate) fn at(&self, editor: &Editor, col: u16, row: u16) -> Option<usize> {
        if self.stamp != stamp(editor) { return None; }
        self.hits.iter().find_map(|&(area, line, end)| {
            (area.contains((col, row).into()) && editor.folded_ranges.get(&line) == Some(&end))
                .then_some(line)
        })
    }
}

pub(crate) fn at(app: &App, col: u16, row: u16) -> Option<usize> {
    let editor = app.editor()?;
    let line = editor.fold_buttons.at(editor, col, row)?;
    if editor.ai_review.active || app.should_render_diff_for_active_editor() { return None; }
    Some(line)
}

pub(crate) fn click(app: &mut App, col: u16, row: u16) -> bool {
    let Some(line) = at(app, col, row) else { return false; };
    app.active_pane = ActivePane::Editor;
    app.clear_diff_selection();
    let editor = app.editor_mut().unwrap();
    editor.toggle_fold(line);
    editor.fold_buttons.clear();
    true
}

pub(crate) fn gutter_style(colors: &ThemeColors, bg: ratatui::style::Color, folded: bool, hovered: bool) -> Style {
    if folded {
        colors.ui.button_style(bg, true, hovered, true).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(if hovered { colors.ui.fg_line_number_active } else { colors.ui.fg_folding }).bg(bg)
    }
}

/// The source renderer supplies its already laid-out rows, so folded summaries
/// do not build another wrap map or scan hidden text.
pub(crate) struct Row {
    pub line: usize,
    pub screen_row: usize,
    pub content_width: usize,
    pub start_col: usize,
    pub end_col: usize,
}

pub(crate) fn draw(
    frame: &mut Frame, app: &mut App, code: Rect, rows: &[Row], skip_rows: usize,
    geometry: &mut crate::ui_editor_animations::Geometry,
) {
    let colors = app.colors;
    let mouse = (app.mouse_col, app.mouse_row);
    let Some(editor) = app.editor_mut() else { return; };
    editor.fold_buttons.clear();
    editor.fold_buttons.stamp = stamp(editor);
    if code.width < 2 { return; }
    for row in rows {
        if row.screen_row < skip_rows || row.screen_row >= usize::from(code.height) { continue; }
        let Some(&end) = editor.folded_ranges.get(&row.line) else { continue; };
        let count = end.saturating_sub(row.line + 1);
        if count == 0 { continue; }
        let full = format!(" + {count} {} folded · Expand ", if count == 1 { "line" } else { "lines" });
        let width = usize::from(code.width);
        // Long headers keep a useful source prefix; small panes use a compact
        // label instead of clipping the count or drawing outside their pane.
        let budget = if row.content_width + 1 + full.width() <= width { width }
            else { (width / 2).max(11).min(width - 1) };
        let mut label = full;
        if label.width() > budget { label = format!(" +{count} folded "); }
        if label.width() > budget { label = format!(" +{count} "); }
        if label.width() > budget { label = "+".into(); }
        let label_width = label.width();
        let mut x = (row.content_width + 1).min(width - label_width);
        // Do not conceal primary or secondary carets on a long folded header.
        let mut cursors: Vec<_> = std::iter::once((editor.cursor_line, editor.cursor_col))
            .chain(editor.extra_cursors.iter().copied())
            .filter(|&(line, col)| line == row.line && col >= row.start_col && col <= row.end_col)
            .map(|(_, col)| {
                let start = editor.cell_at_source_column(row.line, row.start_col, col);
                let end = editor.cell_at_source_column(row.line, row.start_col, col + 1).max(start + 1);
                (start, end)
            }).filter(|&(start, _)| start < width).collect();
        if cursors.iter().any(|&(start, end)| start < x + label_width && end > x) {
            cursors.sort_unstable();
            let mut gap_start = 0;
            let mut best = None;
            for (start, end) in cursors.into_iter().chain(std::iter::once((width, width))) {
                if start.saturating_sub(gap_start) >= label_width {
                    let candidate = x.clamp(gap_start, start - label_width);
                    if best.is_none_or(|old: usize| candidate.abs_diff(x) < old.abs_diff(x)) { best = Some(candidate); }
                }
                gap_start = gap_start.max(end);
            }
            let Some(available) = best else { continue; };
            x = available;
        }
        let area = Rect::new(code.x + x as u16, code.y + row.screen_row as u16, label_width as u16, 1);
        let hovered = area.contains(mouse.into());
        let style = colors.ui.button_style(colors.bg_editor, true, hovered, true)
            .add_modifier(Modifier::BOLD).remove_modifier(Modifier::DIM | Modifier::ITALIC);
        let buffer = frame.buffer_mut();
        // Do not leave the first half of a wide source glyph beneath the badge.
        if x > 0 && buffer[(area.x - 1, area.y)].symbol().width() > 1 {
            buffer[(area.x - 1, area.y)].set_char(' ').set_style(Style::default().bg(colors.bg_editor));
        }
        for column in area.x..area.right() { buffer[(column, area.y)].reset(); }
        buffer.set_string(area.x, area.y, &label, style);
        if geometry.enabled() {
            geometry.overlay(row.screen_row, x, label_width);
            geometry.protect(row.screen_row, x, label_width);
        }
        editor.fold_buttons.hits.push((area, row.line, end));
        if editor.blame.inline_annotation.is_some_and(|(hit, _)| !hit.intersection(area).is_empty()) {
            editor.blame.inline_annotation = None;
        }
        if editor.blame.inline_candidate.is_some_and(|(hit, _)| !hit.intersection(area).is_empty()) {
            editor.blame.inline_candidate = None;
        }
    }
}

#[cfg(test)]
#[path = "ui_editor_folds_tests.rs"]
mod tests;
