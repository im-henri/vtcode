//! Keep large, clean local tabs backed by disk when their contents are idle.
use std::path::Path;

use crate::app::{App, Tab, TabKind};
use crate::session::{RestoreLoad, SessionRestore};

pub(crate) const MIN_DEFERRED_FILE_BYTES: usize = 4 * 1024 * 1024;
const INACTIVE_FILE_CACHE_BYTES: usize = 64 * 1024 * 1024;

impl crate::editor::Editor {
    pub(crate) fn has_deferred_file(&self) -> bool {
        self.bg_loading
            && self
                .pending_session_restore
                .as_ref()
                .is_some_and(|restore| restore.load == RestoreLoad::Deferred)
    }

    pub(crate) fn restoring_deferred_file(&self) -> bool {
        self.bg_loading
            && self
                .pending_session_restore
                .as_ref()
                .is_some_and(|restore| restore.load != RestoreLoad::Eager)
    }
}

impl App {
    pub(crate) fn apply_deferred_session_file(
        &mut self,
        path: &Path,
        restore_id: u64,
        file_size: u64,
        modified: Option<std::time::SystemTime>,
    ) -> bool {
        let mut applied = false;
        for tab in self
            .tabs
            .iter_mut()
            .chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs))
        {
            let editor = &mut tab.editor;
            if editor.bg_loading
                && editor.file_path.as_deref() == Some(path)
                && let Some(restore) = &mut editor.pending_session_restore
                && restore.id == restore_id
                && restore.can_defer()
            {
                restore.load = RestoreLoad::Deferred;
                editor.file_size = file_size;
                editor.last_disk_modified = modified;
                applied = true;
            }
        }
        if applied {
            // The user may have reached the tab while its disk probe ran.
            self.load_visible_deferred_files();
        }
        applied
    }

    pub(crate) fn load_deferred_file(&mut self, path: &Path) {
        let mut restores: Vec<SessionRestore> = Vec::new();
        for tab in self
            .tabs
            .iter_mut()
            .chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs))
        {
            let editor = &mut tab.editor;
            if editor.file_path.as_deref() == Some(path) && editor.has_deferred_file() {
                let restore = editor.pending_session_restore.as_mut().unwrap();
                restore.load = RestoreLoad::Requested;
                if !restores.iter().any(|existing| existing.id == restore.id) {
                    restores.push(restore.clone());
                }
            }
        }
        if !restores.is_empty() {
            self.queue_background_session_file_load(path.to_owned(), restores, true);
        }
    }

    pub(crate) fn load_active_deferred_file(&mut self) {
        if let Some(path) = deferred_path(self.tabs.get(self.active_tab)).map(Path::to_owned) {
            self.load_deferred_file(&path);
        }
    }

    pub(crate) fn load_visible_deferred_files(&mut self) {
        self.load_active_deferred_file();
        let mut paths = Vec::new();
        if let Some(tree) = &self.split_tree {
            tree.visit_leaves(&mut |group| {
                if group != self.active_group
                    && let Some(pane) = self.groups.get(group)
                    && let Some(path) = deferred_path(pane.tabs.get(pane.active_tab))
                {
                    paths.push(path.to_owned());
                }
            });
        }
        for path in paths {
            self.load_deferred_file(&path);
        }
    }

    pub(crate) fn trim_large_file_cache(&mut self) {
        if self.bg_global_replace_running || !self.pending_lsp_workspace_edits.is_empty() {
            return;
        }
        let mut candidates = Vec::new();
        let mut bytes = 0usize;
        for (group, tabs, active, mru) in std::iter::once((
            self.active_group,
            &self.tabs,
            self.active_tab,
            &self.tab_mru_order,
        ))
        .chain(self.groups.iter().enumerate().filter_map(|(group, pane)| {
            (group != self.active_group).then_some((
                group,
                &pane.tabs,
                pane.active_tab,
                &pane.tab_mru_order,
            ))
        })) {
            let start = candidates.len();
            for (index, tab) in tabs.iter().enumerate() {
                if index != active
                    && let Some(size) = self.cacheable_file_bytes(tab)
                {
                    bytes = bytes.saturating_add(size);
                    candidates.push((group, index, size, usize::MAX));
                }
            }
            if start != candidates.len() {
                let mut ranks = vec![usize::MAX; tabs.len()];
                for (rank, &index) in mru.iter().enumerate() {
                    if let Some(entry) = ranks.get_mut(index) {
                        *entry = rank;
                    }
                }
                for (_, index, _, rank) in &mut candidates[start..] {
                    *rank = ranks[*index];
                }
            }
        }
        if bytes <= INACTIVE_FILE_CACHE_BYTES {
            return;
        }
        // Prefer the tabs most recently used in the focused pane.
        candidates.sort_unstable_by_key(|&(group, index, _, rank)| {
            (group != self.active_group, rank, group, index)
        });
        let mut retired = Vec::new();
        while bytes > INACTIVE_FILE_CACHE_BYTES {
            let (group, index, size, _) = candidates.pop().unwrap();
            let tab = if group == self.active_group {
                &mut self.tabs[index]
            } else {
                &mut self.groups[group].tabs[index]
            };
            let path = tab.editor.file_path.clone().unwrap();
            let mut restore = SessionRestore::new(Self::saved_file_tab(tab), Default::default());
            restore.load = RestoreLoad::Deferred;
            let editor = &mut tab.editor;
            editor.pending_reopen_view = Some(crate::editor_view_state::ViewState::capture(editor));
            editor.pending_session_restore = Some(restore);
            editor.bg_loading = true;
            editor.read_only = true;
            editor.content_version = editor.content_version.wrapping_add(1);
            editor.cached_wrapped_total = None;
            editor.cached_row_offsets = None;
            editor.fold_hint_cache.get_mut().clear();
            *editor.conditional_fold_cache.get_mut() = Default::default();
            *editor.render_line_cache.get_mut() = Default::default();
            editor.minimap_cache.get_mut().clear();
            editor.clear_git_line_status();
            editor.reset_unsaved_diff_cache();
            editor.merge_conflicts = Default::default();
            editor.selection_history_source = None;
            editor.document = Default::default();
            editor.saved_rope = None;
            retired.push(std::mem::replace(
                &mut editor.rope,
                ropey::Rope::from_str("Loading file..."),
            ));
            let view = editor.navigation_id;
            self.forget_editor_view_caches(view);
            self.pending_lsp_syncs.remove(&path);
            self.lsp_manager.did_close(&path);
            bytes -= size;
        }
        self.desks.sync_stamp = None;
        let _ = self
            .background
            .pool_sender()
            .send(Box::new(move || drop(retired)));
    }

    pub(crate) fn forget_editor_view_caches(&mut self, view: u64) {
        self.blame_documents.remove(&view);
        self.animations.forget_view(view);
        crate::ui_editor::forget_word_snapshots(self, view);
    }

    fn cacheable_file_bytes(&self, tab: &Tab) -> Option<usize> {
        let editor = &tab.editor;
        let size = editor.rope.len_bytes();
        let path = editor.file_path.as_deref()?;
        // A parked tab can reconstruct its contents from disk. Buffers
        // with local state that couldn't be reconstructed keep their Rope.
        (size >= MIN_DEFERRED_FILE_BYTES
            && tab.kind == TabKind::File
            && !crate::remote::workspace::is_view_key(path)
            && !editor.document_has_other_views()
            && !editor.modified
            && !editor.disk_unavailable
            && editor.save_content_error().is_none()
            && editor
                .saved_rope
                .as_ref()
                .is_some_and(|saved| saved.is_instance(&editor.rope))
            && editor.read_only == self.default_read_only
            && editor.word_wrap == (self.default_word_wrap && !editor.large_file_mode)
            && editor.selection_anchor.is_none()
            && editor.extra_cursors.is_empty()
            && editor.selection_history.is_empty()
            && !editor.snippet_active
            && editor.undo_stack.is_empty()
            && editor.redo_stack.is_empty()
            && !tab.file_search.search_active
            && !self.pending_save_as.contains_key(&editor.save_identity)
            && !self.pending_file_saves.contains_key(path)
            && !self.deferred_file_saves.contains_key(path)
            && !self.pending_file_reloads.contains(path)
            && self.lsp_format_target_path.as_deref() != Some(path)
            && !self.chat.rollback.file_busy(path))
        .then_some(size)
    }
}

fn deferred_path(tab: Option<&Tab>) -> Option<&Path> {
    let editor = &tab.filter(|tab| tab.editor.has_deferred_file())?.editor;
    editor.file_path.as_deref()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;
    use std::path::PathBuf;
    use std::time::Duration;

    fn write_file(dir: &Path, name: &str, bytes: usize, line: &str) -> PathBuf {
        let path = dir.join(name);
        let text = line.repeat(bytes.div_ceil(line.len()).max(4));
        std::fs::write(&path, text).unwrap();
        path
    }

    fn wait_for_active_file(app: &mut App, path: &Path) {
        app.wait_for_background(Duration::from_secs(10), |app| {
            app.editor().is_some_and(|editor| {
                editor.file_path.as_deref() == Some(path) && !editor.bg_loading
            })
        });
        assert!(app.editor().unwrap().saved_rope.is_some());
    }

    #[test]
    fn session_defers_large_hidden_clean_files_until_selected() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let active = write_file(root, "active.txt", MIN_DEFERRED_FILE_BYTES, "active text\n");
        let hidden = write_file(
            root,
            "hidden.txt",
            MIN_DEFERRED_FILE_BYTES,
            "old disk text\n",
        );
        let small = write_file(root, "small.txt", 10, "small text\n");
        let draft = write_file(
            root,
            "draft.txt",
            MIN_DEFERRED_FILE_BYTES,
            "saved baseline\n",
        );
        let saved = |path: &Path| {
            let mut editor = crate::editor::Editor::new_loading(path, 0);
            editor.cursor_line = 2;
            editor.cursor_col = 4;
            editor.bookmarks.insert(3);
            App::saved_file_tab(&Tab::new_file(editor))
        };
        let mut draft_tab = saved(&draft);
        draft_tab.pending_unsaved_content = Some(ropey::Rope::from_str("my draft"));
        let session = Session {
            tabs: vec![saved(&active), saved(&hidden), saved(&small), draft_tab],
            ..Session::default()
        };
        let mut app = App::new(root.to_owned());
        app.restore_session(&session);
        app.wait_for_background(Duration::from_secs(10), |app| {
            !app.tabs[0].editor.bg_loading
                && app.tabs[1].editor.has_deferred_file()
                && !app.tabs[2].editor.bg_loading
                && !app.tabs[3].editor.bg_loading
        });
        assert!(app.tabs[1].editor.saved_rope.is_none());
        assert!(app.tabs[1].editor.rope.len_bytes() < 100);
        assert_eq!(app.tabs[3].editor.rope, "my draft");
        assert!(app.tabs[3].editor.modified);
        assert_eq!(app.build_session().tabs[1], session.tabs[1]);

        // Reopening through the normal path reads the current disk file,
        // even when it changed size while the tab was inactive. A second
        // desk shares the same pending document instead of reading a copy.
        std::fs::write(
            &hidden,
            "new disk text\nsecond line\nthird line\nfourth line\n",
        )
        .unwrap();
        let original_desk = app.desks.active;
        app.new_desk("Second", false);
        app.open_file(&hidden);
        wait_for_active_file(&mut app, &hidden);
        let editor = app.editor().unwrap();
        assert_eq!(editor.rope.line(0), "new disk text\n");
        assert_eq!((editor.cursor_line, editor.cursor_col), (2, 4));
        assert!(editor.bookmarks.contains(&3));

        app.editor_mut().unwrap().insert_char('!');
        app.switch_desk(original_desk);
        app.open_file(&hidden);
        assert_eq!(app.tabs.len(), 4);
        let editor = app.editor_mut().unwrap();
        assert!(editor.modified);
        assert_eq!(editor.rope.line(2), "thir!d line\n");
    }

    #[test]
    fn opening_large_files_evicts_old_clean_text_and_keeps_edits_and_history() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let mut app = App::new(root.to_owned());
        let mut paths = Vec::new();
        for index in 0..10 {
            let path = write_file(
                root,
                &format!("{index}.txt"),
                10 * 1024 * 1024,
                "saved text\n",
            );
            app.open_file(&path);
            wait_for_active_file(&mut app, &path);
            let editor = app.editor_mut().unwrap();
            if index <= 1 {
                editor.insert_char('!');
                if index == 1 {
                    editor.undo();
                    assert!(!editor.modified);
                }
            } else if index == 2 {
                editor.set_cursor(4, 3);
                editor.bookmarks.insert(5);
                let cache = editor.render_line_cache.get_mut();
                cache.prepare_occurrences(
                    (
                        editor.navigation_id,
                        editor.content_version,
                        editor.wrap_revision as usize,
                    ),
                    &editor.rope,
                    "saved",
                    5,
                    true,
                    true,
                );
                assert!(cache.has_source_snapshot());
            }
            paths.push(path);
        }
        assert!(app.tabs[0].editor.modified);
        assert!(!app.tabs[1].editor.redo_stack.is_empty());
        assert!(app.tabs[2].editor.has_deferred_file());
        assert!(
            !app.tabs[2]
                .editor
                .render_line_cache
                .borrow()
                .has_source_snapshot()
        );
        assert!(!app.tabs[3].editor.bg_loading);
        assert_eq!(app.tabs.len(), paths.len());
        let cached: usize = app
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(index, tab)| {
                (index != app.active_tab)
                    .then(|| app.cacheable_file_bytes(tab))
                    .flatten()
            })
            .sum();
        assert!(cached <= INACTIVE_FILE_CACHE_BYTES);

        let identity = app.tabs[2].editor.save_identity;
        let version = app.tabs[2].editor.content_version;
        app.open_file(&paths[2]);
        wait_for_active_file(&mut app, &paths[2]);
        let editor = app.editor().unwrap();
        assert_eq!(editor.save_identity, identity);
        assert_eq!(editor.content_version, version);
        assert_eq!((editor.cursor_line, editor.cursor_col), (4, 3));
        assert!(editor.bookmarks.contains(&5));
    }

    #[test]
    fn workspace_replace_loads_a_deferred_target_and_preserves_undo() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let active = write_file(root, "active.txt", 16, "visible text\n");
        let hidden = root.join("hidden.txt");
        std::fs::write(
            &hidden,
            format!(
                "before\n{}",
                "padding\n".repeat(MIN_DEFERRED_FILE_BYTES / 8)
            ),
        )
        .unwrap();
        let saved = |path: &Path| {
            App::saved_file_tab(&Tab::new_file(crate::editor::Editor::new_loading(path, 0)))
        };
        let mut app = App::new(root.to_owned());
        app.restore_session(&Session {
            tabs: vec![saved(&active), saved(&hidden)],
            ..Session::default()
        });
        app.wait_for_background(Duration::from_secs(10), |app| {
            app.tabs[1].editor.has_deferred_file()
        });
        app.global_search_query = "before".into();
        app.global_search_replace_query = "after".into();
        app.global_search_results = vec![crate::app::GlobalSearchResult {
            file_path: hidden.clone(),
            line_num: 1,
            line_text: "before".into(),
            col_start: 0,
            col_end: 6,
            context_before: None,
            context_after: None,
        }]
        .into();
        app.replace_all_in_files();
        app.wait_for_background(Duration::from_secs(10), |app| {
            !app.bg_global_replace_running
        });
        assert_eq!(app.active_tab, 0);
        assert!(
            std::fs::read_to_string(&hidden)
                .unwrap()
                .starts_with("after\n")
        );
        let editor = &mut app.tabs[1].editor;
        assert!(!editor.modified);
        editor.undo();
        assert_eq!(editor.rope.line(0), "before\n");
        assert!(editor.modified);
    }
}
