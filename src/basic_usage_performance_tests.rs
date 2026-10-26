//! Identical editing operations for baseline/candidate comparisons. Preparation
//! and state reset are outside the timed operation; no system clipboard I/O.
use crate::editor::Editor;
use std::{hint::black_box, time::Instant};

#[test]
#[ignore = "manual optimized baseline/candidate paste replay"]
fn benchmark_basic_paste_operations() {
    let mut rows = Vec::new();
    for size in [1024usize, 16 * 1024, 256 * 1024, 2 * 1024 * 1024] {
        for (case, eol, smart, col) in [
            ("lf-indented", "\n", true, 4),
            ("crlf-indented", "\r\n", true, 4),
            ("crlf-plain", "\r\n", false, 9),
            ("lf-plain", "\n", false, 9),
        ] {
            let line = format!("        let café = compute(界, value);{eol}");
            let payload = line.repeat(size.div_ceil(line.len()));
            let mut editor = Editor::test_with_text("before\n    value = suffix;\nafter\n");
            let source = editor.rope.clone();
            let mut samples = Vec::new();
            let rounds = if size < 20_000 { 100 } else { 5 };
            for _ in 0..7 {
                let mut elapsed = std::time::Duration::ZERO;
                for _ in 0..rounds {
                    editor.rope = source.clone();
                    editor.cursor_line = 1;
                    editor.cursor_col = col;
                    editor.selection_anchor = None;
                    editor.undo_stack.clear();
                    editor.redo_stack.clear();
                    editor.modified = true;
                    editor.break_undo_group();
                    let started = Instant::now();
                    editor.paste_snapshot_text(black_box(&payload), smart, Some(false));
                    elapsed += started.elapsed();
                    black_box(&editor.rope);
                }
                samples.push(elapsed.as_secs_f64() * 1e6 / rounds as f64);
            }
            samples.sort_by(f64::total_cmp);
            use sha2::{Digest, Sha256};
            let text = editor.rope.to_string();
            let row = serde_json::json!({"case":case,"inputBytes":payload.len(),"medianUs":samples[3],
                "outputSha256":format!("{:x}",Sha256::digest(text.as_bytes())),
                "cursor":[editor.cursor_line,editor.cursor_col],"undo":editor.undo_stack.len()});
            eprintln!("{row}");
            rows.push(row);
        }
    }
    if let Some(path) = std::env::var_os("VTCODE_BASIC_PASTE_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&rows).unwrap()).unwrap();
    }
}
