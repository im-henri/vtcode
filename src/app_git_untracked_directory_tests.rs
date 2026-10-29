use super::*;
use crate::background::CancellationToken;

fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(temp.path()).unwrap();
    repo.config().unwrap().set_bool("core.autocrlf", false).unwrap();
    std::fs::create_dir_all(temp.path().join("existing")).unwrap();
    std::fs::write(temp.path().join("existing/base.txt"), "base\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("existing/base.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Test", "test@example.com").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "Initial", &tree, &[]).unwrap();
    temp
}

fn write(root: &Path, path: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "new\n").unwrap();
}

fn status(root: &Path) -> Vec<GitFileChange> {
    collect_source_control_status_with_paths(root.to_owned(), || {}, &CancellationToken::new(), true,
        |path| path.to_string_lossy().into_owned()).files
}

fn directory(root: &Path, name: &str) -> GitFileChange {
    status(root).into_iter().find(|change| change.directory && Path::new(&change.path) == Path::new(name)).unwrap()
}

#[test]
fn untracked_directory_collapses_only_trees_without_indexed_files() {
    let temp = fixture();
    write(temp.path(), "fresh/deep/a.txt");
    write(temp.path(), "fresh/b.txt");
    write(temp.path(), "existing/new.txt");
    write(temp.path(), "existing/nested/new.txt");
    let files = status(temp.path());
    assert_eq!(files.len(), 3);
    assert!(files.iter().any(|change| change.directory && Path::new(&change.path) == Path::new("fresh")));
    assert!(files.iter().any(|change| !change.directory && change.path == "existing/new.txt"));
    assert!(files.iter().any(|change| change.directory && Path::new(&change.path) == Path::new("existing/nested")));
}

#[test]
fn untracked_directory_staging_produces_individual_index_entries_and_excludes_ignored_files() {
    let temp = fixture();
    write(temp.path(), "fresh/deep/a.txt");
    write(temp.path(), "fresh/b.txt");
    write(temp.path(), "fresh/keep.cache");
    std::fs::write(temp.path().join(".gitignore"), "*.cache\n").unwrap();
    changes::stage(temp.path(), &[directory(temp.path(), "fresh")]).unwrap();
    let files: Vec<_> = status(temp.path()).into_iter().filter(|change| change.staged).collect();
    assert_eq!(files.len(), 2);
    assert!(files.iter().all(|change| !change.directory && change.status == 'A'));
    assert!(files.iter().any(|change| change.path == "fresh/deep/a.txt"));
    assert!(files.iter().any(|change| change.path == "fresh/b.txt"));
    let repo = git2::Repository::open(temp.path()).unwrap();
    assert!(repo.index().unwrap().get_path(Path::new("fresh/keep.cache"), 0).is_none());
}

#[test]
fn untracked_directory_partial_staging_uncollapses_parent_and_unstaging_regroups_it() {
    let temp = fixture();
    write(temp.path(), "fresh/a.txt");
    write(temp.path(), "fresh/b.txt");
    let repo = git2::Repository::open(temp.path()).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("fresh/a.txt")).unwrap();
    index.write().unwrap();
    let files = status(temp.path());
    assert_eq!(files.len(), 2);
    assert!(files.iter().all(|change| !change.directory));
    let staged = files.into_iter().filter(|change| change.staged).collect::<Vec<_>>();
    changes::unstage(temp.path(), &staged).unwrap();
    assert_eq!(status(temp.path()).len(), 1);
    assert!(directory(temp.path(), "fresh").directory);
}

#[test]
fn untracked_directory_discard_preserves_ignored_and_newly_staged_files() {
    let temp = fixture();
    write(temp.path(), "fresh/deep/remove.txt");
    write(temp.path(), "fresh/staged.txt");
    write(temp.path(), "fresh/keep.cache");
    std::fs::write(temp.path().join(".gitignore"), "*.cache\n").unwrap();
    let selected = directory(temp.path(), "fresh");
    let repo = git2::Repository::open(temp.path()).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("fresh/staged.txt")).unwrap();
    index.write().unwrap();
    changes::discard(temp.path(), &[selected]).unwrap();
    assert!(!temp.path().join("fresh/deep").exists());
    assert!(temp.path().join("fresh/staged.txt").is_file());
    assert!(temp.path().join("fresh/keep.cache").is_file());
    assert!(repo.index().unwrap().get_path(Path::new("fresh/staged.txt"), 0).is_some());
}

#[test]
fn untracked_directory_discard_removes_empty_directory_and_handles_literal_names() {
    let temp = fixture();
    write(temp.path(), "fresh[1]/deep/a.txt");
    write(temp.path(), "fresh1/keep.txt");
    changes::discard(temp.path(), &[directory(temp.path(), "fresh[1]")]).unwrap();
    assert!(!temp.path().join("fresh[1]").exists());
    assert!(temp.path().join("fresh1/keep.txt").is_file());
}

#[test]
fn untracked_directory_watcher_updates_replace_the_group_instead_of_adding_child_rows() {
    let temp = fixture();
    write(temp.path(), "fresh/a.txt");
    let mut files = status(temp.path());
    write(temp.path(), "fresh/deep/b.txt");
    let mut discovery = crate::git_repositories::discover(temp.path(), &CancellationToken::new());
    let roots = discovery.roots.clone();
    let scans = refresh::collect(&mut discovery, roots,
        Some([temp.path().join("fresh/deep/b.txt")].into_iter().collect()),
        temp.path(), &CancellationToken::new(), true);
    assert_eq!(scans.len(), 1);
    refresh::merge_worktree(&mut files, scans[0].files.clone(), scans[0].worktree_paths.as_ref().unwrap());
    assert_eq!(files.len(), 1);
    assert!(files[0].directory);
    std::fs::remove_file(temp.path().join("fresh/a.txt")).unwrap();
    std::fs::remove_file(temp.path().join("fresh/deep/b.txt")).unwrap();
    let roots = discovery.roots.clone();
    let scans = refresh::collect(&mut discovery, roots,
        Some([temp.path().join("fresh/deep/b.txt")].into_iter().collect()),
        temp.path(), &CancellationToken::new(), true);
    refresh::merge_worktree(&mut files, scans[0].files.clone(), scans[0].worktree_paths.as_ref().unwrap());
    assert!(files.is_empty());
}

#[test]
fn untracked_directory_explorer_badges_reach_children_without_coloring_tracked_siblings() {
    let temp = fixture();
    write(temp.path(), "fresh/deep/a.txt");
    write(temp.path(), "fresh/keep.cache");
    std::fs::write(temp.path().join(".gitignore"), "*.cache\n").unwrap();
    let mut explorer = crate::explorer::FileExplorer::new(temp.path().to_owned());
    explorer.select_path(&temp.path().join("fresh/deep/a.txt"));
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let files = status(temp.path());
    explorer.apply_git_repository_changes(files.iter().map(|change|
        (root.as_path(), change.path.as_str(), change.status, change.directory)));
    assert_eq!(explorer.entries.iter().find(|entry| entry.path.ends_with("fresh/deep/a.txt")).unwrap().git_status,
        crate::explorer::GitStatus::Untracked);
    assert_eq!(explorer.entries.iter().find(|entry| entry.path.ends_with("existing")).unwrap().git_status,
        crate::explorer::GitStatus::Unmodified);
    assert_eq!(explorer.entries.iter().find(|entry| entry.path.ends_with("fresh/keep.cache")).unwrap().git_status,
        crate::explorer::GitStatus::Ignored);
}

#[test]
fn untracked_directory_metadata_survives_path_mapping_and_serialization() {
    let temp = fixture();
    write(temp.path(), "fresh/a.txt");
    let change = directory(temp.path(), "fresh").try_map_paths(|path|
        Ok::<_, std::convert::Infallible>(PathBuf::from(path))).unwrap();
    let encoded = serde_json::to_vec(&change).unwrap();
    let decoded: GitFileChange<PathBuf> = serde_json::from_slice(&encoded).unwrap();
    assert!(decoded.directory && !decoded.staged);
}

#[test]
fn untracked_directory_open_reveals_the_folder_without_opening_an_editor() {
    let temp = fixture();
    write(temp.path(), "fresh/deep/a.txt");
    let mut app = App::test_new();
    app.explorer = crate::explorer::FileExplorer::new(temp.path().to_owned());
    app.source_control_files = status(temp.path());
    let before = app.tabs.len();
    app.open_source_control_diff(0);
    assert_eq!(app.tabs.len(), before);
    assert_eq!(app.sidebar_view, crate::app::SidebarView::Explorer);
    assert!(app.explorer.entries[app.explorer.selected].path.ends_with("fresh"));
}
