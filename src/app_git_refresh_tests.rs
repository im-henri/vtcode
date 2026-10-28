use super::*;
use crate::app::SidebarView;
use std::time::{Duration, Instant};

#[path = "app_git_untracked_tests.rs"]
mod untracked;

fn init(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    let repo = git2::Repository::init(path).unwrap();
    repo.config()
        .unwrap()
        .set_bool("core.autocrlf", false)
        .unwrap();
    std::fs::write(path.join("first.txt"), "first\n").unwrap();
    std::fs::write(path.join("other.txt"), "other\n").unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = git2::Signature::now("Test", "test@example.com").unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "Initial", &tree, &[])
        .unwrap();
}

fn app_at(path: &Path) -> App {
    let mut app = App::test_new();
    app.explorer = crate::explorer::FileExplorer::new_deferred(path.to_owned());
    app.sidebar_view = SidebarView::SourceControl;
    app.workspace_watcher_active = true;
    app
}

fn settle(app: &mut App) {
    app.wait_for_background(Duration::from_secs(15), |app| {
        !app.bg_git_status_running && !app.bg_git_status_pending
    });
}

fn full(app: &mut App) {
    app.refresh_source_control_async();
    settle(app);
}

fn changed(app: &mut App, paths: impl IntoIterator<Item = PathBuf>) {
    app.refresh_source_control_changed_paths(paths);
    settle(app);
}

fn files<'a>(app: &'a App, root: &Path) -> &'a [GitFileChange] {
    if app.repositories.active.as_deref() == Some(root) {
        &app.source_control_files
    } else {
        &app.repositories
            .entries
            .iter()
            .find(|repo| repo.root == root)
            .unwrap()
            .files
    }
}

fn key(files: &[GitFileChange]) -> Vec<(String, Option<String>, char, bool)> {
    files
        .iter()
        .map(|file| {
            (
                file.path.clone(),
                file.original_path.clone(),
                file.status,
                file.staged,
            )
        })
        .collect()
}

fn assert_matches_full(app: &App, root: &Path) {
    let expected = collect_source_control_status_cancellable(
        root.to_owned(),
        || {},
        &CancellationToken::new(),
    );
    assert_eq!(key(files(app, root)), key(&expected.files));
}

#[test]
fn worktree_prefixes_are_literal_component_scopes_and_collapse_redundant_paths() {
    let mut paths = WorktreePaths::new(vec![
        "dir/".into(),
        "dir/file".into(),
        "dir/child/leaf".into(),
        "[literal]".into(),
        "界/a".into(),
        "dir".into(),
    ]);
    assert_eq!(paths.paths.len(), 3);
    for name in [
        "dir",
        "dir/file",
        "dir/new/deleted",
        "[literal]",
        "界/a/child",
    ] {
        assert!(paths.contains(name), "{name}");
    }
    for name in ["dir-other", "dirs/file", "literal", "界/ab"] {
        assert!(!paths.contains(name), "{name}");
    }
    assert!(!paths.contains("DIR/File"));
    paths.set_ignore_case(true);
    assert!(paths.contains("DIR/File"));
    #[cfg(windows)]
    assert!(paths.contains("DIR\\File"));
    paths.set_ignore_case(false);
    assert!(!paths.contains("DIR/File"));
}

#[test]
fn local_file_events_scan_only_the_affected_repository_and_literal_paths() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let left = root.join("left");
    let right = root.join("right");
    init(&left);
    init(&right);
    std::fs::write(right.join("first.txt"), "cached modification\n").unwrap();
    let mut app = app_at(&root);
    full(&mut app);
    // Deliberately withhold these two events: they prove a scoped refresh did
    // not read the other repository or an unrelated path in the selected one.
    std::fs::write(right.join("first.txt"), "first\n").unwrap();
    std::fs::write(left.join("other.txt"), "unreported\n").unwrap();
    std::fs::write(left.join("first.txt"), "reported\n").unwrap();
    changed(&mut app, [left.join("first.txt")]);
    assert_eq!(files(&app, &left).len(), 1);
    assert_eq!(files(&app, &left)[0].path, "first.txt");
    assert_eq!(files(&app, &right).len(), 1);
    assert!(
        app.explorer
            .git_statuses
            .contains_key(&right.join("first.txt"))
    );
    assert!(
        app.explorer
            .git_statuses
            .contains_key(&left.join("first.txt"))
    );
    full(&mut app);
    assert_matches_full(&app, &left);
    assert_matches_full(&app, &right);
    assert!(
        !app.explorer
            .git_statuses
            .contains_key(&right.join("first.txt"))
    );
}

#[test]
fn selective_updates_preserve_staged_renames_and_the_focused_selection() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    init(&root);
    let repo = git2::Repository::open(&root).unwrap();
    std::fs::rename(root.join("first.txt"), root.join("renamed.txt")).unwrap();
    let mut index = repo.index().unwrap();
    index.remove_path(Path::new("first.txt")).unwrap();
    index.add_path(Path::new("renamed.txt")).unwrap();
    index.write().unwrap();
    let mut app = app_at(&root);
    full(&mut app);
    assert_eq!(app.source_control_files[0].status, 'R');
    app.source_control_selected = 0;
    std::fs::write(root.join("renamed.txt"), "edited after staging\n").unwrap();
    changed(&mut app, [root.join("renamed.txt")]);
    assert_matches_full(&app, &root);
    assert_eq!(
        app.source_control_files[0].original_path.as_deref(),
        Some("first.txt")
    );
    assert_eq!(app.source_control_selected, 0);
    assert_eq!(app.source_control_files.len(), 2);
    std::fs::write(root.join("renamed.txt"), "first\n").unwrap();
    changed(&mut app, [root.join("renamed.txt")]);
    assert_matches_full(&app, &root);
    assert_eq!(app.source_control_files.len(), 1);
}

#[test]
fn directory_create_rename_delete_and_recreation_replace_the_whole_changed_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    init(&root);
    let mut app = app_at(&root);
    full(&mut app);
    std::fs::create_dir_all(root.join("dir/sub")).unwrap();
    std::fs::write(root.join("dir/sub/界.txt"), "new\n").unwrap();
    std::fs::write(root.join("dir/one.txt"), "new\n").unwrap();
    changed(&mut app, [root.join("dir")]);
    assert_matches_full(&app, &root);
    assert_eq!(app.source_control_files.len(), 2);
    std::fs::rename(root.join("dir"), root.join("moved")).unwrap();
    changed(&mut app, [root.join("dir"), root.join("moved")]);
    assert_matches_full(&app, &root);
    std::fs::remove_dir_all(root.join("moved")).unwrap();
    changed(&mut app, [root.join("moved")]);
    assert_matches_full(&app, &root);
    assert!(app.source_control_files.is_empty());
    std::fs::create_dir_all(root.join("moved")).unwrap();
    std::fs::write(root.join("moved/new.txt"), "again\n").unwrap();
    changed(&mut app, [root.join("moved")]);
    assert_matches_full(&app, &root);
}

#[test]
fn ignore_and_attribute_events_refresh_other_paths_in_their_repository() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    init(&root);
    std::fs::write(root.join("generated.bin"), "new\n").unwrap();
    let mut app = app_at(&root);
    full(&mut app);
    std::fs::write(root.join(".gitignore"), "generated.bin\n").unwrap();
    changed(&mut app, [root.join(".gitignore")]);
    assert_matches_full(&app, &root);
    assert!(
        !app.source_control_files
            .iter()
            .any(|file| file.path == "generated.bin")
    );
    std::fs::write(root.join(".gitignore"), "").unwrap();
    changed(&mut app, [root.join(".gitignore")]);
    assert_matches_full(&app, &root);
    assert!(
        app.source_control_files
            .iter()
            .any(|file| file.path == "generated.bin")
    );
    std::fs::write(root.join("first.txt"), "outside scoped path\n").unwrap();
    std::fs::write(root.join(".gitattributes"), "*.txt text\n").unwrap();
    changed(&mut app, [root.join(".gitattributes")]);
    assert_matches_full(&app, &root);
}

#[test]
fn metadata_initial_discovery_and_unwatched_refreshes_still_scan_every_repository() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let left = root.join("left");
    let right = root.join("right");
    init(&left);
    init(&right);
    std::fs::write(left.join("first.txt"), "left\n").unwrap();
    std::fs::write(right.join("first.txt"), "right\n").unwrap();
    let mut app = app_at(&root);
    changed(&mut app, [left.join("first.txt")]);
    assert_matches_full(&app, &left);
    assert_matches_full(&app, &right);
    std::fs::write(right.join("first.txt"), "first\n").unwrap();
    changed(&mut app, [left.join(".git/index")]);
    assert_matches_full(&app, &left);
    assert_matches_full(&app, &right);
    app.workspace_watcher_active = false;
    std::fs::write(right.join("other.txt"), "unwatched\n").unwrap();
    changed(&mut app, [left.join("first.txt")]);
    assert_matches_full(&app, &left);
    assert_matches_full(&app, &right);
}

#[test]
fn local_pending_paths_merge_without_downgrading_a_full_refresh() {
    let mut app = App::test_new();
    app.workspace_watcher_active = true;
    app.bg_git_status_running = true;
    app.refresh_source_control_changed_paths(["first".into()]);
    app.refresh_source_control_changed_paths(["second".into()]);
    assert_eq!(app.pending_git_status_paths.as_ref().unwrap().len(), 2);
    app.refresh_source_control_async();
    app.refresh_source_control_changed_paths(["third".into()]);
    assert!(app.pending_git_status_paths.is_none());
}

#[test]
fn worktree_merge_obeys_git_case_policy_without_dropping_other_case_variants() {
    for ignore_case in [false, true] {
        let mut paths = WorktreePaths::new(vec!["file.txt".into()]);
        paths.set_ignore_case(ignore_case);
        let mut files: Vec<_> = ["File.txt", "file.txt"]
            .into_iter()
            .map(|path| GitFileChange {
                path: path.into(),
                original_path: None,
                status: 'M',
                staged: false,
            })
            .collect();
        let mut selection = crate::app_source_control::SourceControlSelection::default();
        selection.update_worktree(&mut files, 0, Vec::new(), &paths, Default::default());
        assert_eq!(files.len(), usize::from(!ignore_case));
        if !ignore_case {
            assert_eq!(files[0].path, "File.txt");
        }
    }
}

#[test]
fn tracked_directory_deletions_keep_index_changes_and_recreation_removes_stale_deletions() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    init(&root);
    std::fs::create_dir_all(root.join("tracked/sub")).unwrap();
    std::fs::write(root.join("tracked/sub/file.txt"), "staged contents\n").unwrap();
    let repo = git2::Repository::open(&root).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("tracked/sub/file.txt")).unwrap();
    index.write().unwrap();
    let mut app = app_at(&root);
    full(&mut app);
    std::fs::remove_dir_all(root.join("tracked")).unwrap();
    changed(&mut app, [root.join("tracked")]);
    assert_matches_full(&app, &root);
    assert!(
        app.source_control_files
            .iter()
            .any(|file| file.status == 'A' && file.staged)
    );
    assert!(
        app.source_control_files
            .iter()
            .any(|file| file.status == 'D' && !file.staged)
    );
    std::fs::create_dir_all(root.join("tracked/sub")).unwrap();
    std::fs::write(root.join("tracked/sub/file.txt"), "staged contents\n").unwrap();
    changed(&mut app, [root.join("tracked")]);
    assert_matches_full(&app, &root);
    assert_eq!(app.source_control_files.len(), 1);
}

#[test]
fn changed_nested_worktrees_refresh_the_superproject_gitlink_and_child_file() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    init(&root);
    let nested = root.join("nested");
    init(&nested);
    let child = git2::Repository::open(&nested).unwrap();
    let parent = git2::Repository::open(&root).unwrap();
    let mut entry = child
        .index()
        .unwrap()
        .get_path(Path::new("first.txt"), 0)
        .unwrap();
    entry.path = b"nested".to_vec();
    entry.flags = 6;
    entry.mode = 0o160000;
    entry.id = child.head().unwrap().target().unwrap();
    std::fs::write(
        root.join(".gitmodules"),
        "[submodule \"nested\"]\n\tpath = nested\n\turl = ./nested\n",
    )
    .unwrap();
    let mut index = parent.index().unwrap();
    index.add(&entry).unwrap();
    index.add_path(Path::new(".gitmodules")).unwrap();
    index.write().unwrap();
    let mut app = app_at(&root);
    full(&mut app);
    assert_eq!(app.repositories.entries.len(), 2);
    std::fs::write(nested.join("first.txt"), "child change\n").unwrap();
    changed(&mut app, [nested.join("first.txt")]);
    assert_matches_full(&app, &root);
    assert_matches_full(&app, &nested);
    assert!(
        files(&app, &root)
            .iter()
            .any(|file| file.path == "nested" && !file.staged)
    );
    std::fs::write(nested.join("first.txt"), "first\n").unwrap();
    changed(&mut app, [nested.join("first.txt")]);
    assert_matches_full(&app, &root);
    assert_matches_full(&app, &nested);
}

#[test]
fn literal_file_events_do_not_treat_brackets_as_git_glob_patterns() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    init(&root);
    let mut app = app_at(&root);
    full(&mut app);
    std::fs::write(root.join("[a].txt"), "reported\n").unwrap();
    std::fs::write(root.join("a.txt"), "unreported\n").unwrap();
    changed(&mut app, [root.join("[a].txt")]);
    assert_eq!(app.source_control_files.len(), 1);
    assert_eq!(app.source_control_files[0].path, "[a].txt");
}

#[test]
fn deleted_repository_ancestors_prune_only_missing_projects_and_keep_other_badges() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let left = root.join("group/left");
    let right = root.join("right");
    init(&left);
    init(&right);
    std::fs::write(right.join("first.txt"), "kept\n").unwrap();
    let mut app = app_at(&root);
    full(&mut app);
    std::fs::remove_dir_all(root.join("group")).unwrap();
    changed(&mut app, [root.join("group")]);
    assert_eq!(app.repositories.entries.len(), 1);
    assert_eq!(app.repositories.entries[0].root, right);
    assert_matches_full(&app, &right);
    assert!(
        app.explorer
            .git_statuses
            .contains_key(&right.join("first.txt"))
    );
}

#[test]
fn independent_repository_selections_survive_partial_updates_and_clean_reverts() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let left = root.join("left");
    let right = root.join("right");
    init(&left);
    init(&right);
    for repo in [&left, &right] {
        std::fs::write(repo.join("first.txt"), "first changed\n").unwrap();
        std::fs::write(repo.join("other.txt"), "other changed\n").unwrap();
    }
    let mut app = app_at(&root);
    full(&mut app);
    app.select_source_control_repository(&right);
    settle(&mut app);
    app.source_control_selected = 1;
    app.select_source_control_repository(&left);
    settle(&mut app);
    app.source_control_selected = 1;
    std::fs::write(right.join("first.txt"), "first\n").unwrap();
    changed(&mut app, [right.join("first.txt")]);
    assert_eq!(
        app.source_control_files[app.source_control_selected].path,
        "other.txt"
    );
    assert_matches_full(&app, &left);
    assert_matches_full(&app, &right);
    app.select_source_control_repository(&right);
    settle(&mut app);
    assert_eq!(
        app.source_control_files[app.source_control_selected].path,
        "other.txt"
    );
}

#[cfg(unix)]
#[test]
fn aliased_workspaces_and_deleted_path_traversals_map_to_canonical_repositories() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("real");
    init(&root);
    let root = std::fs::canonicalize(root).unwrap();
    let alias = temp.path().join("alias");
    std::os::unix::fs::symlink(&root, &alias).unwrap();
    let mut app = app_at(&alias);
    full(&mut app);
    std::fs::write(root.join("first.txt"), "via alias\n").unwrap();
    changed(&mut app, [alias.join("first.txt")]);
    assert_matches_full(&app, &root);
    std::fs::create_dir(root.join("dir")).unwrap();
    std::fs::write(root.join("other.txt"), "traversal\n").unwrap();
    changed(&mut app, [alias.join("dir/../other.txt")]);
    assert_matches_full(&app, &root);
    std::fs::remove_file(root.join("other.txt")).unwrap();
    changed(&mut app, [alias.join("dir/../other.txt")]);
    assert_matches_full(&app, &root);
}

#[cfg(windows)]
#[test]
fn windows_event_paths_accept_case_and_extended_length_aliases() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    init(&root);
    let mut app = app_at(&root);
    full(&mut app);
    std::fs::write(root.join("first.txt"), "changed\n").unwrap();
    let alternate = PathBuf::from(
        root.join("first.txt")
            .to_string_lossy()
            .to_ascii_uppercase(),
    );
    changed(&mut app, [alternate]);
    assert_matches_full(&app, &root);
}

#[cfg(unix)]
#[test]
fn unrepresentable_path_events_keep_distinct_entries_with_the_same_display_spelling() {
    use std::os::unix::ffi::OsStringExt;
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    init(&root);
    let first = root.join(std::ffi::OsString::from_vec(vec![b'a', 0xfe]));
    let second = root.join(std::ffi::OsString::from_vec(vec![b'a', 0xff]));
    std::fs::write(&first, "first\n").unwrap();
    std::fs::write(&second, "second\n").unwrap();
    let mut app = app_at(&root);
    full(&mut app);
    assert_eq!(app.source_control_files.len(), 2);
    std::fs::write(&first, "updated\n").unwrap();
    changed(&mut app, [first]);
    assert_matches_full(&app, &root);
    assert_eq!(app.source_control_files.len(), 2);
}

#[test]
#[ignore = "manual optimized benchmark: full local status versus watcher path scopes"]
fn benchmark_local_changed_path_status() {
    use std::hint::black_box;
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let roots: Vec<_> = (0..4)
        .map(|index| root.join(format!("project-{index}")))
        .collect();
    for repo in &roots {
        init(repo);
        std::fs::create_dir(repo.join("generated")).unwrap();
        for index in 0..1500 {
            std::fs::write(repo.join(format!("generated/file-{index:04}")), "output\n").unwrap();
        }
        std::fs::write(repo.join("first.txt"), "changed\n").unwrap();
    }
    let paths: FxHashSet<_> = [roots[0].join("first.txt")].into_iter().collect();
    let cancel = CancellationToken::new();
    let mut timings = [Vec::new(), Vec::new()];
    for sample in 0..9 {
        for optimized in [sample % 2 == 0, sample % 2 != 0] {
            let mut discovery = Discovery {
                roots: roots.clone(),
                parent: None,
                workspace: root.clone(),
            };
            let start = Instant::now();
            let scans = collect(
                &mut discovery,
                roots.clone(),
                optimized.then(|| paths.clone()),
                &root,
                &cancel,
                true,
            );
            let elapsed = start.elapsed();
            assert_eq!(scans.len(), if optimized { 1 } else { 4 });
            assert_eq!(
                scans.iter().map(|scan| scan.files.len()).sum::<usize>(),
                if optimized { 1 } else { 6004 }
            );
            black_box(&scans);
            if sample > 1 {
                timings[usize::from(optimized)].push(elapsed);
            }
        }
    }
    for times in &mut timings {
        times.sort();
    }
    eprintln!(
        "local status, 4 repos / 6,000 generated files: full={:?}, scoped={:?}; repositories=4->1, status rows=6004->1",
        timings[0][timings[0].len() / 2],
        timings[1][timings[1].len() / 2]
    );
}

#[test]
#[ignore = "manual remote status timing and protocol traffic measurement"]
fn benchmark_remote_changed_path_status() {
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    init(&root);
    let outputs = root.join("generated");
    std::fs::create_dir(&outputs).unwrap();
    for index in 0u64..6000 {
        let hash = index.wrapping_mul(0x9e3779b97f4a7c15).rotate_left(17);
        std::fs::write(outputs.join(format!("file-{index:04}-{hash:016x}")), "output\n").unwrap();
    }
    let incoming = Arc::new(AtomicUsize::new(0));
    let outgoing = Arc::new(AtomicUsize::new(0));
    let workspace = crate::remote::workspace::Workspace::new(crate::remote::tests::connection_with_traffic(
        &root, Arc::default(), outgoing.clone(), Arc::default(), incoming.clone()));
    let mut app = app_at(&workspace.view_root);
    let key = workspace.target_key("first.txt").unwrap();
    app.remote_workspace = Some(workspace.clone());
    full(&mut app);
    let mut times = [Vec::new(), Vec::new()];
    let mut bytes = [Vec::new(), Vec::new()];
    for sample in 0..9 {
        for scoped in [sample % 2 == 0, sample % 2 != 0] {
            std::fs::write(root.join("first.txt"), format!("changed {sample} {scoped}\n")).unwrap();
            let before = incoming.load(Ordering::Relaxed) + outgoing.load(Ordering::Relaxed);
            let started = Instant::now();
            if scoped { changed(&mut app, [key.clone()]); }
            else { full(&mut app); }
            let elapsed = started.elapsed();
            let traffic = incoming.load(Ordering::Relaxed) + outgoing.load(Ordering::Relaxed) - before;
            assert_eq!(app.source_control_files.len(), 6001);
            assert!(app.source_control_files.iter().any(|file| file.path == "first.txt" && file.status == 'M'));
            if sample > 1 {
                times[usize::from(scoped)].push(elapsed.as_secs_f64() * 1000.);
                bytes[usize::from(scoped)].push(traffic);
            }
        }
    }
    for values in &mut times { values.sort_by(f64::total_cmp); }
    for values in &mut bytes { values.sort_unstable(); }
    println!("VTCODE_REMOTE_GIT_STATUS {}", serde_json::json!({
        "untracked_files":6000,
        "full_ms":times[0][times[0].len()/2], "scoped_ms":times[1][times[1].len()/2],
        "full_bytes":bytes[0][bytes[0].len()/2], "scoped_bytes":bytes[1][bytes[1].len()/2]
    }));
}

fn remote_app_at(root: &Path) -> (App, crate::remote::workspace::Workspace) {
    let workspace = crate::remote::workspace::Workspace::new(crate::remote::tests::connection(
        root, Default::default(), Default::default()));
    let mut app = app_at(&workspace.view_root);
    app.remote_workspace = Some(workspace.clone());
    full(&mut app);
    (app, workspace)
}

#[test]
fn remote_status_deltas_preserve_staged_renames_and_query_literal_paths() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    init(&root);
    std::fs::rename(root.join("first.txt"), root.join("renamed.txt")).unwrap();
    let repo = git2::Repository::open(&root).unwrap();
    let mut index = repo.index().unwrap();
    index.remove_path(Path::new("first.txt")).unwrap();
    for name in ["literal[1].rs", "literal1.rs", "renamed.txt"] {
        if name != "renamed.txt" { std::fs::write(root.join(name), "original\n").unwrap(); }
        index.add_path(Path::new(name)).unwrap();
    }
    index.write().unwrap();
    let (mut app, workspace) = remote_app_at(&root);
    let change = |app: &mut App, name: &str| changed(app, [workspace.target_key(name).unwrap()]);
    std::fs::write(root.join("other.txt"), "unreported\n").unwrap();
    std::fs::write(root.join("renamed.txt"), "edited after staging\n").unwrap();
    change(&mut app, "renamed.txt");
    assert!(app.source_control_files.iter().any(|file| file.path == "renamed.txt" && file.staged
        && file.status == 'R' && file.original_path.as_deref() == Some("first.txt")));
    assert!(app.source_control_files.iter().any(|file| file.path == "renamed.txt" && !file.staged && file.status == 'M'));
    assert!(!app.source_control_files.iter().any(|file| file.path == "other.txt"));
    std::fs::write(root.join("literal[1].rs"), "selected\n").unwrap();
    std::fs::write(root.join("literal1.rs"), "unreported neighbor\n").unwrap();
    change(&mut app, "literal[1].rs");
    assert!(app.source_control_files.iter().any(|file| file.path == "literal[1].rs" && !file.staged));
    assert!(!app.source_control_files.iter().any(|file| file.path == "literal1.rs" && !file.staged));
    std::fs::write(root.join("renamed.txt"), "first\n").unwrap();
    change(&mut app, "renamed.txt");
    assert!(!app.source_control_files.iter().any(|file| file.path == "renamed.txt" && !file.staged));
    full(&mut app);
    assert_eq!(key(&app.source_control_files), key(&collect_source_control_status(root).files));
}

#[test]
fn remote_directory_deltas_remove_deleted_rows_and_refresh_rules() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    init(&root);
    let (mut app, workspace) = remote_app_at(&root);
    let change = |app: &mut App, names: &[&str]| changed(app,
        names.iter().map(|name| workspace.target_key(name).unwrap()).collect::<Vec<_>>());
    let matches_full = |app: &App| {
        let normalized = |files: &[GitFileChange]| key(files).into_iter().map(|(path, original, status, staged)|
            (path.replace(std::path::MAIN_SEPARATOR, "/"), original.map(|path| path.replace(std::path::MAIN_SEPARATOR, "/")), status, staged))
            .collect::<Vec<_>>();
        assert_eq!(normalized(&app.source_control_files), normalized(&collect_source_control_status(root.clone()).files));
    };
    std::fs::create_dir_all(root.join("build/sub")).unwrap();
    std::fs::write(root.join("build/sub/界.txt"), "source\n").unwrap();
    change(&mut app, &["build"]);
    matches_full(&app);
    std::fs::rename(root.join("build"), root.join("target")).unwrap();
    change(&mut app, &["build", "target"]);
    matches_full(&app);
    std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
    change(&mut app, &[".gitignore"]);
    matches_full(&app);
    std::fs::write(root.join(".gitignore"), "").unwrap();
    change(&mut app, &[".gitignore"]);
    matches_full(&app);
    std::fs::remove_dir_all(root.join("target")).unwrap();
    change(&mut app, &["target"]);
    matches_full(&app);
}
