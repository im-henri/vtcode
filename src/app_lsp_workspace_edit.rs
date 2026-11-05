//! Workspace edits wait for asynchronous file loading and validate the whole batch.
use crate::{app::App, app_lsp_edits, lsp::LspManager};
use lsp_types::{DocumentChangeOperation, DocumentChanges, TextEdit, WorkspaceEdit};
use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, Instant},
};

const EDIT_TIMEOUT: Duration = Duration::from_secs(30);

struct DocumentEdits {
    path: PathBuf,
    server_version: Option<i32>,
    edits: Vec<TextEdit>,
    identity: Option<u64>,
    version: Option<u64>,
}

impl DocumentEdits {
    fn new(path: PathBuf, server_version: Option<i32>, edits: Vec<TextEdit>) -> Self {
        Self {
            path,
            server_version,
            edits,
            identity: None,
            version: None,
        }
    }
}

pub(crate) struct PendingWorkspaceEdit {
    documents: Vec<DocumentEdits>,
    #[cfg(windows)]
    unresolved_versions: HashMap<PathBuf, u64>,
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
            result.push(DocumentEdits::new(
                path,
                document.text_document.version,
                edits,
            ));
        }
    } else if let Some(changes) = edit.changes {
        for (uri, edits) in changes {
            result.push(DocumentEdits::new(
                uri.to_file_path()
                    .map_err(|_| "Workspace edits require a local file")?,
                None,
                edits,
            ));
        }
    }
    // Repeated entries use sequential versions; do not apply them as a single snapshot.
    let mut paths = std::collections::HashSet::new();
    if result.iter().any(|document| !paths.insert(&document.path)) {
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
    #[cfg(windows)]
    pub(crate) fn resolve_workspace_edit_open_alias(
        &mut self,
        request: &crate::app_tabs::native_open::Request,
        source: &crate::app_tabs::native_open::Candidate,
    ) {
        for edit in &mut self.pending_lsp_workspace_edits {
            for document in &mut edit.documents {
                if document.identity == Some(request.document) && document.path == request.path {
                    document.path.clone_from(&source.path);
                    document.identity = Some(source.document);
                    document.version = document.version
                        .or_else(|| edit.unresolved_versions.get(&source.path).copied())
                        .or(source.version);
                }
            }
        }
    }

    pub(crate) fn queue_lsp_workspace_edit_snapshot(
        &mut self,
        edit: WorkspaceEdit,
        title: String,
        mut versions: HashMap<PathBuf, u64>,
        command: Option<(PathBuf, lsp_types::Command)>,
    ) {
        let previous = self.pending_lsp_workspace_edits.len();
        self.queue_lsp_workspace_edit(edit, title, None, command);
        if self.pending_lsp_workspace_edits.len() > previous {
            let pending = self.pending_lsp_workspace_edits.back_mut().unwrap();
            for document in &mut pending.documents {
                if let Some(version) = versions.remove(&document.path) {
                    document.version = Some(version);
                }
            }
            #[cfg(windows)]
            if pending.documents.iter().any(|document| document.version.is_none()) {
                pending.unresolved_versions = versions;
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
        let mut documents = match documents(edit) {
            Ok(documents) => documents,
            Err(error) => {
                self.lsp_manager
                    .reply_workspace_edit(reply, &Err(error.clone()));
                self.notify_error(error);
                return;
            }
        };
        let snapshot = |editor: &mut crate::editor::Editor| {
            (
                editor.save_identity,
                (!editor.bg_loading).then_some(editor.content_version),
            )
        };
        for document in &mut documents {
            let editor = self
                .find_editor_mut_by_path(&document.path)
                .map(snapshot)
                .or_else(|| {
                    self.open_file(&document.path);
                    self.find_editor_mut_by_path(&document.path).map(snapshot)
                });
            if let Some((identity, version)) = editor {
                document.identity = Some(identity);
                document.version = version;
            }
        }
        self.pending_lsp_workspace_edits
            .push_back(PendingWorkspaceEdit {
                documents,
                #[cfg(windows)]
                unresolved_versions: HashMap::new(),
                reply,
                command,
                title,
                queued: Instant::now(),
            });
    }

    pub(crate) fn lsp_workspace_edit_wait(&self) -> Option<Duration> {
        self.pending_lsp_workspace_edits
            .front()
            .map(|pending| EDIT_TIMEOUT.saturating_sub(pending.queued.elapsed()))
    }

    pub(crate) fn poll_lsp_workspace_edits(&mut self) -> bool {
        let Some(mut pending) = self.pending_lsp_workspace_edits.pop_front() else {
            return false;
        };
        let snapshots = (|| -> Result<Option<Vec<ropey::Rope>>, String> {
            if pending.queued.elapsed() >= EDIT_TIMEOUT {
                return Err("Timed out loading files for the language server edit".into());
            }
            let mut ready = true;
            let mut snapshots = Vec::with_capacity(pending.documents.len());
            let mut seen = rustc_hash::FxHashSet::default();
            for document in &mut pending.documents {
                let server_version_matches = document.server_version.is_none_or(|version| {
                    self.lsp_manager.doc_versions.get(&document.path) == Some(&version)
                });
                let editor = self
                    .find_editor_mut_by_path(&document.path)
                    .ok_or("An edit target was closed or could not be opened")?;
                if *document.identity.get_or_insert(editor.save_identity) != editor.save_identity {
                    return Err("An edit target was closed and reopened".into());
                }
                if !seen.insert(editor.save_identity) {
                    return Err(
                        "Workspace edit targets the same open document more than once".into(),
                    );
                }
                if editor.bg_loading {
                    ready = false;
                    continue;
                }
                if !server_version_matches {
                    return Err(
                        "The document changed before the language server edit could be applied"
                            .into(),
                    );
                }
                app_lsp_edits::ensure_writable(editor)?;
                if *document.version.get_or_insert(editor.content_version) != editor.content_version
                {
                    return Err("The document changed while the edit was loading".into());
                }
                snapshots.push(editor.rope.clone());
            }
            Ok(ready.then_some(snapshots))
        })();
        let snapshots = match snapshots {
            Ok(None) => {
                self.pending_lsp_workspace_edits.push_front(pending);
                return false;
            }
            Ok(Some(snapshots)) => Ok(snapshots),
            Err(error) => Err(error),
        };
        let result = snapshots.and_then(|snapshots| {
            let prepared = pending
                .documents
                .iter()
                .zip(snapshots)
                .map(|(document, rope)| app_lsp_edits::prepare(&rope, &document.edits))
                .collect::<Result<Vec<_>, _>>()?;
            let mut count = 0;
            for (document, edits) in pending.documents.iter().zip(prepared) {
                let editor = self
                    .find_editor_mut_by_path(&document.path)
                    .ok_or("An edit target was closed or could not be opened")?;
                count += app_lsp_edits::apply_prepared(editor, edits)?;
            }
            Ok(count)
        });
        self.completion_index_sync.buffers_changed |=
            result.as_ref().is_ok_and(|count| *count != 0);
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
