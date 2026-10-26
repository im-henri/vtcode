//! Fixed source-derived copy/rename editing transformations, not independent code.
//! Selection uses lexical roles and positions before running either predictor.
use super::*;
use crate::completion_context::{Analyzer, Context, Domain, Event};
use crate::completion_index::examples::{Mode, with_mode};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const WINDOW_TOKENS: usize = 32;
const SOURCE_CAP: usize = 24;
const TOTAL_CAP: usize = 64;
const SAMPLE_STRIDE: usize = 3;

pub(super) fn warm_index_enabled() -> bool {
    std::env::var_os("VTCODE_LEARNING_REPLAY_WARM_INDEX").is_some_and(|value| value == "1")
}

fn warm_visible_state(
    project: &Project,
    session: &Session,
    active: &VtCompleteSnapshot,
    warm: bool,
) -> Option<serde_json::Value> {
    let publication = project.publish_visible(session, active, warm);
    if let Some(publication) = &publication {
        assert_eq!(
            publication["indexedFiles"], 1,
            "isolated per-source validation index"
        );
    }
    publication
}
struct Lexeme<'a> {
    start: usize,
    end: usize,
    word: Option<&'a str>,
    punctuation: Option<char>,
}

fn lexemes(source: &str) -> Vec<Lexeme<'_>> {
    let mut words = Vec::new();
    let mut start = None;
    for (offset, ch) in source
        .char_indices()
        .chain(std::iter::once((source.len(), ' ')))
    {
        if identifier_char(ch) {
            start.get_or_insert(offset);
            continue;
        }
        if let Some(begin) = start.take() {
            words.push(Lexeme {
                start: begin,
                end: offset,
                word: Some(&source[begin..offset]),
                punctuation: None,
            });
        }
        if offset < source.len() && (!ch.is_whitespace() || ch == '\n') {
            words.push(Lexeme {
                start: offset,
                end: offset + ch.len_utf8(),
                word: None,
                punctuation: Some(ch),
            });
        }
    }
    words
}

fn next_non_line(tokens: &[Lexeme<'_>], at: usize) -> Option<usize> {
    (at + 1..tokens.len()).find(|&i| tokens[i].punctuation != Some('\n'))
}

fn previous_non_line(tokens: &[Lexeme<'_>], at: usize) -> Option<usize> {
    (0..at).rev().find(|&i| tokens[i].punctuation != Some('\n'))
}

fn receiver(tokens: &[Lexeme<'_>], at: usize) -> bool {
    let Some(next) = next_non_line(tokens, at) else {
        return false;
    };
    tokens[next].punctuation == Some('.')
        || tokens[next].punctuation == Some(':')
            && tokens
                .get(next + 1)
                .is_some_and(|t| t.punctuation == Some(':'))
}

fn argument(tokens: &[Lexeme<'_>], at: usize) -> bool {
    let before = previous_non_line(tokens, at).and_then(|i| tokens[i].punctuation);
    let after = next_non_line(tokens, at).and_then(|i| tokens[i].punctuation);
    matches!(before, Some('(' | ',')) && matches!(after, Some(',' | ')'))
}

fn code_positions(source: &str) -> BTreeMap<usize, Context> {
    let bytes: Vec<_> = source
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(source.len()))
        .collect();
    let mut positions = BTreeMap::new();
    let mut analyzer = Analyzer::default();
    let mut observe = |event: Event<'_>| {
        if let Event::Word {
            context, offset, ..
        } = event
            && context.domain == Domain::Code
        {
            positions.insert(bytes[offset], context);
        }
    };
    analyzer.feed(source, &mut observe);
    analyzer.flush(observe);
    positions
}

struct Selected {
    source: String,
    language: &'static str,
    byte: usize,
    begin: usize,
    old: String,
    expected: String,
    text: String,
    prefix: String,
    tokens: usize,
}

fn select(source_name: &str, language: &'static str, source: &str) -> Vec<Selected> {
    let tokens = lexemes(source);
    let code = code_positions(source);
    let mut bound = BTreeSet::new();
    let mut eligible = 0;
    let mut selected = Vec::new();
    for (at, token) in tokens.iter().enumerate() {
        let Some(word) = token.word else {
            continue;
        };
        let Some(context) = code.get(&token.start) else {
            continue;
        };
        let after = next_non_line(&tokens, at);
        let assignment = after.is_some_and(|i| {
            tokens[i].punctuation == Some('=')
                && tokens.get(i + 1).is_none_or(|t| t.punctuation != Some('='))
        });
        if receiver(&tokens, at) || assignment {
            bound.insert(word);
        }
        if word.len() < 5
            || word.len() > 64
            || !word.is_ascii()
            || !word
                .chars()
                .next()
                .is_some_and(|ch| ch.is_alphabetic() || ch == '_' || ch == '$')
            || context.declaration
            || context.type_position
            || !bound.contains(word)
            || !(receiver(&tokens, at) || argument(&tokens, at))
            || after.is_some_and(|i| tokens[i].punctuation == Some('('))
        {
            continue;
        }
        let mut first = at.saturating_sub(WINDOW_TOKENS - 1);
        while first < at && !code.contains_key(&tokens[first].start) {
            first += 1;
        }
        if !tokens[first..at]
            .iter()
            .any(|t| t.word == Some(word) && code.contains_key(&t.start))
        {
            continue;
        }
        // Every replaced occurrence is a code token; do not change a quoted key
        // or comment while pretending it is a variable rename.
        if tokens[first..=at]
            .iter()
            .any(|t| t.word == Some(word) && !code.contains_key(&t.start))
        {
            continue;
        }
        if (first..at).any(|i| {
            tokens[i].word == Some(word)
                && next_non_line(&tokens, i)
                    .is_some_and(|next| tokens[next].punctuation == Some('('))
        }) {
            continue;
        }
        let anchored = (first..at).any(|i| {
            tokens[i].word.is_some_and(|name| name != word)
                && code.contains_key(&tokens[i].start)
                && next_non_line(&tokens, i)
                    .is_some_and(|next| tokens[next].punctuation == Some('('))
        });
        if !anchored {
            continue;
        }
        eligible += 1;
        if eligible % SAMPLE_STRIDE != 0 || selected.len() >= SOURCE_CAP {
            continue;
        }
        let expected = format!("{word}Next");
        // A fresh spelling is created by a deterministic rename, never selected
        // because either engine predicts it. Both spellings share the prefix.
        if source[..token.start].contains(&expected) {
            continue;
        }
        let mut renamed = String::new();
        let line_begin = source[..tokens[first].start]
            .rfind('\n')
            .map_or(0, |n| n + 1);
        let copy_first = tokens.partition_point(|token| token.start < line_begin);
        let mut copied = line_begin;
        for previous in &tokens[copy_first..at] {
            if previous.word == Some(word) && code.contains_key(&previous.start) {
                renamed.push_str(&source[copied..previous.start]);
                renamed.push_str(&expected);
                copied = previous.end;
            }
        }
        renamed.push_str(&source[copied..token.start]);
        let prefix_len = if selected.len() % 2 == 0 { 2 } else { 3 };
        let prefix = word[..prefix_len].to_owned();
        // The teacher is the real already-visible file prefix, rather than a
        // cropped example that removes the baseline's receiver/lexical history.
        // The copied fragment begins on a complete source line. The selected
        // witness/label span stays bounded to 32 tokens and never changes.
        let line_end = source[token.end..]
            .find('\n')
            .map_or(source.len(), |n| token.end + n);
        let original = &source[..line_end];
        let text = format!("{original}\n{renamed}{prefix}⟂");
        assert!(
            renamed.contains(&expected),
            "the target must already be visible in the renamed copy"
        );
        selected.push(Selected {
            source: source_name.to_owned(),
            language,
            byte: token.start,
            begin: tokens[first].start,
            old: word.to_owned(),
            expected,
            text,
            prefix,
            tokens: at - first + 1,
        });
    }
    selected
}

struct Source {
    name: String,
    language: &'static str,
    text: String,
    sha256: String,
}

#[derive(Deserialize)]
struct RuntimeFixtures {
    protocol: String,
    sources: Vec<RuntimeSource>,
}

#[derive(Deserialize)]
struct RuntimeSource {
    label: String,
    language: String,
    path: PathBuf,
    sha256: String,
}

fn corpus() -> (Vec<Source>, bool) {
    if let Some(path) = std::env::var_os("VTCODE_LEARNING_RENAME_FIXTURES") {
        // All fixture access and integrity checks happen before constructing a
        // completion snapshot or calling either predictor.
        let path = PathBuf::from(path);
        let fixtures: RuntimeFixtures = serde_json::from_slice(
            &std::fs::read(&path).expect("read frozen fixture manifest during test setup"),
        )
        .unwrap();
        assert_eq!(fixtures.protocol, "completion-final-validation-v1");
        assert_eq!(
            fixtures.sources.len(),
            2,
            "the final protocol selects one Python and one Rust source"
        );
        let mut labels = BTreeSet::new();
        let mut sources = Vec::new();
        for fixture in fixtures.sources {
            assert!(
                labels.insert(fixture.label.clone()),
                "fixture labels must be distinct"
            );
            let language = match fixture.language.as_str() {
                "Rust" => "Rust",
                "Python" => "Python",
                _ => panic!("final fixtures must use predeclared Rust/Python languages"),
            };
            let source_path = if fixture.path.is_absolute() {
                fixture.path
            } else {
                path.parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(fixture.path)
            };
            let bytes =
                std::fs::read(source_path).expect("read copied frozen source during test setup");
            assert!(
                (5 * 1024..=30 * 1024).contains(&bytes.len()),
                "do not replace an out-of-protocol fixture after seeing model quality"
            );
            let sha256 = format!("{:X}", Sha256::digest(&bytes));
            assert!(
                sha256.eq_ignore_ascii_case(&fixture.sha256),
                "frozen source integrity check"
            );
            sources.push(Source { name: fixture.label, language, text: String::from_utf8(bytes).expect("selected source must be UTF-8; report failure rather than choosing another file"), sha256 });
        }
        assert_eq!(
            sources[0].language, "Python",
            "predeclared source order is Python then Rust"
        );
        assert_eq!(sources[1].language, "Rust");
        return (sources, true);
    }
    let sources = [
        (
            "history.rs",
            "Rust",
            include_str!("../tests/fixtures/completion-replay/history.rs"),
        ),
        (
            "peer.py",
            "Python",
            include_str!("../tests/fixtures/completion-replay/peer.py"),
        ),
        (
            "bridge.c",
            "C",
            include_str!("../tests/fixtures/completion-replay/bridge.c"),
        ),
    ]
    .into_iter()
    .map(|(name, language, text)| Source {
        name: name.into(),
        language,
        text: text.into(),
        sha256: format!("{:X}", Sha256::digest(text.as_bytes())),
    })
    .collect();
    (sources, false)
}

fn selected(corpus: &[Source]) -> Vec<Selected> {
    let mut sources: Vec<_> = corpus
        .iter()
        .map(|source| select(&source.name, source.language, &source.text).into_iter())
        .collect();
    let mut selected = Vec::new();
    loop {
        let mut progressed = false;
        for source in &mut sources {
            if let Some(case) = source.next() {
                progressed = true;
                selected.push(case);
                if selected.len() == TOTAL_CAP {
                    return selected;
                }
            }
        }
        if !progressed {
            return selected;
        }
    }
}

fn manifest(cases: &[Selected], sources: &[Source], runtime: bool) -> serde_json::Value {
    let mut manifest = serde_json::json!({
        "protocol": "source-copy-single-rename-v3",
        "selectionParentSha256": "6CF5A414FBEA28317DB9D00F7E383FBC3E311F703958F08B89B3A18C1EF11C48",
        "rules": "v1 selection unchanged: 32-token contiguous witness ending at code target; earlier same target in witness; target is argument or receiver with preceding receiver/assignment evidence; preserve all other words/callees; rename target to targetNext; no string/comment/callee occurrence renamed in witness; first code-word witness start; every third eligible event; cap24 per source, round-robin cap64; typed2/3 character prefix only; no future copy suffix or index; render-only v3: teacher is full original source prefix through target line end, copied query begins on whole line containing witness start, rename only code tokens through target begin, preserve same witness/case IDs",
        "sources": sources.iter().map(|source| serde_json::json!({"path":source.name,"sha256":source.sha256})).collect::<Vec<_>>(),
        "cases": cases.iter().map(|case|serde_json::json!({"source":case.source,"byte":case.byte,"begin":case.begin,"old":case.old,"expected":case.expected,"prefix":case.prefix,"windowTokens":case.tokens})).collect::<Vec<_>>()
    });
    if runtime {
        manifest
            .as_object_mut()
            .unwrap()
            .remove("selectionParentSha256");
        manifest["selectionRulesManifestSha256"] =
            "6CF5A414FBEA28317DB9D00F7E383FBC3E311F703958F08B89B3A18C1EF11C48".into();
        manifest["fixtureScope"] = "fresh-validation".into();
        manifest["sourceCoverage"] = serde_json::json!(sources.iter().map(|source|serde_json::json!({"source":source.name,"cases":cases.iter().filter(|case|case.source==source.name).count()})).collect::<Vec<_>>());
    }
    if warm_index_enabled() {
        manifest["warmIndex"] = true.into();
        manifest["indexState"] = "exact visible Rope published before every query; one active indexed file; both engines share that state".into();
    }
    manifest
}

#[test]
fn source_rename_selection_manifest() {
    let (sources, runtime) = corpus();
    let cases = selected(&sources);
    let manifest = manifest(&cases, &sources, runtime);
    let original: serde_json::Value = serde_json::from_str(include_str!(
        "../tests/fixtures/completion-replay/rename-selection-v1.json"
    ))
    .unwrap();
    if !runtime {
        assert_eq!(
            manifest["cases"], original["cases"],
            "render repair must not change selected cases after v1 outputs were observed"
        );
    }
    eprintln!(
        "source rename selection {}",
        serde_json::to_string_pretty(&manifest).unwrap()
    );
    assert!(runtime || !cases.is_empty());
    assert!(cases.len() <= TOTAL_CAP);
    assert!(cases.iter().all(|case| case.tokens <= WINDOW_TOKENS));
    if let Some(path) = std::env::var_os("VTCODE_LEARNING_RENAME_MANIFEST") {
        std::fs::write(path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    }
}

pub(super) fn requested_mode() -> Mode {
    let requested_mode =
        std::env::var("VTCODE_LEARNING_RENAME_MODE").unwrap_or_else(|_| "Full".into());
    let normalized: String = requested_mode
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    match normalized.as_str() {
        "full" | "longcontexthalf16" | "conditionalonlyhalf16" => Mode::Full,
        "literalonly" => Mode::LiteralOnly,
        "norightcontext" => Mode::NoRightContext,
        "nodeduplicate" | "nodeduplication" => Mode::NoDeduplicate,
        "nostatistical" => Mode::NoStatistical,
        "rawworkspaceprior" => Mode::RawWorkspacePrior,
        "disabled" => Mode::Disabled,
        "noextension" => Mode::NoExtension,
        "noliteralbackoff" => Mode::NoLiteralBackoff,
        "fixedchannelorder" => Mode::FixedChannelOrder,
        "mixturescale192" => Mode::MixtureScale192,
        "phase1" => Mode::Phase1,
        "uniformconcentration" => Mode::UniformConcentration,
        "unconditionalfileprior" => Mode::UnconditionalFilePrior,
        "groundedactivefile" => Mode::Full,
        "noactivefileoccurrences" => Mode::NoActiveFileOccurrences,
        _ => panic!("unrecognized VTCODE_LEARNING_RENAME_MODE: {requested_mode}"),
    }
}

#[test]
fn learning_source_rename_replay_quality() {
    let mode = requested_mode();
    let warm = warm_index_enabled();
    let (sources, runtime) = corpus();
    let cases = selected(&sources);
    let manifest = manifest(&cases, &sources, runtime);
    assert!(
        !runtime || std::env::var_os("VTCODE_LEARNING_RENAME_MANIFEST").is_some(),
        "freeze a fresh case manifest before any validation predictions"
    );
    if let Some(path) = std::env::var_os("VTCODE_LEARNING_RENAME_MANIFEST") {
        let frozen: serde_json::Value = serde_json::from_slice(
            &std::fs::read(path).expect("freeze the selection manifest before running predictions"),
        )
        .unwrap();
        assert_eq!(
            manifest, frozen,
            "selection must not change after inspecting model outputs"
        );
    }
    let mut totals = BTreeMap::<&str, Metrics>::new();
    let mut rows = Vec::new();
    for native in [false, true] {
        let mut projects = BTreeMap::new();
        let group = match (runtime, native, warm) {
            (false, false, false) => "source-rename",
            (false, true, false) => "source-rename-native",
            (true, false, false) => "source-rename-validation",
            (true, true, false) => "source-rename-validation-native",
            (false, false, true) => "source-rename-warm",
            (false, true, true) => "source-rename-native-warm",
            (true, false, true) => "source-rename-validation-warm",
            (true, true, true) => "source-rename-validation-native-warm",
        };
        totals.entry(group).or_default();
        for (i, case) in cases.iter().enumerate() {
            let project = projects
                .entry(case.source.as_str())
                .or_insert_with(|| Project::new(&[]));
            let (name, path) = match case.source.as_str() {
                "history.rs" => ("history.rs", "history.rs"),
                "peer.py" => ("peer.py", "peer.py"),
                "bridge.c" => ("bridge.c", "bridge.c"),
                _ => (
                    "fresh-validation-source",
                    if case.language == "Rust" {
                        "fresh-validation.rs"
                    } else {
                        "fresh-validation.py"
                    },
                ),
            };
            let session = Session {
                name,
                path,
                language: if native { case.language } else { "Unknown" },
                files: vec![],
                probes: vec![],
            };
            let probe = Probe {
                name: format!(
                    "{}:byte{}:{}=>{}",
                    case.source, case.byte, case.old, case.expected
                ),
                group,
                text: case.text.clone(),
                prefix: case.prefix.clone(),
                expected: Some(case.expected.clone()),
            };
            let active = project.snapshot(&session, &probe, 9000 + i as u64);
            let publication = warm_visible_state(project, &session, &active, warm);
            begin_trace(probe.expected.as_deref());
            let new = with_mode(mode, || {
                compute_contextual_invoked(
                    &active,
                    &probe.prefix,
                    session.language,
                    &project.index,
                    &mut TokenCache::default(),
                    false,
                    || false,
                )
            })
            .unwrap();
            let new_recall = candidate_recalled();
            let after = totals.entry(group).or_default();
            after.add(&probe, &new.suggestions, new_recall);
            rows.push(serde_json::json!({"source":case.source,"byte":case.byte,"group":group,"warmIndex":warm,"indexPublication":publication,"prefix":case.prefix,"expected":case.expected,"current":new.suggestions,"currentCandidateRecalled":new_recall}));
        }
    }
    let report = serde_json::json!({"mode":format!("{mode:?}"),"warmIndex":warm,"protocol":manifest,"groups":totals.iter().map(|(group,new)|serde_json::json!({"group":group,"current":new.json()})).collect::<Vec<_>>(),"rows":rows});
    eprintln!(
        "source rename replay {}",
        serde_json::to_string_pretty(&report["groups"]).unwrap()
    );
    if let Some(path) = std::env::var_os("VTCODE_LEARNING_RENAME_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
}

#[test]
#[ignore = "fresh ordinary-source validation only after candidate freeze and fixture selection"]
fn learning_runtime_source_append_validation() {
    let (sources, runtime) = corpus();
    assert!(
        runtime,
        "requires the frozen VTCODE_LEARNING_RENAME_FIXTURES manifest; do not select a source from model outcomes"
    );
    let mode = requested_mode();
    let warm = warm_index_enabled();
    let mut totals = BTreeMap::<&str, Metrics>::new();
    let mut source_totals = BTreeMap::<(String, &str), Metrics>::new();
    let mut rows = Vec::new();
    for native in [false, true] {
        let (group, hard_group) = match (native, warm) {
            (false, false) => ("validation-append", "validation-append-hard"),
            (true, false) => ("validation-append-native", "validation-append-native-hard"),
            (false, true) => ("validation-append-warm", "validation-append-hard-warm"),
            (true, true) => (
                "validation-append-native-warm",
                "validation-append-native-hard-warm",
            ),
        };
        totals.entry(group).or_default();
        totals.entry(hard_group).or_default();
        for (source_index, source) in sources.iter().enumerate() {
            let project = Project::new(&[]);
            // This is the identical chronological sampler used for development:
            // every seventh repeated code identifier, 5-80 ASCII bytes, cap64,
            // alternating 2/3-character prefixes. It supplies no future source.
            let mut session = recorded_session("fresh-validation", &source.text);
            session.language = if native { source.language } else { "Unknown" };
            session.path = if source.language == "Rust" {
                "fresh-validation.rs"
            } else {
                "fresh-validation.py"
            };
            let mut new_cache = TokenCache::default();
            source_totals
                .entry((source.name.clone(), group))
                .or_default();
            for probe in &mut session.probes {
                probe.group = group;
            }
            for probe in &session.probes {
                let (left, _) = probe.text.split_once('⟂').unwrap();
                let before = &left[..left.len() - probe.prefix.len()];
                let hard = before
                    .split(|ch: char| !identifier_char(ch))
                    .filter(|word| {
                        word.starts_with(&probe.prefix) && word.len() > probe.prefix.len()
                    })
                    .collect::<BTreeSet<_>>()
                    .len()
                    >= 3;
                let active = project.snapshot(&session, probe, 12000 + source_index as u64);
                let publication = warm_visible_state(&project, &session, &active, warm);
                begin_trace(probe.expected.as_deref());
                let new = with_mode(mode, || {
                    compute_contextual_invoked(
                        &active,
                        &probe.prefix,
                        session.language,
                        &project.index,
                        &mut new_cache,
                        false,
                        || false,
                    )
                })
                .unwrap();
                let new_recall = candidate_recalled();
                for key in [Some(group), hard.then_some(hard_group)]
                    .into_iter()
                    .flatten()
                {
                    let after = totals.entry(key).or_default();
                    after.add(probe, &new.suggestions, new_recall);
                }
                let after = source_totals
                    .entry((source.name.clone(), group))
                    .or_default();
                after.add(probe, &new.suggestions, new_recall);
                rows.push(serde_json::json!({"source":source.name,"group":group,"warmIndex":warm,"indexPublication":publication,"probe":probe.name,"hard":hard,"prefix":probe.prefix,"expected":probe.expected,"current":new.suggestions,"currentCandidateRecalled":new_recall}));
            }
        }
    }
    let report = serde_json::json!({
        "protocol":"completion-final-validation-v1",
        "kind":"ordinary chronological append; paired Unknown/native events",
        "mode":format!("{mode:?}"),
        "warmIndex":warm,
        "sources":sources.iter().map(|source|serde_json::json!({"source":source.name,"language":source.language,"sha256":source.sha256})).collect::<Vec<_>>(),
        "groups":totals.iter().map(|(group,new)|serde_json::json!({"group":group,"current":new.json()})).collect::<Vec<_>>(),
        "perSource":source_totals.iter().map(|((source,group),new)|serde_json::json!({"source":source,"group":group,"current":new.json()})).collect::<Vec<_>>(),
        "rows":rows,
    });
    eprintln!(
        "fresh source append validation {}",
        serde_json::to_string_pretty(&report["groups"]).unwrap()
    );
    if let Some(path) = std::env::var_os("VTCODE_LEARNING_APPEND_VALIDATION_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
}
