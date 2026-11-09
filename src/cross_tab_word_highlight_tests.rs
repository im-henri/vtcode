use super::*;
use crate::{
    app::{ActivePane, SplitDirection, Tab, TabKind},
    editor::Editor,
};
use ratatui::{Terminal, backend::TestBackend};
use std::{rc::Rc, sync::Arc};

const WORD_BG: Color = Color::Rgb(20, 80, 50);
const SELECTION_BG: Color = Color::Rgb(30, 90, 60);
const TARGET: &str = "beta ALPHA alphabet alpha _alpha alpha_\n";

fn fixture(
    wrapped: bool,
    selected: bool,
    direction: SplitDirection,
) -> (App, Terminal<TestBackend>) {
    let mut app = App::test_new();
    app.tabs = ["alpha beta\nalpha gamma\n\n", TARGET]
        .into_iter()
        .map(|text| {
            let mut editor = Editor::test_with_text(text);
            editor.word_wrap = wrapped;
            editor.cursor_col = 1;
            Tab::new_file(editor)
        })
        .collect();
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.settings_persistence_enabled = false;
    app.show_explorer = false;
    app.show_minimap = false;
    app.show_breadcrumbs = false;
    app.show_line_numbers = false;
    app.sticky_scroll = false;
    app.cursor_visible = false;
    app.colors.ui.preserve_theme_colors = true;
    app.colors.bg_word_highlight = WORD_BG;
    app.colors.ui.bg_selection_highlight = SELECTION_BG;
    app.create_split_group_dir(1, direction);
    app.switch_group(0);
    if selected {
        app.editor_mut().unwrap().selection_anchor = Some((0, 0));
        app.editor_mut().unwrap().cursor_col = 5;
    }
    // Unwrapped panes must show the full assertion line; wrapped panes stay
    // narrow so the same occurrences also exercise continuation rows.
    let width = if wrapped { 76 } else { 130 };
    (app, Terminal::new(TestBackend::new(width, 20)).unwrap())
}

fn draw(app: &mut App, terminal: &mut Terminal<TestBackend>) {
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
}

fn assert_matches(
    editor: &Editor,
    line: usize,
    expected: &[(usize, usize)],
) -> Rc<LineOccurrences> {
    let cached = editor
        .render_line_cache
        .borrow()
        .cached_occurrences(line)
        .unwrap();
    let mut cursor = WordOccurrenceCursor::new(&cached, 0);
    for col in 0..editor.line_len(line) {
        assert_eq!(
            cursor.contains(col),
            expected
                .iter()
                .any(|&(start, end)| start <= col && col < end),
            "column={col} expected={expected:?}"
        );
    }
    cached
}

fn colored_cells(terminal: &Terminal<TestBackend>, area: Rect, color: Color) -> usize {
    (area.y..area.bottom())
        .flat_map(|y| (area.x..area.right()).map(move |x| (x, y)))
        .filter(|&(x, y)| terminal.backend().buffer()[(x, y)].bg == color)
        .count()
}

#[test]
fn active_word_and_selection_highlight_other_panes_with_find_options_and_source_style() {
    for direction in [SplitDirection::Horizontal, SplitDirection::Vertical] {
        for wrapped in [false, true] {
            for selected in [false, true] {
                let (mut app, mut terminal) = fixture(wrapped, selected, direction);
                assert!(!app.highlight_words_across_tabs);
                app.highlight_words_across_tabs = true;
                for (sensitive, whole, expected) in [
                    (
                        false,
                        false,
                        vec![(5, 10), (11, 16), (20, 25), (27, 32), (33, 38)],
                    ),
                    (true, false, vec![(11, 16), (20, 25), (27, 32), (33, 38)]),
                    (true, true, vec![(20, 25)]),
                    (false, true, vec![(5, 10), (20, 25)]),
                ] {
                    app.search_case_sensitive = sensitive;
                    app.search_whole_word = whole;
                    draw(&mut app, &mut terminal);
                    let query = app.shared_word_highlight.as_ref().unwrap();
                    assert_eq!(query.word.as_ref(), "alpha");
                    assert_eq!(query.selected, selected);
                    let original_word = Arc::clone(&query.word);
                    let target = &app.groups[1].tabs[0].editor;
                    let cached = assert_matches(target, 0, &expected);
                    let builds = target.render_line_cache.borrow().occurrence_builds;
                    let area = app.groups[1].code_area.unwrap();
                    let color = if selected { SELECTION_BG } else { WORD_BG };
                    assert_eq!(
                        colored_cells(&terminal, area, color),
                        expected.len() * 5,
                        "wrapped={wrapped} selected={selected} case={sensitive} whole={whole}"
                    );
                    draw(&mut app, &mut terminal);
                    let target = &app.groups[1].tabs[0].editor;
                    assert!(Rc::ptr_eq(&cached, &assert_matches(target, 0, &expected)));
                    assert_eq!(target.render_line_cache.borrow().occurrence_builds, builds);
                    assert!(Arc::ptr_eq(
                        &original_word,
                        &app.shared_word_highlight.as_ref().unwrap().word
                    ));
                }
            }
        }
    }
}

#[test]
fn shared_highlights_follow_edits_focus_and_tab_changes_and_clear_without_a_source() {
    let (mut app, mut terminal) = fixture(false, false, SplitDirection::Horizontal);
    draw(&mut app, &mut terminal);
    assert_matches(&app.groups[1].tabs[0].editor, 0, &[(0, 4)]);
    app.highlight_words_across_tabs = true;
    draw(&mut app, &mut terminal);
    assert_matches(
        &app.groups[1].tabs[0].editor,
        0,
        &[(5, 10), (11, 16), (20, 25), (27, 32), (33, 38)],
    );
    app.editor_mut().unwrap().set_cursor(0, 0);
    app.editor_mut().unwrap().insert_char('x');
    draw(&mut app, &mut terminal);
    assert_eq!(
        app.shared_word_highlight.as_ref().unwrap().word.as_ref(),
        "xalpha"
    );
    assert_matches(&app.groups[1].tabs[0].editor, 0, &[]);
    app.editor_mut().unwrap().undo();
    draw(&mut app, &mut terminal);
    assert_eq!(
        app.shared_word_highlight.as_ref().unwrap().word.as_ref(),
        "alpha"
    );
    let previous = assert_matches(
        &app.groups[1].tabs[0].editor,
        0,
        &[(5, 10), (11, 16), (20, 25), (27, 32), (33, 38)],
    );
    // Equal revision/length does not make a different rope safe to reuse.
    app.groups[1].tabs[0].editor.rope = ropey::Rope::from_str(&TARGET.replacen("beta", "alph", 1));
    draw(&mut app, &mut terminal);
    assert!(!Rc::ptr_eq(
        &previous,
        &assert_matches(
            &app.groups[1].tabs[0].editor,
            0,
            &[(5, 10), (11, 16), (20, 25), (27, 32), (33, 38)]
        )
    ));
    app.editor_mut().unwrap().set_cursor(2, 0);
    draw(&mut app, &mut terminal);
    assert!(app.shared_word_highlight.is_none());
    assert_eq!(
        colored_cells(&terminal, app.groups[1].code_area.unwrap(), WORD_BG),
        0
    );
    app.groups[1].tabs[0].editor.rope = ropey::Rope::from_str(TARGET);
    app.switch_group(1);
    draw(&mut app, &mut terminal);
    assert_eq!(
        app.shared_word_highlight.as_ref().unwrap().word.as_ref(),
        "beta"
    );
    assert_matches(&app.groups[0].tabs[0].editor, 0, &[(6, 10)]);
    app.tabs
        .push(Tab::new_file(Editor::test_with_text("gamma\n")));
    app.active_tab = 1;
    draw(&mut app, &mut terminal);
    assert_eq!(
        app.shared_word_highlight.as_ref().unwrap().word.as_ref(),
        "gamma"
    );
    assert_matches(&app.groups[0].tabs[0].editor, 1, &[(6, 11)]);
    app.highlight_words_across_tabs = false;
    draw(&mut app, &mut terminal);
    assert!(app.shared_word_highlight.is_none());
    assert_eq!(
        colored_cells(&terminal, app.groups[0].code_area.unwrap(), WORD_BG),
        0
    );
}

#[test]
fn warm_frames_and_motion_inside_a_word_reuse_matches_without_visiting_hidden_tabs() {
    let (mut app, mut terminal) = fixture(false, false, SplitDirection::Horizontal);
    app.highlight_words_across_tabs = true;
    let rope = ropey::Rope::from_str(&"alpha beta\n".repeat(40_000));
    app.groups[1].tabs[0].editor.rope = rope.clone();
    for hidden in 0..128 {
        let mut editor = Editor::test_with_text("");
        editor.rope = rope.clone();
        let tab = Tab::new_file(editor);
        if hidden % 2 == 0 {
            app.tabs.push(tab);
        } else {
            app.groups[1].tabs.push(tab);
        }
    }
    draw(&mut app, &mut terminal);
    let word = Arc::clone(&app.shared_word_highlight.as_ref().unwrap().word);
    let cached = assert_matches(&app.groups[1].tabs[0].editor, 0, &[(0, 5)]);
    let builds = app.groups[1].tabs[0]
        .editor
        .render_line_cache
        .borrow()
        .occurrence_builds;
    assert!(builds <= usize::from(app.groups[1].code_area.unwrap().height) + 1);
    for column in [1, 2, 3, 4, 1, 4, 2, 3] {
        app.editor_mut().unwrap().cursor_col = column;
        draw(&mut app, &mut terminal);
        let target = &app.groups[1].tabs[0].editor;
        assert_eq!(target.render_line_cache.borrow().occurrence_builds, builds);
        assert!(Rc::ptr_eq(&cached, &assert_matches(target, 0, &[(0, 5)])));
        assert!(Arc::ptr_eq(
            &word,
            &app.shared_word_highlight.as_ref().unwrap().word
        ));
        let (lines, bytes, _) = target.render_line_cache.borrow().decoration_storage();
        assert!(lines <= 256 && bytes <= 2 * 1024 * 1024);
    }
    assert_eq!(app.editor_cursor_word_cache.borrow().len(), 1);
    for hidden in app
        .tabs
        .iter()
        .skip(1)
        .chain(app.groups[1].tabs.iter().skip(1))
    {
        let cache = hidden.editor.render_line_cache.borrow();
        assert_eq!(cache.occurrence_builds, 0);
        assert_eq!(cache.decoration_storage().0, 0);
    }
}

#[test]
fn large_generated_sources_and_non_text_tabs_do_not_extract_unbounded_words() {
    for mode in 0..4 {
        let mut app = App::test_new();
        let source = if mode == 1 {
            " alpha ".repeat(MAX_RICH_RENDER_LINE_CHARS)
        } else {
            "x".repeat(MAX_RICH_RENDER_LINE_CHARS * 2)
        };
        let mut editor = Editor::test_with_text(&source);
        if mode == 1 {
            editor.selection_anchor = Some((0, 0));
            editor.cursor_col = source.len();
        }
        if mode == 2 {
            editor.large_file_mode = true;
        }
        let mut tab = Tab::new_file(editor);
        if mode == 3 {
            tab.kind = TabKind::Settings;
        }
        app.tabs = vec![tab];
        app.active_tab = 0;
        app.highlight_words_across_tabs = true;
        prepare_shared_word_highlight(&mut app);
        assert!(app.shared_word_highlight.is_none());
        assert!(app.editor_cursor_word_cache.borrow().is_empty());
        assert!(app.editor_selection_word_cache.is_empty());
    }
}
