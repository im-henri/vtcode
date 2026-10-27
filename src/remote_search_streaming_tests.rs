use super::*;
use std::time::{Duration, Instant};

fn query(text: &str) -> Query {
    Query {
        stream: true,
        text: text.into(),
        case_sensitive: false,
        whole_word: false,
        regex: false,
        include: String::new(),
        exclude: String::new(),
        limit: 20_000,
        files_exclude: Default::default(),
        search_exclude: Default::default(),
        use_ignores: true,
        parent_ignores: false,
        global_ignores: false,
        follow_links: true,
        open_files: Vec::new(),
    }
}

fn normalized(hits: Vec<Hit>) -> Vec<(String, usize, usize, usize, String)> {
    let mut hits = hits
        .into_iter()
        .map(|hit| {
            (
                hit.path.display(),
                hit.line,
                hit.start,
                hit.end,
                hit.text.to_string(),
            )
        })
        .collect::<Vec<_>>();
    hits.sort();
    hits
}

#[test]
fn parallel_search_streaming_preserves_filters_encodings_open_files_and_match_ranges() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::create_dir(root.join("ignored")).unwrap();
    std::fs::write(root.join(".gitignore"), "ignored/\n").unwrap();
    std::fs::write(root.join("ignored/hidden.txt"), "needle").unwrap();
    for file in 0..48 {
        std::fs::write(
            root.join(format!("src/{file}.txt")),
            "é needle NEEDLE\r\nneedlework\r\n",
        )
        .unwrap();
    }
    std::fs::write(root.join("src/skip.txt"), "needle excluded").unwrap();
    std::fs::write(root.join("src/open.txt"), "needle stale disk").unwrap();
    std::fs::write(
        root.join("src/binary.txt"),
        format!("needle{}\0", "x".repeat(20_000)),
    )
    .unwrap();
    let utf16 = [0xff, 0xfe]
        .into_iter()
        .chain("猫 needle\r\n".encode_utf16().flat_map(u16::to_le_bytes))
        .collect::<Vec<_>>();
    std::fs::write(root.join("src/utf16.txt"), utf16).unwrap();
    for (text, regex, whole_word, include) in [
        ("needle", false, false, ""),
        ("needle", false, true, "./src/**, src/**"),
        (r"n\w+", true, false, "*.txt"),
        ("NEEDLE\\r?\\nneedlework", true, false, "./src/"),
    ] {
        let make_query = || {
            let mut current = query(text);
            current.regex = regex;
            current.whole_word = whole_word;
            current.include = include.into();
            current.exclude = "**/skip.txt".into();
            current
                .open_files
                .push(RemotePath::new(b"src/open.txt".to_vec()).unwrap());
            current
        };
        let cancel = CancellationToken::new();
        let Reply::Search { hits, truncated } = execute(root, make_query(), &cancel).unwrap()
        else {
            panic!()
        };
        assert!(!truncated);
        assert!(!hits.is_empty());
        let expected = normalized(hits);
        for workers in [2, 4] {
            let mut streamed = Vec::new();
            let Reply::Search { hits, truncated } =
                execute_with_workers(root, make_query(), &cancel, workers, |hits| {
                    streamed.extend(hits);
                    Ok(())
                })
                .unwrap()
            else {
                panic!()
            };
            streamed.extend(hits);
            assert!(!truncated);
            assert_eq!(
                normalized(streamed),
                expected,
                "{text}, {include}, {workers} workers"
            );
        }
    }
}

#[test]
fn parallel_search_streaming_enforces_one_global_limit_and_exact_truncation() {
    let dir = tempfile::tempdir().unwrap();
    for file in 0..16 {
        std::fs::write(
            dir.path().join(format!("{file}.txt")),
            "needle\n".repeat(32),
        )
        .unwrap();
    }
    for limit in [0, 1, 127, 128, 129, 511, 512, 513] {
        let mut current = query("needle");
        current.limit = limit;
        let cancel = CancellationToken::new();
        let mut streamed = Vec::new();
        let Reply::Search { hits, truncated } =
            execute_with_workers(dir.path(), current, &cancel, 4, |hits| {
                assert!(!hits.is_empty() && hits.len() <= 128);
                streamed.extend(hits);
                Ok(())
            })
            .unwrap()
        else {
            panic!()
        };
        streamed.extend(hits);
        assert_eq!(streamed.len(), limit.min(512), "limit={limit}");
        assert_eq!(truncated, limit < 512, "limit={limit}");
        let mut unique = normalized(streamed);
        let count = unique.len();
        unique.dedup();
        assert_eq!(unique.len(), count);
        assert!(
            !cancel.is_cancelled(),
            "reaching the limit must not cancel the caller"
        );
    }
}

#[test]
fn search_streaming_delivers_partial_results_before_completion_and_stops_on_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    for file in 0..128 {
        std::fs::write(dir.path().join(format!("{file}.txt")), "needle\n".repeat(4)).unwrap();
    }
    for fail_receiver in [false, true] {
        let cancel = CancellationToken::new();
        let mut delivered = 0;
        let result = execute_with_workers(dir.path(), query("needle"), &cancel, 4, |hits| {
            delivered += hits.len();
            if fail_receiver {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            cancel.cancel();
            Ok(())
        });
        assert!(delivered > 0 && delivered < 512);
        assert_eq!(
            result.unwrap_err().kind(),
            if fail_receiver {
                io::ErrorKind::BrokenPipe
            } else {
                io::ErrorKind::Interrupted
            }
        );
    }
}

#[test]
#[ignore = "manual headless search throughput and first-result benchmark"]
fn benchmark_headless_search_streaming() {
    let dir = tempfile::tempdir().unwrap();
    let body = format!(
        "{}needle\n",
        "ordinary source line without a match\n".repeat(2048)
    );
    for folder in 0..32 {
        std::fs::create_dir(dir.path().join(format!("src_{folder}"))).unwrap();
        for file in 0..32 {
            std::fs::write(dir.path().join(format!("src_{folder}/{file}.txt")), &body).unwrap();
        }
    }
    for workers in [1, 2, 4, 8] {
        let mut samples = Vec::new();
        for sample in 0..7 {
            let started = Instant::now();
            let mut first = None;
            let mut count = 0;
            let Reply::Search { hits, truncated } = execute_with_workers(
                dir.path(),
                query("needle"),
                &CancellationToken::new(),
                workers,
                |hits| {
                    first.get_or_insert_with(|| started.elapsed());
                    count += hits.len();
                    Ok(())
                },
            )
            .unwrap() else {
                panic!()
            };
            count += hits.len();
            assert_eq!(count, 1024);
            assert!(!truncated);
            if sample > 1 {
                samples.push((started.elapsed(), first.unwrap()));
            }
        }
        samples.sort_by_key(|sample| sample.0);
        let (total, first) = samples[samples.len() / 2];
        println!(
            "headless search: workers={workers}, 1024 files, {:.1} MiB, total={:.2} ms, first={:.2} ms, legacy first={:.2} ms",
            body.len() as f64 * 1024.0 / (1024.0 * 1024.0),
            total.as_secs_f64() * 1000.0,
            first.as_secs_f64() * 1000.0,
            total.as_secs_f64() * 1000.0
        );
        assert!(total < Duration::from_secs(30));
    }
}
