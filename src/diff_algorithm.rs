use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use similar::{Algorithm, ChangeTag, DiffOp, DiffTag, TextDiff, capture_diff_slices_deadline};
use std::{
    borrow::Cow,
    num::NonZeroU32,
    time::{Duration, Instant},
};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DiffAlgorithm {
    Classic,
    #[default]
    #[serde(other)]
    Smart,
}

impl DiffAlgorithm {
    pub(crate) const ALL: [Self; 2] = [Self::Smart, Self::Classic];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Smart => "Smart",
            Self::Classic => "Classic",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MissingFinalNewline {
    Original,
    Target,
}

pub(crate) fn missing_final_newline(base: &str, target: &str) -> Option<MissingFinalNewline> {
    if base.is_empty() || target.is_empty() {
        return None;
    }
    match (base.ends_with(['\r', '\n']), target.ends_with(['\r', '\n'])) {
        (false, true) => Some(MissingFinalNewline::Original),
        (true, false) => Some(MissingFinalNewline::Target),
        _ => None,
    }
}

fn fits_interactive_budget(base: &str, target: &str) -> bool {
    const MAX_BYTES: usize = 256 * 1024;
    const MAX_LINES: usize = 2000;

    base.len().saturating_add(target.len()) <= MAX_BYTES
        && [base, target].into_iter().all(|text| {
            let bytes = text.as_bytes();
            let endings = memchr::memchr2_iter(b'\r', b'\n', bytes)
                .filter(|&index| bytes[index] == b'\n' || bytes.get(index + 1) != Some(&b'\n'))
                .take(MAX_LINES + 1)
                .count();
            let unterminated = bytes
                .last()
                .is_some_and(|byte| !matches!(byte, b'\r' | b'\n'));
            endings + usize::from(unterminated) <= MAX_LINES
        })
}

/// Keep expensive comparisons off the UI thread, including small files with
/// a dense set of edits. A small edit in a long shared prefix/suffix is cheap.
pub(crate) fn interactive_file_diff(
    base: &str,
    target: &str,
    algorithm: DiffAlgorithm,
) -> Option<Vec<(char, String)>> {
    const MAX_LINE_PAIRS: usize = 65_536;
    if !fits_interactive_budget(base, target) {
        return None;
    }
    let base = normalize(base);
    let target = normalize(target);
    let old: Vec<_> = base.split_inclusive('\n').collect();
    let new: Vec<_> = target.split_inclusive('\n').collect();
    let (prefix, suffix) = unchanged_edges(&old, &new);
    let old_changes = old.len() - prefix - suffix;
    let new_changes = new.len() - prefix - suffix;
    if old_changes * new_changes > MAX_LINE_PAIRS {
        return None;
    }
    Some(match algorithm {
        DiffAlgorithm::Smart => smart_diff_rows(&old, &new, prefix, suffix),
        DiffAlgorithm::Classic => classic_diff_rows(&base, &target),
    })
}

pub(crate) fn file_diff(base: &str, target: &str, algorithm: DiffAlgorithm) -> Vec<(char, String)> {
    let base = normalize(base);
    let target = normalize(target);
    if algorithm == DiffAlgorithm::Classic {
        return classic_diff_rows(&base, &target);
    }
    if base == target {
        return base
            .split_inclusive('\n')
            .map(|line| line_row(' ', line))
            .collect();
    }

    let old: Vec<_> = base.split_inclusive('\n').collect();
    let new: Vec<_> = target.split_inclusive('\n').collect();
    let (prefix, suffix) = unchanged_edges(&old, &new);
    smart_diff_rows(&old, &new, prefix, suffix)
}

fn normalize(text: &str) -> Cow<'_, str> {
    let bytes = text.as_bytes();
    let mut returns = memchr::memchr_iter(b'\r', bytes);
    let Some(first) = returns.next() else {
        return text.into();
    };
    let mut normalized = String::with_capacity(text.len());
    let mut start = 0;
    for index in std::iter::once(first).chain(returns) {
        normalized.push_str(&text[start..index]);
        normalized.push('\n');
        start = index + 1 + usize::from(bytes.get(index + 1) == Some(&b'\n'));
    }
    normalized.push_str(&text[start..]);
    normalized.into()
}

fn classic_diff_rows(base: &str, target: &str) -> Vec<(char, String)> {
    TextDiff::from_lines(base, target)
        .iter_all_changes()
        .map(change_row)
        .collect()
}

// Skip hashing and looking for anchors in the unchanged edges. A typical
// edit only needs the diff algorithm to process a small slice of the file.
fn unchanged_edges(old: &[&str], new: &[&str]) -> (usize, usize) {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    (prefix, suffix)
}

fn smart_diff_rows(
    old: &[&str],
    new: &[&str],
    prefix: usize,
    suffix: usize,
) -> Vec<(char, String)> {
    let old_middle = &old[prefix..old.len() - suffix];
    let new_middle = &new[prefix..new.len() - suffix];
    let mut rows = Vec::with_capacity(old.len().max(new.len()));
    rows.extend(old[..prefix].iter().map(|line| line_row(' ', line)));
    if old_middle.is_empty() || new_middle.is_empty() {
        rows.extend(old_middle.iter().map(|line| line_row('-', line)));
        rows.extend(new_middle.iter().map(|line| line_row('+', line)));
    } else {
        for op in smart_operations(old_middle, new_middle) {
            let (tag, old_range, new_range) = op.as_tag_tuple();
            if tag == DiffTag::Equal {
                rows.extend(old_middle[old_range].iter().map(|line| line_row(' ', line)));
            } else {
                rows.extend(old_middle[old_range].iter().map(|line| line_row('-', line)));
                rows.extend(new_middle[new_range].iter().map(|line| line_row('+', line)));
            }
        }
    }
    rows.extend(
        old[old.len() - suffix..]
            .iter()
            .map(|line| line_row(' ', line)),
    );
    rows
}

fn smart_operations(old: &[&str], new: &[&str]) -> Vec<DiffOp> {
    let deadline = Some(Instant::now() + Duration::from_millis(250));
    if old.len().max(new.len()) <= 100 {
        return capture_diff_slices_deadline(Algorithm::Patience, old, new, deadline);
    }

    // Intern exact lines once for cheap comparisons. When the two revisions
    // share no lines, a single replacement is exact and needs no search.
    let mut ids = FxHashMap::<&str, usize>::default();
    let intern = |line, ids: &mut FxHashMap<_, _>| {
        let next = ids.len();
        *ids.entry(line).or_insert(next)
    };
    let old_ids: Vec<_> = old
        .iter()
        .copied()
        .map(|line| intern(line, &mut ids))
        .collect();
    let old_unique = ids.len();
    let mut shared = false;
    let new_ids: Vec<_> = new
        .iter()
        .copied()
        .map(|line| {
            let id = intern(line, &mut ids);
            shared |= id < old_unique;
            id
        })
        .collect();
    if !shared {
        return vec![DiffOp::Replace {
            old_index: 0,
            old_len: old.len(),
            new_index: 0,
            new_len: new.len(),
        }];
    }
    drop(ids);
    capture_diff_slices_deadline(Algorithm::Patience, &old_ids, &new_ids, deadline)
}

fn line_row(kind: char, line: &str) -> (char, String) {
    (kind, line.trim_end_matches(['\n', '\r']).to_owned())
}

fn change_row(change: similar::Change<&str>) -> (char, String) {
    let kind = match change.tag() {
        ChangeTag::Equal => ' ',
        ChangeTag::Delete => '-',
        ChangeTag::Insert => '+',
    };
    line_row(kind, change.value())
}

pub(crate) type MoveId = NonZeroU32;

pub(crate) struct DetectedMoves {
    /// One ID per unified row. Both copies of a moved block share an ID.
    pub rows: Vec<Option<MoveId>>,
    pub lengths: Vec<usize>,
}

/// Find unique matching text across change blocks. Extend from these anchors
/// through shared neighbors, including braces and blank lines.
pub(crate) fn detect_moves(lines: &[(char, String)]) -> Option<DetectedMoves> {
    #[derive(Clone, Copy)]
    struct Anchor {
        row: usize,
        hunk: usize,
    }

    let (mut deletes, mut inserts, mut hunks) = (0, 0, 0);
    let mut in_hunk = false;
    for (kind, _) in lines {
        deletes += usize::from(*kind == '-');
        inserts += usize::from(*kind == '+');
        let changed = *kind != ' ';
        hunks += usize::from(changed && !in_hunk);
        in_hunk = changed;
    }
    if hunks < 2 || deletes == 0 || inserts == 0 {
        return None;
    }
    let (first, second) = if deletes <= inserts {
        ('-', '+')
    } else {
        ('+', '-')
    };
    let mut candidates = FxHashMap::<&str, [Option<Anchor>; 2]>::default();
    for (side, change) in [first, second].into_iter().enumerate() {
        let mut hunk = 0;
        for (row, (kind, text)) in lines.iter().enumerate() {
            if *kind == ' ' {
                hunk = row;
            }
            if *kind != change {
                continue;
            }
            let text = move_text(text);
            let slot = if side == 0 {
                if !text.chars().any(char::is_alphanumeric) {
                    continue;
                }
                &mut candidates.entry(text).or_default()[0]
            } else {
                let Some(sides) = candidates.get_mut(text) else {
                    continue;
                };
                &mut sides[1]
            };
            *slot = Some(Anchor {
                row: slot.as_ref().map_or(row, |_| usize::MAX),
                hunk,
            });
        }
    }

    candidates.retain(|_, sides| match *sides {
        [Some(a), Some(b)] => a.row != usize::MAX && b.row != usize::MAX && a.hunk != b.hunk,
        _ => false,
    });
    if candidates.is_empty() {
        return None;
    }
    // A retained copy makes a line ambiguous. It can still be matched as a
    // neighbor of a distinctive anchor in the same moved block.
    for (kind, text) in lines {
        if *kind == ' ' {
            candidates.remove(move_text(text));
            if candidates.is_empty() {
                return None;
            }
        }
    }
    let mut seeds: Vec<_> = candidates
        .into_iter()
        .filter_map(|(text, sides)| {
            let [Some(a), Some(b)] = sides else {
                return None;
            };
            let (old, new) = if first == '-' { (a, b) } else { (b, a) };
            Some((old.row, new.row, text))
        })
        .collect();
    seeds.sort_unstable_by_key(|&(old, _, _)| old);
    let mut moved = DetectedMoves {
        rows: vec![None; lines.len()],
        lengths: Vec::new(),
    };
    let same = |old: usize, new: usize| matches!((lines.get(old), lines.get(new)), (Some(('-', a)), Some(('+', b))) if move_text(a) == move_text(b));
    for (old, new, text) in seeds {
        if moved.rows[old].is_some() || moved.rows[new].is_some() {
            continue;
        }
        let mut before = 0;
        while before < old.min(new)
            && same(old - before - 1, new - before - 1)
            && moved.rows[old - before - 1].is_none()
            && moved.rows[new - before - 1].is_none()
        {
            before += 1;
        }
        let mut after = 1;
        while same(old + after, new + after)
            && moved.rows[old + after].is_none()
            && moved.rows[new + after].is_none()
        {
            after += 1;
        }
        if before + after > 1 || text.chars().take(4).count() == 4 {
            let Some(id) = u32::try_from(moved.lengths.len() + 1)
                .ok()
                .and_then(MoveId::new)
            else {
                break;
            };
            moved.rows[old - before..old + after].fill(Some(id));
            moved.rows[new - before..new + after].fill(Some(id));
            moved.lengths.push(before + after);
        }
    }
    (!moved.lengths.is_empty()).then_some(moved)
}

fn move_text(line: &str) -> &str {
    // Allow reindentation while retaining meaningful whitespace at the end
    // of a nonblank line, such as a Markdown hard break.
    line.trim_start_matches([' ', '\t'])
}

const MAX_PAIR_CANDIDATES: usize = 4096;
const MAX_LINE_BYTES: usize = 2048;

/// Pick an order-preserving alignment that maximizes shared text. The matrix
/// is bounded so a full rewrite can still render quickly.
pub(crate) fn replacement_pairs(
    old: &[(char, String)],
    new: &[(char, String)],
    old_moved: &[Option<MoveId>],
    new_moved: &[Option<MoveId>],
) -> Vec<(usize, usize)> {
    if old.is_empty() || new.is_empty() || old.len().saturating_mul(new.len()) > MAX_PAIR_CANDIDATES
    {
        return Vec::new();
    }

    let old_lines = replacement_lines(old, old_moved);
    let new_lines = replacement_lines(new, new_moved);
    let columns = new.len() + 1;
    let mut scores = vec![0; (old.len() + 1) * columns];
    let mut matched = vec![false; scores.len()];
    for left in (0..old.len()).rev() {
        for right in (0..new.len()).rev() {
            let cell = left * columns + right;
            let skip = scores[cell + 1].max(scores[cell + columns]);
            let similarity = match (&old_lines[left], &new_lines[right]) {
                (Some(old), Some(new)) => line_similarity(old, new),
                _ => 0,
            };
            let paired = scores[cell + columns + 1] + similarity;
            matched[cell] = similarity > 0 && paired >= skip;
            scores[cell] = if matched[cell] { paired } else { skip };
        }
    }

    let mut pairs = Vec::new();
    let (mut left, mut right) = (0, 0);
    while left < old.len() && right < new.len() {
        let cell = left * columns + right;
        if matched[cell] {
            pairs.push((left, right));
            left += 1;
            right += 1;
        } else if scores[cell + 1] >= scores[cell + columns] {
            right += 1;
        } else {
            left += 1;
        }
    }
    pairs
}

struct ReplacementLine<'a> {
    text: &'a str,
    words: Vec<&'a str>,
}

fn replacement_lines<'a>(
    lines: &'a [(char, String)],
    moved: &[Option<MoveId>],
) -> Vec<Option<ReplacementLine<'a>>> {
    lines
        .iter()
        .enumerate()
        .map(|(index, (_, line))| {
            // Include indentation and trailing whitespace in the budget because
            // the highlight pass will process the full original line.
            if line.len() > MAX_LINE_BYTES || moved.get(index).is_some_and(Option::is_some) {
                return None;
            }
            let text = line.trim();
            let mut words: Vec<_> = text.unicode_words().collect();
            words.sort_unstable();
            Some(ReplacementLine { text, words })
        })
        .collect()
}

fn line_similarity(old: &ReplacementLine<'_>, new: &ReplacementLine<'_>) -> usize {
    let (old_words, new_words) = (&old.words, &new.words);
    let (old, new) = (old.text, new.text);
    if old == new {
        return 250;
    }

    let mut old_iter = old_words.iter().peekable();
    let mut new_iter = new_words.iter().peekable();
    let mut shared = 0usize;
    while let (Some(a), Some(b)) = (old_iter.peek(), new_iter.peek()) {
        use std::cmp::Ordering::*;
        match a.cmp(b) {
            Less => {
                old_iter.next();
            }
            Greater => {
                new_iter.next();
            }
            Equal => {
                shared += 1;
                old_iter.next();
                new_iter.next();
            }
        }
    }
    let word_score = (200 * shared)
        .checked_div(old_words.len() + new_words.len())
        .unwrap_or(0);
    let (prefix, suffix) = common_edges(old, new);
    let edge_score = 200 * (prefix + suffix) / (old.len() + new.len());
    // Shared words also give a small boost to borderline prefix/suffix
    // matches, such as a renamed identifier on a line with a new literal.
    let score = word_score.max(edge_score + (100 - edge_score) * word_score / 400);
    // Break ties between lines with the same words by preferring shared
    // ordered text. An exact (trimmed) match earns the maximum score above.
    if score >= 50 {
        2 * score + edge_score / 2
    } else {
        0
    }
}

/// Return shared UTF-8 byte lengths at both ends, without overlapping.
fn common_edges(old: &str, new: &str) -> (usize, usize) {
    let shared_bytes = |(a, b): (char, char)| (a == b).then_some(a.len_utf8());
    let prefix = old.chars().zip(new.chars()).map_while(shared_bytes).sum();
    let suffix = old[prefix..]
        .chars()
        .rev()
        .zip(new[prefix..].chars().rev())
        .map_while(shared_bytes)
        .sum();
    (prefix, suffix)
}

/// Match whole words and punctuation first. Within one edited token,
/// highlight the changed graphemes when the token retains a common edge.
pub(crate) fn word_highlights(old: &str, new: &str) -> (Vec<bool>, Vec<bool>) {
    let old_tokens: Vec<_> = old.split_word_bounds().collect();
    let new_tokens: Vec<_> = new.split_word_bounds().collect();
    let diff = TextDiff::from_slices(&old_tokens, &new_tokens);
    let mut old_mask = Vec::new();
    let mut new_mask = Vec::new();
    let append = |mask: &mut Vec<bool>, text: &str, changed| {
        mask.extend(std::iter::repeat_n(changed, text.chars().count()));
    };
    for op in diff.ops() {
        let (tag, old_range, new_range) = op.as_tag_tuple();
        if tag == DiffTag::Replace && old_range.len() == 1 && new_range.len() == 1 {
            let (a, b) = (old_tokens[old_range.start], new_tokens[new_range.start]);
            let prefix = a
                .graphemes(true)
                .zip(b.graphemes(true))
                .take_while(|(a, b)| a == b)
                .map(|(g, _)| g.len())
                .sum();
            let suffix: usize = a[prefix..]
                .graphemes(true)
                .rev()
                .zip(b[prefix..].graphemes(true).rev())
                .take_while(|(a, b)| a == b)
                .map(|(g, _)| g.len())
                .sum();
            if 2 * (prefix + suffix) >= a.len().max(b.len()) {
                for (mask, text) in [(&mut old_mask, a), (&mut new_mask, b)] {
                    let end = text.len() - suffix;
                    append(mask, &text[..prefix], false);
                    append(mask, &text[prefix..end], true);
                    append(mask, &text[end..], false);
                }
                continue;
            }
        }
        for token in &old_tokens[old_range] {
            append(&mut old_mask, token, tag != DiffTag::Equal);
        }
        for token in &new_tokens[new_range] {
            append(&mut new_mask, token, tag != DiffTag::Equal);
        }
    }
    // Unicode word boundaries can fall inside a grapheme, such as when
    // a prepend character attaches to punctuation. Paint the whole cluster.
    for (text, mask) in [(old, &mut old_mask), (new, &mut new_mask)] {
        if !text.is_ascii() {
            let mut offset = 0;
            for grapheme in text.graphemes(true) {
                let end = offset + grapheme.chars().count();
                let flags = &mut mask[offset..end];
                if flags.iter().any(|&changed| changed) {
                    flags.fill(true);
                }
                offset = end;
            }
        }
    }
    (old_mask, new_mask)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::DiffRenderCache;

    #[test]
    fn smart_diff_marks_moved_blocks_with_reindentation() {
        let base = "begin\nfn moved() {\n    record_event();\n}\nkeep one\nkeep two\nkeep three\nkeep four\nend\n";
        let target = "begin\nkeep one\nkeep two\nkeep three\nkeep four\n    fn moved() {\n        record_event();\n    }\nend\n";
        let changes = file_diff(base, target, DiffAlgorithm::Smart);
        let cache = DiffRenderCache::with_algorithm(&changes, DiffAlgorithm::Smart);
        assert_eq!(
            cache
                .rows
                .iter()
                .filter_map(|row| row.left_line.filter(|_| row.left_move.is_some()))
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(
            cache
                .rows
                .iter()
                .filter_map(|row| row.right_line.filter(|_| row.right_move.is_some()))
                .collect::<Vec<_>>(),
            [5, 6, 7]
        );
        assert_eq!(cache.hunks.len(), 2);
        assert!(
            cache.rows.iter().all(
                |row| row.left_char_highlights.is_none() && row.right_char_highlights.is_none()
            )
        );
    }

    #[test]
    fn smart_diff_pairs_related_lines_across_insertions() {
        let base = "start\nlet display_name = \"draft\";\nlet userCount = 5;\nend\n";
        let target = "start\nvalidate_inputs();\nlet display_name = \"published\";\nlet usersCount = 6;\nend\n";
        let highlighted = |text: &str, mask: &[bool]| {
            text.chars()
                .zip(mask)
                .filter_map(|(ch, flag)| flag.then_some(ch))
                .collect::<String>()
        };

        let smart = DiffRenderCache::with_algorithm(
            &file_diff(base, target, DiffAlgorithm::Smart),
            DiffAlgorithm::Smart,
        );
        assert_eq!(smart.rows[1].left_line, None);
        assert_eq!(smart.rows[2].left_line, Some(1));
        assert_eq!(smart.rows[3].left_line, Some(2));
        assert_eq!(
            highlighted(
                &smart.rows[2].right_text,
                smart.rows[2].right_char_highlights.as_deref().unwrap()
            ),
            "published"
        );
        assert_eq!(
            highlighted(
                &smart.rows[3].right_text,
                smart.rows[3].right_char_highlights.as_deref().unwrap()
            ),
            "s6"
        );

        let classic = DiffRenderCache::with_algorithm(
            &file_diff(base, target, DiffAlgorithm::Classic),
            DiffAlgorithm::Classic,
        );
        assert_eq!(classic.rows[1].left_line, Some(1));
        assert_eq!(classic.rows[2].left_line, Some(2));
        assert_eq!(classic.rows[3].left_line, None);
    }

    #[test]
    fn smart_diff_keeps_combining_marks_with_their_letters() {
        assert_eq!(
            word_highlights("cafe\u{301}", "cafe\u{300}"),
            (
                vec![false, false, false, true, true],
                vec![false, false, false, true, true]
            ),
        );
    }

    #[test]
    fn smart_diff_ignores_ambiguous_move_anchors_and_preserves_trailing_edits() {
        for (base, target) in [
            (
                "start\nreport();\nsection one\nreport();\nsection two\nsection three\nend\n",
                "start\nsection one\nreport();\nsection two\nsection three\nreport();\nend\n",
            ),
            (
                "start\nhard break  \nsection one\nsection two\nsection three\nend\n",
                "start\nsection one\nsection two\nsection three\nhard break\nend\n",
            ),
        ] {
            let changes = file_diff(base, target, DiffAlgorithm::Smart);
            assert!(changes.iter().any(|(kind, _)| *kind == '-'));
            assert!(changes.iter().any(|(kind, _)| *kind == '+'));
            assert!(detect_moves(&changes).is_none());
        }
    }

    #[test]
    fn smart_diff_prefers_identical_content_to_a_permutation_of_its_words() {
        let old = [('-', "let n = sum(a, b);".to_owned())];
        let new = [
            ('+', "let n = sum(b, a);".to_owned()),
            ('+', "    let n = sum(a, b);".to_owned()),
        ];
        assert_eq!(replacement_pairs(&old, &new, &[], &[]), [(0, 1)]);
    }

    #[test]
    fn smart_diff_measures_full_line_bytes_before_pairing() {
        let old = [('-', format!("{}same", " ".repeat(MAX_LINE_BYTES)))];
        let new = [('+', format!("{}same", "\t".repeat(MAX_LINE_BYTES)))];
        assert!(replacement_pairs(&old, &new, &[], &[]).is_empty());
    }

    #[test]
    fn smart_diff_highlights_graphemes_that_span_word_boundaries() {
        assert_eq!(
            word_highlights("\u{600}!", "\u{600}?"),
            (vec![true, true], vec![true, true]),
        );
    }

    #[test]
    fn interactive_budget_counts_crlf_as_one_break_and_counts_lone_cr() {
        assert!(fits_interactive_budget("", &"x\r\n".repeat(2000)));
        assert!(!fits_interactive_budget("", &"x\r".repeat(2001)));
        assert!(!fits_interactive_budget(
            "",
            &format!("{}x", "\n".repeat(2000))
        ));
        assert!(!fits_interactive_budget(&"x".repeat(256 * 1024), "x"));
    }

    #[test]
    fn interactive_diff_defers_dense_edits_but_keeps_sparse_edits() {
        let base: String = (0..1200).map(|line| format!("old_{line}\n")).collect();
        let rewritten: String = (0..1200).map(|line| format!("new_{line}\n")).collect();
        let edited = base.replacen("old_600\n", "new_600\n", 1);
        assert!(fits_interactive_budget(&base, &rewritten));
        for algorithm in DiffAlgorithm::ALL {
            assert!(interactive_file_diff(&base, &rewritten, algorithm).is_none());
            assert_eq!(
                interactive_file_diff(&base, &edited, algorithm).unwrap(),
                file_diff(&base, &edited, algorithm),
            );
        }
    }

    #[test]
    fn newline_difference_distinguishes_eof_from_line_ending_style() {
        use MissingFinalNewline::{Original, Target};
        assert_eq!(missing_final_newline("same", "same\n"), Some(Original));
        assert_eq!(missing_final_newline("same\r\n", "same"), Some(Target));
        assert_eq!(missing_final_newline("same\r", "same\n"), None);
        assert_eq!(missing_final_newline("", "new"), None);
    }

    #[test]
    fn smart_diff_interner_preserves_patience_alignment() {
        let base: Vec<_> = (0..180)
            .map(|index| format!("keep {}\n", index % 31))
            .collect();
        let inserted: Vec<_> = (0..160).map(|index| format!("insert {index}\n")).collect();
        let old: Vec<_> = base.iter().map(String::as_str).collect();
        let new = [
            &old[72..140],
            &inserted.iter().map(String::as_str).collect::<Vec<_>>(),
            &old[..43],
        ]
        .concat();
        let expected = TextDiff::configure()
            .algorithm(Algorithm::Patience)
            .diff_slices(&old, &new);
        assert_eq!(smart_operations(&old, &new), expected.ops());
    }

    #[test]
    fn smart_diff_disjoint_middle_is_one_exact_replacement() {
        let old = vec!["old\n"; 300];
        let new = vec!["new\n"; 500];
        assert_eq!(
            smart_operations(&old, &new),
            [DiffOp::Replace {
                old_index: 0,
                old_len: 300,
                new_index: 0,
                new_len: 500
            }],
        );
    }

    #[test]
    fn smart_diff_projects_both_revisions_in_order_across_layouts() {
        fn source_row(row: &crate::app::AlignedDiffRow, original: bool) -> Option<(usize, &str)> {
            if original {
                row.left_line.map(|line| (line, row.left_text.as_str()))
            } else {
                row.right_line.map(|line| (line, row.right_text.as_str()))
            }
        }

        fn file(seed: &mut u32) -> String {
            let mut pick = |count: usize| {
                *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (*seed >> 16) as usize % count
            };
            let lines = [
                "",
                " ",
                "{",
                "    }",
                "cafe\u{301}()",
                "\u{600}!",
                "🧑‍💻",
                "abc();",
            ];
            let endings = ["\n", "\r\n", "\r"];
            let mut text = String::new();
            let count = pick(24);
            for row in 0..count {
                text.push_str(lines[pick(lines.len())]);
                let ending = pick(endings.len() + usize::from(row + 1 == count));
                text.push_str(endings.get(ending).copied().unwrap_or(""));
            }
            text
        }

        let mut seed = 1009;
        for _ in 0..400 {
            let base = file(&mut seed);
            let target = file(&mut seed);
            let changes = file_diff(&base, &target, DiffAlgorithm::Smart);
            let cache = DiffRenderCache::with_algorithm(&changes, DiffAlgorithm::Smart);
            for (original, text) in [(true, base), (false, target)] {
                let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
                let expected: Vec<_> = normalized.lines().enumerate().collect();
                let aligned: Vec<_> = cache
                    .rows
                    .iter()
                    .filter_map(|row| source_row(row, original))
                    .collect();
                let omitted = if original { '+' } else { '-' };
                let inline: Vec<_> = cache
                    .inline_rows
                    .iter()
                    .filter(|row| row.kind != omitted)
                    .filter_map(|row| source_row(&cache.rows[row.aligned_row], original))
                    .collect();
                assert_eq!(aligned, expected);
                assert_eq!(inline, expected);
            }
        }
    }
}
