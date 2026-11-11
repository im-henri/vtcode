//! Git diff display and formatting utilities.
//!
//! Contains pure functions for computing line diffs, parsing patch text,
//! formatting time offsets, and mapping lines between worktree/index.
//! Extracted from `app_git.rs`.

use crate::app::App;

impl App {
    pub(crate) fn format_time_ago(now: i64, then: i64) -> String {
        let diff = (now - then).max(0) as u64;
        if diff < 60 {
            return format!("{diff}s ago");
        }
        let mins = diff / 60;
        if mins < 60 {
            return format!("{mins}m ago");
        }
        let hours = mins / 60;
        if hours < 24 {
            return format!("{hours}h ago");
        }
        let days = hours / 24;
        if days < 30 {
            return format!("{days}d ago");
        }
        let months = days / 30;
        if months < 12 {
            return format!("{months}mo ago");
        }
        let years = days / 365;
        format!("{years}y ago")
    }
}

/// Compute per-line diff status using the `similar` crate.
/// Returns a Vec<char> aligned to lines in `new`:
///   'A' = added, 'M' = modified, 'D' = deletion adjacent to this line,
///   ' ' = unchanged. A file whose final lines were all deleted retains one
///   synthetic `D` entry so the editor's remaining empty line can show it.
pub(crate) fn compute_line_diff(old: &[&str], new: &[&str]) -> Vec<char> {
    // Use from_slices to avoid O(n) join allocations
    let diff = similar::TextDiff::from_slices(old, new);
    line_diff_from_ops(old.len(), new.len(), diff.ops())
}

fn line_diff_from_ops(old_len: usize, new_len: usize, ops: &[similar::DiffOp]) -> Vec<char> {
    let mut result = vec![' '; new_len.max(usize::from(old_len > 0 && new_len == 0))];
    for op in ops {
        let (start, len, marker) = match *op {
            similar::DiffOp::Insert { new_index, new_len, .. } => (new_index, new_len, 'A'),
            similar::DiffOp::Replace { new_index, new_len, .. } => (new_index, new_len, 'M'),
            _ => continue,
        };
        result[start..start + len].fill(marker);
    }
    for op in ops {
        if let similar::DiffOp::Delete { new_index, .. } = *op {
            let line = new_index.min(result.len().saturating_sub(1));
            result[line] = merge_kind(result[line], 'D');
        }
    }
    result
}

fn merge_kind(left: char, right: char) -> char {
    match (left, right) {
        (' ', other) | (other, ' ') => other,
        (a, b) if a == b => a,
        _ => 'M',
    }
}

/// Both marker sets must use worktree coordinates. Reuse the unstaged diff's
/// equal ranges and project overlapping staged changes onto the worktree edit.
pub(crate) fn compute_worktree_line_diffs(staged: &[char], index: &[&str], worktree: &[&str], anchors: &[usize]) -> (Vec<char>, Vec<char>) {
    let diff = similar::TextDiff::from_slices(index, worktree);
    let ops = crate::line_changes::anchor_insertions(diff.ops(), anchors, |a, b| Some(worktree[a] == worktree[b]))
        .expect("uncancelled gutter alignment");
    let unstaged = line_diff_from_ops(index.len(), worktree.len(), &ops);
    let mut mapped = vec![' '; worktree.len().max(1)];
    for op in ops.iter() {
        if let similar::DiffOp::Equal { old_index, new_index, len } = *op
            && let Some(source) = staged.get(old_index..old_index + len)
        {
            mapped[new_index..new_index + len].copy_from_slice(source);
        }
    }
    for op in ops.iter().filter(|op| op.tag() != similar::DiffTag::Equal) {
        let Some(source) = staged.get(op.old_range()) else { continue; };
        let target = op.new_range();
        for (offset, &kind) in source.iter().enumerate().filter(|(_, kind)| **kind != ' ') {
            let line = (target.start + offset.min(target.len().saturating_sub(1)))
                .min(mapped.len() - 1);
            mapped[line] = merge_kind(mapped[line], kind);
        }
    }
    if index.is_empty() && staged.first() == Some(&'D') { mapped[0] = 'D'; }
    (mapped, unstaged)
}

pub(crate) fn target_line_markers_from_compare_diff(
    compare_diff_lines: &[(char, String)],
    target_line_count: usize,
) -> Vec<char> {
    let mut target_line_markers = vec![' '; target_line_count];
    let mut target_line = 0usize;
    for run in compare_diff_lines.chunk_by(|a, b| (a.0 == ' ') == (b.0 == ' ')) {
        if run[0].0 == ' ' {
            target_line += run.len();
            continue;
        }
        let additions = run.iter().filter(|(kind, _)| *kind == '+').count();
        let removed = run.iter().any(|(kind, _)| *kind == '-');
        if additions > 0 {
            let end = target_line + additions;
            target_line_markers[target_line.min(target_line_count)..end.min(target_line_count)]
                .fill(if removed { 'M' } else { 'A' });
            target_line = end;
        } else if removed {
            let anchor = target_line.min(target_line_count.saturating_sub(1));
            if let Some(marker) = target_line_markers.get_mut(anchor) {
                *marker = merge_kind(*marker, 'D');
            }
        }
    }
    target_line_markers
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct GitDiffHunk {
    pub(crate) old_start: usize,
    pub(crate) old_count: usize,
    pub(crate) new_start: usize,
    pub(crate) new_count: usize,
    pub(crate) text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitFilePatch {
    pub(crate) header: String,
    pub(crate) hunks: Vec<GitDiffHunk>,
}

pub(crate) fn parse_git_file_patch(diff_text: &str) -> Option<GitFilePatch> {
    let hunk_header = regex::Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@").ok()?;
    let mut header = String::new();
    let mut hunks = Vec::new();
    let mut current_meta: Option<(usize, usize, usize, usize)> = None;
    let mut current_text = String::new();

    // A CR before the patch's LF belongs to the file content. `lines()`
    // removes it and produces a patch that cannot apply to a CRLF file.
    for line in diff_text.split_terminator('\n') {
        if let Some(captures) = hunk_header.captures(line) {
            if let Some((old_start, old_count, new_start, new_count)) = current_meta.take() {
                hunks.push(GitDiffHunk {
                    old_start,
                    old_count,
                    new_start,
                    new_count,
                    text: current_text.clone(),
                });
                current_text.clear();
            }
            let old_start = captures.get(1)?.as_str().parse().ok()?;
            let old_count = captures
                .get(2)
                .map_or(1, |m| m.as_str().parse().unwrap_or(1));
            let new_start = captures.get(3)?.as_str().parse().ok()?;
            let new_count = captures
                .get(4)
                .map_or(1, |m| m.as_str().parse().unwrap_or(1));
            current_meta = Some((old_start, old_count, new_start, new_count));
        }

        if current_meta.is_some() {
            current_text.push_str(line);
            current_text.push('\n');
        } else {
            header.push_str(line);
            header.push('\n');
        }
    }

    if let Some((old_start, old_count, new_start, new_count)) = current_meta.take() {
        hunks.push(GitDiffHunk {
            old_start,
            old_count,
            new_start,
            new_count,
            text: current_text,
        });
    }

    if hunks.is_empty() {
        None
    } else {
        Some(GitFilePatch { header, hunks })
    }
}

pub(crate) fn line_number_in_hunk(line_number: usize, start: usize, count: usize) -> bool {
    if count == 0 {
        // A zero-length range is a boundary after `start`. The gutter marks
        // the following line, or the last remaining line for a deletion at EOF.
        line_number == start.saturating_add(1) || (start > 0 && line_number == start)
    } else {
        line_number >= start && line_number < start.saturating_add(count)
    }
}

pub(crate) fn select_patch_hunk(
    patch: &GitFilePatch,
    line_number: usize,
    use_new_range: bool,
) -> Option<String> {
    let hunk = patch.hunks.iter().find(|hunk| {
        if use_new_range {
            line_number_in_hunk(line_number, hunk.new_start, hunk.new_count)
        } else {
            line_number_in_hunk(line_number, hunk.old_start, hunk.old_count)
        }
    })?;
    Some(format!("{}{}", patch.header, hunk.text))
}

pub(crate) fn map_worktree_line_to_index_line(
    index_text: &str,
    worktree_text: &str,
    worktree_line: usize,
) -> Option<usize> {
    let diff = similar::TextDiff::from_lines(index_text, worktree_text);
    for op in diff.ops() {
        if let similar::DiffOp::Equal {
            old_index,
            new_index,
            len,
        } = *op
        {
            if worktree_line >= new_index && worktree_line < new_index + len {
                return Some(old_index + (worktree_line - new_index));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;

    // --- format_time_ago tests ---

    #[test]
    fn format_time_ago_seconds() {
        assert_eq!(App::format_time_ago(100, 70), "30s ago");
    }

    #[test]
    fn format_time_ago_zero_diff() {
        assert_eq!(App::format_time_ago(100, 100), "0s ago");
    }

    #[test]
    fn format_time_ago_negative_clamps() {
        // When then > now, diff should be clamped to 0
        assert_eq!(App::format_time_ago(50, 100), "0s ago");
    }

    #[test]
    fn format_time_ago_minutes() {
        assert_eq!(App::format_time_ago(1000, 1000 - 300), "5m ago");
    }

    #[test]
    fn format_time_ago_hours() {
        assert_eq!(App::format_time_ago(10000, 10000 - 7200), "2h ago");
    }

    #[test]
    fn format_time_ago_days() {
        assert_eq!(App::format_time_ago(100000, 100000 - 86400 * 3), "3d ago");
    }

    #[test]
    fn format_time_ago_months() {
        assert_eq!(App::format_time_ago(1000000, 1000000 - 86400 * 60), "2mo ago");
    }

    #[test]
    fn format_time_ago_years() {
        assert_eq!(App::format_time_ago(100000000, 100000000 - 86400 * 400), "1y ago");
    }

    // --- compute_line_diff tests ---

    #[test]
    fn compute_line_diff_identical() {
        let result = compute_line_diff(&["a", "b", "c"], &["a", "b", "c"]);
        assert_eq!(result, vec![' ', ' ', ' ']);
    }

    #[test]
    fn blank_insertion_anchors_project_staged_rows_past_the_inserted_line() {
        let index = ["one", "", "", "last"];
        let worktree = ["one", "", "", "", "last"];
        let (staged, unstaged) = compute_worktree_line_diffs(
            &[' ', 'A', 'M', ' '], &index, &worktree, &[1],
        );
        assert_eq!(staged, [' ', ' ', 'A', 'M', ' ']);
        assert_eq!(unstaged, [' ', 'A', ' ', ' ', ' ']);
    }

    #[test]
    fn blank_insertion_after_deletions_preserves_the_equal_rows_from_the_index() {
        let index = ["A", "", "B", "B", "A", "B", "", "B", ""];
        let worktree = ["B", "A", "", "", "", ""];
        let (staged, _) = compute_worktree_line_diffs(
            &[' ', ' ', ' ', ' ', ' ', ' ', ' ', ' ', 'A'], &index, &worktree, &[3, 4],
        );
        assert_eq!(staged, [' ', ' ', ' ', ' ', ' ', 'A']);
    }

    #[test]
    fn compute_line_diff_all_added() {
        let result = compute_line_diff(&[], &["x", "y"]);
        assert_eq!(result, vec!['A', 'A']);
    }

    #[test]
    fn compute_line_diff_all_deleted() {
        let result = compute_line_diff(&["a", "b"], &[]);
        assert_eq!(result, vec!['D']);
    }

    #[test]
    fn compute_line_diff_marks_deletion_next_to_remaining_line() {
        let result = compute_line_diff(&["a", "removed", "b"], &["a", "b"]);
        assert_eq!(result, vec![' ', 'D']);
    }

    #[test]
    fn compute_line_diff_modified() {
        let result = compute_line_diff(&["a", "b"], &["a", "x"]);
        assert_eq!(result[0], ' '); // "a" unchanged
        assert_eq!(result[1], 'M'); // "b" → "x" modified
    }

    #[test]
    fn compute_line_diff_mixed() {
        let result = compute_line_diff(&["a", "b", "c"], &["a", "x", "c", "d"]);
        assert_eq!(result[0], ' '); // unchanged
        assert_eq!(result[1], 'M'); // modified
        assert_eq!(result[2], ' '); // unchanged
        assert_eq!(result[3], 'A'); // added
    }

    #[test]
    fn compute_line_diff_empty_both() {
        let result = compute_line_diff(&[], &[]);
        assert!(result.is_empty());
    }

    // --- target_line_markers_from_compare_diff tests ---

    #[test]
    fn target_markers_addition() {
        let diff = vec![
            (' ', "context".to_string()),
            ('+', "added".to_string()),
        ];
        let markers = target_line_markers_from_compare_diff(&diff, 2);
        assert_eq!(markers, vec![' ', 'A']);
    }

    #[test]
    fn target_markers_deletion() {
        let diff = vec![
            (' ', "context".to_string()),
            ('-', "deleted".to_string()),
        ];
        let markers = target_line_markers_from_compare_diff(&diff, 2);
        assert_eq!(markers[1], 'D');
    }

    #[test]
    fn target_markers_empty_diff() {
        let markers = target_line_markers_from_compare_diff(&[], 3);
        assert_eq!(markers, vec![' ', ' ', ' ']);
    }

    #[test]
    fn target_markers_zero_lines() {
        let markers = target_line_markers_from_compare_diff(&[], 0);
        assert!(markers.is_empty());
    }

    // --- parse_git_file_patch tests ---

    #[test]
    fn parse_patch_single_hunk() {
        let diff = "diff --git a/foo.txt b/foo.txt\nindex abc..def 100644\n--- a/foo.txt\n+++ b/foo.txt\n@@ -1,3 +1,4 @@\n line1\n+added\n line2\n line3\n";
        let patch = parse_git_file_patch(diff).expect("should parse");
        assert_eq!(patch.hunks.len(), 1);
        assert_eq!(patch.hunks[0].old_start, 1);
        assert_eq!(patch.hunks[0].old_count, 3);
        assert_eq!(patch.hunks[0].new_start, 1);
        assert_eq!(patch.hunks[0].new_count, 4);
    }

    #[test]
    fn parse_patch_multiple_hunks() {
        let diff = "diff --git a/foo.txt b/foo.txt\n--- a/foo.txt\n+++ b/foo.txt\n@@ -1,2 +1,2 @@\n-old\n+new\n context\n@@ -10,3 +10,4 @@\n line10\n+inserted\n line11\n line12\n";
        let patch = parse_git_file_patch(diff).expect("should parse");
        assert_eq!(patch.hunks.len(), 2);
        assert_eq!(patch.hunks[0].old_start, 1);
        assert_eq!(patch.hunks[1].old_start, 10);
    }

    #[test]
    fn parse_patch_no_hunks() {
        let diff = "diff --git a/foo.txt b/foo.txt\nNo hunks here\n";
        assert!(parse_git_file_patch(diff).is_none());
    }

    #[test]
    fn parse_patch_single_line_hunk() {
        // @@ -5 +5 @@ means old_count=1, new_count=1 (implicit)
        let diff = "header\n@@ -5 +5 @@\n-old\n+new\n";
        let patch = parse_git_file_patch(diff).expect("should parse");
        assert_eq!(patch.hunks[0].old_count, 1);
        assert_eq!(patch.hunks[0].new_count, 1);
    }

    #[test]
    fn parse_patch_empty_input() {
        assert!(parse_git_file_patch("").is_none());
    }

    // --- line_number_in_hunk tests ---

    #[test]
    fn line_in_hunk_basic() {
        assert!(line_number_in_hunk(5, 5, 3)); // 5 is in [5, 8)
        assert!(line_number_in_hunk(7, 5, 3)); // 7 is in [5, 8)
        assert!(!line_number_in_hunk(8, 5, 3)); // 8 is NOT in [5, 8)
        assert!(!line_number_in_hunk(4, 5, 3)); // 4 is NOT in [5, 8)
    }

    #[test]
    fn line_in_hunk_zero_count() {
        // Empty ranges anchor to the following line, falling back at EOF.
        assert!(line_number_in_hunk(5, 5, 0));
        assert!(line_number_in_hunk(6, 5, 0));
        assert!(!line_number_in_hunk(4, 5, 0));
        assert!(line_number_in_hunk(1, 0, 0));
    }

    #[test]
    fn line_in_hunk_count_one() {
        assert!(line_number_in_hunk(10, 10, 1));
        assert!(!line_number_in_hunk(11, 10, 1));
    }

    // --- select_patch_hunk tests ---

    #[test]
    fn select_hunk_found() {
        let patch = GitFilePatch {
            header: "header\n".to_string(),
            hunks: vec![
                GitDiffHunk {
                    old_start: 1, old_count: 3,
                    new_start: 1, new_count: 4,
                    text: "hunk1\n".to_string(),
                },
                GitDiffHunk {
                    old_start: 10, old_count: 2,
                    new_start: 11, new_count: 3,
                    text: "hunk2\n".to_string(),
                },
            ],
        };
        let result = select_patch_hunk(&patch, 11, true).expect("should find");
        assert!(result.contains("header"));
        assert!(result.contains("hunk2"));
    }

    #[test]
    fn select_hunk_not_found() {
        let patch = GitFilePatch {
            header: "header\n".to_string(),
            hunks: vec![GitDiffHunk {
                old_start: 1, old_count: 3,
                new_start: 1, new_count: 3,
                text: "hunk\n".to_string(),
            }],
        };
        assert!(select_patch_hunk(&patch, 99, true).is_none());
    }

    #[test]
    fn select_hunk_uses_old_range() {
        let patch = GitFilePatch {
            header: "h\n".to_string(),
            hunks: vec![GitDiffHunk {
                old_start: 5, old_count: 2,
                new_start: 10, new_count: 3,
                text: "text\n".to_string(),
            }],
        };
        // Line 5 is in old range [5,7) but NOT in new range [10,13)
        assert!(select_patch_hunk(&patch, 5, false).is_some());
        assert!(select_patch_hunk(&patch, 5, true).is_none());
    }

    // --- map_worktree_line_to_index_line tests ---

    #[test]
    fn map_line_identical() {
        let text = "a\nb\nc\n";
        assert_eq!(map_worktree_line_to_index_line(text, text, 0), Some(0));
        assert_eq!(map_worktree_line_to_index_line(text, text, 1), Some(1));
        assert_eq!(map_worktree_line_to_index_line(text, text, 2), Some(2));
    }

    #[test]
    fn map_line_with_insertion() {
        let index = "a\nb\nc\n";
        let worktree = "a\ninserted\nb\nc\n";
        // worktree line 2 ("b") maps to index line 1
        assert_eq!(map_worktree_line_to_index_line(index, worktree, 2), Some(1));
    }

    #[test]
    fn map_line_with_deletion() {
        let index = "a\nb\nc\n";
        let worktree = "a\nc\n";
        // worktree line 1 ("c") maps to index line 2
        assert_eq!(map_worktree_line_to_index_line(index, worktree, 1), Some(2));
    }

    #[test]
    fn map_line_not_found() {
        let index = "a\nb\n";
        let worktree = "x\ny\n";
        // All lines changed, no equal lines
        assert!(map_worktree_line_to_index_line(index, worktree, 0).is_none());
    }
}
