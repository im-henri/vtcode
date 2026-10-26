use super::*;
use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};

fn generous() -> FuzzyWorkLimits {
    FuzzyWorkLimits {
        candidate_cells: 8_000_000,
        request_cells: 32_000_000,
        candidate_bytes: 8_000_000,
        request_bytes: 32_000_000,
        scratch_bytes: 4_000_000,
    }
}

fn fresh(choice: &str, query: &str) -> Option<i64> {
    SkimMatcherV2::default().fuzzy_match(choice, query)
}

fn completed(scorer: &mut PreparedFuzzyScore<'_>, choice: &str) -> Option<i64> {
    let mut request = FuzzyWorkBudget::new(generous());
    match scorer.score_cancellable(choice, &mut request.begin_candidate(), || false) {
        FuzzyScoreOutcome::Scored(score) => score,
        outcome => panic!("unexpected outcome {outcome:?}"),
    }
}

fn retained(scorer: &PreparedFuzzyScore<'_>) -> usize {
    scorer.pattern_chars.capacity() * std::mem::size_of::<char>()
        + scorer.choice_chars.capacity() * std::mem::size_of::<char>()
        + scorer.scratch.first_matches.capacity() * std::mem::size_of::<usize>()
        + scorer.scratch.bonuses.capacity() * std::mem::size_of::<i32>()
        + scorer.scratch.cells.capacity() * std::mem::size_of::<ScoreCell>()
}

#[test]
fn cancellable_scores_match_fresh_engine_exhaustively_and_across_reuse() {
    fn words(alphabet: &[char], max: u32) -> Vec<String> {
        let mut words = Vec::new();
        for length in 0..=max {
            for mut code in 0..alphabet.len().pow(length) {
                let mut word = String::new();
                for _ in 0..length {
                    word.push(alphabet[code % alphabet.len()]);
                    code /= alphabet.len();
                }
                words.push(word);
            }
        }
        words
    }
    for alphabet in [&['a', 'A', '_', '/'][..], &['ä', '界', '_', 'a'][..]] {
        let choices = words(alphabet, 4);
        for query in words(alphabet, 3) {
            let mut scorer = PreparedFuzzyScore::new(&query);
            for choice in &choices {
                assert_eq!(
                    completed(&mut scorer, choice),
                    fresh(choice, &query),
                    "choice={choice:?} query={query:?}"
                );
            }
        }
    }
    let mut scorer = PreparedFuzzyScore::new("aaa");
    for choice in ["a_aa", "aaaa", "aaa", "a/a_a", "aaa"] {
        assert_eq!(completed(&mut scorer, choice), fresh(choice, "aaa"));
    }
    assert_eq!(completed(&mut scorer, "aaa"), Some(71));
}

#[test]
fn cancellable_random_unicode_and_case_scores_keep_the_fresh_oracle() {
    let alphabet = [
        'a', 'A', 'ä', 'Ä', '界', '🦀', 'İ', '\u{307}', 'ß', 'Σ', 'ς', 'σ', ' ', '/', '_',
        '\u{2003}',
    ];
    let mut seed = 0xb31f_0e73_u64;
    let mut next = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        (seed >> 32) as usize
    };
    for case in 0..1000 {
        let choice: String = (0..next() % 80)
            .map(|_| alphabet[next() % alphabet.len()])
            .collect();
        let query: String = if case % 2 == 0 {
            choice.chars().step_by(1 + next() % 9).take(9).collect()
        } else {
            (0..next() % 9)
                .map(|_| alphabet[next() % alphabet.len()])
                .collect()
        };
        let mut scorer = PreparedFuzzyScore::new(&query);
        for target in [&choice, "src/Ää-界🦀_i\u{307}", "ΟΣa/ος", "", &choice] {
            assert_eq!(
                completed(&mut scorer, target),
                fresh(target, &query),
                "target={target:?} query={query:?}"
            );
        }
    }
}

#[test]
fn every_cancellation_checkpoint_bounds_work_and_allows_both_apis_to_reuse_scratch() {
    let query = "a界".repeat(16);
    let choice = "a_界_".repeat(400);
    let reference = fresh(&choice, &query);
    let mut scorer = PreparedFuzzyScore::new(&query);
    let mut baseline = FuzzyWorkBudget::new(generous());
    let mut checkpoints = 0;
    assert_eq!(
        scorer.score_cancellable(&choice, &mut baseline.begin_candidate(), || {
            checkpoints += 1;
            false
        }),
        FuzzyScoreOutcome::Scored(reference)
    );
    assert!(checkpoints > 50);
    let mut previous = FuzzyWorkUsage::default();
    for stop_at in 1..=checkpoints {
        let mut scorer = PreparedFuzzyScore::new(&query);
        let mut request = FuzzyWorkBudget::new(generous());
        let mut checks = 0;
        assert_eq!(
            scorer.score_cancellable(&choice, &mut request.begin_candidate(), || {
                checks += 1;
                checks >= stop_at
            }),
            FuzzyScoreOutcome::Cancelled
        );
        let usage = request.usage();
        assert!(
            usage.cells.saturating_sub(previous.cells) <= 1024,
            "checkpoint={stop_at} cells={usage:?} prior={previous:?}"
        );
        assert!(
            usage.bytes.saturating_sub(previous.bytes) <= 4096,
            "checkpoint={stop_at} bytes={usage:?} prior={previous:?}"
        );
        assert!(retained(&scorer) <= generous().scratch_bytes);
        previous = usage;
        assert_eq!(completed(&mut scorer, &choice), reference);
        assert_eq!(scorer.score(&choice), reference);
    }
}

#[test]
fn candidate_request_and_byte_limits_are_distinct_from_non_matches() {
    let query = "abc";
    let choice = "a_x_b_x_c";
    let expected = fresh(choice, query);
    let mut scorer = PreparedFuzzyScore::new(query);
    let mut measured = FuzzyWorkBudget::new(generous());
    assert_eq!(
        scorer.score_cancellable(choice, &mut measured.begin_candidate(), || false),
        FuzzyScoreOutcome::Scored(expected)
    );
    let work = measured.usage();
    assert!(work.cells > 0 && work.bytes > 0);

    let mut limits = generous();
    limits.candidate_cells = work.cells - 1;
    let mut request = FuzzyWorkBudget::new(limits);
    assert_eq!(
        scorer.score_cancellable(choice, &mut request.begin_candidate(), || false),
        FuzzyScoreOutcome::WorkLimited
    );
    assert_eq!(
        request.usage().cells,
        0,
        "preflight must reject excessive DP before initialization"
    );
    limits = generous();
    limits.request_cells = work.cells + work.cells / 2;
    let mut request = FuzzyWorkBudget::new(limits);
    assert_eq!(
        scorer.score_cancellable(choice, &mut request.begin_candidate(), || false),
        FuzzyScoreOutcome::Scored(expected)
    );
    assert_eq!(
        scorer.score_cancellable(choice, &mut request.begin_candidate(), || false),
        FuzzyScoreOutcome::WorkLimited
    );
    assert_eq!(request.usage().cells, work.cells);

    limits = generous();
    limits.candidate_bytes = work.bytes - 1;
    let mut request = FuzzyWorkBudget::new(limits);
    assert_eq!(
        scorer.score_cancellable(choice, &mut request.begin_candidate(), || false),
        FuzzyScoreOutcome::WorkLimited
    );
    assert!(request.usage().bytes <= limits.candidate_bytes);
    assert_eq!(completed(&mut scorer, choice), expected);
    assert_eq!(completed(&mut scorer, "xyz"), None);
}

#[test]
fn checked_budget_accounting_never_wraps_and_normalization_shares_the_candidate_quota() {
    let limits = FuzzyWorkLimits {
        candidate_cells: usize::MAX,
        request_cells: usize::MAX,
        candidate_bytes: usize::MAX,
        request_bytes: usize::MAX,
        scratch_bytes: usize::MAX,
    };
    let mut request = FuzzyWorkBudget::new(limits);
    assert!(request.charge_bytes(usize::MAX));
    assert!(!request.charge_bytes(1));
    assert_eq!(request.remaining_bytes(), 0);
    {
        let mut candidate = request.begin_candidate();
        assert!(!candidate.charge_bytes(1));
        assert!(candidate.charge_cells(usize::MAX));
        assert!(!candidate.charge_cells(1));
        assert_eq!(candidate.usage().cells, usize::MAX);
        assert_eq!(candidate.remaining_cells(), 0);
    }
    assert_eq!(
        request.usage(),
        FuzzyWorkUsage {
            cells: usize::MAX,
            bytes: usize::MAX
        }
    );
    let mut limits = generous();
    limits.candidate_bytes = 4;
    let mut request = FuzzyWorkBudget::new(limits);
    let mut candidate = request.begin_candidate();
    assert!(candidate.charge_bytes(4));
    assert_eq!(candidate.remaining_bytes(), 0);
    let mut scorer = PreparedFuzzyScore::new("ab");
    assert_eq!(
        scorer.score_cancellable("ab", &mut candidate, || false),
        FuzzyScoreOutcome::WorkLimited
    );
}

#[test]
fn scratch_limits_enforce_actual_retention_even_after_unbounded_score_and_abort() {
    let mut scorer = PreparedFuzzyScore::new("ab");
    let large = format!("{}ab", "x".repeat(20_000));
    assert_eq!(scorer.score(&large), fresh(&large, "ab"));
    assert!(retained(&scorer) > 1024);
    let mut limits = generous();
    limits.scratch_bytes = 1024;
    let mut request = FuzzyWorkBudget::new(limits);
    assert_eq!(
        scorer.score_cancellable("ab", &mut request.begin_candidate(), || false),
        FuzzyScoreOutcome::Scored(fresh("ab", "ab"))
    );
    assert!(retained(&scorer) <= 1024);
    limits.scratch_bytes = 0;
    let mut request = FuzzyWorkBudget::new(limits);
    assert_eq!(
        scorer.score_cancellable("a_b", &mut request.begin_candidate(), || false),
        FuzzyScoreOutcome::WorkLimited
    );
    assert_eq!(retained(&scorer), 0);
    assert_eq!(
        scorer.score_cancellable("a_b", &mut request.begin_candidate(), || true),
        FuzzyScoreOutcome::Cancelled
    );
    assert_eq!(retained(&scorer), 0);
    assert_eq!(completed(&mut scorer, "a_b"), fresh("a_b", "ab"));
}
