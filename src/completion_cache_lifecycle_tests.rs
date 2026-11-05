use super::*;
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

fn waiters(session: &Session, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while session.gate.state.lock().unwrap().waiters < count {
        assert!(
            Instant::now() < deadline,
            "replacement worker did not reach its handoff barrier"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn rapid_consecutive_restarts_wait_in_order_without_waiting_on_younger_sessions() {
    let root = tempfile::tempdir().unwrap();
    let first = Session::register(root.path());
    let second = Session::register(root.path());
    let third = Session::register(root.path());
    first.stopping();
    second.stopping();
    third.stopping();
    let oldest = Completion::new(Some(first.clone()));
    let (finished, replies) = mpsc::channel();
    let workers: Vec<_> = [(second, 2), (third, 3)]
        .into_iter()
        .map(|(session, id)| {
            let finished = finished.clone();
            std::thread::spawn(move || {
                let completion = Completion::new(Some(session));
                completion.wait_before_restore();
                finished.send(id).unwrap();
            })
        })
        .collect();
    waiters(&first, 2);
    assert!(replies.try_recv().is_err());
    // Own and younger retirement records must not deadlock the oldest writer.
    oldest.wait_before_restore();
    drop(oldest);
    assert_eq!(replies.recv_timeout(Duration::from_secs(3)).unwrap(), 2);
    assert_eq!(replies.recv_timeout(Duration::from_secs(3)).unwrap(), 3);
    for worker in workers {
        worker.join().unwrap();
    }
    assert!(first.gate.state.lock().unwrap().retiring.is_empty());
}

#[test]
fn live_sessions_and_unrelated_cache_roots_do_not_serialize_indexing() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let first = Session::register(a.path());
    let same_cache = Session::register(a.path());
    same_cache.wait(); // A live owner is not a shutdown barrier.
    first.stopping();
    let other_cache = Session::register(b.path());
    other_cache.wait();
    assert!(!Arc::ptr_eq(&first.gate, &other_cache.gate));
    first.finish();
}

#[test]
fn a_finished_worker_cannot_be_registered_as_a_new_shutdown_dependency() {
    let root = tempfile::tempdir().unwrap();
    let finished = Session::register(root.path());
    finished.finish();
    finished.stopping();
    let replacement = Session::register(root.path());
    replacement.wait();
    assert!(replacement.gate.state.lock().unwrap().retiring.is_empty());
}

#[test]
fn handoff_guards_release_waiters_on_unwind_and_registry_keeps_no_cache_alive() {
    let root = tempfile::tempdir().unwrap();
    let first = Session::register(root.path());
    let old_gate = Arc::downgrade(&first.gate);
    first.stopping();
    let replacement = Session::register(root.path());
    let worker = std::thread::spawn(move || {
        let _completion = Completion::new(Some(first));
        panic!("simulate an indexing worker failure");
    });
    replacement.wait();
    assert!(worker.join().is_err());
    drop(replacement);
    assert!(
        old_gate.upgrade().is_none(),
        "registry entries must not retain session or document state"
    );
}
