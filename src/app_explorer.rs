use std::path::Path;

use crate::app::{ActivePane, App, SidebarView};

pub(crate) struct FileClipboard {
    pub paths: Vec<std::path::PathBuf>,
    pub cut: bool,
    // Terminals can translate Ctrl+V into a bracketed text paste. Only our
    // own file-list payload may trigger a filesystem operation from that event.
    terminal_text: String,
}

pub(crate) use crate::fs_ops::rename_no_replace;

#[cfg(test)]
#[path = "explorer_clipboard_tests.rs"]
mod clipboard_tests;

#[cfg(test)]
#[path = "explorer_delete_tests.rs"]
mod delete_tests;

impl App {
    /// File-tree commands are resolved before editor commands, regardless of
    /// their keybinding or whether they came from the Edit menu.
    pub(crate) fn route_explorer_edit_command(&mut self, id: &str) -> bool {
        use crossterm::event::{KeyCode, KeyModifiers};
        if self.active_pane != ActivePane::Explorer {
            return false;
        }
        let (key, action) = match id {
            "copy" => ('c', "Copy File"),
            "cut" => ('x', "Cut File"),
            "paste" | "paste_plain" | "clipboard_history" => ('v', "Paste File"),
            "select_all" => ('a', ""),
            "undo" => ('z', ""),
            "redo" => ('y', ""),
            _ => return false,
        };
        if self.handle_focused_text_input_shortcut(KeyCode::Char(key), KeyModifiers::CONTROL) {
            return true;
        }
        // The tree has no text undo target. Its focus must not undo or redo
        // unrelated edits in the active document.
        if matches!(id, "undo" | "redo") {
            return true;
        }
        if self.sidebar_view == SidebarView::SourceControl && id == "select_all" {
            return false;
        }
        // Other sidebars and non-text controls must never edit the document
        // behind them, nor operate on an invisible file-tree selection.
        if !self.show_explorer
            || self.sidebar_view != SidebarView::Explorer
            || (self.explorer.find.open
                && self.explorer.find.focus != crate::explorer_find::Focus::Tree)
        {
            return true;
        }
        if id == "select_all" {
            self.explorer_multi_selected = self
                .explorer
                .entries
                .iter()
                .map(|entry| entry.path.clone())
                .collect();
            self.explorer_selection_anchor =
                (!self.explorer.entries.is_empty()).then_some(self.explorer.selected);
            return true;
        }
        self.explorer_context_target = self
            .explorer
            .entries
            .get(self.explorer.selected)
            .filter(|_| {
                self.explorer_selection_anchor.is_some() || !self.explorer_multi_selected.is_empty()
            })
            .map(|entry| entry.path.clone());
        self.execute_explorer_action(action);
        true
    }

    pub(crate) fn paste_explorer_clipboard_text(&mut self, text: &str) -> bool {
        if self.active_pane == ActivePane::Explorer
            && self.sidebar_view == SidebarView::Explorer
            && self.explorer_clipboard.as_ref().is_some_and(|clipboard| {
                // Terminals may normalize the separators of a multi-file list.
                // Paths themselves escape control characters when copied.
                clipboard
                    .terminal_text
                    .split(['\r', '\n'])
                    .filter(|line| !line.is_empty())
                    .eq(text.split(['\r', '\n']).filter(|line| !line.is_empty()))
            })
        {
            return self.route_explorer_edit_command("paste");
        }
        false
    }
    pub(crate) fn display_path(&self, path: &Path) -> String {
        let display = self.remote_workspace.as_ref().and_then(|remote| remote.display_path(path).ok())
            .unwrap_or_else(|| path.display().to_string());
        Self::escape_path_controls(display)
    }
    /// Read-only labels use the remote machine's native spelling. Editable
    /// locators can instead need URI escaping to round-trip unusual filenames.
    pub(crate) fn display_path_label(&self, path: &Path) -> String {
        let display = self.remote_workspace.as_ref().and_then(|remote| remote.context_path(path).ok())
            .unwrap_or_else(|| path.display().to_string());
        Self::escape_path_controls(display)
    }
    fn escape_path_controls(display: String) -> String {
        if !display.chars().any(char::is_control) { return display; }
        let mut escaped = String::with_capacity(display.len());
        for ch in display.chars() {
            if ch.is_control() { escaped.extend(ch.escape_default()); } else { escaped.push(ch); }
        }
        escaped
    }
    pub(crate) fn explorer_target_directory(&self, target: Option<&Path>) -> std::path::PathBuf {
        match target {
            Some(target) if target == self.explorer.root || self.explorer.find_path(target).is_some_and(|i| self.explorer.entries[i].is_dir) => target.to_owned(),
            Some(target) => target.parent().unwrap_or(&self.explorer.root).to_owned(),
            None => self.explorer.root.clone(),
        }
    }

    pub(crate) fn begin_remote_creation(&mut self, directory: std::path::PathBuf, folder: bool) {
        let base = if folder { "new folder" } else { "new file" };
        let mut name = base.to_owned();
        for index in 1..=10_000 {
            if self.explorer.find_path(&directory.join(&name)).is_none() { break; }
            name = format!("{base} ({index})");
        }
        let path = directory.join(&name);
        self.explorer.insert_remote_placeholder(path.clone(), name.clone(), folder);
        self.creating_new_file = !folder; self.creating_new_folder = folder;
        self.begin_rename(path, name, true, true);
    }

    pub(crate) fn run_remote_filesystem_action(&mut self, action: &'static str, task: crate::remote::fs::Task,
        finish_rename: Option<std::path::PathBuf>) {
        let Some(remote) = self.remote_workspace.clone() else { return; };
        self.run_filesystem_action_async("remote-filesystem", action, move || {
            let mut result = remote.filesystem(task, &crate::background::CancellationToken::new(), |_| {}).map_err(|error| error.to_string())?;
            result.finish_rename = finish_rename;
            Ok(result)
        });
    }

    pub(crate) fn run_filesystem_action_async(
        &mut self,
        worker_name: &'static str,
        action: &'static str,
        job: impl FnOnce() -> Result<crate::background::FilesystemActionOutcome, String>
            + Send
            + 'static,
    ) {
        if self.bg_filesystem_action_running {
            self.notify("Another file operation is already running");
            return;
        }
        self.bg_filesystem_action_running = true;
        self.status_message = format!("{action}...");
        let sender = self.background.sender.clone();
        let generation = self.workspace_generation;
        self.background.spawn(worker_name, move || {
            let result = job();
            let result = crate::background::BackgroundResult::FilesystemActionDone { action, result };
            let _ = sender.send(crate::background::BackgroundResult::WorkspaceResult { generation, result: Box::new(result) });
        });
    }

    pub(crate) fn execute_explorer_context(&mut self, idx: usize) -> bool {
        self.explorer_context_active = false;
        let items = self.explorer_context_menu_items();
        let dynamic_count = self.explorer_context_extension_commands.len();
        let static_count = items.len().saturating_sub(dynamic_count);
        if idx >= static_count {
            if let Some(command) = self
                .explorer_context_extension_commands
                .get(idx - static_count)
                .cloned()
            {
                let context_path = self.explorer_context_target.clone();
                self.run_extension_command_with_optional_context(
                    &command.extension_id,
                    &command.command_id,
                    context_path.as_deref(),
                );
            }
            self.explorer_context_extension_commands.clear();
            self.explorer_context_target = None;
            return false;
        }
        let action = match items.get(idx) {
            Some(a) => *a,
            None => return false,
        };
        let action = action.to_owned();
        self.execute_explorer_action(&action)
    }

    fn execute_explorer_action(&mut self, action: &str) -> bool {
        self.explorer_context_active = false;
        match action {
            "Find by Name…" => self.open_explorer_find(),
            "Find in Folder…" => {
                if let Some(folder) = self.explorer_context_target.as_deref() {
                    self.search_include_pattern =
                        crate::search_paths::folder_include_pattern(&self.explorer.root, folder);
                    self.global_search_field_cursors[2] = self.search_include_pattern.len();
                    self.global_search_field_selection_anchors[2] = None;
                    self.focus_global_search();
                    self.global_search_field_cursors[0] = self.global_search_query.len();
                    self.global_search_field_selection_anchors[0] =
                        (!self.global_search_query.is_empty()).then_some(0);
                    self.update_global_search();
                }
            }
            "Open" => {
                if let Some(path) = self.toggle_or_open_explorer_async() {
                    self.open_file(&path);
                }
            }
            "Copy File" | "Cut File" => {
                let mut targets = self.explorer_context_targets();
                // Selecting all can include a folder and its visible children.
                // Copy/move each tree only once, never as separate overlapping operations.
                if targets.len() > 1 {
                    let selected: rustc_hash::FxHashSet<_> = targets.iter().cloned().collect();
                    targets.retain(|path| !path.ancestors().skip(1).any(|parent| selected.contains(parent)));
                }
                if !targets.is_empty() {
                    let cut = action == "Cut File";
                    let copied_count = targets.len();
                    let copied_name = targets[0]
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("file")
                        .to_string();
                    let mut terminal_text = String::new();
                    for (index, path) in targets.iter().enumerate() {
                        if index > 0 { terminal_text.push('\n'); }
                        terminal_text.push_str(&self.display_path(path));
                    }
                    // Keep internal file ownership even if the system clipboard
                    // is unavailable (for example a terminal without OSC 52).
                    let _ = crate::clipboard::set_text(&terminal_text);
                    self.explorer_clipboard = Some(FileClipboard { paths: targets, cut, terminal_text });
                    let verb = if cut { "Cut" } else { "Copied" };
                    if copied_count == 1 {
                        self.notify(format!("{verb}: {copied_name}"));
                    } else {
                        self.notify(format!("{verb} {copied_count} items"));
                    }
                }
            }
            "Paste File" => {
                if let Some(clipboard) = &self.explorer_clipboard {
                    let sources = clipboard.paths.clone();
                    let is_cut = clipboard.cut;
                    if let Some(remote) = self.remote_workspace.clone() {
                        let task = (|| -> std::io::Result<_> { Ok(crate::remote::fs::Task::Paste {
                            sources: sources.iter().map(|path| remote.remote_path(path)).collect::<std::io::Result<_>>()?,
                            directory: remote.remote_path(&self.explorer_target_directory(self.explorer_context_target.as_deref()))?, cut: is_cut,
                        }) })();
                        match task { Ok(task) => self.run_remote_filesystem_action("Paste", task, None), Err(error) => self.notify_error(error.to_string()) }
                        return false;
                    }
                    let target = self.explorer_context_target.clone();
                    let root = self.explorer.root.clone();
                    self.run_filesystem_action_async("explorer-paste", "Paste", move || {
                        let dest_dir = match target {
                            Some(target) if target.is_dir() => target,
                            Some(target) => {
                                target.parent().map(Path::to_path_buf).unwrap_or(target)
                            }
                            None => root,
                        };
                        let mut processed = 0usize;
                        let mut failed = 0usize;
                        let mut last_error = None;
                        let mut last_dest = None;
                        let mut renames = Vec::new();
                        for src in sources {
                            let file_name = src.file_name().unwrap_or_default();
                            let mut dest = dest_dir.join(file_name);
                            if (!is_cut || dest != src) && std::fs::symlink_metadata(&dest).is_ok() {
                                let stem = dest
                                    .file_stem()
                                    .and_then(|stem| stem.to_str())
                                    .unwrap_or("file")
                                    .to_string();
                                let extension = dest
                                    .extension()
                                    .and_then(|extension| extension.to_str())
                                    .map(|extension| format!(".{extension}"))
                                    .unwrap_or_default();
                                for suffix in 1.. {
                                    let candidate =
                                        dest_dir.join(format!("{stem} ({suffix}){extension}"));
                                    match std::fs::symlink_metadata(&candidate) {
                                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                                            dest = candidate;
                                            break;
                                        }
                                        Err(error) => return Err(error.to_string()),
                                        Ok(_) => {}
                                    }
                                }
                            }
                            let result = if is_cut {
                                rename_no_replace(&src, &dest)
                            } else {
                                crate::fs_ops::copy_no_replace(&src, &dest, || false)
                            };
                            match result {
                                Ok(()) => {
                                    if is_cut && src != dest {
                                        renames.push((src, dest.clone()));
                                    }
                                    processed += 1;
                                    last_dest = Some(dest);
                                }
                                Err(error) => {
                                    failed += 1;
                                    last_error = Some(error.to_string());
                                }
                            }
                        }
                        if processed == 0 {
                            return Err(
                                last_error.unwrap_or_else(|| "nothing was copied".to_string())
                            );
                        }
                        let verb = if is_cut { "Moved" } else { "Pasted" };
                        let message = if processed == 1 && failed == 0 {
                            format!(
                                "{verb}: {}",
                                last_dest.as_deref().map_or_else(
                                    || dest_dir.display().to_string(),
                                    |path| { path.display().to_string() }
                                )
                            )
                        } else if failed == 0 {
                            format!("{verb} {processed} items")
                        } else {
                            format!(
                                "Processed {processed} items; {failed} failed{}",
                                last_error
                                    .as_deref()
                                    .map(|error| format!(": {error}"))
                                    .unwrap_or_default()
                            )
                        };
                        Ok(crate::background::FilesystemActionOutcome {
                            message,
                            renames,
                            clear_explorer_clipboard: is_cut,
                            ..Default::default()
                        })
                    });
                } else {
                    self.notify("Nothing to paste. Copy or cut a file first.");
                }
            }
            "Rename" => {
                if let Some(target) = &self.explorer_context_target {
                    let initial = target
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string();
                    self.begin_rename(target.clone(), initial, true, false);
                }
            }
            "Delete" => {
                let targets = self.explorer_context_targets();
                if !targets.is_empty() {
                    self.request_explorer_delete_targets(targets);
                }
            }
            "New File" => {
                // The placeholder needs to remain visible for inline naming,
                // even when its temporary name does not match the search.
                if self.explorer.find.open { self.close_explorer_find(); }
                if self.remote_workspace.is_some() {
                    self.begin_remote_creation(self.explorer_target_directory(self.explorer_context_target.as_deref()), false);
                    return false;
                }
                if let Some(target) = &self.explorer_context_target {
                    let dir = if target.is_dir() {
                        target.clone()
                    } else {
                        target.parent().unwrap_or(target).to_path_buf()
                    };
                    let placeholder_path = Self::next_available_child_name(&dir, "new file");
                    match std::fs::File::create(&placeholder_path) {
                        Ok(_) => {
                            self.refresh_explorer_after_edit(Some(&placeholder_path));
                            let initial = placeholder_path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("new file")
                                .to_string();
                            self.creating_new_file = true;
                            self.creating_new_folder = false;
                            self.begin_rename(placeholder_path, initial, true, true);
                        }
                        Err(e) => self.notify(format!("Create failed: {e}")),
                    }
                }
            }
            "New Folder" => {
                if self.explorer.find.open { self.close_explorer_find(); }
                if self.remote_workspace.is_some() {
                    self.begin_remote_creation(self.explorer_target_directory(self.explorer_context_target.as_deref()), true);
                    return false;
                }
                if let Some(target) = &self.explorer_context_target {
                    let dir = if target.is_dir() {
                        target.clone()
                    } else {
                        target.parent().unwrap_or(target).to_path_buf()
                    };
                    let placeholder_path = Self::next_available_child_name(&dir, "new folder");
                    match std::fs::create_dir(&placeholder_path) {
                        Ok(()) => {
                            self.refresh_explorer_after_edit(Some(&placeholder_path));
                            let initial = placeholder_path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("new folder")
                                .to_string();
                            self.creating_new_file = false;
                            self.creating_new_folder = true;
                            self.begin_rename(placeholder_path, initial, true, true);
                        }
                        Err(e) => self.notify(format!("Create folder failed: {e}")),
                    }
                }
            }
            "Open in System App" => {
                if let Some(target) = self.explorer_context_target.clone() {
                    self.open_external(crate::external_open::Request::SystemApp(target));
                }
            }
            "Reveal in File Explorer" => {
                if let Some(target) = self.explorer_context_target.clone() {
                    self.open_external(crate::external_open::Request::Reveal(target));
                }
            }
            "Open in Terminal" => {
                if let Some(target) = self.explorer_context_target.clone() {
                    self.open_external(crate::external_open::Request::Terminal(target));
                }
            }
            "Copy Path" => {
                if let Some(target) = &self.explorer_context_target {
                    let path = self.remote_workspace.as_ref().and_then(|remote| remote.context_path(target).ok()).unwrap_or_else(|| target.display().to_string());
                    if crate::clipboard::set_text(path).is_ok() {
                        self.notify("Path copied to clipboard");
                    }
                }
            }
            "Copy Relative Path" => {
                if let Some(target) = &self.explorer_context_target {
                    let rel = target
                        .strip_prefix(&self.explorer.root)
                        .unwrap_or(target)
                        .to_string_lossy()
                        .to_string();
                    let rel = self.remote_workspace.as_ref().and_then(|remote| remote.remote_path(target).ok()).map(|path| path.display()).unwrap_or(rel);
                    if crate::clipboard::set_text(rel).is_ok() {
                        self.notify("Relative path copied to clipboard");
                    }
                }
            }
            "Select for Compare" => {
                if let Some(target) = self.explorer_context_target.clone() {
                    self.select_file_for_compare(target);
                }
            }
            "Compare with Selected" => {
                if let Some(target) = self.explorer_context_target.clone() {
                    self.compare_file_with_selected(target);
                }
            }
            "Sort by Name" => {
                self.explorer.sort_mode = crate::explorer::ExplorerSortMode::Name;
                self.refresh_explorer_tree_async();
                self.notify("Explorer: Sort by Name");
            }
            "Sort by Size" => {
                self.explorer.sort_mode = crate::explorer::ExplorerSortMode::Size;
                self.refresh_explorer_tree_async();
                self.notify("Explorer: Sort by Size");
            }
            "Sort by Date Modified" => {
                self.explorer.sort_mode = crate::explorer::ExplorerSortMode::Modified;
                self.refresh_explorer_tree_async();
                self.notify("Explorer: Sort by Date Modified");
            }
            "Sort by Type" => {
                self.explorer.sort_mode = crate::explorer::ExplorerSortMode::Type;
                self.refresh_explorer_tree_async();
                self.notify("Explorer: Sort by Type");
            }
            _ => {}
        }
        self.explorer_context_extension_commands.clear();
        self.explorer_context_target = None;
        false
    }

    pub(crate) fn execute_rename(&mut self) {
        if self.remote_workspace.is_some() {
            if self.bg_filesystem_action_running { return; }
            let Some(target) = self.rename.target.clone() else { return; };
            let name = self.rename.query.trim();
            if name.is_empty() { self.cancel_rename(); return; }
            if name.contains(['/', '\0']) || name == "." || name == ".." { self.notify_error("Invalid remote filename"); return; }
            let remote = self.remote_workspace.as_ref().unwrap();
            let task = (|| -> std::io::Result<_> {
                let from = remote.remote_path(&target)?;
                let to = from.parent().ok_or_else(|| std::io::Error::other("Cannot rename workspace root"))?.child(name.as_bytes())?;
                Ok(if self.rename.placeholder_active { crate::remote::fs::Task::Create { path: to, directory: self.creating_new_folder } }
                    else { crate::remote::fs::Task::Rename { from, to } })
            })();
            match task { Ok(task) => self.run_remote_filesystem_action("Rename", task, Some(target)), Err(error) => self.notify_error(error.to_string()) }
            return;
        }
        let creating_file = self.creating_new_file;
        let creating_folder = self.creating_new_folder;
        let placeholder_active = self.rename.placeholder_active;

        // Retain the target and creation flags until the edit succeeds so the
        // user can correct an invalid name or retry a filesystem error.
        if let Some(target) = self.rename.target.clone() {
            let new_name = self.rename.query.trim().to_string();
            if new_name.is_empty() {
                if placeholder_active && target.exists() {
                    let _ = if target.is_dir() {
                        std::fs::remove_dir(&target)
                    } else {
                        std::fs::remove_file(&target)
                    }; // Error OK: cleanup may fail if already removed
                    self.refresh_explorer_after_edit(None);
                }
                self.notify("Cancelled: empty name");
                self.rename.active = false;
                self.rename.selection_anchor = None;
                self.rename.cursor = 0;
                self.rename.placeholder_active = false;
                self.rename.target = None;
                self.creating_new_file = false;
                self.creating_new_folder = false;
                return;
            }

            // Validate filename characters
            let invalid_chars: &[char] = if cfg!(windows) {
                &['<', '>', ':', '"', '/', '\\', '|', '?', '*']
            } else {
                &['/', '\0']
            };
            if let Some(bad) = new_name.chars().find(|c| {
                invalid_chars.contains(c) || *c == '\0' || (cfg!(windows) && *c < ' ')
            }) {
                self.notify_error(format!("Invalid character in filename: {bad:?}"));
                return;
            }
            if new_name == "." || new_name == ".." || (cfg!(windows) && new_name.ends_with('.')) {
                self.notify_error("Invalid filename");
                return;
            }
            #[cfg(windows)]
            {
                let stem = new_name.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
                let port = stem.strip_prefix("COM").or_else(|| stem.strip_prefix("LPT"));
                if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                    || port.is_some_and(|suffix| matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"))
                {
                    self.notify_error("This filename is reserved by Windows");
                    return;
                }
            }

            let new_path = target.parent().unwrap_or(&target).join(&new_name);
            if let Err(error) = rename_no_replace(&target, &new_path) {
                self.notify_error(format!("Rename failed: {error}"));
                return;
            }
            if creating_file || creating_folder {
                self.refresh_explorer_after_edit(Some(&new_path));
                if creating_file {
                    self.notify(format!("Created: {}", new_path.display()));
                    self.open_file(&new_path);
                } else {
                    self.notify(format!("Created folder: {}", new_path.display()));
                }
            } else if new_path != target {
                // Update paths in every group only after the move succeeds.
                for tab in self.tabs.iter_mut().chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs)) {
                    if let Some(suffix) = tab.editor.file_path.as_ref().and_then(|path| path.strip_prefix(&target).ok()) {
                        tab.editor.file_path = Some(new_path.join(suffix));
                    }
                }
                self.lsp_manager.did_close(&target);
                self.lsp_manager.did_rename_files(&target, &new_path);
                self.navigation.rename_path(&target, &new_path);
                self.notify(format!("Renamed to: {}", new_path.display()));
                self.refresh_explorer_after_edit(Some(&new_path));
            }
        }
        self.rename.active = false;
        self.rename.target = None;
        self.creating_new_file = false;
        self.creating_new_folder = false;
        self.rename.selection_anchor = None;
        self.rename.cursor = 0;
        self.rename.placeholder_active = false;
    }

    pub(crate) fn execute_save_as(&mut self) {
        if self.tabs.get(self.active_tab).is_some_and(|tab| tab.kind == crate::app::TabKind::GitRevision) {
            self.save_as_active = false;
            self.notify("Historical versions are read-only. Open the working file to edit.");
            return;
        }
        self.sync_duplicate_file_tabs();
        let Some((path, replace)) = crate::ui_modal::save_target(self) else {
            return;
        };
        if self.remote_workspace.is_some()
            && self.editor().and_then(|editor| editor.file_path.as_ref()) == Some(&path)
        {
            // Saving to this document's own name retains its saved checkpoint
            // and external-change checks, including custom-editor dispatch.
            self.save_as_active = false;
            self.save_current_file();
            return;
        }
        if let Some(view) = self.active_extension_view().cloned() {
            let context = self.extension_editor_context_snapshot();
            if self.extension_manager.queue_document_action(&view.extension_id, &view.view_id,
                crate::extension_handlers::DocumentWorkbenchAction::SaveAs { path, replace }, context) {
                self.save_as_active = false;
                return;
            }
            self.notify_error("This extension view cannot be saved as a file");
            return;
        }
        let path_str = path.display().to_string();
        let trim = self.trim_trailing_on_save;
        let newline = self.insert_final_newline;
        let Some(editor) = self.editor() else { return; };
        let identity = editor.save_identity;
        if self.pending_save_as.contains_key(&identity)
            || self.pending_file_saves.keys().chain(self.deferred_file_saves.keys()).any(|pending|
                Self::same_open_path(pending, &path)
                    || editor.file_path.as_ref().is_some_and(|source| Self::same_open_path(pending, source)))
        {
            self.modal.error = Some("Wait for the current save to finish before using Save As.".into());
            return;
        }
        let request = crate::editor::BackgroundSaveRequest {
            prepared_options: None,
            source_document_id: identity,
            path: path.clone(),
            rope: editor.rope.clone(),
            encoding: editor.encoding,
            line_ending: editor.cached_line_ending,
            trim_trailing: editor.ec_trim_trailing.unwrap_or(trim),
            insert_final_newline: editor.ec_insert_final_newline.unwrap_or(newline),
            source_content_version: editor.content_version,
            fail_if_exists: !replace,
            expected_disk: None,
        };
        if self.queue_background_save_job(request) {
            self.pending_save_as.insert(identity, (self.workspace_generation, path));
            for tab in self.tabs.iter_mut().chain(self.groups.iter_mut().flat_map(|g| &mut g.tabs)) {
                if tab.editor.save_identity == identity { tab.editor.break_undo_group(); }
            }
            self.notify(format!("Saving as: {path_str}..."));
            self.status_message = format!("Saving: {path_str}...");
            self.save_as_active = false;
        } else {
            self.modal.error = Some("Save As could not start. Wait for the current file operation to finish.".into());
        }
    }

    pub(crate) fn finish_save_as(&mut self, identity: u64, path: &Path) {
        let mut previous = None;
        for tab in self.tabs.iter_mut().chain(self.groups.iter_mut().flat_map(|g| &mut g.tabs)) {
            if tab.kind != crate::app::TabKind::File { continue; }
            tab.editor.refresh_document();
            if tab.editor.save_identity != identity { continue; }
            if previous.is_none() { previous = tab.editor.file_path.clone(); }
            tab.editor.file_path = Some(path.to_path_buf());
            tab.editor.suggested_file_name = None;
            tab.editor.publish_document();
            tab.preview = false;
        }
        if let Some(previous) = previous.filter(|previous| previous != path) {
            self.auto_save_attempted_versions.remove(&previous);
            self.lsp_manager.did_close(&previous);
        }
        self.desks.sync_stamp = None;
    }

}

#[cfg(test)]
mod tests {
    use crate::app::App;
    use std::path::PathBuf;

    #[test]
    fn find_in_folder_reruns_existing_search_and_preserves_options() {
        let workspace = tempfile::tempdir().unwrap();
        let folder = workspace.path().join("src/nested [id],{draft");
        std::fs::create_dir_all(&folder).unwrap();
        for name in ["match.txt", "skip.txt"] {
            std::fs::write(folder.join(name), "needle\n").unwrap();
        }
        std::fs::write(folder.join("case.txt"), "NEEDLE\n").unwrap();
        std::fs::write(folder.join("word.txt"), "needles\n").unwrap();
        let outside = workspace.path().join("other.txt");
        std::fs::write(&outside, "needle\n").unwrap();
        let mut app = App::new(workspace.path().to_path_buf());
        app.search_case_sensitive = true;
        app.search_whole_word = true;
        app.search_exclude_pattern = "skip.txt".into();
        app.search_include_pattern = "./other.txt".into();
        app.global_search_query = "needle".into();
        app.global_search_replace_query = "replacement".into();
        app.update_global_search();
        let generation = app.search_generation;
        app.explorer_context_target = Some(folder.clone());
        let action = app.explorer_context_menu_items().iter()
            .position(|&label| label == "Find in Folder…").unwrap();
        app.execute_explorer_context(action);
        app.wait_for_background(std::time::Duration::from_secs(5), |app| !app.bg_global_search_running);

        assert!(app.search_generation > generation);
        assert!(app.global_search_error.is_none());
        assert_eq!(app.global_search_results.len(), 1);
        assert_eq!(app.global_search_results[0].file_path, folder.join("match.txt"));
        assert_eq!(app.search_exclude_pattern, "skip.txt");
        assert_eq!(app.global_search_replace_query, "replacement");
        assert_eq!(app.global_search_field_cursors[2], app.search_include_pattern.len());

        // The existing query is selected so typing can replace it immediately.
        crate::input_keyboard::handle_keyboard_event(&mut app, crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('n'), crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.global_search_query, "n");
        app.wait_for_background(std::time::Duration::from_secs(5), |app| !app.bg_global_search_running);

        app.explorer_context_target = Some(outside);
        assert!(!app.explorer_context_menu_items().contains(&"Find in Folder…"));
    }

    #[test]
    fn explorer_compare_actions_open_two_closed_files_and_reuse_selection() {
        fn choose(app: &mut App, path: &std::path::Path, label: &str) {
            app.explorer_context_target = Some(path.to_path_buf());
            let index = app.explorer_context_menu_items().iter()
                .position(|item| *item == label).expect("compare action");
            app.execute_explorer_context(index);
        }

        let workspace = tempfile::tempdir().unwrap();
        let base = workspace.path().join("left.txt");
        let target = workspace.path().join("right.txt");
        let third = workspace.path().join("third.txt");
        let folder = workspace.path().join("folder");
        std::fs::write(&base, "left\n").unwrap();
        std::fs::write(&target, "right\n").unwrap();
        std::fs::write(&third, "third\n").unwrap();
        std::fs::create_dir(&folder).unwrap();
        let mut app = App::new(workspace.path().to_path_buf());

        app.explorer_context_target = Some(target.clone());
        assert!(!app.explorer_context_menu_items().contains(&"Compare with Selected"));
        choose(&mut app, &base, "Select for Compare");
        assert!(app.tabs.is_empty(), "selecting a file should not open it");
        app.explorer_context_target = Some(base.clone());
        assert!(!app.explorer_context_menu_items().contains(&"Compare with Selected"));
        app.explorer_context_target = Some(folder);
        assert!(!app.explorer_context_menu_items().contains(&"Select for Compare"));
        assert!(!app.explorer_context_menu_items().contains(&"Compare with Selected"));

        for (path, expected) in [(&target, "right\n"), (&third, "third\n")] {
            choose(&mut app, path, "Compare with Selected");
            app.wait_for_background(std::time::Duration::from_secs(5), |app| {
                app.compare_diff_target_version.is_some() && !app.bg_diff_refresh_running
            });
            assert!(app.should_render_diff_for_active_editor());
            assert_eq!(app.compare_base_content.as_deref(), Some("left\n"));
            assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(path));
            assert_eq!(app.compare_diff_lines, crate::app::compute_file_diff("left\n", expected));
            assert_eq!(app.selected_compare_path.as_ref(), Some(&base));
        }
    }

    /// Create a fresh subdirectory for folder browser tests (isolated from App::new side effects)
    fn make_browse_dir(parent: &std::path::Path) -> PathBuf {
        let p = parent.join("browse");
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn wait_for_folder_browser(app: &mut App) {
        app.wait_for_background(std::time::Duration::from_secs(2), |app| {
            !app.folder_browser_loading
        });
    }

    #[test]
    fn refresh_folder_browser_lists_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let browse = make_browse_dir(dir.path());
        std::fs::create_dir(browse.join("alpha")).unwrap();
        std::fs::create_dir(browse.join("beta")).unwrap();
        std::fs::write(browse.join("file.txt"), "").unwrap();

        let mut app = App::new(dir.path().to_path_buf());
        app.folder_browser_path = browse;
        app.refresh_folder_browser();
        wait_for_folder_browser(&mut app);

        assert_eq!(app.folder_browser_entries.len(), 2);
        assert_eq!(app.folder_browser_selected, 0);
    }

    #[test]
    fn refresh_folder_browser_sorts_alphabetically() {
        let dir = tempfile::tempdir().expect("tempdir");
        let browse = make_browse_dir(dir.path());
        std::fs::create_dir(browse.join("zulu")).unwrap();
        std::fs::create_dir(browse.join("alpha")).unwrap();

        let mut app = App::new(dir.path().to_path_buf());
        app.folder_browser_path = browse;
        app.refresh_folder_browser();
        wait_for_folder_browser(&mut app);

        let names: Vec<String> = app
            .folder_browser_entries
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["alpha", "zulu"]);
    }

    #[test]
    fn refresh_folder_browser_excludes_hidden_dirs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let browse = make_browse_dir(dir.path());
        std::fs::create_dir(browse.join(".hidden")).unwrap();
        std::fs::create_dir(browse.join("visible")).unwrap();

        let mut app = App::new(dir.path().to_path_buf());
        app.folder_browser_path = browse;
        app.refresh_folder_browser();
        wait_for_folder_browser(&mut app);

        assert_eq!(app.folder_browser_entries.len(), 1);
    }

    #[test]
    fn refresh_folder_browser_clears_previous_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let browse = make_browse_dir(dir.path());
        std::fs::create_dir(browse.join("a")).unwrap();

        let mut app = App::new(dir.path().to_path_buf());
        app.folder_browser_path = browse;
        app.folder_browser_entries = vec![PathBuf::from("old")];
        app.folder_browser_selected = 5;
        app.folder_browser_scroll = 3;
        app.refresh_folder_browser();
        wait_for_folder_browser(&mut app);

        assert_eq!(app.folder_browser_selected, 0);
        assert_eq!(app.folder_browser_scroll, 0);
        assert_eq!(app.folder_browser_entries.len(), 1);
    }

    #[test]
    fn folder_browser_enter_navigates_into_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let browse = make_browse_dir(dir.path());
        let sub = browse.join("sub");
        std::fs::create_dir(&sub).unwrap();

        let mut app = App::new(dir.path().to_path_buf());
        app.folder_browser_path = browse;
        app.refresh_folder_browser();
        wait_for_folder_browser(&mut app);
        // 0 is ".." (parent entry since browse has parent), 1 is first real dir
        app.folder_browser_selected = 1;
        app.folder_browser_enter();
        wait_for_folder_browser(&mut app);

        assert_eq!(app.folder_browser_path, sub);
    }

    #[test]
    fn folder_browser_enter_at_parent_navigates_up() {
        let dir = tempfile::tempdir().expect("tempdir");
        let browse = make_browse_dir(dir.path());

        let mut app = App::new(dir.path().to_path_buf());
        app.folder_browser_path = browse.clone();
        app.folder_browser_selected = 0; // ".." (parent entry)
        app.folder_browser_enter();
        wait_for_folder_browser(&mut app);

        assert_eq!(app.folder_browser_path, dir.path());
    }

    #[test]
    fn folder_browser_up_navigates_to_parent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let browse = make_browse_dir(dir.path());

        let mut app = App::new(dir.path().to_path_buf());
        app.folder_browser_path = browse;
        app.folder_browser_up();
        wait_for_folder_browser(&mut app);

        assert_eq!(app.folder_browser_path, dir.path());
        assert_eq!(app.change_dir_query, dir.path().display().to_string());
    }

    #[test]
    fn folder_browser_started_from_dot_can_navigate_above_launch_directory() {
        let launch_dir = std::env::current_dir().expect("launch directory");
        let parent = launch_dir.parent().expect("launch directory parent");
        let mut app = App::new(PathBuf::from("."));
        app.folder_browser_path = PathBuf::from(".");

        app.refresh_folder_browser();
        wait_for_folder_browser(&mut app);
        assert_eq!(app.folder_browser_path, launch_dir);

        app.folder_browser_up();
        wait_for_folder_browser(&mut app);
        assert_eq!(app.folder_browser_path, parent);
        assert_ne!(app.folder_browser_path, PathBuf::new());
    }

    #[test]
    fn folder_browser_empty_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let browse = make_browse_dir(dir.path());

        let mut app = App::new(dir.path().to_path_buf());
        app.folder_browser_path = browse;
        app.refresh_folder_browser();
        wait_for_folder_browser(&mut app);

        assert!(app.folder_browser_entries.is_empty());
    }

    #[test]
    fn workspace_context_new_file_creates_at_the_explorer_root() {
        let workspace = tempfile::tempdir().expect("empty workspace");
        let mut app = App::new(workspace.path().to_path_buf());
        app.open_explorer_workspace_context_menu(4, 8);
        let new_file = app
            .explorer_context_menu_items()
            .iter()
            .position(|item| *item == "New File")
            .expect("New File action");

        app.execute_explorer_context(new_file);

        assert!(workspace.path().join("new file").is_file());
        assert!(app.creating_new_file);
        assert_eq!(
            app.rename.target.as_deref(),
            Some(workspace.path().join("new file").as_path())
        );
    }

    #[test]
    fn workspace_context_new_folder_creates_at_the_explorer_root() {
        let workspace = tempfile::tempdir().expect("empty workspace");
        let mut app = App::new(workspace.path().to_path_buf());
        app.open_explorer_workspace_context_menu(4, 8);
        let new_folder = app
            .explorer_context_menu_items()
            .iter()
            .position(|item| *item == "New Folder")
            .expect("New Folder action");

        app.execute_explorer_context(new_folder);

        assert!(workspace.path().join("new folder").is_dir());
        assert!(app.creating_new_folder);
        assert_eq!(
            app.rename.target.as_deref(),
            Some(workspace.path().join("new folder").as_path())
        );
    }

    #[test]
    fn execute_explorer_context_out_of_bounds_returns_false() {
        let mut app = App::new(PathBuf::from("."));
        assert!(!app.execute_explorer_context(999));
    }

    #[test]
    fn explorer_new_file_can_retry_invalid_names_and_filesystem_errors() {
        for rejected_name in ["invalid/name", "occupied"] {
            let workspace = tempfile::tempdir().expect("workspace");
            std::fs::create_dir(workspace.path().join("occupied")).unwrap();
            let mut app = App::new(workspace.path().to_path_buf());
            app.open_explorer_workspace_context_menu(4, 8);
            let action = app.explorer_context_menu_items().iter().position(|&label| label == "New File").unwrap();
            app.execute_explorer_context(action);
            let placeholder = workspace.path().join("new file");

            app.rename.query = rejected_name.to_string();
            app.execute_rename();

            assert!(app.rename.active);
            assert!(app.creating_new_file);
            assert_eq!(app.rename.target.as_ref(), Some(&placeholder));
            assert!(placeholder.is_file());

            app.rename.query = "created.txt".to_string();
            app.execute_rename();

            assert!(!app.rename.active);
            assert!(!app.creating_new_file);
            assert!(app.rename.target.is_none());
            assert!(!placeholder.exists());
            let created = workspace.path().join("created.txt");
            assert!(created.is_file());
            assert!(app.tabs.iter().any(|tab| tab.editor.file_path.as_ref() == Some(&created)));
        }
    }

    #[test]
    fn explorer_new_file_can_cancel_after_invalid_name() {
        let workspace = tempfile::tempdir().expect("workspace");
        let mut app = App::new(workspace.path().to_path_buf());
        app.open_explorer_workspace_context_menu(4, 8);
        let action = app.explorer_context_menu_items().iter().position(|&label| label == "New File").unwrap();
        app.execute_explorer_context(action);
        app.rename.query = "invalid/name".to_string();
        app.execute_rename();

        app.cancel_rename();

        assert!(!workspace.path().join("new file").exists());
        assert!(!app.rename.active);
        assert!(!app.creating_new_file);
    }

    #[test]
    fn edge_case_explorer_rename_never_replaces_an_existing_file() {
        for creating in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let source = dir.path().join("original.txt");
            let destination = dir.path().join("important.txt");
            std::fs::write(&source, "source contents").unwrap();
            std::fs::write(&destination, "keep this file").unwrap();
            let mut app = App::new(dir.path().to_path_buf());
            app.begin_rename(source.clone(), "important.txt".into(), true, creating);
            app.creating_new_file = creating;
            app.execute_rename();
            assert_eq!(std::fs::read_to_string(&destination).unwrap(), "keep this file");
            assert_eq!(std::fs::read_to_string(&source).unwrap(), "source contents");
            assert!(app.rename.active, "the rejected name remains editable");
            app.rename.query = "accepted.txt".into();
            app.execute_rename();
            assert!(!app.rename.active);
            assert_eq!(std::fs::read_to_string(dir.path().join("accepted.txt")).unwrap(), "source contents");
        }
    }

    #[test]
    fn edge_case_explorer_rename_of_a_disappeared_source_never_creates_or_truncates_files() {
        for occupied in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let source = dir.path().join("removed.txt");
            let destination = dir.path().join("destination.txt");
            if occupied { std::fs::write(&destination, "keep this file").unwrap(); }
            let mut app = App::new(dir.path().to_path_buf());
            app.begin_rename(source, "destination.txt".into(), true, false);
            app.execute_rename();
            if occupied {
                assert_eq!(std::fs::read_to_string(&destination).unwrap(), "keep this file");
            } else {
                assert!(!destination.exists(), "a failed rename must not create an unrelated file");
            }
            assert!(app.rename.active);
        }
    }

    #[test]
    fn edge_case_explorer_rename_rejects_invalid_names_without_touching_the_source() {
        let workspace = tempfile::tempdir().unwrap();
        let source = workspace.path().join("original.txt");
        std::fs::write(&source, "keep").unwrap();
        let mut app = App::new(workspace.path().to_path_buf());
        let mut names = vec![".", "..", "bad\0.txt", "nested/name.txt"];
        if cfg!(windows) { names.extend(["trailing.", "NUL.txt", "con", "lpt1.log", "COM²", "bad\u{1}.txt"]); }
        for name in names {
            app.begin_rename(source.clone(), name.into(), true, false);
            app.execute_rename();
            assert!(app.rename.active, "{name:?}");
            assert_eq!(std::fs::read_to_string(&source).unwrap(), "keep");
            assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn edge_case_rename_no_replace_handles_same_name_case_changes_and_directories() {
        let workspace = tempfile::tempdir().unwrap();
        let source = workspace.path().join("MixedCase.txt");
        let destination = workspace.path().join("mixedcase.txt");
        std::fs::write(&source, "keep").unwrap();
        super::rename_no_replace(&source, &source).unwrap();
        super::rename_no_replace(&source, &destination).unwrap();
        assert_eq!(std::fs::read_to_string(&destination).unwrap(), "keep");
        assert_eq!(std::fs::read_dir(workspace.path()).unwrap().next().unwrap().unwrap().file_name(), "mixedcase.txt");
        assert!(super::rename_no_replace(&workspace.path().join("missing"), &workspace.path().join("missing")).is_err());

        let source = workspace.path().join("old-folder");
        let destination = workspace.path().join("new-folder");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("child.txt"), "child").unwrap();
        super::rename_no_replace(&source, &destination).unwrap();
        assert!(!source.exists());
        assert_eq!(std::fs::read_to_string(destination.join("child.txt")).unwrap(), "child");
        std::fs::create_dir(&source).unwrap();
        assert!(super::rename_no_replace(&source, &destination).is_err());
        assert!(source.is_dir());
        assert_eq!(std::fs::read_to_string(destination.join("child.txt")).unwrap(), "child");
    }

    #[test]
    fn edge_case_rename_no_replace_preserves_the_loser_of_a_destination_race() {
        let workspace = tempfile::tempdir().unwrap();
        let sources = [workspace.path().join("first"), workspace.path().join("second")];
        let destination = workspace.path().join("destination");
        for (index, source) in sources.iter().enumerate() { std::fs::write(source, index.to_string()).unwrap(); }
        let barrier = std::sync::Barrier::new(2);
        let results = std::thread::scope(|scope| {
            let workers: Vec<_> = sources.iter().map(|source| {
                let barrier = &barrier;
                let destination = &destination;
                scope.spawn(move || {
                    barrier.wait();
                    super::rename_no_replace(source, destination)
                })
            }).collect();
            workers.into_iter().map(|worker| worker.join().unwrap()).collect::<Vec<_>>()
        });
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let winner = results.iter().position(|result| result.is_ok()).unwrap();
        assert_eq!(std::fs::read_to_string(&destination).unwrap(), winner.to_string());
        assert!(!sources[winner].exists());
        assert_eq!(std::fs::read_to_string(&sources[1 - winner]).unwrap(), (1 - winner).to_string());
    }

    #[cfg(windows)]
    #[test]
    fn edge_case_rename_no_replace_preserves_long_unicode_paths() {
        let workspace = tempfile::tempdir().unwrap();
        let mut directory = workspace.path().to_path_buf();
        for _ in 0..6 { directory.push("long_directory_界_012345678901234567890123456789"); }
        std::fs::create_dir_all(&directory).unwrap();
        let source = directory.join("before_🙂.txt");
        let destination = directory.join("after_界.txt");
        std::fs::write(&source, "long path").unwrap();
        super::rename_no_replace(&source, &destination).unwrap();
        assert!(!source.exists());
        assert_eq!(std::fs::read_to_string(&destination).unwrap(), "long path");
    }

    #[cfg(unix)]
    #[test]
    fn edge_case_rename_no_replace_moves_dangling_symlinks_without_following_them() {
        let workspace = tempfile::tempdir().unwrap();
        let source = workspace.path().join("source");
        let destination = workspace.path().join("destination");
        std::os::unix::fs::symlink("missing", &source).unwrap();
        super::rename_no_replace(&source, &destination).unwrap();
        assert_eq!(std::fs::read_link(&destination).unwrap(), PathBuf::from("missing"));
        std::fs::write(&source, "keep source").unwrap();
        assert!(super::rename_no_replace(&source, &destination).is_err());
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "keep source");
        assert_eq!(std::fs::read_link(&destination).unwrap(), PathBuf::from("missing"));
    }
}
