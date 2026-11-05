//! Independent file opens share a bounded inbox. A stalled read must not pin
//! every later open to its worker, even after more requests have queued up.
//! Mutations and ticket-ordered resource controls stay on their serial actors.
use super::{Actor, ActorMessage, Cancellations, Response, Service};
use std::{
    collections::VecDeque,
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
};

const WORKERS: [&str; 2] = ["headless-reads", "headless-reads-2"];
const QUEUED_READS: usize = 8;
const RESERVED_OPENS: usize = 2;
const FOREGROUND_BURST: usize = 8;

#[derive(Default)]
pub(super) struct ReadLoad {
    pending: AtomicUsize,
}
impl ReadLoad {
    pub fn submitted(&self, command: &super::Command) -> bool {
        // Keep room for explicit opens even when reload notifications arrive
        // in a burst. Background capacity is no smaller than the old inbox.
        let limit = WORKERS.len()
            + QUEUED_READS
            + if foreground(command) {
                RESERVED_OPENS
            } else {
                0
            };
        self.pending
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |pending| {
                (pending < limit).then_some(pending + 1)
            })
            .is_ok()
    }
    pub fn rejected(&self) {
        self.pending.fetch_sub(1, Ordering::Relaxed);
    }
}

pub(super) struct ReadJob(Option<Arc<ReadLoad>>);
impl ReadJob {
    pub fn new(load: Option<Arc<ReadLoad>>) -> Self {
        Self(load)
    }
}
impl Drop for ReadJob {
    fn drop(&mut self) {
        if let Some(load) = &self.0 {
            load.pending.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

#[derive(Clone)]
struct SharedInbox(Arc<Mutex<Inbox>>);
struct Inbox {
    incoming: mpsc::Receiver<ActorMessage>,
    foreground: VecDeque<ActorMessage>,
    background: VecDeque<ActorMessage>,
    foreground_run: usize,
}
fn foreground(command: &super::Command) -> bool {
    // Conditional checks are background work. A first open or unconditional
    // reload remains interactive even when it uses the Reload operation.
    !matches!(command, super::Command::Reload { known: Some(_), .. })
}
impl Inbox {
    fn push(&mut self, message: ActorMessage) {
        if message
            .as_ref()
            .is_none_or(|(_, (request, _))| foreground(&request.command))
        {
            self.foreground.push_back(message);
        } else {
            self.background.push_back(message);
        }
    }
    fn pop(&mut self) -> Option<ActorMessage> {
        if !self.foreground.is_empty()
            && (self.foreground_run < FOREGROUND_BURST || self.background.is_empty())
        {
            self.foreground_run = self.foreground_run.saturating_add(1).min(FOREGROUND_BURST);
            self.foreground.pop_front()
        } else {
            self.foreground_run = 0;
            self.background.pop_front()
        }
    }
}
impl SharedInbox {
    fn new(incoming: mpsc::Receiver<ActorMessage>) -> Self {
        Self(Arc::new(Mutex::new(Inbox {
            incoming,
            foreground: VecDeque::new(),
            background: VecDeque::new(),
            foreground_run: 0,
        })))
    }
}
impl Iterator for SharedInbox {
    type Item = ActorMessage;
    fn next(&mut self) -> Option<Self::Item> {
        // Only receive is serialized. No filesystem work or reply publication
        // runs with this lock held; the next free worker takes the next item.
        let mut queue = self.0.lock().unwrap();
        if queue.foreground.is_empty() && queue.background.is_empty() {
            let message = queue.incoming.recv().ok()?;
            queue.push(message);
        }
        // Newly arrived opens also jump ahead of reloads buffered by a prior
        // receive. The admission counter bounds both buffers together.
        while let Ok(message) = queue.incoming.try_recv() {
            queue.push(message);
        }
        queue.pop()
    }
}

pub(super) struct ReadPool {
    sender: mpsc::SyncSender<ActorMessage>,
    requests: SharedInbox,
    services: Vec<Option<Service>>,
    workers: Vec<Actor>,
    load: Arc<ReadLoad>,
    output: mpsc::SyncSender<Response>,
    cancels: Cancellations,
}
impl ReadPool {
    pub fn new(
        mut service: impl FnMut() -> Service,
        output: mpsc::SyncSender<Response>,
        cancels: Cancellations,
    ) -> Self {
        // Admission bounds running and queued jobs together. The inbox must
        // also accommodate that whole bound before either worker is scheduled,
        // so reloads cannot consume the reserved open slots in that interval.
        let (sender, requests) = mpsc::sync_channel(WORKERS.len() + QUEUED_READS + RESERVED_OPENS);
        Self {
            sender,
            requests: SharedInbox::new(requests),
            services: WORKERS.iter().map(|_| Some(service())).collect(),
            workers: Vec::new(),
            load: Default::default(),
            output,
            cancels,
        }
    }

    pub fn get(&mut self) -> io::Result<&Actor> {
        // Start nothing until the first read, and reuse one worker for a
        // sequential session. A second starts only under concurrent demand.
        if self.workers.is_empty()
            || (self.workers.len() < self.services.len()
                && self.load.pending.load(Ordering::Relaxed) != 0)
        {
            let index = self.workers.len();
            let worker = super::actor_with_inbox(
                WORKERS[index],
                self.services[index].take().expect("unstarted read worker"),
                self.output.clone(),
                self.cancels.clone(),
                self.sender.clone(),
                self.requests.clone(),
                Some(self.load.clone()),
            )?;
            self.workers.push(worker);
        }
        // One submitter owns request tickets. Both read-only workers consume
        // this same inbox; private per-worker queues would reintroduce stalls.
        Ok(&self.workers[0])
    }
}

#[cfg(test)]
#[path = "remote_read_pool_tests.rs"]
mod tests;
