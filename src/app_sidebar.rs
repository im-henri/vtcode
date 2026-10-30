use crossterm::event::{MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::app::{ActivePane, App};

/// Hover visibility is transient; `show_explorer` remains the saved preference.
#[derive(Default)]
pub(crate) struct AutoHide {
    pub enabled: bool,
    pub edge: Option<Rect>,
    revealed: bool,
}

impl App {
    pub(crate) fn sidebar_is_visible(&self) -> bool {
        self.show_explorer
            && (!self.sidebar_auto_hide.enabled
                || (!self.zen_mode
                    && (self.sidebar_auto_hide.revealed
                        || (self.active_pane == ActivePane::Explorer
                            && !self.docked_terminal_has_focus()))))
    }

    pub(crate) fn set_sidebar_auto_hide(&mut self, enabled: bool) {
        if self.sidebar_auto_hide.enabled != enabled {
            self.sidebar_auto_hide.enabled = enabled;
            self.sidebar_auto_hide.revealed = false;
        }
    }

    pub(crate) fn reveal_sidebar(&mut self) {
        self.show_explorer = true;
        self.sidebar_auto_hide.revealed = true;
    }

    /// A reveal never changes keyboard focus. Freeze layout during gestures and
    /// popups, whose hit regions must remain stable until the interaction ends.
    pub(crate) fn update_sidebar_auto_hide(&mut self, mouse: MouseEvent) -> bool {
        if !self.sidebar_auto_hide.enabled
            || self.zen_mode
            || !self.show_explorer
            || !matches!(
                mouse.kind,
                MouseEventKind::Moved | MouseEventKind::Down(_) | MouseEventKind::Up(_)
            )
            || self.sidebar_interaction_active()
        {
            return false;
        }
        let visible = self.sidebar_is_visible();
        let position = (mouse.column, mouse.row).into();
        self.sidebar_auto_hide.revealed = self
            .sidebar_auto_hide
            .edge
            .is_some_and(|area| area.contains(position))
            || self
                .activity_bar_area
                .is_some_and(|area| area.contains(position))
            || (visible
                && self
                    .explorer_area
                    .is_some_and(|area| area.contains(position)));
        let changed = visible != self.sidebar_is_visible();
        if changed {
            self.reset_hover_states();
            self.outline_hovered = None;
            self.mouse_hover_changed = true;
        }
        changed
    }

    fn sidebar_interaction_active(&self) -> bool {
        crate::ui_modal::active(self).is_some()
            || self.palette.active
            || self.quick_open_active
            || self.change_dir_active
            || self.symbol_list_active
            || self.task_picker_active
            || self.git_branch_picker_active
            || self.menu_active
            || self.tab_context_active
            || self.explorer_context_active
            || self.sc_context_active
            || self.context_menu_active
            || self.settings_dropdown_open
            || self.rename.active
            || self.text_input_drag.is_some()
            || self.drag_start_pos.is_some()
            || self.middle_drag_start.is_some()
            || self.dragging_tab.is_some()
            || self.dragging_terminal_tab.is_some()
            || self.dragging_explorer_path.is_some()
            || self.dragging_text
            || self.resizing_explorer
            || self.search_bar_layout.resizing
            || self.explorer_scrollbar_drag.is_some()
            || self.resizing_split
            || self.resizing_diff_split
            || self.resizing_panel
            || self.resizing_sc_graph
            || self.chat.resizing
    }
}

#[cfg(test)]
#[path = "app_sidebar_tests.rs"]
mod tests;
