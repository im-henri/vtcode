use super::*;

fn apply(previous: &Rope, change: &TextDocumentContentChangeEvent) -> Rope {
    let Some(range) = change.range else {
        return Rope::from_str(&change.text);
    };
    let (start, end) = crate::lsp_positions::range(previous, range).unwrap();
    let mut result = previous.clone();
    result.remove(start..end);
    result.insert(start, &change.text);
    result
}

#[test]
fn incremental_edits_round_trip_unicode_line_endings_and_empty_documents() {
    let texts = [
        "",
        "a",
        "abc",
        "é😀𐐀終",
        "ê😁𐐁終",
        "a\r\nb",
        "a\nb\n",
        "a\rb",
        "\r",
        "\n",
        "\r\n",
        "a\r\r\nb",
        "a\r\n\nb",
    ];
    for before in texts {
        for after in texts {
            let previous = Rope::from_str(before);
            let next = Rope::from_str(after);
            let change = incremental_change(&previous, &next);
            assert_eq!(
                apply(&previous, &change),
                next,
                "{before:?} -> {after:?}: {change:?}"
            );
        }
    }
}

#[test]
fn incremental_edits_handle_random_multiline_replacements_and_undo() {
    let mut previous = Rope::from_str(&"let café = \"😀\";\r\n".repeat(400));
    let mut seed = 42u64;
    let mut random = |limit: usize| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        (seed >> 32) as usize % limit
    };
    for _ in 0..500 {
        let mut next = previous.clone();
        for _ in 0..3 {
            let start = random(next.len_chars() + 1);
            let end = (start + random(16)).min(next.len_chars());
            next.remove(start..end);
            next.insert(
                start,
                ["", "x", "😀", "終\r\n", "\r", "\n", "éé"][random(7)],
            );
        }
        assert_eq!(
            apply(&previous, &incremental_change(&previous, &next)),
            next
        );
        assert_eq!(
            apply(&next, &incremental_change(&next, &previous)),
            previous
        );
        previous = next;
    }
}

#[test]
fn typing_in_large_shared_rope_only_compares_changed_chunks_and_sends_changed_text() {
    let previous = Rope::from_str(&"int value = 123; // 😀\r\n".repeat(100_000));
    let mut next = previous.clone();
    let insert = next.line_to_char(50_000) + 4;
    next.insert(insert, "new_");
    COMPARED_BYTES.with(|count| count.set(0));
    let change = incremental_change(&previous, &next);
    let compared = COMPARED_BYTES.with(|count| count.get());
    assert_eq!(change.text, "new_");
    assert_eq!(
        change.range,
        Some(Range::new(
            Position::new(50_000, 4),
            Position::new(50_000, 4)
        ))
    );
    assert!(
        compared < 16_384,
        "only changed chunks should be inspected, compared {compared} bytes"
    );
    assert_eq!(apply(&previous, &change), next);
}

#[test]
fn incremental_noop_updates_version_without_copying_the_document() {
    let previous = Rope::from_str(&"unchanged\n".repeat(1000));
    let change = incremental_change(&previous, &previous.clone());
    assert!(change.text.is_empty());
    assert_eq!(apply(&previous, &change), previous);
    // Equality also holds for independently rebuilt buffers, such as reloads.
    let rebuilt = Rope::from_str(&previous.to_string());
    assert_eq!(
        apply(&previous, &incremental_change(&previous, &rebuilt)),
        previous
    );
}

fn encode(writer: &mut LspWriter, message: LspOutbound) -> Option<Value> {
    let mut buffer = Vec::new();
    writer
        .encode(message, &mut buffer)
        .unwrap()
        .then(|| serde_json::from_slice(&buffer).unwrap())
}

fn open(writer: &mut LspWriter, text: &Rope) {
    encode(
        writer,
        LspOutbound::DidOpen {
            uri: "file:///test.py".into(),
            language_id: "python".into(),
            version: 0,
            text: text.clone(),
        },
    );
}

#[test]
fn writer_respects_full_incremental_and_disabled_sync_modes() {
    for kind in [
        TextDocumentSyncKind::FULL,
        TextDocumentSyncKind::INCREMENTAL,
        TextDocumentSyncKind::NONE,
    ] {
        let mut writer = LspWriter::default();
        encode(&mut writer, LspOutbound::SyncKind(kind));
        let previous = Rope::from_str("value = 1\n");
        open(&mut writer, &previous);
        let mut next = previous.clone();
        next.insert(8, "2");
        let message = encode(
            &mut writer,
            LspOutbound::DidChange {
                uri: "file:///test.py".into(),
                version: 1,
                text: next.clone(),
            },
        );
        if kind == TextDocumentSyncKind::NONE {
            assert!(message.is_none());
            continue;
        }
        let message = message.unwrap();
        assert_eq!(message["params"]["textDocument"]["version"], 1);
        let change: TextDocumentContentChangeEvent =
            serde_json::from_value(message["params"]["contentChanges"][0].clone()).unwrap();
        assert_eq!(
            change.range.is_some(),
            kind == TextDocumentSyncKind::INCREMENTAL
        );
        assert_eq!(apply(&previous, &change), next);
    }
}

#[test]
fn writer_uses_last_sent_snapshot_and_releases_it_on_close() {
    let mut writer = LspWriter::default();
    encode(
        &mut writer,
        LspOutbound::SyncKind(TextDocumentSyncKind::INCREMENTAL),
    );
    let mut previous = Rope::from_str("😀 = 1\r\n");
    open(&mut writer, &previous);
    for version in 1..=30 {
        let mut next = previous.clone();
        next.insert(4, "2");
        let message = encode(
            &mut writer,
            LspOutbound::DidChange {
                uri: "file:///test.py".into(),
                version,
                text: next.clone(),
            },
        )
        .unwrap();
        let change =
            serde_json::from_value(message["params"]["contentChanges"][0].clone()).unwrap();
        assert_eq!(apply(&previous, &change), next);
        assert_eq!(change.text, "2");
        // Requests are serialized between their document versions.
        assert_eq!(
            encode(
                &mut writer,
                LspOutbound::Json(format!("{{\"id\":{version}}}"))
            )
            .unwrap()["id"],
            version
        );
        previous = next;
    }
    encode(
        &mut writer,
        LspOutbound::DidClose {
            uri: Url::parse("file:///test.py").unwrap(),
        },
    );
    assert!(writer.documents.is_empty());
    let reopened = Rope::from_str("other contents");
    open(&mut writer, &reopened);
    assert!(writer.documents["file:///test.py"].is_instance(&reopened));
}
