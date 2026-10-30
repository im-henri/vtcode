use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};
use std::fmt::Write as FmtWrite;
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::source_control_rows::Group;
use crate::helpers::len_as_u16;
use crate::text_input::{TextInputHitMap, TextInputId};
use crate::ui_pickers::truncate_to_width;

const SC_ACTIONS_LABEL: &str = " Actions ▾ ";
const SC_SECTION_ACTION_WIDTH: u16 = 3;
#[path = "ui_source_control_file_actions.rs"]
mod file_actions;
pub(crate) use file_actions::{FileActionRow, handle_click as handle_file_action_click};
const SC_CHANGES_FIXED_ROWS: u16 = 6;
const SC_INITIALIZE_BUTTON_LABEL: &str = "Initialize Repository";

#[cfg(test)]
#[path = "ui_source_control_actions_tests.rs"]
mod actions_tests;

pub(crate) fn handle_key(app: &mut App, code: crossterm::event::KeyCode, modifiers: crossterm::event::KeyModifiers) {
    use crossterm::event::{KeyCode, KeyModifiers};
    if !app.source_control_enabled {
        if matches!(code, KeyCode::Enter | KeyCode::Char(' ')) && modifiers.is_empty() {
            app.toggle_workspace_source_control();
        }
        return;
    }
    if matches!(app.source_control_repository_state,
        crate::app::SourceControlRepositoryState::Checking | crate::app::SourceControlRepositoryState::Loading)
    {
        return;
    }
    if app.source_control_repository_state == crate::app::SourceControlRepositoryState::Missing {
        if matches!(code, KeyCode::Enter | KeyCode::Char(' ')) {
            if app.repositories.parent.is_some() { app.open_parent_repository(); }
            else if !app.repositories.closed.is_empty() { app.reopen_source_control_repositories(); }
            else { app.git_initialize_repository(); }
        }
        return;
    }
    if modifiers.contains(KeyModifiers::ALT) && matches!(code, KeyCode::Up | KeyCode::Down) {
        let roots: Vec<_> = app.repositories.entries.iter().filter(|repo| !app.repositories.closed.contains(&repo.root))
            .map(|repo| repo.root.clone()).collect();
        if !roots.is_empty() {
            let current = roots.iter().position(|root| Some(root) == app.repositories.active.as_ref()).unwrap_or(0);
            let next = if code == KeyCode::Up { current.saturating_sub(1) } else { (current + 1).min(roots.len() - 1) };
            if next != current { app.select_source_control_repository(&roots[next]); }
            app.repositories.header_scroll = app.repositories.header_scroll.min(next);
            let capacity = app.repositories.headers.len().max(1);
            if next >= app.repositories.header_scroll + capacity { app.repositories.header_scroll = next + 1 - capacity; }
        }
        return;
    }
    if app.source_control_repository_state == crate::app::SourceControlRepositoryState::Failed {
        let count = app.repositories.recovery_buttons.len();
        match code {
            KeyCode::Tab | KeyCode::BackTab if count > 0 => {
                let backwards = code == KeyCode::BackTab || modifiers.contains(KeyModifiers::SHIFT);
                app.repositories.recovery_focus = Some(match app.repositories.recovery_focus {
                    Some(index) if backwards => (index + count - 1) % count,
                    Some(index) => (index + 1) % count,
                    None if backwards => count - 1,
                    None => 0,
                });
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let action = app.repositories.recovery_focus
                    .and_then(|index| app.repositories.recovery_buttons.get(index))
                    .map_or(crate::git_trust::RecoveryAction::Retry, |(_, action)| *action);
                action.run(app);
            }
            _ => {}
        }
        return;
    }
    if app.repositories.discovered && !app.repositories.expanded {
        if matches!(code, KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Tab) { app.repositories.expanded = true; }
        return;
    }
    match code {
        KeyCode::Char('a' | 'A') if modifiers.contains(KeyModifiers::CONTROL) && !app.sc_graph_focused => {
            app.select_all_source_control_files();
        }
        KeyCode::Delete if !app.sc_graph_focused => {
            app.request_source_control_discard(app.selected_source_control_changes(), false);
        }
        KeyCode::Esc if !app.sc_graph_focused => {
            app.select_source_control_file(app.source_control_selected, false, false);
        }
        KeyCode::Tab | KeyCode::BackTab => {
            let reverse = code == KeyCode::BackTab || modifiers.contains(KeyModifiers::SHIFT);
            if app.sc_graph_focused == reverse {
                app.sc_graph_focused = !reverse;
                app.sc_graph_collapsed = false;
            } else {
                app.source_control_input_mode = true;
            }
        }
        KeyCode::Char('m') => { app.source_control_input_mode = true; }
        KeyCode::Char('f') if modifiers.is_empty() => { app.execute_git_command("git_filter_changes"); }
        KeyCode::Char('r') | KeyCode::F(5) => { app.repositories.rescan = true; app.refresh_source_control_async(); app.refresh_git_log_async(); }
        KeyCode::Enter => {
            if app.sc_graph_focused { app.open_source_control_commit_details(app.git_log_selected); }
            else if app.source_control_file_visible(app.source_control_selected) { app.open_source_control_diff(app.source_control_selected); }
        }
        KeyCode::Char(' ') if !app.sc_graph_focused => {
            if modifiers.contains(KeyModifiers::CONTROL) {
                app.select_source_control_file(app.source_control_selected, true, false);
            } else if let Some(file) = app.source_control_files.get(app.source_control_selected) {
                if file.staged { app.git_unstage_changes(app.selected_source_control_changes()); }
                else { app.git_stage_changes(app.selected_source_control_changes()); }
            }
        }
        KeyCode::Up | KeyCode::Down | KeyCode::Home | KeyCode::End | KeyCode::PageUp | KeyCode::PageDown => {
            let old_selection = app.source_control_selected;
            let (changes_height, graph_height) = source_control_split_heights(
                app.source_control_detail_area().map_or(24, |area| area.height), app.sc_graph_collapsed, app.sc_graph_height,
            );
            let visible = if app.sc_graph_focused { graph_height.saturating_sub(1).max(1) as usize }
                else { source_control_file_list_visible_rows(changes_height).max(1) };
            let layout = app.source_control_layout();
            let (selected, count) = if app.sc_graph_focused { (app.git_log_selected, app.git_log.len()) }
                else { (layout.file_ordinal(old_selection).unwrap_or(0), layout.file_count()) };
            if count == 0 { return; }
            let max = count.saturating_sub(1);
            let selected = match code {
                KeyCode::Up => selected.saturating_sub(1), KeyCode::Down => (selected + 1).min(max),
                KeyCode::PageUp => selected.saturating_sub(visible), KeyCode::PageDown => (selected + visible).min(max),
                KeyCode::Home => 0, KeyCode::End => max, _ => selected,
            };
            if app.sc_graph_focused {
                app.git_log_selected = selected;
                app.git_log_scroll = app.git_log_scroll.min(selected);
                if selected >= app.git_log_scroll + visible { app.git_log_scroll = selected + 1 - visible; }
                app.sc_graph_collapsed = false;
                app.prefetch_git_history(visible);
            } else if let Some(selected) = layout.file_at(selected) {
                let group = Group::of(&app.source_control_files[selected]);
                app.set_source_control_group_collapsed(group, false);
                let layout = app.source_control_layout();
                if let Some(row) = layout.file_row(selected) {
                    app.source_control_scroll = app.source_control_scroll.min(row);
                    if row >= app.source_control_scroll + visible { app.source_control_scroll = row + 1 - visible; }
                    app.source_control_scroll = app.source_control_scroll.min(layout.max_scroll(visible));
                }
                let shift = modifiers.contains(KeyModifiers::SHIFT);
                let ctrl = modifiers.contains(KeyModifiers::CONTROL);
                if !ctrl || shift {
                    app.select_source_control_file(selected, ctrl, shift);
                } else {
                    app.source_control_selected = selected;
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn source_control_file_list_visible_rows(changes_height: u16) -> usize {
    changes_height.saturating_sub(SC_CHANGES_FIXED_ROWS) as usize
}

/// Return the largest rendered-row offset that still fills the visible list.
#[cfg(test)]
pub(crate) fn source_control_file_list_max_scroll(
    files: &[crate::app::GitFileChange],
    staged_collapsed: bool,
    changes_collapsed: bool,
    changes_height: u16,
) -> usize {
    crate::source_control_rows::Layout::new(files, Default::default(), [false, staged_collapsed, changes_collapsed, false])
        .max_scroll(source_control_file_list_visible_rows(changes_height))
}

/// Allocate a Source Control row's flexible space. The filename always gets
/// first claim; the parent path is only shown in whatever space remains.
fn sc_file_row_text(
    file_name: &str,
    parent: &str,
    row_width: usize,
    fixed_prefix_width: usize,
    status_width: usize,
) -> (String, String, usize) {
    let minimum_gap = usize::from(row_width > fixed_prefix_width + status_width);
    let content_width = row_width
        .saturating_sub(fixed_prefix_width + status_width + minimum_gap);
    let display_name = truncate_to_width(file_name, content_width);
    let name_width = UnicodeWidthStr::width(display_name.as_str());
    let parent_budget = content_width.saturating_sub(name_width);
    let display_parent = truncate_to_width(parent, parent_budget);
    let parent_width = UnicodeWidthStr::width(display_parent.as_str());
    let gap_width = row_width.saturating_sub(
        fixed_prefix_width + name_width + parent_width + status_width,
    );
    (display_name, display_parent, gap_width)
}

pub(crate) fn sc_actions_area(area: Rect) -> Option<Rect> {
    let full = UnicodeWidthStr::width(SC_ACTIONS_LABEL) as u16;
    (!area.is_empty()).then(|| Rect::new(area.x, area.y,
        if area.width >= full { full } else if area.width >= 3 { 3 } else { 1 }, 1))
}

fn draw_sc_actions(f: &mut Frame, app: &mut App, area: Rect) {
    let refresh_width = if app.source_control_enabled && area.width >= 2 {
        if area.width >= 6 { 3 } else { 1 }
    } else { 0 };
    let controls = Rect::new(area.x, area.y, area.width.saturating_sub(refresh_width), area.height);
    let Some(button) = sc_actions_area(controls) else { return; };
    let label = if button.width >= UnicodeWidthStr::width(SC_ACTIONS_LABEL) as u16 {
        SC_ACTIONS_LABEL
    } else if button.width >= 3 { " ⋮ " } else { "⋮" };
    let open = app.source_control_action_menu_open();
    let hovered = button.contains((app.mouse_col, app.mouse_row).into());
    f.render_widget(Paragraph::new(label).style(app.colors.ui.toolbar_layer_style(
        app.colors.ui.sidebar_foreground, app.colors.bg_sidebar, hovered || open, false,
        open || app.sc_visibility != Default::default())), button);
    app.source_control_actions_area = Some(button);
    if !app.source_control_enabled { return; }
    if refresh_width > 0 {
        let refresh = Rect::new(area.right() - refresh_width, area.y, refresh_width, 1);
        f.render_widget(Paragraph::new(if refresh_width >= 3 { " ↻ " } else { "↻" })
            .style(app.colors.ui.toolbar_layer_style(app.colors.ui.sidebar_foreground,
                app.colors.bg_sidebar, refresh.contains((app.mouse_col, app.mouse_row).into()), false, false)), refresh);
        app.source_control_refresh_area = Some(refresh);
    }
    let closed = app.repositories.closed_roots().count();
    if closed == 0 { return; }
    let available = controls.right().saturating_sub(button.right().saturating_add(1));
    let full_label = format!(" Reopen ({closed}) ▾ ");
    let label = if full_label.width() <= usize::from(available) {
        full_label.as_str()
    } else if available >= " Reopen ▾ ".width() as u16 {
        " Reopen ▾ "
    } else { return; }; // The Actions menu still exposes it on narrow sidebars.
    let width = label.width() as u16;
    let reopen = Rect::new(controls.right() - width, area.y, width, 1);
    let open = app.context_menu_active
        && app.context_menu_items.first().is_some_and(|item| item.id == "git_reopen_repository");
    f.render_widget(Paragraph::new(label).style(app.colors.ui.toolbar_layer_style(
        app.colors.ui.sidebar_foreground, app.colors.bg_sidebar,
        open || reopen.contains((app.mouse_col, app.mouse_row).into()), false, open)), reopen);
    app.repositories.reopen_button = Some(reopen);
}

pub(crate) fn source_control_split_heights(
    total_h: u16,
    graph_collapsed: bool,
    preferred_graph_h: u16,
) -> (u16, u16) {
    if total_h == 0 {
        return (0, 0);
    }
    let min_changes = 8u16;
    if graph_collapsed {
        if total_h >= 2 {
            return (total_h.saturating_sub(1), 1);
        }
        return (total_h, 0);
    }

    let max_graph = total_h.saturating_sub(min_changes);
    if max_graph < 2 {
        return (total_h, 0);
    }
    let min_graph = 6u16.min(max_graph);
    let graph_h = preferred_graph_h.clamp(min_graph, max_graph);
    (total_h.saturating_sub(graph_h), graph_h)
}

pub(crate) fn draw_source_control_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    let bg = app.colors.bg_sidebar;
    f.render_widget(Paragraph::new("").style(Style::default().bg(bg)), area);

    // The workbench paints the sidebar/editor seam after panel contents. Keep
    // that edge out of the Source Control layout so its scrollbar is not
    // overwritten by the separator on the following render pass.
    let content_area = if area.width > 1 {
        if app.sidebar_right {
            Rect::new(area.x + 1, area.y, area.width - 1, area.height)
        } else {
            Rect::new(area.x, area.y, area.width - 1, area.height)
        }
    } else {
        area
    };

    app.source_control_initialize_area = None;
    app.source_control_toggle_area = None;
    app.source_control_commit_area = None;
    app.source_control_amend_area = None;
    app.source_control_stage_all_area = None;
    app.source_control_unstage_all_area = None;
    app.source_control_discard_all_area = None;
    app.source_control_untracked_stage_area = None;
    app.source_control_untracked_discard_area = None;
    app.source_control_actions_area = None;
    app.source_control_refresh_area = None;
    app.source_control_file_actions.clear();
    app.repositories.headers.clear();
    app.repositories.closes.clear();
    app.repositories.parent_button = None;
    app.repositories.reopen_button = None;
    app.repositories.detail_area = None;
    app.repositories.recovery_buttons.clear();
    app.repositories.recovery_root = None;
    draw_sc_actions(f, app, content_area);
    match app.source_control_repository_state {
        crate::app::SourceControlRepositoryState::Disabled => {
            draw_sc_disabled(f, app, content_area);
            return;
        }
        crate::app::SourceControlRepositoryState::Checking | crate::app::SourceControlRepositoryState::Loading => {
            draw_sc_repository_state(f, app, content_area, false);
            return;
        }
        crate::app::SourceControlRepositoryState::Missing => {
            draw_sc_repository_state(f, app, content_area, true);
            return;
        }
        crate::app::SourceControlRepositoryState::Available | crate::app::SourceControlRepositoryState::Failed => {}
    }

    let content_area = draw_repository_headers(f, app, content_area);
    app.repositories.detail_area = Some(content_area);
    if app.source_control_repository_state == crate::app::SourceControlRepositoryState::Failed {
        if app.repositories.expanded || app.repositories.open_count() == 0 {
            draw_sc_repository_failure(f, app, content_area);
        }
        return;
    }
    if content_area.is_empty() || (app.repositories.discovered && !app.repositories.expanded) { return; }
    let total_h = content_area.height;
    let (changes_h, graph_h) =
        source_control_split_heights(total_h, app.sc_graph_collapsed, app.sc_graph_height);

    let changes_area = Rect::new(
        content_area.x,
        content_area.y,
        content_area.width,
        changes_h,
    );
    draw_sc_changes_panel(f, app, changes_area);

    if graph_h > 0 {
        let graph_area = Rect::new(
            content_area.x,
            content_area.y + changes_h,
            content_area.width,
            graph_h,
        );
        draw_sc_graph_panel(f, app, graph_area);
    }
}

fn draw_repository_headers(f: &mut Frame, app: &mut App, area: Rect) -> Rect {
    if !app.repositories.discovered { return area; }
    let bg = app.colors.bg_sidebar;
    let mut y = area.y.saturating_add(1).min(area.bottom());
    if app.repositories.parent.is_some() && !app.repositories.parent_open && y < area.bottom() {
        let button = Rect::new(area.x, y, area.width, 1);
        f.render_widget(Paragraph::new("Open parent repository…").style(app.colors.ui.toolbar_layer_style(
            app.colors.ui.sidebar_foreground, bg, button.contains((app.mouse_col, app.mouse_row).into()), false, false)), button);
        app.repositories.parent_button = Some(button);
        y += 1;
    }
    let count = app.repositories.open_count();
    let capacity = area.height.saturating_sub(y - area.y).saturating_sub(8).max(1).min(6) as usize;
    app.repositories.header_scroll = app.repositories.header_scroll.min(count.saturating_sub(capacity));
    for repo in app.repositories.entries.iter().filter(|repo| !app.repositories.closed.contains(&repo.root))
        .skip(app.repositories.header_scroll).take(capacity)
    {
        if y >= area.bottom() { break; }
        let header = Rect::new(area.x, y, area.width, 1);
        let close_width = area.width.min(3);
        let close = Rect::new(area.right().saturating_sub(close_width), y, close_width, 1);
        let active = app.repositories.active.as_ref() == Some(&repo.root);
        let expanded = active && app.repositories.expanded;
        let total = repo.change_count(app.sc_visibility);
        let suffix = format!(" ({total})");
        let name = repo.root.file_name().unwrap_or(repo.root.as_os_str()).to_string_lossy();
        let available = area.width.saturating_sub(close_width) as usize;
        let prefix_width = if count > 1 { 3 } else { 1 };
        let active_label = " Active ";
        let badge = if active && count > 1 && available >= prefix_width + suffix.width() + active_label.width() + 8 { active_label } else { "" };
        let name = truncate_to_width(&name, available.saturating_sub(prefix_width + suffix.width() + badge.width()));
        let hovered = count > 1 && header.contains((app.mouse_col, app.mouse_row).into());
        // Selection remains visible even when its details are collapsed or the
        // editor has focus. A disclosure arrow only describes expansion.
        let mut style = app.colors.ui.list_row_style(app.colors.ui.sidebar_foreground, bg, active && count > 1, true, hovered);
        if active { style = style.add_modifier(Modifier::BOLD); }
        let row_bg = style.bg.unwrap_or(bg);
        let mut spans = Vec::new();
        if count > 1 {
            spans.push(Span::styled(if active { "▌" } else { " " }, Style::default()
                .fg(app.colors.ui.list_icon_color(app.colors.fg_accent_blue, row_bg, active, true))));
            spans.push(Span::raw(if expanded { "▾ " } else { "▸ " }));
        } else { spans.push(Span::raw(" ")); }
        spans.push(Span::raw(format!("{name}{suffix}")));
        if !badge.is_empty() {
            let padding = available.saturating_sub(prefix_width + name.width() + suffix.width() + badge.width());
            spans.push(Span::raw(crate::helpers::spaces(padding)));
            spans.push(Span::styled(badge, app.colors.ui.badge_style(row_bg).add_modifier(Modifier::BOLD)));
        }
        f.render_widget(Paragraph::new(Line::from(spans)).style(style), header);
        f.render_widget(Paragraph::new(" × ").style(app.colors.ui.toolbar_layer_style(
            app.colors.ui.sidebar_foreground, row_bg, close.contains((app.mouse_col, app.mouse_row).into()), false, false)), close);
        app.repositories.headers.push((header, repo.root.clone()));
        app.repositories.closes.push((close, repo.root.clone()));
        y += 1;
    }
    Rect::new(area.x, y, area.width, area.bottom().saturating_sub(y))
}

fn draw_sc_disabled(f: &mut Frame, app: &mut App, area: Rect) {
    if area.height < 2 || area.width == 0 { return; }
    let button = sc_initialize_button_area(area).unwrap_or(Rect::new(area.x, area.bottom() - 1, area.width, 1));
    if button.y > area.y + 2 {
        f.render_widget(Paragraph::new("Source Control is off.")
            .style(Style::default().fg(app.colors.ui.description_color(app.colors.bg_sidebar)))
            .alignment(Alignment::Center).wrap(Wrap { trim: true }),
            Rect::new(area.x.saturating_add(1), area.y + 2, area.width.saturating_sub(2), button.y - area.y - 2));
    }
    let hovered = button.contains((app.mouse_col, app.mouse_row).into());
    f.render_widget(Paragraph::new(if button.width >= 21 { "Enable Source Control" } else { "Enable" }).alignment(Alignment::Center)
        .style(app.colors.ui.toolbar_layer_style(app.colors.ui.sidebar_foreground,
            app.colors.bg_sidebar, hovered, false, true)), button);
    app.source_control_toggle_area = Some(button);
}

fn sc_initialize_button_area(area: Rect) -> Option<Rect> {
    if area.width < 4 || area.height < 4 {
        return None;
    }
    let preferred_width = UnicodeWidthStr::width(SC_INITIALIZE_BUTTON_LABEL) as u16 + 2;
    let width = preferred_width.min(area.width.saturating_sub(2));
    let relative_y = if area.height >= 9 {
        7
    } else {
        area.height.saturating_sub(1)
    };
    Some(Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + relative_y,
        width,
        1,
    ))
}

fn draw_sc_repository_failure(f: &mut Frame, app: &mut App, area: Rect) {
    use crate::git_trust::{FailureKind, RecoveryAction};
    if area.is_empty() { return; }
    let Some(error) = app.repositories.failure().cloned() else { return; };
    let ownership = error.kind == FailureKind::Ownership;
    let busy = app.bg_git_status_running || app.bg_git_action_running || app.bg_git_remote_running;
    let title = if busy { "Updating Git…" }
        else if ownership { "Git repository requires trust" }
        else { "Could not load Git repository" };
    let bg = app.colors.bg_sidebar;
    let normal = Style::default().bg(bg).fg(app.colors.ui.sidebar_text_color(bg));
    f.render_widget(Paragraph::new(title).style(normal.add_modifier(Modifier::BOLD)).wrap(Wrap { trim: true }),
        Rect::new(area.x + u16::from(area.width > 1), area.y, area.width.saturating_sub(2), area.height.min(2)));
    let actions: &[(RecoveryAction, &str)] = if ownership {
        &[(RecoveryAction::Trust, "Trust Repository"), (RecoveryAction::Retry, "Retry")]
    } else { &[(RecoveryAction::Retry, "Retry")] };
    let controls_top = area.bottom().saturating_sub(actions.len() as u16 * 2).max(area.y.saturating_add(3));
    let path = app.remote_workspace.as_ref().and_then(|remote| remote.display_path(&error.root).ok())
        .unwrap_or_else(|| {
            let path = error.root.to_string_lossy();
            #[cfg(windows)]
            {
                if let Some(tail) = path.strip_prefix(r"\\?\UNC\") { return format!(r"\\{tail}"); }
                if let Some(tail) = path.strip_prefix(r"\\?\") { return tail.to_owned(); }
            }
            path.into_owned()
        });
    let detail = if ownership {
        format!("{path}\n\nGit could not verify repository ownership.\n\nTrust adds this repository to Git's safe.directory setting.")
    } else { format!("{path}\n\n{}", error.message) };
    let body_top = area.y.saturating_add(2).min(area.bottom());
    f.render_widget(Paragraph::new(detail).style(normal).wrap(Wrap { trim: false }),
        Rect::new(area.x + u16::from(area.width > 1), body_top, area.width.saturating_sub(2),
            controls_top.min(area.bottom()).saturating_sub(body_top)));
    for (index, (action, label)) in actions.iter().enumerate() {
        let row = controls_top.saturating_add(index as u16 * 2);
        if row >= area.bottom() || label.width() > usize::from(area.width.saturating_sub(2)) { continue; }
        let width = (label.width() as u16 + 2).min(area.width.saturating_sub(2));
        let button = Rect::new(area.x + (area.width - width) / 2, row, width, 1);
        let focused = app.repositories.recovery_focus == Some(app.repositories.recovery_buttons.len());
        let style = crate::ui_widgets::action_button_style(&app.colors, bg,
            app.repositories.recovery_buttons.is_empty(),
            button.contains((app.mouse_col, app.mouse_row).into()) || focused, !busy);
        f.render_widget(Paragraph::new(*label).alignment(Alignment::Center)
            .style(if focused && !busy { style.add_modifier(Modifier::UNDERLINED) } else { style }), button);
        app.repositories.recovery_buttons.push((button, *action));
        app.repositories.recovery_root = Some(error.root.clone());
    }
}

fn draw_sc_repository_state(f: &mut Frame, app: &mut App, area: Rect, missing: bool) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    if !missing {
        if area.height > 2 {
            f.render_widget(
                Paragraph::new(Span::styled(
                    if app.source_control_repository_state == crate::app::SourceControlRepositoryState::Loading {
                        "Loading Git changes…"
                    } else {
                        "Detecting Git repository…"
                    },
                    Style::default().fg(app.colors.ui.description_color(app.colors.bg_sidebar)),
                ))
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true }),
                Rect::new(
                    area.x.saturating_add(1),
                    area.y.saturating_add(2),
                    area.width.saturating_sub(2),
                    area.height.saturating_sub(2).min(2),
                ),
            );
        }
        if area.height > 4
            && app.source_control_repository_state == crate::app::SourceControlRepositoryState::Loading
        {
            f.render_widget(
                Paragraph::new("Scanning tracked and untracked files…")
                    .style(Style::default().fg(app.colors.ui.description_color(app.colors.bg_sidebar)))
                    .alignment(Alignment::Center)
                    .wrap(Wrap { trim: true }),
                Rect::new(area.x.saturating_add(1), area.y.saturating_add(4),
                    area.width.saturating_sub(2), area.height.saturating_sub(4)),
            );
        }
        return;
    }

    if app.repositories.parent.is_some() || !app.repositories.closed.is_empty() {
        let parent = app.repositories.parent.as_ref();
        let message = if parent.is_some() { "Git repository found in the parent folder." } else { "All Git repositories are closed." };
        let path = parent.map_or(String::new(), |path| path.display().to_string());
        f.render_widget(Paragraph::new(format!("{message}\n{path}"))
            .style(Style::default().fg(app.colors.ui.sidebar_text_color(app.colors.bg_sidebar)))
            .alignment(Alignment::Center).wrap(Wrap { trim: true }),
            Rect::new(area.x.saturating_add(1), area.y.saturating_add(2), area.width.saturating_sub(2), area.height.saturating_sub(2).min(4)));
        if let Some(button) = sc_initialize_button_area(area) {
            let label = if parent.is_some() { "Open Repository" }
                else if button.width >= "Reopen All Repositories".width() as u16 { "Reopen All Repositories" }
                else { "Reopen All" };
            f.render_widget(Paragraph::new(label)
                .alignment(Alignment::Center).style(crate::ui_widgets::action_button_style(&app.colors,
                    app.colors.bg_sidebar, true, button.contains((app.mouse_col, app.mouse_row).into()), true)), button);
            app.repositories.parent_button = Some(button);
        }
        return;
    }

    if area.height > 2 {
        f.render_widget(
            Paragraph::new(Span::styled(
                "No Git repository",
                Style::default()
                    .fg(app.colors.ui.sidebar_text_color(app.colors.bg_sidebar))
                    .add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Center),
            Rect::new(
                area.x.saturating_add(1),
                area.y.saturating_add(2),
                area.width.saturating_sub(2),
                1,
            ),
        );
    }

    let button_area = sc_initialize_button_area(area);
    if let Some(button) = button_area {
        if button.y > area.y.saturating_add(4) {
            f.render_widget(
                Paragraph::new(Span::styled(
                    "This folder is not under Git source control.",
                    Style::default().fg(app.colors.ui.description_color(app.colors.bg_sidebar)),
                ))
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true }),
                Rect::new(
                    area.x.saturating_add(1),
                    area.y.saturating_add(4),
                    area.width.saturating_sub(2),
                    button.y.saturating_sub(area.y.saturating_add(4)),
                ),
            );
        }

        let initializing = app.bg_git_action_running;
        let hovered = !initializing
            && app.mouse_row == button.y
            && app.mouse_col >= button.x
            && app.mouse_col < button.x.saturating_add(button.width);
        let label = if initializing {
            "Initializing…"
        } else {
            SC_INITIALIZE_BUTTON_LABEL
        };
        let button_text = format!("{label:^width$}", width = button.width as usize);
        f.render_widget(
            Paragraph::new(Span::styled(
                button_text,
                crate::ui_widgets::action_button_style(&app.colors, app.colors.bg_sidebar, true, hovered, !initializing)
                    .add_modifier(Modifier::BOLD),
            )),
            button,
        );
        app.source_control_initialize_area = Some(button);
    }
}

/// Render the commit message input box with cursor and selection.
fn draw_sc_commit_input(f: &mut Frame, app: &mut App, input_area: Rect) {
    use unicode_segmentation::UnicodeSegmentation;
    let active = app.source_control_input_mode;
    let base = app.colors.ui.input_style(app.colors.bg_sidebar);
    let input_bg = base.bg.unwrap_or(app.colors.bg_sidebar);
    let border = if active { app.colors.ui.focus_border } else { app.colors.ui.input_colors.border };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border.border(input_bg).unwrap_or(input_bg)))
        .style(base);
    let content = block.inner(input_area);
    f.render_widget(block, input_area);
    if content.width == 0 || content.height == 0 {
        return;
    }
    app.prepare_commit_input(content.width as usize, content.height as usize);
    let cursor = app.source_control_commit_cursor;
    let cursor_row = app.commit_input.cursor_row(cursor);
    let selection = App::inline_selection_range(cursor, app.source_control_commit_selection_anchor);
    let selected = app.colors.ui.input_selection_style(input_bg);
    let caret_style = app.colors.ui.input_caret_style(input_bg).add_modifier(Modifier::BOLD);
    let mut lines = Vec::with_capacity(content.height as usize);
    let mut hit_rows = Vec::with_capacity(content.height as usize);
    for y in 0..content.height as usize {
        let row = app.commit_input.scroll + y;
        let mut spans = Vec::new();
        let mut hits = Vec::new();
        let end = if let Some(range) = app.commit_input.rows.get(row) {
            for (offset, g) in app.source_control_commit_msg[range.clone()].grapheme_indices(true) {
                let byte = range.start + offset;
                let caret_at_full_line_end = cursor == range.end
                    && byte + g.len() == cursor
                    && hits.len() + g.width() == content.width as usize
                    && app.source_control_commit_msg.as_bytes().get(cursor) == Some(&b'\n');
                let is_cursor = active
                    && app.cursor_visible
                    && row == cursor_row
                    && (byte == cursor || caret_at_full_line_end);
                let is_selected =
                    selection.is_some_and(|(start, end)| byte < end && byte + g.len() > start);
                let style = if is_cursor {
                    caret_style
                } else if is_selected {
                    selected
                } else {
                    base
                };
                let display = if g == "\t" { " " } else { g };
                spans.push(Span::styled(display, style));
                hits.extend(std::iter::repeat_n(byte, display.width()));
            }
            if hits.len() < content.width as usize {
                let caret =
                    active && app.cursor_visible && row == cursor_row && cursor == range.end;
                let newline_selected =
                    selection.is_some_and(|(start, end)| range.end >= start && range.end < end);
                let style = if caret {
                    caret_style
                } else if newline_selected {
                    selected
                } else {
                    base
                };
                spans.push(Span::styled(" ", style));
            }
            range.end
        } else {
            app.source_control_commit_msg.len()
        };
        hits.resize(content.width as usize, end);
        lines.push(Line::from(spans));
        hit_rows.push(hits);
    }
    if app.source_control_commit_msg.is_empty() && !active {
        lines[0] = Line::from(Span::styled("Message", base.fg(app.colors.ui.input_placeholder_color(input_bg))));
    }
    f.render_widget(Paragraph::new(lines).style(base), content);
    app.register_text_input(
        TextInputId::SourceControlCommit,
        TextInputHitMap::multiline(content, hit_rows, app.source_control_commit_msg.len()),
    );
    if let Some(bar) = app.commit_input.scrollbar(content) {
        let dragging = app.commit_input.scrollbar_drag.is_some();
        let (track, thumb) = app.colors.ui.scrollbar_colors(input_bg, false, dragging);
        bar.symbols('▐', '▐').colors(thumb, track, track).render(f);
    }
}

fn sc_section_header(app: &App, label: &str, collapsed: bool, count: Option<usize>, width: u16) -> Line<'static> {
    let bg = app.colors.ui.bg_section;
    let fg = app.colors.ui.readable_foreground(app.colors.ui.fg_section, bg, 4.5);
    let mut header = crate::ui_widgets::CollapsibleHeader::new(label, collapsed).colors(fg, bg);
    if let Some(count) = count { header = header.count(count); }
    let mut line = header.to_line();
    line.spans.push(Span::styled(crate::helpers::spaces((width as usize).saturating_sub(line.width())),
        Style::default().fg(fg).bg(bg)));
    if let Some(border) = app.colors.ui.border_section {
        line.style = Style::default().add_modifier(Modifier::UNDERLINED).underline_color(border);
    }
    line
}

fn draw_sc_changes_panel(f: &mut Frame, app: &mut App, area: Rect) {
    let bg = app.colors.bg_sidebar;
    app.source_control_stage_all_area = None;
    app.source_control_unstage_all_area = None;
    app.source_control_discard_all_area = None;
    app.source_control_untracked_stage_area = None;
    app.source_control_untracked_discard_area = None;
    app.source_control_file_actions.clear();
    if area.height < 2 { return; }

    // ── Commit message input box (3 rows with border) ──────────────────────
    let input_h = 4u16;
    let input_area = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        input_h.min(area.height.saturating_sub(1)),
    );
    if input_area.height > 0 {
        draw_sc_commit_input(f, app, input_area);
    }

    let after_input_y = area.y + 1 + input_h;
    if after_input_y >= area.y + area.height {
        return;
    }
    let remaining_h = area.height.saturating_sub(1 + input_h);

    // ── Commit / Amend buttons ─────────────────────────────────────────────
    let btn_area = Rect::new(area.x, after_input_y, area.width, 1.min(remaining_h));
    if btn_area.height > 0 {
        let gap = u16::from(area.width > 2);
        let commit_w = if area.width <= 2 {
            area.width
        } else {
            (area.width - gap) / 2
        };
        let amend_w = area.width.saturating_sub(commit_w + gap);
        let enabled = !app.bg_git_action_running && !app.bg_git_remote_running;
        if commit_w > 0 {
            let button = Rect::new(btn_area.x, btn_area.y, commit_w, 1);
            crate::ui_widgets::draw_action_button(
                f,
                button,
                "✓ Commit",
                &app.colors,
                true,
                button.contains((app.mouse_col, app.mouse_row).into()),
                enabled,
            );
            app.source_control_commit_area = Some(button);
        }
        if amend_w > 0 {
            let button = Rect::new(btn_area.x + commit_w + gap, btn_area.y, amend_w, 1);
            crate::ui_widgets::draw_action_button(
                f,
                button,
                "↻ Amend",
                &app.colors,
                false,
                button.contains((app.mouse_col, app.mouse_row).into()),
                enabled,
            );
            app.source_control_amend_area = Some(button);
        }
    }

    let list_start_y = after_input_y + 1;
    if list_start_y >= area.y + area.height {
        return;
    }
    let list_h = source_control_file_list_visible_rows(area.height) as u16;
    if list_h == 0 {
        return;
    }
    let list_area = Rect::new(area.x, list_start_y, area.width, list_h);

    // ── File sections ──────────────────────────────────────────────────────
    let layout = app.source_control_layout();
    let visible = list_h as usize;
    let total_rows = layout.row_count();
    let max_scroll = layout.max_scroll(visible);
    app.source_control_scroll = app.source_control_scroll.min(max_scroll);
    let scroll = app.source_control_scroll;
    let repository = std::sync::Arc::new(app.source_control_root().to_owned());
    let files = &app.source_control_files;
    let show_scrollbar = total_rows > visible && list_area.width > 0;
    let content_area = Rect::new(
        list_area.x,
        list_area.y,
        list_area.width.saturating_sub(u16::from(show_scrollbar)),
        list_area.height,
    );

    let mut lines: Vec<Line> = Vec::with_capacity(visible);
    let mut section_actions: Vec<(Group, u16)> = Vec::with_capacity(3);
    let mut fmt_buf = String::with_capacity(64);

    for row in layout.rows(scroll).take(visible) {
        let i = match row {
            crate::source_control_rows::Row::Section(group) => {
                let count = layout.count(group);
                if count > 0 {
                    section_actions.push((group, len_as_u16(lines.len())));
                }
                lines.push(sc_section_header(app, group.label(), app.source_control_group_collapsed(group), Some(count), content_area.width));
                continue;
            }
            crate::source_control_rows::Row::File(index) => index,
        };
        let file = &files[i];

        let is_selected = app.source_control_file_selected(i);
        let is_hovered = app.mouse_row == list_start_y + len_as_u16(lines.len())
            && app.mouse_col >= content_area.x
            && app.mouse_col < content_area.x + content_area.width;
        let active = app.active_pane == crate::app::ActivePane::Explorer && !app.source_control_input_mode && !app.sc_graph_focused;
        let row_style = app.colors.ui.list_row_style(app.colors.ui.sidebar_foreground, bg, is_selected, active, is_hovered);
        let row_bg = row_style.bg.unwrap_or(bg);
        let description = if is_selected || is_hovered && app.colors.ui.list_hover_foreground.over(row_bg).is_some() {
            row_style.fg.unwrap()
        } else { app.colors.ui.description_color(row_bg) };
        let status_color = app.colors.ui.git_status_color(file.status, row_bg);
        let status_char = match file.status {
            '!' => "!",
            '?' => "U",
            'A' => "A",
            'M' => "M",
            'D' => "D",
            'R' => "R",
            _ => "?",
        };

        let path = std::path::Path::new(&file.path);
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&file.path);
        let parent = path.parent().and_then(|p| p.to_str()).unwrap_or("");
        let moved_path = file.original_path.as_ref().map(|old| format!("{old} → {}", file.path));
        let parent = moved_path.as_deref().unwrap_or(parent);
        let (icon, icon_color) = if file.directory { ("📁", app.colors.ui.folder_icon_color(row_bg)) }
            else { crate::file_icons::for_path(path, &app.language_associations) };
        // Build file row: [indent][icon][name]...[parent][actions][status]. Filename is
        // deliberately allocated before parent so narrow sidebars stay useful.
        fmt_buf.clear();
        let _ = write!(fmt_buf, "  {icon} ");
        let icon_span = std::mem::take(&mut fmt_buf);
        fmt_buf = String::with_capacity(64);
        let fixed_prefix_width = UnicodeWidthStr::width(icon_span.as_str());
        // Keep a separator before the status badge. Count it as fixed width so
        // it never steals the filename's allocation or joins the parent path.
        fmt_buf.clear();
        let _ = write!(fmt_buf, " {status_char}");
        let status_badge = std::mem::take(&mut fmt_buf);
        fmt_buf = String::with_capacity(64);
        let status_width = UnicodeWidthStr::width(status_badge.as_str());
        let action_width = FileActionRow::width(file.staged, content_area.width
            .saturating_sub((fixed_prefix_width + status_width + 4) as u16));
        if action_width > 0 {
            app.source_control_file_actions.push(FileActionRow {
                index: i, change: file.clone(), repository: repository.clone(),
                area: Rect::new(content_area.right().saturating_sub(status_width as u16 + action_width),
                    list_start_y + len_as_u16(lines.len()), action_width, 1), background: row_bg,
            });
        }
        let (display_name, display_parent, gap_len) = sc_file_row_text(
            file_name,
            parent,
            content_area.width as usize,
            fixed_prefix_width,
            status_width + action_width as usize,
        );
        lines.push(Line::from(vec![
            Span::styled(icon_span, row_style.fg(icon_color)),
            Span::styled(
                display_name,
                row_style,
            ),
            Span::styled(
                crate::helpers::spaces(gap_len),
                row_style,
            ),
            Span::styled(
                display_parent,
                row_style.fg(description),
            ),
            Span::styled(
                crate::helpers::spaces(action_width as usize), row_style,
            ),
            Span::styled(
                status_badge,
                row_style.fg(status_color),
            ),
        ]));
    }

    if layout.file_count() == 0 && lines.len() < visible {
        let hidden_untracked = !app.sc_visibility.untracked && layout.count(Group::Untracked) > 0;
        lines.push(Line::from(Span::styled(
            if hidden_untracked { "  No tracked changes" } else { "  No changes" },
            Style::default().fg(app.colors.ui.description_color(bg)).bg(bg),
        )));
        if hidden_untracked && lines.len() < visible {
            lines.push(Line::from(Span::styled("  Untracked files are hidden", Style::default().fg(app.colors.ui.description_color(bg)).bg(bg))));
        }
    }

    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(bg)),
        content_area,
    );
    file_actions::draw(f, app);
    if content_area.width >= SC_SECTION_ACTION_WIDTH {
        for (group, relative_y) in section_actions {
            if group == Group::Merge { continue; }
            let staged = group == Group::Staged;
            let action_area = Rect::new(
                content_area
                    .x
                    .saturating_add(content_area.width - SC_SECTION_ACTION_WIDTH),
                content_area.y.saturating_add(relative_y),
                SC_SECTION_ACTION_WIDTH,
                1,
            );
            let hovered = app.mouse_row == action_area.y
                && app.mouse_col >= action_area.x
                && app.mouse_col < action_area.x.saturating_add(action_area.width);
            let (label, color) = if staged {
                (" − ", app.colors.ui.git_decoration('D'))
            } else {
                (" + ", app.colors.ui.git_decoration('A'))
            };
            let style = app.colors.ui.toolbar_layer_style(color, app.colors.ui.bg_section, hovered, false, false);
            f.render_widget(Paragraph::new(Span::styled(label, style)), action_area);
            if staged {
                app.source_control_unstage_all_area = Some(action_area);
            } else {
                if group == Group::Untracked {
                    app.source_control_untracked_stage_area = Some(action_area);
                } else {
                    app.source_control_stage_all_area = Some(action_area);
                }
                if content_area.width >= SC_SECTION_ACTION_WIDTH * 2 {
                    let discard = Rect::new(action_area.x - SC_SECTION_ACTION_WIDTH,
                        action_area.y, SC_SECTION_ACTION_WIDTH, 1);
                    let hovered = discard.contains((app.mouse_col, app.mouse_row).into());
                    let style = app.colors.ui.toolbar_layer_style(
                        app.colors.ui.sidebar_foreground, app.colors.ui.bg_section, hovered, false, false);
                    f.render_widget(Paragraph::new(Span::styled(" ↶ ", style)), discard);
                    if group == Group::Untracked {
                        app.source_control_untracked_discard_area = Some(discard);
                    } else {
                        app.source_control_discard_all_area = Some(discard);
                    }
                }
            }
        }
    }
    if show_scrollbar {
        let hovered = app.mouse_col == list_area.right().saturating_sub(1) && (list_area.y..list_area.bottom()).contains(&app.mouse_row);
        let (track, thumb) = app.colors.ui.scrollbar_colors(bg, hovered, false);
        crate::ui_widgets::VerticalScrollbar::new(
            list_area.x + list_area.width - 1,
            list_area.y,
            list_area.height,
            total_rows,
            scroll.min(total_rows.saturating_sub(visible)),
        )
        .symbols('▐', '▐')
        .colors(thumb, track, track)
        .render(f);
    }
}

pub(crate) fn draw_action_tooltip(f: &mut Frame, app: &App) {
    if !app.sidebar_is_visible() || app.sidebar_view != crate::app::SidebarView::SourceControl
        || app.sc_context_active || app.context_menu_active || app.menu_active || app.palette.active
        || crate::ui_modal::active(app).is_some() { return; }
    let point = (app.mouse_col, app.mouse_row).into();
    if let Some((area, root)) = app.repositories.closes.iter().find(|(area, _)| area.contains(point)) {
        crate::ui_widgets::Tooltip::hover(vec![Line::raw("Close Repository"), Line::raw(app.display_path(root))], &app.colors)
            .prefer_below().render(f, area.x, area.y);
        return;
    }
    if let Some((area, root)) = app.repositories.headers.iter().find(|(area, _)| area.contains(point)) {
        let active = app.repositories.active.as_ref() == Some(root);
        let mut lines = vec![Line::raw(if active { "Active repository" } else { "Switch to repository" }),
            Line::raw(app.display_path(root))];
        if active && app.repositories.open_count() > 1 {
            lines.push(Line::raw(if app.repositories.expanded { "Click to collapse" } else { "Click to expand" }));
        }
        crate::ui_widgets::Tooltip::hover(lines, &app.colors).prefer_below().render(f, area.x, area.y);
        return;
    }
    file_actions::draw_tooltip(f, app);
    for (area, label) in [
        (app.source_control_refresh_area, "Refresh Source Control"),
        (app.source_control_toggle_area, if app.source_control_enabled { "Disable Source Control for this workspace" } else { "Enable Source Control for this workspace" }),
        (app.source_control_stage_all_area, "Stage All Unstaged Changes"),
        (app.source_control_unstage_all_area, "Unstage All Changes"),
        (app.source_control_discard_all_area, "Discard All Unstaged Changes"),
        (app.source_control_untracked_stage_area, "Stage All Untracked Files"),
        (app.source_control_untracked_discard_area, "Discard All Untracked Files"),
    ] {
        if let Some(area) = area.filter(|area| area.contains((app.mouse_col, app.mouse_row).into())) {
            crate::ui_widgets::Tooltip::hover(vec![Line::raw(label)], &app.colors)
                .prefer_below().render(f, area.x, area.y);
            return;
        }
    }
}

fn draw_sc_graph_panel(f: &mut Frame, app: &mut App, area: Rect) {
    let bg = app.colors.bg_sidebar;
    f.render_widget(Paragraph::new("").style(Style::default().bg(bg)), area);

    if area.height == 0 {
        return;
    }

    // ── Graph header ──────────────────────────────────────────────────────
    let header_area = Rect::new(area.x, area.y, area.width, 1);
    let collapsed = app.sc_graph_collapsed;
    let mut header = sc_section_header(app, "Graph", collapsed, None, area.width);
    if app.sc_graph_focused && app.active_pane == crate::app::ActivePane::Explorer {
        for span in &mut header.spans { span.style = span.style.add_modifier(Modifier::UNDERLINED); }
    }
    if app.resizing_sc_graph {
        let bg = app.colors.ui.bg_section;
        let foreground = app.colors.ui.sash_hover_border.over(bg).unwrap_or(app.colors.ui.fg_section);
        let foreground = app.colors.ui.readable_foreground(foreground, bg, 4.5);
        for span in &mut header.spans { span.style = span.style.fg(foreground); }
    }
    f.render_widget(
        Paragraph::new(header),
        header_area,
    );

    if collapsed || area.height < 2 {
        return;
    }

    let list_area = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    );
    let visible = list_area.height as usize;
    app.prefetch_git_history(visible);
    let scroll = app.git_log_scroll;
    let selected = app.git_log_selected;
    let total_rows = (app.git_log.len() + usize::from(app.git_history_has_footer())).max(1);
    let show_scrollbar = total_rows > visible && list_area.width > 0;
    let content_area = Rect::new(
        list_area.x,
        list_area.y,
        list_area.width.saturating_sub(u16::from(show_scrollbar)),
        list_area.height,
    );

    let mut lines: Vec<Line> = Vec::with_capacity(visible);

    if app.git_log.is_empty() && !app.git_history_has_footer() {
        lines.push(Line::from(Span::styled(
            "  No commit history",
            Style::default().fg(app.colors.ui.description_color(bg)).bg(bg),
        )));
    } else {
        let graph_budget = (content_area.width as usize / 4).clamp(3, 17).saturating_sub(2);
        let graph_column = app.git_log.iter().skip(scroll).take(visible)
            .map(|commit| UnicodeWidthStr::width(commit.graph.as_str()).max(1).min(graph_budget))
            .max().unwrap_or(1);
        for (i, commit) in app.git_log.iter().enumerate().skip(scroll).take(visible) {
            if lines.len() >= visible {
                break;
            }
            let is_selected = i == selected;
            let is_hovered = app.mouse_row == list_area.y + len_as_u16(lines.len())
                && app.mouse_col >= content_area.x
                && app.mouse_col < content_area.x + content_area.width;
            let active = app.active_pane == crate::app::ActivePane::Explorer && !app.source_control_input_mode && app.sc_graph_focused;
            let row_style = app.colors.ui.list_row_style(app.colors.ui.sidebar_foreground, bg, is_selected, active, is_hovered);
            let row_bg = row_style.bg.unwrap_or(bg);
            let description = if is_selected || is_hovered && app.colors.ui.list_hover_foreground.over(row_bg).is_some() {
                row_style.fg.unwrap()
            } else { app.colors.ui.description_color(row_bg) };

            let graph = if commit.graph.is_empty() { if i == 0 { "●" } else { "○" } } else { &commit.graph };
            let graph = truncate_to_width(graph, graph_budget);
            let graph_width = graph_column + 2;
            let total = content_area.width as usize;
            let hash_width = if total >= 90 { commit.hash.len() + 1 } else { 0 };
            let meta_width = if total >= 64 { (total / 4).min(28) } else { 0 };
            let mut badge_budget = total.saturating_sub(graph_width + hash_width + meta_width + 16);
            let mut badges = Vec::new();
            for (index, branch) in commit.branches.iter().enumerate() {
                let label = format!(" {} ", truncate_to_width(&branch.name, 14));
                let width = UnicodeWidthStr::width(label.as_str()) + 1;
                if width > badge_budget {
                    let remaining = format!(" +{} ", commit.branches.len() - index);
                    if remaining.len() <= badge_budget { badges.push(Span::styled(remaining, row_style.fg(description))); }
                    break;
                }
                badge_budget -= width;
                badges.push(Span::styled(label, app.colors.ui.graph_reference_style(branch.remote, row_bg)));
                badges.push(Span::styled(" ", row_style));
            }
            let badge_width: usize = badges.iter().map(Span::width).sum();
            let message_width = total.saturating_sub(graph_width + hash_width + meta_width + badge_width);
            let message = truncate_to_width(&commit.message, message_width);
            let dot_layer = if i == 0 { app.colors.ui.graph_references[0] } else { app.colors.ui.graph_lane_foreground };
            let dot_color = app.colors.ui.readable_foreground(dot_layer.over(row_bg)
                .unwrap_or_else(|| app.colors.ui.icon_color(row_bg)), row_bg, 3.0);
            let graph_padding = crate::helpers::spaces(graph_column.saturating_sub(UnicodeWidthStr::width(graph.as_str())));
            let mut spans = vec![Span::styled(format!(" {graph}{graph_padding} "), row_style.fg(dot_color))];
            if hash_width > 0 { spans.push(Span::styled(format!("{} ", commit.hash), row_style.fg(description))); }
            let message_padding = message_width.saturating_sub(UnicodeWidthStr::width(message.as_str()));
            spans.push(Span::styled(message, row_style));
            spans.push(Span::styled(crate::helpers::spaces(message_padding), row_style));
            spans.extend(badges);
            if meta_width > 0 {
                let meta = truncate_to_width(&format!("{} · {}", commit.author, commit.time_ago), meta_width.saturating_sub(1));
                spans.push(Span::styled(format!(" {meta}"), row_style.fg(description)));
            }
            let mut line = Line::from(spans);
            line.spans.push(Span::styled(crate::helpers::spaces(total.saturating_sub(line.width())), row_style));
            lines.push(line);
        }
    }

    if app.git_history_has_footer() && scroll + lines.len() == app.git_log.len() && lines.len() < visible {
        let message = if app.git_history.error.is_some() {
            "  Load failed — click to retry"
        } else {
            "  Loading history…"
        };
        let foreground = if app.git_history.error.is_some() { app.colors.ui.error_text_color(bg) }
            else { app.colors.ui.description_color(bg) };
        lines.push(Line::from(Span::styled(message, Style::default().fg(foreground).bg(bg))));
    }
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(bg)),
        content_area,
    );
    if show_scrollbar {
        let hovered = app.mouse_col == list_area.right().saturating_sub(1) && (list_area.y..list_area.bottom()).contains(&app.mouse_row);
        let (track, thumb) = app.colors.ui.scrollbar_colors(bg, hovered, false);
        crate::ui_widgets::VerticalScrollbar::new(
            list_area.x + list_area.width - 1,
            list_area.y,
            list_area.height,
            total_rows,
            scroll.min(total_rows.saturating_sub(visible)),
        )
        .symbols('▐', '▐')
        .colors(thumb, track, track)
        .render(f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_messages_align_when_merge_lanes_have_different_widths() {
        let mut app = App::test_new();
        app.git_log = ["●", "│ ╭─●", "●─╯"].iter().enumerate().map(|(i, graph)| crate::app::GitCommitInfo {
            hash: format!("{i:07x}"), full_hash: format!("{i:040x}"), graph: graph.to_string(),
            message: format!("Message {i}"), author: "Alex".into(), time_ago: "1h ago".into(), branches: vec![],
        }).collect();
        let mut terminal = Terminal::new(TestBackend::new(64, 8)).unwrap();
        terminal.draw(|frame| draw_sc_graph_panel(frame, &mut app, Rect::new(0, 0, 64, 8))).unwrap();
        for y in 1..=3 {
            let start = (0..64).find(|&x| terminal.backend().buffer()[(x, y)].symbol() == "M");
            assert_eq!(start, Some(7));
        }
    }

    #[test]
    fn commit_actions_click_the_rendered_bounds_and_ignore_busy_clicks() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        for sidebar_right in [false, true] {
            for width in [24, 35, 36] {
                let mut app = App::test_new();
                app.sidebar_view = crate::app::SidebarView::SourceControl;
                app.source_control_repository_state =
                    crate::app::SourceControlRepositoryState::Available;
                app.show_explorer = true;
                app.sidebar_right = sidebar_right;
                app.sc_graph_collapsed = true;
                app.source_control_commit_msg = "test".into();
                // The confirmation and validation stop these actions before any Git write.
                app.source_control_files.clear();
                app.git_log.clear();
                let area = Rect::new(4, 2, width, 12);
                app.explorer_area = Some(area);
                let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
                terminal
                    .draw(|frame| draw_source_control_sidebar(frame, &mut app, area))
                    .unwrap();
                let commit = app.source_control_commit_area.unwrap();
                let amend = app.source_control_amend_area.unwrap();
                let click = |app: &mut App, x, y| {
                    crate::mouse::handle_mouse(
                        app,
                        MouseEvent {
                            kind: MouseEventKind::Down(MouseButton::Left),
                            column: x,
                            row: y,
                            modifiers: KeyModifiers::NONE,
                        },
                    )
                };
                for (button, is_amend) in [(commit, false), (amend, true)] {
                    for x in [button.x, button.right() - 1] {
                        app.sc_amend_mode = !is_amend;
                        click(&mut app, x, button.y);
                        assert_eq!(
                            app.sc_amend_mode, is_amend,
                            "sidebar_right={sidebar_right}, width={width}, x={x}"
                        );
                        assert!(!app.bg_git_action_running);
                        if !is_amend {
                            assert_eq!(
                                crate::ui_modal::active(&app),
                                Some(crate::ui_modal::Kind::StageAllCommit),
                            );
                            crate::ui_modal::handle_key(
                                &mut app,
                                crossterm::event::KeyEvent::new(
                                    crossterm::event::KeyCode::Esc, KeyModifiers::NONE,
                                ),
                            );
                            assert!(app.sc_stage_all_commit_request.is_none());
                        }
                    }
                    for remote in [false, true] {
                        app.bg_git_action_running = !remote;
                        app.bg_git_remote_running = remote;
                        app.sc_amend_mode = !is_amend;
                        click(&mut app, button.x, button.y);
                        assert_eq!(app.sc_amend_mode, !is_amend, "busy clicks must be ignored");
                        app.bg_git_action_running = false;
                        app.bg_git_remote_running = false;
                    }
                }
                assert_eq!(amend.x - commit.right(), 1);
                for previous in [false, true] {
                    app.sc_amend_mode = previous;
                    click(&mut app, commit.right(), commit.y);
                    assert_eq!(
                        app.sc_amend_mode, previous,
                        "the gap must not trigger an action"
                    );
                }
                terminal
                    .draw(|frame| {
                        draw_source_control_sidebar(frame, &mut app, Rect::new(4, 2, width, 4))
                    })
                    .unwrap();
                assert!(app.source_control_commit_area.is_none());
                assert!(app.source_control_amend_area.is_none());
            }
        }
    }

    #[test]
    fn commit_input_scrollbar_tracks_wrapped_overflow_and_disappears_when_text_fits() {
        let mut app = App::test_new();
        let mut terminal = Terminal::new(TestBackend::new(30, 8)).unwrap();
        for (text, width, overflow) in [
            ("", 13, false),
            ("one\ntwo", 13, false),
            ("one\ntwo\nthree", 13, true),
            ("alpha beta gamma delta epsilon", 13, true),
            ("alpha beta", 6, true),
            ("alpha beta", 24, false),
        ] {
            app.source_control_commit_msg = text.into();
            app.source_control_commit_cursor = text.len();
            app.text_input_regions.clear();
            terminal.draw(|frame| draw_sc_commit_input(frame, &mut app, Rect::new(2, 1, width, 4))).unwrap();
            let x = width + 1;
            for y in 2..4 {
                assert_eq!(terminal.backend().buffer()[(x, y)].symbol(), if overflow { "▐" } else { "│" });
            }
            let expected = app.commit_input.rows.len().saturating_sub(2);
            assert_eq!(app.commit_input.scroll, expected);
            if overflow {
                let bg = app.colors.ui.input_style(app.colors.bg_sidebar).bg.unwrap_or(app.colors.bg_sidebar);
                let (track, thumb) = app.colors.ui.scrollbar_colors(bg, false, false);
                assert_eq!(terminal.backend().buffer()[(x, 2)].fg, track);
                assert_eq!(terminal.backend().buffer()[(x, 3)].fg, thumb);
            }
        }
        // Very small layouts have no content rows and must not paint outside the box.
        for (width, height) in [(1, 1), (2, 4), (8, 2)] {
            app.text_input_regions.clear();
            terminal.draw(|frame| draw_sc_commit_input(frame, &mut app, Rect::new(0, 0, width, height))).unwrap();
            assert!(app.text_input_regions.is_empty());
        }
    }

    #[test]
    fn commit_input_scrollbar_click_drag_and_wheel_preserve_the_message_and_caret() {
        use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        for sidebar_right in [false, true] {
            let mut app = App::test_new();
            app.show_explorer = true;
            app.sidebar_view = crate::app::SidebarView::SourceControl;
            app.source_control_repository_state = crate::app::SourceControlRepositoryState::Available;
            app.sidebar_right = sidebar_right;
            app.explorer_width = 30;
            app.show_panel = false;
            let message = (1..=8).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n");
            app.source_control_commit_msg = message.clone();
            app.source_control_commit_cursor = message.len();
            app.source_control_commit_selection_anchor = Some(0);
            let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
            let draw = |app: &mut App, terminal: &mut Terminal<TestBackend>| {
                terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
            };
            let mouse = |app: &mut App, kind, column, row| {
                crate::mouse::handle_mouse(app, MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE });
            };
            draw(&mut app, &mut terminal);
            let content = app.text_input_area(TextInputId::SourceControlCommit).unwrap();
            let x = content.right();
            let top = content.y;
            let sidebar = app.explorer_area.unwrap();
            let seam = if sidebar_right { sidebar.x } else { sidebar.right() - 1 };
            assert_ne!(x, seam);
            assert_eq!(terminal.backend().buffer()[(x, top)].symbol(), "▐");
            assert_eq!(app.commit_input.scroll, 6);

            // Clicking the track scrolls without placing the caret or selecting text.
            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, top);
            assert_eq!(app.commit_input.scroll, 0);
            assert!(app.text_input_drag.is_none());
            mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x, top);
            draw(&mut app, &mut terminal);
            assert_eq!(app.commit_input.scroll, 0, "painting must not snap back to the caret");
            mouse(&mut app, MouseEventKind::ScrollDown, x, top);
            draw(&mut app, &mut terminal);
            assert_eq!(app.commit_input.scroll, 3);

            // Preserve the rounded thumb's actual offset when grabbing it, even
            // while crossing the sidebar resize seam and leaving the field.
            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, top);
            assert_eq!(app.commit_input.scroll, 3);
            mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), seam, top);
            assert_eq!(app.commit_input.scroll, 3);
            mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), seam, 1000);
            assert_eq!(app.commit_input.scroll, 6);
            mouse(&mut app, MouseEventKind::Up(MouseButton::Left), seam, 1000);
            assert!(app.commit_input.scrollbar_drag.is_none());
            assert!(!app.resizing_explorer);
            assert_eq!(app.explorer_width, 30);
            assert_eq!(app.source_control_commit_cursor, message.len());
            assert_eq!(app.source_control_commit_selection_anchor, Some(0));
            assert_eq!(app.source_control_commit_msg, message);
            assert_eq!(app.source_control_scroll, 0);
            assert_eq!(app.git_log_scroll, 0);

            app.edit_commit_input(KeyCode::Home, KeyModifiers::CONTROL, None);
            draw(&mut app, &mut terminal);
            assert_eq!(app.commit_input.scroll, 0, "keyboard navigation must still reveal the caret");
        }
    }

    #[test]
    fn commit_input_wraps_and_mouse_selects_words_and_lines() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};
        let mut app = App::test_new();
        app.source_control_commit_msg = "alpha beta gamma".into();
        app.source_control_commit_cursor = app.source_control_commit_msg.len();
        app.source_control_input_mode = true;
        let mut terminal = Terminal::new(TestBackend::new(13, 4)).unwrap();
        terminal
            .draw(|frame| draw_sc_commit_input(frame, &mut app, Rect::new(0, 0, 13, 4)))
            .unwrap();
        let row = |y| {
            (1..12)
                .map(|x| terminal.backend().buffer().cell((x, y)).unwrap().symbol())
                .collect::<String>()
        };
        assert_eq!(row(1), "alpha beta ");
        assert!(row(2).starts_with("gamma"));
        app.last_click_time = std::time::Instant::now() - std::time::Duration::from_secs(1);
        for _ in 0..2 {
            assert!(app.handle_text_input_mouse(
                MouseEventKind::Down(MouseButton::Left),
                8,
                1,
                KeyModifiers::empty()
            ));
            app.handle_text_input_mouse(
                MouseEventKind::Up(MouseButton::Left),
                8,
                1,
                KeyModifiers::empty(),
            );
        }
        assert_eq!(app.source_control_commit_selection_anchor, Some(6));
        assert_eq!(app.source_control_commit_cursor, 10);
        app.handle_text_input_mouse(
            MouseEventKind::Down(MouseButton::Left),
            8,
            1,
            KeyModifiers::empty(),
        );
        assert_eq!(app.source_control_commit_selection_anchor, Some(0));
        assert_eq!(app.source_control_commit_cursor, 16);
    }

    #[test]
    fn commit_input_word_drag_and_drag_past_viewport_extend_selection() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};
        let mut app = App::test_new();
        app.source_control_commit_msg = "alpha beta gamma\nmore text\nlast line".into();
        app.source_control_input_mode = true;
        let mut terminal = Terminal::new(TestBackend::new(13, 4)).unwrap();
        terminal
            .draw(|frame| draw_sc_commit_input(frame, &mut app, Rect::new(0, 0, 13, 4)))
            .unwrap();
        app.last_click_time = std::time::Instant::now() - std::time::Duration::from_secs(1);
        for _ in 0..2 {
            app.handle_text_input_mouse(
                MouseEventKind::Down(MouseButton::Left),
                8,
                1,
                KeyModifiers::empty(),
            );
        }
        app.handle_text_input_mouse(
            MouseEventKind::Drag(MouseButton::Left),
            2,
            2,
            KeyModifiers::empty(),
        );
        assert_eq!(app.source_control_commit_selection_anchor, Some(6));
        assert_eq!(app.source_control_commit_cursor, 16);
        app.handle_text_input_mouse(
            MouseEventKind::Drag(MouseButton::Left),
            2,
            1,
            KeyModifiers::empty(),
        );
        assert_eq!(app.source_control_commit_selection_anchor, Some(10));
        assert_eq!(app.source_control_commit_cursor, 0);
        app.handle_text_input_mouse(
            MouseEventKind::Drag(MouseButton::Left),
            5,
            5,
            KeyModifiers::empty(),
        );
        assert!(app.commit_input.scroll > 0);
        assert!(app.source_control_commit_cursor > 16);
    }

    #[test]
    fn commit_input_wheel_scroll_persists_until_caret_moves() {
        use crossterm::event::{KeyCode, KeyModifiers, MouseEventKind};
        let mut app = App::test_new();
        app.source_control_commit_msg = "one\ntwo\nthree\nfour\nfive".into();
        app.source_control_commit_cursor = app.source_control_commit_msg.len();
        let mut terminal = Terminal::new(TestBackend::new(13, 4)).unwrap();
        terminal
            .draw(|frame| draw_sc_commit_input(frame, &mut app, Rect::new(0, 0, 13, 4)))
            .unwrap();
        assert_eq!(app.commit_input.scroll, 3);
        assert!(app.handle_text_input_mouse(MouseEventKind::ScrollUp, 2, 1, KeyModifiers::empty()));
        terminal
            .draw(|frame| draw_sc_commit_input(frame, &mut app, Rect::new(0, 0, 13, 4)))
            .unwrap();
        assert_eq!(app.commit_input.scroll, 0);
        app.edit_commit_input(KeyCode::Left, KeyModifiers::empty(), None);
        assert_eq!(app.commit_input.scroll, 3);
    }
    use crate::app::{GitCommitInfo, GitFileChange};
    use ratatui::{backend::TestBackend, Terminal};

    fn column_symbols(terminal: &Terminal<TestBackend>, x: u16, top: u16, bottom: u16) -> String {
        (top..bottom)
            .filter_map(|y| terminal.backend().buffer().cell((x, y)))
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn actions_button_fits_even_when_the_sidebar_is_one_cell_wide() {
        for width in [0, 1, 2, 3, 8, 15, 24, 80] {
            let area = Rect::new(5, 2, width, 20);
            if let Some(button) = sc_actions_area(area) {
                assert!(width > 0);
                assert_eq!(button.x, area.x);
                assert_eq!(button.y, area.y);
                assert_eq!(button.height, 1);
                assert!(button.right() <= area.right());
            } else { assert_eq!(width, 0); }
        }
    }

    #[test]
    fn missing_repository_replaces_normal_source_control_controls() {
        let width = 34;
        let height = 14;
        let mut app = App::test_new();
        app.source_control_repository_state =
            crate::app::SourceControlRepositoryState::Missing;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");

        terminal
            .draw(|frame| {
                draw_source_control_sidebar(frame, &mut app, Rect::new(0, 0, width, height));
            })
            .expect("draw missing repository state");

        let buffer = terminal.backend().buffer();
        let mut rendered = String::new();
        for y in 0..height {
            for x in 0..width {
                if let Some(cell) = buffer.cell((x, y)) {
                    rendered.push_str(cell.symbol());
                }
            }
            rendered.push('\n');
        }

        assert!(rendered.contains("No Git repository"));
        assert!(rendered.contains(SC_INITIALIZE_BUTTON_LABEL));
        assert!(!rendered.contains("Message"));
        assert!(!rendered.contains("No changes"));
        assert!(app.source_control_initialize_area.is_some());
    }

    #[test]
    fn narrow_file_row_prioritizes_filename_over_parent_path() {
        let (name, parent, gap) = sc_file_row_text(
            "important_file.rs",
            "very/long/parent/directory",
            27,
            7,
            2,
        );

        assert_eq!(name, "important_file.rs");
        assert!(parent.is_empty());
        assert_eq!(gap, 1);
    }

    #[test]
    fn file_row_uses_remaining_space_for_parent_path() {
        let (name, parent, gap) =
            sc_file_row_text("main.rs", "src/deep", 25, 7, 2);

        assert_eq!(name, "main.rs");
        assert_eq!(parent, "src/deep");
        assert_eq!(gap, 1);
    }

    #[test]
    fn long_filename_is_truncated_before_any_parent_is_shown() {
        let (name, parent, _) =
            sc_file_row_text("extraordinarily_long_name.rs", "src", 18, 7, 2);

        assert_eq!(UnicodeWidthStr::width(name.as_str()), 8);
        assert!(parent.is_empty());
    }

    #[test]
    fn moved_file_row_shows_rename_badge_and_both_paths() {
        let mut app = App::test_new();
        app.source_control_files = vec![GitFileChange { directory: false,
            path: "new/file.rs".into(),
            original_path: Some("old/file.rs".into()),
            status: 'R',
            staged: true,
        }];
        for width in [32, 90] {
            let mut terminal = Terminal::new(TestBackend::new(width, 14)).unwrap();
            terminal.draw(|frame| draw_sc_changes_panel(frame, &mut app, Rect::new(0, 0, width, 14))).unwrap();
            let buffer = terminal.backend().buffer();
            let rows: Vec<String> = (0..14).map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect()).collect();
            let row = rows.iter().find(|row| row.contains("file.rs")).unwrap();
            assert!(row.trim_end().ends_with(" R"), "{row}");
            if width == 90 {
                assert!(row.contains("old/file.rs → new/file.rs"), "{row}");
            }
        }
    }

    #[test]
    fn change_sections_expose_stage_all_and_unstage_all_buttons() {
        let width = 34;
        let height = 14;
        let mut app = App::test_new();
        app.source_control_files = vec![
            GitFileChange { directory: false,
                original_path: None,
                path: "staged.rs".to_string(),
                status: 'M',
                staged: true,
            },
            GitFileChange { directory: false,
                original_path: None,
                path: "unstaged.rs".to_string(),
                status: 'M',
                staged: false,
            },
        ];
        app.sc_staged_collapsed = true;
        app.sc_changes_collapsed = true;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");

        terminal
            .draw(|frame| {
                draw_sc_changes_panel(frame, &mut app, Rect::new(0, 0, width, height));
            })
            .expect("draw source control bulk actions");

        let stage = app.source_control_stage_all_area.expect("stage all button");
        let unstage = app
            .source_control_unstage_all_area
            .expect("unstage all button");
        let buffer = terminal.backend().buffer();
        assert_eq!(
            buffer
                .cell((stage.x + 1, stage.y))
                .expect("stage button cell")
                .symbol(),
            "+"
        );
        assert_eq!(
            buffer
                .cell((unstage.x + 1, unstage.y))
                .expect("unstage button cell")
                .symbol(),
            "−"
        );
        assert!(unstage.y < stage.y);

        // Staging the remaining files keeps the empty Changes dropdown, but
        // there is no bulk stage action when nothing can be staged.
        for file in &mut app.source_control_files {
            file.staged = true;
        }
        app.sidebar_view = crate::app::SidebarView::SourceControl;
        app.source_control_repository_state =
            crate::app::SourceControlRepositoryState::Available;
        app.sc_graph_collapsed = true;
        app.explorer_area = Some(Rect::new(0, 0, width, height));
        terminal
            .draw(|frame| {
                draw_source_control_sidebar(frame, &mut app, Rect::new(0, 0, width, height));
            })
            .expect("draw fully staged files");
        assert!(app.source_control_stage_all_area.is_none());
        assert!(app.source_control_unstage_all_area.is_some());
        let changes_row = (0..height)
            .find(|&y| {
                (0..width)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
                    .contains("Changes (0)")
            })
            .expect("empty Changes section stays visible");
        crate::mouse::handle_mouse(
            &mut app,
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: 3,
                row: changes_row,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );
        assert!(!app.sc_changes_collapsed, "the empty dropdown still toggles");
        assert!(!app.bg_git_action_running);
        assert!(app.source_control_files.iter().all(|file| file.staged));
    }

    #[test]
    fn changes_panel_draws_a_visible_scrollbar_when_files_overflow() {
        let width = 30;
        let height = 12;
        let mut app = App::test_new();
        app.source_control_files = (0..20)
            .map(|index| GitFileChange { directory: false,
                original_path: None,
                path: format!("src/file_{index}.rs"),
                status: 'M',
                staged: false,
            })
            .collect();
        app.source_control_scroll = 4;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");

        terminal
            .draw(|frame| {
                draw_sc_changes_panel(frame, &mut app, Rect::new(0, 0, width, height));
            })
            .expect("draw source control changes");

        let scrollbar = column_symbols(&terminal, width - 1, 6, height);
        assert!(
            scrollbar.chars().all(|symbol| symbol == '▐'),
            "sleek scrollbar symbols: {scrollbar:?}"
        );
        let (track, thumb) = app.colors.ui.scrollbar_colors(app.colors.bg_sidebar, false, false);
        assert!((6..height).any(|y| {
            terminal
                .backend()
                .buffer()
                .cell((width - 1, y))
                .is_some_and(|cell| cell.fg == thumb)
        }));
        assert!((6..height).any(|y| {
            terminal
                .backend()
                .buffer()
                .cell((width - 1, y))
                .is_some_and(|cell| cell.fg == track)
        }));
    }

    #[test]
    fn changes_panel_clears_scroll_when_all_files_fit() {
        let width = 30;
        let height = 12;
        let mut app = App::test_new();
        app.source_control_files = (0..2)
            .map(|index| GitFileChange { directory: false,
                original_path: None,
                path: format!("src/file_{index}.rs"),
                status: 'M',
                staged: false,
            })
            .collect();
        app.source_control_scroll = 1;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");

        terminal
            .draw(|frame| {
                draw_sc_changes_panel(frame, &mut app, Rect::new(0, 0, width, height));
            })
            .expect("draw source control changes");

        assert_eq!(app.source_control_scroll, 0);
        let scrollbar = column_symbols(&terminal, width - 1, 6, height);
        assert!(!scrollbar.contains('▐'));
    }

    #[test]
    fn changes_scroll_limit_keeps_overflowing_list_bottom_filled() {
        let files = (0..10)
            .map(|index| GitFileChange { directory: false,
                original_path: None,
                path: format!("src/file_{index}.rs"),
                status: 'M',
                staged: index < 5,
            })
            .collect::<Vec<_>>();

        // A 12-row changes panel leaves six rows for the file list. At the
        // bottom, the Changes header plus its five files exactly fill it.
        assert_eq!(
            source_control_file_list_max_scroll(&files, false, false, 12),
            6
        );

        // The empty Changes header consumes a row after staging every file;
        // scrolling to the bottom must expose it after the last staged file.
        let mut staged = files;
        for file in &mut staged {
            file.staged = true;
        }
        let scroll = source_control_file_list_max_scroll(&staged, false, false, 12);
        assert_eq!(scroll, 6);
        let mut app = App::test_new();
        app.source_control_files = staged;
        app.source_control_scroll = scroll;
        let mut terminal = Terminal::new(TestBackend::new(36, 12)).expect("test terminal");
        terminal
            .draw(|frame| draw_sc_changes_panel(frame, &mut app, Rect::new(0, 0, 36, 12)))
            .expect("draw bottom of fully staged files");
        let bottom: String = (0..36)
            .map(|x| terminal.backend().buffer()[(x, 11)].symbol())
            .collect();
        assert!(bottom.contains("Changes (0)"));
    }

    #[test]
    fn graph_panel_draws_a_visible_scrollbar_when_commits_overflow() {
        let width = 30;
        let height = 8;
        let mut app = App::test_new();
        app.git_log = (0..20)
            .map(|index| GitCommitInfo {
                graph: String::new(),
                hash: format!("{index:07x}"),
                full_hash: format!("{index:040x}"),
                message: format!("Commit {index}"),
                author: "Author".to_string(),
                time_ago: "now".to_string(),
                branches: Vec::new(),
            })
            .collect();
        app.git_log_scroll = 4;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");

        terminal
            .draw(|frame| draw_sc_graph_panel(frame, &mut app, Rect::new(0, 0, width, height)))
            .expect("draw source control graph");

        let scrollbar = column_symbols(&terminal, width - 1, 1, height);
        assert!(
            scrollbar.chars().all(|symbol| symbol == '▐'),
            "sleek scrollbar symbols: {scrollbar:?}"
        );
        let (track, thumb) = app.colors.ui.scrollbar_colors(app.colors.bg_sidebar, false, false);
        assert!((1..height).any(|y| {
            terminal
                .backend()
                .buffer()
                .cell((width - 1, y))
                .is_some_and(|cell| cell.fg == thumb)
        }));
        assert!((1..height).any(|y| {
            terminal
                .backend()
                .buffer()
                .cell((width - 1, y))
                .is_some_and(|cell| cell.fg == track)
        }));
    }

    #[test]
    fn graph_distinguishes_loading_errors_and_empty_history() {
        let mut app = App::test_new();
        app.sc_graph_collapsed = false;
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
        for (loading, error, expected) in [
            (true, None, "Loading history"),
            (false, Some("Read failed".to_string()), "Load failed — click to retry"),
            (false, None, "No commit history"),
        ] {
            app.bg_git_log_running = loading;
            app.git_history.error = error;
            terminal.draw(|frame| {
                draw_sc_graph_panel(frame, &mut app, Rect::new(0, 0, 40, 8));
            }).unwrap();
            let text: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
            assert!(text.contains(expected), "{text}");
            if loading { assert!(!text.contains("No commit history")); }
        }
    }
}
