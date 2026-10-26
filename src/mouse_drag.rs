use crossterm::event::KeyModifiers;
use crate::app::{ActivePane, App, TabKind, TerminalDock};
use crate::explorer::FileExplorer;
use crate::mouse::find_tab_at_col;

#[cfg(test)]
#[path = "mouse_tab_columns_tests.rs"]
mod tab_column_tests;

/// Type alias for cursor placement: (line, cursor_col, selection_anchor).
type CursorPlacement = (usize, usize, Option<(usize, usize)>);

const MAX_CACHED_BLOCK_ROWS: usize = 8_192;

fn apply_cursor_list<T: Clone + PartialEq>(current: &mut Vec<T>, planned: &Vec<T>) {
    let common = current.len().min(planned.len());
    if current[..common] != planned[..common] {
        current.clone_from(planned);
    } else {
        // Vertical extension/shrink normally changes only the tail. Still
        // compare the complete common prefix after arbitrary public mutations.
        current.truncate(common);
        current.extend_from_slice(&planned[common..]);
    }
}

#[derive(PartialEq, Eq)]
struct BlockDragTarget {
    editor_id: u64,
    content_version: u64,
    wrap_revision: u64,
    start: (usize, usize, usize),
    end: (usize, usize),
    screen_col: usize,
    horizontal_scroll: usize,
    scroll_offset: usize,
    wrap_scroll_offset: usize,
    width: usize,
    tab_size: usize,
    wrapped: bool,
    layout_ready: bool,
}

impl BlockDragTarget {
    fn same_line_columns(&self, other: &Self) -> bool {
        !self.wrapped && !other.wrapped
            && self.editor_id == other.editor_id
            && self.content_version == other.content_version
            && self.wrap_revision == other.wrap_revision
            && self.start == other.start
            && self.screen_col == other.screen_col
            && self.horizontal_scroll == other.horizontal_scroll
            && self.tab_size == other.tab_size
    }
}

#[derive(Default)]
struct BlockDragProjection {
    main: Option<CursorPlacement>,
    cursors: Vec<(usize, usize)>,
    anchors: Vec<Option<(usize, usize)>>,
    has_selection: bool,
}

impl BlockDragProjection {
    fn clear(&mut self) {
        self.main = None;
        self.cursors.clear();
        self.anchors.clear();
        self.has_selection = false;
    }

    fn push(&mut self, line: usize, col: usize, anchor: Option<(usize, usize)>) {
        // Once one real span exists, empty rows are excluded from a block.
        // Before that point retain fallback carets for an all-empty block.
        if anchor.is_some() && !self.has_selection {
            self.clear();
            self.has_selection = true;
        } else if anchor.is_none() && self.has_selection {
            return;
        }
        if self.main.is_none() {
            self.main = Some((line, col, anchor));
        } else {
            self.cursors.push((line, col));
            self.anchors.push(anchor);
        }
    }

    fn apply(&self, editor: &mut crate::editor::Editor) {
        if let Some((line, col, anchor)) = self.main {
            editor.cursor_line = line;
            editor.cursor_col = col;
            editor.selection_anchor = anchor;
            editor.desired_col = None;
            // Public cursor state can change between identical pointer samples.
            // Compare the actual output rather than trusting event identity.
            apply_cursor_list(&mut editor.extra_cursors, &self.cursors);
            apply_cursor_list(&mut editor.extra_cursor_anchors, &self.anchors);
        }
    }

    fn update_line_endpoint(&mut self, editor: &crate::editor::Editor,
        previous: &BlockDragTarget, target: &BlockDragTarget) -> bool
    {
        if !previous.same_line_columns(target) || self.main.is_none()
            || target.start.0 >= editor.line_count()
        {
            return false;
        }
        let start = target.start.0;
        if start.min(previous.end.0) != start.min(target.end.0) {
            return false; // Moving the leading edge changes the primary cursor.
        }
        let old_last = start.max(previous.end.0);
        let last = start.max(target.end.0);
        if last < old_last {
            if self.main.is_some_and(|(line, _, _)| line > last) {
                // Removing the last real span revives fallback carets on all
                // remaining empty rows. Rebuild only for this transition.
                return false;
            }
            let keep = self.cursors.partition_point(|&(line, _)| line <= last);
            self.cursors.truncate(keep);
            self.anchors.truncate(keep);
        } else if last > old_last {
            let end_col = target.horizontal_scroll.saturating_add(target.screen_col);
            let min_col = target.start.2.min(end_col);
            let max_col = target.start.2.max(end_col);
            for line in old_last + 1..=last {
                let len = editor.line_len(line);
                let origin = target.horizontal_scroll;
                let col = editor.source_column_at_cell(line, origin, len, min_col.saturating_sub(origin)).0;
                let anchor = editor.source_column_at_cell(line, origin, len, max_col.saturating_sub(origin)).0;
                self.push(line, col, (anchor > col).then_some((line, anchor)));
            }
        }
        true
    }
}

/// Reuse the last gesture projection, without assuming it still owns cursors.
pub(crate) struct BlockDragCache {
    target: BlockDragTarget,
    rope: ropey::Rope,
    folds: Vec<(usize, usize)>,
    projection: BlockDragProjection,
}

fn rect_contains(area: ratatui::layout::Rect, col: u16, row: u16) -> bool {
    col >= area.x
        && col < area.x.saturating_add(area.width)
        && row >= area.y
        && row < area.y.saturating_add(area.height)
}

fn editor_drop_target(
    app: &App,
    col: u16,
    row: u16,
) -> Option<(usize, ratatui::layout::Rect)> {
    if let Some(area) = app.editor_area {
        if rect_contains(area, col, row) {
            return Some((app.active_group, area));
        }
    }
    app.groups.iter().enumerate().find_map(|(group_index, group)| {
        if group_index == app.active_group {
            return None;
        }
        let area = group.editor_area?;
        rect_contains(area, col, row).then_some((group_index, area))
    })
}

fn tab_drop_zone_at(
    area: ratatui::layout::Rect,
    col: u16,
    row: u16,
) -> Option<crate::app::TabDropZone> {
    let rel_x = f32::from(col.saturating_sub(area.x)) / f32::from(area.width.max(1));
    let rel_y = f32::from(row.saturating_sub(area.y)) / f32::from(area.height.max(1));
    if rel_x < 0.25 {
        Some(crate::app::TabDropZone::Left)
    } else if rel_x > 0.75 {
        Some(crate::app::TabDropZone::Right)
    } else if rel_y < 0.25 {
        Some(crate::app::TabDropZone::Top)
    } else if rel_y > 0.75 {
        Some(crate::app::TabDropZone::Bottom)
    } else {
        None
    }
}

/// Tab bars accept a tab into the group; editor edges create a split.
pub(crate) fn tab_drop_target_at(
    app: &App,
    col: u16,
    row: u16,
) -> Option<(usize, Option<crate::app::TabDropZone>)> {
    if app.tab_bar_area.is_some_and(|area| rect_contains(area, col, row)) {
        return Some((app.active_group, None));
    }
    for (index, group) in app.groups.iter().enumerate() {
        if index != app.active_group
            && group.tab_bar_area.is_some_and(|area| rect_contains(area, col, row))
        {
            return Some((index, None));
        }
    }
    let (group, area) = editor_drop_target(app, col, row)?;
    let source = app.dragging_tab_source_group.unwrap_or(app.active_group);
    let source_len = if source == app.active_group {
        app.tabs.len()
    } else {
        app.groups.get(source).map_or(0, |group| group.tabs.len())
    };
    let zone = if app.dragging_tab.is_some() && group == source && source_len == 1 {
        None
    } else {
        tab_drop_zone_at(area, col, row)
    };
    Some((group, zone))
}

pub(crate) fn update_tab_drop_target(app: &mut App, col: u16, row: u16) {
    let target = tab_drop_target_at(app, col, row);
    app.tab_drop_target_group = target.map(|(group, _)| group);
    app.tab_drop_zone = target.and_then(|(_, zone)| zone);
}

fn dragged_editor_tab(app: &App) -> Option<&crate::app::Tab> {
    let tab_index = app.dragging_tab?;
    let source_group = app.dragging_tab_source_group.unwrap_or(app.active_group);
    if source_group == app.active_group {
        app.tabs.get(tab_index)
    } else {
        app.groups
            .get(source_group)
            .and_then(|group| group.tabs.get(tab_index))
    }
}

fn dragged_editor_terminal_index(app: &App) -> Option<usize> {
    let tab = dragged_editor_tab(app)?;
    (tab.kind == TabKind::Terminal)
        .then_some(tab.terminal_idx)
        .flatten()
}

fn terminal_dock_target_at(app: &App, col: u16, row: u16) -> Option<TerminalDock> {
    let body = app.workbench_body_area?;
    if !rect_contains(body, col, row) {
        return None;
    }

    // Keep the right dock reachable while preserving editor split-drop targets.
    let edge = 3.min(body.height).min(body.width);
    if app
        .terminal_sidebar_area
        .is_some_and(|area| rect_contains(area, col, row))
        || col >= body.x.saturating_add(body.width.saturating_sub(edge))
    {
        return Some(TerminalDock::Right);
    }
    None
}

pub(crate) fn code_area_to_buffer_pos(
    ed: &mut crate::editor::Editor,
    code_area: ratatui::layout::Rect,
    col: u16,
    row: u16,
) -> (usize, usize) {
    let screen_row = row.saturating_sub(code_area.y) as usize;
    let screen_col = col.saturating_sub(code_area.x) as usize;
    if let Some(rows) = &ed.diff_display_rows {
        let line = rows.get(screen_row).copied().flatten()
            .or_else(|| rows.iter().skip(screen_row).copied().flatten().next())
            .or_else(|| rows.iter().take(screen_row).rev().copied().flatten().next())
            .unwrap_or(ed.cursor_line);
        let column = ed.source_column_at_cell(line, ed.horizontal_scroll, ed.line_len(line),
            screen_col.saturating_sub(ed.diff_text_inset)).0;
        return (line, column);
    }
    if ed.ai_review.active {
        let mapped = ed.ai_review.source_rows.get(screen_row).copied().flatten()
            .or_else(|| ed.ai_review.source_rows.iter().skip(screen_row).copied().flatten().next())
            .or_else(|| ed.ai_review.source_rows.iter().take(screen_row).rev().copied().flatten().next());
        if let Some(mapped) = mapped {
            let offset = if ed.ai_review.wrapped { mapped.start_col } else { ed.horizontal_scroll };
            let column = ed.source_column_at_cell(mapped.line_idx, offset, mapped.end_col, screen_col).0;
            return (mapped.line_idx, column);
        }
        return (ed.cursor_line, ed.cursor_col);
    }
    if ed.word_wrap {
        return ed.wrapped_position_for_screen(
            ed.wrap_scroll_offset,
            screen_row,
            screen_col,
            ed.effective_wrap_width(),
        );
    }
    let line = (screen_row < ed.viewport_height)
        .then(|| ed.visible_line_at(ed.scroll_offset, screen_row))
        .flatten()
        .unwrap_or_else(|| ed.line_count().saturating_sub(1));
    let col = ed.source_column_at_cell(line, ed.horizontal_scroll, ed.line_len(line), screen_col).0;
    (line, col)
}

/// Shared block/column selection drag logic (used by both middle-drag and Alt+Shift left-drag).
pub(crate) fn handle_block_drag(app: &mut App, col: u16, row: u16) {
    let (Some(start), Some(code_area)) = (app.middle_drag_start, app.code_area) else {
        return;
    };
    if code_area.is_empty() {
        return;
    }
    let mut previous = app.block_drag_cache.take();
    let Some(ed) = app.editor_mut() else {
        return;
    };
    // Like ordinary selection, advance one row per outside drag sample and
    // hit-test the viewport edge. An out-of-viewport row otherwise takes the
    // position lookup's EOF fallback and creates cursors through the whole file.
    if row < code_area.y {
        ed.scroll_up(1);
    } else if row >= code_area.bottom() {
        ed.scroll_down(1);
    }
    let col = col.clamp(code_area.x, code_area.right().saturating_sub(1));
    let row = row.clamp(code_area.y, code_area.bottom().saturating_sub(1));
    let end = code_area_to_buffer_pos(ed, code_area, col, row);
    let width = ed.effective_wrap_width();
    let screen_col = col.saturating_sub(code_area.x) as usize;
    let target = BlockDragTarget {
        editor_id: ed.navigation_id,
        content_version: ed.content_version,
        wrap_revision: ed.wrap_revision,
        start,
        end,
        screen_col,
        horizontal_scroll: ed.horizontal_scroll,
        scroll_offset: ed.scroll_offset,
        wrap_scroll_offset: ed.wrap_scroll_offset,
        width,
        tab_size: ed.tab_size,
        wrapped: ed.word_wrap,
        layout_ready: ed.wrap_layout_ready(width),
    };
    let valid_snapshot = previous.as_ref().is_some_and(|cache|
        cache.rope.is_instance(&ed.rope)
        && ed.folded_ranges.iter().map(|(&a, &b)| (a, b)).eq(cache.folds.iter().copied())
    );
    if let Some(cache) = &previous
        && valid_snapshot && cache.target == target
    {
        cache.projection.apply(ed);
        app.block_drag_cache = previous;
        return;
    }

    if let Some(cache) = &mut previous
        && valid_snapshot
        && cache.projection.update_line_endpoint(ed, &cache.target, &target)
        && cache.projection.cursors.len() < MAX_CACHED_BLOCK_ROWS
        && cache.projection.cursors.capacity() <= MAX_CACHED_BLOCK_ROWS
        && cache.projection.anchors.capacity() <= MAX_CACHED_BLOCK_ROWS
    {
        cache.target = target;
        cache.projection.apply(ed);
        app.block_drag_cache = previous;
        return;
    }

    // Keep the two output buffers across samples; no row or placement vectors.
    let mut projection = previous.as_mut()
        .map(|cache| std::mem::take(&mut cache.projection))
        .unwrap_or_default();
    projection.clear();
    if ed.word_wrap {
        let start_wrap = ed.wrapped_row_for_position(start.0, start.1, width);
        let end_wrap = ed.wrapped_row_for_position(end.0, end.1, width);
        let min_wrap = start_wrap.min(end_wrap);
        let max_wrap = start_wrap.max(end_wrap);
        let min_col = start.2.min(screen_col);
        let max_col = start.2.max(screen_col);
        ed.visit_wrapped_rows(min_wrap, max_wrap.saturating_sub(min_wrap) + 1, width, |ed, row| {
            let col = ed.source_column_at_cell(row.line_idx, row.start_col, row.end_col, min_col).0;
            let anchor = ed.source_column_at_cell(row.line_idx, row.start_col, row.end_col, max_col).0;
            projection.push(row.line_idx, col, (anchor > col).then_some((row.line_idx, anchor)));
        });
    } else {
        let min_line = start.0.min(end.0);
        let max_line = start.0.max(end.0).min(ed.line_count().saturating_sub(1));
        let end_col = ed.horizontal_scroll.saturating_add(screen_col);
        let min_col = start.2.min(end_col);
        let max_col = start.2.max(end_col);
        for line in min_line..=max_line {
            let len = ed.line_len(line);
            let origin = ed.horizontal_scroll;
            let col = ed.source_column_at_cell(line, origin, len, min_col.saturating_sub(origin)).0;
            let anchor = ed.source_column_at_cell(line, origin, len, max_col.saturating_sub(origin)).0;
            projection.push(line, col, (anchor > col).then_some((line, anchor)));
        }
    }
    if projection.cursors.len() >= MAX_CACHED_BLOCK_ROWS {
        // Very large blocks already require large editor cursor lists. Do not
        // retain a second full copy of those lists for the rest of the gesture.
        if let Some((line, col, anchor)) = projection.main {
            ed.cursor_line = line;
            ed.cursor_col = col;
            ed.selection_anchor = anchor;
            ed.desired_col = None;
            ed.extra_cursors = projection.cursors;
            ed.extra_cursor_anchors = projection.anchors;
        }
        return;
    }
    projection.apply(ed);
    if projection.cursors.capacity() > MAX_CACHED_BLOCK_ROWS
        || projection.anchors.capacity() > MAX_CACHED_BLOCK_ROWS
    {
        // Empty fallback rows may have grown these buffers before the first
        // real span cleared them. Drop that scratch without transferring its
        // oversized capacity into a small editor cursor list.
        return;
    }
    let mut folds = previous.map(|cache| cache.folds).unwrap_or_default();
    folds.clear();
    folds.extend(ed.folded_ranges.iter().map(|(&a, &b)| (a, b)));
    let rope = ed.rope.clone();
    app.block_drag_cache = Some(BlockDragCache { target, rope, folds, projection });
}

/// Handle left-button drag events (resize, selection, tab drag, etc.).
pub(crate) fn handle_left_drag(
    app: &mut App,
    col: u16,
    row: u16,
    prev_mouse_col: u16,
    prev_mouse_row: u16,
    modifiers: KeyModifiers,
) {
    if let Some((section, start_row, start_offset)) = app.explorer_scrollbar_drag {
        if app.show_explorer && app.sidebar_view == crate::app::SidebarView::Explorer
            && let Some(area) = app.explorer_area
            && let Some(bar) = crate::ui_chrome::explorer_scrollbar(app, area, section)
        {
            section.scroll_to(app, bar.drag_offset(bar.coordinate(col, row), start_row, start_offset));
        }
        return;
    }
    if app.diff_selection_mouse_drag(col, row) { return; }
    if app.dragging_settings_categories {
        app.drag_settings_category_scrollbar(row);
        return;
    }
    if let Some(drag) = app.settings_scrollbar_drag {
        if let Some(area) = app.settings_sidebar_area
            && let Some((_, track, thumb, max)) = crate::mouse::settings_scrollbar_metrics(app, area)
        {
            app.settings_editor_scroll = drag.offset_at(row, track.saturating_sub(thumb), max);
        }
        return;
    }
    // Drag to resize explorer
    if app.resizing_explorer {
        let new_width = if let Some(exp_area) = app.explorer_area {
            if app.sidebar_right {
                let right_edge = exp_area.x + exp_area.width;
                right_edge.saturating_sub(col).clamp(15, 80)
            } else {
                col.saturating_sub(exp_area.x).saturating_add(1).clamp(15, 80)
            }
        } else {
            col.clamp(15, 80)
        };
        app.explorer_width = new_width;
        return;
    }
    // Drag the internal side-by-side diff divider.
    if app.resizing_diff_split {
        if let Some(area) = app.diff_split_area {
            let divider_width = u16::from(area.width >= 3);
            let available = area.width.saturating_sub(divider_width);
            if available > 0 {
                let min_pane = if available >= 24 {
                    12
                } else {
                    u16::from(available >= 2)
                };
                let max_left = available.saturating_sub(min_pane);
                let desired = col.saturating_sub(area.x);
                let left = desired.clamp(min_pane.min(max_left), max_left.max(min_pane));
                app.diff_split_ratio = f32::from(left) / f32::from(available);
            }
        }
        app.mouse_col = col;
        app.mouse_row = row;
        return;
    }
    // Drag to resize split divider
    if app.resizing_split {
        if let Some((path, child_idx, direction, total_size)) = app.dragging_divider.as_ref() {
            if let Some(ref mut tree) = app.split_tree {
                let delta = match direction {
                    crate::app::SplitDirection::Horizontal => {
                        (f32::from(col) - f32::from(prev_mouse_col))
                            / f32::from(*total_size).max(1.0)
                    }
                    crate::app::SplitDirection::Vertical => {
                        (f32::from(row) - f32::from(prev_mouse_row))
                            / f32::from(*total_size).max(1.0)
                    }
                };
                if delta.abs() > 0.001 {
                    tree.adjust_ratio(path, *child_idx, delta);
                }
            }
        }
        app.mouse_col = col;
        app.mouse_row = row;
        return;
    }
    if !app.resizing_sc_graph {
        if let Some((start_col, start_row)) = app.sc_graph_click_start {
            if col != start_col || row != start_row {
                app.resizing_sc_graph = true;
                app.sc_graph_click_start = None;
            }
        }
    }
    // Drag to resize bottom panel
    if app.resizing_sc_graph {
        if app.sidebar_view == crate::app::SidebarView::SourceControl {
            if let Some(exp_area) = app.source_control_detail_area() {
                let total_h = exp_area.height;
                let max_graph = total_h.saturating_sub(8);
                if max_graph >= 2 {
                    let min_graph = 6u16.min(max_graph);
                    let desired_graph_h = exp_area
                        .y
                        .saturating_add(exp_area.height)
                        .saturating_sub(row)
                        .max(1);
                    app.sc_graph_height = desired_graph_h.clamp(min_graph, max_graph);
                    app.sc_graph_collapsed = false;
                }
            }
        }
        return;
    }
    // Drag to resize bottom panel
    if app.resizing_panel {
        // A press or a stationary drag must not change a maximized panel.
        if app.drag_start_pos == Some((col, row)) { return; }
        if let (Some(panel), Some(body)) = (app.panel_area, app.workbench_body_area) {
            let available = panel.bottom().saturating_sub(body.y);
            let requested = panel.bottom().saturating_sub(row.saturating_add(1));
            app.panel_chrome.maximized = false;
            app.panel_height = crate::ui::responsive_panel_height(available, requested);
            return;
        }
        // The rendered panel already reaches the bottom of the workbench.
        // Querying the OS console size for every pointer sample stalls drags.
        let term_h = app
            .panel_area
            .map(|area| {
                area.bottom()
                    .saturating_add(u16::from(!app.zen_mode && app.show_status_bar))
            })
            .unwrap_or(app.viewport_area.height);
        let max_panel = (term_h / 2).max(4);
        let new_height = term_h.saturating_sub(row).clamp(4, max_panel);
        app.panel_height = new_height;
        return;
    }
    if (app.dragging_tab.is_some()
        || app.dragging_terminal_tab.is_some()
        || app.dragging_explorer_path.is_some())
        && !app.drag_has_moved
    {
        let moved = app
            .drag_start_pos
            .is_none_or(|(sx, sy)| col != sx || row != sy);
        if moved {
            app.drag_has_moved = true;
        } else {
            return;
        }
    }
    // Docked terminals can move between the right side, bottom panel, and an
    // ordinary editor group without changing the underlying PTY.
    if app.dragging_terminal_tab.is_some() {
        if let Some(dock) = terminal_dock_target_at(app, col, row) {
            app.terminal_dock_drop_target = Some(dock);
            app.tab_drop_zone = None;
            app.tab_drop_target_group = None;
            return;
        }
        app.terminal_dock_drop_target = None;
        update_tab_drop_target(app, col, row);
        return;
    }
    // Drag tab to reorder
    if let Some(drag_from) = app.dragging_tab {
        if dragged_editor_tab(app).is_none() {
            app.dragging_tab = None;
            app.dragging_tab_source_group = None;
            app.tab_drop_zone = None;
            app.tab_drop_target_group = None;
            app.terminal_dock_drop_target = None;
            app.drag_start_pos = None;
            app.drag_has_moved = false;
            return;
        }
        if dragged_editor_terminal_index(app).is_some() {
            if let Some(dock) = terminal_dock_target_at(app, col, row) {
                app.terminal_dock_drop_target = Some(dock);
                app.tab_drop_zone = None;
                app.tab_drop_target_group = None;
                return;
            }
        }
        app.terminal_dock_drop_target = None;
        if let Some(tab_area) = app.tab_bar_area {
            if app.dragging_tab_source_group.unwrap_or(app.active_group) == app.active_group
                && rect_contains(tab_area, col, row)
            {
                app.tab_drop_zone = None;
                app.tab_drop_target_group = None;
                if let Some(drag_to) = find_tab_at_col(app, col, tab_area) {
                    if drag_from != drag_to {
                        app.tabs[drag_from].preview = false;
                        app.swap_tabs(drag_from, drag_to);
                        if app.active_tab == drag_from {
                            app.active_tab = drag_to;
                        } else if app.active_tab == drag_to {
                            app.active_tab = drag_from;
                        }
                        app.dragging_tab = Some(drag_to);
                    }
                }
                return;
            }
        }
        update_tab_drop_target(app, col, row);
        return;
    }
    // Drag explorer file into editor area or within explorer (file move)
    if app.dragging_explorer_path.is_some() {
        // Track drop target within explorer tree
        if let Some(exp_area) = app.explorer_area {
            if col >= exp_area.x && col < exp_area.x + exp_area.width
                && row >= exp_area.y && row < exp_area.y + exp_area.height
                && app.sidebar_view == crate::app::SidebarView::Explorer
            {
                let tree_area = crate::ui_chrome::explorer_tree_content_area(app, exp_area);
                let tree_start = tree_area.y;
                if tree_area.contains((col, row).into()) {
                    let inner_row = (row - tree_start) as usize;
                    let visible_height = tree_area.height as usize;
                    app.explorer_drop_target_idx = app.explorer.entry_at_row(inner_row, visible_height);
                } else {
                    app.explorer_drop_target_idx = None;
                }
                app.tab_drop_zone = None;
                app.tab_drop_target_group = None;
                return;
            }
        }
        app.explorer_drop_target_idx = None;

        update_tab_drop_target(app, col, row);
        return;
    }
    // Drag on vertical scrollbar (continue even outside scrollbar area)
    if app.dragging_v_scrollbar {
        if app.inline_diff_scrollbar_input(row, false, false) {
            return;
        }
        if app.markdown_preview_scrollbar_input(row, false) {
            return;
        }
        if let Some(scrollbar_area) = app.scrollbar_area {
            if let Some(ed) = app.editor_mut() {
                let sb_height = scrollbar_area.height as usize;
                if ed.word_wrap {
                    let total_rows = ed.wrapped_total_rows(ed.effective_wrap_width());
                    if sb_height > 0 && total_rows > ed.viewport_height {
                        let click_row = row
                            .max(scrollbar_area.y)
                            .min(scrollbar_area.y + scrollbar_area.height.saturating_sub(1));
                        let rel_row = (click_row.saturating_sub(scrollbar_area.y)) as usize;
                        let max_scroll = total_rows.saturating_sub(ed.viewport_height.max(1));
                        let target = (rel_row * max_scroll) / sb_height;
                        ed.set_wrap_scroll_offset(target.min(max_scroll));
                    }
                } else {
                    let total_lines = ed.line_count();
                    if sb_height > 0 && total_lines > ed.viewport_height {
                        let click_row = row.max(scrollbar_area.y).min(scrollbar_area.y + scrollbar_area.height.saturating_sub(1));
                        let rel_row = (click_row.saturating_sub(scrollbar_area.y)) as usize;
                        let max_scroll = total_lines.saturating_sub(ed.viewport_height);
                        ed.scroll_offset = (rel_row * max_scroll) / sb_height;
                        ed.scroll_offset = ed.scroll_offset.min(max_scroll);
                    }
                }
            }
        }
        return;
    }
    if app.markdown_preview_selection_drag(col, row) {
        return;
    }
    // Drag on the original diff pane's horizontal scrollbar.
    if app.dragging_diff_left_h_scrollbar {
        if let Some(h_area) = app.diff_left_h_scrollbar_area {
            let total_width = app.diff_render_cache.max_left_text_width;
            let visible_width = h_area.width as usize;
            if total_width > visible_width {
                let click_col = col
                    .max(h_area.x)
                    .min(h_area.right().saturating_sub(1));
                let relative = click_col.saturating_sub(h_area.x) as usize;
                let max_scroll = total_width.saturating_sub(visible_width);
                app.diff_left_horizontal_scroll =
                    relative.saturating_mul(max_scroll) / visible_width.max(1);
            }
        }
        return;
    }
    // Drag on horizontal scrollbar (continue even outside scrollbar area)
    if app.dragging_h_scrollbar {
        if app.inline_diff_scrollbar_input(col, true, false) {
            return;
        }
        if let Some(h_area) = app.h_scrollbar_area {
            let max_line_width = app.cached_max_line_width;
            if let Some(code_area) = app.code_area {
                let visible_width = code_area.width as usize;
                if max_line_width > visible_width {
                    let bar_width = h_area.width as usize;
                    let click_col = col.max(h_area.x).min(h_area.x + h_area.width.saturating_sub(1));
                    let rel_col = (click_col.saturating_sub(h_area.x)) as usize;
                    let max_scroll = max_line_width.saturating_sub(visible_width);
                    if let Some(ed) = app.editor_mut() {
                        if !ed.word_wrap {
                            ed.horizontal_scroll = (rel_col * max_scroll) / bar_width.max(1);
                            ed.horizontal_scroll = ed.horizontal_scroll.min(max_scroll);
                        }
                    }
                }
            }
        }
        return;
    }

    // Drag started on minimap keeps control of scroll until mouse up.
    if app.dragging_minimap {
        let drag_start = app.minimap_drag_start;
        if let Some(minimap_area) = app.minimap_area {
            if let Some(ed) = app.editor_mut() {
                let total_lines = ed.line_count();
                let minimap_height = minimap_area.height as usize;
                if minimap_height > 0 && total_lines > 0 {
                    let clamped_row = row
                        .max(minimap_area.y)
                        .min(minimap_area.y + minimap_area.height.saturating_sub(1));
                    let rel_row = (clamped_row - minimap_area.y) as usize;
                    let (start_row, start_scroll) = drag_start.unwrap_or((rel_row, ed.scroll_offset));
                    let layout = crate::ui_editor_minimap::MinimapLayout::new(
                        total_lines,
                        start_scroll,
                        ed.viewport_height,
                        minimap_height,
                        ed.scroll_past_end,
                    );
                    ed.scroll_offset = layout.drag_scroll(start_row, rel_row);
                    if ed.word_wrap {
                        let wrap_row = ed.wrapped_row_for_position(
                            ed.scroll_offset,
                            0,
                            ed.effective_wrap_width(),
                        );
                        ed.set_wrap_scroll_offset(wrap_row);
                    }
                } else {
                    ed.scroll_offset = 0;
                    if ed.word_wrap {
                        ed.set_wrap_scroll_offset(0);
                    }
                }
            }
        }
        return;
    }
    // Drag to extend selection (skip if dragging text)
    if app.dragging_text {
        // Text drag-and-drop: just track mouse position, don't modify selection
        return;
    }
    // Gutter drag: extend line selection
    if app.dragging_gutter {
        if let Some(start_line) = app.dragging_gutter_start_line {
            if let Some(code_area) = app.code_area {
                let clamped_row = row.max(code_area.y).min(code_area.y + code_area.height.saturating_sub(1));
                // Auto-scroll when dragging beyond viewport
                if row < code_area.y {
                    if let Some(ed) = app.editor_mut() { ed.scroll_up(1); }
                } else if row >= code_area.y + code_area.height {
                    if let Some(ed) = app.editor_mut() { ed.scroll_down(1); }
                }
                if let Some(ed) = app.editor_mut() {
                    let (drag_line, _) =
                        code_area_to_buffer_pos(ed, code_area, code_area.x, clamped_row);
                    if drag_line >= start_line {
                        // Dragging down: anchor at start of start_line, cursor at end of drag_line
                        ed.selection_anchor = Some((start_line, 0));
                        let end_col = ed.line_len(drag_line);
                        ed.set_cursor_without_scrolling(drag_line, end_col);
                    } else {
                        // Dragging up: anchor at end of start_line, cursor at start of drag_line
                        let end_col = ed.line_len(start_line);
                        ed.selection_anchor = Some((start_line, end_col));
                        ed.set_cursor_without_scrolling(drag_line, 0);
                    }
                }
            }
        }
        return;
    }
    if app.active_pane == ActivePane::Editor {
        if app.handle_active_extension_view_mouse_drag(col, row, modifiers) { return; }
        // Alt+Shift+Drag: use block/column selection (same as middle drag)
        if app.middle_drag_start.is_some() {
            // Delegate to block selection logic
            handle_block_drag(app, col, row);
            return;
        }
        // Opening a search result can focus the editor mid-gesture. Only a
        // press on this document's text may extend its ordinary selection.
        let Some(document) = app.editor_selection_drag else { return; };
        if app.editor().is_none_or(|editor| editor.navigation_id != document) {
            return;
        }
        if let Some(code_area) = app.code_area {
            // Auto-scroll when dragging beyond viewport
            if row < code_area.y {
                if let Some(ed) = app.editor_mut() { ed.scroll_up(1); }
            } else if row >= code_area.y + code_area.height {
                if let Some(ed) = app.editor_mut() { ed.scroll_down(1); }
            }
            // Clamp mouse position into code_area so drag-select works
            // even when the mouse moves above or below the editor.
            let clamped_col = col.max(code_area.x).min(code_area.x + code_area.width.saturating_sub(1));
            let clamped_row = row.max(code_area.y).min(code_area.y + code_area.height.saturating_sub(1));
            let mc_anchor = app.multi_click_anchor;
            let cc = app.click_count;
            if let Some(ed) = app.editor_mut() {
                let (drag_line, drag_col) =
                    code_area_to_buffer_pos(ed, code_area, clamped_col, clamped_row);
                if let Some((anchor_line, anchor_start, _anchor_end_line, anchor_end)) = mc_anchor {
                    if cc >= 3 {
                        // Line-granularity drag
                        if drag_line >= anchor_line {
                            ed.selection_anchor = Some((anchor_line, 0));
                            let end_col = ed.line_len(drag_line);
                            ed.set_cursor_without_scrolling(drag_line, end_col);
                        } else {
                            let end_col = ed.line_len(anchor_line);
                            ed.selection_anchor = Some((anchor_line, end_col));
                            ed.set_cursor_without_scrolling(drag_line, 0);
                        }
                    } else {
                        // Keep whole words or whitespace runs selected during a double-click drag.
                        let (ws, we) = ed.whitespace_boundaries_at(drag_line, drag_col)
                            .unwrap_or_else(|| ed.word_boundaries_at(drag_line, drag_col));
                        if (drag_line, drag_col) >= (anchor_line, anchor_end) {
                            ed.selection_anchor = Some((anchor_line, anchor_start));
                            ed.set_cursor_without_scrolling(drag_line, we);
                        } else {
                            ed.selection_anchor = Some((anchor_line, anchor_end));
                            ed.set_cursor_without_scrolling(drag_line, ws);
                        }
                    }
                } else {
                    ed.set_cursor_without_scrolling(drag_line, drag_col);
                }
            }
        }
    }
}

/// Handle left-button mouse-up events (tab drop, text drop, cleanup).
pub(crate) fn handle_left_up(app: &mut App, col: u16, row: u16, modifiers: KeyModifiers) {
    if app.explorer_scrollbar_drag.take().is_some() {
        app.drag_start_pos = None;
        app.drag_has_moved = false;
        return;
    }
    if app.diff_selection_mouse_up(col, row) { return; }
    app.block_drag_cache = None;
    if let Some(view) = app.editor_mut().and_then(|ed| ed.markdown_preview_scroll.as_mut()) {
        view.selecting = false;
    }
    // A tab/file transfer belongs to the workbench even when released over a
    // rich view. Its mouse handler must not consume the drop or skip cleanup.
    let workbench_drop = app.dragging_tab.is_some()
        || app.dragging_terminal_tab.is_some()
        || app.dragging_explorer_path.is_some();
    if !workbench_drop && app.handle_active_extension_view_mouse_up(col, row, modifiers) {
        app.dragging_terminal_tab = None;
        app.terminal_dock_drop_target = None;
        app.drag_start_pos = None;
        app.drag_has_moved = false;
        return;
    }
    if app.sidebar_view == crate::app::SidebarView::SourceControl {
        if app.sc_graph_click_start.take().is_some() && !app.resizing_sc_graph {
            app.sc_graph_collapsed = !app.sc_graph_collapsed;
            if !app.sc_graph_collapsed && app.git_log.is_empty() {
                app.refresh_git_log_async();
            }
        }
    } else {
        app.sc_graph_click_start = None;
    }
    if app.drag_has_moved {
        // Mouse-up may arrive at a different position from the last drag event.
        // Resolve the destination before any transfer can compact group indices.
        let editor_target = tab_drop_target_at(app, col, row);
        app.terminal_dock_drop_target = if app.dragging_terminal_tab.is_some()
            || dragged_editor_terminal_index(app).is_some()
        {
            terminal_dock_target_at(app, col, row)
        } else {
            None
        };
        if let Some(dock) = app.terminal_dock_drop_target {
            let terminal_index = app
                .dragging_terminal_tab
                .or_else(|| dragged_editor_terminal_index(app));
            if let Some(index) = terminal_index {
                if app.move_terminal_to_dock(index, dock) {
                    // The editor tab may have been removed and split groups may
                    // have compacted, so suppress the ordinary tab-drop paths.
                    app.dragging_terminal_tab = None;
                    app.dragging_tab = None;
                    app.dragging_tab_source_group = None;
                    app.tab_drop_zone = None;
                    app.tab_drop_target_group = None;
                }
            }
        }
        // Move a terminal panel tab into the editor pane it was dropped over.
        if let Some(terminal_index) = app.dragging_terminal_tab {
            if let Some((target_group, zone)) = editor_target {
                if app.detach_terminal_to_group(terminal_index, target_group) {
                    if let Some(zone) = zone {
                        app.create_split_group_at(app.active_tab, zone);
                    }
                }
            }
        }
        // Center/tab-bar drops merge into the destination; edge drops split it.
        if let (Some(drag_idx), Some((target_group, zone))) = (app.dragging_tab, editor_target) {
            let source_group = app.dragging_tab_source_group.unwrap_or(app.active_group);
            // Keep the destination focused when removing the source's last tab
            // compacts group indices, including when it collapses the split.
            if target_group != app.active_group {
                app.switch_group(target_group);
            }
            if source_group == target_group {
                if let Some(zone) = zone {
                    app.create_split_group_at(drag_idx, zone);
                } else if app.tab_bar_area.is_some_and(|area| {
                    area.contains((col, row).into())
                        && crate::ui_chrome_tabs::TabBarHits::new(app, area)
                            .after_tabs.contains((col, row).into())
                }) {
                    app.move_tab_to_end(drag_idx);
                }
            } else if app.move_tab_between_groups(source_group, drag_idx, target_group) {
                let moved_idx = app.tabs.len() - 1;
                if let Some(zone) = zone {
                    app.create_split_group_at(moved_idx, zone);
                } else {
                    app.active_tab = moved_idx;
                    app.touch_tab_mru(moved_idx);
                    app.sync_diff_tab();
                    app.tab_scroll_manual_anchor = None;
                    app.ensure_active_tab_visible();
                }
                app.active_pane = ActivePane::Editor;
            }
        }
        // Handle explorer file drop within explorer tree (move file/folder)
        if let Some(ref drag_path) = app.dragging_explorer_path.clone() {
            if let Some(target_idx) = app.explorer_drop_target_idx {
                if target_idx < app.explorer.entries.len() {
                    let target_entry = &app.explorer.entries[target_idx];
                    // Determine destination directory
                    let dest_dir = FileExplorer::calc_drop_dest_dir(target_entry);
                    // Validate the move
                    if FileExplorer::is_valid_move(drag_path, &dest_dir) {
                        if let Some(file_name) = drag_path.file_name() {
                            let dest_path = dest_dir.join(file_name);
                            // Existence and collision checks run with the move
                            // itself on the filesystem worker after confirmation.
                            app.request_explorer_move(drag_path.clone(), dest_path);
                        }
                    }
                }
            }
            app.explorer_drop_target_idx = None;
        }
        // Handle explorer file drop onto editor views (center=open in view, edge=open+split)
        if let Some(path) = app.dragging_explorer_path.clone() {
            if let Some((gi, zone)) = editor_target {
                let is_file = app
                    .explorer
                    .entries
                    .iter()
                    .find(|entry| entry.path == path)
                    .is_some_and(|entry| !entry.is_dir);
                if is_file {
                    if gi != app.active_group {
                        app.switch_group(gi);
                    }
                    app.open_file(&path);
                    // Explorer mouse-down may already have opened this file in
                    // another pane. Opening it again focuses that existing tab,
                    // so move it to the drop destination before splitting.
                    if app.active_group != gi {
                        let source_group = app.active_group;
                        let tab_idx = app.active_tab;
                        app.switch_group(gi);
                        if app.move_tab_between_groups(source_group, tab_idx, gi) {
                            // Focusing the destination before moving keeps it
                            // selected even when the empty source is compacted.
                            app.active_tab = app.tabs.len() - 1;
                            app.touch_tab_mru(app.active_tab);
                            app.sync_diff_tab();
                            app.ensure_active_tab_visible();
                        }
                    }
                    if let Some(zone) = zone {
                        app.create_split_group_at(app.active_tab, zone);
                    }
                }
            }
        }
    }
    // Handle text drag-and-drop
    if app.dragging_text {
        let did_drag = app.dragging_text_start.is_some_and(|(sx, sy)| {
            (col as i32 - sx as i32).unsigned_abs() > 2 || (row as i32 - sy as i32).unsigned_abs() > 1
        });
        if did_drag {
            if let Some(code_area) = app.code_area {
                if col >= code_area.x && col < code_area.x + code_area.width
                    && row >= code_area.y && row < code_area.y + code_area.height
                {
                    let ctrl_copy = modifiers.contains(crossterm::event::KeyModifiers::CONTROL);
                    if let Some(ed) = app.editor_mut() {
                        let (drop_line, drop_col) =
                            code_area_to_buffer_pos(ed, code_area, col, row);
                        // Don't drop inside the selection itself
                        if !ed.selection_contains(drop_line, drop_col) {
                            if let Some(text) = ed.selected_text() {
                                ed.save_undo();
                                // Get selection char indices
                                if let Some((sel_start, sel_end, _, _)) = ed.clamped_selection_indices() {
                                    let dirty_start = ed.rope.char_to_line(sel_start).min(drop_line);
                                    let dirty_end = ed.rope.char_to_line(sel_end).max(drop_line);
                                    let drop_char = ed.rope.line_to_char(drop_line) + drop_col;
                                    if ctrl_copy {
                                        // Copy: insert at drop position
                                        ed.rope.insert(drop_char, &text);
                                    } else {
                                        // Move: remove selection, then insert at adjusted position
                                        let adjusted_drop = if drop_char > sel_start {
                                            drop_char - (sel_end - sel_start)
                                        } else {
                                            drop_char
                                        };
                                        ed.rope.remove(sel_start..sel_end);
                                        ed.rope.insert(adjusted_drop, &text);
                                        // Place cursor at end of dropped text
                                        let end_pos = adjusted_drop + text.len();
                                        let new_line = ed.rope.char_to_line(end_pos.min(ed.rope.len_chars().saturating_sub(1)));
                                        let line_start = ed.rope.line_to_char(new_line);
                                        ed.cursor_line = new_line;
                                        ed.cursor_col = end_pos - line_start;
                                    }
                                    ed.clear_selection();
                                    ed.mark_syntax_dirty_range(dirty_start, dirty_end);
                                    ed.content_version += 1;
                                    ed.modified = true;
                                }
                            }
                        }
                    }
                }
            }
        } else {
            // Was a click on selected text without drag - deselect and place cursor
            if let Some(code_area) = app.code_area {
                if let Some(ed) = app.editor_mut() {
                    let (clicked_line, clicked_col) =
                        code_area_to_buffer_pos(ed, code_area, col, row);
                    ed.extra_cursors.clear();
                    ed.extra_cursor_anchors.clear();
                    ed.clear_selection();
                    ed.set_cursor_without_scrolling(clicked_line, clicked_col);
                }
            }
        }
        app.dragging_text = false;
        app.dragging_text_start = None;
    }
    app.dragging_tab = None;
    app.dragging_tab_source_group = None;
    app.dragging_terminal_tab = None;
    app.dragging_explorer_path = None;
    app.drag_start_pos = None;
    app.drag_has_moved = false;
    app.explorer_drop_target_idx = None;
    app.tab_drop_zone = None;
    app.tab_drop_target_group = None;
    app.terminal_dock_drop_target = None;
    app.resizing_explorer = false;
    app.resizing_panel = false;
    app.resizing_sc_graph = false;
    app.sc_graph_click_start = None;
    app.resizing_split = false;
    app.resizing_diff_split = false;
    app.settings_scrollbar_drag = None;
    app.dragging_settings_categories = false;
    app.dragging_divider = None;
    app.dragging_v_scrollbar = false;
    app.dragging_h_scrollbar = false;
    app.dragging_diff_left_h_scrollbar = false;
    app.dragging_minimap = false;
    app.minimap_drag_start = None;
    app.dragging_gutter = false;
    app.dragging_gutter_start_line = None;
    app.middle_drag_start = None;
    // If cursor == anchor, clear selection (was just a click)
    if let Some(ed) = app.editor_mut() {
        if let Some((al, ac)) = ed.selection_anchor {
            if al == ed.cursor_line && ac == ed.cursor_col {
                ed.clear_selection();
            }
        }
    }
}

#[cfg(test)]
#[path = "pane_drag_tests.rs"]
mod pane_drag_tests;

#[cfg(test)]
#[path = "mouse_pointer_performance_tests.rs"]
mod pointer_performance_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, Tab};
    use crate::editor::Editor;
    use ratatui::layout::Rect;

    pub(super) fn tile_area_for_text(app: &App, text: &str) -> Rect {
        if app.tabs.iter().any(|tab| tab.editor.rope.to_string() == text) {
            return app.editor_area.expect("active tile was rendered");
        }
        app.groups.iter().enumerate()
            .find(|(index, group)| *index != app.active_group
                && group.tabs.iter().any(|tab| tab.editor.rope.to_string() == text))
            .and_then(|(_, group)| group.editor_area)
            .expect("inactive tile was rendered")
    }

    fn rendered_label_position(
        terminal: &ratatui::Terminal<ratatui::backend::TestBackend>,
        area: Rect,
        label: &str,
    ) -> (u16, u16) {
        let width = label.len() as u16;
        let buffer = terminal.backend().buffer();
        (area.y..area.bottom()).find_map(|row| {
            (area.x..area.right().saturating_sub(width)).find_map(|col| {
                let text: String = (col..col + width)
                    .map(|x| buffer.cell((x, row)).unwrap().symbol()).collect();
                (text == label).then_some((col, row))
            })
        }).expect("dragged file is visible")
    }

    fn mouse_at(app: &mut App, kind: crossterm::event::MouseEventKind, (column, row): (u16, u16)) {
        crate::mouse::handle_mouse(app, crossterm::event::MouseEvent {
            kind, column, row, modifiers: KeyModifiers::NONE,
        });
    }

    #[test]
    fn explorer_and_tab_drops_split_the_hovered_tile_after_opening_a_file() {
        use crate::app::{SplitDirection, TabDropZone};
        use crossterm::event::{MouseButton, MouseEventKind};
        use ratatui::{backend::TestBackend, Terminal};

        for (target, tab_drag, preview, zone) in (0..3).flat_map(|target| {
            [false, true].into_iter().flat_map(move |tab_drag| {
                [false, true].into_iter().map(move |preview| (target, tab_drag, preview))
            })
        }).flat_map(|(target, tab_drag, preview)| {
            [TabDropZone::Left, TabDropZone::Right, TabDropZone::Top, TabDropZone::Bottom]
                .into_iter().map(move |zone| (target, tab_drag, preview, zone))
        }) {
            let workspace = tempfile::tempdir().unwrap();
            let path = workspace.path().join("dragged.txt");
            std::fs::write(&path, "dragged").unwrap();
            let mut app = App::new(workspace.path().to_path_buf());
            app.explorer.refresh();
            app.set_preview_tabs_enabled(preview);
            app.show_open_editors = false;
            app.tabs = vec![
                Tab::new_file(Editor::test_with_text("left")),
                Tab::new_file(Editor::test_with_text("right")),
            ];
            app.create_split_group_dir(1, SplitDirection::Horizontal);
            if target == 2 {
                app.tabs.push(Tab::new_file(Editor::test_with_text("nested")));
                app.create_split_group_dir(1, SplitDirection::Vertical);
            }
            let source = if target == 0 { 1 } else { 0 };
            app.switch_group(source);
            let target_text = match target { 0 => "left", 1 => "right", _ => "nested" };
            let source_text = if source == 0 { "left" } else { "right" };
            let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
            terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
            let target_area = tile_area_for_text(&app, target_text);
            let source_area = tile_area_for_text(&app, source_text);
            let explorer_pos = rendered_label_position(&terminal, app.explorer_area.unwrap(), "dragged.txt");

            // Mouse-down opens the file before the drag starts. Render between
            // events, just as the interactive editor does.
            mouse_at(&mut app, MouseEventKind::Down(MouseButton::Left), explorer_pos);
            assert_eq!(app.dragging_explorer_path.as_ref(), Some(&path));
            assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&path));
            terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
            if tab_drag {
                mouse_at(&mut app, MouseEventKind::Up(MouseButton::Left), explorer_pos);
                let tab_pos = rendered_label_position(&terminal, app.tab_bar_area.unwrap(), "dragged.txt");
                mouse_at(&mut app, MouseEventKind::Down(MouseButton::Left), tab_pos);
                assert!(app.dragging_tab.is_some());
                terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
            }
            let drop_pos = super::pane_drag_tests::edge_position(target_area, zone);
            mouse_at(&mut app, MouseEventKind::Drag(MouseButton::Left), drop_pos);
            assert_eq!(app.tab_drop_target_group, Some(target));
            assert!(app.tab_drop_zone == Some(zone));
            terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
            mouse_at(&mut app, MouseEventKind::Up(MouseButton::Left), drop_pos);
            app.wait_for_background(std::time::Duration::from_secs(5), |app| {
                app.editor().is_some_and(|editor| !editor.bg_loading)
            });
            terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();

            assert_eq!(app.groups.len(), if target == 2 { 4 } else { 3 });
            assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&path));
            assert!(!app.tabs[app.active_tab].preview);
            let remaining_source = tile_area_for_text(&app, source_text);
            assert!(remaining_source.x.abs_diff(source_area.x) <= 1);
            assert!(remaining_source.y.abs_diff(source_area.y) <= 1);
            assert!(remaining_source.width.abs_diff(source_area.width) <= 1);
            assert!(remaining_source.height.abs_diff(source_area.height) <= 1);
            super::pane_drag_tests::assert_placement(
                tile_area_for_text(&app, "dragged"), tile_area_for_text(&app, target_text), zone,
            );
        }
    }

    #[test]
    fn explorer_drop_preserves_unsaved_content_when_the_source_pane_closes() {
        use crate::app::SplitDirection;
        use crossterm::event::{MouseButton, MouseEventKind};
        use ratatui::{backend::TestBackend, Terminal};

        for (target, split) in [(0, false), (1, false), (0, true), (1, true)] {
            let workspace = tempfile::tempdir().unwrap();
            let path = workspace.path().join("dragged.txt");
            std::fs::write(&path, "saved content").unwrap();
            let mut app = App::new(workspace.path().to_path_buf());
            app.explorer.refresh();
            app.show_open_editors = false;
            app.tabs = vec![
                Tab::new_file(Editor::test_with_text("left")),
                Tab::new_file(Editor::test_with_text("right")),
            ];
            app.create_split_group_dir(1, SplitDirection::Horizontal);
            app.switch_group(1 - target);
            let editor = app.editor_mut().unwrap();
            *editor = Editor::test_with_text("unsaved\nchanges");
            editor.file_path = Some(path.clone());
            editor.modified = true;
            editor.cursor_line = 1;
            editor.cursor_col = 3;
            let target_text = if target == 0 { "left" } else { "right" };
            let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
            terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
            let target_area = tile_area_for_text(&app, target_text);
            let explorer_pos = rendered_label_position(&terminal, app.explorer_area.unwrap(), "dragged.txt");
            let drop_pos = (
                target_area.x + target_area.width / 2,
                if split { target_area.bottom() - 2 } else { target_area.y + target_area.height / 2 },
            );

            for (kind, position) in [
                (MouseEventKind::Down(MouseButton::Left), explorer_pos),
                (MouseEventKind::Drag(MouseButton::Left), drop_pos),
                (MouseEventKind::Up(MouseButton::Left), drop_pos),
            ] {
                mouse_at(&mut app, kind, position);
                terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
            }

            let editor = app.editor().unwrap();
            assert_eq!(editor.file_path.as_ref(), Some(&path));
            assert_eq!(editor.rope.to_string(), "unsaved\nchanges");
            assert!(editor.modified);
            assert_eq!((editor.cursor_line, editor.cursor_col), (1, 3));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "saved content");
            assert_eq!(app.tabs.len() + app.groups.iter().map(|group| group.tabs.len()).sum::<usize>(), 2);
            if split {
                assert_eq!(app.groups.len(), 2);
                let top = tile_area_for_text(&app, target_text);
                let bottom = tile_area_for_text(&app, "unsaved\nchanges");
                assert_eq!((top.x, top.width), (bottom.x, bottom.width));
                assert!(bottom.y >= top.bottom());
            } else {
                assert!(!app.split_active());
                assert!(app.tabs.iter().any(|tab| tab.editor.rope.to_string() == target_text));
            }
        }
    }

    #[test]
    fn tab_drop_bottom_splits_the_hovered_tile_in_both_columns() {
        use crate::app::{SplitDirection, TabDropZone};
        use ratatui::{backend::TestBackend, Terminal};
        for target in [0, 1, 2] {
            let mut app = App::test_new();
            app.tabs = vec![
                Tab::new_file(Editor::test_with_text("left")),
                Tab::new_file(Editor::test_with_text("right")),
            ];
            app.create_split_group_dir(1, SplitDirection::Horizontal);
            if target == 2 {
                app.tabs.push(Tab::new_file(Editor::test_with_text("nested")));
                app.create_split_group_dir(1, SplitDirection::Vertical);
            }
            let source = if target == 0 { 1 } else { 0 };
            app.switch_group(source);
            app.tabs.push(Tab::new_file(Editor::test_with_text("dragged")));
            app.active_tab = app.tabs.len() - 1;
            let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
            terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
            let target_text = match target { 0 => "left", 1 => "right", _ => "nested" };
            let source_text = if source == 0 { "left" } else { "right" };
            let target_area = tile_area_for_text(&app, target_text);
            let source_area = tile_area_for_text(&app, source_text);
            let other_area = (target == 2).then(|| tile_area_for_text(&app, "right"));
            let col = target_area.x + target_area.width / 2;
            let row = target_area.bottom() - 2;
            app.dragging_tab = Some(app.active_tab);
            app.dragging_tab_source_group = Some(source);
            app.drag_has_moved = true;
            handle_left_drag(&mut app, col, row, source_area.x, source_area.y, KeyModifiers::NONE);
            assert_eq!(app.tab_drop_target_group, Some(target));
            assert!(app.tab_drop_zone == Some(TabDropZone::Bottom));
            handle_left_up(&mut app, col, row, KeyModifiers::NONE);
            terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
            assert_eq!(app.groups.len(), if target == 2 { 4 } else { 3 }, "target tile {target}");
            assert_eq!(app.editor().unwrap().rope.to_string(), "dragged");
            assert_eq!(tile_area_for_text(&app, source_text), source_area);
            if let Some(area) = other_area {
                let unchanged_sibling = tile_area_for_text(&app, "right");
                assert_eq!((unchanged_sibling.x, unchanged_sibling.y, unchanged_sibling.width), (area.x, area.y, area.width));
                // Flattening same-direction splits adds a divider cell; ratio
                // rounding can change the sibling's height by one row.
                assert!(unchanged_sibling.height.abs_diff(area.height) <= 1);
            }
            let top = tile_area_for_text(&app, target_text);
            let bottom = tile_area_for_text(&app, "dragged");
            assert_eq!((top.x, top.width), (target_area.x, target_area.width));
            assert_eq!((bottom.x, bottom.width), (target_area.x, target_area.width));
            assert!(bottom.y >= top.bottom());
        }
    }

    #[test]
    fn tab_drop_bottom_preserves_destination_when_source_group_closes() {
        use crate::app::SplitDirection;
        use ratatui::{backend::TestBackend, Terminal};
        for target in [0, 1] {
            let mut app = App::test_new();
            app.tabs = vec![
                Tab::new_file(Editor::test_with_text("left")),
                Tab::new_file(Editor::test_with_text("right")),
            ];
            app.create_split_group_dir(1, SplitDirection::Horizontal);
            let source = 1 - target;
            app.switch_group(source);
            let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
            terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
            let target_text = if target == 0 { "left" } else { "right" };
            let source_text = if source == 0 { "left" } else { "right" };
            let area = tile_area_for_text(&app, target_text);
            let col = area.x + area.width / 2;
            let row = area.bottom() - 2;
            app.dragging_tab = Some(0);
            app.dragging_tab_source_group = Some(source);
            app.drag_has_moved = true;
            handle_left_drag(&mut app, col, row, 0, 0, KeyModifiers::NONE);
            assert_eq!(app.tab_drop_target_group, Some(target));
            handle_left_up(&mut app, col, row, KeyModifiers::NONE);
            terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
            assert_eq!(app.groups.len(), 2);
            assert_eq!(app.editor().unwrap().rope.to_string(), source_text);
            let top = tile_area_for_text(&app, target_text);
            let bottom = tile_area_for_text(&app, source_text);
            assert_eq!((top.x, top.width), (bottom.x, bottom.width));
            assert!(bottom.y >= top.bottom());
            assert_eq!(app.tabs.len() + app.groups.iter().map(|group| group.tabs.len()).sum::<usize>(), 2);
        }
    }
    

    /// Create an App with an editor containing the given text and word_wrap off.
    fn app_with_content(text: &str) -> App {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text(text);
        editor.word_wrap = false;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app
    }

    /// A code area starting at (5, 2) so we can verify offset subtraction.
    fn code_rect() -> Rect {
        Rect::new(5, 2, 80, 24)
    }

    #[test]
    fn dragging_right_terminal_into_editor_keeps_the_problems_panel_visible() {
        let mut app = app_with_content("fn main() {}\n");
        app.show_terminal_sidebar = true;
        app.show_panel = true;
        app.panel_tab = 1;
        app.editor_area = Some(Rect::new(10, 2, 80, 30));
        app.dragging_terminal_tab = Some(0);
        app.drag_start_pos = Some((50, 35));

        handle_left_drag(&mut app, 50, 15, 50, 35, KeyModifiers::NONE);
        assert!(app.drag_has_moved);
        assert_eq!(app.tab_drop_target_group, Some(app.active_group));

        handle_left_up(&mut app, 50, 15, KeyModifiers::NONE);
        let terminal_tab = app.tabs.last().expect("terminal tab after drop");
        assert_eq!(terminal_tab.kind, crate::app::TabKind::Terminal);
        assert_eq!(terminal_tab.terminal_idx, Some(0));
        assert!(app.show_panel);
        assert!(app.dragging_terminal_tab.is_none());
    }

    #[test]
    fn dragging_terminal_to_bottom_edge_does_not_open_a_terminal_panel() {
        let mut app = app_with_content("fn main() {}\n");
        app.workbench_body_area = Some(Rect::new(10, 2, 80, 30));
        app.editor_area = app.workbench_body_area;
        app.dragging_terminal_tab = Some(0);
        app.drag_start_pos = Some((50, 15));

        handle_left_drag(&mut app, 50, 31, 50, 15, KeyModifiers::NONE);
        assert_eq!(app.terminal_dock_drop_target, None);

        handle_left_up(&mut app, 50, 31, KeyModifiers::NONE);
        assert!(app.terminal_is_in_editor(0));
        assert!(!app.show_panel);
    }

    #[test]
    fn dragging_editor_terminal_tab_back_to_right_removes_editor_view() {
        let mut app = app_with_content("fn main() {}\n");
        assert!(app.detach_terminal_to_group(0, app.active_group));
        let terminal_tab_index = app.active_tab;
        app.workbench_body_area = Some(Rect::new(10, 2, 80, 30));
        app.dragging_tab = Some(terminal_tab_index);
        app.dragging_tab_source_group = Some(app.active_group);
        app.drag_start_pos = Some((50, 15));

        handle_left_drag(&mut app, 89, 15, 50, 15, KeyModifiers::NONE);
        handle_left_up(&mut app, 89, 15, KeyModifiers::NONE);

        assert!(app.terminal_is_in_dock(0, TerminalDock::Right));
        assert!(app
            .tabs
            .iter()
            .all(|tab| tab.kind != crate::app::TabKind::Terminal));
        assert!(app.show_terminal_sidebar);
    }

    // ── code_area_to_buffer_pos: basic conversion ──────────────────────

    #[test]
    fn basic_position_first_line() {
        let mut app = app_with_content("hello\nworld\nfoo\n");
        let ed = app.editor_mut().unwrap();
        ed.viewport_height = 24;
        let area = code_rect();
        // Click at screen position (5, 2) → buffer (0, 0)
        let (line, col) = code_area_to_buffer_pos(ed, area, 5, 2);
        assert_eq!((line, col), (0, 0));
    }

    #[test]
    fn basic_position_second_line_mid_col() {
        let mut app = app_with_content("hello\nworld\nfoo\n");
        let ed = app.editor_mut().unwrap();
        ed.viewport_height = 24;
        let area = code_rect();
        // Click at row offset 1 (screen row 3), col offset 3 (screen col 8)
        // → buffer line 1, col 3
        let (line, col) = code_area_to_buffer_pos(ed, area, 8, 3);
        assert_eq!((line, col), (1, 3));
    }

    // ── clamping beyond end of line ────────────────────────────────────

    #[test]
    fn clamps_col_to_line_length() {
        let mut app = app_with_content("hi\nworld\n");
        let ed = app.editor_mut().unwrap();
        ed.viewport_height = 24;
        let area = code_rect();
        // Line 0 is "hi" (len 2). Click at col offset 50 → clamped to 2.
        let (line, col) = code_area_to_buffer_pos(ed, area, 55, 2);
        assert_eq!(line, 0);
        assert_eq!(col, 2); // clamped to line length
    }

    // ── clamping beyond last line ──────────────────────────────────────

    #[test]
    fn clamps_row_beyond_last_line() {
        let mut app = app_with_content("aaa\nbbb\n");
        let ed = app.editor_mut().unwrap();
        ed.viewport_height = 24;
        let area = code_rect();
        // Content has lines 0..2. Click at screen row far below (row offset 20)
        // should clamp to the last line index.
        let (line, _col) = code_area_to_buffer_pos(ed, area, 5, 22);
        assert!(line < ed.line_count(), "line should be within bounds");
    }

    // ── with horizontal scroll offset ──────────────────────────────────

    #[test]
    fn horizontal_scroll_offset_shifts_column() {
        let mut app = app_with_content("abcdefghijklmnop\nsecond\n");
        let ed = app.editor_mut().unwrap();
        ed.viewport_height = 24;
        ed.horizontal_scroll = 5;
        let area = code_rect();
        // Click at screen col offset 0 (screen col 5) with h-scroll 5 → buffer col 5
        let (line, col) = code_area_to_buffer_pos(ed, area, 5, 2);
        assert_eq!((line, col), (0, 5));
    }

    #[test]
    fn horizontal_scroll_with_col_offset() {
        let mut app = app_with_content("abcdefghijklmnop\nsecond\n");
        let ed = app.editor_mut().unwrap();
        ed.viewport_height = 24;
        ed.horizontal_scroll = 3;
        let area = code_rect();
        // Click at screen col offset 4 (screen col 9), h-scroll 3 → buffer col 7
        let (line, col) = code_area_to_buffer_pos(ed, area, 9, 2);
        assert_eq!((line, col), (0, 7));
    }

    // ── with scroll offset (editor scrolled down) ──────────────────────

    #[test]
    fn vertical_scroll_offset_shifts_line() {
        // 10 lines of content
        let text = (0..10).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
        let mut app = app_with_content(&text);
        let ed = app.editor_mut().unwrap();
        ed.viewport_height = 5;
        ed.scroll_offset = 4;
        let area = code_rect();
        // Click at screen row offset 0 → should map to buffer line 4
        let (line, _col) = code_area_to_buffer_pos(ed, area, 5, 2);
        assert_eq!(line, 4);
    }

    // ── single-line / empty edge cases ─────────────────────────────────

    #[test]
    fn single_line_no_trailing_newline() {
        let mut app = app_with_content("only");
        let ed = app.editor_mut().unwrap();
        ed.viewport_height = 24;
        let area = code_rect();
        // Click at col offset 2 → buffer (0, 2)
        let (line, col) = code_area_to_buffer_pos(ed, area, 7, 2);
        assert_eq!((line, col), (0, 2));
    }

    #[test]
    fn minimap_drag_scrubs_long_file_from_top_to_bottom() {
        let text = (0..10_000)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        for scroll_past_end in [false, true] {
            let mut app = app_with_content(&text);
            app.minimap_area = Some(Rect::new(90, 2, 10, 40));
            let ed = app.editor_mut().unwrap();
            ed.viewport_height = 30;
            ed.scroll_past_end = scroll_past_end;
            crate::mouse_selection::handle_editor_area_left_click(&mut app, 95, 2, KeyModifiers::NONE);
            assert!(app.dragging_minimap);
            assert_eq!(app.minimap_drag_start, Some((0, 0)));

            handle_left_drag(&mut app, 95, 41, 95, 40, KeyModifiers::NONE);
            let max_scroll = 10_000 - if scroll_past_end { 1 } else { 30 };
            assert_eq!(app.editor().unwrap().scroll_offset, max_scroll);

            handle_left_drag(&mut app, 95, 2, 95, 3, KeyModifiers::NONE);
            assert_eq!(app.editor().unwrap().scroll_offset, 0);
            assert_eq!(app.editor().unwrap().cursor_line, 0);

            handle_left_up(&mut app, 95, 2, KeyModifiers::NONE);
            assert!(!app.dragging_minimap);
            assert!(app.minimap_drag_start.is_none());
        }
    }

    #[test]
    fn minimap_grab_past_end_preserves_scroll() {
        for word_wrap in [false, true] {
            for total_lines in [10, 60] {
                let mut app = app_with_content(&vec!["line"; total_lines].join("\n"));
                app.minimap_area = Some(Rect::new(90, 2, 10, 12));
                let ed = app.editor_mut().unwrap();
                ed.viewport_height = 12;
                ed.word_wrap = word_wrap;
                ed.scroll_past_end = true;
                ed.scroll_down(total_lines);
                assert_eq!(ed.scroll_offset, total_lines - 1);

                crate::mouse_selection::handle_editor_area_left_click(
                    &mut app,
                    95,
                    12,
                    KeyModifiers::NONE,
                );
                assert_eq!(app.editor().unwrap().scroll_offset, total_lines - 1);
                handle_left_drag(&mut app, 95, 12, 95, 12, KeyModifiers::NONE);
                assert_eq!(app.editor().unwrap().scroll_offset, total_lines - 1);
                if word_wrap {
                    assert_eq!(app.editor().unwrap().wrap_scroll_offset, total_lines - 1);
                }

                handle_left_drag(&mut app, 95, 2, 95, 12, KeyModifiers::NONE);
                assert_eq!(app.editor().unwrap().scroll_offset, 0);
                handle_left_drag(&mut app, 95, 13, 95, 2, KeyModifiers::NONE);
                assert_eq!(app.editor().unwrap().scroll_offset, total_lines - 1);
                assert_eq!(app.editor().unwrap().cursor_line, 0);
            }
        }
    }

    #[test]
    fn minimap_grab_in_middle_does_not_jump_or_accumulate_drag_drift() {
        let mut app = app_with_content(&"line\n".repeat(9999));
        app.minimap_area = Some(Rect::new(90, 2, 10, 40));
        let ed = app.editor_mut().unwrap();
        ed.viewport_height = 40;
        ed.scroll_offset = 4980;
        ed.cursor_line = 4990;

        crate::mouse_selection::handle_editor_area_left_click(&mut app, 95, 22, KeyModifiers::NONE);
        assert_eq!(app.editor().unwrap().scroll_offset, 4980);
        handle_left_drag(&mut app, 95, 27, 95, 22, KeyModifiers::NONE);
        let after_drag = app.editor().unwrap().scroll_offset;
        assert!(after_drag > 4980);
        handle_left_drag(&mut app, 95, 27, 95, 27, KeyModifiers::NONE);
        assert_eq!(app.editor().unwrap().scroll_offset, after_drag);
        handle_left_drag(&mut app, 95, 22, 95, 27, KeyModifiers::NONE);
        assert_eq!(app.editor().unwrap().scroll_offset, 4980);
        assert_eq!(app.editor().unwrap().cursor_line, 4990);
    }
}
