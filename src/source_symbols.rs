//! Fallback source symbols from open Rope snapshots. Small requests complete
//! locally; larger scans yield between bounded batches on the existing CPU
//! pool. No file reads, retained source cache, or idle timer are involved.
use crate::{
    app::{App, SidebarView, SymbolItem},
    background::{BackgroundResult, BackgroundTaskSender, CancellationToken},
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[cfg(test)]
#[path = "source_symbols_tests.rs"]
mod tests;

const SYNC_BYTES: usize = 64 * 1024;
const SYNC_LINES: usize = 2048;
const BATCH_LINES: usize = 2048;
const BATCH_TIME: Duration = Duration::from_millis(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Destination {
    Picker(bool),
    Outline,
}

#[derive(Default)]
pub(crate) struct State {
    serial: u64,
    picker: Option<(u64, CancellationToken)>,
    outline: Option<(u64, CancellationToken)>,
    picker_work: crate::background::latest::LatestWork,
    outline_work: crate::background::latest::LatestWork,
}
impl State {
    pub fn picker_pending(&self) -> bool {
        self.picker.as_ref().is_some_and(|(_, c)| !c.is_cancelled())
    }
    pub fn outline_pending(&self) -> bool {
        self.outline
            .as_ref()
            .is_some_and(|(_, c)| !c.is_cancelled())
    }
    pub fn cancel_picker(&mut self) {
        if let Some((_, c)) = self.picker.take() {
            c.cancel();
        }
        self.picker_work.cancel();
    }
    pub fn cancel_outline(&mut self) {
        if let Some((_, c)) = self.outline.take() {
            c.cancel();
        }
        self.outline_work.cancel();
    }
}
impl Drop for State {
    fn drop(&mut self) {
        self.cancel_picker();
        self.cancel_outline();
    }
}

#[derive(PartialEq, Eq)]
struct Key {
    tab: usize,
    id: u64,
    version: u64,
    bytes: usize,
    path: Option<PathBuf>,
}
impl Key {
    fn from_tab(tab: usize, editor: &crate::editor::Editor) -> Self {
        Self {
            tab,
            id: editor.navigation_id,
            version: editor.content_version,
            bytes: editor.rope.len_bytes(),
            path: editor.file_path.clone(),
        }
    }
    fn matches(&self, editor: &crate::editor::Editor) -> bool {
        self.id == editor.navigation_id
            && self.version == editor.content_version
            && self.bytes == editor.rope.len_bytes()
            && self.path == editor.file_path
    }
}
struct Source {
    tab: usize,
    rope: ropey::Rope,
    file_name: Option<String>,
}

pub(crate) struct Reply {
    serial: u64,
    group: usize,
    destination: Destination,
    keys: Vec<Key>,
    pub rows: Vec<SymbolItem>,
}
struct Builder {
    reply: Reply,
    sources: Vec<Source>,
    source: usize,
    line: usize,
    prefix: String,
}

struct ScanCompletion {
    cancel: CancellationToken,
    completed: bool,
}
impl Drop for ScanCompletion {
    fn drop(&mut self) {
        if !self.completed {
            self.cancel.cancel();
        }
    }
}

impl Builder {
    fn batch(&mut self, cancel: &CancellationToken) -> bool {
        let started = Instant::now();
        let mut processed = 0;
        while let Some(source) = self.sources.get(self.source) {
            for (offset, line) in source.rope.lines_at(self.line).enumerate() {
                if cancel.is_cancelled() {
                    return true;
                }
                if let Some((name, kind)) = symbol_in_line(line, &mut self.prefix, cancel) {
                    self.reply.rows.push(SymbolItem {
                        name,
                        kind,
                        line: self.line + offset,
                        tab_index: Some(source.tab),
                        file_name: source.file_name.clone(),
                        file_path: None,
                    });
                }
                processed += 1;
                if processed >= BATCH_LINES
                    || (processed % 64 == 0 && started.elapsed() >= BATCH_TIME)
                {
                    self.line += offset + 1;
                    return false;
                }
            }
            self.source += 1;
            self.line = 0;
        }
        true
    }
}

fn continue_scan(
    mut builder: Builder,
    pool: BackgroundTaskSender,
    sender: crate::background::BackgroundResultSender,
    cancel: CancellationToken,
    activity: crate::performance::ActivityGuard,
    mut completion: ScanCompletion,
) {
    if cancel.is_cancelled() {
        return;
    }
    if builder.batch(&cancel) {
        if !cancel.is_cancelled() {
            builder.reply.rows.shrink_to_fit();
            completion.completed = sender
                .send(BackgroundResult::SourceSymbols(builder.reply))
                .is_ok();
        }
    } else {
        let next_pool = pool.clone();
        let _ = pool.send(Box::new(move || {
            continue_scan(builder, next_pool, sender, cancel, activity, completion)
        }));
    }
}

impl App {
    pub(crate) fn request_source_symbols(&mut self, destination: Destination) {
        match destination {
            Destination::Picker(_) => self.source_symbol_work.cancel_picker(),
            Destination::Outline => self.source_symbol_work.cancel_outline(),
        }
        if destination == Destination::Outline && self.lsp_outline_available {
            return;
        }
        self.source_symbol_work.serial = self.source_symbol_work.serial.wrapping_add(1);
        let serial = self.source_symbol_work.serial;
        let cancel = CancellationToken::new();
        let mut bytes = 0usize;
        let mut lines = 0usize;
        let mut sources = Vec::new();
        let mut keys = Vec::new();
        for (tab, entry) in self.tabs.iter().enumerate() {
            if destination != Destination::Picker(true) && tab != self.active_tab {
                continue;
            }
            let editor = &entry.editor;
            bytes = bytes.saturating_add(editor.rope.len_bytes());
            lines = lines.saturating_add(editor.rope.len_lines());
            let key = Key::from_tab(tab, editor);
            let file_name = matches!(destination, Destination::Picker(_))
                .then(|| {
                    key.path
                        .as_ref()
                        .and_then(|p| p.file_name())
                        .map(|p| p.to_string_lossy().into_owned())
                })
                .flatten();
            sources.push(Source {
                tab,
                rope: editor.rope.clone(),
                file_name,
            });
            keys.push(key);
        }
        let rows = match destination {
            Destination::Picker(_) => std::mem::take(&mut self.symbol_list_items),
            Destination::Outline => std::mem::take(&mut self.outline_items),
        };
        let reply = Reply {
            serial,
            group: self.active_group,
            destination,
            keys,
            rows: Vec::new(),
        };
        let mut builder = Builder {
            reply,
            sources,
            source: 0,
            line: 0,
            prefix: String::new(),
        };
        let pending = Some((serial, cancel.clone()));
        match destination {
            Destination::Picker(_) => {
                self.source_symbol_work.picker = pending;
                self.update_symbol_filter();
            }
            Destination::Outline => self.source_symbol_work.outline = pending,
        }
        if bytes <= SYNC_BYTES && lines <= SYNC_LINES && rows.len() <= SYNC_LINES {
            // The same streaming parser handles both paths; only scheduling
            // differs. Tiny documents need no background round trip.
            let mut rows = rows;
            rows.clear();
            builder.reply.rows = rows;
            while !builder.batch(&cancel) {}
            self.apply_source_symbols(builder.reply);
        } else {
            let pool = self.background.interactive_sender();
            let sender = self.background.sender.clone();
            let activity = self
                .background
                .activity
                .start("Source symbols", None)
                .cancellable(cancel.clone());
            let next_pool = pool.clone();
            let completion = ScanCompletion {
                cancel: cancel.clone(),
                completed: false,
            };
            let job = crate::background::latest::Job {
                cancel: cancel.clone(),
                run: Box::new(move || {
                    // Reclaim the old result strings away from the input thread.
                    let mut rows = rows;
                    rows.clear();
                    builder.reply.rows = rows;
                    continue_scan(
                        builder,
                        next_pool,
                        sender,
                        cancel,
                        activity.running(),
                        completion,
                    );
                }),
            };
            match destination {
                Destination::Picker(_) => self.source_symbol_work.picker_work.replace(pool, job),
                Destination::Outline => self.source_symbol_work.outline_work.replace(pool, job),
            }
        }
    }

    pub(crate) fn apply_source_symbols(&mut self, reply: Reply) -> bool {
        let pending = match reply.destination {
            Destination::Picker(_) => &mut self.source_symbol_work.picker,
            Destination::Outline => &mut self.source_symbol_work.outline,
        };
        if pending
            .as_ref()
            .is_none_or(|(serial, cancel)| *serial != reply.serial || cancel.is_cancelled())
        {
            return false;
        }
        pending.take();
        let visible = match reply.destination {
            Destination::Picker(workspace) => {
                self.symbol_list_active && self.symbol_list_workspace == workspace
            }
            Destination::Outline => {
                self.sidebar_view == SidebarView::Outline && !self.lsp_outline_available
            }
        };
        if !visible {
            return false;
        }
        let current = self.active_group == reply.group
            && reply.keys.iter().all(|key| {
                self.tabs
                    .get(key.tab)
                    .is_some_and(|tab| key.matches(&tab.editor))
            })
            && match reply.destination {
                Destination::Picker(true) => self.tabs.len() == reply.keys.len(),
                _ => reply
                    .keys
                    .first()
                    .is_none_or(|key| key.tab == self.active_tab),
            };
        if !current {
            self.request_source_symbols(reply.destination);
            return true;
        }
        match reply.destination {
            Destination::Picker(_) => {
                self.symbol_list_items = reply.rows;
                self.filter_symbol_items();
            }
            Destination::Outline => {
                self.outline_items = reply.rows;
                self.outline_scroll = self
                    .outline_scroll
                    .min(self.outline_items.len().saturating_sub(1));
                self.outline_selected = self
                    .outline_selected
                    .min(self.outline_items.len().saturating_sub(1));
            }
        }
        true
    }
}

fn symbol_in_line(
    line: ropey::RopeSlice<'_>,
    prefix: &mut String,
    cancel: &CancellationToken,
) -> Option<(String, &'static str)> {
    if let Some(text) = line.as_str() {
        return source_symbol(text)
            .filter(|(name, _)| !name.is_empty())
            .map(|(name, kind)| (name.into(), kind));
    }
    // A generated line can span megabytes. Only the declaration prefix and
    // its identifier matter; never flatten the rest into a temporary String.
    let mut chars = line.chars().peekable();
    let mut visited = 0usize;
    while chars.peek().is_some_and(|ch| ch.is_whitespace()) {
        chars.next();
        visited += 1;
        if visited % 1024 == 0 && cancel.is_cancelled() {
            return None;
        }
    }
    prefix.clear();
    prefix.extend(chars.by_ref().take(64));
    let (name, kind) = source_symbol(prefix).filter(|(name, _)| !name.is_empty())?;
    let continues = name.as_bytes().as_ptr_range().end == prefix.as_bytes().as_ptr_range().end;
    let mut name = name.to_owned();
    if continues {
        for ch in chars {
            if !(ch.is_alphanumeric() || ch == '_' || ch == '<') {
                break;
            }
            name.push(ch);
            visited += 1;
            if visited % 1024 == 0 && cancel.is_cancelled() {
                return None;
            }
        }
    }
    Some((name, kind))
}

const SOURCE_SYMBOL_PATTERNS: &[(&str, &str)] = &[
    ("fn ", "function"),
    ("pub(crate) fn ", "function"),
    ("async fn ", "function"),
    ("pub async fn ", "function"),
    ("struct ", "struct"),
    ("pub struct ", "struct"),
    ("enum ", "enum"),
    ("pub enum ", "enum"),
    ("trait ", "trait"),
    ("pub trait ", "trait"),
    ("impl ", "impl"),
    ("mod ", "module"),
    ("pub mod ", "module"),
    ("const ", "constant"),
    ("pub const ", "constant"),
    ("static ", "static"),
    ("pub static ", "static"),
    ("type ", "type"),
    ("pub type ", "type"),
    ("class ", "class"),
    ("interface ", "interface"),
    ("function ", "function"),
    ("def ", "function"),
];

const SOURCE_INITIALS: [bool; 128] = {
    let mut initials = [false; 128];
    let mut index = 0;
    while index < SOURCE_SYMBOL_PATTERNS.len() {
        initials[SOURCE_SYMBOL_PATTERNS[index].0.as_bytes()[0] as usize] = true;
        index += 1;
    }
    initials
};

/// Share the outline/picker fallback parser without building visibility-prefix
/// strings for every pattern on every line of a potentially large document.
pub(crate) fn source_symbol(line: &str) -> Option<(&str, &'static str)> {
    let line = line.trim();
    let first = *line.as_bytes().first()?;
    if first >= 128 || !SOURCE_INITIALS[first as usize] {
        return None;
    }
    let crate_visible = line.strip_prefix("pub(crate) ");
    SOURCE_SYMBOL_PATTERNS.iter().find_map(|&(pattern, kind)| {
        let rest = line
            .strip_prefix(pattern)
            .or_else(|| crate_visible.and_then(|line| line.strip_prefix(pattern)))?;
        let end = rest
            .char_indices()
            .find(|&(_, ch)| !(ch.is_alphanumeric() || ch == '_' || ch == '<'))
            .map_or(rest.len(), |(byte, _)| byte);
        Some((&rest[..end], kind))
    })
}
