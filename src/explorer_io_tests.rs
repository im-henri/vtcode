use super::*;

fn refresh(explorer: &mut FileExplorer) {
    let (root, entries, matches) = FileExplorer::build_refresh_entries(explorer.refresh_request());
    explorer.apply_refresh_entries(root, entries, matches);
}

fn workspace() -> (tempfile::TempDir, FileExplorer) {
    let temp = tempfile::tempdir().unwrap();
    for name in ["alpha", "beta", "gamma"] {
        let dir = temp.path().join(name);
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("main.rs"), "fn main() {}\n").unwrap();
    }
    let explorer = FileExplorer::new_deferred(temp.path().into());
    explorer.set_directory_watcher_active(true);
    (temp, explorer)
}

#[test]
fn formatter_working_copies_are_hidden_from_explorer_and_filename_search() {
    let (temp, mut explorer) = workspace();
    let copy = temp.path().join(".vtcode-format-test");
    std::fs::create_dir(&copy).unwrap();
    std::fs::write(copy.join("main.rs"), "temporary text").unwrap();
    refresh(&mut explorer);
    assert!(!explorer.entries.iter().any(|entry| entry.path.starts_with(&copy)));
    explorer.find.open = true;
    explorer.find.filter = true;
    explorer.find.query = "main.rs".into();
    refresh(&mut explorer);
    assert_eq!(explorer.find.results.count, 3);
    assert!(!explorer.entries.iter().any(|entry| entry.path.starts_with(copy.as_path())));
}

#[test]
fn expanding_and_collapsing_only_reads_new_directories() {
    let (_temp, mut explorer) = workspace();
    refresh(&mut explorer);
    assert_eq!(explorer.directory_cache.read_count(), 1);
    for name in ["alpha", "beta", "gamma"] {
        explorer.expanded_dirs.insert(explorer.root.join(name));
        refresh(&mut explorer);
    }
    assert_eq!(explorer.directory_cache.read_count(), 4);
    for _ in 0..10 {
        explorer.expanded_dirs.clear();
        refresh(&mut explorer);
        for name in ["alpha", "beta", "gamma"] {
            explorer.expanded_dirs.insert(explorer.root.join(name));
        }
        refresh(&mut explorer);
    }
    assert_eq!(explorer.entries.len(), 6);
    assert_eq!(explorer.directory_cache.read_count(), 4);
}

#[test]
fn repeated_filename_queries_reuse_the_workspace_scan() {
    let (_temp, mut explorer) = workspace();
    explorer.find.open = true;
    explorer.find.filter = true;
    for query in ["m", "ma", "main", "main.rs", "nothing", "main"] {
        explorer.find.query = query.into();
        refresh(&mut explorer);
    }
    assert_eq!(explorer.find.results.count, 3);
    assert_eq!(explorer.directory_cache.read_count(), 4);
}

#[test]
fn create_delete_and_rename_invalidate_only_affected_listings() {
    let (temp, mut explorer) = workspace();
    for name in ["alpha", "beta", "gamma"] {
        explorer.expanded_dirs.insert(temp.path().join(name));
    }
    refresh(&mut explorer);
    let added = temp.path().join("beta/added.rs");
    std::fs::write(&added, "new").unwrap();
    explorer.invalidate_directory_paths([added.as_path()], true);
    refresh(&mut explorer);
    assert!(explorer.entries.iter().any(|entry| entry.path == added));
    assert_eq!(explorer.directory_cache.read_count(), 5);

    std::fs::remove_file(&added).unwrap();
    explorer.invalidate_directory_paths([added.as_path()], true);
    refresh(&mut explorer);
    assert!(!explorer.entries.iter().any(|entry| entry.path == added));
    assert_eq!(explorer.directory_cache.read_count(), 6);

    let old = temp.path().join("alpha");
    let renamed = temp.path().join("renamed");
    std::fs::rename(&old, &renamed).unwrap();
    explorer.invalidate_directory_paths([old.as_path(), renamed.as_path()], true);
    explorer.expanded_dirs.insert(renamed.clone());
    refresh(&mut explorer);
    assert!(!explorer
        .entries
        .iter()
        .any(|entry| entry.path.starts_with(&old)));
    assert!(explorer
        .entries
        .iter()
        .any(|entry| entry.path == renamed.join("main.rs")));
    assert_eq!(explorer.directory_cache.read_count(), 8);
}

#[test]
fn content_changes_keep_names_cached_but_refresh_size_sort_keys() {
    let (temp, mut explorer) = workspace();
    let dir = temp.path().join("alpha");
    let changed = dir.join("main.rs");
    let other = dir.join("other.rs");
    std::fs::write(&other, "small").unwrap();
    explorer.expanded_dirs.insert(dir.clone());
    refresh(&mut explorer);
    let before = explorer.directory_cache.read_count();
    explorer.invalidate_directory_paths([changed.as_path()], false);
    refresh(&mut explorer);
    assert_eq!(explorer.directory_cache.read_count(), before);

    explorer.sort_mode = ExplorerSortMode::Size;
    refresh(&mut explorer);
    let before = explorer.directory_cache.read_count();
    std::fs::write(&other, "longer than the original file contents").unwrap();
    explorer.invalidate_directory_paths([other.as_path()], false);
    refresh(&mut explorer);
    assert_eq!(explorer.directory_cache.read_count(), before + 1);
    let children: Vec<_> = explorer
        .entries
        .iter()
        .filter(|entry| entry.path.parent() == Some(dir.as_path()))
        .collect();
    assert_eq!(children[0].path, other);
}

#[test]
fn explicit_refresh_discards_cached_directory_contents() {
    let (temp, mut explorer) = workspace();
    refresh(&mut explorer);
    std::fs::write(temp.path().join("external.txt"), "new").unwrap();
    explorer.refresh();
    assert!(explorer
        .entries
        .iter()
        .any(|entry| entry.name == "external.txt"));
    assert_eq!(explorer.directory_cache.read_count(), 2);
}

#[test]
fn reveal_discovers_new_ancestors_without_rereading_known_closed_folders() {
    let (temp, mut explorer) = workspace();
    explorer.expanded_dirs.insert(temp.path().join("alpha"));
    refresh(&mut explorer);
    explorer.expanded_dirs.clear();
    refresh(&mut explorer);
    let reads = explorer.directory_cache.read_count();
    let known = temp.path().join("alpha/main.rs");
    assert!(explorer.reveal_file_deferred(&known));
    refresh(&mut explorer);
    assert_eq!(explorer.entries[explorer.selected].path, known);
    assert_eq!(explorer.directory_cache.read_count(), reads);

    let added = temp.path().join("new/deep/new.rs");
    std::fs::create_dir_all(added.parent().unwrap()).unwrap();
    std::fs::write(&added, "new").unwrap();
    assert!(explorer.reveal_file_deferred(&added));
    refresh(&mut explorer);
    assert_eq!(explorer.entries[explorer.selected].path, added);
    assert_eq!(explorer.directory_cache.read_count(), reads + 3);
}

#[test]
fn canonical_notifications_invalidate_a_noncanonical_workspace_listing() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("child")).unwrap();
    let root = temp.path().join("child/..");
    let mut explorer = FileExplorer::new_deferred(root.clone());
    explorer.set_directory_watcher_active(true);
    refresh(&mut explorer);
    let added = root.join("new.rs");
    std::fs::write(&added, "new").unwrap();
    let canonical = std::fs::canonicalize(&added).unwrap();
    explorer.invalidate_directory_paths([canonical.as_path()], true);
    refresh(&mut explorer);
    assert!(explorer.entries.iter().any(|entry| entry.path == added));
    assert_eq!(explorer.directory_cache.read_count(), 2);
}
