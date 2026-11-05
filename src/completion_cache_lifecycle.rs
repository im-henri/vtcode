//! Order cache restoration after older retiring writers without joining an
//! index thread from the UI. Gates contain only lifecycle metadata, not data.
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Default)]
struct State {
    next: u64,
    retiring: BTreeSet<u64>,
    #[cfg(test)]
    waiters: usize,
}
#[derive(Default)]
struct Gate {
    state: Mutex<State>,
    changed: Condvar,
}

pub(super) struct Session {
    id: u64,
    gate: Arc<Gate>,
    finished: AtomicBool,
}
impl Session {
    pub fn register(cache: &Path) -> Arc<Self> {
        static GATES: OnceLock<Mutex<HashMap<PathBuf, Weak<Gate>>>> = OnceLock::new();
        let gate = {
            let mut gates = GATES
                .get_or_init(Mutex::default)
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            gates.retain(|_, gate| gate.strong_count() != 0);
            if let Some(gate) = gates.get(cache).and_then(Weak::upgrade) {
                gate
            } else {
                let gate = Arc::new(Gate::default());
                gates.insert(cache.to_owned(), Arc::downgrade(&gate));
                gate
            }
        };
        let id = {
            let mut state = gate.state.lock().unwrap_or_else(|e| e.into_inner());
            state.next = state
                .next
                .checked_add(1)
                .expect("completion cache session overflow");
            state.next
        };
        Arc::new(Self {
            id,
            gate,
            finished: AtomicBool::new(false),
        })
    }

    pub fn stopping(&self) {
        let mut state = self.gate.state.lock().unwrap_or_else(|e| e.into_inner());
        if !self.finished.load(Ordering::Relaxed) {
            state.retiring.insert(self.id);
        }
    }

    fn wait(&self) {
        let mut state = self.gate.state.lock().unwrap_or_else(|e| e.into_inner());
        // Ignore this worker and younger sessions. Otherwise rapid consecutive
        // restarts could create a cycle between their shutdown barriers.
        while state.retiring.range(..self.id).next().is_some() {
            #[cfg(test)]
            {
                state.waiters += 1;
            }
            state = self
                .gate
                .changed
                .wait(state)
                .unwrap_or_else(|e| e.into_inner());
            #[cfg(test)]
            {
                state.waiters -= 1;
            }
        }
    }

    pub fn finish(&self) {
        let mut state = self.gate.state.lock().unwrap_or_else(|e| e.into_inner());
        self.finished.store(true, Ordering::Relaxed);
        if state.retiring.remove(&self.id) {
            self.gate.changed.notify_all();
        }
    }
}

/// A separate field guard also covers early returns and unwinding, without
/// imposing Drop/move restrictions on the worker's actual indexing state.
pub(super) struct Completion(Option<Arc<Session>>);
impl Completion {
    pub fn new(session: Option<Arc<Session>>) -> Self {
        Self(session)
    }
    pub fn wait_before_restore(&self) {
        if let Some(session) = &self.0 {
            session.wait();
        }
    }
}
impl Drop for Completion {
    fn drop(&mut self) {
        if let Some(session) = &self.0 {
            session.finish();
        }
    }
}

#[cfg(test)]
#[path = "completion_cache_lifecycle_tests.rs"]
mod tests;
