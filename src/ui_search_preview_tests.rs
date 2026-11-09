use super::*;
use crate::app::{ActivePane, SidebarView};
use ratatui::{Terminal, backend::TestBackend, layout::Rect, style::Color};

fn result(text: &str, start: usize, end: usize) -> GlobalSearchResult {
    GlobalSearchResult {
        file_path: "src/example.rs".into(), line_num: 1, line_text: text.into(),
        col_start: start, col_end: end, context_before: None, context_after: None,
    }
}

fn text(spans: &[Span<'_>]) -> String {
    spans.iter().map(|span| span.content.as_ref()).collect()
}

fn row(app: &App) -> Style {
    Style::default().fg(app.colors.fg_text).bg(app.colors.bg_sidebar)
}

#[test]
fn replacement_uses_theme_diff_colors_and_strikes_only_the_original_match() {
    for theme in [crate::app_theme::EditorTheme::Dark, crate::app_theme::EditorTheme::Light] {
        let mut app = App::test_new();
        app.colors = theme.colors();
        app.colors.fg_diff_removed = Color::Rgb(204, 32, 122);
        app.colors.fg_diff_added = Color::Rgb(19, 160, 93);
        app.colors.bg_diff_removed_highlight = Color::Rgb(54, 12, 37);
        app.colors.bg_diff_added_highlight = Color::Rgb(12, 54, 37);
        app.global_search_query = "foo".into();
        app.global_search_replace_query = "bar".into();
        let found = result("keep foo foo tail", 5, 8);
        let spans = Preview::new(&app).spans(&found, 80, row(&app), &app.colors);
        assert_eq!(text(&spans), "keep foobar foo tail");
        assert_eq!(spans[1].content, "foo");
        assert_eq!(spans[1].style.fg, Some(app.colors.fg_diff_removed));
        assert_eq!(spans[1].style.bg, Some(app.colors.bg_diff_removed_highlight));
        assert!(spans[1].style.add_modifier.contains(Modifier::CROSSED_OUT));
        assert_eq!(spans[2].content, "bar");
        assert_eq!(spans[2].style.fg, Some(app.colors.fg_diff_added));
        assert_eq!(spans[2].style.bg, Some(app.colors.bg_diff_added_highlight));
        for index in [0, 2, 3] {
            assert!(!spans[index].style.add_modifier.contains(Modifier::CROSSED_OUT));
        }
        assert_eq!(&*found.line_text, "keep foo foo tail");
    }
}

#[test]
fn empty_hidden_and_invalid_replacements_keep_normal_find_highlighting() {
    for state in 0..3 {
        let mut app = App::test_new();
        app.global_search_query = "foo".into();
        app.global_search_replace_query = "bar".into();
        match state {
            0 => app.global_search_replace_query.clear(),
            1 => app.global_search_replace_visible = false,
            _ => app.global_search_error = Some("Invalid regex".into()),
        }
        let spans = Preview::new(&app).spans(&result("foo tail", 0, 3), 40, row(&app), &app.colors);
        assert_eq!(text(&spans), "foo tail");
        assert_eq!(spans[1].style, app.colors.ui.search_match_style(row(&app)));
        assert!(spans.iter().all(|span| !span.style.add_modifier.contains(Modifier::CROSSED_OUT)));
    }
}

#[test]
fn preview_expansion_matches_the_actual_replacement_for_captures_case_context_and_line_breaks() {
    for (source, query, replacement, start, end, regex, whole_word, preserve) in [
        ("cat42 cat7", r"(cat)(\d+)", "$2-$1", 6, 10, true, false, false),
        ("foo42 tail", r"foo(\d+)", r"\U$0-$1", 0, 5, true, false, false),
        ("xfoo foo", r"(?<=x)foo", "$0!", 1, 4, true, false, false),
        ("foobar foo", "foo", "$0!", 7, 10, true, true, false),
        ("left foo right", "foo", "$`<$0>$'", 5, 8, true, false, false),
        ("foo\r\nbar tail", r"(foo)\r?\n(bar)", "$2\n$1", 0, 8, true, false, false),
        ("FOO foo", "foo", "bar", 0, 3, false, false, true),
        ("é界 foo", "foo", "👩‍💻e\u{301}", 3, 6, false, false, false),
        ("foo", "foo", r"literal\n$1", 0, 3, false, false, false),
    ] {
        let mut app = App::test_new();
        app.global_search_query = query.into();
        app.global_search_replace_query = replacement.into();
        app.global_search_regex = regex;
        app.global_search_whole_word = whole_word;
        app.global_search_preserve_case = preserve;
        let found = result(source, start, end);
        let spans = Preview::new(&app).spans(&found, 200, row(&app), &app.colors);
        let options = crate::workspace_replace::Options {
            query: query.into(), replacement: replacement.into(), regex, whole_word,
            preserve_case: preserve, case_sensitive: false,
        };
        let program = crate::app_search::SnapshotReplacer::workspace(&options).unwrap();
        let (prepared, count) = program.prepare(ropey::Rope::from_str(source), vec![(1, start, end)],
            &crate::background::CancellationToken::new()).unwrap();
        assert_eq!(count, 1);
        let removed = &spans[1].content;
        let added = &spans[2].content;
        let before: String = source.chars().take(start).collect();
        let after: String = source.chars().skip(end).collect();
        let replaced = prepared.rope().to_string();
        let expected = replaced.strip_prefix(&before).unwrap().strip_suffix(&after).unwrap();
        // The one-line preview displays LF as ↵. A CRLF pair occupies the
        // same two source columns, with the CR rendered as a space.
        assert_eq!(removed.as_ref(), display_text(&source.chars().skip(start).take(end - start).collect::<String>()));
        assert_eq!(added.as_ref(), display_text(&expected.replace("\r\n", "\n")), "{query:?} -> {replacement:?}");
    }
}

#[test]
fn zero_length_and_whitespace_matches_preview_insertions_without_losing_indentation() {
    let mut app = App::test_new();
    app.global_search_replace_query = ">".into();
    let spans = Preview::new(&app).spans(&result("ab", 0, 0), 20, row(&app), &app.colors);
    assert_eq!(text(&spans), ">ab");
    assert!(spans[1].content.is_empty());
    let spans = Preview::new(&app).spans(&result("  foo", 0, 2), 20, row(&app), &app.colors);
    assert_eq!(spans[1].content, "  ");
    assert_eq!(spans[2].content, ">");
    assert_eq!(text(&spans), "  >foo");
}

#[test]
fn narrow_previews_keep_both_change_colors_and_clip_complete_unicode_graphemes() {
    let mut app = App::test_new();
    app.global_search_replace_query = "👩‍💻e\u{301}界".repeat(30);
    let old = "👨‍👩‍👧‍👦a\u{301}界".repeat(20);
    let found = result(&format!("prefix {old} suffix"), 7, 7 + old.chars().count());
    for width in 0..=70 {
        let spans = Preview::new(&app).spans(&found, width, row(&app), &app.colors);
        assert!(UnicodeWidthStr::width(text(&spans).as_str()) <= width, "width={width}");
        for span in &spans {
            assert!(!span.content.ends_with('\u{200d}'));
            assert!(!span.content.starts_with('\u{301}'));
        }
        if width >= 2 {
            assert!(!spans[1].content.is_empty());
            assert!(!spans[2].content.is_empty());
        }
    }
}

#[test]
fn workspace_capture_cache_is_reused_and_kept_separate_from_editor_patterns() {
    let editor = SearchRegex::cached_editor("foo", true, true, true).unwrap();
    let workspace = SearchRegex::cached_workspace("foo", true, true, true).unwrap();
    assert_eq!(editor.captures_at("foobar foo", 0).unwrap().range, 0..3);
    assert_eq!(workspace.captures_at("foobar foo", 0).unwrap().range, 7..10);
    assert!(std::sync::Arc::ptr_eq(&workspace, &SearchRegex::cached_workspace("foo", true, true, true).unwrap()));
    assert!(std::sync::Arc::ptr_eq(&editor, &SearchRegex::cached_editor("foo", true, true, true).unwrap()));
}

#[test]
fn sidebar_and_popup_render_live_previews_without_changing_files_results_or_button_targets() {
    for popup in [false, true] {
        for right in [false, true] {
            for theme in [crate::app_theme::EditorTheme::Dark, crate::app_theme::EditorTheme::Light] {
                for width in [20, 36, 60] {
                    let mut app = App::test_new();
                    app.colors = theme.colors();
                    app.sidebar_view = SidebarView::Search;
                    app.sidebar_right = right;
                    app.active_pane = ActivePane::Explorer;
                    app.global_search_query = "needle".into();
                    app.global_search_replace_query = "replacement".into();
                    app.global_search_active = true;
                    app.mouse_col = u16::MAX;
                    app.mouse_row = u16::MAX;
                    app.global_search_results = vec![result("before needle tail", 7, 13)].into();
                    let revision = app.global_search_results.revision();
                    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
                    let mut buttons = None;
                    for replacement in ["replacement", "other", "é界\nnew"] {
                        app.global_search_replace_query = replacement.into();
                        terminal.draw(|frame| {
                            if popup { crate::ui_search_dialog::draw_global_search(frame, &mut app); }
                            else { crate::ui_search_panel::draw_search_panel(frame, &mut app, Rect::new(0, 0, width, 24)); }
                        }).unwrap();
                        let buffer = terminal.backend().buffer();
                        let removed: Vec<_> = buffer.content.iter().filter(|cell| cell.modifier.contains(Modifier::CROSSED_OUT)).collect();
                        assert!(!removed.is_empty(), "popup={popup}, width={width}");
                        assert!(removed.iter().all(|cell| cell.fg == app.colors.fg_diff_removed
                            && cell.bg == app.colors.bg_diff_removed_highlight));
                        assert!(buffer.content.iter().any(|cell| !cell.symbol().trim().is_empty()
                            && cell.fg == app.colors.fg_diff_added && cell.bg == app.colors.bg_diff_added_highlight
                            && !cell.modifier.contains(Modifier::CROSSED_OUT)));
                        let targets: Vec<_> = app.search_control_regions.iter().filter_map(|button|
                            matches!(button.control, crate::ui_search_controls::SearchControl::ReplaceResult(_, _))
                                .then_some((button.control, button.area))).collect();
                        if let Some(expected) = &buttons { assert_eq!(&targets, expected); } else { buttons = Some(targets); }
                        assert_eq!(app.global_search_results.revision(), revision);
                        assert_eq!(&*app.global_search_results[0].line_text, "before needle tail");
                        assert!(!app.bg_global_replace_running);
                        if replacement == "replacement" {
                            crate::ui_control_visibility_tests::snapshot(buffer, &format!("replace preview {} {} {} {width}",
                                if popup { "popup" } else { "sidebar" }, if right { "right" } else { "left" },
                                if matches!(theme, crate::app_theme::EditorTheme::Light) { "light" } else { "dark" }));
                        }
                    }
                    app.global_search_replace_query.clear();
                    terminal.draw(|frame| {
                        if popup { crate::ui_search_dialog::draw_global_search(frame, &mut app); }
                        else { crate::ui_search_panel::draw_search_panel(frame, &mut app, Rect::new(0, 0, width, 24)); }
                    }).unwrap();
                    assert!(terminal.backend().buffer().content.iter().all(|cell| !cell.modifier.contains(Modifier::CROSSED_OUT)));
                }
            }
        }
    }
}
