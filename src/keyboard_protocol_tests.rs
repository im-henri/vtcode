//! Shared workbench assertions, fed by the real Unix and Windows input readers.

use crate::{
    app::{ActivePane, App, Tab},
    editor::Editor,
    input_keyboard::handle_keyboard_event,
};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

pub(crate) fn fragmented_find_sequences() -> [(&'static str, KeyEvent); 4] {
    [
        (
            "\x1b[13;2u",
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
        ),
        (
            "\x1b[27;2;13~",
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
        ),
        (
            "\x1b[13;2:2u",
            KeyEvent::new_with_kind(KeyCode::Enter, KeyModifiers::SHIFT, KeyEventKind::Repeat),
        ),
        (
            "\x1b[13;2:3u",
            KeyEvent::new_with_kind(KeyCode::Enter, KeyModifiers::SHIFT, KeyEventKind::Release),
        ),
    ]
}

pub(crate) fn fragmented_shortcut_sequences() -> Vec<(&'static str, KeyEvent)> {
    let mut cases = fragmented_find_sequences().to_vec();
    let ctrl_shift = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
    cases.extend([
        ("\x1b[112;6u", KeyEvent::new(KeyCode::Char('p'), ctrl_shift)),
        (
            "\x1b[112:80;6u",
            KeyEvent::new(KeyCode::Char('P'), ctrl_shift),
        ),
        (
            "\x1b[27;6;80~",
            KeyEvent::new(KeyCode::Char('P'), ctrl_shift),
        ),
        (
            "\x1b[112;6:2u",
            KeyEvent::new_with_kind(KeyCode::Char('p'), ctrl_shift, KeyEventKind::Repeat),
        ),
        (
            "\x1b[112;6:3u",
            KeyEvent::new_with_kind(KeyCode::Char('p'), ctrl_shift, KeyEventKind::Release),
        ),
        (
            "\x1b[112;5u",
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        ),
        ("\x1bOP", KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE)),
    ]);
    cases
}

pub(crate) fn vt_cases() -> Vec<(&'static str, KeyEvent)> {
    let mut cases = vec![
        (
            "\x08",
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL),
        ),
        (
            "\x7f",
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        ),
        (
            "\x1b[127;5u",
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL),
        ),
        (
            "\x1b[127u",
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        ),
        (
            "\x1b[104;5u",
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL),
        ),
        (
            "\x1b[127;5:2u",
            KeyEvent::new_with_kind(
                KeyCode::Backspace,
                KeyModifiers::CONTROL,
                KeyEventKind::Repeat,
            ),
        ),
        (
            "\x1b[127;5:3u",
            KeyEvent::new_with_kind(
                KeyCode::Backspace,
                KeyModifiers::CONTROL,
                KeyEventKind::Release,
            ),
        ),
    ];
    let ctrl = KeyModifiers::CONTROL;
    let shift = KeyModifiers::SHIFT;
    for (sequence, code, modifiers) in [
        ("\x1b[27;5;127~", KeyCode::Backspace, ctrl),
        ("\x1b[27;5;8~", KeyCode::Backspace, ctrl),
        ("\x1b[27;5;104~", KeyCode::Char('h'), ctrl),
        ("\x1b[1;5D", KeyCode::Left, ctrl),
        ("\x1b[1;5C", KeyCode::Right, ctrl),
        ("\x1b[1;6D", KeyCode::Left, ctrl | shift),
        ("\x1b[1;6C", KeyCode::Right, ctrl | shift),
        ("\x1b[1;5A", KeyCode::Up, ctrl),
        ("\x1b[1;5B", KeyCode::Down, ctrl),
        ("\x1b[1;6A", KeyCode::Up, ctrl | shift),
        ("\x1b[1;6B", KeyCode::Down, ctrl | shift),
        ("\x1b[1;5H", KeyCode::Home, ctrl),
        ("\x1b[1;5F", KeyCode::End, ctrl),
        ("\x1b[1;6H", KeyCode::Home, ctrl | shift),
        ("\x1b[1;6F", KeyCode::End, ctrl | shift),
        ("\x1b[3;5~", KeyCode::Delete, ctrl),
        ("\x1b[9;5u", KeyCode::Tab, ctrl),
        ("\x1b[9;6u", KeyCode::BackTab, ctrl | shift),
        ("\x1b[1;5Z", KeyCode::BackTab, ctrl | shift),
        ("\x1b[1;6Z", KeyCode::BackTab, ctrl | shift),
        ("\x1b[27;5;9~", KeyCode::Tab, ctrl),
        ("\x1b[27;6;9~", KeyCode::BackTab, ctrl | shift),
        ("\r", KeyCode::Enter, KeyModifiers::NONE),
        ("\x1b[13;2u", KeyCode::Enter, shift),
        ("\x1b[27;2;13~", KeyCode::Enter, shift),
        // Older terminals collapse Ctrl+Shift+P to this same Ctrl+P byte.
        // The app must preserve file search; it cannot recover a lost Shift.
        ("\x10", KeyCode::Char('p'), ctrl),
        ("\x1b[112;5u", KeyCode::Char('p'), ctrl),
        ("\x1bOP", KeyCode::F(1), KeyModifiers::NONE),
        ("\x1b[112;6u", KeyCode::Char('p'), ctrl | shift),
        ("\x1b[112:80;6u", KeyCode::Char('P'), ctrl | shift),
        ("\x1b[27;6;80~", KeyCode::Char('P'), ctrl | shift),
        ("\x1b[122;6u", KeyCode::Char('z'), ctrl | shift),
        ("\x1b[122:90;6u", KeyCode::Char('Z'), ctrl | shift),
        ("\x1b[27;6;90~", KeyCode::Char('Z'), ctrl | shift),
        ("\x1c", KeyCode::Char('\\'), ctrl),
        ("\x1d", KeyCode::Char(']'), ctrl),
        ("\x1e", KeyCode::Char('^'), ctrl),
        ("\x1f", KeyCode::Char('_'), ctrl),
        ("\x1b[47;5u", KeyCode::Char('/'), ctrl),
        ("\x1b[27;5;47~", KeyCode::Char('/'), ctrl),
        ("\x1b[105;6u", KeyCode::Char('i'), ctrl | shift),
        ("\x1b[27;6;73~", KeyCode::Char('I'), ctrl | shift),
        ("\x1b[27;3;233~", KeyCode::Char('é'), KeyModifiers::ALT),
    ] {
        cases.push((sequence, KeyEvent::new(code, modifiers)));
    }
    cases
}

/// Independent expectations from Kitty's key table and modifier specification:
/// https://sw.kovidgoyal.net/kitty/keyboard-protocol/
/// Exercise these through real OS readers and SSH, not a second test decoder.
pub(crate) fn extended_key_cases() -> Vec<(String, KeyEvent)> {
    let mut cases: Vec<_> = vt_cases().into_iter().map(|(s, k)| (s.into(), k)).collect();
    for mask in 0..8 {
        let mut modifiers = KeyModifiers::NONE;
        modifiers.set(KeyModifiers::SHIFT, mask & 1 != 0);
        modifiers.set(KeyModifiers::ALT, mask & 2 != 0);
        modifiers.set(KeyModifiers::CONTROL, mask & 4 != 0);
        let mut codes: Vec<_> = "abcdefghijklmnopqrstuvwxyz0123456789 `-=[]\\;',./é界🦀\u{301}"
            .chars()
            .map(|c| (c as u32, KeyCode::Char(c)))
            .collect();
        codes.extend([
            (27, KeyCode::Esc),
            (13, KeyCode::Enter),
            (127, KeyCode::Backspace),
            (57358, KeyCode::CapsLock),
            (57376, KeyCode::F(13)),
        ]);
        for (number, code) in codes {
            for (event_type, kind) in [
                (1, KeyEventKind::Press),
                (2, KeyEventKind::Repeat),
                (3, KeyEventKind::Release),
            ] {
                cases.push((
                    format!("\x1b[{number};{}:{event_type}u", mask + 1),
                    KeyEvent::new_with_kind(code, modifiers, kind),
                ));
            }
        }
        for (suffix, code) in [
            ("A", KeyCode::Up),
            ("B", KeyCode::Down),
            ("C", KeyCode::Right),
            ("D", KeyCode::Left),
            ("H", KeyCode::Home),
            ("F", KeyCode::End),
            ("P", KeyCode::F(1)),
            ("Q", KeyCode::F(2)),
            ("S", KeyCode::F(4)),
        ] {
            cases.push((
                format!("\x1b[1;{}{suffix}", mask + 1),
                KeyEvent::new(code, modifiers),
            ));
        }
        for (number, code) in [
            (2, KeyCode::Insert),
            (3, KeyCode::Delete),
            (5, KeyCode::PageUp),
            (6, KeyCode::PageDown),
            (13, KeyCode::F(3)),
            (15, KeyCode::F(5)),
            (24, KeyCode::F(12)),
        ] {
            cases.push((
                format!("\x1b[{number};{}~", mask + 1),
                KeyEvent::new(code, modifiers),
            ));
        }
        cases.push((
            format!("\x1b[9;{}u", mask + 1),
            KeyEvent::new(
                if mask & 1 != 0 {
                    KeyCode::BackTab
                } else {
                    KeyCode::Tab
                },
                modifiers,
            ),
        ));
    }
    cases
}

fn editor_app() -> App {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text("keep café");
    editor.record_saved_snapshot(editor.rope.clone());
    editor.set_cursor(0, 9);
    app.tabs.push(Tab::new_file(editor));
    app.active_pane = ActivePane::Editor;
    app
}

#[test]
fn extended_release_reports_never_trigger_editor_or_search_commands() {
    let mut app = editor_app();
    for search in [false,true] {
        if search { app.execute_palette_command("find"); }
        let rope = app.editor().unwrap().rope.clone();
        let cursor = (app.editor().unwrap().cursor_line,app.editor().unwrap().cursor_col);
        for (_,key) in extended_key_cases().into_iter().filter(|(_,key)| key.kind == KeyEventKind::Release) {
            handle_keyboard_event(&mut app,key);
            assert_eq!(app.editor().unwrap().rope,rope,"{key:?}");
            assert_eq!((app.editor().unwrap().cursor_line,app.editor().unwrap().cursor_col),cursor,"{key:?}");
            assert_eq!(app.search_active,search,"{key:?}");
            assert!(!app.palette.active && !app.quick_open_active && !app.replace_mode,"{key:?}");
            assert!(!app.editor().unwrap().can_undo(),"{key:?}");
        }
    }
}

pub(crate) fn check_key(key: KeyEvent) {
    if key.code == KeyCode::Char('h') && key.modifiers == KeyModifiers::CONTROL {
        let mut app = editor_app();
        handle_keyboard_event(&mut app, key);
        assert!(app.search_active && app.replace_mode);
        assert_eq!(app.editor().unwrap().rope.to_string(), "keep café");
        return;
    }
    if key.code != KeyCode::Backspace {
        check_other_shortcut(key);
        return;
    }
    let mut app = editor_app();
    handle_keyboard_event(&mut app, key);
    assert!(
        !app.search_active && !app.replace_mode,
        "{key:?} opened Replace"
    );
    let expected = if key.kind == KeyEventKind::Release {
        "keep café"
    } else if key.modifiers.contains(KeyModifiers::CONTROL) {
        "keep "
    } else {
        "keep caf"
    };
    assert_eq!(app.editor().unwrap().rope.to_string(), expected, "{key:?}");
    if key.kind == KeyEventKind::Release {
        assert!(!app.editor().unwrap().can_undo());
        return;
    }
    handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
    );
    assert_eq!(app.editor().unwrap().rope.to_string(), "keep café");
    assert!(!app.editor().unwrap().modified);
    app.execute_palette_command("redo");
    assert_eq!(app.editor().unwrap().rope.to_string(), expected);

    // Selection deletion must take priority over deleting another word.
    let mut app = editor_app();
    app.editor_mut().unwrap().selection_anchor = Some((0, 7));
    handle_keyboard_event(&mut app, key);
    assert_eq!(app.editor().unwrap().rope.to_string(), "keep ca");
    assert!(!app.search_active);

    let mut app = editor_app();
    app.editor_mut().unwrap().read_only = true;
    handle_keyboard_event(&mut app, key);
    assert_eq!(app.editor().unwrap().rope.to_string(), "keep café");
    assert!(!app.search_active);

    let mut app = editor_app();
    app.execute_palette_command("find");
    app.search_query = "keep café".into();
    app.search_cursor = app.search_query.len();
    app.search_selection_anchor = None;
    handle_keyboard_event(&mut app, key);
    assert_eq!(app.search_query, expected);
    assert!(!app.replace_mode);
    assert_eq!(app.editor().unwrap().rope.to_string(), "keep café");
}

fn check_other_shortcut(key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let mut app = editor_app();
    match key.code {
        KeyCode::Left | KeyCode::Right if ctrl => {
            let start = if key.code == KeyCode::Left { 9 } else { 0 };
            app.editor_mut().unwrap().set_cursor(0, start);
            handle_keyboard_event(&mut app, key);
            let editor = app.editor().unwrap();
            assert_eq!(
                editor.cursor_col,
                if key.code == KeyCode::Left { 5 } else { 4 },
                "{key:?}"
            );
            assert_eq!(
                editor.selection_anchor,
                shift.then_some((0, start)),
                "{key:?}"
            );
            assert!(!editor.modified);
        }
        KeyCode::Home | KeyCode::End if ctrl => {
            app.tabs[0] = Tab::new_file(Editor::test_with_text("one\ntwo\nthree"));
            app.editor_mut().unwrap().set_cursor(1, 1);
            handle_keyboard_event(&mut app, key);
            let editor = app.editor().unwrap();
            let expected = if key.code == KeyCode::Home {
                (0, 0)
            } else {
                (2, 5)
            };
            assert_eq!((editor.cursor_line, editor.cursor_col), expected);
            assert_eq!(editor.selection_anchor, shift.then_some((1, 1)));
        }
        KeyCode::Up | KeyCode::Down if ctrl => {
            app.tabs[0] = Tab::new_file(Editor::test_with_text("one\n\nsecond\n\nthird"));
            let editor = app.editor_mut().unwrap();
            editor.set_cursor(2, 2);
            handle_keyboard_event(&mut app, key);
            let editor = app.editor().unwrap();
            assert_eq!(
                (editor.cursor_line, editor.cursor_col),
                (if key.code == KeyCode::Up { 0 } else { 4 }, 0),
                "{key:?}"
            );
            assert_eq!(editor.selection_anchor, shift.then_some((2, 2)));
        }
        KeyCode::Delete if key.modifiers == KeyModifiers::CONTROL => {
            app.editor_mut().unwrap().set_cursor(0, 5);
            handle_keyboard_event(&mut app, key);
            assert_eq!(app.editor().unwrap().rope.to_string(), "keep ");
            handle_keyboard_event(
                &mut app,
                KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
            );
            assert_eq!(app.editor().unwrap().rope.to_string(), "keep café");
            assert!(!app.editor().unwrap().modified);
        }
        KeyCode::Tab | KeyCode::BackTab if ctrl => {
            app.tabs.push(Tab::new_file(Editor::test_with_text("two")));
            app.tabs
                .push(Tab::new_file(Editor::test_with_text("three")));
            app.tab_mru_order = vec![0, 1, 2];
            handle_keyboard_event(&mut app, key);
            assert!(app.tab_switcher_active, "{key:?}");
            assert_eq!(
                app.tab_switcher_selected,
                if shift { 2 } else { 1 },
                "{key:?}"
            );
            assert_eq!(app.editor().unwrap().rope.to_string(), "keep café");
            handle_keyboard_event(&mut app, key);
            let selected = if shift { 1 } else { 2 };
            assert_eq!(app.tab_switcher_selected, selected);
            handle_keyboard_event(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            assert!(!app.tab_switcher_active);
            assert_eq!(app.active_tab, selected);
        }
        KeyCode::Enter if matches!(key.modifiers, KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            check_find_navigation(key);
        }
        KeyCode::Char('p' | 'P') if ctrl && shift => {
            handle_keyboard_event(&mut app, key);
            assert_eq!(
                app.palette.active,
                key.kind != KeyEventKind::Release,
                "{key:?}"
            );
            assert!(!app.quick_open_active, "{key:?}");
            assert_eq!(app.editor().unwrap().rope, "keep café");
        }
        KeyCode::Char('p') if key.modifiers == KeyModifiers::CONTROL => {
            handle_keyboard_event(&mut app, key);
            assert!(app.quick_open_active && !app.palette.active, "{key:?}");
            assert_eq!(app.editor().unwrap().rope, "keep café");
        }
        KeyCode::F(1) if key.modifiers.is_empty() => {
            handle_keyboard_event(&mut app, key);
            assert!(app.palette.active && !app.quick_open_active, "{key:?}");
            assert_eq!(app.editor().unwrap().rope, "keep café");
        }
        KeyCode::Char('z' | 'Z') if ctrl && shift => {
            handle_keyboard_event(
                &mut app,
                KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            );
            handle_keyboard_event(
                &mut app,
                KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
            );
            assert_eq!(app.editor().unwrap().rope.to_string(), "keep café");
            handle_keyboard_event(&mut app, key);
            assert_eq!(
                app.editor().unwrap().rope.to_string(),
                "keep caféx",
                "{key:?}"
            );
        }
        KeyCode::Char('\\') if key.modifiers == KeyModifiers::CONTROL => {
            app.tabs[0] = Tab::new_file(Editor::test_with_text("(word)"));
            handle_keyboard_event(&mut app, key);
            assert_eq!(
                app.editor().unwrap().cursor_col,
                5,
                "Ctrl+\\ must find the matching bracket"
            );
        }
        KeyCode::Char(']') if key.modifiers == KeyModifiers::CONTROL => {
            handle_keyboard_event(&mut app, key);
            assert_eq!(app.editor().unwrap().rope.to_string(), "    keep café");
        }
        KeyCode::Char('/') if key.modifiers == KeyModifiers::CONTROL => {
            app.editor_mut().unwrap().language_override = Some("Rust".into());
            handle_keyboard_event(&mut app, key);
            assert_eq!(app.editor().unwrap().rope.to_string(), "// keep café");
        }
        _ => {}
    }
}

pub(crate) fn check_find_navigation(key: KeyEvent) {
    for read_only in [false, true] {
        for query in ["foo", "missing"] {
            let mut app = editor_app();
            app.tabs[0] = Tab::new_file(Editor::test_with_text("foo foo foo"));
            app.editor_mut().unwrap().read_only = read_only;
            handle_keyboard_event(
                &mut app,
                KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
            );
            app.search_query = query.into();
            app.update_search();
            assert_eq!(app.search_current, 0);
            for index in if key.modifiers.contains(KeyModifiers::SHIFT) {
                [2, 1, 0, 2]
            } else {
                [1, 2, 0, 1]
            } {
                let expected = if query == "missing" || key.kind == KeyEventKind::Release {
                    0
                } else {
                    index
                };
                assert!(!handle_keyboard_event(&mut app, key));
                assert_eq!(app.search_current, expected, "{key:?}, query={query}");
                assert!(
                    app.search_active && app.search_input_focused,
                    "{key:?} closed Find"
                );
                assert_eq!(app.search_query, query);
                assert_eq!(app.editor().unwrap().rope.to_string(), "foo foo foo");
                assert!(!app.editor().unwrap().can_undo());
                if query == "foo" {
                    assert_eq!(
                        app.editor().unwrap().selection_range(),
                        Some((0, expected * 4, 0, expected * 4 + 3))
                    );
                }
            }
        }
    }
}

#[test]
fn decoded_shortcuts_reach_expected_actions() {
    for (_, key) in vt_cases() {
        check_key(key);
    }
}

#[test]
fn find_shift_enter_repeat_and_release_keep_search_open() {
    for (_, key) in fragmented_find_sequences() {
        check_find_navigation(key);
    }
}

#[test]
fn palette_shortcuts_keep_file_search_and_key_releases_distinct() {
    for (_, key) in fragmented_shortcut_sequences() {
        check_key(key);
    }
}
