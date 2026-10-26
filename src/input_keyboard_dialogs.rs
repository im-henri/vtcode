use crate::text_input::TextInputId;
use crossterm::event::{KeyCode, KeyModifiers};

use crate::app::App;

/// Handle source control commit message keyboard input.
/// Returns `Some(false)` if handled, `None` if not applicable.
pub(crate) fn handle_commit_input_key(app: &mut App, key: crossterm::event::KeyEvent) -> Option<bool> {
    if !(app.sidebar_view == crate::app::SidebarView::SourceControl
        && app.active_pane == crate::app::ActivePane::Explorer
        && app.source_control_input_mode)
    {
        return None;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
        KeyCode::Esc => { app.source_control_input_mode = false; }
        KeyCode::Tab | KeyCode::BackTab => {
            app.source_control_input_mode = false;
            app.sc_graph_focused = key.code == KeyCode::BackTab || shift;
        }
        KeyCode::Enter if ctrl && shift => {
            app.sc_amend_mode = true;
            app.git_amend_commit();
            app.source_control_input_mode = false;
        }
        KeyCode::Enter if ctrl => {
            app.sc_amend_mode = false;
            app.git_commit();
            app.source_control_input_mode = false;
        }
        _ => {
            app.edit_commit_input(key.code, key.modifiers, None);
        }
    }
    Some(false)
}

/// Commit tabs own their browsing keys while workbench shortcuts still run.
/// Returns `Some(false)` if handled, `None` if not applicable.
pub(crate) fn handle_commit_details_key(app: &mut App, key: crossterm::event::KeyEvent) -> Option<bool> {
    if !crate::ui_commit_details::owns_key(app, key) {
        return None;
    }
    crate::ui_commit_details::handle_key(app, key);
    Some(false)
}

/// Handle rename dialog keyboard input.
/// Returns `Some(false)` if handled, `None` if not applicable.
pub(crate) fn handle_rename_key(app: &mut App, key: crossterm::event::KeyEvent) -> Option<bool> {
    if !app.rename.active {
        return None;
    }
    match key.code {
        KeyCode::Esc => { app.cancel_rename(); }
        KeyCode::Enter => { app.execute_rename(); }
        _ => {
            app.edit_text_input(
                TextInputId::ExplorerRename,
                key.code,
                key.modifiers,
                None,
            );
        }
    }
    Some(false)
}

/// Handle save-as dialog keyboard input.
/// Returns `Some(false)` if handled, `None` if not applicable.
pub(crate) fn handle_save_as_key(app: &mut App, key: crossterm::event::KeyEvent) -> Option<bool> {
    if !app.save_as_active {
        return None;
    }
    match key.code {
        KeyCode::Esc => { app.save_as_active = false; }
        KeyCode::Enter => { app.execute_save_as(); }
        _ => {
            app.edit_text_input(
                TextInputId::SaveAs,
                key.code,
                key.modifiers,
                None,
            );
        }
    }
    Some(false)
}

/// Handle rename-symbol dialog keyboard input.
/// Returns `Some(false)` if handled, `None` if not applicable.
pub(crate) fn handle_rename_symbol_key(app: &mut App, key: crossterm::event::KeyEvent) -> Option<bool> {
    if !app.rename.symbol_active {
        return None;
    }
    match key.code {
        KeyCode::Esc => { app.rename.symbol_active = false; }
        KeyCode::Enter => { app.execute_rename_symbol(); }
        _ => {
            app.edit_text_input(
                TextInputId::RenameSymbol,
                key.code,
                key.modifiers,
                None,
            );
        }
    }
    Some(false)
}

/// Handle binary open prompt keyboard input.
/// Returns `Some(false)` if handled, `None` if not applicable.
pub(crate) fn handle_binary_open_prompt_key(app: &mut App, key: crossterm::event::KeyEvent) -> Option<bool> {
    if !app.binary_open_prompt_active {
        return None;
    }
    let option_count = app.binary_open_prompt_option_count();
    match key.code {
        KeyCode::Esc => app.execute_binary_open_prompt_choice(option_count.saturating_sub(1)),
        KeyCode::Up => {
            if app.binary_open_prompt_selected > 0 {
                app.binary_open_prompt_selected -= 1;
            }
        }
        KeyCode::Down => {
            if app.binary_open_prompt_selected + 1 < option_count {
                app.binary_open_prompt_selected += 1;
            }
        }
        KeyCode::Enter => {
            let selected = app.binary_open_prompt_selected;
            app.execute_binary_open_prompt_choice(selected);
        }
        KeyCode::Char(c) if c.is_ascii_digit() => {
            if let Some(digit) = c.to_digit(10) {
                let idx = digit as usize;
                if idx > 0 && idx <= option_count {
                    app.execute_binary_open_prompt_choice(idx - 1);
                }
            }
        }
        _ => {}
    }
    Some(false)
}

/// Handle delete confirmation dialog keyboard input.
/// Returns `Some(false)` if handled, `None` if not applicable.
pub(crate) fn handle_confirm_delete_key(app: &mut App, key: crossterm::event::KeyEvent) -> Option<bool> {
    if !app.confirm_delete_active {
        return None;
    }
    match key.code {
        KeyCode::Esc => app.execute_confirm_delete(false),
        KeyCode::Enter => app.execute_confirm_delete(app.confirm_delete_selected == 0),
        KeyCode::Tab | KeyCode::Left | KeyCode::Right => {
            app.confirm_delete_selected = 1 - app.confirm_delete_selected;
        }
        _ => {}
    }
    Some(false)
}

/// Handle drag-and-drop move confirmation dialog keyboard input.
pub(crate) fn handle_confirm_move_key(app: &mut App, key: crossterm::event::KeyEvent) -> Option<bool> {
    if !app.confirm_move_active {
        return None;
    }
    match key.code {
        KeyCode::Esc => app.execute_confirm_move(false),
        KeyCode::Enter => app.execute_confirm_move(app.confirm_move_selected == 0),
        KeyCode::Tab | KeyCode::Left | KeyCode::Right => {
            app.confirm_move_selected = 1 - app.confirm_move_selected;
        }
        _ => {}
    }
    Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    // --- handle_commit_input_key ---

    #[test]
    fn commit_input_returns_none_when_inactive() {
        let mut app = App::test_new();
        assert!(handle_commit_input_key(&mut app, key(KeyCode::Char('a'))).is_none());
    }

    #[test]
    fn commit_input_esc_exits_mode() {
        let mut app = App::test_new();
        app.sidebar_view = crate::app::SidebarView::SourceControl;
        app.active_pane = crate::app::ActivePane::Explorer;
        app.source_control_input_mode = true;
        let result = handle_commit_input_key(&mut app, key(KeyCode::Esc));
        assert_eq!(result, Some(false));
        assert!(!app.source_control_input_mode);
    }

    #[test]
    fn commit_input_char_inserts() {
        let mut app = App::test_new();
        app.sidebar_view = crate::app::SidebarView::SourceControl;
        app.active_pane = crate::app::ActivePane::Explorer;
        app.source_control_input_mode = true;
        app.source_control_commit_msg.clear();
        app.source_control_commit_cursor = 0;
        handle_commit_input_key(&mut app, key(KeyCode::Char('h')));
        handle_commit_input_key(&mut app, key(KeyCode::Char('i')));
        assert_eq!(app.source_control_commit_msg, "hi");
    }

    #[test]
    fn commit_input_enter_inserts_newline() {
        let mut app = App::test_new();
        app.sidebar_view = crate::app::SidebarView::SourceControl;
        app.active_pane = crate::app::ActivePane::Explorer;
        app.source_control_input_mode = true;
        app.source_control_commit_msg = "test".to_string();
        app.source_control_commit_cursor = 4;
        handle_commit_input_key(&mut app, key(KeyCode::Enter));
        assert!(app.source_control_commit_msg.contains('\n'));
    }

    // --- handle_rename_key ---

    #[test]
    fn rename_returns_none_when_inactive() {
        let mut app = App::test_new();
        assert!(handle_rename_key(&mut app, key(KeyCode::Char('a'))).is_none());
    }

    #[test]
    fn rename_char_inserts() {
        let mut app = App::test_new();
        app.rename.active = true;
        app.rename.query.clear();
        app.rename.cursor = 0;
        handle_rename_key(&mut app, key(KeyCode::Char('x')));
        assert_eq!(app.rename.query, "x");
        assert_eq!(app.rename.cursor, 1);
    }

    #[test]
    fn rename_backspace_deletes() {
        let mut app = App::test_new();
        app.rename.active = true;
        app.rename.query = "ab".to_string();
        app.rename.cursor = 2;
        handle_rename_key(&mut app, key(KeyCode::Backspace));
        assert_eq!(app.rename.query, "a");
    }

    #[test]
    fn rename_home_moves_to_start() {
        let mut app = App::test_new();
        app.rename.active = true;
        app.rename.query = "hello".to_string();
        app.rename.cursor = 3;
        handle_rename_key(&mut app, key(KeyCode::Home));
        assert_eq!(app.rename.cursor, 0);
    }

    #[test]
    fn rename_end_moves_to_end() {
        let mut app = App::test_new();
        app.rename.active = true;
        app.rename.query = "hello".to_string();
        app.rename.cursor = 0;
        handle_rename_key(&mut app, key(KeyCode::End));
        assert_eq!(app.rename.cursor, 5);
    }

    // --- handle_save_as_key ---

    #[test]
    fn save_as_returns_none_when_inactive() {
        let mut app = App::test_new();
        assert!(handle_save_as_key(&mut app, key(KeyCode::Char('a'))).is_none());
    }

    #[test]
    fn save_as_esc_closes() {
        let mut app = App::test_new();
        app.save_as_active = true;
        handle_save_as_key(&mut app, key(KeyCode::Esc));
        assert!(!app.save_as_active);
    }

    // --- handle_rename_symbol_key ---

    #[test]
    fn rename_symbol_returns_none_when_inactive() {
        let mut app = App::test_new();
        assert!(handle_rename_symbol_key(&mut app, key(KeyCode::Char('a'))).is_none());
    }

    #[test]
    fn rename_symbol_esc_closes() {
        let mut app = App::test_new();
        app.rename.symbol_active = true;
        handle_rename_symbol_key(&mut app, key(KeyCode::Esc));
        assert!(!app.rename.symbol_active);
    }

    // --- handle_binary_open_prompt_key ---

    #[test]
    fn binary_prompt_returns_none_when_inactive() {
        let mut app = App::test_new();
        assert!(handle_binary_open_prompt_key(&mut app, key(KeyCode::Char('1'))).is_none());
    }

    #[test]
    fn binary_prompt_down_increments_selected() {
        let mut app = App::test_new();
        app.binary_open_prompt_active = true;
        app.binary_open_prompt_selected = 0;
        handle_binary_open_prompt_key(&mut app, key(KeyCode::Down));
        assert_eq!(app.binary_open_prompt_selected, 1);
    }

    #[test]
    fn binary_prompt_up_decrements_selected() {
        let mut app = App::test_new();
        app.binary_open_prompt_active = true;
        app.binary_open_prompt_selected = 1;
        handle_binary_open_prompt_key(&mut app, key(KeyCode::Up));
        assert_eq!(app.binary_open_prompt_selected, 0);
    }

    #[test]
    fn binary_prompt_up_at_zero_stays() {
        let mut app = App::test_new();
        app.binary_open_prompt_active = true;
        app.binary_open_prompt_selected = 0;
        handle_binary_open_prompt_key(&mut app, key(KeyCode::Up));
        assert_eq!(app.binary_open_prompt_selected, 0);
    }

    // --- handle_confirm_delete_key ---

    #[test]
    fn confirm_delete_returns_none_when_inactive() {
        let mut app = App::test_new();
        assert!(handle_confirm_delete_key(&mut app, key(KeyCode::Enter)).is_none());
    }

    #[test]
    fn confirm_delete_tab_toggles() {
        let mut app = App::test_new();
        app.confirm_delete_active = true;
        app.confirm_delete_selected = 0;
        handle_confirm_delete_key(&mut app, key(KeyCode::Tab));
        assert_eq!(app.confirm_delete_selected, 1);
        handle_confirm_delete_key(&mut app, key(KeyCode::Tab));
        assert_eq!(app.confirm_delete_selected, 0);
    }

    #[test]
    fn confirm_delete_esc_cancels() {
        let mut app = App::test_new();
        app.confirm_delete_active = true;
        handle_confirm_delete_key(&mut app, key(KeyCode::Esc));
        assert!(!app.confirm_delete_active);
    }

    // --- handle_commit_details_key ---

    #[test]
    fn commit_details_returns_none_when_inactive() {
        let mut app = App::test_new();
        assert!(handle_commit_details_key(&mut app, key(KeyCode::Up)).is_none());
    }

    #[test]
    fn commit_details_esc_preserves_the_editor_tab() {
        let mut app = App::test_new();
        let mut tab = crate::app::Tab::new_file(crate::editor::Editor::new());
        tab.kind = crate::app::TabKind::CommitDetails;
        app.tabs.push(tab);
        app.active_pane = crate::app::ActivePane::Editor;
        app.commit_details.active = true;
        handle_commit_details_key(&mut app, key(KeyCode::Esc));
        assert!(app.commit_details.active);
    }

    #[test]
    fn commit_details_up_decrements() {
        let mut app = App::test_new();
        let mut tab = crate::app::Tab::new_file(crate::editor::Editor::new());
        tab.kind = crate::app::TabKind::CommitDetails;
        app.tabs.push(tab);
        app.active_pane = crate::app::ActivePane::Editor;
        app.commit_details.active = true;
        app.commit_details.selected = 2;
        app.commit_details.data.files = (0..3).map(|i| crate::commit_details::CommitFile::new('M', format!("{i}.rs"))).collect();
        app.commit_details.loading = true;
        handle_commit_details_key(&mut app, key(KeyCode::Up));
        assert_eq!(app.commit_details.selected, 1);
    }

    #[test]
    fn commit_details_up_at_zero_stays() {
        let mut app = App::test_new();
        let mut tab = crate::app::Tab::new_file(crate::editor::Editor::new());
        tab.kind = crate::app::TabKind::CommitDetails;
        app.tabs.push(tab);
        app.active_pane = crate::app::ActivePane::Editor;
        app.commit_details.active = true;
        app.commit_details.selected = 0;
        handle_commit_details_key(&mut app, key(KeyCode::Up));
        assert_eq!(app.commit_details.selected, 0);
    }
}
