//! Infer small string programs between related positions in analogous rows.
//! Programs and witnesses borrow the query. Nothing is retained or indexed.
use super::{
    Budget, Kind, Mapping, Predictions, Row, TokenKind, WORD_BYTES, compatible, visible, word_char,
};

const PARTS: usize = 16;

#[derive(Clone, Copy, Default)]
struct Span {
    start: usize,
    end: usize,
}
struct Components {
    spans: [Span; PARTS],
    len: usize,
}
impl Components {
    fn new(word: &str) -> Option<Self> {
        let mut result = Self {
            spans: [Span::default(); PARTS],
            len: 0,
        };
        let mut start = None;
        let mut previous = None;
        let mut chars = word.char_indices().peekable();
        while let Some((at, ch)) = chars.next() {
            let separator = matches!(ch, '_' | '$');
            let boundary = previous.is_some_and(|before: char| {
                before.is_lowercase() && ch.is_uppercase()
                    || before.is_ascii_digit() != ch.is_ascii_digit()
                    || before.is_uppercase()
                        && ch.is_uppercase()
                        && chars.peek().is_some_and(|(_, next)| next.is_lowercase())
            });
            if separator || boundary {
                if let Some(begin) = start.take() {
                    if result.len == PARTS {
                        return None;
                    }
                    result.spans[result.len] = Span {
                        start: begin,
                        end: at,
                    };
                    result.len += 1;
                }
            }
            if !separator && start.is_none() {
                start = Some(at);
            }
            previous = (!separator).then_some(ch);
        }
        if let Some(start) = start {
            if result.len == PARTS {
                return None;
            }
            result.spans[result.len] = Span {
                start,
                end: word.len(),
            };
            result.len += 1;
        }
        (result.len > 0).then_some(result)
    }
    fn part<'a>(&self, word: &'a str, index: usize) -> &'a str {
        let span = self.spans[index];
        &word[span.start..span.end]
    }
}

#[derive(Clone, Copy)]
enum Style {
    Copy,
    Lower,
    Upper,
    UpperFirst,
    LowerFirst,
    Snake,
    UpperSnake,
    Camel,
    Pascal,
    ReverseSnake,
}
const STYLES: [Style; 10] = [
    Style::Copy,
    Style::Lower,
    Style::Upper,
    Style::UpperFirst,
    Style::LowerFirst,
    Style::Snake,
    Style::UpperSnake,
    Style::Camel,
    Style::Pascal,
    Style::ReverseSnake,
];
impl Style {
    fn render(self, value: &str, budget: &mut Budget<'_>) -> Option<String> {
        if !budget.spend(value.len() + 1) {
            return None;
        }
        let word = match self {
            Self::Copy => value.to_owned(),
            Self::Lower => value.to_lowercase(),
            Self::Upper => value.to_uppercase(),
            Self::UpperFirst | Self::LowerFirst => {
                let mut chars = value.chars();
                let first = chars.next()?;
                let mut out = if matches!(self, Self::UpperFirst) {
                    first.to_uppercase().collect::<String>()
                } else {
                    first.to_lowercase().collect::<String>()
                };
                out.push_str(chars.as_str());
                out
            }
            _ => {
                let parts = Components::new(value)?;
                let mut out = String::with_capacity(value.len());
                for position in 0..parts.len {
                    let index = if matches!(self, Self::ReverseSnake) {
                        parts.len - position - 1
                    } else {
                        position
                    };
                    let part = parts.part(value, index);
                    if position > 0
                        && matches!(self, Self::Snake | Self::UpperSnake | Self::ReverseSnake)
                    {
                        out.push('_');
                    }
                    if matches!(self, Self::UpperSnake) {
                        out.extend(part.chars().flat_map(char::to_uppercase));
                    } else if matches!(self, Self::Pascal)
                        || matches!(self, Self::Camel) && position > 0
                    {
                        let mut chars = part.chars();
                        out.extend(chars.next()?.to_uppercase());
                        out.extend(chars.flat_map(char::to_lowercase));
                    } else {
                        out.extend(part.chars().flat_map(char::to_lowercase));
                    }
                    if out.len() > WORD_BYTES {
                        return None;
                    }
                }
                out
            }
        };
        (word.len() <= WORD_BYTES && word.chars().all(word_char)).then_some(word)
    }
}

struct Program<'a> {
    before: &'a str,
    after: &'a str,
    output_before: &'a str,
    output_after: &'a str,
    style: Style,
}
impl Program<'_> {
    fn apply(&self, input: &str, budget: &mut Budget<'_>) -> Option<String> {
        if !budget.spend(input.len() + 1) {
            return None;
        }
        let core = input.strip_prefix(self.before)?.strip_suffix(self.after)?;
        if core.is_empty() {
            return None;
        }
        let transformed = self.style.render(core, budget)?;
        let length = self.output_before.len() + transformed.len() + self.output_after.len();
        if length > WORD_BYTES {
            return None;
        }
        let mut result = String::with_capacity(length);
        result.push_str(self.output_before);
        result.push_str(&transformed);
        result.push_str(self.output_after);
        Some(result)
    }
}

/// All applicable programs consistent with every independent example must agree
/// at the query. Typed text is an additional constraint, never an unseen answer.
fn infer(
    examples: &[(&str, &str)],
    current: &str,
    prefix: &str,
    cases: bool,
    budget: &mut Budget<'_>,
) -> Option<String> {
    let (a, output) = examples[0];
    let (b, _) = *examples
        .iter()
        .find(|(input, target)| *input != a && *target != output)?;
    let a_parts = Components::new(a)?;
    let b_parts = Components::new(b)?;
    let mut common_start = 0;
    while common_start < a_parts.len.min(b_parts.len)
        && a_parts.part(a, common_start) == b_parts.part(b, common_start)
    {
        common_start += 1;
    }
    let mut common_end = 0;
    while common_end < a_parts.len.min(b_parts.len).saturating_sub(common_start)
        && a_parts.part(a, a_parts.len - common_end - 1)
            == b_parts.part(b, b_parts.len - common_end - 1)
    {
        common_end += 1;
    }
    let mut consensus = None;
    for drop_start in 0..=common_start {
        for drop_end in 0..=common_end {
            if drop_start + drop_end >= a_parts.len {
                continue;
            }
            let start = if drop_start == 0 {
                0
            } else {
                a_parts.spans[drop_start].start
            };
            let end = if drop_end == 0 {
                a.len()
            } else {
                a_parts.spans[a_parts.len - drop_end - 1].end
            };
            let before = &a[..start];
            let after = &a[end..];
            let Some(other_core) = b
                .strip_prefix(before)
                .and_then(|word| word.strip_suffix(after))
            else {
                continue;
            };
            if a[start..end] == *other_core {
                continue;
            }
            for style in STYLES
                .into_iter()
                .take(if cases { STYLES.len() } else { 1 })
            {
                let Some(core) = style
                    .render(&a[start..end], budget)
                    .filter(|word| !word.is_empty())
                else {
                    continue;
                };
                let mut search = 0;
                while search < output.len() {
                    if !budget.spend(output.len() - search + 1) {
                        return None;
                    }
                    let Some(relative) = output[search..].find(&core) else {
                        break;
                    };
                    let at = search + relative;
                    let program = Program {
                        before,
                        after,
                        output_before: &output[..at],
                        output_after: &output[at + core.len()..],
                        style,
                    };
                    search = at + output[at..].chars().next()?.len_utf8();
                    let Some(candidate) = program
                        .apply(current, budget)
                        .filter(|word| word.len() > prefix.len() && word.starts_with(prefix))
                    else {
                        continue;
                    };
                    let mut consistent = true;
                    for &(input, target) in examples {
                        if program.apply(input, budget).as_deref() != Some(target) {
                            consistent = false;
                            break;
                        }
                    }
                    if !consistent {
                        continue;
                    }
                    if consensus.as_ref().is_some_and(|word| word != &candidate) {
                        return None;
                    }
                    consensus = Some(candidate);
                }
                if budget.limited {
                    return None;
                }
            }
        }
    }
    consensus
}

fn fits_context(
    rows: &[&Row<'_>],
    left: &Row<'_>,
    right: &Row<'_>,
    at: usize,
    candidate: &str,
    budget: &mut Budget<'_>,
) -> bool {
    let reference = rows[0];
    let mut map = Mapping::new();
    if !map.bind(reference.tokens[at].text, candidate) {
        return false;
    }
    for (index, token) in visible(left, right, at) {
        if !budget.spend(map.pairs.len() + rows.len() + 1) {
            return false;
        }
        let Some(source) = reference.tokens.get(index) else {
            return false;
        };
        if source.kind != token.kind {
            return false;
        }
        if source.kind == TokenKind::Punctuation {
            if source.text != token.text {
                return false;
            }
        } else {
            if rows.iter().all(|row| row.tokens[index].text == source.text)
                && source.text != token.text
            {
                return false;
            }
            if !map.bind(source.text, token.text) {
                return false;
            }
        }
    }
    true
}

pub(super) fn learn(
    rows: &[Row<'_>],
    left: &Row<'_>,
    right: &Row<'_>,
    prefix: &str,
    cases: bool,
    out: &mut Predictions,
    budget: &mut Budget<'_>,
) {
    let at = left.tokens.len();
    let Some(reference) = rows.last().filter(|row| {
        row.tokens
            .get(at)
            .is_some_and(|token| token.kind == TokenKind::Word)
    }) else {
        return;
    };
    let mut family = Vec::new();
    for row in rows.iter().rev() {
        if compatible(reference, row, budget).is_some() {
            family.push(row);
        }
        if budget.limited {
            return;
        }
    }
    if family.len() < 2 {
        return;
    }
    // Reject fixed visible words before doing program enumeration. A changing
    // field must not erase the receiver/call/site shared by all examples.
    if !visible(left, right, at).all(|(index, token)| {
        reference.tokens.get(index).is_some_and(|source| {
            source.kind == token.kind
                && (source.kind != TokenKind::Punctuation || source.text == token.text)
                && (!family
                    .iter()
                    .all(|row| row.tokens[index].text == source.text)
                    || source.text == token.text)
        })
    }) {
        return;
    }
    let mut consensus: Option<(String, usize)> = None;
    for (column, current) in
        visible(left, right, at).filter(|(_, token)| token.kind == TokenKind::Word)
    {
        let Some(source) = reference
            .tokens
            .get(column)
            .filter(|token| token.kind == TokenKind::Word)
        else {
            continue;
        };
        if family
            .iter()
            .all(|row| row.tokens[column].text == source.text)
        {
            continue;
        }
        let mut examples = Vec::new();
        for row in &family {
            let pair = (row.tokens[column].text, row.tokens[at].text);
            if !examples.contains(&pair) {
                examples.push(pair);
            }
        }
        if examples.len() < 2 {
            continue;
        }
        if let Some(word) = infer(&examples, current.text, prefix, cases, budget) {
            if !fits_context(&family, left, right, at, &word, budget) {
                continue;
            }
            match &mut consensus {
                Some((previous, count)) if previous == &word => {
                    *count = (*count).max(examples.len())
                }
                Some(_) => return,
                None => consensus = Some((word, examples.len())),
            }
        }
        if budget.limited {
            return;
        }
    }
    if let Some((word, count)) = consensus {
        out.add(word, prefix, count.min(4) as f64, Kind::Relation);
    }
}

#[cfg(test)]
mod tests {
    use super::super::predict;

    fn prediction(
        pairs: &[(&str, &str)],
        current: &str,
        prefix: &str,
        right: &str,
    ) -> super::Predictions {
        let mut left = pairs
            .iter()
            .map(|(input, output)| format!("let {input} = {output}(payload);\n"))
            .collect::<String>();
        left.push_str(&format!("let {current} = "));
        predict(&left, right, prefix, true, &|| false).unwrap()
    }

    #[test]
    fn related_columns_construct_names_not_present_in_the_examples() {
        for (pairs, current, prefix, expected) in [
            (
                [("user_id", "decode_user_id"), ("task_id", "decode_task_id")],
                "cache_id",
                "decode",
                "decode_cache_id",
            ),
            (
                [("user_id", "user_id_cached"), ("task_id", "task_id_cached")],
                "cache_id",
                "cache",
                "cache_id_cached",
            ),
            (
                [
                    ("source_user", "target_user"),
                    ("source_task", "target_task"),
                ],
                "source_cache",
                "target",
                "target_cache",
            ),
            (
                [
                    ("user_id", "fetch_user_id_cached"),
                    ("task_id", "fetch_task_id_cached"),
                ],
                "cache_id",
                "fetch",
                "fetch_cache_id_cached",
            ),
            (
                [
                    ("staged_user_tmp", "finished_user_ready"),
                    ("staged_task_tmp", "finished_task_ready"),
                ],
                "staged_cache_tmp",
                "finished",
                "finished_cache_ready",
            ),
        ] {
            let result = prediction(&pairs, current, prefix, "");
            assert!(
                result.candidates().any(|word| word == expected),
                "missing {expected}"
            );
            assert!(!result.limited);
        }
    }

    #[test]
    fn case_separator_acronym_and_composed_programs_are_demonstrated() {
        for (pairs, current, prefix, expected) in [
            (
                [("user_id", "UserId"), ("task_name", "TaskName")],
                "cache_key",
                "Ca",
                "CacheKey",
            ),
            (
                [("user_id", "userId"), ("task_name", "taskName")],
                "cache_key",
                "ca",
                "cacheKey",
            ),
            (
                [("HTTPServer", "http_server"), ("JSONValue", "json_value")],
                "TCPClient",
                "tc",
                "tcp_client",
            ),
            (
                [("user_id", "USER_ID"), ("task_name", "TASK_NAME")],
                "cache_key",
                "CA",
                "CACHE_KEY",
            ),
            (
                [
                    ("source_user_id", "decodeUserIdValue"),
                    ("source_task_name", "decodeTaskNameValue"),
                ],
                "source_cache_key",
                "decode",
                "decodeCacheKeyValue",
            ),
            (
                [("user_id", "id_user"), ("task_name", "name_task")],
                "cache_key",
                "ke",
                "key_cache",
            ),
            (
                [
                    ("old_数据", "load_数据_cached"),
                    ("old_文本", "load_文本_cached"),
                ],
                "old_对象",
                "load",
                "load_对象_cached",
            ),
        ] {
            let result = prediction(&pairs, current, prefix, "");
            assert!(
                result.candidates().any(|word| word == expected),
                "missing {expected}"
            );
            assert!(!result.limited);
        }
    }

    #[test]
    fn right_context_and_reused_bindings_must_agree() {
        let pairs = [("user_id", "decode_user_id"), ("task_id", "decode_task_id")];
        assert!(
            prediction(&pairs, "cache_id", "decode", "(payload);")
                .candidates()
                .any(|word| word == "decode_cache_id")
        );
        assert!(!prediction(&pairs, "cache_id", "decode", "(unrelated);").active());
        let left = "let user_id = decode_user_id(user_id);\nlet task_id = decode_task_id(task_id);\nlet cache_id = ";
        assert!(
            !predict(left, "(other_id);", "decode", true, &|| false)
                .unwrap()
                .active()
        );
        assert!(
            predict(left, "(cache_id);", "decode", true, &|| false)
                .unwrap()
                .candidates()
                .any(|word| word == "decode_cache_id")
        );
        let reverse = "user_id = decode_user_id(payload);\ntask_id = decode_task_id(payload);\n";
        assert!(
            predict(
                reverse,
                " = decode_cache_id(payload);",
                "cache",
                true,
                &|| false
            )
            .unwrap()
            .candidates()
            .any(|word| word == "cache_id")
        );
    }

    #[test]
    fn contradictions_clones_and_underdetermined_case_changes_abstain() {
        assert!(!prediction(&[("user_id", "decode_user_id")], "cache_id", "decode", "").active());
        assert!(
            !prediction(
                &[("user_id", "decode_user_id"); 8],
                "cache_id",
                "decode",
                ""
            )
            .active()
        );
        assert!(
            !prediction(
                &[
                    ("user_id", "decode_user_id"),
                    ("task_id", "decode_task_id"),
                    ("entry_id", "encode_entry_id")
                ],
                "cache_id",
                "decode",
                ""
            )
            .active()
        );
        assert!(
            !prediction(
                &[("amber", "Amber"), ("birch", "Birch")],
                "cache_key",
                "Ca",
                ""
            )
            .active()
        );
        let once = prediction(
            &[("user_id", "decode_user_id"), ("task_id", "decode_task_id")],
            "cache_id",
            "decode_c",
            "",
        );
        let copies = prediction(
            &[
                ("user_id", "decode_user_id"),
                ("task_id", "decode_task_id"),
                ("user_id", "decode_user_id"),
                ("task_id", "decode_task_id"),
            ],
            "cache_id",
            "decode_c",
            "",
        );
        assert_eq!(
            once.probability("decode_cache_id", 0.1),
            copies.probability("decode_cache_id", 0.1)
        );
    }

    #[test]
    fn novel_relations_need_independent_validation_but_verified_counters_do_not() {
        let pairs = [("user_id", "decode_user_id"), ("task_id", "decode_task_id")];
        let mut two = prediction(&pairs, "cache_id", "decode", "");
        assert!(two.novel_candidates().next().is_none());
        two.ground_relations(|_| false);
        assert!(!two.active());
        let mut known = prediction(&pairs, "cache_id", "decode", "");
        known.ground_relations(|word| word == "decode_cache_id");
        assert!(known.active());
        let three = prediction(
            &[
                ("user_id", "decode_user_id"),
                ("task_id", "decode_task_id"),
                ("entry_id", "decode_entry_id"),
            ],
            "cache_id",
            "decode",
            "",
        );
        assert!(
            three
                .novel_candidates()
                .any(|word| word == "decode_cache_id")
        );
        let counter = predict(
            "let record1 = input1;\nlet record2 = input2;\nlet record3 = ",
            "",
            "inp",
            true,
            &|| false,
        )
        .unwrap();
        assert!(counter.novel_candidates().any(|word| word == "input3"));
    }

    #[test]
    fn expensive_optional_program_search_preserves_a_verified_counter() {
        let row = |n| {
            (0..24)
                .map(|column| format!("c{column}_step{n} "))
                .collect::<String>()
        };
        let mut left = (1..=4)
            .map(|n| format!("{}root{n}\n", row(n)))
            .collect::<String>();
        left.push_str(&row(5));
        let result = predict(&left, "", "ro", true, &|| false).unwrap();
        assert!(result.candidates().any(|word| word == "root5"));
        assert!(result.work <= super::super::WORK);
        assert!(predict(&left, "", "ro", true, &|| true).is_none());
    }
}
