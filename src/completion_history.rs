//! Bounded project-local preferences, separate from lexical relevance.
use crate::completion_context::{Context, hash_parts};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const MAX_CHOICES: usize = 512;

#[derive(Clone, Serialize, Deserialize)]
struct Choice {
    context: u64,
    prefix: String,
    word: String,
    manual: bool,
    count: u16,
    used: u64,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct History {
    clock: u64,
    choices: Vec<Choice>,
}

fn context_key(context: Context) -> u64 {
    let key = [0, 2, 4]
        .into_iter()
        .map(|slot| context.keys[slot])
        .find(|&key| key != 0)
        .unwrap_or(0);
    if context.source_scope != 0 {
        hash_parts(
            21,
            &[
                &context.source_scope.to_string(),
                &context.source_family.to_string(),
                &key.to_string(),
                &context.domain.slot().to_string(),
            ],
        )
    } else if context.source_family == 0 {
        hash_parts(20, &[&key.to_string(), &context.domain.slot().to_string()])
    } else {
        hash_parts(
            20,
            &[
                &key.to_string(),
                &context.domain.slot().to_string(),
                &context.source_family.to_string(),
            ],
        )
    }
}

impl History {
    pub(crate) fn bounded(mut self) -> Self {
        self.choices.retain(|choice| {
            choice.word.len() <= 160 && choice.prefix.len() <= 64 && choice.count > 0
        });
        self.choices
            .sort_unstable_by_key(|choice| std::cmp::Reverse(choice.used));
        self.choices.truncate(MAX_CHOICES);
        self.clock = self.clock.max(
            self.choices
                .iter()
                .map(|choice| choice.used)
                .max()
                .unwrap_or(0),
        );
        self
    }

    pub(crate) fn record(&mut self, word: &str, prefix: &str, context: Context, manual: bool) {
        if word.len() > 160 || prefix.len() > 64 {
            return;
        }
        self.clock = self.clock.saturating_add(1);
        let key = context_key(context);
        let prefix = prefix.to_lowercase();
        if let Some(choice) = self.choices.iter_mut().find(|choice| {
            choice.context == key
                && choice.prefix == prefix
                && choice.word == word
                && choice.manual == manual
        }) {
            choice.count = choice.count.saturating_add(if manual { 2 } else { 1 });
            choice.used = self.clock;
            return;
        }
        if self.choices.len() >= MAX_CHOICES {
            let oldest = self
                .choices
                .iter()
                .enumerate()
                .min_by_key(|(_, choice)| choice.used)
                .map(|(i, _)| i)
                .unwrap();
            self.choices.swap_remove(oldest);
        }
        self.choices.push(Choice {
            context: key,
            prefix,
            word: word.into(),
            manual,
            count: if manual { 2 } else { 1 },
            used: self.clock,
        });
    }

    pub(crate) fn scores(
        &self,
        prefix: &str,
        context: Context,
        explicit: bool,
    ) -> BTreeMap<String, i64> {
        let key = context_key(context);
        let prefix = prefix.to_lowercase();
        let mut result = BTreeMap::<String, i64>::new();
        for choice in &self.choices {
            if choice.context != key
                || !(choice.prefix.is_empty()
                    || prefix.starts_with(&choice.prefix)
                    || choice.prefix.starts_with(&prefix))
            {
                continue;
            }
            let age = self.clock.saturating_sub(choice.used);
            let recency = if age < 32 {
                16
            } else if age < 128 {
                8
            } else {
                0
            };
            let score = i64::from(choice.count.min(8)) * 7
                + recency
                + i64::from(choice.manual == explicit) * 10
                + i64::from(choice.prefix == prefix) * 12;
            let score = if choice.context == key {
                score
            } else {
                score / 2
            };
            let value = result.entry(choice.word.clone()).or_default();
            *value = (*value).max(score);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receiver(name: &str) -> Context {
        let mut context = Context::default();
        context.keys[0] = hash_parts(1, &[name]);
        context
    }

    #[test]
    fn same_language_choices_are_scoped_to_the_actual_file() {
        let mut history = History::default();
        let first = Context {
            source_family: 17,
            source_scope: 101,
            ..receiver("config")
        };
        let second = Context {
            source_scope: 102,
            ..first
        };
        for _ in 0..8 {
            history.record("readFirst", "read", first, true);
        }
        assert!(
            history
                .scores("read", first, false)
                .contains_key("readFirst")
        );
        assert!(history.scores("read", second, false).is_empty());
    }

    #[test]
    fn typed_choice_history_uses_exact_syntax_names() {
        let mut history = History::default();
        let c = Context {
            source_family: crate::completion_source::Kind::new(None, "C").family,
            ..receiver("client")
        };
        let tcl = Context {
            source_family: crate::completion_source::Kind::new(None, "Tcl").family,
            ..c
        };
        history.record("sendPacket", "sp", c, true);
        assert!(history.scores("sp", c, true).contains_key("sendPacket"));
        assert!(history.scores("sp", tcl, true).is_empty());
        let header = Context {
            source_family: crate::completion_source::Kind::new(None, "C/C++ Header").family,
            ..c
        };
        assert!(history.scores("sp", header, true).is_empty());
        let restored: History =
            serde_json::from_slice(&serde_json::to_vec(&history).unwrap()).unwrap();
        assert_eq!(
            restored.scores("sp", c, true),
            history.scores("sp", c, true)
        );
    }

    #[test]
    fn choices_follow_prefix_context_and_invocation_without_overriding_relevance() {
        let mut history = History::default();
        history.record("readPacket", "rp", receiver("reader"), true);
        assert!(history.scores("rpa", receiver("reader"), true)["readPacket"] > 0);
        assert!(history.scores("w", receiver("reader"), true).is_empty());
        assert!(history.scores("rp", receiver("writer"), true).is_empty());
        assert!(
            history.scores("rp", receiver("reader"), true)["readPacket"]
                > history.scores("rp", receiver("reader"), false)["readPacket"]
        );
        for _ in 0..100 {
            history.record("readPacket", "rp", receiver("reader"), true);
        }
        assert!(history.scores("rp", receiver("reader"), true)["readPacket"] <= 94);
        let restored: History =
            serde_json::from_slice(&serde_json::to_vec(&history).unwrap()).unwrap();
        assert_eq!(
            restored.bounded().scores("rp", receiver("reader"), true),
            history.scores("rp", receiver("reader"), true)
        );
    }

    #[test]
    fn eviction_uses_recency_and_preserves_frequently_refreshed_choices() {
        let mut history = History::default();
        for i in 0..MAX_CHOICES {
            history.record(&format!("word{i}"), "w", Context::default(), false);
        }
        history.record("word0", "w", Context::default(), false);
        history.record("newWord", "w", Context::default(), false);
        assert_eq!(history.choices.len(), MAX_CHOICES);
        assert!(history.choices.iter().any(|choice| choice.word == "word0"));
        assert!(!history.choices.iter().any(|choice| choice.word == "word1"));
    }
}
