use super::tests::{FIELDS, activate_field};
use super::*;
use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::{Terminal, backend::TestBackend};

pub(super) const EXTRA_FIELDS: &[TextInputId] = &[
    TextInputId::Exclusion(0),
    TextInputId::DeskPopup,
    TextInputId::WorkspaceFilter,
    TextInputId::WorkspaceName,
    TextInputId::CompletionNumber,
    TextInputId::OutputFind,
];

fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, modifiers));
}

fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        },
    );
}

pub(super) fn field_app(id: TextInputId) -> App {
    let mut app = App::test_new();
    app.tabs = vec![crate::app::Tab::new_file(
        crate::editor::Editor::test_with_text("underlying document"),
    )];
    app.active_tab = 0;
    match id {
        TextInputId::DeskPopup => crate::ui_desks::open(&mut app, crate::ui_desks::Mode::New),
        TextInputId::WorkspaceFilter => crate::ui_workspaces::open(&mut app),
        TextInputId::WorkspaceName => {
            crate::ui_workspaces::open(&mut app);
            app.workspaces.popup.sets.mode = Some(crate::ui_workspace_sets::Mode::Save(None));
            app.workspaces.popup.sets.focus_input();
        }
        TextInputId::CompletionNumber => {
            crate::ui_vtcomplete_index::open(&mut app);
            crate::ui_vtcomplete_index::handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE),
            );
            crate::ui_vtcomplete_index::handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            );
        }
        TextInputId::OutputFind => {
            app.show_output();
            crate::ui_logs::key(
                &mut app,
                KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
            );
        }
        _ => activate_field(&mut app, id),
    }
    let (text, cursor, anchor) = app.text_input_state_mut(id);
    *text = "12345".into();
    *cursor = 5;
    *anchor = Some(0);
    app
}

fn open_registered_menu(app: &mut App, id: TextInputId) {
    app.text_input_regions.clear();
    app.register_text_input(
        id,
        TextInputHitMap::new(
            Rect::new(10, 5, 20, 1),
            (0..20).map(|i| i.min(5)).collect(),
            5,
        ),
    );
    assert!(app.handle_text_input_mouse(
        MouseEventKind::Down(MouseButton::Right),
        12,
        5,
        KeyModifiers::NONE
    ));
    assert!(app.text_input_context_menu.is_some(), "{id:?}");
}

fn choose(app: &mut App, index: usize) {
    key(app, KeyCode::Home, KeyModifiers::NONE);
    for _ in 0..index {
        key(app, KeyCode::Down, KeyModifiers::NONE);
    }
    key(app, KeyCode::Enter, KeyModifiers::NONE);
}

#[test]
fn every_text_field_menu_copies_cuts_pastes_selects_and_preserves_undo() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    for &id in FIELDS.iter().chain(EXTRA_FIELDS) {
        let mut app = field_app(id);
        open_registered_menu(&mut app, id);
        choose(&mut app, 1);
        assert_eq!(
            crate::clipboard::get_text().unwrap(),
            "12345",
            "copy {id:?}"
        );
        assert_eq!(app.text_input_state_mut(id).0, "12345");

        open_registered_menu(&mut app, id);
        choose(&mut app, 0);
        assert_eq!(app.text_input_state_mut(id).0, "", "cut {id:?}");
        key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(app.text_input_state_mut(id).0, "12345", "cut undo {id:?}");

        crate::clipboard::set_text("67").unwrap();
        open_registered_menu(&mut app, id);
        choose(&mut app, 2);
        assert_eq!(app.text_input_state_mut(id).0, "67", "paste {id:?}");
        key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(app.text_input_state_mut(id).0, "12345", "paste undo {id:?}");

        let (_, cursor, anchor) = app.text_input_state_mut(id);
        *cursor = 2;
        *anchor = None;
        open_registered_menu(&mut app, id);
        choose(&mut app, 3);
        let (_, cursor, anchor) = app.text_input_state_mut(id);
        assert_eq!(
            App::inline_selection_range(*cursor, *anchor),
            Some((0, 5)),
            "select all {id:?}"
        );
        assert_eq!(
            app.tabs[0].editor.rope.to_string(),
            "underlying document",
            "{id:?}"
        );
    }
}

#[test]
fn rendered_fields_own_right_clicks_and_menus_appear_above_dialogs() {
    for id in [
        TextInputId::SearchFind,
        TextInputId::SearchReplace,
        TextInputId::GlobalSearch(0),
        TextInputId::GlobalSearch(1),
        TextInputId::GlobalSearch(2),
        TextInputId::GlobalSearch(3),
        TextInputId::SourceControlCommit,
        TextInputId::SaveAs,
        TextInputId::TerminalRename,
        TextInputId::FolderAddress,
        TextInputId::Chat,
    ]
    .into_iter()
    .chain(EXTRA_FIELDS.iter().copied())
    {
        let mut app = field_app(id);
        app.show_explorer = true;
        if matches!(id, TextInputId::GlobalSearch(_)) {
            app.sidebar_view = crate::app::SidebarView::Search;
        }
        app.source_control_repository_state = crate::app::SourceControlRepositoryState::Available;
        app.sc_graph_collapsed = true;
        let original = (
            app.tabs[0].editor.cursor_line,
            app.tabs[0].editor.cursor_col,
            app.tabs[0].editor.selection_anchor,
        );
        let mut terminal = Terminal::new(TestBackend::new(140, 45)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let area = app
            .text_input_area(id)
            .unwrap_or_else(|| panic!("missing rendered field {id:?}"));
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Right),
            area.x + area.width.min(2).saturating_sub(1),
            area.y,
        );
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Right),
            area.x,
            area.y,
        );
        assert!(app.text_input_context_menu.is_some(), "right-click {id:?}");
        assert!(
            !app.context_menu_active && !app.sc_context_active && !app.explorer_context_active,
            "underlying context menu {id:?}"
        );
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let menu = app
            .text_input_context_menu_area()
            .unwrap_or_else(|| panic!("menu hidden by {id:?}"));
        let labels: String = (menu.y..menu.bottom())
            .flat_map(|y| (menu.x..menu.right()).map(move |x| (x, y)))
            .map(|position| terminal.backend().buffer()[position].symbol())
            .collect();
        for label in ["Cut", "Copy", "Paste", "Select All"] {
            assert!(labels.contains(label), "{id:?}: {label}");
        }
        assert_eq!(
            (
                app.tabs[0].editor.cursor_line,
                app.tabs[0].editor.cursor_col,
                app.tabs[0].editor.selection_anchor
            ),
            original,
            "editor click-through {id:?}"
        );
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.text_input_context_menu.is_none());
        assert_eq!(
            app.focused_text_input_id(),
            Some(id),
            "focus after dismiss {id:?}"
        );
    }
}

#[test]
fn context_menu_mouse_rows_follow_flipping_clipping_and_resizing() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    for (width, height) in [(80, 20), (28, 8), (25, 4)] {
        let mut app = field_app(TextInputId::SourceControlCommit);
        open_registered_menu(&mut app, TextInputId::SourceControlCommit);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        key(&mut app, KeyCode::Home, KeyModifiers::NONE);
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        terminal
            .draw(|frame| app.draw_text_input_context_menu(frame))
            .unwrap();
        let inner = app.text_input_context_menu_area().unwrap();
        let row = (inner.y..inner.bottom())
            .find(|&y| {
                let line: String = (inner.x..inner.right())
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect();
                line.contains("Copy")
            })
            .unwrap();
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            inner.x,
            row,
        );
        assert_eq!(crate::clipboard::get_text().unwrap(), "12345");
        assert!(app.text_input_context_menu.is_none());
        assert_eq!(app.source_control_commit_msg, "12345");
    }
}

#[test]
fn right_click_keeps_selected_unicode_and_moves_caret_outside_selection() {
    let id = TextInputId::SearchFind;
    let mut app = field_app(id);
    app.search_query = "a界🙂b".into();
    app.search_cursor = 8;
    app.search_selection_anchor = Some(1);
    app.register_text_input(
        id,
        TextInputHitMap::new(Rect::new(5, 5, 7, 1), vec![0, 1, 1, 4, 4, 8, 9], 9),
    );
    assert!(app.handle_text_input_mouse(
        MouseEventKind::Down(MouseButton::Right),
        8,
        5,
        KeyModifiers::NONE
    ));
    assert_eq!(
        (app.search_cursor, app.search_selection_anchor),
        (8, Some(1))
    );
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.handle_text_input_mouse(
        MouseEventKind::Down(MouseButton::Right),
        5,
        5,
        KeyModifiers::NONE
    ));
    assert_eq!((app.search_cursor, app.search_selection_anchor), (0, None));
    choose(&mut app, 0);
    assert_eq!(
        app.search_query, "a界🙂b",
        "disabled Cut must preserve the field"
    );
    assert!(app.text_input_context_menu.is_some());
}

#[test]
fn stale_field_menu_does_not_paste_into_a_different_search_owner_or_editor() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    crate::clipboard::set_text("wrong target").unwrap();
    let mut app = field_app(TextInputId::SearchFind);
    open_registered_menu(&mut app, TextInputId::SearchFind);
    app.tabs.push(crate::app::Tab::new_file(
        crate::editor::Editor::test_with_text("other document"),
    ));
    app.active_tab = 1;
    key(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL);
    assert!(app.text_input_context_menu.is_none());
    assert_eq!(app.tabs[1].editor.rope.to_string(), "other document");
}

#[test]
fn menu_paste_honors_numeric_filters_and_multiline_fields() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    crate::clipboard::set_text("a6\n7:8\r\n").unwrap();
    for (id, expected) in [
        (TextInputId::SearchFind, "a67:8"),
        (TextInputId::GotoLine, "67:8"),
        (TextInputId::SettingsValue, "678"),
        (TextInputId::AgentSetting(2), "678"),
        (TextInputId::CompletionNumber, "678"),
        (TextInputId::SourceControlCommit, "a6\n7:8\n"),
    ] {
        let mut app = field_app(id);
        open_registered_menu(&mut app, id);
        choose(&mut app, 2);
        assert_eq!(app.text_input_state_mut(id).0, expected, "{id:?}");
    }
}

#[test]
fn keyboard_context_menu_and_outside_dismissal_keep_field_focus_and_editor_selection() {
    let id = TextInputId::SearchFind;
    let mut app = field_app(id);
    open_registered_menu(&mut app, id);
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    key(&mut app, KeyCode::F(10), KeyModifiers::SHIFT);
    assert!(app.text_input_context_menu.is_some());
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    terminal
        .draw(|frame| app.draw_text_input_context_menu(frame))
        .unwrap();
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 0, 15);
    assert!(app.text_input_context_menu.is_none());
    assert_eq!(app.focused_text_input_id(), Some(id));
    assert_eq!(app.tabs[0].editor.rope.to_string(), "underlying document");
    assert_eq!(
        (
            app.tabs[0].editor.cursor_line,
            app.tabs[0].editor.cursor_col
        ),
        (0, 0)
    );
}

#[test]
fn find_toolbar_borders_and_buttons_never_open_the_editors_context_menu() {
    let mut app = field_app(TextInputId::SearchReplace);
    let mut terminal = Terminal::new(TestBackend::new(120, 35)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let bar = app.file_search_bars[0].area;
    let fields: Vec<_> = [TextInputId::SearchFind, TextInputId::SearchReplace]
        .into_iter()
        .filter_map(|id| app.text_input_area(id))
        .collect();
    let mut checked = 0;
    for row in bar.y..bar.bottom() {
        for column in bar.x..bar.right() {
            if fields
                .iter()
                .any(|area| area.contains((column, row).into()))
            {
                continue;
            }
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Right),
                column,
                row,
            );
            assert!(!app.context_menu_active, "click-through at {column}:{row}");
            assert!(app.text_input_context_menu.is_none());
            checked += 1;
        }
    }
    assert!(checked > 0);
    assert_eq!(
        (
            app.editor().unwrap().cursor_line,
            app.editor().unwrap().cursor_col
        ),
        (0, 0)
    );
}
