//! Ctrl-hover definition probes are independent of actual navigation requests.
use crate::app::{App, TabKind};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::path::PathBuf;

pub(crate) struct LinkHover {
    pub(crate) modifiers: KeyModifiers,
    target: Option<Target>,
    sent: bool,
}

impl Default for LinkHover {
    fn default() -> Self {
        Self {
            modifiers: KeyModifiers::NONE,
            target: None,
            sent: false,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Target {
    document: u64,
    version: u64,
    path: PathBuf,
    line: usize,
    start: usize,
    end: usize,
    document_link: bool,
}

/// Map the actual painted cells, including expanded tabs, wide characters,
/// wrapped continuations and folded lines. Blank cells are not source text.
pub(crate) fn editor_position_at(app: &mut App, col: u16, row: u16) -> Option<(usize, usize)> {
    let area = app.code_area?;
    if !area.contains((col, row).into()) {
        return None;
    }
    let editor = app.editor_mut()?;
    if editor.diff_display_rows.is_some() || editor.ai_review.active {
        return None;
    }
    let screen_row = usize::from(row - area.y);
    let screen_col = usize::from(col - area.x);
    let (line, start, end) = if editor.word_wrap {
        let row = editor.wrapped_row_at(
            editor.wrap_scroll_offset + screen_row,
            editor.effective_wrap_width(),
        )?;
        (row.line_idx, row.start_col, row.end_col)
    } else {
        let line = editor.visible_line_at(editor.scroll_offset, screen_row)?;
        (line, editor.horizontal_scroll, editor.line_len(line))
    };
    let (column, over_text) = editor.source_column_at_cell(line, start, end, screen_col);
    over_text.then_some((line, column))
}

impl App {
    pub(crate) fn update_link_hover_modifiers(&mut self, modifiers: KeyModifiers) -> bool {
        if self.lsp_link_hover.modifiers == modifiers {
            return false;
        }
        self.lsp_link_hover.modifiers = modifiers;
        if modifiers != KeyModifiers::CONTROL {
            self.clear_lsp_link_hover();
        }
        true
    }

    /// Modifier-only key reports must not execute commands or dismiss popups.
    pub(crate) fn update_link_hover_key(&mut self, key: KeyEvent) -> bool {
        use crossterm::event::ModifierKeyCode::{LeftControl, RightControl};
        let mut modifiers = key.modifiers;
        if matches!(key.code, KeyCode::Modifier(LeftControl | RightControl)) {
            modifiers.set(KeyModifiers::CONTROL, key.kind != KeyEventKind::Release);
        }
        self.update_link_hover_modifiers(modifiers)
    }

    pub(crate) fn clear_lsp_link_hover(&mut self) {
        self.lsp_link_hover.target = None;
        self.lsp_link_hover.sent = false;
        let id = self.lsp_manager.active_definition_link_id.take();
        self.lsp_manager.cancel_request_id(id);
        self.lsp_manager.definition_link_available = false;
    }

    fn link_hover_target(&mut self) -> Option<Target> {
        if self.lsp_link_hover.modifiers != KeyModifiers::CONTROL
            || !self.terminal_has_focus
            || self.context_menu_active
            || self.palette.active
            || crate::ui_modal::active(self).is_some()
            || crate::ui_quick_picker::active(self).is_some()
            || self.search_bar_area.is_some_and(|area| {
                self.search_active && area.contains((self.mouse_col, self.mouse_row).into())
            })
        {
            return None;
        }
        let tab = self.tabs.get(self.active_tab)?;
        if tab.kind != TabKind::File
            || tab.extension_view.is_some()
            || tab.editor.large_file_mode
            || tab.editor.bg_loading
            || tab.editor.blame.revision.is_some()
        {
            return None;
        }
        // Avoid decorating text beneath suggestion lists.
        if self.lsp_completion_active || self.vtcomplete_active {
            return None;
        }
        let (line, col) = editor_position_at(self, self.mouse_col, self.mouse_row)?;
        let editor = self.editor()?;
        let path = editor.file_path.as_ref()?;
        let link = self
            .lsp_document_links
            .iter()
            .find(|(row, start, end, _)| *row == line && col >= *start && col < *end);
        let (start, end) = if let Some((_, start, end, _)) = link {
            (*start, *end)
        } else {
            editor.word_boundaries_at(line, col)
        };
        if start == end {
            return None;
        }
        if link.is_none()
            && !self.lsp_manager.clients.iter().any(|client| {
                client.initialized
                    && !client.server_dead
                    && client.handles_file(path)
                    && client.is_file_open(path)
                    && client.supports_request("textDocument/definition")
            })
        {
            return None;
        }
        Some(Target {
            document: editor.navigation_id,
            version: editor.content_version,
            path: path.clone(),
            line,
            start,
            end,
            document_link: link.is_some(),
        })
    }

    /// Run before polling replies, so obsolete probes cannot decorate a new file.
    pub(crate) fn refresh_lsp_link_hover(&mut self) -> bool {
        let target = self.link_hover_target();
        let changed = self.lsp_link_hover.target != target;
        if changed {
            self.clear_lsp_link_hover();
            self.lsp_link_hover.target = target;
        }
        let Some(target) = self.lsp_link_hover.target.as_ref() else {
            return changed;
        };
        if target.document_link || self.lsp_link_hover.sent {
            return changed;
        }
        if self.lsp_manager.last_notified_version.get(&target.path) != Some(&target.version) {
            let rope = self.editor().unwrap().rope.clone();
            self.queue_lsp_document_sync(target.path.clone(), target.version, rope);
            return changed;
        }
        let col =
            crate::lsp_positions::to_utf16(&self.editor().unwrap().rope, target.line, target.start);
        if let Some(client) = self.lsp_manager.clients.iter_mut().find(|client| {
            client.initialized
                && !client.server_dead
                && client.handles_file(&target.path)
                && client.is_file_open(&target.path)
                && client.supports_request("textDocument/definition")
        }) {
            self.lsp_link_hover.sent = true;
            if let Ok(id) = client.request_definition(&target.path, target.line as u32, col) {
                self.lsp_manager.active_definition_link_id = Some(id);
            }
        }
        changed
    }

    pub(crate) fn lsp_link_hover_range(&self) -> Option<(usize, usize, usize)> {
        let target = self.lsp_link_hover.target.as_ref()?;
        let editor = self.editor()?;
        (self.lsp_link_hover.modifiers == KeyModifiers::CONTROL
            && self.terminal_has_focus
            && !self.rendering_inactive_editor
            && editor.navigation_id == target.document
            && editor.content_version == target.version
            && editor.file_path.as_ref() == Some(&target.path)
            && (target.document_link || self.lsp_manager.definition_link_available))
            .then_some((target.line, target.start, target.end))
    }
}

#[cfg(test)]
#[path = "app_lsp_links_tests.rs"]
mod tests;
