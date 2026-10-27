//! Short, distinct path hints without filesystem access or an all-tabs trie.
//! Only potential visible paths are retained; other tabs update their conflicts
//! in one pass. Identical paths in multiple views do not count as conflicts.
use crate::app::{App, Tab, TabKind};
use rustc_hash::{FxHashMap, FxHasher};
use std::{
    collections::VecDeque,
    ffi::OsStr,
    hash::{Hash, Hasher},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

type Name<'a> = (TabKind, &'a OsStr);

fn parent(tab: &Tab) -> Option<(Name<'_>, &Path)> {
    let path = tab.editor.file_path.as_deref()?;
    // Extract both in one backwards parse. Unrelated names need no traversal
    // of their parent directories during the all-tabs pass.
    let mut components = path.components();
    let Component::Normal(name) = components.next_back()? else {
        return None;
    };
    Some(((tab.kind, name), components.as_path()))
}

fn parts(path: &Path) -> impl DoubleEndedIterator<Item = Component<'_>> {
    path.components().filter(|part| *part != Component::CurDir)
}

/// Shared trailing directories, and whether both lexical paths are identical.
fn common_tail(left: &Path, right: &Path) -> (usize, bool) {
    let mut left = parts(left).rev();
    let mut right = parts(right).rev();
    let mut count = 0;
    loop {
        match (left.next(), right.next()) {
            (None, None) => return (count, true),
            (Some(a), Some(b)) if a == b => count += 1,
            _ => return (count, false),
        }
    }
}

fn active_tabs(app: &App) -> &[Tab] {
    app.groups
        .get(app.active_group)
        .filter(|group| !group.tabs.is_empty())
        .map_or(app.tabs.as_slice(), |group| group.tabs.as_slice())
}

fn all_tabs(app: &App) -> impl Iterator<Item = &Tab> {
    active_tabs(app).iter().chain(
        app.groups
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != app.active_group)
            .flat_map(|(_, group)| group.tabs.iter()),
    )
}

/// Direct lookup for short strips and individual geometry queries.
pub(super) fn context(app: &App, tab: &Tab) -> Option<String> {
    let (name, path) = parent(tab)?;
    let mut required = 0;
    let mut shared = usize::MAX;
    for other in all_tabs(app) {
        let Some((other_name, other_path)) = parent(other) else {
            continue;
        };
        if other_name != name {
            continue;
        }
        let (common, identical) = common_tail(path, other_path);
        shared = shared.min(common);
        if !identical {
            required = required.max(common + 1);
        }
    }
    (required > 0).then(|| format_context(path, required, shared))
}

#[derive(Default)]
struct Node<'a> {
    children: FxHashMap<Component<'a>, usize>,
    required: usize,
    candidate: bool,
}

struct Group<'a> {
    root: usize,
    prototype: &'a Path,
    shared: usize,
}

struct Builder<'a> {
    groups: FxHashMap<Name<'a>, Group<'a>>,
    nodes: Vec<Node<'a>>,
}

impl<'a> Builder<'a> {
    fn for_candidates(app: &'a App, candidates: &[&'a Tab]) -> Self {
        let mut result = Self {
            groups: FxHashMap::default(),
            nodes: Vec::new(),
        };
        for tab in candidates {
            result.insert(tab);
        }
        for tab in all_tabs(app) {
            result.observe(tab);
        }
        result
    }

    fn insert(&mut self, tab: &'a Tab) {
        let Some((name, path)) = parent(tab) else {
            return;
        };
        let root = if let Some(group) = self.groups.get(&name) {
            group.root
        } else {
            let root = self.nodes.len();
            self.nodes.push(Node::default());
            self.groups.insert(
                name,
                Group {
                    root,
                    prototype: path,
                    shared: usize::MAX,
                },
            );
            root
        };
        let mut node = root;
        for part in parts(path).rev() {
            node = if let Some(&next) = self.nodes[node].children.get(&part) {
                next
            } else {
                let next = self.nodes.len();
                self.nodes.push(Node::default());
                self.nodes[node].children.insert(part, next);
                next
            };
        }
        self.nodes[node].candidate = true;
    }

    fn observe(&mut self, tab: &'a Tab) {
        let Some((name, path)) = parent(tab) else {
            return;
        };
        let Some(group) = self.groups.get_mut(&name) else {
            return;
        };
        group.shared = group.shared.min(common_tail(group.prototype, path).0);
        let mut node = group.root;
        let mut depth = 0;
        for part in parts(path).rev() {
            // Conflicts between two candidate branches matter even when no
            // off-screen path diverges here during this pass.
            if self.nodes[node].children.len() > 1 {
                self.nodes[node].required = self.nodes[node].required.max(depth + 1);
            }
            let Some(&next) = self.nodes[node].children.get(&part) else {
                self.nodes[node].required = self.nodes[node].required.max(depth + 1);
                return;
            };
            node = next;
            depth += 1;
        }
        if !self.nodes[node].children.is_empty() {
            self.nodes[node].required = self.nodes[node].required.max(depth + 1);
        }
    }

    /// Outer None means this path was outside the candidate viewport.
    pub(super) fn context(&self, tab: &Tab) -> Option<Option<String>> {
        let (name, path) = parent(tab)?;
        let group = self.groups.get(&name)?;
        let mut node = group.root;
        let mut required = self.nodes[node].required;
        for part in parts(path).rev() {
            node = *self.nodes[node].children.get(&part)?;
            required = required.max(self.nodes[node].required);
        }
        self.nodes[node]
            .candidate
            .then(|| (required > 0).then(|| format_context(path, required, group.shared)))
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Fingerprint {
    count: usize,
    sum: u64,
    xor: u64,
}

fn fingerprint<'a>(tabs: impl Iterator<Item = &'a Tab>) -> Fingerprint {
    let mut result = Fingerprint::default();
    for tab in tabs {
        let mut hash = FxHasher::default();
        tab.kind.hash(&mut hash);
        tab.editor
            .file_path
            .as_ref()
            .map(|path| path.as_os_str().as_encoded_bytes())
            .hash(&mut hash);
        let value = hash.finish();
        result.count += 1;
        result.sum = result.sum.wrapping_add(value);
        result.xor ^= value.rotate_left(23);
    }
    result
}

struct Labels {
    contexts: FxHashMap<TabKind, FxHashMap<PathBuf, Option<String>>>,
    #[cfg(test)]
    names: usize,
    #[cfg(test)]
    nodes: usize,
}

/// A bounded set of recent viewports. The fingerprint checks raw path bytes,
/// never filesystem metadata or component-by-component path equality. It is
/// order independent, so reordering the same visible tabs can reuse hints.
#[derive(Default)]
pub(crate) struct NameCache {
    universe: Option<Fingerprint>,
    entries: VecDeque<(Fingerprint, Arc<Labels>)>,
    #[cfg(test)]
    builds: usize,
}

pub(super) struct TabNameDuplicates {
    labels: Arc<Labels>,
}

impl TabNameDuplicates {
    pub(super) fn for_viewport(app: &App, width: u16) -> Option<Self> {
        Self::for_tabs(
            app,
            active_tabs(app),
            app.active_tab,
            app.tab_scroll_offset,
            width,
        )
    }

    pub(super) fn for_tabs(
        app: &App,
        tabs: &[Tab],
        active: usize,
        scroll: usize,
        width: u16,
    ) -> Option<Self> {
        Self::for_candidates(app, tabs, active, scroll, if width == 0 { 0 } else { usize::from(width) / 4 + 2 })
    }

    /// Continuous scrolling needs widths across the whole strip. Share the
    /// duplicate-name index rather than scanning all paths for every label.
    pub(super) fn for_scroll(app: &App, tabs: &[Tab], active: usize) -> Option<Self> {
        Self::for_candidates(app, tabs, active, 0, usize::MAX)
    }

    fn for_candidates(app: &App, tabs: &[Tab], active: usize, scroll: usize, limit: usize) -> Option<Self> {
        let count = active_tabs(app).len()
            + app
                .groups
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != app.active_group)
                .map(|(_, group)| group.tabs.len())
                .sum::<usize>();
        if count <= 64 || limit == 0 {
            return None;
        }
        // Collapsed groups do not consume tab cells. Count renderable tabs,
        // rather than raw indices, to keep later pinned/active tabs cached.
        let candidates: Vec<_> = tabs
            .iter()
            .enumerate()
            .skip(scroll)
            .filter(|(index, tab)| {
                *index == active
                    || tab.pinned
                    || !app.tab_group(tab).is_some_and(|group| group.collapsed)
            })
            .take(limit)
            .map(|(_, tab)| tab)
            .collect();
        let universe = fingerprint(all_tabs(app));
        let key = fingerprint(candidates.iter().copied());
        {
            let mut cache = app.tab_name_cache.borrow_mut();
            if cache.universe != Some(universe) {
                cache.entries.clear();
                cache.universe = Some(universe);
            }
            if let Some(position) = cache.entries.iter().position(|(cached, _)| *cached == key) {
                let entry = cache.entries.remove(position).unwrap();
                let labels = Arc::clone(&entry.1);
                cache.entries.push_front(entry);
                return Some(Self { labels });
            }
        }
        let builder = Builder::for_candidates(app, &candidates);
        let mut contexts = FxHashMap::<TabKind, FxHashMap<PathBuf, Option<String>>>::default();
        for tab in candidates {
            if let Some(path) = &tab.editor.file_path
                && let Some(context) = builder.context(tab)
            {
                contexts
                    .entry(tab.kind)
                    .or_default()
                    .insert(path.clone(), context);
            }
        }
        let labels = Arc::new(Labels {
            contexts,
            #[cfg(test)]
            names: builder.groups.len(),
            #[cfg(test)]
            nodes: builder.nodes.len(),
        });
        let mut cache = app.tab_name_cache.borrow_mut();
        #[cfg(test)]
        {
            cache.builds += 1;
        }
        cache.entries.push_front((key, Arc::clone(&labels)));
        cache.entries.truncate(16);
        Some(Self { labels })
    }

    /// Outer None requests the direct fallback for a path outside the viewport.
    pub(super) fn context(&self, tab: &Tab) -> Option<Option<String>> {
        self.labels
            .contexts
            .get(&tab.kind)?
            .get(tab.editor.file_path.as_deref()?)
            .cloned()
    }

    #[cfg(test)]
    pub(super) fn name_count(&self) -> usize {
        self.labels.names
    }
    #[cfg(test)]
    pub(super) fn node_count(&self) -> usize {
        self.labels.nodes
    }
}

fn format_context(path: &Path, required: usize, shared: usize) -> String {
    let components: Vec<_> = parts(path).collect();
    if components.is_empty() {
        return ".".into();
    }
    let selected = required.min(components.len());
    let directories = components
        .iter()
        .rev()
        .take_while(|part| matches!(part, Component::Normal(_) | Component::ParentDir))
        .count();
    let tail = shared.min(selected.saturating_sub(1)).min(directories);
    let mut start = components.len() - selected;
    // A Windows rooted suffix must retain its drive/UNC prefix when available.
    if start > 0
        && components[start] == Component::RootDir
        && matches!(components[start - 1], Component::Prefix(_))
    {
        start -= 1;
    }
    let middle: PathBuf = components[start..components.len() - tail].iter().collect();
    let mut result = String::new();
    if start > 0 {
        result.push('…');
        result.push(std::path::MAIN_SEPARATOR);
    }
    push_visible(&mut result, middle.as_os_str());
    if tail > 0 {
        let drive_relative = components.len() - tail - start == 1
            && matches!(components[start], Component::Prefix(_))
            && !middle.has_root();
        if !drive_relative && !result.ends_with(std::path::MAIN_SEPARATOR) {
            result.push(std::path::MAIN_SEPARATOR);
        }
        result.push('…');
    }
    result
}

/// Quote exceptional names so literal ellipses, controls and invalid OS bytes
/// cannot be confused with omitted directories. Ordinary paths stay unchanged.
fn push_visible(output: &mut String, value: &OsStr) {
    if let Some(text) = value.to_str()
        && !text
            .chars()
            .any(|ch| ch == '"' || ch == '…' || ch.is_control())
    {
        output.push_str(text);
        return;
    }
    output.push('"');
    let mut bytes = value.as_encoded_bytes();
    while !bytes.is_empty() {
        let (valid, invalid) = match std::str::from_utf8(bytes) {
            Ok(text) => (text, 0),
            Err(error) => (
                std::str::from_utf8(&bytes[..error.valid_up_to()]).unwrap(),
                error
                    .error_len()
                    .unwrap_or(bytes.len() - error.valid_up_to()),
            ),
        };
        for ch in valid.chars() {
            if ch == '\\' || ch == '"' || ch == '…' || ch.is_control() {
                output.extend(ch.escape_default());
            } else {
                output.push(ch);
            }
        }
        bytes = &bytes[valid.len()..];
        for &byte in &bytes[..invalid] {
            push_byte(output, byte);
        }
        bytes = &bytes[invalid..];
    }
    output.push('"');
}

fn push_byte(output: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    output.push_str("\\x");
    output.push(HEX[usize::from(byte >> 4)] as char);
    output.push(HEX[usize::from(byte & 15)] as char);
}

#[cfg(test)]
#[path = "tab_path_labels_tests.rs"]
mod tests;
