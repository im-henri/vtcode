use super::*;
use std::{hint::black_box, time::Instant};

#[test]
#[ignore = "manual mention lookup measurement; no filesystem or network requests"]
fn benchmark_chat_mention_lookup() {
    let source = include_str!("editor_text_ops.rs").replace('@', " ");
    let mut reports = Vec::new();
    for size in [16usize * 1024, 1024 * 1024] {
        let mut plain = source.repeat(size.div_ceil(source.len()));
        plain.truncate(plain.floor_char_boundary(size));
        for case in ["plain-end", "fenced-end", "mention-start", "mention-end"] {
            let (text, cursor) = match case {
                "fenced-end" => {
                    let text = format!("```rust\n{plain}\n```\ncontinue");
                    let n = text.len();
                    (text, n)
                }
                "mention-start" => (format!("@src/editor.rs\n{plain}"), 8),
                "mention-end" => {
                    let text = format!("{plain}\n@src/editor.rs");
                    let n = text.len();
                    (text, n)
                }
                _ => (plain.clone(), plain.len()),
            };
            let expected = current_mention(&text, cursor);
            let mut samples = Vec::new();
            for _ in 0..9 {
                let started = Instant::now();
                for _ in 0..20 {
                    assert_eq!(
                        black_box(current_mention(black_box(&text), black_box(cursor))),
                        expected
                    );
                }
                samples.push((started.elapsed() / 20).as_nanos() as u64);
            }
            samples.sort_unstable();
            let report = serde_json::json!({"case":case,"bytes":text.len(),"cursor":cursor,"medianNs":samples[4],"samples":9});
            eprintln!("{report}");
            reports.push(report);
        }
    }
    if let Some(output) = std::env::var_os("VTCODE_OPTIMIZATION_MENTION_REPORT") {
        std::fs::write(output, serde_json::to_vec_pretty(&reports).unwrap()).unwrap();
    }
}
