use super::*;
use crate::app::ActivePane;
use ratatui::{Terminal, backend::TestBackend};
use std::time::{Duration, Instant};
use vtcode::acp::Session;

#[path = "app_acp_live_tests.rs"]
mod live_tests;
#[path = "app_acp_history_review_tests.rs"]
mod history_review_tests;
#[path = "app_acp_background_review_tests.rs"]
mod background_review_tests;

#[cfg(windows)]
#[test]
fn checkpoint_guards_match_windows_case_aliases() {
    let (root, _data, mut app, _) = seeded();
    let path = root.path().join("a.txt");
    let alias = PathBuf::from(path.to_string_lossy().to_uppercase());
    app.chat.rollback.write_paths = vec![app.chat.rollback.path_key(&path)];
    assert!(app.chat.rollback.file_busy(&alias));
    app.chat.rollback.write_paths.clear();
    let id = app.chat.selected.clone().unwrap();
    app.chat
        .rollback
        .turns
        .insert((0, id.clone()), TurnCapture::default());
    app.chat.rollback.note_user_save(&alias);
    assert!(
        app.chat.rollback.turns[&(0, id)]
            .protected
            .iter()
            .any(|path| App::same_open_path(path, Path::new("a.txt")))
    );
}

#[cfg(any(unix, windows))]
#[test]
fn checkpoint_guards_protect_buffers_opened_through_a_directory_alias() {
    let (root, _data, mut app, _) = seeded();
    let holder = tempfile::tempdir().unwrap();
    let link = holder.path().join("workspace-alias");
    #[cfg(windows)]
    assert!(
        std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&link)
            .arg(root.path())
            .output()
            .unwrap()
            .status
            .success()
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.path(), &link).unwrap();
    let alias = link.join("a.txt");
    app.open_file(&alias);
    app.editor_mut().unwrap().rope = ropey::Rope::from_str("unsaved work through an alias");
    app.editor_mut().unwrap().modified = true;
    let reason = app.rollback_buffer_conflict(&[root.path().join("a.txt")]);
    let id = app.chat.selected.clone().unwrap();
    app.chat
        .rollback
        .turns
        .insert((0, id.clone()), TurnCapture::default());
    app.chat.rollback.note_user_save(&alias);
    let missing = link.join("new.txt");
    app.editor_mut().unwrap().file_path = Some(missing.clone());
    let missing_reason = app.rollback_buffer_conflict(&[root.path().join("new.txt")]);
    app.chat.rollback.note_user_save(&missing);
    let protected = app.chat.rollback.turns[&(0, id)].protected.clone();
    #[cfg(windows)]
    std::fs::remove_dir(&link).unwrap();
    #[cfg(unix)]
    std::fs::remove_file(&link).unwrap();
    assert!(reason.as_ref().is_some_and(|text| text.contains("unsaved")));
    assert!(
        missing_reason
            .as_ref()
            .is_some_and(|text| text.contains("unsaved"))
    );
    assert!(
        protected
            .iter()
            .any(|path| App::same_open_path(path, Path::new("a.txt")))
    );
    assert!(
        protected
            .iter()
            .any(|path| App::same_open_path(path, Path::new("new.txt")))
    );
    assert_eq!(
        app.editor().unwrap().rope.to_string(),
        "unsaved work through an alias"
    );
}

fn settle(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        app.poll_acp();
        if !app.chat.rollback.busy() {
            break;
        }
        assert!(Instant::now() < deadline, "checkpoint worker timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn seeded() -> (tempfile::TempDir, tempfile::TempDir, App, u64) {
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    git2::Repository::init(root.path()).unwrap();
    std::fs::write(root.path().join("a.txt"), "first\nkeep\nlast\n").unwrap();
    let mut journal = Journal::open(root.path(), data.path()).unwrap();
    let cp = journal
        .begin("", "chat", "change this", BTreeSet::new())
        .unwrap();
    std::fs::write(root.path().join("a.txt"), "FIRST\nkeep\nLAST\n").unwrap();
    journal.finish(BTreeSet::new()).unwrap();
    let inline_reviews = Arc::new(journal.inline_reviews());
    let mut app = App::test_new();
    app.explorer.root = root.path().into();
    app.chat.visible = true;
    app.active_pane = ActivePane::Chat;
    let id = p::SessionId::new("chat");
    let mut session = Session::new(id.clone());
    session.begin_prompt("change this".into());
    session.finish(None);
    app.chat.sessions.insert(id.clone(), session);
    app.chat.selected = Some(id.clone());
    app.chat.open_tabs.push(id);
    app.chat.rollback = ReviewState {
        enabled: true,
        initialized: true,
        root: root.path().into(),
        journal: Some(Arc::new(Mutex::new(journal))),
        inline_reviews,
        ..Default::default()
    };
    app.refresh_chat_rollback();
    (root, data, app, cp)
}

fn show_review_file(app: &mut App, path: &Path) {
    let mut editor = crate::editor::Editor::test_with_text(&std::fs::read_to_string(path).unwrap());
    editor.file_path = Some(path.to_path_buf());
    app.tabs.push(crate::app::Tab::new_file(editor));
    app.active_tab = app.tabs.len() - 1;
    app.active_pane = ActivePane::Editor;
    app.chat.visible = false;
    app.show_minimap = false;
}

fn draw_review_file(app: &mut App, terminal: &mut Terminal<TestBackend>) {
    terminal
        .draw(|frame| {
            app.editor_area = Some(frame.area());
            crate::ui_editor::draw_editor(frame, app, frame.area());
        })
        .unwrap();
}

fn click_editor_review(app: &mut App, chunk: Option<(u64, usize)>, accept: bool) {
    press_editor_review(app, chunk, accept);
    settle(app);
}

fn press_editor_review(app: &mut App, chunk: Option<(u64, usize)>, accept: bool) {
    let hit = app
        .editor()
        .unwrap()
        .ai_review
        .hits
        .iter()
        .find(|hit| hit.chunk == chunk && hit.accept == accept)
        .unwrap()
        .clone();
    crate::mouse::handle_mouse(
        app,
        crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: hit.area.x,
            row: hit.area.y,
            modifiers: crossterm::event::KeyModifiers::empty(),
        },
    );
}

#[test]
fn acceptance_in_chat_and_editor_keeps_responses_permissions_and_writes_running() {
    for editor in [false, true] {
        for all in [false, true] {
            let (root, _data, mut app, _) = seeded();
            let path = root.path().join("a.txt");
            let session = app.chat.selected.clone().unwrap();
            let edit = app.chat.session().unwrap().rollback.edits[0].id;
            let checkpoint = app
                .chat
                .rollback
                .journal
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .begin("", "chat", "still working", BTreeSet::new())
                .unwrap();
            app.chat.rollback.turns.insert(
                (0, session.clone()),
                TurnCapture {
                    checkpoint: Some(checkpoint),
                    ..Default::default()
                },
            );
            app.chat
                .session_mut()
                .unwrap()
                .begin_prompt("still working".into());
            app.chat
                .session_mut()
                .unwrap()
                .terminal(vtcode::acp::TerminalSnapshot {
                    session_id: session.clone(),
                    id: "running".into(),
                    command: "python check.py".into(),
                    cwd: root.path().into(),
                    output: String::new(),
                    truncated: false,
                    exit_status: None,
                    released: false,
                    killed: false,
                    error: None,
                    history: None,
                    history_version: 0,
                });
            app.apply_acp_event(vtcode::acp::Event::Permission {
                ticket: 0,
                id: serde_json::from_value(serde_json::json!(17)).unwrap(),
                request: serde_json::from_value(serde_json::json!({
                    "sessionId": session,
                    "toolCall": {"toolCallId": "check", "title": "Run checks"},
                    "options": [{"optionId": "yes", "name": "Allow once", "kind": "allow_once"}]
                }))
                .unwrap(),
            });
            let write = app.chat.rollback.file_request_guard(0, &session, true);
            std::fs::write(&path, "FIRST\nkeep\nACTIVE\n").unwrap();
            let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
            app.refresh_chat_rollback();
            if editor {
                show_review_file(&mut app, &path);
                let mut terminal = Terminal::new(TestBackend::new(80, 22)).unwrap();
                draw_review_file(&mut app, &mut terminal);
                assert!(
                    app.editor()
                        .unwrap()
                        .ai_review
                        .hits
                        .iter()
                        .all(|h| h.accept)
                );
                press_editor_review(&mut app, (!all).then_some((edit, 0)), true);
            } else {
                app.chat_action(ChatAction::Resolve(
                    if all {
                        Selection::All
                    } else {
                        Selection::Chunk(edit, 0)
                    },
                    true,
                ));
            }
            assert!(!app.chat.rollback.reviewing());
            assert!(!app.chat.rollback.mutating);
            settle(&mut app);

            assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
            assert_eq!(
                app.chat.session().unwrap().rollback.edits[0].pending(),
                usize::from(!all)
            );
            assert!(app.chat.session().unwrap().busy);
            assert!(
                app.chat.session().unwrap().terminals["running"]
                    .exit_status
                    .is_none()
            );
            assert!(!app.chat.session().unwrap().terminals["running"].released);
            assert_eq!(app.chat.permissions.len(), 1);
            assert_eq!(app.chat.rollback.file_writes.load(Ordering::Acquire), 1);
            assert_eq!(
                app.chat.session().unwrap().rollback.active,
                Some(checkpoint)
            );
            assert_eq!(
                app.chat.rollback.turns[&(0, session.clone())].checkpoint,
                Some(checkpoint)
            );
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                "FIRST\nkeep\nACTIVE\n"
            );
            assert_eq!(
                std::fs::metadata(&path).unwrap().modified().unwrap(),
                modified
            );
            app.chat_action(ChatAction::Resolve(Selection::All, false));
            assert!(
                app.chat
                    .error
                    .as_ref()
                    .unwrap()
                    .contains("Wait for responses")
            );

            drop(write);
            app.chat.session_mut().unwrap().finish(None);
            app.chat
                .session_mut()
                .unwrap()
                .terminals
                .get_mut("running")
                .unwrap()
                .released = true;
            settle(&mut app);
            let summary = &app.chat.session().unwrap().rollback;
            assert!(summary.active.is_none());
            assert!(
                summary
                    .edits
                    .iter()
                    .filter(|e| e.checkpoint == checkpoint)
                    .any(|e| e.pending() > 0)
            );
            assert_eq!(
                summary
                    .edits
                    .iter()
                    .find(|e| e.id == edit)
                    .unwrap()
                    .pending(),
                usize::from(!all)
            );
        }
    }
}

#[test]
fn accept_all_during_capture_leaves_newly_captured_chunks_pending() {
    for editor in [false, true] {
        let (root, _data, mut app, _) = seeded();
        let path = root.path().join("a.txt");
        let session = app.chat.selected.clone().unwrap();
        let original = app.chat.session().unwrap().rollback.edits[0].id;
        let journal = app.chat.rollback.journal.as_ref().unwrap().clone();
        let checkpoint = journal
            .lock()
            .unwrap()
            .begin("", "chat", "new capture", BTreeSet::new())
            .unwrap();
        app.chat
            .session_mut()
            .unwrap()
            .begin_prompt("new capture".into());
        app.chat.rollback.turns.insert(
            (0, session.clone()),
            TurnCapture {
                checkpoint: Some(checkpoint),
                finishing: true,
                ..Default::default()
            },
        );
        app.refresh_chat_rollback();
        std::fs::write(&path, "FIRST\nnewly captured\nLAST\n").unwrap();
        let (release, wait) = mpsc::channel();
        app.checkpoint_job(JobKind::Finish, Some(session.clone()), true, move || {
            wait.recv_timeout(Duration::from_secs(20)).unwrap();
            journal
                .lock()
                .unwrap()
                .finish_checkpoint(checkpoint, BTreeSet::new())?;
            Ok(Reply::Finished(session))
        });
        if editor {
            show_review_file(&mut app, &path);
            let mut terminal = Terminal::new(TestBackend::new(80, 22)).unwrap();
            draw_review_file(&mut app, &mut terminal);
            press_editor_review(&mut app, None, true);
        } else {
            app.chat_action(ChatAction::Resolve(Selection::All, true));
        }
        assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
        assert!(!app.chat.rollback.reviewing());
        assert!(
            app.chat
                .rollback
                .jobs
                .back()
                .is_some_and(|j| j.context.kind == JobKind::Accept)
        );
        release.send(()).unwrap();
        settle(&mut app);

        assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
        let summary = &app.chat.session().unwrap().rollback;
        assert_eq!(
            summary
                .edits
                .iter()
                .find(|e| e.id == original)
                .unwrap()
                .pending(),
            0
        );
        assert_eq!(
            summary
                .edits
                .iter()
                .find(|e| e.checkpoint == checkpoint)
                .unwrap()
                .pending(),
            1
        );
        assert_eq!(summary.pending_chunks(&Selection::All).unwrap().len(), 1);
        assert_eq!(app.editor_file_review(&path).unwrap().chunks.len(), 1);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "FIRST\nnewly captured\nLAST\n"
        );
        assert!(app.chat.session().unwrap().busy);
    }
}

#[test]
fn editor_ai_review_accepts_one_chunk_and_rejects_another_with_chat_closed() {
    let (root, _data, mut app, _) = seeded();
    let path = root.path().join("a.txt");
    let id = app.chat.session().unwrap().rollback.edits[0].id;
    show_review_file(&mut app, &path);
    let mut terminal = Terminal::new(TestBackend::new(80, 18)).unwrap();
    draw_review_file(&mut app, &mut terminal);
    assert_eq!(app.editor().unwrap().ai_review.hits.len(), 8);
    click_editor_review(&mut app, Some((id, 0)), true);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "FIRST\nkeep\nLAST\n"
    );
    draw_review_file(&mut app, &mut terminal);
    assert_eq!(app.editor().unwrap().ai_review.hits.len(), 6);
    click_editor_review(&mut app, Some((id, 1)), false);
    assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "FIRST\nkeep\nlast\n"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.editor().unwrap().rope.to_string() != "FIRST\nkeep\nlast\n" {
        app.poll_background_results();
        assert!(
            Instant::now() < deadline,
            "editor did not reload the rejected chunk"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    draw_review_file(&mut app, &mut terminal);
    assert!(!app.editor().unwrap().ai_review.active);
    assert!(!app.pending_agent_change(&path));
}

fn editor_toolbar_hit(app: &App, accept: bool) -> crate::editor_ai_review::ReviewHit {
    app.editor()
        .unwrap()
        .ai_review
        .hits
        .iter()
        .rev()
        .find(|hit| hit.chunk.is_some() && hit.accept == accept)
        .unwrap()
        .clone()
}

fn press_review_hit(app: &mut App, hit: &crate::editor_ai_review::ReviewHit) {
    crate::mouse::handle_mouse(
        app,
        crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: hit.area.x,
            row: hit.area.y,
            modifiers: crossterm::event::KeyModifiers::empty(),
        },
    );
}

#[test]
fn editor_ai_review_toolbar_resolves_only_the_navigated_chunk() {
    let (root, _data, mut app, _) = seeded();
    let path = root.path().join("a.txt");
    let edit = app.chat.session().unwrap().rollback.edits[0].id;
    show_review_file(&mut app, &path);
    let mut terminal = Terminal::new(TestBackend::new(80, 18)).unwrap();
    draw_review_file(&mut app, &mut terminal);
    let arrow = app.editor().unwrap().ai_review.navigation_hits[1].clone();
    crate::mouse::handle_mouse(
        &mut app,
        crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: arrow.area.x,
            row: arrow.area.y,
            modifiers: crossterm::event::KeyModifiers::empty(),
        },
    );
    draw_review_file(&mut app, &mut terminal);
    let hit = editor_toolbar_hit(&app, false);
    assert_eq!(hit.chunk, Some((edit, 1)));
    let frame: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(frame.contains("2/2 changes"));
    press_review_hit(&mut app, &hit);
    settle(&mut app);
    assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "FIRST\nkeep\nlast\n"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.editor().unwrap().rope.to_string() != "FIRST\nkeep\nlast\n" {
        app.poll_background_results();
        assert!(Instant::now() < deadline, "rejected chunk did not reload");
        std::thread::sleep(Duration::from_millis(5));
    }
    draw_review_file(&mut app, &mut terminal);
    let hit = editor_toolbar_hit(&app, true);
    assert_eq!(hit.chunk, Some((edit, 0)));
    let frame: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(frame.contains("1/1 changes"));
    press_review_hit(&mut app, &hit);
    settle(&mut app);
    draw_review_file(&mut app, &mut terminal);
    assert!(!app.editor().unwrap().ai_review.active);
    assert!(!app.pending_agent_change(&path));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "FIRST\nkeep\nlast\n"
    );
    assert_eq!(
        app.editor().unwrap().rope.to_string(),
        "FIRST\nkeep\nlast\n"
    );
}

#[test]
fn editor_ai_review_stale_toolbar_click_cannot_resolve_the_neighbor() {
    for accept in [true, false] {
        let (root, _data, mut app, _) = seeded();
        let path = root.path().join("a.txt");
        let edit = app.chat.session().unwrap().rollback.edits[0].id;
        show_review_file(&mut app, &path);
        let mut terminal = Terminal::new(TestBackend::new(80, 18)).unwrap();
        draw_review_file(&mut app, &mut terminal);
        let stale_hit = editor_toolbar_hit(&app, accept);
        assert_eq!(stale_hit.chunk, Some((edit, 0)));
        app.resolve_editor_changes(&path, Some((edit, 0)), true);
        settle(&mut app);
        // The display still refers to chunk 0, while only chunk 1 is pending.
        press_review_hit(&mut app, &stale_hit);
        settle(&mut app);
        let review = app.editor_file_review(&path).unwrap();
        assert_eq!(review.chunks.len(), 1);
        assert_eq!((review.chunks[0].edit, review.chunks[0].index), (edit, 1));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "FIRST\nkeep\nLAST\n"
        );
        assert_eq!(
            app.editor().unwrap().rope.to_string(),
            "FIRST\nkeep\nLAST\n"
        );
        draw_review_file(&mut app, &mut terminal);
        assert_eq!(editor_toolbar_hit(&app, accept).chunk, Some((edit, 1)));
    }
}

#[test]
fn editor_ai_review_toolbar_accept_preserves_typing_before_the_worker_finishes() {
    let (root, _data, mut app, _) = seeded();
    let path = root.path().join("a.txt");
    let edit = app.chat.session().unwrap().rollback.edits[0].id;
    show_review_file(&mut app, &path);
    let mut terminal = Terminal::new(TestBackend::new(80, 18)).unwrap();
    draw_review_file(&mut app, &mut terminal);
    let hit = editor_toolbar_hit(&app, true);
    press_review_hit(&mut app, &hit);
    let editor = app.editor_mut().unwrap();
    editor.set_cursor(0, 2);
    editor.paste_with_text("USER");
    let cursor = (editor.cursor_line, editor.cursor_col);
    let text = editor.rope.to_string();
    settle(&mut app);
    draw_review_file(&mut app, &mut terminal);
    assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
    assert_eq!(
        (
            app.editor().unwrap().cursor_line,
            app.editor().unwrap().cursor_col
        ),
        cursor
    );
    assert_eq!(app.editor().unwrap().rope.to_string(), text);
    assert!(app.editor().unwrap().modified);
    let review = app.editor_file_review(&path).unwrap();
    assert_eq!(review.chunks.len(), 1);
    assert_eq!((review.chunks[0].edit, review.chunks[0].index), (edit, 1));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "FIRST\nkeep\nLAST\n"
    );
}

#[test]
fn editor_ai_review_chunk_accept_advances_with_unsaved_edits_but_file_accept_does_not() {
    for control in ["inline", "toolbar", "file"] {
        let (root, _data, mut app, _) = seeded();
        let path = root.path().join("a.txt");
        let edit = app.chat.session().unwrap().rollback.edits[0].id;
        show_review_file(&mut app, &path);
        let editor = app.editor_mut().unwrap();
        editor.set_cursor(1, 0);
        editor.paste_with_text("my unsaved edit\n");
        editor.set_cursor(0, 1);
        let text = editor.rope.to_string();
        let mut terminal = Terminal::new(TestBackend::new(80, 18)).unwrap();
        draw_review_file(&mut app, &mut terminal);
        let hits = &app.editor().unwrap().ai_review.hits;
        let hit = if control == "file" {
            hits.iter().find(|hit| hit.chunk.is_none() && hit.accept)
        } else if control == "toolbar" {
            hits.iter()
                .rev()
                .find(|hit| hit.chunk == Some((edit, 0)) && hit.accept)
        } else {
            hits.iter()
                .find(|hit| hit.chunk == Some((edit, 0)) && hit.accept)
        }
        .unwrap()
        .clone();
        press_review_hit(&mut app, &hit);
        settle(&mut app);
        draw_review_file(&mut app, &mut terminal);
        assert!(app.chat.error.is_none(), "{control}: {:?}", app.chat.error);
        let editor = app.editor().unwrap();
        assert_eq!(editor.rope.to_string(), text, "{control}");
        assert!(
            editor.modified,
            "{control}: unsaved edits must stay unsaved"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "FIRST\nkeep\nLAST\n"
        );
        if control == "file" {
            assert_eq!((editor.cursor_line, editor.cursor_col), (0, 1));
            assert!(!editor.ai_review.active);
        } else {
            assert_eq!((editor.cursor_line, editor.cursor_col), (3, 0), "{control}");
            assert_eq!(editor_toolbar_hit(&app, true).chunk, Some((edit, 1)));
            let frame: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(frame.contains("1/1 changes"));
        }
    }
}

#[test]
fn editor_ai_review_file_actions_cover_other_chats_and_leave_other_files_pending() {
    for accept in [true, false] {
        let (root, _data, mut app, _) = seeded();
        let path = root.path().join("a.txt");
        let other = root.path().join("other.txt");
        {
            let mut journal = app.chat.rollback.journal.as_ref().unwrap().lock().unwrap();
            journal
                .begin(
                    "another-agent",
                    "another-chat",
                    "edit two files",
                    BTreeSet::new(),
                )
                .unwrap();
            std::fs::write(&path, "FIRST\nKEEP\nLAST\n").unwrap();
            std::fs::write(&other, "other AI change\n").unwrap();
            journal.finish(BTreeSet::new()).unwrap();
            app.chat.rollback.inline_reviews = Arc::new(journal.inline_reviews());
        }
        // There is no selected conversation; file controls still own the file.
        app.chat.selected = None;
        show_review_file(&mut app, &path);
        let mut terminal = Terminal::new(TestBackend::new(80, 22)).unwrap();
        draw_review_file(&mut app, &mut terminal);
        click_editor_review(&mut app, None, accept);
        assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            if accept {
                "FIRST\nKEEP\nLAST\n"
            } else {
                "first\nkeep\nlast\n"
            }
        );
        assert!(app.editor_file_review(&path).is_none());
        assert!(app.editor_file_review(&other).is_some());
        assert_eq!(
            std::fs::read_to_string(&other).unwrap(),
            "other AI change\n"
        );
    }
}

#[test]
fn editor_ai_review_reject_preserves_unsaved_buffers_and_reports_error_in_editor() {
    let (root, _data, mut app, _) = seeded();
    let path = root.path().join("a.txt");
    show_review_file(&mut app, &path);
    app.editor_mut()
        .unwrap()
        .paste_with_text("my unsaved work\n");
    let unsaved = app.editor().unwrap().rope.to_string();
    let mut terminal = Terminal::new(TestBackend::new(80, 22)).unwrap();
    draw_review_file(&mut app, &mut terminal);
    click_editor_review(&mut app, None, false);
    assert!(
        app.chat
            .error
            .as_ref()
            .is_some_and(|error| error.contains("unsaved"))
    );
    assert_eq!(app.editor().unwrap().rope.to_string(), unsaved);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "FIRST\nkeep\nLAST\n"
    );
    assert!(app.editor_file_review(&path).is_some());
}

#[test]
fn editor_ai_review_rejects_created_file_in_disk_and_closes_its_tab() {
    let (root, _data, mut app, _) = seeded();
    let path = root.path().join("created.txt");
    {
        let mut journal = app.chat.rollback.journal.as_ref().unwrap().lock().unwrap();
        journal
            .begin("", "chat", "create file", BTreeSet::new())
            .unwrap();
        std::fs::write(&path, "new AI file\n").unwrap();
        journal.finish(BTreeSet::new()).unwrap();
        app.chat.rollback.inline_reviews = Arc::new(journal.inline_reviews());
    }
    show_review_file(&mut app, &path);
    let mut terminal = Terminal::new(TestBackend::new(80, 18)).unwrap();
    draw_review_file(&mut app, &mut terminal);
    click_editor_review(&mut app, None, false);
    assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
    assert!(!path.exists());
    assert!(
        !app.tabs
            .iter()
            .chain(app.groups.iter().flat_map(|group| &group.tabs))
            .any(|tab| tab.editor.file_path.as_deref() == Some(path.as_path()))
    );
    assert!(app.editor_file_review(&path).is_none());
}

#[test]
fn chat_rollback_buttons_resolve_chunks_and_clear_file_markers() {
    let (root, _data, mut app, cp) = seeded();
    let path = root.path().join("a.txt");
    let id = app.chat.session().unwrap().rollback.edits[0].id;
    assert!(app.pending_agent_change(&path));
    let mut terminal = Terminal::new(TestBackend::new(130, 42)).unwrap();
    terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    assert!(
        app.chat
            .hits
            .iter()
            .any(|h| h.action == ChatAction::Picker(Picker::Changes))
    );
    assert!(
        app.chat
            .hits
            .iter()
            .any(|h| h.action == ChatAction::Picker(Picker::Message(0, 0)))
    );
    app.chat_action(ChatAction::Picker(Picker::Message(0, 0)));
    assert!(app.chat.choices.iter().any(|c| c.action == ChatAction::Picker(Picker::Restore(cp))));
    app.chat_action(ChatAction::Picker(Picker::ChangeChunk(id, 0)));
    terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    let index = app
        .chat
        .choices
        .iter()
        .position(|c| c.label == "Accept chunk")
        .unwrap();
    let hit = app
        .chat
        .hits
        .iter()
        .find(|h| h.action == ChatAction::Pick(index))
        .unwrap()
        .clone();
    app.handle_chat_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: hit.area.x,
        row: hit.area.y,
        modifiers: crossterm::event::KeyModifiers::empty(),
    });
    settle(&mut app);
    assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
    app.chat_action(ChatAction::Resolve(Selection::Chunk(id, 1), false));
    settle(&mut app);
    assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "FIRST\nkeep\nlast\n"
    );
    assert!(!app.pending_agent_change(&path));
    app.chat_action(ChatAction::Resolve(Selection::Checkpoint(cp), false));
    settle(&mut app);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "first\nkeep\nlast\n"
    );
}

#[test]
fn chat_rollback_protects_unsaved_split_buffers_and_pending_saves() {
    let (root, _data, mut app, _) = seeded();
    let path = root.path().join("a.txt");
    app.open_file(&path);
    app.editor_mut().unwrap().rope = ropey::Rope::from_str("my unsaved text");
    app.editor_mut().unwrap().modified = true;
    std::fs::write(root.path().join("other.txt"), "other").unwrap();
    app.open_file(&root.path().join("other.txt"));
    app.create_split_group(app.active_tab);
    assert!(
        app.groups
            .iter()
            .flat_map(|g| &g.tabs)
            .any(|t| t.editor.modified)
    );
    app.chat_action(ChatAction::Resolve(Selection::All, false));
    settle(&mut app);
    assert!(app.chat.error.as_ref().unwrap().contains("unsaved"));
    let protected = app
        .groups
        .iter()
        .flat_map(|g| &g.tabs)
        .find(|t| t.editor.file_path.as_ref() == Some(&path))
        .unwrap();
    assert_eq!(protected.editor.rope.to_string(), "my unsaved text");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "FIRST\nkeep\nLAST\n"
    );
    app.groups
        .iter_mut()
        .flat_map(|g| &mut g.tabs)
        .find(|t| t.editor.file_path.as_ref() == Some(&path))
        .unwrap()
        .editor
        .modified = false;
    app.pending_file_saves.insert(path.clone(), (app.workspace_generation, 1.into()));
    app.chat_action(ChatAction::Resolve(Selection::All, false));
    settle(&mut app);
    assert!(app.chat.error.as_ref().unwrap().contains("pending save"));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "FIRST\nkeep\nLAST\n"
    );
}

#[test]
fn checkpoint_open_failure_keeps_its_cause_and_retries_without_resetting_the_chat() {
    let root = tempfile::tempdir().unwrap();
    git2::Repository::init(root.path()).unwrap();
    let path = root.path().join("a.txt");
    std::fs::write(&path, "before\n").unwrap();
    let config = AgentConfig::new("Checkpoint test", "unused-test-agent");
    let mut app = App::test_new();
    app.explorer.root = root.path().into();
    app.chat.visible = true;
    app.active_pane = ActivePane::Chat;
    app.chat.agent = Some(config.clone());
    let id = p::SessionId::new("chat");
    let mut session = Session::new(id.clone());
    session.begin_prompt("first message".into());
    session.finish(None);
    app.chat.sessions.insert(id.clone(), session);
    app.chat.selected = Some(id.clone());
    app.chat.session_order.push(id.clone());
    app.chat.open_tabs.push(id.clone());
    let directory = crate::portable::workspace_dir(root.path()).join("agent-rollback");
    let mut blocker = Journal::open(root.path(), &directory).unwrap();
    let owner = app.chat.checkpoint_owner();
    let checkpoint = blocker
        .begin(&owner, "chat", "first message", BTreeSet::new())
        .unwrap();
    std::fs::write(&path, "agent edit\n").unwrap();
    blocker.finish(BTreeSet::new()).unwrap();

    app.initialize_chat_rollback(&config);
    settle(&mut app);
    let original_error = app.chat.rollback.initialization_error().unwrap().to_owned();
    assert!(
        original_error.contains("Another vtcode window"),
        "{original_error}"
    );
    assert_eq!(app.chat.error.as_deref(), Some(original_error.as_str()));
    app.apply_acp_event(vtcode::acp::Event::SessionOpened {
        session_id: id.clone(),
        modes: None,
        config_options: vec![],
        loaded: true,
    });
    assert_eq!(app.chat.error.as_deref(), Some(original_error.as_str()));
    app.chat.error = None;
    app.chat.input = "continue 🥝".into();
    app.chat.files.attached.push(path.clone());
    app.chat_action(ChatAction::Send);
    assert_eq!(app.chat.error.as_deref(), Some(original_error.as_str()));
    assert_eq!(app.chat.input, "continue 🥝");
    assert_eq!(app.chat.files.attached, [path.clone()]);
    assert!(!app.chat.session().unwrap().busy);

    // Auth support must not turn a local storage failure into a login button.
    app.chat.initialized = Some(
        serde_json::from_value(serde_json::json!({
            "protocolVersion": 1, "agentCapabilities": {},
            "authMethods": [{"id":"login", "name":"Log in"}]
        }))
        .unwrap(),
    );
    let mut terminal = Terminal::new(TestBackend::new(96, 40)).unwrap();
    terminal
        .draw(|frame| crate::ui_chat::draw(frame, &mut app, frame.area()))
        .unwrap();
    assert!(
        app.chat
            .hits
            .iter()
            .any(|h| h.action == ChatAction::RetryCheckpointStorage)
    );
    assert!(
        !app.chat
            .hits
            .iter()
            .any(|h| h.action == ChatAction::Picker(Picker::Auth))
    );
    app.chat_action(ChatAction::Picker(Picker::Changes));
    assert!(
        app.chat
            .choices
            .iter()
            .any(|c| c.action == ChatAction::RetryCheckpointStorage)
    );
    app.chat_action(ChatAction::DismissPicker);

    let counter = app.chat.rollback.file_writes.clone();
    let write = app.chat.rollback.file_request_guard(0, &id, true);
    drop(blocker);
    app.chat_action(ChatAction::RetryCheckpointStorage);
    settle(&mut app);
    assert!(app.chat.rollback.initialized && app.chat.rollback.enabled);
    assert!(app.chat.rollback.initialization_error().is_none());
    assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
    assert_eq!(app.chat.selected, Some(id));
    assert_eq!(app.chat.input, "continue 🥝");
    assert_eq!(app.chat.files.attached, [path]);
    assert_eq!(
        app.chat
            .session()
            .unwrap()
            .entries
            .iter()
            .filter(|e| matches!(e, ChatEntry::User(_)))
            .count(),
        1
    );
    assert_eq!(
        app.chat.session().unwrap().rollback.checkpoints[0].id,
        checkpoint
    );
    assert_eq!(app.chat.session().unwrap().rollback.edits[0].pending(), 1);
    assert!(Arc::ptr_eq(&counter, &app.chat.rollback.file_writes));
    assert_eq!(counter.load(Ordering::Acquire), 1);
    drop(write);
    assert_eq!(counter.load(Ordering::Acquire), 0);
}

#[test]
fn chat_rollback_non_git_workspace_disables_only_review() {
    let root = tempfile::tempdir().unwrap();
    let mut app = App::test_new();
    app.explorer.root = root.path().into();
    app.initialize_chat_rollback(&AgentConfig::new("test", "test"));
    settle(&mut app);
    assert!(app.chat.rollback.initialized);
    assert!(!app.chat.rollback.enabled);
    assert!(!app.chat.rollback.mutating);
    assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
    app.chat_action(ChatAction::Picker(Picker::Changes));
    assert!(
        app.chat.choices[0]
            .detail
            .contains("agent chat and tools still work")
    );
    git2::Repository::init(root.path()).unwrap();
    app.initialize_chat_rollback(&AgentConfig::new("test", "test"));
    settle(&mut app);
    assert!(
        app.chat.rollback.enabled,
        "reconnecting after git init enables checkpoints"
    );
    assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
}

#[test]
#[ignore = "requires official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn acp_chat_sdk_early_prompt_waits_for_storage_without_pausing_or_sending_twice() {
    let root = tempfile::tempdir().unwrap();
    git2::Repository::init(root.path()).unwrap();
    let path = root.path().join("context.txt");
    std::fs::write(&path, "Checkpoint initialization context\n").unwrap();
    let mut app = App::test_new();
    app.explorer.root = root.path().into();
    app.chat.rollback.root = root.path().into();
    let directory = crate::portable::workspace_dir(root.path()).join("agent-rollback");
    let workspace = root.path().to_path_buf();
    let (release, wait) = mpsc::channel();
    app.checkpoint_job(JobKind::Initialize, None, false, move || {
        wait.recv_timeout(Duration::from_secs(30)).unwrap();
        Journal::open(&workspace, &directory).map(|journal| Reply::Loaded(Some(Box::new(journal))))
    });
    let python = std::env::var("VTCODE_ACP_TEST_PYTHON").expect("set VTCODE_ACP_TEST_PYTHON; see tests/fixtures/acp-python-sdk/README.md");
    let mut config = AgentConfig::new("Checkpoint wait SDK", python);
    config.args = vec![
        "-u".into(),
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/acp-python-sdk/echo_agent.py")
            .to_string_lossy()
            .into_owned(),
    ];
    app.settings_effective.acp_agents = Some(vec![config]);
    app.chat_action(ChatAction::Connect(0));
    let deadline = Instant::now() + Duration::from_secs(30);
    while !app.chat.connected() || app.chat.selected.is_none() {
        app.poll_acp();
        assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
        assert!(Instant::now() < deadline, "echo agent did not connect");
        std::thread::sleep(Duration::from_millis(5));
    }
    let id = app.chat.selected.clone().unwrap();
    let text = "Early queued prompt 🥝";
    app.chat.input = text.into();
    app.chat.files.attached.push(path.clone());
    app.chat_action(ChatAction::Send);
    for _ in 0..4 {
        app.poll_acp();
    }
    let queue = &app.chat.queues[&id];
    assert_eq!(queue.items.len(), 1);
    assert_eq!(queue.items[0].text, text);
    assert_eq!(queue.items[0].files, [path]);
    assert!(!queue.paused && queue.inflight.is_none());
    assert!(!app.chat.session().unwrap().busy);
    assert!(
        !app.chat
            .session()
            .unwrap()
            .entries
            .iter()
            .any(|e| matches!(e, ChatEntry::User(_)))
    );
    assert!(app.chat.error.is_none());
    assert_eq!(app.chat.status, "Opening checkpoints…");

    release.send(()).unwrap();
    while !app.chat.queues[&id].is_empty()
        || app.chat.busy()
        || app.chat.rollback.busy()
        || app.chat.rollback.active
        || !app.chat.rollback.initialized
    {
        app.poll_acp();
        assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
        assert!(Instant::now() < deadline, "queued prompt did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        app.chat
            .session()
            .unwrap()
            .entries
            .iter()
            .filter(|e| matches!(e, ChatEntry::User(message) if message.contains(text)))
            .count(),
        1
    );
    assert!(
        app.chat
            .session()
            .unwrap()
            .entries
            .iter()
            .any(|e| matches!(e, ChatEntry::Agent(message) if message.contains(text)))
    );
    assert_eq!(app.chat.session().unwrap().rollback.checkpoints.len(), 1);
    app.chat_action(ChatAction::Disconnect);
}

#[test]
fn chat_rollback_review_has_actions_in_narrow_and_wide_themed_layouts() {
    let (_root, _data, mut app, _) = seeded();
    for theme in ["dark", "light", "high-contrast"] {
        app.set_theme_by_key(theme);
        for width in [40, 80, 140] {
            let mut terminal = Terminal::new(TestBackend::new(width, 36)).unwrap();
            app.chat_action(ChatAction::DismissPicker);
            app.chat_action(ChatAction::Picker(Picker::Changes));
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            if let Some(directory) = std::env::var_os("VTCODE_ROLLBACK_CAPTURE") {
                let directory = PathBuf::from(directory);
                std::fs::create_dir_all(&directory).unwrap();
                let cells: Vec<_> = terminal.backend().buffer().content.iter().map(|cell| serde_json::json!({"text":cell.symbol(),"fg":format!("{:?}",cell.fg),"bg":format!("{:?}",cell.bg)})).collect();
                std::fs::write(
                    directory.join(format!("rollback-{theme}-{width}.json")),
                    serde_json::to_vec(
                        &serde_json::json!({"width":width,"height":36,"cells":cells}),
                    )
                    .unwrap(),
                )
                .unwrap();
            }
            assert!(
                app.chat
                    .hits
                    .iter()
                    .any(|h| matches!(h.action, ChatAction::Pick(_))),
                "{theme} {width}"
            );
            assert!(
                app.chat
                    .choices
                    .iter()
                    .any(|c| matches!(c.action, ChatAction::Resolve(Selection::All, true)))
            );
            assert!(
                app.chat
                    .choices
                    .iter()
                    .any(|c| matches!(c.action, ChatAction::Resolve(Selection::All, false)))
            );
        }
    }
}

#[test]
fn chat_rollback_ignores_transient_and_stale_watcher_deletions() {
    let (root, _data, mut app, _) = seeded();
    let path = root.path().join("a.txt");
    app.open_file(&path);
    let observed_at = Instant::now();
    app.chat.rollback.write_paths = vec![app.chat.rollback.path_key(&path)];
    let missing = crate::background::WorkspacePathChange {
        path: path.clone(),
        observed_at,
        exists: false,
        modified: None,
        len: None,
    };
    app.background
        .sender
        .send(crate::background::BackgroundResult::FileDiskState(
            missing.clone(),
        ))
        .unwrap();
    app.poll_background_results();
    assert!(!app.editor().unwrap().modified);
    assert!(!app.editor().unwrap().disk_unavailable);
    app.chat.rollback.write_paths.clear();
    app.chat
        .rollback
        .disk_barriers
        .insert(app.chat.rollback.path_key(&path), Instant::now());
    app.background
        .sender
        .send(crate::background::BackgroundResult::FileDiskState(missing))
        .unwrap();
    app.poll_background_results();
    assert!(
        !app.editor().unwrap().modified,
        "an old missing-path observation cannot turn a restored buffer into unsaved content"
    );
    let current = crate::background::WorkspacePathChange {
        path,
        observed_at: Instant::now(),
        exists: false,
        modified: None,
        len: None,
    };
    assert!(
        !app.chat.rollback.defer_disk_change(&current),
        "later real deletions must still be handled"
    );
}

#[test]
#[ignore = "requires official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn acp_chat_sdk_round_trip_rollback_shell_and_no_git() {
    for git in [true, false] {
        let root = tempfile::tempdir().unwrap();
        if git {
            git2::Repository::init(root.path()).unwrap();
        }
        std::fs::write(root.path().join("shell.txt"), "first\nkeep\nlast\n").unwrap();
        let mut app = App::test_new();
        app.explorer.root = root.path().into();
        let python = std::env::var("VTCODE_ACP_TEST_PYTHON").expect("set VTCODE_ACP_TEST_PYTHON; see tests/fixtures/acp-python-sdk/README.md");
        let mut config = AgentConfig::new("Rollback SDK", python);
        config.args = vec![
            "-u".into(),
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/scenario_agent.py")
                .to_string_lossy()
                .into_owned(),
        ];
        app.settings_effective.acp_agents = Some(vec![config]);
        app.chat_action(ChatAction::Connect(0));
        let deadline = Instant::now() + Duration::from_secs(30);
        while app.chat.selected.is_none() || app.chat.rollback.busy() {
            app.poll_acp();
            assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        app.chat.input = "rollback-shell".into();
        app.chat_action(ChatAction::Send);
        while app.chat.busy() || app.chat.rollback.busy() || app.chat.rollback.active {
            app.poll_acp();
            assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
            assert!(
                Instant::now() < deadline,
                "git={git} session_busy={} checkpoint_job={} active={} preparing={:?} writes={} status={} session={:?} log={:?} file={:?}",
                app.chat.busy(),
                app.chat.rollback.busy(),
                app.chat.rollback.active,
                app.chat.rollback.turns,
                app.chat.rollback.file_writes.load(Ordering::Acquire),
                app.chat.status,
                app.chat.session(),
                app.chat.diagnostics,
                std::fs::read_to_string(root.path().join("shell.txt"))
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            std::fs::read_to_string(root.path().join("shell.txt")).unwrap(),
            "FIRST\nkeep\nLAST\n"
        );
        assert_eq!(app.chat.rollback.enabled, git);
        if git {
            let summary = &app.chat.session().unwrap().rollback;
            assert_eq!(summary.checkpoints.len(), 1);
            assert_eq!(summary.edits.len(), 2);
            let cp = summary.checkpoints[0].id;
            app.chat_action(ChatAction::Resolve(Selection::All, true));
            settle(&mut app);
            app.chat_action(ChatAction::Resolve(Selection::Checkpoint(cp), false));
            settle(&mut app);
            assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
            assert_eq!(
                std::fs::read_to_string(root.path().join("shell.txt")).unwrap(),
                "first\nkeep\nlast\n"
            );
            assert!(!root.path().join("created.txt").exists());
        }
        app.chat_action(ChatAction::Disconnect);
    }
}


#[test]
fn checkpoint_finishing_waits_only_for_its_own_file_writes() {
    let (_root, _data, mut app, _) = seeded();
    let first = p::SessionId::new("chat");
    let second = p::SessionId::new("second");
    app.chat.sessions.insert(second.clone(), Session::new(second.clone()));
    for id in [&first, &second] {
        let cp = app.chat.rollback.journal.as_ref().unwrap().lock().unwrap()
            .begin("", &id.to_string(), "new message", BTreeSet::new()).unwrap();
        app.chat.rollback.turns.insert((0, id.clone()), TurnCapture { checkpoint: Some(cp), ..Default::default() });
    }
    let write = app.chat.rollback.file_request_guard(0, &first, true);
    // Identical session IDs belonging to other agents have independent write counters.
    let other_agent_write = app.chat.rollback.file_request_guard(1, &first, true);
    app.refresh_chat_rollback();
    settle(&mut app);
    assert!(app.chat.sessions[&second].rollback.active.is_none());
    assert!(app.chat.sessions[&first].rollback.active.is_some());
    drop(write);
    settle(&mut app);
    assert!(app.chat.sessions[&first].rollback.active.is_none());
    assert!(!app.chat.rollback.conversation_busy(0, Some(&first)));
    assert!(app.chat.rollback.conversation_busy(1, Some(&first)));
    assert_eq!(app.chat.rollback.file_writes.load(Ordering::Acquire), 1);
    drop(other_agent_write);
    assert_eq!(app.chat.rollback.file_writes.load(Ordering::Acquire), 0);
}

#[test]
fn close_agent_waits_for_file_writes_and_keeps_the_finished_checkpoint() {
    let (root, _data, mut app, _) = seeded();
    let id = p::SessionId::new("chat");
    app.chat.agent_order = vec![0];
    let journal = app.chat.rollback.journal.as_ref().unwrap().clone();
    let cp = journal.lock().unwrap().begin("", "chat", "another change", BTreeSet::new()).unwrap();
    app.chat.rollback.turns.insert((0, id.clone()), TurnCapture { checkpoint: Some(cp), ..Default::default() });
    app.chat.session_mut().unwrap().busy = true;
    let write = app.chat.rollback.file_request_guard(0, &id, true);
    app.chat_action(ChatAction::CloseAgent(0));
    app.confirm_close_chat_agent();
    app.poll_acp();
    assert!(app.chat.closing);
    assert!(app.chat.agent_state(0).is_some());
    std::fs::write(root.path().join("a.txt"), "A final file write\n").unwrap();
    drop(write);
    settle(&mut app);
    assert!(app.chat.agent_state(0).is_none());
    let journal = journal.lock().unwrap();
    assert!(journal.active().is_none());
    let summary = journal.summary("", "chat");
    assert!(summary.checkpoints.iter().any(|checkpoint| checkpoint.id == cp));
    assert!(!summary.edits.is_empty());
    assert_eq!(std::fs::read_to_string(root.path().join("a.txt")).unwrap(), "A final file write\n");
}

#[test]
fn close_agent_keeps_failed_checkpoints_available_for_retry() {
    let (_root, _data, mut app, _) = seeded();
    let id = p::SessionId::new("chat");
    app.chat.agent_order = vec![0];
    let cp = app.chat.rollback.journal.as_ref().unwrap().lock().unwrap()
        .begin("", "chat", "another change", BTreeSet::new()).unwrap();
    app.chat.rollback.turns.insert((0, id), TurnCapture { checkpoint: Some(cp), failed: true, ..Default::default() });
    app.chat_action(ChatAction::CloseAgent(0));
    app.confirm_close_chat_agent();
    assert!(app.chat.agent_state(0).is_some());
    assert!(!app.chat.closing);
    assert!(app.chat.error.as_ref().unwrap().contains("Retry the checkpoint"));
    app.chat_action(ChatAction::RetryCheckpoint);
    settle(&mut app);
    app.chat_action(ChatAction::CloseAgent(0));
    app.confirm_close_chat_agent();
    assert!(app.chat.agent_state(0).is_none());
}

#[test]
fn close_agent_waits_for_its_queued_journal_jobs_behind_another_agent() {
    let (_root, _data, mut app, _) = seeded();
    app.chat.agent_order = vec![0, 1];
    app.chat.connections.insert(1, Default::default());
    let (release, wait) = mpsc::channel();
    app.with_agent_context(1, true, |app| {
        app.checkpoint_job(JobKind::Review, None, false, move || {
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(Reply::Accepted)
        });
    });
    app.checkpoint_job(JobKind::Review, None, false, || Ok(Reply::Accepted));
    app.chat_action(ChatAction::CloseAgent(0));
    app.confirm_close_chat_agent();
    assert!(app.chat.agent_state(0).is_some());
    assert!(app.chat.closing);
    release.send(()).unwrap();
    settle(&mut app);
    assert!(app.chat.agent_state(0).is_none());
    assert_eq!(app.chat.agent_order, vec![1]);
}
