use super::*;

#[test]
fn prepared_workers_preserve_grammar_identity_when_names_are_duplicated() {
    let mut highlighter = Highlighter::new();
    let mut builder = highlighter.syntax_set.as_ref().clone().into_builder();
    builder.add(syntect::parsing::SyntaxDefinition::load_from_str(
        "%YAML 1.2\n---\nname: Rust\nfile_extensions: [different-rust]\nscope: source.different-rust\ncontexts:\n  main:\n    - match: magic\n      scope: keyword.control.rust\n",
        true, None,
    ).unwrap());
    highlighter.syntax_set = Arc::new(builder.build());
    highlighter.registry_generation += 1;
    let rope = Rope::from_str("let magic = 123;\n");
    let path = Path::new("ordinary.rs");
    let job = highlighter
        .prepare_job(&rope, path, 0, None, 0..1, None, "dark-plus")
        .unwrap();
    let (syntaxes, themes) = highlighter.shared_sets();
    assert!(!std::ptr::eq(
        &syntaxes.syntaxes()[job.syntax_index],
        syntaxes.find_syntax_by_name("Rust").unwrap()
    ));
    let expected = Highlighter::highlight_line_static_pub(
        "let magic = 123;\n",
        Some(path),
        None,
        "dark-plus",
        &syntaxes,
        &themes,
    );
    let batch = job.run(&rope, &syntaxes, &themes, || false);
    assert_eq!(batch.entries[0].spans, expected);
}

#[test]
fn detection_reuse_preserves_shebang_changes_context_switches_and_reopens() {
    let mut highlighter = Highlighter::new();
    let path = Path::new("extensionless-script");
    let mut rope = Rope::from_str("#!/usr/bin/env python3\nvalue = 123\n");
    let initial = highlighter
        .prepare_job(&rope, path, 0, None, 0..2, None, "dark-plus")
        .unwrap();
    let original_name = initial.syntax_name.clone();
    rope.insert(rope.line_to_char(1), "# comment\n");
    let edited = highlighter
        .prepare_job(&rope, path, 1, Some((1, 2)), 0..3, None, "dark-plus")
        .unwrap();
    assert_eq!(edited.syntax_name, original_name);
    assert_eq!(edited.first_line, initial.first_line);
    let exact = select_syntax(
        &highlighter.syntax_set,
        Some(path),
        None,
        &edited.first_line,
    );
    assert_eq!(edited.syntax_name, exact.name);

    let next = Rope::from_str("#!/bin/bash\necho hello\n");
    let changed = highlighter
        .prepare_job(&next, path, 2, Some((0, 0)), 0..2, None, "dark-plus")
        .unwrap();
    let expected = select_syntax(&highlighter.syntax_set, Some(path), None, "#!/bin/bash\n");
    assert_eq!(changed.syntax_name, expected.name);
    assert_eq!(changed.first_line.as_ref(), "#!/bin/bash\n");
    assert_ne!(changed.syntax_name, original_name);
    let language = highlighter
        .prepare_job(&next, path, 2, None, 0..2, Some("Rust"), "dark-plus")
        .unwrap();
    assert_eq!(language.syntax_name, "Rust");
    let reopened = highlighter
        .prepare_job(&rope, path, 0, Some((0, 2)), 0..2, None, "dark-plus")
        .unwrap();
    assert_eq!(reopened.syntax_name, original_name);
    assert_eq!(reopened.first_line, initial.first_line);
}

#[test]
fn warm_viewports_still_restart_for_registry_theme_language_and_document_changes() {
    let mut highlighter = Highlighter::new();
    let rope = Rope::from_str("let value = 123;\n");
    let path = Path::new("warm.rs");
    let (syntaxes, themes) = highlighter.shared_sets();
    let job = highlighter
        .prepare_job(&rope, path, 0, None, 0..1, None, "dark-plus")
        .unwrap();
    assert!(highlighter.apply_batch(job.run(&rope, &syntaxes, &themes, || false)));
    for _ in 0..3 {
        assert!(
            highlighter
                .prepare_job(&rope, path, 0, None, 0..1, None, "dark-plus")
                .is_none()
        );
    }
    // Model a registry replacement that now associates this path differently.
    // The generation guard must force selection before accepting cached spans.
    highlighter.registry_generation += 1;
    highlighter.documents.get_mut(path).unwrap().syntax_name = "Plain Text".into();
    let refreshed = highlighter
        .prepare_job(&rope, path, 0, None, 0..1, None, "dark-plus")
        .unwrap();
    assert_eq!(refreshed.syntax_name, "Rust");
    assert!(highlighter.apply_batch(refreshed.run(&rope, &syntaxes, &themes, || false)));
    assert!(
        highlighter
            .prepare_job(&rope, path, 0, None, 0..1, None, "base16-ocean.dark")
            .is_some()
    );
    assert!(
        highlighter
            .prepare_job(&rope, path, 0, None, 0..1, Some("Python"), "dark-plus")
            .is_some()
    );
    assert!(
        highlighter
            .prepare_job(&rope, path, 1, Some((0, 0)), 0..1, None, "dark-plus")
            .is_some()
    );
    assert!(
        highlighter
            .prepare_job(&rope, path, 1, Some((0, 0)), 0..1, None, "dark-plus")
            .is_some()
    );
}

fn original_warm_viewport_prepare(
    highlighter: &mut Highlighter,
    rope: &Rope,
    path: &Path,
    visible: Range<usize>,
) {
    highlighter.use_counter = highlighter.use_counter.wrapping_add(1);
    let line = rope.line(0);
    let mut first = line
        .slice(..line.len_chars().min(MAX_SYNTAX_DETECTION_CHARS))
        .to_string();
    Highlighter::normalize_line_for_highlighting(&mut first);
    let name = select_syntax(&highlighter.syntax_set, Some(path), None, &first)
        .name
        .clone();
    let cache = highlighter.documents.get_mut(path).unwrap();
    assert!(cache.context_matches("dark-plus", None, &name));
    cache.last_used = highlighter.use_counter;
    assert!(visible.into_iter().all(|line| {
        cache
            .lines
            .get(&line)
            .is_some_and(|entry| entry.version == 0 && entry.authoritative)
    }));
    std::hint::black_box(first);
}

#[test]
#[ignore = "manual release warmed syntax scheduling benchmark"]
fn benchmark_warm_syntax_job_preparation() {
    let mut highlighter = Highlighter::new();
    let path = Path::new("extensionless-script");
    let source = format!(
        "#!/usr/bin/env python3 {}\n{}",
        "x".repeat(4_096),
        "value = 123\n".repeat(80)
    );
    let rope = Rope::from_str(&source);
    let (syntaxes, themes) = highlighter.shared_sets();
    let job = highlighter
        .prepare_job(&rope, path, 0, None, 1..61, None, "dark-plus")
        .unwrap();
    assert!(highlighter.apply_batch(job.run_with_budget(
        &rope,
        &syntaxes,
        &themes,
        &mut || false,
        Duration::from_secs(10)
    )));
    let mut before = Vec::new();
    let mut after = Vec::new();
    for sample in 0..7 {
        for optimized in [sample % 2 == 0, sample % 2 != 0] {
            let started = Instant::now();
            for _ in 0..1_000 {
                if optimized {
                    assert!(
                        highlighter
                            .prepare_job(&rope, path, 0, None, 1..61, None, "dark-plus")
                            .is_none()
                    );
                } else {
                    original_warm_viewport_prepare(&mut highlighter, &rope, path, 1..61);
                }
            }
            let elapsed = started.elapsed();
            if optimized {
                after.push(elapsed);
            } else {
                before.push(elapsed);
            }
        }
    }
    before.sort();
    after.sort();
    eprintln!(
        "1,000 warm 60-line syntax schedules: original {:?}, reused {:?}",
        before[3], after[3]
    );
}

#[test]
fn job_preparation_shares_suffix_states_and_workers_keep_them_immutable() {
    let mut highlighter = Highlighter::new();
    let path = Path::new("shared-checkpoints.rs");
    let rope = Rope::from_str(&"let value = 123;\n".repeat(2_050));
    let (syntax, themes) = highlighter.shared_sets();
    let initial = highlighter
        .prepare_job(&rope, path, 0, None, 0..rope.len_lines(), None, "dark-plus")
        .unwrap();
    let batch = initial.run_with_budget(
        &rope,
        &syntax,
        &themes,
        &mut || false,
        Duration::from_secs(10),
    );
    assert!(highlighter.apply_batch(batch));

    let original = Arc::clone(&highlighter.documents[path].checkpoints[&256].state);
    let original_state = original.as_ref().clone();
    let mut edited = rope.clone();
    edited.insert(edited.line_to_char(300), "// new line\n");
    let job = highlighter
        .prepare_job(
            &edited,
            path,
            1,
            Some((300, 301)),
            290..540,
            None,
            "dark-plus",
        )
        .unwrap();
    assert_eq!(job.parse_start, 256);
    assert!(Arc::ptr_eq(job.start_state.as_ref().unwrap(), &original));
    assert!(!job.stale_checkpoints.is_empty());
    assert!(job.stale_checkpoints.iter().all(|(line, _)| *line <= job.visible.end),
        "checkpoint snapshots are limited to the reachable worker slice");
    for (line, checkpoint) in &job.stale_checkpoints {
        assert!(Arc::ptr_eq(
            &checkpoint.state,
            &highlighter.documents[path].checkpoints[line].state
        ));
    }
    let cloned = job.clone();
    for (original, copy) in job.stale_checkpoints.iter().zip(&cloned.stale_checkpoints) {
        assert!(Arc::ptr_eq(&original.1.state, &copy.1.state));
    }
    let batch = job.run_with_budget(
        &edited,
        &syntax,
        &themes,
        &mut || false,
        Duration::from_secs(10),
    );
    assert_eq!(batch.converged_at, Some(513));
    assert_eq!(original.as_ref(), &original_state);
    assert!(highlighter.apply_batch(batch));
}

#[test]
#[ignore = "comparative release UI checkpoint snapshot benchmark"]
fn benchmark_shared_syntax_checkpoint_snapshots() {
    let highlighter = Highlighter::new();
    let (syntaxes, themes) = highlighter.shared_sets();
    let syntax = syntaxes.find_syntax_by_extension("rs").unwrap();
    let theme = select_theme(&themes, "dark-plus").unwrap();
    let parser = syntect::highlighting::Highlighter::new(theme);
    let mut state = SyntectState::new(syntax, &parser);
    state
        .highlight_line(&("{".repeat(64) + "\n"), &syntaxes, &parser, false, &mut Vec::new())
        .unwrap();
    let state = Arc::new(state);
    for count in [256, 4_096] {
        let checkpoints: Vec<_> = (0..count)
            .map(|line| {
                (
                    line * CHECKPOINT_INTERVAL,
                    CachedCheckpoint {
                        version: 0,
                        state: Arc::clone(&state),
                        coordinate_origin: (line * CHECKPOINT_INTERVAL) as i128,
                    },
                )
            })
            .collect();
        let mut deep = Vec::new();
        let mut shared = Vec::new();
        for sample in 0..9 {
            for reuse in [sample % 2 == 0, sample % 2 != 0] {
                let started = Instant::now();
                let elapsed = if reuse {
                    let snapshots: Vec<_> = checkpoints
                        .iter()
                        .map(|(line, checkpoint)| (*line, checkpoint.clone()))
                        .collect();
                    let elapsed = started.elapsed();
                    std::hint::black_box(&snapshots);
                    assert_eq!(snapshots.len(), count);
                    assert!(Arc::ptr_eq(&snapshots[count - 1].1.state, &state));
                    elapsed
                } else {
                    // Original suffix snapshot contents, including a deep
                    // mutable SyntectState clone for every retained checkpoint.
                    let snapshots: Vec<_> = checkpoints
                        .iter()
                        .map(|(line, checkpoint)| {
                            (
                                *line,
                                checkpoint.version,
                                checkpoint.line_offset(*line),
                                checkpoint.state.as_ref().clone(),
                            )
                        })
                        .collect();
                    let elapsed = started.elapsed();
                    std::hint::black_box(&snapshots);
                    assert_eq!(snapshots[count - 1].3, *state);
                    elapsed
                };
                if reuse {
                    shared.push(elapsed);
                } else {
                    deep.push(elapsed);
                }
            }
        }
        deep.sort_unstable();
        shared.sort_unstable();
        eprintln!(
            "{count} suffix checkpoints: deep {:?}, shared {:?}",
            deep[4], shared[4]
        );
    }
}
