use super::{TextInputId, TextInputResult};
use crate::{
    app::App,
    ui_widgets::{DropdownItem, DropdownMenu},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Frame, layout::Rect};

const ACTIONS: [(&str, &str, char); 4] = [
    ("Cut", "Ctrl+X", 'x'),
    ("Copy", "Ctrl+C", 'c'),
    ("Paste", "Ctrl+V", 'v'),
    ("Select All", "Ctrl+A", 'a'),
];

pub(crate) struct ContextMenu {
    id: TextInputId,
    generation: u64,
    search_owner: Option<u64>,
    agent: u64,
    position: (u16, u16),
    selected: usize,
    scroll: usize,
    inner: Rect,
}

impl App {
    #[cfg(test)]
    pub(crate) fn text_input_context_menu_area(&self) -> Option<Rect> {
        self.text_input_context_menu.as_ref().map(|menu| menu.inner)
    }

    pub(super) fn open_text_input_context_menu(&mut self, id: TextInputId, column: u16, row: u16) {
        self.clear_drag_states();
        self.text_input_click_id = None;
        self.clear_tab_tooltip();
        self.dismiss_lsp_hover();
        self.reset_hover_states();
        self.context_menu_active = false;
        self.explorer_context_active = false;
        self.sc_context_active = false;
        self.tab_context_active = false;
        self.text_input_context_menu = Some(ContextMenu {
            id,
            generation: self.workspace_generation,
            search_owner: self.file_search_owner(),
            agent: self.chat.active_agent,
            position: (column, row.saturating_add(1)),
            selected: if self.text_input_has_selection(id) {
                0
            } else {
                2
            },
            scroll: 0,
            inner: Rect::default(),
        });
    }

    fn text_input_context_menu_is_current(&self) -> bool {
        self.text_input_context_menu.as_ref().is_some_and(|menu| {
            menu.generation == self.workspace_generation
                && (self.focused_text_input_id() == Some(menu.id)
                    || (menu.id == TextInputId::SourceControlCommit
                        && self.source_control_input_mode
                        && self.active_pane == crate::app::ActivePane::Explorer
                        && self.sidebar_view == crate::app::SidebarView::SourceControl
                        && crate::ui_modal::active(self).is_none()))
                && self.text_input_area(menu.id).is_some()
                && (!matches!(
                    menu.id,
                    TextInputId::SearchFind | TextInputId::SearchReplace
                ) || menu.search_owner == self.file_search_owner())
                && (menu.id != TextInputId::Chat || menu.agent == self.chat.active_agent)
        })
    }

    fn text_input_has_selection(&mut self, id: TextInputId) -> bool {
        let (text, cursor, anchor) = self.text_input_state_mut(id);
        Self::normalize_inline_cursor(text, cursor, anchor);
        Self::inline_selection_range(*cursor, *anchor).is_some()
    }

    fn text_input_context_action_disabled(&mut self, id: TextInputId, index: usize) -> bool {
        let read_only = match id {
            TextInputId::GitBranch => !self.git_branch_dialog.accepts_text(),
            TextInputId::SourceControlCommit => self.current_git_operation().is_some(),
            TextInputId::GitOperationTarget => self.git_operation_ui.pending,
            TextInputId::WorkspaceName => self.workspace_sets_saving(),
            _ => false,
        };
        match index {
            0 => read_only || !self.text_input_has_selection(id),
            1 => !self.text_input_has_selection(id),
            2 => read_only,
            3 => self.text_input_state_mut(id).0.is_empty(),
            _ => true,
        }
    }

    fn execute_text_input_context_action(&mut self, index: usize) {
        if !self.text_input_context_menu_is_current() {
            self.text_input_context_menu = None;
            return;
        }
        let id = self.text_input_context_menu.as_ref().unwrap().id;
        if self.text_input_context_action_disabled(id, index) {
            return;
        }
        self.text_input_context_menu = None;
        let TextInputResult { changed, .. } = self.edit_text_input(
            id,
            KeyCode::Char(ACTIONS[index].2),
            KeyModifiers::CONTROL,
            None,
        );
        if changed {
            self.text_input_changed(id);
        }
        self.cursor_visible = true;
        self.last_blink = std::time::Instant::now();
    }

    pub(crate) fn handle_text_input_context_menu_key(&mut self, key: KeyEvent) -> bool {
        if self.text_input_context_menu.is_none() {
            if key.code == KeyCode::F(10)
                && key.modifiers == KeyModifiers::SHIFT
                && let Some(id) = self.focused_text_input_id()
                && let Some(area) = self.text_input_area(id)
            {
                self.open_text_input_context_menu(id, area.x, area.y);
                return true;
            }
            return false;
        }
        if !self.text_input_context_menu_is_current() {
            self.text_input_context_menu = None;
            return true;
        }
        let menu = self.text_input_context_menu.as_mut().unwrap();
        match key.code {
            KeyCode::Up => menu.selected = menu.selected.saturating_sub(1),
            KeyCode::Down => menu.selected = (menu.selected + 1).min(ACTIONS.len() - 1),
            KeyCode::Home => menu.selected = 0,
            KeyCode::End => menu.selected = ACTIONS.len() - 1,
            KeyCode::Enter | KeyCode::Char(' ') => {
                let index = menu.selected;
                self.execute_text_input_context_action(index);
            }
            KeyCode::Char(c)
                if key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                if let Some(index) = ACTIONS
                    .iter()
                    .position(|(_, _, code)| c.eq_ignore_ascii_case(code))
                {
                    self.execute_text_input_context_action(index);
                } else {
                    self.text_input_context_menu = None;
                }
            }
            _ => self.text_input_context_menu = None,
        }
        true
    }

    pub(crate) fn handle_text_input_context_menu_mouse(&mut self, mouse: MouseEvent) -> bool {
        if self.text_input_context_menu.is_none() {
            return false;
        }
        self.mouse_hover_changed |= (self.mouse_col, self.mouse_row) != (mouse.column, mouse.row);
        self.mouse_col = mouse.column;
        self.mouse_row = mouse.row;
        if !self.text_input_context_menu_is_current() {
            self.text_input_context_menu = None;
            return true;
        }
        let menu = self.text_input_context_menu.as_mut().unwrap();
        let scrollbar = if ACTIONS.len() > usize::from(menu.inner.height) {
            2.min(menu.inner.width.saturating_sub(1))
        } else {
            0
        };
        let items = Rect {
            width: menu.inner.width.saturating_sub(scrollbar),
            ..menu.inner
        };
        let hit = items
            .contains((mouse.column, mouse.row).into())
            .then(|| menu.scroll + usize::from(mouse.row - menu.inner.y))
            .filter(|&index| index < ACTIONS.len());
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(index) = hit {
                    self.execute_text_input_context_action(index);
                } else if !Rect::new(
                    menu.inner.x.saturating_sub(1),
                    menu.inner.y.saturating_sub(1),
                    menu.inner.width.saturating_add(2),
                    menu.inner.height.saturating_add(2),
                )
                .contains((mouse.column, mouse.row).into())
                {
                    self.text_input_context_menu = None;
                }
            }
            MouseEventKind::Moved | MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(index) = hit {
                    menu.selected = index;
                }
            }
            MouseEventKind::ScrollUp => menu.selected = menu.selected.saturating_sub(1),
            MouseEventKind::ScrollDown => {
                menu.selected = (menu.selected + 1).min(ACTIONS.len() - 1)
            }
            MouseEventKind::Down(_) => {
                self.text_input_context_menu = None;
            }
            _ => {}
        }
        true
    }

    pub(crate) fn draw_text_input_context_menu(&mut self, frame: &mut Frame) {
        if self.text_input_context_menu.is_none() {
            return;
        }
        if !self.text_input_context_menu_is_current() {
            self.text_input_context_menu = None;
            return;
        }
        let id = self.text_input_context_menu.as_ref().unwrap().id;
        let items: Vec<_> = ACTIONS
            .iter()
            .enumerate()
            .map(|(index, (label, shortcut, _))| {
                DropdownItem::new(label)
                    .shortcut(shortcut)
                    .disabled(self.text_input_context_action_disabled(id, index))
            })
            .collect();
        let menu = self.text_input_context_menu.as_mut().unwrap();
        menu.inner = DropdownMenu::new(
            &items,
            crate::ui_widgets::themed_dropdown_colors(&self.colors),
        )
        .anchor(menu.position.0, menu.position.1)
        .selected(menu.selected)
        .mouse(self.mouse_row, self.mouse_col)
        .min_width(24)
        .render_scrolled(frame, &mut menu.scroll);
    }
}
