use rustc_hash::{FxHashMap, FxHashSet};

use crate::fuzzy_score::{FuzzyScoreOutcome, FuzzyWorkBudget, FuzzyWorkLimits, PreparedFuzzyScore};

use super::{VtCompleteCandidate, VtCompleteWorkOutcome, CompletionEvidence, CompletionResult};

pub(super) const MAX_COMPLETION_WORD_BYTES: usize = 4096;
pub(super) const MAX_QUERY_BYTES: usize = 64 * 1024;
pub(super) const MAX_REJECTED_WORDS: usize = 4096;
pub(super) const MAX_REJECTED_BYTES: usize = 256 * 1024;
const MAX_DEFERRED_WORDS: usize = 256;
const MAX_DEFERRED_BYTES: usize = 64 * 1024;

pub(super) fn work_limits() -> FuzzyWorkLimits {
    FuzzyWorkLimits {
        candidate_cells: 262_144,
        request_cells: 2_097_152,
        candidate_bytes: 65_536,
        request_bytes: 4 * 1024 * 1024,
        scratch_bytes: 2 * 1024 * 1024,
    }
}

pub(super) fn normalize_query(
    prefix: &str,
    budget: &mut FuzzyWorkBudget,
    cancelled: &impl Fn() -> bool,
) -> Option<Result<String, ()>> {
    if cancelled() {
        return None;
    }
    if prefix.len() > MAX_QUERY_BYTES || !budget.charge_bytes(prefix.len()) {
        return Some(Err(()));
    }
    // Rust's full-string lowercase retains contextual final-sigma behavior.
    // Unicode input is bounded to 64 KiB; ASCII conversion yields every4KiB.
    let lower = if prefix.is_ascii() {
        let mut bytes = Vec::with_capacity(prefix.len());
        for chunk in prefix.as_bytes().chunks(4096) {
            if cancelled() {
                return None;
            }
            bytes.extend(chunk.iter().map(u8::to_ascii_lowercase));
        }
        String::from_utf8(bytes).expect("ASCII lowercase remains UTF-8")
    } else {
        prefix.to_lowercase()
    };
    if cancelled() {
        return None;
    }
    if lower.len() > MAX_QUERY_BYTES || !budget.charge_bytes(lower.len()) {
        Some(Err(()))
    } else {
        Some(Ok(lower))
    }
}

enum CandidateScore {
    Scored(Option<(i64, bool)>),
    Cancelled,
    WorkLimited,
}

pub(super) struct Scores<'a> {
    prefix: &'a str,
    lower: &'a str,
    prefix_score: i64,
    ascii: bool,
    scorer: PreparedFuzzyScore<'a>,
    pub(super) budget: FuzzyWorkBudget,
    keyword_budget: FuzzyWorkBudget,
    pub(super) matches: FxHashMap<String, VtCompleteCandidate>,
    pub(super) rejected: FxHashSet<Box<str>>,
    pub(super) rejected_bytes: usize,
    pub(super) deferred: FxHashSet<Box<str>>,
    deferred_bytes: usize,
    pub(super) outcome: VtCompleteWorkOutcome,
    pub(super) deferred_candidates: u32,
    pub(super) contextual: bool,
}

impl<'a> Scores<'a> {
    pub(super) fn new(prefix: &'a str, lower: &'a str, budget: FuzzyWorkBudget) -> Self {
        let scalars = lower.chars().count();
        let underscores =
            lower.bytes().filter(|&b| b == b'_').count() - usize::from(lower.ends_with('_'));
        let prefix_score = if scalars == 0 {
            0
        } else {
            31 + 20 * (scalars as i64 - 1) + 3 * underscores as i64
        };
        Self {
            prefix,
            lower,
            prefix_score,
            ascii: lower.is_ascii(),
            scorer: PreparedFuzzyScore::new(lower),
            budget,
            keyword_budget: FuzzyWorkBudget::new(FuzzyWorkLimits {
                request_cells: 65_536,
                request_bytes: 128 * 1024,
                ..work_limits()
            }),
            matches: FxHashMap::default(),
            rejected: FxHashSet::default(),
            rejected_bytes: 0,
            deferred: FxHashSet::default(),
            deferred_bytes: 0,
            outcome: VtCompleteWorkOutcome::Exact,
            deferred_candidates: 0,
            contextual: false,
        }
    }

    pub(super) fn defer_candidate(&mut self) {
        self.outcome = VtCompleteWorkOutcome::FuzzyWorkLimited;
        self.deferred_candidates = self.deferred_candidates.saturating_add(1);
    }

    pub(super) fn charge_scan_bytes(&mut self, bytes: usize) -> bool {
        if self.budget.charge_bytes(bytes) {
            true
        } else {
            self.outcome = VtCompleteWorkOutcome::FuzzyWorkLimited;
            false
        }
    }

    pub(super) fn add(
        &mut self,
        candidate: &str,
        proximity: usize,
        keyword: bool,
        cancelled: &impl Fn() -> bool,
    ) -> Option<()> {
        if cancelled() {
            return None;
        }
        if candidate.len() > MAX_COMPLETION_WORD_BYTES {
            self.defer_candidate();
            return Some(());
        }
        if candidate.len() < 2
            || (!self.contextual
                && (candidate == self.prefix || candidate.eq_ignore_ascii_case(self.prefix)))
        {
            return Some(());
        }
        if let Some(entry) = self.matches.get_mut(candidate) {
            entry.proximity = entry.proximity.min(proximity);
            entry.keyword |= keyword;
            return Some(());
        }
        if self.rejected.contains(candidate) || (!keyword && self.deferred.contains(candidate)) {
            return Some(());
        }

        let outcome = {
            let budget = if keyword {
                &mut self.keyword_budget
            } else {
                &mut self.budget
            };
            let mut work = budget.begin_candidate();
            if !work.charge_bytes(candidate.len()) {
                CandidateScore::WorkLimited
            } else {
                // A lowercase ASCII prefix is independent of Unicode/casing in
                // the suffix. Its exact fresh score needs no suffix decoding.
                let mut ascii_prefix = self.ascii && candidate.len() >= self.lower.len();
                if ascii_prefix {
                    for (raw, expected) in candidate.as_bytes()[..self.lower.len()]
                        .chunks(4096)
                        .zip(self.lower.as_bytes().chunks(4096))
                    {
                        if cancelled() {
                            return None;
                        }
                        if !raw.eq_ignore_ascii_case(expected) {
                            ascii_prefix = false;
                            break;
                        }
                    }
                }
                if ascii_prefix {
                    CandidateScore::Scored(Some((self.prefix_score, true)))
                } else {
                    let lower = candidate.to_lowercase(); // bounded4KiB input, contextual Unicode casing
                    if cancelled() {
                        return None;
                    }
                    if !work.charge_bytes(lower.len()) {
                        CandidateScore::WorkLimited
                    } else if lower.starts_with(self.lower) {
                        CandidateScore::Scored(Some((self.prefix_score, true)))
                    } else {
                        match self.scorer.score_cancellable(&lower, &mut work, cancelled) {
                            FuzzyScoreOutcome::Scored(score) => {
                                CandidateScore::Scored(score.map(|score| (score, false)))
                            }
                            FuzzyScoreOutcome::Cancelled => CandidateScore::Cancelled,
                            FuzzyScoreOutcome::WorkLimited => CandidateScore::WorkLimited,
                        }
                    }
                }
            }
        };
        match outcome {
            CandidateScore::Cancelled => return None,
            CandidateScore::WorkLimited => {
                self.defer_candidate();
                if self.deferred.len() < MAX_DEFERRED_WORDS
                    && candidate.len() <= MAX_DEFERRED_BYTES.saturating_sub(self.deferred_bytes)
                    && self.deferred.insert(candidate.into())
                {
                    self.deferred_bytes += candidate.len();
                }
            }
            CandidateScore::Scored(None)
                if self.contextual && super::identifier::typo_prefix(candidate, self.prefix) =>
            {
                self.matches.insert(
                    candidate.to_owned(),
                    VtCompleteCandidate {
                        proximity,
                        fuzzy_score: self.prefix_score - 95,
                        prefix_match: false,
                        keyword,
                    },
                );
            }
            CandidateScore::Scored(None) => {
                if self.rejected.len() < MAX_REJECTED_WORDS
                    && candidate.len() <= MAX_REJECTED_BYTES.saturating_sub(self.rejected_bytes)
                    && self.rejected.insert(candidate.into())
                {
                    self.rejected_bytes += candidate.len();
                }
            }
            CandidateScore::Scored(Some((fuzzy_score, prefix_match))) => {
                self.matches.insert(
                    candidate.to_owned(),
                    VtCompleteCandidate {
                        proximity,
                        fuzzy_score,
                        prefix_match,
                        keyword,
                    },
                );
            }
        }
        Some(())
    }

    #[cfg(test)]
    pub(super) fn finish(self) -> CompletionResult {
        let mut sorted: Vec<_> = self.matches.into_iter().collect();
        let compare = |a: &(String, VtCompleteCandidate), b: &(String, VtCompleteCandidate)| {
            b.1.keyword
                .cmp(&a.1.keyword)
                .then_with(|| b.1.prefix_match.cmp(&a.1.prefix_match))
                .then_with(|| b.1.fuzzy_score.cmp(&a.1.fuzzy_score))
                .then_with(|| a.1.proximity.cmp(&b.1.proximity))
                .then_with(|| a.0.cmp(&b.0))
        };
        if sorted.len() > 10 {
            sorted.select_nth_unstable_by(10, compare);
            sorted.truncate(10);
        }
        sorted.sort_unstable_by(compare);
        CompletionResult {
            details: Vec::new(),
            suggestions: sorted.into_iter().map(|(word, _)| word).collect(),
            outcome: self.outcome,
            deferred_candidates: self.deferred_candidates,
        }
    }

    pub(super) fn finish_contextual(
        self,
        evidence: FxHashMap<String, CompletionEvidence>,
        prefix: &str,
        language: &str,
        context: crate::completion_context::Context,
        explicit: bool,
        learned: &crate::completion_index::examples::Evidence,
        generated: &super::patterns::Predictions,
    ) -> CompletionResult {
        use super::identifier::Match;
        use crate::app_vtcomplete::{CompletionDetail, CompletionKind as Kind};
        #[cfg(test)]
        super::learning_replay_tests::trace_candidates(self.matches.keys().map(String::as_str));
        let vocabulary_mass: f64 = self
            .matches
            .keys()
            .map(|w| {
                crate::completion_index::examples::workspace_count(
                    evidence.get(w).map_or(0, |e| e.frequency),
                )
            })
            .sum();
        let file_mass: f64 = evidence
            .values()
            .map(|e| f64::from(e.file_frequency.min(16)))
            .sum();
        // Conditioning on the typed prefix can leave no compatible observed
        // continuation. That is no ranking evidence for these candidates.
        let conditional_support = self.matches.keys().any(|w| learned.probability(w, 0.) > 0.);
        let edit_candidates = evidence.values().filter(|e| e.edit_history).count();
        let statistical = conditional_support
            || generated.active()
            || edit_candidates > 0
            || file_mass > 0. && crate::completion_index::examples::file_prior_without_context();
        let log_scale = crate::completion_index::examples::likelihood_scale();
        let mut sorted: Vec<_> = self
            .matches
            .into_iter()
            .map(|(word, candidate)| {
                let evidence = evidence.get(&word).copied().unwrap_or_default();
                let frequency = 32 - evidence.frequency.max(1).leading_zeros();
                let mut rank = candidate.fuzzy_score
                    + i64::from(candidate.prefix_match) * 80
                    + evidence.local_context_score.clamp(-100, 200)
                    + evidence.context_score.clamp(0, 320)
                    + evidence.semantic_score.clamp(-220, 430)
                    + evidence.history_score.clamp(0, 94)
                    + i64::from(frequency.min(12)) * 2
                    + i64::from(evidence.local) * 32
                    + i64::from(candidate.keyword) * 4;
                if word.starts_with(prefix) {
                    rank += 24;
                }
                if boundary_abbreviation(&word, prefix) {
                    rank += 52;
                }
                if context.type_position && evidence.type_position {
                    rank += 75;
                }
                if statistical {
                    let workspace =
                        crate::completion_index::examples::workspace_count(evidence.frequency)
                            / vocabulary_mass.max(1.);
                    let prior = (f64::from(evidence.file_frequency.min(16)) + 2. * workspace)
                        / (file_mass + 2.);
                    let mut probability =
                        generated.probability(&word, learned.probability(&word, prior));
                    if edit_candidates > 0 {
                        // Two independent committed changes supply temporal
                        // evidence; one unit of backoff preserves other sources.
                        let count = if evidence.edit_history { 2. } else { 0. };
                        probability = (count + probability)
                            / (2. * edit_candidates as f64 + 1.);
                    }
                    // Conditional estimates replace contextual bonuses. The
                    // measured log scale retains spelling and deliberate UX.
                    rank = candidate.fuzzy_score
                        + i64::from(candidate.prefix_match) * 80
                        + evidence.history_score.clamp(0, 94)
                        + (log_scale * probability.max(1e-12).ln()) as i64;
                }
                let callable = if context.declaration
                    || (evidence.semantic_seen || evidence.lexical_seen)
                        && evidence.domain != crate::completion_context::Domain::Code
                {
                    false
                } else if evidence.local_matched_uses > 0 {
                    evidence.local_matched_calls >= 2
                        && evidence.local_matched_calls.saturating_mul(3)
                            >= evidence.local_matched_uses.saturating_mul(2)
                } else if context.member() && !evidence.project_receiver {
                    false
                } else if evidence.example_reference {
                    // A compatible callback/value role refines unconditional
                    // calls of the same identifier in other contexts. One
                    // positive call with insufficient support is not a veto.
                    false
                } else if evidence.code_uses > 0 {
                    // Streaming state crosses line boundaries. Standalone line
                    // observations cannot distinguish a multiline prose body.
                    evidence.code_calls >= 2
                        && evidence.code_calls.saturating_mul(3)
                            >= evidence.code_uses.saturating_mul(2)
                } else if let Some(callable) = evidence.example_callable {
                    // Names may be learned from raw agnostic token windows;
                    // call rendering also needs code-domain corroboration.
                    callable && evidence.project_callable && evidence.calls >= 2
                } else if evidence.matched_uses > 0 {
                    evidence.matched_calls >= 2
                        && evidence.matched_calls.saturating_mul(3)
                            >= evidence.matched_uses.saturating_mul(2)
                } else {
                    evidence.project_callable
                        && evidence.calls >= 2
                        && evidence.calls.saturating_mul(2) >= evidence.frequency
                };
                let (kind, annotation) = if callable {
                    (Kind::Function, "call")
                } else if context.member()
                    && (evidence.semantic_score >= 100 || evidence.project_receiver)
                {
                    (Kind::Member, "member")
                } else if context.keys[4] != 0 && evidence.semantic_score >= 150 {
                    (Kind::Value, "value")
                } else if context.keys[2] != 0 && evidence.semantic_score >= 150 {
                    (Kind::Argument, "argument")
                } else if evidence.type_position {
                    (Kind::Type, "type")
                } else if evidence.declaration && evidence.semantic_score > 0 {
                    (Kind::Variable, "local")
                } else if candidate.keyword {
                    (Kind::Keyword, "keyword")
                } else if evidence.local {
                    (Kind::Word, "nearby")
                } else {
                    (Kind::Word, "project")
                };
                (
                    word,
                    candidate.proximity,
                    callable,
                    rank,
                    CompletionDetail {
                        kind,
                        annotation: if evidence.edit_history {
                            "edit"
                        } else if evidence.history_score > 0 {
                            "recent"
                        } else if let Some(kind) = evidence.generated {
                            match kind {
                                super::patterns::Kind::Sequence => "sequence",
                                super::patterns::Kind::Transform
                                | super::patterns::Kind::Relation => "pattern",
                                super::patterns::Kind::Stem => "stem",
                            }
                        } else if evidence.example_seen {
                            "example"
                        } else {
                            annotation
                        },
                        context,
                    },
                    evidence,
                    Match::Fuzzy,
                )
            })
            .collect();
        let compare = |a: &(
            String,
            usize,
            bool,
            i64,
            CompletionDetail,
            CompletionEvidence,
            Match,
        ),
                       b: &(
            String,
            usize,
            bool,
            i64,
            CompletionDetail,
            CompletionEvidence,
            Match,
        )| {
            b.3.cmp(&a.3)
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.0.len().cmp(&b.0.len()))
                .then_with(|| a.0.cmp(&b.0))
        };
        // Full identifier alignment only reranks a small relevant shortlist.
        // At most 64 × 48 × 160 cells are visited, independent of project size.
        if sorted.len() > 64 {
            sorted.select_nth_unstable_by(64, compare);
            sorted.truncate(64);
        }
        for row in &mut sorted {
            let (quality, class) = super::identifier::quality(&row.0, prefix);
            row.3 += quality;
            row.6 = class;
        }
        // Stronger evidence in the current source/neighborhood should not be
        // padded with much weaker matches from independent programs. This is a
        // relative relevance cutoff, not a file or language eligibility rule:
        // a better project-wide pattern can still win from any source.
        let grounded = sorted
            .iter()
            .filter(|row| {
                (row.5.local || row.5.example_seen)
                    && row.5.domain == context.domain
                    && (row.0 != prefix || row.2)
            })
            .map(|row| row.3)
            .max();
        // File unigrams alone did not improve ranking in replay. They still
        // supply a useful confidence estimate for keeping unrelated project
        // words out of an automatic popup, independently of fuzzy ordering.
        let file_only_confidence = if !statistical && file_mass > 0. {
            sorted
                .iter()
                .filter(|row| {
                    row.5.local && row.5.domain == context.domain && (row.0 != prefix || row.2)
                })
                .map(|row| {
                    let workspace =
                        crate::completion_index::examples::workspace_count(row.5.frequency)
                            / vocabulary_mass.max(1.);
                    (f64::from(row.5.file_frequency.min(16)) + 2. * workspace) / (file_mass + 2.)
                })
                .reduce(f64::max)
        } else {
            None
        };
        let focused = context.structural()
            && sorted.iter().any(|row| {
                row.5.semantic_score >= 200 || row.5.project_receiver || row.5.example_seen
            });
        // A statistical rank is expressed in log-probability units. Keep the
        // relative popup threshold on that same scale rather than inheriting
        // the much larger gap used by the heuristic fallback.
        let automatic_gap = if statistical {
            (log_scale / 3.).ceil() as i64
        } else {
            64
        };
        sorted.retain(|row| {
            if row.0 == prefix && !row.2 {
                return false;
            }
            if explicit {
                return true;
            }
            if !row.5.local
                && row.5.example_probability < 0.5
                && row.5.history_score < 60
                && row.4.kind != Kind::Keyword
                && if let Some(best) = file_only_confidence {
                    let workspace =
                        crate::completion_index::examples::workspace_count(row.5.frequency)
                            / vocabulary_mass.max(1.);
                    let prior = (f64::from(row.5.file_frequency.min(16)) + 2. * workspace)
                        / (file_mass + 2.);
                    best.ln() - prior.max(1e-12).ln() > 1. / 3.
                } else {
                    grounded.is_some_and(|best| best - row.3 > automatic_gap)
                }
            {
                return false;
            }
            if context.domain == crate::completion_context::Domain::Code
                && (row.5.semantic_seen || row.5.lexical_seen)
                && row.5.domain != context.domain
            {
                return false;
            }
            if row.6 == Match::Typo && row.5.semantic_score < 180 && row.5.example_probability < 0.5
            {
                return false;
            }
            if prefix.chars().count() == 1
                && row.6 == Match::Fuzzy
                && row.5.semantic_score < 150
                && row.5.example_probability < 0.5
            {
                return false;
            }
            if focused
                && !row.5.edit_history
                && row.5.semantic_score < 70
                && !row.5.project_receiver
                && !row.5.example_seen
                && (prefix.is_empty() || row.6 <= Match::Fuzzy)
            {
                return false;
            }
            if prefix.is_empty()
                && !row.5.edit_history
                && context.member()
                && row.5.semantic_score < 100
                && !row.5.project_receiver
                && !row.5.example_seen
            {
                return false;
            }
            true
        });
        if context.domain == crate::completion_context::Domain::Code
            && !context.member()
            && !context.declaration
            && !prefix.is_empty()
        {
            for &trigger in crate::editor_snippets::snippet_triggers(language) {
                let exact = trigger == prefix;
                if !exact
                    && (!explicit && prefix.chars().count() < 2 || !trigger.starts_with(prefix))
                {
                    continue;
                }
                let rank = if exact {
                    100_000
                } else if statistical {
                    // Partial snippet rows share the predictor's probability
                    // scale. Exact triggers retain their deliberate priority.
                    let item = evidence.get(trigger).copied().unwrap_or_default();
                    let count = crate::completion_index::examples::workspace_count(item.frequency);
                    let mass = vocabulary_mass
                        + if sorted.iter().any(|row| row.0 == trigger) {
                            0.
                        } else {
                            count
                        };
                    let workspace = count / mass.max(1.);
                    let prior = (f64::from(item.file_frequency.min(16)) + 2. * workspace)
                        / (file_mass + 2.);
                    105 + prefix.len() as i64 * 24
                        + (log_scale
                            * generated
                                .probability(trigger, learned.probability(trigger, prior))
                                .max(1e-12)
                                .ln()) as i64
                } else {
                    105 + prefix.len() as i64 * 24
                };
                sorted.retain(|row| row.0 != trigger);
                sorted.push((
                    trigger.into(),
                    0,
                    false,
                    rank,
                    CompletionDetail {
                        kind: Kind::Snippet,
                        annotation: "snippet",
                        context,
                    },
                    CompletionEvidence::default(),
                    Match::Prefix,
                ));
            }
        }
        sorted.sort_unstable_by(compare);
        sorted.truncate(10);
        let details = sorted.iter().map(|row| row.4.clone()).collect();
        CompletionResult {
            details,
            suggestions: sorted
                .into_iter()
                .map(
                    |(word, _, callable, _, _, _, _)| {
                        if callable { format!("{word}()") } else { word }
                    },
                )
                .collect(),
            outcome: self.outcome,
            deferred_candidates: self.deferred_candidates,
        }
    }
}

/// Reward abbreviation matches at camelCase, snake_case, and acronym word
/// boundaries. Matching remains Unicode-aware and uses no per-word scratch.
fn boundary_abbreviation(word: &str, prefix: &str) -> bool {
    let lower = prefix.to_lowercase();
    let mut query = lower.chars();
    let mut wanted = query.next();
    let mut previous = None;
    let mut matched = 0usize;
    let mut chars = word.chars().peekable();
    while let Some(ch) = chars.next() {
        let boundary = previous.is_none_or(|before: char| {
            before == '_'
                || before == '-'
                || before == '$'
                || (before.is_lowercase() && ch.is_uppercase())
                || (before.is_uppercase()
                    && ch.is_uppercase()
                    && chars.peek().is_some_and(|next| next.is_lowercase()))
        });
        if boundary && ch.to_lowercase().next() == wanted {
            matched += 1;
            wanted = query.next();
            if wanted.is_none() {
                return matched >= 2;
            }
        }
        previous = Some(ch);
    }
    false
}
