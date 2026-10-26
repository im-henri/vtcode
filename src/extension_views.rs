use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{Frame, layout::Rect};
use serde::Serialize;

use crate::app::{App, ExtensionViewTabRef, TabKind};

#[derive(Clone)]
pub(crate) struct PointerCapture {
    view: ExtensionViewTabRef,
    area: Rect,
}

impl App {
    pub(crate) fn route_custom_editor_shortcut(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> bool {
        if self.active_pane != crate::app::ActivePane::Editor
            || self.active_extension_view().is_none()
        {
            return false;
        }
        if let Some(command) = self.lookup_keybinding(modifiers, code) {
            let command = match command {
                "go_to_line" => "goto_line",
                other => other,
            };
            if self.dispatch_extension_editor_command(command) {
                return true;
            }
            return false;
        }
        if modifiers.contains(KeyModifiers::CONTROL)
            && matches!(
                code,
                KeyCode::Char(
                    's' | 'S'
                        | 'z'
                        | 'Z'
                        | 'y'
                        | 'Y'
                        | 'a'
                        | 'A'
                        | 'c'
                        | 'C'
                        | 'x'
                        | 'X'
                        | 'v'
                        | 'V'
                        | 'f'
                        | 'F'
                        | 'h'
                        | 'H'
                        | 'g'
                        | 'G'
                )
            )
        {
            return self.handle_active_extension_view_key(code, modifiers);
        }
        false
    }
    pub(crate) fn restore_pending_custom_documents(&mut self) {
        let pending = std::mem::take(&mut self.pending_custom_recoveries);
        for record in pending {
            let ready = self
                .extension_manager
                .index_by_id
                .get(&record.extension_id)
                .and_then(|index| self.extension_manager.records.get(*index))
                .is_some_and(|extension| {
                    extension.runtime.is_some()
                        && extension
                            .commands
                            .iter()
                            .any(|command| command.id == record.command_id)
                });
            if !ready {
                self.pending_custom_recoveries.push(record);
                continue;
            }
            self.run_extension_command_with_optional_context(
                &record.extension_id,
                &record.command_id,
                Some(&record.original_path),
            );
        }
    }

    pub(crate) fn queue_custom_document_actions(&mut self, mode: u8) -> usize {
        let mut seen = std::collections::HashSet::new();
        let mut views = Vec::new();
        for record in &self.extension_manager.records {
            for view in record.views.values() {
                if let Some(document) = view.frame.as_ref().and_then(|frame| frame.document)
                    && seen.insert((record.manifest.id.clone(), document))
                {
                    views.push((record.manifest.id.clone(), view.id.clone()));
                }
            }
        }
        let mut count = 0;
        for (extension, view) in views {
            let action = match mode {
                1 => crate::extension_handlers::DocumentWorkbenchAction::SaveForExit,
                2 => crate::extension_handlers::DocumentWorkbenchAction::KeepForExit,
                3 => crate::extension_handlers::DocumentWorkbenchAction::DiscardForExit,
                _ => crate::extension_handlers::DocumentWorkbenchAction::Save,
            };
            if self.extension_manager.queue_document_action(
                &extension,
                &view,
                action,
                self.extension_editor_context_snapshot(),
            ) {
                count += 1;
            }
        }
        count
    }

    pub(crate) fn start_custom_exit(&mut self, keep: bool) -> bool {
        self.custom_exit_started = true;
        self.custom_exit_failed = false;
        self.custom_exit_remaining = self.queue_custom_document_actions(if keep { 2 } else { 1 });
        self.custom_exit_remaining > 0
    }

    pub(crate) fn start_custom_discard_exit(&mut self) -> bool {
        self.custom_exit_started = true;
        self.custom_exit_failed = false;
        self.custom_exit_remaining = self.queue_custom_document_actions(3);
        self.custom_exit_remaining > 0
    }

    fn finalize_extension_view_event(&mut self, extension_id: &str) {
        if let Some(view_id) = self
            .extension_manager
            .take_requested_open_view(extension_id)
        {
            self.open_extension_view(extension_id, &view_id);
        }
        self.refresh_open_extension_view_tabs();
    }

    pub(crate) fn active_extension_view(&self) -> Option<&ExtensionViewTabRef> {
        let tab = self.tabs.get(self.active_tab)?;
        if tab.kind != TabKind::ExtensionView {
            return None;
        }
        tab.extension_view.as_ref()
    }

    fn try_handle_custom_extension_view_key(
        &mut self,
        view: &ExtensionViewTabRef,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Option<bool> {
        let payload = serde_json::to_string(&ExtensionViewKeyEvent::new(code, modifiers)).ok()?;
        if self.queue_rich_view_event(
            view,
            crate::extension_wasm::ViewEventHandlerKind::Key,
            payload.clone(),
        ) {
            return Some(true);
        }
        let result = self.extension_manager.invoke_view_key_handler(
            &view.extension_id,
            &view.view_id,
            &payload,
            Some(self.current_extension_editor_context()),
        );
        match result {
            Ok(Some(handled)) => {
                self.finalize_extension_view_event(&view.extension_id);
                Some(handled)
            }
            Ok(None) => None,
            Err(err) => {
                self.finalize_extension_view_event(&view.extension_id);
                self.notify(err);
                Some(true)
            }
        }
    }

    fn try_handle_custom_extension_view_mouse(
        &mut self,
        view: &ExtensionViewTabRef,
        kind: &'static str,
        col: u16,
        row: u16,
        modifiers: KeyModifiers,
    ) -> Option<bool> {
        let capture = self
            .extension_pointer_capture
            .as_ref()
            .filter(|c| c.view.extension_id == view.extension_id && c.view.view_id == view.view_id);
        let area = capture.map(|c| c.area).or(self.code_area)?;
        let inside = area.contains((col, row).into());
        if !inside && capture.is_none() {
            return Some(false);
        }
        let payload = serde_json::to_string(&ExtensionViewPointerEvent {
            kind,
            column: col,
            row,
            view_column: col.saturating_sub(area.x),
            view_row: row.saturating_sub(area.y),
            // Cancellation must not overwrite a newer resize in the guest.
            width: if kind == "cancel" { 0 } else { area.width },
            height: if kind == "cancel" { 0 } else { area.height },
            pointer_inside: inside && kind != "cancel",
            shift: modifiers.contains(KeyModifiers::SHIFT),
            control: modifiers.contains(KeyModifiers::CONTROL),
            alt: modifiers.contains(KeyModifiers::ALT),
        })
        .ok()?;
        if self.queue_rich_view_event(
            view,
            crate::extension_wasm::ViewEventHandlerKind::Mouse,
            payload.clone(),
        ) {
            return Some(true);
        }
        let result = self.extension_manager.invoke_view_mouse_handler(
            &view.extension_id,
            &view.view_id,
            &payload,
            Some(self.current_extension_editor_context()),
        );
        match result {
            Ok(Some(handled)) => {
                self.finalize_extension_view_event(&view.extension_id);
                Some(handled)
            }
            Ok(None) => None,
            Err(err) => {
                self.finalize_extension_view_event(&view.extension_id);
                self.notify(err);
                Some(true)
            }
        }
    }

    fn try_handle_custom_extension_view_scroll(
        &mut self,
        view: &ExtensionViewTabRef,
        col: u16,
        row: u16,
        direction: i32,
        modifiers: KeyModifiers,
        axis: &'static str,
    ) -> Option<bool> {
        let area = self.code_area?;
        if col < area.x || col >= area.x + area.width || row < area.y || row >= area.y + area.height
        {
            return Some(false);
        }
        let payload = serde_json::to_string(&ExtensionViewScrollEvent {
            direction,
            ctrl: modifiers.contains(KeyModifiers::CONTROL),
            control: modifiers.contains(KeyModifiers::CONTROL),
            shift: modifiers.contains(KeyModifiers::SHIFT),
            alt: modifiers.contains(KeyModifiers::ALT),
            axis,
            column: col,
            row,
            view_column: col.saturating_sub(area.x),
            view_row: row.saturating_sub(area.y),
            width: area.width,
            height: area.height,
        })
        .ok()?;
        if self.queue_rich_view_event(
            view,
            crate::extension_wasm::ViewEventHandlerKind::Scroll,
            payload.clone(),
        ) {
            return Some(true);
        }
        let result = self.extension_manager.invoke_view_scroll_handler(
            &view.extension_id,
            &view.view_id,
            &payload,
            Some(self.current_extension_editor_context()),
        );
        match result {
            Ok(Some(handled)) => {
                self.finalize_extension_view_event(&view.extension_id);
                Some(handled)
            }
            Ok(None) => None,
            Err(err) => {
                self.finalize_extension_view_event(&view.extension_id);
                self.notify(err);
                Some(true)
            }
        }
    }

    pub(crate) fn handle_active_extension_view_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> bool {
        // Workbench navigation remains available while a custom editor owns input.
        if matches!(code, KeyCode::F(1))
            || (modifiers.contains(KeyModifiers::CONTROL)
                && (matches!(
                    code,
                    KeyCode::Char('p' | 'P' | 'w' | 'W') | KeyCode::Tab | KeyCode::BackTab
                ) || (modifiers.contains(KeyModifiers::SHIFT)
                    && matches!(code, KeyCode::Char('s' | 'S')))))
        {
            return false;
        }
        let Some(view) = self.active_extension_view().cloned() else {
            return false;
        };
        if let Some(handled) = self.try_handle_custom_extension_view_key(&view, code, modifiers) {
            return handled;
        }
        false
    }

    fn queue_rich_view_event(
        &mut self,
        view: &ExtensionViewTabRef,
        kind: crate::extension_wasm::ViewEventHandlerKind,
        payload: String,
    ) -> bool {
        let context = self.extension_editor_context_snapshot();
        let needs_clipboard =
            serde_json::from_str::<serde_json::Value>(&payload).is_ok_and(|event| {
                (event["control"] == true
                    && event["text"]
                        .as_str()
                        .is_some_and(|text| text.eq_ignore_ascii_case("v")))
                    || (event["code"] == "command" && event["text"] == "paste")
            });
        let clipboard = if needs_clipboard {
            crate::clipboard::get_text()
                .unwrap_or_else(|_| self.clipboard_history.front().cloned().unwrap_or_default())
        } else {
            String::new()
        };
        self.extension_manager.queue_view_event(
            &view.extension_id,
            &view.view_id,
            kind,
            payload,
            context,
            clipboard,
        )
    }

    pub(crate) fn dispatch_extension_editor_command(&mut self, command: &str) -> bool {
        if !matches!(
            command,
            "save"
                | "undo"
                | "redo"
                | "copy"
                | "cut"
                | "paste"
                | "find"
                | "find_replace"
                | "goto_line"
                | "select_all"
                | "find_next"
                | "find_prev"
        ) {
            return false;
        }
        let Some(view) = self.active_extension_view().cloned() else {
            return false;
        };
        let payload = serde_json::json!({ "code": "command", "text": command }).to_string();
        self.queue_rich_view_event(
            &view,
            crate::extension_wasm::ViewEventHandlerKind::Key,
            payload,
        )
    }

    pub(crate) fn paste_into_extension_editor(&mut self, text: &str) -> bool {
        if self.custom_exit_remaining > 0 {
            return true;
        }
        let Some(view) = self.active_extension_view().cloned() else {
            return false;
        };
        self.queue_rich_view_event(
            &view,
            crate::extension_wasm::ViewEventHandlerKind::Key,
            serde_json::json!({"code": "paste", "text": text}).to_string(),
        )
    }

    pub(crate) fn queue_extension_document_close(&mut self, index: usize, save: bool) -> bool {
        let Some(view) = self
            .tabs
            .get(index)
            .and_then(|tab| tab.extension_view.clone())
        else {
            return false;
        };
        let action = if save {
            crate::extension_handlers::DocumentWorkbenchAction::SaveClose
        } else {
            crate::extension_handlers::DocumentWorkbenchAction::DiscardClose
        };
        let context = self.extension_editor_context_snapshot();
        if !self.extension_manager.queue_document_action(
            &view.extension_id,
            &view.view_id,
            action,
            context,
        ) {
            return false;
        }
        self.confirm_close_active = false;
        true
    }

    pub(crate) fn notify_extension_view_closed(&mut self, view: &ExtensionViewTabRef) {
        let matches = |tab: &crate::app::Tab| {
            tab.extension_view.as_ref().is_some_and(|open| {
                open.extension_id == view.extension_id && open.view_id == view.view_id
            })
        };
        if self.tabs.iter().any(matches)
            || self
                .groups
                .iter()
                .enumerate()
                .any(|(index, group)| index != self.active_group && group.tabs.iter().any(matches))
        {
            return;
        }
        self.queue_rich_view_event(
            view,
            crate::extension_wasm::ViewEventHandlerKind::Close,
            serde_json::json!({ "code": "closed" }).to_string(),
        );
    }

    pub(crate) fn close_extension_document_tabs(&mut self, extension_id: &str, view_id: &str) {
        let matches = |tab: &crate::app::Tab| {
            tab.extension_view
                .as_ref()
                .is_some_and(|view| view.extension_id == extension_id && view.view_id == view_id)
        };
        for index in (0..self.tabs.len()).rev() {
            if matches(&self.tabs[index]) {
                self.force_close_tab(index);
            }
        }
        let removed_inactive = self
            .groups
            .iter()
            .enumerate()
            .any(|(index, group)| index != self.active_group && group.tabs.iter().any(matches));
        for group in &mut self.groups {
            crate::tab_collection::retain(&mut group.tabs, &mut group.active_tab, &mut group.tab_mru_order,
                |_, tab| !matches(tab));
        }
        self.reconcile_navigation();
        if removed_inactive {
            self.notify_extension_view_closed(&ExtensionViewTabRef {
                extension_id: extension_id.into(),
                view_id: view_id.into(),
                title: String::new(),
            });
        }
    }

    pub(crate) fn handle_active_extension_view_mouse_drag(
        &mut self,
        col: u16,
        row: u16,
        modifiers: KeyModifiers,
    ) -> bool {
        let Some(view) = self.active_extension_view().cloned() else {
            return false;
        };
        self.try_handle_custom_extension_view_mouse(&view, "drag", col, row, modifiers)
            .unwrap_or(false)
    }

    /// Passive pointer inspection is opt-in, so existing mouse handlers retain
    /// their original down/drag/up contract. Leave clears guest hover state.
    pub(crate) fn handle_active_extension_view_hover(
        &mut self,
        col: u16,
        row: u16,
        previous: (u16, u16),
        modifiers: KeyModifiers,
    ) -> bool {
        let Some(view) = self.active_extension_view().cloned() else {
            return false;
        };
        let Some(area) = self.code_area else {
            return false;
        };
        if !self
            .extension_manager
            .get_view(&view.extension_id, &view.view_id)
            .is_some_and(|view| view.hover_handler.is_some() && view.frame.is_some())
        {
            return false;
        }
        let inside = area.contains((col, row).into());
        if !inside && !area.contains(previous.into()) {
            return false;
        }
        if (col, row) == previous {
            return inside;
        }
        self.queue_rich_view_event(
            &view,
            crate::extension_wasm::ViewEventHandlerKind::Hover,
            serde_json::json!({
                "kind": if inside { "move" } else { "leave" },
                "column": col, "row": row,
                "viewColumn": col.saturating_sub(area.x), "viewRow": row.saturating_sub(area.y),
                "width": area.width, "height": area.height,
                "shift": modifiers.contains(KeyModifiers::SHIFT),
                "control": modifiers.contains(KeyModifiers::CONTROL),
                "alt": modifiers.contains(KeyModifiers::ALT),
            })
            .to_string(),
        );
        inside
    }

    pub(crate) fn handle_active_extension_view_mouse_down(
        &mut self,
        col: u16,
        row: u16,
        modifiers: KeyModifiers,
    ) -> bool {
        let Some(view) = self.active_extension_view().cloned() else {
            return false;
        };
        if let Some(handled) =
            self.try_handle_custom_extension_view_mouse(&view, "down", col, row, modifiers)
        {
            if handled
                && self
                    .extension_manager
                    .get_view(&view.extension_id, &view.view_id)
                    .is_some_and(|v| v.pointer_capture && v.frame.is_some())
                && let Some(area) = self.code_area
            {
                self.extension_pointer_capture = Some(PointerCapture { view, area });
            }
            return handled;
        }
        false
    }

    /// Right-click menus are opt-in and keep the ordinary mouse contract intact.
    pub(crate) fn handle_active_extension_view_context_menu(
        &mut self,
        col: u16,
        row: u16,
        modifiers: KeyModifiers,
    ) -> bool {
        let Some(view) = self.active_extension_view().cloned() else {
            return false;
        };
        let Some(area) = self.code_area else {
            return false;
        };
        if !area.contains((col, row).into()) {
            return false;
        }
        self.queue_rich_view_event(
            &view,
            crate::extension_wasm::ViewEventHandlerKind::ContextMenu,
            serde_json::json!({
                "kind": "contextMenu", "button": "right",
                "column": col, "row": row,
                "viewColumn": col - area.x, "viewRow": row - area.y,
                "width": area.width, "height": area.height, "pointerInside": true,
                "shift": modifiers.contains(KeyModifiers::SHIFT),
                "control": modifiers.contains(KeyModifiers::CONTROL),
                "alt": modifiers.contains(KeyModifiers::ALT),
            })
            .to_string(),
        )
    }

    pub(crate) fn cancel_extension_pointer_capture(&mut self) {
        if let Some(capture) = self.extension_pointer_capture.clone() {
            self.try_handle_custom_extension_view_mouse(
                &capture.view,
                "cancel",
                capture.area.x,
                capture.area.y,
                KeyModifiers::NONE,
            );
            self.extension_pointer_capture = None;
            self.drag_start_pos = None;
            self.drag_has_moved = false;
        }
    }

    /// A rich view explicitly opts in; ordinary editor and legacy view gestures
    /// still follow workbench routing. Never transfer a held gesture to a new tab.
    pub(crate) fn handle_extension_pointer_capture(
        &mut self,
        mouse: crossterm::event::MouseEvent,
    ) -> bool {
        use crossterm::event::{MouseButton, MouseEventKind};
        if matches!(mouse.kind, MouseEventKind::Down(_)) {
            self.cancel_extension_pointer_capture();
            return false;
        }
        let kind = match mouse.kind {
            MouseEventKind::Drag(MouseButton::Left) => "drag",
            MouseEventKind::Up(MouseButton::Left) => "up",
            _ => return false,
        };
        let Some(capture) = self.extension_pointer_capture.clone() else {
            return false;
        };
        let same_view = self.active_extension_view().is_some_and(|v| {
            v.extension_id == capture.view.extension_id && v.view_id == capture.view.view_id
        });
        if !same_view
            || self.code_area != Some(capture.area)
            || crate::ui_modal::active(self).is_some()
            || self.change_dir_active
        {
            self.cancel_extension_pointer_capture();
            return true;
        }
        self.try_handle_custom_extension_view_mouse(
            &capture.view,
            kind,
            mouse.column,
            mouse.row,
            mouse.modifiers,
        );
        if kind == "up" {
            self.extension_pointer_capture = None;
            self.drag_start_pos = None;
            self.drag_has_moved = false;
        }
        true
    }

    pub(crate) fn handle_active_extension_view_mouse_up(
        &mut self,
        col: u16,
        row: u16,
        modifiers: KeyModifiers,
    ) -> bool {
        let Some(view) = self.active_extension_view().cloned() else {
            return false;
        };
        if let Some(handled) =
            self.try_handle_custom_extension_view_mouse(&view, "up", col, row, modifiers)
        {
            return handled;
        }
        false
    }

    /// Preserve modifiers and the physical wheel axis for rich editors. Keep
    /// `ctrl` in the payload for extensions written against the original API.
    pub(crate) fn handle_extension_view_scroll_input(
        &mut self,
        col: u16,
        row: u16,
        direction: i32,
        modifiers: KeyModifiers,
        axis: &'static str,
    ) -> bool {
        let Some(view) = self.active_extension_view().cloned() else {
            return false;
        };
        if let Some(handled) = self
            .try_handle_custom_extension_view_scroll(&view, col, row, direction, modifiers, axis)
        {
            return handled;
        }
        false
    }

    /// Dispatch a keyboard event to any overlay that has a key handler registered.
    /// Returns true if the event was consumed by an overlay.
    pub(crate) fn handle_overlay_key_event(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> bool {
        let overlays = self.extension_manager.overlay_targets();
        if overlays.is_empty() {
            return false;
        }
        // Try overlays in reverse order (topmost = last rendered = highest priority)
        for (ext_id, overlay) in overlays.iter().rev() {
            if !overlay.key_handler {
                continue;
            }
            let payload = match serde_json::to_string(&ExtensionViewKeyEvent::new(code, modifiers))
            {
                Ok(p) => p,
                Err(_) => continue,
            };
            let ext_id = ext_id.clone();
            let overlay_id = overlay.id.clone();
            let ctx = self.current_extension_editor_context();
            let result = self.extension_manager.invoke_overlay_key_handler(
                &ext_id,
                &overlay_id,
                &payload,
                Some(ctx),
            );
            match result {
                Ok(Some(true)) => {
                    self.finalize_extension_view_event(&ext_id);
                    return true;
                }
                Ok(Some(false)) | Ok(None) => {}
                Err(err) => {
                    self.finalize_extension_view_event(&ext_id);
                    self.notify(err);
                    return true;
                }
            }
        }
        false
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExtensionViewKeyEvent {
    code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    shift: bool,
    control: bool,
    alt: bool,
}

impl ExtensionViewKeyEvent {
    fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        let (code, text) = match code {
            KeyCode::Backspace => ("backspace", None),
            KeyCode::Enter => ("enter", None),
            KeyCode::Left => ("left", None),
            KeyCode::Right => ("right", None),
            KeyCode::Up => ("up", None),
            KeyCode::Down => ("down", None),
            KeyCode::Home => ("home", None),
            KeyCode::End => ("end", None),
            KeyCode::PageUp => ("pageUp", None),
            KeyCode::PageDown => ("pageDown", None),
            KeyCode::Tab => ("tab", None),
            KeyCode::BackTab => ("backTab", None),
            KeyCode::Delete => ("delete", None),
            KeyCode::Insert => ("insert", None),
            KeyCode::Esc => ("escape", None),
            KeyCode::Char(ch) => ("char", Some(ch.to_string())),
            KeyCode::F(number) => ("function", Some(number.to_string())),
            other => ("other", Some(format!("{other:?}"))),
        };
        Self {
            code,
            text,
            shift: modifiers.contains(KeyModifiers::SHIFT),
            control: modifiers.contains(KeyModifiers::CONTROL),
            alt: modifiers.contains(KeyModifiers::ALT),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExtensionViewPointerEvent {
    kind: &'static str,
    column: u16,
    row: u16,
    view_column: u16,
    view_row: u16,
    width: u16,
    height: u16,
    pointer_inside: bool,
    shift: bool,
    control: bool,
    alt: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExtensionViewScrollEvent {
    direction: i32,
    ctrl: bool,
    control: bool,
    shift: bool,
    alt: bool,
    axis: &'static str,
    column: u16,
    row: u16,
    view_column: u16,
    view_row: u16,
    width: u16,
    height: u16,
}

pub(crate) fn draw_active_view(f: &mut Frame, app: &mut App, area: Rect) -> bool {
    let Some(meta) = app.active_extension_view().cloned() else {
        return false;
    };
    let Some(frame) = app
        .extension_manager
        .get_view(&meta.extension_id, &meta.view_id)
        .and_then(|view| view.frame.clone())
    else {
        return false;
    };
    app.code_area = Some(area);
    app.gutter_area = None;
    app.gutter_width = 0;
    app.minimap_area = None;
    app.scrollbar_area = None;
    app.h_scrollbar_area = None;
    if frame.width != area.width || frame.height != area.height {
        let payload =
            serde_json::json!({ "width": area.width, "height": area.height, "kind": "resize" })
                .to_string();
        app.queue_rich_view_event(
            &meta,
            crate::extension_wasm::ViewEventHandlerKind::Resize,
            payload,
        );
    }
    crate::extension_surface::render_frame(f.buffer_mut(), area, &frame, &app.colors);
    if !app.rendering_inactive_editor
        && app.active_pane == crate::app::ActivePane::Editor
        && let Some((column, row)) = frame
            .cursor
            .filter(|(column, row)| *column < area.width && *row < area.height)
    {
        f.set_cursor_position((area.x + column, area.y + row));
    }
    true
}
