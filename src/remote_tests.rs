use super::*;
use ropey::Rope;
use std::{
    io::Write,
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

#[path = "remote_terminal_transport_tests.rs"]
mod terminal_transport;

#[test]
fn remote_breadcrumbs_preserve_escaped_names_and_hosted_workspace_roots() {
    for (native, uri, first) in [
        ("/project space λ", "file:///project%20space%20%CE%BB/", "/"),
        (r"C:\project space λ", "file:///C:/project%20space%20%CE%BB/", r"C:\"),
        ("/root-�", "file:///root-%FF/", "/"),
        (r"\\server\share\repo", "file://server/share/repo/", "file://server/share/repo"),
    ] {
        let hello = Hello {
            version: VERSION,
            app_version: env!("CARGO_PKG_VERSION").into(),
            root: native.into(),
            root_uri: uri.into(),
            session: 42,
        };
        let client = client::Client::attach(io::Cursor::new(Vec::new()), Vec::new(), hello).unwrap();
        let workspace = workspace::Workspace::new(client);
        for (bytes, expected) in [
            ("space λ".as_bytes(), "space λ"),
            (br"literal\name".as_slice(), r"literal\name"),
            (b"100%real".as_slice(), "100%real"),
            (b"~host".as_slice(), "~host"),
            (b"line\nname".as_slice(), r"line\nname"),
            (b"raw-\xff".as_slice(), "raw-�"),
        ] {
            let path = RemotePath::new(bytes.to_vec()).unwrap();
            let key = workspace.view_key(&path);
            let parts = workspace.breadcrumbs(&key).unwrap();
            assert_eq!(parts.first().unwrap().0, first);
            assert_eq!(parts.last().unwrap(), &(expected.to_string(), key));
            assert!(parts.iter().all(|(name, _)| !name.chars().any(char::is_control)));
            for (_, target) in parts {
                assert!(workspace.remote_path(&target).is_ok());
                let uri = url::Url::from_file_path(&target).unwrap();
                let uri = workspace.lsp_uri(uri.as_str(), true).unwrap();
                assert_eq!(workspace.file_uri_key(&uri).as_ref(), Some(&target));
            }
        }
    }
}

#[test]
fn remote_filesystem_roots_keep_native_root_identity_and_stop_at_real_roots() {
    let root = tempfile::tempdir().unwrap();
    let mut service = service::Service::new(root.path()).unwrap();
    let client = client::Client::attach(io::Cursor::new(Vec::new()), Vec::new(), service.hello(42)).unwrap();
    let workspace = workspace::Workspace::new(client);
    let Reply::FilesystemRoots(roots) = service.execute(Command::FilesystemRoots).unwrap() else {
        panic!("Expected filesystem roots");
    };
    assert!(!roots.is_empty());
    for (name, path) in roots {
        assert!(!name.is_empty());
        let native = path.resolve(root.path()).unwrap();
        assert!(native.is_absolute());
        let key = workspace.view_key(&path);
        assert_eq!(workspace.remote_path(&key).unwrap(), path);
        let uri = url::Url::from_file_path(&native).unwrap();
        assert_eq!(workspace.file_uri_key(uri.as_str()), Some(key.clone()));
        let wire_uri = url::Url::from_file_path(&key).unwrap();
        let wire_uri = workspace.lsp_uri(wire_uri.as_str(), true).unwrap();
        assert_eq!(wire_uri.trim_end_matches('/'), uri.as_str().trim_end_matches('/'));
        #[cfg(windows)]
        {
            assert!(native.parent().is_none());
            assert!(workspace.parent_key(&key).unwrap().is_none());
            assert_eq!(workspace.breadcrumbs(&key).unwrap().len(), 1);
            let child = path.child(b"child").unwrap();
            assert_eq!(workspace.parent_key(&workspace.view_key(&child)).unwrap(), Some(key));
        }
    }
}

#[test]
fn displayed_remote_paths_roundtrip_all_roots_and_filename_bytes() {
    for (native, uri) in [
        ("/repo space λ", "file:///repo%20space%20%CE%BB/"),
        (r"C:\repo space λ", "file:///C:/repo%20space%20%CE%BB/"),
        (r"\\server\share\repo", "file://server/share/repo/"),
        ("/root-�", "file:///root-%FF/"),
    ] {
        let hello = Hello {
            version: VERSION,
            app_version: env!("CARGO_PKG_VERSION").into(),
            root: native.into(),
            root_uri: uri.into(),
            session: 42,
        };
        let client =
            client::Client::attach(io::Cursor::new(Vec::new()), Vec::new(), hello).unwrap();
        let workspace = workspace::Workspace::new(client);
        for bytes in [
            b"plain.h".as_slice(),
            "space λ/a.cpp".as_bytes(),
            br"literal\name.c",
            b"trailing ",
            b"line\nfile.c",
            b"bad-\xff.c",
            b"quote\"",
            br"trailing\",
        ] {
            let remote = RemotePath::new(bytes.to_vec()).unwrap();
            let key = workspace.view_key(&remote);
            let text = workspace.display_path(&key).unwrap();
            assert_eq!(
                workspace.target_key(&text).unwrap(),
                key,
                "root={uri}, bytes={bytes:?}, display={text:?}"
            );
            let context = workspace.context_path(&key).unwrap();
            if native.contains('�') || std::str::from_utf8(bytes).is_err() {
                assert!(context.starts_with("file://"));
            } else {
                let name = std::str::from_utf8(bytes).unwrap();
                let expected = if native.starts_with('/') {
                    format!("{native}/{name}")
                } else {
                    format!("{native}\\{}", name.replace('/', "\\"))
                };
                assert_eq!(context, expected);
            }
        }
        assert!(
            workspace
                .file_uri_key("file://unrelated/share/file.txt")
                .is_none()
        );
        assert!(
            workspace
                .file_uri_key("file://server/outside/file.txt")
                .is_none()
        );
        if uri.starts_with("file://server/") {
            let local = workspace
                .file_uri_key("file:///share/repo/file.txt")
                .unwrap();
            assert!(
                workspace.remote_path(&local).unwrap().is_absolute(),
                "A hostless file must not become a relative UNC file"
            );
        }
        let directory = workspace.view_key(&RemotePath::new(b"sub folder".to_vec()).unwrap());
        for name in [
            "child#name",
            "100%real",
            r"literal\name",
            "line\nname",
            "../sibling",
        ] {
            let result = workspace.resolve_from_key(&directory, name).unwrap();
            let expected = if name == "../sibling" {
                b"sibling".to_vec()
            } else {
                [b"sub folder/".as_slice(), name.as_bytes()].concat()
            };
            assert_eq!(
                workspace.remote_path(&result).unwrap(),
                RemotePath::new(expected).unwrap(),
                "{uri}: {name:?}"
            );
        }
    }
}

#[test]
fn handshake_accepts_matching_application_and_protocol_versions() {
    let root = tempfile::tempdir().unwrap();
    let hello = service::Service::new(root.path()).unwrap().hello(42);
    let mut record = Vec::new();
    wire::write_record(
        &mut record,
        &format!(
            "VTCR-READY:{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&hello).unwrap())
        ),
    )
    .unwrap();
    let parsed = client::Client::parse_hello(&record).unwrap().unwrap();
    assert_eq!(parsed.app_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(parsed.version, VERSION);
    assert_eq!(parsed.root_uri, hello.root_uri);

    let mut frame = Vec::new();
    wire::write_message(&mut frame, 'S', 42, 1, b"test").unwrap();
    assert!(
        String::from_utf8(frame)
            .unwrap()
            .contains(&format!("VTCR{VERSION}S:")),
        "The readiness protocol and frame prefix must describe the same protocol"
    );
}

#[test]
fn handshake_rejects_version_mismatches_before_sending_workspace_requests() {
    struct Writes(Arc<AtomicUsize>);
    impl Write for Writes {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.fetch_add(bytes.len(), Ordering::Relaxed);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let root = tempfile::tempdir().unwrap();
    let original =
        serde_json::to_value(service::Service::new(root.path()).unwrap().hello(42)).unwrap();
    for (protocol, application) in [
        (VERSION - 1, Some(env!("CARGO_PKG_VERSION"))),
        (VERSION + 1, Some(env!("CARGO_PKG_VERSION"))),
        (VERSION, Some("0.0.0-test")),
        (VERSION, Some("9999.0.0")),
        (VERSION, None),
        (VERSION - 1, None),
    ] {
        let mut value = original.clone();
        value["version"] = protocol.into();
        match application {
            Some(version) => value["app_version"] = version.into(),
            None => {
                value.as_object_mut().unwrap().remove("app_version");
            }
        }
        let mut bytes = b"Remote startup output\r\n".to_vec();
        wire::write_record(
            &mut bytes,
            &format!(
                "VTCR-READY:{}",
                URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).unwrap())
            ),
        )
        .unwrap();
        let writes = Arc::new(AtomicUsize::new(0));
        let error = match client::Client::connect(io::Cursor::new(bytes), Writes(writes.clone())) {
            Err(error) => error,
            Ok(_) => panic!("Mismatched versions connected"),
        };
        let message = error.to_string();
        assert!(message.starts_with("VTCode version mismatch:"), "{message}");
        assert!(
            message.contains(&format!(
                "local {} (protocol {VERSION})",
                env!("CARGO_PKG_VERSION")
            )),
            "{message}"
        );
        assert!(
            message.contains(&format!("(protocol {protocol})")),
            "{message}"
        );
        assert!(
            message.contains(application.unwrap_or("version not reported")),
            "{message}"
        );
        assert!(
            message.contains("Install matching VTCode builds on both machines"),
            "{message}"
        );
        assert_eq!(
            writes.load(Ordering::Relaxed),
            0,
            "No request may precede compatibility validation"
        );
    }
}

#[test]
#[ignore = "Internal private-PTY headless service fixture"]
fn headless_service_fixture() {
    service::run(&std::env::current_dir().unwrap()).unwrap();
}

// Transport/lab fixtures supply native argv, independent of the public CLI.
pub(super) fn raw_launch(args: impl IntoIterator<Item = std::ffi::OsString>) -> launch::Launch {
    let mut args = args.into_iter();
    launch::Launch::new(args.next().expect("fixture transport executable"), args.collect())
}

#[cfg(target_os = "linux")]
#[test]
fn service_shutdown_kills_pipe_and_pty_children_without_actor_teardown_waits() {
    use std::io::BufRead;
    let root = tempfile::tempdir().unwrap();
    let client = connection(root.path(), Arc::default(), Arc::default());
    let mut owned = Vec::new();
    let mut pids = Vec::new();
    for terminal in [None, Some((80, 24))] {
        let process = client
            .spawn(
                "/bin/sh".into(),
                vec![
                    "-c".into(),
                    "sleep 60 & child=$!; printf '%s %s\\n' \"$$\" \"$child\"; wait".into(),
                ],
                RemotePath::default(),
                terminal,
                Default::default(),
            )
            .unwrap();
        let (reader, writer, killer) = process.split();
        let mut reader = std::io::BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let children: Vec<u32> = line
            .split_whitespace()
            .map(|s| s.parse().unwrap())
            .collect();
        assert_eq!(children.len(), 2, "{line}");
        pids.extend(children);
        owned.push((reader, writer, killer));
    }
    client.call(Command::Shutdown).unwrap();
    let live = |pid: &u32| {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .is_some_and(|stat| {
                stat.rsplit_once(')').unwrap().1.split_whitespace().next() != Some("Z")
            })
    };
    let end = Instant::now() + Duration::from_secs(5);
    while pids.iter().any(live) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        !pids.iter().any(live),
        "All owned process groups must stop before service exit: {pids:?}"
    );
    drop(owned);
}

#[test]
#[ignore = "Internal remote process pipe fixture"]
fn process_echo_fixture() {
    use std::io::Read;
    let mut stdout = io::stdout().lock();
    stdout.write_all(b"\nVT-ECHO-READY\n").unwrap();
    stdout.flush().unwrap();
    let mut stdin = io::stdin().lock();
    let mut buffer = [0; 4096];
    loop {
        let count = stdin.read(&mut buffer).unwrap();
        if count == 0 {
            return;
        }
        stdout.write_all(&buffer[..count]).unwrap();
        stdout.flush().unwrap();
    }
}

#[test]
#[ignore = "Internal remote process output flood fixture"]
fn process_flood_fixture() {
    use std::io::Read;
    let mut stdout = io::stdout().lock();
    stdout.write_all(b"\nVT-FLOOD-READY\n").unwrap();
    stdout.flush().unwrap();
    let block = [b'x'; 4096];
    for _ in 0..1024 {
        stdout.write_all(&block).unwrap();
    }
    stdout.flush().unwrap();
    let _ = io::stdin().read(&mut [0]);
}

#[test]
#[ignore = "Internal stdio language-server transport fixture"]
fn process_lsp_fixture() {
    use serde_json::{Value, json};
    let mut input = std::io::BufReader::new(io::stdin());
    let mut output = io::stdout().lock();
    output.write_all(b"\nVT-LSP-READY\n").unwrap();
    output.flush().unwrap();
    let root = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
    let expected = url::Url::from_file_path(root.join("code 文.rs"))
        .unwrap()
        .to_string();
    let mut current = Rope::new();
    while let Some(message) = crate::lsp::LspClient::read_message(&mut input).unwrap() {
        let message: Value = serde_json::from_str(&message).unwrap();
        let params = &message["params"];
        let result = match message["method"].as_str().unwrap_or("") {
            "initialize" => {
                assert!(params["processId"].is_null());
                assert_eq!(
                    params["rootUri"],
                    url::Url::from_directory_path(&root)
                        .unwrap()
                        .to_string()
                        .trim_end_matches('/')
                );
                Some(
                    json!({"capabilities":{"textDocumentSync":2,"hoverProvider":true,"definitionProvider":true}}),
                )
            }
            "textDocument/didOpen" => {
                assert_eq!(params["textDocument"]["uri"], expected);
                current = Rope::from_str(params["textDocument"]["text"].as_str().unwrap());
                None
            }
            "textDocument/didChange" => {
                assert_eq!(params["textDocument"]["uri"], expected);
                for change in params["contentChanges"].as_array().unwrap() {
                    let text = change["text"].as_str().unwrap();
                    if let Some(range) = change.get("range") {
                        let index = |position: &Value| {
                            let line = position["line"].as_u64().unwrap() as usize;
                            current.line_to_char(line)
                                + crate::lsp_positions::from_utf16(
                                    &current,
                                    line,
                                    position["character"].as_u64().unwrap() as u32,
                                )
                        };
                        let start = index(&range["start"]);
                        let end = index(&range["end"]);
                        current.remove(start..end);
                        current.insert(start, text);
                    } else {
                        current = Rope::from_str(text);
                    }
                }
                None
            }
            "textDocument/hover" => {
                assert_eq!(params["textDocument"]["uri"], expected);
                Some(json!({"contents":{"kind":"plaintext","value":current.to_string()}}))
            }
            "textDocument/definition" => Some(
                json!({"uri":expected,"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}}}),
            ),
            "shutdown" => Some(Value::Null),
            "exit" => return,
            _ => None,
        };
        if let Some(result) = result {
            let bytes =
                serde_json::to_vec(&json!({"jsonrpc":"2.0","id":message["id"],"result":result}))
                    .unwrap();
            write!(output, "Content-Length: {}\r\n\r\n", bytes.len()).unwrap();
            output.write_all(&bytes).unwrap();
            output.flush().unwrap();
        }
    }
}

#[test]
fn remote_lsp_initialization_incremental_unicode_edits_and_navigation_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let workspace =
        workspace::Workspace::new(connection(dir.path(), Arc::default(), Arc::default()));
    let (program, args) = fixture_args("process_lsp_fixture");
    let process = workspace
        .client
        .spawn(
            program,
            args,
            RemotePath::default(),
            None,
            Default::default(),
        )
        .unwrap();
    let (reader, writer, child) = process.split();
    let mut reader = std::io::BufReader::new(reader);
    prelude(&mut reader, "VT-LSP-READY");
    let config = crate::lsp::LspServerConfig {
        language: "rust".into(),
        command: String::new(),
        args: Vec::new(),
        file_patterns: vec!["*.rs".into()],
        settings: None,
        init_options: None,
    };
    let client = crate::lsp::LspClient::start_transport(
        &config,
        &workspace.view_root,
        Default::default(),
        lsp::LspProcess::Remote(child),
        Box::new(writer),
        Box::new(reader),
        Some(workspace.clone()),
    )
    .unwrap();
    let mut manager = crate::lsp::LspManager::new(workspace.view_root.clone());
    manager.clients.push(client);
    let wait = |manager: &mut crate::lsp::LspManager,
                ready: &dyn Fn(&crate::lsp::LspManager) -> bool| {
        let start = Instant::now();
        loop {
            manager.poll();
            if ready(manager) {
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "LSP did not respond: {:?}",
                manager.startup_errors
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    wait(&mut manager, &|m| {
        m.clients.first().is_some_and(|c| c.initialized)
    });
    let key = workspace.target_key("code 文.rs").unwrap();
    let mut text = Rope::from_str("🦀 café\r\n// file:///literal/text\n");
    manager.sync_document(&key, 1, text.clone());
    text.insert(1, "🐱");
    text.remove(4..5);
    manager.sync_document(&key, 2, text.clone());
    assert!(manager.request_hover(&key, 0, 0));
    wait(&mut manager, &|m| m.hover_content.is_some());
    assert!(
        manager
            .hover_content
            .as_ref()
            .unwrap()
            .contains(&text.to_string())
    );
    assert!(manager.request_definition(&key, 0, 0));
    wait(&mut manager, &|m| !m.definition_locations.is_empty());
    assert_eq!(
        manager.definition_locations[0].uri.to_file_path().unwrap(),
        key
    );
}

#[test]
fn remote_terminal_uses_local_grid_resizes_and_closes_without_blocking_editor() {
    let dir = tempfile::tempdir().unwrap();
    let workspace =
        workspace::Workspace::new(connection(dir.path(), Arc::default(), Arc::default()));
    let mut app = app_for_workspace(&workspace);
    app.new_terminal_tab();
    let index = app.active_terminal;
    app.ensure_terminal_pty_at(index, 80, 12);
    assert!(
        app.terminals[index].terminal_launch_error.is_none(),
        "{:?}",
        app.terminals[index].terminal_launch_error
    );
    app.terminals[index]
        .terminal_pty
        .as_ref()
        .unwrap()
        .write(b"echo VT-REMOTE-SHELL-READY\r");
    let start = Instant::now();
    loop {
        let terminal = app.terminals[index].terminal_pty.as_mut().unwrap();
        terminal.process();
        let text: String = terminal
            .grid
            .cells
            .iter()
            .flat_map(|row| row.iter().map(|cell| cell.ch))
            .collect();
        if text.contains("VT-REMOTE-SHELL-READY") {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "Remote shell did not respond"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let terminal = app.terminals[index].terminal_pty.as_mut().unwrap();
    terminal.resize(110, 20);
    assert_eq!((terminal.grid.width, terminal.grid.height), (110, 20));
    app.close_terminal_tab_at(index);
    assert!(app.terminals[0].terminal_pty.is_none());
    assert!(matches!(
        workspace.client.call(Command::Ping).unwrap(),
        Reply::Done
    ));
}

fn fixture_args(name: &str) -> (String, Vec<String>) {
    (
        std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        vec![
            "--ignored".into(),
            "--exact".into(),
            format!("remote::tests::{name}"),
            "--nocapture".into(),
        ],
    )
}
fn prelude(reader: &mut impl std::io::BufRead, marker: &str) {
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() != 0);
        if line.trim() == marker {
            return;
        }
    }
}

#[test]
fn process_flow_control_is_lossless_for_large_binary_input_and_output() {
    use std::io::{BufReader, Read};
    let dir = tempfile::tempdir().unwrap();
    let client = connection(dir.path(), Arc::default(), Arc::default());
    let (program, args) = fixture_args("process_echo_fixture");
    let process = client
        .spawn(
            program,
            args,
            RemotePath::default(),
            None,
            crate::background::UiWake::default(),
        )
        .unwrap();
    let (reader, mut writer, _killer) = process.split();
    let mut reader = BufReader::new(reader);
    prelude(&mut reader, "VT-ECHO-READY");
    let input: Vec<_> = (0..1024 * 1024).map(|n| (n % 256) as u8).collect();
    let sent = input.clone();
    let sending = std::thread::spawn(move || writer.write_all(&sent));
    let mut actual = vec![0; input.len()];
    reader.read_exact(&mut actual).unwrap();
    sending.join().unwrap().unwrap();
    assert_eq!(actual, input);
    assert!(matches!(client.call(Command::Ping).unwrap(), Reply::Done));
}

#[test]
fn noisy_process_cannot_block_other_requests_when_output_is_not_consumed() {
    let dir = tempfile::tempdir().unwrap();
    let client = connection(dir.path(), Arc::default(), Arc::default());
    let (program, args) = fixture_args("process_flood_fixture");
    let process = client
        .spawn(
            program,
            args,
            RemotePath::default(),
            None,
            crate::background::UiWake::default(),
        )
        .unwrap();
    let (reader, _writer, _killer) = process.split();
    let mut reader = std::io::BufReader::new(reader);
    prelude(&mut reader, "VT-FLOOD-READY");
    std::thread::sleep(Duration::from_millis(50));
    assert!(matches!(
        client
            .call_timeout(Command::Ping, Duration::from_secs(5))
            .unwrap(),
        Reply::Done
    ));
}

#[test]
fn lsp_path_translation_preserves_source_text_and_external_remote_locations() {
    let dir = tempfile::tempdir().unwrap();
    let client = connection(dir.path(), Arc::default(), Arc::default());
    let workspace = workspace::Workspace::new(client);
    let path = RemotePath::new("src/文%a\\b.rs".as_bytes().to_vec()).unwrap();
    let key = workspace.view_key(&path);
    assert_eq!(workspace.remote_path(&key).unwrap(), path);
    let external = RemotePath::absolute(b"/other/project/file.rs").unwrap();
    let external_key = workspace.view_key(&external);
    assert_eq!(workspace.remote_path(&external_key).unwrap(), external);
    // Use a POSIX external path even when the fixture service is Windows.
    let mut value = serde_json::json!({"uri": "file:///other/project/file.rs", "text": "file:///other/project/file.rs", "changes": {"file:///other/project/file.rs": [{"newText": "file:///other/project/file.rs"}]}});
    workspace.rewrite_lsp(&mut value, false);
    assert_eq!(
        url::Url::parse(value["uri"].as_str().unwrap())
            .unwrap()
            .to_file_path()
            .unwrap(),
        external_key
    );
    assert_eq!(value["text"], "file:///other/project/file.rs");
    workspace.rewrite_lsp(&mut value, true);
    assert_eq!(value["uri"], "file:///other/project/file.rs");
    assert_eq!(
        value["changes"]["file:///other/project/file.rs"][0]["newText"],
        "file:///other/project/file.rs"
    );
    let mut foreign = serde_json::json!({"uri":"file://other-machine/share/file.rs", "text":"file://other-machine/share/file.rs"});
    workspace.rewrite_lsp(&mut foreign, false);
    assert!(
        url::Url::parse(foreign["uri"].as_str().unwrap())
            .unwrap()
            .to_file_path()
            .is_err(),
        "a remote server must never direct the client into a native UNC share"
    );
    assert_eq!(foreign["text"], "file://other-machine/share/file.rs");
}

#[test]
fn real_pty_attaches_headless_and_transfers_without_screen_reflow() {
    let args = [
        std::env::current_exe().unwrap().into_os_string(),
        "--ignored".into(),
        "--exact".into(),
        "remote::tests::headless_service_fixture".into(),
        "--nocapture".into(),
    ];
    let launch = raw_launch(args);
    let setup = launch
        .start(
            ratatui::layout::Size::new(23, 8),
            crate::background::UiWake::default(),
        )
        .unwrap();
    let (client, _owned) = setup.endpoint_for_test().unwrap();
    assert!(matches!(client.call(Command::Ping).unwrap(), Reply::Done));
    let path = RemotePath::new(b"README.md".to_vec()).unwrap();
    let Reply::Opened { text, .. } = client.call(Command::Open { path }).unwrap() else {
        panic!()
    };
    assert_eq!(text, std::fs::read_to_string("README.md").unwrap());
    #[cfg(unix)]
    check_background_tool_has_no_controlling_terminal(&client);
}

fn check_background_tool_has_no_controlling_terminal(client: &client::Client) {
    let process = client.spawn("/bin/sh".into(),vec!["-c".into(),
        "if (: </dev/tty) 2>/dev/null; then echo VT-TTY-LEAK; exit 7; else echo VT-TTY-DETACHED; fi".into()],
        RemotePath::default(),None,Default::default()).unwrap();
    let (reader, _writer, _killer) = process.split();
    let mut reader = std::io::BufReader::new(reader);
    prelude(&mut reader, "VT-TTY-DETACHED");
    assert!(
        matches!(client.call(Command::Ping).unwrap(), Reply::Done),
        "Detaching the tool must leave the job/service terminal alive"
    );
}

pub(crate) fn connection(
    root: &Path,
    hold: Arc<AtomicBool>,
    transferred: Arc<AtomicUsize>,
) -> client::Client {
    connection_with_status(root, hold, transferred, Arc::default())
}
pub(crate) fn connection_with_status(
    root: &Path,
    hold: Arc<AtomicBool>,
    transferred: Arc<AtomicUsize>,
    blocked: Arc<AtomicBool>,
) -> client::Client {
    connection_with_traffic(root, hold, transferred, blocked, Arc::default())
}
pub(crate) fn connection_with_traffic(
    root: &Path,
    hold: Arc<AtomicBool>,
    transferred: Arc<AtomicUsize>,
    blocked: Arc<AtomicBool>,
    received: Arc<AtomicUsize>,
) -> client::Client {
    struct Output {
        stream: TcpStream,
        hold: Arc<AtomicBool>,
        blocked: Arc<AtomicBool>,
        received: Arc<AtomicUsize>,
    }
    impl Write for Output {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.hold.load(Ordering::Acquire) {
                self.blocked.store(true, Ordering::Release);
                while self.hold.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                self.blocked.store(false, Ordering::Release);
            }
            let written = self.stream.write(bytes)?;
            self.received.fetch_add(written, Ordering::Relaxed);
            Ok(written)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.stream.flush()
        }
    }
    struct Input {
        stream: TcpStream,
        bytes: Arc<AtomicUsize>,
    }
    impl Write for Input {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let count = self.stream.write(bytes)?;
            self.bytes.fetch_add(count, Ordering::Relaxed);
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.stream.flush()
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    stream.set_nodelay(true).unwrap();
    let (server, _) = listener.accept().unwrap();
    server.set_nodelay(true).unwrap();
    let root = root.to_owned();
    std::thread::spawn(move || {
        let result = service::serve(
            &root,
            server.try_clone().unwrap(),
            Output {
                stream: server,
                hold,
                blocked,
                received,
            },
        );
        if let Err(error) = result {
            eprintln!("Test headless service ended: {error}");
        }
    });
    client::Client::connect(
        stream.try_clone().unwrap(),
        Input {
            stream,
            bytes: transferred,
        },
    )
    .unwrap()
}

#[test]
fn app_edits_undo_and_render_need_no_network_and_save_ack_preserves_new_typing() {
    let dir = tempfile::tempdir().unwrap();
    let text = "fn example() {\n    // 🦀 café\n}\n".repeat(2000);
    std::fs::write(dir.path().join("test.rs"), &text).unwrap();
    let hold = Arc::new(AtomicBool::new(false));
    struct Release(Arc<AtomicBool>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    let _release = Release(hold.clone());
    let bytes = Arc::new(AtomicUsize::new(0));
    let client = connection(dir.path(), hold.clone(), bytes.clone());
    let workspace = workspace::Workspace::new(client);
    let key = workspace.view_key(&RemotePath::new(b"test.rs".to_vec()).unwrap());
    assert_eq!(
        url::Url::from_file_path(&key)
            .unwrap()
            .to_file_path()
            .unwrap(),
        key
    );
    // A missed adapter can never operate on the native filesystem.
    assert!(std::fs::read(&key).is_err());
    let mut app = crate::app::App::new_deferred(workspace.view_root.clone());
    app.remote_workspace = Some(workspace);
    app.source_control_enabled = false;
    app.trim_trailing_on_save = false;
    app.insert_final_newline = false;
    app.open_file(&key);
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.editor().is_some_and(|e| !e.bg_loading)
    });
    assert_eq!(app.editor().unwrap().rope.to_string(), text);
    let after_open = bytes.load(Ordering::Relaxed);
    for _ in 0..1000 {
        app.editor_mut().unwrap().move_right();
        app.editor_mut().unwrap().move_left();
    }
    app.editor_mut().unwrap().insert_char('A');
    app.editor_mut().unwrap().undo();
    app.editor_mut().unwrap().redo();
    assert_eq!(
        bytes.load(Ordering::Relaxed),
        after_open,
        "Editing never sends keystrokes"
    );
    hold.store(true, Ordering::Release);
    let submit = Instant::now();
    app.save_current_file();
    assert!(
        submit.elapsed() < Duration::from_millis(100),
        "Save submission cannot wait on the network"
    );
    app.editor_mut().unwrap().insert_char('B');
    assert!(app.editor().unwrap().modified);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    assert!(app.editor().unwrap().rope.to_string().starts_with("AB"));
    assert!(!app.pending_file_saves.is_empty());
    hold.store(false, Ordering::Release);
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.pending_file_saves.is_empty()
    });
    assert!(app.editor().unwrap().modified);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("test.rs")).unwrap(),
        format!("A{text}")
    );
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope.to_string(), format!("A{text}"));
    assert!(!app.editor().unwrap().modified);
    app.editor_mut().unwrap().redo();
    app.save_current_file();
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.pending_file_saves.is_empty()
    });
    assert_eq!(
        std::fs::read_to_string(dir.path().join("test.rs")).unwrap(),
        format!("AB{text}")
    );
    assert!(!app.editor().unwrap().modified);
    assert!(
        bytes.load(Ordering::Relaxed) - after_open < 4096,
        "Two single-character saves must not retransmit the document"
    );
}

#[test]
fn concurrent_opens_wait_for_bounded_capacity_and_pre_cancelled_deletes_do_not_run() {
    use std::sync::mpsc;
    let dir = tempfile::tempdir().unwrap();
    for name in ["one.rs", "two.rs", "three.rs"] {
        std::fs::write(dir.path().join(name), name).unwrap();
    }
    let hold = Arc::new(AtomicBool::new(false));
    struct Release(Arc<AtomicBool>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    let _release = Release(hold.clone());
    let traffic = Arc::new(AtomicUsize::new(0));
    let client = connection(dir.path(), hold.clone(), traffic.clone());
    hold.store(true, Ordering::Release);
    let mut jobs = Vec::new();
    for name in ["one.rs", "two.rs"] {
        let client = client.clone();
        jobs.push(std::thread::spawn(move || {
            client.call_timeout(
                Command::Open {
                    path: RemotePath::new(name.as_bytes().to_vec()).unwrap(),
                },
                Duration::from_secs(10),
            )
        }));
    }
    // Wait until both initial opens have been serialized and hold the window.
    let start = Instant::now();
    while traffic.load(Ordering::Relaxed) < 250 && start.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(5));
    }
    let third = client.clone();
    let (sender, result) = mpsc::sync_channel(1);
    let third = std::thread::spawn(move || {
        let reply = third.call_timeout(
            Command::Open {
                path: RemotePath::new(b"three.rs".to_vec()).unwrap(),
            },
            Duration::from_secs(10),
        );
        sender.send(reply).unwrap();
    });
    assert!(
        matches!(
            result.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "A busy transfer window queues workers instead of creating a failed editor tab"
    );
    let cancelled = crate::background::CancellationToken::new();
    cancelled.cancel();
    assert_eq!(
        client
            .call_progress(
                Command::Filesystem(fs::Task::Delete {
                    paths: vec![RemotePath::new(b"one.rs".to_vec()).unwrap()]
                }),
                &cancelled,
                |_| {}
            )
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    hold.store(false, Ordering::Release);
    for job in jobs {
        assert!(matches!(job.join().unwrap().unwrap(), Reply::Opened { .. }));
    }
    assert!(
        matches!(result.recv_timeout(Duration::from_secs(10)).unwrap().unwrap(),Reply::Opened { text,.. } if text == "three.rs")
    );
    third.join().unwrap();
    assert!(dir.path().join("one.rs").exists());
    client.call(Command::Shutdown).unwrap();
}

#[test]
fn remote_paths_are_not_windows_paths_and_roundtrip_unusual_names() {
    let path = RemotePath::default()
        .child(b"folder")
        .unwrap()
        .child(b"C:\\not-a-drive\xff")
        .unwrap();
    let encoded = serde_json::to_vec(&path).unwrap();
    let decoded: RemotePath = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(path, decoded);
    assert_eq!(path.parent().unwrap().display(), "folder");
    for invalid in [
        b"../bad".as_slice(),
        b"a/../b",
        b"/root",
        b"a//b",
        b"a\0b",
        b".",
    ] {
        assert!(RemotePath::new(invalid.to_vec()).is_err());
    }
}

#[test]
fn rope_delta_covers_unicode_crlf_deletions_and_undo() {
    let originals = ["", "🐱 café\r\n文\n", "a\r\nb\r\n", "x"];
    let targets = ["", "🦀\n", "🐱 cafè\r\n文\n", "a\nb\n", "x\r\n"];
    for old in originals {
        for new in targets {
            let old = Rope::from_str(old);
            let new = Rope::from_str(new);
            assert_eq!(Change::between(&old, &new).apply(&old).unwrap(), new);
            assert_eq!(Change::between(&new, &old).apply(&new).unwrap(), old);
        }
    }
}

#[test]
fn joining_remote_relative_paths_validates_components_and_preserves_host_and_filename_bytes() {
    for root in [
        RemotePath::default(),
        RemotePath::new(b"project".to_vec()).unwrap(),
        RemotePath::absolute(b"/host/root").unwrap(),
    ] {
        for suffix in [
            b"nested/file.rs".as_slice(),
            "space λ/猫".as_bytes(),
            br"literal\name/file",
            b"raw/\xff",
        ] {
            let mut expected = root.clone();
            for part in suffix.split(|byte| *byte == b'/') {
                expected = expected.child(part).unwrap();
            }
            assert_eq!(root.join_relative(suffix).unwrap(), expected);
        }
        for invalid in [
            b"".as_slice(),
            b"/absolute",
            b"trailing/",
            b"empty//component",
            b"../outside",
            b"a/./b",
            b"nul\0",
        ] {
            assert!(root.join_relative(invalid).is_err(), "{invalid:?}");
        }
    }
}

#[test]
fn wire_roundtrips_compression_fragmentation_crlf_and_ignores_echo() {
    let payload = "α🦀\r\nhello\n".repeat(20_000);
    let encoded = wire::encode(&payload).unwrap();
    assert!(encoded.len() < payload.len() / 20);
    let mut bytes = Vec::new();
    wire::write_message(&mut bytes, 'S', 42, 3, &encoded).unwrap();
    let mut decoder = wire::Decoder::new('S', 42);
    let mut echo = wire::Decoder::new('C', 42);
    let mut answer = None;
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        assert!(line.len() <= wire::MAX_LINE);
        assert!(echo.line(line).unwrap().is_none());
        let mut crlf = line[..line.len() - 1].to_vec();
        crlf.extend_from_slice(b"\r\n");
        if let Some((id, data)) = decoder.line(&crlf).unwrap() {
            assert_eq!(id, 3);
            answer = Some(data);
        }
    }
    assert_eq!(
        serde_json::from_slice::<String>(&answer.unwrap()).unwrap(),
        payload
    );
    let mut request = Vec::new();
    wire::write_message(&mut request, 'C', 42, 3, &encoded).unwrap();
    assert!(
        request
            .split_inclusive(|b| *b == b'\n')
            .all(|line| !line.starts_with(b"~")),
        "OpenSSH consumes tilde escape commands at the start of a request line"
    );
}

#[test]
fn wire_rejects_unbounded_truncated_and_reordered_frames() {
    let mut reader = std::io::Cursor::new(vec![b'x'; wire::MAX_LINE + 1]);
    assert!(wire::read_line(&mut reader, &mut Vec::new()).is_err());
    let mut reader = std::io::Cursor::new(b"no newline");
    assert!(wire::read_line(&mut reader, &mut Vec::new()).is_err());
    let mut decoder = wire::Decoder::new('S', 1);
    assert!(
        decoder
            .line(format!("VTCR{VERSION}S:0000000000000001:0000000000000001:1:1:AA\n").as_bytes())
            .is_err()
    );
}

#[test]
fn remote_file_index_reuses_versions_and_quick_open_ranks_locally() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("alpha.rs"), "one").unwrap();
    std::fs::write(dir.path().join("beta.txt"), "two").unwrap();
    let traffic = Arc::new(AtomicUsize::new(0));
    let client = connection(dir.path(), Arc::default(), traffic.clone());
    let Reply::Files {
        version,
        files: Some(files),
    } = client
        .call(Command::Files {
            generation: 1,
            known_version: None,
            excludes: Default::default(),
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(files.len(), 2);
    assert!(matches!(
        client
            .call(Command::Files {
                generation: 2,
                known_version: Some(version),
                excludes: Default::default(),
            })
            .unwrap(),
        Reply::Files { files: None, .. }
    ));
    let workspace = workspace::Workspace::new(client);
    let mut app = crate::app::App::new_deferred(workspace.view_root.clone());
    app.remote_workspace = Some(workspace.clone());
    app.source_control_enabled = false;
    app.quick_open_query = "alp".into();
    app.update_quick_open();
    app.wait_for_background(Duration::from_secs(10), |app| !app.bg_quick_open_running);
    assert_eq!(
        app.quick_open_results,
        vec![workspace.view_key(&RemotePath::new(b"alpha.rs".to_vec()).unwrap())]
    );
    let initial = traffic.load(Ordering::Relaxed);
    app.quick_open_query = "bet".into();
    app.update_quick_open();
    app.wait_for_background(Duration::from_secs(10), |app| !app.bg_quick_open_running);
    assert_eq!(
        app.quick_open_results,
        vec![workspace.view_key(&RemotePath::new(b"beta.txt".to_vec()).unwrap())]
    );
    assert_eq!(traffic.load(Ordering::Relaxed), initial);
}

#[test]
fn remote_explorer_batches_expanded_folders_and_preserves_native_sort_filter_and_ignore_rules() {
    let dir = tempfile::tempdir().unwrap();
    git2::Repository::init(dir.path()).unwrap();
    std::fs::write(dir.path().join(".gitignore"), "generated/\n").unwrap();
    std::fs::create_dir(dir.path().join("generated")).unwrap();
    std::fs::write(dir.path().join("generated/cache.rs"), "ignored").unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
    for index in 0..20 {
        let path = dir.path().join(format!("folder{index:02}"));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("界.rs"), "code").unwrap();
    }
    let traffic = Arc::new(AtomicUsize::new(0));
    let workspace =
        workspace::Workspace::new(connection(dir.path(), Arc::default(), traffic.clone()));
    let mut expanded: rustc_hash::FxHashSet<_> = (0..20)
        .map(|index| workspace.target_key(&format!("folder{index:02}")).unwrap())
        .collect();
    expanded.insert(workspace.target_key("generated").unwrap());
    let before = traffic.load(Ordering::Relaxed);
    let cancel = crate::background::CancellationToken::new();
    let (tree, matches) = workspace
        .explorer_tree(
            &expanded,
            crate::explorer::ExplorerSortMode::Name,
            None,
            true,
            &crate::settings::default_files_exclude(),
            &cancel,
        )
        .unwrap();
    assert!(matches.error.is_none());
    assert_eq!(
        tree.iter().filter(|entry| entry.name == "界.rs").count(),
        20
    );
    assert!(
        traffic.load(Ordering::Relaxed) - before < 4096,
        "An expanded tree uses one compact request, not one round trip per folder"
    );
    assert!(
        tree.iter()
            .filter(|entry| entry.name == "generated" || entry.name == "cache.rs")
            .all(|entry| entry.git_status == crate::explorer::GitStatus::Ignored)
    );
    let find = crate::explorer_find::Query {
        text: "界".into(),
        filter: true,
        fuzzy: false,
        regex: false,
        collapsed: Default::default(),
    };
    let (filtered, matches) = workspace
        .explorer_tree(
            &expanded,
            crate::explorer::ExplorerSortMode::Type,
            Some(find),
            false,
            &crate::settings::default_files_exclude(),
            &cancel,
        )
        .unwrap();
    assert_eq!(matches.count, 20);
    assert!(
        filtered
            .iter()
            .all(|entry| entry.is_dir || entry.name == "界.rs")
    );
    assert!(matches.filtered_tree.is_some());
    std::fs::write(dir.path().join("new.rs"), "new").unwrap();
    workspace.invalidate_directories();
    let (updated, _) = workspace
        .explorer_tree(
            &expanded,
            crate::explorer::ExplorerSortMode::Name,
            None,
            false,
            &crate::settings::default_files_exclude(),
            &cancel,
        )
        .unwrap();
    assert!(updated.iter().any(|entry| entry.name == "new.rs"));
    assert!(
        updated
            .iter()
            .all(|entry| entry.git_status != crate::explorer::GitStatus::Ignored)
    );
    let Reply::Tasks(tasks) = workspace.client.call(Command::Tasks).unwrap() else {
        panic!()
    };
    assert!(
        tasks
            .iter()
            .any(|task| task.command == "cargo test" && task.source == "cargo")
    );
    workspace.client.call(Command::Shutdown).unwrap();
}

#[test]
fn remote_workspace_search_uses_unsaved_local_buffers_instead_of_disk() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("open.txt"), "needle on disk\n").unwrap();
    std::fs::write(dir.path().join("closed.txt"), "needle in closed file\n").unwrap();
    let client = connection(dir.path(), Arc::default(), Arc::default());
    let workspace = workspace::Workspace::new(client);
    let key = workspace.view_key(&RemotePath::new(b"open.txt".to_vec()).unwrap());
    let closed = workspace.view_key(&RemotePath::new(b"closed.txt".to_vec()).unwrap());
    let mut app = crate::app::App::new_deferred(workspace.view_root.clone());
    app.remote_workspace = Some(workspace);
    app.source_control_enabled = false;
    app.open_file(&key);
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.editor().is_some_and(|e| !e.bg_loading)
    });
    app.editor_mut().unwrap().select_all();
    app.editor_mut()
        .unwrap()
        .paste_with_text("no match in the local buffer\n");
    app.global_search_query = "needle".into();
    app.update_global_search();
    app.wait_for_background(Duration::from_secs(10), |app| !app.bg_global_search_running);
    assert!(
        app.global_search_error.is_none(),
        "{:?}",
        app.global_search_error
    );
    assert_eq!(app.global_search_results.len(), 1);
    assert_eq!(app.global_search_results[0].file_path, closed);
    app.editor_mut().unwrap().select_all();
    app.editor_mut()
        .unwrap()
        .paste_with_text("needle only in the local buffer\n");
    app.update_global_search();
    app.wait_for_background(Duration::from_secs(10), |app| !app.bg_global_search_running);
    assert_eq!(app.global_search_results.len(), 2);
    assert!(
        app.global_search_results
            .iter()
            .any(|hit| hit.file_path == key && hit.line_text.contains("only in the local buffer"))
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("open.txt")).unwrap(),
        "needle on disk\n"
    );
}

#[test]
fn remote_git_repositories_stage_and_unstage_use_native_shared_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    std::fs::write(dir.path().join("file[1].rs"), "one\n").unwrap();
    let client = connection(dir.path(), Arc::default(), Arc::default());
    let Reply::GitStatus { discovery, scans } = client
        .call(Command::GitStatus {
            changed: None,
            include_untracked: true,
            known: None,
            parent_open: false,
            closed: Vec::new(),
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(discovery.roots.len(), 1);
    assert_eq!(scans.len(), 1);
    let changes = scans[0].files.clone();
    assert_eq!(changes.len(), 1);
    client
        .call(Command::GitAction {
            repo: RemotePath::default(),
            task: crate::git_tasks::Task::Stage(changes),
        })
        .unwrap();
    assert!(
        repo.index()
            .unwrap()
            .get_path(Path::new("file[1].rs"), 0)
            .is_some()
    );
    client
        .call(Command::GitAction {
            repo: RemotePath::default(),
            task: crate::git_tasks::Task::UnstageAll,
        })
        .unwrap();
    let mut index = repo.index().unwrap();
    index.read(true).unwrap();
    assert!(index.is_empty());
}

#[test]
fn remote_history_pages_retain_the_walk_and_finish_unborn_repositories() {
    for count in [0, 215] {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let tree = repo
            .find_tree(repo.index().unwrap().write_tree().unwrap())
            .unwrap();
        let signature = git2::Signature::now("Remote test", "remote@example.test").unwrap();
        let mut parent = None;
        let mut ids = Vec::new();
        for index in 0..count {
            let parents: Vec<_> = parent.iter().collect();
            let id = repo
                .commit(
                    Some("HEAD"),
                    &signature,
                    &signature,
                    &format!("Commit {index}"),
                    &tree,
                    &parents,
                )
                .unwrap();
            ids.push(id.to_string());
            parent = Some(repo.find_commit(id).unwrap());
        }
        let workspace =
            workspace::Workspace::new(connection(dir.path(), Arc::default(), Arc::default()));
        let mut app = app_for_workspace(&workspace);
        app.source_control_enabled = true;
        app.refresh_git_log_async();
        app.wait_for_background(Duration::from_secs(10), |app| !app.bg_git_log_running);
        assert!(
            app.git_history.error.is_none(),
            "{:?}",
            app.git_history.error
        );
        assert_eq!(app.git_log.len(), count.min(crate::git_history::PAGE_SIZE));
        if count != 0 {
            assert!(app.git_history.has_more);
            app.load_more_git_history();
            app.wait_for_background(Duration::from_secs(10), |app| !app.bg_git_log_running);
            assert!(
                app.git_history.error.is_none(),
                "{:?}",
                app.git_history.error
            );
            assert_eq!(
                app.git_log
                    .iter()
                    .map(|commit| &commit.full_hash)
                    .collect::<Vec<_>>(),
                ids.iter().rev().collect::<Vec<_>>()
            );
        }
        assert!(!app.git_history.has_more);
        workspace.client.call(Command::Shutdown).unwrap();
    }
}

#[test]
fn remote_branch_stash_amend_and_push_share_the_native_repository_implementation() {
    let dir = tempfile::tempdir().unwrap();
    let bare = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    repo.config()
        .unwrap()
        .set_bool("core.autocrlf", false)
        .unwrap();
    let file = dir.path().join("code.rs");
    std::fs::write(&file, "one\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("code.rs")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = git2::Signature::now("Remote test", "remote@example.test").unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
        .unwrap();
    let original = repo.head().unwrap().shorthand().unwrap().to_owned();
    git2::Repository::init_bare(bare.path()).unwrap();
    repo.remote("origin", bare.path().to_str().unwrap())
        .unwrap();
    let client = connection(dir.path(), Arc::default(), Arc::default());
    let action = |task| {
        assert!(matches!(
            client
                .call(Command::GitAction {
                    repo: RemotePath::default(),
                    task
                })
                .unwrap(),
            Reply::GitAction(_)
        ));
    };
    action(crate::git_tasks::Task::CreateBranch {
        branch_name: "feature/remote".into(),
    });
    let Reply::GitBranches(branches) = client
        .call(Command::GitBranches {
            repo: RemotePath::default(),
        })
        .unwrap()
    else {
        panic!()
    };
    assert!(branches.iter().any(|branch| matches!(branch, crate::app::GitBranchPickerItem::Local { name } if name == "feature/remote")));
    action(crate::git_tasks::Task::CheckoutLocal {
        branch_name: original.clone(),
    });
    std::fs::write(&file, "two\n").unwrap();
    action(crate::git_tasks::Task::Stash {
        stash_message: "test".into(),
        label: "test".into(),
    });
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "one\n");
    action(crate::git_tasks::Task::StashPop);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "two\n");
    action(crate::git_tasks::Task::StageAll);
    action(crate::git_tasks::Task::Amend {
        message: "amended remotely".into(),
    });
    assert_eq!(
        git2::Repository::open(dir.path())
            .unwrap()
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .summary(),
        Some("amended remotely")
    );
    let Reply::GitNetwork(pushed) = client
        .call(Command::GitNetwork {
            repo: RemotePath::default(),
            args: Vec::new(),
            push: true,
        })
        .unwrap()
    else {
        panic!()
    };
    assert!(pushed.success, "{}", pushed.stderr);
    let remote = git2::Repository::open_bare(bare.path()).unwrap();
    assert!(
        remote
            .find_reference(&format!("refs/heads/{original}"))
            .is_ok()
    );
    client.call(Command::Shutdown).unwrap();
}

#[test]
fn remote_source_control_diff_refresh_uses_remote_worktree_and_index() {
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let native = dir.path().join("code.rs");
    std::fs::write(&native, "one\nbase\nend\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("code.rs")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = git2::Signature::now("Remote test", "remote@example.test").unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
        .unwrap();
    std::fs::write(&native, "one\nchanged\nend\n").unwrap();
    let workspace =
        workspace::Workspace::new(connection(dir.path(), Arc::default(), Arc::default()));
    let mut app = app_for_workspace(&workspace);
    app.source_control_enabled = true;
    let key = workspace.target_key("code.rs").unwrap();
    app.open_source_control_diff_in_repository(
        &key,
        false,
        false,
        Some(workspace.view_root.clone()),
    );
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.compare_base_content.is_some() && !app.bg_git_compare_running
    });
    assert_eq!(
        app.compare_base_content.as_deref(),
        Some("one\nbase\nend\n")
    );
    assert!(
        app.compare_diff_lines
            .iter()
            .any(|(kind, text)| *kind == '+' && text == "changed")
    );
    std::fs::write(&native, "one\nsecond\nend\n").unwrap();
    app.refresh_source_control_diffs();
    app.wait_for_background(Duration::from_secs(10), |app| {
        !app.diff_tabs.refresh_running
            && app
                .editor()
                .is_some_and(|e| e.rope.to_string() == "one\nsecond\nend\n")
    });
    assert!(
        app.compare_diff_lines
            .iter()
            .any(|(kind, text)| *kind == '+' && text == "second")
    );
    workspace.client.call(Command::Shutdown).unwrap();
}

#[test]
fn remote_commit_details_and_hunks_are_exact_async_and_undoable() {
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    repo.config()
        .unwrap()
        .set_bool("core.autocrlf", false)
        .unwrap();
    let native = dir.path().join("file[1].rs");
    let original = "a\nbase\nc\nd\ne\nbase2\ng\n";
    let changed = "a\nchanged\nc\nd\ne\nchanged2\ng\n";
    std::fs::write(&native, original).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("file[1].rs")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = git2::Signature::now("Remote test", "remote@example.test").unwrap();
    let hash = repo
        .commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
        .unwrap()
        .to_string();
    std::fs::write(&native, changed).unwrap();
    let workspace =
        workspace::Workspace::new(connection(dir.path(), Arc::default(), Arc::default()));
    let details = workspace
        .commit_details(&workspace.view_root, hash.clone())
        .unwrap();
    assert_eq!(details.hash, hash);
    assert_eq!(details.files.len(), 1);
    let diff = workspace
        .commit_diff(
            &workspace.view_root,
            hash,
            details.files[0].clone(),
            1024 * 1024,
        )
        .unwrap();
    let key = workspace.target_key("file[1].rs").unwrap();
    assert_eq!(diff.path, key);
    assert_eq!(diff.target.to_string(), original);
    assert_eq!(diff.prepared.base_content.as_ref(), "");
    let mut app = app_for_workspace(&workspace);
    app.source_control_enabled = true;
    app.open_file(&key);
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.editor().is_some_and(|e| !e.bg_loading)
    });
    let index_text = || {
        let fresh = git2::Repository::open(dir.path()).unwrap();
        let index = fresh.index().unwrap();
        let blob = fresh
            .find_blob(index.get_path(Path::new("file[1].rs"), 0).unwrap().id)
            .unwrap();
        String::from_utf8(blob.content().to_vec()).unwrap()
    };
    app.open_hunk_popup(0, 0, key.clone(), 1, 'U');
    app.wait_for_background(Duration::from_secs(10), |app| app.hunk_peek.is_some());
    app.execute_hunk_popup_action(0);
    app.wait_for_background(Duration::from_secs(10), |app| !app.bg_git_action_running);
    assert_eq!(index_text(), "a\nchanged\nc\nd\ne\nbase2\ng\n");
    app.open_hunk_popup(0, 0, key.clone(), 1, 'S');
    app.wait_for_background(Duration::from_secs(10), |app| app.hunk_peek.is_some());
    app.execute_hunk_popup_action(0);
    app.wait_for_background(Duration::from_secs(10), |app| !app.bg_git_action_running);
    assert_eq!(index_text(), original);
    app.open_hunk_popup(0, 0, key.clone(), 1, 'U');
    app.wait_for_background(Duration::from_secs(10), |app| app.hunk_peek.is_some());
    assert_eq!(app.hunk_peek.as_ref().unwrap().source.hunks.len(), 2);
    // Replacing the exact previewed hunk externally cannot stage another edit.
    std::fs::write(&native, changed.replace("changed\n", "replacement\n")).unwrap();
    app.execute_hunk_popup_action(0);
    app.wait_for_background(Duration::from_secs(10), |app| !app.bg_git_action_running);
    let index = git2::Repository::open(dir.path()).unwrap().index().unwrap();
    let blob = repo
        .find_blob(index.get_path(Path::new("file[1].rs"), 0).unwrap().id)
        .unwrap();
    assert_eq!(blob.content(), original.as_bytes());
    std::fs::write(&native, changed).unwrap();
    app.open_hunk_popup(0, 0, key.clone(), 1, 'U');
    app.wait_for_background(Duration::from_secs(10), |app| app.hunk_peek.is_some());
    app.execute_hunk_popup_action(1); // discard one saved hunk
    let after = "a\nbase\nc\nd\ne\nchanged2\ng\n";
    app.wait_for_background(Duration::from_secs(10), |app| {
        !app.bg_git_action_running
            && app
                .editor()
                .is_some_and(|editor| editor.rope.to_string() == after)
    });
    assert_eq!(std::fs::read_to_string(&native).unwrap(), after);
    assert_eq!(app.editor().unwrap().rope.to_string(), after);
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope.to_string(), changed);
    assert!(app.editor().unwrap().modified);
    app.editor_mut().unwrap().redo();
    assert!(!app.editor().unwrap().modified);
    // Closing an outstanding preview invalidates its delayed result.
    app.open_hunk_popup(0, 0, key, 5, 'U');
    app.close_hunk_popup();
    app.wait_for_background(Duration::from_secs(10), |app| {
        !app.background.has_active_jobs()
    });
    assert!(app.hunk_peek.is_none());
    assert!(!app.hunk_popup_active);
    workspace.client.call(Command::Shutdown).unwrap();
}

#[test]
fn remote_formatter_uses_remote_snapshot_and_preserves_local_undo() {
    let dir = tempfile::tempdir().unwrap();
    let native = dir.path().join("code.txt");
    std::fs::write(&native, "saved\n").unwrap();
    let workspace =
        workspace::Workspace::new(connection(dir.path(), Arc::default(), Arc::default()));
    let mut app = app_for_workspace(&workspace);
    let key = workspace.target_key("code.txt").unwrap();
    app.open_file(&key);
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.editor().is_some_and(|e| !e.bg_loading)
    });
    app.editor_mut().unwrap().insert_char('X');
    let before = app.editor().unwrap().rope.clone();
    #[cfg(windows)]
    let (command,args) = ("powershell.exe".to_owned(), vec!["-NoProfile".into(), "-NonInteractive".into(), "-Command".into(),
        "$text = [Console]::In.ReadToEnd(); if ($text -ne \"Xsaved`n\") { exit 7 }; [Console]::Out.Write(\"remote formatted`n\")".into()]);
    #[cfg(not(windows))]
    let (command, args) = (
        "/bin/sh".to_owned(),
        vec![
            "-c".into(),
            "text=$(cat); test \"$text\" = Xsaved || exit 7; printf 'remote formatted\\n'".into(),
        ],
    );
    app.formatters.insert(
        crate::formatters::language(app.editor().unwrap()),
        crate::formatters::Config {
            provider: crate::formatters::Provider::Command,
            mode: crate::formatters::CommandMode::Stdin,
            command,
            args,
        },
    );
    assert!(app.request_format(false, false));
    let start = Instant::now();
    while app.is_formatting() && start.elapsed() < Duration::from_secs(10) {
        app.poll_external_format();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(app.editor().unwrap().rope.to_string(), "remote formatted\n");
    assert_eq!(std::fs::read_to_string(native).unwrap(), "saved\n");
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope, before);
    workspace.client.call(Command::Shutdown).unwrap();
}

#[test]
fn remote_blame_caches_committed_content_and_navigates_history_without_edit_traffic() {
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    repo.config()
        .unwrap()
        .set_bool("core.autocrlf", false)
        .unwrap();
    let file = dir.path().join("file 文.rs");
    let commit = |text: &str, name: &str| {
        std::fs::write(&file, text).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file 文.rs")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now(name, "remote@example.test").unwrap();
        let parent = repo.head().and_then(|head| head.peel_to_commit()).ok();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            name,
            &tree,
            &parent.iter().collect::<Vec<_>>(),
        )
        .unwrap()
    };
    let original = "one\n界\nthree\n";
    let current = "one\n界 new\nthree\n";
    commit(original, "First");
    commit(current, "Second");
    let bytes = Arc::new(AtomicUsize::new(0));
    let workspace =
        workspace::Workspace::new(connection(dir.path(), Arc::default(), bytes.clone()));
    let key = workspace.target_key("file 文.rs").unwrap();
    let cancel = crate::background::CancellationToken::new();
    let mut first_transfer = None;
    for text in [
        current,
        "inserted\none\n界 new\nthree\n",
        "one\r\n界 new\r\nthree\r\n",
        "one\n界\nthree\n",
        current,
    ] {
        let rope = Rope::from_str(text);
        let remote = workspace.prepare_blame(key.clone(), rope.clone(), None, &cancel);
        assert!(remote.notice.is_none(), "{:?}", remote.notice);
        let local = crate::git_blame::prepare(file.clone(), rope, None);
        let labels = |document: &crate::git_blame::Document| {
            document
                .lines
                .iter()
                .map(|line| match line {
                    crate::git_blame::Line::Committed(commit) => commit.hash.clone(),
                    crate::git_blame::Line::Empty => "empty".into(),
                    _ => "uncommitted".into(),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(labels(&remote), labels(&local), "{text:?}");
        assert_eq!(remote.commit_lines, local.commit_lines);
        if let Some(previous) = first_transfer {
            assert_eq!(
                bytes.load(Ordering::Relaxed),
                previous,
                "Local blame comparisons do not transmit text or rerun remote blame"
            );
        }
        first_transfer = Some(bytes.load(Ordering::Relaxed));
    }
    let document = workspace.prepare_blame(key.clone(), Rope::from_str(current), None, &cancel);
    let crate::git_blame::Line::Committed(commit) = &document.lines[1] else {
        panic!()
    };
    let history = workspace
        .blame_history(&key, commit, 1, 1024 * 1024, None)
        .unwrap();
    assert_eq!(history.contents.to_string(), current);
    assert_eq!(history.diff.base_content.as_ref(), original);
    let snapshot = workspace
        .blame_line_snapshot(&key, commit, None, 1, 1024 * 1024)
        .unwrap();
    assert_eq!(snapshot.document.rope.to_string(), current);
    assert_eq!(snapshot.document.path, key);
    let previous = workspace
        .blame_snapshot(snapshot.previous_revision.as_ref().unwrap(), 0, 1024 * 1024)
        .unwrap();
    assert_eq!(previous.document.rope.to_string(), original);
    assert!(previous.previous_revision.is_none());
    workspace.client.call(Command::Shutdown).unwrap();
}

#[test]
fn service_save_is_exact_conflict_safe_and_supports_new_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.py"), "old\r\n🐱\n").unwrap();
    let path = RemotePath::new(b"test.py".to_vec()).unwrap();
    let mut service = service::Service::new(dir.path()).unwrap();
    let Reply::Opened {
        text,
        revision: base,
        ..
    } = service
        .execute(Command::Open { path: path.clone() })
        .unwrap()
    else {
        panic!()
    };
    let old = Rope::from_str(&text);
    let new = Rope::from_str("new\r\n🐱\n");
    service
        .execute(Command::Close { path: path.clone() })
        .unwrap(); // Cache eviction must not invalidate a safe save.
    let Reply::Saved { revision: next, .. } = service
        .execute(Command::Save {
            path: path.clone(),
            base: Some(base.clone()),
            change: Change::between(&old, &new),
            encoding: "UTF-8".into(),
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        std::fs::read_to_string(dir.path().join("test.py")).unwrap(),
        new.to_string()
    );
    assert!(
        service
            .execute(Command::Save {
                path: path.clone(),
                base: Some(base),
                change: Change::between(&old, &new),
                encoding: "UTF-8".into()
            })
            .is_err()
    );
    std::fs::write(dir.path().join("test.py"), "externally changed").unwrap();
    assert!(
        service
            .execute(Command::Save {
                path: path.clone(),
                base: Some(next),
                change: Change::between(&new, &old),
                encoding: "UTF-8".into()
            })
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("test.py")).unwrap(),
        "externally changed"
    );
    let new_path = RemotePath::new(b"new.txt".to_vec()).unwrap();
    service
        .execute(Command::Save {
            path: new_path,
            base: None,
            change: Change::between(&Rope::new(), &new),
            encoding: "UTF-8 with BOM".into(),
        })
        .unwrap();
    assert!(
        std::fs::read(dir.path().join("new.txt"))
            .unwrap()
            .starts_with(&[0xef, 0xbb, 0xbf])
    );
}

#[test]
fn remote_save_conflict_keeps_typed_reason_and_supports_native_compare_and_explicit_overwrite() {
    for evict in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let native = root.path().join("file.txt");
        std::fs::write(&native, "original\n").unwrap();
        let workspace = workspace::Workspace::new(connection(root.path(), Arc::default(), Arc::default()));
        let mut app = app_for_workspace(&workspace);
        app.format_on_save = false;
        let key = workspace.target_key("file.txt").unwrap();
        app.open_file(&key);
        app.wait_for_background(Duration::from_secs(10), |app| app.editor().is_some_and(|editor| !editor.bg_loading));
        app.editor_mut().unwrap().insert_char('X');
        if evict { workspace.client.call(Command::Close { path: workspace.remote_path(&key).unwrap() }).unwrap(); }
        std::fs::write(&native, "external\n").unwrap();
        let request = app.editor().unwrap().background_save_request(false, false, false).unwrap();
        let error = workspace.save_progress(&request, None, None, |_, _| {}).err().unwrap();
        assert!(crate::editor::save::is_conflict(&error), "{error}");
        app.save_current_file();
        app.wait_for_background(Duration::from_secs(10), |app| app.pending_file_saves.is_empty());
        assert!(app.save_conflict.is_some());
        app.view_save_conflict();
        app.wait_for_background(Duration::from_secs(10), |app| app.save_error.is_none());
        assert_eq!(app.compare_base_content.as_deref(), Some("external\n"));
        assert_eq!(app.editor().unwrap().rope, "Xoriginal\n");
        assert_eq!(std::fs::read_to_string(&native).unwrap(), "external\n");
        app.active_tab = 0;
        app.sync_diff_tab();
        app.save_current_file();
        app.wait_for_background(Duration::from_secs(10), |app| app.pending_file_saves.is_empty());
        assert!(app.save_conflict.is_some());
        app.overwrite_save_conflict();
        app.wait_for_background(Duration::from_secs(10), |app| app.pending_file_saves.is_empty());
        assert!(app.save_error.is_none(), "{:?}", app.save_error);
        assert_eq!(std::fs::read_to_string(&native).unwrap(), "Xoriginal\n");
        assert!(!app.editor().unwrap().modified);
        assert!(app.editor().unwrap().remote_checkpoint.as_ref().is_some_and(|checkpoint| !checkpoint.conflicted));
        app.editor_mut().unwrap().undo();
        assert_eq!(app.editor().unwrap().rope, "original\n");
        assert!(app.editor().unwrap().modified);
        app.editor_mut().unwrap().redo();
        assert!(!app.editor().unwrap().modified);
        workspace.client.call(Command::Shutdown).unwrap();
    }
}

#[test]
fn remote_native_watcher_reloads_clean_files_and_preserves_conflicting_local_edits() {
    let dir = tempfile::tempdir().unwrap();
    let native = dir.path().join("watched.rs");
    std::fs::write(&native, "one\n").unwrap();
    let workspace =
        workspace::Workspace::new(connection(dir.path(), Arc::default(), Arc::default()));
    let mut app = app_for_workspace(&workspace);
    let key = workspace.target_key("watched.rs").unwrap();
    app.open_file(&key);
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.editor().is_some_and(|e| !e.bg_loading)
    });
    workspace.client.call(Command::Watch).unwrap();
    app.wait_for_background(Duration::from_secs(10), |app| app.workspace_watcher_active);
    std::fs::write(&native, "external change\n").unwrap();
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.editor()
            .is_some_and(|e| e.rope.to_string() == "external change\n")
    });
    assert!(!app.editor().unwrap().modified);
    app.editor_mut().unwrap().insert_char('A');
    std::fs::write(&native, "another external change\n").unwrap();
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.editor().is_some_and(|e| e.file_size == 24)
    });
    assert_eq!(app.editor().unwrap().rope.to_string(), "Aexternal change\n");
    app.save_current_file();
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.pending_file_saves.is_empty()
    });
    assert!(app.editor().unwrap().modified);
    assert_eq!(
        std::fs::read_to_string(&native).unwrap(),
        "another external change\n"
    );
    assert_eq!(
        workspace
            .load(&workspace.target_key("missing.rs").unwrap())
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::NotFound
    );
    workspace.client.call(Command::Shutdown).unwrap();
}

fn app_for_workspace(workspace: &workspace::Workspace) -> crate::app::App {
    let mut app = crate::app::App::new_deferred(workspace.view_root.clone());
    workspace.client.set_wake(app.background.wake_handle());
    app.remote_workspace = Some(workspace.clone());
    app.source_control_enabled = false;
    app.trim_trailing_on_save = false;
    app.insert_final_newline = false;
    app
}

#[test]
fn workspace_context_keeps_old_reads_and_saves_in_their_original_folder() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("other folder 文");
    std::fs::create_dir(&nested).unwrap();
    std::fs::write(dir.path().join("same.txt"), "first 🦀\n").unwrap();
    std::fs::write(nested.join("same.txt"), "second 文\n").unwrap();
    let first = workspace::Workspace::with_connection_identity(
        connection(dir.path(), Arc::default(), Arc::default()),
        b"test-host",
    );
    let second = first
        .open_workspace(
            &first.view_key(&RemotePath::new("other folder 文".as_bytes().to_vec()).unwrap()),
        )
        .unwrap();
    let relative = RemotePath::new(b"same.txt".to_vec()).unwrap();
    let first_key = first.view_key(&relative);
    let second_key = second.view_key(&relative);
    assert_ne!(first_key, second_key);
    assert_eq!(
        first.load(&first_key).unwrap().rope.to_string(),
        "first 🦀\n"
    );
    assert_eq!(
        second.load(&second_key).unwrap().rope.to_string(),
        "second 文\n"
    );
    let reopened = first
        .open_workspace(
            &first.view_key(&RemotePath::new("other folder 文".as_bytes().to_vec()).unwrap()),
        )
        .unwrap();
    assert_eq!(
        reopened.view_root, second.view_root,
        "recovery identity is stable across folder switches"
    );
    let mut workers = Vec::new();
    for (workspace, original, replacement) in [
        (first.clone(), "first 🦀\n", "parent change\n"),
        (second.clone(), "second 文\n", "child change\n"),
    ] {
        let path = relative.clone();
        workers.push(std::thread::spawn(move || {
            workspace
                .client
                .call(Command::Save {
                    path,
                    base: Some(revision(&Rope::from_str(original), "UTF-8")),
                    change: Change::between(
                        &Rope::from_str(original),
                        &Rope::from_str(replacement),
                    ),
                    encoding: "UTF-8".into(),
                })
                .unwrap();
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("same.txt")).unwrap(),
        "parent change\n"
    );
    assert_eq!(
        std::fs::read_to_string(nested.join("same.txt")).unwrap(),
        "child change\n"
    );
    let Reply::Files {
        files: Some(files), ..
    } = second
        .client
        .call(Command::Files {
            generation: 1,
            known_version: None,
            excludes: Default::default(),
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(files, [relative]);
    assert!(
        first
            .open_workspace(&first.view_key(&RemotePath::new(b"missing".to_vec()).unwrap()))
            .is_err()
    );
    assert_eq!(
        first.load(&first_key).unwrap().rope.to_string(),
        "parent change\n"
    );
}

#[test]
fn remote_folder_switch_is_async_and_rejects_late_notifications_and_validations() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("second")).unwrap();
    std::fs::write(dir.path().join("same.txt"), "first\n").unwrap();
    std::fs::write(dir.path().join("second/same.txt"), "second\n").unwrap();
    let workspace =
        workspace::Workspace::new(connection(dir.path(), Arc::default(), Arc::default()));
    let second_key = workspace.view_key(&RemotePath::new(b"second".to_vec()).unwrap());
    let second = workspace.open_workspace(&second_key).unwrap();
    let mut app = app_for_workspace(&workspace);
    app.open_file(&workspace.target_key("same.txt").unwrap());
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.editor().is_some_and(|editor| !editor.bg_loading)
    });
    app.editor_mut().unwrap().insert_char('X');
    app.change_workspace(&workspace.target_key("missing").unwrap());
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.notification_history
            .iter()
            .any(|n| n.0.contains("Cannot open remote folder"))
    });
    assert_eq!(app.explorer.root, workspace.view_root);
    assert!(app.editor().unwrap().modified);
    let old_generation = app.workspace_generation;
    let started = Instant::now();
    app.change_workspace(&second_key);
    assert!(started.elapsed() < Duration::from_millis(100));
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.explorer.root == second.view_root
    });
    assert!(app.lsp_manager.remote_workspace.is_some());
    // A successful but superseded validation must not switch back.
    let _ =
        app.background
            .sender
            .send(crate::background::BackgroundResult::RemoteWorkspaceOpened {
                request: app.workspace_open_generation,
                source: old_generation,
                result: Ok(workspace.clone()),
            });
    app.poll_background_results();
    assert_eq!(app.explorer.root, second.view_root);
    app.open_file(&second.target_key("same.txt").unwrap());
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.editor().is_some_and(|editor| !editor.bg_loading)
    });
    assert_eq!(app.editor().unwrap().rope.to_string(), "second\n");
}

#[test]
fn local_settings_file_can_be_opened_and_saved_in_remote_workbench() {
    let remote_dir = tempfile::tempdir().unwrap();
    let local_dir = tempfile::tempdir().unwrap();
    let path = local_dir.path().join("settings.json");
    std::fs::write(&path, "{}\n").unwrap();
    let workspace = workspace::Workspace::new(connection(
        remote_dir.path(),
        Arc::default(),
        Arc::default(),
    ));
    let mut app = app_for_workspace(&workspace);
    app.open_file(&path);
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.editor().is_some_and(|editor| !editor.bg_loading)
    });
    assert_eq!(app.editor().unwrap().rope.to_string(), "{}\n");
    assert!(app.editor().unwrap().remote_checkpoint.is_none());
    app.editor_mut().unwrap().insert_char(' ');
    app.save_current_file();
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.pending_file_saves.is_empty()
    });
    assert_eq!(std::fs::read_to_string(&path).unwrap(), " {}\n");
    assert!(!app.editor().unwrap().modified);
}

#[test]
fn remote_open_preserves_native_binary_and_lossy_text_detection() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("binary.bin"), [0, 1, 2, 3]).unwrap();
    std::fs::write(dir.path().join("invalid.txt"), b"text\xff\n").unwrap();
    let workspace =
        workspace::Workspace::new(connection(dir.path(), Arc::default(), Arc::default()));
    let binary = workspace
        .load(&workspace.target_key("binary.bin").unwrap())
        .unwrap();
    assert_eq!(binary.encoding, "Binary");
    assert_eq!(binary.file_size, 4);
    assert!(binary.rope.to_string().contains("Binary file"));
    let lossy = workspace
        .load(&workspace.target_key("invalid.txt").unwrap())
        .unwrap();
    assert_eq!(lossy.encoding, "UTF-8 (lossy)");
    assert_eq!(lossy.rope.to_string(), "text\u{fffd}\n");
    assert!(
        encoding("Binary").is_err(),
        "a preview cannot accidentally become the saved binary content"
    );
    assert_eq!(
        std::fs::read(dir.path().join("binary.bin")).unwrap(),
        [0, 1, 2, 3]
    );
}

#[test]
fn remote_binary_piece_table_caches_reads_and_keeps_undo_across_saves() {
    use crate::extension_documents::BinaryDocument;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data.bin");
    let original: Vec<u8> = (0..2 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    std::fs::write(&file, &original).unwrap();
    let traffic = Arc::new(AtomicUsize::new(0));
    let workspace =
        workspace::Workspace::new(connection(dir.path(), Arc::default(), traffic.clone()));
    let key = workspace.target_key("data.bin").unwrap();
    let mut document = BinaryDocument::open_remote(&key, &workspace).unwrap();
    let mut page = vec![0; 256];
    document.read(0, &mut page).unwrap();
    assert_eq!(page, original[..256]);
    let fetched = traffic.load(Ordering::Relaxed);
    document.edit(3, 2, &[0xFE, 0xED, 0xAA]).unwrap();
    let mut expected = original.clone();
    expected.splice(3..5, [0xFE, 0xED, 0xAA]);
    document.read(0, &mut page).unwrap();
    assert_eq!(page, expected[..256]);
    assert!(document.undo());
    document.read(0, &mut page).unwrap();
    assert_eq!(page, original[..256]);
    assert!(document.redo());
    assert_eq!(
        traffic.load(Ordering::Relaxed),
        fetched,
        "cached reads, edits and undo must not require a network request"
    );
    document.save().unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), expected);
    assert!(
        traffic.load(Ordering::Relaxed) - fetched < 4096,
        "unchanged binary ranges are not uploaded"
    );
    let saved_traffic = traffic.load(Ordering::Relaxed);
    document.read(0, &mut page).unwrap();
    assert_eq!(
        traffic.load(Ordering::Relaxed),
        saved_traffic,
        "saving does not invalidate a resident viewport"
    );
    assert!(document.undo());
    document.save().unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), original);
    assert!(document.redo());
    document.save().unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), expected);
    let copy = workspace.target_key("copy.bin").unwrap();
    document.save_as(&copy, false).unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("copy.bin")).unwrap(),
        expected
    );
    assert!(!document.info().dirty);
    document.edit(0, 1, &[1, 2, 3]).unwrap();
    std::fs::write(dir.path().join("copy.bin"), b"external edit").unwrap();
    assert!(document.save().is_err());
    assert!(document.info().dirty);
    assert_eq!(
        std::fs::read(dir.path().join("copy.bin")).unwrap(),
        b"external edit"
    );
}

#[test]
fn remote_binary_recovery_detects_replaced_and_missing_originals() {
    use crate::extension_documents::BinaryDocument;
    let dir = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("recover.bin"), [1, 2, 3, 4]).unwrap();
    let workspace =
        workspace::Workspace::new(connection(dir.path(), Arc::default(), Arc::default()));
    let key = workspace.target_key("recover.bin").unwrap();
    let mut document = BinaryDocument::open_remote(&key, &workspace).unwrap();
    document.edit(1, 1, &[0xAB]).unwrap();
    document
        .keep_for_session(local.path(), "extension", "open", 1)
        .unwrap();
    let record = crate::extension_recovery::read(local.path()).pop().unwrap();
    std::fs::write(dir.path().join("recover.bin"), [7, 8]).unwrap();
    let mut restored = BinaryDocument::restore_remote(&record, &workspace).unwrap();
    let mut bytes = [0; 4];
    assert_eq!(restored.read(0, &mut bytes).unwrap(), 4);
    assert_eq!(bytes, [1, 0xAB, 3, 4]);
    assert!(restored.save().is_err());
    assert_eq!(
        std::fs::read(dir.path().join("recover.bin")).unwrap(),
        [7, 8]
    );
    std::fs::remove_file(dir.path().join("recover.bin")).unwrap();
    std::fs::create_dir(dir.path().join("recover.bin")).unwrap();
    let mut replaced = BinaryDocument::restore_remote(&record, &workspace).unwrap();
    assert!(replaced.save().is_err());
    assert_eq!(replaced.read(0, &mut bytes).unwrap(), 4);
    assert_eq!(bytes, [1, 0xAB, 3, 4]);
    std::fs::remove_dir(dir.path().join("recover.bin")).unwrap();
    let mut restored = BinaryDocument::restore_remote(&record, &workspace).unwrap();
    assert!(restored.save().is_err());
    restored
        .save_as(&workspace.target_key("rescued.bin").unwrap(), false)
        .unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("rescued.bin")).unwrap(),
        [1, 0xAB, 3, 4]
    );
}

#[test]
fn text_files_larger_than_64_mib_open_and_save_through_bounded_chunks() {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("large.txt");
    let chunk = "\"quoted\"\\path\t🦀 café\r\n".repeat(2048);
    let blocks = (64 * 1024 * 1024) / chunk.len() + 3;
    let mut file = std::fs::File::create(&path).unwrap();
    for _ in 0..blocks {
        file.write_all(chunk.as_bytes()).unwrap();
    }
    drop(file);
    assert!(std::fs::metadata(&path).unwrap().len() > 64 * 1024 * 1024);
    let workspace =
        workspace::Workspace::new(connection(dir.path(), Arc::default(), Arc::default()));
    let key = workspace.target_key("large.txt").unwrap();
    let mut last = (0, 0);
    let loaded = workspace
        .load_progress(&key, |done, total| {
            assert!(done >= last.0);
            assert!(done <= total);
            last = (done, total);
        })
        .unwrap();
    assert_eq!(last.0, loaded.rope.len_bytes() as u64);
    assert_eq!(last.0, last.1);
    assert_eq!(
        loaded.rope.slice(..chunk.chars().count()).to_string(),
        chunk
    );
    #[cfg(windows)]
    let (command, args) = (
        "powershell.exe".to_owned(),
        vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-Command".into(),
            "[Console]::OpenStandardInput().CopyTo([Console]::OpenStandardOutput())".into(),
        ],
    );
    #[cfg(not(windows))]
    let (command, args) = ("/bin/cat".to_owned(), Vec::new());
    let formatted = workspace
        .format_document(
            &key,
            crate::formatters::Config {
                provider: crate::formatters::Provider::Command,
                mode: crate::formatters::CommandMode::Stdin,
                command,
                args,
            },
            &loaded.rope,
            None,
            &crate::background::CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(formatted.len(), loaded.rope.len_bytes());
    assert_eq!(
        Rope::from_str(&formatted),
        loaded.rope,
        "formatter input and output stream past 64 MiB too"
    );
    drop(formatted);
    let replacement = "\"changed\"\\file\t文 🦀\r\n".repeat(2048);
    let mut builder = ropey::RopeBuilder::new();
    for _ in 0..blocks + 50 {
        builder.append(&replacement);
    }
    let replacement = builder.finish();
    assert!(replacement.len_bytes() > 64 * 1024 * 1024);
    let checkpoint = loaded.remote_checkpoint.clone().unwrap();
    let baseline = loaded.rope.clone();
    let request = crate::editor::BackgroundSaveRequest {
        prepared_options: None,
        source_document_id: 1,
        path: key,
        rope: replacement.clone(),
        encoding: loaded.encoding,
        line_ending: "CRLF",
        trim_trailing: false,
        insert_final_newline: false,
        source_content_version: 1,
        fail_if_exists: false,
        expected_disk: Some(crate::editor::DiskSaveBaseline {
            rope: baseline.clone(),
            encoding: loaded.encoding,
        }),
    };
    last = (0, 0);
    let saved = workspace
        .save_progress(
            &request,
            Some((checkpoint, baseline, loaded.encoding)),
            None,
            |done, total| {
                assert!(done >= last.0 && done <= total);
                last = (done, total);
            },
        )
        .unwrap();
    assert!(last.0 > 64 * 1024 * 1024);
    assert_eq!(last.0, last.1);
    assert_eq!(saved.rope, replacement);
    let mut expected = Sha256::new();
    for chunk in replacement.chunks() {
        expected.update(chunk.as_bytes());
    }
    let mut actual = Sha256::new();
    let mut file = std::fs::File::open(&path).unwrap();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        actual.update(&buffer[..count]);
    }
    assert_eq!(
        actual.finalize(),
        expected.finalize(),
        "all saved bytes, including CRLF and Unicode, are exact"
    );
}

#[test]
fn interrupted_and_conflicting_streamed_saves_never_publish_partial_text() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file.txt");
    std::fs::write(&file, "original\r\n").unwrap();
    let mut service = service::Service::new(dir.path()).unwrap();
    let path = RemotePath::new(b"file.txt".to_vec()).unwrap();
    let Reply::Opened { revision, .. } = service
        .execute(Command::Open { path: path.clone() })
        .unwrap()
    else {
        panic!()
    };
    let begin = |service: &mut service::Service| {
        let Reply::Text(text::Reply::Upload { transfer }) = service
            .execute(Command::Text(text::Command::BeginSave {
                path: path.clone(),
                base: Some(revision.clone()),
                start: 0,
                end: 10,
                characters: 4,
                bytes: 7,
                encoding: "UTF-8".into(),
            }))
            .unwrap()
        else {
            panic!()
        };
        transfer
    };
    let transfer = begin(&mut service);
    assert!(
        service
            .execute(Command::Text(text::Command::Append {
                transfer,
                offset: 2,
                text: "ab".into()
            }))
            .is_err()
    );
    service
        .execute(Command::Text(text::Command::Append {
            transfer,
            offset: 0,
            text: "🦀".into(),
        }))
        .unwrap();
    assert!(
        service
            .execute(Command::Text(text::Command::Finish { transfer }))
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "original\r\n");
    let transfer = begin(&mut service);
    service
        .execute(Command::Text(text::Command::Append {
            transfer,
            offset: 0,
            text: "🦀abc".into(),
        }))
        .unwrap();
    std::fs::write(&file, "external edit").unwrap();
    assert!(
        service
            .execute(Command::Text(text::Command::Finish { transfer }))
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "external edit");
}

#[test]
fn cancelled_transfer_headers_release_their_remote_slots() {
    let dir = tempfile::tempdir().unwrap();
    let hold = Arc::new(AtomicBool::new(false));
    let blocked = Arc::new(AtomicBool::new(false));
    let client = connection_with_status(dir.path(), hold.clone(), Arc::default(), blocked.clone());
    struct Release(Arc<AtomicBool>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    let _release = Release(hold.clone());
    for _ in 0..16 {
        hold.store(true, Ordering::Release);
        let worker = client.clone();
        let cancel = crate::background::CancellationToken::new();
        let flag = cancel.clone();
        let worker = std::thread::spawn(move || {
            worker.call_cancellable(
                Command::Text(text::Command::BeginBlob {
                    characters: 1,
                    bytes: 1,
                }),
                &flag,
            )
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !blocked.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            blocked.load(Ordering::Acquire),
            "service allocated the handle before cancellation"
        );
        cancel.cancel();
        assert_eq!(
            worker.join().unwrap().unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        hold.store(false, Ordering::Release);
        assert!(matches!(client.call(Command::Ping).unwrap(), Reply::Done));
    }
    let Reply::Text(text::Reply::Upload { transfer }) = client
        .call(Command::Text(text::Command::BeginBlob {
            characters: 1,
            bytes: 1,
        }))
        .unwrap()
    else {
        panic!("cancelled headers leaked their server resources");
    };
    client.close_text(transfer);
    client.call(Command::Shutdown).unwrap();
}

#[test]
fn queued_text_transfers_can_cancel_without_waiting_for_other_files() {
    let dir = tempfile::tempdir().unwrap();
    let client = connection(dir.path(), Arc::default(), Arc::default());
    let first = client.text_permit().unwrap();
    let second = client.text_permit().unwrap();
    let cancel = crate::background::CancellationToken::new();
    let flag = cancel.clone();
    let worker = client.clone();
    let (sender, reply) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let result = worker
            .text_permit_cancellable(Some(&flag))
            .map(|_| ())
            .map_err(|error| error.kind());
        sender.send(result).unwrap();
    });
    cancel.cancel();
    assert_eq!(
        reply.recv_timeout(Duration::from_secs(2)).unwrap(),
        Err(io::ErrorKind::Interrupted)
    );
    worker.join().unwrap();
    drop((first, second));
    client.call(Command::Shutdown).unwrap();
}

#[test]
fn stale_file_transfer_progress_does_not_redraw_the_workbench() {
    let dir = tempfile::tempdir().unwrap();
    let workspace =
        workspace::Workspace::new(connection(dir.path(), Arc::default(), Arc::default()));
    let path = workspace.target_key("file.txt").unwrap();
    let mut app = app_for_workspace(&workspace);
    let before = app.status_message.clone();
    app.background
        .sender
        .send(
            crate::background::BackgroundResult::RemoteTransferProgress {
                generation: app.workspace_generation,
                path: path.clone(),
                saving: false,
                completed: 1,
                total: 2,
            },
        )
        .unwrap();
    assert!(!app.poll_background_results());
    assert_eq!(app.status_message, before);
    let mut editor = crate::editor::Editor::new();
    editor.file_path = Some(path.clone());
    editor.bg_loading = true;
    app.tabs.push(crate::app::Tab::new_file(editor));
    app.background
        .sender
        .send(
            crate::background::BackgroundResult::RemoteTransferProgress {
                generation: app.workspace_generation,
                path,
                saving: false,
                completed: 1,
                total: 2,
            },
        )
        .unwrap();
    assert!(app.poll_background_results());
    assert!(app.status_message.contains("50%"));
}

#[test]
fn remote_checkpoint_views_stream_once_and_reuse_unchanged_preview_text() {
    use std::collections::BTreeSet;
    let root = tempfile::tempdir().unwrap();
    git2::Repository::init(root.path()).unwrap();
    let mut seed = 0x68a93021cdb27u64;
    let middle: String = (0..25_000)
        .map(|_| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            format!("{seed:016x} 🦀 café\t\\quoted\"\n")
        })
        .collect();
    let before = format!("first\n{middle}last\n");
    let after = format!("FIRST\n{middle}LAST\n");
    assert!(after.len() > text::INLINE_BYTES);
    let file = root.path().join("file.txt");
    std::fs::write(&file, &before).unwrap();
    let traffic = Arc::new(AtomicUsize::new(0));
    let workspace =
        workspace::Workspace::new(connection(root.path(), Arc::default(), traffic.clone()));
    let mut journal = rollback::Journal::open(workspace.clone()).unwrap().unwrap();
    let checkpoint = journal
        .begin("one", "chat", "edit", BTreeSet::new())
        .unwrap();
    std::fs::write(&file, &after).unwrap();
    let paths = BTreeSet::from([PathBuf::from("file.txt")]);
    journal
        .capture_checkpoint(checkpoint, BTreeSet::new(), &paths)
        .unwrap();
    assert_eq!(journal.inline_reviews()[0].text.as_ref(), after);
    let transferred = traffic.load(Ordering::Relaxed);
    for _ in 0..100 {
        assert_eq!(journal.summary("one", "chat").edits.len(), 1);
        assert_eq!(journal.inline_reviews()[0].text.as_ref(), after);
    }
    assert_eq!(
        traffic.load(Ordering::Relaxed),
        transferred,
        "rendering/querying cached review metadata must never contact the node"
    );
    journal
        .capture_checkpoint(checkpoint, BTreeSet::new(), &paths)
        .unwrap();
    journal
        .finish_checkpoint(checkpoint, BTreeSet::new())
        .unwrap();
    assert!(
        traffic.load(Ordering::Relaxed) - transferred < 16_384,
        "unchanged captures and finishing metadata must not transfer the large preview again"
    );
    assert_eq!(journal.inline_reviews()[0].text.as_ref(), after);
    let summary = journal.summary("one", "chat");
    assert_eq!(summary.active, None);
    let plan = journal
        .plan(
            Some(("one", "chat")),
            &vtcode::acp::rollback::Selection::All,
        )
        .unwrap();
    journal.apply(plan).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
    let undo = journal.undo_plan("one", "chat").unwrap();
    journal.apply(undo).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), after);
    drop(journal);
    // Reopening on the same actor is a barrier after the coalesced close: its
    // native lock and cached snapshots must have been released.
    let reopened = rollback::Journal::open(workspace.clone()).unwrap().unwrap();
    assert_eq!(reopened.summary("one", "chat").edits.len(), 1);
    drop(reopened);
    workspace.client.call(Command::Shutdown).unwrap();
}

#[test]
fn cancelled_checkpoint_open_releases_native_journal_locks() {
    let root = tempfile::tempdir().unwrap();
    git2::Repository::init(root.path()).unwrap();
    let hold = Arc::new(AtomicBool::new(false));
    let blocked = Arc::new(AtomicBool::new(false));
    let client = connection_with_status(root.path(), hold.clone(), Arc::default(), blocked.clone());
    struct Release(Arc<AtomicBool>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    let _release = Release(hold.clone());
    for _ in 0..12 {
        hold.store(true, Ordering::Release);
        let worker = client.clone();
        let cancel = crate::background::CancellationToken::new();
        let flag = cancel.clone();
        let worker = std::thread::spawn(move || {
            worker.call_cancellable(Command::Rollback(rollback::Command::Open), &flag)
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !blocked.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(blocked.load(Ordering::Acquire));
        cancel.cancel();
        assert_eq!(
            worker.join().unwrap().unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        hold.store(false, Ordering::Release);
        client.call(Command::Ping).unwrap();
    }
    let workspace = workspace::Workspace::new(client);
    let journal = rollback::Journal::open(workspace.clone()).unwrap().unwrap();
    drop(journal);
    workspace.client.call(Command::Shutdown).unwrap();
}

#[test]
fn binary_save_coalesces_many_tiny_adjacent_edits_before_transfer() {
    use crate::extension_documents::BinaryDocument;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("many.bin"), []).unwrap();
    let workspace =
        workspace::Workspace::new(connection(dir.path(), Arc::default(), Arc::default()));
    let document =
        BinaryDocument::open_remote(&workspace.target_key("many.bin").unwrap(), &workspace)
            .unwrap();
    let fragments = (0..70_000)
        .map(|i| BinaryDocument::bytes_fragment(vec![(i % 251) as u8]))
        .collect();
    let mut document = document.with_fragments(fragments).unwrap();
    document.save().unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("many.bin")).unwrap(),
        (0..70_000).map(|i| (i % 251) as u8).collect::<Vec<_>>()
    );
}

#[test]
fn recovered_remote_edits_detect_external_changes_across_repeated_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let native = dir.path().join("recover.txt");
    std::fs::write(&native, "original\n").unwrap();
    let workspace =
        workspace::Workspace::new(connection(dir.path(), Arc::default(), Arc::default()));
    let key = workspace.view_key(&RemotePath::new(b"recover.txt".to_vec()).unwrap());
    let mut first = app_for_workspace(&workspace);
    first.open_file(&key);
    first.wait_for_background(Duration::from_secs(10), |app| {
        app.editor().is_some_and(|e| !e.bg_loading)
    });
    first.editor_mut().unwrap().insert_char('A');
    let original_session = first.build_session();
    let encoded = serde_json::to_vec(&original_session).unwrap();
    let decoded: crate::session::Session = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(
        decoded.remote_checkpoints,
        original_session.remote_checkpoints
    );

    // Recovery against an unchanged disk remains safe, including another edit.
    let mut unchanged = app_for_workspace(&workspace);
    unchanged.restore_session(&original_session);
    unchanged.wait_for_background(Duration::from_secs(10), |app| {
        app.editor().is_some_and(|e| !e.bg_loading)
    });
    assert!(
        !unchanged
            .editor()
            .unwrap()
            .remote_checkpoint
            .as_ref()
            .unwrap()
            .conflicted
    );
    assert_eq!(unchanged.editor().unwrap().rope.to_string(), "Aoriginal\n");

    std::fs::write(&native, "someone else's changes\n").unwrap();
    let mut recovered = app_for_workspace(&workspace);
    recovered.restore_session(&original_session);
    recovered.wait_for_background(Duration::from_secs(10), |app| {
        app.editor().is_some_and(|e| !e.bg_loading)
    });
    assert!(
        recovered
            .editor()
            .unwrap()
            .remote_checkpoint
            .as_ref()
            .unwrap()
            .conflicted
    );
    recovered.save_current_file();
    recovered.wait_for_background(Duration::from_secs(10), |app| {
        app.pending_file_saves.is_empty()
    });
    assert_eq!(
        std::fs::read_to_string(&native).unwrap(),
        "someone else's changes\n"
    );
    assert_eq!(recovered.editor().unwrap().rope.to_string(), "Aoriginal\n");

    // Keeping the recovered edits and restarting again must not adopt the new
    // disk as authorization to overwrite it on the second attempt.
    let mut restarted = app_for_workspace(&workspace);
    restarted.restore_session(&recovered.build_session());
    restarted.wait_for_background(Duration::from_secs(10), |app| {
        app.editor().is_some_and(|e| !e.bg_loading)
    });
    assert!(
        restarted
            .editor()
            .unwrap()
            .remote_checkpoint
            .as_ref()
            .unwrap()
            .conflicted
    );
    let editor = restarted.editor().unwrap();
    let mut request = editor.background_save_request(false, false, false).unwrap();
    request.path = workspace.target_key("recovered-copy.txt").unwrap();
    request.expected_disk = None;
    request.fail_if_exists = true;
    assert!(restarted.queue_background_save_job(request));
    restarted.wait_for_background(Duration::from_secs(10), |app| {
        app.pending_file_saves.is_empty()
    });
    assert_eq!(
        std::fs::read_to_string(dir.path().join("recovered-copy.txt")).unwrap(),
        "Aoriginal\n"
    );
    assert_eq!(
        std::fs::read_to_string(&native).unwrap(),
        "someone else's changes\n"
    );
}

#[test]
fn recovered_interrupted_save_that_matches_disk_is_clean() {
    let dir = tempfile::tempdir().unwrap();
    let native = dir.path().join("recover.txt");
    std::fs::write(&native, "original\n").unwrap();
    let workspace =
        workspace::Workspace::new(connection(dir.path(), Arc::default(), Arc::default()));
    let key = workspace.target_key("recover.txt").unwrap();
    let mut first = app_for_workspace(&workspace);
    first.open_file(&key);
    first.wait_for_background(Duration::from_secs(10), |app| {
        app.editor().is_some_and(|e| !e.bg_loading)
    });
    first.editor_mut().unwrap().insert_char('A');
    let session = first.build_session();
    std::fs::write(&native, "Aoriginal\n").unwrap();
    let mut restored = app_for_workspace(&workspace);
    restored.restore_session(&session);
    restored.wait_for_background(Duration::from_secs(10), |app| {
        app.editor().is_some_and(|e| !e.bg_loading)
    });
    assert!(!restored.editor().unwrap().modified);
    assert!(
        !restored
            .editor()
            .unwrap()
            .remote_checkpoint
            .as_ref()
            .unwrap()
            .conflicted
    );
    restored.editor_mut().unwrap().undo();
    assert!(restored.editor().unwrap().modified);
    restored.editor_mut().unwrap().redo();
    assert!(!restored.editor().unwrap().modified);
}

#[test]
fn remote_explorer_create_rename_copy_and_delete_are_async_transactions() {
    let dir = tempfile::tempdir().unwrap();
    let workspace =
        workspace::Workspace::new(connection(dir.path(), Arc::default(), Arc::default()));
    let mut app = app_for_workspace(&workspace);
    app.begin_remote_creation(workspace.view_root.clone(), false);
    assert!(
        !dir.path().join("new file").exists(),
        "Inline naming needs no remote placeholder file"
    );
    app.rename.query = "created.rs".into();
    app.execute_rename();
    app.wait_for_background(Duration::from_secs(10), |app| {
        !app.bg_filesystem_action_running && app.editor().is_some_and(|e| !e.bg_loading)
    });
    let original = workspace.target_key("created.rs").unwrap();
    assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&original));
    app.editor_mut().unwrap().insert_char('X');
    app.begin_rename(original.clone(), "renamed.rs".into(), true, false);
    app.execute_rename();
    app.wait_for_background(Duration::from_secs(10), |app| {
        !app.bg_filesystem_action_running
    });
    let renamed = workspace.target_key("renamed.rs").unwrap();
    assert!(!app.rename.active);
    assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&renamed));
    assert!(app.editor().unwrap().modified);
    app.save_current_file();
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.pending_file_saves.is_empty()
    });
    assert_eq!(
        std::fs::read_to_string(dir.path().join("renamed.rs")).unwrap(),
        "X"
    );
    assert!(!dir.path().join("created.rs").exists());

    let task = fs::Task::Paste {
        sources: vec![RemotePath::new(b"renamed.rs".to_vec()).unwrap()],
        directory: RemotePath::default(),
        cut: false,
    };
    app.run_remote_filesystem_action("Paste", task, None);
    app.wait_for_background(Duration::from_secs(10), |app| {
        !app.bg_filesystem_action_running
    });
    assert_eq!(
        std::fs::read_to_string(dir.path().join("renamed (1).rs")).unwrap(),
        "X"
    );
    app.start_file_deletion(vec![renamed]);
    app.wait_for_background(Duration::from_secs(10), |app| {
        !app.bg_filesystem_action_running
    });
    assert!(!dir.path().join("renamed.rs").exists());
    assert!(dir.path().join("renamed (1).rs").exists());
}

#[test]
fn remote_explorer_clipboard_shortcuts_and_terminal_paste_preserve_the_local_buffer() {
    use crate::app::{ActivePane, SidebarView, Tab};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    for cut in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("source.rs"), "saved remote content\n").unwrap();
        std::fs::create_dir(directory.path().join("destination")).unwrap();
        let workspace =
            workspace::Workspace::new(connection(directory.path(), Arc::default(), Arc::default()));
        let mut app = app_for_workspace(&workspace);
        app.show_explorer = true;
        app.sidebar_view = SidebarView::Explorer;
        app.active_pane = ActivePane::Explorer;
        let source = workspace.target_key("source.rs").unwrap();
        let mut editor = crate::editor::Editor::test_with_text("unsaved remote edits\n");
        editor.file_path = Some(source.clone());
        editor.selection_anchor = Some((0, 3));
        app.tabs = vec![Tab::new_file(editor)];
        app.refresh_explorer_async();
        app.wait_for_background(Duration::from_secs(10), |app| {
            !app.bg_explorer_refresh_running
        });
        let select = |app: &mut crate::app::App, path: &std::path::Path| {
            let index = app.explorer.find_path(path).unwrap();
            app.set_explorer_single_selection(index);
        };
        let key = |app: &mut crate::app::App, letter| {
            crate::input_keyboard::handle_keyboard_event(
                app,
                KeyEvent::new(KeyCode::Char(letter), KeyModifiers::CONTROL),
            );
        };
        select(&mut app, &source);
        key(&mut app, if cut { 'x' } else { 'c' });
        assert_eq!(
            app.explorer_clipboard.as_ref().unwrap().paths,
            [source.clone()]
        );
        if cut {
            // A cut pasted into its own folder must not rename the source.
            key(&mut app, 'v');
            app.wait_for_background(Duration::from_secs(10), |app| {
                !app.bg_filesystem_action_running && !app.bg_explorer_refresh_running
            });
            assert!(directory.path().join("source.rs").exists());
            assert!(!directory.path().join("source (1).rs").exists());
            select(&mut app, &source);
            key(&mut app, 'x');
        }
        let payload = crate::clipboard::get_text().unwrap();
        assert!(
            !payload.contains("vtcode-remote"),
            "use the remote path, not a local view key"
        );
        select(&mut app, &workspace.target_key("destination").unwrap());
        if cut {
            key(&mut app, 'v');
        } else {
            crate::app_event_loop::handle_paste_input(&mut app, &payload);
        }
        app.wait_for_background(Duration::from_secs(10), |app| {
            !app.bg_filesystem_action_running
        });
        assert_eq!(
            std::fs::read_to_string(directory.path().join("destination/source.rs")).unwrap(),
            "saved remote content\n"
        );
        assert_eq!(directory.path().join("source.rs").exists(), !cut);
        assert_eq!(
            app.editor().unwrap().rope.to_string(),
            "unsaved remote edits\n"
        );
        assert_eq!(app.editor().unwrap().selection_anchor, Some((0, 3)));
        assert_eq!(app.active_pane, ActivePane::Explorer);
        workspace.client.call(Command::Shutdown).unwrap();
    }
}

#[test]
fn remote_explorer_cut_same_directory_does_not_report_a_missing_source_as_moved() {
    let directory = tempfile::tempdir().unwrap();
    let result = fs::execute(
        directory.path(),
        fs::Task::Paste {
            sources: vec![RemotePath::new(b"missing.rs".to_vec()).unwrap()],
            directory: RemotePath::default(),
            cut: true,
        },
        &crate::background::CancellationToken::new(),
        |_, _| {},
    );
    assert_eq!(result.err().unwrap().kind(), io::ErrorKind::NotFound);
}

#[test]
fn remote_directory_delete_cancellation_preserves_remaining_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("many")).unwrap();
    for i in 0..100 {
        std::fs::write(dir.path().join(format!("many/{i}.txt")), "keep").unwrap();
    }
    let client = connection(dir.path(), Arc::default(), Arc::default());
    let cancel = crate::background::CancellationToken::new();
    let reply = client.call_progress(
        Command::Filesystem(fs::Task::Delete {
            paths: vec![RemotePath::new(b"many".to_vec()).unwrap()],
        }),
        &cancel,
        |progress| {
            if progress.phase == crate::file_deletion::Phase::Counting {
                cancel.cancel();
            }
        },
    );
    // A small real filesystem may finish before a cancellation crosses the
    // connection. Deterministically test cancellation at the service boundary.
    assert!(reply.is_ok());
    std::fs::create_dir_all(dir.path().join("remaining")).unwrap();
    std::fs::write(dir.path().join("remaining/keep.txt"), "safe").unwrap();
    cancel.cancel();
    fs::execute(
        dir.path(),
        fs::Task::Delete {
            paths: vec![RemotePath::new(b"remaining".to_vec()).unwrap()],
        },
        &cancel,
        |_, _| {},
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("remaining/keep.txt")).unwrap(),
        "safe"
    );
}

#[test]
fn remote_transfer_frames_allow_control_and_small_replies_during_large_opens() {
    let mut value = 0xdead_beefu32;
    let noise: String = (0..128_000)
        .map(|_| {
            value ^= value << 13;
            value ^= value >> 17;
            value ^= value << 5;
            (b'!' + (value % 90) as u8) as char
        })
        .collect();
    let mut queue = wire::OutputQueue::default();
    for id in 1..=7 {
        queue
            .push(Response {
                id,
                result: Ok(Reply::Opened {
                    identity: RemotePath::new(b"test.txt".to_vec()).unwrap(),
                    text: noise.clone(),
                    encoding: "UTF-8".into(),
                    revision: revision(&Rope::from_str(&noise), "UTF-8"),
                    size: noise.len() as u64,
                    modified: None,
                }),
            })
            .unwrap();
    }
    queue
        .push(Response {
            id: 8,
            result: Ok(Reply::Done),
        })
        .unwrap();
    let mut decoder = wire::Decoder::new('S', 5);
    let mut completed = Vec::new();
    let mut steps = 0;
    while !queue.is_empty() {
        if steps == 7 {
            queue
                .push(Response {
                    id: 0,
                    result: Ok(Reply::ProcessEnd {
                        process: 100,
                        error: None,
                    }),
                })
                .unwrap();
        }
        let mut bytes = Vec::new();
        queue.step(&mut bytes, 5).unwrap();
        steps += 1;
        for line in bytes.split_inclusive(|b| *b == b'\n') {
            if let Some((id, data)) = decoder.line(line).unwrap() {
                let reply: Response = serde_json::from_slice(&data).unwrap();
                if reply.id == 0 {
                    assert_ne!(id & wire::EVENT_ID, 0);
                } else {
                    assert_eq!(reply.id, id);
                }
                completed.push(reply.id);
                if let Ok(Reply::Opened { text, .. }) = reply.result {
                    assert_eq!(text, noise);
                }
            }
        }
    }
    assert_eq!(
        &completed[..2],
        &[0, 8],
        "Interactive replies must not wait for a full file transfer"
    );
    assert_eq!(completed.len(), 9);
}

#[test]
fn cancellation_can_overtake_a_partial_upload_without_poisoning_future_requests() {
    use std::io::BufReader;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_owned();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let (server, _) = listener.accept().unwrap();
    std::thread::spawn(move || service::serve(&root, server.try_clone().unwrap(), server).unwrap());
    let mut reader = BufReader::new(socket.try_clone().unwrap());
    let hello = client::Client::read_hello(&mut reader).unwrap();
    let mut seed = 123u32;
    let text: String = (0..40_000)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (b'!' + (seed % 90) as u8) as char
        })
        .collect();
    let request = Request {
        id: 1,
        workspace: None,
        command: Command::Format {
            path: RemotePath::new(b"code.rs".to_vec()).unwrap(),
            config: crate::formatters::Config {
                provider: crate::formatters::Provider::Command,
                command: "must-never-be-launched".into(),
                ..Default::default()
            },
            text,
            lines: None,
        },
    };
    let mut bytes = Vec::new();
    wire::write_message(
        &mut bytes,
        'C',
        hello.session,
        1,
        &wire::encode(&request).unwrap(),
    )
    .unwrap();
    let frames: Vec<_> = bytes.split_inclusive(|byte| *byte == b'\n').collect();
    assert!(frames.len() > 2);
    socket.write_all(frames[0]).unwrap();
    wire::write_message(
        &mut socket,
        'C',
        hello.session,
        0,
        &wire::encode(&Request {
            id: 0,
            workspace: None,
            command: Command::Cancel { request: 1 },
        })
        .unwrap(),
    )
    .unwrap();
    for frame in &frames[1..] {
        socket.write_all(frame).unwrap();
    }
    let mut decoder = wire::Decoder::new('S', hello.session);
    let mut line = Vec::new();
    let reply = loop {
        assert!(wire::read_line(&mut reader, &mut line).unwrap());
        if let Some((_, bytes)) = decoder.line(&line).unwrap() {
            break serde_json::from_slice::<Response>(&bytes).unwrap();
        }
    };
    assert_eq!(reply.id, 1);
    assert_eq!(
        reply.result.unwrap_err().into_io().kind(),
        io::ErrorKind::Interrupted
    );
    // Late cancellation of completed requests must not accumulate tombstones.
    for _ in 0..200 {
        wire::write_message(
            &mut socket,
            'C',
            hello.session,
            0,
            &wire::encode(&Request {
                id: 0,
                workspace: None,
                command: Command::Cancel { request: 1 },
            })
            .unwrap(),
        )
        .unwrap();
    }
    wire::write_message(
        &mut socket,
        'C',
        hello.session,
        2,
        &wire::encode(&Request {
            id: 2,
            workspace: None,
            command: Command::Shutdown,
        })
        .unwrap(),
    )
    .unwrap();
    loop {
        assert!(wire::read_line(&mut reader, &mut line).unwrap());
        if let Some((_, bytes)) = decoder.line(&line).unwrap() {
            assert!(matches!(
                serde_json::from_slice::<Response>(&bytes).unwrap().result,
                Ok(Reply::Done)
            ));
            break;
        }
    }
}

#[test]
fn terminal_input_and_echo_interleave_before_a_large_upload_finishes() {
    use std::io::{BufReader, Read};
    use std::sync::mpsc;
    struct GateWriter {
        socket: TcpStream,
        line: Vec<u8>,
        frame: usize,
        paused: mpsc::SyncSender<usize>,
        release: mpsc::Receiver<()>,
    }
    impl Write for GateWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.socket.write_all(bytes)?;
            for &byte in bytes {
                self.line.push(byte);
                if byte == b'\n' {
                    // Spawn reserves process ID 1 and request ID 2. The new
                    // file upload is request 3; process input is an ID-0 frame.
                    if String::from_utf8_lossy(&self.line).contains(":0000000000000003:") {
                        self.frame += 1;
                        if self.frame <= 2 {
                            self.paused.send(self.frame).map_err(io::Error::other)?;
                            self.release
                                .recv_timeout(Duration::from_secs(10))
                                .map_err(io::Error::other)?;
                        }
                    }
                    self.line.clear();
                }
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            self.socket.flush()
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_owned();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    socket.set_nodelay(true).unwrap();
    let (server, _) = listener.accept().unwrap();
    std::thread::spawn(move || {
        let _ = service::serve(&root, server.try_clone().unwrap(), server);
    });
    let mut reader = BufReader::new(socket.try_clone().unwrap());
    let hello = client::Client::read_hello(&mut reader).unwrap();
    let (paused, stops) = mpsc::sync_channel(2);
    let (release, resume) = mpsc::sync_channel(2);
    let client = client::Client::attach(
        reader,
        GateWriter {
            socket,
            line: Vec::new(),
            frame: 0,
            paused,
            release: resume,
        },
        hello,
    )
    .unwrap();
    let (program, args) = fixture_args("process_echo_fixture");
    let (reader, mut writer, _killer) = client
        .spawn(
            program,
            args,
            RemotePath::default(),
            None,
            Default::default(),
        )
        .unwrap()
        .split();
    let mut reader = BufReader::new(reader);
    prelude(&mut reader, "VT-ECHO-READY");
    let mut seed = 789u32;
    let text: String = (0..40_000)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (b'!' + (seed % 90) as u8) as char
        })
        .collect();
    let uploading = client.clone();
    let uploaded = std::thread::spawn(move || {
        uploading.call(Command::Save {
            path: RemotePath::new(b"new.rs".to_vec()).unwrap(),
            base: None,
            change: Change {
                start: 0,
                end: 0,
                text,
            },
            encoding: "UTF-8".into(),
        })
    });
    assert_eq!(stops.recv_timeout(Duration::from_secs(10)).unwrap(), 1);
    writer.write_all(b"interactive").unwrap();
    release.send(()).unwrap();
    assert_eq!(stops.recv_timeout(Duration::from_secs(10)).unwrap(), 2);
    let (completed, result) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = [0; 11];
        let output = reader.read_exact(&mut bytes).map(|_| bytes);
        let _ = completed.send(output);
    });
    let echoed = result.recv_timeout(Duration::from_secs(5));
    // Always release the writer before asserting, including on a regression.
    release.send(()).unwrap();
    assert_eq!(echoed.unwrap().unwrap(), *b"interactive");
    assert!(matches!(
        uploaded.join().unwrap().unwrap(),
        Reply::Saved { .. }
    ));
    client.call(Command::Shutdown).unwrap();
}

#[cfg(unix)]
#[test]
fn lsp_root_uri_preserves_non_utf8_remote_directory_names() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir
        .path()
        .join(std::ffi::OsStr::from_bytes(b"project-\xff"));
    std::fs::create_dir(&root).unwrap();
    let workspace = workspace::Workspace::new(connection(&root, Arc::default(), Arc::default()));
    let key = workspace.target_key("file.rs").unwrap();
    let local_uri = url::Url::from_file_path(&key).unwrap().to_string();
    let mut request = serde_json::json!({"uri": local_uri});
    workspace.rewrite_lsp(&mut request, true);
    assert!(
        request["uri"]
            .as_str()
            .unwrap()
            .contains("project-%FF/file.rs")
    );
    workspace.rewrite_lsp(&mut request, false);
    assert_eq!(request["uri"], local_uri);
}

#[cfg(unix)]
#[test]
#[ignore = "Run with scripts/bridge_lab_suite.py for isolated CPU/disk/network faults"]
fn headless_latency_benchmark() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::env::var_os("VTCODE_HEADLESS_LAB_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_owned());
    let mut text = String::with_capacity(640_000);
    let mut seed = 0x8c3f_6391u32;
    for index in 0..8000 {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        text.push_str(&format!(
            "fn example_{index}() {{\n\tlet café_{index} = \"🦀{seed:08x}\";\n}}\n"
        ));
    }
    std::fs::write(root.join("benchmark.rs"), &text).unwrap();
    let script = std::env::current_dir()
        .unwrap()
        .join("scripts/bridge_lab.py");
    let mut args = vec!["python3".into(), script.into_os_string()];
    let options: Vec<String> = serde_json::from_str(
        &std::env::var("VTCODE_HEADLESS_LAB_OPTIONS").unwrap_or_else(|_| "[]".into()),
    )
    .unwrap();
    args.extend(options.into_iter().map(Into::into));
    args.extend(["--disk-root".into(), root.clone().into_os_string()]);
    if let Some(metrics) = std::env::var_os("VTCODE_HEADLESS_LAB_METRICS") {
        args.extend(["--metrics".into(), metrics]);
    }
    args.extend([
        "--".into(),
        std::env::current_exe().unwrap().into_os_string(),
        "--exact".into(),
        "remote::tests::headless_service_fixture".into(),
        "--ignored".into(),
        "--nocapture".into(),
    ]);
    let launch = raw_launch(args);
    let (client, owner) = launch
        .start_in(
            ratatui::layout::Size::new(120, 40),
            Default::default(),
            &root,
        )
        .unwrap()
        .endpoint_for_test()
        .unwrap();
    let workspace = workspace::Workspace::new(client);
    let key = workspace.target_key("benchmark.rs").unwrap();
    let mut app = app_for_workspace(&workspace);
    let opening = Instant::now();
    app.open_file(&key);
    app.wait_for_background(Duration::from_secs(60), |app| {
        app.editor().is_some_and(|e| !e.bg_loading)
    });
    let open_ms = opening.elapsed().as_secs_f64() * 1000.;
    assert_eq!(app.editor().unwrap().rope.to_string(), text);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    app.editor_mut().unwrap().insert_char('A');
    let saving = Instant::now();
    app.save_current_file();
    let mut acknowledged_ms = None;
    let mut samples = Vec::new();
    for _ in 0..256 {
        let start = Instant::now();
        let editor = app.editor_mut().unwrap();
        editor.move_word_right();
        editor.move_word_left();
        editor.move_down();
        editor.move_up();
        editor.insert_char('x');
        editor.undo();
        editor.scroll_down(3);
        editor.scroll_up(3);
        editor.selection_anchor = Some((0, 0));
        editor.move_right();
        editor.selection_anchor = None;
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        app.poll_background_results();
        if acknowledged_ms.is_none() && app.pending_file_saves.is_empty() {
            acknowledged_ms = Some(saving.elapsed().as_secs_f64() * 1000.);
        }
        samples.push(start.elapsed().as_secs_f64() * 1000.);
    }
    app.wait_for_background(Duration::from_secs(60), |app| {
        app.pending_file_saves.is_empty()
    });
    let save_ms = acknowledged_ms.unwrap_or_else(|| saving.elapsed().as_secs_f64() * 1000.);
    assert_eq!(
        std::fs::read_to_string(root.join("benchmark.rs")).unwrap(),
        format!("A{text}")
    );
    samples.sort_by(f64::total_cmp);
    println!(
        "VTCODE_HEADLESS_LAB {}",
        serde_json::json!({"bytes":text.len(), "samples":samples.len(), "open_ms":open_ms, "save_ms":save_ms,
        "input_draw_p50_ms":samples[samples.len()/2], "input_draw_p95_ms":samples[samples.len()*95/100], "input_draw_max_ms":samples[samples.len()-1]})
    );
    workspace.client.call(Command::Shutdown).unwrap();
    if let Some(metrics) = std::env::var_os("VTCODE_HEADLESS_LAB_METRICS") {
        let start = Instant::now();
        while !Path::new(&metrics).exists() && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            Path::new(&metrics).exists(),
            "Fault harness must publish metrics after graceful shutdown"
        );
    }
    drop(app);
    drop(workspace);
    drop(owner);
}

#[cfg(windows)]
#[test]
#[ignore = "Requires isolated SSH fixture arguments in VTCODE_HEADLESS_SSH_ARGS"]
fn windows_ssh_scheduler_hop_runs_a_local_editor() {
    let path =
        std::env::var_os("VTCODE_HEADLESS_SSH_ARGS").expect("Path to isolated SSH argument JSON");
    let ssh: Vec<String> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let launch = raw_launch(ssh.into_iter().map(Into::into));
    let (client, owner) = launch
        .start(ratatui::layout::Size::new(81, 25), Default::default())
        .unwrap()
        .endpoint_for_test()
        .unwrap();
    let workspace = workspace::Workspace::new(client);
    let mut app = app_for_workspace(&workspace);
    let key = workspace.target_key("remote.rs").unwrap();
    app.open_file(&key);
    app.wait_for_background(Duration::from_secs(45), |app| {
        app.editor().is_some_and(|e| !e.bg_loading)
    });
    assert_eq!(app.editor().unwrap().rope.to_string(), "fn example() { }\n");
    app.editor_mut().unwrap().insert_char('X');
    app.save_current_file();
    app.editor_mut().unwrap().insert_char('Y');
    app.wait_for_background(Duration::from_secs(45), |app| {
        app.pending_file_saves.is_empty()
    });
    assert_eq!(
        app.editor().unwrap().rope.to_string(),
        "XYfn example() { }\n"
    );
    assert!(app.editor().unwrap().modified);
    let Reply::Opened { text, .. } = workspace
        .client
        .call(Command::Open {
            path: RemotePath::new(b"remote.rs".to_vec()).unwrap(),
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(text, "Xfn example() { }\n");
    app.editor_mut().unwrap().undo();
    assert!(!app.editor().unwrap().modified);
    check_background_tool_has_no_controlling_terminal(&workspace.client);
    check_remote_checkpoints_use_node_filesystem(&workspace);
    agent::test_ssh_roundtrip(&workspace);
    drop(app);
    drop(workspace);
    drop(owner);
}

#[cfg(windows)]
fn check_remote_checkpoints_use_node_filesystem(workspace: &workspace::Workspace) {
    use std::collections::BTreeSet;
    let mut journal = rollback::Journal::open(workspace.clone()).unwrap().unwrap();
    let checkpoint = journal
        .begin("fixture", "chat", "create a file remotely", BTreeSet::new())
        .unwrap();
    let path = RemotePath::new(b"checkpoint.rs".to_vec()).unwrap();
    let contents = "// remotely created 🦀\n";
    workspace
        .client
        .call_file(Command::Save {
            path: path.clone(),
            base: None,
            encoding: "UTF-8".into(),
            change: Change {
                start: 0,
                end: 0,
                text: contents.into(),
            },
        })
        .unwrap();
    journal
        .finish_checkpoint(checkpoint, BTreeSet::new())
        .unwrap();
    assert_eq!(journal.summary("fixture", "chat").edits.len(), 1);
    let plan = journal
        .plan(
            Some(("fixture", "chat")),
            &vtcode::acp::rollback::Selection::All,
        )
        .unwrap();
    let applied = journal.apply(plan).unwrap();
    assert_eq!(applied, vec![(workspace.view_key(&path), false)]);
    assert!(workspace.load(&workspace.view_key(&path)).is_err());
    let undo = journal.undo_plan("fixture", "chat").unwrap();
    journal.apply(undo).unwrap();
    assert_eq!(
        workspace
            .load(&workspace.view_key(&path))
            .unwrap()
            .rope
            .to_string(),
        contents
    );
}

#[cfg(windows)]
#[test]
#[ignore = "Requires isolated encrypted fixture key; VTCODE_HEADLESS_AUTH_ARGS points to SSH argument JSON"]
fn windows_ssh_authentication_uses_the_private_console_before_attachment() {
    let args = std::env::var_os("VTCODE_HEADLESS_AUTH_ARGS")
        .expect("isolated encrypted SSH key argument file");
    let ssh: Vec<String> = serde_json::from_slice(&std::fs::read(args).unwrap()).unwrap();
    let setup = raw_launch(ssh.into_iter().map(Into::into))
        .start(ratatui::layout::Size::new(90, 24), Default::default())
        .unwrap();
    setup.wait_prompt_for_test("passphrase").unwrap();
    setup.type_for_test("vtcode-headless-fixture\r").unwrap();
    let (client, _owner) = setup.endpoint_for_test().unwrap();
    assert!(matches!(client.call(Command::Ping).unwrap(), Reply::Done));
}
