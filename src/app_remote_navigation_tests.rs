use super::*;
use crate::app::Tab;
use crate::editor::Editor;
use crate::remote::{self, RemotePath};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

struct Fixture {
    app: App,
    root: tempfile::TempDir,
    hold: Arc<AtomicBool>,
    blocked: Arc<AtomicBool>,
    traffic: Arc<AtomicUsize>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.hold.store(false, Ordering::Release);
    }
}

fn setup() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let hold = Arc::new(AtomicBool::new(false));
    let blocked = Arc::new(AtomicBool::new(false));
    let traffic = Arc::new(AtomicUsize::new(0));
    let remote = Workspace::new(remote::tests::connection_with_status(
        root.path(),
        hold.clone(),
        traffic.clone(),
        blocked.clone(),
    ));
    let mut app = App::new_deferred(remote.view_root.clone());
    app.remote_workspace = Some(remote);
    app.source_control_enabled = false;
    app.active_pane = ActivePane::Chat;
    app.tabs = vec![Tab::new_file(Editor::test_with_text("origin\n"))];
    app.groups.clear();
    Fixture {
        app,
        root,
        hold,
        blocked,
        traffic,
    }
}
fn key(fixture: &Fixture, value: &str) -> PathBuf {
    fixture
        .app
        .remote_workspace
        .as_ref()
        .unwrap()
        .target_key(value)
        .unwrap()
}
fn file(fixture: &Fixture, value: &str, text: &str) -> PathBuf {
    let native = fixture.root.path().join(value);
    std::fs::create_dir_all(native.parent().unwrap()).unwrap();
    std::fs::write(native, text).unwrap();
    key(fixture, value)
}
fn until(mut ready: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < end, "Remote navigation stalled");
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn drain(fixture: &mut Fixture) {
    until(|| {
        fixture.app.poll_background_results();
        fixture.app.remote_navigation.running.is_none()
            && fixture.app.remote_navigation.retiring.is_none()
            && fixture.app.remote_navigation.pending.is_none()
            && fixture.app.tabs.iter().all(|tab| !tab.editor.bg_loading)
    });
}
fn begin_blocked(fixture: &mut Fixture, path: &Path) {
    fixture.hold.store(true, Ordering::Release);
    fixture.app.open_chat_location(path, None);
    until(|| fixture.blocked.load(Ordering::Acquire));
}

#[test]
fn remote_header_source_command_uses_async_files_and_preserves_missing_targets() {
    let mut fixture = setup();
    let source = file(&fixture, "code/main.cpp", "int main() {}\n");
    let header = file(&fixture, "code/main.hpp", "void example();\n");
    fixture.app.open_file(&source);
    drain(&mut fixture);
    fixture.app.active_pane = ActivePane::Editor;
    fixture.app.execute_palette_command("switch_header_source");
    drain(&mut fixture);
    assert_eq!(
        fixture.app.editor().unwrap().file_path.as_ref(),
        Some(&header)
    );
    assert_eq!(
        fixture.app.editor().unwrap().rope.to_string(),
        "void example();\n"
    );
    fixture.app.execute_palette_command("switch_header_source");
    drain(&mut fixture);
    assert_eq!(
        fixture.app.editor().unwrap().file_path.as_ref(),
        Some(&source)
    );
    std::fs::remove_file(fixture.root.path().join("code/main.hpp")).unwrap();
    // A live dirty counterpart still wins over the deleted disk path.
    fixture
        .app
        .tabs
        .iter_mut()
        .find(|tab| tab.editor.file_path.as_ref() == Some(&header))
        .unwrap()
        .editor
        .insert_char('X');
    fixture.app.execute_palette_command("switch_header_source");
    drain(&mut fixture);
    assert_eq!(
        fixture.app.editor().unwrap().file_path.as_ref(),
        Some(&header)
    );
    assert!(
        fixture
            .app
            .editor()
            .unwrap()
            .rope
            .to_string()
            .starts_with('X')
    );
}

#[test]
fn remote_new_file_save_dialog_keeps_the_suggested_name() {
    let mut fixture = setup();
    fixture.app.editor_mut().unwrap().suggested_file_name = Some("new sketch λ.rs".into());
    for command in ["save_as", "save"] {
        fixture.app.save_as_active = false;
        fixture.app.execute_palette_command(command);
        assert!(fixture.app.save_as_active, "{command}");
        let workspace = fixture.app.remote_workspace.as_ref().unwrap();
        assert_eq!(
            workspace.target_key(&fixture.app.save_as_query).unwrap(),
            key(&fixture, "new sketch λ.rs"),
            "{command}"
        );
    }
}

#[test]
fn remote_save_as_current_file_keeps_conflict_checks_and_other_open_files_protected() {
    for external in [false, true] {
        let mut fixture = setup();
        let source = file(&fixture, "source.txt", "original\n");
        fixture.app.open_file(&source);
        drain(&mut fixture);
        fixture.app.editor_mut().unwrap().insert_char('X');
        fixture.app.execute_palette_command("save_as");
        if external {
            std::fs::write(fixture.root.path().join("source.txt"), "external\n").unwrap();
        }
        fixture.app.execute_save_as();
        assert!(!fixture.app.save_as_active, "{:?}", fixture.app.modal.error);
        until(|| {
            fixture.app.poll_background_results();
            fixture.app.pending_file_saves.is_empty()
        });
        assert_eq!(
            std::fs::read_to_string(fixture.root.path().join("source.txt")).unwrap(),
            if external {
                "external\n"
            } else {
                "Xoriginal\n"
            }
        );
        assert_eq!(fixture.app.editor().unwrap().modified, external);
    }
    let mut fixture = setup();
    let first = file(&fixture, "first.txt", "first");
    let second = file(&fixture, "second.txt", "second");
    fixture.app.open_file(&first);
    drain(&mut fixture);
    fixture.app.open_file(&second);
    drain(&mut fixture);
    fixture.app.execute_palette_command("save_as");
    fixture.app.save_as_query = fixture
        .app
        .remote_workspace
        .as_ref()
        .unwrap()
        .display_path(&first)
        .unwrap();
    fixture.app.execute_save_as();
    assert!(fixture.app.save_as_active && fixture.app.modal.error.is_some());
}

#[test]
fn remote_save_as_browse_preserves_opaque_filename_and_header_commands_reject_folders() {
    let mut fixture = setup();
    let directory = key(&fixture, "chosen folder");
    std::fs::create_dir(fixture.root.path().join("chosen folder")).unwrap();
    for name in [
        br"literal\name.txt".as_slice(),
        b"trailing ",
        b"bad-\xff.txt",
    ] {
        let workspace = fixture.app.remote_workspace.as_ref().unwrap().clone();
        let source = workspace.view_key(&remote::RemotePath::new(name.to_vec()).unwrap());
        fixture.app.save_as_query = workspace.display_path(&source).unwrap();
        fixture.app.folder_browser_path = directory.clone();
        fixture.app.folder_browser.folder = workspace.display_path(&directory).unwrap();
        fixture.app.folder_browser.save_as_return = true;
        fixture.app.confirm_folder_browser();
        let result = workspace.target_key(&fixture.app.save_as_query).unwrap();
        assert_eq!(
            workspace.remote_path(&result).unwrap(),
            remote::RemotePath::new([b"chosen folder/".as_slice(), name].concat()).unwrap()
        );
    }
    let source = file(&fixture, "folder.cpp", "source");
    std::fs::create_dir(fixture.root.path().join("folder.hpp")).unwrap();
    fixture.app.open_file(&source);
    drain(&mut fixture);
    fixture.app.active_pane = ActivePane::Editor;
    fixture.app.execute_palette_command("switch_header_source");
    drain(&mut fixture);
    assert_eq!(fixture.app.active_pane, ActivePane::Editor);
    assert_eq!(
        fixture.app.editor().unwrap().file_path.as_ref(),
        Some(&source)
    );
}

#[test]
fn copying_a_remote_path_preserves_native_spelling_instead_of_ui_escaping() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    let hello = remote::Hello {
        version: remote::VERSION,
        app_version: env!("CARGO_PKG_VERSION").into(),
        root: "/node/project".into(),
        root_uri: "file:///node/project/".into(),
        session: 42,
    };
    let client =
        remote::client::Client::attach(std::io::Cursor::new(Vec::new()), Vec::new(), hello)
            .unwrap();
    let workspace = Workspace::new(client);
    let mut app = App::new_deferred(workspace.view_root.clone());
    app.remote_workspace = Some(workspace.clone());
    for name in [r"literal\name.txt", "trailing "] {
        let key = workspace
            .target_key(&format!(
                "file:///node/project/{}",
                if name == "trailing " {
                    "trailing%20"
                } else {
                    "literal%5Cname.txt"
                }
            ))
            .unwrap();
        app.explorer_context_target = Some(key);
        let action = app
            .explorer_context_menu_items()
            .iter()
            .position(|item| *item == "Copy Path")
            .unwrap();
        app.execute_explorer_context(action);
        assert_eq!(
            crate::clipboard::get_text().unwrap(),
            format!("/node/project/{name}")
        );
    }
}

#[test]
fn unknown_remote_directory_reveals_explorer_without_creating_a_file_tab() {
    let mut fixture = setup();
    std::fs::create_dir_all(fixture.root.path().join("deep/unlisted/directory")).unwrap();
    let path = key(&fixture, "deep/unlisted/directory");
    let id = fixture.app.tabs[0].editor.navigation_id;
    begin_blocked(&mut fixture, &path);
    assert_eq!(fixture.app.tabs.len(), 1);
    assert_eq!(fixture.app.active_pane, ActivePane::Chat);
    fixture.hold.store(false, Ordering::Release);
    drain(&mut fixture);
    until(|| {
        fixture.app.poll_background_results();
        fixture
            .app
            .explorer
            .entries
            .iter()
            .any(|entry| entry.path == path)
    });
    assert_eq!(fixture.app.active_pane, ActivePane::Explorer);
    assert_eq!(fixture.app.sidebar_view, SidebarView::Explorer);
    assert_eq!(fixture.app.tabs.len(), 1);
    assert_eq!(fixture.app.tabs[0].editor.navigation_id, id);
    assert_eq!(fixture.app.tabs[0].editor.rope.to_string(), "origin\n");
    assert!(
        fixture.app.chat.error.is_none(),
        "{:?}",
        fixture.app.chat.error
    );
}

#[test]
fn remote_link_open_reuses_its_one_file_response_and_keeps_deferred_cursor_and_undo() {
    let mut fixture = setup();
    let text = "first\r\n\tλ 🦀\r\nlast\r\n";
    let path = file(&fixture, "nested folder/猫.txt", text);
    // The writer starts asynchronously with a framing newline. Wait for that
    // byte before measuring the location request, rather than racing startup.
    until(|| fixture.traffic.load(Ordering::Relaxed) != 0);
    let start = fixture.traffic.load(Ordering::Relaxed);
    let workspace = fixture.app.remote_workspace.as_ref().unwrap();
    let request = remote::Request {
        id: 1,
        workspace: None,
        command: remote::Command::OpenLocation {
            path: workspace.remote_path(&path).unwrap(),
        },
    };
    let mut expected = Vec::new();
    remote::wire::write_message(
        &mut expected,
        'C',
        workspace.client.hello().session,
        1,
        &remote::wire::encode(&request).unwrap(),
    )
    .unwrap();
    fixture.app.open_chat_location(&path, Some(1));
    drain(&mut fixture);
    assert_eq!(fixture.app.editor().unwrap().rope.to_string(), text);
    assert_eq!(fixture.app.editor().unwrap().cursor_line, 1);
    assert_eq!(
        fixture.traffic.load(Ordering::Relaxed) - start,
        expected.len(),
        "A small unclassified file link needs one OpenLocation, with no inspect or second Open"
    );
    fixture.app.editor_mut().unwrap().insert_char('!');
    fixture.app.editor_mut().unwrap().undo();
    assert_eq!(fixture.app.editor().unwrap().rope.to_string(), text);
    assert!(!fixture.app.editor().unwrap().modified);
    let checkpoint = fixture
        .app
        .editor()
        .unwrap()
        .remote_checkpoint
        .as_ref()
        .unwrap();
    assert_eq!(
        checkpoint.identity.as_ref(),
        Some(&RemotePath::new("nested folder/猫.txt".as_bytes().to_vec()).unwrap())
    );
}

#[test]
fn large_remote_link_uses_streaming_progress_without_an_inline_size_limit() {
    let mut fixture = setup();
    let text = "\tλ 🦀 mixed\r\n".repeat(80_000);
    let path = file(&fixture, "large.txt", &text);
    fixture.app.open_chat_location(&path, None);
    until(|| {
        fixture.app.poll_background_results();
        fixture.app.remote_navigation.running.is_none()
            && fixture.app.editor().is_some_and(|editor| {
                editor.file_path.as_ref() == Some(&path) && !editor.bg_loading
            })
    });
    assert_eq!(fixture.app.editor().unwrap().rope.to_string(), text);
    assert_eq!(fixture.app.editor().unwrap().file_size, text.len() as u64);
    assert_eq!(
        fixture
            .app
            .editor()
            .unwrap()
            .remote_checkpoint
            .as_ref()
            .unwrap()
            .revision,
        remote::revision(&fixture.app.editor().unwrap().rope, "UTF-8")
    );
    assert!(!fixture.app.apply_remote_location_progress(0, 1, 2));
}

#[test]
fn remote_link_progress_is_deduplicated_and_cannot_outlive_its_request() {
    let mut fixture = setup();
    let path = file(&fixture, "progress.txt", "body\n");
    let previous = fixture.app.status_message.clone();
    begin_blocked(&mut fixture, &path);
    let id = fixture.app.remote_navigation.pending.as_ref().unwrap().id;
    assert!(
        fixture
            .app
            .apply_remote_location_progress(id, 1024 * 1024, 2 * 1024 * 1024)
    );
    assert!(fixture.app.status_message.contains("1.0/2.0 MiB (50%)"));
    assert!(
        !fixture
            .app
            .apply_remote_location_progress(id, 1024 * 1024, 2 * 1024 * 1024)
    );
    assert!(!fixture.app.apply_remote_location_progress(id + 1, 0, 1));
    fixture.app.cancel_remote_location();
    assert_eq!(fixture.app.status_message, previous);
    assert!(!fixture.app.apply_remote_location_progress(id, 2, 2));
    fixture.hold.store(false, Ordering::Release);
    drain(&mut fixture);
    assert_eq!(fixture.app.tabs.len(), 1);
}

#[test]
fn a_cancelled_link_does_not_reset_a_newer_status_message() {
    let mut fixture = setup();
    let path = file(&fixture, "cancelled.txt", "body\n");
    begin_blocked(&mut fixture, &path);
    fixture.app.status_message = "A newer save or operation".into();
    fixture.app.cancel_remote_location();
    fixture.hold.store(false, Ordering::Release);
    drain(&mut fixture);
    assert_eq!(fixture.app.status_message, "A newer save or operation");
}

#[cfg(unix)]
#[test]
fn remote_directory_aliases_are_canonical_and_outside_directories_are_rejected() {
    let mut fixture = setup();
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir(fixture.root.path().join("inside")).unwrap();
    std::os::unix::fs::symlink(
        fixture.root.path().join("inside"),
        fixture.root.path().join("inside-alias"),
    )
    .unwrap();
    std::os::unix::fs::symlink(outside.path(), fixture.root.path().join("outside-alias")).unwrap();
    let workspace = fixture.app.remote_workspace.as_ref().unwrap();
    let cancel = CancellationToken::new();
    let Location::Directory(identity) = workspace
        .open_location(&key(&fixture, "inside-alias"), &cancel, |_, _| {})
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(identity, key(&fixture, "inside"));
    assert!(
        workspace
            .open_location(&key(&fixture, "outside-alias"), &cancel, |_, _| {})
            .is_err()
    );
    let path = key(&fixture, "outside-alias");
    fixture.app.open_chat_location(&path, None);
    drain(&mut fixture);
    assert!(fixture.app.chat.error.as_ref().unwrap().contains("outside"));
    assert_eq!(fixture.app.tabs.len(), 1);
}

#[test]
fn rapid_remote_links_keep_two_workers_and_only_the_latest_pending_destination() {
    let mut fixture = setup();
    let first = file(&fixture, "first.txt", "first\n");
    let last = file(&fixture, "last.txt", "last\n");
    begin_blocked(&mut fixture, &first);
    let first_traffic = fixture.traffic.load(Ordering::Relaxed);
    for index in 0..1000 {
        let path = if index % 2 == 0 { &first } else { &last };
        fixture.app.open_chat_location(path, None);
    }
    assert!(
        fixture.traffic.load(Ordering::Relaxed) <= first_traffic + 1024,
        "One replacement may overlap cancellation, not a request per click"
    );
    assert!(fixture.app.background.active_job_count() <= 2);
    assert_eq!(
        fixture.app.remote_navigation.pending.as_ref().unwrap().path,
        last
    );
    assert_eq!(fixture.app.tabs.len(), 1);
    fixture.hold.store(false, Ordering::Release);
    drain(&mut fixture);
    assert_eq!(
        fixture.app.editor().unwrap().file_path.as_ref(),
        Some(&last)
    );
    assert_eq!(fixture.app.editor().unwrap().rope.to_string(), "last\n");
    assert_eq!(fixture.app.tabs.len(), 2);
}

#[test]
fn delayed_remote_link_cannot_steal_focus_after_navigation_or_draft_editing() {
    for action in [
        "tab",
        "cursor",
        "draft",
        "undo",
        "session",
        "agent",
        "palette",
        "workspace",
    ] {
        let mut fixture = setup();
        let path = file(&fixture, "late.txt", "must not open\n");
        if action == "cursor" {
            fixture.app.active_pane = ActivePane::Editor;
        }
        begin_blocked(&mut fixture, &path);
        let cancel = fixture
            .app
            .remote_navigation
            .running
            .as_ref()
            .unwrap()
            .1
            .clone();
        match action {
            "tab" => {
                fixture.app.new_file();
                fixture.app.active_tab = 0;
                fixture.app.active_pane = ActivePane::Chat;
            }
            "cursor" => fixture.app.editor_mut().unwrap().set_cursor(0, 2),
            "draft" | "undo" => {
                fixture.app.edit_chat_input(
                    crossterm::event::KeyCode::Char('x'),
                    crossterm::event::KeyModifiers::NONE,
                    None,
                );
                fixture.app.record_navigation();
                if action == "undo" {
                    fixture.app.edit_chat_input(
                        crossterm::event::KeyCode::Char('z'),
                        crossterm::event::KeyModifiers::CONTROL,
                        None,
                    );
                }
            }
            "session" => {
                fixture.app.chat.selected = Some(vtcode::acp::protocol::SessionId::new("elsewhere"))
            }
            "agent" => fixture.app.chat.active_agent += 1,
            "palette" => fixture.app.palette.active = true,
            "workspace" => {
                fixture.app.workspace_generation += 1;
                fixture.app.cancel_remote_location();
            }
            _ => unreachable!(),
        }
        fixture.app.record_navigation();
        assert!(cancel.is_cancelled(), "{action}");
        fixture.hold.store(false, Ordering::Release);
        drain(&mut fixture);
        assert!(
            !fixture
                .app
                .tabs
                .iter()
                .any(|tab| tab.editor.file_path.as_ref() == Some(&path)),
            "{action}"
        );
        assert!(
            fixture.app.chat.error.is_none(),
            "{action}: {:?}",
            fixture.app.chat.error
        );
    }
}

#[test]
fn live_remote_buffers_and_workspace_root_need_no_resolution_rpc() {
    let mut fixture = setup();
    let path = file(&fixture, "live.txt", "saved\n");
    fixture.app.open_chat_location(&path, None);
    drain(&mut fixture);
    fixture.app.editor_mut().unwrap().insert_char('!');
    let id = fixture.app.editor().unwrap().navigation_id;
    let traffic = fixture.traffic.load(Ordering::Relaxed);
    fixture.app.open_chat_location(&path, Some(0));
    assert_eq!(fixture.app.editor().unwrap().navigation_id, id);
    assert_eq!(fixture.app.editor().unwrap().rope.to_string(), "!saved\n");
    assert_eq!(fixture.traffic.load(Ordering::Relaxed), traffic);
    let root = fixture.app.explorer.root.clone();
    fixture.app.open_chat_location(&root, None);
    assert_eq!(fixture.app.active_pane, ActivePane::Explorer);
    assert_eq!(fixture.traffic.load(Ordering::Relaxed), traffic);
}

#[test]
fn failed_remote_link_preserves_buffers_and_uses_the_initiating_error_surface() {
    for source in [Source::Chat, Source::Markdown] {
        let mut fixture = setup();
        let path = key(&fixture, "missing.txt");
        let id = fixture.app.tabs[0].editor.navigation_id;
        fixture.app.open_remote_location(path, None, source);
        drain(&mut fixture);
        assert_eq!(fixture.app.tabs.len(), 1);
        assert_eq!(fixture.app.tabs[0].editor.navigation_id, id);
        assert_eq!(fixture.app.tabs[0].editor.rope.to_string(), "origin\n");
        assert!(!fixture.app.status_message.starts_with("Opening:"));
        assert_eq!(fixture.app.chat.error.is_some(), source == Source::Chat);
    }
}

struct BufferedOutput {
    shared: Arc<std::sync::Mutex<(bool, Vec<u8>, std::net::TcpStream)>>,
    records: Arc<AtomicUsize>,
}
impl std::io::Write for BufferedOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let mut state = self.shared.lock().unwrap();
        if state.0 {
            assert!(
                state.1.len() + bytes.len() <= 256 * 1024,
                "Bounded stalled-header fixture"
            );
            state.1.extend_from_slice(bytes);
            self.records.fetch_add(
                bytes.iter().filter(|byte| **byte == b'\n').count(),
                Ordering::Release,
            );
            Ok(bytes.len())
        } else {
            std::io::Write::write(&mut state.2, bytes)
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let mut state = self.shared.lock().unwrap();
        if state.0 {
            Ok(())
        } else {
            std::io::Write::flush(&mut state.2)
        }
    }
}
struct Resume(Arc<std::sync::Mutex<(bool, Vec<u8>, std::net::TcpStream)>>);
impl Resume {
    fn release(&self) {
        use std::io::Write;
        let mut state = self.0.lock().unwrap();
        let buffered = std::mem::take(&mut state.1);
        state.0 = false;
        state.2.write_all(&buffered).unwrap();
        state.2.flush().unwrap();
    }
}
impl Drop for Resume {
    fn drop(&mut self) {
        self.release();
    }
}

#[test]
fn repeated_remote_link_cancellation_cannot_exhaust_undelivered_transfer_headers() {
    // Buffer completed responses while keeping the node writer free. This is
    // unlike blocking its first write: it exposes a receiver/network stall
    // where multiple cancelled source headers can otherwise consume all slots.
    let mut fixture = setup();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let client_stream = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client_stream.set_nodelay(true).unwrap();
    let (server, _) = listener.accept().unwrap();
    server.set_nodelay(true).unwrap();
    let shared = Arc::new(std::sync::Mutex::new((
        false,
        Vec::new(),
        server.try_clone().unwrap(),
    )));
    let resume = Resume(shared.clone());
    let records = Arc::new(AtomicUsize::new(0));
    let writer = BufferedOutput {
        shared: shared.clone(),
        records: records.clone(),
    };
    let root = fixture.root.path().to_owned();
    std::thread::spawn(move || remote::service::serve(&root, server, writer).unwrap());
    let client =
        remote::client::Client::connect(client_stream.try_clone().unwrap(), client_stream).unwrap();
    let workspace = Workspace::new(client);
    fixture.app.explorer = crate::explorer::FileExplorer::new_deferred(workspace.view_root.clone());
    fixture.app.remote_workspace = Some(workspace);
    let text = "varied λ 🦀\r\n".repeat(80_000);
    let path = file(&fixture, "stalled.txt", &text);
    shared.lock().unwrap().0 = true;
    fixture.app.open_chat_location(&path, None);
    until(|| records.load(Ordering::Acquire) == 1);
    let end = Instant::now() + Duration::from_secs(1);
    while Instant::now() < end {
        fixture.app.open_chat_location(&path, None);
        fixture.app.poll_background_results();
        std::thread::sleep(Duration::from_millis(2));
    }
    let queued_headers = records.load(Ordering::Acquire);
    resume.release();
    drain(&mut fixture);
    assert!(
        queued_headers <= 2,
        "Only the active and retiring request may retain undelivered headers, found {queued_headers}"
    );
    assert_eq!(fixture.app.editor().unwrap().rope.to_string(), text);
    assert!(
        fixture.app.chat.error.is_none(),
        "{:?}",
        fixture.app.chat.error
    );
}
