use super::*;
use crate::{
    app::Tab,
    background::BackgroundResult,
    editor::Editor,
    remote::{self, RemotePath},
};
use std::{
    sync::{Arc, mpsc},
    time::Duration,
};

fn remote_app(root: &Path) -> (App, remote::workspace::Workspace) {
    let workspace = remote::workspace::Workspace::new(remote::tests::connection(
        root,
        Arc::default(),
        Arc::default(),
    ));
    let mut app = App::new_deferred(workspace.view_root.clone());
    app.source_control_enabled = false;
    app.remote_workspace = Some(workspace.clone());
    (app, workspace)
}
fn search(app: &mut App) {
    app.update_global_search();
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_global_search_running);
    assert!(
        app.global_search_error.is_none(),
        "{:?}",
        app.global_search_error
    );
}

/// A peer whose search completion is controlled by the test, rather than by
/// disk speed or sleeps. This also exercises real framed transport delivery.
fn controlled_search_peer(root: &Path) -> (remote::client::Client, mpsc::Receiver<remote::Request>, mpsc::Sender<remote::Response>) {
    use std::io::{BufReader, Read, Write};
    struct Input { replies: mpsc::Receiver<remote::Response>, bytes: std::io::Cursor<Vec<u8>> }
    impl Read for Input {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if buffer.is_empty() { return Ok(0); }
            if self.bytes.position() as usize == self.bytes.get_ref().len() {
                let Ok(reply) = self.replies.recv() else { return Ok(0); };
                let mut bytes = Vec::new();
                let id = if reply.id == 0 { remote::wire::EVENT_ID | 1 } else { reply.id };
                remote::wire::write_message(&mut bytes, 'S', 42, id, &remote::wire::encode(&reply)?)?;
                self.bytes = std::io::Cursor::new(bytes);
            }
            self.bytes.read(buffer)
        }
    }
    struct Output { requests: mpsc::Sender<remote::Request>, decoder: remote::wire::Decoder, bytes: Vec<u8> }
    impl Write for Output {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.bytes.extend_from_slice(buffer);
            while let Some(end) = memchr::memchr(b'\n', &self.bytes) {
                let line = self.bytes.drain(..=end).collect::<Vec<_>>();
                if let Some((_, data)) = self.decoder.line(&line)? {
                    self.requests.send(serde_json::from_slice(&data)?).map_err(std::io::Error::other)?;
                }
            }
            Ok(buffer.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let (requests, received) = mpsc::channel();
    let (responses, replies) = mpsc::channel();
    let hello = remote::service::Service::new(root).unwrap().hello(42);
    let client = remote::client::Client::attach(BufReader::new(Input { replies, bytes: std::io::Cursor::new(Vec::new()) }),
        Output { requests, decoder: remote::wire::Decoder::new('C', 42), bytes: Vec::new() }, hello).unwrap();
    (client, received, responses)
}

#[test]
fn remote_search_streams_into_the_ui_before_completion_and_discards_cancelled_batches() {
    let root = tempfile::tempdir().unwrap();
    let (client, requests, responses) = controlled_search_peer(root.path());
    let workspace = remote::workspace::Workspace::new(client);
    let mut app = App::new_deferred(workspace.view_root.clone());
    app.source_control_enabled = false;
    app.remote_workspace = Some(workspace.clone());
    let hit = |text: &str| remote::search::Hit { path: RemotePath::new(b"file.txt".to_vec()).unwrap(),
        line: 1, start: 0, end: text.len(), text: text.into() };
    app.global_search_query = "needle".into();
    app.update_global_search();
    let first = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(first.command, remote::Command::Search(ref query) if query.stream));
    responses.send(remote::Response { id: 0, result: Ok(remote::Reply::SearchProgress {
        request: first.id, hits: vec![hit("needle")],
    }) }).unwrap();
    app.wait_for_background(Duration::from_secs(5), |app| app.global_search_results.len() == 1);
    assert!(app.bg_global_search_running, "the final reply is still held by the peer");
    assert_eq!(&*app.global_search_results[0].line_text, "needle");
    app.stop_global_search();
    let cancelled = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(cancelled.command, remote::Command::Cancel { request } if request == first.id));
    app.global_search_query = "new".into();
    app.update_global_search();
    let next = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(next.command, remote::Command::Search(_)));
    responses.send(remote::Response { id: 0, result: Ok(remote::Reply::SearchProgress {
        request: first.id, hits: vec![hit("obsolete")],
    }) }).unwrap();
    responses.send(remote::Response { id: first.id, result: Ok(remote::Reply::Search {
        hits: vec![hit("obsolete final")], truncated: true,
    }) }).unwrap();
    responses.send(remote::Response { id: 0, result: Ok(remote::Reply::SearchProgress {
        request: next.id, hits: vec![hit("new")],
    }) }).unwrap();
    app.wait_for_background(Duration::from_secs(5), |app| app.global_search_results.len() == 1);
    assert!(app.bg_global_search_running);
    assert_eq!(&*app.global_search_results[0].line_text, "new");
    responses.send(remote::Response { id: next.id, result: Ok(remote::Reply::Search {
        hits: vec![hit("new tail")], truncated: false,
    }) }).unwrap();
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_global_search_running);
    assert_eq!(app.global_search_results.len(), 2);
    assert_eq!(&*app.global_search_results[1].line_text, "new tail");
    assert!(!app.global_search_truncated);
    assert!(app.global_search_error.is_none());
    workspace.client.disconnect();
}

#[test]
fn remote_search_oversized_progress_fails_only_the_search_and_accepts_legacy_replies() {
    let root = tempfile::tempdir().unwrap();
    let (client, requests, responses) = controlled_search_peer(root.path());
    let workspace = remote::workspace::Workspace::new(client);
    let mut app = App::new_deferred(workspace.view_root.clone());
    app.source_control_enabled = false;
    app.remote_workspace = Some(workspace.clone());
    app.max_global_search_results = 1;
    app.global_search_query = "needle".into();
    app.update_global_search();
    let first = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    let hit = || remote::search::Hit { path: RemotePath::new(b"file.txt".to_vec()).unwrap(),
        line: 1, start: 0, end: 6, text: "needle".into() };
    responses.send(remote::Response { id: 0, result: Ok(remote::Reply::SearchProgress {
        request: first.id, hits: vec![hit(), hit()],
    }) }).unwrap();
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_global_search_running);
    assert!(app.global_search_error.as_ref().unwrap().contains("result budget"));
    assert!(workspace.client.connected(), "a search budget error must not disconnect the editor");
    let cancelled = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(cancelled.command, remote::Command::Cancel { request } if request == first.id));
    app.update_global_search();
    let next = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    responses.send(remote::Response { id: next.id, result: Ok(remote::Reply::Search {
        hits: vec![hit()], truncated: false,
    }) }).unwrap();
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_global_search_running);
    assert!(app.global_search_error.is_none());
    assert_eq!(app.global_search_results.len(), 1, "older nodes can still send a single complete reply");
    workspace.client.disconnect();
}

#[test]
fn remote_search_filters_keep_percent_names_and_absolute_includes_when_a_file_is_open() {
    let root = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let name = "100%real.txt";
    std::fs::write(root.path().join(name), "disk needle").unwrap();
    let (mut app, workspace) = remote_app(root.path());
    app.global_search_query = "needle".into();
    app.search_include_pattern = name.into();
    search(&mut app);
    assert_eq!(app.global_search_results.len(), 1, "closed disk control");
    let key = workspace.view_key(&RemotePath::new(name.as_bytes().to_vec()).unwrap());
    let mut editor = Editor::test_with_text("draft needle");
    editor.file_path = Some(key.clone());
    app.tabs.push(Tab::new_file(editor));
    search(&mut app);
    assert_eq!(
        app.global_search_results.len(),
        1,
        "opening the same file must not hide it"
    );
    assert_eq!(&*app.global_search_results[0].line_text, "draft needle");
    app.search_include_pattern = root.path().join(name).to_string_lossy().into_owned();
    search(&mut app);
    assert_eq!(
        app.global_search_results.len(),
        1,
        "absolute node path includes the live draft"
    );
    app.search_include_pattern.clear();
    app.search_exclude_pattern = name.into();
    search(&mut app);
    assert!(
        app.global_search_results.is_empty(),
        "exclude spelling is the remote filename"
    );
    workspace.client.call(remote::Command::Shutdown).unwrap();
}

#[test]
fn remote_search_obsolete_failure_cannot_finish_a_new_query_or_enable_partial_replacement() {
    let root = tempfile::tempdir().unwrap();
    let mut app = App::new_deferred(root.path().to_path_buf());
    app.source_control_enabled = false;
    app.global_search_query = "new query".into();
    app.search_generation = 42;
    app.bg_global_search_running = true;
    app.background
        .sender
        .send(BackgroundResult::GlobalSearchResults {
            results: Vec::new(),
            done: false,
            generation: 42,
            truncated: false,
        })
        .unwrap();
    search_failed(
        &app.background.sender,
        41,
        "old request failed after changing query".into(),
    );
    app.poll_background_results();
    assert!(
        app.bg_global_search_running,
        "old errors must not release the new replacement gate"
    );
    assert!(app.global_search_error.is_none());
    search_failed(&app.background.sender, 42, "current request failed".into());
    app.poll_background_results();
    assert!(!app.bg_global_search_running);
    assert!(
        app.global_search_error
            .as_ref()
            .unwrap()
            .contains("current request")
    );
}

#[test]
fn remote_search_query_setup_defers_regex_compilation_until_the_search_worker_runs() {
    let root = tempfile::tempdir().unwrap();
    let mut app = App::new_deferred(root.path().to_path_buf());
    app.source_control_enabled = false;
    let (release, gate) = mpsc::channel::<()>();
    let (ready, started) = mpsc::sync_channel(1);
    app.submit_global_search(move |_| {
        ready.send(()).unwrap();
        let _ = gate.recv();
    });
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    app.global_search_regex = true;
    app.global_search_query = "(".into();
    app.update_global_search();
    let deferred = app.bg_global_search_running && app.global_search_error.is_none();
    drop(release);
    assert!(deferred, "pattern parsing must not run on the input thread");
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_global_search_running);
    assert!(app.global_search_error.is_some());
}

#[test]
fn remote_search_scope_cache_avoids_repeated_requests_and_invalidates_for_changed_paths_and_filters()
 {
    let root = tempfile::tempdir().unwrap();
    let workspace = remote::workspace::Workspace::new(remote::tests::connection(
        root.path(),
        Arc::default(),
        Arc::default(),
    ));
    let cancel = crate::background::CancellationToken::new();
    let first = workspace.view_key(&RemotePath::new(b"100%real.txt".to_vec()).unwrap());
    let other = workspace.view_key(&RemotePath::new(b"other.rs".to_vec()).unwrap());
    let paths = [first.clone(), other.clone()];
    let baseline = workspace.client.next_request_id_for_test();
    assert_eq!(
        workspace.search_scope(&paths, "", "", &cancel).unwrap(),
        [0, 1]
    );
    assert!(
        workspace
            .search_scope(&[], "*.txt", "", &cancel)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        workspace.client.next_request_id_for_test(),
        baseline,
        "default/closed-only filtering is entirely local"
    );
    assert_eq!(
        workspace
            .search_scope(&paths, "*.txt", "", &cancel)
            .unwrap(),
        [0]
    );
    let filtered = workspace.client.next_request_id_for_test();
    assert!(filtered > baseline);
    for _ in 0..12 {
        assert_eq!(
            workspace
                .clone()
                .search_scope(&paths, "*.txt", "", &cancel)
                .unwrap(),
            [0]
        );
    }
    assert_eq!(
        workspace.client.next_request_id_for_test(),
        filtered,
        "query changes and cloned views share one metadata decision"
    );
    assert!(
        workspace
            .search_scope(&paths, "*.txt", "100%real.txt", &cancel)
            .unwrap()
            .is_empty()
    );
    assert!(workspace.client.next_request_id_for_test() > filtered);
    assert_eq!(
        workspace
            .search_scope(&[other, first], "*.txt", "", &cancel)
            .unwrap(),
        [1]
    );
    let filtered = workspace.client.next_request_id_for_test();
    cancel.cancel();
    assert_eq!(
        workspace
            .search_scope(&paths, "*.txt", "", &cancel)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::Interrupted
    );
    assert_eq!(
        workspace.client.next_request_id_for_test(),
        filtered,
        "cancelled query cannot submit metadata work"
    );
    workspace.client.call(remote::Command::Shutdown).unwrap();
}

#[test]
fn remote_search_scope_cache_is_bound_to_its_immutable_workspace_root() {
    let root = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    std::fs::create_dir(root.path().join("nested")).unwrap();
    let workspace = remote::workspace::Workspace::new(remote::tests::connection(
        root.path(),
        Arc::default(),
        Arc::default(),
    ));
    let cancel = crate::background::CancellationToken::new();
    let path = RemotePath::new(b"100%real.txt".to_vec()).unwrap();
    let pattern = root
        .path()
        .join("100%real.txt")
        .to_string_lossy()
        .into_owned();
    let paths = [workspace.view_key(&path)];
    assert_eq!(
        workspace
            .search_scope(&paths, &pattern, "", &cancel)
            .unwrap(),
        [0]
    );
    let nested = workspace
        .open_workspace(&workspace.view_key(&RemotePath::new(b"nested".to_vec()).unwrap()))
        .unwrap();
    assert!(
        nested
            .search_scope(&[nested.view_key(&path)], &pattern, "", &cancel)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        workspace
            .search_scope(&paths, &pattern, "", &cancel)
            .unwrap(),
        [0],
        "opening another root cannot change a pending old-root query"
    );
    workspace.client.call(remote::Command::Shutdown).unwrap();
}

#[test]
fn remote_search_filter_changes_and_undo_use_current_draft_without_uploading_it() {
    let root = tempfile::tempdir().unwrap();
    let name = "100%real.txt";
    std::fs::write(root.path().join(name), "disk needle").unwrap();
    let (mut app, workspace) = remote_app(root.path());
    let key = workspace.view_key(&RemotePath::new(name.as_bytes().to_vec()).unwrap());
    let mut editor = Editor::test_with_text("draft needle");
    editor.file_path = Some(key);
    app.tabs.push(Tab::new_file(editor));
    app.global_search_query = "needle".into();
    app.search_include_pattern = "*.txt".into();
    search(&mut app);
    assert_eq!(app.global_search_results.len(), 1);
    app.editor_mut().unwrap().paste_plain_with_text("new ");
    app.global_search_query = "new".into();
    search(&mut app);
    assert_eq!(app.global_search_results.len(), 1);
    assert!(app.global_search_results[0].line_text.starts_with("new "));
    app.editor_mut().unwrap().undo();
    search(&mut app);
    assert!(
        app.global_search_results.is_empty(),
        "a cached filter must never cache buffer contents"
    );
    app.global_search_query = "needle".into();
    app.search_exclude_pattern = "*.txt".into();
    search(&mut app);
    assert!(app.global_search_results.is_empty());
    assert_eq!(
        std::fs::read_to_string(root.path().join(name)).unwrap(),
        "disk needle"
    );
    workspace.client.call(remote::Command::Shutdown).unwrap();
}

#[test]
#[ignore = "manual UI-thread query setup benchmark; worker gated during measurement"]
fn benchmark_remote_search_query_setup_under_pending_work() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, workspace) = remote_app(root.path());
    let (release, gate) = mpsc::channel::<()>();
    let (ready, started) = mpsc::sync_channel(1);
    app.submit_global_search(move |_| {
        ready.send(()).unwrap();
        let _ = gate.recv();
    });
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    app.global_search_regex = true;
    let mut elapsed = Vec::new();
    for index in 0..24 {
        let query = if index % 2 == 0 {
            format!("needle{index}")
        } else {
            format!(
                "(?:{})",
                (0..400)
                    .map(|part| format!("name_{part}_{index}"))
                    .collect::<Vec<_>>()
                    .join("|")
            )
        };
        app.global_search_query = query;
        let started = std::time::Instant::now();
        app.update_global_search();
        elapsed.push(started.elapsed().as_secs_f64() * 1000.);
    }
    println!(
        "SEARCH_QUERY_SETUP {}",
        serde_json::json!({
            "simple_mean_ms":elapsed.iter().step_by(2).sum::<f64>()/12.,
            "alternation_mean_ms":elapsed.iter().skip(1).step_by(2).sum::<f64>()/12.,
            "max_ms":elapsed.iter().copied().fold(0., f64::max), "updates":elapsed.len()
        })
    );
    app.clear_global_search();
    drop(release);
    workspace.client.call(remote::Command::Shutdown).unwrap();
}

#[test]
#[ignore = "manual node regex construction benchmark"]
fn benchmark_remote_search_node_pattern_construction() {
    let mut timings = Vec::new();
    for whole_word in [false, true] {
        let started = std::time::Instant::now();
        for index in 0..24 {
            let query = format!(
                "(?:{})",
                (0..400)
                    .map(|part| format!("name_{part}_{index}"))
                    .collect::<Vec<_>>()
                    .join("|")
            );
            std::hint::black_box(SearchRegex::workspace(&query, true, whole_word, true).unwrap());
        }
        timings.push(started.elapsed().as_secs_f64() * 1000. / 24.);
    }
    println!(
        "SEARCH_NODE_PATTERN {}",
        serde_json::json!({"plain_mean_ms":timings[0],"whole_word_mean_ms":timings[1]})
    );
}
