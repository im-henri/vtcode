use super::*;

fn snapshot(root: &str) -> Snapshot {
    Snapshot {
        root: root.into(),
        canonical_root: root.into(),
        created: Instant::now(),
        unwatched: false,
        matcher: IgnoreForest::empty(Path::new(root)),
        paths: FxHashMap::default(),
        path_bytes: 0,
    }
}

#[test]
fn invalidations_during_a_worker_checkout_survive_checkin() {
    let cache = IgnoreCache::default();
    let checkout = cache.checkout().unwrap();
    let path = Path::new("project/.gitignore");
    cache.invalidate_paths([path]);
    cache.checkin(checkout.ticket, snapshot("project"), Metrics::default());
    let next = cache.checkout().unwrap();
    assert!(next.pending.paths.contains(path));
    assert!(next.snapshot.is_some());
    cache.invalidate();
    cache.checkin(next.ticket, next.snapshot.unwrap(), Metrics::default());
    assert!(cache.checkout().unwrap().pending.all);
}

#[test]
fn superseded_worker_cannot_replace_a_newer_snapshot() {
    let cache = IgnoreCache::default();
    let old = cache.checkout().unwrap();
    let new = cache.checkout().unwrap();
    cache.checkin(new.ticket, snapshot("new"), Metrics::default());
    cache.checkin(old.ticket, snapshot("old"), Metrics::default());
    assert_eq!(cache.checkout().unwrap().snapshot.unwrap().root, Path::new("new"));
}

#[test]
fn disable_cancels_checked_out_work_and_reenable_uses_fresh_rules() {
    let cache = IgnoreCache::default();
    let old = cache.checkout().unwrap();
    cache.set_enabled(false);
    assert!(old.cancel.is_cancelled());
    assert!(cache.checkout().is_none());
    cache.invalidate_paths([Path::new("project/.gitignore")]);
    cache.set_enabled(true);
    cache.checkin(old.ticket, snapshot("old"), Metrics::default());
    let current = cache.checkout().unwrap();
    assert!(current.snapshot.is_none());
    assert!(!current.cancel.is_cancelled());
}

#[test]
fn watcher_bursts_have_bounded_pending_memory() {
    let cache = IgnoreCache::default();
    let paths: Vec<_> = (0..=MAX_INVALIDATIONS)
        .map(|i| PathBuf::from(format!("folder/{i}")))
        .collect();
    cache.invalidate_paths(paths.iter().map(PathBuf::as_path));
    let pending = cache.checkout().unwrap().pending;
    assert!(pending.all);
    assert!(pending.paths.is_empty());
}

#[test]
fn large_searches_keep_warm_paths_when_the_cache_reaches_its_limits() {
    let mut cached = snapshot("project");
    for i in 0..=MAX_PATHS {
        cached.remember(
            Path::new(&format!("file-{i}")),
            Decoration {
                is_dir: false,
                ignored: true,
            },
        );
    }
    assert_eq!(cached.paths.len(), MAX_PATHS);
    assert!(cached.paths.contains_key(Path::new("file-0")));
    assert!(
        !cached
            .paths
            .contains_key(Path::new(&format!("file-{MAX_PATHS}")))
    );
    let bytes = cached.path_bytes;
    cached.remember(
        Path::new("file-0"),
        Decoration {
            is_dir: true,
            ignored: false,
        },
    );
    assert!(cached.paths[Path::new("file-0")].is_dir);
    assert_eq!(cached.path_bytes, bytes);
    let mut small = snapshot("project");
    small.path_bytes = MAX_PATH_BYTES - 1;
    small.remember(
        Path::new("too-long"),
        Decoration {
            is_dir: false,
            ignored: true,
        },
    );
    assert!(small.paths.is_empty());
}

#[test]
fn watcher_coverage_changes_invalidate_old_snapshots() {
    let cache = IgnoreCache::default();
    for watched in [true, false] {
        cache.set_watched(watched);
        let checkout = cache.checkout().unwrap();
        assert!(checkout.pending.all);
        assert_eq!(checkout.watched, watched);
        cache.checkin(checkout.ticket, snapshot("project"), Metrics::default());
        cache.set_watched(watched);
        let unchanged = cache.checkout().unwrap();
        assert!(!unchanged.pending.all);
        cache.checkin(
            unchanged.ticket,
            unchanged.snapshot.unwrap(),
            Metrics::default(),
        );
    }
}

#[test]
fn missed_changes_expire_quickly_without_watch_coverage_and_eventually_with_it() {
    for (watched, directory) in [(false, "src"), (true, "target"), (true, "src")] {
        let temp = tempfile::tempdir().unwrap();
        let _repo = git2::Repository::init(temp.path()).unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        std::fs::write(root.join(".gitignore"), "*.tmp\n").unwrap();
        std::fs::create_dir(root.join(directory)).unwrap();
        let cache = IgnoreCache::default();
        cache.set_watched(watched);
        let mut entries = [FileEntry {
            path: root.join(directory).join("output.tmp"),
            name: "output.tmp".into(),
            is_dir: false,
            depth: 1,
            expanded: false,
            git_status: GitStatus::Unmodified,
            git_status_summary: Default::default(),
        }];
        cache.mark_ignored(&root, &root, &mut entries, None);
        assert_eq!(entries[0].git_status, GitStatus::Ignored);
        std::fs::write(root.join(".gitignore"), "!*.tmp\n").unwrap();
        cache.0.lock().unwrap().snapshot.as_mut().unwrap().created -= UNWATCHED_TTL;
        cache.mark_ignored(&root, &root, &mut entries, None);
        if watched {
            assert_eq!(cache.metrics().repository_loads, 1);
            cache.expire();
            cache.mark_ignored(&root, &root, &mut entries, None);
        }
        assert_eq!(entries[0].git_status, GitStatus::Unmodified);
        assert_eq!(cache.metrics().repository_loads, 2);
    }
}
