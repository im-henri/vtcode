//! Watch notifications are hints. Validate bytes beside the remote file without
//! sending saved copies over SSH or changing a dirty local document.
use super::*;
use crate::remote::workspace::{Baseline, Validation};

impl App {
    pub(crate) fn remote_save_needs_validation(&self, path: &Path) -> bool {
        self.remote_change_warnings.contains(path) || self.remote_changes_deferred.contains(path)
            || self.pending_file_reloads.contains(path)
    }
    pub(crate) fn recheck_deferred_remote_file(&mut self, path: &Path) {
        if self.remote_changes_deferred.remove(path) {
            self.queue_remote_file_change(path.to_owned());
        }
    }
    pub(crate) fn prune_remote_file_changes(&mut self) {
        if self.remote_change_warnings.is_empty() && self.remote_changes_deferred.is_empty() {
            return;
        }
        let open: rustc_hash::FxHashSet<_> = self
            .tabs
            .iter()
            .chain(self.groups.iter().flat_map(|group| &group.tabs))
            .filter_map(|tab| tab.editor.file_path.as_deref())
            .collect();
        self.remote_change_warnings
            .retain(|path| open.contains(path.as_path()));
        self.remote_changes_deferred
            .retain(|path| open.contains(path.as_path()));
    }

    pub(crate) fn forget_remote_file_changes(&mut self, path: &Path) {
        self.remote_change_warnings.remove(path);
        self.remote_changes_deferred.remove(path);
    }

    pub(crate) fn queue_remote_file_change(&mut self, path: PathBuf) {
        if self.remote_connection.checking.contains(&path) {
            // Revalidation owns the baseline until its result is applied.
            self.remote_changes_deferred.insert(path);
            return;
        }
        let Some(remote) = self.remote_workspace.clone() else {
            return;
        };
        let mut snapshot = None;
        for tab in self
            .tabs
            .iter_mut()
            .chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs))
        {
            if tab.kind != TabKind::File {
                continue;
            }
            tab.editor.refresh_document();
            if tab.editor.file_path.as_ref() != Some(&path) || tab.editor.bg_loading {
                continue;
            }
            snapshot = Some((
                tab.editor.save_identity,
                tab.editor.modified,
                tab.editor
                    .remote_checkpoint
                    .as_ref()
                    .zip(tab.editor.saved_rope.as_ref())
                    .map(|(checkpoint, rope)| Baseline {
                        checkpoint: checkpoint.clone(),
                        rope: rope.clone(),
                        encoding: tab.editor.saved_encoding,
                    }),
            ));
            break;
        }
        let Some((document, dirty, baseline)) = snapshot else {
            self.remote_change_warnings.remove(&path);
            return;
        };
        if self.pending_file_saves.contains_key(&path) {
            self.remote_changes_deferred.insert(path);
            return;
        }
        if !dirty {
            self.queue_file_reload(path);
            return;
        }
        let Some(baseline) = baseline else {
            return;
        };
        if !self.pending_file_reloads.insert(path.clone()) {
            self.pending_file_reload_again.insert(path);
            return;
        }
        let generation = self.workspace_generation;
        let sender = self.background.sender.clone();
        self.background
            .read_for_file("remote-file-validation", &path.clone(), false, move || {
                let result = remote
                    .validate_saved(&path, &baseline)
                    .map_err(|error| error.to_string());
                let _ = sender.send(crate::background::BackgroundResult::RemoteFileValidated {
                    generation,
                    path,
                    document,
                    baseline,
                    result,
                });
            });
    }

    pub(crate) fn finish_remote_file_validation(
        &mut self,
        generation: u64,
        path: PathBuf,
        document: u64,
        baseline: Baseline,
        result: Result<Validation, String>,
    ) -> bool {
        if generation != self.workspace_generation {
            return false;
        }
        self.pending_file_reloads.remove(&path);
        if self.pending_file_saves.contains_key(&path) {
            self.remote_changes_deferred.insert(path.clone());
            self.pending_file_reload_again.remove(&path);
            return false;
        }
        if self.pending_file_reload_again.remove(&path) {
            self.queue_remote_file_change(path);
            return false;
        }
        let mut reload = false;
        let mut dirty = false;
        let mut obsolete = false;
        let mut matched = false;
        for tab in self
            .tabs
            .iter_mut()
            .chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs))
        {
            if tab.kind != TabKind::File {
                continue;
            }
            tab.editor.refresh_document();
            if tab.editor.file_path.as_ref() != Some(&path) || tab.editor.save_identity != document
            {
                continue;
            }
            if tab.editor.saved_encoding != baseline.encoding
                || !tab
                    .editor
                    .saved_rope
                    .as_ref()
                    .is_some_and(|saved| saved.is_instance(&baseline.rope))
            {
                obsolete = true;
                continue;
            }
            matched = true;
            if let Ok(validation) = &result {
                tab.editor.last_disk_modified = validation.modified;
                tab.editor.file_size = validation.size;
                tab.editor.publish_document();
                if validation.changed {
                    dirty |= tab.editor.modified;
                    reload |= !tab.editor.modified;
                }
            }
        }
        if obsolete {
            self.queue_remote_file_change(path.clone());
        }
        if !matched {
            return false;
        }
        match result {
            Ok(validation) => {
                if !validation.changed {
                    self.remote_change_warnings.remove(&path);
                }
                if reload {
                    self.queue_file_reload(path.clone());
                }
                if dirty && self.remote_change_warnings.insert(path.clone()) {
                    self.status_message = format!(
                        "{} changed on disk; your unsaved edits are preserved.",
                        self.display_path(&path)
                    );
                    self.notify_warn(self.status_message.clone());
                    return true;
                }
            }
            Err(error) => {
                // A notification can race deletion. One metadata check confirms
                // that case without changing text or treating transport loss as deletion.
                self.queue_file_disk_checks(std::iter::once(path.clone()));
                if self.remote_change_warnings.insert(path.clone()) {
                    self.notify_warn(format!(
                        "Could not check {}: {error}",
                        self.display_path(&path)
                    ));
                    return true;
                }
            }
        }
        false
    }
}
