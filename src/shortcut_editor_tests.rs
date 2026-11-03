use super::*;
use crate::{
    app::Tab,
    editor::Editor,
    settings::{KeybindingSetting, Settings, SettingsScope},
};
use crossterm::event::{KeyEventKind, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn fixture() -> App {
    let mut app = App::test_new();
    app.settings_persistence_enabled = false;
    app.settings_global = Settings::default();
    app.settings_workspace = Settings::default();
    app.settings_scope = SettingsScope::Global;
    app.apply_effective_settings();
    app.tabs = vec![Tab::new_file(Editor::test_with_text(
        "fn example() {\n    one();\n    two();\n}\ntail();\n",
    ))];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app
}

fn index(app: &App, id: &str) -> usize {
    app.keybindings
        .iter()
        .position(|binding| binding.id == id)
        .unwrap()
}
fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, modifiers));
}
fn edit(app: &mut App, id: &str) -> usize {
    let index = index(app, id);
    let scope = app.settings_scope;
    app.open_settings_tab();
    app.settings_scope = scope;
    app.settings_category_selected = 5;
    app.settings_focus_categories = false;
    let id = format!("kb_{index}");
    app.settings_selected = app
        .settings_items()
        .iter()
        .position(|(_, _, item)| *item == id)
        .unwrap();
    app.begin_shortcut_edit(index, true);
    index
}
fn action(app: &mut App, action: EditAction) {
    let edit = app.shortcut_editor.take().unwrap();
    app.shortcut_editor_action(edit, action);
}
fn render(app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui_settings::draw_settings_editor(frame, app, frame.area());
            draw_editor(frame, app);
        })
        .unwrap();
    terminal.backend().buffer().clone()
}
fn text(buffer: &Buffer) -> String {
    buffer.content.iter().map(|cell| cell.symbol()).collect()
}
fn custom(command: &str, key: &str, index: Option<usize>) -> KeybindingSetting {
    KeybindingSetting {
        command: Some(command.into()),
        key: Some(key.into()),
        index,
        ..Default::default()
    }
}

#[test]
fn all_advertised_shortcuts_have_editable_slots_and_settings_have_no_readonly_rows() {
    let mut app = fixture();
    app.rebuild_keybinding_settings_cache();
    for &(command, _, shortcut) in defaults::BUILTINS {
        assert!(parse_shortcut(shortcut).is_some(), "{command}: {shortcut}");
    }
    for command in App::build_command_list_with_themes(&[], &[]) {
        if command.shortcut.is_empty() {
            continue;
        }
        let id = crate::app_menu::keybinding_id_for_command(&command.id);
        assert!(
            app.keybindings.iter().any(|binding| binding.id == id),
            "{}: {}",
            command.name,
            command.shortcut
        );
    }
    assert!(
        app.keybinding_settings_cache
            .iter()
            .all(|(_, value, id)| value != "Read-only" && !id.starts_with("shortcut_ro_"))
    );
    assert_eq!(
        app.keybinding_settings_cache
            .iter()
            .filter(|(_, _, id)| id.starts_with("kb_"))
            .count(),
        app.keybindings.len()
    );
}

#[test]
fn recording_previews_without_applying_and_save_replaces_a_formerly_fixed_shortcut() {
    let mut app = fixture();
    let target = edit(&mut app, "toggle_zen");
    key(&mut app, KeyCode::F(7), KeyModifiers::NONE);
    assert_eq!(app.keybindings[target].code, KeyCode::F(11));
    assert!(app.settings_kb_editing);
    let screen = text(&render(&mut app, 110, 32));
    assert!(screen.contains("F7"));
    assert!(screen.contains("Current: F11"));
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(!app.settings_kb_editing);
    assert_eq!(app.keybindings[target].code, KeyCode::F(7));
    app.active_tab = 0;
    let before = app.zen_mode;
    key(&mut app, KeyCode::F(11), KeyModifiers::NONE);
    assert_eq!(
        app.zen_mode, before,
        "old hardcoded shortcut must stop firing"
    );
    key(&mut app, KeyCode::F(7), KeyModifiers::NONE);
    assert_ne!(app.zen_mode, before);
}

#[test]
fn escape_and_owner_changes_cancel_without_changing_bindings_or_preferences() {
    for switch_owner in [false, true] {
        let mut app = fixture();
        app.tabs[0].editor.tab_size = 2;
        let target = edit(&mut app, "save_as");
        let original = app.keybindings[target].display();
        key(&mut app, KeyCode::F(24), KeyModifiers::ALT);
        if switch_owner {
            app.active_tab = 0;
        }
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.shortcut_editor.is_none());
        assert_eq!(app.keybindings[target].display(), original);
        assert_eq!(app.tabs[0].editor.tab_size, 2);
        assert!(app.settings_global.keybindings.is_none());
    }
}

#[test]
fn assignment_dialog_disappears_when_its_owning_tab_loses_focus() {
    let mut app = fixture();
    let target = edit(&mut app, "toggle_zen");
    key(&mut app, KeyCode::F(24), KeyModifiers::NONE);
    app.active_tab = 0;
    render(&mut app, 110, 32);
    assert!(app.shortcut_editor.is_none());
    assert!(!app.settings_kb_editing);
    assert_eq!(app.keybindings[target].display(), "F11");
    assert!(app.settings_global.keybindings.is_none());
}

#[test]
fn conflicts_are_named_and_only_removed_after_confirmation() {
    let mut app = fixture();
    let target = edit(&mut app, "toggle_zen");
    let find = index(&app, "find_next");
    key(&mut app, KeyCode::F(3), KeyModifiers::NONE);
    assert_eq!(app.keybindings[find].code, KeyCode::F(3));
    let screen = text(&render(&mut app, 110, 32));
    assert!(screen.contains("Already assigned to:"));
    assert!(screen.contains("Find Next"));
    assert!(screen.contains("[Replace]"));
    assert!(app.settings_global.keybindings.is_none());
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.keybindings[target].code, KeyCode::F(3));
    assert_eq!(app.keybindings[find].code, KeyCode::Null);
}

#[test]
fn reassigned_chords_persist_execute_and_disable_their_old_sequence() {
    let mut app = fixture();
    let target = edit(&mut app, "fold_all");
    key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
    let mut repeat = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL);
    repeat.kind = KeyEventKind::Repeat;
    crate::input_keyboard::handle_keyboard_event(&mut app, repeat);
    assert!(app.shortcut_editor.as_ref().unwrap().recording);
    key(&mut app, KeyCode::F(9), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.keybindings[target].display(), "Ctrl+J, Ctrl+F9");
    assert!(
        app.settings_global
            .keybindings
            .as_ref()
            .unwrap()
            .iter()
            .any(|entry| entry.command.as_deref() == Some("fold_all")
                && entry.key.as_deref() == Some("Ctrl+J, Ctrl+F9"))
    );
    let serialized = serde_json::to_string(&app.settings_global).unwrap();
    let reloaded = serde_json::from_str(&serialized).unwrap();
    assert_eq!(
        App::keybindings_for_settings(&reloaded)[target].display(),
        "Ctrl+J, Ctrl+F9"
    );
    app.active_tab = 0;
    let original = app.editor().unwrap().rope.clone();
    key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::F(9), KeyModifiers::CONTROL);
    assert!(!app.editor().unwrap().folded_ranges.is_empty());
    app.editor_mut().unwrap().unfold_all();
    key(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('0'), KeyModifiers::NONE);
    assert!(app.editor().unwrap().folded_ranges.is_empty());
    assert!(app.editor().unwrap().rope.is_instance(&original));
}

#[test]
fn typed_shortcuts_cover_special_keys_and_reject_invalid_sequences() {
    for shortcut in [
        "Ctrl+K S",
        "Ctrl+K, Ctrl+F",
        "Ctrl+PgUp",
        "Shift+Tab",
        "Ctrl++",
        "Ctrl+,",
        "Space",
        "Esc",
        "Enter",
        "Alt+Shift+F24",
        "Super+Insert",
        "Hyper+Meta+Space",
        "Alt+ß",
    ] {
        let (first, second) = parse_shortcut(shortcut).unwrap_or_else(|| panic!("{shortcut}"));
        let display = second.map_or_else(
            || display_stroke(first.0, first.1),
            |second| {
                format!(
                    "{}, {}",
                    display_stroke(first.0, first.1),
                    display_stroke(second.0, second.1)
                )
            },
        );
        assert_eq!(
            parse_shortcut(&display),
            Some((first, second)),
            "{shortcut}"
        );
    }
    for invalid in [
        "",
        "Ctrl+K, Unassigned",
        "Unassigned, Ctrl+F",
        "Ctrl+K, Ctrl+F, Ctrl+S",
        "NotAKey",
    ] {
        assert!(parse_shortcut(invalid).is_none(), "{invalid}");
    }
    let mut app = fixture();
    edit(&mut app, "toggle_zen");
    action(&mut app, EditAction::Type);
    app.shortcut_editor.as_mut().unwrap().text = "NotAKey".into();
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.shortcut_editor.as_ref().unwrap().error.is_some());
    app.shortcut_editor.as_mut().unwrap().text = "Esc".into();
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        app.keybindings[index(&app, "toggle_zen")].code,
        KeyCode::Esc
    );
}

#[test]
fn delete_can_be_recorded_and_unassignment_is_an_explicit_action() {
    let mut app = fixture();
    let target = edit(&mut app, "toggle_zen");
    key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.keybindings[target].code, KeyCode::Delete);
    app.begin_shortcut_edit(target, true);
    action(&mut app, EditAction::Unassign);
    assert_eq!(app.keybindings[target].code, KeyCode::Delete);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.keybindings[target].code, KeyCode::Null);
}

#[test]
fn scope_edits_preserve_inheritance_unknown_entries_and_document_preferences() {
    let mut app = fixture();
    app.settings_global.keybindings = Some(vec![
        custom("fold_all", "F7", None),
        custom("future.command", "Ctrl+F20", None),
    ]);
    app.settings_workspace.keybindings = Some(vec![custom("toggle_zen", "F9", None)]);
    app.settings_workspace.tab_size = Some(8);
    app.apply_effective_settings();
    app.tabs[0].editor.tab_size = 2;
    app.settings_scope = SettingsScope::Workspace;
    let target = edit(&mut app, "fold_all");
    action(&mut app, EditAction::Record);
    key(&mut app, KeyCode::F(24), KeyModifiers::ALT);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.keybindings[target].display(), "Alt+F24");
    assert_eq!(
        app.settings_global.keybindings.as_ref().unwrap()[0]
            .key
            .as_deref(),
        Some("F7")
    );
    assert!(
        app.settings_workspace
            .keybindings
            .as_ref()
            .unwrap()
            .iter()
            .any(|entry| entry.command.as_deref() == Some("toggle_zen"))
    );
    assert_eq!(app.tabs[0].editor.tab_size, 2);
    assert!(app.reset_keybinding_in_selected_scope(target));
    assert_eq!(app.keybindings[target].display(), "F7");
    assert!(
        app.settings_global
            .keybindings
            .as_ref()
            .unwrap()
            .iter()
            .any(|entry| entry.command.as_deref() == Some("future.command"))
    );
}

#[test]
fn alternative_bindings_have_independent_persistence_reset_and_layer_merging() {
    let mut app = fixture();
    let redo: Vec<_> = app
        .keybindings
        .iter()
        .enumerate()
        .filter_map(|(index, binding)| (binding.id == "redo").then_some(index))
        .collect();
    assert_eq!(redo.len(), 2);
    app.open_settings_tab();
    app.begin_shortcut_edit(redo[1], true);
    key(&mut app, KeyCode::F(24), KeyModifiers::ALT);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.keybindings[redo[0]].display(), "Ctrl+Y");
    assert_eq!(app.keybindings[redo[1]].display(), "Alt+F24");
    let entry = app
        .settings_global
        .keybindings
        .as_ref()
        .unwrap()
        .iter()
        .find(|entry| entry.command.as_deref() == Some("redo"))
        .unwrap();
    assert_eq!(entry.index, Some(redo[1]));
    assert!(!app.setting_is_defined_in_layer(&format!("kb_{}", redo[0]), &app.settings_global));
    let global = Settings {
        keybindings: Some(vec![custom("redo", "F6", None)]),
        ..Default::default()
    };
    let workspace = Settings {
        keybindings: Some(vec![custom("redo", "F7", Some(redo[1]))]),
        ..Default::default()
    };
    let merged = App::keybindings_for_settings(&global.merge(&workspace));
    assert_eq!(merged[redo[0]].display(), "F6");
    assert_eq!(merged[redo[1]].display(), "F7");
    assert!(app.reset_keybinding_in_selected_scope(redo[1]));
    assert_eq!(app.keybindings[redo[0]].display(), "Ctrl+Y");
    assert_eq!(app.keybindings[redo[1]].display(), "Ctrl+Shift+Z");
}

#[test]
fn settings_search_finds_shortcuts_by_key_and_command_identifier() {
    let mut app = fixture();
    app.rebuild_keybinding_settings_cache();
    app.open_settings_tab();
    for query in ["Ctrl+K", "save_as"] {
        app.settings_editor_search = query.into();
        let items = app.settings_items();
        let matches = app.settings_filtered_indices_with_items(5, &items);
        assert!(
            matches
                .iter()
                .any(|&index| items[index].2.starts_with("kb_")),
            "{query}"
        );
    }
}

#[test]
fn assignment_dialog_supports_themes_narrow_layouts_and_mouse_cancellation() {
    for width in [24, 36, 82, 110] {
        for theme in [
            crate::app_theme::EditorTheme::Dark,
            crate::app_theme::EditorTheme::Light,
        ] {
            let mut app = fixture();
            app.colors = theme.colors();
            let target = edit(&mut app, "toggle_zen");
            key(&mut app, KeyCode::F(24), KeyModifiers::NONE);
            let original = app.keybindings[target].display();
            let screen = render(&mut app, width, 36);
            preview(&screen, theme, width, "ready");
            assert!(text(&screen).contains("F24"), "width={width}");
            let cancel = app
                .shortcut_editor
                .as_ref()
                .unwrap()
                .areas
                .iter()
                .find(|(_, action)| *action == EditAction::Cancel)
                .unwrap()
                .0;
            assert_eq!(cancel.intersection(screen.area), cancel);
            crate::mouse::handle_mouse(
                &mut app,
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: cancel.x,
                    row: cancel.y,
                    modifiers: KeyModifiers::NONE,
                },
            );
            assert!(app.shortcut_editor.is_none());
            assert_eq!(app.keybindings[target].display(), original);
        }
    }
}

#[test]
fn unchanged_recording_preserves_shared_defaults_and_keyboard_focus_moves_both_ways() {
    let mut app = fixture();
    let target = edit(&mut app, "goto_bracket");
    key(&mut app, KeyCode::Char('\\'), KeyModifiers::CONTROL);
    assert!(App::shortcut_conflicts(&app.keybindings, target, "Ctrl+\\").is_empty());
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.shortcut_editor.as_ref().unwrap().focus, 1);
    key(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(app.shortcut_editor.as_ref().unwrap().focus, 0);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.settings_global.keybindings.is_none());
    assert_eq!(
        app.keybindings[index(&app, "split_editor")].display(),
        "Ctrl+\\"
    );
}

fn preview(buffer: &Buffer, theme: crate::app_theme::EditorTheme, width: u16, state: &str) {
    if std::env::var_os("VTCODE_SHORTCUT_PREVIEW").is_none() {
        return;
    }
    let name = if matches!(theme, crate::app_theme::EditorTheme::Light) {
        "light"
    } else {
        "dark"
    };
    let rgb = |color| match color {
        ratatui::style::Color::Rgb(r, g, b) => [r, g, b],
        _ => [128, 128, 128],
    };
    let cells: Vec<_> = buffer
        .content
        .iter()
        .map(|cell| {
            serde_json::json!({
                "text": cell.symbol(), "fg": rgb(cell.fg), "bg": rgb(cell.bg),
                "bold": cell.modifier.contains(ratatui::style::Modifier::BOLD),
            })
        })
        .collect();
    std::fs::write(
        format!("target/shortcut-{name}-{width}-{state}.json"),
        serde_json::to_vec(&serde_json::json!({
            "width": width, "height": buffer.area.height, "cells": cells,
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn assignment_dialog_conflict_and_chord_previews_show_the_pending_change() {
    for theme in [
        crate::app_theme::EditorTheme::Dark,
        crate::app_theme::EditorTheme::Light,
    ] {
        for state in ["conflict", "chord"] {
            let mut app = fixture();
            app.colors = theme.colors();
            edit(&mut app, "toggle_zen");
            if state == "conflict" {
                key(&mut app, KeyCode::F(3), KeyModifiers::NONE);
            } else {
                action(&mut app, EditAction::Chord);
                key(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
            }
            let buffer = render(&mut app, 110, 32);
            preview(&buffer, theme, 110, state);
            let screen = text(&buffer);
            if state == "conflict" {
                assert!(screen.contains("[Replace]"));
                assert!(screen.contains("Find Next"));
            } else {
                assert!(screen.contains("Press the second key combination"));
                assert!(
                    !app.shortcut_editor
                        .as_ref()
                        .unwrap()
                        .areas
                        .iter()
                        .any(|(_, action)| *action == EditAction::Save)
                );
            }
        }
    }
}
