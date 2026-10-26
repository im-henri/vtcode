//! Exact, bounded three-way reversal. No fuzzy patch application or conflict markers.
use serde::{Deserialize, Serialize};
use std::{ops::Range, time::Duration};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hunk {
    pub old: Range<usize>,
    pub new: Range<usize>,
}

pub fn hunks(old: &str, new: &str) -> Vec<Hunk> {
    if old == new {
        return Vec::new();
    }
    // Bound both allocation and pathological diff time. A coarse hunk is safe;
    // it merely offers less granular review and more conservative conflicts.
    if old.len() + new.len() > 4 * 1024 * 1024 {
        return vec![Hunk {
            old: 0..old.len(),
            new: 0..new.len(),
        }];
    }
    let offsets = |lines: &[&str]| {
        let mut result = vec![0];
        for line in lines {
            result.push(result.last().unwrap() + line.len());
        }
        result
    };
    let diff = similar::TextDiff::configure()
        .timeout(Duration::from_millis(200))
        .diff_lines(old, new);
    // Use the diff's tokens: it recognizes standalone CR as well as LF/CRLF.
    let a = offsets(diff.old_slices());
    let b = offsets(diff.new_slices());
    let mut hunks: Vec<_> = diff
        .ops()
        .iter()
        .filter_map(|op| {
            let (tag, x, y) = op.as_tag_tuple();
            (tag != similar::DiffTag::Equal).then(|| Hunk {
                old: a[x.start]..a[x.end],
                new: b[y.start]..b[y.end],
            })
        })
        .collect();
    // Diff compaction can move a deletion/insertion across equal lines without
    // updating its unused (empty) side. Those positions are not patch anchors.
    // Derive the anchors from the consumed sides and verify every untouched gap.
    if normalize_empty_anchors(&mut hunks).is_err() || !exact_hunks(old, new, &hunks) {
        return vec![Hunk {
            old: 0..old.len(),
            new: 0..new.len(),
        }];
    }
    hunks
}

/// Preserve chunk identity and text while correcting only the empty side of
/// insertions/deletions emitted by older diff compaction. Verify against both
/// snapshots with `exact_hunks` before persisting or using a corrected sequence.
pub(super) fn normalize_empty_anchors(hunks: &mut [Hunk]) -> Result<bool, String> {
    let (mut old, mut new) = (0usize, 0usize);
    let mut changed = false;
    for hunk in hunks {
        if hunk.old.start > hunk.old.end
            || hunk.new.start > hunk.new.end
            || (hunk.old.is_empty() && hunk.new.is_empty())
        {
            return Err("Invalid chunk sequence in rollback history".into());
        }
        let anchor = |cursor: usize, start: usize, previous: usize| {
            start
                .checked_sub(previous)
                .and_then(|gap| cursor.checked_add(gap))
                .ok_or_else(|| "Invalid chunk sequence in rollback history".to_string())
        };
        if hunk.old.is_empty() {
            let start = anchor(old, hunk.new.start, new)?;
            changed |= hunk.old.start != start;
            hunk.old = start..start;
        } else if hunk.new.is_empty() {
            let start = anchor(new, hunk.old.start, old)?;
            changed |= hunk.new.start != start;
            hunk.new = start..start;
        }
        if hunk.old.start < old || hunk.new.start < new {
            return Err("Invalid chunk sequence in rollback history".into());
        }
        old = hunk.old.end;
        new = hunk.new.end;
    }
    Ok(changed)
}

pub(super) fn exact_hunks(old: &str, new: &str, hunks: &[Hunk]) -> bool {
    let (mut x, mut y) = (0, 0);
    for hunk in hunks {
        if old.get(hunk.old.clone()).is_none() || new.get(hunk.new.clone()).is_none() {
            return false;
        }
        match (old.get(x..hunk.old.start), new.get(y..hunk.new.start)) {
            (Some(a), Some(b)) if a == b => {}
            _ => return false,
        }
        x = hunk.old.end;
        y = hunk.new.end;
    }
    matches!((old.get(x..), new.get(y..)), (Some(a), Some(b)) if a == b)
}

fn overlap(a: &Range<usize>, b: &Range<usize>) -> bool {
    if a.is_empty() || b.is_empty() {
        // Insertions exactly on an edited boundary are deliberately ambiguous.
        a.start <= b.end && b.start <= a.end
    } else {
        a.start < b.end && b.start < a.end
    }
}

/// Merge base -> wanted into current, retaining disjoint base -> current edits.
pub fn merge(base: &str, wanted: &str, current: &str) -> Result<String, String> {
    if current == base {
        return Ok(wanted.into());
    }
    if wanted == base {
        return Ok(current.into());
    }
    let ours = hunks(base, wanted);
    let theirs = hunks(base, current);
    let mut result = current.to_string();
    for hunk in ours.iter().rev() {
        if theirs.iter().any(|other| overlap(&hunk.old, &other.old)) {
            return Err("Later edits overlap this change; no automatic merge is safe".into());
        }
        // Repeated blocks can admit multiple equally valid LCS alignments.
        // Never let that arbitrary tie-break choose which user's block to undo.
        let anchor = if hunk.old.is_empty() {
            let start = base[..hunk.old.start]
                .trim_end_matches('\n')
                .rfind('\n')
                .map_or(0, |i| i + 1);
            let end = base[hunk.old.end..]
                .find('\n')
                .map_or(base.len(), |i| hunk.old.end + i + 1);
            &base[start..end]
        } else {
            &base[hunk.old.clone()]
        };
        if anchor.is_empty()
            || base.match_indices(anchor).take(2).count() != 1
            || current.match_indices(anchor).take(2).count() != 1
        {
            return Err("Repeated or changed text makes this rollback ambiguous".into());
        }
        let shift: isize = theirs
            .iter()
            .filter(|other| other.old.end <= hunk.old.start)
            .map(|other| other.new.len() as isize - other.old.len() as isize)
            .sum();
        let start = hunk
            .old
            .start
            .checked_add_signed(shift)
            .ok_or("Invalid merge offset")?;
        let end = start + hunk.old.len();
        if result.get(start..end) != base.get(hunk.old.clone()) {
            return Err("The current file no longer matches this change".into());
        }
        result.replace_range(start..end, &wanted[hunk.new.clone()]);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::reversed_empty_ranges)] // Exercise rejection of a malformed hunk.
    fn empty_anchors_follow_equal_gaps_without_changing_chunk_identity() {
        let old = "head\nremove\n\nend\n";
        let new = "head\n\ninserted\nend\n";
        let mut hunks = vec![
            Hunk {
                old: 5..12,
                new: 15..15,
            },
            Hunk {
                old: 12..12,
                new: 6..15,
            },
        ];
        assert!(normalize_empty_anchors(&mut hunks).unwrap());
        assert_eq!(
            hunks,
            [
                Hunk {
                    old: 5..12,
                    new: 5..5
                },
                Hunk {
                    old: 13..13,
                    new: 6..15
                },
            ]
        );
        assert!(exact_hunks(old, new, &hunks));
        assert!(!normalize_empty_anchors(&mut hunks).unwrap());
        assert!(!exact_hunks(old, "head\nXinserted\nend\n", &hunks));
        assert!(
            normalize_empty_anchors(&mut [Hunk {
                old: 2..1,
                new: 0..0
            }])
            .is_err()
        );
    }

    #[test]
    fn diff_hunks_round_trip_repeated_lines_and_mixed_newlines() {
        let texts = [
            "",
            "head\nremove\n\nend\n",
            "head\n\ninserted\nend\n",
            "same\nsame\n\nsame\nend",
            "same\n\nsame\nadded\nend",
            "α\r\nβ\rγ\nlast",
            "α\r\nchanged\rγ\nlast",
            "\n\r\n\r\n\n",
            "\r\n\n\r\r\n",
            "same",
            "other",
        ];
        for old in texts {
            for new in texts {
                let changes = hunks(old, new);
                assert!(exact_hunks(old, new, &changes), "old={old:?}, new={new:?}");
                let mut canonical = changes.clone();
                assert!(!normalize_empty_anchors(&mut canonical).unwrap());
                let mut forward = old.to_string();
                let mut reverse = new.to_string();
                for hunk in changes.iter().rev() {
                    forward.replace_range(hunk.old.clone(), &new[hunk.new.clone()]);
                    reverse.replace_range(hunk.new.clone(), &old[hunk.old.clone()]);
                }
                assert_eq!(forward, new);
                assert_eq!(reverse, old);
            }
        }
    }

    #[test]
    fn disjoint_insertions_deletions_and_repeated_lines_never_silently_discard_user_work() {
        for lines in [
            vec!["same\n", "same\n", "same\n", "same\n", "end\n"],
            vec!["\n", "a\n", "\n", "a\n", "\n"],
            vec!["α\r\n", "β\r\n", "α\r\n", "β\r\n", "last"],
            vec!["one\n", "two\n", "three\n", "four\n", "last"],
        ] {
            for a in 0..=lines.len() {
                for b in a..=lines.len().min(a + 2) {
                    for c in 0..=lines.len() {
                        for d in c..=lines.len().min(c + 2) {
                            if overlap(&(a..b), &(c..d)) {
                                continue;
                            }
                            for restore in [vec![], vec!["original\n"]] {
                                for user in [vec![], vec!["USER WORK\n"]] {
                                    let mut wanted = lines.clone();
                                    wanted.splice(a..b, restore.clone());
                                    let mut current = lines.clone();
                                    current.splice(c..d, user.clone());
                                    let mut expected = lines.clone();
                                    if a < c {
                                        expected.splice(c..d, user);
                                        expected.splice(a..b, restore.clone());
                                    } else {
                                        expected.splice(a..b, restore.clone());
                                        expected.splice(c..d, user);
                                    }
                                    if let Ok(merged) =
                                        merge(&lines.concat(), &wanted.concat(), &current.concat())
                                    {
                                        assert_eq!(
                                            merged,
                                            expected.concat(),
                                            "base={lines:?}, restore={a}..{b}, user={c}..{d}"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn preserves_disjoint_edits_and_exact_bytes() {
        assert_eq!(
            merge(
                "α\r\nagent\r\nlast",
                "α\r\nbefore\r\nlast",
                "user\r\nα\r\nagent\r\nlast"
            )
            .unwrap(),
            "user\r\nα\r\nbefore\r\nlast"
        );
        assert!(merge("a\nb\nc\n", "a\nold\nc\n", "a\nuser\nc\n").is_err());
        assert!(merge("a\nc\n", "a\nb\nc\n", "a\nuser\nc\n").is_err());
    }
}
