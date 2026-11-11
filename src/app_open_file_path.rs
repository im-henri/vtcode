//! File picker entry point and local/remote path helpers.
use crate::{app::App, folder_browser::Mode, text_input::TextInputId};
use std::path::PathBuf;

impl App {
    pub(crate) fn begin_open_file_browser(&mut self) {
        self.cancel_remote_location();
        self.clear_transient_editor_state();
        self.close_palette();
        self.quick_open_active = false;
        self.save_as_active = false;
        self.goto_line_active = false;
        self.modal = Default::default();
        self.open_path_browser(Mode::OpenFile);
        self.focus_text_input(TextInputId::FolderName);
    }

    pub(crate) fn local_file_base(&self) -> PathBuf {
        if self.remote_workspace.is_some() {
            self.editor()
                .and_then(|editor| editor.file_path.as_deref())
                .filter(|path| !crate::remote::workspace::is_view_key(path))
                .and_then(|path| path.parent())
                .map(PathBuf::from)
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_default()
        } else {
            self.explorer.root.clone()
        }
    }

    pub(crate) fn editor_uses_remote_files(&self) -> bool {
        self.remote_workspace.is_some()
            && self
                .editor()
                .and_then(|editor| editor.file_path.as_deref())
                .is_none_or(crate::remote::workspace::is_view_key)
    }

    pub(crate) fn explorer_contains_file_path(&self, path: &std::path::Path) -> bool {
        if crate::remote::workspace::is_view_key(path) {
            self.remote_workspace.as_ref().is_some_and(|workspace| {
                workspace
                    .remote_path(path)
                    .is_ok_and(|path| !path.is_absolute())
            })
        } else {
            crate::native_paths::strip_prefix(path, &self.explorer.root).is_some_and(|relative| {
                // Preserve filesystem path semantics, including symlinks, while
                // keeping parent-relative external files out of the tree.
                !relative
                    .components()
                    .any(|part| part == std::path::Component::ParentDir)
            })
        }
    }
}

#[cfg(test)]
#[path = "app_open_file_path_tests.rs"]
mod tests;
