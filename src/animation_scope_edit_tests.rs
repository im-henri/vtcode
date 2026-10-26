use super::*;
use crate::{background::BracketDepthIndex, editor::Editor};

fn editor(source: &str, cursor: (usize, usize)) -> Editor {
    let mut editor = Editor::test_with_text(source);
    editor.language_override = Some("Rust".into());
    editor.cursor_line = cursor.0;
    editor.cursor_col = cursor.1;
    editor
}

fn pair(editor: &Editor, first: usize, last: usize) -> ScopePair {
    (
        (
            first,
            editor
                .rope
                .line(first)
                .chars()
                .position(|ch| ch == '{')
                .unwrap(),
        ),
        (
            last,
            editor
                .rope
                .line(last)
                .chars()
                .position(|ch| ch == '}')
                .unwrap(),
        ),
    )
}

fn index(editor: &Editor) -> BracketDepthIndex {
    BracketDepthIndex::build(
        editor.rope.clone(),
        editor.navigation_id,
        editor.content_version,
        editor.wrap_revision,
    )
}

fn edit(editor: &mut Editor, start: usize, end: usize, replacement: &str) {
    editor.rope.remove(start..end);
    editor.rope.insert(start, replacement);
    editor.content_version += 1;
    editor.wrap_revision += 1;
    let cursor = start + replacement.chars().count();
    editor.cursor_line = editor.rope.char_to_line(cursor);
    editor.cursor_col = cursor - editor.rope.line_to_char(editor.cursor_line);
}

fn snapshot(editor: &Editor) -> Snapshot {
    Snapshot {
        id: editor.navigation_id,
        rope: editor.rope.clone(),
        cursor: (editor.cursor_line, editor.cursor_col),
        offset: editor.rope.line_to_char(editor.cursor_line) + editor.cursor_col,
        content_version: editor.content_version,
        undo_revision: 0,
        scroll: (0, 0, 0),
        selection: None,
        area: Rect::new(0, 0, 80, 20),
    }
}

#[test]
fn bracket_depth_stays_stable_while_worker_lags_typing_deletion_and_line_shifts() {
    let mut editor = editor(
        "fn outer() {\n    fn inner() -> bool {\n        value\n    }\n}\n",
        (2, 10),
    );
    let stale = index(&editor);
    let mut state = AnimationState::default();
    assert_eq!(
        state.scope_bracket_depth(&editor, pair(&editor, 1, 3), Some(&stale)),
        Some(1)
    );
    let position = editor.rope.line_to_char(2) + 10;
    edit(&mut editor, position, position, "x");
    assert!(!stale.matches(&editor));
    assert_eq!(
        state.scope_bracket_depth(&editor, pair(&editor, 1, 3), Some(&stale)),
        Some(1)
    );
    edit(&mut editor, position, position + 1, "");
    assert_eq!(
        state.scope_bracket_depth(&editor, pair(&editor, 1, 3), None),
        Some(1)
    );
    edit(&mut editor, 0, 0, "\n");
    assert_eq!(
        state.scope_bracket_depth(&editor, pair(&editor, 2, 4), Some(&stale)),
        Some(1)
    );
    edit(&mut editor, 0, 1, "");
    assert_eq!(
        state.scope_bracket_depth(&editor, pair(&editor, 1, 3), None),
        Some(1)
    );
}

#[test]
fn bracket_depth_keeps_quoted_body_edits_but_rejects_nesting_changes_before_opener() {
    let mut editor = editor(
        "{\n    {\n        let value = \"text\";\n    }\n}\n",
        (2, 24),
    );
    let mut state = AnimationState::default();
    assert_eq!(
        state.scope_bracket_depth(&editor, pair(&editor, 1, 3), Some(&index(&editor))),
        Some(1)
    );
    let position = editor.rope.line_to_char(2) + 23;
    edit(&mut editor, position, position, "(more)");
    assert_eq!(
        state.scope_bracket_depth(&editor, pair(&editor, 1, 3), None),
        Some(1)
    );
    edit(&mut editor, 0, 0, "(");
    assert_eq!(
        state.scope_bracket_depth(&editor, pair(&editor, 1, 3), None),
        None
    );
    assert_eq!(
        state.scope_bracket_depth(&editor, pair(&editor, 1, 3), Some(&index(&editor))),
        Some(2)
    );
}

#[test]
fn bracket_depth_rejects_same_version_rope_replacement_and_other_editors() {
    let mut editor = editor("{\n {\n value\n }\n}\n", (2, 3));
    let mut state = AnimationState::default();
    let scope = pair(&editor, 1, 3);
    assert_eq!(
        state.scope_bracket_depth(&editor, scope, Some(&index(&editor))),
        Some(1)
    );
    editor.rope = Rope::from_str(" \n {\n value\n }\n \n");
    assert_eq!(state.scope_bracket_depth(&editor, scope, None), None);
    assert_eq!(
        state.scope_bracket_depth(&editor, scope, Some(&index(&editor))),
        Some(0)
    );
    let other = Editor::test_with_text("{\n {\n value\n }\n}\n");
    assert_eq!(state.scope_bracket_depth(&other, scope, None), None);
}

#[test]
fn deep_curly_scope_and_entrance_clock_survive_typing_backspace_delete_and_newline() {
    let source = format!("{}fn run() {{\n    value\n}}\n", "plain\n".repeat(700));
    let mut editor = editor(&source, (701, 7));
    let structural =
        crate::animation_structural_scope::StructuralScopeRequest::from_editor(&editor)
            .unwrap()
            .build(None, || false)
            .unwrap();
    let mut state = AnimationState::default();
    let mut settings = AnimationSettings::default();
    settings.set(Effect::ScopeConnector, true);
    let initial = Instant::now();
    state.observe(snapshot(&editor), &[], &settings, initial);
    let scope = state.scope(&editor, Some(&structural)).unwrap();
    state.update_scope(editor.navigation_id, Some(scope), &settings, initial);
    let position = editor.rope.line_to_char(701) + 7;
    for (step, (start, end, replacement)) in [
        (position, position, "x"),
        (position, position + 1, ""),
        (position - 1, position, ""),
        (position - 1, position - 1, "\n    "),
    ]
    .into_iter()
    .enumerate()
    {
        edit(&mut editor, start, end, replacement);
        let at = initial + Duration::from_millis(20 * (step as u64 + 1));
        let actual = state
            .scope(&editor, Some(&structural))
            .expect("plain typing preserves a deep indexed scope");
        state.observe(snapshot(&editor), &[], &settings, at);
        let events = state.update_scope(editor.navigation_id, Some(actual), &settings, at);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].started, initial);
        assert!(
            matches!(events[0].trigger, Trigger::Scope { pair, exiting: None, .. } if pair == actual)
        );
    }
    let settled = initial + Duration::from_secs(2);
    state.advance_frame(&settings, settled);
    let position = editor.rope.line_to_char(editor.cursor_line) + editor.cursor_col;
    edit(&mut editor, position, position, "s");
    let actual = state.scope(&editor, Some(&structural));
    state.observe(snapshot(&editor), &[], &settings, settled);
    assert!(
        state
            .update_scope(editor.navigation_id, actual, &settings, settled)
            .is_empty()
    );
    assert_eq!(state.next_frame_wait(&settings, settled), None);
}

#[test]
fn verified_scope_edits_handle_unicode_across_different_rope_chunk_boundaries() {
    let source = format!(
        "{}{{\n    value\n}}\n{}",
        "界é🦀\n".repeat(700),
        "é界\n".repeat(700)
    );
    for insertion in ["x", "界", "🦀", "\né"] {
        let old = Rope::from_str(&source);
        for position in [1, 128, old.line_to_char(701) + 7, old.len_chars() - 128] {
            let mut new = old.clone();
            new.insert(position, insertion);
            let delta = ScopeEdit::between(&old, &new).unwrap();
            let old_scope = ((700, 0), (702, 0));
            let mapped = delta.pair(&old, &new, old_scope).unwrap();
            assert_eq!(source_char(&new, mapped.0), Some('{'));
            assert_eq!(source_char(&new, mapped.1), Some('}'));
            let restored = {
                let mut restored = new.clone();
                restored.remove(position..position + insertion.chars().count());
                restored
            };
            let reverse = ScopeEdit::between(&new, &restored).unwrap();
            assert_eq!(reverse.pair(&new, &restored, mapped), Some(old_scope));
        }
    }
}

#[test]
fn scope_discovery_reuse_rejects_literal_prefix_and_javascript_regex_state_changes() {
    for (before, after) in [
        (
            "{\n let value = \"x\";\n}\n",
            "{\n let value = r\"x\";\n}\n",
        ),
        (
            "{\n value = +\n /[{}]/;\n}\n",
            "{\n value = x\n /[{}]/;\n}\n",
        ),
        ("{\n retur\n /[{}]/;\n}\n", "{\n return\n /[{}]/;\n}\n"),
        ("{\n va lue\n /[{}]/;\n}\n", "{\n value\n /[{}]/;\n}\n"),
    ] {
        let mut editor = editor(before, (1, 2));
        editor.language_override = Some("JavaScript".into());
        let changed = Rope::from_str(after);
        let delta = ScopeEdit::between(&editor.rope, &changed).unwrap();
        assert!(!delta.preserves_scope(&editor), "unsafe reuse: {after:?}");
    }
    let mut editor = editor("{\n let value = 12;\n}\n", (1, 8));
    editor.language_override = Some("TypeScript".into());
    let changed = Rope::from_str("{\n let values = 123;\n}\n");
    assert!(
        ScopeEdit::between(&editor.rope, &changed)
            .unwrap()
            .preserves_scope(&editor)
    );
}

#[test]
fn deep_scope_survives_ordinary_string_and_comment_edits_without_worker_refresh() {
    for line in [
        "    let label = \"abcd\";",
        "    let label = abcd; // explanation",
        "    /* abcd */",
    ] {
        let source = format!("{}fn run() {{\n{line}\n}}\n", "plain\n".repeat(700));
        let column = line.find("abcd").unwrap() + 2;
        let mut editor = editor(&source, (701, column));
        let structural =
            crate::animation_structural_scope::StructuralScopeRequest::from_editor(&editor)
                .unwrap()
                .build(None, || false)
                .unwrap();
        let mut state = AnimationState::default();
        let scope = state.scope(&editor, Some(&structural)).unwrap();
        let position = editor.rope.line_to_char(701) + column;
        edit(&mut editor, position, position, "x");
        assert_eq!(
            state.scope(&editor, Some(&structural)),
            Some(scope),
            "lost scope after typing: {line}"
        );
        edit(&mut editor, position, position + 1, "");
        assert_eq!(
            state.scope(&editor, Some(&structural)),
            Some(scope),
            "lost scope after backspace: {line}"
        );
    }
    for (before, after) in [
        (
            "{\n auto s = R\"tag(content)tag\";\n}\n",
            "{\n auto s = R\"tags(content)tag\";\n}\n",
        ),
        ("{\n )tag\";\n}\n", "{\n )tags\";\n}\n"),
        ("{\n /* text */\n}\n", "{\n /x* text */\n}\n"),
        ("{\n \"a\\\"b\"\n}\n", "{\n \"ax\\\"b\"\n}\n"),
    ] {
        let editor = editor(before, (1, 0));
        assert!(
            !ScopeEdit::between(&editor.rope, &Rope::from_str(after))
                .unwrap()
                .preserves_scope(&editor)
        );
    }
}

#[test]
fn bracket_depth_rechecks_lines_crossing_the_index_skip_threshold_in_both_directions() {
    let limit = crate::background::MAX_BRACKET_LINE_CHARS;
    let source = format!("{}{{\n    {{\n value\n }}\n}}\n", "x".repeat(limit - 2));
    let mut editor = editor(&source, (0, 0));
    let mut state = AnimationState::default();
    let original = index(&editor);
    let scope = pair(&editor, 1, 3);
    assert_eq!(editor.rope.line(0).len_chars(), limit);
    assert_eq!(
        state.scope_bracket_depth(&editor, scope, Some(&original)),
        Some(1)
    );
    edit(&mut editor, 0, 0, "x");
    assert_eq!(editor.rope.line(0).len_chars(), limit + 1);
    assert_eq!(
        state.scope_bracket_depth(&editor, scope, Some(&original)),
        None
    );
    let longer = index(&editor);
    assert_eq!(
        state.scope_bracket_depth(&editor, scope, Some(&longer)),
        Some(0)
    );
    edit(&mut editor, 0, 1, "");
    assert_eq!(
        state.scope_bracket_depth(&editor, scope, Some(&longer)),
        None
    );
    assert_eq!(
        state.scope_bracket_depth(&editor, scope, Some(&index(&editor))),
        Some(1)
    );

    // Crossing the limit after an opener still invalidates that whole line:
    // the depth worker skips its earlier delimiter as well as the edit.
    let source = format!("{{{}\n value\n}}\n", "x".repeat(limit - 2));
    let mut editor = Editor::test_with_text(&source);
    let scope = pair(&editor, 0, 2);
    assert_eq!(
        state.scope_bracket_depth(&editor, scope, Some(&index(&editor))),
        Some(0)
    );
    let position = limit - 1;
    edit(&mut editor, position, position, "x");
    assert_eq!(state.scope_bracket_depth(&editor, scope, None), None);
    assert_eq!(
        state.scope_bracket_depth(&editor, scope, Some(&index(&editor))),
        None
    );
}

#[test]
fn scope_reuse_rejects_moving_newlines_across_comment_quotes() {
    for (before, after) in [
        ("{\n// \" \n value\n}\n", "{\n//\n \" value\n}\n"),
        ("{\n// '\n value\n}\n", "{\n//\n 'value\n}\n"),
        (
            "{\n// return\nfoo\n/[{}]/;\n}\n",
            "{\n// return foo\n \n/[{}]/;\n}\n",
        ),
    ] {
        let editor = editor(before, (1, 0));
        let delta = ScopeEdit::between(&editor.rope, &Rope::from_str(after)).unwrap();
        assert!(
            !delta.preserves_scope(&editor),
            "newline moved a comment quote into code"
        );
    }
}

#[test]
fn deep_curly_scope_keeps_its_pair_and_clock_while_typing_round_and_square_delimiters() {
    let source = format!("{}fn run() {{\n    value\n}}\n", "plain\n".repeat(700));
    let mut editor = editor(&source, (701, 7));
    let structural =
        crate::animation_structural_scope::StructuralScopeRequest::from_editor(&editor)
            .unwrap()
            .build(None, || false)
            .unwrap();
    let mut state = AnimationState::default();
    let mut settings = AnimationSettings::default();
    settings.set(Effect::ScopeConnector, true);
    let initial = Instant::now();
    state.observe(snapshot(&editor), &[], &settings, initial);
    let scope = state.scope(&editor, Some(&structural)).unwrap();
    state.update_scope(editor.navigation_id, Some(scope), &settings, initial);
    let position = editor.rope.line_to_char(701) + 7;
    for (step, delimiter) in ['(', ')', '[', ']'].into_iter().enumerate() {
        for (inserting, at) in [
            (true, initial + Duration::from_millis(step as u64 * 20 + 5)),
            (
                false,
                initial + Duration::from_millis(step as u64 * 20 + 10),
            ),
        ] {
            if inserting {
                edit(&mut editor, position, position, &delimiter.to_string());
            } else {
                edit(&mut editor, position, position + 1, "");
            }
            assert_eq!(state.scope(&editor, Some(&structural)), Some(scope));
            state.observe(snapshot(&editor), &[], &settings, at);
            let events = state.update_scope(editor.navigation_id, Some(scope), &settings, at);
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].started, initial);
            assert!(
                matches!(events[0].trigger, Trigger::Scope { pair, exiting: None, .. } if pair == scope)
            );
        }
    }
}
