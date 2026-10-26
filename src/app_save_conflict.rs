//! Explicit resolution of a failed save. Disk reads and diff construction stay
//! on workers; neither action mutates the live document or its undo history.
use crate::{
    app::{App, DiffBaseRevision, DiffLabels, TabKind},
    background::{BackgroundResult, CancellationToken, PreparedDiffRefresh},
    editor::{BackgroundSaveRequest, LoadedTextFile},
};
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone)]
struct Target {
    path: PathBuf,
    document: u64,
    generation: u64,
}
pub(crate) struct Conflict {
    target: Target,
    comparing: Option<CancellationToken>,
}
impl Conflict {
    pub(crate) fn busy(&self) -> bool {
        self.comparing.is_some()
    }
}
impl Drop for Conflict {
    fn drop(&mut self) {
        if let Some(cancel) = &self.comparing {
            cancel.cancel();
        }
    }
}
pub struct DiffResult {
    target: Target,
    task: CancellationToken,
    source: ropey::Rope,
    result: Result<Prepared, String>,
}
struct Prepared {
    diff: PreparedDiffRefresh,
    encoding: &'static str,
    missing: bool,
}

fn load_disk(
    path: &Path,
    remote: Option<&crate::remote::workspace::Workspace>,
    cancel: &CancellationToken,
) -> io::Result<Option<LoadedTextFile>> {
    let loaded = if let Some(remote) = remote {
        remote
            .open_location(path, cancel, |_, _| {})
            .and_then(|location| match location {
                crate::remote::workspace::Location::File(loaded) => Ok(loaded),
                _ => Err(io::Error::other(
                    "The file path now refers to a directory. Your edits remain open.",
                )),
            })
    } else {
        crate::fs_ops::open_document_source(path, false).and_then(|(file, metadata)| {
            crate::editor::decode_text_source(file, metadata, path, Some(cancel))
        })
    };
    match loaded {
        Ok(loaded) if loaded.encoding == "Binary" => Err(io::Error::other(
            "The file on disk is now binary and cannot be compared or overwritten as text. Save your edits under another name.",
        )),
        Ok(loaded) => Ok(Some(loaded)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Only an explicit Overwrite action calls this, within the existing save
/// worker. The normal atomic writer still validates these exact disk bytes
/// immediately before replacement. A missing file uses no-replace publication.
pub(crate) fn prepare_overwrite(
    request: &mut BackgroundSaveRequest,
    remote: Option<&crate::remote::workspace::Workspace>,
) -> io::Result<Option<(Arc<crate::remote::Checkpoint>, ropey::Rope, &'static str)>> {
    let loaded = load_disk(&request.path, remote, &CancellationToken::new())?;
    request.fail_if_exists = loaded.is_none();
    request.expected_disk = loaded
        .as_ref()
        .map(|loaded| crate::editor::DiskSaveBaseline {
            rope: loaded.rope.clone(),
            encoding: loaded.encoding,
        });
    Ok(loaded.and_then(|loaded| {
        loaded
            .remote_checkpoint
            .map(|checkpoint| (checkpoint, loaded.rope, loaded.encoding))
    }))
}

impl App {
    pub(crate) fn save_conflict_path(&self) -> Option<String> {
        self.save_conflict
            .as_ref()
            .map(|conflict| self.display_path(&conflict.target.path))
    }
    fn conflict_editor(&self, target: &Target) -> Option<&crate::editor::Editor> {
        if target.generation != self.workspace_generation {
            return None;
        }
        self.tabs
            .iter()
            .chain(self.groups.iter().flat_map(|group| &group.tabs))
            .find(|tab| {
                tab.kind == TabKind::File
                    && tab.editor.save_identity == target.document
                    && tab.editor.file_path.as_ref() == Some(&target.path)
            })
            .map(|tab| &tab.editor)
    }
    pub(crate) fn show_file_save_error(
        &mut self,
        path: &Path,
        document: u64,
        generation: u64,
        error: io::Error,
    ) {
        let conflict = crate::editor::save::is_conflict(&error)
            || error.kind() == io::ErrorKind::AlreadyExists;
        self.show_save_error(format!(
            "Could not save {}: {error}",
            self.display_path(path)
        ));
        let target = Target {
            path: path.to_owned(),
            document,
            generation,
        };
        if conflict && self.conflict_editor(&target).is_some() {
            self.save_conflict = Some(Conflict {
                target,
                comparing: None,
            });
        }
    }
    pub(crate) fn dismiss_save_error(&mut self) {
        self.save_error = None;
        self.save_conflict = None;
    }
    pub(crate) fn overwrite_save_conflict(&mut self) {
        let Some(conflict) = &self.save_conflict else {
            return;
        };
        if conflict.busy() {
            return;
        }
        let target = conflict.target.clone();
        self.sync_duplicate_file_tabs();
        let request = self
            .conflict_editor(&target)
            .filter(|editor| !editor.read_only)
            .and_then(|editor| {
                editor.background_save_request(
                    self.trim_trailing_on_save,
                    self.insert_final_newline,
                    false,
                )
            });
        let Some(request) = request else {
            self.modal.error = Some("The original document was closed, moved, or became read-only. Your edits have not been overwritten.".into());
            return;
        };
        if self.queue_background_overwrite(request) {
            self.dismiss_save_error();
            self.notify("Saving your edits over the external version…");
        } else {
            self.modal.error = Some("The save could not start. Wait for the current file operation or connection recovery, then try again.".into());
        }
    }
    pub(crate) fn view_save_conflict(&mut self) {
        let Some(conflict) = &self.save_conflict else {
            return;
        };
        if conflict.busy() {
            return;
        }
        let target = conflict.target.clone();
        self.sync_duplicate_file_tabs();
        let Some(editor) = self.conflict_editor(&target) else {
            self.modal.error =
                Some("The original document was closed or moved. Nothing was overwritten.".into());
            return;
        };
        let source = editor.rope.clone();
        let cancel = CancellationToken::new();
        self.save_conflict.as_mut().unwrap().comparing = Some(cancel.clone());
        self.modal.error = None;
        let remote = self
            .remote_workspace
            .clone()
            .filter(|_| crate::remote::workspace::is_view_key(&target.path));
        let sender = self.background.sender.clone();
        let activity_path = target.path.clone();
        self.background
            .read_for_file("save-conflict-diff", &activity_path, true, move || {
                let result = (|| {
                    if cancel.is_cancelled() {
                        return Err(io::ErrorKind::Interrupted.into());
                    }
                    let disk = load_disk(&target.path, remote.as_ref(), &cancel)?;
                    let missing = disk.is_none();
                    let encoding = disk.as_ref().map_or("UTF-8", |disk| disk.encoding);
                    let disk_text: Arc<str> =
                        disk.map_or_else(|| "".into(), |disk| disk.rope.to_string().into());
                    if cancel.is_cancelled() {
                        return Err(io::ErrorKind::Interrupted.into());
                    }
                    let lines = crate::app::compute_file_diff(&disk_text, &source.to_string());
                    let render_cache = crate::app::DiffRenderCache::new(&lines);
                    let markers = crate::app_git_display::target_line_markers_from_compare_diff(
                        &lines,
                        source.len_lines(),
                    );
                    Ok(Prepared {
                        encoding,
                        missing,
                        diff: PreparedDiffRefresh {
                            base_content: disk_text,
                            diff_lines: lines,
                            render_cache,
                            target_line_markers: markers,
                        },
                    })
                })()
                .map_err(|error: io::Error| error.to_string());
                if !cancel.is_cancelled() {
                    let _ = sender.send(BackgroundResult::SaveConflictDiff(DiffResult {
                        target,
                        task: cancel,
                        source,
                        result,
                    }));
                }
            });
    }
    pub(crate) fn apply_save_conflict_diff(&mut self, result: DiffResult) -> bool {
        if self
            .save_conflict
            .as_ref()
            .and_then(|conflict| conflict.comparing.as_ref())
            .is_none_or(|task| !task.same_task(&result.task))
        {
            return false;
        }
        self.save_conflict.as_mut().unwrap().comparing = None;
        self.sync_duplicate_file_tabs();
        let Some(editor) = self
            .conflict_editor(&result.target)
            .filter(|editor| editor.rope.is_instance(&result.source))
        else {
            self.modal.error = Some("The document changed while comparing. Choose View Diff again to compare its latest edits.".into());
            return true;
        };
        let language = Some(editor.language_name().to_owned());
        let encoding = editor.encoding;
        let prepared = match result.result {
            Ok(prepared) => prepared,
            Err(error) => {
                self.modal.error = Some(format!("Could not compare with disk: {error}"));
                return true;
            }
        };
        self.dismiss_save_error();
        self.confirm_close_active = false;
        self.exit_confirm_active = false;
        self.exit_save_and_quit = false;
        // Opening a conflict comparison must not trigger another focus-change
        // save of the already rejected document.
        self.create_snapshot_diff_tab();
        let limit = self.large_file_threshold_mb.saturating_mul(1024 * 1024);
        if let Some(editor) = self.editor_mut() {
            editor.file_path = Some(result.target.path.clone());
            editor.language_override = language;
            editor.file_size = result.source.len_bytes() as u64;
            editor.rope = result.source;
            editor.saved_rope = Some(editor.rope.clone());
            editor.encoding = encoding;
            editor.saved_encoding = encoding;
            editor.modified = false;
            editor.update_large_file_mode(limit);
            editor.mark_syntax_dirty_range(0, editor.rope.len_lines());
        }
        self.compare_base_content = Some(prepared.diff.base_content);
        self.compare_diff_target_version = self.editor().map(|editor| editor.content_version);
        self.compare_diff_base_revision = Some(DiffBaseRevision::Snapshot);
        self.diff_labels = Some(DiffLabels::new(
            if prepared.missing {
                "On disk (deleted)".into()
            } else {
                format!("On disk ({})", prepared.encoding)
            },
            format!("Your unsaved edits ({encoding})"),
        ));
        self.diff_target_path = Some(result.target.path.clone());
        self.diff_tab_label = Some(format!(
            "{} (Save conflict)",
            result
                .target
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        ));
        self.diff_mode = self.preferred_diff_mode;
        self.apply_prepared_compare_diff(
            prepared.diff.diff_lines,
            prepared.diff.render_cache,
            prepared.diff.target_line_markers,
        );
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordinary_errors_never_grant_overwrite_and_dismissed_comparisons_cannot_open_tabs() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file.txt");
        std::fs::write(&path, "disk\n").unwrap();
        let mut editor = crate::editor::Editor::new();
        editor.open_file(&path).unwrap();
        editor.insert_char('X');
        let mut app = App::new_deferred(root.path().into());
        app.tabs = vec![crate::app::Tab::new_file(editor)];
        app.active_tab = 0;
        let document = app.editor().unwrap().save_identity;
        app.show_file_save_error(
            &path,
            document,
            app.workspace_generation,
            io::Error::other("changed on disk, but not a structured conflict"),
        );
        assert!(app.save_conflict.is_none());
        app.show_file_save_error(
            &path,
            document,
            app.workspace_generation,
            crate::editor::save::conflict(&path),
        );
        let task = CancellationToken::new();
        let target = app.save_conflict.as_ref().unwrap().target.clone();
        app.save_conflict.as_mut().unwrap().comparing = Some(task.clone());
        let source = app.editor().unwrap().rope.clone();
        app.dismiss_save_error();
        assert!(task.is_cancelled());
        assert!(!app.apply_save_conflict_diff(DiffResult {
            target,
            task,
            source,
            result: Err("stale result".into())
        }));
        assert_eq!(app.tabs.len(), 1);
        assert!(app.save_error.is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "disk\n");
    }
    #[test]
    fn repeated_compare_coalesces_and_cancel_keeps_the_original_buffer() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file.txt");
        std::fs::write(&path, "disk\n").unwrap();
        let mut editor = crate::editor::Editor::new();
        editor.open_file(&path).unwrap();
        editor.insert_char('X');
        let mut app = App::new_deferred(root.path().into());
        app.tabs = vec![crate::app::Tab::new_file(editor)];
        app.active_tab = 0;
        app.show_file_save_error(
            &path,
            app.editor().unwrap().save_identity,
            app.workspace_generation,
            crate::editor::save::conflict(&path),
        );
        app.view_save_conflict();
        let task = app
            .save_conflict
            .as_ref()
            .unwrap()
            .comparing
            .clone()
            .unwrap();
        app.view_save_conflict();
        assert!(
            app.save_conflict
                .as_ref()
                .unwrap()
                .comparing
                .as_ref()
                .unwrap()
                .same_task(&task)
        );
        crate::ui_modal::handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyModifiers::NONE,
            ),
        );
        assert!(task.is_cancelled());
        assert!(app.save_error.is_none());
        assert_eq!(app.editor().unwrap().rope, "Xdisk\n");
        assert!(app.editor().unwrap().modified);
    }
}
