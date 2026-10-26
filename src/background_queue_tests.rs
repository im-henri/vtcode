use super::*;
use std::{sync::mpsc, thread, time::Duration};
const WAIT: Duration = Duration::from_secs(5);

#[test]
fn priority_is_fifo_and_bulk_work_gets_a_turn_during_interactive_bursts() {
    let (send, recv) = channel();
    for n in 0..3 {
        send.send(n, Priority::Analysis).unwrap();
    }
    for n in 10..22 {
        send.send(n, Priority::Interactive).unwrap();
    }
    drop(send);
    let actual: Vec<_> = std::iter::from_fn(|| recv.recv()).collect();
    assert_eq!(
        actual,
        vec![10, 11, 12, 13, 0, 14, 15, 16, 17, 1, 18, 19, 20, 21, 2]
    );
}

#[test]
fn analysis_only_and_interactive_only_drain_and_last_sender_closes_waiters() {
    for priority in [Priority::Analysis, Priority::Interactive] {
        let (send, recv) = channel();
        let retained = send.clone();
        for n in 0..32 {
            send.send(n, priority).unwrap();
        }
        drop(send);
        for n in 0..32 {
            assert_eq!(recv.recv(), Some(n));
        }
        let (done, completion) = mpsc::channel();
        for _ in 0..3 {
            let recv = recv.clone();
            let done = done.clone();
            thread::spawn(move || {
                done.send(recv.recv()).unwrap();
            });
        }
        drop(retained);
        for _ in 0..3 {
            assert_eq!(completion.recv_timeout(WAIT).unwrap(), None);
        }
    }
}

#[test]
fn multiple_workers_execute_every_item_exactly_once() {
    let (send, recv) = channel();
    let (done, completion) = mpsc::channel();
    for _ in 0..3 {
        let recv = recv.clone();
        let done = done.clone();
        thread::spawn(move || {
            while let Some(item) = recv.recv() {
                done.send(item).unwrap();
            }
        });
    }
    for n in 0..2_000 {
        send.send(
            n,
            if n % 3 == 0 {
                Priority::Interactive
            } else {
                Priority::Analysis
            },
        )
        .unwrap();
    }
    drop(send);
    let mut actual: Vec<_> = (0..2_000)
        .map(|_| completion.recv_timeout(WAIT).unwrap())
        .collect();
    actual.sort_unstable();
    assert_eq!(actual, (0..2_000).collect::<Vec<_>>());
}

#[test]
fn abandoned_jobs_are_dropped_outside_the_queue_lock() {
    struct ReentrantDrop(Option<Sender<ReentrantDrop>>, mpsc::Sender<()>);
    impl Drop for ReentrantDrop {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                // A captured sender may be released or submit cleanup work on
                // destruction. Neither may deadlock the worker queue.
                assert!(
                    sender
                        .send(ReentrantDrop(None, self.1.clone()), Priority::Analysis)
                        .is_err()
                );
                self.1.send(()).unwrap();
            }
        }
    }
    let (send, recv) = channel();
    let (done, completion) = mpsc::channel();
    assert!(
        send.send(
            ReentrantDrop(Some(send.clone()), done),
            Priority::Interactive
        )
        .is_ok()
    );
    thread::spawn(move || drop(recv));
    completion.recv_timeout(WAIT).unwrap();
}
