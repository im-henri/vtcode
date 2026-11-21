//! Paint a displayed desk with its own layout while input stays on the
//! foreground desk. The caller paints the foreground hit regions last.
use crate::{
    app::App,
    tab_windows::{MAIN_WINDOW, WindowId},
};
use ratatui::Frame;

pub(crate) fn draw(frame: &mut Frame, app: &mut App, window: WindowId) {
    let Some(desk) = app.window_desk(window) else {
        return;
    };
    let foreground_desk = app.desks.active;
    let target_desk = desk.id;
    let mut projection = if target_desk == foreground_desk {
        None
    } else {
        let Some(layout) = desk.layout.clone() else {
            return;
        };
        if layout.active_group >= app.groups.len() {
            return;
        }
        Some(layout)
    };
    let foreground_viewport = std::mem::replace(&mut app.viewport_area, frame.area());
    let viewport = app.viewport;
    let active_pane = app.active_pane;
    if let Some(pane) = projection.as_ref().and_then(|layout| layout.active_pane) {
        app.active_pane = pane;
    }
    let pointer = (app.mouse_col, app.mouse_row);
    (app.mouse_col, app.mouse_row) = (u16::MAX, u16::MAX);
    let focused = std::mem::replace(&mut app.terminal_has_focus, false);
    let terminal_focus = std::mem::replace(&mut app.terminal_panel_focused, false);
    let cursor = std::mem::replace(&mut app.cursor_visible, false);
    let animation = std::mem::replace(&mut app.rendering_animation_frame, false);
    let inactive = std::mem::replace(&mut app.rendering_inactive_editor, true);
    let activity = app.activity_bar_selected;
    let settings = app.settings_editor_active;
    let search_focus = std::mem::replace(&mut app.global_search_active, false);
    let tab_scroll_anchor = app.tab_scroll_manual_anchor;
    let strip_host = app.desks.host;
    let terminal_surfaces = std::mem::take(&mut app.terminal_surfaces);
    if window == MAIN_WINDOW {
        std::mem::swap(
            &mut app.terminal_surfaces,
            &mut app.background_main_terminal_surfaces,
        );
        app.terminal_surfaces.clear();
    }
    let chat_placements = std::mem::take(&mut app.chat.layout.placements);
    let chat_surfaces = std::mem::take(&mut app.chat.layout.surfaces);
    let split_dividers = std::mem::take(&mut app.split_dividers);
    let split_layout = std::mem::take(&mut app.split_layout_cache);
    let panel = std::mem::take(&mut app.panel_chrome);
    app.panel_chrome.maximized = panel.maximized;
    let dock_handle = app.terminal_sidebar.handle;
    let source_control_tab_area = app.source_control_tab_area.take();

    app.rendering_background_window = true;
    let comparison = if let Some(layout) = &mut projection {
        app.save_active_group();
        layout.swap_presentation(app);
        app.desks.active = target_desk;
        app.load_group(app.active_group);
        app.sync_activity_bar_selection();
        // Only the focused desk schedules highlighting from input. Request
        // this desk's viewports too, or a theme change paints stale syntax.
        app.refresh_visible_highlights_async();
        app.begin_inactive_diff_view()
    } else {
        None
    };
    let diff_pending = comparison.as_ref().is_some_and(|view| view.pending);
    crate::ui_performance_details::preserve_foreground_layout(app, |app| {
        super::draw_workbench(frame, app, diff_pending);
    });
    app.rendering_background_window = false;

    if let Some(layout) = &mut projection {
        app.end_inactive_diff_view(comparison);
        app.save_active_group();
        // Background windows share the workbench's pane storage. Their cell
        // coordinates must never become hit targets in the foreground window.
        for group in app
            .groups
            .iter_mut()
            .filter(|group| group.desk == target_desk)
        {
            group.clear_hit_areas();
        }
        layout.swap_presentation(app);
        app.desks.active = foreground_desk;
        app.load_group(app.active_group);
    }
    app.viewport_area = foreground_viewport;
    app.viewport = viewport;
    app.active_pane = active_pane;
    (app.mouse_col, app.mouse_row) = pointer;
    app.terminal_has_focus = focused;
    app.terminal_panel_focused = terminal_focus;
    app.cursor_visible = cursor;
    app.rendering_animation_frame = animation;
    app.rendering_inactive_editor = inactive;
    app.activity_bar_selected = activity;
    app.settings_editor_active = settings;
    app.global_search_active = search_focus;
    app.tab_scroll_manual_anchor = tab_scroll_anchor;
    app.desks.host = strip_host;
    if window == MAIN_WINDOW {
        std::mem::swap(
            &mut app.terminal_surfaces,
            &mut app.background_main_terminal_surfaces,
        );
    }
    app.terminal_surfaces = terminal_surfaces;
    app.chat.layout.placements = chat_placements;
    app.chat.layout.surfaces = chat_surfaces;
    app.split_dividers = split_dividers;
    app.split_layout_cache = split_layout;
    app.panel_chrome = panel;
    app.terminal_sidebar.handle = dock_handle;
    app.source_control_tab_area = source_control_tab_area;
}
