use super::*;

/// `text` as the model reads it from the start of a document.
fn tokens(network: &Network<'_>, text: &str) -> Vec<u16> {
    let mut tokens = vec![network.subwords().document()];
    network
        .subwords()
        .encode(text, &mut Cache::default(), &mut tokens, &mut Vec::new());
    tokens
}

fn synthetic(count: usize) -> Vec<u16> {
    (0..count)
        .map(|at| (300 + (at * 37) % 4000) as u16)
        .collect()
}

fn read(
    network: &Network<'_>,
    session: &mut Session,
    passages: &Passages,
    tokens: &[u16],
) -> Vec<f32> {
    assert!(network.read(session, passages, tokens, 0, 1, &|| false));
    network.probabilities(session).to_vec()
}

const SOURCE: &str =
    "fn area(width: u32, height: u32) -> u32 {\n    let total = width * height;\n    total";

#[test]
fn damaged_model_files_are_rejected() {
    let bytes: &[u8] = include_bytes!("../assets/vtcomplete_inline_model.bin");
    assert!(Network::decode(&[]).is_none());
    assert!(Network::decode(b"VTNT1\0").is_none());
    if bytes.is_empty() {
        return; // No model asset in this build.
    }
    let network = Network::decode(bytes).unwrap();
    assert!(
        bytes
            .as_ptr_range()
            .contains(&network.token.cells.as_ptr().cast())
    );
    assert!(Network::decode(&bytes[..bytes.len() - 1]).is_none());
    let mut longer = bytes.to_vec();
    longer.push(0);
    assert!(Network::decode(&longer).is_none());
    let mut wrong = bytes.to_vec();
    wrong[0] = b'X';
    assert!(Network::decode(&wrong).is_none());
}

#[test]
fn the_editor_computes_the_same_probabilities_as_the_trainer() {
    let Some(network) = embedded() else { return };
    let vectors = include_bytes!("../tests/fixtures/vtcomplete_inline_model_vectors.bin");
    let Some(worst) = network.difference(vectors) else {
        panic!("the test vectors do not fit the shipped model");
    };
    // The trainer's arithmetic differs in rounding, and attention here uses
    // a short polynomial for e^x; a real disagreement is far larger.
    assert!(worst < 1.5e-3, "{worst}");
}

#[test]
fn typing_extends_the_previous_computation_instead_of_repeating_it() {
    let Some(network) = embedded() else { return };
    let none = Passages::default();
    let mut session = Session::default();
    let text = tokens(network, SOURCE);
    read(network, &mut session, &none, &text);
    assert_eq!(session.computed, text.len());
    // The same context again costs nothing; one more token costs one position.
    read(network, &mut session, &none, &text);
    assert_eq!(session.computed, text.len());
    let mut longer = text.clone();
    longer.push(text[3]);
    let stepped = read(network, &mut session, &none, &longer);
    assert_eq!(session.computed, text.len() + 1);
    // An edit recomputes only from the first changed token.
    let mut edited = longer.clone();
    *edited.last_mut().unwrap() = text[5];
    read(network, &mut session, &none, &edited);
    assert_eq!(session.computed, text.len() + 2);
    // A fresh session gives the same answer as an extended one.
    let fresh = read(network, &mut Session::default(), &none, &longer);
    for (a, b) in fresh.iter().zip(&stepped) {
        assert!((a - b).abs() < 1e-5);
    }
    let total: f32 = fresh.iter().sum();
    assert!((total - 1.).abs() < 1e-3, "{total}");
}

#[test]
fn the_window_slides_along_instead_of_starting_over() {
    let Some(network) = embedded() else { return };
    let window = network.length;
    let text = synthetic(2 * window + 100);
    let mut session = Session::default();
    let none = Passages::default();
    // A fresh session reads half a window ending at the caret.
    assert_eq!(network.start(&session, 10), 0);
    assert_eq!(network.start(&session, 5000), 5000 - window / 2);
    let mut typed = window / 2;
    while typed <= text.len() {
        let from = network.start(&session, typed);
        assert!(network.read(&mut session, &none, &text[from..typed], from, 1, &|| false));
        assert!(session.held() <= window, "{}", session.held());
        typed += if typed == text.len() {
            1
        } else {
            37.min(text.len() - typed)
        };
    }
    // Typing through two windows computed every token once, never again.
    assert_eq!(session.computed, text.len());
    assert!(session.dropped > window / 2);
    let total: f32 = network.probabilities(&mut session).iter().sum();
    assert!((total - 1.).abs() < 1e-3, "{total}");
    // Moving the caret far away starts a new window; no tokens is no answer.
    assert_eq!(network.start(&session, 50_000), 50_000 - window / 2);
    assert!(!network.read(&mut Session::default(), &none, &[], 0, 0, &|| false));
}

#[test]
fn passages_are_read_once_and_kept_while_typing() {
    let Some(network) = embedded() else { return };
    let encode = |text: &str| tokens(network, text)[1..].to_vec();
    let mut passages = Passages {
        elsewhere: encode("fn area(width: u32, height: u32) -> u32 {\n    width * height\n}"),
        below: encode("\n    println!(\"{}\", area(2, 3));\n}"),
    };
    let pinned = passages.elsewhere.len() + passages.below.len();
    let text = tokens(network, "fn main() {\n    let size = area(");
    let mut session = Session::default();
    read(network, &mut session, &passages, &text);
    assert_eq!(session.computed, pinned + text.len());
    // Typing on costs only the typed tokens, and they are marked as typed.
    let mut longer = text.clone();
    longer.push(text[4]);
    let stepped = read(network, &mut session, &passages, &longer);
    assert_eq!(session.computed, pinned + longer.len());
    assert_eq!(session.origins.last(), Some(&2));
    assert_eq!(session.origins[pinned], 1);
    // The answer is the one a fresh reading gives with the same marks.
    let mut fresh = Session {
        passages: passages.clone(),
        language: 1,
        ..Session::default()
    };
    let mut origins = vec![1u8; longer.len()];
    *origins.last_mut().unwrap() = 2;
    assert!(network.advance(&mut fresh, &longer, Some(&origins), &|| false));
    for (a, b) in network.probabilities(&mut fresh).iter().zip(&stepped) {
        assert!((a - b).abs() < 1e-5, "{a} {b}");
    }
    // Other passages are read anew.
    passages.below.pop();
    read(network, &mut session, &passages, &longer);
    assert_eq!(session.computed, 2 * (pinned + longer.len()) - 1);
}

#[test]
fn the_model_writes_on_from_what_was_typed() {
    let Some(network) = embedded() else { return };
    let none = Passages::default();
    let mut session = Session::default();
    let text = tokens(
        network,
        "fn main() {\n    let total = 1;\n    let other = 2;\n   ",
    );
    read(network, &mut session, &none, &text);
    let mark = session.mark();
    // The unfinished piece is spelled out first, capitals aside, and the
    // text runs to the end of the line at most.
    let written = network.write(&mut session, b" le", false, &|| false);
    assert!(written.bytes.len() > 3, "{written:?}");
    assert!(written.bytes[..3].eq_ignore_ascii_case(b" le"));
    let line_ends = written.bytes.iter().filter(|&&byte| byte == b'\n').count();
    assert!(line_ends <= 1);
    let mut end = 0;
    for &(step, probability) in &written.steps {
        assert!(step > end && probability > 0. && probability <= 1.0001);
        end = step;
    }
    assert_eq!(end, written.bytes.len());
    // Asked for the chance of what it wrote, the model gives the product
    // of the chances it wrote it with.
    let tail = std::str::from_utf8(&written.bytes[3..]).unwrap_or("");
    if !tail.is_empty() && written.bytes[..3] == *b" le" {
        let chance = network
            .chance(&mut session, mark, b" le", tail.as_bytes(), &|| false)
            .unwrap();
        let product: f32 = written.steps.iter().map(|step| step.1).product();
        assert!(
            (chance - product).abs() < 0.02 + product * 0.2,
            "{chance} {product}"
        );
    }
    // Text it would never write has next to no chance.
    let odd = network
        .chance(&mut session, mark, b" le", b"qzx9#", &|| false)
        .unwrap();
    assert!(odd < 0.01, "{odd}");
    // A cancelled writing still returns, and the session reads on.
    network.cut(&mut session, mark);
    let stopped = network.write(&mut session, b"", false, &|| true);
    assert!(stopped.steps.len() <= 1);
    read(network, &mut session, &none, &text);
}

#[test]
fn computing_in_blocks_gives_the_same_scores_as_one_token_at_a_time() {
    let Some(network) = embedded() else { return };
    let none = Passages::default();
    let text = tokens(network, &"let value = compute(value);\n".repeat(12));
    let all = read(network, &mut Session::default(), &none, &text);
    let mut session = Session::default();
    for end in 1..text.len() {
        read(network, &mut session, &none, &text[..end]);
    }
    let stepped = read(network, &mut session, &none, &text);
    for (a, b) in all.iter().zip(&stepped) {
        assert!((a - b).abs() < 1e-4, "{a} {b}");
    }
}

#[test]
fn a_cancelled_computation_leaves_the_session_usable() {
    let Some(network) = embedded() else { return };
    let none = Passages::default();
    let text = tokens(network, &"let value = compute(value);\n".repeat(20));
    let mut session = Session::default();
    assert!(!network.read(&mut session, &none, &text, 0, 1, &|| true));
    let fresh = read(network, &mut Session::default(), &none, &text);
    let again = read(network, &mut session, &none, &text);
    for (a, b) in fresh.iter().zip(&again) {
        assert!((a - b).abs() < 1e-5);
    }
}

#[test]
fn the_blocked_product_matches_plain_inner_products_for_any_shape() {
    for (rows, cols, tokens) in [
        (1, 8, 1),
        (3, 16, 5),
        (6, 24, 4),
        (7, 40, 9),
        (9, 12, 3),
        (32, 64, 130),
        (64, 128, 64),
        (37, 64, 1),
    ] {
        let cells: Vec<i8> = (0..rows * cols).map(|i| (i * 31 % 17) as i8 - 8).collect();
        let weights = Matrix {
            scale: 0.125,
            cells: &cells,
        };
        let input: Vec<f32> = (0..tokens * cols)
            .map(|i| ((i * 13 % 11) as f32 - 5.) * 0.25)
            .collect();
        let mut output = Vec::new();
        matmul(&weights, cols, &input, tokens, &mut output);
        assert_eq!(output.len(), rows * tokens);
        let weights = weights.floats();
        for token in 0..tokens {
            for row in 0..rows {
                let exact: f32 = (0..cols)
                    .map(|k| weights[row * cols + k] * input[token * cols + k])
                    .sum();
                let got = output[token * rows + row];
                assert!(
                    (got - exact).abs() <= 1e-3 * (1. + exact.abs()),
                    "{rows}x{cols}x{tokens} at {row},{token}"
                );
            }
        }
    }
    // Large enough to be divided between threads.
    let (rows, cols, tokens) = (512usize, 512usize, 64usize);
    let cells: Vec<i8> = (0..rows * cols).map(|i| (i * 7 % 13) as i8 - 6).collect();
    let weights = Matrix {
        scale: 0.01,
        cells: &cells,
    };
    let input: Vec<f32> = (0..tokens * cols)
        .map(|i| ((i * 5 % 9) as f32 - 4.) * 0.1)
        .collect();
    let mut output = Vec::new();
    matmul(&weights, cols, &input, tokens, &mut output);
    let weights = weights.floats();
    for (token, row) in [(0, 0), (63, 511), (17, 300), (40, 1)] {
        let exact: f32 = (0..cols)
            .map(|k| weights[row * cols + k] * input[token * cols + k])
            .sum();
        assert!((output[token * rows + row] - exact).abs() <= 1e-3 * (1. + exact.abs()));
    }
}

#[test]
fn attention_weighs_positions_the_same_with_and_without_the_fast_path() {
    for (heads, head, count) in [
        (8, 32, 1),
        (8, 32, 7),
        (8, 32, 133),
        (4, 16, 9),
        (6, 64, 20),
        (3, 12, 5),
    ] {
        let width = heads * head;
        let cell = |i: usize, a: usize, b: usize| ((i * a % b) as f32 - (b / 2) as f32) * 0.07;
        let query: Vec<f32> = (0..width).map(|i| cell(i, 31, 17)).collect();
        let keys: Vec<f32> = (0..count * width).map(|i| cell(i, 13, 23)).collect();
        let values: Vec<f32> = (0..count * width).map(|i| cell(i, 7, 19)).collect();
        let scale = 1. / (head as f32).sqrt();
        let mut out = vec![0.5f32; width];
        attend(
            &query,
            &keys,
            &values,
            heads,
            scale,
            &mut out,
            &mut Vec::new(),
        );
        for part in 0..heads {
            let span = part * head..(part + 1) * head;
            let scores: Vec<f64> = keys
                .chunks_exact(width)
                .map(|key| {
                    let dot: f64 = query[span.clone()]
                        .iter()
                        .zip(&key[span.clone()])
                        .map(|(a, b)| f64::from(*a) * f64::from(*b))
                        .sum();
                    dot * f64::from(scale)
                })
                .collect();
            let peak = scores.iter().copied().fold(f64::MIN, f64::max);
            let total: f64 = scores.iter().map(|score| (score - peak).exp()).sum();
            for cell in span.clone() {
                let exact: f64 = 0.5
                    + scores
                        .iter()
                        .zip(values.chunks_exact(width))
                        .map(|(score, value)| (score - peak).exp() / total * f64::from(value[cell]))
                        .sum::<f64>();
                assert!(
                    (f64::from(out[cell]) - exact).abs() < 1e-4,
                    "{heads}x{head}x{count} at {cell}"
                );
            }
        }
    }
}

#[test]
fn shared_work_runs_every_index_once_whoever_asks() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    // Several callers at once: one gets the helper threads, the others
    // compute alone, and nobody's indices are lost or repeated.
    std::thread::scope(|scope| {
        for caller in 0..4 {
            scope.spawn(move || {
                for count in [0usize, 1, 2, 7, 64, 500] {
                    let hits: Vec<AtomicUsize> = (0..count).map(|_| AtomicUsize::new(0)).collect();
                    parallel(count, &|index| {
                        hits[index].fetch_add(1, Ordering::Relaxed);
                    });
                    assert!(
                        hits.iter().all(|hit| hit.load(Ordering::Relaxed) == 1),
                        "{caller} {count}"
                    );
                }
            });
        }
    });
    let bytes: Vec<i8> = (0..37).map(|i| (i * 7 % 23) as i8 - 11).collect();
    let values: Vec<f32> = (0..37).map(|i| (i % 5) as f32 * 0.5).collect();
    let exact: f32 = bytes
        .iter()
        .zip(&values)
        .map(|(&b, v)| f32::from(b) * v)
        .sum();
    assert!((dot_bytes(&bytes, &values) - exact).abs() < 1e-3);
}

#[test]
fn the_cache_keeps_a_distribution_and_remembers_what_slid_away() {
    let Some(network) = embedded() else { return };
    let none = Passages::default();
    let mut session = Session::default();
    let text = tokens(network, &"let total = compute(total, 7);\n".repeat(8));
    let probabilities = read(network, &mut session, &none, &text);
    let sum: f32 = probabilities.iter().sum();
    assert!((sum - 1.).abs() < 1e-3, "{sum}");
    // Text longer than the window: what slid out is kept with the token
    // that followed it, up to a limit.
    let long = synthetic(network.window() + 300);
    let mut session = Session::default();
    read(network, &mut session, &none, &long);
    assert!(!session.memory_next.is_empty());
    assert_eq!(
        session.memory.len(),
        session.memory_next.len() * network.width
    );
    assert!(session.memory_next.len() <= MEMORY);
}

#[test]
fn a_line_weighed_against_the_text_below_is_whole_and_stays_read() {
    let Some(network) = embedded() else { return };
    let text = tokens(
        network,
        "def area(width, height):\n    if width < 0:\n        raise ValueError(width)\n   ",
    );
    let below = tokens(
        network,
        "\n    return width * height\n\n\ndef perimeter(width, height):\n    return 2 * (width + height)\n",
    );
    let passages = Passages {
        elsewhere: Vec::new(),
        below: below.clone(),
    };
    let mut session = Session::default();
    read(network, &mut session, &passages, &text);
    let mark = session.mark();
    let written = network.write_ranked(&mut session, b" ", false, 0, &below, &|| false);
    assert!(!written.steps.is_empty());
    let mut end = 0;
    for &(step, probability) in &written.steps {
        assert!(
            step > end && probability > 0. && probability <= 1.0001,
            "{written:?}"
        );
        end = step;
    }
    assert_eq!(end, written.bytes.len());
    assert_eq!(written.tokens.len(), written.steps.len());
    assert_eq!(written.evidence.len(), written.steps.len());
    // Like plain writing, it leaves the line read, its last token aside.
    assert_eq!(session.mark(), mark + written.tokens.len() - 1);
    // With no text below there is nothing to weigh: it writes as usual.
    network.cut(&mut session, mark);
    let plain = network.write_block(&mut session, b" ", false, 0, &|| false);
    network.cut(&mut session, mark);
    let unweighed = network.write_ranked(&mut session, b" ", false, 0, &[], &|| false);
    assert_eq!(plain.bytes, unweighed.bytes);
}

#[test]
fn writing_into_an_open_block_goes_on_past_line_ends() {
    let Some(network) = embedded() else { return };
    let none = Passages::default();
    let text = tokens(
        network,
        "fn check(value: i32) -> bool {\n    if value < 0 {\n        return false;\n    }\n    if value > 9 {",
    );
    let mut session = Session::default();
    read(network, &mut session, &none, &text);
    let mark = session.mark();
    let lines = |written: &Written| written.bytes.iter().filter(|&&byte| byte == b'\n').count();
    let line = network.write_block(&mut session, b"", false, 0, &|| false);
    assert!(lines(&line) <= 1);
    network.cut(&mut session, mark);
    let block = network.write_block(&mut session, b"", false, 1, &|| false);
    assert!(lines(&block) <= MAX_WRITTEN_LINES);
    assert!(block.bytes.len() >= line.bytes.len());
}
