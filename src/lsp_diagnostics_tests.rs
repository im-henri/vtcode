use super::*;
use crate::lsp::{LspClient, TestLspServer};
use lsp_types::{Position, Range};
use serde_json::json;

fn fixture() -> (LspManager, TestLspServer, tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.py");
    let mut manager = LspManager::new(directory.path().to_path_buf());
    let (client, server) = LspClient::test_pair("python", &["*.py"]);
    manager.clients.push(client);
    manager.sync_document_owned(&path, 1, "digest = 1\n".into());
    server.take_messages();
    (manager, server, directory, path)
}

fn diagnostic(message: &str) -> Diagnostic {
    Diagnostic {
        range: Range::new(Position::new(1, 0), Position::new(1, 4)),
        severity: Some(DiagnosticSeverity::ERROR),
        message: message.into(),
        ..Default::default()
    }
}

#[test]
fn problems_order_refreshes_on_publication_close_and_empty_reports() {
    let mut manager = LspManager::new(PathBuf::new());
    let a = PathBuf::from("a.py");
    let b = PathBuf::from("b.py");
    let mut warning = diagnostic("warning");
    warning.severity = Some(DiagnosticSeverity::WARNING);
    let mut late = diagnostic("later error");
    late.range.start.line = 20;
    late.range.end.line = 20;
    manager.publish_diagnostics(b.clone(), Arc::new(vec![diagnostic("b error")]));
    manager.publish_diagnostics(a.clone(), Arc::new(vec![warning.clone(), late,
        diagnostic("first same-position error"), diagnostic("second same-position error")]));
    let messages = |manager: &mut LspManager| manager.problems_diagnostics()
        .map(|(_, diagnostic)| diagnostic.message.clone()).collect::<Vec<_>>();
    let expected = ["first same-position error", "second same-position error", "later error", "b error", "warning"];
    assert_eq!(messages(&mut manager), expected);
    assert_eq!(messages(&mut manager), expected);
    // Replacement with the same number of items must also invalidate order.
    manager.publish_diagnostics(b.clone(), Arc::new(vec![warning]));
    assert_eq!(messages(&mut manager), ["first same-position error", "second same-position error", "later error", "warning", "warning"]);
    manager.did_close(&a);
    assert_eq!(messages(&mut manager), ["warning"]);
    manager.publish_diagnostics(b, Arc::new(Vec::new()));
    assert_eq!(manager.problems_diagnostics().len(), 0);
}

fn edit_and_sync(manager: &mut LspManager, path: &Path, version: u64, now: Instant) {
    manager.invalidate_document_feedback(path);
    // Advance a deterministic clock instead of sleeping between keystrokes.
    manager.diagnostic_publication.edited(path, now);
    manager.sync_document_owned(path, version, format!("digest = {version}\ndige\n"));
}

#[test]
fn opening_a_file_displays_diagnostics_without_a_typing_delay() {
    let (mut manager, _server, _directory, path) = fixture();
    let now = Instant::now();
    manager.receive_diagnostics(
        path.clone(),
        Some(0),
        vec![diagnostic("existing error")],
        now,
    );
    assert_eq!(
        manager.get_diagnostics(&path).unwrap()[0].message,
        "existing error"
    );
    assert_eq!(manager.diagnostics_version, 1);
    assert_eq!(manager.diagnostic_publication.next_wait(now), None);
    manager.receive_diagnostics(path.clone(), Some(0), vec![], now);
    assert!(manager.get_diagnostics(&path).unwrap().is_empty());
    assert_eq!(manager.diagnostics_version, 2);
}

#[test]
fn diagnostics_wait_for_the_last_edit_and_only_publish_the_latest_report() {
    let (mut manager, _server, _directory, path) = fixture();
    let start = Instant::now();
    manager.receive_diagnostics(path.clone(), Some(0), vec![diagnostic("confirmed")], start);
    edit_and_sync(&mut manager, &path, 2, start);
    manager.receive_diagnostics(
        path.clone(),
        Some(1),
        vec![diagnostic("partial word")],
        start,
    );
    manager.publish_pending_diagnostics(start + Duration::from_millis(899));
    assert_eq!(
        manager.get_diagnostics(&path).unwrap()[0].message,
        "confirmed"
    );
    assert_eq!(manager.diagnostics_version, 1);

    edit_and_sync(&mut manager, &path, 3, start + Duration::from_millis(900));
    for message in ["partial line", "latest error"] {
        manager.receive_diagnostics(
            path.clone(),
            Some(2),
            vec![diagnostic(message)],
            start + Duration::from_millis(1_100),
        );
    }
    assert_eq!(manager.diagnostic_publication.pending.len(), 1);
    assert_eq!(
        manager
            .diagnostic_publication
            .next_wait(start + Duration::from_millis(1_100)),
        Some(Duration::from_millis(800))
    );
    manager.publish_pending_diagnostics(start + Duration::from_millis(1_899));
    assert_eq!(manager.diagnostics_version, 1);
    manager.publish_pending_diagnostics(start + Duration::from_millis(1_900));
    assert_eq!(
        manager.get_diagnostics(&path).unwrap()[0].message,
        "latest error"
    );
    assert_eq!(manager.diagnostics_version, 2);
    assert_eq!(manager.diagnostic_publication.next_wait(start), None);
}

#[test]
fn continuous_synchronized_typing_never_publishes_transient_errors() {
    let (mut manager, _server, _directory, path) = fixture();
    let start = Instant::now();
    for version in 2..=52 {
        let now = start + Duration::from_millis((version - 2) * 50);
        edit_and_sync(&mut manager, &path, version, now);
        manager.receive_diagnostics(path.clone(), None, vec![diagnostic("unfinished word")], now);
        manager.publish_pending_diagnostics(now + Duration::from_millis(49));
        assert!(manager.get_diagnostics(&path).is_none());
        assert_eq!(manager.diagnostics_version, 0);
        assert_eq!(manager.diagnostic_publication.pending.len(), 1);
    }
    let finished = start + Duration::from_millis(2_550);
    edit_and_sync(&mut manager, &path, 53, finished);
    manager.receive_diagnostics(path.clone(), None, vec![], finished);
    manager.publish_pending_diagnostics(finished + Duration::from_secs(1));
    assert!(manager.get_diagnostics(&path).unwrap().is_empty());
    assert_eq!(manager.diagnostics_version, 1);
}

#[test]
fn a_new_edit_discards_unversioned_pending_diagnostics_and_rejects_stale_replies() {
    let (mut manager, _server, _directory, path) = fixture();
    let start = Instant::now();
    edit_and_sync(&mut manager, &path, 2, start);
    manager.receive_diagnostics(path.clone(), None, vec![diagnostic("obsolete")], start);
    manager.invalidate_document_feedback(&path);
    manager
        .diagnostic_publication
        .edited(&path, start + Duration::from_millis(500));
    assert_eq!(manager.diagnostic_publication.next_wait(start), None);
    manager.receive_diagnostics(
        path.clone(),
        None,
        vec![diagnostic("unsynchronized")],
        start,
    );
    manager.publish_pending_diagnostics(start + Duration::from_secs(2));
    assert!(manager.get_diagnostics(&path).is_none());

    manager.sync_document_owned(&path, 3, "digest = 3\n".into());
    manager.receive_diagnostics(
        path.clone(),
        Some(1),
        vec![diagnostic("old version")],
        start + Duration::from_secs(2),
    );
    assert!(manager.get_diagnostics(&path).is_none());
    manager.receive_diagnostics(
        path.clone(),
        Some(2),
        vec![],
        start + Duration::from_secs(2),
    );
    assert!(manager.get_diagnostics(&path).unwrap().is_empty());
    assert_eq!(manager.diagnostics_version, 1);
}

#[test]
fn completions_and_quick_fix_context_stay_immediate_while_diagnostic_display_waits() {
    let (mut manager, server, _directory, path) = fixture();
    manager.diagnostic_publication.set_delay_ms(MAX_DELAY_MS);
    let start = Instant::now();
    edit_and_sync(&mut manager, &path, 2, start);
    manager.request_completion(&path, 1, 4, "dige");
    server.respond(json!({"method":"textDocument/publishDiagnostics","params":{
        "uri":lsp_types::Url::from_file_path(&path).unwrap(),"version":1,
        "diagnostics":[{"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":4}},
            "message":"dige is not defined","code":"undefined","data":{"fix":17}}]}}));
    server.respond(json!({"id":manager.active_completion_id,"result":[{"label":"digest"}]}));
    for _ in 0..3 {
        manager.poll();
    }
    assert_eq!(manager.completion_items[0].label, "digest");
    assert!(manager.get_diagnostics(&path).is_none());
    assert_eq!(manager.diagnostics_version, 0);
    assert_eq!(
        manager.code_action_diagnostics(&path, 1)[0].data,
        Some(json!({"fix":17}))
    );
    manager.publish_pending_diagnostics(start + Duration::from_millis(MAX_DELAY_MS));
    let displayed = &manager.get_diagnostics(&path).unwrap()[0];
    assert_eq!(displayed.message, "dige is not defined");
    assert_eq!(
        (
            displayed.line,
            displayed.col,
            displayed.end_line,
            displayed.end_col
        ),
        (1, 0, 1, 4)
    );
    assert_eq!(displayed.severity, DiagnosticSeverity::INFORMATION);
}

#[test]
fn an_empty_report_clears_confirmed_problems_after_the_same_quiet_period() {
    let (mut manager, _server, _directory, path) = fixture();
    let start = Instant::now();
    manager.receive_diagnostics(path.clone(), None, vec![diagnostic("confirmed")], start);
    edit_and_sync(&mut manager, &path, 2, start);
    manager.receive_diagnostics(path.clone(), None, vec![diagnostic("intermediate")], start);
    manager.receive_diagnostics(path.clone(), None, vec![], start);
    assert_eq!(
        manager.get_diagnostics(&path).unwrap()[0].message,
        "confirmed"
    );
    assert_eq!(manager.diagnostics_version, 1);
    manager.publish_pending_diagnostics(start + Duration::from_secs(1));
    assert!(manager.get_diagnostics(&path).unwrap().is_empty());
    assert_eq!(manager.diagnostics_version, 2);
}

#[test]
fn diagnostic_deadlines_are_independent_for_each_document() {
    let (mut manager, _server, directory, first) = fixture();
    let second = directory.path().join("second.py");
    manager.sync_document_owned(&second, 1, "broken\n".into());
    let start = Instant::now();
    edit_and_sync(&mut manager, &first, 2, start);
    manager.receive_diagnostics(first.clone(), None, vec![diagnostic("first")], start);
    manager.receive_diagnostics(second.clone(), None, vec![diagnostic("second")], start);
    assert!(manager.get_diagnostics(&first).is_none());
    assert_eq!(
        manager.get_diagnostics(&second).unwrap()[0].message,
        "second"
    );

    edit_and_sync(&mut manager, &second, 2, start + Duration::from_millis(500));
    manager.receive_diagnostics(
        second.clone(),
        None,
        vec![],
        start + Duration::from_millis(600),
    );
    assert_eq!(
        manager
            .diagnostic_publication
            .next_wait(start + Duration::from_millis(600)),
        Some(Duration::from_millis(400))
    );
    manager.publish_pending_diagnostics(start + Duration::from_secs(1));
    assert_eq!(manager.get_diagnostics(&first).unwrap()[0].message, "first");
    assert_eq!(
        manager.get_diagnostics(&second).unwrap()[0].message,
        "second"
    );
    assert_eq!(
        manager
            .diagnostic_publication
            .next_wait(start + Duration::from_secs(1)),
        Some(Duration::from_millis(500))
    );
    manager.publish_pending_diagnostics(start + Duration::from_millis(1_500));
    assert!(manager.get_diagnostics(&second).unwrap().is_empty());
    assert_eq!(manager.diagnostic_publication.next_wait(start), None);
}

#[test]
fn closing_a_document_drops_its_pending_report_and_typing_deadline() {
    let (mut manager, _server, _directory, path) = fixture();
    let start = Instant::now();
    edit_and_sync(&mut manager, &path, 2, start);
    manager.receive_diagnostics(path.clone(), None, vec![diagnostic("obsolete")], start);
    manager.did_close(&path);
    assert_eq!(manager.diagnostic_publication.next_wait(start), None);
    manager.receive_diagnostics(path.clone(), None, vec![diagnostic("closed")], start);
    manager.publish_pending_diagnostics(start + Duration::from_secs(2));
    assert!(manager.get_diagnostics(&path).is_none());
    manager.sync_document_owned(&path, 1, "reopened\n".into());
    manager.receive_diagnostics(path.clone(), Some(0), vec![diagnostic("reopened")], start);
    assert_eq!(
        manager.get_diagnostics(&path).unwrap()[0].message,
        "reopened"
    );
    assert_eq!(manager.diagnostic_publication.next_wait(start), None);
}

#[test]
fn a_queued_report_is_not_published_if_its_document_version_changed() {
    let (mut manager, _server, _directory, path) = fixture();
    let start = Instant::now();
    edit_and_sync(&mut manager, &path, 2, start);
    manager.receive_diagnostics(
        path.clone(),
        Some(1),
        vec![diagnostic("old snapshot")],
        start,
    );
    // File reloads can synchronize a newer snapshot without a typing event.
    manager.sync_document_owned(&path, 3, "reloaded = 3\n".into());
    manager.publish_pending_diagnostics(start + Duration::from_secs(1));
    assert!(manager.get_diagnostics(&path).is_none());
    assert_eq!(manager.diagnostic_publication.next_wait(start), None);
}

#[test]
fn diagnostic_delay_changes_apply_to_queued_reports_and_zero_is_immediate() {
    let (mut manager, _server, _directory, path) = fixture();
    let start = Instant::now();
    edit_and_sync(&mut manager, &path, 2, start);
    manager.receive_diagnostics(path.clone(), None, vec![diagnostic("queued")], start);
    manager.diagnostic_publication.set_delay_ms(2_000);
    manager.publish_pending_diagnostics(start + Duration::from_secs(1));
    assert!(manager.get_diagnostics(&path).is_none());
    manager.diagnostic_publication.set_delay_ms(0);
    assert_eq!(
        manager.diagnostic_publication.next_wait(start),
        Some(Duration::ZERO)
    );
    manager.publish_pending_diagnostics(start);
    assert_eq!(manager.get_diagnostics(&path).unwrap()[0].message, "queued");
    edit_and_sync(&mut manager, &path, 3, start);
    manager.receive_diagnostics(path.clone(), None, vec![diagnostic("immediate")], start);
    assert_eq!(
        manager.get_diagnostics(&path).unwrap()[0].message,
        "immediate"
    );
    assert_eq!(manager.diagnostic_publication.next_wait(start), None);
    manager.diagnostic_publication.set_delay_ms(u64::MAX);
    assert_eq!(manager.diagnostic_publication.delay_ms(), MAX_DELAY_MS);
}

#[test]
fn repeated_reports_keep_display_storage_but_refresh_quick_fix_data() {
    let (mut manager, _server, _directory, path) = fixture();
    let mut report = diagnostic("unchanged error");
    report.data = Some(json!({"fix": 1}));
    let now = Instant::now();
    manager.receive_diagnostics(path.clone(), Some(0), vec![report.clone()], now);
    let storage = manager.get_diagnostics(&path).unwrap().as_ptr();
    for fix in 2..=40 {
        report.data = Some(json!({"fix": fix}));
        manager.receive_diagnostics(path.clone(), Some(0), vec![report.clone()], now);
        assert_eq!(manager.diagnostics_version, 1);
        assert_eq!(manager.get_diagnostics(&path).unwrap().as_ptr(), storage);
        assert_eq!(manager.code_action_diagnostics(&path, 1)[0].data, report.data);
    }
    // Every displayed attribute can independently invalidate the cache.
    report.message = "changed error".into();
    manager.receive_diagnostics(path.clone(), Some(0), vec![report.clone()], now);
    report.range.end.character += 1;
    manager.receive_diagnostics(path.clone(), Some(0), vec![report.clone()], now);
    report.severity = Some(DiagnosticSeverity::WARNING);
    manager.receive_diagnostics(path, Some(0), vec![report], now);
    assert_eq!(manager.diagnostics_version, 4);
}

#[test]
fn delayed_display_shares_one_report_and_quick_fixes_reject_unsynchronized_or_stale_context() {
    let (mut manager, _server, _directory, path) = fixture();
    let now = Instant::now();
    edit_and_sync(&mut manager, &path, 2, now);
    manager.receive_diagnostics(path.clone(), Some(1), vec![diagnostic("current")], now);
    assert!(Arc::ptr_eq(&manager.diagnostic_context[&path].diagnostics,
        &manager.diagnostic_publication.pending[&path].diagnostics));
    assert_eq!(manager.code_action_diagnostics(&path, 1).len(), 1);
    manager.invalidate_document_feedback(&path);
    assert!(manager.code_action_diagnostics(&path, 1).is_empty());
    manager.sync_document_owned(&path, 3, "different text".into());
    assert!(manager.code_action_diagnostics(&path, 1).is_empty());
    manager.receive_diagnostics(path.clone(), Some(2), vec![diagnostic("new version")], now);
    assert_eq!(manager.code_action_diagnostics(&path, 1)[0].message, "new version");
    manager.did_close(&path);
    assert!(manager.current_diagnostic_context(&path).is_none());
}

#[test]
fn late_empty_and_pre_reopen_reports_cannot_repopulate_closed_document_state() {
    let (mut manager, _server, _directory, path) = fixture();
    let now = Instant::now();
    edit_and_sync(&mut manager, &path, 2, now);
    manager.did_close(&path);
    let revision = manager.diagnostics_version;
    manager.receive_diagnostics(path.clone(), None, vec![], now);
    assert!(manager.diagnostics.is_empty());
    assert!(manager.diagnostic_context.is_empty());
    assert_eq!(manager.diagnostics_version, revision);
    manager.sync_document_owned(&path, 3, "reopened".into());
    manager.receive_diagnostics(path.clone(), Some(1), vec![diagnostic("previous open")], now);
    assert!(manager.get_diagnostics(&path).is_none());
    manager.receive_diagnostics(path.clone(), Some(0), vec![diagnostic("new open")], now);
    assert_eq!(manager.get_diagnostics(&path).unwrap()[0].message, "new open");
}
