use super::*;

/// The pattern predictor alone, independent of the shipped neural model.
fn patterns_only() -> Engine<'static> {
    let mut engine = Engine::new();
    engine.set_network(None);
    engine
}

fn predict_in(
    engine: &mut Engine,
    language: &str,
    before: &str,
    rest: &str,
    below: &str,
) -> Option<String> {
    engine
        .predict(
            Caret {
                key: 1,
                before,
                rest,
                below,
                syntax: Syntax::for_language(language),
                language: language_id(language),
            },
            Options::default(),
            &|| false,
        )
        .map(|prediction| prediction.text)
}

fn predict(before: &str) -> Option<String> {
    predict_in(&mut patterns_only(), "Rust", before, "", "")
}

#[test]
fn model_window_can_start_inside_a_long_line() {
    let Some(network) = model::embedded() else {
        return;
    };
    let before = format!("let values = [\n    {}", "1234567890".repeat(410));
    let mut engine = Engine::new();
    engine.set_stream(network, 1, &before);
    let from = network.fresh_start(engine.model_count());
    assert!(from > network.window());

    for output in ["", ", 23"] {
        let extended = format!("{before}{output}");
        let last = subwords::pieces(&extended).last().unwrap().0;
        let mut expected = vec![network.subwords().document()];
        network.subwords().encode(
            &extended[..last],
            &mut subwords::Cache::default(),
            &mut expected,
            &mut Vec::new(),
        );
        let (tokens, typed) = engine.model_state(network, from, output);
        assert_eq!(tokens, expected[from..]);
        assert_eq!(typed, extended.as_bytes()[last..]);
        assert!(tokens.len() <= network.window());
    }
}

#[test]
fn switching_active_files_discards_old_identifiers() {
    let mut engine = patterns_only();
    let syntax = Syntax::for_language("Rust");
    let below = "let tail_value = 0;";
    engine.set_active(1, "let old_name = 1;", below, syntax);
    engine.set_active(1, "let old_name = 2;", below, syntax);
    assert!(engine.names("tail_value"));

    engine.set_active(2, "let new_name = 3;", below, syntax);
    assert!(!engine.interner.ids.contains_key("old_name"));
    assert!(engine.names("new_name"));
    assert!(engine.names("tail_value"));
}

const SETTERS: &str = "\
impl Margins {
    fn set_left(&mut self, left: u32) {
        self.left = left;
    }

    fn set_top(&mut self, top: u32) {
        self.top = top;
    }

    fn set_bottom(&mut self, bottom: u32) {
        self.bottom = bottom;
    }

";

#[test]
fn repeated_structure_is_continued_with_the_new_names() {
    let before = format!("{SETTERS}    fn set_right(&mut self, right: u32) {{\n        self.");
    assert_eq!(predict(&before).as_deref(), Some("right = right;"));
}

#[test]
fn a_typed_prefix_is_completed_and_continued() {
    let before = format!("{SETTERS}    fn set_right(&mut self, right: u32) {{\n        self.ri");
    assert_eq!(predict(&before).as_deref(), Some("ght = right;"));
}

#[test]
fn a_typed_prefix_that_contradicts_the_pattern_gets_no_prediction() {
    let before = format!("{SETTERS}    fn set_right(&mut self, right: u32) {{\n        self.zz");
    assert_eq!(predict(&before), None);
}

#[test]
fn a_new_line_predicts_the_next_statement_of_a_repeated_pattern() {
    let before = format!("{SETTERS}    fn set_right(&mut self, right: u32) {{\n        ");
    assert_eq!(predict(&before).as_deref(), Some("self.right = right;"));
}

#[test]
fn sub_word_renames_are_applied_to_names_outside_the_matched_context() {
    let before = "\
let left_margin = style.left_margin();
let left_padding = style.left_padding();
let top_margin = style.top_margin();
let top_padding = style.top_padding();
let right_margin = style.right_margin();
let right_padding = style.";
    assert_eq!(predict(before).as_deref(), Some("right_padding();"));
}

#[test]
fn sub_word_renames_keep_the_letter_case_of_each_use() {
    let mut subwords = Subwords::default();
    subwords.learn("left_margin", "right_margin");
    assert_eq!(
        subwords.apply("set_left_edge").as_deref(),
        Some("set_right_edge")
    );
    assert_eq!(subwords.apply("LeftEdge").as_deref(), Some("RightEdge"));
    assert_eq!(subwords.apply("LEFT_EDGE").as_deref(), Some("RIGHT_EDGE"));
    assert_eq!(subwords.apply("top_edge"), None);
    // Contradicting evidence withdraws a sub-word renaming.
    subwords.learn("left_pad", "bottom_pad");
    assert_eq!(subwords.apply("set_left_edge"), None);
}

#[test]
fn identifiers_split_at_case_digit_and_underscore_boundaries() {
    assert_eq!(split_subwords("left_margin"), ["left", "margin"]);
    assert_eq!(split_subwords("leftMargin2"), ["left", "Margin", "2"]);
    assert_eq!(split_subwords("_private"), ["private"]);
    assert_eq!(split_subwords("x"), ["x"]);
}

#[test]
fn unseen_code_gets_no_prediction() {
    assert_eq!(predict("fn main() {\n    let total = "), None);
    assert_eq!(predict(""), None);
}

#[test]
fn comments_and_strings_stay_quiet() {
    let lines = "log(\"saved\");\nlog(\"saved\");\nlog(\"saved\");\n";
    assert!(predict(&format!("{lines}log(")).is_some());
    assert_eq!(predict(&format!("{lines}// log(")), None);
    assert_eq!(predict(&format!("{lines}log(\"sa")), None);
    let python = "print(value)\nprint(value)\nprint(value)\n";
    let mut engine = patterns_only();
    assert!(predict_in(&mut engine, "Python", &format!("{python}print("), "", "").is_some());
    assert_eq!(
        predict_in(&mut engine, "Python", &format!("{python}# print("), "", ""),
        None
    );
}

#[test]
fn text_right_of_the_caret_is_not_repeated() {
    let lines = "store.insert(key, value);\nstore.insert(key, value);\nstore.insert(key, value);\n";
    let mut engine = patterns_only();
    // An automatically inserted closing bracket stays where it is.
    assert_eq!(
        predict_in(
            &mut engine,
            "Rust",
            &format!("{lines}store.insert("),
            ")",
            ""
        )
        .as_deref(),
        Some("key, value")
    );
    // Other code right of the caret is being edited in place.
    assert_eq!(
        predict_in(
            &mut engine,
            "Rust",
            &format!("{lines}store."),
            "remove(key);",
            ""
        ),
        None
    );
    assert_eq!(
        predict_in(
            &mut engine,
            "Rust",
            &format!("{lines}sto"),
            "re.insert(key, value);",
            ""
        ),
        None
    );
}

const CHANNELS: &str = "
let alpha = Channel::open(config);
alpha.connect(peer);
let beta = Channel::open(config);
beta.connect(peer);
let gamma = Channel::open(config);
gamma.connect(peer);
";

#[test]
fn text_below_the_caret_is_evidence_too() {
    let mut engine = patterns_only();
    let before = "let delta = Channel::open(config);\ndelta.";
    assert_eq!(predict_in(&mut engine, "Rust", before, "", ""), None);
    assert_eq!(
        predict_in(&mut engine, "Rust", before, "", CHANNELS).as_deref(),
        Some("connect(peer);")
    );
}

#[test]
fn reference_documents_supply_patterns_until_removed() {
    let mut engine = patterns_only();
    engine.set_reference(7, CHANNELS, Syntax::for_language("Rust"));
    assert!(engine.has_reference(7));
    let before = "let peer = lookup();\nlet delta = Channel::open(config);\ndelta.";
    assert_eq!(
        predict_in(&mut engine, "Rust", before, "", "").as_deref(),
        Some("connect(peer);")
    );
    engine.remove_reference(7);
    assert!(!engine.has_reference(7));
    assert_eq!(predict_in(&mut engine, "Rust", before, "", ""), None);
}

fn release_blocks(names: &[&str]) -> String {
    names
        .iter()
        .map(|name| {
            format!(
                "    if let Some({name}) = self.{name}.take() {{
        self.release({name});
    }}
"
            )
        })
        .collect()
}

#[test]
fn an_open_block_is_continued_across_lines_with_the_caret_indentation() {
    let before = format!(
        "{}    if let Some(right) = self.right.take() {{",
        release_blocks(&["left", "top", "bottom"])
    );
    let block = predict(&before).unwrap();
    assert_eq!(
        block,
        "
        self.release(right);
    }"
    );
    // Single-line mode stops at the end of the caret line.
    let options = Options {
        multiline: false,
        ..Options::default()
    };
    let single = patterns_only().predict(
        Caret {
            key: 1,
            before: &before,
            rest: "",
            below: "",
            syntax: Syntax::for_language("Rust"),
            language: language_id("Rust"),
        },
        options,
        &|| false,
    );
    assert_eq!(single, None);
}

#[test]
fn typing_only_lexes_the_changed_line_again() {
    let mut engine = patterns_only();
    let mut before = "let value = compute(input);\n".repeat(2000);
    before.push_str("let value = com");
    predict_in(&mut engine, "Rust", &before, "", "");
    let first = engine.lexed_bytes;
    assert!(first >= before.len() - 16);
    before.push('p');
    predict_in(&mut engine, "Rust", &before, "", "");
    assert!(
        engine.lexed_bytes - first < 64,
        "{}",
        engine.lexed_bytes - first
    );
}

#[test]
fn oversized_input_is_declined_and_cancellation_stops_work() {
    let mut engine = patterns_only();
    let huge = "x = 1\n".repeat(MAX_DOCUMENT_BYTES / 6 + 1);
    assert_eq!(predict_in(&mut engine, "Python", &huge, "", ""), None);
    let before = format!("{SETTERS}    fn set_right(&mut self, right: u32) {{\n        self.");
    assert!(
        engine
            .predict(
                Caret {
                    key: 2,
                    before: &before,
                    rest: "",
                    below: "",
                    syntax: Syntax::for_language("Rust"),
                    language: language_id("Rust"),
                },
                Options::default(),
                &|| true
            )
            .is_none()
    );
}

#[test]
fn reference_corpus_is_bounded() {
    let mut engine = patterns_only();
    let text = "call(argument);\n".repeat(20_000);
    for key in 0..40 {
        engine.set_reference(key, &text, Syntax::for_language("Rust"));
    }
    let references = &engine.documents[2..];
    assert!(references.len() <= MAX_CORPUS_DOCUMENTS);
    assert!(references.iter().map(|d| d.text.len()).sum::<usize>() <= MAX_CORPUS_BYTES);
    // The most recently added documents are the ones kept.
    assert!(engine.has_reference(39));
    assert!(!engine.has_reference(0));
}

#[test]
fn lexer_groups_operators_strings_and_drops_comments() {
    let mut interner = Interner::default();
    let mut document = Document {
        text: "a::b -> c // note\n\n\nlet s = \"x y\"; /* gone */ 'a".into(),
        ..Document::default()
    };
    lex(
        &mut document,
        0,
        Syntax::for_language("Rust"),
        &mut interner,
    );
    let tokens: Vec<&str> = (0..document.ids.len())
        .map(|at| document.token(at))
        .collect();
    assert_eq!(
        tokens,
        [
            "a", "::", "b", "->", "c", "\n", "let", "s", "=", "\"x y\"", ";", "'", "a"
        ]
    );
    assert_eq!(kind(document.ids[6]), KEYWORD);
    assert_eq!(kind(document.ids[9]), STRING);
}

#[test]
fn calibration_model_orders_evidence_sensibly() {
    let step = |supporters, share, score| Step {
        end: 0,
        share,
        supporters,
        score,
        kind: IDENT,
        line_start: false,
        lines: 0,
        renamed: false,
        typed: 0,
        index: 0,
        groups: 1,
        local: true,
        foreign: false,
        nested: false,
        neural: 0,
        prose: false,
        coined: false,
        series: false,
        model: false,
        estimate: 0.,
    };
    // With the language model present every pattern step has its view: a
    // step it could not check is not taken unless the evidence is firm.
    for alone in [false, true] {
        let viewed = |mut step: Step| {
            step.neural = if alone { 0 } else { 9 };
            step
        };
        let weak = calibrated(&viewed(step(1, 0.4, 5.)), alone);
        let lone = calibrated(&viewed(step(1, 0.97, 30.)), alone);
        let strong = calibrated(&viewed(step(4, 0.97, 30.)), alone);
        assert!(
            weak < lone && lone < strong,
            "{alone} {weak} {lone} {strong}"
        );
        assert!(strong > 0.8, "{alone} {strong}");
    }
    for feature in step(4, 0.97, 30.).features() {
        assert!(feature < FEATURES);
    }
}

#[test]
fn comments_and_strings_spanning_lines_stay_quiet_after_incremental_lexing() {
    let calls = "log(value);\nlog(value);\nlog(value);\n";
    let mut engine = patterns_only();
    let open = format!("{calls}/* notes\n   log(");
    assert_eq!(predict_in(&mut engine, "Rust", &open, "", ""), None);
    // The next keystroke restarts lexing on the caret line, inside the comment.
    assert_eq!(
        predict_in(&mut engine, "Rust", &format!("{open}v"), "", ""),
        None
    );
    let closed = format!("{calls}/* notes\n   more */\nlog(");
    assert!(predict_in(&mut engine, "Rust", &closed, "", "").is_some());

    let python = "print(value)\nprint(value)\nprint(value)\n";
    let docstring = format!("{python}def run():\n    \"\"\"Explain.\n\n    print(");
    assert_eq!(predict_in(&mut engine, "Python", &docstring, "", ""), None);
    assert_eq!(
        predict_in(&mut engine, "Python", &format!("{docstring}v"), "", ""),
        None
    );
    let after = format!("{python}def run():\n    \"\"\"Explain.\n    \"\"\"\n    print(");
    assert!(predict_in(&mut engine, "Python", &after, "", "").is_some());
}

#[test]
fn comments_are_continued_by_the_language_model_alone() {
    let Some(network) = model::embedded() else {
        return; // No model asset in this build.
    };
    let before = "\
/// A rectangle with a position and a size.
pub struct Rectangle {
    pub width: f32,
    pub height: f32,
}

impl Rectangle {
    /// Creates a rectangle from its size.
    pub fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }

    /// Returns the area of the ";
    // The text holds no pattern to follow inside a comment.
    assert_eq!(predict(before), None);
    let mut engine = Engine::new();
    engine.set_network(Some(network));
    let caret = Caret {
        key: 1,
        before,
        rest: "",
        below: "",
        syntax: Syntax::for_language("Rust"),
        language: language_id("Rust"),
    };
    // Test builds are unoptimized: give the model the time it needs.
    let options = Options {
        budget: std::time::Duration::from_secs(120),
        ..Options::preset(2)
    };
    let prediction = engine
        .predict(caret, options, &|| false)
        .expect("a continuation of the comment");
    assert!(
        prediction.text.starts_with("rectangle"),
        "{:?}",
        prediction.text
    );
    assert!(!prediction.text.contains('\n'));
    assert!(prediction.trace.iter().all(|step| step.prose && step.model));
    // A word begun in the comment is completed from the words in view.
    let begun = format!("{before}rect");
    let prediction = engine
        .predict(
            Caret {
                before: &begun,
                ..caret
            },
            options,
            &|| false,
        )
        .expect("the rest of the word");
    assert!(
        prediction.text.starts_with("angle"),
        "{:?}",
        prediction.text
    );
}

#[test]
fn typing_without_capitals_matches_any_capitalisation() {
    assert!(begins("LayoutRenderers", "Layout"));
    assert!(begins("LayoutRenderers", "layoutr"));
    assert!(begins("MAX_WIDTH", "max_w"));
    // A capital typed on purpose is taken at its word.
    assert!(!begins("layout_renderers", "Layout"));
    assert!(!begins("LayoutRenderers", "LAYOUT"));
    assert!(!begins("Lay", "layout"));
    // The pattern predictor follows it, and reports the spelling to restore.
    let source = "\
struct LayoutRenderers;
fn first() -> LayoutRenderers {
    LayoutRenderers
}
fn second() -> LayoutRenderers {
    LayoutRenderers
}
fn third() -> layoutr";
    let prediction = patterns_only()
        .predict(
            Caret {
                key: 1,
                before: source,
                rest: "",
                below: "",
                syntax: Syntax::for_language("Rust"),
                language: language_id("Rust"),
            },
            Options::default(),
            &|| false,
        )
        .expect("a prediction");
    assert!(
        prediction.text.starts_with("enderers"),
        "{:?}",
        prediction.text
    );
    assert_eq!(prediction.retype.as_deref(), Some("LayoutR"));
}

#[test]
fn what_the_model_wrote_ahead_is_followed_token_by_token() {
    let mut interner = Interner::default();
    let rust = Syntax::for_language("Rust");
    // Typed " layoutt"; the model spelled it " LayoutT" and wrote on.
    let written = model::Written {
        bytes: b" LayoutTree::new(width);
"
        .to_vec(),
        steps: vec![
            (7, 0.5),
            (11, 0.8),
            (13, 0.9),
            (17, 0.7),
            (22, 0.6),
            (25, 0.9),
            (26, 0.95),
        ],
        ..model::Written::default()
    };
    let plan = Plan::new(0, b" layoutt".to_vec(), written, 0);
    // The first token continues the typed word, in the model's spelling,
    // with the chance of every token that writes it.
    let first = plan.next("", "layoutt", rust, &mut interner).unwrap();
    assert_eq!((first.text.as_str(), first.spaced), ("LayoutTree", false));
    assert!(
        (first.probability - 0.5 * 0.8).abs() < 1e-6,
        "{}",
        first.probability
    );
    // Once the prediction holds that, the next tokens follow.
    let second = plan.next("ree", "", rust, &mut interner).unwrap();
    assert_eq!(second.text, "::");
    assert!((second.probability - 0.9).abs() < 1e-6);
    let third = plan.next("ree::", "", rust, &mut interner).unwrap();
    assert_eq!(third.text, "new");
    // A witness's choice is read off the written text where it agrees.
    assert!((plan.chance_of("ree::", "new").unwrap() - 0.7).abs() < 1e-6);
    assert!(plan.chance_of("ree::", "default").is_none());
    // The line end is proposed as such; a prediction that left the written
    // text gets nothing from it.
    let end = plan
        .next("ree::new(width);", "", rust, &mut interner)
        .unwrap();
    assert_eq!(end.id, NEWLINE_ID);
    assert!(plan.next("ree::with", "", rust, &mut interner).is_none());
    assert!(
        plan.rest(
            "ree::new(width);
"
        )
        .is_none()
    );
}

#[test]
fn a_series_counts_on_by_number_or_letter() {
    let next = |a: &str, b: &str| series_next(a, b);
    assert_eq!(next("item1", "item2").as_deref(), Some("item3"));
    assert_eq!(
        next("myCustomOBSCURE_VAR0", "myCustomOBSCURE_VAR1").as_deref(),
        Some("myCustomOBSCURE_VAR2")
    );
    assert_eq!(next("10", "20").as_deref(), Some("30"));
    assert_eq!(next("file_08", "file_09").as_deref(), Some("file_10"));
    assert_eq!(next("row_a", "row_b").as_deref(), Some("row_c"));
    assert_eq!(next("a", "b").as_deref(), Some("c"));
    assert_eq!(
        next("\"step 1\"", "\"step 2\"").as_deref(),
        Some("\"step 3\"")
    );
    assert_eq!(next("x3", "x2").as_deref(), Some("x1"));
    // Not a series: different stems, no step, a letter inside a word, the end.
    assert_eq!(next("item1", "entry2"), None);
    assert_eq!(next("item1", "item1"), None);
    assert_eq!(next("data", "datb"), None);
    assert_eq!(next("row_y", "row_z"), None);
    assert_eq!(next("1", "0"), None);
    assert_eq!(
        series_around("item2"),
        Some(("item1".into(), "item3".into()))
    );
    assert_eq!(
        series_around("row_b"),
        Some(("row_a".into(), "row_c".into()))
    );
    assert_eq!(series_around("b"), None);
    assert_eq!(series_around("item0"), None);
    assert_eq!(series_around("value"), None);
}

/// Everything the predictor proposes with the thresholds out of the way:
/// what it would say if it had to.
fn best_guess(before: &str) -> String {
    let options = Options {
        first: 0.,
        next: 0.,
        floor: 0.,
        multiline: false,
        ..Options::default()
    };
    let caret = Caret {
        key: 1,
        before,
        rest: "",
        below: "",
        syntax: Syntax::for_language("JavaScript"),
        language: language_id("JavaScript"),
    };
    patterns_only()
        .predict(caret, options, &|| false)
        .map(|prediction| prediction.text)
        .unwrap_or_default()
}

#[test]
fn lines_that_count_through_a_series_are_continued_with_its_next_step() {
    let obscure = "\
function setup() {
    var myCustomOBSCURE_VAR0 = a;
    var myCustomOBSCURE_VAR1 = b;
    ";
    assert_eq!(best_guess(obscure), "var myCustomOBSCURE_VAR2 = c;");
    let items = "\
    const item1 = load(1);
    const item2 = load(2);
    const it";
    assert_eq!(best_guess(items), "em3 = load(3);");
    // One line in view is enough where it introduces the name.
    let rows = "\
    row_a.x = 10;
    row_b.x = 20;
    row_";
    assert!(
        best_guess(rows).starts_with("c.x = 30"),
        "{}",
        best_guess(rows)
    );
    // Using a numbered name again is not counting on.
    let uses = "\
    const item1 = load(1);
    const item2 = load(2);
    show(item1);
    show(it";
    let guess = best_guess(uses);
    assert!(
        guess.starts_with("em1") || guess.starts_with("em2"),
        "{guess}"
    );
}

#[test]
fn a_line_that_opens_a_block_is_kept_whole_when_the_block_is_uncertain() {
    // Every earlier loop stores a different field, so the line after the
    // caret line becomes uncertain partway through.
    let mut before = String::new();
    for body in [
        "store(entry.name)",
        "store(entry.size)",
        "store(entry.kind)",
        "store(entry.date)",
    ] {
        before.push_str(&format!(
            "    for entry in entries.iter() {{\n        {body};\n    }}\n"
        ));
    }
    before.push_str("    for entry in entries.");
    let Some(network) = model::embedded() else {
        return; // No model asset in this build.
    };
    let mut engine = Engine::new();
    engine.set_network(Some(network));
    let caret = Caret {
        key: 1,
        before: &before,
        rest: "",
        below: "",
        syntax: Syntax::for_language("Rust"),
        language: language_id("Rust"),
    };
    // Test builds are unoptimized: give the model the time it needs.
    let options = Options {
        budget: std::time::Duration::from_secs(120),
        ..Options::default()
    };
    let prediction = engine
        .predict(caret, options, &|| false)
        .map(|prediction| prediction.text);
    assert_eq!(prediction.as_deref(), Some("iter() {"));
}

#[test]
fn the_analogous_place_is_the_best_witness_outside_the_window() {
    let syntax = Syntax::for_language("Rust");
    let earlier =
        "fn left(&mut self, value: u32) {\n    self.left = value;\n    self.changed();\n}\n\n";
    let filler: String = (0..40)
        .map(|n| format!("let unrelated_{n} = {n};\n"))
        .collect();
    let before = format!(
        "{earlier}{filler}fn right(&mut self, value: u32) {{\n    self.right = value;\n    self."
    );
    let mut engine = patterns_only();
    engine.set_active(1, &before, "", syntax);
    // The window starts after the earlier method: that is where the
    // passage comes from, holding what followed the matched context there.
    let window_byte = earlier.len() + filler.len() / 2;
    let (index, start, end) = engine
        .analog_place(window_byte)
        .expect("an analogous place");
    assert_eq!(index, 0);
    let passage = &engine.documents[0].text[start..end];
    assert!(passage.contains("self.changed();"), "{passage:?}");
    assert!(end <= window_byte);
    // Inside the window it is not taken: the model sees it there already.
    assert!(engine.analog_place(0).is_none());
}
