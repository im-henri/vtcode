use super::Editor;
use crate::editor_line_ops::{split_line_ending, split_line_text};
use std::borrow::Cow;

fn reference(text: &str, operation: usize) -> String {
    let rope = ropey::Rope::from_str(text);
    let mut lines: Vec<_> = rope.lines().map(|line| line.to_string()).collect();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    if lines.is_empty() {
        return text.into();
    }
    let mut order: Vec<_> = (0..lines.len()).collect();
    match operation {
        0 => order.reverse(),
        1 => order.retain(|&row| !split_line_ending(&lines[row]).0.trim().is_empty()),
        2 => {
            let mut seen = std::collections::HashSet::new();
            order.retain(|&row| seen.insert(split_line_ending(&lines[row]).0));
        }
        _ => unreachable!(),
    }
    let last = split_line_ending(lines.last().unwrap()).1;
    let mut result = String::new();
    for (row, &original) in order.iter().enumerate() {
        result.push_str(split_line_ending(&lines[original]).0);
        result.push_str(if row + 1 == order.len() {
            last
        } else {
            split_line_ending(&lines[if operation == 0 { row } else { original }]).1
        });
    }
    if order.is_empty() {
        result.push_str(last);
    }
    result
}

#[test]
fn line_text_borrows_contiguous_source_and_preserves_fragmented_endings() {
    for text in [
        String::new(),
        "body\r\n".into(),
        "界é\r".into(),
        "\n".into(),
        "last".into(),
        format!("{}\r\n", "界é ".repeat(4_000)),
    ] {
        let rope = ropey::Rope::from_str(&text);
        for line in rope.lines() {
            let (body, ending) = split_line_text(line);
            let text = line.to_string();
            assert_eq!((body.as_ref(), ending), split_line_ending(&text));
            assert_eq!(matches!(body, Cow::Borrowed(_)), line.as_str().is_some());
        }
    }
}

#[test]
fn line_commands_match_reference_with_unicode_mixed_separators_and_history() {
    let generated: String = (0..500)
        .map(|index| {
            let content = ["", " \t", "界é", "alpha", "alpha", "last"][index % 6];
            let ending = ["\n", "\r\n", "\r"][index % 3];
            format!("{content}{ending}")
        })
        .collect();
    for source in [
        String::new(),
        "\n\r\n\r".into(),
        "a\r\nb\rc\na".into(),
        "界\n界\r\n\t\r\nlast".into(),
        generated,
        format!("{}\r\nshort\n{}", "界é".repeat(5_000), "界é".repeat(5_000)),
    ] {
        for (index, operation) in [
            Editor::reverse_lines as fn(&mut Editor),
            Editor::remove_blank_lines,
            Editor::remove_duplicate_lines,
        ]
        .into_iter()
        .enumerate()
        {
            let mut editor = Editor::test_with_text(&source);
            editor.record_saved_snapshot(editor.rope.clone());
            let expected = reference(&source, index);
            operation(&mut editor);
            assert_eq!(editor.rope.to_string(), expected, "operation={index}");
            if expected != source {
                assert!(editor.modified);
                editor.undo();
                assert_eq!(editor.rope.to_string(), source);
                assert!(!editor.modified);
                editor.redo();
                assert_eq!(editor.rope.to_string(), expected);
            } else {
                assert!(!editor.can_undo());
            }
        }
    }
}

#[test]
fn no_op_line_commands_preserve_redo_and_document_revision() {
    for (source, operation) in [
        (
            "let café = 1;\nother();\n",
            Editor::remove_blank_lines as fn(&mut Editor),
        ),
        ("let café = 1;\nother();\n", Editor::remove_duplicate_lines),
        ("use a;\r\nuse z;", Editor::sort_imports),
        ("let café = 1;\nother();\n", Editor::sort_imports),
    ] {
        let mut editor = Editor::test_with_text(source);
        editor.insert_char('x');
        editor.undo();
        let version = editor.content_version;
        let history = editor.undo_history_key();
        operation(&mut editor);
        assert_eq!(editor.content_version, version);
        assert_eq!(editor.undo_history_key(), history);
        assert!(editor.can_redo());
        editor.redo();
        assert_eq!(editor.rope.to_string(), format!("x{source}"));
    }
}

#[test]
fn sort_imports_handles_long_non_imports_multiple_blocks_and_unsaved_shared_views() {
    let source = format!(
        "use z;\r\nuse a;\nlet 界 = 0;\r\n{}\n\timport z\n\timport a\r\n\n{}use z;\r{}use a;",
        "x".repeat(50_000),
        " ".repeat(3_000),
        " ".repeat(3_000)
    );
    let expected = format!(
        "use a;\r\nuse z;\nlet 界 = 0;\r\n{}\n\timport a\n\timport z\r\n\n{}use a;\r{}use z;",
        "x".repeat(50_000),
        " ".repeat(3_000),
        " ".repeat(3_000)
    );
    let mut editor = Editor::test_with_text(&source);
    editor.record_saved_snapshot(editor.rope.clone());
    let mut other = Editor::new();
    other.copy_document_from(&editor);
    editor.sort_imports();
    other.refresh_document();
    assert_eq!(editor.rope.to_string(), expected);
    assert_eq!(other.rope, editor.rope);
    other.undo();
    editor.refresh_document();
    assert_eq!(editor.rope.to_string(), source);
    assert!(!editor.modified);
    editor.redo();
    assert_eq!(editor.rope.to_string(), expected);
}
