//! Open an individual file in either filesystem without changing the workspace.
use crate::{app::App, text_input::TextInputId};
use std::path::PathBuf;

#[derive(Default)]
pub(crate) struct Draft {
    pub query: String,
    pub cursor: usize,
    pub anchor: Option<usize>,
}

#[derive(Default)]
pub(crate) struct State {
    pub active: bool,
    pub pending: bool,
    pub remote: bool,
    pub input: Draft,
    other: Draft,
}

impl App {
    pub(crate) fn begin_open_file_path(&mut self) {
        self.cancel_remote_location();
        self.clear_transient_editor_state();
        self.close_palette();
        self.quick_open_active = false;
        self.change_dir_active = false;
        self.save_as_active = false;
        self.goto_line_active = false;
        self.file_open = State {
            active: true,
            remote: self.remote_workspace.is_some(),
            ..Default::default()
        };
        self.modal = Default::default();
        self.text_input_histories.remove(&TextInputId::OpenFilePath);
        self.focus_text_input(TextInputId::OpenFilePath);
    }

    pub(crate) fn cancel_open_file_path(&mut self) {
        self.file_open.active = false;
        self.file_open.pending = false;
        self.cancel_remote_location();
        self.modal = Default::default();
    }

    pub(crate) fn open_file_path_changed(&mut self) {
        if self.file_open.pending {
            self.cancel_remote_location();
        }
        self.file_open.pending = false;
        self.modal.error = None;
    }

    pub(crate) fn switch_open_file_device(&mut self) {
        if self.remote_workspace.is_none() {
            return;
        }
        self.open_file_path_changed();
        self.file_open.remote = !self.file_open.remote;
        std::mem::swap(&mut self.file_open.input, &mut self.file_open.other);
        self.text_input_histories.remove(&TextInputId::OpenFilePath);
        self.focus_text_input(TextInputId::OpenFilePath);
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

    pub(crate) fn execute_open_file_path(&mut self) {
        if self.file_open.pending {
            return;
        }
        let input = self.file_open.input.query.trim();
        let input = input
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .unwrap_or(input);
        if input.is_empty() {
            self.modal.error = Some("Enter a file path.".into());
            return;
        }
        let path = if self.file_open.remote {
            self.remote_workspace
                .as_ref()
                .ok_or_else(|| "Remote connection is unavailable".to_owned())
                .and_then(|workspace| {
                    let text = if workspace.windows_paths() && !input.starts_with("file://") {
                        let text = input.replace('\\', "/");
                        if text.starts_with("//") {
                            format!("file:{text}")
                        } else if text.as_bytes().get(1) == Some(&b':') {
                            format!("/{text}")
                        } else {
                            text
                        }
                    } else {
                        input.to_owned()
                    };
                    workspace
                        .resolve_from_key(&self.explorer.root, &text)
                        .map_err(|error| error.to_string())
                })
        } else {
            let native = if input.starts_with("file://") {
                url::Url::parse(input)
                    .ok()
                    .and_then(|url| url.to_file_path().ok())
                    .ok_or_else(|| "Invalid local file URL".to_owned())
            } else if input.starts_with("~/") || input.starts_with("~\\") {
                std::env::var_os("USERPROFILE")
                    .or_else(|| std::env::var_os("HOME"))
                    .map(|home| PathBuf::from(home).join(&input[2..]))
                    .ok_or_else(|| "The local home folder is unavailable".to_owned())
            } else {
                Ok(PathBuf::from(input))
            };
            native.map(|path| {
                if path.is_absolute() {
                    path
                } else {
                    self.local_file_base().join(path)
                }
            })
        };
        match path {
            Ok(path) => {
                self.modal.error = None;
                self.file_open.pending = true;
                self.open_path_dialog_location(path);
            }
            Err(error) => self.modal.error = Some(error),
        }
    }
}

#[cfg(test)]
#[path = "app_open_file_path_tests.rs"]
mod tests;
