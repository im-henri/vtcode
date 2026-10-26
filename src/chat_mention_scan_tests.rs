use super::*;
#[derive(Debug, PartialEq, Eq)]
struct Mention {
    range: Range<usize>,
    path: String,
    closed: bool,
    path_start: usize,
    path_end: usize,
}
fn reference_mentions(text: &str) -> Vec<Mention> {
    let mut found = Vec::new();
    let mut pos = 0;
    let mut code_ticks = 0;
    while pos < text.len() {
        let ch = text[pos..].chars().next().unwrap();
        if ch == '`' {
            let count = text[pos..].bytes().take_while(|&b| b == b'`').count();
            if code_ticks == 0 {
                code_ticks = count;
            } else if count == code_ticks {
                code_ticks = 0;
            }
            pos += count;
            continue;
        }
        let boundary = pos == 0
            || text[..pos]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_whitespace() || matches!(c, '(' | '[' | '{'));
        if ch != '@' || !boundary || code_ticks != 0 {
            pos += ch.len_utf8();
            continue;
        }
        let start = pos;
        pos += 1;
        let quote = text[pos..]
            .chars()
            .next()
            .filter(|c| matches!(c, '"' | '\''));
        if quote.is_some() {
            pos += 1;
        }
        let path_start = pos;
        while pos < text.len() {
            let c = text[pos..].chars().next().unwrap();
            if quote == Some(c)
                || (quote.is_none() && (c.is_whitespace() || matches!(c, ')' | ']' | '}' | ',')))
                || c == '\n'
            {
                break;
            }
            pos += c.len_utf8();
        }
        let path_end = pos;
        let closed = quote.is_none() || text[pos..].starts_with(quote.unwrap());
        if quote.is_some() && closed {
            pos += 1;
        }
        found.push(Mention {
            range: start..pos,
            path: text[path_start..path_end].into(),
            closed,
            path_start,
            path_end,
        });
    }
    found
}
fn reference_current(text: &str, cursor: usize) -> Option<Mention> {
    if !text.is_char_boundary(cursor) {
        return None;
    }
    reference_mentions(text).into_iter().find_map(|mut m| {
        if cursor <= m.range.start || cursor > m.range.end {
            return None;
        }
        m.path = text[m.path_start.min(cursor)..m.path_end.min(cursor)].into();
        Some(m)
    })
}

fn current(text: &str, cursor: usize) -> Option<Mention> {
    if !text.is_char_boundary(cursor) {
        return None;
    }
    Scanner::new(text, cursor)
        .find(|span| cursor <= span.range.end)
        .map(|span| Mention {
            path: text[span.path.start.min(cursor)..span.path.end.min(cursor)].into(),
            path_start: span.path.start,
            path_end: span.path.end,
            closed: span.closed,
            range: span.range,
        })
}

#[test]
fn borrowed_scanner_preserves_quotes_fences_boundaries_and_every_caret() {
    let mut inputs = vec![
        String::new(),
        "@".into(),
        "hello @src/main.rs and @\"with spaces/é.rs\"".into(),
        "```\n@hidden.rs\n``` @visible.rs".into(),
        "mail@example.com \\@escaped \u{2003}@界.rs".into(),
        "`one @no` @'open path\n@next".into(),
        "\"@not_a_boundary\" (@yes), [@also]".into(),
    ];
    let alphabet = [
        "\t", "\u{b}", "\u{c}", "\u{a0}", "\0", "@", "`", "'", "\"", " ", "\n", "(", "]", ",",
        "\\", "/", ".", "a", "β", "\u{85}", "\u{2003}", "\r",
    ];
    let mut seed = 0x9e37_79b9_u64;
    for _ in 0..2000 {
        let mut text = String::new();
        for _ in 0..64 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            text.push_str(alphabet[(seed >> 32) as usize % alphabet.len()]);
        }
        inputs.push(text);
    }
    for text in inputs {
        let original = reference_mentions(&text);
        let actual: Vec<_> = Scanner::new(&text, text.len()).collect();
        assert_eq!(actual.len(), original.len(), "{text:?}");
        for (span, old) in actual.iter().zip(&original) {
            assert_eq!(
                (&span.range, span.path.clone(), span.closed),
                (&old.range, old.path_start..old.path_end, old.closed),
                "{text:?}"
            );
        }
        assert_eq!(has_mentions(&text), !original.is_empty());
        for caret in 0..=text.len() + 1 {
            assert_eq!(
                current(&text, caret),
                reference_current(&text, caret),
                "{text:?} at {caret}"
            );
        }
    }
}
