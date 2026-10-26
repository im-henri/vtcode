//! Reusable score-only path for Quick Open. Scores follow a fresh pinned
//! fuzzy-matcher 0.3.7 SkimMatcherV2, with borrowed ASCII and reusable Unicode.
//! No match-position backtracking or per-candidate pattern/bonus allocations
//! are needed when ranking thousands of immutable file names.
//!
//! The score recurrence is adapted from fuzzy-matcher (MIT):
//! Copyright (c) 2019 Jinzhou Zhang
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in all
//! copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//! SOFTWARE.

const NEGATIVE_INFINITY: i32 = i16::MIN as i32;

/// Completed scores remain exact. Cancellation and optional-work limits must
/// never be cached as evidence that a candidate cannot match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FuzzyScoreOutcome {
    Scored(Option<i64>),
    Cancelled,
    WorkLimited,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FuzzyWorkLimits {
    pub(crate) candidate_cells: usize,
    pub(crate) request_cells: usize,
    pub(crate) candidate_bytes: usize,
    pub(crate) request_bytes: usize,
    pub(crate) scratch_bytes: usize,
}

impl Default for FuzzyWorkLimits {
    fn default() -> Self {
        Self {
            candidate_cells: 262_144,
            request_cells: 2_097_152,
            candidate_bytes: 65_536,
            request_bytes: 4_194_304,
            scratch_bytes: 2_097_152,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FuzzyWorkUsage {
    pub(crate) cells: usize,
    pub(crate) bytes: usize,
}

pub(crate) struct FuzzyWorkBudget {
    limits: FuzzyWorkLimits,
    usage: FuzzyWorkUsage,
}

impl FuzzyWorkBudget {
    pub(crate) fn new(limits: FuzzyWorkLimits) -> Self {
        Self {
            limits,
            usage: FuzzyWorkUsage::default(),
        }
    }

    /// Charge request-wide extraction/normalization work outside one word.
    pub(crate) fn charge_bytes(&mut self, bytes: usize) -> bool {
        let Some(total) = self.usage.bytes.checked_add(bytes) else {
            return false;
        };
        if total > self.limits.request_bytes {
            return false;
        }
        self.usage.bytes = total;
        true
    }

    pub(crate) fn begin_candidate(&mut self) -> FuzzyCandidateBudget<'_> {
        FuzzyCandidateBudget {
            request: self,
            usage: FuzzyWorkUsage::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn usage(&self) -> FuzzyWorkUsage {
        self.usage
    }
    #[cfg(test)]
    pub(crate) fn remaining_bytes(&self) -> usize {
        self.limits.request_bytes.saturating_sub(self.usage.bytes)
    }
    pub(crate) fn remaining_cells(&self) -> usize {
        self.limits.request_cells.saturating_sub(self.usage.cells)
    }
}

pub(crate) struct FuzzyCandidateBudget<'a> {
    request: &'a mut FuzzyWorkBudget,
    usage: FuzzyWorkUsage,
}

impl FuzzyCandidateBudget<'_> {
    pub(crate) fn charge_bytes(&mut self, bytes: usize) -> bool {
        let Some(total) = self.usage.bytes.checked_add(bytes) else {
            return false;
        };
        if total > self.request.limits.candidate_bytes || !self.request.charge_bytes(bytes) {
            return false;
        }
        self.usage.bytes = total;
        true
    }

    pub(crate) fn charge_cells(&mut self, cells: usize) -> bool {
        let Some(candidate) = self.usage.cells.checked_add(cells) else {
            return false;
        };
        let Some(request) = self.request.usage.cells.checked_add(cells) else {
            return false;
        };
        if candidate > self.request.limits.candidate_cells
            || request > self.request.limits.request_cells
        {
            return false;
        }
        self.usage.cells = candidate;
        self.request.usage.cells = request;
        true
    }

    #[cfg(test)]
    pub(crate) fn usage(&self) -> FuzzyWorkUsage {
        self.usage
    }
    pub(crate) fn remaining_cells(&self) -> usize {
        self.request
            .limits
            .candidate_cells
            .saturating_sub(self.usage.cells)
            .min(self.request.remaining_cells())
    }
    #[cfg(test)]
    pub(crate) fn remaining_bytes(&self) -> usize {
        self.request
            .limits
            .candidate_bytes
            .saturating_sub(self.usage.bytes)
            .min(self.request.remaining_bytes())
    }
    pub(crate) fn scratch_limit(&self) -> usize {
        self.request.limits.scratch_bytes
    }
}

#[derive(Clone, Copy)]
struct ScoreCell {
    matched: i32,
    skipped: i32,
    bonus: i32,
}

impl Default for ScoreCell {
    fn default() -> Self {
        Self {
            matched: NEGATIVE_INFINITY,
            skipped: NEGATIVE_INFINITY,
            bonus: 0,
        }
    }
}

pub(crate) struct PreparedFuzzyScore<'a> {
    query: &'a str,
    ascii: bool,
    case_sensitive: bool,
    pattern_chars: Vec<char>,
    choice_chars: Vec<char>,
    cancellable_pattern_prepared: bool,
    scratch: ScoreScratch,
}

#[derive(Default)]
struct ScoreScratch {
    first_matches: Vec<usize>,
    bonuses: Vec<i32>,
    cells: Vec<ScoreCell>,
}

impl<'a> PreparedFuzzyScore<'a> {
    pub(crate) fn new(query: &'a str) -> Self {
        Self {
            query,
            ascii: query.is_ascii(),
            case_sensitive: query.bytes().any(|byte| byte.is_ascii_uppercase()),
            pattern_chars: Vec::new(),
            choice_chars: Vec::new(),
            cancellable_pattern_prepared: false,
            scratch: ScoreScratch::default(),
        }
    }

    pub(crate) fn score(&mut self, choice: &str) -> Option<i64> {
        if self.query.is_empty() {
            return Some(0);
        }
        let sensitive = self.case_sensitive;
        if self.ascii && choice.is_ascii() {
            self.scratch.score(
                choice.as_bytes(),
                self.query.as_bytes(),
                0,
                |a, b| {
                    if sensitive {
                        a == b
                    } else {
                        a.eq_ignore_ascii_case(&b)
                    }
                },
                boundary_bonus,
            )
        } else {
            if self.pattern_chars.is_empty() {
                self.pattern_chars.extend(self.query.chars());
            }
            self.choice_chars.clear();
            self.choice_chars.extend(choice.chars());
            self.scratch.score(
                &self.choice_chars,
                &self.pattern_chars,
                '\0',
                |a, b| {
                    if sensitive {
                        a == b
                    } else {
                        a.eq_ignore_ascii_case(&b)
                    }
                },
                |a, b| {
                    boundary_bonus(
                        if a.is_ascii() { a as u8 } else { b'a' },
                        if b.is_ascii() { b as u8 } else { b'a' },
                    )
                },
            )
        }
    }
}

impl ScoreScratch {
    fn score<T: Copy + Eq>(
        &mut self,
        choice: &[T],
        pattern: &[T],
        empty: T,
        equal: impl Fn(T, T) -> bool,
        boundary: impl Fn(T, T) -> i32,
    ) -> Option<i64> {
        if pattern.len() > choice.len() {
            return None;
        }
        if pattern.len() == 1 {
            // The first DP row has a constant -1 skipped score. A one-scalar
            // query therefore needs only the boundary and exact-case bonus;
            // avoid constructing/filling the matrix for the first keystroke
            // across an entire workspace index.
            let byte = pattern[0];
            let mut previous = empty;
            let mut best = None;
            for (index, &candidate) in choice.iter().enumerate() {
                if equal(candidate, byte) {
                    let bonus = boundary(previous, candidate) * if index == 0 { 2 } else { 1 };
                    let score = i64::from(bonus + if candidate == byte { 16 } else { 14 } - 1);
                    best = Some(best.map_or(score, |old: i64| old.max(score)));
                }
                previous = candidate;
            }
            return best;
        }
        self.first_matches.clear();
        let mut from = 0;
        for &byte in pattern {
            let position = from
                + choice[from..]
                    .iter()
                    .position(|&candidate| equal(candidate, byte))?;
            self.first_matches.push(position);
            from = position + 1;
        }

        // No complete alignment can end beyond the last occurrence of its
        // final scalar. Trim that irrelevant tail from every DP row while
        // retaining all gap propagation and reused-row bonus behavior inside
        // the feasible prefix, exactly as the fresh scoring engine does.
        let end = choice
            .iter()
            .rposition(|&candidate| equal(candidate, pattern[pattern.len() - 1]))?
            + 1;
        let choice = &choice[..end];

        let columns = choice.len() + 1;
        self.bonuses.resize(columns, 0);
        let mut previous = empty;
        for (index, &byte) in choice.iter().enumerate() {
            self.bonuses[index + 1] = boundary(previous, byte);
            previous = byte;
        }
        self.bonuses[1] *= 2;
        self.cells.resize(columns * 2, ScoreCell::default());
        // Upstream's cached matrix leaves old bonuses in reused rows. Those
        // can change scores based on earlier queries/candidate order. Reset
        // contents while retaining capacity, matching a fresh engine exactly.
        let (initial, current) = self.cells.split_at_mut(columns);
        initial.fill(ScoreCell {
            skipped: -1,
            ..ScoreCell::default()
        });
        current.fill(ScoreCell::default());

        for (pattern_index, &byte) in pattern.iter().enumerate() {
            let current_row = ((pattern_index + 1) & 1) * columns;
            let previous_row = (pattern_index & 1) * columns;
            for position in self.first_matches[pattern_index]..choice.len() {
                let left = current_row + position;
                let current = left + 1;
                let previous = previous_row + position;
                if equal(choice[position], byte) {
                    let bonus = self.cells[left]
                        .bonus
                        .max(self.bonuses[position + 1])
                        .max(4);
                    self.cells[left].bonus = bonus;
                    let consecutive = self.cells[previous].matched + bonus;
                    let separated = self.cells[previous].skipped + self.bonuses[position + 1];
                    let match_score = if choice[position] == byte { 16 } else { 14 };
                    self.cells[current].matched = consecutive.max(separated) + match_score;
                } else {
                    self.cells[current].matched = NEGATIVE_INFINITY;
                    self.cells[current].bonus = 0;
                }
                self.cells[current].skipped =
                    (self.cells[left].matched - 4).max(self.cells[left].skipped - 1);
            }
        }
        let final_row = (pattern.len() & 1) * columns;
        let first = self.first_matches[pattern.len() - 1];
        self.cells[final_row + first..final_row + columns]
            .iter()
            .map(|cell| i64::from(cell.matched))
            .max()
    }
}

fn boundary_bonus(previous: u8, current: u8) -> i32 {
    match previous {
        0 | b' ' | b'/' | b'\\' | b'|' | b'(' | b')' | b'[' | b']' | b'{' | b'}' => 8,
        b'!'..=b'\'' | b'*'..=b'.' | b':'..=b'@' | b'^'..=b'`' | b'~' => 7,
        b'A'..=b'Z' => 0,
        _ if current.is_ascii_uppercase() => 6,
        _ => 0,
    }
}

#[path = "fuzzy_score_cancellable.rs"]
mod cancellable;

#[cfg(test)]
#[path = "fuzzy_score_cancellable_tests.rs"]
mod cancellable_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};

    fn fresh_score(choice: &str, query: &str) -> Option<i64> {
        SkimMatcherV2::default().fuzzy_match(choice, query)
    }

    #[test]
    fn prepared_ascii_scores_match_upstream_across_reused_rows_queries_and_case() {
        let mut seed = 0x95d0_d173_9284_u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let alphabet = b"abAB09_-. /\\|()[]{}\0\t\r\n!@^`~\x7f";
        for case in 0..600 {
            let choice: String = (0..(next() % 80))
                .map(|_| char::from(alphabet[next() as usize % alphabet.len()]))
                .collect();
            let query: String = if case % 3 == 0 {
                choice
                    .bytes()
                    .step_by(1 + next() as usize % 10)
                    .take(8)
                    .map(char::from)
                    .collect()
            } else {
                (0..(next() % 9))
                    .map(|_| char::from(alphabet[next() as usize % alphabet.len()]))
                    .collect()
            };
            let mut scorer = PreparedFuzzyScore::new(&query);
            for target in [
                &choice,
                "src/Editor/code_view.rs",
                "aAB-ab_A9",
                "",
                "0/a.rs",
                &choice,
            ] {
                assert_eq!(
                    scorer.score(target),
                    fresh_score(target, &query),
                    "target={target:?} query={query:?}"
                );
            }
        }
    }

    #[test]
    fn unicode_and_empty_queries_keep_existing_scores() {
        for query in ["", "cf", "Café", "界", "🦀", "İ", "ä"] {
            let mut scorer = PreparedFuzzyScore::new(query);
            for target in ["src/界/Café.rs", "code.rs", "äa🦀İ", "", "界界"] {
                assert_eq!(
                    scorer.score(target),
                    SkimMatcherV2::default().fuzzy_match(target, query)
                );
            }
        }
    }

    #[test]
    fn single_scalar_and_irrelevant_long_suffix_keep_exact_scores_and_gap_penalties() {
        let choices = [
            format!("src/CamelCase.rs{}", "-irrelevant-tail".repeat(300)),
            format!(
                "a{}b{}c{}",
                "x".repeat(500),
                "_".repeat(300),
                "yz".repeat(400)
            ),
            format!("界/🦀/Café{}", "long-tail".repeat(300)),
        ];
        for query in [
            "c", "C", "界", "🦀", "abc", "CC", "src/", "cf", "Café", "a_b",
        ] {
            let mut scorer = PreparedFuzzyScore::new(query);
            for choice in &choices {
                assert_eq!(
                    scorer.score(choice),
                    fresh_score(choice, query),
                    "query={query:?}"
                );
            }
        }
    }

    #[test]
    fn exhaustive_small_ascii_choices_and_queries_keep_exact_scores() {
        fn words(max_len: u32) -> Vec<String> {
            let alphabet = b"ab A._";
            let mut words = Vec::new();
            for len in 0..=max_len {
                for mut code in 0..alphabet.len().pow(len) {
                    let mut word = String::new();
                    for _ in 0..len {
                        word.push(char::from(alphabet[code % alphabet.len()]));
                        code /= alphabet.len();
                    }
                    words.push(word);
                }
            }
            words
        }
        let choices = words(4);
        for query in words(3) {
            let mut scorer = PreparedFuzzyScore::new(&query);
            for choice in &choices {
                assert_eq!(
                    scorer.score(choice),
                    fresh_score(choice, &query),
                    "choice={choice:?} query={query:?}"
                );
            }
        }
    }

    #[test]
    fn mixed_unicode_scores_match_fresh_engine_and_ignore_prior_candidates() {
        let alphabet = [
            'a', 'A', 'ä', 'Ä', '界', '🦀', 'İ', '\u{307}', 'ß', 'Σ', ' ', '/', '_', '\u{2003}',
        ];
        let mut seed = 0xa34b_59d1_998a_u64;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            (seed >> 32) as usize
        };
        for case in 0..1_000 {
            let choice: String = (0..(next() % 60))
                .map(|_| alphabet[next() % alphabet.len()])
                .collect();
            let query: String = if case % 2 == 0 {
                choice.chars().step_by(1 + next() % 9).take(8).collect()
            } else {
                (0..(next() % 7))
                    .map(|_| alphabet[next() % alphabet.len()])
                    .collect()
            };
            let mut scorer = PreparedFuzzyScore::new(&query);
            for target in [&choice, "a/Ää-界🦀_i\u{307}", "ΣΣ/ß.rs", "", &choice] {
                assert_eq!(
                    scorer.score(target),
                    fresh_score(target, &query),
                    "target={target:?} query={query:?}"
                );
            }
        }
        let mut scorer = PreparedFuzzyScore::new("aaa");
        for target in ["a.a", "a_aa", "a a", "a/aa", "aaa"] {
            scorer.score(target);
        }
        assert_eq!(scorer.score("aaa"), Some(71));
    }

    #[test]
    #[ignore = "manual comparative release scoring benchmark"]
    fn benchmark_prepared_quick_open_scores() {
        let choices: Vec<_> = (0..50_000)
            .map(|index| format!("src/components/component-{index:05}.rs"))
            .collect();
        let upstream = SkimMatcherV2::default();
        for query in ["cmp", "s/cmp", "component-12345", "xyz"] {
            let mut verifier = PreparedFuzzyScore::new(query);
            let mut expected = (0usize, 0i64);
            for choice in &choices {
                let reference = fresh_score(choice, query);
                assert_eq!(verifier.score(choice), reference);
                if let Some(score) = reference {
                    expected.0 += 1;
                    expected.1 += score;
                }
            }
            let mut before = Vec::new();
            let mut after = Vec::new();
            for sample in 0..7 {
                for prepared in [sample % 2 == 0, sample % 2 != 0] {
                    let mut scorer = PreparedFuzzyScore::new(query);
                    let started = std::time::Instant::now();
                    let mut result = (0usize, 0i64);
                    for choice in std::hint::black_box(&choices) {
                        let score = if prepared {
                            scorer.score(choice)
                        } else {
                            upstream.fuzzy_match(choice, query)
                        };
                        if let Some(score) = score {
                            result.0 += 1;
                            result.1 += score;
                        }
                    }
                    let elapsed = started.elapsed();
                    // Cached upstream scores can depend on candidate history;
                    // match eligibility agrees, while prepared scores must also
                    // equal the separately checked fresh-engine oracle.
                    assert_eq!(result.0, expected.0);
                    if prepared {
                        assert_eq!(result, expected);
                    }
                    std::hint::black_box(result);
                    if prepared {
                        after.push(elapsed);
                    } else {
                        before.push(elapsed);
                    }
                }
            }
            before.sort();
            after.sort();
            eprintln!(
                "50k cached path scores query={query:?}: upstream={:?} prepared={:?}",
                before[3], after[3]
            );
        }
    }
}
