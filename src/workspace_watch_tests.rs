use super::*;
use notify::{Event, EventKind, event::{DataChange, ModifyKind}};

#[test]
fn private_git_storage_is_pruned_without_excluding_project_directory_names() {
    let root = Path::new("workspace");
    for path in ["build/generated", "target/debug", "node_modules/package", ".cache/data",
        "objects/source", "logs/source", "src/.git/refs/heads", "src/.git/info"] {
        assert!(directory_is_relevant(root, &root.join(path), &[]), "{path}");
    }
    for path in [".git/objects", ".git/objects/ab", ".git/logs", "src/.git/objects/pack"] {
        assert!(!directory_is_relevant(root, &root.join(path), &[]), "{path}");
    }
    let external = PathBuf::from("metadata");
    assert!(directory_is_relevant(root, &external.join("refs/heads"), &[external.clone()]));
    assert!(!directory_is_relevant(root, &external.join("objects/pack"), &[external.clone()]));
    let nested = external.join("objects/linked-metadata");
    let scopes = [external.clone(), nested.clone()];
    assert!(directory_is_relevant(root, &external.join("objects"), &scopes));
    assert!(directory_is_relevant(root, &nested, &scopes));
    assert!(directory_is_relevant(root, &nested.join("refs/heads"), &scopes));
    assert!(!directory_is_relevant(root, &nested.join("objects/pack"), &scopes));
    assert!(!directory_is_relevant(root, &external.join("objects/unrelated"), &scopes));
    // An external Git directory can contain the actual project workspace.
    assert!(directory_is_relevant(root, &root.join("objects/project"), &[PathBuf::new()]));
}

#[test]
fn shared_event_batch_keeps_hot_files_bounded_and_compacts_large_subtrees() {
    let root = Path::new("workspace");
    let mut burst = Burst::default();
    for _ in 0..100_000 {
        burst.push(root, Ok(Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Any)))
            .add_path(root.join("build/source.rs"))));
    }
    assert_eq!(burst.event.paths, [root.join("build/source.rs")]);
    assert_eq!(burst.paths.len(), 1);
    assert_eq!(burst.data.len(), 1);
    assert!(!burst.event.rescan);
    for index in 0..2_000 {
        burst.push(root, Ok(Event::new(EventKind::Create(notify::event::CreateKind::File))
            .add_path(root.join(format!("files/{index}")))));
    }
    assert!(!burst.event.rescan && burst.event.structure_changed);
    assert_eq!(burst.event.paths, [root.join("build/source.rs"), root.join("files")]);
    assert_eq!(burst.paths.len(), 2);
    assert_eq!(burst.data.len(), 1);
    assert!(burst.metadata.is_empty() && burst.git_paths.is_empty());
}

#[test]
fn compiler_output_bursts_keep_source_and_git_changes_exact_with_bounded_state() {
    let root = Path::new("workspace");
    let source = root.join("src/main.rs");
    let ignore = root.join("target/debug/deps/.gitignore");
    let mut burst = Burst::default();
    burst.push(root, Ok(Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Any))).add_path(source.clone())));
    for index in 0..50_000 {
        burst.push(root, Ok(Event::new(EventKind::Create(notify::event::CreateKind::File))
            .add_path(root.join(format!("target/debug/deps/object-{index}.o")))));
    }
    burst.push(root, Ok(Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Any))).add_path(ignore.clone())));
    burst.push(root, Ok(Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Any))).add_path(root.join(".git/index"))));
    assert!(!burst.event.rescan);
    assert!(burst.event.structure_changed && burst.event.git_metadata_changed);
    assert_eq!(burst.paths.len(), 3);
    assert!(burst.paths.contains(&source));
    assert!(burst.paths.contains(&ignore));
    assert!(burst.paths.contains(&root.join("target/debug/deps")));
    assert!(burst.metadata.len() + burst.data.len() <= 4);
    assert_eq!(burst.git_paths.len(), 1);
    assert_eq!(burst.subtrees.len(), 1);
}

#[test]
fn dispersed_output_directories_compact_without_swallowing_an_isolated_source_edit() {
    let root = Path::new("workspace");
    let mut burst = Burst::default();
    let source = root.join("main.rs");
    burst.push(root, Ok(Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Any))).add_path(source.clone())));
    for index in 0..20_000 {
        burst.push(root, Ok(Event::new(EventKind::Create(notify::event::CreateKind::File))
            .add_path(root.join(format!("build/unit-{index}/object.o")))));
    }
    assert!(!burst.event.rescan);
    assert_eq!(burst.paths.len(), 2);
    assert!(burst.paths.contains(&source));
    assert!(burst.paths.contains(&root.join("build")));
    burst.push(root, Err(notify::Error::generic("watcher lost coverage")));
    assert!(burst.event.rescan && burst.event.watcher_failed);
    assert!(burst.paths.is_empty() && burst.subtrees.is_empty());
}

#[test]
fn large_single_native_events_and_repository_topology_changes_are_not_lost() {
    let root = Path::new("workspace");
    let mut burst = Burst::default();
    let mut event = Event::new(EventKind::Create(notify::event::CreateKind::File));
    for index in 0..5_000 { event.paths.push(root.join(format!("output/item-{index}.o"))); }
    event.paths.push(root.join("nested/.git"));
    burst.push(root, Ok(event));
    assert!(!burst.event.rescan);
    assert!(burst.event.git_metadata_changed && burst.event.structure_changed);
    assert_eq!(burst.paths.len(), 2);
    assert!(burst.paths.contains(&root.join("output")));
    assert!(burst.paths.contains(&root.join("nested/.git")));
}

#[test]
fn losing_the_workspace_root_restores_fallback_coverage() {
    let root = Path::new("workspace");
    for kind in [
        EventKind::Remove(notify::event::RemoveKind::Folder),
        EventKind::Modify(ModifyKind::Name(notify::event::RenameMode::From)),
    ] {
        let mut burst = Burst::default();
        burst.push(root, Ok(Event::new(kind).add_path(root.to_owned())));
        assert!(burst.event.rescan && burst.event.watcher_failed);
    }
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "isolated Linux watch-count fixture; run alone with --nocapture"]
fn linux_watch_resource_fixture() {
    use notify::{RecursiveMode, Watcher};
    use std::{collections::HashSet, time::{Duration, Instant}};

    fn descriptors() -> HashSet<u32> {
        std::fs::read_dir("/proc/self/fd").unwrap().filter_map(|entry| {
            let entry = entry.ok()?;
            let fd = entry.file_name().to_str()?.parse().ok()?;
            std::fs::read_link(entry.path()).ok()?.to_string_lossy()
                .contains("inotify").then_some(fd)
        }).collect()
    }
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    drop(git2::Repository::init(root.path()).unwrap());
    drop(git2::Repository::init_bare(outside.path()).unwrap());
    for directory in [root.path().join(".git"), outside.path().to_owned()] {
        for index in 0..128 {
            std::fs::create_dir_all(directory.join(format!("objects/{index:02x}/nested/deeper"))).unwrap();
        }
        std::fs::create_dir_all(directory.join("logs/refs/heads")).unwrap();
    }
    for index in 0..32 {
        std::fs::create_dir_all(root.path().join(format!("build/generated/{index}/source"))).unwrap();
    }
    std::fs::create_dir_all(root.path().join("node_modules/project/source")).unwrap();
    let config = notify::Config::default().with_open_events(false).with_follow_symlinks(false);
    let mut counts = Vec::new();
    for filtered in [false, true] {
        let before = descriptors();
        let scope = root.path().to_owned();
        let git = vec![outside.path().to_owned()];
        let mut watcher = native_watcher(|_| {}, config,
            move |path| !filtered || directory_is_relevant(&scope, path, &git)).unwrap();
        let added: Vec<_> = descriptors().difference(&before).copied().collect();
        assert_eq!(added.len(), 1);
        let fd = added[0];
        watcher.watch(root.path(), RecursiveMode::Recursive).unwrap();
        watcher.watch(outside.path(), RecursiveMode::Recursive).unwrap();
        let count = || std::fs::read_to_string(format!("/proc/self/fdinfo/{fd}")).unwrap()
            .lines().filter(|line| line.starts_with("inotify wd:")).count();
        counts.push(count());
        if filtered {
            // New repositories must retain the same policy after startup.
            let later = root.path().join("later/.git/objects/ab/child");
            std::fs::create_dir_all(&later).unwrap();
            let source = root.path().join("later/build/source");
            std::fs::create_dir_all(&source).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while count() < counts[1] + 4 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(count(), counts[1] + 4, "only later, .git, build and source need watches");
        }
        drop(watcher);
        let deadline = Instant::now() + Duration::from_secs(5);
        while descriptors().contains(&fd) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!descriptors().contains(&fd));
    }
    assert!(counts[1] * 4 < counts[0], "{counts:?}");
    println!("VTCODE_WATCH_METRICS {}", serde_json::json!({
        "baseline_kernel_watches": counts[0], "filtered_kernel_watches": counts[1],
        "project_directory_coverage": true, "external_git_metadata_coverage": true
    }));
}
