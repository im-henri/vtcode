//! Conservative numeric continuations from literal lists and adjacent rows.
//! Values use checked fixed-point arithmetic; no floating-point extrapolation,
//! project index, retained source, or global popularity participates.

const ROWS: usize = 16;
const LINE_BYTES: usize = 512;
const TOKENS: usize = 96;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Format {
    radix: u32,
    decimals: usize,
    padding: usize,
    uppercase: bool,
    uppercase_prefix: bool,
    group: usize,
}

#[derive(Clone, Copy)]
struct Number {
    value: i128,
    format: Format,
}

impl Number {
    fn parse(text: &str) -> Option<Self> {
        if text.len() > 48 {
            return None;
        }
        let (negative, text) = text.strip_prefix('-').map_or((false, text), |x| (true, x));
        let (radix, uppercase, text) = if let Some(text) = text.strip_prefix("0x") {
            (16, false, text)
        } else if let Some(text) = text.strip_prefix("0X") {
            (16, true, text)
        } else {
            (10, false, text)
        };
        let (whole, fraction) = text.split_once('.').map_or((text, ""), |x| x);
        if whole.is_empty()
            || fraction.len() > 9
            || radix != 10 && !fraction.is_empty()
            || text.ends_with('.')
            || !fraction.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let groups = whole.split('_').collect::<Vec<_>>();
        let group = if groups.len() > 1 {
            let size = groups[1].len();
            if size == 0
                || size > 4
                || groups[0].is_empty()
                || groups[0].len() > size
                || groups[1..].iter().any(|part| part.len() != size)
            {
                return None;
            }
            size
        } else {
            0
        };
        let whole = whole.replace('_', "");
        if !whole.chars().all(|ch| ch.is_digit(radix)) {
            return None;
        }
        // Mixed hexadecimal letter case is deliberately unsupported.
        let has_upper = whole.bytes().any(|b| b.is_ascii_uppercase());
        let has_lower = whole.bytes().any(|b| b.is_ascii_lowercase());
        if has_upper && has_lower {
            return None;
        }
        let uppercase_prefix = uppercase;
        let uppercase = uppercase || has_upper;
        let mut value = i128::from_str_radix(&whole, radix).ok()?;
        for byte in fraction.bytes() {
            value = value
                .checked_mul(10)?
                .checked_add(i128::from(byte - b'0'))?;
        }
        if negative {
            value = value.checked_neg()?;
        }
        Some(Self {
            value,
            format: Format {
                radix,
                decimals: fraction.len(),
                padding: if whole.len() > 1 && whole.starts_with('0') {
                    whole.len()
                } else {
                    0
                },
                uppercase,
                uppercase_prefix,
                group,
            },
        })
    }

    fn render(self, value: i128) -> Option<String> {
        let absolute = value.checked_abs()?;
        let scale = 10i128.checked_pow(self.format.decimals as u32)?;
        let whole = absolute / scale;
        let mut digits = if self.format.radix == 16 {
            if self.format.uppercase {
                format!("{whole:X}")
            } else {
                format!("{whole:x}")
            }
        } else {
            whole.to_string()
        };
        if digits.len() < self.format.padding {
            digits = format!("{:0>width$}", digits, width = self.format.padding);
        }
        if self.format.group > 0 {
            let mut grouped = String::new();
            for (index, ch) in digits.chars().enumerate() {
                if index > 0 && (digits.len() - index).is_multiple_of(self.format.group) {
                    grouped.push('_');
                }
                grouped.push(ch);
            }
            digits = grouped;
        }
        if self.format.decimals > 0 {
            digits.push('.');
            digits.push_str(&format!(
                "{:0width$}",
                absolute % scale,
                width = self.format.decimals
            ));
        }
        if self.format.radix == 16 {
            digits.insert_str(
                0,
                if self.format.uppercase_prefix {
                    "0X"
                } else {
                    "0x"
                },
            );
        }
        if value < 0 {
            digits.insert(0, '-');
        }
        (digits.len() <= 48).then_some(digits)
    }
}

struct Token<'a> {
    text: &'a str,
    number: Option<Number>,
}
struct Row<'a> {
    indent: &'a str,
    tokens: Vec<Token<'a>>,
}

fn lex(text: &str) -> Option<Row<'_>> {
    if text.len() > LINE_BYTES {
        return None;
    }
    let indent = &text[..text.len() - text.trim_start().len()];
    let mut tokens = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((start, ch)) = chars.next() {
        if ch.is_whitespace() {
            continue;
        }
        if tokens.len() == TOKENS || matches!(ch, '#' | '\'' | '"' | '`' | '{' | '}') {
            return None;
        }
        let mut end = start + ch.len_utf8();
        let negative = ch == '-' && chars.peek().is_some_and(|(_, ch)| ch.is_ascii_digit());
        let numeric = ch.is_ascii_digit() || negative;
        if numeric {
            while let Some(&(at, ch)) = chars.peek() {
                if !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '.') {
                    break;
                }
                chars.next();
                end = at + ch.len_utf8();
            }
        } else if ch.is_alphabetic() || matches!(ch, '_' | '$') {
            while let Some(&(at, ch)) = chars.peek() {
                if !(ch.is_alphanumeric() || matches!(ch, '_' | '$')) {
                    break;
                }
                chars.next();
                end = at + ch.len_utf8();
            }
        } else if ch == '/' && chars.peek().is_some_and(|(_, ch)| matches!(ch, '/' | '*')) {
            return None;
        }
        let text = &text[start..end];
        let number = if numeric {
            Some(Number::parse(text)?)
        } else {
            None
        };
        tokens.push(Token { text, number });
    }
    Some(Row { indent, tokens })
}

pub(super) struct Prediction {
    pub(super) word: String,
    pub(super) sequence: bool,
}

fn arithmetic_next(values: &[i128]) -> Option<i128> {
    let step = values.get(1)?.checked_sub(values[0])?;
    if !values
        .windows(2)
        .all(|pair| pair[1].checked_sub(pair[0]) == Some(step))
    {
        return None;
    }
    values.last()?.checked_add(step)
}

/// `None` leaves other row evidence eligible. `Some(None)` means an inline
/// numeric list was found but cannot justify a continuation at this caret.
fn inline_list(row: &Row<'_>, suffix: &Row<'_>, prefix: &str) -> Option<Option<Prediction>> {
    let mut open = Vec::new();
    for (index, token) in row.tokens.iter().enumerate() {
        match token.text {
            "[" => open.push((index, "]")),
            "(" => open.push((index, ")")),
            "]" | ")" => {
                if open
                    .pop()
                    .is_none_or(|(_, expected)| expected != token.text)
                {
                    return None;
                }
            }
            _ => {}
        }
    }
    let &(start, expected) = open.last()?;
    if expected != "]" {
        return None;
    }
    // A name, member, or completed expression before `[` denotes indexing.
    // Literal expression sites include arguments, assignments and nested lists.
    if start > 0
        && !matches!(
            row.tokens[start - 1].text,
            "=" | ":" | "(" | "," | "[" | "!"
        )
    {
        return None;
    }
    let members = &row.tokens[start + 1..];
    if members.len() < 4 {
        return None;
    }
    if !members.len().is_multiple_of(2)
        || !suffix.tokens.first().is_none_or(|token| token.text == "]")
    {
        return Some(None);
    }
    let mut values = Vec::new();
    let Some(first) = members[0].number else {
        return Some(None);
    };
    for pair in members.chunks_exact(2) {
        let Some(number) = pair[0].number else {
            return Some(None);
        };
        if pair[1].text != "," || number.format != first.format {
            return Some(None);
        }
        values.push(number.value);
    }
    let Some(value) = arithmetic_next(&values) else {
        return Some(None);
    };
    let Some(word) = first.render(value) else {
        return Some(None);
    };
    if !word.starts_with(prefix) || word == prefix {
        return Some(None);
    }
    Some(Some(Prediction {
        word,
        sequence: values[0] != values[1],
    }))
}

/// Two ordered witnesses establish an arithmetic continuation, including a
/// repeated constant. Additional same-role rows must agree. A row-specific
/// affine relationship needs three distinct inputs and takes precedence over
/// generic next-row extrapolation when the current input changes the answer.
pub(super) fn predict(
    before: &str,
    after: &str,
    prefix: &str,
    cancelled: &impl Fn() -> bool,
) -> Option<Option<Prediction>> {
    if cancelled() {
        return None;
    }
    if !prefix.is_empty() && !prefix.as_bytes()[0].is_ascii_digit() {
        return Some(None);
    }
    let mut lines = before.rsplit('\n');
    let current = match lex(lines.next().unwrap_or_default()) {
        Some(row) => row,
        None => return Some(None),
    };
    let target = current.tokens.len();
    if target == 0 || !matches!(current.tokens[target - 1].text, "=" | ":" | "(" | "," | "[") {
        return Some(None);
    }
    let Some(suffix) = lex(after) else {
        return Some(None);
    };
    if let Some(result) = inline_list(&current, &suffix, prefix) {
        return Some(result);
    }
    let mut witnesses = Vec::new();
    for text in lines.take(ROWS) {
        if cancelled() {
            return None;
        }
        let Some(row) = lex(text) else { break };
        if row.tokens.is_empty() {
            break;
        }
        if row.indent != current.indent || row.tokens.len() <= target {
            break;
        }
        let same_role = row.tokens[..target]
            .iter()
            .zip(&current.tokens)
            .all(|(old, new)| old.text == new.text || old.number.is_some() && new.number.is_some());
        if !same_role || row.tokens[target].number.is_none() {
            break;
        }
        if suffix.tokens.len() > row.tokens.len() - target - 1
            || !suffix
                .tokens
                .iter()
                .zip(&row.tokens[target + 1..])
                .all(|(a, b)| a.text == b.text)
        {
            return Some(None);
        }
        if let Some(previous) = witnesses.last() {
            let previous: &Row<'_> = previous;
            if row.tokens.len() != previous.tokens.len()
                || !row.tokens[target + 1..]
                    .iter()
                    .zip(&previous.tokens[target + 1..])
                    .all(|(a, b)| a.text == b.text)
            {
                return Some(None);
            }
        }
        witnesses.push(row);
    }
    if witnesses.len() < 2 {
        return Some(None);
    }
    witnesses.reverse();
    let number = witnesses[0].tokens[target].number.unwrap();
    if witnesses
        .iter()
        .any(|row| row.tokens[target].number.unwrap().format != number.format)
    {
        return Some(None);
    }
    let values = witnesses
        .iter()
        .map(|row| row.tokens[target].number.unwrap().value)
        .collect::<Vec<_>>();
    let constant = values.iter().all(|value| *value == values[0]);
    let mut next_row = arithmetic_next(&values);
    let mut relations = Vec::new();
    if !constant {
        // A numeric column at the left can pin the target to a row-specific
        // value. Verify an integer affine relation on three distinct inputs.
        for (index, token) in current.tokens.iter().enumerate() {
            let Some(input) = token.number else { continue };
            if witnesses.iter().any(|row| {
                row.tokens[index]
                    .number
                    .is_none_or(|number| number.format != input.format)
            }) {
                next_row = None;
                continue;
            }
            let inputs = witnesses
                .iter()
                .map(|row| row.tokens[index].number.map(|number| number.value))
                .collect::<Option<Vec<_>>>();
            let Some(inputs) = inputs else { continue };
            // Generic next-row prediction assumes the row's changing inputs
            // also continue their observed progression. It cannot silently
            // use the fourth row's output for an explicitly requested eighth.
            if inputs.iter().any(|value| *value != inputs[0])
                && arithmetic_next(&inputs) != Some(input.value)
            {
                next_row = None;
            }
            let Some(other) = inputs.iter().position(|value| *value != inputs[0]) else {
                continue;
            };
            let Some(dx) = inputs[other].checked_sub(inputs[0]) else {
                continue;
            };
            let Some(dy) = values[other].checked_sub(values[0]) else {
                continue;
            };
            if dy.checked_rem(dx) != Some(0) {
                continue;
            }
            let Some(slope) = dy.checked_div(dx) else {
                continue;
            };
            let Some(offset) = slope
                .checked_mul(inputs[0])
                .and_then(|value| values[0].checked_sub(value))
            else {
                continue;
            };
            if inputs
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                < 3
                || !inputs.iter().zip(&values).all(|(x, y)| {
                    slope
                        .checked_mul(*x)
                        .and_then(|value| value.checked_add(offset))
                        == Some(*y)
                })
            {
                continue;
            }
            let Some(value) = slope
                .checked_mul(input.value)
                .and_then(|value| value.checked_add(offset))
            else {
                // A verified relationship that overflows must not fall back
                // to an unrelated, representable temporal extrapolation.
                return Some(None);
            };
            relations.push(value);
        }
    }
    let value = if let Some(&value) = relations.first() {
        if relations.iter().any(|candidate| *candidate != value) {
            return Some(None);
        }
        value
    } else if let Some(value) = next_row {
        value
    } else {
        return Some(None);
    };
    let Some(word) = number.render(value) else {
        return Some(None);
    };
    if !word.starts_with(prefix) || word == prefix {
        return Some(None);
    }
    Some(Some(Prediction {
        word,
        sequence: !constant,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, prefix: &str) -> Option<String> {
        predict(text, "", prefix, &|| false)
            .unwrap()
            .map(|row| row.word)
    }

    #[test]
    fn predicts_one_digit_and_formatted_fixed_point_sequences() {
        for (text, prefix, expected) in [
            ("wait(1)\nwait(2)\nwait(", "", "3"),
            ("wait(3)\nwait(2)\nwait(", "", "1"),
            ("wait(1)\nwait(0)\nwait(", "", "-1"),
            ("wait(-3)\nwait(-2)\nwait(", "", "-1"),
            ("timeout = 100\ntimeout = 200\ntimeout = ", "3", "300"),
            ("id = 009\nid = 010\nid = ", "", "011"),
            ("mask = 0x09\nmask = 0x0a\nmask = ", "0x", "0x0b"),
            ("opacity = 0.10\nopacity = 0.20\nopacity = ", "", "0.30"),
            ("set(1, 10)\nset(2, 20)\nset(3, ", "", "30"),
            ("wait(1)\nwait(2)\nwait(3)\nwait(", "", "4"),
            (
                "timeout = 100\ntimeout = 200\ntimeout = 300\ntimeout = ",
                "4",
                "400",
            ),
            ("id = 007\nid = 008\nid = 009\nid = ", "", "010"),
            (
                "mask = 0x10\nmask = 0x20\nmask = 0x30\nmask = ",
                "0x",
                "0x40",
            ),
            ("mask = 0X0A\nmask = 0X0B\nmask = 0X0C\nmask = ", "", "0X0D"),
            ("mask = 0x0A\nmask = 0x0B\nmask = 0x0C\nmask = ", "", "0x0D"),
            (
                "delay = 1_000\ndelay = 2_000\ndelay = 3_000\ndelay = ",
                "",
                "4_000",
            ),
            (
                "opacity = 0.25\nopacity = 0.50\nopacity = 0.75\nopacity = ",
                "",
                "1.00",
            ),
            (
                "offset = -10\noffset = -20\noffset = -30\noffset = ",
                "",
                "-40",
            ),
            ("retry(5)\nretry(5)\nretry(", "", "5"),
            ("set(1, 10)\nset(2, 20)\nset(3, 30)\nset(4, ", "", "40"),
            ("set(1, 10)\nset(2, 20)\nset(3, 30)\nset(8, ", "", "80"),
            ("set(1, 10)\nset(3, 30)\nset(8, 80)\nset(9, ", "", "90"),
        ] {
            assert_eq!(word(text, prefix).as_deref(), Some(expected), "{text}");
        }
    }

    #[test]
    fn predicts_inline_literal_lists_without_confusing_indexes_or_arguments() {
        for (text, prefix, expected) in [
            ("values = [10, 20, ", "", "30"),
            ("values = [0.25, 0.50, ", "", "0.75"),
            ("values = [9, 6, ", "", "3"),
            ("values = [009, 010, ", "", "011"),
            ("values = [0x10, 0x20, ", "0x", "0x30"),
            ("values = [-10, -20, ", "", "-30"),
            ("[1, 2, ", "", "3"),
            ("draw([1, 2, ", "", "3"),
            ("draw(other, [1, 2, ", "", "3"),
            ("values = [[1, 2, ", "", "3"),
            ("values = vec![1, 2, ", "", "3"),
            ("values = [10, 20, ", "3", "30"),
            ("values = [5, 5, ", "", "5"),
        ] {
            assert_eq!(word(text, prefix).as_deref(), Some(expected), "{text}");
            assert_eq!(
                predict(text, "]", prefix, &|| false).unwrap().unwrap().word,
                expected
            );
        }
        for text in [
            "values = [10, ",
            "values = [10, 20, 45, ",
            "values = [0.25, 0.50, 0.80, ",
            "values = [01, 2, ",
            "values = [first, 10, 20, ",
            "values = [10, 20 + 1, ",
            "values = [call(10), 20, ",
            "draw(10, 20, ",
            "obj[10, 20, ",
            "obj.values[10, 20, ",
            "(obj)[10, 20, ",
            "values = [170141183460469231731687303715884105726, 170141183460469231731687303715884105727, ",
        ] {
            assert!(word(text, "").is_none(), "{text}");
        }
        assert!(
            predict("values = [10, 20, ", ", 50]", "", &|| false)
                .unwrap()
                .is_none()
        );
        assert!(
            predict("values = [10, 20, ", "}", "", &|| false)
                .unwrap()
                .is_none()
        );
        assert!(word("values = [10, 20, ", "9").is_none());
        // A single member still permits independently verified across-row
        // evidence for the same value position.
        assert_eq!(
            word("values = [1]\nvalues = [2]\nvalues = [", "").as_deref(),
            Some("3")
        );
    }

    #[test]
    fn refuses_ambiguous_unsupported_or_inconsistent_evidence() {
        for text in [
            "wait(1)\nwait(",
            "wait(1)\nwait(2)\nwait(4)\nwait(",
            "wait(1)\nwait(2)\nwait(2)\nwait(",
            "opacity = 0.10\nopacity = 0.20\nopacity = 0.45\nopacity = ",
            "id = 001\nid = 002\nid = 004\nid = ",
            "wait(1)\nother(2)\nwait(3)\nwait(",
            "wait(1, left)\nwait(2, right)\nwait(3, fixed)\nwait(",
            "first = 1\nsecond = 2\nthird = 3\nfourth = ",
            "wait(1)\nwait(2)\nwait(3)\nother(",
            "wait(1)\nwait(2)\nwait(3)\nwait(value, ",
            "  wait(1)\n  wait(2)\n  wait(3)\nwait(",
            "wait(1)\nwait(2)\n}\nwait(3)\nwait(",
            "version = 1.2.1\nversion = 1.2.2\nversion = 1.2.3\nversion = ",
            "duration = 10ms\nduration = 20ms\nduration = 30ms\nduration = ",
            "wait(01)\nwait(2)\nwait(03)\nwait(",
            "wait(1)\n// wait(2)\nwait(3)\nwait(",
            "wait(1)\nwait(2)\nwait(3)\n\"wait(",
            "wait(170141183460469231731687303715884105725)\nwait(170141183460469231731687303715884105726)\nwait(170141183460469231731687303715884105727)\nwait(",
            "wait(170141183460469231731687303715884105726)\nwait(170141183460469231731687303715884105727)\nwait(",
            // Two points can continue a row, but cannot verify a mapping for
            // an explicitly different input. Three independent inputs can.
            "set(1, 10)\nset(2, 20)\nset(8, ",
            "set(1, 10)\nset(2, 20)\nset(3.0, ",
            // Two verified dependencies disagree at the query.
            "set(1, 1, 10)\nset(2, 2, 20)\nset(3, 3, 30)\nset(4, 8, ",
        ] {
            assert!(word(text, "").is_none(), "{text}");
        }
        assert!(word("wait(1)\nwait(2)\nwait(3)\nwait(", "9").is_none());
    }

    #[test]
    fn checks_suffix_budget_and_cancellation() {
        let before = "wait(1, fixed)\nwait(2, fixed)\nwait(3, fixed)\nwait(";
        assert_eq!(
            predict(before, ", fixed)", "", &|| false)
                .unwrap()
                .unwrap()
                .word,
            "4"
        );
        assert!(
            predict(before, ", wrong)", "", &|| false)
                .unwrap()
                .is_none()
        );
        assert!(predict(before, "", "", &|| true).is_none());
        assert!(word(&format!("{}\nwait(", "x".repeat(LINE_BYTES + 1)), "").is_none());
    }

    #[test]
    fn random_checked_progressions_and_arbitrary_unicode_stay_valid() {
        let mut random = 0x6175_4c23_e08b_1a49u64;
        let mut next = || {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            random
        };
        for _ in 0..500 {
            let first = (next() % 20_000) as i128 - 10_000;
            let step = (next() % 99 + 1) as i128 * if next() & 1 == 0 { 1 } else { -1 };
            let text = format!(
                "value = {first}\nvalue = {}\nvalue = {}\nvalue = ",
                first + step,
                first + step * 2
            );
            assert_eq!(word(&text, ""), Some((first + step * 3).to_string()));
            let pair = format!("value = {first}\nvalue = {}\nvalue = ", first + step);
            assert_eq!(word(&pair, ""), Some((first + step * 2).to_string()));
        }
        let alphabet = [
            '界', 'é', '1', '0', '_', '.', '-', 'x', '\n', '\t', '(', ')', ',', '=', '#', '\'', '"',
        ];
        for _ in 0..500 {
            let text = (0..256)
                .map(|_| alphabet[next() as usize % alphabet.len()])
                .collect::<String>();
            if let Some(prediction) = predict(&text, "", "", &|| false).unwrap() {
                assert!(prediction.word.len() <= 48);
                assert!(Number::parse(&prediction.word).is_some());
            }
        }
    }
}
