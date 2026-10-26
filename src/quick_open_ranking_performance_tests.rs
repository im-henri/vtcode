use super::*;
use std::cell::Cell;
use std::sync::Arc;

fn entries(paths: &[&str]) -> Arc<Vec<QuickOpenEntry>> {
    Arc::new(
        paths
            .iter()
            .map(|relative| {
                let relative = relative.to_lowercase().replace('\\', "/");
                let name = relative.rsplit('/').next().unwrap().to_owned();
                QuickOpenEntry {
                    path: PathBuf::from(relative.as_str()),
                    name,
                    relative,
                }
            })
            .collect(),
    )
}

#[test]
fn repeated_filename_scores_match_full_ranking_past_cache_limit() {
    let source = Arc::new((0..8_500).map(|index| {
        let name = match index % 7 {
            0 => "index.ts".to_owned(),
            1 => "café_界.rs".to_owned(),
            _ => format!("file-{index}.rs"),
        };
        let relative = if index % 11 == 0 { name.clone() }
            else { format!("packages/package-{index}/{name}") };
        QuickOpenEntry { path: PathBuf::from(&relative), name, relative }
    }).collect::<Vec<_>>());
    let mut cache = QuickOpenRankingCache::default();
    for query in ["i", "index", "rs", "café", "界", "absent", "f", "file", "file-8", "packages/p"] {
        verify(&mut cache, &source, query);
    }
}

#[test]
fn adaptive_filename_scores_disable_unique_samples_and_bound_repeated_samples() {
    let unique: Vec<_> = (0..QUICK_OPEN_NAME_SCORE_SAMPLE).map(|index| format!("unique-{index}.rs")).collect();
    let calculated = Cell::new(0);
    let mut scores = QuickOpenNameScores::default();
    for name in &unique {
        assert_eq!(scores.score(name, || { calculated.set(calculated.get() + 1); Some(7) }), Some(7));
    }
    assert_eq!(calculated.get(), QUICK_OPEN_NAME_SCORE_SAMPLE);
    assert!(!scores.enabled);
    assert!(scores.scores.is_empty());
    // Once the sample is unique, later duplicate names take the direct path.
    for _ in 0..20 {
        assert_eq!(scores.score(&unique[0], || { calculated.set(calculated.get() + 1); None }), None);
    }
    assert_eq!(calculated.get(), QUICK_OPEN_NAME_SCORE_SAMPLE + 20);

    let repeated = ["index.ts", "package.json", "readme.md", "component.tsx"];
    let distinct: Vec<_> = (0..QUICK_OPEN_NAME_SCORE_CACHE_LIMIT + 20).map(|index| format!("distinct-{index}.rs")).collect();
    let mut scores = QuickOpenNameScores::default();
    calculated.set(0);
    for index in 0..QUICK_OPEN_NAME_SCORE_SAMPLE {
        let expected = (index % 4 != 0).then_some(9);
        assert_eq!(scores.score(repeated[index % 4], || {
            calculated.set(calculated.get() + 1); expected
        }), expected);
    }
    assert!(scores.enabled);
    assert_eq!(calculated.get(), repeated.len(), "completed nonmatches are reusable too");
    for name in &distinct { scores.score(name, || Some(1)); }
    assert_eq!(scores.scores.len(), QUICK_OPEN_NAME_SCORE_CACHE_LIMIT);
    assert_eq!(scores.score(repeated[0], || panic!("known nonmatch must remain cached")), None);
    let uncached = distinct.last().unwrap();
    for value in [2, 3] {
        assert_eq!(scores.score(uncached, || Some(value)), Some(value), "uncached names are still scored after the limit");
    }
}

#[test]
fn adaptive_filename_sampling_preserves_queries_sources_and_cancellation_boundaries() {
    let sources: Vec<_> = [false, true].into_iter().map(|repeat_first| Arc::new((0..512).map(|index| {
        let repeat = (index < QUICK_OPEN_NAME_SCORE_SAMPLE) == repeat_first;
        let name = if repeat { ["index.ts", "input.rs", "other.txt", "界面.rs"][index % 4].to_owned() }
            else { format!("index-{index:05}.rs") };
        let relative = format!("src/package-{index}/{name}");
        QuickOpenEntry { path: PathBuf::from(&relative), name, relative }
    }).collect::<Vec<_>>())).collect();
    let mut cache = QuickOpenRankingCache::default();
    for source in &sources {
        for query in ["i", "in", "index", "other", "界", "src/p", "i"] {
            verify(&mut cache, source, query);
        }
        let previous = cache.rank(source, "unmatched-before-cancellation", || false).unwrap();
        for boundary in [1, 2, 128, 129, 130, source.len() + 2] {
            let probes = Cell::new(0);
            assert!(cache.rank(source, "index", || {
                probes.set(probes.get() + 1);
                probes.get() == boundary
            }).is_none(), "boundary={boundary}");
            assert_eq!(probes.get(), boundary, "sampling must not add cancellation probes");
            assert_eq!(cache.completed.as_ref().unwrap().query, "unmatched-before-cancellation");
            assert_eq!(cache.completed.as_ref().unwrap().results, previous);
        }
        assert_eq!(verify(&mut cache, source, "index"), source.len() + 2);
    }
}

#[test]
fn root_file_reused_score_preserves_fresh_matcher_results() {
    for name in ["aaaa", "a_aa", "index.ts", "café_界.rs", "İnput.rs", "ΟΣ.rs"] {
        let entry = QuickOpenEntry {
            path: PathBuf::from(name), name: name.into(), relative: name.into(),
        };
        for query in ["a", "aaa", "i", "in", "café", "界", "ΟΣ", ""] {
            let mut matcher = crate::fuzzy_score::PreparedFuzzyScore::new(query);
            let expected = matcher.score(name).map(|score|
                score + crate::fuzzy_score::PreparedFuzzyScore::new(query).score(name).unwrap_or(0) / 4);
            assert_eq!(quick_open_score(&mut matcher, &entry, false), expected,
                "name={name} query={query}");
        }
    }
}

#[test]
#[ignore = "manual comparative release benchmark"]
fn benchmark_repeated_quick_open_filenames() {
    let cancel = crate::background::CancellationToken::new();
    // Independent queries deliberately avoid prefix narrowing. The unique
    // fixture exposes memoization overhead when every filename needs scoring.
    let queries = ["index", "ijklmissing", "readme", "package"];
    for repeated in [true, false] {
        let source = Arc::new((0..50_000).map(|index| {
            let base = ["index.ts", "package.json", "readme.md", "component.tsx", "styles.css"][index % 5];
            let name = if repeated { base.to_owned() } else {
                let (stem, extension) = base.rsplit_once('.').unwrap();
                format!("{stem}-{index:05}.{extension}")
            };
            let relative = format!("packages/package-{index:05}/src/{name}");
            QuickOpenEntry { path: PathBuf::from(&relative), name, relative }
        }).collect::<Vec<_>>());
        let expected: Vec<_> = queries.iter().map(|query|
            rank_quick_open_files(&source, query, &cancel)).collect();
        let mut samples = [Vec::new(), Vec::new()];
        for sample in 0..7 {
            for cached in [sample % 2 == 0, sample % 2 != 0] {
                let mut cache = QuickOpenRankingCache {
                    force_direct_name_scores: !cached,
                    ..Default::default()
                };
                let started = std::time::Instant::now();
                for (query, expected) in queries.iter().zip(&expected) {
                    let result = cache.rank(&source, query, || cancel.is_cancelled()).unwrap();
                    assert_eq!(&std::hint::black_box(result), expected);
                }
                samples[usize::from(cached)].push(started.elapsed());
            }
        }
        samples.iter_mut().for_each(|samples| samples.sort());
        let label = if repeated { "repeated" } else { "unique" };
        eprintln!("Quick Open 50,000 files with {label} names / four independent queries: direct-name-scores={:?}, adaptive-name-scores={:?}",
            samples[0][3], samples[1][3]);
    }
}

fn verify(
    cache: &mut QuickOpenRankingCache,
    source: &Arc<Vec<QuickOpenEntry>>,
    query: &str,
) -> usize {
    let checks = Cell::new(0);
    let actual = cache
        .rank(source, query, || {
            checks.set(checks.get() + 1);
            false
        })
        .unwrap();
    let expected =
        rank_quick_open_files(source, query, &crate::background::CancellationToken::new());
    assert_eq!(actual, expected, "query={query:?}");
    checks.get()
}

#[test]
fn incremental_query_sequences_match_full_ranking_across_backspace_and_modes() {
    let source = entries(&[
        "src/components/AlphaControl.rs",
        "src/components/alpha.rs",
        "src/canvas.rs",
        "lib/component.rs",
        "docs/alpha-control.md",
        "tools/abc.txt",
        "src/界/Café.rs",
        "src/界/界面.rs",
        "tests/İnput.rs",
        "src/ΟΣ/value.rs",
        "misc/other.txt",
    ]);
    let mut cache = QuickOpenRankingCache::default();
    for query in [
        "a", "al", "alp", "ALP", "al", "a", "c", "ca", "CAFÉ", "caf", "界", "界面", "src/",
        "src/c", "src/co", "src\\co", "src\\界", "src/界/", "src", "İ", "İn", "ΟΣ", "ΟΣa", "", "x",
        "xy", "xyz",
    ] {
        verify(&mut cache, &source, query);
    }
}

#[test]
fn extensions_visit_only_previous_matches_and_identical_query_reuses_results() {
    let source = entries(&[
        "src/alpha.rs",
        "src/alpine.rs",
        "src/beta.rs",
        "lib/gamma.rs",
    ]);
    let mut cache = QuickOpenRankingCache::default();
    assert_eq!(verify(&mut cache, &source, "al"), source.len() + 2);
    let previous_matches = cache
        .completed
        .as_ref()
        .unwrap()
        .matches
        .as_ref()
        .unwrap()
        .len();
    assert_eq!(previous_matches, 2);
    assert_eq!(verify(&mut cache, &source, "alp"), previous_matches + 2);
    assert_eq!(verify(&mut cache, &source, "ALP"), 1);
    assert_eq!(verify(&mut cache, &source, "a"), source.len() + 2);
    // Switching from name eligibility to path eligibility can admit new files.
    assert_eq!(verify(&mut cache, &source, "src/a"), source.len() + 2);
}

#[test]
fn arc_source_replacement_never_reuses_old_entry_indices() {
    let first = entries(&["src/alpha.rs", "src/beta.rs", "src/alpine.rs"]);
    let second = entries(&[
        "src/beta.rs",
        "src/alphabet.rs",
        "new/alpha.rs",
        "new/alpine.rs",
    ]);
    let mut cache = QuickOpenRankingCache::default();
    verify(&mut cache, &first, "al");
    assert_eq!(verify(&mut cache, &second, "alp"), second.len() + 2);
    assert_eq!(verify(&mut cache, &first, "alp"), first.len() + 2);
    let weak = Arc::downgrade(&first);
    drop(first);
    // Ranking metadata must not retain obsolete full workspace indexes.
    assert!(weak.upgrade().is_none());
}

#[test]
fn cancellation_at_any_scan_boundary_preserves_completed_query_and_results() {
    let source = entries(&[
        "alpha.rs",
        "alpine.rs",
        "alphabet.rs",
        "beta.rs",
        "gamma.rs",
    ]);
    let mut cache = QuickOpenRankingCache::default();
    let expected = cache.rank(&source, "al", || false).unwrap();
    let matches = cache.completed.as_ref().unwrap().matches.clone();
    // First check, midscan checks, and final post-sort check.
    for boundary in [1, 2, 3, 5] {
        let checks = Cell::new(0);
        let result = cache.rank(&source, "alp", || {
            checks.set(checks.get() + 1);
            checks.get() == boundary
        });
        assert!(result.is_none(), "boundary={boundary}");
        let completed = cache.completed.as_ref().unwrap();
        assert_eq!(completed.query, "al");
        assert_eq!(completed.results, expected);
        assert_eq!(completed.matches, matches);
    }
    assert_eq!(cache.rank(&source, "al", || false).unwrap(), expected);
    verify(&mut cache, &source, "alph");
}

#[test]
fn matching_index_cap_falls_back_to_full_scan_without_losing_candidates() {
    let source = Arc::new(
        (0..QUICK_OPEN_MATCH_CACHE_LIMIT + 1)
            .map(|index| {
                let name = format!("file-{index:05}.rs");
                QuickOpenEntry {
                    path: PathBuf::from(&name),
                    relative: format!("src/{name}"),
                    name,
                }
            })
            .collect::<Vec<_>>(),
    );
    let mut cache = QuickOpenRankingCache::default();
    verify(&mut cache, &source, "f");
    assert!(cache.completed.as_ref().unwrap().matches.is_none());
    assert!(cache.spare.capacity() <= QUICK_OPEN_MATCH_CACHE_LIMIT);
    assert_eq!(verify(&mut cache, &source, "fi"), source.len() + 2);
    assert!(cache.completed.as_ref().unwrap().matches.is_none());
    let previous = cache.completed.as_ref().unwrap().results.clone();
    let checks = Cell::new(0);
    assert_eq!(
        cache.rank(&source, "FI", || {
            checks.set(checks.get() + 1);
            false
        }),
        Some(previous)
    );
    assert_eq!(checks.get(), 1, "identical capped query needs no rescoring");
    verify(&mut cache, &source, "file-6553");
    assert!(
        cache
            .completed
            .as_ref()
            .unwrap()
            .matches
            .as_ref()
            .unwrap()
            .len()
            < QUICK_OPEN_MATCH_CACHE_LIMIT
    );
    verify(&mut cache, &source, "file-65536");
    assert_eq!(
        cache
            .completed
            .as_ref()
            .unwrap()
            .matches
            .as_ref()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn shared_quick_open_handles_reuse_worker_ranking_and_honor_cancelled_tokens() {
    let source = entries(&["src/alpha.rs", "src/alpine.rs", "src/beta.rs"]);
    let index = QuickOpenIndex::default();
    let other = index.clone();
    let cancel = crate::background::CancellationToken::new();
    let first = index.rank_files(&source, "al", &cancel).unwrap();
    assert_eq!(other.rank_files(&source, "AL", &cancel).unwrap(), first);
    cancel.cancel();
    assert!(other.rank_files(&source, "alp", &cancel).is_none());
    assert_eq!(
        index
            .ranking
            .lock()
            .unwrap()
            .completed
            .as_ref()
            .unwrap()
            .query,
        "al"
    );
}

fn service_task(
    service: &LatestQueryService,
    run: impl FnOnce() + Send + 'static,
) -> crate::background::CancellationToken {
    let token = crate::background::CancellationToken::new();
    service.replace(LatestQueryJob {
        cancel: token.clone(),
        run: Box::new(run),
    });
    token
}

#[test]
fn latest_request_service_executes_only_latest_pending_task_after_blocked_first() {
    use std::sync::mpsc;
    use std::time::Duration;
    let (service, worker) = LatestQueryService::channel();
    let handle = std::thread::spawn(worker);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let first_tx = result_tx.clone();
    service_task(&service, move || {
        entered_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        first_tx.send(0).unwrap();
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut tokens = Vec::new();
    for id in 1..=64 {
        let result_tx = result_tx.clone();
        tokens.push(service_task(&service, move || {
            result_tx.send(id).unwrap();
        }));
    }
    assert!(
        tokens[..63]
            .iter()
            .all(crate::background::CancellationToken::is_cancelled)
    );
    assert!(!tokens[63].is_cancelled());
    release_tx.send(()).unwrap();
    assert_eq!(result_rx.recv_timeout(Duration::from_secs(5)).unwrap(), 0);
    assert_eq!(result_rx.recv_timeout(Duration::from_secs(5)).unwrap(), 64);
    drop(service);
    handle.join().unwrap();
    assert!(result_rx.try_recv().is_err());
}

#[test]
fn latest_request_service_skips_cancelled_tasks_before_execution() {
    use std::sync::mpsc;
    use std::time::Duration;
    struct Dropped(mpsc::Sender<()>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }
    let (service, worker) = LatestQueryService::channel();
    let (dropped_tx, dropped_rx) = mpsc::channel();
    let (executed_tx, executed_rx) = mpsc::channel();
    let signal = Dropped(dropped_tx);
    let token = service_task(&service, move || {
        let _signal = signal;
        executed_tx.send(()).unwrap();
    });
    token.cancel();
    let handle = std::thread::spawn(worker);
    dropped_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(executed_rx.try_recv().is_err());
    drop(service);
    handle.join().unwrap();
}

#[test]
fn latest_request_service_closes_after_owner_drop_and_discards_pending_work() {
    use std::sync::mpsc;
    use std::time::Duration;
    let (service, worker) = LatestQueryService::channel();
    let owner = Arc::new(service);
    let weak = Arc::downgrade(&owner);
    let (closed_tx, closed_rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        worker();
        closed_tx.send(()).unwrap();
    });
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    service_task(&owner, move || {
        entered_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (executed_tx, executed_rx) = mpsc::channel();
    let pending = service_task(&owner, move || {
        executed_tx.send(()).unwrap();
    });
    drop(owner);
    assert!(weak.upgrade().is_none());
    assert!(pending.is_cancelled());
    release_tx.send(()).unwrap();
    closed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    handle.join().unwrap();
    assert!(executed_rx.try_recv().is_err());
}

#[test]
fn parallel_latest_request_replacements_have_one_exact_surviving_pending_task() {
    use std::sync::{Barrier, mpsc};
    use std::time::Duration;
    let (service, worker) = LatestQueryService::channel();
    let service = Arc::new(service);
    let handle = std::thread::spawn(worker);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let first_tx = result_tx.clone();
    service_task(&service, move || {
        entered_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        first_tx.send(0).unwrap();
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let barrier = Arc::new(Barrier::new(16));
    let requests: Vec<_> = (1..=16)
        .map(|id| {
            let service = Arc::clone(&service);
            let barrier = Arc::clone(&barrier);
            let result_tx = result_tx.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let token = service_task(&service, move || {
                    result_tx.send(id).unwrap();
                });
                (id, token)
            })
        })
        .collect();
    let requests: Vec<_> = requests
        .into_iter()
        .map(|request| request.join().unwrap())
        .collect();
    let survivors: Vec<_> = requests
        .iter()
        .filter(|(_, token)| !token.is_cancelled())
        .collect();
    assert_eq!(survivors.len(), 1);
    let expected = survivors[0].0;
    release_tx.send(()).unwrap();
    assert_eq!(result_rx.recv_timeout(Duration::from_secs(5)).unwrap(), 0);
    assert_eq!(
        result_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        expected
    );
    drop(service);
    handle.join().unwrap();
    assert!(result_rx.try_recv().is_err());
}

#[test]
fn lazy_service_jobs_capture_cache_only_handles_without_sender_self_cycle() {
    let mut app = crate::app::App::test_new();
    let index = QuickOpenIndex::default();
    let owner = Arc::downgrade(index.service.as_ref().unwrap());
    assert!(index.service.as_ref().unwrap().get().is_none());
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    index.submit_latest(&mut app.background, move |worker_index, cancel| {
        assert!(worker_index.service.is_none());
        assert!(!cancel.is_cancelled());
        result_tx.send(()).unwrap();
    });
    assert!(index.service.as_ref().unwrap().get().is_some());
    result_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    drop(index);
    assert!(owner.upgrade().is_none());
}

#[test]
#[ignore = "release-mode sequential Quick Open ranking benchmark; filesystem excluded"]
fn benchmark_quick_open_incremental_query_sequence() {
    let source = Arc::new(
        (0..50_000)
            .map(|index| {
                let name = match index % 10 {
                    0 => format!("component-{index:05}.rs"),
                    1 => format!("canvas-{index:05}.rs"),
                    _ => format!("module-{index:05}.rs"),
                };
                QuickOpenEntry {
                    path: PathBuf::from(format!("src/{name}")),
                    relative: format!("src/{name}"),
                    name,
                }
            })
            .collect::<Vec<_>>(),
    );
    let queries = [
        "c",
        "co",
        "com",
        "comp",
        "compo",
        "compon",
        "component",
        "component-1",
        "component-12",
        "component-12",
    ];
    let cancel = crate::background::CancellationToken::new();
    let expected: Vec<_> = queries
        .iter()
        .map(|query| rank_quick_open_files(&source, query, &cancel))
        .collect();
    let mut timings = [Vec::new(), Vec::new()];
    for sample in 0..7 {
        for incremental in [sample % 2 == 0, sample % 2 != 0] {
            let mut cache = QuickOpenRankingCache::default();
            let started = std::time::Instant::now();
            for (query, expected) in queries.iter().zip(&expected) {
                let results = if incremental {
                    cache.rank(&source, query, || false).unwrap()
                } else {
                    rank_quick_open_files(&source, query, &cancel)
                };
                assert_eq!(&std::hint::black_box(results), expected);
            }
            timings[usize::from(incremental)].push(started.elapsed());
        }
    }
    timings.iter_mut().for_each(|timing| timing.sort());
    eprintln!(
        "Quick Open 50k candidates / ten sequential queries (filesystem excluded): full={:?}, incremental={:?}",
        timings[0][3], timings[1][3]
    );
}
