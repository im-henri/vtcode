//! Sleeping worker queue with bounded preference for latency-sensitive jobs.
//! Jobs are FIFO within each class; a continuous interactive stream cannot
//! starve analysis. No user code or captured job destructor runs under the lock.
use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex},
};

const INTERACTIVE_BURST: usize = 4;

#[derive(Clone, Copy, Default)]
pub(super) enum Priority {
    Interactive,
    #[default]
    Analysis,
}

struct Queue<T> {
    interactive: VecDeque<T>,
    analysis: VecDeque<T>,
    interactive_run: usize,
    producers_open: bool,
    consumers_open: bool,
}

impl<T> Queue<T> {
    fn pop(&mut self) -> Option<T> {
        if !self.interactive.is_empty()
            && (self.interactive_run < INTERACTIVE_BURST || self.analysis.is_empty())
        {
            self.interactive_run = (self.interactive_run + 1).min(INTERACTIVE_BURST);
            self.interactive.pop_front()
        } else {
            self.interactive_run = 0;
            self.analysis.pop_front()
        }
    }
}

struct Shared<T> {
    queue: Mutex<Queue<T>>,
    ready: Condvar,
}

struct Producers<T>(Arc<Shared<T>>);
struct Consumers<T>(Arc<Shared<T>>);
pub(super) struct Sender<T>(Arc<Producers<T>>);
pub(super) struct Receiver<T>(Arc<Consumers<T>>);

pub(super) fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        queue: Mutex::new(Queue {
            interactive: VecDeque::new(),
            analysis: VecDeque::new(),
            interactive_run: 0,
            producers_open: true,
            consumers_open: true,
        }),
        ready: Condvar::new(),
    });
    (
        Sender(Arc::new(Producers(shared.clone()))),
        Receiver(Arc::new(Consumers(shared))),
    )
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Sender<T> {
    pub(super) fn send(&self, item: T, priority: Priority) -> Result<(), T> {
        let shared = &self.0.0;
        let mut queue = shared
            .queue
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !queue.consumers_open {
            return Err(item);
        }
        match priority {
            Priority::Interactive => queue.interactive.push_back(item),
            Priority::Analysis => queue.analysis.push_back(item),
        }
        drop(queue);
        shared.ready.notify_one();
        Ok(())
    }
}

impl<T> Receiver<T> {
    pub(super) fn recv(&self) -> Option<T> {
        let shared = &self.0.0;
        let mut queue = shared
            .queue
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        loop {
            if let Some(item) = queue.pop() {
                return Some(item);
            }
            if !queue.producers_open {
                return None;
            }
            queue = shared
                .ready
                .wait(queue)
                .unwrap_or_else(|error| error.into_inner());
        }
    }
}

impl<T> Drop for Producers<T> {
    fn drop(&mut self) {
        self.0
            .queue
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .producers_open = false;
        self.0.ready.notify_all();
    }
}

impl<T> Drop for Consumers<T> {
    fn drop(&mut self) {
        let mut queue = self
            .0
            .queue
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        queue.consumers_open = false;
        let pending = (
            std::mem::take(&mut queue.interactive),
            std::mem::take(&mut queue.analysis),
        );
        drop(queue);
        drop(pending);
    }
}

#[cfg(test)]
#[path = "background_queue_tests.rs"]
mod tests;
