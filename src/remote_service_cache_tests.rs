use super::*;
use std::time::{Duration, Instant};

fn path(value: &str) -> RemotePath { RemotePath::new(value.as_bytes().to_vec()).unwrap() }
fn document(value: &str) -> Document {
    let rope = ropey::Rope::from_str(value);
    Document { revision: revision(&rope, "UTF-8"), rope, encoding: "UTF-8" }
}
fn files(service: &mut Service, known_version: Option<u64>) -> (u64, Option<Vec<RemotePath>>) {
    let Reply::Files { version, files } = service.execute(Command::Files {
        generation: 1, known_version, excludes: Default::default(),
    }).unwrap() else { panic!("filename index reply"); };
    (version, files)
}

#[test]
fn checkpoints_share_one_budget_without_cross_workspace_eviction_or_subtree_aliases() {
    let mut cache = Documents { entries: cache::Cache::with_limits(3, 1024) };
    let a = Path::new("workspace-a");
    let b = Path::new("workspace-b");
    cache.insert(a, path("dir/same.rs"), document("first root"));
    cache.insert(b, path("dir/same.rs"), document("second root"));
    cache.insert(a, path("directory/same.rs"), document("outside subtree"));
    cache.remove(a, &path("dir"), true);
    assert!(cache.get(a, &path("dir/same.rs")).is_none());
    assert_eq!(cache.get(b, &path("dir/same.rs")).unwrap().rope.to_string(), "second root");
    assert!(cache.get(a, &path("directory/same.rs")).is_some());
    cache.insert(a, path("new.rs"), document("new"));
    cache.get(b, &path("dir/same.rs"));
    cache.insert(b, path("latest.rs"), document("latest"));
    assert!(cache.get(a, &path("directory/same.rs")).is_none(), "one LRU across roots");
    cache.remove(a, &RemotePath::default(), true);
    assert!(cache.get(a, &path("new.rs")).is_none());
    assert!(cache.get(b, &path("dir/same.rs")).is_some());
    cache.insert(a, path("too-big.rs"), document(&"x".repeat(2048)));
    assert!(cache.get(a, &path("too-big.rs")).is_none());
    assert!(cache.get(b, &path("dir/same.rs")).is_some(), "oversized insertion keeps useful entries");
}

#[test]
fn switching_workspaces_reuses_filename_indexes_and_revalidates_changed_roots() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    fs::write(a.path().join("first.rs"), "a").unwrap();
    fs::write(b.path().join("second.rs"), "b").unwrap();
    let mut service = Service::new(a.path()).unwrap();
    let a_root = service.root.clone();
    let b_root = b.path().canonicalize().unwrap();
    let a_watch = service.observations.get(&a_root);
    a_watch.active(true);
    let (a_version, _) = files(&mut service, None);
    let a_built = service.index.as_ref().unwrap().built;
    service.switch_root(b_root.clone());
    let b_watch = service.observations.get(&b_root);
    b_watch.active(true);
    let (b_version, _) = files(&mut service, None);
    let b_built = service.index.as_ref().unwrap().built;
    service.switch_root(a_root.clone());
    assert!(files(&mut service, Some(a_version)).1.is_none());
    assert_eq!(service.index.as_ref().unwrap().built, a_built, "returning to a folder does not walk it again");
    fs::write(a.path().join("added.rs"), "new").unwrap();
    a_watch.invalidate();
    let (changed_version, changed) = files(&mut service, Some(a_version));
    assert_ne!(changed_version, a_version);
    assert_eq!(changed.unwrap().len(), 2);
    service.switch_root(b_root);
    assert!(files(&mut service, Some(b_version)).1.is_none());
    assert_eq!(service.index.as_ref().unwrap().built, b_built, "another root's changes do not invalidate this one");
    service.switch_root(a_root);
    service.index.as_mut().unwrap().built -= Duration::from_secs(31);
    fs::write(a.path().join("missed.rs"), "missed notification").unwrap();
    assert_eq!(files(&mut service, Some(changed_version)).1.unwrap().len(), 3, "missed watcher events keep the existing bounded fallback");
}

#[test]
fn evicted_filename_versions_cannot_hide_new_files_after_a_workspace_switch() {
    for (entry_budget, byte_budget) in [(4, 0), (0, 1024 * 1024)] {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        fs::write(a.path().join("old.rs"), "a").unwrap();
        let mut service = Service::new(a.path()).unwrap();
        service.recent_indexes = cache::Cache::with_limits(entry_budget, byte_budget);
        let a_root = service.root.clone();
        let (old_version, _) = files(&mut service, None);
        service.switch_root(b.path().canonicalize().unwrap());
        files(&mut service, None);
        fs::write(a.path().join("new.rs"), "new").unwrap();
        service.switch_root(a_root);
        let (version, current) = files(&mut service, Some(old_version));
        assert_ne!(version, old_version, "an evicted version is never reused");
        assert_eq!(current.unwrap().len(), 2);
    }
}

#[test]
fn cancelling_a_root_scan_cannot_replace_the_cached_index() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("old.rs"), "old").unwrap();
    let mut service = Service::new(root.path()).unwrap();
    let (version, _) = files(&mut service, None);
    let cancel = crate::background::CancellationToken::new();
    cancel.cancel();
    assert!(service.ensure_file_index(2, true, Some(&Default::default()), &cancel).is_err());
    assert_eq!(service.index.as_ref().unwrap().version, version);
}

#[test]
#[ignore = "Filesystem benchmark; run explicitly on an otherwise idle machine"]
fn headless_workspace_cache_benchmark() {
    let roots: Vec<_> = (0..4).map(|_| tempfile::tempdir().unwrap()).collect();
    for root in &roots {
        for directory in 0..20 {
            let directory = root.path().join(format!("source-{directory}"));
            fs::create_dir(&directory).unwrap();
            for file in 0..50 { fs::write(directory.join(format!("file-{file}.rs")), "fn main() {}\n").unwrap(); }
        }
    }
    for keep in [false, true] {
        let mut service = Service::new(roots[0].path()).unwrap();
        if !keep { service.recent_indexes = cache::Cache::with_limits(0, 0); }
        let native: Vec<_> = roots.iter().map(|root| root.path().canonicalize().unwrap()).collect();
        let observations: Vec<_> = native.iter().map(|root| {
            let watch = service.observations.get(root); watch.active(true); watch
        }).collect();
        let mut seen = vec![None; roots.len()];
        let mut scans = 0;
        let start = Instant::now();
        for visit in 0..80 {
            let root = visit % roots.len();
            service.switch_root(native[root].clone());
            files(&mut service, None);
            let built = service.index.as_ref().unwrap().built;
            if seen[root] != Some(built) { scans += 1; seen[root] = Some(built); }
        }
        eprintln!("retained_workspaces={keep}: {scans} directory walks, {:.2?}, {} observed roots", start.elapsed(), observations.len());
        assert_eq!(scans, if keep { 4 } else { 80 });
    }
}
