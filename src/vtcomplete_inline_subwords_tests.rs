use super::*;

fn split(text: &str) -> Vec<&str> {
    pieces(text).into_iter().map(|(start, end)| &text[start..end]).collect()
}

#[test]
fn text_is_cut_into_words_digits_signs_and_line_starts() {
    assert_eq!(
        split("let myVar_2 = foo(x);\n    return 10"),
        ["let", " myVar_", "2", " =", " foo", "(", "x", ");", "\n    ", "return", " 1", "0"]
    );
    // Runs of spaces keep their last space for the next piece; trailing
    // spaces stay alone; blank lines are line ends of their own.
    assert_eq!(split("a   b  \n\n  c"), ["a", "  ", " b", "  ", "\n", "\n  ", "c"]);
    // Any text at all, every byte covered once.
    let text = "naïve // ünïcödé 日本 ☃\t\tx";
    assert_eq!(split(text).concat(), text);
}

#[test]
fn learned_merges_spell_frequent_pieces_as_one_token_and_anything_else_by_bytes() {
    let mut counts: FxHashMap<Box<[u8]>, u64> = FxHashMap::default();
    for (piece, times) in [(" count", 50u64), (" counter", 20), ("count", 30), (" total", 10)] {
        counts.insert(piece.as_bytes().into(), times);
    }
    let merges = learn(&counts, 40);
    let subwords = Subwords::new(merges).unwrap();
    let (mut tokens, mut starts) = (Vec::new(), Vec::new());
    let mut cache = Cache::default();
    subwords.encode(" count counter Zq9", &mut cache, &mut tokens, &mut starts);
    let spelled: Vec<&[u8]> = tokens.iter().map(|&token| subwords.bytes(token)).collect();
    assert_eq!(spelled[0], b" count");
    // A new word is still written, from smaller tokens.
    assert_eq!(spelled.concat(), b" count counter Zq9");
    assert_eq!(starts[0], 0);
    assert_eq!(starts[1], 6);
    // The merges survive a model file.
    let stored = subwords.encode_merges();
    let mut bytes = &stored[..];
    let again = Subwords::decode_merges(&mut bytes).unwrap();
    assert!(bytes.is_empty());
    let mut other = Vec::new();
    again.encode(" count counter Zq9", &mut Cache::default(), &mut other, &mut Vec::new());
    assert_eq!(other, tokens);
}
