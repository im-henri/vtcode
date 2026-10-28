use super::*;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use vtcode::acp::{self, Command};

struct Sink(Arc<Mutex<Vec<Command>>>);
impl acp::CommandSink for Sink {
    fn send(&self, command: Command, _: bool) -> Result<(), String> {
        self.0.lock().unwrap().push(command);
        Ok(())
    }
    fn disconnect(&self) {}
}
struct Fixture {
    app: App,
    root: tempfile::TempDir,
    commands: Arc<Mutex<Vec<Command>>>,
    _events: mpsc::Sender<acp::HostedEvent>,
    hold: Arc<AtomicBool>,
    traffic: Arc<AtomicUsize>,
    blocked: Arc<AtomicBool>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.hold.store(false, Ordering::Release);
    }
}

fn setup() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let hold = Arc::new(AtomicBool::new(false));
    let traffic = Arc::new(AtomicUsize::new(0));
    let blocked = Arc::new(AtomicBool::new(false));
    let workspace = remote::workspace::Workspace::new(remote::tests::connection_with_status(
        root.path(),
        hold.clone(),
        traffic.clone(),
        blocked.clone(),
    ));
    let commands = Arc::new(Mutex::new(Vec::new()));
    let (events, receiver) = mpsc::channel();
    let client = acp::Client::hosted(
        acp::Workspace::new(root.path()).unwrap(),
        Arc::new(Sink(commands.clone())),
        receiver,
    );
    let mut app = App::test_new();
    app.explorer.root = workspace.view_root.clone();
    app.chat.workspace = workspace.view_root.clone();
    app.remote_workspace = Some(workspace);
    app.chat.client = Some(client);
    app.chat.initialized = Some(
        serde_json::from_value(serde_json::json!({"protocolVersion":1,"agentCapabilities":{}}))
            .unwrap(),
    );
    let id = p::SessionId::new("context-session");
    app.chat
        .sessions
        .insert(id.clone(), acp::Session::new(id.clone()));
    app.chat.selected = Some(id.clone());
    app.chat.session_order.push(id);
    app.chat.rollback.enabled = false;
    app.focus_chat();
    Fixture {
        app,
        root,
        commands,
        _events: events,
        hold,
        traffic,
        blocked,
    }
}
fn file(fixture: &Fixture, name: &str, text: &str) -> PathBuf {
    let path = fixture.root.path().join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    fixture
        .app
        .remote_workspace
        .as_ref()
        .unwrap()
        .target_key(name)
        .unwrap()
}
fn wait(fixture: &mut Fixture, condition: impl Fn(&Fixture) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(12);
    while !condition(fixture) {
        fixture.app.poll_background_results();
        fixture.app.poll_acp();
        assert!(
            Instant::now() < deadline,
            "remote context stalled: {:?}",
            fixture.app.chat.error
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn buffer(path: PathBuf, identity: Option<PathBuf>, text: &str) -> Buffer {
    Buffer {
        path,
        identity,
        rope: ropey::Rope::from_str(text),
        modified: true,
        loading: false,
    }
}

#[test]
fn remote_pasted_paths_use_whole_path_priority_and_complete_quoted_file_lists() {
    let fixture = setup();
    let first = file(&fixture, "one", "one");
    let second = file(&fixture, "two", "two");
    let space = file(&fixture, "space name λ.txt", "space");
    std::fs::create_dir(fixture.root.path().join("directory")).unwrap();
    let workspace = fixture.app.remote_workspace.as_ref().unwrap();
    let mut cache = PreparationCache::default();
    let cancel = CancellationToken::new();
    for (text, expected) in [
        ("one two", Some(vec![first.clone(), second.clone()])),
        ("\"space name λ.txt\" one", Some(vec![space.clone(), first.clone()])),
        ("space\\ name\\ λ.txt", Some(vec![space.clone()])),
        ("one missing", None),
        ("directory", None),
        ("\"one\"two", None),
        ("\"unterminated", None),
        ("  ", None),
    ] {
        assert_eq!(pasted_remote_paths(workspace, text, &mut cache, &cancel).unwrap(), expected, "{text:?}");
    }
    let combined = file(&fixture, "one two", "whole wins");
    assert_eq!(pasted_remote_paths(workspace, "one two", &mut PreparationCache::default(), &cancel).unwrap(), Some(vec![combined]));
    let uri = url::Url::from_file_path(fixture.root.path().join("space name λ.txt")).unwrap();
    assert_eq!(pasted_remote_paths(workspace, uri.as_str(), &mut cache, &cancel).unwrap(), Some(vec![space]));
}

#[test]
fn paste_qualification_and_validation_reuse_inspection_without_cross_action_staleness() {
    use std::sync::atomic::AtomicUsize;
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("file.rs"), "disk").unwrap();
    let traffic = Arc::new(AtomicUsize::new(0));
    let workspace = remote::workspace::Workspace::new(remote::tests::connection(root.path(), Arc::default(), traffic.clone()));
    let path = workspace.target_key("file.rs").unwrap();
    let cancel = CancellationToken::new();
    let mut cache = PreparationCache::default();
    let paths = pasted_remote_paths(&workspace, "file.rs", &mut cache, &cancel).unwrap().unwrap();
    let inspected = traffic.load(Ordering::Relaxed);
    let buffers = vec![buffer(path.clone(), None, "unsaved current text")];
    let prepared = prepare_files_cached(&workspace, paths.clone(), &buffers, false, &cancel, &mut cache).unwrap();
    assert_eq!(prepared.files.len(), 1);
    assert_eq!(traffic.load(Ordering::Relaxed), inspected, "qualification already inspected the direct buffer");
    prepare_files_cached(&workspace, paths, &buffers, false, &cancel, &mut cache).unwrap();
    assert_eq!(traffic.load(Ordering::Relaxed), inspected, "a batch must not inspect or validate an unchanged frozen buffer twice");
    std::fs::remove_file(root.path().join("file.rs")).unwrap();
    std::fs::create_dir(root.path().join("file.rs")).unwrap();
    assert!(pasted_remote_paths(&workspace, "file.rs", &mut PreparationCache::default(), &cancel).unwrap().is_none());
    assert!(traffic.load(Ordering::Relaxed) > inspected, "a new action must inspect current node state");
    workspace.client.call(remote::Command::Shutdown).unwrap();
}

fn late_prompt() -> PreparedPrompt {
    PreparedPrompt {
        content: vec![],
        display: "late".into(),
        source: None,
    }
}

#[test]
fn remote_file_paste_replays_selection_and_continued_typing_without_waiting() {
    let mut fixture = setup();
    let path = file(&fixture, "source.rs", "small context");
    fixture.app.chat.input = "keep selected tail".into();
    fixture.app.chat.cursor = 13;
    fixture.app.chat.anchor = Some(5);
    fixture.hold.store(true, Ordering::Release);
    fixture.app.edit_chat_input(KeyCode::Null, KeyModifiers::NONE, Some("source.rs"));
    fixture.app.edit_chat_input(KeyCode::Char('λ'), KeyModifiers::NONE, None);
    assert_eq!(fixture.app.chat.input, "keep source.rsλ tail");
    assert_eq!(fixture.app.chat.files.pasting.len(), 1);
    fixture.hold.store(false, Ordering::Release);
    wait(&mut fixture, |fixture| fixture.app.chat.files.pasting.is_empty());
    assert_eq!(fixture.app.chat.input, "keep λ tail");
    assert_eq!(fixture.app.chat.files.attached, [path]);
    fixture.app.edit_chat_input(KeyCode::Char('z'), KeyModifiers::CONTROL, None);
    assert_eq!(fixture.app.chat.input, "keep selected tail");
    assert_eq!((fixture.app.chat.cursor, fixture.app.chat.anchor), (13, Some(5)));
    fixture.app.edit_chat_input(KeyCode::Char('y'), KeyModifiers::CONTROL, None);
    assert_eq!(fixture.app.chat.input, "keep λ tail");
}

#[test]
fn remote_plain_text_and_bad_file_pastes_keep_native_draft_and_error_behavior() {
    let mut fixture = setup();
    file(&fixture, "binary.bin", "\0binary");
    fixture.app.chat.input = "original".into();
    fixture.app.chat.cursor = 8;
    fixture.app.chat.error = Some("prior error".into());
    fixture.app.edit_chat_input(KeyCode::Null, KeyModifiers::NONE, Some(" plain missing path "));
    wait(&mut fixture, |fixture| fixture.app.chat.files.pasting.is_empty());
    assert_eq!(fixture.app.chat.input, "original plain missing path ");
    assert_eq!(fixture.app.chat.error.as_deref(), Some("prior error"));
    fixture.app.edit_chat_input(KeyCode::Char('z'), KeyModifiers::CONTROL, None);
    assert_eq!(fixture.app.chat.input, "original");
    fixture.app.chat.anchor = Some(0);
    fixture.app.edit_chat_input(KeyCode::Null, KeyModifiers::NONE, Some("binary.bin"));
    wait(&mut fixture, |fixture| fixture.app.chat.files.pasting.is_empty());
    assert_eq!(fixture.app.chat.input, "original");
    assert_eq!(fixture.app.chat.anchor, Some(0));
    assert!(fixture.app.chat.files.attached.is_empty());
    assert!(fixture.app.chat.error.as_ref().unwrap().contains("inary"));
}

#[test]
fn rapid_remote_file_pastes_use_one_worker_and_preserve_all_accepted_files() {
    let mut fixture = setup();
    let first = file(&fixture, "first.rs", "first");
    let second = file(&fixture, "second.rs", "second");
    fixture.hold.store(true, Ordering::Release);
    for text in ["first.rs", "second.rs", " ordinary text "] {
        fixture.app.edit_chat_input(KeyCode::Null, KeyModifiers::NONE, Some(text));
        assert_eq!(fixture.app.chat.files.pasting.len(), 1);
    }
    fixture.app.edit_chat_input(KeyCode::Char('x'), KeyModifiers::NONE, None);
    fixture.hold.store(false, Ordering::Release);
    wait(&mut fixture, |fixture| fixture.app.chat.files.pasting.is_empty());
    assert_eq!(fixture.app.chat.input, " ordinary text x");
    assert_eq!(fixture.app.chat.files.attached, [first, second]);
}

#[test]
fn typing_during_a_stalled_paste_never_submits_more_metadata_or_workers() {
    let mut fixture = setup();
    let path = file(&fixture, "source.rs", "context");
    fixture.hold.store(true, Ordering::Release);
    fixture.app.edit_chat_input(KeyCode::Null, KeyModifiers::NONE, Some("source.rs"));
    let end = Instant::now() + Duration::from_secs(5);
    while !fixture.blocked.load(Ordering::Acquire) {
        assert!(Instant::now() < end);
        std::thread::yield_now();
    }
    let traffic = fixture.traffic.load(Ordering::Relaxed);
    let serial = fixture.app.chat.files.work_serial;
    for _ in 0..256 {
        fixture.app.edit_chat_input(KeyCode::Char('λ'), KeyModifiers::NONE, None);
        fixture.app.poll_background_results();
        assert_eq!(fixture.app.chat.files.pasting.len(), 1);
    }
    assert_eq!(fixture.traffic.load(Ordering::Relaxed), traffic, "no per-keystroke remote requests");
    assert_eq!(fixture.app.chat.files.work_serial, serial, "one pending worker, no per-key task spawning");
    fixture.hold.store(false, Ordering::Release);
    wait(&mut fixture, |fixture| fixture.app.chat.files.pasting.is_empty());
    assert_eq!(fixture.app.chat.input, "λ".repeat(256));
    assert_eq!(fixture.app.chat.files.attached, [path]);
}

#[test]
fn remote_file_paste_then_immediate_send_uses_file_context_and_preserves_new_draft() {
    let mut fixture = setup();
    file(&fixture, "source.rs", "disk content");
    fixture.hold.store(true, Ordering::Release);
    fixture.app.edit_chat_input(KeyCode::Null, KeyModifiers::NONE, Some("source.rs"));
    fixture.app.chat_action(crate::app_acp::ChatAction::Send);
    fixture.app.edit_chat_input(KeyCode::Null, KeyModifiers::NONE, Some("next draft"));
    fixture.hold.store(false, Ordering::Release);
    wait(&mut fixture, |fixture| fixture.commands.lock().unwrap().iter().any(|command| matches!(command, Command::Prompt { .. })));
    assert_eq!(fixture.app.chat.input, "next draft");
    let commands = fixture.commands.lock().unwrap();
    let Command::Prompt { content, .. } = commands.iter().find(|command| matches!(command, Command::Prompt { .. })).unwrap() else { unreachable!() };
    assert_eq!(content.len(), 2, "the pasted file must not be sent as plain path text");
    let context = serde_json::to_string(&content[1]).unwrap();
    assert!(context.contains("source.rs"));
    assert!(!context.contains("disk content"), "closed-file text stays on the node until the agent consumes its reference");
}

#[test]
fn immediate_file_paste_send_uses_the_local_buffer_captured_at_submission() {
    let mut fixture = setup();
    let path = file(&fixture, "source.rs", "disk content");
    fixture.app.new_file();
    let editor = fixture.app.editor_mut().unwrap();
    editor.file_path = Some(path);
    editor.paste_with_text("unsaved at submit");
    fixture.app.focus_chat();
    fixture.hold.store(true, Ordering::Release);
    fixture.app.edit_chat_input(KeyCode::Null, KeyModifiers::NONE, Some("source.rs"));
    fixture.app.chat_action(crate::app_acp::ChatAction::Send);
    fixture.app.editor_mut().unwrap().paste_with_text(" later editing");
    fixture.hold.store(false, Ordering::Release);
    wait(&mut fixture, |fixture| fixture.commands.lock().unwrap().iter().any(|command| matches!(command, Command::Prompt { .. })));
    let commands = fixture.commands.lock().unwrap();
    let Command::Prompt { content, .. } = commands.iter().find(|command| matches!(command, Command::Prompt { .. })).unwrap() else { unreachable!() };
    let p::ContentBlock::Text(context) = &content[1] else { panic!("local text fallback"); };
    assert!(context.text.contains("unsaved at submit"));
    assert!(!context.text.contains("later editing"));
    assert!(!context.text.contains("disk content"));
}

#[test]
fn rejected_file_only_paste_does_not_send_an_empty_prompt_or_lose_undo() {
    let mut fixture = setup();
    file(&fixture, "binary.bin", "\0binary");
    fixture.hold.store(true, Ordering::Release);
    fixture.app.edit_chat_input(KeyCode::Null, KeyModifiers::NONE, Some("binary.bin"));
    fixture.app.chat_action(crate::app_acp::ChatAction::Send);
    fixture.hold.store(false, Ordering::Release);
    wait(&mut fixture, |fixture| fixture.app.chat.files.preparing.is_empty());
    assert!(fixture.commands.lock().unwrap().iter().all(|command| !matches!(command, Command::Prompt { .. })));
    assert!(fixture.app.chat.input.is_empty());
    assert!(fixture.app.chat.files.attached.is_empty());
    assert!(fixture.app.chat.error.as_ref().unwrap().contains("inary"));
    assert!(!fixture.app.chat.has_queued_prompt());
}

#[test]
fn paste_resolution_updates_saved_composer_without_touching_auxiliary_input() {
    let mut fixture = setup();
    let path = file(&fixture, "source.rs", "context");
    fixture.app.chat.input = "draft ".into();
    fixture.app.chat.cursor = 6;
    fixture.hold.store(true, Ordering::Release);
    fixture.app.edit_chat_input(KeyCode::Null, KeyModifiers::NONE, Some("source.rs"));
    fixture.app.chat_action(crate::app_acp::ChatAction::LoadById);
    fixture.app.edit_chat_input(KeyCode::Null, KeyModifiers::NONE, Some("session-id"));
    fixture.hold.store(false, Ordering::Release);
    wait(&mut fixture, |fixture| fixture.app.chat.files.pasting.is_empty());
    assert_eq!(fixture.app.chat.input, "session-id");
    fixture.app.chat_action(crate::app_acp::ChatAction::BackToChat);
    assert_eq!(fixture.app.chat.input, "draft ");
    assert_eq!(fixture.app.chat.files.attached, [path]);
}

#[test]
fn delayed_paste_resolves_only_its_conversation_and_ignores_a_replaced_draft() {
    let mut fixture = setup();
    let path = file(&fixture, "source.rs", "context");
    let first = fixture.app.chat.selected.clone().unwrap();
    let other = p::SessionId::new("another");
    fixture.app.chat.sessions.insert(other.clone(), acp::Session::new(other.clone()));
    fixture.hold.store(true, Ordering::Release);
    fixture.app.edit_chat_input(KeyCode::Null, KeyModifiers::NONE, Some("source.rs"));
    fixture.app.chat.select_view(Some(other.clone()));
    fixture.app.chat.input = "other draft".into();
    fixture.app.chat.cursor = 11;
    fixture.hold.store(false, Ordering::Release);
    wait(&mut fixture, |fixture| fixture.app.chat.files.pasting.is_empty());
    assert_eq!(fixture.app.chat.input, "other draft");
    assert!(fixture.app.chat.files.attached.is_empty());
    fixture.app.chat.select_view(Some(first));
    assert!(fixture.app.chat.input.is_empty());
    assert_eq!(fixture.app.chat.files.attached, [path]);
    fixture.hold.store(true, Ordering::Release);
    fixture.app.edit_chat_input(KeyCode::Null, KeyModifiers::NONE, Some("source.rs"));
    fixture.app.chat.input = "replacement".into();
    fixture.app.chat.cursor = 11;
    fixture.hold.store(false, Ordering::Release);
    wait(&mut fixture, |fixture| fixture.app.chat.files.pasting.is_empty());
    assert_eq!(fixture.app.chat.input, "replacement");
}

#[test]
fn explicit_attachment_result_keeps_an_attachment_added_by_a_paste_while_pending() {
    let mut fixture = setup();
    let pasted = file(&fixture, "pasted.rs", "first");
    let explicit = file(&fixture, "explicit.rs", "second");
    fixture.hold.store(true, Ordering::Release);
    fixture.app.attach_chat_files(vec![explicit.clone()]);
    let serial = fixture.app.chat.files.attaching.values().next().unwrap().serial;
    // Model the paste's atomic commit before the earlier explicit choice is
    // delivered. Its base changed, but the requested new file is still wanted.
    fixture.app.chat.files.attached = vec![pasted.clone()];
    fixture.app.apply_remote_chat_attachments(serial, Ok(vec![explicit.clone()]));
    fixture.hold.store(false, Ordering::Release);
    wait(&mut fixture, |fixture| fixture.app.chat.files.attaching.is_empty());
    assert_eq!(fixture.app.chat.files.attached, [pasted, explicit]);
}

#[test]
fn remote_context_reads_disk_and_latest_local_rope_without_native_path_io() {
    let fixture = setup();
    let first = file(&fixture, "src/file.rs", "disk version");
    let second = file(&fixture, "guide space.md", "# disk guide\r\n");
    let workspace = fixture.app.remote_workspace.as_ref().unwrap();
    let buffers = vec![buffer(first.clone(), None, "unsaved λ\r\n```\n")];
    let prepared = prompt(
        workspace,
        "compare @src/file.rs".into(),
        vec![first.clone(), second],
        buffers,
        false,
        &CancellationToken::new(),
    )
    .unwrap();
    let (blocks, display) = (prepared.content, prepared.display);
    assert_eq!(blocks.len(), 3);
    let p::ContentBlock::Text(block) = &blocks[1] else {
        panic!("text fallback");
    };
    let json: serde_json::Value =
        serde_json::from_str(block.text.split_once('\n').unwrap().1).unwrap();
    assert_eq!(json["text"], "unsaved λ\r\n```\n");
    assert!(!json["path"].as_str().unwrap().contains('\0'));
    assert!(!json["path"].as_str().unwrap().contains("vtcode-remote-"));
    assert!(display.contains("📎 src/file.rs"));
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("src/file.rs")).unwrap(),
        "disk version"
    );
}

#[test]
fn remote_context_handles_urls_and_embedded_resource_identity() {
    let fixture = setup();
    let path = file(&fixture, "Hello 世界.md", "text");
    let workspace = fixture.app.remote_workspace.as_ref().unwrap();
    let node_uri = url::Url::from_file_path(fixture.root.path().join("Hello 世界.md")).unwrap();
    assert_eq!(resolve(workspace, node_uri.as_str()).unwrap(), path);
    let prepared = prompt(
        workspace,
        "inspect".into(),
        vec![path.clone()],
        vec![],
        true,
        &CancellationToken::new(),
    )
    .unwrap();
    let p::ContentBlock::ResourceLink(link) = &prepared.content[1] else {
        panic!("frozen node reference")
    };
    assert_eq!(link.name, "Hello 世界.md");
    assert!(link.uri.starts_with("vtcode-context:"));
    assert!(link.uri.ends_with(":0:1"));
    assert!(prepared.source.is_some());
    assert!(resolve(workspace, "file://other-host/share/file.rs").is_err());
}

#[test]
fn remote_context_completion_includes_hidden_files_and_skips_build_trees() {
    let mut fixture = setup();
    let hidden = file(&fixture, ".config/settings.toml", "a=1");
    file(&fixture, "target/settings.toml", "must not match");
    fixture.app.chat.input = "@settings".into();
    fixture.app.chat.cursor = fixture.app.chat.input.len();
    fixture.app.refresh_chat_file_completion();
    wait(&mut fixture, |fixture| {
        fixture
            .app
            .chat
            .files
            .completion
            .as_ref()
            .is_some_and(|completion| !completion.searching)
    });
    assert_eq!(
        fixture.app.chat.files.completion.as_ref().unwrap().paths,
        [hidden]
    );
    let root = fixture.app.remote_workspace.as_ref().unwrap();
    let remote::Reply::Files {
        files: Some(files), ..
    } = root
        .client
        .call(remote::Command::Files {
            generation: 0,
            known_version: None,
            excludes: ["**/.config", "**/target"].into_iter()
                .map(|pattern| (pattern.into(), serde_json::json!(true))).collect(),
        })
        .unwrap()
    else {
        panic!("file index");
    };
    assert!(
        files.is_empty(),
        "sharing a richer chat index must respect Quick Open's configured exclusions"
    );
}

#[test]
fn remote_context_attachment_batches_are_atomic_and_stale_completion_is_ignored() {
    let mut fixture = setup();
    let good = file(&fixture, "good.txt", "good");
    assert!(fixture.app.attach_chat_files(vec![good.clone()]));
    wait(&mut fixture, |fixture| {
        fixture.app.chat.files.attaching.is_empty()
    });
    assert_eq!(fixture.app.chat.files.attached, [good.clone()]);
    let bad = file(&fixture, "binary.bin", "binary\0data");
    let other = file(&fixture, "other.txt", "other");
    assert!(fixture.app.attach_chat_files(vec![other, bad]));
    wait(&mut fixture, |fixture| {
        fixture.app.chat.files.attaching.is_empty()
    });
    assert_eq!(fixture.app.chat.files.attached, [good]);
    assert!(
        fixture
            .app
            .chat
            .error
            .as_deref()
            .unwrap()
            .contains("binary")
    );
    assert!(
        !fixture
            .app
            .apply_remote_chat_attachments(u64::MAX, Ok(vec![]))
    );
}

#[test]
fn remote_context_delayed_attachment_does_not_block_editing_or_rendering() {
    let mut fixture = setup();
    let path = file(&fixture, "slow.txt", "small context");
    fixture.hold.store(true, Ordering::Release);
    let started = Instant::now();
    fixture.app.attach_chat_files(vec![path]);
    fixture.app.new_file();
    fixture
        .app
        .editor_mut()
        .unwrap()
        .paste_with_text("locally responsive λ\n");
    fixture.app.editor_mut().unwrap().undo();
    fixture.app.editor_mut().unwrap().redo();
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut fixture.app))
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(fixture.app.chat.files.attaching.len(), 1);
    fixture.hold.store(false, Ordering::Release);
    wait(&mut fixture, |fixture| {
        fixture.app.chat.files.attaching.is_empty()
    });
    assert_eq!(
        fixture.app.editor().unwrap().rope.to_string(),
        "locally responsive λ\n"
    );
}

#[test]
fn remote_context_prompt_preserves_new_draft_and_uses_send_time_buffer_snapshot() {
    let mut fixture = setup();
    let path = file(&fixture, "source.rs", "disk");
    fixture.app.new_file();
    let editor = fixture.app.editor_mut().unwrap();
    editor.file_path = Some(path.clone());
    editor.paste_with_text("unsaved at submit");
    fixture.app.focus_chat();
    fixture.app.chat.input = "check this".into();
    fixture.app.chat.files.attached = vec![path];
    fixture.hold.store(true, Ordering::Release);
    fixture.app.chat_action(crate::app_acp::ChatAction::Send);
    assert!(fixture.app.chat.busy());
    assert!(fixture.app.chat.input.is_empty());
    fixture.app.chat.input = "next draft".into();
    fixture
        .app
        .editor_mut()
        .unwrap()
        .paste_with_text(" later editing");
    fixture.hold.store(false, Ordering::Release);
    wait(&mut fixture, |fixture| {
        fixture
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|command| matches!(command, Command::Prompt { .. }))
    });
    assert_eq!(fixture.app.chat.input, "next draft");
    let commands = fixture.commands.lock().unwrap();
    let Command::Prompt { content, .. } = commands
        .iter()
        .find(|command| matches!(command, Command::Prompt { .. }))
        .unwrap()
    else {
        unreachable!();
    };
    let p::ContentBlock::Text(context) = &content[1] else {
        panic!("text context");
    };
    assert!(context.text.contains("unsaved at submit"));
    assert!(!context.text.contains("later editing"));
}

#[test]
fn remote_context_failed_direct_send_restores_draft_and_files() {
    let mut fixture = setup();
    let missing = fixture
        .app
        .remote_workspace
        .as_ref()
        .unwrap()
        .target_key("missing.txt")
        .unwrap();
    fixture.app.chat.input = "original draft".into();
    fixture.app.chat.files.attached = vec![missing.clone()];
    fixture.app.chat_action(crate::app_acp::ChatAction::Send);
    wait(&mut fixture, |fixture| {
        fixture.app.chat.files.preparing.is_empty()
    });
    assert_eq!(fixture.app.chat.input, "original draft");
    assert_eq!(fixture.app.chat.files.attached, [missing]);
    assert!(
        fixture
            .app
            .chat
            .error
            .as_deref()
            .unwrap()
            .contains("not sent")
    );
    assert!(
        !fixture
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|command| matches!(command, Command::Prompt { .. }))
    );
}

#[test]
fn remote_context_stop_retains_queued_prompt_and_rejects_late_reply() {
    let mut fixture = setup();
    let path = file(&fixture, "source.rs", "disk");
    fixture.hold.store(true, Ordering::Release);
    fixture.app.chat.input = "stop this".into();
    fixture.app.chat.files.attached = vec![path];
    fixture.app.chat_action(crate::app_acp::ChatAction::Send);
    let id = fixture.app.chat.selected.clone().unwrap();
    let serial = fixture.app.chat.files.preparing[&id].serial;
    fixture.app.chat_action(crate::app_acp::ChatAction::Stop);
    assert!(!fixture.app.chat.busy());
    assert_eq!(fixture.app.chat.queues[&id].items.len(), 1);
    assert!(fixture.app.chat.queues[&id].paused);
    assert!(
        !fixture
            .app
            .apply_chat_prompt_context(id, serial, Ok(late_prompt()), None)
    );
    fixture.hold.store(false, Ordering::Release);
    assert!(fixture.commands.lock().unwrap().is_empty());
}

fn choose_partial_mention(fixture: &mut Fixture, path: PathBuf) {
    fixture.app.chat.input = "explain @mai".into();
    fixture.app.chat.cursor = fixture.app.chat.input.len();
    fixture.app.chat.files.completion = Some(FileCompletion {
        mention: current_mention(&fixture.app.chat.input, fixture.app.chat.cursor).unwrap(),
        root: fixture.app.chat_file_root().into(),
        paths: vec![path],
        selected: 0,
        searching: false,
    });
    fixture.app.choose_chat_file(0);
}

#[test]
fn remote_context_sending_during_completion_validation_uses_chosen_file() {
    let mut fixture = setup();
    let path = file(&fixture, "src/main.rs", "chosen file context");
    fixture.hold.store(true, Ordering::Release);
    choose_partial_mention(&mut fixture, path);
    assert_eq!(fixture.app.chat.files.attaching.len(), 1);
    fixture.app.chat_action(crate::app_acp::ChatAction::Send);
    assert!(fixture.app.chat.files.attaching.is_empty());
    assert!(fixture.app.chat.input.is_empty());
    assert!(fixture.app.chat.files.completion.is_none());
    fixture.hold.store(false, Ordering::Release);
    wait(&mut fixture, |fixture| {
        !fixture.commands.lock().unwrap().is_empty()
    });
    let commands = fixture.commands.lock().unwrap();
    let Command::Prompt { content, .. } = &commands[0] else {
        panic!("prompt");
    };
    assert_eq!(acp::content_text(&content[0]), "explain ");
    assert!(
        matches!(&content[1], p::ContentBlock::ResourceLink(link) if link.name == "src/main.rs" && link.uri.starts_with("vtcode-context:"))
    );
}

#[test]
fn remote_context_completed_file_choice_dismisses_its_completion() {
    let mut fixture = setup();
    let path = file(&fixture, "src/main.rs", "context");
    choose_partial_mention(&mut fixture, path.clone());
    wait(&mut fixture, |fixture| {
        fixture.app.chat.files.attaching.is_empty()
    });
    assert_eq!(fixture.app.chat.input, "explain ");
    assert_eq!(fixture.app.chat.files.attached, [path]);
    assert!(fixture.app.chat.files.completion.is_none());
}

#[test]
fn remote_context_stale_prompt_pauses_original_queue_without_erasing_new_draft() {
    for change in ["agent", "epoch", "folder", "connection"] {
        let mut fixture = setup();
        let path = file(&fixture, "source.rs", "disk");
        fixture.hold.store(true, Ordering::Release);
        fixture.app.chat.input = "original".into();
        fixture.app.chat.files.attached = vec![path];
        fixture.app.chat_action(crate::app_acp::ChatAction::Send);
        let id = fixture.app.chat.selected.clone().unwrap();
        let serial = fixture.app.chat.files.preparing[&id].serial;
        fixture.app.chat.input = "new draft".into();
        match change {
            "agent" => {
                let (events, receiver) = mpsc::channel();
                fixture.app.chat.client = Some(acp::Client::hosted(
                    acp::Workspace::new(fixture.root.path()).unwrap(),
                    Arc::new(Sink(fixture.commands.clone())),
                    receiver,
                ));
                fixture._events = events;
            }
            "epoch" => {
                fixture
                    .app
                    .chat
                    .sessions
                    .get_mut(&id)
                    .unwrap()
                    .history_epoch += 1
            }
            "folder" => fixture
                .app
                .remote_workspace
                .as_mut()
                .unwrap()
                .view_root
                .push("other"),
            "connection" => {
                fixture.app.remote_workspace = Some(remote::workspace::Workspace::new(
                    remote::tests::connection(fixture.root.path(), Arc::default(), Arc::default()),
                ))
            }
            _ => unreachable!(),
        }
        assert!(
            fixture
                .app
                .apply_chat_prompt_context(id.clone(), serial, Ok(late_prompt()), None)
        );
        assert!(fixture.app.chat.files.preparing.is_empty());
        assert_eq!(fixture.app.chat.input, "new draft");
        assert!(fixture.app.chat.queues[&id].paused, "{change}");
        assert_eq!(
            fixture.app.chat.queues[&id].items.front().unwrap().text,
            "original"
        );
        assert!(fixture.commands.lock().unwrap().is_empty());
    }
}

#[test]
fn remote_context_editing_or_removing_a_queued_prompt_cancels_its_preparation() {
    for edit in [true, false] {
        let mut fixture = setup();
        let path = file(&fixture, "source.rs", "disk");
        fixture.hold.store(true, Ordering::Release);
        fixture.app.chat.input = "original".into();
        fixture.app.chat.files.attached = vec![path.clone()];
        fixture.app.chat_action(crate::app_acp::ChatAction::Send);
        let id = fixture.app.chat.selected.clone().unwrap();
        let job = &fixture.app.chat.files.preparing[&id];
        let (item, serial) = (job.item, job.serial);
        fixture.app.chat_action(if edit {
            crate::app_acp::ChatAction::EditQueued(item)
        } else {
            crate::app_acp::ChatAction::RemoveQueued(item)
        });
        assert!(fixture.app.chat.files.preparing.is_empty());
        assert!(fixture.app.chat.queues[&id].items.is_empty());
        assert_eq!(fixture.app.chat.queues[&id].context_direct, None);
        assert!(
            !fixture
                .app
                .apply_chat_prompt_context(id, serial, Ok(late_prompt()), None)
        );
        if edit {
            assert_eq!(fixture.app.chat.input, "original");
            assert_eq!(fixture.app.chat.files.attached, [path]);
        } else {
            assert!(fixture.app.chat.input.is_empty());
            assert!(fixture.app.chat.files.attached.is_empty());
        }
        assert!(fixture.commands.lock().unwrap().is_empty());
    }
}

#[test]
fn remote_context_completion_rejects_a_replaced_workspace_connection() {
    let mut fixture = setup();
    let path = file(&fixture, "main.rs", "disk");
    fixture.hold.store(true, Ordering::Release);
    fixture.app.chat.input = "@mai".into();
    fixture.app.chat.cursor = fixture.app.chat.input.len();
    fixture.app.refresh_chat_file_completion();
    let generation = fixture.app.chat.files.generation;
    let root = fixture.app.chat_file_root().to_path_buf();
    fixture.app.remote_workspace = Some(remote::workspace::Workspace::new(
        remote::tests::connection(fixture.root.path(), Arc::default(), Arc::default()),
    ));
    assert!(
        fixture
            .app
            .apply_chat_file_suggestions(generation, root, vec![path])
    );
    assert!(fixture.app.chat.files.completion.is_none());
}

#[cfg(unix)]
#[test]
fn remote_context_alias_deduplication_rejects_conflicting_unsaved_buffers() {
    let fixture = setup();
    let path = file(&fixture, "source.rs", "disk");
    std::os::unix::fs::symlink("source.rs", fixture.root.path().join("alias.rs")).unwrap();
    let workspace = fixture.app.remote_workspace.as_ref().unwrap();
    let alias = workspace.target_key("alias.rs").unwrap();
    let files = prepare_files(
        workspace,
        vec![alias.clone(), path.clone()],
        vec![buffer(path.clone(), Some(path.clone()), "unsaved")],
        true,
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(
        files.files,
        [(alias.clone(), FileText::Local("unsaved".into()))]
    );
    let error = prepare_files(
        workspace,
        vec![alias.clone()],
        vec![
            buffer(path.clone(), Some(path.clone()), "first"),
            buffer(alias, Some(path), "second"),
        ],
        true,
        &CancellationToken::new(),
    )
    .unwrap_err();
    assert!(error.contains("conflicting unsaved buffers"));
}

#[cfg(unix)]
#[test]
fn remote_context_real_acp_peer_gets_unsaved_text_and_node_resource_urls() {
    for open_buffer in [false, true] {
        for embedded in [false, true] {
            let mut fixture = setup();
            git2::Repository::init(fixture.root.path()).unwrap();
            let path = file(&fixture, "agent.txt", "disk content\n");
            let mut config = remote::agent::test_config();
            if embedded {
                config
                    .env
                    .insert("VTCODE_TEST_EMBEDDED_CONTEXT".into(), "1".into());
            }
            fixture.app.settings_effective.acp_agents = Some(vec![config]);
            fixture
                .app
                .chat_action(crate::app_acp::ChatAction::Connect(0));
            wait(&mut fixture, |fixture| {
                fixture.app.chat.connected()
                    && fixture.app.chat.selected.is_some()
                    && !fixture.app.chat.rollback.busy()
            });
            assert!(
                fixture.app.chat.rollback.enabled,
                "Prepared sources must survive the asynchronous checkpoint path"
            );
            eprintln!("VTCODE_CONTEXT_CHECKPOINT open_buffer={open_buffer} embedded={embedded}");
            if open_buffer {
                fixture.app.new_file();
                fixture.app.editor_mut().unwrap().file_path = Some(path.clone());
                fixture
                    .app
                    .editor_mut()
                    .unwrap()
                    .paste_with_text("unsaved from local Rope λ\r\n");
            }
            fixture.app.focus_chat();
            fixture.app.chat.files.attached = vec![path];
            fixture.app.chat.input = "inspect-context".into();
            let expected = if open_buffer {
                "unsaved from local Rope"
            } else {
                "disk content"
            };
            fixture.app.chat_action(crate::app_acp::ChatAction::Send);
            wait(&mut fixture, |fixture| {
                fixture.app.chat.session().is_some_and(|session| session.entries.iter().any(|entry| matches!(entry, acp::ChatEntry::Agent(text) if text.contains(expected))))
            });
            let reply = fixture
                .app
                .chat
                .session()
                .unwrap()
                .entries
                .iter()
                .find_map(|entry| {
                    if let acp::ChatEntry::Agent(text) = entry {
                        Some(text)
                    } else {
                        None
                    }
                })
                .unwrap();
            assert!(!reply.contains("vtcode-remote-"));
            assert!(!reply.contains("%00"));
            if embedded {
                assert!(reply.contains("file://"));
            }
            assert_eq!(
                std::fs::read_to_string(fixture.root.path().join("agent.txt")).unwrap(),
                "disk content\n"
            );
            wait(&mut fixture, |fixture| {
                fixture
                    .app
                    .chat
                    .session()
                    .is_some_and(|session| !session.busy)
                    && !fixture.app.chat.rollback.busy()
            });
            fixture
                .app
                .chat_action(crate::app_acp::ChatAction::Disconnect);
        }
    }
}
