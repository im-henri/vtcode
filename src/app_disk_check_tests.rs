use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

struct Release(Arc<AtomicBool>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "Background disk check did not finish"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn remote_disk_checks_coalesce_blocked_bursts_without_workers_or_redraws_per_path() {
    let root = tempfile::tempdir().unwrap();
    let hold = Arc::new(AtomicBool::new(false));
    let _release = Release(hold.clone());
    let blocked = Arc::new(AtomicBool::new(false));
    let traffic = Arc::new(AtomicUsize::new(0));
    let client = crate::remote::tests::connection_with_status(
        root.path(),
        hold.clone(),
        traffic.clone(),
        blocked.clone(),
    );
    let remote = crate::remote::workspace::Workspace::new(client);
    let paths: Vec<_> = (0..137)
        .map(|index| {
            remote.view_key(
                &crate::remote::RemotePath::new(format!("file-{index}.rs").into_bytes()).unwrap(),
            )
        })
        .collect();
    let mut app = App::new_deferred(remote.view_root.clone());
    app.remote_workspace = Some(remote);
    hold.store(true, Ordering::Release);
    let baseline_jobs = app.background.active_job_count();
    app.queue_file_disk_checks([paths[0].clone()]);
    until(|| blocked.load(Ordering::Acquire));
    let first = app.remote_disk_checks.running.clone().unwrap();
    let first_traffic = traffic.load(Ordering::Relaxed);
    for _ in 0..1000 {
        app.queue_file_disk_checks(paths.iter().cloned());
    }
    assert_eq!(app.pending_disk_checks.len(), paths.len());
    assert_eq!(app.remote_disk_checks.queued.len(), paths.len() - 1);
    assert_eq!(app.background.active_job_count(), baseline_jobs + 1);
    assert!(first.same_task(app.remote_disk_checks.running.as_ref().unwrap()));
    assert_eq!(
        traffic.load(Ordering::Relaxed),
        first_traffic,
        "No waiting RPCs may accumulate"
    );
    hold.store(false, Ordering::Release);
    until(|| {
        assert!(
            !app.poll_background_results(),
            "Unchanged metadata must not request a redraw"
        );
        app.remote_disk_checks.running.is_none()
    });
    assert!(app.remote_disk_checks.queued.is_empty());
    assert!(app.pending_disk_checks.is_empty());
}

#[test]
fn cancelled_remote_disk_checks_cannot_finish_a_new_workspace_job() {
    let root = tempfile::tempdir().unwrap();
    let hold = Arc::new(AtomicBool::new(false));
    let _release = Release(hold.clone());
    let blocked = Arc::new(AtomicBool::new(false));
    let remote =
        crate::remote::workspace::Workspace::new(crate::remote::tests::connection_with_status(
            root.path(),
            hold.clone(),
            Arc::default(),
            blocked.clone(),
        ));
    let path = remote.view_key(&crate::remote::RemotePath::new(b"file.rs".to_vec()).unwrap());
    let mut app = App::new_deferred(remote.view_root.clone());
    app.remote_workspace = Some(remote);
    hold.store(true, Ordering::Release);
    app.queue_file_disk_checks([path.clone()]);
    until(|| blocked.load(Ordering::Acquire));
    let first = app.remote_disk_checks.running.clone().unwrap();
    app.workspace_generation += 1;
    app.cancel_remote_disk_checks();
    assert!(first.is_cancelled());
    assert!(app.pending_disk_checks.is_empty());
    app.queue_file_disk_checks([path]);
    let current = app.remote_disk_checks.running.clone().unwrap();
    assert!(!first.same_task(&current));
    // Even a completion with the current generation must match the job token.
    app.background
        .sender
        .send(crate::background::BackgroundResult::RemoteDiskChecksDone(
            first,
        ))
        .unwrap();
    assert!(!app.poll_background_results());
    assert!(current.same_task(app.remote_disk_checks.running.as_ref().unwrap()));
    assert_eq!(app.pending_disk_checks.len(), 1);
    hold.store(false, Ordering::Release);
    until(|| {
        app.poll_background_results();
        app.remote_disk_checks.running.is_none()
    });
    assert!(app.pending_disk_checks.is_empty());
}

#[test]
fn disconnected_remote_disk_checks_release_pending_paths_for_retry() {
    let root = tempfile::tempdir().unwrap();
    let remote = crate::remote::workspace::Workspace::new(crate::remote::tests::connection(
        root.path(),
        Arc::default(),
        Arc::default(),
    ));
    remote
        .client
        .call(crate::remote::Command::Shutdown)
        .unwrap();
    let path = remote.view_key(&crate::remote::RemotePath::new(b"file.rs".to_vec()).unwrap());
    let mut app = App::new_deferred(remote.view_root.clone());
    app.remote_workspace = Some(remote);
    for _ in 0..2 {
        app.queue_file_disk_checks([path.clone()]);
        until(|| {
            app.poll_background_results();
            app.remote_disk_checks.running.is_none()
        });
        assert!(app.pending_disk_checks.is_empty());
        assert!(app.remote_disk_checks.queued.is_empty());
    }
}

#[test]
fn remote_disk_fallback_reloads_clean_buffers_and_preserves_unsaved_edits() {
    let root = tempfile::tempdir().unwrap();
    let native = root.path().join("file.txt");
    std::fs::write(&native, "saved\n").unwrap();
    let remote = crate::remote::workspace::Workspace::new(crate::remote::tests::connection(
        root.path(),
        Arc::default(),
        Arc::default(),
    ));
    let path = remote.view_key(&crate::remote::RemotePath::new(b"file.txt".to_vec()).unwrap());
    let mut app = App::new_deferred(remote.view_root.clone());
    app.remote_workspace = Some(remote);
    app.source_control_enabled = false;
    app.workspace_watcher_active = false;
    app.open_file(&path);
    until(|| {
        app.poll_background_results();
        app.editor()
            .is_some_and(|editor| !editor.bg_loading && editor.rope.to_string() == "saved\n")
    });
    std::fs::write(&native, "changed externally\n").unwrap();
    app.queue_file_disk_checks([path.clone()]);
    until(|| {
        app.poll_background_results();
        app.editor()
            .is_some_and(|editor| editor.rope.to_string() == "changed externally\n")
            && app.remote_disk_checks.running.is_none()
    });
    app.editor_mut().unwrap().insert_char('!');
    let unsaved = app.editor().unwrap().rope.to_string();
    std::fs::write(&native, "another external change\n").unwrap();
    app.queue_file_disk_checks([path.clone()]);
    until(|| {
        app.poll_background_results();
        app.remote_disk_checks.running.is_none()
    });
    assert_eq!(app.editor().unwrap().rope.to_string(), unsaved);
    assert!(app.editor().unwrap().modified);
    assert!(app.pending_disk_checks.is_empty());
    assert_eq!(
        std::fs::read_to_string(&native).unwrap(),
        "another external change\n"
    );
    std::fs::remove_file(&native).unwrap();
    app.queue_file_disk_checks([path.clone()]);
    until(|| {
        app.poll_background_results();
        app.remote_disk_checks.running.is_none()
    });
    // Match native deletion recovery: keep the dirty document as Untitled so
    // saving cannot accidentally recreate/replace a newly created disk file.
    assert!(app.editor().unwrap().file_path.is_none());
    assert_eq!(app.editor().unwrap().rope.to_string(), unsaved);
    assert!(app.editor().unwrap().modified);
}
