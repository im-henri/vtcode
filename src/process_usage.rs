//! On-demand, current-process counters. No worker, process enumeration or
//! polling thread. The UI owns the single, ten-second deadline and a bounded
//! history of readings; drawing charts never queries the operating system.
use std::time::{Duration, Instant};

pub(crate) const SAMPLE_INTERVAL: Duration = Duration::from_secs(10);
const HISTORY_LIMIT: usize = 30;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Sample {
    pub at: Instant,
    pub cpu_percent: Option<f64>,
    pub resident_bytes: Option<u64>,
    pub averaged_over: Option<Duration>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Counters {
    pub cpu_time: Option<Duration>,
    pub resident_bytes: Option<u64>,
}

#[derive(Default)]
pub(crate) struct ProcessUsage {
    last_attempt: Option<Instant>,
    baseline: Option<(Instant, Duration)>,
    pub cpu_percent: Option<f64>,
    pub resident_bytes: Option<u64>,
    pub averaged_over: Option<Duration>,
    cpu_label: String,
    memory_label: String,
    history: [Option<Sample>; HISTORY_LIMIT],
    next_sample: usize,
    history_len: usize,
    pub peak_resident_bytes: Option<u64>,
    #[cfg(test)]
    pub samples: usize,
}

impl ProcessUsage {
    /// Chronological readings, including unavailable values and gaps. No
    /// samples are invented while the view is hidden or the app is unfocused.
    pub(crate) fn history(&self) -> impl DoubleEndedIterator<Item = Sample> + '_ {
        let first = if self.history_len == HISTORY_LIMIT {
            self.next_sample
        } else {
            0
        };
        (0..self.history_len).filter_map(move |index| self.history[(first + index) % HISTORY_LIMIT])
    }

    pub(crate) fn cpu_label(&self) -> &str {
        if self.cpu_label.is_empty() {
            "—"
        } else {
            &self.cpu_label
        }
    }

    pub(crate) fn memory_label(&self) -> &str {
        if self.memory_label.is_empty() {
            "—"
        } else {
            &self.memory_label
        }
    }

    pub(crate) fn sampled_at(&self) -> Option<Instant> {
        self.last_attempt
    }

    pub(crate) fn cpu_is_cached(&self) -> bool {
        self.cpu_percent.is_some() && self.baseline.is_none()
    }

    pub(crate) fn suspend(&mut self) {
        // Do not average a long, unobserved interval into the next CPU reading.
        // Keep the deadline across close/reopen, so toggling cannot force reads.
        self.baseline = None;
    }

    pub(crate) fn next_wait(&self, visible: bool, now: Instant) -> Option<Duration> {
        visible.then(|| {
            self.last_attempt.map_or(Duration::ZERO, |last| {
                SAMPLE_INTERVAL.saturating_sub(now.saturating_duration_since(last))
            })
        })
    }

    pub(crate) fn refresh(&mut self, visible: bool, now: Instant) -> bool {
        self.refresh_with(visible, now, read_current_process)
    }

    pub(crate) fn refresh_with(
        &mut self,
        visible: bool,
        now: Instant,
        read: impl FnOnce() -> Counters,
    ) -> bool {
        if !visible {
            self.suspend();
            return false;
        }
        if self.next_wait(true, now) != Some(Duration::ZERO) {
            return false;
        }
        // Failed/unsupported reads get the same backoff as successful reads.
        self.last_attempt = Some(now);
        let counters = read();
        self.cpu_percent = None;
        self.averaged_over = None;
        if let (Some((previous_at, previous_cpu)), Some(cpu)) = (self.baseline, counters.cpu_time)
            && let Some(elapsed) = now
                .checked_duration_since(previous_at)
                .filter(|time| !time.is_zero())
            && let Some(used) = cpu.checked_sub(previous_cpu)
        {
            // Like top: 100% means one logical CPU, not the whole machine.
            self.cpu_percent = Some(used.as_secs_f64() / elapsed.as_secs_f64() * 100.0);
            self.averaged_over = Some(elapsed);
        }
        self.baseline = counters.cpu_time.map(|cpu| (now, cpu));
        self.resident_bytes = counters.resident_bytes;
        if let Some(bytes) = counters.resident_bytes {
            self.peak_resident_bytes = Some(self.peak_resident_bytes.unwrap_or(0).max(bytes));
        }
        self.history[self.next_sample] = Some(Sample {
            at: now,
            cpu_percent: self.cpu_percent,
            resident_bytes: self.resident_bytes,
            averaged_over: self.averaged_over,
        });
        self.next_sample = (self.next_sample + 1) % HISTORY_LIMIT;
        self.history_len = (self.history_len + 1).min(HISTORY_LIMIT);
        self.cpu_label = self.cpu_percent.map_or_else(
            || {
                if counters.cpu_time.is_some() {
                    "Measuring…"
                } else {
                    "Unavailable"
                }
                .into()
            },
            |percent| format!("{percent:.1}%"),
        );
        self.memory_label = counters
            .resident_bytes
            .map_or_else(|| "Unavailable".into(), format_bytes);
        #[cfg(test)]
        {
            self.samples += 1;
        }
        true
    }
}

pub(crate) fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.2} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    }
}

#[cfg(windows)]
pub(crate) fn read_current_process() -> Counters {
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::{
            ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
            Threading::{GetCurrentProcess, GetProcessTimes},
        },
    };
    fn cpu_duration(time: FILETIME) -> Duration {
        let ticks = (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
        Duration::new(ticks / 10_000_000, ((ticks % 10_000_000) * 100) as u32)
    }
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let mut memory = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    // SAFETY: the current-process pseudo-handle is always valid and must not be
    // closed. Both calls write only to correctly sized, initialized OS structs.
    unsafe {
        let process = GetCurrentProcess();
        let cpu_time =
            (GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) != 0)
                .then(|| cpu_duration(kernel).checked_add(cpu_duration(user)))
                .flatten();
        let size = memory.cb;
        let resident_bytes = (K32GetProcessMemoryInfo(process, &mut memory, size) != 0)
            .then_some(memory.WorkingSetSize as u64);
        Counters {
            cpu_time,
            resident_bytes,
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn read_current_process() -> Counters {
    use std::{io::Read, sync::OnceLock};
    static UNITS: OnceLock<(u64, u64)> = OnceLock::new();
    let &(ticks, page_size) = UNITS.get_or_init(|| {
        // SAFETY: these sysconf names take no pointers and are process-independent.
        unsafe {
            (
                libc::sysconf(libc::_SC_CLK_TCK).max(0) as u64,
                libc::sysconf(libc::_SC_PAGESIZE).max(0) as u64,
            )
        }
    });
    // This is a bounded read of a kernel pseudo-file, not workspace/storage I/O.
    let Ok(mut file) = std::fs::File::open("/proc/self/stat") else {
        return Counters::default();
    };
    let mut bytes = [0u8; 4096];
    let mut length = 0;
    loop {
        match file.read(&mut bytes[length..]) {
            Ok(0) => break,
            Ok(read) => {
                length += read;
                if length == bytes.len() {
                    return Counters::default();
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Counters::default(),
        }
    }
    parse_linux_stat(&bytes[..length], ticks, page_size).unwrap_or_default()
}

#[cfg(any(target_os = "linux", test))]
fn parse_linux_stat(bytes: &[u8], ticks_per_second: u64, page_size: u64) -> Option<Counters> {
    // comm can contain spaces, closing parentheses and non-UTF-8 bytes. Only
    // the numeric tail needs UTF-8; use the final ')' to locate field 3.
    let end = bytes.iter().rposition(|&byte| byte == b')')?;
    let mut fields = std::str::from_utf8(bytes.get(end + 1..)?)
        .ok()?
        .split_ascii_whitespace();
    let user: u64 = fields.nth(11)?.parse().ok()?; // field 14: utime
    let system: u64 = fields.next()?.parse().ok()?; // field 15: stime
    let resident_pages: u64 = fields.nth(8)?.parse().ok()?; // field 24: rss
    let ticks = user.checked_add(system)?;
    let cpu_time = if ticks_per_second > 0 {
        Some(Duration::new(
            ticks / ticks_per_second,
            ((u128::from(ticks % ticks_per_second) * 1_000_000_000) / u128::from(ticks_per_second))
                as u32,
        ))
    } else {
        None
    };
    let resident_bytes = (page_size > 0)
        .then(|| resident_pages.checked_mul(page_size))
        .flatten();
    Some(Counters {
        cpu_time,
        resident_bytes,
    })
}

#[cfg(target_os = "macos")]
pub(crate) fn read_current_process() -> Counters {
    // SAFETY: proc_taskinfo is a plain C counter structure; proc_pidinfo writes
    // at most its supplied size. Only our own PID is queried, with no scanning.
    unsafe {
        let mut info: libc::proc_taskinfo = std::mem::zeroed();
        let size = std::mem::size_of_val(&info) as libc::c_int;
        if libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDTASKINFO,
            0,
            (&mut info as *mut libc::proc_taskinfo).cast(),
            size,
        ) != size
        {
            return Counters::default();
        }
        Counters {
            cpu_time: info
                .pti_total_user
                .checked_add(info.pti_total_system)
                .map(Duration::from_nanos),
            resident_bytes: Some(info.pti_resident_size),
        }
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub(crate) fn read_current_process() -> Counters {
    Counters::default()
}

#[cfg(test)]
#[path = "process_usage_tests.rs"]
mod tests;
