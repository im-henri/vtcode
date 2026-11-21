use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use similar::{ChangeTag, DiffTag, TextDiff};
use std::time::Duration;
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

pub(crate) fn file_diff(base: &str, target: &str, algorithm: DiffAlgorithm) -> Vec<(char, String)> {
    // Avoid copying files that are already LF-normalized.
    fn normalize(text: &str) -> std::borrow::Cow<'_, str> {
        if memchr::memchr(b'\r', text.as_bytes()).is_none() {
            return text.into();
        }
        text.replace("\r\n", "\n").replace('\r', "\n").into()
    }

    let base = normalize(base);
    let target = normalize(target);
    if algorithm == DiffAlgorithm::Classic {
        return TextDiff::from_lines(base.as_ref(), target.as_ref())
            .iter_all_changes()
            .map(change_row)
            .collect();
    }
    if base == target {
        return base
            .split_inclusive('\n')
            .map(|line| line_row(' ', line))
            .collect();
    }

    // Skip hashing and looking for anchors in the unchanged edges. A typical
    // edit only needs the diff algorithm to process a small slice of the file.
    let old: Vec<_> = base.split_inclusive('\n').collect();
    let new: Vec<_> = target.split_inclusive('\n').collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let old_middle = &old[prefix..old.len() - suffix];
    let new_middle = &new[prefix..new.len() - suffix];
    let mut rows = Vec::with_capacity(old.len().max(new.len()));
    rows.extend(old[..prefix].iter().map(|line| line_row(' ', line)));
    if old_middle.is_empty() || new_middle.is_empty() {
        rows.extend(old_middle.iter().map(|line| line_row('-', line)));
        rows.extend(new_middle.iter().map(|line| line_row('+', line)));
    } else {
        let diff = TextDiff::configure()
            .algorithm(similar::Algorithm::Patience)
            .timeout(Duration::from_millis(250))
            .diff_slices(old_middle, new_middle);
        rows.extend(diff.iter_all_changes().map(change_row));
    }
    rows.extend(
        old[old.len() - suffix..]
            .iter()
            .map(|line| line_row(' ', line)),
    );
    rows
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

/// Find unique matching text across change blocks. Extend from these anchors
/// through shared neighbors, including braces and blank lines.
pub(crate) fn moved_lines(lines: &[(char, String)]) -> Option<Vec<bool>> {
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
            let text = text.trim();
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

    let mut seeds: Vec<_> = candidates
        .into_iter()
        .filter_map(|(text, sides)| {
            let [Some(a), Some(b)] = sides else {
                return None;
            };
            let (old, new) = if first == '-' { (a, b) } else { (b, a) };
            (old.row != usize::MAX && new.row != usize::MAX && old.hunk != new.hunk)
                .then_some((old.row, new.row, text))
        })
        .collect();
    if seeds.is_empty() {
        return None;
    }
    seeds.sort_unstable_by_key(|&(old, _, _)| old);
    let mut moved = vec![false; lines.len()];
    let same = |old: usize, new: usize| matches!((lines.get(old), lines.get(new)), (Some(('-', a)), Some(('+', b))) if a.trim() == b.trim());
    for (old, new, text) in seeds {
        if moved[old] || moved[new] {
            continue;
        }
        let mut before = 0;
        while before < old.min(new)
            && same(old - before - 1, new - before - 1)
            && !moved[old - before - 1]
            && !moved[new - before - 1]
        {
            before += 1;
        }
        let mut after = 1;
        while same(old + after, new + after) && !moved[old + after] && !moved[new + after] {
            after += 1;
        }
        if before + after > 1 || text.chars().take(4).count() == 4 {
            moved[old - before..old + after].fill(true);
            moved[new - before..new + after].fill(true);
        }
    }
    Some(moved)
}

const MAX_PAIR_CANDIDATES: usize = 4096;
const MAX_LINE_BYTES: usize = 2048;

/// Pick an order-preserving alignment that maximizes shared text. The matrix
/// is bounded so a full rewrite can still render quickly.
pub(crate) fn replacement_pairs(
    old: &[(char, String)],
    new: &[(char, String)],
    old_moved: &[bool],
    new_moved: &[bool],
) -> Vec<(usize, usize)> {
    if old.is_empty() || new.is_empty() || old.len().saturating_mul(new.len()) > MAX_PAIR_CANDIDATES
    {
        return Vec::new();
    }

    let old_words: Vec<_> = old
        .iter()
        .map(|(_, line)| significant_words(line))
        .collect();
    let new_words: Vec<_> = new
        .iter()
        .map(|(_, line)| significant_words(line))
        .collect();
    let columns = new.len() + 1;
    let mut scores = vec![0; (old.len() + 1) * columns];
    let mut matched = vec![false; scores.len()];
    for left in (0..old.len()).rev() {
        for right in (0..new.len()).rev() {
            let cell = left * columns + right;
            let skip = scores[cell + 1].max(scores[cell + columns]);
            let similarity =
                if old_moved.get(left) == Some(&true) || new_moved.get(right) == Some(&true) {
                    0
                } else {
                    line_similarity(
                        &old[left].1,
                        &new[right].1,
                        &old_words[left],
                        &new_words[right],
                    )
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

fn significant_words(line: &str) -> Vec<&str> {
    if line.len() > MAX_LINE_BYTES {
        return Vec::new();
    }
    let mut words: Vec<_> = line.unicode_words().collect();
    words.sort_unstable();
    words
}

fn line_similarity(old: &str, new: &str, old_words: &[&str], new_words: &[&str]) -> usize {
    let (old, new) = (old.trim(), new.trim());
    if old.len().max(new.len()) > MAX_LINE_BYTES {
        return 0;
    }
    if old == new {
        return 100;
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
    if score >= 50 { score } else { 0 }
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
                .filter_map(|row| row.left_line.filter(|_| row.left_moved))
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(
            cache
                .rows
                .iter()
                .filter_map(|row| row.right_line.filter(|_| row.right_moved))
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
}
