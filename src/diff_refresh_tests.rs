use super::*;
use crate::background::{BackgroundResult, CancellationToken};
use rustc_hash::FxHashSet;
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

struct Fixture {
    app: App,
    paths: [PathBuf; 2],
    root: PathBuf,
    traffic: Arc<AtomicUsize>,
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
        let mut app = App::test_new();
        let paths = if remote {
            let workspace = crate::remote::workspace::Workspace::new(
                crate::remote::tests::connection(&root, Arc::default(), traffic.clone()),
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
    let mut f = Fixture::new(false);
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
