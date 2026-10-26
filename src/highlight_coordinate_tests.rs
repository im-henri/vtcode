use super::*;
use std::collections::BTreeMap;

// Keep the original map-rebuilding path as an independent correctness and
// timing reference. Production never flattens the sparse coordinate index.
struct Reference {
    lines: FxHashMap<usize, CachedLine>,
    checkpoints: BTreeMap<usize, CachedCheckpoint>,
    version: u64,
    line_count: usize,
    dirty_from: Option<usize>,
    dirty_through: Option<usize>,
}

impl Reference {
    fn from(cache: &DocumentCache) -> Self {
        let mut lines = FxHashMap::default();
        cache.lines.for_each(.., |line, value| {
            lines.insert(line, value.clone());
        });
        let mut checkpoints = BTreeMap::new();
        cache.checkpoints.for_each(.., |line, value| {
            checkpoints.insert(line, value.clone());
        });
        Self {
            lines,
            checkpoints,
            version: cache.version,
            line_count: cache.line_count,
            dirty_from: cache.dirty_from,
            dirty_through: cache.dirty_through,
        }
    }

    fn advance(&mut self, version: u64, rope: &Rope, from: usize, through: usize) {
        let count = rope.len_lines();
        let from = from.min(count);
        let through = through.max(from).min(count);
        let dirty = self.dirty_from.map_or(from, |old| old.min(from));
        let delta = count as i128 - self.line_count as i128;
        let carried = self
            .dirty_through
            .map(|old| old.saturating_add(delta.max(0) as usize).min(count));
        self.version = version;
        self.line_count = count;
        self.dirty_from = Some(dirty);
        self.dirty_through = Some(carried.map_or(through, |old| old.max(through)));
        if delta == 0 {
            return;
        }
        let previous = std::mem::take(&mut self.lines);
        let mut candidate = previous.get(&from).cloned();
        if delta < 0 && from < count {
            let continuation = from.saturating_add((-delta) as usize);
            if let Some(next) = previous.get(&continuation) {
                let line = rope.line(from);
                let text = line
                    .slice(..line.len_chars().min(MAX_SYNTAX_LINE_CHARS))
                    .to_string();
                let hash = Highlighter::line_hash(&normalized_line_prefix(&text));
                let blank = candidate.as_ref().is_none_or(|cached| {
                    cached
                        .spans
                        .iter()
                        .all(|(_, text)| text.chars().all(char::is_whitespace))
                });
                if next.hash == hash || blank {
                    candidate = Some(next.clone());
                }
            }
        }
        for (line, mut entry) in previous {
            if line < from {
                if line < dirty {
                    entry.version = version;
                    entry.display_version = version;
                }
                self.lines.insert(line, entry);
            } else if line > from {
                if let Some(line) = shift_line(line, delta, count).filter(|&line| line > from) {
                    self.lines.insert(line, entry);
                }
            }
        }
        if let Some(candidate) = candidate {
            if delta > 0 {
                if let Some(line) = shift_line(from, delta, count) {
                    self.lines.insert(line, candidate.clone());
                }
            }
            self.lines.insert(from, candidate);
        }
        for (line, mut checkpoint) in std::mem::take(&mut self.checkpoints) {
            if line <= dirty {
                checkpoint.version = version;
                self.checkpoints.insert(line, checkpoint);
            } else if line <= from {
                self.checkpoints.insert(line, checkpoint);
            } else if let Some(line) = shift_line(line, delta, count) {
                if line > dirty {
                    self.checkpoints.insert(line, checkpoint);
                }
            }
        }
    }
}

fn populated(lines: usize, checkpoints: usize) -> (Highlighter, Rope) {
    let mut highlighter = Highlighter::new();
    let rope = Rope::from_str(
        &"let value = 42; // 界\n".repeat((checkpoints * CHECKPOINT_INTERVAL).max(lines)),
    );
    let path = Path::new("coordinates.rs");
    let job = highlighter
        .prepare_job(&rope, path, 0, None, 0..1, None, "dark-plus")
        .unwrap();
    let (syntaxes, themes) = highlighter.shared_sets();
    assert!(highlighter.apply_batch(job.run(&rope, &syntaxes, &themes, || false)));
    let cache = highlighter.documents.get_mut(path).unwrap();
    let line = cache.lines[&0].clone();
    let checkpoint = cache.checkpoints[&0].clone();
    for at in 0..lines {
        cache.insert_line(at, line.clone());
    }
    for at in 0..checkpoints {
        let line = at * CHECKPOINT_INTERVAL;
        cache.checkpoints.insert(
            line,
            CachedCheckpoint {
                coordinate_origin: line as i128,
                ..checkpoint.clone()
            },
        );
    }
    cache.dirty_from = None;
    cache.dirty_through = None;
    (highlighter, rope)
}

#[test]
fn lazy_coordinates_match_original_display_candidates_during_pending_edits() {
    let (mut highlighter, mut rope) = populated(2_000, 16);
    let path = Path::new("coordinates.rs");
    let cache = highlighter.documents.get_mut(path).unwrap();
    let mut reference = Reference::from(cache);
    let mut seed = 891u64;
    for version in 1..=300 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let from = (seed >> 32) as usize % 1_800;
        let through;
        if version % 3 == 0 {
            rope.remove(rope.line_to_char(from)..rope.line_to_char(from + 2));
            through = from;
        } else {
            rope.insert(
                rope.line_to_char(from),
                if version % 2 == 0 { "界\r\n" } else { "\n" },
            );
            through = from + 1;
        }
        reference.advance(version, &rope, from, through);
        cache.advance_version(version, &rope, from, through);
        assert_eq!(cache.line_count, reference.line_count);
        assert_eq!(cache.dirty_from, reference.dirty_from);
        assert_eq!(cache.dirty_through, reference.dirty_through);
        assert_eq!(cache.lines.len(), reference.lines.len());
        cache.lines.for_each(.., |line, entry| {
            let expected = &reference.lines[&line];
            assert_eq!(entry.hash, expected.hash, "version {version}, row {line}");
            assert_eq!(entry.authoritative, expected.authoritative);
            assert!(Rc::ptr_eq(&entry.spans, &expected.spans));
            assert!(Rc::ptr_eq(&entry.comment_ranges, &expected.comment_ranges));
            assert_eq!(
                cache.line_is_current(line, entry.version),
                line < reference.dirty_from.unwrap() || expected.version == version
            );
            assert_eq!(
                cache.line_is_current(line, entry.display_version),
                line < reference.dirty_from.unwrap() || expected.display_version == version
            );
        });
        // Only checkpoints beyond the direct edit can converge. The old path
        // also retained checkpoints from deleted rows, which are now pruned.
        let boundary = cache.dirty_through.unwrap();
        for (&line, checkpoint) in reference.checkpoints.range(boundary.saturating_add(1)..) {
            let entry = cache.checkpoints.get(&line).unwrap();
            assert!(Arc::ptr_eq(&entry.state, &checkpoint.state));
            assert_eq!(entry.line_offset(line), checkpoint.line_offset(line));
        }
    }
}

fn finish(highlighter: &mut Highlighter, rope: &Rope, version: u64) {
    let (syntaxes, themes) = highlighter.shared_sets();
    for _ in 0..30 {
        let Some(job) = highlighter.prepare_job(
            rope,
            Path::new("history.rs"),
            version,
            None,
            0..rope.len_lines(),
            None,
            "dark-plus",
        ) else {
            return;
        };
        assert!(highlighter.apply_batch(job.run_with_budget(
            rope,
            &syntaxes,
            &themes,
            &mut || false,
            Duration::from_secs(5)
        )));
    }
    panic!("parser did not finish");
}

#[test]
fn delayed_jobs_newlines_multiline_scopes_and_history_match_a_fresh_parser() {
    let path = Path::new("history.rs");
    let mut highlighter = Highlighter::new();
    let original = Rope::from_str(&"let value = \"界👩‍💻\"; // comment\r\n".repeat(900));
    let mut rope = original.clone();
    finish(&mut highlighter, &rope, 0);
    let mut history = vec![original];
    let mut obsolete = Vec::new();
    for (step, (from, remove, text)) in [
        (300, 0, "/*\r\n"),
        (700, 0, "*/\n"),
        (100, 2, "\r\n"),
        (510, 3, "\n// changed\n"),
        (0, 1, "// new first\n"),
        (899, 0, "\n"),
        (256, 1, ""),
        (20, 0, "/*"),
        (40, 0, "*/"),
    ]
    .into_iter()
    .enumerate()
    {
        let from = from.min(rope.len_lines() - 1);
        let start = rope.line_to_char(from);
        if remove > 0 {
            rope.remove(start..rope.line_to_char((from + remove).min(rope.len_lines())));
        }
        rope.insert(start, text);
        history.push(rope.clone());
        let version = step as u64 + 1;
        if let Some(job) = highlighter.prepare_job(
            &rope,
            path,
            version,
            Some((from, from + 3)),
            from.saturating_sub(5)..(from + 40).min(rope.len_lines()),
            None,
            "dark-plus",
        ) {
            obsolete.push((job, rope.clone()));
        }
        // Render provisional colors without running the authoritative workers.
        for line in from.saturating_sub(3)..(from + 10).min(rope.len_lines()) {
            if let Some(rendered) = highlighter.render_line(
                &rope.line(line).to_string(),
                line,
                Some(path),
                None,
                "dark-plus",
                version,
            ) {
                let text: String = rendered
                    .spans
                    .iter()
                    .map(|(_, text)| text.as_str())
                    .collect();
                assert_eq!(
                    text.trim_end_matches(['\r', '\n']),
                    rope.line(line).to_string().trim_end_matches(['\r', '\n'])
                );
            }
        }
    }
    let (syntaxes, themes) = highlighter.shared_sets();
    for (job, old) in obsolete.into_iter().take(8) {
        assert!(!highlighter.apply_batch(job.run(&old, &syntaxes, &themes, || false)));
    }
    let mut version = 9;
    let states: Vec<_> = history
        .iter()
        .rev()
        .chain(history.iter())
        .cloned()
        .collect();
    for state in states {
        version += 1;
        highlighter.prepare_job(
            &state,
            path,
            version,
            Some((0, state.len_lines())),
            0..40,
            None,
            "dark-plus",
        );
        finish(&mut highlighter, &state, version);
        let mut fresh = Highlighter::new();
        finish(&mut fresh, &state, 0);
        for line in 0..state.len_lines() {
            let text = state.line(line).to_string();
            let actual = highlighter
                .render_line(&text, line, Some(path), None, "dark-plus", version)
                .unwrap();
            let expected = fresh
                .render_line(&text, line, Some(path), None, "dark-plus", 0)
                .unwrap();
            assert_eq!(
                actual.spans, expected.spans,
                "version {version}, row {line}"
            );
            assert_eq!(
                actual.comments, expected.comments,
                "version {version}, row {line}"
            );
        }
    }
}

#[test]
#[ignore = "manual optimized-profile newline coordinate and viewport benchmark"]
fn benchmark_newline_cache_coordinates() {
    use std::hint::black_box;
    for (lines, checkpoints) in [(4_000, 256), (5_000, 4_096)] {
        let (mut highlighter, original) = populated(lines, checkpoints);
        let cache = highlighter
            .documents
            .get_mut(Path::new("coordinates.rs"))
            .unwrap();
        let mut old = Reference::from(cache);
        let mut samples = [Vec::new(), Vec::new()];
        let mut version = 0;
        for round in 0..11 {
            for reference in [round % 2 == 0, round % 2 != 0] {
                let mut rope = original.clone();
                let started = Instant::now();
                for step in 0..100 {
                    version += 1;
                    let start = rope.line_to_char(300);
                    if step % 2 == 0 {
                        rope.insert(start, "\n");
                    } else {
                        rope.remove(start..start + 1);
                    }
                    if reference {
                        old.advance(version, &rope, 300, 301);
                    } else {
                        cache.advance_version(version, &rope, 300, 301);
                    }
                    black_box(&rope);
                }
                let elapsed = started.elapsed() / 100;
                if round >= 2 {
                    samples[usize::from(reference)].push(elapsed);
                }
            }
        }
        for sample in &mut samples {
            sample.sort_unstable();
        }
        eprintln!(
            "newline edit + coordinate preparation, {lines} rows/{checkpoints} checkpoints: rebuilt {:?}, lazy {:?}",
            samples[1][4], samples[0][4]
        );
        // The unchanged render payloads exercise coordinate lookup and the
        // same Rc clones used by warm editor drawing, with no grammar parsing.
        let mut samples = [Vec::new(), Vec::new()];
        for round in 0..11 {
            for reference in [round % 2 == 0, round % 2 != 0] {
                let started = Instant::now();
                for _ in 0..10_000 {
                    for line in 500..560 {
                        let entry = if reference {
                            old.lines.get(&black_box(line))
                        } else {
                            cache.lines.get(&black_box(line))
                        }
                        .unwrap();
                        black_box(entry.rendered());
                    }
                }
                let elapsed = started.elapsed() / 10_000;
                if round >= 2 {
                    samples[usize::from(reference)].push(elapsed);
                }
            }
        }
        for sample in &mut samples {
            sample.sort_unstable();
        }
        eprintln!(
            "warm 60-row payload lookup, {lines} rows/{checkpoints} checkpoints: hash map {:?}, index {:?}",
            samples[1][4], samples[0][4]
        );
    }
}
