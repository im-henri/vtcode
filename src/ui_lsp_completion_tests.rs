use super::*;
use crate::{
    app::{ActivePane, Tab},
    editor::Editor,
};
use ratatui::{Terminal, backend::TestBackend};

fn fixture(count: usize) -> App {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text("\n\n\n\n\n\n\n\nmeasure\n");
    editor.set_cursor(8, 7);
    app.tabs = vec![Tab::new_file(editor)];
    app.active_pane = ActivePane::Editor;
    app.code_area = Some(Rect::new(4, 2, 112, 34));
    app.lsp_completion_active = true;
    app.lsp_manager.completion_items = (0..count).map(|index| LspCompletionItem {
        label: format!("measurements_{index:02}"),
        insert_text: format!("measurements_{index:02}"),
        kind: Some(lsp_types::CompletionItemKind::FUNCTION),
        detail: Some(format!("measurements_{index:02}(samples: list[float], *, window: int = 10) -> float")),
        documentation: Some("**Average** the samples without changing the input.\n\n- Accepts floating point values.\n- Returns the arithmetic mean.\n\n```python\nresult = measurements(samples)\n```\n\n".repeat(5)),
        text_edit_range: None, additional_text_edits: vec![], is_snippet: false,
        original_item: None,
    }).collect();
    app
}

fn render(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| draw(frame, app)).unwrap();
    terminal.backend().buffer().clone()
}

fn text(buffer: &ratatui::buffer::Buffer) -> String {
    buffer.content.iter().map(|cell| cell.symbol()).collect()
}

fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, modifiers));
}

fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16) {
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        },
    );
}

#[test]
fn completion_layout_stays_in_the_active_editor_and_never_covers_the_caret() {
    let mut app = fixture(200);
    for viewport in [Rect::new(0, 0, 120, 40), Rect::new(6, 5, 50, 16)] {
        for width in [0, 1, 7, 8, 16, 40, 64, 112] {
            for height in [0, 1, 3, 4, 8, 20, 34] {
                for cursor_line in [0, 3, 8] {
                    app.editor_mut().unwrap().set_cursor(cursor_line, 0);
                    app.code_area = Some(Rect::new(4, 2, width, height));
                    for selected in [0, 15, 199, usize::MAX] {
                        app.lsp_completion_selected = selected;
                        if let Some(layout) = layout(&mut app, viewport) {
                            let bounds = app.code_area.unwrap().intersection(viewport);
                            let area = layout.list.popup_area;
                            assert_eq!(area.intersection(bounds), area);
                            assert!(layout.list.inner_area.height > 0);
                            let visible = selected.min(199);
                            assert!(
                                visible >= layout.list.scroll
                                    && visible
                                        < layout.list.scroll
                                            + layout.list.inner_area.height as usize
                            );
                            if let Some(details) = layout.details {
                                assert_eq!(details.intersection(bounds), details);
                                assert!(details.intersection(area).is_empty());
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn completion_paging_and_documentation_keys_leave_the_caret_and_text_unchanged() {
    let mut app = fixture(45);
    let before = app.editor().unwrap().rope.clone();
    render(&mut app, 120, 40);
    key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
    assert_eq!(app.lsp_completion_selected, 10);
    key(&mut app, KeyCode::PageUp, KeyModifiers::NONE);
    assert_eq!(app.lsp_completion_selected, 0);
    key(&mut app, KeyCode::PageDown, KeyModifiers::ALT);
    assert!(app.completion_ui.doc_scroll > 0);
    key(&mut app, KeyCode::Char(' '), KeyModifiers::CONTROL);
    assert!(!app.completion_ui.details_visible);
    assert!(app.lsp_completion_active);
    key(&mut app, KeyCode::Char(' '), KeyModifiers::CONTROL);
    assert!(app.completion_ui.details_visible);
    assert_eq!(app.editor().unwrap().rope, before);
    assert_eq!(
        (
            app.editor().unwrap().cursor_line,
            app.editor().unwrap().cursor_col
        ),
        (8, 7)
    );
}

#[test]
fn completion_mouse_documentation_does_not_accept_or_click_through_and_wheel_stays_local() {
    let mut app = fixture(45);
    render(&mut app, 120, 40);
    let layout = app.completion_ui.layout.unwrap();
    let docs = layout.details.unwrap();
    let before = app.editor().unwrap().rope.clone();
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        docs.x + 2,
        docs.y + 2,
    );
    assert!(app.lsp_completion_active);
    mouse(&mut app, MouseEventKind::ScrollDown, docs.x + 2, docs.y + 2);
    assert!(app.completion_ui.doc_scroll > 0);
    assert_eq!(app.lsp_completion_selected, 0);
    mouse(
        &mut app,
        MouseEventKind::ScrollDown,
        layout.list.inner_area.x,
        layout.list.inner_area.y,
    );
    assert_eq!(app.lsp_completion_selected, 3);
    assert_eq!(app.editor().unwrap().scroll_offset, 0);
    assert_eq!(app.editor().unwrap().rope, before);
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        layout.toggle.x,
        layout.toggle.y,
    );
    assert!(!app.completion_ui.details_visible);
}

#[test]
fn completion_scrollbar_drag_captures_release_outside_the_popup() {
    let mut app = fixture(100);
    render(&mut app, 120, 40);
    let docs = app.completion_ui.layout.unwrap().details.unwrap();
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        docs.right() - 1,
        docs.y + 1,
    );
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        docs.right() - 1,
        39,
    );
    assert!(app.completion_ui.doc_scroll > 0);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 119, 39);
    assert!(app.completion_ui.scroll_drag.is_none());
    assert!(app.lsp_completion_active);
    assert_eq!(app.editor().unwrap().cursor_col, 7);

    let list = app.completion_ui.layout.unwrap().list;
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        list.popup_area.right() - 1,
        list.inner_area.bottom() - 1,
    );
    assert_eq!(
        app.lsp_completion_selected, 99,
        "the last track cell must reach the last result"
    );
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 119, 39);
    render(&mut app, 120, 40);
    let docs = app.completion_ui.layout.unwrap().details.unwrap();
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        docs.right() - 1,
        docs.y + 1,
    );
    app.completion_ui.details_visible = false;
    render(&mut app, 120, 40);
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 119, 39);
    assert!(
        app.completion_ui.scroll_drag.is_none(),
        "hiding/resizing details ends its drag safely"
    );
}

#[test]
fn completion_documentation_is_wrapped_markdown_and_reused_until_the_selection_changes() {
    let mut app = fixture(30);
    let buffer = render(&mut app, 120, 40);
    let rendered = text(&buffer);
    assert!(rendered.contains("LSP suggestions"));
    assert!(rendered.contains("1/30"));
    assert!(rendered.contains("Average"));
    assert!(!rendered.contains("**Average**"));
    let doc = app.completion_ui.doc.as_ref().unwrap() as *const _;
    let key = app.completion_ui.doc_key;
    render(&mut app, 120, 40);
    assert_eq!(doc, app.completion_ui.doc.as_ref().unwrap() as *const _);
    assert_eq!(key, app.completion_ui.doc_key);
    app.completion_ui.doc_scroll = 3;
    app.lsp_completion_selected = 1;
    render(&mut app, 120, 40);
    assert_eq!(app.completion_ui.doc_scroll, 0);
    crate::ui_control_visibility_tests::snapshot(&buffer, "lsp-completion-redesign-wide");
    app.code_area = Some(Rect::new(0, 0, 48, 30));
    let buffer = render(&mut app, 48, 30);
    assert!(
        app.completion_ui.layout.unwrap().details.is_some(),
        "narrow panes stack documentation"
    );
    crate::ui_control_visibility_tests::snapshot(&buffer, "lsp-completion-redesign-narrow");
}

#[test]
fn completion_metadata_deprecation_and_plaintext_docs_are_not_misinterpreted() {
    let mut app = fixture(2);
    let original: lsp_types::CompletionItem = serde_json::from_value(serde_json::json!({
        "label":"old_measurements", "labelDetails":{"description":"statistics"}, "tags":[1],
        "documentation":{"kind":"plaintext", "value":"Literal **stars** and a[0]"}
    }))
    .unwrap();
    let item = &mut app.lsp_manager.completion_items[0];
    item.label = original.label.clone();
    item.documentation = Some("Literal **stars** and a[0]".into());
    item.original_item = Some(original);
    let buffer = render(&mut app, 120, 40);
    assert!(text(&buffer).contains("Literal **stars** and a[0]"));
    assert!(text(&buffer).contains("Deprecated"));
    assert!(text(&buffer).contains("statistics"));
    let body = app.completion_ui.layout.unwrap().list.inner_area;
    assert!(
        buffer[(body.x + 4, body.y)]
            .modifier
            .contains(Modifier::CROSSED_OUT)
    );
}

#[test]
fn completion_loading_empty_and_error_states_are_explicit_only_and_escape_dismisses() {
    let mut app = fixture(0);
    for status in [
        "Loading suggestions…",
        "No suggestions",
        "Unavailable — Ctrl+Space to retry",
    ] {
        app.completion_ui.status = Some(status);
        app.completion_ui.explicit = true;
        let buffer = render(&mut app, 120, 40);
        assert!(text(&buffer).contains(status));
        app.completion_ui.explicit = false;
        assert!(!visible(&app));
    }
    app.completion_ui.explicit = true;
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!visible(&app));
}

#[test]
fn completion_acceptance_never_treats_modified_enter_or_tab_as_accept() {
    for code in [KeyCode::Enter, KeyCode::Tab] {
        let mut app = fixture(2);
        assert!(!handle_key(
            &mut app,
            KeyEvent::new(code, KeyModifiers::CONTROL)
        ));
        assert_eq!(app.editor().unwrap().rope.line(8).to_string(), "measure\n");
    }
}

#[test]
fn signature_overload_controls_do_not_move_or_edit_the_document() {
    let mut app = fixture(2);
    app.lsp_signature_active = true;
    app.lsp_manager.signature_help = Some(crate::lsp::LspSignatureHelp {
        signatures: (0..3)
            .map(|index| crate::lsp::LspSignatureInfo {
                label: format!("overload_{index}(parameter: int)"),
                parameters: vec![(11, 25)],
                documentation: None,
            })
            .collect(),
        active_signature: 0,
        active_parameter: 0,
    });
    let before = app.editor().unwrap().rope.clone();
    key(&mut app, KeyCode::Char('['), KeyModifiers::ALT);
    assert_eq!(
        app.lsp_manager
            .signature_help
            .as_ref()
            .unwrap()
            .active_signature,
        2
    );
    key(&mut app, KeyCode::Char(']'), KeyModifiers::ALT);
    assert_eq!(
        app.lsp_manager
            .signature_help
            .as_ref()
            .unwrap()
            .active_signature,
        0
    );
    assert_eq!(app.editor().unwrap().rope, before);
    assert!(app.lsp_completion_active);
}

#[test]
fn signature_wrap_preserves_active_parameter_highlighting_and_completion_stays_on_top() {
    let mut app = fixture(20);
    app.lsp_signature_active = true;
    let label = "measurements(samples: list[float], window_size: int, default: float = 0) -> float";
    let start = label.find("window_size").unwrap();
    app.lsp_manager.signature_help = Some(crate::lsp::LspSignatureHelp {
        signatures: vec![crate::lsp::LspSignatureInfo {
            label: label.into(),
            parameters: vec![(start, start + "window_size: int".len())],
            documentation: Some("Uses the requested window.".into()),
        }],
        active_signature: 0,
        active_parameter: 0,
    });
    app.code_area = Some(Rect::new(4, 2, 48, 34));
    let mut terminal = Terminal::new(TestBackend::new(60, 40)).unwrap();
    terminal
        .draw(|frame| crate::ui_dialogs::draw_lsp_signature_help(frame, &app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    assert!(text(buffer).contains("window_size"));
    let highlighted: String = buffer
        .content
        .iter()
        .filter(|cell| cell.modifier.contains(Modifier::BOLD))
        .map(|cell| cell.symbol())
        .collect();
    assert!(highlighted.contains("window_size: int"));
    let completion = render(&mut app, 60, 40);
    let area = app.completion_ui.layout.unwrap().list.popup_area;
    terminal
        .draw(|frame| {
            draw(frame, &mut app);
            crate::ui_dialogs::draw_lsp_signature_help(frame, &app);
        })
        .unwrap();
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            assert_eq!(
                terminal.backend().buffer()[(x, y)],
                completion[(x, y)],
                "signature obscured suggestion at {x},{y}"
            );
        }
    }
}

#[test]
fn explicit_empty_popup_never_accepts_a_hidden_local_result() {
    let mut app = fixture(0);
    app.completion_ui.explicit = true;
    app.completion_ui.status = Some("No suggestions");
    app.vtcomplete_active = true;
    app.vtcomplete_suggestions = vec!["wrong_hidden_word".into()];
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        !app.editor()
            .unwrap()
            .rope
            .to_string()
            .contains("wrong_hidden_word")
    );
    assert!(!app.lsp_completion_active && !app.vtcomplete_active);
}

#[test]
fn restarting_language_servers_invalidates_cached_signature_presentation() {
    let mut app = fixture(0);
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    for label in ["before_restart(value)", "after_restart(value)"] {
        if label.starts_with("after") {
            app.refresh_language_servers(true);
        }
        app.lsp_signature_active = true;
        app.lsp_manager.signature_help = Some(crate::lsp::LspSignatureHelp {
            signatures: vec![crate::lsp::LspSignatureInfo {
                label: label.into(),
                parameters: vec![],
                documentation: None,
            }],
            active_signature: 0,
            active_parameter: 0,
        });
        terminal
            .draw(|frame| crate::ui_dialogs::draw_lsp_signature_help(frame, &app))
            .unwrap();
        assert!(text(terminal.backend().buffer()).contains(label));
    }
}

#[test]
#[ignore = "manual optimized presentation benchmark"]
fn benchmark_completion_cached_redraws() {
    use std::{hint::black_box, time::Instant};
    let mut app = fixture(10_000);
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    let mut timings = [std::time::Duration::ZERO; 2];
    for (uncached, elapsed) in timings.iter_mut().enumerate() {
        let start = Instant::now();
        for _ in 0..500 {
            if uncached == 1 {
                app.completion_ui.doc_key = None;
            }
            terminal
                .draw(|frame| draw(frame, black_box(&mut app)))
                .unwrap();
        }
        *elapsed = start.elapsed();
    }
    eprintln!(
        "Completion with 10,000 rows: cached {:.3} ms/frame; reparse {:.3} ms/frame",
        timings[0].as_secs_f64() * 2.0,
        timings[1].as_secs_f64() * 2.0
    );
}
