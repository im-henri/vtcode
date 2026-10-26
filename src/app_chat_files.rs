//! File context for Agent Chat: explicit attachments, @ completion, and file drops.
use crate::{
    app::{ActivePane, App, TabKind},
    background::{BackgroundResult, CancellationToken},
    text_input::{TextInput, TextInputId},
};
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::{
    io::Read,
    ops::Range,
    path::{Path, PathBuf},
};
use vtcode::acp::protocol as p;

const FILE_LIMIT: usize = 128 * 1024;
const TOTAL_LIMIT: usize = 1024 * 1024;
const ATTACHMENT_LIMIT: usize = 32;
const SUGGESTION_LIMIT: usize = 40;
#[cfg(test)]
#[path = "chat_input_performance_tests.rs"]
mod input_performance_tests;
#[path = "app_chat_files_remote.rs"]
pub(crate) mod hosted;

#[derive(Default)]
pub(crate) struct ChatFiles {
    /// Paths, rather than snapshots: send always uses the latest editor contents.
    pub attached: Vec<PathBuf>,
    pub completion: Option<FileCompletion>,
    pub popup: Option<Rect>,
    pub scroll: usize,
    generation: u64,
    cancel: Option<CancellationToken>,
    suggestions: crate::background::latest::LatestWork,
    remote_completion: Option<crate::remote::client::Client>,
    pub(crate) work_serial: u64,
    pub(crate) preparing: std::collections::HashMap<p::SessionId, hosted::PromptJob>,
    pub(crate) attaching: std::collections::HashMap<Option<p::SessionId>, hosted::AttachmentJob>,
    pub(crate) pasting: std::collections::HashMap<Option<p::SessionId>, hosted::paste::Job>,
}

pub(crate) struct FileCompletion {
    pub mention: Mention,
    pub root: PathBuf,
    pub paths: Vec<PathBuf>,
    pub selected: usize,
    pub searching: bool,
}

impl ChatFiles {
    pub fn dismiss(&mut self) {
        self.suggestions.cancel();
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        self.generation = self.generation.wrapping_add(1);
        self.completion = None;
        self.popup = None;
        self.remote_completion = None;
    }
}

impl Drop for ChatFiles {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Mention {
    pub range: Range<usize>,
    pub path: String,
    closed: bool,
    path_start: usize,
    path_end: usize,
}

#[path = "chat_mention_scan.rs"]
mod mention_scan;

fn mentions(text: &str) -> impl Iterator<Item = Mention> + '_ {
    mention_scan::Scanner::new(text, text.len()).map(|span| Mention {
        path: text[span.path.clone()].into(),
        range: span.range,
        closed: span.closed,
        path_start: span.path.start,
        path_end: span.path.end,
    })
}

pub(crate) fn has_mentions(text: &str) -> bool {
    mention_scan::has_mentions(text)
}

fn current_mention(text: &str, cursor: usize) -> Option<Mention> {
    if !text.is_char_boundary(cursor) {
        return None;
    }
    mention_scan::Scanner::new(text, cursor)
        .find(|span| cursor <= span.range.end)
        .map(|span| Mention {
            path: text[span.path.start.min(cursor)..span.path.end.min(cursor)].into(),
            range: span.range,
            closed: span.closed,
            path_start: span.path.start,
            path_end: span.path.end,
        })
}

pub(crate) fn file_label(path: &Path, root: &Path) -> String {
    if let Some(label) = crate::remote::workspace::display_key(path) { return label; }
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/")
}

fn resolve_path(root: &Path, value: &str) -> Result<PathBuf, String> {
    if value.is_empty() {
        return Err("Choose a file after @, or remove the @ before sending.".into());
    }
    let path = if value.starts_with("file://") {
        url::Url::parse(value)
            .ok()
            .and_then(|u| u.to_file_path().ok())
            .ok_or_else(|| "Invalid file URL.".to_string())?
    } else {
        PathBuf::from(value)
    };
    let path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    // Keep the user's display path; canonical identities are used for deduplication.
    Ok(path)
}

fn same_file(a: &Path, b: &Path) -> bool {
    App::same_open_path(a, b)
        || match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => App::same_open_path(&a, &b),
            _ => false,
        }
}

/// Terminal file-manager drops arrive as pasted paths, not OS drag events.
/// Accept only a complete list of existing files; never interpret shell code.
fn pasted_paths(text: &str, root: &Path) -> Option<Vec<PathBuf>> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    // Some terminals paste an unquoted single path containing spaces.
    if let Ok(path) = resolve_path(root, text)
        && path.is_file()
    {
        return Some(vec![path]);
    }
    pasted_path_values(text)?.into_iter().map(|value| {
        let path = resolve_path(root, &value).ok()?;
        path.is_file().then_some(path)
    }).collect()
}

/// Parse terminal clipboard tokens without interpreting shell code or probing
/// any filesystem. The whole unquoted path is checked first by each backend.
fn pasted_path_values(text: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while chars.peek().is_some() {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let quote = chars.peek().copied().filter(|c| matches!(c, '"' | '\''));
        if quote.is_some() {
            chars.next();
        }
        let mut token = String::new();
        let mut closed = quote.is_none();
        while let Some(c) = chars.next() {
            if quote == Some(c) {
                closed = true;
                break;
            }
            if quote.is_none() && c.is_whitespace() {
                break;
            }
            // Unix terminals escape spaces; Windows backslashes are preserved.
            if quote.is_none() && c == '\\' && chars.peek().is_some_and(|c| c.is_whitespace()) {
                token.push(chars.next().unwrap());
            } else {
                token.push(c);
            }
        }
        if !closed
            || token.is_empty()
            || (quote.is_some() && chars.peek().is_some_and(|c| !c.is_whitespace()))
        {
            return None;
        }
        tokens.push(token);
        if tokens.len() > ATTACHMENT_LIMIT {
            return None;
        }
    }
    (!tokens.is_empty()).then_some(tokens)
}

fn search_files(
    root: &Path,
    query: &str,
    preferred: Vec<PathBuf>,
    cancel: &CancellationToken,
) -> Vec<PathBuf> {
    if cancel.is_cancelled() {
        return Vec::new();
    }
    use std::{
        cmp::Reverse,
        collections::{BinaryHeap, HashSet},
    };
    let explicit = resolve_path(root, query).ok().filter(|p| p.is_file());
    let query = query
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_lowercase();
    let mut best = BinaryHeap::new();
    let mut seen = HashSet::new();
    let mut add = |path: PathBuf, preferred: bool| {
        if !seen.insert(path.clone()) {
            return;
        }
        let label = file_label(&path, root).to_lowercase();
        let score = if query.is_empty() {
            Some(0)
        } else {
            crate::app::fuzzy_match_score(&label, &query)
        };
        if let Some(score) = score {
            let score = score + if preferred { 1000 } else { 0 };
            let value = (Reverse(score), path);
            if best.len() < SUGGESTION_LIMIT {
                best.push(value);
            } else if let Some(mut worst) = best.peek_mut()
                && value < *worst
            {
                *worst = value;
            }
        }
    };
    for path in preferred {
        if path.is_file() {
            add(path, true);
        }
    }
    for entry in walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .filter_entry(|e| {
            !cancel.is_cancelled()
                && !(e.file_type().is_dir()
                    && crate::app_global_search::EXCLUDE_DIRS
                        .contains(&e.file_name().to_str().unwrap_or("")))
        })
        .filter_map(Result::ok)
    {
        if cancel.is_cancelled() {
            return Vec::new();
        }
        if entry.file_type().is_file() {
            add(entry.into_path(), false);
        }
    }
    if cancel.is_cancelled() {
        return Vec::new();
    }
    let mut paths: Vec<_> = best.into_sorted_vec().into_iter().map(|(_, p)| p).collect();
    // A fully typed path wins over fuzzy/recent matches. Absolute paths outside
    // the workspace can be attached explicitly without indexing that directory.
    if let Some(path) = explicit {
        paths.retain(|p| !same_file(p, &path));
        paths.insert(0, path);
    }
    paths.truncate(SUGGESTION_LIMIT);
    paths
}

impl App {
    pub(crate) fn open_chat_location(&mut self, path: &Path, line: Option<u32>) {
        if let Some(workspace) = &self.remote_workspace {
            let path = if crate::remote::workspace::is_view_key(path) { Ok(path.to_path_buf()) }
                else { workspace.target_key(&path.to_string_lossy()) };
            let path = match path { Ok(path) => path, Err(error) => { self.chat.error = Some(error.to_string()); return; } };
            self.open_remote_location(path, line, crate::app_remote_navigation::Source::Chat);
            return;
        }
        let path = if path.is_absolute() { path.to_path_buf() } else { self.explorer.root.join(path) };
        if path.is_dir() {
            let relative = self.explorer.root.canonicalize().ok().and_then(|root| {
                path.canonicalize().ok()?.strip_prefix(root).ok().map(Path::to_path_buf)
            });
            let Some(relative) = relative else {
                self.chat.error = Some("This directory is outside the current workspace.".into());
                return;
            };
            self.show_explorer = true;
            self.sidebar_view = crate::app::SidebarView::Explorer;
            self.active_pane = ActivePane::Explorer;
            if relative.as_os_str().is_empty() {
                self.explorer.selected = 0;
                self.explorer.scroll_offset = 0;
            } else {
                let target = self.explorer.root.join(relative);
                if self.explorer.reveal_file_deferred(&target) {
                    self.refresh_explorer_tree_async();
                }
            }
        } else {
            self.open_file(&path);
            if let Some(line) = line && let Some(editor) = self.editor_mut() {
                // File loading is asynchronous; set_cursor also records the
                // requested location until the file's contents arrive.
                editor.set_cursor(line as usize, 0);
            }
        }
        if self.workbench_body_area.is_some_and(|a| a.width == 0) {
            self.chat.visible = false;
        }
    }

    pub(crate) fn chat_file_root(&self) -> &Path {
        if self.chat.client.is_none() {
            &self.explorer.root
        } else {
            &self.chat.workspace
        }
    }

    pub(crate) fn refresh_chat_file_completion(&mut self) {
        if self.chat.terminal_input.is_some()
            || self.chat.loading_id
            || self.active_pane != ActivePane::Chat
        {
            self.chat.files.dismiss();
            return;
        }
        let mention = if self.chat.anchor.is_some_and(|a| a != self.chat.cursor) {
            None
        } else {
            current_mention(&self.chat.input, self.chat.cursor)
        };
        let root = self.chat_file_root().to_path_buf();
        if self
            .chat
            .files
            .completion
            .as_ref()
            .is_some_and(|c| Some(&c.mention) == mention.as_ref() && c.root == root)
        {
            return;
        }
        self.chat.files.dismiss();
        let Some(mention) = mention else {
            return;
        };
        let preferred: Vec<_> = self
            .tabs
            .iter()
            .chain(self.groups.iter().flat_map(|g| &g.tabs))
            .filter(|t| t.kind == TabKind::File)
            .filter_map(|t| t.editor.file_path.clone())
            .chain(self.recent_files.iter().cloned())
            .collect();
        let query = mention.path.clone();
        // Offer already-open files immediately, while the filesystem search runs.
        let normalized_query = if self.remote_workspace.is_some() {
            query.to_lowercase()
        } else {
            query.replace('\\', "/").to_lowercase()
        };
        let mut paths = preferred
            .iter()
            .filter(|p| {
                query.is_empty()
                    || crate::app::fuzzy_match_score(
                        &file_label(p, &root).to_lowercase(),
                        &normalized_query,
                    )
                    .is_some()
            })
            .fold(Vec::new(), |mut paths, p| {
                if !paths.contains(p) && paths.len() < SUGGESTION_LIMIT {
                    paths.push(p.clone());
                }
                paths
            });
        if self.remote_workspace.is_none()
            && let Ok(path) = resolve_path(&root, &query)
            && path.is_file()
        {
            paths.retain(|p| !same_file(p, &path));
            paths.insert(0, path);
            paths.truncate(SUGGESTION_LIMIT);
        }
        self.chat.files.completion = Some(FileCompletion {
            mention,
            root: root.clone(),
            paths,
            selected: 0,
            searching: true,
        });
        let generation = self.chat.files.generation;
        let agent = self.chat.active_agent;
        if self.remote_workspace.is_some() {
            self.start_remote_chat_suggestions(root, query, preferred, generation, agent);
            return;
        }
        let sender = self.background.sender.clone();
        self.submit_chat_file_query(move |cancel| {
            let paths = search_files(&root, &query, preferred, &cancel);
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

    fn submit_chat_file_query(&mut self, run: impl FnOnce(CancellationToken) + Send + 'static) {
        let cancel = CancellationToken::new();
        let worker_cancel = cancel.clone();
        let activity = self
            .background
            .activity
            .start(crate::performance::worker_label("chat-files"), None)
            .cancellable(cancel.clone());
        let pool = self.background.context_sender();
        self.chat.files.suggestions.replace(
            pool,
            crate::background::latest::Job {
                cancel: cancel.clone(),
                run: Box::new(move || {
                    let _activity = activity.running();
                    if !worker_cancel.is_cancelled() {
                        run(worker_cancel);
                    }
                }),
            },
        );
        self.chat.files.cancel = Some(cancel);
    }

    pub(crate) fn apply_chat_file_suggestions(
        &mut self,
        generation: u64,
        root: PathBuf,
        paths: Vec<PathBuf>,
    ) -> bool {
        if generation != self.chat.files.generation || root != self.chat_file_root() {
            return false;
        }
        if let Some(client) = &self.chat.files.remote_completion
            && self.remote_workspace.as_ref().is_none_or(|workspace| {
                workspace.view_root != root || !workspace.client.same_connection(client)
            })
        {
            self.chat.files.dismiss();
            return true;
        }
        let Some(completion) = &mut self.chat.files.completion else {
            return false;
        };
        // Keep the highlighted path stable when background results arrive.
        let selected = completion.paths.get(completion.selected).cloned();
        completion.selected = selected
            .and_then(|p| paths.iter().position(|v| v == &p))
            .unwrap_or(0);
        completion.paths = paths;
        completion.searching = false;
        true
    }

    pub(crate) fn browse_chat_files(&mut self) {
        if self.chat.terminal_input.is_some() || self.chat.loading_id {
            return;
        }
        self.focus_chat();
        self.chat.focus = None;
        self.chat.picker = None;
        if current_mention(&self.chat.input, self.chat.cursor).is_some() {
            self.refresh_chat_file_completion();
        } else {
            let prefix = if self.chat.cursor > 0
                && !self.chat.input[..self.chat.cursor].ends_with(char::is_whitespace)
            {
                " @"
            } else {
                "@"
            };
            self.edit_text_input(
                TextInputId::Chat,
                KeyCode::Null,
                KeyModifiers::empty(),
                Some(prefix),
            );
        }
    }

    pub(crate) fn choose_chat_file(&mut self, index: usize) {
        let Some(completion) = &self.chat.files.completion else {
            return;
        };
        let Some(path) = completion.paths.get(index).cloned() else {
            return;
        };
        let mention = completion.mention.clone();
        // Never replace a stale token after a clipboard or cursor operation.
        if current_mention(&self.chat.input, self.chat.cursor).as_ref() != Some(&mention) {
            self.refresh_chat_file_completion();
            return;
        }
        if self.remote_workspace.is_some() {
            self.attach_remote_chat_files(vec![path], Some(mention));
            return;
        }
        if !self.attach_chat_files(vec![path]) {
            return;
        }
        self.consume_chat_file_mention(&mention);
        self.chat.files.dismiss();
    }

    pub(super) fn consume_chat_file_mention(&mut self, mention: &Mention) {
        let state = &mut self.chat.connection;
        state.cursor = mention.range.end;
        state.anchor = Some(mention.range.start);
        state.input_history.edit(
            TextInput::new(&mut state.input, &mut state.cursor, &mut state.anchor).multiline(true),
            KeyCode::Backspace,
            KeyModifiers::empty(),
            None,
        );
        state.input_goal = None;
        state.focus = None;
    }

    pub(crate) fn try_paste_chat_files(&mut self, text: &str) -> bool {
        if self.chat.terminal_input.is_some() || self.chat.loading_id {
            return false;
        }
        if self.remote_workspace.is_some() { return self.defer_remote_chat_file_paste(text); }
        let Some(paths) = pasted_paths(text, self.chat_file_root()) else {
            return false;
        };
        self.attach_chat_files(paths);
        true
    }

    fn chat_file_text(&self, path: &Path) -> Result<String, String> {
        let label = file_label(path, self.chat_file_root());
        let fail = |why: &str| format!("{label}: {why}");
        let mut buffers = self
            .tabs
            .iter()
            .chain(
                self.groups
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != self.active_group)
                    .flat_map(|(_, g)| &g.tabs),
            )
            .filter(|t| {
                t.kind == TabKind::File
                    && t.editor
                        .file_path
                        .as_ref()
                        .is_some_and(|p| same_file(p, path))
            });
        let first = buffers.next();
        let buffer = first
            .filter(|t| t.editor.modified)
            .or_else(|| buffers.find(|t| t.editor.modified))
            .or(first);
        let text = if let Some(tab) = buffer.filter(|t| !t.editor.bg_loading) {
            if tab.editor.rope.len_bytes() > FILE_LIMIT {
                return Err(fail("file context is limited to 128 KiB per file."));
            }
            tab.editor.rope.to_string()
        } else {
            if !path.is_file() {
                return Err(fail("choose an existing text file."));
            }
            let file = std::fs::File::open(path).map_err(|e| fail(&e.to_string()))?;
            let mut bytes = Vec::new();
            file.take(FILE_LIMIT as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| fail(&e.to_string()))?;
            if bytes.len() > FILE_LIMIT {
                return Err(fail("file context is limited to 128 KiB per file."));
            }
            String::from_utf8(bytes).map_err(|_| fail("only UTF-8 text files can be attached."))?
        };
        if text.contains('\0') {
            return Err(fail("binary files cannot be attached."));
        }
        Ok(text)
    }

    fn prepare_chat_files(
        &self,
        candidates: Vec<PathBuf>,
    ) -> Result<Vec<(PathBuf, String)>, String> {
        let mut prepared: Vec<(PathBuf, String)> = Vec::new();
        let mut total = 0;
        for path in candidates {
            if prepared.iter().any(|(p, _)| same_file(p, &path)) {
                continue;
            }
            if prepared.len() == ATTACHMENT_LIMIT {
                return Err("Attach up to 32 files per message.".into());
            }
            let text = self.chat_file_text(&path)?;
            total += text.len();
            if total > TOTAL_LIMIT {
                return Err(
                    "File context is limited to 1 MiB per message. Remove some files.".into(),
                );
            }
            prepared.push((path, text));
        }
        Ok(prepared)
    }

    /// Atomic batch: a bad drop or paste preserves the entire existing draft.
    pub(crate) fn attach_chat_files(&mut self, paths: Vec<PathBuf>) -> bool {
        if self.remote_workspace.is_some() { return self.attach_remote_chat_files(paths, None); }
        let candidates = self
            .chat
            .files
            .attached
            .iter()
            .cloned()
            .chain(paths)
            .collect();
        match self.prepare_chat_files(candidates) {
            Ok(prepared) => {
                self.chat.files.attached = prepared.into_iter().map(|(p, _)| p).collect();
                self.chat.files.scroll = self.chat.files.attached.len().saturating_sub(3);
                self.chat.error = None;
                true
            }
            Err(error) => {
                self.chat.error = Some(error);
                false
            }
        }
    }

    pub(crate) fn chat_prompt_with_files(&self) -> Result<(Vec<p::ContentBlock>, String), String> {
        self.chat_prompt_with_files_for(&self.chat.input, &self.chat.files.attached)
    }

    pub(crate) fn chat_prompt_with_files_for(&self, input: &str, attached: &[PathBuf]) -> Result<(Vec<p::ContentBlock>, String), String> {
        let mut paths = attached.to_vec();
        for mention in mentions(input) {
            if !mention.closed {
                return Err("Close the quote around the @ file path before sending.".into());
            }
            paths.push(resolve_path(self.chat_file_root(), &mention.path)?);
        }
        let prepared = self.prepare_chat_files(paths)?;
        let mut content = vec![p::ContentBlock::Text(p::TextContent::new(
            input.to_owned(),
        ))];
        let embedded = self
            .chat
            .initialized
            .as_ref()
            .is_some_and(|i| i.agent_capabilities.prompt_capabilities.embedded_context);
        let mut display = input.to_owned();
        for (path, text) in prepared {
            let label = file_label(&path, self.chat_file_root());
            display.push_str(&format!("\n\n📎 {label}"));
            if embedded {
                let uri = url::Url::from_file_path(&path)
                    .map_err(|_| format!("Cannot represent {label} as a file URL."))?;
                content.push(p::ContentBlock::Resource(p::EmbeddedResource::new(
                    p::EmbeddedResourceResource::TextResourceContents(
                        p::TextResourceContents::new(text, uri.to_string())
                            .mime_type("text/plain".to_owned()),
                    ),
                )));
            } else {
                // ACP requires all agents to accept text. JSON preserves file
                // boundaries even when the source itself contains code fences.
                let context = serde_json::json!({"path":path, "text":text});
                content.push(p::ContentBlock::Text(p::TextContent::new(format!(
                    "Attached file context (current editor contents when open):\n{context}"
                ))));
            }
        }
        Ok((content, display))
    }

    pub(crate) fn chat_drag_paths(&self) -> Option<Vec<PathBuf>> {
        if let Some(path) = &self.dragging_explorer_path {
            return Some(if self.explorer_multi_selected.contains(path) {
                self.explorer_multi_selected.to_vec()
            } else {
                vec![path.clone()]
            });
        }
        let index = self.dragging_tab?;
        let source = self.dragging_tab_source_group.unwrap_or(self.active_group);
        let tab = if source == self.active_group {
            self.tabs.get(index)
        } else {
            self.groups.get(source).and_then(|g| g.tabs.get(index))
        }?;
        Some(
            tab.editor
                .file_path
                .clone()
                .filter(|_| tab.kind == TabKind::File)
                .into_iter()
                .collect(),
        )
    }

    pub(crate) fn chat_file_drop_hover(&self) -> bool {
        self.drag_has_moved
            && self
                .chat
                .area
                .is_some_and(|a| a.contains((self.mouse_col, self.mouse_row).into()))
            && self.chat_drag_paths().is_some()
    }

    pub(crate) fn handle_chat_file_drop(&mut self, mouse: MouseEvent) -> bool {
        if !matches!(
            mouse.kind,
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
        ) {
            return false;
        }
        let paths = match self.chat_drag_paths() {
            Some(paths) => paths,
            None if self.dragging_terminal_tab.is_some() => Vec::new(),
            None => return false,
        };
        if self
            .drag_start_pos
            .is_some_and(|p| p != (mouse.column, mouse.row))
        {
            self.drag_has_moved = true;
        }
        self.tab_drop_zone = None;
        self.tab_drop_target_group = None;
        self.explorer_drop_target_idx = None;
        if mouse.kind == MouseEventKind::Up(MouseButton::Left) {
            if self.drag_has_moved {
                self.focus_chat();
                if paths.is_empty() || self.chat.terminal_input.is_some() || self.chat.loading_id {
                    self.chat.error =
                        Some("Drop saved text files into the message composer.".into());
                } else {
                    self.attach_chat_files(paths);
                }
                self.chat.files.dismiss();
            }
            self.clear_drag_states();
            self.drag_has_moved = false;
            self.drag_start_pos = None;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_acp::ChatAction;
    use crossterm::event::KeyEvent;
    use ratatui::{Terminal, backend::TestBackend};
    use std::time::{Duration, Instant};

    fn setup() -> (tempfile::TempDir, App) {
        let root = tempfile::tempdir().unwrap();
        let mut app = App::test_new();
        app.explorer = crate::explorer::FileExplorer::new(root.path().to_owned());
        app.clear_explorer_selection();
        app.focus_chat();
        (root, app)
    }

    fn file(root: &Path, name: &str, text: &str) -> PathBuf {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn chat_directory_locations_reveal_workspace_folders_without_editor_tabs() {
        let (root, mut app) = setup();
        file(root.path(), "src/nested/example.rs", "fn main() {}\n");
        let target = root.path().join("src/nested");
        let original_tabs = app.tabs.len();
        app.show_explorer = false;
        app.sidebar_view = crate::app::SidebarView::SourceControl;
        app.chat_action(ChatAction::OpenFile(target.clone(), None));
        app.wait_for_background(Duration::from_secs(5), |app| {
            app.explorer.entries.get(app.explorer.selected).is_some_and(|e| e.path == target)
        });
        assert!(app.show_explorer);
        assert_eq!(app.sidebar_view, crate::app::SidebarView::Explorer);
        assert_eq!(app.active_pane, ActivePane::Explorer);
        assert_eq!(app.tabs.len(), original_tabs);
        app.chat_action(ChatAction::OpenFile(root.path().to_path_buf(), None));
        assert_eq!(app.explorer.scroll_offset, 0);
        assert_eq!(app.tabs.len(), original_tabs);
        assert!(app.chat.error.is_none());
    }

    #[test]
    fn chat_outside_directory_locations_do_not_open_files_or_switch_workspace() {
        let (root, mut app) = setup();
        let outside = tempfile::tempdir().unwrap();
        let original_tabs = app.tabs.len();
        for path in [outside.path().to_path_buf(), root.path().join("..")] {
            app.chat_action(ChatAction::OpenFile(path, None));
            assert!(app.chat.error.as_deref().unwrap().contains("outside the current workspace"));
            assert_eq!(app.explorer.root, root.path());
            assert_eq!(app.tabs.len(), original_tabs);
            assert_eq!(app.active_pane, ActivePane::Chat);
        }
    }

    #[test]
    fn chat_file_locations_resolve_relative_paths_and_keep_line_after_loading() {
        let (root, mut app) = setup();
        let path = file(root.path(), "src/example.rs", "one\ntwo\nthree\nfour\n");
        app.chat_action(ChatAction::OpenFile("src/example.rs".into(), Some(2)));
        app.wait_for_background(Duration::from_secs(5), |app| app.editor().is_some_and(|e| !e.bg_loading));
        let editor = app.editor().unwrap();
        assert!(same_file(editor.file_path.as_ref().unwrap(), &path));
        assert_eq!(editor.cursor_line, 2);
        assert_eq!(editor.rope.line(editor.cursor_line).to_string(), "three\n");
    }

    fn input(app: &mut App, text: &str) {
        app.chat.input = text.into();
        app.chat.cursor = text.len();
        app.chat.anchor = None;
        app.refresh_chat_file_completion();
    }

    fn wait_search(app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while app
            .chat
            .files
            .completion
            .as_ref()
            .is_some_and(|c| c.searching)
        {
            app.poll_background_results();
            assert!(Instant::now() < deadline, "file search timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16) {
        crate::mouse::handle_mouse(
            app,
            MouseEvent {
                kind,
                column: x,
                row: y,
                modifiers: KeyModifiers::empty(),
            },
        );
    }

    fn draw(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
        terminal
    }

    fn capture(terminal: &Terminal<TestBackend>, name: &str) {
        let Some(folder) = std::env::var_os("VTCODE_CHAT_FILES_CAPTURE") else {
            return;
        };
        let folder = PathBuf::from(folder);
        std::fs::create_dir_all(&folder).unwrap();
        let buffer = terminal.backend().buffer();
        let cells: Vec<_> = buffer
            .content
            .iter()
            .map(|c| {
                serde_json::json!({
                    "text":c.symbol(), "fg":format!("{:?}",c.fg), "bg":format!("{:?}",c.bg),
                    "bold":c.modifier.contains(ratatui::style::Modifier::BOLD)
                })
            })
            .collect();
        std::fs::write(
            folder.join(format!("chat-files-{name}.json")),
            serde_json::to_vec(&serde_json::json!({
                "width":buffer.area.width, "height":buffer.area.height, "cells":cells
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn chat_files_parse_paths_without_treating_emails_or_code_as_attachments() {
        let text = "Review @src/main.rs and @\"docs/Hello 世界.md\" (also @'path with spaces.rs'). Email me@host.com; `@code`\n```\n@code.rs\n```\n\\@escaped";
        let paths: Vec<_> = mentions(text).map(|m| m.path).collect();
        assert_eq!(
            paths,
            ["src/main.rs", "docs/Hello 世界.md", "path with spaces.rs"]
        );
        let text = "🙂 check @\"docs/日本語.md\" after";
        let cursor = text.find("語").unwrap();
        let mention = current_mention(text, cursor).unwrap();
        assert_eq!(mention.path, "docs/日本");
        assert_eq!(&text[mention.range], "@\"docs/日本語.md\"");
        assert!(!mentions("@\"unfinished path").next().unwrap().closed);
        assert!(current_mention("email@host.com", 9).is_none());
    }

    #[test]
    fn chat_files_pasted_drop_paths_support_quotes_spaces_unicode_and_file_urls() {
        let (root, mut app) = setup();
        let a = file(root.path(), "first file.txt", "first");
        let b = file(root.path(), "日本語.txt", "second");
        for payload in [
            format!("\"{}\" \"{}\"", a.display(), b.display()),
            format!("'{}'\n'{}'", a.display(), b.display()),
            format!(
                "{}\n{}",
                url::Url::from_file_path(&a).unwrap(),
                url::Url::from_file_path(&b).unwrap()
            ),
            "first\\ file.txt 日本語.txt".into(),
        ] {
            let paths = pasted_paths(&payload, root.path()).unwrap();
            assert_eq!(paths.len(), 2, "{payload}");
            assert!(same_file(&paths[0], &a));
            assert!(same_file(&paths[1], &b));
        }
        input(&mut app, "Keep this draft");
        app.paste_into_focused_text_input(&format!("\"{}\" \"{}\"", a.display(), b.display()));
        assert_eq!(app.chat.input, "Keep this draft");
        assert_eq!(app.chat.files.attached.len(), 2);
        for prose in [
            "explain first file.txt",
            "echo first\\ file.txt",
            "missing.txt",
            "@日本語.txt",
            "print('hello')",
        ] {
            assert!(pasted_paths(prose, root.path()).is_none(), "{prose}");
        }
    }

    #[test]
    fn chat_files_deduplicate_and_reject_bad_batches_without_losing_context() {
        let (root, mut app) = setup();
        let good = file(root.path(), "good.txt", "good");
        assert!(app.attach_chat_files(vec![
            good.clone(),
            root.path().join("./good.txt"),
            good.canonicalize().unwrap()
        ]));
        assert_eq!(app.chat.files.attached, [good.clone()]);
        let bad = root.path().join("binary.bin");
        std::fs::write(&bad, [0, 255, 1]).unwrap();
        let large = file(root.path(), "large.txt", &"a".repeat(FILE_LIMIT + 1));
        for path in [
            bad,
            large,
            root.path().to_path_buf(),
            root.path().join("missing.txt"),
        ] {
            let other = file(root.path(), "other.txt", "other");
            assert!(!app.attach_chat_files(vec![other, path]));
            assert_eq!(app.chat.files.attached, [good.clone()]);
            assert!(app.chat.error.is_some());
        }
        let many = (0..9)
            .map(|i| file(root.path(), &format!("{i}.txt"), &"x".repeat(FILE_LIMIT)))
            .collect();
        assert!(!app.attach_chat_files(many));
        assert!(app.chat.error.as_deref().unwrap().contains("1 MiB"));
        assert_eq!(app.chat.files.attached, [good]);
    }

    #[test]
    fn chat_files_prompt_resolves_typed_mentions_and_reads_latest_unsaved_buffers() {
        let (root, mut app) = setup();
        let a = file(root.path(), "src/main.rs", "on disk");
        let b = file(root.path(), "docs/guide space.md", "# guide\n");
        assert!(app.attach_chat_files(vec![a.clone()]));
        app.new_file();
        app.editor_mut().unwrap().file_path = Some(a.clone());
        app.editor_mut().unwrap().rope = ropey::Rope::from_str("unsaved 世界\r\n```\n");
        app.editor_mut().unwrap().modified = true;
        app.focus_chat();
        input(
            &mut app,
            "Compare @src/main.rs with @\"docs/guide space.md\"",
        );
        let (content, display) = app.chat_prompt_with_files().unwrap();
        assert_eq!(content.len(), 3);
        let p::ContentBlock::Text(text) = &content[1] else {
            panic!("text fallback")
        };
        let context: serde_json::Value =
            serde_json::from_str(text.text.split_once('\n').unwrap().1).unwrap();
        assert_eq!(context["text"], "unsaved 世界\r\n```\n");
        assert!(display.contains("📎 src/main.rs"));
        assert!(display.contains("📎 docs/guide space.md"));
        app.chat.initialized = Some(serde_json::from_value(serde_json::json!({
            "protocolVersion":1, "agentCapabilities":{"promptCapabilities":{"embeddedContext":true}}
        })).unwrap());
        let (content, _) = app.chat_prompt_with_files().unwrap();
        for (index, path) in [a, b].into_iter().enumerate() {
            let p::ContentBlock::Resource(resource) = &content[index + 1] else {
                panic!("embedded capability")
            };
            let p::EmbeddedResourceResource::TextResourceContents(text) = &resource.resource else {
                panic!("text resource")
            };
            assert!(same_file(
                &url::Url::parse(&text.uri).unwrap().to_file_path().unwrap(),
                &path
            ));
            if index == 0 {
                assert_eq!(text.text, "unsaved 世界\r\n```\n");
            }
        }
    }

    #[test]
    fn chat_files_bad_mentions_and_failed_send_preserve_the_draft() {
        let (root, mut app) = setup();
        let path = file(root.path(), "good.txt", "context");
        app.attach_chat_files(vec![path.clone()]);
        for text in ["Look at @missing.txt", "Look at @\"good.txt", "Look at @"] {
            input(&mut app, text);
            assert!(app.chat_prompt_with_files().is_err());
            assert_eq!(app.chat.input, text);
            assert_eq!(app.chat.files.attached, [path.clone()]);
        }
        input(&mut app, "valid draft");
        app.chat.selected = Some(p::SessionId::new("disconnected"));
        app.chat_action(ChatAction::Send);
        assert_eq!(app.chat.input, "valid draft");
        assert_eq!(app.chat.files.attached, [path]);
        assert!(app.chat.error.is_some());
    }

    #[test]
    fn chat_files_search_is_bounded_fuzzy_and_skips_build_directories() {
        let (root, mut app) = setup();
        let main = file(root.path(), "src/Main.rs", "");
        file(root.path(), "target/src/Main.rs", "");
        file(root.path(), "node_modules/src/Main.rs", "");
        let hidden = file(root.path(), ".config/settings.toml", "");
        for i in 0..100 {
            file(root.path(), &format!("many/{i}.txt"), "");
        }
        let cancel = CancellationToken::new();
        assert_eq!(
            search_files(root.path(), "src/mrs", vec![], &cancel),
            [main.clone()]
        );
        assert_eq!(
            search_files(root.path(), "settings", vec![], &cancel),
            [hidden]
        );
        assert_eq!(
            search_files(root.path(), "", vec![main.clone()], &cancel).len(),
            SUGGESTION_LIMIT
        );
        assert_eq!(
            search_files(root.path(), "", vec![main.clone()], &cancel)[0],
            main
        );
        assert_eq!(
            search_files(root.path(), &main.to_string_lossy(), vec![], &cancel)[0],
            main
        );
        let other = file(root.path(), "old/src/Main.rs", "");
        assert_eq!(
            search_files(root.path(), "src/Main.rs", vec![other.clone()], &cancel)[0],
            main
        );
        app.recent_files = vec![other];
        input(&mut app, "@src/Main.rs");
        assert_eq!(app.chat.files.completion.as_ref().unwrap().paths[0], main);
        wait_search(&mut app);
        let completion = app.chat.files.completion.as_ref().unwrap();
        assert_eq!(completion.paths[completion.selected], main);
        cancel.cancel();
        assert!(search_files(root.path(), "Main", vec![], &cancel).is_empty());
    }

    #[test]
    fn chat_files_suggestions_reject_stale_queries_and_workspaces() {
        let (root, mut app) = setup();
        let a = file(root.path(), "a.rs", "a");
        let b = file(root.path(), "b.rs", "b");
        input(&mut app, "@a");
        let old = app.chat.files.generation;
        input(&mut app, "@b");
        assert!(!app.apply_chat_file_suggestions(old, root.path().into(), vec![a]));
        wait_search(&mut app);
        assert_eq!(
            app.chat.files.completion.as_ref().unwrap().paths,
            [b.clone()]
        );
        let current = app.chat.files.generation;
        app.explorer.root = root.path().join("new-workspace");
        assert!(!app.apply_chat_file_suggestions(current, root.path().into(), vec![b]));
        app.poll_acp();
        assert!(app.chat.files.completion.is_none());
        assert!(!app.apply_chat_file_suggestions(current, root.path().into(), vec![]));
    }

    #[test]
    fn chat_files_keyboard_completion_preserves_surrounding_unicode_and_supports_escape() {
        let (root, mut app) = setup();
        let path = file(root.path(), "src/日本語.rs", "hello");
        input(&mut app, "🙂 Explain @src/日本語.rs please");
        app.chat.cursor = app.chat.input.find(" please").unwrap();
        app.refresh_chat_file_completion();
        wait_search(&mut app);
        assert!(app.handle_chat_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::empty())));
        assert_eq!(app.chat.input, "🙂 Explain  please");
        assert_eq!(app.chat.files.attached, [path]);
        assert!(app.chat.files.completion.is_none());
        assert!(app.chat.selected.is_none()); // Choosing a file never sends a prompt.
        app.chat_action(ChatAction::Attach);
        wait_search(&mut app);
        app.handle_chat_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert!(app.chat.files.completion.is_none());
        assert_eq!(app.active_pane, ActivePane::Chat);
        assert!(app.chat.input.contains('@'));
        app.handle_chat_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::empty()));
        assert_eq!(app.chat.input, "🙂 Explain  please");
    }

    #[test]
    fn chat_files_mouse_completion_remove_paging_and_compact_views() {
        let (root, mut app) = setup();
        for name in [
            "src/chat.rs",
            "src/client.rs",
            "src/config.rs",
            "docs/chat.md",
            "tests/chat.rs",
        ] {
            file(root.path(), name, name);
        }
        app.explorer.refresh();
        input(&mut app, "Compare @chat");
        wait_search(&mut app);
        app.chat.width = 64;
        let terminal = draw(&mut app, 140, 40);
        capture(&terminal, "autocomplete");
        let hit = app
            .chat
            .hits
            .iter()
            .find(|h| matches!(h.action, ChatAction::ChooseFile(0)))
            .unwrap()
            .clone();
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            hit.area.x,
            hit.area.y,
        );
        assert_eq!(app.chat.files.attached.len(), 1);
        assert_eq!(app.chat.input, "Compare ");
        let paths = search_files(root.path(), "", vec![], &CancellationToken::new());
        assert!(app.attach_chat_files(paths));
        let terminal = draw(&mut app, 140, 40);
        capture(&terminal, "attached");
        let hit = app
            .chat
            .hits
            .iter()
            .find(|h| matches!(h.action, ChatAction::RemoveAttachment(_)))
            .unwrap()
            .clone();
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            hit.area.x,
            hit.area.y,
        );
        assert_eq!(app.chat.files.attached.len(), 4);
        app.chat_action(ChatAction::AttachmentPage(true));
        assert_eq!(app.chat.files.scroll, 0);
        for (width, height) in [(80, 24), (45, 14), (24, 10), (8, 4), (1, 1)] {
            let terminal = draw(&mut app, width, height);
            if width == 80 {
                capture(&terminal, "compact");
            }
            for hit in &app.chat.hits {
                assert!(hit.area.right() <= width && hit.area.bottom() <= height);
            }
        }
        app.chat_action(ChatAction::Picker(crate::app_acp::Picker::Attachments));
        assert_eq!(app.chat.choices.len(), 4);
        app.chat_action(ChatAction::Pick(3));
        assert_eq!(app.chat.files.attached.len(), 3);
    }

    #[test]
    fn chat_files_explorer_drop_adds_multiple_files_without_moving_them() {
        let (root, mut app) = setup();
        let a = file(root.path(), "a.rs", "first");
        let b = file(root.path(), "b.rs", "second");
        app.explorer.refresh();
        app.show_explorer = true;
        app.show_open_editors = false;
        app.sidebar_view = crate::app::SidebarView::Explorer;
        app.explorer_area = Some(Rect::new(2, 3, 30, 14));
        app.chat.area = Some(Rect::new(40, 1, 60, 30));
        // Exercise the real Ctrl-click selection and drag initiation routes.
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 6,
            row: 6,
            modifiers: KeyModifiers::CONTROL,
        };
        crate::mouse::handle_mouse(&mut app, click);
        crate::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                ..click
            },
        );
        crate::mouse::handle_mouse(&mut app, MouseEvent { row: 7, ..click });
        assert_eq!(app.explorer_multi_selected.len(), 2);
        app.explorer_drop_target_idx = Some(0);
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 45, 20);
        assert!(app.chat_file_drop_hover());
        assert!(app.explorer_drop_target_idx.is_none());
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 45, 20);
        assert_eq!(app.chat.files.attached, [a.clone(), b.clone()]);
        assert_eq!(std::fs::read_to_string(a).unwrap(), "first");
        assert_eq!(std::fs::read_to_string(b).unwrap(), "second");
        assert!(app.dragging_explorer_path.is_none());
        assert!(!app.drag_has_moved);
    }

    #[test]
    fn chat_files_tab_drop_keeps_the_tab_in_its_source_group() {
        let (root, mut app) = setup();
        let path = file(root.path(), "source.rs", "disk");
        app.new_file();
        app.editor_mut().unwrap().file_path = Some(path.clone());
        app.editor_mut().unwrap().rope = ropey::Rope::from_str("unsaved tab contents");
        app.editor_mut().unwrap().modified = true;
        let source_tab = app.tabs.pop().unwrap();
        // The active group lives in App; reserve its slot before another group.
        app.groups.push(Default::default());
        app.groups.push(Default::default());
        let source_group = app.groups.len() - 1;
        app.groups[source_group].tabs.push(source_tab);
        app.chat.area = Some(Rect::new(40, 1, 60, 30));
        app.dragging_tab = Some(0);
        app.dragging_tab_source_group = Some(source_group);
        app.drag_start_pos = Some((4, 2));
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 45, 20);
        assert_eq!(app.groups[source_group].tabs.len(), 1);
        assert_eq!(app.chat.files.attached, [path]);
        let (prompt, _) = app.chat_prompt_with_files().unwrap();
        assert!(
            serde_json::to_string(&prompt)
                .unwrap()
                .contains("unsaved tab contents")
        );
        assert!(app.dragging_tab.is_none());
    }

    #[test]
    fn chat_files_auxiliary_input_does_not_attach_paths_or_lose_message_files() {
        let (root, mut app) = setup();
        let path = file(root.path(), "notes.txt", "context");
        app.attach_chat_files(vec![path.clone()]);
        input(&mut app, "my message");
        app.chat_action(ChatAction::LoadById);
        app.paste_into_focused_text_input("@notes.txt");
        assert!(app.chat.files.completion.is_none());
        assert_eq!(app.chat.input, "@notes.txt");
        app.chat_action(ChatAction::BackToChat);
        assert_eq!(app.chat.input, "my message");
        assert_eq!(app.chat.files.attached, [path.clone()]);
        let id = p::SessionId::new("auxiliary-input");
        let mut session = vtcode::acp::Session::new(id.clone());
        session.terminal(vtcode::acp::TerminalSnapshot {
            session_id: id.clone(),
            id: "command-1".into(),
            command: "local test".into(),
            output: String::new(),
            truncated: false,
            exit_status: None,
            released: false,
            cwd: Default::default(), history: None, history_version: 0, killed: false, error: None,
        });
        app.chat.sessions.insert(id.clone(), session);
        app.chat.selected = Some(id);
        app.chat_action(ChatAction::Terminal("command-1".into()));
        app.paste_into_focused_text_input(&path.to_string_lossy());
        assert_eq!(app.chat.input, path.to_string_lossy());
        assert!(app.chat.files.completion.is_none());
        app.chat_action(ChatAction::BackToChat);
        assert_eq!(app.chat.input, "my message");
        assert_eq!(app.chat.files.attached, [path]);
    }

    #[test]
    #[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
    fn chat_files_official_sdk_prompt_round_trip() {
        let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let python = std::env::var_os("VTCODE_ACP_TEST_PYTHON")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                project.join(if cfg!(windows) {
                    "target/acp-python/Scripts/python.exe"
                } else {
                    "target/acp-python/bin/python"
                })
            });
        for embedded in [false, true] {
            let (root, mut app) = setup();
            let first = file(root.path(), "first.rs", "disk");
            file(root.path(), "second space.txt", "second context 世界\n");
            let agent = if embedded {
                "tests/fixtures/scenario_agent.py"
            } else {
                "tests/fixtures/acp-python-sdk/echo_agent.py"
            };
            let mut config = vtcode::acp::AgentConfig::new(
                "Official SDK file context",
                python.to_string_lossy(),
            );
            config.args = vec![
                "-u".into(),
                project.join(agent).to_string_lossy().into_owned(),
            ];
            config.env.insert("PYTHONUTF8".into(), "1".into());
            config.env.insert("PYTHONIOENCODING".into(), "utf-8".into());
            if embedded {
                config
                    .env
                    .insert("ACP_TEST_EMBEDDED_CONTEXT".into(), "1".into());
            }
            app.settings_effective.acp_agents = Some(vec![config]);
            app.chat_action(ChatAction::Connect(0));
            let deadline = Instant::now() + Duration::from_secs(20);
            while app.chat.selected.is_none() {
                app.poll_acp();
                assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
            app.new_file();
            app.editor_mut().unwrap().file_path = Some(first.clone());
            app.editor_mut().unwrap().rope = ropey::Rope::from_str("unsaved first context 👋\r\n");
            app.editor_mut().unwrap().modified = true;
            app.focus_chat();
            input(&mut app, "@first.rs");
            wait_search(&mut app);
            app.handle_chat_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()));
            assert_eq!(app.chat.files.attached, [first]);
            input(&mut app, "inspect attachments @\"second space.txt\"");
            // Sending by the button must resolve a complete manually typed mention.
            app.chat_action(ChatAction::Send);
            assert!(app.chat.input.is_empty());
            assert!(app.chat.files.attached.is_empty());
            assert!(app.chat.files.completion.is_none());
            while app.chat.busy() {
                app.poll_acp();
                assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
            let response = app
                .chat
                .session()
                .unwrap()
                .entries
                .iter()
                .filter_map(|e| match e {
                    vtcode::acp::ChatEntry::Agent(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>();
            if embedded {
                let blocks: serde_json::Value = serde_json::from_str(&response).unwrap();
                assert_eq!(blocks.as_array().unwrap().len(), 3);
                assert_eq!(blocks[1]["type"], "resource");
                assert_eq!(
                    blocks[1]["resource"]["text"],
                    "unsaved first context 👋\r\n"
                );
                assert_eq!(blocks[2]["resource"]["text"], "second context 世界\n");
                assert!(
                    blocks[2]["resource"]["uri"]
                        .as_str()
                        .unwrap()
                        .ends_with("second%20space.txt")
                );
            } else {
                assert!(response.contains("unsaved first context 👋"), "{response}");
                assert!(response.contains("second context 世界"), "{response}");
                assert_eq!(response.matches("Attached file context").count(), 2);
            }
            app.chat_action(ChatAction::Disconnect);
        }
    }
}
