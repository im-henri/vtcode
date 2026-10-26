//! One running and one newest pending request, on an existing worker pool.
//! Idle slots hold no worker, timer, or queue entry. Each run returns to the
//! pool before its successor is submitted, preserving the pool's fairness.
use super::{BackgroundTaskSender, CancellationToken};
use std::sync::{Arc, Mutex};

#[cfg(test)]
#[path = "background_latest_tests.rs"]
mod tests;

pub(crate) struct Job {
    pub cancel: CancellationToken,
    pub run: Box<dyn FnOnce() + Send>,
}

#[derive(Default)]
struct Slot {
    pending: Option<Job>,
    running: Option<CancellationToken>,
    scheduled: bool,
    closed: bool,
}

#[derive(Default)]
pub(crate) struct LatestWork {
    slot: Arc<Mutex<Slot>>,
}

impl LatestWork {
    pub fn replace(&self, sender: BackgroundTaskSender, job: Job) {
        let (obsolete, running, schedule) = {
            let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
            let old = slot.pending.replace(job);
            let schedule = !slot.scheduled;
            slot.scheduled = true;
            (old, slot.running.clone(), schedule)
        };
        // Captures and activity tickets may have nontrivial destructors.
        // Neither cancellation nor destruction happens under the slot lock.
        if let Some(old) = obsolete {
            old.cancel.cancel();
        }
        if let Some(running) = running {
            running.cancel();
        }
        if schedule {
            schedule_run(Arc::clone(&self.slot), sender);
        }
    }

    pub fn cancel(&self) {
        let (pending, running) = {
            let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
            (slot.pending.take(), slot.running.clone())
        };
        if let Some(job) = pending {
            job.cancel.cancel();
        }
        if let Some(running) = running {
            running.cancel();
        }
    }
}

fn schedule_run(slot: Arc<Mutex<Slot>>, sender: BackgroundTaskSender) {
    let state = Arc::clone(&slot);
    let next_sender = sender.clone();
    if sender
        .send(Box::new(move || run(state, next_sender)))
        .is_err()
    {
        let pending = {
            let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
            slot.scheduled = false;
            slot.pending.take()
        };
        if let Some(job) = pending {
            job.cancel.cancel();
        }
    }
}

fn run(state: Arc<Mutex<Slot>>, sender: BackgroundTaskSender) {
    let job = {
        let mut slot = state.lock().unwrap_or_else(|e| e.into_inner());
        let job = slot.pending.take();
        slot.running = job.as_ref().map(|job| job.cancel.clone());
        job
    };
    if let Some(job) = job
        && !job.cancel.is_cancelled()
    {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job.run)).is_err() {
            job.cancel.cancel();
        }
    }
    let again = {
        let mut slot = state.lock().unwrap_or_else(|e| e.into_inner());
        slot.running = None;
        let again = !slot.closed && slot.pending.is_some();
        if !again {
            slot.scheduled = false;
        }
        again
    };
    if again {
        schedule_run(state, sender);
    }
}

impl Drop for LatestWork {
    fn drop(&mut self) {
        let (pending, running) = {
            let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
            slot.closed = true;
            (slot.pending.take(), slot.running.take())
        };
        if let Some(job) = pending {
            job.cancel.cancel();
        }
        if let Some(running) = running {
            running.cancel();
        }
    }
}
