use super::*;

#[test]
fn bounded_cache_examples_round_trip_at_both_limits() {
    let data = Data {
        words: (0..MAX_WORDS).map(|i| format!("identifier{i}")).collect(),
        tokens: (0..MAX_TOKENS)
            .map(|i| WORD | (i % MAX_WORDS) as u32)
            .collect(),
    };
    let restored: Data = serde_json::from_slice(&serde_json::to_vec(&data).unwrap()).unwrap();
    assert!(restored.valid());
    assert_eq!(restored.words, data.words);
    assert_eq!(restored.tokens, data.tokens);
}

#[test]
fn cache_words_stop_at_first_excess_without_decoding_the_tail() {
    let words = std::iter::repeat_n("\"\"", MAX_WORDS + 1)
        .collect::<Vec<_>>()
        .join(",");
    let json = format!("{{\"words\":[{words},INVALID],\"tokens\":[]}}");
    let error = serde_json::from_str::<Data>(&json).err().unwrap();
    assert!(error.to_string().contains("words exceed 1024"), "{error}");
}

#[test]
fn cache_tokens_stop_at_first_excess_without_decoding_the_tail() {
    let tokens = std::iter::repeat_n("0", MAX_TOKENS + 1)
        .collect::<Vec<_>>()
        .join(",");
    let json = format!("{{\"words\":[],\"tokens\":[{tokens},INVALID]}}");
    let error = serde_json::from_str::<Data>(&json).err().unwrap();
    assert!(error.to_string().contains("tokens exceed 2048"), "{error}");
}

#[test]
fn cache_identifier_limit_counts_utf8_bytes_before_retaining_the_word() {
    let valid = "é".repeat(MAX_WORD_BYTES / 2);
    let json = serde_json::json!({"words":[valid],"tokens":[]});
    assert!(serde_json::from_value::<Data>(json).is_ok());
    let oversized = "é".repeat(MAX_WORD_BYTES / 2 + 1);
    let json = serde_json::json!({"words":[oversized],"tokens":[]});
    let error = serde_json::from_value::<Data>(json).err().unwrap();
    assert!(
        error.to_string().contains("identifier exceeds 160"),
        "{error}"
    );
}

#[test]
fn cache_arrays_reject_an_untrusted_large_size_hint_before_reading_items() {
    struct HugeHint<'a>(&'a std::cell::Cell<usize>);
    impl<'de> serde::de::SeqAccess<'de> for HugeHint<'_> {
        type Error = serde::de::value::Error;
        fn next_element_seed<T: serde::de::DeserializeSeed<'de>>(
            &mut self,
            _seed: T,
        ) -> Result<Option<T::Value>, Self::Error> {
            self.0.set(self.0.get() + 1);
            Ok(None)
        }
        fn size_hint(&self) -> Option<usize> {
            Some(usize::MAX)
        }
    }
    let reads = std::cell::Cell::new(0);
    let words = deserialize_example_words(serde::de::value::SeqAccessDeserializer::new(HugeHint(
        &reads,
    )));
    assert!(words.is_err());
    let tokens = deserialize_example_tokens(serde::de::value::SeqAccessDeserializer::new(
        HugeHint(&reads),
    ));
    assert!(tokens.is_err());
    assert_eq!(reads.get(), 0);
}
