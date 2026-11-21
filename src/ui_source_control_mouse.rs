use crossterm::event::KeyModifiers;
use ratatui::layout::{Position, Rect};

use crate::{
    app::{App, SourceControlRepositoryState as State},
    source_control_rows::{Group, Row},
};

use super::{Panels, source_control_file_list_visible_rows, source_control_split_heights};

pub(crate) fn click_header(app: &mut App, col: u16, row: u16) -> bool {
    let point = Position::new(col, row);
    if !app
        .source_control_view_area()
        .is_some_and(|area| area.contains(point))
    {
        return false;
    }
    if app
        .source_control_placement_area
        .is_some_and(|area| area.contains(point))
    {
        let command = if app.detached_window_id().is_some() {
            "source_control_main_window"
        } else if app.source_control_tab_surface().is_some() {
            "source_control_new_window"
        } else {
            "source_control_open_editor"
        };
        app.focus_source_control_surface();
        app.execute_palette_command(command);
        return true;
    }
    let actions = app
        .source_control_actions_area
        .filter(|area| area.contains(point));
    let reopen = app
        .repositories
        .reopen_button
        .filter(|area| area.contains(point));
    let refresh = app.source_control_enabled
        && app
            .source_control_refresh_area
            .is_some_and(|area| area.contains(point));
    if actions.is_none() && reopen.is_none() && !refresh {
        return false;
    }
    app.focus_source_control_surface();
    app.source_control_input_mode = false;
    if let Some(button) = actions {
        app.open_source_control_action_menu(button.x, button.bottom());
        crate::mouse_overlay::capture_context_menu_button(app, button);
    } else if let Some(button) = reopen {
        app.context_menu_pos = (button.x, button.bottom());
        app.open_repository_picker();
        crate::mouse_overlay::capture_context_menu_button(app, button);
    } else {
        app.execute_git_command("git_refresh");
    }
    true
}

pub(crate) fn click_body(app: &mut App, col: u16, row: u16, modifiers: KeyModifiers) -> bool {
    let point = Position::new(col, row);
    let Some(surface) = app
        .source_control_view_area()
        .filter(|area| area.contains(point))
    else {
        return false;
    };
    app.focus_source_control_surface();
    if app.source_control_repository_state == State::NoFolder {
        if app.source_control_tab_area.is_some() {
            crate::ui_empty_folder::click_source_control_tab(app, surface, point);
        } else {
            crate::ui_empty_folder::click(app, col, row);
        }
        return true;
    }
    let hits = |area: Rect| area.contains(point);
    if app.source_control_toggle_area.is_some_and(hits) {
        app.toggle_workspace_source_control();
    } else if app.repositories.parent_button.is_some_and(hits) {
        if app.repositories.parent.is_some() {
            app.open_parent_repository();
        } else {
            app.reopen_source_control_repositories();
        }
    } else if let Some((_, root)) = app
        .repositories
        .closes
        .iter()
        .find(|(area, _)| hits(*area))
        .cloned()
    {
        app.close_source_control_repository(&root);
    } else if let Some((_, root)) = app
        .repositories
        .headers
        .iter()
        .find(|(area, _)| hits(*area))
        .cloned()
    {
        app.select_source_control_repository(&root);
    } else if app.source_control_repository_state != State::Available {
        if let Some((_, action)) = app
            .repositories
            .recovery_buttons
            .iter()
            .find(|(area, _)| hits(*area))
            .copied()
        {
            action.run(app);
        } else if app.source_control_repository_state == State::Missing
            && app.source_control_initialize_area.is_some_and(hits)
        {
            app.git_initialize_repository();
        }
    } else if let Some(panels) = Panels::visible(app) {
        if crate::ui_git_operation::click(app, col, row) {
            return true;
        }
        if hits(panels.changes) {
            click_changes(app, panels.changes, point, modifiers);
        } else if hits(panels.graph) {
            app.source_control_input_mode = false;
            app.sc_graph_focused = true;
            if row == panels.graph.y {
                app.sc_graph_click_start = Some((col, row));
            } else if !app.sc_graph_collapsed {
                let index = app.git_log_scroll + usize::from(row - panels.graph.y - 1);
                if index == app.git_log.len() && app.git_history.error.is_some() {
                    app.refresh_git_log_async();
                } else if index < app.git_log.len() {
                    app.git_log_selected = index;
                    app.open_source_control_commit_details(index);
                }
            }
        }
    }
    true
}

fn click_changes(app: &mut App, area: Rect, point: Position, modifiers: KeyModifiers) {
    let hits = |area: Rect| area.contains(point);
    let commit = app.source_control_commit_area.is_some_and(hits);
    let amend = app.source_control_amend_area.is_some_and(hits);
    if commit || amend {
        if !app.bg_git_action_running && !app.bg_git_remote_running {
            if amend {
                app.git_amend_commit();
            } else {
                app.git_commit();
            }
        }
    } else if app.source_control_stage_all_area.is_some_and(hits) {
        app.git_stage_changes(app.source_control_group_changes(Group::Unstaged));
    } else if app.source_control_untracked_stage_area.is_some_and(hits) {
        app.git_stage_changes(app.source_control_group_changes(Group::Untracked));
    } else if app.source_control_unstage_all_area.is_some_and(hits) {
        app.git_unstage_changes(app.source_control_group_changes(Group::Staged));
    } else if app.source_control_discard_all_area.is_some_and(hits) {
        app.request_source_control_discard(
            app.source_control_group_changes(Group::Unstaged),
            false,
        );
    } else if app.source_control_untracked_discard_area.is_some_and(hits) {
        app.request_source_control_discard(
            app.source_control_group_changes(Group::Untracked),
            false,
        );
    } else if !super::handle_file_action_click(app, point.x, point.y, modifiers) {
        let rel_y = point.y - area.y;
        if (1..=4).contains(&rel_y) && app.current_git_operation().is_none() {
            // The text-input handler owns the interior. Clicks on the frame
            // place the caret at the nearest wrapped input row.
            app.sc_graph_focused = false;
            app.source_control_input_mode = true;
            app.prepare_commit_input(usize::from(area.width.saturating_sub(2)), 2);
            let line = app.commit_input.scroll + usize::from(rel_y.saturating_sub(2).min(1));
            let column = usize::from(point.x.saturating_sub(area.x.saturating_add(1)));
            app.source_control_commit_cursor = app.commit_input.byte_at_column(line, column);
            app.source_control_commit_selection_anchor = None;
            app.break_commit_input_group();
        } else if rel_y >= 6 {
            let hit = app
                .source_control_layout()
                .rows(app.source_control_scroll)
                .nth(usize::from(rel_y - 6));
            app.sc_graph_focused = false;
            app.source_control_input_mode = false;
            let toggle = modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
            let range = modifiers.contains(KeyModifiers::SHIFT);
            match hit {
                Some(Row::Section(group)) => app.set_source_control_group_collapsed(
                    group,
                    !app.source_control_group_collapsed(group),
                ),
                Some(Row::File(index)) if toggle || range => {
                    app.select_source_control_file(index, toggle, range)
                }
                Some(Row::File(index)) => app.click_source_control_file(index, point.x, point.y),
                None => {}
            }
        }
    }
}

pub(crate) fn right_click(app: &mut App, col: u16, row: u16) -> bool {
    let point = Position::new(col, row);
    if !app
        .source_control_view_area()
        .is_some_and(|area| area.contains(point))
    {
        return false;
    }
    if app.source_control_repository_state == State::Available
        && let Some(panels) = Panels::visible(app)
        && panels.changes.contains(point)
        && let Some(list_row) = row.checked_sub(panels.changes.y.saturating_add(6))
        && let Some(Row::File(index)) = app
            .source_control_layout()
            .rows(app.source_control_scroll)
            .nth(usize::from(list_row))
        && let Some(change) = app.source_control_files.get(index).cloned()
    {
        app.focus_source_control_surface();
        app.preserve_source_control_selection(index);
        app.open_source_control_context_menu(col, row, change);
    }
    true
}

pub(crate) fn scroll(app: &mut App, col: u16, row: u16, down: bool) -> bool {
    let point = Position::new(col, row);
    if !app
        .source_control_view_area()
        .is_some_and(|area| area.contains(point))
    {
        return false;
    }
    let step = |current: usize, amount: usize, max: usize| {
        if down {
            current.saturating_add(amount)
        } else {
            current.saturating_sub(amount)
        }
        .min(max)
    };
    if app
        .repositories
        .headers
        .iter()
        .any(|(area, _)| area.contains(point))
    {
        let count = app
            .repositories
            .entries
            .iter()
            .filter(|repo| !app.repositories.closed.contains(&repo.root))
            .count();
        app.repositories.header_scroll = step(
            app.repositories.header_scroll,
            1,
            count.saturating_sub(app.repositories.headers.len()),
        );
    } else if app.source_control_repository_state == State::Available
        && let Some(panels) = Panels::visible(app)
    {
        if panels.graph.contains(point) && !app.sc_graph_collapsed {
            let max =
                (app.git_log.len() + usize::from(app.git_history_has_footer())).saturating_sub(1);
            app.git_log_scroll = step(app.git_log_scroll, app.scroll_speed, max);
        } else if panels.changes.contains(point) {
            let max = app
                .source_control_layout()
                .max_scroll(source_control_file_list_visible_rows(panels.changes.height));
            app.source_control_scroll = step(app.source_control_scroll, app.scroll_speed, max);
        }
    }
    true
}

pub(crate) fn drag_graph_header(app: &mut App, col: u16, row: u16) -> bool {
    if app.sc_graph_click_start.is_none() && !app.resizing_sc_graph {
        return false;
    }
    if let (Some(area), Some(panels)) = (app.source_control_detail_area(), Panels::visible(app))
        && !panels.side_by_side
        && !app.sc_graph_collapsed
        && (app.resizing_sc_graph || app.sc_graph_click_start != Some((col, row)))
    {
        app.sc_graph_click_start = None;
        app.resizing_sc_graph = true;
        app.sc_graph_height =
            source_control_split_heights(area.height, false, area.bottom().saturating_sub(row)).1;
    }
    true
}

pub(crate) fn release_graph_header(app: &mut App) {
    if app.sc_graph_click_start.take().is_some()
        && !app.resizing_sc_graph
        && app.source_control_view_area().is_some()
    {
        app.sc_graph_collapsed = !app.sc_graph_collapsed;
        if !app.sc_graph_collapsed && app.git_log.is_empty() {
            app.refresh_git_log_async();
        }
    }
}
