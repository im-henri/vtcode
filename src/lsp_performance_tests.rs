//! Reproducible microbenchmarks, opt-in and without machine-speed assertions.
use super::*;
use std::hint::black_box;
use std::time::Instant;

#[test]
#[ignore = "measures completion UI work and duplicate diagnostic publication; no external server needed"]
fn lsp_performance_response_dispatch_and_repeated_diagnostics() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.py");
    let (client, server) = LspClient::test_pair("python", &["*.py"]);
    let mut manager = LspManager::new(directory.path().to_path_buf());
    manager.clients.push(client);
    manager.sync_document_owned(&path, 1, "value".into());
    let items: Vec<_> = (0..10_000).map(|i| CompletionItem {
        label: format!("value_{:05}", (i * 7919) % 10_000),
        documentation: Some(Documentation::String("Completion documentation".repeat(10))),
        detail: Some("function".into()), ..Default::default()
    }).collect();
    let mut preparation = Vec::new();
    let mut dispatch = Vec::new();
    for _ in 0..7 {
        manager.request_completion(&path, 0, 5, "value_1");
        server.take_messages();
        let raw = items.clone();
        let start = Instant::now();
        let prepared = completion::prepare_completions(raw, Some("value_1"));
        preparation.push(start.elapsed());
        server.queue_event(LspEvent::CompletionResponse {
            request_id: manager.active_completion_id.unwrap(), items: prepared,
        });
        let start = Instant::now();
        manager.poll();
        dispatch.push(start.elapsed());
        assert_eq!(manager.completion_items.len(), 10_000);
    }
    preparation.sort(); dispatch.sort();
    println!("LSP 10,000 completions: sorting/row preparation removed from UI={:?}; prepared-event UI dispatch={:?} (median of 7; excludes JSON and server analysis)", preparation[3], dispatch[3]);

    let diagnostics: Vec<_> = (0..2_000).map(|i| Diagnostic {
        range: Range::new(Position::new(i, 0), Position::new(i, 5)),
        message: format!("diagnostic {i}"),
        data: Some(serde_json::json!({"fix":{"payload":"detail".repeat(200)}})),
        ..Default::default()
    }).collect();
    manager.receive_diagnostics(path.clone(), Some(0), diagnostics.clone(), Instant::now());
    let revision = manager.diagnostics_version;
    let mut previous = Vec::new();
    let mut current = Vec::new();
    for _ in 0..15 {
        let incoming = diagnostics.clone();
        let start = Instant::now();
        // Previous publication retained a deep context copy and rebuilt every
        // display row, even for the same report. Include their destruction.
        let context = incoming.clone();
        let display: Vec<_> = incoming.into_iter().map(|d| LspDiagnostic {
            line:d.range.start.line as usize, col:d.range.start.character as usize,
            end_line:d.range.end.line as usize, end_col:d.range.end.character as usize,
            message:d.message, severity:d.severity.unwrap_or(DiagnosticSeverity::INFORMATION),
        }).collect();
        black_box((&context, &display));
        drop((context, display));
        previous.push(start.elapsed());
        let incoming = diagnostics.clone();
        let start = Instant::now();
        manager.receive_diagnostics(path.clone(), Some(0), incoming, Instant::now());
        current.push(start.elapsed());
        assert_eq!(manager.diagnostics_version, revision);
    }
    previous.sort(); current.sort();
    println!("LSP repeated 2,000-diagnostic report with fix data: deep copy/rebuild={:?}; shared report/unchanged display={:?} (median of 15; excludes parsing and UI redraw savings)", previous[7], current[7]);
}

#[test]
#[ignore = "prints completion detail lookup timings; no external server needed"]
fn lsp_performance_completion_detail_lookup_and_cache() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.py");
    let (client, server) = LspClient::test_pair("python", &["*.py"]);
    let mut manager = LspManager::new(directory.path().to_path_buf());
    manager.clients.push(client);
    manager.sync_document_owned(&path, 1, "value".into());
    manager.request_completion(&path, 0, 5, "value");
    let items: Vec<_> = (0..10_000).map(|i| serde_json::json!({
        "label":"value", "data":{"index":i}, "detail":"candidate overload"
    })).collect();
    server.respond(serde_json::json!({"id":manager.active_completion_id,"result":items}));
    manager.poll();
    let index = 9_999;
    let original = manager.completion_items[index].original_item.clone().unwrap();
    let started = Instant::now();
    for _ in 0..200 {
        let found = manager.completion_items.iter().position(|item| item.original_item.as_ref() == Some(black_box(&original)));
        assert_eq!(black_box(found), Some(index));
    }
    let scan_time = started.elapsed();
    let started = Instant::now();
    for _ in 0..200 {
        black_box(manager.completion_items.get(black_box(index)).unwrap());
    }
    let index_time = started.elapsed();
    server.take_messages();
    manager.request_completion_resolve(&path, index);
    server.respond(serde_json::json!({"id":manager.active_resolve_id,"result":{
        "label":"value", "documentation":"resolved"
    }}));
    manager.poll();
    assert_eq!(server.take_messages().len(), 1);
    let started = Instant::now();
    for _ in 0..10_000 { assert!(manager.request_completion_resolve(&path, index)); }
    println!("LSP completion details: 10,000 rows, 200 reply lookups; scan={scan_time:?}, index={index_time:?}; 10,000 cache visits={:?}, zero additional requests", started.elapsed());
    assert!(server.take_messages().is_empty());
}

#[test]
#[ignore = "prints LSP transport and sorting timings; no external server needed"]
fn lsp_performance_shared_snapshots_and_completion_sorting() {
    let initial = Rope::from_str(&"int value = 123; // 😀\r\n".repeat(100_000));
    let mut edited = initial.clone();
    let position = edited.line_to_char(50_000) + 4;
    let snapshots: Vec<_> = (1..=30)
        .map(|_| {
            edited.insert(position, "x");
            edited.clone()
        })
        .collect();
    let start = Instant::now();
    let mut full_bytes = 0;
    for (version, rope) in snapshots.iter().enumerate() {
        // The previous writer allocated a full snapshot, a JSON Value, a JSON
        // string and a second string containing Content-Length framing.
        let text = rope.to_string();
        let json = serde_json::json!({"jsonrpc":"2.0","method":"textDocument/didChange", "params":{
            "textDocument":{"uri":"file:///perf.cpp","version":version},"contentChanges":[{"text":text}]
        }}).to_string();
        full_bytes += json.len();
        black_box(format!("Content-Length: {}\r\n\r\n{}", json.len(), json));
    }
    let full_time = start.elapsed();
    let mut writer = transport::LspWriter::default();
    let mut buffer = Vec::new();
    writer
        .encode(
            LspOutbound::SyncKind(TextDocumentSyncKind::INCREMENTAL),
            &mut buffer,
        )
        .unwrap();
    writer
        .encode(
            LspOutbound::DidOpen {
                uri: "file:///perf.cpp".into(),
                language_id: "cpp".into(),
                version: 0,
                text: initial.clone(),
            },
            &mut buffer,
        )
        .unwrap();
    let start = Instant::now();
    let mut incremental_bytes = 0;
    for (version, rope) in snapshots.iter().enumerate() {
        writer
            .encode(
                LspOutbound::DidChange {
                    uri: "file:///perf.cpp".into(),
                    version: version as i32 + 1,
                    text: rope.clone(),
                },
                &mut buffer,
            )
            .unwrap();
        incremental_bytes += buffer.len();
        black_box(&buffer);
    }
    let incremental_time = start.elapsed();
    println!(
        "LSP sync: {}-byte document, 30 single-character edits; full={full_time:?} / {full_bytes} JSON bytes, incremental={incremental_time:?} / {incremental_bytes} JSON bytes",
        initial.len_bytes()
    );
    assert!(incremental_bytes < full_bytes / 1000);

    let items: Vec<_> = (0..10_000)
        .map(|i| CompletionItem {
            label: format!("candidate_{:05}", (i * 7919) % 10_000),
            kind: Some(CompletionItemKind::FUNCTION),
            ..Default::default()
        })
        .collect();
    let mut old = items.clone();
    let start = Instant::now();
    old.sort_by(|a, b| {
        let a_lower = a.label.to_lowercase();
        let b_lower = b.label.to_lowercase();
        let rank = |item: &CompletionItem, lower: &str| {
            (
                LspManager::completion_match_rank(lower, "candidate_1"),
                LspManager::completion_kind_rank(item.kind),
                u8::from(item.label.contains("::") || item.label.contains('.')),
                item.label.len(),
            )
        };
        rank(a, &a_lower)
            .cmp(&rank(b, &b_lower))
            .then_with(|| a_lower.cmp(&b_lower))
            .then_with(|| a.label.cmp(&b.label))
    });
    let old_time = start.elapsed();
    let mut new = items;
    let start = Instant::now();
    completion::sort_completions(&mut new, "candidate_1");
    let new_time = start.elapsed();
    assert_eq!(old, new);
    println!(
        "LSP sort: 10,000 completions; comparator allocations={old_time:?}, cached keys={new_time:?}"
    );
}
