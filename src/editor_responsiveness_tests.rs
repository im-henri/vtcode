use super::*;
use std::{hint::black_box, time::Instant};

// These references deliberately avoid the production cache and run batching.
fn reference_hint(editor: &Editor, line: usize) -> bool {
    if line >= editor.line_count().saturating_sub(1) {
        return false;
    }
    if let Ok(index) = editor
        .lsp_folding_ranges
        .binary_search_by_key(&line, |&(start, _)| start)
        && editor.lsp_folding_ranges[index].1 < editor.line_count()
    {
        return true;
    }
    let text = editor.rope.line(line).to_string();
    if Editor::is_region_start(text.trim()) {
        return true;
    }
    let Some(indent) = text.chars().position(|c| !c.is_whitespace()) else {
        return false;
    };
    editor
        .rope
        .lines_at(line + 1)
        .find_map(|line| {
            line.chars()
                .position(|c| !c.is_whitespace())
                .map(|next| next > indent)
        })
        .unwrap_or(false)
}

fn reference_visible(editor: &Editor, from: usize, count: usize) -> Vec<usize> {
    let mut rows = Vec::new();
    let mut line = from;
    while rows.len() < count && line < editor.line_count() {
        if let Some((_, &end)) = editor.folded_ranges.range(..line).next_back()
            && line < end
        {
            line = end;
            continue;
        }
        rows.push(line);
        line = editor.folded_ranges.get(&line).copied().unwrap_or(line + 1);
    }
    rows
}

fn check_hints(editor: &Editor) {
    for line in 0..editor.line_count() + 2 {
        assert_eq!(
            editor.can_fold_line(line),
            reference_hint(editor, line),
            "line {line}"
        );
    }
}

#[test]
fn fold_hint_cache_keeps_unaffected_rows_and_invalidates_blank_dependencies() {
    let mut editor = Editor::test_with_text("root\n\n\n    child\nother\n    nested\nlast\n");
    check_hints(&editor);
    let builds = editor.fold_hint_cache.borrow().builds;
    editor.cursor_line = 3;
    editor.insert_char(' ');
    check_hints(&editor);
    assert_eq!(
        editor.fold_hint_cache.borrow().builds - builds,
        2,
        "only the edited row and its nonblank predecessor need rebuilding"
    );
    editor.cursor_line = 1;
    editor.cursor_col = 0;
    editor.insert_char('x');
    assert!(
        !editor.can_fold_line(0),
        "a newly nonblank row changes the earlier hint"
    );
    check_hints(&editor);
}

#[test]
fn fold_hint_cache_handles_batched_line_shifts_and_same_version_replacement() {
    let mut editor = Editor::test_with_text("root\n  one\nroot\n    two\nend\n");
    check_hints(&editor);
    // Equal final line counts can still shift every line between two edits.
    editor.mark_syntax_dirty_range(1, 1);
    editor.rope.insert(editor.rope.line_to_char(1), "plain\n");
    editor.mark_syntax_dirty_range(4, 5);
    let start = editor.rope.line_to_char(4);
    let end = editor.rope.line_to_char(5);
    editor.rope.remove(start..end);
    check_hints(&editor);
    editor.rope = Rope::from_str("root\none\nroot\ntwo\nend\n");
    check_hints(&editor);
    editor.lsp_folding_ranges = vec![(0, 3)];
    check_hints(&editor);
    editor.lsp_folding_ranges.clear();
    check_hints(&editor);
}

#[test]
fn typing_does_not_scan_offscreen_fold_hints() {
    let mut editor = Editor::test_with_text(&"root\n    child\n".repeat(3_000));
    for line in 0..4_000 { editor.can_fold_line(line); }
    let visits = editor.fold_hint_cache.borrow().invalidation_visits;
    let builds = editor.fold_hint_cache.borrow().builds;
    editor.cursor_line = 2_000;
    editor.insert_char(' ');
    for line in 1_980..2_040 {
        assert_eq!(editor.can_fold_line(line), reference_hint(&editor, line));
    }
    assert_eq!(editor.fold_hint_cache.borrow().builds - builds, 2);
    assert_eq!(editor.fold_hint_cache.borrow().invalidation_visits - visits, 2);
}

#[test]
fn fold_hint_blank_span_index_survives_eviction_and_separated_edits() {
    let mut editor = Editor::test_with_text(&"root\n\n\n    child\n\nnext\n".repeat(900));
    for pass in 0..4 {
        for line in 0..4_096 { editor.can_fold_line(line); }
        // Trigger bounded cache eviction, including its dependency index.
        editor.can_fold_line(4_100);
        for line in (0..editor.line_count()).step_by(7) { editor.can_fold_line(line); }
        for line in [1, 8, 2_000, 4_098, 4_101] {
            editor.cursor_line = line;
            editor.cursor_col = 0;
            editor.insert_char(if pass % 2 == 0 { 'x' } else { ' ' });
            for row in line.saturating_sub(6)..line + 4 {
                assert_eq!(editor.can_fold_line(row), reference_hint(&editor, row));
            }
        }
    }
}

#[test]
#[ignore = "manual optimized-profile typing after browsing many source rows"]
fn benchmark_typing_with_populated_fold_cache() {
    let mut timings = [Vec::new(), Vec::new()];
    for round in 0..7 {
        for reference in [round % 2 == 0, round % 2 != 0] {
            REFERENCE_FOLD_INVALIDATION.set(reference);
            let source = "root\n    child\n".repeat(3_000);
            let mut editor = Editor::test_with_text(&source);
            editor.cursor_line = 2_000;
            for line in 0..4_000 { editor.can_fold_line(line); }
            let start = Instant::now();
            for _ in 0..1_000 {
                editor.insert_char('x');
                for line in 1_980..2_040 { black_box(editor.can_fold_line(line)); }
                editor.backspace();
                for line in 1_980..2_040 { black_box(editor.can_fold_line(line)); }
            }
            timings[usize::from(reference)].push(start.elapsed() / 2_000);
            assert_eq!(editor.rope.to_string(), source);
        }
    }
    REFERENCE_FOLD_INVALIDATION.set(false);
    for times in &mut timings { times.sort(); }
    eprintln!("typing + 60 gutter hints with 4000 visited rows: before={:?}, after={:?}", timings[1][3], timings[0][3]);
}

#[test]
fn fold_hints_match_uncached_results_during_edit_selection_and_history_sequences() {
    let mut editor =
        Editor::test_with_text("// #region sample\nroot\n\n\tchild\n  second\n// #endregion\n");
    let mut random = 0x51ef_98d2_u64;
    for step in 0..300 {
        random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
        editor.cursor_line = (random as usize >> 8) % editor.line_count();
        editor.cursor_col = (random as usize >> 16) % (editor.line_len(editor.cursor_line) + 1);
        match step % 9 {
            0 => editor.insert_char('\n'),
            1 => editor.insert_char(' '),
            2 => editor.insert_char('界'),
            3 => editor.backspace(),
            4 => editor.delete_char(),
            5 => editor.undo(),
            6 => editor.redo(),
            7 => {
                editor.selection_anchor = Some((0, 0));
                editor.delete_selection();
            }
            _ => editor.insert_char('\t'),
        }
        check_hints(&editor);
    }
}

#[test]
fn folded_viewports_match_row_walk_across_nested_and_shifted_ranges() {
    let mut editor = Editor::test_with_text(&"line\n".repeat(160));
    let mut random = 32_u64;
    for _ in 0..80 {
        editor.folded_ranges.clear();
        for _ in 0..25 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let start = random as usize % 170;
            let end = start + 1 + (random >> 32) as usize % 30;
            editor.folded_ranges.insert(start, end);
        }
        for from in [0, 1, 14, 60, 100, 159, 160, 180, usize::MAX] {
            for count in [0, 1, 8, 60, 200, usize::MAX] {
                assert_eq!(
                    editor.visible_lines(from, count),
                    reference_visible(&editor, from, count),
                    "from {from}, count {count}, folds {:?}",
                    editor.folded_ranges
                );
            }
        }
    }
    editor.folded_ranges.clear();
    assert_eq!(
        editor.visible_lines(2, usize::MAX),
        (2..161).collect::<Vec<_>>()
    );
}

fn median(mut operation: impl FnMut(), iterations: u32) -> std::time::Duration {
    operation();
    let mut samples = Vec::new();
    for _ in 0..7 {
        let start = Instant::now();
        for _ in 0..iterations {
            operation();
        }
        samples.push(start.elapsed() / iterations);
    }
    samples.sort();
    samples[3]
}

#[test]
#[ignore = "manual release editor responsiveness benchmark"]
fn benchmark_editor_responsiveness() {
    for clear in [true, false] {
        let source = format!(
            "{}footer\n{}",
            "root\n    child\n".repeat(20),
            "\n".repeat(20_000)
        );
        let mut editor = Editor::test_with_text(&source);
        editor.cursor_line = 1;
        editor.cursor_col = 4;
        let elapsed = median(
            || {
                editor.insert_char('x');
                if clear {
                    editor.fold_hint_cache.get_mut().clear();
                }
                for line in 0..60 {
                    black_box(editor.can_fold_line(line));
                }
                editor.backspace();
                if clear {
                    editor.fold_hint_cache.get_mut().clear();
                }
                for line in 0..60 {
                    black_box(editor.can_fold_line(line));
                }
            },
            50,
        );
        assert_eq!(editor.rope.to_string(), source);
        eprintln!("typing with fold hints clear_all={clear}: {elapsed:?}");
    }
    let mut editor = Editor::test_with_text(&"line\n".repeat(100_000));
    for start in (0..100_000).step_by(20) {
        if !(40_000..60_000).contains(&start) {
            editor.folded_ranges.insert(start, start + 8);
        }
    }
    let expected = reference_visible(&editor, 50_000, 80);
    assert_eq!(editor.visible_lines(50_000, 80), expected);
    for reference in [true, false] {
        let elapsed = median(
            || {
                black_box(if reference {
                    reference_visible(&editor, 50_000, 80)
                } else {
                    editor.visible_lines(50_000, 80)
                });
            },
            10_000,
        );
        eprintln!("folded viewport reference={reference}: {elapsed:?}");
    }
}
