use super::*;

fn stat(name: &[u8], user: &str, system: &str, started: &str) -> Vec<u8> {
    let mut fields = vec!["0"; 24];
    fields[0] = "S"; // field 3
    fields[11] = user;
    fields[12] = system;
    fields[19] = started;
    [b"17 (".as_slice(), name, b") ", fields.join(" ").as_bytes()].concat()
}
#[test]
fn thread_counters_parse_spaces_parentheses_unicode_and_non_utf8_names() {
    for name in [
        b"headless-) name".as_slice(),
        "λ🦀".as_bytes(),
        b"bad-\xff\n",
    ] {
        let value = parse_thread(17, &stat(name, "23", "4", "987654"), 100).unwrap();
        assert_eq!(value.id, 17);
        assert_eq!(value.started, 987654);
        assert_eq!(value.cpu_ns, 270_000_000);
        assert_eq!(value.state, 'S');
        assert!(!value.name.contains('\n'));
    }
    assert_eq!(
        parse_thread(17, &stat(b"a", "1", "1", "0"), 3)
            .unwrap()
            .cpu_ns,
        666_666_666
    );
}
#[test]
fn malformed_or_overflowing_thread_counters_are_unavailable_instead_of_zero() {
    for bytes in [
        b"garbage".to_vec(),
        b"1 ) ( S".to_vec(),
        stat(b"a", "wrong", "0", "1"),
        stat(b"a", "18446744073709551615", "1", "1"),
        stat(b"a", "18446744073709551615", "0", "1"),
    ] {
        assert!(parse_thread(1, &bytes, 100).is_none());
    }
    assert!(parse_thread(1, &stat(b"a", "1", "2", "3"), 0).is_none());
}
#[test]
fn explicit_snapshot_reports_own_process_and_roundtrips_without_workspace_data() {
    let first = snapshot();
    let second = snapshot();
    assert_eq!(first.pid, std::process::id());
    assert!(second.clock_ns >= first.clock_ns);
    let bytes = serde_json::to_vec(&second).unwrap();
    let decoded: Snapshot = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(decoded.cpu_ns, second.cpu_ns);
    assert_eq!(decoded.threads.len(), second.threads.len());
    assert!(bytes.len() < 64 * 1024);
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    {
        assert!(second.cpu_ns.is_some());
        assert!(second.resident_bytes.is_some());
    }
    #[cfg(target_os = "linux")]
    {
        assert!(!second.threads.is_empty());
        assert!(
            second
                .thread_resolution_ns
                .is_some_and(|resolution| resolution > 0)
        );
        assert!(
            second
                .threads
                .iter()
                .any(|thread| thread.id == u64::from(std::process::id()))
        );
    }
}
