//! Prediction regressions for the current completion engine.
use super::*;

struct Case {
    name: &'static str,
    source: String,
    prefix: &'static str,
    language: &'static str,
    expected: &'static str,
}

fn cases() -> Vec<Case> {
    let data = [
        (
            "receiver",
            "logger.printLine(value)\nlogger.printLine(other)\nbuffer.printBuffer(value)\nlogger.pr",
            "pr",
            "printLine()",
        ),
        (
            "case-correct prefix",
            "read_config ReadConfig\nRe",
            "Re",
            "ReadConfig",
        ),
        (
            "case-sensitive initials",
            "readConfig RendererContext\nRC",
            "RC",
            "RendererContext",
        ),
        (
            "acronym boundaries",
            "hatching HTTPClient\nhc",
            "hc",
            "HTTPClient",
        ),
        (
            "snake initials",
            "get_user_config garbuncle\nguc",
            "guc",
            "get_user_config",
        ),
        (
            "constructor receiver family",
            "first = Socket()\nfirst.sendPacket(data)\nfirst.sendPacket(data)\nsecond = Socket()\nsecond.sp",
            "sp",
            "sendPacket()",
        ),
        (
            "factory result receiver",
            "makeReader().readPacket(data)\nmakeReader().readPacket(data)\nreader = makeReader()\nreader.rp",
            "rp",
            "readPacket()",
        ),
        (
            "multiline receiver",
            "other.sendPing() other.sendPing() other.sendPing()\nclient.transport.sendPacket(data)\nclient.transport.sendPacket(data)\nclient.transport\n    .sp",
            "sp",
            "sendPacket()",
        ),
        (
            "multiline argument position",
            "send(channelBeta, payloadAlpha)\nsend(channelGamma, payloadAlpha)\nparse(parseValue)\nparse(parseValue)\nsend(\n    channelDelta,\n    pa",
            "pa",
            "payloadAlpha",
        ),
        (
            "nested argument position",
            "send(select(first, second), payloadRight)\nsend(select(third, fourth), payloadRight)\nparse(parseOther)\nsend(select(alpha, beta), pa",
            "pa",
            "payloadRight",
        ),
        (
            "array commas keep outer argument",
            "send([first, second], payloadRight)\nsend([third, fourth], payloadRight)\nparse(parseOther)\nsend([alpha, beta], pa",
            "pa",
            "payloadRight",
        ),
        (
            "multiline assignment",
            "enabled =\n    true\nenabled =\n    true\nother = truncate\nenabled =\n    tr",
            "tr",
            "true",
        ),
        (
            "named value",
            "mode: fastMode\nmode: fastMode\nfactoryName\nfactoryName\nmode: fa",
            "fa",
            "fastMode",
        ),
        (
            "quoted configuration key",
            "{\"theme\": \"monokai\"}\n{\"theme\": \"monokai\"}\nmodelData modelData modelData\n{\"theme\": \"mo",
            "mo",
            "monokai",
        ),
        (
            "visible local scope",
            "fn other() { let packetAbroad = value; packetAbroad; packetAbroad; }\nfn current() {\n    let packetLocal = value;\n    pa",
            "pa",
            "packetLocal",
        ),
        (
            "code over comment frequency",
            "// sendPacket() sendPacket() sendPacket() sendPacket()\n\"sendPacket() sendPacket()\"\nlet sendPlain = value;\nse",
            "se",
            "sendPlain",
        ),
        (
            "callback value stays a reference",
            "sendPacket() sendPacket() sendPacket() sendPacket()\nhandler = sendPacket\nhandler = sendPacket\nhandler = se",
            "se",
            "sendPacket",
        ),
        (
            "property does not become a call",
            "worker.commit()\nworker.commit()\nconfig.commit = true\nconfig.co",
            "co",
            "commit",
        ),
        (
            "complete an already typed call",
            "renderFrame()\nrenderFrame()\nrenderFrame",
            "renderFrame",
            "renderFrame()",
        ),
        (
            "transposed letters",
            "printStatus printStatus printPacket\npritnS",
            "pritnS",
            "printStatus",
        ),
        (
            "second transposition",
            "packetPayload packetPayload\npakcetP",
            "pakcetP",
            "packetPayload",
        ),
        (
            "upper-case constant",
            "const TRUE_ENABLED = true;\nconst translatedValue = true;\nTR",
            "TR",
            "TRUE_ENABLED",
        ),
    ];
    let mut cases: Vec<_> = data
        .into_iter()
        .map(|(name, source, prefix, expected)| Case {
            name,
            source: source.into(),
            prefix,
            expected,
            language: "Unknown",
        })
        .collect();
    cases.push(Case {
        name: "exact snippet",
        source: "fn noisy() {}\nfn".into(),
        prefix: "fn",
        expected: "fn",
        language: "Rust",
    });
    cases
}

fn active(source: &str) -> VtCompleteSnapshot {
    let rope = ropey::Rope::from_str(source);
    let cursor_line = rope.len_lines() - 1;
    let cursor_col = rope.line(cursor_line).len_chars();
    VtCompleteSnapshot {
        source: Default::default(),
        rope,
        cursor_line,
        cursor_col,
        document_id: 91,
        active: true,
    }
}

#[test]
fn prediction_quality_corpus() {
    let mut new_hits = 0;
    let mut failures = Vec::new();
    let mut report = Vec::new();
    let cases = cases();
    for case in &cases {
        let active = active(&case.source);
        let index = crate::completion_index::CompletionIndex::default();
        let result = compute_contextual_invoked(
            &active,
            case.prefix,
            case.language,
            &index,
            &mut TokenCache::default(),
            true,
            || false,
        )
        .unwrap();
        let current_hit = result
            .suggestions
            .first()
            .is_some_and(|word| word == case.expected);
        new_hits += usize::from(current_hit);
        if !current_hit {
            failures.push(format!(
                "{}: expected {:?}, got {:?}",
                case.name, case.expected, result.suggestions
            ));
        }
        report.push(
            serde_json::json!({ "case": case.name, "expected": case.expected,
            "current": result.suggestions, "currentTop1": current_hit }),
        );
    }
    eprintln!("completion quality: current={new_hits}/{}", cases.len());
    if let Some(path) = std::env::var_os("VTCODE_COMPLETION_EVALUATION") {
        std::fs::write(path, serde_json::to_vec_pretty(&serde_json::json!({"cases": report, "currentTop1": new_hits, "total": cases.len()})).unwrap()).unwrap();
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn learned_identifier_precedes_a_partial_snippet_while_the_snippet_stays_available() {
    let active = active(
        "consume(sensor, temperatureReady)\nconsume(sensor, temperatureReady)\nconsume(sensor, te",
    );
    let result = compute_contextual_invoked(
        &active,
        "te",
        "Rust",
        &crate::completion_index::CompletionIndex::default(),
        &mut TokenCache::default(),
        false,
        || false,
    )
    .unwrap();
    assert_eq!(
        result.suggestions.first().map(String::as_str),
        Some("temperatureReady"),
        "{:?}",
        result.suggestions
    );
    let snippet = result
        .suggestions
        .iter()
        .position(|word| word == "test")
        .expect("partial snippet remains deliberately selectable");
    assert_eq!(
        result.details[snippet].kind,
        crate::app_vtcomplete::CompletionKind::Snippet
    );
}

#[test]
fn automatic_suggestions_are_selective_but_manual_completion_can_broaden_the_list() {
    let active = active("logger.printLine()\nlogger.printLine()\ngrandPaRade\nlogger.pr");
    let index = crate::completion_index::CompletionIndex::default();
    let automatic = compute_contextual_invoked(
        &active,
        "pr",
        "Unknown",
        &index,
        &mut TokenCache::default(),
        false,
        || false,
    )
    .unwrap();
    let manual = compute_contextual_invoked(
        &active,
        "pr",
        "Unknown",
        &index,
        &mut TokenCache::default(),
        true,
        || false,
    )
    .unwrap();
    assert_eq!(
        automatic.suggestions.first().map(String::as_str),
        Some("printLine()")
    );
    assert!(
        !automatic
            .suggestions
            .iter()
            .any(|word| word == "grandPaRade")
    );
    assert!(manual.suggestions.iter().any(|word| word == "grandPaRade"));
    let active = self::active("printStatus printStatus\npritnS");
    let automatic = compute_contextual_invoked(
        &active,
        "pritnS",
        "Unknown",
        &index,
        &mut TokenCache::default(),
        false,
        || false,
    )
    .unwrap();
    assert!(
        automatic.suggestions.is_empty(),
        "weak typo guesses should not intrude while typing"
    );
}

#[test]
fn typing_reuses_the_structural_prefix_and_invalidates_edits_above_it() {
    let source = "let reader = Reader();\nreader.readPacket()\nreader.readPacket()\nreader.r";
    let mut active = active(source);
    let mut cache = TokenCache::default();
    let index = crate::completion_index::CompletionIndex::default();
    compute_contextual(&active, "r", "Unknown", &index, &mut cache, || false).unwrap();
    let initial = cache.prediction.parsed_bytes;
    for ch in ['e', 'a', 'd'] {
        active.rope.insert_char(active.rope.len_chars(), ch);
        active.cursor_col += 1;
        let prefix: String = active
            .rope
            .line(active.cursor_line)
            .chars()
            .skip(7)
            .collect();
        compute_contextual(&active, &prefix, "Unknown", &index, &mut cache, || false).unwrap();
    }
    assert_eq!(cache.prediction.builds, 1);
    assert!(
        cache.prediction.parsed_bytes - initial < 80,
        "warm typing reparsed the document prefix"
    );
    active.rope.insert(0, "let other = Reader();\n");
    active.cursor_line += 1;
    compute_contextual(&active, "read", "Unknown", &index, &mut cache, || false).unwrap();
    assert_eq!(cache.prediction.builds, 2);
}

#[test]
fn multiline_prose_is_quiet_and_fake_calls_cannot_change_code_insertion() {
    let index = crate::completion_index::CompletionIndex::default();
    for source in [
        "text = '''multiline\nprintStatus\npri",
        "/* nested /* comment */\nprintStatus\npri",
        "text = `multiline\nprintStatus\npri",
    ] {
        let result = compute_contextual_invoked(
            &active(source),
            "pri",
            "Unknown",
            &index,
            &mut TokenCache::default(),
            false,
            || false,
        )
        .unwrap();
        assert!(
            result.suggestions.is_empty(),
            "{source}: {:?}",
            result.suggestions
        );
    }
    let source = "let actualCall = value;\n// actualCall() actualCall() actualCall()\n\"actualCall() actualCall()\"\nact";
    let result = compute_contextual(
        &active(source),
        "act",
        "Unknown",
        &index,
        &mut TokenCache::default(),
        || false,
    )
    .unwrap();
    assert!(result.suggestions.iter().any(|word| word == "actualCall"));
    assert!(!result.suggestions.iter().any(|word| word == "actualCall()"));
    for prose in [
        "/*\nactualCall()\nactualCall()\n*/",
        "\"\"\"\nactualCall()\nactualCall()\n\"\"\"",
    ] {
        let source = format!("let handler = actualCall;\n{prose}\nact");
        let result = compute_contextual(
            &active(&source),
            "act",
            "Unknown",
            &index,
            &mut TokenCache::default(),
            || false,
        )
        .unwrap();
        assert!(
            result.suggestions.iter().any(|word| word == "actualCall"),
            "{:?}",
            result.suggestions
        );
        assert!(!result.suggestions.iter().any(|word| word == "actualCall()"));
    }
    let mut before_prose = active("act\n/*\nactualCall()\nactualCall()\n*/");
    before_prose.cursor_line = 0;
    before_prose.cursor_col = 3;
    let result = compute_contextual(
        &before_prose,
        "act",
        "Unknown",
        &index,
        &mut TokenCache::default(),
        || false,
    )
    .unwrap();
    assert!(!result.suggestions.iter().any(|word| word == "actualCall()"));
}
