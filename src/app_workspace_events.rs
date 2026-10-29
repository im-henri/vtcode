//! Apply coalesced filesystem hints once per affected model and workspace.
use crate::app::{App, TabKind};
use crate::background::WorkspacePathChange;
use std::path::PathBuf;

impl App {
    pub(crate) fn apply_workspace_file_changes(
        &mut self, root: PathBuf, changes: Vec<WorkspacePathChange>,
        structure_changed: bool, rescan: bool, git_metadata_changed: bool,
    ) -> bool {
        let mut has_visual_changes = false;
        if root != self.explorer.root { return false; }
        self.completion_index_files_changed(&changes, rescan);
        if rescan || (structure_changed && changes.iter().any(|change| change.path.file_name().is_some_and(|name| name == ".git"))) {
            self.repositories.rescan = true;
        }
        let git_metadata_changed = self.source_control_enabled && (git_metadata_changed || rescan);
        let active_path = self.editor().and_then(|editor| editor.file_path.as_ref());
        let active_file_changed = rescan || active_path.is_some_and(|active_path| {
            changes.iter().any(|change| change.path == *active_path
                || structure_changed && active_path.starts_with(&change.path))
        });
        let blame_inputs_changed = git_metadata_changed || active_file_changed;
        let extension_discovery_changed = rescan || changes.iter().any(|change| {
            self.extension_manager
                .path_may_affect_discovery(&change.path)
        });
        // A watcher burst can contain thousands of paths (especially while a
        // compiler is active). Refresh workspace-wide state only once per burst.
        let explorer_generation = self.explorer_refresh_generation;
        // Invalidate before scheduling: path changes and Git metadata
        // in the same burst intentionally share one tree rebuild.
        if git_metadata_changed {
            self.explorer.invalidate_git_decorations();
        }
        if rescan {
            self.explorer_hover.invalidate(None);
            self.explorer_hover_info_cache.clear();
            self.explorer_line_count_cache.clear();
            self.refresh_explorer_async();
            let paths: rustc_hash::FxHashSet<_> = self.tabs.iter()
                .chain(self.groups.iter().flat_map(|group| &group.tabs))
                .filter(|tab| tab.kind == TabKind::File)
                .filter_map(|tab| tab.editor.file_path.clone()).collect();
            if self.remote_workspace.is_some() {
                // Overflow means path-level events were lost. Check
                // bytes for open models; metadata alone can miss a
                // replacement which retained its size and mtime.
                for path in paths { self.queue_remote_file_change(path); }
            } else { self.queue_file_disk_checks(paths); }
        } else if !changes.is_empty() {
            self.refresh_explorer_changed_paths(
                changes.iter().map(|change| change.path.as_path()),
                structure_changed || changes.iter().any(|change| !change.exists),
            );
        }
        if git_metadata_changed && explorer_generation == self.explorer_refresh_generation {
            // Index, config and info/exclude changes also affect
            // ignored decorations, without changing directory lists.
            self.refresh_explorer_tree_async();
        }
        if git_metadata_changed {
            if rescan { self.refresh_source_control_async(); }
            else { self.refresh_source_control_git_changed_paths(changes.iter().map(|change| change.path.clone())); }
        } else {
            self.refresh_source_control_changed_paths(changes.iter().map(|change| change.path.clone()));
        }
        if git_metadata_changed {
            self.refresh_git_log_async();
            self.refresh_git_branch_async();
        }
        if blame_inputs_changed {
            self.blame_requested_path = None;
            self.blame_documents.retain(|_, document| document.revision.is_some());
        }

        let structural_descendants: rustc_hash::FxHashSet<_> = if structure_changed && !rescan {
            let direct: rustc_hash::FxHashSet<_> = changes.iter().map(|change| change.path.as_path()).collect();
            let parents: rustc_hash::FxHashSet<_> = changes.iter().filter(|change| change.exists).map(|change| change.path.as_path()).collect();
            self.tabs.iter_mut().chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs))
                .filter_map(|tab| {
                    if tab.kind != TabKind::File { return None; }
                    tab.editor.refresh_document();
                    let path = tab.editor.file_path.as_ref()?;
                    (!direct.contains(path.as_path()) && path.ancestors().skip(1).any(|parent| parents.contains(parent)))
                        .then(|| path.clone())
                }).collect()
        } else { Default::default() };
        let mut deleted_names: Vec<String> = Vec::new();
        let mut changed_unsaved = rustc_hash::FxHashSet::default();
        let mut reload_paths = rustc_hash::FxHashSet::default();
        let mut compare_base_changed = false;
        let mut native_changes = rustc_hash::FxHashMap::default();
        let native_documents = self.remote_workspace.is_none()
            && self.tabs.iter().chain(self.groups.iter().flat_map(|group| &group.tabs))
                .any(|tab| tab.kind == TabKind::File && tab.editor.file_path.is_some());
        let mut lsp_paths = self.lsp_manager.clients.iter().any(|client| client.initialized).then(Vec::new);
        // Closed files can still affect Explorer, Git and language servers, but
        // cannot receive a remote buffer reload. Build this inventory once per burst.
        let remote_open: rustc_hash::FxHashSet<_> = if self.remote_workspace.is_some() {
            self.tabs.iter_mut().chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs))
                .filter_map(|tab| {
                    if tab.kind != TabKind::File { return None; }
                    tab.editor.refresh_document();
                    tab.editor.file_path.clone()
                }).collect()
        } else { Default::default() };
        for change in changes {
            if self.chat.rollback.defer_disk_change(&change) {
                if !self.chat.rollback.file_busy(&change.path) {
                    self.queue_file_disk_checks(std::iter::once(change.path));
                }
                continue;
            }
            self.chat.rollback.note_workspace_change(&change.path);
            self.explorer_hover.invalidate(Some(&change.path));
            self.explorer_hover_info_cache.remove(&change.path);
            self.explorer_line_count_cache.remove(&change.path);
            compare_base_changed |=
                self.compare_base_path.as_ref() == Some(&change.path);
            if let Some(paths) = &mut lsp_paths { paths.push(change.path.clone()); }
            if crate::remote::workspace::is_view_key(&change.path) && self.remote_workspace.is_some() {
                if !remote_open.contains(&change.path) && !self.remote_connection.checking.contains(&change.path) { continue; }
                if change.exists || self.pending_file_saves.contains_key(&change.path) {
                    self.queue_remote_file_change(change.path);
                } else {
                    self.queue_file_disk_checks(std::iter::once(change.path));
                }
                continue;
            }
            if self.pending_file_saves.contains_key(&change.path) { continue; }
            if !change.exists {
                if self.close_deleted_file_tabs(&change.path, false) {
                    has_visual_changes = true;
                    deleted_names.push(
                        change
                            .path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
                continue;
            }
            if native_documents { native_changes.insert(change.path.clone(), change); }
        }
        if let Some(paths) = lsp_paths {
            self.lsp_manager.notify_did_change_watched_files(paths.iter().map(PathBuf::as_path));
        }
        // One pass over open views replaces a full tab scan for every changed file.
        // Keep this after deletion handling, which can compact or remove tab groups.
        if !native_changes.is_empty() {
            for tab in self.tabs.iter_mut().chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs)) {
                tab.editor.refresh_document();
                if tab.editor.bg_loading || matches!(tab.kind,
                    TabKind::Diff(_) | TabKind::GitRevision | TabKind::CommitDetails) { continue; }
                let Some(change) = tab.editor.file_path.as_ref().and_then(|path| native_changes.get(path))
                    else { continue; };
                tab.editor.disk_unavailable = false;
                let changed = match (tab.editor.last_disk_modified, change.modified) {
                    (Some(old), Some(new)) => new != old,
                    (None, Some(_)) => true,
                    _ => false,
                } || change.len.is_some_and(|len| len != tab.editor.file_size);
                if changed {
                    tab.editor.last_disk_modified = change.modified;
                    if let Some(len) = change.len { tab.editor.file_size = len; }
                    tab.editor.publish_document();
                    if tab.editor.modified { changed_unsaved.insert(change.path.clone()); }
                    else { reload_paths.insert(change.path.clone()); }
                }
            }
        }
        if compare_base_changed {
            self.compare_diff_base_revision = None;
            self.diff_refresh_generation = self.diff_refresh_generation.wrapping_add(1);
            self.bg_diff_refresh_running = false;
            self.refresh_compare_diff();
        }
        for path in reload_paths {
            self.queue_file_reload(path);
        }
        for path in structural_descendants {
            // A directory can be atomically replaced by another
            // tree (or a regular file) without child-level events.
            if crate::remote::workspace::is_view_key(&path) { self.queue_remote_file_change(path); }
            else { self.queue_file_disk_checks(std::iter::once(path)); }
        }
        for path in changed_unsaved {
            self.status_message = format!("{} changed on disk; your unsaved edits are preserved.", path.display());
            self.notify_warn(self.status_message.clone());
            has_visual_changes = true;
        }

        // Deduplicate notifications when a file is open in multiple groups.
        deleted_names.sort();
        deleted_names.dedup();
        if deleted_names.len() == 1 {
            self.notify_warn(format!("File deleted externally: {}", deleted_names[0]));
        } else if !deleted_names.is_empty() {
            self.notify_warn(format!(
                "{} files deleted externally",
                deleted_names.len()
            ));
        }
        if extension_discovery_changed {
            self.request_extension_scan();
        }
        if blame_inputs_changed { self.refresh_git_line_status_async(); }
        has_visual_changes
    }
}

#[cfg(test)]
#[path = "app_workspace_events_tests.rs"]
mod tests;
