use super::*;

fn entry(line: usize) -> HighlightEntry {
    HighlightEntry {
        line,
        hash: line as u64,
        spans: (0..64)
            .map(|_| (Style::default(), "a".repeat(64)))
            .collect(),
        span_char_lengths: vec![64; 64],
        comment_ranges: vec![(0, 4096)],
    }
}

fn template(highlighter: &mut Highlighter, path: &Path, rope: &Rope) -> HighlightBatch {
    let job = highlighter
        .prepare_job(rope, path, 0, None, 0..1, None, "dark-plus")
        .unwrap();
    let (syntaxes, themes) = highlighter.shared_sets();
    let mut batch = job.run(rope, &syntaxes, &themes, || false);
    batch.entries.clear();
    batch.checkpoints.clear();
    batch
}

fn assert_accounting(cache: &DocumentCache) {
    let mut bytes = 0;
    cache.lines.for_each(.., |_, line| {
        assert_eq!(line.bytes, line.allocation_bytes());
        bytes += line.bytes;
    });
    assert_eq!(cache.line_bytes, bytes);
}

#[test]
fn authoritative_and_provisional_rows_obey_byte_budgets_and_remain_reusable() {
    for provisional in [false, true] {
        let path = Path::new("memory.rs");
        let rope = Rope::from_str(&"line\n".repeat(2000));
        let mut highlighter = Highlighter::new();
        let base = template(&mut highlighter, path, &rope);
        for start in (0..1600).step_by(100) {
            let mut batch = base.clone();
            batch.provisional = provisional;
            batch.visible = start..start + 100;
            batch.entries = (start..start + 100).map(entry).collect();
            assert!(highlighter.apply_batch(batch));
            let cache = &highlighter.documents[path];
            assert_accounting(cache);
            assert!(cache.line_bytes <= MAX_CACHED_LINE_BYTES_PER_DOCUMENT);
            // The worker's visible results and immutable snapshots survive
            // eviction of older rows, including provisional-only scrolling.
            for line in start..start + 100 {
                assert!(cache.lines.get(&line).is_some());
            }
        }
        assert!(highlighter.documents[path].lines.len() < 1600);
        assert!(
            highlighter
                .prepare_job(&rope, path, 0, None, 0..100, None, "dark-plus")
                .is_some()
        );
    }
}

#[test]
fn provisional_only_scrolling_cannot_grow_without_a_row_bound() {
    let path = Path::new("pending.rs");
    let rope = Rope::from_str(&"line\n".repeat(7000));
    let mut highlighter = Highlighter::new();
    let base = template(&mut highlighter, path, &rope);
    for start in (0..6500).step_by(100) {
        let mut batch = base.clone();
        batch.provisional = true;
        batch.visible = start..start + 100;
        batch.entries = (start..start + 100)
            .map(|line| HighlightEntry {
                line,
                hash: line as u64,
                spans: vec![(Style::default(), "line\n".into())],
                span_char_lengths: vec![5],
                comment_ranges: vec![],
            })
            .collect();
        assert!(highlighter.apply_batch(batch));
        let cache = &highlighter.documents[path];
        assert_accounting(cache);
        assert!(cache.lines.len() <= MAX_CACHED_LINES_PER_DOCUMENT);
    }
}

#[test]
fn replacements_and_newline_coordinate_changes_keep_exact_memory_accounting() {
    let path = Path::new("accounting.rs");
    let mut rope = Rope::from_str(&"let value = 1; // comment\n".repeat(100));
    let mut highlighter = Highlighter::new();
    let (syntaxes, themes) = highlighter.shared_sets();
    for (version, line, text) in [
        (0, 0, ""),
        (1, 10, "/*\n"),
        (2, 20, "*/\n"),
        (3, 5, "界"),
        (4, 0, "//\n"),
    ] {
        rope.insert(rope.line_to_char(line), text);
        let job = highlighter
            .prepare_job(
                &rope,
                path,
                version,
                Some((line, line + 2)),
                0..rope.len_lines(),
                None,
                "dark-plus",
            )
            .unwrap();
        let stale = job.run_with_budget(
            &rope,
            &syntaxes,
            &themes,
            &mut || false,
            Duration::from_secs(5),
        );
        assert!(highlighter.apply_batch(stale.clone()));
        assert_accounting(&highlighter.documents[path]);
        // Replacing identical results must neither leak counters nor evict
        // the Rc snapshots that glyph caches already reference.
        let first = highlighter.documents[path].lines[&0].spans.clone();
        assert!(highlighter.apply_batch(stale));
        assert!(Rc::ptr_eq(
            &first,
            &highlighter.documents[path].lines[&0].spans
        ));
        assert_accounting(&highlighter.documents[path]);
    }
    for version in 5..15 {
        rope.remove(rope.line_to_char(10)..rope.line_to_char(12));
        highlighter.prepare_job(
            &rope,
            path,
            version,
            Some((10, 10)),
            0..20,
            None,
            "dark-plus",
        );
        assert_accounting(&highlighter.documents[path]);
    }
}

#[test]
fn total_budget_evicts_old_documents_without_dropping_the_current_view() {
    let mut highlighter = Highlighter::new();
    let rope = Rope::from_str(&"line\n".repeat(1000));
    for index in 0..6 {
        let path = PathBuf::from(format!("memory-{index}.rs"));
        template(&mut highlighter, &path, &rope);
        let line = CachedLine::from_entry(entry(0), 0, true);
        let cache = highlighter.documents.get_mut(&path).unwrap();
        for at in 0..1000 {
            cache.insert_line(at, line.clone());
        }
        assert_accounting(cache);
        highlighter.evict_old_documents(&path);
        let bytes: usize = highlighter
            .documents
            .values()
            .map(|cache| cache.line_bytes)
            .sum();
        assert!(bytes <= MAX_CACHED_LINE_BYTES);
        assert!(highlighter.documents.contains_key(&path));
    }
    assert!(!highlighter.documents.contains_key(Path::new("memory-0.rs")));
    assert!(highlighter.documents.len() < 6);
}

#[test]
fn visible_working_set_is_not_evicted_into_a_reparse_loop() {
    let path = Path::new("visible.rs");
    let rope = Rope::from_str(&"line\n".repeat(1600));
    let mut highlighter = Highlighter::new();
    let mut batch = template(&mut highlighter, path, &rope);
    batch.visible = 0..1600;
    batch.entries = (0..1600).map(entry).collect();
    assert!(highlighter.apply_batch(batch));
    assert!(highlighter.documents[path].line_bytes > MAX_CACHED_LINE_BYTES_PER_DOCUMENT);
    assert!(
        highlighter
            .prepare_job(&rope, path, 0, None, 0..1600, None, "dark-plus")
            .is_none()
    );
    let cache = highlighter.documents.get_mut(path).unwrap();
    cache.trim_lines(&(1590..1600));
    assert!(cache.line_bytes <= CACHED_LINE_BYTES_AFTER_EVICTION);
    assert_accounting(cache);
}
