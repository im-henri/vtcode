use crate::editor::Editor;
use std::collections::BTreeMap;

fn reference(editor: &Editor, level: usize) -> BTreeMap<usize, usize> {
    let mut folds = BTreeMap::new();
    for line in 0..editor.line_count() {
        let indent = editor
            .rope
            .line(line)
            .chars()
            .take_while(|c| *c == ' ')
            .count()
            / editor.tab_size.max(1);
        if indent < level {
            if let Some(end) = editor.find_fold_end(line) {
                if indent + 1 == level {
                    folds.insert(line, end);
                }
            }
        }
    }
    folds
}

#[test]
fn folding_levels_preserve_regions_lsp_ranges_blank_lines_and_tab_behavior() {
    let source = "#region outer\nfn outer() {\n    if ready {\n        run();\n\n        if again {\n            more();\n        }\n    }\n\tother();\n}\n#endregion\n";
    for source in [String::new(), source.into(), source.replace('\n', "\r\n")] {
        for tab_size in [0, 1, 2, 4, 8] {
            for lsp in [false, true] {
                let mut editor = Editor::test_with_text(&source);
                editor.tab_size = tab_size;
                if lsp {
                    editor.lsp_folding_ranges = vec![(1, 10), (2, 8), (5, 7)];
                }
                for level in [0, 1, 2, 3, 4, 5, 6, usize::MAX] {
                    let expected = reference(&editor, level);
                    editor.fold_at_level(level);
                    assert_eq!(
                        editor.folded_ranges, expected,
                        "level={level}, tab_size={tab_size}, lsp={lsp}"
                    );
                }
            }
        }
    }
}

#[test]
#[ignore = "manual performance measurement"]
fn benchmark_folding_levels() {
    use std::{hint::black_box, time::Instant};
    let nested = format!(
        "{}run();\n{}",
        "#region scope\n".repeat(600),
        "#endregion\n".repeat(600)
    );
    let ordinary = "fn outer() {\n    if ready {\n        work();\n    }\n}\n".repeat(10_000);
    for (name, source, level) in [
        ("nested_regions", nested, 2),
        ("ordinary_code", ordinary, 2),
    ] {
        let mut editor = Editor::test_with_text(&source);
        editor.tab_size = 4;
        let expected = reference(&editor, level);
        editor.fold_at_level(level);
        assert_eq!(editor.folded_ranges, expected);
        let mut before = Vec::new();
        let mut after = Vec::new();
        for run in 0..13 {
            for old in if run % 2 == 0 {
                [true, false]
            } else {
                [false, true]
            } {
                let start = Instant::now();
                if old {
                    black_box(reference(black_box(&editor), level));
                } else {
                    editor.fold_at_level(black_box(level));
                    black_box(&editor.folded_ranges);
                }
                if run >= 2 {
                    (if old { &mut before } else { &mut after }).push(start.elapsed());
                }
            }
        }
        before.sort();
        after.sort();
        eprintln!(
            "fold_level workload={name} lines={} before={:?} after={:?}",
            editor.line_count(),
            before[5],
            after[5]
        );
    }
}
