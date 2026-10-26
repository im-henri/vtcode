//! Identifier-specific reranking: case, separators, CamelCase and acronyms.
//! This bounded pass is independent of the shared filename fuzzy matcher.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Match {
    Typo,
    Fuzzy,
    Boundary,
    Prefix,
}

pub(super) fn typo_prefix(word: &str, query: &str) -> bool {
    if !(4..=32).contains(&query.len()) || !query.is_ascii() || !word.is_ascii() {
        return false;
    }
    let word = word.as_bytes();
    let query = query.as_bytes();
    let equal = |a: u8, b: u8| a.eq_ignore_ascii_case(&b);
    let mut i = 0;
    while i < query.len() && i < word.len() && equal(query[i], word[i]) {
        i += 1;
    }
    if i == query.len() {
        return false;
    }
    let rest = |a: usize, b: usize| {
        query[a..].len() <= word.len().saturating_sub(b)
            && query[a..]
                .iter()
                .zip(&word[b.min(word.len())..])
                .all(|(&a, &b)| equal(a, b))
    };
    (i + 1 < query.len()
        && i + 1 < word.len()
        && equal(query[i], word[i + 1])
        && equal(query[i + 1], word[i])
        && rest(i + 2, i + 2))
        || (i < word.len() && rest(i + 1, i + 1))
        || rest(i + 1, i)
        || (i < word.len() && rest(i, i + 1))
}

pub(super) fn quality(word: &str, query: &str) -> (i64, Match) {
    if query.is_empty() {
        return (0, Match::Fuzzy);
    }
    let lower = query.to_lowercase();
    let prefix = word
        .get(..query.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(query))
        || ((!word.is_ascii() || !query.is_ascii()) && word.to_lowercase().starts_with(&lower));
    let boundary = crate::completion_context::initials_start_with(word, &lower);
    let class = if prefix {
        Match::Prefix
    } else if boundary {
        Match::Boundary
    } else {
        Match::Fuzzy
    };
    if !word.is_ascii() || !query.is_ascii() || word.len() > 160 || query.len() > 48 {
        return (
            i64::from(word.starts_with(query)) * 20 + i64::from(boundary) * 24,
            class,
        );
    }
    const MIN: i32 = -100_000;
    let mut previous = [MIN; 160];
    let mut current = [MIN; 160];
    let bytes = word.as_bytes();
    for (qi, &wanted) in query.as_bytes().iter().enumerate() {
        current.fill(MIN);
        let mut gap = MIN;
        for (at, &ch) in bytes.iter().enumerate() {
            if at >= 2 {
                gap = gap.max(previous[at - 2] + (at - 2) as i32);
            }
            if !ch.eq_ignore_ascii_case(&wanted) {
                continue;
            }
            let before = if qi == 0 {
                -(at.min(3) as i32) * 3
            } else {
                (if at > 0 { previous[at - 1] + 5 } else { MIN }).max(gap - at as i32 + 1)
            };
            let boundary = at == 0
                || !bytes[at - 1].is_ascii_alphanumeric()
                || (bytes[at - 1].is_ascii_lowercase() && ch.is_ascii_uppercase())
                || (bytes[at - 1].is_ascii_uppercase()
                    && ch.is_ascii_uppercase()
                    && bytes.get(at + 1).is_some_and(u8::is_ascii_lowercase));
            let case = if ch == wanted {
                if wanted.is_ascii_uppercase() { 18 } else { 3 }
            } else {
                0
            };
            current[at] = before + 12 + i32::from(boundary) * 10 + case;
        }
        std::mem::swap(&mut previous, &mut current);
    }
    let best = previous[..bytes.len()].iter().copied().max().unwrap_or(MIN);
    if best < MIN / 2 && typo_prefix(word, query) {
        return (-70, Match::Typo);
    }
    let length = word.len().saturating_sub(query.len()).min(80) as i64;
    (
        i64::from(best.max(0)) / 2 - length / 4 + i64::from(word.starts_with(query)) * 18,
        class,
    )
}
