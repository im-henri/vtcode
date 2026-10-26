use super::*;
use lsp_types::{Position, Range};

fn edit(line: u32, start: u32, end: u32, text: &str) -> TextEdit {
    TextEdit::new(
        Range::new(Position::new(line, start), Position::new(line, end)),
        text.into(),
    )
}

#[test]
fn lsp_edits_replace_unicode_and_last_character_with_one_undo() {
    for newline in ["\n", "\r\n"] {
        let source = format!("😀 name{newline}last");
        let mut editor = Editor::test_with_text(&source);
        let version = editor.content_version;
        assert_eq!(
            apply(&mut editor, &[edit(0, 3, 7, "renamed"), edit(1, 3, 4, "T")]),
            Ok(2)
        );
        assert_eq!(editor.rope.to_string(), format!("😀 renamed{newline}lasT"));
        assert_eq!(editor.content_version, version + 1);
        editor.undo();
        assert_eq!(editor.rope.to_string(), source);
        editor.redo();
        assert_eq!(editor.rope.to_string(), format!("😀 renamed{newline}lasT"));
    }
}

#[test]
fn lsp_edits_support_empty_document_and_eof_insertions() {
    for source in ["", "abc", "abc\n"] {
        let mut editor = Editor::test_with_text(source);
        let line = editor.rope.len_lines() - 1;
        let column = lsp_positions::to_utf16(&editor.rope, line, usize::MAX);
        apply(&mut editor, &[edit(line as u32, column, column, "終😀")]).unwrap();
        assert_eq!(editor.rope.to_string(), format!("{source}終😀"));
    }
}

#[test]
fn lsp_edits_evaluate_all_ranges_before_modifying_multiline_document() {
    let mut editor = Editor::test_with_text("first\nsecond\nthird");
    editor.set_cursor(2, 5);
    apply(
        &mut editor,
        &[
            edit(0, 0, 0, "import x\n"),
            edit(1, 0, 6, "two\nlines"),
            edit(2, 0, 5, "THIRD"),
        ],
    )
    .unwrap();
    assert_eq!(
        editor.rope.to_string(),
        "import x\nfirst\ntwo\nlines\nTHIRD"
    );
    assert_eq!((editor.cursor_line, editor.cursor_col), (4, 5));
}

#[test]
fn lsp_edits_reject_invalid_or_overlapping_batches_without_partial_changes() {
    for edits in [
        vec![edit(0, 1, 3, "ok"), edit(9, 0, 0, "bad")],
        vec![edit(0, 1, 4, "a"), edit(0, 3, 5, "b")],
        vec![edit(0, 4, 2, "bad")],
    ] {
        let mut editor = Editor::test_with_text("abcdef");
        let version = editor.content_version;
        assert!(apply(&mut editor, &edits).is_err());
        assert_eq!(editor.rope.to_string(), "abcdef");
        assert_eq!(editor.content_version, version);
        assert!(!editor.modified);
    }
}

#[test]
fn lsp_edits_respect_read_only_editors_and_empty_responses() {
    let mut editor = Editor::test_with_text("abc");
    let version = editor.content_version;
    assert_eq!(apply(&mut editor, &[]), Ok(0));
    assert_eq!(editor.content_version, version);
    editor.read_only = true;
    assert!(apply(&mut editor, &[edit(0, 0, 3, "new")]).is_err());
    assert_eq!(editor.rope.to_string(), "abc");
}

fn completion(snippet: bool) -> crate::lsp::LspCompletionItem {
    crate::lsp::LspCompletionItem {
        label: "value".into(),
        detail: None,
        insert_text: if snippet {
            "value(${1:arg})$0"
        } else {
            "value"
        }
        .into(),
        kind: None,
        text_edit_range: Some(((1, 3), (1, 6))),
        additional_text_edits: vec![
            edit(0, 0, 0, "from module import value\n"),
            edit(2, 0, 3, "END"),
        ],
        is_snippet: snippet,
        original_item: None,
        documentation: None,
    }
}

#[test]
fn lsp_completion_auto_imports_and_following_edits_preserve_cursor_and_undo() {
    let source = "# file\n😀 val\nend";
    let mut editor = Editor::test_with_text(source);
    complete(&mut editor, &completion(false)).unwrap();
    assert_eq!(
        editor.rope.to_string(),
        "from module import value\n# file\n😀 value\nEND"
    );
    assert_eq!((editor.cursor_line, editor.cursor_col), (2, 7));
    editor.undo();
    assert_eq!(editor.rope.to_string(), source);
}

#[test]
fn lsp_completion_auto_imports_preserve_snippet_placeholders() {
    let mut editor = Editor::test_with_text("# file\n😀 val\nend");
    complete(&mut editor, &completion(true)).unwrap();
    assert_eq!(
        editor.rope.to_string(),
        "from module import value\n# file\n😀 value(arg)\nEND"
    );
    assert_eq!((editor.cursor_line, editor.cursor_col), (2, 11));
    assert_eq!(editor.selection_anchor, Some((2, 8)));
    assert!(editor.snippet_next_tab_stop());
    assert_eq!((editor.cursor_line, editor.cursor_col), (2, 12));
}

#[test]
fn lsp_completion_rejects_overlapping_additional_edits() {
    let mut editor = Editor::test_with_text("# file\n😀 val\nend");
    let original = editor.rope.clone();
    let mut item = completion(false);
    item.additional_text_edits.push(edit(1, 3, 6, "oops"));
    assert!(complete(&mut editor, &item).is_err());
    assert_eq!(editor.rope, original);
}
