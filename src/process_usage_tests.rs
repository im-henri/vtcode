use super::*;
use std::cell::Cell;

fn counters(cpu_seconds: u64, mib: u64) -> Counters {
    Counters {
        cpu_time: Some(Duration::from_secs(cpu_seconds)),
        resident_bytes: Some(mib * 1024 * 1024),
    }
}

#[test]
fn resource_history_is_bounded_chronological_and_preserves_missing_samples() {
    let mut usage = ProcessUsage::default();
    let start = Instant::now();
    for index in 0..100u32 {
        usage.refresh_with(true, start + SAMPLE_INTERVAL * index, || {
            counters(index as u64, 100 - index as u64)
        });
    }
    let history: Vec<_> = usage.history().collect();
    assert_eq!(history.len(), 30);
    assert_eq!(history.first().unwrap().at, start + SAMPLE_INTERVAL * 70);
    assert_eq!(history.last().unwrap().resident_bytes, Some(1024 * 1024));
    assert_eq!(usage.peak_resident_bytes, Some(100 * 1024 * 1024));
    let resumed = start + SAMPLE_INTERVAL * 110;
    usage.suspend();
    usage.refresh_with(true, resumed, || counters(120, 50));
    assert_eq!(usage.history().next_back().unwrap().cpu_percent, None);
    usage.refresh_with(true, resumed + SAMPLE_INTERVAL, Counters::default);
    let unavailable = usage.history().next_back().unwrap();
    assert_eq!(unavailable.cpu_percent, None);
    assert_eq!(unavailable.resident_bytes, None);
    assert_eq!(usage.history().count(), 30);
    assert_eq!(usage.peak_resident_bytes, Some(100 * 1024 * 1024));
}

#[test]
fn cpu_is_interval_usage_and_memory_is_the_current_resident_set() {
    let now = Instant::now();
    let mut usage = ProcessUsage::default();
    assert_eq!(usage.next_wait(false, now), None);
    assert!(usage.refresh_with(true, now, || counters(100, 80)));
    assert_eq!(usage.cpu_percent, None);
    assert_eq!(usage.cpu_label(), "Measuring…");
    assert_eq!(usage.memory_label(), "80.0 MiB");
    assert!(usage.refresh_with(true, now + SAMPLE_INTERVAL, || counters(102, 60)));
    assert_eq!(usage.cpu_percent, Some(20.0));
    assert_eq!(usage.cpu_label(), "20.0%");
    assert_eq!(usage.memory_label(), "60.0 MiB");
    assert_eq!(usage.averaged_over, Some(SAMPLE_INTERVAL));
    assert!(!usage.cpu_is_cached());
    assert_eq!(
        usage.history().next_back().unwrap().averaged_over,
        Some(SAMPLE_INTERVAL)
    );
    assert!(usage.refresh_with(true, now + SAMPLE_INTERVAL * 2, || counters(122, 2048)));
    assert_eq!(
        usage.cpu_percent,
        Some(200.0),
        "multiple cores must not clamp at 100%"
    );
    assert_eq!(usage.memory_label(), "2.00 GiB");
}

#[test]
fn returning_before_deadline_keeps_old_cpu_reading_explicitly_cached() {
    let now = Instant::now();
    let mut usage = ProcessUsage::default();
    usage.refresh_with(true, now, || counters(0, 64));
    usage.refresh_with(true, now + SAMPLE_INTERVAL, || counters(2, 64));
    usage.suspend();
    assert!(usage.cpu_is_cached());
    assert!(!usage.refresh_with(
        true,
        now + SAMPLE_INTERVAL + Duration::from_secs(1),
        || panic!("do not sample early")
    ));
    assert!(usage.cpu_is_cached());
    usage.refresh_with(true, now + SAMPLE_INTERVAL * 2, || counters(4, 64));
    assert!(!usage.cpu_is_cached());
    assert_eq!(usage.cpu_label(), "Measuring…");
    usage.refresh_with(true, now + SAMPLE_INTERVAL * 3, || counters(6, 64));
    assert_eq!(usage.cpu_percent, Some(20.0));
}

#[test]
fn fast_input_redraws_and_reopening_cannot_force_early_samples() {
    let start = Instant::now();
    let calls = Cell::new(0);
    let mut usage = ProcessUsage::default();
    for milliseconds in 0..30_000 {
        let now = start + Duration::from_millis(milliseconds);
        if milliseconds % 23 == 0 {
            usage.suspend();
        }
        usage.refresh_with(true, now, || {
            calls.set(calls.get() + 1);
            counters(20, 64)
        });
    }
    assert_eq!(calls.get(), 3);
    assert_eq!(usage.samples, 3);
    assert_eq!(
        usage.next_wait(true, start + Duration::from_millis(29_999)),
        Some(Duration::from_millis(1))
    );
    assert!(!usage.refresh_with(true, start, || panic!(
        "backward time must not force a read"
    )));
}

#[test]
fn hidden_views_do_not_read_or_schedule_and_resume_without_catchup() {
    let now = Instant::now();
    let mut usage = ProcessUsage::default();
    usage.refresh_with(true, now, || counters(50, 70));
    for seconds in [1, 10, 20, 100, 10_000] {
        let time = now + Duration::from_secs(seconds);
        assert_eq!(usage.next_wait(false, time), None);
        assert!(!usage.refresh_with(false, time, || panic!("hidden views must not query the OS")));
    }
    let resumed = now + Duration::from_secs(20_000);
    assert!(usage.refresh_with(true, resumed, || counters(9_000, 40)));
    assert_eq!(usage.samples, 2, "one fresh sample, no catch-up loop");
    assert_eq!(
        usage.cpu_percent, None,
        "hidden activity is not a fresh CPU average"
    );
    assert_eq!(usage.memory_label(), "40.0 MiB");
    usage.refresh_with(true, resumed + SAMPLE_INTERVAL, || counters(9_001, 40));
    assert_eq!(usage.cpu_percent, Some(10.0));
}

#[test]
fn failures_counter_resets_and_partial_results_do_not_fabricate_zero_usage_or_busy_loop() {
    let now = Instant::now();
    let mut usage = ProcessUsage::default();
    usage.refresh_with(true, now, Counters::default);
    assert_eq!(usage.cpu_label(), "Unavailable");
    assert_eq!(usage.memory_label(), "Unavailable");
    assert_eq!(usage.next_wait(true, now), Some(SAMPLE_INTERVAL));
    assert!(
        !usage.refresh_with(true, now + Duration::from_secs(9), || panic!(
            "failure backoff"
        ))
    );
    usage.refresh_with(true, now + SAMPLE_INTERVAL, || counters(100, 64));
    assert_eq!(usage.cpu_percent, None);
    usage.refresh_with(true, now + SAMPLE_INTERVAL * 2, || counters(90, 60));
    assert_eq!(
        usage.cpu_percent, None,
        "counter regression resets the baseline"
    );
    usage.refresh_with(true, now + SAMPLE_INTERVAL * 3, || Counters {
        cpu_time: Some(Duration::from_secs(91)),
        resident_bytes: None,
    });
    assert_eq!(usage.cpu_percent, Some(10.0));
    assert_eq!(usage.memory_label(), "Unavailable");
    usage.refresh_with(true, now + SAMPLE_INTERVAL * 4, || Counters {
        cpu_time: None,
        resident_bytes: Some(20 * 1024 * 1024),
    });
    assert_eq!(usage.cpu_label(), "Unavailable");
    assert_eq!(usage.memory_label(), "20.0 MiB");
    usage.refresh_with(true, now + SAMPLE_INTERVAL * 5, || counters(92, 64));
    assert_eq!(
        usage.cpu_percent, None,
        "failed CPU samples break averaging continuity"
    );
}

fn linux_stat(comm: &[u8], user: &str, system: &str, rss: &str) -> Vec<u8> {
    let mut fields = vec!["0"; 22];
    fields[0] = "S";
    fields[11] = user;
    fields[12] = system;
    // Deliberately large child times must not become process CPU usage.
    fields[13] = "999999";
    fields[14] = "999999";
    fields[21] = rss;
    let mut bytes = b"123 (".to_vec();
    bytes.extend_from_slice(comm);
    bytes.extend_from_slice(format!(") {}\n", fields.join(" ")).as_bytes());
    bytes
}

#[test]
fn linux_counter_parser_handles_complex_names_and_uses_correct_units() {
    for name in [b"vtcode".as_slice(), b"vt code (a) )", b"invalid\xff\xfe"] {
        let sample = parse_linux_stat(&linux_stat(name, "123", "77", "512"), 100, 4096).unwrap();
        assert_eq!(sample.cpu_time, Some(Duration::from_secs(2)));
        assert_eq!(sample.resident_bytes, Some(2 * 1024 * 1024));
    }
    let sample = parse_linux_stat(&linux_stat(b"app", "100", "1", "1"), 100, 16_384).unwrap();
    assert_eq!(sample.cpu_time, Some(Duration::from_millis(1010)));
    assert_eq!(sample.resident_bytes, Some(16_384));
}

#[test]
fn linux_counter_parser_rejects_truncation_overflow_and_bad_numbers() {
    for bytes in [
        b"".to_vec(),
        b"123 (name) S 1 2".to_vec(),
        linux_stat(b"app", "invalid", "1", "1"),
        linux_stat(b"app", "18446744073709551615", "1", "1"),
        linux_stat(b"app", "0", "0", "-1"),
    ] {
        assert!(parse_linux_stat(&bytes, 100, 4096).is_none());
    }
    let sample = parse_linux_stat(&linux_stat(b"app", "1", "2", "3"), 0, 0).unwrap();
    assert_eq!(sample.cpu_time, None);
    assert_eq!(sample.resident_bytes, None);
    let sample = parse_linux_stat(
        &linux_stat(b"app", "1", "2", "18446744073709551615"),
        100,
        4096,
    )
    .unwrap();
    assert_eq!(sample.resident_bytes, None);
}

#[test]
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
fn native_current_process_counters_are_available_and_monotonic() {
    let first = read_current_process();
    let second = read_current_process();
    assert!(first.resident_bytes.is_some_and(|bytes| bytes > 0));
    assert!(second.resident_bytes.is_some_and(|bytes| bytes > 0));
    assert!(second.cpu_time.unwrap() >= first.cpu_time.unwrap());
}

#[test]
#[ignore = "manual native process-counter overhead benchmark"]
fn benchmark_process_usage_sample_cost() {
    let mut samples = Vec::with_capacity(101);
    for _ in 0..101 {
        let start = Instant::now();
        std::hint::black_box(read_current_process());
        samples.push(start.elapsed());
    }
    samples.sort_unstable();
    eprintln!(
        "current-process CPU + resident memory: median {:?}, p95 {:?}; production interval {:?}",
        samples[50], samples[95], SAMPLE_INTERVAL
    );
}
