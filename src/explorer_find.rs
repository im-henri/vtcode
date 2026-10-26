use std::path::PathBuf;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::explorer::FileEntry;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Focus {
    #[default]
    Tree,
    Input,
    Filter,
    Fuzzy,
    Regex,
    Collapse,
    Close,
}

pub(crate) struct State {
    pub open: bool,
    pub query: String,
    pub cursor: usize,
    pub anchor: Option<usize>,
    pub focus: Focus,
    pub filter: bool,
    pub fuzzy: bool,
    pub regex: bool,
    pub loading: bool,
    pub select_first: bool,
    pub results: Matches,
    pub collapsed: FxHashSet<PathBuf>,
    pub saved_view: Option<View>,
    pub cancel: Option<crate::background::CancellationToken>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            open: false,
            query: String::new(),
            cursor: 0,
            anchor: None,
            focus: Focus::Tree,
            filter: false,
            fuzzy: true,
            regex: false,
            loading: false,
            select_first: false,
            results: Matches::default(),
            collapsed: FxHashSet::default(),
            saved_view: None,
            cancel: None,
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
    }
}

pub(crate) struct View {
    pub entries: Vec<FileEntry>,
    pub selected: usize,
    pub offset: usize,
}

#[derive(Clone)]
pub(crate) struct Query {
    pub text: String,
    pub filter: bool,
    pub fuzzy: bool,
    pub regex: bool,
    pub collapsed: FxHashSet<PathBuf>,
}

impl State {
    pub fn request(&self) -> Option<Query> {
        (self.open && !self.query.is_empty()).then(|| Query {
            text: self.query.clone(),
            filter: self.filter,
            fuzzy: self.fuzzy,
            regex: self.regex,
            collapsed: self.collapsed.clone(),
        })
    }

    pub fn filtering(&self) -> bool {
        self.open && self.filter && !self.query.is_empty()
    }
}

#[derive(Default, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct Match {
    /// Zero-width regular expressions match a name without highlighting a character.
    pub matched: bool,
    /// Character indices, independent of UTF-8 byte offsets or display width.
    pub indices: Vec<usize>,
    pub descendants: usize,
}

#[derive(Default)]
pub(crate) struct Matches {
    pub count: usize,
    pub rows: FxHashMap<PathBuf, Match>,
    pub error: Option<String>,
    /// All matching paths and their ancestors, including collapsed descendants.
    /// Folder toggles project this snapshot without scanning or matching again.
    pub filtered_tree: Option<Vec<FileEntry>>,
}

impl Matches {
    pub fn filtered_entries(&self, collapsed: &FxHashSet<PathBuf>) -> Option<Vec<FileEntry>> {
        let tree = self.filtered_tree.as_ref()?;
        let mut visible = Vec::new();
        let mut collapsed_depth = None;
        for entry in tree {
            if collapsed_depth.is_some_and(|depth| entry.depth > depth) {
                continue;
            }
            let mut entry = entry.clone();
            entry.expanded = entry.is_dir && !collapsed.contains(&entry.path);
            collapsed_depth = (entry.is_dir && !entry.expanded).then_some(entry.depth);
            visible.push(entry);
        }
        Some(visible)
    }
}

pub(crate) struct NameMatcher {
    pattern: Vec<char>,
    fuzzy: bool,
    regex: Option<regress::Regex>,
}

impl NameMatcher {
    /// Compile once on the Explorer worker, using the same regex syntax as Find.
    pub(crate) fn new(query: &Query) -> Result<Self, String> {
        let regex = query
            .regex
            .then(|| {
                regress::Regex::with_flags(
                    &query.text,
                    regress::Flags {
                        icase: true,
                        unicode: true,
                        ..Default::default()
                    },
                )
                .map_err(|error| crate::search_regex::error_summary(&error.to_string()))
            })
            .transpose()?;
        Ok(Self {
            pattern: query.text.to_lowercase().chars().collect(),
            fuzzy: query.fuzzy,
            regex,
        })
    }

    fn indices(&self, name: &str) -> Option<Vec<usize>> {
        if let Some(regex) = &self.regex {
            let mut indices = Vec::new();
            let mut matched = false;
            let (mut byte, mut col) = (0, 0);
            for found in regex.find_iter(name) {
                matched = true;
                let found = found.range();
                let start = col + name[byte..found.start].chars().count();
                let end = start + name[found.start..found.end].chars().count();
                indices.extend(start..end);
                byte = found.end;
                col = end;
            }
            return matched.then_some(indices);
        }
        if self.pattern.is_empty() {
            return None;
        }
        let folded: Vec<(usize, char)> = name
            .chars()
            .enumerate()
            .flat_map(|(index, ch)| ch.to_lowercase().map(move |ch| (index, ch)))
            .collect();
        let mut indices = Vec::with_capacity(self.pattern.len());
        if self.fuzzy {
            let mut pattern = self.pattern.iter();
            let mut next = pattern.next();
            for (index, ch) in folded {
                if next == Some(&ch) {
                    indices.push(index);
                    next = pattern.next();
                    if next.is_none() {
                        break;
                    }
                }
            }
            if next.is_some() {
                return None;
            }
        } else if let Some(start) = folded
            .windows(self.pattern.len())
            .position(|window| window.iter().map(|(_, ch)| ch).eq(self.pattern.iter()))
        {
            indices.extend(
                folded[start..start + self.pattern.len()]
                    .iter()
                    .map(|(index, _)| *index),
            );
        }
        indices.dedup();
        (!indices.is_empty()).then_some(indices)
    }
}

/// Search every name, then project back into a tree. Highlighting preserves
/// expansion; filtering reveals matching ancestors without changing the normal
/// tree's saved expansion state. Runs on the Explorer worker, never during draw.
pub(crate) fn project(
    entries: Vec<FileEntry>,
    query: &Query,
    matcher: &NameMatcher,
    cancel: Option<&crate::background::CancellationToken>,
    decorate: impl FnOnce(&mut [FileEntry]),
) -> (Vec<FileEntry>, Matches) {
    let mut parents = Vec::with_capacity(entries.len());
    let mut stack: Vec<usize> = Vec::new();
    let mut matches = Vec::with_capacity(entries.len());
    let mut count = 0;
    for (i, entry) in entries.iter().enumerate() {
        if cancel.is_some_and(|cancel| cancel.is_cancelled()) {
            return (Vec::new(), Matches::default());
        }
        while stack
            .last()
            .is_some_and(|&p| entries[p].depth >= entry.depth)
        {
            stack.pop();
        }
        parents.push(stack.last().copied());
        if entry.is_dir {
            stack.push(i);
        }
        let indices = matcher.indices(&entry.name);
        count += usize::from(indices.is_some());
        matches.push(Match {
            matched: indices.is_some(),
            indices: indices.unwrap_or_default(),
            descendants: 0,
        });
    }
    for i in (0..entries.len()).rev() {
        if let Some(parent) = parents[i] {
            matches[parent].descendants += matches[i].descendants + usize::from(matches[i].matched);
        }
    }
    let mut rows = FxHashMap::default();
    if query.filter {
        let mut tree = Vec::new();
        for (entry, matched) in entries.into_iter().zip(matches) {
            if matched.matched || matched.descendants > 0 {
                rows.insert(entry.path.clone(), matched);
                tree.push(entry);
            }
        }
        // Decorate retained results before cloning the visible projection; each
        // path is resolved once, including results beneath collapsed folders.
        decorate(&mut tree);
        let matches = Matches {
            count,
            rows,
            filtered_tree: Some(tree),
            ..Default::default()
        };
        return (matches.filtered_entries(&query.collapsed).unwrap(), matches);
    }
    let mut visible = Vec::new();
    let mut collapsed_depth = None;
    for (entry, matched) in entries.into_iter().zip(matches) {
        if collapsed_depth.is_some_and(|depth| entry.depth > depth) {
            continue;
        }
        collapsed_depth = None;
        let has_match = matched.matched || matched.descendants > 0;
        if entry.is_dir && !entry.expanded {
            collapsed_depth = Some(entry.depth);
        }
        if has_match {
            rows.insert(entry.path.clone(), matched);
        }
        visible.push(entry);
    }
    decorate(&mut visible);
    (
        visible,
        Matches {
            count,
            rows,
            ..Default::default()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::FileExplorer;

    fn matcher(text: &str, fuzzy: bool) -> NameMatcher {
        NameMatcher::new(&Query {
            text: text.into(),
            fuzzy,
            regex: false,
            filter: false,
            collapsed: Default::default(),
        })
        .unwrap()
    }

    #[test]
    fn contiguous_and_fuzzy_matching_are_case_insensitive_and_unicode_safe() {
        assert_eq!(
            matcher("EXR", true).indices("Explorer.rs"),
            Some(vec![0, 1, 5])
        );
        assert_eq!(matcher("EXR", false).indices("Explorer.rs"), None);
        assert_eq!(
            matcher("LOR", false).indices("Explorer.rs"),
            Some(vec![3, 4, 5])
        );
        assert_eq!(matcher("rex", true).indices("Explorer.rs"), None);
        assert_eq!(matcher("ä文", true).indices("Äbc文件.rs"), Some(vec![0, 3]));
        assert_eq!(matcher("文I", false).indices("文件İ.rs"), None);
        assert_eq!(matcher("İ", false).indices("文件İ.rs"), Some(vec![2]));
    }

    fn regex(text: &str) -> Result<NameMatcher, String> {
        NameMatcher::new(&Query {
            text: text.into(),
            filter: false,
            fuzzy: false,
            regex: true,
            collapsed: Default::default(),
        })
    }

    #[test]
    fn regex_matches_names_with_anchors_alternatives_escapes_and_unicode_highlights() {
        let pattern = regex(r"^app(_\w+)?\.(rs|toml)$").unwrap();
        for name in ["app.rs", "APP_INPUT.RS", "app_config.toml"] {
            assert_eq!(
                pattern.indices(name),
                Some((0..name.chars().count()).collect())
            );
        }
        for name in ["myapp.rs", "app.rs.bak", "appXrs", "src/app.rs", "app.txt"] {
            assert_eq!(pattern.indices(name), None, "{name}");
        }
        assert_eq!(
            regex(r"ä|文|🦀|\.rs$").unwrap().indices("Ä🦀bc文.Rs"),
            Some(vec![0, 1, 4, 5, 6, 7])
        );
        assert_eq!(
            regex(r"(?<=app)\.rs$").unwrap().indices("app.rs"),
            Some(vec![3, 4, 5])
        );
        assert_eq!(
            regex(r"^(\w+)_\1\.rs$").unwrap().indices("app_app.rs"),
            Some((0..10).collect())
        );
        assert_eq!(regex(r"^|$").unwrap().indices("文件.rs"), Some(Vec::new()));
        assert_eq!(matcher(r"\.rs$", false).indices("app.rs"), None);
        for pattern in ["[", "(", "*"] {
            assert!(regex(pattern).err().unwrap().starts_with("Invalid regex:"));
        }
    }

    #[test]
    fn zero_width_regex_results_can_be_filtered_counted_and_navigated() {
        let (workspace, mut explorer) = fixture();
        explorer.find.open = true;
        explorer.find.regex = true;
        explorer.find.query = "^(?=app)".into();
        explorer.prepare_find();
        refresh(&mut explorer);
        assert_eq!(explorer.find.results.count, 3);
        assert_eq!(
            explorer.find.results.rows[&workspace.path().join("src")].descendants,
            2
        );
        explorer.find.filter = true;
        explorer.prepare_find();
        refresh(&mut explorer);
        assert_eq!(explorer.find.results.count, 3);
        for path in [
            ".hidden/app_config.rs",
            "src/deep/app_input.rs",
            "src/app.rs",
        ] {
            assert_eq!(
                explorer.entries[explorer.selected].path,
                workspace.path().join(path)
            );
            let hit = &explorer.find.results.rows[&workspace.path().join(path)];
            assert!(hit.matched && hit.indices.is_empty());
            explorer.select_find_match(false, false);
        }
        explorer.find.query = "^deep$".into();
        explorer.prepare_find();
        refresh(&mut explorer);
        assert_eq!(explorer.find.results.count, 1);
        assert_eq!(explorer.entries[explorer.selected].name, "deep");
    }

    #[test]
    fn invalid_regex_preserves_normal_expansion_and_recovers_without_stale_hits() {
        let (_workspace, mut explorer) = fixture();
        let original: Vec<_> = explorer
            .entries
            .iter()
            .map(|entry| (entry.path.clone(), entry.expanded))
            .collect();
        explorer.find.open = true;
        explorer.find.regex = true;
        for filter in [false, true] {
            explorer.find.filter = filter;
            explorer.find.query = "[".into();
            explorer.prepare_find();
            refresh(&mut explorer);
            assert!(explorer.find.results.error.is_some());
            assert_eq!(explorer.find.results.count, 0);
            assert!(!explorer.find.loading);
            if filter {
                assert!(explorer.entries.is_empty());
            } else {
                assert_eq!(
                    explorer
                        .entries
                        .iter()
                        .map(|entry| (entry.path.clone(), entry.expanded))
                        .collect::<Vec<_>>(),
                    original
                );
            }
            explorer.find.query = r"\.rs$".into();
            explorer.prepare_find();
            refresh(&mut explorer);
            assert!(explorer.find.results.error.is_none());
            assert_eq!(explorer.find.results.count, 3);
        }
        explorer.find.query.clear();
        explorer.prepare_find();
        refresh(&mut explorer);
        assert_eq!(
            explorer
                .entries
                .iter()
                .map(|entry| (entry.path.clone(), entry.expanded))
                .collect::<Vec<_>>(),
            original
        );
    }

    pub(super) fn fixture() -> (tempfile::TempDir, FileExplorer) {
        let workspace = tempfile::tempdir().unwrap();
        for path in [
            "src/deep/app_input.rs",
            "src/deep/unrelated.txt",
            "src/app.rs",
            "docs/readme.md",
            ".hidden/app_config.rs",
            "zeta.txt",
        ] {
            let path = workspace.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "test").unwrap();
        }
        let mut explorer = FileExplorer::new(workspace.path().to_path_buf());
        explorer.visible_height = 20;
        (workspace, explorer)
    }

    fn refresh(explorer: &mut FileExplorer) {
        let (root, entries, matches) =
            FileExplorer::build_refresh_entries(explorer.refresh_request());
        explorer.apply_refresh_entries(root, entries, matches);
    }

    #[test]
    fn highlight_counts_matches_in_collapsed_folders_and_filter_reveals_paths() {
        let (workspace, mut explorer) = fixture();
        let original: Vec<_> = explorer.entries.iter().map(|e| e.path.clone()).collect();
        explorer.find.open = true;
        explorer.find.query = "app".into();
        explorer.prepare_find();
        refresh(&mut explorer);
        assert_eq!(explorer.find.results.count, 3);
        assert_eq!(
            explorer.find.results.rows[&workspace.path().join("src")].descendants,
            2
        );
        assert_eq!(
            explorer
                .entries
                .iter()
                .map(|e| e.path.clone())
                .collect::<Vec<_>>(),
            original
        );
        assert!(explorer.entries.iter().all(|e| !e.expanded));
        explorer.find.filter = true;
        explorer.prepare_find();
        refresh(&mut explorer);
        let names: Vec<_> = explorer.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                ".hidden",
                "app_config.rs",
                "src",
                "deep",
                "app_input.rs",
                "app.rs"
            ]
        );
        assert_eq!(explorer.entries[explorer.selected].name, "app_config.rs");
        explorer.select_find_match(false, false);
        assert_eq!(explorer.entries[explorer.selected].name, "app_input.rs");
        let (opened, changed) = explorer.toggle_or_open_deferred();
        assert_eq!(opened, Some(workspace.path().join("src/deep/app_input.rs")));
        assert!(!changed);
        explorer.select_find_match(true, false);
        assert_eq!(explorer.entries[explorer.selected].name, "app_config.rs");
        assert!(!explorer.reveal_file_deferred(&workspace.path().join("zeta.txt")));
    }

    #[test]
    fn filter_restores_selection_scroll_and_expansion_even_after_collapsing_results() {
        let (workspace, mut explorer) = fixture();
        explorer.selected = explorer
            .entries
            .iter()
            .position(|e| e.name == "src")
            .unwrap();
        explorer.toggle_or_open_deferred();
        refresh(&mut explorer);
        explorer.visible_height = 2;
        explorer.selected = explorer.entries.len() - 1;
        explorer.scroll_offset = explorer.entries.len() - 2;
        let selected = explorer.selected;
        let offset = explorer.scroll_offset;
        let original: Vec<_> = explorer
            .entries
            .iter()
            .map(|e| (e.path.clone(), e.expanded))
            .collect();
        explorer.find.open = true;
        explorer.find.filter = true;
        explorer.find.query = "app".into();
        explorer.prepare_find();
        refresh(&mut explorer);
        explorer.selected = explorer
            .entries
            .iter()
            .position(|e| e.name == "src")
            .unwrap();
        explorer.collapse_current_deferred();
        refresh(&mut explorer);
        assert!(!explorer.entries.iter().any(|e| e.name == "app.rs"));
        explorer.find.query = "no-such-match".into();
        explorer.prepare_find();
        refresh(&mut explorer);
        assert!(explorer.entries.is_empty());
        explorer.find.query.clear();
        explorer.prepare_find();
        assert_eq!(explorer.selected, selected);
        assert_eq!(explorer.scroll_offset, offset);
        refresh(&mut explorer);
        assert_eq!(
            explorer
                .entries
                .iter()
                .map(|e| (e.path.clone(), e.expanded))
                .collect::<Vec<_>>(),
            original
        );
        assert_eq!(
            explorer.entries[explorer.selected].path,
            workspace.path().join("zeta.txt")
        );
    }

    #[test]
    fn directory_names_match_and_searches_can_be_cancelled() {
        let (_workspace, mut explorer) = fixture();
        explorer.find.open = true;
        explorer.find.filter = true;
        explorer.find.query = "deep".into();
        explorer.prepare_find();
        refresh(&mut explorer);
        assert_eq!(explorer.find.results.count, 1);
        assert_eq!(
            explorer
                .entries
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            ["src", "deep"]
        );
        let cancel = crate::background::CancellationToken::new();
        cancel.cancel();
        explorer.find.cancel = Some(cancel);
        let (_, entries, matches) = FileExplorer::build_refresh_entries(explorer.refresh_request());
        assert!(entries.is_empty());
        assert_eq!(matches.count, 0);
    }
}
