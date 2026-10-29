use super::*;
use crate::app::Tab;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
use std::time::{Duration, Instant};

const SAVED: &str = "fn main() {\n    let value = 1;\n}\n";
const CURRENT: &str = "fn main() {\n    let value = 2;\n}\n";

fn app(current: &str) -> App {
    let mut app = App::new_deferred(std::env::temp_dir());
    app.source_control_enabled = false;
    let mut editor = crate::editor::Editor::test_with_text(current);
    editor.file_path = Some("editor.rs".into());
    editor.saved_rope = Some(ropey::Rope::from_str(SAVED));
    editor.modified = current != SAVED;
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.show_minimap = false;
    app.sticky_scroll = false;
    app.editor_area = Some(Rect::new(10, 3, 110, 24));
    app.gutter_area = Some(Rect::new(10, 3, 6, 24));
    app.code_area = Some(Rect::new(16, 3, 104, 24));
    app
}

fn open(app: &mut App) {
    app.open_hunk_popup(10, 4, Some("editor.rs".into()), 1, 'N');
}

fn render(app: &mut App, terminal: &mut Terminal<TestBackend>) -> String {
    app.loading.begin_frame();
    terminal.draw(|frame| {
        if app.hunk_popup_active { crate::ui_dialogs::draw_hunk_popup(frame, app); }
    }).unwrap();
    crate::ui_loading::finish_frame(app, Instant::now());
    terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect()
}

fn key(app: &mut App, code: KeyCode) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, KeyModifiers::NONE));
}

fn click(app: &mut App, column: u16, row: u16) {
    crate::mouse::handle_mouse(app, MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE,
    });
}

fn cancel(app: &App) -> CancellationToken {
    let FeedbackState::Loading(cancel) = &app.hunk_preview_feedback.as_ref().unwrap().state else {
        panic!("Expected a pending preview");
    };
    cancel.clone()
}

#[test]
fn hunk_preview_loading_is_immediate_and_stops_animating_when_ready() {
    let mut app = app(CURRENT);
    open(&mut app);
    assert!(app.hunk_peek.is_none());
    let mut terminal = Terminal::new(TestBackend::new(130, 32)).unwrap();
    let text = render(&mut app, &mut terminal);
    assert!(text.contains("editor.rs:2"));
    assert!(text.contains("Loading changes…"));
    assert!(text.contains("Esc to close preview"));
    assert!(!text.contains("Cancel") && !text.contains("Stage Hunk"));
    assert!(app.loading.next_wait(true, Instant::now()).is_some());
    crate::ui_control_visibility_tests::snapshot(terminal.backend().buffer(), "Loading gutter change preview");
    // Pressing Enter while the request is pending must not invoke an action.
    key(&mut app, KeyCode::Enter);
    assert!(app.hunk_popup_active);
    key(&mut app, KeyCode::Tab); // Focus Close while loading.
    app.wait_for_hunk_preview();
    key(&mut app, KeyCode::Enter);
    assert!(app.hunk_popup_active, "A focused loading Close must not turn into Revert after loading");
    assert_eq!(app.editor().unwrap().rope.to_string(), CURRENT);
    let text = render(&mut app, &mut terminal);
    assert!(text.contains("Unsaved Changes"));
    assert!(text.contains("let value = 1;") && text.contains("let value = 2;"));
    assert!(!text.contains("Loading changes") && !text.contains("Cancel"));
    assert!(app.loading.next_wait(true, Instant::now()).is_none());
    crate::ui_control_visibility_tests::snapshot(terminal.backend().buffer(), "Ready gutter change preview");
}

#[test]
fn hunk_preview_close_uses_rendered_geometry_and_rejects_late_results() {
    for action in ["close button", "escape", "outside", "keyboard close"] {
        let mut app = app(CURRENT);
        open(&mut app);
        let cancel = cancel(&app);
        let mut terminal = Terminal::new(TestBackend::new(130, 32)).unwrap();
        render(&mut app, &mut terminal);
        let area = app.hunk_popup_rect(130, 32);
        match action {
            "close button" => {
                assert!(area.right() > 80, "Use the frame rather than terminal-size fallback");
                click(&mut app, area.right() - 2, area.y + 1);
            }
            "escape" => key(&mut app, KeyCode::Esc),
            "outside" => click(&mut app, area.x, area.y - 1),
            _ => {
                key(&mut app, KeyCode::Tab);
                key(&mut app, KeyCode::Enter);
            }
        }
        assert!(!app.hunk_popup_active, "{action}");
        assert!(cancel.is_cancelled(), "{action}");
        app.wait_for_background(Duration::from_secs(10), |app| {
            !app.background.has_active_jobs() && !app.background.has_pending_results()
        });
        assert!(app.hunk_peek.is_none() && app.hunk_preview_feedback.is_none(), "{action}");
        render(&mut app, &mut terminal);
        assert!(app.loading.next_wait(true, Instant::now()).is_none());
        assert_eq!(app.editor().unwrap().rope.to_string(), CURRENT);
    }
}

#[test]
fn hunk_preview_errors_stay_inline_and_retry_the_current_snapshot() {
    let mut app = app(SAVED);
    open(&mut app);
    let mut terminal = Terminal::new(TestBackend::new(130, 32)).unwrap();
    render(&mut app, &mut terminal);
    app.wait_for_hunk_preview();
    let text = render(&mut app, &mut terminal);
    assert!(text.contains("Unable to load change preview."));
    assert!(text.contains("No changes found") && text.contains("Retry"));
    assert!(!text.contains("Cancel") && !text.contains("Stage Hunk"));
    assert!(app.loading.next_wait(true, Instant::now()).is_none());
    crate::ui_control_visibility_tests::snapshot(terminal.backend().buffer(), "Failed gutter change preview");
    let generation = app.hunk_popup_generation;
    let editor = app.editor_mut().unwrap();
    editor.rope = ropey::Rope::from_str(CURRENT);
    editor.modified = true;
    editor.content_version += 1;
    let area = app.hunk_popup_rect(130, 32);
    click(&mut app, area.right() - 5, area.y + 1);
    assert!(app.hunk_popup_generation != generation);
    assert!(app.hunk_preview_feedback.as_ref().unwrap().loading());
    app.wait_for_hunk_preview();
    assert!(app.hunk_peek.as_ref().unwrap().rows.iter().any(|row| row.text.contains("value = 2")));
}

#[test]
fn hunk_preview_superseded_requests_cannot_replace_the_newer_click() {
    let mut app = app(CURRENT);
    open(&mut app);
    let old = app.hunk_popup_generation;
    let old_document = app.editor().map(|editor| (editor.navigation_id, editor.content_version));
    let old_cancel = cancel(&app);
    let editor = app.editor_mut().unwrap();
    editor.rope = ropey::Rope::from_str(&CURRENT.replace("= 2", "= 3"));
    editor.content_version += 1;
    open(&mut app);
    assert!(old_cancel.is_cancelled());
    assert!(!app.apply_hunk_preview(PeekResult {
        generation: old, document: old_document, result: Err("Old connection failed".into()),
    }));
    app.wait_for_hunk_preview();
    let peek = app.hunk_peek.as_ref().unwrap();
    assert!(peek.rows.iter().any(|row| row.text.contains("value = 3")));
    assert!(!peek.rows.iter().any(|row| row.text.contains("value = 2")));
}

#[test]
fn hunk_preview_document_changes_invalidate_inflight_results() {
    let mut app = app(CURRENT);
    open(&mut app);
    app.editor_mut().unwrap().content_version += 1;
    app.wait_for_hunk_preview();
    assert!(!app.hunk_popup_active && app.hunk_peek.is_none());
}

#[test]
fn hunk_preview_loading_and_errors_fit_small_and_resized_editors() {
    let mut app = app(CURRENT);
    open(&mut app);
    for failed in [false, true] {
        if failed {
            cancel(&app).cancel();
            app.hunk_preview_feedback = Some(Feedback::new(FeedbackState::Failed("Connection closed".into())));
        }
        for (width, height) in [(0, 0), (1, 1), (3, 2), (12, 4), (45, 12), (130, 32)] {
            app.gutter_area = None;
            app.code_area = None;
            app.editor_area = Some(Rect::new(0, 0, width, height));
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            render(&mut app, &mut terminal);
            let area = app.hunk_popup_rect(width, height);
            assert_eq!(area.intersection(Rect::new(0, 0, width, height)), area);
            assert_eq!(app.hunk_preview_feedback.as_ref().unwrap().area, Some(area));
            if height > 1 && height < 4 {
                click(&mut app, area.right().saturating_sub(1), area.y + 1);
                assert!(app.hunk_popup_active, "Clipped controls must not leave invisible click targets");
            }
        }
    }
    app.close_hunk_popup();
}
