//! Adversarial checks for the occurrence learner, independent of ranking fixtures.
use super::*;

fn relational_query(right: &str) -> Query {
    Query::new("register(chanNew, handlerNew)\nunregister(chanNew, ", right)
}

fn relational_example(right: &str) -> String {
    format!("register(chanOld, handlerOld)\nunregister(chanOld, handlerOld{right}")
}

#[test]
fn parameter_keys_preserve_equalities_and_inequalities() {
    let a = parse("act(sourceA, destinationA, sourceA)");
    let b = parse("run(sourceB, destinationB, sourceB)");
    let inconsistent = parse("run(sourceB, destinationB, destinationB)");
    let collapsed = parse("run(sourceB, sourceB, sourceB)");
    let sig = |data: &Data| signature(data, 0, data.tokens.len(), true);
    assert_eq!(sig(&a), sig(&b));
    assert_ne!(sig(&a), sig(&inconsistent));
    assert_ne!(sig(&a), sig(&collapsed));
    assert_ne!(
        signature(&a, 0, a.tokens.len(), false),
        signature(&b, 0, b.tokens.len(), false)
    );
}

#[test]
fn adapted_identifier_does_not_need_to_exist_in_example_vocabulary() {
    let q = relational_query(")");
    let text = relational_example(")");
    assert!(!text.contains("handlerNew"));
    let mut e = Evidence::default();
    e.collect(&q, &text, true);
    assert!(e.candidates().any(|word| word == "handlerNew"));
    assert!(e.probability("handlerNew", 0.05) > e.probability("handlerOld", 0.05));
}

#[test]
fn a_visible_suffix_can_bind_a_target_not_yet_seen_on_the_left() {
    let query = Query::new("dispatch(", ", channelNew); validate(payloadNew)");
    let source = "dispatch(payloadOld, channelOld); validate(payloadOld)";
    let mut local = Evidence::default();
    local.collect(&query, source, true);
    assert!(local.candidates().any(|word| word == "payloadNew"));
    assert!(local.probability("payloadNew", 0.05) > 0.05);

    let mut index = Index::default();
    index.insert(7, Arc::new(parse(source)));
    let mut retrieved = Evidence::default();
    index.query(&query, 0, &mut retrieved);
    assert!(retrieved.candidates().any(|word| word == "payloadNew"));
}

#[test]
fn suffix_target_binding_needs_a_repeated_target_and_fixed_operation_anchors() {
    let source = "dispatch(payloadOld, channelOld); validate(payloadOld)";
    for right in [
        ", channelNew); remove(payloadNew)",
        ", payloadNew); validate(payloadNew)",
        ", channelNew); validate(payloadNew, extra)",
    ] {
        let query = Query::new("dispatch(", right);
        let mut evidence = Evidence::default();
        evidence.collect(&query, source, true);
        assert!(!evidence.candidates().any(|word| word == "payloadNew"), "{right}");
    }
    let mut evidence = Evidence::default();
    evidence.collect(
        &Query::new("dispatch(", ", channelNew); validate(otherNew)"),
        "dispatch(payloadOld, channelOld); validate(otherOld)",
        true,
    );
    assert!(!evidence.candidates().any(|word| word == "otherNew"));
}

#[test]
fn suffix_binding_keeps_left_mappings_and_both_directions_injective() {
    let source = "saved = payloadOld; dispatch(payloadOld, channelOld); validate(payloadOld)";
    let query = Query::new(
        "saved = firstNew; dispatch(",
        ", channelNew); validate(secondNew)",
    );
    let mut evidence = Evidence::default();
    evidence.collect(&query, source, true);
    assert!(!evidence.candidates().any(|word| matches!(word, "firstNew" | "secondNew")));

    let query = Query::new("dispatch(", ", sharedNew); validate(sharedNew)");
    let mut evidence = Evidence::default();
    evidence.collect(&query, "dispatch(payloadOld, channelOld); validate(payloadOld)", true);
    assert!(!evidence.candidates().any(|word| word == "sharedNew"));

    let mut evidence = Evidence::default();
    evidence.collect(
        &Query::new("dispatch(", "); verify(firstNew, secondNew)"),
        "dispatch(payloadOld); verify(payloadOld, payloadOld)",
        true,
    );
    assert!(!evidence.candidates().any(|word| matches!(word, "firstNew" | "secondNew")));
}

#[test]
fn suffix_bindings_back_off_when_disabled_and_do_not_invent_unseen_active_file_names() {
    let source = Arc::new(parse("dispatch(payloadOld, channelOld); validate(payloadOld)"));
    let mut index = Index::default();
    index.insert(7, source);
    for mode in [Mode::NoRightContext, Mode::LiteralOnly] {
        with_mode(mode, || {
            let query = Query::new("dispatch(", ", channelNew); validate(payloadNew)");
            let mut evidence = Evidence::default();
            index.query(&query, 0, &mut evidence);
            assert!(!evidence.candidates().any(|word| word == "payloadNew"));
        });
    }
    let query = Query::new("dispatch(", ", channelNew); validate(payloadNew)");
    let mut evidence = Evidence::default();
    index.query(&query, 7, &mut evidence);
    assert!(!evidence.candidates().any(|word| word == "payloadNew"));
}

#[test]
fn bidirectional_bindings_preserve_unicode_names_under_many_consistent_renamings() {
    let mut random = 0xc5a8_117d_6b20_9e43u64;
    for _ in 0..256 {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        let old_target = format!("vana_界_{random}");
        let old_channel = format!("kanal_é_{random}");
        let new_target = format!("uusNäide_λ_{}", random.rotate_left(17));
        let new_channel = format!("uusKanal_Ü_{}", random.rotate_right(11));
        let source = format!("dispatch({old_target}, {old_channel}); validate({old_target})");
        let right = format!(", {new_channel}); validate({new_target})");
        let query = Query::new("dispatch(", &right);
        let mut evidence = Evidence::default();
        evidence.collect(&query, &source, true);
        assert!(evidence.candidates().any(|word| word == new_target));
        assert!(!evidence.candidates().any(|word| word == old_target));
        assert!(!evidence.limited);

        let collapsed = format!(", {new_target}); validate({new_target})");
        let query = Query::new("dispatch(", &collapsed);
        let mut evidence = Evidence::default();
        evidence.collect(&query, &source, true);
        assert!(!evidence.candidates().any(|word| word == new_target));
    }
}

#[test]
fn fresh_suffix_identifier_must_keep_one_forward_mapping() {
    let q = relational_query(", decorate(extraFirst, extraSecond))");
    let text = relational_example(", decorate(extraOld, extraOld))");
    let mut e = Evidence::default();
    e.collect(&q, &text, true);
    assert!(
        e.mass() > 0.0,
        "contradictions must remain confidence evidence"
    );
    assert!(!e.candidates().any(|word| word == "handlerNew"));
}

#[test]
fn fresh_suffix_identifier_must_keep_one_reverse_mapping() {
    let q = relational_query(", decorate(extraNew, extraNew))");
    let text = relational_example(", decorate(extraFirst, extraSecond))");
    let mut e = Evidence::default();
    e.collect(&q, &text, true);
    assert!(e.mass() > 0.0);
    assert!(!e.candidates().any(|word| word == "handlerNew"));
}

#[test]
fn suffix_uses_the_target_mapping_established_on_the_left() {
    let q = relational_query(", handlerDifferent)");
    let text = relational_example(", handlerOld)");
    let mut e = Evidence::default();
    e.collect(&q, &text, true);
    assert!(e.mass() > 0.0);
    assert!(!e.candidates().any(|word| word == "handlerNew"));
}

#[test]
fn no_evidence_is_exact_backoff_and_conflicts_lower_probability() {
    let q = Query::new("choose(channel, ", ", expectedMode)");
    let mut e = Evidence::default();
    assert_eq!(e.probability("payload", 0.125), 0.125);
    e.collect(&q, "choose(channel, payload, wrongMode)", true);
    assert!(e.mass() > 0.0);
    assert!(e.probability("payload", 0.125) < 0.125);
}

#[test]
fn suffix_incompatible_long_context_backs_off_to_a_shorter_compatible_witness() {
    let query = Query::new("prepare(channel); choose(channel, ", ", finalMode)");
    let incompatible = "prepare(channel); choose(channel, wrongPayload, wrongMode)";
    let compatible = "choose(channel, rightPayload, finalMode)";
    let mut probabilities = Vec::new();
    for reverse in [false, true] {
        let sources = if reverse { [compatible, incompatible] } else { [incompatible, compatible] };
        let mut evidence = Evidence::default();
        for source in sources {
            evidence.collect(&query, source, true);
        }
        assert!(evidence.candidates().any(|word| word == "rightPayload"));
        assert!(!evidence.candidates().any(|word| word == "wrongPayload"));
        let mut clean = Evidence::default();
        clean.collect(&query, compatible, true);
        assert!(evidence.probability("rightPayload", 0.1) > 0.1);
        assert!(evidence.probability("rightPayload", 0.1) < clean.probability("rightPayload", 0.1));
        probabilities.push(evidence.probability("rightPayload", 0.1));
    }
    assert_eq!(probabilities[0], probabilities[1]);
    let mut index = Index::default();
    index.insert(1, Arc::new(parse(incompatible)));
    index.insert(2, Arc::new(parse(compatible)));
    let mut evidence = Evidence::default();
    index.query(&query, 0, &mut evidence);
    assert!(evidence.candidates().any(|word| word == "rightPayload"));
}

#[test]
fn compatible_long_context_keeps_priority_and_disables_short_context_fallback() {
    let query = Query::new("prepare(channel); choose(channel, ", ", finalMode)");
    let shorter = "choose(channel, genericPayload, finalMode)";
    let incompatible = "prepare(channel); choose(channel, wrongPayload, wrongMode)";
    let exact = "prepare(channel); choose(channel, exactPayload, finalMode)";
    let mut evidence = Evidence::default();
    evidence.collect(&query, shorter, true);
    evidence.collect(&query, incompatible, true);
    assert!(evidence.candidates().any(|word| word == "genericPayload"));
    evidence.collect(&query, exact, true);
    assert!(evidence.candidates().any(|word| word == "exactPayload"));
    assert!(!evidence.candidates().any(|word| word == "genericPayload"));
    assert!(evidence.probability("exactPayload", 0.1) > evidence.probability("genericPayload", 0.1));
    assert!(!evidence.limited);
}

#[test]
fn optional_extension_target_collision_preserves_the_more_specific_literal_seed() {
    let left = "choose(channel, rightPayload, finalMode)\nprepare(channel); choose(channel, rightSpecific, finalMode)\nprepare(channel); choose(channel, ";
    let query = Query::new(left, ", finalMode)");
    let mut evidence = Evidence::default();
    evidence.collect(&query, left, true);
    assert!(evidence.candidates().any(|word| word == "rightSpecific"));
    assert!(evidence.probability("rightSpecific", 0.1) > evidence.probability("rightPayload", 0.1));
    assert!(evidence.pools[5].length >= 8);
    with_mode(Mode::NoLiteralBackoff, || {
        let query = Query::new(left, ", finalMode)");
        let mut evidence = Evidence::default();
        evidence.collect(&query, left, true);
        assert!(!evidence.candidates().any(|word| word == "rightSpecific"));
    });
}

#[test]
fn target_collision_fallback_requires_a_literal_seed_and_still_verifies_suffix_mapping() {
    let left = "choose(channel, rightPayload, finalMode)\nprepare(channel); choose(channel, rightSpecific, finalMode)\nprepare(channel); choose(channel, ";
    let query = Query::new(left, ", finalMode)");
    let data = parse(left);
    let at = data.tokens.iter().position(|&t| data.word(t) == Some("rightSpecific")).unwrap();
    let mut renamed = Evidence::default();
    renamed.observe(&query, &data, at, 8, true, 4, 1, false);
    assert_eq!(renamed.mass(), 0.);
    assert!(!renamed.candidates().any(|word| word == "rightSpecific"));

    let incompatible = "choose(channel, rightPayload, finalMode)\nprepare(channel); choose(channel, rightSpecific, channel)\nprepare(channel); choose(channel, ";
    let mut evidence = Evidence::default();
    evidence.collect(&query, incompatible, true);
    assert!(!evidence.candidates().any(|word| word == "rightSpecific"));
}

#[test]
fn cloned_shorter_fallbacks_do_not_inflate_confidence_or_call_support() {
    let query = Query::new("prepare(channel); choose(channel, ", ", finalMode)");
    let bad = Arc::new(parse("prepare(channel); choose(channel, wrongPayload, wrongMode)"));
    let good = Arc::new(parse("choose(channel, rightPayload, finalMode)"));
    let mut index = Index::default();
    index.insert(1, bad);
    index.insert(2, Arc::clone(&good));
    let mut once = Evidence::default();
    index.query(&query, 0, &mut once);
    for file in 3..=1000 {
        index.insert(file, Arc::clone(&good));
    }
    let mut copies = Evidence::default();
    index.query(&query, 0, &mut copies);
    assert_eq!(copies.mass(), once.mass());
    assert_eq!(copies.probability("rightPayload", 0.1), once.probability("rightPayload", 0.1));
    assert_eq!(copies.callable("rightPayload"), Some(false));
    assert!(!copies.limited);
    assert!(std::mem::size_of::<Evidence>() < 2048);
}

#[test]
fn compatible_literal_suffix_is_not_hidden_by_a_renamed_clone_representative() {
    let query = Query::new("choose(channel, ", ", finalMode)");
    for reverse in [false, true] {
        let mut index = Index::default();
        let mut sources = [
            "choose(channel, payload, wrongMode)",
            "choose(channel, payload, finalMode)",
        ];
        if reverse {
            sources.reverse();
        }
        for (i, text) in sources.into_iter().enumerate() {
            index.insert(i as u64 + 1, Arc::new(parse(text)));
        }
        let mut evidence = Evidence::default();
        index.query(&query, 0, &mut evidence);
        assert!(evidence.candidates().any(|word| word == "payload"));
        assert!(evidence.probability("payload", 0.1) > 0.1);
    }
}

#[test]
fn two_thousand_cross_file_clones_do_not_increase_prediction_confidence() {
    let q = relational_query(")");
    let data = Arc::new(parse(&relational_example(")")));
    let mut index = Index::default();
    index.insert(1, Arc::clone(&data));
    let mut once = Evidence::default();
    index.query(&q, 0, &mut once);
    for file in 2..=2000 {
        index.insert(file, Arc::clone(&data));
    }
    let mut clones = Evidence::default();
    index.query(&q, 0, &mut clones);
    assert!(once.mass() > 0.0);
    assert_eq!(clones.mass(), once.mass());
    assert_eq!(
        clones.probability("handlerNew", 0.1),
        once.probability("handlerNew", 0.1)
    );
    assert!(
        !clones.limited,
        "clone groups should not consume one match per file"
    );
}

#[test]
fn local_repetition_is_preserved_separately_from_cross_file_clone_mass() {
    let q = Query::new("emit(channel, ", ")");
    let once = "emit(channel, payload)\n";
    let repeated = once.repeat(12);
    let mut one = Evidence::default();
    one.collect(&q, once, true);
    let mut many = Evidence::default();
    many.collect(&q, &repeated, true);
    assert!(many.mass() > one.mass());
    assert!(many.probability("payload", 0.1) > one.probability("payload", 0.1));
}

#[test]
fn current_file_before_and_after_segments_do_not_alias_occurrence_offsets() {
    let q = Query::new("emit(channel, ", ")");
    let mut e = Evidence::default();
    e.collect(&q, "emit(channel, payloadBefore)\n", false);
    e.collect(&q, "emit(channel, payloadAfter)\n", false);
    assert!(e.candidates().any(|word| word == "payloadBefore"));
    assert!(e.candidates().any(|word| word == "payloadAfter"));
}

#[test]
fn every_file_identity_uses_the_same_occurrence_retrieval_path() {
    let q = Query::new("bridge.send", "(");
    // Identity values carry no extension, grammar, directory, or relationship.
    // The learner must handle a new/high identity exactly like the first file.
    for file in [1, 99, u64::MAX - 1] {
        let mut index = Index::default();
        index.insert(file, Arc::new(parse("bridge.send payload(")));
        let mut e = Evidence::default();
        index.query(&q, 0, &mut e);
        assert!(e.candidates().any(|word| word == "payload"));
        let mut excluded = Evidence::default();
        index.query(&q, file, &mut excluded);
        assert_eq!(excluded.mass(), 0.0);
    }
}

#[test]
fn active_indexed_relationships_can_adapt_only_to_names_already_visible_at_the_caret() {
    with_mode(Mode::Full, || {
        let file = 51;
        let mut index = Index::default();
        index.insert(
            file,
            Arc::new(parse(
                "register(chanOld, handlerOld)\nunregister(chanOld, handlerOld)",
            )),
        );
        let mut evidence = Evidence::default();
        index.query(&relational_query(")"), file, &mut evidence);
        assert!(evidence.candidates().any(|word| word == "handlerNew"));
        let mut stale = Index::default();
        stale.insert(file, Arc::new(parse("bridge.send payload(")));
        let mut evidence = Evidence::default();
        stale.query(&Query::new("bridge.send", "("), file, &mut evidence);
        assert_eq!(evidence.mass(), 0.);
    });
}

#[test]
fn unanchored_renamed_clones_do_not_exhaust_verification_before_an_anchored_witness() {
    let mut index = Index::default();
    for file in 1..=1000 {
        index.insert(
            file,
            Arc::new(parse(&format!(
                "left{file}(arg{file}, out{file}) right{file}(arg{file}, out{file})"
            ))),
        );
    }
    index.insert(1001, Arc::new(parse(&relational_example(")"))));
    let mut evidence = Evidence::default();
    index.query(&relational_query(")"), 0, &mut evidence);
    assert!(
        !evidence.limited,
        "unanchored shapes must not consume the expensive verification allowance"
    );
    assert!(evidence.candidates().any(|word| word == "handlerNew"));
}

#[test]
fn query_keeps_the_caret_tail_after_more_than_the_index_token_limit() {
    let text = "unrelated(value);\n".repeat(MAX_TOKENS)
        + "register(chanNew, handlerNew)\nunregister(chanNew, ";
    let q = Query::new(&text, ")");
    let mut e = Evidence::default();
    e.collect(&q, &relational_example(")"), true);
    assert!(e.candidates().any(|word| word == "handlerNew"));
    assert!(q.left.tokens.len() <= 64);
}

#[test]
fn removing_all_files_releases_postings_and_keeps_accounting_balanced() {
    let mut index = Index::default();
    for file in 1..=12 {
        index.insert(
            file,
            Arc::new(parse(&format!("emit(channel{file}, payload{file})\n"))),
        );
    }
    assert!(index.memory() > 0);
    for file in (1..=12).rev() {
        index.remove(file);
    }
    assert_eq!(index.memory(), 0);
    assert!(index.files.is_empty());
    assert!(index.postings.is_empty());
}

#[test]
fn churn_releases_sparse_posting_capacity_and_reconstructs_accounted_bytes() {
    let mut index = Index::default();
    for file in 1..=80 {
        index.insert(file, Arc::new(parse(&format!(
            "emit{file}(channel{file}, payload{file})\nrecv{file}(channel{file}, payload{file})\n"
        ))));
    }
    for file in 2..=80 {
        index.remove(file);
    }
    let mut expected = index.files.len() * 48 + 256
        + index.postings.len() * 128 + 512;
    for locations in index.postings.values() {
        expected += locations.capacity() * std::mem::size_of::<Location>();
        assert!(locations.capacity() <= (locations.len() * 2).max(4));
        assert!(locations.windows(2).all(|pair| (pair[0].fingerprint, pair[0].file)
            < (pair[1].fingerprint, pair[1].file)));
    }
    assert_eq!(index.memory(), expected);
    index.remove(1);
    assert_eq!(index.memory(), 0);
}

#[test]
fn compact_posting_admission_bounds_capacity_growth_and_replacement() {
    let mut index = Index::default();
    for file in 0..100 {
        let data = Arc::new(parse(&format!("register(chan{file}, handler{file}); unregister(chan{file}, handler{file});")));
        let before = index.memory();
        let additional = index.additional_memory(&data);
        index.insert(file, Arc::clone(&data));
        assert!(index.memory() <= before + additional, "file {file}: before {before}, bound {additional}, actual {}", index.memory());
        let installed = index.memory();
        index.insert(file, data);
        assert_eq!(index.memory(), installed);
    }
    for file in 0..100 { index.remove(file); }
    assert_eq!(index.memory(), 0);
}

#[test]
fn nonmatching_context_hashes_consume_the_matching_work_allowance() {
    let left = (0..40)
        .map(|n| format!("anchor{n}"))
        .collect::<Vec<_>>()
        .join(" ");
    let q = Query::new(&left, "");
    let mut e = Evidence::default();
    e.collect(&q, &"unrelated ".repeat(MAX_TOKENS), true);
    assert!(
        e.limited,
        "failed retrievals must also consume the work allowance"
    );
    assert_eq!(e.mass(), 0.0);
    assert!(!e.candidates().any(|_| true));
}

#[test]
fn one_call_cannot_become_two_through_literal_and_parameter_channels() {
    let q = Query::new("saved = flush; client.", "");
    let mut e = Evidence::default();
    e.collect(&q, "saved = flush; client.flush()", true);
    assert!(e.call_probability("flush").is_some());
    assert_eq!(e.callable("flush"), Some(false));
}

#[test]
fn repeated_local_calls_support_parentheses_but_mixed_field_uses_do_not() {
    let q = Query::new("client.", "");
    let mut calls = Evidence::default();
    calls.collect(&q, "client.flush(); client.flush();", true);
    assert_eq!(calls.callable("flush"), Some(true));
    let mut mixed = Evidence::default();
    mixed.collect(
        &q,
        "client.flush(); client.flush = first; client.flush = second;",
        true,
    );
    assert_eq!(mixed.callable("flush"), Some(false));
}

#[test]
fn cloned_call_examples_cannot_fabricate_independent_call_support() {
    let q = Query::new("saved = flush; client.", "");
    let data = Arc::new(parse("saved = flush; client.flush()"));
    let mut index = Index::default();
    for file in 1..=1000 {
        index.insert(file, Arc::clone(&data));
    }
    let mut e = Evidence::default();
    index.query(&q, 0, &mut e);
    assert_eq!(e.callable("flush"), Some(false));
}

#[test]
fn retrieved_repeat_mass_is_bounded_and_generated_boundary_windows_are_not_votes() {
    let query = Query::new("client.", "");
    let evidence = |source: &str| {
        let mut index = Index::default();
        index.insert(1, Arc::new(parse(source)));
        let mut evidence = Evidence::default();
        index.query(&query, 0, &mut evidence);
        evidence
    };
    let two = evidence("client.flush(); client.flush();");
    let many = evidence(&"client.flush(); ".repeat(100));
    assert_eq!(two.callable("flush"), Some(true));
    assert_eq!(
        many.call_probability("flush"),
        two.call_probability("flush")
    );
    assert_eq!(
        many.probability("flush", 0.1),
        two.probability("flush", 0.1)
    );
    let mut mixed = Index::default();
    mixed.insert(1, Arc::new(parse(&"client.flush(); ".repeat(100))));
    mixed.insert(
        2,
        Arc::new(parse("client.flush = true; client.flush = false;")),
    );
    let mut e = Evidence::default();
    mixed.query(&query, 0, &mut e);
    assert_eq!(e.callable("flush"), Some(false));
}

#[test]
fn largest_ordered_token_builder_fits_reserved_half_megabyte() {
    let mut builder = Builder::default();
    let source: String = (0..MAX_WORDS)
        .map(|n| format!("{n:04}{}\n", "x".repeat(MAX_WORD_BYTES - 4)))
        .collect();
    for chunk in source.as_bytes().chunks(64 * 1024) {
        builder.feed(std::str::from_utf8(chunk).unwrap());
    }
    assert_eq!(builder.data.words.len(), MAX_WORDS);
    assert!(builder.working_memory() <= 512 * 1024);
    assert!(builder.finish().valid());
}

#[test]
fn extended_left_verification_rejects_a_receiver_conflict_before_the_seed() {
    let data = parse("logger.printLine(value)\nlogger.printLine(value)");
    let at = data.tokens.len() - 4;
    with_mode(Mode::Phase1, || {
        let query = Query::new("buffer.printBuffer(value)\nlogger.", "");
        let mut evidence = Evidence::default();
        evidence.observe(&query, &data, at, 8, true, 4, 1, false);
        assert!(evidence.candidates().any(|word| word == "printBuffer"));
    });
    with_mode(Mode::NoLiteralBackoff, || {
        let query = Query::new("buffer.printBuffer(value)\nlogger.", "");
        let mut evidence = Evidence::default();
        evidence.observe(&query, &data, at, 8, true, 4, 1, false);
        assert_eq!(evidence.mass(), 0.);
    });
}

#[test]
fn literal_seed_can_bind_an_earlier_identifier_with_matching_roles() {
    with_mode(Mode::NoLiteralBackoff, || {
        let data = parse("pair.left = value; consume(pair.left, value)");
        let query = Query::new("pair.left = valueNext; consume(pair.left, ", "");
        let mut evidence = Evidence::default();
        evidence.observe(&query, &data, data.tokens.len() - 2, 2, false, 5, 1, false);
        assert!(evidence.candidates().any(|word| word == "valueNext"));
        assert!(!evidence.candidates().any(|word| word == "value"));
    });
}

#[test]
fn fresh_left_binding_requires_both_neighbor_roles() {
    with_mode(Mode::NoLiteralBackoff, || {
        let data = parse("mode: fastMode\nmode: fastMode");
        let query = Query::new("factoryName\nmode:", "");
        let mut evidence = Evidence::default();
        evidence.observe(&query, &data, data.tokens.len() - 1, 2, false, 5, 1, false);
        assert!(evidence.candidates().any(|word| word == "fastMode"));
        assert!(!evidence.candidates().any(|word| word == "factoryName"));
    });
}

#[test]
fn literal_backoff_keeps_the_exact_witness_without_the_conflicting_rename() {
    with_mode(Mode::Full, || {
        let data = parse("logger.printLine(value)\nlogger.printLine(value)");
        let query = Query::new("buffer.printBuffer(value)\nlogger.", "");
        let at = data.tokens.len() - 4;
        let mut literal = Evidence::default();
        literal.observe(&query, &data, at, 2, false, 5, 1, false);
        assert!(literal.candidates().any(|word| word == "printLine"));
        assert!(!literal.candidates().any(|word| word == "printBuffer"));
        let mut parameter = Evidence::default();
        parameter.observe(&query, &data, at, 8, true, 4, 1, false);
        assert_eq!(parameter.mass(), 0.);
    });
}

#[test]
fn renamed_seed_edge_needs_a_compatible_neighbor_role() {
    let data = parse("mode: fastMode\nmode: fastMode");
    let query = Query::new("factoryName\nmode:", "");
    let mut evidence = Evidence::default();
    evidence.observe(&query, &data, data.tokens.len() - 1, 4, true, 4, 1, false);
    assert!(!evidence.candidates().any(|word| word == "factoryName"));
    assert_eq!(evidence.mass(), 0.);
}

#[test]
fn workspace_presence_prior_does_not_gain_mass_from_generated_repetition() {
    assert_eq!(workspace_count(0), 1.0);
    assert_eq!(workspace_count(1), 2.0);
    assert_eq!(workspace_count(1000), workspace_count(1));
    with_mode(Mode::RawWorkspacePrior, || {
        assert!(workspace_count(1000) > workspace_count(1));
    });
    assert_eq!(workspace_count(1000), 2.0);
}
