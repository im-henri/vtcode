use crate::{app::{ActivePane, App, SplitDirection, Tab}, editor::Editor, text_input::TextInputId};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{backend::TestBackend, Terminal};

fn fixture() -> (tempfile::TempDir, App) {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::test_new();
    app.explorer = crate::explorer::FileExplorer::new_deferred(directory.path().into());
    app.source_control_enabled = false;
    app.lsp_enabled = false;
    app.show_explorer = false;
    app.show_panel = false;
    app.show_minimap = false;
    app.active_pane = ActivePane::Editor;
    app.tabs = ["first.rs", "second.rs"].into_iter().map(|name| {
        let mut editor = Editor::test_with_text("original code\n");
        editor.file_path = Some(directory.path().join(name));
        Tab::new_file(editor)
    }).collect();
    app.active_tab = 0;
    (directory, app)
}

fn draw(app: &mut App) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(180, 42)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    terminal
}

fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
    crate::mouse::handle_mouse(app, MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE });
}

fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, modifiers));
}

#[test]
fn save_error_over_folder_browser_is_visible_and_owns_pointer_input() {
    let (_directory, mut app) = fixture();
    app.open_folder_browser();
    let path = app.folder_browser_path.clone();
    let query = app.change_dir_query.clone();
    app.show_save_error("Asynchronous save failed while choosing a folder");
    let terminal = draw(&mut app);
    let text: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
    assert!(text.contains("Asynchronous save failed while choosing a folder"), "the active modal must be visible");
    assert_eq!(app.modal.kind, Some(crate::ui_modal::Kind::SaveError));
    let dismiss = app.modal.buttons[0].0;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), dismiss.x, dismiss.y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), dismiss.x, dismiss.y);
    assert!(app.save_error.is_none(), "the visible Dismiss button must receive the click");
    assert!(app.change_dir_active);
    assert_eq!(app.folder_browser_path, path);
    assert_eq!(app.change_dir_query, query);
}

#[test]
fn a_save_error_interrupts_find_resize_before_or_after_the_next_frame() {
    for repaint in [false, true] {
        let (_directory, mut app) = fixture();
        app.execute_palette_command("find");
        draw(&mut app);
        let bar = app.search_bar_area.unwrap();
        let width = app.search_bar_layout.width;
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), bar.x, bar.y + 1);
        assert!(app.search_bar_layout.resizing);
        app.show_save_error("Save interrupted the resize gesture");
        if repaint { draw(&mut app); }
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), bar.x - 20, bar.y + 1);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), bar.x - 20, bar.y + 1);
        assert_eq!(app.search_bar_layout.width, width, "an obscured Find widget must not resize");
        assert!(!app.search_bar_layout.resizing);
        assert!(app.save_error.is_some());
        assert_eq!(app.editor().unwrap().rope.to_string(), "original code\n");
    }
}

#[test]
fn clicking_find_cancels_a_pending_editor_chord_before_typing() {
    let (_directory, mut app) = fixture();
    app.execute_palette_command("find");
    draw(&mut app);
    let code = app.code_area.unwrap();
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), code.x + 2, code.y + 6);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), code.x + 2, code.y + 6);
    key(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
    assert!(app.chord_pending);
    let field = app.text_input_area(TextInputId::SearchFind).unwrap();
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), field.x + 1, field.y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), field.x + 1, field.y);
    key(&mut app, KeyCode::Char('w'), KeyModifiers::NONE);
    assert_eq!(app.search_query, "w");
    assert_eq!(app.tabs.len(), 2, "typing into Find must not run Ctrl+K W (close all tabs)");
    assert!(!app.confirm_close_active && !app.chord_pending);
}

#[test]
fn an_asynchronous_modal_cancels_a_pending_chord_before_returning_to_the_editor() {
    let (_directory, mut app) = fixture();
    key(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
    assert!(app.chord_pending);
    app.show_save_error("Save failed during a pending shortcut");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.save_error.is_none());
    key(&mut app, KeyCode::Char('w'), KeyModifiers::NONE);
    assert_eq!(app.tabs.len(), 2);
    assert!(app.editor().unwrap().rope.to_string().starts_with('w'));
    assert!(!app.confirm_close_active && !app.chord_pending);
}

#[test]
fn an_inactive_short_file_does_not_borrow_the_active_files_horizontal_overflow() {
    let (_directory, mut app) = fixture();
    app.tabs[0].editor = Editor::test_with_text(&"x".repeat(5_000));
    app.create_split_group_dir(1, SplitDirection::Horizontal);
    app.switch_group(0);
    app.cached_max_line_width = 5_000;
    draw(&mut app);
    assert!(app.h_scrollbar_area.is_some());
    assert!(app.groups[1].h_scrollbar_area.is_none(), "the short file fits its pane and needs no horizontal scrollbar");
    assert_eq!(app.groups[1].tabs[0].editor.horizontal_scroll, 0);
    app.switch_group(1);
    app.cached_max_line_width = 13;
    draw(&mut app);
    assert!(app.h_scrollbar_area.is_none());
    assert!(app.groups[0].h_scrollbar_area.is_some(), "an inactive long file must retain its own overflow");
}
