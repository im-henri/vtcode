//! Session-local directional intent from committed identifier replacements.
//! Query snapshots coalesce keystrokes; distinct edit sites supply evidence.
//! Nothing is persisted, indexed, or read from disk.
use super::VtCompleteSnapshot;
use crate::completion_context::{Analyzer, Domain};
use ropey::Rope;

const DOCUMENTS: usize = 4;
const CHANGES: usize = 24;
const WORD_BYTES: usize = 160;
const MAX_AGE: u64 = 256;

#[derive(Clone)]
struct Change {
    before: String,
    after: String,
    site: usize,
    used: u64,
}

#[derive(Clone)]
struct Pending {
    before: String,
    after: String,
    site: usize,
    predicted: bool,
}

#[derive(Clone)]
struct Document {
    id: u64,
    rope: Rope,
    changes: Vec<Change>,
    pending: Option<Pending>,
    predictions: Vec<String>,
    clock: u64,
    used: u64,
}

#[derive(Clone, Default)]
pub(super) struct Cache {
    documents: Vec<Document>,
    clock: u64,
}

fn word_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '$')
}

fn token(rope: &Rope, at: usize, end: usize) -> Option<(usize, String)> {
    let mut first = at;
    let mut last = end;
    while first > 0 && at - first <= WORD_BYTES && word_char(rope.char(first - 1)) {
        first -= 1;
    }
    while last < rope.len_chars() && last - first <= WORD_BYTES && word_char(rope.char(last)) {
        last += 1;
    }
    if last - first > WORD_BYTES {
        return None;
    }
    let text = rope.slice(first..last).to_string();
    (text.len() <= WORD_BYTES && text.chars().all(word_char)).then_some((first, text))
}

fn code_at(rope: &Rope, at: usize, cancelled: &impl Fn() -> bool) -> Option<bool> {
    let line = rope.char_to_line(at);
    let start = rope.line_to_char(line.saturating_sub(128));
    let start = start.max(at.saturating_sub(48 * 1024));
    let mut analyzer = Analyzer::at(start);
    for text in rope.slice(start..at).chunks() {
        if cancelled() { return None; }
        analyzer.feed(text, |_| {});
    }
    analyzer.flush(|_| {});
    Some(analyzer.context().domain == Domain::Code)
}

// Identifier components, including acronym boundaries, retain byte boundaries.
fn components(word: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    let mut previous = None;
    let mut chars = word.char_indices().peekable();
    while let Some((at, ch)) = chars.next() {
        let separator = matches!(ch, '_' | '$');
        let boundary = previous.is_some_and(|before: char| {
            before.is_lowercase() && ch.is_uppercase()
                || before.is_numeric() != ch.is_numeric()
                || before.is_uppercase()
                    && ch.is_uppercase()
                    && chars.peek().is_some_and(|(_, next)| next.is_lowercase())
        });
        if separator || boundary {
            if let Some(first) = start.take() {
                spans.push((first, at));
            }
        }
        if !separator && start.is_none() {
            start = Some(at);
        }
        previous = (!separator).then_some(ch);
    }
    if let Some(first) = start {
        spans.push((first, word.len()));
    }
    spans
}

fn rule(before: &str, after: &str) -> Option<(String, String)> {
    let left = components(before);
    let right = components(after);
    if left.len() != right.len() || left.is_empty() {
        return None;
    }
    let mut different = None;
    for ((a, b), (c, d)) in left.into_iter().zip(right) {
        if before[a..b] != after[c..d] {
            if different.is_some() {
                return None;
            }
            let from = before[a..b].to_lowercase();
            let to = after[c..d].to_lowercase();
            if from == to || from.len() < 2 || to.len() < 2 {
                return None;
            }
            different = Some((from, to));
        }
    }
    different
}

fn transform(word: &str, from: &str, to: &str) -> Option<String> {
    let mut found = None;
    for (start, end) in components(word) {
        let component = &word[start..end];
        if component.to_lowercase() != from {
            continue;
        }
        if found.is_some() {
            return None;
        }
        let replacement = if component.chars().all(char::is_uppercase) {
            to.to_uppercase()
        } else if component.chars().all(char::is_lowercase) {
            to.to_owned()
        } else {
            let mut chars = component.chars();
            if !chars.next()?.is_uppercase() || !chars.all(char::is_lowercase) {
                return None;
            }
            let mut chars = to.chars();
            let mut replacement = chars.next()?.to_uppercase().collect::<String>();
            replacement.push_str(chars.as_str());
            replacement
        };
        found = Some(format!("{}{}{}", &word[..start], replacement, &word[end..]));
    }
    found.filter(|result| result.len() <= WORD_BYTES)
}

impl Document {
    fn commit(&mut self) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        if pending.predicted
            || pending.before.len() < 2
            || pending.after.len() < 2
            || pending.before.starts_with(&pending.after)
            || pending.after.starts_with(&pending.before)
            || !pending.before.chars().all(word_char)
            || !pending.after.chars().all(word_char)
        {
            // An edited/deleted witness no longer proves its old replacement,
            // even when this operation cannot teach another rule.
            self.changes.retain(|change| change.site != pending.site);
            return;
        }
        // A reversal withdraws the earlier intent. Undo must never train its
        // inverse as another independent positive example.
        let reverse = rule(&pending.after, &pending.before);
        let reversal = self.changes.iter().any(|change| {
            change.before == pending.after && change.after == pending.before
                || reverse.is_some() && rule(&change.before, &change.after) == reverse
        });
        if reversal {
            self.changes.retain(|change| {
                !(change.before == pending.after && change.after == pending.before
                    || reverse.is_some() && rule(&change.before, &change.after) == reverse)
            });
            return;
        }
        // Revisiting a site replaces its evidence; polling and edit loops never
        // manufacture additional witnesses.
        self.changes.retain(|change| change.site != pending.site);
        if self.changes.len() == CHANGES {
            self.changes.remove(0);
        }
        self.changes.push(Change {
            before: pending.before,
            after: pending.after,
            site: pending.site,
            used: self.clock,
        });
    }

    fn moved_away(&mut self, snapshot: &VtCompleteSnapshot, prefix: &str) {
        let cursor = snapshot.rope.line_to_char(snapshot.cursor_line) + snapshot.cursor_col;
        let start = cursor.saturating_sub(prefix.chars().count());
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.site != start)
        {
            self.commit();
        }
    }
}

impl Cache {
    pub(super) fn missing(&self, document_id: u64) -> bool {
        self.documents
            .iter()
            .all(|document| document.id != document_id)
    }

    /// One existing undo checkpoint captures the first replacement even when
    /// no completion query has observed the document before this keystroke.
    pub(super) fn seed(
        &mut self,
        snapshot: &VtCompleteSnapshot,
        baseline: &Rope,
        cancelled: &impl Fn() -> bool,
    ) -> Option<()> {
        if self.missing(snapshot.document_id)
            && baseline.len_bytes() <= super::MAX_DOCUMENT_BYTES
            && baseline.len_lines() <= super::MAX_DOCUMENT_LINES
        {
            let baseline = VtCompleteSnapshot {
                rope: baseline.clone(),
                cursor_line: 0,
                cursor_col: 0,
                ..snapshot.clone()
            };
            self.observe(&baseline, "", cancelled)?;
        }
        Some(())
    }

    pub(super) fn observe(
        &mut self,
        snapshot: &VtCompleteSnapshot,
        prefix: &str,
        cancelled: &impl Fn() -> bool,
    ) -> Option<()> {
        if cancelled() {
            return None;
        }
        if snapshot.rope.len_bytes() > super::MAX_DOCUMENT_BYTES
            || snapshot.rope.len_lines() > super::MAX_DOCUMENT_LINES
        {
            self.documents
                .retain(|document| document.id != snapshot.document_id);
            return Some(());
        }
        self.clock = self.clock.saturating_add(1);
        let index = self
            .documents
            .iter()
            .position(|document| document.id == snapshot.document_id);
        let Some(index) = index else {
            if self.documents.len() == DOCUMENTS {
                let oldest = self
                    .documents
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, d)| d.used)
                    .unwrap()
                    .0;
                self.documents.swap_remove(oldest);
            }
            self.documents.push(Document {
                id: snapshot.document_id,
                rope: snapshot.rope.clone(),
                changes: Vec::new(),
                pending: None,
                predictions: Vec::new(),
                clock: 0,
                used: self.clock,
            });
            return Some(());
        };
        let document = &mut self.documents[index];
        document.used = self.clock;
        let (old, new) = crate::rope_changes::changed_chars_cancellable(&document.rope, &snapshot.rope, cancelled)?;
        if cancelled() {
            return None;
        }
        if !old.is_empty() || !new.is_empty() {
            document.clock = document.clock.saturating_add(1);
            document
                .changes
                .retain(|change| document.clock.saturating_sub(change.used) < MAX_AGE);
            if old.len() > WORD_BYTES || new.len() > WORD_BYTES {
                // Bulk replacements/undo can reverse several witnesses at once.
                document.changes.clear();
                document.pending = None;
            } else {
                let before = token(&document.rope, old.start, old.end);
                let after = token(&snapshot.rope, new.start, new.end);
                if let (Some((site, before)), Some((new_site, after))) = (before, after) {
                    let same_pending = document
                        .pending
                        .as_ref()
                        .is_some_and(|pending| pending.site == site && pending.after == before);
                    if same_pending {
                        let pending = document.pending.as_mut().unwrap();
                        pending.after = after.clone();
                        pending.predicted |= document.predictions.contains(&after);
                    } else {
                        document.commit();
                        if site == new_site
                            && before.len() >= 2
                            && !after.starts_with(&before)
                            && code_at(&document.rope, site, cancelled)?
                            && code_at(&snapshot.rope, new_site, cancelled)?
                        {
                            document.pending = Some(Pending {
                                before,
                                predicted: document.predictions.contains(&after),
                                after,
                                site: new_site,
                            });
                        }
                    }
                } else {
                    if !old.is_empty() {
                        // Short multi-token undo can reverse multiple witnesses.
                        document.changes.clear();
                        document.pending = None;
                    } else {
                        document.commit();
                    }
                }
                let map = |site: &mut usize| {
                    if *site > old.start && *site >= old.end {
                        *site = new.end + site.saturating_sub(old.end);
                    }
                };
                for change in &mut document.changes {
                    map(&mut change.site);
                }
                if let Some(pending) = &mut document.pending
                    && pending.site != new.start
                {
                    map(&mut pending.site);
                }
            }
            document.rope = snapshot.rope.clone();
        }
        document.moved_away(snapshot, prefix);
        Some(())
    }

    pub(super) fn predict(&mut self, snapshot: &VtCompleteSnapshot, prefix: &str) -> Vec<String> {
        let Some(document) = self
            .documents
            .iter_mut()
            .find(|document| document.id == snapshot.document_id)
        else {
            return Vec::new();
        };
        document.predictions.clear();
        if prefix.chars().count() < 2 || document.changes.len() < 2 {
            return Vec::new();
        }
        let cursor = snapshot.rope.line_to_char(snapshot.cursor_line) + snapshot.cursor_col;
        // A small visible vocabulary supplies source names for unseen outputs.
        let start = cursor.saturating_sub(4096);
        let end = (cursor + 4096).min(snapshot.rope.len_chars());
        let visible = snapshot.rope.slice(start..end).to_string();
        let mut words = Vec::new();
        let mut analyzer = Analyzer::at(start);
        let mut collect = |event: crate::completion_context::Event<'_>| {
            if let crate::completion_context::Event::Word { word, context, .. } = event
                && context.domain == Domain::Code
                && words.len() < 512
            {
                words.push(word.to_owned());
            }
        };
        analyzer.feed(&visible, &mut collect);
        analyzer.flush(collect);
        words.sort_unstable();
        words.dedup();
        let mut candidates = Vec::new();
        let mut applied = Vec::new();
        for change in &document.changes {
            if candidates.len() == 8 {
                break;
            }
            if document
                .changes
                .iter()
                .any(|other| other.before == change.before && other.after != change.after)
            {
                continue;
            }
            let literal_support = document
                .changes
                .iter()
                .filter(|other| other.before == change.before && other.after == change.after)
                .count();
            if literal_support >= 2
                && change.after.starts_with(prefix)
                && !candidates.contains(&change.after)
            {
                candidates.push(change.after.clone());
            }
            let Some((from, to)) = rule(&change.before, &change.after) else {
                continue;
            };
            let distinct = document.changes.iter().any(|other| {
                other.before != change.before
                    && rule(&other.before, &other.after).as_ref()
                        == Some(&(from.clone(), to.clone()))
            });
            if !distinct {
                continue;
            }
            // Conflicting replacement directions abstain instead of arbitrarily
            // picking whichever edit happened to be most recent.
            if document.changes.iter().any(|other| {
                rule(&other.before, &other.after).is_some_and(|(a, b)| a == from && b != to)
            }) {
                continue;
            }
            if applied.contains(&(from.clone(), to.clone())) {
                continue;
            }
            applied.push((from.clone(), to.clone()));
            for word in &words {
                if candidates.len() == 8 {
                    break;
                }
                if word.len() > WORD_BYTES {
                    continue;
                }
                if let Some(candidate) = transform(word, &from, &to)
                    && candidate.starts_with(prefix)
                    && candidate != prefix
                    && !candidates.contains(&candidate)
                {
                    candidates.push(candidate);
                }
            }
        }
        candidates.sort_unstable();
        candidates.dedup();
        candidates.truncate(8);
        document.predictions = candidates.clone();
        candidates
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(text: &str, cursor: usize) -> VtCompleteSnapshot {
        VtCompleteSnapshot {
            source: Default::default(),
            rope: Rope::from_str(text),
            document_id: 7,
            cursor_line: 0,
            cursor_col: cursor,
            active: true,
        }
    }

    fn observe(cache: &mut Cache, text: &str, cursor: usize, prefix: &str) {
        cache
            .observe(&snapshot(text, cursor), prefix, &|| false)
            .unwrap();
    }

    fn trained() -> (Cache, String) {
        let mut cache = Cache::default();
        let text = "load_user(); save_user(); decode_user(); de";
        observe(&mut cache, text, 4, "load");
        let first = text.replacen("load_user", "load_customer", 1);
        observe(&mut cache, &first, 13, "load_customer");
        observe(&mut cache, &first, 20, "save");
        let second = first.replacen("save_user", "save_customer", 1);
        observe(&mut cache, &second, 30, "save_customer");
        let cursor = second.len();
        observe(&mut cache, &second, cursor, "de");
        (cache, second)
    }

    #[test]
    fn independent_committed_edits_generate_a_new_component_transformation() {
        let (mut cache, text) = trained();
        assert!(
            cache
                .predict(&snapshot(&text, text.len()), "de")
                .contains(&"decode_customer".into())
        );
        assert!(!text.contains("decode_customer"));
    }

    #[test]
    fn typing_is_coalesced_and_repeated_queries_do_not_supply_witnesses() {
        let mut cache = Cache::default();
        let text = "load_user(); decode_user(); de";
        observe(&mut cache, text, 4, "load");
        for replacement in ["load_c", "load_cu", "load_custom", "load_customer"] {
            let text = text.replacen("load_user", replacement, 1);
            observe(&mut cache, &text, replacement.len(), replacement);
        }
        let text = text.replacen("load_user", "load_customer", 1);
        for _ in 0..12 {
            observe(&mut cache, &text, text.len(), "de");
        }
        assert_eq!(cache.documents[0].changes.len(), 1);
        assert_eq!(cache.documents[0].changes[0].before, "load_user");
        assert_eq!(cache.documents[0].changes[0].after, "load_customer");
        assert!(cache.predict(&snapshot(&text, text.len()), "de").is_empty());
    }

    #[test]
    fn reversal_withdraws_intent_without_learning_the_inverse() {
        let (mut cache, text) = trained();
        let reversed = text.replacen("load_customer", "load_user", 1);
        observe(&mut cache, &reversed, reversed.len(), "de");
        assert!(
            cache
                .predict(&snapshot(&reversed, reversed.len()), "de")
                .is_empty()
        );
        assert!(cache.documents[0].changes.is_empty());
    }

    #[test]
    fn comments_strings_and_ordinary_prefix_typing_do_not_train_transformations() {
        for text in [
            "// load_user save_user decode_user de",
            "\"load_user save_user decode_user de\"",
        ] {
            let mut cache = Cache::default();
            observe(&mut cache, text, 0, "");
            let first = text.replacen("load_user", "load_customer", 1);
            observe(&mut cache, &first, 0, "");
            let second = first.replacen("save_user", "save_customer", 1);
            observe(&mut cache, &second, 0, "");
            assert!(cache.documents[0].changes.is_empty());
        }
        let mut cache = Cache::default();
        observe(&mut cache, "lo", 2, "lo");
        observe(&mut cache, "load_user", 9, "load_user");
        observe(&mut cache, "load_user; de", 13, "de");
        assert!(cache.documents[0].changes.is_empty());
    }

    #[test]
    fn rules_preserve_component_case_and_abstain_on_partial_components() {
        assert_eq!(
            transform("load_USER", "user", "customer"),
            Some("load_CUSTOMER".into())
        );
        assert_eq!(
            transform("decodeUserId", "user", "customer"),
            Some("decodeCustomerId".into())
        );
        assert_eq!(transform("get_username", "user", "customer"), None);
        assert_eq!(transform("user_user", "user", "customer"), None);
        assert_eq!(transform("get_õun", "õun", "pirn"), Some("get_pirn".into()));
    }

    #[test]
    fn canceled_observation_and_other_documents_cannot_supply_evidence() {
        let (mut cache, text) = trained();
        let mut other = snapshot(&text, text.len());
        other.document_id = 8;
        assert!(cache.predict(&other, "de").is_empty());
        assert!(cache.observe(&other, "de", &|| true).is_none());
        assert_eq!(cache.documents.len(), 1);
    }

    #[test]
    fn ambiguous_directions_abstain() {
        let (mut cache, text) = trained();
        let changed = text.replacen("decode_user", "decode_account", 1);
        observe(&mut cache, &changed, changed.len(), "de");
        assert!(
            cache
                .predict(&snapshot(&changed, changed.len()), "de")
                .is_empty()
        );
    }

    #[test]
    fn worker_ranks_temporal_transformations_and_history_setting_removes_them() {
        let (history, text) = trained();
        let mut cache = super::super::TokenCache::default();
        cache.edit_history = history;
        let index = crate::completion_index::CompletionIndex::default();
        let active = snapshot(&text, text.len());
        let result = super::super::compute_contextual_invoked(
            &active,
            "de",
            "Unknown",
            &index,
            &mut cache,
            false,
            || false,
        )
        .unwrap();
        assert_eq!(
            result.suggestions.first().map(String::as_str),
            Some("decode_customer")
        );
        assert_eq!(result.details[0].annotation, "edit");
        let mut settings = index.settings();
        settings.use_history = false;
        index.configure(settings);
        let result = super::super::compute_contextual_invoked(
            &active,
            "de",
            "Unknown",
            &index,
            &mut cache,
            false,
            || false,
        )
        .unwrap();
        assert!(
            !result
                .suggestions
                .iter()
                .any(|word| word == "decode_customer")
        );
        assert!(cache.edit_history.documents.is_empty());
    }

    #[test]
    fn own_predictions_do_not_supply_additional_independent_witnesses() {
        let (mut cache, text) = trained();
        cache.predict(&snapshot(&text, text.len()), "de");
        let changed = text.replacen("decode_user", "decode_customer", 1);
        observe(&mut cache, &changed, changed.len(), "de");
        assert_eq!(cache.documents[0].changes.len(), 2);
    }

    #[test]
    fn snapshot_retention_is_bounded_and_oversized_documents_are_removed() {
        let mut cache = Cache::default();
        for id in 0..12 {
            let mut active = snapshot("load_user(); lo", 14);
            active.document_id = id;
            cache.observe(&active, "lo", &|| false).unwrap();
            assert!(cache.documents.len() <= DOCUMENTS);
        }
        assert_eq!(
            cache
                .documents
                .iter()
                .map(|d| d.id)
                .collect::<Vec<_>>()
                .len(),
            DOCUMENTS
        );
        let mut active = snapshot(&"x".repeat(super::super::MAX_DOCUMENT_BYTES + 1), 2);
        active.document_id = 11;
        cache.observe(&active, "xx", &|| false).unwrap();
        assert!(cache.documents.iter().all(|document| document.id != 11));
    }

    #[test]
    fn undo_checkpoint_seeds_first_replacement_with_a_shorter_baseline() {
        let mut cache = Cache::default();
        let active = snapshot("load_customer", 13);
        cache
            .seed(&active, &Rope::from_str("load_user"), &|| false)
            .unwrap();
        cache.observe(&active, "load_customer", &|| false).unwrap();
        let next = snapshot("load_customer(); save_user", 25);
        cache.observe(&next, "save_user", &|| false).unwrap();
        assert_eq!(cache.documents[0].changes.len(), 1);
        assert_eq!(cache.documents[0].changes[0].before, "load_user");
        assert_eq!(cache.documents[0].changes[0].after, "load_customer");
        // Repeated request baselines must not reset accumulated evidence.
        cache
            .seed(&next, &Rope::from_str("different"), &|| false)
            .unwrap();
        assert_eq!(cache.documents[0].changes.len(), 1);
    }

    #[test]
    fn repeated_literal_rename_requires_different_sites() {
        let mut cache = Cache::default();
        observe(&mut cache, "user; user; cu", 4, "user");
        observe(&mut cache, "customer; user; cu", 8, "customer");
        observe(&mut cache, "customer; user; cu", 18, "cu");
        assert!(
            cache
                .predict(&snapshot("customer; user; cu", 18), "cu")
                .is_empty()
        );
        observe(&mut cache, "customer; customer; cu", 22, "cu");
        assert_eq!(
            cache.predict(&snapshot("customer; customer; cu", 22), "cu"),
            vec!["customer"]
        );
    }

    #[test]
    fn grouped_undo_and_deleted_witnesses_withdraw_stale_intent() {
        let (mut cache, text) = trained();
        let undone = text.replace("customer", "user");
        observe(&mut cache, &undone, undone.len(), "de");
        assert!(cache.documents[0].changes.is_empty());
        assert!(
            cache
                .predict(&snapshot(&undone, undone.len()), "de")
                .is_empty()
        );

        let (mut cache, text) = trained();
        let deleted = text.replacen("load_customer", "", 1);
        observe(&mut cache, &deleted, deleted.len(), "de");
        assert_eq!(cache.documents[0].changes.len(), 1);
        assert!(
            cache
                .predict(&snapshot(&deleted, deleted.len()), "de")
                .is_empty()
        );
    }

    #[test]
    fn unicode_edits_use_character_offsets_for_coalescing_and_witness_sites() {
        let mut cache = Cache::default();
        let text = "load_õun(); save_õun(); decode_õun(); de";
        observe(&mut cache, text, 4, "load");
        let first = text.replacen("load_õun", "load_pirn", 1);
        observe(&mut cache, &first, first.chars().count(), "de");
        let second = first.replacen("save_õun", "save_pirn", 1);
        observe(&mut cache, &second, second.chars().count(), "de");
        assert!(
            cache
                .predict(&snapshot(&second, second.chars().count()), "de")
                .contains(&"decode_pirn".into())
        );
    }
}
