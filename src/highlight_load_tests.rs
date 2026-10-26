use super::*;

fn reference(enabled: bool) {
    REFERENCE_EAGER_PREFIX.set(enabled);
    REFERENCE_ALL_CHECKPOINTS.set(enabled);
}

fn settle(highlighter: &mut Highlighter, rope: &Rope, version: u64, eager: bool) {
    reference(eager);
    let (syntax, themes) = highlighter.shared_sets();
    for _ in 0..20 {
        let Some(job) = highlighter.prepare_job(rope, Path::new("load.rs"), version,
            None, 0..rope.len_lines(), None, "dark-plus") else { reference(false); return; };
        assert!(highlighter.apply_batch(job.run_with_budget(rope, &syntax, &themes,
            &mut || false, Duration::from_secs(5))));
    }
    panic!("syntax did not settle");
}

fn displayed(highlighter: &mut Highlighter, rope: &Rope, version: u64, line: usize)
    -> Option<(HighlightSpans, Vec<(usize, usize)>)>
{
    highlighter.render_line(&rope.line(line).to_string(), line, Some(Path::new("load.rs")),
        None, "dark-plus", version).map(|line| (line.spans.as_ref().clone(), line.comments.as_ref().clone()))
}

#[test]
fn lazy_prefix_matches_eager_highlighting_through_pending_edits_and_history() {
    let mut rope = Rope::from_str(&"let value = \"界\"; // comment\n".repeat(600));
    let mut fast = Highlighter::new();
    let mut old = Highlighter::new();
    settle(&mut fast, &rope, 0, false);
    settle(&mut old, &rope, 0, true);
    let original = rope.clone();
    // Earlier edits shrink the valid prefix; newlines shift its coordinates.
    // Delay the workers across several versions, as on a heavily loaded CPU.
    for (step, (line, text)) in [(450, "x"), (320, "/*"), (200, "//"), (510, "*/"),
        (110, "界\n"), (100, "\r\n"), (10, "/*"), (550, "*/")].into_iter().enumerate() {
        let version = step as u64 + 1;
        rope.insert(rope.line_to_char(line), text);
        for (highlighter, eager) in [(&mut fast, false), (&mut old, true)] {
            reference(eager);
            highlighter.prepare_job(&rope, Path::new("load.rs"), version,
                Some((line, line + text.matches('\n').count())), 0..60, None, "dark-plus");
        }
        reference(false);
        for at in [0, 9, 10, 99, 100, 109, 110, 199, 200, 319, 320, 449, 450, 510, 550] {
            assert_eq!(displayed(&mut fast, &rope, version, at), displayed(&mut old, &rope, version, at),
                "step={step}, line={at}");
        }
        if step == 3 || step == 7 {
            settle(&mut fast, &rope, version, false);
            settle(&mut old, &rope, version, true);
            for at in 0..rope.len_lines() {
                assert_eq!(displayed(&mut fast, &rope, version, at), displayed(&mut old, &rope, version, at));
            }
        }
    }
    for (version, next) in [(9, original.clone()), (10, rope.clone()), (11, original)] {
        for (highlighter, eager) in [(&mut fast, false), (&mut old, true)] {
            reference(eager);
            highlighter.prepare_job(&next, Path::new("load.rs"), version,
                Some((0, next.len_lines())), 0..60, None, "dark-plus");
            settle(highlighter, &next, version, eager);
        }
        reference(false);
        for at in 0..next.len_lines() {
            assert_eq!(displayed(&mut fast, &next, version, at), displayed(&mut old, &next, version, at));
        }
    }
}

// Populate immutable snapshots directly to measure UI work independently of
// regex parsing and CPU speed. These are the same entries real workers publish.
fn populated(lines: usize, checkpoints: usize) -> (Highlighter, Rope) {
    let mut highlighter = Highlighter::new();
    let rope = Rope::from_str(&"let value = 42;\n".repeat((checkpoints * CHECKPOINT_INTERVAL).max(lines)));
    let path = Path::new("load.rs");
    let job = highlighter.prepare_job(&rope, path, 0, None, 0..1, None, "dark-plus").unwrap();
    let (syntax, themes) = highlighter.shared_sets();
    let batch = job.run_with_budget(&rope, &syntax, &themes, &mut || false, Duration::from_secs(1));
    assert!(highlighter.apply_batch(batch));
    let cache = highlighter.documents.get_mut(path).unwrap();
    let line = cache.lines[&0].clone();
    let checkpoint = cache.checkpoints[&0].clone();
    for at in 0..lines { cache.insert_line(at, line.clone()); }
    for at in 0..checkpoints { cache.checkpoints.insert(at * CHECKPOINT_INTERVAL, checkpoint.clone()); }
    cache.dirty_from = None;
    cache.dirty_through = None;
    (highlighter, rope)
}

#[test]
fn typing_preserves_prefix_snapshots_without_visiting_or_restamping_them() {
    let (mut highlighter, mut rope) = populated(4_000, 512);
    let path = Path::new("load.rs");
    let spans = highlighter.documents[path].lines[&20].spans.clone();
    rope.insert(rope.line_to_char(3_500), "x");
    assert!(highlighter.prepare_job(&rope, path, 1, Some((3_500, 3_500)),
        0..50, None, "dark-plus").is_none());
    assert_eq!(highlighter.documents[path].lines[&20].version, 0);
    assert_eq!(highlighter.documents[path].checkpoints[&256].version, 0);
    let displayed = highlighter.highlighted_line_by_index_if_cached(20, Some(path), None, "dark-plus", 1).unwrap();
    assert!(Rc::ptr_eq(&spans, &displayed.0));
    let job = highlighter.prepare_job(&rope, path, 1, None, 3_450..3_550, None, "dark-plus").unwrap();
    assert_eq!(job.parse_start, 3_328);
    assert!(highlighter.prepare_provisional_job(&job).is_none());
}

#[test]
fn pending_syntax_jobs_only_copy_reachable_checkpoints_including_final_boundary() {
    let (mut highlighter, mut rope) = populated(4_000, 4_096);
    rope.insert(0, "/*");
    for visible in [0..50, 0..256, 20_000..20_100] {
        let job = highlighter.prepare_job(&rope, Path::new("load.rs"), 1,
            Some((0, 0)), visible, None, "dark-plus").unwrap();
        let end = HighlightJob::hard_end(job.parse_start, &job.visible, rope.len_lines(), false);
        assert!(job.stale_checkpoints.iter().all(|(line, _)| *line <= end));
        // The initial one-line worker also published its final checkpoint at 1.
        assert_eq!(job.stale_checkpoints.len(), end / CHECKPOINT_INTERVAL + 1);
        if end.is_multiple_of(CHECKPOINT_INTERVAL) {
            assert_eq!(job.stale_checkpoints.last().unwrap().0, end);
        }
    }
}

#[test]
fn run_projection_preserves_every_style_through_unicode_edits_and_empty_spans() {
    let alphabet = ['a', 'b', ' ', '\t', '\r', '\n', '界', 'é', '\u{301}', '👩', '\u{200d}', '💻'];
    let mut seed = 19u64;
    for case in 0..500 {
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            (seed >> 32) as usize
        };
        let mut old = Vec::new();
        for _ in 0..case % 19 {
            let style = Style { foreground: Color { r: next() as u8, g: next() as u8,
                b: next() as u8, a: 255 }, ..Style::default() };
            let text: String = (0..next() % 16).map(|_| alphabet[next() % alphabet.len()]).collect();
            old.push((style, text));
        }
        let source: String = old.iter().flat_map(|(_, text)| text.trim_end_matches(['\r', '\n']).chars()).collect();
        let chars: Vec<_> = source.chars().collect();
        let first = next() % (chars.len() + 1);
        let end = first + next() % (chars.len() + 1 - first);
        let inserted: String = (0..next() % 20).map(|_| alphabet[next() % alphabet.len()]).collect();
        let changed: String = chars[..first].iter().copied().chain(inserted.chars()).chain(chars[end..].iter().copied()).collect();
        for current in [source.as_str(), changed.as_str(), "", "\n", "\r\n", "new text\n"] {
            assert_eq!(project_stale_spans(&old, current), reference_project_stale_spans(&old, current),
                "case={case}, current={current:?}");
        }
    }
}

#[test]
#[ignore = "manual optimized-profile stale syntax projection benchmark"]
fn benchmark_stale_syntax_projection() {
    use std::hint::black_box;
    for (name, length, unicode) in [("ordinary", 120, false), ("wide", 16_000, false), ("unicode", 4_000, true)] {
        let old: HighlightSpans = (0..8).map(|index| {
            let style = Style { foreground: Color { r: index * 30, g: 90, b: 180, a: 255 }, ..Style::default() };
            (style, if unicode { "界e\u{301} ".repeat(length / 32) } else { "name = value + 1; ".repeat(length / 8 / 18 + 1) })
        }).collect();
        let mut current: String = old.iter().map(|(_, text)| text.as_str()).collect();
        current.insert(0, 'x');
        assert_eq!(project_stale_spans(&old, &current), reference_project_stale_spans(&old, &current));
        let mut samples = [Vec::new(), Vec::new()];
        for round in 0..9 {
            for reference in [round % 2 == 0, round % 2 != 0] {
                let started = Instant::now();
                for _ in 0..100 {
                    black_box(if reference { reference_project_stale_spans(&old, &current) }
                        else { project_stale_spans(&old, &current) });
                }
                samples[usize::from(reference)].push(started.elapsed() / 100);
            }
        }
        for times in &mut samples { times.sort(); }
        eprintln!("style projection {name}: character arrays={:?}, runs={:?}", samples[1][4], samples[0][4]);
    }
}

#[test]
#[ignore = "manual optimized-profile UI syntax scheduling benchmark"]
fn benchmark_syntax_input_under_backlog() {
    use std::hint::black_box;
    for (name, edited, visible) in [("typing near end", 3_500, 3_450..3_550),
        ("pending parser near start", 0, 0..60), ("scroll past pending parser", 0, 20_000..20_100)] {
        let mut samples = [Vec::new(), Vec::new()];
        for round in 0..7 {
            for eager in [round % 2 == 0, round % 2 != 0] {
                let (mut highlighter, mut rope) = populated(4_000, 4_096);
                reference(eager);
                let started = Instant::now();
                for version in 1..=200 {
                    rope.insert(rope.line_to_char(edited), "x");
                    black_box(highlighter.prepare_job(&rope, Path::new("load.rs"), version,
                        Some((edited, edited)), visible.clone(), None, "dark-plus"));
                }
                samples[usize::from(eager)].push(started.elapsed() / 200);
                reference(false);
            }
        }
        for times in &mut samples { times.sort(); }
        eprintln!("syntax {name}: eager={:?} bounded={:?}", samples[1][3], samples[0][3]);
    }
}
