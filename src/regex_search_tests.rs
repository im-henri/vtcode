use super::*;
use crate::app_search_algo::{MAX_SEARCH_MATCHES, scan_search_matches};

fn assert_both_scopes(
    text: &str,
    query: &str,
    case: bool,
    whole: bool,
    expected: &[(usize, usize)],
) {
    let rope = ropey::Rope::from_str(text);
    let cancel = crate::background::CancellationToken::new();
    let (local, truncated, error) =
        scan_search_matches(&rope, query, case, true, whole, None, &cancel);
    assert!(error.is_none(), "{query:?}: {error:?}");
    assert!(!truncated, "unexpected truncation for {query:?}");
    let positions = |row, start, end| {
        let offset = rope.line_to_char(row);
        (offset + start, offset + end)
    };
    assert_eq!(
        local
            .iter()
            .map(|&(l, s, e)| positions(l, s, e))
            .collect::<Vec<_>>(),
        expected,
        "Ctrl+F: {query:?} in {text:?}, case={case}, whole={whole}"
    );

    let regex = SearchRegex::workspace(query, case, whole, true).unwrap();
    let spec = GlobalSearchSpec {
        query,
        regex: Some(&regex),
        include: "",
        exclude: "",
        max_results: MAX_SEARCH_MATCHES,
        case_sensitive: case,
        open_buffers: None,
    };
    let mut workspace = Vec::new();
    let mut truncated = false;
    App::search_file_content(
        Path::new("fixture.txt"),
        text,
        spec,
        &mut workspace,
        &mut truncated,
        &cancel,
    );
    assert!(!truncated, "unexpected workspace truncation for {query:?}");
    assert_eq!(
        workspace
            .iter()
            .map(|r| positions(r.line_num - 1, r.col_start, r.col_end))
            .collect::<Vec<_>>(),
        expected,
        "workspace: {query:?} in {text:?}, case={case}, whole={whole}"
    );
}

#[test]
fn regex_search_operators_are_regex_not_globs() {
    for (text, query, expected) in [
        ("a*b **", r"\*", vec![(1, 2), (4, 5), (5, 6)]),
        ("a?b ?", r"\?", vec![(1, 2), (4, 5)]),
        ("ab axb aXXb", "a.b", vec![(3, 6)]),
        ("ab axb aXXb", "a.*b", vec![(0, 11)]),
        ("ab axb aXXb", "a.*?b", vec![(0, 2), (3, 6), (7, 11)]),
        ("b ab aaab", "a*b", vec![(0, 1), (2, 4), (5, 9)]),
        ("b ab aaab", "a+b", vec![(2, 4), (5, 9)]),
        ("1 12 1234", r"\d{2,3}", vec![(2, 4), (5, 8)]),
        (
            "cat dog catdog",
            "cat|dog",
            vec![(0, 3), (4, 7), (8, 11), (11, 14)],
        ),
        ("a1 B2 c3", "[A-Z][0-9]", vec![(3, 5)]),
        ("foo food bar", r"\bfoo\b", vec![(0, 3)]),
        ("é 猫 42", r"\p{L}+", vec![(0, 1), (2, 3)]),
    ] {
        assert_both_scopes(text, query, true, false, &expected);
    }
}

#[test]
fn regex_search_does_not_silently_limit_matches_per_line() {
    let text = "x ".repeat(1_500);
    let expected = (0..1_500).map(|i| (i * 2, i * 2 + 1)).collect::<Vec<_>>();
    assert_both_scopes(&text, "x", true, false, &expected);
}

#[test]
fn regex_search_invalid_patterns_fail_instead_of_becoming_literals() {
    for query in ["*", "+", "[", "(", r"\", "a{3,2}", r"\A", r"\z", "(?s).*"] {
        let (matches, truncated, error) = scan_search_matches(
            &ropey::Rope::from_str("* + [ ( foo aa"),
            query,
            true,
            true,
            false,
            None,
            &crate::background::CancellationToken::new(),
        );
        assert!(matches.is_empty(), "{query:?}");
        assert!(!truncated);
        assert!(error.is_some(), "must report an error for {query:?}");
    }
}

#[test]
fn regex_search_workspace_preview_shows_matched_line_breaks() {
    for (text, query, expected) in [
        ("foo\nbar", r"foo\n", "foo↵"),
        ("foo\r\nbar", r"foo\r?\nbar", "foo ↵bar"),
    ] {
        let regex = SearchRegex::workspace(query, true, false, true).unwrap();
        let spec = GlobalSearchSpec {
            query,
            regex: Some(&regex),
            include: "",
            exclude: "",
            max_results: 100,
            case_sensitive: true,
            open_buffers: None,
        };
        let mut results = Vec::new();
        App::search_file_content(
            Path::new("fixture"),
            text,
            spec,
            &mut results,
            &mut false,
            &crate::background::CancellationToken::new(),
        );
        assert_eq!(results.len(), 1);
        assert_eq!(&*results[0].line_text, expected);
    }
}

fn find_app(text: &str, query: &str, whole_word: bool) -> App {
    let mut app = App::test_new();
    app.tabs.push(crate::app::Tab::new_file(
        crate::editor::Editor::test_with_text(text),
    ));
    app.search_active = true;
    app.search_input_focused = true;
    app.search_regex = true;
    app.search_case_sensitive = true;
    app.search_whole_word = whole_word;
    app.search_query = query.to_string();
    app.update_search();
    app
}

#[test]
fn regex_search_replace_preserves_context_captures_and_empty_matches() {
    for (text, query, whole, replacement, expected) in [
        ("foo\nbar\n", "^", false, ">", ">foo\n>bar\n>"),
        ("foo\nbar\n", "$", false, "!", "foo!\nbar!\n!"),
        ("foo bar", r"\Boo", false, "X", "fX bar"),
        ("foobar", r"foo\B", false, "X", "Xbar"),
        ("foo foobar", "(foo|foobar)", true, "$1!", "foo! foobar"),
        (
            "é-12 é-34",
            r"(?<letter>é)-(\d+)",
            false,
            "$1:$2",
            "é:12 é:34",
        ),
        (
            "foo\r\nbar\r\n",
            r"(foo)\r?\n(bar)",
            false,
            "$2-$1",
            "bar-foo\r\n",
        ),
        ("foo\nbar", "^foo", false, "", "\nbar"),
        ("foo\nbar", "bar$", false, "", "foo\n"),
        ("foo foo", "foo", false, "$$$0", "$foo $foo"),
    ] {
        let mut app = find_app(text, query, whole);
        app.replace_query = replacement.to_string();
        assert!(!app.search_matches.is_empty(), "{query:?}");
        app.replace_all();
        assert_eq!(
            app.editor().unwrap().rope.to_string(),
            expected,
            "replace {query:?}"
        );
        app.editor_mut().unwrap().undo();
        assert_eq!(
            app.editor().unwrap().rope.to_string(),
            text,
            "undo {query:?}"
        );
    }
}

#[test]
fn regex_search_replace_current_advances_past_empty_matches() {
    let mut app = find_app("foo\nbar", "^", false);
    app.replace_query = ">".into();
    app.replace_current();
    assert_eq!(app.editor().unwrap().rope.to_string(), ">foo\nbar");
    assert_eq!(app.search_matches[app.search_current], (1, 0, 0));
    app.replace_current();
    assert_eq!(app.editor().unwrap().rope.to_string(), ">foo\n>bar");
    let mut app = find_app("foo bar", r"\Boo", false);
    app.replace_query = "X".into();
    app.replace_current();
    assert_eq!(app.editor().unwrap().rope.to_string(), "fX bar");
}

#[test]
fn regex_search_cannot_replace_stale_ranges_while_refresh_is_pending() {
    for replace_all in [false, true] {
        let mut app = find_app("old\nfoo", "foo", false);
        app.editor_mut().unwrap().rope = ropey::Rope::from_str("new");
        app.file_search_refresh_pending = true;
        app.replace_query = "replacement".into();
        if replace_all {
            app.replace_all();
        } else {
            app.replace_current();
        }
        assert_eq!(app.editor().unwrap().rope.to_string(), "new");
    }
}

#[test]
fn regex_search_selection_clips_input_and_keeps_complete_ranges() {
    let rope = ropey::Rope::from_str("foo\nbar\nfoo\nbar");
    let cancel = crate::background::CancellationToken::new();
    for (query, selection, expected) in [
        (r"foo\nbar", (2, 0, 3, 3), vec![(2, 0, 7)]),
        (r"foo\nbar", (2, 1, 3, 3), vec![]),
        (r"foo\nbar", (2, 0, 3, 2), vec![]),
        ("^foo", (2, 0, 3, 3), vec![(2, 0, 3)]),
        ("^", (2, 0, 3, 3), vec![(2, 0, 0), (3, 0, 0)]),
    ] {
        let (matches, truncated, error) =
            scan_search_matches(&rope, query, true, true, false, Some(selection), &cancel);
        assert_eq!(matches, expected, "{query:?} in {selection:?}");
        assert!(!truncated);
        assert!(error.is_none());
    }
}

#[test]
fn regex_search_reports_only_actual_result_truncation() {
    for length in [MAX_SEARCH_MATCHES, MAX_SEARCH_MATCHES + 1] {
        let rope = ropey::Rope::from_str(&"x".repeat(length));
        let (matches, truncated, error) = scan_search_matches(
            &rope,
            "x",
            true,
            true,
            false,
            None,
            &crate::background::CancellationToken::new(),
        );
        assert_eq!(matches.len(), MAX_SEARCH_MATCHES);
        assert_eq!(truncated, length > MAX_SEARCH_MATCHES);
        assert!(error.is_none());
    }
}

fn wait_for_workspace(app: &mut App) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        app.poll_background_results();
        if !app.bg_global_search_running && !app.bg_global_replace_running {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "workspace search/replacement timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn vscode_search_navigation_and_replace_all_continue_past_highlight_limit() {
    let count = MAX_SEARCH_MATCHES + 5;
    let mut app = find_app(&"hit ".repeat(count), "hit", false);
    assert_eq!(app.search_matches.len(), MAX_SEARCH_MATCHES);
    assert!(app.search_truncated);
    let start = MAX_SEARCH_MATCHES * 4;
    app.editor_mut().unwrap().clear_selection();
    app.editor_mut().unwrap().set_cursor(0, start);
    app.search_next();
    assert_eq!(
        app.editor().unwrap().selection_range(),
        Some((0, start, 0, start + 3))
    );
    app.search_next();
    assert_eq!(
        app.editor().unwrap().selection_range(),
        Some((0, start + 4, 0, start + 7))
    );
    app.search_prev();
    assert_eq!(
        app.editor().unwrap().selection_range(),
        Some((0, start, 0, start + 3))
    );
    app.replace_query = "x".into();
    app.replace_all();
    assert_eq!(app.editor().unwrap().rope.to_string(), "x ".repeat(count));
    assert!(app.search_matches.is_empty());
}

#[test]
fn vscode_search_selection_replacement_clips_context_and_tracks_inserted_lines() {
    let mut app = find_app("left foo foo right", "foo", false);
    app.search_in_selection = true;
    app.search_selection_range = Some((0, 5, 0, 12));
    app.search_query = "(?<=left )foo".into();
    app.update_search();
    assert!(app.search_matches.is_empty());
    app.search_query = "^foo".into();
    app.replace_query = "long\nx".into();
    app.update_search();
    app.replace_current();
    assert_eq!(
        app.editor().unwrap().rope.to_string(),
        "left long\nx foo right"
    );
    // A scope that becomes multiline expands to complete lines in FindModel.
    assert_eq!(app.search_selection_range, Some((0, 0, 1, 11)));
    app.search_query = "foo".into();
    app.replace_query = "B".into();
    app.update_search();
    assert_eq!(app.search_matches, vec![(1, 2, 5)]);
    app.replace_all();
    assert_eq!(
        app.editor().unwrap().rope.to_string(),
        "left long\nx B right"
    );
    assert_eq!(app.search_selection_range, Some((0, 0, 1, 9)));
}

#[test]
fn vscode_search_multiline_selection_uses_complete_selected_lines() {
    for (selection, expected_scope, expected_matches) in [
        (
            (0, 4, 2, 2),
            (0, 0, 2, 7),
            vec![(0, 0, 3), (1, 0, 3), (2, 0, 3)],
        ),
        ((0, 4, 2, 0), (0, 0, 1, 7), vec![(0, 0, 3), (1, 0, 3)]),
    ] {
        let mut app = find_app("hit one\nhit two\nhit end", "hit", false);
        app.search_in_selection = true;
        app.search_selection_range = Some(selection);
        app.update_search();
        assert_eq!(app.search_selection_range, Some(expected_scope));
        assert_eq!(app.search_matches, expected_matches);
    }
}

#[test]
fn vscode_search_workspace_replace_preserves_boms_encoding_and_crlf() {
    fn encode(text: &str, kind: usize) -> Vec<u8> {
        let mut bytes = match kind {
            0 => vec![0xef, 0xbb, 0xbf],
            1 => vec![0xff, 0xfe],
            _ => vec![0xfe, 0xff],
        };
        if kind == 0 {
            bytes.extend_from_slice(text.as_bytes());
        } else {
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&if kind == 1 {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
        }
        bytes
    }
    let root = tempfile::tempdir().unwrap();
    let original = "prefix 猫\r\nsuffix\r\n";
    let expected = "prefix 犬\r\nline\r\nsuffix\r\n";
    for kind in 0..3 {
        std::fs::write(
            root.path().join(format!("{kind}.txt")),
            encode(original, kind),
        )
        .unwrap();
    }
    let mut app = App::new(root.path().to_path_buf());
    app.global_search_query = "猫".into();
    app.global_search_replace_query = "犬\nline".into();
    app.update_global_search();
    wait_for_workspace(&mut app);
    assert_eq!(app.global_search_results.len(), 3);
    app.replace_all_in_files();
    wait_for_workspace(&mut app);
    for kind in 0..3 {
        assert_eq!(
            std::fs::read(root.path().join(format!("{kind}.txt"))).unwrap(),
            encode(expected, kind)
        );
    }
}

#[test]
fn vscode_search_workspace_marks_truncation_only_when_an_extra_match_exists() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("matches.txt");
    let mut app = App::new(root.path().to_path_buf());
    app.global_search_query = "hit".into();
    app.max_global_search_results = 2;
    for count in [2, 3] {
        std::fs::write(&path, "hit ".repeat(count)).unwrap();
        app.update_global_search();
        wait_for_workspace(&mut app);
        assert_eq!(app.global_search_results.len(), 2);
        assert_eq!(app.global_search_truncated, count > 2);
    }
}

#[test]
fn regex_search_workspace_replacement_matches_open_and_closed_files() {
    for crlf in [false, true] {
        for (query, replacement) in [
            ("^", ">"),
            (r"\Boo", "X"),
            (r"(foo|foobar)\b", "$1!"),
            (r"(foo)\r?\n(bar)", "$2-$1"),
        ] {
            let workspace = tempfile::tempdir().unwrap();
            let open = workspace.path().join("open.txt");
            let closed = workspace.path().join("closed.txt");
            let text = if crlf {
                "foo\r\nbar\r\nfoobar\r\n"
            } else {
                "foo\nbar\nfoobar\n"
            };
            std::fs::write(&open, text).unwrap();
            std::fs::write(&closed, text).unwrap();
            let mut app = App::new(workspace.path().to_path_buf());
            let mut editor = crate::editor::Editor::new();
            editor.open_file(&open).unwrap();
            app.tabs.push(crate::app::Tab::new_file(editor));
            app.search_regex = true;
            app.search_case_sensitive = true;
            app.global_search_query = query.into();
            app.global_search_replace_query = replacement.into();
            app.update_global_search();
            wait_for_workspace(&mut app);
            let count = match query {
                "^" => 7,
                r"\Boo" => 4,
                r"(foo|foobar)\b" => 4,
                _ => 2,
            };
            assert_eq!(
                app.global_search_results.len(),
                count,
                "{query:?}, CRLF={crlf}"
            );
            let open_expected = match query {
                "^" => ">foo\n>bar\n>foobar\n>",
                r"\Boo" => "fX\nbar\nfXbar\n",
                r"(foo|foobar)\b" => "foo!\nbar\nfoobar!\n",
                _ => "bar-foo\nfoobar\n",
            };
            let closed_expected = if query == "^" {
                ">foo\n>bar\n>foobar\n"
            } else {
                open_expected
            };
            let open_expected = if crlf {
                open_expected.replace('\n', "\r\n")
            } else {
                open_expected.to_owned()
            };
            let closed_expected = if crlf {
                closed_expected.replace('\n', "\r\n")
            } else {
                closed_expected.to_owned()
            };
            app.replace_all_in_files();
            wait_for_workspace(&mut app);
            assert_eq!(
                app.editor().unwrap().rope.to_string(),
                open_expected,
                "open {query:?}, CRLF={crlf}"
            );
            assert_eq!(
                std::fs::read_to_string(&closed).unwrap(),
                closed_expected,
                "closed {query:?}, CRLF={crlf}"
            );
            assert_eq!(
                std::fs::read_to_string(&open).unwrap(),
                text,
                "open file remains unsaved"
            );
        }
    }
}

#[test]
fn regex_search_highlights_multiline_and_empty_matches_in_editor() {
    use ratatui::{Terminal, backend::TestBackend};
    for (wrap, long_line) in [(false, false), (true, false), (false, true)] {
        let prefix = if long_line {
            "x".repeat(50_000)
        } else {
            String::new()
        };
        let text = format!("{prefix}\n\n\n\nfoo\nbar");
        let mut app = find_app(&text, r"foo\nbar", false);
        app.editor_mut().unwrap().clear_selection();
        let border = ratatui::style::Color::Rgb(230, 180, 80);
        let foreground = ratatui::style::Color::Rgb(255, 143, 64);
        app.colors.ui.border_find = Some(border);
        app.colors.ui.fg_find = Some(foreground);
        app.show_explorer = false;
        app.show_minimap = false;
        app.editor_mut().unwrap().word_wrap = wrap;
        let mut terminal = Terminal::new(TestBackend::new(120, 35)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let area = app.code_area.unwrap();
        for row in [4, 5] {
            for col in 0..3 {
                assert_eq!(
                    terminal.backend().buffer()[(area.x + col, area.y + row)].bg,
                    app.colors.ui.bg_find_current,
                    "multiline highlight row={row}, col={col}, wrap={wrap}, long_line={long_line}"
                );
                let cell = &terminal.backend().buffer()[(area.x + col, area.y + row)];
                assert_eq!(cell.fg, foreground);
                assert_eq!(cell.underline_color, border);
                assert!(cell.modifier.contains(ratatui::style::Modifier::UNDERLINED));
            }
        }
        app.search_query = "$".into();
        app.update_search();
        app.search_current = app
            .search_matches
            .iter()
            .position(|&m| m == (4, 3, 3))
            .unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let area = app.code_area.unwrap();
        assert_eq!(
            terminal.backend().buffer()[(area.x + 3, area.y + 4)].bg,
            app.colors.ui.bg_find_current,
            "end-of-line match marker, wrap={wrap}, long_line={long_line}"
        );
        let cell = &terminal.backend().buffer()[(area.x + 3, area.y + 4)];
        assert_eq!(cell.underline_color, border);
        assert!(cell.modifier.contains(ratatui::style::Modifier::UNDERLINED));
    }
}
