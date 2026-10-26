use std::hint::black_box;
use std::time::{Duration, Instant};

use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};

use super::analyze_problem_snapshots;
use crate::app::{App, PaletteCommand, fuzzy_match, fuzzy_match_score};

#[test]
fn workbench_symbol_membership_matches_skim_smart_case_and_unicode() {
    for name in [
        "",
        "Measurements",
        "get_measurements",
        "README.md",
        "résumé_变量😀",
        "École",
        "école",
        "İstanbul",
        "ASCII_École",
        "a\u{301}b",
        "a\0b",
    ] {
        for query in [
            "",
            "m",
            "Me",
            "me",
            "RM",
            "rm",
            "umé",
            "É",
            "é",
            "İs",
            "i",
            "AÉ",
            "变量",
            "😀",
            "ab",
            "a\u{301}",
            "\0",
            "not_present",
        ] {
            let expected = SkimMatcherV2::default().fuzzy_match(name, query).is_some();
            assert_eq!(fuzzy_match(name, query), expected, "{name:?}, {query:?}");
        }
    }

    let alphabet = ['a', 'b', 'A', 'B', '_', '-', '/', ' ', 'é', 'É', '变', '😀'];
    let mut state = 0xd115_cafe_u64;
    let mut random = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state as usize
    };
    for _ in 0..2_000 {
        let name: String = (0..random() % 80)
            .map(|_| alphabet[random() % alphabet.len()])
            .collect();
        let query: String = if random() % 2 == 0 {
            name.chars().filter(|_| random() % 4 == 0).collect()
        } else {
            (0..random() % 12)
                .map(|_| alphabet[random() % alphabet.len()])
                .collect()
        };
        assert_eq!(
            fuzzy_match(&name, &query),
            SkimMatcherV2::default()
                .fuzzy_match(&name, &query)
                .is_some(),
            "{name:?}, {query:?}"
        );
    }
}

fn reference_palette(commands: &[PaletteCommand], query: &str) -> Vec<usize> {
    let query = query.to_ascii_lowercase();
    let mut ranked: Vec<_> = commands
        .iter()
        .enumerate()
        .filter_map(|(index, command)| {
            SkimMatcherV2::default()
                .fuzzy_match(&command.name, &query)
                .map(|score| (index, score))
        })
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1));
    ranked.into_iter().map(|(index, _)| index).collect()
}

#[test]
fn workbench_palette_case_variants_keep_the_same_results_and_rank() {
    let mut app = App::test_new();
    app.open_palette();
    for queries in [
        ["theme", "THEME", "ThEmE"],
        ["file", "FILE", "FiLe"],
        ["git", "GIT", "gIt"],
        ["lsp", "LSP", "LsP"],
        ["tln", "TLN", "TlN"],
    ] {
        app.palette.query = queries[0].into();
        app.filter_palette_commands();
        let expected = app.palette.filtered.clone();
        assert!(!expected.is_empty(), "query {:?}", queries[0]);
        for query in &queries[1..] {
            app.palette.query = (*query).into();
            app.filter_palette_commands();
            assert_eq!(app.palette.filtered, expected, "query {query:?}");
        }
    }
}

#[test]
fn workbench_palette_reuses_scores_without_changing_rank_or_ties() {
    let mut app = App::test_new();
    for name in [
        "Éditer le thème",
        "école_变量",
        "Open File",
        "Open File",
        "Close File",
        "🍎 apple",
        "a_a_a_a_apple",
        "Abc",
        "aBC",
        "a_b_c",
    ] {
        app.palette.commands.push(PaletteCommand {
            name: name.into(),
            shortcut: String::new(),
            id: "test-command".into(),
        });
    }
    // Vary query size/order to exercise reused Unicode and scoring buffers.
    for query in [
        "file",
        "a",
        "open",
        "theme",
        "É",
        "é",
        "变量",
        "Ab",
        "abc",
        "🍎",
        "a_a_a",
        "not a real command",
        "file",
    ] {
        let expected = reference_palette(&app.palette.commands, query);
        app.palette.query = query.into();
        app.filter_palette_commands();
        assert_eq!(app.palette.filtered, expected, "query {query:?}");
    }
}

#[test]
fn workbench_problem_scan_handles_contiguous_and_chunk_split_lines() {
    let padding = "é😀".repeat(1_000);
    let files = vec![
        (
            "alpha.rs".into(),
            ropey::Rope::from_str(
                "// TODO: finish\r\n//FIXME: broken\n// HACK: workaround\r\nBUG42\n// NOTE: later\n",
            ),
        ),
        (
            "beta.rs".into(),
            ropey::Rope::from_str(&format!("{padding} BUG: failure\nunchanged")),
        ),
    ];
    assert!(files[0].1.line(0).as_str().is_some());
    assert!(files[1].1.line(0).as_str().is_none());
    let files = files.into_iter().enumerate().map(|(id, (name, rope))| (name, rope, id as u64)).collect();
    let (items, errors, warnings, info, file_count) = analyze_problem_snapshots(files);
    assert_eq!((errors, warnings, info, file_count), (2, 2, 1, 2));
    assert_eq!(items.iter().map(|item| item.4).collect::<Vec<_>>(), [0, 1, 0, 0, 0]);
    assert_eq!(
        items.into_iter().map(|(name, line, marker, text, _)| (name, line, marker, text)).collect::<Vec<_>>(),
        vec![
            (
                "alpha.rs".into(),
                2,
                "FIXME".into(),
                "//FIXME: broken".into()
            ),
            (
                "beta.rs".into(),
                1,
                "BUG".into(),
                format!("{padding} BUG: failure")
            ),
            (
                "alpha.rs".into(),
                1,
                "TODO".into(),
                "// TODO: finish".into()
            ),
            (
                "alpha.rs".into(),
                3,
                "HACK".into(),
                "// HACK: workaround".into()
            ),
            ("alpha.rs".into(), 5, "NOTE".into(), "// NOTE: later".into()),
        ]
    );
}

#[test]
fn workbench_problem_scan_keeps_marker_boundaries_priority_and_result_limit() {
    let files = vec![(
        "markers.rs".into(),
        ropey::Rope::from_str("TODO42\nNOTED\nBUGFIXME\n// TODO: before FIXME\nNOTEé\n// HACK\n"),
    )];
    let files = files.into_iter().enumerate().map(|(id, (name, rope))| (name, rope, id as u64)).collect();
    let (items, errors, warnings, info, file_count) = analyze_problem_snapshots(files);
    assert_eq!((errors, warnings, info, file_count), (0, 2, 1, 1));
    assert_eq!(
        items
            .iter()
            .map(|item| (item.1, item.2.as_str()))
            .collect::<Vec<_>>(),
        vec![(4, "TODO"), (6, "HACK"), (5, "NOTE")]
    );

    let files = vec![
        (
            "same.rs".into(),
            ropey::Rope::from_str(&"TODO\n".repeat(800)),
        ),
        (
            "same.rs".into(),
            ropey::Rope::from_str(&"NOTE\n".repeat(800)),
        ),
        ("ignored.rs".into(), ropey::Rope::from_str("BUG\n")),
    ];
    let files = files.into_iter().enumerate().map(|(id, (name, rope))| (name, rope, id as u64)).collect();
    let (items, errors, warnings, info, file_count) = analyze_problem_snapshots(files);
    assert_eq!(items.len(), 1_000);
    assert_eq!((errors, warnings, info, file_count), (0, 800, 200, 2));
}

fn median(samples: &mut [Duration]) -> Duration {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

#[test]
#[ignore = "manual release benchmark"]
fn benchmark_workbench_symbol_filter() {
    let names: Vec<_> = (0..10_000)
        .map(|index| match index % 4 {
            0 => format!("measurements::render_output_{index}"),
            1 => format!("MeasurementService::get_result_{index}"),
            2 => format!("résumé_变量::measurement_{index}"),
            _ => format!("input::read_file_{index}"),
        })
        .collect();
    for query in ["mea", "msgr", "résumé", "not_present"] {
        let expected: Vec<_> = names
            .iter()
            .enumerate()
            .filter(|(_, name)| fuzzy_match_score(name, query).is_some())
            .map(|(i, _)| i)
            .collect();
        let actual: Vec<_> = names
            .iter()
            .enumerate()
            .filter(|(_, name)| fuzzy_match(name, query))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(actual, expected);
        let mut before = Vec::new();
        let mut after = Vec::new();
        for sample in 0..7 {
            for optimized in [sample % 2 == 0, sample % 2 != 0] {
                let start = Instant::now();
                let matches: Vec<_> = names
                    .iter()
                    .enumerate()
                    .filter(|(_, name)| {
                        if optimized {
                            fuzzy_match(black_box(name), black_box(query))
                        } else {
                            fuzzy_match_score(black_box(name), black_box(query)).is_some()
                        }
                    })
                    .map(|(index, _)| index)
                    .collect();
                black_box(matches);
                if optimized {
                    after.push(start.elapsed());
                } else {
                    before.push(start.elapsed());
                }
            }
        }
        eprintln!(
            "10,000-symbol filter {query:?}: {:?} -> {:?}",
            median(&mut before),
            median(&mut after)
        );
    }
}

#[test]
#[ignore = "manual release benchmark"]
fn benchmark_workbench_palette_filter() {
    let mut app = App::test_new();
    for index in 0..1_000 {
        app.palette.commands.push(PaletteCommand {
            name: format!("Extension {index}: Open File in Project Editor"),
            shortcut: String::new(),
            id: "extension-command".into(),
        });
    }
    for query in ["open", "file", "epe", "not_present"] {
        app.palette.query = query.into();
        let expected = reference_palette(&app.palette.commands, query);
        app.filter_palette_commands();
        assert_eq!(app.palette.filtered, expected);
        let mut before = Vec::new();
        let mut after = Vec::new();
        for sample in 0..7 {
            for optimized in [sample % 2 == 0, sample % 2 != 0] {
                let start = Instant::now();
                for _ in 0..10 {
                    if optimized {
                        app.filter_palette_commands();
                    } else {
                        let mut scored: Vec<_> = app
                            .palette
                            .commands
                            .iter()
                            .enumerate()
                            .filter_map(|(i, command)| {
                                fuzzy_match_score(&command.name, query).map(|score| (i, score))
                            })
                            .collect();
                        scored.sort_by(|a, b| b.1.cmp(&a.1));
                        app.palette.filtered = scored.into_iter().map(|(i, _)| i).collect();
                    }
                    black_box(&app.palette.filtered);
                }
                if optimized {
                    after.push(start.elapsed());
                } else {
                    before.push(start.elapsed());
                }
            }
        }
        eprintln!(
            "{}-command palette {query:?}: {:?} -> {:?}",
            app.palette.commands.len(),
            median(&mut before) / 10,
            median(&mut after) / 10
        );
    }
}
