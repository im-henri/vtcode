use super::*;

fn write(root: &Path, relative: &str, text: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn workspace() -> (tempfile::TempDir, git2::Repository, FileExplorer) {
    let temp = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(temp.path()).unwrap();
    // Isolate the fixture from the user's global ignore patterns.
    let excludes = temp.path().join(".git/test-excludes");
    std::fs::write(&excludes, "").unwrap();
    repo.config()
        .unwrap()
        .set_str("core.excludesFile", excludes.to_str().unwrap())
        .unwrap();
    let explorer = FileExplorer::new_deferred(temp.path().into());
    explorer.set_directory_watcher_active(true);
    (temp, repo, explorer)
}

fn refresh(explorer: &mut FileExplorer) {
    let (root, entries, matches) = FileExplorer::build_refresh_entries(explorer.refresh_request());
    explorer.apply_refresh_entries(root, entries, matches);
}

fn assert_ignored(explorer: &FileExplorer, path: &str, ignored: bool) {
    let path = explorer.root.join(path);
    let entry = explorer
        .entries
        .iter()
        .find(|entry| entry.path == path)
        .unwrap();
    assert_eq!(entry.git_status == GitStatus::Ignored, ignored, "{path:?}");
    assert_eq!(
        entry.git_status_summary.contains(GitStatus::Ignored),
        ignored,
        "{path:?}"
    );
}

#[test]
fn disabled_source_control_does_no_git_io_and_clears_cached_decorations() {
    let (temp, _repo, mut explorer) = workspace();
    write(temp.path(), ".gitignore", "*.log\n");
    write(temp.path(), "build.log", "ignored");
    write(temp.path(), "source.rs", "source");
    explorer.set_source_control_enabled(false);
    for _ in 0..3 { refresh(&mut explorer); }
    assert_eq!(explorer.ignore_cache.metrics(), super::git_cache::Metrics::default());
    assert_ignored(&explorer, "build.log", false);
    explorer.set_source_control_enabled(true);
    refresh(&mut explorer);
    assert_ignored(&explorer, "build.log", true);
    explorer.apply_git_status_changes(Some(temp.path()), [("source.rs", 'M')]);
    let late = FileExplorer::build_refresh_entries(explorer.refresh_request());
    let before = explorer.ignore_cache.metrics();
    explorer.set_source_control_enabled(false);
    explorer.apply_refresh_entries(late.0, late.1, late.2);
    explorer.apply_git_status_changes(Some(temp.path()), [("source.rs", 'M')]);
    for _ in 0..3 { refresh(&mut explorer); }
    assert_eq!(explorer.ignore_cache.metrics(), before);
    assert_ignored(&explorer, "build.log", false);
    assert!(explorer.entries.iter().all(|entry| entry.git_status_summary == Default::default()));
}

#[test]
fn ignored_files_and_folders_follow_nested_rules_and_negations() {
    let (temp, _repo, mut explorer) = workspace();
    let root = temp.path();
    write(root, ".gitignore", "*.log\nbuild/\n!build/keep.log\n");
    write(root, "ignored.log", "log");
    write(root, "visible.rs", "source");
    write(root, "build/keep.log", "still ignored: excluded parent");
    write(root, "nested/.gitignore", "!keep.log\n*.tmp\n");
    write(root, "nested/keep.log", "exception");
    write(root, "nested/ignored.log", "inherited rule");
    write(root, "nested/café.tmp", "nested rule");
    explorer.expanded_dirs.insert(root.join("nested"));
    refresh(&mut explorer);
    assert_ignored(&explorer, "ignored.log", true);
    assert_ignored(&explorer, "build", true);
    assert_ignored(&explorer, "visible.rs", false);
    assert_ignored(&explorer, "nested", false);
    assert_ignored(&explorer, "nested/keep.log", false);
    assert_ignored(&explorer, "nested/ignored.log", true);
    assert_ignored(&explorer, "nested/café.tmp", true);
    // Collapsed ignored directories do not need recursive enumeration.
    assert_eq!(explorer.directory_cache.read_count(), 2);
    assert!(
        !explorer
            .entries
            .iter()
            .any(|entry| entry.path == root.join("build/keep.log"))
    );
    explorer.expanded_dirs.insert(root.join("build"));
    refresh(&mut explorer);
    assert_ignored(&explorer, "build/keep.log", true);
    assert_eq!(explorer.directory_cache.read_count(), 3);
}

#[test]
fn tracked_files_and_their_parents_are_not_dimmed_by_ignore_rules() {
    let (temp, repo, mut explorer) = workspace();
    let root = temp.path();
    write(root, ".gitignore", "build/\ncache/\n*.log\n");
    for name in [
        "build/kept.rs",
        "build/ignored.rs",
        "cache/ignored.rs",
        "cache-sibling/kept.rs",
        "tracked.log",
    ] {
        write(root, name, "source");
    }
    let mut index = repo.index().unwrap();
    for name in ["build/kept.rs", "cache-sibling/kept.rs", "tracked.log"] {
        index.add_path(Path::new(name)).unwrap();
    }
    index.write().unwrap();
    explorer.expanded_dirs.insert(root.join("build"));
    refresh(&mut explorer);
    assert_ignored(&explorer, "build", false);
    assert_ignored(&explorer, "build/kept.rs", false);
    assert_ignored(&explorer, "build/ignored.rs", true);
    assert_ignored(&explorer, "tracked.log", false);
    // A tracked sibling with the same prefix does not make this folder tracked.
    assert_ignored(&explorer, "cache", true);
}

#[test]
fn index_and_ignore_rule_changes_refresh_without_rereading_directory_listings() {
    let (temp, repo, mut explorer) = workspace();
    let root = temp.path();
    write(root, ".gitignore", "*.log\n");
    write(root, "output.log", "log");
    write(root, "other.tmp", "temp");
    refresh(&mut explorer);
    assert_ignored(&explorer, "output.log", true);
    assert_ignored(&explorer, "other.tmp", false);
    let reads = explorer.directory_cache.read_count();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("output.log")).unwrap();
    index.write().unwrap();
    explorer.invalidate_git_decorations();
    refresh(&mut explorer);
    assert_ignored(&explorer, "output.log", false);
    index.remove_path(Path::new("output.log")).unwrap();
    index.write().unwrap();
    explorer.invalidate_git_decorations();
    refresh(&mut explorer);
    assert_ignored(&explorer, "output.log", true);
    write(root, ".gitignore", "*.tmp\n");
    explorer.invalidate_directory_paths([root.join(".gitignore").as_path()], false);
    refresh(&mut explorer);
    assert_ignored(&explorer, "output.log", false);
    assert_ignored(&explorer, "other.tmp", true);
    assert_eq!(explorer.directory_cache.read_count(), reads);
}

#[test]
fn subfolder_workspaces_and_path_aliases_use_repository_and_global_excludes() {
    let (temp, _repo, _) = workspace();
    let root = temp.path();
    write(root, ".gitignore", "nested/*.log\n!nested/keep.global\n");
    write(root, ".git/info/exclude", "*.local\n!nested/info.global\n");
    write(root, ".git/test-excludes", "*.global\n");
    for name in [
        "nested/ignored.log",
        "nested/info.local",
        "nested/user.global",
        "nested/keep.global",
        "nested/info.global",
        "nested/visible.rs",
    ] {
        write(root, name, "source");
    }
    std::fs::create_dir(root.join("nested/child")).unwrap();
    for folder in [root.join("nested"), root.join("nested/child/..")] {
        let mut explorer = FileExplorer::new_deferred(folder);
        refresh(&mut explorer);
        for name in ["ignored.log", "info.local", "user.global"] {
            assert_ignored(&explorer, name, true);
        }
        assert_ignored(&explorer, "visible.rs", false);
        assert_ignored(&explorer, "keep.global", false);
        assert_ignored(&explorer, "info.global", false);
    }
}

#[test]
fn ignore_rules_respect_repository_case_sensitivity() {
    let (temp, repo, mut explorer) = workspace();
    write(temp.path(), ".gitignore", "*.tmp\n");
    write(temp.path(), "OUTPUT.TMP", "temp");
    for case_insensitive in [false, true, false] {
        repo.config()
            .unwrap()
            .set_bool("core.ignorecase", case_insensitive)
            .unwrap();
        explorer.invalidate_git_decorations();
        refresh(&mut explorer);
        assert_ignored(&explorer, "OUTPUT.TMP", case_insensitive);
    }
}

#[test]
fn git_change_updates_preserve_ignored_state_without_dimming_ancestor_folders() {
    let (temp, _repo, mut explorer) = workspace();
    let root = temp.path();
    write(root, ".gitignore", "*.log\n");
    write(root, "nested/output.log", "log");
    write(root, "changed.rs", "source");
    explorer.expanded_dirs.insert(root.join("nested"));
    refresh(&mut explorer);
    for changes in [vec![("changed.rs", 'M')], Vec::new()] {
        explorer.apply_git_status_changes(Some(root), changes);
        assert_ignored(&explorer, "nested/output.log", true);
        assert_ignored(&explorer, "nested", false);
        assert!(
            !explorer
                .git_statuses
                .values()
                .any(|summary| summary.contains(GitStatus::Ignored))
        );
    }
}

#[test]
fn filtered_find_and_invalid_regex_preserve_ignored_decorations() {
    let (temp, _repo, mut explorer) = workspace();
    let root = temp.path();
    write(root, ".gitignore", "cache/\n");
    write(root, "cache/match.rs", "source");
    explorer.find.open = true;
    explorer.find.filter = true;
    explorer.find.query = "match".into();
    refresh(&mut explorer);
    assert_ignored(&explorer, "cache/match.rs", true);
    explorer.find.collapsed.insert(root.join("cache"));
    assert!(explorer.rebuild_filtered_entries());
    assert_ignored(&explorer, "cache", true);
    explorer.find.collapsed.clear();
    assert!(explorer.rebuild_filtered_entries());
    assert_ignored(&explorer, "cache/match.rs", true);
    explorer.find.filter = false;
    explorer.find.regex = true;
    explorer.find.query = "[".into();
    refresh(&mut explorer);
    assert!(explorer.find.results.error.is_some());
    assert_ignored(&explorer, "cache", true);
}

#[test]
fn ignore_files_outside_repositories_do_not_dim_entries() {
    let temp = tempfile::tempdir().unwrap();
    write(temp.path(), ".gitignore", "*.log\n");
    write(temp.path(), "output.log", "log");
    let mut explorer = FileExplorer::new_deferred(temp.path().into());
    refresh(&mut explorer);
    assert_ignored(&explorer, "output.log", false);
}

#[test]
fn nested_repositories_use_their_own_rules_and_indices_with_or_without_a_parent_repository() {
    for parent_repository in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let _parent = parent_repository.then(|| git2::Repository::init(root).unwrap());
        write(root, ".gitignore", "*.outer\n*.tmp\n");
        let nested = root.join("project");
        let repository = git2::Repository::init(&nested).unwrap();
        write(root, "project/.gitignore", "*.log\n*.tmp\n");
        write(root, "project/ignored.log", "ignored");
        write(root, "project/tracked.tmp", "tracked");
        write(root, "project/visible.outer", "parent rules must not cross repository boundary");
        let mut index = repository.index().unwrap();
        index.add_path(Path::new("tracked.tmp")).unwrap();
        index.write().unwrap();
        let mut explorer = FileExplorer::new_deferred(root.into());
        explorer.set_directory_watcher_active(true);
        explorer.expanded_dirs.insert(nested);
        refresh(&mut explorer);
        assert_ignored(&explorer, "project/ignored.log", true);
        assert_ignored(&explorer, "project/tracked.tmp", false);
        assert_ignored(&explorer, "project/visible.outer", false);
        let before = explorer.ignore_cache.metrics();
        for _ in 0..16 { refresh(&mut explorer); }
        let after = explorer.ignore_cache.metrics();
        assert_eq!(after.repository_loads, before.repository_loads);
        assert_eq!(after.rule_loads, before.rule_loads);
        assert_eq!(after.index_loads, before.index_loads);
        assert_eq!(after.boundary_checks, before.boundary_checks, "warm redraw/refresh does no filesystem boundary probes");
    }
}

#[test]
#[ignore = "local Explorer ignore-decoration performance comparison"]
fn benchmark_ignore_decorations() {
    let (temp, repo, mut explorer) = workspace();
    let root = temp.path();
    write(root, ".gitignore", "*.tmp\n");
    write(root, "tracked.rs", "source");
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("tracked.rs")).unwrap();
    for i in 0..20_000 {
        let mut entry = index.get(0).unwrap();
        entry.path = format!("tracked-{i:05}.rs").into_bytes();
        index.add(&entry).unwrap();
    }
    index.write().unwrap();
    let patterns = (0..64)
        .map(|i| format!("ignored-{i}-*\n"))
        .collect::<String>();
    for dir in 0..16 {
        let folder = format!("folder-{dir}");
        write(root, &format!("{folder}/.gitignore"), &patterns);
        explorer.expanded_dirs.insert(root.join(&folder));
        for file in 0..128 {
            let ext = if file % 2 == 0 { "tmp" } else { "rs" };
            write(root, &format!("{folder}/file-{file}.{ext}"), "source");
        }
    }
    refresh(&mut explorer);
    let canonical = std::fs::canonicalize(root).unwrap();
    let mut times = Vec::new();
    for _ in 0..21 {
        let start = std::time::Instant::now();
        explorer
            .ignore_cache
            .mark_ignored(root, &canonical, &mut explorer.entries, None);
        times.push(start.elapsed());
        std::hint::black_box(&explorer.entries);
    }
    times.sort();
    eprintln!(
        "ignore decorations: {} entries, 20,001 index entries, median {:?}, min {:?}",
        explorer.entries.len(),
        times[times.len() / 2],
        times[0]
    );
}

fn assert_no_ignore_work(before: git_cache::Metrics, after: git_cache::Metrics) {
    assert_eq!(after.boundary_checks, before.boundary_checks, "cached directories reprobed");
    assert_eq!(
        after.repository_loads, before.repository_loads,
        "repository reopened"
    );
    assert_eq!(
        after.rule_loads, before.rule_loads,
        "rules reread/recompiled"
    );
    assert_eq!(after.index_loads, before.index_loads, "Git index reread");
    assert_eq!(after.matches, before.matches, "cached paths rematched");
}

#[test]
fn warm_refresh_sort_and_file_events_reuse_ignore_results_without_io() {
    let (temp, _repo, mut explorer) = workspace();
    let root = temp.path();
    write(root, ".gitignore", "*.tmp\n");
    write(root, "source.rs", "source");
    write(root, "ignored.tmp", "temporary");
    refresh(&mut explorer);
    let before = explorer.ignore_cache.metrics();
    for sort in [
        ExplorerSortMode::Name,
        ExplorerSortMode::Type,
        ExplorerSortMode::Size,
    ] {
        explorer.sort_mode = sort;
        refresh(&mut explorer);
        assert_ignored(&explorer, "ignored.tmp", true);
    }
    for structural in [false, true] {
        write(root, "ignored.tmp", "replacement");
        explorer.invalidate_directory_paths([root.join("ignored.tmp").as_path()], structural);
        refresh(&mut explorer);
        assert_ignored(&explorer, "ignored.tmp", true);
    }
    let after = explorer.ignore_cache.metrics();
    assert_no_ignore_work(before, after);
    assert!(after.hits > before.hits);
}

#[test]
fn expansion_only_loads_new_rules_and_index_when_an_ignored_file_needs_it() {
    let (temp, _repo, mut explorer) = workspace();
    let root = temp.path();
    write(root, ".gitignore", "*.tmp\n");
    write(root, "nested/.gitignore", "!keep.tmp\n");
    write(root, "nested/keep.tmp", "keep");
    write(root, "nested/ignored.tmp", "ignored");
    refresh(&mut explorer);
    let before = explorer.ignore_cache.metrics();
    assert_eq!(
        before.index_loads, 0,
        "collapsed folders and .git need no index read"
    );
    explorer.expanded_dirs.insert(root.join("nested"));
    refresh(&mut explorer);
    assert_ignored(&explorer, "nested/keep.tmp", false);
    assert_ignored(&explorer, "nested/ignored.tmp", true);
    let expanded = explorer.ignore_cache.metrics();
    assert_eq!(expanded.repository_loads, before.repository_loads);
    assert_eq!(expanded.rule_loads, before.rule_loads + 1);
    assert_eq!(expanded.index_loads, 1);
    assert_eq!(expanded.matches, before.matches + 3);
    explorer.expanded_dirs.clear();
    refresh(&mut explorer);
    explorer.expanded_dirs.insert(root.join("nested"));
    refresh(&mut explorer);
    assert_no_ignore_work(expanded, explorer.ignore_cache.metrics());
}

#[test]
fn nested_rule_edits_invalidate_only_the_affected_subtree() {
    let (temp, _repo, mut explorer) = workspace();
    let root = temp.path();
    for dir in ["one", "two"] {
        write(root, &format!("{dir}/.gitignore"), "*.tmp\n");
        write(root, &format!("{dir}/output.tmp"), "ignored");
        explorer.expanded_dirs.insert(root.join(dir));
    }
    refresh(&mut explorer);
    let before = explorer.ignore_cache.metrics();
    write(root, "one/.gitignore", "");
    explorer.invalidate_directory_paths([root.join("one/.gitignore").as_path()], false);
    refresh(&mut explorer);
    assert_ignored(&explorer, "one/output.tmp", false);
    assert_ignored(&explorer, "two/output.tmp", true);
    let after = explorer.ignore_cache.metrics();
    assert_eq!(after.repository_loads, before.repository_loads);
    assert_eq!(after.rule_loads, before.rule_loads + 1);
    assert_eq!(after.matches, before.matches + 3);
    assert_eq!(
        after.index_loads, before.index_loads,
        "cached ignored sibling reuses its tracked check"
    );
}

#[test]
fn folder_replacement_and_canonical_events_drop_nested_rules() {
    let (temp, _repo, _) = workspace();
    let root = temp.path();
    write(root, "nested/.gitignore", "*.tmp\n");
    write(root, "nested/output.tmp", "ignored");
    std::fs::create_dir(root.join("alias")).unwrap();
    let mut explorer = FileExplorer::new_deferred(root.join("alias/.."));
    explorer.set_directory_watcher_active(true);
    explorer.expanded_dirs.insert(explorer.root.join("nested"));
    refresh(&mut explorer);
    assert_ignored(&explorer, "nested/output.tmp", true);
    // A directory replacement can arrive as one structural event without a
    // separate notification for the removed nested .gitignore.
    std::fs::remove_file(root.join("nested/.gitignore")).unwrap();
    let canonical = std::fs::canonicalize(root.join("nested")).unwrap();
    explorer.invalidate_directory_paths([canonical.as_path()], true);
    refresh(&mut explorer);
    assert_ignored(&explorer, "nested/output.tmp", false);
}

#[test]
fn git_config_excludes_expiry_and_manual_refresh_revalidate_cached_rules() {
    let (temp, _repo, mut explorer) = workspace();
    let root = temp.path();
    write(root, "output.tmp", "source");
    refresh(&mut explorer);
    assert_ignored(&explorer, "output.tmp", false);
    write(root, ".git/info/exclude", "*.tmp\n");
    explorer.invalidate_git_decorations();
    refresh(&mut explorer);
    assert_ignored(&explorer, "output.tmp", true);
    write(root, ".git/info/exclude", "");
    write(root, ".git/test-excludes", "*.tmp\n");
    explorer.ignore_cache.expire(); // Missed global/shared-filesystem event.
    refresh(&mut explorer);
    assert_ignored(&explorer, "output.tmp", true);
    write(root, ".git/test-excludes", "");
    explorer.refresh();
    assert_ignored(&explorer, "output.tmp", false);
    assert_eq!(explorer.ignore_cache.metrics().repository_loads, 4);
}

#[test]
fn failed_repository_discovery_is_cached_until_invalidated() {
    let temp = tempfile::tempdir().unwrap();
    write(temp.path(), ".gitignore", "*.tmp\n");
    write(temp.path(), "output.tmp", "source");
    let mut explorer = FileExplorer::new_deferred(temp.path().into());
    explorer.set_directory_watcher_active(true);
    refresh(&mut explorer);
    refresh(&mut explorer);
    assert_eq!(explorer.ignore_cache.metrics().repository_loads, 1);
    assert_ignored(&explorer, "output.tmp", false);
    let _repo = git2::Repository::init(temp.path()).unwrap();
    explorer.invalidate_git_decorations();
    refresh(&mut explorer);
    assert_ignored(&explorer, "output.tmp", true);
    assert_eq!(explorer.ignore_cache.metrics().repository_loads, 2);
}

#[test]
fn filtered_find_decorates_retained_results_once_before_cloning_visible_rows() {
    let (temp, _repo, mut explorer) = workspace();
    write(temp.path(), ".gitignore", "*.tmp\n");
    write(temp.path(), "nested/match.tmp", "ignored");
    write(temp.path(), "nested/match.rs", "source");
    explorer.find.open = true;
    explorer.find.filter = true;
    explorer.find.query = "match".into();
    refresh(&mut explorer);
    let before = explorer.ignore_cache.metrics();
    let retained = explorer.find.results.filtered_tree.as_ref().unwrap();
    assert_eq!(before.matches, retained.len());
    assert_eq!(
        before.hits, 0,
        "visible results must not be decorated a second time"
    );
    assert_ignored(&explorer, "nested/match.tmp", true);
    explorer.find.query = "match.tmp".into();
    refresh(&mut explorer);
    assert_no_ignore_work(before, explorer.ignore_cache.metrics());
}

#[test]
fn empty_and_cancelled_ignore_batches_do_not_discover_or_read_git() {
    let (temp, _repo, mut explorer) = workspace();
    let cancel = crate::background::CancellationToken::new();
    explorer
        .ignore_cache
        .mark_ignored(temp.path(), temp.path(), &mut [], None);
    write(temp.path(), "file.rs", "source");
    explorer.scan_dir(temp.path(), 0, false, None);
    assert!(!explorer.entries.is_empty());
    cancel.cancel();
    explorer.ignore_cache.mark_ignored(
        temp.path(),
        temp.path(),
        &mut explorer.entries,
        Some(&cancel),
    );
    assert_eq!(
        explorer.ignore_cache.metrics(),
        git_cache::Metrics::default()
    );
}
