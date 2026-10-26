//! Workspace edits wait for asynchronous file loading and validate the whole batch.
use crate::{app::App, app_lsp_edits, lsp::LspManager};
use lsp_types::{DocumentChangeOperation, DocumentChanges, TextEdit, WorkspaceEdit};
use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, Instant},
};

type DocumentEdits = (PathBuf, Option<i32>, Vec<TextEdit>);

pub(crate) struct PendingWorkspaceEdit {
    documents: Vec<DocumentEdits>,
    versions: HashMap<PathBuf, u64>,
    reply: Option<(u32, serde_json::Value)>,
    command: Option<(PathBuf, lsp_types::Command)>,
    title: String,
    queued: Instant,
}

fn documents(edit: WorkspaceEdit) -> Result<Vec<DocumentEdits>, String> {
    let mut result = Vec::new();
    if let Some(changes) = edit.document_changes {
        let changes = match changes {
            DocumentChanges::Edits(edits) => edits,
            DocumentChanges::Operations(operations) => operations
                .into_iter()
                .map(|operation| match operation {
                    DocumentChangeOperation::Edit(edit) => Ok(edit),
                    DocumentChangeOperation::Op(_) => Err(
                        "This code action requires unsupported file creation, deletion or renaming"
                            .to_owned(),
                    ),
                })
                .collect::<Result<Vec<_>, _>>()?,
        };
        for document in changes {
            let path = document
                .text_document
                .uri
                .to_file_path()
                .map_err(|_| "Workspace edits require a local file")?;
            let edits = document
                .edits
                .into_iter()
                .map(|edit| match edit {
                    lsp_types::OneOf::Left(edit) => edit,
                    lsp_types::OneOf::Right(edit) => edit.text_edit,
                })
                .collect();
            result.push((path, document.text_document.version, edits));
        }
    } else if let Some(changes) = edit.changes {
        for (uri, edits) in changes {
            result.push((
                uri.to_file_path()
                    .map_err(|_| "Workspace edits require a local file")?,
                None,
                edits,
            ));
        }
    }
    // Repeated entries use sequential versions; do not apply them as a single snapshot.
    let mut paths = std::collections::HashSet::new();
    if result
        .iter()
        .any(|(path, _, _)| !paths.insert(path.clone()))
    {
        return Err("Workspace edit contains repeated versions of the same file".into());
    }
    Ok(result)
}

impl LspManager {
    pub(crate) fn reply_workspace_edit(
        &mut self,
        reply: Option<(u32, serde_json::Value)>,
        result: &Result<usize, String>,
    ) {
        if let Some((process, id)) = reply
            && let Some(client) = self
                .clients
                .iter_mut()
                .find(|client| client.child.id() == process)
        {
            let response = match result {
                Ok(_) => serde_json::json!({"applied":true}),
                Err(error) => serde_json::json!({"applied":false,"failureReason":error}),
            };
            let _ = client.send_response(id, response);
        }
    }

    pub(crate) fn execute_command(
        &mut self,
        path: &std::path::Path,
        command: &lsp_types::Command,
    ) -> bool {
        self.clients
            .iter_mut()
            .find(|client| client.handles_file(path))
            .is_some_and(|client| {
                client
                    .send_request(
                        "workspace/executeCommand",
                        Some(serde_json::json!({
                            "command":command.command,"arguments":command.arguments
                        })),
                    )
                    .is_ok()
            })
    }
}

impl App {
    pub(crate) fn queue_lsp_workspace_edit_snapshot(
        &mut self,
        edit: WorkspaceEdit,
        title: String,
        versions: HashMap<PathBuf, u64>,
        command: Option<(PathBuf, lsp_types::Command)>,
    ) {
        let previous = self.pending_lsp_workspace_edits.len();
        self.queue_lsp_workspace_edit(edit, title, None, command);
        if self.pending_lsp_workspace_edits.len() > previous {
            let pending = self.pending_lsp_workspace_edits.back_mut().unwrap();
            for (path, version) in versions {
                if pending.documents.iter().any(|document| document.0 == path) {
                    pending.versions.insert(path, version);
                }
            }
        }
    }

    pub(crate) fn queue_lsp_workspace_edit(
        &mut self,
        edit: WorkspaceEdit,
        title: String,
        reply: Option<(u32, serde_json::Value)>,
        command: Option<(PathBuf, lsp_types::Command)>,
    ) {
        let documents = match documents(edit) {
            Ok(documents) => documents,
            Err(error) => {
                self.lsp_manager
                    .reply_workspace_edit(reply, &Err(error.clone()));
                self.notify_error(error);
                return;
            }
        };
        let mut versions = HashMap::new();
        for (path, _, _) in &documents {
            if let Some(editor) = self.find_editor_mut_by_path(path) {
                if !editor.bg_loading {
                    versions.insert(path.clone(), editor.content_version);
                }
            } else {
                self.open_file(path);
            }
        }
        self.pending_lsp_workspace_edits
            .push_back(PendingWorkspaceEdit {
                documents,
                versions,
                reply,
                command,
                title,
                queued: Instant::now(),
            });
    }

    pub(crate) fn poll_lsp_workspace_edits(&mut self) -> bool {
        let Some(mut pending) = self.pending_lsp_workspace_edits.pop_front() else {
            return false;
        };
        let mut ready = true;
        let validation: Result<(), String> = (|| {
            for (path, version, edits) in &pending.documents {
                if version.is_some_and(|version| {
                    self.lsp_manager.doc_versions.get(path) != Some(&version)
                }) {
                    return Err(
                        "The document changed before the language server edit could be applied"
                            .into(),
                    );
                }
                let editor = self
                    .find_editor_mut_by_path(path)
                    .ok_or("An edit target was closed or could not be opened")?;
                if editor.bg_loading {
                    ready = false;
                    continue;
                }
                if editor.read_only {
                    return Err("An edit target is read-only".into());
                }
                let expected = pending
                    .versions
                    .entry(path.clone())
                    .or_insert(editor.content_version);
                if *expected != editor.content_version {
                    return Err("The document changed while the edit was loading".into());
                }
                app_lsp_edits::prepare(&editor.rope, edits)?;
            }
            if pending.queued.elapsed() > Duration::from_secs(30) {
                return Err("Timed out loading files for the language server edit".into());
            }
            Ok(())
        })();
        if validation.is_ok() && !ready {
            self.pending_lsp_workspace_edits.push_front(pending);
            return false;
        }
        let result = validation.and_then(|()| {
            let mut count = 0;
            for (path, _, edits) in &pending.documents {
                count += app_lsp_edits::apply(self.find_editor_mut_by_path(path).unwrap(), edits)?;
            }
            Ok(count)
        });
        self.lsp_manager
            .reply_workspace_edit(pending.reply, &result);
        match result {
            Ok(count) => {
                self.notify(format!("{} ({count} edits)", pending.title));
                if let Some((path, command)) = pending.command
                    && !self.lsp_manager.execute_command(&path, &command)
                {
                    self.notify_error("Language server command is unavailable");
                }
            }
            Err(error) => self.notify_error(error),
        }
        if !self.pending_lsp_workspace_edits.is_empty() {
            self.background.wake_handle().notify();
        }
        true
    }
}

#[cfg(test)]
#[path = "app_lsp_workspace_edit_tests.rs"]
mod tests;
