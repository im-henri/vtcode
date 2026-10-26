//! Reads and filesystem metadata work are independent of the CPU and save
//! pools. Opening/restoring tabs or browsing folders must not create a thread
//! per file or occupy syntax slots.
use super::{ActiveJobGuard, PoolJobGuard, PoolLoad, PoolTask, PoolThreadGuard, QueuedTask, UiWake};
use std::path::{Path, PathBuf};
use std::{collections::VecDeque, sync::{Arc, Condvar, Mutex, atomic::{AtomicUsize, Ordering}}, thread};

const FOREGROUND_BURST: usize = 8;

#[derive(Default)]
struct Queue {
    foreground: VecDeque<QueuedTask>,
    background: VecDeque<(PathBuf, QueuedTask)>,
    foreground_run: usize,
    stopped: bool,
}

impl Queue {
    fn pop(&mut self) -> Option<QueuedTask> {
        // Explicit opens jump ahead of queued reloads/recovery, with a turn
        // for background reads after each burst to prevent starvation.
        if !self.foreground.is_empty()
            && (self.foreground_run < FOREGROUND_BURST || self.background.is_empty())
        {
            self.foreground_run = self.foreground_run.saturating_add(1);
            self.foreground.pop_front()
        } else {
            self.foreground_run = 0;
            self.background.pop_front().map(|(_, task)| task)
        }
    }
}

pub(super) struct ReadPool {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    pub(super) load: Arc<PoolLoad>,
    active: Arc<AtomicUsize>,
    wake: UiWake,
}

impl ReadPool {
    pub(super) fn new(size: usize, active: Arc<AtomicUsize>, wake: UiWake) -> Self {
        let queue = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let load = Arc::new(PoolLoad { live: AtomicUsize::new(size), ..PoolLoad::default() });
        for index in 0..size {
            let queue = queue.clone();
            let load = load.clone();
            thread::Builder::new().name(format!("file-read-{index}")).spawn(move || {
                let _thread_guard = PoolThreadGuard(load);
                crate::worker_priority::lower_current_thread();
                loop {
                    let task = {
                        let (lock, ready) = &*queue;
                        let mut queue = lock.lock().expect("file read queue poisoned");
                        loop {
                            if queue.stopped { return; }
                            if let Some(task) = queue.pop() { break task; }
                            queue = ready.wait(queue).expect("file read queue poisoned");
                        }
                    };
                    task.run();
                }
            }).expect("failed to spawn file read worker");
        }
        Self { queue, load, active, wake }
    }

    #[cfg(test)]
    pub(super) fn send(&self, foreground: bool, task: PoolTask) {
        self.send_for_path(Path::new(""), foreground, task);
    }

    pub(super) fn send_for_path(&self, path: &Path, foreground: bool, task: PoolTask) {
        self.active.fetch_add(1, Ordering::Relaxed);
        self.load.queued.fetch_add(1, Ordering::Relaxed);
        let guard = PoolJobGuard {
            _active: ActiveJobGuard(self.active.clone(), self.wake.clone()),
            load: self.load.clone(), running: false,
        };
        let task = QueuedTask { task, guard };
        let (lock, ready) = &*self.queue;
        let mut queue = lock.lock().expect("file read queue poisoned");
        if foreground { queue.foreground.push_back(task); }
        else { queue.background.push_back((path.to_path_buf(), task)); }
        drop(queue);
        ready.notify_one();
    }

    pub(super) fn prioritize(&self, path: &Path) {
        let (lock, ready) = &*self.queue;
        let mut queue = lock.lock().expect("file read queue poisoned");
        if let Some(index) = queue.background.iter().position(|(queued, _)| queued == path) {
            let (_, task) = queue.background.remove(index).unwrap();
            queue.foreground.push_back(task);
            drop(queue);
            ready.notify_one();
        }
    }
}

impl Drop for ReadPool {
    fn drop(&mut self) {
        let (lock, ready) = &*self.queue;
        let mut queue = lock.lock().expect("file read queue poisoned");
        queue.stopped = true;
        let foreground = std::mem::take(&mut queue.foreground);
        let background = std::mem::take(&mut queue.background);
        drop(queue);
        ready.notify_all();
        // Release captured snapshots and activity counters without holding
        // the queue mutex or waiting for potentially slow running disk reads.
        drop((foreground, background));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};
    const WAIT: Duration = Duration::from_secs(5);

    #[test]
    fn foreground_reads_jump_the_queue_without_starving_background_reads() {
        let active = Arc::new(AtomicUsize::new(0));
        let pool = ReadPool::new(1, active.clone(), UiWake::default());
        let (started, running) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        pool.send(false, Box::new(move || { started.send(()).unwrap(); resume.recv().unwrap(); }));
        running.recv_timeout(WAIT).unwrap();
        let (completed, results) = mpsc::channel();
        for (foreground, id) in [(false, 0), (false, 1)].into_iter().chain((2..14).map(|id| (true, id))) {
            let completed = completed.clone();
            pool.send(foreground, Box::new(move || completed.send((id, thread::current().id())).unwrap()));
        }
        assert_eq!(pool.load.running.load(Ordering::Relaxed), 1);
        assert_eq!(pool.load.queued.load(Ordering::Relaxed), 14);
        release.send(()).unwrap();
        let values: Vec<_> = (0..14).map(|_| results.recv_timeout(WAIT).unwrap()).collect();
        assert_eq!(values.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            (2..10).chain([0]).chain(10..14).chain([1]).collect::<Vec<_>>());
        assert!(values.iter().all(|(_, id)| *id == values[0].1), "reuse one worker");
    }

    #[test]
    fn dropping_read_pool_cancels_queued_work_and_does_not_wait_for_disk() {
        let active = Arc::new(AtomicUsize::new(0));
        let wake = UiWake::default();
        let pool = ReadPool::new(1, active.clone(), wake.clone());
        let load = pool.load.clone();
        let (started, running) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        pool.send(false, Box::new(move || { started.send(()).unwrap(); resume.recv().unwrap(); }));
        running.recv_timeout(WAIT).unwrap();
        for _ in 0..20 { pool.send(false, Box::new(|| panic!("cancelled read ran"))); }
        assert_eq!(active.load(Ordering::Relaxed), 21);
        drop(pool);
        assert_eq!(active.load(Ordering::Relaxed), 1);
        assert_eq!(load.queued.load(Ordering::Relaxed), 0);
        let observed = wake.generation();
        release.send(()).unwrap();
        wake.wait_for_change(observed, Some(WAIT));
        assert_eq!(active.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn blocked_reads_leave_cpu_work_available_and_limit_concurrent_file_jobs() {
        let mut workers = super::super::BackgroundWorkers::new();
        let (started, running) = mpsc::channel();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        for _ in 0..20 {
            let started = started.clone();
            let gate = gate.clone();
            workers.read_for_file("file-load", std::path::Path::new("fixture"), true, move || {
                let _ = started.send(());
                let guard = gate.0.lock().unwrap();
                drop(gate.1.wait_while(guard, |ready| !*ready).unwrap());
            });
        }
        let size = thread::available_parallelism().map_or(1, |n| super::super::save_pool_size(n.get()));
        for _ in 0..size { running.recv_timeout(WAIT).unwrap(); }
        let (cpu, done) = mpsc::channel();
        workers.pool_sender().send(Box::new(move || cpu.send(()).unwrap())).unwrap();
        // Always release readers before an assertion can fail.
        let cpu_result = done.recv_timeout(WAIT);
        let running_count = workers.read_pool.as_ref().unwrap().load.running.load(Ordering::Relaxed);
        let queued = workers.read_pool.as_ref().unwrap().load.queued.load(Ordering::Relaxed);
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        assert!(cpu_result.is_ok());
        assert_eq!(running_count, size);
        assert_eq!(queued, 20 - size);
    }
}
