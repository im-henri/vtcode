use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};

fn app() -> App {
    let mut app = super::super::tests::seeded();
    app.chat.agent_order = vec![app.chat.active_agent];
    let id = app.chat.selected.clone().unwrap();
    *app.chat.session_mut().unwrap() = Session::new(id);
    app.chat.rollback.enabled = true;
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    app
}

fn paint(app: &mut App, terminal: &mut Terminal<TestBackend>) {
    app.text_input_regions.clear();
    terminal
        .draw(|f| super::super::draw(f, app, f.area()))
        .unwrap();
}

fn click(app: &mut App, action: ChatAction) {
    let hit = app
        .chat
        .hits
        .iter()
        .find(|hit| hit.action == action)
        .unwrap()
        .area;
    app.handle_chat_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: hit.x,
        row: hit.y,
        modifiers: KeyModifiers::NONE,
    });
}

fn capture(terminal: &Terminal<TestBackend>, name: &str) {
    super::super::presentation_tests::capture(terminal, name);
}

fn pending_edit() -> vtcode::acp::rollback::Edit {
    serde_json::from_value(serde_json::json!({
        "id":1,"checkpoint":1,"path":"src/example.rs","conflict":null,
        "before":null,"after":null,"whole_file":false,
        "chunks":[{"hunk":{"old":{"start":0,"end":0},"new":{"start":0,"end":10}},
            "decision":"Pending","line":1,"preview":"let x = 1;"}]
    }))
    .unwrap()
}

#[test]
fn header_keeps_named_yolo_and_display_stable_as_pending_changes_arrive() {
    for theme in ["dark", "light", "high-contrast"] {
        for width in [24, 36, 58, 110] {
            let mut app = app();
            app.set_theme_by_key(theme);
            let mut terminal = Terminal::new(TestBackend::new(width, 32)).unwrap();
            paint(&mut app, &mut terminal);
            let yolo = app
                .chat
                .hits
                .iter()
                .find(|hit| hit.action == ChatAction::Yolo)
                .unwrap()
                .area;
            let display = app
                .chat
                .hits
                .iter()
                .find(|hit| hit.action == ChatAction::Picker(Picker::Display))
                .unwrap()
                .area;
            assert!(!app.chat.hits.iter().any(|hit| matches!(
                hit.action,
                ChatAction::OutputDetail
                    | ChatAction::ThinkingDetail
                    | ChatAction::Picker(Picker::Checkpoints | Picker::Changes)
            )));
            capture(&terminal, &format!("polished-empty-{theme}-{width}"));
            app.chat
                .session_mut()
                .unwrap()
                .rollback
                .edits
                .push(pending_edit());
            paint(&mut app, &mut terminal);
            for (action, previous) in [
                (ChatAction::Yolo, yolo),
                (ChatAction::Picker(Picker::Display), display),
            ] {
                assert_eq!(
                    app.chat
                        .hits
                        .iter()
                        .find(|hit| hit.action == action)
                        .unwrap()
                        .area,
                    previous
                );
            }
            let bounds = Rect::new(1, 0, width - 1, 32);
            let controls = layout(&app, bounds).controls;
            assert!(
                controls
                    .windows(2)
                    .all(|pair| pair[0].0.right() <= pair[1].0.x)
            );
            assert!(
                controls
                    .iter()
                    .all(|(rect, _, _)| rect.intersection(bounds) == *rect)
            );
            assert!(
                controls
                    .iter()
                    .find(|(_, _, action)| *action == ChatAction::Yolo)
                    .unwrap()
                    .1
                    .contains("YOLO [ ]")
            );
            if width >= 36 {
                assert!(controls.iter().any(|(_, label, action)| *action
                    == ChatAction::Picker(Picker::Changes)
                    && label.contains("Changes (1)")));
            }
            capture(&terminal, &format!("polished-pending-{theme}-{width}"));
        }
    }
}

#[test]
fn narrow_non_git_chat_keeps_yolo_display_and_the_composer_available() {
    let mut app = app();
    app.chat.rollback.enabled = false;
    app.chat.rollback.initialized = true;
    app.chat.input = "Work without Git".into();
    app.chat.cursor = app.chat.input.len();
    let mut terminal = Terminal::new(TestBackend::new(24, 32)).unwrap();
    paint(&mut app, &mut terminal);
    for action in [ChatAction::Yolo, ChatAction::Picker(Picker::Display)] {
        assert!(app.chat.hits.iter().any(|hit| hit.action == action));
    }
    assert!(app.chat.session().is_some());
    assert_eq!(app.text_input_regions.len(), 1);
    assert!(app.handle_chat_key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE)));
    assert_eq!(app.chat.input, "Work without Git!");
    paint(&mut app, &mut terminal);
    capture(&terminal, "polished-non-git-narrow");
}

#[test]
fn display_menu_changes_both_defaults_without_closing_or_touching_chat_work() {
    let mut app = app();
    app.chat
        .session_mut()
        .unwrap()
        .begin_prompt("Keep working".into());
    app.chat.input = "Keep this draft".into();
    let entries = app.chat.session().unwrap().entries.len();
    let mut terminal = Terminal::new(TestBackend::new(58, 34)).unwrap();
    paint(&mut app, &mut terminal);
    click(&mut app, ChatAction::Picker(Picker::Display));
    paint(&mut app, &mut terminal);
    assert_eq!(app.chat.choices.len(), 5);
    for action in [
        ChatAction::SetOutputDetail(OutputDetail::Minimal),
        ChatAction::SetThinkingDetail(true),
    ] {
        let index = app
            .chat
            .choices
            .iter()
            .position(|choice| choice.action == action)
            .unwrap();
        click(&mut app, ChatAction::Pick(index));
        paint(&mut app, &mut terminal);
        assert_eq!(app.chat.picker, Some(Picker::Display));
        assert_eq!(app.chat.picker_selected, index);
        assert!(app.chat.choices[index].label.starts_with("✓ "));
        assert_eq!(app.chat.input, "Keep this draft");
        assert!(app.chat.busy());
        assert_eq!(app.chat.session().unwrap().entries.len(), entries);
    }
    assert_eq!(app.chat.output_detail, OutputDetail::Minimal);
    assert!(app.chat.thinking_expanded);
    capture(&terminal, "polished-display-menu");
    click(&mut app, ChatAction::Picker(Picker::Display));
    assert!(app.chat.picker.is_none());
    paint(&mut app, &mut terminal);
    click(&mut app, ChatAction::Picker(Picker::Display));
    app.handle_chat_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.chat.picker.is_none());
}

#[test]
fn checkpoint_commands_and_message_restore_remain_available_without_toolbar_clutter() {
    let mut app = app();
    app.chat
        .session_mut()
        .unwrap()
        .begin_prompt("Original prompt".into());
    app.chat.session_mut().unwrap().set_checkpoint(0, 17);
    app.chat
        .session_mut()
        .unwrap()
        .finish(Some(p::StopReason::EndTurn));
    let mut terminal = Terminal::new(TestBackend::new(58, 32)).unwrap();
    paint(&mut app, &mut terminal);
    assert!(
        !app.chat
            .hits
            .iter()
            .any(|hit| hit.action == ChatAction::Picker(Picker::Checkpoints))
    );
    click(&mut app, ChatAction::Picker(Picker::Commands));
    paint(&mut app, &mut terminal);
    let checkpoint = app
        .chat
        .choices
        .iter()
        .position(|choice| choice.action == ChatAction::Picker(Picker::Checkpoints))
        .unwrap();
    click(&mut app, ChatAction::Pick(checkpoint));
    assert_eq!(app.chat.picker, Some(Picker::Checkpoints));
    app.chat_action(ChatAction::DismissPicker);
    paint(&mut app, &mut terminal);
    click(&mut app, ChatAction::Picker(Picker::Message(0, 0)));
    assert!(
        app.chat
            .choices
            .iter()
            .any(|choice| choice.action == ChatAction::Picker(Picker::Restore(17)))
    );
}
