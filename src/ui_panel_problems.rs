//! One virtual row mapping for rendering, scrolling, hovering and navigation.
//! Only visible rows are formatted; the language-server index is revision cached.
use super::*;
use crate::app::ActivePane;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Row {
    Summary,
    Marker(usize),
    DiagnosticHeading,
    Diagnostic(usize),
    Empty,
}

pub(super) struct Layout {
    markers: usize,
    diagnostics: usize,
    pub total: usize,
}
impl Layout {
    fn row(&self, index: usize) -> Option<Row> {
        if index >= self.total {
            return None;
        }
        if index == 0 {
            return Some(Row::Summary);
        }
        let index = index - 1;
        if index < self.markers {
            return Some(Row::Marker(index));
        }
        if self.diagnostics == 0 {
            return Some(Row::Empty);
        }
        if index == self.markers {
            return Some(Row::DiagnosticHeading);
        }
        Some(Row::Diagnostic(index - self.markers - 1))
    }
}
pub(super) fn layout(app: &mut App) -> Layout {
    let markers = app.cached_problems.len();
    let diagnostics = app.lsp_manager.problems_diagnostics().len();
    Layout {
        markers,
        diagnostics,
        total: 1
            + markers
            + diagnostics
            + usize::from(diagnostics > 0)
            + usize::from(markers == 0 && diagnostics == 0),
    }
}

pub(super) fn hit(app: &mut App, col: u16, row: u16) -> Option<Row> {
    if !app.show_panel || app.panel_tab != 1 {
        return None;
    }
    let panel = app.panel_area?;
    let content = Rect::new(
        panel.x,
        panel.y + panel.height.min(1),
        panel.width,
        panel.height.saturating_sub(1),
    );
    if !content.contains((col, row).into()) {
        return None;
    }
    let layout = layout(app);
    if col + 1 == content.right() && content.width > 1 && layout.total > content.height as usize {
        return None;
    }
    let offset = app
        .problem_scroll
        .min(layout.total.saturating_sub(content.height as usize));
    layout.row(offset + usize::from(row - content.y))
}

pub(super) fn activate(app: &mut App, col: u16, row: u16) {
    let target = match hit(app, col, row) {
        Some(Row::Marker(index)) => {
            let (_, line, _, _, id) = &app.cached_problems[index];
            let (id, line) = (*id, line.saturating_sub(1));
            let target = app
                .tabs
                .iter()
                .position(|tab| tab.editor.navigation_id == id)
                .map(|index| (None, index))
                .or_else(|| {
                    app.groups.iter().enumerate().find_map(|(index, group)| {
                        (group.desk == app.desks.active)
                            .then(|| {
                                group
                                    .tabs
                                    .iter()
                                    .position(|tab| tab.editor.navigation_id == id)
                            })
                            .flatten()
                            .map(|tab| (Some(index), tab))
                    })
                });
            let Some((group, tab)) = target else {
                return;
            };
            let switched = group.is_some() || tab != app.active_tab;
            app.push_nav_history();
            app.cancel_remote_location();
            app.sync_duplicate_file_tabs();
            if switched {
                app.auto_save_on_focus_change();
            }
            app.clear_transient_editor_state();
            app.suspend_diff_tab();
            if let Some(group) = group {
                app.switch_group(group);
            }
            app.active_tab = tab;
            app.touch_tab_mru(tab);
            app.ensure_active_tab_visible();
            app.sync_diff_tab();
            if switched {
                app.refresh_git_line_status_async();
            }
            Some((line, 0, false))
        }
        Some(Row::Diagnostic(index)) => {
            let target = app
                .lsp_manager
                .problems_diagnostics()
                .nth(index)
                .map(|(path, diagnostic)| (path.to_path_buf(), diagnostic.line, diagnostic.col));
            let Some((path, line, column)) = target else {
                return;
            };
            app.push_nav_history();
            app.open_file(&path);
            if app
                .editor()
                .is_none_or(|editor| editor.file_path.as_ref() != Some(&path))
            {
                return;
            }
            Some((line, column, true))
        }
        _ => None,
    };
    let Some((line, column, utf16)) = target else {
        return;
    };
    app.active_pane = ActivePane::Editor;
    app.terminal_panel_focused = false;
    if let Some(editor) = app.editor_mut() {
        editor.clear_selection();
        if editor.bg_loading {
            editor.pending_cursor_position = Some((line, column));
            editor.pending_cursor_utf16 = utf16;
        } else {
            let column = if utf16 {
                crate::lsp_positions::from_utf16(&editor.rope, line, column as u32)
            } else {
                column
            };
            editor.set_cursor(line, column);
            editor.center_cursor();
        }
    }
}

pub(super) fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    if area.is_empty() {
        return;
    }
    app.request_problems_analysis();
    let layout = layout(app);
    app.problem_scroll = app
        .problem_scroll
        .min(layout.total.saturating_sub(area.height as usize));
    let (errors, warnings, info) = app.lsp_manager.problem_counts();
    let summary = format!(
        " Problems: {} errors, {} warnings, {} info",
        errors + app.cached_problems_error_count,
        warnings + app.cached_problems_warning_count,
        info + app.cached_problems_info_count
    );
    let bg = app.colors.ui.bg_panel;
    let dim = app.colors.ui.description_color(bg);
    let text_width = area.width.saturating_sub(u16::from(
        area.width > 1 && layout.total > area.height as usize,
    ));
    let first_diag = app.problem_scroll.saturating_sub(layout.markers + 2);
    let mut diagnostics = app.lsp_manager.problems_diagnostics().skip(first_diag);
    let mut lines = Vec::with_capacity(area.height as usize);
    for index in app.problem_scroll
        ..layout
            .total
            .min(app.problem_scroll.saturating_add(area.height as usize))
    {
        let row_y = area.y + lines.len() as u16;
        let hovered =
            app.mouse_row == row_y && (area.x..area.x + text_width).contains(&app.mouse_col);
        let row_bg = if hovered { app.colors.bg_hover } else { bg };
        let fg = app.colors.ui.workbench_text_color(row_bg);
        let row = match layout.row(index).unwrap() {
            Row::Summary => Line::styled(
                summary.clone(),
                Style::default().fg(dim).bg(bg).add_modifier(Modifier::BOLD),
            ),
            Row::DiagnosticHeading => Line::styled(
                format!(" ── LSP Diagnostics ({}) ──", layout.diagnostics),
                Style::default().fg(dim).bg(bg).add_modifier(Modifier::BOLD),
            ),
            Row::Empty => Line::styled(
                if app.bg_problems_analysis_running {
                    " Scanning problems..."
                } else {
                    " ✓ No problems found"
                },
                Style::default().fg(dim).bg(bg),
            ),
            Row::Marker(marker) => {
                let (file, line, kind, text, _) = &app.cached_problems[marker];
                let color = match kind.as_str() {
                    "FIXME" | "BUG" => app.colors.fg_fixme,
                    "TODO" => app.colors.fg_todo,
                    "HACK" => app.colors.fg_hack,
                    "NOTE" => app.colors.fg_info_label,
                    _ => app.colors.fg_dim,
                };
                let icon = match kind.as_str() {
                    "FIXME" | "BUG" => "✕",
                    "TODO" | "HACK" => "⚠",
                    _ => "ℹ",
                };
                let prefix = format!(" {icon} {file}:{line} [{kind}] ");
                let message = crate::ui_pickers::truncate_to_width(
                    text,
                    text_width as usize - prefix.width().min(text_width as usize),
                );
                problem_row(
                    vec![
                        Span::styled(
                            prefix,
                            Style::default()
                                .fg(app.colors.ui.readable_foreground(color, row_bg, 4.5))
                                .bg(row_bg),
                        ),
                        Span::styled(
                            message,
                            Style::default()
                                .fg(app.colors.ui.workbench_text_color(row_bg))
                                .bg(row_bg),
                        ),
                    ],
                    row_bg,
                    text_width,
                )
            }
            Row::Diagnostic(_) => {
                let Some((path, diagnostic)) = diagnostics.next() else {
                    break;
                };
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                let icon = format!(" {} ", crate::lsp::severity_icon(diagnostic.severity));
                let location = format!("{name}:{} ", diagnostic.line + 1);
                let message = diagnostic.message.lines().next().unwrap_or_default();
                let message = crate::ui_pickers::truncate_to_width(
                    message,
                    text_width as usize - (icon.width() + location.width()).min(text_width as usize),
                );
                let color = app.colors.ui.readable_foreground(
                    app.colors.ui.problem_icon_color(diagnostic.severity, row_bg),
                    row_bg,
                    4.5,
                );
                problem_row(
                    vec![
                        Span::styled(icon, Style::default().fg(color).bg(row_bg)),
                        Span::styled(location, Style::default().fg(fg).bg(row_bg)),
                        Span::styled(message, Style::default().fg(fg).bg(row_bg)),
                    ],
                    row_bg,
                    text_width,
                )
            }
        };
        lines.push(row);
    }
    drop(diagnostics);
    f.render_widget(Paragraph::new(lines).style(Style::default().bg(bg)), area);
    super::draw_list_scrollbar(f, app, area, layout.total, app.problem_scroll, bg);
}
