use super::*;
use crate::background::BackgroundWorkers;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};

struct Payload {
    live: Arc<AtomicUsize>,
    _bytes: Vec<u8>,
}
impl Payload {
    fn new(live: &Arc<AtomicUsize>, peak: &AtomicUsize) -> Self {
        peak.fetch_max(live.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
        Self {
            live: live.clone(),
            _bytes: vec![0; 64 * 1024],
        }
    }
}
impl Drop for Payload {
    fn drop(&mut self) {
        self.live.fetch_sub(1, Ordering::SeqCst);
    }
}

#[test]
fn rapid_requests_retain_one_pending_payload_and_yield_to_other_pool_work() {
    let mut workers = BackgroundWorkers::new();
    workers.use_single_cpu_worker();
    let pool = workers.pool_sender();
    let latest = LatestWork::default();
    let live = Arc::new(AtomicUsize::new(0));
    let peak = AtomicUsize::new(0);
    let (entered, started) = mpsc::channel();
    let (release, unblock) = mpsc::channel();
    let payload = Payload::new(&live, &peak);
    let first = CancellationToken::new();
    latest.replace(
        pool.clone(),
        Job {
            cancel: first.clone(),
            run: Box::new(move || {
                entered.send(()).unwrap();
                unblock.recv_timeout(Duration::from_secs(5)).unwrap();
                drop(payload);
            }),
        },
    );
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    let (events, results) = mpsc::channel();
    let mut tokens = Vec::new();
    for index in 0..100 {
        let payload = Payload::new(&live, &peak);
        let token = CancellationToken::new();
        tokens.push(token.clone());
        let events = events.clone();
        latest.replace(
            pool.clone(),
            Job {
                cancel: token,
                run: Box::new(move || {
                    drop(payload);
                    events.send(index).unwrap();
                }),
            },
        );
    }
    assert!(first.is_cancelled());
    assert!(tokens[..99].iter().all(CancellationToken::is_cancelled));
    assert!(!tokens[99].is_cancelled());
    assert_eq!(
        live.load(Ordering::SeqCst),
        2,
        "only running and newest pending captures may survive"
    );
    assert!(
        peak.load(Ordering::SeqCst) <= 3,
        "replacement may briefly own the new and previous pending captures"
    );
    pool.send(Box::new(move || events.send(1000).unwrap()))
        .unwrap();
    release.send(()).unwrap();
    assert_eq!(results.recv_timeout(Duration::from_secs(5)).unwrap(), 1000);
    assert_eq!(results.recv_timeout(Duration::from_secs(5)).unwrap(), 99);
    assert_eq!(live.load(Ordering::SeqCst), 0);
}

#[test]
fn closing_a_slot_releases_queued_captures_and_cancels_before_execution() {
    let mut workers = BackgroundWorkers::new();
    workers.use_single_cpu_worker();
    let pool = workers.pool_sender();
    let (entered, started) = mpsc::channel();
    let (release, unblock) = mpsc::channel();
    pool.send(Box::new(move || {
        entered.send(()).unwrap();
        unblock.recv_timeout(Duration::from_secs(5)).unwrap();
    }))
    .unwrap();
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    let latest = LatestWork::default();
    let cancel = CancellationToken::new();
    let live = Arc::new(AtomicUsize::new(0));
    let peak = AtomicUsize::new(0);
    let payload = Payload::new(&live, &peak);
    latest.replace(
        pool.clone(),
        Job {
            cancel: cancel.clone(),
            run: Box::new(move || {
                drop(payload);
                panic!("closed request ran");
            }),
        },
    );
    drop(latest);
    assert!(cancel.is_cancelled());
    assert_eq!(live.load(Ordering::SeqCst), 0);
    let (done, finished) = mpsc::channel();
    pool.send(Box::new(move || done.send(()).unwrap())).unwrap();
    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(5)).unwrap();
}

#[test]
fn a_panicking_request_is_cancelled_and_does_not_stop_the_latest_successor() {
    let mut workers = BackgroundWorkers::new();
    workers.use_single_cpu_worker();
    let pool = workers.pool_sender();
    let latest = LatestWork::default();
    let token = CancellationToken::new();
    let (entered, started) = mpsc::channel();
    let (release, unblock) = mpsc::channel();
    latest.replace(
        pool.clone(),
        Job {
            cancel: token.clone(),
            run: Box::new(move || {
                entered.send(()).unwrap();
                unblock.recv_timeout(Duration::from_secs(5)).unwrap();
                panic!("intentional request panic");
            }),
        },
    );
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    let (done, finished) = mpsc::channel();
    latest.replace(
        pool,
        Job {
            cancel: CancellationToken::new(),
            run: Box::new(move || done.send(()).unwrap()),
        },
    );
    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(token.is_cancelled());
}
