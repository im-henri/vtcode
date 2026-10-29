use super::*;
use notify::{RecursiveMode, Watcher};
use std::time::{Duration, Instant};

#[test]
fn external_directory_links_are_not_workspace_subscriptions_and_defaults_still_follow_them() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_file = outside.path().join("activity");
    std::fs::write(&outside_file, "outside").unwrap();
    let link = root.path().join("linked");
    std::os::unix::fs::symlink(outside.path(), &link).unwrap();
    for follow in [true, false] {
        let (send, receive) = mpsc::channel();
        let config = notify::Config::default().with_open_events(false).with_follow_symlinks(follow);
        let mut watcher = notify::INotifyWatcher::new(move |event| { let _ = send.send(event); }, config).unwrap();
        watcher.watch(root.path(), RecursiveMode::Recursive).unwrap();
        std::fs::write(&outside_file, "activity").unwrap();
        if follow {
            let event: notify::Result<notify::Event> = receive.recv_timeout(Duration::from_secs(3)).unwrap();
            assert!(event.unwrap().paths.contains(&link.join("activity")));
        } else {
            assert!(receive.recv_timeout(Duration::from_millis(200)).is_err());
            let inside = root.path().join("inside");
            std::fs::write(&inside, "source").unwrap();
            let event: notify::Result<notify::Event> = receive.recv_timeout(Duration::from_secs(3)).unwrap();
            assert!(event.unwrap().paths.contains(&inside));
        }
    }
}

fn next_git(receive: &mpsc::Receiver<Response>) {
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        let response = receive.recv_timeout(deadline.saturating_duration_since(Instant::now())).unwrap();
        if let Ok(Reply::WorkspaceChanges(batch)) = response.result {
            assert_ne!(batch.active, Some(false));
            if batch.git { return; }
        }
    }
}

#[test]
fn external_git_directories_remain_watched_for_gitfiles_and_directory_links() {
    for directory_link in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        drop(git2::Repository::init(root.path()).unwrap());
        let metadata = outside.path().join("metadata");
        std::fs::rename(root.path().join(".git"), &metadata).unwrap();
        if directory_link {
            std::os::unix::fs::symlink(&metadata, root.path().join(".git")).unwrap();
        } else {
            std::fs::write(root.path().join(".git"), format!("gitdir: {}\n", metadata.display())).unwrap();
        }
        let observation = Arc::new(Observation::default());
        observation.repositories(root.path(), &git::Discovery { errors: Vec::new(), roots: vec![RemotePath::default()], parent: None }, false, &[]);
        let (send, receive) = mpsc::sync_channel(32);
        let _watch = Watch::start(root.path().to_owned(), send, observation).unwrap();
        next_git(&receive);
        std::fs::write(metadata.join("HEAD"), "ref: refs/heads/changed\n").unwrap();
        next_git(&receive);
        std::fs::write(metadata.join("objects/object"), "private").unwrap();
        if let Ok(response) = receive.recv_timeout(Duration::from_millis(600)) {
            panic!("unexpected Git object notification (directory_link={directory_link}): {}",
                serde_json::to_string(&response).unwrap());
        }
    }
}

#[test]
fn external_git_scope_preserves_project_events_inside_an_ancestor_metadata_directory() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let mut burst = Burst::default();
    burst.push_with_dependencies(root.path(),
        Ok(notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Any))).add_path(source.clone())),
        &[root.path().parent().unwrap().to_owned()]);
    assert_eq!(burst.event.paths, vec![source]);
    assert!(!burst.event.git_metadata_changed);
}

#[test]
fn removing_an_external_git_subscription_root_reports_lost_coverage() {
    let root = tempfile::tempdir().unwrap();
    let metadata = tempfile::tempdir().unwrap();
    let mut burst = Burst::default();
    burst.push_with_dependencies(root.path(),
        Ok(notify::Event::new(notify::EventKind::Remove(notify::event::RemoveKind::Folder))
            .add_path(metadata.path().to_owned())), &[metadata.path().to_owned()]);
    assert!(burst.event.rescan);
    assert!(burst.event.watcher_failed);
}

#[test]
fn narrowing_and_widening_external_git_subscriptions_keep_child_watches_alive() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let metadata = outside.path().join("metadata");
    let child = metadata.join("worktrees/one");
    std::fs::create_dir_all(&child).unwrap();
    let head = child.join("HEAD");
    std::fs::write(&head, "ref: refs/heads/initial\n").unwrap();
    let observation = Arc::new(Observation::default());
    *observation.git.lock().unwrap() = vec![metadata.clone()];
    let (send, receive) = mpsc::sync_channel(32);
    let _watch = Watch::start(root.path().to_owned(), send, observation.clone()).unwrap();
    next_git(&receive);
    for (scope, branch) in [(child, "narrow"), (metadata, "wide")] {
        *observation.git.lock().unwrap() = vec![scope];
        observation.wake.lock().unwrap().as_ref().unwrap().try_send(()).unwrap();
        next_git(&receive);
        std::fs::write(&head, format!("ref: refs/heads/{branch}\n")).unwrap();
        next_git(&receive);
    }
}

#[test]
fn queued_events_from_retired_git_roots_do_not_become_project_changes() {
    let root = tempfile::tempdir().unwrap();
    let retired = tempfile::tempdir().unwrap();
    let mut burst = Burst::default();
    burst.push_with_dependencies(root.path(),
        Ok(notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Any))).add_path(retired.path().join("HEAD"))), &[]);
    assert!(burst.event.is_empty());
}

#[test]
fn explicit_git_roots_inside_pruned_storage_reconcile_existing_parent_watches() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let metadata = outside.path().join("metadata");
    let child = metadata.join("objects/another-git-directory");
    std::fs::create_dir_all(&child).unwrap();
    let head = child.join("HEAD");
    std::fs::write(&head, "ref: refs/heads/initial\n").unwrap();
    let observation = Arc::new(Observation::default());
    *observation.git.lock().unwrap() = vec![metadata.clone()];
    let (send, receive) = mpsc::sync_channel(32);
    let _watch = Watch::start(root.path().to_owned(), send, observation.clone()).unwrap();
    next_git(&receive);
    *observation.git.lock().unwrap() = vec![metadata.clone(), child];
    observation.wake.lock().unwrap().as_ref().unwrap().try_send(()).unwrap();
    next_git(&receive);
    std::fs::write(&head, "ref: refs/heads/changed\n").unwrap();
    next_git(&receive);
    *observation.git.lock().unwrap() = vec![metadata];
    observation.wake.lock().unwrap().as_ref().unwrap().try_send(()).unwrap();
    next_git(&receive);
    std::fs::write(&head, "ref: refs/heads/retired\n").unwrap();
    assert!(receive.recv_timeout(Duration::from_millis(600)).is_err());
}
