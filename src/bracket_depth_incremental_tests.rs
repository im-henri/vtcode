use super::*;
use std::{cell::Cell, time::Duration};

fn update(previous: Option<&BracketDepthIndex>, source: &Rope, id: u64) -> BracketDepthIndex {
    BracketDepthIndex::update_cancellable(
        previous,
        source.clone(),
        id,
        0,
        0,
        Instant::now() + Duration::from_secs(20),
        &|| false,
    )
    .unwrap()
}

// The original byte scan is independent of block summaries and transfer
// composition, so it catches both invalid reuse and depth-clamping mistakes.
fn reference(source: &Rope) -> Vec<(usize, i32)> {
    let mut checkpoints = vec![(0, 0)];
    let mut depth = 0i32;
    for (line, text) in source.lines().enumerate() {
        if checkpoints.last().unwrap().1 != depth {
            checkpoints.push((line, depth));
        }
        if text.len_chars() > MAX_BRACKET_LINE_CHARS {
            continue;
        }
        for byte in text.chunks().flat_map(str::bytes) {
            match byte {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth = (depth - 1).max(0),
                _ => {}
            }
        }
    }
    checkpoints
}

fn verify(index: &BracketDepthIndex) {
    assert!(index.enabled());
    let expected = reference(&index.source);
    for line in 0..index.source.len_lines() {
        let at = expected.partition_point(|&(start, _)| start <= line) - 1;
        assert_eq!(
            index.line_starting_depth(line),
            Some(expected[at].1),
            "line {line}"
        );
    }
    assert_eq!(index.line_starting_depth(index.source.len_lines()), None);
}

#[test]
fn local_edit_reuses_summary_storage_and_propagates_depth_through_unchanged_suffix() {
    let mut source = Rope::from_str(&"fn f() {\n    call(value);\n}\n".repeat(20_000));
    let before = update(None, &source, 1);
    source.insert(0, "{{{\n");
    let after = update(Some(&before), &source, 1);
    verify(&after);
    assert!(after.scanned_lines <= BLOCK_LINES + 1);
    assert!(Arc::ptr_eq(
        &before.blocks[10].summary,
        &after.blocks[11].summary
    ));
    assert_eq!(after.line_starting_depth(30_001), Some(3));
    let same = update(Some(&after), &source, 1);
    assert_eq!(same.scanned_lines, 0);
    let reloaded = Rope::from_str(&source.to_string());
    assert_eq!(update(Some(&after), &reloaded, 1).scanned_lines, 0);
    assert_eq!(
        update(Some(&after), &source, 2).scanned_lines,
        source.len_lines()
    );
}

#[test]
fn incremental_depths_match_full_scans_after_random_edits_and_undo_redo() {
    for ending in ["\n", "\r\n", "\r"] {
        let mut source = Rope::from_str(
            &format!("]}}[{ending}界 (🦀){{{ending}value{ending}}}{ending}").repeat(170),
        );
        let mut index = update(None, &source, 1);
        let mut history = vec![source.clone()];
        let mut random = 1997_u64;
        let inserts = ["x", "", "\r", "\n", "\r\n", "){\n[(", "🦀]\t", "界({})"];
        for step in 0..300 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let at = (random >> 32) as usize % (source.len_chars() + 1);
            let len = (random as usize % 11).min(source.len_chars() - at);
            source.remove(at..at + len);
            source.insert(at, inserts[step % inserts.len()]);
            index = update(Some(&index), &source, 1);
            verify(&index);
            if step % 30 == 0 {
                history.push(source.clone());
            }
        }
        for text in history.iter().rev().chain(history.iter()) {
            index = update(Some(&index), text, 1);
            verify(&index);
        }
    }
}

#[test]
fn long_line_threshold_crossings_empty_documents_and_crlf_boundaries_are_exact() {
    let mut source = Rope::from_str(&format!(
        "{{\r\n{}\r\n}}\r\n",
        "[".repeat(MAX_BRACKET_LINE_CHARS - 2)
    ));
    let mut index = update(None, &source, 1);
    verify(&index);
    for edit in ["x", "[", "\n", "\r\n", "🦀"] {
        source.insert(4, edit);
        index = update(Some(&index), &source, 1);
        verify(&index);
    }
    for text in ["", "\r", "\r\n", "x\r\nx\n", ")\n)\n(\n", "{{\n", ""] {
        source = Rope::from_str(text);
        index = update(Some(&index), &source, 1);
        verify(&index);
    }
}

#[test]
fn repeated_newlines_do_not_fragment_the_retained_index() {
    let mut source = Rope::from_str(&"{value}\n".repeat(2_000));
    let mut index = update(None, &source, 1);
    for _ in 0..1_000 {
        source.insert(source.line_to_char(128), "\n");
        index = update(Some(&index), &source, 1);
        assert!(index.blocks.len() <= source.len_lines().div_ceil(BLOCK_LINES) + 1);
        assert!(index.scanned_lines <= BLOCK_LINES * 2);
    }
    verify(&index);
}

#[test]
fn cancelled_or_expired_incremental_work_does_not_modify_the_previous_index() {
    let mut source = Rope::from_str(&"{\n)\n".repeat(2_000));
    let before = update(None, &source, 1);
    source.insert(0, "[[\n");
    let checks = Cell::new(0);
    assert!(
        BracketDepthIndex::update_cancellable(
            Some(&before),
            source.clone(),
            1,
            1,
            1,
            Instant::now() + Duration::from_secs(10),
            &|| {
                checks.set(checks.get() + 1);
                checks.get() > 8
            }
        )
        .is_none()
    );
    verify(&before);
    let expired = BracketDepthIndex::update_cancellable(
        Some(&before),
        source.clone(),
        1,
        1,
        1,
        Instant::now(),
        &|| false,
    )
    .unwrap();
    assert!(!expired.enabled());
    assert!(expired.blocks.is_empty());
    verify(&update(Some(&before), &source, 1));
}

#[test]
#[ignore = "manual component benchmark; use optimized build on one logical processor"]
fn benchmark_incremental_bracket_depth() {
    use std::hint::black_box;
    for lines in [2_000, 100_000] {
        let source =
            Rope::from_str(&"fn f() {\n    call(value);\n    more(value);\n}\n".repeat(lines / 4));
        let before = update(None, &source, 1);
        let mut edited = source.clone();
        edited.insert(edited.line_to_char(lines / 2), "{{\n");
        verify(&update(Some(&before), &edited, 1));
        let mut full = Vec::new();
        let mut cold = Vec::new();
        let mut warm = Vec::new();
        for n in 0..15 {
            for which in [n % 3, (n + 1) % 3, (n + 2) % 3] {
                let start = Instant::now();
                match which {
                    0 => {
                        black_box(reference(black_box(&edited)));
                        full.push(start.elapsed());
                    }
                    1 => {
                        black_box(update(None, black_box(&edited), 1));
                        cold.push(start.elapsed());
                    }
                    _ => {
                        black_box(update(Some(&before), black_box(&edited), 1));
                        warm.push(start.elapsed());
                    }
                }
            }
        }
        full.sort();
        cold.sort();
        warm.sort();
        eprintln!(
            "bracket depth {lines} lines: previous={:?}, initial={:?}, edited={:?}, scanned={}",
            full[7],
            cold[7],
            warm[7],
            update(Some(&before), &edited, 1).scanned_lines
        );
    }
}
