//! Randomized differential check of incremental highlighting. Edits, scrolls,
//! late/dropped worker batches, provisional passes and prefetch run through
//! the same API the event loop uses; once a viewport settles, every row must
//! match a fresh full TextMate pass over the final text.
use super::*;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

const THEME: &str = "dark-plus";
const PATH: &str = "fuzz.c";
const VIEW: usize = 50;

fn source(lines: usize) -> String {
    (0..lines)
        .map(|line| match line % 7 {
            0 => format!("int value_{line} = {line};\n"),
            1 => format!("/* note {line} */ call({line});\n"),
            2 => "    if (ready) { run(\"text\"); }\n".to_string(),
            3 => format!("// line comment {line}\n"),
            4 => "char *s = \"quoted\";\n".to_string(),
            5 => "\n".to_string(),
            _ => format!("return value_{line};\n"),
        })
        .collect()
}

fn reference(rope: &Rope) -> Vec<HighlightSpans> {
    let mut highlighter = Highlighter::new();
    let path = Path::new(PATH);
    let lines = rope.len_lines();
    let job = highlighter
        .prepare_job(rope, path, 0, None, 0..lines, None, THEME)
        .unwrap();
    let (syntax_set, theme_set) = highlighter.shared_sets();
    let batch = job.run_with_budget(
        rope,
        &syntax_set,
        &theme_set,
        &mut || false,
        Duration::from_secs(60),
    );
    assert!(highlighter.apply_batch(batch));
    (0..lines)
        .map(|line| {
            highlighter
                .highlighted_line_by_index_if_cached(line, Some(path), None, THEME, 0)
                .unwrap()
                .0
                .as_ref()
                .clone()
        })
        .collect()
}

fn summarize(batch: &HighlightBatch) -> String {
    format!(
        "v{} prov={} pre={} parsed_to={} converged={:?} entries={:?} checkpoints={:?}",
        batch.content_version,
        batch.provisional,
        batch.prefetch,
        batch.parsed_to,
        batch.converged_at,
        batch.entries.first().map(|e| e.line)..batch.entries.last().map(|e| e.line + 1),
        batch
            .checkpoints
            .iter()
            .map(|(line, _, _)| *line)
            .collect::<Vec<_>>()
    )
}

fn line_text(rope: &Rope, line: usize) -> String {
    rope.line(line).to_string()
}

struct Driver {
    highlighter: Highlighter,
    rope: Rope,
    version: u64,
    dirty: Option<(usize, usize)>,
    scroll: usize,
    pending: Vec<HighlightBatch>,
    log: Vec<String>,
}

impl Driver {
    fn viewport(&self) -> Range<usize> {
        let lines = self.rope.len_lines();
        let start = self.scroll.min(lines);
        let end = (start + VIEW).min(lines);
        start.saturating_sub(25)..(end + 25).min(lines)
    }

    fn visible(&self) -> Range<usize> {
        let lines = self.rope.len_lines();
        let start = self.scroll.min(lines.saturating_sub(1));
        start..(start + VIEW).min(lines)
    }

    fn mark(&mut self, start: usize, end: usize) {
        let last = self.rope.len_lines().saturating_sub(1);
        let start = start.min(last);
        let end = end.max(start).min(last);
        self.dirty = Some(self.dirty.map_or((start, end), |(from, through)| {
            (from.min(start), through.max(end))
        }));
    }

    fn edit(&mut self, rng: &mut Rng) {
        let len = self.rope.len_chars();
        // Bias edits toward the visible region, as a user types there.
        let visible = self.visible();
        let line = visible.start + rng.below(visible.len().max(1));
        let line = line.min(self.rope.len_lines().saturating_sub(1));
        let line_start = self.rope.line_to_char(line);
        let line_len = self.rope.line(line).len_chars();
        let at = (line_start + rng.below(line_len + 1)).min(len);
        if rng.chance(35) && len > 0 {
            let count = 1 + rng.below(30);
            let end = (at + count).min(len);
            let first = self.rope.char_to_line(at);
            let last_old = self.rope.char_to_line(end);
            self.mark(first, last_old);
            self.rope.remove(at..end);
            self.log.push(format!("delete {at}..{end}"));
            self.version += 1;
            self.mark(first, first);
        } else {
            const SNIPPETS: &[&str] = &[
                "/*",
                "*/",
                "\n",
                "/* x */",
                "\"",
                "// c\n",
                "int y;\n",
                "x",
                "/* a\nb */",
                "\n\n",
                "{",
                "}",
                "'",
                "\\",
            ];
            let text = SNIPPETS[rng.below(SNIPPETS.len())];
            let first = self.rope.char_to_line(at);
            self.mark(first, first);
            self.rope.insert(at, text);
            self.log.push(format!("insert {at} {text:?}"));
            self.version += 1;
            self.mark(first, first + text.matches('\n').count());
        }
    }

    fn render(&mut self) {
        let path = Path::new(PATH);
        for line in self.visible() {
            let text = line_text(&self.rope, line);
            let _ =
                self.highlighter
                    .render_line(&text, line, Some(path), None, THEME, self.version);
        }
    }

    fn run(&self, job: HighlightJob, budget: Duration) -> HighlightBatch {
        let (syntax_set, theme_set) = self.highlighter.shared_sets();
        job.run_with_budget(&self.rope, &syntax_set, &theme_set, &mut || false, budget)
    }

    /// One event-loop visit: schedule, maybe run now, later, or never.
    fn pump(&mut self, rng: &mut Rng) {
        let path = Path::new(PATH);
        let changed = self.dirty.take();
        let job = self.highlighter.prepare_job(
            &self.rope,
            path,
            self.version,
            changed,
            self.viewport(),
            None,
            THEME,
        );
        self.log.push(format!(
            "  prepare v{} changed={changed:?} view={:?} -> {}",
            self.version,
            self.viewport(),
            job.as_ref().map_or("none".into(), |job| format!(
                "parse_start={} through={:?} stale={:?}",
                job.parse_start,
                job.invalidated_through,
                job.stale_checkpoints
                    .iter()
                    .map(|(line, c)| (*line, c.version))
                    .collect::<Vec<_>>()
            ))
        ));
        let Some(job) = job else {
            if rng.chance(30)
                && let Some(prefetch) = self.highlighter.prepare_prefetch_job(
                    &self.rope,
                    path,
                    self.version,
                    None,
                    THEME,
                )
            {
                let batch = self.run(prefetch, Duration::ZERO);
                self.pending.push(batch);
            }
            return;
        };
        if let Some(provisional) = self.highlighter.prepare_provisional_job(&job) {
            let batch = self.run(provisional, Duration::from_secs(10));
            self.pending.push(batch);
        }
        if rng.chance(20) {
            return; // cancelled by the next event
        }
        let budget = if rng.chance(50) {
            Duration::ZERO
        } else {
            Duration::from_secs(10)
        };
        let batch = self.run(job, budget);
        self.pending.push(batch);
    }

    fn deliver(&mut self, rng: &mut Rng) {
        while !self.pending.is_empty() && rng.chance(60) {
            let index = rng.below(self.pending.len());
            let batch = self.pending.swap_remove(index);
            let summary = summarize(&batch);
            let outcome = self.highlighter.apply_batch_with_outcome(batch);
            self.log
                .push(format!("  apply {summary} accepted={}", outcome.accepted()));
        }
    }

    fn settle(&mut self) {
        for batch in self.pending.drain(..) {
            self.highlighter.apply_batch_with_outcome(batch);
        }
        let path = Path::new(PATH);
        for _ in 0..200 {
            let changed = self.dirty.take();
            let Some(job) = self.highlighter.prepare_job(
                &self.rope,
                path,
                self.version,
                changed,
                self.viewport(),
                None,
                THEME,
            ) else {
                return;
            };
            let batch = self.run(job, Duration::from_secs(10));
            assert!(self.highlighter.apply_batch(batch));
        }
        panic!("viewport never settled\n{}", self.log.join("\n"));
    }

    fn check(&mut self, seed: u64, step: usize) {
        self.settle();
        let expected = reference(&self.rope);
        let path = Path::new(PATH);
        for line in self.visible() {
            let text = line_text(&self.rope, line);
            let got = self
                .highlighter
                .render_line(&text, line, Some(path), None, THEME, self.version)
                .map(|rendered| rendered.spans.as_ref().clone());
            if got.as_ref() != Some(&expected[line]) {
                panic!(
                    "seed {seed} step {step}: line {line} {text:?}\n got: {got:?}\nwant: {:?}\nlog:\n{}",
                    expected[line],
                    self.log.join("\n")
                );
            }
        }
    }
}

fn fuzz(seed: u64, steps: usize, lines: usize) {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let rope = Rope::from_str(&source(lines));
    let mut driver = Driver {
        highlighter: Highlighter::new(),
        rope,
        version: 0,
        dirty: None,
        scroll: 0,
        pending: Vec::new(),
        log: Vec::new(),
    };
    for step in 0..steps {
        match rng.below(10) {
            0..=5 => driver.edit(&mut rng),
            6 | 7 => {
                let lines = driver.rope.len_lines();
                driver.scroll = if rng.chance(50) {
                    rng.below(lines)
                } else {
                    driver
                        .scroll
                        .saturating_add(rng.below(60))
                        .saturating_sub(30)
                };
                driver.log.push(format!("scroll {}", driver.scroll));
            }
            _ => {}
        }
        driver.pump(&mut rng);
        driver.deliver(&mut rng);
        driver.render();
        if step % 25 == 24 {
            driver.check(seed, step);
        }
    }
    driver.check(seed, steps);
}

#[test]
fn incremental_highlighting_matches_a_fresh_parse_after_random_edits() {
    let seeds: u64 = std::env::var("VT_HIGHLIGHT_FUZZ_SEEDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(8);
    for seed in 1..=seeds {
        fuzz(seed, 150, 900);
    }
}
