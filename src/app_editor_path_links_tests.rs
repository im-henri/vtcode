use super::*;
use crate::app::{ActivePane, Tab};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, style::Modifier};
use std::time::Duration;

fn fixture(root: &Path, file: &str, text: &str) -> App {
    let mut app = App::new_deferred(root.to_owned());
    let mut editor = Editor::test_with_text(text);
    editor.file_path = Some(root.join(file));
    app.tabs.push(Tab::new_file(editor));
    app.active_pane = ActivePane::Editor;
    app.source_control_enabled = false;
    app.show_minimap = false;
    app.sticky_scroll = false;
    app
}

fn render(app: &mut App, width: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, 15)).unwrap();
    terminal.draw(|frame| crate::ui_editor::draw_editor(frame, app, frame.area())).unwrap();
    terminal.backend().buffer().clone()
}

fn cell(buffer: &Buffer, text: &str) -> (u16, u16) {
    for y in 0..buffer.area.height {
        for x in 0..=buffer.area.width.saturating_sub(text.len() as u16) {
            let actual: String = (x..x + text.len() as u16).map(|col| buffer[(col, y)].symbol()).collect();
            if actual == text { return (x, y); }
        }
    }
    panic!("Expected visible text {text:?}");
}

fn mouse(app: &mut App, at: (u16, u16), kind: MouseEventKind, modifiers: KeyModifiers) {
    crate::mouse::handle_mouse(app, MouseEvent { kind, column: at.0, row: at.1, modifiers });
}

fn click(app: &mut App, at: (u16, u16), modifiers: KeyModifiers) {
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
        mouse(app, at, kind, modifiers);
    }
}

fn wait_for_file(app: &mut App, path: &Path) {
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.editor().is_some_and(|editor| editor.file_path.as_deref() == Some(path) && !editor.bg_loading)
    });
}

#[test]
fn editor_path_links_open_log_diagnostics_with_a_workspace_fallback() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("src/main.txt");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::create_dir(root.path().join("logs")).unwrap();
    std::fs::write(&path, "123456789\n".repeat(100)).unwrap();
    // The editor hit uses source character indices rather than UTF-8 bytes
    // or terminal cells, even when the prefix includes tabs and wide glyphs.
    let source = "\t資料 error: src/main.txt:42:5\n";
    for large in [false, true] {
        let mut app = fixture(root.path(), "logs/build.log", source);
        app.editor_mut().unwrap().large_file_mode = large;
        app.editor_mut().unwrap().word_wrap = large;
        let plain = render(&mut app, 100);
        let at = cell(&plain, "main.txt");
        mouse(&mut app, at, MouseEventKind::Moved, KeyModifiers::NONE);
        app.refresh_lsp_link_hover();
        assert!(!plain[at].modifier.contains(Modifier::UNDERLINED));
        assert!(app.lsp_link_hover.path_links.key.is_none());

        mouse(&mut app, at, MouseEventKind::Moved, KeyModifiers::CONTROL);
        assert!(app.refresh_lsp_link_hover());
        assert_eq!(app.lsp_link_hover_range(), Some((0, 11, 28)));
        assert!(render(&mut app, 100)[at].modifier.contains(Modifier::UNDERLINED));
        assert_eq!(app.tabs.len(), 1);

        click(&mut app, at, KeyModifiers::CONTROL);
        wait_for_file(&mut app, &path);
        render(&mut app, 100);
        let editor = app.editor().unwrap();
        assert_eq!((editor.cursor_line, editor.cursor_col), (41, 4));
        assert!(editor.scroll_offset < 41);
        assert_eq!(app.tabs[0].editor.rope, source);
        assert!(app.editor_selection_drag.is_none());
    }
}

#[test]
fn editor_path_links_open_markdown_destinations_relative_to_the_document() {
    let root = tempfile::tempdir().unwrap();
    let docs = root.path().join("docs");
    std::fs::create_dir(&docs).unwrap();
    let path = docs.join("work notes.txt");
    std::fs::write(&path, "one\ntwo\n").unwrap();
    std::fs::write(root.path().join("work notes.txt"), "workspace copy").unwrap();
    let text = "[Open notes](work%20notes.txt#L2)\n[Web](https://example.com/a.md)\n";
    let mut app = fixture(root.path(), "docs/readme.md", text);
    app.editor_mut().unwrap().word_wrap = true;
    let buffer = render(&mut app, 30);
    let web = cell(&buffer, "Web");
    mouse(&mut app, web, MouseEventKind::Moved, KeyModifiers::CONTROL);
    app.refresh_lsp_link_hover();
    assert!(app.lsp_link_hover_range().is_none());

    // The label can wrap at its space. Click on one word in the rendered link.
    let at = cell(&buffer, "Open");
    click(&mut app, at, KeyModifiers::NONE);
    assert_eq!(app.tabs.len(), 1);
    click(&mut app, at, KeyModifiers::CONTROL);
    wait_for_file(&mut app, &path);
    assert_eq!(app.editor().unwrap().cursor_line, 1);
    assert_eq!(app.editor().unwrap().rope, "one\ntwo\n");
}

#[test]
fn editor_path_links_open_a_quoted_absolute_path_from_plain_text() {
    let root = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let path = elsewhere.path().join("other notes.txt");
    std::fs::write(&path, "stored\n").unwrap();
    let text = format!("See \"{}\".\n", path.display());
    let mut app = fixture(root.path(), "notes.txt", &text);
    let at = cell(&render(&mut app, 200), "notes.txt");
    click(&mut app, at, KeyModifiers::CONTROL);
    wait_for_file(&mut app, &path);
    assert_eq!(app.editor().unwrap().rope, "stored\n");
    assert_eq!(app.explorer.root, root.path());
}

#[test]
fn editor_path_links_limit_detection_to_the_hovered_part_of_long_lines() {
    let prefix = "x".repeat(16 * LINE_WINDOW);
    let text = format!("{prefix} (../src/notes.txt).\n");
    let editor = Editor::test_with_text(&text);
    let mut cache = Cache::default();
    assert!(cache.hit(&editor, 0, 2, false).is_none());
    let link = cache.hit(&editor, 0, prefix.len() + 4, false).unwrap();
    assert_eq!(link.location.path, "../src/notes.txt");
    assert_eq!(link.columns.start, prefix.len() + 2);
}
