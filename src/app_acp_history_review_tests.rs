use super::*;

#[test]
fn unloaded_history_cannot_be_deleted_until_its_durable_pending_changes_are_resolved() {
    let (root, data, mut app, _) = seeded();
    let id = app.chat.selected.clone().unwrap();
    app.chat.sessions.clear();
    app.chat.selected = None;
    app.chat.open_tabs.clear();
    drop(app.chat.rollback.journal.take());
    app.chat.rollback.journal = Some(Arc::new(Mutex::new(
        Journal::open(root.path(), data.path()).unwrap(),
    )));
    assert!(
        app.chat
            .history_delete_unavailable(&id)
            .unwrap()
            .contains("pending changes")
    );
    app.chat_action(ChatAction::DeleteSession(id.clone()));
    assert!(!app.chat.history_filter.contains(&id));
    assert!(
        app.chat
            .error
            .as_deref()
            .unwrap()
            .contains("pending changes")
    );
    let journal = app.chat.rollback.journal.clone().unwrap();
    journal
        .lock()
        .unwrap()
        .accept("", &id.to_string(), &Selection::All)
        .unwrap();
    assert!(app.chat.history_delete_unavailable(&id).is_none());
    app.chat_action(ChatAction::DeleteSession(id.clone()));
    assert!(app.chat.history_filter.contains(&id));
}

#[test]
fn history_deletion_waits_for_a_busy_or_failed_journal_instead_of_guessing_no_edits() {
    let (_root, _data, mut app, _) = seeded();
    let id = p::SessionId::new("unloaded");
    let journal = app.chat.rollback.journal.clone().unwrap();
    let guard = journal.lock().unwrap();
    assert!(
        app.chat
            .history_delete_unavailable(&id)
            .unwrap()
            .contains("checkpoint capture")
    );
    app.chat_action(ChatAction::DeleteSession(id.clone()));
    assert!(!app.chat.history_filter.contains(&id));
    drop(guard);
    assert!(app.chat.history_delete_unavailable(&id).is_none());
    app.chat.rollback.initialization_error = Some("Storage unavailable".into());
    assert!(
        app.chat
            .history_delete_unavailable(&id)
            .unwrap()
            .contains("Retry checkpoint storage")
    );
}
