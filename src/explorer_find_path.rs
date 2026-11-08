use std::path::PathBuf;

use crate::{app::App, text_input::TextInputId};

impl App {
    pub(crate) fn sync_explorer_find_path(&mut self) {
        let path = self
            .explorer
            .find
            .directory
            .as_deref()
            .map_or_else(String::new, |directory| {
                if self.remote_workspace.is_some() {
                    self.display_path(directory)
                } else {
                    directory
                        .strip_prefix(&self.explorer.root)
                        .unwrap_or(directory)
                        .to_string_lossy()
                        .into_owned()
                }
            });
        let find = &mut self.explorer.find;
        find.path_cursor = path.len();
        find.path = path;
        find.path_anchor = None;
        find.path_dirty = false;
        find.path_error = None;
    }

    fn resolve_explorer_find_path(&self) -> Result<Option<PathBuf>, String> {
        let text = self.explorer.find.path.trim().trim_matches('"');
        if text.is_empty() {
            return Ok(None);
        }
        let directory = if let Some(remote) = &self.remote_workspace {
            let text = if remote.windows_paths() && !text.starts_with("file://") {
                let path = text.replace('\\', "/");
                if path.starts_with("//") {
                    format!("file:{path}")
                } else if path.as_bytes().get(1) == Some(&b':') {
                    format!("/{path}")
                } else {
                    path
                }
            } else {
                text.to_owned()
            };
            let directory = remote
                .resolve_from_key(&self.explorer.root, &text)
                .map_err(|error| error.to_string())?;
            // Host-absolute remote paths share the view-key namespace, but
            // are outside this Explorer's workspace tree.
            if remote
                .remote_path(&directory)
                .map_err(|error| error.to_string())?
                .is_absolute()
            {
                return Err("Choose a folder inside the workspace".to_owned());
            }
            directory
        } else {
            let path = if text.starts_with("file://") {
                url::Url::parse(text)
                    .ok()
                    .and_then(|url| url.to_file_path().ok())
                    .ok_or_else(|| "Invalid folder file URL".to_owned())?
            } else {
                self.explorer.root.join(text)
            };
            let root = crate::folder_browser::normalized_path(&self.explorer.root);
            let path = crate::folder_browser::normalized_path(&path);
            let relative = crate::native_paths::strip_prefix(&path, &root)
                .ok_or_else(|| "Choose a folder inside the workspace".to_owned())?;
            self.explorer.root.join(relative)
        };
        let relative = crate::native_paths::strip_prefix(&directory, &self.explorer.root)
            .ok_or_else(|| "Choose a folder inside the workspace".to_owned())?;
        Ok((!relative.as_os_str().is_empty()).then_some(directory))
    }

    pub(crate) fn apply_explorer_find_path(&mut self) {
        let directory = match self.resolve_explorer_find_path() {
            Ok(directory) => directory,
            Err(error) => {
                self.explorer.find.path_error = Some(error);
                return;
            }
        };
        let changed = self.explorer.find.directory != directory;
        self.explorer.find.directory = directory;
        self.sync_explorer_find_path();
        if changed {
            self.update_explorer_find();
        }
        self.focus_text_input(TextInputId::ExplorerFind);
    }
}
