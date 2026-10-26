//! Bridge document lifetimes to the sleeping completion-index worker.
//! Only compact document revisions are retained here, never source snapshots.
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use rustc_hash::FxHashMap;

use crate::app::{App, TabKind};

const EDIT_QUIET_PERIOD: Duration = Duration::from_millis(350);
const DOCUMENTS_PER_TURN: usize = 4;
const INVENTORY_PER_TURN: usize = 64;
const INVENTORY_INTERVAL: Duration = Duration::from_millis(8);

#[derive(Default)]
pub(crate) struct State {
    root: Option<PathBuf>,
    remote: bool,
    observed: BTreeMap<u64, (PathBuf, u64, bool, u64)>,
    paths: FxHashMap<PathBuf, usize>,
    next_due: Option<Instant>,
    revision: u64,
    enabled: bool,
    epoch: u64,
    scan: Option<(usize, usize)>,
    pruning: bool,
    prune_after: Option<u64>,
    refresh_all: bool,
    settings: Option<crate::completion_index::IndexSettings>,
    last_edit: Option<Instant>,
    inventory: Option<(usize, usize, usize, usize, u64, u64, u64)>,
    active: Option<(u64, PathBuf)>,
    active_retry: bool,
    pub(crate) buffers_changed: bool,
}

impl State {
    pub(crate) fn refresh_buffers(&mut self) {
        self.refresh_all = true;
        self.buffers_changed = true;
        self.active = None;
    }

    fn release_path(&mut self, path: PathBuf) -> Option<PathBuf> {
        let Some(owners) = self.paths.get_mut(&path) else {
            return None;
        };
        *owners -= 1;
        if *owners != 0 {
            return None;
        }
        self.paths.remove(&path);
        Some(path)
    }

    fn observe(&mut self, id: u64, path: PathBuf, version: u64, modified: bool) -> Option<PathBuf> {
        let old = self
            .observed
            .insert(id, (path.clone(), version, modified, self.epoch));
        if old.as_ref().is_some_and(|old| old.0 == path) {
            return None;
        }
        *self.paths.entry(path).or_default() += 1;
        old.and_then(|old| self.release_path(old.0))
    }
}

impl App {
    /// Called on real event-loop turns. There is no index-specific idle poll.
    /// The worker wakes the UI only when its published status changes.
    pub(crate) fn poll_completion_index(&mut self) -> bool {
        if self.completion_index_sync.settings.is_none() {
            self.completion_index
                .set_activity(self.vtcomplete_worker.activity().clone());
        }
        let remote = self.remote_workspace.is_some();
        if self.settings_persistence_enabled
            && (self.completion_index_sync.root.as_ref() != Some(&self.explorer.root)
                || self.completion_index_sync.remote != remote)
        {
            let restart = self.vtcomplete_active || self.vtcomplete_cancel_token.is_some();
            let explicit = self.vtcomplete_explicit;
            self.dismiss_vtcomplete();
            self.completion_index = if let Some(workspace) = &self.remote_workspace {
                crate::completion_index::CompletionIndex::new_remote(workspace)
            } else {
                crate::completion_index::CompletionIndex::new(self.explorer.root.clone())
            };
            self.completion_index
                .set_wake(self.background.wake_handle());
            self.completion_index
                .set_activity(self.vtcomplete_worker.activity().clone());
            self.completion_index_sync = State {
                root: Some(self.explorer.root.clone()),
                remote,
                ..Default::default()
            };
            if restart {
                if explicit {
                    self.update_vtcomplete_explicit();
                } else {
                    self.update_vtcomplete();
                }
            }
        }

        let revision = self.completion_index.revision();
        let changed = revision != self.completion_index_sync.revision;
        self.completion_index_sync.revision = revision;
        let settings = self.completion_index.settings();
        let paused = settings.paused;
        let resumed = self
            .completion_index_sync
            .settings
            .as_ref()
            .is_some_and(|previous| previous.paused && !paused);
        let enabled = settings.enabled;
        let budget_changed = self
            .completion_index_sync
            .settings
            .as_ref()
            .is_some_and(|old| {
                old.max_file_bytes != settings.max_file_bytes
                    || old.max_files != settings.max_files
                    || old.max_memory_mb != settings.max_memory_mb
            });
        if enabled != self.completion_index_sync.enabled || budget_changed {
            // Re-enabling must offer current revisions, including buffers
            // edited while indexing was disabled.
            self.completion_index_sync.refresh_all = true;
            self.completion_index_sync.active = None;
            self.completion_index_sync.active_retry = false;
            self.completion_index_sync.enabled = enabled;
            self.completion_index_sync.buffers_changed = true;
        }
        self.completion_index_sync.settings = Some(settings);
        self.completion_index_sync.buffers_changed |= resumed;
        self.completion_index_sync.refresh_all |= resumed;
        if !enabled || paused {
            self.completion_index_sync.active = None;
            self.completion_index_sync.active_retry = false;
            self.completion_index.clear_active_file();
            self.completion_index_sync.next_due = None;
            self.completion_index_sync.buffers_changed = false;
            return changed;
        }

        let now = Instant::now();
        if let Some(delay) = self.vtcomplete_worker.activity().remaining() {
            self.completion_index_sync.next_due = Some(now + delay);
            return changed;
        }
        if self.editor().is_none_or(|editor| {
            editor.bg_loading
                || editor
                    .file_path
                    .as_ref()
                    .is_none_or(|path| remote != crate::remote::workspace::is_view_key(path))
        }) {
            self.completion_index_sync.active = None;
            self.completion_index_sync.active_retry = false;
            self.completion_index.clear_active_file();
        }
        let activation = self
            .editor()
            .filter(|editor| !editor.bg_loading)
            .and_then(|editor| editor.file_path.as_ref().map(|path| (editor, path)))
            .filter(|(_, path)| remote == crate::remote::workspace::is_view_key(path))
            .filter(|(editor, path)| {
                self.completion_index_sync
                    .active
                    .as_ref()
                    .is_none_or(|(id, old)| *id != editor.navigation_id || old != *path)
                    || self.completion_index_sync.active_retry
                        && self
                            .completion_index_sync
                            .next_due
                            .is_none_or(|due| now >= due)
            })
            .map(|(editor, path)| {
                (
                    editor.navigation_id,
                    path.clone(),
                    editor.rope.clone(),
                    editor.content_version,
                    editor.modified,
                )
            });
        if let Some((id, path, rope, version, modified)) = activation {
            let accepted =
                self.completion_index
                    .observe_active_buffer(path.clone(), rope, modified);
            self.completion_index_sync.active = Some((id, path.clone()));
            self.completion_index_sync.active_retry = !accepted;
            if !accepted {
                self.completion_index_sync.next_due = Some(now + Duration::from_millis(50));
                return changed;
            }
            if let Some(previous) = self
                .completion_index_sync
                .observe(id, path, version, modified)
            {
                self.completion_index.release(previous);
            }
        }
        if self.completion_index_sync.active_retry {
            return changed;
        }
        let due = self.last_edit_time + EDIT_QUIET_PERIOD;
        if now < due {
            self.completion_index_sync.next_due = Some(due);
            return changed;
        }
        let editor = self.editor();
        let inventory = (
            self.active_group,
            self.tabs.len(),
            self.groups.len(),
            self.groups.iter().map(|group| group.tabs.len()).sum(),
            editor.map_or(0, |editor| editor.navigation_id),
            editor.map_or(0, |editor| editor.content_version),
            editor.map_or(0, |editor| {
                let mut fingerprint = rustc_hash::FxHasher::default();
                editor.file_path.hash(&mut fingerprint);
                editor.syntax_language_token().hash(&mut fingerprint);
                fingerprint.finish()
            }),
        );
        let invalidated = self.completion_index_sync.last_edit != Some(self.last_edit_time)
            || self.completion_index_sync.inventory != Some(inventory)
            || self.completion_index_sync.buffers_changed;
        if invalidated {
            let state = &mut self.completion_index_sync;
            state.last_edit = Some(self.last_edit_time);
            state.inventory = Some(inventory);
            state.buffers_changed = false;
            state.epoch = state.epoch.wrapping_add(1);
            state.scan = Some((0, 0));
            state.pruning = false;
            state.prune_after = None;
        }
        let state = &mut self.completion_index_sync;
        state.next_due = None;
        let mut submitted = 0;
        for _ in 0..INVENTORY_PER_TURN {
            let Some((surface, tab)) = state.scan else {
                break;
            };
            let tabs = if surface == 0 {
                &self.tabs
            } else if let Some(group) = self.groups.get(surface - 1) {
                &group.tabs
            } else {
                state.scan = None;
                state.pruning = true;
                break;
            };
            let Some(tab) = tabs.get(tab) else {
                state.scan = Some((surface + 1, 0));
                continue;
            };
            let editor = &tab.editor;
            let next = (surface, state.scan.unwrap().1 + 1);
            if tab.kind != TabKind::File || editor.bg_loading {
                state.scan = Some(next);
                continue;
            }
            let Some(path) = &editor.file_path else {
                state.scan = Some(next);
                continue;
            };
            if remote != crate::remote::workspace::is_view_key(path) {
                state.scan = Some(next);
                continue;
            }
            if let Some((old_path, version, modified, seen)) =
                state.observed.get_mut(&editor.navigation_id)
                && old_path == path
                && *version == editor.content_version
                && *modified == editor.modified
                && (!state.refresh_all || *seen == state.epoch)
            {
                *seen = state.epoch;
                state.scan = Some(next);
                continue;
            }
            if submitted == DOCUMENTS_PER_TURN {
                break;
            }
            let active = state
                .active
                .as_ref()
                .is_some_and(|(id, active)| *id == editor.navigation_id && active == path);
            let accepted = if active {
                self.completion_index.observe_active_buffer(
                    path.clone(),
                    editor.rope.clone(),
                    editor.modified,
                )
            } else {
                self.completion_index.observe_buffer(
                    path.clone(),
                    editor.rope.clone(),
                    editor.modified,
                )
            };
            if !accepted {
                state.next_due = Some(now + EDIT_QUIET_PERIOD);
                return changed;
            }
            if let Some(old_path) = state.observe(
                editor.navigation_id,
                path.clone(),
                editor.content_version,
                editor.modified,
            ) {
                self.completion_index.release(old_path);
            }
            submitted += 1;
            state.scan = Some(next);
        }
        if state.scan.is_some() {
            state.next_due = Some(now + INVENTORY_INTERVAL);
            return changed;
        }
        // Retirement is paged too. Closing thousands of tabs must not scan or
        // drop their entire observation map on the input/presentation thread.
        if state.pruning {
            use std::ops::Bound::{Excluded, Unbounded};
            for _ in 0..INVENTORY_PER_TURN {
                let next = match state.prune_after {
                    Some(id) => state.observed.range((Excluded(id), Unbounded)).next(),
                    None => state.observed.first_key_value(),
                }
                .map(|(&id, record)| (id, record.3));
                let Some((id, seen)) = next else {
                    state.pruning = false;
                    state.refresh_all = false;
                    break;
                };
                state.prune_after = Some(id);
                if seen != state.epoch {
                    let (path, _, _, _) = state.observed.remove(&id).unwrap();
                    if let Some(path) = state.release_path(path) {
                        self.completion_index.release(path);
                    }
                }
            }
            if state.pruning {
                state.next_due = Some(now + INVENTORY_INTERVAL);
            }
        }
        changed
    }

    pub(crate) fn completion_index_next_wait(&self) -> Option<Duration> {
        if let Some(due) = self.completion_index_sync.next_due {
            return Some(due.saturating_duration_since(Instant::now()));
        }
        if self.completion_index_sync.buffers_changed && self.completion_index_sync.enabled {
            return Some(
                (self.last_edit_time + EDIT_QUIET_PERIOD).saturating_duration_since(Instant::now()),
            );
        }
        self.completion_index_sync
            .next_due
            .map(|due| due.saturating_duration_since(Instant::now()))
    }

    pub(crate) fn completion_index_files_changed(
        &self,
        changes: &[crate::background::WorkspacePathChange],
        rescan: bool,
    ) {
        if self.remote_workspace.is_some() {
            return;
        }
        if rescan {
            self.completion_index.rescan();
            return;
        }
        // Watcher bursts can contain thousands of paths. Build this set once,
        // rather than walking every open tab for every changed project file.
        let dirty: rustc_hash::FxHashSet<&std::path::Path> = self
            .tabs
            .iter()
            .chain(self.groups.iter().flat_map(|group| &group.tabs))
            .filter(|tab| tab.kind == TabKind::File && tab.editor.modified)
            .filter_map(|tab| tab.editor.file_path.as_deref())
            .collect();
        for change in changes {
            // The live buffer is authoritative while it has unsaved edits.
            if !dirty.contains(change.path.as_path()) {
                self.completion_index.changed(change.path.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::Tab, editor::Editor};

    fn wait_for_word(index: &crate::completion_index::CompletionIndex, word: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !index
            .query(word, "")
            .iter()
            .any(|entry| entry.word.starts_with(word))
        {
            assert!(
                Instant::now() < deadline,
                "word did not reach the index: {word}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn path_changes_reindex_the_open_buffer_without_a_content_edit() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let path = root.join("source.custom");
        std::fs::write(&path, "loadDomain").unwrap();
        let mut app = App::new_deferred(root.clone());
        app.completion_index = crate::completion_index::CompletionIndex::new_for_test(
            root.clone(),
            temporary.path().join("cache"),
        );
        let mut editor = Editor::test_with_text("loadDomain");
        editor.file_path = Some(path.clone());
        editor.language_override = Some("My Custom Syntax".into());
        let version = editor.content_version;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.last_edit_time = Instant::now() - Duration::from_secs(1);
        app.poll_completion_index();
        wait_for_word(&app.completion_index, "load");

        let renamed = root.join("renamed.custom");
        std::fs::rename(&path, &renamed).unwrap();
        app.editor_mut().unwrap().file_path = Some(renamed.clone());
        app.poll_completion_index();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !app
            .completion_index
            .file_page(0, 32)
            .paths
            .contains(&renamed)
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            app.completion_index
                .query("load", "")
                .iter()
                .any(|word| word.word == "loadDomain")
        );
        assert_eq!(app.editor().unwrap().content_version, version);
        assert_eq!(
            app.completion_index_sync
                .observed
                .values()
                .next()
                .unwrap()
                .0,
            renamed
        );
        assert!(app.completion_index_next_wait().is_none());
    }

    #[test]
    fn focus_learns_immediately_edits_debounce_and_idle_has_no_deadline() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let path = root.join("source.custom");
        std::fs::write(&path, "savedIdentifier").unwrap();
        let mut app = App::new_deferred(root.clone());
        app.completion_index =
            crate::completion_index::CompletionIndex::new_for_test(root, temp.path().join("cache"));
        let mut editor = Editor::test_with_text("savedIdentifier");
        editor.file_path = Some(path);
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.last_edit_time = Instant::now();
        app.poll_completion_index();
        assert_eq!(app.completion_index_sync.observed.len(), 1);
        wait_for_word(&app.completion_index, "saved");
        let initial = app
            .completion_index_sync
            .observed
            .values()
            .next()
            .unwrap()
            .1;
        let editor = app.editor_mut().unwrap();
        editor.rope = ropey::Rope::from_str("updatedIdentifier");
        editor.modified = true;
        editor.content_version += 1;
        app.last_edit_time = Instant::now();
        app.poll_completion_index();
        assert_eq!(
            app.completion_index_sync
                .observed
                .values()
                .next()
                .unwrap()
                .1,
            initial
        );
        assert!(app.completion_index_next_wait().is_some());
        app.last_edit_time = Instant::now() - Duration::from_secs(1);
        app.poll_completion_index();
        assert_eq!(app.completion_index_sync.observed.len(), 1);
        assert_eq!(app.completion_index_next_wait(), None);
        wait_for_word(&app.completion_index, "updated");
        for _ in 0..10 {
            app.poll_completion_index();
        }
        assert_eq!(app.completion_index_next_wait(), None);
    }

    #[test]
    fn refocusing_an_unchanged_evicted_buffer_restores_its_current_contents() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let mut app = App::new_deferred(root.clone());
        app.completion_index = crate::completion_index::CompletionIndex::new_for_test(
            root.clone(),
            temp.path().join("cache"),
        );
        let mut settings = app.completion_index.settings();
        settings.max_files = 1;
        settings.speed = crate::completion_index::IndexSpeed::Fast;
        app.completion_index.configure(settings);
        for (name, word) in [
            ("first", "firstDraftIdentifier"),
            ("second", "secondDraftIdentifier"),
        ] {
            let path = root.join(name);
            std::fs::write(&path, "savedIdentifier").unwrap();
            let mut editor = Editor::test_with_text(word);
            editor.file_path = Some(path);
            editor.modified = true;
            app.tabs.push(Tab::new_file(editor));
        }
        for (tab, word) in [(0, "firstDraft"), (1, "secondDraft"), (0, "firstDraft")] {
            app.active_tab = tab;
            app.last_edit_time = Instant::now();
            app.poll_completion_index();
            wait_for_word(&app.completion_index, word);
            assert_eq!(
                app.completion_index.file_page(0, 32).paths,
                vec![app.editor().unwrap().file_path.clone().unwrap()]
            );
        }
        assert!(app.completion_index.query("savedIdentifier", "").is_empty());
    }

    #[test]
    fn closing_last_dirty_view_replaces_discarded_words_with_the_saved_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let path = root.join("source.custom");
        std::fs::write(&path, "diskIdentifier").unwrap();
        let mut app = App::new_deferred(root.clone());
        app.completion_index =
            crate::completion_index::CompletionIndex::new_for_test(root, temp.path().join("cache"));
        let mut editor = Editor::test_with_text("discardedIdentifier");
        editor.file_path = Some(path);
        editor.modified = true;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.last_edit_time = Instant::now() - Duration::from_secs(1);
        app.poll_completion_index();
        wait_for_word(&app.completion_index, "discarded");
        app.tabs.clear();
        app.poll_completion_index();
        wait_for_word(&app.completion_index, "disk");
        assert!(app.completion_index.query("discarded", "").is_empty());
        assert_eq!(app.completion_index_next_wait(), None);
    }

    #[test]
    fn paused_bridge_has_no_retry_timer_and_resumes_large_file_buffers() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let path = root.join("large.custom");
        std::fs::write(&path, "largeBufferIdentifier").unwrap();
        let mut app = App::new_deferred(root.clone());
        app.completion_index =
            crate::completion_index::CompletionIndex::new_for_test(root, temp.path().join("cache"));
        let mut editor = Editor::test_with_text("largeBufferIdentifier");
        editor.file_path = Some(path);
        editor.large_file_mode = true;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        let mut settings = app.completion_index.settings();
        settings.paused = true;
        app.completion_index.configure(settings.clone());
        app.last_edit_time = Instant::now();
        app.poll_completion_index();
        assert_eq!(app.completion_index_next_wait(), None);
        assert!(app.completion_index_sync.observed.is_empty());
        settings.paused = false;
        app.completion_index.configure(settings);
        app.last_edit_time = Instant::now() - Duration::from_secs(1);
        app.poll_completion_index();
        wait_for_word(&app.completion_index, "largeBuffer");
        assert_eq!(app.completion_index_sync.observed.len(), 1);
        assert_eq!(app.completion_index_next_wait(), None);
    }

    #[test]
    fn inert_index_has_no_work_or_deadline() {
        let mut app = App::test_new();
        app.tabs
            .push(Tab::new_file(Editor::test_with_text("identifier")));
        for _ in 0..5 {
            assert!(!app.poll_completion_index());
        }
        assert_eq!(app.completion_index_next_wait(), None);
    }

    #[test]
    fn closing_a_tab_during_backpressure_clears_the_active_retry_deadline() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new_deferred(temp.path().to_owned());
        app.completion_index = crate::completion_index::CompletionIndex::new_for_test(
            temp.path().to_owned(),
            temp.path().join("cache"),
        );
        app.last_edit_time = Instant::now() - Duration::from_secs(1);
        app.poll_completion_index();
        app.completion_index_sync.active = Some((1, temp.path().join("closed")));
        app.completion_index_sync.active_retry = true;
        app.completion_index_sync.next_due = Some(Instant::now());
        app.poll_completion_index();
        assert!(app.completion_index_sync.active.is_none());
        assert!(!app.completion_index_sync.active_retry);
        assert_eq!(app.completion_index_next_wait(), None);
    }

    #[test]
    fn save_as_releases_the_previous_paths_unsaved_overlay() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let original = root.join("original.custom");
        let destination = root.join("renamed.custom");
        std::fs::write(&original, "originalDiskIdentifier").unwrap();
        let mut app = App::new_deferred(root.clone());
        app.completion_index =
            crate::completion_index::CompletionIndex::new_for_test(root, temp.path().join("cache"));
        let mut editor = Editor::test_with_text("draftIdentifier");
        editor.file_path = Some(original);
        editor.modified = true;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.last_edit_time = Instant::now() - Duration::from_secs(1);
        app.poll_completion_index();
        wait_for_word(&app.completion_index, "draft");
        std::fs::write(&destination, "draftIdentifier").unwrap();
        app.tabs[0].editor.file_path = Some(destination);
        app.tabs[0].editor.modified = false;
        app.completion_index_sync.buffers_changed = true;
        app.poll_completion_index();
        wait_for_word(&app.completion_index, "originalDisk");
        wait_for_word(&app.completion_index, "draft");
        assert!(!app.completion_index.query("originalDisk", "").is_empty());
    }
}

#[cfg(test)]
#[path = "app_vtcomplete_index_removal_tests.rs"]
mod removal_tests;

#[cfg(test)]
#[path = "app_vtcomplete_scheduling_tests.rs"]
mod scheduling_tests;

#[cfg(test)]
#[path = "app_vtcomplete_remote_tests.rs"]
mod remote_tests;
