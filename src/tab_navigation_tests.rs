use super::handle_keyboard_event;
use crate::{
    app::{ActivePane, App, EditorGroup, Tab, TabKind},
    editor::Editor,
};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

fn fixture() -> App {
    let mut app = App::test_new();
    app.tabs = ["first", "middle", "last"].into_iter()
        .map(|text| Tab::new_file(Editor::test_with_text(text))).collect();
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.tab_mru_order = vec![0, 2, 1];
    app
}

#[test]
fn tab_navigation_follows_strip_order_and_wraps_for_all_key_encodings() {
    let ctrl = KeyModifiers::CONTROL;
    let shift = KeyModifiers::SHIFT;
    for (code, modifiers, expected) in [
        (KeyCode::Tab, ctrl, [1, 2, 0]),
        (KeyCode::PageDown, ctrl, [1, 2, 0]),
        (KeyCode::Tab, ctrl | shift, [2, 1, 0]),
        (KeyCode::BackTab, ctrl | shift, [2, 1, 0]),
        (KeyCode::BackTab, ctrl, [2, 1, 0]),
        (KeyCode::PageUp, ctrl, [2, 1, 0]),
    ] {
        let mut app = fixture();
        for (index, tab) in expected.into_iter().enumerate() {
            let kind = if index == 0 { KeyEventKind::Press } else { KeyEventKind::Repeat };
            handle_keyboard_event(&mut app, KeyEvent::new_with_kind(code, modifiers, kind));
            assert_eq!(app.active_tab, tab, "{code:?} {modifiers:?}");
            assert_eq!(app.active_pane, ActivePane::Editor);
            assert!(!app.quick_open_active && !app.palette.active);
        }
        handle_keyboard_event(&mut app,
            KeyEvent::new_with_kind(code, modifiers, KeyEventKind::Release));
        assert_eq!(app.active_tab, 0, "key release must not switch again");
    }
}

#[test]
fn tab_navigation_activates_immediately_and_next_input_edits_the_new_tab() {
    let mut app = fixture();
    handle_keyboard_event(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL));
    assert_eq!(app.active_tab, 1);
    handle_keyboard_event(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.tabs[1].editor.rope.to_string(), "\nmiddle");
    assert_eq!(app.tabs[0].editor.rope.to_string(), "first");
    assert_eq!(app.tabs[2].editor.rope.to_string(), "last");
}

#[test]
fn tab_navigation_handles_empty_and_single_tab_strips() {
    for count in [0, 1] {
        let mut app = fixture();
        app.tabs.truncate(count);
        for (code, modifiers) in [
            (KeyCode::Tab, KeyModifiers::CONTROL),
            (KeyCode::Tab, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
            (KeyCode::BackTab, KeyModifiers::CONTROL),
        ] {
            handle_keyboard_event(&mut app, KeyEvent::new(code, modifiers));
            assert_eq!(app.active_tab, 0);
            assert_eq!(app.tabs.len(), count);
            if count == 1 {
                assert_eq!(app.tabs[0].editor.rope.to_string(), "first");
            }
        }
    }
}

#[test]
fn tab_navigation_includes_terminal_and_settings_tabs() {
    let mut app = fixture();
    app.tabs[1].kind = TabKind::Terminal;
    app.tabs[2].kind = TabKind::Settings;
    for expected in [1, 2, 0] {
        handle_keyboard_event(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL));
        assert_eq!(app.active_tab, expected);
        assert!(!app.terminal_panel_focused);
    }
}

#[test]
fn tab_navigation_stays_in_the_focused_split_and_uses_reordered_tabs() {
    let mut app = fixture();
    let mut other = EditorGroup::new();
    other.tabs = ["other left", "other right"].into_iter()
        .map(|text| Tab::new_file(Editor::test_with_text(text))).collect();
    other.active_tab = 1;
    app.groups = vec![EditorGroup::new(), other];
    app.switch_group(1);
    handle_keyboard_event(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL));
    assert_eq!(app.active_group, 1);
    assert_eq!(app.active_tab, 0);
    assert_eq!(app.editor().unwrap().rope.to_string(), "other left");
    app.switch_group(0);
    assert_eq!(app.active_tab, 0);
    app.tabs.swap(1, 2);
    handle_keyboard_event(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL));
    assert_eq!(app.editor().unwrap().rope.to_string(), "last");
    assert_eq!(app.groups[1].active_tab, 0);
}

#[test]
fn tab_navigation_accepts_custom_backtab_bindings_in_both_encodings() {
    let mut app = fixture();
    let previous = app.keybindings.iter_mut().find(|binding| binding.id == "prev_tab").unwrap();
    previous.code = KeyCode::BackTab;
    previous.modifiers = KeyModifiers::CONTROL;
    app.keybindings = App::keybindings_for_settings(&app.current_settings());
    handle_keyboard_event(&mut app,
        KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL | KeyModifiers::SHIFT));
    assert_eq!(app.active_tab, 2);
    handle_keyboard_event(&mut app,
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::CONTROL));
    assert_eq!(app.active_tab, 1);
}

#[test]
fn tab_navigation_bindings_can_be_reassigned_and_restored_independently() {
    let mut app = fixture();
    let page = app.keybindings.iter_mut().find(|binding| binding.id == "next_tab_page").unwrap();
    page.code = KeyCode::F(6);
    page.modifiers = KeyModifiers::NONE;
    app.keybindings = App::keybindings_for_settings(&app.current_settings());
    handle_keyboard_event(&mut app, KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE));
    assert_eq!(app.active_tab, 1);
    handle_keyboard_event(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL));
    assert_eq!(app.active_tab, 2);
}
