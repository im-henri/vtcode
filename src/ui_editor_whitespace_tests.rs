use crate::{
    app::{App, Tab, WhitespaceMode},
    editor::Editor,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect, style::Modifier};

fn fixture(source: &str, extension: &str, wrapped: bool) -> App {
    let path = std::path::PathBuf::from(format!("comment-whitespace.{extension}"));
    let mut editor = Editor::test_with_text(source);
    editor.file_path = Some(path.clone());
    editor.word_wrap = wrapped;
    editor.whitespace_mode = WhitespaceMode::Trailing;
    editor.cursor_line = editor.rope.len_lines() - 1;
    let rope = editor.rope.clone();
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.show_line_numbers = false;
    app.show_minimap = false;
    app.show_indent_guides = false;
    app.sticky_scroll = false;
    app.cursor_visible = false;
    let job = app
        .highlighter
        .prepare_job(
            &rope,
            &path,
            0,
            None,
            0..rope.len_lines(),
            None,
            app.syntax_theme.as_str(),
        )
        .unwrap();
    let (syntaxes, themes) = app.highlighter.shared_sets();
    assert!(
        app.highlighter
            .apply_batch(job.run(&rope, &syntaxes, &themes, || false))
    );
    app
}

fn draw(app: &mut App, width: u16) -> Buffer {
    let area = Rect::new(0, 0, width, 16);
    let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
    terminal
        .draw(|frame| super::draw_editor(frame, app, area))
        .unwrap();
    terminal.backend().buffer().clone()
}

fn row(app: &App, buffer: &Buffer, line: u16) -> String {
    let area = app.code_area.unwrap();
    (area.x..area.right())
        .map(|x| buffer[(x, area.y + line)].symbol())
        .collect()
}

#[test]
fn comment_whitespace_overrides_only_comments_and_inherit_tracks_general_mode() {
    use WhitespaceMode::{All, None as Hidden, Trailing};
    let source =
        "int n = 1; /* α a\tb  c\n * d\te f  \n */ int z = 2;  \nchar *s = \"/* a b */\";\n";
    for wrapped in [false, true] {
        let mut app = fixture(source, "c", wrapped);
        for (general, comments) in [
            (Trailing, Some(All)),
            (Hidden, Some(All)),
            (All, Some(Hidden)),
            (Hidden, None),
            (Trailing, None),
            (All, None),
        ] {
            app.editor_mut().unwrap().whitespace_mode = general;
            app.comment_whitespace_mode = comments;
            let buffer = draw(&mut app, 80);
            let area = app.code_area.unwrap();
            for (line_idx, text) in source.lines().enumerate() {
                let mut visual_col = 0;
                let chars: Vec<_> = text.chars().collect();
                let trailing = text.trim_end().chars().count();
                for (col, &ch) in chars.iter().enumerate() {
                    // Explicit source ranges: code before/after block delimiters,
                    // a complete continuation, and a string containing delimiters.
                    let in_comment = match line_idx {
                        0 => col >= 11,
                        1 => true,
                        2 => col < 3,
                        _ => false,
                    };
                    let mode = if in_comment {
                        comments.unwrap_or(general)
                    } else {
                        general
                    };
                    if matches!(ch, ' ' | '\t') {
                        let marked = mode == All || (mode == Trailing && col >= trailing);
                        let expected = if !marked {
                            " "
                        } else if ch == '\t' {
                            "→"
                        } else {
                            "·"
                        };
                        let cell = &buffer[(area.x + visual_col as u16, area.y + line_idx as u16)];
                        assert_eq!(
                            cell.symbol(),
                            expected,
                            "wrapped={wrapped}, general={general:?}, comments={comments:?}, line={line_idx}, col={col}"
                        );
                        if marked {
                            assert_eq!(cell.fg, app.colors.ui.fg_whitespace);
                            assert!(!cell.modifier.contains(Modifier::DIM));
                        }
                    }
                    visual_col += if ch == '\t' { 4 - visual_col % 4 } else { 1 };
                }
            }
        }
        app.comment_whitespace_mode = Some(All);
        app.editor_mut().unwrap().whitespace_mode = Trailing;
        let buffer = draw(&mut app, 80);
        if !wrapped {
            crate::ui_control_visibility_tests::snapshot(&buffer, "comment-whitespace");
        }
    }
}

#[test]
fn comment_whitespace_uses_grammars_across_languages() {
    for (extension, source, expected) in [
        (
            "rs",
            "let s = \"// a b\"; // α a b\n",
            "let s = \"// a b\"; //·α·a·b",
        ),
        ("py", "s = '# a b' # α a b\n", "s = '# a b' #·α·a·b"),
        (
            "html",
            "<p>a b</p><!-- α a b -->\n",
            "<p>a b</p><!--·α·a·b·-->",
        ),
    ] {
        for wrapped in [false, true] {
            let mut app = fixture(source, extension, wrapped);
            app.editor_mut().unwrap().whitespace_mode = WhitespaceMode::None;
            let buffer = draw(&mut app, 80);
            assert_eq!(
                row(&app, &buffer, 0).trim_end(),
                expected,
                "{extension}, wrapped={wrapped}"
            );
        }
    }
}

#[test]
fn comment_whitespace_selection_boundary_and_trailing_modes() {
    use WhitespaceMode::{All, Boundary, None as Hidden, Selection, Trailing};
    for wrapped in [false, true] {
        let mut app = fixture("/* a b  c\td */  \n", "c", wrapped);
        app.editor_mut().unwrap().whitespace_mode = Hidden;
        for (mode, expected) in [
            (All, "/*·a·b··c→  d·*/"),
            (Boundary, "/* a b··c→  d */"),
            (Trailing, "/* a b  c   d */"),
            (Selection, "/* a·b  c   d */"),
        ] {
            app.comment_whitespace_mode = Some(mode);
            let editor = app.editor_mut().unwrap();
            editor.selection_anchor = Some((0, 3));
            editor.cursor_line = 0;
            editor.cursor_col = 6;
            let buffer = draw(&mut app, 80);
            assert_eq!(
                row(&app, &buffer, 0).trim_end(),
                expected,
                "{mode:?}, wrapped={wrapped}"
            );
            let area = app.code_area.unwrap();
            assert_eq!(buffer[(area.x + 4, area.y)].bg, app.colors.bg_selection);
        }
    }
}

#[test]
fn comment_whitespace_survives_wrapped_continuations_and_horizontal_scroll() {
    let source = format!("/*{}*/\n", " word\t pair ".repeat(8));
    for wrapped in [false, true] {
        let mut app = fixture(&source, "c", wrapped);
        app.editor_mut().unwrap().whitespace_mode = WhitespaceMode::None;
        if !wrapped {
            app.editor_mut().unwrap().horizontal_scroll = 18;
        }
        let buffer = draw(&mut app, 32);
        assert!(row(&app, &buffer, 0).contains('→'));
        assert!(row(&app, &buffer, 0).contains('·'));
        if wrapped {
            for line in 1..4 {
                let text = row(&app, &buffer, line);
                assert!(text.contains('→'), "continuation {line}: {text}");
                assert!(text.contains('·'), "continuation {line}: {text}");
            }
        }
    }
}

#[test]
fn boundary_mode_renders_internal_tabs_with_or_without_wrapping() {
    for wrapped in [false, true] {
        let mut app = fixture("a\tb c  d\n", "txt", wrapped);
        app.comment_whitespace_mode = None;
        app.editor_mut().unwrap().whitespace_mode = WhitespaceMode::Boundary;
        let buffer = draw(&mut app, 80);
        assert_eq!(row(&app, &buffer, 0).trim_end(), "a→  b c··d");
    }
}

#[test]
fn indent_guides_preserve_each_whitespace_mode_in_one_column() {
    for wrapped in [false, true] {
        let source = "        value  \nroot\n";
        for (mode, expected) in [
            (WhitespaceMode::None, "│   │   value"),
            (WhitespaceMode::All, "┆···┆···value··"),
            (WhitespaceMode::Boundary, "┆···┆···value··"),
            (WhitespaceMode::Selection, "│  ·┆·  value"),
            (WhitespaceMode::Trailing, "│   │   value··"),
        ] {
            let mut app = fixture(source, "txt", wrapped);
            app.show_indent_guides = true;
            app.editor_mut().unwrap().whitespace_mode = mode;
            if mode == WhitespaceMode::Selection {
                let editor = app.editor_mut().unwrap();
                editor.selection_anchor = Some((0, 3));
                editor.cursor_line = 0;
                editor.cursor_col = 6;
            }
            let buffer = draw(&mut app, 80);
            assert_eq!(row(&app, &buffer, 0).trim_end(), expected, "{mode:?}, wrapped={wrapped}");
            let area = app.code_area.unwrap();
            assert_eq!(buffer[(area.x + 8, area.y)].symbol(), "v");
            assert_eq!(app.editor().unwrap().rope.to_string(), source);
        }
    }
    for symbol in ['│', '┆', '↦'] { assert_eq!(unicode_width::UnicodeWidthChar::width(symbol), Some(1)); }
}

#[test]
fn tab_and_mixed_indentation_share_visual_guide_columns() {
    for wrapped in [false, true] {
        for (indent, expected) in [
            ("\t\t", "↦   ↦   value"),
            ("\t    ", "↦   ┆···value"),
            ("  \t    ", "┆·→ ┆···value"),
        ] {
            let source = format!("{indent}value\nroot\n");
            let mut app = fixture(&source, "txt", wrapped);
            app.show_indent_guides = true;
            app.editor_mut().unwrap().whitespace_mode = WhitespaceMode::All;
            let buffer = draw(&mut app, 80);
            assert_eq!(row(&app, &buffer, 0).trim_end(), expected, "{indent:?}, wrapped={wrapped}");
            crate::ui_control_visibility_tests::snapshot(&buffer, &format!("tab-indent-guides-{}-wrapped-{wrapped}", indent.len()));
            app.editor_mut().unwrap().whitespace_mode = WhitespaceMode::None;
            let buffer = draw(&mut app, 80);
            assert_eq!(row(&app, &buffer, 0).trim_end(), "│   │   value");
            assert_eq!(app.editor().unwrap().rope.to_string(), source);
        }
    }
}

#[test]
fn every_builtin_draws_visible_guides_and_marks_the_closest_level_active() {
    for wrapped in [false, true] {
        let mut app = fixture("            value\n        cursor\n", "txt", wrapped);
        app.loaded_themes = crate::app_theme_loader::load_builtin_themes();
        app.show_indent_guides = true;
        app.editor_mut().unwrap().cursor_line = 1;
        app.editor_mut().unwrap().whitespace_mode = WhitespaceMode::None;
        for key in ["dark", "light", "monokai", "high-contrast"] {
            assert!(app.set_theme_by_key(key));
            let buffer = draw(&mut app, 80);
            let area = app.code_area.unwrap();
            for (depth, column) in [0, 4, 8].into_iter().enumerate() {
                let cell = &buffer[(area.x + column, area.y)];
                assert_eq!(cell.symbol(), "│", "{key} wrapped={wrapped} col={column}");
                assert_eq!(cell.fg, app.colors.ui.indent_guide_color(depth, column == 4));
                assert_ne!(cell.fg, cell.bg, "{key} wrapped={wrapped} col={column}");
                assert!(!cell.modifier.contains(Modifier::DIM));
            }
            crate::ui_control_visibility_tests::snapshot(&buffer, &format!("indent-guides-{key}-wrapped-{wrapped}"));
            app.editor_mut().unwrap().whitespace_mode = WhitespaceMode::All;
            let buffer = draw(&mut app, 80);
            crate::ui_control_visibility_tests::snapshot(&buffer, &format!("indent-and-whitespace-{key}-wrapped-{wrapped}"));
            app.editor_mut().unwrap().whitespace_mode = WhitespaceMode::None;
        }
    }
}

#[test]
fn whitespace_only_rows_wrap_and_scroll_without_losing_or_repeating_guides() {
    let source = format!("{}value {}\n{}\nroot\n", " ".repeat(24), "word ".repeat(20), " ".repeat(8));
    for wrapped in [false, true] {
        let mut app = fixture(&source, "txt", wrapped);
        app.show_indent_guides = true;
        app.editor_mut().unwrap().whitespace_mode = WhitespaceMode::All;
        if wrapped {
            let buffer = draw(&mut app, 20);
            let guides = buffer.content.iter().filter(|cell| cell.symbol() == "┆").count();
            assert_eq!(guides, 8, "six guides in the code indentation and two in the blank row");
        } else {
            app.editor_mut().unwrap().horizontal_scroll = 6;
            let buffer = draw(&mut app, 40);
            assert!(row(&app, &buffer, 0).starts_with("··┆···┆···┆···┆···value"));
            app.editor_mut().unwrap().horizontal_scroll = 0;
            app.editor_mut().unwrap().whitespace_mode = WhitespaceMode::Trailing;
            let buffer = draw(&mut app, 40);
            assert_eq!(row(&app, &buffer, 1).trim_end(), "┆···┆···");
        }
    }
}

#[test]
fn guides_leave_selection_and_cursor_foregrounds_in_charge() {
    use ratatui::style::Color::Rgb;
    for wrapped in [false, true] {
        let mut app = fixture("        value\nroot\n", "txt", wrapped);
        app.show_indent_guides = true;
        app.colors.ui.fg_editor_selection = Some(Rgb(240, 210, 180));
        let editor = app.editor_mut().unwrap();
        editor.whitespace_mode = WhitespaceMode::All;
        editor.selection_anchor = Some((0, 0));
        editor.cursor_line = 0;
        editor.cursor_col = 7;
        let buffer = draw(&mut app, 80);
        let area = app.code_area.unwrap();
        for column in [0, 4] {
            let cell = &buffer[(area.x + column, area.y)];
            assert_eq!(cell.symbol(), "┆");
            assert_eq!(cell.fg, Rgb(240, 210, 180));
            assert_eq!(cell.bg, app.colors.bg_selection);
        }
        let editor = app.editor_mut().unwrap();
        editor.selection_anchor = None;
        editor.cursor_col = 4;
        app.cursor_visible = true;
        let buffer = draw(&mut app, 80);
        let cell = &buffer[(area.x + 4, area.y)];
        assert_eq!(cell.symbol(), "┆");
        assert_eq!(cell.fg, app.colors.fg_cursor);
        assert_eq!(cell.bg, app.colors.bg_cursor);
    }
}
