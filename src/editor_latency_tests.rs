use super::{Editor, RopeSearch};
use crate::ui_editor_highlight::{HighlightedLine, PLAIN_STYLE};
use ropey::Rope;
use std::{hint::black_box, time::Instant};

fn warm_coordinates(editor: &Editor) {
    for (line, source) in editor.rope.lines().enumerate() {
        let text = source.to_string();
        let text = text.trim_end_matches(['\r', '\n']);
        let length = text.chars().count();
        let view = HighlightedLine::new(None, text, length, PLAIN_STYLE);
        editor.render_line_cache.borrow_mut().line(
            line,
            (
                editor.content_version,
                editor.wrap_revision,
                editor.tab_size,
            ),
            &editor.rope,
            &view,
            None,
            length,
            false,
        );
    }
}

fn measure(label: &str, rounds: usize, mut run: impl FnMut()) {
    let mut samples = Vec::new();
    run();
    for _ in 0..9 {
        let started = Instant::now();
        for _ in 0..rounds {
            run();
        }
        samples.push(started.elapsed() / rounds as u32);
    }
    samples.sort();
    eprintln!(
        "LATENCY {label}: median={:?}, max={:?}",
        samples[4], samples[8]
    );
}

#[test]
#[ignore = "manual optimized-profile search and painted-coordinate survey"]
fn benchmark_native_latency_survey() {
    for (label, text, pattern) in [
        (
            "absent identifier / 100k source lines",
            "    let value = calculate(input);\n".repeat(100_000),
            "missing_symbol",
        ),
        (
            "common-prefix miss / 100k source lines",
            "    let value = calculate(input);\n".repeat(100_000),
            "value_absent",
        ),
        (
            "Unicode miss / 100k source lines",
            "    let café = calculate(\"世界🙂\");\n".repeat(100_000),
            "世界🙂!",
        ),
        (
            "dense matches / 100k source lines",
            "    let value = calculate(input);\n".repeat(100_000),
            "value",
        ),
        (
            "short source / all 60 occurrences",
            "let value = calculate(value);\n".repeat(30),
            "value",
        ),
        (
            "repeated prefix / 64k chars",
            format!("{}b", "a".repeat(65_536)),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab",
        ),
    ] {
        let rope = Rope::from_str(&text);
        let search = RopeSearch::new(pattern);
        measure(label, if text.len() < 4096 { 1000 } else { 5 }, || {
            black_box(search.matches(&rope, 0, rope.len_chars()).count());
        });
    }

    for (name, source) in [
        (
            "ordinary ASCII",
            "    let value = calculate(input);".repeat(4),
        ),
        ("ordinary Unicode", "\tlet café = \"世界🙂 é\"; ".repeat(4)),
        ("long Unicode", "\tlet café = \"世界🙂 é\"; ".repeat(160)),
    ] {
        let editor = Editor::test_with_text(&(source.clone() + "\n").repeat(2));
        let column = source.chars().count().saturating_sub(3);
        warm_coordinates(&editor);
        eprintln!(
            "STORAGE {name}: {} bytes",
            editor.render_line_cache.borrow().decoration_storage().1
        );
        let cell = editor.cell_at_source_column(0, 0, column);
        measure(&format!("{name} / source-to-cell"), 1000, || {
            black_box(editor.cell_at_source_column(0, 0, column));
        });
        measure(&format!("{name} / cell-to-source"), 1000, || {
            black_box(editor.source_column_at_cell(0, 0, editor.line_len(0), cell));
        });
        let mut editor = editor;
        editor.cursor_col = column;
        editor.viewport_width = 100;
        measure(&format!("{name} / Up+Down"), 200, || {
            editor.move_down();
            editor.move_up();
            black_box((editor.cursor_line, editor.cursor_col));
        });
    }
}
