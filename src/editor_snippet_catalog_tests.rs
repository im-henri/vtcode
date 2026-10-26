use super::*;
use std::collections::HashSet;

#[test]
fn snippet_catalog_expands_every_entry_and_preserves_navigation_and_undo() {
    let mut languages = HashSet::new();
    let mut entries = 0;
    for &group in SNIPPET_LANGUAGE_GROUPS {
        let canonical = group[0];
        let triggers = snippet_triggers(canonical);
        let mut unique = HashSet::new();
        for &alias in group {
            assert!(languages.insert(alias), "duplicate language: {alias}");
            assert_eq!(snippet_triggers(alias), triggers);
            assert_eq!(
                max_snippet_trigger_bytes(alias),
                triggers
                    .iter()
                    .map(|trigger| trigger.len())
                    .max()
                    .unwrap_or(0)
            );
        }
        for &trigger in triggers {
            entries += 1;
            assert!(
                unique.insert(trigger),
                "duplicate trigger: {canonical}/{trigger}"
            );
            assert!(
                trigger
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            );
            let body = get_snippet(trigger, canonical).unwrap();
            for &alias in group {
                assert_eq!(get_snippet(trigger, alias), Some(body));
            }
            let original = format!("  {trigger}");
            let mut editor = Editor::test_with_text(&original);
            editor.language_override = Some(canonical.into());
            editor.cursor_col = original.chars().count();
            let (expected, stops) = parse_snippet_body(&body.replace('\n', "\n  "));
            let chars = expected.chars().count();
            assert!(
                stops
                    .iter()
                    .all(|(_, offset, len)| *offset <= chars && *len <= chars - offset),
                "{canonical}/{trigger}"
            );
            assert!(editor.try_expand_snippet(), "{canonical}/{trigger}");
            assert_eq!(editor.rope.to_string(), format!("  {expected}"));
            for _ in 0..=stops.len() {
                assert!(editor.cursor_line < editor.rope.len_lines());
                assert!(editor.cursor_col <= editor.rope.line(editor.cursor_line).len_chars());
                if !editor.snippet_active {
                    break;
                }
                assert!(editor.snippet_next_tab_stop());
            }
            assert!(
                !editor.snippet_active,
                "{canonical}/{trigger} navigation did not finish"
            );
            editor.undo();
            assert_eq!(
                editor.rope.to_string(),
                original,
                "{canonical}/{trigger} undo"
            );
        }
    }
    assert!(snippet_triggers("Unknown language").is_empty());
    assert_eq!(get_snippet("fn", "Unknown language"), None);
    eprintln!(
        "{} language families, {} language names, {entries} triggers",
        SNIPPET_LANGUAGE_GROUPS.len(),
        languages.len()
    );
}

#[test]
fn snippets_are_reachable_through_real_file_names_before_highlighting_loads() {
    for (path, trigger, text) in [
        ("example.c", "main", "int main("),
        ("example.cpp", "class", "class Name"),
        ("example.C", "namespace", "namespace name"),
        ("example.hpp", "class", "class Name"),
        ("example.cs", "prop", "public string Name"),
        ("example.lua", "lfn", "local function name"),
        ("example.tcl", "proc", "proc name {args}"),
        ("example.tm", "dict", "dict set dictionary key value"),
        ("example.go", "fn", "func name("),
        ("example.java", "main", "public static void main("),
        ("example.php", "fore", "foreach ($items as $item)"),
        ("example.sh", "fn", "name()"),
        ("example.fish", "fn", "function name"),
        ("example.ps1", "fore", "foreach ($item in $items)"),
        ("example.kt", "data", "data class Name"),
        ("example.swift", "struct", "struct Name"),
        ("example.dart", "afn", "Future<void> name("),
        ("example.jsx", "fn", "function name("),
        ("example.tsx", "afn", "async function name("),
        ("example.sql", "select", "SELECT columns"),
        ("Cargo.toml", "table", "[table]"),
        ("CMakeLists.txt", "exe", "add_executable(target sources)"),
        (
            "example.cmake",
            "inc",
            "${CMAKE_CURRENT_SOURCE_DIR}/include",
        ),
        ("example.mk", "rule", "$(CC) $(CPPFLAGS)"),
        ("Dockerfile", "cmd", "CMD [\"executable\"]"),
        ("example.proto", "message", "message Name"),
        ("example.hcl", "locals", "locals {"),
    ] {
        let mut editor = Editor::test_with_text(trigger);
        editor.file_path = Some(path.into());
        editor.cursor_col = trigger.chars().count();
        assert!(
            editor.try_expand_snippet(),
            "{path}/{trigger}: {}",
            editor.language_name()
        );
        assert!(
            editor.rope.to_string().contains(text),
            "{path}/{trigger}: {}",
            editor.rope
        );
    }
}

#[test]
fn snippets_keep_literal_dollars_newline_escapes_and_make_recipe_tabs() {
    for (language, trigger, expected) in [
        ("Tcl", "for", "for {set i 0} {$i < 10} {incr i} {\n    \n}"),
        ("Tcl", "foreach", "foreach item $items {\n    \n}"),
        (
            "PowerShell",
            "foreach",
            "foreach ($item in $items) {\n    \n}",
        ),
        ("PHP", "foreach", "foreach ($items as $item) {\n    \n}"),
        ("Shell Script", "arg", "\"$1\""),
        ("C", "printf", "printf(\"message\\n\");"),
        ("C++", "cout", "std::cout << value << '\\n';"),
        (
            "JavaScript",
            "for",
            "for (let i = 0; i < length; i++) {\n    \n}",
        ),
        (
            "Makefile",
            "rule",
            "%.o: %.c\n\t$(CC) $(CPPFLAGS) $(CFLAGS) -c $< -o $@\n",
        ),
    ] {
        let (text, _) = parse_snippet_body(get_snippet(trigger, language).unwrap());
        assert_eq!(text, expected, "{language}/{trigger}");
    }
    let (text, _) = parse_snippet_body(get_snippet("inc", "CMake").unwrap());
    assert!(text.contains("${CMAKE_CURRENT_SOURCE_DIR}"));
}

#[test]
fn required_language_snippets_are_offered_and_accepted_by_vtcomplete() {
    use crate::app::{ActivePane, App, Tab};
    for (path, trigger) in [
        ("test.c", "fn"),
        ("test.cpp", "namespace"),
        ("test.cs", "prop"),
        ("test.lua", "lfn"),
        ("test.tcl", "proc"),
    ] {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text(trigger);
        editor.file_path = Some(path.into());
        editor.cursor_col = trigger.len();
        let body = get_snippet(trigger, editor.language_name()).unwrap();
        let expected = parse_snippet_body(body).0;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.active_pane = ActivePane::Editor;
        app.update_vtcomplete_explicit();
        app.wait_for_background(std::time::Duration::from_secs(5), |app| {
            app.vtcomplete_active
        });
        let selected = app
            .vtcomplete_suggestions
            .iter()
            .position(|candidate| candidate == trigger)
            .unwrap_or_else(|| panic!("{path}/{trigger}: {:?}", app.vtcomplete_suggestions));
        assert_eq!(
            app.vtcomplete_details[trigger].kind,
            crate::app_vtcomplete::CompletionKind::Snippet
        );
        app.vtcomplete_selected = selected;
        app.apply_vtcomplete();
        let editor = app.editor_mut().unwrap();
        assert_eq!(editor.rope.to_string(), expected, "{path}/{trigger}");
        assert!(editor.snippet_active);
        editor.undo();
        assert_eq!(editor.rope.to_string(), trigger);
    }
}

#[test]
fn export_snippet_catalog_for_language_parser_validation() {
    let Some(path) = std::env::var_os("VTCODE_SNIPPET_CATALOG") else {
        return;
    };
    let groups = SNIPPET_LANGUAGE_GROUPS
        .iter()
        .map(|languages| {
            let snippets = snippet_triggers(languages[0])
                .iter()
                .map(|trigger| {
                    let body = get_snippet(trigger, languages[0]).unwrap();
                    let (text, _) = parse_snippet_body(body);
                    serde_json::json!({"trigger": trigger, "body": body, "text": text})
                })
                .collect::<Vec<_>>();
            serde_json::json!({"languages": languages, "snippets": snippets})
        })
        .collect::<Vec<_>>();
    std::fs::write(path, serde_json::to_vec_pretty(&groups).unwrap()).unwrap();
}
