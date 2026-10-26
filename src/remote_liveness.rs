//! A silent SSH pipe is not an EOF. Probe the service independently of file
//! workers, without putting a whole-operation timeout on slow files/transfers.
//! Healthy traffic uses atomic timestamps; only an idle probe wakes this thread.
use super::*;
use std::sync::{Weak, atomic::AtomicU64};
use std::time::Instant;

const IDLE: Duration = Duration::from_secs(30);
const GRACE: Duration = Duration::from_secs(90);

pub(super) struct State {
    origin: Instant,
    progress_ms: AtomicU64,
    acknowledged: AtomicU64,
    outgoing: AtomicU64,
    park: Mutex<()>,
    ready: Condvar,
}
impl Default for State {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
            progress_ms: AtomicU64::new(0),
            acknowledged: AtomicU64::new(0),
            outgoing: AtomicU64::new(0),
            park: Mutex::new(()),
            ready: Condvar::new(),
        }
    }
}
impl State {
    pub(super) fn progress(&self) {
        self.progress_ms.fetch_max(
            self.origin.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            Ordering::Relaxed,
        );
    }
    pub(super) fn acknowledge(&self, nonce: u64) {
        self.acknowledged.store(nonce, Ordering::Release);
        self.wake();
    }
    #[cfg(all(test, windows))]
    pub(super) fn acknowledged_for_test(&self) -> u64 {
        self.acknowledged.load(Ordering::Acquire)
    }
    pub(super) fn take_outgoing(&self) -> Option<u64> {
        if self.outgoing.load(Ordering::Acquire) == 0 {
            return None;
        }
        let nonce = self.outgoing.swap(0, Ordering::AcqRel);
        (nonce != 0).then_some(nonce)
    }
    pub(super) fn wake(&self) {
        let _guard = self.park.lock().unwrap();
        self.ready.notify_all();
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Wait(Duration),
    Probe(u64),
    Failed,
}
struct Tracker {
    next: u64,
    probe: Option<(u64, Duration)>,
    idle: Duration,
    grace: Duration,
}
impl Default for Tracker {
    fn default() -> Self {
        Self {
            next: 0,
            probe: None,
            idle: IDLE,
            grace: GRACE,
        }
    }
}
impl Tracker {
    fn step(&mut self, now: Duration, progress: Duration, acknowledged: u64) -> Action {
        if self.probe.is_some_and(|(nonce, _)| nonce == acknowledged) {
            self.probe = None;
        }
        if let Some((_, sent)) = self.probe {
            let deadline = sent.max(progress).saturating_add(self.grace);
            return if now >= deadline {
                Action::Failed
            } else {
                Action::Wait(deadline - now)
            };
        }
        let due = progress.saturating_add(self.idle);
        if now < due {
            return Action::Wait(due - now);
        }
        self.next = self.next.wrapping_add(1).max(1);
        self.probe = Some((self.next, now));
        Action::Probe(self.next)
    }
}

/// Observe bytes before line assembly/decompression: a slow, progressing frame
/// must stay alive even if its final newline has not arrived yet.
pub(super) struct Reader<R> {
    pub(super) inner: R,
    pub(super) shared: Arc<Shared>,
}
impl<R: Read> Read for Reader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(bytes)?;
        if read != 0 {
            self.shared.liveness.progress();
        }
        Ok(read)
    }
}
impl<R: BufRead> BufRead for Reader<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        let bytes = self.inner.fill_buf()?;
        if !bytes.is_empty() {
            self.shared.liveness.progress();
        }
        Ok(bytes)
    }
    fn consume(&mut self, amount: usize) {
        self.inner.consume(amount);
    }
}

/// Short writes on very slow uploads are progress too. Waiting to count a
/// completed frame would put an accidental speed limit on healthy transfers.
pub(super) struct Writer<W> {
    pub(super) inner: W,
    pub(super) shared: Arc<Shared>,
}
impl<W: Write> Write for Writer<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(bytes)?;
        if written != 0 {
            self.shared.liveness.progress();
        }
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

pub(super) fn start(
    shared: Weak<Shared>,
    sender: Weak<mpsc::SyncSender<Option<Request>>>,
) -> io::Result<()> {
    spawn(shared, sender, Tracker::default())
}
#[cfg(test)]
pub(super) fn expire_silent_peer_for_test(
    shared: Weak<Shared>,
    sender: Weak<mpsc::SyncSender<Option<Request>>>,
) -> io::Result<()> {
    spawn(
        shared,
        sender,
        Tracker {
            idle: Duration::ZERO,
            grace: Duration::from_millis(20),
            ..Tracker::default()
        },
    )
}
fn spawn(
    shared: Weak<Shared>,
    sender: Weak<mpsc::SyncSender<Option<Request>>>,
    mut tracker: Tracker,
) -> io::Result<()> {
    std::thread::Builder::new().name("remote-liveness".into()).spawn(move || {
        crate::worker_priority::lower_current_thread();
        loop {
            let Some(shared) = shared.upgrade() else { return; };
            let guard = shared.liveness.park.lock().unwrap();
            if shared.closed.load(Ordering::Acquire) { return; }
            let acknowledged = shared.liveness.acknowledged.load(Ordering::Acquire);
            let progress = Duration::from_millis(shared.liveness.progress_ms.load(Ordering::Relaxed));
            match tracker.step(shared.liveness.origin.elapsed(), progress, acknowledged) {
                Action::Wait(wait) => {
                    drop(shared.liveness.ready.wait_timeout(guard, wait).unwrap());
                }
                Action::Probe(nonce) => {
                    shared.liveness.outgoing.store(nonce, Ordering::Release);
                    drop(guard);
                    if let Some(trace) = &shared.diagnostics {
                        trace.event("Connection check", "No recent transfer progress; checking remote service responsiveness");
                    }
                    let Some(sender) = sender.upgrade() else { return; };
                    // A full data queue already wakes the writer. Its control
                    // path drains this one coalesced probe between bulk frames.
                    let _ = sender.try_send(None);
                }
                Action::Failed => {
                    drop(guard);
                    shared.fail("The remote connection stopped responding. Your buffers remain local; verify any interrupted save before retrying.".into());
                    return;
                }
            }
        }
    }).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn secs(value: u64) -> Duration {
        Duration::from_secs(value)
    }

    #[test]
    fn idle_connection_probes_once_and_a_silent_peer_expires_without_polling() {
        let mut tracker = Tracker::default();
        assert_eq!(tracker.step(secs(0), secs(0), 0), Action::Wait(IDLE));
        assert_eq!(tracker.step(IDLE, secs(0), 0), Action::Probe(1));
        assert_eq!(tracker.step(IDLE, secs(0), 0), Action::Wait(GRACE));
        assert_eq!(tracker.step(IDLE + GRACE, secs(0), 0), Action::Failed);
    }

    #[test]
    fn healthy_idle_connections_repeat_at_the_idle_interval_and_ignore_stale_acks() {
        let mut tracker = Tracker::default();
        assert_eq!(tracker.step(secs(30), secs(0), 0), Action::Probe(1));
        assert_eq!(tracker.step(secs(31), secs(31), 1), Action::Wait(IDLE));
        assert_eq!(tracker.step(secs(61), secs(31), 1), Action::Probe(2));
        assert_eq!(tracker.step(secs(62), secs(31), 1), Action::Wait(secs(89)));
        assert_eq!(tracker.step(secs(63), secs(63), 2), Action::Wait(IDLE));
    }

    #[test]
    fn sustained_transfer_progress_has_no_file_size_or_whole_operation_deadline() {
        let mut tracker = Tracker::default();
        assert_eq!(tracker.step(secs(30), secs(0), 0), Action::Probe(1));
        for second in (60..=3600).step_by(30) {
            assert_eq!(
                tracker.step(secs(second), secs(second - 1), 0),
                Action::Wait(secs(89))
            );
        }
        assert_eq!(tracker.step(secs(3689), secs(3599), 0), Action::Failed);
    }
}
