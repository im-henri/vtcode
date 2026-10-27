//! Chronological editing replays. Hidden answer bytes never enter a Rope or index.
//! Designed relationship probes and source-code replays are reported separately.
use super::*;
use std::{
    collections::BTreeSet,
    path::PathBuf,
    time::{Duration, Instant},
};

struct Probe {
    name: String,
    group: &'static str,
    text: String,
    prefix: String,
    expected: Option<String>,
}

struct Session {
    name: &'static str,
    path: &'static str,
    language: &'static str,
    files: Vec<(String, String)>,
    probes: Vec<Probe>,
}

fn probe(name: &str, text: &str, prefix: &str, expected: &str) -> Probe {
    Probe {
        name: name.into(),
        group: "difficult",
        text: text.into(),
        prefix: prefix.into(),
        expected: Some(expected.into()),
    }
}

fn designed_sessions() -> Vec<Session> {
    let mut relations = Vec::new();
    // Same operation pair with independently renamed operands; the newest
    // first operation is visible before its corresponding second operation.
    for (first, second, a, b, x, y, prefix) in [
        (
            "register",
            "unregister",
            "chanA",
            "handlerA",
            "chanB",
            "handlerB",
            "ha",
        ),
        (
            "attach",
            "detach",
            "nodeOld",
            "listenerOld",
            "nodeNew",
            "listenerNew",
            "li",
        ),
        (
            "subscribe",
            "unsubscribe",
            "topicOne",
            "callbackOne",
            "topicTwo",
            "callbackTwo",
            "ca",
        ),
        (
            "pair",
            "unpair",
            "inputLeft",
            "outputLeft",
            "inputRight",
            "outputRight",
            "ou",
        ),
        (
            "connect",
            "disconnect",
            "socketAlpha",
            "peerAlpha",
            "socketBeta",
            "peerBeta",
            "pe",
        ),
        (
            "mount",
            "unmount",
            "volumeFirst",
            "pathFirst",
            "volumeSecond",
            "pathSecond",
            "pa",
        ),
        (
            "lease",
            "release",
            "objectFirst",
            "ownerFirst",
            "objectSecond",
            "ownerSecond",
            "ow",
        ),
        (
            "push",
            "pop",
            "stackFirst",
            "itemFirst",
            "stackSecond",
            "itemSecond",
            "it",
        ),
    ] {
        relations.push(probe(
            &format!("{first}/{second}"),
            &format!(
                "{first}({a}, {b})\n{second}({a}, {b})\n{first}({x}, {y})\n{second}({x}, {prefix}⟂)"
            ),
            prefix,
            y,
        ));
        relations.push(probe(
            &format!("{first}/{second} reverse-order"),
            &format!(
                "{first}({a}, {b})\n{second}({b}, {a})\n{first}({x}, {y})\n{second}({prefix}⟂, {x})"
            ),
            prefix,
            y,
        ));
    }
    relations.extend([
        probe("assignment resource pairing", "sourceOld = open(pathOld)\nclose(sourceOld, pathOld)\nsourceNew = open(pathNew)\nclose(sourceNew, pa⟂)", "pa", "pathNew"),
        probe("indexed relationship", "routes[keyOld] = handlerOld\nremove(routes, keyOld, handlerOld)\nroutes[keyNew] = handlerNew\nremove(routes, keyNew, ha⟂)", "ha", "handlerNew"),
        probe("arrow relation custom language", "map [streamOld -> sinkOld]\nclear [streamOld -> sinkOld]\nmap [streamNew -> sinkNew]\nclear [streamNew -> si⟂]", "si", "sinkNew"),
        probe("three operands reordered", "schedule(taskOld, timerOld, callbackOld)\ncancel(timerOld, callbackOld, taskOld)\nschedule(taskNew, timerNew, callbackNew)\ncancel(timerNew, ca⟂, taskNew)", "ca", "callbackNew"),
        probe("multiline tuple relation", "begin(\n chanOld,\n payloadOld\n)\nend(chanOld, payloadOld)\nbegin(\n chanNew,\n payloadNew\n)\nend(chanNew, pa⟂)", "pa", "payloadNew"),
        probe("receiver relationship", "sessionOld.acquire(lockOld)\nrelease(sessionOld, lockOld)\nsessionNew.acquire(lockNew)\nrelease(sessionNew, lo⟂)", "lo", "lockNew"),
    ]);
    let mut literal = Vec::new();
    for (left, right, distractor, prefix) in [
        ("frame", "frameWidth", "frameHeight", "fr"),
        ("texture", "texturePixels", "textureFormat", "te"),
        ("session", "sessionToken", "sessionTimeout", "se"),
        ("request", "requestPayload", "requestHeaders", "re"),
    ] {
        literal.push(probe(&format!("literal-{left}"), &format!("emit({left}, {right})\nemit({left}, {right})\nemit(other, {distractor})\nemit({left}, {prefix}⟂)"), prefix, right));
        literal.push(probe(&format!("suffix-{left}"), &format!("assign({left}, {right}, width)\nassign({left}, {distractor}, height)\nassign({left}, {prefix}⟂, width)"), prefix, right));
    }
    let mut contradictions = Vec::new();
    for (a, b, x, y, p) in [
        ("chanA", "handlerA", "chanB", "handlerB", "ha"),
        ("nodeA", "listenerA", "nodeB", "listenerB", "li"),
    ] {
        contradictions.push(probe("inconsistent mapping rejected", &format!("register({a}, {a})\nunregister({a}, {b})\nregister({x}, {y})\nunregister({x}, {p}⟂)"), p, y));
        contradictions.push(probe(
            "visible suffix rejects contradictory occurrence",
            &format!("emit({a}, {b}, oldMode)\nemit({x}, {y}, newMode)\nemit({x}, {p}⟂, newMode)"),
            p,
            y,
        ));
    }
    let mixed = vec![
        probe(
            "unknown cross-extension occurrence",
            "sendLocal\nbridge.sendPa⟂",
            "sendPa",
            "sendPacket",
        ),
        probe(
            "custom language relation",
            "bind <portNew : consumerNew>\nunbind <portNew : co⟂>",
            "co",
            "consumerNew",
        ),
        probe(
            "independent program local evidence",
            "client.sendPrivate(packet)\nclient.sendPrivate(packet)\nclient.sendPr⟂",
            "sendPr",
            "sendPrivate",
        ),
        probe(
            "clone flood versus current occurrence",
            "emit(channelLocal, payloadLocal)\nemit(channelLocal, pa⟂)",
            "pa",
            "payloadLocal",
        ),
    ];
    let mut files = vec![
        (
            "library/bridge.unrecognized".into(),
            "bridge.sendPacket(data)\nbridge.sendPacket(other)".into(),
        ),
        (
            "devices/example.portmap".into(),
            "bind <portOld : consumerOld>\nunbind <portOld : consumerOld>".into(),
        ),
        (
            "independent/main.rs".into(),
            "client.sendPublic(packet)\nclient.sendPublic(packet)".repeat(20),
        ),
    ];
    for i in 0..24 {
        files.push((
            format!("generated/clone{i}.alien"),
            "emit(channelGenerated, payloadGenerated)\nemit(channelGenerated, payloadGenerated)"
                .repeat(32),
        ));
    }
    let negatives = [
        (
            "quoted text",
            "value = \"repeatedCandidate repeatedCandidate\nrep⟂",
            "rep",
        ),
        ("weak typo", "printStatus printStatus\npritnS⟂", "pritnS"),
        ("absent prefix", "localValue otherValue\nzzzz⟂", "zzzz"),
        ("empty vocabulary", "xy⟂", "xy"),
    ]
    .into_iter()
    .map(|(name, text, prefix)| Probe {
        name: name.into(),
        group: "negative",
        text: text.into(),
        prefix: prefix.into(),
        expected: None,
    })
    .collect();
    let distant_known = "readCurrent\n".to_owned() + &"\n".repeat(260);
    let distant_text = distant_known.clone() + "read⟂";
    vec![
        Session {
            name: "renaming",
            path: "current.unknown",
            language: "Unknown",
            files: vec![],
            probes: relations,
        },
        Session {
            name: "literal-and-right-context",
            path: "current.custom",
            language: "Unknown",
            files: vec![],
            probes: literal,
        },
        Session {
            name: "contradictions",
            path: "current.other",
            language: "Unknown",
            files: vec![],
            probes: contradictions,
        },
        Session {
            name: "mixed-workspace",
            path: "current.no_grammar",
            language: "NeverBeforeSeen",
            files,
            probes: mixed,
        },
        Session {
            name: "distant-current-file",
            path: "main.odd",
            language: "Unknown",
            files: vec![
                ("main.odd".into(), distant_known),
                (
                    "foreign/generated.unknown".into(),
                    "readForeign ".repeat(1000),
                ),
            ],
            probes: vec![probe(
                "known earlier active-file identifier outside local window",
                &distant_text,
                "read",
                "readCurrent",
            )],
        },
        Session {
            name: "unwanted",
            path: "current.unknown",
            language: "Unknown",
            files: vec![],
            probes: negatives,
        },
        Session {
            name: "comments",
            path: "current.unknown",
            language: "Unknown",
            files: vec![],
            probes: vec![
                probe("line comment", "// repeatedCandidate repeatedCandidate\n// rep⟂", "rep", "repeatedCandidate"),
                probe("block comment", "/* repeatedCandidate repeatedCandidate\nrep⟂", "rep", "repeatedCandidate"),
            ],
        },
        Session {
            name: "cold-start",
            path: "fresh.unknown",
            language: "Unknown",
            files: vec![],
            probes: vec![Probe {
                name: "unseen identifier is unavailable".into(),
                group: "cold-oov",
                text: "ha⟂".into(),
                prefix: "ha".into(),
                expected: Some("handlerNeverObserved".into()),
            }],
        },
        Session {
            name: "generated-workspace-prior",
            path: "current.unrecognized",
            language: "Unknown",
            files: vec![
                (
                    "reference/example.custom".into(),
                    "commit\nreadAnchor".into(),
                ),
                (
                    "generated/copies.alien".into(),
                    "readGenerated ".repeat(1000),
                ),
            ],
            probes: vec![Probe {
                name: "one exact occurrence versus one thousand unrelated generated copies".into(),
                group: "clone-prior",
                text: "commit\nread⟂".into(),
                prefix: "read".into(),
                expected: Some("readAnchor".into()),
            }],
        },
    ]
}

fn recorded_session(name: &'static str, source: &str) -> Session {
    let mut probes = Vec::new();
    let mut seen = BTreeSet::new();
    let positions: Vec<_> = source
        .char_indices()
        .map(|(byte, _)| byte)
        .chain(std::iter::once(source.len()))
        .collect();
    let mut code = BTreeSet::new();
    let mut analyzer = crate::completion_context::Analyzer::default();
    let mut mark = |event: crate::completion_context::Event<'_>| {
        if let crate::completion_context::Event::Word {
            context, offset, ..
        } = event
        {
            if context.domain == crate::completion_context::Domain::Code {
                code.insert(positions[offset]);
            }
        }
    };
    analyzer.feed(source, &mut mark);
    analyzer.flush(mark);
    let mut start = None;
    let mut eligible = 0usize;
    for (offset, ch) in source
        .char_indices()
        .chain(std::iter::once((source.len(), ' ')))
    {
        if identifier_char(ch) {
            start.get_or_insert(offset);
        } else if let Some(begin) = start.take() {
            let word = &source[begin..offset];
            if code.contains(&begin)
                && word.len() >= 5
                && word.len() <= 80
                && word.is_ascii()
                && seen.contains(word)
            {
                eligible += 1;
                if eligible % 7 == 0 && probes.len() < 64 {
                    let prefix_len = if eligible % 2 == 0 { 2 } else { 3 };
                    let text = format!("{}{}⟂", &source[..begin], &word[..prefix_len]);
                    probes.push(Probe {
                        name: format!("{name}:byte{begin}:{word}"),
                        group: if name.starts_with("heldout") {
                            "heldout"
                        } else if name.starts_with("diagnostic") {
                            "diagnostic"
                        } else {
                            "recorded"
                        },
                        text,
                        prefix: word[..prefix_len].into(),
                        expected: Some(word.into()),
                    });
                }
            }
            seen.insert(word.to_owned());
        }
    }
    Session {
        name,
        path: name,
        language: "Unknown",
        files: vec![],
        probes,
    }
}

fn sessions(include_holdout: bool) -> Vec<Session> {
    let mut sessions = designed_sessions();
    sessions.push(recorded_session(
        "history.rs",
        include_str!("../tests/fixtures/completion-replay/history.rs"),
    ));
    sessions.push(recorded_session(
        "peer.py",
        include_str!("../tests/fixtures/completion-replay/peer.py"),
    ));
    sessions.push(recorded_session(
        "bridge.c",
        include_str!("../tests/fixtures/completion-replay/bridge.c"),
    ));
    for (name, source, language) in [
        (
            "history.rs-native",
            include_str!("../tests/fixtures/completion-replay/history.rs"),
            "Rust",
        ),
        (
            "peer.py-native",
            include_str!("../tests/fixtures/completion-replay/peer.py"),
            "Python",
        ),
        (
            "bridge.c-native",
            include_str!("../tests/fixtures/completion-replay/bridge.c"),
            "C",
        ),
    ] {
        let mut native = recorded_session(name, source);
        native.language = language;
        for probe in &mut native.probes {
            probe.group = "recorded-native";
        }
        sessions.push(native);
    }
    sessions.push(recorded_session(
        "diagnostic_bench.py",
        include_str!("../tests/fixtures/completion-replay/diagnostic_bench.py"),
    ));
    if include_holdout {
        let path = std::env::var_os("VTCODE_LEARNING_HOLDOUT_FIXTURE")
            .expect("final holdout requires an explicit frozen fixture path");
        let source =
            std::fs::read_to_string(path).expect("read final holdout fixture during test setup");
        let holdout = recorded_session("heldout-final", &source);
        assert!(
            !holdout.probes.is_empty(),
            "final holdout must contain eligible repeated code identifiers"
        );
        sessions.push(holdout);
    }
    sessions
}

struct Project {
    _directory: tempfile::TempDir,
    root: PathBuf,
    index: crate::completion_index::CompletionIndex,
    initial_paths: BTreeSet<PathBuf>,
}
impl Project {
    fn new(files: &[(String, String)]) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let index = crate::completion_index::CompletionIndex::new_for_test(
            root.clone(),
            directory.path().join("cache"),
        );
        index.configure(crate::completion_index::IndexSettings {
            paused: false,
            speed: crate::completion_index::IndexSpeed::Fast,
            ..Default::default()
        });
        for (path, text) in files {
            assert!(index.observe_buffer(root.join(path), ropey::Rope::from_str(text), true));
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let s = index.status();
            if s.indexed_files == files.len()
                && matches!(
                    s.phase,
                    crate::completion_index::IndexPhase::Idle
                        | crate::completion_index::IndexPhase::Limited
                )
            {
                break;
            }
            assert!(Instant::now() < deadline, "index did not settle: {s:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
        Self {
            _directory: directory,
            initial_paths: files.iter().map(|(path, _)| root.join(path)).collect(),
            root,
            index,
        }
    }
    fn publish_visible(
        &self,
        session: &Session,
        active: &VtCompleteSnapshot,
        warm: bool,
    ) -> Option<serde_json::Value> {
        use sha2::{Digest, Sha256};
        if !warm {
            return None;
        }
        let path = self.root.join(session.path);
        let expected_files =
            self.initial_paths.len() + usize::from(!self.initial_paths.contains(&path));
        let before = self.index.revision();
        assert!(
            self.index.observe_buffer(path, active.rope.clone(), true),
            "visible snapshot must be accepted by the existing scheduler"
        );
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let status = self.index.status();
            assert!(status.error.is_none(), "warm index error: {status:?}");
            assert_ne!(
                status.phase,
                crate::completion_index::IndexPhase::Limited,
                "warm indexing must not silently fall back after a limit"
            );
            let after = self.index.revision();
            if after > before
                && status.phase == crate::completion_index::IndexPhase::Idle
                && status.pending_files == 0
                && status.indexed_files == expected_files
            {
                assert!(
                    !status.settings.project_wide,
                    "opened-files-only validation"
                );
                let mut digest = Sha256::new();
                for chunk in active.rope.chunks() {
                    digest.update(chunk.as_bytes());
                }
                return Some(
                    serde_json::json!({"before":before,"after":after,"indexedFiles":status.indexed_files,"expectedIndexedFiles":expected_files,"pendingFiles":status.pending_files,"visibleBytes":active.rope.len_bytes(),"visibleSha256":format!("{:X}",digest.finalize())}),
                );
            }
            assert!(
                Instant::now() < deadline,
                "exact visible snapshot did not finish indexing: {status:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn snapshot(&self, session: &Session, probe: &Probe, document_id: u64) -> VtCompleteSnapshot {
        assert_eq!(
            probe.text.matches('⟂').count(),
            1,
            "exactly one caret marker; do not expose a hidden future suffix through a second marker"
        );
        let (left, right) = probe.text.split_once('⟂').expect("caret marker");
        let rope = ropey::Rope::from_str(&(left.to_owned() + right));
        let cursor_line = left.bytes().filter(|&b| b == b'\n').count();
        let cursor_col = left.rsplit('\n').next().unwrap().chars().count();
        VtCompleteSnapshot {
            source: crate::completion_source::Source::new(
                Some(&self.root.join(session.path)),
                session.language,
            ),
            rope,
            document_id,
            cursor_line,
            cursor_col,
            active: true,
        }
    }
}

fn bare(word: &str) -> &str {
    word.strip_suffix("()").unwrap_or(word)
}

thread_local! {
    static CANDIDATE_TRACE: std::cell::RefCell<(Option<String>, bool)> = const { std::cell::RefCell::new((None, false)) };
}
pub(super) fn trace_candidates<'a>(words: impl Iterator<Item = &'a str>) {
    CANDIDATE_TRACE.with(|trace| {
        let mut trace = trace.borrow_mut();
        if let Some(expected) = &trace.0 {
            trace.1 = words.into_iter().any(|word| word == expected);
        }
    });
}
pub(super) fn begin_trace(expected: Option<&str>) {
    CANDIDATE_TRACE.with(|trace| *trace.borrow_mut() = (expected.map(str::to_owned), false));
}
pub(super) fn candidate_recalled() -> bool {
    CANDIDATE_TRACE.with(|trace| trace.borrow().1)
}

fn recorded_hard(probe: &Probe) -> bool {
    if !matches!(
        probe.group,
        "recorded" | "recorded-native" | "heldout" | "diagnostic"
    ) {
        return false;
    }
    let (left, _) = probe.text.split_once('⟂').unwrap();
    let before = &left[..left.len() - probe.prefix.len()];
    before
        .split(|ch: char| !identifier_char(ch))
        .filter(|word| word.starts_with(&probe.prefix) && word.len() > probe.prefix.len())
        .collect::<BTreeSet<_>>()
        .len()
        >= 3
}

#[derive(Default)]
struct Metrics {
    total: usize,
    top1: usize,
    admitted: usize,
    recall10: usize,
    reciprocal: f64,
    negative: usize,
    unwanted: usize,
}
impl Metrics {
    fn add(&mut self, probe: &Probe, suggestions: &[String], admitted: bool) {
        if let Some(expected) = &probe.expected {
            self.total += 1;
            self.admitted += usize::from(admitted);
            if let Some(rank) = suggestions.iter().position(|w| bare(w) == expected) {
                self.recall10 += 1;
                self.top1 += usize::from(rank == 0);
                self.reciprocal += 1.0 / (rank + 1) as f64;
            }
        } else {
            self.negative += 1;
            self.unwanted += usize::from(!suggestions.is_empty());
        }
    }
    fn json(&self) -> serde_json::Value {
        serde_json::json!({"queries":self.total,"top1":self.top1,"topChoiceErrors":self.total-self.top1,"candidateRecall":self.admitted,"recallAt10":self.recall10,"mrr":if self.total>0 { self.reciprocal/self.total as f64 } else {0.0},"negativeProbes":self.negative,"unwantedPopups":self.unwanted})
    }
}

/// Partial worker accounting, reported separately from index.status().memory_bytes.
/// Includes counted token/cache/source bytes, excludes allocator overhead/prediction
/// internals; never presented as process RSS or a proof of the complete budget.
fn token_cache_bytes(cache: &TokenCache) -> usize {
    cache
        .documents
        .iter()
        .map(|d| {
            d.rope.len_bytes()
                + d.word_bytes
                + d.lines.capacity() * std::mem::size_of::<LineIndex>()
                + d.postings * 4
                + d.context_bytes
                + d.word_ids.capacity() * 32
        })
        .sum()
}

#[test]
fn learning_replay_quality() {
    let warm = source_rename_tests::warm_index_enabled();
    let mode = source_rename_tests::requested_mode();
    let mut new = std::collections::BTreeMap::<&str, Metrics>::new();
    let mut rows = Vec::new();
    let mut index_bytes = 0;
    let mut example_bytes = 0;
    let mut vocabulary_index_bytes = 0;
    let mut worker_bytes = 0;
    let include_holdout = std::env::var_os("VTCODE_LEARNING_HOLDOUT").is_some();
    for (i, session) in sessions(include_holdout)
        .iter()
        .enumerate()
        .filter(|(_, s)| include_holdout || !s.name.starts_with("heldout"))
    {
        let project = Project::new(&session.files);
        let mut new_cache = TokenCache::default();
        for probe in &session.probes {
            let active = project.snapshot(session, probe, (i + 1) as u64);
            let publication = project.publish_visible(session, &active, warm);
            begin_trace(probe.expected.as_deref());
            let after = crate::completion_index::examples::with_mode(mode, || {
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
            let after_recall = candidate_recalled();
            new.entry(probe.group)
                .or_default()
                .add(probe, &after.suggestions, after_recall);
            if recorded_hard(probe) {
                let hard_group = match probe.group {
                    "heldout" => "heldout-hard",
                    "diagnostic" => "diagnostic-hard",
                    "recorded-native" => "recorded-native-hard",
                    _ => "recorded-hard",
                };
                new.entry(hard_group)
                    .or_default()
                    .add(probe, &after.suggestions, after_recall);
            }
            rows.push(serde_json::json!({"session":session.name,"probe":probe.name,"group":probe.group,"warmIndex":warm,"indexPublication":publication,"prefix":probe.prefix,"hard":recorded_hard(probe),"expected":probe.expected,"current":after.suggestions,"currentCandidateRecalled":after_recall}));
        }
        index_bytes = index_bytes.max(project.index.status().memory_bytes);
        let examples = project.index.example_memory();
        example_bytes = example_bytes.max(examples);
        vocabulary_index_bytes = vocabulary_index_bytes
            .max(project.index.status().memory_bytes.saturating_sub(examples));
        worker_bytes = worker_bytes.max(token_cache_bytes(&new_cache));
    }
    let groups: Vec<_> = new
        .iter()
        .map(|(group, current)| serde_json::json!({"group":group,"current":current.json()}))
        .collect();
    let report = serde_json::json!({"includeHoldout":include_holdout,"warmIndex":warm,"mode":format!("{mode:?}"),"groups":groups,"maxIndexEstimatedBytes":index_bytes,"maxExampleEstimatedBytes":example_bytes,"maxVocabularyIndexEstimatedBytes":vocabulary_index_bytes,"maxPartialWorkerBytes":worker_bytes,"rows":rows});
    eprintln!(
        "learning replay {}",
        serde_json::to_string_pretty(&report["groups"]).unwrap()
    );
    if let Some(path) = std::env::var_os("VTCODE_LEARNING_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
    assert!(new["difficult"].total >= 30);
    assert!(new["recorded"].total >= 50);
    assert!(
        new["negative"].unwanted == 0,
        "unwanted automatic popups regressed"
    );
}

#[test]
#[ignore = "development-only component ablation; holdout intentionally excluded"]
fn learning_replay_component_ablation() {
    use crate::completion_index::examples::{Mode, with_mode};
    let warm = source_rename_tests::warm_index_enabled();
    let modes = [
        ("full", Mode::Full),
        ("literal-only", Mode::LiteralOnly),
        ("no-right-context", Mode::NoRightContext),
        ("no-deduplication", Mode::NoDeduplicate),
        ("disabled", Mode::Disabled),
        ("no-statistical", Mode::NoStatistical),
        ("raw-workspace-prior", Mode::RawWorkspacePrior),
        ("no-extension", Mode::NoExtension),
        ("no-literal-backoff", Mode::NoLiteralBackoff),
        ("fixed-channel-order", Mode::FixedChannelOrder),
        ("mixture-scale192", Mode::MixtureScale192),
        ("phase1", Mode::Phase1),
        ("uniform-concentration", Mode::UniformConcentration),
        ("unconditional-file-prior", Mode::UnconditionalFilePrior),
        ("no-active-file-occurrences", Mode::NoActiveFileOccurrences),
    ];
    let mut totals = std::collections::BTreeMap::<(&str, &str), Metrics>::new();
    let mut rows = Vec::new();
    for (i, session) in sessions(false)
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.name.starts_with("heldout"))
    {
        let project = Project::new(&session.files);
        for (name, mode) in modes {
            let mut cache = TokenCache::default();
            for probe in &session.probes {
                let active = project.snapshot(session, probe, (i + 1) as u64);
                let publication = project.publish_visible(session, &active, warm);
                begin_trace(probe.expected.as_deref());
                let result = with_mode(mode, || {
                    compute_contextual_invoked(
                        &active,
                        &probe.prefix,
                        session.language,
                        &project.index,
                        &mut cache,
                        false,
                        || false,
                    )
                })
                .unwrap();
                totals.entry((name, probe.group)).or_default().add(
                    probe,
                    &result.suggestions,
                    candidate_recalled(),
                );
                if recorded_hard(probe) {
                    totals
                        .entry((
                            name,
                            match probe.group {
                                "diagnostic" => "diagnostic-hard",
                                "recorded-native" => "recorded-native-hard",
                                _ => "recorded-hard",
                            },
                        ))
                        .or_default()
                        .add(probe, &result.suggestions, candidate_recalled());
                }
                rows.push(serde_json::json!({"mode":name,"session":session.name,"probe":probe.name,"group":probe.group,"warmIndex":warm,"indexPublication":publication,"prefix":probe.prefix,"hard":recorded_hard(probe),"expected":probe.expected,"suggestions":result.suggestions}));
            }
        }
    }
    let groups: Vec<_> = totals
        .iter()
        .map(|((mode, group), m)| serde_json::json!({"mode":mode,"group":group,"metrics":m.json()}))
        .collect();
    let report =
        serde_json::json!({"groups":groups,"rows":rows,"holdoutIncluded":false,"warmIndex":warm});
    eprintln!(
        "learning ablation {}",
        serde_json::to_string_pretty(&report["groups"]).unwrap()
    );
    if let Some(path) = std::env::var_os("VTCODE_LEARNING_ABLATION_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
}

fn percentiles(mut ns: Vec<u128>) -> serde_json::Value {
    ns.sort_unstable();
    serde_json::json!({"samples":ns.len(),"p50Us":ns[ns.len()/2] as f64/1000.0,"p95Us":ns[(ns.len()*95/100).min(ns.len()-1)] as f64/1000.0,"maxUs":ns[ns.len()-1] as f64/1000.0})
}

#[test]
#[ignore = "optimized worker latency distribution; run --release --ignored --nocapture"]
fn benchmark_learning_replay_worker_latency() {
    use std::hint::black_box;
    let mut session = designed_sessions().remove(3);
    let mut text = String::new();
    for i in 0..9997 {
        text.push_str(&format!(
            "client.getUserConfig(v{}); logger.printLine(v{}); buffer.pushMessage(v{});\n",
            i % 64,
            i % 64,
            i % 64
        ));
    }
    session.probes = vec![probe("10k lines", "", "ha", "handlerB")];
    session.probes[0].text = text
        + "register(chanA, handlerA)\nunregister(chanA, handlerA)\nregister(chanB, handlerB)\nunregister(chanB, ha⟂)";
    let project = Project::new(&session.files);
    let active = project.snapshot(&session, &session.probes[0], 700);
    let prefix = &session.probes[0].prefix;
    let mut report = Vec::new();
    {
        begin_trace(None);
        let mut cache = TokenCache::default();
        let call = |cache: &mut TokenCache| {
            compute_contextual_invoked(
                black_box(&active),
                black_box(prefix),
                "Unknown",
                &project.index,
                cache,
                false,
                || false,
            )
        };
        let start = Instant::now();
        black_box(call(&mut cache));
        let cold = start.elapsed();
        for _ in 0..100 {
            black_box(call(&mut cache));
        }
        let mut samples = Vec::with_capacity(2000);
        for _ in 0..2000 {
            let start = Instant::now();
            black_box(call(&mut cache));
            samples.push(start.elapsed().as_nanos());
        }
        report.push(serde_json::json!({"engine":"current","coldUs":cold.as_nanos() as f64/1000.0,"warm":percentiles(samples),"partialWorkerBytes":token_cache_bytes(&cache)}));
    }
    // Actual interactive pool + VTCompleteWorker queue-to-result latency.
    // The hidden answer is never supplied: refine/backspace only the prefix.
    let background = crate::background::BackgroundWorkers::new();
    let worker = VtCompleteWorker::default();
    let mut edited = active.clone();
    let mut previous_prefix = prefix.chars().count();
    let mut queued = Vec::with_capacity(300);
    let mut queue_cold = 0;
    let mut limited = 0;
    for generation in 0..321u64 {
        let typed = ["h", "ha", "han", "hand", "handl", "handle"][generation as usize % 6];
        let caret = edited.rope.line_to_char(edited.cursor_line) + edited.cursor_col;
        edited.rope.remove(caret - previous_prefix..caret);
        edited.rope.insert(caret - previous_prefix, typed);
        edited.cursor_col = edited.cursor_col - previous_prefix + typed.len();
        previous_prefix = typed.len();
        let request = VtCompleteRequest {
            active: edited.clone(),
            edit_baseline: None,
            index: project.index.clone(),
            explicit: false,
            language: "Unknown".into(),
            generation,
            active_group: 0,
            active_tab: 0,
            content_version: generation,
            cancel: CancellationToken::new(),
            sender: background.sender.clone(),
        };
        let start = Instant::now();
        worker.submit(request, background.interactive_sender());
        let result = background
            .receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("worker completion");
        let elapsed = start.elapsed().as_nanos();
        if generation == 0 {
            queue_cold = elapsed;
        }
        if generation > 20 {
            queued.push(elapsed);
        }
        if let BackgroundResult::VtComplete {
            generation: received,
            work_outcome,
            ..
        } = result
        {
            assert_eq!(generation, received);
            limited += usize::from(work_outcome == VtCompleteWorkOutcome::FuzzyWorkLimited);
        } else {
            panic!("unexpected background result");
        }
    }
    let value = serde_json::json!({"lines":active.rope.len_lines(),"activeBytes":active.rope.len_bytes(),"indexedFiles":project.index.status().indexed_files,"indexEstimatedBytes":project.index.status().memory_bytes,"exampleEstimatedBytes":project.index.example_memory(),"vocabularyIndexEstimatedBytes":project.index.status().memory_bytes.saturating_sub(project.index.example_memory()),"engines":report,"workerQueue":{"coldUs":queue_cold as f64/1000.0,"warm":percentiles(queued),"workLimitedResults":limited,"description":"sequential prefix refinement/backspace; real interactive worker pool; submission through result channel; excludes editor debounce/rendering"}});
    eprintln!(
        "learning worker latency {}",
        serde_json::to_string_pretty(&value).unwrap()
    );
    if let Some(path) = std::env::var_os("VTCODE_LEARNING_LATENCY_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    }
}

#[path = "vtcomplete_learning_rename_replay_tests.rs"]
mod source_rename_tests;
