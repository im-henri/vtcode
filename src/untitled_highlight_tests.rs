use crate::{app::App, ui_chrome_tabs::TabBarHits};
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, layout::Rect, style::Color};
use std::{path::PathBuf, time::Duration};

fn app() -> App {
    let mut app = App::test_new();
    assert!(app.set_theme_by_key("dark"));
    app.show_minimap = false;
    app.sticky_scroll = false;
    app.new_file();
    app
}

fn plus(app: &mut App) {
    let area = Rect::new(0, 0, 160, 1);
    app.tab_bar_area = Some(area);
    let button = TabBarHits::new(app, area).new_tab;
    assert!(!button.is_empty());
    let count = app.tabs.len();
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
        crate::mouse::handle_mouse(app, MouseEvent {
            kind, column: button.x + 1, row: button.y, modifiers: KeyModifiers::NONE,
        });
    }
    assert_eq!(app.tabs.len(), count + 1);
    assert!(app.editor().unwrap().file_path.is_none());
    assert!(app.editor().unwrap().suggested_file_name.is_none());
}

fn language(app: &mut App, name: &str) {
    app.execute_editor_command("change_language");
    let command = crate::app_menu::language_command_id(name);
    app.palette.selected = app.palette.filtered.iter()
        .position(|&index| app.palette.commands[index].id == command).unwrap();
    crate::input_pickers::handle_palette_key(app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(!app.palette.active);
    assert_eq!(app.editor().unwrap().syntax_language_token(), Some(name));
}

fn settle(app: &mut App) {
    app.refresh_visible_highlights_async();
    app.wait_for_background(Duration::from_secs(10), |app| {
        !app.background.has_active_jobs() && !app.background.has_pending_results()
    });
    assert!(app.editor().unwrap().syntax_dirty_from.is_none());
}

fn foreground(app: &mut App) -> Color {
    let editor = app.editor_mut().unwrap();
    editor.cursor_line = editor.line_count() - 1;
    editor.cursor_col = 0;
    editor.scroll_offset = 0;
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| {
        let area = frame.area();
        crate::ui_editor::draw_editor(frame, app, area);
    }).unwrap();
    let code = app.code_area.unwrap();
    terminal.backend().buffer()[(code.x + 1, code.y)].fg
}

#[test]
fn plus_created_untitled_files_highlight_when_language_is_chosen_before_or_after_typing() {
    for wrapped in [false, true] {
        for type_first in [false, true] {
            let mut app = app();
            plus(&mut app);
            app.editor_mut().unwrap().word_wrap = wrapped;
            if type_first { app.editor_mut().unwrap().paste_with_text("int value = 42;\n"); }
            language(&mut app, "C++");
            if !type_first { app.editor_mut().unwrap().paste_with_text("int value = 42;\n"); }
            settle(&mut app);
            assert_ne!(foreground(&mut app), app.colors.fg_text, "wrapped={wrapped}, type_first={type_first}");
            let editor = app.editor().unwrap();
            assert!(editor.file_path.is_none());
            assert!(editor.suggested_file_name.is_none());
            assert!(editor.syntax_file_path().is_none(), "the highlighting identity must not become a filename");
        }
    }
}

#[test]
fn untitled_highlighting_survives_edits_and_switching_to_plain_text_and_back() {
    let mut app = app();
    plus(&mut app);
    language(&mut app, "C++");
    app.editor_mut().unwrap().paste_with_text("int value = 42;\n");
    settle(&mut app);
    let keyword = foreground(&mut app);
    assert_ne!(keyword, app.colors.fg_text);
    app.editor_mut().unwrap().paste_with_text("// a later edit\n");
    settle(&mut app);
    assert_eq!(foreground(&mut app), keyword);
    language(&mut app, "Plain Text");
    settle(&mut app);
    assert_eq!(foreground(&mut app), app.colors.fg_text);
    language(&mut app, "C++");
    settle(&mut app);
    assert_eq!(foreground(&mut app), keyword);
}

#[test]
fn different_untitled_buffers_keep_independent_syntax_caches() {
    let mut app = app();
    plus(&mut app);
    let first = app.active_tab;
    language(&mut app, "C++");
    app.editor_mut().unwrap().paste_with_text("int value = 42;\n");
    settle(&mut app);
    let first_path = app.editor().unwrap().highlight_path().to_path_buf();
    let keyword = foreground(&mut app);
    plus(&mut app);
    language(&mut app, "C++");
    app.editor_mut().unwrap().paste_with_text("// a comment only\n");
    settle(&mut app);
    assert_ne!(app.editor().unwrap().highlight_path(), first_path.as_path());
    assert_ne!(foreground(&mut app), keyword);
    app.active_tab = first;
    assert_eq!(foreground(&mut app), keyword, "another untitled file must not replace these cached colors");
    app.editor_mut().unwrap().file_path = Some(PathBuf::from("saved.cpp"));
    assert_eq!(app.editor().unwrap().highlight_path(), std::path::Path::new("saved.cpp"));
    settle(&mut app);
    assert_eq!(foreground(&mut app), keyword);
}
