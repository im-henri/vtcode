use super::*;

#[test]
fn native_git_operations_record_outcomes_without_idle_work() {
    let log = Log::default();
    let operation = native(&log, "stage files", Path::new("repository"));
    operation.result(&Ok::<_, String>(()), |_| "1 file staged".into());
    let report = log.report();
    assert!(
        report.contains("stage files")
            && report.contains("Running")
            && report.contains("Completed")
            && report.contains("1 file staged")
    );
    let version = log.version();
    drop(operation);
    assert_eq!(log.version(), version);
}

#[test]
fn local_git_actions_and_failures_are_retained_before_output_is_opened() {
    use std::time::Duration;
    let directory = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(directory.path()).unwrap();
    std::fs::write(directory.path().join("file.txt"), "PRIVATE FILE CONTENT\n").unwrap();
    let mut app = App::test_new();
    app.explorer.root = directory.path().to_owned();
    app.workspace_watcher_active = true;
    let log = app.git_output();
    assert!(!app.show_panel || app.panel_tab != 2);
    app.git_stage_all();
    app.wait_for_background(Duration::from_secs(10), |app| !app.bg_git_action_running);
    assert!(
        repo.index()
            .unwrap()
            .get_path(Path::new("file.txt"), 0)
            .is_some()
    );
    let report = log.report();
    assert!(report.contains("stage all") && report.contains("Completed"));
    assert!(!report.contains("PRIVATE FILE CONTENT"));

    app.queue_git_task(
        "git-output-test",
        "checkout branch",
        crate::git_tasks::Task::CheckoutLocal {
            branch_name: "missing-branch".into(),
        },
    );
    app.wait_for_background(Duration::from_secs(10), |app| !app.bg_git_action_running);
    let report = log.report();
    assert!(report.contains("checkout branch") && report.contains("Failed"));
    app.refresh_source_control_async();
    app.wait_for_background(Duration::from_secs(10), |app| {
        !app.bg_git_status_running && !app.bg_git_status_pending
    });
    assert!(log.report().contains("Refresh status"));
}

#[test]
fn network_logs_keep_command_output_unicode_and_failure_without_terminal_controls() {
    let log = Log::default();
    let operation = native(&log, "git pull", Path::new("repository"));
    operation.network(&crate::remote::git::NetworkResult {
        stdout: "Already up to date.\nλ🦀\n".into(),
        stderr: "\x1b[31mPermission denied\x1b[0m\x1b]52;c;c2VjcmV0\x07".into(),
        success: false,
    });
    let report = log.report();
    for text in [
        "git pull",
        "stdout",
        "Already up to date.",
        "λ🦀",
        "stderr",
        "Permission denied",
        "Failed",
    ] {
        assert!(report.contains(text), "missing {text}");
    }
    assert!(!report.contains('\x1b') && !report.contains("c2VjcmV0"));
    let version = log.version();
    operation.network(&crate::remote::git::NetworkResult {
        stdout: "late output".into(),
        stderr: String::new(),
        success: true,
    });
    assert_eq!(
        log.version(),
        version,
        "a late completion cannot append output"
    );
}
