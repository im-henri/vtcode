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
fn suffix_bindings_back_off_when_disabled() {
    let source = "dispatch(payloadOld, channelOld); validate(payloadOld)";
    for mode in [Mode::NoRightContext, Mode::LiteralOnly] {
        with_mode(mode, || {
            let query = Query::new("dispatch(", ", channelNew); validate(payloadNew)");
            let mut evidence = Evidence::default();
            evidence.collect(&query, source, true);
            assert!(!evidence.candidates().any(|word| word == "payloadNew"));
        });
    }
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
    assert!(evidence.pools[3].length >= 8);
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
    renamed.observe(&query, &data, at, 8, true, 2, 1);
    assert_eq!(renamed.mass(), 0.);
    assert!(!renamed.candidates().any(|word| word == "rightSpecific"));

    let incompatible = "choose(channel, rightPayload, finalMode)\nprepare(channel); choose(channel, rightSpecific, channel)\nprepare(channel); choose(channel, ";
    let mut evidence = Evidence::default();
    evidence.collect(&query, incompatible, true);
    assert!(!evidence.candidates().any(|word| word == "rightSpecific"));
}

#[test]
fn local_repetition_adds_evidence() {
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
fn extended_left_verification_rejects_a_receiver_conflict_before_the_seed() {
    let data = parse("logger.printLine(value)\nlogger.printLine(value)");
    let at = data.tokens.len() - 4;
    with_mode(Mode::Phase1, || {
        let query = Query::new("buffer.printBuffer(value)\nlogger.", "");
        let mut evidence = Evidence::default();
        evidence.observe(&query, &data, at, 8, true, 2, 1);
        assert!(evidence.candidates().any(|word| word == "printBuffer"));
    });
    with_mode(Mode::NoLiteralBackoff, || {
        let query = Query::new("buffer.printBuffer(value)\nlogger.", "");
        let mut evidence = Evidence::default();
        evidence.observe(&query, &data, at, 8, true, 2, 1);
        assert_eq!(evidence.mass(), 0.);
    });
}

#[test]
fn literal_seed_can_bind_an_earlier_identifier_with_matching_roles() {
    with_mode(Mode::NoLiteralBackoff, || {
        let data = parse("pair.left = value; consume(pair.left, value)");
        let query = Query::new("pair.left = valueNext; consume(pair.left, ", "");
        let mut evidence = Evidence::default();
        evidence.observe(&query, &data, data.tokens.len() - 2, 2, false, 3, 1);
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
        evidence.observe(&query, &data, data.tokens.len() - 1, 2, false, 3, 1);
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
        literal.observe(&query, &data, at, 2, false, 3, 1);
        assert!(literal.candidates().any(|word| word == "printLine"));
        assert!(!literal.candidates().any(|word| word == "printBuffer"));
        let mut parameter = Evidence::default();
        parameter.observe(&query, &data, at, 8, true, 2, 1);
        assert_eq!(parameter.mass(), 0.);
    });
}

#[test]
fn renamed_seed_edge_needs_a_compatible_neighbor_role() {
    let data = parse("mode: fastMode\nmode: fastMode");
    let query = Query::new("factoryName\nmode:", "");
    let mut evidence = Evidence::default();
    evidence.observe(&query, &data, data.tokens.len() - 1, 4, true, 2, 1);
    assert!(!evidence.candidates().any(|word| word == "factoryName"));
    assert_eq!(evidence.mass(), 0.);
}
