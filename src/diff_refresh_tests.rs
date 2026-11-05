use super::*;
use crate::background::{BackgroundResult, CancellationToken};
use rustc_hash::FxHashSet;
use std::{
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

struct Fixture {
    app: App,
    paths: [PathBuf; 2],
    root: PathBuf,
    traffic: Arc<AtomicUsize>,
    received: Arc<AtomicUsize>,
    hold: Arc<AtomicBool>,
    blocked: Arc<AtomicBool>,
    _directory: tempfile::TempDir,
}

impl Fixture {
    fn new(remote: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(directory.path()).unwrap();
        let repo = git2::Repository::init(&root).unwrap();
        repo.config()
            .unwrap()
            .set_bool("core.autocrlf", false)
            .unwrap();
        let mut index = repo.index().unwrap();
        for name in ["a.txt", "b.txt"] {
            std::fs::write(root.join(name), "original\n").unwrap();
            index.add_path(Path::new(name)).unwrap();
        }
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("Test", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "Initial", &tree, &[])
            .unwrap();
        for name in ["a.txt", "b.txt"] {
            std::fs::write(root.join(name), "worktree\n").unwrap();
        }
        let traffic = Arc::new(AtomicUsize::new(0));
        let received = Arc::new(AtomicUsize::new(0));
        let hold = Arc::new(AtomicBool::new(false));
        let blocked = Arc::new(AtomicBool::new(false));
        let mut app = App::test_new();
        // These refresh scenarios retain multiple independent comparisons.
        // Preview-specific scenarios below enable replacement explicitly.
        app.set_preview_tabs_enabled(false);
        let paths = if remote {
            let workspace = crate::remote::workspace::Workspace::new(
                crate::remote::tests::connection_with_traffic(&root, hold.clone(), traffic.clone(), blocked.clone(), received.clone()),
            );
            let paths = [
                workspace.target_key("a.txt").unwrap(),
                workspace.target_key("b.txt").unwrap(),
            ];
            app.explorer = crate::explorer::FileExplorer::new_deferred(workspace.view_root.clone());
            app.remote_workspace = Some(workspace);
            paths
        } else {
            app.explorer = crate::explorer::FileExplorer::new_deferred(root.clone());
            [root.join("a.txt"), root.join("b.txt")]
        };
        app.workspace_watcher_active = true;
        Self {
            app,
            paths,
            root,
            traffic,
            received, hold, blocked,
            _directory: directory,
        }
    }
    fn write(&self, file: usize, text: &str) {
        std::fs::write(
            self.root.join(if file == 0 { "a.txt" } else { "b.txt" }),
            text,
        )
        .unwrap();
    }
    fn stage(&self, file: usize, text: &str) {
        let repo = git2::Repository::open(&self.root).unwrap();
        let mut index = repo.index().unwrap();
        let entry = index
            .get_path(Path::new(if file == 0 { "a.txt" } else { "b.txt" }), 0)
            .unwrap();
        index.add_frombuffer(&entry, text.as_bytes()).unwrap();
        index.write().unwrap();
    }
    fn open(&mut self, file: usize, staged: bool) -> (usize, u64) {
        self.app.open_source_control_diff_in_repository(
            &self.paths[file],
            false,
            staged,
            Some(self.app.explorer.root.clone()),
        );
        self.app
            .wait_for_background(Duration::from_secs(15), |app| {
                app.should_render_diff_for_active_editor()
                    && !app.bg_git_compare_running
                    && !app.diff_tabs.refresh_running
            });
        (self.app.active_tab, self.app.active_diff_tab_id().unwrap())
    }
    fn invalidate(&mut self, file: usize) {
        self.app.refresh_source_control_diffs_for_paths(Some(
            &[self.paths[file].clone()].into_iter().collect(),
        ));
    }
    fn settle(&mut self) {
        self.app
            .wait_for_background(Duration::from_secs(15), |app| {
                !app.diff_tabs.refresh_running
                    && !app.bg_git_compare_running
                    && !app.bg_git_status_running
            });
    }
    fn activate(&mut self, tab: usize) {
        self.app.active_tab = tab;
        self.app.sync_diff_tab();
        self.settle();
    }
}

#[test]
fn diff_browsing_respects_preview_settings_locally_and_remotely() {
    for remote in [false, true] {
        for enabled in [false, true] {
            let mut f = Fixture::new(remote);
            f.app.set_preview_tabs_enabled(enabled);
            for (index, (file, staged)) in [(0, false), (0, true), (1, false), (1, true)].into_iter().enumerate() {
                let (_, id) = f.open(file, staged);
                assert_eq!(f.app.tabs.len(), if enabled { 1 } else { index + 1 });
                assert_eq!(f.app.tabs[f.app.active_tab].preview, enabled);
                assert_eq!(f.app.diff_tabs.source_control.len(), f.app.tabs.len());
                assert_eq!(f.app.diff_tabs.staged_for_tab(&f.app.tabs[f.app.active_tab]), Some(staged));
                assert_eq!(f.app.diff_tabs.owner, Some(id));
                assert!(f.app.should_render_diff_for_active_editor());
            }
        }
    }
}

#[test]
fn diff_previews_preserve_kept_pinned_and_existing_tabs() {
    for remote in [false, true] {
        let mut f = Fixture::new(remote);
        f.app.set_preview_tabs_enabled(true);
        let (_, kept) = f.open(0, false);
        f.app.pin_preview_tab();
        f.open(0, true);
        let (_, preview) = f.open(1, false);
        assert_eq!(f.app.tabs.len(), 2);
        let (_, existing) = f.open(0, false);
        assert_eq!(existing, kept);
        assert!(!f.app.tabs[f.app.active_tab].preview);
        let (_, existing) = f.open(1, false);
        assert_eq!(existing, preview);
        assert!(f.app.tabs[f.app.active_tab].preview);
        f.app.tabs[f.app.active_tab].pinned = true;
        f.open(1, true);
        assert_eq!(f.app.tabs.len(), 3);
        assert!(f.app.tabs.iter().any(|tab| tab.kind == TabKind::Diff(preview) && tab.pinned && !tab.preview));
        assert!(f.app.tabs.iter().any(|tab| tab.kind == TabKind::Diff(kept) && !tab.preview));
        f.app.set_preview_tabs_enabled(false);
        assert!(f.app.tabs.iter().all(|tab| !tab.preview));
    }
}

#[test]
fn files_and_diffs_share_a_preview_without_discarding_unsaved_edits() {
    for remote in [false, true] {
        let mut f = Fixture::new(remote);
        f.app.set_preview_tabs_enabled(true);
        f.app.open_file_preview(&f.paths[0]);
        assert!(f.app.editor().unwrap().bg_loading);
        let (_, diff) = f.open(1, true);
        assert_eq!(f.app.tabs.len(), 1);
        f.app.open_file_preview(&f.paths[0]);
        assert_eq!(f.app.tabs.len(), 1);
        assert!(!f.app.diff_tabs.source_control.contains_key(&diff));
        assert!(!f.app.diff_tabs.views.contains_key(&diff));
        assert!(!f.app.should_render_diff_for_active_editor());
        f.app.wait_for_background(Duration::from_secs(5), |app| !app.editor().unwrap().bg_loading);
        f.app.editor_mut().unwrap().insert_char('!');
        let edited = f.app.editor().unwrap().rope.clone();
        f.open(0, false);
        assert_eq!(f.app.tabs.len(), 2);
        assert_eq!(f.app.tabs[0].kind, TabKind::File);
        assert!(!f.app.tabs[0].preview);
        assert!(f.app.tabs[0].has_unsaved_changes());
        assert_eq!(f.app.tabs[0].editor.rope, edited);
        assert!(f.app.tabs[f.app.active_tab].preview);
        assert_eq!(std::fs::read_to_string(f.root.join("a.txt")).unwrap(), "worktree\n");
    }
}

#[test]
fn diff_previews_are_replaced_only_in_their_own_split_group() {
    for remote in [false, true] {
        let mut f = Fixture::new(remote);
        f.app.set_preview_tabs_enabled(true);
        f.app.open_file(&f.paths[0]);
        f.app.open_file(&f.paths[1]);
        f.app.create_split_group_dir(1, crate::app::SplitDirection::Horizontal);
        let other_group = f.app.active_group;
        let (_, other_diff) = f.open(0, false);
        f.app.switch_group(0);
        f.open(1, false);
        f.open(0, true);
        assert_eq!(f.app.active_group, 0);
        assert_eq!(f.app.tabs.len(), 2);
        assert_eq!(f.app.tabs.iter().filter(|tab| tab.preview).count(), 1);
        assert!(f.app.groups[other_group].tabs.iter().any(|tab| tab.kind == TabKind::Diff(other_diff) && tab.preview));
        let (_, existing) = f.open(0, false);
        assert_eq!(existing, other_diff);
        assert_eq!(f.app.active_group, other_group);
        assert!(f.app.tabs[f.app.active_tab].preview);
    }
}

#[test]
fn rapid_diff_preview_replacement_ignores_old_file_and_git_replies() {
    for remote in [false, true] {
        for staged in [false, true] {
            let mut f = Fixture::new(remote);
            f.app.set_preview_tabs_enabled(true);
            f.stage(0, "staged\n");
            for staged in [!staged, staged] {
                f.app.open_source_control_diff_in_repository(&f.paths[0], false, staged,
                    Some(f.app.explorer.root.clone()));
            }
            f.app.wait_for_background(Duration::from_secs(15), |app| {
                app.should_render_diff_for_active_editor() && !app.background.has_active_jobs()
                    && !app.bg_git_compare_running && !app.diff_tabs.refresh_running
            });
            assert_eq!(f.app.tabs.len(), 1);
            assert!(f.app.tabs[0].preview);
            assert_eq!(f.app.diff_tabs.staged_for_tab(&f.app.tabs[0]), Some(staged));
            assert_eq!(f.app.editor().unwrap().rope.to_string(), if staged { "staged\n" } else { "worktree\n" });
            assert_eq!(f.app.compare_base_content.as_deref(), Some(if staged { "original\n" } else { "staged\n" }));
        }
    }
}

#[test]
fn recent_diff_cache_restores_colored_views_before_stalled_remote_validation() {
    struct Release(Arc<AtomicBool>);
    impl Drop for Release { fn drop(&mut self) { self.0.store(false, Ordering::Release); } }
    let mut f = Fixture::new(true);
    let _release = Release(f.hold.clone());
    f.app.set_preview_tabs_enabled(true);
    let contents: String = (0..2048u32).map(|index| format!("{index:04x} {:08x}\n", index.wrapping_mul(2_654_435_761))).collect();
    f.write(0, &contents);
    let before = f.received.load(Ordering::Relaxed);
    f.open(0, false);
    let cold_bytes = f.received.load(Ordering::Relaxed) - before;
    assert!(f.app.editor().unwrap().remote_checkpoint.is_some());
    let cache = f.app.diff_render_cache.clone();
    f.app.editor_mut().unwrap().set_cursor(12, 3);
    f.open(1, false);
    let before = f.received.load(Ordering::Relaxed);
    f.hold.store(true, Ordering::Release);
    f.app.open_source_control_diff_in_repository(&f.paths[0], false, false, Some(f.app.explorer.root.clone()));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !f.blocked.load(Ordering::Acquire) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(f.blocked.load(Ordering::Acquire));
    assert!(!f.app.editor().unwrap().bg_loading);
    assert!(f.app.should_render_diff_for_active_editor());
    assert!(Arc::ptr_eq(&cache, &f.app.diff_render_cache));
    assert_eq!(f.app.editor().unwrap().rope.to_string(), contents);
    assert_eq!((f.app.editor().unwrap().cursor_line, f.app.editor().unwrap().cursor_col), (12, 3));
    assert!(f.app.status_message.starts_with("Updating diff:"));
    let cached_at = f.app.diff_tabs.source_control[&f.app.active_diff_tab_id().unwrap()].validating_cached;
    for _ in 0..3 {
        for file in [1, 0] {
            f.app.open_source_control_diff_in_repository(&f.paths[file], false, false, Some(f.app.explorer.root.clone()));
            assert!(!f.app.editor().unwrap().bg_loading);
            assert!(f.app.should_render_diff_for_active_editor());
        }
    }
    assert!(Arc::ptr_eq(&cache, &f.app.diff_render_cache));
    assert_eq!(f.app.diff_tabs.source_control[&f.app.active_diff_tab_id().unwrap()].validating_cached,
        cached_at, "bouncing between previews must not extend an unvalidated snapshot's lifetime");
    f.hold.store(false, Ordering::Release);
    f.settle();
    assert!(Arc::ptr_eq(&cache, &f.app.diff_render_cache));
    let warm_bytes = f.received.load(Ordering::Relaxed) - before;
    assert!(warm_bytes < cold_bytes / 4, "cached diff transferred {warm_bytes} bytes versus {cold_bytes} cold");
    assert!(!f.app.status_message.starts_with("Updating diff:"));
    eprintln!("remote cached diff: cold={cold_bytes} bytes, warm={warm_bytes} bytes");
}

#[test]
fn recent_diff_cache_revalidates_worktree_bytes_and_git_ids_even_without_events() {
    for remote in [false, true] {
        for staged in [false, true] {
            let mut f = Fixture::new(remote);
            f.app.set_preview_tabs_enabled(true);
            f.open(0, staged);
            f.open(1, staged);
            let path = f.root.join("a.txt");
            let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
            f.write(0, "newtree!\n");
            std::fs::File::options().write(true).open(&path).unwrap().set_modified(modified).unwrap();
            f.stage(0, "restaged\n");
            f.app.open_source_control_diff_in_repository(&f.paths[0], false, staged, Some(f.app.explorer.root.clone()));
            assert!(!f.app.editor().unwrap().bg_loading);
            assert!(f.app.should_render_diff_for_active_editor());
            f.settle();
            assert_eq!(f.app.editor().unwrap().rope.to_string(), if staged { "restaged\n" } else { "newtree!\n" });
            assert_eq!(f.app.compare_base_content.as_deref(), Some(if staged { "original\n" } else { "restaged\n" }));
        }
    }
}

#[test]
fn recent_diff_cache_invalidates_only_relevant_paths_and_clears_on_metadata_or_reconnect() {
    for remote in [false, true] {
        let mut f = Fixture::new(remote);
        f.app.set_preview_tabs_enabled(true);
        f.open(0, false);
        f.open(1, false);
        assert_eq!(f.app.diff_tabs.recent.entries.len(), 1);
        f.app.refresh_source_control_diffs_for_paths(Some(&[f.app.explorer.root.join("unrelated")].into_iter().collect()));
        assert_eq!(f.app.diff_tabs.recent.entries.len(), 1);
        f.invalidate(0);
        assert!(f.app.diff_tabs.recent.entries.is_empty());
        f.open(0, false);
        assert!(!f.app.diff_tabs.recent.entries.is_empty());
        f.app.refresh_source_control_diffs();
        assert!(f.app.diff_tabs.recent.entries.is_empty());
        f.settle();
        f.open(1, false);
        assert!(!f.app.diff_tabs.recent.entries.is_empty());
        f.app.cancel_source_control_diff_refresh();
        assert!(f.app.diff_tabs.recent.entries.is_empty());
        f.settle();
    }
}

#[test]
fn recent_diff_cache_keys_include_repository_and_stage_and_expire_without_a_timer() {
    let mut f = Fixture::new(false);
    f.app.set_preview_tabs_enabled(true);
    f.open(0, false);
    f.open(1, false);
    let now = Instant::now();
    let recent = &mut f.app.diff_tabs.recent;
    assert!(recent.take(&f.paths[0], Some(&f.root.join("other-repo")), false, false, u64::MAX, now).is_none());
    assert!(recent.take(&f.paths[0], Some(&f.root), true, false, u64::MAX, now).is_none());
    assert_eq!(recent.entries.len(), 1);
    assert!(recent.take(&f.paths[0], Some(&f.root), false, false, u64::MAX,
        now + recent::LIFETIME).is_none());
    assert!(recent.entries.is_empty());
}

#[test]
fn recent_diff_cache_is_bounded_by_entry_count_and_memory() {
    let mut f = Fixture::new(false);
    f.app.set_preview_tabs_enabled(true);
    for index in 0..recent::MAX_ENTRIES + 3 {
        let path = f.root.join(format!("recent-{index}.txt"));
        std::fs::write(&path, format!("file {index}\n")).unwrap();
        f.app.open_source_control_diff_in_repository(&path, false, false, Some(f.root.clone()));
        f.app.wait_for_background(Duration::from_secs(5), |app| app.should_render_diff_for_active_editor()
            && !app.bg_git_compare_running && !app.diff_tabs.refresh_running);
        assert!(f.app.diff_tabs.recent.entries.len() <= recent::MAX_ENTRIES);
        assert!(f.app.diff_tabs.recent.entries.iter().map(recent::Entry::memory_bytes).sum::<usize>() <= recent::MAX_BYTES);
    }
    let recent = &mut f.app.diff_tabs.recent;
    assert_eq!(recent.entries.len(), recent::MAX_ENTRIES);
    assert!(!recent.entries.iter().any(|entry| entry.path == f.root.join("recent-0.txt")));
    let mut oversized = recent.entries.pop_back().unwrap();
    oversized.target.rope = ropey::Rope::from_str(&"x".repeat(recent::MAX_BYTES / 8 + 1));
    let count = recent.entries.len();
    recent.insert(oversized);
    assert_eq!(recent.entries.len(), count, "oversized views must not displace useful small views");
}

fn completed_refresh(app: &App) -> (PathBuf, u64, Vec<SourceControlDiffUpdate>) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let message = app
            .background
            .receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        if let BackgroundResult::SourceControlRead { result, .. } = message
            && let BackgroundResult::SourceControlDiffs {
                root,
                generation,
                updates,
            } = *result
        {
            return (root, generation, updates);
        }
    }
}

#[test]
fn unrelated_file_events_and_recent_navigation_do_not_schedule_diff_reads() {
    for remote in [false, true] {
        let mut f = Fixture::new(remote);
        let (_, a) = f.open(0, false);
        f.open(1, false);
        let active = f.app.diff_render_cache.clone();
        let hidden = f.app.diff_tabs.views[&a].cache.clone();
        let generation = f.app.diff_tabs.refresh_generation;
        let traffic = f.traffic.load(Ordering::Relaxed);
        f.app.source_control_last_refresh = Some(Instant::now());
        for _ in 0..30 {
            f.app.refresh_source_control_diffs_for_paths(Some(
                &[f.app.explorer.root.join("other.txt")]
                    .into_iter()
                    .collect(),
            ));
            f.app.refresh_source_control_for_navigation();
            f.app.sync_diff_tab();
        }
        assert!(!f.app.diff_tabs.refresh_running);
        assert!(f.app.diff_tabs.dirty.is_empty());
        assert_eq!(generation, f.app.diff_tabs.refresh_generation);
        assert_eq!(traffic, f.traffic.load(Ordering::Relaxed));
        assert!(Arc::ptr_eq(&active, &f.app.diff_render_cache));
        assert!(Arc::ptr_eq(&hidden, &f.app.diff_tabs.views[&a].cache));
    }
}

#[test]
fn hidden_dirty_diffs_wait_for_activation_and_leave_other_views_unchanged() {
    for remote in [false, true] {
        let mut f = Fixture::new(remote);
        let (tab, a) = f.open(0, false);
        f.open(1, false);
        let cache = f.app.diff_render_cache.clone();
        let traffic = f.traffic.load(Ordering::Relaxed);
        f.write(0, "latest\n");
        f.invalidate(0);
        assert!(!f.app.diff_tabs.refresh_running);
        assert!(f.app.diff_tabs.dirty.contains(&a));
        assert_eq!(f.app.tabs[tab].editor.rope.to_string(), "worktree\n");
        assert_eq!(traffic, f.traffic.load(Ordering::Relaxed));
        assert!(Arc::ptr_eq(&cache, &f.app.diff_render_cache));
        f.activate(tab);
        assert_eq!(f.app.editor().unwrap().rope.to_string(), "latest\n");
        assert_eq!(f.app.compare_base_content.as_deref(), Some("original\n"));
        assert!(!f.app.diff_tabs.dirty.contains(&a));
    }
}

#[test]
fn watcher_overflow_revalidates_worktree_bytes_and_workspace_events_cover_descendants() {
    for remote in [false, true] {
        let mut f = Fixture::new(remote);
        f.open(0, false);
        f.write(0, "event was lost\n");
        f.app
            .background
            .sender
            .send(BackgroundResult::FilesChanged {
                root: f.app.explorer.root.clone(),
                changes: Vec::new(),
                structure_changed: false,
                rescan: true,
                git_metadata_changed: false,
            })
            .unwrap();
        f.app.poll_background_results();
        f.settle();
        assert_eq!(f.app.editor().unwrap().rope.to_string(), "event was lost\n");
        f.write(0, "workspace replaced\n");
        f.app.refresh_source_control_diffs_for_paths(Some(
            &[f.app.explorer.root.clone()].into_iter().collect(),
        ));
        f.settle();
        assert_eq!(
            f.app.editor().unwrap().rope.to_string(),
            "workspace replaced\n"
        );
    }
}

#[test]
fn navigation_status_scan_does_not_invalidate_watched_diff_views() {
    let mut f = Fixture::new(false);
    let (_, a) = f.open(0, false);
    f.open(1, false);
    f.app.source_control_last_refresh = Some(Instant::now() - Duration::from_secs(3));
    let cache = f.app.diff_render_cache.clone();
    f.app.refresh_source_control_for_navigation();
    assert!(f.app.bg_git_status_running);
    assert!(!f.app.diff_tabs.refresh_running);
    assert!(f.app.diff_tabs.dirty.is_empty());
    f.settle();
    assert!(Arc::ptr_eq(&cache, &f.app.diff_render_cache));
    assert!(!f.app.diff_tabs.dirty.contains(&a));
}

#[cfg(windows)]
#[test]
fn dirty_paths_match_windows_case_and_extended_prefix_aliases() {
    let mut f = Fixture::new(false);
    f.open(0, false);
    f.write(0, "changed through an alias\n");
    let alternate = PathBuf::from(f.paths[0].to_string_lossy().to_uppercase());
    f.app
        .refresh_source_control_diffs_for_paths(Some(&[alternate].into_iter().collect()));
    f.settle();
    assert_eq!(
        f.app.editor().unwrap().rope.to_string(),
        "changed through an alias\n"
    );
}

#[test]
fn worktree_events_reuse_git_sources_and_git_events_reuse_worktree_bytes() {
    for remote in [false, true] {
        let mut f = Fixture::new(remote);
        let (staged_tab, staged_id) = f.open(0, true);
        f.open(0, false);
        let original = f.app.compare_base_content.clone().unwrap();
        f.stage(0, "index changed without an event\n");
        f.write(0, "new worktree\n");
        f.invalidate(0);
        f.settle();
        assert!(Arc::ptr_eq(
            &original,
            f.app.compare_base_content.as_ref().unwrap()
        ));
        assert_eq!(f.app.editor().unwrap().rope.to_string(), "new worktree\n");
        assert!(
            !f.app.diff_tabs.dirty.contains(&staged_id),
            "Worktree events cannot alter a staged diff"
        );
        f.write(0, "unreported worktree change\n");
        f.app
            .refresh_source_control_diff_git_changed(&FxHashSet::default());
        f.settle();
        assert_eq!(
            f.app.compare_base_content.as_deref(),
            Some("index changed without an event\n")
        );
        assert_eq!(
            f.app.editor().unwrap().rope.to_string(),
            "new worktree\n",
            "Git-only invalidation must not reread the worktree"
        );
        assert!(f.app.diff_tabs.dirty.contains(&staged_id));
        f.activate(staged_tab);
        assert_eq!(
            f.app.editor().unwrap().rope.to_string(),
            "index changed without an event\n"
        );
    }
}

#[test]
fn relevant_event_during_git_read_retries_both_git_and_worktree_revisions() {
    for remote in [false, true] {
        let mut f = Fixture::new(remote);
        f.open(0, false);
        f.stage(0, "new index\n");
        f.app
            .refresh_source_control_diff_git_changed(&FxHashSet::default());
        let (root, generation, updates) = completed_refresh(&f.app);
        f.write(0, "new worktree\n");
        f.invalidate(0);
        f.app
            .finish_source_control_diff_refresh(root, generation, updates);
        f.settle();
        assert_eq!(f.app.compare_base_content.as_deref(), Some("new index\n"));
        assert_eq!(f.app.editor().unwrap().rope.to_string(), "new worktree\n");
    }
}

#[test]
fn unrelated_event_during_read_does_not_discard_the_completed_view() {
    for remote in [false, true] {
        let mut f = Fixture::new(remote);
        let (_, a) = f.open(0, false);
        f.open(1, false);
        f.write(1, "reported\n");
        f.invalidate(1);
        let (root, generation, updates) = completed_refresh(&f.app);
        f.write(1, "not yet reported\n");
        f.invalidate(0);
        f.app
            .finish_source_control_diff_refresh(root, generation, updates);
        f.settle();
        assert_eq!(f.app.editor().unwrap().rope.to_string(), "reported\n");
        assert!(f.app.diff_tabs.dirty.contains(&a));
        f.invalidate(1);
        f.settle();
        assert_eq!(
            f.app.editor().unwrap().rope.to_string(),
            "not yet reported\n"
        );
    }
}

#[test]
fn visible_inactive_split_refreshes_only_its_affected_diff() {
    let mut f = Fixture::new(false);
    let (_, a) = f.open(0, false);
    f.open(1, false);
    f.app
        .create_split_group_dir(f.app.active_tab, crate::app::SplitDirection::Horizontal);
    let active_cache = f.app.diff_render_cache.clone();
    f.write(0, "changed left pane\n");
    f.write(1, "unreported right pane\n");
    f.invalidate(0);
    f.settle();
    assert!(Arc::ptr_eq(&active_cache, &f.app.diff_render_cache));
    assert_eq!(f.app.editor().unwrap().rope.to_string(), "worktree\n");
    assert!(!f.app.diff_tabs.dirty.contains(&a));
    f.app.switch_group(0);
    assert_eq!(
        f.app.editor().unwrap().rope.to_string(),
        "changed left pane\n"
    );
}

#[test]
fn inactive_desks_keep_dirty_diff_views_lazy() {
    let mut f = Fixture::new(false);
    let (_, a) = f.open(0, false);
    let original = f.app.desks.active;
    f.app.new_desk("Other", false);
    f.open(1, false);
    f.write(0, "changed in hidden desk\n");
    f.invalidate(0);
    assert!(!f.app.diff_tabs.refresh_running);
    assert!(f.app.diff_tabs.dirty.contains(&a));
    f.app.switch_desk(original);
    f.settle();
    assert_eq!(
        f.app.editor().unwrap().rope.to_string(),
        "changed in hidden desk\n"
    );
}

#[test]
fn stale_completion_cannot_release_a_new_refresh_gate() {
    let mut f = Fixture::new(false);
    f.open(0, false);
    f.invalidate(0);
    let (root, generation, updates) = completed_refresh(&f.app);
    f.app.cancel_source_control_diff_refresh();
    f.app.refresh_dirty_source_control_diffs();
    assert!(f.app.diff_tabs.refresh_running);
    assert!(
        !f.app
            .finish_source_control_diff_refresh(root, generation, updates)
    );
    assert!(f.app.diff_tabs.refresh_running);
    f.settle();
}

#[test]
fn full_refresh_detects_replacements_and_deletion_after_hidden_activation() {
    for remote in [false, true] {
        let mut f = Fixture::new(remote);
        let (tab, _) = f.open(0, false);
        f.open(1, false);
        std::fs::remove_file(f.root.join("a.txt")).unwrap();
        f.app.refresh_source_control_diffs();
        f.settle();
        f.activate(tab);
        assert!(f.app.editor().unwrap().disk_unavailable);
        assert_eq!(f.app.editor().unwrap().rope.len_bytes(), 0);
        f.write(0, "recreated\n");
        f.invalidate(0);
        f.settle();
        assert!(!f.app.editor().unwrap().disk_unavailable);
        assert_eq!(f.app.editor().unwrap().rope.to_string(), "recreated\n");
    }
}

#[test]
fn unchanged_git_ids_omit_blob_payloads_locally_and_over_the_protocol() {
    for remote in [false, true] {
        let f = Fixture::new(remote);
        f.stage(0, "staged\n");
        let read = |known| {
            if let Some(remote) = &f.app.remote_workspace {
                remote
                    .git_diff_sources(
                        &f.paths[0],
                        Some(&f.app.explorer.root),
                        true,
                        known,
                        &CancellationToken::new(),
                    )
                    .unwrap()
            } else {
                crate::git_diff_sources::read(
                    &git2::Repository::open(&f.root).unwrap(),
                    Path::new("a.txt"),
                    true,
                    known,
                )
                .unwrap()
            }
        };
        let initial = read(None);
        assert_eq!(initial.base.as_deref(), Some("original\n"));
        assert_eq!(initial.index.as_deref(), Some("staged\n"));
        let repeated = read(Some(initial.revisions));
        assert_eq!(repeated.revisions, initial.revisions);
        assert!(repeated.base.is_none() && repeated.index.is_none());
        f.stage(0, "restaged\n");
        let changed = read(Some(initial.revisions));
        assert!(changed.base.is_none());
        assert_eq!(changed.index.as_deref(), Some("restaged\n"));
    }
}

#[test]
fn index_returning_to_head_reuses_the_cached_base_for_its_target() {
    for remote in [false, true] {
        let mut f = Fixture::new(remote);
        f.stage(0, "staged\n");
        f.open(0, true);
        assert_eq!(f.app.editor().unwrap().rope.to_string(), "staged\n");
        let cache = f.app.compare_base_content.clone().unwrap();
        f.stage(0, "original\n");
        f.app
            .refresh_source_control_diff_git_changed(&FxHashSet::default());
        f.settle();
        assert!(Arc::ptr_eq(
            &cache,
            f.app.compare_base_content.as_ref().unwrap()
        ));
        assert_eq!(f.app.editor().unwrap().rope.to_string(), "original\n");
        assert!(f.app.diff_render_cache.hunks.is_empty());
    }
}

#[test]
#[ignore = "manual optimized benchmark: conditional Git sources versus repeated blob reads"]
fn benchmark_conditional_git_diff_sources() {
    let f = Fixture::new(false);
    let text: String = (0..40_000)
        .map(|line| format!("unique line {line:08} with enough data to decompress\n"))
        .collect();
    f.stage(0, &text);
    let repo = git2::Repository::open(&f.root).unwrap();
    let known = crate::git_diff_sources::read(&repo, Path::new("a.txt"), true, None)
        .unwrap()
        .revisions;
    let mut timings = [Vec::new(), Vec::new()];
    for sample in 0..11 {
        for conditional in [sample % 2 == 0, sample % 2 != 0] {
            let start = Instant::now();
            let sources = crate::git_diff_sources::read(
                &repo,
                Path::new("a.txt"),
                true,
                conditional.then_some(known),
            )
            .unwrap();
            let elapsed = start.elapsed();
            assert_eq!(
                sources.index.as_ref().map_or(0, |text| text.len()),
                if conditional { 0 } else { text.len() }
            );
            std::hint::black_box(sources);
            if sample >= 2 {
                timings[usize::from(conditional)].push(elapsed);
            }
        }
    }
    for times in &mut timings {
        times.sort();
    }
    eprintln!(
        "unchanged staged Git sources ({} index bytes): full={:?}, conditional={:?}",
        text.len(),
        timings[0][4],
        timings[1][4]
    );
}
