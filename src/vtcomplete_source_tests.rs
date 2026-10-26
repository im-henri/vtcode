//! Exercise current-file and occurrence evidence through the interactive ranker.
//! File names and import-like text do not establish completion relationships.
use super::*;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn project(
    files: &[(&str, &str)],
) -> (
    tempfile::TempDir,
    PathBuf,
    crate::completion_index::CompletionIndex,
) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let index = crate::completion_index::CompletionIndex::new_for_test(
        root.clone(),
        directory.path().join("cache"),
    );
    for &(path, text) in files {
        index.observe(root.join(path), ropey::Rope::from_str(text));
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let status = index.status();
        if status.indexed_files == files.len()
            && status.phase == crate::completion_index::IndexPhase::Idle
        {
            break;
        }
        assert!(Instant::now() < deadline, "{status:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
    (directory, root, index)
}

fn suggest(
    index: &crate::completion_index::CompletionIndex,
    path: &Path,
    language: &str,
    text: &str,
    prefix: &str,
) -> Vec<String> {
    suggest_invoked(index, path, language, text, prefix, false)
}

fn suggest_invoked(
    index: &crate::completion_index::CompletionIndex,
    path: &Path,
    language: &str,
    text: &str,
    prefix: &str,
    explicit: bool,
) -> Vec<String> {
    let rope = ropey::Rope::from_str(text);
    let cursor_line = rope.len_lines() - 1;
    let cursor_col = rope.line(cursor_line).len_chars();
    let active = VtCompleteSnapshot {
        source: crate::completion_source::Source::new(Some(path), language),
        rope,
        document_id: 902,
        cursor_line,
        cursor_col,
        active: true,
    };
    compute_contextual_invoked(
        &active,
        prefix,
        language,
        index,
        &mut TokenCache::default(),
        explicit,
        || false,
    )
    .unwrap()
    .suggestions
}

#[test]
fn distant_current_file_words_reach_the_popup_ahead_of_headers_and_foreign_frequency() {
    let current = "#include \"helper.h\"\nreadCurrent\n".to_owned() + &"\n".repeat(260) + "read";
    let foreign = "readJavascript ".repeat(1000);
    let (_directory, root, index) = project(&[
        ("main.c", &current),
        ("helper.h", "readHeader"),
        ("page.js", &foreign),
    ]);
    let words = suggest(&index, &root.join("main.c"), "C", &current, "read");
    assert_eq!(words.first().map(String::as_str), Some("readCurrent"));
    // The include-like path does not promote the header's whole vocabulary.
    // Explicit completion still admits that indexed spelling.
    let manual = suggest_invoked(&index, &root.join("main.c"), "C", &current, "read", true);
    assert!(manual.iter().any(|word| word == "readHeader"), "{manual:?}");
    assert!(
        !words.iter().any(|word| word == "readJavascript"),
        "{words:?}"
    );
}

#[test]
fn distant_active_file_relationship_adapts_only_identifiers_already_visible_in_the_buffer() {
    let teacher = "channelOld = source(); chosenOld = handler(); handlerOld = noise(); unregister(channelOld, chosenOld);\n";
    let current = teacher.to_owned()
        + &"\n".repeat(4100)
        + "channelNew = source(); handlerNew = handler(); handlerBad = noise(); unregister(channelNew, ha";
    let (_directory, root, index) = project(&[("main.unknown", &current)]);
    let path = root.join("main.unknown");
    let no_active = crate::completion_index::examples::with_mode(
        crate::completion_index::examples::Mode::NoActiveFileOccurrences,
        || suggest(&index, &path, "Unknown", &current, "ha"),
    );
    let words = suggest(&index, &path, "Unknown", &current, "ha");
    assert_eq!(
        words.first().map(String::as_str),
        Some("handlerNew"),
        "{words:?}"
    );
    assert_ne!(
        no_active.first().map(String::as_str),
        Some("handlerNew"),
        "{no_active:?}"
    );
}

#[test]
fn learned_callback_reference_does_not_inherit_unrelated_calls_of_the_same_identifier() {
    let current = "register(chanA, handlerA); unregister(chanA, handlerA); handlerB(); handlerB(); register(chanB, handlerB); unregister(chanB, ha";
    let (_directory, root, index) = project(&[("main.unknown", current)]);
    let words = suggest(&index, &root.join("main.unknown"), "Unknown", current, "ha");
    assert_eq!(
        words.first().map(String::as_str),
        Some("handlerB"),
        "{words:?}"
    );
    let calls = suggest(
        &index,
        &root.join("main.unknown"),
        "Unknown",
        "handlerB(); handlerB(); ha",
        "ha",
    );
    assert_eq!(
        calls.first().map(String::as_str),
        Some("handlerB()"),
        "{calls:?}"
    );
}

#[test]
fn strong_foreign_context_beats_an_unrelated_local_identifier() {
    let current = "use(\"embedded.tcl\");\nlet sendLocal = value;\nbridge.send";
    let (_directory, root, index) = project(&[
        ("main.c", current),
        ("embedded.tcl", "bridge.sendPacket(); bridge.sendPacket();"),
    ]);
    let words = suggest(&index, &root.join("main.c"), "C", current, "send");
    assert_eq!(
        words.first().map(String::as_str),
        Some("sendPacket()"),
        "{words:?}"
    );
    assert!(words.iter().any(|word| word == "sendLocal"));
}

#[test]
fn fresh_unknown_buffers_use_distinctive_code_to_find_related_unknown_files() {
    let shared = "launchSequencer transportSession commitTransaction";
    let definitions = format!("{shared}\nnextPacket");
    let (_directory, root, index) = project(&[
        ("library.other_mystery", &definitions),
        ("page.js", "nextAnimation nextAnimation nextAnimation"),
    ]);
    let text = format!("{shared}\nnext");
    let words = suggest(
        &index,
        &root.join("program.mystery"),
        "Unknown",
        &text,
        "next",
    );
    assert_eq!(
        words.first().map(String::as_str),
        Some("nextPacket"),
        "{words:?}"
    );
}

#[test]
fn foreign_function_usage_does_not_add_parentheses_to_a_c_field() {
    let foreign = "config.commit(); ".repeat(100);
    // Field usage is visible in the active program. A header path alone cannot
    // distinguish these contradictory same-spelling occurrences.
    let current = "config.commit = true; config.commit = false;\nconfig.co";
    let (_directory, root, index) = project(&[
        ("main.c", current),
        (
            "definitions.h",
            "config.commit = true; config.commit = false;",
        ),
        ("page.js", &foreign),
    ]);
    let words = suggest(&index, &root.join("main.c"), "C", current, "co");
    assert_eq!(
        words.first().map(String::as_str),
        Some("commit"),
        "{words:?}"
    );
}

#[test]
fn generated_call_repetition_does_not_fabricate_certainty_for_a_fresh_use() {
    let current = "#include \"definitions.h\"\nconfig.co";
    let foreign = "config.commit(); ".repeat(100);
    let (_directory, root, index) = project(&[
        ("main.c", current),
        (
            "definitions.h",
            "config.commit = true; config.commit = false;",
        ),
        ("page.js", &foreign),
    ]);
    let words = suggest(&index, &root.join("main.c"), "C", current, "co");
    assert_eq!(
        words.first().map(String::as_str),
        Some("commit"),
        "{words:?}"
    );
}

#[test]
fn a_comment_in_the_current_file_cannot_hide_a_real_cross_language_code_candidate() {
    for current in [
        "use(\"embedded.tcl\");\n// readBridge is an embedded command\nread",
        "use(\"embedded.tcl\");\n\"readBridge\"\nread",
    ] {
        let (_directory, root, index) = project(&[
            ("main.c", current),
            ("embedded.tcl", "readBridge(); readBridge();"),
        ]);
        let words = suggest(&index, &root.join("main.c"), "C", current, "read");
        assert!(words.iter().any(|word| word == "readBridge()"), "{words:?}");
    }
}

#[test]
fn repeated_manual_choices_can_promote_a_cross_language_exception_in_that_context() {
    let current = "read";
    let (_directory, root, index) = project(&[
        ("main.c", current),
        ("other.c", "readStatic readStatic"),
        ("embedded.tcl", "readExternalBridge"),
    ]);
    let path = root.join("main.c");
    assert_eq!(
        suggest(&index, &path, "C", current, "read")[0],
        "readStatic"
    );
    let context = crate::completion_context::Context {
        source_family: crate::completion_source::Kind::new(Some(&path), "C").family,
        source_scope: crate::completion_source::Source::new(Some(&path), "C").file,
        ..Default::default()
    };
    for _ in 0..8 {
        index.record_choice("readExternalBridge", "read", context, true);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        suggest(&index, &path, "C", current, "read")[0],
        "readExternalBridge"
    );
    let js_context = crate::completion_context::Context {
        source_family: crate::completion_source::Kind::new(None, "JavaScript").family,
        ..Default::default()
    };
    assert!(index.history_scores("read", js_context, false).is_empty());
}

#[test]
#[ignore = "manual native latency with a mixed-language catalog and a 10k-line active buffer"]
fn benchmark_typed_native_project_hot_path() {
    let mut text = String::new();
    for row in 0..9997 {
        text.push_str(&format!(
            "client.getUserConfig(v{}); logger.printLine(value); buffer.pushMessage(value);\n",
            row % 64
        ));
    }
    text.push_str("logger.pr\nclient.guc\nbuffer.pm");
    let header: String = (0..1500).map(|n| format!("printHeader{n} ")).collect();
    let foreign: String = (0..1500).map(|n| format!("printJavascript{n} ")).collect();
    let (_directory, root, index) = project(&[
        ("main.c", &text),
        ("definitions.h", &header),
        ("page.js", &foreign),
    ]);
    let rope = ropey::Rope::from_str(&text);
    let source = crate::completion_source::Source::new(Some(&root.join("main.c")), "C");
    let cases: Vec<_> = [
        ("pr", "printLine()", 9),
        ("guc", "getUserConfig()", 10),
        ("pm", "pushMessage()", 9),
    ]
    .into_iter()
    .enumerate()
    .map(|(line, (prefix, expected, col))| {
        (
            VtCompleteSnapshot {
                source,
                rope: rope.clone(),
                document_id: 700,
                cursor_line: 9997 + line,
                cursor_col: col,
                active: true,
            },
            prefix,
            expected,
        )
    })
    .collect();
    let mut cache = TokenCache::default();
    for (active, prefix, expected) in &cases {
        let result =
            compute_contextual_invoked(active, prefix, "C", &index, &mut cache, false, || false)
                .unwrap();
        assert_eq!(result.suggestions[0], *expected);
    }
    let started = Instant::now();
    for i in 0..1000 {
        let (active, prefix, expected) = &cases[i % cases.len()];
        let result =
            compute_contextual_invoked(active, prefix, "C", &index, &mut cache, false, || false)
                .unwrap();
        assert_eq!(result.suggestions[0], *expected);
        std::hint::black_box(result);
    }
    eprintln!(
        "typed native completion, 10k lines + C header/JavaScript vocabulary: {:?} per warm request",
        started.elapsed() / 1000
    );
}

#[test]
fn automatic_popup_keeps_standalone_tests_separate_even_in_the_same_directory() {
    let first = "config.readFirst(); config.readFirst();\nconfig.read";
    let second = "config.readSecond(); config.readSecond();";
    let (_directory, root, index) = project(&[
        ("cores/arm/tests/uart/main.c", first),
        ("cores/arm/tests/uart/standalone.c", second),
        ("cores/arm/tests/cache/main.c", second),
        ("cores/riscv/tests/uart/main.cpp", second),
    ]);
    let words = suggest(
        &index,
        &root.join("cores/arm/tests/uart/main.c"),
        "C",
        first,
        "read",
    );
    assert!(words.iter().any(|word| word == "readFirst()"), "{words:?}");
    assert!(
        !words.iter().any(|word| word.starts_with("readSecond")),
        "{words:?}"
    );
}

#[test]
fn ordered_occurrences_find_the_correct_core_helper_across_languages() {
    let current = "decodeInstruction registerLayout emitPipeline\nread";
    let (_directory, root, index) = project(&[
        ("cores/arm/src/entry.cps", current),
        (
            "scripts/arm.py",
            "decodeInstruction registerLayout emitPipeline\nreadArmHelper",
        ),
        ("scripts/riscv.py", "readRiscvHelper"),
        ("cores/riscv/src/entry.cps", "readRiscvCore"),
    ]);
    let words = suggest(
        &index,
        &root.join("cores/arm/src/entry.cps"),
        "Unknown",
        current,
        "read",
    );
    assert!(
        words.iter().any(|word| word == "readArmHelper"),
        "{words:?}"
    );
    assert_eq!(
        words.first().map(String::as_str),
        Some("readArmHelper"),
        "{words:?}"
    );
    assert!(
        !words.iter().any(|word| word.starts_with("readRiscv")),
        "{words:?}"
    );
}

#[test]
fn manual_completion_retains_disconnected_project_words() {
    let (_directory, root, index) = project(&[("other/program.c", "readUnrelated")]);
    let text = "readLocal\nread";
    let prefix = "read";
    let active = VtCompleteSnapshot {
        source: crate::completion_source::Source::new(Some(&root.join("main.c")), "C"),
        rope: ropey::Rope::from_str(text),
        document_id: 903,
        cursor_line: 1,
        cursor_col: prefix.len(),
        active: true,
    };
    assert!(
        !suggest(&index, &root.join("main.c"), "C", text, prefix)
            .iter()
            .any(|word| word == "readUnrelated")
    );
    let result = compute_contextual_invoked(
        &active,
        prefix,
        "C",
        &index,
        &mut TokenCache::default(),
        true,
        || false,
    )
    .unwrap();
    assert!(
        result
            .suggestions
            .iter()
            .any(|word| word == "readUnrelated"),
        "{:?}",
        result.suggestions
    );
}

#[test]
fn a_fresh_buffer_does_not_filter_words_by_a_neighboring_module_name() {
    let (_directory, root, index) = project(&[
        ("helpers/transport.py", "readTransport"),
        ("elsewhere/transport.py", "readOtherTransport"),
        ("helpers/unrelated.py", "readUnrelated"),
    ]);
    for language in ["Python", "My Custom Language"] {
        let words = suggest(
            &index,
            &root.join("helpers/new_program.whatever"),
            language,
            "import transport\nread",
            "read",
        );
        assert!(
            words.iter().any(|word| word == "readTransport"),
            "{words:?}"
        );
        // A bare module-like spelling supplies no ordered occurrence linking
        // the unfinished identifier to one of these independent examples.
        assert!(
            words.iter().any(|word| word == "readOtherTransport"),
            "{words:?}"
        );
        assert!(
            words.iter().any(|word| word == "readUnrelated"),
            "{words:?}"
        );
    }
}

#[test]
fn a_strong_pattern_can_cross_unknown_extensions_without_any_inferred_file_link() {
    let (_directory, root, index) = project(&[
        (
            "tools/unrelated-layout.custom_one",
            "decoder.emitInstruction(); decoder.emitInstruction();",
        ),
        ("elsewhere/noise.custom_two", "emitIrrelevant"),
    ]);
    let words = suggest(
        &index,
        &root.join("cores/new_core/entry.custom_three"),
        "Unknown",
        "decoder.em",
        "em",
    );
    assert_eq!(
        words.first().map(String::as_str),
        Some("emitInstruction()"),
        "{words:?}"
    );
}

#[test]
fn specific_spelling_can_surface_a_project_word_without_a_file_relationship() {
    let (_directory, root, index) =
        project(&[("elsewhere/source.unknown_one", "rebuildInstructionCache")]);
    let words = suggest(
        &index,
        &root.join("cores/new_core/entry.unknown_two"),
        "Unknown",
        "rebuildIns",
        "rebuildIns",
    );
    assert!(
        words.iter().any(|word| word == "rebuildInstructionCache"),
        "{words:?}"
    );
}

#[test]
fn global_patterns_do_not_require_a_special_receiver_name_or_known_language() {
    let (_directory, root, index) = project(&[(
        "elsewhere/logging.custom_one",
        "logger.printLine(); logger.printLine();",
    )]);
    let words = suggest(
        &index,
        &root.join("cores/new/entry.custom_two"),
        "Unknown",
        "logger.pr",
        "pr",
    );
    assert_eq!(
        words.first().map(String::as_str),
        Some("printLine()"),
        "{words:?}"
    );
}

#[test]
fn a_distant_matching_pattern_beats_a_nearer_different_use_of_the_same_identifier() {
    let (_directory, root, index) = project(&[
        (
            "nearby.c",
            "config.sendPacket = value; config.sendPacket = other;",
        ),
        (
            "elsewhere/embedded.custom",
            "bridge.sendPacket(); bridge.sendPacket();",
        ),
    ]);
    let words = suggest(&index, &root.join("main.c"), "C", "bridge.sp", "sp");
    assert_eq!(
        words.first().map(String::as_str),
        Some("sendPacket()"),
        "{words:?}"
    );
}

#[test]
fn project_words_remain_available_without_a_known_extension_context_or_file_connection() {
    let (_directory, root, index) =
        project(&[("elsewhere/library.custom_one", "decodeInstructionStream")]);
    let words = suggest(
        &index,
        &root.join("generated/entry.custom_two"),
        "Unknown",
        "dec",
        "dec",
    );
    assert!(
        words.iter().any(|word| word == "decodeInstructionStream"),
        "{words:?}"
    );
}
