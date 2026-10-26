use super::*;

#[test]
fn streaming_initial_prefixes_preserve_unicode_acronyms_and_digit_boundaries() {
    for word in ["HTTPClient", "get_user_config", "İstanbulValue", "KelvinValue", "Δelta42Next", "foo$bar", "___", "変数名", ""] {
        let full = initials(word);
        for prefix in ["", "g", "gu", "guc", "hc", "http", "i", "i\u{307}", "kv", "δ4n", "f", "fb", "変", "x"] {
            assert_eq!(initials_start_with(word, prefix), full.starts_with(prefix), "{word:?}, {prefix:?}");
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Word(String, Context, usize, bool);

fn words(chunks: &[&str]) -> Vec<Word> {
    let mut analyzer = Analyzer::default();
    let mut result: Vec<Word> = Vec::new();
    let mut record = |event: Event<'_>| match event {
        Event::Word {
            word,
            context,
            offset,
        } => result.push(Word(word.into(), context, offset, false)),
        Event::Call { offset } => {
            if let Some(word) = result.iter_mut().rev().find(|word| word.2 == offset) {
                word.3 = true;
            }
        }
    };
    for chunk in chunks {
        analyzer.feed(chunk, &mut record);
    }
    analyzer.flush(record);
    result
}

fn context(text: &str) -> Context {
    let mut analyzer = Analyzer::default();
    analyzer.feed(text, |_| {});
    analyzer.query(text.chars().count(), true, |_| {})
}

#[test]
fn lexical_state_and_context_are_independent_of_utf8_chunk_boundaries() {
    let source = "first = Socket()\nfirst.sendPacket(data) /* outer /* nested */ tail */\n# prose fakeCall()\nlet theme = \"monokai\";\nvalue = '''multi\nquotedCall() text'''\n'long_label: loop { client.結果(引数); }\n#include <thing>\ncodeWord()";
    let expected = words(&[source]);
    for split in (0..=source.len()).filter(|&split| source.is_char_boundary(split)) {
        assert_eq!(
            words(&[&source[..split], &source[split..]]),
            expected,
            "split {split}"
        );
    }
    let chars: Vec<_> = source
        .char_indices()
        .map(|(at, ch)| &source[at..at + ch.len_utf8()])
        .collect();
    assert_eq!(words(&chars), expected);
    assert!(
        expected
            .iter()
            .any(|word| word.0 == "codeWord" && word.1.domain == Domain::Code && word.3)
    );
    assert!(
        expected
            .iter()
            .any(|word| word.0 == "quotedCall" && word.1.domain == Domain::String && !word.3)
    );
    assert!(
        expected
            .iter()
            .any(|word| word.0 == "fakeCall" && word.1.domain == Domain::Comment && !word.3)
    );
}

#[test]
fn multiline_receivers_and_arguments_preserve_structural_identity() {
    let direct = context("client.transport.");
    for source in ["client.transport\n    .", "client.transport.\n    "] {
        assert_eq!(context(source).keys[..2], direct.keys[..2], "{source}");
    }
    assert_eq!(
        context("send(first, ").keys[2],
        context("send(\n first,\n ").keys[2]
    );
    assert_eq!(
        context("send(first, ").keys[2],
        context("send(select(a, b), ").keys[2]
    );
    assert_eq!(
        context("send(first, ").keys[2],
        context("send([a, b], ").keys[2]
    );
    assert_ne!(context("send(").keys[2], context("send(first, ").keys[2]);
}

#[test]
fn assignment_scope_and_aliases_do_not_leak_into_new_statements() {
    assert_eq!(
        context("enabled =\n    ").keys[4],
        hash_parts(5, &["enabled"])
    );
    assert_eq!(context("fn run() {\n theme = \"dark\"\n next ").keys[4], 0);
    let source = "first = Socket()\nfirst.sendPacket()\nsecond = Socket()\nsecond.";
    let query = context(source);
    let learned = words(&[source]);
    let member = learned.iter().find(|word| word.0 == "sendPacket").unwrap();
    assert_eq!(query.keys[1], member.1.keys[1]);
    let source = "fn one() { reader = Factory(); reader.readPacket(); }\nfn two() { reader = Different(); reader.";
    assert_ne!(context(source).keys[1], hash_parts(2, &["@Factory"]));
}

#[test]
fn quotes_and_control_keywords_cannot_fabricate_call_evidence() {
    let learned =
        words(&["if (ready) {} while (ready) {} actualCall(); \"actualCall()\" // actualCall()"]);
    assert!(
        !learned
            .iter()
            .any(|word| matches!(word.0.as_str(), "if" | "while") && word.3)
    );
    assert_eq!(
        learned
            .iter()
            .filter(|word| word.0 == "actualCall" && word.3)
            .count(),
        1
    );
}

#[test]
fn pathological_input_keeps_stacks_tokens_and_aliases_bounded() {
    let source = format!("send({}, payload)", "(".repeat(1000) + &")".repeat(1000));
    let analyzer_context = context(&source);
    assert_eq!(analyzer_context.keys[2], 0);
    assert!(
        words(&[&"識".repeat(1000), " validIdentifier"])
            .iter()
            .all(|word| word.0.len() <= TOKEN_BYTES)
    );
    let mut analyzer = Analyzer::default();
    for i in 0..1000 {
        analyzer.feed(&format!("value{i} = Factory();\n"), |_| {});
    }
    analyzer.flush(|_| {});
    assert!(analyzer.tracker.aliases.len() <= ALIASES);
    assert!(analyzer.tracker.frames.len() <= STACK_DEPTH);
    assert!(analyzer.pending.len() <= TOKEN_BYTES + 4);
}
