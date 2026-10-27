use super::*;
use std::time::{Duration, Instant};

#[test]
fn completion_waits_for_presentation_without_blocking_the_navigation_pool() {
    let mut background = crate::background::BackgroundWorkers::new();
    background.use_single_cpu_worker();
    let worker = VtCompleteWorker::default();
    let foreground = worker.activity().enter();
    worker.submit(
        request(1, background.sender.clone()),
        background.completion_sender(),
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while worker.queue.lock().unwrap().active_cancel.is_none() {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    let (send, receive) = std::sync::mpsc::channel();
    background
        .interactive_sender()
        .send(Box::new(move || send.send(()).unwrap()))
        .unwrap();
    receive
        .recv_timeout(Duration::from_secs(2))
        .expect("navigation must not queue behind completion");
    assert!(
        background.poll().is_empty(),
        "completion must remain parked through presentation"
    );
    drop(foreground);
    loop {
        if background
            .poll()
            .iter()
            .any(|result| matches!(result, BackgroundResult::VtComplete { generation: 1, .. }))
        {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn replacing_a_running_request_cancels_it_without_relying_on_the_callers_token() {
    let background = crate::background::BackgroundWorkers::new();
    let worker = VtCompleteWorker::default();
    let old = request(1, background.sender.clone());
    let cancelled = old.cancel.clone();
    worker.queue.lock().unwrap().active_cancel = Some(cancelled.clone());
    worker.push(request(2, background.sender.clone()));
    assert!(cancelled.is_cancelled());
}

#[test]
fn automatic_comment_queries_use_the_local_word_index() {
    let active = snapshot(3, "identifierUseful\n// commentIdentifier co", 1, true);
    let mut active = active;
    active.cursor_col = active.rope.line(1).len_chars();
    let mut cache = TokenCache::default();
    let result = compute_contextual_invoked(
        &active,
        "co",
        "Rust",
        &Default::default(),
        &mut cache,
        false,
        || false,
    )
    .unwrap();
    assert!(result.suggestions.iter().any(|word| word == "commentIdentifier"));
    assert_eq!(cache.documents.len(), 1);
    let result = compute_contextual_invoked(
        &active,
        "co",
        "Rust",
        &Default::default(),
        &mut cache,
        true,
        || false,
    )
    .unwrap();
    assert!(
        !result.suggestions.is_empty(),
        "manual completion remains available in comments"
    );
    assert_eq!(cache.documents.len(), 1);
}

#[test]
fn automatic_quoted_queries_do_not_build_the_local_word_index() {
    let mut active = snapshot(4, "identifierUseful\nlet value = \"identifierU", 1, true);
    active.cursor_col = active.rope.line(1).len_chars();
    let mut cache = TokenCache::default();
    let result = compute_contextual_invoked(
        &active, "identifierU", "Rust", &Default::default(), &mut cache, false, || false,
    ).unwrap();
    assert!(result.suggestions.is_empty());
    assert!(cache.documents.is_empty());
    let result = compute_contextual_invoked(
        &active, "identifierU", "Rust", &Default::default(), &mut cache, true, || false,
    ).unwrap();
    assert!(result.suggestions.iter().any(|word| word == "identifierUseful"));
    assert_eq!(cache.documents.len(), 1);
}

#[test]
fn cancelled_snapshot_comparison_stops_between_shared_rope_leaves() {
    use std::cell::Cell;
    let left = ropey::Rope::from_str(&"identifierValue\n".repeat(100_000));
    let right = ropey::Rope::from_str(&left.to_string());
    let checks = Cell::new(0);
    let result = crate::rope_changes::changed_chars_cancellable(&left, &right, &|| {
        checks.set(checks.get() + 1);
        checks.get() >= 8
    });
    assert!(result.is_none());
    assert_eq!(checks.get(), 8);
    assert_eq!(
        crate::rope_changes::changed_chars_cancellable(&left, &right, &|| false).unwrap(),
        crate::rope_changes::changed_chars(&left, &right)
    );
}
