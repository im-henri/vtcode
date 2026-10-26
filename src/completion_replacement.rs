//! Treat retained text at the caret as an explicit constraint on insertion.
//! Replacement remains available when no insertion fits, and via Ctrl+Space.
use super::{VtCompleteSnapshot, CompletionResult, identifier_char};

pub(super) fn refine(
    result: &mut CompletionResult,
    snapshot: &VtCompleteSnapshot,
    prefix: &str,
    explicit: bool,
) {
    if prefix.len() > 160 {
        return;
    }
    let Some(line) = snapshot.rope.get_line(snapshot.cursor_line) else {
        return;
    };
    let col = snapshot.cursor_col.min(line.len_chars());
    let mut tail = String::new();
    let mut end = col;
    for ch in line.chars_at(col) {
        if !identifier_char(ch) {
            break;
        }
        if tail.len() + ch.len_utf8() > 160 {
            return;
        }
        tail.push(ch);
        end += 1;
    }
    let existing = format!("{prefix}{tail}");
    let existing_call = line
        .chars_at(end)
        .take(17)
        .find(|ch| !matches!(ch, ' ' | '\t'))
        == Some('(');
    // A suggestion that only re-inserts the existing identifier is no edit.
    // Appending () remains useful unless the opening delimiter already exists.
    let mut i = 0;
    while i < result.suggestions.len() {
        if result.details[i].kind == crate::app_vtcomplete::CompletionKind::Snippet {
            i += 1;
            continue;
        }
        let word = &result.suggestions[i];
        let unchanged = if let Some(name) = word.strip_suffix("()") {
            name == existing && existing_call
        } else {
            word == &existing
        };
        if unchanged {
            result.suggestions.remove(i);
            result.details.remove(i);
        } else {
            i += 1;
        }
    }
    // A nonempty prefix and suffix describe a fill-in-the-middle query. If an
    // admitted answer preserves both, automatic completion should not discard
    // the user's retained suffix. Deliberate historical transformations may
    // replace that suffix, and explicit requests keep all replacement choices.
    let preserves = |word: &str| {
        let word = word.strip_suffix("()").unwrap_or(word);
        word.starts_with(prefix) && word.ends_with(&tail) && word.len() >= prefix.len() + tail.len()
    };
    if explicit
        || prefix.is_empty()
        || tail.chars().count() < 2
        || !result.suggestions.iter().any(|word| preserves(word))
    {
        return;
    }
    let mut i = 0;
    while i < result.suggestions.len() {
        if preserves(&result.suggestions[i]) {
            if !matches!(
                result.details[i].annotation,
                "edit" | "number" | "number sequence"
            ) {
                result.details[i].annotation = "suffix";
            }
            i += 1;
        } else if matches!(
            result.details[i].annotation,
            "edit" | "number" | "number sequence"
        ) {
            i += 1;
        } else {
            result.suggestions.remove(i);
            result.details.remove(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_vtcomplete::CompletionDetail;

    fn run(source: &str, prefix: &str, words: &[&str], explicit: bool) -> Vec<String> {
        let (left, right) = source.split_once('|').unwrap();
        let snapshot = VtCompleteSnapshot {
            source: Default::default(),
            rope: ropey::Rope::from_str(&format!("{left}{right}")),
            document_id: 1,
            cursor_line: 0,
            cursor_col: left.chars().count(),
            active: true,
        };
        let mut result = CompletionResult {
            suggestions: words.iter().map(|s| s.to_string()).collect(),
            details: vec![CompletionDetail::default(); words.len()],
            outcome: Default::default(),
            deferred_candidates: 0,
        };
        refine(&mut result, &snapshot, prefix, explicit);
        assert_eq!(result.suggestions.len(), result.details.len());
        result.suggestions
    }

    #[test]
    fn preserves_retained_suffix_when_a_real_insertion_is_available() {
        let words = ["readCache", "readConfig", "readDiskCache"];
        assert_eq!(run("read|Cache", "read", &words, false), ["readDiskCache"]);
        assert_eq!(
            run("read|Cache", "read", &words, true),
            ["readConfig", "readDiskCache"]
        );
        assert_eq!(run("read|Old", "read", &words, false), words);
        assert_eq!(
            run("loe|Andmed", "loe", &["loeAndmed", "loeFailiAndmed"], false),
            ["loeFailiAndmed"]
        );
    }

    #[test]
    fn avoids_no_op_calls_but_keeps_new_parentheses() {
        assert!(run("read|Cache(value)", "read", &["readCache()"], false).is_empty());
        assert!(run("readCache| (value)", "readCache", &["readCache()"], false).is_empty());
        assert_eq!(
            run("readCache|", "readCache", &["readCache()"], false),
            ["readCache()"]
        );
        assert_eq!(run("|", "", &["readCache"], false), ["readCache"]);
    }

    #[test]
    fn large_suffixes_do_not_trigger_unbounded_scans() {
        let source = format!("read|{}", "x".repeat(100_000));
        assert_eq!(run(&source, "read", &["readCache"], false), ["readCache"]);
    }

    #[test]
    fn verified_numeric_replacement_survives_an_identifier_suffix_match() {
        let snapshot = VtCompleteSnapshot {
            source: Default::default(),
            rope: ropey::Rope::from_str("wait(499)"),
            document_id: 1,
            cursor_line: 0,
            cursor_col: 6,
            active: true,
        };
        let mut result = CompletionResult {
            suggestions: vec!["400".into(), "4_99".into()],
            details: vec![
                CompletionDetail {
                    kind: crate::app_vtcomplete::CompletionKind::Value,
                    annotation: "number sequence",
                    ..Default::default()
                },
                CompletionDetail::default(),
            ],
            outcome: Default::default(),
            deferred_candidates: 0,
        };
        refine(&mut result, &snapshot, "4", false);
        assert_eq!(result.suggestions, ["400", "4_99"]);
        assert_eq!(result.details[0].annotation, "number sequence");
        result.suggestions = vec!["4499".into(), "4_99".into()];
        refine(&mut result, &snapshot, "4", false);
        assert_eq!(result.details[0].annotation, "number sequence");
    }
}
