use super::*;
use crate::{app::Tab, editor::Editor};

#[test]
fn ordinary_caret_motion_resolves_negative_pair_matches_without_worker_round_trips() {
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(Editor::test_with_text("alpha beta gamma\n"))];
    app.active_tab = 0;
    app.bracket_pair_colorization = false;
    for effect in crate::animation_config::AnimationEffect::ALL { app.animation_settings.set(effect, false); }
    // Settle unrelated caches so this test only observes cursor matching.
    app.cached_editor_analysis_target = app.editor_analysis_target();
    for col in 0..16 {
        app.tabs[0].editor.cursor_col = col;
        app.cached_matching_bracket = Some((0, 0));
        refresh_caches(&mut app);
        assert!(!app.bg_cursor_match_running);
        assert_eq!(app.cached_cursor_match_target, app.cursor_match_target());
        assert!(app.cached_matching_bracket.is_none());
        assert!(app.cached_matching_tag.is_none());
    }
    app.tabs[0].editor = Editor::test_with_text("(value)\n");
    refresh_caches(&mut app);
    assert!(app.bg_cursor_match_running, "real brackets still need matching");
    app.wait_for_background(Duration::from_secs(3), |app| !app.bg_cursor_match_running);
    assert_eq!(app.cached_matching_bracket, Some((0, 6)));
}

#[test]
fn resolved_negative_caret_does_not_redraw_when_an_obsolete_match_arrives() {
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(Editor::test_with_text("(value)"))];
    app.active_tab = 0;
    let old = app.cursor_match_target().unwrap();
    app.tabs[0].editor.cursor_col = 3;
    app.cached_cursor_match_target = app.cursor_match_target();
    app.bg_cursor_match_running = true;
    app.background.sender.send(crate::background::BackgroundResult::CursorMatches {
        target: old, bracket: Some((0, 6)), tag: None,
    }).unwrap();
    assert!(!app.poll_background_results());
    assert!(!app.bg_cursor_match_running);
    assert!(app.cached_matching_bracket.is_none());
}

#[test]
fn pair_candidate_check_preserves_delimiters_unicode_crlf_and_markup() {
    for text in ["alpha βeta\r\n", "(value) [other] {body}\n", "界()\r\n", "", "plain"] {
        let mut editor = Editor::test_with_text(text);
        for line in 0..editor.line_count() {
            editor.cursor_line = line;
            for col in 0..=editor.rope.line(line).len_chars() + 1 {
                editor.cursor_col = col;
                if !editor.may_have_cursor_pair() {
                    assert_eq!(editor.find_matching_bracket(), None, "{text:?}, {line}:{col}");
                }
            }
        }
    }
    let mut editor = Editor::test_with_text("<div title=\"x\">body</div>");
    for language in ["HTML", "XML", "JSX", "TSX"] {
        editor.language_override = Some(language.into());
        for col in 0..editor.rope.len_chars() {
            editor.cursor_col = col;
            if editor.find_matching_tag().is_some() { assert!(editor.may_have_cursor_pair()); }
        }
    }
}
