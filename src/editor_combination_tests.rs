use super::Editor;
use crate::{app::{App, Tab}, app_lsp_edits};
use lsp_types::{Position, Range, TextEdit};

fn edit(start: (u32, u32), end: (u32, u32), text: &str) -> TextEdit {
    TextEdit::new(Range::new(Position::new(start.0, start.1), Position::new(end.0, end.1)), text.into())
}

fn saved(text: &str) -> Editor {
    let mut editor = Editor::test_with_text(text);
    editor.record_saved_snapshot(editor.rope.clone());
    editor
}

#[test]
fn combination_unchanged_lsp_formatting_preserves_redo_selection_and_clean_state() {
    let mut editor = saved("αβ\r\nlast");
    editor.insert_char('X');
    editor.undo();
    editor.selection_anchor = Some((0, 0));
    editor.cursor_col = 2;
    let version = editor.content_version;
    assert_eq!(app_lsp_edits::apply(&mut editor, &[
        edit((0, 0), (0, 2), "αβ"), edit((1, 4), (1, 4), ""),
    ]), Ok(0));
    assert_eq!(editor.content_version, version);
    assert!(!editor.modified);
    assert_eq!(editor.selection_anchor, Some((0, 0)));
    assert!(!editor.can_undo());
    editor.redo();
    assert_eq!(editor.rope, "Xαβ\r\nlast");
}

#[test]
fn combination_lsp_formatting_multicursor_typing_and_history_keep_every_caret() {
    for ending in ["\n", "\r\n"] {
        let source = ["head", "one", "two", "last"].join(ending);
        let mut editor = saved(&source);
        (editor.cursor_line, editor.cursor_col) = (1, 3);
        editor.selection_anchor = Some((1, 0));
        editor.extra_cursors = vec![(3, 4)];
        editor.extra_cursor_anchors = vec![Some((3, 0))];
        app_lsp_edits::apply(&mut editor, &[edit((0, 0), (1, 0), "")]).unwrap();
        assert_eq!((editor.cursor_line, editor.cursor_col), (0, 3));
        assert_eq!(editor.extra_cursors, [(2, 4)]);
        assert!(editor.extra_cursor_anchors.iter().all(Option::is_none));
        editor.multi_cursor_insert_char('X');
        let after = ["oneX", "two", "lastX"].join(ending);
        assert_eq!(editor.rope, after);
        editor.undo();
        assert_eq!(editor.rope, ["one", "two", "last"].join(ending));
        editor.undo();
        assert_eq!(editor.rope, source);
        assert_eq!(editor.selection_anchor, Some((1, 0)));
        assert_eq!(editor.extra_cursor_anchors, [Some((3, 0))]);
        assert!(!editor.modified);
        editor.redo();
        editor.redo();
        assert_eq!(editor.rope, after);
    }
}

#[test]
fn combination_virtual_caret_and_lsp_edit_do_not_move_to_another_line() {
    let mut editor = saved("ab\nnext");
    editor.cursor_col = 30;
    app_lsp_edits::apply(&mut editor, &[edit((0, 0), (0, 0), "Z")]).unwrap();
    assert_eq!((editor.cursor_line, editor.cursor_col), (0, 3));
    editor.insert_char('!');
    assert_eq!(editor.rope, "Zab!\nnext");
}

fn folded() -> Editor {
    let mut editor = saved("head\nfn f() {\n one();\n two();\n}\nend");
    editor.folded_ranges.insert(1, 4);
    editor
}

#[test]
fn combination_lsp_edit_above_fold_does_not_hide_unrelated_lines() {
    let mut editor = folded();
    app_lsp_edits::apply(&mut editor, &[edit((0, 0), (1, 0), "")]).unwrap();
    assert!(editor.folded_ranges.is_empty());
    editor.undo();
    assert_eq!(editor.rope, "head\nfn f() {\n one();\n two();\n}\nend");
    editor.redo();
    assert!(editor.folded_ranges.is_empty());
}

#[test]
fn combination_external_formatting_above_fold_does_not_hide_unrelated_lines() {
    let mut editor = folded();
    let result = crate::formatters::prepare(&editor.rope, "fn f() {\n one();\n two();\n}\nend");
    result.apply(&mut editor).unwrap();
    assert!(editor.folded_ranges.is_empty());
    editor.undo();
    editor.redo();
    assert!(editor.folded_ranges.is_empty());
}

#[test]
fn combination_split_edit_clears_stale_folds_and_clamps_all_selection_endpoints() {
    let mut app = App::test_new();
    let mut editor = folded();
    editor.file_path = Some("combination.rs".into());
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.duplicate_tab(0);
    app.create_split_group(1);
    let peer = 1 - app.active_group;
    let editor = &mut app.groups[peer].tabs[0].editor;
    editor.cursor_line = 5;
    editor.cursor_col = 3;
    editor.selection_anchor = Some((3, 7));
    editor.extra_cursors = vec![(4, 1)];
    editor.extra_cursor_anchors = vec![Some((2, 7))];
    let editor = app.editor_mut().unwrap();
    editor.selection_anchor = Some((0, 0));
    (editor.cursor_line, editor.cursor_col) = (5, 3);
    editor.paste_plain_with_text("a\nb");
    app.sync_duplicate_file_tabs();
    let editor = &app.groups[peer].tabs[0].editor;
    assert!(editor.folded_ranges.is_empty());
    for state in editor.collect_multi_cursors() {
        assert!(state.line < editor.line_count());
        assert!(state.col <= editor.line_len(state.line));
        if let Some((line, col)) = state.anchor {
            assert!(line < editor.line_count() && col <= editor.line_len(line));
        }
    }
    app.switch_group(peer);
    app.editor_mut().unwrap().multi_cursor_insert_char('X');
    app.editor_mut().unwrap().undo();
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope, "head\nfn f() {\n one();\n two();\n}\nend");
    assert!(!app.editor().unwrap().modified);
}

#[test]
fn combination_snippet_replacement_paste_and_tab_navigation_follow_the_edited_text() {
    for replacement in ["longer_name", "界😀", "multi\r\nline"] {
        let mut editor = saved("");
        editor.save_undo();
        editor.insert_snippet_text(0, "${1:name}(${2:arg})\n$0");
        editor.paste_plain_with_text(replacement);
        assert!(editor.snippet_next_tab_stop());
        assert_eq!(editor.selected_text().as_deref(), Some("arg"), "{replacement:?}");
        editor.insert_char_replacing_selection('X');
        assert!(editor.snippet_prev_tab_stop());
        assert_eq!(editor.selected_text(), Some(replacement.replace("\r\n", "\n")));
        assert!(editor.snippet_next_tab_stop());
        assert_eq!(editor.selected_text().as_deref(), Some("X"));
        assert!(editor.snippet_next_tab_stop());
        assert_eq!((editor.cursor_line, editor.cursor_col), (editor.line_count() - 1, 0));
        editor.undo();
        assert!(!editor.snippet_active);
        editor.redo();
        assert!(!editor.snippet_active);
    }
}

#[test]
fn combination_snippet_eof_placeholders_and_followup_typing_stay_in_bounds() {
    for body in ["one\n$0", "${1:one\n}$0", "one\r\n$0"] {
        let mut editor = saved("");
        editor.save_undo();
        editor.insert_snippet_text(0, body);
        if editor.has_selection() { editor.snippet_next_tab_stop(); }
        assert_eq!((editor.cursor_line, editor.cursor_col), (1, 0), "{body}");
        editor.insert_char_replacing_selection('X');
        assert_eq!(editor.rope.line(1).to_string(), "X");
        editor.undo();
        editor.undo();
        assert_eq!(editor.rope, "");
    }
}

#[test]
fn combination_reload_cancels_snippet_navigation_but_keeps_undo() {
    let mut editor = saved("fn");
    editor.file_path = Some("snippet.rs".into());
    editor.cursor_col = 2;
    assert!(editor.try_expand_snippet());
    let expanded = editor.rope.clone();
    editor.apply_reloaded_file(super::LoadedTextFile {
            remote_checkpoint: None,
        rope: ropey::Rope::from_str("x"), encoding: "UTF-8", file_size: 1,
        last_disk_modified: None,
    });
    assert!(!editor.snippet_active);
    assert!(!editor.snippet_next_tab_stop());
    editor.undo();
    assert_eq!(editor.rope, expanded);
    assert!(editor.modified);
    editor.redo();
    assert_eq!(editor.rope, "x");
    assert!(!editor.modified);
}

#[test]
fn combination_snippet_repeated_boundary_text_does_not_confuse_placeholder_relocation() {
    for (unit, replacement) in [("a", "aa"), ("界", "界界"), ("😀", ""), ("", "xyz")] {
        let mut editor = saved("");
        editor.save_undo();
        editor.insert_snippet_text(0, &format!("${{1:{unit}}}{unit}${{2:arg}}$0"));
        if replacement.is_empty() { editor.backspace_replacing_selection(); }
        else { editor.paste_plain_with_text(replacement); }
        assert!(editor.snippet_next_tab_stop(), "{unit:?}, {replacement:?}");
        assert_eq!(editor.selected_text().as_deref(), Some("arg"));
        assert!(editor.snippet_prev_tab_stop());
        assert_eq!(editor.selected_text().unwrap_or_default(), replacement);
    }
}

#[test]
fn combination_snippet_tab_movement_separates_typing_history() {
    let mut editor = saved("");
    editor.save_undo();
    editor.insert_snippet_text(0, "$1 $2 $0");
    editor.insert_char('a');
    assert!(editor.snippet_next_tab_stop());
    editor.insert_char('b');
    assert_eq!(editor.rope, "a b ");
    editor.undo();
    assert_eq!(editor.rope, "a  ");
    editor.undo();
    assert_eq!(editor.rope, "  ");
    editor.undo();
    assert_eq!(editor.rope, "");
    assert!(!editor.modified);
}

#[test]
fn combination_snippet_navigation_after_editing_elsewhere_cancels_stale_positions() {
    for replacement in ["X", "different\ntext\n", ""] {
        let mut editor = saved("prefix ");
        editor.save_undo();
        editor.insert_snippet_text(7, "${1:name} ${2:arg}");
        editor.selection_anchor = Some((0, 0));
        editor.cursor_col = 6;
        if replacement.is_empty() { editor.backspace_replacing_selection(); }
        else { editor.paste_plain_with_text(replacement); }
        let text = editor.rope.clone();
        assert!(!editor.snippet_next_tab_stop());
        assert!(!editor.snippet_active);
        assert!(editor.snippet_rope.is_none());
        assert_eq!(editor.rope, text);
        editor.undo();
        editor.redo();
        assert_eq!(editor.rope, text);
    }
}

#[test]
fn combination_formatting_back_to_saved_text_updates_dirty_state_without_losing_undo() {
    for external in [false, true] {
        for change_encoding in [false, true] {
            let mut editor = saved("let x = 1;\n");
            editor.selection_anchor = Some((0, 0));
            (editor.cursor_line, editor.cursor_col) = (1, 0);
            editor.paste_plain_with_text("let x=1;\n");
            if change_encoding { editor.set_encoding("UTF-16 LE"); }
            if external {
                crate::formatters::prepare(&editor.rope, "let x = 1;\n").apply(&mut editor).unwrap();
            } else {
                app_lsp_edits::apply(&mut editor, &[edit((0, 0), (0, 8), "let x = 1;")]).unwrap();
            }
            assert_eq!(editor.rope, "let x = 1;\n");
            assert_eq!(editor.modified, change_encoding);
            editor.undo();
            assert_eq!(editor.rope, "let x=1;\n");
            assert!(editor.modified);
            editor.redo();
            assert_eq!(editor.rope, "let x = 1;\n");
            assert_eq!(editor.modified, change_encoding);
        }
    }
}

#[test]
fn combination_inline_formatting_preserves_valid_folds() {
    for external in [false, true] {
        let mut editor = folded();
        if external {
            crate::formatters::prepare(&editor.rope, "HEAD\nfn f() {\n one();\n two();\n}\nend")
                .apply(&mut editor).unwrap();
        } else {
            app_lsp_edits::apply(&mut editor, &[edit((0, 0), (0, 4), "HEAD")]).unwrap();
        }
        assert_eq!(editor.folded_ranges.get(&1), Some(&4));
    }
}

#[test]
fn combination_unicode_change_ranges_rebuild_exact_text_at_character_boundaries() {
    for prefix in ["", "same\r\n", "😀界"] {
        for before in ["", "é", "ê", "界", "😀", "\r\n", "\n"] {
            for after in ["", "ê", "é", "世", "😁", "\n", "\r\n"] {
                let old = ropey::Rope::from_str(&format!("{prefix}{before}suffix"));
                let new = ropey::Rope::from_str(&format!("{prefix}{after}suffix"));
                let (old_range, new_range) = crate::rope_changes::changed_chars(&old, &new);
                let mut rebuilt = old.clone();
                rebuilt.remove(old_range.clone());
                rebuilt.insert(old_range.start, &new.slice(new_range).to_string());
                assert_eq!(rebuilt, new, "{prefix:?} {before:?} -> {after:?}");
            }
        }
    }
}

#[test]
fn combination_completion_imports_relocate_secondary_carets_and_retire_old_snippets() {
    for snippet in [false, true] {
        let mut editor = saved("");
        editor.save_undo();
        editor.insert_snippet_text(0, "${1:name}\n${2:keep}");
        editor.extra_cursors = vec![(1, 4)];
        editor.folded_ranges.insert(0, 1);
        let item = crate::lsp::LspCompletionItem {
            label: "call".into(), detail: None, kind: None, documentation: None,
            insert_text: if snippet { "call(${1:arg})$0" } else { "call" }.into(),
            text_edit_range: Some(((0, 0), (0, 4))),
            additional_text_edits: vec![edit((0, 0), (0, 0), "import x\n")],
            is_snippet: snippet, original_item: None,
        };
        app_lsp_edits::complete(&mut editor, &item).unwrap();
        assert_eq!(editor.extra_cursors, [(2, 4)]);
        assert!(editor.folded_ranges.is_empty());
        assert_eq!(editor.snippet_active, snippet);
        if snippet { assert!(editor.snippet_next_tab_stop()); }
        else { assert!(!editor.snippet_next_tab_stop()); }
        editor.multi_cursor_insert_char('X');
        assert_eq!(editor.rope, if snippet { "import x\ncall(arg)X\nkeepX" } else { "import x\ncallX\nkeepX" });
        editor.undo();
        editor.undo();
        assert_eq!(editor.rope, "name\nkeep");
        editor.undo();
        assert_eq!(editor.rope, "");
    }
}

#[test]
fn combination_completion_at_virtual_column_stays_on_its_line_after_imports() {
    for ending in ["\n", "\r\n"] {
        let source = format!("va{ending}next");
        let mut editor = saved(&source);
        editor.cursor_col = 99;
        editor.extra_cursors = vec![(1, 4)];
        editor.extra_cursor_anchors = vec![Some((1, 0))];
        let item = crate::lsp::LspCompletionItem {
            label: "value".into(), detail: None, kind: None, documentation: None,
            insert_text: "value".into(), text_edit_range: None,
            additional_text_edits: vec![edit((0, 0), (0, 0), &format!("# 😀 import{ending}"))],
            is_snippet: false, original_item: None,
        };
        app_lsp_edits::complete(&mut editor, &item).unwrap();
        assert_eq!(editor.rope, format!("# 😀 import{ending}value{ending}next"));
        assert_eq!((editor.cursor_line, editor.cursor_col), (1, 5));
        assert_eq!(editor.extra_cursors, [(2, 4)]);
        assert_eq!(editor.extra_cursor_anchors, [Some((2, 0))]);
        editor.multi_cursor_insert_char('!');
        assert_eq!(editor.rope, format!("# 😀 import{ending}value!{ending}!"));
        editor.undo();
        editor.undo();
        assert_eq!(editor.rope, source);
        assert!(!editor.modified);
    }
}
