use super::*;
use std::{cell::Cell, time::Duration};

fn analyze_document(cache: &mut Cache, document: u64, rope: &Rope) -> Metrics {
    let metrics = cache
        .analyze(
            document,
            rope,
            Instant::now() + Duration::from_secs(20),
            &|| false,
        )
        .unwrap();
    assert_eq!(metrics, super::super::analyze_editor_content(rope));
    assert!(cache.snapshots.len() <= MAX_DOCUMENTS);
    assert!(cache.source_bytes <= MAX_SOURCE_BYTES);
    assert!(cache.cache_bytes <= MAX_CACHE_BYTES);
    assert_eq!(
        cache.source_bytes,
        cache
            .snapshots
            .iter()
            .map(|snapshot| snapshot.source.len_bytes())
            .sum::<usize>()
    );
    assert_eq!(
        cache.cache_bytes,
        cache
            .snapshots
            .iter()
            .map(|snapshot| snapshot.retained_bytes)
            .sum::<usize>()
    );
    metrics
}

#[test]
fn switching_files_reuses_each_documents_summaries_and_keeps_edits_local() {
    let mut cache = Cache::default();
    let mut documents: Vec<_> = (1..=3)
        .map(|id| Rope::from_str(&format!("root_{id}:\n  TODO BUG 界\n").repeat(2_000)))
        .collect();
    for (id, source) in documents.iter().enumerate() {
        analyze_document(&mut cache, id as u64 + 1, source);
        assert_eq!(cache.scanned_lines, source.len_lines());
    }
    for _ in 0..3 {
        for (id, source) in documents.iter().enumerate().rev() {
            analyze_document(&mut cache, id as u64 + 1, source);
            assert_eq!(cache.scanned_lines, 0);
        }
    }
    let at = documents[0].line_to_char(128);
    documents[0].insert(at, "  added:\n");
    analyze_document(&mut cache, 1, &documents[0]);
    assert!(cache.scanned_lines <= BLOCK_LINES * 2 + 1);
    analyze_document(&mut cache, 2, &documents[1]);
    assert_eq!(cache.scanned_lines, 0);
}

#[test]
fn unchanged_tabs_share_completed_metrics_and_edits_preserve_previous_results() {
    let mut cache = Cache::default();
    let mut source = Rope::from_str("outer:\n  inner:\n    value\n");
    let first = analyze_document(&mut cache, 1, &source);
    let bytes = cache.cache_bytes;
    let repeated = analyze_document(&mut cache, 1, &source);
    assert!(Arc::ptr_eq(&first.4, &repeated.4));
    assert_eq!(cache.cache_bytes, bytes);
    assert_eq!(cache.scanned_lines, 0);
    source.insert(0, "new:\n");
    let changed = analyze_document(&mut cache, 1, &source);
    assert!(!Arc::ptr_eq(&first.4, &changed.4));
    assert_eq!(first.4[0].line, 0);
    assert_eq!(first.4[1].parent, Some(0));
    assert_eq!(changed.4.len(), 3);
}

#[test]
fn cancelled_cache_hits_do_not_change_recency_or_drop_completed_results() {
    let mut cache = Cache::default();
    let source = Rope::from_str("scope:\n  value\n");
    let first = analyze_document(&mut cache, 1, &source);
    analyze_document(&mut cache, 2, &source);
    let checks = Cell::new(0);
    assert!(
        cache
            .analyze(1, &source, Instant::now() + Duration::from_secs(1), &|| {
                checks.set(checks.get() + 1);
                checks.get() == 2
            })
            .is_none()
    );
    assert_eq!(cache.snapshots.front().unwrap().document, 1);
    let repeated = analyze_document(&mut cache, 1, &source);
    assert!(Arc::ptr_eq(&first.4, &repeated.4));
}

#[test]
fn document_limit_evicts_the_least_recently_used_entry() {
    let source = Rope::from_str(&"scope:\n  value\n".repeat(100));
    let mut cache = Cache::default();
    for document in 1..=MAX_DOCUMENTS as u64 {
        analyze_document(&mut cache, document, &source);
    }
    analyze_document(&mut cache, 1, &source);
    assert_eq!(cache.scanned_lines, 0);
    analyze_document(&mut cache, MAX_DOCUMENTS as u64 + 1, &source);
    assert!(
        cache
            .snapshots
            .iter()
            .any(|snapshot| snapshot.document == 1)
    );
    assert!(
        !cache
            .snapshots
            .iter()
            .any(|snapshot| snapshot.document == 2)
    );
    analyze_document(&mut cache, 1, &source);
    assert_eq!(cache.scanned_lines, 0);
    analyze_document(&mut cache, 2, &source);
    assert_eq!(cache.scanned_lines, source.len_lines());
}

#[test]
fn cancellation_keeps_other_documents_and_the_previous_lru_order() {
    let mut cache = Cache::default();
    let original = Rope::from_str(&"value\n".repeat(2_000));
    analyze_document(&mut cache, 1, &original);
    analyze_document(&mut cache, 2, &original);
    let order: Vec<_> = cache
        .snapshots
        .iter()
        .map(|snapshot| snapshot.document)
        .collect();
    let mut changed = original.clone();
    changed.insert(0, &"TODO BUG αβ ".repeat(100_000));
    let polls = Cell::new(0);
    assert!(
        cache
            .analyze(
                1,
                &changed,
                Instant::now() + Duration::from_secs(20),
                &|| {
                    polls.set(polls.get() + 1);
                    polls.get() >= 20
                }
            )
            .is_none()
    );
    assert!(
        cache
            .analyze(1, &changed, Instant::now(), &|| false)
            .is_none()
    );
    assert_eq!(
        cache
            .snapshots
            .iter()
            .map(|snapshot| snapshot.document)
            .collect::<Vec<_>>(),
        order
    );
    for snapshot in &cache.snapshots {
        assert!(snapshot.source.is_instance(&original));
    }
    for document in [1, 2] {
        analyze_document(&mut cache, document, &original);
        assert_eq!(cache.scanned_lines, 0);
    }
}

#[test]
fn oversized_results_leave_other_cached_documents_available() {
    let mut cache = Cache::default();
    let small = Rope::from_str("root:\n  value\n");
    analyze_document(&mut cache, 1, &small);
    let large = Rope::from_str(&"scope:\n".repeat(280_000));
    assert_eq!(analyze_document(&mut cache, 2, &large).4.len(), 280_000);
    assert_eq!(cache.snapshots.len(), 1);
    analyze_document(&mut cache, 1, &small);
    assert_eq!(cache.scanned_lines, 0);
}

#[test]
fn the_metadata_budget_is_shared_across_all_cached_documents() {
    let mut cache = Cache::default();
    let source = Rope::from_str(&"scope:\n".repeat(65_536));
    for document in 1..=MAX_DOCUMENTS as u64 {
        analyze_document(&mut cache, document, &source);
    }
    assert!(cache.snapshots.len() < MAX_DOCUMENTS);
    assert!(
        !cache
            .snapshots
            .iter()
            .any(|snapshot| snapshot.document == 1)
    );
    analyze_document(&mut cache, MAX_DOCUMENTS as u64, &source);
    assert_eq!(cache.scanned_lines, 0);
}

#[test]
fn source_budget_evicts_without_limiting_exact_results() {
    let mut cache = Cache::default();
    let source = Rope::from_str(&" ".repeat(MAX_SOURCE_BYTES / 2 + 1));
    for document in 1..=3 {
        let metrics = analyze_document(&mut cache, document, &source);
        assert_eq!(metrics.2, source.len_chars());
        assert_eq!(cache.snapshots.len(), 1);
        assert_eq!(cache.snapshots.back().unwrap().document, document);
    }
    analyze_document(&mut cache, 3, &source);
    assert_eq!(cache.scanned_lines, 0);
}
