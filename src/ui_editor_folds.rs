//! Visible, clickable summaries for collapsed source regions.
use crate::{app::{ActivePane, App, ContextMenuItem}, app_theme::ThemeColors, editor::Editor,
    editor_transform::PreprocessorDirective};
use ratatui::{Frame, layout::Rect, style::{Modifier, Style}};
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Default)]
pub(crate) struct Buttons {
    stamp: (u64, u64, u64, usize, usize, usize),
    pub(crate) hits: Vec<(Rect, usize, usize)>,
    pub(crate) scope_hits: Vec<(Rect, usize, usize, ScopeAction)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScopeAction { EntireConditional, BranchOnly }

fn stamp(editor: &Editor) -> (u64, u64, u64, usize, usize, usize) {
    (editor.content_version, editor.wrap_revision, editor.folded_ranges.revision(), editor.scroll_offset,
        editor.horizontal_scroll, editor.wrap_scroll_offset)
}

impl Buttons {
    pub(crate) fn clear(&mut self) { self.hits.clear(); self.scope_hits.clear(); }

    pub(crate) fn at(&self, editor: &Editor, col: u16, row: u16) -> Option<usize> {
        if self.stamp != stamp(editor) { return None; }
        self.hits.iter().copied()
            .chain(self.scope_hits.iter().map(|&(area, line, end, _)| (area, line, end)))
            .find_map(|(area, line, end)| {
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
    let action = app.editor().unwrap().fold_buttons.scope_hits.iter()
        .find_map(|&(area, _, _, action)| area.contains((col, row).into()).then_some(action));
    app.active_pane = ActivePane::Editor;
    app.clear_diff_selection();
    let editor = app.editor_mut().unwrap();
    match action {
        Some(ScopeAction::EntireConditional) => { editor.collapse_conditional(line); }
        Some(ScopeAction::BranchOnly) => { editor.collapse_conditional_branch(line); }
        None => editor.toggle_fold(line),
    }
    editor.fold_buttons.clear();
    true
}

fn directive_name(editor: &Editor, line: usize) -> String {
    if line >= editor.line_count() { return "end of file".into(); }
    let name: String = editor.rope.line(line).chars().skip_while(|c| c.is_whitespace() || *c == '#')
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
    format!("#{name}")
}

pub(crate) fn gutter_at(app: &App, col: u16, row: u16) -> bool {
    app.gutter_area.is_some_and(|area| area.width > 0 && col == area.right() - 1
        && row >= area.y && row < area.bottom())
}

pub(crate) fn hover_text(app: &mut App) -> Option<Vec<String>> {
    let (col, row) = (app.mouse_col, app.mouse_row);
    let gutter = app.gutter_area;
    let gutter_hover = gutter_at(app, col, row);
    if app.should_render_diff_for_active_editor() { return None; }
    let editor = app.editor_mut()?;
    if editor.ai_review.active { return None; }
    let (line, action) = if let Some(line) = editor.fold_buttons.at(editor, col, row) {
        let action = editor.fold_buttons.scope_hits.iter().find_map(|&(area, _, _, action)|
            area.contains((col, row).into()).then_some(action));
        (line, action)
    } else if gutter_hover {
        let screen_row = usize::from(row - gutter?.y);
        let line = editor.source_line_at_screen(screen_row).filter(|&(_, first)| first)?.0;
        (line, None)
    } else { return None; };
    if !matches!(editor.conditional_directive(line), Some(PreprocessorDirective::If | PreprocessorDirective::Branch)) {
        return None;
    }
    let scope = editor.conditional_fold(line);
    if let Some(action) = action {
        return Some(match action {
            ScopeAction::EntireConditional => {
                let scope = scope?;
                vec![format!("Fold the entire conditional: all {} branches.", scope.branches.len()),
                    format!("From line {} through #endif on line {}.", line + 1, scope.end + 1)]
            }
            ScopeAction::BranchOnly => vec!["Keep only the first branch folded.".into(), "Reveal the other branches.".into()],
        });
    }
    if let Some(&end) = editor.folded_ranges.get(&line) {
        if let Some(scope) = scope.filter(|scope| scope.branches.len() > 1 && end >= scope.end) {
            return Some(vec![format!("Entire conditional: all {} branches are folded, including nested code.", scope.branches.len()),
                format!("Includes {}. Matching #endif: line {}.", scope.branches.iter().skip(1)
                    .map(|&branch| directive_name(editor, branch)).collect::<Vec<_>>().join(", "), scope.end + 1),
                "Click to expand everything, or choose Branch only.".into()]);
        }
        return Some(vec![format!("This branch only: {} hidden lines ({}–{}).", end.saturating_sub(line + 1), line + 2, end),
            format!("Stops before {} on line {}. Other branch headers stay visible.", directive_name(editor, end), end + 1),
            "Click to expand this branch.".into()]);
    }
    let end = editor.find_fold_end(line)?;
    Some(vec![format!("Fold this branch only ({} lines).", end.saturating_sub(line + 1)),
        format!("{} on line {} stays visible.", directive_name(editor, end), end + 1),
        "For the whole tree, right-click and choose Fold entire conditional.".into()])
}

pub(crate) fn draw_tooltip(frame: &mut Frame, app: &mut App) {
    if app.context_menu_active || app.menu_active || app.dragging_text || app.dragging_minimap
        || crate::ui_modal::active(app).is_some() || crate::ui_quick_picker::active(app).is_some() { return; }
    let Some(lines) = hover_text(app) else { return; };
    crate::ui_widgets::Tooltip::hover(lines.into_iter().map(ratatui::text::Line::raw).collect(), &app.colors)
        .max_width(84).render(frame, app.mouse_col, app.mouse_row);
}

pub(crate) fn add_context_actions(app: &mut App) {
    let Some(editor) = app.editor() else { return; };
    if editor.ai_review.active || app.should_render_diff_for_active_editor() { return; }
    let Some(scope) = editor.conditional_at(editor.cursor_line) else { return; };
    let (branch, end) = scope.branch_at(editor.cursor_line);
    let mut items = Vec::new();
    if end > branch + 1 {
        items.push(ContextMenuItem { label: format!("Fold this branch only (to {})", directive_name(editor, end)),
            shortcut: String::new(), id: "fold_conditional_branch", separator_after: false });
    }
    items.push(ContextMenuItem { label: format!("Fold entire conditional ({} {})", scope.branches.len(),
        if scope.branches.len() == 1 { "branch" } else { "branches" }),
        shortcut: String::new(), id: "fold_conditional", separator_after: true });
    app.context_menu_items.splice(0..0, items);
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
        let directive = editor.conditional_directive(row.line);
        let conditional = matches!(directive, Some(PreprocessorDirective::If | PreprocessorDirective::Branch));
        let scope = editor.conditional_fold(row.line);
        let all_branches = scope.as_ref().is_some_and(|scope| end >= scope.end && scope.branches.len() > 1);
        let name = if all_branches {
            format!("All {} branches", scope.as_ref().unwrap().branches.len())
        } else if scope.as_ref().is_some_and(|scope| scope.branches.len() == 1) {
            "Conditional".into()
        } else { "Branch only".into() };
        let boundary = if all_branches { scope.as_ref().unwrap().end } else { end };
        let full = if conditional {
            format!(" + {name} · {count} {} → {} · Expand ", if count == 1 { "line" } else { "lines" }, directive_name(editor, boundary))
        } else {
            format!(" + {count} {} folded · Expand ", if count == 1 { "line" } else { "lines" })
        };
        let width = usize::from(code.width);
        // Long headers keep a useful source prefix; small panes use a compact
        // label instead of clipping the count or drawing outside their pane.
        let budget = if row.content_width + 1 + full.width() <= width { width }
            else { (width / 2).max(11).min(width - 1) };
        let mut label = full;
        if conditional {
            if label.width() > budget { label = format!(" + {name} · {count} lines "); }
            let compact = if all_branches { "All" } else { "Branch" };
            if label.width() > budget { label = format!(" {compact} +{count} "); }
            if label.width() > budget { label = compact.into(); }
            if label.width() > budget { label = if all_branches { "A" } else { "B" }.into(); }
        } else {
            if label.width() > budget { label = format!(" +{count} folded "); }
            if label.width() > budget { label = format!(" +{count} "); }
            if label.width() > budget { label = "+".into(); }
        }
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
            for (start, end) in cursors.iter().copied().chain(std::iter::once((width, width))) {
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
        if let Some(scope) = scope.filter(|scope| scope.branches.len() > 1) {
            let (action, label) = if all_branches {
                (ScopeAction::BranchOnly, " Branch only ".to_string())
            } else {
                (ScopeAction::EntireConditional, format!(" Fold all {} branches ", scope.branches.len()))
            };
            let left = x + label_width + 1;
            let button_width = label.width();
            if left + button_width <= width
                && !cursors.iter().any(|&(start, end)| start < left + button_width && end > left)
            {
                let button = Rect::new(code.x + left as u16, area.y, button_width as u16, 1);
                let style = colors.ui.button_style(colors.bg_editor, false, button.contains(mouse.into()), true);
                for column in button.x..button.right() { buffer[(column, button.y)].reset(); }
                buffer.set_string(button.x, button.y, &label, style);
                editor.fold_buttons.scope_hits.push((button, row.line, end, action));
                if geometry.enabled() {
                    geometry.overlay(row.screen_row, left, button_width);
                    geometry.protect(row.screen_row, left, button_width);
                }
            }
        }
        if editor.blame.inline_annotation.is_some_and(|(hit, _)| !hit.intersection(area).is_empty()
            || editor.fold_buttons.scope_hits.iter().any(|&(button, _, _, _)| !hit.intersection(button).is_empty())) {
            editor.blame.inline_annotation = None;
        }
        if editor.blame.inline_candidate.is_some_and(|(hit, _)| !hit.intersection(area).is_empty()
            || editor.fold_buttons.scope_hits.iter().any(|&(button, _, _, _)| !hit.intersection(button).is_empty())) {
            editor.blame.inline_candidate = None;
        }
    }
}

#[cfg(test)]
#[path = "ui_editor_folds_tests.rs"]
mod tests;
