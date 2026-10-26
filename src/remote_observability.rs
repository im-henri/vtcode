//! Explicit OS counter snapshots. This code is reached only by a diagnostic
//! request. No timer, worker instrumentation, per-operation counters or startup
//! initialization. Ordinary headless operations do not call this module.
use serde::{Deserialize, Serialize};
use std::{sync::OnceLock, time::Instant};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    pub pid: u32,
    /// Process-local monotonic sample clock; not process uptime or wall time.
    pub clock_ns: u64,
    pub cpu_ns: Option<u64>,
    pub resident_bytes: Option<u64>,
    pub threads: Vec<Thread>,
    pub threads_complete: bool,
    pub thread_resolution_ns: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Thread {
    pub id: u64,
    pub started: u64,
    pub name: String,
    pub cpu_ns: u64,
    pub state: char,
}
pub(super) fn snapshot() -> Snapshot {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    let clock_ns = nanos(ORIGIN.get_or_init(Instant::now).elapsed());
    let counters = crate::process_usage::read_current_process();
    let (threads, threads_complete, thread_resolution_ns) = threads();
    Snapshot {
        pid: std::process::id(),
        clock_ns,
        cpu_ns: counters.cpu_time.map(nanos),
        resident_bytes: counters.resident_bytes,
        threads,
        threads_complete,
        thread_resolution_ns,
    }
}
fn nanos(duration: std::time::Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

#[cfg(target_os = "linux")]
fn threads() -> (Vec<Thread>, bool, Option<u64>) {
    use std::io::Read;
    static TICKS: OnceLock<u64> = OnceLock::new();
    // SAFETY: sysconf takes no pointer for this name.
    let ticks = *TICKS.get_or_init(|| unsafe { libc::sysconf(libc::_SC_CLK_TCK).max(0) as u64 });
    let resolution = (ticks > 0).then(|| 1_000_000_000 / ticks);
    let Ok(entries) = std::fs::read_dir("/proc/self/task") else {
        return (Vec::new(), false, resolution);
    };
    let mut result = Vec::new();
    let mut complete = true;
    for (index, entry) in entries.enumerate() {
        // Bound work and wire size even when tools have created many threads.
        if index >= 256 {
            complete = false;
            break;
        }
        let parsed = (|| {
            let entry = entry.ok()?;
            let id = entry.file_name().to_str()?.parse().ok()?;
            let mut file = std::fs::File::open(entry.path().join("stat")).ok()?;
            let mut bytes = [0u8; 4096];
            let mut length = 0;
            loop {
                match file.read(&mut bytes[length..]) {
                    Ok(0) => break,
                    Ok(read) => {
                        length += read;
                        if length == bytes.len() {
                            return None;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => return None,
                }
            }
            parse_thread(id, &bytes[..length], ticks)
        })();
        if let Some(thread) = parsed {
            result.push(thread);
        } else {
            complete = false;
        }
    }
    (result, complete, resolution)
}
#[cfg(not(target_os = "linux"))]
fn threads() -> (Vec<Thread>, bool, Option<u64>) {
    (Vec::new(), false, None)
}

#[cfg(any(target_os = "linux", test))]
fn parse_thread(id: u64, bytes: &[u8], ticks: u64) -> Option<Thread> {
    if ticks == 0 {
        return None;
    }
    let begin = bytes.iter().position(|byte| *byte == b'(')? + 1;
    let end = bytes.iter().rposition(|byte| *byte == b')')?;
    let name = String::from_utf8_lossy(bytes.get(begin..end)?)
        .chars()
        .take(64)
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let mut fields = std::str::from_utf8(bytes.get(end + 1..)?)
        .ok()?
        .split_ascii_whitespace();
    let state = fields.next()?.chars().next()?;
    let user: u64 = fields.nth(10)?.parse().ok()?; // field 14
    let system: u64 = fields.next()?.parse().ok()?; // field 15
    let started = fields.nth(6)?.parse().ok()?; // field 22
    let used = user.checked_add(system)?;
    let cpu_ns = (u128::from(used) * 1_000_000_000 / u128::from(ticks))
        .try_into()
        .ok()?;
    Some(Thread {
        id,
        started,
        name,
        cpu_ns,
        state,
    })
}

#[cfg(test)]
#[path = "remote_observability_tests.rs"]
mod tests;
