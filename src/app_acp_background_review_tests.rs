use super::*;
use crate::app_acp::AgentState;

#[cfg(any(unix, windows))]
struct WorkspaceAlias {
    path: PathBuf,
    _holder: tempfile::TempDir,
}

#[cfg(any(unix, windows))]
impl WorkspaceAlias {
    fn new(root: &Path) -> Self {
        let holder = tempfile::tempdir().unwrap();
        let alias = Self {
            path: holder.path().join("workspace-alias"),
            _holder: holder,
        };
        #[cfg(windows)]
        assert!(
            std::process::Command::new("cmd.exe")
                .args(["/d", "/c", "mklink", "/J"])
                .arg(&alias.path)
                .arg(root)
                .output()
                .unwrap()
                .status
                .success()
        );
        #[cfg(unix)]
        std::os::unix::fs::symlink(root, &alias.path).unwrap();
        alias
    }

    fn unlink(&self) {
        #[cfg(windows)]
        let _ = std::fs::remove_dir(&self.path);
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(any(unix, windows))]
impl Drop for WorkspaceAlias {
    fn drop(&mut self) {
        // Unlink the junction before TempDir cleans up its containing directory,
        // including after a failed assertion. Never traverse its target.
        self.unlink();
    }
}

pub(super) fn other_agent(app: &mut App, id: &p::SessionId) {
    let mut session = Session::new(id.clone());
    session.begin_prompt("another agent's message".into());
    session.finish(None);
    app.chat.connections.insert(
        1,
        AgentState {
            agent: Some(AgentConfig::new("Other", "other-agent")),
            sessions: [(id.clone(), session)].into(),
            selected: Some(id.clone()),
            dock_selected: Some(id.clone()),
            open_tabs: vec![id.clone()],
            ..Default::default()
        },
    );
    app.chat.agent_order = vec![0, 1];
    app.chat.next_agent = 2;
}

pub(super) fn assert_file_marked(app: &mut App, path: &Path, chunks: usize) {
    assert!(
        app.pending_agent_change(path),
        "pending file lost its AI marker"
    );
    assert_eq!(app.editor_file_review(path).unwrap().chunks.len(), chunks);
    let tab = &app.tabs[app.active_tab];
    assert!(crate::ui_chrome_tabs::tab_display_name(app, app.active_tab, tab).ends_with(" [AI]"));
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    draw_review_file(app, &mut terminal);
    assert!(app.editor().unwrap().ai_review.active);
    assert_eq!(
        app.editor()
            .unwrap()
            .ai_review
            .hits
            .iter()
            .filter(|hit| hit.accept)
            .filter_map(|hit| hit.chunk)
            .collect::<BTreeSet<_>>()
            .len(),
        chunks,
    );
}

#[cfg(any(unix, windows))]
#[test]
fn workspace_alias_preserves_file_markers_and_chunk_decisions() {
    let (root, _data, mut app, _) = seeded();
    let alias = WorkspaceAlias::new(root.path());
    app.explorer.root = alias.path.clone();
    app.chat.rollback.root = alias.path.clone();
    let path = alias.path.join("a.txt");
    show_review_file(&mut app, &path);
    // Remove the link before assertions so a failed test never recursively
    // cleans up a Windows junction's target. Cached markers require no I/O.
    alias.unlink();
    assert_file_marked(&mut app, &path, 2);
    // Decisions still address the journal's file, rather than the spelling of
    // the editor path. Accept is metadata-only, including with unsaved text.
    app.editor_mut()
        .unwrap()
        .paste_with_text("unsaved user work\n");
    let buffer = app.editor().unwrap().rope.to_string();
    let chunk = app.editor_file_review(&path).unwrap().chunks[0].clone();
    app.resolve_editor_changes(&path, Some((chunk.edit, chunk.index)), true);
    settle(&mut app);
    assert_file_marked(&mut app, &path, 1);
    assert_eq!(app.editor().unwrap().rope.to_string(), buffer);
    assert_eq!(
        std::fs::read_to_string(root.path().join("a.txt")).unwrap(),
        "FIRST\nkeep\nLAST\n"
    );
}

#[cfg(any(unix, windows))]
#[test]
fn workspace_alias_rejection_protects_unsaved_work_reloads_buffers_and_closes_deleted_files() {
    let (root, _data, mut app, _) = seeded();
    let alias = WorkspaceAlias::new(root.path());
    app.explorer.root = alias.path.clone();
    app.chat.rollback.root = alias.path.clone();
    let path = alias.path.join("a.txt");
    show_review_file(&mut app, &path);
    app.editor_mut()
        .unwrap()
        .paste_with_text("unsaved user work\n");
    let unsaved = app.editor().unwrap().rope.to_string();
    app.resolve_editor_changes(&path, None, false);
    settle(&mut app);
    assert!(app.chat.error.as_deref().unwrap().contains("unsaved"));
    assert_eq!(app.editor().unwrap().rope.to_string(), unsaved);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "FIRST\nkeep\nLAST\n"
    );
    assert_file_marked(&mut app, &path, 2);

    // Discard only this test fixture's unsaved buffer, then reject through the
    // alias. The clean editor must receive the same result as the disk file.
    let mut editor = crate::editor::Editor::test_with_text("FIRST\nkeep\nLAST\n");
    editor.file_path = Some(path.clone());
    app.tabs[app.active_tab].editor = editor;
    app.chat.error = None;
    app.resolve_editor_changes(&path, None, false);
    settle(&mut app);
    let deadline = Instant::now() + Duration::from_secs(20);
    while app.editor().unwrap().rope.to_string() != "first\nkeep\nlast\n" {
        assert!(Instant::now() < deadline, "alias buffer was not reloaded");
        app.poll_background_results();
        app.poll_acp();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!app.editor().unwrap().modified);
    assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
    assert!(!app.pending_agent_change(&path));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "first\nkeep\nlast\n"
    );

    let created = root.path().join("created.txt");
    {
        let mut journal = app.chat.rollback.journal.as_ref().unwrap().lock().unwrap();
        journal
            .begin("", "chat", "create file", BTreeSet::new())
            .unwrap();
        std::fs::write(&created, "AI created file\n").unwrap();
        journal.finish(BTreeSet::new()).unwrap();
        app.chat.rollback.inline_reviews = Arc::new(journal.inline_reviews());
    }
    let created_alias = alias.path.join("created.txt");
    show_review_file(&mut app, &created_alias);
    assert_file_marked(&mut app, &created_alias, 1);
    app.resolve_editor_changes(&created_alias, None, false);
    settle(&mut app);
    assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
    assert!(!created.exists());
    assert!(
        !app.tabs
            .iter()
            .chain(app.groups.iter().flat_map(|g| &g.tabs))
            .any(|tab| tab.editor.file_path.as_deref() == Some(&created_alias))
    );
    assert!(!app.pending_agent_change(&created_alias));
}

#[test]
fn editor_review_markers_survive_unloaded_originating_conversations() {
    let (root, _data, mut app, _) = seeded();
    let path = root.path().join("a.txt");
    show_review_file(&mut app, &path);
    assert_file_marked(&mut app, &path, 2);
    // A reconnect or a partial history replay can unload all transcript caches.
    app.chat.sessions.clear();
    app.chat.selected = None;
    app.chat.open_tabs.clear();
    assert_file_marked(&mut app, &path, 2);
    let first = app.editor_file_review(&path).unwrap().chunks[0].edit;
    app.resolve_editor_changes(&path, Some((first, 0)), true);
    settle(&mut app);
    assert_file_marked(&mut app, &path, 1);
    app.resolve_editor_changes(&path, None, false);
    settle(&mut app);
    assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
    assert!(!app.pending_agent_change(&path));
    assert!(app.editor_file_review(&path).is_none());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "FIRST\nkeep\nlast\n"
    );
}

#[test]
fn closing_background_agent_keeps_its_pending_file_and_chunk_markers() {
    let (root, _data, mut app, _) = seeded();
    let path = root.path().join("a.txt");
    let id = app.chat.selected.clone().unwrap();
    // Agent IDs disambiguate otherwise identical session IDs.
    other_agent(&mut app, &id);
    app.select_chat_agent(1);
    app.refresh_chat_rollback();
    assert!(app.chat.session().unwrap().rollback.edits.is_empty());
    show_review_file(&mut app, &path);
    assert_file_marked(&mut app, &path, 2);
    app.chat_action(ChatAction::CloseAgent(0));
    app.confirm_close_chat_agent();
    settle(&mut app);
    assert!(app.chat.agent_state(0).is_none());
    assert_eq!(app.chat.active_agent, 1);
    assert_file_marked(&mut app, &path, 2);
    app.resolve_editor_changes(&path, None, true);
    settle(&mut app);
    assert!(!app.pending_agent_change(&path));
    assert!(app.editor_file_review(&path).is_none());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "FIRST\nkeep\nLAST\n"
    );
}

#[test]
fn journal_reload_restores_markers_without_loading_the_originating_chat() {
    let (root, data, mut app, _) = seeded();
    let path = root.path().join("a.txt");
    show_review_file(&mut app, &path);
    app.chat.sessions.clear();
    app.chat.selected = None;
    app.chat.open_tabs.clear();
    drop(app.chat.rollback.journal.take());
    app.chat.rollback.inline_reviews = Arc::default();
    app.chat.rollback.inline_root = PathBuf::new();
    let directory = data.path().to_path_buf();
    let workspace = root.path().to_path_buf();
    app.checkpoint_job(JobKind::Initialize, None, false, move || {
        Journal::open(&workspace, &directory).map(|journal| Reply::Loaded(Some(Box::new(journal))))
    });
    settle(&mut app);
    assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
    assert_file_marked(&mut app, &path, 2);
    assert!(app.chat.sessions.is_empty());

    // Matching is by complete workspace path, never just the file name or a
    // suffix. A file outside the workspace cannot resolve these chunks.
    let unrelated = tempfile::tempdir().unwrap();
    for other in [
        unrelated.path().join("a.txt"),
        root.path().join("nested/a.txt"),
    ] {
        assert!(!app.pending_agent_change(&other));
        assert!(app.editor_file_review(&other).is_none());
        app.resolve_editor_changes(&other, None, true);
        assert!(!app.chat.rollback.busy());
    }
    assert_file_marked(&mut app, &path, 2);
    app.resolve_editor_changes(&path, None, true);
    settle(&mut app);
    assert!(!app.pending_agent_change(&path));
    assert!(app.editor_file_review(&path).is_none());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "FIRST\nkeep\nLAST\n"
    );
}
