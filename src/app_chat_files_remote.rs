//! Remote file context is assembled off the UI thread from immutable Rope
//! snapshots. View keys never reach the client's native filesystem.
use super::*;
use crate::remote::{self, context};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
#[path = "app_chat_paste.rs"]
pub(crate) mod paste;
#[cfg(test)]
#[path = "app_chat_files_remote_tests.rs"]
mod tests;

#[derive(Debug)]
pub(crate) struct PreparedPrompt {
    pub content: Vec<p::ContentBlock>,
    pub display: String,
    pub source: Option<std::sync::Arc<context::Source>>,
}

#[derive(Debug, PartialEq)]
enum FileText {
    Local(String),
    Remote(usize, usize),
}
#[derive(Debug)]
struct PreparedFiles {
    files: Vec<(PathBuf, FileText)>,
    source: Option<std::sync::Arc<context::Source>>,
}

pub(crate) struct PromptJob {
    pub serial: u64,
    pub item: u64,
    pub root: PathBuf,
    pub handle: vtcode::acp::ClientHandle,
    pub client: remote::client::Client,
    pub epoch: u64,
    cancel: CancellationToken,
}
impl Drop for PromptJob {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

pub(crate) struct AttachmentJob {
    serial: u64,
    root: PathBuf,
    client: remote::client::Client,
    pub candidates: Vec<PathBuf>,
    base: Vec<PathBuf>,
    mention: Option<Mention>,
    cancel: CancellationToken,
}
impl Drop for AttachmentJob {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

struct Buffer {
    path: PathBuf,
    identity: Option<PathBuf>,
    rope: ropey::Rope,
    modified: bool,
    loading: bool,
}

type Inspection = (PathBuf, context::Kind);

/// One preparation uses one coherent inspection/validation cache. This is not
/// retained between user actions: the next send must inspect current files.
#[derive(Default)]
struct PreparationCache {
    inspections: HashMap<PathBuf, Result<Inspection, String>>,
    sizes: HashMap<PathBuf, usize>,
}
impl PreparationCache {
    fn inspect(&mut self, workspace: &remote::workspace::Workspace, paths: &[PathBuf], cancel: &CancellationToken) -> Result<Vec<Result<Inspection, String>>, String> {
        let mut unique = HashSet::new();
        let missing: Vec<_> = paths.iter().filter(|path| !self.inspections.contains_key(*path) && unique.insert(*path)).cloned().collect();
        for batch in missing.chunks(context::INSPECT_BATCH) {
            let found = workspace.inspect_context(batch, cancel).map_err(|error| error.to_string())?;
            self.inspections.extend(batch.iter().cloned().zip(found));
        }
        paths.iter().map(|path| self.inspections.get(path).cloned().ok_or_else(|| "Incomplete file inspection.".to_string())).collect()
    }
}

fn pasted_remote_paths(workspace: &remote::workspace::Workspace, text: &str, cache: &mut PreparationCache, cancel: &CancellationToken) -> Result<Option<Vec<PathBuf>>, String> {
    let text = text.trim();
    if text.is_empty() { return Ok(None); }
    let whole = resolve(workspace, text).ok();
    let tokens = pasted_path_values(text).and_then(|values| values.iter().map(|value| resolve(workspace, value).ok()).collect::<Option<Vec<_>>>());
    let paths: Vec<_> = whole.iter().chain(tokens.iter().flatten()).cloned().collect();
    let info = cache.inspect(workspace, &paths, cancel)?;
    let info: HashMap<_, _> = paths.into_iter().zip(info).collect();
    let is_file = |path: &PathBuf| matches!(info.get(path), Some(Ok((_, context::Kind::File))));
    if let Some(path) = whole.filter(is_file) { return Ok(Some(vec![path])); }
    Ok(tokens.filter(|paths| paths.iter().all(is_file)))
}

fn resolve(workspace: &remote::workspace::Workspace, value: &str) -> Result<PathBuf, String> {
    if value.is_empty() {
        return Err("Choose a file after @, or remove the @ before sending.".into());
    }
    if value.starts_with("file:") {
        return workspace
            .file_uri_key(value)
            .ok_or_else(|| "Invalid remote file URL.".into());
    }
    workspace
        .target_key(value)
        .map_err(|error| error.to_string())
}

fn prepare_files(
    workspace: &remote::workspace::Workspace,
    candidates: Vec<PathBuf>,
    buffers: Vec<Buffer>,
    retain: bool,
    cancel: &CancellationToken,
) -> Result<PreparedFiles, String> {
    prepare_files_cached(workspace, candidates, &buffers, retain, cancel, &mut PreparationCache::default())
}

fn prepare_files_cached(
    workspace: &remote::workspace::Workspace,
    candidates: Vec<PathBuf>,
    buffers: &[Buffer],
    retain: bool,
    cancel: &CancellationToken,
    cache: &mut PreparationCache,
) -> Result<PreparedFiles, String> {
    let mut unique = HashSet::new();
    let candidates: Vec<_> = candidates
        .into_iter()
        .filter(|path| unique.insert(path.clone()))
        .collect();
    let mut files = Vec::new();
    let mut identities = HashSet::new();
    for batch in candidates.chunks(context::INSPECT_BATCH) {
        let info = cache.inspect(workspace, batch, cancel)?;
        for (path, info) in batch.iter().zip(info) {
            let (identity, kind) = info
                .map_err(|error| format!("{}: {error}", file_label(path, &workspace.view_root)))?;
            if !identities.insert(identity.clone()) {
                continue;
            }
            if files.len() == ATTACHMENT_LIMIT {
                return Err("Attach up to 32 files per message.".into());
            }
            files.push((path.clone(), identity, kind));
        }
    }
    // Load-time identities restrict fresh alias checks to plausible matches;
    // attaching one file never stats every unrelated open file on the server.
    let requested: HashSet<_> = files.iter().map(|(path, _, _)| path).collect();
    let buffers: Vec<_> = buffers
        .iter()
        .filter(|buffer| {
            requested.contains(&buffer.path)
                || buffer
                    .identity
                    .as_ref()
                    .is_none_or(|identity| identities.contains(identity))
        })
        .collect();
    // Direct buffer paths were just inspected with the requested files. Reuse
    // those identities instead of paying another network round trip and stat.
    let mut inspected: HashMap<_, _> = files
        .iter()
        .map(|(path, identity, kind)| (path.clone(), (identity.clone(), *kind)))
        .collect();
    let paths: Vec<_> = buffers
        .iter()
        .map(|buffer| buffer.path.clone())
        .filter(|path| !inspected.contains_key(path))
        .collect();
    let aliases = cache.inspect(workspace, &paths, cancel)?;
    let mut by_identity: HashMap<PathBuf, Vec<&Buffer>> = HashMap::new();
    inspected.extend(
        paths
            .into_iter()
            .zip(aliases)
            .filter_map(|(path, info)| info.ok().map(|info| (path, info))),
    );
    for buffer in &buffers {
        if let Some((identity, _)) = inspected.get(&buffer.path) {
            by_identity
                .entry(identity.clone())
                .or_default()
                .push(buffer);
        }
    }
    let mut texts = (0..files.len()).map(|_| None).collect::<Vec<_>>();
    let mut disk = Vec::new();
    let mut disk_positions = Vec::new();
    for (index, (path, identity, kind)) in files.iter().enumerate() {
        let fail = |error| format!("{}: {error}", file_label(path, &workspace.view_root));
        if !retain && let Some(bytes) = cache.sizes.get(identity) {
            texts[index] = Some(FileText::Remote(0, *bytes));
            continue;
        }
        let matching = by_identity.get(identity);
        if let Some(matching) = matching {
            if matching.iter().any(|buffer| buffer.loading) {
                return Err(fail("wait for the file to finish loading."));
            }
            let modified: Vec<_> = matching.iter().filter(|buffer| buffer.modified).collect();
            if let Some(first) = modified.first()
                && modified
                    .iter()
                    .skip(1)
                    .any(|other| !crate::rope_changes::equal(&first.rope, &other.rope))
            {
                return Err(fail(
                    "this file has conflicting unsaved buffers through different aliases. Save or reconcile them before sending.",
                ));
            }
            let buffer = matching
                .iter()
                .max_by_key(|buffer| buffer.modified)
                .unwrap();
            if buffer.rope.len_bytes() > FILE_LIMIT {
                return Err(fail("file context is limited to 128 KiB per file."));
            }
            let text = buffer.rope.to_string();
            if text.contains('\0') { return Err(fail("binary files cannot be attached.")); }
            if !retain { cache.sizes.insert(identity.clone(), text.len()); }
            texts[index] = Some(FileText::Local(text));
        } else {
            if *kind != context::Kind::File {
                return Err(fail("choose an existing text file."));
            }
            disk.push((path.clone(), identity.clone()));
            disk_positions.push(index);
        }
    }
    let mut source = None;
    if !disk.is_empty() {
        let (loaded, retained) = workspace
            .file_context(&disk, retain, cancel)
            .map_err(|error| error.to_string())?;
        if loaded.len() != disk.len() {
            return Err("Incomplete attached file contents.".into());
        }
        source = retained;
        if retain && source.is_none() {
            return Err("Missing prepared file context.".into());
        }
        for (source_index, (index, bytes)) in disk_positions.into_iter().zip(loaded).enumerate() {
            if !retain { cache.sizes.insert(files[index].1.clone(), bytes); }
            texts[index] = Some(FileText::Remote(source_index, bytes));
        }
    }
    let mut total = 0;
    let files = files
        .into_iter()
        .zip(texts)
        .map(|((path, _, _), text)| {
            if cancel.is_cancelled() {
                return Err("File context preparation cancelled.".into());
            }
            let text = text.ok_or("Missing attached file contents.")?;
            let bytes = match &text {
                FileText::Local(text) => text.len(),
                FileText::Remote(_, bytes) => *bytes,
            };
            if bytes > FILE_LIMIT {
                return Err("File context is limited to 128 KiB per file.".into());
            }
            if matches!(&text, FileText::Local(text) if text.contains('\0')) {
                return Err("Binary files cannot be attached.".into());
            }
            total += bytes;
            if total > TOTAL_LIMIT {
                return Err(
                    "File context is limited to 1 MiB per message. Remove some files.".into(),
                );
            }
            Ok((path, text))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(PreparedFiles { files, source })
}

fn prompt(
    workspace: &remote::workspace::Workspace,
    input: String,
    attached: Vec<PathBuf>,
    buffers: Vec<Buffer>,
    embedded: bool,
    cancel: &CancellationToken,
) -> Result<PreparedPrompt, String> {
    prompt_cached(workspace, input, attached, buffers, embedded, cancel, &mut PreparationCache::default())
}

fn prompt_cached(
    workspace: &remote::workspace::Workspace,
    input: String,
    attached: Vec<PathBuf>,
    buffers: Vec<Buffer>,
    embedded: bool,
    cancel: &CancellationToken,
    cache: &mut PreparationCache,
) -> Result<PreparedPrompt, String> {
    let mut paths = attached;
    for mention in mentions(&input) {
        if !mention.closed {
            return Err("Close the quote around the @ file path before sending.".into());
        }
        paths.push(resolve(workspace, &mention.path)?);
    }
    let mut content = vec![p::ContentBlock::Text(p::TextContent::new(input.clone()))];
    let mut display = input;
    let prepared = prepare_files_cached(workspace, paths, &buffers, true, cancel, cache)?;
    for (path, text) in prepared.files {
        let label = file_label(&path, &workspace.view_root);
        display.push_str(&format!("\n\n📎 {label}"));
        let text = match text {
            FileText::Local(text) => text,
            FileText::Remote(index, _) => {
                content.push(
                    prepared
                        .source
                        .as_ref()
                        .ok_or("Missing prepared file context.")?
                        .reference(index, embedded, label),
                );
                continue;
            }
        };
        if embedded {
            // The ACP transport rewrites this schema URI on its worker. Text
            // fallback below needs a node path because arbitrary text is opaque.
            let uri = url::Url::from_file_path(&path)
                .map_err(|_| format!("Cannot represent {label} as a file URL."))?;
            content.push(p::ContentBlock::Resource(p::EmbeddedResource::new(
                p::EmbeddedResourceResource::TextResourceContents(
                    p::TextResourceContents::new(text, uri.to_string()).mime_type("text/plain"),
                ),
            )));
        } else {
            let node_path = workspace
                .context_path(&path)
                .map_err(|error| error.to_string())?;
            let context = serde_json::json!({"path":node_path,"text":text});
            content.push(p::ContentBlock::Text(p::TextContent::new(format!(
                "Attached file context (current editor contents when open):\n{context}"
            ))));
        }
    }
    Ok(PreparedPrompt {
        content,
        display,
        source: prepared.source,
    })
}

impl App {
    fn remote_chat_buffers(&self, workspace: &remote::workspace::Workspace) -> Vec<Buffer> {
        self.tabs
            .iter()
            .chain(self.groups.iter().flat_map(|group| &group.tabs))
            .filter(|tab| tab.kind == TabKind::File)
            .filter_map(|tab| {
                let path = tab.editor.file_path.as_ref()?;
                workspace.remote_path(path).ok()?;
                Some(Buffer {
                    path: path.clone(),
                    identity: tab
                        .editor
                        .remote_checkpoint
                        .as_ref()
                        .and_then(|checkpoint| checkpoint.identity.as_ref())
                        .map(|path| workspace.view_key(path)),
                    rope: tab.editor.rope.clone(),
                    modified: tab.editor.modified,
                    loading: tab.editor.bg_loading,
                })
            })
            .collect()
    }

    pub(crate) fn start_remote_prompt_context(
        &mut self,
        session: p::SessionId,
        item: u64,
        input: String,
        attached: Vec<PathBuf>,
        paste: Option<Arc<paste::FrozenPaste>>,
    ) -> Result<(), String> {
        let workspace = self
            .remote_workspace
            .clone()
            .ok_or("Remote workspace is unavailable.")?;
        if workspace.view_root != self.chat_file_root() {
            return Err("The chat workspace changed. Reconnect before sending.".into());
        }
        let handle = self
            .chat
            .client
            .as_ref()
            .ok_or("The agent is disconnected.")?
            .handle();
        let epoch = self
            .chat
            .sessions
            .get(&session)
            .ok_or("The conversation was closed.")?
            .history_epoch;
        let buffers = self.remote_chat_buffers(&workspace);
        let embedded = self
            .chat
            .initialized
            .as_ref()
            .is_some_and(|info| info.agent_capabilities.prompt_capabilities.embedded_context);
        let agent = self.chat.active_agent;
        self.chat.files.work_serial = self.chat.files.work_serial.wrapping_add(1);
        let serial = self.chat.files.work_serial;
        let root = workspace.view_root.clone();
        let client = workspace.client.clone();
        let sender = self.background.sender.clone();
        let target = session.clone();
        let cancel = self
            .background
            .submit_context("chat-file-context", move |cancel| {
                let (result, draft) = paste::prepare_prompt(&workspace, input, attached, buffers, embedded, &cancel, paste);
                if !cancel.is_cancelled() {
                    let _ = sender.send(BackgroundResult::ChatPromptPrepared {
                        agent,
                        session: target,
                        serial,
                        result,
                        draft,
                    });
                }
            });
        self.chat.files.preparing.insert(
            session,
            PromptJob {
                serial,
                item,
                root,
                handle,
                client,
                epoch,
                cancel,
            },
        );
        self.chat.status = "Preparing message…".into();
        Ok(())
    }

    pub(super) fn attach_remote_chat_files(
        &mut self,
        paths: Vec<PathBuf>,
        mention: Option<Mention>,
    ) -> bool {
        let Some(workspace) = self
            .remote_workspace
            .clone()
            .filter(|workspace| workspace.view_root == self.chat_file_root())
        else {
            self.chat.error =
                Some("The chat workspace changed. Reconnect before attaching files.".into());
            return false;
        };
        let session = self.chat.selected.clone();
        let old = self.chat.files.attaching.remove(&session);
        let base = self.chat.files.attached.clone();
        let mut candidates = old
            .as_ref()
            .map_or_else(|| base.clone(), |job| job.candidates.clone());
        candidates.extend(paths);
        let mut seen = HashSet::new();
        candidates.retain(|path| seen.insert(path.clone()));
        let buffers = self.remote_chat_buffers(&workspace);
        self.chat.files.work_serial = self.chat.files.work_serial.wrapping_add(1);
        let serial = self.chat.files.work_serial;
        let agent = self.chat.active_agent;
        let root = workspace.view_root.clone();
        let client = workspace.client.clone();
        let work = candidates.clone();
        let sender = self.background.sender.clone();
        let cancel = self
            .background
            .submit_context("chat-attachments", move |cancel| {
                let result = prepare_files(&workspace, work, buffers, false, &cancel)
                    .map(|files| files.files.into_iter().map(|(path, _)| path).collect());
                if !cancel.is_cancelled() {
                    let _ = sender.send(BackgroundResult::ChatAttachmentsPrepared {
                        agent,
                        serial,
                        result,
                    });
                }
            });
        self.chat.files.attaching.insert(
            session,
            AttachmentJob {
                serial,
                root,
                client,
                candidates,
                base,
                mention,
                cancel,
            },
        );
        true
    }

    pub(crate) fn apply_remote_chat_attachments(
        &mut self,
        serial: u64,
        result: Result<Vec<PathBuf>, String>,
    ) -> bool {
        let target = self
            .chat
            .files
            .attaching
            .iter()
            .find_map(|(target, job)| (job.serial == serial).then(|| target.clone()));
        let Some(target) = target else {
            return false;
        };
        let job = self.chat.files.attaching.remove(&target).unwrap();
        if self.remote_workspace.as_ref().is_none_or(|workspace| {
            workspace.view_root != job.root || !workspace.client.same_connection(&job.client)
        }) {
            return true;
        }
        if target
            .as_ref()
            .is_some_and(|id| !self.chat.sessions.contains_key(id))
        {
            return true;
        }
        let previous = self.chat.selected.clone();
        let visible = previous == target;
        self.chat.select_view(target);
        if self.chat_file_root() == job.root && self.chat.files.attached != job.base {
            let additions = job.candidates.iter().filter(|path| !job.base.contains(path)).cloned().collect();
            self.attach_remote_chat_files(additions, job.mention.clone());
            self.chat.select_view(previous);
            return visible;
        }
        let valid = self.chat_file_root() == job.root && self.chat.files.attached == job.base;
        if valid {
            match result {
                Ok(paths) => {
                    self.chat.files.attached = paths;
                    self.chat.files.scroll = self.chat.files.attached.len().saturating_sub(3);
                    self.chat.error = None;
                    if let Some(mention) = &job.mention
                        && current_mention(&self.chat.input, self.chat.cursor).as_ref()
                            == Some(mention)
                    {
                        self.consume_chat_file_mention(mention);
                        if visible {
                            self.chat.files.dismiss();
                        }
                    }
                }
                Err(error) => self.chat.error = Some(error),
            }
        }
        self.chat.select_view(previous);
        true
    }

    /// Clicking a completion and immediately sending is one user action even
    /// while remote validation is pending. Preserve its chosen path and remove
    /// only the exact partial token it replaced; the send worker validates it.
    pub(crate) fn commit_remote_chat_attachments_for_send(&mut self) -> bool {
        let target = self.chat.selected.clone();
        let Some(job) = self.chat.files.attaching.remove(&target) else {
            return true;
        };
        if self.remote_workspace.as_ref().is_none_or(|workspace| {
            workspace.view_root != job.root || !workspace.client.same_connection(&job.client)
        }) || self.chat_file_root() != job.root
            || self.chat.files.attached != job.base
        {
            self.chat.error =
                Some("The chat workspace or attachments changed. Choose the files again.".into());
            return false;
        }
        if let Some(mention) = &job.mention
            && current_mention(&self.chat.input, self.chat.cursor).as_ref() == Some(mention)
        {
            self.consume_chat_file_mention(mention);
        }
        self.chat.files.attached = job.candidates.clone();
        self.chat.files.dismiss();
        true
    }

    pub(super) fn start_remote_chat_suggestions(
        &mut self,
        root: PathBuf,
        query: String,
        preferred: Vec<PathBuf>,
        generation: u64,
        agent: u64,
    ) {
        let workspace = self
            .remote_workspace
            .clone()
            .filter(|workspace| workspace.view_root == root);
        self.chat.files.remote_completion =
            workspace.as_ref().map(|workspace| workspace.client.clone());
        let file_generation = self.workspace_file_index_generation;
        let sender = self.background.sender.clone();
        self.submit_chat_file_query(move |cancel| {
            let paths = workspace
                .and_then(|workspace| {
                    let preferred = preferred
                        .iter()
                        .filter_map(|path| workspace.remote_path(path).ok())
                        .take(context::INSPECT_BATCH)
                        .collect();
                    let explicit = resolve(&workspace, &query)
                        .ok()
                        .and_then(|path| workspace.remote_path(&path).ok());
                    match workspace
                        .client
                        .call_cancellable(
                            remote::Command::Context(context::Command::Suggest {
                                generation: file_generation,
                                query,
                                preferred,
                                explicit,
                            }),
                            &cancel,
                        )
                        .ok()?
                    {
                        remote::Reply::Context(context::Reply::Suggestions(paths)) => Some(
                            paths
                                .into_iter()
                                .map(|path| workspace.view_key(&path))
                                .collect(),
                        ),
                        _ => None,
                    }
                })
                .unwrap_or_default();
            if !cancel.is_cancelled() {
                let _ = sender.send(BackgroundResult::ChatFileSuggestions {
                    agent,
                    generation,
                    root,
                    paths,
                });
            }
        });
    }

}
