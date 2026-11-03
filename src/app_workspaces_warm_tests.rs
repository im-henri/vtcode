use super::*;
use crate::remote::{self, Command, RemotePath};
#[path = "app_workspaces_runtime_tests.rs"]
mod runtime_tests;

fn settle(app: &mut App) {
    let mut idle = Instant::now();
    until(|| {
        services(app);
        app.poll_git_watcher_refresh(Instant::now());
        app.poll_completion_index();
        app.background.refresh_activity(Instant::now());
        if app.background.has_active_jobs()
            || app.workspaces_have_pending_results()
            || app.git_watcher_refresh_wait(Instant::now()).is_some()
            || app.bg_git_log_running
            || app.bg_explorer_refresh_running
        {
            idle = Instant::now();
        }
        idle.elapsed() > Duration::from_millis(450)
    });
}

fn warm_app(project: &Project, wake: crate::background::UiWake) -> App {
    let mut app = project.app(wake);
    // Asset discovery has a separate first-visit lifecycle. These tests measure
    // warm workspace observation, without depending on installed user themes.
    app.workspace_observation.assets_started = true;
    app.source_control_enabled = true;
    app.start_workspace_watcher();
    settle(&mut app);
    app
}

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    explorer: u64,
    git: Option<Instant>,
    history: u64,
    jobs: u64,
    index: u64,
    document: u64,
}
fn snapshot(app: &App) -> Snapshot {
    Snapshot {
        explorer: app.explorer_refresh_generation,
        git: app.source_control_last_refresh,
        history: app.git_history.generation,
        jobs: app.background.activity.totals.completed,
        index: app.completion_index.revision(),
        document: app.editor().unwrap().content_version,
    }
}

#[test]
fn unchanged_native_workspaces_reuse_git_explorer_documents_and_index() {
    let a = Project::new();
    let b = Project::new();
    for project in [&a, &b] {
        git2::Repository::init(project.path()).unwrap();
    }
    let wake = crate::background::UiWake::default();
    let mut app = warm_app(&a, wake.clone());
    let next = warm_app(&b, wake);
    let first = snapshot(&app);
    let second = snapshot(&next);
    app.insert_workspace(next);
    for _ in 0..25 {
        app.switch_workspace(0);
        services(&mut app);
        app.poll_completion_index();
        assert_eq!(snapshot(&app), first);
        app.switch_workspace(1);
        services(&mut app);
        app.poll_completion_index();
        assert_eq!(snapshot(&app), second);
    }
    settle(&mut app);
    assert_eq!(snapshot(&app), second, "no delayed refresh jobs either");
    app.switch_workspace(0);
    settle(&mut app);
    assert_eq!(snapshot(&app), first);
}

#[test]
fn hidden_file_changes_do_not_restart_git_history_or_reload_other_documents() {
    let a = Project::new();
    let b = Project::new();
    git2::Repository::init(a.path()).unwrap();
    let touched = a.path().join("touched.txt");
    let untouched = a.path().join("untouched.txt");
    for path in [&touched, &untouched] {
        std::fs::write(path, "original").unwrap();
    }
    let wake = crate::background::UiWake::default();
    let mut app = warm_app(&a, wake.clone());
    app.tabs.clear();
    for path in [&touched, &untouched] {
        let mut editor = Editor::new();
        editor.open_file(path).unwrap();
        app.tabs.push(Tab::new_file(editor));
    }
    settle(&mut app);
    let history = app.git_history.generation;
    let version = app.tabs[1].editor.content_version;
    let last_job = app
        .background
        .activity
        .recent
        .iter()
        .map(|job| job.id)
        .max()
        .unwrap_or(0);
    app.insert_workspace(warm_app(&b, wake));
    std::fs::write(&touched, "new contents while hidden").unwrap();
    app.switch_workspace(0);
    until(|| {
        services(&mut app);
        app.editor().unwrap().rope.to_string() == "new contents while hidden"
    });
    settle(&mut app);
    assert_eq!(app.git_history.generation, history);
    assert_eq!(app.tabs[1].editor.content_version, version);
    assert!(
        !app.background
            .activity
            .recent
            .iter()
            .any(|job| job.id > last_job && job.path.as_deref() == Some(untouched.as_path()))
    );
}

#[test]
fn unchanged_headless_workspaces_preserve_cached_state_across_rapid_switches() {
    let a = Project::new();
    let b = Project::new();
    for project in [&a, &b] {
        git2::Repository::init(project.path()).unwrap();
        std::fs::write(project.path().join("file.txt"), "original").unwrap();
    }
    let first = remote::workspace::Workspace::new(headless_connection(a.path()));
    let second = first
        .open_workspace(&first.view_key(&remote::git::from_native(b.path(), a.path()).unwrap()))
        .unwrap();
    let wake = crate::background::UiWake::default();
    first.client.set_wake(wake.clone());
    let make_app = |remote: &remote::workspace::Workspace| {
        let mut app = App::new_startup(remote.view_root.clone(), wake.clone());
        app.remote_workspace = Some(remote.clone());
        app.lsp_manager.remote_workspace = Some(remote.clone());
        app.lsp_manager.configs.clear();
        app.workspace_observation.assets_started = true;
        app.source_control_enabled = true;
        app.start_workspace_watcher();
        app.open_file(&remote.view_key(&RemotePath::new(b"file.txt".to_vec()).unwrap()));
        settle(&mut app);
        app
    };
    let mut app = make_app(&first);
    let next = make_app(&second);
    let before_first = snapshot(&app);
    let before_second = snapshot(&next);
    app.insert_workspace(next);
    for _ in 0..25 {
        app.switch_workspace(0);
        services(&mut app);
        assert_eq!(snapshot(&app), before_first);
        app.switch_workspace(1);
        services(&mut app);
        assert_eq!(snapshot(&app), before_second);
    }
    settle(&mut app);
    assert_eq!(snapshot(&app), before_second);
    app.switch_workspace(0);
    settle(&mut app);
    assert_eq!(snapshot(&app), before_first);
    assert!(first.client.connected());
    first.disconnect();
}

#[test]
fn headless_resume_delivers_only_the_changed_paths_and_no_idle_rescan() {
    let project = Project::new();
    std::fs::write(project.path().join("file.txt"), "original").unwrap();
    let remote = remote::workspace::Workspace::new(headless_connection(project.path()));
    remote.client.call(Command::Watch).unwrap();
    until(|| {
        remote
            .client
            .take_workspace_changes()
            .is_some_and(|batch| batch.active == Some(true))
    });
    remote.client.call(Command::PauseWatch).unwrap();
    let wake = crate::background::UiWake::default();
    remote.client.set_wake(wake.clone());
    let observed = wake.generation();
    std::fs::write(project.path().join("file.txt"), "changed").unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(wake.generation(), observed);
    assert!(remote.client.take_workspace_changes().is_none());
    remote.client.call(Command::Watch).unwrap();
    until(|| {
        let Some(batch) = remote.client.take_workspace_changes() else {
            return false;
        };
        assert!(!batch.rescan && !batch.git && !batch.structural);
        batch.active == Some(true)
            && batch
                .states
                .iter()
                .any(|state| state.path == RemotePath::new(b"file.txt".to_vec()).unwrap())
    });
    remote.client.call(Command::PauseWatch).unwrap();
    remote.client.call(Command::Watch).unwrap();
    until(|| {
        let Some(batch) = remote.client.take_workspace_changes() else {
            return false;
        };
        assert!(!batch.rescan && !batch.git && !batch.structural && batch.states.is_empty());
        batch.active == Some(true)
    });
    remote.disconnect();
}

#[test]
fn hidden_linked_worktree_head_changes_are_observed_outside_the_workspace() {
    let store = Project::new();
    let linked = Project::new();
    let other = Project::new();
    let repo = git2::Repository::init(store.path()).unwrap();
    let signature = git2::Signature::now("VTCode Test", "test@example.com").unwrap();
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    let id = repo
        .commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
        .unwrap();
    repo.branch("alternate", &repo.find_commit(id).unwrap(), false)
        .unwrap();
    std::fs::remove_dir(linked.path()).unwrap();
    repo.worktree("linked", linked.path(), None).unwrap();
    let linked_repo = git2::Repository::open(linked.path()).unwrap();
    assert!(!linked_repo.path().starts_with(linked.path()));
    let wake = crate::background::UiWake::default();
    let mut app = warm_app(&linked, wake.clone());
    assert!(app.cached_git_branch.contains("linked"));
    app.insert_workspace(warm_app(&other, wake.clone()));
    let observed = wake.generation();
    linked_repo.set_head("refs/heads/alternate").unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        wake.generation(),
        observed,
        "hidden Git writes must not wake the UI"
    );
    app.switch_workspace(0);
    until(|| {
        services(&mut app);
        app.poll_git_watcher_refresh(Instant::now());
        app.cached_git_branch.contains("alternate")
    });
}

#[test]
fn replacing_a_hidden_workspace_root_reestablishes_watch_coverage() {
    for headless in [false, true] {
        for rename in [false, true] {
            let a = Project::new();
            let b = Project::new();
            let retired = Project::new();
            let wake = crate::background::UiWake::default();
            let mut app = if headless {
                let remote = remote::workspace::Workspace::new(headless_connection(a.path()));
                let mut app = App::new_startup(remote.view_root.clone(), wake.clone());
                app.remote_workspace = Some(remote);
                app.source_control_enabled = false;
                app.lsp_manager.configs.clear();
                app.workspace_observation.assets_started = true;
                app.start_workspace_watcher();
                settle(&mut app);
                app
            } else {
                warm_app(&a, wake.clone())
            };
            app.insert_workspace(warm_app(&b, wake));
            if let Some(remote) = &app.workspaces.parked[0].app.remote_workspace {
                remote.client.call(Command::PauseWatch).unwrap();
            }
            if rename {
                std::fs::rename(a.path(), retired.path().join("previous")).unwrap();
            } else {
                std::fs::remove_dir(a.path()).unwrap();
            }
            std::thread::sleep(Duration::from_millis(100));
            std::fs::create_dir(a.path()).unwrap();
            app.switch_workspace(0);
            settle(&mut app);
            assert!(app.workspace_watcher_active);
            std::fs::write(a.path().join("after-replacement.txt"), "new").unwrap();
            until(|| {
                services(&mut app);
                app.explorer
                    .entries
                    .iter()
                    .any(|entry| entry.name == "after-replacement.txt")
            });
            if let Some(remote) = &app.remote_workspace {
                remote.disconnect();
            }
        }
    }
}

#[test]
fn replacing_external_git_metadata_while_hidden_invalidates_the_cached_branch() {
    for headless in [false, true] {
        for repoint in [false, true] {
            let workspace = Project::new();
            let metadata = Project::new();
            let replacement = Project::new();
            let other = Project::new();
            let init = |root: &Path, branch: &str| {
                let repo = git2::Repository::init(root).unwrap();
                repo.set_head(&format!("refs/heads/{branch}")).unwrap();
                let signature = git2::Signature::now("VTCode Test", "test@example.com").unwrap();
                let tree = repo
                    .find_tree(repo.index().unwrap().write_tree().unwrap())
                    .unwrap();
                repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
                    .unwrap();
            };
            init(workspace.path(), "initial");
            git2::Repository::open(workspace.path())
                .unwrap()
                .config()
                .unwrap()
                .set_str("core.worktree", &workspace.path().to_string_lossy())
                .unwrap();
            let store = metadata.path().join("store");
            std::fs::rename(workspace.path().join(".git"), &store).unwrap();
            std::fs::write(
                workspace.path().join(".git"),
                format!("gitdir: {}\n", store.display()),
            )
            .unwrap();
            init(replacement.path(), "alternate");
            git2::Repository::open(replacement.path())
                .unwrap()
                .config()
                .unwrap()
                .set_str("core.worktree", &workspace.path().to_string_lossy())
                .unwrap();
            let initial = git2::Repository::open(workspace.path()).unwrap();
            assert_eq!(
                std::fs::canonicalize(initial.workdir().unwrap()).unwrap(),
                std::fs::canonicalize(workspace.path()).unwrap()
            );
            assert_eq!(initial.head().unwrap().shorthand(), Some("initial"));
            drop(initial);
            let wake = crate::background::UiWake::default();
            let mut app = if headless {
                let remote =
                    remote::workspace::Workspace::new(headless_connection(workspace.path()));
                let mut app = App::new_startup(remote.view_root.clone(), wake.clone());
                app.remote_workspace = Some(remote);
                app.lsp_manager.configs.clear();
                app.workspace_observation.assets_started = true;
                app.start_workspace_watcher();
                settle(&mut app);
                app
            } else {
                warm_app(&workspace, wake.clone())
            };
            assert!(
                app.cached_git_branch.contains("initial"),
                "headless={headless}, branch={}",
                app.cached_git_branch
            );
            app.insert_workspace(warm_app(&other, wake));
            if let Some(remote) = &app.workspaces.parked[0].app.remote_workspace {
                remote.client.call(Command::PauseWatch).unwrap();
            }
            let next_store = if repoint {
                metadata.path().join("other-store")
            } else {
                std::fs::rename(&store, metadata.path().join("retired")).unwrap();
                store.clone()
            };
            std::fs::rename(replacement.path().join(".git"), &next_store).unwrap();
            if repoint {
                std::fs::write(
                    workspace.path().join(".git"),
                    format!("gitdir: {}\n", next_store.display()),
                )
                .unwrap();
            }
            assert_eq!(
                git2::Repository::open(workspace.path())
                    .unwrap()
                    .head()
                    .unwrap()
                    .shorthand(),
                Some("alternate")
            );
            std::thread::sleep(Duration::from_millis(100));
            app.switch_workspace(0);
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                services(&mut app);
                app.poll_git_watcher_refresh(Instant::now());
                if app.cached_git_branch.contains("alternate") {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "headless={headless}, branch={}, watched={}, repos={:?}, errors={}, status={}, pending={}, jobs={}",
                    app.cached_git_branch,
                    app.workspace_watcher_active,
                    app.repositories
                        .entries
                        .iter()
                        .map(|repo| &repo.root)
                        .collect::<Vec<_>>(),
                    app.repositories.errors.len(),
                    app.bg_git_status_running,
                    app.bg_git_status_pending,
                    app.background.active_job_count()
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            settle(&mut app);
            app.switch_workspace(1);
            if let Some(remote) = &app.workspaces.parked[0].app.remote_workspace {
                remote.client.call(Command::PauseWatch).unwrap();
            }
            {
                let repo = git2::Repository::open(workspace.path()).unwrap();
                repo.branch(
                    "third",
                    &repo.head().unwrap().peel_to_commit().unwrap(),
                    false,
                )
                .unwrap();
                repo.set_head("refs/heads/third").unwrap();
            }
            app.switch_workspace(0);
            until(|| {
                services(&mut app);
                app.poll_git_watcher_refresh(Instant::now());
                app.cached_git_branch.contains("third")
            });
            if let Some(remote) = &app.remote_workspace {
                remote.disconnect();
            }
        }
    }
}
