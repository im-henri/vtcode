//! Pointer capture for chat and inline terminal scrollbar thumbs.
use super::*;

#[derive(Clone)]
pub(crate) struct ChatScrollbar {
    pub area: Rect,
    pub first: usize,
    pub total: usize,
    pub visible: usize,
    pub terminal: Option<String>,
    pub picker: Option<Picker>,
}
impl ChatScrollbar {
    pub fn thumb(&self) -> (usize, usize) {
        let height = self.area.height as usize;
        let size = (height * self.visible / self.total.max(1)).clamp(1, height.max(1));
        let max = self.total.saturating_sub(self.visible);
        let start = self.first.min(max) * height.saturating_sub(size) / max.max(1);
        (start, size)
    }
}
pub(crate) struct ScrollDrag {
    bar: ChatScrollbar,
    grab: usize,
}

impl App {
    fn drag_chat_scrollbar(&mut self, row: u16) {
        let Some(drag) = &self.chat.scroll_drag else {
            return;
        };
        let (_, size) = drag.bar.thumb();
        let travel = (drag.bar.area.height as usize).saturating_sub(size);
        let pos = (row.saturating_sub(drag.bar.area.y) as usize)
            .saturating_sub(drag.grab)
            .min(travel);
        let max = drag.bar.total.saturating_sub(drag.bar.visible);
        let first = if travel == 0 {
            0
        } else {
            (pos * max + travel / 2) / travel
        };
        if let Some(picker) = &drag.bar.picker {
            if self.chat.picker.as_ref() != Some(picker) { return; }
            let visible = drag.bar.visible.min(self.chat.choices.len()).max(1);
            let first = first.min(self.chat.choices.len().saturating_sub(visible));
            self.chat.picker_scroll = first;
            self.chat.picker_selected = self.chat.picker_selected.clamp(first,
                (first + visible).saturating_sub(1));
        } else if let Some(id) = &drag.bar.terminal {
            if let Some(view) = self.chat.connection.terminal_views.get_mut(id) {
                view.first = if first == max { None } else { Some(first) };
            }
            self.chat.follow = false;
        } else {
            self.chat.scroll = first.min(self.chat.max_scroll);
            self.chat.follow = self.chat.scroll == self.chat.max_scroll;
        }
    }
    pub(crate) fn handle_chat_scrollbar(&mut self, mouse: MouseEvent) -> bool {
        if self.chat.scroll_drag.is_some() {
            match mouse.kind {
                MouseEventKind::Drag(MouseButton::Left) => self.drag_chat_scrollbar(mouse.row),
                MouseEventKind::Up(MouseButton::Left) => {
                    self.drag_chat_scrollbar(mouse.row);
                    self.chat.scroll_drag = None;
                }
                _ => {}
            }
            return true;
        }
        if mouse.kind != MouseEventKind::Down(MouseButton::Left)
            || self.chat.files.popup.is_some()
        {
            return false;
        }
        let Some(bar) = self
            .chat
            .scrollbars
            .iter()
            .find(|b| b.picker.as_ref() == self.chat.picker.as_ref()
                && b.area.contains((mouse.column, mouse.row).into()))
            .cloned()
        else {
            return false;
        };
        let (start, size) = bar.thumb();
        let pos = mouse.row.saturating_sub(bar.area.y) as usize;
        let grab = if (start..start + size).contains(&pos) {
            pos - start
        } else {
            size / 2
        };
        self.chat.scroll_drag = Some(ScrollDrag { bar, grab });
        self.drag_chat_scrollbar(mouse.row);
        true
    }
}
