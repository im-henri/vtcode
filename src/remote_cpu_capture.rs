//! Explicit two-snapshot capture. The client sleeps between RPCs; the node
//! starts no timer or profiling session and retains no capture allocations.
use super::*;
use crate::remote::observability::Snapshot;

#[derive(Default)]
pub(super) struct CaptureState {
    active: AtomicBool,
    wait: Mutex<()>,
    changed: Condvar,
}
impl CaptureState {
    pub(super) fn disconnected(&self) {
        let _guard = self.wait.lock().unwrap();
        self.changed.notify_all();
    }
}
struct Capture(Client);
impl Drop for Capture {
    fn drop(&mut self) {
        self.0
            .0
            .shared
            .capture
            .active
            .store(false, Ordering::Release);
        self.0.0.shared.wake.lock().unwrap().notify();
    }
}
impl Client {
    pub(crate) fn can_capture_cpu(&self) -> bool {
        self.connected() && self.0.shared.diagnostics.is_some()
    }
    pub(crate) fn capturing_cpu(&self) -> bool {
        self.0.shared.capture.active.load(Ordering::Acquire)
    }
    pub(crate) fn capture_cpu(&self) -> io::Result<bool> {
        self.capture_cpu_for(Duration::from_secs(10))
    }
    fn capture_cpu_for(&self, interval: Duration) -> io::Result<bool> {
        if !self.connected() {
            return Err(io::ErrorKind::NotConnected.into());
        }
        let trace = self
            .0
            .shared
            .diagnostics
            .clone()
            .ok_or_else(|| io::Error::other("Connection diagnostics are unavailable"))?;
        if self
            .0
            .shared
            .capture
            .active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(false);
        }
        let capture = Capture(self.clone());
        // Capture is a guard even if spawning fails, so the button cannot stick.
        std::thread::Builder::new().name("remote-cpu-capture".into()).spawn(move || {
            crate::worker_priority::lower_current_thread();
            let client = &capture.0;
            trace.event("Headless CPU", &format!("Capture started; measuring for {:.1}s. Keep working or leave the editor idle to investigate idle CPU. Two on-demand OS snapshots; no continuous profiler.", interval.as_secs_f64()));
            let result = (|| -> io::Result<()> {
                let before = client.performance_snapshot()?;
                let pending = client.0.shared.pending.lock().unwrap().len();
                trace.event("Headless CPU", &format!("Baseline received from headless PID {}; {pending} requests awaiting completion locally. CPU percentages use the remote sampling interval. Request IDs/phases in this log provide operation context; server queue/execution times are not measured.", before.pid));
                let guard = client.0.shared.capture.wait.lock().unwrap();
                let (_guard, _) = client.0.shared.capture.changed.wait_timeout_while(guard, interval, |_| client.connected()).unwrap();
                if !client.connected() { return Err(io::ErrorKind::NotConnected.into()); }
                drop(_guard);
                let after = client.performance_snapshot()?;
                for line in report(&before, &after) {
                    trace.event("Headless CPU", &line);
                }
                let pending = client.0.shared.pending.lock().unwrap().len();
                trace.event("Headless CPU", &format!("Capture complete; {pending} requests awaiting completion locally. Process/thread CPU is actual OS CPU time. Child processes (Git executables, LSPs, terminals, agents) are excluded from the process CPU total."));
                Ok(())
            })();
            if let Err(error) = result { trace.event("Headless CPU", &format!("Capture stopped: {error}")); }
        })?;
        Ok(true)
    }
    fn performance_snapshot(&self) -> io::Result<Snapshot> {
        match self.call_timeout(Command::PerformanceSnapshot, Duration::from_secs(15))? {
            Reply::PerformanceSnapshot(snapshot) => Ok(snapshot),
            _ => Err(io::Error::other("Invalid remote performance snapshot")),
        }
    }
}

fn millis(nanos: u64) -> f64 {
    nanos as f64 / 1e6
}
fn report(before: &Snapshot, after: &Snapshot) -> Vec<String> {
    let Some(interval) = after
        .clock_ns
        .checked_sub(before.clock_ns)
        .filter(|nanos| *nanos > 0)
    else {
        return vec!["Capture unavailable: the remote sample clock did not advance. No CPU percentage was inferred.".into()];
    };
    if before.pid != after.pid {
        return vec![
            "Capture unavailable: the headless process changed. Start a new capture.".into(),
        ];
    }
    let cpu = before
        .cpu_ns
        .zip(after.cpu_ns)
        .and_then(|(before, after)| after.checked_sub(before));
    let mut lines = vec![format!(
        "PID {} over {:.3}s: CPU {}; resident memory {}; 100% = one logical CPU. Local VTCode and SSH are separate processes.",
        after.pid,
        interval as f64 / 1e9,
        cpu.map_or_else(
            || "unavailable (missing/reset OS counters)".into(),
            |used| format!(
                "{:.3}% ({:.3} ms)",
                used as f64 / interval as f64 * 100.0,
                millis(used)
            )
        ),
        after
            .resident_bytes
            .map_or_else(|| "unavailable".into(), crate::process_usage::format_bytes),
    )];
    let mut matched = Vec::new();
    if let Some(resolution) = after.thread_resolution_ns {
        lines.push(format!("Thread counters have {:.3} ms resolution. Snapshots read threads sequentially; very short-lived threads may exist between snapshots. CPU total includes capture overhead.", millis(resolution)));
    }
    let mut new = 0;
    let mut previous: rustc_hash::FxHashMap<_, _> = before
        .threads
        .iter()
        .map(|thread| ((thread.id, thread.started), thread))
        .collect();
    for current in &after.threads {
        if let Some(previous) = previous.remove(&(current.id, current.started)) {
            if let Some(used) = current.cpu_ns.checked_sub(previous.cpu_ns) {
                matched.push((used, current));
            } else {
                new += 1;
            }
        } else {
            new += 1;
        }
    }
    let gone = previous.len();
    matched.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.id.cmp(&b.1.id)));
    for (used, thread) in &matched {
        lines.push(format!(
            "Thread {} [{}]: {:.3}% / {:.3} ms CPU; end state {}{}",
            thread.id,
            thread.name,
            *used as f64 / interval as f64 * 100.0,
            millis(*used),
            thread.state,
            if *used == 0 {
                " (below OS counter resolution or idle)"
            } else {
                ""
            }
        ));
    }
    if new > 0 || gone > 0 || !before.threads_complete || !after.threads_complete {
        lines.push(format!("Thread coverage: {} matched, {new} new/reset, {gone} ended/unreadable; snapshot lists {}. No CPU delta is invented for missing/reused thread IDs. The process total still includes exited threads. Per-thread counters are currently available on Linux (kernel clock-tick resolution).", matched.len(), if before.threads_complete && after.threads_complete { "complete" } else { "partial/unavailable" }));
    }
    lines
}
#[cfg(test)]
#[path = "remote_cpu_capture_tests.rs"]
mod tests;
