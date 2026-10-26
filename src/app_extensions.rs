use std::collections::HashMap;
use std::path::PathBuf;

use crate::app::{
    App, ExtensionPaletteAction, ExtensionViewTabRef, PaletteCommand, SidebarView, Tab, TabKind,
};
use crate::editor::Editor;
use crate::extensions::{
    EditorContext, EditorContextSnapshot, ExtensionRecordView, ExtensionState,
};
use ropey::Rope;

const EXTENSION_COMMAND_PREFIX: &str = "ext_cmd:";
const EXTENSION_DYNAMIC_ACTION_ID: &str = "ext_dynamic";

type ExtensionViewSnapshot = HashMap<
    (String, String),
    (String, String, Option<std::sync::Arc<crate::extension_surface::ViewFrame>>),
>;

impl App {
    pub(crate) fn current_extension_editor_context(&self) -> EditorContext {
        self.extension_editor_context_snapshot().into_context()
    }

    pub(crate) fn extension_editor_context_snapshot(&self) -> EditorContextSnapshot {
        let mut context = EditorContext {
            remote_workspace: self.remote_workspace.clone(),
            workspace_root: Some(self.explorer.root.to_string_lossy().into_owned()),
            modified_file_paths: self.tabs.iter().chain(self.groups.iter().flat_map(|group| group.tabs.iter()))
                .filter(|tab| tab.kind == TabKind::File && tab.editor.modified)
                .filter_map(|tab| tab.editor.file_path.clone()).collect(),
            ..EditorContext::default()
        };
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return EditorContextSnapshot { context };
        };
        context.cursor_line = Some(tab.editor.cursor_line);
        context.cursor_col = Some(tab.editor.cursor_col);
        context.selection_range = tab.editor.selection_range();
        context.active_modified = tab.kind == TabKind::File && tab.editor.modified;
        if matches!(tab.kind, TabKind::File | TabKind::ExtensionView) {
            context.active_file_path = tab
                .editor
                .file_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned());
        }
        if tab.kind == TabKind::File {
            context.active_buffer = Some(tab.editor.extension_buffer_snapshot());
        }
        EditorContextSnapshot { context }
    }

    pub(crate) fn run_extension_command_with_optional_context(
        &mut self,
        extension_id: &str,
        command_id: &str,
        context_path: Option<&std::path::Path>,
    ) -> bool {
        let editor_context = self.extension_editor_context_snapshot();
        match self.extension_manager.invoke_command_async(
            extension_id,
            command_id,
            &mut self.background,
            context_path.map(std::path::Path::to_path_buf),
            Some(editor_context),
        ) {
            Ok(()) => {
                self.notify("Running extension command...");
                true
            }
            Err(err) => {
                self.notify(err);
                false
            }
        }
    }

    pub(crate) fn initialize_extensions(&mut self) {
        let recovery_dir = crate::extension_recovery::directory(&self.extension_manager.settings_file);
        let sender = self.background.sender.clone();
        self.background.spawn("Restoring custom documents", move || {
            let _ = sender.send(crate::background::BackgroundResult::ExtensionRecoveries(crate::extension_recovery::read(&recovery_dir)));
        });
        self.clamp_extensions_selection();
        // Manifest reads and Wasm activation can be arbitrarily expensive.
        // Queue initial discovery just like subsequent rescans so the first
        // interactive frame does not wait for extensions.
        self.request_extension_scan();
    }

    pub(crate) fn request_extension_scan(&mut self) {
        self.extension_scan_generation = self.extension_scan_generation.wrapping_add(1);
        if self.bg_extension_scan_running {
            self.bg_extension_scan_pending = true;
            return;
        }
        self.bg_extension_scan_running = true;
        let roots = self.extension_manager.scan_roots();
        let sender = self.background.sender.clone();
        let pool = self.background.maintenance_sender();
        let initial = self.extension_manager.records.is_empty()
            .then(|| self.extension_manager.initial_scan_manager());
        let generation = self.extension_scan_generation;
        let activity = self.background.activity.start("Loading extensions", None);
        let queued = pool.send(Box::new(move || {
            let _activity = activity.running();
            let (discovered, notices) = crate::extension_manifest::discover_extensions(&roots);
            if let Some(mut manager) = initial {
                let notices = manager.sync_discovered(discovered, notices);
                let _ = sender.send(crate::background::BackgroundResult::ExtensionsInitialized {
                    generation, manager: Box::new(manager), notices,
                });
            } else {
                let _ = sender.send(crate::background::BackgroundResult::ExtensionsDiscovered {
                    generation, discovered, notices,
                });
            }
        }));
        if queued.is_err() {
            self.bg_extension_scan_running = false;
        }
    }

    pub(crate) fn apply_discovered_extensions(
        &mut self,
        discovered: Vec<crate::extension_manifest::DiscoveredExtension>,
        notices: Vec<String>,
    ) -> bool {
        // Applying discovery can reorder records or replace runtimes. Wait for
        // worker-owned Wasmtime stores to return, then rescan current state.
        if self.extension_manager.has_busy_runtimes() {
            self.bg_extension_scan_pending = true;
            return false;
        }
        if notices.is_empty() && !self.extension_manager.discovery_requires_sync(&discovered) {
            if self.bg_extension_scan_pending {
                self.bg_extension_scan_pending = false;
                self.request_extension_scan();
            }
            return false;
        }
        let selected_id = self.extension_manager.records.get(self.extensions_selected).map(|record| record.manifest.id.clone());
        let notices = self.extension_manager.sync_discovered(discovered, notices);
        if let Some(index) = self.extension_manager.records.iter().position(|record| Some(&record.manifest.id) == selected_id.as_ref()) {
            self.extensions_selected = index;
        }
        self.finish_extension_discovery(notices)
    }

    pub(crate) fn apply_initialized_extensions(
        &mut self, generation: u64, manager: Box<crate::extensions::ExtensionManager>, notices: Vec<String>,
    ) -> bool {
        self.bg_extension_scan_running = false;
        if generation != self.extension_scan_generation || !self.extension_manager.records.is_empty() {
            // Never replace runtimes installed by a newer workspace/operation.
            // Releasing compiled modules can be expensive too.
            let _ = self.background.maintenance_sender().send(Box::new(move || drop(manager)));
            self.resume_pending_extension_scan();
            return false;
        }
        let changed = !manager.records.is_empty() || !notices.is_empty();
        self.extension_manager = *manager;
        if changed { self.finish_extension_discovery(notices) }
        else { self.resume_pending_extension_scan(); false }
    }

    fn finish_extension_discovery(&mut self, notices: Vec<String>) -> bool {
        self.restore_pending_custom_documents();
        self.clamp_extensions_selection();
        self.refresh_palette_commands();
        self.rebuild_keybinding_settings_cache();
        self.refresh_open_extension_view_tabs();
        self.refresh_language_servers(false);
        if let Some(first) = notices
            .iter()
            .find(|notice| !notice.starts_with("Loaded extension:"))
        {
            self.notify(first.clone());
        }
        if self.bg_extension_scan_pending {
            self.bg_extension_scan_pending = false;
            self.request_extension_scan();
        }
        true
    }

    pub(crate) fn resume_pending_extension_scan(&mut self) {
        if self.bg_extension_scan_pending
            && !self.bg_extension_scan_running
            && !self.extension_manager.has_busy_runtimes()
        {
            self.bg_extension_scan_pending = false;
            self.request_extension_scan();
        }
    }

    pub(crate) fn refresh_palette_commands(&mut self) {
        let selected = self.palette.filtered.get(self.palette.selected)
            .filter(|&&index| index < self.palette.commands.len())
            .map(|&index| self.palette_recent_key_for_index(index));
        let syntax_names = self.highlighter.syntax_names();
        let mut commands = Self::build_command_list_with_themes(&self.loaded_themes, &syntax_names);
        if self.remote_workspace.is_some() {
            for (name, id) in [
                ("Remote: Show Connection Status", "remote_connection_status"),
                ("Remote: Show Connection Diagnostics", "remote_connection_diagnostics"),
                ("Remote: Copy Connection Diagnostics", "remote_connection_diagnostics_copy"),
                ("Remote: Capture Headless CPU (10 seconds)", "remote_capture_cpu"),
                ("Remote: Reconnect", "remote_reconnect"),
                ("Remote: Stop Reconnecting", "remote_reconnect_stop"),
            ] {
                commands.push(PaletteCommand { name: name.into(), shortcut: String::new(), id: id.into() });
            }
        }
        self.extension_palette_action_map.clear();
        let start_index = commands.len();
        let extension_commands = self.extension_palette_commands();
        for (offset, (command, action)) in extension_commands.into_iter().enumerate() {
            if let Some(action) = action {
                self.extension_palette_action_map
                    .insert(start_index + offset, action);
            }
            commands.push(command);
        }
        self.palette.commands = commands;
        if self.palette.active {
            self.filter_palette_commands();
            if let Some(selected) = selected {
                if let Some(position) = self.palette.filtered.iter()
                    .position(|&index| self.palette_recent_key_for_index(index) == selected)
                {
                    self.palette.selected = position;
                }
            }
            let visible = crate::ui_pickers::PALETTE_MAX_ITEMS as usize;
            if self.palette.selected < self.palette.scroll_offset {
                self.palette.scroll_offset = self.palette.selected;
            } else if self.palette.selected >= self.palette.scroll_offset + visible {
                self.palette.scroll_offset = self.palette.selected.saturating_sub(visible - 1);
            }
            self.preview_palette_theme();
        }
    }

    pub(crate) fn open_extensions_sidebar(&mut self) {
        self.show_explorer = true;
        self.sidebar_view = SidebarView::Extensions;
        self.activity_bar_selected = 4;
        self.active_pane = crate::app::ActivePane::Explorer;
        self.ext_sidebar.search_focused = true;
        Self::normalize_inline_cursor(
            &self.extensions_filter,
            &mut self.extensions_filter_cursor,
            &mut self.extensions_filter_selection_anchor,
        );
        self.clamp_extensions_selection();
    }

    pub(crate) fn extension_views(&self) -> Vec<ExtensionRecordView> {
        self.extension_manager.views()
    }

    /// Returns the set of extension IDs that have at least one view tab currently open.
    #[cfg(test)]
    pub(crate) fn open_extension_view_ids(&self) -> rustc_hash::FxHashSet<String> {
        let mut ids = rustc_hash::FxHashSet::default();
        for tab in &self.tabs {
            if let Some(ref meta) = tab.extension_view {
                ids.insert(meta.extension_id.clone());
            }
        }
        for group in &self.groups {
            for tab in &group.tabs {
                if let Some(ref meta) = tab.extension_view {
                    ids.insert(meta.extension_id.clone());
                }
            }
        }
        ids
    }

    pub(crate) fn extension_palette_commands(
        &self,
    ) -> Vec<(PaletteCommand, Option<ExtensionPaletteAction>)> {
        let mut commands = vec![
            (PaletteCommand { name: "Extensions: Install Extension from GitHub".into(), shortcut: String::new(),
                id: std::borrow::Cow::Borrowed("extensions_install") }, None),
            (
                PaletteCommand {
                    name: "Extensions: Show Extensions".to_string(),
                    shortcut: "Ctrl+Shift+X".to_string(),
                    id: std::borrow::Cow::Borrowed("extensions"),
                },
                None,
            ),
            (
                PaletteCommand {
                    name: "Extensions: Rescan Extensions".to_string(),
                    shortcut: String::new(),
                    id: std::borrow::Cow::Borrowed("extensions_rescan"),
                },
                None,
            ),
        ];
        if self.extension_manager.is_empty() {
            return commands;
        }

        commands.extend([
            (
                PaletteCommand {
                    name: "Extensions: Reload All Extensions".to_string(),
                    shortcut: String::new(),
                    id: std::borrow::Cow::Borrowed("extensions_reload_all"),
                },
                None,
            ),
            (
                PaletteCommand {
                    name: "Extensions: Reload Selected Extension".to_string(),
                    shortcut: String::new(),
                    id: std::borrow::Cow::Borrowed("extensions_reload_selected"),
                },
                None,
            ),
            (
                PaletteCommand {
                    name: "Extensions: Unload Selected Extension".to_string(),
                    shortcut: String::new(),
                    id: std::borrow::Cow::Borrowed("extensions_unload_selected"),
                },
                None,
            ),
            (
                PaletteCommand {
                    name: "Extensions: Load Selected Extension".to_string(),
                    shortcut: String::new(),
                    id: std::borrow::Cow::Borrowed("extensions_load_selected"),
                },
                None,
            ),
        ]);

        for extension in self.extension_views() {
            if extension.state != ExtensionState::Loaded {
                continue;
            }
            for command in extension.commands {
                commands.push((
                    PaletteCommand {
                        name: format!("{}: {}", extension.name, command.title),
                        shortcut: String::new(),
                        id: std::borrow::Cow::Borrowed(EXTENSION_DYNAMIC_ACTION_ID),
                    },
                    Some(ExtensionPaletteAction::Command {
                        extension_id: extension.id.clone(),
                        command_id: command.id,
                    }),
                ));
            }
            for view in extension.views.into_values() {
                commands.push((
                    PaletteCommand {
                        name: format!("{}: Open View: {}", extension.name, view.title),
                        shortcut: String::new(),
                        id: std::borrow::Cow::Borrowed(EXTENSION_DYNAMIC_ACTION_ID),
                    },
                    Some(ExtensionPaletteAction::View {
                        extension_id: extension.id.clone(),
                        view_id: view.id,
                    }),
                ));
            }
        }
        commands
    }

    pub(crate) fn execute_extension_palette_index(&mut self, index: usize) -> bool {
        let Some(action) = self.extension_palette_action_map.get(&index).cloned() else {
            return false;
        };
        match action {
            ExtensionPaletteAction::Command {
                extension_id,
                command_id,
            } => {
                self.run_extension_command_with_optional_context(&extension_id, &command_id, None);
                true
            }
            ExtensionPaletteAction::View {
                extension_id,
                view_id,
            } => {
                let opened = self.open_extension_view(&extension_id, &view_id);
                self.refresh_open_extension_view_tabs();
                opened
            }
        }
    }

    fn extension_view_tab_path(extension_id: &str, title: &str) -> PathBuf {
        PathBuf::from(format!("Extension View {extension_id} {title}"))
    }

    fn apply_extension_view_to_editor(
        editor: &mut Editor,
        extension_id: &str,
        title: &str,
        content: &str,
    ) {
        editor.rope = Rope::from_str(content);
        editor.saved_rope = Some(editor.rope.clone());
        editor.file_path = Some(Self::extension_view_tab_path(extension_id, title));
        editor.read_only = true;
        editor.word_wrap = true;
        editor.modified = false;
        editor.cursor_line = 0;
        editor.cursor_col = 0;
        editor.scroll_offset = 0;
        editor.wrap_scroll_offset = 0;
        editor.horizontal_scroll = 0;
        editor.selection_anchor = None;
        editor.mark_syntax_dirty_range(0, editor.rope.len_lines());
        editor.content_version = editor.content_version.saturating_add(1);
    }

    pub(crate) fn rope_equals_text(rope: &Rope, text: &str) -> bool {
        if rope.len_bytes() != text.len() {
            return false;
        }
        let mut offset = 0usize;
        for chunk in rope.chunks() {
            if !text[offset..].starts_with(chunk) {
                return false;
            }
            offset += chunk.len();
        }
        true
    }

    fn extension_view_snapshot(&self) -> ExtensionViewSnapshot {
        let mut views = HashMap::new();
        for extension in self.extension_views() {
            let extension_id = extension.id;
            for view in extension.views.into_values() {
                views.insert((extension_id.clone(), view.id), (view.title, view.content, view.frame));
            }
        }
        views
    }

    fn refresh_extension_view_tabs_in(
        tabs: &mut [Tab],
        view_snapshot: &ExtensionViewSnapshot,
    ) {
        for tab in tabs {
            if tab.kind != TabKind::ExtensionView {
                continue;
            }
            let Some(meta) = tab.extension_view.clone() else {
                continue;
            };
            let key = (meta.extension_id.clone(), meta.view_id.clone());
            if let Some((title, content, frame)) = view_snapshot.get(&key) {
                let needs_refresh =
                    meta.title != *title || !Self::rope_equals_text(&tab.editor.rope, content);
                if needs_refresh {
                    Self::apply_extension_view_to_editor(
                        &mut tab.editor,
                        &meta.extension_id,
                        title,
                        content,
                    );
                } else {
                    tab.editor.read_only = true;
                    tab.editor.word_wrap = true;
                }
                if let Some(frame) = frame {
                    tab.editor.modified = frame.dirty;
                    if let Some(path) = &frame.path { tab.editor.file_path = Some(PathBuf::from(path)); }
                }
                tab.extension_view = Some(ExtensionViewTabRef {
                    extension_id: meta.extension_id,
                    view_id: meta.view_id,
                    title: title.clone(),
                });
            } else {
                let unavailable = format!(
                    "This extension view is currently unavailable.\n\nExtension: {}\nView: {}\n\nReload the extension or open the view again.",
                    meta.extension_id, meta.view_id
                );
                if !Self::rope_equals_text(&tab.editor.rope, &unavailable) {
                    Self::apply_extension_view_to_editor(
                        &mut tab.editor,
                        &meta.extension_id,
                        &meta.title,
                        &unavailable,
                    );
                }
                tab.extension_view = Some(meta);
            }
        }
    }

    pub(crate) fn refresh_open_extension_view_tabs(&mut self) {
        let view_snapshot = self.extension_view_snapshot();
        Self::refresh_extension_view_tabs_in(&mut self.tabs, &view_snapshot);
        for group in &mut self.groups {
            Self::refresh_extension_view_tabs_in(&mut group.tabs, &view_snapshot);
        }
    }

    pub(crate) fn open_extension_view(&mut self, extension_id: &str, view_id: &str) -> bool {
        let Some(view) = self.extension_manager.get_view(extension_id, view_id) else {
            self.notify(format!("Unknown extension view: {extension_id}/{view_id}"));
            return true;
        };
        self.extension_manager
            .view_resize_requests
            .remove(&(extension_id.to_owned(), view_id.to_owned()));
        let existing_group = self.groups.iter().enumerate().filter(|(index, _)| *index != self.active_group)
            .find_map(|(group_index, group)| group.tabs.iter().position(|tab| tab.extension_view.as_ref()
                .is_some_and(|meta| meta.extension_id == extension_id && meta.view_id == view_id))
                .map(|tab_index| (group_index, tab_index)));
        if let Some((group, tab)) = existing_group {
            self.switch_group(group); self.active_tab = tab; self.active_pane = crate::app::ActivePane::Editor;
            self.touch_tab_mru(tab); return true;
        }
        if let Some(existing_idx) = self.tabs.iter().position(|tab| {
            tab.extension_view
                .as_ref()
                .is_some_and(|meta| meta.extension_id == extension_id && meta.view_id == view_id)
        }) {
            self.active_tab = existing_idx;
            self.active_pane = crate::app::ActivePane::Editor;
            self.touch_tab_mru(self.active_tab);
            self.notify(format!("Focused extension view: {}", view.title));
            return true;
        }

        let mut editor = self.new_editor();
        Self::apply_extension_view_to_editor(&mut editor, extension_id, &view.title, &view.content);
        if let Some(frame) = &view.frame {
            editor.modified = frame.dirty;
            if let Some(path) = &frame.path { editor.file_path = Some(PathBuf::from(path)); }
        }

        self.tabs.push(Tab {
            editor,
            pinned: false,
            preview: false,
            kind: TabKind::ExtensionView,
            extension_view: Some(ExtensionViewTabRef {
                extension_id: extension_id.to_string(),
                view_id: view_id.to_string(),
                title: view.title.clone(),
            }),
            terminal_idx: None,
        });
        self.active_tab = self.tabs.len() - 1;
        self.active_pane = crate::app::ActivePane::Editor;
        self.touch_tab_mru(self.active_tab);
        self.notify(format!("Opened extension view: {}", view.title));
        true
    }

    pub(crate) fn execute_extension_command_id(&mut self, id: &str) -> bool {
        let Some(rest) = id.strip_prefix(EXTENSION_COMMAND_PREFIX) else {
            return false;
        };
        let mut parts = rest.splitn(2, ':');
        let ext_id = parts.next().unwrap_or_default();
        let cmd_id = parts.next().unwrap_or_default();
        if ext_id.is_empty() || cmd_id.is_empty() {
            self.notify(format!("Invalid extension command id: {id}"));
            return true;
        }
        let editor_context = self.extension_editor_context_snapshot();
        match self.extension_manager.invoke_command_async(
            ext_id,
            cmd_id,
            &mut self.background,
            None,
            Some(editor_context),
        ) {
            Ok(()) => self.notify("Running extension command...".to_string()),
            Err(err) => self.notify(err),
        }
        self.refresh_open_extension_view_tabs();
        true
    }

    pub(crate) fn extensions_select_next(&mut self) {
        self.move_extensions_selection(1);
    }

    pub(crate) fn extensions_select_prev(&mut self) {
        self.move_extensions_selection(-1);
    }

    pub(crate) fn filtered_extension_indices(&self) -> Vec<usize> {
        self.extension_views().iter().enumerate()
            .filter(|(_, ext)| crate::ui_extensions::matches(ext, &self.extensions_filter))
            .map(|(index, _)| index).collect()
    }

    pub(crate) fn move_extensions_selection(&mut self, delta: isize) {
        self.clamp_extensions_selection();
        let filtered = self.filtered_extension_indices();
        let Some(position) = filtered.iter().position(|index| *index == self.extensions_selected) else { return; };
        let next = position.saturating_add_signed(delta).min(filtered.len() - 1);
        self.extensions_selected = filtered[next];
        let visible = usize::from(self.ext_sidebar.list_area.height / crate::ui_extensions::CARD_HEIGHT).max(1);
        if next < self.extensions_scroll { self.extensions_scroll = next; }
        else if next >= self.extensions_scroll + visible { self.extensions_scroll = next + 1 - visible; }
    }

    pub(crate) fn extensions_rescan(&mut self) {
        self.request_extension_scan();
        self.notify("Scanning extensions...");
    }

    pub(crate) fn extensions_reload_all(&mut self) {
        let notices = self.extension_manager.reload_all();
        self.refresh_palette_commands();
        self.refresh_open_extension_view_tabs();
        if let Some(first) = notices.first() {
            self.notify(first.clone());
        } else {
            self.notify("No enabled extensions to reload");
        }
    }

    pub(crate) fn extensions_reload_selected(&mut self) {
        if !self.filtered_extension_indices().contains(&self.extensions_selected) {
            self.notify("No extension selected");
            return;
        }
        match self
            .extension_manager
            .reload_extension(self.extensions_selected)
        {
            Ok(msg) => self.notify(msg),
            Err(err) => self.notify(err),
        }
        self.refresh_palette_commands();
        self.refresh_open_extension_view_tabs();
        self.clamp_extensions_selection();
    }

    pub(crate) fn extensions_unload_selected(&mut self) {
        if !self.filtered_extension_indices().contains(&self.extensions_selected) {
            self.notify("No extension selected");
            return;
        }
        match self
            .extension_manager
            .unload_extension(self.extensions_selected)
        {
            Ok(msg) => self.notify(msg),
            Err(err) => self.notify(err),
        }
        self.refresh_palette_commands();
        self.refresh_open_extension_view_tabs();
        self.clamp_extensions_selection();
    }

    pub(crate) fn extensions_load_selected(&mut self) {
        if !self.filtered_extension_indices().contains(&self.extensions_selected) {
            self.notify("No extension selected");
            return;
        }
        match self
            .extension_manager
            .load_extension(self.extensions_selected)
        {
            Ok(msg) => self.notify(msg),
            Err(err) => self.notify(err),
        }
        self.refresh_palette_commands();
        self.refresh_open_extension_view_tabs();
        self.clamp_extensions_selection();
    }

    pub(crate) fn clamp_extensions_selection(&mut self) {
        let filtered = self.filtered_extension_indices();
        if filtered.is_empty() {
            self.extensions_selected = 0;
            self.extensions_scroll = 0;
            return;
        }
        if !filtered.contains(&self.extensions_selected) { self.extensions_selected = filtered[0]; }
        let visible = usize::from(self.ext_sidebar.list_area.height / crate::ui_extensions::CARD_HEIGHT).max(1);
        self.extensions_scroll = self.extensions_scroll.min(filtered.len().saturating_sub(visible));
    }

    pub(crate) fn apply_extension_text_edits(&mut self) -> bool {
        let edits = self.extension_manager.take_pending_text_edits();
        if edits.is_empty() {
            return false;
        }

        for edit in edits {
            self.apply_extension_text_edit(edit);
        }
        true
    }

    fn apply_extension_text_edit(&mut self, edit: crate::extensions::TextEditRequest) {
        match edit {
            crate::extensions::TextEditRequest::OpenDocument { name, text } => {
                self.new_file();
                if let Some(editor) = self.editor_mut() {
                    editor.rope = Rope::from_str(&text);
                    editor.suggested_file_name = Some(name);
                    editor.modified = true;
                    editor.mark_syntax_dirty_range(0, editor.rope.len_lines());
                }
                self.notify("Export opened. Save As chooses its destination.");
            }
            crate::extensions::TextEditRequest::InsertText { line, col, text } => {
                if let Some(editor) = self.editor_mut() {
                    let line_count = editor.rope.len_lines();
                    if line < line_count {
                        editor.save_undo_public();
                        editor.mark_syntax_dirty_range(line, line + text.matches('\n').count());
                        let line_start = editor.rope.line_to_char(line);
                        let line_len = editor.rope.line(line).len_chars();
                        let char_idx = line_start + col.min(line_len);
                        editor.rope.insert(char_idx, &text);
                        editor.modified = true;
                    }
                }
            }
            crate::extensions::TextEditRequest::DeleteRange {
                start_line,
                start_col,
                end_line,
                end_col,
            } => {
                if let Some(editor) = self.editor_mut() {
                    let line_count = editor.rope.len_lines();
                    if start_line < line_count && end_line < line_count {
                        let start_line_start = editor.rope.line_to_char(start_line);
                        let start_line_len = editor.rope.line(start_line).len_chars();
                        let start_idx = start_line_start + start_col.min(start_line_len);

                        let end_line_start = editor.rope.line_to_char(end_line);
                        let end_line_len = editor.rope.line(end_line).len_chars();
                        let end_idx = end_line_start + end_col.min(end_line_len);

                        if end_idx > start_idx {
                            editor.save_undo_public();
                            editor.mark_syntax_dirty_range(start_line, end_line);
                            editor.rope.remove(start_idx..end_idx);
                            editor.modified = true;
                        }
                    }
                }
            }
            crate::extensions::TextEditRequest::ReplaceSelection { text } => {
                if let Some(editor) = self.editor_mut() {
                    editor.save_undo_public();
                    editor.delete_selection_for_replacement();
                    let line = editor.cursor_line;
                    let col = editor.cursor_col;
                    let line_count = editor.rope.len_lines();
                    if line < line_count {
                        let line_start = editor.rope.line_to_char(line);
                        let line_len = editor.rope.line(line).len_chars();
                        let char_idx = line_start + col.min(line_len);
                        editor.mark_syntax_dirty_range(line, line + text.matches('\n').count());
                        editor.rope.insert(char_idx, &text);
                        editor.modified = true;
                        let new_idx = char_idx + text.chars().count();
                        editor.cursor_line = editor.rope.char_to_line(new_idx);
                        let new_line_start = editor.rope.line_to_char(editor.cursor_line);
                        editor.cursor_col = new_idx - new_line_start;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_replacement_without_selection_has_one_undo_snapshot() {
        let mut app = App::test_new();
        app.new_file();
        let editor = app.editor_mut().unwrap();
        editor.rope = Rope::from_str("ab\ncd");
        editor.cursor_col = 1;
        let version = editor.content_version;

        app.apply_extension_text_edit(crate::extensions::TextEditRequest::ReplaceSelection {
            text: "λ\n🦀".to_owned(),
        });
        let editor = app.editor_mut().unwrap();
        assert_eq!(editor.rope.to_string(), "aλ\n🦀b\ncd");
        assert_eq!((editor.cursor_line, editor.cursor_col), (1, 1));
        assert_eq!(editor.content_version, version + 1);
        assert_eq!(editor.undo_stack.len(), 1);
        assert!(editor.modified);
        editor.undo();
        assert_eq!(editor.rope.to_string(), "ab\ncd");
        assert_eq!((editor.cursor_line, editor.cursor_col), (0, 1));
        assert!(!editor.modified);
    }

    #[test]
    fn extension_replacement_restores_reversed_multiline_selection_on_undo() {
        let mut app = App::test_new();
        app.new_file();
        let editor = app.editor_mut().unwrap();
        editor.rope = Rope::from_str("abc\r\ndef\r\nghi");
        editor.cursor_col = 1;
        editor.selection_anchor = Some((2, 2));
        let version = editor.content_version;

        app.apply_extension_text_edit(crate::extensions::TextEditRequest::ReplaceSelection {
            text: "λ".to_owned(),
        });
        let editor = app.editor_mut().unwrap();
        assert_eq!(editor.rope.to_string(), "aλi");
        assert_eq!((editor.cursor_line, editor.cursor_col), (0, 2));
        assert_eq!(editor.selection_anchor, None);
        assert_eq!(editor.content_version, version + 1);
        assert_eq!(editor.undo_stack.len(), 1);
        editor.undo();
        assert_eq!(editor.rope.to_string(), "abc\r\ndef\r\nghi");
        assert_eq!((editor.cursor_line, editor.cursor_col), (0, 1));
        assert_eq!(editor.selection_anchor, Some((2, 2)));
    }

    #[test]
    fn rope_equals_text_equal() {
        let rope = Rope::from_str("hello world");
        assert!(App::rope_equals_text(&rope, "hello world"));
    }

    #[test]
    fn rope_equals_text_different() {
        let rope = Rope::from_str("hello");
        assert!(!App::rope_equals_text(&rope, "world"));
    }

    #[test]
    fn rope_equals_text_different_length() {
        let rope = Rope::from_str("hi");
        assert!(!App::rope_equals_text(&rope, "hello"));
    }

    #[test]
    fn rope_equals_text_empty() {
        let rope = Rope::from_str("");
        assert!(App::rope_equals_text(&rope, ""));
    }

    #[test]
    fn current_extension_editor_context_includes_buffer_cursor_and_selection() {
        let mut app = App::test_new();
        app.new_file();
        let file_path = app.explorer.root.join("sample.txt");
        {
            let editor = app.editor_mut().unwrap();
            editor.rope = Rope::from_str("hello");
            editor.file_path = Some(file_path.clone());
            editor.cursor_line = 0;
            editor.cursor_col = 4;
            editor.selection_anchor = Some((0, 1));
        }

        let context = app.current_extension_editor_context();
        let file_path_str = file_path.to_string_lossy().into_owned();
        let workspace_root = app.explorer.root.to_string_lossy().into_owned();

        assert_eq!(
            context.active_file_path.as_deref(),
            Some(file_path_str.as_str())
        );
        let buffer = context.active_buffer.as_ref().unwrap();
        assert_eq!(buffer.bytes(), b"hello");
        assert_eq!(context.cursor_line, Some(0));
        assert_eq!(context.cursor_col, Some(4));
        assert_eq!(context.selection_range, Some((0, 1, 0, 4)));
        assert_eq!(
            context.workspace_root.as_deref(),
            Some(workspace_root.as_str())
        );
    }

    #[test]
    fn current_extension_editor_context_without_tabs_still_reports_workspace_root() {
        let app = App::test_new();

        let context = app.current_extension_editor_context();
        let workspace_root = app.explorer.root.to_string_lossy().into_owned();

        assert!(context.active_file_path.is_none());
        assert!(context.active_buffer.is_none());
        assert_eq!(context.cursor_line, None);
        assert_eq!(context.cursor_col, None);
        assert_eq!(
            context.workspace_root.as_deref(),
            Some(workspace_root.as_str())
        );
    }

    #[test]
    fn vanilla_mode_keeps_extensions_and_the_git_installer_discoverable() {
        let mut app = App::test_new();
        assert!(app.extension_manager.is_empty());

        app.open_extensions_sidebar();
        assert_eq!(app.sidebar_view, SidebarView::Extensions);
        assert!(app.show_explorer);
        assert_eq!(app.activity_bar_selected, 4);

        app.refresh_palette_commands();
        for id in ["extensions", "extensions_install", "extensions_rescan"] {
            assert!(app.palette.commands.iter().any(|command| command.id == id));
        }

        app.rebuild_keybinding_settings_cache();
        assert!(app
            .keybinding_settings_cache
            .iter()
            .any(|(label, _, _)| label == "Shortcut: Extensions"));
    }
}
